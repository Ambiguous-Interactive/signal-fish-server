#![cfg(test)]

//! Guards for `scripts/dev-loop.sh`, the scoped red-green loop accelerator.
//!
//! The script institutionalizes the target-scoped iteration rule from
//! `.llm/context.md`: resolve a test-name pattern to its owning target with
//! grep, then run one scoped `cargo nextest` invocation instead of the bare
//! filterset that rebuilds every test binary. These tests drive the script in
//! a fixture repository with `--dry-run` so resolution and command shaping
//! stay pinned without compiling anything.

mod common;

use common::{bash_command, repo_root, unique_temp_dir, write_file};
use std::fs;
use std::path::Path;

#[test]
fn editor_keeps_save_checks_out_of_the_agent_build_loop() {
    let path = ".vscode/settings.json";
    let raw = fs::read_to_string(repo_root().join(path)).expect("read editor settings");
    let settings: serde_json::Value =
        serde_json::from_str(&common::strip_comment_lines(&raw)).expect("parse editor settings");
    assert_eq!(
        settings["rust-analyzer.checkOnSave"], false,
        "{path}: saving must not launch a full Clippy sweep that locks scoped Cargo runs"
    );
    assert_eq!(
        settings["rust-analyzer.check.command"], "clippy",
        "{path}: manual editor checks must retain Clippy"
    );
    assert_eq!(
        settings["rust-analyzer.check.allTargets"], true,
        "{path}: manual editor checks must retain all targets"
    );
    assert_eq!(
        settings["rust-analyzer.cargo.features"], "all",
        "{path}: manual editor checks must retain all features"
    );
}

fn copy_dev_loop_script(temp_root: &Path) {
    let script_src = repo_root().join("scripts/dev-loop.sh");
    let script_dst = temp_root.join("scripts/dev-loop.sh");
    let script = fs::read_to_string(&script_src)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", script_src.display()));
    write_file(&script_dst, &script);
}

/// Build a fixture repository: `tests/foo.rs` and `tests/bar.rs` integration
/// targets plus a `src/lib.rs` carrying a unit test module.
fn seed_fixture_repo(temp_root: &Path) {
    write_file(
        &temp_root.join("tests/foo.rs"),
        "#[test]\nfn test_alpha_runs() {}\n\n#[test]\nfn mid_name_alpha_helper() {}\n",
    );
    write_file(
        &temp_root.join("tests/bar.rs"),
        "#[test]\nfn test_other() {}\n",
    );
    // Flavor-form attributes must also signal a runnable target.
    write_file(
        &temp_root.join("tests/burst.rs"),
        "#[tokio::test(flavor = \"multi_thread\", worker_threads = 2)]\nasync fn test_flavor_preserves_progress() {}\n",
    );
    // A proptest-only target: its property names are runnable even though
    // the source has no `#[test]` attribute outside the macro input.
    write_file(
        &temp_root.join("tests/prop.rs"),
        "proptest! {\n    fn test_prop_wire_shape(x in any::<u8>()) {\n        prop_assert!(x >= 0);\n    }\n}\n",
    );
    // A shared harness module: helper names appear here, but it runs nothing.
    write_file(
        &temp_root.join("tests/harness.rs"),
        "pub fn start_test_server_with_config_and_protocol() {}\n",
    );
    // Tests inside a helper module run under every top-level target that
    // includes the module directory.
    write_file(
        &temp_root.join("tests/util/proxy.rs"),
        "#[test]\nfn test_chunk_pacing_in_helper() {}\n",
    );
    write_file(
        &temp_root.join("tests/consumer.rs"),
        "mod util;\n\n#[test]\nfn test_consumer_own() {}\n",
    );
    write_file(
        &temp_root.join("src/lib.rs"),
        "#[cfg(test)]\nmod protocol_tests {\n    #[test]\n    fn test_alpha_unit() {}\n}\n",
    );
}

fn run_dev_loop(args: &[&str]) -> (i32, String) {
    let temp_root = unique_temp_dir("dev-loop-script");
    copy_dev_loop_script(temp_root.path());
    seed_fixture_repo(temp_root.path());

    let output = bash_command()
        .arg("scripts/dev-loop.sh")
        .args(args)
        .current_dir(temp_root.path())
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "Failed to run dev-loop.sh in {}: {e}",
                temp_root.path().display()
            )
        });

    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    (
        output.status.code().unwrap_or(-1),
        combined.replace("\r\n", "\n"),
    )
}

