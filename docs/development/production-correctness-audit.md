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
| A07 | `server/room_service.rs`, `database/` | Join, leave, capacity, passwords, room codes, tenant ownership, partial admission | Initial; stale expiry cleanup in F32 |
| A08 | `server/ready_state.rs`, `coordination/room_coordinator.rs` | PlayerReady, StartGame, membership at commit, readiness snapshots, publication | Initial |
| A09 | `server/authority.rs`, `moderation.rs` | AuthorityRequest, kick, ban, unban, transfer, code rotation, access changes | Initial |
| A10 | `server/spectator_service.rs`, `spectator_handlers.rs` | Join/leave spectator, role exclusion, rosters, stale detach, moderation | Initial; lost terminal events during detach panic recovery recorded in F31 |
| A11 | `server/session_policy.rs`, `signaling.rs` | ProvideConnectionInfo, Signal, plan selection, host eligibility, generations, fallback | Initial; explicit hostless relay reset in F38 |
| A12 | `server/message_router.rs`, `relay_policy.rs` | Dispatch, TransportStatus, negotiated capabilities, stale source identity | Initial |
| A13 | `server/game_data.rs`, `coordination/mod.rs` | JSON/binary GameData, acceptance stamps, exact recipient set, fan-out | Initial |
| A14 | `coordination/outbound_queue.rs`, `protocol/delivery.rs` | Reliable/latest/volatile, sequence ranges, generations, sojourn, bounded memory | Initial |
| A15 | `websocket/sending.rs`, `batching.rs`, writer in `connection.rs` | Encoding, partial writes, idle reports, cancellation, terminal close | Initial; certificate-bound partial-write history in session 413; combined tenant/certificate history in session 414; encrypted-record loss in session 415; aggregate reconnect admission bound in F21; socket close before queue commit in F22 |
| A16 | `server/heartbeat.rs`, `maintenance.rs`, `deadline.rs`, `distributed.rs`, `retry.rs` | Ping, deadlines, clock jumps, lease ownership, GC, stale cleanup | Initial; stale expiry cleanup in F32; delayed first-tick overflow in F36 |
| A17 | `server/shutdown.rs`, `main.rs`, deployment configs | Drain, restart, room routing, directional partitions, process failure | Initial; plain HTTP response loss repaired in F12 |
| A18 | `config/`, `rate_limit.rs`, `server.rs`, `lib.rs` | Construction validation, safe limits, public embedder contract, feature combinations | Initial; invalid binding aliases in F34; room-code frame bounds in F35; timer overflow in F36 |
| A19 | Metrics, logging, admin, dashboard cache, session records | Bounded resources, accounting consistency, diagnostic claims | Initial; credential diagnostics reviewed in F08 |
| A20 | Native, browser, Fortress, WASM clients | Event application, numeric precision, interop, reconnect, generation resets | Initial; stale peer exchange evidence recorded in F16; invalid probe receipts recorded in F17; buffered ICE rejection recorded in F20; duplicate data-channel replacement recorded in F23; unsupported admission and reset/reopen failure recorded in F24; retired poller retention in F25; unread required-label announcement growth in F26; incoming message backlog in F27; browser input and output retention in F28; native stdout stalls in F29; native stderr stalls in F30; nominated ICE path evidence in F33; native remote shutdown and stream termination in F37; required delivery modes in F39; SCTP reset/reopen lifetime in F40; retired channel retention in F41; browser peer-requested retries in F42; native late retries in F43; browser authoritative replacement waits in F44; wall-clock duration changes in F45 |
| A21 | `formal/`, `trace_validation.rs` | Model/source correspondence, fairness, finite bounds, trace completeness, negative controls | Initial |
| A22 | Tests, helpers, fuzz targets, CI | Oracle independence, missing/duplicate events, skips, mutations, features and platforms | Initial; registry preparation failures recorded in F11; false exchange-success oracle recorded in F17 |

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
