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
//! `--locked` build. The standalone native and fuzz packages both commit their
//! lockfiles; cargo-fuzz itself rejects `--locked`, so its workflow performs a
//! locked Cargo metadata preflight before invoking cargo-fuzz.

#![cfg(test)]

mod common;

use common::{read_file, repo_root};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The workspace crate that committed nested lockfiles pin via a path dep.
const ROOT_CRATE: &str = "signal-fish-server";

/// Committed nested lockfiles that must always be covered. Discovery is dynamic,
/// but these are asserted present so broken discovery can never pass vacuously
/// — mirroring the "missing client manifest is a hard failure" philosophy in
/// `msrv_consistency_script_tests.rs`.
const REQUIRED_NESTED_LOCKS: &[&str] = &["clients/native/Cargo.lock", "fuzz/Cargo.lock"];

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
        let versions = locked_root_crate_versions(&text);
        match versions.as_slice() {
            // Lockfiles that do not reference the workspace crate are irrelevant
            // to this guard (none today, but stay future-proof).
            [] => continue,
            [found] if found == &expected => checked.push(rel),
            [found] => {
                checked.push(rel.clone());
                let manifest = manifest_for_lockfile(&rel);
                problems.push(format!(
                    "  - {rel}  pins {ROOT_CRATE} {found}, but root Cargo.toml is {expected}\n    \
                     fix: run `cargo update -p {ROOT_CRATE}` using manifest path {manifest:?}"
                ));
            }
            _ => problems.push(format!(
                "  - {rel} contains {} unsourced {ROOT_CRATE} package entries; expected exactly one",
                versions.len()
            )),
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
    for required in REQUIRED_NESTED_LOCKS {
        assert!(
            checked
                .iter()
                .any(|rel| rel.replace('\\', "/") == *required),
            "expected to verify the committed nested lockfile `{required}`, but only checked \
             {checked:?}; lockfile discovery may be broken or the crate moved (update \
             REQUIRED_NESTED_LOCKS in tests/workspace_lockfile_consistency.rs)"
        );
    }
}

fn manifest_for_lockfile(lockfile: &str) -> String {
    let normalized = lockfile.replace('\\', "/");
    match normalized.strip_suffix("/Cargo.lock") {
        Some(directory) => format!("{directory}/Cargo.toml"),
        None => "Cargo.toml".to_string(),
    }
}

#[test]
fn lockfile_diagnostics_point_to_cargo_manifests() {
    for (lockfile, expected) in [
        ("Cargo.lock", "Cargo.toml"),
        ("clients/native/Cargo.lock", "clients/native/Cargo.toml"),
        ("fuzz\\Cargo.lock", "fuzz/Cargo.toml"),
        (
            "tools/replay client/Cargo.lock",
            "tools/replay client/Cargo.toml",
        ),
    ] {
        assert_eq!(manifest_for_lockfile(lockfile), expected, "{lockfile}");
    }
    assert_eq!(
        format!("{:?}", "tools/replay client's/Cargo.toml"),
        "\"tools/replay client's/Cargo.toml\""
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
    parse_string_assignment(line, "version")
}

fn parse_string_assignment(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.trim_start();
    let rest = rest.strip_prefix('=')?.trim();
    let quote = rest
        .chars()
        .next()
        .filter(|quote| matches!(quote, '"' | '\''))?;
    let rest = &rest[quote.len_utf8()..];
    let closing = rest.find(quote)?;
    let value = &rest[..closing];
    let suffix = rest[closing + quote.len_utf8()..].trim();
    if !suffix.is_empty() && !suffix.starts_with('#') {
        return None;
    }
    Some(value.to_string())
}

/// Find the version `signal-fish-server` is pinned at inside a `Cargo.lock`.
///
/// Cargo writes each `[[package]]` block as `name` then `version` on the next
/// line; we still scan forward to the first `version = "..."` within the block
/// (bounded by the next `[[package]]` / blank line) so a future cargo lockfile
/// field-ordering change cannot silently break the parse.
fn locked_root_crate_versions(lock_text: &str) -> Vec<String> {
    lock_text
        .split("[[package]]")
        .skip(1)
        .filter_map(|block| {
            let mut is_root = false;
            let mut has_source = false;
            let mut version = None;
            for line in block.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("[[") {
                    break;
                }
                is_root |= parse_string_assignment(trimmed, "name").as_deref() == Some(ROOT_CRATE);
                has_source |= parse_string_assignment(trimmed, "source").is_some();
                if version.is_none() {
                    version = parse_version_assignment(trimmed);
                }
            }
            (is_root && !has_source).then_some(version).flatten()
        })
        .collect()
}

#[test]
fn lockfile_parser_distinguishes_path_and_registry_packages() {
    let path = "[[package]] # local table\nname = \"signal-fish-server\" # local package\nversion = \"1.2.3\" # local version\n";
    let registry = "[[package]]\nname = \"signal-fish-server\"\nversion = \"9.9.9\"\nsource=\"registry+https://github.com/rust-lang/crates.io-index\" # registry package\nchecksum = \"abc\"\n";

    for (description, contents, expected) in [
        ("path only", path.to_string(), vec!["1.2.3".to_string()]),
        ("registry only", registry.to_string(), Vec::new()),
        (
            "mixed registry and path",
            format!("{registry}\n{path}"),
            vec!["1.2.3".to_string()],
        ),
    ] {
        assert_eq!(
            locked_root_crate_versions(&contents),
            expected,
            "{description}"
        );
    }
}

#[test]
fn tracked_root_path_dependencies_do_not_pin_the_root_version() {
    let root = repo_root();
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["ls-files", "-z", "--", ":(glob)**/Cargo.toml"])
        .output()
        .expect("discover tracked Cargo manifests");
    assert!(output.status.success(), "git manifest discovery failed");

    let mut checked = 0;
    let mut problems = Vec::new();
    for relative in String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|path| !path.is_empty())
    {
        checked += 1;
        let manifest = root.join(relative);
        let metadata = Command::new("cargo")
            .args([
                "metadata",
                "--locked",
                "--format-version",
                "1",
                "--no-deps",
                "--manifest-path",
            ])
            .arg(&manifest)
            .output()
            .unwrap_or_else(|error| panic!("run cargo metadata for {relative}: {error}"));
        assert!(
            metadata.status.success(),
            "cargo metadata failed for {relative}:\n{}",
            String::from_utf8_lossy(&metadata.stderr)
        );
        if metadata_has_versioned_root_path_dependency(&metadata.stdout) {
            problems.push(relative.to_string());
        }
    }
    assert!(checked > 0, "git found no tracked Cargo.toml files");
    assert!(
        problems.is_empty(),
        "Local {ROOT_CRATE} path dependencies must omit redundant `version` constraints so \
         patch, minor, and major release preparation all remain valid. Remove `version` from:\n  - {}",
        problems.join("\n  - ")
    );
}

fn metadata_has_versioned_root_path_dependency(metadata: &[u8]) -> bool {
    let metadata: serde_json::Value =
        serde_json::from_slice(metadata).expect("cargo metadata must be valid JSON");
    metadata["packages"]
        .as_array()
        .expect("cargo metadata packages must be an array")
        .iter()
        .flat_map(|package| {
            package["dependencies"]
                .as_array()
                .expect("cargo metadata dependencies must be an array")
        })
        .any(|dependency| {
            dependency["name"] == ROOT_CRATE
                && dependency["source"].is_null()
                && dependency["path"].is_string()
                && dependency["req"] != "*"
        })
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
        .args(["ls-files", "-z", "--", ":(glob)**/Cargo.lock"])
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
