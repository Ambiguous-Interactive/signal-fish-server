//! Clock-source hygiene guard for server production code.
//!
//! Sibling of the other scan guards (`tests/source_hygiene_guards.rs`,
//! `tests/async_timeout_policy_scan.rs`, `tests/loud_test_failures_scan.rs`).
//! It pins the "Injectable time" convention from `.llm/context-testing.md`
//! (issue #495): time-driven logic must stay deterministic to test, so a
//! production decision path must never read a `std` clock directly.
//!
//! The two sanctioned patterns are:
//!   1. Async logic uses `tokio::time::Instant`, controllable in tests via
//!      `#[tokio::test(start_paused = true)]` + `tokio::time::advance`.
//!   2. Sync logic reads the clock only inside a thin public wrapper that
//!      delegates to an `*_at(..., now: Instant)` / `*_since(..., now)`
//!      injected-time variant.
//!
//! A new `use std::time::...Instant/SystemTime...` import (including glob,
//! alias, and rustfmt's multi-line brace forms) or a qualified
//! `std::time::Instant` / `std::time::SystemTime` use in `src/` fails this
//! guard unless the file is on the explicit allowlist below with a stated
//! reason. Entries marked `test_module_only` additionally require every
//! match to sit after the file's first `#[cfg(test)]` marker.
//!
//! Scope note: this guard pins `std` clock sources. `chrono` `Utc::now()`
//! wall-clock reads (durable-record timestamps, embedder conveniences) are a
//! separate tracked surface (#498). `clients/` is out of scope: its
//! remaining `std` clock sites are upstream-API boundaries (e.g. matchbox
//! `get_stats(std::time::Instant, ...)`) or test code.

#![cfg(test)]

mod common;

use common::{read_file, repo_root};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Why an allowlisted file may name a `std` time type. Entries with
/// `test_module_only` are pinned to the file's `#[cfg(test)]` region: a
/// production-code match above the first marker fails the scan.
struct Exemption {
    reason: &'static str,
    test_module_only: bool,
}

/// Files allowed to name `std::time::Instant` / `std::time::SystemTime`,
/// each with the reason the allowlisting is sound. Everything else in `src/`
/// must use tokio time or an injected-time seam.
fn std_time_allowlist() -> BTreeMap<&'static str, Exemption> {
    BTreeMap::from([
        (
            "src/auth/rate_limiter.rs",
            Exemption {
                reason: "clock reads confined to the thin check_rate_limit/cleanup \
                         wrappers over the check_rate_limit_at/cleanup_at injection \
                         seams",
                test_module_only: false,
            },
        ),
        (
            "src/websocket/upgrade_rejection_log.rs",
            Exemption {
                reason: "clock read confined to the thin record() wrapper over record_at",
                test_module_only: false,
            },
        ),
        (
            "src/websocket/metrics.rs",
            Exemption {
                reason: "clock read confined to the thin record() wrapper over record_at",
                test_module_only: false,
            },
        ),
        (
            "src/server/shutdown.rs",
            Exemption {
                reason: "absolute epoch-ms drain deadline advertised to v3 clients and \
                         the wait_before_close grace computation, both behind thin \
                         wrappers over the *_since injected variants; the wall clock \
                         is the point of those deadlines (clients see absolute ms); \
                         monotonic shutdown logic uses tokio time",
                test_module_only: false,
            },
        ),
        (
            "src/main.rs",
            Exemption {
                reason: "startup-timing assertion",
                test_module_only: true,
            },
        ),
        (
            "src/coordination/mod.rs",
            Exemption {
                reason: "progress deadline loop",
                test_module_only: true,
            },
        ),
    ])
}

/// Strip a trailing `//` line comment, but not `://` inside a string literal
/// (URLs). A `//` pair directly preceded by `:` is treated as part of a
/// scheme and scanning continues past it.
fn strip_line_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut index = 1;
    while index < bytes.len() {
        if bytes[index] == b'/' && bytes[index - 1] == b'/' {
            let preceded_by_colon = index >= 2 && bytes[index - 2] == b':';
            if !preceded_by_colon {
                return &line[..index - 1];
            }
        }
        index += 1;
    }
    line
}

