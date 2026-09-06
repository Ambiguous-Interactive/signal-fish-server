#!/usr/bin/env bash
# Runtime smoke checks for the devcontainers/ci build. This runs as `vscode`.
set -euo pipefail

expected_prefix="/home/vscode/.npm-global"
actual_prefix="$(npm config get prefix)"
workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "$actual_prefix" != "$expected_prefix" ]]; then
    echo "ERROR: npm global prefix is '$actual_prefix', expected '$expected_prefix'." >&2
    exit 1
fi

if [[ ! -w "$expected_prefix" ]]; then
    echo "ERROR: npm global prefix '$expected_prefix' is not writable by $(id -un)." >&2
    exit 1
fi

# These are named volumes in devcontainer.json. Post-create must transfer them
# from Docker's root ownership to the remote user before a plain local
# `npm install` writes dependency trees.
for dependency_dir in \
    "$workspace_root/node_modules" \
    "$workspace_root/clients/browser/node_modules"; do
    if [[ ! -d "$dependency_dir" || ! -w "$dependency_dir" ]]; then
        echo "ERROR: local npm dependency directory '$dependency_dir' is not writable by $(id -un)." >&2
        exit 1
    fi
done

# Prove the actual `npm install --global` operation succeeds without sudo and
# without registry access by installing a generated local fixture package.
smoke_dir="$(mktemp -d)"
trap 'rm -rf "$smoke_dir"' EXIT
printf '%s\n' '{"name":"signal-fish-npm-permission-smoke","version":"1.0.0","bin":{"signal-fish-npm-permission-smoke":"index.js"}}' >"$smoke_dir/package.json"
printf '%s\n' '#!/usr/bin/env node' 'process.stdout.write("npm prefix writable\n");' >"$smoke_dir/index.js"
chmod 0755 "$smoke_dir/index.js"
npm install --global --ignore-scripts "$smoke_dir"
signal-fish-npm-permission-smoke
npm uninstall --global --ignore-scripts signal-fish-npm-permission-smoke >/dev/null

for command_name in codex opencode nanocoder zai-mcp-server; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "ERROR: expected agent tool '$command_name' is not on PATH." >&2
        exit 1
    fi
done

test -x /usr/local/bin/github-mcp-server
test -x /home/vscode/.local/bin/signal-fish-zai-mcp-headers

python3 "$workspace_root/scripts/test_zai_mcp.py"

echo "Agent tooling runtime smoke checks passed."
