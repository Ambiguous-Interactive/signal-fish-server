# Git Hooks Guide - Signal Fish Server

This project treats git hooks as fast, last-resort safeguards. They catch cheap
staged or pushed-file mistakes, but they do not replace agent verification,
local CI, or GitHub CI.

## Design Contract

- Target hook runtime: less than 1 second.
- Hook logic is PowerShell 7 (`pwsh`) for native Linux, macOS, and Windows use.
- Extensionless files in `.githooks/` are tiny POSIX wrappers because Git
  executes hook files directly.
- Hooks must not run `cargo fmt`, `cargo clippy`, `cargo test`, `cargo doc`,
  `npm install`, `npm ci`, or `npx`.
- Hooks must not bootstrap tools from the network.
- Staged and pushed path discovery uses NUL-delimited Git output.
- PowerShell native-process helpers set stdout/stderr decoding to UTF-8.
- Checks that read multiple staged blobs batch Git index access with
  `git ls-files -s -z`, `git cat-file --batch-check`, and
  `git cat-file --batch`; avoid per-file `git show` loops in hooks.
- Batched blob reads cap aggregate bytes before loading content.
- Safe deterministic recovery is allowed. The pre-commit hook regenerates
  `.llm/skills/index.md` when skill inputs changed, mirrors it to the worktree
  when there are no unstaged edits, then verifies the repaired index entry by
  Git object id.

Recent hook incidents reinforced the split: hooks catch only cheap staged or
pushed-file policy failures, while agents, local CI, and CI run semantic checks.
The hook stays to explicit panic macros and metadata guards; `.expect()` /
`.unwrap()`, clippy, tests, markdownlint, and broader documentation policy stay
outside git hooks.

## Installation

```bash
./scripts/enable-hooks.sh
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1
```

To repair hook setup:

```bash
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1 -Repair
```

The readiness check verifies:

- `core.hooksPath` is `.githooks`
- `.githooks/pre-commit` and `.githooks/pre-push` exist
- hook executable bits are correct in the Git index
- required tools `git` and `pwsh` are available
- optional local-CI tools only when `-WorkflowTools` is supplied

## What Runs

### Pre-Commit

The pre-commit hook runs `scripts/hooks/pre-commit.ps1`. When production Rust
files are staged, it runs only the code-path guards needed for last-resort
safety and budget, unless metadata paths also changed:

- new explicit `panic!`, `todo!`, `unimplemented!`, and `unreachable!` macro
  additions in `src/**/*.rs`, excluding test-only files and staged
  `#[cfg(test)]`/test-function ranges.

Production `.expect()` and `.unwrap()` policy is enforced by
`scripts/check-no-panics.sh` in agent workflow, local CI, and CI, not by the git
hook.

When matching paths changed, it also checks lightweight repository metadata
guards, even in mixed Rust/metadata commits:

- generated skills index freshness when skill inputs changed, with auto-repair
- staged `.llm/*.md` files stay at or below 300 lines
- README Shields.io badges use `style=for-the-badge`
- hook source does not reintroduce slow semantic or install commands

### Pre-Push

The pre-push hook runs `scripts/hooks/pre-push.ps1` and checks:

- changed files across all commits introduced by the push, including new branch pushes
- workflow `run:` lines do not invoke local scripts directly through executable bits
- hook source does not reintroduce slow semantic or install commands

## Required Verification Before Handoff

Agents and developers must run semantic checks outside hooks:

```bash
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-commit.ps1 -Worktree
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-push.ps1 -Worktree
./scripts/run-local-ci.sh
```

`pre-commit.ps1 -Worktree` checks unstaged worktree policy paths and staged
policy paths. If a staged policy path differs from the worktree, the preflight
fails closed because the real git hook validates the staged snapshot.

`scripts/run-local-ci.sh` owns slower policy checks including hook readiness,
worktree hook preflights, LLM file-size/example policies, markdownlint,
workflow hygiene, doc/changelog consistency, doc policy tests, Dockerfile
portability, advisory checks, and README badge checks.

## Measured Runtime Budget

The hook enforces a 1,000 ms target (`HookBudgetMs`) as a warning with the
slowest checks named; `-EnforceBudget` turns it into a failure. The target
applies to warm runs of normal paths — a real staged commit and the
`-Worktree` preflight. The first run after opening a repository pays a one-time
Git index refresh whose cost belongs to the filesystem, not the hook, and can
exceed the target on cold container volumes.

Per-check timing diagnostics are opt-in and deterministic:

```bash
SIGNAL_FISH_HOOK_PROFILE=1 pwsh -NoLogo -NoProfile -NonInteractive \
    -File scripts/hooks/pre-commit.ps1 -Worktree
```

On Windows, set the variable for the session first:
`$env:SIGNAL_FISH_HOOK_PROFILE = "1"`.

Every executed check or phase then prints one
`[pre-commit] PROFILE: <check> <ms>ms`
line. The runtime contract test
`test_pre_commit_profile_diagnostics_contract_when_pwsh_available` keeps this
boundary working: profiling stays silent unless requested, and every check
reports its own timing so slow checks identify themselves on any machine.

Reference baseline (Linux devcontainer, 2026-08-23, warm):

| Scenario | Total | Changed-file discovery | Panic-pattern scan |
| --- | --- | --- | --- |
| Staged commit path (one production Rust file) | ~400 ms | ~65 ms | ~90 ms |
| Worktree preflight, clean tree | ~750 ms | ~540-630 ms | skipped |
| Worktree preflight, small panic-relevant diff | ~900 ms | ~540 ms | ~115 ms |
| PowerShell startup alone | ~225 ms | — | — |

Worktree discovery is dominated by `git status` refreshing the index over
policy paths; its cost scales with directory count on the local filesystem
(~4-5 ms per directory on the reference container volume), not with file
count. Measured dead ends on that volume: `core.untrackedCache` made status
25-45% slower, and the fsmonitor daemon is unsuitable for ephemeral containers.
Do not reintroduce either without new evidence.

macOS and Windows baselines are pending; record them with the same command and
table shape before revisiting the budget.

## Markdownlint

Markdownlint remains fail-closed in local CI and CI. It is intentionally not run
inside git hooks.

Install the pinned version locally:

```bash
npm install --save-dev --save-exact markdownlint-cli2@$(cat .markdownlint-version)
```

Or install the pinned version globally:

```bash
npm install -g markdownlint-cli2@$(cat .markdownlint-version)
```

## Troubleshooting

### PowerShell Missing

Hooks require PowerShell 7+ (`pwsh`):

```bash
pwsh -NoLogo -NoProfile -NonInteractive -Command '$PSVersionTable.PSVersion'
```

Install from Microsoft’s PowerShell documentation.

### Hook Setup Drift

```bash
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1 -Repair
git config --local core.hooksPath
git ls-files --stage .githooks/pre-commit .githooks/pre-push
```

Expected hook index mode is `100755`.

### Slow Hook

If a hook exceeds the sub-second budget, profile it with
`SIGNAL_FISH_HOOK_PROFILE=1` (see "Measured Runtime Budget") and inspect the
slowest reported check for slow commands, per-file process fanout, or
unbatched staged-blob reads. Move slow semantic checks to
`scripts/run-local-ci.sh` or CI. The static test suite rejects common slow
commands in `.githooks/*` and `scripts/hooks/*.ps1`.

### Bypass

```bash
git commit --no-verify
git push --no-verify
```

Use bypass only for hook false positives or emergency work. Run local CI before
handoff or merge.
