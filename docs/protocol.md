# Protocol Reference

Signal Fish Server uses a JSON-based WebSocket protocol. All messages are JSON objects with a `type` field and
optional `data` field.

MessagePack encoding is also supported for game data when `enable_message_pack_game_data` is enabled.

Illustrative v2 examples and canonical, machine-checked v3 wire samples:

- v2 client messages (non-exhaustive examples):
  <https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/.llm/code-samples/protocol/v2-client-messages.jsonl>
- v2 server messages (non-exhaustive examples):
  <https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/.llm/code-samples/protocol/v2-server-messages.jsonl>
- v3 client messages (canonical wire frames):
  <https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/.llm/code-samples/protocol/v3-client-messages.jsonl>
- v3 server messages (canonical wire frames):
  <https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/.llm/code-samples/protocol/v3-server-messages.jsonl>

## Client Messages

### Authenticate

Submit the application's public app ID (required when app-ID allowlist
enforcement is enabled). This identifies an accounting/quota context; it is not
proof of client or tenant identity because any client can replay a known ID.

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
- `protocol_version`, `supported_transports`, and `supported_topologies` - v3 negotiation fields described in
  [Capability negotiation handshake](#capability-negotiation-handshake)
- `requested_capabilities` - v3 additive extensions the client wants to use; unknown tokens are ignored and the
  client must wait for the same token in `ProtocolInfo.capabilities`

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
- `relay_transport` - Reserved compatibility hint (`tcp`, `udp`, `websocket`, or `auto`). The server currently
  ignores this field: omission and every accepted value use the same authenticated WebSocket relay path.

New clients should omit `relay_transport`. It remains accepted so existing
protocol-v2 payloads continue to decode, but it will not become actionable
without an explicitly negotiated contract. Removing it requires a future
breaking protocol version; migration is simply to stop sending the field.

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
field and can be any JSON value (object, array, scalar, or null) and is
relayed verbatim.

Omitting delivery metadata selects the reliable relay-floor behavior used by
protocol v2. A negotiated-v3 client may instead classify JSON `GameData`:

```json
{
  "type": "GameData",
  "data": {
    "data": { "position": [100, 200] },
    "class": "latest",
    "key": 7
  }
}
```

The valid v3 combinations are exact:

| `class` | `key` | Meaning |
| --- | --- | --- |
| omitted or `reliable` | omitted | Preserve the message or close the slow recipient loudly |
| `latest` | required `u32` | Retain only the newest queued value for this sender-defined key |
| `volatile` | omitted | Deliver opportunistically without pacing the sender |

A well-typed but illegal class/key pairing returns `INVALID_DELIVERY_CLASS` and
is not relayed. Malformed metadata -- including an unknown class token, an
out-of-range or non-integer key, or explicit `null` -- fails message decoding
with `INVALID_INPUT`. A connection below v3 may send only the legacy form with
both fields omitted. Raw client WebSocket binary frames have no class/key
envelope and are always reliable; delivery classes apply only to JSON
`GameData`.

Like every other command surface, game data sent from a connection that is
not currently seated in any room is rejected with `NOT_IN_ROOM` (both the
JSON and raw-binary lanes) and never relayed. Other per-frame validation
runs first: delivery-metadata violations answer `INVALID_DELIVERY_CLASS`,
and oversized binary payloads answer `MESSAGE_TOO_LARGE`, regardless of
seat state. This includes spectators, whose connections are never seated in
the player roster.

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
member. It then emits a per-recipient [`SessionPlan`](#sessionplan) to every
negotiated-v3 member. A relay-resolved room sends an explicit `relay`/`relay`
plan with no peers; protocol-v2 members receive no plan. Sending `StartGame` to
an already `finalized` room returns an `Error` with `INVALID_ROOM_STATE`.

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
seconds counted from the disconnect. That window is measured on a monotonic
clock captured at the disconnect, so a host clock adjustment cannot close it
early or keep it open late, and a reconnect attempt after it elapses is
rejected with `RECONNECTION_EXPIRED` (a bad or mismatched token remains
`RECONNECTION_TOKEN_INVALID`). For pure-v2 clients the token is still
minted at disconnect time and never reaches the wire — reconnection is
effectively a v3+ feature.

### ProvideConnectionInfo

Provide legacy, self-declared peer connection metadata. It remains visible in
v2-compatible room snapshots and `GameStarting.peer_connections`. It is not a
v3 capability negotiation signal and does not prove reachability. One narrow v3
use exists: a syntactically usable `direct` host/port makes an otherwise
capability-compatible `host + direct` rung executable and is repeated as the
elected host's `SessionPlan.direct_endpoint`. `ProvideConnectionInfo` is not
itself a capability, status, or metrics event, and it does not authorize
`Signal` or `NewPeer`; those retain their negotiated-v3 gates.

The `relay` variant is also unvalidated client input. Signal Fish stores and
forwards its host, port, transport, allocation, token, and client ID, but does
not resolve or probe the endpoint, authenticate the token, bind the claimed
client ID to the sender, or select/open that path. A consuming integration
must validate reachability, credentials, and source identity before use; do not
put secrets here unless every receiving peer may see them.

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
- `room_code` - Code of the room to spectate (validated and matched case-insensitively)
- `spectator_name` - Name for the spectator

Spectator admission consumes the same per-player room-admission attempt budget
as seated joins and room creation. Invalid game names, invalid room codes, and
exhausted admission budgets return `SpectatorJoinFailed` with `INVALID_GAME_NAME`,
`INVALID_ROOM_CODE`, or `RATE_LIMIT_EXCEEDED`, respectively.

### LeaveSpectator

Leave spectator mode.

```json

{
  "type": "LeaveSpectator"
}

```

This message has no data payload.

## Server Messages

### TokenBindingChallenge

On a connection that negotiated `signalfish.tokenbinding.v2`, this is the first
server application message after the WebSocket upgrade. Its 256-bit random
nonce contributes server freshness to the per-connection proof key; the
sequence applies to every subsequent client JSON and binary frame.

```json

{
  "type": "TokenBindingChallenge",
  "data": {
    "version": 2,
    "scheme": "server_nonce_hkdf_sha256",
    "nonce": "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=",
    "first_sequence": 1
  }
}

```

The nonce and sequence state expire with this WebSocket and are never resumed
on reconnect. The sequence and every integer in a signed JSON message are
limited to the interoperable JSON range
`-9007199254740991..=9007199254740991`. See the [token-binding configuration
recipe](configuration-recipes.md#token-binding) for the normative HKDF, RFC
8785, HMAC, binary-envelope, and certificate-rotation contract.

### Authenticated

The public app ID was accepted. Includes its configured context and rate limits;
this legacy message name does not prove client identity.

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

`per_minute` is enforced when the accepted app entry configures
`rate_limit_per_minute`. The frozen v2 `per_hour` and `per_day` fields are
legacy advisory projections for client budgeting and are not enforced by the
server.

Optional fields:

- `organization` - Organization name (if any)

### ProtocolInfo

SDK/protocol compatibility details advertised after the app-ID handshake.

```json

{
  "type": "ProtocolInfo",
  "data": {
    "capabilities": ["reconnection", "spectators", "authority"],
    "game_data_formats": ["json", "message_pack"]
  }
}

```

Negotiated-v3 responses also include `max_outbound_message_size`, the maximum
aggregate encoded application payload the deployment will send in one
WebSocket message. Native clients can read the same decimal byte value from
the `x-signal-fish-max-outbound-message-size` HTTP upgrade response header.
The field remains absent from negotiated-v2 `ProtocolInfo` to preserve its
frozen wire shape.

Every client, including browser and negotiated-v2 clients, can fetch the same
version-neutral JSON before opening a socket: replace the endpoint's `/ws`
suffix with `/client-config` (`GET /v2/client-config` or
`GET /v3/client-config`). The endpoint is governed by the deployment's normal
HTTP CORS policy and returns
`{"max_outbound_message_size":8388608}`. Configure the WebSocket receive limit
to at least that value before connecting. Valid deployment values are
`1..=67108864`, an exact portable range for JavaScript and 32-bit clients. The
response uses `Cache-Control: no-store` so a deployment change cannot leave a
client with a stale receive ceiling.

The limit counts the complete JSON UTF-8 text payload or MessagePack binary
payload after Signal Fish protocol encoding and before WebSocket framing.
Signal Fish serializes through a fallible bounded writer and checks the encoded
payload before handing it to the WebSocket sink, so allocation growth and a
fragmented transport message cannot bypass the cap.
If a roster, reconnect replay, spectator snapshot, custom ICE/TURN data, or
other legal aggregate exceeds the limit, the server writes none of that
application message, abandons later queued messages for the affected client,
and closes it with `1009 outbound_message_too_large`. Protocol data is never
silently truncated.
If the connection was already closing for a distinct operational cause such as
shutdown or slow-consumer eviction, that earlier causal close code remains
authoritative and an oversized best-effort farewell is simply omitted.

For negotiated binary-format conversion, the opaque source payload must also
fit the outbound cap before the server decodes it into a JSON value tree. This
bounds compact MessagePack allocation amplification. An over-budget conversion
uses the existing `UnsupportedGameDataFormat` delivery-report/advisory path;
direct binary forwarding remains zero-copy and is judged by its encoded frame.

### AuthenticationError

The app-ID handshake was rejected. This frozen legacy message name does not
mean a client credential was checked.

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
        "id": "player-id-1",
        "name": "Player 1",
        "is_authority": false,
        "is_ready": false,
        "connected_at": "2024-01-01T00:00:00Z",
        "epoch": 1,
        "seq": 0
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

Every `relay_type` field in `RoomJoined`, `Reconnected`, and
`GameStarting.peer_connections` is a deployment-defined legacy integration
label. It is informational only: it neither selects nor proves the active
physical transport. Use the authenticated WebSocket relay path or the
negotiated `SessionPlan` for executable routing.

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
      "connected_at": "2024-01-01T00:00:00Z",
      "epoch": 1,
      "seq": 0
    }
  }
}

