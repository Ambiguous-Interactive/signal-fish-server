---
name: version-control-workflow
description: Use Git safely and maintain Signal Fish repository hooks. Use for branches, status, diffs, staging, commits, pushes, forbidden destructive Git operations, hook installation, pre-commit or pre-push checks, hook performance, or hook regression tests.
---

<!-- markdownlint-disable MD013 -->

# Version Control Workflow

Preserve user work and inspect state before mutation. Use local `git` for branches, commits, and pushes; use the connected GitHub app for supported pull-request operations.

## Route the task

- Read [git-safety-safe-operations.md](references/git-safety-safe-operations.md) before mutating repository state.
- Read [git-safety-forbidden-operations.md](references/git-safety-forbidden-operations.md) before cleanup, rollback, or history operations.
- Read [git-hooks-installation.md](references/git-hooks-installation.md) for hook setup and cross-platform behavior.
- Read [git-hooks-checks.md](references/git-hooks-checks.md) for hook implementation, testing, and performance.
- Use [pre-commit-fast.sh](references/pre-commit-fast.sh), [performance-patterns.sh](references/performance-patterns.sh), [debugging-snippets.sh](references/debugging-snippets.sh), and [ci-hook-validation-tests.rs](references/ci-hook-validation-tests.rs) as hook examples only after reading the relevant hook guidance.

Keep hooks PowerShell-plus-Git only, cross-platform, and sub-second. Run all hook readiness and profiling commands from `AGENTS.md` after hook-adjacent changes.
