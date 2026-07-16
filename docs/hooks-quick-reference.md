# Git Hooks Quick Reference

## Installation

```bash
./scripts/enable-hooks.sh
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1
```

## Hook Contract

Git hooks are last-resort checks and target sub-second execution. They do not run
`cargo fmt`, `cargo clippy`, `cargo test`, `cargo doc`, `npm install`, `npm ci`,
or `npx`. Hook runners use PowerShell 7, force UTF-8 native process output, and
batch staged Git index reads with an aggregate byte cap when a check needs
multiple file contents.

| Hook | Fast Checks |
|------|-------------|
| pre-commit | production Rust commits: explicit panic-macro additions only (`panic!`, `todo!`, `unimplemented!`, `unreachable!`) with test-code ranges excluded; matching metadata paths trigger documentation version sync, README badge style, and hook speed policy |
| pre-push | pushed-file discovery, workflow direct-script invocation policy, hook speed policy |

## Required Agent Checks

Run these before handoff or push:

```bash
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-commit.ps1 -Worktree
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-push.ps1 -Worktree
./scripts/run-local-ci.sh
```

`pre-commit.ps1 -Worktree` includes staged policy paths as well as worktree
changes and fails closed when staged policy content differs from the worktree.

## Hook Readiness

```bash
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1 -Repair
```

The default readiness check verifies `core.hooksPath`, executable bits, and
required tools (`git`, `pwsh`). Add `-WorkflowTools` to inventory optional
local-CI tools.

## Markdownlint

Markdownlint is fail-closed in local CI and CI, not in git hooks. Install the
pinned version with either:

```bash
npm install --save-dev --save-exact markdownlint-cli2@$(cat .markdownlint-version)
npm install -g markdownlint-cli2@$(cat .markdownlint-version)
```

## Bypass Hooks

```bash
git commit --no-verify
git push --no-verify
```

Use bypass only for hook false positives or emergency work; run local CI before
handoff.
