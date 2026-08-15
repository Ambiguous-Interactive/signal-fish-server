#!/usr/bin/env bash
# Print NUL-delimited tracked Cargo.lock paths containing the local server package.

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || {
    echo "ERROR: list-release-lockfiles.sh must run inside a Git worktree." >&2
    exit 1
}

tracked_inventory=$(mktemp)
cleanup() {
    rm -f "$tracked_inventory"
}
trap cleanup EXIT

if ! git -C "$REPO_ROOT" ls-files -z -- ':(glob)**/Cargo.lock' > "$tracked_inventory"; then
    echo "ERROR: Could not inventory tracked Cargo.lock files." >&2
    exit 1
fi

lockfiles=()
lockfile_count=0
while IFS= read -r -d '' lockfile; do
    lockfiles+=("$REPO_ROOT/$lockfile")
    lockfile_count=$((lockfile_count + 1))
done < "$tracked_inventory"

if [ "$lockfile_count" -eq 0 ]; then
    echo "ERROR: No tracked Cargo.lock files were found." >&2
    exit 1
fi

awk -v mode=list -f "$SCRIPT_DIR/release-lockfile-packages.awk" -- "${lockfiles[@]}" \
    | while IFS= read -r -d '' lockfile; do
        printf '%s\0' "${lockfile#"$REPO_ROOT/"}"
    done
