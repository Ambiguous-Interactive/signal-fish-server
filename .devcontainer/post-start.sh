#!/usr/bin/env bash
# Signal Fish Server — Post-start refresh (runs on EVERY container start/launch)
#
# Keeps the terminal agent CLIs current even when Docker reuses a cached
# container between launches, and re-applies the Codex GitHub MCP wiring.
# A registry version-check fast path makes the refresh a ~1s no-op when the
# CLIs are already current, so container launch stays fast. Everything here is
# best-effort: a failure must never block the container from opening.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=lib-agent-tools.sh
. "$SCRIPT_DIR/lib-agent-tools.sh"

if is_truthy "${SIGNAL_FISH_SKIP_AGENT_REFRESH:-0}"; then
    echo "[post-start] SIGNAL_FISH_SKIP_AGENT_REFRESH is set; skipping agent CLI refresh."
    exit 0
fi

echo ""
echo "============================================"
echo "  Signal Fish Server — Refreshing agent CLIs"
echo "============================================"
echo ""

# Route npm global installs through the user-owned prefix (no sudo, ever).
if ! configure_user_npm_prefix; then
    echo "[post-start] Warning: could not configure the user-owned npm prefix."
fi

if ! install_codex_cli; then
    echo "[post-start] Warning: Codex CLI refresh failed; continuing."
fi

if ! install_opencode_cli; then
    echo "[post-start] Warning: OpenCode CLI refresh failed; continuing."
fi

if ! install_nanocoder_cli; then
    echo "[post-start] Warning: Nanocoder CLI refresh failed; continuing."
fi

if ! configure_codex_github_mcp; then
    echo "[post-start] Warning: Codex GitHub MCP configuration failed; continuing."
fi

echo ""
echo "[post-start] Agent CLI refresh complete (fast path: reinstall only when outdated)."
