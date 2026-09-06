#!/usr/bin/env bash
# Signal Fish Server — Post-start refresh (runs on EVERY container start/launch)
#
# Keeps the terminal agent CLIs and Z.AI Vision MCP current even when Docker
# reuses a cached container, and re-applies the Codex GitHub + Z.AI MCP wiring.
# A bulk registry version-check makes the refresh a low-cost no-op when the
# tools are already current, so container launch stays fast. Everything here is
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

# Local configuration goes first so registry latency never delays MCP wiring.
if ! configure_user_npm_prefix; then
    echo "[post-start] Warning: could not configure the user-owned npm prefix."
fi

if ! configure_codex_mcp_servers; then
    echo "[post-start] Warning: Codex MCP configuration failed; continuing."
fi

if ! refresh_agent_npm_tools; then
    echo "[post-start] Warning: one or more agent npm tools failed to refresh; continuing."
fi

echo ""
echo "[post-start] Agent CLI refresh complete (fast path: reinstall only when outdated)."
