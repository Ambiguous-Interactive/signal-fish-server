# Skill: Git Hooks Setup and Maintenance

<!--
  trigger: git hook, pre-commit hook, git hooks, hook installation, executable permission, hook maintenance
  | Creating and maintaining git hooks with proper permissions
  | Infrastructure
-->

**Trigger**: When setting up pre-commit hooks, ensuring hook permissions, or debugging git hook failures.

---

## When to Use

- Creating new git hooks (pre-commit, pre-push, etc.)
- Installing hooks in repository
- Debugging permission denied errors
- Ensuring hooks are tracked in git correctly
- Writing cross-platform compatible hooks
- Validating hook execution before commit

## When NOT to Use

- Application code (hooks are for CI/CD automation)
- Complex validation logic (move to separate scripts)
- Performance-critical paths (hooks should be fast)

---

## TL;DR

**Two Permissions Required:**

1. **Filesystem permission**: `chmod +x .githooks/pre-commit`
2. **Git index permission**: `git update-index --chmod=+x .githooks/pre-commit`

**Without both, hooks work locally but fail for others (or in CI).**

**Hook Best Practices:**

- Store hooks in `.githooks/` directory (not `.git/hooks/`)
- Configure git: `git config core.hooksPath .githooks`
- Make hooks fast (< 5 seconds) with clear progress output
- Allow bypass for emergencies: `git commit --no-verify`
- Test hooks locally before pushing

---

## Git Hook Permissions: The Two-Step Process

### The Problem: Permission Denied

**Symptom:**

```text
# Locally works fine, but on clone or in CI:
error: cannot run .git/hooks/pre-commit: Permission denied
```

**Root cause:** Git doesn't automatically track the executable bit on all systems (especially Windows).

### The Solution: Two-Step Permission Setup

#### Step 1: Set filesystem permission

```bash
chmod +x .githooks/pre-commit
```

#### Step 2: Tell Git to track the executable bit

```bash
git update-index --chmod=+x .githooks/pre-commit
```

**Verify it's set correctly:**

```bash
git ls-files -s .githooks/pre-commit
# Should show: 100755 <hash> 0 .githooks/pre-commit
#              ^^^^^^ = executable
# NOT:         100644 = regular file
```

**Both steps are required:**

- `chmod +x` - Allows local execution
- `git update-index --chmod=+x` - Tracks executable bit in git
- When others clone, git restores the executable bit

### Common Mistake: Only Setting Filesystem Permission

```bash
# ❌ WRONG: Only sets filesystem permission
touch .githooks/pre-commit
chmod +x .githooks/pre-commit
git add .githooks/pre-commit
git commit -m "Add pre-commit hook"

# Works locally, but fails when others clone!
```

```bash
# ✅ CORRECT: Sets both permissions
touch .githooks/pre-commit
chmod +x .githooks/pre-commit
git update-index --chmod=+x .githooks/pre-commit
git add .githooks/pre-commit
git commit -m "Add pre-commit hook"

# Works for everyone
```

---

## Hook Installation

### Directory Structure

**Use custom hooks directory (not .git/hooks/):**

```text
.githooks/
├── pre-commit          # Runs before commit
├── pre-push            # Runs before push
└── commit-msg          # Validates commit message
```

**Why `.githooks/` instead of `.git/hooks/`:**

- `.git/` is not tracked by git (local only)
- `.githooks/` is tracked and shared with team
- Central location for all repository hooks

### Installation Script

**Create `scripts/enable-hooks.sh`:**

```bash
#!/usr/bin/env bash
set -euo pipefail

echo "Enabling git hooks..."

# Configure git to use .githooks directory
git config core.hooksPath .githooks

# Ensure hooks are executable (filesystem permission)
chmod +x .githooks/*

# Ensure hooks have executable bit in git (git permission)
for hook in .githooks/*; do
  git update-index --chmod=+x "$hook"
done

echo "✓ Git hooks enabled successfully"
echo ""
echo "Configured hooks:"
ls -la .githooks/

echo ""
echo "To bypass hooks (emergencies only):"
echo "  git commit --no-verify"
```

**Make installation script executable:**

```bash
chmod +x scripts/enable-hooks.sh
git update-index --chmod=+x scripts/enable-hooks.sh
git add scripts/enable-hooks.sh
git commit -m "Add hook installation script"
```

### Team Onboarding

**Add to README.md or docs/development.md:**

```markdown
## Development Setup

### Enable Git Hooks

```bash

./scripts/enable-hooks.sh

