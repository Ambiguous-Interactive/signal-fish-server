#!/usr/bin/env bash
# Print the unique first-parent commit that introduced a package version.
set -euo pipefail

if [ "$#" -ne 4 ]; then
    echo "Usage: $0 <repository> <history-revision> <version> <read-toml-script>" >&2
    exit 2
fi

repository=$1
history_revision=$2
expected_version=$3
read_toml_script=$4

if [ "$(git -C "$repository" rev-parse --is-shallow-repository)" != "false" ]; then
    echo "ERROR: Release source selection requires complete first-parent history." >&2
    exit 1
fi

scratch_base=${RUNNER_TEMP:-${TMPDIR:-/tmp}}
finder_scratch=$(mktemp -d "${scratch_base}/release-version-introduction.XXXXXX")
cleanup() {
    rm -rf -- "$finder_scratch"
}
trap cleanup EXIT

match_count=0
matched_revision=
candidate_index=0
while IFS= read -r candidate; do
    candidate_index=$((candidate_index + 1))
    candidate_manifest="${finder_scratch}/Cargo.${candidate_index}.toml"
    if ! git -C "$repository" show "${candidate}:Cargo.toml" > "$candidate_manifest" 2>/dev/null; then
        continue
    fi
    candidate_version=$(bash "$read_toml_script" "$candidate_manifest" version package 2>/dev/null || true)
    [ "$candidate_version" = "$expected_version" ] || continue

    parent=$(git -C "$repository" rev-parse "${candidate}^1" 2>/dev/null || true)
    parent_version=
    if [ -n "$parent" ]; then
        parent_manifest="${finder_scratch}/Cargo.${candidate_index}.parent.toml"
        if git -C "$repository" show "${parent}:Cargo.toml" > "$parent_manifest" 2>/dev/null; then
            parent_version=$(bash "$read_toml_script" "$parent_manifest" version package 2>/dev/null || true)
        fi
    fi
    [ "$parent_version" != "$expected_version" ] || continue

    matched_revision=$candidate
    match_count=$((match_count + 1))
done < <(git -C "$repository" rev-list --first-parent "$history_revision" -- Cargo.toml)

if [ "$match_count" -eq 0 ]; then
    echo "ERROR: No first-parent commit introduces release version ${expected_version}." >&2
    exit 1
fi
if [ "$match_count" -ne 1 ]; then
    echo "ERROR: Release version ${expected_version} has multiple first-parent introduction commits; refusing ambiguous source selection." >&2
    exit 1
fi

printf '%s\n' "$matched_revision"
