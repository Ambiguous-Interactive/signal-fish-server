# Skill: Git Safety Protocol

<!-- trigger: git, commit, git-config, git-commit, git-push, version-control, repository-safety | Critical safety rules for git operations | Core -->

**Trigger**: Before performing ANY git operation (commit, config, push, etc.)

---

## When to Use
- **ALWAYS** - Before any git command that modifies repository state or configuration
- Before staging files (`git add`)
- Before creating commits (`git commit`)
- Before modifying git configuration (`git config`)
- Before pushing to remote (`git push`)
- When user requests git operations

---

## When NOT to Use
- Read-only git operations (git status, git log, git diff, git show) are safe
- Git operations explicitly requested by the user in their CLAUDE.md or project configuration

---

## ⛔ CRITICAL: Absolutely Forbidden Operations

These operations are **STRICTLY PROHIBITED** under ALL circumstances:

### 🚫 NEVER Create Git Commits

**❌ FORBIDDEN:**
```bash
git commit
git commit -m "message"
git commit --amend
git commit --fixup
git commit --all
```text

**✅ ALLOWED (Read-only):**
```bash
git status
git diff
git diff --cached
git log
git show
```

**Rationale:**
- Git commits record **your** identity, not the user's
- Commits create permanent history that may misattribute work
- Users must control when and what gets committed
- Commit messages should reflect user's intent and voice

### 🚫 NEVER Modify Git Configuration

**❌ FORBIDDEN:**
```bash
git config user.name
git config user.email
git config --global user.name
git config --global user.email
git config --local user.name
git config --system user.name
```bash

**✅ ALLOWED (Read-only):**
```bash
git config --list
git config user.name  # Reading only, not setting
git config --get user.email
```

**Rationale:**
- Git configuration defines user identity for all future commits
- Changing user.name or user.email misattributes authorship
- Configuration changes persist beyond current session
- User's git identity is personal and should never be modified by automation

### 🚫 NEVER Stage Files Without Explicit User Request

**❌ FORBIDDEN (unless explicitly requested):**
```bash
git add .
git add -A
git add file.rs
git stage file.rs
```bash

**✅ ALLOWED:**
- Only if user explicitly says "stage these files" or "git add X"
- Only the specific files user requested

**Rationale:**
- Staging is the first step toward committing
- User should control what gets included in commits
- Prevents accidental inclusion of sensitive files

---

## What You CAN Do

### ✅ Safe Git Operations

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

### ✅ What to Tell the User Instead

When you need git operations performed, provide **clear instructions** for the user:

**Instead of committing:**
```markdown
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
```text
```

**Instead of configuring git:**
```markdown
To ensure commits are attributed correctly, please verify your git configuration:
```

```bash
git config user.name   # Should be your name
git config user.email  # Should be your email
```

If these need to be set:

```bash
git config user.name "Your Name"
git config user.email "your.email@example.com"
```

---

## Exception: User-Provided Co-Authorship

If the user has **explicitly configured** in their CLAUDE.md or project documentation that commits should include:
- `Co-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>`

This is **acceptable** because:
- User has explicitly requested it in durable configuration
- It's a co-author attribution, not primary author
- User remains the primary author (git user.name/email unchanged)

**Example from this repository's CLAUDE.md:**
> The CLAUDE.md file contains git commit instructions that include Co-Authored-By attribution.
> This is user-configured and acceptable to follow.

---

## Common Scenarios

### Scenario 1: User Asks "Can you commit these changes?"

**❌ BAD Response:**
```bash
git add file.rs
git commit -m "fix: something"
```

**✅ GOOD Response:**
```markdown
I've completed the changes. Here's what was modified:
- src/file.rs: Fixed the validation logic

To commit these changes:
```

```bash
git add src/file.rs
git commit -m "fix: correct validation logic in file.rs

- Handle edge case for empty input
- Add boundary checks
"
```

You can modify the commit message as needed before running the command.

### Scenario 2: User's Git Identity Appears Wrong

**❌ BAD Response:**
```bash
git config user.name "Correct Name"
git config user.email "correct@email.com"
```bash

**✅ GOOD Response:**
```markdown
I noticed the git configuration may need updating. Currently:
- user.name: [current value]
- user.email: [current value]