/// True if the source line references a `std` time type. Imports are flagged
/// unless the imported surface is provably Duration-only: any `use ...
/// std::time ...` statement is examined wherever it appears on the line
/// (including `pub use` re-exports and statements nested in `mod { ... }`),
/// and the glob (`::*`), alias (` as X`), module (`use std::time;`), brace-
/// `self`, unclosed-brace (rustfmt multi-line), and `Instant`/`SystemTime`
/// naming forms all flag. Qualified `std::time::Instant`/`SystemTime` uses
/// anywhere on the line flag too. Bare `Instant::now()` is deliberately not
/// flagged: depending on the file's imports it may resolve to the
/// pause-controllable tokio type, which is the sanctioned async pattern.
///
/// Known line-scanning limits (accepted, safe-direction or absent today):
/// a `//` pair inside a non-URL string literal can hide a later real use,
/// and a multi-line block comment mentioning the types false-positives.
fn references_std_time_type(line: &str) -> bool {
    let code = strip_line_comment(line);
    if code.contains("std::time::Instant") || code.contains("std::time::SystemTime") {
        return true;
    }
    // A glob re-export in any position (`pub use std::time::*;`) imports the
    // types without naming them.
    if code.contains("std::time::*") {
        return true;
    }
    let Some(statement_start) = code.find("use ") else {
        return false;
    };
    let statement = &code[statement_start + "use ".len()..];
    let Some(import_position) = statement.find("std::time") else {
        return false;
    };
    let rest = statement[import_position + "std::time".len()..].trim_start();
    if rest.starts_with("::*")
        || rest.starts_with(" as ")
        || rest.starts_with("as ")
        || rest.starts_with(';')
    {
        // Glob, alias, or plain module import: the imported surface cannot
        // be proven Duration-only.
        return true;
    }
    let Some(names) = rest.strip_prefix("::") else {
        return false;
    };
    if names.contains('{') {
        // A brace import is provably Duration-only only when it closes on
        // this line without naming a time type or `self` (which imports the
        // module itself); an unclosed `use std::time::{` (rustfmt
        // multi-line style) hides the names on later lines.
        let closes_on_line = names.contains('}');
        let names_time = names.contains("Instant") || names.contains("SystemTime");
        return !closes_on_line || names_time || names.contains("self");
    }
    names.contains("Instant") || names.contains("SystemTime")
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn server_src_reads_time_only_through_injectable_or_tokio_seams() {
    let allowlist = std_time_allowlist();
    let mut files = Vec::new();
    collect_rs_files(&repo_root().join("src"), &mut files);
    assert!(
        files.len() > 50,
        "expected to scan the real src/ tree, found {} files",
        files.len()
    );

    let mut violations = Vec::new();
    for path in &files {
        let relative = path
            .strip_prefix(repo_root())
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let exemption = allowlist.get(relative.as_str());
        // Panics loudly on read failure: a scan that silently skips files is
        // a false-negative factory.
        let content = read_file(path);
        // Module-level `#[cfg(test)]`: the marker's next non-blank line must
        // declare a `mod`. A struct-field attribute (e.g. a test-only field
        // mid-file) does not start the test region.
        let mut test_module_start: Option<usize> = None;
        let lines: Vec<&str> = content.lines().collect();
        for (line_number, line) in lines.iter().enumerate() {
            if line.trim() == "#[cfg(test)]" && test_module_start.is_none() {
                // The region starts only if the next non-blank,
                // non-comment, non-attribute line declares a `mod`. A
                // struct-field attribute (e.g. a test-only field mid-file)
                // does not start the test region.
                let next_is_mod = lines[line_number + 1..]
                    .iter()
                    .map(|l| l.trim())
                    .find(|l| !l.is_empty() && !l.starts_with("//") && !l.starts_with("#["))
                    .is_some_and(|next| next.starts_with("mod "));
                if next_is_mod {
                    test_module_start = Some(line_number);
                }
            }
        }
        for (line_number, line) in lines.iter().enumerate() {
            if !references_std_time_type(line) {
                continue;
            }
            match exemption {
                None => violations.push(format!(
                    "{relative}:{}: {line}\n  use tokio::time::Instant (pause-controllable) \
                     or an injected *_at(.., now) seam; allowlist \
                     tests/clock_source_scan.rs only with a sound reason",
                    line_number + 1
                )),
                Some(entry) if entry.test_module_only => {
                    let after_test_marker =
                        test_module_start.is_some_and(|marker| line_number > marker);
                    if !after_test_marker {
                        violations.push(format!(
                            "{relative}:{}: {line}\n  allowlisted as test-module-only, but \
                             this std time reference is not inside the file's test module",
                            line_number + 1
                        ));
                    }
                }
                Some(_) => {}
            }
        }
    }

    assert!(
        violations.is_empty(),
        "std clock sources in server src/ outside the allowlist (breaks the \
         injectable-time convention, .llm/context-testing.md):\n{}",
        violations.join("\n")
    );
}

/// Every allowlist entry must still point at a real file that actually uses
/// a std time type — so stale entries are surfaced for cleanup instead of
/// silently outliving their reason.
#[test]
fn allowlist_entries_stay_relevant() {
    let allowlist = std_time_allowlist();
    assert!(!allowlist.is_empty());
    for (relative, exemption) in &allowlist {
        let path = repo_root().join(relative);
        assert!(path.is_file(), "allowlisted file is missing: {relative}");
        let content = read_file(&path);
        assert!(
            content.lines().any(references_std_time_type),
            "allowlisted file no longer uses a std time type; drop the stale \
             entry from tests/clock_source_scan.rs: {relative} ({})",
            exemption.reason
        );
    }
}

#[test]
fn classifier_matches_imports_and_qualified_uses_only() {
    // Imports.
    assert!(references_std_time_type(
        "use std::time::{Duration, Instant};"
    ));
    assert!(references_std_time_type("use std::time::SystemTime;"));
    // rustfmt multi-line brace form: the type names live on later lines.
    assert!(references_std_time_type("use std::time::{"));
    assert!(references_std_time_type("    use std::time::{"));
    // A brace import that closes on the line without naming a time type is safe.
    assert!(!references_std_time_type("use std::time::{Duration};"));
    // Glob and alias imports hide the surface.
    assert!(references_std_time_type("use std::time::*;"));
    assert!(references_std_time_type("use std::time as stdtime;"));
    // Module import (`use std::time;`) enables `time::Instant::now()` too.
    assert!(references_std_time_type("use std::time;"));
    // Brace-self imports the module with the same capability.
    assert!(references_std_time_type("use std::time::{self, Duration};"));
    // Re-export and nested-statement positions are still seen.
    assert!(references_std_time_type("pub use std::time::*;"));
    assert!(references_std_time_type(
        "mod m { use std::time::Instant; }"
    ));
    // Qualified uses.
    assert!(references_std_time_type(
        "let started = std::time::Instant::now();"
    ));
    assert!(references_std_time_type(
        "let stamp = std::time::SystemTime::now();"
    ));
    // Plain Duration imports and tokio time are fine.
    assert!(!references_std_time_type("use std::time::Duration;"));
    assert!(!references_std_time_type("use std::time::{Duration};"));
    assert!(!references_std_time_type("use tokio::time::Instant;"));
    assert!(!references_std_time_type(
        "let now = Instant::now(); // tokio or injected"
    ));
    assert!(!references_std_time_type(
        "record_at(peer_ip, outcome, Instant::now())"
    ));
    // Comments about the convention are not uses of it...
    assert!(!references_std_time_type(
        "// `tokio::time::Instant` (not `std::time::Instant`) so the reaper pauses"
    ));
    assert!(!references_std_time_type(
        "/// See the injectable-time convention; never call std::time::Instant::now()"
    ));
    // ...and a scheme's `://` must not truncate the code before a real use.
    assert!(references_std_time_type(
        "let url = \"https://example.com\"; let t = std::time::Instant::now();"
    ));
}
