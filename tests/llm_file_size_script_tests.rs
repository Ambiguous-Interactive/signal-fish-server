#![cfg(test)]

mod common;

use common::{bash_command, repo_root, unique_temp_dir, write_file};
use std::fs;

fn make_lines(count: usize) -> String {
    let mut content = String::new();
    for i in 1..=count {
        content.push_str(&format!("line-{i}\n"));
    }
    content
}

fn fixture_file(relative_path: &str, content: String) -> (String, String) {
    (relative_path.to_owned(), content)
}

fn run_checker_with_fixture(files: &[(String, String)], args: &[&str]) -> (i32, String) {
    run_checker_with_fixture_and_env(files, args, &[])
}

fn run_checker_with_fixture_and_env(
    files: &[(String, String)],
    args: &[&str],
    env: &[(&str, &str)],
) -> (i32, String) {
    let temp_root = unique_temp_dir("llm-size-check");
    let script_src = repo_root().join("scripts/check-llm-file-sizes.sh");
    let script_dst = temp_root.path().join("scripts/check-llm-file-sizes.sh");

    let script = fs::read_to_string(&script_src)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", script_src.display()));
    write_file(&script_dst, &script);

    for (relative_path, content) in files {
        write_file(&temp_root.path().join(relative_path), content);
    }

    let mut command = bash_command();
    command.arg("scripts/check-llm-file-sizes.sh");
    for arg in args {
        command.arg(arg);
    }
    for (key, value) in env {
        command.env(key, value);
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

fn describe_exit_code(exit_code: i32) -> String {
    if exit_code > 128 {
        let signal = exit_code - 128;
        let signal_name = match signal {
            1 => "SIGHUP",
            2 => "SIGINT",
            3 => "SIGQUIT",
            6 => "SIGABRT",
            9 => "SIGKILL",
            13 => "SIGPIPE",
            15 => "SIGTERM",
            _ => "signal",
        };
        format!("{exit_code} (128 + {signal_name})")
    } else {
        exit_code.to_string()
    }
}

#[derive(Debug)]
struct ScriptCase {
    name: &'static str,
    files: Vec<(String, String)>,
    args: Vec<&'static str>,
    expected_exit: i32,
    must_contain: Vec<&'static str>,
}

#[test]
fn test_llm_file_size_checker_data_driven_cases() {
    let cases = vec![
        ScriptCase {
            name: "passes_when_all_files_within_limit",
            files: vec![fixture_file(".llm/skills/ok.md", make_lines(42))],
            args: vec![],
            expected_exit: 0,
            must_contain: vec![
                "[INFO] Checked 1 file(s) in .llm/",
                "[OK] All 1 LLM file(s) are within the 300-line limit.",
            ],
        },
        ScriptCase {
            name: "warns_when_file_hits_limit",
            files: vec![fixture_file(".llm/skills/near-limit.md", make_lines(300))],
            args: vec![],
            expected_exit: 0,
            must_contain: vec![
                "[WARN] .llm/skills/near-limit.md: 300 lines (at limit — next added line will fail)",
                "[WARN] LLM file size check passed with 1 warning(s)",
            ],
        },
        ScriptCase {
            name: "fails_when_file_exceeds_limit",
            files: vec![fixture_file(".llm/skills/too-long.md", make_lines(301))],
            args: vec![],
            expected_exit: 1,
            must_contain: vec![
                "[ERROR] .llm/skills/too-long.md: 301 lines (max: 300 — exceeds by 1)",
                "[ERROR] LLM file size check found 1 file(s) exceeding the 300-line limit",
            ],
        },
        ScriptCase {
            name: "files_mode_skips_generated_index",
            files: vec![
                fixture_file(".llm/skills/index.md", make_lines(800)),
                fixture_file(".llm/context.md", make_lines(5)),
            ],
            args: vec!["--files", ".llm/skills/index.md", ".llm/context.md"],
            expected_exit: 0,
            must_contain: vec![
                "[INFO] Scanning 2 explicitly provided file(s) for size violations...",
                "[INFO] Checked 1 explicitly provided file(s)",
            ],
        },
        ScriptCase {
            name: "directory_scan_skips_generated_index",
            files: vec![
                fixture_file(".llm/skills/index.md", make_lines(800)),
                fixture_file(".llm/skills/ok.md", make_lines(5)),
            ],
            args: vec![],
            expected_exit: 0,
            must_contain: vec![
                "[INFO] Scanning files in .llm/ for size violations...",
                "[INFO] Checked 1 file(s) in .llm/",
                "[OK] All 1 LLM file(s) are within the 300-line limit.",
            ],
        },
        ScriptCase {
            name: "directory_scan_enforces_non_generated_index_files",
            files: vec![
                fixture_file(".llm/skills/index.md", make_lines(800)),
                fixture_file(".llm/references/index.md", make_lines(301)),
            ],
            args: vec![],
            expected_exit: 1,
            must_contain: vec![
                "[INFO] Checked 1 file(s) in .llm/",
                "[ERROR] .llm/references/index.md: 301 lines (max: 300 — exceeds by 1)",
            ],
        },
        ScriptCase {
            name: "files_mode_warns_for_missing_paths",
            files: vec![fixture_file(".llm/context.md", make_lines(8))],
            args: vec!["--files", ".llm/missing.md", ".llm/context.md"],
            expected_exit: 0,
            must_contain: vec![
                "[WARN] Skipping non-existent file: .llm/missing.md",
                "[WARN] Skipped 1 missing file argument(s).",
                "[INFO] Checked 1 explicitly provided file(s)",
            ],
        },
        ScriptCase {
            name: "files_mode_supports_spaces_in_paths",
            files: vec![fixture_file(".llm/skills/space file.md", make_lines(12))],
            args: vec!["--files", ".llm/skills/space file.md"],
            expected_exit: 0,
            must_contain: vec!["[INFO] Checked 1 explicitly provided file(s)"],
        },
        ScriptCase {
            name: "files_mode_requires_arguments",
            files: vec![],
            args: vec!["--files"],
            expected_exit: 2,
            must_contain: vec!["[ERROR] --files requires at least one file argument"],
        },
    ];

    for case in cases {
        let (exit_code, output) = run_checker_with_fixture(&case.files, &case.args);

        assert_eq!(
            exit_code,
            case.expected_exit,
            "Case '{}' exit code mismatch.\nExpected: {}\nActual: {}\nOutput:\n{}",
            case.name,
            describe_exit_code(case.expected_exit),
            describe_exit_code(exit_code),
            output
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

#[test]
fn test_warning_threshold_matches_documented_policy() {
    let cases = [
        (294, None),
        (295, Some("5 lines from limit")),
        (299, Some("1 line from limit")),
        (300, Some("at limit")),
    ];

    for (line_count, warning_fragment) in cases {
        let files = vec![fixture_file(
            ".llm/skills/threshold.md",
            make_lines(line_count),
        )];
        let (exit_code, output) = run_checker_with_fixture(&files, &[]);

        assert_eq!(
            exit_code, 0,
            "line count {line_count} should pass.\nOutput:\n{output}"
        );

        if let Some(fragment) = warning_fragment {
            assert!(
                output.contains("[WARN] .llm/skills/threshold.md")
                    && output.contains(fragment),
                "line count {line_count} should warn with fragment {fragment:?}.\nOutput:\n{output}"
            );
        } else {
            assert!(
                !output.contains("[WARN] .llm/skills/threshold.md"),
                "line count {line_count} is below the documented warning zone and should not warn.\nOutput:\n{output}"
            );
        }
    }
}

#[test]
fn test_warning_diagnostics_consume_all_sorted_input_under_pipefail() {
    let script = fs::read_to_string(repo_root().join("scripts/check-llm-file-sizes.sh"))
        .expect("failed to read LLM size checker script");
    assert!(
        !script.contains("| head"),
        "diagnostic pipelines must not use head under pipefail; early pipe closure can surface as exit 141 (SIGPIPE)"
    );

    let files = (1..=16)
        .map(|index| {
            fixture_file(
                &format!(".llm/skills/near-limit-{index:03}.md"),
                make_lines(300),
            )
        })
        .collect::<Vec<_>>();
    let (exit_code, output) =
        run_checker_with_fixture_and_env(&files, &[], &[("GITHUB_ACTIONS", "true")]);

    assert_eq!(
        exit_code, 0,
        "warning-only diagnostics must not trip pipefail or SIGPIPE.\nActual exit: {}\nOutput:\n{output}",
        describe_exit_code(exit_code)
    );
    assert!(
        output.contains("Largest checked .llm files:")
            && output.contains("[WARN] LLM file size check passed with 16 warning(s)"),
        "warning diagnostics should print the largest-file table and final summary.\nOutput:\n{output}"
    );
}
