#![cfg(test)]

mod common;

use common::{bash_command, repo_root, unique_temp_dir, write_file};
use std::fs;
use std::thread;

use regex::RegexBuilder;

fn base_fixture_files() -> Vec<(&'static str, String)> {
    vec![
        (
            "Cargo.toml",
            r#"[package]
name = "signal-fish-server"
version = "0.1.1"
edition = "2021"
"#
            .to_string(),
        ),
        (
            "docs/library-usage.md",
            r#"# Library Usage

```toml
[dependencies]
signal-fish-server = "0.1.1"
```

```toml
[dependencies]
signal-fish-server = { version = "0.1.1", features = ["tls"] }
```
"#
            .to_string(),
        ),
        (
            ".llm/context.md",
            r#"# Context

- **Version:** 0.1.1

[v2 client sample](code-samples/protocol/v2-client-messages.jsonl)
[v2 server sample](code-samples/protocol/v2-server-messages.jsonl)
"#
            .to_string(),
        ),
        (
            "README.md",
            r#"# README

[v2 client sample](.llm/code-samples/protocol/v2-client-messages.jsonl)
[v2 server sample](.llm/code-samples/protocol/v2-server-messages.jsonl)
"#
            .to_string(),
        ),
        (
            ".llm/code-samples/protocol/v2-client-messages.jsonl",
            r#"{"type":"Authenticate","data":{"app_id":"...","sdk_version":"1.2.3","platform":"unity"}}
{"type":"JoinRoom","data":{"game_name":"...","player_name":"Player1","room_code":"ABC123"}}
"#
            .to_string(),
        ),
        (
            ".llm/code-samples/protocol/v2-server-messages.jsonl",
            r#"{"type":"Authenticated","data":{"app_name":"my-game","rate_limits":{"per_minute":60}}}
{"type":"ProtocolInfo","data":{"capabilities":["reconnection"],"game_data_formats":["json"]}}
"#
            .to_string(),
        ),
        (
            "CHANGELOG.md",
            r#"# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Changed
- Keep docs aligned with protocol payloads.

## [0.1.0] - 2026-02-15

### Added
- Initial release.

[Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/releases/tag/v0.1.0
"#
            .to_string(),
        ),
    ]
}

fn run_checker_with_fixture(
    overrides: &[(&str, &str)],
    extra_files: &[(&str, &str)],
    args: &[&str],
) -> (i32, String) {
    let temp_root = unique_temp_dir("doc-consistency");
    let script_src = repo_root().join("scripts/check-doc-consistency.sh");
    let script_dst = temp_root.path().join("scripts/check-doc-consistency.sh");

    let script = fs::read_to_string(&script_src)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", script_src.display()));
    write_file(&script_dst, &script);

    for (path, content) in base_fixture_files() {
        write_file(&temp_root.path().join(path), &content);
    }

    for (path, content) in overrides {
        write_file(&temp_root.path().join(path), content);
    }

    for (path, content) in extra_files {
        write_file(&temp_root.path().join(path), content);
    }

    let mut command = bash_command();
    command.arg("scripts/check-doc-consistency.sh");
    for arg in args {
        command.arg(arg);
    }

    let output = command
        .current_dir(temp_root.path())
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "Failed to run checker script in {}: {e}",
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

#[derive(Debug)]
struct ScriptCase {
    name: &'static str,
    overrides: Vec<(&'static str, &'static str)>,
    args: Vec<&'static str>,
    expected_exit: i32,
    must_contain: Vec<&'static str>,
    /// Fragments that must NOT appear in the script output.
    ///
    /// Use error-line-specific prefixes (e.g., `"  - Cargo.lock"`) rather than
    /// bare filenames to avoid false positives from diagnostic help text.
    /// The `"  - "` prefix matches the error-listed file format produced by
    /// `scripts/check-doc-consistency.sh` (see the format-stability comment
    /// near its `is_internal_path` error loop).
    must_not_contain: Vec<&'static str>,
}

fn evaluate_script_case(case: ScriptCase) -> Option<String> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_checker_with_fixture(&case.overrides, &[], &case.args)
    }));

    let (exit_code, output) = match result {
        Ok(result) => result,
        Err(payload) => {
            let message = if let Some(message) = payload.downcast_ref::<&str>() {
                *message
            } else if let Some(message) = payload.downcast_ref::<String>() {
                message.as_str()
            } else {
                "unknown panic"
            };
            return Some(format!("Case '{}' panicked: {message}", case.name));
        }
    };

    let mut failures = Vec::new();
    if exit_code != case.expected_exit {
        failures.push(format!(
            "exit code mismatch. Expected: {}. Actual: {}.",
            case.expected_exit, exit_code
        ));
    }

    for needle in &case.must_contain {
        if !output.contains(needle) {
            failures.push(format!("missing expected output fragment: '{needle}'"));
        }
    }

    for needle in &case.must_not_contain {
        if output.contains(needle) {
            failures.push(format!(
                "unexpectedly contains forbidden fragment: '{needle}'"
            ));
        }
    }

    if failures.is_empty() {
        None
    } else {
        Some(format!(
            "Case '{}' failed:\n{}\nFull output:\n{}",
            case.name,
            failures.join("\n"),
            output
        ))
    }
}

