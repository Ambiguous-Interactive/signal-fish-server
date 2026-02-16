# Markdown Code Validation Test Cases

This file contains comprehensive test cases for the markdown code validation workflow.
Each test case is designed to catch specific bugs that were fixed in the validation logic.

## Test Case 1: Multi-line Rust Code Block (Basic)

```rust
fn hello_world() {
    println!("Hello, world!");
}
```

**Expected behavior:** Should validate and compile successfully.
**Tests:** Basic multi-line Rust code block extraction and validation.

---

## Test Case 2: Code Block with Empty First Line

```rust

fn empty_first_line() {
    println!("This block has an empty first line");
}
```

**Expected behavior:** Should validate and compile successfully.
**Tests:** Bug fix #1 - Content accumulation with empty first lines.
**Bug context:** AWK script was incorrectly handling empty first lines, causing content to be lost.

---

## Test Case 3: Code Block with Case Variation `rust`

```rust
fn lowercase_rust() {
    let x = 42;
    println!("Value: {}", x);
}
```

**Expected behavior:** Should validate and compile successfully.
**Tests:** Case-insensitive regex matching for `rust` fence.

---

## Test Case 4: Code Block with Case Variation `Rust`

```Rust
fn uppercase_rust() {
    let y = 100;
    println!("Another value: {}", y);
}
```

**Expected behavior:** Should validate and compile successfully.
**Tests:** Bug fix #3 - Case-insensitive regex for Rust/rust.
**Bug context:** AWK script was not matching uppercase `Rust` fence markers.

---

## Test Case 5: Code Block with `ignore` Attribute

```rust,ignore
fn this_should_be_skipped() {
    // This won't compile but should be skipped
    this_is_invalid_syntax
}
```

**Expected behavior:** Should be skipped (not validated).
**Tests:** Proper handling of `ignore` attribute.

---

## Test Case 6: Code Block with `no_run` Attribute

```rust,no_run
fn main() {
    println!("This compiles but doesn't run");
    std::process::exit(1);
}
```

**Expected behavior:** Should compile but not run.
**Tests:** Proper handling of `no_run` attribute.

---

## Test Case 7: Code Block with `should_panic` Attribute

```rust,should_panic
fn this_panics() {
    panic!("Expected panic!");
}
```

**Expected behavior:** Should be skipped (not validated).
**Tests:** Proper handling of `should_panic` attribute.

---

## Test Case 8: Empty Code Block

```rust
```

**Expected behavior:** Should be skipped with message "Skipping empty block".
**Tests:** Empty block detection and skipping.

---

## Test Case 9: Code Block with Only Whitespace

```rust

```

**Expected behavior:** Should be skipped as empty.
**Tests:** Whitespace-only block detection.

---

## Test Case 10: Code Block with Placeholder `todo!()`

```rust
fn incomplete_function() {
    todo!()
}
```

**Expected behavior:** Should be skipped as placeholder code.
**Tests:** Detection of `todo!()` macro.

---

## Test Case 11: Code Block with Ellipsis Placeholder

```rust
fn example() {
    // ...
    /* ... */
}
```

**Expected behavior:** Should be skipped as placeholder code.
**Tests:** Detection of ellipsis placeholders.

---

## Test Case 12: Code Block with Documentation Snippet Marker

```rust
// Note: This is just an example
fn example_config() {
    /* config */
}
```

**Expected behavior:** Should be skipped as documentation snippet.
**Tests:** Detection of documentation markers.

---

## Test Case 13: Multiple Consecutive Rust Blocks

```rust
fn first_function() {
    println!("First");
}
```

```rust
fn second_function() {
    println!("Second");
}
```

**Expected behavior:** Both blocks should be validated separately.
**Tests:** Multiple block handling with independent validation.

---

## Test Case 14: Unclosed Code Block at EOF (No trailing backticks)

**Note:** This test case is tricky to represent in a valid markdown file.
The workflow's AWK END block handles this case.

**Expected behavior:** Should still extract and validate the content.
**Tests:** Bug fix #2 - END block to handle unclosed blocks at EOF.
**Bug context:** AWK script would lose content if a code block wasn't closed before EOF.

---

## Test Case 15: Nested/Malformed Blocks

```rust
fn outer() {
    println!("Outer function");
}
```
// No closing fence here, next block starts
```rust
fn recovery() {
    println!("This should still be extracted");
}
```

**Expected behavior:** The recovery block should be extracted and validated.
**Tests:** Resilience to malformed/nested blocks.

---

## Test Case 16: Complex Multi-line Block with Various Syntax

```rust
use std::collections::HashMap;

struct TestStruct {
    data: HashMap<String, i32>,
}

impl TestStruct {
    fn new() -> Self {
        Self {
            data: HashMap::new(),
        }
    }

    fn insert(&mut self, key: String, value: i32) {
        self.data.insert(key, value);
    }
}

fn main() {
    let mut test = TestStruct::new();
    test.insert("answer".to_string(), 42);
}
```

**Expected behavior:** Should validate and compile successfully.
**Tests:** Complex code with imports, structs, impl blocks, and main function.

---

## Test Case 17: Code Block with External Dependencies

```rust
use signal_fish_server::config::Config;

fn load_config() -> Config {
    Config::default()
}
```

**Expected behavior:** Should pass syntax validation but skip compilation (missing dependencies).
**Tests:** Handling of code requiring external crates.

---

## Test Case 18: JSON Code Block

```json
{
  "name": "test",
  "value": 42,
  "nested": {
    "array": [1, 2, 3]
  }
}
```

**Expected behavior:** Should validate as valid JSON.
**Tests:** JSON validation workflow.

---

## Test Case 19: Invalid JSON Code Block

