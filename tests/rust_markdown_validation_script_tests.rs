#![cfg(test)]

mod common;

use common::{bash_command, repo_root, unique_temp_dir, write_file};
use std::fs;
use std::path::Path;
use std::process::Command;

fn copy_validator_fixture(temp_root: &std::path::Path) {
    let root = repo_root();

    let scripts = [
        ".github/scripts/extract-rust-blocks.awk",
        ".github/scripts/validate-rust-markdown-blocks.sh",
    ];

    for script in scripts {
        let source = root.join(script);
        let destination = temp_root.join(script);
        let content = fs::read_to_string(&source)
            .unwrap_or_else(|error| panic!("Failed to read {}: {error}", source.display()));
        write_file(&destination, &content);
    }
}

fn run_validator_command(temp_root: &Path, args: &[&str]) -> (bool, String) {
    let output = bash_command()
        .arg(".github/scripts/validate-rust-markdown-blocks.sh")
        .args(args)
        .current_dir(temp_root)
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "Failed to run Rust markdown validator in {}: {error}",
                temp_root.display()
            )
        });

    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));

    (output.status.success(), combined.replace("\r\n", "\n"))
}

fn run_validator(markdown: &str) -> (bool, String) {
    let temp_root = unique_temp_dir("rust-markdown-validation");
    copy_validator_fixture(temp_root.path());

    let fixture_path = temp_root.path().join("samples.md");
    write_file(&fixture_path, markdown);

    run_validator_command(temp_root.path(), &["samples.md"])
}

fn git_command(temp_root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(temp_root)
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "Failed to run git {:?} in {}: {error}",
                args,
                temp_root.display()
            )
        });

    if !output.status.success() {
        let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
        panic!(
            "git {:?} failed in {}\nOutput:\n{}",
            args,
            temp_root.display(),
            combined
        );
    }
}

fn valid_rust_block(name: &str) -> String {
    format!("```rust\nfn {name}() {{}}\n```\n")
}

fn invalid_rust_block() -> &'static str {
    "```rust\nfn invalid() {\n    let =\n}\n```\n"
}

fn assert_only_repository_markdown_was_validated(output: &str, mode: &str) {
    assert!(
        output.contains("Processing: ./tracked.md")
            && output.contains("Processing: ./untracked.md")
            && output.contains("Total blocks: 2")
            && output.contains("Validated: 2")
            && output.contains("Failed: 0"),
        "Unexpected {mode} discovery summary.\nOutput:\n{output}"
    );

    for excluded in [
        "ignored.md",
        ".github/test-fixtures/invalid.md",
        "target/generated.md",
        "node_modules/package/invalid.md",
    ] {
        assert!(
            !output.contains(excluded),
            "{mode} discovery should not process excluded markdown path {excluded}.\nOutput:\n{output}"
        );
    }
}

