# Reconnection

This scenario shows a client recovering from a dropped connection. When a player disconnects, the server holds a
reconnection slot and issues an authentication token. The client opens a fresh WebSocket and sends `Reconnect`
with its `player_id`, `room_id`, and `auth_token`; the server replies with `Reconnected`, carrying the current
room state plus replayable control events the client missed while away. The failure case (an invalid token) is
shown at the end.

Throughout this page:

- Bob, id `00000000-0000-0000-0000-00000000000b`, reconnecting.
- Alice, id `00000000-0000-0000-0000-00000000000a`, still in the room.
- Room `11111111-1111-1111-1111-111111111111`, code `ABC123`.

Bob is mid-game (already joined and ready) when his network drops. The lifecycle
is the same for v2 and v3, but the v3 `Reconnected` wire also carries replay
status, complete `sender_watermarks`, and a new physical-connection accounting
lifetime. A v3 client that reconnects into an active non-relay session
additionally receives a fresh `SessionPlan` (noted at the end).

## 1. Bob disconnects

Intent: Bob's socket closes unexpectedly. There is no client message here — the server observes the closed
connection.

On disconnect, the server keeps Bob's seat for the reconnection window and generates a reconnection token bound to
his `player_id` and `room_id`. The token is delivered to the client out of band by the SDK's transport layer (it is
not a JSON protocol message); Bob's client stores it alongside the `player_id` and `room_id` it already saved from
the original `RoomJoined`.

Meanwhile, Alice (still connected) sees nothing special yet — Bob is in a reconnecting state, not removed.

Next: Bob's client opens a brand-new WebSocket to the same endpoint and attempts to reconnect.

## 2. Bob reconnects with his token

Intent: on the fresh connection, Bob skips the normal join flow and sends `Reconnect` with the three stored values.

Bob sends:

```json
{
  "type": "Reconnect",
  "data": {
    "player_id": "00000000-0000-0000-0000-00000000000b",
    "room_id": "11111111-1111-1111-1111-111111111111",
    "auth_token": "rTok_9f3c1a7e8b2d4c6f0a1b2c3d4e5f6071"
  }
}
```

The server validates the token, re-seats Bob, and replies with the full current room state plus any replayable
control events he missed. High-rate `GameData` is not replayed; Bob resynchronizes gameplay from the room snapshot
and application state:

```json
{
  "type": "Reconnected",
  "data": {
    "room_id": "11111111-1111-1111-1111-111111111111",
    "room_code": "ABC123",
    "player_id": "00000000-0000-0000-0000-00000000000b",
    "game_name": "my-game",
    "max_players": 2,
    "supports_authority": true,
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
    "is_authority": false,
    "lobby_state": "finalized",
    "ready_players": [
      "00000000-0000-0000-0000-00000000000a",
      "00000000-0000-0000-0000-00000000000b"
    ],
    "relay_type": "matchbox",
    "current_spectators": [],
    "missed_events": [],
    "replay": "complete",
    "sender_watermarks": [
      {
        "player_id": "00000000-0000-0000-0000-00000000000a",
        "epoch": 1,
        "seq": 42
      },
      {
        "player_id": "00000000-0000-0000-0000-00000000000b",
        "epoch": 2,
        "seq": 0
      }
    ]
  }
}
```

At the same time, the server tells the other members that Bob is back with `PlayerReconnected`. Alice receives:

```json
{
  "type": "PlayerReconnected",
  "data": {
    "player_id": "00000000-0000-0000-0000-00000000000b"
  }
}
```

Next: Bob's client reconciles its local state from `current_players` / `lobby_state`, replays any
`missed_events` control entries **in order**, and asks the game authority or peers for the current gameplay state.
For v3 relay sequencing, `sender_watermarks` resets Bob's per-sender `(epoch, seq)` baselines after his absence.
The game resumes after that application-level resync.

## 3. v3 note — reconnecting into an active session

Bob's `Reconnected` is followed by a fresh, per-recipient `SessionPlan`, and
every current v3 incumbent receives its own complete refresh after
`PlayerReconnected`. For a non-relay session those plans carry current peers,
`initiate` flags, `host`, and fresh ICE. For a relay-floor room they are explicit
empty-peer `relay`/`relay` resets. `Reconnected` carries no `ice_servers` of its
own in a finalized WebRTC session; fresh ICE arrives in the plan. See the
[mesh](v3-mesh-webrtc.md) and [host failover](v3-host-failover.md) scenarios.

## Failure case — invalid reconnection token

Intent: Bob's token is wrong, malformed, or its reconnection window expired. The server rejects the attempt.

Bob sends a `Reconnect` with a bad token:

```json
{
  "type": "Reconnect",
  "data": {
    "player_id": "00000000-0000-0000-0000-00000000000b",
    "room_id": "11111111-1111-1111-1111-111111111111",
    "auth_token": "not-a-valid-token"
  }
}
```

The server replies:

```json
{
  "type": "ReconnectionFailed",
  "data": {
    "reason": "Invalid reconnection token",
    "error_code": "RECONNECTION_TOKEN_INVALID"
  }
}
```

Next: Bob cannot resume his old seat. His client must start over — authenticate and `JoinRoom` (with
`room_code: "ABC123"` to re-enter the same room as a new player), exactly as in the
[v2 two-player relay](v2-two-player-relay.md) flow. A related failure, `RECONNECTION_EXPIRED`, is returned when the
reconnection window has elapsed; the recovery is the same — rejoin as a new player.
