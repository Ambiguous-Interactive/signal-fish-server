# Reconnection

WebSocket connections are fragile. Players lose connectivity when they
switch networks, when their device sleeps, when they walk through a bad
Wi-Fi zone, or when a mobile carrier briefly drops a data session.
Without reconnection support, any of these events would force a player
to re-authenticate, rejoin the room (which may already be full), and
lose every event that happened while they were away.

Signal Fish Server solves this with token-based reconnection and event
replay.

## How Reconnection Works

The reconnection flow has two phases: **preparation** (during the
initial join) and **recovery** (after a disconnect).

### Phase 1: Save Your Identifiers

When a player joins a room, store the `player_id` and `room_id` from
the `RoomJoined` response. These identifiers are needed if you need to
reconnect later.

```json
{
  "type": "RoomJoined",
  "data": {
    "room_id": "550e8400-e29b-41d4-a716-446655440000",
    "room_code": "HK7T3W",
    "player_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "game_name": "my-game",
    "max_players": 4,
    "supports_authority": true,
    "current_players": [
      {
        "id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
        "name": "Alice",
        "is_authority": true,
        "is_ready": false,
        "connected_at": "2025-01-15T10:30:00Z"
      }
    ],
    "is_authority": true,
    "lobby_state": "waiting",
    "ready_players": [],
    "relay_type": "matchbox",
    "current_spectators": []
  }
}
```

The reconnection token (a server-generated UUID bound to your player ID
and room ID) arrives ON THE WIRE at join time: v3+ clients read it from
`RoomJoined.reconnection_token` and store it before anything can go
wrong. It is the `auth_token` value the client supplies when sending a
`Reconnect` message — treat it like a session credential. The string is
stable from join through a later disconnect, but it only becomes
claimable for `server.reconnection_window` seconds counted from the
disconnect (holding it early does not widen the window), and it rotates
on every join and every successful reconnect
(`Reconnected.reconnection_token` carries the replacement). Discarded on
a voluntary `LeaveRoom` — leaving cleanly is not a disconnect. It is also not
armed for a server shutdown drain (`GoingAway` / close code `4000
server_shutdown`); the instance is going away, so clients should join or create a
fresh room on another healthy instance.

### Phase 2: Reconnect After a Disconnect

When the WebSocket connection drops, open a new WebSocket to the server
and send a `Reconnect` message with the stored credentials:

```json
{
  "type": "Reconnect",
  "data": {
    "player_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "room_id": "550e8400-e29b-41d4-a716-446655440000",
    "auth_token": "f47ac10b-58cc-4372-a567-0e02b2c3d479"
  }
}
```

