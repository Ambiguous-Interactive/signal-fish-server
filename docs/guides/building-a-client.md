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
  message through the WebSocket to the other players in the room. No
  peer-to-peer, no WebRTC, no capability negotiation. Every client MUST implement
  this and it always works.
- **v3 — capability negotiation + WebRTC (optional).** A client may additionally
  advertise the transports and topologies it supports; if the room negotiates a
  non-relay plan, the server hands each peer a `SessionPlan` and brokers WebRTC
  signaling. The relay floor never closes — v3 is strictly additive.

See [Protocol Versions](../concepts/protocol-versions.md) for how the negotiated
version is chosen and clamped.

## Mandatory v2 relay-floor flow

This is the lifecycle every client must implement. Connect a WebSocket to the
server's `/v2/ws` (or `/v3/ws`) endpoint, then:

```text
Authenticate  ->  JoinRoom  ->  PlayerReady  ->  StartGame  ->  GameStarting  ->  GameData ... ->  LeaveRoom
```

1. **Authenticate** — MUST be the first message. Send your public `app_id` (it is
   an identifier, not a secret — safe to embed in builds). The server replies
   `Authenticated` followed by `ProtocolInfo`. On failure you get
   `AuthenticationError` with an `error_code`.
2. **JoinRoom** — create or join a room. Omit `room_code` to create a new room
   (the server generates one); provide a `room_code` to join an existing room, or
   to create one with that specific code if none exists yet. The server replies
   `RoomJoined` (with your `player_id`, the current players, and the lobby state).
   On failure you get `RoomJoinFailed`. While in the room you receive
   `PlayerJoined` / `PlayerLeft` as others come and go.
3. **PlayerReady** — toggle your readiness. The server broadcasts
   `LobbyStateChanged` after each toggle, with `all_ready: true` once every
   _current_ player is ready. Readiness alone does **not** start the game.
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

`StartGame` is accepted only when **every current player is ready** (`all_ready`
was `true` in the most recent `LobbyStateChanged`). `max_players` is a ceiling,
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

## Mandatory vs optional

| Message / feature | Status |
| --- | --- |
| `Authenticate`, `JoinRoom`, `PlayerReady`, `StartGame`, `GameStarting`, `GameData`, `LeaveRoom` | **Mandatory** (v2 floor) |
| `Ping` / `Pong` heartbeat | **Mandatory** (avoid idle timeout) |
| Handling `Error` and the `*Failed` messages with `error_code` | **Mandatory** |
| `Reconnect` after a drop | Optional (see [Reconnection](../concepts/reconnection.md)) |
| `JoinAsSpectator` / `LeaveSpectator` | Optional (see [Spectator Mode](../concepts/spectator-mode.md)) |
| `AuthorityRequest` | Optional (see [Authority System](../concepts/authority.md)) |
| `ProvideConnectionInfo` | Optional legacy peer metadata |
| `Signal`, `SessionPlan`, `NewPeer`, `TransportStatus`, `PeerTransportStatus` | Optional (**v3 only**) |

## Optional v3 upgrade

If you want peer-to-peer (WebRTC mesh or host topology), opt in during
`Authenticate` by advertising your capabilities:

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
even on the `/v3/ws` endpoint. The server echoes the negotiated
`protocol_version`, accepted range, and current server message `transports`
(`["websocket"]` today) in `ProtocolInfo`.

When a v3 room negotiates a non-relay plan, in addition to `GameStarting` you
receive a per-recipient **`SessionPlan`** describing the chosen `topology`
(`host` or `mesh`), `transport` (`webrtc` / `direct`), the `host` (for `host`
topology), your `peers` list with per-peer `initiate` flags, the `ice_servers`
to gather against, and the universal `fallback` (always `relay`).

The signaling rules you must follow:

- **Latest `SessionPlan` wins.** A new `SessionPlan` supersedes the previous one
  (e.g. on host failover or a late join). Apply the most recent one — including
  its `ice_servers`, since TURN credentials rotate.
- **On `NewPeer`, additively connect.** A `NewPeer` message means a new peer
  joined an active session; connect to it _in addition_ to your existing peers.
  Do not tear down existing connections.
