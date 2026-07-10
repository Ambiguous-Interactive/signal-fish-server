# Spectator

This scenario shows the server's current spectator contract. A spectator joins a
running room by its code, receives a one-time roster and spectator snapshot, then
leaves. Spectators never send `GameData` and never affect the lobby or readiness.
They are not currently members of the room broadcast set, so live gameplay and
presence updates must come from an out-of-band application channel.

Throughout this page:

- Alice (`...000a`) and Bob (`...000b`) are players already in a finalized game.
- Observer1, spectator id `00000000-0000-0000-0000-000000000051`, joins to watch.
- Observer2, spectator id `00000000-0000-0000-0000-000000000052`, joins later.
- Room `11111111-1111-1111-1111-111111111111`, code `ABC123`.

The spectator authenticates with the same `Authenticate` operation as a player
(see step 1 of the [v2 two-player relay](v2-two-player-relay.md) flow). The
examples below assume `/v3/ws` with protocol 3 negotiated. The lifecycle is the
same in v2, but v2 omits player epochs.

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
        "connected_at": "2026-06-14T10:00:00Z",
        "epoch": 1
      },
      {
        "id": "00000000-0000-0000-0000-00000000000b",
        "name": "Bob",
        "is_authority": false,
        "is_ready": true,
        "connected_at": "2026-06-14T10:00:30Z",
        "epoch": 2
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

Next: Observer1's client renders the one-time player and spectator snapshot.
Alice and Bob receive `NewSpectatorJoined`; Observer1 does not receive subsequent
room broadcasts.

## 2. Gameplay continues out of band

Intent: the application can use the snapshot to initialize a read-only view, but
must carry live game state separately. When Alice sends a relayed `GameData`, Bob
receives it and Observer1 receives no WebSocket frame.

Next: Observer1's application consumes its out-of-band stream. The signaling
server does not issue `DeliveryReport` for gameplay the spectator was never
eligible to receive.

## 3. A second spectator joins

Intent: Observer2 joins the same room (sending the identical `JoinAsSpectator` with `spectator_name: "Observer2"`).
Observer2 receives its own `SpectatorJoined`; the room's **players** are told
about the newcomer. Existing spectators are not broadcast recipients.

Alice and Bob receive `NewSpectatorJoined`, carrying the new spectator and the
full updated spectator list:

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

Next: Observer1 receives no frame. It retains its original snapshot unless the
application refreshes that state out of band.

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

and tells Alice and Bob that Observer1 is gone:

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
