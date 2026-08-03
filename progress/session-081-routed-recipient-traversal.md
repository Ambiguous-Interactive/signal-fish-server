# Session 081 — Allocation-free routed-recipient traversal

## Scope

Advance issue #207 with P38: remove the healthy game-data handoff's temporary
routed-recipient vector while preserving the stamp/snapshot ordering boundary,
backpressure concurrency, and exact delivery set.

## Failure-first baseline and prediction

The unchanged P37 harness relayed 4,096 messages at 2, 8, and 16 players across
five exact allocator samples. The isolated borrowed coordinator handoff used
2/3/3 operations and 400/1,320/1,640 bytes per relay; JSON and binary production
ingress used 3/4/4 operations and 696/1,616/1,936 bytes.

The only room-size-dependent allocation left at this seam was the recipient
snapshot `Vec`: 48, 288, and 608 bytes. The pre-registered prediction was that
walking the already-guarded routing maps directly through synchronous delivery
start would remove exactly one allocation and those bytes at every room size.
Only exceptional full queues should retain owned backpressure state, after the
routing guards are released.

## Implementation and measured result

Projection-cohort discovery and healthy delivery now make two allocation-free
passes over the guarded room membership. The first decides whether compatible
recipients share a relay frame cache; the second clones each delivery handle
directly into queue delivery. The routing guards are dropped before any
backpressured delivery future is awaited. Boxed downstream-compatible builders
and the borrowed production builder use the same start/finish state machine.

Five exact repeats confirm the prediction for both JSON and binary ingress:

| Room size | Isolated before | Isolated after | Production before | Production after |
| --- | --- | --- | --- | --- |
| 2 | 2 ops / 400 B | 1 op / 352 B | 3 ops / 696 B | 2 ops / 648 B |
| 8 | 3 ops / 1,320 B | 2 ops / 1,032 B | 4 ops / 1,616 B | 3 ops / 1,328 B |
| 16 | 3 ops / 1,640 B | 2 ops / 1,032 B | 4 ops / 1,936 B | 3 ops / 1,328 B |

The checked-in ceilings retain a 16-byte isolated-handoff margin while the
production ceilings equal the deterministic observed values. Delivery
attempts, enqueues, receiver drains, message variants, and five-repeat allocator
identity remain non-vacuous gates. No latency improvement is claimed.

## Concurrency proof and validation

A paused-time, data-driven regression fills the original recipient's queue for
both boxed and borrowed builders. A late same-room registration must complete
while the relay awaits capacity, proving routing guards are not held across the
wait, while the late joiner must not receive the already-started relay. The
existing missing-stamp, terminal-unroute, reconnect-baseline, slow-consumer,
wire-output, and accounting suites remain required gates.

## Adversarial audit follow-ups

The session-wide audit confirmed two unrelated P0 contract gaps and one P1
interop gap. They remain outside this focused performance diff and have
complete follow-up scopes:

- issue #249: atomically enforce persisted application room ownership plus
  configured per-app room/player limits across seated join, spectator join,
  reconnect, creation, cleanup, and legacy unowned rooms;
- issue #250: decide and enforce a truthful client-authentication trust boundary
  because production accepts the public app ID while docs describe the unused
  configured app secret as active authentication; and
- issue #251: select `Host + Direct` only when the authoritative plan is
  executable and the client has usable endpoint information.

The same audit disproved a proposed reconnect rate-limit defect: token reconnect
restores the original logical player ID, so its limiter window survives. Fresh
anonymous identity churn is an explicit threat-model/design question rather
than a demonstrated reconnect regression, and no misleading bug was filed.

## Local completion evidence

The exact allocation benchmark passed every JSON and binary cell across five
repeats. `cargo fmt --check`, all-target/all-feature Clippy with warnings denied,
the complete all-feature test suite, `cargo deny --all-features check`, the CI,
MSRV, documentation, workflow, and LLM policy scripts, the explicit policy-test
targets, hook readiness, and both worktree hook preflights all passed. The final
adversarial diff review reported zero major or minor findings after independently
checking delivery semantics, guarded traversal, backpressure, snapshot isolation,
shared-cache activation, benchmark attribution, and documentation consistency.

Hosted CI and exact-head publication evidence remain pending; P38 is not marked
complete until that final review loop is green.
