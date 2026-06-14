# Reconnection

This scenario shows a client recovering from a dropped connection. When a player disconnects, the server holds a
reconnection slot and issues an authentication token. The client opens a fresh WebSocket and sends `Reconnect`
with its `player_id`, `room_id`, and `auth_token`; the server replies with `Reconnected`, carrying the current
room state plus every event the client missed while away. The failure case (an invalid token) is shown at the end.

Throughout this page:

- Bob, id `00000000-0000-0000-0000-00000000000b`, reconnecting.
- Alice, id `00000000-0000-0000-0000-00000000000a`, still in the room.
- Room `11111111-1111-1111-1111-111111111111`, code `ABC123`.

Bob is mid-game (already joined and ready) when his network drops. The reconnection flow is identical for v2 and v3
clients; a v3 client that reconnects into an active non-relay session additionally receives a fresh `SessionPlan`
(noted at the end).

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

The server validates the token, re-seats Bob, and replies with the full current room state plus the events he
missed:

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
    "is_authority": false,
    "lobby_state": "finalized",
    "ready_players": [
      "00000000-0000-0000-0000-00000000000a",
      "00000000-0000-0000-0000-00000000000b"
    ],
    "relay_type": "matchbox",
    "current_spectators": [],
    "missed_events": [
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

Next: Bob's client reconciles its local state from `current_players` / `lobby_state`, then replays the
`missed_events` array **in order** to catch up on gameplay that happened while he was away (here, one `GameData`
move from Alice). The game resumes.

## 3. v3 note — reconnecting into an active session

If the room is running a non-relay v3 session, Bob's `Reconnected` is followed by a fresh, per-recipient
`SessionPlan` describing the running session (its current peers, `initiate` flags, `host`, and fresh ICE). The
existing members receive a `NewPeer` delta for the rejoined Bob instead of a full plan. Bob's `Reconnected`
carries no `ice_servers` of its own in that case — the fresh ICE arrives in the `SessionPlan`. See the
[mesh](v3-mesh-webrtc.md) and [host failover](v3-host-failover.md) scenarios for the `SessionPlan` shape.

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
    "reason": "The reconnection token is invalid or malformed.",
    "error_code": "RECONNECTION_TOKEN_INVALID"
  }
}
```

Next: Bob cannot resume his old seat. His client must start over — authenticate and `JoinRoom` (with
`room_code: "ABC123"` to re-enter the same room as a new player), exactly as in the
[v2 two-player relay](v2-two-player-relay.md) flow. A related failure, `RECONNECTION_EXPIRED`, is returned when the
reconnection window has elapsed; the recovery is the same — rejoin as a new player.
