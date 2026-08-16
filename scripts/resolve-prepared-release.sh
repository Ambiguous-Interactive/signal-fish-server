#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
: "${GH_TOKEN:?GH_TOKEN is required}"
: "${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"

version=$(bash "$SCRIPT_DIR/read-toml-string.sh" Cargo.toml version package || true)
if [[ ! "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
    echo "ERROR: Prepared Cargo.toml version is not strict X.Y.Z semver: $version" >&2
    exit 1
fi

branch="release/v${version}"
tag="v${version}"
auth_header=$(printf 'x-access-token:%s' "$GH_TOKEN" | base64 | tr -d '\n')
if ! base_sha=$(git rev-parse --verify HEAD 2>&1); then
    echo "ERROR: Failed to resolve immutable local HEAD: ${base_sha}" >&2
    exit 1
fi

remote_git() {
    git -c "http.https://github.com/.extraheader=AUTHORIZATION: basic ${auth_header}" "$@"
}

set +e
tag_result=$(remote_git ls-remote --exit-code --tags origin "refs/tags/${tag}" 2>&1)
tag_status=$?
branch_result=$(remote_git ls-remote --exit-code --heads origin "refs/heads/${branch}" 2>&1)
branch_status=$?
set -e

if [ "$tag_status" -eq 0 ]; then
    echo "ERROR: Release tag ${tag} already exists." >&2
    exit 1
elif [ "$tag_status" -ne 2 ]; then
    echo "ERROR: Failed to check release tag ${tag}: ${tag_result}" >&2
    exit "$tag_status"
fi

branch_sha=""
if [ "$branch_status" -eq 0 ]; then
    remote_sha=${branch_result%%[[:space:]]*}
    remote_git fetch --no-tags origin "refs/heads/${branch}"
    fetched_sha=$(git rev-parse FETCH_HEAD)
    if [ "$fetched_sha" != "$remote_sha" ]; then
        echo "ERROR: Release branch ${branch} changed while it was being verified." >&2
        exit 1
    fi
    if ! parent_record=$(git rev-list --parents --max-count=1 "$fetched_sha" 2>&1); then
        echo "ERROR: Failed to inspect release branch ${branch} ancestry: ${parent_record}" >&2
        exit 1
    fi
    read -r -a commit_and_parents <<< "$parent_record"
    parent_count=$((${#commit_and_parents[@]} - 1))
    if [ "${commit_and_parents[0]:-}" != "$fetched_sha" ] || [ "$parent_count" -ne 1 ]; then
        echo "ERROR: Release branch ${branch} must be exactly one non-merge commit above local HEAD ${base_sha}; found ${parent_count} parent(s) at ${fetched_sha}." >&2
        exit 1
    fi
    if [ "${commit_and_parents[1]}" != "$base_sha" ]; then
        echo "ERROR: Release branch ${branch} may be reused only when its sole parent is local HEAD ${base_sha}; found ${commit_and_parents[1]}." >&2
        exit 1
    fi
    branch_sha=$fetched_sha
    set +e
    git diff --quiet FETCH_HEAD --
    tree_status=$?
    set -e
    if [ "$tree_status" -eq 1 ]; then
        echo "ERROR: Release branch ${branch} exists with a different tree." >&2
        exit 1
    elif [ "$tree_status" -ne 0 ]; then
        echo "ERROR: Failed to compare release branch ${branch}." >&2
        exit "$tree_status"
    fi
    branch_exists=true
elif [ "$branch_status" -ne 2 ]; then
    echo "ERROR: Failed to check release branch ${branch}: ${branch_result}" >&2
    exit "$branch_status"
else
    branch_exists=false
fi

{
    echo "version=$version"
    echo "tag=$tag"
    echo "branch=$branch"
    echo "branch_exists=$branch_exists"
    echo "branch_sha=$branch_sha"
} >> "$GITHUB_OUTPUT"
