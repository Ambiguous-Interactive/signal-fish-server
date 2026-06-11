# signal-fish-reference-native

A **native Rust reference client** for the Signal Fish protocol v3 with a **real WebRTC stack**
([webrtc-rs](https://github.com/webrtc-rs/webrtc) 0.17: actual ICE gathering, DTLS handshakes, and SCTP data
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
| `--leave-on-game-start` | off | Exit 0 as soon as `GameStarting` arrives WITHOUT ever pairing (a received `SessionPlan` is logged, not acted on). Used to vacate a seat in a finalized room — the server only finalizes full rooms, so late joins are seat fills |
| `--game-name <NAME>` | `reference-native` | Game name for room create/join |
| `--player-name <NAME>` | `RefNative` | Display name |
| `--app-id <ID>` | `reference-native-app` | `Authenticate.app_id` (interop servers run with WebSocket auth disabled) |
| `--platform <P>` | `reference-native` | `Authenticate.platform` |
| `--exchange` | off | When a P2P pair fully opens, send exactly one text message per channel and require the symmetric receives (see success criteria) |
| `--relay-payload <TEXT>` | — | After `GameStarting` (+250 ms settle), send one `GameData {"relay_msg": TEXT}` over the WebSocket relay floor and require the other `--peers - 1` members' payloads. A late joiner (entry into a finalized room) arms the send on entry instead — `GameStarting` pre-dates the join — and its receive requirement is waived: payloads sent before the join are never replayed |
| `--cripple-ice` | off | Deterministically break ICE: reject every interface during gathering AND drop all outbound/inbound `IceCandidate` signals (SDP offer/answer still flows). Forces the relay fallback |
| `--p2p-timeout-secs <S>` | `15` | Window for WebRTC pair establishment before the overall transport status resolves |
| `--run-for-secs <S>` | `30` | Soft cap: exit 1 if the flag-driven success criteria are still unmet |
| `--max-runtime-secs <S>` | `60` | Hard watchdog: abort with exit 4 no matter what (the no-hang guarantee) |
| `--protocol-version <V>` | `3` | `2` omits every v3 `Authenticate` field — a pure v2 client for mixed-room tests |
| `--supported-topologies <LIST>` | `relay,host,mesh` | Comma-separated topologies advertised in v3 `Authenticate` |
| `--supported-transports <LIST>` | `relay,webrtc` | Comma-separated transports advertised in v3 `Authenticate` |

## JSONL event contract

One JSON object per stdout line, tagged by a snake_case `event` field. Per-client ordering is causal (a single
emitter task); events carry no timestamps. If the stdout consumer closes the pipe mid-run, the client never
panics on the resulting `EPIPE`: the failure is logged to stderr once, further events are suppressed, and the
process continues to its normal bounded exit.

| Event | Fields | Emitted when |
|-------|--------|--------------|
| `connected` | — | WebSocket connection established |
| `authenticated` | — | Server accepted `Authenticate` |
| `protocol_info` | `negotiated_version` | Negotiation result (v2 connections report `2`) |
| `room_created` | `room_code` | This client created the room (harnesses scrape the code) |
| `room_joined` | `room_id`, `player_id`, `lobby_state` | Seated in the room; `lobby_state` ∈ `waiting`/`lobby`/`finalized` — `finalized` marks a late join into a running session |
| `peer_joined` | `player_id` | Another player joined |
| `player_left` | `player_id` | Another player left |
| `game_starting` | `is_authority` | Lobby finalized (this client's own authority flag); never re-broadcast to late joiners |
| `session_plan` | `topology`, `transport`, `host`, `peers[{player_id, initiate}]`, `ice_servers_count`, `fallback` | The per-recipient v3 session directive |
| `new_peer` | `peer_id`, `you_initiate` | Late-join pairing delta (existing members only) |
| `signal_sent` | `to`, `kind` | Outbound `Signal` relayed (`kind` ∈ `offer`/`answer`/`ice_candidate`/`other`) |
| `signal_received` | `from`, `kind` | Inbound `Signal` arrived (emitted even when `--cripple-ice` then drops it) |
| `pc_state` | `peer`, `state` | RTCPeerConnection state transition (informational) |
| `channel_open` | `peer`, `label` | One data channel reached open (`label` ∈ `reliable`/`unreliable`) |
| `channel_message_sent` | `peer`, `label`, `text` | An `--exchange` message was sent |
| `channel_message` | `peer`, `label`, `text` | A data-channel text message arrived |
| `p2p_pair_connected` | `peer` | BOTH channels toward `peer` are open |
| `transport_status_sent` | `transport`, `connected` | The single overall `TransportStatus` report went out (Appendix G) |
| `peer_transport_status` | `peer`, `transport`, `connected` | A same-room peer's reported state changed (server fan-out) |
| `game_data_sent` | — | The `--relay-payload` GameData was sent |
| `game_data_received` | `from`, `payload` | A relayed GameData payload arrived over the WebSocket |
| `fallback_engaged` | — | The P2P window resolved with ZERO connected pairs; the relay floor carries the session |
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

## Transport-status semantics

- The overall WebRTC status resolves **exactly once** per run (Appendix G): early, when every currently expected
  pair is connected (a departure that removes the last unconnected expected pair counts), or at
  `--p2p-timeout-secs`. `connected: true` iff **at least one** pair is connected at resolution; a zero-pair
  resolution also emits `fallback_engaged`.
- Pairs that connect **after** resolution (late-join `NewPeer`) do not re-send the report: the state did not
  change, and the server deduplicates repeat `(transport, connected)` states anyway — re-sending would fan out
  nothing (see `PeerTransportStatus` in [`docs/protocol.md`](../../docs/protocol.md)).
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
| Mesh N=3 full WebRTC + live relay floor | ✅ `mesh_n3_full_webrtc_session_with_live_relay_floor` | ⏳ pending (browser client planned) |
| Host star N=3 | ✅ `host_star_n3_webrtc` | ⏳ pending |
| Crippled-ICE relay fallback | ✅ `mesh_n3_partial_ice_cripple_relay_fallback` | ⏳ pending |
| Late join (`NewPeer` seat fill) | ✅ `late_join_newpeer_real_webrtc_n3` | ⏳ pending |
| Mixed v2/v3 relay floor | ✅ `mixed_v2_v3_n3_relay_floor_with_reference_client` | ⏳ pending |

CI runs the suite via [`scripts/run-webrtc-interop.sh`](../../scripts/run-webrtc-interop.sh) in
`.github/workflows/webrtc-interop.yml`.

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
