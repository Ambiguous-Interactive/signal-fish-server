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

- Initial hook setup and permissions (see [Git Hooks Installation](./git-hooks-installation.md))

---

## TL;DR

- Target execution time: < 1 second per hook
- Git hooks are last-resort guards only; do not run `cargo fmt`, `cargo clippy`,
  `cargo test`, `cargo doc`, `npm install`, `npm ci`, or `npx` in hooks
- Put semantic checks in the mandatory agent workflow, `scripts/run-local-ci.sh`,
  and CI
- Prefer PowerShell 7 (`pwsh`) for hook logic; keep extensionless `.githooks/*`
  files as tiny Git wrappers
- Check only staged or pushed files and use NUL-delimited Git output for path safety
- Force UTF-8 stdout/stderr decoding in native process helpers; never rely on
  platform-default code pages for generated-file comparisons
- PowerShell helpers must return exactly one result object. Assign or `[void]`
  any async `GetResult()`/native process completion calls so task result objects
  do not leak onto the pipeline and turn `$result` into `Object[]`.
- Batch staged blob reads with `git ls-files -s -z`, `git cat-file --batch-check`,
  and `git cat-file --batch`; cap aggregate bytes and avoid per-file `git show`
  loops in hooks
- Staged Rust panic checks must be production-only and line-number aware: exclude
  `*_test.rs`/`*_tests.rs` and skip additions inside staged `#[cfg(test)]` or
  direct test-function ranges.
- When production Rust files are staged, run only the code-path last-resort
  guards and stop; metadata guards run for non-production-Rust commits so mixed
  code/docs changes stay under budget.
- Verify auto-repaired generated files by Git object id, not decoded text
- Auto-repair only deterministic, fast generated artifacts that can be restaged safely
- Fail fast after the first concrete blocker so hooks stay under the <1 second target;
  rely on agent workflow/local CI for comprehensive reports

---

## Scope Matching: Staged Detection vs. Script Execution

When a pre-commit check gates on staged files, the script invocation MUST also
be scoped to those same files. Detecting staged files but running the checker
on ALL repository files is a scope mismatch bug.

**Pattern (CORRECT):**

```bash
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-commit.ps1
```

**Anti-pattern (WRONG):**

```bash
STAGED_FILES=$(git diff --cached --name-only --diff-filter=ACM | grep -E '\.ext$' || true)
if [ -n "$STAGED_FILES" ]; then
    scripts/check-foo.sh  # Runs on ALL files, not just staged!
fi
```

**Exceptions:** Cross-file consistency checks belong in local CI or CI unless they
are demonstrably sub-second and dependency-light.

## Slow Checks Belong Outside Hooks

Run before handoff:

```bash
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
./scripts/run-local-ci.sh
```

The pre-commit failure in `pre-commit.txt` is the reference incident: `cargo
clippy --fix` took 20.99s on Windows and still could not repair a cfg-specific
unused variable. That category must be caught by agent verification and CI, not
by a slow git hook.

---

## Canonical Code Samples

- Minimal Git wrapper reference:
  [pre-commit-fast.sh](../code-samples/git-hooks/pre-commit-fast.sh)
- Performance patterns and anti-patterns:
  [performance-patterns.sh](../code-samples/git-hooks/performance-patterns.sh)
- CI validation test patterns:
  [ci-hook-validation-tests.rs](../code-samples/git-hooks/ci-hook-validation-tests.rs)
- Debugging snippets:
  [debugging-snippets.sh](../code-samples/git-hooks/debugging-snippets.sh)

---

## Critical Pattern: Keep Linter Output Visible

Use output capture instead of suppressing stderr/stdout in agent workflow, local
CI, and CI failure paths. Do not put markdownlint in pre-commit.

```bash
if ! MARKDOWN_OUTPUT=$(./scripts/check-markdown.sh 2>&1); then
  echo "[local-ci] ERROR: Markdown linting failed"
  echo "$MARKDOWN_OUTPUT"
  echo "[local-ci] Fix: ./scripts/check-markdown.sh fix"
  exit 1
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
- Hooks delegate to `scripts/hooks/*.ps1`
- Hooks do not invoke slow semantic/install commands
- Runners parse staged/pushed paths from NUL-delimited Git output

Reference:
[ci-hook-validation-tests.rs](../code-samples/git-hooks/ci-hook-validation-tests.rs).

---

## Hook Debugging

```powershell
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1 -Repair
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

1. **Separate hooks from CI parity** — hooks are fast last-resort checks; local CI
   and GitHub CI own slow semantic validation
2. **Always document `--no-verify`** — show bypass in error message
3. **Keep hook dependencies minimal** — no network install or environment bootstrap
   in the commit path

When to bypass: emergency hotfix, hook false positive, iterating on hook itself.

---

## See Also

- [Git Hooks Installation](./git-hooks-installation.md) — Setup, permissions, team onboarding
- [GitHub Actions Workflow Config](./github-actions-workflow-config.md) — CI/CD workflow patterns
- [CI CD Troubleshooting Scripts](./ci-cd-troubleshooting-scripts.md) — Debugging CI failures
- [Mandatory Workflow](./mandatory-workflow.md) — Required validation steps
