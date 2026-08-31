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
//!      delegates to an `*_at(..., now: Instant)` injected-time variant.
//!
//! A new `use std::time::...Instant/SystemTime...` import (or a qualified
//! `std::time::Instant::now()` / `std::time::SystemTime::now()` call) in
//! `src/` therefore fails this guard unless the file is on the explicit
//! allowlist below with a stated reason. `clients/` is out of scope: its
//! remaining `std` clock sites are upstream-API boundaries (e.g. matchbox
//! `get_stats(std::time::Instant, ...)`) or test code.

#![cfg(test)]

mod common;

use common::{read_file, repo_root};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Files allowed to name `std::time::Instant` / `std::time::SystemTime`,
/// each with the reason the allowlisting is sound. Everything else in `src/`
/// must use tokio time or an injected-time seam.
fn std_time_allowlist() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        (
            "src/auth/rate_limiter.rs",
            "clock reads confined to the thin check_rate_limit/cleanup wrappers \
             over the check_rate_limit_at/cleanup_at injection seams",
        ),
        (
            "src/websocket/upgrade_rejection_log.rs",
            "clock read confined to the thin record() wrapper over record_at",
        ),
        (
            "src/websocket/metrics.rs",
            "clock read confined to the thin record() wrapper over record_at",
        ),
        (
            "src/server/shutdown.rs",
            "wall-clock SystemTime epoch stamp in the shutdown log line only; \
             monotonic shutdown logic uses tokio time",
        ),
        ("src/main.rs", "test module only: startup-timing assertion"),
        (
            "src/coordination/mod.rs",
            "test module only: progress deadline loop",
        ),
    ])
}

/// True if the source line references a `std` time type (an import naming
/// `Instant`/`SystemTime`, or a qualified `std::time::...` use). Line comments
/// are stripped first so documentation *about* the convention does not trip
/// the guard.
fn references_std_time_type(line: &str) -> bool {
    let code = line.split("//").next().unwrap_or("");
    let trimmed = code.trim();
    let is_std_time_import = trimmed.starts_with("use std::time::")
        && (trimmed.contains("Instant") || trimmed.contains("SystemTime"));
    // Only qualified std paths count: a bare `Instant::now()` may resolve to
    // the pause-controllable tokio type depending on the file's imports, and
    // the import rule above already covers files that resolve it to std.
    let is_qualified_use =
        code.contains("std::time::Instant") || code.contains("std::time::SystemTime");
    is_std_time_import || is_qualified_use
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
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let relative = path
            .strip_prefix(repo_root())
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if allowlist.contains_key(relative.as_str()) {
            continue;
        }
        for (line_number, line) in content.lines().enumerate() {
            if references_std_time_type(line) {
                violations.push(format!(
                    "{relative}:{}: {line}\n  use tokio::time::Instant (pause-controllable) \
                     or an injected *_at(.., now) seam; allowlist \
                     tests/clock_source_scan.rs only with a sound reason",
                    line_number + 1
                ));
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
    for (relative, reason) in &allowlist {
        let path = repo_root().join(relative);
        assert!(path.is_file(), "allowlisted file is missing: {relative}");
        let content = read_file(&path);
        assert!(
            content.lines().any(references_std_time_type),
            "allowlisted file no longer uses a std time type; drop the stale \
             entry from tests/clock_source_scan.rs: {relative} ({reason})"
        );
    }
}

#[test]
fn classifier_matches_imports_and_qualified_uses_only() {
    assert!(references_std_time_type(
        "use std::time::{Duration, Instant};"
    ));
    assert!(references_std_time_type("use std::time::SystemTime;"));
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
    // Comments about the convention are not uses of it.
    assert!(!references_std_time_type(
        "// `tokio::time::Instant` (not `std::time::Instant`) so the reaper pauses"
    ));
    assert!(!references_std_time_type(
        "/// See the injectable-time convention; never call std::time::Instant::now()"
    ));
}
