//! Integration tests for `scripts/run-mutants.sh`, the single source of truth
//! for running cargo-mutants fast (mold linker, `--in-place`, slice sharding,
//! the `mutants` build profile, `--baseline=skip`, no `--all-features`).
//!
//! These tests exercise the script's `--print-cmd` mode (also reachable via the
//! `MUTANTS_DRY_RUN=1` env var), which prints the exact command it WOULD run and
//! exits without launching a real mutation pass. That makes the tests fast and
//! lets them lock the performance-critical flags forever without paying for a
//! full mutation run.
//!
//! Mirrors the structure of tests/workflow_hygiene_script_tests.rs: each test
//! invokes `bash` through `common::bash_command()` (which scrubs nested-Cargo
//! instrumentation env so a parent `cargo test` cannot leak RUSTFLAGS into the
//! script) and runs from the repository root.

#![cfg(test)]

mod common;

#[cfg(unix)]
use common::{bash_command, repo_root, strip_comment_lines};

/// Run `scripts/run-mutants.sh` with `args` from the repository root and return
/// `(success, stdout, stderr)`.
///
/// The working directory is the real repo root (not a temp fixture) because the
/// script reads no other repo files in `--print-cmd` mode; it only assembles and
/// prints the command. Nested-Cargo env is scrubbed by `bash_command()` so the
/// script's RUSTFLAGS-append assertions observe a clean baseline unless a test
/// explicitly re-adds RUSTFLAGS.
#[cfg(unix)]
fn run_mutants_script(args: &[&str]) -> (bool, String, String) {
    let output = bash_command()
        .arg("scripts/run-mutants.sh")
        .args(args)
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|e| panic!("Failed to run scripts/run-mutants.sh {args:?}: {e}"));

    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

#[cfg(unix)]
#[test]
fn test_run_mutants_script_exists_and_has_strict_mode() {
    let script_path = repo_root().join("scripts/run-mutants.sh");
    assert!(
        script_path.exists(),
        "scripts/run-mutants.sh must exist: it is the single source of truth for the\n\
         mutation-testing command shared by .github/workflows/mutation.yml and local devs.\n\
         File: {}",
        script_path.display()
    );

    let body = common::read_file(&script_path);
    // The shebang is itself a `#`-prefixed line, so assert it on the raw body;
    // the live (comment-stripped) view is used only for the strict-mode presence
    // check so commenting the real `set -euo pipefail` line cannot pass silently.
    let body_live = strip_comment_lines(&body);

    assert!(
        body.starts_with("#!/usr/bin/env bash"),
        "scripts/run-mutants.sh must start with the `#!/usr/bin/env bash` shebang so it runs\n\
         under bash regardless of the invoking shell.\n\
         File: {}",
        script_path.display()
    );

    assert!(
        body_live.contains("set -euo pipefail"),
        "scripts/run-mutants.sh must enable strict mode (`set -euo pipefail`) so an unset\n\
         variable or a failing command aborts the script instead of silently running a\n\
         malformed mutation command.\n\
         Fix: add `set -euo pipefail` near the top of scripts/run-mutants.sh.\n\
         File: {}",
        script_path.display()
    );
}

