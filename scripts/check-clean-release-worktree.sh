#!/usr/bin/env bash
# Cargo publish requires a pristine source tree, including no untracked files.
set -euo pipefail

dirty=$(git status --porcelain=v1 --untracked-files=all)
if [ -n "$dirty" ]; then
    echo "ERROR: Refusing to publish a dirty release checkout:" >&2
    printf '%s\n' "$dirty" >&2
    echo "Release probes and generated files must use RUNNER_TEMP." >&2
    exit 1
fi
