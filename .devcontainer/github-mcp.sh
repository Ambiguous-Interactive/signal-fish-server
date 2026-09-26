#!/usr/bin/env bash
# github-mcp.sh - Launch the pinned GitHub MCP server with credentials from
# .env.local when the harness environment has none.
#
# Every harness config (.vscode/mcp.json, .mcp.json, opencode.json, and the
# Codex block written by .devcontainer/lib-agent-tools.sh) execs this wrapper
# instead of the bare binary. Observed with PR #638: an opencode session whose
# environment carried an empty GITHUB_PERSONAL_ACCESS_TOKEN made the server
# device-flow on every call, even though .env.local held a working key.
#
# Precedence matches .devcontainer/zai-mcp.mjs: the file key wins over a
# stale inherited value, and both file names (GITHUB_PERSONAL_ACCESS_TOKEN,
# GITHUB_MCP_PAT) are accepted. With no key in either place the server starts
# exactly as before (its interactive device-authorization fallback), so the
# wrapper never hard-fails a credential-free container. The token is only
# exported to the server process: never echoed, logged, or written.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENV_FILE="${SCRIPT_DIR}/../.env.local"

token="${GITHUB_PERSONAL_ACCESS_TOKEN:-}"
if [ -f "$ENV_FILE" ]; then
    # Only key names and the masked value traverse this pipeline; nothing is
    # printed. `|| true` keeps a key-less file (CI, fresh clones) non-fatal.
    file_token="$(
        grep -E '^(export )?(GITHUB_PERSONAL_ACCESS_TOKEN|GITHUB_MCP_PAT)=' \
            "$ENV_FILE" 2>/dev/null |
            tail -n 1 | cut -d= -f2- | tr -d '\r' |
            sed -e "s/^'//" -e "s/'$//" -e 's/^"//' -e 's/"$//' || true
    )"
    if [ -n "$file_token" ]; then
        token="$file_token"
    fi
fi
export GITHUB_PERSONAL_ACCESS_TOKEN="$token"

exec github-mcp-server "$@"
