# Session 109 — Compact shared relay frame cache

## Scope and prioritization

Remote triage found no open or draft pull requests and no dependency update to
incorporate. Main's applicable checks had no observed failure. P56/#290 remains
the highest gameplay-risk item, but its production fix and deterministic RED
regression are already merged; closure is correctly gated on the unchanged
20-attempt hosted cohort, currently 3/20. P53/#274 is likewise fixed at 3/20
hosted timing allocations per OS. The highest-impact immediately actionable
work was therefore a bounded, measured #207 relay optimization increment.

## Failure-first baseline and prediction

The unchanged `relay_serialization_allocations` production-seam benchmark ran
1,024 relays per cell across 15 JSON, direct-binary, mixed-format, and frozen-v2
cells, repeating each cell five times. It showed that each 8-/16-player
v3/mixed cell with repeated projection work added exactly one 680-byte
`RelayFrameCache` allocation:

| Scenario | 2 players | 8 players | 16 players |
| --- | ---: | ---: | ---: |
| v3 JSON | 2,538 B | 3,218 B | 3,218 B |
| v3 MessagePack | 1,533 B | 2,213 B | 2,213 B |
| mixed MessagePack source | 3,524 B | 7,802 B | 7,802 B |

The cache reserved six full `OnceLock<Result<MaterializedFrame, _>>` slots even
though a logical relay is exclusively JSON or binary. The pre-registered
prediction was that sharing the text/binary v2/v3 primary storage would remove
two slots and reduce every shared-cache relay by the same fixed byte count,
without changing any allocation-operation, reallocation, codec-work, wire, or
delivery ledger.

## Implementation and measured result

`RelayFrameCache` is now constructed for the logical message kind. Text-v2 and
binary-direct-v2 share one primary slot; their v3 counterparts share another;
binary fallbacks retain separate v2/v3 slots and the shared decoded-value cell.
The materializer verifies the cache kind and safely uses the uncached path for
a mismatch, so mutually exclusive slot storage cannot return a frame for a
different message kind. Non-game-data messages do not receive a relay cache.

Five exact post-change repeats matched the prediction. Allocation operations,
reallocations, codec counts, delivery counts, queue drainage, and all 15 wire
digests were unchanged. Every shared relay allocated 208 fewer bytes:

| Scenario | 2 players | 8 players | 16 players | Shared-relay change |
| --- | ---: | ---: | ---: | ---: |
| v3 JSON | 2,538 B | 3,010 B | 3,010 B | -208 B (-6.5% total) |
| v3 MessagePack | 1,533 B | 2,005 B | 2,005 B | -208 B (-9.4% total) |
| mixed MessagePack source | 3,524 B | 7,594 B | 7,594 B | -208 B (-2.7% total) |

The cache itself falls from 680 to 472 bytes, a 30.6% reduction. Frozen-v2 raw
passthrough and two-player cells remain unchanged because they correctly skip
the shared cache. The checked-in operation ceilings now equal the deterministic
measurement, and the byte ceilings retain the existing roughly 49-byte
per-relay margin while removing stale room-size growth already eliminated by
P38.

## Changelog classification and verification boundary

The deterministic reduction affects the universal WebSocket relay hot path and
is user-visible performance work, so `[Unreleased] / Changed` records it. No
latency claim is made: the instrumented allocator adds sequentially consistent
atomics and this experiment targeted retained heap bytes, not wall-clock time.

P53 and P56 retain their pre-registered 20-attempt hosted thresholds. This work
does not reset, weaken, or infer a result from either cohort.

## Adversarial review loop

The first cache review found that kind filtering covered frame materialization
but not binary fallback preflight. A wrongly supplied text cache could therefore
retain decoded data from one binary message, reuse it for another, and strand
the MessagePack decode attribution even though materialization later bypassed
the frame slots. The fix applies the immutable binary-kind guard at preflight
and adds a two-payload regression proving uncached-equivalent wire frames and
codec work after repeated wrong-kind use.

The same review found that strengthening the race to cover simultaneous v2/v3
fallback slots had removed same-slot initialization coverage. The final suite
now proves both cases: cross-slot initialization emits two distinct frames with
one shared decode, while a barrier-synchronized same-slot race emits one frame
encode, one decode attribution, and identical results for both writers.

The evidence review narrowed overbroad “multi-recipient” wording to the actual
repeated-projection predicate, removed a duplicated completed P67 narrative from
the local PLAN, and found that P38-era operation ceilings still permitted one
extra operation per relay. All operation ceilings now equal the deterministic
measurement plus the separately documented sample-scoped operation. Two final
adversarial passes and an independent finding evaluator reported zero remaining
issues.

## Validation

The final tree passed formatting, Clippy with warnings denied, the complete
all-features test suite, `cargo deny`, CI/MSRV/document/workflow policy checks,
LLM file and example checks, tooling parity, markdown validation, hook readiness,
both worktree hook preflights, the focused documentation policy suites, and all
313 CI configuration tests (312 passed, one intentionally ignored). The
allocation benchmark also passed its tightened byte, operation, reallocation,
codec, delivery, and wire-digest bounds. The pre-commit worktree preflight
remained functionally green but reported a non-gating speed warning under the
concurrent validation load; its existing 1-second target was not changed.

At the final pre-branch snapshot, all 29 applicable check runs on remote `main`
were successful. No draft, dependency, or other open pull request required
incorporation before this work.
