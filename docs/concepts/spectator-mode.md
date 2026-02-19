# Spectator Mode

Spectators are read-only observers who watch a game room without
participating. They see everything that happens -- player joins, lobby
state changes, game data broadcasts -- but they cannot affect the game
in any way. Spectators do not count toward the room's `max_players`
limit, so they never block a real player from joining.

## Joining as a Spectator

To spectate a room, send a `JoinAsSpectator` message with the game name,
room code, and a display name:

```json
{
  "type": "JoinAsSpectator",
  "data": {
    "game_name": "my-game",
    "room_code": "HK7T3W",
    "spectator_name": "Observer1"
  }
}
```

All three fields are required. Unlike `JoinRoom`, you cannot create a
room by spectating -- you must provide an existing `room_code`.

## Spectator Joined Response

On success, the server sends a `SpectatorJoined` message with the current
room state:

```json
{
  "type": "SpectatorJoined",
  "data": {
    "room_id": "550e8400-e29b-41d4-a716-446655440000",
    "room_code": "HK7T3W",
    "spectator_id": "c3d4e5f6-a7b8-9012-cdef-123456789012",
    "game_name": "my-game",
    "current_players": [
      {
        "id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
        "name": "Alice",
        "is_authority": false,
        "is_ready": true,
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
    "current_spectators": [
      {
        "id": "c3d4e5f6-a7b8-9012-cdef-123456789012",
        "name": "Observer1",
        "connected_at": "2025-01-15T10:35:00Z"
      }
    ],
    "lobby_state": "lobby"
  }
}
```

This gives the spectator an immediate snapshot of who is in the room,
what state the lobby is in, and who else is spectating.

## What Spectators Receive

Once joined, spectators receive the same broadcast messages that players
receive:

- `PlayerJoined` -- A new player entered the room.
- `PlayerLeft` -- A player left the room.
- `LobbyStateChanged` -- The lobby state transitioned (Waiting, Lobby, or
  Finalized).
- `GameData` -- Game data sent between players.
- `AuthorityChanged` -- The room's authority player changed.
- `GameStarting` -- The game has been finalized with peer connection info.
- `NewSpectatorJoined` -- Another spectator joined.
- `SpectatorDisconnected` -- Another spectator left.

## What Spectators Cannot Do

Spectators are strictly read-only. The following actions are **not
available** to spectators:

- **Send GameData** -- Spectators cannot inject game data into the room.
- **Send PlayerReady** -- Spectators cannot affect the ready-up flow.
- **Send AuthorityRequest** -- Spectators cannot claim or release
  authority.
- **Affect max_players** -- Spectators are tracked separately and do not
  occupy player slots.

## Spectator Notifications

When a spectator joins, all players and other spectators in the room
receive a `NewSpectatorJoined` broadcast:

```json
{
  "type": "NewSpectatorJoined",
  "data": {
    "spectator": {
      "id": "c3d4e5f6-a7b8-9012-cdef-123456789012",
      "name": "Observer1",
      "connected_at": "2025-01-15T10:35:00Z"
    },
    "current_spectators": [
      {
        "id": "c3d4e5f6-a7b8-9012-cdef-123456789012",
        "name": "Observer1",
        "connected_at": "2025-01-15T10:35:00Z"
      }
    ]
  }
}
```

When a spectator leaves or disconnects, a `SpectatorDisconnected`
message is broadcast:

```json
{
  "type": "SpectatorDisconnected",
  "data": {
    "spectator_id": "c3d4e5f6-a7b8-9012-cdef-123456789012",
    "reason": "disconnected",
    "current_spectators": []
  }
}
```

## Leaving Spectator Mode

To leave, send a `LeaveSpectator` message:

```json
{
  "type": "LeaveSpectator"
}
```

The server confirms with a `SpectatorLeft` response:

```json
{
  "type": "SpectatorLeft",
  "data": {
    "room_id": "550e8400-e29b-41d4-a716-446655440000",
    "room_code": "HK7T3W",
    "reason": "voluntary_leave",
    "current_spectators": []
  }
}
```

## When Spectator Join Fails

If the room does not exist or spectating is not allowed, the server
responds with `SpectatorJoinFailed`:

```json
{
  "type": "SpectatorJoinFailed",
  "data": {
    "reason": "The requested room could not be found.",
    "error_code": "ROOM_NOT_FOUND"
  }
}
```

## Use Cases

- **Tournament viewing** -- Let an audience watch competitive matches in
  real time without interfering with gameplay.
- **Coaching** -- A coach observes a student's game to provide feedback
  after the session.
- **Debugging** -- During development, connect a spectator client to
  watch the raw message flow and room state transitions without
  disrupting the game under test.
- **Streaming** -- Feed spectator data into a broadcast overlay or
  streaming tool that renders game state for viewers.

## Spectator Limits

By default, rooms accept an unlimited number of spectators. Server
operators can configure a maximum spectator count per room if needed.
When the limit is reached, new spectators receive a `SpectatorJoinFailed`
response with the `TOO_MANY_SPECTATORS` error code.

## Next Steps

- [Rooms and Lobbies](rooms-and-lobbies.md) -- Room lifecycle and the
  ready-up flow
- [Authority System](authority.md) -- How authority works for players
- [Reconnection](reconnection.md) -- Recovering from dropped connections
