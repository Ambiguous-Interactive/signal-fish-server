# Spectator

This scenario shows the server's current spectator contract. A spectator joins a
running room by its code, receives a roster and spectator snapshot, then live
relay data and public room events. Spectators never send `GameData` and never
affect the lobby or readiness. On v3, check the `spectator_relay` capability
before enabling this flow.

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
        "epoch": 1,
        "seq": 17
      },
      {
        "id": "00000000-0000-0000-0000-00000000000b",
        "name": "Bob",
        "is_authority": false,
        "is_ready": true,
        "epoch": 2,
        "seq": 0
      }
    ],
    "current_spectators": [
      {
        "id": "00000000-0000-0000-0000-000000000051",
        "name": "Observer1"
      }
    ],
    "lobby_state": "finalized",
    "reason": "joined"
  }
}
```

Next: Observer1 renders the baseline. Alice and Bob receive
`NewSpectatorJoined`. Observer1 then receives live room broadcasts.

## 2. Gameplay continues over the relay

Intent: Alice sends `GameData` that includes the application state needed for
viewing. Bob and Observer1 receive the same player payload. On v3, Observer1
uses the snapshot's `epoch` and `seq` watermarks as its relay baseline.
Reliable delivery stays contiguous; lossy delivery has exact `DeliveryReport`
gaps under the existing queue rules. A late viewer needs an application state
snapshot from a player; the server does not replay prior gameplay.

Next: Observer1 updates its read-only view. It receives no private session plan
and does not enter the seated roster. Direct or WebRTC gameplay requires an
additional server relay payload for viewing.

## 3. A second spectator joins

Intent: Observer2 joins the same room (sending the identical `JoinAsSpectator` with `spectator_name: "Observer2"`).
Observer2 receives its own `SpectatorJoined`. Players and existing spectators
receive the newcomer notification.

Alice, Bob, and Observer1 receive `NewSpectatorJoined`, carrying the new spectator and the
full updated spectator list (protocol v2 connections; on v3 connections the
event is a join delta — `current_spectators` is `[]` and `spectator_count`
holds the room total):

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

Next: Observer1 applies the spectator delta. Observer2 uses its own baseline.

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

and tells Alice and Bob that Observer1 is gone (protocol v2 shape; on v3
connections `current_spectators` is `[]` and `spectator_count` is `1`):

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