#[cfg(unix)]
#[test]
fn test_run_mutants_print_cmd_uses_mold_in_place_slice() {
    let (success, stdout, stderr) = run_mutants_script(&["--shard", "0/16", "--print-cmd"]);
    assert!(
        success,
        "scripts/run-mutants.sh --shard 0/16 --print-cmd must exit 0.\n\
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // Every flag here is a measured performance lever (mold relink, in-place
    // build reuse, slice sharding for incremental-compilation locality, the
    // trimmed `mutants` profile, the green-gate baseline skip, and the pinned
    // Cargo.lock). Losing any of them regresses per-shard wall-clock or
    // correctness. The script is the single source of truth, so assert them
    // here rather than duplicating the flag list in the workflow.
    let required = [
        "-fuse-ld=mold",
        "--in-place",
        "--sharding slice",
        "--profile mutants",
        "--baseline=skip",
        "--cargo-arg=--locked",
        "--shard 0/16",
    ];
    for needle in required {
        assert!(
            stdout.contains(needle),
            "scripts/run-mutants.sh --shard 0/16 --print-cmd output is missing `{needle}`.\n\
             This is a performance/correctness lever from the mutation-speed contract\n\
             (mold linker + --in-place + slice sharding + mutants profile + --baseline=skip +\n\
             --locked). See .llm/skills/mutation-testing-performance.md.\n\
             Fix: restore `{needle}` to the shard command in scripts/run-mutants.sh.\n\
             Verify: bash scripts/run-mutants.sh --shard 0/16 --print-cmd\n\
             Full output:\n{stdout}"
        );
    }
}

#[cfg(unix)]
#[test]
fn test_run_mutants_print_cmd_omits_all_features() {
    let (success, stdout, stderr) = run_mutants_script(&["--shard", "0/16", "--print-cmd"]);
    assert!(
        success,
        "scripts/run-mutants.sh --shard 0/16 --print-cmd must exit 0.\n\
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // The scoped mutation modules contain ZERO `#[cfg(feature = ...)]`, so
    // --all-features cannot change the mutant set or the catch outcome; it only
    // drags the heavy tls / legacy-fullmesh deps into every per-mutant relink.
    assert!(
        !stdout.contains("--all-features"),
        "scripts/run-mutants.sh must NOT pass --all-features: the scoped modules have no\n\
         feature gates, so enabling tls/legacy-fullmesh only slows every per-mutant build\n\
         without changing the mutant set or catch outcome.\n\
         Fix: remove --all-features from scripts/run-mutants.sh.\n\
         Verify: bash scripts/run-mutants.sh --shard 0/16 --print-cmd\n\
         Full output:\n{stdout}"
    );
}

#[cfg(unix)]
#[test]
fn test_run_mutants_rustflags_appends_not_clobbers() {
    // A pre-existing RUSTFLAGS (e.g. from a profile/devcontainer) must be
    // preserved: the script APPENDS the mold flags, it never clobbers. If it
    // overwrote RUSTFLAGS, the warm cache fingerprint would diverge from the
    // shards, defeating the shared-cache reuse the whole script exists for.
    let output = bash_command()
        .arg("scripts/run-mutants.sh")
        .args(["--shard", "1/16", "--print-cmd"])
        .current_dir(repo_root())
        // bash_command() scrubbed RUSTFLAGS; re-add a sentinel so we can prove
        // it survives alongside the appended mold flags.
        .env("RUSTFLAGS", "-C debuginfo=0")
        .output()
        .expect("Failed to run scripts/run-mutants.sh with a preset RUSTFLAGS");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "scripts/run-mutants.sh --shard 1/16 --print-cmd must exit 0.\n\
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );

    assert!(
        stdout.contains("-fuse-ld=mold"),
        "scripts/run-mutants.sh must add the mold linker to RUSTFLAGS.\n\
         Full output:\n{stdout}"
    );
    assert!(
        stdout.contains("-C debuginfo=0"),
        "scripts/run-mutants.sh must APPEND to a pre-existing RUSTFLAGS, not clobber it:\n\
         the preset `-C debuginfo=0` disappeared from the printed RUSTFLAGS.\n\
         Why this matters: clobbering RUSTFLAGS would change the build fingerprint vs. the\n\
         warm shared cache, forcing every shard into a cold rebuild.\n\
         Fix: keep `export RUSTFLAGS=\"... ${{RUSTFLAGS:-}}\"` (append form) in run-mutants.sh.\n\
         Verify: RUSTFLAGS='-C debuginfo=0' bash scripts/run-mutants.sh --shard 1/16 --print-cmd\n\
         Full output:\n{stdout}"
    );
}

#[cfg(unix)]
#[test]
fn test_run_mutants_rejects_missing_shard() {
    // `--shard` with no value must fail fast (exit 2) rather than running a
    // malformed/empty shard. This guards CI from silently doing nothing.
    let (success, stdout, stderr) = run_mutants_script(&["--shard"]);
    assert!(
        !success,
        "scripts/run-mutants.sh --shard (no value) must exit non-zero, not run a malformed shard.\n\
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("ERROR") || combined.to_lowercase().contains("usage"),
        "scripts/run-mutants.sh --shard (no value) must print a usage/error message explaining\n\
         the <k>/<N> requirement.\n\
         Fix: keep the `--shard` argument-validation branch in run-mutants.sh.\n\
         Combined output:\n{combined}"
    );
}

#[cfg(unix)]
#[test]
fn test_run_mutants_rejects_malformed_shard() {
    // A non `<k>/<N>` shard value must be rejected with exit 2 BEFORE any
    // expensive build, even with --print-cmd.
    let (success, stdout, stderr) = run_mutants_script(&["--shard", "abc", "--print-cmd"]);
    assert!(
        !success,
        "scripts/run-mutants.sh --shard abc --print-cmd must fail (malformed shard).\n\
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );

    let code = bash_command()
        .arg("scripts/run-mutants.sh")
        .args(["--shard", "abc", "--print-cmd"])
        .current_dir(repo_root())
        .status()
        .expect("Failed to run scripts/run-mutants.sh --shard abc")
        .code();
    assert_eq!(
        code,
        Some(2),
        "scripts/run-mutants.sh must exit with code 2 on a malformed --shard value (got {code:?}).\n\
         Fix: keep the `grep -Eq '^[0-9]+/[0-9]+$'` validation guard in run-mutants.sh.\n\
         Verify: bash scripts/run-mutants.sh --shard abc --print-cmd; echo $?"
    );

    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("ERROR") || combined.to_lowercase().contains("must be"),
        "scripts/run-mutants.sh --shard abc must print an error explaining the <k>/<N> form.\n\
         Combined output:\n{combined}"
    );
}

#[cfg(unix)]
#[test]
fn test_run_mutants_warm_mode_uses_exact_unmutated_oracle() {
    // The --warm mode is the baseline job's green-gate AND cache-warmer. It must
    // run the exact unmutated nextest oracle used by cargo-mutants: --lib scope,
    // the Cargo `mutants` build profile, the nextest `mutants` runtime profile,
    // and the locked dependency graph.
    let (success, stdout, stderr) = run_mutants_script(&["--warm", "--print-cmd"]);
    assert!(
        success,
        "scripts/run-mutants.sh --warm --print-cmd must exit 0.\n\
         stdout:\n{stdout}\nstderr:\n{stderr}"
    );

    let cargo_lines: Vec<_> = stdout
        .lines()
        .filter(|line| line.starts_with("cargo "))
        .collect();
    assert_eq!(
        cargo_lines,
        ["cargo nextest run --lib --cargo-profile mutants --profile mutants --locked"],
        "scripts/run-mutants.sh --warm --print-cmd must print exactly the unmutated \
         cargo-mutants oracle. `--cargo-profile mutants` selects the matching Cargo build \
         profile; `--profile mutants` selects nextest's per-test 10s termination policy.\n\
         Fix: restore the exact nextest command in scripts/run-mutants.sh.\n\
         Verify: bash scripts/run-mutants.sh --warm --print-cmd\n\
         Full output:\n{stdout}"
    );
}
