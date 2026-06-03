#!/usr/bin/env sh
# enable-hooks.sh - Configure git to use the .githooks directory
#
# This script sets up git to use the project's .githooks directory for git hooks.
# Run this once after cloning the repository to enable repository hooks
# (pre-commit and pre-push) for local policy validation.
#
# Usage:
#   ./scripts/enable-hooks.sh         # Enable hooks
#   ./scripts/enable-hooks.sh --quiet # Enable hooks silently

set -eu

QUIET=false
if [ "${1:-}" = "--quiet" ] || [ "${1:-}" = "-q" ]; then
    QUIET=true
fi

log() {
    if [ "$QUIET" = "false" ]; then
        echo "[hooks] $*"
    fi
}

# Ensure git is available
if ! command -v git >/dev/null 2>&1; then
    exit 0
fi

# Get repo root
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$REPO_ROOT"

# Check current hooks path
CURRENT=$(git config --local --get core.hooksPath 2>/dev/null || echo "")
DESIRED=".githooks"

if [ "$CURRENT" = "$DESIRED" ]; then
    # Already configured
    exit 0
fi

# Set local hooks path
if git config --local core.hooksPath "$DESIRED" 2>/dev/null; then
    log "Configured core.hooksPath to $DESIRED"
    log ""
    log "Git hooks are now enabled. Hooks are fast last-resort checks (<1s target)."
    log ""
    log "Pre-commit:"
    log "  - staged whitespace"
    log "  - production Rust panic-pattern additions"
    log "  - skills index auto-repair"
    log "  - .llm file size and README badge policy"
    log "  - hook speed policy"
    log ""
    log "Pre-push:"
    log "  - pushed workflow direct-script invocation policy"
    log "  - hook speed policy"
    log ""
    log "Slow semantic checks do NOT run in git hooks. Run before handoff:"
    log "  cargo fmt --check"
    log "  cargo clippy --locked --all-targets --all-features -- -D warnings"
    log "  cargo test --locked --all-features"
    log "  ./scripts/run-local-ci.sh"
    log ""
    log "Verify or repair hook setup:"
    log "  pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1"
    log "  pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1 -Repair"
    log ""
    log "Optional dependencies:"
    log "  - pwsh (required for hooks): https://learn.microsoft.com/powershell/scripting/install/installing-powershell"
    log "  - shellcheck: apt-get install shellcheck"
    log "  - markdownlint-cli2 (pinned, local): npm install --save-dev --save-exact markdownlint-cli2@\$(cat .markdownlint-version)"
    log "    or global: npm install -g markdownlint-cli2@\$(cat .markdownlint-version)"
    log "  - lychee: cargo install lychee"
    log ""
    log "Documentation: docs/git-hooks-guide.md"
    log "Run all CI checks: ./scripts/run-local-ci.sh"
    log "Skip hooks (not recommended): git commit --no-verify"
fi