#[test]
fn test_dev_loop_resolves_the_owning_target_and_scopes_the_run() {
    let (code, output) = run_dev_loop(&["--dry-run", "test_alpha_runs"]);
    assert_eq!(code, 0, "resolution must succeed: {output}");
    assert!(
        output.contains("--test foo"),
        "the integration owner must be scoped by target stem: {output}"
    );
    assert!(
        output.contains("--no-tests fail"),
        "a scoped owner without matching tests must fail: {output}"
    );
    assert!(
        output.contains("test_alpha_runs"),
        "the user pattern must flow through to the nextest filter: {output}"
    );
    assert!(
        !output.contains("--test bar"),
        "targets that do not own the pattern must not run: {output}"
    );
    assert!(
        !output.contains("--lib"),
        "a pattern absent from src/ must not schedule the unit-test target: {output}"
    );
}

#[test]
fn test_dev_loop_owns_substring_patterns_and_unit_targets() {
    // Substring ownership mirrors nextest's `test(...)` filter: a pattern may
    // match the middle of a test name, and a match under src/ adds --lib.
    let (code, output) = run_dev_loop(&["--dry-run", "mid_name_alpha"]);
    assert_eq!(code, 0, "substring resolution must succeed: {output}");
    assert!(
        output.contains("--test foo"),
        "substring matches resolve to the owning integration target: {output}"
    );
}

#[test]
fn test_dev_loop_reports_patterns_with_no_owning_target() {
    let (code, output) = run_dev_loop(&["--dry-run", "test_absent_everywhere"]);
    assert_eq!(code, 1, "an unresolvable pattern must fail: {output}");
    assert!(
        output.contains("no owning target found for 'test_absent_everywhere'"),
        "the failure must name the pattern and the searched surface: {output}"
    );
}

#[test]
fn test_dev_loop_owns_proptest_targets_and_skips_bare_helper_modules() {
    // A `proptest!`-only file owns its property names without a `#[test]`
    // attribute; a helper-only module never owns anything runnable.
    let (code, output) = run_dev_loop(&["--dry-run", "test_prop_wire_shape"]);
    assert_eq!(code, 0, "proptest ownership must resolve: {output}");
    assert!(
        output.contains("--test prop"),
        "the proptest-only target must be scheduled: {output}"
    );

    let (code, output) = run_dev_loop(&["--dry-run", "start_test_server_with_config_and_protocol"]);
    assert_eq!(
        code, 1,
        "a helper-only module must not become a phantom owner: {output}"
    );
    assert!(
        output.contains("no owning target found"),
        "a helper-only match must surface the actionable no-owner message: {output}"
    );
}

#[test]
fn test_dev_loop_owns_flavor_form_and_helper_module_tests() {
    // `#[tokio::test(flavor = ...)]` must signal a runnable target.
    let (code, output) = run_dev_loop(&["--dry-run", "test_flavor_preserves_progress"]);
    assert_eq!(code, 0, "flavor-form ownership must resolve: {output}");
    assert!(
        output.contains("--test burst"),
        "the flavor-form target must be scheduled: {output}"
    );

    // A helper-module test runs under every target that includes the module.
    let (code, output) = run_dev_loop(&["--dry-run", "test_chunk_pacing_in_helper"]);
    assert_eq!(code, 0, "helper-module ownership must resolve: {output}");
    assert!(
        output.contains("--test consumer"),
        "the module-including target must be scheduled: {output}"
    );
    assert!(
        !output.contains("--test burst") && !output.contains("--test prop"),
        "targets that do not include the module must not run: {output}"
    );
}

#[test]
fn test_dev_loop_forwards_options_and_accepts_multiple_patterns() {
    let (code, output) = run_dev_loop(&[
        "--dry-run",
        "--all-features",
        "--clippy",
        "=test_alpha_unit",
        "mod::test_other",
    ]);
    assert_eq!(code, 0, "option forwarding must succeed: {output}");
    assert!(
        output.contains("--all-features"),
        "feature flags must reach the scoped cargo invocations: {output}"
    );
    assert!(
        output.contains("--lib"),
        "an exact pattern under src/ resolves to the unit-test target: {output}"
    );
    assert!(
        output.contains("--test bar"),
        "module-path prefixes are stripped before ownership resolution: {output}"
    );
    assert!(
        output.contains("cargo clippy"),
        "--clippy must schedule the scoped clippy pass: {output}"
    );
}

/// Patterns that resolve to the same owning target run in ONE scoped
/// invocation (`-E 'test(a) or test(b)'`): a multi-pattern red-green loop
/// pays one build, not one per pattern (issue #512, session 266).
#[test]
fn test_dev_loop_merges_same_target_patterns_into_one_invocation() {
    let (code, output) = run_dev_loop(&["--dry-run", "test_alpha_runs", "mid_name_alpha_helper"]);
    assert_eq!(code, 0, "same-target patterns must resolve: {output}");
    let nextest_runs = output
        .lines()
        .filter(|line| line.contains("cargo nextest run"))
        .count();
    assert_eq!(
        nextest_runs, 1,
        "patterns sharing one owning target must merge into one invocation: {output}"
    );
    assert!(
        output.contains("\\ or\\"),
        "the merged filterset must OR the patterns: {output}"
    );
    assert!(
        output.contains("test_alpha_runs") && output.contains("mid_name_alpha_helper"),
        "both patterns must survive into the merged filterset: {output}"
    );
}

