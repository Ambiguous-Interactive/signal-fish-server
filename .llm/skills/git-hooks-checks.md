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

- Initial hook setup and permissions (see [Hook Installation](./git-hooks-installation.md))

---

## TL;DR

- Target execution time: < 5 seconds per hook
- Check only staged files (`git diff --cached --name-only`)
- Use NUL-delimited file lists (`git diff -z` + `xargs -0`) for path-safe tooling
- Gracefully skip checks if optional tools not installed
- Always document `git commit --no-verify` for emergencies
- Run all checks even on failure, report summary at end

---

## Canonical Code Samples

- Full pre-commit reference implementation:
  [pre-commit-fast.sh](../code-samples/git-hooks/pre-commit-fast.sh)
- Performance patterns and anti-patterns:
  [performance-patterns.sh](../code-samples/git-hooks/performance-patterns.sh)
- CI validation test patterns:
  [ci-hook-validation-tests.rs](../code-samples/git-hooks/ci-hook-validation-tests.rs)
- Debugging snippets:
  [debugging-snippets.sh](../code-samples/git-hooks/debugging-snippets.sh)

---

## Critical Pattern: Keep Markdown Lint Output Visible

Use output capture instead of suppressing stderr/stdout in failure paths.

```bash
if [ -x scripts/check-markdown.sh ]; then
  if ! MARKDOWN_OUTPUT=$(./scripts/check-markdown.sh 2>&1); then
    echo "[pre-commit] ERROR: Markdown linting failed"
    echo "$MARKDOWN_OUTPUT"
    echo "[pre-commit] Fix: ./scripts/check-markdown.sh fix"
    exit 1
  fi
fi
```

See full context:
[pre-commit-fast.sh](../code-samples/git-hooks/pre-commit-fast.sh).

---

## Testing Hooks

```bash
./.githooks/pre-commit && echo "PASS" || echo "FAIL"   # Direct execution
git commit --dry-run                                   # Through git path
git commit --no-verify -m "Bypass test"               # Verify bypass behavior
ls -la .githooks/pre-commit                            # Expect executable bit
git ls-files -s .githooks/pre-commit                   # Expect mode 100755
```

---

## Hook Validation in CI

Validate:

- Hook files exist
- Hooks and installer are executable
- `cargo test` invocations use `--locked` and `--` separator for multiple filters

Reference:
[ci-hook-validation-tests.rs](../code-samples/git-hooks/ci-hook-validation-tests.rs).

---

## Hook Debugging

```bash
if [ "${DEBUG:-0}" = "1" ]; then
  set -x
fi
set -euo pipefail
```

Common troubleshooting commands:

```bash
git config core.hooksPath              # Should output: .githooks
git config core.hooksPath .githooks    # Re-enable if needed
chmod +x .githooks/pre-commit
git update-index --chmod=+x .githooks/pre-commit
export PATH="$HOME/.cargo/bin:/usr/local/bin:$PATH"
```

See full debug examples:
[debugging-snippets.sh](../code-samples/git-hooks/debugging-snippets.sh).

---

## Best Practices

1. **Make hooks optional** — graceful degradation if tool not installed
2. **Always document `--no-verify`** — show bypass in error message
3. **Keep in sync with CI** — hooks should match CI validation steps

When to bypass: emergency hotfix, hook false positive, iterating on hook itself.

---

## See Also

- [Hook Installation](./git-hooks-installation.md) — Setup, permissions, team onboarding
- [Workflow Configuration](./github-actions-workflow-config.md) — CI/CD workflow patterns
- [CI Troubleshooting Scripts](./ci-cd-troubleshooting-scripts.md) — Debugging CI failures
- [Mandatory Workflow](./mandatory-workflow.md) — Required validation steps
