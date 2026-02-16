# Testing the Markdown Validation Workflow

Quick reference for running and validating the markdown code extraction tests.

## Quick Start

```bash
# Run the main test suite
.github/test-fixtures/validate-test-cases.sh
```

Expected result: All 5 tests pass

## What Gets Tested

### Bug Fixes Validated

1. **Empty first line handling** - Code blocks with empty first lines are extracted correctly
2. **Unclosed EOF blocks** - Code blocks without closing fence at EOF are handled
3. **Case-insensitive matching** - Both `rust` and `Rust` fence markers work
4. **Attribute extraction** - Attributes like `ignore`, `no_run` are parsed correctly
5. **File-based counters** - Counter values persist across subshell boundaries (in CI)

### Test Coverage

- 30+ test cases in `markdown-validation-test-cases.md`
- Covers Rust, JSON, YAML, TOML, and Bash code blocks
- Tests edge cases: empty blocks, placeholders, malformed blocks
- Validates multi-line code blocks with complex indentation

## Available Test Scripts

| Script | Purpose | Use When |
|--------|---------|----------|
| `validate-test-cases.sh` | Fast, simple validation | Running locally, CI checks |
| `extract-rust-blocks.py` | Python extractor tool | Debugging, manual testing |
| `simple-test.sh` | Intermediate test suite | Detailed diagnostics |
| `test-markdown-validation.sh` | Comprehensive suite | Full AWK compatibility check |

## Continuous Integration

The test fixture is automatically validated by:

`.github/workflows/doc-validation.yml`

This workflow runs on:

- Push to `main` (when markdown or Rust files change)
- Pull requests to `main`

## Manual Testing

### Test a specific markdown file

```bash
python3 .github/test-fixtures/extract-rust-blocks.py your-file.md
```

Output format: `line_number\tattributes\tcontent\0` (NUL-delimited)

### Validate extraction logic

```bash
# Extract and count blocks
python3 .github/test-fixtures/extract-rust-blocks.py README.md | tr '\0' '\n' | wc -l

# View first block
python3 .github/test-fixtures/extract-rust-blocks.py README.md | tr '\0' '\n' | head -1
```

## Troubleshooting

### Tests fail locally but pass in CI

- Ensure you have Python 3, rustc, and rustfmt installed
- Check that you're using bash (not sh or zsh)
- Verify file permissions: `chmod +x .github/test-fixtures/*.sh`

### AWK-related errors

- The GitHub Actions workflow uses `gawk` (GNU AWK)
- Some systems use `mawk` which has different syntax
- The Python extractor (`extract-rust-blocks.py`) is portable across all systems

### Extraction returns 0 blocks

- Verify the markdown file has properly formatted code fences
- Check that fences use ` ```rust ` not ` ~~~rust `
- Ensure there's no indentation before the fence markers

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