#[test]
fn test_dev_loop_usage_error_without_patterns() {
    let (code, output) = run_dev_loop(&["--dry-run"]);
    assert_eq!(
        code, 2,
        "a pattern-less invocation is a usage error: {output}"
    );
    assert!(
        output.contains("Usage:"),
        "the usage error must print the help text: {output}"
    );
}

/// Prepare the fixture repository for `--changed`: commit the seeded baseline
/// so working-tree deltas (and untracked files) are expressible.
fn commit_fixture_repo(temp_root: &Path) {
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(temp_root)
            .env("GIT_AUTHOR_NAME", "dev-loop-test")
            .env("GIT_AUTHOR_EMAIL", "dev-loop-test@example.com")
            .env("GIT_COMMITTER_NAME", "dev-loop-test")
            .env("GIT_COMMITTER_EMAIL", "dev-loop-test@example.com")
            .status()
            .expect("run git for the dev-loop --changed fixture");
        assert!(
            status.success(),
            "git {args:?} failed in {}",
            temp_root.display()
        );
    };
    git(&["init", "--quiet"]);
    git(&["add", "-A"]);
    git(&["commit", "--quiet", "--allow-empty", "-m", "baseline"]);
}

#[test]
fn test_dev_loop_changed_mode_runs_every_owning_target_without_a_name_filter() {
    let temp_root = unique_temp_dir("dev-loop-script-changed");
    copy_dev_loop_script(temp_root.path());
    seed_fixture_repo(temp_root.path());
    commit_fixture_repo(temp_root.path());

    // A src delta (unit tests), a tracked integration delta, and an untracked
    // brand-new target must all resolve; a client-crate delta stays advisory.
    write_file(
        &temp_root.path().join("src/lib.rs"),
        "#[cfg(test)]\nmod protocol_tests {\n    #[test]\n    fn test_alpha_unit_changed() {}\n}\n",
    );
    write_file(
        &temp_root.path().join("tests/bar.rs"),
        "#[test]\nfn test_other_changed() {}\n",
    );
    write_file(
        &temp_root.path().join("tests/fresh.rs"),
        "#[test]\nfn test_fresh_target() {}\n",
    );

    let (code, output) = run_dev_loop_changed(temp_root.path(), &["--dry-run", "--changed"]);
    assert_eq!(code, 0, "changed-mode resolution must succeed: {output}");
    assert!(
        output.contains("--lib"),
        "a src/ delta schedules the unit-test target: {output}"
    );
    assert!(
        output.contains("--test bar"),
        "the tracked integration delta schedules its target: {output}"
    );
    assert!(
        output.contains("--test fresh"),
        "an untracked new target is owned too: {output}"
    );
    assert!(
        output.contains("--test async_timeout_policy_scan"),
        "a root-crate delta schedules the source-policy scanners: {output}"
    );
    assert!(
        !output.contains("-E test("),
        "changed mode runs whole targets, never name-filtered: {output}"
    );
    assert!(
        !output.contains("--test foo") && !output.contains("--test burst"),
        "targets with no delta must not run: {output}"
    );
}

/// Run dev-loop inside an already-prepared fixture repo (no re-seed), merging
/// stdout+stderr like [`run_dev_loop`].
fn run_dev_loop_changed(temp_root: &Path, args: &[&str]) -> (i32, String) {
    let output = bash_command()
        .arg("scripts/dev-loop.sh")
        .args(args)
        .current_dir(temp_root)
        .output()
        .unwrap_or_else(|e| panic!("Failed to run dev-loop.sh in {}: {e}", temp_root.display()));
    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    (
        output.status.code().unwrap_or(-1),
        combined.replace("\r\n", "\n"),
    )
}

#[test]
fn test_dev_loop_changed_mode_runs_every_target_including_a_changed_helper_module() {
    let temp_root = unique_temp_dir("dev-loop-script-changed-helper");
    copy_dev_loop_script(temp_root.path());
    seed_fixture_repo(temp_root.path());
    commit_fixture_repo(temp_root.path());

    write_file(
        &temp_root.path().join("tests/util/proxy.rs"),
        "#[test]\nfn test_chunk_pacing_in_helper_changed() {}\n",
    );

    let (code, output) = run_dev_loop_changed(temp_root.path(), &["--dry-run", "--changed"]);
    assert_eq!(code, 0, "helper-module resolution must succeed: {output}");
    assert!(
        output.contains("--test consumer"),
        "the module-including target must be scheduled: {output}"
    );
    assert!(
        !output.contains("--test foo") && !output.contains("--test bar"),
        "targets that do not include the module must not run: {output}"
    );
}

