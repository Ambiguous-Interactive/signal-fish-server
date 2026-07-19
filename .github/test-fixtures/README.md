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
3. **Canonical Rust fences** - `rust` and `Rust`; non-canonical variants are ignored consistently
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

Python helper that extracts Rust code blocks from markdown files with byte-compatible output to
the canonical AWK extractor in `.github/scripts/extract-rust-blocks.awk`. Outputs tab-separated
records (line number, attributes, content) delimited by NUL bytes.

Output format: `line_number\tattributes\tcontent\0`

**`validate-test-cases.sh`** (Recommended)

Canonical test script that validates the core bug fixes and AWK/Python extractor parity.
Runs focused checks for:

1. **Block extraction** - Verifies blocks are extracted from test fixture
2. **Empty first line** - Bug Fix #1
3. **Unclosed EOF** - Bug Fix #2
4. **Canonical Rust fences** - Bug Fix #3 (`rust` and `Rust` only)
5. **Attributes** - Verifies ignore, no_run extraction
6. **Extractor parity** - Verifies the Python helper matches canonical AWK output, including CRLF input

Usage: `./validate-test-cases.sh`

**`test-markdown-validation.sh`**

Compatibility wrapper for `validate-test-cases.sh`.

**`simple-test.sh`**

Compatibility wrapper for `validate-test-cases.sh`.

## Bug Fixes Covered

### Bug Fix #1: Content Accumulation with Empty First Lines

**Problem:** AWK script was incorrectly handling code blocks with empty first lines,
causing content to be lost.

**Fix:** Track whether content has been seen separately from the accumulated text, preserving
leading blank lines without confusing an empty first line for "no content yet":

```awk
in_block {
  if (seen_content) {
    content = content "\n" $0
  } else {
    content = $0
    seen_content = 1
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
    printf "%s\t%s\t%s%c", block_start, attrs, content, 0
  }
}
```

**Test Cases:** 14

---

### Bug Fix #3: Canonical Rust Fence Matching

**Problem:** Only lowercase `rust` fence markers were matched; uppercase `Rust` was ignored.
Bare prefix patterns also matched non-canonical languages such as `rustic` and `rusty`.

**Fix:** Use the canonical extractor in `.github/scripts/extract-rust-blocks.awk`. It parses the
fence separately, then applies a token-boundary info-string check so both case variants and
attribute styles share one extractor path without matching longer language names:

```awk
if (rest ~ /^[Rr]ust([[:space:],]|$)/) {
  attrs = rest
  sub(/^[Rr]ust,?/, "", attrs)
  if (attrs == "") attrs = "none"
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

# Compatibility wrappers delegate to the same canonical script
.github/test-fixtures/test-markdown-validation.sh
.github/test-fixtures/simple-test.sh
```

### Running via GitHub Actions

The `doc-validation.yml` workflow runs `validate-test-cases.sh` in its `Validate Rust markdown extractor fixtures` step. The workflow triggers on `.github/test-fixtures/**` changes, but the fixture markdown files remain excluded from normal repository markdown validation because they intentionally contain malformed examples.

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
INFO: Testing canonical rust/Rust matching...
PASS: Canonical rust/Rust matching works
INFO: Testing attribute extraction...
PASS: Attribute extraction works
INFO: Testing multiple consecutive blocks...
PASS: Multiple blocks extracted correctly

PASS: All tests passed!

INFO: Summary:
  - Extracted 25 blocks from test fixture
  - Empty first line handling: OK
  - Unclosed EOF handling: OK
  - Canonical rust/Rust matching: OK
  - Attribute extraction: OK
  - Multiple blocks: OK
  - Python extractor parity: OK
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

- [ ] Run `validate-test-cases.sh` locally
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
- Testing standards: `.llm/skills/testing/SKILL.md`

## License

Copyright (c) 2025 Ambiguous Interactive. All rights reserved.

Part of the Signal Fish Server project.
