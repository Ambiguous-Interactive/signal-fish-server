# Features

Complete overview of Signal Fish Server capabilities.

## Room Management

### Automatic Room Codes

Rooms are identified by auto-generated 6-character codes (configurable length).

Create a room by joining without a room code:

```json
{
  "type": "JoinRoom",
  "data": {
    "game_name": "my-game",
    "player_name": "Player1",
    "max_players": 8
  }
}

```

Response:

```json

{
  "type": "RoomJoined",
  "data": {
    "room_code": "ABC123",
    "room_id": "uuid-string",
    "player_id": "your-player-id",
    "game_name": "my-game",
    "max_players": 8,
    "supports_authority": true,
    "current_players": [
      {
        "id": "your-player-id",
        "name": "Player1",
        "is_authority": false,
        "is_ready": false,
        "connected_at": "2025-01-15T10:30:00Z"
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

Players join using the room code:

```json

{
  "type": "JoinRoom",
  "data": {
    "game_name": "my-game",
    "room_code": "ABC123",
    "player_name": "Player2"
  }
}

```

### Room Limits

Configure per-game room limits:

```json

{
  "server": {
    "max_rooms_per_game": 1000
  }
}

```

When auth is enabled, per-app limits apply:

```json

{
  "security": {
    "authorized_apps": [
      {
        "app_id": "my-game",
        "max_rooms": 100,
        "max_players_per_room": 16
      }
    ]
  }
}

```

## Lobby State Machine

Rooms transition through three states based on player ready status:

### Waiting

Initial state. The room is empty, waiting for its first player. No ready-state
toggles happen in this state.

### Lobby

Players are present and coordinating readiness. `max_players` is a ceiling, not
a required count — the room need not be full.

### Finalized

A member sent `StartGame` while every current player was ready, and the game is
starting.

### State Transitions

```text

Waiting --> Lobby (first player joins)
Lobby --> Finalized (StartGame, every current player ready)
Finalized --> [*] (game started, room cleanup)

```

Clients are notified of state changes:

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

## Player Ready State

Players toggle their own ready state by sending `PlayerReady`:

```json

{
  "type": "PlayerReady"
}

```

Each `PlayerReady` send flips that player's ready/unready status and broadcasts
`LobbyStateChanged` (with `all_ready` set once every current player is ready).
Readiness no longer starts the game on its own: once every current player is
ready, a member sends `StartGame` to finalize the lobby, and the server then
sends `GameStarting`. The starter must be the room's authority if it has one,
else any member; `max_players` is a ceiling, so the room need not be full.

## Authority Management

Players can request game authority (e.g., for server-authoritative gameplay):

```json

{
  "type": "AuthorityRequest",
  "data": {
    "become_authority": true
  }
}

```

When granted, all players are notified:

```json

{
  "type": "AuthorityChanged",
  "data": {
    "authority_player": "player-id",
    "you_are_authority": false
  }
}

```

Only one player can hold authority at a time.

## Spectator Mode

Join rooms as a spectator without participating in gameplay:

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

Spectators:

- Don't count toward max_players
- Can't send `PlayerReady` (cannot toggle readiness)
- Receive a room snapshot on join, not live game-data broadcasts
- Don't participate in authority decisions

## Reconnection

Token-based reconnection with event replay.

### Disconnect and Token Generation

When a player disconnects, the server generates a reconnection token
bound to their player ID and room ID. The server buffers room events
during the disconnection window so they can be replayed on reconnect.

### Reconnecting

If the connection is lost, reconnect using stored credentials:

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

### Event Replay

On successful reconnection, the server sends a `Reconnected` message with the current room state and replays any
events that occurred during the disconnection window.

### Configuration

```json

{
  "server": {
    "enable_reconnection": true,
    "reconnection_window": 300,
    "event_buffer_size": 100
  }
}

