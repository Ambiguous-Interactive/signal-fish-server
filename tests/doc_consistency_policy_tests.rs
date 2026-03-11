#![cfg(test)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_file(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {}", path.display(), e))
}

fn bash_command() -> Command {
    #[cfg(target_os = "windows")]
    {
        let candidates = [
            Path::new("C:\\Program Files\\Git\\bin\\bash.exe"),
            Path::new("C:\\Program Files (x86)\\Git\\bin\\bash.exe"),
        ];
        for path in &candidates {
            if path.exists() {
                return Command::new(path);
            }
        }
        panic!(
            "Git Bash not found at any known location ({candidates:?}). \
             Cannot run bash scripts on Windows without Git Bash."
        );
    }
    #[cfg(not(target_os = "windows"))]
    {
        Command::new("bash")
    }
}

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
        workflow.contains("check-doc-consistency.sh") && workflow.contains("--changed-files"),
        "ci.yml must invoke scripts/check-doc-consistency.sh with --changed-files for PR/push diff-aware changelog gating."
    );
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
