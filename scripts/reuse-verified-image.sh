#!/usr/bin/env bash
# Reuse an immutable container digest and repair any missing immutable aliases.

set -euo pipefail

: "${IMAGE:?IMAGE is required}"
: "${SOURCE_REVISION:?SOURCE_REVISION is required}"
: "${SHA_TAG:?SHA_TAG is required}"
: "${IMAGE_VERSION:?IMAGE_VERSION is required}"
: "${IS_RELEASE:?IS_RELEASE is required}"
: "${IS_BACKFILL:?IS_BACKFILL is required}"
: "${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"

VERIFY_RELEASE_IMAGE_SCRIPT=${VERIFY_RELEASE_IMAGE_SCRIPT:-scripts/verify-release-image.sh}

tag_exists() {
  local reference=$1
  local inspect_output
  if inspect_output=$(docker buildx imagetools inspect "$reference" 2>&1); then
    return 0
  fi
  if grep -Eqi 'not found|manifest unknown|name unknown' <<< "$inspect_output"; then
    return 1
  fi
  echo "ERROR: Could not determine whether $reference exists; refusing to rebuild over an unknown registry state." >&2
  printf '%s\n' "$inspect_output" >&2
  exit 1
}

# The sha-* alias is immutable for both rolling and release events. Once any
# immutable alias exists, verify its identity and repair every missing alias
# from those exact bytes instead of rebuilding.
desired_tags=("$SHA_TAG")
preserve_historical_sha=false
if [ "$IS_RELEASE" = "true" ]; then
  : "${RELEASE_TAG:?RELEASE_TAG is required for a release}"
  : "${RELEASE_VERSION:?RELEASE_VERSION is required for a release}"
  if [ "$IS_BACKFILL" = "true" ]; then
    # Historical main images can carry image.version=latest. Their immutable
    # sha-* alias must neither be moved nor treated as a same-version release
    # candidate; only release/version aliases are eligible for reuse.
    desired_tags=("$RELEASE_TAG" "$RELEASE_VERSION")
    if tag_exists "${IMAGE}:${SHA_TAG}"; then
      preserve_historical_sha=true
    fi
  else
    desired_tags=("$RELEASE_TAG" "$RELEASE_VERSION" "$SHA_TAG")
  fi
fi

existing_tags=()
missing_tags=()
for tag in "${desired_tags[@]}"; do
  if tag_exists "${IMAGE}:${tag}"; then
    existing_tags+=("$tag")
  else
    missing_tags+=("$tag")
  fi
done

if [ "${#existing_tags[@]}" -eq 0 ]; then
  publish_sha=true
  verify_sha=true
  if [ "$preserve_historical_sha" = "true" ]; then
    publish_sha=false
    verify_sha=false
  fi
  {
    echo "reuse=false"
    echo "publish_sha=$publish_sha"
    echo "verify_sha=$verify_sha"
  } >> "$GITHUB_OUTPUT"
  exit 0
fi

digest=$(GITHUB_OUTPUT='' bash "$VERIFY_RELEASE_IMAGE_SCRIPT" \
  "$IMAGE" "$SOURCE_REVISION" "$IMAGE_VERSION" "${existing_tags[@]}")
if [ "${#missing_tags[@]}" -gt 0 ]; then
  for tag in "${missing_tags[@]}"; do
    docker buildx imagetools create --tag "${IMAGE}:${tag}" "${IMAGE}@${digest}"
  done
fi
if [ "$IS_BACKFILL" = "true" ] && [ "$preserve_historical_sha" != "true" ]; then
  docker buildx imagetools create --tag "${IMAGE}:${SHA_TAG}" "${IMAGE}@${digest}"
fi
echo "Reusing verified immutable image digest $digest"
{
  echo "reuse=true"
  echo "digest=$digest"
  echo "publish_sha=false"
  if [ "$preserve_historical_sha" = "true" ]; then
    echo "verify_sha=false"
  else
    echo "verify_sha=true"
  fi
} >> "$GITHUB_OUTPUT"
