# Session 065 — Bounded recipient progress and WebSocket liveness

**Branch:** `agent/session-065-accountable-progress`
**Base:** `eff0307` (PR #219, session 064)

## Objective

Advance `PLAN.md` through the highest-impact open gameplay issue, #217:
distinguish a stalled recipient from one that drains below the offered reliable
rate, preserve exact lossy accountability, and close any production defect
exposed by the real-socket experiments.

## GitHub state at start

- No open or draft pull requests.
- No open Dependabot pull request required incorporation.
- PR #219's exact head was fully green; the new `main` push had no observed
  failure and three workflows still running at the audit boundary.
- Gameplay-impact order was #217, #211, then the research/tooling and design
  issues.

## Red evidence and root cause

The ignored H10 asymmetric-bandwidth experiment was made deterministic with
`taskset -c 0`. Both reliable phases failed closed as expected, but the
supposedly stable volatile phase lost its transport near the 4,500-offer mark.
The improved primary-cause oracle reported:

```text
slow_consumer_disconnects=2
websocket_ping_timeouts=1
expired_players_cleaned=0
proxy ServerToClient source read failed: Connection reset by peer
```

The server was still completing application socket writes at 32 KiB/s. An RFC
6455 Ping written behind that accepted traffic could nevertheless miss its
five-second Pong deadline before the client received it, producing a false
`4003 activity_timeout`.

The review loop then falsified two incomplete fixes:

1. A latest-only outbound timestamp could coalesce a timely write away when a
   late write followed it. Outbound evidence now latches the first completion
   on the active probe, parallel to inbound evidence.
2. Suppressing Ping frames while output progressed prevented read-only clients
   from returning automatic Pongs, so the independent inbound-activity reaper
   could expire them. Ping frames are still emitted; only their stale deadline
   is superseded.
3. Classifying every blocked protocol-Ping write as an idle timeout let a Ping
   queued behind proven application progress close `4003`; it now inherits the
   bounded `4002` delivery budget in that case.
4. The activity reaper acted on an expired-client snapshot after an awaited
   farewell lookup. Its final enqueue predicate now atomically revalidates and
   pins expiry before any terminal advisory escapes, preventing a refreshed
   client from receiving a stale terminal error or eviction. A pre-existing
   close owner also prevents the reaper from emitting a contradictory farewell,
   unregistering the connection, or incrementing its eviction metric.

The same audit found that `server.ping_timeout=0` was documented and validated
as disabling the activity reaper, but cleanup passed a zero duration to the
expiry scan and evicted every client immediately.

## Decision

Keep `4002 slow_consumer` as the delivery-contract outcome for both a recipient
that stops draining and sustained reliable oversubscription. Reliable traffic
cannot be dropped or buffered without a bound, and one queue timeout cannot
support a stable physical diagnosis. No new close code or negotiated wire
surface is added.

Use lane-correct queue geometry:

```text
queue_drain_seconds =
    (socket_bytes_ahead + queue_capacity * encoded_frame_bytes)
    / drain_bytes_per_second
available_queue_bytes =
    max(0, drain_bytes_per_second * max_sojourn_seconds - socket_bytes_ahead)
max_capacity_at_sojourn = floor(available_queue_bytes / encoded_frame_bytes)
```

Data uses `send_queue_capacity=1024`; accountability/lifecycle control uses
`control_queue_capacity=128`. The configured socket buffer is a kernel request,
so the result is a measured deployment input rather than a portable promise.

## Implementation

- Fold outbound generation and timestamped first evidence into the existing
  `PingProbeState`, keeping begin/evidence/deadline transitions atomic.
- Record actual completed application writes, including standalone coalesced
  reports and unsupported-format advisories that return `AccountedDrop`.
- Continue writing scheduled WebSocket Pings. If application output advanced
  across the probe boundary, clear only that Pong deadline; an eventual
  automatic Pong still refreshes the activity reaper.
- Preserve inclusive deadline ordering and reject late-only evidence.
- Make `server.ping_timeout=0` skip activity expiry as documented.
- Revalidate activity after asynchronous farewell lookup and atomically pin the
  activity-timeout close only while the connection is still expired.
- Add synchronized E2Es for both pre-deadline progress and a relay write after
  an observed Ping whose automatic Pong is held by a directional proxy.
- Retain bounded chaos-proxy pump termination diagnostics and upgrade H10
  failures to report the primary termination, exact accounting, delivery
  frontier, probe/reaper metrics, and proxy cause.
- Record the stable decision in ADR-0007 and update protocol, feature,
  configuration, scaling, changelog, and `PLAN.md` guidance.

## Verification

- Focused probe-state tests cover inclusive on-time evidence, late-only
  rejection, and a timely outbound write followed by a late write.
- All 19 server-Ping E2Es pass, including survival beyond the exact cancelled
  deadline and timeout after progress stops.
- Activity-reaper regressions cover the disabled zero value, refresh after a
  cleanup snapshot, stale-farewell suppression, and preserving a pre-existing
  close owner.
- `cargo check --locked --all-targets --all-features`, formatting, and clippy
  with warnings denied pass.
- `cargo test --locked --all-features` passes from the final staged state (631
  library tests plus every enabled integration and documentation target).
- Document consistency, workflow hygiene, LLM policy, Markdown, hook readiness,
  pre-commit/pre-push worktree preflights, policy test suites, and
  `cargo deny --all-features check` pass. Existing informational duplicate
  dependency, unmatched-license, nightly-age, and hook-runtime warnings remain
  non-failing and are unrelated to this change.
- H10 passed twice consecutively under `taskset -c 0` after the final design:
  - 9,484 offers = 4,003 delivered + 5,481 exactly reported drops;
  - 9,485 offers = 4,007 delivered + 5,478 exactly reported drops;
  - 40 exact ranges in each run, two expected reliable `4002` closures, and no
    volatile close.
- H14 passed under `taskset -c 0`: 5,000/5,000 sequences accounted, four
  reports, two advisories, 2,218 fallback bytes versus 389,618 compatible
  bytes (0.01x), and no slow-consumer eviction.
- The adversarial reviewer found and drove the timestamp-coalescing,
  report-only-write, Ping-write classification, reaper zero-value and
  snapshot/close-owner races, deterministic-E2E, metric-description, and
  socket-backlog corrections. The final complete-diff re-review returned PASS.

Exact-head hosted PR, reviewer, and CI evidence follow before the session is
complete.
