# Session 112 — Pre-sized JSON relay projection

## Scope and prioritization

Remote triage found nine open issues and no open or draft pull request or
dependency update to incorporate. Exact `main` at `356c00a` completed all 47
check runs successfully. P56/#290 remains the highest gameplay risk, but its
deterministic production fix and formal proof are merged and closure still
requires the unchanged scheduled cohort, currently 4/20. P53/#274 likewise
remains at 3/20 per operating system. The next actionable gameplay-path work
was therefore another bounded, measured #207 relay optimization.

This session also reconciles P69's stale awaiting-merge status after PR #321
landed and adds issue #318 to PLAN's lower-priority open follow-ups.

## Failure-first baseline and prediction

The exact `relay_serialization_allocations` benchmark runs 1,024 relays per
2-/8-/16-player cell and pins wire digests plus codec-work counts. JSON text
projection used three reallocations per relay: 7.001 allocation operations and
2,538 bytes per relay in the 2-player cell, and 8.001 operations and 3,010
bytes in the larger repeated-projection cells. The corresponding binary paths
used zero reallocations.

The failure-first ceiling required zero JSON reallocations and four/five total
operations; unchanged `main` failed the first JSON cell at 7,169 operations
against a 4,097-operation ceiling. The registered prediction was that sizing
the output from the already-parsed JSON value plus fixed envelope headroom
would remove all three growth reallocations without changing wire bytes.

## Implementation and measured result

JSON game-data projection now derives a non-allocating lower-bound size from
the existing `serde_json::Value`, adds bounded relay-envelope headroom, and
serializes directly into that buffer. Unusually escape-heavy strings may still
grow normally; the estimate is an optimization hint rather than a new frame
limit or protocol invariant. Control serialization and the existing fallible
binary-to-JSON fallback path are unchanged.

The five-repeat deterministic result matched the allocation prediction and
retained every checked-in wire digest and codec-work ledger:

| Room size | Before ops / bytes | After ops / bytes |
| --- | ---: | ---: |
| 2 players | 7 / 2,538 | 4 / 1,627 |
| 8 players | 8 / 3,010 | 5 / 2,099 |
| 16 players | 8 / 3,010 | 5 / 2,099 |

A data-driven unit regression compares pre-sized output with the reference
serializer for null, empty object/array, nested gameplay state, Unicode, and
escape-heavy strings. Adversarial review reproduced two retained-capacity
counterexamples in the first estimator: a 60,155-byte numeric-dense frame kept
750,257 bytes, while escape-heavy growth kept about 82 KiB for a near-limit
frame. Small inputs use constant-time integer digit counts and a one-byte float
lower bound, but sizing stops after 256 work units so structurally dense or
float-uncertain input uses the reference serializer instead of paying for a
second traversal. Every value node costs one unit and a float costs 23 more,
matching the maximum representation length omitted by its lower bound; the
representative workload's seven floats remain eligible while an 11-float array
crosses the budget.
Excessive slack is compacted only after the pre-sized buffer actually grows.
Focused regressions pin exact wire output, keep the numeric case within the
default 64 KiB frame allocation, and keep escape-growth capacity within 256
bytes of its wire length.

## Runtime guard and changelog classification

The first implementation reused the fallback path's fallible writer and failed
the runtime guard: despite the allocation reduction, the two-player cell was
4.2% slower. That implementation was rejected. Direct `Vec` serialization
restores the old path's allocator-exhaustion semantics and avoids a capacity
check on every serializer write.

Adversarial timing also rejected exact number formatting: optimized synthetic
zeros and floats regressed 110.5% and 89.2%. The final bounded estimator reduced
the zero-dense comparison to 1.8%. A later reviewer found a 250-float shape just
inside the original node cutoff regressed 19–24%; charging float uncertainty
moves that shape to the one-pass path. Seven optimized 100,000-encode A/B pairs
then measured 447.6 ms reference versus 451.4 ms bounded mean (+0.9%, within
local run noise). Against the saved same-session representative baseline,
Criterion's final isolated mean point estimates improved 14.8%, 33.6%, and
10.8% in
2-/8-/16-player rooms, with every confidence interval excluding zero. These are
local falsification points, not portable performance claims; the deterministic
representative allocation ceilings remain the CI regression gate.

The change is user-visible relay performance work, so the existing
`[Unreleased] / Changed` #207 entry is consolidated with the new measured
allocation result. It remains backward compatible and makes no hosted latency
claim.

## Verification and review

The exact final source passed locked full tests, all-target/all-feature Clippy
with warnings denied, formatting, both relay allocation suites, cargo-deny,
CI/document/workflow/Markdown/link policy checks, and hook preflight. The
deterministic serialization benchmark reproduced 4.001/5.001 operations, zero
growth reallocations, 1,626.98/2,098.98 bytes per relay, and every existing wire
digest and codec-work ledger. Three adversarial rounds found and drove fixes
for numeric retained-capacity amplification, escape-growth slack, double number
formatting, and the float-cutoff runtime cliff; the final independent reread
reported zero findings.

Publication and exact-head hosted CI remain pending.
