# Signal Fish Server Plan

This file is a forward-only work queue. It contains only future, incomplete, or
actively collecting work. When an item is fully complete, remove it here;
completion evidence belongs in source, tests, durable documentation, GitHub,
and the ignored `progress/` session notes rather than being duplicated in this
plan.

## Goal and ordering

Advance the server toward production-ready cross-platform signaling and relay
operation. Prioritize observed gameplay correctness first, then usability and
operability, then measured performance. Prefer the smallest change that closes
a demonstrated failure class, with red-first tests and evidence proportionate
to risk.

Highest priority: complete the correctness-first ARM capacity campaign below.
It takes precedence over external acceptance and the unscheduled frontier.
Confirmed player-impacting defects take precedence over capacity tooling and
performance experiments within the campaign.

## Execution rules

- Keep one session's work in one pull request; do not stack pull requests.
- Time-box each session to roughly one hour of active work (see GOAL.md).
  Scope the session to one milestone and one PR. Reserve the last 15 minutes
  for validation and handoff. If CI or review is pending at the hour mark,
  record the exact PR state and continue it next session. Carry other work
  forward here; do not extend the session for a new milestone.
- Start production fixes with a deterministic failing test and sweep adjacent
  paths for the same failure class.
- Run the mandatory local Rust sequence and repository gauntlet before
  publication. The exact pull-request head must finish with all applicable
  hosted checks green and no unresolved substantive review feedback.
- Do not infer hosted acceptance from reruns or hand-picked survivors. Count
  every eligible schedule-triggered first attempt according to the cohort's
  pre-registered rules.
- Open a focused GitHub issue for newly discovered work that cannot be closed
  completely in the current change.

## Highest priority — Correctness-first ARM capacity campaign

