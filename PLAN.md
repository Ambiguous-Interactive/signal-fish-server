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
- [ ] **Recovery:** cancellation at relevant await boundaries; partial state
  mutation or publication; rollback failure and retry; task panic recovery;
  cleanup racing join/reconnect; deadlines at before/equal/after boundaries;
  wall-clock changes versus monotonic expiry; drain/shutdown with queued data
  and active reconnect claims; process-loss behavior versus documented limits.
- [ ] **Resource and input safety:** queue and replay bounds; inactive records;
  pending detach/claim retention; task ownership; metrics label cardinality;
  parser depth, size, malformed frames, Unicode, and numeric boundaries;
  unauthenticated floods; rate-limit rejection accounting; configuration
  validation and reload consistency; error and logging paths under pressure.
- [ ] **Client and deployment boundaries:** inspect reference-client handling
  of reconnect, reports, transport fallback, and negotiation. Audit plain/TLS
  server paths and optional features, including `legacy-fullmesh`. Distinguish
  code/test evidence from untested external mobile, Steam, and TURN support.

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

- [ ] Reuse existing WebSocket clients, multiprocess tests, and delivery
  ledgers. Run the release server as a separate process. The in-process
  `tests/load_tests.rs` smoke tests are not socket-delivery capacity evidence.
  The shared-process experiment in `docs/architecture/scaling.md` is not a
  standalone server ceiling.
- [ ] Define runner inputs for endpoint, workload/seed, room and player counts,
  protocol/encoding mix, payload size, sender rate, delivery class, warm-up,
  duration, churn schedule, and output directory. Keep the runner independent
  of a cloud provider. Do not add production protocol fields for benchmarking.
- [ ] Emit a run manifest, interval measurements, latency histograms, exact
  outcome summary, and diagnostic logs as machine-readable artifacts. Include
  schema version, run ID, SHAs, toolchain, features, binary/config hashes,
  CPU/kernel, resource limits, TLS mode, network path, and generator resources.
- [ ] Schedule offered traffic independently of response completion. Record
  intended send time, actual send time, receipt time, generator lag, unsent
  work, and outstanding deliveries. Bound generator queues and mark saturation
  as an invalid measurement; never silently drop scheduled work.
- [ ] Prefer sender and receiver tasks sharing one generator's monotonic clock
  for latency pairs. For distributed generators, require a recorded clock
  error bound or use same-clock round trips as a separate metric. Do not
  compare unsynchronized timestamps to the 50-ms one-way target.
- [ ] Track deliveries by run/room/sender/sequence/recipient. Check missing,
  duplicate, unexpected, cross-room, and out-of-order outcomes against the
  delivery contract. For latest/volatile traffic and unsupported formats,
  validate permitted outcomes and reports rather than requiring reliable
  delivery. Unfinished work remains a failure, not an omitted latency sample.
- [ ] Collect CPU, RSS, cgroup memory, available socket-memory accounting,
  ingress/egress bytes, queue depth/age, disconnect reasons, live objects,
  cleanup backlog, and maintenance duration. Record unavailable counters.
  Keep instrumentation out of timed hot paths where possible and quantify
  its overhead before using profiled runs for capacity claims.
- [ ] Add runner negative controls: deliberately missing/duplicate/misrouted
  deliveries, delayed sends, slow readers, generator saturation, and server
  termination must fail or invalidate the result as appropriate. Verify that
  a pause appears in scheduled-send latency instead of reducing offered load.

Acceptance: a small real-socket scenario passes, each negative control is
detected, artifacts replay the result, and generator limits are distinguishable
from server saturation. No production API change is required for this phase.

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