```

This configures git to use pre-commit hooks that validate:

- Code formatting (`cargo fmt --check`)
- Markdown linting (if markdownlint-cli2 is installed)
- Link checking (if lychee is installed)
- Panic-prone patterns

**To bypass hooks (emergencies only):**

```bash
git commit --no-verify
```

---

## Pre-Commit Hook Design

### Example: Fast Pre-Commit Hook

**`.githooks/pre-commit`:**

```bash
#!/usr/bin/env bash
#
# Pre-commit hook for Signal Fish Server
# Runs fast checks before each commit
#
# To bypass: git commit --no-verify

set -euo pipefail

echo "[pre-commit] Running pre-commit checks..."

# Track failures (but continue to run all checks)
FAILURES=0

# ==============================================================================
# 1. Rust code formatting
# ==============================================================================

echo "[pre-commit] Checking Rust code formatting..."
if ! cargo fmt --check >/dev/null 2>&1; then
  echo "[pre-commit] ERROR: Code formatting issues detected"
  echo "[pre-commit] Fix: cargo fmt"
  FAILURES=$((FAILURES + 1))
fi

# ==============================================================================
# 2. Panic-prone patterns
# ==============================================================================

echo "[pre-commit] Checking for panic-prone patterns..."
if [ -f scripts/check-no-panics.sh ]; then
  if ! ./scripts/check-no-panics.sh >/dev/null 2>&1; then
    echo "[pre-commit] ERROR: Panic-prone patterns detected"
    echo "[pre-commit] Fix: Review and fix reported issues"
    FAILURES=$((FAILURES + 1))
  fi
fi

# ==============================================================================
# 3. Markdown linting (if available)
# ==============================================================================

if command -v markdownlint-cli2 >/dev/null 2>&1; then
  echo "[pre-commit] Checking markdown files..."

  # Get staged markdown files
  STAGED_MD=$(git diff --cached --name-only --diff-filter=ACM | grep '\.md$' || true)

  if [ -n "$STAGED_MD" ]; then
    # shellcheck disable=SC2086
    if ! markdownlint-cli2 $STAGED_MD >/dev/null 2>&1; then
      echo "[pre-commit] ERROR: Markdown linting failed"
      echo "[pre-commit] Fix: ./scripts/check-markdown.sh fix"
      FAILURES=$((FAILURES + 1))
    fi
  fi
else
  echo "[pre-commit] Skipping markdown check (markdownlint-cli2 not installed)"
fi

# ==============================================================================
# 4. Link checking (if available, offline mode for speed)
# ==============================================================================

if command -v lychee >/dev/null 2>&1; then
  echo "[pre-commit] Checking links (offline mode)..."

  # Get staged markdown files
  STAGED_MD=$(git diff --cached --name-only --diff-filter=ACM | grep '\.md$' || true)

  if [ -n "$STAGED_MD" ]; then
    # Use offline mode for speed (only validates internal links)
    # shellcheck disable=SC2086
    if ! lychee --offline --config .lychee.toml $STAGED_MD >/dev/null 2>&1; then
      echo "[pre-commit] ERROR: Link checking failed"
      echo "[pre-commit] Fix: ./scripts/check-links-fast.sh"
      FAILURES=$((FAILURES + 1))
    fi
  fi
else
  echo "[pre-commit] Skipping link check (lychee not installed)"
fi

# ==============================================================================
# Summary and exit
# ==============================================================================

echo ""
if [ "$FAILURES" -eq 0 ]; then
  echo "[pre-commit] ✓ All checks passed"
  exit 0
else
  echo "[pre-commit] ✗ $FAILURES check(s) failed"
  echo ""
  echo "To bypass hooks (emergencies only):"
  echo "  git commit --no-verify"
  exit 1
fi
```

**Key features:**

1. ✅ **Clear progress output** - Shows what's being checked
2. ✅ **Graceful degradation** - Skips checks if tools not installed
3. ✅ **Fast execution** - Uses offline mode for link checking
4. ✅ **Actionable errors** - Shows fix commands
5. ✅ **Bypass option** - Documents `--no-verify` for emergencies
6. ✅ **Only staged files** - Checks only files being committed

---

## Hook Performance

### Keep Hooks Fast

**Target execution time:** < 5 seconds

**Performance strategies:**

```bash
# 1. Check only staged files
STAGED_FILES=$(git diff --cached --name-only --diff-filter=ACM)

# 2. Use offline mode when possible
lychee --offline $STAGED_FILES

# 3. Skip slow checks if tool not installed
if command -v slow_tool >/dev/null 2>&1; then
  slow_tool --check  # Only run if installed
fi

# 4. Parallel execution for independent checks
cargo fmt --check &
FMT_PID=$!

./scripts/check-panics.sh &
PANICS_PID=$!

# Wait for both
wait $FMT_PID || FAILURES=$((FAILURES + 1))
wait $PANICS_PID || FAILURES=$((FAILURES + 1))

