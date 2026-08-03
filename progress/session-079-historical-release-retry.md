# Session 079 — Historical release retry completion

## Scope

Complete P36 by exercising the merged historical GitHub Release retry against
v0.5.2, fixing any newly exposed integrity defect, and verifying the public
Release artifact set.

## Registry retry root cause

The first post-merge retry, Release workflow run 30769058595, resolved the
immutable annotated v0.5.2 tag and passed its CI preflight, then failed in the
crates.io idempotency probe before any publication or asset mutation. The
published archive had the expected registry checksum and exact source revision
`09238c36ab8b086b13a5e50d679df51e32376134`, but its clean Cargo VCS metadata
omitted `git.dirty`. The probe incorrectly required an explicit `false`.

Cargo's canonical clean package form omits that optional field; dirty package
metadata emits `true`. The corrected probe accepts an omitted field or explicit
`false`, while retaining fail-closed checks for `true`, non-boolean values,
malformed metadata, source-revision drift, registry checksum drift, lookup
outages, and archive-download failures. A data-driven failure-first regression
test uses the real clean metadata shape, and the corrected script succeeds
against the immutable crates.io 0.5.2 archive.

## Adversarial Release audit

The first hostile review found that the inline existing-Release gate checked
only its tag before replacing the SBOM. A draft Release could therefore be
mutated before the later public-state check failed, while a prerelease or stale
body could finish green without recording the expected source revision and
GHCR digest. Token-presence policy tests did not execute those states.

The gate is now a fixture-testable helper that distinguishes an absent Release
from API failures and validates the expected tag, name, non-draft/non-prerelease
state, exact notes, source revision, and image digest. The publish job invokes
it before SBOM replacement, and binary recovery repeats the public identity and
provenance check before uploading archives. Data-driven cases cover absence,
success, every metadata mismatch, stale notes that retain provenance tokens,
malformed API data, and non-404 failure. The Release Runbook and workflow
commentary now describe the asset-only retry behavior directly.

## Validation and publication

Focused release-publish tests and a live read-only registry probe pass locally.
The full Rust and repository policy suites pass on the settled worktree. Three
adversarial review rounds exercised registry metadata, public Release identity,
exact-byte notes integrity, retry ordering, and binary recovery; the final pass
reported zero findings. Hosted PR validation, the final default-branch retry,
and public Release asset verification remain in progress.

The first retry after PR #246, Release run 30770844614, passed the corrected
registry probe, all six binary builds, immutable GHCR digest reuse, public
Release creation, and exact metadata validation. It then failed before the
first asset mutation because `gh release upload` ran from the parent of the
split checkouts and attempted repository discovery through an absent `.git`
directory. The follow-up passes `--repo "$GITHUB_REPOSITORY"` to both SBOM and
binary asset-only uploads and locks that independence into workflow policy.

PR #247 merged that correction at `6d8499897f271b5a477688d07c71dfe55f758da6`.
Historical Release run 30772076461 then completed successfully from the later
default branch while preserving the immutable source revision
`09238c36ab8b086b13a5e50d679df51e32376134` and GHCR digest
`sha256:efede9dbed5cba2d7f1c09b2143d568a91bc731e6510fd8fbdc81fe66d800d4c`.
Independent public verification matched the exact Release notes, opened all
six platform archives, checked all six checksum files, and confirmed the
CycloneDX SBOM. P36 is therefore complete.
