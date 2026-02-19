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
    "supports_authority": false,
    "current_players": [
      {
        "id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
        "name": "Alice",
        "is_authority": false,
        "is_ready": false,
        "connected_at": "2025-01-15T10:30:00Z"
      }
    ],
    "is_authority": false,
    "lobby_state": "waiting",
    "ready_players": [],
    "relay_type": "WebRTC",
    "current_spectators": []
  }
}
```

When a player disconnects, the server generates a reconnection token
(a server-generated UUID) bound to the player ID, room ID, and
authority status. The token is created at disconnect time and provided
to the client through the reconnection mechanism. Treat it like a
session credential.

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
message contains the current room state **and** an array of all events
you missed while disconnected:

```json
{
  "type": "Reconnected",
  "data": {
    "room_id": "550e8400-e29b-41d4-a716-446655440000",
    "room_code": "HK7T3W",
    "player_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "game_name": "my-game",
    "max_players": 4,
    "supports_authority": false,
    "current_players": [
      {
        "id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
        "name": "Alice",
        "is_authority": false,
        "is_ready": false,
        "connected_at": "2025-01-15T10:30:00Z"
      },
      {
        "id": "b2c3d4e5-f6a7-8901-bcde-f12345678901",
        "name": "Bob",
        "is_authority": false,
        "is_ready": true,
        "connected_at": "2025-01-15T10:31:00Z"
      }
    ],
    "is_authority": false,
    "lobby_state": "lobby",
    "ready_players": [
      "b2c3d4e5-f6a7-8901-bcde-f12345678901"
    ],
    "relay_type": "WebRTC",
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
    ]
  }
}
```

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
  - Starts buffering room events for this player
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
  - Replays missed events from the buffer
  - Restores authority role if previously held
  - Notifies other players via PlayerReconnected
  - Sends Reconnected with full room state + missed events
```

## Reconnection Window

The reconnection window is the amount of time the server holds a
player's spot after a disconnect. The default is **300 seconds
(5 minutes)**. After this window expires, the reconnection token becomes
invalid and the player's slot is freed.

## Event Buffer

While a player is disconnected, the server buffers events that occur in
their room. The default buffer size is **100 events** per disconnected
player. The buffer uses a ring structure -- if more than 100 events occur,
the oldest events are evicted. When the last disconnected player in a room
reconnects or their token expires, the buffer is cleared.

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
    "reason": "The reconnection window has expired. You must join the room again as a new player.",
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
- `event_buffer_size` -- Maximum number of events buffered per
  disconnected player. Default: `100`.

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