This should cause validation to fail if uncommented:

<!--
```json
{
  "invalid": "missing closing brace"
```
-->

**Expected behavior:** Would fail JSON validation (currently commented out).
**Tests:** JSON validation detects syntax errors.

---

## Test Case 20: YAML Code Block

```yaml
name: test-workflow
on:
  push:
    branches: [main]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@v4
```

**Expected behavior:** Should validate as valid YAML.
**Tests:** YAML validation workflow with file-based counters.

---

## Test Case 21: TOML Code Block

```toml
[package]
name = "signal-fish-server"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1.0", features = ["full"] }
```

**Expected behavior:** Should validate as valid TOML.
**Tests:** TOML validation workflow with file-based counters.

---

## Test Case 22: Bash Code Block

```bash
#!/bin/bash
set -euo pipefail

echo "Hello, World!"
for i in {1..5}; do
  echo "Count: $i"
done
```

**Expected behavior:** Should validate as valid Bash syntax.
**Tests:** Bash validation workflow with file-based counters.

---

## Test Case 23: Code Block with Mixed Case Attributes

```Rust,no_run
fn mixed_case_with_attribute() {
    println!("Testing Rust (uppercase) with no_run");
}
```

**Expected behavior:** Should compile but not run.
**Tests:** Combination of case-insensitive fence and attribute handling.

---

## Test Case 24: Very Long Multi-line Block

```rust
// This tests that the AWK NUL-delimiter approach works for very long blocks
fn very_long_function() {
    let line1 = "Line 1";
    let line2 = "Line 2";
    let line3 = "Line 3";
    let line4 = "Line 4";
    let line5 = "Line 5";
    let line6 = "Line 6";
    let line7 = "Line 7";
    let line8 = "Line 8";
    let line9 = "Line 9";
    let line10 = "Line 10";

    println!("{} {} {} {} {} {} {} {} {} {}",
        line1, line2, line3, line4, line5,
        line6, line7, line8, line9, line10
    );
}
```

**Expected behavior:** Should validate and compile successfully.
**Tests:** AWK NUL-delimiter preservation of multi-line content.
**Bug context:** Without NUL delimiters, multi-line blocks would split across records.

---

## Test Case 25: Code Block Immediately After Header

### Header With No Gap
```rust
fn no_gap_after_header() {
    println!("This block immediately follows a header");
}
```

**Expected behavior:** Should validate and compile successfully.
**Tests:** Block extraction regardless of surrounding markdown structure.

---

## Test Case 26: Multiple Empty Lines Inside Block

```rust
fn with_empty_lines() {


    println!("Has multiple empty lines above");


    println!("And multiple empty lines between");
}
```

**Expected behavior:** Should validate and compile successfully.
**Tests:** Preservation of internal empty lines in content.

---

## Test Case 27: Code Block with Trailing Whitespace

```rust
fn with_trailing_spaces() {
    println!("This line has trailing spaces");
}
```

**Expected behavior:** Should validate (rustfmt will normalize).
**Tests:** Handling of trailing whitespace in code blocks.

---

## Test Case 28: Incomplete Code Snippet (No main/use/mod)

```rust
let x = 42;
let y = 100;
println!("{} {}", x, y);
```

**Expected behavior:** Should pass syntax validation but skip compilation (incomplete snippet).
**Tests:** Detection of incomplete code snippets that shouldn't be compiled.

---

## Test Case 29: Shell Script Variant

```sh
#!/bin/sh
echo "Testing sh variant"
```

**Expected behavior:** Should validate as valid shell syntax.
**Tests:** Alternative shell script fence markers.

---

## Test Case 30: Code Block at Start of File

```rust
fn at_start_of_file() {
    println!("This is near the start of the file");
}
```

**Expected behavior:** Should validate and compile successfully.
**Tests:** Extraction works regardless of position in file.

---

## Summary of Bug Fixes Tested

### Bug Fix #1: Content Accumulation with Empty First Lines
- **Test Cases:** 2, 26
- **Fix:** AWK script now properly handles empty first lines in code blocks
- **Before:** Empty first line would cause content to be lost
- **After:** All lines preserved correctly

### Bug Fix #2: Unclosed Blocks at EOF
- **Test Cases:** 14
- **Fix:** Added END block in AWK script to handle unclosed blocks
- **Before:** Content in unclosed blocks at EOF would be lost
- **After:** END block outputs any remaining content

### Bug Fix #3: Case-Insensitive Regex for Rust/rust
- **Test Cases:** 3, 4, 23
- **Fix:** AWK regex now matches both `rust` and `Rust`
- **Before:** Only lowercase `rust` was matched
- **After:** Both case variations are matched

### Bug Fix #4: File-Based Counters for All Validators
- **Test Cases:** All (infrastructure)
- **Fix:** All validators now use file-based counters instead of bash variables
- **Before:** Subshell scope issues caused incorrect counts
- **After:** Counters persist correctly across subshell boundaries

---

## Running the Tests

These test cases are automatically validated by the GitHub Actions workflow:
`.github/workflows/doc-validation.yml`

To run locally:

```bash
# Extract Rust blocks (from the workflow)
.github/workflows/doc-validation.yml  # markdown-code-samples job

# Or run the full workflow locally with act:
act -j markdown-code-samples
```

---

## Expected Workflow Output

When processing this file, the workflow should:

1. Extract ~20-25 Rust code blocks
2. Skip ~5-7 blocks (ignore, should_panic, placeholders, empty)
3. Validate ~15-18 blocks (syntax or compilation)
4. Fail 0 blocks (all valid or properly skipped)
5. Report detailed stats at the end

The counters should accurately reflect all blocks processed across all markdown files.
