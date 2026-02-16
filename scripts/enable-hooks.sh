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
    log "  - Code formatting (cargo fmt)"
    log "  - Panic-prone pattern detection (no .unwrap(), panic!, etc.)"
    log "  - Markdown linting (if markdownlint-cli2 is installed)"
    log ""
    log "To install markdownlint-cli2: npm install -g markdownlint-cli2"
    log "To skip hooks (not recommended): git commit --no-verify"
fi
