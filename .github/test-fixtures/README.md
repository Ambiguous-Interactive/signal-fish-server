# Markdown Validation Test Fixtures

This directory contains comprehensive test fixtures for the markdown code validation workflow defined in `.github/workflows/doc-validation.yml`.

## Purpose

These test fixtures ensure that critical bug fixes in the markdown validation workflow do not regress. The workflow validates code blocks in markdown files across multiple languages (Rust, JSON, YAML, TOML, Bash) and must handle various edge cases correctly.

## Files

### Test Fixture

**`markdown-validation-test-cases.md`**

A comprehensive markdown file containing 30+ test cases covering:

1. **Multi-line code blocks** - Basic extraction and validation
2. **Empty first lines** - Bug fix for content accumulation
3. **Case variations** - `rust` vs `Rust` (case-insensitive matching)
4. **Attributes** - `ignore`, `no_run`, `should_panic`
5. **Edge cases** - Empty blocks, unclosed blocks, nested blocks
6. **Placeholders** - `todo!()`, ellipsis, documentation markers
7. **Multiple languages** - JSON, YAML, TOML, Bash validation
8. **Complex code** - Multi-line structs, impls, functions

Each test case documents:
- **Expected behavior** - What should happen when the workflow processes it
- **Tests** - What specific functionality it validates
- **Bug context** - Which bug fix it prevents from regressing

### Test Scripts

**`extract-rust-blocks.py`**

Python script that extracts Rust code blocks from markdown files using the same logic as the AWK
script in the GitHub Actions workflow. Outputs tab-separated records (line number, attributes, content)
delimited by NUL bytes.

Output format: `line_number\tattributes\tcontent\0`

**`validate-test-cases.sh`** (Recommended)

Simple, reliable test script that validates the core bug fixes. Runs 5 focused tests:

1. **Block extraction** - Verifies blocks are extracted from test fixture
2. **Empty first line** - Bug Fix #1
3. **Unclosed EOF** - Bug Fix #2
4. **Case-insensitive** - Bug Fix #3 (rust/Rust)
5. **Attributes** - Verifies ignore, no_run extraction

Usage: `./validate-test-cases.sh`

**`test-markdown-validation.sh`** (Advanced)

Comprehensive test script with 8 tests including AWK compatibility checks.
More complex and may have shell-specific issues.

**`simple-test.sh`**

Intermediate test script using the Python extractor with detailed validation.

## Bug Fixes Covered

### Bug Fix #1: Content Accumulation with Empty First Lines

**Problem:** AWK script was incorrectly handling code blocks with empty first lines,
causing content to be lost.

**Fix:** Improved content accumulation logic in AWK script:

```awk
in_block {
  if (content == "") {
    content = $0
  } else {
    content = content "\n" $0
  }
}
```

**Test Cases:** 2, 26

---

### Bug Fix #2: Unclosed Blocks at EOF

**Problem:** Code blocks without closing backticks at end of file were not extracted.

**Fix:** Added END block to AWK script:

```awk
END {
  if (in_block) {
    printf "%s:::%s:::%s\0", block_start, attrs, content
  }
}
```

**Test Cases:** 14

---

### Bug Fix #3: Case-Insensitive Regex for Rust/Rust

**Problem:** Only lowercase `rust` fence markers were matched; uppercase `Rust` was ignored.

**Fix:** Updated regex to use `[Rr]ust`:

```awk
/^```[Rr]ust(,.*)?$/ {
  # Match both rust and Rust
}
```

**Test Cases:** 3, 4, 23

---

### Bug Fix #4: File-Based Counters for All Validators

**Problem:** Bash subshell scope issues caused incorrect counter values when using pipes and while loops.

**Fix:** All validators now use temporary files to store counters:

```bash
COUNTER_FILE="$TEMP_DIR/counters"
echo "0 0 0 0" > "$COUNTER_FILE"  # total validated skipped failed