If this is incorrect, you can update it with:
```

```bash
# For this repository only:
git config user.name "Your Name"
git config user.email "your.email@example.com"

# Or globally for all repositories:
git config --global user.name "Your Name"
git config --global user.email "your.email@example.com"
```

### Scenario 3: CI/CD Requires a Commit

**❌ BAD Response:**
"I'll commit the changes so CI can run."
```bash
git commit -m "CI fixes"
```

**✅ GOOD Response:**
```markdown
The CI/CD failures have been fixed. For CI to validate the fixes, these changes need to be committed:

- File 1: Description
- File 2: Description

Suggested commit command:
```

```bash
git add file1 file2
git commit -m "fix: CI/CD issues - [description]

[Detailed explanation]
"
git push origin branch-name
```

Would you like to review the changes before committing?

### Scenario 4: Following a Workflow that Mentions Commits

**❌ BAD Response:**
Following a documented workflow blindly and executing `git commit` commands.

**✅ GOOD Response:**
Recognize that documented workflows are **templates** for the user to execute, not instructions for you to automate. Present the commands to the user instead.

---

## Rationalizations to Reject

| Excuse | Why It's Wrong | Correct Action |
|--------|---------------|----------------|
| "The workflow says to commit" | Workflows are user templates, not automation scripts | Present commit command to user |
| "The user asked me to commit" | User's git identity will be used, not yours | Provide commit command for user to run |
| "I need to test in CI" | CI can run on uncommitted changes locally | Use local validation instead |
| "I'll use --author to set correct attribution" | Still modifies git history without user control | Never modify git history |
| "It's just git config --local" | Still persists beyond current session | Never modify git config |
| "The commit message includes Co-Authored-By" | Primary author is still you, not user | User must create the commit |
| "I'll commit then let user amend it" | Creates extra work and wrong history | User creates commit correctly first time |

---

## Agent Checklist

Before ANY git operation:

- [ ] Is this a read-only operation? (status, log, diff, show) → ✅ Safe
- [ ] Does this modify repository state? (add, commit, push) → 🛑 STOP
- [ ] Does this modify git configuration? (config --set, config user.*) → 🛑 STOP
- [ ] Has user **explicitly** requested this exact operation in current session? → Only then consider
- [ ] Can I provide instructions instead of executing? → ✅ Do this

---

## Error Messages to Watch For

If you see these patterns in your own thinking or prompts, **STOP IMMEDIATELY**:

- "Let me commit these changes"
- "I'll create a commit with message..."
- "Let me configure git to use..."
- "I'll stage these files and commit..."
- "I'll set up git config for..."
- "Let me commit this fix so CI can run..."

**Correct response:** Provide instructions to user instead.

---

## Integration with Other Skills

This git safety protocol integrates with:

- **[agentic-workflow-patterns](./agentic-workflow-patterns.md)** - The workflow mentions "commit" but means user commits
- **[agent-self-review-checklist](./agent-self-review-checklist.md)** - Verification before presenting commit instructions
- **[mandatory-workflow](./mandatory-workflow.md)** - User's mandatory workflow, not automated
- **[github-actions-best-practices](./github-actions-best-practices.md)** - CI/CD validation happens after user commits

**Key principle:** You verify and prepare changes; user commits them.

---

## Summary

### ⛔ NEVER
- Create commits (`git commit`)
- Modify git configuration (`git config user.*`)
- Stage files without explicit user request (`git add`)
- Push to remote (`git push`)
- Modify git history (`git rebase`, `git reset`, `git amend`)

### ✅ ALWAYS
- Use read-only git operations (`git status`, `git diff`, `git log`)
- Provide clear commit instructions to user
- Let user control when and what gets committed
- Respect user's git identity and configuration

### 🎯 PRINCIPLE
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

- [agentic-workflow-patterns](./agentic-workflow-patterns.md) - Agent workflow that integrates with user commits
- [agent-self-review-checklist](./agent-self-review-checklist.md) - Pre-commit verification (user commits after)
- [mandatory-workflow](./mandatory-workflow.md) - User's mandatory workflow requirements
- [github-actions-best-practices](./github-actions-best-practices.md) - CI/CD patterns
