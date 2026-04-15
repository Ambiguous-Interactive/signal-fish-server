#![cfg(test)]

mod common;

use common::{bash_command, read_file, repo_root};

#[test]
fn test_repository_passes_doc_consistency_script() {
    let root = repo_root();
    let output = bash_command()
        .arg("scripts/check-doc-consistency.sh")
        .current_dir(&root)
        .output()
        .unwrap_or_else(|e| panic!("Failed to run doc consistency script: {e}"));

    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));

    assert!(
        output.status.success(),
        "Repository must satisfy scripts/check-doc-consistency.sh policy checks.\nOutput:\n{combined}",
    );
}

#[test]
fn test_pre_commit_hook_includes_doc_consistency_check() {
    let root = repo_root();
    let hook = read_file(&root.join(".githooks/pre-commit"));

    assert!(
        hook.contains("# Check 21: Documentation + changelog consistency"),
        ".githooks/pre-commit must include Check 21 heading for doc/changelog consistency policy."
    );
    assert!(
        hook.contains("scripts/check-doc-consistency.sh --staged"),
        "Check 21 must run scripts/check-doc-consistency.sh in --staged mode."
    );
}

#[test]
fn test_run_local_ci_includes_doc_consistency_check() {
    let root = repo_root();
    let script = read_file(&root.join("scripts/run-local-ci.sh"));

    assert!(
        script.contains("check-doc-consistency.sh"),
        "scripts/run-local-ci.sh must execute scripts/check-doc-consistency.sh as a local CI gate."
    );
}

#[test]
fn test_ci_workflow_runs_doc_consistency_check_with_changed_files() {
    let root = repo_root();
    let workflow = read_file(&root.join(".github/workflows/ci.yml"));

    assert!(
        workflow.contains("Doc Consistency") || workflow.contains("doc-consistency"),
        "ci.yml must define a doc consistency job or step."
    );
    assert!(
        workflow.lines().any(|line| {
            line.contains("check-doc-consistency.sh") && line.contains("--changed-files")
        }),
        "ci.yml must have a line that invokes check-doc-consistency.sh with --changed-files for PR/push diff-aware changelog gating."
    );
}

fn extract_ci_dep_detect_step_block(workflow: &str) -> String {
    let step_start = workflow
        .find("      - name: Detect dependency-only changes")
        .expect("doc-consistency dep-detect step header not found in .github/workflows/ci.yml");
    let after_start = &workflow[step_start..];
    let step_end = after_start
        .find("\n      - name: Run documentation/changelog consistency checks")
        .expect("dep-detect step terminator not found in .github/workflows/ci.yml");
    after_start[..step_end].to_string()
}

#[test]
fn test_ci_workflow_has_file_based_actor_agnostic_dep_detect_step() {
    let root = repo_root();
    let workflow = read_file(&root.join(".github/workflows/ci.yml"));
    let dep_detect_step = extract_ci_dep_detect_step_block(&workflow);

    assert!(
        dep_detect_step.contains("Detect dependency-only changes"),
        "ci.yml must contain a 'Detect dependency-only changes' step."
    );
    assert!(
        dep_detect_step.contains("id: dep-detect"),
        "ci.yml dependency detection step must have id 'dep-detect'."
    );
    assert!(
        dep_detect_step.contains("skip_changelog"),
        "ci.yml dep-detect step must set a skip_changelog output."
    );
    assert!(
        dep_detect_step.contains("HAS_CARGO_CHANGE=\"false\"")
            && dep_detect_step.contains("Cargo.toml|Cargo.lock) HAS_CARGO_CHANGE=\"true\" ;;")
            && dep_detect_step
                .contains("if [ \"$NON_INTERNAL\" = \"false\" ] && [ \"$HAS_CARGO_CHANGE\" = \"true\" ]; then"),
        "ci.yml dep-detect step must use file-based dependency-only detection for Cargo manifest/lock changes."
    );

    assert!(
        workflow.contains("--skip-changelog-gate"),
        "ci.yml must pass --skip-changelog-gate to the checker when dep-detect triggers."
    );

    assert!(
        !dep_detect_step.contains("${{ github.actor }}")
            && !dep_detect_step.contains("dependabot[bot]"),
        "ci.yml dep-detect step must be actor-agnostic."
    );
    assert!(
        !dep_detect_step.contains("COMMIT_MSG=")
            && !dep_detect_step.contains("git log -1 --format")
            && !dep_detect_step.contains("grep -qiE"),
        "ci.yml dep-detect step must not rely on commit-message pattern matching."
    );
}

