#!/usr/bin/env bash

# Verify that one or more GHCR tags resolve to one release manifest whose
# platforms and OCI identity labels match the tagged source revision.

set -euo pipefail

usage() {
  echo "Usage: $0 <image> <source-revision> <version> <tag> [<tag> ...]" >&2
}

if [ "$#" -lt 4 ]; then
  usage
  exit 2
fi

image=$1
expected_revision=$2
expected_version=$3
shift 3
tags=("$@")

for command in docker jq; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "ERROR: '$command' is required to verify a release image." >&2
    exit 1
  fi
done

if [[ ! "$expected_revision" =~ ^[0-9a-f]{40}$ ]]; then
  echo "ERROR: expected source revision must be a full 40-character lowercase Git SHA." >&2
  exit 1
fi

expected_platforms=$'linux/amd64\nlinux/arm/v7\nlinux/arm64'
resolved_digest=""

for tag in "${tags[@]}"; do
  reference="${image}:${tag}"
  inspect=$(docker buildx imagetools inspect "$reference")
  digest=$(printf '%s\n' "$inspect" | sed -n 's/^Digest:[[:space:]]*//p' | head -n 1)
  if [[ ! "$digest" =~ ^sha256:[0-9a-f]{64}$ ]]; then
    echo "ERROR: could not resolve an OCI digest for $reference." >&2
    exit 1
  fi

  if [ -z "$resolved_digest" ]; then
    resolved_digest=$digest
  elif [ "$digest" != "$resolved_digest" ]; then
    echo "ERROR: release tags do not resolve to one manifest: $reference is $digest, expected $resolved_digest." >&2
    exit 1
  fi
done

canonical_reference="${image}@${resolved_digest}"
raw_manifest=$(docker buildx imagetools inspect --raw "$canonical_reference")
media_type=$(printf '%s' "$raw_manifest" | jq -r '.mediaType // empty')
case "$media_type" in
  application/vnd.docker.distribution.manifest.list.v2+json|application/vnd.oci.image.index.v1+json) ;;
  *)
    echo "ERROR: $canonical_reference is not a multi-architecture manifest index (mediaType=$media_type)." >&2
    exit 1
    ;;
esac

actual_platforms=$(printf '%s' "$raw_manifest" | jq -r '
  .manifests[]
  | select(.platform.os != "unknown" and .platform.architecture != "unknown")
  | .platform.os + "/" + .platform.architecture
    + (if (.platform.variant // "") == "" then "" else "/" + .platform.variant end)
' | sort -u)
if [ "$actual_platforms" != "$expected_platforms" ]; then
  echo "ERROR: $canonical_reference platform set disagrees with release policy." >&2
  echo "Expected:" >&2
  printf '%s\n' "$expected_platforms" >&2
  echo "Actual:" >&2
  printf '%s\n' "$actual_platforms" >&2
  exit 1
fi

while IFS=$'\t' read -r manifest_digest platform; do
  [ -n "$manifest_digest" ] || continue
  image_config=$(docker buildx imagetools inspect \
    --format '{{json .Image}}' "${image}@${manifest_digest}")
  revision=$(printf '%s' "$image_config" | jq -r '.config.Labels["org.opencontainers.image.revision"] // empty')
  version=$(printf '%s' "$image_config" | jq -r '.config.Labels["org.opencontainers.image.version"] // empty')

  if [ "$revision" != "$expected_revision" ]; then
    echo "ERROR: $platform image label revision is '$revision', expected '$expected_revision'." >&2
    exit 1
  fi
  if [ "$version" != "$expected_version" ]; then
    echo "ERROR: $platform image label version is '$version', expected '$expected_version'." >&2
    exit 1
  fi
done < <(printf '%s' "$raw_manifest" | jq -r '
  .manifests[]
  | select(.platform.os != "unknown" and .platform.architecture != "unknown")
  | [
      .digest,
      (.platform.os + "/" + .platform.architecture
        + (if (.platform.variant // "") == "" then "" else "/" + .platform.variant end))
    ]
  | @tsv
')

if [ -n "${GITHUB_OUTPUT:-}" ]; then
  echo "digest=$resolved_digest" >> "$GITHUB_OUTPUT"
fi
printf '%s\n' "$resolved_digest"