If the token is valid and the reconnection window has not expired, the
server restores you to the room and sends a `Reconnected` message. This
message contains the current room state **and** the room-uniform
**control events** that were broadcast while you were away (see
[Event Buffer](#event-buffer) for exactly which events are replayed and
the completeness contract):

```json
{
  "type": "Reconnected",
  "data": {
    "room_id": "550e8400-e29b-41d4-a716-446655440000",
    "room_code": "HK7T3W",
    "player_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "game_name": "my-game",
    "max_players": 4,
    "supports_authority": true,
    "current_players": [
      {
        "id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
        "name": "Alice",
        "is_authority": true,
        "is_ready": false,
        "connected_at": "2025-01-15T10:30:00Z",
        "epoch": 1
      },
      {
        "id": "b2c3d4e5-f6a7-8901-bcde-f12345678901",
        "name": "Bob",
        "is_authority": false,
        "is_ready": true,
        "connected_at": "2025-01-15T10:31:00Z",
        "epoch": 2
      }
    ],
    "is_authority": true,
    "lobby_state": "lobby",
    "ready_players": [
      "b2c3d4e5-f6a7-8901-bcde-f12345678901"
    ],
    "relay_type": "matchbox",
    "current_spectators": [],
    "missed_events": [
      {
        "type": "PlayerJoined",
        "data": {
          "player": {
            "id": "b2c3d4e5-f6a7-8901-bcde-f12345678901",
            "name": "Bob",
            "is_authority": false,
            "is_ready": false,
            "connected_at": "2025-01-15T10:31:00Z"
          }
        }
      },
      {
        "type": "LobbyStateChanged",
        "data": {
          "lobby_state": "lobby",
          "ready_players": [
            "b2c3d4e5-f6a7-8901-bcde-f12345678901"
          ],
          "all_ready": false
        }
      }
    ],
    "replay": "complete",
    "sender_watermarks": [
      {
        "player_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
        "epoch": 1,
        "seq": 12
      },
      {
        "player_id": "b2c3d4e5-f6a7-8901-bcde-f12345678901",
        "epoch": 2,
        "seq": 0
      }
    ]
  }
}
```

The `replay` field states how complete `missed_events` is. It is sent
only to clients that negotiated protocol v3 or higher (the v2 wire is
unchanged — no `replay` key) and takes one of three values:

- `complete` -- Every replayable control event since your disconnect is
  in `missed_events`.
- `truncated` -- The bounded replay ring evicted events you needed, so
  `missed_events` is only a suffix of what happened. Discard any local
  room-membership bookkeeping and resync from the `Reconnected`
  snapshot fields (`current_players`, `lobby_state`, `ready_players`,
  `current_spectators`).
- `unavailable` -- Event replay is disabled on this deployment
  (`event_buffer_size` is `0`). Treat every reconnection as a full
  resync from the snapshot fields, exactly as for `truncated`.

v2 clients should always resync from the snapshot fields; the replayed
events are a convenience, not a completeness guarantee, without the
`replay` field.

The `sender_watermarks` field is also v3-only. It gives one authoritative
`(epoch, seq)` baseline for every current room member, including the
reconnected player (`seq: 0` when that member has not relayed `GameData` in
its current incarnation). `missed_events` never contains `GameData`; use the
snapshot plus application-level state sync for gameplay, and use
`sender_watermarks` only to resume relay gap accounting.

Other players in the room receive a `PlayerReconnected` notification so
they know you are back:

```json
{
  "type": "PlayerReconnected",
  "data": {
    "player_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
  }
}
```

## The Full Reconnection Timeline

```text
Player disconnects
       |
       v
Server detects disconnect (register_disconnection)
  - Generates reconnection token (UUID bound to player, room)
  - Records authority status for potential restoration
  - Starts buffering the room's control events for this player
  - Starts expiration timer (default: 300 seconds)
       |
       |  ... time passes, events happen in the room ...
       |
Player reconnects (new WebSocket)
  - Sends Reconnect message with player_id, room_id, auth_token
       |
       v
Server validates token
  - Checks token matches player_id and room_id
  - Checks reconnection window has not expired
  - Checks player is not already connected (duplicate guard)
  - Checks room still exists
       |
       v
Server restores player
  - Replays missed control events from the buffer
  - Restores authority role if previously held
  - Notifies other players via PlayerReconnected
  - Sends Reconnected with full room state + missed events
    (+ v3-only replay completeness and sender watermark fields)
```

## Reconnection Window

The reconnection window is the amount of time the server holds a
player's spot after a disconnect. The default is **300 seconds
(5 minutes)**. After this window expires, the reconnection token becomes
invalid and the player's slot is freed.

## Event Buffer

While at least one player of a room is disconnected (awaiting
reconnection), the server buffers the room's **control events** so they
can be replayed on reconnect. Only room-uniform events — messages every
member receives identically — are buffered:

- `PlayerJoined` / `PlayerLeft` / `PlayerReconnected`
- `NewSpectatorJoined` / `SpectatorDisconnected`
- `LobbyStateChanged`
- `AuthorityChanged` (the uniform broadcast form)

Two categories are deliberately **never** replayed:

- **`GameData` / `GameDataBinary` / `Signal`** -- The data path is
  high-rate; buffering it would immediately purge the control events
  that matter. Reconnecting clients resync game state at the
  application level.
- **`GameStarting`** -- Its `peer_connections` are customized per
  recipient, so replaying another player's copy would be wrong. A
  reconnect into a started session receives the room snapshot plus a
  fresh, tailored `SessionPlan` via the dedicated late-join flow, which
  fully covers session re-entry.

The buffer is a bounded ring (default **100 events** per room): when it
overflows, the oldest events are evicted and the server records the
highest evicted sequence number as an eviction watermark. On reconnect,
the watermark — not a gap in sequence numbers, which can occur benignly
because sequence numbers are global across rooms — decides whether your
replay was truncated, and the `replay` field reports it (see above).
Each eviction also increments the
`signal_fish_reconnection_events_evicted_total` metric; a sustained
non-zero rate means `event_buffer_size` is too small for the room churn
it serves.

The buffer exists only while someone is pending: it is created when a
disconnect is registered, and released when the last disconnected
player of the room reconnects or their reconnection window expires
(and, as a backstop, when the room itself is cleaned up).

## When Reconnection Fails

The server responds with a `ReconnectionFailed` message if:

- **Token expired** -- The reconnection window has passed.
- **Invalid token** -- The token does not match the player or room.
- **Room closed** -- The room was cleaned up while the player was away.
- **Already connected** -- The player is already connected from another
  session.

```json
{
  "type": "ReconnectionFailed",
  "data": {
    "reason": "Reconnection window has expired",
    "error_code": "RECONNECTION_EXPIRED"
  }
}
```

When reconnection fails, the client should fall back to a fresh
`JoinRoom` flow.

## Configuration

Reconnection is enabled by default. Relevant server settings:

- `enable_reconnection` -- Set to `true` (default) to enable, or `false`
  to disable reconnection entirely.
- `reconnection_window` -- Seconds before a disconnected player's token
  expires. Default: `300` (5 minutes).
- `event_buffer_size` -- Maximum number of control events buffered per
  room (the replay ring). Default: `100`. Set to `0` to disable event
  replay entirely; v3 clients are then told `replay: "unavailable"`.

```json
{
  "server": {
    "enable_reconnection": true,
    "reconnection_window": 300,
    "event_buffer_size": 100
  }
}
```

## Security Notes

- Reconnection tokens are single-use UUIDs with short validity windows.
- Tokens are validated against both the player ID and room ID to prevent
  reuse across sessions.
- A duplicate connection guard prevents a player from being connected
  twice simultaneously.

## Next Steps

- [Rooms and Lobbies](rooms-and-lobbies.md) -- Room lifecycle and states
- [Authority System](authority.md) -- How authority interacts with
  reconnection
- [Spectator Mode](spectator-mode.md) -- Read-only observers
