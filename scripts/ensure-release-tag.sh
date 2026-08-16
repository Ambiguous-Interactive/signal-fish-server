#!/usr/bin/env bash
set -euo pipefail

: "${GH_TOKEN:?GH_TOKEN is required}"
: "${TAG:?TAG is required}"
: "${SOURCE_REVISION:?SOURCE_REVISION is required}"

if [[ ! "$TAG" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
    echo "ERROR: Release tag is not canonical vX.Y.Z: ${TAG}" >&2
    exit 2
fi
if ! git check-ref-format "refs/tags/${TAG}" >/dev/null 2>&1; then
    echo "ERROR: Invalid release tag ref: ${TAG}" >&2
    exit 2
fi
if [[ ! "$SOURCE_REVISION" =~ ^[0-9a-f]{40}$ ]]; then
    echo "ERROR: Source revision is not a full lowercase Git SHA: ${SOURCE_REVISION}" >&2
    exit 2
fi

auth_header=$(printf 'x-access-token:%s' "$GH_TOKEN" | base64 | tr -d '\n')
remote_git() {
    git -c "http.https://github.com/.extraheader=AUTHORIZATION: basic ${auth_header}" "$@"
}

tag_ref="refs/tags/${TAG}"
peeled_ref="${tag_ref}^{}"
readback_ref="refs/release-readback/tags/${TAG}"
REMOTE_TAG_RESULT=""
REMOTE_TAG_STATUS=0
FETCH_RESULT=""
FETCH_STATUS=0

query_remote_tag() {
    set +e
    REMOTE_TAG_RESULT=$(remote_git ls-remote --exit-code --tags origin "$tag_ref" "$peeled_ref" 2>&1)
    REMOTE_TAG_STATUS=$?
    set -e
}

# A valid annotated remote tag has one object ref and one peeled commit ref.
# Requiring both rejects lightweight tags without trusting the local checkout.
validate_remote_listing() {
    local object_id ref extra
    local tag_object=""
    local peeled_commit=""
    local line_count=0

    while IFS=$'\t' read -r object_id ref extra; do
        [ -n "$object_id" ] || continue
        line_count=$((line_count + 1))
        if [[ ! "$object_id" =~ ^[0-9a-f]{40}$ ]] || [ -n "${extra:-}" ]; then
            echo "ERROR: Remote tag ${TAG} returned malformed metadata; refusing to publish." >&2
            exit 1
        fi
        case "$ref" in
            "$tag_ref")
                [ -z "$tag_object" ] || {
                    echo "ERROR: Remote tag ${TAG} returned duplicate object metadata; refusing to publish." >&2
                    exit 1
                }
                tag_object=$object_id
                ;;
            "$peeled_ref")
                [ -z "$peeled_commit" ] || {
                    echo "ERROR: Remote tag ${TAG} returned duplicate peeled metadata; refusing to publish." >&2
                    exit 1
                }
                peeled_commit=$object_id
                ;;
            *)
                echo "ERROR: Remote tag ${TAG} returned unexpected ref ${ref}; refusing to publish." >&2
                exit 1
                ;;
        esac
    done <<< "$REMOTE_TAG_RESULT"

    if [ "$line_count" -ne 2 ] || [ -z "$tag_object" ] || [ -z "$peeled_commit" ]; then
        echo "ERROR: ${TAG} exists but is not an annotated Git tag; refusing to publish." >&2
        exit 1
    fi
    if [ "$peeled_commit" != "$SOURCE_REVISION" ]; then
        echo "ERROR: ${TAG} resolves to ${peeled_commit}, not ${SOURCE_REVISION}; refusing to move it." >&2
        exit 1
    fi
}

fetch_remote_tag() {
    set +e
    FETCH_RESULT=$(remote_git fetch --force --no-tags origin "${tag_ref}:${readback_ref}" 2>&1)
    FETCH_STATUS=$?
    set -e
}

validate_fetched_tag() {
    local tag_type actual_revision
    if ! tag_type=$(git cat-file -t "$readback_ref" 2>&1); then
        echo "ERROR: Could not inspect fetched tag ${TAG}: ${tag_type}" >&2
        exit 1
    fi
    if [ "$tag_type" != "tag" ]; then
        echo "ERROR: ${TAG} exists but is not an annotated Git tag; refusing to publish." >&2
        exit 1
    fi
    if ! actual_revision=$(git rev-parse --verify "${readback_ref}^{commit}" 2>&1); then
        echo "ERROR: Could not peel fetched tag ${TAG}: ${actual_revision}" >&2
        exit 1
    fi
    if [ "$actual_revision" != "$SOURCE_REVISION" ]; then
        echo "ERROR: ${TAG} resolves to ${actual_revision}, not ${SOURCE_REVISION}; refusing to move it." >&2
        exit 1
    fi
}

