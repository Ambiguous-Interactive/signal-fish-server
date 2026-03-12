#![cfg(test)]

mod common;

use common::{bash_command, repo_root, unique_temp_dir, write_file};
use std::fs;

fn run_hygiene_with_fixture(
    workflow_name: &str,
    workflow_content: &str,
    extra_files: &[(&str, &str)],
) -> (bool, String) {
    let temp_root = unique_temp_dir("workflow-hygiene");
    let script_src = repo_root().join("scripts/check-workflow-hygiene.sh");
    let script_dst = temp_root.path().join("scripts/check-workflow-hygiene.sh");
    let workflow_path = temp_root
        .path()
        .join(format!(".github/workflows/{workflow_name}"));

    let script = fs::read_to_string(&script_src)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", script_src.display()));

    write_file(&script_dst, &script);
    write_file(&workflow_path, workflow_content);
    for (relative_path, content) in extra_files {
        write_file(&temp_root.path().join(relative_path), content);
    }

    let output = bash_command()
        .arg("scripts/check-workflow-hygiene.sh")
        .current_dir(temp_root.path())
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "Failed to run workflow hygiene script in {}: {e}",
                temp_root.path().display()
            )
        });

    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));

    (output.status.success(), combined)
}

fn run_hygiene_with_workflow(workflow_name: &str, workflow_content: &str) -> (bool, String) {
    run_hygiene_with_fixture(workflow_name, workflow_content, &[])
}

#[test]
fn test_workflow_hygiene_detects_missing_locked_in_multiline_cargo_command() {
    let workflow = r#"name: Test Workflow
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - name: Run tests
        run: |
          cargo test \
            --all-features \
            --no-fail-fast
"#;

    let (success, output) = run_hygiene_with_workflow("missing-locked.yml", workflow);

    assert!(
        success,
        "Workflow hygiene script should exit success for warning-only cases.\nOutput:\n{output}"
    );
    assert!(
        output.contains("missing --locked flag"),
        "Expected missing --locked warning for multiline cargo command.\nOutput:\n{output}"
    );
}

#[test]
fn test_workflow_hygiene_accepts_locked_flag_on_later_multiline_line() {
    let workflow = r#"name: Test Workflow
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - name: Run tests
        run: |
          cargo test \
            --all-features \
            --locked \
            --no-fail-fast
"#;

    let (success, output) = run_hygiene_with_workflow("locked-on-next-line.yml", workflow);

    assert!(
        success,
        "Workflow hygiene script should succeed when no errors are found.\nOutput:\n{output}"
    );
    assert!(
        !output.contains("missing --locked flag"),
        "Did not expect missing --locked warning when --locked is present on a continued line.\nOutput:\n{output}"
    );
}

#[test]
fn test_workflow_hygiene_ignores_cargo_strings_outside_run_blocks() {
    let workflow = r#"name: Cache cargo registry and build
on: [push]
jobs:
  cache:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - name: Cache cargo registry
        uses: actions/cache@v4
        with:
          path: ~/.cargo/registry
          key: test-cargo-cache
"#;

    let (success, output) = run_hygiene_with_workflow("non-run-cargo.yml", workflow);

    assert!(
        success,
        "Workflow hygiene script should not fail on metadata-only cargo strings.\nOutput:\n{output}"
    );
    assert!(
        !output.contains("missing --locked flag"),
        "Metadata-only cargo strings should not be interpreted as cargo commands.\nOutput:\n{output}"
    );
}

#[test]
fn test_workflow_hygiene_flags_multiline_cargo_sbom_locked_as_error() {
    let workflow = r#"name: SBOM Workflow
on: [push]
jobs:
  sbom:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - name: Generate SBOM
        run: |
          cargo sbom \
            --locked \
            --output-format cyclone_dx_json_1_5 > sbom.cdx.json
"#;

    let (success, output) = run_hygiene_with_workflow("sbom-locked.yml", workflow);

    assert!(
        !success,
        "Workflow hygiene script must fail when cargo sbom uses --locked.\nOutput:\n{output}"
    );
    assert!(
        output.contains("cargo sbom does not support --locked"),
        "Expected cargo sbom incompatibility error.\nOutput:\n{output}"
    );
}