```

### PlayerLeft

A player left the room. Protocol v3 includes the departed incarnation's
terminal relay watermark:

```json

{
  "type": "PlayerLeft",
  "data": {
    "player_id": "player-id",
    "epoch": 1,
    "final_seq": 42
  }
}

```

Priority control can overtake queued game data. A v3 recipient therefore keeps
the departed sender's cursor until delivered frames plus causally prior exact
`DeliveryReport` gaps cover `final_seq` in `epoch`, then retires that sender's
cursor, gaps, and tombstone. `final_seq: 0` retires immediately. Protocol v2
retains its frozen one-field `data` object and receives neither watermark field.

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

For a v3 recipient, the server adds `seq` and `epoch` and echoes `class`/`key`
when the sender supplied them:

```json
{
  "type": "GameData",
  "data": {
    "from_player": "00000000-0000-0000-0000-00000000000a",
    "data": { "position": [100, 200] },
    "seq": 43,
    "epoch": 1,
    "class": "latest",
    "key": 7
  }
}
```

An omitted `class` means reliable. Within the v3 delivery lane, the class never
changes: reliable data is never coalesced or reclassified, and `latest` values
coalesce only with the same `(sender, room, key)`. Pre-v3 recipients use their
legacy reliable lane and do not receive either field.

### GameDataBinary

Binary game data payload from another player. This server message variant is an
internal broadcast carrier only. Every negotiated-v3 binary recipient receives
a WebSocket binary frame containing a MessagePack metadata map. The `encoding`
field describes the opaque `payload` bytes; it does not change the outer
envelope encoding. The frame is not wrapped in the JSON
`{ "type": ..., "data": ... }` envelope.

```text
MessagePack map:
  from_player: 16 UUID bytes  # MessagePack bin, RFC 4122/network byte order
  encoding: json | message_pack | rkyv
  payload: raw bytes          # MessagePack bin
  seq: 43       # required for v3
  epoch: 1      # required for v3
