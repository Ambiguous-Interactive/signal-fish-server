#!/usr/bin/env bash

# Move one mutable GHCR alias only when the immutable publication is still the
# authoritative Git source for that alias.

set -euo pipefail
export LC_ALL=C

fail() {
  echo "ERROR: $*" >&2
  exit 1
}

: "${ALIAS_KIND:?ALIAS_KIND is required}"
: "${IMAGE:?IMAGE is required}"
: "${DIGEST:?DIGEST is required}"
: "${SOURCE_REVISION:?SOURCE_REVISION is required}"
: "${IMAGE_VERSION:?IMAGE_VERSION is required}"

SOURCE_DIR=${SOURCE_DIR:-source}
VERIFY_RELEASE_IMAGE_SCRIPT=${VERIFY_RELEASE_IMAGE_SCRIPT:-scripts/verify-release-image.sh}
alias=""
should_update=true

[[ "$IMAGE" != *[[:space:]]* ]] || fail "image name contains whitespace"
[[ "$DIGEST" =~ ^sha256:[0-9a-f]{64}$ ]] || fail "digest must be a canonical sha256 OCI digest"
[[ "$SOURCE_REVISION" =~ ^[0-9a-f]{40}$ ]] || fail "source revision must be a full lowercase Git SHA"
[[ "$IMAGE_VERSION" != *$'\n'* && -n "$IMAGE_VERSION" ]] || fail "image version is invalid"
[ -f "$VERIFY_RELEASE_IMAGE_SCRIPT" ] || fail "image verifier is missing: $VERIFY_RELEASE_IMAGE_SCRIPT"

remote_lines() {
  local description=$1
  shift
  local response
  if ! response=$(git -C "$SOURCE_DIR" ls-remote "$@" 2>&1); then
    echo "ERROR: could not resolve $description; refusing to move a mutable alias." >&2
    printf '%s\n' "$response" >&2
    exit 1
  fi
  printf '%s' "$response"
}

