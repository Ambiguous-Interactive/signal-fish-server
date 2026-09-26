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
        output.contains("--no-tests warn"),
        "a scoped owner without matching tests must fail open (warn), not red: {output}"
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