/// Behavioral classification cases for the Rust markdown validator, driven by a
/// single table so adding a scenario is a few data lines rather than another
/// copy-pasted `#[test]`. That copy-paste pattern is exactly what previously let
/// a sibling test file drift into a corrupted, non-compiling state, so keeping
/// the repetitive cases as data (not duplicated control flow) is a deliberate
/// fragility reduction. Each case feeds one markdown fixture to
/// `validate-rust-markdown-blocks.sh` and asserts on the run's success plus
/// required and forbidden output fragments.
///
/// `git`-backed discovery scenarios stay as dedicated tests below: they need
/// repository setup rather than a single inline fixture, so a table would only
/// obscure them.
#[test]
fn test_rust_markdown_validator_classification_cases() {
    struct ValidatorCase {
        name: &'static str,
        markdown: &'static str,
        expect_success: bool,
        expected_substrings: &'static [&'static str],
        forbidden_substrings: &'static [&'static str],
    }

    let cases = [
        ValidatorCase {
            name: "compiles_blocks_after_leading_blank_lines",
            markdown: r#"# Rust Markdown Samples

```rust

fn leading_blank_fn() {}
```

```rust

use std::fmt;
```

```rust

#[derive(Debug)]
struct LeadingBlankStruct;
```

```rust

let expression_only = 1;
```

```rust

```
"#,
            expect_success: true,
            expected_substrings: &[
                "Total blocks: 5",
                "Validated: 3",
                "Skipped: 2",
                "Failed: 0",
                "partial snippet, no item-level keywords",
            ],
            forbidden_substrings: &[],
        },
        ValidatorCase {
            name: "does_not_skip_invalid_item_after_leading_blank_line",
            markdown: r#"# Invalid Rust Markdown Sample

```rust

fn leading_blank_invalid() {
    let =
}
```
"#,
            expect_success: false,
            expected_substrings: &["FAILED: Block at line", "Failed: 1"],
            forbidden_substrings: &["partial snippet, no item-level keywords"],
        },
        ValidatorCase {
            name: "external_context_warning_does_not_mask_syntax_errors",
            markdown: r#"# Mixed External Context And Syntax Error

```rust

use missing_crate::MissingType;

fn leading_blank_invalid(_: MissingType) {
    let =
}
```
"#,
            expect_success: false,
            expected_substrings: &["FAILED: Block at line", "Failed: 1"],
            forbidden_substrings: &["requires external context"],
        },
        ValidatorCase {
            name: "does_not_skip_item_blocks_with_placeholder_markers",
            markdown: r#"# Placeholder Marker Inside Item Block

```rust
#[derive(Default)]
struct Config {
    value: u8,
}

fn placeholder_marker_inside_item() {
    let _config = Config {
        .. Default::default()
    };
    let =
}
```
"#,
            expect_success: false,
            expected_substrings: &["FAILED: Block at line", "Failed: 1"],
            forbidden_substrings: &["incomplete/placeholder code"],
        },
        ValidatorCase {
            name: "does_not_skip_item_blocks_with_documentation_markers",
            markdown: r#"# Documentation Marker Inside Item Block

```rust
fn documentation_marker_inside_item() {
    // Example: comments inside real item blocks are not skip directives.
    let =
}
```
"#,
            expect_success: false,
            expected_substrings: &["FAILED: Block at line", "Failed: 1"],
            forbidden_substrings: &["documentation snippet"],
        },
        ValidatorCase {
            name: "still_warns_for_pure_external_context",
            markdown: r#"# External Context Sample

```rust

use missing_crate::MissingType;

pub fn needs_external_context(_: MissingType) {}
```
"#,
            expect_success: true,
            expected_substrings: &[
                "requires external context",
                "Warned: 1",
                "Failed: 0",
            ],
            forbidden_substrings: &[],
        },
        ValidatorCase {
            name: "compiles_rustdoc_style_statement_blocks",
            markdown: r#"# Rustdoc-Style Snippet

```rust
use std::fmt;

#[derive(Debug)]
struct Move {
    x: i32,
}

let movement = Move { x: 42 };
let rendered = format!("{movement:?}");
assert!(rendered.contains("42"));
```
"#,
            expect_success: true,
            expected_substrings: &[
                "wrapped Rustdoc-style snippet",
                "Validated: 1",
                "Failed: 0",
            ],
            forbidden_substrings: &[],
        },
        ValidatorCase {
            name: "closes_fences_with_trailing_whitespace",
            markdown: "# Trailing Space Closing Fence\n\n```rust\n\nfn trailing_space_close() {\n    let =\n}\n```   \n",
            expect_success: false,
            expected_substrings: &[
                "Total blocks: 1",
                "FAILED: Block at line",
                "Failed: 1",
            ],
            forbidden_substrings: &[],
        },
        ValidatorCase {
            name: "ignores_literal_rust_fences_inside_longer_fences",
            markdown: r#"# Literal Rust Fence

````text
```rust
fn literal_invalid_rust() {
    let =
}
```
````

```rust
fn real_rust_block() {}
```
"#,
            expect_success: true,
            expected_substrings: &["Total blocks: 1", "Validated: 1", "Failed: 0"],
            forbidden_substrings: &[],
        },
        ValidatorCase {
            name: "does_not_close_long_rust_fence_on_shorter_fence",
            markdown: r#"# Longer Rust Fence

````rust
fn before_shorter_fence() {}
```
fn invalid_after_shorter_fence() {
    let =
}
````
"#,
            expect_success: false,
            expected_substrings: &[
                "Total blocks: 1",
                "FAILED: Block at line",
                "Failed: 1",
            ],
            forbidden_substrings: &[],
        },
    ];

    for case in cases {
        let (success, output) = run_validator(case.markdown);

        assert_eq!(
            success, case.expect_success,
            "Case '{}' run success mismatch (expected success = {}).\nOutput:\n{}",
            case.name, case.expect_success, output
        );

        for expected in case.expected_substrings {
            assert!(
                output.contains(expected),
                "Case '{}' expected output to contain '{}'.\nOutput:\n{}",
                case.name,
                expected,
                output
            );
        }

        for forbidden in case.forbidden_substrings {
            assert!(
                !output.contains(forbidden),
                "Case '{}' expected output to NOT contain '{}'.\nOutput:\n{}",
                case.name,
                forbidden,
                output
            );
        }
    }
}

#[test]
fn test_rust_markdown_validator_validates_user_facing_docs() {
    let temp_root = unique_temp_dir("rust-markdown-validation-docs");
    copy_validator_fixture(temp_root.path());

    git_command(temp_root.path(), &["init"]);
    write_file(
        &temp_root.path().join("docs/bad.md"),
        r#"# User-Facing Doc

```rust
fn invalid_user_doc() {
    let =
}
```
"#,
    );
    git_command(temp_root.path(), &["add", "docs/bad.md"]);

    let (success, output) = run_validator_command(temp_root.path(), &[]);
    assert!(
        !success,
        "Default discovery must validate user-facing docs instead of blanket-skipping docs/.\nOutput:\n{output}"
    );
    assert!(
        output.contains("Processing: ./docs/bad.md")
            && output.contains("FAILED: Block at line")
            && output.contains("Failed: 1")
            && !output.contains("reference documentation file"),
        "Expected docs/ Rust block to fail compilation, not be treated as reference docs.\nOutput:\n{output}"
    );
}

#[test]
fn test_rust_markdown_validator_discovery_excludes_ignored_and_generated_markdown() {
    let temp_root = unique_temp_dir("rust-markdown-validation-discovery");
    copy_validator_fixture(temp_root.path());

    git_command(temp_root.path(), &["init"]);

    write_file(&temp_root.path().join(".gitignore"), "ignored.md\n");
    write_file(
        &temp_root.path().join("tracked.md"),
        &valid_rust_block("tracked"),
    );
    write_file(
        &temp_root.path().join("untracked.md"),
        &valid_rust_block("untracked"),
    );
    write_file(&temp_root.path().join("ignored.md"), invalid_rust_block());
    write_file(
        &temp_root.path().join(".github/test-fixtures/invalid.md"),
        invalid_rust_block(),
    );
    write_file(
        &temp_root.path().join("target/generated.md"),
        invalid_rust_block(),
    );
    write_file(
        &temp_root.path().join("node_modules/package/invalid.md"),
        invalid_rust_block(),
    );

    git_command(temp_root.path(), &["add", ".gitignore", "tracked.md"]);

    let (success, output) = run_validator_command(temp_root.path(), &[]);
    assert!(
        success,
        "Default discovery should validate only non-ignored repository markdown.\nOutput:\n{output}"
    );
    assert_only_repository_markdown_was_validated(&output, "default");

    let (success, output) = run_validator_command(temp_root.path(), &["."]);
    assert!(
        success,
        "Directory discovery should apply the same generated and ignored path exclusions.\nOutput:\n{output}"
    );
    assert_only_repository_markdown_was_validated(&output, "directory");
}
