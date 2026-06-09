# Testing the Markdown Validation Workflow

Quick reference for running and validating the markdown code extraction tests.

## Quick Start

```bash
# Run the main test suite
.github/test-fixtures/validate-test-cases.sh
```

Expected result: extraction plus all focused checks pass

## What Gets Tested

### Bug Fixes Validated

1. **Empty first line handling** - Code blocks with empty first lines are extracted correctly
2. **Unclosed EOF blocks** - Code blocks without closing fence at EOF are handled
3. **Canonical Rust fence matching** - `rust` and `Rust` fence markers work; non-canonical variants such as `RUST` are ignored consistently
4. **Attribute extraction** - Attributes like `ignore`, `no_run` are parsed correctly
5. **File-based counters** - Counter values persist across subshell boundaries (in CI)
6. **Extractor parity** - Python helper output matches canonical AWK output byte-for-byte,
   including CRLF input and POSIX-whitespace fence attributes

### Test Coverage

- 30+ test cases in `markdown-validation-test-cases.md`
- Covers Rust, JSON, YAML, TOML, and Bash code blocks
- Tests edge cases: empty blocks, placeholders, malformed blocks
- Validates multi-line code blocks with complex indentation

## Available Test Scripts

| Script | Purpose | Use When |
|--------|---------|----------|
| `validate-test-cases.sh` | Canonical AWK/Python extractor parity validation | Running locally, CI checks |
| `extract-rust-blocks.py` | Python extractor tool | Debugging, manual testing |
| `simple-test.sh` | Compatibility wrapper for `validate-test-cases.sh` | Existing local workflows |
| `test-markdown-validation.sh` | Compatibility wrapper for `validate-test-cases.sh` | Existing local workflows |

## Continuous Integration

The fixture parity script is automatically validated by:

`.github/workflows/doc-validation.yml`

Fixtures are intentionally excluded from normal repository markdown validation because
they include malformed examples. The workflow runs `validate-test-cases.sh` directly.

This workflow runs on:

- Push to `main` (when markdown, Rust, workflow helper, or fixture files change)
- Pull requests to `main`

## Manual Testing

### Test a specific markdown file

```bash
awk -f .github/scripts/extract-rust-blocks.awk your-file.md
```

Output format: `line_number\tattributes\tcontent\0` (NUL-delimited)

### Validate extraction logic

```bash
# Extract and count blocks
count=0
while IFS= read -r -d '' _record; do
  count=$((count + 1))
done < <(awk -f .github/scripts/extract-rust-blocks.awk README.md)
printf '%s\n' "$count"

# View first block
awk -f .github/scripts/extract-rust-blocks.awk README.md | tr '\0' '\n' | sed -n '1p'
```

## Troubleshooting

### Tests fail locally but pass in CI

- Ensure you have Python 3 and awk installed
- Check that you're using bash (not sh or zsh)
- Verify file permissions: `chmod +x .github/test-fixtures/*.sh`

### AWK-related errors

- The GitHub Actions workflow invokes `awk -f .github/scripts/extract-rust-blocks.awk`
- Some systems use different AWK implementations with different extension support
- The Python extractor (`extract-rust-blocks.py`) is portable across all systems

### Extraction returns 0 blocks

- Verify the markdown file has properly formatted code fences
- Check that fences use ` ```rust ` not ` ~~~rust `
- CommonMark allows up to three leading spaces before fence markers; four or more spaces are indented code and will not start a fenced block

## Adding New Test Cases

1. Edit `markdown-validation-test-cases.md`
2. Add your test case with documentation
3. Run `validate-test-cases.sh` to verify
4. Update the summary section in the test file

See `README.md` for detailed instructions.

## Exit Codes

- `0` - All tests passed
- `1` - One or more tests failed
- `2` - Missing dependencies or configuration error
