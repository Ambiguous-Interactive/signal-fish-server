# Session 105 — Fortress Rollback 0.12 upgrade

## Scope and prioritization

Session 104 / PR #308 merged with no open pull request. P53 and P56 remain
registered hosted-evidence cohorts rather than deterministic in-repo work. The
new issue #309 was the next concrete dependency task on a real gameplay path:
both rollback-netcode compatibility fixtures pinned Fortress Rollback 0.10.0
while crates.io had released 0.12.0.

The separately tracked 0.6.0 preparation in issue #307 follows this work so the
fresh release candidate contains the updated compatibility graph. It remains a
separate session and PR under the repository's no-stacking policy.

## Failure-first contract

The native and WASM structural policy tests were first changed to require the
0.12.0 exact pins and lock identity. Both failed against the old manifests,
proving the repository guard observed the requested dependency boundary before
the implementation changed.

The completed guard also rejects the full obsolete supply-chain state in both
standalone graphs: original `bincode`, a missing `bincode-next` 2.1.0 package,
or restoration of `RUSTSEC-2025-0141` in either deny policy.

## Dependency and compatibility closure

- Both exact crates.io pins advance from Fortress Rollback 0.10.0 to 0.12.0.
- The native Signal Fish client remains exactly 0.8.0. The WASM client and
  Godot adapter remain exactly 0.9.0, and Godot-Rust remains exactly 0.4.5.
- Both lockfiles replace unmaintained `bincode` 2.0.1, `bincode_derive`,
  `virtue`, and `unty` with `bincode-next` 2.1.0 and `unty-next` 0.1.2.
- The two narrow advisory exceptions are removed rather than carried forward.
- The central push, pull-request, and daily security jobs now run `cargo deny`
  and `cargo audit` over every tracked Cargo lockfile. Dynamic inventory guards
  keep exact-release fixtures covered even though they intentionally opt out of
  Dependabot.
- Rust report identity, JavaScript validation, runner feature-tree validation,
  fixture documentation, PLAN, and the Unreleased changelog all name the new
  exact version.

No application adapter change was needed: Fortress 0.12.0 retains the socket,
session, event, frame, and metrics interfaces exercised by both fixtures.

## Changelog classification

This dependency update changes the maintained compatibility and supply-chain
guarantees of release-enforced gameplay fixtures. `CHANGELOG.md` records it
under Unreleased without implying a Signal Fish Server wire or runtime change.

## Validation

- The focused native/WASM structural policy tests pass after observed RED
  failures against the old pins.
- Both standalone `cargo deny` policies pass with no advisory exceptions.
- Both standalone lockfiles pass the independent `cargo audit` RustSec scan.
- The native crate checks and lints against Fortress Rollback 0.12.0.
- The real native two-process interoperability gate passes: both games confirm
  frame 601, exchange matching 1,323-message ledgers, compare nine matching
  checksums, drain their queues, and report zero stalls, waits, loss, overflow,
  retries, malformed input, or runtime errors.
- The local WASM build reaches the unchanged Godot-Rust custom-API boundary but
  cannot complete because this container has no Godot 4.5 executable. The
  pinned hosted Godot/Emscripten/Chromium workflow is the authoritative proof.
- Root formatting, strict all-target/all-feature Clippy, and the full locked
  all-feature test suite pass. The 313-test CI policy suite, documentation
  policy suites, workflow hygiene, actionlint, markdown, MSRV, tooling parity,
  diff, hook-readiness, pre-commit, and pre-push gates also pass. The profiled
  pre-commit rerun completed in 919 ms.
- The first adversarial review found five gaps: exact lock edge/source proof,
  WASM runtime-identity coverage, all-lockfile `cargo audit` coverage, exact
  workflow-trigger coverage, and the ignored progress file. All five were
  corrected. The independent follow-up review reported zero findings.
- Exact-head hosted CI and GitHub review evidence are appended before the
  session closes.

## Follow-up boundary

P53 and P56 keep their existing hosted sample requirements. After this PR is
merged green, issue #307 prepares the fresh 0.6.0 candidate from the updated
main tree; this session does not stack a release PR.
