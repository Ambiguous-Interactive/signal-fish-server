#!/usr/bin/env bash
# Fail closed unless required CI passed on the exact default-branch release commit.
#
# Each required workflow must pass one of two acceptance legs:
#   1. Push leg: a completed successful `push` run on the default branch at
#      the exact release commit.
#   2. Release pull-request leg: the merged pull request whose squash merge
#      commit is the release commit, proven by a completed successful
#      `pull_request` run at that pull request's head SHA. A squash merge
#      keeps the pull-request run's content identical to the release
#      commit's tree, so this leg carries the same evidence as the removed
#      post-merge push wave (issue #557) for workflows that no longer
#      trigger on push. The leg refuses a multi-parent commit: a merge
#      commit's tree can carry manual conflict resolutions that no
#      pull-request run ever validated.
set -euo pipefail

require_value() {
    local name=$1
    local value=$2
    if [ -z "$value" ]; then
        echo "ERROR: ${name} is required for release preflight." >&2
        exit 1
    fi
}

# Map the release commit to the merged pull request that produced it.
# Caches the result; a failed resolution stays failed for the whole run.
RELEASE_PR_NUMBER=""
RELEASE_PR_HEAD_SHA=""
RELEASE_PR_RESOLUTION=""

resolve_release_pr() {
    if [ -n "$RELEASE_PR_NUMBER" ]; then
        return 0
    fi
    if [ -n "$RELEASE_PR_RESOLUTION" ]; then
        echo "ERROR: ${RELEASE_PR_RESOLUTION}" >&2
        return 1
    fi

    # The pull-request leg's evidence is content identity: only a squash
    # merge (single parent) guarantees the pull-request run's tree equals
    # the release commit's tree. A merge commit can carry manual conflict
    # resolutions no pull-request run ever validated, so refuse it here
    # instead of accepting a weaker proof.
    local parents
    if ! parents=$(gh api \
        --method GET \
        "repos/${REPO}/commits/${COMMIT_SHA}" \
        --jq '.parents | length'); then
        RELEASE_PR_RESOLUTION="Could not retrieve commit ${COMMIT_SHA} from GitHub."
        echo "ERROR: ${RELEASE_PR_RESOLUTION}" >&2
        return 1
    fi
    if [ "$parents" -ne 1 ]; then
        RELEASE_PR_RESOLUTION="Commit ${COMMIT_SHA} has ${parents} parents; a release commit must be a single-parent squash merge."
        echo "ERROR: ${RELEASE_PR_RESOLUTION}" >&2
        return 1
    fi

    local matches
    if ! matches=$(gh api \
        --method GET \
        --paginate \
        "repos/${REPO}/pulls?state=closed&base=${DEFAULT_BRANCH}&per_page=100" \
        --jq "[.[] | select(.merged_at != null and .merge_commit_sha == \"${COMMIT_SHA}\")][0:2] | .[] | [.number, .head.sha] | @tsv"); then
        RELEASE_PR_RESOLUTION="Could not query merged pull requests from GitHub."
        echo "ERROR: ${RELEASE_PR_RESOLUTION}" >&2
        return 1
    fi

    local count
    count=$(printf '%s' "$matches" | grep -c . || true)
    if [ "$count" -eq 0 ]; then
        RELEASE_PR_RESOLUTION="No merged pull request found for commit ${COMMIT_SHA}."
        echo "ERROR: ${RELEASE_PR_RESOLUTION}" >&2
        echo "  The release commit must be the squash merge of its release pull request." >&2
        return 1
    fi
    if [ "$count" -gt 1 ]; then
        RELEASE_PR_RESOLUTION="Multiple merged pull requests found for commit ${COMMIT_SHA}."
        echo "ERROR: ${RELEASE_PR_RESOLUTION}" >&2
        return 1
    fi

    local number head_sha
    IFS=$'\t' read -r number head_sha <<< "$matches"
    if [ -z "$number" ] || [ -z "$head_sha" ]; then
        RELEASE_PR_RESOLUTION="Malformed pull-request metadata for commit ${COMMIT_SHA}."
        echo "ERROR: ${RELEASE_PR_RESOLUTION}" >&2
        return 1
    fi
    RELEASE_PR_NUMBER=$number
    RELEASE_PR_HEAD_SHA=$head_sha
}

