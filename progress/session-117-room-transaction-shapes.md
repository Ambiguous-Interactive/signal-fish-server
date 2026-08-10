# Session 117 — Sparse room-transaction and retry integrity

## Correctness-first scope

GitHub triage found nine open issues and no open or draft pull requests. Issue
#290 remains the highest direct gameplay risk, but its deterministic fix is
merged and the remaining P56 acceptance is the unchanged scheduled hosted
cohort at 4/20; manual runs cannot advance it. P53 likewise remains at 3/20
eligible scheduled allocations per operating system. No dependency pull
request was available to incorporate.

The next actionable in-repository boundary was issue #220. P74 proved the
symmetric two-recipient/two-frame transaction, but an independent audit and
two adversarial reviews found that this shape was not a reduction of every
batch accepted by `commit_room_messages_if_members_with_hook`. Production also
accepts zero-frame members for identity validation, one-phase and
phase-one-only batches, solo transactions, and canceled pre-hook reservations
that release a partial attempt and retry without consuming either callback.

## Failure-first evidence

The original healthy model reached 1,091 distinct states at depth 16 and all
seven seeded defects failed correctly. The focused 30-test message-coordinator
suite was green. This was a proof and deterministic-coverage gap, not evidence
of a runtime defect.

The missing retry was then reproduced deterministically through the production
classified queue. One next-generation transition permit and one old-generation
actor permit fill the actor's two control slots while its second transaction
reservation waits; the incumbent concurrently holds its sibling permit.
Committing the transition makes that waiter stale. The first attempt records
exactly three cancellations — the stale attempt, its held actor permit, and the
incumbent permit — before refreshing the delivery handle and committing both
phases exactly once on generation two.

## Expanded proof and regressions

`RoomMessageTransaction.tla` now selects each member's frame plan independently
from every subset of the two phases under the same one/two-recipient maximum.
The exhaustive two-recipient run covers all 15 labeled nonempty plans (nine up
to symmetry), including the empty/phase-one shape used by the Rust regression,
and reaches 574,597 generated and 228,040 distinct states at depth 25. A
972-state, depth-15 singleton configuration separately exhausts all three
nonempty one-member plans.

The retry is now causally tied to explicit connection-generation changes and
the complete set of canceled recipient batches. It records every canceled
waiter and held sibling permit, refreshes the attempt snapshot, and
independently retains both one-shot
callbacks. A separate weakly fair 35-state witness forces that complete chain
through hook acceptance and commit. `ReservationUnavailable` is limited to a
member with physical frames and models conditional sender removal followed by
either replacement recollection or a missing-route result. Five new seeded
bugs independently omit retry release, consume either callback, reuse the stale
snapshot, or ignore a zero-frame member's changed sender; each produces its
exact named invariant alongside the seven original non-vacuity gates.
The unavailable-sender abstraction also preserves production's joined-result
precedence: any stale physical waiter must take the canceled-attempt retry
before a sibling closed/slow result can trigger handle recollection. Mixed
result vectors preserve the unavailable result, release any permits already
held by that sibling batch, and include them in exact retry telemetry.

Two no-sleep Rust regressions pin the corresponding production seams. The
classified stale-reservation test asserts exactly six attempts across the full
stale and refreshed transactions, exactly three cancellations, one hook and
callback, phase order, and recovery of both physical capacity slots. A
data-driven sparse test proves the healthy empty-member/phase-one transaction
and fail-closed route changes for both a different legacy channel and a
classified same-queue/different-generation replacement.

## Changelog classification

This is assurance and test infrastructure for an existing runtime contract,
not a user-visible protocol or configuration change. The existing Unreleased
issue-#220 entry is expanded to describe the stronger proof; no new runtime
release note or breaking-change notice is required.

## Review and verification

The initial task audit recommended proving the per-room event sequencer next.
Adversarial review correctly superseded that proposal with this smaller live
P74 closure; a sequencer model remains the next issue-#220 increment and must
model mutation-gate handoff rather than inventing a multi-job FIFO queue.

The final independent formal review and Rust-test review both approved with
zero findings. All 15 room-transaction configurations passed: the general run
reached 574,597 generated / 228,040 distinct states at depth 25, the singleton
run reached 972 distinct states at depth 15, the fair retry witness reached 35
distinct states at depth 18, and all 12 mutants failed only their named
invariants. Review instrumentation also confirmed that the mixed
canceled/unavailable action was non-vacuous with 1,080 transitions. Both
focused Rust regressions passed 500 consecutive repetitions each.

The complete repository gate passed on commit `fc47146`: formatting, clippy
with warnings denied, all-feature Rust tests, every TLA+ model and expected
failure, every Z3 proof, cargo-deny, CI/MSRV/workflow/LLM policy, documentation
consistency, and hook-readiness/pre-commit/pre-push checks. The change was
published as ready pull request #327. Hosted results and automated reviewer
feedback are monitored on that single session PR until green.

## Publication closure

PR #327 subsequently completed its hosted checks and merged to `main` as
`e7234d6`. The monitored-PR wording above records the pre-merge session
snapshot; this closure records the final publication evidence.
