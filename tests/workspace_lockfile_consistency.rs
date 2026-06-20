//! Workspace lockfile consistency guard.
//!
//! Every **git-tracked** `Cargo.lock` that references the workspace crate
//! `signal-fish-server` MUST record the same version as the root `Cargo.toml`
//! `[package].version`.
//!
//! ## Why this exists (the class of bug it prevents)
//!
//! The nested crate `clients/native` depends on the root crate via a `path`
//! dependency, so it carries its OWN committed `Cargo.lock` that pins
//! `signal-fish-server` at a concrete version. When the root version bumps
//! (e.g. `0.2.0` -> `0.3.0`), that nested lockfile keeps pinning the OLD version
//! until it is regenerated. CI builds the nested crate with `--locked`, which
//! then fails with the cryptic:
//!
//! ```text
//! error: the lock file .../clients/native/Cargo.lock needs to be updated
//!        but --locked was passed to prevent this
//! ```
//!
//! That is exactly what broke the Browser Interop and WebRTC Interop jobs after
//! a version bump: a confusing, late failure (after a multi-minute cold build)
//! in a path-filtered workflow that does not even run on most PRs.
//!
//! This guard turns that whole class into an instant, actionable failure in the
//! always-on main test suite. It is offline (pure file parsing — no registry, no
//! network), sub-millisecond, deterministic across platforms, and **discovers
//! lockfiles from git** so any future committed nested crate is covered
//! automatically with no list to keep in sync.
//!
//! ## Scope: only *committed* lockfiles
//!
//! Discovery uses `git ls-files`, so the protected set is exactly the lockfiles
//! under version control — the only ones that can go stale-in-git and break a
//! `--locked` build. Gitignored lockfiles (e.g. `fuzz/Cargo.lock`, which is
//! regenerated fresh and built *without* `--locked` because cargo-fuzz rejects
//! it) are correctly excluded; requiring one would wrongly fail on a clean
//! checkout where it does not exist.

#![cfg(test)]

mod common;

use common::{read_file, repo_root};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The workspace crate that committed nested lockfiles pin via a path dep.
const ROOT_CRATE: &str = "signal-fish-server";

/// Committed nested lockfile that must always be covered. Discovery is dynamic,
/// but this is asserted present so a broken discovery can never make the guard
/// pass vacuously — mirroring the "missing client manifest is a hard failure"
/// philosophy in `msrv_consistency_script_tests.rs`. (`fuzz/Cargo.lock` is
/// deliberately absent here: it is gitignored and built without `--locked`.)
const REQUIRED_NESTED_LOCK: &str = "clients/native/Cargo.lock";

#[test]
fn every_tracked_cargo_lock_pins_current_root_crate_version() {
    let root = repo_root();
    let expected = root_crate_version(&root);
    let locks = tracked_cargo_locks(&root);

    let mut checked: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    for lock in &locks {
        let rel = lock
            .strip_prefix(&root)
            .unwrap_or(lock)
            .display()
            .to_string();
        let text = read_file(lock);
        match locked_root_crate_version(&text) {
            // Lockfiles that do not reference the workspace crate are irrelevant
            // to this guard (none today, but stay future-proof).
            None => continue,
            Some(found) if found == expected => checked.push(rel),
            Some(found) => {
                checked.push(rel.clone());
                problems.push(format!(
                    "  - {rel}  pins {ROOT_CRATE} {found}, but root Cargo.toml is {expected}\n    \
                     fix: cargo update -p {ROOT_CRATE} --manifest-path {rel}"
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "Stale committed lockfile(s) detected — these pin an outdated {ROOT_CRATE} version and \
         will fail `--locked` CI builds (e.g. the interop workflows) with a cryptic \
         \"lock file needs to be updated\" error:\n{}\n\n\
         The root Cargo.toml version is the single source of truth; regenerate the lockfile(s) \
         above and commit them.",
        problems.join("\n")
    );

    // Discovery sanity: a broken walk/ls-files must fail loudly, not pass empty.
    assert!(
        checked
            .iter()
            .any(|rel| rel.replace('\\', "/") == REQUIRED_NESTED_LOCK),
        "expected to verify the committed nested lockfile `{REQUIRED_NESTED_LOCK}`, but only \
         checked {checked:?}; lockfile discovery may be broken or the crate moved (update \
         REQUIRED_NESTED_LOCK in tests/workspace_lockfile_consistency.rs)"
    );
}

/// Parse `[package].version` from the root `Cargo.toml`.
///
/// A tiny hand-rolled section scan (no toml dependency): read `version = "..."`
/// inside the first `[package]` table. Mirrors the awk in
/// `scripts/check-doc-consistency.sh` so the two stay behaviorally aligned.
fn root_crate_version(root: &Path) -> String {
    let manifest = read_file(&root.join("Cargo.toml"));
    let mut in_package = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed == "[package]" {
            in_package = true;
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_package = false; // any other table header ends [package]
            continue;
        }
        if in_package {
            if let Some(version) = parse_version_assignment(trimmed) {
                return version;
            }
        }
    }
    panic!("could not parse [package].version from root Cargo.toml");
}

/// Extract the value of a `version = "X"` (or `'X'`) assignment, ignoring
/// surrounding whitespace. Returns `None` for any other line.
fn parse_version_assignment(line: &str) -> Option<String> {
    let rest = line.strip_prefix("version")?.trim_start();
    let rest = rest.strip_prefix('=')?.trim();
    let value = rest
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| rest.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))?;
    Some(value.to_string())
}

/// Find the version `signal-fish-server` is pinned at inside a `Cargo.lock`.
///
/// Cargo writes each `[[package]]` block as `name` then `version` on the next
/// line; we still scan forward to the first `version = "..."` within the block
/// (bounded by the next `[[package]]` / blank line) so a future cargo lockfile
/// field-ordering change cannot silently break the parse.
fn locked_root_crate_version(lock_text: &str) -> Option<String> {
    let needle = format!("name = \"{ROOT_CRATE}\"");
    let mut lines = lock_text.lines();
    while let Some(line) = lines.next() {
        if line.trim() != needle {
            continue;
        }
        for block_line in lines.by_ref() {
            let trimmed = block_line.trim();
            if trimmed.is_empty() || trimmed.starts_with("[[") {
                break; // end of this package block without a version
            }
            if let Some(version) = parse_version_assignment(trimmed) {
                return Some(version);
            }
        }
        return None;
    }
    None
}

/// Committed `Cargo.lock` paths (absolute), discovered via `git ls-files` — the
/// exact "what is under version control" contract, and the only lockfiles that
/// can drift in git and break a `--locked` build.
///
/// Integration tests always run inside the git checkout (CI checks out with
/// `.git`; the repo's sibling guards likewise shell out to git), so a
/// missing/failed git or an empty result is a loud hard error here rather than a
/// silently-degraded scan that could pass vacuously.
fn tracked_cargo_locks(root: &Path) -> Vec<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z", "--", "*Cargo.lock", "Cargo.lock"])
        .output()
        .unwrap_or_else(|e| {
            panic!("`git ls-files` failed ({e}); this guard requires a git checkout")
        });
    assert!(
        output.status.success(),
        "`git ls-files` exited with {}; this guard requires a git checkout",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let locks: Vec<PathBuf> = stdout
        .split('\0')
        .filter(|rel| !rel.is_empty())
        .map(|rel| root.join(rel))
        .collect();
    assert!(
        !locks.is_empty(),
        "`git ls-files` found no tracked Cargo.lock files; lockfile discovery is broken"
    );
    locks
}