#[test]
fn test_dev_loop_changed_mode_clean_tree_and_non_crate_deltas() {
    let temp_root = unique_temp_dir("dev-loop-script-changed-clean");
    copy_dev_loop_script(temp_root.path());
    seed_fixture_repo(temp_root.path());
    commit_fixture_repo(temp_root.path());

    // A clean tree is a clean no-op: exit 0, no cargo invocation.
    let (code, output) = run_dev_loop_changed(temp_root.path(), &["--dry-run", "--changed"]);
    assert_eq!(code, 0, "an empty delta must not fail: {output}");
    assert!(
        output.contains("nothing to run"),
        "the no-op must be stated, not silent: {output}"
    );
    assert!(
        !output.contains("cargo nextest"),
        "a no-op must not schedule work: {output}"
    );

    // A client delta without a manifest must report incomplete verification.
    write_file(
        &temp_root.path().join("clients/native/src/lib.rs"),
        "// changed\n",
    );
    let (code, output) = run_dev_loop_changed(temp_root.path(), &["--dry-run", "--changed"]);
    assert_eq!(
        code, 1,
        "a client delta without an owning manifest must fail: {output}"
    );
    assert!(
        output.contains("belongs to a client crate"),
        "the advisory must redirect to the client crate's own suite: {output}"
    );
    assert!(
        !output.contains("cargo nextest"),
        "a clients/ delta must not schedule root-crate work: {output}"
    );
}

#[test]
fn test_dev_loop_changed_mode_validates_the_script_itself() {
    let temp_root = unique_temp_dir("dev-loop-script-changed-self");
    copy_dev_loop_script(temp_root.path());
    seed_fixture_repo(temp_root.path());
    commit_fixture_repo(temp_root.path());

    // A dev-loop.sh edit is not a .rs delta, so the ownership mapping cannot
    // see it; the script must still schedule its own contract suite, and must
    // not schedule root-crate test work for a script-only delta.
    let script_path = temp_root.path().join("scripts/dev-loop.sh");
    let script = fs::read_to_string(&script_path).expect("read the copied dev-loop script");
    write_file(&script_path, &format!("{script}\n# touched\n"));

    let (code, output) = run_dev_loop_changed(temp_root.path(), &["--dry-run", "--changed"]);
    assert_eq!(code, 0, "a script-only delta must resolve: {output}");
    assert!(
        output.contains("--test dev_loop_script_tests"),
        "a dev-loop.sh edit must schedule its contract suite: {output}"
    );
    assert!(
        !output.contains("--lib"),
        "a script-only delta schedules no root-crate test work: {output}"
    );
    assert!(
        !output.contains("ignoring non-crate Rust change ''"),
        "the empty changed-files list must not iterate as an empty path: {output}"
    );
}

#[test]
fn test_dev_loop_changed_mode_usage_and_base_ref_handling() {
    let temp_root = unique_temp_dir("dev-loop-script-changed-usage");
    copy_dev_loop_script(temp_root.path());
    seed_fixture_repo(temp_root.path());
    commit_fixture_repo(temp_root.path());

    // An invalid base ref is a usage error, not a silent full diff.
    let (code, output) =
        run_dev_loop_changed(temp_root.path(), &["--dry-run", "--changed", "not-a-ref"]);
    assert_eq!(code, 2, "an invalid ref must be a usage error: {output}");
    assert!(
        output.contains("not a valid git ref"),
        "the failure must name the offending ref: {output}"
    );

    // A pattern-like argument after --changed is rejected.
    let (code, output) = run_dev_loop_changed(
        temp_root.path(),
        &["--dry-run", "--changed", "test_alpha_runs"],
    );
    assert_eq!(
        code, 2,
        "patterns after --changed are a usage error: {output}"
    );

    // An explicit base ref diffs against that ref: the baseline commit itself
    // is clean against HEAD, so nothing runs.
    let (code, output) =
        run_dev_loop_changed(temp_root.path(), &["--dry-run", "--changed", "HEAD"]);
    assert_eq!(code, 0, "an explicit clean base must resolve: {output}");
    assert!(
        output.contains("nothing to run"),
        "a clean base ref is a no-op: {output}"
    );
}

#[test]
fn test_dev_loop_batches_different_targets_in_one_build() {
    let (code, output) = run_dev_loop(&["--dry-run", "--clippy", "test_alpha_runs", "test_other"]);
    assert_eq!(code, 0, "resolution must succeed: {output}");
    assert_eq!(
        output
            .lines()
            .filter(|line| line.contains("cargo nextest run"))
            .count(),
        1,
        "one package must pay for one build: {output}"
    );
    assert_eq!(
        output
            .lines()
            .filter(|line| line.contains("cargo clippy"))
            .count(),
        1,
        "one package must pay for one lint pass: {output}"
    );
}