Owner-approved direction, 2026-09-27. Extend
[#636](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/636).
Maximize sustainable lobbies and relay sessions on one small ARM node.
Correctness is a hard constraint, not a throughput tradeoff.

### Agreed targets and boundaries

- Use a provisional ARM baseline of two vCPUs and 4 GiB RAM. Pin the server
  to two logical CPUs and apply a two-CPU quota and a 4-GiB memory limit.
  Record the effective limits and swap policy. Keep load generators outside
  those limits, preferably on separate hosts.
- Define usable relay capacity by p99 latency at or below 50 ms, from
  scheduled application send to recipient receipt. Report idle lobby density,
  churn capacity, active relay capacity, and mixed-load capacity separately.
- Preserve existing defaults, wire formats, delivery guarantees, and client
  compatibility. Opt-in deployment presets and negotiated protocol additions
  are allowed, but require their own evidence-backed design and tests.
- Audit all production subsystems and shipped reference clients. Keep TURN
  operation, external directory routing, and cross-node room replication in
  their separate workstreams. Do not silently move server costs to clients.
- No particular AWS SKU is selected. Constrained ARM results are provisional;
  actual AWS hardware and network validation are required before deployment
  capacity claims. This campaign does not authorize provisioning paid hosts.
- The planning exploration found measurement gaps and audit candidates, not
  confirmed new defects. Do not report hypotheses as bugs or prior benchmark
  results as fresh measurements.

### Priority and task order

Execute C0 first. Start the audit in C1 next, with C2 as the first capacity
tooling task. C1 continues across all later phases. Any confirmed correctness
defect interrupts C2-C5 until its reproduction and fix are handled. Run C3 only
after C2's measurement controls pass. Run C4 only from C3 profiles. C5 validates
the selected operating point and closes the audit coverage record.

Keep each session to one focused PR. Split large tasks into numbered subtasks
under the campaign issue. Do not combine unrelated fixes or a protocol change
with the measurement harness. If the first session cannot safely include both
the ledger and harness foundation, land the ledger and runner specification
first, then implement the runner in the next PR.

### C0 — Audit ledger and experiment contract (complete)

Use the [audit ledger](docs/development/arm-capacity-audit.md) for coverage,
historical evidence, finding records, and experiment registration. Its C1 and
C2 contracts define the next audit slice and first runner PR. Inventory rows
remain unreviewed until C1 supplies evidence.

### C1 — Audit and fix player-visible correctness ([first slice #647](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/647))

Work through these slices in order. Within each slice, prioritize unauthorized
access, misrouting, lost reliable data, stuck players, and unbounded resources.

- [ ] **Identity and membership:** concurrent joins at room/app limits;
  join-only behavior; leave/disconnect races; stale socket cleanup after
  reconnect; simultaneous claims of one token; expiry during a claim; failed
  restore and retry; token rotation; spectator transitions; kick/ban races;
  application isolation; authentication and allowlist/key reload boundaries.
  The #686 lifecycle-capture-point sweep is complete (ARM-C030, issue
  closed): every capture-check-await site is fenced or has a documented
  exclusion, and the #697 close-pin residual is fixed. The failed-restore
  and retry review is complete (audit ledger, 2026-09-30): every
  post-claim reconnect rejection is pinned or excluded, with degraded
  authority restoration recorded as intentional with a client-driven
  recovery path. The token rotation boundary review is complete (audit
  ledger, 2026-09-30): rotation ordering, concurrent-claim refusals, both
  post-rotation unwinding phases, and the post-rotation reject-path discard
  (drain-flip pin, #707) are pinned. The allowlist/key reload boundary review
  is complete (audit ledger, 2026-10-01): the SIGHUP glue applies nothing on
  a rejected config (validation gates both swaps synchronously), a valid
  reload applies the allowlist and key swaps together, the posture-before-key
  install order keeps every mixed state fail-closed, and both outcomes
  (rejected and valid reloads) are pinned through the glue. The identity
  slice is complete (identity-slice completion review, audit ledger,
  2026-10-01): spectator transitions (leave/disconnect detach race pinned in
  both orders, red-proofed; detach idempotency and the #241 TOCTOU fence
  verified), kick/ban races (gate-ordered ban vs claim, tombstone fences,
  lifecycle-gate revalidation — pinned or derived), and application
  isolation (owner gates on every admission perimeter, pinned) each carry a
  recorded disposition.
- [ ] **Gameplay transitions:** ready-state invalidation on membership change;
  start/leave races; authority election and loss; host/direct readiness;
  v2/v3 negotiation; transport capability intersections; stale transport
  reports (ARM-C012 fixes status delivery after leave/rejoin); reconnect with
  changed encoding or capabilities; publication order
  of room snapshots, session plans, and gameplay events. The
  gameplay-transitions review is complete (audit ledger, 2026-10-01): every
  case family carries a recorded disposition, and the start path's
  room-assignment trust is now pinned (a spectator's `StartGame` refuses
  `NOT_IN_ROOM`; `spectator-mode.md` lists `StartGame` as
  spectator-forbidden).
- [ ] **Delivery:** reliable ordering and duplicate handling; reconnect epoch
  and sequence changes; latest coalescing keys and generations; permitted
  volatile loss; exact gap/report accounting; mixed encodings and unsupported
  conversions; serialization size limits; priority-control saturation;
  slow-recipient isolation; healthy-room progress during another room's stall.
  The #713 duplicate-delivery report is dispositioned (audit ledger,
  2026-10-01): the suspected lane lease re-run does not exist, the doubling
  was not reproducible in any arrangement, and room-uniform lobby
  exactly-once under interleaved awaits — including across a parked
  auto-advance window — is pinned
  (`interleaved_awaits_deliver_each_lobby_broadcast_exactly_once`).
  The reconnect epoch/sequence family is reviewed (audit ledger, 2026-10-01):
  the resumed epoch is part of reassignment with no provisional value
  observable, recipient-visible `(epoch, seq)` strictly increases across
  reconnect/rejoin/room-switch, the stale-sender dispatch is lifecycle-gate
  fenced (#686), and saturation is loud — with cross-epoch exact gap
  accounting now pinned (`cross_epoch_gaps_of_one_sender_stay_distinct_ranges`;
  red-proofed by dropping the merge rule's epoch-equality clause).
  The slow-recipient isolation and cross-room stall fairness families are
  reviewed (audit ledger, 2026-10-01): no shared seam is held across a
  stalled recipient's delivery wait, and the cross-room invariant — room B's
  relay plane progresses through room A's full slow-consumer window with no
  inter-frame gap approaching it, a mid-stall join's lifecycle broadcast is
  delivered, and the eviction tears down exactly one connection with no
  leakage — is pinned (`stalled_room_does_not_strand_a_healthy_room`;
  red-proofed with a shared-gate probe).
  The latest coalescing keys/generations family is reviewed (audit ledger,
  2026-10-01): the full `(from_player, room_id, key)` composition isolates
  streams — key-value isolation now pinned
  (`latest_supersede_requires_the_matching_key_value`, red-proofed with a
  constant-key probe) beside the existing dispatch class/key gate,
  owner/room, generation-shielding, supersession-report, saturation,
  window, and counter-conservation pins.
  The mixed encodings/unsupported conversions family is reviewed (audit
  ledger, 2026-10-02): negotiation downgrades an unsupported request to
  JSON, MessagePack falls back losslessly, opaque sources are refused to
  every other format with an exact v3 gap plus a rate-limited advisory, and
  pre-v3 recipients of opaque payloads get the advisory without any
  v3-only report — now pinned
  (`v2_recipients_of_opaque_payloads_get_advisories_without_v3_reports`;
  red-proofed with a lossy-fabrication probe).
  The permitted volatile loss and exact gap/report accounting families are
  reviewed (audit ledger, 2026-10-02): every lossy drop path emits a causal
  exact gap or fails the connection closed, the observable stream stays a
  gap-free prefix plus exact reports, and the two composed-path gaps (no
  real-socket volatile eviction, no non-zero per-connection
  `dropped_for_you`) are now pinned
  (`flooded_nonreading_recipient_observes_exact_volatile_gaps_and_dropped_for_you`;
  red-proofed by suppressing the eviction's causal gap report).
  The protocol subsystem coverage row is reviewed (audit ledger,
  2026-10-05): wire bytes, delivery-class carry, depth/size symmetry, and
  fail-closed enum handling match the contract on every decode path; the
  previously undocumented decode behaviors (duplicate-member precedence
  and out-of-i64/u64 integer-literal fidelity on the plain JSON lane) are
  now pinned with recorded dispositions.
- [ ] **Recovery:** cancellation at relevant await boundaries; partial state
  mutation or publication; rollback failure and retry; task panic recovery;
  cleanup racing join/reconnect; deadlines at before/equal/after boundaries;
  wall-clock changes versus monotonic expiry; drain/shutdown with queued data
  and active reconnect claims; process-loss behavior versus documented limits.
  The transaction reservation/commit cancellation and panic seam is reviewed
  (audit ledger, 2026-10-02): structured cancellation at every pre-hook await
  was already explicit, the silent panic-accounting class (hook and phase
  callback panics released reserved frames without cancellation accounting)
  is fixed with drop-accounting guards on both publication paths, and each of
  the three fixed panic seams (batch commit hook, phase callback, broadcast
  replay hook) is pinned (no frame past the panic, exact once accounting,
  capacity release). The panic-recovery shape (lane job isolation plus the
  caller's publication-failure arm and the reconnect baseline) carries a
  recorded disposition. The deadlines and wall-clock-versus-monotonic family
  is reviewed (audit ledger, 2026-10-02): the activity-reaper pair flips once
  at the strict `ping_timeout` boundary and is now pinned
  (`activity_reaper_expiry_flips_once_at_the_ping_timeout_boundary`;
  red-proofed by flipping both comparisons), the zero-timeout reaper disable
  and every sibling expiry predicate carry a pinned or inspected disposition,
  and wall-clock steps cannot open or close any monotonic deadline (reconnect
  window, room GC, dashboard staleness, drain waits). The drain/shutdown with
  queued data and active reconnect claims family is reviewed (audit ledger,
  2026-10-03): the reconnect-commit fence guarantees every committed claim
  reaches the coded 4000 close fan-out, close ordering flushes queued frames
  before the close frame with abandoned remainders counted, terminal
  reconnection teardown is pinned, and the two drain-accounting defects found
  (a parked-baseline drain flip counted as a dropped message; silent releases
  on baseline build failure and gate refusal) are fixed; four red-proven pins
  and two no-double-count/regression pins cover the seams.
- [ ] **Resource and input safety:** queue and replay bounds; inactive records;
  pending detach/claim retention; task ownership; metrics label cardinality;
  parser depth, size, malformed frames, Unicode, and numeric boundaries;
  unauthenticated floods; rate-limit rejection accounting; configuration
  validation and reload consistency; error and logging paths under pressure.
  The unauthenticated-admission boundary review is complete (audit ledger,
  2026-10-03): flood posture verified (budget charged before credential
  verification, refusal closes the socket, log-safety gate and app-ID cap
  ahead of every policy path), the absolute activity-immune auth deadline
  pinned (`pre_handshake_activity_does_not_extend_the_auth_deadline`;
  red-proofed with a sliding-deadline probe), and concurrent ceiling
  conservation with exact rejection accounting pinned
  (`concurrent_handshakes_conserve_the_app_ceiling_and_count_every_rejection`;
  red-proofed by disabling app-window enforcement). The Authentication
  ledger row is reviewed. The metrics label cardinality family is reviewed
  (audit ledger, 2026-10-03): every label surface is bounded —
  connection-scoped ledgers die with their registration, per-app relay
  attribution is allowlist-bounded with #552 pruning, client-chosen
  game-name maps are response-bounded — and the shutdown-drain close
  fan-out is exported as `websocket_shutdown_disconnects` (#727).
  The parser-boundary families are reviewed (audit ledger, 2026-10-03): every
ingress parser carries a bounded or flat decode — the confirmed exception
(rmp-serde recursion into a recursive target) is fixed as ARM-C030 with an
iterative depth scanner shared by the JSON conversion path and the
token-bound binary envelope, red-proven by a stack-overflow abort and pinned
at the exact 128-level boundary. The inactive-record, pending detach/claim
retention, and task-ownership families are reviewed (audit ledger,
2026-10-04): every audited map and task carries a verified removal or abort
on all exit paths, the pre-issued-token teardown-leak hypothesis was
falsified by the layered room-removal and maintenance discards, and the
owned-task supervisor-panic strand residual (#738) is fixed (ARM-C037): the
reconnect transaction's in-task unwind supervisor is itself
`catch_unwind`-guarded with a commit-state-aware record fallback — a
delivered terminal consumes the record, an uncommitted one is released for
retry — red-proven and pinned. The
rate-limit rejection accounting family is reviewed (audit ledger,
2026-10-04): every refusal path charges exactly once and every counter
lands on the refusing budget; the drain-window creation refusal's
deliberate budget-free shape (bounded by the charged error-reply gate) is
now pinned. The error and logging under pressure family is reviewed (audit
ledger, 2026-10-04): the rejected app-ID log forgery is fixed (ARM-C031,
Debug-escaped), the unthrottled per-recipient undeliverable warning now
rides the advisory cadence (ARM-C032), and reply amplification, log
content, and volume carry pinned or dispositioned bounds. The
configuration-validation family is reviewed (C1 config and reload coverage
review, audit ledger, 2026-10-04): defaults pass every guard once the
documented metrics-credential gate is satisfied, malformed-document and
env-override breadth is pinned, and five fixed silent-revert / fail-open /
crash classes (ARM-C038..ARM-C042) plus the per-IP default-roster coherence
fix (ARM-C043) are red-proven. The resource-and-input-safety slice is
complete.
- [ ] **Client and deployment boundaries:** inspect reference-client handling
  of reconnect, reports, transport fallback, and negotiation. Audit plain/TLS
  server paths and optional features, including `legacy-fullmesh`. Distinguish
  code/test evidence from untested external mobile, Steam, and TURN support.
  The review is complete (audit ledger, 2026-10-04): the reference clients'
  downgrade abort (ARM-C033) and the opaque-over-v2 seam are fixed with pins,
  report/fallback/negotiation families are dispositioned per client, the
  listener's no-half-started-server order and the `legacy-fullmesh`
  separation are verified, and the TLS-drain and startup-failure coverage
  gaps are closed over the real binary (`tests/tls_deployment_boundaries_e2e.rs`,
  ARM-C035/ARM-C036, issue #740). Client rows are partially reviewed:
  reconnect initiation stays out of scope by documentation, with the
  restore-contract exercisability and fault-injection cells tracked as
  follow-ups (#741).

For each finding:

1. Write a minimal deterministic failing test against the production seam.
   Save the failing command and output. Use paused time, explicit barriers,
   injected failures, and fixed seeds where applicable.
2. Fix the smallest failure class. Sweep sibling entry points and rollback
   paths. Add positive, negative, boundary, recovery, and concurrency cases
   needed to prove the invariant without mirroring the implementation.
3. Extend existing property tests, fuzz targets, or formal models when the
   state space warrants it. Connect model counterexamples to executable
   production regressions; a model alone does not prove the implementation.
4. Run scoped red-green checks, then the relevant hosted acceptance gates.
   Update the ledger with evidence or a specific unresolved issue.

Acceptance: each reviewed slice has a recorded disposition and reproducible
evidence. Never label a suspected defect confirmed without a reproduction or
equivalent direct proof. Do not weaken correctness checks to improve capacity.

### C2 — Build a standalone, delivery-aware capacity runner ([#648](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/648))

The runner foundation is landed (`tests/capacity_runner/`, first runner PR,
2026-10-04): full contract input set with `CAPACITY_RUNNER_*` standalone
entry, spawn-or-connect to the real binary on a separate process, scheduled
sends independent of response completion under a generator-lag bound, one
monotonic run epoch for send/receipt pairs, versioned artifacts
(manifest with binary/config hashes, tagged event log, interval server
resource samples, latency histogram, summary), exact replay (`replay ==
summary`), the passing small reliable scenario, and every registered negative
control invalidating with an explicit reason (missing, duplicate, misrouted,
out-of-order, paused generator, saturation, server termination, slow reader
with server-counter accounting). The latest/volatile slice is landed
(second runner PR, 2026-10-05): permitted loss only with exact gap
accounting (class-legal reasons, no overlaps, no out-of-range coverage),
key isolation pinned, per-recipient latency tails in the summary, the run
class's accountable server outcomes in every interval sample, and two
deterministic pressure cells in exact server-counter agreement. The
reconnect-burst slice is landed (third runner PR, 2026-10-05): the C3
reconnect-storm shape with per-incarnation stream validation across
rejoins (runner-owned incarnation indices resolved per `PlayerId` through
a sender registry, rejoin snapshot tails as owed floors, stale-epoch and
below-tail misroute detection, storm-execution enforcement), schema-3
artifacts, a real-socket 50% storm cell with replay equality, and
deterministic controls for each new permitted outcome. The
room-replacement slice is landed (fourth runner PR, 2026-10-05): whole
rooms cycle into fresh room-code generations per wave (the C3 churn
cell), the member roster per room never changes so the oracle's static
co-room checks and per-incarnation stream machinery validate unchanged,
the multi-wave shift composes on the original timeline, and two
real-socket cells plus deterministic controls (plan shape, composed
shift, code uniqueness, missing rejoin half, stale-generation misroute)
carry red proofs. The unsupported-format slice is landed (seventh runner
PR, 2026-10-06, see the ledger's contract record): a labeled
`unsupported-format` contract experiment where the room's opaque `rkyv`
sender reaches no cross-format recipient — exact `unsupported_format`
coverage, zero payload leaks, bounded advisories, exact server-counter
agreement — with six red-proven controls. Remaining below: the
resource-collection remainder.

- [x] Churn/reconnect schedules: reconnect-burst and room-replacement
  shapes per the C3 cells, with red-first controls for each new permitted
  outcome. The reconnect-burst slice is landed (third runner PR,
  2026-10-05, see the ledger's contract record): the storm shape, per-
  incarnation stream validation, and the storm cell plus controls. The
  room-replacement shape is landed (fourth runner PR, 2026-10-05): whole
  rooms cycle into fresh generations per wave while others keep serving.
- [x] Unsupported-format cells: validate permitted
  outcomes and reports per the delivery contract instead of reliable
  semantics; label them as separate contract experiments. Landed (seventh
  runner PR, 2026-10-06): the labeled `unsupported-format` experiment —
  the room's opaque `rkyv` sender must reach no cross-format recipient,
  every omission covered by an exact `unsupported_format` gap report, the
  rate-limited advisory cadence bounded, no payload leak, the gap contract
  closed on text streams, the server's `unsupported_format` counter in
  exact agreement, and the labeled artifacts replaying (schema 5). The
  remaining unsupported-format ground (v2 observer cohorts, same-format
  opaque twins, churn × experiment composition) is recorded in the ledger
  for the C3 encoding-mix cells rather than silently skipped.
- [ ] Extend runner inputs where a new cell needs them (encoding mix cohorts,
  churn schedule shapes). Current inputs: endpoint, seed, room/player count,
  encoding (v2/v3 JSON), payload bytes, per-sender rate, delivery class
  (with `latest_keys_per_sender`), warm-up, duration, churn/reconnect
  schedule, output directory. MessagePack
  cohorts and mixed-format cohorts ride the existing `Encoding` input.
- [x] Emit a run manifest, interval measurements, latency histograms, exact
  outcome summary, and diagnostic logs as machine-readable artifacts, with
  schema version, run ID, run-scoped room-code prefix, toolchain, features,
  config-overlay and server-binary hashes, arch/kernel, resource samples
  (server delivery counters, server RSS, cgroup memory, generator RSS), and
  the clock method. Unavailable counters are recorded (as null or an explicit
  scrape-error sample), never omitted. CPU model, resource limits, and the
  network path are environment facts a capacity host records around the run
  (the audit experiment contract), not things the generator can know.
- [x] Schedule offered traffic independently of response completion. Record
  intended send time, actual send time, receipt time, and generator lag per
  delivery; bound the generator with the lag bound and mark saturation as an
  invalid measurement (explicit reason with worst lag and unsent work) —
  never silently drop scheduled work.
- [x] Prefer sender and receiver tasks sharing one monotonic clock for
  latency pairs (one run epoch in one process). Distributed generators are
  out of scope for this runner; single-host same-clock pairs are the only
  numbers compared to the 50-ms target.
- [x] Track deliveries by run/room/sender/sequence/recipient and check
  missing, duplicate, unexpected/misrouted, and out-of-order outcomes
  against the delivery contract. Unfinished reliable work is a failure
  (outstanding/unsent, with unsent invalidating when no declared fault
  explains it), not an omitted latency sample. Latest/volatile permitted
  outcomes are pinned (exact gap coverage; the class slice above).
- [ ] Collect CPU, RSS, cgroup memory, available socket-memory accounting,
  ingress/egress bytes, queue depth/age, disconnect reasons, live objects,
  cleanup backlog, maintenance duration, and generator CPU. Landed: server
  delivery counters, slow-consumer disconnects, active connections, server
  RSS, server and generator CPU time (the schema-4 interval CPU pair),
  cgroup memory, generator RSS, disconnect reasons (recorded as
  events), and — on lossy-class runs — the run class's seven accountable
  per-class outcomes, and the byte pair (ingress
  `signal_fish_relay_bytes_total`, egress
  `signal_fish_websocket_egress_bytes_total`, maintained on the write path)
  plus the queue posture (`signal_fish_websocket_queue_depth`,
  `signal_fish_websocket_queue_oldest_age_milliseconds`, computed at
  scrape time), with scrape failures recorded as explicit samples.
  Remaining: socket-memory accounting,
  live objects, cleanup backlog, and maintenance duration. Keep
  instrumentation out of timed hot paths.
- [x] Runner negative controls: deliberately missing/duplicate/misrouted
  deliveries, delayed sends (pause appears in scheduled-send latency instead
  of reducing offered load), slow readers, generator saturation, and server
  termination must fail or invalidate the result as appropriate — all seven
  are pinned.

Acceptance: a small real-socket scenario passes, each negative control is
detected, artifacts replay the result, and generator limits are
distinguishable from server saturation. No production API change is required
for this phase. The scenario, replay-equality, and all controls are pinned in
`tests/capacity_runner/`; acceptance for the remaining slices rides the same
target.

### C3 — Measure capacity curves and resource costs

- [ ] Use 2-, 8-, and 16-player rooms, 96-byte and 1-KiB application payloads,
  and 30/60 messages per player per second for relay cells. Record encoded
  sizes and recipient deliveries, not just ingress message counts.
- [ ] Cover v2/v3 JSON, v3 MessagePack, homogeneous supported opaque formats,
  and the existing mixed-format benchmark cohorts. Use reliable delivery for
  the primary capacity ceiling. Run latest/volatile and intentionally
  unsupported conversion cases as separately labeled contract experiments.
- [ ] Measure idle lobbies, sustained room creation/join/leave, reconnect
  bursts, active relays, and relays plus idle rooms. For mixed loads, test
  1:1 and 1:10 active-to-idle room ratios. For churn, replace 10% of rooms
  per minute; for reconnect storms, disconnect and restore 10%, then 50%,
  of clients over ten seconds. Keep seeds and schedules fixed across pairs.
- [ ] Test plain and TLS configurations separately. Record TLS termination
  and network latency; do not subtract network delay from the agreed target.
  Keep production defaults except explicit resource/admission limits needed
  by a cell, and record those overrides. Distinguish policy refusal from
  physical saturation.
- [ ] Increase room counts geometrically until a failure boundary, then
  refine within 10%. For churn, independently vary operations per second at
  a fixed occupancy. Report a curve per workload rather than one universal
  room limit.
- [ ] Use five independent runs per accepted point, each with a two-minute
  warm-up and ten-minute measurement. Retain all attempts. Predefine generator
  invalidation criteria; report invalid runs and replacements rather than
  selecting only favorable trials.
- [ ] Accept a relay point only when all five runs meet p99 <= 50 ms, complete
  the offered workload, remain inside memory limits, and have no unexplained
  loss, duplicates, misrouting, or disconnects. Report p50/p95/p99/max and
  per-client tails so aggregates cannot hide a starved room.
- [ ] For idle/churn points, require exact lifecycle outcomes, bounded state,
  and successful heartbeat/control progress. Report their latency separately;
  do not present the relay SLO as a measured lobby-operation guarantee.
- [ ] Continue beyond the accepted boundary in separate overload runs. Check
  bounded memory, explicit refusals/closures, healthy-room fairness, and
  recovery after offered load falls. Overload delivery loss must follow the
  contract, even though overload points do not count as usable capacity.
- [ ] Measure bytes per idle connection/room and retained-state growth across
  occupancy levels. Profile near the knee for CPU, lock contention, task
  scheduling, allocation rate, syscalls, serialization, and maintenance work.

Acceptance: reproducible capacity curves, a failure boundary, resource cost
slopes, and a ranked bottleneck list. Mark generator-limited or network-limited
curves explicitly. Do not extrapolate microbenchmark throughput to node capacity.

### C4 — Run one controlled optimization experiment at a time

Rank by measured whole-node cost and expected player benefit. The initial
investigation order is provisional:

1. **Maintenance and memory:** repeated room/connection scans, replay and
   reconnect retention, pending cleanup, idle objects, and dashboard refresh.
   Establish complexity versus room count before proposing indexes, bounded
   sweep batches, or incremental cleanup. Preserve race and expiry guarantees.
2. **Scheduling and contention:** room mutation locks, long fan-out loops,
   wakeups, ready-task fairness, and cross-room interference. Test bounded work
   or yielding only when profiles show a scheduling problem.
3. **Encoding and I/O:** ingress parsing, output serialization, copies,
   WebSocket writes, socket buffers, batching, and TLS. Preserve control
   priority and reliable/volatile latency semantics.
4. **Targeted alternatives:** SIMD JSON, allocator changes, SmallVec, static
   lookup/perfect hashing, and ARM compiler tuning. Apply them only to measured
   costs. Keep untrusted-key collision resistance and artifact portability.

- [ ] Separate JSON parse and serialization benchmarks. Include required input
  copies and conversions in end-to-end costs. SIMD parsing speed alone is not
  evidence of an outbound serialization win.
- [ ] Differential-test any JSON replacement against current behavior:
  accepted/rejected inputs, duplicate keys, numeric limits and representation,
  negative zero, Unicode/escapes, nesting/size limits, errors, and exact output
  bytes. Review dependency `unsafe`, MSRV, licensing, and target support.
- [ ] Alternate baseline/candidate order over at least five paired independent
  runs on the same environment. Keep correctness oracles enabled. Report raw
  values, paired changes, and uncertainty; reject results inside run noise.
- [ ] Land a performance change only with at least 5% reproducible improvement
  in the registered capacity or resource metric. Require no new correctness
  failures or SLO failures. Treat a repeatable >5% regression in a secondary
  latency, capacity, or memory metric as material and reject or redesign it.
  Correctness fixes do not need to meet the performance-improvement threshold.
- [ ] For opt-in features, specify the negotiation/configuration interface,
  unchanged default path, old/new client matrix, failure behavior, and rollback
  before implementation. Evaluate the feature on an explicitly labeled curve.
  Do not force room cohort homogeneity or change directory ownership implicitly.
- [ ] Preserve failed hypotheses and negative results in the experiment ledger.
  Do not repeat queue-allocation, cached-frame-clone, or estimate-prepass work
  without evidence that the current implementation has a new bottleneck.

Acceptance: each experiment ends in an evidence-backed land, reject, or defer
decision. A microbenchmark-only gain remains provisional until measured in
the relevant standalone workload.

### C5 — Validate sustained operation and close the coverage gaps

- [ ] Select an operating point at 80% of the measured passing room/load
  capacity. Run a one-hour soak with churn and reconnect bursts. Require the
  same delivery and latency checks, no OOM, and recovery after disturbances.
- [ ] After churn stops, wait through configured expiry and cleanup windows.
  Verify live room/player/claim/replay/task counts return to expected levels.
  Compare repeated churn cycles for growth. Distinguish allocator-retained
  pages from live-state leaks; RSS need not return exactly to startup.
- [ ] Revalidate on the intended AWS SKU when hardware access is available.
  Record CPU credits and credit mode for burstable instances, sustained
  network limits, TLS path, kernel, and process limits. Separate credit-backed
  burst results from sustainable baseline capacity and any paid surplus use.
- [ ] Run applicable hosted feature, interoperability, fuzz, mutation, and
  formal checks for the final revision. Keep expensive suites in hosted CI;
  local iteration uses owning-target tests and scoped clippy. Run required
  formatting/lint gates before each publication.
- [ ] Publish the workload-specific capacity envelope, operating headroom,
  exact configuration, remaining bottlenecks, known failures, and rollback
  instructions in durable documentation. Link evidence from the campaign.
- [ ] Close the audit only when every subsystem has a disposition and every
  confirmed defect is fixed or tracked with severity, reproduction, and
  mitigation. Do not claim bug freedom or untested platform support.

Acceptance: sustained measurements support the recommended operating point;
the audit has no unreviewed production subsystem; unresolved work is explicit.
If AWS validation is unavailable, complete the portable work and leave only
that external validation item open. Do not claim deployed capacity.

### Evidence and research starting points

- Existing in-repo evidence: relay allocation/runtime benches,
  `tests/model_based_state_machines.rs`, `fuzz/fuzz_targets/`, `formal/tla/`,
  real-socket delivery tests, and `docs/architecture/scaling.md`.
- Read current #636/#207 discussion before reusing a historical candidate.
  The shared-body JSON splice already removed duplicate mixed-room JSON body
  serialization. Re-measure current code rather than applying obsolete gains.
- [HDR Histogram](https://github.com/HdrHistogram/HdrHistogram): scheduled-load
  latency and coordinated omission. Retain actual unsent work in the runner;
  histogram correction alone cannot repair an overloaded generator.
- [Tokio cooperative scheduling](https://tokio.rs/blog/2020-04-preemption):
  long CPU loops still require bounded work; measure before adding yields.
- [simd-json](https://github.com/simd-lite/simd-json): ARM NEON support and
  compatibility considerations. A library's benchmark is not this workload.
- [AWS CPU credits](https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/burstable-credits-baseline-concepts.html):
  distinguish burst performance from sustainable small-instance capacity.

## External acceptance

### P7 — Mobile and Steam interoperability

- Run the documented v3 interoperability matrix with maintained out-of-repo
  mobile and Steam builds, including live signaling, reliable and unreliable
  data channels, relay fallback, and reconnect behavior.
- Record exact client revisions, platform/WebRTC stacks, and evidence for each
  supported platform cell.

Acceptance: mobile and Steam rows have reproducible green cross-stack evidence;
documentation clearly distinguishes demonstrated support from integration
guidance until then.

### P8 — Operated self-hosted TURN

- Provision and operate the documented self-hosted coturn deployment with TLS,
  ephemeral credentials, rotation, monitoring, and capacity evidence.
- Validate relay-only browser/native and external-platform sessions against the
  operated service without weakening credential or candidate-path assertions.

Acceptance: a maintained environment demonstrates reproducible relay-only
sessions and operational secret rotation. Multi-node room-spanning fan-out is
outside this plan's architecture scope.

## Unscheduled open-issue frontier

These items remain live but are not active phases. Re-rank them whenever new
correctness evidence appears.

- #733 — the scheduled ASan lane went red on 2026-10-03 (`54feb610`, exit
  101). Root-caused via the run artifact: crates.io transport flakiness
  tripped the zero-diagnostics packaging pin — zero sanitizer findings.
  The recovered-transport warning class is now tolerated in both packaging
  filters (PR #734); the post-merge watch is complete: the scheduled
  Advanced Safety run on main (2026-10-05, head `8f80cc6e`) is green. The
  npm-side red is fixed by the #732 gate. CLOSED.
- #732 — GHSA-vfj7-8cjw-p6xm (braces) is unfixable upstream; the npm audit
  gate accepts it per-graph with a 2026-12-01 revisit date. Drop the entry
  when a patched braces release exists.
- #636 — promoted to the highest-priority correctness-first ARM capacity
  campaign above. Its C0-C5 tasks own the active audit and optimization queue.
- #512 — hosted CI: the session-239 audit found every per-event workflow
  path-narrowed, cache-warmed, and cohort-consolidated. Remaining levers
  need owner input: self-hosted runner labels; the interop quartet stays
  per-PR per #568; a cargo-deny single-container consolidation is blocked
  by the pinned action's one-manifest-per-boot input and the fortress-wasm
  1.94 toolchain pin. Local loop: sessions 262-266 landed the cheap levers;
  session 267 measured the remaining floor dead ends (mold: no gain, link
  is 1.2 s of ~10.5 s; dev-loop resolution: 0.3 s; nightly `-Zthreads`:
  slower than stable) and shrank the last 1 s test classifier window
  (full `--lib` wall 4.19 s -> 3.44 s). Session 269 swept the last
  real-time negative-wait family the floor audit named (#642's "known
  remainder"): the 100 ms `assert_silent` windows and the drain/GC race
  sequencing now run on the paused clock (the formerly-slowest
  clock-bound test 0.93 s -> 0.02 s; the suite's remaining slowest test
  is pre-existing CPU-bound metrics work, not a clock wait). Session 270
  swept the last real-time negative-wait windows in the suite — the two
  distributed-lock contention tests now run on the paused clock and
  assert lease start on the monotonic `expires_at` domain (#642 known
  remainder; PR #646).
  The remaining floor is rustc
  crate-size work. The owner green-lit split exploration on #642
  2026-09-27 ("worth exploring"); session 270's spike falsified the
  leaf-first
  split order — extracting protocol/config cannot cut the per-touch
  floor because every downstream crate rebuilds and re-expands on any
  upstream edit (data on #642). The win only exists fragmenting the fat
  server crate itself (downstream-most edits), and any split PR carries
  owner-tier decisions: crates.io publish order (path deps break
  `cargo publish`) and workspace-mode mutation inventory. Sequential
  domain fragmentation or accepting the floor until `-Zthreads` matures
  on stable are the remaining options.
  Session 343 re-measured on the same box: stable 1.98.1 is ~25% slower
  than the pinned 1.91.0 on the warm per-touch `--lib` build (8.2 s vs
  6.4 s), nightly `-Zthreads=4` loses to the same nightly without it
  (9.8 s vs 8.5 s, one target dir; 16 threads already lost in session
  267), the #642 named remainder test already runs on the paused clock
  (0.026 s), and the pre-push discovery walk is single-spawn (#653).
  Session 344 decomposed the per-touch floor: a production-only touch
  rebuilds in 4.5 s while the scoped `--lib` test loop costs 11.1 s — the
  ~6.6 s delta (60%) is the lib's test-cfg unit re-expanding and
  re-typechecking the ~45k lines of inline `#[cfg(test)]` modules on every
  touch. This sharpens the crate-split calculus: because inline tests ride
  their owning crate's test unit, a workspace split shrinks each domain's
  per-touch cost by production AND test mass together (the
  WebSocket+coordination domains total ~31k lines, ~3 s at this crate's
  per-line rate), roughly double the production-only estimate. The floor
  data still favors a split as the only drastic lever; the visibility
  sweep (pub(crate) across future crate boundaries) remains the cost
  driver.
- #207 — pursue the next optimization only from current allocation and latency
  profiles, with exact wire and delivery semantics held constant. The
  2026-09-01 profile found the fan-out core at its floor (0–1 allocation ops
  per relay across room sizes; the classified queue lane at zero) and
  per-relay projection cost proportional to the distinct wire cohorts the
  room's recipient mix requires (4–5 allocs/relay single-encoding; 14–21
  for v2+v3 mixed rooms, with each cohort's frame cached once per relay and
  sibling recipients reusing clones), so no allocation-level target remains at
  these layers without changing wire bytes or delivery semantics. The
  session-212 budget gate reads app policy through a lock-guarded projection
  of `Copy` fields, keeping the charge path allocation-free.
- #396 — CLOSED 2026-09-12 (standing correctness/perf sweep, closed with the
  session-237 enforcement-seam sweep). The sweep practice continues
  opportunistically wherever new features open seams; per-session closure
  evidence lives in the closed issue, session notes, and merged PRs.
- #525 — CLOSED (minimal moderation set, access-control tier, and spectator
  fan-out slimming landed across sessions 217-220; the #546 squat design
  resolved in session 220). Follow-on credential work is tracked under #517.
- #378 — CLOSED (canonical Link Check gate, session 217).
- #517 — the credential story is ratified (owner decisions 2026-09-11:
  no shared secret, 5-minute TTL, self-hosting must keep public-`app_id`
  mode, `connect_token` field name) and this repo's half is implemented:
  `Authenticate` carries an optional `connect_token`
  (`sfct_v1.<b64url(payload)>.<b64url(sig)>`, Ed25519), verified after the
  allowlist resolves in the order encoding → signature → expiry →
  300 s + 60 s skew TTL ceiling → app-id binding, all failures reported as
  `CONNECT_TOKEN_INVALID` on a retryable, budget-charged refusal. Key config
  is `security.connect_token.public_key`/`public_key_path` (public material,
  file folded at load, fail-closed), SIGHUP-reloadable alongside the
  allowlist. Absent field is byte-identical to today; presented token
  without a configured key is refused fail closed. Session 236 implemented
  the #574 enforcement knob (owner sign-off 2026-09-12): layered
  `security.connect_token.required` global default + per-app
  `require_connect_token` override, missing-token refusals report the
  distinct `CONNECT_TOKEN_REQUIRED` (retryable, budget-charged, 4006 on
  exhaustion), posture reloads with the key, and a required entry with no
  key is dead config rejected at startup/SIGHUP. Enforcement in open mode
  also closes the legacy skip-`Authenticate` path (pre-auth frames refused
  `MISSING_APP_ID`, silence hits `4001 auth_timeout`). Cloud-side edge
  enforcement (option 1) and token minting are the control plane's work,
  filed 2026-09-14 as signal-fish-cloud#782 (owner-directed hand-off);
  remaining frontier: SDK mint/attach halves (tracked in the SDK repos).
  Verified safe (session-236 sweep): enforcement × reconnect identity swap
  (handshake guards block re-entry), enforcement × allowlist reload races
  (fail-closed in both swap orders).
