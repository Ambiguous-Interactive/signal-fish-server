# Session 119 — Room-scoped routing isolation

## Correctness-first scope

The default branch began clean, with nine open issues, no open or draft pull
requests, no dependency pull request, and all 18 applicable workflows green on
`b2eee7c`. Issue #290 remains the highest gameplay risk, but P56 can advance
only through the unchanged scheduled H14 cohort, now 5/20 eligible first
attempts. P53 is likewise evidence-bound at 4/20 scheduled allocations per OS.
No manual run can advance either pre-registered cohort.

The next actionable boundary was a production defect under issue #220, recorded
concretely as issue #329. P76
proved that separate process-local room-event lanes progress independently,
but the coordinator still retained process-global `room_players` and
`local_clients` Tokio locks across room-local durable/replay hooks and async
join/reconnect baseline builders. Ordinary latency in one room could therefore
exclude routing mutation in every room; after a writer queued, Tokio's fair,
write-preferring lock could also queue later unrelated relay readers.

## Failure-first evidence

The deterministic production regression pauses room A inside an exact
transaction's durable hook, then registers and relays to room B. On unchanged
`main`, `paused_room_transaction_does_not_block_other_room_registration`
failed because room-B registration remained pending. This establishes a live
cross-room availability defect rather than an assurance-only recomposition of
the P74–P76 proofs.

## Implementation

The coordinator now resolves stable weak-registry routing gates keyed by room.
Exact transactions and replay-hook publications retain the room's read gate;
initial join/reconnect registration and every same-room route mutation retain
its write gate. The process-global maps remain the storage directory but are
held only for brief snapshots or updates, never across asynchronous hooks or
builders. Active routed rooms retain their gate identity so the healthy relay
path does not allocate a new lock for each frame; pointer-checked weak cleanup
reclaims inactive entries safely.

A separate weak player gate serializes one identity's route changes. Each
mutation snapshots that player's old rooms, combines the destination, sorts and
deduplicates the UUIDs, and acquires every affected room write gate in canonical
order. This preserves unique routing during concurrent cross-room moves without
introducing a reverse-index consistency surface. Slow-consumer pruning,
terminal-tail capture, exact routed lookup, game-data stamp/snapshot builders,
and sync/async initial registration all participate in the same boundary.

## Deterministic coverage

Barrier-driven tests cover all three formerly global asynchronous fences:
paused exact transaction hook, paused replay hook, and paused async initial
builder. In each case same-room mutation/publication remains pending while an
unrelated room completes both routing and relay. An opposite-room reroute test
proves canonical multi-room acquisition terminates with one exact route per
player, and a registry lifecycle test proves stable active identity plus
complete inactive room/player cleanup. Existing replay/live exact-once,
baseline-first, terminal watermark, route generation, hook outcome, and
degraded-publication tests remain the semantic oracles.

The checked-in mutation inventory remains exactly 389. The production relay
allocation benchmark retains its one-allocation ceiling and zero healthy
delivery-handle clones, so neither mutation sharding nor the P71/P72 hot-path
budgets change.

## Hosted evidence integrity

Scheduled run `31353165063` advances P56 to 5/20 eligible first attempts.
Relay Timing run `31297435602` supplies complete eligible Linux, Windows, and
macOS artifacts and advances P53 to 4/20 per OS. The first P53 samples cross an
implementation boundary: semantic outcomes may be aggregated, but any timing
threshold or comparative timing claim must remain stratified by source commit
or use one implementation cohort. This phase changes no selector, workload,
threshold, toolchain, or evidence contract.

## Adversarial review

The first plan proposal broadly recomposed P74–P76 and was rejected as
duplicative. Independent production review instead found the concrete
process-global await fence, and a second reviewer confirmed its cross-room
availability blast radius while rejecting unsafe snapshot/drop/revalidate
shortcuts. Issue #329 records the resulting failure and acceptance boundary.

Implementation review then found and closed three evidence gaps: an initially
vacuous opposite-reroute scheduling witness, missing cancellation/error cleanup
coverage, and missing stale-destructor/replacement coverage. The exact
acquisition-order oracle, aborted/failed initial-builder regression, and
pointer-identity replacement test now pin those cases. The allocation benchmark
also caught an enlarged async-trait future before publication; an uncontended
immediate handoff restored the existing 88/560-byte result while boxing the
larger state only on actual contention. Final independent code and
documentation re-reviews report zero findings.

## Local verification

The complete local gauntlet passes: formatting; clippy for all targets and
features with warnings denied; the locked all-feature Rust suite; all TLA+
models and expected failures; all Z3 proofs; cargo-deny; 389-mutant inventory;
the production relay allocation/clone ceilings; CI, MSRV, workflow, tooling,
LLM, and documentation policies; all 212 live GitHub Action tag checks; and
hook readiness. The first all-feature run usefully exposed three new
barrier-test assertions whose nested `try_recv` expressions violated the
repository's explicit-result policy; binding and asserting the messages fixed
that policy failure, and the complete rerun passed.
