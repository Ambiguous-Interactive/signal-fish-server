#!/usr/bin/env bash
# Emit the exact release-preparation file inventory as NUL-delimited paths.

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || {
    echo "ERROR: list-release-files.sh must run inside a Git worktree." >&2
    exit 1
}
cd "$REPO_ROOT"

release_files=(
    .llm/context.md
    CHANGELOG.md
    Cargo.toml
    docs/getting-started.md
    docs/library-usage.md
    fuzz/Cargo.toml
)
for release_file in "${release_files[@]}"; do
    if ! git cat-file -e "HEAD:$release_file" 2>/dev/null; then
        echo "ERROR: Canonical release file is not tracked at HEAD: $release_file" >&2
        exit 1
    fi
done

printf '%s\0' "${release_files[@]}"

bash "$SCRIPT_DIR/list-release-lockfiles.sh"