#[cfg(unix)]
#[test]
fn test_dev_loop_propagates_empty_selection_from_real_cargo_process() {
    use std::os::unix::fs::PermissionsExt;
    let temp_root = unique_temp_dir("dev-loop-empty-selection");
    copy_dev_loop_script(temp_root.path());
    seed_fixture_repo(temp_root.path());
    let bin = temp_root.path().join("bin");
    let cargo = bin.join("cargo");
    write_file(
        &cargo,
        r#"#!/bin/sh
case "$1 $2" in
    'nextest list') printf '{"rust-suites":{"fixture":{"testcases":{"test_alpha_runs":{"filter-match":{"status":"matches"},"ignored":false}}}}}\n' ;;
    'nextest run') printf '%s\n' "$*" > "$DEV_LOOP_CARGO_LOG"; exit 4 ;;
    *) printf '{}\n' ;;
esac
"#,
    );
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755))
        .expect("make cargo fixture executable");
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").expect("PATH"),
    ));
    let output = bash_command()
        .arg("scripts/dev-loop.sh")
        .arg("test_alpha_runs")
        .env("PATH", std::env::join_paths(paths).expect("fixture PATH"))
        .env("DEV_LOOP_CARGO_LOG", temp_root.path().join("executed"))
        .current_dir(temp_root.path())
        .output()
        .expect("run real script with cargo fixture");
    let actual_run = fs::read_to_string(temp_root.path().join("executed"))
        .expect("fixture must reach test execution");
    assert!(
        actual_run.contains("--no-tests fail"),
        "execution must fail on empty selection: {actual_run}"
    );
    assert!(
        !output.status.success(),
        "empty selections must propagate as failures: {output:?}"
    );
}

#[test]
fn test_dev_loop_changed_client_routes_to_owning_package_once() {
    let temp_root = unique_temp_dir("dev-loop-client-routing");
    copy_dev_loop_script(temp_root.path());
    seed_fixture_repo(temp_root.path());
    write_file(
        &temp_root.path().join("clients/native/Cargo.toml"),
        "[package]\nname = \"native-client\"\n",
    );
    commit_fixture_repo(temp_root.path());
    write_file(
        &temp_root.path().join("clients/native/src/lib.rs"),
        "// changed\n",
    );
    write_file(
        &temp_root.path().join("clients/native/src/helper.rs"),
        "// changed\n",
    );
    let (code, output) = run_dev_loop_changed(temp_root.path(), &["--dry-run", "--changed"]);
    assert_eq!(code, 0, "client package must be scheduled: {output}");
    assert!(
        output.contains("--manifest-path clients/native/Cargo.toml"),
        "owning manifest required: {output}"
    );
    assert_eq!(
        output
            .lines()
            .filter(|line| line.contains("cargo nextest run"))
            .count(),
        1,
        "client changes share one build: {output}"
    );
}

#[test]
fn test_dev_loop_tooling_owner_does_not_receive_server_features() {
    let temp_root = unique_temp_dir("dev-loop-tooling-owner");
    copy_dev_loop_script(temp_root.path());
    seed_fixture_repo(temp_root.path());
    write_file(
        &temp_root.path().join("tools/repo-checks/Cargo.toml"),
        "[package]\nname = \"repo-checks\"\n[[test]]\nname = \"bar\"\npath = \"../../tests/bar.rs\"\n",
    );
    let (code, output) = run_dev_loop_changed(
        temp_root.path(),
        &[
            "--dry-run",
            "--features",
            "tls",
            "test_alpha_runs",
            "test_other",
        ],
    );
    assert_eq!(code, 0, "both packages resolve: {output}");
    let commands: Vec<_> = output
        .lines()
        .filter(|line| line.starts_with("DRY-RUN:"))
        .collect();
    assert_eq!(commands.len(), 2, "each package has one build: {output}");
    assert!(
        commands
            .iter()
            .any(|line| line.contains("--features tls") && line.contains("--test foo")),
        "server receives its features: {output}"
    );
    assert!(
        commands.iter().any(
            |line| line.contains("--manifest-path tools/repo-checks/Cargo.toml")
                && !line.contains("--features")
        ),
        "tooling must not receive server feature names: {output}"
    );
}

#[test]
fn test_dev_loop_changed_mode_rejects_multiple_base_refs() {
    let (code, output) = run_dev_loop(&["--dry-run", "--changed", "HEAD", "HEAD"]);
    assert_eq!(code, 2, "multiple bases must be rejected: {output}");
}

