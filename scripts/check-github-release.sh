#!/usr/bin/env bash
# Classify an absent GitHub Release or fail closed unless its public identity
# and provenance match the already-verified tag, source, and container.
set -euo pipefail

TAG=${TAG:-}
RELEASE_NAME=${RELEASE_NAME:-}
SOURCE_REVISION=${RELEASE_SOURCE_REVISION:-}
IMAGE_DIGEST=${RELEASE_IMAGE_DIGEST:-}
NOTES_FILE=${RELEASE_NOTES_FILE:-}
NOTES_SHA256=${RELEASE_NOTES_SHA256:-}
REQUIRE_EXISTING=${RELEASE_REQUIRE_EXISTING:-false}
OUTPUT_FILE=${RELEASE_OUTPUT_FILE:-${GITHUB_OUTPUT:-}}
GH_CLI=${GH_CLI:-gh}

if [[ ! "$TAG" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] \
    || [ -z "$RELEASE_NAME" ] \
    || [[ ! "$SOURCE_REVISION" =~ ^[0-9a-f]{40}$ ]] \
    || [[ ! "$IMAGE_DIGEST" =~ ^sha256:[0-9a-f]{64}$ ]] \
    || [[ ! "$REQUIRE_EXISTING" =~ ^(true|false)$ ]] \
    || [ -z "$OUTPUT_FILE" ] \
    || [ -z "${GITHUB_REPOSITORY:-}" ]; then
    echo "ERROR: Release tag, name, source revision, image digest, repository, and output file are required." >&2
    exit 2
fi

if { [ -z "$NOTES_FILE" ] && [ -z "$NOTES_SHA256" ]; } \
    || { [ -n "$NOTES_FILE" ] && [ -n "$NOTES_SHA256" ]; }; then
    echo "ERROR: Exactly one expected Release notes file or SHA-256 digest is required." >&2
    exit 2
fi
if [ -n "$NOTES_FILE" ] && [ ! -f "$NOTES_FILE" ]; then
    echo "ERROR: Expected Release notes file does not exist: $NOTES_FILE" >&2
    exit 2
fi
if [ -n "$NOTES_SHA256" ] && [[ ! "$NOTES_SHA256" =~ ^[0-9a-f]{64}$ ]]; then
    echo "ERROR: Expected Release notes SHA-256 digest is invalid." >&2
    exit 2
fi

if [ -n "$NOTES_FILE" ]; then
    NOTES_SHA256=$(sha256sum "$NOTES_FILE" | awk '{print $1}')
fi

api_path="repos/${GITHUB_REPOSITORY}/releases/tags/${TAG}"
if ! release=$($GH_CLI api "$api_path" 2>/dev/null); then
    status=$($GH_CLI api --include "$api_path" 2>&1 \
        | sed -n 's/^HTTP\/[^ ]* \([0-9][0-9][0-9]\).*/\1/p' \
        | head -n 1 || true)
    if [ "$status" != "404" ]; then
        echo "ERROR: Could not verify whether GitHub Release $TAG exists (HTTP ${status:-unknown})." >&2
        exit 1
    fi
    if [ "$REQUIRE_EXISTING" = "true" ]; then
        echo "ERROR: GitHub Release $TAG does not exist before asset upload." >&2
        exit 1
    fi
    echo "exists=false" >> "$OUTPUT_FILE"
    exit 0
fi

expected_source_line="Source revision: \`${SOURCE_REVISION}\`"
expected_digest_line="Multi-architecture manifest digest: \`${IMAGE_DIGEST}\`"
if ! mismatches=$(printf '%s' "$release" | jq -r \
    --arg tag "$TAG" \
    --arg name "$RELEASE_NAME" \
    --arg source_line "$expected_source_line" \
    --arg digest_line "$expected_digest_line" \
    '
        [
          if .tag_name != $tag then "tag_name" else empty end,
          if .name != $name then "name" else empty end,
          if .draft != false then "draft" else empty end,
          if .prerelease != false then "prerelease" else empty end,
          if (.body | type) != "string" then "source revision note"
            elif (.body | contains($source_line) | not) then "source revision note"
            else empty end,
          if (.body | type) != "string" then "image digest note"
            elif (.body | contains($digest_line) | not) then "image digest note"
            else empty end
        ] | join(", ")
    ' 2>/dev/null); then
    echo "ERROR: GitHub Release $TAG returned malformed metadata." >&2
    exit 1
fi

if [ -n "$mismatches" ]; then
    echo "ERROR: Existing GitHub Release $TAG disagrees on: $mismatches." >&2
    exit 1
fi

actual_notes_sha256=$(printf '%s' "$release" | jq -j '.body' | sha256sum | awk '{print $1}')
if [ "$actual_notes_sha256" != "$NOTES_SHA256" ]; then
    echo "ERROR: Existing GitHub Release $TAG disagrees on: release notes body." >&2
    exit 1
fi

echo "Existing GitHub Release for $TAG has verified public identity and provenance."
echo "exists=true" >> "$OUTPUT_FILE"
