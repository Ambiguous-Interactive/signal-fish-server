# Production Correctness Audit

Campaign: [#826](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/826).
Baseline: `5edad6e0`. Started: 2026-10-08. Status: **in progress**.

This report records a staged audit, not a production certification. An inspected
path is not a verified subsystem. A passing bounded model does not prove the
implementation, its environment, or all possible schedules correct.

## Contract and method

The shipped server owns each room in one process. In-memory lock and coordinator
types do not implement cross-process consensus. Process loss discards rooms,
routes, tokens, and replay. Read the
[consistency contract](../architecture/consistency-and-durability.md) and
[deployment boundary](../architecture/single-instance-deployment.md).

Audit each observable operation at five separate boundaries: validation, local
mutation, queue admission, socket write, and client application. Record an
unknown outcome when transport failure prevents the client from distinguishing
commit from rejection. Correlation IDs do not imply deduplication or idempotency.

For each hypothesis, record the required invariant, competing explanation,
smallest failure history, baseline revision, command, seed or schedule, result,
and limits. Demonstrate a red test before repair. Preserve the green regression
and use an independent review. A timeout, harness saturation, missing platform,
or skipped scenario is not a successful experiment.

Method references:

- [Gilbert and Lynch's CAP result](https://www.cs.princeton.edu/courses/archive/spr22/cos418/papers/cap.pdf)
  defines the partition tradeoff. Apply it to an explicit consistency and
  availability contract; it does not establish this server's local correctness.
- [Jepsen's linearizability definition](https://jepsen.io/consistency/models/linearizable)
  motivates checking concurrent histories against independently specified legal
  sequential histories, including incomplete operations.
- [Tokio cancellation semantics](https://tokio.rs/tokio/tutorial/select)
  motivate inspecting every suspension between mutation and publication.

## Coverage ledger

`Initial` means source and contract reconnaissance only. `Pending` means no
audit conclusion. Findings below describe only the exact paths investigated.

| Task | Implementation and operations | Invariants and experiments | Status |
| --- | --- | --- | --- |
| A01 | `protocol/`, AsyncAPI, wire samples | Schema, errors, optional fields, v2/v3 projections, exhaustive operation mapping | Initial |
| A02 | `websocket/handler.rs`, `routes.rs`, `mod.rs` | Upgrade, routes, frame limits, TLS/plain entrypoint parity | Initial; HTTP/2 route rejection recorded in F13; certificate-bound partial-write history in session 413; combined tenant/certificate history in session 414; encrypted-record loss in session 415 |
| A03 | `auth/`, `security/`, handshake in `websocket/connection.rs` | Authenticate, app isolation, token binding, origin, admission, expiry, replay | Initial; concurrent handshake retry-budget loss recorded in F15; signed-credential response-loss history in session 412; certificate-bound partial-write history in session 413; combined tenant/certificate history in session 414; encrypted-record loss in session 415 |
| A04 | `server/connection_manager.rs`, `websocket/connection.rs` | Identity fencing, socket ownership, reader/writer shutdown, cancellation | Initial; caller cancellation and failure cleanup repaired in F07/F10 |
| A05 | `reconnection.rs`, `server/reconnection_service.rs` | Claim, restore, rollback, token rotation, reconnect races and expiry | Initial; signed-credential response-loss history in session 412; certificate-bound partial-write history in session 413; combined tenant/certificate history in session 414; encrypted-record loss in session 415; embedded receiver-loss rollback in F18; size-refusal rollback in F21; known socket-close rollback in F22; stale expiry cleanup in F32 |
| A06 | Control replay and `Reconnected` snapshots | Snapshot precedence, replay completeness, lost responses, resynchronization | Initial; signed-credential response-loss history in session 412; certificate-bound partial-write history in session 413; combined tenant/certificate history in session 414; encrypted-record loss in session 415; known receiver closure before response submission in F18; aggregate replay bounds in F21; socket close before queue commit in F22 |
| A07 | `server/room_service.rs`, `database/` | Join, leave, capacity, passwords, room codes, tenant ownership, partial admission | Initial; stale expiry cleanup in F32; concurrent join/start histories in session 471; concurrent departure/start histories in session 474; spectator admission versus ready-off/start in session 589; spectator departure versus ready-off/start in session 592 |
| A08 | `server/ready_state.rs`, `coordination/room_coordinator.rs` | PlayerReady, StartGame, membership at commit, readiness snapshots, publication | Initial; two ordered concurrent unready/start histories and recovery checked in session 456; concurrent join/start histories in session 471; concurrent departure/start histories in session 474; spectator admission versus ready-off/start in session 589; spectator departure versus ready-off/start in session 592 |
| A09 | `server/authority.rs`, `moderation.rs` | AuthorityRequest, kick, ban, unban, transfer, code rotation, access changes | Initial; concurrent transfer/start and host election checked in session 483 |
| A10 | `server/spectator_service.rs`, `spectator_handlers.rs` | Join/leave spectator, role exclusion, rosters, stale detach, moderation | Initial; lost terminal events during detach panic recovery recorded in F31; spectator baseline versus ready-off/start in session 589; spectator departure versus ready-off/start in session 592 |
| A11 | `server/session_policy.rs`, `signaling.rs` | ProvideConnectionInfo, Signal, plan selection, host eligibility, generations, fallback | Initial; explicit hostless relay reset in F38; concurrent join/start mesh publications in session 471; backpressured signal/departure ordering in session 502 |
| A12 | `server/message_router.rs`, `relay_policy.rs` | Dispatch, TransportStatus, negotiated capabilities, stale source identity | Initial; concurrent library transport-status publication in F100 |
| A13 | `server/game_data.rs`, `coordination/mod.rs` | JSON/binary GameData, acceptance stamps, exact recipient set, fan-out | Initial; embedded gameplay crossing reconnect repaired in F123; exact JSON sizing in F228 |
| A14 | `coordination/outbound_queue.rs`, `protocol/delivery.rs` | Reliable/latest/volatile, sequence ranges, generations, sojourn, bounded memory | Initial |
| A15 | `websocket/sending.rs`, `batching.rs`, writer in `connection.rs` | Encoding, partial writes, idle reports, cancellation, terminal close | Initial; certificate-bound partial-write history in session 413; combined tenant/certificate history in session 414; encrypted-record loss in session 415; aggregate reconnect admission bound in F21; socket close before queue commit in F22 |
| A16 | `server/heartbeat.rs`, `maintenance.rs`, `deadline.rs`, `distributed.rs`, `retry.rs` | Ping, deadlines, clock jumps, lease ownership, GC, stale cleanup | Initial; stale expiry cleanup in F32; delayed first-tick overflow in F36; interrupted heartbeat retry in F224 |
| A17 | `server/shutdown.rs`, `main.rs`, deployment configs | Drain, restart, room routing, directional partitions, process failure | Initial; plain HTTP response loss repaired in F12 |
| A18 | `config/`, `rate_limit.rs`, `server.rs`, `lib.rs` | Construction validation, safe limits, public embedder contract, feature combinations | Initial; invalid binding aliases in F34; room-code frame bounds in F35; timer overflow in F36 |
| A19 | Metrics, logging, admin, dashboard cache, session records | Bounded resources, accounting consistency, diagnostic claims | Initial; credential diagnostics reviewed in F08 |
| A20 | Native, browser, Fortress, WASM clients | Event application, numeric precision, interop, reconnect, generation resets | Initial; stale peer exchange evidence recorded in F16; invalid probe receipts recorded in F17; buffered ICE rejection recorded in F20; duplicate data-channel replacement recorded in F23; unsupported admission and reset/reopen failure recorded in F24; retired poller retention in F25; unread required-label announcement growth in F26; incoming message backlog in F27; browser input and output retention in F28; native stdout stalls in F29; native stderr stalls in F30; nominated ICE path evidence in F33; native remote shutdown and stream termination in F37; required delivery modes in F39; SCTP reset/reopen lifetime in F40; retired channel retention in F41; browser peer-requested retries in F42; native late retries in F43; browser authoritative replacement waits in F44; wall-clock duration changes in F45; authoritative recovery events in F46; peer incarnation link fencing in F47; closed browser channel registration in F49; blocked browser negotiation in F50; serial planned-peer deadline renewal in F54; handshake command windows in F55; blocked WebSocket writes and swallowed signaling failures in F56; expired ready negotiation results in F57; expired WebSocket upgrades in F58; buffered receive deadline bypass in F59; late native write completion in F60; shortened native Pong response windows in F61; latched reference-client readiness in F62; crippled browser SDP in F66; native exchange send loss in F67; native receipt cutoffs in F84; browser linger expiry in F85; native held-send windows in F86; replacement-link probes in F87; expired relay sends in F88; relay receipt admission in F89; terminal callback formation in F91; queued keepalive liveness in F97; admitted native receipt retention in F101; local closing notification in F102; browser state history in session 486; native fragmentation/reset histories in session 504; native stdout receive ownership in F197; premature native rebuild success in F198 |
| A21 | `formal/`, `trace_validation.rs` | Model/source correspondence, fairness, finite bounds, trace completeness, negative controls | Initial |
| A22 | Tests, helpers, fuzz targets, CI | Oracle independence, missing/duplicate events, skips, mutations, features and platforms | Initial; registry preparation failures recorded in F11; false exchange-success oracle recorded in F17; Windows backpressure fixtures in F75; transport failure summaries in F97; closed-epoch delivery evidence in F130; initial join cancellation and state-matched allocation evidence in F228 |

For each task, record reviewed functions and tests, unresolved hypotheses, and
the exact scope of any successful experiment. Default, TLS, legacy-fullmesh,
and trace-validation paths need separate dispositions. Client packages have
separate build and test gates.

### Inbound operation crosswalk

This inventory follows `ClientMessage` and `RoomOperationRequest` at the
baseline. It maps audit ownership; it does not certify these operations.

| Wire operation | Audit tasks | Required observable boundaries |
| --- | --- | --- |
| `Authenticate` | A02, A03, A18 | Endpoint default, explicit version, tenant proof, negotiated capability publication |
| `JoinRoom` | A07, A08, A11 | Admission rollback, membership baseline, readiness and plan publication |
| `LeaveRoom` | A04, A07, A09, A14 | Unroute, terminal watermark, authority change, old queued tail |
| `GameData` and binary relay | A13, A14, A15 | Validate before stamp, fan-out set, exact delivery or omission evidence |
| `Signal` | A11, A12 | Same room, negotiated transport, session generation, error and valid budgets |
| `AuthorityRequest` | A09 | Role commit, personalized event, denial and reply budget |
| `PlayerReady` | A08 | Toggle, membership snapshot, broadcast; repeated requests are not idempotent |
| `StartGame` | A08, A11 | Authorization, exact readiness set, game and session-plan transaction |
| `ProvideConnectionInfo` | A06, A11 | Metadata validation, endpoint usability, snapshot freshness |
| `Ping` and WebSocket Ping/Pong | A04, A16 | Source identity, liveness, response budget, probe completion |
| `Reconnect` | A04, A05, A06 | Claim, identity reassignment, snapshot queue commit, token rotation |
| `JoinAsSpectator`, `LeaveSpectator` | A10, A14 | Role exclusion, baseline/terminal event, generation barrier |
| `TransportStatus` | A11, A12 | Negotiated capability, per-generation deduplication, fan-out suppression |
| `RoomOperation` envelope | A01, A12 | Capability gate, canonical ID, correlated result; no deduplication promise |
| Wrapped join/leave/reconnect/spectator operations | A05, A07, A10 | Same transaction as legacy command, one correctly correlated terminal result |
| `KickPlayer`, `BanPlayer`, `UnbanPlayer` | A05, A09, A15 | Authority check, target lifecycle, reconnect tombstone, ban perimeter, close |
| `RegenerateRoomCode`, `SetRoomAccess` | A07, A09 | Admission serialization, authorization, uncertain response recovery |
| `TransferAuthority` | A09, A11 | Eligible live member, replay/publication order, transport-host independence |

## Bounded histories without a confirmed defect

### A11 — Session 502 signal admission and target departure

At the [base commit](https://github.com/Ambiguous-Interactive/signal-fish-server/commit/ba65aa23087e3be0a9b4d6f60d14363a31b1bb63),
a new bounded history tests the interval after targeted Signal routing validation
and before queue admission. The target queue is full.
The test polls the real handler until its backpressure counter increases, then
runs an ordinary `LeaveRoom` through the message router. The existing lifecycle
attempt notification establishes that the leave worker has started. On the
current-thread runtime it acquires its uncontested lifecycle and reaches the
room gate before yielding. Both the target's stored membership and its local
assignment remain intact while the signal waits for queue capacity.

Draining the queue permits the exact original Signal, then RoomLeft and the
incumbent's PlayerLeft. A new signal to the departed target is rejected.
An ordinary rejoin increases the target epoch, and a fresh signal reaches it.
The history uses real coordinator delivery and room operations. It adds no
production pause hook, clock sleep, or synthetic epoch mutation. It covers
in-process queue admission; it does not establish socket-write ordering or
all reconnect schedules.

Temporarily removing only the signal room-gate acquisition fails the membership
assertion in Nextest `173f648d-9777-45e7-aeed-7680bdc8c557`: LeaveRoom clears
the target assignment while Signal still waits. Production source is restored.
The initial complete history passes in Nextest `8b22737f-b2b1-45db-ad68-15485d0e7d08`.
Final restored-source Nextest `f137c2a2-923d-4387-8a03-653fd9d3d7c4` passes the new
history and both existing signal room-gate controls.

The suspected replacement race is not a confirmed defect. Signal retains the
room gate through delivery. Ordinary leave takes that gate before clearing
membership, and reconnect takes it before identity reassignment. A seated
WebRTC connection cannot renegotiate its protocol in place. A test-only epoch
bump after validation would therefore bypass the production synchronization.
This control narrows A11 coverage under open #826. It changes no runtime,
wire, client API, or configuration behavior and needs no client migration.

### A08/A11/A20 — Session 484 endpoint, negotiation, and mesh-debt histories

At `9dcbafcf`, fresh investigations continue open #826, #853, and #889.
No new runtime defect or historical incident cause is confirmed.

The server queues StartGame from one member and a Direct host endpoint update
from another against a held room event gate, in both orders. The harness waits until the endpoint worker
holds its own lifecycle. In update-first order, it then queues StartGame. This
runs on one thread; the worker has no suspending work before the held room gate. An initial
harness incorrectly observed the `lock_owned` attempt counter for a `lock`
caller and timed out. The final harness observes that the worker holds its
lifecycle and verifies unchanged lobby state before releasing the room gate.
Both recipient queues contain GameStarting with the same membership and
expected host endpoint. When start commits first, they contain the old endpoint then
the new endpoint in a fresh common generation. When update commits first, they
contain one plan with the new endpoint. Peer direction, fallback, stored metadata,
finalized readiness, and empty output tails agree. Treating every valid endpoint
change as unchanged suppresses the replacement plan and fails Nextest
`9bec79f0`. Original production source is restored. Final `853ab427` passes the
new history plus neighboring readiness/start and transfer/start controls.

The native control injects parse-invalid SDP into the initiator and responder
in separate runs,
then buffers malformed ICE before applying the original valid SDP. It rejects
another malformed candidate after SDP, preserves the same physical generation,
and opens both real required channels. Reliable messages sent before and after an
unreliable probe arrive in reliable order in both directions. It awaits each opposite
endpoint's receive queue. An unreliable receipt alone does not prove reliable
delivery; this control independently requires both reliable payloads. Propagating
buffered ICE rejection instead of continuing the Offer flush fails Nextest
`4cfabc3e`; the independent Answer-only fault fails `302a6c77`. Restored
Nextest `f65781eb` passes both role histories. This excludes
these specific parse-level rejection histories as deterministic establishment
failures. It does not cover every stack error or reproduce Chromium failure.

Eight browser orchestrator histories cover both mixed mesh roles, either
missing receive label, and recovery versus unpaid exit. UUIDs follow the
server's lesser-UUID offerer rule. Both pairs send both exact probes. A
wrong-player payload cannot pay the missing label. One pair's channel error
retires only that pair; healthy callbacks continue. Without recovery the exit
names only the unpaid peer/label, and the per-peer exchange snapshot names only
the missing receive label. Selective retry sends only the rebuilt pair's
probes and the missing receipt permits success. Three in-memory faults remove
peer identity validation, ignore exchange debt, or clear all connected pairs
on one loss; each fails the test. These are controlled application
histories with mock browser channels, not real Chromium transport evidence.

Independent review corrects browser role realism and verifies root scheduling
and native role/receive boundaries. All changes are test coverage and evidence.
No server wire, runtime, SDK API, configuration, or dependency behavior changes;
no changelog entry or external Rust, .NET, or Godot migration is required.
The three original issues remain open with their acceptance unchanged.

Final fixed-artifact Nextest `d7597feb` passes native restore, native/browser
retry, and three-player browser mesh in 3.364 seconds. All seven endpoint
streams exit zero, with 26 channel sends and 26 receipts. Server, native
executable, browser CLI, and page hashes match before and after. Full endpoint
streams and server logs remain in local `progress/session-484/fixed-final`.
Initial `3d149c39` also passes but rebuilds the native executable during harness
compilation; it is excluded from the fixed-artifact claim. These passing
controls do not explain either historical incident. Strict root and native
lint, browser tests/type checks/build, and source-policy checks pass.

### A08/A09/A11/A20 — Session 483 gameplay ordering investigations

At `e190bed7`, new histories investigate open #826, #853, and #889.
No new runtime defect is confirmed. These controls narrow specific hypotheses;
they do not establish the historical browser incident causes.

The server control queues TransferAuthority and StartGame from different source
lifecycles against the same held room event gate in both FIFO orders.
The successor can start only after transfer commits. A premature start must
leave the lobby and active plan unchanged; a later retry must succeed.
All three members receive the exact authority snapshot, GameStarting roster,
and personalized Host/WebRTC plan. Stored authority flags, readiness, host,
peer sets, offerer directions, common generation, and empty output tails agree.
The private refusal has no order contract against AuthorityChanged; both orders
are accepted. Bypassing only the start-role comparison fails negative control
Nextest `b6aa3863`. Production source is restored before final validation. Scoped Nextest
`9cb1e2e9` passes this history and three neighboring contention controls.
After naming empty-queue reads, final `ea685421` passes the new history and
all 11 source-policy controls pass.

The native control holds either required RemoteChannel admission while a real
native offerer opens both channels and sends both probes. The admitted sibling
receives credit before pair formation. The held label's RTC messages_received
counter must reach one before its application poller starts. The RTC core
increments this counter after complete message decode; DCEP does not count.
This proves transport arrival before delayed admission, without a sleep or
cross-stream ordering assumption. Later admission preserves both receipts,
forms one pair, and sends exactly two deferred replies. Repeated admission and
open observation do not add replies. Both replies reach the remote endpoint.
Dropping receipt credit until pair formation fails Nextest `13b55c0b`.
Restored source passes `52b478c2`; final `668fc044` also passes.
This is native/native transport evidence;
it does not prove Chromium has the same callback schedule.

The browser controls hold remote SDP while candidates arrive before and during
its application, for both valid offerer roles. Candidates retain their exact
sdpMid, sdpMLineIndex, and usernameFragment and flush in arrival order only after
SDP completes. One rejected early candidate must not discard later candidates,
block stable negotiation, or suppress the correctly addressed and generation
stamped Answer. Later trickle ICE still reaches the same link. In-memory
ablations that reverse the queue, drop media binding, or abort on rejection each
fail the new engine assertions. Aborting on rejection also fails the
orchestrator control. These are controlled browser-stack histories, not a real
Chromium reproduction. Local scripts and complete outputs remain in ignored
`progress/browser-negotiation-validation.*` and `progress/session-483`.

Independent review corrected a private-error ordering assumption and verified
the real RTC receive barrier. Changes are internal test coverage and audit
evidence. No runtime, wire, SDK, configuration, or dependency behavior changes;
no changelog or external SDK migration is required. The wider audit and both
historical investigations remain open.

Session 483 fixed-artifact qualification passes three restores, three retries,
and three browser meshes in Nextest `f8529003`, `23ad71a6`, `ce26e9ab`.
All 21 endpoint streams exit zero, with 78 channel sends and 78 receipts.
Server, native executable, browser CLI, and page hashes match before and after.
Complete endpoint streams and server logs remain in local `progress/session-483`.

### A07/A08/A11 — Concurrent departure and game start

At `16eff7ff`, three ready mesh members run StartGame and voluntary departure
against the same held room event gate. The control polls the two production
operations in each FIFO order. It holds the departing member's lifecycle gate
through completion, as the departure contract requires. Before room-gate
release, all three members remain stored, the departing member remains routed,
and all output queues stay empty. No sleep selects the order.

When StartGame wins, all three members receive GameStarting and tailored
SessionPlans before the departing member receives RoomLeft and the remaining
members receive PlayerLeft. The existing mesh plan stays active; mesh departure
does not publish a new plan. When departure wins, the remaining members receive
PlayerLeft before a two-member GameStarting and tailored SessionPlans. Exact
counts and identities exclude duplicate members and peers. All plan recipients
agree on their history's generation; each pair uses opposite offerer roles.
Both histories end with the two remaining members finalized and ready in storage and the
coordinator, no departed route, and no extra output.

`concurrent_leave_and_start_publish_exact_membership_in_both_orders` passes
in scoped library Nextest `510c8cb0`. A temporary wrong-room departure lock
fails the pre-release route assertion in `350ab636`; production source is
restored before the green run. Independent review corrected the lifecycle
precondition and found no remaining issue. This adds bounded handler/queue
coverage for open #826. It does not prove all departure schedules or TCP writes.

New browser and native source audits also examined initial pairing, queued
channel callbacks, candidate application, replacement probe sends, and terminal
success arbitration for #853 and #889. No new defect was confirmed. In
particular, input first observed after the native finite exit frontier is not
proof that the frontier omitted eligible work. No speculative runtime change
ships for that hypothesis. Fresh fixed-artifact Nextest `90ceb60b` passes room
restore, link retry, and the three-player browser mesh. All seven endpoint
streams and server logs remain under local `progress/session-474-baseline`;
before/after hashes match for the server, native executable, browser CLI, and
page bundle. These observations do not explain either historical incident.
Both issues and the wider correctness campaign remain open.

### A20/A22 — Partial-channel receipt and replacement histories

At `c5e63448`, new controls investigate open #826, #853, and #889. A valid
probe can arrive on one data channel before the second channel's open callback
is applied. Receiving it must not create pair connectivity or start local
probes. If the pair later forms, the timely receipt must retain its credit.
A wrong-player probe must leave receive debt unpaid.

The browser control dispatches both first-label orders through the real
orchestrator with a controlled browser stack and monotonic clock. It holds
receipt output, then announces the second channel. Separate close and error
histories retire the first channel while that output is held. These histories
must report fallback without forming a pair or sending probes. The live pair
must send both probes and complete; wrong-player receipts must fail criteria.
Eight histories pass. Temporary in-memory controls that discard early receipt
credit or ignore required-channel loss fail the corresponding histories.
No production source change is needed for these bounded schedules.
The browser runner retains them in `partial-channel.test.ts`.

The native control uses real paired WebRTC channels. It holds local open
callbacks, applies one probe, and retains the other probe across required-channel
closure and physical-link replacement. Both offerer roles and both first labels
are covered. It checks that old queued sends do not credit the replacement and
that the retired-generation receipt cannot pay missing debt. Fresh replacement
traffic must repay that debt while the already-applied logical receipt survives.
Native Nextest `e8d26cf3` passes the four histories in
`probe_before_open_and_loss_before_send_preserve_debt_until_replacement`.
A temporary control that bypasses the deferred-send channel identity check
fails because an old queued send credits the replacement. The production file
is restored byte for byte before the passing run. Independent review found no
required changes in either client control.

Live baseline `f83bfd9d` passes restore and browser mesh. Fixed-artifact
`5523971c` passes restore, retry, and mesh. Fixed-artifact stress `e9c824cb`
passes ten serial repetitions each of restore and mesh. All endpoint streams
and server logs remain under local `progress/session-473-{baseline,fixed-baseline,repeated-baseline}`.
Before/after hashes match for the server, native client, browser CLI, and page
bundle in the fixed-artifact runs. The initial baseline rebuilt the native
binary and has no pre-build hash claim. These passes neither reproduce nor
explain the original #853 or #889 incidents. Both issues and the wider #826
audit remain open. Tests and audit evidence change; shipped behavior does not.

Publication exposed [#1043](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1043):
the changelog gate requires release notes for browser unit tests and their test
runner. The classifiers need the same internal-test scope in the hook, checker,
and CI detector. Runtime source and build scripts retain their existing rules.

### A07/A08/A11 — Concurrent join and game start

At `62a9dfa5`, two ready mesh members and one joiner contend on the same room
event gate. The real join and StartGame handlers run in both FIFO orders.
Before release, the joiner has no room, storage still has two members, and
all three output queues are empty. The lifecycle barrier and competing
operation waits are bounded; no sleep chooses the order.

When join wins, the new member remains unready and StartGame returns
GAME_START_NOT_READY. The joiner's ready toggle then allows a successful
three-member start. When StartGame wins, GameStarting names the original two
members. The finalized-room join then publishes the new membership and fresh
mesh plans. Both histories check exact baseline readiness, peer sets,
generation agreement and replacement, offerer roles, publication order,
storage/coordinator readiness, and empty output tails.

The initial scoped run `5669d011` passes both histories. A temporary control
that makes StartGame acquire an unrelated room gate fails in `fef6a03f`
because output appears before the contested gate is released. The original
production source is restored. This is evidence for two controlled handler
and queue histories, not TCP delivery, all storage backends, or all schedules.
No new server defect is claimed; A07, A08, A11, and #826 remain incomplete.

Final focused run `4d9b7dc4` passes the join/start histories, the prior
unready/start histories, and the sequential join/readiness control. The
three tests pass without changes to production code.

### A20 — Browser establishment phase evidence

At `62a9dfa5`, the browser failure snapshot reports ICE connection, SCTP, and
channel state but omits signaling, ICE gathering, DTLS, remote-description
readiness, and queued candidate counts. The #853 and #889 investigations need
these observations to separate establishment phases. The snapshot now reads
those fields without awaiting the browser stack. It remains stderr evidence;
it does not alter exchange criteria, deadlines, retries, or exit codes.

The admitted candidate count includes candidates whose later stack application
fails. The pending count covers candidates still queued for the remote
description, not an in-flight stack call. Neither count proves a usable ICE
path. Absent SCTP and DTLS transports report `null`. No candidate or SDP content
is added to the snapshot. Native snapshots retain their existing scope: its
wrapper state getters are asynchronous, so this change adds no timeout-path
waits to native clients.

The initial live run `65fb28c3` passes room restore and the
three-player browser mesh. All five endpoint streams and server logs are
retained under local `progress/session-471-baseline`. Passing histories
do not establish the original #853 or #889 failure causes. Both remain open.

Final fixed-artifact runs `56f2da1e`, `f3b78434`, and `e0e97285` pass three
restores, three retries, three browser meshes, and one controlled crippled-ICE
fallback. The 24 retained endpoint streams contain 82 channel sends and 82
receipts, plus 12 relay sends and 24 relay receipts. Eight real browser timeout
snapshots include every new phase field. They distinguish an absent transport
from applied remote SDP with DTLS still new. Before/after hashes match for all
four executable/bundle artifacts; endpoint and server logs remain under local
`progress/session-471-qualified-{1,2,3}`. Earlier candidate `41a75599` passes
the four cells but rebuilds two executables; its pre-build hashes are excluded.

The browser phase regression fails against the original snapshot and passes
after the added observations. Full browser tests, build, type checking, and
formatting pass. Final root `43f8e794` passes the three focused ready/join
controls; source-policy `f333b8f2` passes all 11 checks. Strict root Clippy and
independent adversarial review pass. Rust and .NET SDKs own signaling/relay;
Godot owns its engine mesh. None imports this browser stderr snapshot, and no
server or SDK behavior changes. Hosted CI and feedback remain delivery gates.

### A08 — Concurrent unready and game start

At `d503f04d`, two ready v3 mesh members contend on the room event gate.
The retained `concurrent_unready_and_start_follow_room_gate_order` test polls
both real handlers until pending behind the held gate, in each FIFO order.
Neither client receives output before the gate releases.

When StartGame wins, both clients receive GameStarting then SessionPlan.
The ready toggle receives INVALID_ROOM_STATE. The finalized room keeps both
members ready. When the unready toggle wins, both clients receive the unready
lobby snapshot and StartGame receives GAME_START_NOT_READY. The room stays in
Lobby. Making the member ready again then permits a successful start.
Both histories check exact output queues and the live and durable ready sets
at their appropriate lifecycle boundaries.

A temporary control that bypassed StartGame's room gate failed the test.
The production source was restored. This checks two controlled handler and
queue histories. It does not cover TCP delivery, storage failures, all
schedules, or all A08 membership operations. No new defect is claimed;
A08 and campaign #826 remain incomplete.

## Initial findings

### F01 — A canceled write does not fence every close path

**High impact; high confidence.** Issue
[#827](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/827).
Affected code: `websocket/connection.rs::finalize_closed_connection`.

`InboundRateLimited` and `Kicked` bypassed the branch that abandons queued data
after a canceled write. If the selected frame never reached the socket, the
close flush could deliver later sequence numbers without an exact omission
report. A terminal close does not authorize an earlier unexplained hole.

The existing regression exercised only the no-reason close. It could not prove
the invariant for named close reasons. Extending its real-socket matrix exposed
the rate-limit branch. The repair applies the abandonment check to every
remaining close reason after the specialized slow-consumer and oversize paths.
Healthy teardown remains a separate positive control.

The experiment controls the selected-write cancellation seam and observes real
WebSocket output. It does not reproduce every possible kernel partial-write
schedule. The retained matrix includes all ten reasons and the absent reason,
with both healthy and abandoned-write states. It checks gameplay output and
drop counts, not exact close-code or delivery-report ordering.

### F02 — Handwritten proofs certify obsolete decision rules

**Verification defect; high confidence.** Issue
[#828](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/828).
Affected code: `formal/z3/protocol_invariants.py`, sets A and G.

Set G raised below-floor versions to the server minimum. Rust preserves a lower
client ceiling so the caller can reject it. With client maximum 2 and server
range 3 through 3, the old model returned 3 while Rust returns 2. A passing old
proof established the wrong negotiation contract.

Set A selected Host+Direct when capabilities matched, without requiring a
usable host endpoint. Rust also checks execution readiness. With WebRTC
disabled, Direct enabled and supported, and no usable endpoint, the old model
selected Host while Rust falls back to Relay.

New obligations fail against both obsolete rules. The corrected models keep
explicit mutant witnesses and a positive executable-Direct control. The proof
documentation now distinguishes model assertions from Rust implementation
verification and integer arithmetic from bounded machine arithmetic.

The TLA session model still assumes executable Direct endpoints. Its profiles
do not model endpoint loss on reconnect. This boundary is now explicit; the
current Rust endpoint tests do not extend that model's state space.

### F03 — Documented replay order overwrites fresh peer metadata

**Client correctness defect in documentation; high confidence.** Issue
[#829](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/829).
Affected contract: reconnect scenario, protocol, client guide, and ReplayStatus.

During A's absence, B joins without connection information and then supplies a
Direct endpoint. On a v2 replacement connection, the reconnect snapshot contains
B's new endpoint, while the historical `PlayerJoined` contains none. Following
the documented instruction to apply the snapshot and then replay removes the
endpoint.

The experiment falsifies that client algorithm; it does not identify a server
snapshot defect. The retained negative control demonstrates the stale overwrite
and verifies that final snapshot replacement restores the endpoint. Corrected
guidance requires authoritative snapshot replacement for every replay status.
Historical processing is optional and must precede that replacement.

`complete` reports ring retention before recipient filtering. It does not mean
every authority transition was returned or that the client received an old
socket's unread events. Existing filtering is deliberate. The native reference
client already ignores history when applying the snapshot.
The initial fixture inspected internal v3 messages before wire projection.
Independent review identified that v3 omits `connection_info`; that experiment
does not establish v3 wire-visible endpoint loss. The corrected counterexample
uses a v2 replacement connection and a JSON round trip matching its serializer.
A separate v3 fixture retains the authority-filtering and `complete` assertions.
These tests use server handlers, outbound queues, and a small client state map;
they do not exercise a TCP reconnect or every reference client.

### F04 — Idle delivery reports bypass the write-progress deadline

**Availability defect; high confidence.** Issue
[#830](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/830).
Affected code: the writer's pending-report timer in `websocket/connection.rs`.

An idle flush awaited `write_pending_unsupported_report` without the deadline
used for queued reports. A non-reading peer could hold this write beyond the
configured progress budget. The heartbeat task waits for the same writer to
acknowledge a Ping command, so it cannot independently bound this await.

The repair shares the existing selected-write deadline and slow-consumer close
policy. An incomplete write retains its pending omission evidence. A zero
budget retains the existing explicit deadline-disable behavior.

The real-socket experiment fills a clamped peer's receive pipeline, prepares a
pending omission, advances a 20 ms virtual budget, and awaits the writer with a
2 ms virtual scheduling allowance. Restoring the old unbounded await makes the
corrected test fail. The repair passes. Healthy controls check exact ranges,
counter totals, cleared pending state, no close, and disabled deadlines.

An initial probe assumed a timer must resolve on the first poll after clock
advance. That assumption was false; its red result is excluded. The corrected
experiment proves bounded completion after timer-driver progress, not exact
wall-clock latency on every operating system.

### F05 — Retrying an ambiguous report duplicates exact omission ranges

**Client correctness defect; high confidence.** Issue
[#833](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/833).
Affected code: pending, queued, and final delivery-report writes.

Canceling a report send can leave its frame buffered inside the WebSocket sink.
The server has not committed the pending omission ledger. Teardown retries that
ledger, so a recovering peer receives overlapping reports with identical
cumulative counters. A timeout cannot distinguish an unwritten report from one
that will become visible when a later close flushes the sink.

The real-socket regression expires an idle report against a non-reading peer,
then resumes the peer while slow-consumer teardown runs. It observed two reports
for sender 9, epoch 1, sequence 1, both with `unsupported_format: 1`. The close
code was 4002 and no later queued gameplay arrived. This exposes a gap in F04's
initial oracle: bounded writer completion alone does not prove safe recovery.

The repair records ambiguity for the physical connection when a report-write
guard drops before completion. Successful later writes cannot clear that state.
Pending, queued, and final report paths preserve the unconfirmed evidence,
prevent report retries, and fence later gameplay. Known size rejection occurs
before socket submission and leaves the connection's report state unambiguous.

The test compares the complete recovered report and requires exactly one copy.
It uses an oversized binary filler solely to create transport pressure; that
filler is not valid protocol gameplay. It covers one controlled cancellation
and recovery schedule, not every partial-write boundary or client implementation.

### F06 — Priority traffic hides reliable delivery age

**Delivery correctness defect; high confidence.** Issue
[#832](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/832).
Affected code: the outbound queue, queued deadlines, and the live socket writer.

Both receive modes prefer current-generation control over data. The batch drain
also checks control before staged data. The old deadline consulted unresolved
reliable age only when the selected payload itself was reliable. Fresh control
could therefore continue reaching a healthy recipient after queued reliable
had exceeded its enqueue-to-write budget. Ping and idle-report writes also
bypassed that resident age, including reliable admitted during an existing write.

The repair watches reliable admission independently of writer selection. Its
notification does not consume the receiver's wakeups. The watcher includes
all queue generations, rechecks rows after timer wake, and closes only for
currently unresolved reliable data. Every selected write also retains the
oldest resident or staged reliable bound. This covers immediately ready control
drains that might otherwise run without yielding to the watcher. Queue order
and transition barriers remain intact. Lossy age does not expire fresh control.
Known unsupported payloads keep their own write-progress policy; unrelated
unresolved reliable data still bounds the writer. Zero and unrepresentable
internal deadlines retain their existing inert behavior.

The initial real-socket red observed three fresh controls while expired reliable
remained resident, in both receive modes. The regression then strengthened the
history to write fresh controls at two and four seconds, before crossing the
five-second reliable budget. Separate controls retain timely reliable delivery,
stale latest/volatile progress, and an explicitly disabled internal budget.

Queue tests cover delayed admission on both v2 and v3, stale timer removal,
fresh successors, future generations, and simultaneous enqueue/expiry. Socket
seam tests cover reliable arrival during stalled control and Ping writes, then
restore reader progress and check semantic close 4002, no reliable leak, and
exact abandonment. An exaggerated batching wait isolates watchdog cancellation;
it is an internal seam test, not a claim that this operator configuration passes
validation. The initial positive Latest fixture lacked its required key, and
initial teardown queues lacked metrics; those invalid fixture results establish
no production conclusion.

The full `/v3/ws` experiment authenticates and joins a room, then exercises the
spawned `handle_socket` writer in both batching modes. A player-keyed test gate
holds only that writer between selections. Reliable data enters its actual
queue once; one fresh control enters before each selection. Controls at two
and four seconds reach the client. Crossing the five-second reliable budget
while the next selection remains held causes close 4002 without releasing the
gate. No overdue reliable or later control reaches the client. Abandonment and
disconnection each count once. The gate and its temporary sender compile out of
production; the test establishes a controlled schedule, not physical latency
on every platform. An initial handler fixture used an invalid authentication
timeout; that setup failure is excluded.

Seventeen focused local tests pass. A temporary negative control disables the
independent live watcher and restores selected-item-only deadlines. All five
selected regressions fail: the two timed socket seams write the third control,
the two full handlers miss the close deadline, and the all-class matrix rejects
the extended deadline. The production repair is restored after that experiment.
Hosted acceptance remains required.

### F07 — Caller cancellation loses the live socket supervisor

**Lifecycle defect; high confidence.** Issue
[#835](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/835).
Affected code: `websocket/connection.rs::handle_socket`.

The handler owned shutdown tracking but spawned its reader and writer separately.
Canceling the handler dropped their join handles, detached those tasks, and
removed the tracker. A real socket remained registered and answered a later
application Ping while `has_active_socket_tasks()` returned false.

The repair gives the complete handler an owned supervisor. Canceling its caller
stops that caller's wait; it does not interrupt the supervisor or its in-flight
room transactions. Registration, early challenge waits, socket halves, and
their existing bounded teardown remain inside the tracked lifetime. The
regression cancels the caller after a wire-visible room join, checks retained
tracking and a Pong, then closes the client and checks removal of registration
and tracking. The original handler fails this history at the tracking boundary.

The normal standalone drain signals closes rather than canceling this caller.
This experiment proves the caller-cancellation boundary, not an observed
deployment shutdown failure. The repair adds one Tokio task per accepted socket;
its performance cost has not been measured. Supervisor panic and runtime loss
are separate boundaries; this repair does not certify them. F10 records the
later experiments at those boundaries.

The identity sweep inspected lifecycle acquisition, identity reassignment and
rollback, pointer-match checks, and unregister cleanup. Seventeen existing
focused cases passed. This evidence does not complete every A04 schedule.

### F08 — Diagnostic formatting exposes session credentials

**Credential disclosure; high confidence.** Issue
[#836](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/836).
Affected code: reconnect registration, credential-holder Debug implementations,
and `websocket/token_binding.rs::TokenBindingViolation`.

The production disconnect log emitted eight characters of a bearer reconnect
token. Derived Debug output also exposed complete reconnect and relay tokens,
join passwords, TURN credentials, token-binding signatures and session keys,
metrics credentials, and stored room password hashes. Nested messages, room
state, and configuration inherited those disclosures.

A separate malformed-frame history places a credential marker in an invalid
message type, protocol-version value, proof scheme, or duplicate object key.
The JSON and MessagePack parser errors quote those values, and the receive loop
logs their Display text. The red parser test exercises those decode boundaries.
The disconnect-log test captures the production registration event and checks
its player/room metadata and credential absence.

The repair removes the token-prefix field and gives credential holders explicit
safe Debug output. Command bodies are omitted where they can contain secrets;
safe operation names, IDs, scalars, and snapshot counts remain. Parser
diagnostics keep safe error classes and numeric JSON positions rather than raw
serde error text. Normal, pretty, and nested formatting are covered. JSON and
MessagePack still carry the original wire credentials; configuration persistence,
password verification, and reconnect validation remain separate positive checks.

The prefix alone does not establish a practical seat-takeover exploit. Full
Debug disclosure matters when callers log these public types. Arbitrary
application payloads are not classified as typed credentials, and a caller can
still explicitly log a public raw credential field. No credential format,
public field, or verification algorithm changes.

### Session 382 reconnect recovery characterization

The A05/A06 history queues a successful `Reconnected` baseline and abandons its
receiver before reading it. The committed reconnect consumes the old claim and
retains a rotated credential. Actual unregister then arms that new credential.
Retrying with the only token known to the client fails with
`RECONNECTION_TOKEN_INVALID`; a fresh join recovers the same room and name under
a new player identity. The old pending record remains bounded by its window.

This is a characterization of the documented contract, not a new defect.
Queue admission does not prove client observation. The experiment controls queue
abandonment; physical socket-write loss, partial writes, and authenticated
application recovery remain separate untested histories.

Reproduce the focused session histories:

```bash
cargo nextest run --lib -E 'test(canceled_socket_caller_preserves_tracking)'
cargo nextest run --lib -E 'test(reconnection_token_debug_never) | test(disconnect_registration_logs)'
cargo nextest run --lib -E 'test(rejected_frame_diagnostics_never) | test(runtime_server_config_debug)'
cargo nextest run --lib -E 'test(losing_unread_reconnected_baseline)'
cargo nextest run --test protocol_v3_negotiation -E 'test(debug_) | test(configuration_and_room_password_debug)'
```

The default and all-feature builds exercise the same redaction and supervisor
code. TLS still uses the same `handle_socket` entrypoint; the legacy-fullmesh
listener remains a separate lifecycle. This slice does not certify its socket
supervision or the complete TLS route, schema, or feature audit. Applicable
hosted CI and independent review remain required before merge.

### F09 — Cross-platform fixtures assume incidental buffering and timing

**Test-oracle defects; high confidence.** Issues
[#838](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/838)
and [#795](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/795).
The scheduled main CI run
[37928204106](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/37928204106)
tested `9fa7c85c` and failed on Windows and macOS.

The Windows canceled-report regression failed before its delivery assertion.
Its first 8 MiB transport-pressure write completed. Clamping the peer receive
buffer does not prove that this first write must remain pending. Three socket
fixtures shared that assumption. Their setup now sends pressure frames until
a write stalls for 50 ms on the real clock, with a limit of sixteen frames
(128 MiB). A first Pending poll alone can mean transient socket readiness. The
recovering-peer oracle counts every complete pressure frame and still requires
one exact report, no later gameplay, and close 4002. A setup that never stalls
fails; it is not a skipped or successful experiment.

The macOS payload-size fixture rejected a run with 390167 us generator lag
against a 250000 us budget. This fixture verifies live ingress and egress sizes
and exact artifact replay; it does not establish deployment capacity. Related
wire and provenance fixtures now declare a two-second functional lag budget.
Their workload and delivery assertions remain intact. Production defaults and
capacity acceptance retain their existing budgets. The live saturation control
must reject a declared stall above either the short or functional budget, retain
unsent work, and reproduce that rejection from artifacts.

These corrections require focused tests and full hosted CI on Linux, macOS,
and Windows. They do not prove a universal kernel-buffer limit, physical latency,
or deployed capacity. The original hosted failures are the red evidence.

### F10 — Admission and supervisor failure lose connection ownership

**Lifecycle defect; high confidence.** Issue
[#839](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/839).
Baseline: `9fa7c85c`. A real socket joins a room, then a test-only gate injects
an unwind in its owned supervisor. The caller returns while the connection
remains registered. Dropping the supervisor's reader and writer JoinHandles
detaches those tasks; dropping its tracker does not close them.

The repair retains the lifecycle, close signal, and child handles outside the
unwind boundary. An unwind requests physical close before asynchronous room
cleanup. Cleanup follows the saved lifecycle through reconnect identity moves
and preserves its pointer-match fence. Tracking ends after every remaining
child has joined. Completed handles are removed before diagnostics or cleanup
can unwind, so recovery never polls a completed JoinHandle twice. Failure logs
record panic and cancellation categories without formatting panic payloads.

Enabled Ping and RelayStats watchers also belong to the connection. Their
unexpected termination now starts teardown; disabled watchers remain pending
in the supervisor's select. The failure matrix covers reader, writer, Ping,
and RelayStats aborts, joined supervisor unwind, and unwind after a reconnect
identity move. A separate early-unwind case checks registration before any
child starts. The established caller-cancellation control still requires a live
Pong and orderly cleanup after client Close.

The same ownership sweep found an earlier admission boundary. The manager
reserves IP and global slots, inserts the client, and awaits the routing adapter
before returning its identity. Callback unwind and caller cancellation both
leave a live slot on the baseline. Red controls fail the release oracle; an
ordinary adapter error still admits the client, as the existing policy requires.

Admission now owns local rollback from reservation through caller handoff. An
owned task handles the routing callback and its asynchronous rollback. The
caller acknowledges receipt without another await; cancellation after the reply
is sent still rolls back admission. The fresh lifecycle gate prevents maintenance
from removing a pending client while routing runs. Connection slots remain held
through routing cleanup, so a blocked adapter consumes bounded admission capacity.
A real socket closes with code `4000` during pending admission while its routing
pause stays held; registration and socket tracking then reach zero. Cleanup
releases local registrations, connection budgets, metrics, and delivery ledgers. An adapter that cannot
unregister may retain its own route; the close signal prevents that route from
representing a live socket. Arbitrary adapter recovery remains outside this
local cleanup guarantee.

The injected unwind establishes this failure boundary; it does not identify a
normal client request that triggers a panic. Process abort cannot unwind. No
wire fields, runtime dependencies, or room transaction cancellation rules change.
The public error enum gains `RegisterClientError::AdmissionFailed`: add this arm
to exhaustive matches, as described in the [library guide](../library-usage.md).
Admission unwind cleans up local state and returns that error. An unexpected
owned-task exit returns the same error; abrupt runtime loss still requires
discarding the server instance. WebSocket admission failure sends a bounded `1011 admission_failed`
close without exposing the panic payload. Production code does not rethrow a
panic; the repository's source-policy scan enforces that boundary. The routing
cleanup catch covers both callback future construction and polling. Its controls
include a synchronous constructor unwind and cleanup failures before and after
routing effects.

A separate experiment destroys a dedicated Tokio runtime after a wire-visible
join while retaining the server Arc. The caller and listener terminate and
socket tracking reaches zero, but registration, room membership, and active
connection metrics remain. The inspection runtime only reads this stale state;
it does not reuse the server. The [library guide](../library-usage.md) now
requires completing drain while the owning runtime lives, or discarding the
server instance after abrupt runtime destruction. Asynchronous cleanup cannot
run on a destroyed runtime. This does not change standalone process-loss
semantics or certify arbitrary cross-runtime reuse.

### F11 — Container publication depends on anonymous Docker Hub pulls

**Delivery infrastructure defect; high confidence.** Issue
[#841](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/841).
Main publication of `a381ad2d` failed in
[run 37990034187](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/37990034187).
Both attempts failed before QEMU setup completed. Docker Hub refused the
anonymous `tonistiigi/binfmt:latest` pull with `toomanyrequests`. No image
was built or published. Full main CI
[37991420668](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/37991420668)
also failed while preparing the Docker-based dependency audit action. Its
pinned Rust base pull returned HTTP 429 before workflow steps ran. A daemon
configuration step cannot repair action preparation. This failure does not
establish a server runtime defect.

The source sweep found the same registry dependency in both Buildx bootstrap
steps and the Dockerfile's Rust and Debian base images. Hosted CI proved that
QEMU and linter images can miss the public mirror and still hit HTTP 429.
The repair uses native QEMU and Docker's embedded builder with the containerd
image store. It removes both bootstrap image pulls. The embedded builder uses
the daemon's registry mirrors for base images. The
[Google mirror contract](https://cloud.google.com/artifact-registry/docs/pull-cached-dockerhub-images)
retains Docker Hub fallback for uncached images. A cache miss or an outage of
both registries can still fail the build; the repair does not guarantee
external registry availability.

The dependency audit uses native cargo-deny `0.20.2`, the same version shipped
by the old action. Every Cargo graph retains its policy, all-feature selection,
and explicit metadata toolchain. The central CI relevance gate and manual
interop audit gates still apply. Native installation removes the Docker image
preparation boundary; it still depends on external tool and advisory downloads.

Hosted PR CI also exposed the workflow linter's runtime Docker pull and a
coturn cache miss. Native actionlint uses the same release and retains its
ShellCheck and Python analyzers. The TURN profile and offline harness use the official
GHCR coturn image with the same `faca4aa5` manifest digest. Registry inspection
confirmed all seven platform descriptors match that pinned manifest. The
registry change alters the pull location, not the coturn version or image bytes.

### F12 — Plain HTTP shutdown drops active responses

**Lifecycle defect; high confidence.** Issue
[#843](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/843).
Baseline: `e5ffa4f8`. A real HTTP/1.1 request reaches a handler whose response
waits on a notification. The test signals shutdown while that response remains
pending, then releases the handler. The baseline connection task panics with
`async fn resumed after completion` and drops the request without its 200 response.
Dropping the shutdown sender reproduces the same failure.

The listener repeatedly selects a pinned `shutdown_resolved` async future.
That future returns when shutdown starts; selecting it again polls a completed
future. The outer JoinSet drain discards the resulting JoinError. An initial
busy-spin hypothesis was disproved by the observed panic.

The repair selects shutdown once, requests graceful connection shutdown, then
awaits that connection without polling the signal again. Both regression
histories require the server to retain the active request, deliver its complete
200 response, close the socket, and finish the listener. The handler remains
pending for 50 ms on the real clock to overlap shutdown with an active request;
all socket and task waits have separate five-second observation bounds.
The existing parked HTTP/2 keep-alive history remains a separate control.

This establishes response loss on the plain listener. Production starts HTTP
shutdown after WebSocket drain; the history does not prove gameplay loss during
that earlier choreography. TLS uses axum-server's separate shutdown path and
is not certified by this repair. No timeout, wire format, or public API changes.
The A17 restart and partition coverage remains incomplete.

Reproduce the focused histories:

```bash
cargo nextest run --lib -E 'test(http_shutdown) | test(parked_h2_connection)'
```

### F13 — WebSocket routes reject HTTP/2 CONNECT

**Gameplay admission defect; high confidence.** Issue
[#845](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/845).
Baseline: `1e7d5932`. The plain and TLS listeners enable extended CONNECT;
the TLS listener advertises HTTP/2 through ALPN. The routes accept only GET.
A real HTTP/2 WebSocket request receives 405 before the upgrade handler runs.
The library `/ws` route and production `/v2/ws` route reproduce that refusal.

[RFC 8441](https://www.rfc-editor.org/rfc/rfc8441.html#section-5) uses CONNECT
to open a WebSocket stream over HTTP/2. The route repair allows GET and CONNECT
explicitly and keeps each shared upgrade handler. The sweep covers the nested
v2 route, standalone aliases, and the public v3 route helper.
The dependency sweep also found that Axum's `http2` feature was disabled.
Its extractor validates the HTTP/2 `:protocol` value only with that feature.
The repair enables it so CONNECT accepts `websocket` and rejects other
protocols before creating a socket.
With the route change alone, a real `other-protocol` CONNECT receives 200
instead of 400. This negative control establishes why the feature change is
required; the original GET-only routes refused CONNECT before this boundary.

The client waits until Hyper reports the server's extended CONNECT setting
before sending the request. TLS cells require an actual `h2` ALPN result.
Originless native requests remain allowed by the existing policy; an initial
missing-Origin rejection expectation was invalid and was corrected.

The real-connection matrix checks authentication, the endpoint's default
protocol version, and room admission through the production plain and TLS
listeners and both library router shapes. Separate controls retain HTTP/1.1
GET, reject blocked origins, drain-time upgrades, POST, and a wrong HTTP/2
protocol, and refuse an unbound request when TLS token binding is required.
Library and method-policy controls run with default features on all platforms;
the existing real-binary TLS target runs on Unix with the TLS feature.

Reproduce the focused histories:

```bash
cargo nextest run --test http_header_timeout_e2e -E 'test(http2_extended_connect)'
cargo nextest run --all-features --test http_header_timeout_e2e --test tls_deployment_boundaries_e2e -E 'test(http2_extended_connect)'
```

The route repair left token-binding v2 as a separate transport limit:
extended CONNECT does not process the HTTP/1.1 `Sec-WebSocket-Key`.
[#846](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/846)
records that follow-up. The client-key contract and evidence below address it.

This finding does not certify HTTP/2 reconnect loss, arbitrary reverse proxies,
TLS shutdown, or the legacy listener. The broader A02 audit remains incomplete.

### Session 383 physical reconnect-response loss

The A05/A06 experiment uses a real WebSocket relay between two TCP connections.
It receives the server's serialized `Reconnected`, then either forwards that
baseline or discards it before dropping both transports without a WebSocket
Close. Real unregister arms the rotated credential; the test never unregisters
the lost connection directly.

Both cells reject the spent token. In the forwarding control, the received new
credential restores the original identity. In the loss cell, the application
receives only transport failure and fresh-joins the same room and player name
under a new identity. Its unknown rotated credential remains pending. The
test uses that intercepted credential only to inspect server state, never to
recover the loss-cell application. Forcing the relay to forward the lost
baseline fails the no-application-frame oracle.

This is a contract characterization, not a new reconnect defect. It establishes
post-write physical response loss before client receipt. Pre-commit cuts,
partial writes, TLS, and authenticated recovery remain separate boundaries.
The relay and listener handles are retained and joined during failure cleanup.
Focused validation and all applicable hosted checks remain required.

Reproduce the session histories:

```bash
cargo nextest run --lib -E 'test(admission_callback_panic_releases) | test(canceled_admission) | test(ordinary_admission_callback_error)'
cargo nextest run --lib -E 'test(socket_task_failures_close_registration_and_retain_tracking)'
cargo nextest run --lib -E 'test(early_socket_supervisor_panic) | test(runtime_loss_leaves_retained_server_state_unusable)'
cargo nextest run --lib -E 'test(test_lost_reconnected_wire_response_requires_fresh_join)'
cargo nextest run --lib -E 'test(canceled_idle_report_is_not_replayed) | test(idle_omission_report_write_expires) | test(test_reliable_arrival_interrupts_stalled)'
cargo nextest run --test capacity_runner -E 'test(exact_payload_cells) | test(generator_saturation_invalidates)'
```

### Session 380 validation

| Experiment | Red or negative control | Green evidence and limits |
| --- | --- | --- |
| Close-tail matrix | Rate-limit close emitted later queued data after an abandoned write | All reasons, healthy/abandoned states; real socket payload and drop-count checks |
| Idle omission report | Timeout-bypass mutant fails corrected paused-clock oracle | Deadline test plus healthy, empty, and disabled-budget controls |
| Canceled report recovery | Recovering peer receives two identical reports | Exactly one complete report, close 4002, no later gameplay; sticky-state and prewrite controls |
| Z3 A/G | A7 fails with Direct-only support and no host; G5 fails with client 1 and server range `[2,2]` | 33 UNSAT obligations and 7 SAT controls; G6 also exhibits the supported v2/`[3,3]` clamp error |
| Rust/model crosschecks | Existing implementation tests, not new red cases | Three negotiation and Direct-endpoint tests pass |
| Snapshot/replay | Old documented algorithm clears B's endpoint on serialized v2 messages | Snapshot-last restores it; separate v3 control checks authority filtering and `replay: complete` |

Reproduce the focused behavioral checks:

```bash
cargo nextest run --lib -E 'test(close_flush_never_writes_the_queue_behind_an_abandoned_write)'
cargo nextest run --lib -E 'test(idle_omission_report_)'
cargo nextest run --lib -E 'test(canceled_idle_report_is_not_replayed_when_the_peer_recovers)'
cargo nextest run --lib -E 'test(report_write_guard_preserves_evidence_and_fences_retries)'
cargo nextest run --lib -E 'test(reconnect_replay_drops_authority_events_the_snapshot_supersedes)'
cargo nextest run --lib -E 'test(reconnect_v2_snapshot_replaces_historical_peer_metadata)'
cargo nextest run --lib -E 'test(negotiate_caps_at_server_max_without_raising_client_max)'
cargo nextest run --lib -E 'test(host_direct_rejects_missing_and_malformed_host_endpoints)'
cargo nextest run --lib -E 'test(host_direct_requires_a_valid_endpoint_and_elects_an_executable_host)'
python3 formal/z3/protocol_invariants.py
```

Production repairs change no wire fields or public signatures. Internal writer
helpers are shared between queued and idle paths. Reconnect clients following
the old scenario must apply the fresh snapshot last; the server wire stays
unchanged. Hosted checks and PR review remain required before merge.

Local validation passed eleven delivery tests, three Rust/model crosschecks,
two reconnect tests, and the 40 Z3 checks. The final recovery oracle compares
the complete report. Independent review found no remaining issue in this
repair batch. Formatting and all-target/all-feature Clippy with warnings denied
passed again after the cancellation repair. Markdown, documentation
consistency, CI-config, and hook policy checks passed.
Hook execution exceeded its one-second target;
file discovery and source scanning dominated, within the area tracked by
[#811](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/811).

### Session 381 validation

The socket histories cover both receive modes and the actual spawned writer.
Queue cases cover both protocol lanes and preserve generation barriers. The
all-class deadline matrix separately pins resident and staged reliable age.
No wire fields or public signatures change. No migration is required.

Reproduce the focused runtime histories:

```bash
cargo nextest run --lib -E 'test(test_live_writer)'
cargo nextest run --lib -E 'test(test_priority_control) | test(test_reliable_)'
cargo nextest run --lib -E 'test(reliable_sojourn_watcher)'
cargo nextest run --lib -E 'test(resident_reliable_age_bounds_every_selected_write)'
cargo nextest run --lib -E 'test(writer_deadlines_are_partitioned_by_delivery_class)'
cargo nextest run --lib -E 'test(send_batch_control_bypasses)'
```

### Unresolved hypotheses and explicit limits

- Reconnect token rotation commits at queue admission. Session 382 quantifies
  unread-baseline loss and fresh-join recovery. Physical socket-write loss still
  needs its own history before designing any duplicate-token grace period or
  acknowledgement.
- The single-home deployment and model-based subsystem contracts were read.
  The full partition, restart, interop, fuzz, mutation, and platform campaigns
  have not been rerun in this session.

## Existing issues and measurement limits

- [#678](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/678):
  encoding support and capability enforcement. Audit existing formats before
  considering new codecs.
- [#775](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/775):
  generator and oracle memory grow with workload duration and fan-out. Large
  runs cannot establish server capacity when the generator exhausts resources.
- [#795](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/795):
  macOS generator saturation. A later pass does not identify the failure cause.

The two-instance split-brain test is an executable unsupported-topology failure
catalog. Passing it does not prove multi-instance room operation. The
model-based replay and delivery tests exercise selected subsystems; their
results do not prove the complete socket-to-client transaction.

## Campaign completion

Close the campaign only after each ledger entry has a documented disposition,
confirmed defects have verified repairs, significant test claims have negative
controls, and remaining uncertainties are explicit. Record unavailable
environments and bounded experiments without extending their conclusions.

Use one reviewed repair PR per working session. Keep temporary logs and session
notes under ignored `progress/`; keep durable results, regression tests, and
issue links in the repository. Recheck relevant evidence after later changes
invalidate a prior assumption.

### F14 — Token-binding clients cannot use extended CONNECT

**Status:** Repaired. Related issue
[#846](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/846),
A02/A03.

The route repair in F13 accepts HTTP/2 WebSockets, but token-binding negotiation
still requires the HTTP/1.1 handshake key. A real mTLS client negotiates `h2`
through ALPN, offers the v2 binding subprotocol, and sends extended CONNECT with
an application key and no `Sec-WebSocket-Key`. The baseline returns HTTP 400.
This is a transport interoperability failure, not an authentication bypass.

The repair accepts `x-signalfish-token-binding-key` as the explicit HTTP/2
client input. It carries standard Base64 for 16 cryptographically random bytes
per WebSocket stream. HTTP/1.1 clients retain their existing handshake input.
The server requires exactly one key header across both names. It rejects
repeated headers, competing sources, non-text values, invalid Base64, and keys
of the wrong size. Invalid input cannot trigger a fallback to a second source.

The v2 challenge, HKDF salt and info, signature domains, certificate proof,
and shared JSON/binary sequence remain unchanged. Each stream receives a fresh
32-byte server nonce. Clients derive a new key and restart their sequence when
they open a new stream. The configuration recipe records the client and proxy
contract, existing HTTP/1.1 compatibility, and browser API limitations.

Three unit regressions fail against the baseline: application-key acceptance,
ambiguous-key rejection, and invalid-input rejection without fallback. The
repaired token-binding unit module passes all 21 cases, including the existing
JSON and MessagePack goldens. The socket regression independently derives the
client HKDF and pins the real listener boundary. All three HTTP/2 socket
regressions and four selected HTTP/1.1 controls pass.

The real listener histories verify `h2` ALPN, both production aliases, the
selected subprotocol, absence of `Sec-WebSocket-Key` and
`Sec-WebSocket-Accept`, signed authentication, room admission, and byte-exact
binary gameplay relay to another player. A signed JSON Ping after the binary
frame verifies the shared sequence frontier. Negative histories cover invalid
signatures, correctly signed sequence gaps, missing proofs, missing or wrong
certificate fingerprints, exact JSON proof replay, and binary sequence replay.
Each authenticated negative starts with room admission and a valid binary
payload plus signed Pong. Cross-stream replay reuses the TLS connection and
client key, verifies different nonces and derived keys, and refuses the old
proof. Missing and malformed client keys fail before upgrade.

Focused runtime verification:

```bash
cargo nextest run --all-features --test mtls_token_binding_e2e -E 'test(http2_token_binding_)'
```

This work does not certify arbitrary reverse proxies, browser token binding,
HTTP/2 reconnect-response loss, or the rest of A02/A03. The broader audit stays
open.

### F15 Concurrent handshake rejection consumes a source retry budget

**Availability defect; high confidence.** Issue
[#849](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/849).
Baseline: `2dcc2726`. A03/A18. A prior capacity audit recorded the wasted
source charge as a tolerated boundary. Its concurrent test proved the ceiling
and rejection metrics, but did not test the rejected source's next retry.

With an application ceiling of two and a source share of one, source A resolves
at t=0. At t=59, source B probes both windows and charges its source. A test-only
gate pauses B before its application charge. Source C takes the remaining
application slot. B is rejected, but its source charge remains. At t=60, A's
charge expires and the application has capacity; B's wasted charge still blocks
its retry. The baseline regression fails with a rejected-source count of one
instead of zero. A stronger run directly attempts B's t=60 retry and fails
with `RateLimitExceeded`. This is a controlled resolver schedule, not a deployed
incident.

The repair stores application timestamps and typed source counters in one
application window. Its entry lock covers expiry, both checks, and the charge.
No rejection charges either dimension. Source counters are removed with their
last expired timestamp; rejected sources create no counter. The source share,
application ceiling, inclusive 60-second boundary, reload history, and one
metric increment per rejection remain. Successful app-ID resolution still
spends budget before later tenant credential verification, as documented.

Concurrent callers can also sample the clock before taking the entry lock.
The old append-only deque assumes timestamps arrive in order. An older stamp
behind a newer stamp remains counted after its own expiry. The repair orders
these arrivals. The out-of-order regression covers the public application-only
method and the source-aware method, before, at, and after expiry. Restoring
append-only insertion is its negative control. Cleanup uses the same expiry
rule and removes source counters and empty application windows.

The retained resolver gate runs after the atomic decision and outside its
entry lock. The race oracle accepts either winning source, requires exactly
one winner, compares each source's charges with its successful resolutions,
and verifies the losing source can retry at t=60. The larger concurrent matrix
also checks every source count against successful resolutions and retains its
aggregate rejection-metric assertions. These tests do not certify every
socket schedule, authenticated tenant policy, or other A03/A18 boundary.

Focused verification:

```bash
cargo nextest run --lib -E 'test(auth::)'
```

### F16 Reference clients reuse exchange evidence after peer incarnation changes

**Client correctness defect; high confidence.** Issue
[#851](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/851).
Baseline: `0e1f1d5a`. A20.

The browser restores membership on `PlayerReconnected` but retains the peer's
completed data-channel exchange. Its sent-label checks skip traffic on the
replacement channels. A native peer clears its exchange during restore and
waits for fresh traffic that the browser does not send. Old browser receipts
can also satisfy its success criteria. Both reference clients retain this
evidence when a departed peer rejoins with the same player ID. Same-socket
`LeaveRoom` followed by `JoinRoom` supports that history.

The repair retains logical exchange debt but clears sent and received labels
for the returning peer. V3 accountability distinguishes new epochs from
repeated current or already-announced epochs. A new epoch resets exchange
even if no departure event was seen. A duplicate keeps exchange evidence and
does not create a new session-plan wait or readiness invalidation. V2 has no
epoch discriminator; a returning `PlayerJoined` resets only a previously seen,
absent member, and `PlayerReconnected` retains its existing reset behavior.
Other peers and a physical transport rebuild without a membership change keep
their completed exchanges. The native rejoin path also re-arms the existing
exchange and rebuild gates, as its reconnect path already does.

The controlled browser history drives the real dispatcher, channel callbacks,
and success check with socket and peer-connection doubles. It completes the
initial exchange, announces departure and reconnect, then creates replacement
channels. The corrected baseline fails with only the two initial sends; fresh
reliable and unreliable sends are absent. An initial fixture used the wrong
receipt event name and never triggered restore; that failure is excluded.
The final dispatcher oracle uses a virtual clock and explicit input turns.
Withholding replacement receipts keeps success pending after the release and
linger deadlines. Restoring only the send labels fails that assertion. Both
join and reconnect histories also cover a new epoch without departure, then
a repeated epoch after fresh exchange without a new session plan.

The native handler history covers both v2 and v3. It completes exchanges for
the returning peer and another peer, confirms that a duplicate live join and
departure retain evidence, then announces a returning join. The baseline fails
on the missing fresh-send obligation. The repair preserves the other peer,
clears both directions for the returning peer, and re-arms its harness gates.
Four selected native handler and exchange controls pass.

The real-process history runs the shipped server, native client, and Chromium
browser. It completes the initial exchange, holds success behind a shared
release file, drops the native socket, waits for the browser's departure
event, then restores the native identity. The baseline browser bundle opens
replacement channels but sends no fresh exchange. The native recipient's
bounded fresh-receipt wait fails. The repaired browser passes the same history
in 2.292 seconds; the final diagnostic revision passes in 2.147 seconds.
Both clients observe exactly two replacement channel opens
and exact reliable/unreliable send and receipt payloads. Identity is retained
and the token rotates. An initial setup waited for native success before its
requested reconnect; that deadlocked setup is excluded.

One later run failed during initial channel establishment, before reconnect.
Native opened both channels and sent both payloads, but received neither
browser payload. That attempt lacked consumed browser diagnostics. Subsequent
runs passed with native as offerer and answerer; those passes do not explain
or repair the failure. Follow-up
[#853](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/853)
remains open. The runtime cell now collects both initial event streams
concurrently and includes both diagnostics in its bounded failure report.

Focused verification:

```bash
(cd clients/browser && npm test && npm run typecheck && npm run format:check)
cargo nextest run --manifest-path clients/native/Cargo.toml --lib -E 'test(returning_player_join_requires_fresh_exchange)'
cargo test --manifest-path clients/native/Cargo.toml --features browser-interop --test browser_interop_e2e native_restore_restarts_browser_bidirectional_exchange
```

The runtime command requires the server, native, and browser bundles plus
Chromium; `scripts/run-browser-interop.sh` sets those prerequisites for hosted
acceptance. This evidence does not certify arbitrary network failures, all
client packages, or the remaining A20 boundaries.

### Session 392 browser channel-establishment evidence

At baseline `6910b21c`, the initial restore history and 20 further bounded
attempts pass with both client event streams retained. These passes do not
explain or repair [#853](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/853).
The unsupported DTLS extension warnings also appear in passing attempts.

Source inspection confirms that the native driver announces a remote channel
before delivering its open event. The browser stores the channel before wiring
handlers and handles channels already open at registration. The historical
failure already had both native channels open and both native sends complete.
It leaves browser bookkeeping, browser sends, transport delivery, and browser
termination unresolved.

The browser now reports a synchronous state snapshot for unresolved peers when
the P2P window expires. It records connection, ICE, SCTP, required channels,
buffer sizes, and observed open callbacks without SDP or credentials. The
snapshot does not await network work or change timeout and fallback behavior.
Responder controls exercise already-open and later-open channels, immediate
messages, repeated open events, and retired callbacks, including a queued
already-open notification retired before dispatch. The real Chromium/native
restore and crippled-ICE histories pass locally. The deliberate ICE failure
emits two peer snapshots through CLI stderr; the healthy browser emits none.
Hosted browser interop acceptance remains required. This is diagnostic progress,
not a root-cause repair or completion of A20/A22. Keep #853 open for a captured
failure.

### F17 Reference clients count invalid payloads as exchange receipts

**Client correctness defect; high confidence.** Issue
[#856](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/856).
Baseline: `f12b88fc`. A20/A22.

Both shipped clients record a receipt whenever any text arrives on a required
channel label. Their `--exchange` success decisions do not check the documented
probe fields. Malformed JSON, another sender, a mismatched channel, or a wrong
sequence can therefore satisfy a missing receive. The external process harness
checks those fields separately; that stronger oracle does not repair the client
runtime. The earlier browser restore fixture itself used non-JSON receipts and
completed successfully. Its membership-reset evidence remains valid, but it
provided no evidence of payload validation.

The native real-handler regression fails on its first `not-json` receipt. The
browser dispatcher regression fails because invalid receipts emit
`success_criteria_met`. Both regressions send invalid payloads through the
production handler before any valid receipt. The repair credits only a JSON
object with the actual remote peer, the actual required channel, and numeric
sequence zero. It preserves received-message events for all current traffic.
Whitespace, key order, extra fields, and numeric zero spellings remain valid.
The native history also rejects a valid probe from an old physical generation.
Valid probes then complete both-label exchange in each client. The browser
history retains reconnect, rejoin, duplicate-announcement, and physical-rebuild
controls with valid payloads.

This repair changes the reference clients' success decision, not server wire
formats or delivery guarantees. It does not explain or close #853. The broader
A20/A22 audit remains incomplete.

Focused verification:

```bash
(cd clients/browser && npm test && npm run typecheck && npm run format:check)
cargo nextest run --manifest-path clients/native/Cargo.toml --lib -E 'test(exchange_receipts_require_the_peer_channel_and_zero_sequence)'
```

### F18 Embedded reconnect spends a token after response receiver closure

**Player recovery defect; high confidence.** Issue
[#858](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/858).
Baseline: `d4fcdb2b`. A05/A06/A15.

The public channel-based embedder path reserves baseline capacity, restores
identity, and awaits its room snapshot builder. Closing the response receiver
while that builder is paused still returns a successful reconnect on the
baseline. The server consumes the old token even though receiver closure was
already visible before submission. Dropping the receiver also loses the
response. This differs from the documented unknown outcome after queue commit.

`DeliveryPermit::Legacy` called Tokio's `OwnedPermit::send` and unconditionally
reported an enqueue. Tokio deliberately permits sending after receiver close.
The classified WebSocket queue checks its accepting state at permit commit.
The repair retains a sender with the legacy permit and refuses submission
when its receiver is already closed. The rejected permit releases capacity.
All reserved-send callers share this check: initial room transitions,
conditional targeted and room control delivery, and phased room transactions.
Existing callers classify the refusal as a closed channel; transactions whose
state already committed retain their existing degraded-delivery policy.

The real reconnect-handler regression parks the database read inside the
baseline builder, closes or drops the receiver, and then releases that read.
It requires rejection, absent room membership and routing, no peer lifecycle
announcement, restored temporary identity, no retained rotated credential,
and a successful fresh-receiver retry with the original token. The baseline
fails the rejection assertion in nextest run
`fb690f09-cd2c-4f5b-9387-4dbba90be7ff`. A direct reserved-send matrix also fails
against the baseline. It covers both queue types, immediate and awaited
reservations, receiver close and drop, and exact healthy control receipts.
The repaired final run passes both regressions and three existing controls for
successful publication, capacity refusal, and lost post-commit responses.

These are handler and queue histories, not physical socket writes or a
`legacy-fullmesh` listener test. A receiver can still close after the final
live-state check. That race and later response loss remain ambiguous, and
applications still need fresh-join recovery after a committed response is lost.
No public type, token format, wire field, or runtime dependency changes.

Focused verification:

```bash
cargo nextest run --lib -E 'test(receiver_lost_during_reconnect_baseline_build)'
cargo nextest run --lib -E 'test(reserved_control_detects_receiver_loss) | test(losing_unread_reconnected_baseline)'
```

### F19 Reference clients admit unbounded remote ICE candidate state

**Resource-bound defect; high confidence.** Issue
[#861](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/861).
Baseline: `b477069c`. A20.

A planned peer can withhold its remote description while continuing to send
candidate signals. Both engines append every candidate to their pending queue.
The server bounds individual frames and arrival rate, but those limits do not
bound retained client state over time. After description application, neither
engine bounds cumulative input passed to its WebRTC stack.

Both baseline engine regressions accept a 4,097-byte payload that the input
budget must reject. The repaired engines enforce 128 candidate admissions,
64 KiB total UTF-8 payload, and 4,096 bytes per payload per physical link.
The budget includes JSON metadata and stack application failures. It survives
queue drain and resets on physical link replacement. Excess input reports a
fixed error and leaves accepted candidates and the live link intact.

Data-driven controls cover count and byte exhaustion, exact payload limits,
UTF-8 accounting, independent peers, replacement links, and post-description
admission. The browser control flushes accepted candidates through the engine's
actual answer path. A native real-SDP control buffers 127 JSON candidates,
flushes them through the WebRTC stack on offer application, admits one after
description, and rejects the next without renewing the budget. Real
browser/native exchange and restore remain a separate runtime control.

This finding does not establish deployed memory exhaustion, bound every WebRTC
stack allocation, or explain #853. The broader A20 audit remains incomplete.

### F20 Buffered ICE rejection stops negotiation and drops healthy candidates

**Player connection defect; high confidence.** Issue
[#863](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/863).
Baseline: `9c2fb8fa`. A20.

Both reference engines drain their pending candidate queue after applying the
remote description. The first WebRTC stack rejection aborts that drain. The
remaining candidates disappear, and the responder does not produce its SDP
answer. A candidate received after the description only fails its own signal
operation, so equivalent input has different recovery behavior by timing.

Red-first browser engine and native real-SDP regressions reproduce the
failure. Native failures report `ErrAttributeTooShortIceCandidate` on both
offer and answer paths. The repair attempts all buffered candidates once in
order and reports each rejected candidate with a fixed diagnostic. Candidate
contents and external error text are omitted. Description and answer failures
still propagate. Admission counts and byte budgets survive the drain.

Browser controls cover empty and healthy queues, rejection first and middle,
multiple rejections, later direct candidate failure and recovery, and no replay
on repeated description. Native controls use actual SDP and remote-candidate
statistics to prove that every healthy queued endpoint reaches the ICE stack
on both paths, including after rejection, and later signaling stays usable.
Existing candidate-budget regressions also pass. These histories do not prove
that rejected candidates caused the initial setup failure in #853.

Thirty frozen-baseline capture histories with both client streams retained
passed in 100.441 seconds. They did not reproduce #853. That issue and the
broader A20 audit remain open. The parallel A05/A06/A15 source review found an
aggregate reconnect-replay size boundary tracked in
[#864](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/864);
it still needs a deterministic runtime reproduction.

Focused verification:

```bash
npm test --prefix clients/browser
cargo nextest run --manifest-path clients/native/Cargo.toml --lib -E 'test(buffered_candidate_rejection) | test(remote_candidate_)'
```

### F21 Aggregate reconnect replay exceeds the outbound frame limit

**Player restore defect; high confidence.** Issue
[#864](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/864).
Baseline: `8c619e54`. A05/A06/A15.

The replay ring bounds event count, but a `Reconnected` response combines the
current snapshot, retained control history, and a rotated credential into one
frame. The reconnect path committed queue admission without checking this
aggregate size. The socket writer later rejected the frame with code 1009.
This prevented room restore even when the live roster and each event fit the
configured frame cap.

Red-first real-socket histories used a valid 4096-byte outbound cap, a
2048-byte inbound cap, and a 128-event ring. Twenty short-lived members joined
and left while one member was disconnected. The live roster stayed small;
retained replay alone was over 6400 JSON bytes. Direct v3, correlated v3, and
direct v2 reconnects closed with `1009 outbound_message_too_large` before the
repair. These tests negotiate both JSON and MessagePack game data; reconnect
controls remain JSON.

The repair applies the socket writer's recipient projection before sizing.
It counts exact JSON bytes without allocating an aggregate frame, including
the operation envelope and rotated credential. It preserves the full current
snapshot and the largest fitting ordered suffix of filtered history. A byte
omission reports `truncated` to v3 recipients; v2 gains no new field. If the
snapshot alone cannot fit, the builder rejects before queue admission through
the existing identity, membership, and credential rollback path.

The three socket regressions pass after repair. They check frame bytes,
intact roster, ordered nonempty suffix, honest status, and a subsequent Pong.
Boundary controls compare the counting serializer with actual JSON, including
UTF-8 and escaped characters. They check complete history at the exact cap,
truncation one byte below it, the largest suffix at its exact cap, empty replay,
an oversized newest event, and snapshot refusal. Direct and correlated
lifecycle controls exercise refusal after credential preissue and retry.

This repair does not make queue admission a physical delivery acknowledgement.
A later socket failure can still lose a committed response or its rotated
token. It does not finish the physical pre-commit or partial-write audit, #853,
or the broader A01–A22 campaign.

Focused verification:

```bash
cargo nextest run --test reconnection_replay_e2e -E 'test(frame_limit)'
cargo nextest run --lib -E 'test(reconnect_replay_budget) | test(reconnect_oversized_snapshot_rolls_back_rotated_token_and_allows_retry)'
```

### F22 Known socket close before reconnect response commit consumes the retry token

Confirmed defect. Issue [#867](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/867).
Baseline: `7ac4e757`. A05/A06/A15.

The production handler reassigns the socket identity before it builds the
reconnect baseline. The writer closes the response queue only after it observes
the socket close signal. A close pinned during that baseline build therefore
left the queue accepting. The reconnect committed and consumed the original
credential despite the already-terminal socket.

The deterministic history pauses the database baseline builder after identity
reassignment. It pins the actual socket close signal while keeping the response
receiver alive, then resumes the builder. The baseline returned success on the
old source. Red nextest run `accddd67` failed the restore-refusal assertion for
`ActivityTimeout`.

The repair holds the close watch read guard only during synchronous queue
admission. A close that wins cancels admission; a commit that wins retains its
existing semantics. The shared initial-transition seam covers reconnect,
player join, and spectator join. The default coordinator fallback fences its
send too. Neither guard spans asynchronous routing updates or a close request.
The shutdown commit gate keeps its existing order. A canceled reserved attempt
is counted once and releases its queue capacity.

The reconnect control covers legacy and classified queues, direct and correlated
requests, and activity timeout, slow consumer, and unregister close reasons.
Each case checks transient-identity restoration, restored membership and route
removal, no peer announcement, provisional-token removal, and a successful retry
with the original token. Green nextest run `0f188b88` passed all twelve cases.
The coordinator control covers synchronous and asynchronous builders on both
queue types, with exact cancellation accounting, no baseline or route, and
released capacity. Its positive cells retain a committed response after a later
close. The fallback control also cancels a close pinned by its builder.

This is a known server-close history before queue admission. It does not test
physical partial writes, TLS, authenticated recovery, or unknown transport loss
after admission. Queue commit still does not acknowledge client receipt. The
broader A01–A22 audit and #853 remain open.

Focused verification:

```bash
cargo nextest run --lib -E 'test(socket_close_during_reconnect_baseline_build_preserves_retry_token)'
cargo nextest run --lib -E 'test(initial_registration_close_before_commit) | test(default_initial_registration)'
```

### F23 — Duplicate data-channel labels replace a working channel

Baseline: `4bb7fdad`. A20.

Both reference clients store data channels by label. A peer can announce
another channel with the same label in the same physical peer connection.
The old implementation replaced the selected channel without resetting its
open or exchange state. The browser then suppressed original-channel messages
through its object-identity guards. Native events carried only the peer-link
generation and label, so duplicate-channel events affected the selected pair.
[#869](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/869)
records this finding.

The repair keeps the first accepted channel for each label. A repeated
announcement of that same object is a no-op. A distinct duplicate closes
without installing event handlers. Native remote-channel polling starts only
after admission, so refusing storage also refuses its open, message, and close
events. Retiring the physical link permits a fresh generation to accept new
channels. Both locally created and remotely announced channels use this rule.

This finding is separate from the initial browser/native establishment failure
in #853. Twenty focused capture histories passed in nextest run `341866de`
(82.258 seconds), with both event streams retained. Source edits overlapped the
build, so this is not a frozen-baseline cohort. No new initial-establishment
failure was captured. These passes do not explain or repair #853.

At the F23 baseline, distinct optional labels had no application-level channel-count bound.
[#870](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/870)
records that resource audit. The stack's effective limits and memory costs have
not been measured. This repair does not certify that separate boundary.

Browser and native regressions first failed when a duplicate replaced the first
`reliable` channel. The green controls cover initiator and responder channels,
required and optional labels, repeated original announcements, closed-label
retention, and fresh-link reuse. The real native pair receives duplicate
announcements in both directions and for both required labels. It checks
original-message delivery, duplicate closure without client callbacks, and
original-channel close delivery. Nextest run `89a19936` passed the two new
regressions and the existing detached-statistics control. Browser tests,
typecheck, formatting, and strict native lint pass. Hosted checks remain the
publication gate.

Focused verification:

```bash
npm --prefix clients/browser test
cargo nextest run --locked --manifest-path clients/native/Cargo.toml --lib \
  -E 'test(duplicate_remote_channels_preserve_first_channel) | test(live_duplicate_channels_do_not_publish_callbacks)'
cargo nextest run --locked --manifest-path clients/native/Cargo.toml --lib \
  -E 'test(detached_selected_pair_probe_observes_before_live_channel_closes)'
```

### F24 — Unsupported remote channels grow client-owned state

Baseline: `cee7bd9b`. A20. Follow-up: #870.

Both reference clients retained each distinct remote label. The browser also
installed open/message handlers. Native admission started a polling task and
queued every announcement before the orchestrator inspected it. Neither
reference client uses extra labels for gameplay. Required channels are
`reliable` and `unreliable`; the server relays signaling without imposing this
client channel contract.

Red-first browser regression: the first unsupported channel remained open.
Red-first native regression: the first unsupported announcement entered the
unbounded engine event queue. These are direct admission failures, not
measured process exhaustion or evidence about #853.

The repair closes unsupported remote channels before browser retention or
native event-queue admission. Native storage also refuses direct unsupported
calls. Client-owned channel maps retain at most two fixed labels per link.
Rejected channels install no browser callbacks and start no native polling
tasks. Required-label duplicate admission keeps the F23 rule.

Boundary controls use empty, case-mismatched, NUL-suffixed, 4096-character
Unicode, and 128 distinct optional labels before required admission. Browser
controls repeat the burst after required admission, for both roles and both
connecting/open announcements. Native live peers offer 18 optional labels
and two required-label duplicates in each direction after connection, then
exchange required-channel traffic. The generator allocates the full workload
before closure and verifies 20 distinct wire stream IDs so faulty SID reuse
cannot lose offered opens (#872).
The direct controls check closure, no optional event delivery, the two-channel
bound, required delivery, map teardown, and fresh-link admission. They do not
prove retired stack or polling-task lifetime bounds.

Migration: reference-engine callers that need extra channels must own that
handling. No server wire or signaling change is introduced. WebRTC allocates
incoming channel objects before these callbacks run. This repair does not
bound stack memory, required-label announcement floods, or application message
queues. Keep general A20 resource coverage open.

The initial sequential close/reopen generator exposed a stack defect: after the optional burst
and a refused reliable duplicate, the next unreliable duplicate never reaches
remote announcement. Three failures include one SCTP trace. The trace shows
stream ID 5 reused for the new OPEN, then unregistered by a late reset, followed
by `SctpHandler.handle_write got error: Stream not existed`. The allocator
frees a locally closed SID before the reset exchange completes. An isolated
no-reuse source probe passes the same history in both directions (4.449 s),
which supports the reuse cause. It is not a production repair: permanently
reserving retired IDs can exhaust the SID space. The first probe failed to
compile with an LLVM output-memory error; a single-job, nonincremental build
completed and ran the control.
[#872](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/872)
owns this reopening failure. The corrected admission oracle keeps every
offered channel on a fresh SID and still requires remote closure and required
delivery. It does not claim closed-stream reopening support. #870 remains
open for stack resource and lifetime measurements.

External-client assessment: current Rust and .NET READMEs leave peer networking
to the game; this reference-only restriction changes no signaling or relay
surface for them. Godot delegates channels to `WebRTCMultiplayerPeer`.
[Godot #355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355)
tracks its actual channel contract, resource bounds, and intended interop.
Do not infer that Godot's engine channel names must match reference labels.

### F25 — Retired native channel tasks retain peer state

Baseline: `06016fb6`. A20. Finding: #874, under #870.

The native engine discarded channel polling task handles. Runtime handle drop
detaches a task. Each pending poll holds its channel, which holds the peer core
and channel event senders. Local peer closure stops the driver without ending
these polls. Each retired link can retain two tasks and both channel objects.

The red regression uses a real unconnected initiator and weak references to
both required channels. Peer removal succeeds and empties the map, but the
channel objects remain alive past the one-second yield deadline. The failure
is object retention evidence, not a measured process memory slope.

The peer link now owns at most two polling task handles. Removal and failed
pairing abort them before awaiting peer closure. Link destruction also aborts
them, including engine destruction and cancellation during teardown. This
does not depend on a stack close event. The green control covers local and
remote admission, three remove/re-pair generations, and engine destruction.
Each retired channel object must be released before the next generation.
The live duplicate-channel history preserves bidirectional required delivery
and the original close callback.

No server wire, channel labels, or client API changes are required. The Rust
and .NET sibling SDKs leave peer networking to the game. Godot delegates it
to its engine; [Godot #355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355)
continues its separate resource assessment. #870 remains open for stack
allocation and required-label announcement floods. #872 still owns SCTP
reset completion and closed-stream reuse. This repair proves poller-owned
channel release, not total WebRTC stack memory reclamation.

### F26 — Required-label announcements grow the unread native queue

Baseline: `009a45cb`. A20. Finding: #876, under #870.

Native callback admission rejected unsupported labels but queued every
`reliable` and `unreliable` announcement. The orchestrator rejected duplicates
only after reading them. A stalled consumer could retain many channel objects
despite the two-entry final map. The browser checks duplicates synchronously
and has no equivalent announcement queue.

The red regression creates real channels on an unconnected peer and invokes
the native callback 32 times for each required label without draining events.
The queue contains 64 announcements instead of two. This is retained-object
evidence, not a measured process exhaustion incident.

The repair shares two identity slots between callbacks and storage. Selection
runs before queue admission. Initiator channels reserve their slots before
offering. Repeated callbacks for the same object are ignored; distinct
duplicates close without an event. Queue drain and selected-channel closure
do not renew admission. Weak references preserve identity without retaining
channel objects. A failed event send closes the unowned channel.

Controls cover unread bursts for both roles across three physical generations,
concurrent callbacks, repeated selected objects, closure before replacement,
queue-receiver loss, direct admission, and retired-object release with the old
handler still alive. The live bidirectional control waits for each sender's
close event before verifying original required-channel delivery. It now forbids
duplicate announcement events as well as duplicate lifecycle callbacks, and
retains the distinct-stream workload check that excludes #872 reuse failures.

The change affects only the native reference engine's announcement admission.
No server wire or client API changes are required. Rust and .NET SDKs leave
peer networking to the game. Godot delegates channels to its engine;
[Godot #355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355)
continues that separate assessment. #870 remains open for full stack resource
bounds and application message queues; #872 reset/reopen and #853 initial
establishment remain unresolved. A bounded announcement queue does not prove
total WebRTC memory safety.

### F27 — Incoming data-channel messages grow client application backlogs

Baseline: `4c006627`. A20. Finding: #878, under #870.

Native channel pollers put every decoded message into an unbounded engine event
queue. Browser callbacks retain every decoded message in the orchestrator's
serialized promise chain. A channel-map bound does not bound those payloads.

Red evidence: a real local native pair sends 257 ordered reliable messages
while the receiver event queue is unread. The close marker ends observation;
all 257 messages arrive in order, exceeding the expected 256-entry limit.
The browser callback barrier also fails with 257 retained messages instead of
256. A separate deferred-output control shows that releasing capacity before
the browser's stdout bridge acknowledges delivery moves the backlog downstream.

The repair limits each raw and decoded message to 64 KiB, and pending decoded
messages to 256 entries and 1 MiB across peers and physical-link generations.
Native events own count and byte permits until processing or discard. Both
pollers share a serialized rejection latch. Browser callbacks reserve before
queuing and release in `finally`, after stdout acknowledges the message line.
The CLI acknowledges the write callback, including stream backpressure, and
fails the run on stdout errors. Admission failure reports one terminal error,
closes the failed link, and uses the existing relay-fallback path. Payload
capacity does not prevent a terminal notification from entering the queue.

Focused controls cover exact count and byte ceilings, empty messages, decoded
UTF-8 expansion, both native channel pollers, retained old-generation messages,
stale rejection callbacks, closed receivers, admitted FIFO traffic, fresh-link
recovery, healthy-peer progress, deferred browser output, and output errors.
The native fallback control checks the actual disconnected `TransportStatus`
frame on the signaling socket. Browser controls check fallback after a connected
pair fails. Existing live admission controls preserve both required channels.

The first native oversized-wire experiment timed out without an application
callback. rtc 0.21 defaults to a 65536-byte maximum SCTP message size and sizes
its internal message read buffer to that limit, so that history
does not test the application guard. The raw-size guard has a direct boundary
control; a 22000-byte invalid UTF-8 binary message reaches the live callback
and exceeds the decoded cap. An early concurrent test closed the sender before
receipt and lost offered work. The corrected history waits for receive rejection
before closure, then waits for both real polling handles to finish. Neither
timeout was accepted as a passing observation or repaired by a longer deadline.

**Migration:** Chunk reference-client messages above 64 KiB and consume output.
Native library consumers must keep the message's `queue_permit` through handling
and process `ChannelReceiveRejected` as a failed link. Server wire formats and
opaque relay payloads do not change. Rust and .NET SDKs leave peer networking
to the application. [Godot #355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355)
tracks its mesh receive limits and reference-client compatibility.

These controls bound decoded data-channel application traffic, not total
WebRTC or process memory. Browser WebSocket input and ordinary diagnostic output
have separate queue paths repaired in F28 (#879). #870 remains
open for full stack retention. #872 reset/reopen and #853 initial
establishment remain unresolved; the broader A20 audit is incomplete.

### F28 — Browser input and output retain work during stdout stalls

Baseline: `42b732e5`. A20. Finding: #879, under #870.

The browser buffered every unread handshake frame and appended every streaming
frame to a promise chain. Ordinary diagnostic and relay events entered the
page/CLI bridge without count or byte limits. Deferred output could move the
backlog downstream instead of applying backpressure.

Red evidence: 257 ordinary events enter an unacknowledged bridge. A handshake
burst reaches a later protocol error instead of refusing the receive backlog.
The real Chromium regression stops reading stdout after `room_joined` and
sends ordered 64 KiB relay payloads. The pre-fix bundle from `42b732e5` does not
exit within the unchanged 15-second process gate (run `b0aa0d82`). The repaired
bundle reports receive backlog overload and exits 3 (run `1f74dfd0`, 3.959 s).
The stub remains open until the client closes it and verifies more than 256
actual sends. These histories prove retained work, not process exhaustion.

The browser reserves before parsing or queuing: 64 MiB per server frame,
256 pending frames, and 128 MiB retained wire bytes. The per-frame limit matches
`config::defaults::MAX_OUTBOUND_MESSAGE_SIZE`; the default outbound cap is
8 MiB, independent of the 64 KiB inbound cap. Thus large room and reconnect
snapshots retain the server's portable contract. Credits follow active pulls,
early membership arrays, phase transfer, streaming processing, and stdout
acknowledgment. A binary view charges its backing buffer. Callback tasks have
a separate 512-entry and 128 MiB signal budget. Fatal admission interrupts
handshake output waits, stops later work, and closes the socket and peer links.
Suspended pairing and signaling handlers check session and link identity before
resuming native peer operations. Red controls reproduce a second peer and SDP
mutations after teardown; the guarded histories stop both.

Both page and CLI output reserve 256 events and 128 MiB serialized UTF-8 bytes,
including newlines. This permits base64 expansion of a maximum-size opaque
frame. The page reserves one terminal error with a 4096-byte cap. Its bridge flag
bypasses ordinary CLI admission. The CLI reserves distinct 4096-byte slots for
a terminal error and its own `exiting` event. A CLI-owned stdout failure
reserves the error before starting teardown; it does not wait for a later page
callback. The page and CLI failure reports share the same error slot.
CLI stderr retains at most
256 lines and 1 MiB; individual diagnostics truncate after 16000 UTF-16 units.
Stream callbacks own credits through consumption. Output closure or overload
fails the run with exit 3. Success flushes required output. Chromium teardown
runs beside a one-second final-output gate; the hard watchdog does not wait
for output. A full pipe can prevent the terminal line from being observed;
the nonzero process exit and bounded teardown remain authoritative.

Controls cover stalled authentication, room entry and baseline transfer;
sequential early membership retention; exact frame/count/byte boundaries;
small views with oversized backing buffers; stale callbacks; large snapshots;
v2/v3 JSON and v3 rkyv/protobuf FIFO; actual Node stream backpressure, closure,
capacity recovery, and the CLI diagnostic configuration. A held Writable
control proves the page error and CLI exit-code-2 event bypass a full ordinary
queue in FIFO order without changing the exit decision. Real Chromium/native
restore, v2 relay, both opaque relay formats, mid-handshake closure, and
signal/reaper controls pass in run `7b330ada` (six tests, 15.912 s).

**Migration:** Drain stdout and stderr continuously. Handle exit 3 as a failed
run and start a fresh client. The change belongs to the browser reference
driver, not the server wire. Rust, .NET and Godot do not use its page/CDP/CLI
queues. Their SDK behavior is unchanged. Godot's separate engine and interop
assessment remains in [Godot #355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355).

These are application retention limits, not a 128 MiB process-memory promise.
Decoded objects, Chromium WebSocket buffers, CDP and WebRTC stack retention
remain separate resource boundaries. #870 and the wider A20 audit remain open.
Issues #872 (reset/reopen) and #853 (initial establishment) remain unresolved.

### F29 — Native stdout blocks the watchdog and hides required output failures

Confirmed defect. Issue [#882](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/882).
Baseline: `d467965d`. A20, under #870.

The native emitter wrote and flushed stdout synchronously on the orchestrator
executor. An unread pipe could block both the client and the binary watchdog.
A closed pipe instead suppressed subsequent events while the run continued.

The child-process history holds authentication after the connected event. A
1 MiB downgrade notice fills unread stdout. Both runtime modes remain alive
past an eight-second observation with a two-second watchdog on the old source.
A second history closes stdout before a valid authentication reply. A third
holds output after the room baseline and sends 512 ordered 64 KiB relay frames.
The stub keeps its socket open, so an unrelated server close cannot end the run.
Fixture frames are decoded with the production message types before use.

The repair moves serialized JSONL writes to one process writer thread. Its
queue admits at most 256 ordinary events and 128 MiB of serialized UTF-8,
including newlines and the in-flight write. Bounded serialization refuses a
line before it exceeds remaining byte capacity. Charges last through flush or
failed-write cleanup. Overload and failed output end the run with connection
error 3. The owner reserves two terminal lines of at most 4096 bytes each.
Terminal events stay behind admitted ordinary events. Final flush waits are
bounded to one second each, and successful exit requires required output to
flush. The watchdog keeps exit code 4 when output is blocked.

Held-writer controls pin in-flight count and byte boundaries, Unicode and JSON
escaping, capacity renewal, FIFO output, reserved terminal slots, duplicate
finish suppression, and write/flush failure. A broken system write cannot be
canceled by Rust; one bounded process writer thread can remain blocked until
the pipe drains, closes, or the process exits. This is an explicit library
lifetime boundary. Stdout is a required output interface, so callers must drain
it continuously and inspect the process exit code when terminal lines cannot
arrive. See the native README migration note.

The Rust, .NET, and Godot SDK README assessments show separate SDK/event and
engine paths. They do not consume this native reference executable's stdout
writer. Their wire schemas, negotiation, ordering, and reconnect rules do not
change. Godot #355 remains its separate engine assessment. Browser output
continues to use its own driver.

This slice bounds native application-owned stdout retention and tests process
liveness under real pipe pressure. It does not establish stderr, OS pipe,
WebSocket, or WebRTC stack resource bounds; repair SCTP reset/reuse (#872); or
explain initial browser channel establishment (#853). The broader A20/#870 and
A01–A22 audits remain open.

Focused verification:

```bash
cargo nextest run --locked --manifest-path clients/native/Cargo.toml --test output_e2e
cargo nextest run --locked --manifest-path clients/native/Cargo.toml --lib -E 'test(event_output::tests)'
```

### F30 — Native synchronous stderr logging strands the watchdog

Confirmed defect, repaired in this slice [#883](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/883).
A20, under #870. Found during F29 from `d467965d` plus its stdout repair.

A real native current-thread process uses a two-second watchdog. Stdout drains
to `/dev/null`; stderr stays open and unread. A loopback WebSocket stub sends
one valid `AuthenticationError` with `INVALID_APP_ID` and a 1 MiB error string,
then keeps the connection open. The process remains alive after eight seconds
and requires an external kill. The binary installs a synchronous stderr
tracing writer, and `client::run` logs the complete fatal error on its executor.
This history is independent of the stdout writer. Both runtime modes reproduce
the eight-second stall with the corrected permanent regression on `ef68e7e1`.

The binary now admits at most 256 diagnostic records, including formatting,
queued work, and the in-flight write. Each record reserves at most 4 KiB,
so admitted record text storage is at most 1 MiB. A full or failed queue discards
new diagnostics. Long records end with `[truncated]` at a UTF-8 boundary;
embedded line breaks are escaped. The worker alone writes and flushes stderr.
Shutdown waits at most one second outside Tokio. Required stdout failure
semantics and protocol exit codes remain unchanged.

The default tracing formatter builds an unbounded thread-local string before
`MakeWriter` runs. The repair formats directly into admitted bounded records.
Cached span fields have a separate 4 KiB text limit across repeated updates.
The tracing writer is a sink, with internal-error logging disabled: formatter
or write errors cannot fall back to synchronous `eprintln!`. The binary routes
CLI errors and panic hooks to the same worker. An outer unwind handler gives
main-panic diagnostics one second to flush before exit 101. Enabled
`RUST_BACKTRACE` output remains bounded, independent of `RUST_LIB_BACKTRACE`.
The hook admits at most one unsymbolicated stack. Symbol resolution runs on
the worker, and truncation stops cooperative formatters. Captured-stack and
symbol-resolution allocations are outside the record-text bound.
WebRTC and RTC production log
sources use the installed tracing/log subscriber; their direct print examples
are documentation, not runtime calls. Library callers own their subscribers.

Permanent child-process controls keep stdout healthy and stderr unread or
closed. They require the complete fatal error and exit 2, sustained Ping/Pong
diagnostic production followed by watchdog exit 4 in both runtime modes,
and bounded CLI-error exit before runtime startup. Unit controls hold a write
in flight, saturate formatting/queue admission, release capacity, fail writes,
check UTF-8 and record boundaries, and bound cached span updates. A separate
child-process panic control delays a healthy stderr reader behind a queued
backlog, requires complete admitted records and the panic frame in enabled
backtrace text, and
checks unread and closed stderr. Its prior-hook control loses the panic text.
A held worker formatter proves the flush wait expires before formatting
finishes. A cooperative formatter control fails under the prior truncation
behavior because it still walks the entire discarded tail.

**Migration and client assessment:** Drain stderr for complete diagnostics.
Use stdout and exit codes for outcomes. Rust and .NET SDKs use separate library
event paths; Godot uses its engine path. None consumes the native CLI's stderr,
and server wire behavior is unchanged. Godot's separate assessment remains
[Godot #355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355).

One blocked OS write can retain one worker until pipe/process closure. This
slice does not bound OS buffers, WebRTC stack memory, live span count, or
external formatter allocations. Keep #870 and A20 open.

```bash
cargo nextest run --locked --manifest-path clients/native/Cargo.toml --test output_e2e -E 'test(stderr)'
cargo nextest run --locked --manifest-path clients/native/Cargo.toml --lib -E 'test(diagnostic_output::tests)'
```

### F31 — Spectator detach recovery loses terminal events

Baseline: `c411ee51`. A10. Finding:
[#887](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/887).

**Invariant:** A committed spectator removal completes the local role transition
and publishes its terminal acknowledgement, room roster event, and replay entry,
subject to drain and connection closure. Recovery must keep the room event order
and advance the delivery generation once.

The existing detach panic controls checked storage, local role, and generation.
They did not check notifications. Red-first controls at both injected completion
points received zero terminal room events. A coordinator registration panic
after the local generation change also made the old compensation advance the
generation twice. These are recovery-path failures; no ordinary request that
causes such a panic has been established.

The repair completes local state inside the original room and lifecycle guards.
It separates the local generation change from coordinator registration and keeps
the exact advanced handle for one retry. An exhausted error or panic closes the
connection; the captured room departure event still proceeds through the
ordinary FIFO publication and replay path. The missing-room branch uses the same
completion rule. The obsolete marker-based compensation is removed.

Permanent controls check both original panic points, coordinator panics before
and after registration, retryable errors, and exhausted errors or panics. They
require the correlated acknowledgement through the advanced classified handle,
one room event and replay entry, one generation change, clean stored/local
roles, and a working rejoin. Exhausted publication closes the connection and
queues no stale acknowledgement. A missing-room control checks the same
correlated exit. A barrier-driven cancellation history checks exact leave-before-
join order in live room events and replay; drain controls suppress notifications
while storage converges.

These controls cover handler and queue behavior. They do not prove physical
socket receipt, unrelated publication-panic recovery, or the full A10 lifecycle.

```bash
cargo nextest run --lib -E 'test(server::spectator_service::tests)'
```

External validation remains open in
[Rust #286](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/286),
[.NET #137](https://github.com/Ambiguous-Interactive/signal-fish-client-dotnet/issues/137),
and [Godot #356](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/356).
The clients consume the existing terminal events. No schema migration is required.
The broader A10 and A01–A22 audits remain incomplete.

### F32 — Stale reconnect expiry cleanup deletes a fresh membership

Confirmed defect. Issue [#890](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/890).
Baseline: `471b6be8509e4f1397abfcd6f07609e20a75235c`. A05/A07/A16.

`cleanup_expired_reconnections` snapshots expired, unclaimed records before
removing their durable room seats. A fresh same-room join discards the pending
record, but the sweep did not recheck that record or enter the room mutation
lane. Its stale candidate could therefore remove the fresh seat and authority
while leaving the live connection assigned to the room. The sweep then reported
zero removals because the old record was already gone.

The deterministic history reuses the established still-registered connection
and pending-record residue boundary. Both joins deliver `RoomJoined`. The
actual sweep is split at its candidate snapshot, then resumed after the fresh
join. Red nextest run `6475f10a-f5fc-45de-a9f1-85a2a2b8d3ab` failed at
`stale expiry must preserve the fresh seat`. This establishes that state
transition; it does not establish a remote incident or every socket history.

The repair enters the room mutation lane and excludes a currently routed
player. It then rechecks the room, claim state, and deadline while holding the
replay-state write lock through durable seat removal and record consumption.
A replacement record cannot appear during that storage suspension. Failure or
cancellation retains the record for retry. Existing public manager APIs stay
available; the server sweep uses the guarded transaction.

Final nextest run `92410296-7b1b-4b1f-af4e-e7cd09dd3acf` passed nine
focused unit tests. The fresh-membership history checks its seat, explicitly
claimed authority, readiness and game start, then disconnects and claims its
new token. Other controls cover an expired replacement record in another room,
unexpired and claimed records, wrong-room and missing candidates, replay and
metric retention, replacement during suspended storage, failure, and
cancellation. A paused membership restore proves same-room cleanup waits for
the room lane, then removes its expired sibling after reconnect succeeds.
Independent adversarial review found no remaining issues in this repair.

After moving the unchanged reconnect tests to a separate test file, all 49
owning reconnect-module and focused server tests passed. The formatted source
reconstruction matches the original after removing five redundant test-only
attributes. Strict all-target, all-feature Clippy and all 38 panic, timeout,
and clock policy tests pass on the final layout.

Focused reproduction:

```bash
cargo nextest run --lib -E 'test(stale_reconnection_expiry_snapshot) | test(expired_membership_cleanup)'
cargo nextest run --lib -E 'test(reconnect_claim_survives_expiry_during_membership_restore)'
```

The storage operation runs under the replay-state lock. The shipped in-memory
backend does not reenter the reconnection manager from that operation. This
bounded experiment does not establish callback reentrancy for custom backends.

External client acceptance is tracked in
[Rust #287](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/287),
[.NET #138](https://github.com/Ambiguous-Interactive/signal-fish-client-dotnet/issues/138),
and [Godot #357](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/357).
Their own validation remains separate from server regression and CI evidence.

### F33 — Native selected-path diagnostics omit a learned remote address

Confirmed diagnostic defect. Issue [#892](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/892).
Baseline: `bb1936f2eebd5486a667397f88427e5921650476`. A20/A22.

The hosted native IPv6 cell completed both channel exchanges, but one client
reported a local IPv6 host and an addressless remote `prflx` candidate. The
other client reported host/host IPv6. The strict family gate failed because
that report did not prove the remote address family. Ten unchanged local
attempts passed; they did not repair or explain away the hosted observation.

The rtc 0.21 statistics registry records signaled remote candidates. A candidate
learned from an inbound ICE check can remain nominated without an entry in that
registry. A later signaled host at the same endpoint is a distinct candidate:
the agent's equality rule includes candidate type. The old diagnostic inferred
`prflx` from the missing registry entry and emitted a null address. The typed
ICE transport API exposes the actual nominated pair, including its types and
addresses. The repair reads that pair directly. It does not infer an address
from advertisements, relabel a peer-reflexive candidate, or fabricate evidence
when no pair is nominated.

The IPv6 cell now requires a local host and a remote host or peer-reflexive
candidate with two concrete dialable IPv6 addresses for the planned peer.
Every local advertisement must be an IPv6 host. The prior host/host-only type
rule rejected a legitimate direct peer-reflexive path even when its actual
address was known. The deterministic red fixture supplied that exact address
evidence and failed at the old host/host assertion. The revised oracle rejects
missing or null addresses, IPv4, unspecified, multicast and link-local
addresses, server-reflexive and relay types, another peer, missing nomination,
and missing or non-host advertisements. The existing baseline alone retains
compatibility with legacy addressless reports. TURN still requires relay/relay.
Exchange, fallback, and deadline checks remain unchanged.

Focused producer and IPv6 oracle tests pass. The producer preserves nominated
candidate types and addresses, including peer-reflexive and relay types, and
returns no evidence without nomination. These controls cover diagnostic and
oracle semantics. They do not establish a new platform or capacity claim.

Focused reproduction:

```bash
cargo test --manifest-path clients/native/Cargo.toml --locked --lib nominated_pair_evidence_preserves_peer_reflexive_addresses_and_types
cargo test --manifest-path clients/native/Cargo.toml --locked --test interop_e2e ipv6_direct_oracle
```

All 19 native interop tests passed locally, including the live IPv6 cell.
Strict native all-target Clippy and both owning CI acceptance guards pass.
This changes only the native reference client's diagnostic event producer.
The signaling wire format and server API remain unchanged. External Rust,
.NET, and Godot clients are unaffected by F33; their acceptance issues above
track the separate F32 server repair.

### Session 407 mesh exchange diagnostics

Open issue [#889](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/889)
records an initial three-player mesh failure with one missing inbound exchange.
The retained creator tail did not identify the missing channel. The harness
waited for the creator before reading the other clients. A creator failure
therefore prevented collection of the other endpoint streams. Successful runs
moved the logs before the process guard saved them, producing empty event files.
The hosted workflow did not capture or upload those files.

Reference-client failure messages now name missing labels for each peer and
direction. The three-player harness collects all success waits before reporting
failure and retains each endpoint's log for artifact persistence. The hosted
browser job uploads endpoint logs, server diagnostics, runner output, and hashes
of the built server, native client, and browser bundles on success or failure.

This repairs diagnosis. It does not establish the cause of #889 or #853.
Unreliable probes remain one-shot and can be lost. The existing deadlines,
receipt validation, success barriers, and nonzero failure exits remain binding.
The server protocol and external Rust, .NET, and Godot clients are unaffected:
these changes apply only to reference-client failure text and its test harness.
A20 and A22 remain incomplete.

### Session 408 partial reconnect-response writes

Baseline: `40165ca6`. A05/A06/A15. The session 383 response-loss relay reads a
complete upstream frame before cutting delivery. It does not establish a
partial write by the production WebSocket writer.

The new history serves the real router over TCP through a test `AsyncWrite`
wrapper. After the replacement socket upgrades, the wrapper accepts only one
or 32 bytes of its first response, then parks the remaining write. The client
reads that exact prefix before dropping the socket. The 32-byte cell verifies
the `Reconnected` frame header, declared payload length, and payload prefix.
The client cannot decode a complete response. The partial v3 response does not
give the application its rotated credential.

The history waits for real socket cleanup and pending reconnect state. The old
credential fails. A fresh join restores the room and player name with a new
identity, exactly two members, and no old route or membership. The complete
v3 response control instead receives the rotated credential and restores the
original identity. The complete v2 response omits the credential and uses a
fresh join after disconnect. Replacement sockets cover the standalone v2 `/ws` route
and v3 `/v3/ws` route with batching enabled and disabled. Every cell drains
socket handlers and joins listener tasks, including assertion failure and
timeout paths.

This is a controlled partial transport-write history, not a new production bug.
It verifies the existing queue-commit contract: a consumed token does not
become reusable when a later write is incomplete. It does not certify every
kernel write schedule, TLS, authenticated recovery, or application receipt.
The server API, protocol, and external Rust, .NET, and Godot clients are
unchanged. A05, A06, A15, and campaign #826 remain incomplete.

Focused reproduction:

```bash
cargo nextest run --lib -E 'test(test_reconnected_partial_transport_write_requires_fresh_join)'
```

### F34 — Invalid binding aliases prevent negotiation

Baseline: `8dd7656c`. A02/A03/A18. Finding:
[#895](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/895).

Enabled token binding accepted arbitrary nonempty names after trimming checks.
The configured alias `game,binding` can never equal a client offer: the offer
parser splits each header value at commas. Required binding rejects every
connection; optional binding cannot select that alias. Spaces, separators,
controls, and non-ASCII names also violate the WebSocket token grammar in
[RFC 6455 section 4.1](https://www.rfc-editor.org/rfc/rfc6455#section-4.1).

The shared startup and public-constructor gate now accepts only nonempty ASCII
HTTP tokens. Negotiation applies the same grammar when validation is bypassed.
Invalid values are not echoed into diagnostics. Disabled values remain inert,
and the reserved namespace still accepts only exact `signalfish.tokenbinding.v2`.
Operators must replace invalid names with a legal token on server and clients.

Red validation admitted the comma alias on both construction paths: the new
configuration rejection and public-constructor tests failed while compatibility
passed. Green controls cover separators, controls, every Latin-1 byte class,
all legal punctuation, disabled settings, and reserved versions. A negotiation
control derives the key from the server challenge, verifies a signed Ping for
custom aliases, and rejects replay. This is configuration and negotiation
coverage; it does not certify every TLS or client connection history.

External Rust at `da8f2d16` offers the unchanged default v2 token. .NET at
`e467c871` constructs `ClientWebSocket` without a subprotocol offer. Godot at
`db7d30f2` uses the default `WebSocketPeer` without setting supported protocols.
Each client is unaffected: its current valid/default negotiation is unchanged,
and no message, proof, or API shape changes. Custom integrations must follow
the [configuration migration](../configuration.md#token-binding-subprotocol-names).

The constructor sweep also found oversized generated-code/frame combinations
([#896](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/896))
and a missed-tick overflow candidate
([#897](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/897)).
They require their own runtime evidence and repairs. A18 and the broader
A01–A22 audit remain incomplete.

### F35 — Generated room codes exceed signaling frames

Baseline: `52807400`. A18/A09/A20. Finding:
[#896](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/896).

Shared startup and direct constructor validation accepted a generated-code
length of 65,537 with the default 65,536-byte inbound frame cap. A real TCP
server created that room and returned its code. The shortest explicit JSON
join used 65,614 bytes. The production receive path rejected it before room
admission. Both configuration rejection tests failed on the baseline.

The shared gate now reserves the required JSON request and one-player reply
envelopes before accepting code length. The request reserves 77 bytes for
required fields and one-byte game and player names. The reply reserves the
version-specific player metadata and configured relay label, including the
maximum widths of mandatory timestamp, epoch, sequence, capacity, boolean, and lobby-state
values. V2 carries its
join timestamp. V3 carries epoch, sequence, and a UUID reconnect credential
when reconnection is enabled. Validation checks each enabled version and
configured label. Arithmetic does not overflow or allocate the configured
code length, including `usize::MAX`.

The exact inbound boundary permits a 65,459-byte prefixed code and a
65,536-byte join request. A real create/join history receives both replies.
A second real-socket control uses tight outbound limits on both protocol
versions and checks mandatory player metadata. Configuration controls cover
exact bounds, one byte over, envelopes larger than their caps, huge lengths,
and both admission paths.

Creation and regeneration share the server's validated code generator.
Explicit joins use the same configured length validator. The database's
standalone default generator uses six characters. Public low-level generator
helpers remain caller-owned APIs without frame-cap inputs; callers must bound
untrusted requested lengths before invoking them. The server never passes an
unvalidated configuration length to these helpers.

**Migration and client assessment:** Shorten rejected code lengths or increase
the named frame cap. Keep room for actual names and optional fields. Valid
custom lengths, prefixes, and default codes retain their wire shape. Rust,
.NET, and Godot clients need no implementation changes: this repair changes
server configuration admission, not client messages or negotiation.

These bounds reserve one-player mandatory envelopes, not arbitrary room
snapshots. Additional members, names, passwords, operation correlation,
connection metadata, spectators, and ICE entries still consume frame space.
The existing inbound and outbound guards remain authoritative for complete
messages. A01–A22 and campaign #826 remain incomplete.

```bash
cargo nextest run --lib -E 'test(room_code_) | test(loaded_config_rejects_generated_code)'
cargo nextest run --lib -E 'test(constructor_rejects_invalid_runtime_inputs_before_initialization)'
cargo nextest run --test e2e_tests -E 'test(generated_room_code_fits)'
```

### F36 — Delayed first cleanup ticks overflow accepted periods

**Availability defect; high confidence.** Baseline: `b1d0de7e`. A16/A18.
Finding: [#897](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/897).

`Duration::MAX` passes the server constructor and both public limiter
constructors. Tokio 1.53.2 checks ordinary interval advancement, but its
missed-tick calculation adds the period without checking the platform clock
range. A first tick more than five milliseconds late enters that calculation.

The retained experiment exhausts the normal cooperative task budget, polls
the production cleanup future until its first tick suspends, then advances
the paused clock by ten milliseconds. All three old paths panic with
`overflow when adding duration to instant`: `EnhancedGameServer` cleanup,
room rate-limit cleanup, and application rate-limit cleanup. The limiter
futures are the same private functions passed to the public spawn methods;
there is no replacement timer or cleanup implementation in the experiment.
This establishes the runtime branch, rather than assuming an OS preemption.

The shared periodic timer now checks deadline arithmetic for Burst, Delay,
and Skip. It keeps the immediate first tick, the five-millisecond tolerance,
and each caller's missed-tick policy. A deadline outside the clock range
stays pending and can still be cancelled. Skip computes the phase remainder
as a full duration, without narrowing total elapsed nanoseconds to `u64`.
Zero limiter periods retain their existing one-second floors.

The sweep replaces all six production interval sites: both limiters, room
maintenance, lease renewal, WebSocket Ping, and RelayStats. Lease renewal and
both connection tasks still consume the immediate tick before waiting their
first full period. No configuration is silently shortened or rejected.
There are no protocol, default, or client API changes. Rust, .NET, and Godot
clients need no implementation changes because their messages and timer
configuration interfaces are unchanged.

The repaired runtime controls establish a completed server sweep and shutdown,
a completed application-limiter sweep, and room-limiter sweep admission and
owner release. Other controls compare cadence directly with Tokio under
Burst/Delay/Skip and late polls, preserve cancelled-tick deadlines, bound
repeated waits after overflow, check near-clock-limit arithmetic, and check
Skip remainders wider than `u64` nanoseconds without requiring a platform to
represent a 634-year instant. Existing owner-loss, drain, lease-renewal, Ping,
and RelayStats controls cover ordinary behavior.

The failing histories were captured on Linux ARM64 with paused Tokio time.
They do not reproduce every OS scheduling history or establish the safety
of all timer-driver internals. The wider audit and campaign #826 remain open.

```bash
cargo nextest run --lib -E 'test(oversized_cleanup_period_survives_a_delayed_first_tick) | test(deadline::tests)'
cargo nextest run --lib -E 'test(cleanup_task_) | test(lease_renewal_) | test(draining_cleanup_task_)'
cargo nextest run --test v3_relay_stats_e2e -E 'test(periodic_relay_stats)'
```

### Session 412 authenticated partial reconnect-response writes

Baseline: `0ab34110`. A03/A05/A06/A15. The session 408 partial-write
history used open sockets. It did not establish recovery with signed
application credentials or tenant ownership checks.

The extended history requires Ed25519 connect tokens for two allowlisted
applications. A pre-auth join gets `MISSING_APP_ID`. A credential from another
signer gets `CONNECT_TOKEN_INVALID`; a valid credential then completes both
handshake replies. A socket authenticated for another application presents the
legitimate player's reconnect token. It gets the non-enumerating
`ROOM_NOT_FOUND` response, publishes no restored route, and leaves the pending
record available. The legitimate application then reconnects with that same
token. Temporarily bypassing the production room-owner check makes this test
fail at the wrong-application refusal. The check is restored in the final tree.

Each signed-credential history covers v2 `/ws` and v3 `/v3/ws`, with batching
on and off, and complete, one-byte, or 32-byte response writes. Authentication
finishes before the transport cut is armed. Both partial cells observe only
an incomplete `Reconnected` frame before the client drops. The committed old
token cannot be reused. The complete v3 control recovers with its rotated
token; the other cells recover by joining with a fresh identity. Fresh joins
preserve the room's application, exactly two seats, and no old membership.
Every cell drains socket handlers and joins listener tasks on success,
assertion failure, or timeout. The original open-socket history remains a
separate control.

These tests verify existing plain-TCP behavior. They do not establish TLS,
client fingerprint binding, every kernel write history, or application receipt.
No production defect was found in this scope. A03/A05/A06/A15 remain
incomplete. Campaign #826 remains open. Rust, .NET, and Godot clients are unaffected: no server
behavior, protocol, or client API changes. This is internal audit evidence and
needs no changelog entry.

```bash
cargo nextest run --lib -E 'test(partial_reconnect) | test(test_reconnected_partial_transport_write)'
```

### Session 413 certificate-bound partial reconnect-response writes

Baseline: `9097cb7e`. A02/A03/A05/A06/A15. The plain signed-credential
history did not verify TLS certificate identity after credential rotation.
The existing mTLS history checked only the initial credential and first
successful restore.

The new history uses real TCP, rustls client and server handshakes, and the
production `VerifiedClientCertificateAcceptor`. Both valid client certificates
use the same application ID. Each connection signs its requests with the
fresh server challenge and its own verified certificate fingerprint.
An alternate valid certificate cannot restore the original player. The
issuing certificate then restores that player with the same credential.

The matrix covers v2 `/ws` and v3 `/v3/ws`, batching on and off, and complete,
one-byte, or 32-byte response writes. The controlled `AsyncWrite` wrapper sits
above TLS, after certificate verification. It accepts only the selected
plaintext prefix and flushes that prefix through rustls before parking the
remaining write. Partial cells read that prefix from the real TLS client and
observe no further plaintext. The 32-byte cell identifies `Reconnected`.
These are controlled plaintext submission histories, not TLS-record,
packet-loss, or kernel-specific encrypted-write experiments.

The history waits for the committed claim to disappear before dropping the
restored socket. The original credential fails on a fresh socket with the
issuing certificate. The complete v3 cell receives a different credential,
refuses it from the alternate certificate, and restores the original player
with the issuing certificate. Other cells recover through a fresh join,
then disconnect and restore that new player with its new credential. The
alternate certificate cannot spend that credential either. Every recovery
checks the room route, exactly two database members, removal of any replaced
identity, and gameplay delivery from the restored identity to the incumbent.
Socket handlers drain and both listener tasks are joined after success,
assertion failure, or the outer history deadline. Temporarily removing the
certificate identity carry during connection reassignment makes the complete
v3 cell fail at recovery with the rotated credential. The production carry
is restored in the final tree.

This scope verifies existing behavior. It does not verify HTTP/2 reconnect,
TLS shutdown choreography, encrypted-record cuts, or every client recovery
implementation. No production defect was found here. The wider audit and
campaign #826 remain open. Rust, .NET, and Godot clients are unaffected:
there are no server behavior, protocol, or client API changes. This internal
verification change needs no changelog entry.

```bash
cargo nextest run --all-features --lib \
  -E 'test(test_mtls_partial_reconnect) | test(partial_reconnect) | test(test_reconnected_partial_transport_write)'
```

### Session 414 combined tenant and certificate reconnect recovery

Baseline: `3287fad4`. A02/A03/A05/A06/A15. The prior plain signed-credential
and TLS certificate-bound histories tested each admission gate separately.
They did not verify their composition after a reconnect response was lost.

The TLS history now has a second control with two allowlisted applications
and required Ed25519 connect tokens. Each socket rejects a credential from
another signer, then accepts a valid signed retry inside its certificate-bound
envelope. Both application handshake replies finish before the write cut is
armed. The original open-admission mTLS history remains a separate control.

A socket with a valid other-application credential and the issuing certificate
presents the owner's reconnect token. It gets `ROOM_NOT_FOUND`, leaves no
restored route or database member, and preserves the pending record. A socket
with a valid owner-application credential and another valid certificate gets
`RECONNECTION_TOKEN_INVALID`. The owner then restores with the same token and
issuing certificate. These refusals repeat for the rotated token in complete
v3 cells and for the fresh identity's token after response loss.

The matrix covers v2 `/ws` and v3 `/v3/ws`, batching on and off, and complete,
one-byte, or 32-byte response writes above TLS. The 32-byte cut identifies
`Reconnected` and checks that the declared frame extends beyond the prefix.
The old committed token fails. Complete v3 cells use the rotated token; the
other cells join with a fresh identity, then disconnect and restore it.
Recovery preserves the original application context and database room owner,
exactly two members, the restored route, and gameplay delivery to the incumbent.
Every cell drains its socket handlers and joins both listener tasks on success,
panic, or deadline. Temporarily bypassing room-owner authorization only for
certificate-bound reconnects makes the combined history fail at its first
wrong-tenant refusal. The production check is restored in the final tree.

No production defect was found in this scope. This is internal audit evidence.
No server behavior, protocol, configuration,
or client API changes. Rust, .NET, and Godot clients are unaffected. No
changelog entry is needed. The history does not verify encrypted TLS-record
cuts, HTTP/2 reconnect, kernel-specific writes, or external client recovery.
Campaign #826 and the wider A01–A22 audit remain open.

```bash
cargo nextest run --all-features --lib \
  -E 'test(test_signed_mtls_partial_reconnect) | test(test_mtls_partial_reconnect)'
```

### Session 415 encrypted TLS-record reconnect response loss

Baseline: `0c76b5cd`. A02/A03/A05/A06/A15. The prior TLS histories cut
plaintext above TLS. They did not establish recovery after the client received
an incomplete encrypted record.

A TCP relay now forwards traffic into the unchanged production TLS acceptor.
It sees ciphertext only. After signed, certificate-bound authentication
finishes, it forwards exactly 1, 5, or 32 bytes of the next response and parks
the remaining write. The shared write-cut fixture captures only the forwarded
prefix. A non-consuming peek at the client's TCP socket proves exact receipt.
The record starts with TLS application-data framing; the 5-byte and 32-byte
cells verify that the declared record extends beyond the received prefix.
Polling the real TLS WebSocket client publishes no decoded response.

Sixteen cells cover v2 `/ws` and v3 `/v3/ws`, batching on and off, and complete
response controls beside the three encrypted cuts. The history waits for the
original reconnect claim to complete before dropping the client. The relay
then closes both TCP directions. The committed old token fails; complete v3
controls use the rotated token. Other cells recover through a fresh join,
then disconnect and restore with the fresh credential. Wrong-tenant and
wrong-certificate refusals preserve the legitimate pending seat. Recovery
retains the original application owner, exactly two members, the restored
route, and gameplay delivery to the incumbent. Each cell drains all handlers
and verifies successful relay completion. Temporarily bypassing the
production claim token comparison makes the first encrypted-cut cell fail
at old-token refusal. The comparison and full matrix are restored in the
final tree.

No production defect was found in this scope. This is internal audit evidence;
server behavior, protocol, configuration, and client APIs are unchanged.
Rust, .NET, and Godot clients need no changes. No changelog entry is needed.
The relay does not prove production writer backpressure, HTTP/2 reconnect,
TLS shutdown choreography, or kernel-specific write histories. Campaign #826
and the wider A01–A22 audit remain open.

```bash
cargo nextest run --all-features --lib \
  -E 'test(test_encrypted_tls_record_loss_preserves_bound_reconnect_and_gameplay_recovery)'
```

### F37 — Native remote shutdown leaves required channels open

Baseline: `79a15aee`. A20. Finding:
[#905](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/905).

A real native pair opens both required channels, then one engine removes its
peer. With locked webrtc/rtc 0.21.0, the remaining engine receives no required
channel closure within five seconds. The red nextest run `c1b9eb11` fails after
successful establishment. The old runtime aborts its driver before queued
shutdown writes can reliably leave the socket. The old RTC channel handler
also omits closure fan-out when remote DTLS closes. This delays the reference
client's existing relay fallback and link cleanup.

The native manifest now requires published webrtc/rtc 0.21.1. The lockfile
updates their coordinated RTC packages without unrelated dependency changes.
The release gives the driver a bounded shutdown flush and closes channels on
remote DTLS shutdown ([RTC repair](https://github.com/webrtc-rs/rtc/pull/262),
[release backport](https://github.com/webrtc-rs/rtc/pull/272)). It also ends
application event streams when a driver stops. That new EOF path exposed a second integration defect: retaining an
engine generation and closing its live stack stops both pollers without any
`ChannelClosed` event. The separate red control on 0.21.1 observes an empty
closure set instead of the two required labels.

The engine now treats channel EOF, error, and closure as terminal. It publishes
one `ChannelClosed` per producer and stops polling. A receive-limit rejection
or missing application receiver returns through its existing terminal path;
EOF cannot publish another event after shared rejection. The orchestrator's
existing generation fence discards terminal callbacks from retired links.

The live remote-close history passes in both directions. It checks exact peer,
label, and generation identity, unique close events, producer completion, empty
peer maps after cleanup, newer generations on the same engine and player IDs,
and fresh reliable/unreliable delivery in both directions. A four-cell live
matrix covers EOF and ordinary channel closure, each with and without shared
receive rejection. The orchestrator control covers closure and receive
rejection: each reports `TransportStatus{webrtc,false}`, keeps the planned peer
obligation, and preserves a newer generation from an old terminal callback.

Hosted verification exposed an inconsistent netem oracle. It allowed status
transitions but counted fallback events and cumulative transport metrics as
one final state per client. The eight-player mesh failed with one fallback
instead of zero. An isolated Docker trace confirms two clients report
`true → false → true` between `exchange_ready` and
`exchange_reliable_ready`. The deliberate pair rebuild closes the old remote
DTLS link before its WebSocket retry marker can arrive. Both clients recover
before fresh reliable exchange, while all clients remain held before exit.

The oracle now matches each false report to one ordered fallback event. It
permits transient loss only in the rebuild window and requires recovery before
the reliable proof. It freezes sender status histories at the success barrier,
settles every room peer's full history, and requires exact ordered fan-out.
Both cumulative transport metrics count the validated transitions. The
production control-plane budget also counts every status report. Final graph,
fresh channel payloads, relay payloads, errors, and message conservation retain
their exact checks. Pure negative controls reject missing, duplicate, reordered,
early, late, or unrecovered reports. Hosted netem cells now retain endpoint
event streams in separate artifacts for each matrix cell.
The repaired eight-player cell passes with 31 real qdisc drops and 476 exact
signal events. Three clients exercise transient fallback and recover before
the reliable proof; this is not a clean run that omits the failing history.

This is a native reference-client repair. Server wire messages and defaults do
not change. Current Rust and .NET SDKs leave peer networking to the application;
Godot uses its own `WebRTCMultiplayerPeer`. These clients need no changes for
this repair. Graceful shutdown packets remain best effort on a lossy network;
these controls do not prove prompt detection of packet loss or process death.
The 0.21.1 reset code still permits premature SID reuse and lacks complete reset
replay handling. Keep #872, #870, #853, #889, and the wider A20/#826 audit open.

```bash
cargo nextest run --locked --manifest-path clients/native/Cargo.toml --lib \
  -E 'test(remote_peer_close_) | test(ended_channel_streams_) | test(channel_termination_)'
```

### F38 — A hostless departure retains the obsolete client plan

Issue [#907](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/907).
Baseline: `82efc9a8`. A11. This repairs a documented recovery exception; it is
not a claim that the old branch violated its explicit silence contract.

A Direct session starts with one usable host endpoint. The host replaces its
metadata with an unusable address and port through `ProvideConnectionInfo`.
Another member leaves. Departure recovery finds no eligible host and removes
the stored plan, but sends no replacement. Remaining clients can retain their
last authoritative host, endpoint, and peer obligations. A downgraded reconnect
already has empty peers; that weaker history does not establish this failure.

The regression invokes the production metadata and leave handlers, observes
`PlayerLeft`, then requires a fresh Relay/Relay reset. It fails on the old
branch waiting for that plan (run `640a7a83`). This is a controlled in-process
history with real coordinator queues, not an observed remote incident or a
TCP client experiment.

Departure recovery now shares the membership decision used by finalized joins
and reconnects. Its exact-membership transaction publishes one fresh relay
generation and removes sticky state in the same commit hook. The reset clears
host, Direct endpoint, peers, and ICE servers. It does not mint TURN credentials
or count as replacement-host election. Pure v2 survivors receive no v3 frames.
The ended session stays on relay; the selection ladder does not run again.
Routing rejection retains the old stored state for later recovery.

Final focused controls pass (run `9d366d12`), and all 83 session-policy tests
pass (run `b9332373`). These include existing replacement-host, capability,
TURN, and ordinary-departure controls. Strict all-target, all-feature lint
passes. Independent review found and corrected a stale host-failover scenario
note. Hosted verification and external client acceptance are separate gates.

The protocol and handoff guide now state this terminal reset instead of the
silence exception. Current Rust, .NET, and Godot handlers accept authoritative
relay resets. Their acceptance tests remain separate work in
[Rust #288](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/288),
[.NET #141](https://github.com/Ambiguous-Interactive/signal-fish-client-dotnet/issues/141),
and [Godot #358](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/358).
No wire schema or v2 behavior changes. These client issues remain open until
their own tests prove this trigger's cleanup and recovery.

Session CI repair [#908](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/908)
addresses a separate mutation-baseline timeout. Each old TCP reconnect test ran
twelve independent histories within a ten-second per-test limit. The signed
matrix timed out at 10.003 seconds in hosted job `114695185811` and at 10.021
seconds in a focused local run. All twenty-four cells now have independent test
processes with unchanged histories, assertions, deadlines, and cleanup. The
focused mutation-profile matrix passes 24/24; its slowest cell takes 1.309 seconds.
TLS matrices are unchanged. This is verification maintenance, not a gameplay
repair. The broader A01–A22 campaign remains open.

The first hosted rerun passes all 1,444 baseline tests in 9.520 seconds, but
manual dispatch exposes a skipped-ancestor gate defect
[#910](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/910).
The scheduled guard is skipped, which suppresses every shard through implicit
`success()`. Shards now explicitly require a successful baseline and no
cancellation. The existing workflow contract test fails before this repair and
passes after it. Duplicate scheduled heads still skip both baseline and shards.
The full mutation matrix must pass on the final PR head.

### F39 — Required data-channel labels admit incorrect delivery modes

Baseline: `6326d6a1`. A20. Finding:
[#911](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/911).

Both reference engines accepted required channels by label alone. A peer could
announce unordered or partially reliable `reliable` channels, or ordered or
unlimited-retry `unreliable` channels. These modes change gameplay delivery.
The native callback fails the negative control before repair (run `47bec625`).
The browser callback and real Chromium probe also fail against the baseline.

Both admission boundaries now check the negotiated properties before storing
a channel or publishing callbacks. `reliable` requires ordered delivery and
no retry or lifetime limit. `unreliable` requires unordered delivery, zero
retries, and no lifetime limit. Refused channels close without occupying the
required label. A valid follow-up can supply that label. Custom peers must use
these settings; the README files document this compatibility change.

A real native connection exposed a second defect in published RTC 0.21.1.
Remote acceptance discarded the DCEP retry and lifetime properties. It also
left responder SCTP streams at their ordered, reliable defaults. A repaired
getter alone would therefore report a mode that outgoing packets did not use.
The native live positive control failed before the dependency repair (run
`6799e1b7`). The signaling server does not interpret these opaque RTC settings.

The native client now uses a local copy of the published RTC package, with
registry transitive dependencies unchanged. The patch preserves remote
properties, rejects partial-mode values above the API's 16-bit range, configures
responder streams before acknowledgment, and resets refused streams. Real SCTP
controls cover all six DCEP modes, unordered DATA flags, abandonment under
packet loss, duplicate announcements, and rejected-stream reset and reuse.
The focused dependency controls pass 3/3. Native callback and live controls
pass 6/6 (run `344495b6`), including correct follow-up, traffic on both required
channels, duplicate admission, shutdown, and teardown. Browser controls pass
72 mode/timing cases and seven real Chromium refusal cells.

Independent review found no remaining issues in this patch. CI runs its focused
library controls and scans its separate lockfile with the existing security
policy. Source provenance and removal criteria are in `third_party/rtc/PATCH.md`.
Return to a registry release only after it passes these controls; track that
work in [#912](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/912).
This does not repair the separate native repeated-reset failure in
[#872](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/872).

The external Rust SDK uses an application-owned `WebRtcDriver`. The .NET SDK
provides signaling and relay APIs. Godot owns its `WebRTCMultiplayerPeer`.
This reference-engine change does not alter those SDKs or the signaling schema.
Keep #872, #870, #853, #889, and the wider A20/#826 audit open.

### F40 — Native stream reuse races unfinished SCTP resets

Baseline: `ee52a96b`. A20. Finding:
[#872](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/872).

The sequential refusal history reopened streams before the prior reset finished.
RTC emitted stream closure immediately after requesting a reset. SCTP removed
streams on the incoming reset before its outgoing reset was acknowledged.
It also discarded outgoing requests on `InProgress` and applied duplicate
requests to a reused stream. Each transition has a deterministic failing control
against the published behavior.

The repair reserves a stream until both reset directions complete. It retries `InProgress` without
exhausting the error budget, retries a temporary busy response with the same sequence number, and
validates peer request sequences. Duplicate requests replay the cached response without touching a
new stream. Reciprocal requests follow queued outgoing data. Deferred incoming resets resume after
data drains, the reader stops, or cumulative TSN advances. New-generation DATA that arrives before
the final acknowledgment stays within a bounded buffer. Transport acknowledgment continues while
generation delivery waits. Hidden reassembly applies forward-TSN abandonment before the new channel
is exposed. RTC applies the old channel's payloads and stream events in read order before admitting
the reopened channel. Generation-bound SCTP handles and event cleanup prevent an old reader or
notification from touching the new stream.

Outgoing forward-TSN reports remain within an outstanding reset's TSN boundary.
Their stream sequence entries cover only that interval. Once the reset succeeds,
the sender retires that stream's old cached sequence and ignores abandoned old
DATA when updating the new generation. This prevents an old high sequence from
skipping freshly reopened ordered messages. Other streams can still send DATA.
A fence with no retransmittable DATA pauses blocked forward recovery. Terminal
reset responses resume recovery so a lost first forward report can be retried.

The native regression creates and refuses channels sequentially in both
directions. Each direction offers empty and long Unicode labels, 16 distinct
optional labels, and repeated required-label duplicates. Receiver statistics
prove that streams are actually reused. Required-channel traffic, callback
exclusion, teardown, and a fresh-link repetition remain part of the history.
The existing distinct-stream admission control stays separate.

Both source patches use qualified copies of the pinned published packages.
Registry versions for other dependencies remain unchanged. CI runs the focused
reset controls, native suite, browser interoperability, and security checks for
all tracked graphs. Provenance and removal criteria are in
`third_party/rtc/PATCH.md` and `third_party/rtc-sctp/PATCH.md`.

The Rust SDK owns no WebRTC stack; applications provide `WebRtcDriver`. The .NET
SDK provides signaling and relay. Godot uses its engine's WebRTC implementation.
This reference-engine repair changes no server message or external SDK API.
The broad WebRTC resource audit #870, unexplained establishment failures #853
and #889, published-package qualification #912, and campaign #826 stay open.

The focused SCTP gate passes 36 reset controls, including 22 new cases and 14
retained upstream controls. The corrected native sequential test loses three
of 18 optional OPENs against the unpatched dependency; the patched history
preserves every offered OPEN, required traffic, and actual stream reuse.
The earlier failed sender-statistics oracle was corrected and is not evidence
for this defect. The RTC owning-module gate passes 27 controls. Both native admission and
sequential-reopen controls pass on the final patch. Formatting and strict native
lint pass. Strict standalone vendor test lint still reports pre-existing upstream
warnings; library lint and the consumer's strict gate pass. Hosted qualification
and exact-head review remain required before merging.

### F41 — Closed native channels retain lifetime statistics and event queues

Baseline: [release commit](https://github.com/Ambiguous-Interactive/signal-fish-server/commit/ba8867afc89c0d058957d9efee6018525e0ab0d6).
A20. Finding:
[#919](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/919).
Resource follow-up: [#870](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/870).

The RTC registry releases closed streams for reuse, but its statistics map keeps
labels and protocol strings under monotonic channel handles. Repeated channel
refusal therefore grows retained state beyond the concurrent stream limit.
Four real-handler controls fail at 65 closed summaries plus two live channels.
They cover ordered stream-close input, legacy close events, DTLS shutdown, and
DCEP timeout followed by reset. This is deterministic retained-state evidence,
not a measured production process-memory incident.

The statistics accumulator now retains all live records and the 64 most recently
closed summaries. Lifetime opened and closed totals remain cumulative. Duplicate
closure does not refresh history or recreate evicted records. A delayed-reset
control expires 96 channels before physical closure; reset completion neither
recreates the oldest summaries nor repeats closure events or accounting.
An independent control closes channels in reverse creation order and checks
that eviction follows closure order.

The WebRTC wrapper has a separate retained-event map. Its per-channel queue cap
does not bound the number of retired handles. When callback admission drops a
refused channel, its receiver disappears; queued open and close events can
remain forever. The qualified local wrapper patch releases undeliverable events
at terminal closure and rejects late events for retired, unregistered handles.
It preserves pre-registration events while the core channel exists and preserves
message-before-close ordering for registered slow readers. Disconnected sender
entries are pruned during driver flush. Refusal adds no polling task.

Review also found that the wrapper helper capped missing-receiver queues before
OPEN registration. A deterministic helper control stages 513 messages and an
early flush retains only 256 on the baseline. The repair distinguishes pending
registration from an announced channel whose receiver was dropped. It preserves
all 513 distinct messages, delivers OPEN first, and retains the orphan cap after
announcement. A small marker exists only for each live announced handle and is
removed on CLOSE. Late OPEN events for retired handles do not restore markers.
This control proves the helper contract; it is not a reproduced live TURN loss.
The SCTP read-pipeline limit also bounds the current live ingress path.

The native sequential history offers 82 optional channels in each direction on
one physical link, then repeats with fresh links. It observes each remote OPEN
before that summary can age out, keeps at most 66 channel summaries, preserves
both live required-channel records, and requires exact lifetime open counts.
The stream identifiers must demonstrate reuse. Existing duplicate refusal,
required reliable/unreliable traffic, callback exclusion, and cleanup remain
part of the history. The focused live history passes in 5.417 seconds (nextest
run `89d8836d`). The wrapper's driver controls pass, including 4,096 orphan
generations, pre-registration delivery, late callbacks, lifted open-event queues,
and registered unread message-before-close order. The baseline retired-handle
control fails at its first undeliverable event. RTC handler controls pass 14/14
and the separate closure-order accumulator control passes. Independent review
corrected a stale registration observation and lifted-event queue resurrection;
its final source review has no remaining findings. Hosted verification and
exact-head review remain required.

The closed-history bound changes `get_stats` results for older closed channels;
the native README and patch notes state this explicitly. No signaling message
or external SDK API changes. The Rust SDK uses an application-owned WebRTC
driver, .NET supplies signaling and relay, and Godot owns its engine WebRTC
stack. Those implementations do not consume this local wrapper patch.

Source provenance and published-replacement controls are recorded in
`third_party/webrtc/PATCH.md` and `third_party/rtc/PATCH.md`.
All three local patches must pass qualification before returning to published
packages under #912. This finding does not bound simultaneous channel admission,
all WebRTC stack buffers, browser/CDP retention, or whole-process memory.
Keep #870 and the wider A01–A22 campaign open.

```bash
cargo test --locked --manifest-path third_party/rtc/Cargo.toml --lib statistics
cargo test --locked --manifest-path third_party/webrtc/Cargo.toml --lib peer_connection::driver::tests
cargo nextest run --locked --manifest-path clients/native/Cargo.toml --lib \
  -E 'test(live_refused_channels_reopen_and_recover_on_fresh_link)'
```

### F42 Browser reference clients cannot join native peer-link retries

Tracking: [#921](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/921).
Baseline: `d2f2abcf`. A20.

The native reference client can opt into `PairRetry` signaling before it
rebuilds a planned physical link. The browser had neither a matching option nor
a retry handler. It classified the marker as an unknown signal and applied the
following Offer to its old peer connection. Ordinary interoperability uses no
retry extension and does not expose this gap.

A dispatcher history fails at the first incomplete-link replacement in both
server-assigned roles. A real Chromium/browser-host and native-initiator history
holds the native exchange, opens the initial pair, then requests its reserved
retry. The baseline fails after 31.922 seconds in run
`47c133b5-73e0-43c2-b434-cc26308ac915`: the browser reports an unknown signal and
neither endpoint completes the replacement. A CLI adapter removes only the
unsupported retry option for this baseline run; its page and engine are unchanged.
The baseline also separately rejects the new CLI option before room admission.

The repair adds explicit browser opt-in with `--p2p-retry-count` (default 0,
maximum 255). Valid new attempts replace only the current planned peer's link,
preserve its offerer role, and run before later signaling. Duplicate or older
attempts do nothing. Departures and new session generations clear attempt
history. The engine's existing callback generation fences exclude retired output.
A peer that never connected cannot reopen an expired establishment window or extend an
active window. A connected peer can join a coordinated rebuild after that window.
A retained successful-formation bit survives physical close within that session
generation, so an earlier channel-close callback cannot hide the later retry
marker. A new close-before-marker control fails before this addition and passes
in both roles after it. Success waits for an active retry to connect or expire. Logical exchange receipts
survive a physical rebuild, as documented before this repair.

The final dispatcher histories cover both roles, deadline expiry, stale and
unplanned markers, invalid attempts, duplicate suppression, callback retirement,
new-generation budgets, and exit behavior. The real retry history passes in
2.697 seconds; ordinary browser/native room restore also passes in 2.549 seconds
(run `8631ed70-c6ac-43ee-bbf9-15df67f07a9c`). Each retry endpoint opens four
channels, emits one logical pair connection and one physical reconnection, and
completes the exact reliable and unreliable logical exchange with no errors.
Browser sends can precede the rebuild; this control does not claim fresh
bidirectional payloads on every replacement channel. The native sends are held
until its replacement opens. The final real control uses a zero-second browser
establishment window and passes in 3.617 seconds (run
`25657d76-5a28-4939-93a7-e85032e4b6c6`). It settles both original browser probes
before requesting the close. An earlier unsynchronized version lost a queued
old-link reliable probe during teardown; that version is excluded as a recovery
oracle. A retry that opens after its wait expires still emits the physical
reconnection event. Reviewer feedback also produced a red control for premature
fallback publication: retry cleanup now retires physical state without resolving
the transport decision before its deadline.

The Rust, .NET, and Godot SDKs are unaffected: this change accepts a reference-only
opt-in extension and adds no new outgoing retry signals, server schema, or default
peer traffic. Their current mesh guides and signaling boundaries were inspected
at `da8f2d16`, `e467c871`, and `db7d30f2`, respectively. This result does not
identify the initial cause of #853 or #889. Those issues and A20 remain open.

### F43 Native formed peers refuse remote link retries after the initial wait

**High gameplay impact; confirmed on the native reference endpoint.** Issue
[#923](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/923).
Baseline `af582cd6`. This continues A20 recovery coverage under #826.

A native peer accepts an in-budget `PairRetry` after its original establishment
window only when its own local rebuild harness is configured. A remote
initiator can retire its connection while a receiver keeps the old connection.
The receiver also refuses the marker when the old channel closes first.

The dispatcher control drives the real native engine in both glare roles.
Both current-link channel-open callbacks establish the planned pair, a
zero-second window expires, and a current-generation retry arrives. Baseline
nextest run `b25f7d01-cf46-4707-8e5d-b1eb9a21ac85` fails at the missing attempt.
The repaired control also checks close-before-marker, stale and unplanned
signals, invalid and duplicate attempts, expired never-formed links, retained
exchange debt, an unchanged active deadline, and authoritative replacement.

A real two-native host-star history holds exchange until replacement. Only the
joining initiator has a rebuild gate. The creator uses a zero-second initial
wait and no gate. Baseline run `3b55e386-283c-464a-a33f-58528ef0dcc3` fails after
31.081 seconds while awaiting pair recovery. The repaired history passes
in 2.208 seconds (`7cae9805-320a-4a0b-b41f-6bcde92127b5`): four channel opens per
endpoint, one logical pair event, one physical recovery event, then exactly one
message per required label in both directions, and two zero exits.

The repair retains formation only within the current authoritative generation.
It survives physical loss, and departure or authoritative replacement clears
it. A formed receiver accepts newer in-budget attempts without a local gate.
An existing window stays fixed; a new wait uses the configured timeout. The
retained attempt also reports replacement recovery after that wait expires.
The first real fixture read `room_joined` for a code; that event has no code.
After correcting it to `room_created`, the real test exposed a separate
missing recovery event after the zero-second wait cleared the retry-wait set.
Those intermediate failures are not the original physical refusal proof.

Server messages, SDK behavior, and default retry count stay unchanged. Rust and
.NET SDKs provide signaling without this native WebRTC backend. Godot uses its
separate mesh adapter. The reference-only extension remains opt-in; no SDK
change or migration is required. This history does not identify the causes of
issues #853 or #889, qualify every retry/loss history, or complete A20 or #826.

### F44 Browser exits before authoritative peer-link replacement settles

Finding: [#925](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/925).
Baseline `196e812c`. This continues A20 recovery coverage under #826.

A new session-plan generation replaces a retained browser peer's physical link.
The browser clears its pending wait while retaining completed logical exchange
receipts and peer status. Its success criteria can then pass and close the new
connection before either required channel opens. The native endpoint already
keeps this replacement pending until formation, timeout, or peer removal.

A controlled dispatcher history drives the real browser orchestrator and engine.
It completes the original exchange, applies a new authoritative generation,
releases the success barrier, and advances past the exit linger while the new
channels remain closed. The baseline fails at `success must wait for an
authoritative replacement link`. The repair keeps the replacement pending.
The same history covers both offerer roles, old-channel closure before a retry,
fresh retry budgets, and completion through channel opening, deadline expiry,
or peer departure. Logical exchange receipts remain intact.

The browser README states the replacement wait. No signaling schema, server
behavior, or external SDK code changes. The Rust and .NET SDKs supply signaling
without this browser process. Godot uses its own engine adapter and has no
reference-client exit barrier. Current SDK READMEs were inspected at `da8f2d16`,
`e467c871`, and `db7d30f2`. No client migration is required.

This controlled exit failure does not identify the initial formation causes in
issues #853 or #889. Captured real mesh and restore runs remain separate evidence.
The repaired real Chromium mesh and native restore controls pass in nextest run
`ddc61788-b50b-4037-9f3e-401e03e4d97c` (3.931 and 3.494 seconds). Browser
suite, typecheck, formatting, root strict lint, and document checks pass.
Broader A01–A22 coverage remains incomplete. Hosted CI and exact-head review are
required before delivery.

### F45 Browser wall-clock changes alter connection and run waits

Finding: [#929](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/929).
Baseline `dba33ae1`. A16/A20.

The browser reference-client absolute deadlines used `Date.now`: P2P establishment,
run window, keepalive, exchange retry, relay settle, and exit linger. The
shared scheduler and CLI startup/watchdog calculations used the same wall
clock. A clock adjustment could end establishment early or delay fallback.
The native reference client already uses `Instant` for these durations.

The production-dispatch history installs an incomplete planned pair, changes
only wall time by sixty seconds, and delivers a frame to process timers. The
forward baseline fails at `clock step 60000 cannot expire the P2P window early`.
The backward baseline fails at `retry cannot extend the original incomplete-link
deadline`: elapsed time reaches the original deadline without fallback. These
are controlled clock changes, not evidence of a remote clock incident.

The repair uses whole milliseconds from each process's monotonic clock for
every duration read and the shared scheduler. The CLI transfers only elapsed
startup time to the page. Page and CLI clock origins differ; subtracting startup
time can produce a valid negative page run base. Integer rounding preserves
the deadline helper's safe-integer contract and existing overflow handling.
Relative host timers and protocol messages do not change.

The thirty-six dispatcher histories cover forward, backward, and unchanged
wall time in both roles, close-before-retry, and authoritative replacement
completion by opening, timeout, or departure. They retain logical exchange
receipts, use fractional monotonic reads, derive a negative page run base, and
finish successfully. Six additional histories keep an unmet run alive until
one millisecond before its elapsed deadline and require failure at the exact
boundary in both roles and all three clock cases. A separate production-scheduler control proves that an
early callback retains the remaining wait and that wall-clock changes cannot
postpone its elapsed deadline. Browser suite, typecheck, and bundle build pass.
Real Chromium mesh and native restore controls pass in nextest run
`3c5b4a68-fc2f-4f9e-b6e2-6eb6b2bf51df` (10.472 and 7.438 seconds).
Root strict lint passes. Independent review found no remaining concerns after
the exact run-expiry controls were added. Hosted CI remains a delivery gate.

Rust and .NET SDKs handle signaling without this browser reference process.
Godot owns separate engine timers. Current SDK READMEs were inspected at
`da8f2d16`, `e467c871`, and `226acf41`. No SDK migration is required. This
repair does not identify the initial failures in #853/#889 or complete A16,
A20, or #826. A separate authoritative recovery-event candidate is tracked in
[#930](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/930)
for runtime verification.

### F46 Authoritative peer-link replacement omits recovery events

Finding: [#930](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/930).
Baseline `990fa327`. A20.

The native reference client required a PairRetry attempt before emitting
`p2p_pair_reconnected`. A new authoritative plan clears those attempts, so
replacement channels could reopen without an event. The browser retained a
replacement wait, but timeout cleared that wait and could suppress a late
recovery event. Physical formation and message delivery are separate from
these reporting defects. Neither finding identifies the initial formation
causes in #853 or #889.

The event contract follows the logical peer obligation. First successful
formation emits one `p2p_pair_connected`. Each later physical formation emits
one `p2p_pair_reconnected`, after a retry or an authoritative replacement,
including after the P2P deadline. Timeout alone emits no recovery. Departure
clears the logical history, so rejoining creates a new obligation. Completed
exchange evidence survives a transport replacement.

The production dispatchers reproduce both defects. Native baseline fails the
recovery assertion for a previously formed responder before timeout. Browser
baseline fails the late-opening authoritative replacement assertion. Native
controls cover both roles, opening before and after timeout, previously formed
and never-formed links, duplicate plans, duplicate opens, stale callbacks,
departure, and new obligations. The browser runs 58 controlled histories,
including the prior retry and clock controls, plus authoritative recovery,
late recovery, departure, and first formation after replacement. The repair
selects the event from retained logical formation. Both engines already
permit one full-pair formation per physical link and reject retired callbacks.
No new state or deadline behavior is needed. The held native exchange still
requires the reserved PairRetry attempt, so an authoritative recovery cannot
release that harness barrier. A failing gate control proves this distinction
for no attempt, an earlier attempt, and the reserved attempt.

The real Chromium/native late-join control passes in nextest run
`ad3af02c-fd27-4c5f-973c-40e09a6062c4` (2.714 seconds). It establishes the
initial pair, fills an open room seat, and requires one recovery event at each
retained endpoint. It also requires one logical pair event per peer, preserved
exchange counts, new-peer exchanges, and clean exits. Root and native strict
lint, browser suite, typecheck, build, and document checks pass. Hosted CI and
review of the final head remain delivery gates.

Rust (`da8f2d16`) and .NET (`e467c871`) SDK READMEs describe signaling clients
without the reference-process event stream. Godot (`2a74de36`) owns a separate
engine adapter. This fix changes reference-process JSONL reporting only;
server messages and SDK contracts stay unchanged. No SDK migration is needed.
A20 and the broader #826 audit remain incomplete.

### F47 Fresh peer epochs retain old physical links until the plan

Finding: [#937](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/937).
Baseline `587bbf6a`. A20.

Both reference clients clear exchange labels on fresh `PlayerJoined` and
`PlayerReconnected` announcements. Without a preceding departure, the old
physical link remains current until `SessionPlan`. A queued old-channel message
processed between those controls can satisfy the new incarnation's receipt
obligation. The browser also exposes that stale traffic as `channel_message`.
A departure already retires the link and closes this window.

The browser dispatcher baseline fails the old-link receipt assertion after a
fresh reconnect without departure. The native dispatcher baseline fails the
same assertion after a fresh join. The repair retires the physical link when
membership invalidates exchange evidence. It preserves logical obligations
and fences queued callbacks with the existing physical generation checks.
While finalized v3 membership awaits its plan, signals, compatibility pairing
directives, and native automatic retries cannot recreate the old link. The
replacement plan clears that wait and installs the new physical generation.

Browser controls cover join and reconnect, with and without departure, both
receipt labels, old retry/candidate signals, an intervening `NewPeer`, duplicate
epochs, and fresh replacement traffic. A generation-only replacement retains
completed exchange evidence. Native controls also verify the unaffected peer,
automatic retry suppression, duplicate epochs, and replacement receipts through
the real engine event dispatcher. Four focused native controls pass in nextest
run `bf667caa-6f2f-4203-8c3d-2a6e84d34b12`. The real Chromium/native restore
control passes in run `ba5a8cf3-3536-4464-9270-bc0e0d731b91` (4.102 seconds).
It verifies fresh bidirectional traffic after actual socket loss and restore;
the controlled histories isolate the window without a prior departure. These
histories do not measure the frequency of that window on live networks.
Root and native strict lint, browser tests, typecheck, build, and document
checks pass.

Rust (`da8f2d16`) and .NET (`e467c871`) SDKs own signaling without these reference
WebRTC engines. Their wire contract is unchanged. Godot (`2a74de36`) owns a
separate mesh adapter that drops links on changed plans but does not subscribe
to remote incarnation announcements. Its intervening engine behavior remains
unproved and has a runtime assessment in
[Godot #364](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/364).
No SDK migration is required by this reference-client repair. This finding
does not identify the initial formation failures in #853/#889 or complete A20
or #826. Hosted CI and final-head review remain delivery gates.

### F48 Queued channel opens report an already-closed native pair

Finding: [#948](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/948).
Baseline `b35d9a40`. A20.

The native engine records historical required-channel open labels. The channel
pollers continue while the orchestrator awaits other work. Both channels can
open and a required channel can close before the orchestrator consumes the
open callbacks. Those callbacks still belong to the current physical link,
so generation fencing does not exclude them. The old engine then declares
the pair connected. The dispatcher records formation and can attempt its
exchange on the closed channel before consuming the terminal callback.

The real-WebRTC regression retains actual open callbacks from both endpoints,
closes a required channel, and then applies the retained opens. Baseline
nextest `a2224f99` fails the connection assertion for the reliable channel.
The repair checks both selected channels' physical state before reporting
formation. Missing channels, Connecting channels, and failed state lookups
cannot establish a connected pair. Each channel must report Open when checked; the two reads are not an atomic
snapshot. The callback-label and once-per-physical-link gates remain in place.

The live controls cover each required label and both endpoint roles. They
also check local closure before consuming the terminal notifications. The
existing remote-close control proves replacement links reopen and exchange
fresh messages in both directions. Initial repaired controls pass in native
nextest `5bd9fea9`. These state observations do not guarantee a future send:
a channel can still close after a successful check. This repair excludes
already-closing or closed channels when delayed opens are processed.

Browser formation already checks both selected channels' current readyState.
Rust (`da8f2d16`) and .NET (`e467c871`) SDKs own signaling and relay without
these WebRTC engines, so their wire contracts and migration needs do not
change. Godot (`2a74de36`) counts connected peer connections in its separate
engine mesh adapter. Required-channel closure remains an engine assessment in
[Godot #365](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/365).
Native library consumers must now await `Engine::note_channel_open`.
The retry controls now establish real SDP/ICE channels before dispatching
formation callbacks. Their timeout, retry, stale-generation, and recovery
assertions remain intact. The dispatcher control retains a real terminal
callback while draining earlier opens with exchange enabled; no probe is sent,
no recovery is recorded, and the terminal callback still retires the link and
reports relay fallback with unpaid exchange debt preserved. The rebuilt
three-player native mesh and live relay-floor control passes in nextest
`6ea16902`. Independent review added a real repeated-open once-per-link control,
which passes with the delayed-close control in `8bc2bdb2`.
This finding does not identify the initial failures in #853/#889 or complete
A20 or #826. Hosted CI and final-head review remain delivery gates.

### A11 follow-up — Refresh a running Direct host endpoint

Usability improvement: [#947](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/947).
Baseline `f5bff878`.

The previous protocol guide deferred host endpoint updates until a membership
refresh. A host could replace its address while every peer retained the old
`SessionPlan.direct_endpoint`. This did not violate that documented contract.
The new trigger publishes a fresh authoritative plan as soon as the running
Direct host changes its validated endpoint. Endpoint loss elects an executable
successor or publishes the existing Relay/Relay reset. Duplicate endpoints,
non-host metadata, open lobbies, WebRTC sessions, and Relay sessions do not
rebuild plans. Relay sessions never upgrade from metadata changes.

Baseline nextest `920f20c3` fails the endpoint-replacement control because the
old handler sends no replacement plan. The repair owns the operation beyond
caller cancellation and holds the source lifecycle and room mutation gates.
It builds the candidate plan without changing stored metadata. The exact-member
publication reserves delivery capacity and rechecks routing generations before
its hook writes metadata and the active plan. A rejected pre-commit publication
keeps the old metadata and returns an error, allowing the identical update to
be retried. A successful publication shares one fresh generation across the
current v3 members. Final scoped nextest `0174f663` passes 12 controls: endpoint
replacement and successor election, immediate hostless reset, sticky Relay,
duplicate and non-host suppression, WebRTC and v2 gates, pre-commit rejection
with identical retry, real slow-peer refusal, caller cancellation, concurrent
metadata and membership, stale source exclusion, legacy storage, and reconnect
snapshots. Independent review finds no remaining source or test issue. These
controls prove server publication, not delivery over an external Direct socket.
The Rust library method now requires its existing server
`Arc` receiver; consumers that passed `&EnhancedGameServer` must pass the `Arc`.

Rust and .NET session adapters replace the endpoint and reset their liveness
state on a fresh plan generation. Godot stores the new plan and clears WebRTC
links for Direct or Relay plans. Direct sockets belong to each game's adapter;
server publication alone does not prove their teardown or replacement. No
wire schema changes. Game adapters must process every new generation, even
when the host and peer IDs stay the same. Client acceptance remains separate:
[Rust #288](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/288),
[.NET #141](https://github.com/Ambiguous-Interactive/signal-fish-client-dotnet/issues/141),
and [Godot #358](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/358).
Those issues retain their terminal hostless-reset tests and now also require
replacement, successor election, duplicate, ordering, and fresh recovery tests.

Native and browser reference clients accept authoritative generations but
reject Direct execution and use relay. Fortress fixtures use relay adapters.
They do not establish external Direct transport acceptance. This work does not
complete the wider A11 or #826 audit.

### A17/A18 follow-up — Compile and drain the embedding starter

Baseline `154e5805`. Issue
[#951](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/951).
The complete-looking Library Usage starter failed compilation with `E0063`
for missing relay-byte rate limits and `E0308` for an extra `Arc` layer.
Its direct dependency block also omitted `axum`. Other loaded limits fell
back to runtime defaults, and the listener had no shutdown trigger.

The repaired starter maps every runtime setting from validated configuration,
uses the constructor's existing `Arc`, installs the configured tenant key,
and drains on Ctrl+C before the listener and cleanup task finish.
`examples/embedded_server.rs` is compiled by the existing all-target gates.
`tests/library_usage_tests.rs` imports its startup and shutdown functions,
ties the guide to that source, checks altered limits and invalid configuration,
and observes v3 `GoingAway` followed by close code `4000 server_shutdown`
over a real loopback socket. The retained controls require zero remaining
registrations and test shutdown without clients. A signal setup failure
returns its original error after cleanup. An isolated embedding application
also compiles with only the documented direct dependencies.

These controls verify the starter's in-memory startup and requested drain.
They do not qualify runtime destruction during drain, every deployment signal,
TLS listener ownership, or every configuration field's behavior. A17, A18,
and the wider #826 campaign remain incomplete.

### F49 Browser misses channel closure before handler registration

Issue [#955](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/955)
affects the browser reference engine under A20. A required channel can reach
`closing` or `closed` before the engine registers its handlers. The previous
code recovers an already-open channel but waits for another close/error event
on a terminal channel. The client delays transport-loss handling until its P2P
wait expires.

The repair schedules the existing terminal callback for both terminal states.
The callback keeps its generation, channel identity, and one-notification
checks. Connecting and open registration behavior stays the same. The existing
orchestrator then reports channel closure and handles transport loss.

Red evidence: the old engine fails the registration-state regression at
`reliable/closing/false`. Real Chromium also fails when the sender closes a
required channel while its receiver announcement is held. The receiver reaches
`closed` before the held announcement is passed to the engine. The old engine
emits no channel event at registration. The repaired engine reports one close
for each required label. Later close/error events do not duplicate that report.

The deterministic controls cover both labels, all four registration states,
retirement before the scheduled notification, fresh-link recovery, and old
callbacks after replacement. The real-wire controls run with the existing
channel-mode suite in `npm run test:channel-modes` under `clients/browser`.
The browser tests, strict type checks, and real channel-mode controls pass.

This is a controlled delayed-registration history. It does not identify the
unexplained establishment failure in #853 or the mesh receipt debt in #889.
The private reference engine changes no server message or external-client API.
Rust, .NET, and Godot clients do not consume this browser engine. Their separate
channel-closure assessments remain independent.

### Session 435 browser closure and hosted relay timing evidence

Browser repair F49 is verified by deterministic controls, real Chromium
closure histories, and the hosted Browser Interop workflow at `b8637712`.
The same head fails the Linux root job's clean JSON 16-player relay cell:
p99 is 422,623 us against the unchanged 250,000 us limit. Issue
[#956](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/956)
owns the unresolved cause. The package verifier overlaps that cell and takes
315.423 s. That overlap is an observation, not a proved cause.

The local all-feature CI-profile relay and package tests pass together under
CPU affinity 0,1 and four nextest slots. The relay takes 6.643 s; the package
verifier takes 109.357 s. A later isolated run of the unchanged relay binary
on CPU 0 completes all six cells, with JSON 16-player p99 14,170 us and
MessagePack 16-player p99 10,745 us. These controls do not reproduce the hosted
failure. The attempted one-CPU coload run began after the package process
ended; it supplies isolated evidence only.

The next head adds CPU snapshots before and after the root suite and prints a
completed relay cell's full observation before its latency assertion. The
250 ms limit, six cells, delivery ledgers, epoch/sequence checks, backpressure
checks, eviction checks, feature set, and scheduling stay the same. A forced
zero-limit negative control verifies that a refused cell retains its full
observation; the real limit is restored before validation and publication.

Retain both failed cold attempts. `d9020bca` stopped at the outdated H14
command guard. `b8637712` compiles in 395 s and runs only 2,692/3,232 tests in
614.246 s; 2,691 pass, one fails, 540 do not run, and 37 remain skipped.
Neither attempt saves a cache or runs the later non-TLS, allocation, and H14
controls. Neither proves the timing bound in
[#953](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/953).
The separate dependency-audit failure remains outside the user's work scope.

### F50 Browser negotiation blocks relay traffic and connection deadlines

Issue [#960](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/960)
affects the browser reference orchestrator under A20. It awaits browser offer,
answer, and ICE operations on its single input chain. P2P and run timer callbacks
append work to that chain. An unresolved browser operation therefore blocks
both deadline processing and queued relay traffic until the CLI watchdog aborts
or the browser operation settles.

The deterministic regression on `ad4a1cac` holds each of six browser negotiation
paths. It queues an accountable relay frame, advances the monotonic clock beyond
the P2P deadline, and runs due timers. Every path fails to close the stalled
link, report fallback, or process relay traffic. Releasing the held promise also
permits late negotiation and signaling on the old implementation.

The repair races each complete pairing or incoming signal operation against an
absolute deadline outside the chain. It uses the remaining P2P window, or the
configured P2P duration when no window remains, and the active run deadline.
A zero P2P timeout controls fallback reporting only. It uses the configured run
duration for operations, capped by the active run deadline.
The existing success-release hold keeps its run-window exemption. Expiry retires
the physical link before rejecting the wait. Existing engine identity checks
stop late offer, answer, and buffered-candidate continuations. Shutdown rejects
pending waits and cancels their timers.

The retained regression covers eight suspension seams: offer creation, offer
local description, remote offer, remote answer, answer creation, answer local
description, live ICE, and buffered ICE. Each runs with the P2P deadline first
and with the run deadline first. A third matrix sets the P2P timeout to zero:
negotiation stays live before the run deadline and expires at that deadline.
P2P expiry admits the queued relay frame and
reports fallback before the held operation settles. Run expiry ends using the
existing success criteria. Late completion sends no old signal and performs no
later negotiation step. A new authoritative generation negotiates a fresh link.
Successful operations and shutdown leave no extra deadline timers.

Review also found that a failed fallback send could throw from the timer before
rejecting the wait. Eight red controls reproduce that second hang. The callback
now routes teardown failures to the connection-error exit and always rejects
the wait. No timer exception escapes, and shutdown cancels every retained timer.

These controlled promises prove orchestration behavior. They do not reproduce
an actual hung Chromium operation or explain #853 or #889. Browser tests and
strict type checks pass. The first hosted browser/native run passed 16 cells
and failed `native_retry_rebuilds_browser_link_before_gameplay`. Its browser sets
`--p2p-timeout-secs 0` to report fallback before forming a late connection.
The first repair incorrectly used that reporting deadline to cancel browser
operations. All eight zero-budget regressions expose early retirement. The
corrected operation budget preserves immediate fallback and late connection.
The focused real Chromium/native retry cell fails in 31.982 seconds with the
first repair bundle and passes in 5.503 seconds with the corrected bundle.
Hosted browser/native interoperability must pass before merge.
The native engine has similar inline waits;
[#961](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/961)
tracks its independent cancellation assessment before any repair. The server wire protocol is unchanged. Rust,
.NET, and Godot SDKs do not import the private browser reference orchestrator,
so this repair requires no SDK migration or tracking issue.

### F51 Native negotiation blocks deadlines and canceled drivers outlive links

Issue [#961](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/961)
continues A20 from baseline `438bf9f9`. The native orchestrator awaits peer
construction, offers, answers, and ICE operations before returning to its
WebSocket and timer loop. Its binary watchdog does not make the P2P or run
deadlines interrupt those waits.

The baseline fails 32 controlled suspension histories. Sixteen role/seam cases
each use the P2P deadline first and the run deadline first. The seams cover
route resolution, built-but-unregistered peers, both local channel creations,
offer creation and local description, remote offer and answer, buffered ICE
for both roles, answer creation and local description, and live ICE for both
roles. Each stays pending after the relevant deadline. These controls suspend
native operations; they do not claim a captured spontaneous stack stall.

Independent real-driver controls find three cancellation leaks in the wrapper.
Canceling construction while it awaits driver initialization drops a handle
that detaches by default. Dropping an initialized default-runtime peer also
leaves the driver alive. Canceling close after it takes the driver handle
drops that handle before the explicit abort. All three baseline controls fail
to release the handler and its driver resources.

The wrapper now owns its driver through an abort-on-drop guard. Construction,
peer destruction, and interrupted close retain cancellation ownership.
Explicit close keeps its bounded graceful stop and callback-specific detach.
The native orchestrator bounds each complete negotiation operation by its
remaining P2P window and active run deadline. Zero P2P timeout controls fallback
reporting only; operations use the run duration. The held-success harness
retains its run-window exemption. Overflow-safe deadlines keep their existing
meaning.

Expiry drops the operation before removing the physical link and canceling its
pollers. Close waits at most 250 ms; the driver owner stops any remaining task.
The orchestrator clears current connection observations, retains logical pair
and exchange obligations, and rejects events from the old physical generation.
Queued relay traffic and server replies then resume. This bounds suspension;
it does not make an inline negotiation operation serve WebSocket frames before
its deadline.

The final focused native run passes 43 tests. These include all 32 deadline
histories, eight zero-P2P histories, eight held-success histories, and retained
retry and channel-lifetime controls. Every canceled history then completes a
fresh bidirectional exchange on both required channels. All four wrapper
driver lifetime cases pass. Three real Chromium/native cells also pass:
room restore, coordinated retry, and a three-player mesh.

The first extended run passes 40 of 41 tests and exposes a stack overflow in
the retained retry test. Boxing the large negotiation future and the shared
test helper removes the added nested stack frames. The final run includes a
future-size regression. Root and native strict lint pass. Hosted CI and final
review remain delivery gates for this repair.

Review finds that a close timeout can still abort a coordinated retry after
the physical link has been retired. A direct retry control fails at that
error. The timeout now permits retry while ordinary close errors keep their
existing handling. A fourth wrapper control also fails when close waits on
the core and another caller retains the peer. Close now takes driver
cancellation ownership and marks closing before that wait. Canceling it stops
the driver even while peer clones remain. All four wrapper controls pass.
The interop runner includes the wrapper cancellation controls with its retained
event-queue tests. Release qualification in #912 must retain these controls.

The wire protocol is unchanged. External Rust, .NET, and Godot SDKs do not
import this private native orchestrator or its local wrapper graph, so no SDK
migration or client tracking issue is needed. This finding does not diagnose
issues #853 or #889, qualify a replacement registry release, or complete A20 or #826.

### F52 The loss oracle rejects recovered initial channel closure

Issue [#966](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/966)
comes from PR #965 head `abb8dff3`, Verification run `38640004745`, job
`116112796174`. The host-eight loss cell fails after 18.957 seconds. The other
three loss cells pass. Artifact `webrtc-netem-38640004745-1` retains the matrix
output and endpoint event streams.

Client c07 reports a required `unreliable` channel close before its first pair
connection. It reports fallback, retries, forms the initial pair, reaches
`exchange_ready`, completes the reserved clean rebuild, exchanges both required
labels in both directions, and reaches `success_criteria_met`. No negotiation
deadline or error event occurs. The oracle rejects its early false transport
report because it only permits fallback between the two exchange barriers.

The scenario allows initial loss and automatic retries before its reserved
clean rebuild. Native channel-close handling requires immediate fallback when
no pair remains. The assertion rejects this permitted recovery. A minimized
replay of the captured lifecycle fails at the same assertion.

The repair permits an initial false report only after required-channel or
terminal peer loss in the latest plan. The same peer must form before
`exchange_ready`, with no intervening connection that makes the loss stale.
Its fallback event must stay in that phase. The reserved rebuild window keeps
its existing bounds. Controls reject missing plans, unplanned peers, optional
channels, nonterminal states, absent or stale causes, missing or late recovery,
superseded plans, missing fallback, and duplicate reports. Exact channel,
relay, retry, signal, peer-status, and server metric ledgers remain required.

The scoped oracle controls pass. The final hosted loss matrix remains a
delivery gate. This assertion repair does not identify why the initial
channel closed or claim that a passing repetition repairs a stack defect.

### F53 Browser retry negotiation renews an active connection window

Issue [#969](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/969)
affects the browser reference orchestrator under A20. A remote `PairRetry`
replaces an incomplete physical link and starts `establishPair`. That method
arms a new P2P deadline before offer creation. The caller restores the original
deadline only after negotiation completes. A stalled replacement offer thus
delays fallback and queued relay gameplay beyond the original window.

Two deterministic dispatch histories fail on `cd994865`. Each starts a
1,000 ms connection window and requests a retry at 700 ms. The replacement
holds either `createOffer` or `setLocalDescription`. At 999 ms the link stays
live. At 1,000 ms the baseline fails link retirement, fallback reporting, and
queued relay delivery. Releasing the held promise permits late signaling.
These controlled promises do not establish a spontaneous Chromium stall or
explain the separate failures in #853 and #889.

The repair passes the retained deadline into pairing before negotiation starts.
It removes the late restoration. The operation timer now retires the link at
the original deadline. The regression checks exact, once-only relay delivery
before the held promise resolves, refusal of another incomplete retry after
expiry, late continuation exclusion, fresh-generation negotiation, and timer
cleanup. Existing retry histories cover both roles, wall-clock changes,
previously connected recovery, and authoritative replacement. The zero-timeout
and run-first negotiation controls remain unchanged.

Native `rebuild_pair` already uses `PairGeneration::Retry`, which does not arm
a new connection window. The server protocol and configuration are unchanged.
The Rust, .NET, and Godot SDKs do not import this private browser orchestrator.
They require no migration or tracking issue for this repair.

The browser suite, strict type checks, formatting, and build pass locally.
Real Chromium/native retry, restore, and three-player mesh controls pass
in 6.487 seconds. Hosted verification and review remain the merge gate.

### F54 Sequential planned peers renew the connection window

Issue [#972](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/972)
continues A20 from `9196b197`. Both reference orchestrators await each peer in
an authoritative plan and arm a fresh shared P2P deadline per peer. A slow first
peer thus extends the wait for later peers and delays queued relay gameplay.
The native deferred pairing gate has the same serial renewal path. This is
separate from the browser retry renewal in F53.

Two browser dispatch histories fail on the baseline. In a three-player plan,
the first offer consumes 700 ms of a 1,000 ms window. The second peer holds
`createOffer` or `setLocalDescription`. At 1,000 ms its link remains live,
fallback is absent, and a queued relay message has no receipt. Releasing the
held promise permits late signaling. Two more histories hold the first peer
through the whole window and show that later peers still start negotiation
after expiry. These controls use browser doubles and do not prove a spontaneous
Chromium stall.

A native real-stack history also fails on the baseline. With two planned
initiators and a held offer, dispatch does not settle within 100 ms after the
1,000 ms window expires. Native controls cover both offer seams, authoritative
plan dispatch, deferred gate release, and first-peer completion at 700 ms or
expiry at 1,000 ms. They use paused Tokio time and controlled stack suspension;
they do not measure real-world setup latency.

Both clients now share one start time or absolute deadline across each planned
batch. Later peers retain their logical obligations but do not start physical
negotiation after that window expires. Native retry scheduling uses the same
batch start time. Individual legacy `NewPeer` directives and coordinated
same-link retries retain their separate existing behavior. Native signal-drain
and pairing call sites were swept; the deferred gate and authoritative plan
were the only other serial multi-peer pairing loops.

The browser suite and type checks pass. All 37 scoped native negotiation and
connection-window controls pass in 7.835 seconds. Browser controls verify exact,
once-only queued relay receipt, fallback, late-completion exclusion, fresh-plan
recovery, and timer cleanup. Native controls retain the zero-timeout, run-first,
held-success, driver cancellation, and live fresh-link exchange boundaries.
Strict root and native lint pass with all targets and features. Real
Chromium/native mesh, restore, and retry controls pass in 4.405 seconds. Hosted
CI and exact-head review remain the delivery gates.

The server protocol and configuration do not change. The external Rust, .NET,
and Godot SDKs do not import either private reference orchestrator and need no
migration for this repair. This finding does not explain #853 or #889, establish
the CI timing bound in #953, qualify published RTC replacements in #912, or
complete the broader A01–A22 audit in #826.

### F55 Reference-client handshake preludes renew command waits

Issue [#974](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/974)
continues A16/A20 from `0d702411`. Native and browser reference clients start a
new 20-second response wait for every accepted application frame. A delayed
`Authenticated` reply gives `ProtocolInfo` another full window. Room accounting
or membership preludes can extend `JoinRoom` indefinitely. Native `Reconnect`
uses the same repeated wait. Browser output flushes can also suspend a command
before its response timer starts.

Browser command histories deliver an accepted prelude at 19,999 ms. The old
client remains pending at 20,000 ms during authentication, v2 membership,
v3 accounting, and held authentication output. On-time baseline controls pass.
A separate expiry control delivers a valid room baseline before the due host
timer callback runs. The repair rejects it before publishing room success.
Another control completes late authentication and gives the following room
command its own full window. Both versions retain timer cleanup, socket closure,
and late-continuation exclusion. Review found that final `protocol_info` output
still ran under the next command's window. Two additional histories reproduce
that gap on PR #975's first head. The command wrapper now drains final output
before clearing its deadline. Held-join ingress coverage suspends actual
`room_created` output and retains the exact baseline-plus-255 frame boundary.

Each command now has one monotonic deadline. Browser frame and output waits
share a scheduled timer and abort state. Native command writes, response frames,
and reconnect cleanup retries retain one absolute deadline. The native read
path checks expiry before and after a ready frame; buffered replies cannot
bypass the deadline. The reconnect response loop is separate from snapshot
application, so peer rebuilding keeps its existing run and connection windows.

Native tests use real loopback WebSockets and a controlled Tokio clock. A
runnable clock guard prevents automatic time advancement during socket I/O.
They allow one millisecond for Tokio's timer-wheel resolution. Nine histories
cover authentication, join, and reconnect with on-time, missing, and late
terminal replies. Restoring per-read renewal makes all three commands remain
pending for missing replies and accept late replies; on-time controls still pass.
These controlled histories do not establish a spontaneous
server stall. Earlier malformed-reply and automatically advanced-clock probes
were invalidated and do not count as evidence.

The browser suite, type checks, and build pass locally. The restored native
controls pass in 0.113 seconds. Strict root and native lint pass with all
features and targets. Real Chromium/native mesh, room restore, and the mixed
v2/v3 relay floor pass in 3.732 seconds. These checks cover the repaired seams;
they do not complete the wider audit.

The server protocol and configuration do not change. The Rust, .NET, and Godot
SDKs do not import either private reference orchestrator; they need no migration
or acceptance issue for this repair. A16/A20 and the wider #826 audit remain
incomplete. The repair does not establish #953's CI timing bound or explain the
initial peer-formation failures in #853 and #889. Hosted CI and final-head review
remain delivery gates.

### F56 Native WebSocket writes block timers and signaling failures stay nonfatal

Issue [#976](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/976)
continues A20 from `27a9b6c6`. Native runtime control, signaling, and relay writes
await the WebSocket sink without a deadline. A nonreading endpoint can hold the
single orchestrator task inside a write. The task cannot process its keepalive,
P2P, or run timers until that write returns. The binary watchdog is separate
from these timers and from the public library lifecycle.

Two real loopback histories complete the WebSocket upgrade, retain the server
socket without reading, and constrain the client socket's send buffer. An
8-MiB text or binary write remains pending during a 50-ms real-clock observation.
After a further 20 seconds on the paused Tokio clock, both baseline writes stay
pending. The test does not require a network outage or infer one from timing.

The shared native frame writer now gives each complete send and flush one
20-second budget. Text control, signaling, JSON relay, explicit delivery-class
relay, and binary relay use that boundary. Existing command deadlines still
bound the whole handshake. A timed-out write returns a connection error; callers
must discard the connection because a partial frame may remain. This bounds
write suspension. It does not make the single task process incoming frames
while a write is pending or make the soft run deadline interrupt each write.

The caller sweep also finds swallowed connection failures. Native remote
`PairRetry` converts a fatal retry error into a nonfatal signal diagnostic.
A closed-socket regression reproduces that behavior during the fallback-status
write. Browser offer creation and incoming signal handling likewise catch
connection failures as peer negotiation errors. Two browser dispatch histories
make replacement offer and incoming-offer answer writes throw. The baseline
answer history continues instead of ending the session. Restoring the old
pairing catch after repairing the signal catch reproduces the offer failure.

The native retry handler and both browser catches now preserve connection
failures as terminal. Ordinary peer negotiation and invalid retry input retain
their existing nonfatal handling. The focused native run passes eight tests,
including both blocked-write histories, four healthy wire frame shapes, the
retry failure, and the retained command-window controls. Browser tests, type
checks, and build pass. All five native retry tests pass. Real Chromium/native
coordinated retry, room restore, and mixed v2/v3 relay pass. Hosted CI and review
remain delivery gates.

The wire protocol is unchanged. The external Rust, .NET, and Godot SDKs do not
import these private reference-client implementations. No SDK migration or
tracking issue is needed. This repair does not diagnose #853 or #889 or complete
A20 or the broader audit in #826.

### F57 Reference-client ready peer work bypasses expired deadlines

Issue [#979](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/979)
continues A20 from `81e8c081`. Browser peer negotiation races a promise against
an absolute timer. If the promise settles after expiry but before the timer
callback runs, it cancels the timer and can publish a late offer or answer.
The physical peer link remains installed. The native engine has a related
boundary: Tokio polls a ready operation before checking its expired timer.
A synchronous candidate-buffer operation can therefore succeed at expiry.

Eight browser histories hold offer creation, local description, remote offer,
remote answer, answer creation, answer description, live ICE, or buffered ICE.
They advance the monotonic clock past expiry and release the operation before
running due timer callbacks. All eight baseline histories retain the expired
link; six also publish late signaling. The repaired controls complete at the
exact deadline and reject the results. Eight more histories reject the held
operation after expiry. Both sets verify retired links, no late signaling,
once-only intact queued relay delivery, fresh-generation recovery, and timer
cleanup. Existing on-time and scheduled-expiry controls remain in the suite.
These browser doubles do not establish a spontaneous Chromium stall.

A separate real native-stack history buffers an ICE candidate at an expired
absolute deadline. The baseline returns `Ok(None)` instead of retiring the
peer. Its on-time control succeeds. The engine now checks the deadline before
polling work and after it settles, and uses the existing bounded peer cleanup
on expiry. The browser checks both successful and failed settlement before
allowing further signaling. A signaling failure during fallback remains fatal.
A second native history holds the real operation after candidate buffering,
then releases its ready result at expiry before polling the timer. Restoring
only the old completion match makes it fail with `Ok(None)`; the on-time control
still succeeds. These controlled histories do not establish every stack
completion or operating-system schedule.

Review of the first repair found that a trickle-ICE completion at the shared
setup cutoff also retired an already connected pair. Browser dispatch and a
real native pair both reproduce this regression. Connected peers now receive
a fresh bounded signal-operation window instead of inheriting the setup cutoff.
Controls preserve the connected link at the setup cutoff, deliver queued relay
data, and exchange real native gameplay on both channels afterward. Separate
controls hold ICE through the new operation deadline and still require teardown
and fallback. These controls use controlled clock and stack suspension.

The browser suite, type checks, and build pass. The 40 scoped native engine and
negotiation controls pass, including zero P2P timeout, held
success, shared plan windows, relay fallback, and retry cleanup. Strict root
and native lint pass. Real Chromium/native mesh, coordinated retry, and room
restore controls pass. Full hosted validation and final-head review remain
delivery gates.

The wire protocol and server behavior do not change. The external Rust, .NET,
and Godot SDKs do not import these private reference implementations and need
no migration or acceptance issue. This finding does not explain #853 or #889,
prove #953's timing bound, qualify #912's dependencies, or complete A20 or the
broader #826 audit.

### F58 Reference-client WebSocket upgrades bypass the connection deadline

Issue [#981](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/981)
continues A02/A20 from `b3699622`. The browser connector uses only a scheduled
timeout callback. An open event dispatched at expiry can cancel that due timer
and return the socket. The native connector uses Tokio's timeout, which polls
a ready upgrade before its expired timer. Both clients can start authentication
after the original ten-second connection window.

Two browser histories advance the monotonic clock to the deadline or one
millisecond past it, then dispatch open before the timer callback. Both fail
against the baseline. A real native loopback history suspends the connector
while the server finishes writing its HTTP upgrade. Resuming the ready connector
at expiry returns a socket on the baseline. The on-time control passes one
millisecond before expiry. These controlled schedules do not prove a spontaneous
network or browser stall.

Both connectors now compare completion with the original monotonic deadline.
The browser closes an expired socket and cancels its timer before rejecting.
Native drops an expired upgrade result, releasing the transport. Controls retain
on-time upgrades, no-response timeouts, ordinary refusals, and fresh connections
after expiry. Browser callbacks settle once; native endpoints observe transport
closure without application frames. The native clock guard excludes automatic
clock advances during socket I/O.

The server protocol and configuration are unchanged. The external Rust, .NET,
and Godot SDKs do not import these private reference connectors and need no
migration or acceptance issue. The command windows in F55 and peer negotiation
in F57 remain separate controls. This finding does not diagnose #853 or #889,
prove #953's timing bound, qualify #912's dependencies, or complete A02/A20 or
the broader #826 audit. All 11 scoped native wire tests pass, including both
write controls retained from F56. Browser tests, strict type checks, formatting,
and build pass. Root and native strict lint pass with all features and targets.
Real Chromium/native mixed mesh, room restore, and mixed v2/v3 relay controls
pass in 3.955 seconds. Hosted CI and final-head review remain delivery gates.

### F59 Buffered native WebSocket controls bypass receive deadlines

Issue [#983](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/983)
continues A20 from `f09b072b`. The native reference receive helper skips ready
Ping/Pong frames without checking elapsed time. Tokio polls the ready stream
before its timeout. A control sequence can therefore keep one poll running
past the cutoff. The handshake wrapper cannot interrupt that poll, and the
helper starts a fresh inner window instead of using the command cutoff.

Two baseline regressions return a reply after expiry: a paused-clock read
resumes at its deadline, and a finite ready control sequence crosses the
cutoff within one poll. A separate production handshake control uses a shorter
original command window. It fails because the helper reads all eight control
frames instead of stopping at that window. These finite streams establish
the scheduling defect;
they do not establish a spontaneous network incident or unbounded CPU work
inside a single transport poll.

The receive helper now checks the same monotonic cutoff before and after each
frame read. The handshake passes its original command cutoff into that helper.
The public duration-based helper keeps its signature and overflow behavior.
Controls cover on-time replies, exact and late expiry, a read that crosses the
cutoff before returning ready, zero-budget input preservation, both transparent
control types, malformed and binary frames, close, transport failure, and EOF.
Finite and unrepresentable durations retain their existing meaning.

The server protocol and configuration are unchanged. The external Rust, .NET,
and Godot SDKs do not import this reference receive helper and need no migration
or acceptance issue. The browser uses a separate event-driven receive path.
This repair does not diagnose #853 or #889 or complete A20 or audit #826.
All 22 scoped wire and handshake controls pass. Root and native strict lint
pass with all targets and features. Real native three-player mesh and mixed
v2/v3 relay controls pass in 2.582 seconds. Hosted validation and final-head
review remain delivery gates.

### F60 Native WebSocket writes report success after expiry

Issue [#985](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/985)
continues A20 from `f9accf4e`. The native frame writer uses Tokio's timeout
without checking completion time. Tokio polls the sink before its timer, so a
ready flush can return success at or after expiry. The orchestrator can then
emit a sent event and continue using a connection whose write window expired.
The handshake wrapper has the same gap at its original command cutoff.

A deterministic sink history holds a text or binary flush, advances the paused
clock, and releases the flush before polling the expired timer. The baseline
accepts both frame types at 20 seconds and at 20.001 seconds. Controls at
19.999 seconds succeed. A separate caller control uses a two-second command
window. Restoring the old outer-timeout behavior while keeping the repaired
20-second writer reproduces success at two seconds and at 2.001 seconds.
These controlled sink schedules do not establish a spontaneous socket stall.

The shared writer now checks its absolute monotonic cutoff before sending and
after completion. Handshakes pass their original command cutoff into that
writer. The write ceiling still applies to longer or unrepresentable command
windows. An expired result is a connection failure. A failed write can already
have emitted bytes; callers must discard the connection, and delivery remains
uncertain. The repair does not retract a frame or interrupt synchronous work
inside one sink poll.

Controls retain on-time text and binary flushes, exact and late expiry, the
shorter command window, no sink work for expired commands, pending flushes,
and ordinary sink errors. Existing real-socket controls retain blocked-endpoint
expiry and exact healthy text, delivery-class, and binary frame shapes.

All 25 focused wire and handshake controls pass. Root and native strict lint
pass with all targets and features. Real three-player native mesh and mixed
v2/v3 relay controls pass in 2.417 seconds.

The server protocol and settings are unchanged. The Rust, .NET, and Godot SDKs
do not import this reference-client wire helper and need no migration or
acceptance issue. Browser writes use a separate synchronous API. This repair
does not diagnose #853 or #889, establish #953's timing bound, or complete A20
or the wider #826 audit. Hosted CI and final-head review remain delivery gates.

### F61 Native Ping write time consumes the Pong response window

Issue [#987](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/987)
continues A20 from `1fc0a8cf`. The native orchestrator samples time before
awaiting its Ping write, then uses that sample for the next Ping cadence and
Pong response deadline. A successful write can take up to 20 seconds, exceeding
the ten-second response window. The client can enter its one-second drain grace
immediately after finishing the write and then close a responding connection.

A real loopback history queues an 8-MiB frame on a constrained socket ahead of
Ping. It establishes actual write backpressure before pausing the clock,
advances 12 seconds, and drains the endpoint. The complete queued frame and
Ping arrive. The baseline leaves a Pong deadline in the past. A runnable clock
guard prevents socket I/O from automatically advancing the paused Tokio clock.
This controlled schedule proves the timestamp defect, not a spontaneous network
stall or the cause of earlier peer-formation failures.

Both keepalive timestamps now start after the successful Ping write. The
regression retains the complete ten-second response window and accepts Pong
one millisecond before expiry. A separate real-socket control sends a normal
Ping, preserves one outstanding request, enters drain grace at ten seconds,
and fails after that grace when no reply arrives. The existing wake-scheduling
control also passes. Write failures remain terminal under F56/F60.

The caller sweep finds no other native Ping timestamp writer after an awaited
send. Connection construction and room restore initialize their next Ping from
a fresh instant. Browser Ping writes use a synchronous API and have no awaited
write between their time sample and timestamp assignment. The repair does not
change other timer semantics or make the single task process input during a
pending write.

The wire protocol and server settings are unchanged. Rust, .NET, and Godot SDKs
are separate implementations and do not import the native reference
orchestrator. They need no migration or acceptance issue for this private
keepalive repair. The broader A20 audit, #826, #853, #889, and #953 remain open.
The three scoped keepalive controls pass in 4.922 seconds. Root and native
strict lint pass with all targets and features. Real three-player native mesh
and mixed v2/v3 relay controls pass in 2.654 seconds. Hosted validation and
final-head review remain delivery gates.

### Native establishment diagnostics — issue #853

At `665a256f`, 20 captured native/browser room-restore histories passed in
43.277 seconds (Nextest `b178a333-7dc3-47b9-a711-d8432b612072`). Each history
includes initial establishment and replacement exchange. This does not explain
or repair the original failure. Issue #853 remains open.

The native timeout path now records each incomplete expected peer before it
resolves transport status. The snapshot reports the current physical-link
generation, remote-description application, remote candidate admission and
queue counts, channel registration, and handled open callbacks. Snapshot
construction reads local state without awaiting the WebRTC driver. A removed
link produces `null`. Observed opens do not prove current channel state.

The existing three-player partial-ICE scenario is the runtime control: both
healthy endpoints report only their missing edge; the isolated endpoint reports
both missing edges. Healthy channel exchange, transport-status fan-out, and
relay delivery remain required. The baseline failed because it emitted zero
timeout snapshots. Unit controls cover unpaired, initiator, responder, queued
candidate, delayed open observation, removal, and replacement states.

Rust, .NET, and Godot SDKs are unaffected: this change adds stderr diagnostics
inside the private native reference client. Server behavior, protocol messages,
and external client APIs are unchanged.

### F62 — Readiness recovery leaves reference-client starts latched

Issue #990, found at `dcde125b`. Native and browser reference clients latch a
`StartGame` send. Their readiness snapshots replaced the ready set without
invalidating that latch when a player became unready. If the server rejects
the pending start, a later all-ready snapshot can arrive before the rejection
error. The snapshot cannot retry, and the error only rearms the latch. The
lobby then waits for another event until the run timeout.

The server permits this order. Ready toggles publish through the room event
lane. A `NotReady` result releases the mutation gate before the handler sends
its error. A later toggle can publish first. This is a client latch defect;
no server ordering change is required.

Both clients now clear the send latch when a snapshot removes a previously
ready player. The next all-ready snapshot can retry without waiting for an
error. Unchanged and add-only snapshots preserve the latch. Membership and
rejection invalidations retain their existing behavior.

The native handler-level loopback regression fails on the original source
because no second `StartGame` reaches the socket after readiness recovers.
Its repaired history covers rejection before and after recovery, repeated
snapshots, silence while unready, and finalization. Browser gate controls
cover rejection before, between, after, or absent from the false/true snapshots,
set-order independence, unchanged/add-only suppression, non-creators, and
finalized rooms. These are deterministic permitted message histories, not a
capture of a hosted production stall. Existing membership recovery controls
remain required.

The change fixes private orchestration in the shipped reference clients.
Rust, .NET, and Godot SDKs do not import these modules. The server's readiness
rules and wire messages are unchanged, so these SDKs need no migration or
acceptance issue for this implementation repair. Broader A20, #826, #853, and
issue #889 remain open. Timing investigations also remain open.

### F63 — Reference-client MessagePack and mixed-room reception

At `de5adb40`, the browser CLI rejects `message_pack`. The native CLI accepts
it, but sends the relay text as raw bytes and emits received bytes through its
opaque path. These bytes are not a MessagePack application value. A compatible
recipient cannot decode them. This continues encoding task #678 and A20.

The browser CLI regression fails on the baseline option. A real three-player
history with a v2 MessagePack browser, a v3 MessagePack native client, and a
JSON native client fails when the browser receives the native raw text.
The native receipt also retains a base64 wrapper instead of a decoded value.
Both clients now encode the relay probe as a MessagePack value and apply
received values through their normal gameplay event and delivery-ledger paths.
Their v2 envelopes omit delivery stamps; v3 envelopes require them. Negotiation
checks deployment support in both versions. Opaque v2 reference-client runs
remain unsupported because raw passthrough has no sender identity.

The sweep found browser issue #994. The browser rejects text `GameData` in
opaque modes, although the server delivers JSON-source data and compatible
MessagePack conversions as text. Its new unit control fails on the baseline.
A real Chromium rkyv recipient also exits on the valid text frame. Opaque modes
now accept JSON fallback and retain binary-envelope and delivery validation.

Two three-player MessagePack controls cover browser v2/native v3 and browser
v3/native v2, each with a JSON peer. Every directed receipt must preserve its
sender and exact payload. Separate rkyv and protobuf Chromium controls receive
exact JSON and MessagePack fallback payloads from real socket senders.
Existing same-format opaque, mesh, and room-restore controls remain required.
All eight scoped runtime controls pass in 31.408 seconds. Unit controls cover
malformed envelopes, version mismatch, trailing payloads, unsupported encoding,
Unicode, nested values, and browser integer precision. Passing captured
attempts do not explain or repair #853 or #889.

**Migration:** Native MessagePack receipt events now contain decoded application
values instead of the opaque base64 wrapper. Server messages, negotiated
formats, deployment defaults, and room rules are unchanged. Rust, .NET, and
Godot SDKs have separate MessagePack implementations and do not import these
reference clients. They need no implementation change or acceptance issue for
this repair. The complete catalog assessment in #678 and the wider A01–A22
audit remain open. Final-head hosted CI and review are delivery gates.

### Catalog qualification after F63

At `7606fd8c`, the real-socket coverage does not qualify every format and
version pair. Two new controls cover all four formats on both versions.
The relay matrix uses eight source combinations and eight recipients per
source. All 64 paths preserve exact payloads or report an unsupported format.
The negotiation matrix checks all eight deployment flag combinations against
all eight version/request pairs. Each advertisement and effective format must
match the deployment policy. Disabled requests must warn and fall back to JSON.

The relay checks use the recipient's version for stamps and envelope shape.
They check exact v3 omission ranges, v2 field and report exclusion, and ordered
source departure after each payload or refusal. The matrix passes in 0.673
seconds; negotiation passes in 0.322 seconds. V2 opaque paths preserve raw
bytes, as required by the frozen contract. Those bytes cannot identify a sender
without application metadata. Both reference clients therefore retain their
explicit v3 requirement for opaque modes. This documents that boundary and
does not claim v2 reference-client attribution parity. Keep #678 open for its
full acceptance decision. No new encoding or capability is introduced.

### F64 — Browser MessagePack rejects valid object keys

The catalog assessment found #996. On `7606fd8c`, the browser decoder rejects a
MessagePack payload with an object property named `__proto__`. JSON fallback
and native decoding can preserve this value, so the recipient format changes
whether the same game message can be received. The new wire regression fails
with the decoder's key rejection before the repair. A real Chromium v3
recipient also fails in 1.313 seconds on the baseline browser bundle. Its
bundle hashes stay fixed before and after that run.

The browser now prefixes decoded map keys during MessagePack materialization
and restores their exact names through `Object.fromEntries`. Values and
application-visible key names stay unchanged. Empty names, nested objects,
NUL-prefixed names, and a leading Unicode byte-order mark have distinct
controls. Numeric map keys retain their existing conversion to string names.
Non-JSON payload values remain rejected. Existing integer precision and version-specific envelope validation still apply.

Real Chromium v2 and v3 recipients each receive exact JSON and MessagePack
payloads from live senders. Separate opaque recipients preserve these keys on
the JSON fallback path. All four runtime controls pass. The public server
wire, deployment flags, room behavior, and SDK APIs do not change. Rust, .NET,
and Godot clients do not import this browser implementation and need no
migration for this repair. Full hosted CI and final review remain delivery
gates. Captured passing attempts for #853 and #889 do not establish their
failure causes; both issues stay open.

### F65 — Opaque reference probes on the frozen v2 wire

At `d82e0818`, both reference clients refuse v2 rkyv/protobuf runs. The server
already preserves their bytes on v2, but raw passthrough cannot identify the
sender. A new Chromium matrix fails at the CLI refusal before the repair.
The browser CLI unit control fails on the same boundary. This continues #678.

Both clients now use the documented application probe header: ASCII `SFRP`,
version byte 1, a 16-byte sender UUID, then the exact CLI payload. The header
uses 21 bytes of the existing payload limit. It stays inside the opaque
payload on both versions. V2 keeps raw passthrough and emits no delivery
stamps. V3 keeps its server envelope and ledger and checks agreement between
its server sender and a tagged probe. Receipt events retain the original
payload bytes and encoding token. JSON and MessagePack behavior is unchanged.

Shared literal vectors cover empty, binary, Unicode, and long payloads.
Truncation, unknown versions, absent v2 attribution, sender mismatch, offset
views, and old untagged v3 payloads have separate controls. Live Chromium/native
pairs cover each opaque format with v2/v2, v2/v3, v3/v2, and v3/v3 endpoints.
Each endpoint must receive exactly one attributed, byte-exact probe and exit
successfully. V2 must emit no session plan, delivery report, or relay stats.
The server's independent 64-path relay and 64-case deployment matrix still
passes. No encoding, capability, deployment flag, or server wire shape is added.

**Migration:** Upgrade both reference clients for opaque v2 or mixed-version
runs. New opaque sends include the probe header on both versions. New v3
receivers also accept old untagged payloads except the reserved `SFRP` prefix.
Old receivers expose the new header in their payload. This is a reference
probe schema, not a general rkyv/protobuf serializer. Other applications keep
their own opaque schemas. See the native README's opaque relay probe contract.

Rust SDK `da8f2d16`, .NET SDK `e467c871`, and Godot SDK `2a74de36` use separate
binary codecs and do not import these reference probe modules. Their server
wire and application payload contracts do not change. Each SDK is unaffected
and needs no migration or acceptance issue for this private probe schema.
Captured passing attempts for #853 and #889 do not establish their original
failure causes. Those issues and the broader #826 audit remain open.

Final fixed-binary selection `d4174a39` passes all 12 controls in 10.275
seconds. It includes eight opaque cross-client version pairs, both original
browser opaque controls, both-version fallback recipients, native opaque and
downgrade controls, reversed MessagePack mixed rooms, and the two open WebRTC
failure histories. Server, native executable, browser CLI, and page bundle
hashes match before and after. The #853 restore control passes in 3.779 seconds;
The #889 mesh control passes in 4.816 seconds. Both endpoint streams and server logs are
retained. These are passing attempts, not repairs of the original failures.

### F66 — Browser crippled ICE admits candidates inside remote SDP

Issue #999, found at `10365436`. The browser drops local candidate callbacks
and incoming candidate signals in crippled mode. Its remote offer and answer
handlers still pass embedded candidates to Chromium. Complete SDP can therefore
establish both required channels, so the fallback control can report a path
that the option promised to disable. This is a reference transport control
defect; it does not identify the original #853 or #889 failure causes.

The repair removes candidate attributes from remote SDP only in crippled mode.
Both offer and answer paths use the same transformation. Healthy SDP stays
byte-identical. The engine also ignores incoming candidate calls before
parsing or buffering them. The orchestrator retains its existing candidate
signal filter. Unit controls cover both roles, both modes, CRLF/LF,
candidates before and inside a media section, and
an unterminated final candidate. They verify that crippled candidates never
queue or apply, while healthy candidates flush in order after SDP. Both the
SDP regression and the candidate guard fail before their respective repairs.

A real Chromium experiment supplies fully gathered SDP from a healthy peer.
On the baseline, the crippled responder connects and opens both required
channels in less than one second. After the repair, crippled offer and answer
controls stay unconnected during the bounded observation. Healthy controls
exchange exact messages on both labels in both directions. The four cases also
run in the durable Chromium channel suite. Each crippled case observes 1.5
seconds without formation. These observations cover candidate-bearing SDP in
this local runtime; they do not certify every ICE implementation or explain
unrelated timeouts.

Rust SDK `da8f2d16`, .NET SDK `e467c871`, and Godot SDK `2a74de36` have separate
implementations and do not import this browser engine or its crippled-mode
option. They are unaffected. Server signaling payloads remain opaque and
unchanged. No SDK migration or acceptance issue is needed. The broader A20/A22
coverage and audit #826 remain open. Hosted CI and final review remain gates.

### F67 — Native exchange send failure ends the relay session

Issue #1000, found at `10365436`. A required channel can close after physical
pair formation while its terminal callback is still queued. The native
exchange gate can run first. Its send error becomes a fatal connection error,
so the signaling session ends instead of using the documented relay floor.
The same send helper serves immediate, deferred, and unreliable-only exchange.

The real native-pair regression closes a required channel and holds its
terminal callback before releasing exchange. The baseline fails with exit
code 3 and a data-channel-closed error. Both required labels, the separate
unreliable release, and the immediate send entry point have controls. Missing
channel storage has its own control.

The repair uses the existing P2P loss handler for missing-channel or failed
exchange sends. It retires the physical link and reports the current transport
state. Failed labels are not credited. Completed labels and missing receipt
debt remain unchanged. Successful sends keep their existing behavior. If the
fallback status cannot reach the signaling socket, that write error still
ends the session. No new retry policy or send deadline is introduced.

The scoped tests verify fallback, a subsequent exact relay payload and Ping
on the WebSocket, and a fresh coordinated link rebuild that discharges missing
send and receipt debt. The missing-channel control separately checks terminal
signaling write failure. These are controlled real-stack and
loopback histories, not captured causes of #853 or #889. The browser already
retries exchange send errors without ending its signaling session and needs
no equivalent repair. Broader audit and timing work remain open.

Rust SDK `da8f2d16`, .NET SDK `e467c871`, and Godot SDK `2a74de36` do not import
the native reference orchestrator or its private exchange helper. Each SDK is
unaffected. Server messages, payloads, defaults, and SDK APIs are unchanged;
no SDK migration or acceptance issue is required. Final-head hosted tests and
review remain delivery gates.

### F68 — Pending native exchange sends stop relay processing

Issue #1002 continues A20 after F67. The native exchange helper awaits the
WebRTC send on the single orchestrator task without a local deadline. The
shipped driver awaits capacity, its peer core lock, and driver wake work.
A pending send therefore stops relay input, keepalives, and timer processing.
The independent binary watchdog can end the process but cannot preserve the
relay session.

The regression forms a real native pair, then holds the send future at the
exchange helper. Baseline nextest `a52283cf` fails at the outer watchdog after
both channels open and the pair reports connected. This is a controlled
pending-operation history. It does not prove a spontaneous driver stall or
explain #853 or #889.

Each local exchange enqueue now has a one-second deadline, shortened by the
existing active operation and run deadline. Checks before work starts and when
it settles reject already-expired work and ready completions at the cutoff.
A failed or expired send uses F67's physical-link cleanup and current transport
report. Completed labels and missing send/receipt debt remain unchanged.
A timed-out operation can already have queued bytes; the client does not claim
rollback or delivery certainty. Signaling write failures remain terminal.

The focused history covers both required labels, a permanently pending send,
a shorter run cutoff, already-expired work, and a ready completion at the
cutoff. Each failure retains exchange debt and permits exact relay data and
Ping on the signaling socket. F67's successful real sends, coordinated rebuild,
ordinary send errors, missing-channel control, and signaling-error control
remain separate checks. Scoped nextest `838d4ca9` passes all three controls.
The held-send control also verifies that cancellation releases captured state.
Hosted interoperability and review remain delivery gates. These controls prove
a bound for one send and its recovery. F68's repair still processes deferred peers
in sequence; they do not prove keepalive
responsiveness across an arbitrary mesh of stalled sends. That batch-scheduling
question remains outside this repair and is tracked in #1003.

The browser API sends synchronously. The Rust, .NET, and Godot SDKs do not
import this private native reference helper. Server messages, SDK APIs, and
retry defaults are unchanged. No SDK migration is required.
The broader A20 and #826 audits remain open.

### F69 — Deferred native exchange batches delay signaling across peers

Issue #1003 continues A20 at `d503f04d`. Both deferred release gates await
every connected peer in sequence on the orchestrator task. F68 bounds each
send, but the caller cannot read signaling between those waits.

The initial baseline control forms four real peer links and holds all four
local send futures.
After the first send starts, the socket receives an on-time Pong and relay
data, then a close. The baseline delays this input for 4.027 seconds and fails
the 2.8-second response bound. This is a controlled send-stall history. It
does not establish a spontaneous driver stall or a false Pong timeout.

The gates now queue each channel label separately. The event loop checks
ready signaling, engine events, probes, and timers before the next deferred
send. It limits normal input to 16 events per burst so continuous input
cannot prevent exchange work. Each send keeps its existing one-second local
limit and shorter operation or run deadline. Each queued item retains its
physical channel identity; replacement channels cannot inherit old work.
Successful labels retain their evidence. Failed or cancelled labels keep
their debt and use the existing physical-link cleanup and relay fallback.

The final seven-control selection passes. Both release gates preserve real
bidirectional exchange with a healthy peer after a stalled peer. The socket
control checks a cleared Pong deadline, exactly-once v3 relay accounting,
and unpaid stalled-peer debt. Separate controls cover missing and replaced
channel identities, an expired run window, terminal signaling writes,
single-send cancellation, and existing retry exchange. A ready-callback
burst control fails when the input-burst bound is removed, with Tokio's
cooperative yield disabled. No timeout is enlarged and no cancelled send is
recorded as successful.

Hosted restore validation exposed a separate test-order assumption. The test
waited for inbound reliable data, then required the local send event to be
already recorded. A held local exchange gate now forces the inbound event
first and makes the old assertion fail. The repaired oracle waits for both
directions within the same original deadline, then keeps the exact counts,
payload checks, and final exchange checks. This repairs the test oracle;
it does not identify a new transport delivery defect.

The browser sends synchronously and has no matching deferred async batch.
Rust SDK `da8f2d16` uses a nonblocking driver seam, .NET SDK `e467c871` builds
its own C# assembly, and Godot SDK `2a74de36` uses the engine mesh transport.
None imports this reference orchestrator or its exchange probe. All three
are unaffected. Server wire messages, SDK APIs, defaults, and timeout values
are unchanged; no SDK migration is required.

The captured #853 reconnect and #889 mesh controls pass with fixed baseline
binaries in 1.85 and 2.32 seconds. Their source failure causes remain unknown.
These passes do not close either issue or the broader A20 and #826 audit.

### F70 — Capacity receipts bypass recipient and application identity checks

Issues #1006 and #1007 continue A22 at `560ec0df`. The capacity oracle indexed
all receipts but visited only roster recipients. Two additional receipts to an
unknown recipient left a complete history valid. A separate control changed
one receipt's application ledger sequence while retaining its valid server
stream position. That history also stayed valid. These are confirmed checker
defects. They do not prove a server misroute or payload corruption.

The oracle now rejects each unknown-recipient arrival with its exact delivery
key. It also compares each receipt's application sequence with the recorded
send at that sender epoch's server position. The comparison uses the epoch's
send block, including after reconnect; application sequences continue across
epochs while server positions restart. Both violations use the existing
`MisroutedDeliveries` reason. Raw receipts, byte evidence, and latency accounting
remain observable. No artifact schema or server protocol changes.

Both focused regressions fail before their respective repairs. Final controls
cover all three delivery classes, unknown recipients in the opaque experiment,
another known ledger sequence, an absent ledger sequence, and a previous-epoch
sequence after reconnect. A real-socket history is then edited and replayed
through the public artifact path. Both mutations fail with their exact keys;
restoring the original events reproduces the complete original summary.
Opaque leaks still occupy coverage and retain duplicate and ordering evidence.
A matching omission report must fail as overlap. A four-case control passes on
the original oracle, fails on the first compact version, and passes after the
coverage repair. Existing missing, churn, and payload-size controls stay
separate. The broader A22 and #826 audits remain open.

The same review replaces the per-receipt pointer index with compact stream
observations. The independent position-set and unique-arrival reference covers
all 4,096 six-arrival histories over zero, one, two, and `u64::MAX`. It checks
unique counts, duplicate counts and first keys, and order counts and first keys.
An ordered 100,000-message stream retains one coverage range. The
[capacity report](arm-capacity-audit.md#c3-stream-observation-memory)
records the measured gains and limits. Raw record retention and fragmented
coverage remain workload-dependent; this is not a complete memory bound.

Five fixed-binary #853 restore attempts and five #889 three-player browser mesh
attempts pass. The test executable, server, native executable, browser CLI, and
page bundle hashes match before and after the alternating attempts. Each run
retains endpoint streams and server diagnostics. These runs do not reproduce
or repair the original failures. Both issues remain open.

Rust, .NET, and Godot SDKs are unaffected: these changes are private capacity
checker code and its tests. No client imports this checker. Server behavior,
SDK APIs, wire messages, and defaults are unchanged.

### Session 458 initial WebRTC captures

Ten fresh fixed-binary #853 restore attempts and ten #889 three-player mesh
attempts pass at `a7abbd60`. The test executable, server, native executable,
browser CLI, and page bundle hashes match the session 457 values before and
after capture. All twenty attempts retain complete endpoint streams and server
stdout and stderr. Fifty client streams contain 2,508 events. Exact peer and
channel-label send and receipt counts match; each client exits successfully.

The attempts take 65.625 seconds in total. They use the unchanged baseline
binaries, before the session 458 source repairs. They do not validate those
repairs, reproduce either original failure, or establish a cause. #853 still
needs a failed establishment history with both endpoints. #889 still needs the
missing label and endpoint transitions from a failed mesh run. Both stay open.

### F71 — Native deferred pairing restores removed peers

Issue [#1010](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1010)
affects the private native reference orchestrator under A20. While
`--p2p-release-file` holds pairing, `NewPeer` and WebRTC plans retain a deferred
directive. `remove_pair_obligation` cleared the live link, roles, signals, and
logical expectation, but retained that directive. Releasing the gate then
recreated the removed link and pairing obligation and sent an offer to the
removed peer.

At `a7abbd60`, the real-engine and real-WebSocket regression
`p2p_gate_does_not_signal_removed_peers` fails after dispatching `PlayerLeft`.
Nextest run `8f5291a9-44ae-492f-ac85-2e2335aa94bd` records the departure, then
releases two deferred directives and sends an offer to the departed identity.
The test fails in 0.678 seconds. The repair removes the pending directive at
the shared logical removal boundary. Restore already clears the full map;
retry and retained-peer replacement already remove their individual entries.

The retained table covers departure, a relay-only authoritative plan, a reduced
WebRTC plan, and departure followed by the same identity returning as answerer.
It checks the physical link, expected peer set, current role, and actual outbound
signaling. A retained offerer must still send its offer. A late signal from a
removed peer cannot recreate buffered signaling. The returning answerer must
not use the old offerer directive.

This is a controlled deferred-gate history, not an explanation of #853 or #889.
The fix changes no server wire format, defaults, or SDK API. Rust, .NET, and
Godot SDKs do not use this private native CLI pairing gate and need no migration.

### F72 — Pending browser negotiation delays relay gameplay and other peers

Issue [#1011](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1011)
affects the private browser reference orchestrator under A20. Earlier F50,
F53, and F54 repairs enforced absolute deadlines. Before expiry, an unresolved
browser operation still held the shared input chain. Relay gameplay and other
peers' negotiation waited for that operation or its deadline.

The new pre-expiry assertions fail on `a7abbd60`: an accountable relay message
remains queued while offer creation is held. The same matrix exercises local
description, remote offer, remote answer, answer creation, answer description,
live ICE, and buffered ICE. This is a controlled browser promise history, not
a diagnosis of the spontaneous #853 or #889 failures.

The repair schedules negotiation separately for each peer while preserving
that peer's SDP and ICE order. The room input chain continues to process relay,
membership, and timer work. Queued peer operations retain task backlog credits
until completion. Logical removal and authoritative replacement invalidate old
work. Retry keeps its current queue order. Operation timeouts compare physical
link generations before retiring a link, so an old wait cannot remove a fresh
replacement. Existing connection and run windows remain unchanged.

The focused peer-work regression also fails the baseline because the healthy
peer does not start while the first offer waits. Its repaired controls check
healthy Answer and ICE order, queued same-peer signals, held retry offers, and
same-generation removal and re-addition. Old work is resolved, rejected, or
left pending through its original deadline while the replacement stays live.
Small signal batches retain peer-task credits until the existing backlog limit
refuses further work. Browser tests, strict type checks, and the build pass.

The earlier shared-deadline test expected the second planned peer never to
start if the first peer used the full window. Both now start before the same
deadline. Neither starts or continues after expiry. The output-cancellation
control likewise expects two independently pending offers, and still requires
no late signaling or successful exit after output failure.

Rust, .NET, and Godot SDKs are unaffected: they do not import this private
browser orchestrator. The server protocol, defaults, and SDK APIs are unchanged.
No migration or external-client tracking issue is required for either repair.

The rebuilt native executable and browser bundle pass two restore and two
three-player mesh controls in 12.556 seconds. All ten endpoint streams retain
505 events, exact peer/label send and receipt counts, and successful exits.
All five binary and bundle hashes stay fixed during these final-source runs.
Real Chromium also passes seven delivery-mode controls, two channel-closure
controls, and four embedded-SDP controls. Hosted CI must validate the wider
interop suite. These bounded successes still do not explain #853 or #889.

Remote review found a second timer-order defect in the first F72 repair.
At the run cutoff, the room wake could evaluate success before an equally due
peer-operation timer cleared its physical work. The new generic pending-work
criterion then failed a run whose gameplay requirements were already met.

The retained eight-case matrix controls both timer orders. On `a0157029`,
a never-formed zero-P2P-timeout offer and a formed pair with settled exchange
receipts fail when the room wake runs first; both pass when the peer timer runs
first. Missing reliable or unreliable receipts fail in both orders. The first correction
retires pending physical work synchronously at the original non-deferred run
cutoff, before checking logical success. It adds no grace and preserves pending
work before cutoff, retry obligations, and exchange debt. Both timer orders now
agree. Late completion and late receipts cannot change the result. Browser
tests, type checks, the build, and independent adversarial review pass.

The corrected bundle passes two more restore and two more three-player mesh
controls in 11.199 seconds. Ten complete streams retain 508 events and exact
peer/label send and receipt counts, with no errors. Binary and bundle hashes
stay fixed throughout these final corrected-source captures.

A further review compared cutoff transport reports with original `a7abbd60`.
With the default 15-second P2P timeout, held ICE already reports transport
`true` then `false` and fallback at the active run deadline. Both original timer
orders succeed. The held-ICE report itself is not new behavior.

A narrower completion history exposes a real regression in `5cd5c305`.
An Answer completes before cutoff and its operation timer is canceled. The
Answer send then triggers the room wake before the detached handler finalizer
clears its task entry. Original main succeeds with transport `true` only;
`5cd5c305` succeeds but adds a false loss and fallback report. The retained
regression fails that exact report on `5cd5c305`.

The final repair tracks active browser-operation expirations separately from
peer-task bookkeeping. At cutoff it expires only due active waits through the
existing physical-generation checks, then invalidates queued and finishing
handlers without reporting physical loss. The 18-case matrix covers both timer
orders, zero and default P2P timeouts, completed Answers, and each missing
exchange label. Held-ICE fallback still matches original main. Completed
Answers do not report loss. Missing receipts still fail. Package checks and
independent adversarial review pass.

The final active-wait repair passes two restore and two three-player mesh
controls in 10.269 seconds. Ten complete streams retain 502 events and exact
peer/label send and receipt counts, with no errors. Source and all five binary
and bundle hashes remain fixed during these latest controls.

### F73 — Browser pair retries wait behind the physical link they replace

Issue [#1013](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1013)
affects the private browser reference orchestrator under A20. F72 separates
peers' SDP and ICE queues, but a retry still enters the old physical link's
queue. If that link's browser operation remains pending, the retry waits.
The old operation's deadline then retires the queue token and discards the
retry. The peer cannot recover within the original connection window.

The regression on `6aa69c25` holds offer creation, queues old Answer and ICE,
then sends retry attempt one and replacement Answer and ICE. It fails because
no replacement link starts before the unchanged deadline. The retained matrix
covers offer creation, local offer, remote answer, remote offer, answer
creation, local answer, and live ICE. Each old operation resolves, rejects,
or remains pending. All 21 repaired histories create a fresh link without
waiting for the old operation.

The room dispatcher now recognizes an eligible new retry before entering the
peer queue. It cancels the old queue token and starts a new queue. The existing
retry handler retires the physical link and keeps its original deadline.
Both paths use the same eligibility predicate. Old queued SDP and ICE cannot
reach the replacement. Replacement SDP still precedes ICE. Stale generations,
unplanned peers, invalid attempts, and duplicate attempts cannot interrupt
valid work. Late old completion and expiry cannot close the replacement or
send old SDP. Existing exchange debt and recovery-after-formation controls
remain in the browser suite.

Browser tests, type checks, formatting, and the build pass. The rebuilt bundle
passes two retry, two restore, and two three-player mesh controls in 16.983
seconds. Fourteen complete endpoint streams retain 700 events. Binary and
bundle hashes remain fixed across these six runs. These bounded successes do
not establish the spontaneous causes in #853 or #889; both remain open.

Rust, .NET, and Godot SDKs are unaffected because they do not import this
private browser orchestrator. The server protocol, defaults, and public SDK
APIs are unchanged. No migration or external-client tracking issue is needed.

### F74 — Retired browser buffered signals reach replacement peer links

Issue [#1015](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1015)
continues A20 at `cb9c08ef`. A planned peer can lose its physical link before
SDP and ICE arrive. The browser buffers those signals until a pairing directive
creates a new link. Its buffer drain checks the task token and session before
the loop, then awaits each entry. A retry or authoritative replacement can
retire that task during an await. When the old operation completes or fails,
the old loop applies its next entry through the replacement's engine link and
captures the replacement task token. Old ICE and SDP can alter the new link.

The deterministic regression fails against the unchanged baseline orchestrator
when a retry replaces a held remote-offer operation. The old operation then
completes, and its buffered ICE reaches the replacement. The repair rechecks
the original session, task token, and planned membership before each buffered
entry. Existing physical-link checks still fence the current awaited operation.

The 64-case matrix holds remote-offer application, answer creation, local-answer
application, or buffered ICE. Each held operation completes, fails, or remains
pending across a retry, a changed plan generation, departure and rejoin, a fresh
player incarnation, or transport loss. Four uninterrupted controls drain both
ICE entries and send both buffered-offer answers. Replacement SDP precedes its
ICE. Late old completion, failure, and expiry cannot add old ICE, send old SDP,
report a current-link error, or close the replacement.

The browser test suite, type checks, build, and formatting pass. Fresh baseline
retry, restore, and three-player mesh controls each pass twice with fixed file
hashes and complete endpoint streams. The rebuilt browser also passes two
retry, two restore, and two mesh controls in 17.359 seconds. Fourteen complete
endpoint streams retain 702 events. File hashes remain fixed across all six
runs. These bounded successes do not identify
the spontaneous causes in #853 or #889. Both issues and the broader #826 audit
remain open.

Rust, .NET, and Godot SDKs are unaffected because they do not import this
private browser orchestrator. The server wire contract, defaults, and public
SDK APIs are unchanged. No client migration or acceptance issue is needed.

### F75 — Windows deadline fixtures complete before backpressure

Issue [#1017](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1017)
continues A22. Scheduled WebRTC run `38853983812` failed two native deadline
fixtures on Windows at `3330d6c`. Both failures occurred at their setup
assertion, before simulated time advanced. Linux and macOS passed. A fresh
manual run `38961414369` reproduced both failures on `d2878e4a`.

Diagnostic head `bdcbbe10`, run `38962172874`, retains the completed results.
The Pong fixture returns `Ok(Ok(None))`; the text-write fixture returns
`Ok(Ok(()))`. Both writes succeeded before the 50 ms setup observation ended.
The 8 MiB frame and 1,024-byte sender buffer did not establish the assumed
backpressure on Windows. The evidence does not identify the internal Windows
buffering mechanism or show a production deadline failure.

Constraining the listener alone also failed in run `38963141010`, head
`866b5f43`, with the same successful-write results. That candidate does not
establish a repair. The revised shared fixture sets the accepted socket's
receive buffer explicitly and records its inherited and configured values.
It sends 8 MiB fixture frames until a write remains pending for 50 ms, with
a hard limit of 16 frames. A setup error or failure to establish backpressure
still fails the test. The fixture records the exact frame count.

The deadline controls retain both text and binary writes, the original 8 MiB
application payloads, the 50 ms pending-write assertion, and the complete
20-second write budget. The Pong control still advances 12 seconds during the
blocked write, drains and checks every queued frame before Ping, and requires
the full response window after successful write completion. No production
code or timeout changes. Hosted CI owns cross-platform acceptance.

The browser buffered-signal repair in F74 remains independent. Rust, .NET, and
Godot SDKs are unaffected because this change only adjusts native reference
unit-test sockets. No public API, wire, or migration change is needed. The
broader A22 and #826 audits remain open.

### F76 — Browser link retirement removes failure diagnostics

Continues A20 and A22 in [#826](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/826),
[#853](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/853),
and [#889](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/889).
The browser removes a failed physical link before its P2P timeout snapshot.
The later snapshot can be null. Connection-state and channel-close events do
not retain the channel buffers, observed open labels, or SCTP state at loss.
This prevents a later timeout log from distinguishing these terminal histories.

The browser now writes a snapshot to stderr before removing a failed link.
It identifies a failed or closed connection, a closed or failed required
channel, or an expired negotiation operation. Each snapshot includes the
physical generation, connection and ICE states, SCTP state, and both required
channels. Intentional plan changes, departures, and retries do not report a
failure. Old operations cannot report a loss against a replacement link.
The existing timeout snapshot remains available for links that still exist.

The 124-case deterministic regression fails on the unchanged browser because
the loss snapshot is absent. It covers held offer application, answer creation,
local-answer application, and ICE application. Connection loss, required-channel
close and error, and operation timeout each retain the retiring generation.
Old operations complete, reject, or remain pending. Late old results and
expiry do not report replacement loss. Uninterrupted drains, plan replacement,
retry, departure, and incarnation controls retain their original behavior.

This repairs missing observations. It does not establish the cause of the
original spontaneous establishment or mesh failures. Keep #853, #889, and
broader #826 open. Rust, .NET, and Godot SDKs are unaffected because they do
not import the private browser orchestrator. Wire formats, JSONL event types,
public SDK APIs, and recovery behavior do not change.

The complete browser unit suite, type checks, build, and formatting pass.
The rebuilt bundle passes two native/browser retry controls, two restore
controls for #853, and two three-player mesh controls for #889 in 19.969
seconds. All fourteen endpoint event streams and server logs are retained
locally under `progress/session-461-final-interop`. Executable and bundle
hashes match before and after the six runs. No original spontaneous failure
recurs. These runs qualify the added diagnostics without settling either
original cause.

### F77 — Native link retirement removes failure diagnostics

Issue [#1019](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1019)
continues A20 and A22 at `5db7ce37`. Native connection failure, required-channel
closure, receive rejection, and exchange-send failure remove the engine link
without retaining its observations. Negotiation expiry also removes the link
inside the engine before returning to the orchestrator. A later P2P timeout
snapshot can therefore be null. Browser F76 does not cover these native paths.

The native client now reports the peer, reason, and existing engine snapshot
before these removals. The snapshot retains the physical generation, candidate
counts, remote-description observation, and both channel registration and open
observations. It makes no driver calls. An observed open callback does not
establish current channel readiness. The record uses the existing bounded
stderr writer. Intentional departures and replacements do not report failure.

The new regression captures real tracing output. It fails against the unchanged
runtime because the first failed connection produces no snapshot. Its twelve
histories cover failed and closed connections, both required channel labels,
receive rejection, already-expired negotiation, a pending operation, and a
result ready at expiry. They also cover intentional retirement and stale
events. Retired callbacks cannot duplicate loss or affect a fresh link.
The existing held-negotiation controls now require loss snapshots at every
retained-link stage. Route and build cancellation occur before engine storage
and must not fabricate link observations. Eight real-wire send-timeout histories
retain both channel observations before removal while preserving unpaid
exchange debt and live relay traffic. All forty selected native tests pass.

This repairs missing observations. It does not identify the spontaneous causes
in #853 or #889. Both issues and the broader #826 audit remain open. The
independent Rust, .NET, and Godot SDKs are unaffected: this change adds stderr
records only in the native reference engine and orchestrator. Wire formats,
JSONL event types, public APIs, and recovery behavior do not change.

The initial native build passes two browser retry controls, two restore
controls for #853, and two three-player mesh controls for #889 in 17.075
seconds. Fourteen complete endpoint streams retain 701 events, including
48 sends, 48 receipts, fourteen success barriers, and fourteen exits with code
zero. No endpoint emits an error. All server logs and file hashes are retained
locally under `progress/session-462-final-interop`. The five executable and
bundle hashes match before and after all six runs. No original spontaneous
failure recurs. Root and native formatting and strict all-target, all-feature
clippy pass. The browser unit suite and bundle build also pass.

### F78 — Withdrawn dependency blocks package verification

Issue [#1021](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1021)
records a root verification failure at PR #1020 head `91757b40`. Cargo reports
that locked `inout 0.2.3` was withdrawn from crates.io. The package control
rejects that warning. The suite stops after 2,726 passes and one failure,
leaving 524 tests and the later allocation and H14 gates unrun.

The registry lists compatible `inout 0.2.4` with Rust 1.85 support. The repair
updates only that package and its checksum in the root, native, and fuzz
lockfiles. No other retained lockfile contains `inout 0.2.3`. Manifests and
package-verification assertions remain unchanged. The source delta affects
only the optional `block-padding` feature, which the server does not enable
even with all root features. This is a package publication repair.

The old lockfile reproduces the hosted warning at the same package assertion
(Nextest `5b2c2a1c`). With `0.2.4`, the unchanged control passes in 195.631
seconds (`45c11115`), including the generated archive's all-feature library
test compilation. Root and native strict lint pass on the updated lockfiles.
The package assertions, warning filters, and workflow coverage are unchanged.
No dependency-only changelog entry is needed because the enabled runtime
code and public contracts do not change.

The final `0.2.4` builds pass two browser retry, two restore, and two
three-player mesh controls in 15.717 seconds. Fourteen complete endpoint
streams retain 702 events, 48 sends, 48 receipts, fourteen success barriers,
and fourteen exits with code zero. No endpoint emits an error. All five file
hashes match before and after the six runs. Full endpoint and server logs
remain under `progress/session-462-lockfix-interop`. These runs also provide
new #853 and #889 attempts on the updated graph; neither original failure
recurs, so neither investigation is complete.

### F79 — A crossed old answer replaces fresh retry state

Issue [#1022](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1022).
Baseline `24487aee`. A20. The native reference client can request a physical
peer retry while an old answer travels back from the remote peer. The server
preserves each sender's FIFO order, but cannot order those opposite directions.
The local callback generation fences retired callbacks. The server session
generation remains unchanged across the physical retry. Neither excludes the
old answer carried by the signaling socket.

A real-engine experiment creates Offer0 and Answer0, holds Answer0, replaces
the initiator, and creates Offer1. The replacement accepts held Answer0. After
the responder replaces its link, its fresh Answer1 fails because the initiator
is already in the stable signaling state. The experiment uses the repository's
qualified RTC, SCTP, and WebRTC sources. This confirms a new retry defect;
it does not identify the original no-retry failures in #853 or #889.

The repair carries an attempt number on post-retry SDP and ICE signals in both
reference clients. Each endpoint applies only traffic for its current physical
attempt. Bare Matchbox signals remain valid before the first retry. Retry
markers, configured limits, server generations, and server-assigned roles
keep their existing meaning. Retry-enabled endpoints must upgrade together.
The default retry count remains zero.

The baseline native orchestrator regression fails in 0.654 seconds (Nextest
`53131252`): the old answer consumes the replacement offer's answer slot.
The repaired regression rejects that old answer, applies a tagged fresh answer
and ICE through the orchestrator, opens both replacement channels, and delivers
fresh gameplay in both directions. The 52 scoped native retry and negotiation
controls pass in 16.587 seconds. The shared test helper keeps its direct-engine
path for initial links whose earlier client deadlines deliberately expired.
Post-retry links use the tagged orchestrator path.

Browser dispatch controls cover both offerer roles, default bare signaling,
successive retries, wrong attempts, and malformed envelopes. Stale browser
inputs in this test are defensive controls: the browser only accepts retries
from peers, so its own sender FIFO excludes the native requester's crossed
answer history. Existing stalled-work, retirement, and authoritative-reset
assertions remain intact. Independent reviews found no remaining source
findings. These controls establish attempt isolation, not the causes of #853
or #889, and do not complete A20 or #826.

Eight fixed-binary live controls pass in 24.918 seconds: two native/native
retries, two browser/native retries, two browser/native restores for #853,
and two three-player meshes for #889. Eighteen complete endpoint streams retain
922 events, 56 sends, 56 receipts, eighteen success barriers, and eighteen
exits with code zero. No endpoint emits an error. All six executable and bundle
hashes match before and after the controls. Full endpoint and server logs
remain under `progress/session-463-live`. Neither original spontaneous failure
recurs, so #853 and #889 remain open. Root and native strict full lint gates,
browser type checking, and the browser suite and build pass.

### F80 — Retain peer state when gameplay exchange misses the run cutoff

Continues A20 in [#826](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/826),
[#853](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/853), and
[#889](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/889).
Baseline `fac957f1`. The original #889 history has connected channels but unpaid
exchange receipts at the end of the run. Existing peer snapshots cover formation
timeouts and physical-link loss. Neither reference client captures a snapshot
for a link that stays connected with unpaid gameplay exchange at run timeout.
The final error names missing labels, but cannot recover the lost channel state.
This is a confirmed diagnostic gap, not a confirmed cause of the historical
missing receipt or establishment failure.

Both reference clients now report each unpaid exchange peer before run teardown.
Each report includes missing send and receipt labels, logical connection state,
and the existing physical-link snapshot. Completed peers are excluded. Departed
peers keep their logical debt and report a null link. Exchange-disabled runs
emit no exchange report. Output uses the existing bounded diagnostic path and
does not query or wait for the WebRTC driver. Original run cutoffs, exit codes,
exchange assertions, and retry behavior remain unchanged.

The same final browser regression fails against baseline source with
`missing-reliable: snapshot only unpaid peers once`. The repaired controls
cover missing sends and receipts on each required label, live and removed
links, paid obligations, and the original zero/default P2P deadline histories.
A blocked send retains its earlier 50 ms retry wake; delivering that wake late
at the original run cutoff still captures the live unpaid link. Browser build,
tests, type checking, and formatting pass. Native baseline Nextest `171e1247`
fails because a connected unpaid peer emits no snapshot. Repaired Nextest
`0e22cab6` passes both focused controls in 0.188 seconds, with reliable,
unreliable, and both-label debt in both directions, completed and disabled
exchange, and removed links. Root and native strict full lint gates pass.
Independent source review found no remaining issues.

The new native real-wire control holds unreliable exchange release at both
connected endpoints. Both complete reliable exchange before the cutoff client
exits with the original criteria-unmet code. The incumbent remains live.
Nextest `15e88bb0` passes in 8.358 seconds. The report names only unreliable
send and receipt debt and retains both registered, open-observed channels.
A separate browser/native control holds the native unreliable send. Chromium
exits with code one while the native remains live. Its report has no missing
send, one missing unreliable receipt, connected PC/ICE/SCTP, and both required
channels open with zero buffered bytes. Four binary/bundle hashes match before
and after. A first attempt with the old browser bundle correctly lacks the new
report; it remains recorded separately. These are controlled omissions, not
reproductions of the original spontaneous failures. Full logs remain under
`progress/session-464-native-cutoff`, `progress/session-464-browser-cutoff`,
and `progress/session-464-browser-cutoff-stale-bundle`.

Twenty fresh baseline fixed-binary histories also pass in 45.793 seconds: ten
restores for #853 and ten three-player meshes for #889. Fifty complete endpoint
streams retain 2,517 events, 200 channel opens, 200 sends, 200 receipts, fifty
success barriers, fifty exits with code zero, and ten native restores. Five
executable/bundle hashes match before and after. All attempts and server logs
remain under `progress/session-464-live`. These baseline runs do not validate
later source changes or explain either historical failure. Both issues and the
broader A20 and #826 audits remain open. Rust, .NET, and Godot SDKs need no
change: this repair adds only private reference-client stderr reports and
changes no server API, wire message, or SDK event contract.

### F81 — Initial native exchange bypasses per-label signaling service

Issue #1025, found at `496d020a` while continuing #826, #853, and #889.
The release gates queue exchange labels separately, but `on_pair_connected`
still awaits both labels within one engine-event handler. Signaling that
arrives during the first send cannot run before the next label starts.
Each local send can consume its existing one-second limit.

The new real-WebRTC and loopback-socket control holds the first successful
send. The signaling peer queues Pong, relay GameData, and Close before it
releases that send. Baseline Nextest `3409c37a` fails with two send calls
instead of one. The assertion checks causal ordering without a timing bound.
This is a controlled scheduling history, not a captured spontaneous stall.

Formation now queues its labels through the existing scheduler. Initial and
replacement links use the same channel-identity fencing, one-label dispatch,
input-burst bound, send deadlines, failure cleanup, and unpaid exchange debt
as the release gates. The existing manual recovery fixture now drives queued
sends before checking its exact send and receipt evidence.

Three fresh fixed-binary restore attempts and three three-player mesh attempts
pass at the baseline. Every endpoint stream and server log is retained, with
matching executable and bundle hashes before and after capture. These passes
neither validate the later source change nor explain the original failures.
Issues #853 and #889 remain open with their original acceptance.

The Rust SDK provides signaling without a WebRTC backend. The .NET SDK ships
its own signaling assembly. Godot uses its engine-owned WebRTC mesh. None
uses this private native reference orchestrator. All three are unaffected;
server wire messages, SDK APIs, defaults, and deadlines are unchanged.

Final scoped Nextest `d9da2b9e` passes all 15 retry and exchange controls,
including this regression, both release gates, stalled sends, expired and
replaced channels, ready-callback bursts, signaling failure, and real retry
recovery. The rebuilt native binary passes three restore and three mesh
controls: 15 endpoint streams, 758 events, and 60 exact sends and receipts.
All executable and bundle hashes match before and after those runs; only the
native binary differs from the baseline capture. Native and root strict
Clippy, formatting, CI configuration, Markdown, and documentation checks pass.
Hosted final-head CI and feedback remain publication gates.

The first hosted native run passes 244 unit controls but the new fixture
misses queued relay input. Its server task drops the socket immediately after
sending Close, while client output can remain unread. The fixture now retains
the socket until the client stops and checks the intended close reason.
With that lifetime fix, the regression still fails against the original
production code in Nextest `21299aec`: two sends start instead of one.
The production repair is unchanged.
The corrected fixture passes 20 consecutive focused stress runs. All 15
retry/exchange controls and strict native Clippy pass again.
The next hosted run retains the socket and consumes the relay, but still
starts two sends. Server write completion does not establish readiness on
the client socket. The final fixture peeks through the empty server Close
frame on a duplicated client socket before releasing the first send. It
consumes no input; the real run loop must still process every queued frame.
This removes the transport-arrival assumption without adding a sleep or
changing the send-order assertion.
The socket-readiness fixture passes 20 consecutive focused runs and remains
red against the original handler in Nextest `17245b3d` (two sends versus one).

### F82 — Browser exchange retries can pay expired send debt

Issue #1027, found at `26adb27c` while continuing #826, #853, and #889.
The browser room timer flushes pending exchange sends before it evaluates
its run deadline. A channel can reject a probe before expiry and accept it
when a delayed retry wake runs at expiry. The late send clears the last
required debt, so the client reports success outside its original window.

The final regression fails against baseline source because an exchange send
starts at the cutoff. The initial control also observes exit zero instead
of the existing criteria-unmet code. This controlled channel-readiness
history does not explain either original spontaneous interop failure.

Each exchange label now checks the original monotonic run cutoff before
it starts a send. An expired run retains its missing-label debt, the live
peer snapshot, and the existing failure code. The explicit success hold
keeps its existing deadline deferral and permits fresh exchange after a
peer restore. No timeout or exchange requirement changes.

The expanded controls cover reliable and unreliable send recovery at the
exact cutoff and one millisecond later, both zero and default P2P windows,
and successful retries before expiry. A held-success restore control crosses
the original run deadline, sends both fresh labels, requires both new
receipts, and exits only after release and linger.

The Rust SDK owns no WebRTC backend. The .NET SDK handles signaling and relay.
Godot uses its own engine mesh. None uses this private browser run scheduler;
all three are unaffected. Server messages, SDK APIs, and defaults are unchanged.

Fresh fixed-binary baseline captures pass three browser/native restores and
three three-player meshes. All endpoint streams, server logs, and binary
and bundle hashes are retained under `progress/session-466-baseline-live`.
The rebuilt browser bundle also passes three restores and three meshes:
15 endpoint streams, 759 events, and 60 exact sends and receipts. Hashes match
before and after each capture set; only the browser page bundle differs
between the two sets. The browser build, full tests, type checks, and formatting
pass. Root strict Clippy, formatting, CI configuration, and documentation
checks pass. Manual adversarial review finds no remaining issue in this diff.
Passing captures do not explain the historical failures; #853, #889, and the
broader #826 audit remain open. Hosted CI and feedback remain delivery gates.

### F83 — Late browser receipts can pay expired exchange debt

Issue #1029, found at `f8098558` while continuing #826, #853, and #889.
The browser fences exchange sends at its run cutoff, but accepts receipts
without checking their arrival time. A last receive callback can clear missing
exchange debt before its handler runs the overdue timer. The client can report
success outside its original window.

The final regression fails against baseline source: a reliable receipt at the
exact cutoff produces exit zero instead of criteria-unmet. The clock starts at
100 ms and the one-second cutoff is 1100 ms. Both local sends and the other
receipt have completed. This is a controlled callback history, not a capture
of either historical interop failure.

Send and receipt paths now share the original monotonic exchange window and
its explicit held-success deferral. Receipt eligibility belongs to arrival;
eligible queued receipts drain before the cutoff decision. Each handler still
checks its physical-link generation before crediting the receipt. Late traffic
remains observable and the missing label stays in the timeout snapshot.

Controls cover both required labels at cutoff and one millisecond later,
zero and default P2P windows, pre-cutoff receipt success, queued receipts behind
blocked output, and queued receipts from retired links. Removing the drain
safeguard makes the queued reliable-receipt control fail with exit one. Existing
held-success restore controls still require fresh sends and receipts after the
original deadline. No timeout or success criterion is relaxed.

Fixed-binary baseline capture passes three native restores and three
three-player meshes. The rebuilt browser passes the same six controls. Each
capture retains every endpoint stream, server log, and before/after executable
and bundle hash. Only the browser page bundle changes between capture sets.
The full browser tests, build, type checks, and formatting pass. Root strict
Clippy, formatting, CI configuration, and documentation checks pass.
These bounded passes do not explain #853 or #889; both remain open.

Current external SDK READMEs were inspected through the GitHub connector.
Rust supplies signaling without a WebRTC backend. .NET supplies signaling and
relay. Godot uses its engine-owned WebRTC mesh. None uses this private browser
run scheduler, so all three are unaffected. No server message, SDK API, channel
mode, or default changes. Hosted current-head CI and feedback remain delivery
gates.

### F84 — Queued native receipts can miss the run cutoff

Issue #1031, found at `5933b6be` while continuing #826, #853, and #889.
The native loop checks its run deadline before consuming queued engine events.
A required receipt already queued before expiry can remain unpaid, causing
criteria-unmet exit despite completed exchange. The receive handler also lacks
arrival evidence, so draining alone could count a late receipt as timely.

Baseline Nextest `1dd55357` fails with exit one instead of zero while the last
reliable receipt is queued. The engine now records monotonic publication time
on each message. At expiry, the orchestrator consumes the existing callback
queue in FIFO order before judging success. It credits only pre-cutoff receipts
from the current physical link. Late traffic remains visible in event output.
The queue snapshot bounds the drain; new traffic cannot renew the run window.

Controls cover both labels, zero and default P2P windows, arrivals one
millisecond before, exactly at, and one millisecond after expiry, queued
receipts, stale and removed links, close-before-message and message-before-close
ordering, and fresh exchange during explicit held-success restore. Removing
only the arrival guard makes the exact-cutoff control fail. Existing deadlines,
missing-label debt, callback permits, and held-success behavior remain in force.
This is a controlled history; it does not identify the historical causes of
issues #853 or #889. Both remain open with their original acceptance.

Current external SDK READMEs were inspected through the GitHub connector.
Rust supplies signaling without a WebRTC backend. .NET supplies signaling and
relay. Godot uses its engine-owned WebRTC mesh. None uses this native reference
orchestrator; all three are unaffected. No server message, SDK API, channel
mode, or default changes.

Native receipt qualification in Nextest `4de691c4` passes all 19 exchange, retry, membership, and
receipt controls. The new matrix covers 68 histories. Native-only baseline and
candidate captures each pass three native restores and three three-player meshes, with
15 endpoint streams, 60 sends, 60 receipts, and stable before/after executable
and bundle hashes. Only the native binary changes between capture sets.
Strict native and root Clippy, formatting, CI configuration, Markdown, and
documentation checks pass. Hosted current-head CI and feedback remain delivery
gates.

PR review found that the first hold exception stayed active after release
and after linger ended. Nextest `9f86ce54` reproduces the gap: a late reliable
receipt pays fresh debt after release. Receipt eligibility now uses the
existing cutoff-deferral rule with the current hold state and the receipt's
position before the linger deadline. Added cases retain held and active-linger
exchange, and reject late receipts after release or at an expired linger.

A second review found that linger expiry cleared its deadline before queued
receipts could use it. Nextest `49878972` reproduces the failure with a reliable
receipt queued during linger and processed at expiry. Both expiry paths now
share a bounded FIFO drain. Linger keeps its deadline until eligible queued
receipts have been applied. Added controls reject queued arrivals exactly at
and after linger expiry while retaining an earlier queued receipt.

### F85 — Browser exchange can continue after success linger expires

Issue #1033, found during the #1031 class sweep at `545fb785`. The browser
exchange guard treats any non-null linger deadline as active. A callback or
retry that runs before the overdue timer can pay new exchange debt at or after
linger expiry. The normal run-window guard does not close this hold exception.

The public-dispatch regression completes initial held exchange, releases at
1200 ms, and retains the original linger cutoff at 1450 ms. A SessionPlan adds
an already-present third peer. Its final reliable receipt at 1450 ms produces
exit zero on the baseline, instead of criteria-unmet. Removing only the repaired
linger guard also lets a retry send at 1450 ms report success. Both negative
controls retain the passing 1449 ms history.

The shared browser send and receipt guard now checks the monotonic linger
deadline. The 36 new histories cover both labels, zero and default P2P windows,
receipts, retry sends, and queued receipts before, exactly at, and after expiry.
Timely queued receipts remain eligible after delayed dispatch. Active holds,
physical-generation checks, original deadlines, and missing-label debt remain
in force. Full browser tests, build, type checks, and formatting pass.

### F86 — Native held-success sends can pay expired debt

Issue #1034, found in the same sweep at `545fb785`. Native negotiation permits
operations after reported held success, but the send path does not check the
actual hold or linger window. A successful local send can pay debt after the
release marker exists and linger has ended.

Baseline Nextest `f238a1ed` fails at a reliable send completing exactly at the
100 ms linger cutoff, inside the existing one-second local send budget.
Active-hold and 99 ms completion controls pass first. The repair captures
an exchange-window end when each send starts and caps its local wait. A send
admitted during a hold keeps its original local deadline across release.
Expired starts never call the sender, and completions past the captured
deadline retain unpaid debt and existing failed-link cleanup.

Twenty-eight new send histories cover both labels, zero and default P2P windows,
active hold, timely linger completion, exact and late expiry, and start after
release without linger, plus release during a send and a queued held receipt.
They check invocation, debt, physical ownership, and
exit behavior. The full focused scope retains the existing real relay, stalled
send, callback-ordering, retry, and membership controls.

The F84 external SDK assessment also applies to F85 and F86. These changes
affect only the private reference-client schedulers. Server wire messages,
SDK APIs, channel modes, and defaults do not change. Neither controlled defect
identifies the historical causes of #853 or #889; their acceptance stays open.

The combined candidate in Nextest `5c1b1bdc` passes all 20 scoped controls, including 68
receipt histories and 20 new send-window histories. Strict native Clippy and
build pass. The rebuilt browser and native pair pass three room restores and
three three-player meshes. Every endpoint stream and server log is retained
with stable before/after hashes under `progress/session-468-window-qualified-live`.
Only the native binary and browser page bundle differ from baseline. Hosted
current-head CI and feedback remain delivery gates.

Review also exposed a release-observation race. A live release check at send
completion can revoke a valid admitted send before linger is armed. The
negative control in Nextest `782ef184` reproduces that failure. Dropping held
receipt history also loses a queued receipt that arrived before release;
Nextest `fc09dafd` proves that separate failure.

Native sends now keep their captured admission deadline. The client records
the hold start and first release observation on the monotonic clock. Queued
receipts retain eligibility inside that completed hold interval. New sends
use the current window, and a release marker cannot reopen the same success barrier's
hold after observation. Added boundaries cover arrivals before and exactly at hold start,
exactly at release observation, and after release. The native receipt matrix
now has 80 histories and the send matrix has 28. The browser adds 36 histories.

Owned-window qualification in Nextest `4bc0e448` passes all 21 focused
controls, including 80 receipt and 28 send histories. Strict native Clippy
and the native build pass. The rebuilt pair passes another three restores
and three three-player meshes with stable hashes. Only the native binary
and browser page bundle differ from baseline. Evidence remains under
`progress/session-468-owned-qualified-live`.

Room restore starts a fresh success barrier. A further review found that the
new hold timestamps were not cleared with the existing report and linger
latches. The restored snapshot now discards both timestamps. Its behavioral
control first credits an old held receipt and observes a one-way release,
then restores the room. The old receipt must become ineligible, and the new
barrier must inspect its release marker again. Invalid snapshots still keep
the previous state.

Baseline Nextest `23ef9a1f` fails the restored-room receipt check. Final
Nextest `e5b413fc` passes all 22 focused controls, including both reset
behaviors, the original invalid-snapshot checks, and all exchange boundaries.
Strict native Clippy and build pass.

The strengthened snapshot control also passes in Nextest `64315ced`. It
checks both timing invariants after every rejected snapshot, then checks the
fresh phase after valid restore. Strict native Clippy and build pass again.

### F87 — Replacement links skip probes lost on the old link

Issue #1035, found at `f95307ed` while continuing #826, #853, and #889.
Both reference clients use logical sent-label evidence to suppress sends on
replacement physical links. An accepted local send does not establish remote
delivery. If the old link loses that probe before delivery, a successful retry
or authoritative replacement cannot pay the remote endpoint's receipt debt.

The native regression opens real paired channels and injects one accepted,
undelivered old-link send. Remote channel events establish the missing label.
The baseline opens a replacement but times out waiting for the missing receipt.
Four histories cover both labels and retry or authoritative replacement. The
browser dispatch matrix models accepted old-link sends that never reach its
remote receipt ledger. The baseline fails the replacement-send assertion.

The clients now track successful sends per physical link separately from
logical exchange evidence. Fresh channels resend both probes within the existing
exchange window. Duplicate work on the same channel does not resend. Logical
sent and received evidence survives replacement; a membership incarnation still
requires fresh exchange. Existing run and linger cutoffs, release gates, and
physical callback ownership remain in force.

Review also checks pending replacement sends before success. Old logical
evidence must not let a client exit while fresh probes wait in its send queue
or retry loop. Retired links do not keep physical send work due. This check
preserves the original run and linger cutoffs.

Initial live Nextest `24edb566` passes native restore and the three-player browser
mesh, with five endpoint streams, server logs, and stable binary and bundle
hashes retained under `progress/session-469-baseline`. These passing histories
do not establish the historical causes of #853 or #889. Both issues remain open.

The current external SDK READMEs were inspected through the GitHub connector.
Rust provides signaling without a WebRTC backend. .NET provides signaling and
relay. Godot uses its engine-owned mesh. None uses these private reference-client
send schedulers. Server messages, SDK APIs, channel modes, and defaults do not
change. Final native Nextest `07c61c30` passes the four real-pair recovery
histories and 40 send-window histories. Full browser tests, type checks, strict
native and root Clippy, formatting, and documentation checks pass. Live candidate
qualification in Nextest `0ac3274f` passes three retries, three restores, and
three three-player meshes. All 21 endpoint streams and server logs are retained
under `progress/session-469-qualified-live`, with unchanged before/after hashes.
Only the native binary and browser bundle differ from baseline. Hosted CI and
feedback remain delivery gates.

Hosted interop found two late-join assertions that still required one probe
across all physical links. Both failures retain the expected replacement
probes. The native reproduction `71369712` fails the same assertion locally.
The late-join controls now drain each physical exchange before shared release
and check exact sender/label counts on both sides of the replacement. Each
incumbent has six sends and six receipts; the joiner has four of each. Logical
pair formation still occurs once per peer.

The repaired late-join controls pass in native Nextest `3709fe7d` and real
Chromium/native Nextest `696cbc98`. Targeted strict Clippy, formatting, and
Markdown checks pass. Production code and the fixed-binary qualification
artifacts do not change in this follow-up.

### F88 — Relay timers can complete expired reference-client runs

Issue [#1037](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1037),
found at `c6d7bd90` while continuing #826, #853, and #889.
Both reference clients process the relay-settle timer before judging the run
cutoff. A delayed timer can send the final required probe at expiry and then
arm a fresh success linger. Native relay writes also capture a fresh wire
limit instead of the remaining run window.

Native Nextest `3510270a` fails because `relay_sent` becomes true at the exact
cutoff. The browser real-dispatch regression fails at `json/1100`: one probe
reaches the socket after its run expires, instead of zero. A separate native
real-socket control fills the outbound path before starting the relay write.
The original source does not end that pending write at the run cutoff. The
control retains the original 20-second wire limit and shortens only the test's
run window; it does not emulate a WebRTC driver stall.

The browser now checks its existing run/hold/linger window immediately before
the probe. Native admission uses the same existing window policy and passes
its captured absolute cutoff to the common wire sender. That sender still
caps work at 20 seconds and rejects late completions. A canceled native write
ends the signaling connection with exit code 3; it cannot safely reuse a
partially written frame. A timer that has already expired keeps unpaid send
debt and the existing criteria-unmet exit code. No duration is renewed.

Boundary controls cover JSON, MessagePack, rkyv, and protobuf, valid sends,
exact cutoff, delayed timer wakes, established successful holds, and active
and expired linger. Socket controls cover all four native encoding paths and
retain unpaid debt after write cancellation. Payload assertions preserve the
old JSON envelope, MessagePack value, and opaque sender header.

The live baseline passes room restore and the three-player browser mesh,
with all five endpoint streams and server logs retained under local
`progress/session-470-baseline`. These controlled defects do not identify the
historical causes of #853 or #889. Their original acceptance remains open.
The Rust, .NET, and Godot SDKs own independent send APIs and do not import these
private reference-client process timers. No server protocol or SDK API changes.

Initial fixed-binary Nextest `97236547` passes three restores, three link retries,
and three browser meshes. All 21 endpoint streams and server logs are retained
under local `progress/session-470-fixed-live`, with 78 sends and 78 receipts.
Before/after hashes match for the server, native process, browser CLI, and page
bundle. The earlier candidate `2c3307c1` rebuilt the native process at setup;
its pre-build hash is excluded. Native scoped `9bf9bb10`, full browser tests,
and strict native/root lint also pass. Hosted CI and PR feedback remain the
publication gates.

Final review found another boundary in the initial browser repair: payload
preparation runs after the early window check. Encoding work or process
preemption can cross the cutoff before the prepared frame reaches the socket.
The browser also checks the window in the encoded-frame callback and retains
send debt if that final admission fails. Controlled payload preparation crosses
the exact cutoff in each encoding; this checks elapsed work between admission
and send without claiming a spontaneous browser incident.

After that final-admission repair, fixed-binary Nextest `06ad856b` passes the
same nine live cells. All 21 endpoint streams retain 78 sends and 78 receipts
under local `progress/session-470-final-live`. All executable and bundle hashes
match before and after. The browser's 28 regression histories, full package
tests, type checking, build, and formatting pass. Independent final review
reports no remaining findings.

### F89 — Relay receipt timing can misclassify a run

Issue [#1039](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1039),
found while sweeping #1037 at `1cf73bbb` under the same #826 audit.
A browser client joins a two-player lobby, finalizes, and sends its relay probe
at 350 ms. Its remote receipt remains due. A valid peer GameData frame arrives
at the exact 1,100 ms absolute run cutoff before the timer callback. The
real-dispatch regression exits zero instead of the existing criteria-unmet
code. A 1,099 ms control succeeds as intended.

The opposite browser history also fails: a valid frame arrives at 1,099 ms
behind held PeerTransportStatus output. When output resumes at 1,101 ms, timer
processing fails the run before consuming that already-arrived receipt.
These histories distinguish frame arrival from later queued processing.
They do not reproduce the historical #853 or #889 incidents.

The native real-socket regression `481cc981` also fails. It enters the actual
run loop before expiry, advances the clock to the exact cutoff, and sends a
valid peer frame. All 64 retained baseline histories, 16 in each encoding,
credit the late receipt and return success. The run loop checks timers before
selecting input, but a frame can become ready before the due timer is handled.
This is application observation timing, not an inferred kernel arrival time.

The browser captures relay eligibility at raw input entry before frame sizing
or decoding. Its retained frame holds one admission marker until dispatch or
cancellation. A valid queued receipt postpones the cutoff decision until that
receipt is consumed; stale receipts release the marker without paying debt.
The native client carries the complete frame's observation time through JSON,
MessagePack, and opaque decoding. Only success-criterion credit depends on
eligibility. Delivery validation and normal receipt events remain in place.

Browser controls cover 72 histories: outgoing admission plus v2/v3 receipt
formats, exact/late boundaries, timely output/decode delays, and stale receipt
drain. Native `c5c96d50` passes 48 deterministic observation-time cases and
eight real-socket exact/late histories. V2 opaque table cases check the legacy
handler; they do not test negotiation. Native `f6e4a20d` passes 13 focused
regressions, including relay writes, queued exchange, retry, and backpressure.
Full browser tests, type checking, build, formatting, and strict native Clippy
pass. Independent adversarial review reports no remaining findings.

Final fixed-artifact Nextest `c6b16e65` passes three restores, three retries,
and three browser meshes after both send and receipt repairs. All 21 endpoint
streams retain 78 channel sends and 78 receipts, plus nine relay sends and
18 relay receipts. Server logs and matching before/after hashes for all four
executables/bundles remain under local `progress/session-470-receipt-qualified-live`
and `progress/session-470/receipt-qualified-before.sha256`. The earlier receipt
candidate `39f6f2f9` rebuilt its native executable during setup; its pre-build
hash is excluded. Original #853/#889 causes and the broader audit stay open.

### F90 — Queued room state can be skipped at successful exit

Issue [#1041](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1041)
continues A20 and A22 at `d47f236a`. Both reference clients can decide success
from old room state while a newer authoritative update is already available.
This investigation advances open #826, #853, and #889. It does not establish
the cause of either historical browser incident.

The browser arms an exit linger from an initial relay plan at monotonic
100 ms, then blocks on output at 200 ms. A PlayerJoined, fresh-epoch
PlayerReconnected, or replacement WebRTC SessionPlan arrives at 300 ms.
Output resumes at 400 ms, after the 350 ms linger expiry. The old source exits
zero without dispatching the arrived update. A separate channel-open history
shows that queued engine callbacks can also add skipped exchange debt.
The final baseline control fails both linger and run-cutoff histories, plus
controls that require consumption of 255 timely benign frames.

The browser now retains admission markers for raw server input and queued
callback tasks. Markers use the existing run, hold, and linger window. A
handler consumes its marker before applying state. Cancellation also releases
the marker. Terminal timer decisions wait for admitted input to drain.
Post-window input has no marker and cannot extend the run. The existing
backlog limits still apply. Gameplay receipt credit retains its own original
arrival check; processing late never renews eligibility.

The native regression uses a real WebSocket pair. A nonconsuming TCP peek
confirms complete frames before the run loop evaluates an expired run or
linger. The old loop skips those frames and exits zero. It drains queued
engine callbacks but never polls signaling before its terminal decision.
The repair uses one common server-frame dispatcher for ordinary input and
terminal drainage. It checks at most 256 ready frames and fails with a
connection error if input does not settle. This deliberately rejects even an
exactly full 256-frame frontier rather than claiming an unread tail is empty.
A shorter harmless prefix can still finish successfully. Receipt timestamps
remain complete-frame observation times; this does not infer kernel arrival.

Browser controls cover membership, reconnect, replacement plans, pair-open
callbacks, a full admitted backlog, and late input at both terminal boundaries.
Native controls cover empty input, harmless replies, reconnect, and plans,
with zero, 32, and 256 prefix frames at both boundaries. Exact and late relay
receipts keep their original windows. These are controlled histories, not a
claim that either historical incident has been reproduced.

The Rust, .NET, and Godot SDK assessments were refreshed at `da8f2d16`,
`e467c871`, and `2a74de36`. Rust provides no WebRTC backend. .NET owns signaling
and relay. Godot owns its engine mesh. None imports these private reference
process exit schedulers. No server protocol or SDK API changes, migration, or
client acceptance issue is required. The broader audit stays open.

Final native negative `7b927756` fails all eight authoritative zero/32-prefix
histories and all eight unsettled-frontier controls. Eight harmless histories
remain successful. Candidate `ac6cb9e5` passes all 24 cases and seven nearby
receipt and deferred-exchange regressions. All 12 browser histories, the full
browser package tests, type checking, formatting, and strict native/root
Clippy pass. Independent review found no remaining issues.

Fixed-artifact `bc2a4a55`, `6cc3edb1`, and `17e179ad` pass three restores, three
retries, three browser meshes, and one controlled fallback. All 24 endpoint
streams exit zero and retain 82 channel sends, 82 receipts, 12 relay sends,
and 24 relay receipts. Complete streams, server logs, and matching before/after
hashes for all four executable/bundle files remain under local
`progress/session-472-qualified-{2,3,4}` and `progress/session-472`.
The first candidate `e718fdbf` passes but setup rebuilds its native binary;
its pre-build manifest is excluded. Final comment formatting changes no
executable or test logic; independent review verifies this distinction.

After that formatting correction, rebuilt-artifact `e29fb93b` passes restore,
retry, mesh, and controlled fallback again. All ten endpoints exit zero.
The native binary rebuilds during this setup, so its pre-build manifest is
excluded. The final source hashes, endpoint streams, and server logs remain
under local `progress/session-472` and `progress/session-472-qualified-5`.

Final fixed-artifact `e609e1d4` passes restore, mesh, and retry on the formatted
source. All seven endpoints exit zero. The final-fixed-before and
final-fixed-after manifests match for all four files; retain these manifests
with `progress/session-472-qualified-6` as the final source qualification.

### F91 — Queued opens can form a pair after observed transport failure

Issue [#1046](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1046),
found at `bbd9f6ff` while investigating open #826, #853, and #889.
The browser holds output after an early probe on its first required channel.
Its second channel opens and queues application work. Either required channel
then emits an error while its `readyState` remains open. Releasing output lets
the old engine form a pair and send probes before it applies the queued error.
That creates receipt debt on a link already observed to have failed.
A separate control reproduces the same ordering after peer-connection failure.

The real-dispatch browser controls fail on the original engine at
`reliable/queued-error-first` and `reliable/queued-pc-failed`. Native Nextest
`d4ad5267` also fails after the production peer handler observes `Failed`.
Its channels are real and open; its application formation is held. The test
injects the connection-state callback. It does not reproduce network failure
or claim that the underlying connection actually changed state.

Both engines now record terminal state before publishing failure notifications.
Queued opens cannot form a terminal link. Browser callbacks also stop new
channel input, candidate output, channel access, and negotiation on that link.
Native channel close, error, receive rejection, and event-stream termination
mark the same formation barrier. Identity checks still let each orchestrator
consume its terminal notification and retire the correct physical link.
A transient `disconnected` state remains recoverable.

Browser controls cover both first-label orders, errors and closes on either
channel, failed and closed peer connections, held output, healthy input,
duplicate terminal callbacks, retired callbacks, and successful fresh-link
exchange. The native controls cover both offerer roles, failed and closed
callbacks, the disconnected control, and fresh-link formation after retired
callbacks. These are bounded callback histories. The original #853 and #889
incidents remain unexplained and their acceptance stays open.

Final native Nextest `c6044e92` passes ten focused controls, including the
terminal histories, channel shutdown, old-handler recovery, early probes,
replacement links, receive rejection, and signaling fairness. Browser package
tests, type checking, formatting, strict native/root Clippy, and documentation
checks pass. Manual adversarial review found no remaining issue in this scope.

Fixed-artifact Nextest `edf63708` passes three restores, three link retries,
and three three-player browser meshes. All 21 endpoints exit zero and retain
78 channel sends and 78 receipts, plus nine relay sends and 18 relay receipts.
Complete streams and server logs remain under local
`progress/session-476-qualified-live`. The before/after SHA-256 manifests match
for the server, native process, browser CLI, and page bundle. These passing
runs do not explain the original browser incidents.

The external SDK README assessments at blob `112c546c` (Rust), `467024de`
(.NET), and `d491bcd6` (Godot) show independent implementations. Rust provides
no WebRTC backend, .NET provides signaling and relay, and Godot owns its
engine mesh. None imports these private reference-client engines. No server
protocol, SDK API, default, or migration changes.

### A22 — Decoder-ready signaling control on Windows

Main WebRTC Interop run `39099313438` at `bbd9f6ff` fails only the Windows
native unit job, `117592132901`. The immediate-exchange control observes two
sends where it requires one. It still consumes Pong, relay data, and Close.
Issue [#1047](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1047)
records the failure and its full acceptance.

The old fixture peeks a cloned TCP socket before releasing its first send.
That proves kernel-buffered bytes on the peek handle. It does not prove that
the original WebSocket read future is ready. The production loop prioritizes
ready input and bounds input bursts to preserve send fairness. The old
fixture does not establish the readiness its assertion requires. The hosted
log alone does not identify the exact Windows completion schedule.

The control now completes its first real channel send, then places the same
Pong, relay, and Close frames in the real WebSocket decoder with
`from_partially_read`. All original debt, first-label credit, Pong, relay,
Close, and exact one-send assertions remain. The real-socket mesh-signaling
and bounded callback-fairness controls remain separate. No sleep, retry,
platform skip, timeout increase, or production arbitration change is added.

The repaired control, real-socket mesh-signaling control, and callback-fairness
control pass in native Nextest `c03304d9`. An intentional priority inversion
makes the repaired control fail in `5794279d`: it records two sends instead of
one. Production source is restored byte for byte after that negative control.
Hosted Windows remains the platform acceptance gate.

### F92 — Native deferred exchange can outlive observed channel loss

Issue [#1049](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1049),
found at `26850024` while continuing open #826, #853, and #889.
Both native required channels open, form a pair, and queue their gameplay probes.
One channel closes. Its producer records terminal state and publishes the close,
but application of that notification stays held. The other channel remains open.
The old send path sends and credits its queued probe on the failed physical pair.
The production loop permits deferred work after 16 ordinary inputs; ordinary
callback priority does not replace an admission check.

Real-channel Nextest `477d2e68` fails all four loss histories: either required
label closes, with either offerer role. Both healthy controls deliver the exact
probe to the remote endpoint. The test drives the production deferred-send path
before applying its actual close event. It controls callback application order;
it does not reproduce the original #853 or #889 network incidents.

The native send path now looks up a channel through the engine's terminal-state
barrier immediately before admission. Queue identity and cleanup retain their
stored-channel lookups. A refused send keeps its exchange debt and uses the
existing loss handler. A terminal event can still occur after admission; this
check does not promise cancellation of a send already in progress.

The controls also check Failed and Closed peer callbacks, recoverable
Disconnected state, stored handles for cleanup, retired callbacks, and usable
fresh-link channels. Peer-state controls inject the production handler callback
on real open channels; they do not claim a physical connection-state transition.
Existing partial-channel and replacement controls retain the fresh exchange and
old-generation receipt checks. Historical incident acceptance remains open.

Independent SDK README assessments confirm that Rust supplies no WebRTC backend,
.NET owns signaling and relay only, and Godot uses its own engine mesh. None
imports this private native reference-client send scheduler. Server wire formats,
SDK APIs, defaults, and migration requirements do not change.

Final scoped Nextest `4aa72484` passes seven tests, including actual send-call
exclusion after loss, healthy delivery, fairness, relay fallback, unpaid debt,
and fresh-link recovery. Strict root/native Clippy, formatting, Markdown, and
document consistency checks pass. Main-thread adversarial review found no
further issue within this admission boundary.

Fixed-artifact Nextest `70f8835c` passes three restores, three retries, and three
browser meshes in 25.276 seconds. All 21 endpoint logs exit zero and retain
78 channel sends, 78 receipts, nine relay sends, and 18 relay receipts. Complete
endpoint/server logs and matching before/after hashes for the server, native
process, browser CLI, and page bundle remain under local
`progress/session-477-qualified-live` and `progress/session-477`.
These passing runs do not explain the historical browser failures. Hosted CI,
platform qualification, and PR feedback remain publication gates.

### F93 — Browser required-channel closing does not retire the pair

Issue [#1051](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1051),
found at `5c85a2a7` while investigating open #826, #853, and #889.
The browser engine handles required-channel close and error, but does not
register the earlier closing event. A remote reset puts a registered required
channel into closing while its sibling remains open. The engine still exposes
channel handles and admits sibling messages until the later close event.
That delays relay fallback and allows queued work on an unusable physical pair.

The real Chromium negative control first exchanges both labels in both
directions. The remote endpoint then closes a required channel. During the
receiver's actual closing event, its readyState is closing, the pair-loss
callback has not run, and engine channel lookups still return handles.
The original engine reports `{state:closing,notified:false,usable:true}`.
The direct engine regression also admits post-closing input. A held-output
partial-channel regression admits a sibling message after observed closing.
These are new histories. They do not explain the original #853/#889 incidents.

The engine now registers closing with its existing close/error handler.
It marks the physical pair terminal before publishing loss. Closing, close,
and error share the existing single-notification and generation checks.
The controls cover either label, held output, unpaid exchange debt, stale
callbacks, and fresh-link recovery. Real Chromium covers remote reliable
closure with the engine as answerer and remote unreliable closure with the
engine as offerer, after healthy bidirectional exchange.

Native source inspection finds OnClosing declared and forwarded by the wrapper,
but no current core emission. This browser evidence does not establish a native
runtime defect, so no native behavior changes. The three external SDKs are
independent: Rust supplies no WebRTC backend, .NET owns signaling and relay,
and Godot uses its own engine mesh. No server wire, SDK API, default, or
migration changes are required.

The full browser package tests, type checking, formatting, real Chromium
controls, strict root Clippy, and documentation checks pass. Main-thread
adversarial review checks duplicate and retired callbacks, handler ordering,
input admission, and fresh-link recovery. No further finding remains in scope.

Fixed-artifact Nextest `775d2674` passes three room restores, three retries,
and three three-player browser meshes in 14.751 seconds. All 21 endpoint
streams exit zero and retain 78 channel sends and 78 receipts, nine relay
sends, and 18 relay receipts. Complete endpoint/server logs remain under local
`progress/session-478-qualified-live`; before/after hashes match for the
server, native process, browser CLI, and page bundle. These passing runs do
not establish the original historical failure causes. Hosted CI and feedback
remain publication gates.

### F94 — Native sibling messages can enter a terminal pair

Issue [#1053](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1053),
found at `e5b6d62c` while continuing open #826, #853, and #889.
The native required-channel producer checks receive rejection before publishing
messages, but does not check the shared physical pair's terminal flag. A required
channel closes while its sibling stays open. With application teardown held,
the sibling can still decode, reserve shared receive capacity, and publish
new gameplay callbacks after the engine observes loss.

The real-channel negative control closes the unreliable channel and observes
its actual close notification. It sends a reliable sibling message, waits for
SCTP acknowledgement, then closes that stream as a drain marker. The original
engine publishes the post-terminal message and fails the assertion. The first
control closed the sender immediately and passed; that result did not establish
message arrival and is excluded from defect evidence. Reliable acknowledgement
before reset and the wrapper's message-before-close queue order supply the
revised arrival and drain boundaries.

This proves engine admission after loss. Ordinary FIFO application handles a
close by removing the physical generation and ignores later callbacks. The
experiment does not establish visible application receipt credit or explain
the historical #853/#889 incidents. Those incidents remain open.

The repair checks shared terminal state before decoding or reserving incoming
gameplay messages. It keeps draining lifecycle events, retains callbacks admitted
before loss, and leaves generation checks and exchange debt unchanged. A failure
can still occur after admission; this is not cancellation of accepted input.

Current external SDK README assessments confirm separate implementations: Rust
has no WebRTC backend, .NET owns signaling and relay, and Godot owns its engine
mesh. None imports this private native reference-client event loop. No server
wire, SDK API, default, or migration requirement changes.

The data-driven regression covers both receiving roles, actual unreliable-channel
closure, and Failed/Closed production peer callbacks on real open channels.
The callback cases inject state observations; they do not claim physical PC
state transitions. Disconnected remains a positive control. Each case retains
an earlier message and its receive permit through loss, drains the later
reliable message through acknowledgement/reset, then verifies a fresh generation
admits traffic. Existing remote-close controls check bidirectional recovery on
both required labels. Independent adversarial review found no further scoped
issue in the guard, arrival/drain oracle, or evidence claims.

Final scoped native Nextest `9e771fdd` passes six controls in 13.305 seconds.
Root and native strict all-target lint checks, formatting, Markdown, and document
consistency pass. Fixed-artifact Nextest `6685f5cb` passes three restores, three
retries, and three browser meshes in 13.975 seconds. All 21 endpoint streams
exit zero and retain 78 channel sends and 78 receipts, nine relay sends, and
18 relay receipts. Server, native executable, browser CLI, and page hashes
match before and after. Complete streams remain in local
`progress/session-479-fixed-live`. An earlier passing run rebuilt the native
executable while enabling the interop feature; its differing hashes are retained
and excluded from this fixed-artifact claim. Hosted CI remains a publication gate.

### F95 — Browser live terminal state precedes failure callbacks

Issue [#1055](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1055),
found at `fd623b30` while continuing open #826, #853, and #889.
The browser engine relies on a cached terminal flag. A required data channel
can enter `closing` before its close callback runs. In that interval the
engine still returns the healthy sibling for gameplay sends and admits its
messages. Live failed or closed peer state has the same gap in controlled
engine histories.

The real Chromium negative control first exchanges both required labels in
both directions. It calls a retained channel's actual `close()` method and
checks its immediate `closing` state before a loss callback runs. Looking up
the sibling still returns a usable channel, so the new assertion fails on
original production source. This proves a real send-admission interval.
Controlled engine histories separately prove message and negotiation admission;
they inject peer-state observations and do not claim physical connection failure.
The first fixed-source Chromium harness incorrectly waited for a local `closing`
event. Chromium emits that event for remote closure; local close changes state
synchronously. The corrected local control captures the immediate observation
and waits for the actual close event to verify notification deduplication.
Both failed experiments remain in local session evidence.

The repair observes live failed/closed peer state and closing/closed retained
required channels at each engine admission boundary. It marks the link terminal
before publishing one loss notification. Subsequent callbacks cannot publish a
second terminal notification or admit work. Missing or connecting required
channels and transient peer disconnection remain valid setup states. The
orchestrator keeps already admitted input and can replace the physical link.
No server wire, API, configuration, or dependency changes.

Current external SDK README assessments confirm separate implementations:
Rust supplies no WebRTC backend, .NET owns signaling and relay only, and Godot
owns its engine mesh. None imports this private browser reference engine.
No external client migration or tracking issue is required for this repair.

Baseline real browser/native restore, retry, and three-player mesh controls
pass with retained endpoint streams. Passing controls do not diagnose the
historical incidents in #853 or #889. Those issues and the wider #826 campaign
remain open.

Fixed-artifact Nextest `812f5f7a` passes three restores, three retries, and three
browser meshes in 35.165 seconds. All 21 endpoint streams exit zero, with 78
channel sends and receipts, nine relay sends, and 18 relay receipts. Server,
native executable, browser CLI, and page hashes match before and after.
Complete streams remain in local `progress/session-480-fixed-live`. Browser
unit tests, type checks, real Chromium channel controls, formatting, strict
root lint, and document checks pass. Independent adversarial review finds no
further scoped defect. Hosted CI and feedback remain publication gates.

### F96 — Native live channel loss precedes poller observation

Issue [#1059](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1059),
found at `1cceb808` while investigating open #826, #853, and #889.
The native engine checks a cached terminal flag before gameplay admission.
A required channel can close while its lifecycle poller remains suspended.
The engine still returns the healthy sibling for sends during that interval.
The baseline real-channel regression fails at this send-admission assertion.
The RTC core does not emit its declared `OnClosing` event, so changing the
native event match alone cannot repair this history.

The repair observes both selected channels before send and queued-open admission.
Receive admission observes the sibling channel. Each channel preserves its own
message-before-close event order; the RTC core already refuses later input on
a closing channel. Missing and connecting channels remain valid setup states.
Closing, closed, or retired stack handles fence the physical link before a
loss notification. Each required label reports loss once, including when its
suspended poller resumes. Live observations cannot prevent a later concurrent
close. The checks add local channel-state reads; they do not wait for network
responses. Send checks share the original absolute exchange deadline.

The regression uses real native connections and explicitly suspends lifecycle
pollers. It covers both required labels and both offerer roles at each admission
point. Earlier admitted input retains its permit and payload. New input does
not consume receive capacity after loss. Resumed pollers drain without duplicate
loss, and replacement generations exchange gameplay again.
This controlled scheduling history does not identify the historical incident
causes in #853 or #889. The wider #826 audit also remains open.

Removing only the live-state receive check reproduces sibling message admission
in the same real-channel history. Restoring it passes all 12 histories.
Existing controls also pass for setup, observed peer loss, stream termination,
poller release, queued receipt cutoffs, and the active exchange-send window.
An adversarial control catches same-channel input loss in the first repair:
when no other seam has observed pair loss, reliable input acknowledged before
graceful reset must drain before its own queue's close notification. The final repair excludes the receiving
channel's later live state and keeps the sibling check. Both offerer roles cover
this ordering control. No initial repair was merged.
Strict root and native Clippy, formatting, and document checks pass.
External SDK README assessment confirms independent implementations: Rust has no
WebRTC backend, .NET owns signaling and relay, and Godot owns its engine mesh.
None uses this private native engine. No server wire, SDK API, configuration,
dependency, or client migration changes are required.

Final fixed-artifact Nextest `9032acfd` passes three restores, three retries,
and three three-player browser meshes in 10.605 seconds. All 21 endpoint streams exit zero,
with 78 channel sends and receipts, nine relay sends, and 18 relay receipts.
Server, native executable, browser CLI, and page hashes match before and after.
Earlier qualification attempts rebuilt the native CLI after its initial hash
or preceded the ordering correction; they do not qualify the final repair. Both attempts and all
endpoint streams remain in local `progress/session-481`. Hosted CI and review
remain publication gates.

### F97 — Timely browser keepalive replies can expire behind gameplay output

Issue [#1062](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1062),
found at `d8d35fb4` while investigating open #826, #853, and #889.
The browser clears outstanding Ping debt when a server-frame handler runs.
Two gameplay callbacks can hold that ordered chain on stdout acknowledgments.
A valid Pong already received by the socket then waits past the original
timeout and drain grace. The client exits with a false connection failure.
An old queued frame can also clear a Ping sent after that frame arrived.

The regression runs the real orchestrator with controlled socket, channel,
monotonic clock, and output acknowledgment schedules. Ping is sent at
10,100 ms. Gameplay output blocks at 11,000 ms. A second callback queues
ahead of a Pong received at 12,000 ms. Releasing the outputs at 20,100 ms
and 21,101 ms makes the original source fail with exit 3 before the Pong
handler runs. The baseline source is injected into the test bundle without
changing the working source. This is a controlled scheduling reproduction,
not evidence of the historical incident causes in #853 or #889.

The repair records server liveness immediately after successful synchronous
frame classification. Neither ordinary dispatch nor the Pong handler clears
debt again. Six controls cover timely Pong, timely ordinary server traffic,
old queued Pong versus a newer unanswered Ping, a missing response, malformed
input, and socket closure while gameplay output waits. The timeout and grace
constants stay unchanged. The native driver observes frames directly before
dispatch and has no equivalent browser handler queue; no speculative native
change is included. Independent source review checks the arrival/dispatch
ordering and stale-response case.

The #853 and #889 investigation also finds that failure text can omit early
pair roles and channel observations after they leave its 12-event tail.
The harness now includes full-captured-history counts for session plans,
new peers, connection states, required-label opens/closes/sends/receipts,
pair establishment, transport reports, and fallback. It keeps the chronological
tail and stderr. Counts include retries; they do not establish event order,
current state, or delivery. A behavioral regression fails with the old
tail-only formatter and passes with the repair. Full persisted event streams
remain the authoritative detailed record.

External SDK README inspection confirms separate implementations. Rust owns
signaling without a WebRTC backend, .NET owns signaling and relay, and Godot
owns its engine mesh. None imports this private browser orchestrator or test
harness. No server wire, SDK API, dependency, or migration changes are required.
These bounded findings leave A20, #826, #853, and #889 open.

Final fixed-artifact qualification passes three restores, three retries, and
three three-player browser meshes. All 21 endpoint streams complete, with
78 channel sends and receipts, nine relay sends, and 18 relay receipts.
Server, native executable, browser CLI, and page hashes match before and after.
Nextest runs `dfef9de8`, `afb5c35a`, and `e3dc6df3` and all endpoint streams
remain in local `progress/session-482`. Browser tests, typecheck, formatting,
strict root/native Clippy, and document checks pass. Hosted current-head CI
and feedback remain publication gates.

### F98 — Native peer retries consume the keepalive drain grace

Issue [#1067](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1067),
found at `0d25060d` while investigating open #853. A native timer pass arms
keepalive drain grace and then runs a due peer retry. A slow route operation
can consume that grace before the socket gets a read. The next timer pass
then rejects a complete Pong already buffered before the original timeout.
F97's native assessment excluded the browser's ordered output queue, but did
not exclude this distinct timer/retry history.

The regression sends a real Ping, seeds complete Pong bytes in the decoder,
advances to the response deadline, and holds the automatic retry's route stage
past drain grace. The baseline reports connection failure. The repair polls
ready socket input before rejecting expired drain grace. It stops after an application
frame or 256 transparent control frames. The response and grace durations stay
unchanged. Missing input, control-only input, malformed input, and socket close
remain failures. A consumed old response cannot erase the next Ping's debt.
This controlled history does not establish the historical #853 cause.

### F99 — Physical timeout discards an admitted browser gameplay receipt

Issue [#1066](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1066),
found at `0d25060d` while investigating open #889. A valid required-label
receipt arrives before its exchange cutoff while earlier output holds the
ordered handler queue. An outstanding ICE operation expires and retires the
physical link before the receipt handler runs. The old handler checks that
retired link and discards the timely evidence despite unchanged membership.
The baseline reports unmet exchange criteria.

The repair assigns pending receipts an owner for their logical peer obligation.
Physical transport loss preserves admitted evidence. Departure, fresh peer
incarnation, and authoritative link replacement invalidate it. Arrival still
requires the current usable physical link. The history covers both required
labels, finite and unbounded P2P waits, late-only callbacks, replacement plans,
and departure. These controls distinguish transport loss from membership loss
and do not prove the historical three-player incident's cause.

### F100 — Concurrent library transport reports publish stale final state

Issue [#1068](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1068),
found at `0d25060d` during the #826 A12 audit. Concurrent calls to public
`EnhancedGameServer::handle_client_message` store transport states in order
under the source lifecycle gate, then deliver outside it. Pausing the first
fan-out lets the second publish first. A peer ends with the old state while
the server stores the newer state. The normal WebSocket reader awaits each
call, so this reproduced trigger is concurrent library embedding.

The repair serializes transport reports per physical connection through queue
admission. It releases lifecycle and room gates before backpressure, so leave,
reconnect, and other messages can proceed. Delivery owns its reservation
futures directly; caller cancellation drops them instead of leaving a detached
old enqueue worker. The regression covers both transition directions, caller
cancellation, duplicate silence, and exact fan-out counts. The existing
leave/rejoin history checks that old membership still invalidates delivery.

The three repairs leave #826, #853, and #889 open. Their broader acceptance
conditions and the historical client incident causes remain unproved.

External SDK assessment reads the current README in each sibling repository.
Rust and .NET own signaling/relay, and Godot owns its engine mesh. None imports
the private native or browser reference orchestrator, so #1066 and #1067 require
no SDK migration. Tracking for the server's observable status-order repair is
[Rust #292](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/292),
[.NET #143](https://github.com/Ambiguous-Interactive/signal-fish-client-dotnet/issues/143),
and [Godot #367](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/367).
The status wire schema stays unchanged. SDK validation remains separate from
server CI and these tracking issues remain open.

Reproduce F98 with `cargo nextest run --manifest-path clients/native/Cargo.toml
--lib -E 'test(queued_liveness_survives) | test(keepalive_final_poll)'`.
Reproduce F99 with `cd clients/browser && npm test`; its `run-cutoff.test.ts`
scenarios include `queued-expired`, `queued-expired-late`, and `queued-replanned`.
Reproduce F100 with `cargo nextest run --lib
-E 'test(concurrent_transport_reports_publish_in_acceptance_order)'`.
Server baseline run `357c832d` fails on reversed status publication; repaired
run `40ac63f6` passes the new history and existing membership controls.
Browser baseline and repaired output plus the exact schedule remain in local
`progress/session-485/mesh`. Native final validation is recorded below.
Native run `1b6554cb` passes the retry, bounded-poll, unanswered-Ping, and
post-write deadline controls. Removing only the final poll makes the stronger
retry history fail with connection exit 3 in `c3ade767`; production is restored.
The nine-row frame matrix includes a ready application reply after 32 controls,
control-only input, the 256-frame cap, malformed input, and socket closure.

Final live qualification `fcfb03f5` passes the browser mesh, native-initiated
retry, and native room-restore cells. Server, native executable, browser CLI,
and page bundle hashes match before and after the three-cell run. Full endpoint
streams remain in local `progress/session-485/interop`. These are integration
checks; the controlled histories above establish the three specific defects.
Root and native strict lint, browser tests/typecheck/format, document checks,
and all 51 source-policy controls pass. Hosted CI and review remain merge gates.

### F101 — Native physical retirement discards admitted gameplay evidence

Issue [#1070](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1070),
found at `71093de2` while investigating #889. The native engine publishes a
required-label gameplay receipt with its arrival time and queue permit. A
negotiation timeout or physical loss removes the link before the orchestrator
handles that receipt. The old physical-generation check discards the event,
although the logical peer obligation and original receipt window still apply.

Baseline run `84a36c5f` rejects both timely channel labels after loss and timeout.
The repair retains a constant-size generation interval per logical peer so
admitted message events survive physical retirement and ordinary retry. Other
event kinds still require the current physical generation. Authoritative plan
replacement, departure, fresh incarnation, and room restore invalidate old
pending receipts. Arrival cutoffs and exchange payload validation stay intact.

Run `8c6b1271` passes the strengthened receipt matrix and a real WebRTC history.
The matrix interleaves another peer's generation before retry, rejects old
lifecycle events, and accepts fresh receipts after logical invalidation. The
real history receives both required probes from a live remote engine, retains
the published events and queue permits, expires negotiation, and then processes
those receipts after physical removal. These histories prove this failure mode;
they do not establish the historical #889 cause.

### A20 follow-up — Retain browser establishment state transitions

Open #853 records an establishment failure whose endpoint evidence cannot show
which browser transition occurred before the final failure. The reference
browser now retains the newest 32 observed signaling, ICE gathering, ICE
connection, and peer-connection state changes per physical link. Observations
use relative monotonic milliseconds. Duplicate observations add no entries;
old-link callbacks cannot alter a replacement's history. Timeout and loss
snapshots include this history before teardown.

The negative baseline lacks the history and fails the new control. The repaired
browser suite checks chronology, all four callbacks, the exact retention bound,
snapshot mutation isolation, replacement, and terminal-state visibility before
loss callbacks. History-only callbacks add no orchestrator tasks. The longest
realistic history serializes to 5,505 UTF-16 units, below the CLI diagnostic cap.
Browser events can combine state changes; this is an observation history, not a
proof of the historical failure cause. #853 remains open.

External SDK assessment reads the current sibling README files through the
GitHub connector. Rust provides signaling and relay without a WebRTC backend;
.NET also scopes itself to signaling and relay. Godot uses its own GDScript and
engine mesh. None imports the private reference engines or vendored RTC stack.
These reference-client changes require no SDK migration or wire change.

### F102 — Native local closing waits for reciprocal reset before fallback

Issue [#1071](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1071),
found at `71093de2` during #826. The RTC layer defines a closing event but does
not produce it on local close. The native poller also ignores that event.
A required channel can therefore remain logically connected while its stream
waits for the remote SCTP reset to complete.

Baseline `095403d1` connects real peer engines, pauses only the remote reactor,
closes a local required channel, and receives no pair-loss event while reset
is held. The repaired history `df302193` covers both initiator roles and both
required labels. It receives one loss event while the channel is still Closing,
fences sibling sends and input, resumes the remote reactor, and rejects any
duplicate loss notification. Existing peer-shutdown and ended-stream controls
also pass.

RTC now queues local closing behind previously published channel input. Local
reset completion follows the same read queue instead of overtaking it through
the separate event queue. The wrapper forwards final close through its existing
closure handler. Core control `cddcedfd` injects immediate reciprocal completion
and observes input, closing, then final close, with one reset after two close
calls. The wrapper retains FIFO for registered receivers. Its missing-receiver
cleanup distinguishes a first registration still in flight from a dropped
receiver after announcement. The latter can release queued input at closing
without waiting for reset. The former retains its original input history.

These controls establish the local established-channel failure mode. They do
not prove the historical #853 or #889 causes. The broader #826 audit and
published-package replacement qualification #912 remain open.

The same review found that closing before SCTP exists reaches Closed directly
and has no reciprocal reset to supply its final notification. Baseline
`c26c0e5a` fails that history. The repair publishes closing then closed, records
one lifetime closure and closed summary, and suppresses later teardown
notification. Run `669a548d` passes both in-band and explicitly negotiated
pre-SCTP cases plus the established reset-order control. The wrapper's 25 driver
controls pass after a causal negative check for announced receiver cleanup;
the original pre-registration retention control stays unchanged.

Final native run `6e325b8d` passes receipt, held-reset, and loss-snapshot controls.
Live qualification `01e07010` passes the three-player browser mesh, native retry,
and native room restore. Server, native executable, browser CLI, and page bundle
hashes match before and after all three cells. All seven endpoint streams remain
in local `progress/session-486/interop`. Initial run `63541ce3` also passed, but
Cargo relinked the native executable before the tests; that attempt is retained
separately and is not the fixed-artifact qualification. Root and native strict
lint, browser build/typecheck/format, and document checks pass. Hosted CI and
review remain publication gates.

Hosted CI exposed two older native controls that treated every retired physical
generation as an invalid receipt. One already held a real engine-published
message before loss. The revised histories preserve that timely physical-retry
receipt, reject exact/late arrivals and authoritative replacement, and retain
old-send fencing. Run `ae3c364e` passes both revised controls and both new receipt
controls. The cutoff matrix captures arrival before teardown. The real retry
matrix covers both roles, both labels, and timely, late, and logical-replacement
histories. No production change was needed. The root source-policy scan also
required an explicit empty/disconnected result when draining the new close
control; all six scanner controls pass in `f21a24a2`.

### F103 — Browser payload normalization can reject timely gameplay input

Issue [#1073](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1073),
found at `96becaa8` while investigating #889. The engine decodes binary channel
input and computes its UTF-8 size before the orchestrator checks the receipt
window. A probe entering the raw callback before cutoff can finish normalization
after cutoff and lose its receipt credit.

The red-first `run-cutoff.test.ts` history enters at 1099 ms, advances the clock
to 1101 ms during normalization, and fails against the 1100 ms cutoff. The
repair captures monotonic time at callback entry and passes it to the existing
strict receipt-window check. The matrix covers text and binary input, both
required labels, and arrivals before, exactly at, and after cutoff. Exact and
late input remain ineligible. Logical ownership, authoritative replacement,
terminal-link checks, and backlog accounting retain their existing behavior.
The focused matrix, full browser tests, and typecheck pass. Independent review
finds no blocking issue. These histories prove a deadline defect, not the
historical missing-inbound cause in #889.

### F104 — Native receive admission can replace gameplay arrival time

Issue [#1074](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1074),
found at `96becaa8` under #826 and #889. The channel poller records receipt time
only when it publishes an engine event. Shared admission checks can suspend
between the completed channel poll and publication. A timely probe can thus
receive a late timestamp and fail the existing receipt cutoff.

Baseline `e06bccfa` fails the real-pair admission history. The test holds the
actual receive-admission mutex, sends a required probe, and waits for an
explicit completed-poll notification. It advances monotonic time past cutoff,
then releases admission. The repair captures time immediately after the poll;
run `94ab80b5` passes both roles and both required labels. The clock is paused
only after real transport delivery reaches the held admission seam. Existing
client receipt controls cover exact/late cutoff and logical ownership; this
history pins the engine timestamp at its source. Independent review finds no
blocking issue. It does not establish socket arrival time before channel poll,
or identify the historical #889 cause.

### F105 — Remote stream reset can overtake accepted channel input

Issue [#1075](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1075),
found at `96becaa8` under #826 and #889. The RTC endpoint moves a remote reset
closure onto its lifecycle event queue while prior accepted input remains on
its read queue. The wrapper polls lifecycle events first. Its registered
channel consumer can therefore receive closure before earlier input.

The red core history feeds an accepted payload and an ordered SCTP stream-reset
completion through the channel handler, then pumps the peer pipeline. Polling
public lifecycle output before data reproduces the inversion. The repair routes
remote closure through the same read pipeline as input. Both required delivery
modes retain exact payload-before-close order. The existing stream-reuse history
now requires old input, old closure, and new-channel input in that order.
Nine expanded reset, reuse, opening, and statistics controls pass in `08f6949a`.
The repair also routes opening through the ordered data-read path. The wrapper
maps opening and closure to its existing callbacks. It stops a core drain at
closure and flushes the old registered receiver before consuming later lifetime
markers. If a late capacity recheck clears the pending close, the driver resumes
core reads before waiting again. Otherwise a replacement open could wait for an
unrelated socket or timer wake. The wrapper controls cover marker registration
and the actual read-drain boundary under a full receiver queue.

Published replacement qualification in #912 must retain this order; ordinary
exchange alone does not prove it. The async wrapper API retains its existing
opening and closure events; the private patched core uses ordered read markers.

A separate attempted live history pauses the receiving reactor, sends input,
and immediately closes the sender channel. It loses the input both before and
after this repair. Sender closing progress does not prove transmission, so that
history cannot qualify F105. Its source and both logs remain local under
`progress/session-487/`. Issue [#1076](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1076)
tracks the separate sender-boundary investigation.
These bounded findings do not identify the historical #853 or #889 causes.

External SDK assessment reads the current sibling README files through the
GitHub connector. Rust and .NET provide signaling and relay without these
WebRTC engines. Godot uses its own GDScript and Godot mesh. These fixes change
private reference-client arrival accounting and the vendored native stack.
They change no server messages, session ordering, or SDK behavior, so no SDK
migration is required.

Final qualification `e8a4e3c9` passes the real browser mesh, native retry, and
native room restore cells. Server, native executable, browser CLI, and page
bundle hashes match before and after all three cells. All seven endpoint streams
remain local under `progress/session-487/interop-final/`. These live checks
qualify ordinary interoperability; the controlled histories establish the
specific receipt and ordering defects above. Native final `f4f3601d` passes
eight controls for arrival time, admitted receipt retention, retry, local close,
ended streams, and terminal sibling input. Wrapper `36a4e2d8` passes all 40
peer-module controls. Removing only the wrapper lifetime drain boundary fails
the slow-receiver history; restored `ccc27377` passes. Root, native, RTC, and
wrapper strict lint, browser build/typecheck/format, and document checks pass.
Hosted current-head checks and feedback remain merge gates.

### A20 — First-open browser input and same-stream reset

Session 488 adds a discriminator for open
[#853](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/853)
and [#889](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/889).
A real Chromium endpoint sends on each channel's first open event. It does not
wait for the receiving reference engine to announce the channel or form the
pair. The engine records raw message events and admitted callbacks separately.
The matrix covers both offerer roles and both required labels. It then sends a
final probe and either closes immediately or waits for an application receipt
acknowledgement before closing that same stream.

All eight histories pass. In all four engine-answerer histories, both first
sends precede receiver channel registration. All 16 first-open probes and eight
final probes reach raw receipt and engine admission. Final admission precedes
pair loss in every history. The assertions require reliable receipt; they
allow absent no-retransmit input. Any observed raw receipt before pair loss
must cross admission exactly once. A raw callback after pair loss cannot pay
receipt debt. Both required handles become unusable after the reset.

Two temporary in-memory bundle faults qualify these controls. Dropping
first-open callbacks fails the reliable-admission assertion. Dropping only
unreliable first-open callbacks fails the final per-label admission assertion.
The real channel suite passes all 23 controls after restoring the bundle.
The browser engine source remains unchanged. Its SHA-256 is
`d291acde9b4f515a1c6a5219943af5d2a0a100b930e04273a30d297f28d374db`.
Raw capture and fault logs remain local under `progress/interop-diagnosis-*`.

This qualifies browser channel registration and input/reset admission on these
bounded Chromium-to-Chromium histories. It supplies a browser comparison for
[#1076](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1076).
It does not reproduce the historical browser/native failures, exclude a native
DCEP or transport failure, or establish unreliable delivery on a lossy path.
Issues #853 and #889 remain open. Replacement qualification in
[#912](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/912)
must retain startup and same-stream reset evidence separately from ordinary
mesh exchange.

### F106 — Local channel close overtakes accepted native sends

Issue [#1076](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1076),
found at `b2a9911b` under the unfinished #826/#889 investigation. A native
`send_text` accepts `final-before-reset`, then the application closes that
channel while only the remote reactor is paused. The receiver later closes
without the payload. Its received-message count remains zero.

The endpoint retains accepted writes until the driver runs. Local close
previously inserted a DCEP close directly into the lower data-channel queue.
The later endpoint drain appended the accepted payload behind that marker.
SCTP processed close first and skipped the payload on its non-writable stream.
The core red history records close before payload. The paired SCTP red history
retains 18 outstanding bytes and emits only RECONFIG (chunk type 130), with no
DATA packet.
This establishes a sender-boundary cause for this reproduction, beyond the
previous receiver-only zero-count observation. The red logs remain local in
`progress/session-488/send-close-{core,wire,native}-red.log`.

Local close now inserts its marker in the same endpoint FIFO as accepted
writes. Public state becomes Closing immediately, repeated close remains a
no-op, and further core sends fail. The lower reset follows accepted payloads.
The wire control now emits DATA then RECONFIG. Its reset last-TSN includes that
DATA, and the paired receiving SCTP stream reads the exact payload. Both
required delivery modes pass. The core matrix also retains in-band DCEP OPEN
before accepted payload and reset while its acknowledgement is pending.
Six final RTC controls pass in `42ead198`, including repeated/unconnected
close, late-ACK rejection, and stream reservation/reuse after reset completion.

The original live history is retained as an eight-case matrix: both native
offerer roles, both required channel modes, and pending versus acknowledged
sends. Pending sends retain 18 outstanding bytes while the receiver is paused;
acknowledged controls drain to zero before close. Each receiver reports the
exact payload before closure, with one message and 18 bytes in its closed
statistics. Send-after-close fails and duplicate close adds no second reset.
Each case also establishes a fresh link and delivers new input. Three final
native controls pass in `b0c4f44f`, including the matrix and existing local
Closing/remote-close histories. Independent reviewers found no remaining
production issue after the DCEP-pending control was added.

This repair changes the private vendored native stack. Rust and .NET SDKs
explicitly provide signaling/relay without this WebRTC backend. Godot uses
its own GDScript/Godot mesh. Current external README inspection confirms these
scopes. No server wire, SDK API, or migration changes are required.
[#912](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/912)
remains open: a published replacement must preserve this accepted-send/reset
boundary as well as the prior reset/reuse and lifecycle ordering controls.
These histories do not identify the historical #853/#889 causes or promise
no-retransmit delivery across packet loss.

Final interoperability run `48f7b492` passes the real browser mesh, native retry,
and native room-restore cells. All four executable/bundle hashes match before
and after the complete run. Seven endpoint event streams remain local under
`progress/session-488/interop-final/`. The first passing run `eba77aa8` used an
incorrect pre-build registration: enabling the browser-interop feature rebuilt
the native executable after its initial hash was recorded. That attempt is
retained but does not qualify a fixed-artifact cohort. The final run uses the
already-built feature set and passes the matching-hash check. This correction
is not evidence of a repaired transport failure.
The final hosted RTC filter also passes all eight reset controls in `7a790ccc`,
including both new accepted-send controls, queued input order, stream reopen,
and the parked-reader recovery path. The new names match the existing
`stream_reset_` hosted filter, so CI will execute their full bodies.

### F107 — Native channel allocation rejects the final valid stream

Issue [#1079](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1079),
found at `99e867d8` under #826 and #912. The RTC registry treated the negotiated
stream count minus one as an exclusive bound. A DTLS client with one stream
could not use stream zero. A DTLS server with two streams could not use stream
one. Both identifiers are valid: the transport reports a count, not a final ID.

The registry now compares against the exclusive count. A wider cursor can step
past the last valid 16-bit ID without wrapping. True red controls fail at the
small and full-range boundaries. Green controls cover counts zero through six,
both DTLS roles, occupied streams, repeated exhaustion, the full 65,535-stream
count, the unknown-count fallback, and final-stream reuse with a fresh handle.
The upstream count type cannot represent 65,536.

A paired SCTP history negotiates actual stream counts one through four and
passes the reported count into the production RTC allocation and send pipeline.
All eight cells deliver exact payloads on their highest role-valid stream:
ordered reliable and unordered zero-retransmit modes. Restoring the old bound
makes the count-one history fail before delivery. These are paired SCTP/RTC
histories; they do not model the complete ICE/DTLS low-count handshake.
The retained eight RTC reset controls and 36 SCTP reset controls also pass.

### F108 — Failed native channel creation keeps an inaccessible channel

Issue [#1080](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1080),
found while reviewing F107. The public creation method inserted its registry
entry before allocating and dialing its stream. A later error returned no
handle but retained that entry. Future allocation could assign a freed stream
to the failed, inaccessible channel before the requested replacement.

The red history fills valid even streams and requests another channel. Creation
returns the expected exhaustion error, but the registry grows from 32,768 to
32,769 channels. The repair removes the newly inserted entry on allocation or
dial failure. Its stream index is removed only if that handle still owns it.
Existing channels and the monotonic handle counter remain intact.

The green history repeats exhaustion three times without a retained pending
channel, frees the final valid stream, and successfully dials its replacement.
Both DTLS roles pass. A separate removal control covers unassigned entries,
assigned streams, repeat removal, and a reverse index owned by another handle.
The public-creation history uses the existing association-present test seam;
it is not a complete peer handshake. Explicit hosted selectors run allocation,
paired-wire, and creation rollback controls.

F107 and F108 change only the private reference-client RTC stack. Current Rust
and .NET SDK documentation excludes this WebRTC backend; Godot uses its own
GDScript/Godot mesh. No signaling wire or external SDK migration changes are
required. Published replacement qualification in
[#912](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/912)
must retain both repairs and the earlier reset, delivery, and statistics controls.
The source copies remain necessary. These findings do not identify the
historical #853/#889 failures or complete the broader #826 audit.

### A20 — Browser receipt admission diagnostics

Session 489 adds new failure evidence for open
[#853](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/853)
and [#889](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/889).
Snapshots now report each selected channel's stream ID and bounded counts for
raw callbacks, accepted callbacks, rejected input, and terminal discards.
Structured browser RTC errors retain their detail and SCTP cause code.
Weak channel keys retain no retired handle or message history. Replacement and
stale-callback controls keep evidence within the selected channel incarnation.

Loss callbacks see the triggering rejection or discard before teardown. A
snapshot called inside the message consumer can observe admission in progress:
acceptance is counted after that consumer returns. These counters establish
Engine callback admission, not SCTP packet statistics or application receipt.
The browser README defines that boundary.

Both labels pass size, type, backlog, prior-terminal, and live-terminal controls.
Five independent omissions of diagnostic capture fail their intended assertions.
All 24 real Chromium controls pass. A reliable-channel history admits small text
and exactly 65,536 binary bytes, then rejects a 65,537-byte message. Its loss
callback records three raw callbacks, two admissions, one rejection, and zero
discards before removing the peer. The other label retains zero callbacks.
Structured RTC error capture has controlled event coverage; the real Chromium
run produces no SCTP error.

The historical browser/native failures remain unexplained. These diagnostics
help distinguish a missing callback from rejected or discarded input without
changing exchange success, terminal admission, delivery modes, or deadlines.

The final fixed-artifact native/browser cohort `8d5ab819` passes mesh, native
retry, and native room-restore histories. The server, native executable, browser
CLI, and browser page hashes match before and after the run. Both endpoint event
streams remain local under `progress/session-489/interop/`. These passes qualify
the current artifacts; they do not explain the historical failures.

### F109 — Deferred channel failure prevents valid native channels from opening

Issue [#1081](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1081),
found while sweeping the F107/F108 creation paths. The SCTP connected handler
assigned every pending stream before dialing any channel. One excess pending
channel returned an allocation error and stopped that procedure. A valid channel
could retain its assigned stream without DCEP output or a handshake deadline.
The [WebRTC connected procedure](https://www.w3.org/TR/webrtc/#rtcsctptransport-connected-procedure)
requires failing the affected channel while continuing the others.

An initial real paired-SCTP probe negotiates a stream count of two. Two deferred
client channels require the same role's single available stream. The handler
returns an allocation error after assigning stream zero to the first channel.
That channel has no dial, deadline, or outgoing data. A later isolated control
records the same cause independently for both DTLS roles before asserting.
Restoring the old connected procedure makes that control and both complete
role histories fail. Production source hashes match before and after the
controlled temporary restoration.

The connected procedure now retires closed channels that have no lower
transport before assigning streams. It closes each unallocatable or invalid
fixed-ID channel independently and continues valid channel setup. Closed
summaries retain known configured IDs and lifetime totals. Existing closure
notifications are not repeated. Repeated connected events neither redial a
channel nor renew its original DCEP deadline.

The class sweep also reaches already-connected public creation. Reserved stream
ID 65,535 and fixed IDs outside a known negotiated count are rejected without
retaining a new channel. Before the count is known, ordinary fixed IDs can
still be created; the connected procedure checks them against the negotiated
count later. Negotiated fixed IDs have no in-band DTLS parity restriction.

The retained controls separate fixed-ID admission from clean overflow. Complete
paired histories cover both roles and both required modes, local closes before
connection, invalid fixed IDs, exact single closure and statistics, repeated
connected events, unchanged deadlines, DCEP completion, gameplay receipt,
reset, and fresh stream reuse. These exercise actual paired SCTP and production
RTC handlers. They do not model a complete low-stream-count ICE/DTLS handshake
or identify the historical browser/native failures. Published replacement work
in [#912](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/912)
must retain these creation-state and delivery controls.

All seven creation-state controls pass on the final source. The old public
fixed-ID guard path separately fails the reserved-ID control and each connected
role control. The old deferred procedure separately fails both paired role
histories and the isolated overflow control. The four registry controls, eight
RTC reset controls, public rollback control, paired boundary control, and closed
statistics control remain green. RTC production and native all-target strict
lint also pass. Closure statistics prove configured IDs and state/counts here;
they do not establish that an invalid channel ever occupied a wire stream.

The final native/browser cohort `7d878730` passes mesh, native retry, and native
room-restore histories after the complete F109 repair. The server, final native
executable, browser CLI, and browser page hashes match before and after all three
cells. Endpoint event streams remain local under
`progress/session-489/interop-delivery/`.

### F110 — Duplicate fixed IDs replace native gameplay channel ownership

Baseline `fe85dd81`. Finding [#1085](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1085)
continues A20 and replacement qualification #912. Public native RTC creation
inserted an occupied fixed SID into the registry. The reverse index then named
the new channel while the original handle remained live. Incoming data and
reset completion could reach the wrong channel.

The original code fails
`create_data_channel_failure_duplicate_fixed_sid_preserves_original_owner`:
a second fixed-SID creation succeeds before connection. The repair checks the
registry's authoritative ownership before insertion and returns
`ErrStreamAlreadyExist`. It preserves closing and closed reservations until
their actual release. A repeated failed call cannot replace the owner.

The control covers both roles, pre-connection and connected creation, all four
channel states, and fixed, automatically assigned, and remotely accepted
owners. Remote owners come from a real DCEP OPEN decode. It verifies retained
identity, label, state, registry size, and fresh-handle reuse after release.
These are core ownership controls, not a claim about the historical failures
in #853 or #889. Retained paired-wire exchange and reset controls remain
required before publication.

### F111 — Channels created after connection skip native startup outcomes

Baseline `fe85dd81`. Finding [#1086](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1086)
continues A20 and #912. Public creation after SCTP connection called the raw
channel dial operation. It skipped both the negotiated channel's open marker
and statistics, and the in-band channel's configured DCEP deadline.

Independent original-code controls fail for the missing open marker and missing
handshake deadline. The repair shares channel initialization with deferred
startup. It preserves disabled timeouts and the first deadline across repeated
connected events. Negotiated startup publishes one open marker and one opened
statistics record. The marker enters the existing ordered data queue.

Adversarial review found that a marker left in the private handler would still
be invisible to the wrapper's event-first data polling. Public creation now
pumps the read pipeline after successful initialization. The regression creates
two consecutive negotiated channels without new input or a new negotiation
notification, polls events first, and then reads the ordered public data queue.
It verifies exact open identity and no duplicate marker. Review also found
creation during an association handshake could dial too early. The repair
requires an established association. A real pending-handshake control checks
that neither channel type opens or starts its DCEP deadline before connection.

Wrapper review found two related boundaries. The wrapper now acquires the core
and event-sender map before creating a channel, then samples the runtime clock.
Creation and local receiver registration have no intervening await. This keeps
the driver from announcing a local channel as remote, prevents cancellation
from leaving an unregistered channel, and anchors its deadline after idle or
lock contention. Both locks release before the driver wake.

Independent old-code controls fail for cancellation while the sender map is
blocked and for a marker timestamp from a core clock 60 seconds behind the
runtime. The ownership control also routes one open marker and first gameplay
message through the production driver to the returned local receiver, with no
remote announcement or duplicate open. The clock control proves wrapper clock
refresh; the RTC controls separately prove configured timeout and ACK behavior.

Both roles, enabled and disabled deadlines, timeout closure, actual paired-wire
payload delivery, DCEP ACK, and stream reuse remain qualification controls.
This work does not qualify a replacement release or complete #912.

### F112 — Initial negotiation failure delays reference-client fallback

Baseline `fe85dd81`. Finding [#1087](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1087)
continues A20. The browser removes a failed initial link before its queued PC
callback runs. The orchestrator discards the old generation and only logs the
failed operation. The native orchestrator likewise waited for an expired
setup deadline or a formerly connected peer before reporting a removed link.
Both clients could wait the full P2P window to report relay fallback despite
an already known creation failure.

The browser regression fails before the repair with a frozen clock before the
P2P deadline. The native regression fails with unresolved status after an
invalid ICE configuration rejects the real peer builder. The repairs resolve
the current availability as soon as the initial operation fails. The native
path retains its no-live-link guard, so an ordinary malformed signal on a live
connection does not imply transport loss.

Browser controls cover failed and closed PC state, rejected offer creation,
and rejected local description, with and without a connected sibling. They
retain healthy signaling and relay receipts and complete a fresh tagged retry.
Native controls cover both roles, connected and connecting siblings, preserved
logical obligation, a future setup deadline, exact socket status frames, and
real retry recovery with gameplay on both required channel labels.
The original timeouts and exchange requirements remain unchanged.

Current Rust SDK documentation states that it has no WebRTC backend. The .NET
SDK supplies signaling and relay only. Godot uses its own
`WebRTCMultiplayerPeer` mesh. These changes affect only reference implementations
and their private RTC copy; they change no server message, SDK source, or SDK
migration requirement. The fresh SDK README assessment used the connected
GitHub app. Keep historical #853/#889 and the broad #826 audit open: these
controls do not identify the cause of those past failures.

### F113 — Native channel closure leaves send-capacity waiters blocked

Baseline `de067ca8`. Finding [#1088](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1088)
continues A20, [#826](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/826),
and replacement qualification [#912](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/912).
The async wrapper checked peer-connection closure and outstanding bytes but
ignored the individual channel state. A locally closed channel could retain
accepted bytes until its reciprocal reset. Its connection and sibling channel
remained live, so blocked sends could wait indefinitely. Nonblocking sends
reported buffer pressure before consulting the terminal channel state. With
an unlimited buffer, `writable()` returned success on a closing channel.

Real paired native controls configure the public send-buffer limit, pause the
remote reactor, and fill one channel. They explicitly poll `writable`, binary
send, and text send to pending before closing only that channel. Original code
returns `ErrSendBufferFull` from a terminal nonblocking send, times out waiting
for all three blocking operations, and returns success from unlimited
`writable()`. The repair checks terminal channel state before capacity and
notifies waiters after local close. Unbounded sends retain one core lock and
use the core's existing terminal send check. The original limits and accepted
payload remain unchanged. These controls prove terminal results after repolling;
they do not isolate notifier latency from the existing 50 ms liveness backstop.

The controls distinguish transport sibling continuity from reference-engine
pair policy: the wrapper keeps the sibling open, while the engine deliberately
stops gameplay admission after a required-channel loss. Outstanding accepted
input must reach the remote before its close marker, and its retained byte
count must return to zero. Fresh-link gameplay remains a separate control.
This finding does not identify the historical #853/#889 failure causes or
qualify a published replacement for the private WebRTC dependency.

### A20 — Fragmented browser gameplay through same-stream reset

Baseline `de067ca8`. This qualification continues
[#853](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/853)
and [#889](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/889).
Earlier first-open/reset controls used small messages. The expanded real
Chromium histories send three distinct 65,536-byte messages, as text or binary,
through both channel labels and offerer roles. Multibyte UTF-8 content reaches
the existing receive-size boundary and requires SCTP fragmentation. Immediate
reset and application-acknowledged reset remain separate histories.

The assertions compare the full body at raw receipt and engine admission,
check message type and channel identity, retain reliable ordering, reject
duplicate delivery, and require one pair-loss event. Immediate-reset histories allow unreliable loss. Acknowledged controls
require all receipts before reset. Any raw receipt before pair loss must cross
admission exactly once. Unexpected bodies, types, and cross-label traffic cannot count as allowed
loss. Separate in-memory faults remove large text/binary admission or corrupt
and misroute unreliable input. Each must fail its intended assertion.

The 40 real Chromium controls pass. All 48 large messages were observed at
both receipt seams in this cohort. These results qualify this fragmentation
boundary, not the historical native/browser establishment or three-player
mesh failure. The historical issues remain open, and this Chromium-only
comparison does not qualify the native SCTP implementation.

### F114 — Allocated native channels accept gameplay before DCEP acknowledgment

Baseline `76a372fb`. Finding [#1091](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1091)
continues A20, [#826](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/826),
and replacement qualification [#912](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/912).
The native RTC core checked for a missing SCTP stream but did not reject
`Connecting` channels with an allocated stream. Stream creation precedes the
remote DCEP ACK. A public send could return success and charge outstanding
bytes before the channel opened. The existing wrapper pre-open control covered
only the earlier state with no allocated stream.

The new regression dials a real data-channel object and retains its
`Connecting` state with an allocated stream. Baseline text send returns
`Ok(())`, which fails the expected typed `ErrDataChannelNotOpen` assertion.
The repair requires `Open` before both text and binary sends. The control covers
both required delivery modes, checks zero queued gameplay and zero charged
bytes on rejection, processes an actual DCEP ACK through the channel handler,
and verifies post-ACK sends and typed closure. The reset-order control now
processes that ACK before admitting its final gameplay payload. Its negotiated
channels still open without a DCEP ACK.

The focused RTC reset controls and existing native capacity/closure controls
pass. The rebuilt native/browser restore history also passes in run
`ac4eb69d-aa85-4eee-b4d5-76dfdedcb7b6` (3.357 seconds). Its initial invocation
omitted the required executable paths and failed before spawning a server;
that setup failure remains in the session log. Full hosted interop remains a
separate qualification gate. This finding
does not identify the historical #853 or #889 causes. It changes only the
private native reference-client stack. The Rust, .NET, and Godot external
clients do not use this patched dependency. The Rust manifest at `da8f2d16`
has no RTC dependency. The .NET project at `e467c871` has no package dependency.
Godot's mesh at `2a74de36` uses engine `WebRTCPeerConnection` objects. Server
messages, schemas, ordering, and capability negotiation are unchanged.

### F115 — Transport-driven native channel closure overtakes accepted gameplay

Baseline `dac8e4ae`. Finding [#1093](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1093)
continues A20, [#826](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/826),
and replacement qualification [#912](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/912).
Modern SCTP reset completion queues channel closure beside accepted input.
The legacy SCTP stream-close event and DTLS shutdown instead used a separate
lifecycle queue. An event-first consumer could receive `OnClose` before the
last gameplay payload already accepted into the data-read queue.

The new `stream_reset_transport_close_preserves_queued_gameplay_before_close`
control fails on the baseline with `close overtook input`. It opens a public
peer channel, accepts a payload through the data-channel handler, queues
transport closure, and drains public events before data. The repair routes
transport-driven channel-close announcements through the ordered read queue. Local
`Closing` still uses its distinct completion marker. Closure statistics and
duplicate suppression retain their existing behavior.

The claim concerns channel notifications and input already accepted by RTC.
It does not promise delivery after arbitrary transport loss or identify the
historical #853/#889 causes. The Rust external client at `da8f2d16` supplies
its own WebRTC backend. The .NET SDK at `e467c871` provides signaling and relay.
Godot at `2a74de36` uses engine `WebRTCPeerConnection` objects. None uses this
private reference-client dependency. Server protocol and capability negotiation
remain unchanged.

The green regression covers negotiated channels and actual in-band DCEP ACK,
ordered reliable and unordered zero-retry channels, legacy SCTP and DTLS
closure, one closed-statistics count, registry retirement, and duplicate
closure. All nine selected RTC reset controls and 113 statistics controls pass.
Independent review found no unresolved issue. DTLS closure enters through the
handler event seam; this control does not qualify wire `close_notify` parsing.
The rebuilt native/browser restore, retry, and three-player mesh histories pass
in Nextest run `c1bfffae-8fca-4e75-b7a5-beb0b66e57d3`. Hosted full interop remains
a separate qualification gate.

### F116 — Setup acknowledgments release unsent native gameplay credit

Baseline `26b80054`. Finding [#1095](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1095)
continues A20, [#826](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/826),
and native replacement qualification [#912](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/912).
RTC charges accepted application payload bytes at the public send boundary.
SCTP reports physical DATA payload bytes when acknowledged or abandoned.
DCEP setup and empty-message filler bytes enter only the latter count.
Subtracting that count from application debt can release queued gameplay
credit before the peer acknowledges any gameplay.

The new `transport_control_ack_preserves_pending_gameplay_accounting` control
fails on the baseline: a real paired SCTP acknowledgment for an earlier DCEP
message changes pending gameplay debt from 22 to 21. Gameplay remains in the
RTC write queue. The setup acknowledgment cannot pay that debt. A final negative control
uses valid marshaled DCEP OPEN and the unchanged final regression: restoring
physical-byte forwarding leaves 2 bytes charged instead of 22. Byte-exact
restoration of the repair passes the same test.

The repair retains the physical SCTP release count and adds a separate
application release count at the DATA chunk boundary. Only ordinary text and
binary PPIDs pay application debt. DCEP and empty-message PPIDs do not.
Cumulative acknowledgment, selective acknowledgment, and abandonment retain
the same stream-generation checks and physical-buffer release behavior.
RTC forwards the application count to its synchronous send counter.

This changes the private native reference-client dependency graph. The Rust
external client's current Cargo manifest has no native RTC dependency; games
supply the backend. The .NET SDK provides signaling and relay. The Godot client
uses engine `WebRTCMultiplayerPeer`. None uses this vendored graph. The server
wire and SDK contracts stay unchanged. The finding does not explain historical
establishment or mesh-receipt failures in #853 or #889.

The green RTC history covers valid DCEP OPEN and ACK, empty text and binary,
and ordinary binary release. Earlier setup acknowledgment leaves 22 queued
gameplay bytes charged; the gameplay acknowledgment later releases them all.
The SCTP control covers fragmented text and binary, selective acknowledgment
before an earlier DCEP message, duplicate acknowledgment, abandonment,
and an unsent fragmented tail. Its separate wire totals include empty fillers.
All 36 SCTP reset and nine RTC reset controls pass. Both native blocked-send
closure controls pass. Strict native and direct RTC/SCTP production lint pass.
Independent code review has no unresolved finding. The rebuilt native/browser
restore, retry, and three-player mesh histories pass in Nextest run
`af20d0f7-11f3-47a4-8778-bf1910a32e41`. Hosted full qualification is separate.

### F117 — Queued native probes bypass re-armed exchange gates

Baseline `b84508f7`. Finding [#1097](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1097)
continues A20 and [#826](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/826).
A returning room member re-arms the native CLI's global exchange release gates.
A queued probe to a retained sibling still has the same physical channel and
connected membership. Deferred dispatch checked those properties but ignored
the re-armed gates. It could send gameplay before the operator released the
new exchange phase.

Dispatch now rechecks the global exchange gate and the label-specific
unreliable gate. Held work leaves the sent ledger unchanged. Gate release
requeues current connected peers; the ledger prevents duplicate probes.
Channel-identity and connection checks still fence retired work. This repair
changes only the native reference CLI's file-release options. It does not
change the server wire, room lifecycle, or SDK contracts, and it does not
explain the historical #853 or #889 failures.

The Rust external client at `da8f2d16` supplies its own WebRTC backend. The .NET
SDK at `e467c871` provides signaling and relay. Godot's mesh source blob
`0d4fb588` uses engine WebRTC objects. None implements these reference CLI
release gates or uses the offline server capacity oracle. All three clients
are unaffected and need no migration.

The new `deferred_exchange_obeys_rearmed_incarnation_gates` regression dispatches
an actual returning `PlayerJoined` while a physical sibling channel has queued
work. It fails on the baseline with one send instead of zero. The repaired
history passes six cases: both required labels under the main gate, the
separate unreliable gate, and no gate. It verifies unsent debt while held,
exactly one send after release, and an actual remote channel receipt. The
independent source and regression review has no open finding. Hosted full
interop remains a separate qualification gate.

### F118 — Physical timeout discards admitted browser gameplay payloads

Baseline `47236332`. Finding [#1099](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1099)
continues A20 and [#826](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/826).
The browser engine admits an ordinary data-channel payload while output blocks
the orchestrator queue. The P2P deadline removes the physical link before the
handler runs. The old handler checks the removed generation and drops the
payload. Only recognized exchange probes held a logical receipt owner through
physical timeout. This loses ordinary gameplay input already accepted from the
current membership.

Every admitted payload now captures the logical receipt owner. Physical timeout
and ordinary retry preserve this owner. Peer departure, a new incarnation, and
authoritative replacement invalidate it. The engine still rejects callbacks
from retired physical generations before queue admission. Queue count and byte
bounds, arrival deadlines, and exact exchange criteria remain in force.

The controlled run-cutoff history fails on the baseline at
`queued-expired-reliable` with the new ordinary-payload assertion. This is a
new ordinary-input gap, distinct from the prior exchange-probe repair in F99.
It does not explain the historical establishment or mesh-receipt incidents in
[#853](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/853)
or [#889](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/889).

This repair changes the private browser reference orchestrator. The external
Rust SDK uses an application-supplied WebRTC backend; .NET supplies signaling and relay;
Godot uses engine WebRTC mesh objects. None imports this orchestrator or the
capacity oracle. All three clients are unaffected and need no migration.

The final controlled history passes both required labels with exchange enabled
and disabled. It uses text on the reliable channel and binary input on the
unreliable channel. It requires exactly one pre-timeout ordinary payload and
no payload from a post-timeout callback. Departure and authoritative generation
replacement discard queued input. Browser tests, strict type checks, formatting,
and independent adversarial review pass. Rebuilt real Chromium/native restore,
pair retry, and three-player browser mesh controls pass in Nextest runs
`0b4a40b2` and `3726e41e`. These bounded controls do not reproduce either
historical incident. Hosted interop remains a separate qualification gate.

### A20 — Native receipt admission diagnostics

Session 497 advances the open establishment and mesh-receipt investigations
[#853](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/853)
and [#889](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/889).
The native physical-link snapshot previously exposed only channel registration
and handled-open state. It could not distinguish a complete driver receipt from
queue publication, input refusal, or terminal-link discard. Each required label
now records those four observations. Existing timeout and pre-removal reports
include the counts. This is a diagnostic addition, not a confirmed gameplay
repair or an explanation of either historical incident.

Raw counts start when `poll()` returns a complete message, before admission
waits and decoding. Admitted counts require successful engine-event queue
publication. Rejected counts mark decoding or capacity refusal. Discarded counts
mark observed link loss or a dropped client event receiver. Counters belong to
a physical generation and retain no payload history. Their separate atomic
reads are not a transactional ledger: raw input can still await admission or
lose its poller during cancellation. Queue publication does not prove
orchestrator consumption, stdout delivery, or remote gameplay receipt.

At baseline `2e1a01e7`, assertions for the new diagnostic contract fail in
Nextest run `89f170a2-cd90-45f3-8599-79e06133069b` because receipt fields are absent.
The controlled histories use real paired channels, both local roles, terminal
and transient observations, receive-count and decoded-byte refusal, dropped
client receivers, and fresh physical generations. Assertions use completed
rejection or close barriers rather than a sleep or a possibly unfinished
message publication. Final Nextest run
`9c8b6ccc-bc1f-409d-8d01-68cdda47b5fa` passes all four focused tests.
Strict native all-target, all-feature clippy and formatting also pass.
Rebuilt browser/native restore and three-player browser mesh histories pass
in Nextest run `1e4c01eb-1e89-4b79-9cb3-6ddebe37c896`. These histories do not
reproduce or explain either historical incident.
Independent adversarial review reports no open finding.

This change affects only the private native reference engine. The Rust SDK
uses an application-supplied WebRTC driver; .NET supplies signaling and relay;
Godot uses its own engine WebRTC objects. Their sources do not import this
engine. The external clients need no migration. Broader A20 coverage and
[#826](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/826)
remain open.

### F119 — Capacity send ledgers can bypass the declared workload

Session 498 confirms [#1102](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1102)
at `62e8ded631ab2d69dabb0a06e351e1721bd48ef6`, under open audit #826. The capacity oracle validates deliveries
against recorded sends, but it does not validate those sends against the declared
schedule. It subtracts raw send-row count from planned count to find unsent work.
A changed room, phase, intended time, sequence, or duplicate replacing another
sequence can therefore produce a valid verdict for a different workload.
Changing a phase can also exclude measured latency samples.

Nextest `9aacbe24-3d28-4d92-861f-69b610559de0` fails the new negative control:
all five corrupt histories remain valid. Delivery ledger keys are changed with
send keys where required, so this is distinct from a receipt-pairing failure.
A later adversarial audit confirms an additional false-valid boundary. All
sends can name epoch zero or an unannounced epoch. The completeness loop has
no adopted stream for those sends. Nextest
`c563ae24-b638-4fa8-93a5-cdff16a576e4` accepts all six histories for epochs
0, 2, and `u32::MAX`, with either complete receipts or no receipts at all.
No server delivery defect or historical generator-stall cause is established.

The repair compares every recorded send with the deterministic sender schedule.
It rejects unknown senders, unplanned sequence keys, duplicate keys across
incarnations, and room, phase, or intended-time differences. A send must use
initial epoch 1 or a nonzero later epoch recorded by that sender's rejoin.
The unsent count now uses distinct matching sends. Extra and malformed rows
remain in raw send totals, byte evidence, and latency evidence. They cannot
replace missing planned work. Canonical send blocks are borrowed; arbitrary
row order uses the existing stable pointer index. Validation stores only peer
and rejoin lookups plus the current key, rather than a key per send.

Nextest `88de3a99-8679-4049-a876-f502b014d5f8` passes exact reason and unsent-count
controls, raw artifact round trips, reversed send order, both invalid-epoch
receipt histories, and valid reconnect, room-replacement, and snapshot-tail
controls. The valid warm-up fixtures now build real warm-up schedules instead
of changing the phase of measured sends. A real socket history and its public
artifact replay reject changed room and phase metadata. Earlier live replay
and reconnect controls pass in `ccb8f922-f115-402a-aec1-8d51e6921091`.
Final incarnation-aware all-feature live replay and reconnect controls pass in
`1746b5be-442a-49ac-b840-158c0b88e13c`. All ten focused all-feature controls
pass in `288d018d-d682-4a8d-863f-e55788ae2296`. Strict all-target, all-feature
clippy, formatting, and the panic, timeout, and clock source scans pass.

The change affects the standalone capacity runner and its verdicts. It changes
no server wire message, library API, client runtime, or dependency. Rust, .NET,
and Godot clients do not consume this test-target oracle and need no migration.
The broader audit, long-workload memory proof, and historical incidents remain
open. This finding does not certify arbitrary churn-event histories.

### F120 — Capacity rejoin snapshots can name the wrong stream

Session 499 confirms [#1104](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1104)
at `5a7db157`, under open audit #826. The snapshot resolver reads the registry
incarnation without checking its player name. It also ignores foreign snapshot
members and accepts tails beyond the recorded send ledger. Five independently
malformed snapshots still receive a valid verdict in Nextest
`42640965-230e-49a2-9b07-ec951c11d4a0`.

The repair validates every recorded rejoin tail against the roster and registry.
The member must belong to the recipient's room and match the registered name.
Its epoch must be nonzero and any later incarnation must have a recorded rejoin.
Its tail cannot exceed that incarnation's send ledger or reach a send scheduled
after the snapshot. The validator uses intended send time because a snapshot
can include data before the sender records its completed socket write. A
snapshot may also precede another task's recorded rejoin time. Both valid
inversions remain covered. Missing snapshot members remain legal under the
existing concurrent-join race.

Eight malformed histories now assert their exact invalidation reason. Reversed
send/churn order and raw artifact round trips preserve the same summary. The
room-replacement fixture now keys tails by member name, matching the real
runner; its former player-ID key silently bypassed snapshot-floor validation.
Focused reconnect, replacement, and snapshot controls pass in Nextest
`f999822c-e483-42b9-bac0-67ed179302bc`. Independent adversarial review has no
open finding. Real-socket reconnect and two-wave room replacement, including public artifact
replay of a changed snapshot identity, pass in all-feature Nextest
`ce169648-2153-4ddd-a08c-a1cf7566c503`. Final hosted checks are recorded
in the session PR.

This finding affects the standalone capacity oracle. It establishes no server
delivery defect and changes no server wire message or client API. Rust, .NET,
and Godot clients do not consume this test-target oracle and need no migration.
Arbitrary churn-event validation and broader A01–A22 coverage remain open.

### F121 — Capacity churn counts can replace actual disconnect evidence

Session 500 confirms [#1106](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1106)
at `870c9c28`, under open audit #826. The oracle counts recorded rejoins against
the planned storm. It does not check the lifecycle actions themselves. Removing
every recorded disconnect while retaining rejoins, sends, and receipts still
produces a valid verdict. Red Nextest `2dbc0ef0-cfeb-4746-9f7b-2dc1b23a4240`
reproduces this missing evidence. No server delivery defect is established.

The repair validates each action against its peer's declared reconnect cycles.
It checks alternating disconnect/rejoin phases, scheduled lower time bounds,
empty disconnect metadata, contiguous rejoin epochs, and registered incarnation
identity. Unknown peers and unplanned actions are invalid. Existing missing-work
checks remain. The raw action records and all delivery evidence remain intact.
Delayed actions and cross-peer logging inversions stay legal. Same-microsecond
transitions use disconnect ordinals and rejoin epochs to reconstruct legal
cycle order without treating another task's log order as causality.

The new negative controls exercise missing and duplicate actions, unknown and
unplanned peers, early transitions, malformed disconnect fields, invalid epochs,
and missing registry identity. Controls check typed invalidation reasons.
Malformed recorded-action cases also check artifact round-trip verdicts and
reordered histories. Positive controls retain delayed,
tied, repeated cycles and the earlier snapshot timing inversions. Independent
review found the equal-timestamp edge before publication and requested its
explicit positive control. All 11 final focused all-feature controls pass in
Nextest `ade5eb58-96ff-4e90-8be7-9bdcdb08f7e2`, including real reconnect public
replay of removed disconnects and missing rejoin epochs, and two-wave room
replacement. Full strict all-target, all-feature lint passes.

This changes only the private capacity runner verdict. Rust, .NET, and Godot
clients consume no oracle output and need no migration. Server wire messages,
client APIs, dependencies, offered workload, and timing limits stay the same.
The broader audit and full long-workload proof remain open.

### F122 — Delayed transport status can reach a v2 replacement

Session 501 confirms [#1108](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1108)
at `52a58471`, under open audit #826. `TransportStatusFanOut::deliver` checks
recipient protocol before conditional routing. A recipient can disconnect and
reconnect as v2 between that check and delivery-handle lookup. The condition
checks only the source membership, so the v3-only `PeerTransportStatus` frame
can reach the v2 replacement. The real disconnect/reconnect history in
`transport_status_rechecks_recipient_protocol_after_reconnect` fails before
repair in Nextest `b87d8fb7`.

The repair also checks the current recipient protocol in the conditional send
predicate at queue admission. It keeps the existing source membership check
and does not hold the room mutation gate during slow delivery. The same real
history rejects the frame for a v2 replacement and delivers it for a v3
replacement. Both existing paused-delivery and concurrent-order controls pass
with the regression in Nextest `5f28291f-bf46-4924-85ab-d75e2ecd9ff9`.
Independent review found no correctness issues.

The external clients need no migration. Rust `da8f2d16` in `docs/protocol.md`,
.NET `e467c871` in `docs/polling-client.md`, and Godot `2a74de36` in
`.llm/research/protocol-fixtures.md` already classify this event as v3-only.
The schema, v3 behavior, defaults, and client APIs stay the same. This repairs
one recipient-version race; it does not complete the broader correctness audit.

### A20 — Native fragmented gameplay through same-stream reset

The [baseline](https://github.com/Ambiguous-Interactive/signal-fish-server/commit/f206b635caa11f09926557ae1aa56515ca2442a6)
leaves native fragmentation unqualified by the earlier Chromium-only comparison.
This investigation advances [#853](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/853),
[#889](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/889),
and native replacement qualification [#912](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/912).
The new `fragmented_gameplay_preserves_receipts_through_same_stream_reset`
control uses real native ICE, DTLS, and SCTP peers over local UDP.

Sixteen histories cover both sender roles, required channel labels, text and
binary send entrypoints, and immediate versus application-receipt-confirmed
reset. Each sends three distinct 65,536-byte UTF-8 bodies. The immediate
histories pause the receiver reactor before sending and retain all 196,608
outstanding bytes until close starts. The control histories consume every
application receipt and wait for zero outstanding bytes before reset.

Assertions check the full bodies, peer, physical generation, label, duplicates,
reliable order, same-stream close order, complete driver receipts, engine
admission, closed transport statistics, released queue permits, and fresh-link
recovery. Immediate-reset unreliable loss remains allowed. Receipt-confirmed
controls require all messages on either label. The stopped producer must leave
no late message, rejection, or duplicate close in the event queue.

All 16 histories pass with 48 exact large receipts and 3,145,728 received
bytes. The existing eight small-message paused-reset histories also pass.
Three temporary faults corrupt large text, omit large binary admission, and
misroute unreliable input. Each fails the intended body, required-receipt, or
label assertion. Restoring the source restores the passing controls.

These bounded histories do not identify either historical failure cause.
They do not cover packet loss injection, browser/native fragmented exchange,
first-open sends, or a published dependency replacement. The broader A20 audit
and all linked campaigns remain open. This internal qualification changes no
server behavior, protocol schema, or external Rust, .NET, and Godot client
contract. Those clients need no migration.

### A20 — Fragmented first-open Chromium/native delivery

The [baseline](https://github.com/Ambiguous-Interactive/signal-fish-server/commit/19e3d99b4c3a99d1e76f525d43add656aeebc516)
leaves cross-runtime fragmented sends at first channel open unqualified.
This control advances [#826](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/826),
[#853](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/853),
and [#889](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/889).

`fragmented_first_open_crosses_chromium_native_in_both_roles` uses the real
native and browser engines with Chromium 156.0.8078.4 and Playwright 1.64.1.
Four independent histories cover both offerer roles and text versus binary.
Each endpoint sends one message on each required label at that label's first
open callback, before pair bookkeeping. Each distinct body contains exactly
65,536 UTF-8 bytes with multibyte characters. Both encodings are the actual
first send on their fresh channels. The histories require 16 complete
application receipts, totaling 1,048,576 bytes.

Assertions check full content, byte counts, native peer and physical generation,
exact labels, duplicate receipts and opens, pair completion,
and per-label raw/admitted/rejected/discarded counters at both engines.
Failures retain open and receipt progress plus both engine snapshots. Browser
callback errors fail the control. The helper closes Chromium before exit.

The revised control passes all four histories in 7.03 seconds. Temporary
same-size corruption fails the full-body assertion in 1.55 seconds. Omitting
a required reliable first text fails the existing 30-second deadline in
30.30 seconds. Final validation is recorded in the session PR. Raw commands,
outputs, and source hashes remain in local `progress/session-505/`.

After Tokio 1.54.1 replaces the withdrawn 1.54.0, the control passes again.
Both selective faults still fail their intended assertions, and the helper
is restored byte for byte. This does not establish a cause for either
historical failure.

An earlier two-send burst delivered seven of eight messages in one history.
The native-to-browser second unreliable binary message was absent at the
browser's raw callback. Browser counts were reliable 2 and unreliable 1,
with no admission rejection. Native counts were 2 on each label. Both links
remained open. This is compatible with allowed unreliable loss; it does not
establish the loss mechanism or explain either historical failure. The
revised test qualifies first sends, not that burst. The failed attempt is
retained as `first-open-unreliable-burst-absence.log`.

The test bridges SDP and ICE directly through its helper pipe. It does not
exercise signaling-server membership or repair either historical failure.
It injects no packet loss and does not make unreliable delivery a protocol
guarantee. The campaigns remain open for their full scope. The native
reference library now exposes its existing local diagnostic snapshot for
library callers. The server protocol and configuration stay the same.
External Rust, .NET, and Godot clients need no migration.

### A20 — First-open burst loss mechanism

The two-message burst above now has transport evidence. Each endpoint sends
one 65,536-byte text message and one 65,536-byte binary message per label
at the first open callback. Both offerer roles use the real engines.
The original single-message first-open qualification stays unchanged.

A failed native-offerer run reproduces the missing second browser receipt.
Chromium reports two reliable messages and one unreliable message received.
Each sender reports two sends per label. Both channels remain open.
A separate traced browser-offerer failure shows the native stack sending all
binary fragments. Chromium acknowledges later TSNs while its cumulative ACK
stops at TSN 2143070677. The native stack sends FORWARD-TSN through 2143070710.
The stack retries reliable fragments. It abandons the missing unreliable
message under the channel's zero-retry policy.

A per-socket Linux monitor confirms remote receive loss in another burst run.
The Chromium UDP socket drop count rises from 0 to 39 in one role, and from
0 to 31 in the other role. The monitored native sockets retain zero drops.
This proves local UDP receive overflow in that reproduction. It does not
prove the cause of the historical #853 or #889 failures, or every burst loss.
Local raw traces, socket owners, commands, and source copies remain in
`progress/session-506/`.

The ignored `fragmented_first_open_burst_transport_diagnostic` retains the
burst as a manual probe. Run it with:

```sh
RUST_LOG=rtc_sctp=trace cargo test --manifest-path clients/native/Cargo.toml \
  --features browser-interop --test browser_interop_e2e \
  fragmented_first_open_burst_transport_diagnostic -- --ignored --nocapture
```

The probe requires both reliable receipts in both directions. It checks exact
content and rejects duplicate or corrupt receipts on either label. It prints
browser transport statistics and both engine receipt counters at the reliable
completion point. Unreliable receipts can still be in flight at that point;
absence in that snapshot alone does not prove loss. The 30-second deadline
bounds each history. Neither the probe nor an open channel guarantees delivery
on the unreliable label. Use reliable messages for data that must arrive.
The campaigns remain open. Server protocol and external-client contracts stay
the same; Rust, .NET, and Godot clients need no migration.

### F123 — Embedded gameplay crosses reconnect into a fresh epoch

Issue [#1117](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1117),
found at `849321ff` under open #826 (A13/A04). Concurrent library use exposes a
gameplay identity defect. `handle_game_data`, `handle_game_data_binary`, and
`handle_client_message(GameData)` supply no socket lifecycle to relay admission.
The shared handlers interpret that absence as an ungated call. An old frame
can pause at a sender or room budget wait while unregister and reconnect finish.
When the frame resumes, stamp lookup uses the restored connection. Peers receive
old gameplay labeled with the replacement's epoch and sequence.

The existing admission history now exercises all three public entrypoints and
both socket entrypoints at both budget waits. It records the original relay
epoch before admission, waits at the existing admission seam, and runs a real
unregister and token-based reconnect. It releases the old frame, inspects its
actual peer payload and epoch, then requires a fresh frame from the replacement
to use its new epoch with sequence one. It also checks charged bytes and that
neither call produces a refusal. No new production pause hook is added.
Baseline Nextest `b251cacd-1b80-4a12-bcbd-79f1299f910c` fails because the old
public JSON frame has epoch two instead of epoch one. Earlier gate-only control
`b6257619-1f96-4aff-b2c0-169afff8eb59` independently shows reconnect completing
while that frame remains paused.

Both shared handlers now capture the current lifecycle when the caller did not
supply one. The existing lifecycle gate then covers budget admission through
stamp and enqueue, as it does for socket calls. It releases before delivery
backpressure. Explicit socket sources retain their existing identity check.
Calls with no registered lifecycle keep their existing compatibility behavior.
The same gate serializes voluntary leave with admission; the reproduced history
covers reconnect, not every overlapping leave/rejoin schedule or socket write.
The wire format and client API are unchanged. The release note describes the
observable embedded-server repair. Rust, .NET, and Godot clients consume the
network protocol and do not call these server library entrypoints. They require
no migration or tracking issue. The session PR records the source assessment.
This finding does not complete the broader #826 audit.

Reproduce with `cargo nextest run --lib
-E 'test(old_relay_budget_admission_serializes_restored_player_reconnect)'`.

Repaired Nextest `1e7900c8-c619-43ca-8c53-aaf297f151cd` passes the ten-history
matrix and all 15 neighboring gameplay handler and budget tests. Review adds
an exact one-receipt check and requires the old frame's sequence to be one.

Independent review finds an earlier ownership gap in public
`handle_client_message`: room liveness can suspend before relay dispatch.
The existing pre-dispatch pause now exercises public and explicit socket calls.
It restores the connection before releasing the old call. Baseline Nextest
`df2e9189-380f-4fe6-90c3-d4c182a4bec1` delivers the old public-router payload
with epoch two and sequence one. The router now resolves a missing source at
entry, before liveness waits. The existing stale-source check in the shared
relay handler then drops the old frame after reconnect. The regression also
requires fresh JSON and binary frames to begin at sequences one and two.
Final Nextest `a874b100-9a20-4bd9-bf8f-a972a6081dca` passes both gameplay
histories and all 15 neighboring handler tests. Independent review confirms
the router capture occurs before its first await and adds no recursive gate
acquisition. The original socket checks remain active.

### F124 — Gameplay queued behind leave charges the old room

Issue [#1119](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1119),
found under open #826 (A07/A13). JSON and binary gameplay resolved the sender's
room before waiting for its lifecycle gate. An ordinary `LeaveRoom` can win that
gate without replacing the connection. The waiting frame then charges the sender
and old room budgets, but stamp lookup drops it. The sender receives no
`NOT_IN_ROOM` refusal.

The regression holds the existing lifecycle gate, queues a real leave first,
and polls gameplay into the same wait. It releases the gate, awaits both calls,
and requires `RoomLeft`, zero relay accounting, and `NOT_IN_ROOM`. It also
reserves each full byte budget to prove that the stale frame spent neither
budget. Paused time prevents a window reset from masking a charge. The matrix
covers the public JSON and binary entrypoints. It adds no production pause hook.
Baseline Nextest `0f3c1d22-dfd5-46c8-816c-87c0a4a552b1` fails the JSON history:
relay accounting is nine bytes instead of zero. The repair resolves membership
under the existing source gate in both lanes. Refusal delivery releases the
gate before calling the helper that takes it again.

Run `cargo nextest run --lib -E 'test(game_data_tests)'`. Initial repaired run
`b0bcc2ad-cdf7-4d4e-95ba-64cb66bfcad8` passes both queued-leave histories and
all 15 neighboring handler tests. This is a bounded library concurrency
history. It does not certify every leave/rejoin schedule or socket-write
outcome. The wire format and public API stay the same. Rust, .NET, and Godot
network clients require no migration or tracking issue because the change
corrects server admission ordering. The broader #826 campaign remains open.

Final Nextest `878cc7a0-1f5b-40f1-ba8c-15d6134a649f` passes all 16 tests,
including the full sender and room budget reservations. Independent source
review finds no remaining correctness issue in the changed gate scope.

### F125 — A peer reset overtakes accepted native gameplay writes

Issue [#1121](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1121),
under open #826 (A20). A native send accepted its payload into the endpoint
queue and charged outstanding bytes. An incoming stream reset could stop the
SCTP outgoing stream before the next write flush. The later handoff skipped
the payload without returning an error. The production-read-pump regression
fails on the baseline with no DATA for the accepted reliable body. This differs
from #1076, which repaired local close ordering.

The core now hands application data to SCTP under the same mutable borrow as
send admission. The driver still batches transport packets. Earlier driver
control errors stay separate from the current send result. Refused sends leave
no payload, outstanding byte charge, or sent statistics. Media retains its
normal write path and FIFO. The four text/binary and reliable/zero-retransmit
histories require three complete bodies each, exact wire types and order,
DATA before RECONFIG, the full reset TSN boundary, peer DATA ingestion, and
both reset completions. Eight refusal histories cover direct-core oversized
input and association shutdown. They require unchanged counters and no late
DATA. The existing local-close fixture now uses a real SCTP association and
checks DATA/reset wire order. All new controls use the existing hosted
`stream_reset_` filter.

The initial production-read-pump control fails before repair and passes after
admission handoff. The final session PR records focused checks and independent
review. These controls establish local admission and transport ordering. They
do not prove application delivery after the peer closes its reader, or a cause
for the historical #853/#889 failures or the separate #1120 Closed-state timeout.
The server wire format and API stay the same. Rust, .NET, and Godot SDKs do not
use this standalone reference-client dependency patch and need no migration.
The broader A20 audit remains open.

The first hosted native interop run exposed an admission regression in the new
SCTP preflight. A valid negotiated channel can report `Open` while its local
stream initialization is still queued. The unchanged paired-wire boundary
control failed its first send with `ErrDataChannelClosed`. Removing that
redundant preflight lets the target channel hand its initialization and payload
to SCTP in FIFO order. Send accounting still commits only after SCTP accepts
the body.

A new remote-first control then fills the existing application read backlog.
SCTP learns the negotiated stream from peer DATA while the channel handler
cannot drain that input. Its first local send failed with `ErrStreamAlreadyExist`
although its body could emit. Negotiated initialization now configures the
existing stream without reopening it. In-band duplicate OPEN remains an error.
The helper stops and discards the remaining own batch on a handoff error.
Eight role, mode, and encoding histories require first-send success, exact
sent credit, both directional bodies, and correct wire type and ordering.
Both boundary controls pass after repair. The existing hosted
`stream_count_boundary` filter includes the new regression.

### F126 — Native abandonment retains public send credit without input

Issue [#1124](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1124),
under open #826 (A20). SCTP queues application-byte release events when a T3
timeout abandons zero-retransmit DATA. The RTC timeout path did not drain those
association events. The transmit path also missed events when a timed message
expired before its first packet was collected. Both paths could leave public
send capacity charged until unrelated input arrived.

The event-first core pump had another gap. It pumped internal read messages
only when lifecycle events were pending. The async driver's split read queues
could therefore miss a release that the SCTP handler had already forwarded.
A control using the driver's write/event/media/data poll order retains eight
bytes before this second repair. The event poll now pumps queued reads before
the split consumers inspect their output queues.

Two deterministic controls fail before repair. The no-inbound T3 history
releases zero of eight application bytes. The gather-only timed history retains
eight public outstanding bytes after expiry. Neither history needs a new
inbound packet. The repair drains association events after timeout processing
and transmit collection. It preserves the shared payload-read budget, pending
reads, per-association transport metadata, and final teardown packets.

Controls cover ordered and unordered zero-retransmit sends, text and binary,
unsent timed expiry, a full read backlog, one-time release, and reliable sends
that must keep their unacknowledged bytes and stay open. The native public
`writable()` control holds the remote reactor, requires release before the
unchanged deadline, and admits another send in both offerer roles and encodings.
The hosted interop script retains the `timer_abandonment_` controls.

The same investigation adds endpoint datagram and reactor progress evidence to
the unchanged remote-reset matrix. Explicit application-transmit loss and
pre-transmit hold histories pass. Encrypted datagrams do not identify their
SCTP chunk type. This does not prove a cause for the macOS failure in
[#1120](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1120):
at `cf36f243`, the offerer-sends/unreliable/one-byte cell retains local Open and
one outstanding byte while remote Closing retains zero. Keep #1120 open.
The server wire contract and the external Rust, .NET, and Godot clients remain
unchanged; none consumes this reference-client RTC patch.

### F127 — Repeated native reads renew one pump's payload budget

Issue [#1125](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1125),
under open #826 (A20). During one core pump, SCTP payloads move through an
intermediate queue before the application queue depth changes. The handler
recomputed its remaining budget after its own output queue emptied. It omitted
payloads already moved to that intermediate queue and could refill repeatedly.

A paired-SCTP history queues 320 messages with normal acknowledgments. The
event-first pump exceeds the 256-message application backlog before repair.
Each popped payload now stays charged to that handler's downstream depth until
the pump completes. A later handler reads the updated application queue depth.
The control requires bounded pumps and all 320 messages after the application
drains. Stream recovery and control events retain their existing paths.
This qualifies the bound activated by F126's event-first pump. It changes the
native reference RTC patch, with no server wire or external SDK migration.

### F128 — Native close oracle races sibling admission retirement

Issue [#1126](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1126),
under open #826 (A22). Local validation required one target receipt before
close but sent a sibling probe first. The sibling poller could observe required
channel loss and apply the existing pair-wide admission fence before the target
poller drained its queued input. A reliable, acknowledged target history
reproduces zero admitted receipts with raw=1 and discarded=1.

The original fixture keeps its exact one-receipt and close assertions. It now
checks sibling transport delivery afterward. A separate held-poller history
requires the established sibling-first fence, exact discard counters for both
labels, and one target close event. Production engine behavior stays unchanged.
This explains the local oracle failure, not the separate historical #1120 reset.

### F129 — Native pending channels outlive SCTP handshake failure

Issue [#1128](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1128),
under open #826 (A11 and A22). The SCTP association reports handshake retry
exhaustion, but the RTC handler only logs that event. Channels created before
SCTP connects have no DCEP deadline. Their handles stay Connecting without a
terminal close, so the reference engine cannot use channel loss for recovery.

A deterministic public-core control withholds INIT replies or COOKIE ACK and
exhausts the configured retry budget. Before repair it observes no close events
for the two pending channels. The handler now closes the association and sends
one failure event through the channel pipeline. Pending channels close once.
Channel creation on the failed physical transport returns a typed error.
The failure stays terminal when late handshake packets arrive; a fresh peer
connection can establish channels and deliver gameplay.

The public peer connection negotiates one SCTP association. Its channel registry
keys streams by stream ID. This change does not add support for multiple SCTP
associations within one peer connection. The generic SCTP handler retains its
existing attribution controls. No server wire or external SDK change is needed.
The external Rust, .NET, and Godot clients do not use this RTC path patch.

This confirmed recovery defect does not establish the cause of the historical
initial-exchange failures in #853 or #889, or the reciprocal-reset timeout in
issue #1120. Those investigations remain open with their original acceptance criteria.

### A20 — Combined reciprocal-reset packet loss

A new typed SCTP history advances open
[#1120](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1120).
It loses three distinct packets: accepted local DATA, the peer's initial reset
request, and the final success reply to the reciprocal reset. Four cells cover
reliable and zero-retransmit DATA with ordinary and wrapped TSNs.

The initial prefix retains one local byte before the reset arrives, while the
peer has requested reset with no buffered bytes. The real timer deadlines drive
reset retransmission, reciprocal InProgress, and DATA recovery. Reliable DATA
retransmits once and reaches the application. Zero-retransmit DATA produces one
FORWARD-TSN. Losing the final success reply requires the cached reply on retry.
Both reset directions finish, both retry timers stop, retained bytes reach zero,
and the same SID delivers fresh gameplay. Synthetic time never moves backward.

The four cells pass. Removing reset retransmission fails the exact retry count.
Removing cached success replay leaves the SID reserved. Production source is
restored byte for byte. The existing hosted SCTP `reset` selector includes the
new `reciprocal_reset_recovers_lost_request_data_and_final_response` test.

This narrows a combined packet-loss history that the native encrypted-datagram
controls could not identify. It does not reproduce the historical native
Open/Closing timeout or establish its cause. The public-channel state and async
reactor scheduling remain separate boundaries. Keep #1120 open.

### F130 — Closed epochs hide observed delivery violations

Issue [#1130](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1130),
under open #826 (A22), also affects the exact evidence contract in #775 and #636.
At `d2413907`, the capacity oracle records old-epoch duplicates and backward
receipts, then skips their verdict checks after reconnect closes that epoch.
The gap pass also skips bounds and overlap checks when completeness is exempt.

A valid small churn history inserts a duplicate before the next old-epoch
receipt and before reconnect, with increasing receipt timestamps. The baseline
oracle reports no reason. Twelve independent mutations are accepted: duplicate
and backward arrivals in all three delivery classes, plus out-of-range gaps,
gap/receipt overlap, and repeated gaps in both lossy classes. Legal old-epoch
gaps remain valid. The controls preserve the original event chronology.

The repair checks observed arrival and gap validity independently of whether a
stream still owes completeness. Closing an epoch still exempts its missing
receipts across the offline window. It does not permit observed duplicates,
backward arrivals, impossible ranges, or overlapping reports. Artifact replay
uses the same repaired oracle. The raw ledger and workload remain unchanged.

This changes internal capacity validation. The server protocol, runtime, and
external Rust, .NET, and Godot clients have no behavior or migration change.
The broader audit, memory campaign, and historical gameplay investigations
remain open with their original acceptance criteria.

### F131 — Handshake timer failure stops the driver without peer failure

Issue [#1131](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1131),
under open #826 (A20), is distinct from F129's SCTP failure after DTLS connects.
The DTLS timeout handler returns retry exhaustion as an error. The async driver
then stops without publishing a failed connection callback. A responder with no
admitted remote channels has no channel poller to notice that exit.

The public asynchronous control preserves ICE traffic and drops DTLS handshake
records. The offerer creates one pending gameplay channel. The responder starts
its handshake, exhausts at least eight attempts, and remains Connecting before
repair. Its ICE state has not failed. The baseline fails the exact missing
Failed-callback assertion in 1.58 seconds. This does not prove the cause of the
historical #853 or #889 initial-exchange failures.

The handler now records Failed, retains queued final output, retires its endpoint
and pending handshake setup, and publishes the existing terminal transport event.
The existing pipeline closes pending channels and publishes peer failure even
without an admitted channel. Late handshake input cannot reopen that physical
transport. Public channel creation rejects the terminal transport.

Two public histories cover a responder with no local channel and a responder
with pending in-band and negotiated handles. They require one Failed callback,
terminal channel events and typed Closed send/writable errors where handles
exist, and fresh-peer gameplay on the same network. The offerer that never
received a first handshake flight is not claimed to have exhausted a timer.
A direct RTC control replays a late first flight and processes later timeout and
candidate-pair events while checking terminal state and one failure callback.
Hosted selectors retain both the public and direct-core controls.

Only the native reference RTC patch changes runtime behavior. The server wire
contract stays the same. Refreshed source assessments show that the external
Rust SDK has no dependency on this patch, the .NET SDK has no native package
dependency, and Godot uses its engine's WebRTC implementation. All three are
unaffected and need no migration issue. The broader investigations remain open.

### F132 — Reset retry exhaustion leaves a timerless native stream

Issue [#1133](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1133)
advances open #1120, #912, and #826 (A20). A new deterministic history at
`9660fae3` exhausts the existing RECONFIG retry limit. The timeout path clears
its timer but ignores retry failure. The association stays Established and
retains the reset stream without another deadline.

The source control fails before repair at configured retry limits 0, 1, and 5.
It now requires one closure per stream, one terminal association event, and no
remaining timers. The repair closes the failed association and sends a typed
terminal event through RTC's existing transport cleanup. Undialed and established
channels retire; late input and new channel creation cannot reopen the transport.
The internal failure event and latch now name transport failure rather than
handshake failure, since both phases use this path.

The public asynchronous control opens two negotiated gameplay channels on each
endpoint before losing encrypted SCTP packets. ICE traffic remains live. One
channel requests reset; its sibling holds an admitted byte and blocked binary,
text, and writable calls. The real retry timer reaches the existing threshold.
The local channels close once, blocked operations return typed Closed errors,
and new creation fails. The untouched remote stays Open until explicit cleanup;
the test does not invent a remote receipt. Fresh peers deliver gameplay on the
same mock network. Existing reset and handshake-loss controls remain required.

This is a new confirmed teardown defect. It does not establish the cause of
issue #1120's historical five-second native timeout. That issue and published-package
replacement #912 retain their original acceptance criteria.

### F133 — Fatal native driver exit omits peer closure

Issue [#1134](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1134)
advances open #853, #889, #912, and #826 (A20). A fatal socket receive error
stops the async driver. The baseline marks the wrapper closing, but leaves the
RTC core live and sends no terminal peer callback. A responder with no admitted
remote channels has no channel event stream that can report its loss.

The new public control injects a typed BrokenPipe receive failure before remote
channel admission. It fails on the missing terminal callback before repair.
The driver now closes its core and publishes the ordinary terminal connection
event before ending. Pending handles close, new creation fails, and a fresh peer
can deliver gameplay on the same network. Established reliable and zero-retry
unordered channels also release blocked binary, text, and writable operations.
Both controls fail when the retirement call is removed and pass after restoration.
The driver-task callback marker stays in place so a close from the terminal
callback uses the existing reentrant path.
Creation checks the closing flag under its core and registration locks, so
a creator waiting on registration cannot add a handle after the driver stops.
Recoverable socket errors and ICE restart keep their existing behavior.

This repair does not establish the cause of the historical browser/native
exchange failures. Delivery across an independently failed physical peer remains
outside the earlier ordered-channel-closure claim in F115. The server wire is
unchanged. Current external-client source assessments show that Rust and .NET
do not use these patched packages, and Godot uses its engine WebRTC stack.
All three external SDKs are unaffected and need no migration issue.

### A22 — Typed generator sender identity

The session 511 generator change advances open #775, #1009, and #636 by removing
UUID string allocation from each receipt and gap lookup under the shared sender
registry lock. A fixed lookup probe removes 1,200,000 allocations totaling
43,200,000 bytes with the same checksum. The artifact registry retains canonical
string UUID keys. Focused controls retain exact old/new incarnation mapping,
unknown-player handling, byte accounting, and live reliable/reconnect replay.
See the bounded measurement in [ARM capacity audit](arm-capacity-audit.md).
This allocation result does not identify a historical stall cause or establish
full-workload memory, latency, or capacity. The original full-cell requirements
and all invalid attempts remain required evidence.

### F134 — Native shutdown overtakes capacity listener registration

Issue [#1135](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1135)
advances A20 under open #826 and the native investigations #1120, #853, and #889.
A final rerun of F133's public established-channel control hung after its Closed
callback. Diagnostics place the wait in parked sender completion, not cleanup.
The capacity waiter checks connection closure before awaiting the core lock.
If shutdown notifies while that lock is held, the waiter has no listener yet.
Its later lazy listener misses the non-sticky terminal wake. A real runtime
waits for the 50 ms polling timer; a static mock clock cannot make progress.

The deterministic control holds the core lock, polls writable until pending,
marks the connection closing, sends the actual terminal wake, then releases the
lock. The old path stays pending. The repaired shortage path registers an eager
listener and rechecks the full condition before waiting. Healthy sends allocate
no listener. Public established controls require parked binary, text, and
writable futures to return typed Closed errors immediately after the terminal
callback. They do not advance a fallback timer to obtain success.
The same lock ordering can admit a send after shutdown when capacity is free.
Writable and binary/text send variants recheck closure under the core lock.
Deterministic controls cover full and unlimited capacity, all four blocking
and nonblocking send variants, and typed Closed errors before the live core path.
The send-variant fixture uses pending channels; it proves terminal error
classification, not successful admission on an already-open channel.

This is a new confirmed wake-order defect. It does not establish the cause of
any older hosted failure. The server protocol and external SDKs are unchanged.

### F135 — Retired native SCTP accepts delayed gameplay

Issue [#1138](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1138)
advances A20 under #826, #1120, and #912. A deterministic packet history fails
on main because delayed DATA recreates a stream after reset retry exhaustion.
The same ingress accepts data after local abort or after an ABORT chunk in the
same packet. Stream creation and ACK scheduling can resume after terminal
cleanup.

The association now records retirement separately from its public Closed
state. Fresh server associations also start Closed and must still accept the
first handshake. Retired associations ignore new packets. Chunk processing
stops immediately if a chunk retires the association, before later chunks or
end-of-packet ACK work can run. Four teardown histories require no recreated
streams, timers, late events, or output. Delayed INIT and COOKIE-ECHO controls
cannot reopen the retired association; a fresh Closed server still completes
a real handshake. Existing reset, reliable gameplay, peer abort, graceful
shutdown, and public full-channel reset controls pass. Independent source
review approved the repair. Vendor production lint passes; its test target
still reports pre-existing upstream lint warnings.

This is a separate confirmed terminal-ingress defect. It does not establish
the cause of #1120's historical hosted timeout. The external SDKs do not use
this vendored SCTP association: Rust and .NET use other transport stacks and
Godot uses engine WebRTC. Their spectator behavior assessment is separate.

### F136 — Spectators have no live relay route

Issue [#1137](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1137)
advances the server lifecycle audit under #826. The new real WebSocket control
receives `SpectatorJoined` on main, then times out waiting for player gameplay.
Spectator admission records a read-only role but never publishes a room route.

The repair publishes a separate observer role in the broadcast routing set.
Its seated connection assignment stays empty. The join baseline captures live
player relay watermarks under the same routing fence that queues the snapshot
and publishes the route. Failed or canceled publication never makes the
observer visible. Exact seated membership and capability decisions exclude
observers. Personalized transactions reserve explicit public lifecycle frames
for observers; private session plans remain seated-only. Observer delivery
failure cannot degrade seated confirmation or cancel a later player plan.
Leaving advances the queue generation and removes the route. Already admitted
reliable data drains before the terminal membership barrier; old-room frames
cannot enter the next membership.

Focused controls cover real v2/v3 relay projection, all delivery classes,
JSON and binary fan-out, read-only rejection, lobby readiness, game start,
finalized late joins and reconnects, room switching, snapshot publication
failure, the routing fence, and slow-observer isolation. The v3 server-owned
`spectator_relay` capability lets clients detect support. V2 keeps its frozen
bytes and needs a qualified deployment. Custom coordinators must implement
read-only snapshot publication; its default refuses unsupported admission.

External client acceptance remains tracked separately in
[Rust #293](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/293),
[.NET #144](https://github.com/Ambiguous-Interactive/signal-fish-client-dotnet/issues/144),
and [Godot #368](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/368).
Direct and WebRTC game traffic is not mirrored automatically. Applications
must relay the viewing state and supply late viewers with an application
snapshot. The server does not retain or replay prior gameplay for spectators.

### F137 — DATA and shutdown retries leave timerless native links

Issue [#1140](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1140)
advances A20 under #826, #1120, and #912. The SCTP DATA and shutdown retry
handlers only log exhaustion. They leave streams and send reservations alive
after the last recovery timer stops. The DATA timer defaults to five retries;
its former comment incorrectly described that default as unlimited.

A source control fails on main with an Established association after zero DATA
retries. A separate shutdown mutation leaves ShutdownSent after its limit.
The controls cover configured limits of zero, one, and five, including both
SHUTDOWN and SHUTDOWN-ACK recovery. The repair reuses the existing reset-failure
terminal path. It clears streams, reservations, and timers, publishes one
association failure and drained event, and rejects local stream creation and
delayed packets after retirement. It does not change retry thresholds.

A paired public wrapper history drops encrypted SCTP records while preserving
ICE consent. On main, DATA recovery stops but the public transport remains
Connected through 300 virtual seconds. After repair, both negotiated reliable
channel handles close once, blocked binary, text, and writable futures return typed
Closed errors, and a fresh peer delivers gameplay on the same network. The
existing reset-loss history follows the same public terminal path. Graceful
shutdown uses the lower-level SCTP API; public wrapper close uses abort.
The shutdown controls prove the low-level terminal contract rather than a
separate public graceful-close path.

This confirmed defect does not identify the cause of the earlier hosted reset
or browser exchange failures. The server wire and all three external SDKs are
unchanged: Rust owns its WebRTC driver, .NET uses its own WebSocket transport,
and Godot uses engine WebRTC. None imports this private vendored client graph.

### F138 — Retired browser links suppress fresh exchange errors

Issue [#1141](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1141)
advances diagnostics for #853 and #889 under #826. The browser stores exchange
error keys containing the error kind, peer, and channel label. Both link and
membership retirement delete only the bare peer ID, which matches no key.
A new physical link to that peer therefore suppresses its first exchange
failure, and old peers retain diagnostic entries.

The browser now groups diagnostic keys by peer. Existing retirement removes
that peer's complete set; retries on one live link still suppress repeats.
A real orchestrator regression fails on the second physical incarnation before
repair. It covers both initiator roles across twelve incarnations, alternates
link replacement and membership retirement, preserves duplicate suppression,
and completes gameplay exchange after sends recover. Each retained peer has at
most four diagnostic kinds and required-label combinations. This is a source
bound and a lifecycle control, not a heap measurement or an explanation of
the historical exchange failures. External SDKs do not use this reference
orchestrator and require no migration.

### A22 — Bounded generator binary identity reads

The session 513 startup change advances #775 and #636 by replacing a whole
server-binary buffer with an 8 KiB read buffer. Paired fresh-process peaks on
1 MiB, 16 MiB, and a 51,597,736-byte debug binary preserve exact digest and byte
counts. The largest peak falls from 60,336 to 10,300 KiB. See the bounded
measurement and remaining limits in [ARM capacity audit](arm-capacity-audit.md).
This removes startup scratch only; retained event bounds and past generator
stall causes remain open.

### F139 — Graceful native shutdown loses gameplay acknowledgments

Issue [#1143](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1143)
advances A20 under #1120 and #912. Outbound SCTP SHUTDOWN reports the local
send acknowledgment point instead of the last received peer TSN. Its receive
path ignores the cumulative acknowledgment. If the final separate SACK is
lost, already delivered gameplay remains outstanding during shutdown.

The paired association control fails before repair on the outbound TSN.
The repair shares cumulative acknowledgment processing with SACK, including
stream byte release, retransmission timers, and forward-TSN bookkeeping.
SHUTDOWN carries no selective gaps or receive window, so the repair does not
invent those fields. The paired history covers text, binary, TSN wrap, a lost shutdown ACK,
duplicate credit, and simultaneous shutdown. Partial credit preserves the
remaining DATA and its timer. Pending unsent DATA prevents a premature ACK.

This proves the low-level association path. Public WebRTC close uses abort.
Separate pending-DATA drain and early endpoint retirement remain tracked in
[#1145](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1145).
The original hosted reciprocal-reset failure remains unexplained. No server
wire change occurs; the independent SDKs do not import this private stack.

### F140 — Failed browser answers retain unusable gameplay links

Issue [#1144](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1144)
advances A20 under #853 and #889. Initial offer failure retires its link, but
a rejected remote offer, answer creation, local answer, or remote answer only
reports an error. The unusable link remains until its P2P deadline.

The control fails before repair at the immediate availability assertion.
The browser now retires the affected current link after an SDP operation
rejects. The existing transport-loss path reports availability and permits a
fresh peer-requested retry. Candidate errors remain nonterminal. The table
covers all four SDP stages with a formed or unformed healthy sibling and
completes both required-channel exchanges after recovery.

This is a new failure history, not an explanation of either historical hosted
browser incident. The independent Rust, .NET, and Godot SDKs use their own
WebRTC implementations and do not import this reference orchestrator.

### F141 — Graceful shutdown skips admitted native gameplay

Open [#1145](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1145)
advances A20 under #826 and #912. Shutdown consulted an unused queue-length
field and retired endpoint routing at initiation. The shutdown writer retried
in-flight DATA but did not transmit pending admitted DATA. Terminal close also
removed complete unread messages before the application consumed them.

Paired wire controls fail before repair on premature ShutdownSent and Drained.
Shutdown now refuses new writes, drains both outgoing queues, and waits for the
cumulative tail acknowledgment. Complete received messages on read-enabled streams remain readable
through graceful transport closure. Stopped reads remain stopped. Endpoint retirement waits for the final
packet and the application read or stop of retained messages. A bounded RTC
pipeline control covers application read deferral beyond one handler budget.

The controls cover fragments, a small receive window, DATA loss, shutdown and
ACK loss, simultaneous shutdown, no-data closure, and delayed reads. Public
WebRTC close still uses abort. Loss of the final SHUTDOWN COMPLETE exposes a
separate endpoint response gap tracked in
[#1147](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1147).
These histories do not explain the original hosted #1120 reset timeout.

### A20 — Browser stack counters for unpaid exchange

Open [#889](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/889)
now retains browser data-channel message and byte totals beside raw and
admitted callback counts. Reports bind to the retained channel label, stream
identifier, and physical link. Sample and pending-query ages expose stale or
unavailable evidence. Sampling never delays gameplay or the run decision.

Both endpoint roles and missing required labels have quiet timer-driven
controls. A hung query remains single and does not extend the deadline. A real
Chromium paired-channel history verifies exact message counts and multibyte
payload sizes at both endpoints. This improves future failure evidence; it does
not establish the cause of #889 or #853. All three independent SDKs use their
own transport implementations and do not import this reference engine.

### A20 — Reciprocal-reset fixture core-lock placement

PR [#1148](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/1148)
recorded a new macOS #1120 failure in the reliable eight-byte initiator cell.
The local channel remained open with eight bytes outstanding. Local reactor
polls stopped while the remote retried reset and public state queries still
completed. The log does not reveal the exact pending core-lock waiter.

A separate deterministic history proves a fixture invalidation. The fixture
polls blocked binary, text, and writable futures once, then suspends them until
reset completes. A caller can instead become pending on the shared core mutex.
If it occupies the first notified waiter slot and stays unpolled, subsequent
fast-path state queries can suppress wakeups of the driver behind it. The
primitive uses an explicitly unfair mutex; the control cannot assume that any
Pending result proves buffer pressure.

The fixture now holds the local reactor while arming those suspended callers.
It verifies that no local reactor poll ran during arming, then resumes it before
ordinary reset. The explicit accepted-before-UDP control keeps its longer hold.
Both roles, required labels, byte limits, injected loss, original reset deadline,
blocked-send closure errors, byte release, and terminal send rejection remain.
This removes the demonstrated setup race. It does not establish the exact
waiter history of either hosted failure, so #1120 stays open.

### F142 — Lost final shutdown completion strands the native peer

Open [#1147](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1147)
advances A20 under #826 and native replacement qualification under #912.
After one endpoint sent SHUTDOWN COMPLETE and retired its association, a lost
COMPLETE left the peer retransmitting SHUTDOWN ACK. The retired endpoint
ignored that ACK, so the peer could only exhaust its shutdown retry budget.

A deterministic paired-endpoint history drops the first final COMPLETE after
association retirement. It fails before repair with the peer still in
ShutdownAckSent. The endpoint now produces a stateless COMPLETE reply through
its Sans-I/O output. The reply reflects the received verification tag and sets
the T bit, as specified by RFC 9260 sections 8.4 and 8.5.1. Recovery needs no extra
retention of closed associations or retirement timer. Accepted handshake metadata routes a
reflected reply to the surviving peer association.

A separate loss control delays the application's read of complete gameplay.
While that unread tail keeps the closed association registered, a validated
repeated ACK produces only a completion reply. It cannot revive DATA, timers,
or the association state. Reading the tail still returns its exact bytes and
permits final endpoint retirement.

This repairs the low-level graceful shutdown API. The public native WebRTC
wrapper still closes with ABORT. It does not establish the cause of either
historical #1120 reset timeout or the browser exchange investigations.

### A20 — Browser exchange receipt classifications

Open [#889](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/889)
now distinguishes admitted valid probes from invalid JSON, invalid shape, wrong
sender, wrong label, wrong sequence, and arrival outside the exchange window.
The counts remain separate from logical exchange debt. They survive physical
link retirement and clear when authoritative membership starts a new incarnation.

Both endpoint roles and both missing required labels have controls that compare
zero callbacks against six admitted invalid probes. Stack, raw, and admission
totals remain visible beside the exact classifications. Cutoff controls distinguish late
receipts from absent traffic. These diagnostics do not pay receipt debt, extend
run deadlines, or establish the historical incident's cause.

### F143 — Application reads leave native senders with a zero window

Open [#1145](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1145)
advances A20 under #826 and dependency replacement qualification under #912.
An application read frees SCTP receive space, but ordinary stream reads did
not advertise the reopened window. A sender with queued admitted DATA could
wait for a retransmission timer despite available receive space.

A paired wire control fails before repair because the read produces no window
update. It now delivers the exact admitted tail without advancing time during
ordinary gameplay, local shutdown, remote shutdown, and simultaneous shutdown.
Only a successful read that changes a full receive window to positive credit
requests an immediate acknowledgment. Empty, stopped, stale, and retired reads
do not request one. Reads with remaining credit keep the delayed-ACK policy.
These histories do not establish either historical native reset timeout's cause.

### F144 — Room leave keeps reference-client gameplay alive

New [#1150](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1150)
advances A20 under open #826 and the #853 lifecycle investigation. Browser and
native `RoomLeft` handlers reset accounting and generation but leave physical
peer links, queued gameplay, and room timers alive. Old callbacks can still
publish gameplay and pay receipt debt after room membership ends.

Both clients now retire links, cancel scheduled room sends and pairing work,
and clear current membership and readiness. Native callback receivers are
replaced at leave and restore. This also fences retained receipts from links
already removed before leave. Recorded exchange debt remains unpaid; leave
cannot convert an incomplete exchange into success. WebSocket keepalive stays
active. Native pairing and rebuild gates do not rearm outside a room.

Red-first controls cover connected browser channels, a held browser offer,
native link and retained-receipt retirement, queued and late callbacks,
scheduled relay and exchange work, held native gates, and fresh-generation
recovery. This is separate from #1119's server relay admission repair and does
not explain the original #853 or #889 histories. The server wire is unchanged.

### A20 — Compact replay send lookup

Open [#775](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/775)
now uses four-byte raw-record positions for out-of-order send lookup. An
explicit original-position tie preserves stable duplicate order and the last
record for each sender and sequence. Canonical ordered records still borrow
their send blocks. Explicit host overrides above the compact position range
keep the prior wide lookup. Raw events and replay verdicts do not change.

The focused resource control fails before repair with eight retained bytes per
send. A one-million-send cohort retains 8,000,000 bytes before and 4,000,000
after repair, with equal record checksums and an unchanged relay index.
Process observations and their overlap limits are recorded in the
[capacity audit](arm-capacity-audit.md). This does not qualify the whole process
memory envelope or explain historical generator stalls.

### F145 — Parent close leaves native channel handles live

New [#1151](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1151)
advances A20 under #826 and #912. The stronger native #1150 regression retains
a real connecting channel across room leave. It fails because the public
handle still reports `Connecting` after its parent peer connection closes.
The core data-channel handler's close method did not close retained channel entries.

Parent close now marks all retained channels `Closed`, clears handshake
deadlines and outstanding send bytes, and releases their internal channel
transport. Close statistics change once. Existing SCTP ABORT and DTLS output
flush before handler teardown. The repair adds no channel callbacks and does
not change pending message order.

A direct core matrix fails before repair and covers Connecting, Open, Closing,
already Closed, retained send credit, deadlines, and repeated close. Native
room-leave histories cover retained connecting and real open channels plus
old receipt ownership after link removal. The wrapper already rejected writes
through its global closing state; the repair corrects retained core lifecycle
and accounting instead of changing the wrapper getter. Published replacement
qualification in #912 must retain these controls.

### A20 — Native reset reactor outcome evidence

Open [#1120](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1120)
now records reactor poll returns and driver wake requests beside packet counts.
The counts help distinguish an entered poll without a recorded return, a
returned pending poll, a driver wake without another poll, and a completed
driver. Snapshots sample live counters separately; they do not prove a stall
or a lost wake on their own. The forwarding waker wakes the latest parent task. Fixture pause and resume requests remain
separate from driver wake counts. These counters exist only in the reset test
fixture and appear in its existing endpoint timeout snapshots.

A deterministic control checks pending, wake forwarding, held polling, and
completion. The original reset deadline and terminal-state assertions remain
unchanged. These observations improve the next failure report. They do not
reproduce or explain the historical hosted timeout, prove a scheduler cause,
or repair a confirmed transport defect.

### F146 — Stopped reads block graceful native drain

Open [#1145](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1145)
advances A20 under #826 and native replacement qualification under #912.
Stopping a read stream during graceful shutdown changes its state, then tries
to start a reset that is allowed only during ordinary gameplay. The operation
returns `ErrResetPacketInStateNotExist`. Its unread queue retains receive
space that the application can no longer free. A peer can still have admitted
DATA waiting for that space.

Graceful transitions now discard unread data only for stopped current stream
generations and advertise the freed receive space. Later DATA for those stopped
generations is acknowledged without refilling their queues. Read-enabled streams
and saved reset generations keep their data. Stopping reads during graceful
shutdown does not start a new stream reset. Ordinary gameplay reset pressure
and pre-handshake rejection behavior stay unchanged.

A red paired history fails with the reset-state error before repair. Wire
histories cover local and peer shutdown with stops before and during the
transition, full receive pressure, exact admitted outbound delivery, and
terminal endpoint retirement. Separate controls cover all four draining states,
repeated stops, stopped read rejection, and saved generation ownership. The
public WebRTC wrapper still uses ABORT; this repair concerns low-level graceful
shutdown and does not explain the historical #1120 timeout.

### F147 — Full driver queues block public native close

New [#1154](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1154)
advances native lifecycle qualification under #826 and #912. Public close awaits
space in the driver's command queue before it reaches its bounded driver wait.
A close from a driver callback cannot free that space. An external close also
waits indefinitely when the callback holds the driver.

Close now records the durable closing flag and tries a nonblocking wake. A full
queue already wakes the driver. Only the caller that owns the driver handle
consumes its completion receiver. Normal driver exit retains its graceful
cleanup window. Real public callback and external-close histories fail before
repair and pass with both reactor modes. A separate concurrent-close control
protects completion ownership.

### F148 — Canceled native close strands channel readers

New [#1156](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1156)
extends #1151. Canceling close after it takes the driver handle aborts the driver
but skips the async stream cleanup. A retained public channel then waits for an
event that can never arrive. The regression holds the core lock, cancels close,
observes driver completion, releases the lock, and requires terminal channel EOF.
The baseline fails that requirement. Both reactor modes pass after repair.

An independent cleanup task protects this close operation from caller
cancellation. It uses a weak peer reference and exits when ordinary close
finishes. This adds no task to an idle live peer. The wire format does not change.
Neither finding establishes the cause of the historical #853 or #889 failures.

### F149 — Native selective acknowledgments lose wrapped DATA

New [#1158](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1158)
advances A20 under #826, #912, and #1145. SACK gap offsets used absolute TSN
subtraction and truncated the result to 16 bits. Reordered DATA across sequence
wrap produced incorrect gaps. Grouping at offset 65,535 could overflow.

Gap offsets now use wrapping distance and checked wire-width conversion.
Unreportable DATA stays queued until cumulative progress makes it reportable.
Three controls fail before repair. Paired histories cover ordinary and wrapped
sequences, selective byte release, retry of only the missing first payload,
exact ordered delivery, and graceful closure at both endpoints. All 37 scoped
shutdown and queue controls pass. Native replacement qualification must retain
these controls. This does not establish the cause of #1120.

### F150 — Capacity sends escape their incarnation lifetime

New [#1159](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1159)
advances exact capacity evidence under #775 and #826. An announced epoch was
enough to count a send as completed, even before rejoin or after disconnect.
Closed-epoch completeness exemptions could then hide the missing deliveries.

The oracle now checks completion time against each incarnation's active
interval. Equal-microsecond boundaries remain legal. The runner records send
completion and churn in the same peer task on one clock. Controls reject old,
future, and offline sends across repeated reconnects and shuffled artifacts.
A complete-summary control removes the impossible send's entire fan-out and
proves rejection plus exact JSONL replay. State grows with incarnations rather
than sends. This does not bound whole-process memory or explain generator lag.

### Relay and browser receive work

New [#1160](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1160)
advances #636. Reliable rows receive timestamps under the queue lock and append
in timestamp order. Deadline lookup now stops at each lane's first reliable
row, then takes the minimum across lanes. A 1,024-row dense reliable drain
needs 1,024 visits instead of 524,800. Mixed lossy rows, inherited Latest age,
the legacy prefix, and generation barriers retain the full-scan deadline.
Long lossy or control prefixes can still require scans.

Browser binary gameplay also reuses one nonstreaming UTF-8 decoder per engine.
The boundary control constructs 58 decoders before repair and one afterward.
Text receipts create none. Truncated UTF-8, repeated BOMs, channel replacement,
and the existing byte limit retain the same results. An isolated Node probe
of 500,000 decodes takes 65.8–70.6 ms before and 40.8–53.4 ms after, with equal
38,000,000 checksums. This measures local decoder work, not Chromium throughput
or a repair for #853 or #889.

### F151 — Early capacity sends count as offered work

New [#1162](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1162)
advances #775 and #826. A complete archive with a send completed one microsecond
before its declared schedule returns a valid summary on the baseline. The lag
calculation saturates the negative difference to zero. Incarnation checks do
not establish scheduled timing.

The oracle now rejects early completion with a typed scheduled-send mismatch.
Raw sent count stays visible, and the invalid send does not cover offered work.
Equal-microsecond and late sends remain legal. The control checks the full
summary and exact JSONL replay. The earlier incarnation control now places its
relative timeline after the planned send, so it isolates the lifetime boundary.
This does not identify a cause of the historical generator stalls.

### F152 — Native selective acknowledgments exceed the packet budget

New [#1163](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1163)
advances #1145, #912, and #826. A paired history sends 800 two-byte ordered
messages, starts graceful shutdown, and delays alternate DATA chunks. The
baseline emits a 1628-byte SACK with a 1428-byte association MTU.

SACK generation now prioritizes the earliest gaps within the packet and chunk
wire budgets. Duplicate reports use the remaining space. Received DATA remains
retained even when its gap report does not fit. The paired regression covers
TSN wrap, exact ordered bytes, later cumulative acknowledgment, and closure at
both endpoints. Separate budget controls inject duplicate reports at queue level
and cover minimum packet size and the largest chunk wire length. The separate
inbound duplicate timing and reporting candidate is tracked in
[#1165](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1165). The public wrapper still uses
ABORT for close. This does not explain the historical #1120 reset timeout.

### Native ordered queue insertion work

New [#1164](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1164)
advances #636 and replacement qualification under #912. The payload queue
binary-searched each serially increasing TSN despite its documented append
path. A 1024-row insertion control records 11242 serial comparisons before
repair. Direct append uses at most one comparison per nonempty insertion.
Reordered arrivals retain binary insertion. Exact drain order and bytes survive
sequence wrap and wrapped deque storage. This measures queue work and does not
establish end-to-end throughput or whole-node capacity.

### F153 — Duplicate native DATA omits immediate acknowledgments

Issue [#1165](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1165)
advances A20 under #826 and #912. The inbound DATA handler checked queue
admission before calling the queue's duplicate recorder. Cumulative duplicates
could select delayed acknowledgment, and selective duplicates did not report
their TSN. A lost first SACK therefore left the sender waiting for another timer.

The paired control fails before repair with zero immediate SACKs instead of one.
The saved reset-generation control fails with an empty duplicate report instead
of TSN 1. The handler now records duplicates and requests immediate acknowledgment
before stream admission or generation selection. It does not redeliver payloads
or release sender bytes twice. Seventeen table histories cover cumulative and
selective duplicates, TSN wrap, graceful sender drain, saved reset generations,
and actual inbound duplicate reports at packet/chunk size limits.

The three focused duplicate-DATA controls pass. This qualifies the private
native SCTP stack; it does not establish the historical #1120 timeout's cause.
The independent Rust, .NET and Godot SDKs do not import this vendored stack.

### F154 — Fragment completeness rescans growing native messages

Issue [#1168](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1168)
advances #636, #826 and replacement qualification under #912. A 1,024-fragment
history with the ending fragment present early performs 523,776 adjacency
checks before repair. Repeated completeness scans cause this quadratic work.

Each fragment set now stores its count of nonadjacent neighbors. Construction
scans the initial vector once; insertion updates at most three neighbor pairs.
Completeness reads that count and the endpoint flags. This adds one machine
word per fragment set and no allocation. It does not remove sorted-vector
insertion work or establish a whole-client throughput improvement.

Twenty-one focused reassembly controls pass. An independent full-scan oracle
checks all 720 permutations of six fragments at ordinary and wrapped TSNs.
Constructor controls retain gap, duplicate and nonordered-vector behavior.
A paired wire history checks exact bytes, one receive, and sender byte release
for a reordered 1,024-fragment message. The hosted native runner includes the
reassembly selector. The independent SDKs do not import this private stack.

### F155 — Reconnect observers cannot restore current peer metadata

Issue [#1167](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1167)
advances A01 and A20 under #826. PlayerLeft removes the departed roster entry.
The old PlayerReconnected carries only identity and epoch. A spectator admitted
during the absence has never received the name or current flags. A departed
metadata cache therefore cannot restore every observer's roster.

The new member/spectator control fails before repair with null metadata.
Successful restores now include optional current PlayerInfo inside the same
PlayerReconnected frame. The publication snapshot captures coordinator readiness
before retention. The notification uses current room authority and incarnation,
with sequence zero. This preserves the two-frame transaction capacity and adds
no PlayerJoined callback. Terminal repair notifications can omit metadata.

Focused checks cover current readiness, authority restoration and a successor
holding authority, publication retry with two control slots, and replay baseline
precedence. A real WebSocket history compares observer metadata with the
returning player's baseline in both v2 and v3. Live and replay projections omit
nested incarnation fields for v2 and legacy snapshot fields for v3. Reference
clients consume readiness before reporting membership. Duplicate v2 metadata
does not re-arm StartGame; v3 retains its incarnation fence.

Rust library callers must add the optional player field when constructing the
public message variant. Independent SDK adoption remains tracked separately:
[Rust #294](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/294),
[.NET #145](https://github.com/Ambiguous-Interactive/signal-fish-client-dotnet/issues/145),
and [Godot #374](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/374).
Their current decoders accept the added field but do not apply it. SDK tests and
the Godot runtime drill remain client work; server controls do not prove them.

### F156 — Delayed ordered fragments overtake completed unordered messages

Confirmed on `b264ec73` under #826 and #912; repair tracked in #1170.
The queue's mixed-message read contract selects the earlier completion.
Ordered messages instead received their arrival counter on their first
fragment. The history ordered-start, unordered-complete, ordered-end returned
the ordered message first. The new queue control fails on that exact payload
assertion before repair.

Assign the ordered arrival counter once when the message first completes.
Keep ordered SSN delivery and completed timestamps unchanged. Queue controls
cover both completion orders and TSN wrap. A paired-wire history uses actual
fragmented writes, reordered packets, mixed ordering modes, exact bytes,
released send/receive credit and graceful shutdown. This establishes the
low-level mixed-message contract; it does not claim a public browser incident.

### F157 — Complete unordered gameplay scans unrelated retained fragments

Confirmed on `b264ec73` under #636, #826 and #912; repair tracked in #1171.
Complete single-chunk unordered messages entered the incomplete-fragment
vector before a linear completion scan. With 1,024 retained fragments, 1,024
complete messages performed 1,049,600 fragment visits. The red control first
checks exact delivered bytes and retained accounting, then fails its work bound.

Queue complete unordered messages directly. The same history performs zero
fragment visits after repair, retains exactly 4,096 incomplete payload bytes,
and releases them on FORWARD TSN across ordinary and wrapped TSNs. Fragmented
unordered messages keep their existing reassembly path. This is a deterministic
operation count, not a throughput measurement or a bound on fragmented traffic.

### F158 — SACK construction builds reports outside its wire budget

Confirmed on `b264ec73` under #636, #826 and #912; repair tracked in #1172.
The prior packet-size repair truncated gap reports after construction.
A 2,048-gap receive history still scanned all retained chunks with zero report
slots. Positive small budgets also built and discarded later reports.

Apply the budget while generating reports. Complete the final included range
before stopping. The disjoint-gap control scans zero chunks with zero slots
and at most budget plus one chunks otherwise. Contiguous runs still require
visiting their full range. Reference-prefix controls retain long contiguous
runs, sparse gaps, TSN wrap, offsets 65,535 and 65,536, empty queues and unlimited
reports. Existing paired shutdown histories retain unreported DATA and later
deliver all gameplay exactly. No receive payload is removed by report trimming.

All three findings affect only the vendored native reference-client stack.
The independent Rust SDK manifest has no RTC/SCTP dependency. The .NET SDK
owns signaling and relay only. Godot uses browser or Godot WebRTC transports.
No signaling schema, server relay ordering or external SDK API changes.
Their current manifests and scope descriptions were inspected for this session.

### F159 — Unordered fragmented gameplay repeatedly scans retained fragments

Confirmed on `7345ae76` under #636, #826 and #912; repair tracked in #1175.
A 1,024-fragment unordered message performs 524,800 completion-scan visits.
The red control first delivers exact payload bytes and releases receive credit,
then fails its work bound. This is separate from the single-chunk F157 path.

Index incomplete contiguous runs by their first and last TSNs. Join at most two
runs per fragment, respecting message beginning and ending flags. The same
message uses 2,046 endpoint probes. Reverse and alternating arrival histories
have the same probe bound. Completed-message extraction and its one adjacency
check remain linear. Reordered vector insertion can still move a linear suffix.
FORWARD TSN rebuilds the retained run index after discarding a prefix. The index
holds at most two entries per retained run. This is an operation-count result,
not a throughput or whole-queue complexity claim.

Controls cover all 720 arrival orders of two adjacent three-fragment messages,
ordinary and wrapped TSNs, retained duplicates, partial abandonment, later
message completion and exact byte accounting. The paired tail-first association
control now covers both ordered and unordered messages with exact payloads,
one send-credit release and an empty in-flight queue. The existing hosted
`reassembly_` selector includes the new controls.

External-client assessment: the Rust SDK manifest at `da8f2d16` has no RTC/SCTP
dependency. The .NET README at `e467c871` assigns peer networking to the game.
The Godot README at `5717e958` uses browser WebRTC or the Godot WebRTC extension.
These clients do not use this vendored native reference-client queue. No SDK
change or migration is required.

### F160 — Native chunk-array sources confuse bytes and chunks

Confirmed on `8bf30de2`; repair tracked in #1177 under #826 and #912.
After one byte is split from a six-byte array entry, `remaining()` still returns
six. Exhausted multi-byte entries still report readable bytes. Empty entries
can make the old subtraction underflow. The red source control fails on the
first partial read. Track remaining bytes independently from consumed positions.
The same control passes with zero limits, empty arrays, interior and trailing
empty entries, uneven sizes, exact payloads, and complete-chunk counts.
The current stream writer checks remaining bytes before one source pop, so this
is a public source-contract defect, not proof of wrapper gameplay corruption.
The hosted native runner now includes the `bytes_array_` selector.

### F161 — Graceful native shutdown leaves writable readiness set

Confirmed on `8bf30de2` under #1145. Stream readiness reports writable after
shutdown starts, while writes return `ErrStreamClosed`. The red paired control
fails in `ShutdownPending`. Move the existing shutdown-state refusal into
`is_writable`. Cover all four live shutdown states, unchanged caller buffers
on refusal, and exact delivery of gameplay admitted before shutdown.

### F162 — Unread native ordered messages cross the stale sequence boundary

Confirmed on `8bf30de2`; repair tracked in #1178 under #826 and #912.
The application-read cursor also controls ordered protocol admission. With no
application reads, one-byte messages SSN 0 through 32768 enter the queue, but
SSN 32769 is rejected as stale with only 32769 payload bytes retained.
The same paired-wire control fails on the baseline after the sender receives
an acknowledgment for message 32769; its receiver retains 32769 bytes instead
of 32770. The repair retains and delivers all 65540 messages in that history.
Advance protocol admission when contiguous complete messages become ready.
Keep a separate ready queue until application reads release those bytes.
Preserve stream order, mixed completion timestamps, FORWARD TSN, and saved
reset-generation ownership. Queue and paired histories cross a complete SSN
cycle with unread messages. This does not establish arbitrary reordering
beyond the serial-number half-space.

External-client assessment for F160–F162: the current Rust SDK manifest has no
RTC/SCTP dependency. The .NET SDK project has no native peer transport dependency.
Godot's mesh adapter constructs engine `WebRTCPeerConnection` objects.
None imports these private reference-client transport sources. The signaling
wire is unchanged; no SDK migration is required.

### A18 follow-up — Native reset reactor scheduling evidence

Issue #1120 remains open. The reset fixture now records monotonic poll start,
poll end, last driver wake, and longest poll duration with existing counters.
The deterministic probe covers an active poll, Pending, a wake before the next
poll, a held fixture, and terminal completion. Independent snapshot loads can
race with reactor work and do not establish a causal verdict. Passing focused
wire histories do not explain or repair the hosted timeout.

### F163 — Graceful shutdown admits unusable new native streams

Confirmed on `0aa407ae` under #1145. `open_stream` succeeds during graceful
shutdown even though its returned stream rejects every write. The paired
regression fails first in `ShutdownPending`. Refuse new local streams in all
four shutdown states without changing stream or generation state. Preserve
pre-handshake opens and internal receive admission. A remote stream first seen
after local shutdown still delivers its previously admitted exact payload.
The paired control checks both endpoints through terminal application reads.

### A18 performance follow-up — Ordered fragment insertion

Issue #636: a 1,024-fragment ordered message requires 10,230 serial comparisons
on the old insertion path. Appending increasing TSNs directly reduces that to
1,023, including wrap. Reordering keeps binary insertion; duplicates retain
exact data. Existing exhaustive six-fragment permutations and paired gameplay
controls qualify the changed seam. This measures insertion work only.

External-client assessment: the current Rust SDK manifest has no RTC/SCTP
implementation dependency. The .NET SDK project has no package dependencies.
Godot constructs engine `WebRTCPeerConnection` and `WebRTCMultiplayerPeer`
objects. None imports this private reference-client stack. These changes leave
the signaling wire unchanged and require no external SDK migration.

### F164 — Unordered abandonment repeatedly rebuilds retained fragment indexes

Confirmed on `7ad88189`; tracked in #1182 under #636. Repeated unordered
FORWARD TSN prefix discards rebuild every retained run endpoint and shift the
retained vector. A 1,024-fragment control advances one fragment at a time.
The baseline performs 525,823 scan/index-rebuild fragment inspections and
523,776 retained element shifts. Incremental run trimming and deque prefix
removal reduce the measured inspections to 2,047 and remove those shifts.
These counts exclude byte-release loop visits and do not measure throughput.
The control verifies byte accounting, sequence wrap, later reordered message
completion, and exact payloads. A second control keeps later run endpoints
intact when an earlier run loses its beginning. Existing fragment permutation
controls continue to cover reordered completion.

### F165 — Retired browser negotiation holds gameplay backlog credits

Confirmed on `7ad88189`; tracked in #1183 under #853/#889/#826. A retired
physical link fences stale SDP but its hung negotiation wait keeps task and
queued-signal backlog credits until the deadline. Twelve authoritative
removal/readd cycles, each with a held offer and 48 queued answers, stop the
baseline with connection error code 3. Cancel retired-link wait wrappers when
removing or replacing that link. The same history keeps fresh same-ID SDP,
healthy sibling ICE, and relay gameplay active. Late stack resolve and reject
remain fenced. The underlying browser promise cannot be canceled; this repair
releases the orchestrator wait and its queued task credits. Existing receipt
retirement, global teardown, deadline, and backlog refusal rules remain intact.
This is a new reproduced recovery defect, not proof of the historical hosted
establishment or missing-exchange failure causes. Real browser/native three-player
mesh and reconnect exchange pass in Nextest `5342ece6` after rebuilding both
reference clients.

### F166 — Reset during graceful shutdown loses future ordered sequence progress

Confirmed on `7ad88189`; tracked in #1184 under #1145/#826. Stream removal
retains a deferred future-generation sequence marker only in Established.
A reset held by unread old DATA can finish during graceful shutdown after
FORWARD TSN skips future SSN 0, before future DATA arrives. Dropping its marker
makes later admitted SSN 1 unreadable despite transport acknowledgment.
The first red control loses the SSN 1 marker in ShutdownPending. Its final
old-source replay also fails the application read: accepted SSN 1 waits for
abandoned SSN 0.
Retain markers in all four live shutdown states. The control checks exact old
and future payloads through terminal reads and endpoint drain. Terminal close
still clears markers and refuses reopening. Strict-peer reachability of this
early future-generation ordering is unproven: reciprocal reset also acknowledges
the original request in a conforming exchange. This control qualifies the
implementation's existing accepted-DATA contract; it is not a paired-wire
reproduction or proof of a public gameplay incident.

External-client assessment for F164–F166: the Rust SDK manifest at `da8f2d16`
has no native RTC/SCTP dependency. The .NET project at `e467c871` has no package
dependencies. The Godot README at `b7c04a86` assigns mesh transport to browser
WebRTC or the Godot extension. None imports these private native transport or
browser reference-engine sources. The signaling wire is unchanged; no SDK
migration is required.

### F167 — FORWARD TSN scans absent sequence numbers

Confirmed on `7d63f953`; tracked in #1187 under #636/#826. A forward cumulative
advance invokes receive-queue removal for every sequence number, including
absent DATA. A red-first control advances 4,096 TSNs with two retained chunks
at or below its marker. It records 4,097 removal attempts, including the next
cumulative-progress probe. Remove the retained sorted prefix and set the
accepted cumulative point directly. The repaired history makes three attempts.
Established and ShutdownSent controls cover normal and wrapped TSNs, exact
unread payloads, later queued gaps, subsequent cumulative advancement, and
full receive-credit release. This is a queue-operation measurement; no
end-to-end throughput or historical timeout cause is established.

Endpoint qualification also fixes open #1186: process-local port counters
collide under Nextest. The original endpoint shutdown/reset selector fails
11 tests with `AddrInUse`. Holding OS-assigned sockets and deriving each
endpoint address from its live socket passes the same 15 controls. Packet
assertions and production endpoint code are unchanged.

External-client assessment: the Rust SDK manifest at `da8f2d16` has no native
RTC/SCTP dependency. The .NET project at `e467c871` has no package dependencies.
The Godot README at `ca58e382` uses browser WebRTC or the Godot extension for
mesh transport. These changes affect only the native reference-client graph;
the signaling wire and SDK contracts do not change.

### F168 — Delayed native channel admission suppresses its close event

Confirmed on `7d63f953`; tracked in #1188 under #826/#1120/#853. Callback
admission validates immutable delivery modes and queues a selected channel.
If its core retires before that announcement drains, engine admission repeats
property reads against the removed core. It returns without polling the
channel's retained terminal event. The baseline real-wire history times out
waiting for close after the peer acknowledges input and completes reset.

Trust the previously selected channel identity, then poll its queued events.
Unselected/direct channels still require delivery-mode validation. Four cells
cover both required labels with held-open and production-style queued-open
handling. They check exactly one terminal close, finished polling tasks, no
false pair formation, and fresh-generation delivery on both labels. Held-open
controls retain the exact queued payload. Production-style controls keep the
existing observed-loss fence; any delivered payload must remain exact and
unique. The repair does not promise payload delivery after observed loss.
This is a new reproduced close-notification defect, not proof of the historical
reset timeout or initial browser/native establishment failure.

The external-client assessment for F167 also applies to F168: none of the
three independent SDKs imports the native reference engine; no wire or SDK
migration is required.

### F169 — Empty SCTP writes skip ordered gameplay sequences

Confirmed on `060f7d19`; tracked in #1190 under #826/#1145. The public SCTP
write API returns zero for an empty source. Packetization emits no DATA but
advances the ordered SSN. Following complete messages receive transport ACKs
without becoming readable. The red paired control `6b50e334` fails its exact
application read after three empty slice writes.

Treat zero remaining source bytes as a no-op after checking writability and
before allocating a message identity. The final control covers slice, Bytes
and chunk-array sources, SSN 0 and wrap, ordinary delivery and graceful shutdown,
exact following payloads, unchanged message identity and sequence, and terminal
endpoint drain. The existing four-state shutdown control also refuses empty
writes. This qualifies the low-level API. WebRTC empty text and binary messages
already use a one-byte filler, so this does not establish a public data-channel
incident or explain the historical reset timeout.

### F170 — In-order unordered fragments repeatedly search retained DATA

Confirmed on `060f7d19`; tracked in #1191 under #636/#826. Insertion searches
the retained incomplete unordered queue for each increasing TSN. Red control
`92a21851` records 11,242 comparisons for 1,024 fragments. Append after a strictly
earlier tail TSN and retain binary search for reorder and duplicate paths.
Green control `bf69d77b` bounds the same work to 1,024 comparisons and passes
five unordered controls. They retain exact payload, receive credit, ordinary
and wrapped TSNs, reversed and alternating arrivals, 720 permutations, and
partial abandonment. No throughput or historical timeout claim follows.

### F171 — Delivery reports repeat sorting and terminal-tail scans

Confirmed on `060f7d19`; tracked in #1192 under #636/#826. Both reference
validators append and sort one retained gap at a time, then attempt retirement
once per gap. A legal 256-range report repeats the same incomplete terminal
scan 256 times. The native red control `b567942e` records 65,536 gap visits
instead of 256. Batch append, sorting and retirement per sender/epoch using
the existing whole-report validation groups. No validation moves past mutation.
Controls retain reversed ranges, interleaved senders and epochs, atomic
rejection, exact counters, terminal tail reads, and fresh-epoch delivery.

A fixed Node workload with 8,192 alternating Latest/Volatile singleton gaps
in 32 legal reports changes median report processing from 993.832 ms to
120.430 ms across seven alternating-order attempts per source. The 256-range
case is inconclusive. The capacity ledger records scope and measurements.
The historical browser mesh and native reset failures remain unexplained.

External-client assessment: #1190/#1191 affect the private native transport
graph only. The current Rust manifest has no RTC/SCTP dependency; .NET uses
its own implementation; Godot uses browser WebRTC or its engine extension.
For #1192, current Rust and .NET validators have the same per-gap sorting
and retirement pattern. Follow-up issues
[Rust #295](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/295)
and [.NET #146](https://github.com/Ambiguous-Interactive/signal-fish-client-dotnet/issues/146)
track independent repairs within their existing retained-gap limits. The Godot
source tree has no delivery-accountability implementation and imports neither
reference validator. The signaling wire is unchanged; no application migration
is required. SDK issue completion is separate from server CI.

### A20 shutdown qualification — Mixed streams and lost abandonment recovery

Open #1145/#826 now has a paired simultaneous-shutdown history with a lost
middle fragment of an ordered Rexmit(0) message and a lost standalone FORWARD
TSN. Ordinary and wrapped TSNs require an identical FORWARD TSN retry before
both endpoints close. Reliable successors retain exact order and bytes on the
same stream and another stream. Reverse gameplay remains readable after
transport closure. Each emitted SHUTDOWN or SHUTDOWN ACK requires an empty
outbound tail and the exact final cumulative acknowledgment. New writes fail.
Each endpoint emits exactly one Drained event after the final application read.
The abandoned fragmented payload never reaches the application.

The new `simultaneous_shutdown_retries_lost_forward_tsn_` control passes on the
existing implementation. It adds acceptance evidence; it is not a new repair
or proof of the historical #1120 cause. Broader #1145 qualification stays open.

### A20 browser recovery qualification — Retirement at each stack wait

Open #853/#889/#826 now has fourteen orchestrator histories: seven held WebRTC
operations, each with late resolution and late rejection. The held operations
are offer creation, local offer installation, remote offer installation, answer
creation, local answer installation, remote answer installation, and candidate
installation. Each history retires the same peer twelve times with 48 queued
signals per incarnation, then starts a fresh physical link.

Old calls cannot advance past their held phase or deliver queued SDP/ICE into
the replacement. Fresh and healthy sibling links each send and receive exactly
one probe on both required labels, with exact payloads. Relay gameplay also
continues. This extends the existing `peer-work.test.ts` fixture. The stack is
a deterministic fake; these histories qualify orchestrator cancellation and
credit release, not Chromium transport establishment or historical failure
causes. No production browser recovery change was needed.

### F172 — Retained delivery gaps repeat linear searches and backlog sorting

Confirmed on `4e4db8d0`; tracked in #1194 under #636/#826. Both reference
validators scan the retained sender/epoch gaps for each overlap or contiguous
GameData membership check. They also sort the entire retained backlog when a
report adds later ranges. Retained gaps are sorted and nonoverlapping, so
binary lookup can check the first range whose end reaches the input start.
Reports still validate completely before any retained-state mutation.

The new backlog controls retain 512 singleton ranges in ascending, descending,
and interleaved arrival order, reject duplicate and boundary/interior overlaps,
and drain exact intervening gameplay. Existing
controls cover interleaved senders/epochs, reversed report ranges, counters,
terminal reads and fresh epochs. The capacity ledger records measurements and
their limits. This reduces local validator work; it does not establish a
historical WebRTC failure cause or a server throughput result.

External-client assessment: current Rust SDK `da8f2d16` and .NET SDK `e467c871`
validators have matching retained-gap scans and sorting. Existing
[Rust #295](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/295)
and [.NET #146](https://github.com/Ambiguous-Interactive/signal-fish-client-dotnet/issues/146)
now include this follow-up and acceptance cases within their current state
limits. Godot `38bb6765` has no delivery-accountability implementation and uses
browser WebRTC or its engine extension. It imports neither reference validator
nor the private native transport graph. The signaling wire and public SDK
contracts stay unchanged; no migration is required. The new browser recovery
and SCTP shutdown histories change tests only. SDK follow-up completion is
separate from server CI.

### F173 — Delivered gameplay overlaps a retained drop after an explained gap

Confirmed on `cd9db830`; tracked in #1196 under #826. Both reference validators
check delivered-sequence overlap only for contiguous DATA. With baseline
sequence 0 and retained exact ranges [1,1] and [2,2], DATA sequence 2 uses the
first range to explain its skipped prefix and reaches Apply despite the second
range reporting it dropped. The same branch exists in the root conformance
oracle. Red-first controls reproduce the contradictory acceptance.

Check the first retained range after the explained prefix before consuming
ranges or advancing the sender. Rejection preserves the exact retained state,
so a later valid sequence can still use those ranges. This changes invalid
traffic handling through the existing delivery-violation path. Valid traffic
and the wire schema stay unchanged. It does not explain the historical native
reset or browser mesh failures.

Current independent Rust `da8f2d16` and .NET `e467c871` sources have the same
conditional check. Follow-ups [Rust #296](https://github.com/Ambiguous-Interactive/signal-fish-client-rust/issues/296)
and [.NET #147](https://github.com/Ambiguous-Interactive/signal-fish-client-dotnet/issues/147)
track separate correctness repairs. Godot `e3c075f7` has no delivery-accountability
validator and imports neither reference implementation. No valid application
traffic needs migration. SDK issue completion is separate from server CI.

### A20 browser failure evidence — Initial offer cleanup

Open #853/#889 now retains the failed browser operation and bounded connection
snapshot before initial offer cleanup deletes its link. Previously the later
orchestrator snapshot could no longer inspect that connection. Controls cover
both channel-creation steps, offer creation, local description installation,
missing SDP, and rejection after retirement. They retain pre-close transport
state, channel progress, the original exception, and fresh-link recovery.
Late failures cannot describe or remove a replacement. These are deterministic
failure controls; the historical Chromium/native incident remains unexplained.
The three external SDKs import neither this browser reference engine nor its
private diagnostic output; no SDK migration is required.

### F174 — Multi-stream abandonment exceeds the native packet budget

Confirmed on `cd9db830`; tracked in #1197 under #1145/#826/#912. A paired
history with sixteen ordered streams and MTU 64 emits an 84-byte FORWARD TSN.
The report includes all abandoned stream sequences without a packet budget.
The red control fails on the published source; normal-MTU controls also cover
more ordered streams than one recovery report can hold.

Cap the report at the last cumulative TSN before an ordered stream whose entry
cannot fit. Acknowledgment permits the next bounded prefix. Never advance the
cumulative point over an omitted required stream entry. Paired controls use
MTU 64 and 1200, ordinary and wrapped TSNs, a lost first report, and exact
matching retry before graceful completion. Every reliable successor stays
readable with exact bytes after transport closure. Budget controls cover
repeated stream sequences, unordered chunks, fragment boundaries, small report
slots, and the 16-bit chunk-length limit.

This repairs the low-level multi-stream API. The reference gameplay engines use
two required channels; these histories do not establish a historical public
channel failure cause. The independent Rust SDK has no native RTC/SCTP graph,
.NET uses its own implementation, and Godot uses browser WebRTC or its engine
extension. None imports this private patched transport. Their wire contracts
and migration requirements do not change. Published replacement qualification
under #912 and the broader native shutdown investigation remain open.

### F175 — Incoming native recovery follows the local send-order setting

Confirmed on `66b55f71`; tracked in #1199 under #1145/#912/#826. The stream
receive FORWARD TSN methods return early based on `StreamState.unordered`.
That flag selects local outgoing DATA; incoming DATA carries its own U bit.
With opposite endpoint send settings, the receiver ignores ordered progress or
unordered incomplete-fragment cleanup. Both paired red controls retain 2,841
bytes instead of the 24-byte reliable successor.

Remove both local-send-policy gates. The receive queue methods already operate
on their respective ordered and unordered collections. Paired controls retain
both reliable receive classes on one SID, lose one middle partial-reliability
fragment, wrap TSNs, and complete exact application reads after graceful close.
The final focused shutdown/FORWARD qualification passes 70 controls. Three old
report-construction fixtures now supply a real MTU instead of default zero.
This repairs a legal low-level SCTP history. It does not prove that negotiated
public data channels produced the historical #1120/#853/#889 failures.

### F176 — Native stream-reset batches exceed packet and chunk sizes

Confirmed on `66b55f71`; tracked in #1200 under #1145/#912/#826. The sender
collects every ready reset ID into one RECONFIG parameter. Seven closing
streams emit a 48-byte request at MTU 36. A 33,000-ID request also exceeds the
16-bit chunk-length field. Both boundary controls fail before the repair.

Limit each request to its padded packet and chunk budget. Preserve the queued
suffix and the existing one-outstanding-request rule. Paired controls retain
admitted old gameplay, lose the first request, require identical retry, complete
both reset directions, reuse every SID, and read exact fresh payloads. Controls
cover packet budgets 36, 64, 1191, and 1200; a separate large request checks
16-bit lengths and retained suffix ownership. MTUs below 36 cannot represent a
one-ID reset. That existing configuration limit remains open in #1201.

### A20 browser failure evidence — Candidate application failures

Open #853/#889 now retains a per-link ICE rejection count and the last buffered
or trickle error name in browser connection snapshots. Previously buffered
rejection left only a generic log, and later snapshots could not distinguish
candidate rejection from missing channel DATA. The retained state has fixed
size and copied snapshots; old pending failures cannot alter replacement links.
Controls retain original trickle rejection, buffered recovery, both SDP roles,
repeated failures, later successful ICE, and fresh-link recovery. Historical
incident causes remain unknown.

External-client assessment: Rust `da8f2d16` has no native RTC/SCTP dependency;
.NET `e467c871` has no package dependencies and uses its own mesh driver;
Godot `e5e20e0e` delegates WebRTC to the browser or engine extension. None imports
this repository's private patched SCTP or browser reference engine. These
repairs do not change signaling messages or public SDK contracts. No client
migration or client tracking issue is required. Published replacement
qualification in #912 remains open.

### F177 — Impossible native reset budgets change stream state

Confirmed on `78140b75`; tracked in #1201 under #912/#826. DATA packets
can use a 32-byte budget, but a padded outgoing reset with one SID needs
36 bytes. Local stop/close formerly changed stream halves before admitting
the reset. The #1200 sender then emitted a request beyond that smaller
budget. The red-first local control returned `Ok(())` at an effective
28-byte budget instead of rejecting it.

Admit a local reset before changing either stream half. Reject a smaller
budget with `ErrOutboundPacketTooLarge`. Apply stop before finish so close
also preserves both halves on failure. Deny incoming reset requests before
changing streams when a reciprocal request cannot fit; replay that denial
without changing the generation. This preserves DATA-only configurations;
it does not claim that INIT/INIT ACK cookies fit a DATA-only packet budget.

Controls cover direct payload settings 0, 1, and 4 through 7, both stop and
close, queued outbound gameplay, unread inbound gameplay, sequence and byte
state, and unchanged reset/event state. After a representable retry, paired
endpoints deliver exact old/tail values, complete reciprocal reset, and
release both SIDs. Incoming controls cover effective budgets 28, 29, 32,
and 35 with duplicate requests. Existing 36-byte and larger packet controls
retain lost-request retries and fresh-SID gameplay.

### F178 — Zero native fragment capacity consumes input and cannot progress

Confirmed on `78140b75`; tracked in #1203 under #912/#826. The direct
`EndpointConfig::max_payload_size(0)` path gives each stream zero fragment
capacity. A nonempty write allocates its message identity and consumes its
source before packetizing. `fragment_size` remains zero, so the remaining
byte count never decreases. A guarded-source red control fails at source
consumption before entering that loop; no live incident is claimed.

Reject nonempty writes with `ErrOutboundPacketTooLarge` before consuming
input or identity. Empty writes keep their existing no-op contract. Controls
cover all six public write adapters, the direct configuration constructor,
unchanged chunk arrays, message/stream sequences, queued bytes, and successful
positive-budget writes. Hosted replacement qualification now includes
`zero_fragment_budget` alongside reset/shutdown controls.

### A20 replay memory follow-up — Exact relay offset capacity

Open #775/#636: arbitrary replay order previously grew each sender/epoch
offset vector geometrically. The red resource control retains 5,376 offset
bytes for 1,001 sends against a 4,004-byte bound. Count each block before
allocating its index. Canonical contiguous sorted blocks still borrow raw
slices, compact blocks reserve exactly four bytes per send, and wide inputs
retain the pointer fallback. Independent ledgers cover 4,096 combinations
of senders, epochs, duplicate/gapped sequences, completion-time order, Unicode,
and integer limits. Raw events and exact replay semantics remain unchanged.

The 18 fixed-binary debug trials and executable hashes are recorded in
[the capacity ledger](arm-capacity-audit.md#exact-reordered-relay-offset-capacity-775).
At four million sends, relay capacity falls from 25,165,824 to 16,000,000 bytes;
median peak RSS falls from 297,384 to 292,348 KiB. Total debug probe time rises
from 26.954 to 34.491 seconds. This is a memory/work tradeoff, not a throughput
claim. The long live cell and whole-process bound remain open in #775.

External-client assessment: current Rust `da8f2d16` has no RTC/SCTP dependency;
.NET `e467c871` supplies its own peer networking; Godot `766f0fbf` uses browser
or engine WebRTC. None imports the private patched SCTP package or capacity
runner. These changes need no SDK migration or client issue. No signaling
schema, room rule, or server runtime default changes.

### F179 — Native custom DATA fragments exceed wire limits

Confirmed on `cda46c14`; tracked in #1205/#1206 under #912/#826.
A configured payload budget of 65520 lets a DATA chunk overflow its 16-bit
length during marshaling. A budget of five emits a 36-byte padded DATA packet
against its derived 33-byte packet budget. These are new custom-configuration
defects. Neither establishes the historical channel reset timeout's cause.

Packetization now limits each fragment by padded packet capacity and the
16-bit DATA length. It preserves the complete admitted message. Budgets below
four reject nonempty writes before consuming input or allocating identity.
The configured getter and normal default fragment size remain unchanged.
Paired histories drop the first DATA packet, recover by retransmission, close
gracefully, and compare complete original and successor bodies through final
application reads. All six public write adapters retain atomic refusal checks.
The existing tail-first fragmented-message stress retains 1024 fragments with
a representable four-byte payload budget.

### F180 — Browser relay admission omits the next frame

Confirmed on `cda46c14`; tracked in #1207 under #826. The browser reference
client checked only already buffered WebSocket bytes. A new public-run control
leaves one byte less room than the encoded relay frame. The old client still
sends it and emits `game_data_sent`, exceeding its documented local ceiling.

Admission now includes the next frame's UTF-8 or binary bytes. Exact remaining
capacity stays valid; a one-byte overrun returns the existing connection error
before sending or recording gameplay. Controls cover all four encodings,
ASCII and multibyte text, and before/exact/after boundaries. This is a new
local pacing repair, not an explanation of #853/#889.

### F181 — Native congestion arithmetic overflows during loss recovery

Confirmed on `cda46c14`; tracked in #1208 under #912/#826. The full-width
packet setting in F179 reaches timeout recovery after the first DATA loss.
The old congestion threshold calculation panics on `4 * mtu`. Fast recovery
uses the same formula, and congestion-window growth uses unchecked addition.
The repair saturates representable congestion values while preserving the
normal formula. Boundary controls and paired exact-payload loss recovery
qualify the repair. Normal WebRTC settings do not reach these numeric limits.

External-client assessment: Rust `da8f2d16` has no RTC/SCTP dependency;
.NET `e467c871` supplies its own peer networking; Godot's addon uses its own
engine/browser WebRTC integration. None imports these private SCTP sources,
the browser reference orchestrator, or the capacity runner. These changes
need no SDK migration or client issue and change no signaling contract.

### F182 — Native chunk-array admission uses the wrong message size

Confirmed on `8755be58`; tracked in #1210 under #826/#912. With a six-byte
message limit, an array of two six-byte buffers fails before its first write.
The successful adapter sends only one nonempty buffer, so the total array is
not the size of that message. The red control returns `ErrOutboundPacketTooLarge`.

The adapter now checks and sends the next nonempty buffer, then clears that
caller buffer only after success. Later buffers remain unchanged. Repeated
calls preserve exact bytes, separate ordered SSNs, and fragment boundaries.
Oversized next messages retain input, identity, and sequence state. Existing
zero-fragment, exhausted-identity, shutdown, and empty-write controls remain.
This affects the low-level SCTP API; the native WebRTC wrapper does not call
this array adapter. It does not explain a historical public-client failure.

### F183 — Late browser channel announcements escape explicit retirement

Confirmed on `8755be58`; tracked in #1211 under #826. A queued `datachannel`
event can hold the previous physical link after removal or replacement.
The old handler returns without closing the announced channel. A red control
requires the late channel to close and fails on the removed-link history.

The handler now closes that channel before returning. Removed and replaced
link controls require no handler registration, no application callbacks, and
no channel in the replacement link. The existing responder open-order control
also verifies late announcement closure. Existing duplicate-label, mode,
terminal-channel, and fresh-link recovery controls remain. This is explicit
resource retirement; it does not prove the cause of #853 or #889.

External-client assessment: Rust `da8f2d16` has no RTC/SCTP dependency;
.NET `e467c871` leaves peer networking to applications. Godot `908146fb`
ships its own addon rather than importing the reference browser engine or
these private SCTP sources. The signaling protocol is unchanged. None needs
migration or a client issue for these reference-stack changes.

### Native fragment allocation measurement (#636)

The packetizer knows its complete fragment count before building descriptors.
It now reserves that count once. The control checks exact bytes, fragment
boundaries, and descriptor capacity for payload budgets 4, 1200, and 65535,
with application sizes 1, 4, 1201, and 65536. The old vector reserves four
slots even for one descriptor; its first capacity assertion fails on main.

Three fresh debug-process trials per source packetize 1000 identical 65536-byte
messages. Descriptors are 128 bytes on this ARM64 host. At budget 1200, baseline
capacity totals 64000 slots and candidate capacity totals 55000 slots. Each
message saves 1152 descriptor bytes (14.06%) during packetization. The fragment
count remains 55. At budget 4, both retain 16384 slots per message, while the
candidate avoids intermediate vector growth. One-fragment messages reserve
one slot instead of four, saving 384 descriptor bytes during packetization.

| Budget | Baseline elapsed us | Candidate elapsed us |
| --- | --- | --- |
| 4 | 2646753, 2615529, 3777096 | 2397331, 2825369, 2267725 |
| 1200 | 8867, 9189, 12356 | 8075, 7871, 9424 |

These debug timings vary on a shared host. They do not establish gameplay
throughput or server capacity. Capacity and exact output controls establish
the allocation-shape improvement. Reproduce with the ignored
`packetization_allocation_profile` control in the SCTP library, in fresh
processes. Raw observations remain in ignored `progress/session-535/`.
The broader optimization and source-replacement campaigns remain open.

### F184 — Native chunk-cursor reads allocate the requested limit

Confirmed on `ecedac84`; tracked in #1213 under #826/#912. A seven-byte
fragmented message followed by `Chunks::next(usize::MAX)` panics with
`capacity overflow`. Partial prior reads retain the same defect. The argument
limits the returned bytes, but the old implementation reserves that limit
without checking the remaining message.

Each read now reserves only the bytes it can return. Its sizing pass visits
only the fragments needed for that read. Controls compare exact bytes across
zero and partial reads, empty fragments, fragment boundaries, exhaustion, and
full-width limits. All 29 reassembly controls pass locally. The native WebRTC
wrapper uses `to_payload`, so this is a public low-level read contract repair,
not evidence of a wrapper gameplay incident.

### F185 — Completed native channel reset rejects repeated close

Confirmed on `ecedac84`; tracked in #1214 under #826/#912. Public
`DataChannel::close` promises successful repeated cleanup. Once reciprocal
reset removes the core channel, the old wrapper returns `ErrDataChannelClosed`.
The real-wire red history fails after both endpoint handles report removal.

The wrapper now treats an absent core handle as already closed. Both roles
and both required labels cover repeated close before and after core removal
and peer cleanup. Binary/text sends and writable waits still refuse terminal
channels. Existing remote-close and fresh-gameplay controls pass. Logical
channel handles are allocated independently of reusable SCTP stream IDs, so
an old wrapper cannot close a replacement channel. This does not explain the
historical reset timeout in #1120.

### F186 — Browser MessagePack strings lose leading BOM characters

Confirmed on `ecedac84`; tracked in #1215 under #826. The MessagePack decoder
switches string decoding above 200 UTF-8 bytes. Its default `TextDecoder`
strips a leading U+FEFF, while its short-string decoder preserves it. Valid
root, array, and nested gameplay text therefore changes with string length.
The baseline control fails for a repeated-BOM v2 string at that boundary.

The browser keeps ordinary decoding as its type oracle. Only payloads that
contain BOM bytes receive a second supported raw-string decode. String values
then use a BOM-preserving decoder. Binary and extension values retain their
original types and existing rejection rules. Controls cover both protocol
versions, values around the size boundary, repeated BOMs, nested strings, and
multibyte text. The JSON signaling path remains unchanged.

External-client assessment for F184–F186: Rust `da8f2d16` has no RTC/SCTP
runtime dependency; .NET `e467c871` has no package dependencies and leaves peer
networking to applications. Godot `64969b97` uses its own addon and engine or
browser WebRTC. None imports the private native sources or browser reference
wire decoder changed here. No signaling schema, SDK API, or migration changes;
no client tracking issue is required for these reference-stack repairs.

### F187 — Native relay sockets retain Nagle buffering

Confirmed on `81c5db90`; tracked in #1217 under #826/#636. The native
reference client uses the default WebSocket connector. Its connected TCP
socket retains Nagle buffering even though accepted server sockets disable it.
The new real-socket control fails with `TCP_NODELAY=false` before repair.

The connector now disables Nagle before the handshake. The original absolute
connection deadline and default WebSocket configuration remain in place.
The green control verifies the socket option and exact small text/binary
frame exchange. All four scoped socket, deadline, and refusal controls pass.
This proves a socket pacing setting, not a measured latency improvement or
the cause of a historical interop failure.

External-client assessment: Rust `da8f2d16` already defaults to
`disable_nagle=true` in its independent WebSocket transport. .NET `e467c871`
uses its own transport and has no native-reference dependency. Godot
`b24443e3` wraps its engine's `WebSocketPeer`. None imports the changed
reference connector. No signaling contract or SDK migration changes.

### Compact capacity send-sort positions (#636/#775)

The capacity collector now stores each temporary send-sort position in four
bytes on 64-bit hosts. Equal sequence keys retain their original order through
an explicit original-position tie key. Larger-than-u32 event counts retain the
original full-width stable-sort fallback. Raw events and duplicate evidence
remain unchanged. This is separate from the earlier replay lookup repair.

The resource control fails on the old eight-byte position vector. All 26
scoped records controls pass after repair, including 4,096 differential
histories and exact collector/artifact roundtrips. The isolated send-sort
experiment and its limits are recorded in the
[capacity audit](arm-capacity-audit.md). Neither campaign is complete.

### A20 — Browser local ICE server error evidence (#853/#889)

The browser reference engine now retains a count and latest numeric code from
`icecandidateerror` events. The browser previously discarded these events.
The initial-negotiation failure control fails on the missing summary before
this change. It now verifies that error code 701 survives in the pre-teardown
diagnostic output. Repeated errors use constant storage; snapshots copy the
summary, and retired physical links cannot change replacement evidence.

The nonfatal-error control delivers exact gameplay after local ICE server
errors. An error from one STUN or TURN server must not close an otherwise
usable peer. No transport decisions change. This adds evidence for the
unresolved initial-establishment and three-player investigations; it does not
reproduce their historical failures or identify their cause. Browser tests and
type checks pass. Both issues remain open.

External-client assessment: Rust `da8f2d16`, .NET `e467c871`, and Godot
`b24443e3` do not import this reference engine or its diagnostic snapshots.
Their transport implementations remain independent. The added fields change
no signaling frame or SDK contract and need no client migration.

### A20 — Browser DTLS and SCTP transition evidence (#853/#889)

The browser reference engine previously retained only peer-connection, ICE,
and signaling transitions. DTLS and SCTP appeared only as final snapshot
values. A transport could pass through a failed state before the timeout
snapshot and lose that evidence.

The engine now observes DTLS and SCTP state events in its existing 32-entry
history. It attaches observers when description operations expose the
transports. The red-first control loses the transient DTLS failure on the
baseline. Controls cover both offerer roles, bounded history, independent
snapshots, one observer per transport, retired-link fencing, and fresh
application receipts. This changes diagnostic evidence, not transport decisions.
The full browser unit harness, strict type checks, formatting, and real
Chromium exchange pass. Chromium retains connected DTLS/SCTP transitions on
both endpoints while exact payload totals remain unchanged. The historical
causes in #853 and #889 remain unresolved.

### F188 — Empty native messages inflate received application bytes

Confirmed on `b8ae0549`; tracked in #1219 under #826/#912. The RTC
receive handler updates channel byte statistics before normalizing Empty
payload identifiers. RFC 8831 carries an empty message with one SCTP padding
byte. The application receives zero bytes, but the old receive counter adds
one. The real-pair baseline control fails with sender zero and receiver one.

The handler now records the normalized application length. Message counts
still include empty messages. The regression checks exact empty text and
binary receipts, multibyte text, nonempty binary data, both required labels,
and both offerer roles. It also checks final totals after reciprocal reset. The new live control and
the existing completed-reset control both pass in Nextest `29937882`.
This repairs traffic evidence; it does not establish gameplay loss or explain
the reset timeout in #1120.

External-client assessment for F188 and the new browser transition evidence:
Rust `da8f2d16` declares no RTC/SCTP runtime dependency; .NET `e467c871` has
no package dependencies and leaves peer networking to applications. Godot
`59a24685` uses its own addon and engine or browser WebRTC. None imports the
private native RTC source or browser reference engine changed here. No
signaling schema, SDK API, or client migration changes are required.

### Shared capacity room-change ordering (#636)

Capacity replay now groups room-change events and sorts each room once.
It preserves exact record identity and stable own-first timestamp ties for
every viewer. Nine focused controls pass, including 1,152 independent
old-sort comparisons. Shared-host measurements, all attempts, and limits
are recorded in the [capacity audit](arm-capacity-audit.md).
This reduces repeated analysis work; it does not close #775 or qualify a
server capacity point.

### A20 — Selected browser ICE route evidence (#853/#889)

The browser previously retained channel counters but discarded the selected
ICE route from the same stats report. It could not show whether the selected
route had received transport bytes when a gameplay receipt was missing. The
red-first control fails on the baseline because that evidence is absent.

The reference engine now records one transport-selected pair, its observed
state, nomination, exact byte counters, round-trip seconds, and local/remote
candidate types. It does not infer selection from a nominated or successful
pair. Missing references, ambiguous transport selections, and unsupported
fields remain explicit as null. The existing sample age and pending/failed
query state apply to this evidence. A failed refresh retains aged previous
values; a successful refresh with no route clears them. Snapshots copy scalars,
and a query from a retired physical link cannot change its replacement.

The browser unit harness and type checks pass. The three-player missing-label
controls verify the selected route survives into the final timeout output for
both roles and both labels. Hung queries preserve the existing exit deadline.
The real Chromium pair retains succeeded host/host routes on both endpoints,
with 2,468/2,596 transport bytes in opposite directions and 0.003 second RTT in
this observation. Exact multibyte application receipts remain 20/22 bytes per
required label. Transport bytes include protocol traffic and do not prove
application receipt. No historical failure cause or transport repair is claimed.
The two investigations stay open. The change affects diagnostic output only.

External-client assessment: Rust `da8f2d16` declares no concrete RTC runtime
and exposes a driver seam. .NET `e467c871` has no package dependencies and
leaves peer networking to applications. Godot `e4537c17` uses its own addon
and engine WebRTC classes. Current source checks confirm none imports the
browser reference engine or capacity collector. No signaling wire, SDK API,
transport decision, or migration changes are required for these clients.

### High-cardinality capacity collector memory (#775/#636)

The compact send-sort path now also handles high sender counts. The prior
fallback used stable full-event scratch. Nine sparse differential layouts,
complete artifact roundtrips, and all 27 focused records controls pass. Raw
sends and duplicate order remain exact. The [capacity audit](arm-capacity-audit.md)
records all measured cohorts, binary hashes, peak RSS reductions, and the
higher sorting cost. This advances the memory campaign without claiming a
complete process bound, a stall repair, or a server capacity point.

### Native receive storage ownership (#636/#912/#1145)

Session 540 starts from `61cbf41e`. The SCTP handler copied every complete
message into fresh storage, including an unfragmented message whose received
packet had no other owner. `Chunks::into_payload` now consumes that message
and tries to recover its mutable buffer. Shared and fragmented messages use
the existing bounded copy fallback. The borrowed `to_payload` API is unchanged.
Cursor reads within one fragment also share its storage; cross-fragment reads
keep the existing bounded copy. This does not change wire data or delivery modes.

The paired handler control fails with the old `to_payload` call: a 64-byte
received message does not retain the input packet's storage. It passes with
the consuming call. The history also drives acknowledgments for a 5,000-byte
fragmented message and checks exact delivered bytes after transport drop.
The graceful-shutdown control now holds the delivered mutable buffers through
terminal endpoint retirement, instead of copying them before that boundary.
Its normal and explicit-abort histories pass. These controls advance the
retained-data qualification in #1145; they do not establish a historical
public-client failure cause. Existing `reassembly_` and SCTP-handler selectors
in `run-webrtc-interop.sh` retain the new controls for #912 replacement.

Local debug probes retain counts and times, not a capacity or throughput claim.
For 65,536 unique messages of 64, 1,200, and 65,536 bytes, the consuming path
reuses all 65,536 input buffers; the borrowed path reuses none. Observed times
are 48/57/129 ms versus 65/76/318 ms. For 4,096 64-KiB cursor messages with
64/1,200/65,536-byte read limits, shared read counts rise from zero to
4,194,304/225,280/4,096. The reads preserve exact lengths and final bytes.
Runs used the warm ARM devcontainer and can include scheduling contention.
Reusing a fragment retains its packet allocation until the delivered buffer
is dropped; it does not promise lower retained payload memory.

External-client assessment: the Rust crate has no RTC dependency; .NET has no
native reference-client dependency; Godot's mesh uses its engine WebRTC classes.
None consumes these vendored reference-client sources or the capacity runner.
Server messages, defaults, ordering, relay, and reconnection rules are unchanged.
All three clients are unaffected and need no tracking change.

### Borrowed capacity artifact rows (#775/#636)

Session 540 removes owned event copies from `RunRecords::events`. Artifact
writing no longer increments shared-name counts for every send, receipt, and
gap, and it no longer clones the entire registry when creating the iterator.
The owned tagged event type remains the replay input. The all-event JSONL
control compares borrowed output byte-for-byte with owned serialization.
Replay controls retain missing, duplicate, misroute, order, and gap evidence.

The isolated probe uses 500,000 receipts and 15,984 registry entries. Both paths
write exactly 84,835,440 bytes. Initial debug observations show registry-stage
RSS growth of 2,748,416 bytes for the old iterator and 528,384 bytes for the
borrowed iterator. Whole-write times are 3.59 s and 4.85 s under concurrent
build work; this is mixed timing evidence, not a speed claim. The record vectors
still grow with offered work. This does not complete #775's ten-minute cell.

Three alternating optimized runs use the same probe and separate executables.
Only the event iterator and its matching test pattern differ in the old build.
The candidate's median iterator time is 0.829 ms versus 22.952 ms. Registry-stage
RSS growth is 69,632 bytes versus 2,355,200 bytes. Whole-write times overlap:
old 175/204/209 ms, candidate 165/170/245 ms (sorted). The candidate's lower
median is not a general export-throughput claim. Every run writes the same
84,835,440 bytes. The earlier mixed debug result remains recorded above.

Validation: 29 selected capacity records/replay controls, 46 SCTP
reassembly/graceful-shutdown controls, three RTC receive/retirement controls,
three live native receive/reset controls, and the real two-process native
WebRTC exchange pass. Root and native all-target/all-feature strict lint pass.
Markdown and doc consistency pass. The local CI-config wrapper retains its
excluded audit failure; no audit change was made.

### Native ordered receive credit and retained fragments (#1223/#636/#1145)

Session 541 starts from `3e30142f`. Ordered reassembly adds fragment bytes
before rejecting a retained duplicate TSN. A red-first control repeats one
byte and observes two accounted bytes. Reading or abandoning the message
subtracts only unique payload bytes. The repair credits only newly retained
fragments. Controls cover all six three-fragment arrival orders, TSN wrap,
repeated duplicates, a complete future message, wrong streams, stale messages,
exact reads, and FORWARD TSN abandonment. Association duplicate filtering may
exclude this history for ordinary peers. This is a low-level queue repair;
it does not establish a public gameplay incident or explain #1120.

Complete one-fragment ordered messages now retain one descriptor slot. The
baseline regression observes four slots. Incomplete messages keep the prior
growth path. A fresh-process debug probe retains 16,384 messages with exactly
1,048,576 payload bytes. Descriptor storage falls from 8,388,608 to 2,097,152
bytes. Sampled process peak RSS is 15,200 versus 8,992 KiB. This is a retained
storage observation, not a general receive-throughput or process-memory bound.

The paired graceful-shutdown history now also retains 1,024 small unread
messages per endpoint, plus its original fragmented message. Both ordinary
and simultaneous shutdown preserve exact message order and bytes, reject new
writes, keep routing until terminal reads, and retire each endpoint once.
This advances #1145's unread-tail qualification and #912's replacement contract.
Both broader issues remain open. Existing `reassembly_` and shutdown selectors
in the interop runner include these controls.

External-client assessment: Rust `da8f2d16` declares no RTC runtime dependency;
.NET `e467c871` has no package dependencies and supplies its own transport seam.
Godot `c6229ee2` uses Godot or browser WebRTC classes. Current source inspection
confirms none consumes these vendored native sources or the capacity runner.
Server messages, SDK interfaces, transport choices, and migration needs are
unchanged. All three clients are unaffected.

Local validation passes: 80 selected reassembly/shutdown histories, three live
native reset/loss controls, 29 source-policy controls, focused schedule/replay
checks, and live reconnect/room-replacement cells. Root and native strict
all-target/all-feature lint and SCTP production lint pass. Markdown and doc
consistency pass. The CI-config wrapper retains its excluded audit error.
An extra strict vendored-test lint pass finds the same 61 existing findings on
both main and the candidate; #1224 tracks that separate cleanup.

### Native zero-window retries behind retained DATA (#1227/#1145/#912)

Session 542 starts from `3c2fee74`. T3 retry selection permits a zero-window
probe only at `cumulative_ack + 1`. An abandoned or gap-acknowledged entry can
retain that position while later reliable DATA still needs delivery. Two new
regressions fail with the old selection condition. The repair uses the earliest
outstanding DATA, sharing the existing SACK timer lookup. It does not treat an
already retried but still outstanding predecessor as permission for another
probe.

The paired serialized packet history drops an obsolete partial-reliability
message and the ending fragment of a later reliable message. Its beginning
fragment fills the receiver. The receiver sends a real zero-window SACK with
a gap acknowledgment. T3 abandons the obsolete message. The old source emits
no live retry. The repaired source emits one, and the network delivers FORWARD
TSN before that probe while withholding newer cumulative progress. The
receiver then accepts the ending fragment and returns the exact application
bytes once. Established and graceful-shutdown histories cover ordinary and
wrapped TSNs. Shutdown retires both endpoints once after acknowledgment.
Additional controls cover a partial window, two live candidates, expiry during
retry selection, and repeated polls. This establishes a low-level paired SCTP
history. It does not establish the cause of #1120 or a historical public-client
incident. The broader #1145 and #912 campaigns remain open.

The final sweep reproduces the same retained-versus-outstanding mistake in
pending-send admission. An abandoned-only flight blocks its first unsent probe.
The pending path now shares the outstanding lookup. A new selection control
fails before that repair. A paired serialized history fills the receive
window, loses the window-update SACK after the application reads, and loses an
obsolete probe. When T3 abandons that probe, the old pending condition emits
no DATA. The repair admits one fragment of the waiting reliable message.
FORWARD TSN and the remaining fragment preserve exact reads and graceful
retirement. The controls cover TSN wrap, repeated polls, and both Established
and ShutdownPending. Other retained-flight emptiness checks intentionally
wait for peer acknowledgment before shutdown or timer retirement. Both pending
controls fail when only the old pending condition is restored. All 54 selected
shutdown controls and strict root/native lint pass with the complete repair.

### Capacity schedule iteration (#636)

Sequential schedule iteration now advances through each sorted churn boundary
once. The live writer peeks at that iterator and advances only after a recorded
completed send, so churn keeps the pending item. It retains constant iterator
state and no new per-send storage. Indexed access keeps its former rule. Unsorted synthetic churn histories use that rule
as a fallback. Every shift still uses the original intended time. Frozen eager
timelines and indexed comparisons cover jitter, phases, independent iterators,
repeated boundaries, and exhaustion.

An alternating local debug probe visits 720,000 sends and 26 churn boundaries.
All eight passes retain digest `260363167789419`. Indexed scans take
465,723–580,183 microseconds. Sequential iteration takes 43,595–52,047
microseconds. This measures generator schedule work, not server capacity or
release throughput. It does not explain the no-churn #1009 workload or complete
the ten-minute capacity qualification in #775.

External-client assessment: the current Rust manifest declares no RTC runtime
dependency. .NET `e467c871` has no package dependencies. Godot `c4bf6e01` uses
engine or browser WebRTC classes. None consumes the vendored reference-client
SCTP sources or the capacity runner. Server messages and SDK contracts are
unchanged. All three clients are unaffected and need no tracking change.

Local validation passes: 52 selected SCTP shutdown histories, eight recovery
controls, 14 capacity schedule/replay controls, and root and native strict
all-target/all-feature lint. The new paired control also fails when only the
old probe-selection condition is restored. The retained dependency audit
failure remains excluded from session delivery.

The live-writer iterator connection also passes 11 selected schedule, reconnect,
room-replacement, lag, pause, and replay controls plus strict root lint.

### Retained long-cell attempt (#775/#1009)

Fixed binaries from `3967a709` attempt the original one-room, 16-player cell:
60 sends/s per sender, 96 application bytes, seed 1, v3 JSON reliable delivery,
120 seconds of warm-up and 600 seconds of measurement, with the 250 ms lag
bound. No local builds or tests overlap the attempt. The shared ARM64 WSL2
host has 12 available CPUs and no cgroup CPU quota. It is not an isolated host.
The later live-writer iterator connection is absent from this fixed binary.
This no-churn cell does not exercise the churn-scan optimization.

Run `480649b4-5e01-4cc8-9eec-3aefc487368b` is invalid. It schedules 691,232
sends, completes 121,854, and retains 569,378 unsent. All 1,827,810 owed
deliveries arrive; outstanding and gap-covered totals are zero. Full artifact
replay reproduces the summary. The standalone test passes that equality;
it does not turn this invalid workload into capacity evidence.

The first recorded completed-write lag is 250,287 microseconds: intended
126,934,706, wake 127,160,287, preparation 251, and write 24,455. Later
before-write lag reaches 1,245,388 microseconds. The summary retains that
fault and a separate 364,371-microsecond completed-send maximum. The largest
sampler wake gap is 1,432,133 microseconds. Between neighboring resource
observations at 126,755,157 and 128,212,322 microseconds, server CPU rises by
12.4 seconds and generator CPU by 0.84 seconds. Generator RSS rises from
149,991,424 to 150,241,280 bytes. Worker runqueue-wait deltas range from
8.5 to 174.3 ms; the scheduler-enable setting is unavailable. These counters
do not identify a cause. The server CPU burst warrants a matched profile;
late fault-report work must also be distinguished from the initial overrun.

The 2,889 interval samples retain a peak generator RSS of 163,950,592 bytes.
The child-process peak is 174,156 KiB. The process finishes after 768.922
seconds, including its original receive deadline, export, and replay.
Artifacts remain at `/tmp/session542-live-long`; binary hashes, logs, selected
stall intervals, and the resource report remain in `progress/session-542/`.
This is a new partial-work memory observation. It does not bound the full
planned workload or complete #775 and #1009.

### F189 — Native receive acknowledgment gaps retain delivered payloads

Confirmed on `116c6240`; tracked in #1229 under #636/#1145/#912. The receive
acknowledgment queue clones accepted DATA, although it uses only the TSN.
A missing prefix on one stream therefore keeps later packet buffers alive
after another stream's application reads. It also prevents single-fragment
storage reuse. The new paired wire control fails on the old source after
independent receive buffers and packet ownership are released.

The repair retains TSN metadata without payload ownership. Sixteen messages
on another stream reuse their exact received storage while the first packet
is withheld. Exact duplicate TSNs, gap blocks, receive credit, final cumulative
progress, and application bytes survive ordinary and wrapped TSNs. Graceful
shutdown retires both endpoints once. Application buffers remain valid after
closure. A fresh-process debug cohort delivers and reads 16,384 messages of
1,024 bytes behind one withheld packet. All three baseline trials retain
16,777,216 receive payload bytes; all three candidates retain zero. Peak RSS
is 69,388–69,712 KiB before and 54,352–54,384 KiB after. This measures this
retained-storage workload, not general process memory or server capacity.
Existing deferred-generation reset controls qualify the other
receive-admission branch. This does not establish a historical timeout cause.

### F190 — Native released send bytes keep their backing allocation

Confirmed on `116c6240`; tracked in #1230 under #636/#1145/#912. Send payload
release uses `Bytes::clear`, which can retain the backing owner after its
logical length reaches zero. Selective acknowledgment and abandonment can
therefore report released bytes while a missing prefix still pins storage.
The owner-drop regression and paired fragmented-send control fail before repair.

The repair drops payload ownership while preserving acknowledgment, message,
and TSN metadata. Shared caller ownership remains valid. Repeated release
returns zero and drops no owner twice. A paired history holds the reliable
prefix, selectively acknowledges or abandons a later fragmented message, and
requires its owner to retire before cumulative progress. Exact reliable
successor bytes and graceful endpoint retirement survive ordinary and wrapped
TSNs. This qualifies the storage boundary; it is not a throughput result.

### F191 — Full-width native receive credit wraps after accepted DATA

Confirmed on `116c6240`; tracked in #1231 under #1145/#826/#912. Receive credit
narrows queue totals to `u32` before summing them. Multiple stream totals can
panic on overflow. One accepted message can cross the final positive receive
credit, making a full-width queue advertise free space again. The boundary
table panics on the old sum. The accepted-DATA history reports 4,294,929,591
bytes of free space instead of zero.

The repair subtracts each stream and deferred-reset owner with saturation
before narrowing the result. The table covers zero, ordinary, and full-width
limits. The large history admits 66,077 distinct ordered messages of 65,000
bytes through `handle_data`, across SSN wrap. Shared physical buffers keep
test memory small. Every accepted message remains exact through application
reads in ShutdownPending, and each read returns the exact remaining credit.
This is a low-level accepted-DATA history, not a 4 GiB socket experiment or
proof that a particular UDP route supports this configured workload.

External-client assessment for F189–F191: Rust `da8f2d16` has no RTC runtime
dependency. .NET `e467c871` has no package dependency on this stack and exposes
an independent transport seam. Godot `abb17511` uses engine or browser WebRTC
classes. Current manifest and mesh-source inspection confirms that none uses
these vendored native sources. Server wire messages, SDK APIs, and migration
needs are unchanged. All three independent clients are unaffected.

Local validation for F189–F191 passes: 108 selected SCTP shutdown, reset, and
payload-queue controls; all four receive-gap shutdown-role histories; four
real native reset/loss histories; and a live two-peer WebRTC exchange. Strict
root and native all-target/all-feature lint and SCTP production lint pass.
Markdown and documentation consistency pass. The CI-config wrapper retains
its excluded audit failure. The three new defects are separate from the
historical public-client timeout investigations, which remain open.

### F192 — Busy native reset responses erase retry limits and deadlines

Confirmed on `c6ee48b5`; tracked in #1233 under #826/#912. The reset response
handler stops and restarts its timer for both `InProgress` and a busy error.
Stopping clears the retry count and control backoff. Repeated busy responses
can therefore keep a stream reset pending beyond its configured retry limit.
Restarting also lets a duplicate response postpone an active or overdue timer.
The red regression with retry limit one remains Established after its final
allowed expiration. A separate red interval assertion observes one second
instead of the expected two-second backoff.

The repair preserves the busy request's retry count, backoff, and live deadline.
An idle busy timer can still start. `InProgress` keeps its separate restart and
no-error retry policy. Controls retain retry limits zero, one, and five, exact
request sequences, one terminal failure event, timer cleanup, and duplicates
before and after a deadline. A paired recovery history accepts the request
after two busy replies, completes both reset directions, reopens the SID, and
reads exact fresh gameplay. All 75 selected reset and shutdown controls pass;
the existing isolated storage profile stays ignored. Strict SCTP production
lint passes. Independent review found the duplicate-deadline defect in the
first candidate; the final source and regressions have no remaining findings.

This is a low-level accepted-response liveness defect. It does not establish
the cause of historical public-client reset timeout #1120 or complete the
published dependency replacement in #912. Server wire messages and SDK APIs
are unchanged. The independent Rust, .NET, and Godot clients do not import this
private vendored reference-client stack, so all three are unaffected and need
no tracking change.

### Filtered session metrics allocation (#636)

At `c6ee48b5`, application and completion-cursor queries clone the full session
registry before removing excluded records. A no-match application view allocates
6,336 bytes for 16 rooms and 595,456 bytes for 1,024 rooms in the release
allocation probe. The regression fails on that source. These are allocation
bytes per snapshot, not retained RSS or a server capacity measurement.

The view now checks both predicates before cloning. Active ordering, newest-first
completion ordering, strict cursor comparison, counts, global drop counters,
and response truncation remain unchanged. Unfiltered completed snapshots keep
exact preallocation. The probe compares no-match views with an empty registry's
fixed shard-guard overhead; one-match views may allocate only output-sized
storage. It covers active and completed registries at 16 and 1,024 rooms,
exhausted cursors, and unfiltered completed snapshots without reallocations.

The final release probe allocates 1,536 bytes for each no-match view at both
room counts, equal to empty-registry traversal. One-match active and completed
views allocate 2,676 bytes in both shapes. Unfiltered completed snapshots have
zero reallocations. The full relay allocation ceiling bench passes.

All 17 session-record controls and two endpoint truncation controls pass.
Independent source reviews report no remaining findings. Metrics JSON does not
change, so Rust, .NET, and Godot SDKs need no migration or source changes.

### F193 — Repeated shutdown ACKs postpone due recovery

Confirmed on `2347552d`; tracked in #1235 under #1145/#826/#912. Replaying
SHUTDOWN ACK starts T2 again even when its previous deadline is already due.
The red regression observes a fresh retry interval instead of the original deadline.
Repeated peer requests can therefore postpone configured retry exhaustion.

ACK replays now arm T2 only when idle. The first simultaneous shutdown
transition clears the old deadline, so it still restarts T2 as RFC 9260
section 9.2 requires, without clearing retry history. Controls cover ordinary
and simultaneous shutdown, due input before timeout processing, backoff,
configured exhaustion, and paired loss of SHUTDOWN and ACK. A separate control
named `shutdown_simultaneous_transition_retains_retry_history` spends retry limits
zero, one, and five before simultaneous shutdown and verifies that the transition
retains both backoff and exhaustion. The paired history
loses a replayed ACK before servicing the still-due timeout, then completes
both endpoint retirements. This is a low-level graceful association repair;
it does not explain historical reset timeout #1120. The public WebRTC close
path uses abort. External Rust, .NET, and Godot clients do not import these
private vendored reference-client sources and need no migration.

### Active session response materialization (#636)

The metrics endpoint used to construct JSON for every matching active record
before applying its 512-record response cap. It now selects the same oldest
prefix before serialization. Full library snapshots remain complete. Filters,
strict completed cursors, deterministic timestamp/room-ID ordering, pre-cap
counts, and global counters remain unchanged. The regression compares capped
views with complete snapshots across filters, timestamp ties, and empty views.
With 10,000 active fixture records, JSON materialization falls from 10,000
records and 5,160,140 serialized bytes to 512 records and 264,332 bytes.
These are intermediate JSON sizes, not allocation-byte or throughput results.
The registry snapshot still clones matching active records before selection;
this repair bounds JSON materialization, not all snapshot memory.

### Sparse replay sort scratch (#775/#636)

The compact send index's high-cardinality fallback used a stable offset sort.
It now uses an in-place unstable sort with the original row offset as a final
key. Duplicate sender/sequence records retain their exact original order.
Existing sparse-sender controls compare with an independent stable sort and
round-trip full delivery artifacts. Offered work, event budgets, raw records,
and oracle checks remain unchanged. This does not identify the historical
long-cell stall cause in #1009 or bound the largest planned capacity sweep.

Three alternating fresh-process debug pairs sort 1,000,000 sends from 100,000
senders. Baseline peaks are 90,708–90,792 KiB; candidate peaks are
88,860–89,104 KiB. Exact retained event identities pass in every process.
Candidate sorting takes 3.35–5.93 seconds versus 1.37–2.29 seconds for baseline.
The shared host and overlapping local checks limit timing attribution, but
these samples do not support a speed improvement. This is a memory tradeoff
for high-cardinality replay, not a live relay throughput improvement.

### F194 — Queued native shutdown work precedes final completion

Confirmed on `70f70d28`; tracked in #1237 under #1145/#826/#912. Both peers
start graceful shutdown. The remote receives the local SHUTDOWN and sends
its ACK. The local receives both that ACK and the remote SHUTDOWN before
its next flush. Pending ACK work takes priority over SHUTDOWN COMPLETE,
so the flush sends another ACK, rearms T2, and leaves the association live.
The red paired regression fails its first-flush terminal assertion. Both
inbound orders reproduce it. A separate paired control queues a T2 SHUTDOWN
retry before accepting the ACK and checks the same final-packet priority.

The writer now sends COMPLETE before either queued SHUTDOWN or ACK work.
Both paired controls verify the exact final chunk, terminal states, cleared
timers, one drain per endpoint, and no later transmit. Existing paired loss
recovery and configured retry-history controls pass. This changes low-level
graceful association behavior. The public WebRTC wrapper uses abort; no
public gameplay incident or cause of #1120 is established.

### Bounded active session snapshot storage (#636)

The capped metrics view previously cloned every matching active record,
selected the oldest prefix, and retained the original vector capacity. It
now keeps at most the requested number of owned records plus a bounded
heap of ordering keys. Each registry entry is examined under its existing
DashMap guard; the implementation does not hold guards between entries or
perform a second registry lookup. Full library snapshots remain complete.
Filtering, timestamp/room-ID order, cursors, and pre-cap counts stay unchanged.
The prefix comparison covers zero, one, 17, 512, and unlimited records with
application filters and timestamp ties. It now checks retained capacity too.

Two fresh-process pairs use 10,000 active rooms and the same 512-record JSON
response. The baseline reproduces the old clone-all-then-truncate snapshot.
Its record vector retains 16,384 slots (4,587,520 bytes); the candidate retains
512 slots (143,360 bytes). Candidate selection also retains 512 heap keys;
the record-vector figures exclude those keys, strings, and the registry.
Process peaks are 27,592–27,608 KiB for baseline and 25,308–25,528 KiB for the
candidate. JSON remains 264,332 bytes in every run. These are storage results,
not relay throughput measurements. Heap replacement can still copy more than
512 records over the scan; simultaneous retained snapshot storage is bounded.

### Churn evidence sort scratch (#775)

Churn evidence now sorts raw row positions in place. Raw position is the
final tie key in both passes, preserving the previous stable sort's exact
identity order. The retained two-word index stays the same size. This removes
stable-sort scratch without changing raw records, workload, budgets, replay,
or invalidation rules. A differential control covers empty and reversed
inputs, repeated timestamps, duplicate epochs, invalid epochs, and the full
epoch range. Existing delayed-cycle and missing/extra-event controls pass.

Two alternating fresh-process debug pairs sort one million fixture rows.
Baseline process peaks are 134,140–134,376 KiB; candidate peaks are
126,084–126,444 KiB. Retained indexes are 16,000,000 bytes in both paths.
Baseline sorting takes 7.53–14.39 seconds; candidate sorting takes
11.36–28.35 seconds. Local lint work overlapped the first pair. The second
pair still shows a debug sorting cost. This is a scratch-memory tradeoff,
not a speed improvement or a repair of #1009. The full planned long workload
and largest capacity sweep remain unqualified under #775.

External Rust, .NET, and Godot clients need no changes for these repairs.
Their current manifests or documentation confirm that they use their own
WebSocket or peer-transport implementations, not these private vendored
SCTP sources. Signal Fish protocol messages and metrics response content
remain unchanged; capacity ordering storage is internal tooling.

### F195 — Reset recovery aborts an acknowledged native shutdown

Confirmed on `97fc7e01`; tracked in #1239 under #1145/#826/#912. A paired
history delivers and acknowledges gameplay on stream 2, then leaves it unread.
The local endpoint requests reset on empty stream 1 and loses the RECONFIG
packet. Both endpoints then exchange graceful shutdown packets. After the
local endpoint accepts SHUTDOWN ACK, its reset timer remains armed. A driver
that services the saved deadline before the final transmit can exhaust reset
recovery, emit `AssociationFailed`, discard unread gameplay, and suppress
SHUTDOWN COMPLETE. The red regression fails at its no-failure assertion.

Accepting the terminal acknowledgment now stops all obsolete recovery timers.
Timeout callbacks also stop timers rearmed by accepted late control replies
while final completion is pending. A negative control that retains the initial
cleanup but removes this callback fence fails on a due late busy reset reply.
These extra late-response cells test accepted input; strict-peer reachability
of their ordering is not established.
The final transmit still owns graceful closure and endpoint drain waits for
retained reads. The paired regression covers reset retry limits 0, 1, and 5
with ordinary and simultaneous shutdown. It retains exact unread bytes,
final-packet identity, terminal state, timer cleanup, and one endpoint drain.
This is a low-level graceful association repair. The public WebRTC wrapper
uses abort; this does not establish a public gameplay incident or explain
historical reset timeout #1120.

### Metrics JSON byte-count scratch storage (#636)

Metrics response, snapshot, session, and room-lookup size checks previously
serialized each JSON value into a temporary byte vector. They now reuse the
existing canonical JSON counting writer. The same serializer counts UTF-8,
escapes, separators, and numbers without retaining its output. Caps, response
content, truncation order, and omission counters remain unchanged. The
existing byte-equivalence and serialization-failure controls pass alongside
all focused metrics cap boundaries.

Two alternating fresh-process debug pairs each size the same 720-sample JSON
history four times. Every arm reports 47,205,971 bytes. Baseline process peaks
are 103,436–103,624 KiB; candidate peaks are 57,704–57,832 KiB. Baseline elapsed
time is 9.702–16.931 seconds and candidate time is 8.861–9.973 seconds, with
concurrent local builds. These results establish scratch-memory reduction for
this fixture, not a speed or live relay throughput improvement. Building the
JSON value and the final HTTP response still allocates.

### Sparse arrival violation state (#775)

The capacity oracle now allocates violation counters only when a stream first
shows a duplicate, an order violation, or a misroute. Each counter uses its
count to distinguish an empty value from its first sequence. Sequence zero
and the full `u64` range remain exact. Valid streams retain no violation box.
The arrival value shrinks from 216 to 152 bytes on this ARM64 host; an invalid
stream adds one 48-byte box. Raw events, category order, summary fields,
workload budgets, and replay rules stay the same.

Fresh debug processes compare the same retained baseline and candidate
binaries in baseline/candidate/candidate/baseline order. The probe retains
240 streams per room, each with 100 ordered arrivals. A second fixture adds
all three violation classes to every stream. At 1, 64, and 999 rooms, both
binaries retain the same unique counts. The 999-room fixture retains 239,760
streams and 23,976,000 unique arrivals, or 24,215,760 in the invalid fixture.

| Rooms | Fixture | Baseline peak KiB | Candidate peak KiB |
| --- | --- | --- | --- |
| 1 | Ordered | 9,516–9,628 | 9,716 |
| 64 | Ordered | 15,388–15,520 | 13,752–13,936 |
| 999 | Ordered | 106,904–107,168 | 79,672–79,912 |
| 1 | All violation classes | 9,576–9,780 | 9,640–9,844 |
| 64 | All violation classes | 15,468–15,904 | 15,032 |
| 999 | All violation classes | 107,216–107,296 | 94,736–94,776 |

The largest fixture's current RSS falls from about 108.8 to 80.8 MB for
ordered streams and from about 109.0 to 96.3 MB for invalid streams. Small
process differences remain visible in the one-room fixture. Timing ranges
overlap; this establishes lower retained memory, not a speed improvement.
The probe measures oracle state only, not full raw-event retention or live
server capacity. It does not qualify the planned ten-minute cell under #775.

Controls preserve the independent 4,096-sequence duplicate/order scan, empty
and zero/full-width first keys, roster-first category ordering after churn,
opaque leaks, misroutes, and exact raw/JSONL replay verdicts. External Rust,
.NET, and Godot clients need no change: this is internal capacity-tool storage
and does not change the protocol or server behavior.

### Godot-profile binary packet ownership (#1240)

On `c5272199`, the native Engine converts each channel message with lossy
UTF-8 decoding. A real reliable packet `[0, 255, 128, 1, 192, 175, 0, 254]`
therefore produces 18 text bytes instead of the eight offered bytes. The
red regression compares the received bytes and fails. The channel-layout
profile alone cannot preserve arbitrary Godot gameplay packets.

The opt-in Godot profile now publishes binary frames through
`EngineEvent::ChannelBinaryMessage`. The event owns exact bytes, the original
arrival time, and the shared receive permit. The CLI emits standard-base64
`data_base64` in a `channel_binary_message` event. Binary frames do not count
as reference text exchange probes. Text frames and the default reference
profile keep their prior event contract. Exhaustive Rust event matches must
handle the added variant.

Native-stack controls cover both send directions, empty packets, every byte
value, malformed UTF-8, a full 64 KiB packet, exact permit charge and release,
and text traffic after binary traffic. Mixed binary controls cover all three
negotiated channels, both close roles, required-channel loss, held callback
retirement, and fresh-link recovery. Lifetime-limited channels may lose packets;
every receipt must match an exact offered packet and preserve its ordering mode.
One local control history observes zero receipts from an eight-packet unreliable
burst. Its actual loss cause is unproved. The previous per-burst positive
requirement did not allow the mode's full loss contract. The control now retains
all 576 offered packets and all burst counts, including zero. It requires
positive delivery in each direction and mode, separately before and after
replacement, across 12 cells of 48 offers. Every reliable burst must still
deliver all eight packets. No history is retried or omitted.
The regression does not prove Godot multiplayer framing, native or Web Godot
application compatibility, full service sessions, or remote-network behavior.
Those remain open in #1240 and Godot #355/#433.

Rust and .NET external SDKs need no changes: their current READMEs assign peer
networking to the application and do not use this reference Engine. Existing
Godot tracking remains necessary. The server protocol is unchanged.

### F196 — Native startup DATA precedes negotiated channel initialization

At `18cadb5d`, SCTP publishes `Event::Connected` through the event queue and
consumed DATA through the read queue. A read sweep can reach a pre-created
negotiated channel before its connection event initializes the channel.
`DataChannelHandler` returns `ErrDataChannelNotExisted`; the pipeline logs the
error and continues. The acknowledged payload no longer exists in SCTP.
A later event poll opens the channel but cannot restore the reliable prefix.

The paired-wire regression
`negotiated_startup_burst_preserves_every_reliable_prefix_before_event_poll`
creates negotiated SID 1 and sends 32 exact binary packets around COOKIE-ACK.
It pumps reads after each datagram and defers event polling through the burst.
The base fails at sequence 0 with COOKIE-ACK first. The repair carries the
association connection through the ordered read queue before following DATA.
Both COOKIE-ACK-first and DATA-first controls preserve all bytes, order, one
open notification, and no extra receipts. All 34 scoped SCTP handler controls
pass. The existing early-DCEP control now requires the connection marker
before the DCEP packet.

The public Engine control sends 128 binary snapshot packets of 1,024 bytes
as soon as each reliable channel opens. Both directions require the complete
prefix in the initial generation and after physical-link replacement.
No application-ready barrier delays those sends. This supplements the existing
mixed negotiated-channel, required-channel closure, stale-callback, and binary
ownership controls. The repair changes no public API or server wire message.
Track the confirmed defect in #1250 and retain these controls for #912.

The captured Godot 4.3 incident in #1244 has the same missing-prefix symptom,
but that does not prove a common cause. Matched Godot runtime comparisons and
service/network qualification remain separate evidence. The Rust and .NET
SDKs own signaling and relay, not this native peer stack. Their current
READMEs assign peer networking to the application. Godot #355 tracks the
reference Engine pairing and probe migration.

The matched Godot 4.3 control uses official Linux ARM64 Godot 4.3,
`webrtc-native` 1.2.2-stable, Chromium 154, and the portable Godot driver at
`1a23f8fb28b9583507bab6538fa1306cf66a1d80`. Each treatment runs in
baseline/candidate/candidate/baseline order with both offer roles. One
cohort adds no busy workers; the other adds four CPU workers on the 12-CPU
host. All 16 pairings pass both physical generations. They preserve all
8,192 offered reliable snapshot receipts across directions and generations,
plus the existing exact mixed-mode and required-channel retirement checks.
Godot 4.7.2 also passes both native offer-role pairings on the candidate.
All attempts remain retained. These shared-host controls do not reproduce issue #1244.
They do not exclude its original load pattern or establish its cause.

Both native probe builds use the portable source's unchanged send and wait
rules. Its event match adds `ChannelBinaryMessage`, converts the drill's
known UTF-8 payloads with checked `String::from_utf8`, and fails on receive
rejection. This is required after the existing binary-event migration;
ignoring the new event would invalidate the comparison. The exact opaque
binary ownership checks remain separate native tests. Probe SHA-256 values:

- Main: `44cbcfe1e1c93da13df88b1af4efae6bb1350a4c83d89cbfecc5918661dc8840`.
- Candidate: `bcee53a01d0d35b01adabd59fb4b766fe294eee58697397642b42f35bbd0aaf4`.

The candidate also passes a real v3 service rejoin against Godot 4.7.2 Web
with the native Engine offering. Both seats join the same service room and
exchange Offer, Answer, ICE, and transport status through the service.
The HTTP coordinator rejects signaling payloads; it records zero such
attempts. Phase 1 leaves the room while retaining a selected packet and
retires the old receiver. Both seats rejoin the same room with a fresh plan
generation and reference epoch 2. Each phase receives and acknowledges all
128 snapshot chunks in each direction. Exact mixed-mode delivery, required
channel closure, retained-packet retirement, and final room leave pass.

This advances #1240's service boundary for this one local two-seat cell.
It does not qualify other offer roles, native Godot service sessions, larger
meshes, remote networks, or finite lifetimes on repaired Godot engines.
The portable probe still requires its explicit binary-event migration in
Godot #355. No service behavior or client API changes in this control.

### Borrowed capacity stream boundaries (#775, #636)

The oracle previously copied each stream's fixed churn closure and snapshot
floor into `ArrivalEvidence`. It now borrows those values from immutable
churn views for the duration of summary evaluation. Invalid streams still
have no valid contract. Zero and full-width coordinates retain their exact
meaning. No per-stream allocation replaces the copied values.

On ARM64, the arrival record falls from 112 to 96 bytes. All 40 scoped oracle
controls pass, including independent sequence scans, churn boundaries,
malformed senders, loss coverage, and exact artifact replay. The profiling
fixture and workload are unchanged. The capacity audit records matched
retained-memory observations and their limits. This does not establish a
whole-generator memory bound or qualify the planned ten-minute workload.
External client APIs and server behavior do not change.

### Capacity receipt coordinates beyond the recorded stream (#1252)

A raw-arrival control on base `67725d67` finds that completeness accounting
uses every observed sequence as its upper boundary. A receipt beyond the
sender's recorded tail invents missing positions. An opaque receipt at
`u64::MAX` can overflow the aggregate missing count and stop report generation.
This is oracle and artifact-replay correctness. It does not prove live relay loss.

Completeness now intersects coverage with `(adopted floor, recorded tail]`.
The full observed ranges remain available for duplicate, order, and gap-overlap
checks. Invalid receipts keep their original bytes, sequence, and first-offender
classification. They cannot extend the delivery obligation. Two new controls
fail with the former unbounded accounting: one checks regular deliveries in
all three classes; the other checks opaque receipts and raw first-arrival order.
Both round-trip artifact records and require exact summary equality after replay.
A position-set control checks 6,400 floor/ceiling windows, plus full-width limits.

First rejected-arrival coordinates also move into the existing rejection
accumulator. Valid streams no longer retain that unused field. The ARM64
arrival record falls from 96 to 88 bytes. This storage change does not change
raw evidence or the number of offered events. The capacity audit records
component memory measurements; whole-generator and long-cell qualification
remain open under #775 and #636.

### Native RTC and callback receipt observations (#1244)

`Engine::transport_diagnostic_snapshot` adds RTC channel identities, states,
and message/byte totals to the local callback counts for one physical generation.
RTC receive counts advance after message reassembly and before Engine polling.
They are not raw SCTP packet counters. Samples across layers are not atomic.
Retained closed channels keep their own identities; labels alone are not keys.

A live control holds the Engine's channel pollers, sends four exact binary
packets, and requires RTC to record all four while Engine raw/admitted counts
remain zero. Resuming the reliable poller must deliver the exact prefix in order
and bring both callback counts to four. Both receiver roles and two physical
generations run this history. Removed peers return null. Fresh-link counters
must start empty. This adds evidence for locating future #1244 reproductions;
it does not establish the historical missing-prefix cause or repair it.

The Godot 4.7.2 retirement matrix uses official Linux ARM64 Godot, Chromium
154, `webrtc-native` 1.2.2-stable, and the portable driver at
`1a23f8fb28b9583507bab6538fa1306cf66a1d80`. It runs native and Web Godot,
both offer roles, and each of `reliable`, `ordered`, and `unreliable` closed
from either seat. All 24 cases pass both physical generations. Every phase
requires 128 exact reliable snapshot chunks in each direction, mixed gameplay,
selected-packet retirement, required-channel closure, and fresh-link recovery.
All 12,288 reliable snapshot receipts pass. The 48 retained native RTC snapshots
cover 144 channel comparisons; RTC received-message and Engine raw counts agree
at these teardown observations. This does not make live samples atomic.

The probe's snapshot operation awaits the new diagnostic method. It preserves
the existing checked binary-event handling and exact gameplay assertions.
Its SHA-256 is `9673048737abf952d239537e32efe559b3f0ed2186cbda8d5c50c164ae0942a6`.
Reproduce with the portable driver's `--native-reference-probe`,
`--safe-receive --mixed-traffic --retirement-matrix --require-gameplay` options,
using both offer roles and both runtimes. Point `XDG_DATA_HOME` at the installed
Godot export templates for Web exports. The probe's known UTF-8 drill packets
do not replace the native opaque-binary ownership controls.

All attempts remain retained. An initial shared-filesystem cohort passes two
cases, then records a phase-2 teardown timeout and a separate 120-second editor
import timeout. The local-output cohort passes all 12 native cases. Its first
12 Web attempts stop before gameplay because the template-directory environment
was omitted; the corrected Web-only cohort passes all 12 cases. No reliable
receipt assertion or deadline changes. The passing cohort does not explain the
earlier teardown timeout or the historical Godot 4.3 prefix loss in #1244.
Full service coverage, larger meshes, remote networks, and repaired Godot
packet lifetimes remain open under #1240 and Godot #355/#433.

### Native packet ingress observations (#1244)

The diagnostic snapshot now includes transport IDs, ICE/DTLS states, and
sent/received packet and byte counts from the same RTC stats report as the
channel counters. The counters advance at the raw transport demultiplexer.
They include control traffic and protocol overhead. They do not prove SCTP
DATA acceptance, message completion, or application delivery. Compare changes
within one physical generation, then inspect channel and callback counts.
Transport stats IDs are scoped to that generation and can repeat on a new peer.

The held-poller control fails on the former snapshot because it lacks transport
observations. The new snapshot records packet and byte growth while all four
exact 1,024-byte binary packets reach channel reassembly and callbacks remain
held. Resuming polling delivers the exact reliable prefix. Both receiver roles
and both physical generations pass, including unknown/removed null snapshots,
initial empty channel counters, stable transport identity within a generation,
and distinct generation/transport identities after replacement.

One initial test incorrectly required the RTC stats ID alone to change across
peer replacement. RTC uses the report-local ID `RTCTransport`. The corrected
control compares the physical generation and stats ID together. This is a test
assumption correction, not an RTC transport identity defect. Both failed
attempts remain retained. All five scoped native profile controls pass.

The snapshot change is additive. Existing callback and channel fields remain.
Rust and .NET SDKs delegate peer networking to applications and do not use this
Engine. Godot #355 tracks optional probe adoption. The server protocol does not
change. Historical Godot 4.3 missing-prefix cause #1244 remains open.

### Native Godot service rejoin and delayed consumption (#1240, #1244)

The portable Godot driver at `1a23f8fb28b9583507bab6538fa1306cf66a1d80`
now has a local native-runtime service control. The only driver edit removes
`args.runtime != "web" or` from its service CLI restriction. Reference-offer,
service identity, signaling, room-leave, and rejoin verdict rules remain.
The server route is `/v2/ws`. The initial `/ws` attempt fails before login;
its evidence remains retained. No gameplay conclusion follows from it.

Godot 4.7.2 native completes two service generations with the native Engine
offering. Both seats join the same service room. The reference leaves with a
selected packet, then rejoins at epoch 2 with a new plan and physical peer.
Both phases preserve exact mixed gameplay, all 128 reliable chunks in each
direction, retired old input, required-channel closure, and final room leave.
The HTTP coordinator records zero signaling payload attempts. A second cohort
also passes native and Web Godot service rejoin with a 1.5-second reference
consumption pause and credit sent over WebRTC. These are local two-seat controls.

The final probe includes transport packet observations. Its SHA-256 is
`0ab096160269253ca0979a6118cff2969c392d5453eb9720b45bfe5ee5355960`.
Four final pairings run concurrently on the Linux ARM64 host: native and Web
Godot 4.7.2 service rejoin with the reference offering, plus native Godot 4.3
with both offer roles and four added CPU workers. All use `--safe-receive
--mixed-traffic --slow-reference-receiver --credit-transport webrtc
--require-gameplay`. The native extension is `webrtc-native` 1.2.2-stable.
Web uses Chromium 154. Native snapshots retain all three channel contracts
and transport packet/byte counts in both physical generations.

All four final histories preserve all 2,048 reliable snapshot receipts across
directions and generations. Exact mixed-mode predicates, complete heartbeat
tails, selected-packet retirement, and required-channel closure pass. Both
service histories also pass room and signaling qualification. Two stronger
pause checks fail: phase 1 of Godot 4.3 with the native Engine offering,
and both phases of Web Godot service rejoin. Their final overall gameplay verdicts stay false.
At pause/resume, sent/echoed heartbeat counts are 2/0 then 15/2, and 3/0 then
17/2. Web phase 2 records 1/0 then 14/1. None proves a new round trip
during its pause. All later heartbeats arrive. A passing earlier cohort does not repair or explain these failures.

[Godot #440](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/440)
tracks command-bridge, native-send, Godot-echo, and callback timing needed to
locate that delay. All attempts and full verdicts remain retained in ignored
`progress/session-552/`. No deadline or receipt rule changes. The reliable
prefix loss in #1244 is not reproduced. Other service offer roles, larger
meshes, remote networks, and corrected Godot packet lifetimes remain open in
issue #1240 and Godot #355/#433. Rust and .NET SDKs do not own this peer stack.

### Native SCTP ingress observations (#1244)

The diagnostic boundary now includes existing association DATA/SACK counters,
local retransmission and delayed-acknowledgment timeouts, and fast retransmissions.
`RTCSctpTransport::association_stats` reads the current association without
adding per-packet state. The async wrapper uses its existing core lock.
`Engine::transport_diagnostic_snapshot` exposes the result as `sctp`.
An absent association returns null; a sampling error also sets `sctp_error`.
Terminal associations can already be removed, so sample before teardown.

DATA counters advance before message reassembly. They include duplicates and
refused chunks, do not identify a channel, and do not prove DATA acceptance.
RTC channel counts still start after reassembly. Engine counts start when its
poller sees a message. Samples across these layers are not atomic.

The native control holds callback polling while four 4 KiB reliable packets
arrive. SCTP records more than four chunks, RTC records exactly four messages
and 16,384 bytes, and Engine records zero receipts. Resuming polling delivers
every original byte in order. Both offer roles repeat on fresh physical links.
Distinct-counter and absent-association core controls pass. The five native
profile controls also pass with fragmented reliable startup and recovery traffic.
This is diagnostic progress. It does not explain or repair the historical
Godot 4.3 reliable-prefix loss.

### Fragmented Godot gameplay and Web service buffer loss (#1240)

Godot client `988797b4` supplies the local probe and driver. Only snapshot and
partial bodies change from `repeat(64)` to `repeat(256)` in the JS `chunk`
function and GDScript `_reference_chunk`. Each body has 4,096 bytes plus its
prefix. Startup packets, 128 snapshot chunks, 32-packet credits, mixed modes,
closure, retirement, two generations, and existing deadlines stay unchanged.
The probe samples SCTP diagnostics while processing snapshot chunk 127.
Its SHA-256 is `289ab87dd0e0d1cca496a8af80d4a2dfacac61a6e110a0954bcb84e25cea60a8`.

All six final Engine-level cases pass: Godot 4.7.2 native/Web and Godot 4.3
native, each offering and answering. They retain 3,072 exact reliable snapshot
receipts. Active SCTP samples record 560–579 DATA chunks per generation,
including other channel traffic. RTC reliable counts include credit messages.
These counts are observations, not proof of a historical loss repair.

The native Godot 4.7.2 service control also passes fragmented mixed gameplay,
room leave/rejoin, epoch-2 recovery, selected-packet retirement, and closure.
The Web service control fails in phase 1. Godot logs nine channel-buffer-full
drops, then rejects a snapshot packet after 111 exact receipts. The native
reference receives all 128 Godot chunks. Its bridge schedules 128 outgoing
chunks, while its last observed credit acknowledges 104. Scheduling does not
prove each write completed. A matched Web service control with the original
1 KiB body passes both generations and all service predicates.

[Godot #442](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/442)
tracks the buffer capacity, exact missing sequence, native admission, and
receiver-drain investigation. The failed 4 KiB service verdict remains false.
No receipt requirement, credit window, or deadline was relaxed. This leaves
full Godot support open in #1240 and Godot #355. Remote networks, larger
meshes, and corrected Godot lifetimes remain unqualified. Rust and .NET SDKs
have no dependency on this patched peer stack; their wire contracts do not change.

All attempts remain in ignored `progress/session-553/`. Earlier attempts used
an incompatible cached bridge, lacked the export-template path, or exhausted
the temporary filesystem before service gameplay. Those setup failures remain
separate from the retained Web buffer-loss history.

### Classify native SCTP DATA admission (#1244)

The native transport snapshot now splits DATA attempts into accepted, redundant,
and refused chunks. Acceptance records entry into the receive TSN space. It
includes saved reset DATA and DATA acknowledged for stopped readers during
graceful drain. Redundant TSNs include received and FORWARD-TSN-covered positions.
Refused chunks retain the existing retry behavior. Counters belong to one physical
association; they neither identify channels nor prove reassembly or delivery.
The receive path adds no allocation.

Three deterministic controls cover ordinary and wrapped TSNs, duplicate DATA,
full receive windows, exact reads, saved reset queues, counter reset, the
full-window fragment-completion exception, and stopped-reader drain. The live
held-poller control retains four fragmented 4 KiB packets in both receiver roles
and both generations. All chunks are classified before callback admission resumes.
RTC mapping/absence controls retain the existing null semantics.

### Godot-offer service sessions (#1240, #1244)

The [pinned fixture](../../clients/native/tests/fixtures/README.md) applies to
Godot client `988797b4c2f2a870b946870b59046ea5a5f1a297`. The control elects the
reference as authority/host so Godot offers through the real v3 service.
Its portable patch keeps early service signals until native pairing completes,
counts signals from phase entry, and freezes the gameplay generation before
teardown can publish another plan. Early attempts retain authority-selection,
pre-pair signal, and teardown-evidence failures. These are harness failures;
they do not establish production packet loss.

Godot 4.7.2 native and Web use Chromium 154 on Linux ARM64, with native
webrtc-native 1.2.2-stable. Both offer-role service histories pass two generations
in fresh rooms. Each direction receives and acknowledges all 128 reliable chunks
per generation: 1,024 exact snapshot receipts across the two runtimes. Mixed
reliable/ordered/unordered traffic, selected-packet retirement, required-channel
closure, and fresh links pass. Both sides record the same room and plan generation;
Godot records initiate=true and the reference initiate=false in every phase.
HTTP carries markers only. Snapshot bodies retain their original 1 KiB size,
32-packet outstanding window, receipt requirements, and deadlines.

At snapshot chunk 127, four native SCTP samples classify 182, 176, 187, and 189
received chunks as accepted, with zero redundant or refused chunks. These totals
include all channel modes and credits. They cannot attribute the historical
Godot 4.3 missing prefix. The probe SHA-256 is
`777bc6c066e799001732d97021dea81f11569c0848de10c1257cccec040222fd`.

Earlier 4 KiB/8-outstanding diagnostic attempts remain separate. One intermediate
cohort also inherited an 8-packet Godot window before the final 32-packet fixture
was frozen; it is not the source of the final original-workload qualification.
All attempts remain in ignored `progress/session-554/`.

Godot-offer host departure/rejoin, larger meshes, remote networks, corrected
Godot lifetimes, and historical-prefix attribution remain open in #1240/#1244
and Godot #355/#433.
[Godot #444](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/444)
tracks the general driver and host-departure control. Rust at `da8f2d16` and .NET at `e467c871` have no dependency
on the patched peer stack in their inspected manifests. Their wire contracts do
not change. Godot's optional reference probe can consume the added diagnostics;
its production adapter keeps the existing negotiated channel contract.

### Godot service recovery through observed UDP loss (#1240, #1244)

The new `clients/native/tests/fixtures/webrtc_loss_control.py` wraps the merged
Godot driver at `f3d3f8611e2c85290f222074da6d40807099e4ec` (Godot PR #445).
It removes inline SDP candidates and selects one IPv4 UDP host candidate per seat.
Both advertised addresses point to a local relay. Web mDNS endpoints are learned
from their first UDP packet. Each physical generation gets fresh sockets and a
fresh loss budget. Selected candidates, omitted-candidate totals, endpoints,
packet counts and sizes, monotonic timestamps, and relay errors remain in `udp-loss.json`.

After native channel open, the relay drops one DTLS application datagram of at
least 1,000 bytes in each direction. It forwards smaller application packets,
STUN, and handshake traffic. The wrapper requires observed loss in both directions
and two distinct physical generations. The ordinary driver retains exact snapshot
bytes and sequences, final credits, mixed modes, selected-packet retirement,
required closure, same-room rejoin, and the original receipt deadlines.

Six final local pairings pass: official Godot 4.7.2 native/Web and Godot 4.3 native,
both initial offer roles, on Linux ARM64 with Chromium 156.0.8078.4.
All 3,072 required reliable snapshot receipts pass. The 12 physical
generations retain all 24 injected drops, each 1,101-1,141 UDP payload bytes.
At reliable snapshot 127, native SCTP samples report one or two fast
retransmissions per generation. Snapshot bodies remain 1 KiB, with 32 outstanding
chunks. Godot-offer host departure changes the second generation's offer role;
the merged driver retains that complementary role and epoch-2 evidence.

Five socket/verdict controls cover exact byte forwarding, small-packet exemption,
loss arming and budget, mDNS endpoint learning, inline-candidate removal, candidate
formats, fresh generations, and missing-loss rejection. Disabling injected loss
or dropping small application packets makes the focused control fail. A root
Rust test runs these controls in hosted CI. Three initial attempts retain fixture
candidate-format failures; a fourth small-packet loss control passes separately.
Those attempts do not establish gameplay loss. All attempts remain in ignored
`progress/session-555/`.

The probe SHA-256 is
`777bc6c066e799001732d97021dea81f11569c0848de10c1257cccec040222fd`.
The wrapper SHA-256 is
`4eb41776a92afe75398b9d590224a2de011d4d0033e1566b122fffd613c0920b`.
The fixture README gives the pinned build and run recipe.

This qualifies a local post-open loss boundary. The relay cannot identify an
encrypted SCTP chunk, and these passes do not attribute the historical Godot 4.3
missing prefix. Remote networks, larger meshes, and corrected Godot lifetimes
remain open under #1240/#1244 and Godot #355/#433. Rust and .NET SDK behavior and
all server messages remain unchanged. The Godot production adapter also stays
unchanged; its assessment issue tracks this optional reference control.

### Empty native message send capacity (#1260, #912, #1240)

Main `eb01c98f` admits empty text and binary packets without charging send capacity.
The new `empty_channel_sends_respect_backpressure_and_resume_exact_receipts`
control holds both real reactors. Its first empty binary send leaves outstanding
bytes at zero instead of one. `empty_message_credit_preserves_unreleased_successor_bytes`
also fails before repair. These are send-admission failures, not observed packet loss.
The existing #1095 application-byte release remains correct.

The repair charges at least one buffer byte per admitted message. SCTP preserves
three release totals: wire bytes, application bytes, and data-channel buffer bytes.
Buffer credit includes empty fillers and excludes DCEP. The wrapper checks the
padded length in `try_send` and `try_send_text`. Empty traffic still contributes
zero application bytes to sent and received statistics.

The public control covers both sender roles and both reference/Godot profiles,
with a held reactor in each of two physical generations. It checks empty binary
and text admission, full-buffer refusal, suspended sends and writable operations,
exact empty and nonempty receipts, final zero debt, closed-handle refusal, and
fresh-link gameplay. It uses fresh runtime fixtures because each pausable runtime
wraps one driver. The first candidate's second-generation fixture failure remains
retained in the local session record.

Paired SCTP acknowledgments preserve a queued nonempty successor after the empty
packet is acknowledged. Fragmented gap/duplicate acknowledgment and abandonment
controls check all three release totals. Timer-only expiry covers reliable,
zero-retry, and unsent timed empty and nonempty text/binary packets. These controls
are required when qualifying published replacements under #912. They do not
qualify a replacement release or explain the historical #1244 prefix loss.

Rust `da8f2d16` supplies its own WebRTC backend. .NET `e467c871` has no data-channel
implementation. Godot `92e3b545` uses separate Godot/browser channels. None requires
a wire or API migration. [Godot #355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355)
tracks the native reference-peer assessment. The new profile controls use two
native Engines; they do not establish new Godot runtime or remote-network results.

### Paired deferred-reset sequence progress during shutdown (#1184, #1145)

The new `graceful_shutdown_cross_stream_reset_reaches_forwarded_future_sequence`
control reaches the #1184 ordering through two real association packet histories.
One endpoint retains old unread data on stream 1. Its outgoing reset completes,
but the reciprocal incoming reset waits for that read. A stream 2 reset carries
the reciprocal request's response sequence and releases the sender's stream 1
reset fence. The sender then reuses stream 1, expires sequence 0, and transmits
FORWARD TSN before the old application read completes.

The receiver starts graceful shutdown, reads the exact old body, and accepts
reliable sequence 1. Both associations close before the final exact body is read.
The control requires write refusal, cleared reset markers, no remaining timers,
and one endpoint drain event per association. Removing the #1184 marker retention
leaves accepted sequence 1 unreadable after expired sequence 0. The final control
fails its successor-readability assertion before terminal closure.

The cross-stream acknowledgment matches
[RFC 6525 section 5.2.2](https://www.rfc-editor.org/rfc/rfc6525.html#section-5.2.2).
The application-unread reset delay is this implementation's behavior. This proves
paired implementation reachability; it does not establish independent-peer
conformance or explain the historical #1120 close timeout. Those limits keep #1184
open. The shipped WebRTC wrapper uses association abort, not this graceful API.

### Godot profile lifetime expiry beside reliable snapshots (#1240, #1244)

The new `godot_lifetime_expiry_releases_credit_without_losing_reliable_prefix`
control covers both offer roles and both finite-lifetime Godot labels. It holds
the receiver reactor and drops one encrypted DTLS application transmit. The
socket observation does not identify its SCTP chunks. The control requires an
observed retransmission timeout and release of a fragmented 4 KiB finite-lifetime
send without peer output. All 16 KiB of reliable prefix data stays outstanding until
the receiver resumes, then arrives as four exact ordered bodies.

A later reliable barrier arrives exactly. The finite channel stays open and
releases all credit. A fresh finite packet may arrive or expire under its 1 ms
lifetime; any observed body must match exactly, with no duplicate or stale body.
Disabling the Godot lifetime makes the no-peer-acknowledgment expiry check fail.

These are two native reference Engines. No new Godot runtime result, full service
qualification, or historical #1244 cause is claimed. Godot
[#355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355)
and [#433](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/433)
retain runtime assessment. Rust `da8f2d16` supplies a separate peer backend;
.NET `e467c871` has no peer-stack dependency; Godot `2cb9ac39` uses Godot/browser
channels. Server messages and SDK migration requirements remain unchanged.

### Retired reset sequence cleanup (#1263, #1184, #1145)

A new paired history exposes a terminal cleanup gap. A cross-stream reset leaves
old reliable gameplay unread. The sender starts a fragmented future-generation
message with a finite lifetime. The fixture drops complete packets containing
its later fragments, then delivers the actual timed FORWARD TSN. Both associations
complete graceful shutdown before the old reader drains.

The final old read or stop used to restore the expired future sequence marker
after retirement. The repair preserves markers only while the association is
live. The red control fails at the terminal marker assertion. Restored controls
retain exact old payload bytes, repeated EOF and stop behavior, refused writes
and stream creation, empty deferred storage and markers, stopped timers, and one
endpoint drain event each. Existing live-state and accepted-future-DATA controls
still pass.

This repairs the terminal-cleanup acceptance condition. It does not demonstrate
payload loss, an independent peer's conformance, or the cause of #1120. The public
WebRTC wrapper uses abort rather than this graceful association API.

### Godot-profile reliable recovery after transport queue saturation (#1240, #1244)

The new native control holds channel application pollers while two native Engines
continue their transport reactors. It sends 2,048 distinct 4 KiB reliable bodies,
including sequence and physical-generation stamps. Each cell requires observed
SCTP receive-credit refusal, at least 256 reassembled messages retained downstream,
an upstream tail, zero Engine raw receipts, and outstanding sender bytes.

The receiver then polls its public data channel directly. Every body must arrive
in exact order and all sender debt must clear. Both offer roles repeat after
removing the old physical peers and creating fresh links. The four cells preserve
8,192 packets, or 32 MiB. This exercises transport backpressure beyond the earlier
four-packet held-poller controls.

Direct channel polling does not qualify the Engine's separate application-event
admission queue. An earlier fixture that released this burst into that queue
encountered a terminal event without distinguishing receive rejection from channel
closure. That failed attempt is retained locally; #1264 tracks its unproven cause.
Direct polling separates transport retention from the 1 MiB admission budget.
No queue limit or deadline changed. These are two native Engines, not new Godot
runtime, full-service, or remote-network results. The historical missing-prefix
cause remains unknown.

External-client assessment: Rust main `da8f2d16` has no dependency on this SCTP
package. The .NET client project has no peer-stack dependency. Godot main
`a5728f62` uses Godot's native or browser peer channels. None requires a signaling
or API migration. Godot [#355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355)
retains reference-profile runtime qualification. The capacity collector changes
are local workload-tool behavior and do not change any client contract.

### Separate Godot binary receive-credit limits (#1264)

The initial 4 KiB fixture reaches both application-event limits together:
256 retained messages occupy 1 MiB. Admission reserves capacity immediately;
it does not wait for the consumer. An active consumer alone cannot guarantee
that a burst remains below those limits. This explains a possible refusal path,
not the cause of the retained ambiguous attempt.

The new Godot-profile control separates the binary path from the existing
Reference/text limit controls. It retains actual `EngineEvent` permits for
128 exact 8 KiB bodies to fill bytes only, or 256 exact 2 KiB bodies to fill
message slots only. The next body must produce `ChannelReceiveRejected`, with
exact raw, admitted, rejected, and discarded totals. A physical `ChannelClosed`
event is not accepted as that verdict. Failed byte admission must return its
reserved message slot, and releasing all retained events must restore both
budgets.

Fresh physical links use the same previously terminal Engine and budget owner.
Dropping exactly one retained receipt before the next send must admit that exact
successor, keep the channel open, and release all sender debt. Both offer roles
exercise both dimensions. This qualifies intentional binary admission and fresh
recovery. It does not classify the original combined terminal event or prove
Godot runtime or remote-network behavior. The unresolved attempt stays in #1264.

### Active Engine consumers after callback saturation (#1264, #1244, #1240)

A new control resumes production Engine callback polling while its application
consumer immediately checks and drops every receipt. Both offer roles send
2,048 distinct 4 KiB reliable packets. Before resume, stopped callback tasks must
produce observed SCTP receive-credit refusal with zero raw Engine receipts.
Each role repeats on a fresh physical generation in the same Engines. Packet
bytes include sequence and generation. A skipped prefix, unexplained closure,
or stalled tail fails the control.

The consumer must receive either all 2,048 exact packets with zero sender debt,
or the exact admitted prefix followed by one typed pending-message refusal.
The second result is terminal; all three required channels must refuse new work.
It is not a successful full-stream delivery. Every case checks raw, admitted,
rejected and discarded counts, released application permits, and fresh mixed
traffic after physical-link replacement. No receive budget, offered packet count,
or reliable receipt requirement changes.

An initial local attempt records one active-consumer refusal after 640 exact
packets. The producer samples 256 pending messages and 1 MiB at refusal. The
other three cases receive all 2,048 packets. Two later cohorts receive all
packets in all four cases. All attempts remain local under
`progress/session-562/`. The initial attempt does not have a complete retained
source manifest; it is supporting evidence, not a fixed-source causal comparison.
The final seven-test cohort also passes. Its four active cases receive every
packet. Source checksums, the complete diff, binary checksum, and all snapshots
are retained with that attempt. Its test binary SHA-256 is
`675d7d834705883ff5c75c935c34b5cabdb7a5474e61d9e12a3e375cc1fb046d`.
These results establish that active consumption can coincide with a concrete
budget refusal. They do not identify the original ambiguous event or explain
the historical Godot 4.3 prefix loss.

Receive failure evidence now travels with the Engine terminal event and the
native CLI's `channel_receive_rejected` JSONL event. It includes the physical
generation, label, first failed limit, raw message bytes and sampled Engine
backlog. The snapshot retains the same `receive_failure` value when application
permits drop. Removing a peer clears its snapshot but cannot change a retained
event. The counts are sampled separately after the failed reservation; they
are not an atomic occupancy history. The existing `receive_rejection` name and
live `receive_budget` fields retain their meanings.

The held-consumer control pins exact count and byte refusals in both roles,
then releases one event, removes the link, retains its failure value, and
recovers on a fresh link. The Reference/text controls also cover decoded
single-message expansion. Removing the frozen sample fails the new assertion.
Changing only the terminal event's pending bytes to zero must also fail; a
snapshot alone cannot substitute for event evidence.

These are native/native Engine results. They do not add Godot runtime,
full-service, or remote-network qualification. Godot
[#355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355)
tracks probe adoption of the failure event and runtime assessment. Its current
probe ignores this variant and can still use the existing snapshot fields.
Native Engine callers that construct `ChannelReceiveRejected` must set
`failure`; callers that read the variant can bind it or use `..`. Rust client
main `da8f2d16` has no reference-Engine dependency. The .NET project has no
peer-stack dependency. Their signaling contracts and APIs are unaffected.

### Portable Godot runtime evidence under blocked stdout (#1240, #1244, #1264)

The server now owns the native JSONL probe under
`clients/native/tests/fixtures/godot_reference_probe.rs`. The pinned Godot
bridge keeps its existing event names. Receipt rows add live samples at the
first snapshot callback and sequence 127 in each physical generation. Markers
keep the actual first sequence; they must not invent sequence zero. Samples
include Engine receipts, SCTP admission, RTC channels, ICE/DTLS state, and
outstanding send bytes. Cross-layer counts are not atomic. A final teardown
sample can have no SCTP association, so it cannot replace these live samples.

One dedicated thread writes stdout. Each payload keeps its Engine permit until
its JSONL row is flushed. The owner can process a terminal event while output
is blocked. It retains the typed failure and pre-removal snapshot, refuses new
commands except diagnostics, and writes earlier receipts before the terminal
row. Writer failure ends an idle probe; stdin EOF flushes queued output.

The portable `probe_output_control.py` uses two probes and the pinned Godot
JSONL bridge. Both offer roles hold receiver stdout until the sender observes
channel closure. A snapshot command enters stdin before output resumes. Count
cases preserve 264 exact 4 KiB packets each; byte cases preserve 134 exact 8 KiB
packets each. Every admitted packet arrives in order before one explicit
refusal. The frozen sample retains 256 pending messages or 128 pending messages,
respectively, with 1 MiB charged. Cumulative receipts include packets flushed
before the pipe filled; they need not equal pending capacity. The control pins
all three negotiated stream IDs and preserves the snapshot after link removal.
Removing permit ownership until flush makes the control fail. Removing typed
failure evidence makes its contract test fail. Four hosted output-contract
cases cover failure fields, physical closure, writer errors, and first-marker
identity across fresh generations.

Eight final-source local service pairings cover Godot 4.3 native with baseline
and added CPU contention, and Godot 4.7.2 native/Web, in both offer roles. Each
pairing has two physical generations, same-room rejoin, original 1 KiB snapshot
bodies, the 32-credit window, mixed modes, and 32 extra startup packets per
sender and generation. Both 4.3 cohorts use CPUs 0/1; the loaded cohort adds one
busy worker on each CPU. This is a shared host, not an isolated benchmark.
An independent artifact audit confirms all 4,096 required reliable snapshot
receipts and all 1,024 startup receipts. All first/last live samples retain SCTP
state. Native receipt bytes and sequence, outgoing markers, driver receipt
checks, physical generations, and frozen probe hashes agree.

The strict gameplay verdict passes seven pairings. Loaded Godot 4.3 with Godot
offering fails after its second generation misses the initial
`reference:2:unreliable` probe. The native enqueue is recorded. Godot receives
17 later positions on that channel, all reliable traffic, and all eight ordered
probes. Service recovery and retirement complete with no script error. This
case remains a failed qualification; it is not rerun until green or counted as
supported. [Godot #458](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/458)
tracks exact transmit/abandon and ingress evidence. Current evidence does not
locate the finite packet gap or prove expiry caused it.

An earlier diagnostic-event prototype failed because the pinned bridge rejects
unknown event names. The final source nests diagnostics in existing receipt
rows. All failed attempts and source manifests remain local under
`progress/session-563/`. Final probe SHA-256 is
`41cf46f9acedbef1adbd7517102b0d88e46c814b85e314150e21f6db57985804`;
the driver is Godot commit `f3d3f8611e2c85290f222074da6d40807099e4ec`.
These results do not explain the historical #1244 reliable gap or #1264 event,
or qualify remote networks or corrected Godot channel lifetimes. Keep those
issues open. [Godot #355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355)
tracks probe adoption; [#433](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/433)
tracks lifetime repair.
The Rust client manifest and .NET project have no reference-Engine dependency.
Their public APIs and signaling behavior are unchanged. This is internal probe
and qualification work, so it needs no changelog entry.

### Ordered expiry before first native transmission (#1269)

At main `485e11da`, `pending_message_expired` exempts the first ordered
fragment. A positive-lifetime ordered message can therefore emit DATA after
its deadline. The new `outbound_evidence_separates_unsent_expiry_retries_and_reliable_delivery`
control fails its no-DATA assertion before the repair. Removing that exemption
makes the control pass. Timed(0) still permits first transmission.

`expired_ordered_prefix_forwards_sequence_and_preserves_exact_reliable_successor`
uses paired wire histories. Whole and fragmented expired prefixes emit no DATA.
FORWARD TSN advances their ordered SSN. The sender releases their buffer bytes,
and the next reliable message arrives exactly once with exact bytes. The cases
also cross TSN wraparound. Existing lifetime, retry and shutdown controls pass.
This repairs a confirmed native ordered-send contract. It does not explain the
historical reliable-prefix loss in #1244 or terminal event in #1264.

SCTP snapshots now separate local enqueue, packet serialization and
partial-reliability abandonment. They retain the last queued and first/last
abandoned message identities, stream incarnations, SSNs, ordering and captured
policies in constant space without payloads. Unordered SSNs can repeat; compare
message IDs only within the same physical association. Aggregate emitted DATA
includes retries and does not prove UDP transmission or per-message receipt.
First/last records can omit intermediate retirements. An absent matching record
cannot exclude abandonment.

`transmission_selected=false` in an abandonment record proves no fragment was
selected before retirement. A true value includes acknowledged fragment prefixes
and is not proof of packet serialization or remote receipt. Controls distinguish
unsent lifetime expiry from retry-limit retirement, retain the first record after
later retirements, prevent duplicate counts, preserve reliable retries, and clear
all metadata on statistics reset. The partial-ACK reset control also preserves
selection evidence after the acknowledged prefix leaves the in-flight queue.
RTC conversion preserves these fields and serializes real association expiry.
The hosted native runner executes the new low-level and conversion controls.

Initial native probe rows now sample the queued identity after a successful
`reference:<phase>:<label>` send. Live first/last reliable receipt samples retain
later abandonment evidence. These cross-layer observations are not atomic and
can affect scheduling. A successful enqueue is not a transmission verdict.

Both blocked-stdout offer roles still preserve every exact admitted reliable
packet before one typed count/byte refusal. The sender shows SCTP output with
zero message abandonment; the receiver's retained pre-removal snapshot also
shows zero abandonment. Each count case preserves 264 packets, and each byte
case preserves 134. These controls distinguish application-budget refusal from
finite sender expiry without changing receive budgets or failure handling.

Godot [#355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355)
tracks diagnostic adoption and qualification. The external Rust manifest at
`da8f2d16` and .NET project at `e467c871` do not depend on this native SCTP stack.
Their signaling, relay behavior and APIs are unchanged. Native ordered expiry
and diagnostic API changes have user-visible changelog entries.

The final eight local service pairings cover Godot 4.3 native baseline and added
CPU contention, plus Godot 4.7.2 native/Web, with both offer roles and two physical
generations each. Every exact reliable snapshot and startup receipt survives:
4,096 snapshot receipts and 1,024 startup receipts. The independent artifact
audit checks native bytes/order, outgoing markers, driver receipt checks,
physical generations, live samples and frozen probe hashes. It also verifies
that the runtime diff's 11 changed Rust files match the final source.

Six pairings pass the strict initial-probe verdict. Two Godot 4.3 cases fail:
baseline Godot-offer phase 1 and loaded reference-offer phase 2 each miss the
initial unreliable probe. Each send row and first-abandonment record identifies
message 33, stream 3, stream incarnation 2 in the same physical generation.
Each abandonment has `reliability=lifetime` and `transmission_selected=false`.
These two fresh missing probes expired locally before first selection. All
reliable traffic, service rejoin and retirement checks pass. The 1 ms native
finite-channel lifetime is unchanged. These are failed strict qualifications,
not successful initial-probe delivery. They are not rerun until green.

The pinned driver's native-profile rule can accept `received=[reliable]`.
Its `gameplay_passed` flag alone is therefore weaker than the strict initial-probe
requirement. The independent audit requires both labels in both phase lists and
retains the original driver result alongside the stricter failed verdict.
[Godot #458](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/458)
records both the new causal boundary and this qualification distinction.

Final probe SHA-256 is
`e36e4d4df4796b87863357c4e25e633713b56dee2ba8ddeb89c1bdea044ea822`;
probe source SHA-256 is
`eeff7311ee8d605585dd6263fe33da120e6eed1344e5ca72b697badcdb25fe70`.
The driver remains `f3d3f8611e2c85290f222074da6d40807099e4ec`.
An earlier cohort also preserves all reliable receipts and matches three missing
initial probes to lifetime abandonment. It lacks selection evidence and has
five strict passes. Its manifest records a transient arithmetic spelling edit
after compilation; it is supporting evidence, not the final-source claim.
All attempts remain under local `progress/session-564/`. Both 4.3 cohorts use
CPUs 0/1, and the loaded cohort adds one busy worker per CPU. Other checks share
the host; the baseline is not an isolated benchmark. No result attributes the
older #1244 reliable gap, #1264 event, or #458 binary, or qualifies remote networks.

### Large ordered expiry backlogs and public drain readiness (#1271, #1272)

At main `e8a2d969`, a paired native history queues 32,770 one-byte ordered
Timed(1) messages and a reliable successor on the same stream. At +2 ms, the
expired prefix consumes more than half the 16-bit SSN space in one poll. The
receiver acknowledges the successor but cannot read it. A 32,768-message control
passes. The red regression fails at the exact successor-read assertion.
An isolated `git archive e8a2d969` control directly confirms the successor's
cumulative SACK, zero sender buffered bytes and absent receiver read, both for
the reliable successor and for an unchanged Timed(1) channel. The full-wrap
65,538-message case already passes on that baseline; it is a positive wrap
control, not a red proof.

The #1271 repair bounds each FORWARD TSN step and new DATA assignment to 32,767
TSNs beyond cumulative acknowledgment. The normal sender and zero-window probe
both enforce the fence. Expired fragmented messages still retire atomically;
the peer acknowledges bounded forward reports before farther DATA can proceed.
This is a conservative association-wide sequence limit, including unordered
DATA. It does not change byte budgets or retransmission policies. It adds a
constant-time check to selection; capped forward reports use the existing
retained-prefix map reconstruction. This is correctness evidence, not a measured
throughput improvement.

The paired backlog matrix covers the half-range boundary, full SSN wrap, TSN
wrap, lost first FORWARD TSN with actual timeout recovery, a stale repeated
report, and zero-window probing. Separate controls check a fragmented expired
message spanning more than one report and exact ordered live gameplay across
the new fence. They require exact successor bytes, no extra receipt, exact
abandonment totals and released send credit. Four low-level cases change one
stream's reliability policy through the public SCTP API. Two further cases keep
Timed(1) throughout: after the old prefix expires, a fresh message arrives with
exact bytes before its own deadline. They cover normal and zero-window paths,
including full SSN and TSN wrap. They require no Godot channel policy change.
The virtual clock remains fixed during this fresh finite-message exchange;
these cases do not promise finite delivery after a real scheduling delay.

A separate public API regression confirms #1272. `Association::is_drained`
returned true in all live shutdown states and false in Closed. All four new
drain controls fail on the old predicate. The repair separates internal
shutdown classification from public retirement. It preserves stream write
refusal and reset suppression while requiring terminal retirement, final
application read/stop and final packet handoff before reporting drained.
Paired controls retain exact admitted gameplay through shutdown, unread closed
tails, read/stop cleanup, abrupt closure and fresh/live associations.

The native WebRTC wrapper does not call the public association drain predicate.
The deterministic defect does not establish the cause of #1120 or the original
issue #1184 history. The new sequence defect likewise does not attribute the
historical Godot #1244 reliable-prefix gap or #1264 terminal event. The broader
issues #1145, #1184, #1240, #1244 and #826 remain open. Replacement-package qualification
under #912 must retain the new serial-window, backlog and drain selectors.

The external Rust manifest at `da8f2d16` and .NET project at `e467c871` do not
consume this native SCTP stack. Their wire protocol and APIs are unchanged.
[Godot #355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355)
tracks native reference-stack adoption and qualification. These paired native
controls do not qualify Godot runtime, service sessions or remote networks.

Local final controls pass: the six-cell backlog matrix, two serial-window
controls, 73 selected shutdown/expiry/drain tests, and the native Engine startup,
lifetime-expiry and active-consumer recovery controls. Strict root and native
all-target/all-feature lint pass. Independent adversarial review found no
blocking issue. These scoped results leave full hosted qualification to CI.

The current-head native CI run `40594066687` passes all 308 library tests and
21 interop tests, then fails the existing idle-output-owner control. Issue #1274
records the actual failure: completion wakes the owner before the writer thread
drops its pending receiver, so a later event can still enqueue. The probe now
drops that receiver before sending completion. This closes admission and releases
queued receive permits before notification. The existing error and late-emission
assertions and five-second deadline stay unchanged. This is a qualification-probe
lifecycle repair, not evidence of native SCTP loss or the historical #1264 cause.

### Native shared receive-capacity owners (#1264, #1244, #1240)

The previous live overload controls use one peer link per Engine. A new
three-Engine, two-link workload retains application events from both senders
in the same receiving Engine. Both offer roles separately fill the unchanged
256-message limit with 16-byte packets and the 1 MiB limit with 8 KiB packets.
Each sender owns half the retained messages. The next reliable packet on the
second link must produce the exact typed refusal. Closure cannot substitute
for that event. Neither link's local retained count may include the refusal.

The first link must still permit work on all three required channels. Dropping
one of its retained events restores exactly one slot for an exact reliable
successor. The refused link remains terminal. Replacing both physical links
must produce empty local ownership counts while all retained old-generation
events still charge the same Engine budget. Those events must be stale. After
they drop, both fresh links carry exact reliable binary gameplay toward the
center and exact reliable text gameplay in reverse. This is a two-link local
workload, not a complete three-player mesh or a Godot runtime assessment.

Each diagnostic channel now exposes `pending_application.messages` and
`pending_application.bytes`. The message permit owns these counts until the
event drops, including after peer removal. Binary uses raw bytes; text uses
its decoded UTF-8 size. Empty messages count against message capacity. Each
fresh physical generation owns new counters. Samples are separate and do not
form an atomic ledger. Transport callbacks that the Engine has not published
are excluded. Existing receipt totals and shared-budget fields keep their
meanings. No receive limit, negotiation setting or refusal behavior changes.

The red-first control on main `d0d10f20` fails with null ownership observations
instead of 128 messages and 2,048 bytes. Candidate run `d61c7309` passes all
four role/limit cases. Run `a126e68c` also passes the new text/binary observation
control and existing held-consumer, active-consumer and shared-budget controls.
Final run `c879c741` adds reverse gameplay and a closed application receiver;
all three selected tests pass. Failed publication must release local and global
receive capacity. The projection control retains empty, invalid-UTF8 and Unicode
binary input under both Reference and Godot profiles. Local logs remain in
`progress/session-566/`; portable source is in the native Engine test modules.

This adds capacity-owner evidence for future #1264 and #1244 reproductions and
multi-peer admission/recovery qualification for #1240. It does not attribute
the historical terminal event or reliable-prefix loss. Those issues remain
open. [Godot #355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355)
tracks optional probe use of the additive snapshot field and separate native/Web
runtime, service and remote-network qualification. Existing readers can ignore
it. The inspected Rust manifest at `da8f2d16` and .NET project at `e467c871` do
not consume the reference Engine. Their APIs and signaling wire are unchanged.

Final-source run `2c7a7cd1-d3e0-428c-85b2-f056bbda8ac4` passes the three new controls.
Strict root/native all-target/all-feature lint and documentation checks pass.
The CI configuration validator reports only its bundled dependency-audit failure;
that result stays visible under the GOAL audit exception.

### Idle replay copies and terminal native readiness (#1277, #1276)

The root replay path cloned each replayable room event before checking whether
its room had a pending reconnect. A baseline on `9846ef44` records 641
allocations and 8,472,256 allocated bytes for 128 ignored `PlayerJoined`
events with 64 KiB metadata. This includes one 64-byte runtime allocation.
The repair borrows the event through the existing replay-state lock and clones
it only when the room ring will retain it. Unconditional `buffer_event`
still transfers ownership. Gate admission, sequence assignment and capture
remain under one lock; the change adds no separate pending-room check.

The allocation control warms the runtime before measurement. It requires zero
allocations, reallocations and allocated bytes across 128 ignored events, both
before any disconnect and after the last pending reconnect completes. Its
positive case retains the complete metadata while a reconnect is pending.
This advances #636 and the A06 replay review under #826. It does not change
wire messages, replay contents or the snapshot precedence contract.

A separate public native Engine defect kept `connected_pair_count()` positive
after required-channel closure or receive refusal had made the link terminal.
The retained generation refused all sends while the count still reported one.
The red-first actual-channel closure control reproduces that mismatch. The
repair excludes the existing terminal admission latch from the count. It keeps
transient `Disconnected` distinct from terminal loss and counts a replacement
only after its required channels open.

Existing live controls now check both offer roles, terminal Reference channel
and peer states, all three Godot required-channel closures, shared Engine
receive refusal, the healthy sibling link, and fresh-link recovery. The count
is an observation of Engine state, not a synchronous physical-state query.
The production CLI tracks a separate count; this repair affects public Engine
readiness consumers. It does not establish the cause of the historical #1244
prefix gap or #1264 terminal event. Those investigations remain open.

The inspected Rust client manifest at `da8f2d16` and .NET project at `e467c871`
do not consume this reference Engine. The server replay change preserves
client-visible messages and semantics. No client migration is required.
[Godot #355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355)
tracks reference Engine qualification separately from native/Web Godot,
service sessions and remote networks.

Local final controls pass: five focused replay tests (`3a013539`) and three
native readiness/closure/refusal controls (`932f60e2`). Both repairs pass
independent adversarial review. Strict root and native all-target/all-feature
lint pass. The allocation result is local; full hosted qualification remains
a separate gate.

### Room occupancy percentile storage (#636)

Room percentile queries copied and sorted one occupancy value per room.
The allocation control on 1,024 rooms records 8,739 bytes for the overall
query and 11,280 bytes for four game queries before the repair. The new
frequency distribution retains one counter per distinct occupancy. The same
control records 599 and 3,392 bytes. These new byte totals are also unchanged
at 16 rooms. The existing allocation benchmark now enforces fixed ceilings
for both query paths.

The distribution preserves the integer rounded ranks for all six public
percentiles, including the maximum. Empty databases still return empty maps.
A differential control covers every per-mille rank through 1,001, repeated
sizes, zero, 255, 256 and `usize::MAX`. A live database control retains exact
overall and per-game metrics. The result is a storage improvement; it does
not establish a relay-throughput increase or a new room-capacity limit.

The server wire and public metrics schema are unchanged. Rust, .NET and Godot
clients require no migration for this change. Dependency refresh validation
includes the existing canonical-name matching control, six YAML controls,
15 RTC library controls and two native Godot-profile exchange/recovery controls.
The native RTC dependency manifest now uses `pem` 4.0.0 and `x509-parser`
0.18.1. No transport source repair is claimed by those dependency changes.

### F197 — Native stdout releases gameplay receive charges before flush

Related: #1280, #1264 and #826. Baseline: `8cbf5d9e`.
The production native CLI queued text and binary JSONL rows, then dropped their
Engine receive permits. The asynchronous writer still owned their serialized
payloads. This violated the Engine's documented permit lifetime. The separate
stdout count and byte limits remained finite; no unbounded-memory claim follows.
The Godot control probe already retained permits through its own output worker.

A held-writer regression preserves the original CLI release order through a
small output helper. Nextest `bb6d997c-ccce-4016-add4-90aa1567f80b` fails:
a retained 4 KiB receipt leaves all 256 messages and 1,048,576 bytes free,
instead of 255 messages and 1,044,480 bytes. The writer is released before the
assertion so this negative control cannot strand its output thread.

The repair moves each gameplay permit into its queued output row. The writer
releases it after write and flush, or on discard. Failed writers drop their
queued receiver before making terminal state visible. Output admission refusal
releases its unqueued permit before notifying waiters. Existing queue limits,
reserved terminal slots, and writes outside Tokio remain unchanged.

Focused controls cover held binary writes, a held text flush with a second
queued row, count and serialization refusal, finish refusal, and write/flush
failure. Failure notification and completed drain require all discarded charges
to be free. Six live Godot-profile cases close each required label in both
receiver offer roles while one exact 4 KiB reliable receipt remains held by
stdout. The old output row still owns shared Engine capacity after physical-link
replacement, while fresh per-channel counts start at zero. Flushing that row
releases capacity; each fresh link then delivers an exact reliable successor.
These cases use two native Engines, not a Godot runtime.

### Native required-channel closure observations (#1264, #1244, #1240)

`ChannelClosed` previously named only the peer, physical generation and label.
A poller error, closing callback, closed callback, ended stream, or observation
of an unusable sibling state could produce the same event. The initial #1264
artifact cannot resolve that ambiguity after the fact.

The Engine now records a typed local observation in each close event and keeps
the first observed label/reason in the physical link's `channel_closure`
snapshot. Later labels and repeated callbacks cannot replace the first record.
CLI and portable Godot probe rows retain the observation; fresh links start
with no closure record. A receive-budget refusal remains a separate typed
failure and does not fabricate a physical closure observation.

The initial live regression fails on the baseline's absent snapshot in Nextest
`9339b87a-82da-4e70-8582-af5251953ff6`. The final control also retains its terminal
event through removal and rejects it after fresh pairing. Existing held-poller
controls distinguish sibling state observation from drained terminal callbacks
and preserve queued reliable input before its own close. Output serialization
pins all seven observation names and the separate synthetic-refusal shape.

This improves future closure and missing-prefix diagnosis. It does not identify
the original #1264 event or historical #1244 loss. Native/Web Godot runtimes,
service sessions and remote networks remain separate qualification boundaries.
The inspected Rust Cargo manifest and dependency-free .NET project do not use
this reference Engine or its stdout rows, and the server wire is unchanged.
[Godot #355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355)
tracks optional probe adoption and runtime qualification. Engine close-event
constructors need the new reason; native JSONL close rows add generation and
observation fields. The native README records that migration.

Local qualification: the 15 ownership, saturation and closure controls pass in
Nextest `749053bd-49cc-49ea-b792-21524c14d700`; eight portable-probe and unread-output
process controls pass in `8869d7a8-2dc0-4e06-9ac8-9ddfc21ecf08`. The earlier six
closure/FIFO controls also pass in `5000a220-c54d-48d5-86d7-6e817cf80259`.
Root and native strict all-target/all-feature lint and the markdown, documentation
consistency and CI-configuration checks pass locally. Hosted qualification is a
separate publication gate.

The disconnected-output admission control also passes in
`1c6d02f8-9acf-41fd-86f4-e578bbd9be3c`. It requires the refused gameplay charge to
be free before output failure and completed drain are observed.

### F198 — Initial fallback completes a requested native rebuild early

Related: #1282 and #826. Final-head qualification of PR #1281 found a separate
native CLI completion defect on `eec6accf`. Verification Nightly run
`40628349941`, job `122618603505`, failed the unchanged eight-player host
1% loss cell. The server recorded 137 relayed signals against a frozen client
success-prefix ledger of 127.

Artifact `webrtc-netem-40628349941-1` (`12790104030`) locates all ten extra
signals in `client-c03.jsonl`. Initial physical failure produces relay fallback,
then `success_criteria_met` at zero-based event 39. The leaf later sends ten
signals for automatic retry and its requested reserved clean-path rebuild.
Its prefix has four signals; its complete history has fourteen. The other
clients contribute 123. The exact server ledger exposes the premature marker;
it does not establish duplicate server relay or a gameplay delivery failure.

`process_exchange_gate` already waits for local rebuild release, every planned
peer's current reconnect evidence, and the reserved final retry attempt.
`unmet_criteria` omitted that condition. Initial fallback before any pair opens
has no formed-pair exchange debt, so the client could announce success before
its explicitly requested future phase. This condition predates the session's
stdout ownership change and the portable Godot Engine probe does not use it.

The deterministic initial-fallback regression fails at the premature success
assertion in Nextest `a28bb7cb-5414-4d3e-9647-fc256136799b`. The repair shares one
reserved-rebuild predicate between exchange release and success. The regression
checks the unreleased gate, release without recovery, missing/older reserved
attempts, exact reserved recovery, renewed loss, and ordinary fallback without
a requested rebuild. It and the existing reserved-generation and stale-open
controls pass in `2120e100-acd4-4178-85b4-7703b27e7c7b`.

The network-loss rate, deadlines, exact signal ledger, graph, gameplay receipts
and transport-status assertions remain unchanged. Native CLI completion flags
are not server wire messages. Rust and .NET SDKs and the Godot adapter do not
consume this CLI success gate. Hosted loss qualification remains required.

A local run of the exact loss cell stopped at netem setup with
`RTNETLINK answers: Operation not permitted`. It did not exercise packet loss.
The loopback qdisc remains `noqueue`. This setup failure is retained separately;
it is not a passing loss control. Final hosted qualification must run the
unchanged cell on a runner with the required network-fault capability.

### F199 — Native gameplay output loses its physical generation

Related: #1283, #1244, #1264 and #826. Baseline: `320e7829`.
The native orchestrator preserves admitted gameplay across physical link loss.
Its Engine events retain the original generation, but the CLI omitted that
identity from text and binary receipt rows. Connection and channel-open rows
also omitted it. A reader could not distinguish an old queued receipt from
fresh gameplay for the same peer after a rebuild. This is an output defect;
it does not establish a transport delivery failure.

The binary-output control fails on the absent generation in Nextest
`ef9f15ca-75b1-439a-b9c2-141446072ec9`. The repair forwards the event's original
identity to stdout without replacing it with the current link's generation.
All four output constructors now require generation. JSON consumers receive
an added field. The native README records both migration requirements.
Generation numbers belong to one Engine and cannot be compared between endpoints.

Six live native/native cases hold exact binary gameplay in stdout across each
Godot-required channel closure, in both receiver offer roles. They rebuild
the physical link before releasing stdout, then check the old output's generation,
peer, and exact bytes. The new generation differs and delivers an exact reliable
successor. The same controls retain the receive charge until flush.
The process interop receipt oracle now matches each text receipt to its own
connection and channel-open observations by peer, label and local generation.

### Native terminal connection and actual channel contracts (#1264, #1240)

The Engine's local snapshot previously kept receive refusal and required-channel
closure but omitted terminal peer-connection callbacks. `connection_terminal`
now retains the first `failed` or `closed` callback for that physical generation.
Transient `disconnected` observations leave it empty. Repeated callbacks cannot
replace the first observation. Old handlers cannot alter a fresh link's record.
The record does not invent a channel closure, receive refusal, or network cause.

Transport snapshots now query each registered channel for its actual label,
internal ID, protocol, negotiated flag, ordering, lifetime and retry limit.
These `channel_properties` complement RTC statistics and SCTP stream IDs.
They describe stored channel objects, not just the selected profile's intended
settings. Query failures return an error without guessed settings. All layers
remain separately sampled; a combined snapshot is not atomic.

The initial snapshot controls fail in Nextest
`f35ba0eb-5d9d-4598-9168-c97e250b75d5`. Final focused controls pass in
`44ea1128-2870-45b5-aa35-c758da0c17fd`. They check both channel profiles and
both endpoint roles, every Godot negotiated stream ID, first terminal states,
transient disconnection, stale callbacks, and exact gameplay after recovery.
A closed-driver experiment initially assumed properties become unreadable.
Source inspection and its failed assertion show that closed channel objects
remain queryable. The corrected control checks unchanged actual contracts and
closed RTC states, then verifies removal returns a null snapshot.

This advances #1240, #1244 and #1264 without attributing their historical
runtime failures. Native/Web Godot, service and remote-network qualification
remain separate. The inspected Rust Cargo manifest and dependency-free .NET
project do not consume this reference Engine or its output enum. Server wire
messages and channel delivery rules are unchanged.
[Godot #355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355)
tracks optional evidence adoption and separate runtime qualification.

Three focused process controls pass in
`b25d1c49-a8be-4698-a25d-0d614667b82e`: initial two-peer WebRTC, remote retry,
and active-room restore with exact exchange. They use the rebuilt server and
native client binaries. Root and native strict all-target/all-feature lint pass.
Hosted qualification remains the publication gate.

### Operation latency range and replay-index ownership (#1285, #636, #775, #826)

Baseline: `39d36c4a`. Server operation histograms reserve the full five-minute
range at their first sample. Each default histogram owns 20,480 `u64` buckets
(163,840 bytes), even when every sample is below one millisecond. The new
range covers 0 through 4,095 microseconds initially and grows at the same
precision. This keeps the existing steady-state timing band free of allocations.
Input clamping still occurs before recording, so the five-minute limit and
reported metrics stay unchanged.

The range regression fails on the baseline with 20,480 initial buckets versus
20,480 full-range buckets. The repaired comparison checks exact JSON metrics
against the fixed-range implementation after each sample. It covers zero,
non-default lower bounds and precision, range growth, the configured maximum,
and over-limit values through `u64::MAX`. Existing average and sample-limit
controls also pass in Nextest `6f0c1db2-acc9-48b2-9721-9592adec7ad0`.

Separate component profiles retain 128 histograms and the same five samples per
histogram (0, 100, 500, 1,000 and 4,000 microseconds). Fixed-range bucket storage
is 20,971,520 bytes; growing-range storage is 3,145,728 bytes. Both retain 640
samples. This measures histogram buckets, not allocator overhead or server RSS.
Growth can allocate when a new range is first observed. No throughput claim
follows from this component memory result.

Capacity summary construction also keeps a second offset vector beside its
send lookup index. The new relay blocks borrow the existing sorted offsets
when each sender incarnation is contiguous in sequence order. Arbitrary replay
histories that interleave incarnations keep the original independent index.
Raw events, duplicate tie order, completion times and verdicts stay unchanged.
A new interleaved-incarnation control first fails with retained offset bytes,
then passes without them. The independent 4,096-history ledger comparison also
checks the shared path, including duplicate and extreme sequence keys.

The one-million-send component profile uses 64 interleaved senders and 16
incarnations per sender. The send index owns 4,001,536 bytes in both controls.
The separate relay index owns another 4,000,000 bytes; the shared version owns
no additional offset array. Both report 1,024 blocks and checksum 499,999,500,000.
One debug construction sample takes 1,611,535 microseconds separately and
118,714 microseconds with sharing. These single shared-host samples do not
establish a general speed ratio. Nine focused replay, corruption, ledger and
real-socket controls pass in `f93de86c-d467-4c44-ae4e-deae4e8a2ac3`.

These changes preserve server protocol messages and metrics JSON. The Rust,
.NET and Godot clients need no changes. This does not close the broader audit,
the full generator memory bound, or the historical generator-stall cause.

The first hosted allocation check rejects growth from the 1 ms warm-up sample
through the existing 0–4,095 microsecond steady-state band. The final initial
range reserves that band. The unchanged relay allocation benchmark then passes
with zero timing allocations, reallocations and allocated bytes. The measured
128-operation bucket total remains 3,145,728 bytes.

### F200 — Native transport diagnostics wait without a deadline

Baseline: `6de82293`. The first-party Engine samples RTC statistics, SCTP
statistics, and channel settings through the wrapper's shared RTC core mutex.
The Godot probe then reads channel send debt with more unbounded awaits.
A busy core can therefore prevent a diagnostic report from returning.
This is separate from the historical missing reliable prefix in #1244 and
the ambiguous terminal event in #1264. Related: #1287 and #1240.

The new regression uses a custom runtime clock only on a named writer thread.
`DataChannel::send` calls that clock while it owns the actual RTC core mutex.
The clock signals entry and waits for the fixture to release it. The Engine's
ordinary transport snapshot then waits for that same mutex. The initial
control fails its one-second outer deadline in Nextest
`81d371ad-306e-42bd-a720-ad1b8b88e268`. It releases and joins the writer before
asserting, so the failed attempt does not leave blocked work behind.

All transport queries now share one 250 ms cooperative timeout. On expiry,
the snapshot keeps completed transport samples, samples current local callback
and receive-budget observations, and identifies the unfinished stage and
channel when applicable. Uncollected fields stay absent. Send debt is part of
the same Engine query; the probe no longer starts unbounded follow-up reads.
Cancellation drops the query future without closing or replacing the link.
This bounds asynchronous waits, not synchronous work or host scheduling delay.

The extended control covers the reference and negotiated Godot profiles,
both receiver offer roles, and two fresh physical generations on the same
Engines. Each generation retains one exact 4 KiB incoming reliable packet
while the core is held. The partial snapshot preserves its generation,
per-channel ownership, and shared budget, including the first generation's
retained packet after replacement. Releasing the core delivers one exact
4 KiB reliable packet in reverse. Complete snapshots work again afterward.
Dropping both retained events releases all count and byte permits.

The first extended control incorrectly required the whole callback queue to
be empty after removal. Its failure retained an ordinary connection-lifecycle
event, not an extra gameplay receipt. The final assertion drains lifecycle
callbacks and still rejects any extra gameplay receipt. The extended control
and both existing actual/closed-channel contract controls pass in Nextest
`cc17bce4-76cc-45f6-844f-7d7a1c8b1224`.

This is new native/native recovery and diagnosis evidence for #1240, #1244
and #1264. It does not classify their historical failures or qualify Godot
runtimes, full service sessions, larger meshes or remote networks. Channel
IDs, delivery modes, receipt requirements and receive limits stay unchanged.
The inspected Rust SDK manifest does not depend on this reference Engine.
The .NET SDK project has no package dependencies. Neither needs a change;
server protocol messages stay unchanged. Godot #355 tracks optional adoption
of partial snapshot fields and separate runtime qualification.

A second new control covers actual terminal causes in both receiver offer
roles. It closes the real unordered channel, or retains 256 actual empty
reliable receipts and sends one extra message to trigger `pending_messages`.
With the core then held on a sibling channel, the partial snapshot preserves
the exact closure or refusal record and shared budget. Engine send selection
remains refused on every required label. The fixture's direct sibling write
only holds the core; it is not a claim of new Engine work admission after loss.
Dropping the retained receipts restores capacity without erasing the failure.
Both new controls pass in Nextest `e0df1f0f-0368-49bb-b6a6-405704c5b3c8`.

Five existing startup, closure, terminal-connection and saturated-recovery
controls pass in `a52600d2-7157-459f-8d2b-5afb0d78ad7a`. Four portable probe
output controls pass in `4b4a18e7-9c6d-4383-95a0-d536a22eeb3d`. The source
policy scanner required explicit Empty/Disconnected handling in the new
callback-drain check; the final control passes in
`8016b688-9f99-4ca4-8a6b-226eb87a349c`. Root and native strict all-target,
all-feature lint, formatting, Markdown and document consistency checks pass.
Godot #355 now records the inspected driver's missing timeout-marker check
and the required partial-snapshot adoption controls. No runtime result is
inferred from these native/native tests.

Final self-review found that callback publication precedes its admission-counter
update. The recovery control now waits for classification before comparing
snapshots; it does not assume that separately sampled counters update atomically.
Both new controls pass again in `c09b59cc-f5ee-4275-a713-13ad1faca5c5`.

Hosted root lint at `dcafa001` rejects JSON-map indexing in the new sampler.
Its additional `clippy::indexing_slicing` rule is stricter than the normal
local Clippy gate. The sampler now inserts into typed maps retained outside
the query future, then publishes completed maps after sampling. A timeout
omits maps that have no completed channel entries. Healthy empty maps remain
explicit. Four snapshot/contract controls pass in
`9a2b9b87-1dbb-4b9a-90ce-a34e077abff6`. The exact native production lint command
with all nine hosted rules and the normal native all-target/all-feature gate
both pass. The original failed hosted job remains visible at
[122944866399](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/40724246142/job/122944866399).

### F201 — Directional partition proxy can discard semantic close evidence

The root test run at `66eea45f` fails the client-to-server blackhole control
with Broken pipe before its required close frame. The proxy's default policy
aborts the opposite pump as soon as one direction ends. That policy cannot
promise a remaining server tail when resumed client traffic reaches an already
closed upstream. The hosted attempt retains no per-pump trace, so it does not
establish a production server close-write defect.

A new raw TCP control holds the server-to-client pump, writes an exact server
tail and closes the upstream, then forces client forwarding to end before
resuming the tail. It fails with UnexpectedEof in Nextest
`df4d2daa-5f81-4962-a5eb-02fb1a969e79`. This reproduces the fixture contract gap
without relying on a Ping timer race. Related: #1289 and #826.

The three directional partition scenarios now select an opt-in half-close
mode. Each direction keeps the opposite pump alive. A completed pump shuts
down its destination write half and drains late source acknowledgments until
EOF or explicit kill. Two standard-library socket clones provide shutdown
handles; the original socket owners stay alive until each drain ends. A reset
can therefore still reach all live sockets. Default full teardown is unchanged.
No server liveness setting, receipt assertion or test deadline changes.

The tail and terminal-injection controls live in the canonical helper target,
so every integration binary that imports the proxy does not repeat them.
FIN, RST and Drop stop a held-tail drain. Linux still requires the RST result,
and the live socket-owner count returns to zero. Existing default FIN/RST
controls pass too. Those four tests pass in
`fb9cf464-5ed4-4430-a4da-5a442f6a08f6`. The three exact-close and room-recovery
scenarios pass in `46c9a4f3-81bc-4462-91f4-a9322740e16e`. Their existing Windows
reset allowance remains; these local results add no Windows qualification.
Root strict all-target/all-feature lint and all 34 source-policy controls pass.
This repairs first-party test transport fidelity. Server messages and external
client behavior stay unchanged.

### Native closed-stdout timeout evidence and control isolation (#1290)

Native Interop at `ed23981c` times out the existing closed-stdout handshake
control after eight seconds. Its child diagnostics were discarded. The log
does not distinguish writer failure detection, client cancellation, teardown,
or a pipe that still had a temporary reader. The isolated local baseline
passes in `99e9f811-da1d-4992-8274-805bbd7d6d01`; the original four-thread,
14-test process suite also passes. Neither pass explains the hosted result.

Ten process/pipe controls previously ran concurrently in one parent. They
now share a named serial guard, so another fixture cannot spawn while a pipe
failure control establishes exclusive reader ownership. The reader closes
explicitly before the stub releases Authenticated. The handshake fixture
retains the first 16 KiB of child stderr and continues draining later bytes,
so diagnostic retention cannot add pipe backpressure. Failure assertions
include that evidence. Exit codes, offered traffic and deadlines stay unchanged.

A Linux negative control retains a second read descriptor after closing the
original reader. It observes the exact Authenticated output and a live child.
After the last reader closes, ProtocolInfo forces another output write, which
must produce exit code 3 within the original eight-second bound. This proves
that an extra reader changes the pipe-failure precondition. It does not prove
that concurrent descriptor inheritance caused the hosted timeout. The original
cause remains open in #1290; no production CLI defect or repair is claimed.

All 15 output controls pass with four test threads in the final local run
(`progress/session-572/output-reader-control.log`, 16.08 seconds). The normal
native all-target/all-feature lint gate passes. These changes isolate and
improve first-party failure evidence; they do not relax the closed-pipe
expectation or establish new Godot runtime qualification.

### F202 — Godot probe packet commands bypass terminal-link admission

Related: #1291, #1240, #1244 and #1264. Baseline: `c970b947`.

The server-owned Godot probe selected packet-command and startup channels with
`Engine::channel`. This raw accessor retains handles after a required sibling
channel closes or the Engine refuses input. The native CLI already used the
Engine send guard. A live red control closed the reliable channel, held owner
event handling, then sent on the surviving ordered channel. The command returned
success. Nextest `d31dd55c-22bc-4f35-9410-36f6d1f1f426` retains that failure.
This is a probe command-admission defect, not a server signaling defect or proof
of the historical missing Godot prefix or ambiguous terminal event.

`Engine::send_channel` is now public. Packet commands and every startup packet
use this same guard. Selection refuses observed terminal connection state,
required-channel loss and receive refusal before selecting a transport handle.
Raw lookup remains available for diagnostics and closure. Selection is not atomic
with the subsequent send; callers still need send-error handling and deadlines.
The negotiated channel contract and receive limits do not change.

Six live closure cases cover all three required channels and both offer roles.
They retain an exact 4 KiB reliable receipt while the owner has not drained the
terminal event. Text, binary and startup sends refuse on the old link; refused
commands emit no sent row and do not change the SCTP last-enqueued message.
Replacement keeps the old receipt charged until its owner drops it. Each fresh
physical generation then receives the exact reliable payload.

Four additional cases fill the actual Engine count and byte budgets in both
offer roles. They hold 256 empty receipts or 128 exact 8 KiB receipts, respectively,
and send one excess packet. Commands refuse before owner event drain. The exact
admitted prefix precedes the typed refusal, whose sampled failure remains intact.
Fresh pairing retains old receipt ownership and resumes exact reliable startup
after those receipts drop. Removing command guarding makes both regressions fail;
all attempts remain under ignored `progress/session-573/`.

The Rust client manifest at `da8f2d16` and .NET project at `e467c871` do not depend
on the native reference Engine. Their APIs and signaling remain unchanged.
[Godot #355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355)
tracks probe adoption and separate runtime qualification. These native/native
controls advance three open gameplay investigations without claiming their
historical causes or remote-network acceptance complete.

#### Initial guarded probe runtime qualification

These runs use the first guarded candidate, `0e072e8c`, before the startup-owner
repair below. Their frozen source and binary remain separate from later attempts.

Six local Linux ARM64 service pairings use that initial guarded candidate: Godot 4.3 native,
Godot 4.7.2 native and Godot 4.7.2 Web, each in both offer roles. Every case keeps
two physical generations, same-room rejoin, mixed modes, the original 1 KiB
snapshot bodies, 32-credit window and 32 startup packets per direction and phase.
All 3,072 required reliable snapshot receipts and 768 startup receipts complete.
The independent artifact audit checks native bytes, sequence, generation, live
first/last samples, frozen probe hashes and driver receipt counts. No local
build overlaps these runs. The 4.3 driver uses CPUs 0/1 on a shared host; this is
not an isolated performance result.

The driver reports six passes, but the strict initial-probe audit reports four
passes and two failures. Godot-offer phase 1 lacks the initial unreliable probe
in both the 4.3 native and 4.7.2 Web cases. These failures remain visible; no case
was rerun. In each failed case, the `reference:1:unreliable` send boundary records
physical generation 1, message ID 33, stream ID 3, stream incarnation 2, SSN 0,
unordered lifetime policy and `transmission_selected=false`. A later live sample
at `snapshot:1:godot:127` records that exact identity as the first abandoned
message, still with no transmission selection. No channel closure or terminal
connection observation exists in that sample. This locates these two omissions
at local lifetime retirement before first transmission selection. It does not
locate the historical #1244 reliable gap, #1264 terminal event, or original
[Godot #458](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/458)
packet. Keep strict finite-probe qualification separate from reliable recovery.

The driver is `f3d3f8611e2c85290f222074da6d40807099e4ec`. Probe binary SHA-256 is
`cfa7d20b9b8aa61bd7ef42eb70e9310b847bd3eb4ae9192aa73b2f18f395fc92`;
probe source is `38bc1ed7b6589bc81dd39e01ea67bc89f055281a739fae1abe0255f80216bb43`.
The runtime manifest, independent reliable audit, failed strict verdict and exact
finite-retirement witnesses remain in `progress/session-573/runtime-guarded/`.
These are local artifacts, not hosted evidence. The portable fixture and pinned
driver provide the reproduction source. Remote-network and corrected Godot
lifetime qualification remain open.

#### Startup refusal must preserve the owner event loop

Review found that a guarded startup refusal still propagated from the inline
startup burst through `main`. This could exit before the owner consumed its
queued receive refusal and pre-removal evidence. The real open-event handler is
now independently callable by the fixture controls. Red Nextest
`dc007e17-4fe3-490f-ae12-48bc3064ab13` fails both closure and actual-budget cases
because startup handling returns an error before owner drain.

The handler now stops a refused burst without emitting `startup_sent` or exiting
the owner loop. The same controls then consume the exact admitted prefix and
typed failure. Fresh links also pass all 32 exact startup packets and publish
completion only after the full burst. All six probe tests pass in
`ecf23176-d333-4bed-a66b-0a00116f9f6c`. Hosted root CI also found that the new
empty-output assertions used a macro form its source policy cannot inspect.
Explicit Empty/Disconnected branches preserve the assertions and pass the
focused policy control in `96078bb6-965b-4ba5-af26-bda718c9c0e5`.

#### Selected-channel startup send errors

A later review identified a separate error class: a live peer can negotiate a
message-size limit below the startup payload. The send then returns
`ErrOutboundPacketTooLarge` without closing the link. Treating every startup
error as link refusal lost that evidence. Red Nextest
`8b93fed7-865e-467a-86c7-017e05152d91` confirms an open negotiated pair silently
returns without a failure row at a 512-byte limit.

Startup selection now returns link refusal separately from a selected send error.
Guard refusal remains silent so pending terminal evidence drains. A selected send
error emits the existing `startup_sent` event with the exact admitted count,
channel label and `startup_failure` error before returning to the owner loop.
A count below 32 cannot establish completion. Reusing the event name preserves
the pinned bridge, which rejects unknown names. This row does not invent a
receive refusal, channel closure or new peer state.

Four live cases cover both offer roles at 512 and 1,046 negotiated bytes. The
first limit refuses packet zero. The second admits exactly ten 1,046-byte packets
and refuses sequence 10 at 1,047 bytes. Each case pins the typed send error,
exact admitted count and payload prefix, open link state and a later exact small
reliable packet. Budget-recovery cases now also consume the full 32-packet
startup burst and assert its completion/open rows after dropping old receipts.
All seven probe tests pass in `c5e69ee7-d66a-47f0-b134-9d35080f59e4`; native scoped
strict lint and source policy pass. This refines #1291 and the terminal evidence
needed by #1264; it does not change negotiated limits or supported Godot pairings.

### F203 — Lobby promotion clears accepted public Room readiness (#1293)

Baseline: `01a85868`. `Room::set_player_ready` accepts readiness in Waiting,
but `Room::enter_lobby` clears only `ready_players`. The player flags remain
ready. A room whose members all readied before promotion then fails
`all_players_ready` and `finalize_game`. The two readiness views disagree.

The red control marks one of two Waiting members ready, then promotes the
room. The original code returns an empty ready list instead of that member.
The repair preserves the existing list. The table covers no ready members,
a ready subset, all members ready, flag/list agreement, repeated promotion,
the original lobby timestamp, and the finalization precondition. A stored-room
control also exercises `GameDatabase::transition_room_to_lobby`. The four
focused lifecycle controls pass in Nextest
`8e0989ff-23d0-4fac-859e-8f169384155d`; the red run is
`f0157ab0-88e7-4e1c-990c-0f27e6232c75`.

The source sweep finds that database transition as the sole production caller
of `enter_lobby`. The normal coordinator keeps a separate readiness set and
already preserves its published lobby readiness. This finding establishes a
public Room/storage inconsistency, not a normal WebSocket gameplay incident.
The Rust client does not depend on the server crate; .NET and Godot do not call
its Rust Room API. No wire fields or client migration change.

### Borrowed session host candidates (#636)

Initial host selection and host replacement clone every eligible member into
a temporary vector. Names, endpoint strings and capability vectors are copied
only to elect one member. Direct eligibility also constructs an owned endpoint
only to check whether it exists. The repaired selection borrows candidates
and validates endpoint parts before allocating only the endpoint returned to
a plan consumer. Authority preference, oldest-join ordering and UUID ties stay
the same.

The allocation control calls each initial/replacement selector 64 times for
2, 16 and 256 members under WebRTC and Direct. All twelve baseline cells
allocate. At 16 members they allocate 225,728 through 235,520 bytes, plus
128 reallocations per cell. All twelve repaired cells allocate and reallocate
zero bytes. The existing relay allocation ceilings also pass. Nineteen focused
selection, endpoint and failover tests pass in Nextest
`e72b3ca6-5a7b-4b53-ada6-ecadadd04eb7`, as do all nine independent
session-policy property tests. These are component allocation measurements,
not a server throughput claim.

### Capacity room-churn sort scratch (#775)

Room-churn indexing still stably sorts raw offsets by timestamp. The repair
sorts in place by timestamp and unique raw offset. This keeps the original
stable order for equal timestamps, including the viewer-first merge, without
scratch allocation proportional to the history. Compact and wide offset paths
remain available; raw records and replay verdicts are unchanged. Three focused
churn controls pass in Nextest `e297c3c5-16ed-41fd-9473-995451bffe1c`,
including independent stable viewer comparisons through 4,096 events.

An isolated counting-allocator control sorts one million events through the
production offset implementation. The baseline allocates 4,000,000 scratch
bytes for compact offsets and 8,000,000 for wide offsets. The repair allocates
zero for either path. All four controls produce checksum
`3359507878442902874`. This measures sort scratch, not whole-run RSS or the
full ten-minute workload. The generator memory-bound requirement stays open.

### F204 — Unread inbound frames reset the semantic server close (#1295)

Fortress Interop at `f3bfa464` receives `GoingAway`, then a TCP reset instead
of close 4000/`server_shutdown`. Job `123141685411` retains clean pre-drain
Running snapshots from both peers. One unchanged focused local drain/restart
run passes in 14.27 seconds. That pass does not explain the hosted failure.

Source inspection finds that the reader drops its split stream when closure
starts. The writer sends a semantic close and flushes its sink, but it does
not consume the peer's close response. Dropping the transport with unread
inbound frames can reset the socket and lose the close tail.

A controlled real-socket history holds the reader on a selected Ping, queues
128 KiB of outbound frames and 64 unread inbound Ping messages, then requests
shutdown. After the writer unregisters, the reader resumes. Baseline Nextest
`215975e0-c1d2-45b5-8963-531756a4eabe` records premature handler completion
and `Connection reset by peer` instead of a semantic close. This proves the
first-party failure class; it does not establish the original hosted schedule.

The repair retains the reader during common receive teardown and discards
in-flight frames until peer Close, EOF, error or the existing four-second
close budget. It runs with lifecycle cleanup, not after that cleanup's waits.
The first close request records its timestamp in the existing watch state.
Duplicate requests and a Shutdown reason upgrade preserve that timestamp. A
late reader cannot start a fresh four-second window. One absolute deadline
covers every discarded frame; cooperative task budgeting lets cleanup and
the writer progress. No discarded message reaches application
handlers. Common placement also covers a lifecycle change after frame read,
rather than only the close-listener branch. Invalid transport frames retain
the WebSocket codec's existing error-close behavior.

The repaired real-socket control observes exact close 4000/`server_shutdown`,
retains socket ownership through the peer acknowledgment, and records no new
heartbeat effects. All 13 final close controls pass with all features in
`9af77a1b-00d9-4ad9-aa8e-1759f6c0d3c6`. A fresh server binary also passes
the real Fortress drain/restart case in 14.36 seconds. Its first green run is
`370a1d30-c76d-486e-9e2e-c91f843e38dd`. Replacing the request-time anchor
with the late reader exit time fails the withheld-acknowledgment control in
`6ab22057-0f64-4b2f-af48-72b43215d4c1`. Paused-clock controls check silent
peers, late and continuously ready frames, deadline expiry, and terminal frames.

The wire schema and close codes stay unchanged. SDK runtime qualification is
tracked separately in Rust #297, .NET #149 and Godot #482; all three already
expose peer close codes. A completed server control does not complete those
client assessments.

### F205 — Duplicate room admission replaces an existing member (#1296)

Baseline `ee20b51e`, under #826. The public Room and in-memory storage
admission methods check available capacity but do not check whether the ID
already occupies that roster. With spare seats, a duplicate player replaces
its stored snapshot and increments the session join count. A duplicate
spectator also replaces its snapshot, refreshes room activity and increments
the spectator join count. Uncapped spectators have the same behavior. A full
roster instead refuses, so unrelated occupancy changes the result.

The player storage control fails on the baseline in Nextest
`d34c0748-8ed9-4f3d-8545-43d0c54d123b`. The public Room control also fails
with spare capacity. The two spectator controls fail before their repair in
`8447cc24-febd-4ab6-926f-d9e711418e6d`, including the uncapped case.

The repair refuses an ID already in the relevant roster before mutation.
Controls preserve complete stored snapshots, readiness, authority, activity
and session counts. They cover full and non-full player rosters, capped and
uncapped spectator rosters, concurrent duplicate player admission, and a real
remove-then-rejoin. The guard uses the same storage write lock as admission.
It does not add a second membership transaction.

This is a public Room/storage API defect. The normal server checks membership
before joining; a WebSocket gameplay race has not been established. Rust's
client manifest at `da8f2d16` does not depend on the server crate. The .NET
project at `e467c871` has no server library dependency, and the Godot client at
`58f896c6` uses GDScript. These clients do not call the changed Rust methods.
Peer-list reservation preserves wire values and order. No client migration is
required for these repairs. The wider #826 campaign remains open.

### Session peer-list reservation (#636)

Host and Mesh publications build filtered peer lists with an iterator whose
final length is unknown to Vec. The old collection grows its allocation as
peers arrive. The repair counts eligible members before cloning the same
peer records in the same order, then reserves the final list length.

An isolated counting-allocator control builds each list 64 times through the
production plan method. The 16-member baseline performs 128 reallocations per
topology; the 256-member baseline performs 384 and grows 774,144 bytes per
topology. All six repaired controls (2, 16 and 256 members, Host and Mesh)
perform zero reallocations. At two members the total allocated bytes fall
from 13,696 to 4,480 per topology. Required peer-name copies remain. Existing
session-policy controls cover filtering, offer direction and exact peers.
These are component allocation results, not a server throughput result.

Final all-feature admission and session-plan validation passes all 15 selected
controls in Nextest `81ee7ed5-e7ee-4e4e-9962-dd9d36614733`. The capacity
byte-budget and exact-replay controls pass all six selected tests in
`a206f7ab-cd6b-4bb4-a247-3c24264ccca3`. Strict all-target, all-feature Clippy
passes. These scoped results do not complete either broad campaign.

### F206 — Storage assigns room authority to an absent player (#1298)

Baseline `ca95e6c0`, under #826. `update_room_authority` clears the previous
member's flag and assigns any requested ID. Unknown players, departed members,
and spectator-only IDs can leave a room with an authority outside its player
roster. That slot also blocks ordinary authority requests. The public
`Room::set_authority` method already refuses absent players.

The regression fails before repair in Nextest
`ca86c38a-d49a-49d8-b7d3-2807e240c054`: an unknown-player grant returns true.
The repair delegates to the Room method while retaining the storage write lock.
Membership validation and assignment remain atomic. The first green run is
`eb63a744-e20c-4ba9-a21a-dff6832e3408`. The control covers unknown, departed,
and spectator-only targets, existing and empty authority slots, supported and
unsupported rooms, missing rooms, valid transfers, repeated assignment, release,
and recovery after refusal. Refused requests preserve all member snapshots,
readiness and activity. The public storage contract now states these rules.

Normal authority transfer checks membership before the write; reconnect rollback
clears the role with `None`. This finding proves a storage API inconsistency,
not a live WebSocket incident. The Rust client manifest at `da8f2d16` has no
server-crate dependency. The .NET project at `e467c871` and the Godot GDScript
addon do not call this Rust storage API. No wire field or client migration
changes. The wider #826 campaign remains open.

### Readiness snapshot storage (#636)

Live readiness snapshots cloned a HashSet, built a membership set, then grew a
filtered result vector. The repair copies ready IDs directly into their result
vector and filters it in place. It preserves iteration order, membership
filtering, the durable finalized-room fallback, and empty results on storage
failure. The snapshot drops its map lock before the membership read; a paused
read control proves a concurrent clear completes and the original snapshot
still returns only its present member.

An isolated allocation control performs 64 production snapshots for 2, 16 and
128 members, with no departures and with half the members departed. Every cell
removes 64 allocations. At 16 members with no departures, allocated bytes fall
from 306,752 to 269,888 and reallocations fall from 128 to zero. At 128 members,
bytes fall from 2,313,792 to 2,033,216 and reallocations fall from 320 to zero.
All six repaired cells perform zero reallocations. The existing relay allocation
ceilings also pass. These are component allocation measurements, not a server
throughput claim. The wider #636 campaign remains open.

### Compact churn evidence ordering (#775)

Churn validation stored one raw offset and one ordinal as two host-width
integers per event. The repair stores compact offsets and disconnect counts
when the raw history fits u32. It derives rejoin ordinals from unchanged raw
epochs and retains a host-width fallback. Both widths preserve the independent
stable reference's exact raw order, including tied timestamps, malformed
extreme epochs, and reversed input. Seventeen invalid-history mutations retain
exact verdicts through artifact replay.

At one million rows, retained index bytes fall from 16,000,000 to 8,000,000 on
this 64-bit host. Six alternating optimized component controls produce digest
`1604667450296019028`. Compact times are 816, 969 and 947 ms; the frozen stable
reference takes 997, 942 and 1,216 ms in the same binary. Debug comparison is
slower for compact ordering, so these measurements do not establish a general
speedup. The memory claim covers this index, not process RSS or the full
ten-minute C3 workload. The wider #775 qualification remains open.

Final all-feature authority/readiness controls pass 13 tests in Nextest
`7d035f2c-91c7-4fcc-ba94-04875eeefa06`. Churn ordering passes three controls in
`bed45e66-1e47-413d-9577-14a1dcd0f6ea`; negative histories and exact replay pass
in `2d1c107a-bac6-44d2-933d-00bfd9e8b674`. Strict all-target, all-feature Clippy
and both source-policy scans pass. These scoped checks do not complete the
broad correctness or optimization campaigns.

### F207 — Public Room admission keeps stale member flags (#1300)

Baseline `fada65ae`, under #826. Public `Room::add_player` stores incoming
readiness and authority flags unchanged. A stale authority flag can report two
authorities. An open-room ready flag can disagree with the ready list. Restoring
a finalized member keeps its ready flag but omits it from the ready list.
In-memory admission already reconciles these values. Separately, storage
`toggle_player_ready` returns success for an absent lobby member without a toggle.
The live ready handler uses the coordinator, not this storage method.

Both controls fail on the baseline in Nextest
`39f1ba6c-5c1d-423e-8e3f-6e68c25ecbb3`. The repair puts admission reconciliation
in Room and reuses it under the storage write lock. Authority follows the
selected room member. Readiness follows the open-room list or a finalized
returning member's snapshot. Duplicate and full-room refusals still precede all
mutation. The absent-member toggle returns None. First green validation passes
four controls in `bd2ac6b5-5e2a-4c81-9273-1712e63c1e77`.

These are public Rust API defects. No live WebSocket incident is established.
The Rust client manifest at `da8f2d16` has no server-crate dependency. The .NET
project at `e467c871` uses a dependency-free C# client. The Godot addon at
`6f915b44` uses GDScript. They do not call these Rust Room/storage methods.
Dashboard values and capacity artifacts keep their formats and meanings. No
client migration is required. The broader #826 campaign stays open.

### Dashboard refresh storage (#636)

Each refresh cloned every snapshot map before storing the snapshot, even when
history selected only the active-room count. History also pushed its next entry
before removing the oldest entry. A full vector therefore grew beyond its sample
limit. The repair moves the snapshot after recording selected history fields and
removes the oldest entry before pushing. Zero-capacity construction consistently
uses the existing one-sample minimum.

The bounded-history control fails before repair in Nextest
`9ac512a2-8c93-40e2-aa78-5c4edb60fc2a`. It checks capacities zero, one and four,
field selection, chronological rollover, failure retention, staleness and recovery.
Final all-feature library validation passes 57 selected controls in
`f0482f10-98cd-4921-a878-41fbae259f4d`, including admission, dashboard and
restored-connection histories.

An isolated current-thread StatsAlloc control calls the production refresh method
with one, 16 and 128 game entries, four history slots and eight samples. Snapshot
construction and view reads stay outside the measured region. At 128 games, the
steady refresh with full history falls from 1,032 allocations and 94,854 bytes to
516 allocations and 47,427 bytes. Count-only history falls from 516 allocations
and 47,427 bytes to zero. At the first rollover, the baseline grows the vector;
the repair performs no reallocation. Selected history still owns its required
copies. These are component allocation results, not server throughput results.

### Shared initial churn epochs (#775)

Every adopted member allocated a one-element epoch vector, including rooms with
no churn. The repair borrows the immutable initial epoch slice. A new distinct
epoch promotes only that member to owned storage. Repeated epochs keep the
borrowed representation. Sorted membership, sparse and reversed epochs, and
extreme epoch values retain the independent set reference's results. Raw events
and artifact formats do not change.

An isolated counting-allocator control calls production `churn_views` for 1,000
rooms with 16 players each. Baseline allocations are 25,118 and 3,754,128 bytes;
the repair uses 9,118 allocations and 3,690,128 bytes. This removes 16,000 small
allocations and 64,000 requested bytes. Both controls build 16,000 viewer entries
with 16 adopted members each. Process RSS is not an exact allocator measurement.
This component result does not demonstrate the full ten-minute C3 workload; #775
stays open.

All 32 selected all-feature capacity and replay controls pass in Nextest
`e4c010c3-d1bb-4e5e-aaa6-e9ee9b51e330`. They include exact live delivery,
invalid-ledger replay, mixed unchanged/churned views and independent epoch-set
comparisons. Both source-policy targets pass all 29 controls. Strict all-target,
all-feature Clippy and the markdown, documentation and CI-configuration gates
pass. These scoped checks do not complete the broader campaigns.

A caller review also finds four legacy protocol fixtures that seeded authority
through player flags or direct field assignment. They now select authority
through `Room::set_authority` and keep their original peer-list, single-authority,
transfer and departure assertions. All 29 selected protocol and admission controls
pass after this fixture correction.

Hosted Linux Nextest at `7bb717eb` rejects the new dashboard helper's wall-clock
read in `clock_source_scan`. The helper now injects distinct fixed timestamps;
no policy exception is added. All nine clock-policy controls pass, and the
paused-clock dashboard control passes in `fe15ea46-a120-43b0-9297-fcd9e0823523`.

### F208 — Public player membership leaves the expiry clock stale (#1302)

Baseline `301df5ad`, under #826. `Room::add_player` and `Room::remove_player`
change membership without refreshing `last_activity`. An embedder that uses
`Room::is_expired_at` can therefore expire a newly occupied room or give its
last departure less than the full empty timeout. The spectator methods already
refresh activity. The shipped in-memory storage updates its own wall and
monotonic clocks, so this finding does not establish a live server incident.

The membership regression fails on the baseline in Nextest
`ee07f875-d005-4f6d-bb9a-71747312be3b`. The repair refreshes activity only after
successful admission or actual removal. Storage reuses the Room methods while
retaining its monotonic clock and session counts. Duplicate and full-room
admissions, absent removals, creation time, readiness and authority keep their
contracts. The expiry control covers open and finalized rooms and the exact
empty-timeout boundary. All 17 selected all-feature room and storage controls
pass in `800960f2-1d1b-4a3d-8a97-16f5c43b80ab`, including monotonic-clock steps,
concurrent duplicate admission and spectator occupancy.

The Rust client manifest at `da8f2d16` has no server-crate dependency. The .NET
project at `e467c871` is a C# client without that dependency. The Godot addon at
`39085860` is a GDScript client. None calls these Rust Room methods. Server
storage preserves its existing lifetime policy, and wire messages do not change.
No client migration is required. The broader #826 campaign remains open.

### Borrowed sender epoch index (#636, #775)

The capacity oracle built a separate `BTreeSet` for every sender, including
senders with only one epoch. It now borrows that epoch from the immutable relay
block key. Churned senders promote to a sorted owned list. Distinct, sorted block
keys preserve exact set iteration and binary-search membership. The independent
set control covers empty input, duplicate keys, Unicode names, sparse and
extreme epochs, and borrowed-pointer identity. The full oracle retains raw
records, invalidation reasons and artifact formats.

An isolated StatsAlloc control compiles the production helper verbatim with a
unit-valued relay-block stand-in. It measures index construction only; names and
block keys are built before the measured region. The frozen reference uses the
previous set-building loop. On this 64-bit ARM host, the results are:

| Senders | Epochs each | Set allocations | Borrowed allocations | Set bytes | Borrowed bytes |
| --- | --- | --- | --- | --- | --- |
| 16 | 1 | 19 | 3 | 2,360 | 1,464 |
| 15,984 | 1 | 18,646 | 2,662 | 2,145,360 | 1,250,256 |
| 63,936 | 1 | 74,591 | 10,655 | 8,585,112 | 5,004,696 |
| 15,984 | 4 | 18,646 | 18,646 | 2,145,360 | 1,506,000 |

The four-epoch list also performs one reallocation per sender; the single-epoch
case performs none. Every control compares exact epoch lists with the set
reference. Requested allocation bytes are not process RSS or peak memory.
The ignored `generator_sender_epoch_index_memory_profile` provides a separate
production-helper RSS and serialized-digest control; set
`CAPACITY_EPOCH_SET_REFERENCE=1` for its frozen reference and
`CAPACITY_EPOCH_CHURN=1` for four epochs. This component result does not prove
server throughput or the full ten-minute C3 qualification. Both campaigns stay
open.

All ten selected all-feature oracle, negative-history and live relay/replay
controls pass in `b2eaf851-ee3e-4025-b73a-9353c3c7df86`. They include inactive,
unannounced and stale epochs, exact gap failures, mixed churn views and opaque
payload leaks. These controls do not complete the wider correctness campaign.

The fresh-process helper profiles retain identical serialized output between
borrowed and set indexes: single-epoch SHA-256
`47b630e3d4c1d68af6e3aaf31e73ccb800e2e19e29d3d45e95ed28216357c86c`
and four-epoch SHA-256
`871f1688d5d1e00db91e402aaaa7a4e3b8401bb94e728d1498c3e7d178723301`.
Single-epoch RSS growth is 2,256,896 bytes for borrowed storage and 3,215,360
for the set reference; four-epoch growth is 2,834,432 and 3,215,360 bytes.
These one-shot observations support only this component's storage claim.
Strict all-target, all-feature Clippy and all 38 source-policy controls pass.

### F209 — Public room admission assigns both roles to one ID (#1304)

Baseline `6391c5b3`, under #826. `Room::add_player` checks only the player map;
`Room::add_spectator` checks only the spectator map. With free capacity, ordinary
calls admit the same ID to both maps in either order. Removing one role leaves
the other occupancy behind. The in-memory storage delegates both operations to
these methods under its room write lock, so concurrent requests also both win.
The service layer already checks both role registries. This is a public Rust
Room/storage contract defect; no live wire incident is established.

Three red-first controls fail on the baseline in Nextest
`c5557d1e-52a2-42ff-b78d-93a622dfd22e`. They cover direct Room calls, sequential
storage admission and opposing admission futures queued behind the room lock.
The repair checks the other role map before either insertion. Refusal leaves
member data, authority, readiness, activity clocks and session join counts
unchanged. Removing the existing role permits a later role switch.

The Rust client manifest at `da8f2d16` has no server-crate dependency. The .NET
project at `e467c871` is a standalone C# SDK. The Godot addon at `39085860` is
GDScript. None calls the server Room admission methods. The server's existing
wire role policy and messages do not change. All three clients are unaffected;
no migration is required. The broader #826 campaign remains open.

All 11 selected all-feature Room/storage controls pass in Nextest
`14d65cf1-97e9-431e-b3d5-b6a9f0dc4322`. Three existing service controls pass in
`1d4ac506-c4c3-43ce-9bea-4e7a8a67b57e`, including player/spectator refusal and
spectator admission that supersedes a pending reconnection. Independent source
review finds no further defect in these changed paths.

### Borrowed recipient count scratch (#636, #775)

The capacity oracle built a sender vector and a temporary count tree for each
recipient, then copied counts into its retained report map. It now borrows room
members and folds adjacent sender epochs into one compact list. The arrival
index sorts by recipient, sender and epoch. The final bulk-built report map,
shared sender names, roster violation order and serialized results stay intact.

An isolated counting-allocator control measures the full production `summarize`
call with raw records built before measurement. Each room has 16 players and
one send per player, with all reliable receipts present. Baseline and candidate
use the same input and compare full serialized summary digests.

| Rooms | Baseline allocations | Candidate allocations | Baseline allocated bytes | Candidate allocated bytes |
| --- | --- | --- | --- | --- |
| 100 | 25,975 | 19,575 | 14,104,744 | 12,389,544 |
| 999 | 259,212 | 195,276 | 140,773,180 | 123,638,332 |

At 999 rooms, reallocations fall from 35,964 to 19,980. Both summaries contain
6,668,621 bytes with SHA-256
`9d4f1df36f22819d6fcc89e1a41f84ceaf20afb3c208a65faa4e29735dd10156`.
Allocated-minus-freed bytes remain 13,882,580 in both versions. These results
measure cumulative allocator work, not peak memory, process RSS, throughput or
full ten-minute C3 qualification. The change runs after traffic collection and
cannot explain or repair the live generator stalls. Both campaigns stay open.

All 45 selected all-feature oracle and live relay/replay controls pass in
Nextest `6deec54f-604d-418a-b61a-90271854c36c`. A new independent set-based
control checks exact per-sender counts across epochs, duplicate receipts, zero
and maximum sequences, Unicode names, self/cross-room/unknown senders and exact
artifact replay. Existing controls retain inactive, unannounced and stale epoch
failures, closed-epoch evidence, gap violation order and unsupported-format
refusal. These checks do not complete the wider campaigns.

### F210 — Public state transitions leave room expiry stale (#1306)

Baseline `7605f2f0`, under #826. Public Room lobby entry, readiness changes,
authority changes and finalization leave `last_activity` unchanged. Their
storage counterparts also leave the monotonic expiry stamp unchanged. An
embedder can change an old room and lose it at the next cleanup. The shipped
socket router refreshes activity separately; no live socket incident is proved.

The paused-clock storage control fails on baseline at the lobby wall-clock
assertion in Nextest `1ffd3bad-ef56-4212-b422-2f2e4cede808`. The repair refreshes
both clocks under the room and liveness locks before returning a changed state.
Room methods refresh their own activity. Repeated and refused calls preserve
both clocks. Authority request/release reuses the Room transition. Finalization
keeps its exact-snapshot comparison and one-winner result. Controls check the
full renewed expiry window, eventual cleanup, direct Room state and cancellation
while the transition holds the room lock and waits for the liveness lock.

The Rust client at `da8f2d16` has no server-crate dependency. The .NET project at
`e467c871` is a standalone C# SDK. The Godot addon at `584773fc` is GDScript.
None calls these server Room/storage methods. Socket messages and their router
activity policy do not change. All three clients are unaffected; no migration
is required. The broader #826 campaign remains open.

### Lazy gap-stream failure storage (#636, #775)

Each capacity-oracle gap stream retained an inline violation count and three
optional diagnostic strings even when every report was valid. It now allocates
that diagnostic state only after the first stream violation. Exact gap coverage,
violation counts, roster-ordered first failures and raw artifact replay retain
their contracts. This changes analysis after traffic collection, not live
traffic generation.

An isolated StatsAlloc control measures the full production `summarize` call.
Each room has 16 players and one send per player. Every owed receipt is replaced
with its exact valid latest-class gap report. Raw records are built before the
measured region. Baseline and candidate use the same inputs.

| Rooms | Baseline allocated bytes | Candidate allocated bytes | Allocations in either |
| --- | --- | --- | --- |
| 100 | 12,588,200 | 9,674,432 | 11,567 |
| 999 | 125,657,410 | 96,533,194 | 115,339 |

Full serialized summaries match: at 100 rooms, 396,683 bytes with SHA-256
`5640fb64017b4e30865a0fe623d5e562d8bd1b9d46b9417d52b9bdf1ae29af49`;
at 999 rooms, 3,983,700 bytes with SHA-256
`f0a5843286e56ccbbc4946246ad4b042e964c187b0dcec46d831dd5e22c01e4c`.
At 999 rooms, retained allocated-minus-freed bytes remain 2,401,834 in both
versions. These are cumulative allocator measurements, not peak memory, RSS,
throughput or full ten-minute C3 qualification. Both campaigns remain open.

All eight selected all-feature Room/storage and game-start race controls pass
in Nextest `930a9c40-ad48-4e77-9755-8c6dd2098404`. Fifteen selected storage
cleanup, authority and clock controls also pass. All 43 source-policy controls
pass. Strict all-target, all-feature Clippy passes with zero warnings.

All 46 selected all-feature gap, replay and live relay controls pass in Nextest
`ad3fbcf9-23a6-45ba-9768-71c7bdf0b005`. They retain overlap, beyond-tail, closed
and unadopted-epoch failures, opaque payload refusal, exact first-failure order,
complete raw artifacts and identical replay. The added multi-stream control
checks both valid reports and repeated overlapping/beyond-tail failures.

### F211 — Player metadata changes leave stored room expiry stale (#1309)

Baseline `3eda6da9`, under #826. The public storage methods for member names,
peer metadata updates and peer metadata clearing change the room without
refreshing either activity clock. An embedder can update an old room and lose
it at the next inactive-room sweep. The socket router refreshes activity
separately; this finding does not establish a live socket incident.

The new paused-clock regression fails on baseline at the changed-name wall
stamp. Its cancellation control also fails: the old method completes while
the expiry lock is held. Nextest retains both failures in run
`b9481e08-e4c2-4144-802a-61e5acd9c575`. The repair acquires the room and liveness locks before
mutation, then refreshes both clocks when the stored value changes. Repeated
values, absent members and absent rooms preserve the clocks and return values.
Name updates reuse string storage. Connection metadata gains structural equality
without changing its wire format.

All three metadata controls pass after repair in Nextest
`7428799e-1b16-4da5-ba3a-5ba4f98b4954`. The controls cover each changed field,
the full renewed expiry window, eventual cleanup, unchanged and missing rows,
and cancellation while the room lock is held and the liveness lock is pending.

The Rust client at `da8f2d16` has no server-crate dependency. The .NET client at
`e467c871` is an independent C# SDK. The Godot addon at `113c0f14` is GDScript.
None calls these server storage methods. Wire messages, session selection and
socket activity throttling retain their contracts. All three clients are
unaffected; no migration is required. The broader #826 campaign stays open.

### Game-start membership scratch (#636)

Storage finalization previously allocated both sorted current and expected
member lists, then allocated the current members again for persisted readiness.
It now sorts only the caller's copied expectation and checks its length,
uniqueness and membership against the room map. Successful finalization still
retains the room's original member order in its ready list. Failed comparisons
leave the room unchanged. The caller's expectation remains unchanged.

An isolated StatsAlloc process measures 100 successful storage finalizations.
Each room has 255 members, the public storage API's u8 ceiling; this is not a
socket-configured capacity workload. Rooms and expectations are built before
the measured region. Baseline and candidate use the same shape and command.

| Measure | Baseline | Candidate |
| --- | ---: | ---: |
| Allocations | 400 | 300 |
| Allocated bytes | 1,240,000 | 832,000 |
| Deallocated bytes | 832,000 | 424,000 |
| Retained allocated-minus-freed bytes | 408,000 | 408,000 |

This measures cumulative allocator work, not peak memory or throughput. The
new control covers 1, 4 and 255 members, reversed expectations, duplicates,
missing, extra and foreign members, changed authority and state, one-winner
finalization and unchanged stored ready order. #636 remains open.

### Shared capacity roster index (#636, #775)

Delivery, churn, snapshot, latency and room-member checks now borrow one roster
index per summary. The prior implementation rebuilt three name-to-room trees,
two roster-name sets and two room-member maps across those passes. Raw records,
roster iteration order, first-failure order and artifact formats do not change.

An isolated StatsAlloc control measures the full production `summarize` call.
Each room has 16 players and one send per player. Raw records are built before
measurement. The reliable cell retains every receipt. The all-gap cell replaces
each receipt with its exact valid latest-class report. Baseline and candidate
use the same input shape and compare complete serialized summaries.

| Rooms | Cell | Baseline allocations | Candidate allocations | Baseline allocated bytes | Candidate allocated bytes |
| --- | --- | ---: | ---: | ---: | ---: |
| 100 | All-gap | 11,567 | 10,854 | 9,674,432 | 9,257,456 |
| 999 | All-gap | 115,339 | 108,341 | 96,533,194 | 92,382,346 |
| 999 | Reliable | 195,276 | 188,278 | 123,638,332 | 119,487,484 |

The 999-room all-gap summary retains 3,983,700 bytes and SHA-256
`f0a5843286e56ccbbc4946246ad4b042e964c187b0dcec46d831dd5e22c01e4c`.
The reliable summary retains 6,668,621 bytes and SHA-256
`9d4f1df36f22819d6fcc89e1a41f84ceaf20afb3c208a65faa4e29735dd10156`.
The 100-room all-gap summary retains 396,683 bytes and SHA-256
`5640fb64017b4e30865a0fe623d5e562d8bd1b9d46b9417d52b9bdf1ae29af49`.
Retained allocated-minus-freed bytes match in every pair. These are cumulative
allocator measurements after collection, not peak memory, RSS, live generator
stall repairs, throughput or ten-minute C3 qualification. Both campaigns stay
open.

### Half-close fixture portability (#1310)

Main CI run `40870634179` fails two different helper controls at `3eda6da9`.
Windows job `123437607746` receives early EOF instead of the queued server tail.
macOS job `123437607796` fails a later `SO_LINGER=0` operation with EINVAL.
Both fixtures close and drop the upstream socket before flooding client bytes
to force a forwarding error. This introduces platform TCP reset and socket
shutdown behavior into a test of independent directional pumping. The logs do
not establish the exact kernel history or a production-server defect.

The revised fixtures inject BrokenPipe through the proxy's normal destination
write-error branch while the upstream socket remains alive. The independent
reverse pump must retain the exact tail. Explicit FIN, RST and proxy-drop
controls must still release retained socket owners; Linux still requires an
observable RST. A separate real source-EOF control requires the exact tail and
normal owner release after both directions end. No live-socket option failure
is swallowed, and no reset assertion is removed.

Disabling half-close preservation makes all three controls fail. The restored
implementation passes the selected controls and the existing pause, kill,
fragmentation, reset and rate controls. Hosted Windows and macOS qualification
is still required before this issue can close.

Local validation passes all 26 selected room/session controls in Nextest
`fd1e426e-91b0-4009-b776-ce092dff8c6e`, all 59 selected oracle and live
relay/replay controls in `0a19f1f2-f6b5-4bfa-993e-98222e702f7d`, and 13 selected
proxy controls in `2c1fc691-d8bf-4195-8234-5e2c82bdcdb8`. The oracle controls
retain independent set-based receipt counts, wide latency comparisons,
malformed identities, closed and unadopted epochs, gap violation ordering,
complete raw artifacts and exact replay. All 43 source-policy controls and
strict all-target, all-feature Clippy pass.

The first hosted head `89cb671b` fails the additional indexing lint at the
new two-member duplicate check. The final comparison uses a slice pattern;
the strict hosted lint command and focused membership control pass locally.
The allocation algorithm and membership result remain unchanged.

The same head's Godot WASM released cell records one joiner wait recommendation.
Its complete artifact and measured peer reports are retained in #1312. Both
peers confirm frame 600, match all nine checksum comparisons, and send and
receive 1,327 relay messages with matching sequence counts and hashes. These
aggregates do not locate the recommendation. No cause or repair is claimed,
and its zero-recommendation gate remains in force.

Review also found that the new standalone database controls would enter the
published crate. Their module now requires the repository-test cfg, and both
build inventories and the package allowlist include its exclusion. The exact
archive inventory omits all nine repository-only modules. The build-script
checkout/archive/partial-source control and all four gated mutation controls
pass. The local packaged-unit compilation stops on a confirmed disk-full error;
its log is retained, generated package build cache is cleared, and full archive
compilation remains a hosted gate. No test or package assertion is removed.

### Room cleanup and initial-epoch analysis allocations (#636, #775)

The two storage sweeps now extract expired room rows and move their game/code
strings into index removal. They keep the same room, code, liveness and
publication lock order. Pending publications and protected rooms remain live.
The expiry predicate removes liveness only for an extracted row; the loop
removes its code and records its close reason. Neither step suspends.

An isolated StatsAlloc control creates 1,000 empty rooms before measurement.
The region covers one production cleanup call, including session close records
and the empty-sweep result vector. Baseline and candidate use identical input.

| Sweep | Baseline allocations | Candidate allocations | Baseline allocated bytes | Candidate allocated bytes |
| --- | ---: | ---: | ---: | ---: |
| Empty | 2,004 | 3 | 393,778 | 303,352 |
| Expired | 2,003 | 2 | 385,602 | 286,984 |

These are cumulative allocator counters, not peak RSS or throughput. The new
mixed-room control checks empty and occupied expiry, spectator occupancy,
protected and pending rooms, fresh rooms, all retained indexes and reuse of
identical room codes in separate games. Existing close-reason controls remain.

Capacity analysis now represents the initial `(epoch=1, start=0, no end)`
interval implicitly when no churn is recorded. Any recorded churn keeps the
materialized interval path. The registration index is built only when an
observed rejoin reaches its registration check. No raw event, output schema,
validation order or delivery rule changes. An adversarial differential control
compares implicit intervals with materialized intervals for valid, duplicate,
unknown-sender, malformed-epoch and malformed-schedule evidence in both orders.

A full-summary StatsAlloc control uses 16 players and one send per player in
each room. Raw records exist before measurement. The reliable case retains all
receipts; the all-gap case replaces each receipt with its exact valid report.
The synthetic input has no churn or registry rows.

| Rooms | Cell | Baseline allocations | Candidate allocations | Baseline allocated bytes | Candidate allocated bytes |
| --- | --- | ---: | ---: | ---: | ---: |
| 100 | All-gap | 10,854 | 10,705 | 9,257,456 | 9,022,640 |
| 999 | All-gap | 108,341 | 106,883 | 92,382,346 | 90,044,010 |
| 999 | Reliable | 188,278 | 186,820 | 119,487,484 | 117,149,148 |

Complete summary lengths and SHA-256 values remain identical to the preceding
shared-roster measurements. Retained allocated-minus-freed bytes also match.
The 999-room reduction is 2,338,336 allocated bytes per summary. This does not
qualify the ten-minute C3 cell, bound peak generator memory or diagnose live
stalls. #636 and #775 remain open.

### Bounded Godot WASM wait observations (#1312)

The released fixture retains the first 64 consumed wait events and counts
later events. It records callback/poll identity, elapsed time, callback spacing,
remote-poll duration, pre/post-poll frames, confirmation, frame advantage, both
peer lag estimates, requested skips and relay state. Consumption occurs after
remote polling; generation can occur during the prior frame advance. Lag
estimates are not synchronized remote clock measurements.

The bounded-storage control checks event order, serialization and overflow.
Harness controls reject excess records, invalid counts, mismatched callback
identity and invalid skip values. The existing zero-wait health gate is
unchanged. The original attempt and released graph remain retained in #1312.
No causal repair is claimed from these diagnostics or from a passing rerun.
Matched runtime evidence is still required before that issue can close.

### F212 — Public Room finalization accepts stale ready IDs (#1315)

Baseline `e627cc77`, under #826. `Room::all_players_ready` compares only the
ready-list and player-map lengths. With two current members, a list containing
the first ID twice, or the first ID and an absent ID, passes while the second
member is unready. A complete ID list also passes with a member marked
unready. `Room::finalize_game` then commits the game start. Both collections are
public. This is a public Rust Room contract defect; no live socket incident
is established. The service coordinator checks readiness separately.

The red-first control fails at the first duplicate-list assertion in Nextest
`19dc9005-7141-4754-9f1e-cccb705fab60`. The repair requires a nonempty membership, equal list
and map lengths, and a true ready flag plus ready-list membership for every
current member. Equal lengths and full membership exclude duplicate and absent
IDs without allocating another set. Order and unused room capacity do not
change the result. Refusal leaves state and timestamps unchanged.

The control covers duplicate and absent IDs, inconsistent flags, reordered
valid IDs, empty membership and one-time finalization. Ten focused Room tests
pass. Five selected Room and service controls also pass, including concurrent
join/start and unready/start histories. This does not complete #826.

The Rust client manifest at `da8f2d16` has no server-crate dependency. The .NET
SDK at `e467c871` is an independent C# project. The Godot addon at `ec1740df`
uses GDScript. None calls this server
Room API. Wire messages, the service readiness coordinator and client migration
requirements do not change. All three external clients are unaffected.

### Deferred sparse recipient histograms (#636, #775)

The capacity oracle now retains up to 16 exact latency samples inline per
recipient. A seventeenth sample promotes that recipient to the existing
adaptive histogram. Sparse recipients reuse one scratch histogram when the
oracle emits statistics. HDR bucket rounding, percentiles and raw maximums use
the same histogram implementation. Raw records and replay formats are unchanged.

An isolated process builds 15,984 recipient states with 15 samples each, from
4,096 to 61,440 microseconds. Baseline retains 114,573,312 bucket bytes and
reports RSS 141,156,352 bytes after recording. The candidate retains zero bucket
bytes in recipient states and reports RSS 9,117,696 bytes at the same point.
Each recipient state grows from 112 to 144 bytes. All per-recipient statistics
match an independent wide-counter HDR accumulator. RSS is one local process
observation, not a portable bound or whole-run peak measurement.

Sparse storage is bounded. Busy recipients keep the existing adaptive bucket
promotion and incur the 32-byte larger enum state. The aggregate histogram
remains wide. This analysis change does not repair live generator stalls or
prove the planned ten-minute C3 workload. Both campaigns remain open.

A separate isolated StatsAlloc control runs the full production `summarize`
function with one reliable send per player and all receipts present. Each room
has 16 players. Raw records are built before measurement.

| Rooms | Baseline allocations | Candidate allocations | Baseline allocated bytes | Candidate allocated bytes |
| --- | ---: | ---: | ---: | ---: |
| 16 | 2,990 | 2,735 | 1,890,192 | 1,381,328 |
| 999 | 185,821 | 169,838 | 116,922,796 | 85,007,308 |

The 999-room serialized summary has 6,668,621 bytes and SHA-256
`9d4f1df36f22819d6fcc89e1a41f84ceaf20afb3c208a65faa4e29735dd10156`
in both versions. Exact all-gap summaries also match at 16 and 999 rooms, with
unchanged allocation totals. At 999 rooms, retained allocated-minus-freed bytes
remain 13,882,580 for reliable and 2,401,834 for all-gap. These cumulative
allocator results do not measure peak memory or throughput.

Six final all-feature latency controls pass in Nextest
`df087df1-937b-4c1b-8506-01a4bbe8f310`. They cover sparse-to-histogram promotion,
zero and maximum-width samples, hot-bucket counter widening, exact percentiles,
raw maxima and reused scratch across sparse, dense and empty recipients.
Twenty-nine source-policy controls and strict all-target, all-feature Clippy
also pass. The allocation and memory controls above are local measurements.

PR review found that one extreme sparse sample could leave the shared scratch
with 56,320 counters, all cleared again for each later small recipient. The
new range control fails on that retained size before repair. The oracle now
discards a range over four times the next recipient's maximum, with a floor at
the three-digit HDR exact-value ceiling. After a maximum-width sample, 1,000
small-recipient statistics calls retain only the minimum 2,048 counters and
preserve exact results. No elapsed-time speedup is claimed. The review also
adds full state-preservation assertions for invalid IDs after all member flags
are ready.

### F213 — Public Room authority changes retain stale member flags (#1317)

Baseline `b69eb24d`, under #826. A restored public `Room` can have member
`is_authority` flags that disagree with `authority_player`. Assignment updates
only the previous and selected records. Clearing an unselected authority leaves
stale true flags unchanged. Selecting a member can leave other members marked
as authorities. This is a public Rust Room contract defect; no live WebSocket
incident is established. Normal service transitions already maintain consistent
flags.

The first control fails when clearing `None` with three stale true flags in
Nextest `4f9e6266-d4fb-4d9a-9f3b-23382d30813e`. The repair derives every member's
flag from the accepted selection. An absent target or unsupported room refuses
without mutation. Changed selection or flags refresh activity; a consistent
repeated call does not. The control covers absent, current and departed previous
selections, clearance, assignment, repetition and refusal.

Storage needs the same change result for its monotonic expiry clock. Keeping
its old selected-ID-only comparison fails the paused-clock control at
`repaired flags must renew monotonic liveness`, even after the Room repair.
The shared transition now returns an internal acceptance/change result, so
storage renews both clocks on flag repair. A second consistent call preserves
both clocks and permits the inactive room to expire.

The external Rust client manifest has no server-crate dependency. The .NET SDK
is an independent C# project, and the Godot plugin uses its own client code.
None calls this Rust Room API. Wire fields, normal service authority outcomes
and client migration requirements do not change. All three are unaffected.
The wider #826 campaign remains open.

### Shared summary identities and count scratch (#636, #775)

The capacity oracle now shares each retained summary sender name with that
sender's raw send ledger. The summary owns its shared references and remains
valid after raw records are dropped. A sender with receipts but no sends keeps
its original receipt counts and misroute evidence through an owned fallback name.
One count vector retains capacity across recipient summaries; each drain removes
all entries before the next recipient. Counts, sender ordering and artifact
formats do not change.

An isolated StatsAlloc control runs the full production `summarize` function
with one reliable send per player and all receipts present. Each room has 16
players. Raw records are built before measurement. Baseline is `b69eb24d`.

| Rooms | Baseline allocations | Candidate allocations | Baseline allocated bytes | Candidate allocated bytes |
| --- | ---: | ---: | ---: | ---: |
| 16 | 2,735 | 2,224 | 1,381,328 | 1,339,512 |
| 999 | 169,838 | 137,871 | 85,007,308 | 82,350,090 |

At 999 rooms, allocated-minus-freed bytes fall from 13,882,580 to 13,143,082,
with raw records still alive in both versions. The complete 6,668,621-byte JSON
summary retains SHA-256
`9d4f1df36f22819d6fcc89e1a41f84ceaf20afb3c208a65faa4e29735dd10156`.
The 16-room JSON also matches. Exact all-gap summaries and allocation totals
remain unchanged at both room counts. These are cumulative allocator and retained
summary measurements, not peak-memory or throughput results. No generator stall
or ten-minute C3 qualification is claimed. Both campaigns remain open.

Twelve selected Room/storage controls pass at the repaired source, including the
paused-clock control. Thirty selected capacity/replay controls pass across the
final local checks, including reliable relay, lossy pressure, malformed ledgers,
missing-send fallback, shared identity lifetime and independent epoch counts.
Thirty-eight source-policy controls, strict all-target/all-feature Clippy,
Markdown, CI configuration and document consistency checks also pass locally.
Hosted qualification remains separate from these local measurements.

### Held Godot saturation setup timeout (#1319)

PR #1318 head `73ee1796` fails the native active-consumer control in run
`40999023767`, job `123878821334`. The first cell resumes all 2,048 exact
reliable 4 KiB packets with no rejection, closure or remaining sender debt.
The next setup times out waiting for actual SCTP receive-credit refusal,
before callback resume. This differs from #1264's original terminal-event
failure. The original timeout cannot locate send admission or transport progress.

The helper now retains the completed send count, payload generation, packet
size and last transport snapshot on timeout. Snapshot sampling keeps its existing
bounded wait and partial-result markers. The 15-second setup bound, all 2,048
packets, receive-refusal precondition, receive limits, exact reliable checks,
offer roles and fresh-link controls remain unchanged. This adds diagnostic
evidence; it does not repair a confirmed production defect.

The instrumented local single-test control passes all four cells in 18.27
seconds. A four-thread control passes all 11 Godot profile tests in 28.03
seconds. Its source SHA-256 is
`9a142560e89f17db6da5906324b48306f32eb70163f59e24e05ad3989f2f121c`.
Strict native all-target Clippy passes. These native/native controls do not
explain the hosted timeout, #1264's terminal event or #1244's historical Godot
prefix loss. All three issues remain open; no attempt or original failure is
removed from the evidence.

A second hosted attempt at `7d585a07` fails the sibling raw-channel control in
run `41000313181`, job `123882945060`. Both receiver-answerer generations drain
all 2,048 exact packets. The next held receive-refusal wait times out before
raw-channel polling resumes. That sibling requested cancellation and dropped
the callback handles without observing completion. Cancellation is cooperative,
so this did not prove its queues were held before sending.

A new controlled handle history fails with the old abort-only hold step:
`an abort request does not establish a held callback queue`. It keeps two
pollers unfinished, then finishes each separately. The shared fixture hold step
now waits for every poller to finish, under the existing five-second cancellation
bound, before either saturation workload sends. The control passes after repair.
The shared Engine-resume setup already waited for completion; this repair alone
does not explain its earlier timeout. #1319 remains open.

Both held waits now retain their last snapshot on timeout. As PR review notes,
a bounded transport snapshot can omit SCTP fields. An absent refusal count now
continues waiting with the incomplete snapshot retained, rather than panicking
before diagnostics are saved. Success still requires an observed positive
refusal count. Offered work, receive limits, receive-refusal deadlines and exact
reliable receipt checks do not change.

Final review also identified a send-phase evidence gap in the sibling control.
Both saturation send paths now keep completed-send counts and capture a bounded
receiver snapshot after a timeout. `timeout_snapshot` is a post-timeout sample;
it does not prove transport state at the deadline. The shared helper also keeps
its last in-budget snapshot. Success deadlines and exact receipt requirements
remain unchanged; only failure reporting adds the existing bounded snapshot wait.

### F214 — Repeated public Room readiness retains duplicate entries (#1320)

Baseline `6ec4f9ab`, A08/A18, under #826. A public Rust `Room` can carry a
restored ready list with duplicate member IDs. `set_player_ready(id, true)`
accepts the call but leaves those duplicates. Exact readiness stays false and
`finalize_game` refuses. A separate list-only repair leaves `last_activity`
unchanged when the member flag already matches the request. No live socket
failure is established; normal service transitions already keep this list
consistent.

Both original-source controls fail. The repeated-readiness history checks two
ready members with interleaved duplicates. Each accepted call repairs only its
member's entries, preserves peer order, and allows finalization after both
members are repaired. The 16-case table covers flags, zero through three list
copies, ready/unready requests, activity, repetition, absent IDs and finalized
refusals. The repair keeps the first matching entry for a ready member and
removes all matching entries for an unready member. A changed flag or list
renews activity. Consistent calls and refusals preserve activity.

The Rust client manifest at `da8f2d16` has no server-crate dependency. The .NET
SDK at `e467c871` is an independent C# project. The Godot plugin at `1ad5ff97`
uses its own GDScript protocol client; its native smoke fixture imports server
ICE and player-ID types but does not call `Room::set_player_ready`. None of the
three clients calls this helper. Wire fields, normal service readiness and
migration requirements do not change. All three clients are unaffected. The
wider #826 campaign remains open.

### Recipient-grouped stream storage (#636, #775)

Baseline `6ec4f9ab`. The oracle stored a recipient name in every arrival and
gap-stream tree key. It now keeps one recipient key and a sorted vector of
sender/epoch entries per recipient. The initial vector reserves the ordinary
room's peer count; additional epochs and malformed foreign senders remain
observable and can grow it. Binary search preserves exact keys and first-failure
order. Remaining entries keep their existing lifetime through the recipient pass,
including malformed repeated roster rows. No raw event, offered send, receipt, gap, replay check or verdict is removed.

A 16-player control compares complete reliable and all-gap summaries at 16 and
999 rooms. Each sender offers one measured send; all 240 recipient obligations
per room are retained. Fresh processes use the instrumented system allocator
only around `summarize`. The unchanged fixture and serialized summary hashes
compare the original flat tree with the final vector implementation.

| Rooms | Contract | Allocations before / after | Allocated bytes before / after |
| --- | --- | --- | --- |
| 16 | Reliable | 2,224 / 1,930 | 1,339,512 / 963,912 |
| 999 | Reliable | 137,871 / 119,405 | 82,350,090 / 58,785,610 |
| 16 | All-gap latest | 1,708 / 1,120 | 1,454,448 / 850,416 |
| 999 | All-gap latest | 105,884 / 68,952 | 89,817,658 / 51,914,306 |

The 999-room reliable JSON has 6,668,621 bytes and SHA-256
`9d4f1df36f22819d6fcc89e1a41f84ceaf20afb3c208a65faa4e29735dd10156`.
The all-gap JSON has 3,983,700 bytes and SHA-256
`f0a5843286e56ccbbc4946246ad4b042e964c187b0dcec46d831dd5e22c01e4c`.
All four complete hashes match. Retained allocated-minus-freed summary bytes
also match: 13,143,082 for reliable and 2,401,834 for all-gap at 999 rooms.
These are cumulative allocation and retained-summary measurements, not
whole-run peak memory, elapsed-time or throughput claims.

A rejected recipient-grouped tree increases allocations and bytes. Its
16-room all-gap case grows from 1,708 to 2,144 allocations and from 1,454,448
to 1,624,560 bytes. The final implementation uses vectors instead. The independent
flat-map control retains empty, prefix-related and Unicode names, epoch zero,
maximum epochs, reverse insertion, sender ranges and partial removals. Existing
replay and negative controls still validate duplicate, ordering, missing,
misroute, opaque, gap and churn histories. The broad campaigns remain open;
this does not qualify the planned ten-minute C3 workload.

PR review also identified an older standalone arrival-state profile that still
used numeric flat-tree keys. It now defaults to the actual recipient vectors
and inserts through `stream_entry`, with shared string fixtures allocated
before timing. `CAPACITY_ARRIVAL_PROBE_FLAT_REFERENCE=1` retains an explicit
flat-tree comparison with the same string keys. Both modes check the full
stream count and exact unique-arrival total. Their retained RSS includes the
name fixture and process state; it is separate from the allocator results above.

### Player-name comparison qualification (#826, #636)

Baseline `8a889d99`, A08/A18. Room admission normalized the proposed name and
all compared member names, even when both names were ASCII. ASCII pairs now
use case-insensitive byte comparison. The first Unicode pair creates the
proposed name's canonical key once; Unicode pairs retain the existing NFD,
full case-fold, NFD comparison. Empty rosters need no key.

The qualification compares all 24,649 pairs in a 157-entry corpus against the
canonical-key reference. It includes every ASCII byte, empty names, sharp-s,
long-s, Kelvin and Angstrom signs, composed/decomposed Latin and Hangul,
Greek sigma and combining marks, Turkish I variants, full-width letters and
CJK. Mixed rooms also check both collisions and a distinct proposed name.
The comparator is tested separately from the name-character admission rules.
The existing Unicode 17 and canonical-equivalence controls pass. This is a
comparison-path qualification, not a new defect or a complete Unicode audit.

An isolated instrumented system allocator measures new ASCII-name checks:

| Existing members | Allocations before / after | Reallocations before / after | Allocated bytes before / after |
| --- | --- | --- | --- |
| 0 | 3 / 0 | 4 / 0 | 48 / 0 |
| 1 | 6 / 0 | 5 / 0 | 72 / 0 |
| 16 | 51 / 0 | 20 / 0 | 432 / 0 |
| 64 | 195 / 0 | 68 / 0 | 1,584 / 0 |
| 255 | 768 / 0 | 724 / 0 | 9,888 / 0 |

The allocation regression fails on the original implementation. The repair
allocates only the existing error string for an ASCII collision. These are
allocation counts, not room-join latency or throughput measurements. The
Rust, .NET and Godot clients need no change: name decisions, error text,
messages, defaults and ordering remain the same.

### Consumed capacity recipient storage (#775, #636)

Baseline `8a889d99`. Removing the last sender entry did not free its recipient
vector's capacity. The oracle retained those empty vectors while building the
owned summary. It now removes each completed recipient's arrival and gap
entries when roster names are unique. Repeated roster names keep the original
lifetime, so foreign, closed and otherwise unconsumed evidence remains visible
to later rows. No raw record or delivery check is removed.

Fresh processes run the same 16-player reliable, all-gap latest and malformed
zero-sequence controls at 16 and 999 rooms. Each sender offers one measured
send. The system allocator records cumulative allocations and frees. Live
allocated bytes are sampled before the recipient loop and after each recipient;
the table gives the largest sampled increase over the pre-summary live total.
These samples exclude retained inputs and are not an allocation-by-allocation
peak, RSS measurement or whole-run memory bound.

| Rooms | Contract | Sampled live bytes before / after |
| --- | --- | --- |
| 16 | Reliable | 727,392 / 542,420 |
| 999 | Reliable | 45,263,402 / 33,688,460 |
| 16 | All-gap latest | 745,448 / 744,232 |
| 999 | All-gap latest | 46,387,154 / 46,287,016 |
| 16 | Invalid reliable | 728,047 / 726,328 |
| 999 | Invalid reliable | 45,264,254 / 45,196,528 |

All six complete JSON hashes, cumulative allocation totals and retained summary
bytes match the baseline. At 999 rooms, reliable summary SHA-256 is
`9d4f1df36f22819d6fcc89e1a41f84ceaf20afb3c208a65faa4e29735dd10156`;
all-gap is `f0a5843286e56ccbbc4946246ad4b042e964c187b0dcec46d831dd5e22c01e4c`;
invalid reliable is
`113cc09bf3f4539a588137379ec40584eea7ae1f96c27f3bba2debb7ab4b37b6`.
A repeated-roster control preserves two exact foreign-stream violations and
complete artifact replay; unconditional recipient release fails that control.
The scoped oracle controls retain missing, duplicate, order, gap, opaque and
churn checks. The broader campaigns remain open. This does not qualify the
ten-minute C3 workload or prove a throughput gain.

### Retained stdout reader qualification (#1290)

Baseline `8a889d99` already contains a Linux/current-runtime extra-reader
control. It opens `/proc/self/fd`, reads the authenticated event through the
retained reader, checks immediate process liveness, then closes that reader
and verifies code-3 exit on the next output write. That mechanism is existing
coverage, not a discovery from this session.

The replacement uses `std::io::pipe` and `PipeReader::try_clone`, removes the
Linux-only gate, and runs both current-thread and multi-thread CLI runtimes.
It creates exactly two parent readers. It reads `connected`, closes that
reader, releases authentication, and reads `authenticated` through the
retained reader. That read proves a write succeeded after primary-reader
closure. The regular control observes 100 ms of retained-reader liveness,
then closes the last reader and releases `ProtocolInfo` to cause another
output write. It retains the original eight-second final-exit bound and
code-3 requirement.

An ignored `retained_stdout_reader_survives_the_original_exit_window`
qualification uses the same helper with the full historical eight-second
retained-reader window in both runtimes. The hard watchdog remains 90 seconds;
the handshake bound remains 20 seconds. This keeps routine checks short and
retains an executable exact-window experiment. Child stderr is drained with
bounded retention and included in terminal assertion failures.

The consolidated local run passes the routine two-runtime control in 0.404
seconds, the ignored exact-window qualification in 16.292 seconds, and the
existing isolated `closed_stdout_fails_held_handshake` in 0.157 seconds. All
three run together with the exact-window test explicitly enabled.

The new scope is broader runtime coverage, a platform-independent pipe fixture
and explicit original-window qualification. It does not establish temporary
reader inheritance in the historical hosted failure, exclude an unrelated
writer-notification or teardown defect there, or repair a production defect.
Keep #1290 open for the original parallel-versus-isolated ownership
investigation. No original output deadline or exit contract is relaxed.

### F215 — Capacity receipts can precede their scheduled sends (#1323)

Confirmed on `4cecf3b6` under #826 and #775. A complete ledger stays valid after
one receipt's `received_us` becomes `intended_us - 1`. The latency calculation
saturates that difference to zero. Send-completion validation from #1162 does
not validate receipt times.

The repair checks each matched receipt in the existing raw-event pass. Matching
uses sender, epoch and ledger sequence. Cross-epoch duplicate records cannot lend
another incarnation's intended time. Other consumers keep their global
sender/sequence last-key-wins lookup. An `InvalidReceiptTimes` reason retains the count and first exact recipient,
sender, epoch, server sequence, intended time and observed time. Raw receipts,
byte totals, latency samples and existing delivery violations remain visible.
Unmatched receipts retain their existing misroute handling. No new index or
per-receipt storage is added. Ordered, compact-reordered and wide lookup controls
cover unique and repeated keys, missing epochs and raw-order ties. The original
time lookup fails the new cross-epoch control.

The red-first control fails on the baseline. Warm-up and measured controls cover
one microsecond before, equal to, and one, five and eight microseconds after the
intended send. Equal times pass. A receipt before the send future completes may
also pass: the remote consumer can run before the sender returns. Early duplicate
and foreign-recipient evidence keeps its other violations. JSONL replay produces
the same full summary. These are internal evidence checks; no server timing defect
or cause of the historical generator stalls is established.

### Ready-toggle snapshot allocation (#636)

Baseline `4cecf3b6`. `handle_player_ready` clones a complete room to read its lobby
state, then reads its member list separately. The complete snapshot copies
spectator names even though the toggle does not use them. It also clones the
ready list before publishing it and discards the retained original.

`GameDatabase::get_room_lobby_state` has a compatible default over the complete
snapshot. The in-memory backend projects the field under its existing read lock.
Ready toggles use that projection and move the ready list into the publication.
Membership reads, room gates, validation, event fields and publication order stay
the same. Pending, missing, finalized and failed-read controls preserve the
snapshot contract. Existing database adapters can keep using their snapshot read.

An isolated instrumented system allocator measures the real coordinator with one
routed member and zero, 16 or 255 stored spectators. Each case warms the room lane
with two toggles, then measures ready-on and ready-off through queue publication.

| Spectators | Ready-on allocations before / after | Ready-off allocations before / after | Ready-on allocated bytes before / after |
| --- | --- | --- | --- |
| 0 | 41 / 33 | 39 / 32 | 8,187 / 6,891 |
| 16 | 58 / 33 | 56 / 32 | 10,697 / 6,891 |
| 255 | 297 / 33 | 295 / 32 | 48,521 / 6,891 |

The new growth control fails against the original ready-toggle implementation.
Direct projected reads allocate only their async result: one allocation and 120
bytes in this build, compared with 264 allocations and 41,302 bytes for a complete
255-spectator room snapshot. These are allocation counts, not throughput or
latency gains. Rust, .NET and Godot clients need no changes: wire behavior,
validation, ordering, defaults and reconnect rules stay the same.

### Unused collected record storage (#775, #636)

Baseline `4cecf3b6`. The runner reserves the complete planned send and receipt
vectors before collection to avoid growth during the timed workload. Partial
collection leaves that reservation alive throughout analysis and artifact output.
After all producers stop, `take_records` now releases unused vector capacity
before sorting sends and running the oracle. Receipt order and every retained
raw event stay unchanged. Empty reservations are freed directly. Partial nonempty
vectors move only after a fallible replacement reservation succeeds and reduces
capacity. Failed reservations keep the original records. Fully used vectors do
not allocate. The collection reservation and admission budgets remain.

The isolated `generator_collected_record_storage_profile` offers one send per
sender with 16 players per room. It measures vector capacity times row size
before and after collection. The retained receipt percentages are deliberate
prefix controls, not claims of healthy delivery. Each control compares all raw
records and complete summary JSON against its unmodified prefix.

| Rooms | Retained receipts | Raw-row bytes before / after |
| --- | --- | --- |
| 16 | 0% | 262,144 / 16,384 |
| 16 | 1% | 262,144 / 18,816 |
| 16 | 100% | 262,144 / 262,144 |
| 999 | 0% | 16,367,616 / 1,022,976 |
| 999 | 1% | 16,367,616 / 1,176,384 |
| 999 | 100% | 16,367,616 / 16,367,616 |

All six complete summaries and raw records match their controls. At 999 rooms,
the complete reliable summary retains SHA-256
`9d4f1df36f22819d6fcc89e1a41f84ceaf20afb3c208a65faa4e29735dd10156`.
The ordinary regression also checks zero, one, half and complete collection and
exact artifact replay. This measures retained raw-row storage, not RSS or peak
allocator use. Nonempty compaction temporarily holds replacement storage after
the workload. It does not reduce
the collection peak, establish a whole-run memory bound or qualify the largest
ten-minute C3 cell. Keep #775 and the broader #636/#826 campaigns open.

The first hosted session head, `aa7a2977`, also exposes a synthetic fixture with
receipts dated at zero against sends planned after 100 ms. The failed-write drain
control advances its paused clock before receiving those frames. It retains the
same three receipts, six terminal cases and one-second drain window. Its exact
prefix, missing-hole and replay checks pass without weakening receipt-time
validation. Nonempty-capacity assertions now check non-increase; empty collection
still requires zero capacity. The storage profile records the measured reduction
without requiring every allocator to return exact vector capacity. Failure,
insufficient capacity and oversized replacement controls keep the original
records and buffer. Empty and fully used vectors do not attempt a reservation.

### A08/A10 — Spectator baseline and lobby event order

Baseline `538dfe1b`, under #826. A new four-cell history runs the actual server,
in-memory database, message coordinator and client queues. One ready member
races spectator admission with either ready-off or game start. Each operation
wins the held room gate in turn. The test polls the owned spectator admission
body with its connection lifecycle guard retained, so wrapper-task scheduling
does not choose the gate order.

Before gate release, no spectator row or client frame may appear. After release,
the spectator baseline contains exactly the member and spectator. Its ready flag
and lobby state match the winning operation. A spectator admitted first receives
exactly the later readiness or game-start event after its baseline. An admission
that follows the change receives only the updated baseline. The member receives
the spectator notification and lobby change in gate order. Game-start receipts
contain exactly the seated member; only that member receives one session plan.

An incorrect initial test expectation rejected a live spectator update. Source
inspection confirmed that current room routing includes spectators; the test
now requires the exact update instead of excluding it. No production defect is
inferred from that fixture error.

The negative control changes only spectator admission's room-gate key to an
unrelated room lane. Nextest `ded1c0c9-88fa-4e32-9a8b-b8511cfd071b` fails before
release because a spectator row has already appeared. The original source is
restored byte-for-byte. Nextest `b25e3875-0f0e-4399-8e3d-a8e8a97581bc` passes all
four restored-source cells in 0.027 seconds. The unchanged gate therefore prevents
a demonstrated premature admission, rather than merely coinciding with a passing
receipt check.

This qualifies four local queue histories for one seated member and one
spectator. It does not exercise socket transport, multiple member changes,
process cancellation, storage failures, or the spawned admission wrapper.
No server behavior or client wire format changes. Rust, .NET and Godot clients
need no changes. A08, A10 and the broader #826 campaign remain open.

### Spectator admission snapshot copies (#636)

Baseline `538dfe1b`. Spectator admission copies the fresh room, its player map,
and spectator names after acquiring the room gate. It also copies the roster
again for the member notification. The replacement consumes the owned room's
maps and strings. Only the separate baseline roster needs a copy. The original
database row, routing checks, readiness derivation, publication order, replay
and rollback rules stay the same.

An isolated instrumented allocator measures the warmed actual server admission.
The control checks the exact room identity and member, and compares the complete
spectator roster in the baseline with the member notification. Each shape adds
one spectator to zero, 16 or 254 stored spectators.

| Existing spectators | Allocations before / after | Allocated bytes before / after |
| --- | --- | --- |
| 0 | 173 / 156 | 26,835 / 24,969 |
| 16 | 301 / 236 | 40,519 / 35,359 |
| 254 | 2,205 / 1,426 | 272,167 / 212,465 |

The allocation-growth ceiling fails on the original implementation in Nextest
`3d0bd60f-c52d-4dd9-8ead-60e8f7a95515` and passes after the ownership change in
`a70ba8b9-dce7-40b9-9cd9-2133a7937f65`. Growth falls
from eight to five allocations per stored spectator. These are allocation
counts and allocated bytes, not throughput, latency or peak RSS measurements.
Rust, .NET and Godot clients need no changes: messages, membership, readiness,
defaults and event order stay the same. The broader #636 campaign remains open.

### Sparse recipient stream reservations (#775, #636)

Baseline `538dfe1b`. On the first observed arrival or gap, the capacity oracle
reserves every co-room sender slot for that recipient. A recipient with only
one observed sender retains the same reservation as a complete room. The
replacement reserves one slot first and the planned fan-out only on the second
distinct sender/epoch. Repeated observations reuse the slot. Additional epochs
and unexpected senders still grow the vector and retain their exact evidence.
The existing sorted-key insertion and validation rules stay the same.

Final review found that the first helper version evaluated the room fan-out
argument on every observation. The final helper takes a deferred query and
calls it only for the second distinct stream's reservation. A counter control
rejects eager evaluation and requires zero queries for the first stream and its
repeats, then exactly one across subsequent observations. The isolated storage
profile does not measure caller lookup work; it cannot establish this property.

The storage regression fails with the original reservation in Nextest
`1e6b6d14` and passes in `c5ff1e36`. Four 16-player controls combine sparse or
complete receipts with valid or duplicate gaps. Every raw record and complete
summary survives artifact replay. All four summary SHA-256 values match the
original implementation. Controls also retain repeated and reverse-order keys,
removal and more epochs than the planned room fan-out.

The existing isolated arrival-state profile compares both reservation methods
in fresh processes. Each of 999 rooms has 16 recipients and 100 observations
per stream. Three paired attempts are retained for each shape.

| Observed senders per recipient | Stream slots in bytes before / after | Buffer allocations before / after | Buffer growths before / after |
| --- | --- | --- | --- |
| 1 | 24,935,040 / 1,662,336 | 15,984 / 15,984 | 0 / 0 |
| 15 | 24,935,040 / 24,935,040 | 15,984 / 15,984 | 0 / 15,984 |

Sparse retained stream storage falls by 93.3 percent. Sparse process RSS ranges
from 36,880,384–36,892,672 bytes before and 13,684,736–13,815,808 bytes after.
Dense construction takes 2.472–2.719 seconds before and 2.259–2.642 seconds after;
these overlapping observations establish no speed gain. Dense recipients pay
one additional buffer growth each. This bounds one analysis index for the
measured shapes. It does not bound total runner memory, reduce collection
storage or qualify the ten-minute C3 cell. Keep #775 and #636 open.

### Lobby promotion state projection (#636)

Baseline `92842406`. Lobby promotion reads a full owned room only to check its
state. The replacement uses the existing `get_room_lobby_state` projection.
The default backend path still delegates to the complete snapshot. The in-memory
path retains pending-row and read-failure semantics without copying member maps,
spectator names or room strings. Membership reads, ready-state derivation,
mutation ownership and publication order stay the same.

An isolated allocator measures actual coordinator promotion after a ready toggle
warms its event lane. Each shape retains one ready member and zero, 16 or 255
spectators. The control requires the exact ready lobby event, durable Lobby state,
unchanged spectator count, and a repeated promotion with no extra event.

| Spectators | Allocations before / after | Allocated bytes before / after |
| --- | --- | --- |
| 0 | 43 / 36 | 8,379 / 7,527 |
| 16 | 60 / 36 | 10,889 / 7,527 |
| 255 | 299 / 36 | 48,713 / 7,527 |

The growth ceiling fails on the baseline in Nextest `0ad3591f` and the retained
repeat `8023ceed`. The repaired implementation passes in Nextest `7f5e6aa7`.
These are allocation counts and allocated bytes, not latency, throughput or RSS.
The existing projected-read failure, pending-row and finalized-state controls
remain applicable. Rust, .NET and Godot clients need no changes: protocol,
membership, defaults and event order stay the same. Keep #636 open.

### Compact scheduled-send disconnect index (#775, #636)

Baseline `92842406`. Scheduled-send validation stores and sorts a pointer for
every disconnect while reconstructing active send epochs. The replacement counts
those rows first, reserves exactly their offset count, and uses four-byte raw
offsets when the full history fits. Larger histories retain the usize fallback.
The shared room-churn offset container keeps its existing timestamp/raw-offset
order; disconnect validation sorts by recipient and timestamp. Equal disconnect
keys have the same effect on epoch end times, independent of their raw tie order.
No raw event changes, and every disconnect identity remains present once.

Ordinary controls compare compact and wide indexes with an independent stable
sort across empty, sparse, reversed and tied histories with malformed epochs.
They check the complete offset set, sorted semantic keys and retained storage.
Existing controls for active incarnations, delayed repeated churn, invalid sends
and exact artifact replay pass on the replacement. The one-million-row profile retains 333,334
disconnects and checks the same recipient/time digest, `12681057109706136158`,
for the original pointer implementation and replacement in separate processes.

| Index | Retained buffer bytes | Three construction times in milliseconds |
| --- | --- | --- |
| Original pointers | 4,194,304 | 786, 786, 674 |
| Compact offsets | 1,333,336 | 1,012, 845, 783 |

Retained index storage falls by 68.2 percent on this 64-bit host. The compact
index takes longer to construct in these debug-profile attempts; no speed gain
is claimed. Earlier diagnostic attempts with raw-offset tie sorting are retained
locally and are not included in this final-source table. The index is built after
collection, outside offered gameplay. This is one analysis buffer, not a whole-run
memory bound, peak RSS measurement or qualification of the ten-minute C3 cell.
Keep #775 and #636 open.

### A08/A10 — Spectator departure and lobby publication order

Baseline `92842406`, under #826. Four new histories race the owned spectator
detach body against ready-off or game start, placing each operation first on the
held room gate. The fixture retains the spectator lifecycle guard. Before gate
release, the spectator must remain stored and locally routed, and both client
queues must stay empty. After release, the exact member events follow gate order.
The spectator receives the lobby change only when that change won the gate,
then exactly one departure acknowledgement with an empty roster. The final room
retains its member, correct lobby state, no spectator role, and no spectator row.
Game start publishes one member-only peer list and one member session plan.

The negative control changes only detach's room-gate key to an unrelated lane.
Nextest `63debed4` fails before release because the spectator row is already
removed. Production source is restored byte-for-byte. The restored source and
shared fixture pass both admission and departure histories in Nextest `311362c2`.
The existing ready-before-lobby and cancelled-promotion-after-commit controls also
pass. These are local queue histories, not socket, replay, storage-failure or
spawned-detach-wrapper qualification. No production defect is inferred.
Rust, .NET and Godot clients need no changes: runtime behavior and wire format
stay the same. Keep A08, A10 and #826 open.

The first hosted head `d018da94` fails the test-read policy scanner at seven new
read sites, including the member-drain loop (CI run `41172572497`, job
`124453951203`). The fix names each result, checks Empty explicitly and treats
a disconnected member queue as a failure. The scanner itself is unchanged.
Nextest `dafd6b25` passes the focused scanner; `176d240c` and `bf348be9` pass the
same departure history and lobby allocation contract after the read changes.

Review of `2c16c03d` also identifies that Pending alone does not prove where an
operation waits. Both ordering histories now poll with distinct wake observers
and disable cooperative yields during that poll. Before either operation is
polled again, releasing the held gate must wake the expected first operation
and must not wake its competitor. This proves FIFO permit assignment at gate
release, rather than inferring it from Pending or `join!` poll order. A negative
control stalls the expected first operation before the gate; the same probe must
reject that false synchronization. All eight admission/departure cells and this
negative control pass in Nextest `56c067c2`. No runtime code changes.

### F216 — Coordinator publishes Lobby for a room without members (#1328)

Baseline `92842406`, under #826. The public in-memory coordinator checks Waiting
state, reads an empty player list, then calls the database transition. Storage
returns success without entering Lobby because `Room::enter_lobby` requires a
seated member. The coordinator treats that no-op as a transition and publishes
`LobbyStateChanged(Lobby)`. Its return value and event contradict stored Waiting
state. A spectator-only room can receive this false update.

The red-first regression fails at the success-result assertion in Nextest
`802aa875`. The repair returns false immediately after an empty membership read,
before route queries, mutation or event enqueue. The regression checks empty and
spectator-only rooms, exact before/after spectator serialization, unchanged activity
and lobby timestamp, no broadcast, then readmits a member and requires one valid
Lobby event. Occupied readiness, cancellation ownership, repeated promotion,
read failures and broadcast failures retain their existing controls.
All five focused controls pass in Nextest `341bf441`.

This is a confirmed public coordinator/API defect. The server's normal join path
admits its player before promotion; no production socket incident is established.
The room gate still serializes cooperating room operations. This does not claim
atomicity against an embedder that mutates storage outside that gate.

External-client assessment uses current main sources: Rust `client_core.rs` at
`da8f2d16`, .NET `Polling/FramePipeline.cs` at `e467c871`, and Godot
`signal_fish_client.gd` at `7ac2578e`. Rust applies Lobby updates to current room
membership; .NET decodes and emits the existing lobby payload; Godot applies
state and ready flags while preserving its spectator role. None requires a
Lobby event when a room has no seated members. The repair suppresses a false
event, adds no message or error code, and preserves valid promotion and join
behavior. All three clients are unaffected and need no migration or tracking
change. The wider #826 campaign remains open.

### F217 — An early ready toggle announces an uncommitted lobby (#1330)

At main `dcae839a`, `InMemoryRoomOperationCoordinator::handle_player_ready`
accepts an occupied Waiting room and sends `LobbyStateChanged { lobby_state:
Lobby }` without changing storage. A readiness operation that reaches the
coordinator before the join flow's separate promotion can therefore disagree
with an authoritative room snapshot. The direct coordinator history establishes
the contract defect; it does not reproduce a remote gameplay incident.

`ready_toggle_promotes_waiting_storage_before_its_lobby_event` checks ready and
unready operations from both open states. The original source fails with
`Some(Waiting)` where the delivered Lobby event requires `Some(Lobby)` in
Nextest `76f410b9-4636-41a3-964b-ccbf0f5e2eea`.
The repair promotes only after effective membership validation. It preserves the
documented Lobby-only event and makes subsequent explicit promotion a no-op.
An owned task retains the room mutation guard through storage, ready-set update
and FIFO publication. Missing members and finalized rooms still fail before
mutation. Storage failures keep their existing typed error classification.

The cancellation history holds a ready-map read guard, starts the real toggle,
and observes committed Lobby storage while the toggle waits for the write
lock. It cancels the caller, releases the read guard, and requires the exact
ready list and one all-ready Lobby event. Removing task ownership leaves no
publication and fails this control in Nextest
`d8631494-9d79-4a16-a6bb-564e68888cd3`. Restored ownership passes in Nextest
`d08eab7c-3803-4b0b-b375-4c66c2c9f20d`. This isolates cancellation after promotion;
it does not claim to cover every executor or storage-backend failure.

The initial hypothesis that wire snapshots leak unrouted ready IDs is excluded
by source inspection: join and reconnect builders filter ready IDs against the
captured routed player list, and spectator snapshots expose readiness only on
filtered player records. No routing read was added to `current_ready_players`:
join and reconnect builders call it while holding the routing write gate.

#### Untouched readiness snapshot storage (#636)

When no live ready entry exists, open-room snapshots now read only lobby state.
Finalized rooms still load the durable ready set. In the isolated allocation
control, an untouched snapshot previously allocated 1,123 bytes in 9 allocations
with no spectators, 3,633 bytes in 26 allocations with 16 spectators, and
41,457 bytes in 265 allocations with 255 spectators. Waiting and Lobby match.
The repair uses 272 bytes in 2 allocations in every case. These measurements
include the asynchronous result and exclude setup. Existing finalized-readiness
and membership-read-failure controls remain required. This is an allocation
result, not a gameplay throughput measurement.

#### Retained summary sender counts (#775)

`ReceivedCounts` retains sorted owned sender/count pairs in one vector per
recipient. It preserves the previous tree map's JSON object, key order,
last-value rule for duplicate keys, exact u64 values, shared sender identities,
and summary lifetime after raw records drop. Arbitrary decoded key order and
malformed numeric values have independent tree-map comparison controls.
Existing epoch-set and replay comparisons exercise the production summary path.
All 67 scoped oracle/count controls pass in the final local run.
All 49 scoped coordinator and server readiness controls also pass.

Fresh-process profiles compare shared-name trees with compact counts using the
same binary and artifact writer:

| Rooms × players | Sender entries | Tree RSS bytes | Compact RSS bytes | JSON bytes |
| --- | --- | --- | --- | --- |
| 999 × 16 | 239,760 | 22,032,384 | 13,791,232 | 5,226,344 |
| 250 × 64 | 1,008,000 | 36,184,064 | 26,890,240 | 21,702,982 |

Both variants produce the same SHA-256 for each shape: respectively
`d45e13ac6c28416eb42b0088d48b83d4bc282f7635107334a76355f436c3e181` and
`42dee19aae9773dcce5cf2912b1b247dae4542ca302c51b5002e0a5913b89890`.
These RSS readings include the process baseline and are single retained-storage
observations. The 999 × 16 construction took 74,710 µs for trees and 88,821 µs
for compact counts; the 250 × 64 control took 275,163 µs and 227,324 µs.
No CPU improvement is claimed. The profile retains summary counts, not the
planned ten-minute raw delivery ledger. #775 stays open for its full workload
qualification and remaining generator-memory work.

#### External-client assessment

Rust client `da8f2d16`, .NET client `e467c871`, and Godot client `379cd9ab` need
no changes. Rust's `client_core.rs` accepts the existing Lobby event; .NET's
`FramePipeline.cs` decodes and forwards it; Godot's `sf_events.gd` accepts its
known state and existing fields. The repair changes no wire field, ready list,
explicit start rule or encoding. It commits the state those clients already
receive. The snapshot allocation change and capacity summary storage are also
wire-compatible. No Waiting event is introduced.

### F218 — Lobby promotion accepts an empty effective roster (#1332)

At main `45a9486d`, promotion rejects empty stored membership before intersecting
it with routed seats. A stored member with no active route still promotes the
room, changes its activity time and starts its lobby timer. A read-only spectator
can receive that false Lobby event. This is a public coordinator contract defect;
no remote gameplay incident is established.

The red-first control uses the shipped database and message coordinator. With
one stored member and no routes, promotion returns true instead of false in
Nextest `9d42fce3-9f10-43d6-8e16-be093d1a34ca`. The repair rejects an empty
intersection before mutation. The final control also covers disjoint routes and
spectator-only routing, stale ready IDs, unchanged activity and start time,
subsequent valid promotion, spectator delivery and duplicate suppression.
Coordinators without a routed snapshot retain their stored-membership fallback.
The room mutation gate still serializes cooperating operations; direct storage
mutation outside that gate is not made atomic by this repair.

#### Identity-only readiness reads (#636)

`GameDatabase::get_room_player_ids` has a default implementation that projects
`get_room_players`, preserving custom backends' membership and error behavior.
The shipped backend copies only stored IDs under its room read lock. Ready
toggles, promotion and live readiness snapshots use this projection. Start-game
publication still captures full player records. The existing injected storage
failures and paused-read controls also apply to the projection.

The isolated allocation control varies one member's name from 1 to 4,096 to
65,536 bytes. Setup and assertions are outside each measured interval:

| Name bytes | Old snapshot bytes | New snapshot bytes | Old toggle bytes | New toggle bytes |
| --- | --- | --- | --- | --- |
| 1 | 1,153 | 476 | 7,325 | 6,648 |
| 4,096 | 5,248 | 476 | 11,420 | 6,648 |
| 65,536 | 66,688 | 476 | 72,860 | 6,648 |

Snapshots drop from seven allocations to five; toggles drop from 33 to 31.
These are local allocation measurements, not throughput or network results.
Pending, Waiting, Lobby, Finalized, empty and missing storage controls preserve
membership semantics. Existing cancellation and typed-error controls remain.

#### Collected omission and lifecycle storage (#775)

The capacity collector already releases spare send and receipt row storage.
It now applies the same fallible compaction to gap reports, unsupported notices,
disconnects, churn, join failures and faults. It retains every row and its order;
failed reservation leaves the original evidence intact. Empty buffers release
their storage. Existing workload and event budgets do not change.

At 1,048,577 retained gap reports, row-buffer capacity falls from 83,886,080 to
41,943,080 bytes. Collection takes 25,011 microseconds in this isolated control.
This measures retained row storage, not peak RSS: replacement allocation can
coexist with its source during compaction. Controls cover empty and growing
buffers, exact raw JSON, unchanged replay summaries, and failed reservations.
The planned ten-minute large workload remains unqualified; #775 stays open.

#### External-client assessment

Current Rust main `da8f2d16` gates lobby updates on room membership and applies
the existing state in `src/client_core.rs`. .NET main `e467c871` decodes and
emits the existing Lobby payload in `Polling/FramePipeline.cs`. Godot main
`57d342e0` applies server lobby state and ready flags while preserving the
spectator role in `signal_fish_client.gd`; `sf_events.gd` accepts the existing
state and fields. These clients require no event when there are no effective
seats. Valid promotion events, encodings and errors stay compatible. The new
storage projection and capacity storage do not change client messages. All
three clients are unaffected and need no tracking or migration change.

### F219 — Public authority transfer announces an uncommitted role (#1334)

At main `de3bd9fb`, `coordinate_authority_transfer` queues an
`AuthorityChanged` event without changing storage. A valid transfer returns
true while room snapshots still name the old authority. Missing rooms,
absent target members and rooms without authority support also return true.
This is a public library coordinator defect. The socket room-operation path
uses a separate storage transaction; no live socket incident is established.

Two red-first controls fail in Nextest
`15e72e7e-86fe-4abd-b1f7-2c1e0467c513`: the stored authority remains player 1
when transfer to player 2 succeeds, and an absent target is accepted. The
repair uses the existing atomic validated storage assignment before queuing
its live and replay event. Storage refusal returns false without publication.
An owned task retains the room mutation guard across assignment and FIFO
publication, so cancellation of the waiting caller cannot split the transfer.
A later delivery error does not undo the committed role.

Controls check transfer in both directions, every member's stored authority
flag, recipient-specific flags, ordered replay, refused transfers with unchanged
storage, cancellation after commit, and a failed broadcast followed by another
valid transfer. The existing broadcast-failure fixture now creates a valid
stored room so it still reaches its intended failure.

#### One-player disconnect snapshots (#636)

`GameDatabase::get_room_player_snapshot` returns the selected member and the
room authority from one snapshot. Its default implementation projects one
`get_room_by_id` result and preserves custom-backend errors and absence
semantics. The shipped backend clones only the selected player while holding
one room read lock. Disconnect capture retains the previous distinction
between a missing room, missing member and failed storage read. Pending room
visibility and the existing injected storage-read controls also apply.

An isolated allocation control retains the selected player's nested connection
metadata and increases unrelated room metadata. Setup and assertions are
outside the measured intervals:

| Members | Spectators | Old capture allocations | Old bytes | New allocations | New bytes |
| --- | --- | --- | --- | --- | --- |
| 1 | 0 | 18 | 2,357 | 7 | 828 |
| 4 | 16 | 53 | 194,949 | 7 | 828 |
| 8 | 255 | 316 | 2,490,205 | 7 | 828 |

These are local allocation measurements, not throughput or network results.
The old capture includes the full room clone and the selected-player clone.
Controls compare player metadata, authority assignment and clearing,
missing members and rooms, pending visibility and default-adapter errors.
Existing disconnect failure and reconnect controls exercise the server path.

#### External-client assessment

Rust main `da8f2d16` validates the existing `AuthorityChanged` roster identity
and local flag, then updates its authority in `src/client_core.rs`. .NET main
`e467c871` decodes and emits that message in `Polling/FramePipeline.cs`.
Godot main `689ecdfb` applies roster authority flags before emitting
`authority_changed` in `signal_fish_client.gd`. All three clients are unaffected:
the public coordinator now stores the role its existing event already names.
No wire field, encoding, request/release rule or client migration changes.
Disconnect projection and capacity storage also preserve wire behavior.

#### Retained rejoin snapshot tails (#775)

Capacity churn records now retain snapshot tails in sorted boxed entries
instead of per-snapshot tree nodes. Each tail still owns its member name,
player identity and exact sequence. Lookup remains ordered by member name.
JSON remains the same sorted object, including omitted empty tails and
last-value handling for duplicate keys. Historical artifact rows keep their
existing decode and replay behavior. No workload or memory budget changes.

A fresh-process profile retains 15,984 snapshots with 16 members each
(255,744 tails). Tree storage uses 58,249,216 RSS bytes; compact storage uses
41,771,008, a difference of 16,478,208 bytes. Both write the same
19,188,418-byte artifact with SHA-256
`f7bd1e17bfa129ef0af1d62332bd67f1f60bacb67e7bedae466dd38d815f2c97`.
Construction samples are 176,555 and 113,002 microseconds respectively.
A second shape retains 16,000 snapshots with 64 members (1,024,000 tails).
Tree RSS is 151,080,960 bytes and compact RSS is 137,056,256 bytes.
Both artifacts contain 76,730,882 bytes with SHA-256
`ad84504f76d69bf879350792eb50d3a8b757f9d4c802cb23dadd0c0c9e245d33`.
Construction samples are 488,194 and 331,813 microseconds respectively.
These single local retained-storage observations include the process baseline;
they do not establish peak memory, general CPU speed or server capacity.
The live collector converts an owned tree; its source and replacement buffers
can overlap during conversion.

Independent tree-map controls cover empty and omitted tails, escaped and
Unicode names, NUL bytes, duplicate keys, exact maximum sequences and malformed
values. Existing oracle controls preserve exact incarnation, missing, duplicate,
misroute, gap and replay decisions. The planned ten-minute large workload is
still unqualified, so #775 remains open.

### F220 — Held-receive diagnostics repeatedly reacquire the reactor core (#1319)

PR #1335 head `951b5862` fails Native Client Interop run `41243959421`,
job `124663419695`, during held-callback setup. All 2,048 sends are admitted.
The timeout retains 3,086 accepted SCTP chunks, zero refusals, 527 completed
reliable transport messages and 68 acknowledgement timeouts. Engine raw,
admitted, rejected and pending receipts remain zero. ICE and DTLS remain
connected, with no recorded channel closure. This excludes incomplete send
admission in that attempt; it does not establish a transport stall or its cause.
The job records 324 passes and this one failure. The unchanged local control
passes all four cells in 16.807 seconds. That pass does not repair the failure.

Each full transport snapshot takes the shared WebRTC core lock repeatedly.
The shipped futures mutex permits an observer to reacquire the lock before a
woken reactor runs. An isolated real peer-connection control holds the core,
registers a waiting driver timeout, releases the core and observes its wake.
It then completes 128 public stats queries before polling the driver again.
The driver subsequently completes. This qualifies the reacquisition mechanism;
it uses the standalone wrapper's locked dependency graph, no negotiated network,
and does not establish the hosted timeout's cause. Its temporary test is removed.

Instrumented four-cell controls retain the same offered packets, deadlines,
refusal precondition and exact receipts:

| Available CPUs | Tight full snapshots | 10 ms full snapshots |
| --- | --- | --- |
| 12 | 23,509 | 305 |
| 2 | 30,698 | 525 |

Both versions pass. A two-CPU, four-thread profile cohort also passes with both
samplers: 23.03 seconds for tight sampling and 24.23 seconds for spaced sampling.
These are observations of sampler work, not a generalized speed improvement or
historical-cause proof. Exact source identities and every attempt are retained.
An uncompiled dependency test probe was removed between cohort builds; no
native runtime change from that probe is claimed.

The fixtures now wait 10 ms after failed transport-core observations: full
snapshots, outstanding bytes, channel state and physical receipt counters.
One shared test-only interval covers the engine profile, receive and close
controls and the client receive control. Offered-traffic yields, one-shot
assertions, task completion and local atomic waits keep their existing rules.
A successful observation still returns immediately.
Both saturation paths share the same refusal predicate and retain the original
15-second deadlines, 2,048 offered packets, receive limits, role and generation
checks, and exact receipt requirements. Timeout evidence adds observation counts,
elapsed setup time and a bounded sender snapshot. Missing, null and malformed
refusal fields still cannot establish saturation.

The paused-clock regression uses the shipped runtime mutex and a waiting worker.
The busy-yield negative control samples twice before worker progress and fails.
The spaced observer does not take another sample during its wait; the worker
then progresses and the next synthetic observation detects refusal. This checks observer
scheduling without requiring a future mutex implementation to remain unfair.
The native fixture change does not alter client protocol or gameplay behavior.
The hosted failure was not reproduced locally, and #1319 remains open for its
cause and full qualification.

### F221 — A direct response error suppresses a committed authority event (#1337)

At main `6611624e`, `enqueue_authority_result` returns on a failed
`AuthorityResponse` send before it publishes or records `AuthorityChanged`.
Storage has already committed the role. A public `MessageCoordinator` backend
that reports a direct-send error can therefore leave live peers and replay on
the old authority. The shipped in-memory message coordinator returns success
after its delivery attempt, so no shipped socket incident is established.

The red-first control fails in Nextest
`b724d357-977f-4e40-aa11-54a05c68b1e8`: the release commits but produces zero
room broadcasts instead of one. The repair retains the response result,
attempts the ordered room publication, then reports the response error.
A denial still produces no authority event. No storage mutation is undone.

Controls cover release, a new claim, a denied conflicting claim and another
release while direct responses fail. They check exact stored authority flags,
live event count and order, and three replay records. A second cell also fails
the last room broadcast and still retains its committed role and replay.
Existing cancellation, FIFO, recipient-flag and broadcast-failure controls
continue to apply. The test coordinator records response and publication
attempts before either injected error. A temporary source control defers the
response until after publication; Nextest
`128ed288-c508-4f5c-97c1-91253ebf4d60` fails with
`[Publication, Response]` instead of `[Response, Publication]`. The control is
removed and the production order is restored before publication.

#### Finalized readiness capture (#636)

The finalized readiness fallback previously loaded lobby state and then cloned
the entire room to move out its ready identities. The new database projection
reads state and persisted readiness under one room read lock and copies only
the ready identities. The default backend adapter projects one full snapshot
and preserves missing-room and storage-error behavior. Open rooms return an
empty list, including pending rooms; finalized rooms return their durable set.
The live readiness path and snapshot routing are unchanged.

An isolated allocation control holds eight members and their nested metadata
constant while increasing spectator metadata. Setup and assertions are outside
the measured intervals:

| Spectators | Full-room allocations | Full-room bytes | Projection allocations | Projection bytes |
| --- | --- | --- | --- | --- |
| 0 | 51 | 108,595 | 2 | 248 |
| 16 | 68 | 258,395 | 2 | 248 |
| 255 | 307 | 2,496,059 | 2 | 248 |

The baseline measures the full-room read alone, excluding the preceding state
read. These are local allocation measurements, not network or throughput
results. Controls check exact finalized identities, open and pending states,
missing rooms, read failures, default-adapter behavior and the existing
finalization-straddling readiness snapshot.

An initial routing-filter proposal was rejected during source inspection.
Join and reconnect baseline builders already apply routing while owning the
routing write gate. Reading routing again inside `current_ready_players` would
deadlock those builders. The experimental filter and its tests were removed;
Issue #1336 records the rejected report and is closed without a repair claim.

#### Bounded churn completion accounting (#775)

The capacity oracle previously scanned every churn event once per planned peer
to find missing rejoins. It now stores remaining counts for planned identities
and walks events once. A completed identity is removed immediately. Unplanned
identities do not extend the map, excess rejoins do not recreate entries, and
output retains sorted missing-peer order. The peak counter bound remains one
entry per distinct planned identity; no per-event validation index is added.

Independent scan controls compare empty, repeated, missing, excess, unplanned,
reversed and malformed histories. The existing churn evidence and replay
controls retain their exact invalidations. This changes neither offered work
nor the separate lifecycle, delivery, gap or incarnation checks. Local scaling
observations compare the unchanged scan with the final counter implementation:

| Planned peers | Raw events | Missing peers | Scan microseconds | Counter microseconds |
| --- | --- | --- | --- | --- |
| 1,000 | 3,857 | 143 | 55,703 | 2,713 |
| 4,000 | 15,428 | 572 | 912,781 | 13,743 |
| 15,984 | 61,652 | 2,284 | 14,850,013 | 72,114 |

Each shape includes two planned cycles and deliberately missing final rejoins.
Both implementations return the same peer list. These single local
observations measure this analysis phase alone. The planned ten-minute large
workload and total generator peak memory remain unqualified, so #775 remains open.

#### External-client assessment

Rust main `da8f2d16` validates the existing authority identity and recipient
flag in `src/client_core.rs`. .NET main `e467c871` decodes authority changes
and responses separately in `Polling/FramePipeline.cs`. Godot main `c6fc227c`
applies authority flags before emitting its existing event in
`signal_fish_client.gd`. All three clients are unaffected: the public
coordinator preserves the existing message schema and response-before-event
order, and the shipped socket backend does not report this direct-send error.
The readiness projection preserves snapshot values and the capacity change is
internal. No client tracking issue or migration is needed.

### F222 — Durable departure repair clears authority without an event (#1339)

At main `af29ac08`, a custom storage backend can fail both a disconnected
member's removal and its fallback authority release. The server removes local
routing and publishes `PlayerLeft`, but retains the authority-bearing storage
row for repair. Both pending durable-detach cleanup and expired reconnect
cleanup discard the later removal result. Storage then has no authority, while
live peers and replay receive no `AuthorityChanged` for that committed change.
The shipped in-memory backend does not return these storage errors; no shipped
socket incident is established.

The pending-detach red control fails in Nextest
`c33cd150-a593-44d2-8f54-7be856cc14f9`: storage reports no authority, but the
survivor receives an empty event list. The repair retains the removed player
record and enqueues the existing cleared-authority event only when that record
held the role. Both cleanup paths own their transaction before the first
storage mutation. The room lane orders publication behind the earlier departure
and ahead of later claims. Shutdown uses the normal drain predicate.

The removal-path sweep also inspected stale-seat moderation and shutdown
cleanup. Stale-seat moderation requires a distinct acting authority, so the
target cannot hold the room's single authority slot under the storage contract.
Shutdown deliberately suppresses normal room traffic. Neither excluded path
needs an added role event.

#### Game-start policy capture (#636)

Game start previously cloned the complete room, including spectator metadata,
then loaded the member records again. The new database projection copies only
`game_name`, `relay_type`, `authority_player` and `lobby_state` under one room
read lock. Its default adapter projects the existing full snapshot. Membership
reads, error behavior and the final exact-membership compare-and-set stay the
same.

The isolated control uses 0, 16 and 255 spectators with unique 32-byte names
that pass the default production name validator. Setup and assertions are
outside each allocation interval. With 255 spectators:

| Outcome | Previous allocated bytes | New allocated bytes |
| --- | --- | --- |
| Forbidden | 47,825 | 1,099 |
| Not ready | 48,932 | 2,206 |
| Started | 55,674 | 8,938 |
| Already started | 47,733 | 991 |

The new measurements are identical at all three spectator counts. Successful
start allocations fall from 319 to 56 at the largest roster. The legal-name
source control fails the allocation ceiling; restored production passes in
Nextest `f54e5a5b-71e2-4602-bb12-d3cd02ca1932`. Earlier large-name storage stress
measurements are retained locally but are not the supported-room comparison.
These are local allocation measurements, not latency or throughput results.

#### Offset-only churn ordering (#775)

The capacity oracle previously retained a raw offset and disconnect count for
every churn row, plus a temporary per-recipient ordinal map. It now retains
only raw offsets. Contiguous recipient/timestamp groups use one running
recipient count; mixed ties temporarily retain disconnect offsets for sorting.
The compact index falls from eight to four bytes per row, and the wide fallback
falls from two machine words to one. Tie scratch reuses a buffer bounded by the
largest mixed group's disconnect count. Index plus scratch therefore does not
exceed the old index alone. No raw event or receipt is
removed.

Independent stable ordering controls compare both widths, reversed raw
histories, pure and mixed ties, Unicode identities, absent epochs and malformed
extremes. The retained-width regression fails on the previous tuple index in
Nextest `d62061ff`. Three paired algorithm-fixture observations use the actual
old compact function and the new function, with 767,232 rows:

| Shape | Peers | Previous median microseconds | New median microseconds |
| --- | --- | --- | --- |
| Separate timestamps | 15,984 | 147,493 | 103,791 |
| Tied timestamps per peer | 15,984 | 148,865 | 99,571 |
| One mixed timestamp group | 1 | 12,511 | 28,664 |

Every paired digest matches. Retained index bytes fall from 6,137,856 to
3,068,928. The extreme single-group case trades more sort work for less
retained memory. The fixture copies the event fields used by sorting and omits
other raw-event fields, so its process RSS is not a full-generator memory
result. All attempts and extracted source identities remain in the ignored
session artifacts. Whole-run peak memory and the planned ten-minute workload
remain unqualified; #775 stays open.

#### External-client assessment

Rust main `da8f2d16` accepts the existing cleared `AuthorityChanged` identity
and false recipient flag. .NET main `e467c871` decodes the existing authority
event in `Polling/FramePipeline.cs`. Godot main `c6fc227c` clears roster flags
before emitting that event in `signal_fish_client.gd`. All three clients need
no changes: the repair restores the existing role event after a departed
member's removal without changing its shape or order. The shipped storage
backend does not produce the two-error history. The start projection preserves
all wire values and the capacity change is internal. No client migration or
tracking issue is needed.

#### Final cleanup qualification

The expiry-only source control suppresses the returned removal evidence at
its enqueue seam and fails with an empty role-event list in Nextest
`e02943d3-0a02-4b4d-8fdc-7ed2371cd1f9`. The control is removed before delivery.
The shared controls cover both cleanup paths, failed and repeated repair,
a previously committed successor, shutdown suppression and application-claim
rollback failure. The cancellation history pauses after durable removal but
before storage returns its result, aborts the caller, and confirms the next
claim waits for `[cleared role, claim response, claimed role]` in live traffic
and the corresponding exact role order in replay.

All 46 scoped capacity churn/replay controls pass in Nextest
`ac671849`, including exact delivery and unchanged replay invalidations. The
two final compact/wide ordering controls also pass in `8bd1446f`. These results
do not close the broader correctness or capacity campaigns.

The final 13 cleanup controls pass with the temporary source control removed.
The 17 scoped game-start controls and the default storage-adapter parity
control also pass. New in-process cleanup tests remain enabled under Miri;
no Miri completion is claimed from these ordinary local runs.

Review strengthened the allocation regression to compare each outcome across
spectator counts, with a 512-byte margin over the zero-spectator baseline and
the existing absolute ceiling. A temporary projection control copies only
spectator UUIDs. It adds 4,080 bytes at 255 spectators and fails the relative
comparison while passing the absolute ceiling. The control is removed before
publication; production still copies only the four game-start policy fields.

The cancellation replay control now also requires the leaver's `PlayerLeft`
before the cleared role and successor role. Swapping only the departure and
clear leaves the old role-only assertion passing but fails the stronger check
in Nextest `5f88cb10`. Restored source passes both cleanup cells in `5a964d32`.

#### Browser setup follow-up (#1341)

The browser qualification attempt on `55342d9c` restored its Chromium cache,
then stopped during APT metadata setup before builds or gameplay. The job
ended canceled near its 75-minute limit. The exact wait cause and cancellation
actor remain unknown; this does not establish a browser or server defect.

Both opt-in Playwright dependency setup sites now download through the locked
package and launch the selected executable before installing OS packages.
A ready browser avoids package setup. Failed startup permits one dependency
install and requires a successful second launch. Download, setup, persistent
startup and close failures remain fatal. Full Chromium and the headless shell
use their respective executables. Fortress's default opt-out and Firefox's
Mesa/Xvfb setup remain unchanged. Startup does not qualify WebGL or gameplay;
the full hosted suites and their job limits remain required.

Seven CLI fixture tests pass, including setup-forbidden ready hosts and failure
propagation. The previous unconditional setup commands fail those controls.
The locked Chromium shell also starts locally through the new helper. These
results repair avoidable package setup on ready hosts; they do not determine
the historical runner's readiness or explain its APT stall. #1341 stays open.

### F223 — Caller cancellation hides a committed v3 authority transfer (#1342)

At main `92b1c6bd`, the v3 `TransferAuthority` handler writes storage in the
caller's task. An asynchronous backend can commit the role and then suspend
before returning. Cancellation at that point drops the room lane without
publishing `AuthorityChanged` or recording replay. Stored authority and the
members' role views disagree. This differs from the public coordinator's
already-owned transfer path.

The deterministic source control pauses the in-memory adapter after the
storage write, aborts only the handler caller, and releases storage. The
successor receives no event within the existing one-second receive bound in
Nextest `99ea2e7a-13f3-4b0f-a6a8-26dfb4289a64`. This reproduces an asynchronous
adapter boundary; it does not establish a production network incident.
The shipped in-memory write has no post-commit wait. The pause represents
a source-compatible asynchronous backend.

The caller validates the request and acquires the lifecycle and room mutation
guards before it hands the storage write and publication to an owned task.
This preserves existing room-lane FIFO admission. The owned task keeps both
guards, the storage result and the FIFO publication alive when the caller
stops waiting. A later transfer cannot publish ahead of the
committed event. Existing correlated results, recipient authority flags and
replay shapes stay the same. The regression checks both role changes in live
traffic and replay. Refused transfers still preserve storage and emit no role
change. All 62 scoped room, readiness and transfer controls pass with all features
after the final ownership boundary change.

The first candidate spawned before acquiring the room lane. Hosted root CI
caught a changed admission order in the existing concurrent transfer/start
control. The unchanged control also fails locally in Nextest `402d745c`.
Moving ownership after validation and lane acquisition, but before the write,
retains that FIFO contract without weakening the cancellation control.

#### Admission and transfer projections (#636)

Spectator admission previously cloned a whole room to identify its event
lane, then loaded its required baseline inside that lane. The new
`GameDatabase::get_room_id` projects only the published ID. The default
adapter calls the existing lookup; the shipped adapter holds the same
membership, code and publication locks as a full lookup. Admission still
loads fresh baseline fields inside the room lane. Pending visibility,
application scoping, capacity checks and code-rotation ordering stay intact.
The transfer handler also uses the existing atomic member/authority snapshot
instead of copying unrelated members and spectators.

The isolated complete spectator-admission profile retains every existing
baseline and notification assertion. Stored spectators are not live routes,
so this measures baseline construction rather than spectator fan-out cost.

| Existing spectators | Previous allocations / bytes | New allocations / bytes |
| --- | --- | --- |
| 0 | 147 / 23,808 | 139 / 21,565 |
| 16 | 211 / 32,036 | 187 / 27,631 |
| 254 | 1,163 / 172,044 | 901 / 130,541 |

The stricter three-copies-per-spectator ceiling fails on the old source in
Nextest `c50bc235` and passes after the projection. The lookup tests cover
publication, rotation, deletion, code reuse, stale indexes, game scope, case,
legacy adapter errors and all three lock boundaries. These are local
allocation results, not latency or throughput measurements.

#### Exact room churn index capacity (#775)

The capacity oracle previously grew each room's offset vector geometrically.
The new first pass counts accepted rows per room; the second pass fills exact
capacity in the existing compact or wide representation. Unknown recipients
still add no index entries. Timestamp order, raw-order ties, viewer-first
ordering and the raw records stay unchanged. Both widths compare with
independent stable sorts, including empty input, Unicode and mixed ties.
The retained-storage ceiling fails on old source: one row retains 16 bytes
instead of four.

A fresh-process million-row profile retains 4,000,000 index bytes instead of
4,194,304. Both sources produce checksum `4894385488969092582`. The one
observation takes 1,435,974 microseconds before and 1,483,981 after; this does
not establish a speed improvement. The extra count pass trades work for an
exact retained bound. A second paired shape has 767,232 rows: index storage
falls from 4,194,304 to 3,068,928 bytes with the same checksum
`6131198148197686732`. The single observations take 1,074,304 microseconds
before and 1,279,534 after. Set `CAPACITY_ROOM_CHURN_ROWS=767232` when running
`generator_room_churn_index_memory_profile` to repeat this shape. All 48
scoped churn and replay controls pass in Nextest `cb7430ea`.
Peak process memory, full generator storage and the planned ten-minute
workload remain unqualified; #775 stays open.

#### External-client assessment

Rust main `da8f2d16` validates the existing authority identity and recipient
flag and applies the role in `src/client_core.rs`. .NET main `e467c871`
decodes the existing event in `Polling/FramePipeline.cs`. Godot main
`c58b1631` applies roster flags before emitting `authority_changed` in
`addons/signal_fish/signal_fish_client.gd`. All three are unaffected: the
repair completes an existing accepted operation and restores its existing
ordered event. It adds no message, field, error, default or migration need.
The projections preserve wire values. The new storage method has a default
for existing backend implementations. The capacity change is internal.
No client tracking issue is required.

### F224 — Interrupted heartbeat refresh consumes its retry window (#1344)

At main `d964d88d`, the heartbeat path records its throttle timestamp before
awaiting player persistence and room activity. Canceling the refresh while
it waits for the in-memory room write leaves the old room timestamp intact.
The next message then skips persistence for the full throttle window. A room
near its expiry boundary can lose the refresh needed to remain present.
A storage error has the same throttle effect for asynchronous adapters.
No production network incident is established.

The deterministic control holds a room read guard, polls the refresh until
its room write waits, drops that future, releases the guard and tries again.
The original source fails because the next message leaves room activity
unchanged in Nextest `1d34abe9`. The repair reserves the throttle window and
completes it only after both writes succeed. Dropping an unfinished reservation
permits an immediate retry. A per-connection revision and physical lifecycle
identity keep an old reservation from clearing a newer refresh or replacement
connection. Reconnection and rollback carry the revision forward.
A temporary control that resets that revision fails the same-socket reconnect
rollback test in Nextest `e5fb2d1d`. Restored source passes the control.

Controls cover initial and previously completed refreshes, both persistence
errors, exact throttle boundaries, unknown players, the disabled throttle,
newer successful refreshes and replacement identity. After retry, expired-room
cleanup retains the room. The reservation adds no heap allocation. It adds one
`u64` revision field per connection. Existing refresh metrics continue to count
attempts, including failed and canceled attempts. This repairs retry eligibility;
it does not complete the broader liveness or production correctness audit.

#### Player admission allocation (#636)

The seated admission code now resolves its room lane with the existing
`GameDatabase::get_room_id` projection. It still reads fresh room state after
entering the lane. The default adapter fallback remains available. Creation,
join-only refusal, room-code locking and admission checks keep their contracts.
The new complete-admission allocation control checks both player identities,
the exact spectator baseline and the incumbent's `PlayerJoined` event.
Stored spectators are not live routes, so this measures baseline construction.

| Stored spectators | Previous allocations / bytes | New allocations / bytes |
| --- | --- | --- |
| 0 | 114 / 25,794 | 107 / 24,150 |
| 16 | 199 / 36,656 | 175 / 32,550 |
| 255 | 1,394 / 200,535 | 1,131 / 159,322 |

The four-copies-per-spectator bound fails on the original lookup and passes
with the projection. These are cumulative allocator measurements, not peak
memory, latency or throughput results. #636 remains open.
Hosted CI caught an unchecked setup-message drain in the new allocation fixture.
The fixture now checks the creation baseline, lobby event and empty queue before
measurement. Its allocated-byte and allocation totals stay unchanged.

#### Rejoin-only room view index (#775)

Room epoch views ignore disconnect rows. Their index now retains only rejoin
positions. Rooms with disconnects but no rejoins share their unchanged initial
view. The raw churn ledger, lifecycle ordering and disconnect validation still
consume every recorded row. Both offset widths match independent stable
per-viewer sorts, including unknown recipients, Unicode, empty input and ties.

A fresh-process profile at 767,232 raw rows retains 511,488 positions instead
of 767,232. Index storage falls from 3,068,928 to 2,045,952 bytes. Both sources
produce rejoin-order checksum `7286540773032410126`. The paired observations
take 1,142,715 and 976,101 microseconds. One pair does not establish a speed gain.

A separate temporary source comparison keeps 72 complete summary lengths and
SHA-256 digests identical. It covers reliable, latest and volatile delivery;
forward and reversed raw order; valid churn; missing and duplicate disconnects
or rejoins; tied times; unknown recipients and snapshot identities; missing
and duplicate receipts; wrong receipt epochs; and disconnect-only records.
The temporary probes are removed before publication. The durable index and
view controls remain. Whole-generator peak memory and the planned ten-minute
cell remain unqualified; #775 stays open.

Final local validation passes all 19 scoped all-feature lifecycle controls,
46 capacity/replay controls, the complete player allocation control and strict
all-target/all-feature Clippy. Hosted qualification remains required.

#### External-client assessment

Rust main `da8f2d16` queues the existing `ClientOperation::Ping` in
`src/polling_client.rs`. .NET main `e467c871` uses the existing
`EnvelopeWriter.WritePing` in `Polling/SignalFishPollingClient.cs`. Godot main
`c58b1631` sends the existing ping envelope in
`addons/signal_fish/signal_fish_client.gd`. All three are unaffected: the repair
changes server persistence retry state only. It preserves heartbeat cadence,
Ping/Pong payloads, error codes and room events. The admission projection
preserves baseline values. The oracle change is internal. No client migration
or tracking issue is required.

### F225 — Clearing a public replay ring hides discarded events (#1347)

At main `2d068b42`, `EventBuffer::clear()` removes retained events without
advancing `evicted_watermark`. An embedder can discard events needed by an
older cursor and still read no truncation evidence. The production manager
removes whole room buffers and does not call this public method. No live
WebSocket incident is established.

The new clear regression fails on the original source in Nextest `0cf10ad4`:
a ring containing sequences 8, 20 and 13 retains no watermark after clear.
The repair records the highest discarded sequence before clearing. Empty and
repeated clears preserve prior evidence, including an already higher
watermark. A cursor control checks replay after a later push on both sides of
the discarded tail.

#### Full replay ring storage (#636)

The previous push path appends before evicting. When a ring fills its backing
allocation, the next event grows that allocation even though its oldest event
is immediately removed. A zero-capacity ring also allocates before discarding
every event. The repair evicts before appending and records zero-capacity
loss without storing the event. Mutable-capacity controls retain the exact
removed count and maximum loss sequence after shrinking or disabling a ring.
The original storage regression also fails in Nextest `0cf10ad4`.
All 28 selected all-feature replay controls pass in Nextest `775741be`.

The original allocation benchmark fails its no-growth ceiling. At capacities
4, 16, 64 and 256 it retains twice the filled ring storage after the next
push. The 256-event ring grows from 53,248 to 106,496 bytes; a disabled ring
allocates 832 bytes. Capacity 1 already has spare backing slots and does not
grow. The repaired benchmark keeps the 256-event ring at 53,248 bytes and
the disabled ring at zero bytes. All six capacities add no allocations or
reallocations during 128 further pushes. The complete allocation benchmark
passes its existing relay ceilings too. These are deterministic ring-allocation
results, not process peak memory or throughput measurements.

#### Planned churn progress storage (#775)

The capacity oracle previously retained one progress-map entry for every
observed churn identity, including unknown and unplanned identities. It also
kept a separate cycle-plan map. Progress now lives with each planned peer;
unplanned events retain no counter. The raw ledger, event ordering, exact
invalid count, first violation and phase/epoch checks remain unchanged.

A fresh-process 500,000-row unknown-identity profile reduces peak process RSS
from 81,764 to 56,924 KiB. Both sources return exactly 500,000 invalid events
with the same first identity, phase and reason. The original and repaired
binary SHA-256 prefixes are `809becdb` and `1572f4bd`. The retained fixture is
`generator_unplanned_churn_memory_profile`; run it alone with `--ignored`.
Linux `wait4` attributes the final RSS measurement to that exact test child.
An earlier repaired-source control records 57,052 KiB with the same verdict.
The new complete-summary control includes 4,096 unknown peers, an unplanned
roster peer and a valid planned lifecycle. It compares forward and reversed
histories and artifact replay exactly. Fifteen scoped churn controls pass in
Nextest `082dc301`.

This is one oracle-phase memory measurement. It does not establish server
throughput, whole-generator peak memory or the planned ten-minute cell.
Campaigns #826, #636 and #775 remain open.

#### External-client assessment

Rust main `da8f2d16` has no server-crate dependency in `Cargo.toml`. .NET main
`e467c871` is an independent `netstandard2.1` SDK. Godot main `c2184135` uses
the GDScript client in `addons/signal_fish/signal_fish_client.gd`. None calls
the Rust server's public replay-ring method. All three are unaffected: the
clear repair has no production caller, ring storage preserves event values
and order, and the capacity change is internal. No wire field, error code,
default, client migration or client tracking issue is required.

### F226 — Dashboard collection mixes room snapshots (#1349)

At `0365dc5d`, the dashboard collects counts and occupancy percentiles through
three independent room-table reads. Deleting the only room after the count
read leaves a successful sample with one active room and empty percentiles.
This affects monitoring. It does not change gameplay membership.

A deterministic control pauses after the old count read, deletes the room,
and resumes collection. It fails in Nextest `12ed53d5`: counts are nonempty while percentiles are
empty. The repaired control passes in Nextest `9ac6e1f4`. An earlier test build failed because its
room-creation call lacked an argument; that attempt is retained separately.

`GameDatabase::get_room_metrics_snapshot` provides a combined query with a
source-compatible default. The in-memory override collects counts, overall
occupancy and per-game occupancy under one read lock and builds percentile
maps after releasing it. The dashboard uses this query. Custom backends retain
the separate-query default and must override it for snapshot consistency.
Mixed occupancy, empty storage, pending rooms and read failures retain their
previous values and behavior. A second negative control reverts only the cache
query to separate reads and fails at the cache boundary; restoring the combined
query passes it. Metrics tests use separate pause controls so a background
refresh cannot consume a room-operation test pause. A focused negative control
reproduces the original shared-control interference before this isolation.

#### Dashboard work and history retention (#636)

An alternating local profile collects 20 samples over 20,000 empty rooms in
16 games. Four combined-query measurements take 249,078–280,870 microseconds;
four separate-query controls take 480,132–573,861 microseconds. Every sample
compares equal. This is a local component profile, not gameplay throughput.

History uses a bounded deque. It removes the oldest sample before adding the
next sample and still returns oldest-to-newest JSON arrays. At 10,000 retained
samples, 30,000 rollover pushes take 1,860 microseconds instead of 1,089,100.
These debug-build samples exclude view serialization and map cloning. The
existing history controls cover wraparound, capacity, selected fields, failures
and recovery without changing the retained sample count.

#### Scheduled readiness allocation measurement (#1350)

Main `1d9def4c` fails macOS Nextest job `124843195926` in run `41304770223`.
Each untouched Waiting snapshot records 3 allocations and 336 bytes;
each Lobby snapshot records 2 allocations and 272 bytes regardless of
spectator count. An ordinary local control passes and is not a repair.

A focused control leaves one cooperative poll credit in a fresh current-thread
runtime before the same readiness operation. Its second lock read yields and
records the exact 3-allocation, 336-byte shape. Initializing the deferred-waker
queue before that operation gives 2 allocations and 272 bytes. Nextest
`f1aa70a9` records both controls. The allocation test now initializes that queue
before its measured regions. Every operation, state, spectator cell and
allocation assertion remains unchanged. This warm-up candidate is insufficient:
macOS job `124919556555` on head `85b4ee0a` still records 3 allocations and
336 bytes for every Waiting sample, versus 2 allocations and 272 bytes for
Lobby. The scheduler control did not establish that hosted cause.

Rust 1.94's macOS mutex stores its native pthread mutex in a lazy `OnceBox`.
Tokio 1.54.1's semaphore locks that mutex when a read guard releases its permit.
The coordinator's readiness lock is new before Waiting and already used before
Lobby. This per-lock initialization explains the state-shaped difference; a
scheduler warm-up cannot initialize a newly created lock.

The fixture now reads the coordinator's empty readiness-key list before the
allocation regions. This initializes the lock without creating readiness
entries or calling the measured snapshot method. Every original snapshot call,
state, spectator cell and allocation assertion stays intact. A macOS-only
ignored qualification separately checks that the first native Tokio read-lock release
allocates one extra 64-byte block while the second does not. Those controls
run without cooperative yielding to separate native lock and scheduler costs.
The exact native allocation cost is diagnostic rather than a normal test
contract because supported platform or dependency changes may alter it.
The failure-only component diagnostics remain available. New-source macOS
qualification and the retained attempts stay tracked in #1350.

#### External-client assessment

Rust main `da8f2d16` has no server-crate dependency in its manifest. .NET main
`e467c871` targets an independent signaling SDK. Godot main `f0ae2e81` uses the
GDScript WebSocket client. These clients do not call the Rust storage trait or
collect the operator dashboard. All three are unaffected: no WebSocket field,
ordering, default or error changes. The capacity snapshot change is internal.
No migration or client tracking issue is required.
Campaigns #826, #636 and #775 remain open.

### F227 — Session capture can list a closed room twice (#1353)

At `c06a4400`, session capture copies active rooms before locking the completed
ring. Closing a copied room before that lock lists the same room in both lists.
The active copy has no end time; the completed copy has end time and `seq=1`.
Nextest `77fa0158` retains this deterministic failing history. This affects
operator session telemetry, not room membership or gameplay delivery.

The repair locks the completed ring before a close removes an active record.
Capture holds that same lock across active traversal, completed copying and
lifetime drop-counter reads. Both paths take the ring lock before active-map
locks. Relay counter updates need no new lock. This orders close movement with
capture; concurrent creation and counter updates are not a globally atomic
snapshot. The next `since=0` read must receive the completion exactly once.
Application filters, active caps and sequence ordering retain their contracts.

#### Completed history storage (#636)

The same component inserts before evicting a full 1,024-record ring. Its first
rollover doubles backing capacity to 2,048 while retaining 1,024 records.
Nextest `7e34c5ad` fails at that first rollover. Evicting before insertion reuses
the full ring. The regression also checks retained room IDs, exact completion
sequences and lifetime drop accounting through more than one full rollover.
This is a retained-storage improvement, not a gameplay throughput result.

#### Snapshot player identities (#775, #636)

Capacity snapshots already share member keys but keep independent player-ID
strings in every tail. They now share exact player identities within the run
and keep sequence tails independent. A pointer-sharing regression fails on the
old source. Historical JSON, duplicate-key behavior, unusual names, full-width
sequence values, raw event evidence and replay remain exact.

Fresh-process profiles compare retained source binaries `439586e8` and
`ca7428f4`. At 255,744 tails, RSS falls from 30,420,992 to 19,628,032 bytes.
Both artifacts contain 19,188,418 bytes with SHA-256
`f7bd1e17bfa129ef0af1d62332bd67f1f60bacb67e7bedae466dd38d815f2c97`.
Construction takes 313,955 versus 588,785 microseconds in this local observation.
At 1,024,000 tails, RSS falls from 86,167,552 to 38,596,608 bytes. Both artifacts
contain 76,730,882 bytes with SHA-256
`ad84504f76d69bf879350792eb50d3a8b757f9d4c802cb23dadd0c0c9e245d33`.
Construction takes 1,154,027 versus 2,068,390 microseconds.

Sharing has a cost when identities do not repeat. A 64,000-tail, one-member
control raises RSS from 22,904,832 to 24,932,352 bytes and construction from
187,491 to 332,084 microseconds, with identical 5,481,782-byte artifacts.
These are component observations, not whole-generator peak memory or the
planned ten-minute cell. No workload, memory admission budget, receipt check
or replay requirement changes. Campaigns #826, #636 and #775 remain open.

#### Room replacement progress (#1352)

Each peer now retains one lifecycle snapshot: current stage and start time,
last completed stage and time, incarnation and whether the task returned.
Invalid runs write `peer-progress.json` and include it in existing diagnostics.
Held upgrade, authentication reply and room-snapshot controls preserve the
pending stage after cancellation. Repeated transitions retain constant state.
The unchanged live replacement cell passes in Nextest `6266dd27`. This ordinary
pass does not explain the historical instrumented timeout. No production cause
is established. #1352 remains open for matched controls and qualification.

#### External-client assessment

Rust main `da8f2d16` has no server dependency in its manifest. .NET main
`e467c871` is an independent `netstandard2.1` SDK. Godot main `10bd07ad` uses
the GDScript WebSocket signaling client. All three are unaffected: session
telemetry changes no gameplay messages, ordering, defaults or client APIs.
The capacity changes are internal. No client migration or tracking issue is
required.

#### Local qualification

Nextest `08d8e1b2` passes 20 selected session-record controls. Expanded empty
and full-ring histories plus endpoint truncation controls pass in `f526ecf1`
(22 tests). Full-ring backing storage is 286,720 bytes instead of 573,440.
Adversarial review replaces a timed negative wait with a direct lock-ownership
check at the paused capture boundary. Final deterministic qualification passes
in `11cd8c5b` (22 tests); endpoint integration controls pass in `4e0eca05`
(2 tests). Snapshot identity controls pass in `34a0f8bd` (12 tests), and
65 related churn, rejoin and replay controls pass in `173fe1b8`. Held lifecycle
and unchanged room replacement controls pass in `6266dd27` (3 tests).

#### Session scan cost after review

The close fence includes the full active traversal. A capped response still
visits every active record. Trace diagnostics report visited and matching
counts, response limit, scan time and total capture time. Timing starts after
lock acquisition; logging runs after the completed-ring guard is released.
Disabled trace logging reads no clocks. Response fields and lock order do not
change.

Five debug-build samples per 10,000-room cell measure capped all-match scans
at 2,657–9,985 microseconds and captures at 2,807–10,154 microseconds. Unlimited
all-match scans take 2,655–9,187 microseconds and captures 6,576–14,479
microseconds. No-match scans still visit all 10,000 records and take
1,231–2,285 microseconds. These local component observations exclude lock wait,
JSON serialization and gameplay throughput. They do not establish a production
latency bound. The ignored `session_snapshot_scan_duration_profile` retains
the four cells. Nextest `192e438d` passes 22 focused controls; `ebac8c5b` passes
the isolated profile. Adversarial review finds no remaining issue.

### F228 — Initial joins lose progress and completed refusals on cancellation (#1355)

At `b7c41a31`, initial joins have no lifecycle tracker. The manifest is written
only after they finish. A held first-room upgrade leaves no run identity or
planned-peer progress when the caller cancels. A refusal that returns while
another join remains pending also stays inside `join_all` until the whole room
finishes. Cancellation discards that completed failure.

The cancellation regression fails before the repair in Nextest `5f0f7346`
(the retained log names the missing initial manifest). A second negative control
restores the old post-`join_all` error recording while keeping the new early
manifest. It fails in `b8eba0a6`: the cancelled run retains zero completed
refusals instead of one. The repair writes the
manifest before connecting, creates one fixed-size row for each planned peer,
and records each refusal as soon as it returns. Successful peers reuse those
trackers during workload and churn. Preparation and workload timestamps name
separate clocks. Each handshake write has a step deadline; each room's initial
joins have a six-step outer deadline. Other rooms get their own setup budget.

Dropping preparation writes partial progress, exact completed failures and an
explicit cancelled-preparation marker. It does not write a successful summary.
The normal path writes progress before arming and after all producers finish.
Held upgrade, negotiation and baseline controls retain every planned peer,
including unstarted rooms. A mixed held/refused control retains the completed
refusal. The unchanged replacement workload still requires every receipt and
an identical replay verdict. These controls do not explain the historical
post-epoch second replacement in #1352. That investigation remains open.

#### JSON gameplay sizing (#636)

The relay byte measure now counts JSON value structure, integer digits and
string escape overhead directly. Ordinary string fragments skip eight bytes
at a time with safe integer operations. Float formatting still uses the same
serializer. The serializer remains the byte-count reference; no wire encoding,
payload limit, admission budget or relay ordering changes.

Property histories cover nested values, Unicode keys and values, signed and
unsigned integers, and floats. Explicit histories cover every ASCII byte at
every eight-byte offset, control escapes, UTF-8 boundaries, integer limits,
negative zero and small/large floats. The text-lane byte-budget and payload-size
controls remain exact. Allocation benchmarks check both sizing methods.

The first scalar candidate takes 3.098 microseconds for the 4 KiB ASCII cell,
versus the 1.445 microsecond serializer baseline, and is rejected. A second
candidate improves escaped strings and integers but regresses ASCII to
4.211 microseconds. Its matched serializer control takes 1.430 microseconds.
These attempts remain part of the evidence; neither ships.

The retained first benchmark also stops when Criterion cannot find a baseline
for the newly added serializer control. That tooling error does not qualify the
unmeasured cells. Later attempts save separate baselines and retain both paths.

The final safe word scan has these matched component times on Linux aarch64,
Rust 1.94.0, in the release bench profile (20 samples, one-second warm-up,
three-second measurement per cell):

| Payload | Direct count (µs) | Serializer (µs) |
| --- | ---: | ---: |
| 4 KiB ASCII object | 0.558 | 1.720 |
| 96-byte ASCII value in an object | 0.023 | 0.081 |
| Unicode string object | 0.781 | 2.101 |
| Escaped string object | 4.112 | 8.996 |
| 512 unsigned integers | 2.204 | 3.546 |
| 64 floats | 1.316 | 1.267 |

The float-only array costs about 4% more in this observation. These are sizing
component measurements on one shared host. End-to-end relay throughput and
remote-network capacity remain unmeasured. The portable comparison is
`cargo bench --locked --bench relay_serialization_runtime --features allocation-tracking -- payload_sizing`.

#### Readiness allocation comparison (#1356)

Main CI run `41304770223`, job `124843195926`, records 3 allocations / 336 bytes
for Waiting and 2 allocations / 272 bytes for Lobby. Each state has the same
profile at 0, 16 and 255 spectators. The old assertion compares all states as
one profile. It fails even though neither state grows with the spectator count.
The corrected assertion compares the same ordered state history across roster
sizes. It retains exact allocation/byte equality and diagnostics on mismatch.
No source for the extra 64-byte allocation is inferred from this log.

#### External-client assessment

The Rust client at `da8f2d16` has no server dependency. The .NET client at
`e467c871` is an independent `netstandard2.1` package. The Godot client at
`5d4c1801` uses its GDScript WebSocket client. Each is unaffected: gameplay
bytes, message schemas, ordering, errors and defaults stay the same. Initial
preparation artifacts and the allocation comparison are internal. No migration
or client tracking issue is required. Campaigns #636 and #826 remain open.

#### Local qualification

The held-stage, clock, refusal/replay, write-deadline and unchanged replacement
controls pass in Nextest `a6bf9668` (10 selected tests). The exact JSON sizing
property, byte-offset and text-lane controls pass in `d7c4c46e` (four tests).
The corrected readiness comparison passes in `fb8d16e1`.

The full local formatting and all-target/all-feature Clippy gate passes with
zero warnings. The deterministic relay allocation gate passes; both sizing
paths allocate zero bytes at 96, 1,024, 16,384 and 65,536 payload bytes. CI
configuration, documentation consistency and Markdown checks pass.

Final cancellation flags pass in `f5f23274`; the unchanged replacement history
also proves that prepared trackers survive into the workload clock and later
incarnations. Both relay allocation gates pass with their existing wire digests
and ceilings. The final all-target/all-feature Clippy gate passes again after
the terminal-flag assertions.

Review found that the guard was disarmed before sampler construction and task
arming. The extended readiness-failure control fails in `5afb6083`: a completed
initial refusal has no persisted deliveries file. The guard now owns the arming
call and disarms only after the workload epoch is published. Arming failures
stop and join every producer before the guard writes partial records. Sampler
construction errors also retain the active guard. No workload timing changes.

The repaired guard passes eleven selected preparation, readiness and unchanged
replacement controls in `a07ce725`. Exact completed refusals and cancellation
markers survive both returned readiness failure and readiness timeout.

The earlier full run `41358544147` passes macOS and coverage at `cc87ede39a62e9d746196af0fcc6dcd46d10c1e5`.
Its Windows job `125004398596` fails the held-socket write control: the 16 MiB
send completes successfully, so no pending write exists to time out. That
fixture inferred backpressure from payload size and was not portable. The
replacement uses a controlled pending sink through the same protocol-write
helper. It checks readiness before the exact deadline, expiry at the deadline,
immediate success and the unchanged transport error. Removing the timeout
fails in `2ced1737`; the bounded control passes in `3e2a2c81`. Live held
upgrade, negotiation and room-baseline controls remain unchanged.

Function documentation now covers all touched sizing, benchmark, preparation,
readiness and allocation functions. The PR explicitly states the #636 sizing
requirements; that broader campaign remains open.

Portable preparation and replacement controls pass in `74c0737f` (eleven tests);
JSON sizing and text-lane controls pass in `ddae9193` (four tests). Strict
all-target/all-feature Clippy, Markdown and doc consistency pass. Production
sizing and benchmark bodies remain identical to the measured source; later
changes to those paths add Rustdoc only.

A further review control blocks each cancellation artifact path with a directory.
The old short-circuit fails in `55244216`: a progress-file error prevents the
cancellation marker and exact failures from reaching their writable paths. The
guard now attempts all three writes independently before reporting errors.
Blocked progress, marker and delivery paths each preserve the other writable
outputs. No successful summary is written for partial preparation.

All twelve artifact, preparation and replacement controls pass in
`f9066f14-5e60-4d66-804c-601bd749d190`. Strict all-target/all-feature Clippy, spelling,
Markdown and doc consistency checks pass. Review reports 100% documentation
coverage for touched complex functions and accepts the explicit #636 scope.

Full macOS verification later fails a separate spectator-admission allocation
profile in run `41362145836`, job `125015008082`. Counts are 144, 193 and
906 for 0, 16 and 254 stored spectators. The middle cell exceeds the same
three-copy ceiling by one allocation; its cross-platform difference includes
an additional 64 bytes. The historical allocation site is untraced; #1359
retains that investigation. No gameplay fault or fourth roster copy is proved.

The profile now owns a fresh runtime per cohort and warms deferred scheduler
storage before setup. Every admission/message assertion and the exact
three-copy ceiling remains unchanged. The existing controlled scheduler probe
passes in `2638d125`, with cold 3 allocations / 336 bytes versus warmed
2 / 272. Local isolated spectator counts remain 139, 187 and 901. Adding one
extra complete roster copy inside measurement fails the same ceiling in
`bde58a8f-fe62-4187-9074-1c4bc70e1c44`. This removes an uncontrolled measurement factor;
it does not identify the historical allocation producer or close #1359.

### F229 — Departure logging copies unrelated rosters; owner-exit evidence is partial

Scope: #636, #826 and #1358, session 605, starting at
`843761c1d3a80b19e2ae48dddd2fe49b3e6c87f5`.

A successful player departure fetched an owned room solely to log its current
code. That copies every stored player and spectator name after the departure
is already committed. The in-memory backend now reads only the code under the
room read lock. The additive database-trait method defaults to the previous
full-snapshot query for custom backends. Missing rooms, pending visibility and
storage errors retain the full query's semantics. The log still uses the latest
code and reports unknown after a missing room or read error.

The isolated 64-read baseline allocates 576 / 1,664 / 16,896 objects and
64,640 / 289,792 / 3,669,376 bytes for 0 / 16 / 254 stored spectators. The
projection allocates 128 objects and 8,064 bytes in every cell. The full leave
control retains its confirmation, peer departure, routing, room membership and
all spectator names. Its repaired counts are 41 allocations and 10,678 bytes
in every cell. Restoring the old departure lookup fails the same full-operation
ceiling in `ed1bdc95`: 48 / 65 / 303 allocations and
11,529 / 15,047 / 67,853 bytes. The repaired allocation controls pass in
`a2dcb93b`. The test bounds roster-dependent growth; it does not claim a
whole-server throughput improvement.

Lifecycle controls cover missing and pending rooms, publication, rotation,
a rotation while a code read is held, deletion and storage failure. Existing
leave controls retain the storage-error rollback and ordered terminal/join
history. The Rust, .NET and Godot clients need no migration: this changes a
server storage projection, not their protocol messages, errors or event order.

The held-readiness cancellation control qualifies the distinct #1358 boundary.
An owner-exit artifact precedes producer return and can omit later derivative
channel-closure notices and task-return flags. The marker now declares that
partial contract. Exact initial refusals and all planned rows survive, no
workload epoch is published, and no success summary is written. The owner drops
both worker handles; separate signals and task-state handles confirm both
detached producers finish after capture. Returned arming
errors retain their existing abort-and-join path. See the owner-exit section in
`arm-capacity-audit.md` for the capture contract. The broader audit remains open.

The held-readiness control first fails on the absent capture-contract marker in
`a0544238`, then passes with the marker in `9c5a1cad` alongside returned
readiness failure, bounded protocol write and unchanged replacement controls.
Code-query lifecycle and existing departure-order controls pass in `31a1608d`.
Strict all-target/all-feature Clippy passes. The historical #1352 and #1359
investigations remain open; these results do not attribute their source causes.
