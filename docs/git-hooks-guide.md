# Git Hooks Guide - Signal Fish Server

This guide explains how to install, configure, and troubleshoot git hooks for the Signal Fish Server project.

## Overview

Pre-commit hooks run automatically before each commit to catch issues early and maintain code quality.
Our hooks prevent the types of issues that have caused CI failures in the past.

## Installation

### Quick Start

```bash
# From the repository root
./scripts/enable-hooks.sh
```

This configures git to use the hooks in `.githooks/pre-commit`.

### Verify Installation

```bash
# Check that hooks are enabled
git config --local core.hooksPath
# Should output: .githooks

# Test the hook (make a dummy change)
echo "# test" >> README.md
git add README.md
git commit -m "test"  # Hook will run
git reset HEAD~1      # Undo test commit
git restore README.md # Restore file
```

## What Gets Checked

The pre-commit hook runs these checks (in order):

### 1. Code Formatting (cargo fmt)

**What it checks:** Rust code follows standard formatting conventions.

**When it runs:** Always (on every commit).

**How to fix:**

```bash
cargo fmt
```

**Example error:**

```text
✗ FAIL: Code formatting
[pre-commit] ERROR: Run 'cargo fmt' to fix formatting issues.
```

### 2. Clippy Lints (cargo clippy)

**What it checks:** Code quality and potential bugs using Rust's official linter.

**When it runs:** When `.rs` files are staged for commit.

**How to fix:**

```bash
# Auto-fix most issues
cargo clippy --fix --allow-dirty --all-targets --all-features

# Or manually fix based on warnings
cargo clippy --all-targets --all-features
```

**Example error:**

```text
✗ FAIL: Clippy lints
[pre-commit] ERROR: Fix clippy warnings before committing.
```

**Common clippy issues:**

- `uninlined_format_args`: Use `format!("{x}")` instead of `format!("{}", x)`
- `unnecessary_unwrap`: Use `?` operator or pattern matching instead
- `redundant_clone`: Remove unnecessary `.clone()` calls

### 3. Panic-Prone Patterns

**What it checks:** Production code doesn't use `.unwrap()`, `panic!()`, or `.expect()`.

**When it runs:** Always (on every commit).

**How to fix:**

```rust
// ❌ Bad: Can panic at runtime
let value = some_option.unwrap();

// ✅ Good: Use ? operator
let value = some_option?;

// ✅ Good: Pattern matching
let value = match some_option {
    Some(v) => v,
    None => return Err(Error::MissingValue),
};
```

**Example error:**

```text
✗ FAIL: Panic patterns
[pre-commit] ERROR: Remove .unwrap(), panic!(), expect() from production code.
```

### 4. MSRV Consistency

**What it checks:** Minimum Supported Rust Version (MSRV) is consistent across:

- `Cargo.toml` (`rust-version`)
- `rust-toolchain.toml` (channel)
- `clippy.toml` (msrv)
<!-- markdownlint-disable-next-line MD044 -- rust:X.Y.Z is a Docker image name -->
- `Dockerfile` (`FROM rust:X.Y.Z`)

**When it runs:** When `Cargo.toml`, `rust-toolchain.toml`, `clippy.toml`, or `Dockerfile` are modified.

**How to fix:**

```bash
# Check for inconsistencies
./scripts/check-msrv-consistency.sh

# Update all files to match Cargo.toml
# See output for specific instructions
```

**Example error:**

```text
✗ FAIL: MSRV consistency
[pre-commit] ERROR: MSRV mismatch across configuration files.
```

**Real-world issue this prevents:**

In commit `1c8ed3b`, we had `Dockerfile` using `rust:1.88` while `Cargo.toml` specified `1.88.0`.
This check catches such inconsistencies.

### 5. Workflow AWK Validation

**What it checks:** AWK scripts in GitHub Actions workflows are syntactically valid and portable.

**When it runs:** When `.github/workflows/*.yml` files are modified.

**How to fix:**

```bash
# Validate AWK scripts
./scripts/validate-workflow-awk.sh

# Common fixes:
# - Use /^```[Rr]ust/ instead of /^```[Rr]ust(,.*)?$/
# - Use printf "%c", 0 instead of printf "\0"
# - Use sub()/gsub() instead of match()
```

**Example error:**

```text
✗ FAIL: AWK validation
[pre-commit] ERROR: AWK script syntax errors in workflow files.
```

**Real-world issue this prevents:**

In commit `1c8ed3b`,
we changed `/^```[Rr]ust(,.*)?$/` to `/^```[Rr]ust/` to handle variations like `rust ignore` vs `rust,ignore`.
This check catches regex patterns that are too strict.

### 6. Markdown Linting

**What it checks:** Markdown files follow formatting standards (headings, lists, code blocks).

**When it runs:** When `.md` files are modified (requires `markdownlint-cli2`).

**How to fix:**

```bash
# Auto-fix markdown issues
./scripts/check-markdown.sh fix

# Or manually check
./scripts/check-markdown.sh
```

**Example error:**

```text
✗ FAIL: Markdown linting
[pre-commit] ERROR: Markdown files have formatting issues.
```

