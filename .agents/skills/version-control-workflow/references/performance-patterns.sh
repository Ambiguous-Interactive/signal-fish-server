#!/bin/sh

# GOOD: keep extensionless Git hooks as tiny wrappers.
exec pwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass \
  -File scripts/hooks/pre-commit.ps1

# GOOD: hook runners inspect staged paths only and use NUL delimiters.
git diff --cached --name-only -z --diff-filter=ACDMR -- '*.rs'

# GOOD: fail fast after a concrete blocker.
if ! run_fast_staged_guard; then
  exit 1
fi

# BAD: semantic tools are too slow for last-resort hooks.
cargo fmt --check
cargo clippy --all-targets --all-features
cargo test --locked --all-features

# BAD: optional package managers and network checks do not belong in hooks.
npm install
npx markdownlint-cli2 '**/*.md'
lychee '**/*.md'

# GOOD: run slow semantic checks before handoff, in local CI, or in GitHub CI.
./scripts/run-local-ci.sh
