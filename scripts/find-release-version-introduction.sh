#!/usr/bin/env bash
# Print the reviewed first-parent source for a version, including explicit refreshes.
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
marker_selected=0
selected_marker=
refresh_revision=
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
    parent_manifest="${finder_scratch}/missing-parent.toml"
    if [ -n "$parent" ]; then
        parent_manifest="${finder_scratch}/Cargo.${candidate_index}.parent.toml"
        if git -C "$repository" show "${parent}:Cargo.toml" > "$parent_manifest" 2>/dev/null; then
            parent_version=$(bash "$read_toml_script" "$parent_manifest" version package 2>/dev/null || true)
        fi
    fi
    marker=$(bash "$read_toml_script" "$candidate_manifest" prepared-from package.metadata.signal-fish-release 2>/dev/null || true)
    if [ "$marker_selected" -eq 0 ]; then
        selected_marker=$marker
        marker_selected=1
        if [ -n "$selected_marker" ] && [[ ! "$selected_marker" =~ ^[0-9a-f]{40}$ ]]; then
            echo "ERROR: Release refresh prepared-from must be a full commit SHA." >&2
            exit 1
        fi
    fi
    if [ -n "$marker" ] && [ -z "$selected_marker" ]; then
        echo "ERROR: Release refresh marker was removed without a version bump." >&2
        exit 1
    fi
    if [ -n "$selected_marker" ] && [ "$marker" = "$selected_marker" ]; then
        parent_marker=$(bash "$read_toml_script" "$parent_manifest" prepared-from package.metadata.signal-fish-release 2>/dev/null || true)
        if [ "$parent_marker" != "$selected_marker" ]; then
            if [ -n "$refresh_revision" ]; then
                echo "ERROR: Release refresh marker has multiple first-parent introduction commits." >&2
                exit 1
            fi
            if ! git -C "$repository" merge-base --is-ancestor "$selected_marker" "$parent"; then
                echo "ERROR: Release refresh base must be an ancestor of its preparation commit." >&2
                exit 1
            fi
            base_manifest="${finder_scratch}/refresh-base.toml"
            git -C "$repository" show "${selected_marker}:Cargo.toml" > "$base_manifest"
            base_version=$(bash "$read_toml_script" "$base_manifest" version package 2>/dev/null || true)
            if [ "$base_version" != "$expected_version" ]; then
                echo "ERROR: Release refresh base must identify the same package version." >&2
                exit 1
            fi
            refresh_revision=$candidate
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

if [ -n "$selected_marker" ] && [ -z "$refresh_revision" ]; then
    echo "ERROR: Release refresh marker has no first-parent introduction commit." >&2
    exit 1
fi
printf '%s\n' "${refresh_revision:-$matched_revision}"
