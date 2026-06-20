# Spectator

This scenario shows a read-only observer. A spectator joins a running room by its code, receives the current
roster and spectator list, observes gameplay `GameData` and other spectators joining, then leaves. Spectators
never send `GameData` and never affect the lobby or readiness — they only watch.

Throughout this page:

- Alice (`...000a`) and Bob (`...000b`) are players already in a finalized game.
- Observer1, spectator id `00000000-0000-0000-0000-000000000051`, joins to watch.
- Observer2, spectator id `00000000-0000-0000-0000-000000000052`, joins later.
- Room `11111111-1111-1111-1111-111111111111`, code `ABC123`.

The spectator first authenticates exactly as a player does (step 1 of the
[v2 two-player relay](v2-two-player-relay.md) flow); the spectator flow is identical for v2 and v3.

## 1. Observer1 joins as a spectator

Intent: Observer1 joins an existing room as a read-only observer. Unlike `JoinRoom`, `JoinAsSpectator` **requires**
a `room_code` — a spectator can only watch a room that already exists.

Observer1 sends:

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

The server admits Observer1 and replies with the current room snapshot:

```json
{
  "type": "SpectatorJoined",
  "data": {
    "room_id": "11111111-1111-1111-1111-111111111111",
    "room_code": "ABC123",
    "spectator_id": "00000000-0000-0000-0000-000000000051",
    "game_name": "my-game",
    "current_players": [
      {
        "id": "00000000-0000-0000-0000-00000000000a",
        "name": "Alice",
        "is_authority": false,
        "is_ready": true,
        "connected_at": "2026-06-14T10:00:00Z"
      },
      {
        "id": "00000000-0000-0000-0000-00000000000b",
        "name": "Bob",
        "is_authority": false,
        "is_ready": true,
        "connected_at": "2026-06-14T10:00:30Z"
      }
    ],
    "current_spectators": [
      {
        "id": "00000000-0000-0000-0000-000000000051",
        "name": "Observer1",
        "connected_at": "2026-06-14T10:05:00Z"
      }
    ],
    "lobby_state": "finalized",
    "reason": "joined"
  }
}
```

Next: Observer1's client renders the player roster and the spectator list, then settles into a watch loop. The
players (Alice and Bob) are not notified about a spectator joining unless the deployment fans spectator presence to
players; spectators are notified about other spectators (step 3).

## 2. Observer1 observes GameData

Intent: spectators receive the same relayed `GameData` the players exchange, so they can render the match. A
spectator never sends `GameData`.

When Alice sends a move (the client-to-server `GameData` shown in the
[v2 relay scenario](v2-two-player-relay.md)), Observer1 receives the relayed form, identical to what Bob receives:

```json
{
  "type": "GameData",
  "data": {
    "from_player": "00000000-0000-0000-0000-00000000000a",
    "data": {
      "action": "move",
      "x": 100,
      "y": 200
    }
  }
}
```

Next: Observer1 renders the move. It keeps receiving every player's `GameData` for the lifetime of its spectator
session.

## 3. A second spectator joins

Intent: Observer2 joins the same room (sending the identical `JoinAsSpectator` with `spectator_name: "Observer2"`).
Observer2 receives its own `SpectatorJoined`; the **existing** spectators are told about the newcomer.

Observer1 receives `NewSpectatorJoined`, carrying the new spectator and the full updated spectator list:

```json
{
  "type": "NewSpectatorJoined",
  "data": {
    "spectator": {
      "id": "00000000-0000-0000-0000-000000000052",
      "name": "Observer2",
      "connected_at": "2026-06-14T10:06:00Z"
    },
    "current_spectators": [
      {
        "id": "00000000-0000-0000-0000-000000000051",
        "name": "Observer1",
        "connected_at": "2026-06-14T10:05:00Z"
      },
      {
        "id": "00000000-0000-0000-0000-000000000052",
        "name": "Observer2",
        "connected_at": "2026-06-14T10:06:00Z"
      }
    ],
    "reason": "joined"
  }
}
```

Next: Observer1 updates its spectator-count display. Both spectators now watch the same `GameData` stream.

## 4. Observer1 leaves

Intent: Observer1 stops watching. `LeaveSpectator` has no payload.

Observer1 sends:

```json
{
  "type": "LeaveSpectator"
}
```

The server confirms to **Observer1**:

```json
{
  "type": "SpectatorLeft",
  "data": {
    "room_id": "11111111-1111-1111-1111-111111111111",
    "room_code": "ABC123",
    "reason": "voluntary_leave",
    "current_spectators": [
      {
        "id": "00000000-0000-0000-0000-000000000052",
        "name": "Observer2",
        "connected_at": "2026-06-14T10:06:00Z"
      }
    ]
  }
}
```

and tells the remaining spectator, Observer2, that Observer1 is gone:

```json
{
  "type": "SpectatorDisconnected",
  "data": {
    "spectator_id": "00000000-0000-0000-0000-000000000051",
    "reason": "voluntary_leave",
    "current_spectators": [
      {
        "id": "00000000-0000-0000-0000-000000000052",
        "name": "Observer2",
        "connected_at": "2026-06-14T10:06:00Z"
      }
    ]
  }
}
```

Next: Observer1's client returns to its menu. The players' game is unaffected — spectators joining and leaving
never change lobby state, readiness, or the relay/transport plan.
