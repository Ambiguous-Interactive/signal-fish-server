#![cfg(test)]

mod common;

use common::{bash_command, repo_root, unique_temp_dir, write_file};
use std::fs;

fn copy_reader_script(temp_root: &std::path::Path) {
    let script_src = repo_root().join("scripts/read-toml-string.sh");
    let script_dst = temp_root.join("scripts/read-toml-string.sh");
    let script = fs::read_to_string(&script_src)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", script_src.display()));
    write_file(&script_dst, &script);
}

fn run_reader(args: &[&str], toml: &str) -> (i32, String) {
    let temp_root = unique_temp_dir("read-toml-string");
    copy_reader_script(temp_root.path());
    write_file(&temp_root.path().join("sample.toml"), toml);

    let output = bash_command()
        .arg("scripts/read-toml-string.sh")
        .args(args)
        .current_dir(temp_root.path())
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "Failed to run read-toml-string.sh in {}: {e}",
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

#[test]
fn test_read_toml_string_accepts_whitespace_comments_and_quote_variants() {
    let toml = r#"
channel	=	"1.88.0"

[package] # package metadata
rust-version='1.88.0'
version = "0.2.0" # package version

[tool]
version = 'wrong-section'
"#;

    let cases = [
        (
            ["sample.toml", "rust-version", "package"],
            "1.88.0\n",
            "single-quoted package rust-version",
        ),
        (
            ["sample.toml", "version", "package"],
            "0.2.0\n",
            "double-quoted package version with trailing comment",
        ),
        (
            ["sample.toml", "channel", ""],
            "1.88.0\n",
            "tab-spaced root channel",
        ),
    ];

    for (args, expected, name) in cases {
        let filtered_args: Vec<&str> = args.into_iter().filter(|arg| !arg.is_empty()).collect();
        let (exit_code, output) = run_reader(&filtered_args, toml);

        assert_eq!(
            exit_code, 0,
            "case {name} should read the TOML value.\nOutput:\n{output}"
        );
        assert_eq!(
            output, expected,
            "case {name} should return only the parsed value"
        );
    }
}

#[test]
fn test_read_toml_string_fails_closed_for_missing_section_key() {
    let (exit_code, output) = run_reader(
        &["sample.toml", "version", "missing"],
        "[package]\nversion = \"0.2.0\"\n",
    );

    assert_eq!(
        exit_code, 1,
        "missing section/key should fail closed.\nOutput:\n{output}"
    );
    assert!(
        output.is_empty(),
        "missing section/key should not emit a misleading value.\nOutput:\n{output}"
    );
}

#[test]
fn test_read_toml_string_array_of_tables_ends_previous_section() {
    let (exit_code, output) = run_reader(
        &["sample.toml", "version", "package"],
        r#"
[package]
name = "signal-fish-server"

[[bin]]
version = "wrong-section"
"#,
    );

    assert_eq!(
        exit_code, 1,
        "array-of-tables headers should end the previous section.\nOutput:\n{output}"
    );
    assert!(
        output.is_empty(),
        "reader must not return values from [[bin]] while scoped to [package].\nOutput:\n{output}"
    );
}
