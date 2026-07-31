# Session 070 — Relay synchronous fast path

**Branch:** `agent/session-070-relay-fast-path`
**Base:** `875057d` (PR #231, session 069)

## Objective

Advance the open optimization program in issue #207 through one falsifiable
gameplay-path hypothesis: determine whether constructing a dynamic async wait
for every healthy relay recipient contributes materially to the existing
16-player H2 saturation knee. Retain a production change only if the identical
allocator ledger and real-WebSocket workload both improve without weakening
delivery semantics.

## Starting evidence

- No open or draft pull request or dependency pull request existed.
- The exact tree merged through PR #231 had every applicable workflow green.
  The subsequent `main` push had no failures while its longest jobs were still
  running; its tree was byte-identical to the fully green PR head.
- The warmed classified queue itself performed zero steady-state allocations,
  but current fan-out still built `join_all` state around every recipient even
  though `try_send_delivery` normally resolved synchronously.
- The current post-P21 baseline was 6.0002, 7.0002, and 7.0002 allocation
  operations per relay for 2-, 8-, and 16-player rooms, with 1,112, 6,208, and
  12,096 allocated bytes respectively.

## Pre-registered decision rule

The optimization could land only if:

1. one shared delivery state machine preserved all accounting, trace,
   close-reason, cache-ownership, and timeout behavior;
2. multiple full recipients still waited concurrently rather than serially;
3. the deterministic allocator workload fell to at most four operations per
   healthy fan-out; and
4. repeated same-machine H2 comparison showed a material release-profile
   throughput or latency improvement beyond the isolated microbenchmark.

If H2 did not improve, the production patch would be rejected and the session
would pivot to issue #226.

## Red-green implementation

The allocation benchmark first gained the predicted healthy-fan-out ceiling.
It failed on unchanged production code at 24,577 operations over 4,096
two-player relays (6.0002 per relay).

The delivery contract is now split at its natural cancellation-safe boundary:

- `start_message_delivery_in_room` performs attempt accounting, connection
  ledger lookup, negotiated-class resolution, the non-blocking queue attempt,
  and every terminal fast outcome;
- only `Full` returns a `BackpressuredDelivery` carrying the exact logical
  message/cache, recipient handle, room, accounting state, and an absolute
  deadline captured when `Full` is observed; and
- `finish_backpressured_delivery_in_room` owns the unchanged bounded wait,
  timeout-close race, trace actions, and final outcome accounting. Its bound
  context makes cross-recipient continuation calls unrepresentable.

The continuation checks an already-expired deadline before polling the queue
send. Tokio polls a timeout's inner future first, so this guard prevents
capacity returned after the grace window from reviving an expired logical
delivery.

Room fan-out runs the first stage for every recipient synchronously. It creates
and awaits one concurrent `join_all` only when at least one recipient is
actually full. This avoids duplicating the state machine and preserves the
original maximum-of-recipient-waits latency bound.

A paused-clock mixed-recipient test proves that the healthy recipient is
enqueued before either full queue drains and that the second full recipient
can enqueue while the first remains blocked, which a serial wait loop cannot
satisfy. Separate delayed-continuation tests prove that only the unused
portion of the deadline captured at `Full` remains and that capacity returned
after expiry cannot enqueue the message. Existing classified full-retry,
shared-cache, accounted-drop, cancellation, channel-close, slow-consumer,
generation-pruning, and trace-validation suites remain the
broader oracle.

## Quantitative result

Five deterministic repeats of the identical allocator workload report:

| room | recipients | before ops/relay | after ops/relay | before bytes/relay | after bytes/relay |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 2 | 1 | 6.0002 | 3.0002 | 1,112 | 424 |
| 8 | 7 | 7.0002 | 4.0002 | 6,208 | 1,344 |
| 16 | 15 | 7.0002 | 4.0002 | 12,096 | 1,664 |

Allocation operations fall 42.9–50.0%, and allocated bytes fall 61.9–86.2%.
The separately measured warmed classified queue remains at zero operations and
zero bytes.

The canonical debug H2 grid remained exact with zero queue backpressure and
zero eviction. Three same-machine comparisons at 240 messages/s/player showed
roughly 3% higher completed throughput, but that was close enough to run noise
to require production-profile corroboration.

The release build did not saturate at the canonical 480 target. The checked-in
ignored `sixteen_player_relay_saturation_diagnostic_preserves_exact_delivery`
therefore retains the exact 960 and 1,920 messages/s/player inputs for manual
comparison. The command is:

```bash
cargo test --release --locked --all-features \
  --test sixteen_player_matrix_e2e \
  sixteen_player_relay_saturation_diagnostic_preserves_exact_delivery \
  -- --exact --ignored --nocapture --test-threads=1
```

Five alternating base/candidate pairs at 960 messages/s/player produced these
raw observations; the p99 column is microseconds:

| pair | base deliveries/s | candidate deliveries/s | base p99 | candidate p99 |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 220,492.9 | 226,388.8 | 35,554 | 21,572 |
| 2 | 224,689.1 | 220,897.6 | 23,738 | 42,995 |
| 3 | 219,561.2 | 227,814.8 | 48,767 | 12,620 |
| 4 | 221,473.4 | 229,617.0 | 39,146 | 9,368 |
| 5 | 219,868.5 | 226,951.1 | 45,924 | 14,649 |

The medians are 220,492.9 versus 226,951.1 completed deliveries/s (+2.9%)
and 39,146 versus 14,649 microseconds p99 (-62.6%). Every comparison delivered
the complete ledger with no backpressure event or slow-consumer eviction.
Measurements used the exact base `875057d` and the candidate production tree,
Rust 1.89.0, release profile, Linux aarch64 under WSL2, and a 12-core Qualcomm
host. These shared-process rates satisfy the pre-registered same-machine gate
but are diagnostic evidence, not portable production capacity claims; the
public changelog therefore reports the deterministic allocation reduction
rather than promoting the timing figures.

## PLAN and documentation cleanup

- Record P24 and current allocation/H2 evidence.
- Correct the stale open-issue inventory and historical completed-task text.
- Replace obsolete `/v3/ws`, `emptied_at`, ConformanceAuditor retrofit, and
  late-join `NewPeer` instructions with the shipped behavior.
- Consolidate the user-visible performance result into the existing Unreleased
  relay-allocation changelog entry.

## Changelog classification

The relay hot-path allocation reduction is user-visible performance work, so
the existing `[Unreleased] / Changed` allocation bullet records the
deterministic result rather than adding a second fragmented entry. The noisier
same-machine timing observations remain in this evidence log.

## Adversarial review

The first post-implementation review correctly rejected the draft for four
substantive reasons:

- the timeout began when the continuation was polled instead of when `Full`
  was observed;
- the continuation accepted free recipient/room arguments and could be
  cross-wired;
- the mixed-recipient test counted starts but did not distinguish concurrent
  from serial waits; and
- the public timing claim lacked a checked-in diagnostic input and raw samples.

The fix binds identity, queue, room, accounting state, and absolute deadline
into the full-path-only continuation, returns its bound identity with the
outcome, strengthens both paused-time oracles, checks in the manual saturation
diagnostic, records the five paired 960-rate observations above, and keeps
non-portable timing out of the changelog. The allocator ceiling also names its
single fixed sample-level operation.

A second review then exposed Tokio's inner-future-first timeout boundary:
capacity returned after an already-expired deadline could win on the
continuation's first poll. Another reviewer independently confirmed it against
the locked Tokio source. The final implementation uses a timer-first biased
select and the expired-then-drain regression proves exact timeout accounting
without a late enqueue. Two final exact-index reviews report zero substantive
findings.

## Verification

- Red allocator ceiling reproduced before the production change.
- Five-repeat post-change allocator benchmark passes exactly at 3.0002,
  4.0002, and 4.0002 operations and 424, 1,344, and 1,664 bytes per relay.
- Three canonical debug H2 candidate runs pass.
- Three base and three candidate release H2 runs pass at the canonical grid.
- Five alternating release base/candidate saturation comparisons pass.
- Focused mixed-recipient concurrency, delayed-continuation deadline, and
  expired-deadline/capacity-return tests pass, as do the async
  timeout/read-result policy scans.
- `cargo fmt --all -- --check`, warnings-denied all-target/all-feature Clippy,
  and the complete locked all-feature test suite pass.
- The mutation inventory is 349; all eight mutation feasibility guards pass
  and the 36-shard worst case remains 10 × 29 seconds = 290 seconds.
- Doc consistency, workflow hygiene, LLM size/example policies, Markdown and
  link-text checks, cargo-deny, MSRV/tooling parity, hook readiness,
  worktree pre-commit/pre-push, and the focused doc/CI policy suites pass.

## Publication and hosted evidence

- Pull request
  [#232](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/232)
  is open, ready for review, and mergeable against exact audited `main`
  `875057df9748184a824961c6265d26fad010298a`.
- The implementation head
  `3e4b149663da90a62b83514cadd0471f6ace2c16` completed all 14 applicable
  pull-request workflows successfully: Advanced Safety, Browser Interop, CI,
  Documentation Validation, Formal Verification, Fortress Interop, Fortress
  WASM Interop, Link Check, Markdownlint, Mutation Testing, Spellcheck, Unused
  Dependencies, Verification Nightly, and WebRTC Interop. The two Dependabot
  auto-merge runs skipped as intended for the draft and human-authored ready
  states.
- The first Advanced Safety attempt had no sanitizer finding: the production
  mixed-encoding relay test passed, while one reused chaos-proxy pause timing
  oracle raced in one integration binary after passing elsewhere in the same
  run. A failed-job retry passed the complete AddressSanitizer job and Miri
  again, so no unrelated production change was made.
- Cursor Bugbot reviewed that exact implementation head and reported no new
  issues. Copilot was explicitly retriggered but could not review because the
  requester quota was exhausted. The PR has no inline review threads.
- The repository exposes no CODEOWNERS, review team, or independent human
  reviewer candidate; the only discoverable recent human is the branch author.
  Marking the PR ready for review is therefore the available human
  notification path without inventing an assignee.

This publication-only follow-up records the immutable implementation-head
evidence. Its own exact head must independently complete hosted workflows and
reviewer retriggers before the session is closed.

## Closure

PR #232 completed at final reviewed head
`65256bb495c2f6ec586e7cb47d2e1eaec645a56d`. All 14 applicable workflows
passed, reviewer feedback was resolved, and the pull request merged as
`664f415bd90376237f860ffe65d500de1d1dd536`. That merge is the session's
canonical production state; the implementation head above remains the
immutable measurement reference.
