# Session 099 — Capacity-deadline arbitration proof (P57)

## Scope and triage

Remote state was clean before this session: exact `main` head `8237916` had 20
successful workflow runs and 57 successful checks; no open, draft, or
dependency pull request needed incorporation. P56's first eligible scheduled
observation passed the unchanged strong H14 workload, but its pre-registered
cohort is only 1 of 20, so issue #290 remains open.

The next bounded gameplay-integrity increment advances #220 against the exact
P56 state-space boundary without modifying production behavior or the hosted
workload. It changes no API, configuration, wire contract, runtime behavior,
performance, or security posture. The repository nevertheless classifies
`formal/**` as changelog-gated, so the Unreleased notes record the strengthened
verification guarantee.

## Model and correspondence

`CapacityDeadlineArbitration.tla` begins with a waiter observing a full
classified lane. TLC explores:

- writer capacity release before, exactly at, and after the exclusive deadline;
- a producer left unscheduled until after both capacity and timer are ready;
- continuous availability across more than one drain;
- release, competing refill, and later release, which must discard stale
  progress evidence; and
- both lock acquisition orders between a late waiter and a competing refill.

The model maps writer drains and competing enqueues/reservations to
`QueueState::refresh_capacity_availability`, witness validation to
`CapacityReleaseWitness::permits_locked`, and atomic late admission to the
`try_enqueue_*_released_before` / `try_reserve_control_scoped_released_before`
families. Its admitted state is a capacity claim: it covers direct enqueue and
control reservation, but deliberately stops before a returned permit is sent,
canceled, or dropped.

## Non-vacuity and evidence

The corrected `_Small` configuration checks 76 distinct states at graph depth
9 with no invariant violation. It proves:

- no false `SlowConsumer` when capacity became and remained available strictly
  before expiry;
- no late revival from capacity returned at or after expiry;
- refill invalidation of a prior release;
- strict lane capacity; and
- conservation of the waiter's waiting, capacity-admitted, or loudly abandoned
  resolution (not post-reservation permit lifecycle).

The `_ExpectedFailure` configuration reintroduces the old timer-first behavior.
TLC finds a retained release strictly before the deadline, followed by a
deadline-or-later poll that evicts the waiter despite continuous capacity. The
runner pins the invariant diagnostic rather than one scheduler-equivalent
trace's exact tick choices, and passes this configuration only for the exact
`Invariant NoFalseSlowConsumer is violated.` diagnostic; parser errors, clean
runs, and unrelated failures remain red.

P57 is a bounded #220 proof increment. P56 remains validating until 20 eligible
scheduled first-attempt H14 observations pass; this model does not substitute
for that hosted distribution.

## Adversarial review

The first independent review found four integration and claim-scope defects:
top-level control waits could bypass the formal workflow path filter, the
expected-failure prose over-specified one parallel scheduler trace, an action
name was stale, and reservation admission was mislabeled as final enqueue
conservation. The fixes add a parsed exact-filter regression, use an
invariant-level negative oracle, consistently model `ProducerAdmit`, and stop
the conservation claim at capacity admission. A second pass corrected one
ignored PLAN claim and the data timer branch's source mapping. The final fresh
pass re-read the complete diff and reported zero findings.

## Local validation

- Full TLA+ suite: green, including the 19,200,001-state
  `EndToEndGapAccountability_Sim` run; the new positive model checks 76 distinct
  states at depth 9 and the seeded configuration reports only its exact expected
  invariant failure.
- Z3 protocol invariant suite: all proof sets pass.
- `cargo fmt --check`, all-target/all-feature Clippy with warnings denied, and
  `cargo test --locked --all-features`: pass.
- `cargo deny --all-features check`: advisories, bans, licenses, and sources
  pass; only the repository's accepted duplicate/unmatched-license warnings
  remain.
- CI configuration, workflow hygiene, document consistency, Markdown and link
  text, LLM file-size/example policy, hook readiness, and worktree pre-commit /
  pre-push checks: pass.
- Hook/local-policy suites: 309 CI configuration tests pass (1 intentionally
  ignored), plus all 10 document-policy and 5 document-script tests.