**Install markdownlint:**

```bash
npm install -g markdownlint-cli2
```

### 7. Link Checking (Warning Only)

**What it checks:** Links in markdown files are valid (offline mode).

**When it runs:** When `.md` files are modified (requires `lychee`).

**Note:** This is a **warning only** in pre-commit (offline mode for speed). Full link checking runs in CI.

**Install lychee:**

```bash
cargo install lychee
```

## Bypassing Hooks (Not Recommended)

In rare cases, you may need to bypass hooks:

```bash
# Skip all pre-commit checks
git commit --no-verify

# Or use alias
git commit -n
```

**⚠️ WARNING:** Only bypass hooks if:

- You're committing work-in-progress that you'll fix before merging
- The hook is incorrectly flagging valid code (report this as a bug)
- You're in an emergency hotfix situation

**Never bypass hooks on commits that will be merged to main.**

## Running Checks Manually

### Individual Checks

```bash
# Format code
cargo fmt

# Run clippy
cargo clippy --all-targets --all-features

# Check for panics
./scripts/check-no-panics.sh patterns

# Check MSRV
./scripts/check-msrv-consistency.sh

# Validate AWK scripts
./scripts/validate-workflow-awk.sh

# Check markdown
./scripts/check-markdown.sh
```

### All Checks (Local CI)

```bash
# Run all CI checks locally
./scripts/run-local-ci.sh

# Fast mode (skip tests)
./scripts/run-local-ci.sh --fast

# Auto-fix mode
./scripts/run-local-ci.sh --fix
```

## Troubleshooting

### Hook Doesn't Run

**Problem:** Committing succeeds without running checks.

**Solution:**

```bash
# Verify hooks are enabled
git config --local core.hooksPath
# Should output: .githooks

# Re-enable if not set
./scripts/enable-hooks.sh

# Check hook is executable
ls -la .githooks/pre-commit
# Should show: -rwxr-xr-x
```

### "Permission Denied" Error

**Problem:**

```text
.githooks/pre-commit: Permission denied
```

**Solution:**

```bash
# Make hook executable
chmod +x .githooks/pre-commit

# Or re-run enable script
./scripts/enable-hooks.sh
```

### Hook Takes Too Long

**Problem:** Pre-commit hook runs for several minutes.

**Context:** Some checks (especially clippy on large changesets) can be slow.

**Solutions:**

1. **Use `--fast` mode for quick commits:**

   ```bash
   # Run faster checks only
   git commit --no-verify  # Skip all hooks
   ./scripts/run-local-ci.sh --fast  # Then run fast checks
   ```

2. **Run clippy incrementally:**

   ```bash
   # Run clippy on specific package
   cargo clippy -p signal-fish-server
   ```

3. **Commit smaller changesets:**
   - Break large changes into smaller, focused commits
   - This makes each pre-commit check faster

### Clippy Fails with "Cannot Fix Automatically"

**Problem:**

```text
error: `cargo fix` is not compatible with --all-targets
```

**Solution:**

```bash
# Run clippy without auto-fix to see warnings
cargo clippy --all-targets --all-features

# Manually fix issues based on output
# Then commit
```

### MSRV Check Fails After Rust Update

**Problem:**

```text
✗ FAIL: rust-toolchain.toml channel=1.87.0 (expected 1.88.0)
```

**Solution:**

```bash
# Update all MSRV references
# 1. Edit rust-toolchain.toml
channel = "1.88.0"

# 2. Edit clippy.toml
msrv = "1.88.0"

# 3. Edit Dockerfile
FROM rust:1.88.0-bookworm

# Verify consistency
./scripts/check-msrv-consistency.sh
```

### AWK Validation Fails

**Problem:**

```text
✗ FAIL: AWK validation
doc-validation.yml:210 - AWK syntax error
```

**Solution:**

1. **Check AWK syntax:**

   ```bash
   # Extract the AWK script and test it
   awk 'YOUR_SCRIPT_HERE' < /dev/null
   ```

2. **Common AWK portability issues:**

   ```awk
   # ❌ Bad: GNU-specific match()
   match($0, /pattern/)

   # ✅ Good: POSIX sub()
   sub(/pattern/, "replacement")

   # ❌ Bad: \0 in printf (not POSIX)
   printf "text\0"

   # ✅ Good: Use %c with 0
   printf "text%c", 0

   # ❌ Bad: Too strict regex
   /^```[Rr]ust(,.*)?$/

   # ✅ Good: Prefix match
   /^```[Rr]ust/
   ```

3. **Test with different AWK implementations:**

   ```bash
   # Test with mawk (POSIX-compliant)
   mawk 'YOUR_SCRIPT' < input.txt

   # Test with gawk (GNU AWK)
   gawk 'YOUR_SCRIPT' < input.txt
   ```

### Markdown Linting Fails

**Problem:**

```text
README.md:42 MD022/blanks-around-headings
```

**Solution:**

```bash
# Auto-fix markdown issues
./scripts/check-markdown.sh fix

# Or manually check what's wrong
./scripts/check-markdown.sh
```

**Common markdown issues:**

