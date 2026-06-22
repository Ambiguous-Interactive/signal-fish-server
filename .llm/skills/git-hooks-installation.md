# Skill: Git Hooks Installation

<!--
  trigger: git hook, pre-commit hook, hook installation, executable permission, core.hooksPath, enable-hooks, githooks
  | Setting up git hooks with correct permissions, directory structure, and team onboarding
  | Infrastructure
-->

**Trigger**: When setting up git hooks in a repository, ensuring hooks are tracked with correct
permissions, or onboarding team members to use pre-commit checks.

---

## When to Use

- Creating new git hooks (pre-commit, pre-push, etc.)
- Installing hooks in a repository for team use
- Debugging permission denied errors on hook execution
- Writing cross-platform compatible hooks

## When NOT to Use

- Application code (hooks are for CI/CD automation)
- Performance-critical paths (hooks should be fast)

---

## TL;DR

**Required hook shape:**

1. `.githooks/*` files are tiny extensionless Git entrypoints.
2. Hook policy logic lives in `scripts/hooks/*.ps1` and runs on PowerShell 7+.
3. Slow semantic checks stay out of hooks; hooks target <1 second.
4. Use `scripts/check-hook-readiness.ps1 -Repair` for automated setup repair.

On Unix filesystems, hooks also need executable bits in both the filesystem and
Git index. Without both, hooks can work locally but fail for others.

Additional requirements:

- Store hooks in `.githooks/` (not `.git/hooks/`)
- Configure git: `git config core.hooksPath .githooks`
- Delegate policy logic to PowerShell 7 (`pwsh`) instead of writing new Bash hook logic

---

## Git Hook Permissions: The Two-Step Process

### The Problem: Permission Denied

```text
# Locally works fine, but on clone or in CI:
error: cannot run .git/hooks/pre-commit: Permission denied
```

**Root cause:** Git doesn't automatically track the executable bit on all systems (especially Windows).

### The Solution

#### Step 1: Prefer automated repair

```powershell
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1 -Repair
```

#### Step 2: Set filesystem permission manually when needed

```bash
chmod +x .githooks/pre-commit
```

#### Step 3: Tell Git to track the executable bit

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

### Common Mistake: Only Setting Filesystem Permission

```bash
# WRONG: Only sets filesystem permission
touch .githooks/pre-commit
chmod +x .githooks/pre-commit
# Works locally, but fails when others clone!

# CORRECT: Sets both permissions
touch .githooks/pre-commit
chmod +x .githooks/pre-commit
git update-index --chmod=+x .githooks/pre-commit
```

---

## Directory Structure

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

---

## Installation Script

**Keep `scripts/enable-hooks.sh` as a tiny setup helper:**

```bash
#!/usr/bin/env sh
set -eu

echo "Enabling git hooks..."

# Configure git to use .githooks directory
git config core.hooksPath .githooks

pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1 -Repair

echo "Git hooks enabled successfully"
```

**Make installation script executable when adding or changing it:**

```bash
chmod +x scripts/enable-hooks.sh
git update-index --chmod=+x scripts/enable-hooks.sh
```

---

## Team Onboarding

**Add to README.md or docs/development.md:**

````markdown
## Development Setup

### Enable Git Hooks

```bash
./scripts/enable-hooks.sh
```

This configures git to use pre-commit hooks that validate:

- Staged whitespace
- Panic-prone production Rust additions
- Generated skills index freshness with auto-repair
- `.llm` file size, README badge, and hook speed policy

**To bypass hooks (emergencies only):**

```bash
git commit --no-verify
```
````

---

## Cross-Platform Compatibility

### Hook Entrypoints

```bash
# CORRECT: extensionless Git hook wrapper delegates to PowerShell
#!/bin/sh
exec pwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass \
  -File scripts/hooks/pre-commit.ps1

# WRONG: putting policy logic directly in Bash
cargo test --locked --all-features
```

**Why:** Git hooks are extensionless entrypoints. Keep wrappers tiny and put
cross-platform logic in versioned PowerShell scripts.

### Platform-Specific Checks

Keep platform-specific and optional-tool checks out of git hooks. Use
`scripts/check-hook-readiness.ps1` for fast hook setup, `-WorkflowTools` for an
optional tool inventory, and `scripts/run-local-ci.sh` for slower workflow
validation.

### Windows Considerations

- Require Git and PowerShell 7+ (`pwsh`) as the only hook runtime dependencies
- Do not require Node, Cargo, devcontainers, WSL, or auto-installed toolchains in hooks
- Validate optional workflow tools with `check-hook-readiness.ps1 -WorkflowTools`
  or local CI, not in pre-commit

---

## Prevention Checklist

Before committing new hooks:

- [ ] Extensionless `.githooks/*` wrapper delegates to `pwsh`
- [ ] PowerShell runner uses strict mode and shared native process helpers
- [ ] Filesystem permission set: `chmod +x .githooks/pre-commit`
- [ ] Git index permission set: `git update-index --chmod=+x .githooks/pre-commit`
- [ ] Readiness repair passes: `pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1 -Repair`
- [ ] Hook tested locally: `pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-commit.ps1`
- [ ] Hook targets <1 second; profile with `SIGNAL_FISH_HOOK_PROFILE=1` if slower
- [ ] Installation script updated (if needed)
- [ ] Documentation updated (README or docs/development.md)

---

## See Also

- [Git Hooks Checks](./git-hooks-checks.md) — Pre-commit hook design, checks, testing, debugging
- [GitHub Actions Workflow Config](./github-actions-workflow-config.md) — CI/CD workflow patterns
- [Shell Scripting Patterns](./shell-scripting-patterns.md) — Shell scripting best practices
