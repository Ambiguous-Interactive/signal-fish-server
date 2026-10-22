# signal-fish-reference-native

A **native Rust reference client** for the Signal Fish protocol v3 with a **real WebRTC stack**
([webrtc-rs](https://github.com/webrtc-rs/webrtc) 0.21.1: actual ICE gathering, DTLS handshakes, and SCTP data
channels). It exists for **conformance and reference** — executable documentation of the client side of
[`docs/protocol.md`](../../docs/protocol.md) and the engine of the in-repo multi-process interop suite. It is
**not a product**: no automatic reconnection, no game loop, no API stability promises.

Design rationale (standalone crate, direct webrtc-rs, path-dependency type reuse, supply-chain scope) lives in
[ADR-0004](../../docs/adr/0004-native-reference-client.md).

The reference engine accepts only `reliable` and `unreliable` data channels.
`reliable` must be ordered with no retry or packet lifetime limit.
`unreliable` must be unordered with `maxRetransmits: 0` and no packet lifetime limit.
The engine closes channels with other delivery settings before admission.
**Migration:** Custom peers must use these settings for the required labels.
The engine cancels their polling tasks when it removes a peer or stops.
Remote channel resets follow earlier queued input before reporting closure.
A remote close or a stopped channel event stream triggers link cleanup and
relay fallback. A missing channel, failed send, or stalled exchange send uses
the same cleanup and keeps the signaling session live. Each local exchange
send has a one-second limit and respects the shorter active operation deadline.
A failed or expired send keeps its exchange debt; a failed signaling write
still ends the session. The engine observes both selected channels as open before
reporting a connected pair. Delayed open callbacks do not revive channels
that already closed. A channel can still close after its state check.
**Migration:** Native library callers must now await `Engine::note_channel_open`.
Network loss still follows the configured P2P deadlines.

The orchestrator bounds each pairing, SDP, and ICE operation by its remaining
P2P window and active run deadline. A zero P2P timeout reports fallback
immediately but allows late negotiation until the run deadline.
An expired operation retires its physical link before queued relay traffic and
server replies resume. Cleanup waits at most 250 ms before stopping the driver.
Logical exchange obligations remain due. A new plan or allowed retry can create
a fresh link. The success-release harness keeps its existing run-window exemption.
It closes other remote labels without retaining them or starting callbacks.
**Migration:** Applications that need extra channels must own their channel
handling. This restriction belongs to these reference clients; the server
continues to relay opaque signaling payloads.

The native build uses qualified local RTC and SCTP source patches.
A closed channel retains its stream ID until both reset directions finish.
Completed stream IDs can be reused; late duplicate resets do not close a new channel.
Accepted text and binary writes enter SCTP before a peer reset can stop the stream.
A successful send means local admission. It does not prove remote receipt.
DTLS handshake retry exhaustion reports a failed link even before remote channel admission.
The patch scope and removal criteria are in
[RTC patch notes](../../third_party/rtc/PATCH.md) and
[SCTP patch notes](../../third_party/rtc-sctp/PATCH.md).

The client accepts at most 64 KiB per received data-channel message, measured
as raw bytes and decoded UTF-8 text. Across all peers and link generations,
pending messages retain at most 256 entries and 1 MiB of decoded text.
Exceeding either limit reports an error and closes the failed P2P link through
relay fallback. The limits bound the application backlog, not WebRTC stack memory.
The 64 KiB limit is a message size limit. It does not guarantee delivery on
`unreliable`. This channel does not retry lost fragments. A burst of large
messages can overflow the remote UDP receive buffer, including on localhost.
Use `reliable` for data that must arrive. Use small, replaceable updates on
`unreliable`. An open channel and zero `bufferedAmount` do not prove receipt.
**Migration:** Split larger payloads into smaller messages and keep consuming
output. Native library consumers must retain `EngineEvent::ChannelMessage`'s
`queue_permit` until the message is handled or discarded, including output waits.
Handle the new `EngineEvent::ChannelReceiveRejected` by retiring the failed link.

Before deciding success at the run cutoff or exit linger, the client checks
ready signaling input and queued engine events. New room state can add gameplay
work. A signaling queue that does not settle within 256 frames ends the run
with a connection error. Receipt credit still uses the complete frame
observation time and the original run window.

## Quick start

One line from the repository root (build server → lint client → run the full suite):

```bash
bash scripts/run-webrtc-interop.sh
```

Manually:

```bash
# 1. Build the server binary the harness spawns (repo root).
cargo build --bin signal-fish-server

# 2. Run the client suite (this directory).
cd clients/native
cargo fmt --check
cargo clippy --all-targets -- -D warnings
SIGNAL_FISH_SERVER_BIN="$(git rev-parse --show-toplevel)/target/debug/signal-fish-server" cargo test
```

Driving one client by hand against a running server:

```bash
cargo run -- --server-url ws://127.0.0.1:3536/v3/ws --create-room --peers 2 --exchange
```

stdout is a machine interface (one JSON event per line, see the event contract below); all logging goes to stderr
(`RUST_LOG`, default `info`).

The client records `P2P link lost` before failed connections, required channels,
or negotiation deadlines remove a peer link. Each record names the peer and
reason. It retains the link generation, candidate counts, remote-description
observation, and channel registration and open observations. An observed open
callback does not prove that a channel is still open. Intentional replacements
and stale callbacks do not produce failure records.

The binary writes diagnostics on one thread outside Tokio. It admits at most
256 records, including formatting and the in-flight write. Each record retains
at most 4 KiB, for at most 1 MiB of admitted record storage. Oversized records
end with `[truncated]` at a UTF-8 boundary. Embedded line breaks are escaped.
The binary discards new diagnostics when the queue fills or stderr fails.
Cached tracing span fields retain at most 4 KiB of text, including updates.
CLI errors and panic diagnostics use the same bounded path. The owner waits
at most one second for diagnostic output after runtime shutdown or before
a main-panic exit with code 101. Enabled `RUST_BACKTRACE` output uses the same
record limits. The hook captures at most one unsymbolicated panic stack.
The worker resolves symbols, so slow symbolication cannot extend the flush
wait. The 1 MiB bound covers record text; it excludes the standard library's
captured-stack and symbol-resolution allocations.

**Migration:** Drain stderr continuously to retain diagnostics. Use stdout
JSONL and process exit codes for protocol outcomes. Diagnostic loss does not
change those outcomes. The library leaves subscriber and panic-hook setup to
its caller. A blocked OS write can retain one worker until the pipe closes or
the process exits. These limits do not bound OS buffers, WebRTC stack memory,
the number of live tracing spans, or allocations inside external formatters.

## CLI reference

Exactly one of `--create-room` / `--join-code` is required; everything else has a default.

| Flag | Default | Meaning |
|------|---------|---------|
| `--server-url <URL>` | (required) | Full WebSocket URL of the signaling endpoint (e.g. `ws://127.0.0.1:3536/v3/ws`; use `/v2/ws` for a faithful v2 run) |
| `--create-room` | — | Create a new room (the `room_created` event carries the code for sibling processes) |
| `--join-code <CODE>` | — | Join an existing room by code. Sets `JoinRoom.join_only`: a code that does not resolve is refused `ROOM_NOT_FOUND` instead of silently creating a room (issue #630) |
| `--peers <N>` | `2` | Expected member count incl. self; `PlayerReady` is sent once N members are seated AND the room is in the Lobby state. Also the default room capacity (`max_players`) when creating |
| `--max-players <N>` | `--peers` | Creator-only room capacity; conflicts with `--join-code`. Values above `--peers` leave open seats after the room finalizes, so a late joiner can seat-fill the running session without any prior departure. Must not sit below `--peers` (the ready barrier could never be reached) |
| `--expect-total-peers <N>` | `--peers` | Distinct members (incl. self, cumulative across departures) that must have been OBSERVED before a successful exit. Late-join incumbents set this above `--peers` so they outlive the session until the joiner arrives; room capacity is `--peers` unless `--max-players` raises it |
| `--leave-on-game-start` | off | Exit 0 after `GameStarting` and its authoritative `SessionPlan` WITHOUT pairing (the plan is logged, not acted on). Used to vacate a seat in a finalized room so a late joiner can seat-fill the running session |
| `--game-name <NAME>` | `reference-native` | Game name for room create/join |
| `--player-name <NAME>` | `RefNative` | Display name |
| `--app-id <ID>` | `reference-native-app` | Public `Authenticate.app_id` (interop servers use an open app-ID policy) |
| `--platform <P>` | `reference-native` | `Authenticate.platform` |
| `--exchange` | off | When each physical P2P pair fully opens, send one text message per channel and require the symmetric receives (see success criteria) |
| `--exchange-release-file <PATH>` | — | Test harness only; requires `--exchange`. Establish every planned pair and finish local ICE gathering, emit `exchange_ready`, then hold the exact channel exchange until PATH exists. Normal exchange behavior is unchanged when omitted |
| `--p2p-rebuild-release-file <PATH>` | — | Loss-harness only; requires `--exchange-release-file` and a nonzero `--p2p-retry-count`. The final retry attempt is reserved: after `exchange_ready`, wait for PATH, then have each pair's designated initiator trigger that coordinated clean-path PairRetry rebuild before exchange release |
| `--unreliable-exchange-release-file <PATH>` | — | Loss-harness only; requires `--exchange-release-file`. After its first gate opens, send and receive the exact reliable message across every planned pair and emit `exchange_reliable_ready`, then hold the exact unreliable message until PATH exists |
| `--relay-payload <TEXT>` | — | After `GameStarting` (+250 ms settle), send one `GameData {"relay_msg": TEXT}` over the WebSocket relay floor and require the other `--peers - 1` members' payloads. A late joiner (entry into a finalized room) arms the send on entry instead — `GameStarting` pre-dates the join — and its receive requirement is waived: payloads sent before the join are never replayed |
| `--cripple-ice` | off | Deterministically break ICE: bind an isolated dummy UDP transport while allowing only unconfigured TCP candidates in the ICE agent, then drop all outbound/inbound `IceCandidate` signals (SDP offer/answer and gathering completion still flow). Forces the relay fallback |
| `--disable-mdns` | off | Test harness only: disable resolution of remote `.local` candidates so packet-loss experiments do not fault their mDNS discovery control plane. Native host candidates are raw IPs in either mode; normal mode retains mDNS query support for browser peers |
| `--ip-family <FAMILY>` | `any` | Restrict the ICE sockets this client binds — and therefore the family of every host candidate it advertises — to `ipv4` or `ipv6`. `any` binds every usable interface address. webrtc 0.21 turns each supplied bind directly into a host candidate, so this is what pins the negotiated path's family. A requested family the host cannot serve fails the process before the WebSocket opens (exit 2), instead of falling back to the other one or degrading to the relay floor. `--cripple-ice` is exempt: that transport is a deliberate dead end and always binds the IPv4 loopback |
| `--drop-ice-from <N>` | — | Matrix-harness fault injection: drop inbound `IceCandidate` signals from the planned peer named `cNN`, while preserving offer/answer signaling, every other P2P edge, and the relay floor. The flag fails loudly if the ordinal does not resolve to exactly one planned peer |
| `--ice-transport-policy <POLICY>` | `all` | ICE candidate policy: `all` permits every gathered candidate type; `relay` requires a TURN-relayed path. The repository's local coturn gate uses `relay` to prove production-minted TURN credentials rather than a direct host path |
| `--p2p-release-file <PATH>` | — | Test harness only: keep processing WebSocket traffic but defer peer-connection creation until PATH exists. The TURN gate uses this to prove the relay floor before ICE establishment begins |
| `--p2p-timeout-secs <S>` | `15` | Window for WebRTC pair establishment before the overall transport status resolves |
| `--p2p-retry-count <N>` | `0` | Bounded coordinated rebuilds for a planned pair whose data channels do not open. Both endpoints must support the reference client's `PairRetry` signal extension; homogeneous recovery tests opt in, while general interoperability stays matchbox-only |
| `--run-for-secs <S>` | `30` | Soft cap: exit 1 if the flag-driven success criteria are still unmet |
| `--max-runtime-secs <S>` | `60` | Watchdog: abort with exit 4 when its absolute deadline is representable; larger accepted values remain beyond the process lifetime as described below |
| `--success-release-file <PATH>` | — | Test harness only: after success criteria hold, emit `success_criteria_met` and stay connected until PATH exists; the normal bounded exit behavior is unchanged when omitted |
| `--require-ice-gathering-complete` | off | Signal-ledger harness only; requires `--success-release-file`. Add end-of-gathering for every live peer-connection generation to the success criteria. Ordinary gameplay barriers do not wait for unrelated candidate transactions after a selected path is connected |
| `--reconnect-release-file <PATH>` | — | Harness only; repeat to drop and restore the current room in order when each path exists. Requires v3 and a room token. Normal runs do not reconnect |
| `--reconnect-resume-file <PATH>` | — | Harness only; hold each restore after the socket drops until its matching path exists. Supply one path per reconnect trigger to coordinate missed traffic or drain refusal |
| `--protocol-version <V>` | `3` | `2` omits every v3 `Authenticate` field — a pure v2 client for mixed-room tests |
| `--supported-topologies <LIST>` | `relay,host,mesh` | Comma-separated topologies advertised in v3 `Authenticate` |
| `--supported-transports <LIST>` | `relay,webrtc` | Comma-separated transports advertised in v3 `Authenticate` |
| `--runtime <FLAVOR>` | `multi` | Tokio runtime flavor: `multi` (multi-threaded) or `current` (single current-thread runtime — the shape most susceptible to being starved by a blocking game loop) |
| `--tick-stall-ms <MS>` | `0` | **Fault injection**: block the orchestrator's executor thread for this many ms after each processed input (`std::thread::sleep`, deliberately not async), simulating a game loop that hogs the runtime instead of continuously driving it. Used by the starved-runtime conformance matrix to pin the server's slow-consumer contract and the docs' "continuously drive your runtime" requirement (`docs/protocol.md`, Delivery reliability and backpressure) |

Each room restore starts a new success barrier. Once the client observes
that barrier's release marker, removing it does not restore that hold.
A send started during the hold keeps its existing local deadline.
Queued receipts use their arrival time. New sends use the remaining run or
linger window.

Duration flags accept the complete `u64` seconds range. When a requested
absolute deadline is later than Tokio can represent on the current platform,
the client treats it as beyond the process lifetime instead of expiring it
immediately; zero and ordinary representable durations retain their exact
boundary behavior.

## Delivery accountability

The v3 receive loop enforces the same exact-gap contract as the server's
conformance auditor. It validates cumulative `DeliveryReport` counters, requires
each loss-counter delta to equal the current report's exact range units, and
retains ranges across reports (one frame carries at most 256) until their
non-overlapping union covers a later sequence hole. Aggregate counters,
`RelayStats`, and supplemental errors never authorize a gap. RelayStats
snapshots are checked for a positive stable interval and monotonic cumulative
counters. Unsupported-format errors are optional rate-limited advisories: when
present they require a prior causal exact report, but need not be adjacent to
it. V2 mode rejects all v3 stamps/reports and remains reliable FIFO; raw
binary game data is always reliable. The default format is `json`, which
rejects binary frames and text `GameDataBinary`. Use
`--game-data-format message_pack` with protocol v2 or v3 to encode relay
payloads as MessagePack and receive ordinary `game_data_received` payloads.
V2 uses the legacy three-field relay envelope. V3 requires delivery stamps.
Both versions accept JSON text fallback from mixed-room senders.
**Migration:** MessagePack runs now emit the decoded value in `payload`,
replacing the former `{encoding, payload}` base64 wrapper. Consumers must
read the application value directly. Use `rkyv` or `protobuf` for opaque bytes.
The opt-in `rkyv` and `protobuf` modes support v2 and v3 with an application
sender header. They also accept JSON text fallback. See [opaque relay probes](#opaque-relay-probes).

Peer lifecycle control has priority and may overtake already-queued old-epoch
data. The client therefore keeps the old cursor long enough to validate that
tail, but suppresses its payload from the application after `PlayerLeft` or a
newer incarnation announcement. A future epoch must have been announced exactly
by `PlayerJoined` or `PlayerReconnected`; after data advances, older epochs are
rejected. `RoomJoined` / `SpectatorJoined` snapshots establish exact paired
`PlayerInfo.(epoch, seq)` baselines. The recipient's own room/spectator transition clears room cursors but
retains connection counters; `Reconnected.sender_watermarks` replaces cursors
and starts a new physical connection accounting lifetime.

Consequently, `game_data_received` is emitted only for the current application
incarnation. A trailing stale frame can be valid for wire accountability without
becoming a JSONL application event.

With `--exchange`, a receive counts only when its JSON object names the actual
sending peer in `from`, names the receiving channel in `channel`, and has
numeric `seq` zero. Only `reliable` and `unreliable` count. Whitespace, key order,
and extra fields do not affect this check. Invalid payloads still emit
`channel_message`; they leave the receive criterion unmet until a valid probe
arrives or the run expires.

## Controlled restore

Reconnect initiation is opt-in and intended for the conformance harness.
Repeat `--reconnect-release-file` to request a fixed number of restores.
Use matching `--reconnect-resume-file` paths to hold each attempt after its
socket drops. The existing run deadline bounds every hold and attempt.
The client does not reconnect after an unrelated socket failure.

Each successful restore keeps the player identity, replaces room membership
and readiness from the snapshot, and adopts the rotated token. It resets
connection delivery counters and old transport and exchange state. A
finalized room requires a fresh session plan and new exchange evidence.
Missed gameplay data is not replayed. The application must resync its own
state; sender watermarks establish the post-restore delivery baseline.

When another peer reconnects or rejoins with the same ID, `--exchange` requires
fresh sends and receipts on both channels for that peer. Other peers retain
their completed exchange. A transport rebuild alone retains exchange evidence.
Each replacement link sends the probes again within the existing exchange window.
This lets a peer recover a probe lost with the old link. Duplicate valid probes
do not add new receive obligations.

The client keeps the first data channel for each label in a peer connection.
It closes duplicate channels so they cannot replace a working channel.
Remote callbacks queue at most one channel per required label on each physical
link, even when the orchestrator has not read its events. Draining the queue or
closing a selected channel does not permit another channel on that link.
Native library callers must now await `Engine::store_remote_channel`.
The native RTC stack retains all live channel statistics and the 64 most recently
closed channel summaries per peer connection. Older closed summaries disappear
from `get_stats`; lifetime opened and closed counters remain cumulative.
The wrapper discards undeliverable channel events after terminal closure.
These bounds do not establish a limit on the entire WebRTC stack's memory.

## JSONL event contract

One JSON object per stdout line, tagged by a snake_case `event` field. Events
keep causal order and carry no timestamps. Drain stdout continuously. The
writer runs outside the async executor. Pending output is limited to 256 events
and 128 MiB of serialized UTF-8, including the event being written and newlines.
Two terminal lines have separate 4096-byte limits.

**Migration:** A closed pipe or output overload now ends the process with exit
code 3. The process requires output to flush before reporting success. Each
final flush wait is limited to one second. The watchdog still exits with code 4
when stdout is unread. A blocked or closed consumer can prevent the final
`error` and `exiting` lines from arriving; always inspect the process exit code.
Library callers must bound `event_output::stdout().drained()` and require
successful flushing before their own successful exit. A blocked system write can retain one bounded writer thread
until the pipe drains, closes, or the process exits.

| Event | Fields | Emitted when |
|-------|--------|--------------|
| `connected` | `runtime`, `tick_stall_ms` | WebSocket connection established; echoes the `--runtime` token and `--tick-stall-ms` value so harnesses can assert the intended runtime/fault shape was in effect |
| `authenticated` | — | Server accepted the legacy `Authenticate` app-ID handshake |
| `protocol_info` | `negotiated_version` | Negotiation result (v2 connections report `2`) |
| `room_created` | `room_code` | This client created the room (harnesses scrape the code) |
| `room_joined` | `room_id`, `player_id`, `lobby_state` | Seated in the room; `lobby_state` ∈ `waiting`/`lobby`/`finalized` — `finalized` marks a late join into a running session |
| `reconnect_started` | `attempt` | Harness mode dropped the old socket; attempts start at one |
| `reconnected` | `attempt`, `player_id`, `lobby_state`, `current_players`, `sender_watermarks`, `replay`, `token_rotated` | A restore replaced room and delivery state; the token stays private |
| `peer_joined` | `player_id`, optional `epoch` | Another player joined or reconnected |
| `player_left` | `player_id`, optional `epoch`/`final_seq` | Another player left with its v3 terminal watermark |
| `game_starting` | `is_authority` | Lobby finalized (this client's own authority flag); never re-broadcast to late joiners |
| `session_plan` | `topology`, `transport`, `host`, `peers[{player_id, initiate}]`, `ice_servers_count`, `fallback` | The full authoritative per-recipient v3 directive; Relay/Relay carries no peers. This WebRTC reference client explicitly rejects Direct plans, reports `connected: false`, and uses the relay fallback |
| `new_peer` | `peer_id`, `you_initiate` | Compatible incremental pairing directive (the universal server uses full plans) |
| `p2p_gate_released` | `pending_pairs` | Native-only harness proof that the release-file barrier opened and released the expected planned pairs |
| `signal_sent` | `to`, `kind` | Outbound `Signal` relayed (`kind` ∈ `offer`/`answer`/`ice_candidate`/`pair_retry`/`other`) |
| `signal_received` | `from`, `kind` | Inbound `Signal` arrived (emitted even when `--cripple-ice` then drops it) |
| `ice_candidate_dropped` | `from` | Native-only `--drop-ice-from` discarded this peer's inbound candidate after `signal_received` made the signaling hop observable. The older shared `--cripple-ice` contract remains unchanged and does not emit this event |
| `local_candidate` | `peer`, `candidate_type`, `address`, `port`, `protocol` | Native-only: a gathered local ICE candidate was advertised to `peer`, reported after the relay so the set describes what the remote side can see. `candidate_type` ∈ `host`/`srflx`/`prflx`/`relay`. The relay-only TURN cell requires every entry to be `relay`, and the IPv6 cell requires every entry to be an IPv6 host; without it, a session that gathers nothing reports only "no candidate pairs" |
| `pc_state` | `peer`, `state` | RTCPeerConnection state transition (informational) |
| `channel_open` | `peer`, `label` | One data channel reached open (`label` ∈ `reliable`/`unreliable`) |
| `channel_closed` | `peer`, `label` | A required data channel started closing, closed, or became unreadable; the unusable pair is removed and relay fallback remains live |
| `channel_message_sent` | `peer`, `label`, `text` | An `--exchange` message was sent |
| `channel_message` | `peer`, `label`, `text` | A data-channel text message arrived |
| `p2p_pair_connected` | `peer` | Both channels first opened for this logical peer obligation |
| `p2p_pair_reconnected` | `peer` | Both channels reopened for a retained logical pair after PairRetry or a new session plan, even after the P2P deadline |
| `selected_candidate_pair` | `peer`, `local_candidate_type`, `remote_candidate_type`, `local_candidate_address`, `remote_candidate_address` | Native-only nominated ICE pair after both channels open. The client reads the transport pair directly, including the actual type and address of a learned `prflx` remote candidate. Exchange proceeds while the probe runs, but clean exit requires this event. TURN requires both types to be `relay`. IPv6 requires a local `host`, a remote `host` or `prflx`, and two concrete dialable IPv6 addresses. Missing or `null` address evidence fails the IPv6 gate |
| `exchange_ready` | — | Harness-only barrier: every planned pair is open and local ICE gathering is complete, while `--exchange-release-file` still holds application exchange |
| `exchange_reliable_ready` | — | Loss-harness barrier: every planned pair has sent and received its exact reliable exchange, while `--unreliable-exchange-release-file` still holds the unreliable half |
| `transport_status_sent` | `transport`, `connected` | An overall `TransportStatus` state change went out under the transport-fallback contract |
| `peer_transport_status` | `peer`, `transport`, `connected` | A same-room peer's reported state changed (server fan-out) |
| `game_data_sent` | — | The `--relay-payload` GameData was sent |
| `game_data_received` | `from`, `payload`, optional `seq`/`epoch`/`class`/`key` | A validated, application-current relayed GameData payload arrived; v3 accountability stamps are preserved while lifecycle-overtaken stale tails are suppressed |
| `delivery_report` | `report` | A validated v3 cumulative delivery-counter update plus any exact, causally prior gap ranges |
| `fallback_engaged` | — | The P2P window resolved with ZERO connected pairs; the relay floor carries the session |
| `success_criteria_met` | — | Harness-only barrier emitted when criteria hold and `--success-release-file` was supplied; the client remains connected until the path exists |
| `error` | `message` | Non-fatal or fatal error (fatal ones are followed by `exiting`) |
| `exiting` | `code` | Final event before process exit |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | All flag-driven success criteria met within the run window |
| `1` | `--run-for-secs` elapsed with unmet criteria (the `error` event lists them) |
| `2` | Protocol error (app-ID handshake/join rejection, malformed frame), or a local pre-flight failure such as an `--ip-family` the host cannot serve. Exit 2 is **also** clap's default exit code for CLI-usage errors (unknown/missing flags); on that path the usage message goes to stderr and NO `exiting` event is emitted (the event stream never starts) |
| `3` | Transport failure (connect failed, socket died mid-session) |
| `4` | `--max-runtime-secs` watchdog fired (hard abort) |
| `101` | Unexpected panic in main; stderr flush is limited to one second, and `exiting` may be absent |

Each `Authenticate`, `JoinRoom`, and `Reconnect` command has one 20-second
response window. Prelude frames and old-socket cleanup retries do not restart
it. Expiry ends the run with exit code 3. Start a fresh client after this failure.
Runtime WebSocket writes also have a 20-second limit, including their flush.
Relay probes also use the remaining run window. An established successful
harness hold or active exit linger keeps its allowed extension. The client
does not start a relay probe at or after that window ends.
A failed write ends the session with exit code 3. The client does not retry
a partial frame on the same connection.

Relay receipt criteria use the time when the client observes a complete frame.
Timely receipts remain valid through decoding and queued processing.
Late receipts keep the criterion unpaid. The client still validates and reports
late frames. Existing successful holds and active linger windows still apply.

Data-channel receipts use the time when the channel poll completes, before
receive admission and text decoding. Admission waits cannot make timely input late.
Receipts keep this time and logical peer ownership through physical loss,
negotiation timeout, and ordinary retry.
A newer authoritative plan, departure, or fresh incarnation invalidates queued
old-link receipts. The original receipt cutoff still applies.

Deferred exchange sends let the client read signaling between channel attempts.
A stalled send keeps its unmet exchange criteria and triggers relay fallback
for its peer. The run deadline still applies.

## Signal payload convention (matchbox `PeerSignal`)

Per [ADR-0002](../../docs/adr/0002-matchbox-compatibility.md) the opaque `signal` field uses the matchbox shape —
exactly one key, one string value:

```json
{ "Offer": "<sdp>" }
{ "Answer": "<sdp>" }
{ "IceCandidate": "<payload>" }
```

The `IceCandidate` payload this client SENDS is the JSON serialization of webrtc-rs's `RTCIceCandidateInit`
(camelCase `candidate` / `sdpMid` / `sdpMLineIndex` / `usernameFragment`) — byte-compatible with what
`matchbox_socket` emits:

```json
{ "IceCandidate": "{\"candidate\":\"candidate:...\",\"sdpMid\":\"\",\"sdpMLineIndex\":0,\"usernameFragment\":null}" }
```

On receive it tolerates a payload that is not valid `RTCIceCandidateInit` JSON by treating it as a bare candidate
string (interop with minimal clients).

Every wire `Signal` also carries the UUID from the latest
`SessionPlan.generation`. A changed plan generation rebuilds retained physical
peer connections with the new ICE configuration; signals from any older or
unknown generation are discarded.

## Transport-status semantics

- Before status resolution, an incomplete pair is rebuilt at most
  `--p2p-retry-count` times. A `PairRetry` marker crosses the same ordered
  signaling relay before the fresh Offer, so both endpoints discard the old
  generation while retaining the server-authored glare role. The default is
  zero because this coordination extension is only safe when both endpoints
  implement it; general interoperability must not assume that.
- After `PairRetry`, peers send SDP and ICE as
  `{"RetrySignal":{"attempt":1,"signal":{"Answer":"..."}}}`.
  The attempt must match the current retry. Peers drop bare SDP/ICE after a retry
  and discard envelopes for other attempts. Initial signaling stays bare.
  This prevents an old answer in flight from changing the replacement link.
  **Migration:** Both peers in an opt-in retry session must implement this envelope.
  The server still relays the payload unchanged.
- A peer that formed in the current server generation can accept a newer
  in-budget remote retry after its original window expires, including when
  the old channel closes before the marker arrives. The receiver needs no
  local rebuild gate. A fresh retry wait is bounded by `--p2p-timeout-secs`;
  an active window stays fixed. A late replacement still reports recovery.
  Departure and a new server generation clear this formation history.
- A retained logical pair emits `p2p_pair_reconnected` once when both replacement
  channels open, after a retry or a new authoritative plan. Timeout alone emits
  no recovery event. First formation emits only `p2p_pair_connected`. Departure
  clears the logical history, so rejoining creates a new obligation. Completed
  exchanges survive a transport replacement.
- The overall WebRTC status resolves when every currently expected pair is connected or at
  `--p2p-timeout-secs`. `connected: true` iff **at least one** pair is connected; a zero-pair resolution also
  emits `fallback_engaged`.
- Membership churn reports later real `true`/`false` state changes. Unchanged states are suppressed, matching
  the server's `(transport, connected)` deduplication (see `PeerTransportStatus` in
  [`docs/protocol.md`](../../docs/protocol.md)).
- **Peer-status wait (deliberate deviation):** when a WebRTC session is expected, the success criteria
  additionally require a `peer_transport_status` from every expected pair peer before exit. Without it a fast
  client could disconnect before slower siblings' reports fan out, making multi-process assertions racy. This
  cannot deadlock — expected pairs are v3+WebRTC by the server's session predicate and a reference peer always
  resolves by its own timeout. The wait is **waived after a late join**: fan-outs fire once, at report time, so
  reports that pre-date this client's entry are never observable (the server replays nothing).

## Interop scenario matrix

All scenarios live in [`tests/interop_e2e.rs`](tests/interop_e2e.rs) and spawn the REAL server binary plus real
client processes (loopback only; the interop server config disables TURN with zero STUN URLs, so plans carry
`ice_servers_count: 0` and nothing touches an external network).

| Scenario | native ↔ native | browser ↔ native |
|----------|-----------------|------------------|
| Mesh N=3 full WebRTC + live relay floor | ✅ `mesh_n3_full_webrtc_session_with_live_relay_floor` | ✅ `mixed_mesh_n3_full_webrtc_with_browser` |
| Host star N=3 | ✅ `host_star_n3_webrtc` | ✅ `host_star_n3_browser_client` |
| Crippled-ICE relay fallback | ✅ `mesh_n3_partial_ice_cripple_relay_fallback` | ✅ `mesh_n3_browser_crippled_ice_fallback` |
| Pairwise ICE partition with healthy partial mesh + relay floor | ✅ nightly `pairwise_ice_partition_preserves_partial_mesh_and_relay_floor` | — |
| Late join (authoritative full-plan seat fill) | ✅ `late_join_authoritative_replan_real_webrtc_n3` | — (native-only cell) |
| Mixed v2/v3 relay floor | ✅ `mixed_v2_v3_n3_relay_floor_with_reference_client` | ✅ `mixed_v2_browser_v3_native_relay_floor` |
| Cross-platform two-peer live WebRTC | ✅ `two_peer_mesh_exchanges_over_live_webrtc` | — |
| Browser ↔ browser mesh (Chromium↔Chromium pair) | n/a | ✅ `browser_pair_mesh_n3` |
| mDNS `.local` obfuscation trap | n/a | ✅ `mesh_n3_browser_mdns_obfuscation` |
| Mid-handshake close → one `error` + prompt exit 3 | n/a | ✅ `browser_cli_mid_handshake_close_single_error_exit_3` |
| SIGTERM/SIGKILL Chromium teardown (orphan reaper) | n/a | ✅ `browser_cli_signal_teardown_reaps_chromium` |
| TURN-only pair + mismatched-secret fallback controls | ✅ `turn_only_pair_selects_relay_candidates_and_keeps_websocket_floor_live`; `mismatched_turn_secret_fails_p2p_and_uses_websocket_fallback` | — |
| IPv6-only host path (both channels over IPv6) | ✅ `ipv6_only_mesh_pair_exchanges_on_a_host_ipv6_path` | — |

The browser cells live in [`tests/browser_interop_e2e.rs`](tests/browser_interop_e2e.rs) behind the
`browser-interop` cargo feature (this crate's default suite never compiles them; they additionally need
`SIGNAL_FISH_BROWSER_CLI` pointing at the built [browser client](../browser/README.md) bundle).

Scenario 6 is the smallest complete live WebRTC cell: two real clients negotiate a direct host path and
exchange exact messages on both SCTP channel labels. With no STUN or TURN configured, rtc may learn the
remote host socket from an inbound connectivity check before its signaled candidate is registered, so that
side is reported with its actual nominated `prflx` type and address. For legacy diagnostic compatibility,
the family-agnostic baseline also accepts an addressless peer-reflexive report only
when both clients advertised non-empty host-only UDP candidate sets. CI runs the selector on Linux on every
push and pull request, and on Windows and macOS on the daily cron cohort (see [Platform
coverage](#platform-coverage)).

Scenario 8 is the IPv6 cell: both clients run with `--ip-family ipv6`, so only IPv6 host candidates can be
advertised. The nominated pair must have a local host and a remote host or peer-reflexive candidate, with
two concrete dialable IPv6 addresses plus the exact
exchange on both channel labels. It shares the file's scenario-serialization guard, so it never overlaps another
multi-process cell. Signaling still runs over the harness server's IPv4 loopback listener, so the IPv6 claim is
about the WebRTC data path (ICE, DTLS, SCTP), not the WebSocket. Its precondition applies the client's own
interface-selection rule, so a runner that cannot serve IPv6 fails the cell with an actionable message; it is
never skipped. The cell also requires every **advertised** candidate to be an IPv6 host, so a
`--ip-family` that silently became a no-op cannot pass on a dual-stack host that happens to select IPv6
anyway.

## Remote ICE input limits

Each physical peer link admits at most 128 remote candidates and 64 KiB of
candidate payload in total. Each payload must fit in 4,096 UTF-8 bytes, including
JSON fields. The limits apply before and after the remote description is set.
Rejected candidates emit an error or warning without their content. They do
not clear accepted candidates or close the peer link. When the stack rejects
a buffered candidate, the client applies the remaining candidates and
continues the offer or answer. Failed stack applications consume budget too.
A replacement peer link receives a fresh budget.

## ICE socket selection

`--ip-family` decides which families this client binds; two rules decide _which addresses_:

1. **Every usable interface address.** webrtc 0.21 turns each supplied bind directly into a host candidate, so
   a wildcard bind would advertise `0.0.0.0`. Unspecified, multicast, and IPv6 link-local addresses are dropped
   (the candidate grammar cannot carry a scope ID).
2. **The kernel's own source address for each configured STUN/TURN server.** Interface enumeration reports only
   interfaces whose operational status is _up_, while reachability is a property of the routing table. A Docker
   bridge, a VPN adapter, or any interface that is momentarily carrier-down is therefore invisible to (1) while
   still being the only address that can reach the server. webrtc starts a binding request or allocation from
   _every_ bound socket, so when none of them routes to the server the session gathers no relay candidate at
   all — and under `--ice-transport-policy relay`, no candidate whatsoever. The source address is obtained by
   `connect()`ing an unbound UDP socket, which performs the route lookup without sending a packet.

The two sets are unioned and then filtered by the same rule, so a routing answer can never widen `--ip-family`.
`--cripple-ice` is exempt from (2): that transport exists to be unreachable.

Rule (2) runs on the task that also pumps the WebSocket, so it is bounded twice: the whole probe shares a
two-second budget, and its answers are resolved once per endpoint set rather than once per pair. A resolver that
hangs therefore costs the union and nothing else — never the pairing, and never the server's activity deadlines.
A replan that changes servers re-probes; a credential rotation, which keeps the same URLs, does not.

CI runs the native suite via [`scripts/run-webrtc-interop.sh`](../../scripts/run-webrtc-interop.sh) in
`.github/workflows/webrtc-interop.yml`, and the browser cells via
[`scripts/run-browser-interop.sh`](../../scripts/run-browser-interop.sh) in
`.github/workflows/browser-interop.yml`. The isolated TURN-only controls run via
[`scripts/run-turn-interop.sh`](../../scripts/run-turn-interop.sh) in
`.github/workflows/turn-interop.yml`; they start a digest-pinned local coturn
container and never depend on a public STUN/TURN service.

## Platform coverage

The two kinds of evidence this crate carries are deliberately distinct — a green lane means only what its column
says:

| Platform | Compile + unit suite | Live WebRTC transport |
|----------|----------------------|-----------------------|
| Linux (`ubuntu-latest`) | ✅ `Native Client Interop (Linux)` | ✅ every native, browser, TURN-only, and IPv6 cell |
| Windows (`windows-latest`) | ✅ `Native Client Build + Live WebRTC (windows-latest)` | ✅ two-peer direct host/host-or-prflx path + both SCTP channels |
| macOS (`macos-latest`) | ✅ `Native Client Build + Live WebRTC (macos-latest)` | ✅ two-peer direct host/host-or-prflx path + both SCTP channels |

The `native-platforms` matrix in `.github/workflows/webrtc-interop.yml` runs, on the repository MSRV:
`cargo metadata --locked`, `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`,
`cargo test --locked --all-targets --no-run`, and `cargo test --locked --lib --bins`. The `--no-run` step is
what actually builds and links the multi-process cells — clippy only type-checks them (`--emit=metadata`, no
codegen). The matrix then builds the real server binary and runs
`two_peer_mesh_exchanges_over_live_webrtc` on each host. That cell executes the platform's interface
enumeration, UDP binding, ICE, DTLS, SCTP, child-process, and path semantics and requires a corroborated direct
path plus an exact exchange on both data channels. Linux is absent from the matrix because the `interop` job
runs the full eight-scenario suite,
including the same two-peer cell. Windows and macOS now carry a live transport proof, while the larger topology,
fault-injection, TURN, browser, and IPv6 matrices remain Linux-specific evidence.

Cadence (issue #512, the #513 cohort pattern): Linux runs on every push and pull request. The Windows and
macOS matrix runs on the daily cron against main — plus manual dispatch — because macOS bills at 10x and
Windows at 2x Linux per minute and the server deploys on Linux. Cross-platform regressions surface next-day.

## Troubleshooting

When a peer connection times out, stderr logs `P2P connection timed out` for
that peer. The snapshot names the physical-link generation, remote-description
status, admitted and queued remote ICE candidate counts, and both required
channels. `registered` means the client stored a selected channel. `open_observed`
means the client handled its open callback. These are local observations;
they do not prove current channel or network state. A `null` snapshot means
the physical link was already removed. The snapshot does not wait for the
WebRTC driver. Compare both endpoints and their `pc_state` events when
investigating a failed connection.

Library callers can read the same local snapshot with
`Engine::diagnostic_snapshot(peer)`.

Each required channel also reports physical-generation `receipts` counts:
`raw` counts complete messages returned by the driver poll; `admitted` counts
successful publication to the client's event queue; `rejected` counts messages
refused during decoding or queue-capacity checks; `discarded` counts input
stopped by observed link loss or a closed client event queue. Admission does
not prove the orchestrator consumed the message or delivered its stdout event.
Compare those counts with the sender's events and the receiver's
`channel_message` events. Counts start at zero for each physical generation.
Snapshots read counters separately without waiting. A raw message can still
be waiting for admission, or its poller can be cancelled during admission.
A difference between raw and classified counts does not prove network loss.

At run timeout, stderr also reports a snapshot for each peer with missing
exchange sends or receipts, including connected peers and departed peers with
unpaid exchange debt. Each report names missing sends, missing receipts, and
logical connection state. A `link: null` field means the physical link was removed.
Compare the reports with both endpoints' `channel_message_sent` and
`channel_message` events. A report does not prove the cause of a missing receipt.

- **`SIGNAL_FISH_SERVER_BIN is not set` panic:** the interop tests drive the real server binary. Run
  `cargo build --bin signal-fish-server` at the repository root and export
  `SIGNAL_FISH_SERVER_BIN=<repo>/target/debug/signal-fish-server`, or just use
  `bash scripts/run-webrtc-interop.sh`, which does both.
- **`SIGNAL_FISH_SERVER_BIN points at ... not a file`:** the path is stale (e.g. a `--release` binary was
  expected but only the debug profile was built). Rebuild, or re-run the script with the matching profile.
- **First build is slow:** the webrtc dependency tree is large; budget a few minutes cold. The crate mirrors the
  root dev-profile trick (`debug = 1` for the leaf, `debug = 0` for dependencies) to keep rebuilds fast.
- **Unit tests pass but interop hangs locally:** the harness passes ordinary, representable
  `--max-runtime-secs` values, so every client process self-bounds with exit 4, and every harness await carries
  a deadline that panics with the child's recent events and stderr — check that diagnostic output first; it
  names the event being awaited.

## Opaque relay probes

Both reference clients use an application header for `--relay-payload` with
`--game-data-format rkyv|protobuf`. This permits sender attribution on v2,
where the server relays raw bytes. It does not add a server wire envelope,
capability, or a general rkyv/protobuf serializer.

| Offset | Length | Content |
| --- | --- | --- |
| 0 | 4 | ASCII `SFRP` |
| 4 | 1 | Probe version `1` |
| 5 | 16 | Sender UUID in network byte order |
| 21 | Remaining bytes | Exact UTF-8 bytes from `--relay-payload` |

The header counts toward the server's existing payload limit. Empty payloads
are valid. Receivers remove exactly one header and preserve every payload byte.
Events still contain the encoding token and base64 of the original CLI payload.
V2 emits no delivery stamps or reports. Its sender ID comes from the application
header. V3 validates its server delivery stamps and requires a tagged probe's
sender to match the server sender. Compatible JSON fallback has no probe header.

**Migration:** Upgrade both reference clients for opaque v2 or mixed v2/v3
runs. All new opaque probe senders use this header on both versions. A new v3
receiver also accepts old untagged payloads, except the reserved `SFRP` prefix.
Old v3 receivers expose the new header as part of their payload, so upgrade
both ends when comparing original CLI payloads. Truncated headers and unknown
probe versions are errors. This framing belongs only to reference probes;
other applications keep their own opaque payload schemas.
