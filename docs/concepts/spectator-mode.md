# Spectator Mode

Spectators are read-only observers who attach to a game room without
participating. They receive a one-time room snapshot when they join, but
they cannot affect the game in any way. Spectators do not count toward
the room's `max_players` limit, so they never block a real player from
joining.

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
        "connected_at": "2025-01-15T10:30:00Z",
        "epoch": 1,
        "seq": 17
      },
      {
        "id": "b2c3d4e5-f6a7-8901-bcde-f12345678901",
        "name": "Bob",
        "is_authority": false,
        "is_ready": true,
        "connected_at": "2025-01-15T10:31:00Z",
        "epoch": 2,
        "seq": 0
      }
    ],
    "current_spectators": [
      {
        "id": "c3d4e5f6-a7b8-9012-cdef-123456789012",
        "name": "Observer1",
        "connected_at": "2025-01-15T10:35:00Z"
      }
    ],
    "lobby_state": "lobby",
    "reason": "joined"
  }
}
```

The example shows negotiated v3: every `current_players` entry carries its
current relay `epoch` and exact recipient-visible `seq` baseline. A pre-v3
recipient receives the same snapshot without either field. The pair makes the
snapshot usable as an accountability baseline if live spectator delivery is
added later; spectators do not receive relayed gameplay today.

This gives the spectator an immediate snapshot of who is in the room,
what state the lobby is in, and who else is spectating.

## What Spectators Receive

A spectator receives only two direct messages tied to its own spectator
session:

- `SpectatorJoined` -- the room snapshot delivered once on a successful
  join (current players, current spectators, and lobby state).
- `SpectatorLeft` -- the confirmation delivered when the spectator leaves
  or disconnects.

> **Known limitation:** In the current server, spectators are not added
> to the room's broadcast set. Room broadcasts -- `PlayerJoined`,
> `PlayerLeft`, `LobbyStateChanged`, `GameData`, `AuthorityChanged`,
> `GameStarting` -- and peer spectator notifications
> (`NewSpectatorJoined`, `SpectatorDisconnected`) are delivered to the
> room's **players**, not to spectators. A spectator that needs live room
> updates must re-fetch or rely on out-of-band data; it will not receive
> these events over its WebSocket today.

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

When a spectator joins, all players in the room receive a
`NewSpectatorJoined` broadcast (spectators do not receive this
notification):

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
    ],
    "reason": "joined"
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
responds with an `Error` carrying the reason and an `error_code` (for a
missing room, the literal reason is `Room not found`):

```json
{
  "type": "Error",
  "data": {
    "message": "Room not found",
    "error_code": "ROOM_NOT_FOUND"
  }
}
```

## Use Cases

- **Tournament viewing** -- Initialize an audience view without interfering
  with gameplay, then carry live match state through an out-of-band channel.
- **Coaching** -- A coach observes a student's game to provide feedback
  after the session.
- **Debugging** -- During development, attach a spectator to capture the
  `SpectatorJoined` room snapshot (current players, spectators, and lobby
  state) without disrupting the game under test. Live room updates are
  not delivered to spectators today (see the limitation above).
- **Streaming** -- Seed a broadcast overlay from the room snapshot, with live
  game state supplied by an out-of-band application channel.

## Spectator Limits

Rooms accept an unlimited number of spectators -- there is currently no
configuration option to cap spectator count per room. The protocol does
reserve a `TOO_MANY_SPECTATORS` error code: if a per-room limit is ever
introduced, a spectator exceeding it would receive an `Error` response
carrying that code. Until then, the limit is never reached.

## Next Steps

- [Rooms and Lobbies](rooms-and-lobbies.md) -- Room lifecycle and the
  ready-up flow
- [Authority System](authority.md) -- How authority works for players
- [Reconnection](reconnection.md) -- Recovering from dropped connections
