# Session 116 — Exact room-publication transaction

## Correctness-first scope

GitHub triage found nine open issues and no open, draft, review, or dependency
pull requests. Issue #290 remains the highest direct gameplay risk, but its
deterministic fix is merged and the remaining P56 acceptance is the unchanged
scheduled hosted cohort, currently 4/20; manual runs cannot advance it. P53's
issue #274 evidence is likewise fixed at 3/20 eligible scheduled allocations per
operating system. The next actionable in-repository correctness boundary was
therefore issue #220: exact composition of classified permits inside the shared
room-publication transaction.

At the audit snapshot, local `main`, `origin/main`, and the connected GitHub
repository all resolved to `dad800d`. Fourteen of its 17 push workflow runs had
completed successfully and three long-running jobs were still in progress;
none had failed. No red workflow or review existed to repair before beginning
the bounded falsification work.

## Failure-first audit

P73 proved the lifecycle of two held classified control permits, but did not
model `commit_room_messages_if_members_with_hook`. That production primitive
composes several recipients and ordered phases: it must reserve all logical
frames, revalidate exact member routes and sender generations, await an async
durable-state hook, commit phase zero, call one decision callback with the
exact phase-zero failures, and either continue or cancel dependent phase-one
permits. Physical receiver close remains possible after the durable hook;
normal sender replacement is blocked by the routing read guards held across
the transaction.

The Rust audit found that the successful high-level two-frame transaction test
still used the legacy Tokio channel. Classified high-level coverage exercised
only one degradation case, and final-permit wake coverage called `recv()` even
though the production writer path calls `recv_batched()`. Focused baseline runs
of all 51 outbound-queue tests and all 26 message-coordinator tests were green;
the audit found a proof-composition gap, not evidence of a production defect.

## Formal proof and non-vacuity

`RoomMessageTransaction.tla` fixes the production-critical bound at two
recipients and two phases while exploring every reservation order, pre-hook
leave or sender-generation replacement, hook accept / reject / error,
post-hook close, a synthetic stale-permit composition action, both phase-zero
callback decisions, and all frame resolutions. Its healthy configuration
reaches 2,850 generated and 1,091 distinct states at depth 16.

The invariants require complete reservation and final exact-routing validation
before the hook, no publication without hook acceptance, phase-zero completion
before phase one, exactly one callback with the exact phase-zero failure count,
exact total degraded-frame accounting, and equality between live reserved
frames, capacity claims, and permit-backed producer capabilities. Every
terminal outcome releases all permits. Stale-scope rejection is imported from
P73's independently mutated queue proof; P74 verifies the surrounding
transaction's accounting when that queue lemma returns cancellation.

Seven independent expected-failure configurations seed premature hook
invocation, skipped routing validation, phase inversion, duplicate callback,
wrong callback input, wrong failed-frame total, or omitted permit release.
Each must fail with its exact named invariant; all seven diagnostic oracles
pass. The healthy model passes exhaustively.

## Deterministic production-seam coverage

The classified queue integration regressions now pin the production
composition without sleeps:

- a two-frame transaction at minimum control capacity waits for one occupied
  slot, then commits both phases in order and leaves capacity reusable;
- a synthetic queue-scope invalidation during the async hook makes one held
  phase-zero permit stale, and the callback observes exactly one failure;
  callback continuation commits an incumbent's independent phase-one frame,
  while callback stop cancels it and reports exactly two failures. This
  defense-in-depth seam injection bypasses normal routing and is not presented
  as a reachable routed lifecycle interleaving;
- replacement of the routed sender generation before final validation returns
  `RoutingChanged`, never calls the hook, publishes no partial frame, and
  releases every reservation;
- hook rejection and injected hook error publish nothing, skip the phase
  callback, cancel all four reservations exactly, and leave both recipients'
  capacity reusable; and
- final stale-permit cancellation wakes the production `recv_batched()` path
  to terminal EOF after a generation transition has drained.

The focused message-coordinator suite grows from 26 to 30 passing tests, and
the batched receiver regression passes independently. The formal README now
matches the current process-local design: the shared `RoomEventMutationGuard`
and FIFO `RoomEventSequencer` serialize same-room mutations, while physical
receiver closure during the async hook is handled by exact degraded
accounting.

## Review and publication evidence

The first adversarial audit found no production divergence in permit
accounting, commit-time scope fencing, or producer wake behavior. It identified
the classified high-level and batched-receiver integration gaps closed above.
The final shared-tree audit corrected two overclaims: normal sender replacement
cannot interleave with the hook while routing read guards are held, and P74
imports rather than reproves P73's no-stale-scope-commit lemma.

The final local tree passes formatting, warnings-denied all-target/all-feature
Clippy, the locked all-feature Rust suite, the complete TLA+ corpus (including
the 19.2-million-state simulation), all Z3 proofs, Cargo deny, documentation
consistency, workflow hygiene, LLM file/example policies, hook readiness, and
both worktree hook preflights. Cargo deny reports only the repository's existing
allowed duplicate/license warnings. The profiled pre-commit preflight passed in
1,737 ms; its 813 ms panic scan and 586 ms changed-file discovery remain within
already-open issue #318 rather than expanding this correctness phase.

The proof and regression commit is `563afc4`. Pull-request check identities and
reviewer status are recorded after the branch is published.
