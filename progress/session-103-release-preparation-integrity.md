# Session 103 — Release-preparation integrity

## Scope and prioritization

PR #299 from session 102 was merged and fully green. Current `main` was green,
with no draft pull requests. The highest delivery blocker was issue #305: the
successful Prepare Release run opened PR #304, but five CI lanes deterministically
failed the same version-sensitive resolver test. The open `lru` 0.18.2 update in
PR #302 was also relevant because it fixes upstream panic-safety in `pop`, which
the coordination deduplication path uses.

The gameplay-risk frontier remains #290, but its registered resolution requires
a hosted evidence window; it was not mixed into this deterministic release fix.

## Root cause and RED evidence

Release preparation updated the package and release branch identity from
`0.5.2` to `0.5.3`, but `prepared_release_resolver_handles_every_remote_state`
hard-coded `release/v0.5.2`. AddressSanitizer, MSRV, Linux/macOS Nextest, and
coverage all failed at that same assertion. The workflow validated the release
file diff but did not execute the version-sensitive release contract tests before
pushing the branch, so a successful preparation run could create a known-red PR.

A second independent contract gap permitted that exact patch release even though
Unreleased contains multiple `**Breaking...:**` notes and names `0.6.0` as their
release floor. The workflow asked a maintainer for a bump and trusted the answer
without checking it against the compatibility declaration.

Both regression tests were observed RED before the fix:

- breaking notes accepted a `0.5.2 -> 0.5.3` patch;
- the workflow policy test proved no prepared-state test gate existed.

## Fix

- Release-resolver fake-remote identities and assertions derive from
  `CARGO_PKG_VERSION`; the AWK portability test likewise uses the package version
  and a checked next patch rather than frozen repository versions.
- The release fixture's fuzz dependency now follows the fixture version, which
  makes non-`1.2.3` semver cases genuine end-to-end preparations.
- `prepare-release.sh` rejects insufficient bumps before mutation: breaking
  notes require minor-or-major during `0.x` and major after `1.0`.
- Prepare Release runs `release_prepare_tests` and
  `workspace_lockfile_consistency` against the prepared tree before resolving,
  pushing, or opening its release PR.
- The root lockfile incorporates `lru` 0.18.2; the native and fuzz standalone
  graphs already held that version.

The adversarial sweep then closed three adjacent integrity gaps instead of
carrying them forward:

- Frozen-v2 authority grants now show their actual `reason: null` field in both
  guides and the canonical sample, and typed `RoomNotFound` authority denials map
  exhaustively to `ROOM_NOT_FOUND`.
- The obsolete performance suite, which benchmarked local mock broadcast and
  `SmallVec` types after their production counterparts were removed, is deleted
  together with the direct `smallvec` dev dependency and stale guidance.
- Issue #300's `INVALID_TOKEN`, `AUTHENTICATION_REQUIRED`, three future-backend
  app-status codes, and `SERVICE_UNAVAILABLE` remain decode-compatible in Rust
  but are explicitly reserved. AsyncAPI and client guidance now expose only
  outcomes reachable with the shipped backend, protected by exact emitted-set
  equality, exhaustive classification, and production-reference checks rather
  than one-way token presence.

## Validation

- `cargo test --locked --test release_prepare_tests` — 20/20 pass.
- `cargo test --locked --test workspace_lockfile_consistency` — 6/6 pass.
- Focused release-workflow, authority-mapping, and exact emitted-error-code
  contract tests pass.
- A real-worktree `--bump patch` preparation fails before mutation with the
  expected breaking-change diagnostic.
- Full mandatory and session-end validation follows before publication.

## Changelog classification

The release gate is maintainer-visible, the authority mapping and emitted-code
contract are client-visible corrections, and the dependency change affects the
shipped graph. `CHANGELOG.md` records each under Unreleased. Deleting tests and
stale internal guidance has no independent runtime behavior to advertise.

## Remaining frontier

P53 has 2/20 eligible scheduled relay-timing allocations per OS. P56 has 3/20
eligible scheduled H14 attempts in its fixed cohort; neither pre-registered
hosted decision is weakened or declared complete. Issue #301 is the next
live-gameplay investigation and should first improve missing-candidate-pair
diagnostics before inferring a production transport fix from a single hosted
occurrence.