```

For a recipient whose negotiated format differs, the server attempts a JSON
`GameData` fallback by decoding JSON or MessagePack payload bytes. An opaque
`rkyv` payload cannot be converted without its application type: the recipient
instead gets an `unsupported_format` DeliveryReport followed by
`UNSUPPORTED_GAME_DATA_FORMAT` (or just the legacy error on v2). The currently
advertised binary format is `message_pack`; `rkyv` remains reserved/internal.
The uniform v3 envelope still covers every internal binary encoding so no v3
binary delivery can lose its sender identity or accountability stamp.

`from_player` is the UUID's canonical 16-octet sequence, in network byte order;
it is not the UTF-8 bytes of the hyphenated UUID string. Decoders must require a
MessagePack binary value of exactly 16 bytes and render those bytes as the
usual lowercase hyphenated UUID for application-facing identifiers.

The v2 wire remains frozen: MessagePack recipients receive the historical
three-field map (`from_player`, `encoding`, `payload`), while legacy JSON/rkyv
binary paths pass the payload bytes through unchanged. Neither v2 form carries
`seq` or `epoch`.

Binary game data is always reliable. The bare binary frame has no `class` or
`key`, and clients must not infer a delivery class from its contents.

### Delivery semantics

The v2 contract and an unclassified v3 `GameData` are reliable: a full data
queue applies backpressure for at most
`websocket.slow_consumer_timeout_ms` (default 5000). If space does not become
available, the recipient is closed loudly as a slow consumer. Raw binary game
data is also always reliable. The capacity boundary uses the queue writer's
continuous-availability timestamp: capacity that becomes available strictly
before the deadline and remains non-full may be claimed after a delayed
producer poll, while capacity first available at or after the exclusive
boundary remains expired.

Negotiated v3 adds two explicitly lossy JSON classes. Loss is never silent:
every omitted server-stamped sequence range is named by a prior
[`DeliveryReport`](#deliveryreport).

Class policy is per recipient. A pre-v3 recipient remains on its reliable FIFO
lane even when a v3 sender supplies class metadata; the server strips the v3
fields from that recipient's wire frame.

| Class | Full-queue behavior | Terminal outcomes |
| --- | --- | --- |
| `reliable` | Wait for space, then close loudly on timeout | `delivered`, `abandoned`, `unsupported_format` |
| `latest` | Replace an older queued value with the same key; for a new key, evict the oldest queued volatile value or drop the latest arrival | `delivered`, `superseded`, `dropped_full`, `abandoned`, `unsupported_format` |
| `volatile` | Evict the oldest queued volatile value when possible; otherwise drop the arrival | `delivered`, `dropped`, `abandoned`, `unsupported_format` |

`latest` and `volatile` never wait for data-queue capacity and therefore never
backpressure their senders. Latest replacement is per
`(sender, room, key)`: interleaved keys have independent histories, so there is
no scalar "superseded sequence" field on a successor.

At quiescence, every attempted message for a recipient connection has exactly
one terminal outcome:

```text
attempted_reliable = delivered + abandoned + unsupported_format
attempted_latest   = delivered + superseded + dropped_full + abandoned + unsupported_format
attempted_volatile = delivered + dropped + abandoned + unsupported_format
```

Before quiescence, queued and in-flight messages are still pending and belong on
neither side's terminal totals. Delivery counters are cumulative snapshots for
one physical connection. They cannot identify a missing sender, epoch, or
sequence, and a snapshot observed before close need not include messages
abandoned during that close.

A close can cancel the socket write that owns one queued payload, and the
cancellation may land either side of the point where the sink accepts the frame,
so that payload's wire position is unknown. The server therefore writes nothing
that was still queued behind it: the recipient's stream ends as a gap-free
prefix, and the remainder is counted `abandoned`. Truncation at close is always
legal; a hole never is.

V3 control messages use a separate bounded priority lane
(`websocket.control_queue_capacity`, default 128). Within the recipient's
active room generation, the writer drains that lane strictly before queued data
so a gap-bearing `DeliveryReport`, peer lifecycle event, or error cannot starve
behind a data backlog. The report for a lossy operation is queued atomically
before any later data may expose its gap. The recipient's own `RoomLeft`,
`RoomJoined`, `Reconnected`, `SpectatorJoined`, and `SpectatorLeft` transitions
are generation barriers: the old generation drains before the transition, and
no old-room data appears after the new snapshot. If the server cannot preserve
that ordering or record exact accountability, it fails closed: no later data is
exposed and the connection closes with `4002 slow_consumer`. V2 retains its
legacy FIFO ordering.

Queue priority ends once TCP accepts bytes, so accepted sockets also inherit a
bounded send-buffer request (`websocket.socket_send_buffer_bytes`, default
65536; `0` keeps the platform default). This limits data already committed
ahead of a later WebSocket Ping or delivery report. The operating system may
clamp or account the request differently; Linux commonly reports twice the
requested value.

`websocket.max_sojourn_ms` (default 15000) is class-aware. Reliable traffic is
bounded from the oldest reliable queue/batch enqueue through socket-write
completion. Control traffic uses its own enqueue timestamp, so stale lossy
data cannot expire a fresh report. Latest/volatile queue age is resolved by
their explicit coalesce/drop policy; after selection, the same duration bounds
socket-write progress so a transport that stops draining cannot wedge the sole
writer. Deadline expiry fails closed with `4002 slow_consumer`. Farewell
`Error` frames are best effort; the close code is authoritative. These are
exclusive deadlines: socket-write completion must be observed strictly before
the boundary. A write that becomes ready at or after the deadline is expired,
even when scheduler delay makes both the write and timer ready on the same
poll.

Practical consequences:

- Clients driving async runtimes must continuously poll/drive their
  connection. A runtime that is merely "ticked" occasionally starves the
  transport: inbound frames back up on the server side and manifest as
  apparent stalls, ending in a `SLOW_CONSUMER` disconnect.
- Reliable throughput is bounded by the slowest recipient's ability to drain
  its connection. `latest` and `volatile` traffic keeps producers moving but
  requires clients to process `DeliveryReport` before later data. A dead
  recipient costs reliable senders at most one timeout window before it is
  evicted. Operators can watch the
  `signal_fish_websocket_backpressure_events_total` and
  `signal_fish_websocket_slow_consumer_disconnects_total` Prometheus
  counters to spot backpressure and slow-consumer evictions in production. The
  labeled `signal_fish_websocket_delivery_class_outcomes_total{class,outcome}`
  counter and JSON
  `metricsSnapshot.connections.delivery_by_class` (`connections.delivery_by_class`
  within the raw snapshot) expose each class's attempted and terminal outcomes;
  the equality above holds at quiescence.
- Close `4002 slow_consumer` describes the failed delivery contract, not a
  diagnosis that the peer made zero physical progress. Sustained reliable input
  above a recipient's drain rate must eventually fail closed even while each
  individual write still advances; reliable traffic can neither be dropped nor
  buffered without a bound. Use measured frame size, link rate, queue capacity,
  and `max_sojourn_ms` to size that boundary. No separate oversubscription close
  code is emitted.

A binary game-data payload that cannot be converted for a v3 recipient is not
silently dropped either: the server accounts the omission as an exact
`DeliveryReport` gap with reason `unsupported_format`, and attempts a
supplemental `Error` with code `UNSUPPORTED_GAME_DATA_FORMAT` after it. Both
precede any later data for that recipient. Consecutive omissions from one sender
coalesce into one range and one report — at most one per second while the
recipient is otherwise idle — so a recipient that cannot represent the room's
encoding is not charged one report frame per relayed message. The supplemental
error is best effort: if its write fails after the report succeeds, the socket
disconnects and no successor is exposed. The report, not the aggregate counter
or error alone, authorizes a gap on a continuing stream. Once the writer has
deterministically classified the conversion as unsupported, that payload has
reached its accounted terminal outcome; its report/advisory uses the bounded
socket-progress deadline rather than inheriting unresolved reliable queue age.

### Close codes

Farewell `Error` frames are best-effort: on the congested socket a
slow-consumer eviction escapes, they frequently cannot be delivered at all.
The WebSocket close frame's code travels in the closing handshake itself, so
it is the one attribution signal a client can always read. The server uses
the standard RFC 6455 `1000` and `1009` codes plus stable private-range
assignments that are never renumbered:

| Code | Reason string | Meaning |
| ---- | ------------- | ------- |
| `4000` | `server_shutdown` | The server is shutting down after a graceful drain |
| `4001` | `auth_timeout` | Authentication input was not observed strictly before the `websocket.auth_timeout_secs` deadline |
| `4002` | `slow_consumer` | Delivery contract failed closed: reliable queue/sojourn timeout, selected socket write stopped progressing, or exact accountability/control priority could not be preserved |
| `4003` | `activity_timeout` | An idle server WebSocket Ping write timed out, the matching Pong missed its deadline while no inbound or outbound application progress superseded the probe, or the `server.ping_timeout` activity reaper evicted the connection. A Ping queued after outbound progress inherits the earlier capacity-wait/maximum-sojourn delivery budget and closes `4002` if that write stalls |
| `4004` | `idle_timeout` | No inbound frame was observed strictly before the `websocket.idle_timeout_secs` deadline |
| `4005` | `room_inactive` | The assigned room exceeded `server.inactive_room_timeout` and was deleted; the client must join or create a new room |
| `1000` | `unregistered` | Normal closure (leave, replaced connection, ordinary teardown) |
| `1009` | `outbound_message_too_large` | A complete encoded server application message exceeded the advertised outbound payload limit; no prefix of that message was written |

During a shutdown drain the process stops accepting new WebSocket upgrades,
rejects new room creation with `SERVER_DRAINING`, sends v3 clients a best-effort
[`GoingAway`](#goingaway) advisory, then closes remaining sockets with `4000
server_shutdown` after `server.drain_grace_secs` (default 30). Shutdown-drain
disconnects do not arm reconnection tokens; the instance is going away, so
clients should create or join a fresh room on another healthy instance.

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
    "reason": null
  }
}

```