#[test]
fn test_workflow_hygiene_fails_on_npx_usage_in_automation_scripts() {
    let workflow = r#"name: Minimal Workflow
on: [push]
jobs:
  check:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - run: echo "ok"
"#;

    let (success, output) = run_hygiene_with_fixture(
        "npx-policy.yml",
        workflow,
        &[(
            "scripts/check-docs.sh",
            "#!/usr/bin/env bash\nnpx --yes markdownlint-cli2 '**/*.md'\n",
        )],
    );

    assert!(
        !success,
        "Workflow hygiene script must fail when automation scripts use npx.\nOutput:\n{output}"
    );
    assert!(
        output.contains("Uses npx invocation in automation"),
        "Expected npx policy violation message.\nOutput:\n{output}"
    );
}

#[test]
fn test_workflow_hygiene_fails_on_external_latest_image_usage() {
    let workflow = r#"name: Minimal Workflow
on: [push]
jobs:
  check:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - run: echo "ok"
"#;

    let (success, output) = run_hygiene_with_fixture(
        "external-latest.yml",
        workflow,
        &[(
            "scripts/check-docs.sh",
            "#!/usr/bin/env bash\ndocker run davidanson/markdownlint-cli2:latest '**/*.md'\n",
        )],
    );

    assert!(
        !success,
        "Workflow hygiene script must fail when automation scripts use external ':latest' image tags.\nOutput:\n{output}"
    );
    assert!(
        output.contains("Uses mutable Docker tag ':latest'"),
        "Expected Docker latest-tag policy violation message.\nOutput:\n{output}"
    );
}

#[test]
fn test_workflow_hygiene_allows_first_party_latest_image_usage() {
    let workflow = r#"name: Minimal Workflow
on: [push]
jobs:
  check:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - run: echo "ok"
"#;

    let (success, output) = run_hygiene_with_fixture(
        "first-party-latest.yml",
        workflow,
        &[(
            "scripts/check-docs.sh",
            "#!/usr/bin/env bash\ndocker run ghcr.io/ambiguous-interactive/signal-fish-server:latest --help\n",
        )],
    );

    assert!(
        success,
        "Workflow hygiene script should allow first-party image ':latest' tags.\nOutput:\n{output}"
    );
    assert!(
        !output.contains("Uses mutable Docker tag ':latest'"),
        "Did not expect external-latest policy error for first-party image usage.\nOutput:\n{output}"
    );
}

#[test]
fn test_workflow_hygiene_fails_on_malformed_remote_action_reference() {
    let workflow = r#"name: Malformed Uses
on: [push]
jobs:
  lint:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - uses: actions/checkout
      - run: echo ok
"#;

    let (success, output) = run_hygiene_with_workflow("malformed-uses.yml", workflow);

    assert!(
        !success,
        "Workflow hygiene script must fail on malformed remote action refs.\nOutput:\n{output}"
    );
    assert!(
        output.contains("Malformed remote action reference"),
        "Expected malformed action reference violation message.\nOutput:\n{output}"
    );
    assert!(
        output.contains("owner/repo@ref"),
        "Expected remediation guidance to include owner/repo@ref syntax.\nOutput:\n{output}"
    );
}

#[test]
fn test_workflow_hygiene_fails_on_commit_hash_action_reference() {
    let workflow = r#"name: Commit Hash Uses
on: [push]
jobs:
  lint:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd
      - run: echo ok
"#;

    let (success, output) = run_hygiene_with_workflow("commit-hash-uses.yml", workflow);

    assert!(
        !success,
        "Workflow hygiene script must fail on commit-hash action refs.\nOutput:\n{output}"
    );
    assert!(
        output.contains("Action uses commit hash ref (disallowed)"),
        "Expected commit-hash policy violation message.\nOutput:\n{output}"
    );
}

#[test]
fn test_workflow_hygiene_accepts_explicit_version_action_references() {
    let workflow = r#"name: Versioned Uses
on: [push]
jobs:
  lint:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - uses: actions/checkout@v6.0.2
      - uses: docker/build-push-action@v6.19.2
      - run: echo ok
"#;

    let (success, output) = run_hygiene_with_workflow("versioned-uses.yml", workflow);

    assert!(
        success,
        "Workflow hygiene script should succeed when remote actions use explicit version tags.\nOutput:\n{output}"
    );
    assert!(
        !output.contains("Action uses commit hash ref (disallowed)"),
        "Did not expect commit-hash policy violations for explicit version tags.\nOutput:\n{output}"
    );
    assert!(
        !output.contains("Action uses invalid ref format"),
        "Did not expect invalid-ref errors for explicit version tags.\nOutput:\n{output}"
    );
}