# 5. Cache expensive operations
if [ ! -f .git/hook-cache/last-check ]; then
  : # First run or cache cleared
fi
```

### Performance Anti-Patterns

```bash
# ❌ BAD: Checks all files every time
cargo clippy --all-targets --all-features  # Slow!

# ❌ BAD: Network requests block commit
lychee '**/*.md'  # Checks external links (slow!)

# ❌ BAD: No progress output
cargo test  # User doesn't know what's happening

# ✅ GOOD: Fast, local-only checks with progress
echo "[pre-commit] Running fast checks..."
cargo fmt --check  # Fast, local only
```

---

## Testing Hooks

### Test Hook Execution

**Before committing hook changes:**

```bash
# 1. Test hook directly
./.githooks/pre-commit
echo "Exit code: $?"

# 2. Test with git commit (dry run)
git add .
git commit --dry-run

# 3. Test actual commit
touch test-file.txt
git add test-file.txt
git commit -m "Test commit"

# 4. Test bypass
git commit --no-verify -m "Bypass test"
```

### Test Permission Setup

**Verify permissions are tracked:**

```bash
# Check filesystem permission
ls -la .githooks/pre-commit
# Should show: -rwxr-xr-x (executable)

# Check git index permission
git ls-files -s .githooks/pre-commit
# Should show: 100755 (executable in git)

# Simulate clone
cd /tmp
git clone /path/to/repo test-clone
cd test-clone

# Should work without setup
./.githooks/pre-commit
```

---

## Hook Validation in CI

### Prevent Permission Issues

**Add test to validate hook permissions:**

```rust
// tests/ci_config_tests.rs

#[test]
fn test_git_hooks_are_executable() {
    let githooks_dir = repo_root().join(".githooks");

    if !githooks_dir.exists() {
        // No hooks directory - skip test
        return;
    }

    for entry in std::fs::read_dir(&githooks_dir).unwrap() {
        let path = entry.unwrap().path();

        if path.is_file() && path.extension().is_none() {
            // Git hook files typically have no extension
            let metadata = std::fs::metadata(&path).unwrap();

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = metadata.permissions().mode();
                let is_executable = mode & 0o111 != 0;

                assert!(
                    is_executable,
                    "{} is not executable.\n\
                     Fix:\n\
                       chmod +x {}\n\
                       git update-index --chmod=+x {}",
                    path.display(),
                    path.display(),
                    path.display()
                );
            }
        }
    }
}

#[test]
fn test_hook_installation_script_exists() {
    let script = repo_root().join("scripts/enable-hooks.sh");

    assert!(
        script.exists(),
        "scripts/enable-hooks.sh is required for hook installation.\n\
         Create it to simplify team onboarding."
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(&script).unwrap();
        let mode = metadata.permissions().mode();
        let is_executable = mode & 0o111 != 0;

        assert!(
            is_executable,
            "scripts/enable-hooks.sh must be executable.\n\
             Fix: chmod +x scripts/enable-hooks.sh && \
             git update-index --chmod=+x scripts/enable-hooks.sh"
        );
    }
}
```

---

## Cross-Platform Compatibility

### Shebang Line

**Use `#!/usr/bin/env bash` (not `#!/bin/bash`):**

```bash
# ✅ CORRECT: Works on macOS, Linux, BSD
#!/usr/bin/env bash
set -euo pipefail

# ❌ WRONG: Assumes bash location
#!/bin/bash
```

**Why:** `/bin/bash` may not exist on all systems (e.g., FreeBSD uses `/usr/local/bin/bash`).

### Platform-Specific Checks

**Handle platform differences gracefully:**

```bash
# Check if command exists before using
if command -v markdownlint-cli2 >/dev/null 2>&1; then
  markdownlint-cli2 '**/*.md'
else
  echo "Skipping markdown check (markdownlint-cli2 not installed)"
fi

# Platform-specific paths
if [ "$(uname)" = "Darwin" ]; then
  # macOS-specific logic
  CLIPBOARD=pbcopy
else
  # Linux-specific logic
  CLIPBOARD=xclip
fi
```

### Windows Considerations

**Hooks don't work the same on Windows:**

- Git Bash (MINGW) - Hooks work with bash scripts
- PowerShell - Hooks need .ps1 extension
- WSL - Works like Linux

**Recommendation:** Document that hooks work best on Unix-like systems (macOS, Linux, WSL).

---

## Hook Debugging

### Enable Debug Output

**Add debug mode to hook:**

```bash
#!/usr/bin/env bash

# Enable debug mode with: DEBUG=1 git commit
if [ "${DEBUG:-0}" = "1" ]; then
  set -x
fi

set -euo pipefail
```