```

## Peer-to-Peer Transports (Protocol v3)

Protocol v3 is purely additive over v2: every v2 client keeps working unchanged
on the server relay floor, while v3-capable clients can negotiate a peer-to-peer
data path. A non-relay session is only chosen when _every_ room member is
v3-capable and supports the selected topology/transport; otherwise the whole
room stays on the relay floor.

### Transports and Topologies

The negotiated data-path transport is one of three wire tokens:

- `relay` - server WebSocket fan-out (the mandatory floor, always available)
- `direct` - LAN / routable host IP:port path
- `webrtc` - peer-to-peer WebRTC data channel

The session topology is likewise one of:

- `relay` - the v2 server-relay hub
- `host` - a star around a single authoritative host
- `mesh` - full peer-to-peer mesh

The preferred topology is configured per deployment, with optional per-game
overrides, and each transport is independently gated:

```json

{
  "session": {
    "default_topology": "relay",
    "game_topology_mappings": { "FastFPS": "mesh" },
    "enable_webrtc": true,
    "enable_direct": true,
    "enable_ice_pregather": true
  }
}

```

### WebRTC Signaling

When a WebRTC plan is in play, peers exchange connection setup over a targeted
`Signal` relay. A client sends `Signal` with a `to` (peer id) and an opaque
`signal` payload; the server forwards it verbatim to that one peer and never
parses it (by convention the payload is matchbox-compatible: `Offer`, `Answer`,
or `IceCandidate`). Signal relay is gated by `session.enable_webrtc`. Signal
valid-signal dispatch is rate limited, while rejected signals receive detailed
errors only up to a separate budget before generic rate-limit errors replace
them (see [Rate Limiting](#rate-limiting)).

### Session Plans

At lobby finalization, each v3-capable member of a non-relay room receives a
per-recipient `SessionPlan` alongside the unchanged `GameStarting`. The plan
carries the chosen `topology` and `transport`, the elected `host` (for `host`
topology), a validated `direct_endpoint` when the selected transport is
`direct`, the `peers` this recipient should connect to (each with an `initiate`
flag so exactly one side of every pair sends the offer), the `ice_servers` to
gather against, and a universal `fallback` transport that is always `relay`.
`host + direct` is eligible only when at least one capable host has supplied a
syntactically usable endpoint; the endpoint remains self-declared, so clients
must retain the relay fallback.

### ICE Pre-Gather

When `session.enable_ice_pregather` is enabled, the composed ICE server list is
advertised on `RoomJoined` and `Reconnected` so WebRTC-capable clients can
pre-gather candidates during the lobby wait. The `SessionPlan` ICE list always
supersedes it, and the gate never fires for v2 clients, relay-only clients,
relay-desired games, or finalized rooms.

### TURN and STUN

TURN is _self-hosted only_. When `turn.enabled` is set, the server mints
short-lived coturn REST credentials (from the operator's
`turn.static_auth_secret`, the coturn `--static-auth-secret`) for an
operator-run TURN server and advertises them in the ICE list; the secret never
leaves the server. STUN URLs are advertised regardless. There is no managed,
built-in, or automatic TURN provisioning.

See [Handoff & Topologies](architecture/handoff-and-topologies.md),
[Transport Fallback](architecture/transport-fallback.md), and
[TURN Deployment](deployment-turn.md) for the full design.

## Delivery Classes and Exact Accountability (Protocol v3)

Negotiated-v3 JSON `GameData` supports three per-recipient queue policies:

- `reliable` (or omitted) preserves every message or closes a slow recipient
  loudly.
- `latest` requires a `u32` key and keeps the newest queued value for each
  `(sender, room, key)` stream.
- `volatile` delivers opportunistically and never waits for data-queue space.

Class policy is not a room-wide P2P upgrade. In a mixed room, the same
classified send may use latest/volatile policy for a v3 recipient while a v2
recipient gets reliable FIFO with all v3 metadata stripped. Raw WebSocket
binary game data is always reliable.

Every intentional v3 omission is named by a causally prior `DeliveryReport`.
One report carries at most 256 exact inclusive ranges; additional ranges roll
into further priority reports. A client accepts a continuing-stream sequence
hole only when the non-overlapping union of all prior ranges covers every
missing sequence for the same sender and epoch. Cumulative counters and
`RelayStats` are diagnostics, not gap authorization.

Within an active recipient generation, report and peer-lifecycle control has
priority over data. Lifecycle control can therefore overtake queued old-epoch
payloads: clients keep accounting for that tail but suppress it from application
state after the lifecycle change. The recipient's own room/spectator transitions
are generation barriers. Accepted sockets use a bounded TCP send-buffer request
so bytes already handed to the kernel cannot indefinitely bury later control.
See [Delivery semantics](protocol.md#delivery-semantics)
for conservation, close behavior, and reconnect rules.

## Message Batching

Batch outbound messages for improved throughput.

```json