#[test]
fn test_ci_dep_detect_internal_paths_match_script() {
    let root = repo_root();
    let workflow = read_file(&root.join(".github/workflows/ci.yml"));
    let script = read_file(&root.join("scripts/check-doc-consistency.sh"));

    // Both the CI dep-detect case statement and the script's is_internal_path()
    // must classify the same directory prefixes as internal. Extract the
    // directory-glob patterns from each and verify they match.
    //
    // The script uses patterns like: .github/*|.githooks/*|...
    // The CI uses patterns like:     .github/*|.githooks/*|...
    // We verify the shared directory prefixes are present in both.
    let shared_directory_prefixes = [
        ".github/*",
        ".githooks/*",
        ".devcontainer/*",
        ".config/*",
        ".vscode/*",
        ".claude/*",
        "scripts/*",
        "tests/*",
        "test-fixtures/*",
        ".llm/*",
        "target/*",
        "progress/*",
    ];

    for prefix in shared_directory_prefixes {
        assert!(
            script.contains(prefix),
            "scripts/check-doc-consistency.sh is_internal_path() must contain '{prefix}'."
        );
        assert!(
            workflow.contains(prefix),
            "ci.yml dep-detect case statement must contain '{prefix}' to stay in sync with the script."
        );
    }

    // Standalone internal files that both should recognize.
    let shared_standalone_files = [
        "Cargo.lock",
        "PLAN.md",
        "AGENTS.md",
        "pre-push.txt",
        ".gitignore",
        ".dockerignore",
        "clippy.toml",
        "deny.toml",
        "tarpaulin.toml",
        "rust-toolchain.toml",
        "mkdocs.yml",
        "requirements-docs.txt",
    ];

    for file in shared_standalone_files {
        assert!(
            script.contains(file),
            "scripts/check-doc-consistency.sh is_internal_path() must list '{file}'."
        );
        assert!(
            workflow.contains(file),
            "ci.yml dep-detect case statement must list '{file}' to stay in sync with the script."
        );
    }
}

#[test]
fn test_pre_push_hook_includes_doc_consistency_gate_and_tests() {
    let root = repo_root();
    let hook = read_file(&root.join(".githooks/pre-push"));

    assert!(
        hook.contains("scripts/check-doc-consistency.sh --changed-files"),
        ".githooks/pre-push must run scripts/check-doc-consistency.sh in --changed-files mode for relevant push diffs."
    );
    assert!(
        hook.contains("cargo test --locked --test doc_consistency_policy_tests --test doc_consistency_script_tests"),
        ".githooks/pre-push must run doc consistency policy tests when docs/changelog policy files change."
    );
}

#[derive(Debug)]
struct ProtocolReferenceCase {
    file: &'static str,
    required_references: &'static [&'static str],
}

#[derive(Debug)]
struct ProtocolSampleCase {
    file: &'static str,
    required_tokens: &'static [&'static str],
    forbidden_tokens: &'static [&'static str],
}

#[test]
fn test_protocol_docs_reference_canonical_samples_data_driven() {
    let root = repo_root();
    let cases = [
        ProtocolReferenceCase {
            file: ".llm/context.md",
            required_references: &[
                "code-samples/protocol/v2-client-messages.jsonl",
                "code-samples/protocol/v2-server-messages.jsonl",
            ],
        },
        ProtocolReferenceCase {
            file: "README.md",
            required_references: &[
                ".llm/code-samples/protocol/v2-client-messages.jsonl",
                ".llm/code-samples/protocol/v2-server-messages.jsonl",
            ],
        },
    ];

    for case in cases {
        let content = read_file(&root.join(case.file));
        for required_reference in case.required_references {
            assert!(
                content.contains(required_reference),
                "{} must reference canonical protocol sample {}",
                case.file,
                required_reference
            );
        }
    }
}

#[test]
fn test_protocol_sample_files_are_present_and_valid_data_driven() {
    let root = repo_root();
    let cases = [
        ProtocolSampleCase {
            file: ".llm/code-samples/protocol/v2-client-messages.jsonl",
            required_tokens: &["\"Authenticate\"", "\"JoinRoom\""],
            forbidden_tokens: &["server_version", "CreateRoom", "SetReady"],
        },
        ProtocolSampleCase {
            file: ".llm/code-samples/protocol/v2-server-messages.jsonl",
            required_tokens: &["\"app_name\"", "\"rate_limits\"", "\"ProtocolInfo\""],
            forbidden_tokens: &["server_version", "RoomCreated", "AuthorityGranted"],
        },
    ];

    for case in cases {
        let path = root.join(case.file);
        assert!(
            path.exists(),
            "Protocol sample file is missing: {}",
            case.file
        );

        let content = read_file(&path);
        for token in case.required_tokens {
            assert!(
                content.contains(token),
                "Protocol sample file {} must include token {}",
                case.file,
                token
            );
        }

        for token in case.forbidden_tokens {
            assert!(
                !content.contains(token),
                "Protocol sample file {} contains stale token {}",
                case.file,
                token
            );
        }

        let non_empty_line_count = content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count();
        assert!(
            non_empty_line_count > 0,
            "Protocol sample file {} must contain at least one non-empty JSON line",
            case.file,
        );
    }
}
