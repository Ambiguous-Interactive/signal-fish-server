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
repo_owner=${GITHUB_REPOSITORY%%/*}

# The REST head filter is owner-qualified, so a fork using the same branch name
# cannot be mistaken for this repository's release branch.
set +e
existing_pr=$(gh api --method GET "repos/$GITHUB_REPOSITORY/pulls" \
    -f state=open \
    -f base="$default_branch" \
    -f head="${repo_owner}:${branch}" \
    --jq '.[0].number // empty' 2>&1)
existing_pr_status=$?
set -e
if [ "$existing_pr_status" -ne 0 ]; then
    echo "ERROR: Failed to list open release PRs (status ${existing_pr_status}): ${existing_pr}" >&2
    exit "$existing_pr_status"
fi
if [ -n "$existing_pr" ]; then
    echo "Reusing open release PR #${existing_pr}."
    create_status=0
    create_result=""
else
    set +e
    create_result=$(gh pr create \
        --repo "$GITHUB_REPOSITORY" \
        --base "$default_branch" \
        --head "$branch" \
        --title "release: ${version}" \
        --body-file "$body_file" 2>&1)
    create_status=$?
    set -e
    if [ "$create_status" -eq 0 ]; then
        [ -z "$create_result" ] || printf '%s\n' "$create_result"
    fi
fi

# PR creation can succeed remotely even when its mutation response is lost.
# Retry only the exact read-back; never repeat the mutation. Absence or a read
# error may be transient, but a visible PR at the wrong SHA fails immediately.
max_readback_attempts=3
readback_attempt=1
while [ "$readback_attempt" -le "$max_readback_attempts" ]; do
    set +e
    pr_state=$(gh api --method GET "repos/$GITHUB_REPOSITORY/pulls" \
        -f state=open \
        -f base="$default_branch" \
        -f head="${repo_owner}:${branch}" \
        --jq 'if length == 0 then empty else .[0] | [.number, .head.sha] | @tsv end' 2>&1)
    pr_state_status=$?
    set -e
    if [ "$pr_state_status" -eq 0 ] && [ -n "$pr_state" ]; then
        actual_head_sha=${pr_state#*$'\t'}
        if [ "$actual_head_sha" != "$expected_head_sha" ]; then
            if [ "$create_status" -ne 0 ]; then
                echo "ERROR: Failed to create release PR (status ${create_status}): ${create_result}" >&2
            fi
            echo "ERROR: Release PR head changed while preparing ${branch}; expected ${expected_head_sha}, got ${actual_head_sha}." >&2
            exit 1
        fi
        break
    fi
    if [ "$readback_attempt" -lt "$max_readback_attempts" ]; then
        echo "Release PR read-back attempt ${readback_attempt}/${max_readback_attempts} was inconclusive; retrying." >&2
        sleep $((readback_attempt * 2))
    fi
    readback_attempt=$((readback_attempt + 1))
done

if [ "$pr_state_status" -ne 0 ]; then
    if [ "$create_status" -ne 0 ]; then
        echo "ERROR: Failed to create release PR (status ${create_status}): ${create_result}" >&2
    fi
    echo "ERROR: Failed to read back the open release PR after ${max_readback_attempts} attempts (last status ${pr_state_status}): ${pr_state}" >&2
    exit "$pr_state_status"
fi
if [ -z "$pr_state" ]; then
    if [ "$create_status" -ne 0 ]; then
        echo "ERROR: Failed to create release PR (status ${create_status}): ${create_result}" >&2
    fi
    echo "ERROR: No open release PR found after ${max_readback_attempts} read-backs for ${branch}." >&2
    exit 1
fi
if [ "$create_status" -ne 0 ]; then
    echo "PR creation returned status ${create_status}, but the open PR has the exact expected head ${expected_head_sha}; continuing."
fi

echo "Release ${tag} is represented by ${branch}."