#[cfg(unix)]
#[test]
fn test_dev_loop_rejects_an_empty_pattern_beside_a_valid_pattern() {
    use std::os::unix::fs::PermissionsExt;
    let temp_root = unique_temp_dir("dev-loop-partial-empty-selection");
    copy_dev_loop_script(temp_root.path());
    seed_fixture_repo(temp_root.path());
    write_file(
        &temp_root.path().join("tests/foo.rs"),
        "#[test]\nfn test_alpha_runs() {}\nfn non_runnable_helper() {}\n",
    );
    let bin = temp_root.path().join("bin");
    let cargo = bin.join("cargo");
    write_file(
        &cargo,
        r#"#!/bin/sh
if [ "$1 $2" = 'nextest list' ]; then
    case "$*" in
        *'test(non_runnable_helper)'*) status=mismatch ;;
        *) status=matches ;;
    esac
    printf '{"rust-suites":{"fixture":{"testcases":{"test_alpha_runs":{"filter-match":{"status":"%s"},"ignored":false}}}}}\n' "$status"
elif [ "$1" = metadata ]; then
    printf '{}\n'
else
    echo executed > "$DEV_LOOP_CARGO_LOG"
fi
"#,
    );
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755))
        .expect("make cargo fixture executable");
    let log = temp_root.path().join("executed");
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").expect("PATH"),
    ));
    let output = bash_command()
        .arg("scripts/dev-loop.sh")
        .args(["test_alpha_runs", "non_runnable_helper"])
        .env("PATH", std::env::join_paths(paths).expect("fixture PATH"))
        .env("DEV_LOOP_CARGO_LOG", &log)
        .current_dir(temp_root.path())
        .output()
        .expect("run script with discovery fixture");
    assert!(
        !output.status.success(),
        "a valid sibling must not hide an empty request: {output:?}"
    );
    assert!(
        !log.exists(),
        "reject incomplete selections before running any tests"
    );
}

#[test]
fn test_dev_loop_owns_tooling_sources_and_root_build_inputs() {
    let temp_root = unique_temp_dir("dev-loop-tooling-sources");
    copy_dev_loop_script(temp_root.path());
    seed_fixture_repo(temp_root.path());
    write_file(
        &temp_root.path().join("tools/repo-checks/Cargo.toml"),
        "[package]\nname = \"repo-checks\"\n[[test]]\nname = \"ownership\"\npath = \"tests/ownership.rs\"\n",
    );
    write_file(
        &temp_root
            .path()
            .join("tools/repo-checks/tests/ownership.rs"),
        "#[test]\nfn test_tooling_ownership() {}\n",
    );
    commit_fixture_repo(temp_root.path());
    let (code, output) =
        run_dev_loop_changed(temp_root.path(), &["--dry-run", "test_tooling_ownership"]);
    assert_eq!(code, 0, "tooling test pattern must resolve: {output}");
    assert!(
        output.contains("--manifest-path tools/repo-checks/Cargo.toml")
            && output.contains("--test ownership"),
        "tooling pattern requires its own manifest: {output}"
    );
    write_file(
        &temp_root
            .path()
            .join("tools/repo-checks/tests/ownership.rs"),
        "#[test]\nfn test_tooling_ownership_changed() {}\n",
    );
    let (code, output) = run_dev_loop_changed(temp_root.path(), &["--dry-run", "--changed"]);
    assert_eq!(code, 0, "tooling source delta must resolve: {output}");
    assert!(
        output.contains("--manifest-path tools/repo-checks/Cargo.toml")
            && output.contains("--tests"),
        "tooling source delta validates its owning package: {output}"
    );
    write_file(&temp_root.path().join("build.rs"), "fn main() {}\n");
    let (code, output) = run_dev_loop_changed(temp_root.path(), &["--dry-run", "--changed"]);
    assert_eq!(code, 0, "build input must resolve: {output}");
    assert!(
        output.contains("--lib"),
        "build.rs affects the runtime compilation: {output}"
    );
}

fn seed_core_ownership(temp_root: &Path) {
    write_file(
        &temp_root.join("scripts/ci-cohort.py"),
        &fs::read_to_string(repo_root().join("scripts/ci-cohort.py"))
            .expect("read shared owner map"),
    );
    write_file(
        &temp_root.join("formal/source-linked/lean/verify.py"),
        "SOURCES = (\"src/protocol_core.rs\",)\n",
    );
    write_file(
        &temp_root.join("formal/source-linked/lean/src/lib.rs"),
        "#[path = \"../../../../src/protocol_core.rs\"]\nmod protocol_core;\n",
    );
    write_file(
        &temp_root.join("src/protocol_core.rs"),
        "#[cfg(test)]\nmod tests { #[test] fn test_core_projection_runs() {} }\n",
    );
    write_file(
        &temp_root.join("tools/repo-checks/Cargo.toml"),
        "[package]\nname = \"repo-checks\"\n[[test]]\nname = \"async_timeout_policy_scan\"\n[[test]]\nname = \"loud_test_failures_scan\"\n",
    );
}

