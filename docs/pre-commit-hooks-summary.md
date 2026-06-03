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

The failure in `pre-commit.txt` showed that pre-commit was running full clippy.
On Windows, `cargo clippy --fix` took 20.99 seconds and still could not repair a
cfg-specific unused variable. That is too slow for a git hook and too late in
the workflow.

Clippy, tests, rustdoc, markdownlint, and broader policy checks now run in
agent workflows, `scripts/run-local-ci.sh`, and CI.

## Pre-Commit Checks

- staged whitespace errors
- panic-prone additions in production Rust source
- `.llm/skills/index.md` freshness with auto-repair
- staged `.llm/*.md` line-count limit
- README badge style
- hook speed policy

## Pre-Push Checks

- pushed-file discovery across all introduced commits
- direct workflow script invocation policy
- hook speed policy

## Required Verification

```bash
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
./scripts/run-local-ci.sh
```

## Readiness

```bash
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1 -Repair
```
