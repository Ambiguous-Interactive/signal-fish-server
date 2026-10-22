# Spectator Mode

Spectators are read-only observers who attach to a game room without
participating. They receive a room snapshot when they join, then live room broadcasts and
relayed gameplay. They cannot affect the game. Spectators do not count toward
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

`spectator_name` follows the same charset and byte-length rules as player
names, but — unlike player names — it carries **no uniqueness contract**:
several spectators may share (or visually match) a display name. This is
deliberate. Spectator identity is non-authoritative display metadata, and
spectator capacity can be unlimited, so enforced uniqueness would let the first
client to join under any name permanently block everyone else who uses it.
Player names keep strict uniqueness because player identity gates gameplay
authority; spectator names do not.

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
        "epoch": 1,
        "seq": 17
      },
      {
        "id": "b2c3d4e5-f6a7-8901-bcde-f12345678901",
        "name": "Bob",
        "is_authority": false,
        "is_ready": true,
        "epoch": 2,
        "seq": 0
      }
    ],
    "current_spectators": [
      {
        "id": "c3d4e5f6-a7b8-9012-cdef-123456789012",
        "name": "Observer1"
      }
    ],
    "lobby_state": "lobby",
    "reason": "joined"
  }
}
```

The example shows negotiated v3: every `current_players` entry carries its
current relay `epoch` and exact recipient-visible `seq` baseline. A pre-v3
recipient receives the same snapshot without either field, but with the
frozen legacy `connection_info` field. Protocol v3 snapshots never echo
`connection_info` (issue #529): the self-declared metadata keeps its single
consumer, the `GameStarting` legacy handoff surface. The pair is the
accountability baseline for live relay delivery. The server captures these
watermarks and queues `SpectatorJoined` under the room routing fence before it
publishes the spectator as a recipient.

This gives the spectator an immediate snapshot of who is in the room,
what state the lobby is in, and who else is spectating.

## What Spectators Receive

A spectator receives `SpectatorJoined` before live room traffic. It then receives
public room broadcasts, including `PlayerJoined`, `PlayerLeft`,
`PlayerReconnected`, `LobbyStateChanged`, `AuthorityChanged`, `GameStarting`,
`NewSpectatorJoined`, and `SpectatorDisconnected`. Its own join appears in the
snapshot, so it does not get a second notification for that admission.
`SpectatorLeft` confirms the end of its observer membership.

The server relays player `GameData` and binary game data to spectators. The
sender remains a seated player. Spectators get no personalized `SessionPlan`,
peer assignment, authority, or player slot. Direct and WebRTC gameplay does not
pass through this relay. A game that uses those transports must also send the
viewing state through server `GameData` if spectators need it. A late spectator
gets current room metadata, not saved gameplay or an application state snapshot.
The game must provide the application snapshot in its own relayed payloads.

### Delivery and limits

Spectators use the same bounded outbound queues and message-size limits as
players. Reliable data waits for capacity, then closes a slow recipient at the
configured timeout. On v3, volatile and latest data use the existing loss and
coalescing rules. Exact `DeliveryReport` gaps account for discarded sequences.
Control frames have priority over data. Sequence order is per sender and room
incarnation; there is no total order across senders. Pre-v3 recipients use the
legacy reliable FIFO format without v3 sequence, epoch, class, or gap fields.
The recipient's negotiated game-data encoding still controls projection.

Leaving removes the broadcast route and advances the delivery generation.
Queued old-room data cannot enter the next observer or player membership.
Spectators do not have seated reconnection tokens. After a lost connection,
join as a spectator again and use the new baseline. Viewing does not acknowledge
or confirm a player's application state, and spectator data cannot enter the
player relay.

Room admission uses `server.default_max_spectators` and the server connection
limits. The default derives a room cap of twice its player limit. An explicit
zero removes that room cap; it does not remove queue or connection bounds.

### Detecting support

For v3 and later, check for `spectator_relay` in `ProtocolInfo.capabilities`.
The server advertises this token independently of SDK compatibility metadata.
Older v3 servers lack the token. A successful spectator join alone does not
prove live delivery. Protocol v2 keeps its frozen capability bytes; use a
qualified server deployment to enable live viewing on that version.

### Custom coordinators

This changes the public `MessageCoordinator` extension API.
`commit_room_messages_if_members_with_hook` now takes `spectator_messages`
after `recipient_messages`. Reserve these public frames only for observers,
and keep them in the same publication phases. Keep observer failures outside
the seated failure count and transaction outcome. Private player plans must
never enter that vector.

Implement `register_local_spectator_with_initial_message_async` to capture the
seated roster and relay watermarks, enqueue the baseline, and publish the
observer under one routing fence. The default refuses admission. Keep
`routed_player_ids` restricted to seated players, while room-wide broadcasts
include observers. Roomless registration and unregister must clear observer
routes. The production in-memory coordinator implements this contract.

## What Spectators Cannot Do

Spectators are strictly read-only. The following actions are **not
available** to spectators:

- **Send GameData** -- Spectators cannot inject game data into the room.
  Attempted game data (JSON or raw binary) is answered with `NOT_IN_ROOM`,
  since spectator connections are never seated in the player roster.
- **Send PlayerReady** -- Spectators cannot affect the ready-up flow.
- **Send StartGame** -- Spectators cannot finalize the lobby. Like the other
  in-room lobby control actions, a spectator's `StartGame` is answered with
  `NOT_IN_ROOM`: the start authorization trusts the sender's seated room
  assignment, and spectators have only a read-only broadcast route.
- **Send AuthorityRequest** -- Spectators cannot claim or release
  authority.
- **Affect max_players** -- Spectators are tracked separately and do not
  occupy player slots.

## Spectator Notifications

When a spectator joins, players and existing spectators in the room receive a
`NewSpectatorJoined` broadcast:

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

On protocol v3 connections both broadcasts are delta events:
`current_spectators` is `[]` and `spectator_count` carries the room's total
after the change. Members track the roster by applying these deltas to the
roster from their `RoomJoined`, `SpectatorJoined`, or `Reconnected`
snapshot. Pre-v3 connections receive the full roster and never receive
`spectator_count`.

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

After the server removes the stored spectator entry, a recovered panic during
local detach completion still completes the leave. The server sends the same
confirmation and room notification, and records the notification for reconnect replay. Shutdown drain
can suppress these messages as it does for an ordinary detach.
If the server cannot publish the updated delivery handle after one retry, it
closes the spectator connection and still publishes the room notification.

## When Spectator Join Fails

If the room does not exist, admission input is invalid, the shared
room-admission attempt budget is exhausted, or spectating is not allowed, the
server responds with a `SpectatorJoinFailed` carrying the reason and an
`error_code` (for a missing room, the literal reason is `Room not found`):

```json
{
  "type": "SpectatorJoinFailed",
  "data": {
    "reason": "Room not found",
    "error_code": "ROOM_NOT_FOUND"
  }
}
```

Every `JoinAsSpectator` is answered by exactly one of `SpectatorJoined` or
`SpectatorJoinFailed`, so a client can await that pair without a timeout —
the same contract `JoinRoom` has with `RoomJoined` / `RoomJoinFailed`.
Valid room codes are case-insensitive and normalized before lookup. Invalid
game names and room codes use `INVALID_GAME_NAME` and `INVALID_ROOM_CODE`;
exhausted shared admission budgets use `RATE_LIMIT_EXCEEDED`.

## Use Cases

- **Tournament viewing** -- Initialize an audience view without interfering
  with gameplay, then relay live match state from the players.
- **Coaching** -- A coach observes a student's game to provide feedback
  after the session.
- **Debugging** -- During development, attach a spectator to capture the
  `SpectatorJoined` room snapshot (current players, spectators, and lobby
  state) and live relay data without taking a player slot.
- **Streaming** -- Seed a broadcast overlay from the room snapshot and relay
  application viewing state from the players.

## Spectator Limits

New rooms get a spectator capacity by default. `server.default_max_spectators`
is `null` by default, which derives each room's capacity from its player
ceiling (`2 * max_players`). Set a positive value to fix the capacity for
every created room, or `0` to restore unlimited spectators (not recommended
for multi-tenant deployments). A join past the capacity is refused before any
room mutation and surfaces through the terminal `SpectatorJoinFailed`
response carrying the `TOO_MANY_SPECTATORS` error code.

## Next Steps

- [Rooms and Lobbies](rooms-and-lobbies.md) -- Room lifecycle and the
  ready-up flow
- [Authority System](authority.md) -- How authority works for players
- [Reconnection](reconnection.md) -- Recovering from dropped connections