{
  "websocket": {
    "enable_batching": true,
    "batch_size": 10,
    "batch_interval_ms": 16
  }
}

```

- `batch_size` - Max messages per batch
- `batch_interval_ms` - Max time to wait before flushing

Batching is transparent to clients.

## Rate Limiting

In-memory rate limiting for room creation, join attempts, validated WebRTC
Signal dispatch attempts, and detailed rejected-signal error responses.

```json

{
  "rate_limit": {
    "max_room_creations": 5,
    "time_window": 60,
    "max_join_attempts": 20,
    "max_signals": 600,
    "max_signal_errors": 60
  }
}

```

- `max_room_creations` - Max room creations per player per time window
- `time_window` - Window duration in seconds
- `max_join_attempts` - Max join attempts per player per window
- `max_signals` - Max validated WebRTC Signal dispatch attempts per player per window
- `max_signal_errors` - Detailed rejected-signal errors per player per window before generic rate-limit errors

When auth is enabled, per-app rate limits apply:

```json

{
  "security": {
    "authorized_apps": [
      {
        "app_id": "my-game",
        "rate_limit_per_minute": 60
      }
    ]
  }
}

```

## Metrics

### JSON Metrics

```bash

curl http://localhost:3536/metrics

```

Returns a camelCase, nested JSON document:

```json

{
  "timeRange": "1h",
  "timestamp": "2026-06-13T12:00:00Z",
  "activeRooms": 42,
  "roomsByGame": { "my-game": 12 },
  "playerPercentiles": { "p50": 4, "p95": 16 },
  "gamePercentiles": { "p50": 1, "p95": 5 },
  "dashboardCache": {
    "fetchedAt": "2026-06-13T11:59:30Z",
    "ageSeconds": 30,
    "stale": false,
    "lastError": null,
    "refreshIntervalSeconds": 60,
    "history": []
  },
  "serverMetrics": {
    "connections": { "total": 1024, "active": 156, "disconnections": 868 },
    "rooms": { "created": 1024, "joined": 4096, "deleted": 982 },
    "performance": { "queries": 0, "room_creation_latency": 0, "room_join_latency": 0, "query_latency": 0 },
    "errors": { "internal": 0, "websocket": 0, "total": 0 },
    "rateLimiting": {
      "total_rejections": 0,
      "auth_rejections": 0,
      "room_creation_rejections": 0,
      "join_attempt_rejections": 0,
      "signal_rejections": 0,
      "signal_error_rejections": 0
    }
  }
}

