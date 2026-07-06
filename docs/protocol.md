# Protocol Reference

Signal Fish Server uses a JSON-based WebSocket protocol. All messages are JSON objects with a `type` field and
optional `data` field.

MessagePack encoding is also supported for game data when `enable_message_pack_game_data` is enabled.

## Client Messages

### Authenticate

Authenticate with app credentials (required when auth is enabled). App ID is a public identifier that identifies
the game application.

```json
{
  "type": "Authenticate",
  "data": {
    "app_id": "my-game"
  }
}

```

Optional fields:

- `sdk_version` - SDK version for debugging and analytics
- `platform` - Platform information (e.g., "unity", "godot", "unreal")
- `game_data_format` - Preferred game data encoding (defaults to JSON text frames)

### JoinRoom

Join or create a room for a specific game. There is no separate room-creation
message. `JoinRoom` behavior depends on `room_code`:

1. Omit `room_code`: create a new room with a generated room code.
2. Provide `room_code` and room exists for that `game_name`: join that room.
3. Provide `room_code` and no room exists for that `game_name`: create a new
   room with that room code.

```json

{
  "type": "JoinRoom",
  "data": {
    "game_name": "my-game",
    "player_name": "Player1"
  }
}

```

Required fields:

- `game_name` - Name of the game
- `player_name` - Name for the player

Optional fields:

- `room_code` - Code used to join/create a room for this `game_name`
- `max_players` - Maximum players (applied only when a new room is created)
- `supports_authority` - Authority support (applied only when a new room is created)
- `relay_transport` - Preferred relay transport protocol (`tcp`, `udp`, `websocket`, or `auto`; default `auto`)

### GameData

Send arbitrary game data to other players in the room.

```json

{
  "type": "GameData",
  "data": {
    "data": {
      "action": "move",
      "x": 100,
      "y": 200
    }
  }
}

```

The outer `data` is the serde content tag. The inner `data` is the variant
field and can be any JSON-serializable object.

### PlayerReady

Toggle your own ready state in the lobby. This message has no payload.

Behavior:

1. First send in `lobby` state marks the player ready.
2. Sending again in `lobby` state marks the player unready.
3. Readiness may be toggled any time before the room is `finalized`; the room
   need not be full.
4. The server broadcasts `LobbyStateChanged` after each toggle, with
   `all_ready` set once every current player is ready.