# Update in loop
read -r total validated skipped failed < "$COUNTER_FILE"
total=$((total + 1))
echo "$total $validated $skipped $failed" > "$COUNTER_FILE"

# Read final values
read -r total validated skipped failed < "$COUNTER_FILE"
```

**Test Cases:** All (infrastructure)

---

## Usage

### Running Tests Locally

```bash
# Run the recommended test script (fast and reliable)
.github/test-fixtures/validate-test-cases.sh

# Or run the comprehensive test suite
.github/test-fixtures/test-markdown-validation.sh

# Or run the intermediate version
.github/test-fixtures/simple-test.sh
```

### Running via GitHub Actions

The test fixture is automatically validated by the `doc-validation.yml` workflow whenever markdown files or the workflow itself is modified.

```bash
# Trigger manually (requires act or GitHub Actions)
gh workflow run doc-validation.yml

# Or use act for local testing
act -j markdown-code-samples
```

### Expected Output

When the test script runs successfully, you should see:

```text
INFO: Running markdown validation tests...

INFO: Extracting Rust code blocks...
PASS: Extracted 25 blocks
INFO: Testing empty first line handling...
PASS: Empty first line handled correctly
INFO: Testing unclosed block at EOF...
PASS: Unclosed EOF handled correctly
INFO: Testing case-insensitive rust/Rust...
PASS: Case-insensitive matching works
INFO: Testing attribute extraction...
PASS: Attribute extraction works
INFO: Testing multiple consecutive blocks...
PASS: Multiple blocks extracted correctly

PASS: All tests passed!

INFO: Summary:
  - Extracted 25 blocks from test fixture
  - Empty first line handling: OK
  - Unclosed EOF handling: OK
  - Case-insensitive matching: OK
  - Attribute extraction: OK
  - Multiple blocks: OK
```

## Adding New Test Cases

To add new test cases to `markdown-validation-test-cases.md`:

1. Add a new section with a descriptive header
2. Include the code block to test
3. Document:
   - **Expected behavior** - What should happen
   - **Tests** - What it validates
   - **Bug context** (if applicable) - What bug it prevents

4. Update the summary section if testing a new category of bugs

Example:

````markdown
## Test Case 31: Your New Test

```rust
fn your_test_code() {
    // Your test code here
}
```

**Expected behavior:** Should validate and compile successfully.

**Tests:** Your specific test scenario.

**Bug context:** (if applicable) What bug this prevents.
````

## Maintenance

### When to Update These Fixtures

1. **After fixing a markdown validation bug** - Add test cases that would have caught the bug
2. **When adding new language support** - Add examples for the new language
3. **When changing validation logic** - Ensure existing test cases still pass
4. **When adding new attributes** - Test the new attribute handling

### Validation Checklist

Before committing changes to the validation workflow or test fixtures:

- [ ] Run `test-markdown-validation.sh` locally
- [ ] Verify all test cases in `markdown-validation-test-cases.md` are documented
- [ ] Update this README if adding new bug fixes or test categories
- [ ] Ensure CI workflow passes with the new changes
- [ ] Document any new edge cases discovered

## Integration with CI/CD

These test fixtures are part of the comprehensive documentation validation strategy:

```text
Documentation Validation Workflow
├── rustdoc: Build and validate Rust API docs
├── doc-tests: Run tests in documentation comments
├── markdown-code-samples: ← These fixtures test this job
│   ├── Rust code blocks
│   ├── JSON validation
│   ├── YAML validation
│   ├── TOML validation
│   └── Bash validation
└── link-check: Validate all internal/external links
```

The workflow ensures:

- Zero broken links in documentation
- All code examples compile and run
- Multi-language code blocks are syntactically valid
- Edge cases (empty lines, unclosed blocks, case variations) are handled correctly

## References

- Workflow definition: `.github/workflows/doc-validation.yml`
- Project guidelines: `.llm/context.md`
- Testing standards: `skills/testing-strategies.md`

## License

Copyright (c) 2025 Ambiguous Interactive. All rights reserved.

Part of the Signal Fish Server project.
