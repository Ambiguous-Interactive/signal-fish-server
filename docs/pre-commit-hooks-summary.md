# Pre-Commit Hooks Summary

## Current Model

Signal Fish hooks are sub-second, last-resort checks. The extensionless Git hook
files in `.githooks/` delegate to PowerShell 7 runners:

- `.githooks/pre-commit` -> `scripts/hooks/pre-commit.ps1`
- `.githooks/pre-push` -> `scripts/hooks/pre-push.ps1`

The runners force UTF-8 native process output and batch staged Git index reads
with an aggregate byte cap. Generated-file repairs verify Git object ids instead
of decoded text round trips.

## Why This Changed

The current failure in `pre-commit.txt` was a fast 484 ms Rust panic-pattern
failure on a production `.expect(...)` addition. The code had the documented
`SAFETY:` rationale and clippy allowance, so the root issue was policy drift
between the hook and the agent workflow guidance. The hook no longer adjudicates
`.expect()`/`.unwrap()` policy; local CI now runs the full panic policy so
agents catch those semantic failures before staging.

An earlier Windows hook incident ran full `cargo clippy --fix`, took 20.99
seconds, and still could not repair a cfg-specific unused variable. That remains
too slow for a git hook and too late in the workflow.

Clippy, tests, rustdoc, markdownlint, and broader policy checks now run in
agent workflows, `scripts/run-local-ci.sh`, and CI.

## Pre-Commit Checks

Production Rust commit path:

- explicit `panic!`/`todo!`/`unimplemented!`/`unreachable!` macro additions in
  production Rust source, excluding test-only files and `#[cfg(test)]` ranges

Metadata path:

- selected documentation version synchronization
- README badge style
- hook speed policy

Agent Skill validation remains in local and hosted CI to preserve the hook's sub-second budget.

## Pre-Push Checks

- pushed-file discovery across all introduced commits
- direct workflow script invocation policy
- hook speed policy

## Required Verification

```bash
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-commit.ps1 -Worktree
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-push.ps1 -Worktree
./scripts/run-local-ci.sh
```

## Readiness

```bash
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1 -Repair
```
