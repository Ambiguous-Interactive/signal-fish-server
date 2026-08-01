#!/usr/bin/env bash
# Print NUL-delimited tracked Cargo.lock paths containing the local server package.

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || {
    echo "ERROR: list-release-lockfiles.sh must run inside a Git worktree." >&2
    exit 1
}

lockfiles=()
while IFS= read -r -d '' lockfile; do
    lockfiles+=("$REPO_ROOT/$lockfile")
done < <(git -C "$REPO_ROOT" ls-files -z -- ':(glob)**/Cargo.lock')

if [ "${#lockfiles[@]}" -eq 0 ]; then
    echo "ERROR: No tracked Cargo.lock files were found." >&2
    exit 1
fi

awk -v mode=list -f "$SCRIPT_DIR/release-lockfile-packages.awk" -- "${lockfiles[@]}" \
    | while IFS= read -r -d '' lockfile; do
        printf '%s\0' "${lockfile#"$REPO_ROOT/"}"
    done
