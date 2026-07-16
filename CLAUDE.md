# Claude AI - Repository Guidelines

See [`.llm/context.md`](.llm/context.md) for all AI agent guidelines.

For GitHub work, use local `git` for branches, commits, and pushes, and use the
connected VS Code GitHub extension / GitHub app for PRs, comments, reviewers,
review state, and other supported GitHub operations. Fall back to an
authenticated `gh` CLI only when the extension/app does not expose a required
capability (for example, detailed Actions logs). An unauthenticated `gh` CLI
must not block the workflow when the extension/app and Git remote are available.
