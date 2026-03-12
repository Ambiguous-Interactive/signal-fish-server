#![cfg(test)]

mod common;

use common::{bash_command, repo_root, unique_temp_dir, write_file};
use std::fs;
use std::path::{Path, PathBuf};

fn make_lines(count: usize) -> String {
    let mut content = String::new();
    for i in 1..=count {
        content.push_str(&format!("line-{i}\n"));
    }
    content
}

fn collect_markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("Failed to read directory {}: {e}", dir.display()));

    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("Failed to read directory entry: {e}"));
        let path = entry.path();
        let file_type = entry
            .file_type()
            .unwrap_or_else(|e| panic!("Failed to read file type for {}: {e}", path.display()));

        if file_type.is_dir() {
            collect_markdown_files(&path, out);
            continue;
        }

        if file_type.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
}

fn format_top_line_counts(paths: &[PathBuf], limit: usize) -> String {
    let mut counts = paths
        .iter()
        .map(|path| {
            let content = fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
            let line_count = content.lines().count();
            let relative = path
                .strip_prefix(repo_root())
                .unwrap_or_else(|e| {
                    panic!("Failed to create relative path for {}: {e}", path.display())
                })
                .display()
                .to_string();
            (line_count, relative)
        })
        .collect::<Vec<_>>();

    counts.sort_by(|left, right| right.cmp(left));

    counts
        .into_iter()
        .take(limit)
        .map(|(line_count, relative)| format!("{relative}: {line_count}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn run_checker_with_fixture(files: &[(&str, String)], args: &[&str]) -> (i32, String) {
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
    command.arg("scripts/check-llm-file-sizes.sh");
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
    files: Vec<(&'static str, String)>,
    args: Vec<&'static str>,
    expected_exit: i32,
    must_contain: Vec<&'static str>,
}

#[test]
fn test_llm_file_size_checker_data_driven_cases() {
    let cases = vec![
        ScriptCase {
            name: "passes_when_all_files_within_limit",
            files: vec![(".llm/skills/ok.md", make_lines(42))],
            args: vec![],
            expected_exit: 0,
            must_contain: vec![
                "[INFO] Checked 1 file(s) in .llm/",
                "[OK] All 1 LLM file(s) are within the 300-line limit.",
            ],
        },
        ScriptCase {
            name: "warns_when_file_hits_limit",
            files: vec![(".llm/skills/near-limit.md", make_lines(300))],
            args: vec![],
            expected_exit: 0,
            must_contain: vec![
                "[WARN] .llm/skills/near-limit.md: 300 lines (at limit — next added line will fail)",
                "[WARN] LLM file size check passed with 1 warning(s)",
            ],
        },
        ScriptCase {
            name: "fails_when_file_exceeds_limit",
            files: vec![(".llm/skills/too-long.md", make_lines(301))],
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
                (".llm/skills/index.md", make_lines(800)),
                (".llm/context.md", make_lines(5)),
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
                (".llm/skills/index.md", make_lines(800)),
                (".llm/skills/ok.md", make_lines(5)),
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
            name: "files_mode_warns_for_missing_paths",
            files: vec![(".llm/context.md", make_lines(8))],
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
            files: vec![(".llm/skills/space file.md", make_lines(12))],
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
fn test_repository_llm_file_size_checker_script_passes_on_current_repo() {
    let (exit_code, output) = run_checker_in_repo(&[]);

    assert_eq!(
        exit_code, 0,
        "Repository LLM size checker should pass on the current repo.\nOutput:\n{output}"
    );
    assert!(
        !output.contains("[ERROR]"),
        "Repository LLM size checker reported errors unexpectedly.\nOutput:\n{output}"
    );
}

#[test]
fn test_repository_llm_markdown_files_respect_line_limit() {
    const MAX_LINES: usize = 300;
    let llm_root = repo_root().join(".llm");
    assert!(
        llm_root.exists(),
        "Expected .llm/ directory to exist at {}",
        llm_root.display()
    );

    let mut files = Vec::new();
    collect_markdown_files(&llm_root, &mut files);
    files.sort();
    let checked_files = files
        .iter()
        .filter(|path| path.file_name().is_none_or(|name| name != "index.md"))
        .cloned()
        .collect::<Vec<_>>();

    let mut violations = Vec::new();
    let mut near_limit = Vec::new();

    for file in &checked_files {
        let content = fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("Failed to read {}: {e}", file.display()));
        let line_count = content.lines().count();
        let relative = file
            .strip_prefix(repo_root())
            .unwrap_or_else(|e| {
                panic!("Failed to create relative path for {}: {e}", file.display())
            })
            .display()
            .to_string();

        if line_count > MAX_LINES {
            violations.push(format!(
                "{relative}: {line_count} lines (exceeds by {})",
                line_count - MAX_LINES
            ));
        } else if line_count >= (MAX_LINES - 5) {
            near_limit.push(format!("{relative}: {line_count}/{MAX_LINES}"));
        }
    }

    assert!(
        violations.is_empty(),
        "LLM markdown files must stay within {MAX_LINES} lines (index.md excluded).\n\
         Violations:\n{}\n\
         Near-limit files (>=295 lines):\n{}",
        violations.join("\n"),
        if near_limit.is_empty() {
            format!(
                "(none)\nTop file sizes:\n{}",
                format_top_line_counts(&checked_files, 10)
            )
        } else {
            format!(
                "{}\nTop file sizes:\n{}",
                near_limit.join("\n"),
                format_top_line_counts(&checked_files, 10)
            )
        }
    );
}
