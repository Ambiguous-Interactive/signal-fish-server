# Skill: Git Safety — Safe Operations and Commit Instructions

<!--
  trigger: git status, git diff, git log, git read-only, commit instructions, git scenarios
  | Safe git operations and how to provide commit instructions to users
  | Core
-->

**Trigger**: When you need to perform git read operations or provide commit instructions to the user.

---

## When to Use

- Running read-only git commands (status, diff, log, show)
- Providing commit instructions to the user instead of committing yourself
- Explaining git scenarios to the user
- Checking git configuration (read-only)

---

## When NOT to Use

- See [git-safety-forbidden-operations](./git-safety-forbidden-operations.md) for what is strictly forbidden

---

## What You CAN Do

### Safe Git Operations

These operations are **safe** and **encouraged**:

```bash
# Status and inspection
git status
git status --short
git status --porcelain

# Viewing changes
git diff
git diff --cached
git diff HEAD
git diff file.rs
git show commit-hash

# History and logs
git log
git log --oneline
git log --graph
git log -p file.rs

# Branch information (read-only)
git branch
git branch -r
git branch --show-current
git rev-parse HEAD

# Checking configuration (read-only)
git config --list
git config --get user.name
git config --get user.email

# Checking remote information
git remote -v
git remote show origin
```

---

## What to Tell the User Instead

When you need git operations performed, provide **clear instructions** for the user:

**Instead of committing:**

````markdown
I've made the following changes:

- Modified: src/server.rs (added validation)
- Modified: tests/integration_tests.rs (added test cases)

To commit these changes:

```bash
git add src/server.rs tests/integration_tests.rs
git commit -m "feat: add input validation to server

- Add validation for room codes
- Add comprehensive test coverage
"
```
````

**Instead of configuring git:**

````markdown
To ensure commits are attributed correctly, please verify your git configuration:

```bash
git config user.name   # Should be your name
git config user.email  # Should be your email
```

If these need to be set:

```bash
git config user.name "Your Name"
git config user.email "your.email@example.com"
```
````

---

## Common Scenarios

### Scenario 1: User Asks "Can you commit these changes?"

**BAD Response:**

```bash
git add file.rs
git commit -m "fix: something"
```

**GOOD Response:**

````markdown
I've completed the changes. Here's what was modified:

- src/file.rs: Fixed the validation logic

To commit these changes:

```bash
git add src/file.rs
git commit -m "fix: correct validation logic in file.rs

- Handle edge case for empty input
- Add boundary checks
"
```

You can modify the commit message as needed before running the command.
````

### Scenario 2: User's Git Identity Appears Wrong

**BAD Response:**

```bash
git config user.name "Correct Name"
git config user.email "correct@email.com"
```

**GOOD Response:**

````markdown
I noticed the git configuration may need updating. Currently:

- user.name: [current value]
- user.email: [current value]

If this is incorrect, you can update it with:

```bash
# For this repository only:
git config user.name "Your Name"
git config user.email "your.email@example.com"

# Or globally for all repositories:
git config --global user.name "Your Name"
git config --global user.email "your.email@example.com"
```
````

### Scenario 3: CI/CD Requires a Commit

**BAD Response:**
"I'll commit the changes so CI can run."

```bash
git commit -m "CI fixes"
```

**GOOD Response:**

````markdown
The CI/CD failures have been fixed. For CI to validate the fixes, these changes need to be committed:

- File 1: Description
- File 2: Description

Suggested commit command:

```bash
git add file1 file2
git commit -m "fix: CI/CD issues - [description]

[Detailed explanation]
"
git push origin branch-name
```

Would you like to review the changes before committing?
````

### Scenario 4: Following a Workflow that Mentions Commits

**BAD Response:**
Following a documented workflow blindly and executing `git commit` commands.

**GOOD Response:**
Recognize that documented workflows are **templates** for the user to execute, not instructions for you to automate.
Present the commands to the user instead.

---

## Agent Checklist

Before ANY git operation:

- [ ] Is this a read-only operation? (status, log, diff, show) → Safe
- [ ] Does this modify repository state? (add, commit, push) → STOP
- [ ] Does this modify git configuration? (config --set, config user.*) → STOP
- [ ] Has user **explicitly** requested this exact operation in current session? → Only then consider
- [ ] Can I provide instructions instead of executing? → Do this

---

## ALWAYS

- Use read-only git operations (`git status`, `git diff`, `git log`)
- Provide clear commit instructions to user
- Let user control when and what gets committed
- Respect user's git identity and configuration

## PRINCIPLE

**You prepare the work. The user commits it.**

Your role is to:

1. Make changes to files
2. Verify changes are correct (cargo check, clippy, test)
3. Provide clear instructions for user to commit
4. Answer questions about git operations

The user's role is to:

1. Review your changes
2. Stage files (`git add`)
3. Create commits (`git commit`)
4. Push to remote (`git push`)
5. Manage their git identity and configuration

---

## Related Skills

- [git-safety-forbidden-operations](./git-safety-forbidden-operations.md) — What is absolutely forbidden and why
- [agentic-workflow-patterns](./agentic-workflow-patterns.md) — Agent workflow that integrates with user commits
- [agent-self-review-checklist](./agent-self-review-checklist.md) — Pre-commit verification (user commits after)
- [GitHub-actions-best-practices](./github-actions-workflow-config.md) — CI/CD patterns
