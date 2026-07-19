#!/usr/bin/env bash
# Compatibility entrypoint; canonical skill tooling is colocated with the manage-skills skill.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GENERATOR="$REPO_ROOT/.llm/skills/manage-skills/scripts/generate_skills_index.py"

if [ ! -f "$GENERATOR" ]; then
    echo "Skills index generator not found: $GENERATOR" >&2
    exit 1
fi

exec python3 "$GENERATOR" "$@"
