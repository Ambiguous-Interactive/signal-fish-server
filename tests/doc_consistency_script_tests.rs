#![cfg(test)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn unique_temp_dir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("signal-fish-{prefix}-"))
        .tempdir()
        .unwrap_or_else(|e| panic!("Failed to create temporary directory: {e}"))
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("Failed to create {}: {e}", parent.display()));
    }
    fs::write(path, content).unwrap_or_else(|e| panic!("Failed to write {}: {e}", path.display()));
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
}

#[test]
fn test_doc_consistency_script_data_driven_cases() {
    let cases = vec![
        ScriptCase {
            name: "passes_with_valid_fixture",
            overrides: vec![],
            args: vec![],
            expected_exit: 0,
            must_contain: vec!["Doc consistency checks passed"],
        },
        ScriptCase {
            name: "fails_on_stale_dependency_version",
            overrides: vec![(
                "docs/library-usage.md",
                "# Library Usage\n\n```toml\n[dependencies]\nsignal-fish-server = \"0.1\"\n```\n",
            )],
            args: vec![],
            expected_exit: 1,
            must_contain: vec!["stale signal-fish-server version '0.1'"],
        },
        ScriptCase {
            name: "fails_on_non_standard_unreleased_section",
            overrides: vec![(
                "CHANGELOG.md",
                "# Changelog\n\nThe format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).\n\n## [Unreleased]\n\n### Notes\n- Not allowed.\n\n## [0.1.0] - 2026-02-15\n\n[Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.1.0...HEAD\n[0.1.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/releases/tag/v0.1.0\n",
            )],
            args: vec![],
            expected_exit: 1,
            must_contain: vec!["non-keep-a-changelog section"],
        },
        ScriptCase {
            name: "fails_changed_files_gate_when_non_internal_without_changelog",
            overrides: vec![],
            args: vec!["--changed-files", "src/main.rs"],
            expected_exit: 1,
            must_contain: vec![
                "non-internal changes without CHANGELOG.md update",
                "src/main.rs",
            ],
        },
        ScriptCase {
            name: "passes_changed_files_gate_when_non_internal_with_changelog",
            overrides: vec![],
            args: vec!["--changed-files", "src/main.rs", "CHANGELOG.md"],
            expected_exit: 0,
            must_contain: vec!["CHANGELOG.md updated alongside non-internal changes"],
        },
        ScriptCase {
            name: "passes_changed_files_gate_for_internal_only_changes",
            overrides: vec![],
            args: vec!["--changed-files", "scripts/run-local-ci.sh", "tests/integration_tests.rs"],
            expected_exit: 0,
            must_contain: vec!["No non-internal changed files detected"],
        },
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
        },
    ];

    for case in cases {
        let (exit_code, output) = run_checker_with_fixture(&case.overrides, &[], &case.args);

        assert_eq!(
            exit_code, case.expected_exit,
            "Case '{}' exit code mismatch.\nExpected: {}\nActual: {}\nOutput:\n{}",
            case.name, case.expected_exit, exit_code, output
        );

        for needle in case.must_contain {
            assert!(
                output.contains(needle),
                "Case '{}' missing expected output fragment: '{}'\nOutput:\n{}",
                case.name,
                needle,
                output
            );
        }
    }
}
