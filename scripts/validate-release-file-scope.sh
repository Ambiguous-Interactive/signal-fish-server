#!/usr/bin/env bash
# Fail unless the prepared worktree changes exactly the canonical release files.

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || {
    echo "ERROR: validate-release-file-scope.sh must run inside a Git worktree." >&2
    exit 1
}
cd "$REPO_ROOT"

if ! whitespace_errors=$(git diff --check HEAD --); then
    echo "ERROR: Release preparation introduced whitespace errors:" >&2
    printf '%s\n' "$whitespace_errors" >&2
    exit 1
fi

inventory_file=$(mktemp)
actual_file=$(mktemp)
present_file=$(mktemp)
untracked_file=$(mktemp)
cleanup() {
    rm -f "$inventory_file" "$actual_file" "$present_file" "$untracked_file"
}
trap cleanup EXIT

bash "$SCRIPT_DIR/list-release-files.sh" > "$inventory_file"
git diff --name-only --no-renames -z HEAD -- > "$actual_file"
git diff --name-only --no-renames --diff-filter=d -z HEAD -- > "$present_file"
git ls-files --others --exclude-standard -z -- > "$untracked_file"
cat "$untracked_file" >> "$actual_file"
cat "$untracked_file" >> "$present_file"

expected_order=()
actual_order=()
present_order=()
expected_count=0
actual_count=0
present_count=0

array_contains() {
    local needle="$1"
    local candidate
    shift
    for candidate in "$@"; do
        if [ "$candidate" = "$needle" ]; then
            return 0
        fi
    done
    return 1
}

while IFS= read -r -d '' path; do
    if [ "$expected_count" -gt 0 ] && array_contains "$path" "${expected_order[@]}"; then
        echo "ERROR: Canonical release inventory contains duplicate path: $path" >&2
        exit 1
    fi
    expected_order+=("$path")
    expected_count=$((expected_count + 1))
done < "$inventory_file"

if [ "$expected_count" -eq 0 ]; then
    echo "ERROR: Canonical release inventory is empty." >&2
    exit 1
fi

while IFS= read -r -d '' path; do
    if [ "$actual_count" -eq 0 ] || ! array_contains "$path" "${actual_order[@]}"; then
        actual_order+=("$path")
        actual_count=$((actual_count + 1))
    fi
done < "$actual_file"

while IFS= read -r -d '' path; do
    if [ "$present_count" -eq 0 ] || ! array_contains "$path" "${present_order[@]}"; then
        present_order+=("$path")
        present_count=$((present_count + 1))
    fi
done < "$present_file"

status=0
missing_header=0
for path in "${expected_order[@]}"; do
    if [ "$present_count" -eq 0 ] || ! array_contains "$path" "${present_order[@]}"; then
        if [ "$missing_header" -eq 0 ]; then
            echo "ERROR: Missing release files:" >&2
            missing_header=1
        fi
        printf '  - %s\n' "$path" >&2
        status=1
    fi
done

unexpected_header=0
if [ "$actual_count" -gt 0 ]; then
    for path in "${actual_order[@]}"; do
        if ! array_contains "$path" "${expected_order[@]}"; then
            if [ "$unexpected_header" -eq 0 ]; then
                echo "ERROR: Unexpected release files:" >&2
                unexpected_header=1
            fi
            printf '  - %s\n' "$path" >&2
            status=1
        fi
    done
fi

if [ "$status" -ne 0 ]; then
    exit "$status"
fi

printf 'Release preparation changed exactly %s canonical files.\n' "$expected_count"
