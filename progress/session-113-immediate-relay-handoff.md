# Session 113 — Allocation-free healthy relay handoff

## Scope and prioritization

Remote triage found nine open issues and no open, draft, or dependency pull
request to incorporate. Exact `main` at `1e8f5f8` initially had eight successful
and six still-running push workflows with no observed failure. Thirteen later
finished green; Advanced Safety's Miri job failed one scheduling regression
after 539 passing tests. The exact test passed 100 native repeats and initially
passed under the pinned Miri interpreter locally. RCA found that its intentional
capacity wait used a one-second real deadline, so full-suite interpreter
slowdown could evict the recipient before the test drained its queue. The test
now pauses Tokio time and explicitly asserts that no slow-consumer eviction
occurred; production deadline behavior is unchanged.
P56/#290 remains the highest gameplay risk, but its production fix and formal
proof are merged and closure still requires the unchanged scheduled cohort,
locally recorded at 4/20. P53/#274 likewise remains at 3/20 per operating
system. Manual runs cannot advance either pre-registered cohort, so the highest
impact actionable work was a new measured #207 relay optimization.

## Failure-first baseline and prediction

The production `relay_allocations` benchmark isolates message construction,
coordinator fan-out, and the classified queue while building caller payloads
outside its measured region. Exact unchanged `main` allocated twice per relay:
648 bytes in a two-player room and 1,104 bytes in 8-/16-player rooms. Its
prebuilt-message control still allocated once and 352 bytes in the two-player
case even though the classified queue itself allocated nothing. Larger controls
added the already-measured 472-byte shared frame cache.

That fixed operation was the boxed future required by the object-safe async
coordinator handoff. The failure-first ceiling therefore required one
production-ingress allocation at every room size; unchanged `main` would fail
at two. The registered prediction was that a synchronous uncontended try-start
would remove exactly one operation and 352 bytes without changing the message,
cache, queue, wire, or delivery ledgers.

## Implementation and measured result

`MessageCoordinator` now exposes a hidden optional try-start seam. Its default
returns unavailable, preserving the existing async compatibility path for
alternate implementations. The process-local coordinator uses non-waiting
routing read acquisition, builds the message only after both guards are held,
and starts every queue delivery synchronously. Healthy delivery completes
without an async state allocation. Routing contention leaves the one-shot
builder untouched for the ordinary async retry. Actual backpressure or a
synchronous slow-consumer result returns a boxed completion future only after
the routing guards have been released.

Five exact post-change repeats matched the allocation prediction for both JSON
and binary ingress:

| Room size | Before ops / bytes | After ops / bytes |
| --- | ---: | ---: |
| 2 players | 2 / 648 | 1 / 296 |
| 8 players | 2 / 1,104 | 1 / 752 |
| 16 players | 2 / 1,104 | 1 / 752 |

The existing allocation gate now enforces those exact operation and byte
ceilings. The classified queue remains at zero allocations. No wall-clock
latency claim is made: the instrumented allocator applies sequentially
consistent accounting to every allocation, and this experiment targets the
deterministic heap boundary.

## Regression boundary and compatibility

The builder/backpressure regression now includes the immediate path beside the
boxed, borrowed, and borrowed-owned async paths. It proves a full recipient
wait releases routing locks, excludes a concurrent late joiner from the frozen
snapshot, preserves one shared relay carrier through retry, and completes every
original delivery. Focused trait-object tests separately contend the room and
client routing locks, prove try-start returns unavailable without invoking the
builder, prove the first guard is released when the second acquisition fails,
and prove the async fallback consumes the builder exactly once.

The new trait method has a default and is hidden from generated documentation;
existing downstream implementers remain source compatible. Frozen v2/v3 wire
formats, relay stamps, projection cohorts, queue deadlines, slow-consumer
pruning, and metrics are unchanged. P53 and P56 retain their existing hosted
cohorts because neither workflow, workload, semantic oracle, nor production
queue timing changed.

## Changelog classification and verification

Removing one allocation and 352 allocated bytes from every healthy relayed
game-data handoff is user-visible performance work. The existing
`[Unreleased] / Changed` #207 entry is consolidated with that measured result
and explicitly preserves the contended/backpressured compatibility path.

Exact-tree verification passed formatting, all-target/all-feature Clippy with
warnings denied, the complete all-feature test matrix, `cargo deny`, CI
configuration and MSRV checks, workflow hygiene, LLM file/example policy, and
CI/devcontainer tooling parity. The allocation benchmark passed its tightened
one-operation ceilings on five repeats, and the mutation inventory plus shard
feasibility guards passed at 388 mutants.

The first adversarial review identified four gaps: the ignored progress record,
a missing `must_use` diagnostic on pending completion, incomplete lock/fallback
coverage, and imprecise changelog wording. The record was force-added; the enum
now warns if discarded; both lock-contention boundaries run through the real
trait-object fallback; and the changelog distinguishes routing fallback from
backpressure semantics. An independent follow-up review returned zero findings.
