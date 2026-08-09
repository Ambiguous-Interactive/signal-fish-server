# Session 111 — Single-allocation relay carrier

## Scope and prioritization

Remote triage found no open or draft pull request or dependency update to
incorporate. Exact `main` at `b443e64` completed all 49 check runs successfully.
P56/#290 remains the highest gameplay-risk issue, but its deterministic fix is
merged and closure still requires the unchanged 20-attempt hosted cohort. The
fourth eligible `h14-capacity-v1` attempt succeeded; P56 is 4/20. P53/#274
remains 3/20 per operating system. The next actionable gameplay-path work was
therefore a bounded, measured #207 relay allocation increment.

PR #320 had already merged, so this session also reconciles P68's stale
awaiting-merge bookkeeping before adding P69.

## Failure-first baseline and prediction

The production-seam `relay_allocations` benchmark ran 4,096 JSON and binary
relays per cell, repeated five times. Two-player ingress used two allocation
operations and 648 bytes per relay. Eight- and 16-player ingress used three
operations and 1,120 bytes because repeated wire projection added a separate
472-byte `Arc<RelayFrameCache>` beside the message `Arc`.

The failure-first ceiling required two operations at every room size and failed
on the first eight-player cell. The registered prediction was that co-owning a
newly built message and its cache would remove exactly one allocation operation
and one 16-byte `Arc` header from repeated-projection relays. The public boxed
and borrowed-`Arc<ServerMessage>` compatibility seams were expected to remain
unchanged.

## Implementation and measured result

`MessageCoordinator` now has an additive borrowed owned-message seam. Its
default wraps the message and delegates normally; the in-memory production
implementation builds one co-owned relay carrier only when projection work
repeats and every recipient uses a classified queue. Existing Arc-based callers
retain separate compatibility storage.

`DeliveryMessage` preserves the carrier through healthy enqueue, classified
full/retry, batching, frame materialization, and trace correlation. The socket
writer borrows the message and cache from the carrier, so it does not split,
clone, or drop that ownership before the write.

The five-repeat measurement matched the prediction for both JSON and binary:

| Room size | Before ops / bytes | After ops / bytes |
| --- | ---: | ---: |
| 2 players | 2 / 648 | 2 / 648 |
| 8 players | 3 / 1,120 | 2 / 1,104 |
| 16 players | 3 / 1,120 | 2 / 1,104 |

The isolated prebuilt-Arc handoff remains at its existing 1/2/2 operation
boundary. No latency claim is made; the instrumented allocator adds sequentially
consistent atomics and this experiment measures allocations, not wall clock.

## Evidence-cohort integrity

H14 has one direct MessagePack recipient and one JSON-fallback recipient. Those
are distinct projection cohorts, so repeated work is false and the co-owned
cache path does not run; P56 remains in the unchanged `h14-capacity-v1` cohort
at 4/20.

P53's first three `relay-clean-v1` hosted allocations precede this source
boundary and later records carry their exact commit. The workload schema and
semantic oracles do not change, so the preregistration does not require a
restart, but any final timing analysis must identify the implementation split
and must not attribute a mixed-source distribution to one implementation.

## Changelog classification and verification

The universal relay hot-path allocation reduction is user-visible performance
work, so the existing `[Unreleased] / Changed` #207 entry is consolidated with
the new production-ingress result. The entry remains compatibility-focused and
makes no latency claim.

The final local gate passed `cargo fmt`, all-target/all-feature Clippy with
warnings denied, and the complete locked all-feature test suite. Both checked
allocation benchmarks passed, as did cargo-deny, workflow/config/MSRV/tooling
policy, Markdown and LLM policy, hook readiness, and the mutation inventory
guard at 387 mutants. The changelog review verdict is PASS: the consolidated
entry is a non-breaking performance change under `Changed` and contains only
the measured allocation claims.

The first adversarial pass found that the repeated-cohort scan returned before
observing a later legacy recipient. That could route a co-owned carrier through
the compatibility conversion, clone its payload, discard its cache, and break
trace correlation. The scan now exhaustively returns both repetition and
all-classified facts; an exact classified/classified/legacy regression pins the
former failure. The second adversarial pass reported zero remaining issues and
also verified that `DeliveryMessage`, `OutboundData`, and `OutboundPayload`
retain their prior 16-, 72-, and 120-byte footprints.

PR #321 was opened from `ea488e3` and marked ready for review. Its exact source
head completed 20 hosted workflow runs successfully, including CI #1114,
Advanced Safety #884, the 40-shard mutation matrix, every interop lane, and the
new combined Relay Allocation Ceilings job. The only two skipped runs were the
expected duplicate Dependabot auto-merge triggers; no hosted run failed.

GitHub exposed no actionable review thread or human review. Copilot reported
that it could not review because the requesting account had exhausted its
quota; the independent two-round adversarial review therefore remains the
available code-review evidence and ended with zero findings.
