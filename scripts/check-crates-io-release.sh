#!/usr/bin/env bash
# Fail closed unless a crates.io version is absent or matches the exact source.
set -euo pipefail

VERSION=${RELEASE_VERSION:-}
SOURCE_REVISION=${RELEASE_SOURCE_REVISION:-}
OUTPUT_FILE=${RELEASE_OUTPUT_FILE:-${GITHUB_OUTPUT:-}}
API_BASE=${CRATES_IO_API_BASE:-https://crates.io/api/v1}

if [[ ! "$VERSION" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
    echo "ERROR: RELEASE_VERSION must be strict X.Y.Z semver: ${VERSION}" >&2
    exit 2
fi
if [[ ! "$SOURCE_REVISION" =~ ^[0-9a-f]{40}$ ]] || [ -z "$OUTPUT_FILE" ]; then
    echo "ERROR: RELEASE_SOURCE_REVISION and release output file are required." >&2
    exit 2
fi

: "${RUNNER_TEMP:?RUNNER_TEMP is required}"
scratch_dir=$(mktemp -d "${RUNNER_TEMP}/crates-io-retry.XXXXXX")
cleanup() {
    rm -rf -- "$scratch_dir"
}
trap cleanup EXIT

api_url="${API_BASE}/crates/signal-fish-server/${VERSION}"
user_agent="signal-fish-server-release-workflow (https://github.com/${GITHUB_REPOSITORY:-Ambiguous-Interactive/signal-fish-server})"
response_file="${scratch_dir}/crate-version.json"
status=$(curl --user-agent "$user_agent" --retry 3 --retry-all-errors --silent --show-error \
    --output "$response_file" --write-out '%{http_code}' "$api_url")

case "$status" in
    404)
        echo "exists=false" >> "$OUTPUT_FILE"
        ;;
    200)
        checksum=$(jq -r '.version.checksum // empty' "$response_file")
        if [[ ! "$checksum" =~ ^[0-9a-f]{64}$ ]]; then
            echo "ERROR: crates.io returned no valid checksum for signal-fish-server ${VERSION}." >&2
            exit 1
        fi

        crate_file="${scratch_dir}/signal-fish-server-${VERSION}.crate"
        curl --user-agent "$user_agent" --retry 3 --retry-all-errors --fail --silent --show-error \
            --location --output "$crate_file" \
            "${API_BASE}/crates/signal-fish-server/${VERSION}/download"
        actual_checksum=$(sha256sum "$crate_file" | awk '{print $1}')
        if [ "$actual_checksum" != "$checksum" ]; then
            echo "ERROR: Downloaded crate checksum ${actual_checksum} does not match crates.io ${checksum}." >&2
            exit 1
        fi

        vcs_info=$(tar -xOf "$crate_file" --wildcards '*/.cargo_vcs_info.json' 2>/dev/null || true)
        published_revision=$(printf '%s' "$vcs_info" | jq -r '.git.sha1 // empty' 2>/dev/null || true)
        if [ "$published_revision" != "$SOURCE_REVISION" ]; then
            echo "ERROR: signal-fish-server ${VERSION} already exists on crates.io from revision '${published_revision}', expected '${SOURCE_REVISION}'." >&2
            exit 1
        fi
        # Cargo omits `git.dirty` for clean packages and serializes it only
        # when the source is dirty. Accept an explicit false for compatibility,
        # but reject true and every non-boolean value.
        published_dirty=$(printf '%s' "$vcs_info" | jq -r \
            'if (.git | type) != "object" then "missing-git"
             elif (.git | has("dirty") | not) then "clean-omitted"
             elif (.git.dirty | type) != "boolean" then "invalid-type"
             else (.git.dirty | tostring) end' \
            2>/dev/null || true)
        if [ "$published_dirty" != "clean-omitted" ] && [ "$published_dirty" != "false" ]; then
            echo "ERROR: signal-fish-server ${VERSION} has invalid cargo VCS cleanliness metadata '${published_dirty:-unreadable}'; expected clean Cargo metadata (dirty absent or false)." >&2
            exit 1
        fi
        echo "Crate ${VERSION} already exists from ${SOURCE_REVISION}; retry is safe."
        echo "exists=true" >> "$OUTPUT_FILE"
        ;;
    *)
        echo "ERROR: crates.io version lookup failed closed with HTTP ${status}." >&2
        exit 1
        ;;
esac
