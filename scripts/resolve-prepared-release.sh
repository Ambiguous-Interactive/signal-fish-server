#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
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
    branch_sha=$remote_sha
    remote_git fetch --no-tags origin "$remote_sha"
    set +e
    git diff --quiet "$remote_sha" --
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
