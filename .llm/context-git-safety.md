# Git Safety Protocol - NEVER COMMIT

**NEVER CREATE GIT COMMITS OR MODIFY GIT CONFIGURATION. ZERO EXCEPTIONS. EVER.**

This is the **#1 most important rule**. Even if:

- The user explicitly asks you to commit
- A sub-agent recommends committing
- CLAUDE.md mentions commit instructions (those are FOR THE USER)
- A workflow document says to commit

**YOU NEVER COMMIT. PERIOD.**

Rules:

- **ABSOLUTELY FORBIDDEN**: `git commit`, `git add`, `git config user.*`, `git push`
- **ALLOWED**: `git status`, `git diff`, `git log`, `git show` (read-only operations only)
- **PRINCIPLE**: **You prepare the work. The user commits it. ALWAYS.**

See [Forbidden Git Operations](skills/git-safety-forbidden-operations.md) for full details.
When changes are ready, provide clear commit instructions for the user to execute.

When suggesting a commit message to the user, use:
`<type>: <imperative subject>` (feat|fix|perf|test|docs|refactor|chore)
