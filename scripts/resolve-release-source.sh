#!/usr/bin/env bash
# Resolve the one immutable version/tag/source identity used by release.yml.
set -euo pipefail

EVENT_NAME=${RELEASE_EVENT_NAME:-${GITHUB_EVENT_NAME:-}}
DEFAULT_BRANCH=${RELEASE_DEFAULT_BRANCH:-}
EVENT_REF=${RELEASE_EVENT_REF:-${GITHUB_REF:-}}
OUTPUT_FILE=${RELEASE_OUTPUT_FILE:-${GITHUB_OUTPUT:-}}

# Preserve the dispatch revision's parser before a retry detaches the worktree
# to a historical tag. Tests may inject an already-isolated helper explicitly.
resolver_scratch=
cleanup() {
    if [ -n "$resolver_scratch" ]; then
        rm -rf -- "$resolver_scratch"
    fi
}
trap cleanup EXIT
if [ -n "${READ_TOML_SCRIPT:-}" ]; then
    readonly READ_TOML_SCRIPT
else
    scratch_base=${RUNNER_TEMP:-${TMPDIR:-/tmp}}
    resolver_scratch=$(mktemp -d "${scratch_base}/release-resolver.XXXXXX")
    cp scripts/read-toml-string.sh "${resolver_scratch}/read-toml-string.sh"
    readonly READ_TOML_SCRIPT="${resolver_scratch}/read-toml-string.sh"
fi

if [ -z "$EVENT_NAME" ] || [ -z "$DEFAULT_BRANCH" ] || [ -z "$EVENT_REF" ] || [ -z "$OUTPUT_FILE" ]; then
    echo "ERROR: release event, default branch, ref, and output file are required." >&2
    exit 2
fi

read_cargo_version() {
    bash "$READ_TOML_SCRIPT" Cargo.toml version package 2>/dev/null || true
}

validate_annotated_tag() {
    local candidate_tag=$1
    if [ "$(git cat-file -t "refs/tags/${candidate_tag}" 2>/dev/null || true)" != "tag" ]; then
        echo "ERROR: Existing release tag ${candidate_tag} is lightweight; releases require an annotated tag." >&2
        exit 1
    fi
}

if [ "$EVENT_NAME" = "workflow_dispatch" ]; then
    if [ "$EVENT_REF" != "refs/heads/${DEFAULT_BRANCH}" ]; then
        echo "ERROR: Release publication must be dispatched from ${DEFAULT_BRANCH}; got ${EVENT_REF}." >&2
        exit 1
    fi

    dispatch_revision=$(git rev-parse "HEAD^{commit}")
    version=$(read_cargo_version)
    if [ -z "$version" ]; then
        echo "ERROR: Could not extract [package].version from Cargo.toml." >&2
        exit 1
    fi
    tag="v${version}"

    set +e
    tag_result=$(git ls-remote --exit-code --tags origin "refs/tags/${tag}" 2>&1)
    tag_status=$?
    set -e
    if [ "$tag_status" -eq 0 ]; then
        git fetch --force origin "refs/tags/${tag}:refs/tags/${tag}"
        validate_annotated_tag "$tag"
        source_revision=$(git rev-parse "refs/tags/${tag}^{commit}")
        if ! git merge-base --is-ancestor "$source_revision" "$dispatch_revision"; then
            echo "ERROR: Existing release tag ${tag} was not reachable from dispatched commit ${dispatch_revision}." >&2
            exit 1
        fi
        echo "Reusing existing annotated tag ${tag} at ${source_revision}."
    elif [ "$tag_status" -eq 2 ]; then
        source_revision=$dispatch_revision
    else
        echo "ERROR: Failed to check release tag ${tag}: ${tag_result}" >&2
        exit "$tag_status"
    fi
elif [ "$EVENT_NAME" = "push" ]; then
    case "$EVENT_REF" in
        refs/tags/v*) ;;
        *)
            echo "ERROR: Release tag events must use refs/tags/v*: ${EVENT_REF}." >&2
            exit 1
            ;;
    esac
    tag=${EVENT_REF#refs/tags/}
    version=${tag#v}
    event_revision=$(git rev-parse "HEAD^{commit}")
    git fetch --force origin "refs/tags/${tag}:refs/tags/${tag}"
    validate_annotated_tag "$tag"
    source_revision=$(git rev-parse "refs/tags/${tag}^{commit}")
    if [ "$source_revision" != "$event_revision" ]; then
        echo "ERROR: ${tag} resolves to ${source_revision}, not release commit ${event_revision}." >&2
        exit 1
    fi
else
    echo "ERROR: Unsupported release event: ${EVENT_NAME}." >&2
    exit 1
fi

if [[ ! "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
    echo "ERROR: Release version is not canonical semantic version text: ${version}" >&2
    exit 1
fi

git fetch --no-tags origin \
    "refs/heads/${DEFAULT_BRANCH}:refs/remotes/origin/${DEFAULT_BRANCH}"
if ! git merge-base --is-ancestor "$source_revision" "origin/${DEFAULT_BRANCH}"; then
    echo "ERROR: Release source ${source_revision} is not merged into ${DEFAULT_BRANCH}." >&2
    exit 1
fi

git checkout --detach "$source_revision"
cargo_version=$(read_cargo_version)
if [ "$version" != "$cargo_version" ] || [ "$tag" != "v${cargo_version}" ]; then
    echo "ERROR: Workflow version (${version}), Git tag (${tag}), and source Cargo.toml (${cargo_version}) disagree." >&2
    exit 1
fi
escaped_version=${version//./\\.}
if ! grep -Eq "^## \\[${escaped_version}\\]( - [0-9]{4}-[0-9]{2}-[0-9]{2})?$" CHANGELOG.md; then
    echo "ERROR: CHANGELOG.md has no ## [${version}] release section, with or without a date." >&2
    exit 1
fi

{
    echo "version=$version"
    echo "tag=$tag"
    echo "source_revision=$source_revision"
} >> "$OUTPUT_FILE"
