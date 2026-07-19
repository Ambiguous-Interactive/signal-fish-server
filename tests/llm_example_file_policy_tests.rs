#![cfg(test)]

mod common;

use common::{bash_command, repo_root, unique_temp_dir, write_file};
use std::fs;

fn run_checker_with_fixture(files: &[(&str, &str)], args: &[&str]) -> (i32, String) {
    let temp_root = unique_temp_dir("llm-example-policy");
    let script_src = repo_root().join("scripts/check-llm-example-files.sh");
    let script_dst = temp_root.path().join("scripts/check-llm-example-files.sh");

    let script = fs::read_to_string(&script_src)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", script_src.display()));
    write_file(&script_dst, &script);

    for (relative_path, content) in files {
        write_file(&temp_root.path().join(relative_path), content);
    }

    let mut command = bash_command();
    command.arg("scripts/check-llm-example-files.sh");
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

fn run_checker_in_repo(args: &[&str]) -> (i32, String) {
    let mut command = bash_command();
    command.arg("scripts/check-llm-example-files.sh");
    for arg in args {
        command.arg(arg);
    }
    let output = command
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|e| panic!("Failed to run checker script in repo root: {e}"));

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
    files: Vec<(&'static str, &'static str)>,
    args: Vec<&'static str>,
    expected_exit: i32,
    must_contain: Vec<&'static str>,
}

#[test]
fn test_llm_example_policy_checker_data_driven_cases() {
    let cases = vec![
        ScriptCase {
            name: "passes_when_parent_skill_links_examples",
            files: vec![(
                ".llm/skills/ci-guide/SKILL.md",
                "# CI Guide\n\n## Real-World Cases\n\n- [example](references/example-cache.md)\n",
            )],
            args: vec![],
            expected_exit: 0,
            must_contain: vec![
                "[INFO] Checked 1 skill file(s)",
                "[OK] No inline example sections found in checked skill files.",
            ],
        },
        ScriptCase {
            name: "fails_when_inline_example_heading_exists",
            files: vec![(
                ".llm/skills/ci-guide/SKILL.md",
                "# CI Guide\n\n## Real-World Examples\n\n### Example 1: Cache mismatch\n",
            )],
            args: vec![],
            expected_exit: 1,
            must_contain: vec![
                "inline example heading is disallowed",
                "Move each example into the skill's references/ directory",
            ],
        },
        ScriptCase {
            name: "skips_reference_example_files",
            files: vec![(
                ".llm/skills/ci-guide/references/example-cache.md",
                "# Example\n\n## Example\n\nDetails\n",
            )],
            args: vec![],
            expected_exit: 0,
            must_contain: vec!["[INFO] Checked 0 skill file(s)"],
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

#[test]
fn test_repository_llm_example_policy_checker_passes_on_current_repo() {
    let (exit_code, output) = run_checker_in_repo(&[]);

    assert_eq!(
        exit_code, 0,
        "Repository LLM example policy checker should pass on the current repo.\nOutput:\n{output}"
    );
    assert!(
        !output.contains("[ERROR]"),
        "Repository LLM example policy checker reported errors unexpectedly.\nOutput:\n{output}"
    );
}
