#!/usr/bin/env bash
# Emit the short-lived header object expected by Codex's http_headers_helper.
# The API key stays in the process environment and is never written to disk.
set -euo pipefail

if [[ -z "${Z_AI_API_KEY:-}" ]]; then
    echo 'Z_AI_API_KEY is empty; set it in the agent environment and restart the agent.' >&2
    exit 1
fi

node -e '
    process.stdout.write(JSON.stringify({
        Authorization: `Bearer ${process.env.Z_AI_API_KEY}`,
    }));
'
