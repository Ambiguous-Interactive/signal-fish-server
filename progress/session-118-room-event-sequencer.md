# Session 118 — Exact room-event mutation handoff

## Correctness-first scope

GitHub triage found nine open issues, no open or draft pull requests, no
Dependabot work, and a green `main` baseline. Issue #290 remains the highest
direct gameplay risk, but the remaining P56 acceptance is the unchanged
scheduled hosted cohort at 4/20; manual runs cannot advance it. P53 likewise
remains at 3/20 eligible scheduled allocations per operating system. No
selector, workload, timing threshold, or contract version changed in this
session.

The next actionable in-repository boundary was issue #220. The room/session
models treat each event as atomic, while production transfers a per-room Tokio
mutex guard synchronously into a lane-owned job. That mechanism needed its own
bounded proof and deterministic regressions before later models could rely on
the abstraction without implicitly inventing a general multi-job FIFO.

## Bounded proof

`RoomEventSequencer.tla` models two same-room events and two weak-registry lane
generations. It covers guard acquisition, mutation, synchronous enqueue,
queued/running execution, success, explicit error, isolated panic, receiver
detachment while work remains live, worker drain and restart, and stale weak
registry cleanup racing a replacement.

The healthy configuration exhausts 3,016 generated / 1,704 distinct states at
depth 19. It proves exact mutation, admission, start, and terminal order; guard
ownership through lane-owned work; zero mutated-event loss after caller
detachment; the same-room terminal barrier; exact queue and active-worker
state; panic recovery; and active-generation registry protection. Four
targeted mutants independently release the guard at enqueue, cancel work with
its receiver, strand the worker after panic, or let stale cleanup remove a
replacement. Each expected-failure configuration reports its exact named
diagnostic. A fifth exact-failure configuration is a non-vacuity witness that
forces live receiver detachment before panic, replacement cleanup, and a
successful second event in exact terminal order.

The theorem is deliberately process-local. It assumes an active Tokio runtime,
makes no multi-node ordering claim, and cannot provide liveness for a job whose
own future blocks forever.

## Production regressions and diagnostics

Seven deterministic seams now pin the production lifecycle. They prove the
same-room mutation lock remains pending through its predecessor, a queued
receiver drop cannot cancel owned work or release the gate, explicit errors
and isolated panics release the next event, different rooms progress
independently, drain-empty handoff covers both worker reuse and restart, and a
stale lane destructor cannot delete a replacement registry entry. Futures are
polled explicitly where pending state is the assertion, progress waits are
bounded at five seconds, and the drain handoff uses a test-only post-job seam
rather than sleeps.

Panic isolation now retains both observability surfaces: normal error display
still includes the child-task panic diagnostic used by production logs, while
the `anyhow` source chain preserves the semantic Tokio `JoinError` and its
`is_panic()` classification.

The production-shaped extraction of lane construction adds one scoped mutant.
The measured inventory is therefore 389. Forty contiguous shards still cap
the modeled worst shard at ten mutants, or 290 seconds at the existing
29-second budget, so neither the shard count nor the ten-minute timeout needed
to change.

## Adversarial review

Three initial audits independently selected this boundary. Formal and Rust
adversarial reviews then found and closed: an initially underconstrained
recovery chronology, overbroad documentation, scheduler-sensitive pending-lock
assertions, an unreachable registry-race setup, incomplete drain reuse/restart
coverage, unbounded progress waits, and string-only panic classification.

The integration review additionally caught hidden top-level panic details, a
receiver drop that covered only the running state, an excessive unit-test
timeout, and duplicate changelog framing. The final implementation preserves
detailed display plus the semantic source, drops the receiver at the
current-thread pre-yield queued point, uses five-second bounds, and consolidates
the assurance into the existing Unreleased issue-#220 entry. All independent
re-reviews ended with zero code or documentation findings.

## Changelog classification

This phase strengthens assurance for an existing process-local runtime
contract; it changes no wire protocol, configuration surface, or advertised
deployment behavior. The existing Unreleased issue-#220 formal-verification
entry is expanded rather than adding a duplicate release-note bullet.

## Verification

The final repository gauntlet passes: formatting, clippy with warnings denied,
the locked all-feature Rust suite, all TLA+ models and expected failures, all
Z3 proofs, cargo-deny, the exact mutation inventory and feasibility guards,
CI/MSRV/workflow/LLM policies, documentation consistency, hook readiness, and
worktree pre-commit/pre-push checks. The pre-commit hook remained functionally
green but took 2,708 ms versus its 1,000 ms target; that known repository-wide
latency is already tracked by issue #318. The single ready session pull request
is monitored through hosted checks and reviewer feedback until green.
