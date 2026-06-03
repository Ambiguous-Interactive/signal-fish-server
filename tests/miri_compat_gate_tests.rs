//! Parser-contract regression tests for `scripts/check-miri-compat.sh`.
//!
//! These live in their own file (not ci_config_tests.rs) because the fixtures
//! embed Rust source — including `fn` and `#[test]` lines — as string literals,
//! which would otherwise confuse the line-scanning meta-tests in that file.
//!
//! Gated to unix: the guard script needs bash + python3, absent on the Windows
//! CI leg.

#![cfg(unix)]

mod common;

use common::{bash_command, repo_root, unique_temp_dir, write_file};
use std::path::Path;
use std::process::Output;

/// Run the gate against a specific scan directory.
fn run_gate(script: &Path, scan_dir: &Path) -> Output {
    bash_command()
        .arg(script)
        .arg(scan_dir)
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|e| panic!("Failed to run {}: {e}", script.display()))
}

/// A wall-clock token that appears only inside a string, raw string, char
/// literal, or comment must NOT be flagged — and Rust lifetimes must survive
/// the literal/comment stripping intact.
#[test]
fn gate_ignores_wall_clock_in_strings_comments_and_preserves_lifetimes() {
    let root = repo_root();
    let script = root.join("scripts/check-miri-compat.sh");
    let dir = unique_temp_dir("miri-gate-clean");

    write_file(
        &dir.path().join("sample_tests.rs"),
        r##"
fn echo<'a>(x: &'a str) -> &'a str { x }
#[test]
fn mentions_clock_only_in_noncode() {
    let note = "remember to call Utc::now() somewhere"; // Utc::now() in a comment too
    let raw = r#"SystemTime::now() lives in a raw string"#;
    let quote = '"';
    assert_eq!(echo("ok"), "ok");
    let _ = (note, raw, quote);
}
"##,
    );

    let out = run_gate(&script, dir.path());
    assert!(
        out.status.success(),
        "Gate must not flag wall-clock tokens that appear only in strings/comments, \
         and must not corrupt lifetimes.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A non-ignored test that reaches a wall-clock call transitively through helper
/// functions MUST be flagged (exit 1) with the full reachability chain.
#[test]
fn gate_flags_transitive_wall_clock_reach_through_helpers() {
    let root = repo_root();
    let script = root.join("scripts/check-miri-compat.sh");
    let dir = unique_temp_dir("miri-gate-dirty");

    write_file(
        &dir.path().join("sample_tests.rs"),
        r#"
fn stamp() -> i64 { chrono::Utc::now().timestamp() }
fn build() -> i64 { stamp() }
#[test]
fn reaches_clock_via_helper() {
    assert!(build() > 0);
}
"#,
    );

    let out = run_gate(&script, dir.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "Gate must flag a non-ignored test that reaches a wall-clock call through \
         helpers.\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("reaches_clock_via_helper -> build -> stamp"),
        "Gate should report the transitive reachability chain.\nstdout: {stdout}"
    );
}

/// Rust block comments NEST (`/* a /* b */ c */`). A clock-reaching helper named
/// only inside a nested comment (never actually called) must NOT be flagged,
/// while the same comment alongside a real call MUST be flagged. This pins the
/// scanner's nesting behavior, which a non-greedy regex cannot express.
#[test]
fn gate_handles_nested_block_comments() {
    let root = repo_root();
    let script = root.join("scripts/check-miri-compat.sh");

    let clean = unique_temp_dir("miri-gate-nested-clean");
    write_file(
        &clean.path().join("sample_tests.rs"),
        r#"
fn helper() -> i64 { chrono::Utc::now().timestamp() }
#[test]
fn only_mentions_helper_in_nested_comment() {
    /* outer /* inner */ helper() is still inside the outer comment */
    assert_eq!(1 + 1, 2);
}
"#,
    );
    let clean_out = run_gate(&script, clean.path());
    assert!(
        clean_out.status.success(),
        "A helper named only inside a nested block comment must not be flagged.\n\
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&clean_out.stdout),
        String::from_utf8_lossy(&clean_out.stderr)
    );

    let dirty = unique_temp_dir("miri-gate-nested-dirty");
    write_file(
        &dirty.path().join("sample_tests.rs"),
        r#"
fn helper() -> i64 { chrono::Utc::now().timestamp() }
#[test]
fn calls_helper_after_nested_comment() {
    /* outer /* inner */ comment */
    assert!(helper() >= 0);
}
"#,
    );
    let dirty_out = run_gate(&script, dirty.path());
    assert_eq!(
        dirty_out.status.code(),
        Some(1),
        "A real helper call after a nested comment must still be flagged.\n\
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&dirty_out.stdout),
        String::from_utf8_lossy(&dirty_out.stderr)
    );
}

/// An ignored test (`#[cfg_attr(miri, ignore)]`) may reach a wall-clock call —
/// that is the sanctioned escape hatch and must NOT be flagged.
#[test]
fn gate_allows_wall_clock_in_ignored_tests() {
    let root = repo_root();
    let script = root.join("scripts/check-miri-compat.sh");
    let dir = unique_temp_dir("miri-gate-ignored");

    write_file(
        &dir.path().join("sample_tests.rs"),
        r#"
#[test]
#[cfg_attr(miri, ignore)]
fn needs_real_time() {
    let _ = chrono::Utc::now();
}
"#,
    );

    let out = run_gate(&script, dir.path());
    assert!(
        out.status.success(),
        "Gate must not flag a wall-clock call inside a #[cfg_attr(miri, ignore)] test.\n\
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
