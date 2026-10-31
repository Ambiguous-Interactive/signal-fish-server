#!/usr/bin/env bash
# Emit the exact release-preparation file inventory as NUL-delimited paths.

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || {
    echo "ERROR: list-release-files.sh must run inside a Git worktree." >&2
    exit 1
}
cd "$REPO_ROOT"

# The canonical v3 sample advertises `implementation_version`, which
# tests/protocol_samples.rs pins to CARGO_PKG_VERSION, so the release
# preparation must rewrite it in the same commit (issue #631 contract).
release_files=(
    .llm/code-samples/protocol/v3-server-messages.jsonl
    .llm/context.md
    CHANGELOG.md
    Cargo.toml
    docs/getting-started.md
    docs/library-usage.md
    fuzz/Cargo.toml
)
# A refresh changes the release boundary and source marker, not the version.
if [ "$#" -gt 0 ]; then
    if [ "$#" -ne 2 ] || [ "$1" != "--bump" ]; then
        echo "Usage: $0 [--bump <auto|major|minor|patch|refresh>]" >&2
        exit 2
    fi
    case "$2" in
        refresh) release_files=(Cargo.toml CHANGELOG.md) ;;
        auto|major|minor|patch) ;;
        *) echo "ERROR: Unknown preparation mode: $2" >&2; exit 2 ;;
    esac
fi
for release_file in "${release_files[@]}"; do
    if ! git cat-file -e "HEAD:$release_file" 2>/dev/null; then
        echo "ERROR: Canonical release file is not tracked at HEAD: $release_file" >&2
        exit 1
    fi
done

printf '%s\0' "${release_files[@]}"

if [ "${2:-}" != "refresh" ]; then
    bash "$SCRIPT_DIR/list-release-lockfiles.sh"
fi
