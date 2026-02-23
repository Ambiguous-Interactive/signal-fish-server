# Skill: Git Hooks Check Implementations

<!--
  trigger: pre-commit check, hook validation, hook testing, hook debugging,
  hook performance, hook ci, cargo fmt hook, markdownlint hook
  | Pre-commit hook check design, performance patterns, testing, CI validation,
  and debugging | Infrastructure
-->

**Trigger**: When writing specific check implementations for pre-commit hooks, optimizing hook
performance, debugging hook failures, or validating hook permissions in CI.

---

## When to Use

- Writing check implementations for `.githooks/pre-commit`
- Optimizing hook execution time
- Testing hooks before committing
- Validating hook permissions in CI tests
- Debugging hook failures

## When NOT to Use

- Initial hook setup and permissions (see [git-hooks-installation](./git-hooks-installation.md))

---

## TL;DR

- Target execution time: < 5 seconds per hook
- Check only staged files (`git diff --cached --name-only`)
- Gracefully skip checks if optional tools not installed
- Always document `git commit --no-verify` for emergencies
- Run all checks even on failure, report summary at end

---

## Example: Fast Pre-Commit Hook

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
FAILURES=0

# 1. Rust code formatting
echo "[pre-commit] Checking Rust code formatting..."
if ! cargo fmt --check >/dev/null 2>&1; then
  echo "[pre-commit] ERROR: Code formatting issues detected"
  echo "[pre-commit] Fix: cargo fmt"
  FAILURES=$((FAILURES + 1))
fi

# 2. Panic-prone patterns
echo "[pre-commit] Checking for panic-prone patterns..."
if [ -f scripts/check-no-panics.sh ]; then
  if ! ./scripts/check-no-panics.sh >/dev/null 2>&1; then
    echo "[pre-commit] ERROR: Panic-prone patterns detected"
    FAILURES=$((FAILURES + 1))
  fi
fi

# 3. Markdown linting (if pinned version is available)
if [ -x scripts/check-markdown.sh ]; then
  echo "[pre-commit] Checking markdown files..."
  STAGED_MD=$(git diff --cached --name-only --diff-filter=ACM | grep '\.md$' || true)
  if [ -n "$STAGED_MD" ]; then
    if ! ./scripts/check-markdown.sh >/dev/null 2>&1; then
      echo "[pre-commit] ERROR: Markdown linting failed"
      echo "[pre-commit] Fix: ./scripts/check-markdown.sh fix"
      FAILURES=$((FAILURES + 1))
    fi
  fi
else
  echo "[pre-commit] Skipping markdown check (scripts/check-markdown.sh not found)"
fi

# 4. Link checking (offline mode for speed)
if command -v lychee >/dev/null 2>&1; then
  echo "[pre-commit] Checking links (offline mode)..."
  STAGED_MD=$(git diff --cached --name-only --diff-filter=ACM | grep '\.md$' || true)
  if [ -n "$STAGED_MD" ]; then
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

# Summary and exit
echo ""
if [ "$FAILURES" -eq 0 ]; then
  echo "[pre-commit] All checks passed"
  exit 0
else
  echo "[pre-commit] $FAILURES check(s) failed"
  echo ""
  echo "To bypass hooks (emergencies only):"
  echo "  git commit --no-verify"
  exit 1
fi
```

---

## Hook Performance

### Performance Strategies

```bash
# 1. Check only staged files
STAGED_FILES=$(git diff --cached --name-only --diff-filter=ACM)

# 2. Use offline mode when possible
lychee --offline $STAGED_FILES

# 3. Skip slow checks if tool not installed
if command -v slow_tool >/dev/null 2>&1; then
  slow_tool --check
fi

# 4. Parallel execution for independent checks
cargo fmt --check &
FMT_PID=$!
./scripts/check-panics.sh &
PANICS_PID=$!
wait $FMT_PID || FAILURES=$((FAILURES + 1))
wait $PANICS_PID || FAILURES=$((FAILURES + 1))
```

### Anti-Patterns

```bash
# BAD: Checks all files every time
cargo clippy --all-targets --all-features  # Slow!

# BAD: Network requests block commit
lychee '**/*.md'  # Checks external links (slow!)

# BAD: No progress output
cargo test  # User doesn't know what's happening

# GOOD: Fast, local-only checks with progress
echo "[pre-commit] Running fast checks..."
cargo fmt --check
```

---

## Testing Hooks

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

```bash
# Check filesystem permission
ls -la .githooks/pre-commit
# Should show: -rwxr-xr-x (executable)

# Check git index permission
git ls-files -s .githooks/pre-commit
# Should show: 100755 (executable in git)
```

---

## Hook Validation in CI

```rust
// tests/ci_config_tests.rs

#[test]
fn test_git_hooks_are_executable() {
    let githooks_dir = repo_root().join(".githooks");
    if !githooks_dir.exists() { return; }

    for entry in std::fs::read_dir(&githooks_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() && path.extension().is_none() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&path).unwrap().permissions().mode();
                assert!(
                    mode & 0o111 != 0,
                    "{} is not executable.\nFix:\n  chmod +x {}\n  git update-index --chmod=+x {}",
                    path.display(), path.display(), path.display()
                );
            }
        }
    }
}

#[test]
fn test_hook_installation_script_exists() {
    let script = repo_root().join("scripts/enable-hooks.sh");
    assert!(script.exists(), "scripts/enable-hooks.sh is required for hook installation.");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&script).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "scripts/enable-hooks.sh must be executable.");
    }
}
```

---

## Hook Debugging

```bash
#!/usr/bin/env bash

# Enable debug mode with: DEBUG=1 git commit
if [ "${DEBUG:-0}" = "1" ]; then
  set -x
fi
set -euo pipefail
```

### Common Issues

**Hook not running:**

```bash
git config core.hooksPath
# Should output: .githooks
git config core.hooksPath .githooks  # Re-enable if needed
```

**Permission denied:**

```bash
chmod +x .githooks/pre-commit
git update-index --chmod=+x .githooks/pre-commit
```

**Command not found:**

```bash
export PATH="$HOME/.cargo/bin:$PATH"
export PATH="/usr/local/bin:$PATH"
```

---

## Best Practices

1. **Make hooks optional** — graceful degradation if tool not installed
2. **Always document `--no-verify`** — show bypass in error message
3. **Keep in sync with CI** — hooks should match CI validation steps
4. **Document requirements** — list optional tools in README

When to bypass: emergency hotfix, hook false positive, iterating on hook itself.
When NOT to bypass: "I'll fix it later", "tests are slow", "I know what I'm doing".

---

## See Also

- [git-hooks-installation](./git-hooks-installation.md) — Setup, permissions, directory structure, team onboarding
- [GitHub-actions-best-practices](./github-actions-workflow-config.md) — CI/CD workflow patterns
- [ci-cd-troubleshooting-scripts](./ci-cd-troubleshooting-scripts.md) — Debugging CI failures
- [mandatory-workflow](./mandatory-workflow.md) — Required validation steps
