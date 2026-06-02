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
- Safe deterministic recovery is allowed. The pre-commit hook regenerates and
  stages `.llm/skills/index.md` when skill inputs changed, then verifies the
  repaired index entry by Git object id.

The failure captured in `pre-commit.txt` is the reference incident: a Windows
pre-commit run spent 20.99 seconds in `cargo clippy --fix` and still could not
repair a cfg-specific unused variable. Clippy is therefore enforced by agent
workflow and CI, not by git hooks.

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
- optional local-CI tools are visible when installed

## What Runs

### Pre-Commit

The pre-commit hook runs `scripts/hooks/pre-commit.ps1`. When production Rust
files are staged, it runs only the code-path guards needed for last-resort
safety and budget:

- staged diff whitespace via `git diff --cached --check`
- new panic-prone production Rust additions in `src/**/*.rs`, excluding test
  files and staged `#[cfg(test)]`/test-function ranges

When no production Rust files are staged, it also checks lightweight repository
metadata guards:

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
./scripts/run-local-ci.sh
```

`scripts/run-local-ci.sh` owns slower policy checks including markdownlint,
workflow hygiene, doc/changelog consistency, doc policy tests, Dockerfile
portability, advisory checks, and README badge checks.

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

If a hook exceeds the sub-second budget, inspect the hook source for slow
commands, per-file process fanout, or unbatched staged-blob reads. Move slow
semantic checks to `scripts/run-local-ci.sh` or CI. The static test suite
rejects common slow commands in `.githooks/*` and `scripts/hooks/*.ps1`.

### Bypass

```bash
git commit --no-verify
git push --no-verify
```

Use bypass only for hook false positives or emergency work. Run local CI before
handoff or merge.