# Validate one completed run against the leg's expected identity.
# Arguments: workflow name, expected event, expected head SHA, expected
# branch (empty = any non-empty branch), run metadata.
check_run_metadata() {
    local workflow=$1 expected_event=$2 expected_sha=$3 expected_branch=$4 metadata=$5
    local run_event run_branch run_sha run_status conclusion
    IFS=$'\t' read -r run_event run_branch run_sha run_status conclusion <<< "$metadata"
    if [[ "$metadata" == *$'\n'* ]] || \
        [ "$run_event" != "$expected_event" ] || \
        [ "$run_sha" != "$expected_sha" ] || \
        { [ -n "$expected_branch" ] && [ "$run_branch" != "$expected_branch" ]; } || \
        [ -z "$run_branch" ] || \
        [ "$run_status" != "completed" ] || \
        [ -z "$conclusion" ]; then
        echo "ERROR: GitHub returned unrelated or malformed run metadata for '${workflow}'." >&2
        echo "  event=${run_event} branch=${run_branch} sha=${run_sha} status=${run_status}" >&2
        return 1
    fi
    if [ "$conclusion" != "success" ]; then
        echo "ERROR: '${workflow}' conclusion is '${conclusion}' (expected 'success')" >&2
        echo "  Fix the failing checks before releasing." >&2
        return 1
    fi
}

require_value RELEASE_REPOSITORY "${RELEASE_REPOSITORY:-}"
require_value RELEASE_COMMIT_SHA "${RELEASE_COMMIT_SHA:-}"
require_value RELEASE_DEFAULT_BRANCH "${RELEASE_DEFAULT_BRANCH:-}"

REPO=$RELEASE_REPOSITORY
COMMIT_SHA=$RELEASE_COMMIT_SHA
DEFAULT_BRANCH=$RELEASE_DEFAULT_BRANCH

if [[ ! "$COMMIT_SHA" =~ ^[0-9a-fA-F]{40}$ ]]; then
    echo "ERROR: RELEASE_COMMIT_SHA must be a full 40-character commit SHA, got '${COMMIT_SHA}'." >&2
    exit 1
fi

echo "Verifying CI status for commit: $COMMIT_SHA"

REQUIRED_WORKFLOWS=("CI" "Documentation Validation")
UNIQUE_WORKFLOW_COUNT=$(printf '%s\n' "${REQUIRED_WORKFLOWS[@]}" | sort -u | wc -l)
if [ "$UNIQUE_WORKFLOW_COUNT" -ne "${#REQUIRED_WORKFLOWS[@]}" ]; then
    echo "ERROR: Duplicate required workflow names in REQUIRED_WORKFLOWS." >&2
    exit 1
fi

if ! WORKFLOW_INVENTORY=$(gh api \
    --method GET \
    --paginate \
    "repos/${REPO}/actions/workflows" \
    -f per_page=100 \
    --jq '.workflows[] | [.name, .id] | @tsv'); then
    echo "ERROR: Could not retrieve repository workflows from GitHub." >&2
    exit 1
fi

