# Session 072 — Strict WebSocket I/O deadlines

## Objective

Advance the highest-gameplay-impact ready in-repository work from the merged
session-071 baseline to one green pull request.

## Baseline and prioritization

- `main` and `origin/main` began at session 071's merge commit `d6e8388`.
- The connected GitHub repository had no open or draft pull request.
- Open issues were #204, #205, #206, #207, #213, #220, and #233.
- Three independent audits selected #233 as the only fully specified
  production-runtime issue and the natural completion of P24/P25. The broader
  #205 remains an umbrella; #206/#207/#213/#220 are open-ended research or
  optimization scopes, and #204 is front-end asset work off the gameplay path.
- Session 071's exact final tree is present on current `main`, and its final PR
  head passed all 11 applicable workflows with zero unresolved review threads.
  The connector cannot expose the squash merge's separate push-triggered runs,
  and no authenticated `gh` fallback is installed, so that main-event-only
  evidence was not overstated.

## Issue #233 — strict WebSocket I/O deadline semantics

### Red evidence

Tokio 1.53.1's `timeout_at` polls its inner future before its timer. A paused
time regression polled a gated operation pending, advanced exactly to its
deadline without polling it, released the operation so both futures were ready,
and observed `Ok(Ok(()))` instead of expiry. The same primitive behavior
affected four production boundaries:

1. selected outbound application/control socket writes;
2. server WebSocket Ping writes;
3. pre-authentication input; and
4. authenticated idle input.

The pre-authentication path also selected the frame before its timer without
biased ordering.

### Contract and fix

All four paths now use one half-open policy: completion or input must be
observed strictly before the deadline; readiness at or after it expires.

- Selected outbound writes and Ping writes share one absolute timer-first
  primitive. The selected write retains its class-aware queue/write deadline;
  Ping retains the existing policy that maps a write after outbound progress
  to `4002 slow_consumer` and an otherwise-idle write to `4003
  activity_timeout`.
- Authentication and idle reads share one close-first, timer-first primitive.
  Authentication keeps its connection-start absolute deadline; each
  authenticated read gets one idle window; `idle_timeout_secs = 0` supplies no
  deadline.
- Existing error frames, authoritative `4001 auth_timeout` / `4004
  idle_timeout` close reasons, metrics, Ping probe cleanup, delivery accounting,
  and cancellation-safe report frontiers are unchanged.
- The outer send loop is explicitly close-first so an already-requested
  lifecycle close cancels in-flight I/O before considering a simultaneously
  ready deadline or write.

### Deterministic regressions

Paused-time gated-future tables poll each operation pending before advancing
time. They prove:

- selected socket writes and Ping writes succeed just before the deadline and
  expire at or after it;
- authentication and idle input succeed just before the deadline and are
  rejected at or after it;
- an already-requested close beats a ready input and timer;
- no-deadline reads remain live across arbitrary time advancement; and
- the two inbound timeout kinds retain their exact error code, text, and close
  reason.

Existing complementary tests continue to prove delivery-report frontiers move
only after successful send/flush, Ping evidence and timeout ownership,
real-socket authentication/idle close codes, idle-disable behavior, selected
write eviction, and timeout-source policy.

The Pong deadline is intentionally unchanged: its evidence remains timestamped
and inclusive at the exact boundary. Teardown close/flush budgets and the
outbound batch wake timer retain their different best-effort/wakeup semantics
and were not swept.

## Documentation and changelog

The change is user-visible: clients at an exact configured boundary are now
closed rather than admitted. `[Unreleased] / Fixed`, the protocol/configuration
references, authentication and idle guidance, error-code reference, README,
config field comments, and `PLAN.md` all state the same exclusive semantics.

## Adversarial review

Three independent re-audits reached zero-revision on production behavior,
regression coverage, and documentation semantics after the following findings
were addressed:

- initial helper-only tests were replaced with production-seam tests that prove
  selected-write and Ping expiry side effects, including cancellation, semantic
  close ownership, metrics, probe cleanup, and just-before success;
- inbound tests now construct the production authentication/idle policy and
  prove its exact farewell contract; the send loop has direct close-precedence
  evidence;
- synchronous close/farewell assertions use non-blocking inspection so a
  regression fails loudly instead of hanging; and
- the release note, error-code reference, exclusions, and in-progress PLAN
  acceptance language were aligned with the implementation and current gate
  state.

The final coverage audit also confirmed the existing real-socket close-code,
idle-disable, Ping/Pong, delivery-accounting, and backpressure suites remain
valid complementary evidence.

## Verification

The complete local gauntlet is green:

- `cargo fmt --all -- --check`;
- strict locked Clippy across every target and feature with warnings denied;
- `cargo test --locked --all-features`, including 655 library unit tests and
  every default integration/e2e target (only the repository's intentional
  nightly/manual/expensive tests were ignored);
- `cargo deny --all-features check` and `scripts/check-ci-config.sh` (the
  existing allowed duplicate/unencountered-license warnings remain);
- MSRV, doc consistency, workflow hygiene, LLM file-size/example policy, hook
  readiness, and the dedicated doc/CI policy test suites; and
- pre-push worktree preflight plus the real staged pre-commit path.

The worktree pre-commit diagnostic initially took 1.44–1.53 seconds: profiling
attributed about 0.50 seconds to mounted-worktree `git status` discovery and
about 0.63 seconds to the lazy Rust panic classifier triggered by new test
attributes. The intended staged hook path avoids the worktree-status cost and
completed with profiling in 960 ms, below the repository's 1-second target.

The reviewed implementation head also passed every hosted workflow, including
the coincident full nightly and advanced-safety lanes.

## Zero-flake CI follow-up

A later documentation-validation run exposed a separate, pre-existing
nondeterministic gate: Lychee timed out while probing the unowned
`inria.hal.science` paper host. The same URL passed on the implementation head
and the failed job passed when retried, confirming that application behavior
and repository-owned documentation were not the cause.

Required link checks in both `doc-validation.yml` and `link-check.yml` now run
offline, so pull-request results depend only on the checkout. The weekly
external-link audit remains available for link-rot discovery, but is explicitly
scheduled and non-gating. A CI-config regression test locks that boundary.

The local fast-link helper also stopped forcing a repository-root `--base-url`;
that override made valid links in nested Markdown resolve from the wrong
directory. Its all-file offline run now deterministically validates all 184
tracked Markdown inputs with zero errors.

Focused verification for the follow-up passed:

- all 296 active CI-config tests (one intentional expensive test ignored);
- the complete 184-file fast offline link check;
- Lychee configuration validation, workflow hygiene, changed-file doc
  consistency, `actionlint`, YAML validation, formatting, and
  `git diff --check`.

## Publication

- Pull request:
  [#235](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/235)
- Green reviewed implementation head:
  `98910a2f9d7d49895bef80f44120efcceaa4f5da`.
- All 13 applicable hosted workflows succeeded: Advanced Safety, Browser
  Interop, CI, Documentation Validation, Formal Verification, Fortress
  Interop, Fortress WASM Interop, Link Check, Markdownlint, Spellcheck, Unused
  Dependencies, Verification Nightly, and WebRTC Interop. The two Dependabot
  auto-merge runs skipped as intended for this human-authored pull request.
- Cursor Bugbot reviewed that exact head and found no issues, and no inline
  review threads exist. Copilot was requested through both the reviewer API and
  a tagged comment but reported requester quota exhaustion. The prior
  repository audit found no distinct human contributor available to review a
  pull request authored by its sole human contributor.
- The pull request closes #233 and references its umbrella #205.