Note: The frozen v2 wire shape always carries `reason`; it is `null` on a
granted response and a human-readable string on a denial. The optional
`error_code` is omitted when granted and names the cause when denied (see
[Authority](concepts/authority.md#requesting-authority) for the full table).

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

`peer_connections` carries player identity, authority, a legacy integration
label, and optional
self-declared `connection_info` from `ProvideConnectionInfo`. It is kept for
v2/back-compat and does not prove direct or WebRTC reachability. v3 clients use
the negotiated `SessionPlan` for its generation fence, topology, transport,
peers, ICE servers, and relay fallback.

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
- `MISSING_APP_ID` - Required app-ID handshake was not completed
- `INVALID_APP_ID` - Invalid app ID
- `INVALID_DELIVERY_CLASS` - Well-typed but illegal v3 class/key pairing
- `INVALID_INPUT` - Malformed message or delivery metadata

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
`event_buffer_size = 0`). v3+ recipients also receive
`sender_watermarks`, the authoritative `(epoch, seq)` tail for every current
room member, so they can re-baseline after skipped `GameData`. See
[Reconnection Flow](#reconnection-flow).

`sender_watermarks` replaces every pre-disconnect sequence expectation; it is
not a replay promise. A new physical connection also starts new
connection-scoped `DeliveryReport` counters and gap-accounting state. Clients
must resynchronize application state after any reconnect, especially when
`replay` is `truncated` or `unavailable`.

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
        "id": "player-id-1",
        "name": "Player 1",
        "is_authority": false,
        "is_ready": false,
        "connected_at": "2024-01-01T00:00:00Z",
        "epoch": 1,
        "seq": 42
      }
    ],
    "is_authority": false,
    "lobby_state": "lobby",
    "ready_players": ["player-id-1"],
    "relay_type": "matchbox",
    "current_spectators": [],
    "missed_events": [],
    "replay": "complete",
    "sender_watermarks": [
      {
        "player_id": "player-id-1",
        "epoch": 1,
        "seq": 42
      }
    ]
  }
}

