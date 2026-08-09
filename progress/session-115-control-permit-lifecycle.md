# Session 115 — Reserved control-permit lifecycle

## Correctness-first scope

GitHub triage found nine open issues and no open, draft, or dependency pull
requests. Issue #290 remains the highest gameplay risk, but its deterministic
fix and capacity-deadline proof are merged and its remaining acceptance
criterion is the unchanged preregistered scheduled cohort, currently 4/20;
manual runs cannot advance it. P53/#274 is likewise cohort-bound at 3/20 per
operating system. Under the required correctness → usability → performance
ordering, this session therefore selected issue #220's exact unmodeled
boundary: lifecycle conservation after a classified control permit has claimed
capacity. The previously considered #207 cache optimization was explicitly
deferred.

## Failure-first finding

P57's `CapacityDeadlineArbitration.tla` deliberately ends at atomic admission.
Production then carries distinct lane-reservation, permit-producer, accepting,
generation, room, ordinary-control, transition-barrier, cancellation, and
destructor states. The first model and tests covered only one permit. The first
adversarial review rejected that as too weak because a room transaction can
hold two phase permits per recipient, and it also found the receiver-wake test
was scheduler-vacuous.

Expanding that review into an already-pending receiver regression exposed a
real production divergence. With one old-generation permit held as the final
producer, another producer could enqueue and drain a generation transition.
The receiver then parked because the permit still kept `producers_open` true.
When the old permit resolved as stale, `send_control_inner` released
`reserved_control` and `permit_count` and notified capacity waiters, but did not
notify `item_available`. The qualified unchanged-code test failed
deterministically at its one-second wake bound instead of observing EOF.

## Implementation and executable closure

Every permit-consuming cancellation arm now snapshots whether the permit is
the final producer while holding the queue lock and wakes item waiters after
releasing that lock. Successful commit, receiver-close failure, invalid payload
Drop, and ordinary Drop preserve their existing notification behavior.

The Rust coverage now pins the production composition without sleeps:

- an already-pending receiver wakes when the final permit is dropped;
- an already-pending receiver wakes and receives a successful final commit;
- a drained transition followed by final stale cancellation wakes the receiver
  to EOF (the deterministic RED/GREEN regression);
- two simultaneously held permits conserve exact reservation and producer
  counts across mixed Drop and commit;
- the `DeliverySender` / `DeliveryPermit` wrapper commits a transition and
  cancels an old-scope ordinary control;
- receiver close, accountability failure, invalid control payload, and stale
  cancellation all release exact capacity and producer state.

## Formal proof and non-vacuity

`CapacityPermitLifecycle.tla` uses two permit identities and a two-slot control
lane, matching the production two-phase transaction bound. It explores two
held permits, ordinary and next-generation transition commits, mixed Drop /
commit, receiver close, accountability failure from another live producer,
last-sender drop, parked receiver notification/resume, queue drain, and stale
scope cancellation. The healthy exhaustive configuration reaches 6,902
distinct states at graph depth 11 with TLC 2.19.

The checked invariants prove exact `queued + reserved` capacity, reservation
and producer-count equality with the held-permit set, per-permit lifecycle
conservation, no EOF overtaking an accepting held permit, no stale-scope
commit, and notification on every modeled progress edge visible to a parked
receiver. The wake invariant proves that the production action issued a
notification; it deliberately makes no wall-clock or scheduler-fairness claim.

Six independent expected-failure configurations make the proof non-vacuous.
They omit the permit producer capability, Drop release, failed-send release,
stale-cancel release, commit-time scope validation, or permit wake. Each must
make TLC report its exact named invariant; a clean run, parser error, or
unrelated invariant failure remains red. The TLA runner now resolves the
longest checked-in module prefix so these independently named failure scenarios
all map to the same module without weakening the exact diagnostic oracle.
Its hermetic regression covers multiword scenarios, underscore-containing
module names, alternate `--tla-dir` bundles, and missing-module rejection
without requiring Java or a downloaded checker.

The second review found one remaining fidelity abstraction: the draft assigned
ordinary versus transition kind at reservation, while production records only
generation and room and discovers message kind at send. The final model instead
chooses commit kind nondeterministically for every held permit, exercising both
cross-kind error directions. It reaches 6,902 distinct states at depth 11. The
wake mutant is deliberately class-level non-vacuity; the healthy exhaustive
invariant covers every modeled progress action, while distinct deterministic
Rust regressions pin the actual ordinary-stale defect, stale-transition branch,
and defensive unscoped-transition branch.

## Review, compatibility, and publication evidence

The first adversarial pass found the one-permit proof boundary, terminal-path
overclaim, missing transition/wrapper composition, unguarded accountability
abstraction, scheduler-vacuous wake test, absent wake invariant, and incomplete
documentation. Those findings produced the two-permit model, separate terminal
release seeds, wrapper and mixed-permit tests, guarded model action, pinned and
bounded receiver futures, documentation, and the production stale-cancel wake
fix. A second adversarial pass reviewed the revised shared tree before
publication and found the remaining cross-kind fidelity and stale-transition
wake gaps. After those were closed, the final pass reported zero findings.

The runtime fix changes no wire shape, queue capacity, ordering, deadline,
hosted workload, or timing threshold. It is user-visible correctness work, so
the `[Unreleased] / Fixed` changelog records that a final stale permit can no
longer strand the receiver. P53 and P56 retain their existing hosted cohorts.
Compatible dependency updates exist locally but current RustSec and Cargo
policy audits are clean; mixing maintenance refreshes into this correctness
change would weaken its review boundary.

The final tree passes formatting, warnings-denied all-target/all-feature
Clippy, the locked all-feature Rust suite, the complete TLA+ and Z3 suites,
dependency and advisory policy, documentation and CI configuration policy,
workflow hygiene, tooling parity, hook readiness, and both worktree hook
preflights. The profiled pre-commit preflight passed in 2,912 ms; its 1,278 ms
panic scan and 719 ms changed-file discovery confirm the already-tracked #318
hook-speed regression, and this phase changes no hook code or policy. Exact
commit, hosted checks, and reviewer evidence are attached to the pull request.
