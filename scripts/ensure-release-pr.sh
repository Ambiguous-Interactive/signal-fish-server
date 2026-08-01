#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 6 ]; then
    echo "Usage: $0 <default-branch> <release-branch> <version> <tag> <body-file> <expected-head-sha>" >&2
    exit 2
fi
: "${GH_TOKEN:?GH_TOKEN is required}"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"

default_branch=$1
branch=$2
version=$3
tag=$4
body_file=$5
expected_head_sha=$6

existing_pr=$(gh pr list \
    --repo "$GITHUB_REPOSITORY" \
    --state open \
    --base "$default_branch" \
    --head "$branch" \
    --json number \
    --jq '.[0].number // empty')
if [ -n "$existing_pr" ]; then
    echo "Reusing open release PR #${existing_pr}."
else
    gh pr create \
        --repo "$GITHUB_REPOSITORY" \
        --base "$default_branch" \
        --head "$branch" \
        --title "release: ${version}" \
        --body-file "$body_file"
fi

pr_state=$(gh pr list \
    --repo "$GITHUB_REPOSITORY" \
    --state open \
    --base "$default_branch" \
    --head "$branch" \
    --json number,headRefOid \
    --jq 'if length == 0 then empty else .[0] | [.number, .headRefOid] | @tsv end')
if [ -z "$pr_state" ]; then
    echo "ERROR: No open release PR found after ensuring ${branch}." >&2
    exit 1
fi
actual_head_sha=${pr_state#*$'\t'}
if [ "$actual_head_sha" != "$expected_head_sha" ]; then
    echo "ERROR: Release PR head changed while preparing ${branch}; expected ${expected_head_sha}, got ${actual_head_sha}." >&2
    exit 1
fi

echo "Release ${tag} is represented by ${branch}."