- `MD022`: Missing blank lines around headings
- `MD032`: Missing blank lines around lists
- `MD040`: Missing language in fenced code blocks
- `MD041`: First line must be a top-level heading

### Hook Fails in CI but Not Locally

**Problem:** Pre-commit passes locally but CI fails.

**Causes:**

1. **Different Rust versions:**

   ```bash
   # Check your Rust version
   rustc --version

   # Should match rust-toolchain.toml
   cat rust-toolchain.toml
   ```

2. **Uncommitted changes:**

   ```bash
   # Check git status
   git status

   # Ensure all changes are committed
   git add -A
   git commit
   ```

3. **Local dependencies not in Cargo.lock:**

   ```bash
   # Update lockfile
   cargo update

   # Commit the updated Cargo.lock
   git add Cargo.lock
   git commit -m "Update Cargo.lock"
   ```

## Performance Tips

### Make Hooks Faster

1. **Only check staged files:**
   - The hook already does this for clippy, markdown, and links
   - Avoids checking unchanged code

2. **Use incremental compilation:**

   ```bash
   # Enable in ~/.cargo/config.toml
   [build]
   incremental = true
   ```

3. **Cache cargo artifacts:**
   - Already configured in repository
   - Speeds up subsequent runs

### When to Run Full CI Locally

Run `./scripts/run-local-ci.sh` before:

- Opening a pull request
- Pushing to main
- After major refactoring
- When in doubt about code quality

## Integration with CI/CD

### Pre-commit vs CI

| Check                | Pre-commit | CI  | Notes                                    |
| -------------------- | ---------- | --- | ---------------------------------------- |
| cargo fmt            | ✓          | ✓   | Fast, always run                         |
| cargo clippy         | ✓          | ✓   | Staged files only in pre-commit          |
| Tests                | ✗          | ✓   | Too slow for pre-commit                  |
| MSRV verification    | ✓          | ✓   | Only if config files changed (pre-commit)|
| AWK validation       | ✓          | ✓   | Only if workflows changed (pre-commit)   |
| Markdown linting     | ✓          | ✓   | Only if .md files changed (pre-commit)   |
| Link checking (full) | ✗          | ✓   | Offline only in pre-commit (warning)     |
| cargo-deny           | ✗          | ✓   | Security audits run in CI only           |
| Docker build         | ✗          | ✓   | Too slow for pre-commit                  |

### Philosophy

**Pre-commit:** Fast feedback (< 30 seconds), catches common mistakes

**CI:** Comprehensive checks (1-3 minutes), ensures production quality

## Customization

### Disable Specific Checks

Edit `.githooks/pre-commit` to comment out checks:

```bash
# Check 2: Clippy lints (disabled for this project)
# if [ -n "$STAGED_RS_FILES" ]; then
#   ...
# fi
```

### Add Custom Checks

Add to `.githooks/pre-commit`:

```bash
# Check N: Your custom check
echo "${BLUE}[N/7]${NC} Running custom check..."
if your-check-command; then
    check_pass "Custom check"
else
    check_fail "Custom check" "Explanation of what went wrong"
fi
```

## Related Documentation

- [Mandatory Workflow](../.llm/skills/mandatory-workflow.md) - Required checks before every commit
- [CI/CD Troubleshooting](../.llm/skills/ci-cd-troubleshooting.md) - Debugging CI failures
- [MSRV Management](../.llm/skills/msrv-and-toolchain-management.md) - Rust version management

## FAQ

### Q: Can I use `git commit -a` with hooks?

**A:** Yes, `git commit -a` (commit all modified files) works fine with hooks.
The hook checks all staged files, whether staged manually with `git add` or automatically with `-a`.

### Q: Do hooks run on `git commit --amend`?

**A:** Yes, by default. To skip: `git commit --amend --no-verify`

### Q: Do hooks run on merge commits?

**A:** Yes, hooks run on all commits including merges.
However, the hook only checks **new changes** (files you modified), not the entire merged state.

### Q: What if I need to commit broken code temporarily?

**A:** Use a feature branch:

```bash
# Create WIP branch
git checkout -b wip/my-feature

# Commit with hook bypass
git commit --no-verify -m "WIP: broken but saving progress"

# Later, fix and rebase/squash before merging
```

### Q: Can hooks auto-fix issues and commit them?

**A:** No. Hooks should never modify your working directory automatically.
They detect issues; you fix them manually or with auto-fix commands, then recommit.

## Support

If you encounter issues not covered in this guide:

1. Check `.githooks/pre-commit` source code for details
2. Run checks individually to isolate the problem
3. Review recent commits for similar issues
4. Ask in team chat or create an issue

## Historical Context

These hooks were enhanced after encountering three types of CI failures:

1. **Clippy format args** (commit `1c8ed3b`): `format!("{}", x)` instead of `format!("{x}")`
2. **MSRV mismatch** (commit `1c8ed3b`): `Dockerfile` using `1.88` instead of `1.88.0`
3. **AWK regex too strict** (commit `1c8ed3b`): Pattern didn't match `rust ignore` (space-separated)

The current hooks prevent all three categories of issues.