```

`total_rejections` is the sum of the five category counters. Room creation
consumes both a room-creation slot and a join-attempt slot; a rejection is
attributed to the budget that made the decision.

Room and connection counters live under `serverMetrics.connections`
(`total` / `active` / `disconnections`) and `serverMetrics.rooms`
(`created` / `joined` / `deleted`). The endpoint accepts two optional query
parameters: `?time_range=<range>` selects the dashboard window (echoed back as
`timeRange`, default `1h`), and `?includeSnapshot=true` adds a `metricsSnapshot`
object with the raw counter snapshot. Delivery-class counters are at
`metricsSnapshot.connections.delivery_by_class.{reliable,latest,volatile}`.
Each class exposes `attempted`, `delivered`, `superseded`, `dropped_full`,
`dropped`, `abandoned`, and `unsupported_format`; impossible outcomes for a
class remain zero. At quiescence, `attempted` equals the sum of that class's
terminal outcomes.

### Prometheus Metrics

```bash

curl http://localhost:3536/metrics/prom

```

Returns Prometheus text format for scraping. Per-class delivery outcomes use:

```text
signal_fish_websocket_delivery_class_outcomes_total{class="latest",outcome="superseded"}
```

`class` is `reliable`, `latest`, or `volatile`; `outcome` is `attempted`,
`delivered`, `superseded`, `dropped_full`, `dropped`, `abandoned`, or
`unsupported_format`. Apply the same quiescent conservation rule as the JSON
snapshot.

### Metrics Authentication

Protect metrics endpoints:

```json

{
  "security": {
    "require_metrics_auth": true,
    "metrics_auth_token": "your-secret-token"
  }
}

```

`metrics_auth_token` must be set — it defaults to `null`, and when
`require_metrics_auth` is `true` but no token is configured, every request is
rejected with HTTP 401. The server compares the bearer token against
`metrics_auth_token` as a single opaque string (constant-time), not against an
`app_id:app_secret` pair.

Access with:

```bash

curl -H "Authorization: Bearer your-secret-token" \
  http://localhost:3536/metrics

```

## Authentication

Optional app-ID allowlisting with per-app room, player, and request limits. The
current client handshake validates `app_id` only; see the authentication guide
for the exact trust boundary.

```json

{
  "security": {
    "require_websocket_auth": true,
    "authorized_apps": [
      {
        "app_id": "my-game",
        "app_secret": "RESERVED_NOT_USED_BY_CLIENTS",
        "max_rooms": 100,
        "max_players_per_room": 16,
        "rate_limit_per_minute": 60
      }
    ]
  }
}

```

See [Authentication](authentication.md) for full details.

## MessagePack Support

Enable MessagePack encoding for game data:

```json

{
  "protocol": {
    "enable_message_pack_game_data": true
  }
}

```

Game data messages can be sent in MessagePack format for reduced bandwidth.

Negotiated-v3 binary frames use a MessagePack metadata envelope containing
`from_player`, opaque `payload`, mandatory `seq` / `epoch`, and an `encoding`
tag for the payload bytes. The server advertises its accepted formats in
`ProtocolInfo.game_data_formats`; requesting an unsupported format yields
`UNSUPPORTED_GAME_DATA_FORMAT` and falls back to JSON. V2 binary wire shapes
remain byte-identical and unstamped.

If an opaque binary payload cannot be converted for a differently encoded
recipient, protocol v3 emits an exact `DeliveryReport` for every omitted
sequence. Supplemental `UNSUPPORTED_GAME_DATA_FORMAT` prose errors are limited
to one per sender per second so a malformed stream cannot amplify advisory
traffic; clients use the reports, not those advisory errors, for gap accounting.

## CORS Support

Configure allowed origins:

```json

{
  "security": {
    "cors_origins": "https://yourgame.com"
  }
}

```

Multiple origins (comma-separated):

```json

{
  "security": {
    "cors_origins": "https://game.com,https://beta.game.com"
  }
}

```

Allow all (development only):

```json

{
  "security": {
    "cors_origins": "*"
  }
}

```

## Connection Limits

Limit concurrent connections per IP:

```json

{
  "security": {
    "max_connections_per_ip": 24
  }
}

```

## Message Size Limits

Limit maximum WebSocket message size:

```json

{
  "security": {
    "max_message_size": 65536
  }
}