**Usage:**

```bash
# Normal execution
git commit -m "message"

# Debug execution
DEBUG=1 git commit -m "message"
```

### Common Issues

#### Issue 1: Hook not running

```bash
# Check if hooks are enabled
git config core.hooksPath
# Should output: .githooks

# Re-enable if needed
git config core.hooksPath .githooks
```

#### Issue 2: Permission denied

```bash
# Check permissions
ls -la .githooks/pre-commit
git ls-files -s .githooks/pre-commit

# Fix permissions
chmod +x .githooks/pre-commit
git update-index --chmod=+x .githooks/pre-commit
```

#### Issue 3: Command not found

```bash
# Check PATH in hook
echo "$PATH" | tr ':' '\n'

# Ensure PATH includes tool locations
export PATH="$HOME/.cargo/bin:$PATH"
export PATH="/usr/local/bin:$PATH"
```

---

## Best Practices

### 1. Make Hooks Optional Tools

**Don't require hooks to be installed:**

```bash
# ✅ GOOD: Hook is optional
if command -v markdownlint-cli2 >/dev/null 2>&1; then
  markdownlint-cli2 '**/*.md'
else
  echo "Skipping markdown check (install: npm install -g markdownlint-cli2)"
fi

# ❌ BAD: Forces tool installation
markdownlint-cli2 '**/*.md' || exit 1
```

**Why:** Not all developers may have or need all tools. Graceful degradation is better.

### 2. Provide Bypass Option

**Always document `--no-verify`:**

```bash
echo ""
echo "To bypass hooks (emergencies only):"
echo "  git commit --no-verify"
```

**When to bypass:**

- Emergency hotfix needed immediately
- Hook has false positive that needs investigation
- Working on hook itself (iterative development)

**When NOT to bypass:**

- "I'll fix it later" (creates technical debt)
- "Tests are slow" (make tests faster)
- "I know what I'm doing" (everyone makes mistakes)

### 3. Keep Hooks in Sync with CI

**Hooks should match CI validation:**

```yaml
# Pre-commit hook
cargo fmt --check
cargo clippy

# CI workflow (.github/workflows/ci.yml)
- run: cargo fmt --check
- run: cargo clippy
```

**Why:** Developers should catch issues locally that would fail in CI.

### 4. Document Hook Requirements

**In README.md or docs/development.md:**

```markdown
## Git Hooks

### Installation

```bash

./scripts/enable-hooks.sh

```

### What Hooks Check

- **pre-commit**: Code formatting, markdown linting, link checking
- **pre-push**: (future) full test suite

### Optional Tools

Hooks work best with these tools installed:

- `markdownlint-cli2`: `npm install -g markdownlint-cli2`
- `lychee`: `cargo install lychee`

Hooks gracefully skip checks if tools aren't installed.

### Bypassing Hooks

Only in emergencies:

```bash
git commit --no-verify
```

---

## Prevention Checklist

Before committing new hooks:

- [ ] Shebang uses `#!/usr/bin/env bash`
- [ ] Strict mode: `set -euo pipefail`
- [ ] Filesystem permission set: `chmod +x .githooks/pre-commit`
- [ ] Git index permission set: `git update-index --chmod=+x .githooks/pre-commit`
- [ ] Hook tested locally: `./.githooks/pre-commit`
- [ ] Hook executes in < 5 seconds
- [ ] Clear progress output during execution
- [ ] Graceful degradation if tools not installed
- [ ] Bypass documented: `git commit --no-verify`
- [ ] Installation script updated (if needed)
- [ ] Documentation updated (README or docs/development.md)
- [ ] CI test validates hook permissions

---

## Related Skills

- [`github-actions-best-practices`](./github-actions-best-practices.md) — CI/CD workflow patterns
- [awk-and-shell-scripting](./awk-and-shell-scripting.md) — Shell scripting best practices
- [ci-cd-troubleshooting](./ci-cd-troubleshooting.md) — Debugging permission issues
- [mandatory-workflow](./mandatory-workflow.md) — Required validation steps

---

## Summary

**Git hooks require two permissions:**

1. **Filesystem permission** (`chmod +x`) - Allows local execution
2. **Git index permission** (`git update-index --chmod=+x`) - Tracks executable bit in git

**Without both, hooks work locally but fail for others.**

**Hook best practices:**

- Store in `.githooks/` (not `.git/hooks/`)
- Configure: `git config core.hooksPath .githooks`
- Make fast (< 5 seconds) with clear output
- Allow bypass for emergencies: `git commit --no-verify`
- Test locally before pushing
- Validate permissions in CI tests
- Keep in sync with CI validation
- Document installation and requirements
