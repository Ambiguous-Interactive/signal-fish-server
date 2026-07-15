# Formal Verification (TLA+ / TLC)

This directory contains the TLA+ specification of the Protocol v3 per-room session
lifecycle — finalize-time plan selection, authoritative per-recipient
`SessionPlan` publication, late-join / seat-fill membership refreshes, and
host-failover re-planning — together with the TLC
model configurations that exhaustively check it.

The spec mirrors the implementation, not an idealization: every operator corresponds to
a concrete function in the Rust code, and each TLA+ action models one membership-touching
event together with all of its session side effects as a single atomic step. That
atomicity is a deliberate **sequential abstraction**: the server runs each event's side
effects on one task, but it does not serialize distinct events on the same room against
each other — see [Atomicity argument](#atomicity-argument) for exactly what the
abstraction proves and what it leaves to the heal-on-next-event mechanism. When
`src/server/session_policy.rs`,
`src/server/signaling.rs`, or `src/server/room_service.rs` change behavior, the spec must
be re-checked and, if the contract moved, updated deliberately — CI enforces a run via
path filters in `.github/workflows/formal-verification.yml`.

## Layout

Each `.tla` module carries one or more `<Module>_<Scenario>.cfg` configurations;
the runner auto-globs **every** `formal/tla/*.cfg`, so a new spec or scenario is
picked up with zero CI plumbing. A configuration whose basename ends `_Sim` is
checked by **bounded random simulation** (`tlc -simulate`) instead of exhaustive
enumeration — for a state space deliberately too large to enumerate; it shares
the module's invariants and a violation still fails the run (TLC exits non-zero
under `-simulate`). Everything else is exhaustive and CI-gating.

| Path                          | Purpose                                                                            |
| ----------------------------- | --------------------------------------------------------------------------------- |
| `tla/SignalFishSession.tla`   | Per-room session lifecycle: negotiation, finalize, replan, late-join, reconnect (`_Mesh` / `_Host` / `_HostDirect` / `_Floor`) |
| `tla/DeliveryContract.tla`    | The #131 deliver-or-disconnect queue contract: bounded queue, backpressure, grace expiry, conservation |
| `tla/DeliveryContractTrace.tla` | P10.D7 replay checker for generated reliable-queue JSONL traces; an invalid next action deadlocks at its exact index |
| `tla/ConnectionTeardown.tla`  | Per-connection task teardown: no zombie sockets, exact drop accounting             |
| `tla/SequencedRelay.tla`      | v3 per-(sender, room) sequence contract: gap accountability + the split-brain theorem |
| `tla/ReconnectReplay.tla`     | v3 reconnect replay: faithful replay, honest status + the split-brain theorem      |
| `tla/RoomLifecycleGC.tla`     | Room GC vs activity refresh + the reconnection-window guard (BUG-1)                |
| `tla/SenderPacingReaper.tla`  | Sender-pacing vs the activity reaper: the timeout inversion, discrete-time (BUG-2) |
| `tla/ControlPriorityDelivery.tla` | Spec-first for v3/P10.E2: control-priority queue split + sojourn eviction (liveness) |
| `tla/DeliveryClasses.tla`     | Spec-first for v3/P10.E2: reliable/latest/volatile delivery classes + supersession accounting |
| `tla/EndToEndGapAccountability.tla` | Flagship v3/P10.D4 composition: end-to-end gap accountability over two senders + socket-buffer loss + reconnect snapshot heal (validates E5); exhaustive `_Small` + simulation `_Sim` |
| `traces/slow-consumer-close-flush-invalid.jsonl` | Checked-in negative proving a slow-consumer close cannot enter the healthy lifecycle close-flush path |
| `traces/post-queue-close-live-drain-invalid.jsonl` | Checked-in negative proving a canceled live writer cannot drain after finalization closes the queue |
| `z3/protocol_invariants.py`   | Z3 SMT proofs of the pure decision functions (selector, glare, host election)     |

This directory holds **two complementary** formal checks:

- **TLA+ / TLC** (`tla/`) explores the reachable _states_ of the per-room session
  lifecycle (join / depart / finalize / replan / late-join / reconnect) and checks
  invariants and temporal properties over them.
