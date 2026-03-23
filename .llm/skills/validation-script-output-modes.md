# Skill: Validation Script Output Modes

<!--
  trigger: quiet, --quiet, silent, suppress output, validation script, success messages, summary
  | Output contracts for quiet validation scripts in CI, hooks, and local tooling
  | Infrastructure
-->

**Trigger**: When adding or reviewing `--quiet` or `-q` behavior in validation, hook, or CI shell scripts.

---

## When to Use

- Adding `--quiet` to repo validation scripts
- Deciding which messages should still print on warnings or failures
- Writing tests for script output in quiet vs normal modes

## When NOT to Use

- Interactive CLIs where users expect progress updates
- Application logging configuration outside shell validation scripts

---

## TL;DR

- Quiet mode suppresses banner, info, success, and passing summaries
- Warnings and errors still print in quiet mode
- Failure summaries still print in quiet mode
- `--help` and usage output should still print when explicitly requested

---

## Output Contract

Quiet mode is a presentation flag, not a safety flag. It should reduce noise without hiding actionable
information.

```bash
info() {
    if [ "$QUIET" = false ]; then
        printf '[INFO] %s\n' "$1"
    fi
}

warn() {
    printf '[WARN] %s\n' "$1"
}

fail() {
    printf '[FAIL] %s\n' "$1"
}
```

Use the same contract for banners and success lines:

- Suppress startup banners when `QUIET=true`
- Suppress pass/success lines when `QUIET=true`
- Never suppress warnings, errors, or actionable remediation text

---

## Summary Behavior

Quiet mode should still show enough output to explain a non-zero exit:

```bash
if [ "$ERRORS" -gt 0 ]; then
    echo "=========================================="
    printf 'FAILED: %d error(s)\n' "$ERRORS"
    exit 1
elif [ "$QUIET" = false ]; then
    echo "=========================================="
    printf 'ALL PASSED: %d check(s)\n' "$CHECKS_PASSED"
fi
```

This avoids the worst quiet-mode failure mode: a script exits `1` with no explanation.

---

## Edge Cases to Validate

- Quiet success: no banner, no info lines, no pass summary
- Quiet warning: warning text still visible
- Quiet failure: failure details and failed summary still visible
- `--quiet --help`: usage still prints because help was explicitly requested
- Quiet mode must not change exit codes or skip validations

Prefer data-driven tests that vary only:

1. `QUIET=true` vs `false`
2. success vs warning vs failure result
3. expected visible fragments

---

## Related Skills

- [Shell Scripting Patterns](./shell-scripting-patterns.md) — Strict mode, quoting, traps, argument parsing
- [GitHub Actions Bash Scripts](./github-actions-bash-scripts.md) — CI-focused Bash patterns and subshell pitfalls
- [Testing Error Message Quality](./testing-error-message-quality.md) — Diagnostic output expectations
