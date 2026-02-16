# CI/CD Testing and Preventative Measures

This document describes the comprehensive testing and automation infrastructure designed to prevent CI/CD issues from recurring.

## Table of Contents

- [Overview](#overview)
- [Test Infrastructure](#test-infrastructure)
- [Pre-commit Hooks](#pre-commit-hooks)
- [Helper Scripts](#helper-scripts)
- [Running Tests Locally](#running-tests-locally)
- [Troubleshooting](#troubleshooting)
- [Architecture Decisions](#architecture-decisions)

## Overview

The CI/CD testing infrastructure was created in response to several actual production issues:

1. **Link check failures**: Placeholder URLs (e.g., `https://github.com/owner/repo`) causing lychee to fail
2. **Markdown lint failures**: Missing language identifiers on code blocks (MD040 rule)
3. **MSRV inconsistencies**: Mismatched Rust versions between Cargo.toml, Dockerfile, and CI workflows
4. **AWK compatibility**: Non-portable AWK patterns causing failures with different AWK implementations

### Goals

- **Prevent entire categories of issues**, not just specific bugs
- **Fast feedback loops** with pre-commit hooks and helper scripts
- **Data-driven tests** that are easy to extend with new test cases
- **Clear diagnostics** with actionable error messages
- **Documentation** for troubleshooting and maintenance

## Test Infrastructure

All CI/CD tests are located in [`tests/ci_config_tests.rs`](/workspaces/signal-fish-server/tests/ci_config_tests.rs).

### Test Categories

#### 1. Link Check Tests

Tests that validate link checking configuration and catch broken links:

| Test | Purpose | What It Catches |
|------|---------|-----------------|
| `test_lychee_config_exists_and_is_valid` | Validates `.lychee.toml` exists and has required fields | Missing or malformed link checker config |
| `test_lychee_excludes_placeholder_urls` | Ensures placeholder URLs are excluded | Link checker failures on example URLs |
| `test_no_actual_placeholder_urls_in_docs` | Flags placeholder URLs that should be replaced | Documentation quality issues |
| `test_link_check_workflow_uses_lychee_config` | Verifies CI workflow references `.lychee.toml` | Config drift between local and CI |
| `test_lychee_config_format_is_valid_toml` | Validates TOML syntax | Syntax errors causing workflow failures |

**Example:** Preventing the placeholder URL issue

```rust
// This test ensures placeholders are excluded
let test_cases = vec![
    ("http://localhost", "Localhost URLs are placeholders"),
    ("https://github.com/owner/repo", "Generic placeholder pattern"),
    ("https://github.com/{}", "Template placeholder pattern"),
];
```

#### 2. Markdown Lint Tests

Tests that validate markdown formatting and consistency:

| Test | Purpose | What It Catches |
|------|---------|-----------------|
| `test_markdown_files_have_language_identifiers` | Ensures code blocks have language identifiers | MD040 violations (missing language on code blocks) |
| `test_markdown_no_capitalized_filenames_in_links` | Catches capitalization issues in links | Link breakage on case-sensitive filesystems |
| `test_markdown_technical_terms_consistency` | Validates technical term capitalization | Inconsistent documentation (GitHub vs github) |
| `test_markdown_common_patterns_are_correct` | Data-driven pattern validation | Common formatting mistakes |
| `test_markdown_config_exists` | Validates `.markdownlint.json` exists | Missing markdownlint configuration |

**Example:** Data-driven pattern validation

```rust
let test_cases = vec![
    (
        r"```\s*$",
        "Code block without language identifier",
        "Add language: ```rust or ```bash",
    ),
    (
        r"\]\([A-Z]:/",
        "Windows path in link",
        "Use forward slashes in links",
    ),
];
```

#### 3. CI Workflow Validation Tests

Tests that validate CI workflow configuration:

| Test | Purpose | What It Catches |
|------|---------|-----------------|
| `test_link_check_workflow_exists_and_is_configured` | Validates link-check workflow setup | Missing or misconfigured link checking |
| `test_markdownlint_workflow_exists_and_is_configured` | Validates markdownlint workflow setup | Missing or misconfigured markdown linting |
| `test_doc_validation_workflow_has_shellcheck` | Ensures doc-validation validates its own scripts | AWK/bash syntax errors in workflows |
| `test_workflows_use_concurrency_groups` | Validates concurrency configuration | Wasted CI resources on outdated runs |
| `test_workflows_have_timeouts` | Ensures workflows have reasonable timeouts | Hanging jobs consuming resources |
| `test_workflows_use_minimal_permissions` | Validates least-privilege principle | Security issues from overly permissive workflows |

**Example:** Preventing AWK syntax errors

```rust
// This test ensures the doc-validation workflow validates its own inline scripts
assert!(
    content.contains("shellcheck"),
    "doc-validation.yml should include shellcheck validation of inline scripts.\n\
     This prevents shell/AWK syntax errors in workflow scripts."
);
```

#### 4. MSRV Consistency Tests

Existing comprehensive tests for Rust version consistency (see previous documentation).

## Pre-commit Hooks

The pre-commit hook (`.githooks/pre-commit`) runs fast checks before each commit:

### What It Checks

1. **Code formatting** (`cargo fmt --check`)
2. **Panic-prone patterns** (`scripts/check-no-panics.sh`)
3. **Markdown linting** (`markdownlint-cli2`) - if installed
4. **Link checking** (`lychee --offline`) - if installed, on staged files only

### Installation

```bash
# Enable pre-commit hooks
./scripts/enable-hooks.sh

# Verify installation
git config core.hooksPath
# Should output: .githooks
```

### Link Checking in Pre-commit

The pre-commit hook runs link checks in offline mode for speed:

```bash
# Only checks staged markdown files
# Uses --offline flag to skip network requests (fast)
# Validates internal links and markdown structure only
```

To check external links manually:

```bash
# Check specific file with full link checking
lychee --config .lychee.toml docs/setup.md

# Check all files (includes external links)
lychee --config .lychee.toml '**/*.md'
```

### Bypassing Hooks (Not Recommended)

```bash
# Only use in emergencies (e.g., fixing broken CI)
git commit --no-verify
```

## Helper Scripts

### 1. Fast Link Checking: `scripts/check-links-fast.sh`

Quickly validate links in modified files.

**Usage:**

```bash
# Check modified files (git status)
./scripts/check-links-fast.sh

# Check staged files only
./scripts/check-links-fast.sh --staged

# Check all markdown files
./scripts/check-links-fast.sh --all

# Check specific files
./scripts/check-links-fast.sh README.md docs/setup.md
```

**Features:**

- Fast offline mode by default (local links only)
- Respects `.lychee.toml` configuration
- Color-coded output
- Clear error messages

**Example output:**

```text
=========================================
Fast Link Check
=========================================

Checking modified markdown files...
Files to check: 3

Running lychee link checker...

✓ All local links are valid

Note: This was a fast check (--offline mode).
To check external links, run: lychee --config .lychee.toml <file>
```

### 2. Lychee Config Validation: `scripts/validate-lychee-config.sh`

Validate `.lychee.toml` configuration file.

**Usage:**

```bash
./scripts/validate-lychee-config.sh
```

**What it checks:**

- Configuration file exists
- TOML syntax is valid
- Required fields are present
- Placeholder URL exclusions
- Common configuration mistakes
- Reasonable timeout and concurrency settings

**Example output:**

```text
=========================================
Lychee Configuration Validation
=========================================

[INFO]  Checking for .lychee.toml...
[OK]    .lychee.toml found
[INFO]  Testing configuration syntax...
[OK]    Configuration syntax is valid
[INFO]  Checking required fields...
[OK]    Found: max_concurrency
[OK]    Found: accept
[OK]    Found: exclude
[OK]    Found: timeout
[OK]    Found: user_agent
[INFO]  Checking placeholder URL exclusions...
[OK]    Excludes: http://localhost
[OK]    Excludes: http://127.0.0.1
[OK]    Excludes: ws://localhost
[OK]    Excludes: mailto:

=========================================
Validation Summary
=========================================
✓ All validations passed
```

### 3. Markdown Checking: `scripts/check-markdown.sh`

Validate and auto-fix markdown files.

**Usage:**

```bash
# Check all markdown files
./scripts/check-markdown.sh

# Auto-fix issues
./scripts/check-markdown.sh fix
```

## Running Tests Locally

### Run All CI Config Tests

```bash
# Run all CI configuration tests
cargo test --test ci_config_tests

# Run with verbose output
cargo test --test ci_config_tests -- --nocapture

# Run specific test
cargo test --test ci_config_tests test_lychee_config_exists
```

### Run Pre-commit Checks Manually

```bash
# Run pre-commit hook manually (without committing)
.githooks/pre-commit

# Run individual checks
cargo fmt --check
./scripts/check-markdown.sh
./scripts/check-links-fast.sh --staged
```

### Full CI Validation Locally

```bash
# Run the full mandatory workflow (same as CI)
cargo fmt --check
cargo clippy --all-targets --all-features
cargo test --all-features

# Additionally run CI-specific checks
./scripts/check-ci-config.sh
./scripts/validate-lychee-config.sh
./scripts/check-markdown.sh
```

## Troubleshooting

### Common Issues and Solutions

#### 1. Link Check Failing on Placeholder URLs

**Symptom:**

```text
✗ https://github.com/owner/repo | 404 Not Found
```

**Solution:**

Add the URL pattern to `.lychee.toml` exclude list:

```toml
exclude = [
    "https://github.com/owner/repo/*",
    "https://github.com/{}/*",
]
```

**Why it happens:** Documentation uses placeholder URLs for examples.

**Test that prevents this:** `test_lychee_excludes_placeholder_urls`

#### 2. Markdown Lint Failing on Code Blocks

**Symptom:**

```text
README.md:42 MD040/fenced-code-language Fenced code blocks should have a language specified
```

**Solution:**

Add language identifier to code blocks:

```markdown
<!-- Before (fails) -->
````text
code here
````

<!-- After (passes) -->
````bash
code here
````

```

**Why it happens:** Missing language identifier prevents syntax highlighting.

**Test that prevents this:** `test_markdown_files_have_language_identifiers`

#### 3. MSRV Version Mismatch

**Symptom:**

```text
ERROR: Dockerfile Rust version must match Cargo.toml rust-version.
Expected: FROM rust:1.88.0 or FROM rust:1.88
Found: FROM rust:1.87
```

**Solution:**

Update Dockerfile to match Cargo.toml:

```dockerfile
FROM rust:1.88.0-bookworm AS builder
```

**Why it happens:** Manual updates to one file without updating others.

**Test that prevents this:** `test_msrv_consistency_across_config_files`

#### 4. AWK Pattern Not Working in CI

**Symptom:**

```text
awk: line 1: syntax error at or near /
```

**Solution:**

Use POSIX-compatible AWK patterns:

```bash
# Before (GNU awk only)
awk '/^```[Rr]ust(,.*)?$/ { ... }'

# After (POSIX compatible)
awk '/^```[Rr]ust/ { ... }'
```

**Why it happens:** Different AWK implementations (gawk vs mawk).

**Test that prevents this:** `test_doc_validation_workflow_has_shellcheck`

#### 5. Pre-commit Hook Not Running

**Symptom:** Pre-commit checks don't run when committing.

**Solution:**

```bash
# Reinstall hooks
./scripts/enable-hooks.sh

# Verify configuration
git config core.hooksPath
# Should output: .githooks

# Check hook is executable
ls -la .githooks/pre-commit
# Should show: -rwxr-xr-x
```

**Why it happens:** Hooks not enabled or lost during git operations.

#### 6. Tests Failing After Config Changes

**Symptom:** CI tests fail after updating `.lychee.toml` or `.markdownlint.json`.

**Solution:**

```bash
# Run validation scripts
./scripts/validate-lychee-config.sh
./scripts/check-markdown.sh

# Run tests locally
cargo test --test ci_config_tests

# Check for syntax errors
# For .lychee.toml
lychee --dump .lychee.toml

# For .markdownlint.json
markdownlint-cli2 --help  # Validates config on load
```

## Architecture Decisions

### Why Data-Driven Tests?

Data-driven tests make it easy to add new test cases without duplicating code:

```rust
// Adding a new test case is just adding an entry to the array
let test_cases = vec![
    ("http://localhost", "Localhost URLs are placeholders"),
    ("https://github.com/owner/repo", "Generic placeholder pattern"),
    // Easy to add more cases here
];
```

**Benefits:**

- Easy to extend with new patterns
- Clear and maintainable
- Self-documenting test cases
- Reduces code duplication

### Why Separate Helper Scripts?

Helper scripts provide fast feedback during development:

**Benefits:**

- Faster than running full CI locally
- Can be integrated into editor workflows
- Provide more detailed output than CI
- Easy to run on specific files

**Design principle:** Scripts should be usable standalone and in CI.

### Why Pre-commit Hooks?

Pre-commit hooks catch issues before they reach CI:

**Benefits:**

- Immediate feedback (seconds vs minutes)
- Prevents broken commits from polluting history
- Saves CI resources
- Encourages good practices

**Design principle:** Hooks should be fast (<5 seconds) and non-blocking for edge cases.

### Why Offline Link Checking in Pre-commit?

Offline mode checks internal links only, skipping external URLs:

**Benefits:**

- Fast (no network requests)
- Works without internet connection
- Catches most common errors (broken internal links)
- Full checks still run in CI

**Tradeoff:** Doesn't catch broken external links until CI runs.

### Why File-based Counters in Shell Scripts?

Shell scripts use files to accumulate counters instead of variables:

```bash
# Use files to avoid bash subshell scope issues
COUNTER_FILE="$TEMP_DIR/counters"
echo "0 0 0 0" > "$COUNTER_FILE"

# Read and update counters
read -r total validated skipped failed < "$COUNTER_FILE"
total=$((total + 1))
echo "$total $validated $skipped $failed" > "$COUNTER_FILE"
```

**Reason:** Bash subshells (from pipes and while loops) cannot modify parent shell variables. Files persist state across subshells.

**Alternative considered:** Using process substitution (`< <(command)`), but file-based approach is more portable and debuggable.

## Extending the Test Suite

### Adding New Link Check Tests

1. Add test case to `test_lychee_excludes_placeholder_urls`:

```rust
let test_cases = vec![
    // ... existing cases ...
    ("https://my-new-placeholder.com", "New placeholder pattern"),
];
```

2. Update `.lychee.toml` with the exclusion:

```toml
exclude = [
    # ... existing exclusions ...
    "https://my-new-placeholder.com/*",
]
```

3. Run tests to verify:

```bash
cargo test test_lychee_excludes_placeholder_urls
```

### Adding New Markdown Pattern Tests

1. Add test case to `test_markdown_common_patterns_are_correct`:

```rust
let test_cases = vec![
    // ... existing cases ...
    (
        r"new_anti_pattern",
        "Description of the issue",
        "Suggested fix",
    ),
];
```

2. Run tests to verify:

```bash
cargo test test_markdown_common_patterns_are_correct
```

### Adding New Workflow Validation Tests

1. Create new test function in `tests/ci_config_tests.rs`:

```rust
#[test]
fn test_my_new_workflow_validation() {
    let root = repo_root();
    let workflow = root.join(".github/workflows/my-workflow.yml");

    // Add validation logic
    assert!(workflow.exists(), "Workflow is missing");

    let content = read_file(&workflow);
    assert!(content.contains("expected-content"), "Missing required content");
}
```

2. Run the test:

```bash
cargo test test_my_new_workflow_validation
```

## Summary

This testing infrastructure provides defense in depth against CI/CD issues:

| Layer | Purpose | Speed | Coverage |
|-------|---------|-------|----------|
| **Pre-commit hooks** | Fast feedback during development | <5s | Basic checks on changed files |
| **Helper scripts** | Quick validation during development | <10s | Targeted checks on specific areas |
| **Unit tests** | Comprehensive validation | ~30s | All configuration and patterns |
| **CI workflows** | Final validation before merge | 5-10min | Full integration testing |

**Key principle:** Catch issues as early as possible, with progressively more thorough checks at each stage.

## References

- [Lychee Configuration Documentation](https://github.com/lycheeverse/lychee#configuration)
- [Markdownlint Rules](https://github.com/DavidAnson/markdownlint/blob/main/doc/Rules.md)
- [GitHub Actions Best Practices](.llm/skills/github-actions-best-practices.md)
- [CI/CD Troubleshooting](.llm/skills/ci-cd-troubleshooting.md)
