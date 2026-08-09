# Session 114 — Borrowed healthy relay handles

## Scope and prioritization

Remote triage found nine open issues and no open, draft, or dependency pull
request to incorporate. P56/#290 remains the highest gameplay risk, but its
deterministic production fix and formal proof are merged; closure still requires
the unchanged scheduled cohort, currently 4/20. P53/#274 likewise remains at
3/20 per operating system. Manual runs cannot advance either preregistered
cohort, so the highest-impact actionable work remained a bounded, measured #207
relay optimization.

The initial audit also reproduced issue #318's developer-hook latency: a clean
worktree pre-commit preflight took 2,249 ms, including 1,407 ms in changed-file
discovery. That maintenance issue remains below gameplay-path work and is not
mixed into this relay change.

## Failure-first baseline and prediction

P71 removed the healthy handoff's fixed async allocation, but the guarded
routing walk still cloned each `ClientDeliveryHandle` before calling the
delivery state machine. `OutboundSender::clone` locks the recipient queue,
increments its producer count, and clones the shared queue reference; the close
signal is cloned too. The delivery state machine then clones the handle again
only if a full queue needs owned backpressure state. Healthy delivery therefore
paid ownership work that no state retained.

The production `relay_allocations` benchmark now records classified sender
clone operations beside its existing allocation, byte, delivery-attempt,
enqueue, and receiver-drain ledgers. The new zero-clone ceiling failed on
unchanged fan-out immediately: the two-player JSON cell recorded 4,096 handle
clones for 4,096 relays. The registered prediction was zero healthy handle
clones at every room size, with ownership retained only for exceptional
backpressure or slow-consumer cleanup.

## Implementation and deterministic result

`start_deliveries` now accepts borrowed delivery handles. Both routing paths
walk the guarded client map by reference, and the general owned-recipient path
borrows its already-owned vector for the synchronous attempt. A full queue
continues to clone its handle inside `start_message_delivery_in_room`, where the
pending future genuinely needs ownership after routing guards are released. An
immediate slow-consumer result clones only the sender needed to verify and prune
the attempted connection.

Five exact post-change repeats retained every allocation and delivery ledger
while reducing handle clones to zero:

| Scope | Room sizes | Before clones / relay | After clones / relay |
| --- | --- | ---: | ---: |
| JSON production ingress | 2 / 8 / 16 | 1 / 7 / 15 | 0 / 0 / 0 |
| Binary production ingress | 2 / 8 / 16 | 1 / 7 / 15 | 0 / 0 / 0 |
| Prebuilt coordinator handoff | 2 / 8 / 16 | 1 / 7 / 15 | 0 / 0 / 0 |

Production ingress remains exactly one allocation and 296/752 bytes per relay
in 2-player versus 8-/16-player rooms. The classified queue remains at zero
allocations. The benchmark counter exists only under the existing
`allocation-tracking` feature and does not enlarge normal queue state.

## Runtime and compatibility

The same-session uninstrumented Criterion comparison found statistically
significant improvements in seven larger or mixed cells, four cells unchanged
or within noise, and one initial two-player v3 JSON outlier. An isolated rerun
of that outlier improved by 8.2% at the mean instead of reproducing the
regression. These are local falsification points, not portable latency claims;
the deterministic zero-clone ledger is the regression gate.

Frozen v2/v3 wire output, codec work, relay stamps, projection cohorts, queue
capacity, ordering, deadlines, slow-consumer pruning, and metrics are unchanged.
P53 and P56 retain their existing hosted cohorts because neither workflow,
workload, semantic oracle, projection behavior, nor queue-progress boundary
changed.

## Changelog classification and verification

Removing one synchronized queue-handle clone per recipient per relay is
user-visible performance work. The existing `[Unreleased] / Changed` #207 entry
is consolidated with that result and explicitly preserves exceptional
backpressure ownership and cleanup.

The first adversarial review found that the general bulk-delivery path borrowed
its already-owned snapshot but retained that vector through the backpressure
wait. One blocked recipient could therefore prolong every unrelated healthy
queue and close signal. The snapshot is now explicitly dropped after all
synchronous starts and before any wait. A paused-time regression routes one
healthy and one full legacy queue, removes the healthy route while the other
remains blocked, and proves the healthy delivery lands and its queue terminates
before the blocked broadcast finishes.

The full locked all-feature test suite, formatting, warnings-denied Clippy,
dependency/advisory checks, documentation and CI policy suites, workflow
hygiene, tooling parity, hook readiness, and both worktree hook preflights pass.
The exact final-tree allocation benchmark repeats every cell five times and
records zero delivery-handle clones without changing the allocation, byte,
delivery-attempt, enqueue, or drain ledgers. The pre-commit preflight remains
above its one-second target (2,496 ms here), consistent with open issue #318;
its checks passed and this PR does not change hook code. Remaining hosted
publication evidence is recorded on the exact commit and PR.
