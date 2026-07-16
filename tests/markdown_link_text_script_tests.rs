#![cfg(test)]

mod common;

use common::{bash_command, repo_root, unique_temp_dir, write_file};
use std::fs;

fn run_checker_with_fixture(files: &[(&str, &str)], args: &[&str]) -> (i32, String) {
    let temp_root = unique_temp_dir("md-link-text");
    let script_src = repo_root().join("scripts/check-markdown-link-text.sh");
    let script_dst = temp_root.path().join("scripts/check-markdown-link-text.sh");

    let script = fs::read_to_string(&script_src)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", script_src.display()));
    write_file(&script_dst, &script);

    // Script discovers files through git ls-files by default.
    // Initialize a local repo for deterministic fixture discovery.
    let init = bash_command()
        .arg("-lc")
        .arg("git init -q && git config user.email test@example.com && git config user.name test")
        .current_dir(temp_root.path())
        .output()
        .unwrap_or_else(|e| panic!("Failed to initialize temporary git repo: {e}"));
    assert!(
        init.status.success(),
        "Failed to initialize temp git repo:\n{}",
        String::from_utf8_lossy(&init.stderr)
    );

    for (relative_path, content) in files {
        write_file(&temp_root.path().join(relative_path), content);
    }

    let add = bash_command()
        .arg("-lc")
        .arg("git add .")
        .current_dir(temp_root.path())
        .output()
        .unwrap_or_else(|e| panic!("Failed to stage fixture files: {e}"));
    assert!(
        add.status.success(),
        "Failed to stage fixture files:\n{}",
        String::from_utf8_lossy(&add.stderr)
    );

    let mut command = bash_command();
    command.arg("scripts/check-markdown-link-text.sh");
    for arg in args {
        command.arg(arg);
    }

    let output = command
        .current_dir(temp_root.path())
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "Failed to run markdown link text checker in {}: {e}",
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
    command.arg("scripts/check-markdown-link-text.sh");
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
fn test_markdown_link_text_checker_data_driven_cases() {
    let cases = vec![
        ScriptCase {
            name: "fails_on_filename_style_link_text",
            files: vec![(
                "docs/a.md",
                "See [testing-core-patterns](../.agents/skills/testing-rust/references/testing-core-patterns.md).\n",
            )],
            args: vec![],
            expected_exit: 1,
            must_contain: vec![
                "Found 1 filename-style internal markdown link(s).",
                "[testing-core-patterns](../.agents/skills/testing-rust/references/testing-core-patterns.md) -> [Testing Core Patterns]",
            ],
        },
        ScriptCase {
            name: "passes_on_human_readable_link_text",
            files: vec![(
                "docs/a.md",
                "See [Testing Core Patterns](../.agents/skills/testing-rust/references/testing-core-patterns.md).\n",
            )],
            args: vec![],
            expected_exit: 0,
            must_contain: vec!["[OK] No filename-style internal markdown links found."],
        },
        ScriptCase {
            name: "fix_mode_rewrites_filename_style_link",
            files: vec![(
                "docs/a.md",
                "See [testing-core-patterns](../.agents/skills/testing-rust/references/testing-core-patterns.md).\n",
            )],
            args: vec!["--fix"],
            expected_exit: 0,
            must_contain: vec!["[OK] Applied auto-fixes for all reported links."],
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
                "Case '{}' missing expected output fragment: '{}'.\nOutput:\n{}",
                case.name,
                needle,
                output
            );
        }
    }
}

#[test]
fn test_repository_markdown_link_text_checker_passes_on_current_repo() {
    let (exit_code, output) = run_checker_in_repo(&[]);

    assert_eq!(
        exit_code, 0,
        "Repository markdown link text checker should pass on current repo.\nOutput:\n{output}"
    );
    assert!(
        !output.contains("[ERROR]"),
        "Repository markdown link text checker reported errors unexpectedly.\nOutput:\n{output}"
    );
}