FAILED=0
for WORKFLOW_NAME in "${REQUIRED_WORKFLOWS[@]}"; do
    echo ""
    echo "Checking workflow: $WORKFLOW_NAME"

    WORKFLOW_IDS=()
    while IFS=$'\t' read -r FOUND_NAME FOUND_ID; do
        [ "$FOUND_NAME" = "$WORKFLOW_NAME" ] || continue
        if [[ ! "$FOUND_ID" =~ ^[0-9]+$ ]]; then
            echo "ERROR: Workflow '${WORKFLOW_NAME}' returned malformed ID '${FOUND_ID}'." >&2
            FAILED=1
            continue
        fi
        WORKFLOW_IDS+=("$FOUND_ID")
    done <<< "$WORKFLOW_INVENTORY"

    if [ "${#WORKFLOW_IDS[@]}" -eq 0 ]; then
        echo "ERROR: Workflow '${WORKFLOW_NAME}' not found in repository" >&2
        FAILED=1
        continue
    fi
    if [ "${#WORKFLOW_IDS[@]}" -gt 1 ]; then
        echo "ERROR: Multiple workflows found with name '${WORKFLOW_NAME}'" >&2
        printf '  ID: %s\n' "${WORKFLOW_IDS[@]}" >&2
        FAILED=1
        continue
    fi
    WORKFLOW_ID=${WORKFLOW_IDS[0]}

    # Leg 1: an exact push run on the default branch at the release commit.
    if ! RUN_METADATA=$(gh api \
        --method GET \
        "repos/${REPO}/actions/workflows/${WORKFLOW_ID}/runs" \
        -f branch="$DEFAULT_BRANCH" \
        -f event=push \
        -f head_sha="$COMMIT_SHA" \
        -f status=completed \
        -f per_page=1 \
        --jq '.workflow_runs[0] // empty | [.event, .head_branch, .head_sha, .status, .conclusion] | @tsv'); then
        echo "ERROR: Could not retrieve '${WORKFLOW_NAME}' runs from GitHub." >&2
        FAILED=1
        continue
    fi

    if [ -n "$RUN_METADATA" ]; then
        if check_run_metadata "$WORKFLOW_NAME" "push" "$COMMIT_SHA" "$DEFAULT_BRANCH" "$RUN_METADATA"; then
            echo "OK: '${WORKFLOW_NAME}' passed on commit ${COMMIT_SHA}"
        else
            FAILED=1
        fi
        continue
    fi

    # Leg 2: no push run exists (issue #557 removed the push triggers), so
    # prove this workflow through the merged release pull request.
    if ! resolve_release_pr; then
        FAILED=1
        continue
    fi
    echo "  No push run found; checking release pull request #${RELEASE_PR_NUMBER} (head ${RELEASE_PR_HEAD_SHA})."

    if ! RUN_METADATA=$(gh api \
        --method GET \
        "repos/${REPO}/actions/workflows/${WORKFLOW_ID}/runs" \
        -f event=pull_request \
        -f head_sha="$RELEASE_PR_HEAD_SHA" \
        -f status=completed \
        -f per_page=1 \
        --jq '.workflow_runs[0] // empty | [.event, .head_branch, .head_sha, .status, .conclusion] | @tsv'); then
        echo "ERROR: Could not retrieve '${WORKFLOW_NAME}' pull-request runs from GitHub." >&2
        FAILED=1
        continue
    fi

    if [ -z "$RUN_METADATA" ]; then
        echo "ERROR: No completed pull-request run found for '${WORKFLOW_NAME}' on release pull request #${RELEASE_PR_NUMBER} (head ${RELEASE_PR_HEAD_SHA})." >&2
        echo "  Re-run this workflow on the pull request head commit, or merge a successor pull request for the release." >&2
        FAILED=1
        continue
    fi

    if check_run_metadata "$WORKFLOW_NAME" "pull_request" "$RELEASE_PR_HEAD_SHA" "" "$RUN_METADATA"; then
        echo "OK: '${WORKFLOW_NAME}' passed on release pull request #${RELEASE_PR_NUMBER} (head ${RELEASE_PR_HEAD_SHA})"
    else
        FAILED=1
    fi
done

echo ""
if [ "$FAILED" -ne 0 ]; then
    echo "RELEASE BLOCKED: Required CI checks have not passed." >&2
    exit 1
fi

echo "All required CI checks passed. Proceeding with release."
