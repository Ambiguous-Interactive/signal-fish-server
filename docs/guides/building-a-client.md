# Building a Client

This is the client-author contract for Signal Fish Server: the message flow you
**must** implement, the parts that are optional, and the rules you must not
re-derive yourself. It is protocol-level and language-agnostic. For a concrete
Rust walkthrough see the [Rust Client Guide](rust-client.md); for engine
integration see the [Platform Integration Guide](platform-integration.md).

The protocol is JSON over WebSocket. Every message is a `type`-discriminated
envelope:

```json
{ "type": "JoinRoom", "data": { "game_name": "my_game", "player_name": "Alice" } }
```

A message with no payload (such as `PlayerReady`, `Ping`, `StartGame`, `Pong`,
`RoomLeft`) is sent as `{ "type": "PlayerReady" }` with **no** `data` field.

The full, codegen-ready description of every message lives in the machine-readable
spec — see [Generating client code](#generating-client-code-from-the-spec) at the
end of this guide.

## The two protocol levels

Signal Fish has a mandatory floor and an optional upgrade. You can ship a fully
working client implementing only the floor.

- **v2 — the relay floor (mandatory).** The server relays every `GameData`
  message reliably through the WebSocket to the other players in the room. No
  peer-to-peer, no WebRTC, no capability negotiation. Every client MUST implement
  this and it always works.
- **v3 — capability negotiation + classified delivery (optional).** A client may
  advertise transports/topologies for a peer-to-peer plan and may classify JSON
  relay data as reliable, keyed-latest, or volatile. Exact `DeliveryReport`
  ranges account for every intentional sequence omission. P2P status never
  disables the relay path; the physical WebSocket can still close loudly when
  its delivery contract fails. Raw binary game data remains reliable.

See [Protocol Versions](../concepts/protocol-versions.md) for how the negotiated
version is capped downward and rejected when it cannot meet the server floor.

## Mandatory v2 relay-floor flow

This is the lifecycle every client must implement. Connect a WebSocket to the
server's `/v2/ws` (or `/v3/ws`) endpoint, then:

```text
[Authenticate when needed]  ->  JoinRoom  ->  PlayerReady  ->  StartGame  ->  GameStarting  ->  GameData ... ->  LeaveRoom
```

1. **Authenticate when needed** — in allowlist mode, this MUST be the first
   message and its public `app_id` must be allowed. In open mode it is optional;
   you may send `JoinRoom` first. If you do send `Authenticate` in open mode, it
   must still be your first message. Use it to negotiate v3 capabilities or a
   non-default game-data format. A successful handshake returns `Authenticated`
   followed by `ProtocolInfo`; a rejected one returns `AuthenticationError`.
2. **JoinRoom** — create or join a room. Omit `room_code` to create a new room
   (the server generates one); provide a `room_code` to join an existing room, or
   to create one with that specific code if none exists yet. The server replies
   `RoomJoined` (with your `player_id`, the current players, and the lobby state).
   On failure you get `RoomJoinFailed`. While in the room you receive
   `PlayerJoined` / `PlayerLeft` as others come and go.
3. **PlayerReady** — toggle your readiness. The server broadcasts
   `LobbyStateChanged` after each toggle, with `all_ready: true` once every
   _current_ player is ready. A player who joins later is always unready and
   triggers **no** corrective broadcast, so a cached `all_ready: true` is
   stale after a `PlayerJoined` — recompute it (every current player present
   in `ready_players`) whenever membership changes. Readiness alone does
   **not** start the game.
4. **StartGame** — explicitly finalize and start. See
   [StartGame authorization](#startgame-authorization-and-readiness) below — this
   is the most common source of client bugs.
5. **GameStarting** — the server broadcasts this once the lobby finalizes. It
   carries legacy `peer_connections` metadata. For a relay-only room this is your
   signal that gameplay traffic may begin.
6. **GameData** — send `{ "type": "GameData", "data": { "data": <anything> } }`.
   The server relays it to the other players, who receive
   `{ "type": "GameData", "data": { "from_player": <uuid>, "data": <anything> } }`.
   The inner `data` is opaque to the server.
7. **LeaveRoom** — leave cleanly; the server replies `RoomLeft` and tells the
   others `PlayerLeft`.

Throughout the session, send `Ping` periodically to keep the connection alive
(the server replies `Pong`); an idle connection is closed with
`CONNECTION_IDLE_TIMEOUT`.

See the worked walkthrough in
[v2 two-player relay](../scenarios/v2-two-player-relay.md).

### StartGame authorization and readiness

`StartGame` is accepted only when **every current player is ready** —
recomputed from the room's current membership, not from a cached flag
(`all_ready` snapshots are advisory; see below). `max_players` is a ceiling,
not a required count — a room with a single ready player may start (solo is
allowed). The server rejects a premature start with the error code
`GAME_START_NOT_READY`.

Authorization:

- If the room has a **designated authority** player, **only that authority** may
  send `StartGame`. Anyone else gets `GAME_START_FORBIDDEN`.
- If **no authority** is set, **any** player in the room may start.

So: track `all_ready` from `LobbyStateChanged`, track whether you are the
authority (from `RoomJoined.is_authority` / `AuthorityChanged`), and only enable
your "Start" affordance when both conditions allow it.

Treat `all_ready` as advisory, never as a guarantee: the server re-checks
readiness when `StartGame` arrives, and a member that joined after the last
`LobbyStateChanged` (always unready, no corrective broadcast) makes the room
not-all-ready. On `GAME_START_NOT_READY`, recompute readiness from the current
membership and re-issue `StartGame` once every current player is ready again —
via a `LobbyStateChanged { all_ready: true }` toggle or because the unready
member left (no broadcast fires). A one-shot latch leaves the lobby stalled.

## Mandatory vs optional

| Message / feature | Status |
| --- | --- |
| `JoinRoom`, `PlayerReady`, `StartGame`, `GameStarting`, `GameData`, `LeaveRoom` | **Mandatory** (v2 floor) |
| `Authenticate` | **Mandatory in allowlist mode**; optional in open mode, but required to advertise v3 capabilities or select a non-default game-data format |
| `Ping` / `Pong` heartbeat | **Mandatory** (avoid idle timeout) |
| Handling `Error` and the `*Failed` messages with `error_code` | **Mandatory** |
| `Reconnect` after a drop | Optional (see [Reconnection](../concepts/reconnection.md)) |
| `JoinAsSpectator` / `LeaveSpectator` | Optional (see [Spectator Mode](../concepts/spectator-mode.md)) |
| `AuthorityRequest` | Optional (see [Authority System](../concepts/authority.md)) |
| `ProvideConnectionInfo` | Optional self-declared peer metadata; a usable Direct endpoint is required before `host + direct` can be selected |
| `Signal`, `SessionPlan`, `NewPeer`, `TransportStatus`, `PeerTransportStatus` | Optional (**v3 only**) |
| `DeliveryReport` | **Mandatory for negotiated v3**; peers may send `latest` / `volatile` even when you do not |
| `RelayStats`, `GoingAway` | Optional v3 diagnostics / shutdown handling |

## Optional v3 upgrade

To advertise peer-to-peer capabilities, send `Authenticate` as the first frame,
request v3, and list only the transports and topologies you actually implement:

```json
{
  "type": "Authenticate",
  "data": {
    "app_id": "mb_app_abc123",
    "protocol_version": 3,
    "supported_transports": ["relay", "direct", "webrtc"],
    "supported_topologies": ["relay", "host", "mesh"]
  }
}
```

Omitting `supported_transports` / `supported_topologies` keeps you relay-only
even on the `/v3/ws` endpoint. In open mode, a client may skip `Authenticate`:
`/v3/ws` then uses v3 classified relay without peer-to-peer capabilities, while
`/v2/ws` uses v2. When you do authenticate, the server echoes the negotiated
`protocol_version`, accepted range, and current server message `transports`
(`["websocket"]` today) in `ProtocolInfo`.

After `GameStarting`, every v3 member receives a per-recipient
**`SessionPlan`** describing its opaque `generation`, chosen `topology`, `transport`, optional
`host`, optional validated `direct_endpoint`, `peers` with per-peer `initiate`
flags, `ice_servers`, and universal `fallback`. Relay-resolved rooms send
`relay`/`relay` with an empty peer list, which is an authoritative instruction
to clear stale P2P state. Advertise `direct` only when the client implements a
Direct socket and, if it may host, send `ProvideConnectionInfo` before
finalization. A Direct endpoint is self-declared rather than a reachability
proof, so failure still transitions to the relay floor.

The signaling rules you must follow:

- **Latest `SessionPlan` wins.** A new `SessionPlan` supersedes the previous one
  (e.g. on host failover, join, or reconnect). When `generation` changes,
  rebuild retained WebRTC pairs with its latest ICE credentials and discard
  signals from every other generation; also remove peers no longer listed.
- **The glare rule is server-driven.** Each `SessionPlan` peer's `initiate`
  tells you whether _you_ send the WebRTC offer.
  Exactly one side of every pair is the offerer. **Do not recompute this
  yourself** from UUID ordering or topology — just obey the flag.
- **Relay `Signal` verbatim.** Copy the current plan's generation when sending
  `{ "type": "Signal", "data": { "to": <peer>, "generation": <generation>,
  "signal": <opaque> } }`; you receive the peer's signal as
  `{ "type": "Signal", "data": { "from": <peer>, "generation": <generation>, "signal": <opaque> } }`. The
  server never inspects `signal`; by convention it is matchbox-compatible
  (`{"Offer":..}` / `{"Answer":..}` / `{"IceCandidate":..}`).
- **Report transport state (optional).** Send `TransportStatus` when your WebRTC
  path comes up or dies; peers are told via `PeerTransportStatus`. This is purely
  informational — the relay floor stays open regardless.

### Classified relay delivery and exact gaps

Only JSON `GameData` on a negotiated-v3 connection can be classified:

```json
{ "type": "GameData", "data": { "data": { "x": 12 }, "class": "latest", "key": 7 } }
```

- Omit `class` (or send `reliable`) with no key for commands and critical
  events. Reliable delivery waits for queue capacity and closes a slow
  recipient loudly rather than omitting a message.
- Send `latest` with a required `u32` key for replaceable state. Reuse a stable
  key for each independent state stream; values with other keys do not
  supersede one another.
- Send `volatile` with no key for opportunistic data. It never backpressures.
- Never attach class/key metadata to a raw binary frame; binary is reliable.

Received v3 `GameData` includes `epoch` and `seq`, and echoes `class`/`key` only
when supplied by the sender. Within one sender epoch, delivered sequences are
strictly increasing but can skip. Before accepting a skip on a continuing
connection, your receive loop must already have consumed enough exact
`DeliveryReport` ranges that their non-overlapping union covers every missing
sequence for that sender and epoch. Reports carry at most 256 ranges and may
roll over into multiple priority frames. Aggregate `RelayStats`, per-class
counter deltas, and a later report are not proof.

Treat baselines separately from gaps. For every v3 `PlayerInfo`, require paired
`epoch` and `seq`; the pair is the exact recipient-visible baseline and the next
delivery or exact gap begins at `seq + 1`. Peer lifecycle control has priority and can overtake
already-queued old-epoch data. Keep accounting for that trailing data, but do
not apply it after `PlayerLeft` or a newer incarnation announcement. Accept a
future epoch only if `PlayerJoined` or `PlayerReconnected` announced that exact
epoch; once data advances, reject older epochs. Your own room/spectator
transition is a generation barrier and resets room-scoped cursors while keeping
physical-connection counters. After your own reconnect, replace every
expectation with `Reconnected.sender_watermarks`, reset connection-scoped report
counters, and resynchronize application state. If an unexplained same-epoch
hole appears, stop applying dependent deltas and surface a protocol error.

Worked v3 sessions: [mesh + WebRTC](../scenarios/v3-mesh-webrtc.md),
[host topology](../scenarios/v3-host-topology.md),
[host failover](../scenarios/v3-host-failover.md).

## Common pitfalls

- **Do not route from legacy relay metadata.** Omit `JoinRoom.relay_transport`:
  all accepted values are ignored and use the same authenticated WebSocket
  relay floor. Treat every `relay_type` as an informational deployment label,
  and treat `ConnectionInfo.relay` as unvalidated peer input. Use
  `SessionPlan` for v3 routing and validate any external endpoint, credential,
  and source identity in the consuming integration.
- **Don't recompute glare.** The single most common P2P bug is deriving the
  offerer from UUID order or topology. The server already did it; obey
  `you_initiate` / `initiate`.
- **Always handle relay fallback.** WebRTC may never connect (NAT, firewalls).
  The `fallback` is always `relay`, and the server never disables it because of
  P2P state. A correct v3 client keeps working over relay when P2P fails -- never
  block gameplay on a WebRTC connection succeeding. Delivery failures can
  still close the physical WebSocket loudly.
- **`StartGame` is explicit and authorized.** Readiness does not auto-start.
  Gate the action on `all_ready` and on authority (see
  [StartGame authorization](#startgame-authorization-and-readiness)).
- **Treat `signal` as opaque.** Forward it verbatim; do not parse or rewrite it.
- **Honor the negotiated version.** If `ProtocolInfo.protocol_version` comes back
  as 2, do not send v3 messages (`Signal`, `TransportStatus`) or classified
  `GameData`. Well-typed but illegal class/key pairings return
  `INVALID_DELIVERY_CLASS`; malformed metadata, including explicit `null`,
  returns `INVALID_INPUT`. Unnegotiated WebRTC signaling returns
  `UNSUPPORTED_TRANSPORT`.
- **Do not infer gaps from totals.** Only the union of causally prior exact
  `DeliveryReport` ranges authorizes a continuing-connection sequence hole. Process priority
  control before later data, retain old-epoch accounting while suppressing
  stale application payloads, and reset baselines after reconnect.
- **Treat `4002 slow_consumer` as authoritative.** A final `SLOW_CONSUMER` error
  is best effort. Queue timeout, maximum sojourn, or failure to preserve exact
  accountability can all produce the close.
- **Keep the connection alive.** Send `Ping` on an interval or you will be
  dropped with `CONNECTION_IDLE_TIMEOUT`.
- **Liveness is bidirectional.** Receiving room traffic does not prove that
  your writes or automatic RFC 6455 Pong replies reach the server; sending
  application `Ping` does not prove that the server-to-client path drains.
  Keep reading and writing the socket, let the WebSocket stack answer protocol
  Pings, and treat close `4002 slow_consumer` or `4003 activity_timeout` as a
  terminal physical connection that must be reconnected. See the
  [directional partition table](../architecture/scaling.md#directional-partition-detection).
- **Choose the handshake path before sending anything.** In allowlist mode,
  `Authenticate` must be first. In open mode you may omit it and start with
  `JoinRoom`; if you use it for app identification or protocol negotiation, it
  must still precede every application message.
- **Rejoining by code re-creates a vanished room — carry your original
  `max_players`.** If a room no longer exists (the server restarted, or every
  member left and its reconnection window elapsed) and your party rejoins by room
  code, the _first_ rejoiner **re-creates** the room. Omit `max_players` and it
  falls back to the server's per-room default (`8`), so a larger party strands
  its overflow with `ROOM_FULL`. Always re-supply the original `max_players` on a
  rejoin-by-code so the whole party fits.
- **`Reconnect` is windowed and single-winner.** The reconnection `auth_token` is
  only claimable within the reconnection window (default 300 s); after it,
  `Reconnect` fails with `RECONNECTION_EXPIRED` / `RECONNECTION_TOKEN_INVALID`,
  so fall back to a fresh `JoinRoom`. Exactly one of two concurrent claims on the
  same token wins. If a reconnect races the old connection's teardown you may get
  `PLAYER_ALREADY_CONNECTED` — the token is **not** consumed, so wait briefly and
  retry. See [Reconnection](../concepts/reconnection.md).

## Testing checklist

A client is conformant when it passes these scenarios:

- [ ] **Handshake policy + join + relay:** in allowlist mode, authenticate first;
      in open mode, exercise the path your client supports (optional first
      `Authenticate` or immediate `JoinRoom`). Create a room, send and receive a
      `GameData` round-trip, then `LeaveRoom`.
- [ ] **Two-player lobby + start:** two clients join, both `PlayerReady`, observe
      `all_ready`, the authorized client sends `StartGame`, both see
      `GameStarting`.
- [ ] **StartGame rejection:** sending `StartGame` before `all_ready` returns
      `GAME_START_NOT_READY`; a non-authority sending it in an authority room
      returns `GAME_START_FORBIDDEN`.
- [ ] **`all_ready` invalidation:** reach `all_ready: true`, have another player
      join (a `PlayerJoined`, no corrective `LobbyStateChanged`), observe a
      `StartGame` attempt return `GAME_START_NOT_READY`, then confirm the
      client re-issues once the joiner readies and `all_ready` reports `true`
      again.
- [ ] **Error handling:** join a full room (`ROOM_FULL`), use a bad room code
      (`ROOM_NOT_FOUND`), and surface `error_code` values to the user. See
      [error handling](../scenarios/error-handling.md) and the
      [error code reference](../reference/error-codes.md).
- [ ] **Heartbeat:** the connection survives an idle period because `Ping` is
      sent.
- [ ] **Directional liveness:** independently block client → server and server
      → client traffic; confirm the unaffected direction can still carry data,
      then surface `4003 activity_timeout` or `4002 slow_consumer` and reconnect
      instead of treating one-way progress as a healthy connection.
- [ ] **Reconnect (if implemented):** drop and `Reconnect` with the join
      `auth_token`, replay control-only `missed_events`, resync gameplay state
      at the application layer, and for v3 apply `sender_watermarks` as
      per-sender `(epoch, seq)` baselines while resetting report counters.
- [ ] **v3 negotiation (if implemented):** advertise WebRTC, receive a
      `SessionPlan`, complete the offer/answer/ICE exchange following
      `you_initiate`, and **fall back to relay** when WebRTC fails.
- [ ] **v3 dynamics (if implemented):** apply superseding `SessionPlan`s for
      host failover and finalized membership changes, including the empty-peer
      relay reset.
- [ ] **v3 delivery classes (if implemented):** validate every class/key
      combination, keep binary reliable, and verify keyed-latest and volatile
      omissions are covered by the union of prior exact `DeliveryReport` ranges,
      including rollover beyond 256 ranges.
- [ ] **v3 gap lifecycle (if implemented):** distinguish an initial sender
      baseline, an epoch reset, a recipient reconnect watermark, and an
      authorized same-epoch gap; account but suppress lifecycle-overtaken stale
      payloads, require future-epoch announcements, and reject an unexplained
      hole or backward epoch.

## Generating client code from the spec

The protocol is described in a machine-readable AsyncAPI 3.0 document:
[`spec/signal-fish-protocol.asyncapi.yaml`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/spec/signal-fish-protocol.asyncapi.yaml).
It models every `ClientMessage` (send) and `ServerMessage` (receive) variant as a
message with a JSON-Schema `payload`, including the `type` discriminator, every
field's name/type/optionality, and the enum tokens for transports, topologies,
and error codes. A Rust test
([`tests/protocol_spec_consistency.rs`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/tests/protocol_spec_consistency.rs))
fails the build if it drifts from the Rust source, so you can trust it.

Accountability-bearing server messages use closed wire-shape unions. Generated
models therefore distinguish the v2 forms from v3 snapshots, sequence/epoch
pairs, terminal watermarks, and reconnect replay state instead of accepting a
single bag of optional fields. `SpectatorJoined` also has a shared branch for an
empty `current_players` snapshot: in a spectator-only room that payload has no
wire-visible version field and is valid for either negotiated version.
`Reconnected.missed_events` is limited to the exact replayable control-message
subset and uses the matching v2 or v3 lifecycle shapes.

Generate models or a client scaffold with the AsyncAPI generator:

```bash
npx @asyncapi/generator spec/signal-fish-protocol.asyncapi.yaml \
  @asyncapi/typescript-template -o ./generated
```

Or feed the self-contained `components/schemas` subtree to any JSON-Schema model
generator (quicktype, `json-schema-to-typescript`, schemafy, …). The spec's
header comment lists concrete invocations. The canonical wire examples to test
your generated types against live in
[`.llm/code-samples/protocol/`](https://github.com/Ambiguous-Interactive/signal-fish-server/tree/main/.llm/code-samples/protocol)
and the full prose reference is [Protocol Reference](../protocol.md).
