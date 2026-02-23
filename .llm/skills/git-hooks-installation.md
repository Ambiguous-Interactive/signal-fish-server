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

**Two Permissions Required:**

1. **Filesystem permission**: `chmod +x .githooks/pre-commit`
2. **Git index permission**: `git update-index --chmod=+x .githooks/pre-commit`

**Without both, hooks work locally but fail for others (or in CI).**

Additional requirements:

- Store hooks in `.githooks/` (not `.git/hooks/`)
- Configure git: `git config core.hooksPath .githooks`
- Use `#!/usr/bin/env bash` shebang (portable across platforms)

---

## Git Hook Permissions: The Two-Step Process

### The Problem: Permission Denied

```text
# Locally works fine, but on clone or in CI:
error: cannot run .git/hooks/pre-commit: Permission denied
```

**Root cause:** Git doesn't automatically track the executable bit on all systems (especially Windows).

### The Solution

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

### Common Mistake: Only Setting Filesystem Permission

```bash
# WRONG: Only sets filesystem permission
touch .githooks/pre-commit
chmod +x .githooks/pre-commit
git add .githooks/pre-commit
git commit -m "Add pre-commit hook"
# Works locally, but fails when others clone!

# CORRECT: Sets both permissions
touch .githooks/pre-commit
chmod +x .githooks/pre-commit
git update-index --chmod=+x .githooks/pre-commit
git add .githooks/pre-commit
git commit -m "Add pre-commit hook"
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

echo "Git hooks enabled successfully"
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

- Code formatting (`cargo fmt --check`)
- Markdown linting (if pinned markdownlint-cli2 version is installed)
- Panic-prone patterns

**To bypass hooks (emergencies only):**

```bash
git commit --no-verify
```
````

---

## Cross-Platform Compatibility

### Shebang Line

```bash
# CORRECT: Works on macOS, Linux, BSD
#!/usr/bin/env bash
set -euo pipefail

# WRONG: Assumes bash location
#!/bin/bash
```

**Why:** `/bin/bash` may not exist on all systems (e.g., FreeBSD uses `/usr/local/bin/bash`).

### Platform-Specific Checks

```bash
# Check if command exists before using
if [ -x scripts/check-markdown.sh ]; then
  ./scripts/check-markdown.sh
else
  echo "Skipping markdown check (check-markdown.sh not found)"
fi

# Platform-specific paths
if [ "$(uname)" = "Darwin" ]; then
  CLIPBOARD=pbcopy
else
  CLIPBOARD=xclip
fi
```

### Windows Considerations

- Git Bash (MINGW) — hooks work with bash scripts
- PowerShell — hooks need `.ps1` extension
- WSL — works like Linux

Hooks work best on Unix-like systems (macOS, Linux, WSL).

---

## Prevention Checklist

Before committing new hooks:

- [ ] Shebang uses `#!/usr/bin/env bash`
- [ ] Strict mode: `set -euo pipefail`
- [ ] Filesystem permission set: `chmod +x .githooks/pre-commit`
- [ ] Git index permission set: `git update-index --chmod=+x .githooks/pre-commit`
- [ ] Hook tested locally: `./.githooks/pre-commit`
- [ ] Hook executes in < 5 seconds
- [ ] Installation script updated (if needed)
- [ ] Documentation updated (README or docs/development.md)

---

## See Also

- [git-hooks-checks](./git-hooks-checks.md) — Pre-commit hook design, checks, testing, debugging
- [GitHub-actions-best-practices](./github-actions-workflow-config.md) — CI/CD workflow patterns
- [shell-scripting-patterns](./shell-scripting-patterns.md) — Shell scripting best practices
