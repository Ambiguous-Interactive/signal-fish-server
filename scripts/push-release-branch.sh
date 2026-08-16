#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "Usage: $0 <release-branch> <expected-head-sha>" >&2
    exit 2
fi
: "${GH_TOKEN:?GH_TOKEN is required}"

branch=$1
expected_head_sha=$2
if [[ ! "$branch" =~ ^release/v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
    echo "ERROR: Release branch is not release/vX.Y.Z: ${branch}" >&2
    exit 2
fi
if [[ ! "$expected_head_sha" =~ ^[0-9a-f]{40}$ ]]; then
    echo "ERROR: Expected release branch head is not a full Git SHA: ${expected_head_sha}" >&2
    exit 2
fi

auth_header=$(printf 'x-access-token:%s' "$GH_TOKEN" | base64 | tr -d '\n')
remote_git() {
    git -c "http.https://github.com/.extraheader=AUTHORIZATION: basic ${auth_header}" "$@"
}

set +e
push_result=$(remote_git push origin "HEAD:refs/heads/${branch}" 2>&1)
push_status=$?
set -e
if [ "$push_status" -eq 0 ]; then
    [ -z "$push_result" ] || printf '%s\n' "$push_result"
    exit 0
fi

# A failed mutation response can be ambiguous: the server may have accepted the
# update before the connection failed. Retry only the exact read-back; never
# repeat the mutation. A visible conflicting ref is authoritative and fails
# immediately, while absence or a read error may be transient.
expected_state="${expected_head_sha}"$'\t'"refs/heads/${branch}"
max_readback_attempts=3
readback_attempt=1
while [ "$readback_attempt" -le "$max_readback_attempts" ]; do
    set +e
    branch_state=$(remote_git ls-remote --exit-code --heads origin "refs/heads/${branch}" 2>&1)
    branch_status=$?
    set -e
    if [ "$branch_status" -eq 0 ]; then
        if [ "$branch_state" = "$expected_state" ]; then
            echo "Push returned status ${push_status}, but ${branch} exists at the exact expected commit ${expected_head_sha}; continuing."
            exit 0
        fi
        echo "ERROR: Failed to push ${branch} (status ${push_status}): ${push_result}" >&2
        echo "ERROR: The remote branch did not match the expected ${expected_head_sha}: ${branch_state}" >&2
        exit "$push_status"
    fi
    if [ "$readback_attempt" -lt "$max_readback_attempts" ]; then
        echo "Remote branch read-back attempt ${readback_attempt}/${max_readback_attempts} was inconclusive; retrying." >&2
        sleep $((readback_attempt * 2))
    fi
    readback_attempt=$((readback_attempt + 1))
done

echo "ERROR: Failed to push ${branch} (status ${push_status}): ${push_result}" >&2
if [ "$branch_status" -eq 2 ]; then
    echo "ERROR: ${max_readback_attempts} remote read-backs found no ${branch}; retry the preparation workflow." >&2
elif [ "$branch_status" -ne 0 ]; then
    echo "ERROR: All ${max_readback_attempts} remote read-backs failed; last status ${branch_status}: ${branch_state}" >&2
fi
exit "$push_status"