#[test]
fn test_dev_loop_routes_core_patterns_and_changes_without_root_builds() {
    let temp_root = unique_temp_dir("dev-loop-core-owner");
    copy_dev_loop_script(temp_root.path());
    seed_fixture_repo(temp_root.path());
    seed_core_ownership(temp_root.path());
    commit_fixture_repo(temp_root.path());
    let (code, output) = run_dev_loop_changed(
        temp_root.path(),
        &[
            "--dry-run",
            "--features",
            "tls",
            "test_core_projection_runs",
        ],
    );
    assert_eq!(code, 0, "production-core pattern must resolve: {output}");
    assert!(
        output.contains("--manifest-path formal/source-linked/lean/Cargo.toml")
            && output.contains("--lib"),
        "use existing production-core crate: {output}"
    );
    assert!(
        !output.contains("--features"),
        "server-only features cannot leak into proof core: {output}"
    );
    write_file(
        &temp_root.path().join("src/protocol_core.rs"),
        "#[cfg(test)]\nmod tests { #[test] fn test_core_projection_changed() {} }\n",
    );
    let (code, output) = run_dev_loop_changed(temp_root.path(), &["--dry-run", "--changed"]);
    assert_eq!(code, 0, "core source delta must resolve: {output}");
    let commands: Vec<_> = output
        .lines()
        .filter(|line| line.starts_with("DRY-RUN:"))
        .collect();
    assert_eq!(
        commands.len(),
        2,
        "core plus scanner package only: {output}"
    );
    assert!(
        commands.iter().all(|line| line.contains("--manifest-path")),
        "core delta must not compile root tests: {output}"
    );
    assert!(
        output.contains("--test async_timeout_policy_scan")
            && output.contains("--test loud_test_failures_scan"),
        "source-policy coverage must remain: {output}"
    );
}

#[test]
fn test_dev_loop_core_and_server_patterns_keep_separate_builds() {
    let temp_root = unique_temp_dir("dev-loop-mixed-core-owner");
    copy_dev_loop_script(temp_root.path());
    seed_fixture_repo(temp_root.path());
    seed_core_ownership(temp_root.path());
    let (code, output) = run_dev_loop_changed(
        temp_root.path(),
        &["--dry-run", "test_core_projection_runs", "test_alpha_unit"],
    );
    assert_eq!(code, 0, "mixed source patterns resolve: {output}");
    assert_eq!(
        output
            .lines()
            .filter(|line| line.contains("cargo nextest run"))
            .count(),
        2,
        "each package owns its build: {output}"
    );
    assert!(
        output.contains("DRY-RUN: cargo nextest run --lib --no-tests fail"),
        "server source still gets real root tests: {output}"
    );
    assert!(
        output.contains("--manifest-path formal/source-linked/lean/Cargo.toml"),
        "core source keeps its fast owner: {output}"
    );
}

#[test]
fn test_dev_loop_directory_targets_own_entrypoints_and_modules() {
    let temp_root = unique_temp_dir("dev-loop-directory-owner");
    copy_dev_loop_script(temp_root.path());
    seed_fixture_repo(temp_root.path());
    write_file(&temp_root.path().join("Cargo.toml"), "[package]\nname = \"fixture\"\nautotests = false\n[[test]]\nname = \"capacity_runner\"\npath = \"tests/capacity/main.rs\"\n");
    write_file(
        &temp_root.path().join("tests/capacity/main.rs"),
        "#[test]\nfn test_capacity_entry() {}\nmod helpers;\n",
    );
    write_file(
        &temp_root.path().join("tests/capacity/helpers.rs"),
        "pub fn helper() {}\n",
    );
    commit_fixture_repo(temp_root.path());
    for file in ["main.rs", "helpers.rs"] {
        let path = temp_root.path().join("tests/capacity").join(file);
        let original = fs::read_to_string(&path).expect("read directory fixture");
        write_file(&path, &format!("{original}\n// changed\n"));
        let (code, output) = run_dev_loop_changed(temp_root.path(), &["--dry-run", "--changed"]);
        assert_eq!(code, 0, "directory source must resolve: {output}");
        assert!(
            output.contains("--test capacity_runner"),
            "registered name owns both entrypoint and module: {output}"
        );
        write_file(&path, &original);
    }
    let (code, output) =
        run_dev_loop_changed(temp_root.path(), &["--dry-run", "test_capacity_entry"]);
    assert_eq!(code, 0, "directory target pattern resolves: {output}");
    assert!(
        output.contains("--test capacity_runner"),
        "directory pattern must preserve registered name: {output}"
    );
}