parse_remote_line() {
  local line=$1
  local oid ref
  if [[ "$line" != *$'\t'* ]]; then
    return 1
  fi
  oid=${line%%$'\t'*}
  ref=${line#*$'\t'}
  if [[ ! "$oid" =~ ^[0-9a-f]{40}$ || -z "$ref" || "$ref" == *$'\t'* ]]; then
    return 1
  fi
  printf '%s\n%s\n' "$oid" "$ref"
}

resolve_latest_alias() {
  : "${DEFAULT_BRANCH:?DEFAULT_BRANCH is required for latest}"
  git -C "$SOURCE_DIR" check-ref-format --branch "$DEFAULT_BRANCH" >/dev/null 2>&1 ||
    fail "default branch is not a valid Git branch name"

  local response line parsed head ref count=0
  response=$(remote_lines "the current default-branch head" --heads origin "refs/heads/${DEFAULT_BRANCH}")
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    if ! parsed=$(parse_remote_line "$line"); then
      fail "invalid default-branch response from Git; refusing to move a mutable alias"
    fi
    head=${parsed%%$'\n'*}
    ref=${parsed#*$'\n'}
    [ "$ref" = "refs/heads/${DEFAULT_BRANCH}" ] ||
      fail "invalid default-branch response from Git; refusing to move a mutable alias"
    count=$((count + 1))
  done <<< "$response"
  [ "$count" -eq 1 ] || fail "invalid default-branch response from Git; expected one ref, found $count"

  if [ "$head" != "$SOURCE_REVISION" ]; then
    echo "Skipping mutable alias ${IMAGE}:latest: publication source $SOURCE_REVISION is not current ${DEFAULT_BRANCH} head $head."
    should_update=false
    return 0
  fi
  alias=latest
}

canonical_release_version() {
  local version=$1
  [[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]
}

patch_is_greater() {
  local candidate=$1
  local current=$2
  if [ "${#candidate}" -ne "${#current}" ]; then
    [ "${#candidate}" -gt "${#current}" ]
  else
    [[ "$candidate" > "$current" ]]
  fi
}

inspect_current_major_minor_alias() {
  local reference="${IMAGE}:${MAJOR_MINOR}"
  local raw_manifest inspect_status manifest_digest image_config alias_identity extra
  current_alias_exists=false
  current_alias_version=""
  current_alias_digest=""

  set +e
  raw_manifest=$(docker buildx imagetools inspect --raw "$reference" 2>&1)
  inspect_status=$?
  set -e
  if [ "$inspect_status" -ne 0 ]; then
    if [ "$raw_manifest" = "ERROR: ${reference}: not found" ] ||
       grep -Eqi '(^|[[:space:]:])(manifest unknown|name unknown)([[:space:]:]|$)' <<< "$raw_manifest"; then
      return 0
    fi
    echo "ERROR: could not inspect current mutable alias $reference; refusing to move it." >&2
    printf '%s\n' "$raw_manifest" >&2
    exit 1
  fi

  command -v jq >/dev/null 2>&1 || fail "'jq' is required to inspect current mutable alias $reference"
  if ! manifest_digest=$(printf '%s' "$raw_manifest" | jq -er '
    if .mediaType == "application/vnd.docker.distribution.manifest.list.v2+json"
       or .mediaType == "application/vnd.oci.image.index.v1+json"
    then
      [.manifests[]?
       | select(.platform.os != "unknown" and .platform.architecture != "unknown")]
      | if length > 0 then .[0].digest else error("no runnable manifests") end
    else
      error("not a manifest index")
    end
  '); then
    fail "invalid registry alias identity for $reference; refusing to overwrite it"
  fi
  [[ "$manifest_digest" =~ ^sha256:[0-9a-f]{64}$ ]] ||
    fail "invalid registry alias identity for $reference; refusing to overwrite it"
  if ! image_config=$(docker buildx imagetools inspect \
    --format '{{json .Image}}' "${IMAGE}@${manifest_digest}" 2>&1); then
    echo "ERROR: could not inspect current mutable alias image config for $reference." >&2
    printf '%s\n' "$image_config" >&2
    exit 1
  fi
  if ! alias_identity=$(printf '%s' "$image_config" | jq -er '
    (.config.Labels["org.opencontainers.image.revision"] // "") as $revision
    | (.config.Labels["org.opencontainers.image.version"] // "") as $version
    | select($revision != "" and $version != "")
    | [$revision, $version]
    | @tsv
  '); then
    fail "invalid registry alias identity for $reference; refusing to overwrite it"
  fi
  IFS=$'\t' read -r current_alias_revision current_alias_version extra <<< "$alias_identity"
  if [ -n "${extra:-}" ] || [[ ! "$current_alias_revision" =~ ^[0-9a-f]{40}$ ]] || \
     ! canonical_release_version "$current_alias_version" || \
     [ "${current_alias_version%.*}" != "$MAJOR_MINOR" ]; then
    fail "invalid registry alias identity for $reference; refusing to overwrite it"
  fi

  if ! current_alias_digest=$(GITHUB_OUTPUT='' bash "$VERIFY_RELEASE_IMAGE_SCRIPT" \
    "$IMAGE" "$current_alias_revision" "$current_alias_version" "$MAJOR_MINOR"); then
    fail "existing mutable alias verification failed for $reference"
  fi
  [[ "$current_alias_digest" =~ ^sha256:[0-9a-f]{64}$ ]] ||
    fail "existing mutable alias verifier returned an invalid digest for $reference"
  current_alias_exists=true
}

find_version_index() {
  local wanted=$1
  local index
  found_index=-1
  for index in "${!tag_versions[@]}"; do
    if [ "${tag_versions[$index]}" = "$wanted" ]; then
      found_index=$index
      return 0
    fi
  done
}

resolve_major_minor_alias() {
  : "${RELEASE_VERSION:?RELEASE_VERSION is required for a release alias}"
  : "${MAJOR_MINOR:?MAJOR_MINOR is required for a release alias}"
  canonical_release_version "$RELEASE_VERSION" ||
    fail "release version is not canonical semantic version text: $RELEASE_VERSION"
  [ "${RELEASE_VERSION%.*}" = "$MAJOR_MINOR" ] ||
    fail "release version $RELEASE_VERSION does not belong to alias $MAJOR_MINOR"

  local response line parsed oid ref base_ref tag version patch peeled index
  local current_patch release_patch
  local highest_patch="" highest_version=""
  tag_versions=()
  tag_objects=()
  tag_commits=()

  response=$(remote_lines "annotated ${MAJOR_MINOR}.x release tags" --tags origin "refs/tags/v${MAJOR_MINOR}.*")
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    if ! parsed=$(parse_remote_line "$line"); then
      fail "malformed tag response from Git; refusing to move a mutable alias"
    fi
    oid=${parsed%%$'\n'*}
    ref=${parsed#*$'\n'}
    peeled=false
    base_ref=$ref
    if [[ "$ref" == *'^{}' ]]; then
      peeled=true
      base_ref=${ref%'^{}'}
    fi
    [[ "$base_ref" == refs/tags/v* ]] ||
      fail "malformed tag ref '$ref'; refusing to move a mutable alias"
    tag=${base_ref#refs/tags/}
    version=${tag#v}
    if ! canonical_release_version "$version" || [ "${version%.*}" != "$MAJOR_MINOR" ]; then
      fail "non-canonical tag '$tag' exists in the ${MAJOR_MINOR}.x release namespace"
    fi

    find_version_index "$version"
    if [ "$found_index" -eq -1 ]; then
      index=${#tag_versions[@]}
      tag_versions+=("$version")
      tag_objects+=("")
      tag_commits+=("")
    else
      index=$found_index
    fi

    if [ "$peeled" = "true" ]; then
      [ -z "${tag_commits[$index]}" ] || fail "duplicate peeled response for $tag"
      tag_commits[index]=$oid
    else
      [ -z "${tag_objects[$index]}" ] || fail "duplicate tag response for $tag"
      tag_objects[index]=$oid
    fi
  done <<< "$response"

  [ "${#tag_versions[@]}" -gt 0 ] || fail "no canonical ${MAJOR_MINOR}.x release tags were returned"
  for index in "${!tag_versions[@]}"; do
    version=${tag_versions[$index]}
    [ -n "${tag_objects[$index]}" ] || fail "peeled response for v${version} has no tag object"
    [ -n "${tag_commits[$index]}" ] || fail "v${version} must be annotated"
    patch=${version##*.}
    if [ -z "$highest_patch" ] || patch_is_greater "$patch" "$highest_patch"; then
      highest_patch=$patch
      highest_version=$version
    fi
  done

  find_version_index "$RELEASE_VERSION"
  [ "$found_index" -ne -1 ] ||
    fail "release tag v${RELEASE_VERSION} must be annotated"
  [ "${tag_commits[$found_index]}" = "$SOURCE_REVISION" ] ||
    fail "release tag v${RELEASE_VERSION} does not resolve to the publication source $SOURCE_REVISION"

  if [ "$RELEASE_VERSION" != "$highest_version" ]; then
    echo "Skipping mutable alias ${IMAGE}:${MAJOR_MINOR}: v${RELEASE_VERSION} is older than canonical v${highest_version}."
    should_update=false
    return 0
  fi

  inspect_current_major_minor_alias
  if [ "$current_alias_exists" = "true" ]; then
    current_patch=${current_alias_version##*.}
    release_patch=${RELEASE_VERSION##*.}
    if patch_is_greater "$current_patch" "$release_patch"; then
      echo "Skipping mutable alias ${IMAGE}:${MAJOR_MINOR}: registry version ${current_alias_version} is newer than v${RELEASE_VERSION}."
      should_update=false
      return 0
    fi
    if [ "$current_alias_version" = "$RELEASE_VERSION" ]; then
      if [ "$current_alias_digest" = "$DIGEST" ]; then
        echo "Mutable alias ${IMAGE}:${MAJOR_MINOR} already resolves to verified ${RELEASE_VERSION} digest ${DIGEST}."
        should_update=false
        return 0
      fi
      fail "mutable alias ${IMAGE}:${MAJOR_MINOR} identity conflict: registry version ${current_alias_version} resolves to ${current_alias_digest}, expected ${DIGEST}"
    fi
  fi
  alias=$MAJOR_MINOR
}

case "$ALIAS_KIND" in
  latest) resolve_latest_alias ;;
  major-minor) resolve_major_minor_alias ;;
  *) fail "unsupported mutable alias kind: $ALIAS_KIND" ;;
esac

if [ "$should_update" != "true" ]; then
  exit 0
fi

docker buildx imagetools create --tag "${IMAGE}:${alias}" "${IMAGE}@${DIGEST}"
if ! verified_digest=$(GITHUB_OUTPUT='' bash "$VERIFY_RELEASE_IMAGE_SCRIPT" \
  "$IMAGE" "$SOURCE_REVISION" "$IMAGE_VERSION" "$alias"); then
  fail "mutable alias verification failed for ${IMAGE}:${alias}"
fi
[ "$verified_digest" = "$DIGEST" ] ||
  fail "mutable alias ${IMAGE}:${alias} resolved to $verified_digest after update, expected $DIGEST"
echo "Updated and verified mutable alias ${IMAGE}:${alias} at $DIGEST."
