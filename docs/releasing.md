# Release Runbook

Signal Fish Server releases use one version identity across source, crates.io,
GitHub Releases, and GHCR. A release is complete only when all four artifacts
resolve to the same tagged commit.

## Prepare the Release Commit

1. Set `[package].version` in `Cargo.toml` and refresh `Cargo.lock`.
2. Move the release notes from `[Unreleased]` to an exact
   `## [X.Y.Z] - YYYY-MM-DD` section in `CHANGELOG.md` and update its comparison
   links.
3. Push the release commit to the default branch and wait for the required CI
   and documentation workflows to pass on that commit.

Do not create or move the version tag by hand for the normal manual path. Run
the **Release - Publish Crate** workflow with `release_version=X.Y.Z`. It creates
an annotated `vX.Y.Z` tag before publication, or verifies that an existing tag
is annotated and already points at the exact release commit.

The release workflow directly calls the reusable container workflow. It does
not depend on a tag created with `GITHUB_TOKEN` starting a second workflow.
Publication proceeds in this order:

1. validate the Cargo, changelog, source, and tag identity;
2. verify required CI for the source commit;
3. create or validate the immutable annotated tag;
4. build and verify the versioned multi-architecture GHCR manifest;
5. publish the crate idempotently;
6. create or complete the GitHub Release and record the image digest;
7. attach the SBOM and available platform binaries.

If a run stops after creating one artifact, rerun the same version. The retry
accepts only artifacts that prove the same source revision. Existing version
tags are never moved; missing aliases are repaired from the verified digest.

## Direct Tags and Historical Backfills

A human-pushed annotated `vX.Y.Z` tag remains supported. It must match the
version in that commit's `Cargo.toml`; the release and container workflows fail
closed for a lightweight, moved, or mismatched tag.

To backfill GHCR tags for a historical release, dispatch **Docker Publish** from
the default branch with `release_tag=vX.Y.Z`. The workflow checks out that tag,
validates its annotated source commit, and builds from it. It never aliases a
historical version to `latest`.

## Verify the Published Image

The GitHub Release notes record the manifest digest and source revision. Repeat
the workflow's verification locally with Docker Buildx and `jq`:

```bash
VERSION=0.4.0
TAG="v${VERSION}"
IMAGE=ghcr.io/ambiguous-interactive/signal-fish-server
SOURCE_REVISION=$(git rev-list -n 1 "$TAG")
SHA_TAG="sha-${SOURCE_REVISION:0:7}"

bash scripts/verify-release-image.sh \
  "$IMAGE" "$SOURCE_REVISION" "$VERSION" \
  "$TAG" "$VERSION" "$SHA_TAG"
```

The command prints the shared digest only after proving that all supplied tags
resolve to one manifest, the platform set is exactly `linux/amd64`,
`linux/arm64`, and `linux/arm/v7`, and every platform image carries matching
`org.opencontainers.image.revision` and
`org.opencontainers.image.version` labels.

Never repair a failed release by moving an existing version tag. Investigate
the conflicting artifact, preserve it for audit, and publish a new patch
version if the bytes or source identity differ.
