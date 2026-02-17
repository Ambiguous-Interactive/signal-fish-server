#!/usr/bin/env sh
# enable-hooks.sh - Configure git to use the .githooks directory
#
# This script sets up git to use the project's .githooks directory for git hooks.
# Run this once after cloning the repository to enable pre-commit hooks that
# check for formatting and panic-prone patterns.
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
    log "Git hooks are now enabled. The following checks will run on commit:"
    log "  1. Code formatting (cargo fmt)"
    log "  2. Clippy lints (cargo clippy)"
    log "  3. Panic-prone patterns (no .unwrap(), panic!, etc.)"
    log "  4. MSRV consistency (when config files change)"
    log "  5. Workflow AWK validation (when workflows change)"
    log "  6. Markdown linting (when .md files change)"
    log "  7. Link checking (when .md files change, warning only)"
    log ""
    log "Optional dependencies:"
    log "  - markdownlint-cli2: npm install -g markdownlint-cli2"
    log "  - lychee: cargo install lychee"
    log ""
    log "Documentation: docs/git-hooks-guide.md"
    log "Run all CI checks: ./scripts/run-local-ci.sh"
    log "Skip hooks (not recommended): git commit --no-verify"
fi
