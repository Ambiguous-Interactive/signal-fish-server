# signal-fish-reference-native

A **native Rust reference client** for the Signal Fish protocol v3 with a **real WebRTC stack**
([webrtc-rs](https://github.com/webrtc-rs/webrtc) 0.20: actual ICE gathering, DTLS handshakes, and SCTP data
channels). It exists for **conformance and reference** — executable documentation of the client side of
[`docs/protocol.md`](../../docs/protocol.md) and the engine of the in-repo multi-process interop suite. It is
**not a product**: no reconnection logic, no game loop, no API stability promises.

Design rationale (standalone crate, direct webrtc-rs, path-dependency type reuse, supply-chain scope) lives in
[ADR-0004](../../docs/adr/0004-native-reference-client.md).

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

## CLI reference

Exactly one of `--create-room` / `--join-code` is required; everything else has a default.

| Flag | Default | Meaning |
|------|---------|---------|
| `--server-url <URL>` | (required) | Full WebSocket URL of the signaling endpoint (e.g. `ws://127.0.0.1:3536/v3/ws`; use `/v2/ws` for a faithful v2 run) |
| `--create-room` | — | Create a new room (the `room_created` event carries the code for sibling processes) |
| `--join-code <CODE>` | — | Join an existing room by code |
| `--peers <N>` | `2` | Expected member count incl. self; `PlayerReady` is sent once N members are seated AND the room is in the Lobby state. Also sets the room capacity (`max_players`) when creating |
| `--expect-total-peers <N>` | `--peers` | Distinct members (incl. self, cumulative across departures) that must have been OBSERVED before a successful exit. Late-join incumbents set this above `--peers` so they outlive the session until the joiner arrives; room capacity stays `--peers` |
| `--leave-on-game-start` | off | Exit 0 after `GameStarting` and its authoritative `SessionPlan` WITHOUT pairing (the plan is logged, not acted on). Used to vacate a seat in a finalized room — the server only finalizes full rooms, so late joins are seat fills |
| `--game-name <NAME>` | `reference-native` | Game name for room create/join |
| `--player-name <NAME>` | `RefNative` | Display name |
| `--app-id <ID>` | `reference-native-app` | `Authenticate.app_id` (interop servers run with WebSocket auth disabled) |
| `--platform <P>` | `reference-native` | `Authenticate.platform` |
| `--exchange` | off | When a P2P pair fully opens, send exactly one text message per channel and require the symmetric receives (see success criteria) |
| `--exchange-release-file <PATH>` | — | Test harness only; requires `--exchange`. Establish every planned pair and finish local ICE gathering, emit `exchange_ready`, then hold the exact channel exchange until PATH exists. Normal exchange behavior is unchanged when omitted |
| `--relay-payload <TEXT>` | — | After `GameStarting` (+250 ms settle), send one `GameData {"relay_msg": TEXT}` over the WebSocket relay floor and require the other `--peers - 1` members' payloads. A late joiner (entry into a finalized room) arms the send on entry instead — `GameStarting` pre-dates the join — and its receive requirement is waived: payloads sent before the join are never replayed |
| `--cripple-ice` | off | Deterministically break ICE: bind no UDP transport sockets AND drop all outbound/inbound `IceCandidate` signals (SDP offer/answer still flows). Forces the relay fallback |
| `--disable-mdns` | off | Test harness only: disable resolution of remote `.local` candidates so packet-loss experiments do not fault their mDNS discovery control plane. Native host candidates are raw IPs in either mode; normal mode retains mDNS query support for browser peers |
| `--drop-ice-from <N>` | — | Matrix-harness fault injection: drop inbound `IceCandidate` signals from the planned peer named `cNN`, while preserving offer/answer signaling, every other P2P edge, and the relay floor. The flag fails loudly if the ordinal does not resolve to exactly one planned peer |
| `--ice-transport-policy <POLICY>` | `all` | ICE candidate policy: `all` permits every gathered candidate type; `relay` requires a TURN-relayed path. The repository's local coturn gate uses `relay` to prove production-minted TURN credentials rather than a direct host path |
| `--p2p-release-file <PATH>` | — | Test harness only: keep processing WebSocket traffic but defer peer-connection creation until PATH exists. The TURN gate uses this to prove the relay floor before ICE establishment begins |
| `--p2p-timeout-secs <S>` | `15` | Window for WebRTC pair establishment before the overall transport status resolves |
| `--p2p-retry-count <N>` | `0` | Bounded coordinated rebuilds for a planned pair whose data channels do not open. Both endpoints must support the reference client's `PairRetry` signal extension; homogeneous recovery tests opt in, while general interoperability stays matchbox-only |
| `--run-for-secs <S>` | `30` | Soft cap: exit 1 if the flag-driven success criteria are still unmet |
| `--max-runtime-secs <S>` | `60` | Hard watchdog: abort with exit 4 no matter what (the no-hang guarantee) |
| `--success-release-file <PATH>` | — | Test harness only: after success criteria hold, emit `success_criteria_met` and stay connected until PATH exists; the normal bounded exit behavior is unchanged when omitted |
| `--protocol-version <V>` | `3` | `2` omits every v3 `Authenticate` field — a pure v2 client for mixed-room tests |
| `--supported-topologies <LIST>` | `relay,host,mesh` | Comma-separated topologies advertised in v3 `Authenticate` |
| `--supported-transports <LIST>` | `relay,webrtc` | Comma-separated transports advertised in v3 `Authenticate` |
| `--runtime <FLAVOR>` | `multi` | Tokio runtime flavor: `multi` (multi-threaded) or `current` (single current-thread runtime — the shape most susceptible to being starved by a blocking game loop) |
| `--tick-stall-ms <MS>` | `0` | **Fault injection**: block the orchestrator's executor thread for this many ms after each processed input (`std::thread::sleep`, deliberately not async), simulating a game loop that hogs the runtime instead of continuously driving it. Used by the starved-runtime conformance matrix to pin the server's slow-consumer contract and the docs' "continuously drive your runtime" requirement (`docs/protocol.md`, Delivery reliability and backpressure) |

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
binary game data is always reliable. This runtime negotiates
`game_data_format: "json"`, so an incoming binary frame or text
`GameDataBinary` is a protocol error. The strict MessagePack decoder remains a
tested protocol utility for binary-capable clients.

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

## JSONL event contract

One JSON object per stdout line, tagged by a snake_case `event` field. Per-client ordering is causal (a single
emitter task); events carry no timestamps. If the stdout consumer closes the pipe mid-run, the client never
panics on the resulting `EPIPE`: the failure is logged to stderr once, further events are suppressed, and the
process continues to its normal bounded exit.

| Event | Fields | Emitted when |
|-------|--------|--------------|
| `connected` | `runtime`, `tick_stall_ms` | WebSocket connection established; echoes the `--runtime` token and `--tick-stall-ms` value so harnesses can assert the intended runtime/fault shape was in effect |
| `authenticated` | — | Server accepted `Authenticate` |
| `protocol_info` | `negotiated_version` | Negotiation result (v2 connections report `2`) |
| `room_created` | `room_code` | This client created the room (harnesses scrape the code) |
| `room_joined` | `room_id`, `player_id`, `lobby_state` | Seated in the room; `lobby_state` ∈ `waiting`/`lobby`/`finalized` — `finalized` marks a late join into a running session |
| `peer_joined` | `player_id` | Another player joined |
| `player_left` | `player_id` | Another player left |
| `game_starting` | `is_authority` | Lobby finalized (this client's own authority flag); never re-broadcast to late joiners |
| `session_plan` | `topology`, `transport`, `host`, `peers[{player_id, initiate}]`, `ice_servers_count`, `fallback` | The full authoritative per-recipient v3 directive; Relay/Relay carries no peers. This WebRTC reference client explicitly rejects Direct plans, reports `connected: false`, and uses the relay fallback |
| `new_peer` | `peer_id`, `you_initiate` | Compatible incremental pairing directive (the universal server uses full plans) |
| `p2p_gate_released` | `pending_pairs` | Native-only harness proof that the release-file barrier opened and released the expected planned pairs |
| `signal_sent` | `to`, `kind` | Outbound `Signal` relayed (`kind` ∈ `offer`/`answer`/`ice_candidate`/`pair_retry`/`other`) |
| `signal_received` | `from`, `kind` | Inbound `Signal` arrived (emitted even when `--cripple-ice` then drops it) |
| `ice_candidate_dropped` | `from` | Native-only `--drop-ice-from` discarded this peer's inbound candidate after `signal_received` made the signaling hop observable. The older shared `--cripple-ice` contract remains unchanged and does not emit this event |
| `pc_state` | `peer`, `state` | RTCPeerConnection state transition (informational) |
| `channel_open` | `peer`, `label` | One data channel reached open (`label` ∈ `reliable`/`unreliable`) |
| `channel_closed` | `peer`, `label` | A required data channel closed or became unreadable; the unusable pair is removed and relay fallback remains live |
| `channel_message_sent` | `peer`, `label`, `text` | An `--exchange` message was sent |
| `channel_message` | `peer`, `label`, `text` | A data-channel text message arrived |
| `p2p_pair_connected` | `peer` | BOTH channels toward `peer` are open |
| `selected_candidate_pair` | `peer`, `local_candidate_type`, `remote_candidate_type` | Native-only selected ICE pair after both channels open; the local TURN gate requires both candidate types to be `relay` |
| `exchange_ready` | — | Harness-only barrier: every planned pair is open and local ICE gathering is complete, while `--exchange-release-file` still holds application exchange |
| `transport_status_sent` | `transport`, `connected` | An overall `TransportStatus` state change went out (Appendix G) |
| `peer_transport_status` | `peer`, `transport`, `connected` | A same-room peer's reported state changed (server fan-out) |
| `game_data_sent` | — | The `--relay-payload` GameData was sent |
| `game_data_received` | `from`, `payload` | A validated, application-current relayed GameData payload arrived; lifecycle-overtaken stale tails are accounted but suppressed |
| `fallback_engaged` | — | The P2P window resolved with ZERO connected pairs; the relay floor carries the session |
| `success_criteria_met` | — | Harness-only barrier emitted when criteria hold and `--success-release-file` was supplied; the client remains connected until the path exists |
| `error` | `message` | Non-fatal or fatal error (fatal ones are followed by `exiting`) |
| `exiting` | `code` | Final event before process exit |

## Exit codes

| Code | Meaning |
|------|---------|
| `0` | All flag-driven success criteria met within the run window |
| `1` | `--run-for-secs` elapsed with unmet criteria (the `error` event lists them) |
| `2` | Protocol error (auth/join rejection, malformed frame). Exit 2 is **also** clap's default exit code for CLI-usage errors (unknown/missing flags); on that path the usage message goes to stderr and NO `exiting` event is emitted (the event stream never starts) |
| `3` | Transport failure (connect failed, socket died mid-session) |
| `4` | `--max-runtime-secs` watchdog fired (hard abort) |

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
  implement it; matchbox/browser interoperability must not assume that.
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
| Browser ↔ browser mesh (Chromium↔Chromium pair) | n/a | ✅ `browser_pair_mesh_n3` |
| mDNS `.local` obfuscation trap | n/a | ✅ `mesh_n3_browser_mdns_obfuscation` |
| Mid-handshake close → one `error` + prompt exit 3 | n/a | ✅ `browser_cli_mid_handshake_close_single_error_exit_3` |
| SIGTERM/SIGKILL Chromium teardown (orphan reaper) | n/a | ✅ `browser_cli_signal_teardown_reaps_chromium` |
| TURN-only pair + mismatched-secret fallback controls | ✅ `turn_only_pair_selects_relay_candidates_and_keeps_websocket_floor_live`; `mismatched_turn_secret_fails_p2p_and_uses_websocket_fallback` | — |

The browser cells live in [`tests/browser_interop_e2e.rs`](tests/browser_interop_e2e.rs) behind the
`browser-interop` cargo feature (this crate's default suite never compiles them; they additionally need
`SIGNAL_FISH_BROWSER_CLI` pointing at the built [browser client](../browser/README.md) bundle).

CI runs the native suite via [`scripts/run-webrtc-interop.sh`](../../scripts/run-webrtc-interop.sh) in
`.github/workflows/webrtc-interop.yml`, and the browser cells via
[`scripts/run-browser-interop.sh`](../../scripts/run-browser-interop.sh) in
`.github/workflows/browser-interop.yml`. The isolated TURN-only controls run via
[`scripts/run-turn-interop.sh`](../../scripts/run-turn-interop.sh) in
`.github/workflows/turn-interop.yml`; they start a digest-pinned local coturn
container and never depend on a public STUN/TURN service.

## Troubleshooting

- **`SIGNAL_FISH_SERVER_BIN is not set` panic:** the interop tests drive the real server binary. Run
  `cargo build --bin signal-fish-server` at the repository root and export
  `SIGNAL_FISH_SERVER_BIN=<repo>/target/debug/signal-fish-server`, or just use
  `bash scripts/run-webrtc-interop.sh`, which does both.
- **`SIGNAL_FISH_SERVER_BIN points at ... not a file`:** the path is stale (e.g. a `--release` binary was
  expected but only the debug profile was built). Rebuild, or re-run the script with the matching profile.
- **First build is slow:** the webrtc dependency tree is large; budget a few minutes cold. The crate mirrors the
  root dev-profile trick (`debug = 1` for the leaf, `debug = 0` for dependencies) to keep rebuilds fast.
- **Unit tests pass but interop hangs locally:** every client process self-bounds (`--max-runtime-secs`, exit 4)
  and every harness await carries a deadline that panics with the child's recent events and stderr — check that
  diagnostic output first; it names the event being awaited.