- **The glare rule is server-driven.** Each `NewPeer.you_initiate` (and each
  `SessionPlan` peer's `initiate`) tells you whether _you_ send the WebRTC offer.
  Exactly one side of every pair is the offerer. **Do not recompute this
  yourself** from UUID ordering or topology — just obey the flag.
- **Relay `Signal` verbatim.** Send `{ "type": "Signal", "data": { "to": <peer>,
  "signal": <opaque> } }`; you receive the peer's signal as
  `{ "type": "Signal", "data": { "from": <peer>, "signal": <opaque> } }`. The
  server never inspects `signal`; by convention it is matchbox-compatible
  (`{"Offer":..}` / `{"Answer":..}` / `{"IceCandidate":..}`).
- **Report transport state (optional).** Send `TransportStatus` when your WebRTC
  path comes up or dies; peers are told via `PeerTransportStatus`. This is purely
  informational — the relay floor stays open regardless.

Worked v3 sessions: [mesh + WebRTC](../scenarios/v3-mesh-webrtc.md),
[host topology](../scenarios/v3-host-topology.md),
[host failover](../scenarios/v3-host-failover.md).

## Common pitfalls

- **Don't recompute glare.** The single most common P2P bug is deriving the
  offerer from UUID order or topology. The server already did it; obey
  `you_initiate` / `initiate`.
- **Always handle relay fallback.** WebRTC may never connect (NAT, firewalls).
  The `fallback` is always `relay` and the relay floor never closes. A correct
  v3 client keeps working over relay when P2P fails — never block gameplay on a
  WebRTC connection succeeding.
- **`StartGame` is explicit and authorized.** Readiness does not auto-start.
  Gate the action on `all_ready` and on authority (see
  [StartGame authorization](#startgame-authorization-and-readiness)).
- **Treat `signal` as opaque.** Forward it verbatim; do not parse or rewrite it.
- **Honor the negotiated version.** If `ProtocolInfo.protocol_version` comes back
  as 2, do not send v3 messages (`Signal`, `TransportStatus`); they require the
  WebRTC transport to have been negotiated and are rejected otherwise
  (`UNSUPPORTED_TRANSPORT`).
- **Keep the connection alive.** Send `Ping` on an interval or you will be
  dropped with `CONNECTION_IDLE_TIMEOUT`.
- **Authenticate first.** Any other message before `Authenticate` is an error.
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

- [ ] **Auth + join + relay:** authenticate, create a room, send and receive a
      `GameData` round-trip, then `LeaveRoom`.
- [ ] **Two-player lobby + start:** two clients join, both `PlayerReady`, observe
      `all_ready`, the authorized client sends `StartGame`, both see
      `GameStarting`.
- [ ] **StartGame rejection:** sending `StartGame` before `all_ready` returns
      `GAME_START_NOT_READY`; a non-authority sending it in an authority room
      returns `GAME_START_FORBIDDEN`.
- [ ] **Error handling:** join a full room (`ROOM_FULL`), use a bad room code
      (`ROOM_NOT_FOUND`), and surface `error_code` values to the user. See
      [error handling](../scenarios/error-handling.md) and the
      [error code reference](../reference/error-codes.md).
- [ ] **Heartbeat:** the connection survives an idle period because `Ping` is
      sent.
- [ ] **Reconnect (if implemented):** drop and `Reconnect` with the join
      `auth_token`, then replay `missed_events`.
- [ ] **v3 negotiation (if implemented):** advertise WebRTC, receive a
      `SessionPlan`, complete the offer/answer/ICE exchange following
      `you_initiate`, and **fall back to relay** when WebRTC fails.
- [ ] **v3 dynamics (if implemented):** apply a superseding `SessionPlan` (host
      failover) and additively connect on `NewPeer`.

## Generating client code from the spec

The protocol is described in a machine-readable AsyncAPI 3.0 document:
[`spec/signal-fish-protocol.asyncapi.yaml`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/spec/signal-fish-protocol.asyncapi.yaml).
It models every `ClientMessage` (send) and `ServerMessage` (receive) variant as a
message with a JSON-Schema `payload`, including the `type` discriminator, every
field's name/type/optionality, and the enum tokens for transports, topologies,
and error codes. A Rust test
([`tests/protocol_spec_consistency.rs`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/tests/protocol_spec_consistency.rs))
fails the build if it drifts from the Rust source, so you can trust it.

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
