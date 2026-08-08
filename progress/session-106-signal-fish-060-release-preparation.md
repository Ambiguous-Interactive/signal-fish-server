# Session 106 — Signal Fish Server 0.6.0 release preparation

## Scope and prioritization

P64 / PR #310 merged the remaining planned Fortress compatibility work, leaving
issue #307 as the bounded release-preparation dependency. P53 and P56 remain
fixed hosted-evidence cohorts: their in-repo implementation and evidence
instrumentation are complete, but neither may be declared finished before its
pre-registered sample threshold. No draft, in-progress, or dependency pull
request remained to incorporate.

## Release preparation closure

Prepare Release run `31270840717` succeeded from `main` at `6a065e0` with the
required minor bump and non-dry-run mode. It prepared exactly version `0.6.0` on
`release/v0.6.0`, ran the release-preparation and standalone-lockfile contract
tests before pushing the release branch and opening its PR, and reused PR #311
for the canonical eight-file candidate.

PR #311 changed only the root package and lock identity, native and fuzz
standalone lock identities, fuzz path dependency, dated 0.6.0 changelog section
and comparison links, library usage examples, and central context version. It
merged as `3b1ad61eb3657fa910a9390ea144725fd464e0df` after the required hosted
checks completed successfully. The obsolete 0.5.3 candidate remained closed
and unpublished.

This closes release _preparation_ only. P11's separately reviewed publication
phase still governs crates.io, the annotated tag and GitHub Release, versioned
GHCR images, checksums, SBOMs, and binary archives; none is inferred from the
prepared version in the repository. Issue #312 records that publication and its
immutable cross-artifact verification as the next bounded release operation.

The carry-forward audit also corrected two documentation drifts: P12/P13 now
distinguish their historical Fortress 0.10.0 baseline from P64's maintained
0.12.0 pins, and the development guide now documents the implemented `-c`
alias as config validation rather than claiming it does not exist.

## Hosted evidence frontier

The third consecutive eligible scheduled Relay Timing Observations allocation,
run `31242417565`, completed successfully on Linux, Windows, and macOS. P53 is
therefore 3/20 eligible allocations per OS under its unchanged fixed cohort.

P56 remains 3/20 eligible scheduled H14 attempts. Its first three eligible
Verification Nightly runs are `31070254464`, `31146055404`, and `31237473849`;
all three scenario-profile jobs passed. Neither cohort count excludes RED,
cancelled, missing, or incomplete attempts, and neither acceptance threshold is
weakened.

## Changelog classification

This session records already-merged release preparation and hosted-evidence
state. It changes no server API, configuration, wire behavior, runtime behavior,
performance, security contract, or public release artifact, so no new
`CHANGELOG.md` entry is appropriate.

## Validation

- Prepare Release run `31270840717` succeeded.
- PR #311 merged the exact 0.6.0 candidate as `3b1ad61`.
- The current release identities are guarded by `release_prepare_tests`,
  `workspace_lockfile_consistency`, and the documentation consistency policy.
- The short config-validation alias is pinned by
  `cli_tests::test_cli_validate_config_short`.
- Exact-main CI run `31270993620` completed 16/16 jobs successfully. Docker
  Publish `31270993611`, Advanced Safety `31270993610`, and Fortress WASM
  `31270993605` each completed 2/2 jobs successfully; no failure or rerun was
  required.
- Issue #307 was closed as completed with that exact-main evidence. Issue #312
  remains open for the separate public-artifact publication and verification.
- Independent adversarial review verifies that the PLAN/progress update neither
  overclaims public release publication nor weakens P53/P56 acceptance.
