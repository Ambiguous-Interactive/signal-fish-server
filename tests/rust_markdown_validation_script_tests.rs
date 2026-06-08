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

#[test]
fn test_rust_markdown_validator_compiles_blocks_after_leading_blank_lines() {
    let (success, output) = run_validator(
        r#"# Rust Markdown Samples

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
    );

    assert!(
        success,
        "Validator should accept complete Rust items after leading blank lines.\nOutput:\n{output}"
    );
    assert!(
        output.contains("Total blocks: 5")
            && output.contains("Validated: 3")
            && output.contains("Skipped: 2")
            && output.contains("Failed: 0"),
        "Unexpected validation summary for leading-blank fixture.\nOutput:\n{output}"
    );
    assert!(
        output.contains("partial snippet, no item-level keywords"),
        "Expression-only block should still be classified as a partial snippet.\nOutput:\n{output}"
    );
}

#[test]
fn test_rust_markdown_validator_does_not_skip_invalid_item_after_leading_blank_line() {
    let (success, output) = run_validator(
        r#"# Invalid Rust Markdown Sample

```rust

fn leading_blank_invalid() {
    let =
}
```
"#,
    );

    assert!(
        !success,
        "Validator must fail invalid Rust items after leading blank lines, not skip them.\nOutput:\n{output}"
    );
    assert!(
        output.contains("FAILED: Block at line") && output.contains("Failed: 1"),
        "Expected a compilation failure diagnostic for invalid leading-blank item.\nOutput:\n{output}"
    );
    assert!(
        !output.contains("partial snippet, no item-level keywords"),
        "Invalid item was misclassified as a partial snippet.\nOutput:\n{output}"
    );
}

#[test]
fn test_rust_markdown_validator_external_context_warning_does_not_mask_syntax_errors() {
    let (success, output) = run_validator(
        r#"# Mixed External Context And Syntax Error

```rust

use missing_crate::MissingType;

fn leading_blank_invalid(_: MissingType) {
    let =
}
```
"#,
    );

    assert!(
        !success,
        "Validator must fail blocks that mix missing external context with syntax errors.\nOutput:\n{output}"
    );
    assert!(
        output.contains("FAILED: Block at line")
            && output.contains("Failed: 1")
            && !output.contains("requires external context"),
        "Expected syntax failure instead of external-context downgrade.\nOutput:\n{output}"
    );
}

#[test]
fn test_rust_markdown_validator_does_not_skip_item_blocks_with_placeholder_markers() {
    let (success, output) = run_validator(
        r#"# Placeholder Marker Inside Item Block

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
    );

    assert!(
        !success,
        "Item-level Rust blocks containing placeholder-looking tokens must compile or fail, not skip.\nOutput:\n{output}"
    );
    assert!(
        output.contains("FAILED: Block at line")
            && output.contains("Failed: 1")
            && !output.contains("incomplete/placeholder code"),
        "Expected a compilation failure instead of a placeholder skip.\nOutput:\n{output}"
    );
}

#[test]
fn test_rust_markdown_validator_does_not_skip_item_blocks_with_documentation_markers() {
    let (success, output) = run_validator(
        r#"# Documentation Marker Inside Item Block

```rust
fn documentation_marker_inside_item() {
    // Example: comments inside real item blocks are not skip directives.
    let =
}
```
"#,
    );

    assert!(
        !success,
        "Item-level Rust blocks containing documentation comments must compile or fail, not skip.\nOutput:\n{output}"
    );
    assert!(
        output.contains("FAILED: Block at line")
            && output.contains("Failed: 1")
            && !output.contains("documentation snippet"),
        "Expected a compilation failure instead of a documentation snippet skip.\nOutput:\n{output}"
    );
}

#[test]
fn test_rust_markdown_validator_still_warns_for_pure_external_context() {
    let (success, output) = run_validator(
        r#"# External Context Sample

```rust

use missing_crate::MissingType;

pub fn needs_external_context(_: MissingType) {}
```
"#,
    );

    assert!(
        success,
        "Pure external-context failures should remain informational warnings.\nOutput:\n{output}"
    );
    assert!(
        output.contains("requires external context")
            && output.contains("Warned: 1")
            && output.contains("Failed: 0"),
        "Expected an external-context warning, not a hard failure.\nOutput:\n{output}"
    );
}

#[test]
fn test_rust_markdown_validator_compiles_rustdoc_style_statement_blocks() {
    let (success, output) = run_validator(
        r#"# Rustdoc-Style Snippet

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
    );

    assert!(
        success,
        "Validator should compile Rustdoc-style snippets with top-level statements via a wrapper.\nOutput:\n{output}"
    );
    assert!(
        output.contains("wrapped Rustdoc-style snippet")
            && output.contains("Validated: 1")
            && output.contains("Failed: 0"),
        "Expected the top-level statement block to validate through the wrapper path.\nOutput:\n{output}"
    );
}

#[test]
fn test_rust_markdown_validator_closes_fences_with_trailing_whitespace() {
    let (success, output) = run_validator(
        "# Trailing Space Closing Fence\n\n```rust\n\nfn trailing_space_close() {\n    let =\n}\n```   \n",
    );

    assert!(
        !success,
        "Validator must extract and fail Rust blocks whose closing fence has trailing spaces.\nOutput:\n{output}"
    );
    assert!(
        output.contains("Total blocks: 1")
            && output.contains("FAILED: Block at line")
            && output.contains("Failed: 1"),
        "Expected one extracted block and one compilation failure.\nOutput:\n{output}"
    );
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
fn test_rust_markdown_validator_ignores_literal_rust_fences_inside_longer_fences() {
    let (success, output) = run_validator(
        r#"# Literal Rust Fence

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
    );

    assert!(
        success,
        "Literal Rust fences inside a longer non-Rust fence should not be extracted.\nOutput:\n{output}"
    );
    assert!(
        output.contains("Total blocks: 1")
            && output.contains("Validated: 1")
            && output.contains("Failed: 0"),
        "Expected only the real Rust block outside the literal fence to be validated.\nOutput:\n{output}"
    );
}

#[test]
fn test_rust_markdown_validator_does_not_close_long_rust_fence_on_shorter_fence() {
    let (success, output) = run_validator(
        r#"# Longer Rust Fence

````rust
fn before_shorter_fence() {}
```
fn invalid_after_shorter_fence() {
    let =
}
````
"#,
    );

    assert!(
        !success,
        "A Rust block opened with four backticks must not close on a three-backtick fence.\nOutput:\n{output}"
    );
    assert!(
        output.contains("Total blocks: 1")
            && output.contains("FAILED: Block at line")
            && output.contains("Failed: 1"),
        "Expected one extracted long-fence block and a compilation failure.\nOutput:\n{output}"
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
