# Session 051 — Issue 155 Release Automation

## Objective

Complete issue 155's two-phase manual release flow: prepare a reviewed release
commit from a semantic-version dropdown, then publish the crate, GitHub Release,
and packaged artifacts.

## Design

- Reused the already-merged P11 publication path in `.github/workflows/release.yml`.
  It already owns the canonical annotated tag, crates.io publication, verified
  multi-architecture GHCR image, GitHub Release, SBOM, and platform archives.
- Added `.github/workflows/prepare-release.yml` as the missing first phase. It
  accepts `patch`, `minor`, or `major`, supports a no-write dry run, requires a
  default-branch dispatch, validates exact tag/branch nonexistence, and opens a
  `release/vX.Y.Z` pull request.
- The real PR path uses the organization's auto-commit GitHub App installation
  token. A PR created with `GITHUB_TOKEN` would not start normal pull-request
  workflows, so silently falling back would leave the release commit without
  the repository's required CI evidence.
- Added `scripts/prepare-release.sh` as the deterministic local and workflow
  implementation. It updates `Cargo.toml`, all tracked lockfiles containing the
  root path package, public dependency examples, `.llm/context.md`, and the dated
  changelog section/link graph; it then validates locked Cargo metadata and the
  repository documentation policy.

## Verification

- Data-driven tests cover all three bump choices and every synchronized output.
- Negative tests cover invalid input, invalid dates, empty release notes,
  duplicate release sections, missing lockfile identity, and arithmetic
  overflow.
- CI policy tests pin the workflow choice input, non-cancellable concurrency,
  least privilege, GitHub App token, branch/tag collision checks, recovery
  artifact, PR creation, and handoff to the publication workflow.

## Exact-head CI follow-up

The first PR-head macOS Nextest run exposed a platform-specific date-validation
bug: `prepare-release.sh` used GNU `date -d`, which BSD `date` does not provide.
The validator now performs strict Gregorian calendar checks in Bash, including
century leap-year rules, so release preparation has identical behavior on the
Linux, macOS, and Windows CI runners. The regression matrix includes a valid
leap day plus invalid leap-day, range, and year-zero cases.

Bugbot also identified that the workflow invoked locked Cargo metadata without
first installing the repository's pinned Rust toolchain. Prepare Release now
reads `rust-toolchain.toml` after checkout and installs that exact toolchain
before running the transformer; a policy-ordering test prevents regression.

A later review caught that the generated release comparison used the newest
dated changelog section instead of the actual pre-bump package identity. Since
the repository has `v0.3.0` and `v0.4.0` tags but no matching dated sections,
that would have skipped both tags. The comparison now always starts at the
pre-bump `Cargo.toml` version; the fixture intentionally keeps an older dated
section to prove the two identities cannot be confused again.

The existing documentation checker originally recognized compare endpoints
only when they had dated changelog sections. It now also accepts an exact local
`vX.Y.Z` Git tag, preserving typo detection while supporting this repository's
real `v0.3.0` and `v0.4.0` releases. A Git-backed fixture proves an absent tag
still fails and an existing immutable tag passes.

The replacement macOS lane then found one last fixture-only assumption: the
test seams hard-coded Linux's `/bin/true`, while macOS provides
`/usr/bin/true`. They now invoke `true` through `PATH`, matching every required
runner. The same head's ASan suite passed all tests before reproducing the
repository's known process-exit leak signature (29 allocations, about 39.8 KiB),
so that job is classified for an exact-head retry rather than a code change.
