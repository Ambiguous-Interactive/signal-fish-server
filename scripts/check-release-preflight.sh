#!/usr/bin/env bash
# Fail closed unless required CI passed on the exact default-branch release commit.
set -euo pipefail

require_value() {
    local name=$1
    local value=$2
    if [ -z "$value" ]; then
        echo "ERROR: ${name} is required for release preflight." >&2
        exit 1
    fi
}

require_value RELEASE_REPOSITORY "${RELEASE_REPOSITORY:-}"
require_value RELEASE_COMMIT_SHA "${RELEASE_COMMIT_SHA:-}"
require_value RELEASE_DEFAULT_BRANCH "${RELEASE_DEFAULT_BRANCH:-}"

REPO=$RELEASE_REPOSITORY
COMMIT_SHA=$RELEASE_COMMIT_SHA
DEFAULT_BRANCH=$RELEASE_DEFAULT_BRANCH

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
        RUN_EVENT=""
        RUN_BRANCH=""
        RUN_SHA=""
        RUN_STATUS=""
        CONCLUSION=""
        IFS=$'\t' read -r RUN_EVENT RUN_BRANCH RUN_SHA RUN_STATUS CONCLUSION <<< "$RUN_METADATA"
        if [[ "$RUN_METADATA" == *$'\n'* ]] || \
            [ "$RUN_EVENT" != "push" ] || \
            [ "$RUN_BRANCH" != "$DEFAULT_BRANCH" ] || \
            [ "$RUN_SHA" != "$COMMIT_SHA" ] || \
            [ "$RUN_STATUS" != "completed" ] || \
            [ -z "$CONCLUSION" ]; then
            echo "ERROR: GitHub returned unrelated or malformed run metadata for '${WORKFLOW_NAME}'." >&2
            echo "  event=${RUN_EVENT} branch=${RUN_BRANCH} sha=${RUN_SHA} status=${RUN_STATUS}" >&2
            FAILED=1
            continue
        fi

        if [ "$CONCLUSION" != "success" ]; then
            echo "ERROR: '${WORKFLOW_NAME}' conclusion is '${CONCLUSION}' (expected 'success')" >&2
            echo "  Fix the failing checks before releasing." >&2
            FAILED=1
        else
            echo "OK: '${WORKFLOW_NAME}' passed on commit ${COMMIT_SHA}"
        fi
        continue
    fi

    echo "ERROR: No completed default-branch push run found for '${WORKFLOW_NAME}' on commit ${COMMIT_SHA}" >&2
    echo "  Ensure CI has run and completed on this commit before releasing." >&2
    FAILED=1
done

echo ""
if [ "$FAILED" -ne 0 ]; then
    echo "RELEASE BLOCKED: Required CI checks have not passed." >&2
    exit 1
fi

echo "All required CI checks passed. Proceeding with release."