fn evaluate_script_cases_in_batches(cases: Vec<ScriptCase>) -> Vec<String> {
    let parallelism = thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(4)
        .clamp(1, 6);
    let mut failures = Vec::new();
    let mut remaining = cases.into_iter();

    loop {
        let batch = remaining.by_ref().take(parallelism).collect::<Vec<_>>();
        if batch.is_empty() {
            break;
        }

        let batch_failures = thread::scope(|scope| {
            let handles = batch
                .into_iter()
                .map(|case| scope.spawn(move || evaluate_script_case(case)))
                .collect::<Vec<_>>();
            let mut failures = Vec::new();

            for handle in handles {
                match handle.join() {
                    Ok(Some(failure)) => failures.push(failure),
                    Ok(None) => {}
                    Err(_) => failures.push("script case worker panicked unexpectedly".to_string()),
                }
            }

            failures
        });

        failures.extend(batch_failures);
    }

    failures
}

#[test]
fn test_doc_consistency_script_data_driven_cases() {
    let cases = vec![
        // ---------------------------------------------------------------
        // Basic fixture validation
        // ---------------------------------------------------------------
        ScriptCase {
            name: "passes_with_valid_fixture",
            overrides: vec![],
            args: vec![],
            expected_exit: 0,
            must_contain: vec!["Doc consistency checks passed"],
            must_not_contain: vec!["[ERROR]"],
        },
        // ---------------------------------------------------------------
        // Version sync checks (gate 1)
        // ---------------------------------------------------------------
        ScriptCase {
            name: "fails_on_stale_dependency_version",
            overrides: vec![(
                "docs/library-usage.md",
                "# Library Usage\n\n```toml\n[dependencies]\nsignal-fish-server = \"0.1\"\n```\n",
            )],
            args: vec![],
            expected_exit: 1,
            must_contain: vec!["stale signal-fish-server version '0.1'"],
            must_not_contain: vec![],
        },
        ScriptCase {
            name: "fails_on_stale_context_version",
            overrides: vec![(
                ".llm/context.md",
                "# Context\n\n- **Version:** 0.0.9\n\n[v2 client sample](code-samples/protocol/v2-client-messages.jsonl)\n[v2 server sample](code-samples/protocol/v2-server-messages.jsonl)\n",
            )],
            args: vec![],
            expected_exit: 1,
            must_contain: vec![".llm/context.md must contain exact line"],
            must_not_contain: vec![],
        },
        // ---------------------------------------------------------------
        // Changelog format checks (gate 2)
        // ---------------------------------------------------------------
        ScriptCase {
            name: "fails_on_non_standard_unreleased_section",
            overrides: vec![(
                "CHANGELOG.md",
                "# Changelog\n\nThe format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).\n\n## [Unreleased]\n\n### Notes\n- Not allowed.\n\n## [0.1.0] - 2026-02-15\n\n[Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.1.0...HEAD\n[0.1.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/releases/tag/v0.1.0\n",
            )],
            args: vec![],
            expected_exit: 1,
            must_contain: vec!["non-keep-a-changelog section"],
            must_not_contain: vec![],
        },
        ScriptCase {
            name: "fails_on_missing_unreleased_heading",
            overrides: vec![(
                "CHANGELOG.md",
                "# Changelog\n\nThe format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).\n\n## [0.1.0] - 2026-02-15\n\n### Added\n- Initial release.\n\n[0.1.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/releases/tag/v0.1.0\n",
            )],
            args: vec![],
            expected_exit: 1,
            must_contain: vec!["must contain an exact '## [Unreleased]' heading"],
            must_not_contain: vec![],
        },
        ScriptCase {
            name: "fails_on_unbracketed_unreleased_heading",
            overrides: vec![(
                "CHANGELOG.md",
                "# Changelog\n\nThe format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).\n\n## Unreleased\n\n## [Unreleased]\n\n## [0.1.0] - 2026-02-15\n\n### Added\n- Initial release.\n\n[Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.1.0...HEAD\n[0.1.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/releases/tag/v0.1.0\n",
            )],
            args: vec![],
            expected_exit: 1,
            must_contain: vec!["[Unreleased]' (bracketed)"],
            must_not_contain: vec![],
        },
        ScriptCase {
            name: "fails_on_undated_current_version_header",
            overrides: vec![(
                "CHANGELOG.md",
                "# Changelog\n\nThe format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).\n\n## [Unreleased]\n\n## [0.1.1]\n\n### Added\n- Something.\n\n## [0.1.0] - 2026-02-15\n\n### Added\n- Initial release.\n\n[Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.1.0...HEAD\n[0.1.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/releases/tag/v0.1.0\n",
            )],
            args: vec![],
            expected_exit: 1,
            must_contain: vec!["undated current-version header"],
            must_not_contain: vec![],
        },
        ScriptCase {
            name: "fails_on_missing_unreleased_link_reference",
            overrides: vec![(
                "CHANGELOG.md",
                "# Changelog\n\nThe format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).\n\n## [Unreleased]\n\n## [0.1.0] - 2026-02-15\n\n### Added\n- Initial release.\n\n[0.1.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/releases/tag/v0.1.0\n",
            )],
            args: vec![],
            expected_exit: 1,
            must_contain: vec!["must define a [Unreleased]: link reference"],
            must_not_contain: vec![],
        },
        ScriptCase {
            name: "fails_when_release_compare_link_references_unknown_previous_version",
            overrides: vec![(
                "CHANGELOG.md",
                "# Changelog\n\nThe format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).\n\n## [Unreleased]\n\n## [0.2.0] - 2026-02-24\n\n### Added\n- Release notes.\n\n## [0.1.0] - 2026-02-15\n\n### Added\n- Initial release.\n\n[Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.2.0...HEAD\n[0.2.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.9.0...v0.2.0\n[0.1.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/releases/tag/v0.1.0\n",
            )],
            args: vec![],
            expected_exit: 1,
            must_contain: vec!["compare link references unknown previous version"],
            must_not_contain: vec![],
        },
        // ---------------------------------------------------------------
        // Protocol anti-drift checks (gate 3)
        // ---------------------------------------------------------------
        ScriptCase {
            name: "fails_on_stale_protocol_token_in_readme",
            overrides: vec![
                (
                    "README.md",
                    "# README\n\n{\"type\":\"Authenticated\",\"data\":{\"server_version\":\"2.0.0\"}}\n",
                ),
            ],
            args: vec![],
            expected_exit: 1,
            must_contain: vec!["stale protocol token 'server_version'"],
            must_not_contain: vec![],
        },
        ScriptCase {
            name: "fails_on_stale_protocol_token_in_sample_file",
            overrides: vec![(
                ".llm/code-samples/protocol/v2-server-messages.jsonl",
                "{\"type\":\"Authenticated\",\"data\":{\"server_version\":\"2.0.0\"}}\n",
            )],
            args: vec![],
            expected_exit: 1,
            must_contain: vec!["stale protocol token 'server_version'"],
            must_not_contain: vec![],
        },
        ScriptCase {
            name: "fails_when_context_omits_canonical_sample_references",
            overrides: vec![(".llm/context.md", "# Context\n\n- **Version:** 0.1.1\n")],
            args: vec![],
            expected_exit: 1,
            must_contain: vec![
                ".llm/context.md must reference canonical protocol sample: code-samples/protocol/v2-client-messages.jsonl",
                ".llm/context.md must reference canonical protocol sample: code-samples/protocol/v2-server-messages.jsonl",
            ],
            must_not_contain: vec![],
        },
        // ---------------------------------------------------------------
        // Changelog gate: basic non-internal detection (gate 4)
        // ---------------------------------------------------------------
        ScriptCase {
            name: "fails_changed_files_gate_when_non_internal_without_changelog",
            overrides: vec![],
            args: vec!["--changed-files", "src/main.rs"],
            expected_exit: 1,
            must_contain: vec![
                "non-internal changes without CHANGELOG.md update",
                "src/main.rs",
            ],
            must_not_contain: vec![],
        },
        ScriptCase {
            name: "passes_changed_files_gate_when_non_internal_with_changelog",
            overrides: vec![],
            args: vec!["--changed-files", "src/main.rs", "CHANGELOG.md"],
            expected_exit: 0,
            must_contain: vec!["CHANGELOG.md updated alongside non-internal changes"],
            must_not_contain: vec!["[ERROR]"],
        },
        ScriptCase {
            name: "passes_changed_files_gate_for_internal_only_changes",
            overrides: vec![],
            args: vec!["--changed-files", "scripts/run-local-ci.sh", "tests/integration_tests.rs"],
            expected_exit: 0,
            must_contain: vec!["No non-internal changed files detected"],
            must_not_contain: vec!["[ERROR]"],
        },
        ScriptCase {
            name: "multiple_non_internal_files_all_listed_in_error",
            overrides: vec![],
            args: vec!["--changed-files", "src/main.rs", "Cargo.toml", "README.md"],
            expected_exit: 1,
            must_contain: vec![
                "non-internal changes without CHANGELOG.md update",
                "src/main.rs",
                "Cargo.toml",
                "README.md",
            ],
            must_not_contain: vec![],
        },
        ScriptCase {
            name: "changelog_alone_does_not_fail",
            overrides: vec![],
            args: vec!["--changed-files", "CHANGELOG.md"],
            expected_exit: 0,
            must_contain: vec!["No non-internal changed files detected"],
            must_not_contain: vec!["[ERROR]"],
        },
        // ---------------------------------------------------------------
        // Changelog gate: Cargo.toml / Cargo.lock edge cases
        // ---------------------------------------------------------------
        ScriptCase {
            name: "cargo_toml_is_non_internal_without_skip_flag",
            overrides: vec![],
            args: vec!["--changed-files", "Cargo.toml"],
            expected_exit: 1,
            must_contain: vec!["non-internal changes without CHANGELOG"],
            must_not_contain: vec![],
        },
        ScriptCase {
            name: "cargo_lock_is_internal",
            overrides: vec![],
            args: vec!["--changed-files", "Cargo.lock"],
            expected_exit: 0,
            must_contain: vec!["No non-internal changed files detected"],
            must_not_contain: vec!["[ERROR]"],
        },
        ScriptCase {
            name: "cargo_toml_plus_lock_fails_without_skip_flag",
            overrides: vec![],
            args: vec!["--changed-files", "Cargo.toml", "Cargo.lock"],
            expected_exit: 1,
            must_contain: vec![
                "non-internal changes without CHANGELOG",
                "Cargo.toml",
            ],
            must_not_contain: vec!["  - Cargo.lock"],
        },
        ScriptCase {
            name: "cargo_toml_plus_src_fails_without_skip_flag",
            overrides: vec![],
            args: vec!["--changed-files", "Cargo.toml", "src/lib.rs"],
            expected_exit: 1,
            must_contain: vec![
                "non-internal changes without CHANGELOG",
                "Cargo.toml",
                "src/lib.rs",
            ],
            must_not_contain: vec![],
        },
        ScriptCase {
            name: "cargo_toml_plus_lock_with_changelog_passes",
            overrides: vec![],
            args: vec!["--changed-files", "Cargo.toml", "Cargo.lock", "CHANGELOG.md"],
            expected_exit: 0,
            must_contain: vec!["CHANGELOG.md updated alongside non-internal changes"],
            must_not_contain: vec!["[ERROR]"],
        },
        // Regression test: the diagnostic help text that explains internal
        // path patterns must never cause "  - Cargo.lock" to appear as if
        // it were a non-internal error-listed file.
        ScriptCase {
            name: "cargo_toml_alone_does_not_list_lock_as_non_internal",
            overrides: vec![],
            args: vec!["--changed-files", "Cargo.toml"],
            expected_exit: 1,
            must_contain: vec![
                "non-internal changes without CHANGELOG",
                "Cargo.toml",
            ],
            must_not_contain: vec!["  - Cargo.lock"],
        },
        // ---------------------------------------------------------------
        // --skip-changelog-gate flag
        // ---------------------------------------------------------------
        ScriptCase {
            name: "skip_changelog_gate_passes_with_non_internal_changes",
            overrides: vec![],
            args: vec![
                "--skip-changelog-gate",
                "--changed-files",
                "src/main.rs",
            ],
            expected_exit: 0,
            must_contain: vec!["Changelog gate skipped"],
            must_not_contain: vec!["[ERROR]"],
        },
        ScriptCase {
            name: "skip_changelog_gate_passes_with_cargo_toml_change",
            overrides: vec![],
            args: vec![
                "--skip-changelog-gate",
                "--changed-files",
                "Cargo.toml",
                "Cargo.lock",
            ],
            expected_exit: 0,
            must_contain: vec!["Changelog gate skipped"],
            must_not_contain: vec!["[ERROR]"],
        },
        ScriptCase {
            name: "skip_flag_bypasses_gate_for_internal_only_cargo_lock",
            overrides: vec![],
            args: vec![
                "--skip-changelog-gate",
                "--changed-files",
                "Cargo.lock",
            ],
            expected_exit: 0,
            must_contain: vec!["Changelog gate skipped"],
            must_not_contain: vec!["[ERROR]"],
        },
        // ---------------------------------------------------------------
        // Internal path classification: deeply nested paths
        // ---------------------------------------------------------------
        ScriptCase {
            name: "deep_nested_github_is_internal",
            overrides: vec![],
            args: vec!["--changed-files", ".github/workflows/deep/nested.yml"],
            expected_exit: 0,
            must_contain: vec!["No non-internal changed files detected"],
            must_not_contain: vec!["[ERROR]"],
        },
        ScriptCase {
            name: "deep_nested_llm_is_internal",
            overrides: vec![],
            args: vec!["--changed-files", ".llm/skills/sub/deep/file.md"],
            expected_exit: 0,
            must_contain: vec!["No non-internal changed files detected"],
            must_not_contain: vec!["[ERROR]"],
        },
        ScriptCase {
            name: "deep_nested_tests_is_internal",
            overrides: vec![],
            args: vec!["--changed-files", "tests/integration/sub/test.rs"],
            expected_exit: 0,
            must_contain: vec!["No non-internal changed files detected"],
            must_not_contain: vec!["[ERROR]"],
        },
        // ---------------------------------------------------------------
        // Mixed internal + non-internal combinations
        // ---------------------------------------------------------------
        ScriptCase {
            name: "mix_internal_and_non_internal_without_changelog_fails",
            overrides: vec![],
            args: vec![
                "--changed-files",
                "scripts/foo.sh",
                "src/main.rs",
                ".github/workflows/ci.yml",
            ],
            expected_exit: 1,
            must_contain: vec![
                "non-internal changes without CHANGELOG",
                "src/main.rs",
            ],
            must_not_contain: vec!["  - scripts/foo.sh", "  - .github/workflows/ci.yml"],
        },
        ScriptCase {
            name: "mix_internal_and_non_internal_with_changelog_passes",
            overrides: vec![],
            args: vec![
                "--changed-files",
                "scripts/foo.sh",
                "src/main.rs",
                ".github/workflows/ci.yml",
                "CHANGELOG.md",
            ],
            expected_exit: 0,
            must_contain: vec!["CHANGELOG.md updated alongside non-internal changes"],
            must_not_contain: vec!["[ERROR]"],
        },
        // ---------------------------------------------------------------
        // Argument parsing edge cases
        // ---------------------------------------------------------------
        ScriptCase {
            name: "changed_files_without_paths_exits_with_code_2",
            overrides: vec![],
            args: vec!["--changed-files"],
            expected_exit: 2,
            must_contain: vec!["--changed-files requires at least one file path"],
            must_not_contain: vec![],
        },
        ScriptCase {
            name: "unknown_argument_exits_with_code_2",
            overrides: vec![],
            args: vec!["--nonexistent-flag"],
            expected_exit: 2,
            must_contain: vec!["Unknown argument"],
            must_not_contain: vec![],
        },
        ScriptCase {
            name: "skip_flag_after_changed_files_is_rejected",
            overrides: vec![],
            args: vec![
                "--changed-files",
                "src/main.rs",
                "--skip-changelog-gate",
            ],
            expected_exit: 2,
            must_contain: vec![
                "Unexpected flag '--skip-changelog-gate' after --changed-files",
                "must come before --changed-files",
            ],
            must_not_contain: vec![],
        },
        // ---------------------------------------------------------------
        // Dependabot squash-merge scenario (the original CI failure)
        //
        // When a human squash-merges a Dependabot PR, the changed files
        // are Cargo.toml + Cargo.lock, but the actor is the human, not
        // dependabot[bot]. The CI dep-detect step uses commit message
        // pattern matching to handle this. The script itself only sees
        // --skip-changelog-gate (set by CI) or --changed-files. This
        // test verifies the script correctly handles the skip flag for
        // the exact file set from a dependency bump.
        // ---------------------------------------------------------------
        ScriptCase {
            name: "dependabot_squash_merge_with_skip_flag_passes",
            overrides: vec![],
            args: vec![
                "--skip-changelog-gate",
                "--changed-files",
                "Cargo.toml",
                "Cargo.lock",
            ],
            expected_exit: 0,
            must_contain: vec!["Changelog gate skipped"],
            must_not_contain: vec!["[ERROR]"],
        },
        ScriptCase {
            name: "dependabot_squash_merge_without_skip_flag_fails",
            overrides: vec![],
            args: vec![
                "--changed-files",
                "Cargo.toml",
                "Cargo.lock",
                ".github/dependabot.yml",
            ],
            expected_exit: 1,
            must_contain: vec![
                "non-internal changes without CHANGELOG",
                "Cargo.toml",
            ],
            must_not_contain: vec![
                "  - Cargo.lock",
                "  - .github/dependabot.yml",
            ],
        },
    ];

    let failures = evaluate_script_cases_in_batches(cases);
    assert!(
        failures.is_empty(),
        "Doc consistency script cases failed:\n\n{}",
        failures.join("\n\n")
    );
}