#[cfg(unix)]
#[test]
fn test_dev_loop_metadata_is_fresh_and_failure_cannot_reuse_previous_build() {
    use std::os::unix::fs::PermissionsExt;
    let temp_root = unique_temp_dir("dev-loop-native-metadata");
    copy_dev_loop_script(temp_root.path());
    seed_fixture_repo(temp_root.path());
    let bin = temp_root.path().join("bin");
    let cargo = bin.join("cargo");
    write_file(
        &cargo,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$DEV_LOOP_CARGO_LOG"
case "$1 $2" in
    'metadata --format-version')
        if [ -f "$DEV_LOOP_FAIL_METADATA" ]; then exit 7; fi
        printf '{}\n' ;;
    'nextest list')
        printf '{"rust-suites":{"fixture":{"testcases":{"test_alpha_runs":{"filter-match":{"status":"matches"},"ignored":false}}}}}\n' ;;
    'nextest run') exit 0 ;;
    *) exit 8 ;;
esac
"#,
    );
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755))
        .expect("make cargo fixture executable");
    let log = temp_root.path().join("commands");
    let fail_metadata = temp_root.path().join("fail-metadata");
    let scratch = temp_root.path().join("scratch");
    fs::create_dir(&scratch).expect("metadata scratch directory");
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").expect("PATH"),
    ));
    let fixture_path = std::env::join_paths(paths).expect("fixture PATH");
    let run = || {
        bash_command()
            .arg("scripts/dev-loop.sh")
            .arg("test_alpha_runs")
            .env("PATH", &fixture_path)
            .env("DEV_LOOP_CARGO_LOG", &log)
            .env("DEV_LOOP_FAIL_METADATA", &fail_metadata)
            .env("TMPDIR", &scratch)
            .current_dir(temp_root.path())
            .output()
            .expect("run metadata fixture")
    };
    let output = run();
    assert!(
        output.status.success(),
        "matching cached discovery must execute tests: {output:?}"
    );
    assert_eq!(
        fs::read_dir(&scratch).expect("scratch").count(),
        0,
        "metadata must be removed after success"
    );
    write_file(
        &temp_root.path().join("tests/foo.rs"),
        "#[test]\nfn test_alpha_runs() {}\n// edited after first build\n",
    );
    write_file(&fail_metadata, "fail");
    let output = run();
    assert!(
        !output.status.success(),
        "new metadata failure cannot reuse old evidence: {output:?}"
    );
    let commands = fs::read_to_string(&log).expect("captured commands");
    assert_eq!(
        commands
            .lines()
            .filter(|line| line.contains("--list-type binaries-only"))
            .count(),
        2,
        "every invocation must build current sources: {commands}"
    );
    let runs: Vec<_> = commands
        .lines()
        .filter(|line| line.starts_with("nextest run "))
        .collect();
    assert_eq!(
        runs.len(),
        1,
        "failed metadata must prevent the second execution: {commands}"
    );
    assert!(
        runs[0].contains("--binaries-metadata")
            && runs[0].contains("--cargo-metadata")
            && !runs[0].contains("--test"),
        "native reuse must omit conflicting Cargo selectors: {commands}"
    );
    assert_eq!(
        fs::read_dir(&scratch).expect("scratch").count(),
        0,
        "metadata must be removed after failure"
    );
}

#[test]
fn test_dev_loop_resolves_checkout_from_nested_working_directory() {
    let temp_root = unique_temp_dir("dev-loop-nested-working-directory");
    copy_dev_loop_script(temp_root.path());
    seed_fixture_repo(temp_root.path());
    commit_fixture_repo(temp_root.path());
    let nested = temp_root.path().join("nested/deeper");
    fs::create_dir_all(&nested).expect("create nested working directory");
    let run = |args: &[&str]| {
        let output = bash_command()
            .arg("../../scripts/dev-loop.sh")
            .args(args)
            .current_dir(&nested)
            .output()
            .expect("invoke from nested working directory");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success(),
            "nested invocation must succeed: {text}"
        );
        text
    };
    assert!(
        run(&["--dry-run", "test_alpha_runs"]).contains("--test foo"),
        "named selection must resolve at checkout root"
    );
    assert!(
        run(&["--help"]).contains("Usage:"),
        "help must read the absolute script path after changing directory"
    );
    write_file(
        &temp_root.path().join("src/lib.rs"),
        "#[cfg(test)] mod tests { #[test] fn changed() {} }\n",
    );
    let changed = run(&["--dry-run", "--changed"]);
    assert!(
        changed.contains("--lib"),
        "nested invocation must discover root source changes: {changed}"
    );
    assert!(
        !changed.contains("nothing to run"),
        "nested Git pathspec must not falsely report a clean tree: {changed}"
    );
}
