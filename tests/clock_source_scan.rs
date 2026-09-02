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
//! The chrono rule (`Utc::now()` / `Local::now()`) completes the same
//! convention for wall-clock reads (#498). Production chrono reads are
//! classified at each site as
//!   1. `durable record` — absolute-time semantics by design (reconnection
//!      token mints, room lifecycle stamps, TURN credential expiry clients
//!      see, snapshot/gauge stamps),
//!   2. `embedder convenience` — a thin documented wrapper reading the wall
//!      clock so embedders get an honest answer (`is_expired` seams), or
//!   3. `converted` — the decision itself now runs on monotonic tokio time
//!      or an injected seam.
//!
//! A new unallowlisted production chrono read fails
//! `server_src_reads_chrono_time_only_through_documented_or_injected_seams`;
//! every allowlist entry must still have a live production-region match.
//!
//! Scope note: `clients/` is out of scope: its remaining `std` clock sites
//! are upstream-API boundaries (e.g. matchbox `get_stats(std::time::Instant,
//! ...)`) or test code.

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
            "src/auth/middleware.rs",
            Exemption {
                reason: "clock read confined to the thin resolve_app_id wrapper over \
                         the resolve_app_id_at injection seam",
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

/// Files whose production-region `Utc::now()` / `Local::now()` reads are
/// classified (durable record / embedder convenience / observability readout
/// derived from a durable stamp) instead of converted. Every entry must keep
/// at least one live production-region match, and each classified site in the
/// file carries its class at the site (`Wall clock (...):` comments). New
/// reads anywhere else in `src/` fail the scan.
fn chrono_clock_allowlist() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        (
            "src/database/mod.rs",
            "durable stamps: room/player created_at, connected_at, last_activity (GC \
             windows must survive restarts, paired with a monotonic liveness stamp), \
             game_finalized_at; defensive wall fallback in room_idle_for for pre-stamp \
             rows. Cleanup claim/prune decisions run on monotonic time.",
        ),
        (
            "src/distributed.rs",
            "durable stamps: informational lock acquired_at on the embedder-facing \
             handle (lease decisions are monotonic) and DistributedMessage wire stamp.",
        ),
        (
            "src/metrics.rs",
            "durable stamp: MetricsSnapshot timestamp surfaced to dashboard/API \
             consumers.",
        ),
        (
            "src/protocol/room_state.rs",
            "durable stamps: Room::new/update_activity/enter_lobby/finalize_game \
             lifecycle stamps; is_expired is the thin documented embedder wrapper over \
             the is_expired_at(now, ..) decision seam.",
        ),
        (
            "src/reconnection.rs",
            "durable stamps: token created_at/expires_at mints (cross-restart \
             semantics), BufferedEvent replay stamp, claim claimed_at (crash-recovery \
             record), registration UTC capture paired with the monotonic deadline; \
             is_expired seams are documented embedder conveniences.",
        ),
        (
            "src/security/crypto.rs",
            "durable stamp: AAD-authenticated created_at on encrypted bundles.",
        ),
        (
            "src/server/dashboard_cache.rs",
            "durable stamp: snapshot fetched_at surfaced in payloads/history and the \
             last-refresh gauge; the staleness decision reads fetched_at_instant \
             (monotonic).",
        ),
        (
            "src/server/reconnection_service.rs",
            "durable stamp: TURN credential mint (absolute expiry instants clients \
             see), captured once per emission.",
        ),
        (
            "src/server/room_service.rs",
            "durable stamps: PlayerInfo connected_at on room-state rows (including \
             two failure-path fallback constructors).",
        ),
        (
            "src/server/session_policy.rs",
            "durable stamps: TURN credential mints (absolute expiry instants clients \
             see), captured once per emission / at ICE pregather.",
        ),
        (
            "src/server/signaling.rs",
            "durable stamp: TURN credential mint (absolute expiry instants clients \
             see), captured once per emission.",
        ),
        (
            "src/server/spectator_service.rs",
            "durable stamp: SpectatorInfo connected_at on the room-state row.",
        ),
        (
            "src/websocket/metrics.rs",
            "observability readout: cache_age_seconds derived from the snapshot's \
             durable wall stamp for API consumers; the staleness decision is \
             monotonic.",
        ),
        (
            "src/websocket/prometheus.rs",
            "observability readout: dashboard cache age gauge derived from the \
             durable unix last-refresh stamp at scrape time; the staleness decision \
             is monotonic.",
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

// ---------------------------------------------------------------------------
// chrono wall-clock rule (#498)
// ---------------------------------------------------------------------------

/// True if the source line reads a chrono wall clock (`Utc::now()` /
/// `Local::now()`, qualified or imported) or imports chrono in a form that
/// could re-expose those reads under a hidden name (globs and aliases; an
/// unclosed rustfmt multi-line brace import can hide an aliased name on a
/// later line). Plain named chrono imports stay safe: `DateTime<Utc>` typing
/// is legitimate and a clock read through a plain import is still visible as
/// `Utc::now()`/`Local::now()`.
///
/// Matching runs on a copy with `::`-adjacent whitespace squeezed out, so
/// hand-formatted evasion forms (`Utc :: now()`, `use chrono :: Utc as X;`)
/// are caught too. The squeeze also mangles string-literal content on the
/// line — accepted, same safe direction as the `//`-in-string limit of the
/// std classifier.
fn references_chrono_clock(line: &str) -> bool {
    let code = collapse_path_separators(strip_line_comment(line));
    // The bare-path form (`Utc::now` without call parens, e.g. passed as a
    // function reference) is matched too: nothing else in chrono starts with
    // it, so flagging the prefix is safe.
    if code.contains("Utc::now") || code.contains("Local::now") {
        return true;
    }
    let Some(statement_start) = code.find("use chrono") else {
        return false;
    };
    let rest = code[statement_start + "use chrono".len()..].trim_start();
    let Some(path) = rest.strip_prefix("::") else {
        return false;
    };
    // Glob (`use chrono::*;`, `use chrono::prelude::*;`) and alias
    // (`use chrono::Utc as Clock;`) imports hide the clock surface.
    if path.contains('*') || path.contains(" as ") {
        return true;
    }
    // An unclosed rustfmt multi-line brace import hides later lines.
    path.contains('{') && !path.contains('}')
}

/// Squeeze whitespace away from every `::` separator (`Utc :: now()` becomes
/// `Utc::now()`).
fn collapse_path_separators(code: &str) -> String {
    code.split("::")
        .map(|segment| segment.trim())
        .collect::<Vec<_>>()
        .join("::")
}

/// Per-line marker of whether the line sits inside an item gated by
/// `#[cfg(test)]` or `#[cfg(signal_fish_repository_tests)]` (the build script
/// enables the latter for the repository-only test modules), plus the final
/// brace depth of the scan.
///
/// Brace-aware mini-scanner: string, raw-string, and char literals plus
/// (nested) comments are skipped — with lexer state persisting across lines,
/// so multi-line literals cannot leak braces into the depth — and a `'` that
/// is not a char literal (a lifetime) is ignored. When a gate attribute is
/// seen, the next non-attribute, non-blank, non-comment line opens the gated
/// item, and the item's HEAD TOKEN decides how the gate resolves: a
/// brace-bodied or `;`-terminated item (fn with a possibly multi-line
/// signature and where clauses, mod, struct, enum, impl, trait, use, const,
/// static, type) stays open until its `{` or `;`, while a bare inner item
/// (enum variant, struct field, match arm, or a `#[cfg(test)]` fn parameter)
/// closes at its `,` once parens are back to the depth the item head started
/// at — so no later production code can be swallowed by a stuck gate.
///
/// Known limits (accepted): gate recognition is exact-match on the trimmed
/// attribute line, so trailing comments on a gate line or
/// `#[cfg(any(test, ...))]` forms do not gate (consistent with the std rule).
/// The final-depth return exists so tests can assert the lexer returns every
/// real file to depth 0 (`region_lexer_returns_every_src_file_to_depth_zero`).
fn cfg_test_gated_regions(lines: &[&str]) -> (Vec<bool>, i64) {
    let mut in_region = vec![false; lines.len()];
    let mut depth: i64 = 0;
    // Parenthesis balance across lines: a bare inner item (variant, field,
    // match arm, fn parameter) only self-terminates at a `,` written at the
    // paren depth its head started at.
    let mut paren_depth: i64 = 0;
    // Gated items whose region is still open (innermost last). A gate is
    // `AwaitingBody` until its item resolves: a `{` switches it to
    // `DepthWatch`, a `;` closes a brace-less item (`mod foo;`,
    // `const X: u32 = 1;`), and a `,` at the head's paren depth closes a
    // bare inner item.
    let mut open_gates: Vec<(i64, GateBody)> = Vec::new();
    let mut pending_gate: Option<i64> = None;
    // Literal/comment lexer state persists across lines: multi-line raw
    // strings, strings with `\`-continuations, and nested block comments
    // must not leak their braces into the depth.
    let mut lexer: Lexer = Lexer::Normal;

    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let is_gate_attribute =
            trimmed == "#[cfg(test)]" || trimmed == "#[cfg(signal_fish_repository_tests)]";
        if is_gate_attribute {
            pending_gate = Some(depth);
        } else if let Some(gate_depth) = pending_gate {
            let opens_item =
                !trimmed.is_empty() && !trimmed.starts_with("//") && !trimmed.starts_with("#[");
            if opens_item {
                open_gates.push((
                    gate_depth,
                    GateBody::AwaitingBody {
                        body_seeking: item_head_seeks_body(trimmed),
                        open_paren: paren_depth,
                    },
                ));
                pending_gate = None;
            }
        }
        in_region[index] = !open_gates.is_empty();

        // Lex the line for brace/paren depth outside literals/comments.
        let bytes = line.as_bytes();
        let mut cursor = 0;
        let mut saw_open_brace = false;
        let mut saw_semicolon = false;
        while cursor < bytes.len() {
            let byte = bytes[cursor];
            match lexer {
                Lexer::Normal => match byte {
                    b'/' if bytes.get(cursor + 1) == Some(&b'/') => break,
                    b'/' if bytes.get(cursor + 1) == Some(&b'*') => {
                        lexer = Lexer::BlockComment(1);
                        cursor += 2;
                        continue;
                    }
                    b'"' => lexer = Lexer::String,
                    b'r' if bytes.get(cursor + 1) == Some(&b'#')
                        || bytes.get(cursor + 1) == Some(&b'"') =>
                    {
                        // A raw string starter unless the `r` continues a
                        // longer identifier. `br#"..."#` (byte raw string)
                        // is allowed through: its `b` prefix is what makes
                        // the `r` a token start.
                        let continues_ident = (cursor > 0
                            && bytes[cursor - 1].is_ascii_alphanumeric()
                            && bytes[cursor - 1] != b'b')
                            || (cursor > 0 && bytes[cursor - 1] == b'_');
                        if !continues_ident {
                            let mut hashes = 0usize;
                            while bytes.get(cursor + 1 + hashes) == Some(&b'#') {
                                hashes += 1;
                            }
                            if bytes.get(cursor + 1 + hashes) == Some(&b'"') {
                                lexer = Lexer::RawString(hashes);
                                // Skip past the opening `"` (+1) and its
                                // hash marks (+hashes) so the same quote
                                // does not terminate a hashless raw string
                                // immediately.
                                cursor += hashes + 2;
                                continue;
                            }
                        }
                    }
                    b'\'' => {
                        // Char literal only when `'x'` or `'\\x'` closes here;
                        // otherwise it is a lifetime tick.
                        let closes = bytes.get(cursor + 1).is_some_and(|&next| next != b'\'')
                            && (bytes.get(cursor + 2) == Some(&b'\'')
                                || (bytes.get(cursor + 1) == Some(&b'\\')
                                    && bytes.get(cursor + 3) == Some(&b'\'')));
                        if closes {
                            lexer = Lexer::Char;
                        }
                    }
                    b'{' => {
                        depth += 1;
                        saw_open_brace = true;
                        if let Some((_, GateBody::AwaitingBody { .. })) = open_gates.last() {
                            open_gates.last_mut().expect("just inspected").1 = GateBody::DepthWatch;
                        }
                    }
                    b'}' => depth -= 1,
                    b'(' => paren_depth += 1,
                    b')' => paren_depth -= 1,
                    b';' => saw_semicolon = true,
                    _ => {}
                },
                Lexer::String => match byte {
                    b'\\' => cursor += 1,
                    b'"' => lexer = Lexer::Normal,
                    _ => {}
                },
                Lexer::RawString(hashes) => {
                    if byte == b'"'
                        && (1..=hashes).all(|offset| bytes.get(cursor + offset) == Some(&b'#'))
                    {
                        lexer = Lexer::Normal;
                        cursor += hashes;
                    }
                }
                Lexer::Char => match byte {
                    b'\\' => cursor += 1,
                    b'\'' => lexer = Lexer::Normal,
                    _ => {}
                },
                Lexer::BlockComment(nesting) => {
                    if byte == b'*' && bytes.get(cursor + 1) == Some(&b'/') {
                        if nesting == 1 {
                            lexer = Lexer::Normal;
                        } else {
                            lexer = Lexer::BlockComment(nesting - 1);
                        }
                        cursor += 1;
                    } else if byte == b'/' && bytes.get(cursor + 1) == Some(&b'*') {
                        lexer = Lexer::BlockComment(nesting + 1);
                        cursor += 1;
                    }
                }
            }
            cursor += 1;
        }

        // Resolve awaiting-body gates against what this line revealed, then
        // close any depth-watched gate whose body has ended.
        if let Some((
            _,
            GateBody::AwaitingBody {
                body_seeking,
                open_paren,
            },
        )) = open_gates.last_mut()
        {
            if saw_open_brace {
                // The item's body opened (for a fn-like head, possibly after
                // a multi-line signature and where clauses).
                open_gates.last_mut().expect("just inspected").1 = GateBody::DepthWatch;
            } else if saw_semicolon {
                // Brace-less item (`mod foo;`, `use ...;`, `const ...;`,
                // a tuple struct's `);`).
                open_gates.pop();
            } else if !*body_seeking
                && paren_depth == *open_paren
                && strip_line_comment(trimmed).trim().ends_with(',')
            {
                // Self-terminating bare inner item: enum variant (including
                // a multi-line one ending `),`), struct field, match arm, or
                // a `#[cfg(test)]` function parameter.
                open_gates.pop();
            }
            // Otherwise stay open: a body-seeking head continues (`fn f() -> T
            // where ..`), or the bare item's line is still inside its own
            // parens or simply has not terminated yet.
        }
        while let Some(&(gate_depth, GateBody::DepthWatch)) = open_gates.last() {
            if depth <= gate_depth {
                open_gates.pop();
            } else {
                break;
            }
        }
    }
    (in_region, depth)
}

/// True when a gated item's head declares a brace-bodied or `;`-terminated
/// item — a function (whose signature and `where` clauses may span lines), a
/// module, an aggregate, or a declaration — as opposed to a bare inner item
/// (enum variant, struct field, match arm) that self-terminates at a `,`.
fn item_head_seeks_body(trimmed: &str) -> bool {
    let mut rest = trimmed;
    loop {
        // Visibility qualifier: `pub`, `pub(crate)`, `pub (crate)` — note
        // rustfmt glues the scope to `pub` with no space, so it must be
        // stripped as a prefix before first-space tokenization.
        if let Some(stripped) = strip_visibility(rest) {
            rest = stripped;
        }
        let token_end = rest.find(' ').unwrap_or(rest.len());
        let token = &rest[..token_end];
        match token {
            "async" | "const" | "unsafe" | "extern" => {
                rest = rest[token_end..].trim_start();
                // `extern "C"`.
                if token == "extern" && rest.starts_with('"') {
                    if let Some(close) = rest[1..].find('"') {
                        rest = rest[close + 2..].trim_start();
                    }
                }
            }
            "fn" | "mod" | "struct" | "enum" | "union" | "impl" | "trait" | "type" | "static"
            | "use" | "macro_rules!" => return true,
            _ => return false,
        }
    }
}

/// Consume a leading `pub` visibility qualifier (`pub`, `pub(scope)`, or
/// `pub (scope)`), returning the rest of the head, or `None` when the line
/// does not start with one (e.g. the identifier `published`).
fn strip_visibility(rest: &str) -> Option<&str> {
    let after = rest.strip_prefix("pub")?;
    let after = if after.is_empty() || after.starts_with(' ') {
        after.trim_start()
    } else if after.starts_with('(') {
        after
    } else {
        return None;
    };
    if after.starts_with('(') {
        let bytes = after.as_bytes();
        let mut nesting = 0i64;
        let mut cursor = 0;
        while cursor < bytes.len() {
            match bytes[cursor] {
                b'(' => nesting += 1,
                b')' => {
                    nesting -= 1;
                    if nesting == 0 {
                        return Some(after[cursor + 1..].trim_start());
                    }
                }
                _ => {}
            }
            cursor += 1;
        }
        // Unbalanced scope parens: malformed code, do not consume.
        return None;
    }
    Some(after)
}

#[derive(Clone, Copy, PartialEq)]
enum GateBody {
    /// The item's head has been seen but its shape is not yet resolved: a
    /// body-seeking head (fn-like/braced/`;`-terminated) waits for its `{`
    /// or `;` however many lines that takes, while a bare inner item closes
    /// at its `,` once parens return to `open_paren`.
    AwaitingBody {
        /// The head declares a brace-bodied or `;`-terminated item.
        body_seeking: bool,
        /// Paren depth at the item head; a bare inner item's terminating
        /// `,` must bring parens back here.
        open_paren: i64,
    },
    /// The item's body is open; close when depth falls back to the gate.
    DepthWatch,
}

#[derive(Clone, Copy, PartialEq)]
enum Lexer {
    Normal,
    String,
    /// Raw string with `hashes` leading `#` marks (`r#"..."#`).
    RawString(usize),
    Char,
    /// Block comment with `nesting` open `/*` markers.
    BlockComment(usize),
}

/// Resolve the file that declares `mod <stem>;` for a `src/` file, if any:
/// `src/a/b.rs` is declared by `src/a/mod.rs` when that exists, else by the
/// sibling module file `src/a.rs`; `src/a/mod.rs` is declared by `src/a.rs`
/// or the crate root (`lib.rs`/`main.rs`). Returns `None` when no candidate
/// exists (and for the crate roots themselves, which nothing declares).
fn declaring_module_file(path: &Path) -> Option<PathBuf> {
    let relative_dir = path.parent()?;
    let stem = path.file_stem()?.to_str()?;
    if stem == "lib" || stem == "main" {
        return None;
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if stem != "mod" {
        candidates.push(relative_dir.join("mod.rs"));
    }
    if let Some(parent_dir) = relative_dir.parent() {
        if let Some(dir_name) = relative_dir.file_name().and_then(|name| name.to_str()) {
            candidates.push(parent_dir.join(format!("{dir_name}.rs")));
        }
    }
    if stem == "mod" {
        candidates.push(relative_dir.join("lib.rs"));
        candidates.push(relative_dir.join("main.rs"));
    }
    candidates.into_iter().find(|candidate| candidate.is_file())
}

/// True if the file is wired into the crate behind `#[cfg(test)]` /
/// `#[cfg(signal_fish_repository_tests)]` (the repository-only `*_tests.rs`
/// modules), so its whole content is test code.
fn is_test_gated_module_file(path: &Path) -> bool {
    let Some(declaring) = declaring_module_file(path) else {
        return false;
    };
    let content = read_file(&declaring);
    let stem = match path.file_stem().and_then(|stem| stem.to_str()) {
        Some(stem) => stem,
        None => return false,
    };
    let lines: Vec<&str> = content.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let declares_module = trimmed.ends_with(&format!("mod {stem};"))
            && (trimmed.starts_with("mod ")
                || trimmed.starts_with("pub mod ")
                || trimmed.starts_with("pub(crate) mod ")
                || trimmed.starts_with("pub(super) mod "));
        if !declares_module {
            continue;
        }
        // Walk back over the contiguous attribute block.
        for attribute in lines[..index].iter().rev() {
            let attr = attribute.trim();
            if !attr.starts_with("#[") {
                break;
            }
            if attr.contains("cfg(test)") || attr.contains("cfg(signal_fish_repository_tests)") {
                return true;
            }
        }
    }
    false
}

#[test]
fn server_src_reads_chrono_time_only_through_documented_or_injected_seams() {
    let allowlist = chrono_clock_allowlist();
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
        let content = read_file(path);
        let lines: Vec<&str> = content.lines().collect();
        let (regions, _) = cfg_test_gated_regions(&lines);
        for (line_number, line) in lines.iter().enumerate() {
            if regions[line_number] || !references_chrono_clock(line) {
                continue;
            }
            if allowlist.contains_key(relative.as_str()) {
                continue;
            }
            // Whole-file test modules (repository-only `*_tests.rs`) are
            // wired behind the gate in their declaring module file.
            if is_test_gated_module_file(path) {
                continue;
            }
            violations.push(format!(
                "{relative}:{}: {line}\n  classify the read (durable record / embedder \
                 convenience) or convert the decision to monotonic tokio time or an \
                 injected *_at(.., now) seam; allowlist tests/clock_source_scan.rs only \
                 with a sound reason",
                line_number + 1
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "unclassified chrono wall-clock reads in server src/ production code (breaks \
         the injectable-time convention, .llm/context-testing.md, #498):\n{}",
        violations.join("\n")
    );
}

/// Every chrono allowlist entry must still point at a real file with at least
/// one live production-region chrono read, so converted-away entries are
/// surfaced for cleanup instead of silently outliving their reason.
#[test]
fn chrono_allowlist_entries_stay_relevant() {
    let allowlist = chrono_clock_allowlist();
    assert!(!allowlist.is_empty());
    for (relative, reason) in &allowlist {
        let path = repo_root().join(relative);
        assert!(path.is_file(), "allowlisted file is missing: {relative}");
        let content = read_file(&path);
        let lines: Vec<&str> = content.lines().collect();
        let (regions, _) = cfg_test_gated_regions(&lines);
        let live_matches = lines
            .iter()
            .zip(&regions)
            .any(|(line, gated)| !gated && references_chrono_clock(line));
        assert!(
            live_matches,
            "allowlisted file no longer reads a chrono clock in production code; drop \
             the stale entry from tests/clock_source_scan.rs: {relative} ({reason})"
        );
    }
}

#[test]
fn chrono_classifier_matches_clock_reads_and_hiding_imports() {
    // Direct reads, qualified or imported.
    assert!(references_chrono_clock("let now = Utc::now();"));
    assert!(references_chrono_clock("let now = chrono::Utc::now();"));
    assert!(references_chrono_clock("let now = Local::now();"));
    assert!(references_chrono_clock(
        ".then(|| chrono::Utc::now().timestamp())"
    ));
    // Function-reference form without call parens is still a clock read.
    assert!(references_chrono_clock("timestamps.map(Utc::now)"));
    // Hand-formatted evasion forms (path-separator whitespace).
    assert!(references_chrono_clock("let now = Utc :: now();"));
    assert!(references_chrono_clock("let now = chrono :: Utc :: now();"));
    assert!(references_chrono_clock("let now = Utc:: now();"));
    assert!(references_chrono_clock("use chrono :: Utc as Clock;"));
    // Comments about the convention are not uses of it.
    assert!(!references_chrono_clock(
        "// never call Utc::now() in a decision path"
    ));
    // Hiding imports.
    assert!(references_chrono_clock("use chrono::*;"));
    assert!(references_chrono_clock("use chrono::prelude::*;"));
    assert!(references_chrono_clock("use chrono::Utc as Clock;"));
    assert!(references_chrono_clock(
        "use chrono::{DateTime, Utc as Now};"
    ));
    // rustfmt multi-line brace form hides later lines.
    assert!(references_chrono_clock("use chrono::{"));
    // Plain named imports are safe: the read stays visible.
    assert!(!references_chrono_clock("use chrono::Utc;"));
    assert!(!references_chrono_clock("use chrono::{DateTime, Utc};"));
    assert!(!references_chrono_clock("use chrono::{TimeZone, Utc};"));
    // Unrelated chrono types never flag.
    assert!(!references_chrono_clock(
        "fn f(x: chrono::Duration) -> chrono::DateTime<chrono::Utc> { todo!() }"
    ));
}

#[test]
fn cfg_test_region_tracker_handles_items_modules_and_literals() {
    // Module-level gate.
    let lines = [
        "#[cfg(test)]",
        "mod tests {",
        "let x = 1;",
        "}",
        "let y = 2;",
    ];
    let (regions, _) = cfg_test_gated_regions(&lines);
    assert_eq!(regions, vec![false, true, true, true, false]);

    // Item-level gate (e.g. a #[cfg(test)] test helper fn).
    let lines = [
        "#[cfg(test)]",
        "fn helper() {",
        "    let x = format!(\"{ }}{\");",
        "}",
        "fn production() { let y = 1; }",
    ];
    let (regions, _) = cfg_test_gated_regions(&lines);
    assert_eq!(regions, vec![false, true, true, true, false]);

    // Brace-less gated items close immediately.
    let lines = ["#[cfg(test)]", "mod some_tests;", "fn real() {}"];
    let (regions, _) = cfg_test_gated_regions(&lines);
    assert_eq!(regions, vec![false, true, false]);

    // Stacked gates (repository-only module wiring) stay one region.
    let lines = [
        "#[cfg(test)]",
        "#[cfg(signal_fish_repository_tests)]",
        "fn helper() {}",
        "fn real() {}",
    ];
    let (regions, _) = cfg_test_gated_regions(&lines);
    assert_eq!(regions, vec![false, false, true, false]);

    // Nested gated items restore the outer region correctly.
    let lines = [
        "#[cfg(test)]",
        "mod outer {",
        "    #[cfg(test)]",
        "    fn inner() {}",
        "    fn still_test() {}",
        "}",
        "fn real() {}",
    ];
    let (regions, _) = cfg_test_gated_regions(&lines);
    assert_eq!(regions, vec![false, true, true, true, true, true, false]);

    // Non-test cfg attributes do not gate.
    let lines = ["#[cfg(unix)]", "fn f() {}", "fn g() {}"];
    let (regions, _) = cfg_test_gated_regions(&lines);
    assert_eq!(regions, vec![false, false, false]);

    // Strings, comments, and lifetimes do not corrupt brace depth.
    let lines = [
        "fn f() {",
        "    let s = \"}{\"; // } {",
        "    let g: Vec<'a, u8> = Vec::new();",
        "    let c = '{}';",
        "}",
        "fn after() {}",
    ];
    let (regions, final_depth) = cfg_test_gated_regions(&lines);
    assert_eq!(regions, vec![false, false, false, false, false, false]);
    assert_eq!(final_depth, 0);

    // Raw strings — single-line and multi-line (the real config-fixture
    // shape) — and byte raw strings do not corrupt brace depth.
    let lines = [
        "fn f() {",
        "    let a = r#\"{ \"nested\": [1]}\"#;",
        "    let b = r\"{plain}\";",
        "    let c = br#\"{bytes}\"#;",
        "}",
        "fn after() {}",
    ];
    let (regions, final_depth) = cfg_test_gated_regions(&lines);
    assert_eq!(regions, vec![false, false, false, false, false, false]);
    assert_eq!(final_depth, 0);

    let lines = [
        "fn f() {",
        "    let a = r#\"{",
        "  \"nested\": [1],",
        "  \"closing\": \"}\"",
        "}\"#;",
        "    let b = \"multi",
        "line \\\" with brace {\"",
        "    ;",
        "}",
        "fn after() {}",
    ];
    let (regions, final_depth) = cfg_test_gated_regions(&lines);
    assert_eq!(
        regions,
        vec![false, false, false, false, false, false, false, false, false, false]
    );
    assert_eq!(
        final_depth, 0,
        "multi-line raw strings and continued strings must not leak braces"
    );

    // A gated item whose signature spans multiple lines keeps its region
    // open through the signature and body.
    let lines = [
        "#[cfg(test)]",
        "fn helper(",
        "    x: u8,",
        ") -> u8 {",
        "    x + 1",
        "}",
        "fn production() { let y = 1; }",
    ];
    let (regions, _) = cfg_test_gated_regions(&lines);
    assert_eq!(regions, vec![false, true, true, true, true, true, false]);

    // Self-terminating gated inner items (enum variant, struct field) close
    // at their terminating comma: the region must not swallow what follows.
    let lines = [
        "enum RoomLiveness {",
        "    Live(tokio::time::Instant),",
        "    #[cfg(test)]",
        "    AgedFor(std::time::Duration),",
        "}",
        "fn production() { let y = Utc::now(); }",
    ];
    let (regions, _) = cfg_test_gated_regions(&lines);
    assert_eq!(regions, vec![false, false, false, true, false, false]);

    let lines = [
        "struct S {",
        "    a: u8,",
        "    #[cfg(test)]",
        "    only_in_tests: bool,",
        "}",
        "fn production() { let y = Utc::now(); }",
    ];
    let (regions, _) = cfg_test_gated_regions(&lines);
    assert_eq!(regions, vec![false, false, false, true, false, false]);

    // A signature with a where clause stays gated through the clause and
    // into the body.
    let lines = [
        "#[cfg(test)]",
        "fn bounded() -> impl Iterator<Item = u8>",
        "where",
        "    (): Sized,",
        "{",
        "    std::iter::empty()",
        "}",
        "fn production() { let y = Utc::now(); }",
    ];
    let (regions, _) = cfg_test_gated_regions(&lines);
    assert_eq!(
        regions,
        vec![false, true, true, true, true, true, true, false]
    );

    // A multi-line enum variant closes at its terminating `),` — a stuck
    // gate here would swallow every later production read.
    let lines = [
        "enum RoomLiveness {",
        "    Live(tokio::time::Instant),",
        "    #[cfg(test)]",
        "    AgedFor(",
        "        std::time::Duration,",
        "    ),",
        "}",
        "fn production() { let y = Utc::now(); }",
    ];
    let (regions, _) = cfg_test_gated_regions(&lines);
    assert_eq!(
        regions,
        vec![false, false, false, true, true, true, false, false]
    );

    // A `#[cfg(test)]` fn parameter inside a production signature closes at
    // its own comma: the production function's body must stay ungated.
    let lines = [
        "fn f(",
        "    a: u8,",
        "    #[cfg(test)]",
        "    b: u8,",
        ") {",
        "    let y = Utc::now();",
        "}",
    ];
    let (regions, _) = cfg_test_gated_regions(&lines);
    assert_eq!(
        regions,
        vec![false, false, false, true, false, false, false]
    );

    // A tuple struct declaration is a body-seeking head: it closes at its
    // `;`, never at an interior field comma.
    let lines = [
        "#[cfg(test)]",
        "struct Point(",
        "    u8,",
        "    u8,",
        ");",
        "fn production() { let y = Utc::now(); }",
    ];
    let (regions, _) = cfg_test_gated_regions(&lines);
    assert_eq!(regions, vec![false, true, true, true, true, false]);

    // Visibility-qualified heads stay body-seeking through a where clause
    // whose bound list ends in a comma: rustfmt glues the scope to `pub`
    // (`pub(crate)` with no space), which must not degrade the head to a
    // bare comma-terminated item.
    let lines = [
        "#[cfg(test)]",
        "pub(crate) fn pick<T>(x: T) -> T",
        "where",
        "    T: Default,",
        "{",
        "    let _ = Utc::now();",
        "    x",
        "}",
        "fn production() { let y = Utc::now(); }",
    ];
    let (regions, _) = cfg_test_gated_regions(&lines);
    assert_eq!(
        regions,
        vec![false, true, true, true, true, true, true, true, false]
    );

    // Spaced (`pub (crate)`) and declaration (`pub(super) const`)
    // visibility forms are body-seeking too.
    let lines = [
        "#[cfg(test)]",
        "pub (crate) fn g() {",
        "    let x = 1;",
        "}",
        "fn p() {}",
    ];
    let (regions, _) = cfg_test_gated_regions(&lines);
    assert_eq!(regions, vec![false, true, true, true, false]);

    let lines = [
        "#[cfg(test)]",
        "pub(super) const X: u32 = 1;",
        "fn p() { let y = 1; }",
    ];
    let (regions, _) = cfg_test_gated_regions(&lines);
    assert_eq!(regions, vec![false, true, false]);

    // An identifier merely starting with "pub" (`published`) is a bare
    // inner item: it still closes at its comma.
    let lines = [
        "enum E {",
        "    A(u8),",
        "    #[cfg(test)]",
        "    published(u8),",
        "}",
        "fn production() { let y = Utc::now(); }",
    ];
    let (regions, _) = cfg_test_gated_regions(&lines);
    assert_eq!(regions, vec![false, false, false, true, false, false]);

    // Nested block comments are tracked, so they cannot hide a gate's body.
    let lines = [
        "fn f() {",
        "    /* outer /* inner } */ still comment } */",
        "    let x = 1;",
        "}",
        "fn after() {}",
    ];
    let (regions, _) = cfg_test_gated_regions(&lines);
    assert_eq!(regions, vec![false, false, false, false, false]);
}

/// End-to-end wiring: the production scan loop's exact classification over
/// synthetic file content — gated test code is skipped, allowlisted
/// production reads pass, and an unallowlisted production read produces a
/// violation naming the file and line.
/// The region lexer must return every real `src/` file to brace depth 0:
/// a nonzero final depth means a literal or comment was modeled incorrectly
/// and every region classification after that point in the file is
/// untrustworthy.
#[test]
fn region_lexer_returns_every_src_file_to_depth_zero() {
    let mut files = Vec::new();
    collect_rs_files(&repo_root().join("src"), &mut files);
    assert!(files.len() > 50, "expected the real src/ tree");

    let mut drifted = Vec::new();
    for path in &files {
        let content = read_file(path);
        let lines: Vec<&str> = content.lines().collect();
        let (_, final_depth) = cfg_test_gated_regions(&lines);
        if final_depth != 0 {
            drifted.push(format!(
                "{}: final brace depth {final_depth}",
                path.strip_prefix(repo_root())
                    .unwrap_or(path)
                    .to_string_lossy()
            ));
        }
    }
    assert!(
        drifted.is_empty(),
        "the region lexer models literals/comments incorrectly in these files (region \
         classification after the drift point is untrustworthy):\n{}",
        drifted.join("\n")
    );
}

#[test]
fn chrono_scan_wiring_flags_unallowlisted_production_reads_only() {
    let allowlist: BTreeMap<&'static str, &'static str> =
        BTreeMap::from([("src/allowlisted.rs", "durable stamps only")]);

    let classify = |relative: &str, content: &str| {
        let lines: Vec<&str> = content.lines().collect();
        let (regions, _) = cfg_test_gated_regions(&lines);
        let mut violations = Vec::new();
        for (line_number, line) in lines.iter().enumerate() {
            if regions[line_number] || !references_chrono_clock(line) {
                continue;
            }
            if allowlist.contains_key(relative) {
                continue;
            }
            violations.push(format!("{relative}:{}", line_number + 1));
        }
        violations
    };

    let content = "\
use chrono::Utc;
fn mint() -> i64 {
    Utc::now().timestamp()
}
#[cfg(test)]
fn test_helper() {
    let _ = Utc::now();
}
";
    // The gated test read is skipped; the production read flags with its
    // exact location, and the plain import never flags.
    assert_eq!(classify("src/plain.rs", content), vec!["src/plain.rs:3"]);
    // The same file passes wholesale when it is allowlisted.
    assert!(classify("src/allowlisted.rs", content).is_empty());

    let violating = "\
fn mint() -> i64 {
    Utc::now().timestamp()
}
";
    assert_eq!(classify("src/plain.rs", violating), vec!["src/plain.rs:2"]);
    // Same content inside an allowlisted file passes.
    assert!(classify("src/allowlisted.rs", violating).is_empty());
    // (Whole-file test modules — the repository-only `*_tests.rs` wiring —
    // are skipped by `is_test_gated_module_file` on the real file tree; the
    // main scan passing with zero violations over src/ proves that path.)
}