- **Z3 / SMT** (`z3/`) proves _universally quantified_ properties of the pure
  decision functions — the ladder selector, the `all_support` relay-floor
  invariant, the glare/offerer rule, and host election — over **unbounded** inputs
  (any member count, any capability mix, any id space) that an explicit-state
  checker can only sample. See [Z3 proofs](#z3-proofs) below.

## How to run

```bash
# TLA+: every configuration in formal/tla/ (downloads + verifies the pinned tla2tools.jar once):
bash scripts/run-tla-model-check.sh

# One configuration / full TLC output:
bash scripts/run-tla-model-check.sh --config Mesh
bash scripts/run-tla-model-check.sh --config Host --verbose

# P10.D7: capture the paused-clock delivery property corpus, then replay it.
: > /tmp/delivery.jsonl
SIGNAL_FISH_DELIVERY_TRACE_PATH=/tmp/delivery.jsonl PROPTEST_CASES=32 \
  cargo test --locked --features trace-validation \
  --test model_based_state_machines delivery_contract_matches_reference_ledger -- --exact
SIGNAL_FISH_DELIVERY_TRACE_PATH=/tmp/delivery.jsonl \
  cargo test --locked --features trace-validation \
  --test e2e_tests test_websocket_connection -- --exact
bash scripts/run-delivery-trace-validation.sh /tmp/delivery.jsonl

# Z3: all SMT proofs (needs the python `z3` module — `python3-z3` or `pip install z3-solver`):
bash scripts/run-z3-proofs.sh
```

Requirements (TLA+): a Java runtime (11+). The script downloads `tla2tools.jar` pinned by
version **and** SHA256 into `${XDG_CACHE_HOME:-~/.cache}/signal-fish/tla` (override with
`SIGNAL_FISH_TLA_CACHE_DIR`) and re-verifies the checksum on every run, so a corrupted or
tampered jar never executes. CI runs both scripts via
`.github/workflows/formal-verification.yml` (a `tlc` job and a `z3` job).

## Delivery trace validation (P10.D7)

The trace pilot closes one deliberately narrow spec-to-code loop. With the
internal `trace-validation` Cargo feature, a harness can attach one
`DeliveryTraceRecorder` to a `ConnectionCloseSignal`. The exact
`deliver_or_disconnect` result arms, writer write/finalize points, queue-close,
and close transition then append ordered, payload-free events under one
per-connection mutex. It is inert unless explicitly attached or the feature
build is run with `SIGNAL_FISH_DELIVERY_TRACE_PATH`; that environment variable
attaches it to real v2 socket tasks for the nightly E2E acceptance trace.

`scripts/generate-delivery-contract-trace.py` accepts only the declared
`v2_legacy_reliable_fifo` projection: one bounded v2 FIFO, reliable messages,
and no generation cancellation, lossy class, or sojourn-only outcome. V3
classified
`AccountedDrop`/`Canceled` and fail-closed metadata/accountability branches emit
`Unsupported`, which the generator rejects rather than relabeling. Shutdown's
priority upgrade over an earlier close reason is also outside the base model's
strict first-reason abstraction. This scope matches the existing paused-clock
model-based producer property plus the real-socket v2 E2E case that drives the
production writer/finalizer hooks. Harness-only receiver mutations stop the
producer projection rather than inventing socket events. A dequeue that races
ahead of its post-send enqueue record is explicitly `Unsupported`, so an
overlapping history fails closed instead of becoming a false TLC divergence.
The inverse observation race is rejected too: if Tokio has freed a physical
slot but the writer has not yet recorded the dequeue, a successful enqueue
cannot be projected onto the still-full model queue.
Any direct/untraced v2 queue item also emits `Unsupported`: hidden occupancy
would otherwise change FIFO capacity and make a valid `SendFull` look invalid
to the projected model.

The generated module supplies concrete traces to
`DeliveryContractTrace.tla`. Each delivery call becomes a one-message model
sender. `WriterStart` moves the queue head to an explicit `inFlight` slot when
the real writer frees queue capacity; `WriterDrain` resolves it only after a
successful socket write. The slot also records its `Live` or `CloseFlush`
phase, and both the strict generator and TLA guard require the matching drain;
a hostile trace cannot relabel a forbidden live drain as a close-flush drain.
`CloseFinish` accounts an interrupted in-flight item with the closing
connection. `QueueClose` separately marks the point at
which new sends begin returning channel-closed, and slow-consumer closes may
never take the healthy lifecycle close-flush path. TLC chooses every captured trace and permits
only event `i`; a false guard is therefore a deadlock whose state names both
`traceId` and `i`. The wrapper also generates a seeded-negative bundle that
substitutes `WriterDrain` at `i = 1`; the empty initial queue must deadlock, and
the wrapper fails if TLC either accepts it or fails for another reason.
It also replays a checked-in negative trace proving
`GraceExpired -> QueueClose -> CloseFlushStart` deadlocks at the flush action.
Another proves a live `WriterDrain` may complete during close-request overlap,
but never after the finalizer has emitted `QueueClose`.

The daily `verification-nightly.yml` job is initially informational
(`continue-on-error: true`). It uses a fixed proptest seed, retains the JSONL
and TLC evidence, and runs only for schedule/manual dispatch—not PRs. Parser,
schema, feature compilation, action emission, and positive generator tests
remain ordinary PR gates.

## Z3 proofs

`z3/protocol_invariants.py` discharges 14 proof obligations across four sets, each by
asserting the **negation** of a property and checking it is `unsat` (no counterexample
exists, so the property holds for every input):

| Set | Models | Proves |
| --- | ------ | ------ |
| **A** | the ladder walk in `choose_session_plan` (`session_policy.rs:338`) | the selector is total and legal, never exceeds the `desired` ceiling, falls to the relay floor when no transport is enabled, is sound (a chosen rung genuinely fits), is richest-first (mesh+webrtc is never skipped when it fits), and never enables WebRTC signaling for a `host+direct` plan |
| **B** | `all_support` over an unbounded member set (`session_policy.rs:175`) | a single non-v3 member denies every upgrade rung (the relay-floor back-compat invariant), `all_support` implies pointwise support, and an empty room never upgrades |
| **C** | `local_initiates` glare rule (`signaling.rs:60`) | exactly one peer offers per distinct pair, no peer self-initiates, and the offer orientation is acyclic (no glare deadlock) |
| **D** | `elect_host` (`session_policy.rs:372`) | `(joined_at, id)` totally orders members (a unique host), and a seated authority is the unambiguous host |

The proofs are deliberately _decomposed from member counting_ where it sharpens decidability
(set A abstracts each rung's `all_support` to a free boolean; set B re-attaches it), and the
harness is self-checking: a deliberately wrong selector produces a `sat` counterexample, so a
`PASS` is never vacuous.

## Correspondence table (spec ⇄ code)

Function names are the stable anchors; line numbers are as of the commit that introduced
this table and may drift a few lines.

| Spec operator / action                  | Code                                                                                          |
| --------------------------------------- | --------------------------------------------------------------------------------------------- |
| `UpgradeLadder`                          | `UPGRADE_LADDER` — `src/server/session_policy.rs:179`                                          |
| `RelayPair` / `RelayPlan`                | `RELAY_FLOOR` and the explicit relay branch in `membership_session_decision` — `src/server/session_policy.rs` |
| `TopologyRank`                           | `topology_rank` — `src/server/session_policy.rs:197`                                           |
| `TransportEnabled`                       | `transport_enabled` — `src/server/session_policy.rs:207`                                       |
| `IsValidPair`                            | `is_valid_pair` — `src/server/session_policy.rs:224`                                           |
| `SupportsSession`                        | `SessionMember::supports_session` — `src/server/session_policy.rs:142`                         |
| `AllSupportOver`                         | `all_support` — `src/server/session_policy.rs:164`                                             |
| `ChoosePair`                             | ladder walk in `choose_session_plan` — `src/server/session_policy.rs:254`                      |
| `ElectHost`                              | `elect_host` — `src/server/session_policy.rs:303`                                              |
| `Pairable`                               | `SessionPlanDecision::pairable` / `ActiveSessionPlan::supported_by` — `session_policy.rs:366`  |
| `HostInvalid`                            | `ActiveSessionPlan::host_invalid` — `src/server/session_policy.rs:88`                          |
| `PlanFor`                                | `SessionPlanDecision::plan_for` + `host_peers_for` — `src/server/session_policy.rs:409,450`    |
| `V3Members` delivery gate                | v3 gate in `send_session_plan_to` — `src/server/session_policy.rs:899`                         |
| `PlansForAll`                            | `send_session_plans_to_members` — `src/server/session_policy.rs:860`                           |
| `PlanPublication`                        | per-recipient plan batches in `start_game_publication_builder` / `publish_finalized_join_membership` |
| `ReplanResult`                           | `replan_host_session` — `src/server/session_policy.rs:793`                                     |
| `LateJoinResult` (inside `Join`)         | `membership_session_decision` + `publish_finalized_join_membership`                            |
| `DepartureResult` (inside `Depart`)      | `handle_session_member_departure` — `src/server/session_policy.rs:692`                         |
| `Finalize` trigger                       | `RoomOperationCoordinator::handle_start_game` — `src/coordination/room_coordinator.rs:568` (explicit `StartGame`: not already `Finalized`, every current player ready, sender authorized — the room's `authority_player` if set, else any member; min 1 player) |
| `Finalize` emission                      | `emit_session_plan` (after coordinator finalize) — `src/server/session_policy.rs:544`          |
| `Join` fullness-only gate                | `add_player_to_room` — `src/database/mod.rs:398` (seat-fill into `Finalized` non-full is legal) |
| `Depart` + authority clearing            | `leave_room` — `src/server/room_service.rs:279`; `remove_player_from_room` — `database/mod.rs:412` |
| `GrantAuthority`                         | `request_room_authority` — `src/database/mod.rs:477` (no version gate; only while unheld)      |
| `Finalize` membership precondition (`members # <<>>`) | `Room::should_enter_lobby` — `src/protocol/room_state.rs:350` (lobby is now entered by any **non-empty** `Waiting` room; `max_players` is a **ceiling**, not a required count — the old fullness gate is gone, so finalize may fire below `max_players`) |
| `r < q` (glare rule, election tie-break) | `local_initiates` — `src/server/signaling.rs:60`; UUID order via integer player ids            |

### Invariants and properties

| Name (spec)                   | Pins (code contract)                                                                        |
| ----------------------------- | -------------------------------------------------------------------------------------------- |
| `TypeOK`                      | Variable domains; member list duplicate-free and within `max_players`                         |
| `AuthorityIsCurrentMember`    | `remove_player_from_room` clears a departing authority                                        |
| `PlanLegality`                | Only ladder rungs are ever stored; the relay floor is never stored (`is_valid_pair`)          |
| `V2Gating`                    | Appendix K: no `SessionPlan` ever reaches a sub-v3 connection                                  |
| `HostValid`                   | A stored host plan always names a current, session-capable member — a theorem of the atomic-event abstraction (see [Atomicity argument](#atomicity-argument)) |
| `CeilingRespected`            | Stored topology rank never exceeds the desired ceiling (`topology_rank` gate)                 |
| `PeerCapability`              | No peer list (even a stale one) names the recipient or a member that cannot run the pair      |
| `MeshPlanExactness`           | Fresh mesh plans list exactly the other capable members, glare-correct `initiate`             |
| `GlareAntisymmetry`           | Exactly one initiator per mutually listed mesh pair — the smaller id (`local_initiates`)      |
| `StarProperty`                | Fresh host plans form a star: host answers all capable clients; clients offer to host only    |
| `EmissionMatchesSessionState` | Fresh plans match the stored decision, or explicitly reset to relay when no plan is stored     |
| `NoEmissionWithoutQualifier`  | A replan emission implies a capable elected host; the no-qualifier arm drops + stays silent   |
| `PublicationCoverage`         | Every publication covers exactly every current v3 member, including incumbents after a join   |
| `StickyPairProperty` (action) | Topology/transport never change while stored; failover rewrites only `host`                   |
| `HostDepartureHealedSameStep` (action) | A departing stored host is re-elected (qualifier survives) or the entry dropped — same step |
| `RelayFloorOnly` (`Floor` model only) | With both upgrade transports disabled, no plan is stored; every v3 delivery is an explicit relay plan |

## Model configurations

`MAX_PLAYERS` is smaller than the player universe in every model, so finalize-time
membership varies per behavior. Finalization is now driven by an **explicit
`StartGame`** (`handle_start_game`), not by the room becoming full, so a room can
finalize at **any non-empty membership** from 1 up to `MAX_PLAYERS` — `max_players`
is a ceiling, not a required count. Post-finalize **seat-fill** joins (the
`add_player_to_room` fullness-only gate) then bring capability-mismatched players into
live sessions, up to that same ceiling.

| Configuration | Players (profiles)                                                       | Reaches                                                                                                       |
| ------------- | ------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------- |
| `Mesh`        | 2 × `V3Full`, `V3MeshWebRtc`, `V3HostWebRtcOnly`, `V3RelayOnly`           | mesh+webrtc rung; host+webrtc under the mesh ceiling; relay floor; v3-but-incapable seat-fills both directions |
| `Host`        | 2 × `V3Full`, `V3HostWebRtcOnly`, `V2`                                    | host+webrtc star; v2 seat-fill with full v3-incumbent refresh; capability-filtered failover; v2 authority never elected; no-qualifier entry drop |
| `HostDirect`  | `V3HostDirect`, 2 × `V3Full`, `V3RelayOnly`; `enable_webrtc = false`      | host+direct rung via the transport config gate; complete non-WebRTC `SessionPlan` refreshes                     |
| `Floor`       | same players as `Mesh`; `enable_webrtc = false`, `enable_direct = false`  | enabled-gate denial end-to-end: nothing stored; explicit relay plans delivered to every current v3 member (`RelayFloorOnly`) |

The five required capability profiles (`V2`, `V3RelayOnly`, `V3MeshWebRtc`,
`V3HostWebRtcOnly`, `V3Full`) are all covered across the `Mesh`, `Host`, and
`HostDirect` models; `V3HostDirect` (direct without WebRTC) is added so the third
ladder rung is reachable while WebRTC-capable members exist. The `Floor` model pins the
opposite direction — config gates deny every rung even though the members could run
them, so `ChoosePair` must always return the relay floor, the room must store
nothing, and every v3 publication must be an explicit relay reset. The
`DESIRED = relay` ceiling (denial by topology rank rather than transport
gate) is additionally covered by the randomized-config proptests
(`session_policy_tests.rs::properties`, whose generated `SessionConfig`s draw
`default_topology` and per-game mappings from all three topologies).

### Observed state spaces (TLC 2.19, `CHURN_BUDGET = 10`, 12 workers)

| Configuration | States generated | Distinct states | Graph depth | Wall time |
| ------------- | ---------------- | --------------- | ----------- | --------- |
| `Mesh`        | 517,050          | 151,344         | 12          | ~7 s      |
| `Host`        | 79,948           | 22,710          | 12          | ~2 s      |
| `HostDirect`  | 111,630          | 32,751          | 12          | ~3 s      |
| `Floor`       | 66,463           | 16,676          | 12          | ~2 s      |

The reachable state space includes finalization at every non-empty membership below
`MAX_PLAYERS`, plus an explicit relay delivery for each v3 recipient and complete
membership refreshes after finalized joins. Those observable publications make the
floor configuration non-vacuous. Every listed invariant and action property holds
across all four models.

Reachability of the interesting states was confirmed with temporary negated "sanity"
invariants during development (each must be _violated_): mesh and host plans stored, the
host+direct rung selected, replan emissions, v2 seat-fill into a live session,
the no-qualifier entry drop, and full late-join plan publication. The `Floor`
model reaches explicit relay deliveries while proving that no sticky plan is
ever stored, directly through the `RelayFloorOnly` invariant.

## Modeling decisions

### Atomicity argument

Each TLA+ action bundles one external event with all of its session side effects into a
single atomic step. _Within_ one event the server really is sequential: `leave_room`
runs the departure hook inline after the `PlayerLeft` broadcast
(`src/server/room_service.rs`), the join/reconnect handlers run
`handle_active_session_late_join` inline, finalize runs `emit_session_plan` inline, and
`replan_host_session` rewrites the stored entry _before_ emitting, so every emission is
computed from one membership snapshot and is internally consistent. Cross-room
interleavings do not interact (state is per-room).

What the abstraction deliberately drops: distinct events on the same room are **not**
serialized against each other. `leave_room` takes no per-room lock (see the concurrency
note on `handle_session_member_departure` in `src/server/session_policy.rs`), so two
concurrent departures can interleave: a stalled departure hook can insert a stored-host
entry computed from its older membership snapshot _after_ a faster event already healed
the entry — the stored-plan map is last-writer-wins — resurrecting an already-departed
player as the stored host. The reconnect-failure rollback (`reject_claimed_reconnect`
in `src/server/reconnection_service.rs`) is another window: it removes a just-restored
member without running the departure hook.

`HostValid` is therefore a theorem **of the sequential abstraction**, not of every
machine-level interleaving. The contract the running system keeps is _eventually
healed_, not instantaneously valid: between heals a stored host is a current capable
member; a concurrent-membership interleaving can transiently wedge the stored entry;
and the next membership-touching event repairs or drops it, because the
`host_invalid` trigger is deliberately "the stored host is invalid" rather than "the
departed player was the host" — it was widened exactly so these windows self-heal (its
doc comment enumerates them). The model proves the strong half — no _sequence_ of whole
events ever wedges a room — and the Rust unit tests around the widened trigger
(`session_policy_tests.rs`, `signaling_tests.rs`) cover the transient windows the
abstraction collapses.

### Stale client-held plans (relay-floor soundness)

When the stored entry is dropped (last member out, or no qualified host remains), the
server does **not** retract previously delivered plans — clients keep them and fall back
to the relay floor. The model mirrors this honestly: `delivered` is never cleared, so
"no stale plan exists" is deliberately **not** an invariant. The soundness claim is about
_emission_: `EmissionMatchesSessionState` states plans carry the stored pair and
host when one exists, or the explicit relay reset when it does not. Exactness claims
(`MeshPlanExactness`, `StarProperty`, `GlareAntisymmetry`) are evaluated against
`lastEmission` — the freshly emitted plans, for which the membership is current — because
a stale plan legitimately disagrees with the current membership (mesh departures re-emit
nothing; `PlayerLeft` prunes client-side).

### Liveness

Healing is atomic with its triggering event, so the meaningful "eventually" facts are
single-step consequences, stated as **action properties** (`StickyPairProperty`,
`HostDepartureHealedSameStep` — both `[][...]_vars`, checked under `PROPERTIES`). A
classic weak-fairness liveness property would have to assume the _environment_ keeps
acting (clients keep joining/departing), an assumption the server neither makes nor
controls — under churn-budget exhaustion the room legitimately stutters forever. No
fairness conditions are therefore asserted, and `HostValid` (a state invariant) already
expresses the strongest form of "the room is never wedged on an invalid host": it holds
in _every_ reachable state of the model. (How that maps onto the running system's
eventually-healed contract is the [Atomicity argument](#atomicity-argument).)

### Deadlock checking

TLC deadlock checking stays **enabled**. The churn budget's terminal states would be
spurious deadlocks, so the spec adds an explicit `Done` self-loop action once the budget
is exhausted; any remaining deadlock TLC reports is then a real modeling bug (a reachable
mid-protocol state with no enabled action).

### Tooling decisions (P10.D8)

- **TLC-first (explicit-state), not Apalache-first.** Every model here is small and
  finite by construction (tiny budgets/caps), so exhaustive state enumeration is fast and
  gives concrete, minimal counterexample traces — which is what the seeded-bug non-vacuity
  discipline needs. Apalache (symbolic, SMT-backed) is kept **dev-side only** (a future
  `scripts/run-apalache.sh` could discharge, e.g., `SenderPacingReaper`'s inequality
  symbolically for _all_ constant valuations rather than the pinned ones) and is **not
  CI-gating** until it catches something TLC missed — adding an SMT dependency to CI earns
  its keep only then.
- **Discrete-tick integer time, not dense/real time.** Every timed property in these
  models is a _relation between timeout constants_ (grace vs ping deadline, age vs sojourn
  bound), never a dense-time reachability question. An integer `now`/`Tick` (or per-frame
  `age`) with absolute-deadline guards captures them exactly, keeps the state space finite,
  and avoids a real-time model checker. See `SenderPacingReaper` and
  `ControlPriorityDelivery`.
- **The action↔code correspondence style, not PlusCal.** Each action bundles one external
  event with all its side effects and maps to one code function (the correspondence tables
  and mapping comments). PlusCal's generated control-flow variable (`pc`) would break that
  one-action-per-function readability, so it is deliberately **not** used — the specs are
  written directly in TLA+.

## Single-instance theorems (split brain / ARCH-10)

Several of the v3 relay/reconnect invariants are **theorems of a single relay
instance** — they hold for one process that owns a room, and are _false_ once a
load balancer lets the same room live on two instances at once. This is not a
gap in the proofs; it is a deliberately documented boundary of the design. The
server keeps all room state per-instance and in-memory, and
join-with-unknown-code **creates** the room (the `Ok(None)` create arm of
`join_room_with_coordination` → `src/server/room_service.rs:519`), so the same
room code presented to two instances behind a naive LB yields two independent
live rooms — each with its own stamp counter, its own replay ring, and its own
reconnection tokens (ARCH-10 in `PLAN.md`). The honest posture is therefore
**LB room-affinity** (one home per room; reconnects must land on that home),
documented as doctrine — not multi-instance sharding.

Two seeded-bug constants make that boundary **executable** — flip either one and
TLC exhibits the split-brain counterexample:

| Spec (invariant)                                | Seeded constant (checked `FALSE`) | What TRUE models | Result |
| ----------------------------------------------- | --------------------------------- | ---------------- | ------ |
| `SequencedRelay.tla` (`GapAccountable`)         | `SplitBrainStampBug`              | a second instance (`SendSplit`) stamps the same sender's stream from an independent `counter2` (a no-affinity LB collapses both onto one recipient queue) | a recipient interleaves duplicate/regressing `seq` with no bracket → `GapAccountable` violated in 4 actions |
| `ReconnectReplay.tla` (`ReplayFaithful` / `StatusHonest`) | `SplitBrainCounterBug`   | the reconnect is served by a second instance that join-created the room fresh (empty ring, zero watermark, its own `next_sequence`) | the empty replay drops a retained needed event → `ReplayFaithful` violated in 3 actions; `complete`-over-eviction also violates `StatusHonest` at 5 (masked by `ReplayFaithful`@3 unless it is dropped from the checked `INVARIANTS`) |

These join the module's existing non-vacuity constants — `NoResetNotificationBug`
(`SequencedRelay`), `NaiveGapPredicateBug` (`ReconnectReplay`), and `SilentDropBug`
(`DeliveryContract`) — each of which likewise makes TLC produce a labeled
counterexample, so every one of these safety invariants is demonstrably
non-vacuous. All are pinned `FALSE` in the checked `.cfg`s; each spec's header
comment carries the minimal trace and the flip-it-locally instructions.

**Which invariants are single-instance theorems.** Any per-`(sender, room)`
sequencing or per-room replay guarantee — `GapAccountable` (contiguous,
bracket-accounted `seq`), `ReplayFaithful` / `StatusHonest` (honest reconnect
replay), and by extension the `DeliveryContract` conservation law — assumes one
authoritative counter and one queue/ring per room, i.e. a single home. The
session-lifecycle invariants in `SignalFishSession.tla` are likewise per-room and
single-instance by the same construction. The multi-instance seams
(`DedupCache`, the in-memory "distributed lock", `should_process_message`) are
dead stubs today; the deliberate single-node CP stance and the LB room-affinity
requirement are the subject of the `F1` doctrine page in `PLAN.md`.

## Timing theorem (sender pacing vs the activity reaper)

`SenderPacingReaper.tla` (P10.D3) is the repo's first **discrete-time** model
(Appendix-O house rule: an integer `now` + a `Tick` action, timers as
absolute-deadline guards). It pins **BUG-2**, the timeout inversion the P10.A2
config cross-field check prevents: a message handler records a sender's activity
at dispatch, then — still on the same task — does a throttled room refresh
(`maybe_update_last_seen`, a DB write + `rooms` write-lock) and parks on the
broadcast `join_all` while a slow recipient drains (up to
`websocket.slow_consumer_timeout_ms`). The receive loop is that same task, so a
long enough park freezes its recorded activity past `server.ping_timeout` and the
activity reaper evicts the **healthy** sender (close 4003) before its slow
recipient is ever disconnected. Time advances only in the two waiting states: while
parked (capped at the grace deadline) and **once while broadcasting** — the
pre-park delay `d` (the `maybe_update_last_seen` lock/DB await), so the
reaper-visible gap peaks at `d + SLOW`. `HealthySenderNeverReaped == ~sndEvicted`
is the contract, with the stronger `GapWithinPingDeadline` (the reaper never sees a
healthy gap over the deadline) pinning the boundary it rests on.

| Spec (invariant)                                    | Seeded constant (checked `FALSE`) | What TRUE models | Result |
| --------------------------------------------------- | --------------------------------- | ---------------- | ------ |
| `SenderPacingReaper.tla` (`HealthySenderNeverReaped` / `GapWithinPingDeadline`) | `TimeoutInversionBug` | the effective grace period forced to exactly `PING_TIMEOUT` — the `slow = ping` boundary the check rejects (legal per-field, forbidden cross-field; legal system-wide before A2) | via the `d = 1` pre-park path the peak gap reaches `PING + 1 > PING` and the reaper evicts the healthy parked sender → both invariants violated (`GapWithinPingDeadline` trips one step earlier) |

**Derived deliverable (the A2 inequality).** With the pre-park delay `d` (0 or 1
tick) modeled, TLC derives that `slow >= ping` is unsafe — **exactly** the region
`validate_config_security` rejects (`slow_consumer_timeout_ms >= ping_timeout *
1000`): at the boundary (effective grace `= PING`, exhibited via the
`TimeoutInversionBug` flag since the checked configs pin `SLOW < PING`) the
`d = 1` path pushes the peak gap to `PING + 1 > PING` and the reaper (strict `>`)
evicts. Two checked configs are green — `_Small`
(`SLOW = 2 < PING = 4`) and `_Boundary` (`SLOW = 3 = PING − 1`, the tightest safe:
peak gap `SLOW + 1 = PING`, never `> PING`). So the strict `<` is the **necessary
floor** the model derives — it eliminates every `SLOW >= PING` inversion, gross
(60 s vs 30 s) and boundary alike. It is **not proven sufficient**: the model
bounds `d` to one tick, but the lock/DB pre-park delay is unbounded under
contention, so a thin-margin config can still invert if `d` exceeds `PING − SLOW`.
True safety is an operator sizing concern (keep the margin above the worst-case
pre-park delay; the default 25 s dwarfs it) — the check is the derived guardrail
against the provable inversion region, not a liveness proof under unbounded load.

## Delivery-revision spec-first (control priority + sojourn)

`ControlPriorityDelivery.tla` is **spec-first** for the protocol-v3 P10.E2
delivery revision — it is merged BEFORE the code and pins the two properties the
queue split must satisfy, composing with the #131 `DeliveryContract.tla`
substrate rather than re-deriving it. Frames are modeled by CLASS (data | ctrl)
and carry a discrete AGE (the Appendix-O `now`/`Tick` convention, sojourn as an
absolute-age guard). The writer (the peer draining) is deliberately UNFAIR.

| Property | What it pins | Seeded bug (checked `FALSE`) → result |
| --- | --- | --- |
| `ControlAgeBounded` | control frames ride a separate queue drained **strictly before** data — never starved behind a data backlog | `SingleQueueBug` (control misrouted onto the data FIFO) → a data frame is written while a control frame waits behind it → **violated** |
| `DeliveryEventuallyResolves` (liveness) | a frame that sits too long triggers a **sojourn** close, so `enqueued ~> written ∨ closed` holds even against a peer that pings but never reads | `NoSojournEvictionBug` (no sojourn close) → the frame parks forever behind the unfair writer → **violated** |

Also checked: `PerClassConservation` (each class independently queued ∨ written ∨
dropped-with-close), `CtrlDropsAreLoud`, `StalenessBounded` (no head ages past
the bound while open — safety, given the Tick cap), `ShutdownWins`, and
`ShutdownPriorityStable`. The close properties match E3 production semantics:
an ordinary Stale/Lifecycle reason stays stable unless process drain upgrades it
to Shutdown, after which it is terminally stable. The liveness rests only on
`WF(Tick, SojournEvict, CloseFinish)` — **never** on writer fairness, which is
exactly what makes the sojourn close load-bearing.

## Delivery classes (reliable / latest / volatile)

`DeliveryClasses.tla` is the second **spec-first** module for P10.E2 (with
`ControlPriorityDelivery.tla`), pinning the per-class and wire-accountability
contract before the code. One recipient observes one sender whose globally
monotone `seq` spans all three classes and at least two latest keys. This matters:
key-local queue replacement still has to preserve the sender-global WebSocket
order.

- **reliable** — backpressures while the queue is full (enablement); conserved,
  never coalesced.
- **latest** (keyed) — a same-key send removes the queued predecessor and appends
  the successor, preserving global sequence order among surviving frames; a
  new-key send on a full queue drops the oldest volatile or drops the arrival
  (`latDropped`) — it **never parks**.
- **volatile** — enqueue if space, else drop-oldest-volatile (`volDropped`).

Every supersession or best-effort drop atomically appends an **exact gap range**
to the priority `DeliveryReport` queue. The writer drains that control queue
before data. Thus a report is visible before any later GameData can expose the
gap; close-time abandonment is separate because the socket closes loudly and no
later frame is observable on that connection.

This D5 model has one implicit sender in one fixed epoch; D4 is the multi-sender
composition. Its exact-report lane is explicitly bounded (`ReportCap=1` in the
checked model). When that lane is full, a lossy send leaves queued predecessors
untouched, abandons only its new frame with a loud close, and never parks. D2
separately covers general control/data queue priority and sojourn eviction; the
two modules remain consistent standalone models rather than a formal refinement.

| Invariant | Pins | Seeded bug (checked `FALSE`) → result |
| --- | --- | --- |
| `ReliableConservation` | reliable is queued ∨ written ∨ dropped-with-close — never coalesced | `CoalesceReliableBug` → a reliable Head evicted into `volDropped` → **violated** |
| `LatestConservation` / `VolatileConservation` | each class lands only in its legitimate buckets | `MisdropLatestBug` → a latest misdropped into `volDropped` → **violated** |
| `ExactGapAccounting` / `QueueSeqMonotone` / `WireSeqMonotone` | reports name exactly the lost sequences and surviving queued/written data stays globally increasing | `ScalarInPlaceBug` → A1,B2,A3 reports live B2 as lost and leaves A3 before B2 → **violated** |
| `ReportsRemainCausal` / `ReportsAreCausallyPrioritized` | every exact range is queued-or-written while open, and its report precedes later gap-observing data | `SilentSupersedeBug` → supersede without report ledger/queue → **violated** |
| `LatestValueLastWrite` | ≤1 queued latest per key; the queued representative is newest vs superseded ∪ written | — |
| `ReportHonest` | every queued/written cumulative report snapshot is bounded by true counts | `ReportOverstateBug` → published count above truth → **violated** |

Also checked: `TypeOK`, `UniqueSeqs`, `WrittenMatchesWire`,
`CoalesceNeverTouchesReliable`, `DropsWithCloseAreLoud`,
`LossyClassesNeverPark`, `ShutdownWins`, and `ShutdownPriorityStable`. It is
consistent with (not a formal refinement of) the #131 `DeliveryContract.tla`;
its counted per-class dispositions replace that model's single conservation law.

**Seeded scalar/in-place counterexample.** In
`DeliveryClasses_Small.cfg`, changing only `ScalarInPlaceBug = FALSE` to `TRUE`
produces the registered minimal trace:

1. `SendLatest("A")` queues `A:seq1`.
2. `SendLatest("B")` queues `A:seq1, B:seq2`.
3. `SendLatest("A")` replaces A in place, leaving `A:seq3, B:seq2`, and emits
   the old scalar interval `[1,3)` as report range `1..2`.

TLC fails `ExactGapAccounting` immediately after the third send: only A1 is in `superseded`, but
`accountedGaps = 1..2` falsely reports live B2. A second diagnostic run with
the earlier `ExactGapAccounting` and `QueueSeqMonotone` guards temporarily
removed lets that same trace drain: after the report, the writer emits A3 then
B2, and `WireSeqMonotone` fails on that second data write.
With the checked `FALSE` arm, the model removes A1, appends A3, queues exact
singleton range `1..1`, and drains report → B2 → A3. The exhaustive checked
model is green at 390,068 generated / 168,615 distinct states, depth 15.

**Report-capacity edge.** A diagnostic invariant that temporarily forbids the
overflow state fails as soon as a second loss needs the full report lane: with
`ReportCap=1`, one A supersession occupies
the report lane, then the next A offer leaves the queued predecessor untouched,
places only the new sequence in `droppedWithClose`, and atomically requests
Stale close. The checked `DropsWithCloseAreLoud` and `LossyClassesNeverPark`
invariants pin the two production obligations on that reachable branch.

## End-to-end gap accountability (flagship composition)

`EndToEndGapAccountability.tla` (P10.D4) is the **flagship** module: it composes
three previously separate contracts — `SequencedRelay.tla` (per-`(sender, room)`
contiguous stamping + every-gap-bracketed accountability), `ReconnectReplay.tla`
(the bounded replay ring + eviction watermark), and `ConnectionTeardown` (a
slow-consumer eviction that abandons a recipient's queued messages) — into one
behavior and proves the **client-facing** promise: driven only by what the server
puts on its socket, a client can classify every sequence discontinuity it ever
observes, and the reconnection snapshot heals the tail the server dropped when it
evicted the client. This is the executable proof of the P10.E5 client-SDK
re-baseline obligation.

Topology is deliberately **one recipient observing two senders**. Two senders is
mandatory: the single-sender `justified` bracket in `SequencedRelay` is a per-
recipient flag, which is sound with one sender but **unsound at ≥2** — a bracket
armed for sender A is spent by the next contiguous frame from B, so A's later
epoch reset reads as an unexplained regression. D4 tracks justification per
`(recipient, sender)` pair. Beyond the two-sender axis it adds two structures the
single-contract specs cannot see:

- a per-recipient **socket buffer** between the server's writer and the client's
  observation (`WriterDrain` moves a frame queue → `sockBuf`; only `ClientObserve`
  acts on it). An `Evict` wipes **both** the queue and `sockBuf` — the kernel
  send/receive buffers die with the TCP connection — so a frame the server already
  handed to the OS can still be lost. `DroppedNeverObserved` proves a wiped frame
  never resurfaces out of order.
- the reconnection **snapshot as authoritative heal**: on reconnect the server
  sends the current member set (`RoomJoined.current_players`) and each live
  sender's `(epoch, seq)` high-water mark (`Reconnected.sender_watermarks`, the E5
  field). The client (a) **replaces** membership with the snapshot set (an upsert,
  not a delta replay) so a dropped `PlayerLeft` cannot strand a phantom member, and
  (b) **re-baselines** each per-sender `(epoch, seq)` expectation from the
  watermarks so the next frame is contiguous against a fresh baseline.

Three seeded-bug constants make each guarantee **non-vacuous** — all pinned
`FALSE` in the checked configs; flip exactly one to watch TLC catch the design
bug (traces in the module header):

| Seeded constant (checked `FALSE`) | What TRUE models | Result |
| --------------------------------- | ---------------- | ------ |
| `SingleFlagBug` | justification collapsed from per-`(recipient, sender)` to one shared flag | a contiguous frame from `s2` spends the bracket `s1` needs for its epoch reset → `ClientCanClassify` **violated** |
| `NoBaselineResetBug` | reconnect skips the per-sender watermark re-baseline | a post-outage frame is a gap against a stale baseline with no armed bracket → `ClientCanClassify` **violated** (proves E5's watermarks are necessary) |
| `NoSnapshotReconcileBug` | reconnect applies only the (possibly incomplete) delta replay instead of the authoritative member set | a `PlayerReconnected` truncated from the wiped socket buffer + replay ring leaves the client believing a seated sender left → `MembershipEventuallyHonest` **violated** |

Also checked: `TypeOK`, `RingSnapshotSound` (retained control events strictly
ascending, everything evicted strictly below everything retained, no cursor or
watermark ahead of the global counter). `MembershipEventuallyHonest` is encoded
as a safety obligation on the terminal (stutter) states — the house convention
for an eventually-property under TLC.

**Single-instance theorem (ARCH-10).** Every invariant rests on ONE authoritative
relay: one stamp counter per `(sender, room)`, one global control-sequence
counter, one replay ring, and a reconnect served by the instance that owns the
room. D4 does not re-derive the split-brain boundary (that is
`SequencedRelay`'s `SplitBrainStampBug` and `ReconnectReplay`'s
`SplitBrainCounterBug`, above); it composes the single-instance behaviors to prove
the end-to-end client contract.

**Configurations.** `_Small` is exhaustive (2 senders, 1 recipient, a 1-slot
queue, a 1-slot socket buffer, and a 1-slot ring, one leave/rejoin and one
eviction/reconnect cycle — a complete state graph at depth 16). `_Sim` runs the
same invariants over
a **wider** shape (deeper send budget, 2-slot queue/buffer/ring, two cycles) under
bounded random simulation, sampling interleavings the exhaustive model cannot
afford. Both are green with all three bugs pinned `FALSE`.

## Intentionally not modeled (and why)

- **Rate limits / relay backpressure** — quantitative throttling and queue dynamics,
  orthogonal to the session state machine this spec checks. Note what this does **not**
  mean: since the slow-consumer hardening (issue #131), per-connection delivery is _not_
  best-effort — the server never silently drops a relayed message. A full recipient queue
  backpressures the sender for up to `websocket.slow_consumer_timeout_ms`; a recipient
  that still cannot absorb the message is loudly disconnected (`CloseReason::SlowConsumer`,
  surfaced in metrics), abandoning its queue only together with the connection itself.
  That contract lives in `coordination::deliver_or_disconnect` and is covered by
  paused-clock unit tests plus the real-socket suite in
  `tests/relay_backpressure_e2e.rs` — and, at the design level, by the dedicated
  `tla/DeliveryContract.tla` model (`DeliveryContract_Small.cfg`): a bounded queue, an
  unfair consumer, and a nondeterministic grace expiry, checked for message
  conservation, no-silent-loss, first-close-reason-wins, close preemption against a
  wedged writer, and bounded sender blocking. Its `SilentDropBug` constant reintroduces
  the pre-#131 drop and makes TLC exhibit the `Conservation` counterexample, so the
  invariant is demonstrably non-vacuous. What remains unmodeled is only the
  _quantitative_ side (actual rates, timeout durations, queue sizing).
- **`Signal` payload relay** — `handle_signal` is transport-only plumbing over opaque
  payloads (deliberately weaker than the session predicate, see
  `src/server/signaling.rs`); its gates are direct conditionals with no state evolution,
  exhaustively covered by `signaling_tests.rs` and the wire/fuzz property suites.
- **Reconnection tokens / auth** — orthogonal subsystems; a reconnect is modeled as
  depart-then-join of the same player. One real difference is deliberately not
  captured: a reconnect restores the disconnect-time `PlayerInfo` snapshot with the
  _original_ `connected_at` (`src/server/reconnection_service.rs`), so the reconnector
  re-enters the host-election order at its original position — and can have its
  previously held authority auto-restored while unheld — whereas the modeled
  depart-then-join re-enters at the end of the join order and re-acquires authority
  only through `GrantAuthority`. Both differences change only _which_ capable member
  election picks, and no checked invariant or property depends on the elected identity
  — each requires only that the elected host is a current, session-capable member — so
  every checked claim transfers.
- **Authority release and reconnect auto-restore** — `request_room_authority`'s release
  arm (`src/database/mod.rs`) and the reconnect path's authority auto-restore are not
  modeled as actions; they are coverage-equivalent because every (membership, authority)
  input the replan logic can observe is already reachable through the timing of the
  modeled grant-while-unheld action (releasing and re-granting is the same as granting
  later, or to the other member, in some behavior).
- **TURN minting / ICE lists** — per-recipient data that never feeds back into the state
  machine; covered by deterministic-HMAC property tests
  (`src/security/turn_credentials.rs`, `tests/v3_wire_properties.rs`).
- **Multi-room state** — all session state is keyed per room and rooms do not interact;
  a single-room model loses nothing.
- **Storage errors and capability-downgrading reconnects** — the code self-heals a
  wedged host entry left by a transient storage error or a reconnect that shrank the
  host's negotiated capabilities (`host_invalid`'s broader gate, checked on _every_
  membership-touching event). With reliable atomic actions and constant per-player
  capabilities, those wedge states are unreachable in the model, so the late-join heal
  arm is modeled (it mirrors the code structure) but never fires. The defensive arms
  remain covered by Rust unit tests (`session_policy_tests.rs`,
  `signaling_tests.rs`).
- **`joined_at` ties** — model join times strictly increase (sequence order), so the
  smaller-UUID election tie-break is structurally dead in the model; it is covered by
  the `elect_host` property tests.
- **`player_name` / `is_authority` peer fields** — informational wire payload fields
  that never drive pairing or validation.

## See also

- [`docs/architecture/formal-verification.md`](../docs/architecture/formal-verification.md)
  — how this layer fits with the property-test and fuzz-hardening layers.
- [`docs/adr/0003-formal-verification-and-fuzzing.md`](../docs/adr/0003-formal-verification-and-fuzzing.md)
  — the decision record (why TLC + proptest, why not cargo-fuzz/SMT/kani/loom today).
- [`docs/architecture/handoff-and-topologies.md`](../docs/architecture/handoff-and-topologies.md)
  — the documented protocol contract this spec checks.