Readiness **no longer** starts the game. When every current player is ready,
the game starts only after an explicit [`StartGame`](#startgame) message.

```json

{
  "type": "PlayerReady"
}

```

This message has no data payload.

If sent while not in a joinable lobby state, the server returns an `Error`
with `INVALID_ROOM_STATE`.

### StartGame

Explicitly start (finalize) the game with the room's **current** members. This
message has no payload. `max_players` is a ceiling, not a required count — a
room need not be full to start (a single ready player may start; solo is
allowed).

```json

{
  "type": "StartGame"
}

```

This message has no data payload.

Preconditions:

1. Every **current** player in the room must be ready (`all_ready`). Otherwise
   the server returns an `Error` with `GAME_START_NOT_READY`.
2. The sender must be permitted to start: if the room has a designated
   authority player, only that authority may start; if no authority is set,
   **any** member may start. An unauthorized sender receives an `Error` with
   `GAME_START_FORBIDDEN`.

On success the server transitions the room to `finalized` and broadcasts the
unchanged [`GameStarting`](#gamestarting) (legacy peer metadata) to every
member. For a negotiated v3 non-relay room it additionally emits the
per-recipient [`SessionPlan`](#sessionplan). Sending `StartGame` to an already
`finalized` room returns an `Error` with `INVALID_ROOM_STATE`.

!!! note "Authority and start liveness"
    In an authority room, only the authority may start, so the game does not
    begin until the authority sends `StartGame` — design your client so the
    authority's UI offers a "Start" action once `all_ready` is reported. If the
    authority **leaves** the room, the server clears the authority designation,
    after which any remaining member may start (the room is never locked into
    `GAME_START_FORBIDDEN` by an authority departure).

### AuthorityRequest

Request or release game authority.

```json

{
  "type": "AuthorityRequest",
  "data": {
    "become_authority": true
  }
}

```

### LeaveRoom

Leave the current room.

```json

{
  "type": "LeaveRoom"
}

```

### Ping

Heartbeat ping. Server responds with `Pong`.

```json

{
  "type": "Ping"
}

```

### Reconnect

Reconnect to a room after disconnection using authentication token.

```json

{
  "type": "Reconnect",
  "data": {
    "player_id": "player-id",
    "room_id": "room-id",
    "auth_token": "token-string"
  }
}

```

The `auth_token` is the reconnection token the server minted for this room:
v3+ clients receive it on the wire in `RoomJoined.reconnection_token` at join
time (rotated again in `Reconnected.reconnection_token` after every
successful reconnect). The token string is stable from join through the
disconnect, but it only becomes claimable for `server.reconnection_window`
seconds counted from the disconnect. For pure-v2 clients the token is still
minted at disconnect time and never reaches the wire — reconnection is
effectively a v3+ feature.

### ProvideConnectionInfo

Provide legacy, self-declared peer connection metadata for the v2
`GameStarting.peer_connections` handoff. This metadata is preserved for
backward compatibility and is not part of protocol v3 capability negotiation:
it does not prove that a client negotiated `direct` or `webrtc`, and it does not
drive v3 `SessionPlan`, `Signal`, `NewPeer`, `TransportStatus`,
`PeerTransportStatus`, or transport metrics.

```json

{
  "type": "ProvideConnectionInfo",
  "data": {
    "connection_info": {
      "type": "direct",
      "host": "192.168.1.10",
      "port": 7777
    }
  }
}

```

### JoinAsSpectator

Join a room as a spectator (read-only observer).

```json

{
  "type": "JoinAsSpectator",
  "data": {
    "game_name": "my-game",
    "room_code": "ABC123",
    "spectator_name": "Observer1"
  }
}

```

Required fields:

- `game_name` - Name of the game
- `room_code` - Code of the room to spectate
- `spectator_name` - Name for the spectator

### LeaveSpectator

Leave spectator mode.

```json

{
  "type": "LeaveSpectator"
}

```

This message has no data payload.

## Server Messages

### Authenticated

Authentication successful. Includes app information and rate limits.

```json

{
  "type": "Authenticated",
  "data": {
    "app_name": "my-game",
    "organization": "My Organization",
    "rate_limits": {
      "per_minute": 60,
      "per_hour": 3600,
      "per_day": 86400
    }
  }
}

```

Optional fields:

- `organization` - Organization name (if any)

### ProtocolInfo

SDK/protocol compatibility details advertised after authentication.

```json

{
  "type": "ProtocolInfo",
  "data": {
    "capabilities": ["reconnection", "spectators", "authority"],
    "game_data_formats": ["json", "message_pack"]
  }
}

```

### AuthenticationError

Authentication failed.

```json

{
  "type": "AuthenticationError",
  "data": {
    "error": "Invalid app_id",
    "error_code": "INVALID_APP_ID"
  }
}

```

### RoomJoined

Successfully joined or created a room. This message is sent both when creating
a new room and when joining an existing room. There is no separate
room-created response type.

```json

{
  "type": "RoomJoined",
  "data": {
    "room_id": "uuid-string",
    "room_code": "ABC123",
    "player_id": "your-player-id",
    "game_name": "my-game",
    "max_players": 8,
    "supports_authority": true,
    "current_players": [
      {
        "id": "player-id",
        "name": "Player 1",
        "is_authority": false,
        "is_ready": false,
        "connected_at": "2024-01-01T00:00:00Z"
      }
    ],
    "is_authority": false,
    "lobby_state": "waiting",
    "ready_players": [],
    "relay_type": "matchbox",
    "current_spectators": []
  }
}

```

### PlayerJoined

Another player joined the room.

```json

{
  "type": "PlayerJoined",
  "data": {
    "player": {
      "id": "player-id",
      "name": "Player 2",
      "is_authority": false,
      "is_ready": false,
      "connected_at": "2024-01-01T00:00:00Z"
    }
  }
}

```

### PlayerLeft

A player left the room.

```json

{
  "type": "PlayerLeft",
  "data": {
    "player_id": "player-id"
  }
}

```

### RoomJoinFailed

Failed to join room.

```json

{
  "type": "RoomJoinFailed",
  "data": {
    "reason": "Room is full",
    "error_code": "ROOM_FULL"
  }
}

```

Note: The `error_code` field is optional.

### RoomLeft

Successfully left room.

```json

{
  "type": "RoomLeft"
}

```

This message has no data payload.

### GameData

Game data relayed from another player.

```json

{
  "type": "GameData",
  "data": {
    "from_player": "player-id",
    "data": {
      "action": "move",
      "x": 100,
      "y": 200
    }
  }
}

```

### GameDataBinary

Binary game data payload from another player. This server message variant is an
internal broadcast carrier only; MessagePack-capable clients receive a WebSocket
binary frame containing a bare MessagePack map with `from_player`, `encoding`,
and `payload` fields. It is not wrapped in the JSON `{ "type": ..., "data": ... }`
envelope.

```text
MessagePack map:
  from_player: player-id
  encoding: message_pack
  payload: raw bytes
```

Clients that did not negotiate `game_data_format: "message_pack"` receive the
JSON `GameData` fallback instead.

### Delivery semantics

Relayed messages — `GameData`, `GameDataBinary`, and every other server
message — are delivered reliably and in order per connection over the
WebSocket. The server never silently drops a delivery: when a recipient
cannot keep up, its bounded outbound queue
(`websocket.send_queue_capacity`, default 1024 messages) fills and delivery
applies backpressure to senders, waiting up to
`websocket.slow_consumer_timeout_ms` (default 5000) for space. A recipient
whose queue stays full past that timeout is disconnected as a slow consumer:
the server sends a best-effort `Error` with code `SLOW_CONSUMER`, then
closes the socket through the normal disconnect flow (so the reconnection
grace period still applies).

Practical consequences:

- Clients driving async runtimes must continuously poll/drive their
  connection. A runtime that is merely "ticked" occasionally starves the
  transport: inbound frames back up on the server side and manifest as
  apparent stalls, ending in a `SLOW_CONSUMER` disconnect.
- Sustainable throughput is bounded by the slowest recipient's ability to
  drain its connection: room senders are paced to their slowest healthy
  recipient, and a dead recipient costs senders at most one timeout window
  before it is evicted. Operators can watch the
  `signal_fish_websocket_backpressure_events_total` and
  `signal_fish_websocket_slow_consumer_disconnects_total` Prometheus
  counters to spot backpressure and slow-consumer evictions in production.

A binary game-data payload that cannot be converted for a recipient (for
example, an internal binary encoding relayed to a JSON-only client) is not
silently dropped either: the recipient receives an explicit `Error` with
code `UNSUPPORTED_GAME_DATA_FORMAT` in place of each undeliverable payload.

### Close codes

Farewell `Error` frames are best-effort: on the congested socket a
slow-consumer eviction escapes, they frequently cannot be delivered at all.
The WebSocket close frame's code travels in the closing handshake itself, so
it is the one attribution signal a client can always read. The server closes
with RFC 6455 private-range codes (these assignments are stable protocol
surface and are never renumbered):

| Code | Reason string | Meaning |
| ---- | ------------- | ------- |
| `4000` | `server_shutdown` | The server is shutting down (reserved; no in-process trigger today) |
| `4001` | `auth_timeout` | Never authenticated within `websocket.auth_timeout_secs` |
| `4002` | `slow_consumer` | Evicted by the delivery contract (outbound queue full past `websocket.slow_consumer_timeout_ms`) |
| `4003` | `activity_timeout` | Evicted by the `server.ping_timeout` activity reaper |
| `4004` | `idle_timeout` | No inbound frame within `websocket.idle_timeout_secs` |
| `1000` | `unregistered` | Normal closure (leave, replaced connection, ordinary teardown) |

### LobbyStateChanged

Lobby state transitioned.

```json

{
  "type": "LobbyStateChanged",
  "data": {
    "lobby_state": "finalized",
    "ready_players": ["player-id-1", "player-id-2"],
    "all_ready": true
  }
}

```

Possible states:

- `waiting` - Waiting for the first player to join
- `lobby` - Players are present and coordinating readiness (the room need not be
  full; `max_players` is a ceiling)
- `finalized` - The game has started after an explicit `StartGame` (sent once
  every current player is ready)

### AuthorityChanged

Authority status changed in the room.

```json

{
  "type": "AuthorityChanged",
  "data": {
    "authority_player": "player-id",
    "you_are_authority": false
  }
}

```

The `authority_player` field can be `null` if no player currently has authority.

### AuthorityResponse

Authority request response.

```json

{
  "type": "AuthorityResponse",
  "data": {
    "granted": true,
    "reason": "Authority granted"
  }
}

```

Note: The `reason` and `error_code` fields are optional.

### GameStarting

Game is starting with legacy peer metadata.

```json

{
  "type": "GameStarting",
  "data": {
    "peer_connections": [
      {
        "player_id": "player-id-1",
        "player_name": "Player 1",
        "is_authority": false,
        "relay_type": "matchbox"
      }
    ]
  }
}

```

`peer_connections` carries player identity, authority, relay type, and optional
self-declared `connection_info` from `ProvideConnectionInfo`. It is kept for
v2/back-compat and does not prove direct or WebRTC reachability. v3 clients use
the negotiated `SessionPlan` for topology, transport, peers, ICE servers, and
relay fallback.

### Error

An error occurred.

```json

{
  "type": "Error",
  "data": {
    "message": "Room is full",
    "error_code": "ROOM_FULL"
  }
}

```

Note: The `error_code` field is optional.

Common error codes:

- `ROOM_FULL` - Room has reached max players
- `ROOM_NOT_FOUND` - Room code does not exist
- `INVALID_GAME_NAME` - Game name validation failed
- `RATE_LIMIT_EXCEEDED` - Too many requests
- `AUTHENTICATION_REQUIRED` - Authentication required
- `INVALID_APP_ID` - Invalid app ID

### Pong

Response to client `Ping`.

```json

{
  "type": "Pong"
}

```

### Reconnected

Reconnection successful. Includes current room state. The `missed_events`
field carries the replayable **control** events (membership / lobby / authority
transitions) broadcast to the room while the player was disconnected, oldest
first, from a bounded per-room replay ring (`server.event_buffer_size`); it is
NOT empty when such events occurred during the absence. High-rate data-path
traffic (`GameData` / `Signal`) is deliberately not replayed. The companion
`replay` field (v3+ recipients only) reports the completeness of that list —
`complete`, `truncated` (the ring evicted an event the player needed, so
`missed_events` is only a suffix), or `unavailable` (replay disabled,
`event_buffer_size = 0`). See [Reconnection Flow](#reconnection-flow).

```json

{
  "type": "Reconnected",
  "data": {
    "room_id": "uuid-string",
    "room_code": "ABC123",
    "player_id": "your-player-id",
    "game_name": "my-game",
    "max_players": 8,
    "supports_authority": true,
    "current_players": [
      {
        "id": "player-id",
        "name": "Player 1",
        "is_authority": false,
        "is_ready": false,
        "connected_at": "2024-01-01T00:00:00Z"
      }
    ],
    "is_authority": false,
    "lobby_state": "lobby",
    "ready_players": ["player-id-1"],
    "relay_type": "matchbox",
    "current_spectators": [],
    "missed_events": [],
    "replay": "complete"
  }
}

```

### ReconnectionFailed

Reconnection failed.

```json

{
  "type": "ReconnectionFailed",
  "data": {
    "reason": "Invalid reconnection token",
    "error_code": "RECONNECTION_TOKEN_INVALID"
  }
}

```

### PlayerReconnected

Another player reconnected to the room.

```json

{
  "type": "PlayerReconnected",
  "data": {
    "player_id": "player-id"
  }
}

```

### SpectatorJoined

Successfully joined a room as spectator.

```json

{
  "type": "SpectatorJoined",
  "data": {
    "room_id": "uuid-string",
    "room_code": "ABC123",
    "spectator_id": "your-spectator-id",
    "game_name": "my-game",
    "current_players": [
      {
        "id": "player-id",
        "name": "Player 1",
        "is_authority": false,
        "is_ready": false,
        "connected_at": "2024-01-01T00:00:00Z"
      }
    ],
    "current_spectators": [
      {
        "id": "spectator-id",
        "name": "Observer1",
        "connected_at": "2025-01-15T10:35:00Z"
      }
    ],
    "lobby_state": "lobby",
    "reason": "joined"
  }
}

```

Note: The `reason` field is optional.

### SpectatorJoinFailed

Failed to join as spectator.

```json

{
  "type": "SpectatorJoinFailed",
  "data": {
    "reason": "Room not found",
    "error_code": "ROOM_NOT_FOUND"
  }
}

```

Note: The `error_code` field is optional.

### SpectatorLeft

Successfully left spectator mode.

```json

{
  "type": "SpectatorLeft",
  "data": {
    "room_id": "uuid-string",
    "room_code": "ABC123",
    "reason": "voluntary_leave",
    "current_spectators": []
  }
}

```

Note: `room_id`, `room_code`, and `reason` are optional (omitted when absent).
`current_spectators` is always present (serialized as `[]` when empty).

### NewSpectatorJoined

Another spectator joined the room.

```json

{
  "type": "NewSpectatorJoined",
  "data": {
    "spectator": {
      "id": "spectator-id",
      "name": "Observer2",
      "connected_at": "2025-01-15T10:36:00Z"
    },
    "current_spectators": [
      {
        "id": "spectator-id-1",
        "name": "Observer1",
        "connected_at": "2025-01-15T10:35:00Z"
      },
      {
        "id": "spectator-id-2",
        "name": "Observer2",
        "connected_at": "2025-01-15T10:36:00Z"
      }
    ],
    "reason": "joined"
  }
}

```

Note: The `reason` field is optional.

### SpectatorDisconnected

Another spectator left the room.

```json

{
  "type": "SpectatorDisconnected",
  "data": {
    "spectator_id": "spectator-id",
    "reason": "disconnected",
    "current_spectators": []
  }
}

```

Note: The `reason` field is optional.

## Session Flow

```text

Client                              Server
  |                                    |
  |--- Authenticate ------------------>|
  |<-- Authenticated ------------------|
  |                                    |
  |--- JoinRoom (no room_code) ------->|
  |<-- RoomJoined ---------------------|
  |                                    |
  |         (other client joins)       |
  |<-- PlayerJoined -------------------|
  |                                    |
  |--- PlayerReady ------------------->|
  |<-- LobbyStateChanged (all_ready) --|
  |                                    |
  |--- StartGame --------------------->|
  |<-- GameStarting -------------------|
  |    (+ SessionPlan on v3 non-relay) |
  |                                    |
  |--- GameData ---------------------->|
  |<-- GameData (from other player) ---|
  |                                    |
  |--- LeaveRoom --------------------->|
  |<-- RoomLeft -----------------------|

```

## Reconnection Flow

When a client disconnects, the server generates a reconnection token bound to
the player's ID and room. The client uses this token along with the `player_id`
and `room_id` (from the original `RoomJoined` response) to reconnect:

```json

{
  "type": "Reconnect",
  "data": {
    "player_id": "your-player-id",
    "room_id": "your-room-id",
    "auth_token": "stored-token"
  }
}

```

On successful reconnection, the server sends a `Reconnected` message with the current room state.

Note: replayable **control** events (membership / lobby / authority
transitions) broadcast while the player was disconnected ARE buffered in a
bounded per-room replay ring and returned in the `Reconnected` payload's
`missed_events` list, with the `replay` field reporting completeness
(`complete` / `truncated` / `unavailable`). High-rate data-path traffic
(`GameData` / `Signal`) is **not** replayed, and a `truncated` or
`unavailable` replay means control history is incomplete — so clients must
still treat reconnection as requiring an application-level state resync (for
example, have the authority or another peer re-send the current game state
after `PlayerReconnected`).

## Protocol v3 additions

Protocol v3 is a **purely additive** layer on top of the v2 wire contract documented above. Everything in the
preceding sections still applies unchanged; v3 only adds optional `Authenticate` fields, five new message types,
a capability-negotiation handshake, and an optional `ice_servers` field on `RoomJoined` / `Reconnected` (the
[ICE pre-gather](#ice-pre-gather), emitted only to v3 WebRTC-capable clients). A v2 client never sends or receives
a v3 message — the relay floor is the universal default and a v2 client observes byte-identical v2 behavior.

Canonical wire samples for this section:

- [v3 client messages](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/.llm/code-samples/protocol/v3-client-messages.jsonl)
- [v3 server messages](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/.llm/code-samples/protocol/v3-server-messages.jsonl)

See also the [Transport Fallback Contract](architecture/transport-fallback.md) (client-side state machine and the
relay-floor guarantee) and [Handoff and Topologies](architecture/handoff-and-topologies.md) (mesh / host / relay
topologies and the finalization handoff seam).

### Capability negotiation handshake

A v3-capable client advertises its capabilities by adding three optional fields to the first `Authenticate`
message:

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

- `protocol_version` — the highest protocol version the client speaks. When absent, the endpoint default is used
  (`/v2/ws` ⇒ 2, `/v3/ws` ⇒ 3).
- `supported_transports` — data-path transports the client supports. Absent means the capability set is relay-only
  even when `/v3/ws` defaulted the protocol version to 3. Tokens: `relay`, `direct`, `webrtc`.
- `supported_topologies` — session topologies the client supports. Absent means the capability set is relay-only
  even when `/v3/ws` defaulted the protocol version to 3. Tokens: `relay`, `host`, `mesh`.

The server clamps the negotiated version into its configured range:
`negotiated = clamp(client_max, min_protocol_version, max_protocol_version)`, i.e.
`min(client_max, max_protocol_version)` raised to at least `min_protocol_version`. A client that advertises a higher
version than the deployment speaks is clamped **down** to `max_protocol_version`; one that omits the field is
negotiated from the endpoint default (`/v2/ws` defaults to v2; `/v3/ws` defaults to v3). If the negotiated
version is below 3, the connection is **relay-only** regardless of the advertised transports/topologies. If the
negotiated version is 3 but transports/topologies are absent, the connection is v3 relay-only. Defaults:
`min_protocol_version = 2`, `max_protocol_version = 3`.

The negotiated result is echoed back in an extended `ProtocolInfo` (the v2 fields plus three new ones):

```json
{
  "type": "ProtocolInfo",
  "data": {
    "capabilities": ["reconnection", "spectators", "authority"],
    "game_data_formats": ["json", "message_pack"],
    "protocol_version": 3,
    "min_protocol_version": 2,
    "max_protocol_version": 3
  }
}
```

The three new fields are omitted from the wire for a negotiated v2 connection, so the v2 `ProtocolInfo` shape stays
byte-identical.

**Endpoints.** `/v2/ws` and `/v3/ws` share the same handler. `/v3/ws` only changes the _default_ protocol version
to 3 when the client omits `protocol_version`; an explicit `protocol_version` in `Authenticate` always wins (then
clamped). `/v2/ws` behavior is unchanged.

**Back-compat invariant.** A non-relay plan requires _every_ member of a room to be v3-capable and to support the
chosen topology and transport. A single v2 (or relay-only) member forces the whole room to the relay floor, where
no v3 messages are emitted at all. This is the relay-floor guarantee: v2 and v3 clients interoperate, always.

### New v3 messages

These five messages exist only on a negotiated v3 connection.

| Message | Direction | Purpose |
|---|---|---|
| `Signal` | client ⇄ server | Relay an opaque, matchbox-shaped WebRTC signal to/from a specific peer in the same room |
| `NewPeer` | server → client | A new peer is available for a WebRTC connection (late join); designates the offerer |
| `SessionPlan` | server → client | Per-recipient session directive emitted at finalization (alongside `GameStarting`) |
| `TransportStatus` | client → server | Client reports its current data-path transport state (informational; drives metrics) |
| `PeerTransportStatus` | server → client | A same-room peer's reported transport state changed (fan-out of an accepted `TransportStatus`) |

#### Signal

`Signal` carries an **opaque** payload that the server never parses — it is forwarded verbatim to the target peer.
By convention the payload is matchbox-compatible: one of `{"Offer": "..."}`, `{"Answer": "..."}`, or
`{"IceCandidate": "..."}`. The server validates only the envelope (payload size cap, same room, negotiated WebRTC,
rate limit, v3 target); it never inspects the SDP or ICE strings. A payload whose serialized JSON exceeds
`security.max_signal_bytes` (default 16 KiB) is rejected with `SIGNAL_TOO_LARGE` and is not relayed.

Client → server (`to` names the target peer):

```json
{
  "type": "Signal",
  "data": { "to": "<player-uuid>", "signal": { "Offer": "<sdp>" } }
}
```

Server → client (`from` names the originating peer):

```json
{
  "type": "Signal",
  "data": { "from": "<player-uuid>", "signal": { "Answer": "<sdp>" } }
}
```

#### NewPeer

`NewPeer` is the **late-join** pairing delta: it tells the _existing_ members of an already-running v3 WebRTC
session that a peer is now available for a WebRTC peer connection. `you_initiate` designates exactly one side of
each pair as the offerer, avoiding glare (see the glare rule below).

```json
{
  "type": "NewPeer",
  "data": { "peer_id": "<player-uuid>", "you_initiate": true }
}
```

Initial pairing at finalization is owned by `SessionPlan` (below); `NewPeer` covers a peer joining or reconnecting
_after_ finalization — and is sent **only to the existing members**, never to the joiner itself. The joiner's
pairing (the same peers with the mirrored `initiate` flags) arrives in the fresh `SessionPlan` it receives on
entry, keeping the client contract uniform: on `SessionPlan`, (re)configure the session and connect per
`peers[].initiate`; on `NewPeer`, additively connect to that one peer. See the
[late-join decision table](#late-join-decision-table).

#### SessionPlan

`SessionPlan` is the **per-recipient** session directive first emitted at lobby finalization. It is sent alongside
the unchanged `GameStarting` (and only to v3-capable members) when a room negotiates a non-relay plan. A relay-only
room emits **no** `SessionPlan`, so v2 clients never observe it. Each recipient gets its own tailored `peers` list,
`initiate` flags, and, for WebRTC transports, ICE servers with freshly minted TURN credentials. It carries
topology, transport, peers, ICE servers, and relay fallback; it does not carry legacy `ConnectionInfo` or direct
host/port endpoint details.

`SessionPlan` can also be **re-issued mid-session** (same message shape, same v3 gating). Two triggers:

- **Host failover.** When the host of a running `host`-topology session is found to be invalid — gone after a
  departure, self-healed on a late join that finds the stored host missing, or (after a reconnect that downgraded
  its negotiated capabilities) seated but no longer able to run the session — the server re-elects a host over the
  remaining members and sends every remaining v3 member a fresh tailored plan — same topology and transport, new
  `host`, fresh per-recipient ICE for WebRTC. Only members that negotiated v3 plus the session's sticky
  topology/transport pair are electable (a seat-filling relay-only member is never named host of a session it
  cannot run); among those the rule is authority preferred, else earliest joiner, smaller-UUID tie-break. If no
  member qualifies, no plan is re-issued — the session is over and the relay floor carries the room. A host
  departure itself is still signaled by the unchanged `PlayerLeft`.
- **Late join / reconnect into an active non-relay session.** Only the **joiner** receives a plan (its full
  tailored view of the running session: current peers, `initiate` flags, `host`, fresh ICE); existing members
  receive the additive [`NewPeer`](#newpeer) delta instead. The joiner is never sent `NewPeer` — its pairing is
  the `peers[].initiate` flags in its plan.

The topology and transport of a session are **sticky for its lifetime**: the selection ladder runs once at
finalization and is never re-run mid-session, even when departures widen the capability intersection. A re-issued
plan only ever changes membership-derived fields (`peers`, `host`, `ice_servers`). Re-issued and late-join plan
peer lists contain only peers that can run the session — that negotiated v3 plus the session's topology **and**
transport: a v3 member that did not (e.g. a relay-only seat-filler, or one with the `webrtc` transport but not the
session's topology) still receives its plan, but with an **empty** `peers` list — it has no P2P peers and
participates via the relay floor (`host` stays as elected, informational) — and never appears in other members'
`peers` (the `NewPeer` gating applies this same predicate). At finalization this filter is vacuous, because a plan
is only selected when every member supports it. The client contract is uniform:
**the latest `SessionPlan` wins** — (re)configure the session and connect per `peers[].initiate`; on `NewPeer`,
additively connect to that one peer.

```json
{
  "type": "SessionPlan",
  "data": {
    "topology": "mesh",
    "transport": "webrtc",
    "peers": [
      { "player_id": "<player-uuid>", "player_name": "Bob", "is_authority": false, "initiate": true }
    ],
    "ice_servers": [
      { "urls": ["stun:stun.l.google.com:19302"] },
      {
        "urls": ["turn:turn.example.com:3478"],
        "username": "<expiry-unix>:<player-uuid>",
        "credential": "<base64-hmac>"
      }
    ],
    "fallback": "relay"
  }
}
```

Fields:

- `topology` — `relay`, `host`, or `mesh`.
- `transport` — `relay`, `direct`, or `webrtc`.
- `host` — the elected host's player id; present only for `host` topology, omitted otherwise.
- `peers` — the peers _this recipient_ should connect to (always excludes the recipient itself, and lists only
  peers that negotiated the session's topology and transport — empty when the recipient itself did not). Each
  entry carries `player_id`, `player_name`, `is_authority`, and a per-recipient `initiate` flag. In a `mesh`
  plan `is_authority` mirrors the room's `authority_player` (so it is `false` for every peer in a room created
  with `supports_authority: false`); in a `host` plan it marks the elected host (`true` on the host entry in
  client plans, `false` on the client entries in the host's plan).
- `ice_servers` — STUN/TURN servers for WebRTC; omitted (empty) for non-WebRTC plans.
- `fallback` — the universal fallback transport, always `relay` (the floor).

#### TransportStatus

`TransportStatus` lets a client report its current data-path transport state, so the server can distinguish
P2P-connected peers from relay-fallback peers (this drives metrics). It is **purely informational**: the relay
floor never closes regardless of what is reported. Metrics count the first report for a connection and real
per-connection state transitions; duplicate `(transport, connected)` reports do not move counters. The server
accepts a report only from a negotiated v3 connection and only when `transport` is in that connection's
negotiated transport set. Reports from non-v3 clients or for unnegotiated transports are ignored and do not
update per-connection state or metrics.

```json
{
  "type": "TransportStatus",
  "data": { "transport": "webrtc", "connected": true }
}
```

An accepted report that records a real state change is additionally fanned out to the sender's current room
as [`PeerTransportStatus`](#peertransportstatus) (below); ignored and duplicate reports fan out nothing.

#### PeerTransportStatus

`PeerTransportStatus` tells the **other** members of a room that a peer's reported data-path transport state
changed — for example the host's WebRTC path died and it fell back to the relay, so relay-path traffic from it
should be expected. It is the server-side fan-out of an accepted `TransportStatus` report and mirrors that
message's fields, plus the reporting peer's id:

```json
{
  "type": "PeerTransportStatus",
  "data": { "peer_id": "<player-uuid>", "transport": "webrtc", "connected": true }
}
```

Semantics:

- **Deduplicated.** A fan-out fires only when the report records a real per-connection state change — the first
  report on a connection, or a `(transport, connected)` transition. A duplicate report is dropped at the server
  and fans out nothing. (A reconnect clears the stored state, so a reconnected client's first re-report fans out
  again.)
- **Sender excluded; room scoped.** Only the reporter's current room members hear it, never the reporter itself.
  A report from a client that is not in a room is still recorded but fans out nothing.
- **v3-gated per recipient.** Like every v3-only message, it is delivered only to members that negotiated v3
  (Appendix K); a v2 member observes nothing. Deliberately, delivery is **not** gated on the recipient's own
  transport capabilities (unlike `NewPeer` / plan-peer pairing, which apply the full session predicate): this is
  informational status about a _peer's_ data path — useful even to a relay-only v3 member — not an instruction
  for the recipient to use that transport.
- **Purely informational**, like the report it relays: it never changes how the server relays `GameData`.

### Topology / transport selection ladder

At finalization the server picks a single room-wide plan by walking a richest-first ladder and settling on the
first rung that (a) is no richer than the per-game _desired_ ceiling, (b) has its transport enabled in config, and
(c) is supported by **every** member. Otherwise it falls to the universal relay floor:

```text
mesh + webrtc      ← richest
host + webrtc
host + direct
relay (floor)      ← always available
```

Rules:

- **All-members-v3 required.** Any non-relay rung requires every room member to be v3-capable _and_ to support that
  rung's topology and transport. A single non-supporting member skips the rung.
- **Relay floor always wins** when no rung fits. A relay-floor room emits no `SessionPlan` and relays exactly like
  v2.
- **`desired` is a ceiling, not an exact match.** A mesh-preferring room that cannot run mesh falls back to a host
  topology before collapsing to relay.

This ladder is the single source of truth in `src/server/session_policy.rs` (`UPGRADE_LADDER` + `RELAY_FLOOR`).

### Late-join decision table

A peer joining or reconnecting _after_ finalization is brought up to date from the **stored** plan the room is
actually running — the decision recorded at finalization (the ladder is _not_ re-run over the current members, so
a session that finalized to the relay floor stays relay even if every remaining member could now do better). The
joiner's view arrives as a fresh `SessionPlan`; existing members get the additive `NewPeer` delta:

| `room.lobby_state` | Stored (running) plan | Joiner receives | Existing members receive |
|---|---|---|---|
| not `Finalized` | any | nothing (initial pairing is owned by the finalize-time `SessionPlan`) | nothing |
| `Finalized` | none (relay floor / pre-v3 room) | nothing | nothing |
| `Finalized` | `mesh + webrtc` | `SessionPlan` (every session-capable current peer, glare `initiate`, fresh ICE) | `NewPeer` to every session-capable member |
| `Finalized` | `host + webrtc` | `SessionPlan` (star view: client targets the stored host; a rejoining host targets all clients) | `NewPeer` along the star edge only |
| `Finalized` | `host + direct` | `SessionPlan` (empty `ice_servers`) | nothing (`NewPeer` is WebRTC-only) |

The joiner-directed plan is v3-gated; the `NewPeer` delta additionally requires — of the joiner **and** of every
announced-to member — the full session predicate: v3 plus the session's topology **and** transport, the same rule
that filters plan peer lists, so existing members are never told to pair with a peer the plan itself would not
list. In particular, a v3 joiner that cannot run the session — a relay-only client, or one that negotiated the
`webrtc` transport but **not** the session's topology (e.g. `topologies: ["relay"]` entering a `mesh + webrtc`
session) — still receives the (v3-gated) `SessionPlan` describing the running session — with an **empty** `peers`
list, since every pair with it sits outside the session contract (a relay-only peer would additionally be rejected
by `Signal` validation); `fallback: "relay"` is its data path — and no `NewPeer` pairing fires for it in either
direction. Symmetrically, a session-incapable member already seated in the room is omitted from a capable joiner's
`peers` and receives no `NewPeer` about the joiner. `NewPeer` is emitted only
when the stored **transport** is `webrtc`; a `host + direct` (LAN) session emits no `NewPeer` because there is no
WebRTC signaling to broker — its `SessionPlan` still names the `host + direct` topology/transport and peers; any
address metadata remains the legacy, self-declared `GameStarting.peer_connections` / `ProvideConnectionInfo`
surface rather than a negotiated v3 transport proof. After a host failover the stored host is the _re-elected_
one, so an ex-host that reconnects is paired as a client of the new host.

### Glare / offerer rule

For any WebRTC pair, exactly one side must send the offer. Which side is encoded in the `initiate` flag on a
`SessionPlan` peer and in `you_initiate` on `NewPeer`:

- **Mesh:** the peer whose `player_id` is the **lesser** of the two UUIDs sends the offer (a deterministic,
  stateless, antisymmetric rule).
- **Host:** the direction is fixed regardless of UUID order — each **client initiates to the host**, and the host
  answers every client. Clients never signal each other in a star topology.

### ICE and TURN credentials

Every WebRTC `SessionPlan` carries an `ice_servers` list:

- **STUN is always present** in a WebRTC plan (the configured `turn.stun_urls`, advertised credential-less since
  public STUN needs no auth).
- **Ephemeral per-player TURN credentials** are added when the `[turn]` block is enabled (with non-empty
  `urls`). Each recipient receives its **own** short-lived credential: `username` is
  `"<expiry-unix>:<player-uuid>"` and `credential` is the base64 of `HMAC-SHA1(static_auth_secret, username)`
  (the coturn REST scheme). The static auth secret is **never** sent to clients. The `username` / `credential`
  values in the
  [v3 server-message samples](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/.llm/code-samples/protocol/v3-server-messages.jsonl)
  are **illustrative placeholders, not a real credential** (the sample `credential` is not the actual HMAC of the
  shown `username`).

See the [TURN and STUN configuration](configuration.md#turn-and-stun-ice-credentials-protocol-v3) section and the
[Transport Fallback Contract](architecture/transport-fallback.md) for the full ICE/fallback behavior.

### ICE pre-gather

`RoomJoined` and `Reconnected` carry an optional `ice_servers` field (same shape and composition as the
`SessionPlan` list: the operator's static `session.ice_servers` first, then the configured STUN, then a freshly
minted per-player TURN credential) so a WebRTC-capable client can start gathering ICE candidates **during the
lobby wait** instead of adding that latency at game start. The field is populated **iff all of**:

- `session.enable_ice_pregather` is `true` (the default; `false` is the operator kill switch), and
- `session.enable_webrtc` is `true`, and
- the game's desired topology (per-game mapping, else `session.default_topology`) is non-relay — a relay-desired
  game can never select a WebRTC plan, so minting for it would hand out credentials that can never be used, and
- the room is **not** `Finalized` — a join/reconnect into an active non-relay session already receives a fresh
  per-recipient `SessionPlan` (pre-gathering too would double-mint), and a room floored to relay stays relay
  (sticky), so pre-gather is pointless there, and
- the recipient negotiated v3 **and** the `webrtc` transport, and
- the recipient's negotiated topologies contain the game's desired topology — the relay-desired argument applied
  per-recipient: the ladder seats a member on a rung only when that member negotiated the rung's topology, so a
  relay-only-topology client can never appear in any WebRTC plan and its credentials could never be used.

In every other case the field is **absent from the wire entirely**, so the v2 `RoomJoined` / `Reconnected` bytes
are untouched. The `SessionPlan` ICE list **supersedes** the pre-gather list: clients should always apply the most
recent set — pre-gather TURN credentials can expire during a long lobby (their TTL starts at join time), and fresh
ones always arrive in the `SessionPlan`.

### Sequence diagrams

**Mesh + WebRTC finalization → connect → fallback.** Two v3 peers A and B (with `A < B` by UUID, so A offers):

```text
A                         server                          B
|  Authenticate (v3, mesh+webrtc)  |                       |
|--------------------------------->|<----------------------|  Authenticate (v3, mesh+webrtc)
|  (both ready → finalize)         |                       |
|<--- GameStarting ----------------|---- GameStarting ---->|
|<--- SessionPlan(mesh,webrtc) ----|---- SessionPlan ----->|   per-recipient: A.peers=[B initiate=true],
|                                  |                       |                  B.peers=[A initiate=false]
|  Signal{to:B, Offer} ----------->|---- Signal{from:A} -->|
|<-- Signal{from:B} ---------------|<--- Signal{to:A,Answer}|
|  Signal{to:B, IceCandidate} ---->|---- Signal{from:A} -->|   (ICE trickle, both directions)
|  == WebRTC data channel open ==  |                       |
|  TransportStatus{webrtc, true} ->|<-- TransportStatus{webrtc, true}
|                                  |                       |
|  (if P2P fails or times out)     |                       |
|  TransportStatus{webrtc, false}->|   server keeps relaying GameData (floor never closes)
|  GameData over relay ----------->|---- GameData -------->|
```

**Host + WebRTC finalization.** Clients C1, C2 and elected host H:

```text
C1                        server                          H (host)            C2
|  SessionPlan(host,webrtc,host=H) |                       |                   |
|<--- (C1.peers=[H initiate=true]) |-- SessionPlan ------->|-- SessionPlan --->|  (C2.peers=[H initiate=true];
|                                  |   H.peers=[C1,C2 initiate=false]          |   each client offers to H)
|  Signal{to:H, Offer} ----------->|---- Signal{from:C1} ->|                   |
|<-- Signal{from:H, Answer} -------|<--- Signal{to:C1} ----|                   |
|                                  |<--- Signal{from:C2} --|<-- Signal{to:H,Offer} (C2 offers to H)
|  == C1⇄H channel open ==         |    == C2⇄H channel open ==                |
|  (C1 and C2 never signal each other in a star topology)                     |
|  (on failure: each client falls back to GameData over the relay floor)      |
```

## Protocol v3 delivery reliability

The v3 additions above cover WebRTC signaling. v3 ALSO adds a delivery
reliability surface (there is no separate v4: v3 is the single unshipped
"current" version, so everything additive over the frozen v2 floor negotiates
under `protocol_version: 3`). A deployment can clamp
`protocol.max_protocol_version` back to `2` to disable all of it (pure v2).
This surface exists so clients can _detect_ relay loss end-to-end instead of
trusting the delivery contract blindly.

### Sequenced relay (`seq`)

Relayed `GameData` (JSON) and `GameDataBinary` / bare MessagePack frames
delivered to a v3 recipient carry an additional server-stamped `seq` field:
a per-(sender, room) counter that starts at `1` and increases by exactly one
for every message the server relays from that sender to the room. Pre-v3
(v2) recipients in the same room receive byte-identical frames with no `seq` key.

Recipient rules:

- Per sender, `seq` is strictly contiguous while you stay connected. A gap
  can only mean (a) the server abandoned messages together with _your_
  slow-consumer disconnect (you were told: `SLOW_CONSUMER` + close), or
  (b) the sender left and rejoined, which resets its counter to `1` — and the
  `epoch` bumps at the same time (see below), so the reset is self-describing;
  you are also told out of band (`PlayerLeft` / `PlayerJoined` /
  `PlayerReconnected`), or
  (c) a single binary payload that could not be converted for your
  negotiated format was replaced in-stream by an `Error` with code
  `UNSUPPORTED_GAME_DATA_FORMAT` (you were told, and the connection stayed
  open) — you skip that one `seq` while other recipients receive it.
- An unexplained gap — one with none of the above notifications — is a
  server bug; report it. That is exactly the condition the sequence numbers
  exist to make observable.

### Incarnation epoch (`epoch`)

Alongside `seq`, every relayed `GameData` / `GameDataBinary` to a v3 recipient
also carries an `epoch`: a **monotonic per-sender** counter that increments once
per **incarnation** of that sender's membership — its first-ever incarnation is
`epoch` 1, and each join-after-leave or reconnect increments it. The server
tracks it per sender connection and never resets it on a room switch, so a
sender's first frame in a given room may begin at `epoch` 2 or higher if that
sender was previously in another room — do not assume a room's first observed
epoch is 1. What the contract guarantees is per `(sender, room)`: because `seq`
restarts at `1` within every epoch, the pair `(epoch, seq)` is strictly
**lexicographically increasing** per `(sender, room)` as observed by any single
recipient:

- `(1, 1), (1, 2), (1, 3)` — the sender's first incarnation, and then
- `(2, 1), (2, 2), …` — after the sender left+rejoined or reconnected.

This makes the `seq` reset in rule (b) above **self-describing**: you attribute
the backwards `seq` jump to the `epoch` bump directly, rather than having to
correlate a separately-ordered `PlayerLeft`/`PlayerJoined`/`PlayerReconnected`
control message. Each member's current epoch is also carried on the room
snapshots — `RoomJoined.current_players[].epoch`, `PlayerJoined.player.epoch`,
`PlayerReconnected.epoch`, and the `Reconnected` member snapshot — so you can
baseline a sender's stream before its first relayed frame arrives. Like `seq`,
`epoch` is stripped for pre-v3 (v2) recipients (their bytes stay byte-identical), so
its absence and its presence are both part of the frozen wire contract.

The `epoch` value is only meaningful **relatively**: baseline each sender from
the `epoch` you first observe for it (on a snapshot or its first frame) and
compare subsequent values against that — do NOT assume a newly observed sender
starts at `epoch` 1. The server tracks epoch as a single monotonic counter per
connection, so a sender that reached your room after being in another room (on
the same connection) may first appear at `epoch` 2 or higher. The only
guarantee — and the only one you need — is that, for a given sender in your
room, `(epoch, seq)` never goes backwards while you stay connected.

### RelayStats

Periodic per-connection delivery accounting, emitted only to v3 connections
and only when `websocket.delivery_stats_interval_secs` is nonzero (default
`0`, disabled):

```json

{
  "type": "RelayStats",
  "data": {
    "interval_ms": 5000,
    "sent_to_you": 1234,
    "dropped_for_you": 0,
    "backpressure_events": 3
  }
}

```

Counters are cumulative for the life of the connection. `dropped_for_you`
becoming nonzero always coincides with your own slow-consumer disconnect;
`backpressure_events` rising while your `seq` stream stays contiguous means
you are draining slower than senders produce — pace yourself or expect
eviction. Together with `seq`, this lets a client attribute loss ("server
dropped and told me" vs "my own bug") without server log access.

## Next Steps

- [Getting Started](getting-started.md) - Basic usage examples
- [Platform Integration Guide](guides/platform-integration.md) - which WebRTC stack to use per platform
- [Features](features.md) - Complete feature overview
