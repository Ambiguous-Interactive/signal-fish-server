# Session 066 — Relay allocation baseline and measured optimization

**Branch:** `agent/session-066-relay-allocation-baseline`
**Base:** `4533ee7` (PR #221, session 065)

## Objective

Advance `PLAN.md` through the highest-gameplay-impact actionable issue, #211:
measure heap traffic in the relay coordinator fan-out, establish a non-vacuous
baseline, and land an optimization only when the same harness proves an
improvement.

## GitHub state at start

- No open or draft pull requests.
- No open Dependabot pull requests.
- The `main` push for PR #221 completed all 15 workflows successfully, including
  CI, advanced safety, WASM, Docker, documentation, and interop coverage.
- The live issue order was #211, #210, #220, #213, then the off-gameplay design
  work in #204. Issues #205 and #209 describe server work already completed in
  P16/session 062; #206 and #207 have concrete children #210 and #211.

## Measurement design

The new `relay_allocations` benchmark:

- installs `stats_alloc` only in its own dev-only binary, preserving the
  production allocator and the repository-wide `unsafe_code = "forbid"` policy;
- uses a current-thread Tokio runtime and no background tasks, so global
  allocator counters cannot be contaminated by concurrent work;
- warms Tokio and each classified queue's backing storage while intentionally
  rebuilding recipient and `join_all` storage inside each measured fan-out;
- exercises the production
  `broadcast_to_room_except_with_message` path with real protocol-v3,
  generation-scoped classified queues and a prebuilt shared payload, isolating
  coordinator routing, fan-out, and enqueue costs from the inbound handler's
  stamp/message construction and outer builder allocation;
- measures 4,096 coordinator fan-out calls at room sizes 2, 8, and 16, plus the
  warmed reliable classified queue in isolation;
- repeats every cell five times and requires the complete allocator statistics
  to match exactly; and
- proves non-vacuity from three independent ledgers: expected receiver drains,
  delivery attempts, and successful enqueues.

The instrumented allocator adds sequentially consistent atomics to allocation
operations. Its elapsed time is therefore not a production latency result.
Criterion timing remains on demand and non-gating; hosted-runner performance
thresholds would add machine noise rather than a reliable correctness gate.

## Baseline and prediction

Before changing production collection behavior:

| scope | room size | recipients | allocation ops / call | bytes / call |
| --- | ---: | ---: | ---: | ---: |
| fan-out | 2 | 1 | 6.0002 | 1,160.02 |
| fan-out | 8 | 7 | 7.0002 | 5,160.02 |
| fan-out | 16 | 15 | 8.0002 | 10,600.02 |
| classified queue | 2 | 1 | 0 | 0 |

The exact 1/2/3 reallocation progression identified recipient snapshot growth:
the filtered iterator exposed no useful lower bound to `Vec::collect`, even
though room membership already supplied an exact upper bound.

Prediction: pre-sizing the recipient vector from membership will remove every
room-size-dependent reallocation without changing routing or delivery
behavior.

## Result

The same five-repeat workload after pre-sizing:

| scope | room size | recipients | allocation ops / call | bytes / call | operation change |
| --- | ---: | ---: | ---: | ---: | ---: |
| fan-out | 2 | 1 | 6.0002 | 1,040.02 | 0% |
| fan-out | 8 | 7 | 6.0002 | 5,120.02 | -14.3% |
| fan-out | 16 | 15 | 6.0002 | 10,560.02 | -25.0% |
| classified queue | 2 | 1 | 0 | 0 | unchanged |

All room-size-dependent growth disappeared. One invariant reallocation remains
inside the concurrent fan-out machinery; the classified queue itself requires
no steady-state heap operation.

## Falsified optimization

The repository already uses `SmallVec` with an eight-element broadcast
capacity, so the next hypothesis was that stack-storing the recipient snapshot
would remove its remaining allocation for typical rooms.

It did reduce allocation operations to 5, 5, and 6, but increased bytes per
call to 1,608.02, 5,448.02, and 11,168.02. In this async-trait call chain the
inline recipient array becomes part of a boxed future; the storage moved into a
larger heap allocation rather than becoming free. The candidate is rejected,
and the smaller pre-sized `Vec` is retained.

## Serialization boundary audit

Issue #211 also named pre-serialization as its highest expected-value candidate.
The audit found that `BroadcastMessage`, `PreSerializedMessage`, and
`SerializationBuffer` have no production consumer outside `src/broadcast.rs`
tests; socket writers still perform per-recipient serialization. This benchmark
cannot measure that cost because it deliberately stops after classified-queue
enqueue.

Issue #222 now tracks a non-vacuous measurement through real frame serialization
for homogeneous and mixed protocol/encoding cohorts, followed by compatible
recipient reuse only if the same harness proves a gain. Keeping that work
separate avoids conflating coordinator fan-out allocation results with complete
inbound-to-wire cost.

## Implementation

- Add the opt-in `allocation-tracking` feature and dev-only, zero-transitive-
  dependency `stats_alloc` crate.
- Expose only the six queue measurement types/functions and one explicit
  generation-scoped handle constructor behind a doc-hidden feature gate.
- Pre-size both ordinary room snapshots and the stamp-coupled game-data
  recipient snapshot through one shared helper.
- Document the benchmark command, interpretation boundary, measured result,
  and rejected candidate.

`stats_alloc` is small, MIT-licensed, has no transitive dependencies or known
advisories, and compiles at the repository's Rust 1.89 MSRV. Its 2022 release
and inactive upstream are a maintenance warning; accepting it only as a
dev-only benchmark dependency keeps that risk outside production artifacts,
while `cargo deny` and the locked dependency audit remain enforcement points.

## Verification

- Five exact repeats of every pre-change, intermediate, rejected-candidate, and
  final allocation cell.
- All 17 focused message-coordinator tests pass after the production change.
- Final benchmark: 6.0002 allocation operations per coordinator fan-out at
  room sizes 2, 8, and 16; 1,040.02, 5,120.02, and 10,560.02 bytes per call;
  zero allocation operations and bytes in the warmed classified queue.
- `cargo check --locked --all-targets --all-features`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `cargo test --locked --all-features`
- Rust 1.89 MSRV benchmark compile
- Exact 344-mutant inventory and mutation-shard feasibility guards
- Full advisory and `cargo deny --all-features check` dependency policy
- Doc consistency, direct ignored-file markdown lint, workflow hygiene, and
  `.llm` size/example policy
- Hook readiness and worktree pre-commit/pre-push policy
- Fresh adversarial review: PASS with zero critical, warning, or suggestion
  findings after four review/fix cycles
- Exact base `main` commit: all 15 workflows completed successfully

Issue #209 was closed with its PR #208 teardown fix and current successful ASan
evidence. Hosted exact-head CI and reviewer evidence for this session is
recorded on the PR because it necessarily occurs after this commit exists.
