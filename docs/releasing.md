# Release Runbook

Signal Fish Server releases use one version identity across source, crates.io,
GitHub Releases, and GHCR. A release is complete only when all four artifacts
resolve to the same tagged commit.

## Prepare the Release Commit

Run the **Prepare Release** workflow from the default branch:

1. Choose `patch`, `minor`, or `major` from the `bump` dropdown. Use `dry_run`
   first when you only want to validate and inspect the generated diff.
2. The workflow computes the next version from `Cargo.toml`, updates the root
   package and every tracked lockfile that embeds it, synchronizes public dependency
   examples and project metadata, and moves `[Unreleased]` into an exact
   `## [X.Y.Z] - YYYY-MM-DD` changelog section with corrected comparison links.
3. Review and merge the generated `release/vX.Y.Z` pull request only after its
   normal CI and documentation workflows pass on the exact release commit.

The non-dry-run path requires the secret `AUTO_COMMIT_APP_PRIVATE_KEY` plus one
GitHub App identifier. The preferred identifier is the repository variable
`AUTO_COMMIT_APP_CLIENT_ID`; the existing `AUTO_COMMIT_APP_ID` secret remains a
compatible fallback, and the client ID takes precedence when both are present.
To migrate without an outage, add and verify the client-ID variable before
removing the App-ID secret. The installed GitHub App needs read/write access to
repository contents and pull requests. The workflow deliberately uses its
installation token when pushing the branch and opening the pull request so the
generated `pull_request` event starts normal CI and avoids the
`github-actions[bot]` approval hold. Equivalent pull-request events created with
the built-in `GITHUB_TOKEN` can be left waiting for approval, and most other
events created by that token do not start new workflow runs.

Preparation pins the exact default-branch dispatch commit before validating App
credentials. If a later live step fails, recovery capture is attempted only
from that valid checkout and cannot replace the primary failure with a
secondary Git error. Credential-free dry runs do not generate an App token or a
recovery artifact.

For local recovery or troubleshooting, run the same deterministic transformer
from a clean default-branch checkout, inspect the diff, and open the release PR
normally:

```bash
bash scripts/prepare-release.sh --bump patch
```

Do not create or move the version tag by hand for the normal manual path. Run
the **Release - Publish Crate** workflow from the default branch without a
version input. It derives the reviewed version from `Cargo.toml`, eliminating a
second operator-entered identity. When the tag is absent, it walks the complete
first-parent history and selects the unique commit that introduced that package
version. This preserves the reviewed release candidate if documentation or
workflow fixes reach the default branch before publication, and it fails closed
if history is shallow or a version was reused. If the matching tag already
exists after a partial run, the workflow requires that it is annotated, on the
default branch's first-parent history, reachable from the dispatched
default-branch commit, exactly at the unique commit that introduced the
version, and consistent with the tagged Cargo and changelog metadata; the retry
then publishes that immutable source commit.

The release workflow directly calls the reusable container workflow. It does
not depend on a tag created with `GITHUB_TOKEN` starting a second workflow.
Publication proceeds in this order:

1. validate the Cargo, changelog, source, and tag identity;
2. verify required CI for the source commit;
3. query crates.io from an isolated temporary directory, reject conflicting
   published bytes/source, require the token only for an absent version, and
   dry-run the exact package from a clean checkout;
4. create or validate the immutable annotated tag;
5. build and verify the versioned multi-architecture GHCR manifest;
6. verify the source checkout is still clean and publish the crate idempotently;
7. create or validate the public GitHub Release and record the image digest;
8. attach the SBOM and available platform binaries.

If a run stops after creating one artifact, rerun the same version. The retry
accepts only artifacts that prove the same source revision. Existing version
tags are never moved; missing aliases are repaired from the verified digest.
Resolver, container, publication, and binary jobs load policy helpers from the
dispatched workflow revision in a checkout separate from the immutable tagged
source, so a workflow-only recovery fix can safely complete an older release.
The GitHub Release step reuses the already-verified tag without passing
`target_commitish`, never patches an existing Release's identity or notes, and
requires the expected public name, non-draft/non-prerelease state, exact notes,
source revision, and GHCR digest before replacing any SBOM asset. Binary
recovery repeats the public identity and provenance check immediately before
its asset-only `gh release upload --clobber` operation.
Registry responses and downloaded `.crate` files remain under `RUNNER_TEMP`,
and the final untracked-aware cleanliness gate prevents publication probes from
silently changing the package contents. Never bypass that boundary with
`cargo publish --allow-dirty`.

## Direct Tags and Historical Backfills

A human-pushed annotated `vX.Y.Z` tag remains supported. Its target must be the
unique commit on the default branch's first-parent history that introduced
`X.Y.Z` in `Cargo.toml`; a later same-version commit is not a release source. It
does not have to remain the current default-branch tip, so a queued run remains
valid if the branch advances. Because that introduction commit necessarily
touches `Cargo.toml`, both required CI workflows must have an exact successful
default-branch push run for it that is still retained by GitHub Actions. A
second-parent pull-request head, a later same-version commit, or a commit whose
CI run records have expired cannot be published through this path. Human tag
pushes enter only through the release workflow, which invokes Docker
publication after CI preflight; Docker Publish does not have a second, ungated
tag-push trigger. Release publication fails closed for a lightweight, moved,
mismatched, or unproven tag.

To backfill GHCR tags for a historical release, dispatch **Docker Publish** from
the default branch with `release_tag=vX.Y.Z`. The workflow checks out that tag,
requires its annotated source commit to remain the unique first-parent commit
that introduced `X.Y.Z`, and builds from it. It never aliases a
historical version to `latest`, and it updates the mutable `X.Y` alias only when
`vX.Y.Z` is the highest canonical annotated patch tag in that release line.
Older backfills and delayed retries still repair missing immutable `sha-*`,
`vX.Y.Z`, and `X.Y.Z` aliases without rolling `X.Y` backward. Likewise, a
delayed default-branch run updates `latest` only while its source revision is
still the current remote default-branch head. Before moving `X.Y`, the workflow
also verifies the existing alias's version, source revision, and digest; a
newer version or conflicting equal-version digest is never overwritten.
Ambiguous, malformed, moved, or unavailable Git or registry state fails closed
before a mutable alias is moved, and any update or post-update verification
failure fails the publication run.

## Verify the Published Image

The GitHub Release notes record the manifest digest and source revision. Repeat
the workflow's verification locally with Docker Buildx and `jq`:

```bash
VERSION=0.4.0
TAG="v${VERSION}"
IMAGE=ghcr.io/ambiguous-interactive/signal-fish-server
SOURCE_REVISION=$(git rev-list -n 1 "$TAG")
SHA_TAG="sha-${SOURCE_REVISION}"

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