/// The CI workflow's dep-detect step uses a grep ERE pattern to identify
/// dependency-bump commit messages. This test verifies that pattern against
/// real-world and edge-case commit messages.
///
/// The pattern from ci.yml (with `-i` for case-insensitive matching):
///   `(^bump |^chore\(deps\)|dependabot|dependency.bump|update.*dependencies)`
#[test]
fn test_dep_detect_commit_message_pattern_data_driven() {
    // This is the ERE pattern from the CI workflow, converted to Rust regex.
    // The CI uses `grep -qiE`, so we replicate case-insensitive matching.
    let pattern = r"(^bump |^chore\(deps\)|dependabot|dependency.bump|update.*dependencies)";
    let re = RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .unwrap_or_else(|e| panic!("Failed to compile dep-detect pattern: {e}"));

    struct CommitMsgCase {
        message: &'static str,
        should_match: bool,
        description: &'static str,
    }

    let cases = [
        // Patterns that SHOULD match (dependency bumps)
        CommitMsgCase {
            message: "chore(deps): Bump tempfile from 3.26.0 to 3.27.0 (#50)",
            should_match: true,
            description: "standard Dependabot squash-merge commit (fixed PR title)",
        },
        CommitMsgCase {
            message: "chore(deps)(deps): Bump uuid from 1.21.0 to 1.22.0 (#43)",
            should_match: true,
            description: "doubled (deps) scope from earlier Dependabot config",
        },
        CommitMsgCase {
            message: "Bump tokio from 1.49.0 to 1.50.0",
            should_match: true,
            description: "bare Bump prefix (default Dependabot title)",
        },
        CommitMsgCase {
            message: "bump serde from 1.0.200 to 1.0.201",
            should_match: true,
            description: "lowercase bump prefix",
        },
        CommitMsgCase {
            message: "chore(deps): update dependencies",
            should_match: true,
            description: "manual dependency update with chore(deps) scope",
        },
        CommitMsgCase {
            message: "Update all dependencies to latest",
            should_match: true,
            description: "manual bulk update message",
        },
        CommitMsgCase {
            message: "dependency bump: serde_json 1.0 -> 1.1",
            should_match: true,
            description: "dependency bump with space separator",
        },
        CommitMsgCase {
            message: "dependency-bump: serde_json 1.0 -> 1.1",
            should_match: true,
            description: "dependency-bump with hyphen separator (dot matches any char)",
        },
        CommitMsgCase {
            message: "Merged dependabot PR for axum update",
            should_match: true,
            description: "commit message mentioning dependabot by name",
        },
        CommitMsgCase {
            message: "chore(deps): Bump the patch-updates group with 2 updates",
            should_match: true,
            description: "grouped Dependabot PR",
        },
        // Patterns that should NOT match (real code changes)
        CommitMsgCase {
            message: "feat: add WebSocket reconnection support",
            should_match: false,
            description: "feature commit",
        },
        CommitMsgCase {
            message: "fix: resolve race condition in room cleanup",
            should_match: false,
            description: "bugfix commit",
        },
        CommitMsgCase {
            message: "refactor: extract room management into submodule",
            should_match: false,
            description: "refactoring commit",
        },
        CommitMsgCase {
            message: "docs: update API documentation for v2 protocol",
            should_match: false,
            description: "documentation commit",
        },
        CommitMsgCase {
            message: "test: add integration tests for player authority",
            should_match: false,
            description: "test-only commit",
        },
        CommitMsgCase {
            message: "chore: clean up unused imports",
            should_match: false,
            description: "chore commit without deps scope",
        },
        CommitMsgCase {
            message: "chore(ci): update workflow permissions",
            should_match: false,
            description: "CI chore without deps scope",
        },
        CommitMsgCase {
            message: "perf: optimize message serialization",
            should_match: false,
            description: "performance commit",
        },
    ];

    for case in cases {
        let matched = re.is_match(case.message);
        assert_eq!(
            matched,
            case.should_match,
            "Commit message pattern {} for '{}' ({}).\n\
             Pattern: {pattern}\n\
             Expected match: {}\n\
             Actual match: {matched}",
            if case.should_match {
                "failed to match"
            } else {
                "unexpectedly matched"
            },
            case.message,
            case.description,
            case.should_match,
        );
    }
}
