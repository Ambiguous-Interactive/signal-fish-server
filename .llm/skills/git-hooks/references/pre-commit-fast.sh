#!/bin/sh
#
# Minimal Git pre-commit wrapper for Signal Fish Server.
# Keep policy logic in scripts/hooks/pre-commit.ps1 so native Linux, macOS, and
# Windows users share the same versioned implementation.

set -eu

if command -v pwsh >/dev/null 2>&1; then
    exec pwsh -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass \
        -File scripts/hooks/pre-commit.ps1
fi

echo "[pre-commit] ERROR: PowerShell 7+ (pwsh) is required for Signal Fish hooks." >&2
echo "[pre-commit] Install from: https://learn.microsoft.com/powershell/scripting/install/installing-powershell" >&2
exit 1
