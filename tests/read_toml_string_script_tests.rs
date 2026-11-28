#![cfg(test)]

mod common;

use common::{bash_command, repo_root, unique_temp_dir, write_file};
use std::fs;

fn copy_reader_script(temp_root: &std::path::Path) {
    for name in ["read-toml-string.sh", "read_toml_string.py"] {
        let source = repo_root().join("scripts").join(name);
        let destination = temp_root.join("scripts").join(name);
        let contents = fs::read_to_string(&source)
            .unwrap_or_else(|e| panic!("Failed to read {}: {e}", source.display()));
        write_file(&destination, &contents);
    }
}

fn run_reader_bytes(args: &[&str], toml: &[u8]) -> (i32, String) {
    let temp_root = unique_temp_dir("read-toml-string");
    copy_reader_script(temp_root.path());
    fs::write(temp_root.path().join("sample.toml"), toml).expect("write sample TOML bytes");

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

fn run_reader(args: &[&str], toml: &str) -> (i32, String) {
    run_reader_bytes(args, toml.as_bytes())
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
fn test_read_toml_string_decodes_escapes_and_scalars() {
    let toml = r#"
description = "a \"quoted\" value"
path = "C:\\temp"
version = 1
enabled = true
"#;

    for (args, expected, name) in [
        (
            ["sample.toml", "description"],
            "a \"quoted\" value\n",
            "escaped quotes",
        ),
        (["sample.toml", "path"], "C:\\temp\n", "escaped backslash"),
        (["sample.toml", "version"], "1\n", "integer scalar"),
        (["sample.toml", "enabled"], "true\n", "boolean scalar"),
    ] {
        let (exit_code, output) = run_reader(&args, toml);

        assert_eq!(
            exit_code, 0,
            "case {name} should read the TOML value.\nOutput:\n{output}"
        );
        assert_eq!(
            output, expected,
            "case {name} should return the decoded value"
        );
    }
}

#[test]
fn test_read_toml_string_fails_closed_for_invalid_toml() {
    let (exit_code, output) = run_reader(
        &[
            "sample.toml",
            "built-in-tls",
            "package.metadata.signal-fish-release",
        ],
        "[package.metadata.signal-fish-release]\nbuilt-in-tls = maybe\n",
    );

    assert_eq!(
        exit_code, 3,
        "invalid TOML must not look like an absent key.\nOutput:\n{output}"
    );
    assert!(
        output.contains("Could not read TOML file"),
        "invalid TOML should name the parse failure.\nOutput:\n{output}"
    );
}

#[test]
fn test_read_toml_string_fails_closed_for_invalid_utf8() {
    let (exit_code, output) = run_reader_bytes(
        &["sample.toml", "version", "package"],
        b"[package]\n# invalid byte: \xff\nversion = \"0.1.0\"\n",
    );

    assert_eq!(
        exit_code, 3,
        "invalid UTF-8 must not look like an absent key.\nOutput:\n{output}"
    );
    assert!(
        output.contains("Could not read TOML file"),
        "invalid UTF-8 should name the read failure.\nOutput:\n{output}"
    );
}

#[test]
fn test_read_toml_string_reads_array_of_table_sections() {
    let (exit_code, output) = run_reader(
        &["sample.toml", "version", "bin"],
        "[[bin]]\nversion = \"0.1.0\"\n\n[[bin]]\nversion = \"0.2.0\"\n",
    );

    assert_eq!(
        exit_code, 0,
        "an array-of-tables section must keep its first table readable.\nOutput:\n{output}"
    );
    assert_eq!(output, "0.1.0\n");
}

#[test]
fn test_read_toml_string_reads_nested_sections_inside_array_tables() {
    let (exit_code, output) = run_reader(
        &["sample.toml", "version", "bin.metadata"],
        "[[bin]]\nname = \"first\"\n\n[bin.metadata]\nversion = \"0.1\"\n",
    );

    assert_eq!(
        exit_code, 0,
        "nested sections inside an array-of-tables entry must stay readable.\nOutput:\n{output}"
    );
    assert_eq!(output, "0.1\n");
}

#[test]
fn test_read_toml_string_ignores_headers_inside_multiline_strings() {
    let toml = "items = { version = \"wrong\" }\n\ndescription = \"\"\"\n[items]\n\"\"\"\n";
    let (exit_code, output) = run_reader(&["sample.toml", "version", "items"], toml);

    assert_eq!(
        exit_code, 1,
        "a header-shaped line inside a multiline string is not a section.\nOutput:\n{output}"
    );
    assert!(
        output.is_empty(),
        "a multiline string must not declare a section.\nOutput:\n{output}"
    );
}

#[test]
fn test_read_toml_string_reads_quoted_dotted_section_paths() {
    let (exit_code, output) = run_reader(
        &["sample.toml", "value", "parent.\"child.name\""],
        "[parent.\"child.name\"]\nvalue = \"ok\"\n",
    );

    assert_eq!(
        exit_code, 0,
        "periods inside quoted section keys must stay in the key.\nOutput:\n{output}"
    );
    assert_eq!(output, "ok\n");
}

#[test]
fn test_read_toml_string_ignores_inline_table_arrays() {
    let (exit_code, output) = run_reader(
        &["sample.toml", "version", "items"],
        "items = [{ version = \"wrong\" }]\n",
    );

    assert_eq!(
        exit_code, 1,
        "an inline-table array is a value, not a table section.\nOutput:\n{output}"
    );
    assert!(
        output.is_empty(),
        "an inline-table array must not return a section value.\nOutput:\n{output}"
    );
}

#[test]
fn test_read_toml_string_reads_direct_keys_from_later_array_tables() {
    let (exit_code, output) = run_reader(
        &["sample.toml", "version", "bin"],
        "[[bin]]\nname = \"a\"\n\n[[bin]]\nversion = \"0.2\"\n",
    );

    assert_eq!(
        exit_code, 0,
        "a direct key may belong to a later array-of-tables entry.\nOutput:\n{output}"
    );
    assert_eq!(output, "0.2\n");
}

#[test]
fn test_read_toml_string_reads_nested_sections_in_later_array_tables() {
    let (exit_code, output) = run_reader(
        &["sample.toml", "version", "bin.metadata"],
        "[[bin]]\nname = \"first\"\n\n[[bin]]\nname = \"second\"\n\n[bin.metadata]\nversion = \"0.2\"\n",
    );

    assert_eq!(
        exit_code, 0,
        "a nested section may belong to a later array-of-tables entry.\nOutput:\n{output}"
    );
    assert_eq!(output, "0.2\n");
}

#[test]
fn test_read_toml_string_reads_dotted_section_paths() {
    let (exit_code, output) = run_reader(
        &[
            "sample.toml",
            "prepared-from",
            "package.metadata.signal-fish-release",
        ],
        "[package.metadata.signal-fish-release]\nprepared-from = \"abc\"\n",
    );

    assert_eq!(
        exit_code, 0,
        "dotted section paths must resolve nested tables.\nOutput:\n{output}"
    );
    assert_eq!(output, "abc\n");
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
