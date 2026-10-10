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
| A02 | `websocket/handler.rs`, `routes.rs`, `mod.rs` | Upgrade, routes, frame limits, TLS/plain entrypoint parity | Initial; HTTP/2 route rejection recorded in F13 |
| A03 | `auth/`, `security/`, handshake in `websocket/connection.rs` | Authenticate, app isolation, token binding, origin, admission, expiry, replay | Initial; concurrent handshake retry-budget loss recorded in F15 |
| A04 | `server/connection_manager.rs`, `websocket/connection.rs` | Identity fencing, socket ownership, reader/writer shutdown, cancellation | Initial; caller cancellation and failure cleanup repaired in F07/F10 |
| A05 | `reconnection.rs`, `server/reconnection_service.rs` | Claim, restore, rollback, token rotation, reconnect races and expiry | Initial; embedded receiver-loss rollback in F18; size-refusal rollback in F21; known socket-close rollback in F22 |
| A06 | Control replay and `Reconnected` snapshots | Snapshot precedence, replay completeness, lost responses, resynchronization | Initial; known receiver closure before response submission in F18; aggregate replay bounds in F21; socket close before queue commit in F22 |
| A07 | `server/room_service.rs`, `database/` | Join, leave, capacity, passwords, room codes, tenant ownership, partial admission | Initial |
| A08 | `server/ready_state.rs`, `coordination/room_coordinator.rs` | PlayerReady, StartGame, membership at commit, readiness snapshots, publication | Initial |
| A09 | `server/authority.rs`, `moderation.rs` | AuthorityRequest, kick, ban, unban, transfer, code rotation, access changes | Initial |
| A10 | `server/spectator_service.rs`, `spectator_handlers.rs` | Join/leave spectator, role exclusion, rosters, stale detach, moderation | Pending |
| A11 | `server/session_policy.rs`, `signaling.rs` | ProvideConnectionInfo, Signal, plan selection, host eligibility, generations, fallback | Initial |
| A12 | `server/message_router.rs`, `relay_policy.rs` | Dispatch, TransportStatus, negotiated capabilities, stale source identity | Initial |
| A13 | `server/game_data.rs`, `coordination/mod.rs` | JSON/binary GameData, acceptance stamps, exact recipient set, fan-out | Initial |
| A14 | `coordination/outbound_queue.rs`, `protocol/delivery.rs` | Reliable/latest/volatile, sequence ranges, generations, sojourn, bounded memory | Initial |
| A15 | `websocket/sending.rs`, `batching.rs`, writer in `connection.rs` | Encoding, partial writes, idle reports, cancellation, terminal close | Initial; aggregate reconnect admission bound in F21; socket close before queue commit in F22 |
| A16 | `server/heartbeat.rs`, `maintenance.rs`, `deadline.rs`, `distributed.rs`, `retry.rs` | Ping, deadlines, clock jumps, lease ownership, GC, stale cleanup | Initial |
| A17 | `server/shutdown.rs`, `main.rs`, deployment configs | Drain, restart, room routing, directional partitions, process failure | Initial; plain HTTP response loss repaired in F12 |
| A18 | `config/`, `rate_limit.rs`, `server.rs`, `lib.rs` | Construction validation, safe limits, public embedder contract, feature combinations | Pending |
| A19 | Metrics, logging, admin, dashboard cache, session records | Bounded resources, accounting consistency, diagnostic claims | Initial; credential diagnostics reviewed in F08 |
| A20 | Native, browser, Fortress, WASM clients | Event application, numeric precision, interop, reconnect, generation resets | Initial; stale peer exchange evidence recorded in F16; invalid probe receipts recorded in F17; buffered ICE rejection recorded in F20; duplicate data-channel replacement recorded in F23 |
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

Distinct optional labels still have no application-level channel-count bound.
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