```

As in `RoomJoined`, `relay_type` is an informational legacy label, not a
transport selector or reachability proof.

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
        "connected_at": "2024-01-01T00:00:00Z",
        "epoch": 1,
        "seq": 42
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

Note: The `reason` field is optional. On a negotiated-v3 connection, every
`current_players` entry carries its current `epoch` and exact recipient-visible
`seq` baseline, as shown above. Pre-v3 recipients receive the same snapshot
without either field.

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

Note: The `error_code` field is optional. Spectator admission validation and
throttling use `INVALID_GAME_NAME`, `INVALID_ROOM_CODE`, and
`RATE_LIMIT_EXCEEDED` on this role-specific response.

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
  |    (+ SessionPlan on v3)           |
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
`unavailable` replay means control history is incomplete. v3 clients also use
`sender_watermarks` from `Reconnected` to reset each current member's
`(epoch, seq)` baseline after their absence. Discard all old next-sequence
expectations and connection-scoped delivery counters; the replacement socket
starts a new accounting lifetime. Clients must still treat reconnection as
requiring an application-level state resync (for example, have the authority or
another peer re-send the current game state after `PlayerReconnected`). A
`truncated` or `unavailable` replay specifically requires snapshot resync because
the control-event history is incomplete.

## Protocol v3 additions

Protocol v3 is a **purely additive** layer on top of the v2 wire contract documented above. The legacy shapes and
default reliable behavior remain unchanged; v3 adds optional `Authenticate` fields, v3-only messages, a
capability-negotiation handshake, classified JSON relay delivery with exact accountability, relay sequence
metadata, and optional `ice_servers` fields on `RoomJoined` /
`Reconnected` (the [ICE pre-gather](#ice-pre-gather), emitted only to v3 WebRTC-capable clients). A v2 client never
sends or receives a v3 message — the relay floor is the universal default and a v2 client observes byte-identical v2
behavior.

Canonical wire samples for this section:

- [v3 client messages](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/.llm/code-samples/protocol/v3-client-messages.jsonl)
- [v3 server messages](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/.llm/code-samples/protocol/v3-server-messages.jsonl)

See also the [Transport Fallback Contract](architecture/transport-fallback.md) (client-side state machine and the
relay-floor guarantee) and [Handoff and Topologies](architecture/handoff-and-topologies.md) (mesh / host / relay
topologies and the finalization handoff seam).

### Capability negotiation handshake

A v3-capable client advertises its capabilities by adding optional fields to the first `Authenticate`
message:

```json
{
  "type": "Authenticate",
  "data": {
    "app_id": "mb_app_abc123",
    "protocol_version": 3,
    "supported_transports": ["relay", "direct", "webrtc"],
    "supported_topologies": ["relay", "host", "mesh"],
    "requested_capabilities": ["room_operation_ids"]
  }
}
```

- `protocol_version` — the highest protocol version the client speaks. When absent, the endpoint default is used
  (`/v2/ws` ⇒ 2, `/v3/ws` ⇒ 3).
- `supported_transports` — data-path transports the client supports. Absent means the capability set is relay-only
  even when `/v3/ws` defaulted the protocol version to 3. Tokens: `relay`, `direct`, `webrtc`.
- `supported_topologies` — session topologies the client supports. Absent means the capability set is relay-only
  even when `/v3/ws` defaulted the protocol version to 3. Tokens: `relay`, `host`, `mesh`.
- `requested_capabilities` — additive wire extensions the client is prepared to use. Unknown and duplicate tokens
  are ignored. `room_operation_ids` is enabled only for negotiated v3 and only after the server echoes that token
  in `ProtocolInfo.capabilities`.

The server caps the negotiated version at its configured ceiling:
`negotiated = min(client_max, max_protocol_version)`. A client that advertises a higher version than the deployment
speaks is clamped **down** to `max_protocol_version`; the server never raises a client above its declared maximum.
If the result is below `min_protocol_version`, the app-ID handshake fails with
`UNSUPPORTED_PROTOCOL_VERSION`. An omitted field uses the endpoint default (`/v2/ws` defaults to v2; `/v3/ws`
defaults to v3). If the negotiated
version is below 3, the connection is **relay-only** regardless of the advertised `supported_transports` /
`supported_topologies`. If the negotiated version is 3 but `supported_transports` / `supported_topologies` are
absent, the connection is v3 relay-only. Defaults:
`min_protocol_version = 2`, `max_protocol_version = 3`.

The negotiated result is echoed back in an extended `ProtocolInfo` (the v2 fields plus v3-only additions):

```json
{
  "type": "ProtocolInfo",
  "data": {
    "capabilities": ["reconnection", "spectators", "authority", "room_operation_ids"],
    "game_data_formats": ["json", "message_pack"],
    "protocol_version": 3,
    "min_protocol_version": 2,
    "max_protocol_version": 3,
    "transports": ["websocket"],
    "max_outbound_message_size": 8388608
  }
}
```

These v3-only fields are omitted from the wire for a negotiated v2 connection, so the v2 `ProtocolInfo` shape stays
byte-identical. `ProtocolInfo.transports` names the server message lanes available to the connection; today it is
always `["websocket"]` for negotiated v3 and reserves a future advertisement point for another server relay lane. It
does not participate in the `Authenticate.supported_transports` data-path negotiation above.
`ProtocolInfo.max_outbound_message_size` is the deployment's exact aggregate
encoded WebSocket application-payload ceiling; clients should configure their
receive limit to at least this value. Browser and v2 clients obtain the same
value before connecting from `/v2/client-config` or `/v3/client-config`.
The reserved `room_operation_ids` token is absent unless explicitly requested and successfully negotiated;
SDK-compatibility capability tokens may appear alongside it.

**Endpoints.** `/v2/ws` and `/v3/ws` share the same handler, and their version-neutral
`/v2/client-config` and `/v3/client-config` companions return the current receive-limit metadata. `/v3/ws` only
changes the _default_ protocol version to 3 when the client omits `protocol_version`; an explicit
`protocol_version` in `Authenticate` always wins (then capped downward at the server maximum, or rejected below
its minimum). `/v2/ws` behavior is unchanged.

**Back-compat invariant.** A non-relay plan requires _every_ member of a room to be v3-capable and to support the
chosen topology and transport. A single v2 (or relay-only) member forces the whole room to the relay floor. Every
v3 member receives an explicit `relay`/`relay` `SessionPlan` with no peers, while v2 members receive no v3 message
and retain byte-identical behavior. WebRTC `Signal` relay remains transport-gated between same-room v3 WebRTC
peers; informational status, connection-level diagnostics, and shutdown advisories (`TransportStatus`,
`PeerTransportStatus`, `RelayStats`, `DeliveryReport`, `GoingAway`) keep their own feature gates. This is the
relay-floor guarantee: v2 and v3 clients interoperate, always.

### New v3 messages

These messages exist only on a negotiated v3 connection (and the correlated pair additionally requires the
`room_operation_ids` capability).

| Message | Direction | Purpose |
|---|---|---|
| `Signal` | client ⇄ server | Relay an opaque, matchbox-shaped WebRTC signal to/from a specific peer in the same room |
| `NewPeer` | server → client | Compatibility shape for an additive peer directive; current finalized membership changes use full `SessionPlan` refreshes |
| `SessionPlan` | server → client | Per-recipient authoritative session directive emitted at finalization and finalized membership refreshes |
| `TransportStatus` | client → server | Client reports its current data-path transport state (informational; drives metrics) |
| `PeerTransportStatus` | server → client | A same-room peer's reported transport state changed (fan-out of an accepted `TransportStatus`) |
| `RelayStats` | server → client | Optional per-connection relay delivery counters when `websocket.delivery_stats_interval_secs` is enabled |
| `DeliveryReport` | server → client | Cumulative per-class outcomes plus exact sequence ranges omitted for this connection |
| `GoingAway` | server → client | Shutdown-drain advisory sent before the server closes the socket with `4000 server_shutdown` |
| `RoomOperation` | client → server | Wrap one join, leave, reconnect, spectator-join, or spectator-leave command with a client UUID |
| `RoomOperationResult` | server → client | Echo that UUID on the command's terminal result |

#### Correlated room operations

After `ProtocolInfo.capabilities` contains `room_operation_ids`, send the legacy room command shape inside a
`RoomOperation` envelope:

```json
{
  "type": "RoomOperation",
  "data": {
    "operation_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
    "operation": {
      "type": "JoinRoom",
      "data": {
        "game_name": "my-game",
        "room_code": "ABC123",
        "player_name": "Player1"
      }
    }
  }
}
```

While the physical connection remains deliverable, the server attempts one terminal envelope for an accepted
operation and echoes the same UUID:

```json
{
  "type": "RoomOperationResult",
  "data": {
    "operation_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
    "result": { "type": "RoomJoinFailed", "data": { "reason": "Room is full", "error_code": "ROOM_FULL" } }
  }
}
```

The nested request types are `JoinRoom`, `LeaveRoom`, `Reconnect`, `JoinAsSpectator`, and `LeaveSpectator`.
Their success/failure result types retain the legacy payloads; `OperationFailed` covers a valid correlated command
that cannot produce its operation-specific success result. A malformed top-level frame may still receive an
uncorrelated top-level `Error`, because the server cannot safely trust an operation ID from undecodable input.

Generate a UUID that is unique among live and recently completed operations on the current physical WebSocket.
The scope continues across a successful `Reconnect` on that socket, while a new physical connection starts a new
scope. The server echoes IDs but does not deduplicate requests or make them idempotent. Autonomous lifecycle events
(for example, disconnect-driven `SpectatorLeft`) remain top-level and never impersonate an operation result.
As with every WebSocket response, a connection close, shutdown drain, or terminal slow-consumer eviction can prevent
the envelope from reaching the client; a missing result is not permission to reuse the UUID or assume the command did
not commit.

For Server 0.4/0.7 compatibility, do not send `RoomOperation` until the server advertises the capability. Continue
to send legacy top-level room commands otherwise; their response ambiguity remains unchanged.

#### Signal

`Signal` carries an **opaque** payload that the server never parses — it is forwarded verbatim to the target peer.
Every signal also carries the `generation` UUID from the sender's latest
`SessionPlan`. Recipients accept it only when that UUID equals their current
plan generation; delayed offers, answers, and candidates from older connection
attempts are discarded.
By convention the payload is matchbox-compatible: one of `{"Offer": "..."}`, `{"Answer": "..."}`, or
`{"IceCandidate": "..."}`. The server validates only the envelope (payload size cap, same room, negotiated WebRTC,
rate limit, v3 target); it never inspects the SDP or ICE strings. A payload whose serialized JSON exceeds
`security.max_signal_bytes` (default 16 KiB) is rejected with `SIGNAL_TOO_LARGE` and is not relayed. Dispatch to
the validated target is best effort: a signal whose recipient's queue has closed or been superseded by a newer
connection generation is dropped without notice to either side; a recipient that stalls until its slow-consumer
deadline abandons the signal along with the recipient's whole connection; validation failures are always reported
to the sender with their coded error.

Client → server (`to` names the target peer):

```json
{
  "type": "Signal",
  "data": { "to": "<player-uuid>", "generation": "<session-generation-uuid>", "signal": { "Offer": "<sdp>" } }
}
```

Server → client (`from` names the originating peer):

```json
{
  "type": "Signal",
  "data": { "from": "<player-uuid>", "generation": "<session-generation-uuid>", "signal": { "Answer": "<sdp>" } }
}
```

#### NewPeer

`NewPeer` is the v3 compatibility shape for an additive WebRTC peer directive.
`you_initiate` designates exactly one side of the pair as the offerer, avoiding
glare (see the glare rule below).

```json
{
  "type": "NewPeer",
  "data": { "peer_id": "<player-uuid>", "you_initiate": true }
}
```

The current server does not use `NewPeer` as the finalized-room membership
delta. A join or reconnect refreshes every v3 member with a complete
`SessionPlan`, which atomically removes stale peers as well as adding new ones.
Clients may retain `NewPeer` decoding for compatibility, but the authoritative
rule is always: the latest `SessionPlan` wins.

#### SessionPlan

`SessionPlan` is the **per-recipient authoritative** session directive first emitted at lobby finalization. It is
sent after the unchanged `GameStarting`, and only to v3-capable members. Every v3 member receives one: when no
upgrade rung fits, the server emits an explicit `topology: "relay"`, `transport: "relay"`, empty-peers reset.
Protocol-v2 members never observe it. Each recipient gets its own tailored `peers` list, `initiate` flags, and, for
WebRTC transports, ICE servers with freshly minted TURN credentials. It carries topology, transport, peers, ICE
servers, a required opaque `generation` UUID, and relay fallback. All plans in
one authoritative publication share that generation. For `host + direct`, it also carries
`direct_endpoint: { host, port }`, projected from the elected host's validated
self-declared `ConnectionInfo::Direct`. Other plans omit the field. Validation
proves only that the host/port is syntactically connectable (non-zero port;
non-empty bounded IP address or DNS hostname; no unspecified address), not that
the endpoint is reachable, so the relay fallback remains mandatory.

Endpoint visibility follows the existing room-member metadata boundary, not the
v3 plan boundary. `PlayerInfo.connection_info` can expose the self-declared
host/port to v2 or v3 players through `RoomJoined`, `PlayerJoined`, and
`Reconnected`, and to spectators through `SpectatorJoined.current_players`.
`GameStarting.peer_connections` repeats it to players. A finalized v3 room also
repeats the elected endpoint in `SessionPlan.direct_endpoint`. Clients should
therefore advertise only an address they intend room players and spectators to
observe and players to attempt. The server does not publish it through room
discovery, authenticate it as a network identity, or prove it reachable.

`SessionPlan` can also be **re-issued mid-session** (same message shape, same v3 gating). Two triggers:

- **Host failover.** When the host of a running `host`-topology session is found to be invalid — gone after a
  departure, self-healed on a late join that finds the stored host missing, or (after a reconnect that downgraded
  its negotiated capabilities) seated but no longer able to run the session — the server re-elects a host over the
  remaining members and sends every remaining v3 member a fresh tailored plan — same topology and transport, new
  `host`, fresh per-recipient ICE for WebRTC, or the replacement host's
  validated endpoint for Direct. Only members that negotiated v3 plus the
  session's sticky topology/transport pair are electable, and Direct candidates
  must also expose a usable endpoint (a member that cannot run the session is never named host of a
  session it
  cannot run); among those the rule is authority preferred, else earliest joiner, smaller-UUID tie-break. If no
  member qualifies, no plan is re-issued — the session is over and the relay floor carries the room. A host
  departure itself is still signaled by `PlayerLeft` (with the v3 terminal
  watermark; its v2 projection remains byte-for-byte frozen).
- **Late join / reconnect into a finalized room.** Every current v3 member receives a complete, tailored plan.
  The joining actor and incumbents therefore cross the same lifecycle boundary with one authoritative peer set;
  stale links are removed without depending on an additive delta. A room with no sticky non-relay entry derives
  an explicit `relay`/`relay` refresh from that absence.

The topology and transport of a session are **sticky for its lifetime**: the selection ladder runs once at
finalization and is never re-run mid-session, even when departures widen the capability intersection. A re-issued
plan only ever changes membership-derived fields (`peers`, `host`, `ice_servers`). Re-issued and late-join plan
peer lists contain only peers that can run the session — that negotiated v3 plus the session's topology **and**
transport. A joiner that cannot run the session is rejected at admission with
`ROOM_SESSION_INCOMPATIBLE` (see below), so a live non-relay session's membership stays uniformly capable; what
can remain is an incumbent that **reconnected** with downgraded capabilities (a reconnect owns its seat and is
never rejected). Such a member still receives its plan, but with an **empty** `peers`
list — it has no P2P peers and
participates via the relay floor (`host` stays as elected, informational) — and never appears in other members'
`peers`. Every publication observes this mixed-path shape (the `mixed_path_members_observed` counter plus a warn
log) so operators can quantify residual path-splitting. At finalization this filter is vacuous for non-relay
decisions, because an upgrade is selected only when
every member supports it. The client contract is uniform: **the latest
`SessionPlan` wins**. A changed generation is a connection-attempt barrier:
tear down and rebuild even retained WebRTC pairs with the new ICE credentials,
reject signals from every other generation, remove peers absent from the new
list, and initiate only where `peers[].initiate` is true.

The relay-floor reset has this exact shape:

```json
{
  "type": "SessionPlan",
  "data": {
    "generation": "<session-generation-uuid>",
    "topology": "relay",
    "transport": "relay",
    "peers": [],
    "fallback": "relay"
  }
}
```

```json
{
  "type": "SessionPlan",
  "data": {
    "generation": "<session-generation-uuid>",
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
- `ice_servers` — STUN/TURN servers for WebRTC; omitted (empty) for non-WebRTC plans, and also for a
  recipient that did not negotiate this session's topology and transport (its `peers` list is empty and its
  data path is the relay fallback, so it has nothing to gather against). Such a recipient may still hold ICE
  servers from an earlier `RoomJoined` pre-gather, which is decided before a session exists — see
  [ICE pre-gather](#ice-pre-gather).
- `fallback` — the universal fallback transport, always `relay` (the floor).

#### TransportStatus

`TransportStatus` lets a client report its current data-path transport state, so the server can distinguish
P2P-connected peers from relay-fallback peers (this drives metrics). It is **purely informational**: the relay
floor never closes regardless of what is reported. Metrics count the first report in a room/spectator membership
generation and real state transitions within that generation; duplicate `(transport, connected)`
reports in that generation do not move counters. Room joins, leaves, same-room rejoins, spectator-role changes,
and reconnects start fresh generations. The server accepts a report only from a negotiated v3 connection and
only when `transport` is in that connection's negotiated transport set. Reports from non-v3 clients or for
unnegotiated transports are ignored and do not update stored state or metrics.

```json
{
  "type": "TransportStatus",
  "data": { "transport": "webrtc", "connected": true }
}
```

An accepted report that records a real state change is additionally fanned out to the sender's current room
as [`PeerTransportStatus`](#peertransportstatus) (below), subject to the sender's pooled signaling budget:
an exhausted budget accepts the state change but suppresses the fan-out. Ignored and duplicate reports fan
out nothing.

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

- **Deduplicated per membership generation.** Accepted state and metrics move only for the first report after a
  room or spectator membership transition or reconnect, or for a later `(transport, connected)` transition in
  that generation. When the reporter has a seated room, that accepted change fans out. A same-generation duplicate
  is dropped and fans out nothing. Leaving and rejoining the same room still creates a fresh generation.
- **Sender excluded; room scoped.** Only the reporter's current room members hear it, never the reporter itself.
  A report from a client that is not seated in a room is still recorded but fans out nothing. This includes a
  spectator: spectator entry and leave reset deduplication, so the next accepted report is counted again, but
  reports made while spectating have no seated-room fan-out. The next seated join starts a fresh generation, so
  roomless or spectator state cannot suppress the first report seen by new peers.
- **v3-gated per recipient.** Like every v3-only message, it is delivered only to members that negotiated v3
  (the per-recipient v3 capability gate); a v2 member observes nothing.
  Deliberately, delivery is **not** gated on the recipient's own
  transport capabilities (unlike `NewPeer` / plan-peer pairing, which apply the full session predicate): this is
  informational status about a _peer's_ data path — useful even to a relay-only v3 member — not an instruction
  for the recipient to use that transport.
- **Purely informational**, like the report it relays: it never changes how the server relays `GameData`.

#### GoingAway

`GoingAway` is a best-effort v3 advisory emitted when the server begins a
graceful shutdown drain. It gives clients an absolute drain deadline before the
authoritative WebSocket close frame arrives:

```json
{
  "type": "GoingAway",
  "data": { "deadline_ms": 1700000000000, "retry_after_secs": 30 }
}
```

Fields:

- `deadline_ms` - Unix epoch millisecond time at or before which the server will
  close the socket with `4000 server_shutdown`.
- `retry_after_secs` - optional operator hint for reconnect backoff. It is
  omitted when the drain is configured for immediate close.

The close frame is authoritative. A v3 client may miss `GoingAway` if its socket
is already congested, and v2 clients never receive the advisory. In both cases,
`4000 server_shutdown` is the shutdown signal. A shutdown drain does not preserve
room/reconnect state on this single-instance server; clients should not attempt
to claim a stored reconnection token after observing `GoingAway` or `4000`.

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
- **Direct must be executable.** `host + direct` additionally requires at least
  one capable member with a validated `ProvideConnectionInfo` Direct endpoint.
  Host election considers only those endpoint-ready members: an endpoint-less
  authority is skipped in favor of the earliest eligible host. If none exists,
  the ladder continues to relay.
- **Relay floor always wins** when no rung fits. V3 members receive an explicit no-peer `relay`/`relay`
  `SessionPlan`; v2 members receive no plan and keep their frozen wire behavior.
- **`desired` is a ceiling, not an exact match.** A mesh-preferring room that cannot run mesh falls back to a host
  topology before collapsing to relay.

This ladder is the single source of truth in `src/server/session_policy.rs` (`UPGRADE_LADDER` + `RELAY_FLOOR`).

### Late-join decision table

A peer joining or reconnecting _after_ finalization is brought up to date from the room's running decision (the
ladder is _not_ re-run over the current members, so a session that finalized to the relay floor stays relay even if
every remaining member could now do better). The server refreshes the complete `SessionPlan` for every current v3
member; v2 members receive only their frozen lifecycle traffic:

| `room.lobby_state` | Stored (running) plan | Joiner receives | Existing members receive |
|---|---|---|---|
| not `Finalized` | any | nothing (initial pairing is owned by the finalize-time `SessionPlan`) | nothing |
| `Finalized` | none (relay floor / pre-v3 room) | v3: explicit `relay + relay` plan; v2: nothing | each v3 incumbent: explicit `relay + relay` plan; v2: nothing |
| `Finalized` | `mesh + webrtc` | capable: `SessionPlan` (every session-capable current peer, glare `initiate`, fresh ICE); incapable: rejected with `ROOM_SESSION_INCOMPATIBLE` | every v3 incumbent: complete refreshed `SessionPlan` |
| `Finalized` | `host + webrtc` | capable: `SessionPlan` (star view: client targets the stored host; a rejoining host targets all clients); incapable: rejected with `ROOM_SESSION_INCOMPATIBLE` | every v3 incumbent: complete refreshed `SessionPlan` |
| `Finalized` | `host + direct` | capable: `SessionPlan` (empty `ice_servers`); incapable: rejected with `ROOM_SESSION_INCOMPATIBLE` | every v3 incumbent: complete refreshed `SessionPlan` |

"Capable" means the joiner negotiated v3 plus the stored session's topology
and transport — exactly the membership shape selection required at finalize.

Every plan is v3-gated. A **new** joiner that cannot run the stored non-relay
session — one that did not negotiate v3 plus the session's topology **and**
transport (a relay-only client, a pure-v2 client, or one that negotiated the
`webrtc` transport but not the session's topology) — is rejected at admission
with `ROOM_SESSION_INCOMPATIBLE`: admitting it would silently split the data
path, because its WebSocket-relayed `GameData` reaches the room while P2P
traffic between capable members never reaches it. Rooms that finalized to the
relay floor store no sticky plan and stay open to everyone (the floor never
closes). The session predicate (v3 plus the session's topology **and**
transport) still filters peer lists on every publication, so members are never
told to pair with a peer that cannot run the session: an incumbent that
reconnects with downgraded capabilities keeps its seat but receives an
**empty** `peers` list (every pair with it sits outside the session contract;
a relay-only peer would additionally be rejected by `Signal`
validation); `fallback: "relay"` is its data path. Symmetrically, such a
member is omitted from capable members' `peers`, and every publication
observes it via the `mixed_path_members_observed` counter. A `host + direct`
(LAN) plan names its topology/transport and peers, carries the elected host's
validated `direct_endpoint`, and carries no ICE because there is no WebRTC
signaling to broker. The endpoint originates from `ProvideConnectionInfo`,
remains self-declared, and is repeated in every authoritative plan so
finalization, late joins, reconnects, and failover do not depend on separately
correlating legacy `GameStarting` metadata. A Direct host that reconnects
without a usable endpoint is invalid and triggers endpoint-aware re-election;
after failover, an ex-host reconnects as a client of the new host. Endpoint
updates do not change a finalized room's sticky topology/transport; they are
observed on the next membership refresh, while an already-relay room is never
upgraded mid-session.

### Glare / offerer rule

For any WebRTC pair, exactly one side must send the offer. Which side is encoded in the `initiate` flag on a
`SessionPlan` peer (and in `you_initiate` if a compatibility `NewPeer` is received):

- **Mesh:** the peer whose `player_id` is the **lesser** of the two UUIDs sends the offer (a deterministic,
  stateless, antisymmetric rule).
- **Host:** the direction is fixed regardless of UUID order — each **client initiates to the host**, and the host
  answers every client. Clients never signal each other in a star topology.

### ICE and TURN credentials

Every WebRTC `SessionPlan` for a recipient that can pair in the session carries an `ice_servers` list (a
recipient that did not negotiate the session's topology and transport receives none — see `peers` above):

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
|<--- SessionPlan(mesh,webrtc,generation=G) - SessionPlan(generation=G) ->| per-recipient: A.peers=[B initiate=true],
|                                  |                       |                  B.peers=[A initiate=false]
|  Signal{to:B, generation:G, Offer} ->|-- Signal{from:A, generation:G} ->|
|<- Signal{from:B, generation:G} --|<- Signal{to:A, generation:G, Answer}|
|  Signal{to:B, generation:G, ICE} ->|-- Signal{from:A, generation:G} ->| (ICE trickle, both directions)
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
|  SessionPlan(host,webrtc,host=H,generation=G)             |                   |
|<--- (C1.peers=[H initiate=true]) |-- SessionPlan ------->|-- SessionPlan --->|  (C2.peers=[H initiate=true];
|                                  |   H.peers=[C1,C2 initiate=false]          |   each client offers to H)
|  Signal{to:H,generation:G,Offer}->|-- Signal{from:C1,generation:G} ->|        |
|<- Signal{from:H,generation:G,Answer} <- Signal{to:C1,generation:G} --|        |
|                                  |<- Signal{from:C2,generation:G} <- Signal{to:H,generation:G,Offer}
|  == C1⇄H channel open ==         |    == C2⇄H channel open ==                |
|  (C1 and C2 never signal each other in a star topology)                     |
|  (on failure: each client falls back to GameData over the relay floor)      |
```

## Protocol v3 delivery reliability

Protocol v3 also adds classified JSON delivery and exact relay accountability
(there is no separate v4). A deployment can clamp
`protocol.max_protocol_version` back to `2` to disable this surface and retain
the pure v2 reliable FIFO contract.

### Sequenced relay (`seq`)

Every relayed `GameData` (JSON) and v3 binary metadata envelope delivered to a
v3 recipient carries a server-stamped `seq`. The server allocates
the sequence before applying per-recipient delivery policy. It starts at `1`
within a sender's room incarnation and increases by one for every accepted
message from that sender. Text and binary use the same stream. Pre-v3 recipients
receive byte-identical frames with no `seq` key.

Recipient rules:

- Within one `(sender, epoch)`, delivered sequence numbers are strictly
  increasing but need not be contiguous. A continuing connection may accept a
  hole only when the union of causally prior `DeliveryReport` ranges covers
  every missing sequence for the same sender and epoch. Aggregate counters, an
  error frame, or a report that arrives after the successor are not
  authorization.
- A sender first seen in `RoomJoined.current_players` or
  `SpectatorJoined.current_players` may already be active. The paired
  `PlayerInfo.(epoch, seq)` is the exact recipient-visible baseline: `seq` is
  the last sequence already outside this recipient's delivery obligation, so
  delivery or exact gap coverage starts at `seq + 1`.
- Peer lifecycle control has priority over data in the active room generation.
  `PlayerLeft`, a later `PlayerJoined`, or `PlayerReconnected` can therefore
  arrive before old-epoch data that was already queued. Retain and advance the
  old epoch's accounting cursor while it drains, but do not apply that stale
  incarnation's payload after its lifecycle change. A future data/report epoch
  is valid only if a lifecycle message announced that exact epoch. Once data
  advances to an announced newer epoch, reject any later frame from an older
  epoch. Multiple announcements may be outstanding; application data becomes
  current only at the newest announced incarnation.
- A v3 `PlayerLeft` terminal watermark bounds that stale state. Retain the
  departed sender until delivered frames plus exact prior gaps contiguously
  cover `final_seq` in the named `epoch`; then retire its cursor, pending gaps,
  and tombstone. Reject delivery or gaps beyond that terminal. A zero terminal
  sequence retires immediately. The v2 `PlayerLeft` shape has no watermark.
- On this recipient's own `RoomLeft` / `RoomJoined` or `SpectatorLeft` /
  `SpectatorJoined` transition, discard every prior-room sender baseline and
  outstanding gap range. These recipient transitions are ordering barriers: no
  old-room data may appear after the new room or spectator snapshot. Cumulative
  counters retain their physical-connection lifetime.
- After this recipient reconnects, `Reconnected.sender_watermarks` is the
  authoritative baseline for every current sender and must exactly match the
  paired baseline in `current_players`. Discard pre-disconnect expectations.
  High-rate data is not replayed, so resynchronize application state before
  applying new deltas.
- This recipient's own disconnect — loud or graceful — terminates the
  observable stream. Messages abandoned during close do not need gap records
  because no later data can appear on that physical connection: once a payload
  is abandoned while a socket write owns it, nothing queued behind it is
  written. A replacement connection starts a new accounting lifetime.

Any same-epoch hole on a continuing connection without complete coverage by
prior exact reports is a protocol violation. Process control messages in
arrival order before applying later game data so reports and lifecycle state
are visible when a successor arrives.

### Incarnation epoch (`epoch`)

Alongside `seq`, every relayed `GameData` / binary envelope to a v3 recipient
also carries an `epoch`: a **monotonic per-sender** counter that increments once
per **incarnation** of that sender's membership — its first-ever incarnation is
`epoch` 1, and each join-after-leave or reconnect increments it. The server
tracks it per sender connection and never resets it on a room switch, so a
sender's first frame in a given room may begin at `epoch` 2 or higher if that
sender was previously in another room -- do not assume a room's first observed
epoch is 1. In data-lane order, the pair `(epoch, seq)` advances
lexicographically per `(sender, room)`:

- `(1, 1), (1, 2), (1, 3)` — the sender's first incarnation, and then
- `(2, 1), (2, 2), …` — after the sender left+rejoined or reconnected.

Priority lifecycle control may be observed before the tail of the earlier epoch,
so the complete WebSocket frame stream is not necessarily lexicographically
ordered across a lifecycle message. The stamped data still makes a `seq` reset
self-describing, while the lifecycle announcement determines which incarnation
is safe for application use. Each v3 `PlayerInfo` on room lifecycle and
snapshot messages carries paired `epoch` and `seq` values. That `seq` is the
last relay sequence already outside this recipient's obligation in the named
epoch; it is `0` for a new incarnation. The same pair appears on
`RoomJoined.current_players[]`, `SpectatorJoined.current_players[]`,
`PlayerJoined.player`, and the `Reconnected` member snapshot.
`PlayerReconnected.epoch` announces a new incarnation with an implicit baseline
of zero. `Reconnected.sender_watermarks` repeats the pair after recipient
absence and must match the member snapshot exactly. Both `PlayerInfo` fields and
`sender_watermarks` are stripped for pre-v3 (v2) recipients (their bytes stay
byte-identical), so absence and presence are part of the frozen wire contract.

The `epoch` value is only meaningful **relatively**: baseline each sender from
the epoch first observed in a snapshot or frame and compare later values against
it. Do not assume a newly observed sender starts at epoch 1. The server tracks a
monotonic epoch per sender connection, so a sender that reached your room after
being in another room on the same connection may first appear at epoch 2 or
higher.

### DeliveryReport

`DeliveryReport` is the authoritative v3 accounting message. Its counters are
cumulative for one physical recipient connection; its optional `gaps` list
names exact, newly omitted inclusive sequence ranges:

```json
{
  "type": "DeliveryReport",
  "data": {
    "per_class": {
      "reliable": {
        "delivered": 10,
        "abandoned": 0,
        "unsupported_format": 0
      },
      "latest": {
        "delivered": 20,
        "superseded": 2,
        "dropped_full": 0,
        "abandoned": 0,
        "unsupported_format": 0
      },
      "volatile": {
        "delivered": 30,
        "dropped": 0,
        "abandoned": 0,
        "unsupported_format": 0
      }
    },
    "gaps": [
      {
        "from_player": "00000000-0000-0000-0000-00000000000a",
        "epoch": 1,
        "from_seq": 41,
        "to_seq": 42,
        "reason": "latest_superseded"
      }
    ]
  }
}
```

`from_seq` and `to_seq` are inclusive. Gap reasons are
`latest_superseded`, `latest_dropped_full`, `volatile_dropped`, and
`unsupported_format`. `gaps` is omitted when empty and otherwise contains from
1 through 256 ranges. If more non-mergeable ranges accumulate, the server rolls
them into additional priority reports. A client must retain prior ranges and
authorize a hole only when their non-overlapping union covers every missing
sequence; one report or range need not cover the entire hole alone.

**No reason has a privileged shape.** A range may span any number of sequences,
one report may carry several ranges of the same reason, and reasons may be mixed
in one report. A client must not require a single-sequence range or a single gap
for any reason, `unsupported_format` included: the exactness a client can rely on
is that the reported ranges never overlap and that each loss counter advances by
exactly the number of sequences the same report attributes to it.

Gap-bearing reports are event-driven even when
`websocket.delivery_stats_interval_secs` is zero. Queue-policy reports for
`latest` / `volatile` travel on the active generation's strict-priority control
lane and are committed atomically with the omission. Unsupported-format ranges
are coalesced by the socket writer and reach the recipient before any later
data, when the supplemental advisory is emitted, carried by an already-queued
report, or at most one second after the first omission if nothing else is
written; a closing connection flushes them too. Consecutive omissions from one
sender and delivery class therefore arrive as one range rather than one report
per message. The supplemental
`UNSUPPORTED_GAME_DATA_FORMAT` prose `Error` is rate-limited to at most one per
sender per second for each recipient; a later notice reports how many similar
advisories were suppressed. Clients MUST use the exact reports—not advisory
errors—to account sequence gaps. If either an exact report or a chosen
supplemental error write fails, the stream disconnects.
Counter-only snapshots may be periodic. If exact reporting cannot be preserved,
the server closes the connection with `4002 slow_consumer` before exposing
later data.

Each report freezes its cumulative counter frontier at the ranges it contains,
so rollover reports remain monotonic and causal. For each lossy reason, the
counter increase since the preceding report equals the number of sequence units
with that reason in the current report's `gaps`. The per-class counters obey the conservation equations in
[Delivery semantics](#delivery-semantics). They are useful for totals and
diagnostics, but only the union of prior exact ranges identifies and authorizes
which sender, epoch, and sequences were omitted. Do not infer a gap from counter
deltas.

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

These frozen aggregate counters are cumulative for the life of the connection
and remain useful operational diagnostics. `backpressure_events` rising means
reliable traffic had to wait for this recipient, so it should drain faster or
expect eviction. `dropped_for_you` cannot identify a sender, epoch, range, or
delivery class. It never authorizes a sequence gap; use a prior exact
`DeliveryReport` range union for that.

## Next Steps

- [Install Signal Fish Server](getting-started.md) - installation options
- [Platform Integration Guide](guides/platform-integration.md) - which WebRTC stack to use per platform
- [Features](features.md) - Complete feature overview