```

Messages exceeding this size are rejected.

## Room Cleanup

Automatic cleanup of empty and inactive rooms:

```json

{
  "server": {
    "room_cleanup_interval": 60,
    "empty_room_timeout": 300,
    "inactive_room_timeout": 3600
  }
}

```

- `room_cleanup_interval` - Seconds between cleanup sweeps
- `empty_room_timeout` - Seconds before empty room removal
- `inactive_room_timeout` - Seconds before inactive room removal

## Ping/Pong

Keep-alive mechanism to detect dead connections:

```json

{
  "server": {
    "ping_timeout": 30
  }
}

```

Clients should still send periodic application `Ping` messages. The server also
probes the underlying WebSocket transport independently:

```json

{
  "websocket": {
    "server_ping_interval_secs": 10,
    "pong_timeout_secs": 5,
    "socket_send_buffer_bytes": 65536
  }
}

```

After an otherwise-idle interval, the RFC 6455 Ping bypasses application
queues; the bounded socket send buffer limits data already handed to TCP ahead
of it. Decoded inbound non-Pong traffic skips an unnecessary probe. Completed
outbound application writes supersede its Pong deadline, but the Ping is still
sent so compliant WebSocket stacks on read-only clients answer and refresh
inbound activity automatically. Outbound queue/sojourn deadlines still close a
send path that stops progressing with `4002 slow_consumer`; a silent
authoritative missing matching Pong closes the socket with `4003
activity_timeout`. Set `server_ping_interval_secs` to `0` to disable probes.
Separately, the activity reaper disconnects clients that send no traffic for
longer than `server.ping_timeout`; keep it nonzero when continuous outbound
traffic must not mask a client-to-server half-partition. Size it above
`server_ping_interval_secs` plus the worst measured Ping queue/write delay and
operational jitter. Otherwise the reaper can win before an automatic Pong
arrives; the defaults provide nominal headroom, not a guarantee under arbitrary
socket backlog.

Prometheus reports missed matching-Pong deadlines with
`signal_fish_websocket_ping_timeouts_total`. Successful probe latency uses the
`signal_fish_websocket_ping_rtt_*` family, including
`signal_fish_websocket_ping_rtt_samples_total` and millisecond summary gauges.
Activity-based decisions use
`signal_fish_websocket_ping_probes_skipped_activity_total` and
`signal_fish_websocket_ping_probes_cancelled_activity_total`.

## Idle Timeout

A separate, socket-level idle cap closes authenticated connections that go
quiet, independent of `server.ping_timeout`:

```json

{
  "websocket": {
    "idle_timeout_secs": 300
  }
}

```

`idle_timeout_secs` (default 300; `0` disables) closes an authenticated
connection that produces no inbound WebSocket frame of any kind (including
Ping/Pong) for that long, surfacing the
[`CONNECTION_IDLE_TIMEOUT`](reference/error-codes.md) error and disconnecting
through the normal path (so the reconnection grace period still applies). This
is an exclusive boundary: the server must observe a frame strictly before the
deadline; input ready but unobserved at the boundary does not reset the window.
This is distinct from `server.ping_timeout` (the server-side activity reaper,
default 30s): clients that heartbeat as `server.ping_timeout` already requires
are never affected by the 300s default, which only reclaims zombie sockets.

## Structured Logging

JSON-formatted structured logs for production observability:

```json

{
  "logging": {
    "enable_file_logging": true,
    "dir": "logs",
    "filename": "server.log",
    "rotation": "daily",
    "format": "json"
  }
}

```

## Zero External Dependencies

Everything runs in-memory:

- No database required
- No message broker
- No cloud services
- No external runtime dependencies

Perfect for:

- Local development
- LAN games
- Self-hosted deployments
- Embedded systems

## Next Steps

- [Getting Started](getting-started.md) - Quick start guide
- [Protocol Reference](protocol.md) - Complete message documentation
- [Configuration](configuration.md) - Full configuration options