query_remote_tag
if [ "$REMOTE_TAG_STATUS" -eq 0 ]; then
    validate_remote_listing
    fetch_remote_tag
    if [ "$FETCH_STATUS" -ne 0 ]; then
        echo "ERROR: Failed to fetch existing remote tag ${TAG} (status ${FETCH_STATUS}): ${FETCH_RESULT}" >&2
        exit "$FETCH_STATUS"
    fi
    validate_fetched_tag
    echo "Reusing immutable annotated tag ${TAG} at ${SOURCE_REVISION}."
    exit 0
elif [ "$REMOTE_TAG_STATUS" -ne 2 ]; then
    echo "ERROR: Failed to inspect remote tag ${TAG} (status ${REMOTE_TAG_STATUS}): ${REMOTE_TAG_RESULT}" >&2
    exit "$REMOTE_TAG_STATUS"
fi

# The remote is authoritatively absent. Remove only a stale local copy left by
# checkout before creating the exact annotated object intended for this run.
set +e
git show-ref --verify --quiet "$tag_ref"
local_tag_status=$?
set -e
if [ "$local_tag_status" -eq 0 ]; then
    git tag -d "$TAG"
elif [ "$local_tag_status" -ne 1 ]; then
    echo "ERROR: Failed to inspect local tag ${TAG} (status ${local_tag_status})." >&2
    exit "$local_tag_status"
fi

git config user.name "github-actions[bot]"
git config user.email "github-actions[bot]@users.noreply.github.com"
git tag -a "$TAG" "$SOURCE_REVISION" -m "Release $TAG"

# This is the only remote mutation. A nonzero response is ambiguous because the
# server may have accepted the tag before the connection failed.
set +e
push_result=$(remote_git push origin "$tag_ref" 2>&1)
push_status=$?
set -e
if [ "$push_status" -eq 0 ]; then
    [ -z "$push_result" ] || printf '%s\n' "$push_result"
    echo "Created annotated tag ${TAG} at ${SOURCE_REVISION}."
    exit 0
fi

# Reconcile an ambiguous response with at most three read-only observations.
# Never replay the push. A visible conflict is authoritative and fails at once;
# absence, lookup failure, or fetch failure may be transient.
max_readback_attempts=3
readback_attempt=1
last_readback_kind="error"
last_readback_status=0
last_readback_result=""
while [ "$readback_attempt" -le "$max_readback_attempts" ]; do
    query_remote_tag
    last_readback_status=$REMOTE_TAG_STATUS
    last_readback_result=$REMOTE_TAG_RESULT
    if [ "$REMOTE_TAG_STATUS" -eq 0 ]; then
        validate_remote_listing
        fetch_remote_tag
        if [ "$FETCH_STATUS" -eq 0 ]; then
            validate_fetched_tag
            echo "Push returned status ${push_status}, but ${TAG} exists at the exact expected commit ${SOURCE_REVISION}; continuing."
            exit 0
        fi
        last_readback_kind="fetch"
        last_readback_status=$FETCH_STATUS
        last_readback_result=$FETCH_RESULT
    elif [ "$REMOTE_TAG_STATUS" -eq 2 ]; then
        last_readback_kind="absent"
    else
        last_readback_kind="error"
    fi

    if [ "$readback_attempt" -lt "$max_readback_attempts" ]; then
        echo "Remote tag read-back attempt ${readback_attempt}/${max_readback_attempts} was inconclusive; retrying." >&2
        sleep "$readback_attempt"
    fi
    readback_attempt=$((readback_attempt + 1))
done

echo "ERROR: Failed to push ${TAG} (status ${push_status}): ${push_result}" >&2
case "$last_readback_kind" in
    absent)
        echo "ERROR: ${max_readback_attempts} remote tag read-backs found no ${TAG}; retry the release workflow." >&2
        ;;
    fetch)
        echo "ERROR: All ${max_readback_attempts} remote tag read-backs were inconclusive; last fetch status ${last_readback_status}: ${last_readback_result}" >&2
        ;;
    *)
        echo "ERROR: All ${max_readback_attempts} remote tag read-backs failed; last status ${last_readback_status}: ${last_readback_result}" >&2
        ;;
esac
exit "$push_status"
