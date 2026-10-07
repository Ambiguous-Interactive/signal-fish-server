#![cfg(test)]

mod common;

use common::{
    bash_command, read_file, read_live_file, repo_root, strip_comment_lines, unique_temp_dir,
    write_file,
};

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
fn test_run_local_ci_includes_doc_consistency_check() {
    let root = repo_root();
    let local_ci = read_live_file(&root.join("scripts/run-local-ci.sh"));

    assert!(
        local_ci.contains("doc-consistency"),
        "scripts/run-local-ci.sh must include a docs/changelog consistency check."
    );
    assert!(
        local_ci.contains("scripts/check-doc-consistency.sh"),
        "docs/changelog consistency must run outside sub-second git hooks."
    );
}

#[test]
fn test_ci_workflow_runs_doc_consistency_check_with_changed_files() {
    let root = repo_root();
    let workflow = read_live_file(&root.join(".github/workflows/ci.yml"));

    assert!(
        workflow.contains("Doc Consistency") || workflow.contains("doc-consistency"),
        "ci.yml must define a doc consistency job or step."
    );
    let invocation_line = workflow
        .lines()
        .find(|line| line.contains("check-doc-consistency.sh") && line.contains("--changed-files"));
    let invocation_line = invocation_line.expect(
        "ci.yml must have a line that invokes check-doc-consistency.sh with --changed-files for PR/push diff-aware changelog gating.",
    );
    assert!(
        invocation_line.contains("--diff-base"),
        "ci.yml must pass --diff-base so the changelog gate can exempt test-module-only Rust diffs in hosted CI (issue #722); \
         without it the hook passes locally while hosted CI demands an entry.\nline: {invocation_line}"
    );
    assert!(
        workflow.contains("diff_base="),
        "ci.yml must publish the diff_base output the doc-consistency invocation consumes."
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
    let workflow_live = strip_comment_lines(&workflow);
    let dep_detect_step = extract_ci_dep_detect_step_block(&workflow);
    let dep_detect_step_live = strip_comment_lines(&dep_detect_step);

    assert!(
        dep_detect_step_live.contains("Detect dependency-only changes"),
        "ci.yml must contain a 'Detect dependency-only changes' step."
    );
    assert!(
        dep_detect_step_live.contains("id: dep-detect"),
        "ci.yml dependency detection step must have id 'dep-detect'."
    );
    assert!(
        dep_detect_step_live.contains("skip_changelog"),
        "ci.yml dep-detect step must set a skip_changelog output."
    );
    assert!(
        dep_detect_step_live.contains("HAS_DEPENDENCY_CHANGE=\"false\"")
            && dep_detect_step_live.contains(
                "Cargo.toml|Cargo.lock|clients/native/Cargo.toml|clients/native/Cargo.lock) HAS_DEPENDENCY_CHANGE=\"true\" ;;"
            )
            && dep_detect_step_live
                .contains("package.json|package-lock.json|clients/browser/package.json|clients/browser/package-lock.json) HAS_DEPENDENCY_CHANGE=\"true\" ;;")
            && dep_detect_step_live
                .contains("if [ \"$NON_INTERNAL\" = \"false\" ] && [ \"$HAS_DEPENDENCY_CHANGE\" = \"true\" ]; then"),
        "ci.yml dep-detect step must use file-based dependency-only detection for every tracked Cargo and npm package graph."
    );

    assert!(
        workflow_live.contains("--skip-changelog-gate"),
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

fn dependency_detection_script(step: &str) -> String {
    step.split_once("        run: |\n")
        .expect("dep-detect step must have a shell run block")
        .1
        .lines()
        .map(|line| line.strip_prefix("          ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn test_ci_dependency_detection_classifies_cargo_npm_and_mixed_changes() {
    let root = repo_root();
    let workflow = read_file(&root.join(".github/workflows/ci.yml"));
    let script = dependency_detection_script(&extract_ci_dep_detect_step_block(&workflow));
    let cases = [
        ("cargo-root", "Cargo.toml\nCargo.lock\n", true),
        ("npm-root", "package.json\npackage-lock.json\n", true),
        (
            "npm-browser",
            "clients/browser/package.json\nclients/browser/package-lock.json\n",
            true,
        ),
        (
            "npm-plus-internal",
            "clients/browser/package-lock.json\ntests/ci_config_tests.rs\n",
            true,
        ),
        (
            "npm-plus-production",
            "clients/browser/package-lock.json\nsrc/server.rs\n",
            false,
        ),
        ("browser-tests", "Cargo.lock\nclients/browser/tests/deep/control.js\n", true),
        ("native-tests", "Cargo.lock\nclients/native/tests/interop.rs\n", true),
        ("fortress-tests", "Cargo.lock\nclients/fortress/tests/multiprocess.rs\n", true),
        ("wasm-tests", "Cargo.lock\nclients/fortress-wasm/tests/deep/control.rs\n", true),
        ("fortress-fixture", "Cargo.lock\nclients/fortress/src/main.rs\n", true),
        ("wasm-fixture", "Cargo.lock\nclients/fortress-wasm/src/nested/peer.rs\n", true),
        ("fixtures-only", "clients/fortress/src/main.rs\nclients/fortress-wasm/src/lib.rs\n", false),
        ("fixture-and-server", "Cargo.lock\nclients/fortress/src/main.rs\nsrc/server.rs\n", false),
        ("fixture-and-browser", "Cargo.lock\nclients/fortress-wasm/src/lib.rs\nclients/browser/src/client.js\n", false),
        ("fixture-and-native", "Cargo.lock\nclients/fortress/src/main.rs\nclients/native/src/client.rs\n", false),
        ("near-prefix-fixture", "Cargo.lock\nclients/fortress-other/src/main.rs\n", false),
        ("near-prefix-src", "Cargo.lock\nclients/fortress/src_like/main.rs\n", false),
        ("fixture-uppercase-src", "Cargo.lock\nclients/fortress/Src/main.rs\n", false),
        ("fixture-wasm-near-prefix", "Cargo.lock\nclients/fortress-wasm-extra/src/lib.rs\n", false),
        ("client-runtime", "Cargo.lock\nclients/native/tests/interop.rs\nclients/native/src/client.rs\n", false),
        ("nested-runtime-tests", "Cargo.lock\nclients/native/tests/interop.rs\nclients/native/src/tests/runtime.rs\n", false),
        ("deep-runtime-tests", "Cargo.lock\nclients/native/tests/interop.rs\nclients/native/src/nested/tests/runtime.rs\n", false),
        ("tests-like-runtime", "Cargo.lock\nclients/native/tests/interop.rs\nclients/native/tests_like/runtime.rs\n", false),
        ("uppercase-tests", "Cargo.lock\nclients/native/tests/interop.rs\nclients/native/Tests/control.rs\n", false),
        ("internal-only", "tests/ci_config_tests.rs\n", false),
    ];

    for (name, changed_files, expected_skip) in cases {
        let fixture = unique_temp_dir(&format!("dep-detect-{name}"));
        let github_output = fixture.path().join("github-output.txt");
        write_file(&fixture.path().join("changed-files.txt"), changed_files);
        write_file(&github_output, "");
        let output = bash_command()
            .arg("-c")
            .arg(&script)
            .current_dir(fixture.path())
            .env("GITHUB_OUTPUT", &github_output)
            .output()
            .unwrap_or_else(|error| panic!("{name}: dep-detect failed to execute: {error}"));
        assert!(
            output.status.success(),
            "{name}: dep-detect failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let outputs = read_file(&github_output);
        assert_eq!(
            outputs.lines().any(|line| line == "skip_changelog=true"),
            expected_skip,
            "{name}: unexpected dependency-only verdict in {outputs:?}"
        );
    }
}

#[test]
fn test_ci_dep_detect_internal_paths_match_script() {
    let root = repo_root();
    let workflow = read_live_file(&root.join(".github/workflows/ci.yml"));
    let script = read_live_file(&root.join("scripts/check-doc-consistency.sh"));

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
        "clients/browser/tests/*",
        "clients/native/tests/*",
        "clients/fortress/tests/*",
        "clients/fortress-wasm/tests/*",
        "clients/fortress/src/*",
        "clients/fortress-wasm/src/*",
        "test-fixtures/*",
        ".llm/*",
        "target/*",
        "progress/*",
        "docs/development/*",
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
        "CLAUDE.md",
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
fn test_ci_dep_detect_allows_dependency_maintenance_touchpoints() {
    let root = repo_root();
    let workflow = read_live_file(&root.join(".github/workflows/ci.yml"));
    let dep_detect_step = extract_ci_dep_detect_step_block(&workflow);
    let dep_detect_step_live = strip_comment_lines(&dep_detect_step);

    let dependency_maintenance_paths = [
        "clients/native/Cargo.toml",
        "clients/native/Cargo.lock",
        "package.json",
        "package-lock.json",
        "clients/browser/package.json",
        "clients/browser/package-lock.json",
        "docs/quickstart.md",
        "Dockerfile",
        "README.md",
    ];

    for path in dependency_maintenance_paths {
        assert!(
            dep_detect_step_live.contains(path),
            "ci.yml dep-detect must treat '{path}' as a dependency-maintenance touchpoint so dependency-only updates can skip changelog gating."
        );
    }
}

#[test]
fn test_pre_commit_doc_version_sync_sites_match_checker() {
    let root = repo_root();
    let hook = read_live_file(&root.join("scripts/hooks/pre-commit.ps1"));
    let checker = read_live_file(&root.join("scripts/check-doc-consistency.sh"));

    // The pre-commit auto-repair and the CI checker are two implementations of
    // one policy: every doc that quotes the crate version must equal
    // Cargo.toml [package].version. If they target different sites they drift
    // (auto-fix one file, gate another), which is the exact fragility this
    // automation exists to prevent. Lock the shared sites and markers here so
    // the fixer can never cover less than the checker validates.
    let version_sync_sites = ["docs/library-usage.md", ".llm/context.md"];
    for site in version_sync_sites {
        assert!(
            hook.contains(site),
            "scripts/hooks/pre-commit.ps1 must auto-sync the crate version in '{site}'."
        );
        assert!(
            checker.contains(site),
            "scripts/check-doc-consistency.sh must validate the crate version in '{site}'."
        );
    }

    let shared_markers = ["signal-fish-server", "- **Version:**"];
    for marker in shared_markers {
        assert!(
            hook.contains(marker),
            "scripts/hooks/pre-commit.ps1 version sync must handle the '{marker}' marker."
        );
        assert!(
            checker.contains(marker),
            "scripts/check-doc-consistency.sh version check must handle the '{marker}' marker."
        );
    }

    // Assert the canonical site list is actually *declared* with both sites
    // (not merely mentioned in a comment), so the fixer cannot quietly cover
    // fewer files than the checker validates.
    assert!(
        hook.contains(
            "$script:DocVersionSyncFiles = @(\"docs/library-usage.md\", \".llm/context.md\")"
        ),
        "pre-commit.ps1 must declare $script:DocVersionSyncFiles with both canonical version-sync sites."
    );
    assert!(
        hook.contains("Repair-DocVersionsIfNeeded"),
        "pre-commit.ps1 must run Repair-DocVersionsIfNeeded so a Cargo.toml version bump auto-syncs docs on commit."
    );
}

fn extract_hook_changelog_internal_path_globs(hook: &str) -> Vec<String> {
    const DECLARATION: &str = "$script:ChangelogInternalPathGlobs = [string[]]@(";
    let start = hook
        .find(DECLARATION)
        .expect("scripts/hooks/pre-commit.ps1 must declare $script:ChangelogInternalPathGlobs");
    let body = &hook[start + DECLARATION.len()..];
    let end = body
        .find("\n)")
        .expect("$script:ChangelogInternalPathGlobs array must be closed by a ')' on its own line");

    // The array body alternates between quoted glob strings and comments or
    // whitespace, so the odd segments of a double-quote split are exactly the
    // declared globs.
    body[..end]
        .split('"')
        .enumerate()
        .filter(|(index, _)| index % 2 == 1)
        .map(|(_, glob)| glob.to_string())
        .collect()
}

fn extract_checker_internal_path_patterns(checker: &str) -> (Vec<String>, usize, usize) {
    const FUNCTION: &str = "is_internal_path() {";
    let start = checker
        .find(FUNCTION)
        .expect("scripts/check-doc-consistency.sh must define is_internal_path()");
    let body = &checker[start..];
    let end = body.find("\n}").expect("is_internal_path() must be closed");

    // A bash case-pattern line is one or more glob alternatives separated by
    // '|' and terminated by ')'. The ')' terminates the line and is not part
    // of the alternatives, so strip it before the character check. All other
    // lines in the function (local, case, return, ;;, esac) fail this shape.
    // The '*' catch-all (match-everything default) ends the internal list.
    let mut patterns = Vec::new();
    let mut consumed_groups = 0;
    for line in body[..end].lines() {
        let Some(alternatives) = line.trim().strip_suffix(')') else {
            continue;
        };
        let is_pattern_line = !alternatives.is_empty()
            && alternatives.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '*' | '.' | '/' | '|' | '-' | '_')
            });
        if !is_pattern_line {
            continue;
        }
        if alternatives == "*" {
            break;
        }
        consumed_groups += 1;
        patterns.extend(alternatives.split('|').map(str::to_string));
    }

    // Every pattern arm pairs with exactly one `return 0`. If the checker
    // gains a shape this parser cannot read (for example a trailing comment
    // after the ')'), the counts diverge and the parity test fails loudly
    // instead of comparing a silently shrunken pattern set.
    let return_zero_arms = body[..end]
        .lines()
        .filter(|line| line.trim() == "return 0")
        .count();
    (patterns, consumed_groups, return_zero_arms)
}

#[test]
fn test_pre_commit_changelog_gate_internal_paths_match_checker() {
    let root = repo_root();
    let hook = read_live_file(&root.join("scripts/hooks/pre-commit.ps1"));
    let checker = read_live_file(&root.join("scripts/check-doc-consistency.sh"));

    let mut hook_globs = extract_hook_changelog_internal_path_globs(&hook);
    let (raw_checker_patterns, consumed_groups, return_zero_arms) =
        extract_checker_internal_path_patterns(&checker);
    let mut checker_patterns = raw_checker_patterns;

    assert_eq!(
        consumed_groups, return_zero_arms,
        "every parsed is_internal_path() pattern arm must pair with its 'return 0'; \
         a mismatch means the checker has a pattern shape this parser cannot read \
         and the parity comparison below would silently shrink."
    );
    assert!(
        !checker_patterns.is_empty(),
        "is_internal_path() in scripts/check-doc-consistency.sh must list internal path patterns."
    );
    assert!(
        !hook_globs.is_empty(),
        "$script:ChangelogInternalPathGlobs in scripts/hooks/pre-commit.ps1 must list internal path globs."
    );

    hook_globs.sort();
    hook_globs.dedup();
    checker_patterns.sort();
    checker_patterns.dedup();

    let missing_from_hook: Vec<&String> = checker_patterns
        .iter()
        .filter(|pattern| !hook_globs.contains(pattern))
        .collect();
    let extra_in_hook: Vec<&String> = hook_globs
        .iter()
        .filter(|glob| !checker_patterns.contains(glob))
        .collect();

    assert!(
        missing_from_hook.is_empty() && extra_in_hook.is_empty(),
        "The pre-commit changelog gate and the checker's is_internal_path() must classify internal paths identically.\n\
         Missing from scripts/hooks/pre-commit.ps1 $script:ChangelogInternalPathGlobs:\n{}\n\
         Not present in is_internal_path() in scripts/check-doc-consistency.sh:\n{}",
        missing_from_hook
            .iter()
            .map(|pattern| format!("  - {pattern}"))
            .collect::<Vec<_>>()
            .join("\n"),
        extra_in_hook
            .iter()
            .map(|glob| format!("  - {glob}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

#[test]
fn test_pre_commit_runs_changelog_gate_on_every_commit_shape() {
    let root = repo_root();
    let hook = read_live_file(&root.join("scripts/hooks/pre-commit.ps1"));

    assert!(
        hook.contains(
            "Invoke-Check \"Changelog gate\" { Test-ChangelogGate -ChangedFiles (Get-ChangelogGateChangedFiles) }"
        ),
        "pre-commit.ps1 must run the changelog gate on the gate-scoped changed-file set."
    );

    // The staged gate must classify the same diff-filter set as the checker's
    // collect_changed_files(): deletions are excluded, so a staged
    // `git rm CHANGELOG.md` cannot pose as changelog accompaniment and a
    // staged deletion of a non-internal path cannot trip the gate.
    assert!(
        hook.contains(
            "\"diff\", \"--cached\", \"--name-only\", \"-z\", \"--diff-filter=ACMRTUXB\""
        ),
        "Get-ChangelogGateChangedFiles must mirror the checker's --diff-filter=ACMRTUXB set."
    );

    // The gate must run before the metadata-policy early exit so a src-only
    // commit (no .llm/README files staged) is still gated.
    let doc_sync = hook
        .find("Invoke-Check \"Doc version sync\"")
        .expect("pre-commit.ps1 must run the doc version sync check");
    let changelog_gate = hook
        .find("Invoke-Check \"Changelog gate\"")
        .expect("pre-commit.ps1 must run the changelog gate check");
    let metadata_early_exit = hook
        .find("if (-not $metadataPolicyTriggered)")
        .expect("pre-commit.ps1 must have a metadata-policy early exit");
    assert!(
        doc_sync < changelog_gate && changelog_gate < metadata_early_exit,
        "the changelog gate must run after doc version sync and before the metadata-policy early exit."
    );

    // -Worktree discovery must see CHANGELOG.md so the worktree preflight can
    // verify that non-internal changes carry a changelog entry.
    let pathspecs_start = hook
        .find("$script:WorktreePolicyPathspecs = @(")
        .expect("pre-commit.ps1 must declare $script:WorktreePolicyPathspecs");
    let pathspecs_block = &hook[pathspecs_start..];
    let pathspecs_end = pathspecs_block
        .find(") + $script:DocVersionSyncFiles")
        .expect("$script:WorktreePolicyPathspecs must append the policy file lists");
    assert!(
        pathspecs_block[..pathspecs_end].contains("\"CHANGELOG.md\""),
        "$script:WorktreePolicyPathspecs must include CHANGELOG.md so -Worktree mode can see changelog accompaniment."
    );
}

#[test]
fn test_local_ci_includes_doc_consistency_gate_and_tests() {
    let root = repo_root();
    let local_ci = read_live_file(&root.join("scripts/run-local-ci.sh"));

    assert!(
        local_ci.contains("doc-consistency")
            && local_ci.contains("scripts/check-doc-consistency.sh"),
        "scripts/run-local-ci.sh must run scripts/check-doc-consistency.sh before handoff/CI."
    );
    assert!(
        local_ci.contains("doc-policy-tests")
            && local_ci.contains("--test doc_consistency_policy_tests")
            && local_ci.contains("--test doc_consistency_script_tests"),
        "scripts/run-local-ci.sh must run doc consistency policy tests outside git hooks."
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
                "code-samples/protocol/v3-client-messages.jsonl",
                "code-samples/protocol/v3-server-messages.jsonl",
            ],
        },
        ProtocolReferenceCase {
            file: "docs/protocol.md",
            required_references: &[
                ".llm/code-samples/protocol/v2-client-messages.jsonl",
                ".llm/code-samples/protocol/v2-server-messages.jsonl",
                ".llm/code-samples/protocol/v3-client-messages.jsonl",
                ".llm/code-samples/protocol/v3-server-messages.jsonl",
            ],
        },
    ];

    let mut missing_references = Vec::new();
    for case in cases {
        let content = read_file(&root.join(case.file));
        for required_reference in case.required_references {
            if !content.contains(required_reference) {
                missing_references.push(format!(
                    "{} must reference canonical protocol sample {}",
                    case.file, required_reference
                ));
            }
        }
    }

    assert!(
        missing_references.is_empty(),
        "Protocol docs are missing canonical sample references:\n{}",
        missing_references.join("\n")
    );
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
        ProtocolSampleCase {
            file: ".llm/code-samples/protocol/v3-client-messages.jsonl",
            required_tokens: &["\"protocol_version\"", "\"Signal\"", "\"TransportStatus\""],
            forbidden_tokens: &["server_version", "CreateRoom", "SetReady"],
        },
        ProtocolSampleCase {
            file: ".llm/code-samples/protocol/v3-server-messages.jsonl",
            required_tokens: &["\"DeliveryReport\"", "\"SessionPlan\"", "\"GoingAway\""],
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

/// The `[Unreleased]` user-visibility rule (issue #722: no Tests:/CI:
/// release notes) is enforced by both the bash checker and the PowerShell
/// pre-commit hook. Extract the declared pattern from each and require the
/// same `(tests?|ci)` core so the two implementations cannot drift apart.
#[test]
fn test_unreleased_user_visibility_pattern_matches_checker_and_hook() {
    let root = repo_root();
    let checker = read_live_file(&root.join("scripts/check-doc-consistency.sh"));
    let hook = read_live_file(&root.join("scripts/hooks/pre-commit.ps1"));

    const BASH_DECLARATION: &str = "CHANGELOG_FORBIDDEN_BULLET_RE='";
    let bash_start = checker
        .find(BASH_DECLARATION)
        .expect("scripts/check-doc-consistency.sh must declare CHANGELOG_FORBIDDEN_BULLET_RE");
    let bash_body = &checker[bash_start + BASH_DECLARATION.len()..];
    let bash_end = bash_body
        .find('\'')
        .expect("CHANGELOG_FORBIDDEN_BULLET_RE must be a single-quoted POSIX ERE");
    let bash_pattern = &bash_body[..bash_end];

    const HOOK_DECLARATION: &str = "$script:ChangelogForbiddenBulletRes = [string[]]@(";
    let hook_start = hook
        .find(HOOK_DECLARATION)
        .expect("scripts/hooks/pre-commit.ps1 must declare $script:ChangelogForbiddenBulletRes");
    let hook_body = &hook[hook_start + HOOK_DECLARATION.len()..];
    let hook_end = hook_body.find("\n)").expect(
        "$script:ChangelogForbiddenBulletRes array must be closed by a ')' on its own line",
    );
    // Entries are single-quoted .NET regexes and may themselves contain
    // double quotes (markup-wrapper character classes), so parse quoted
    // strings instead of splitting on '"'.
    let hook_patterns: Vec<String> = hook_body[..hook_end]
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            line.strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
                .map(str::to_string)
                .unwrap_or_else(|| {
                    panic!("hook forbidden-bullet entry must be single-quoted: {line}")
                })
        })
        .collect();

    // Both sides must express the same rule: bullets of the form "- Tests:"
    // or "- CI:" (any case, flexible spacing, optional markup wrappers and
    // indentation). The bash side spells out both letter cases because POSIX
    // ERE has no inline case-insensitive flag; the PowerShell side matches
    // case-insensitively via -imatch.
    assert!(
        bash_pattern.contains("([Tt]ests?|[Cc][Ii])"),
        "the bash pattern must spell out both cases of the tests/ci core: {bash_pattern}"
    );
    assert!(
        bash_pattern.starts_with("^[[:space:]]*-+")
            && bash_pattern.contains("[*_\"`]*")
            && bash_pattern.ends_with(":"),
        "the bash pattern must anchor the bullet prefix, tolerate markup wrappers, and end at the tag colon: {bash_pattern}"
    );
    assert_eq!(
        hook_patterns.len(),
        1,
        "the hook must declare exactly one forbidden-bullet pattern: {hook_patterns:?}"
    );
    let hook_pattern = &hook_patterns[0];
    assert!(
        hook_pattern.contains("(tests?|ci)"),
        "the hook pattern must carry the tests/ci core for case-insensitive matching: {hook_pattern}"
    );
    assert!(
        hook_pattern.starts_with("^\\s*-+")
            && hook_pattern.contains("[*_\"`]*")
            && hook_pattern.ends_with(":"),
        "the hook pattern must anchor the bullet prefix, tolerate markup wrappers, and end at the tag colon: {hook_pattern}"
    );
}
