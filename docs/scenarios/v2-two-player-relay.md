# v2: Two-Player Relay

This is the canonical Signal Fish session. Two players authenticate, one creates a room and the other joins by its
code, both signal readiness, the game starts, they exchange `GameData` through the relay, and finally one leaves.
Everything here is plain v2 and works unchanged on both `/v2/ws` and `/v3/ws`.

Throughout this page:

- Alice is the host (room creator), id `00000000-0000-0000-0000-00000000000a`.
- Bob is the joiner, id `00000000-0000-0000-0000-00000000000b`.
- The room is `11111111-1111-1111-1111-111111111111`, room code `ABC123`.

Each player has opened its own WebSocket to `ws://localhost:3536/v2/ws`.

The `relay_type: "matchbox"` values below are informational legacy integration
labels, not transport selectors or reachability proof. The actual relay in this
scenario is the authenticated WebSocket `GameData` fan-out shown below;
`JoinRoom.relay_transport`, if supplied, is ignored.

## 1. Alice authenticates

Intent: Alice's client identifies the game application before doing anything else. `Authenticate` must be the first
message on the connection.

Alice sends:

```json
{
  "type": "Authenticate",
  "data": {
    "app_id": "mb_app_abc123",
    "sdk_version": "1.2.3",
    "platform": "unity"
  }
}
```

The server replies with two messages, in order:

```json
{
  "type": "Authenticated",
  "data": {
    "app_name": "my-game",
    "organization": "Ambiguous Interactive",
    "rate_limits": {
      "per_minute": 60,
      "per_hour": 3600,
      "per_day": 86400
    }
  }
}
```

```json
{
  "type": "ProtocolInfo",
  "data": {
    "capabilities": ["reconnection", "spectators", "authority"],
    "game_data_formats": ["json", "message_pack"]
  }
}
```

Next: Alice's client records the rate limits, notes that this is a negotiated v2 connection (the `ProtocolInfo`
carries no `protocol_version` field), and proceeds to create a room.

## 2. Alice creates a room

Intent: there is no separate room-creation message. Omitting `room_code` on `JoinRoom` asks the server to mint a
new room with a generated code.

Alice sends:

```json
{
  "type": "JoinRoom",
  "data": {
    "game_name": "my-game",
    "player_name": "Alice",
    "max_players": 2
  }
}
```

The server creates the room and replies:

```json
{
  "type": "RoomJoined",
  "data": {
    "room_id": "11111111-1111-1111-1111-111111111111",
    "room_code": "ABC123",
    "player_id": "00000000-0000-0000-0000-00000000000a",
    "game_name": "my-game",
    "max_players": 2,
    "supports_authority": true,
    "current_players": [
      {
        "id": "00000000-0000-0000-0000-00000000000a",
        "name": "Alice",
        "is_authority": false,
        "is_ready": false,
        "connected_at": "2026-06-14T10:00:00Z"
      }
    ],
    "is_authority": false,
    "lobby_state": "waiting",
    "ready_players": [],
    "relay_type": "matchbox",
    "current_spectators": []
  }
}
```

Next: Alice stores `room_id` and `player_id` (both are needed later for reconnection) and shows the room code
`ABC123` so Bob can join. The `RoomJoined` snapshot shows `waiting`; because the room is now non-empty the server
immediately follows with a `LobbyStateChanged{lobby}` (`max_players` is a ceiling, so the room enters the lobby
without being full).

## 3. Bob authenticates and joins by code

Intent: Bob authenticates on his own connection (step 1, identical shape), then joins Alice's room by supplying its
`room_code`.

Bob sends:

```json
{
  "type": "JoinRoom",
  "data": {
    "game_name": "my-game",
    "player_name": "Bob",
    "room_code": "ABC123"
  }
}
```

The server adds Bob and replies to **Bob** with his own `RoomJoined`:

```json
{
  "type": "RoomJoined",
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
        "is_ready": false,
        "connected_at": "2026-06-14T10:00:00Z"
      },
      {
        "id": "00000000-0000-0000-0000-00000000000b",
        "name": "Bob",
        "is_authority": false,
        "is_ready": false,
        "connected_at": "2026-06-14T10:00:30Z"
      }
    ],
    "is_authority": false,
    "lobby_state": "lobby",
    "ready_players": [],
    "relay_type": "matchbox",
    "current_spectators": []
  }
}
```

At the same time the server tells **Alice** that a player arrived:

```json
{
  "type": "PlayerJoined",
  "data": {
    "player": {
      "id": "00000000-0000-0000-0000-00000000000b",
      "name": "Bob",
      "is_authority": false,
      "is_ready": false,
      "connected_at": "2026-06-14T10:00:30Z"
    }
  }
}
```

Next: the room is already in the `lobby` state (it entered the lobby as soon as Alice created it — `max_players`
is a ceiling, not a required count). Both clients render the two-player roster and wait for ready-up.

## 4. Both players ready up

Intent: each player toggles its own ready state. `PlayerReady` has no payload — sending it the first time in the
`lobby` state marks the sender ready; sending it again would mark them unready.

Alice sends:

```json
{
  "type": "PlayerReady"
}
```

The server broadcasts the new lobby state to **both** players:

```json
{
  "type": "LobbyStateChanged",
  "data": {
    "lobby_state": "lobby",
    "ready_players": ["00000000-0000-0000-0000-00000000000a"],
    "all_ready": false
  }
}
```

Bob then sends his own ready toggle:

```json
{
  "type": "PlayerReady"
}
```

Now every player is ready, so the server broadcasts a lobby state with `all_ready: true`. Readiness alone does
**not** start the game — the room stays in `lobby` until a player explicitly starts it:

```json
{
  "type": "LobbyStateChanged",
  "data": {
    "lobby_state": "lobby",
    "ready_players": [
      "00000000-0000-0000-0000-00000000000a",
      "00000000-0000-0000-0000-00000000000b"
    ],
    "all_ready": true
  }
}
```

Next: every current player is ready, so any member may now finalize the lobby. (`max_players` is a ceiling, not a
required count — the room need not be full to start, and a single ready player could start a solo session.)

## 5. A player starts the game

Intent: a member sends `StartGame` to finalize the lobby with its current members. The message has no payload. It
is accepted only when every current player is ready (`all_ready: true`); the room here has no designated authority
(`supports_authority` is `true` but no one claimed it), so **any** member may start.

Alice sends:

```json
{
  "type": "StartGame"
}
```

The server finalizes the room and broadcasts `GameStarting` to **both** players. (The move into `finalized` is
signaled by `GameStarting` itself — the server never broadcasts a `LobbyStateChanged` carrying `finalized`; the
`finalized` value appears only in the `lobby_state` field of a later `RoomJoined` / `Reconnected` snapshot.)

```json
{
  "type": "GameStarting",
  "data": {
    "peer_connections": [
      {
        "player_id": "00000000-0000-0000-0000-00000000000a",
        "player_name": "Alice",
        "is_authority": false,
        "relay_type": "matchbox"
      },
      {
        "player_id": "00000000-0000-0000-0000-00000000000b",
        "player_name": "Bob",
        "is_authority": false,
        "relay_type": "matchbox"
      }
    ]
  }
}
```

Next: this is a relay-only (v2) room, so **no** `SessionPlan` is emitted. `GameStarting.peer_connections` is
legacy identity/authority metadata only — it is not proof of any peer-to-peer path. Both clients now relay all
gameplay traffic through the server as `GameData`.

## 6. Exchange GameData in both directions

Intent: gameplay traffic flows as `GameData`. A client wraps an arbitrary JSON-serializable object; the server
relays it to every other member of the room, tagging it with the sender's id.

Alice sends a move:

```json
{
  "type": "GameData",
  "data": {
    "data": {
      "action": "move",
      "x": 100,
      "y": 200
    }
  }
}
```

Bob receives it with `from_player` stamped on:

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

Bob replies with his own update:

```json
{
  "type": "GameData",
  "data": {
    "data": {
      "action": "fire",
      "target": "enemy-7"
    }
  }
}
```

Alice receives:

```json
{
  "type": "GameData",
  "data": {
    "from_player": "00000000-0000-0000-0000-00000000000b",
    "data": {
      "action": "fire",
      "target": "enemy-7"
    }
  }
}
```

Note the asymmetry between the two `GameData` shapes: the **client-to-server** form nests the payload under an
inner `data` key (the outer `data` is the serde content tag, the inner `data` is the variant field). The
**server-to-client** form replaces that inner key with `from_player` plus the same payload under `data`. The
sender never receives an echo of its own message.

Next: the two clients keep exchanging `GameData` for the lifetime of the match.

## 7. Alice leaves the room

Intent: Alice ends her session cleanly. `LeaveRoom` has no payload.

Alice sends:

```json
{
  "type": "LeaveRoom"
}
```

The server confirms to **Alice**:

```json
{
  "type": "RoomLeft"
}
```

and tells **Bob** that Alice is gone:

```json
{
  "type": "PlayerLeft",
  "data": {
    "player_id": "00000000-0000-0000-0000-00000000000a"
  }
}
```

Next: Alice's client returns to its main menu. Bob is now alone in the room; depending on the game he may wait for
another player to join by code, or leave himself with his own `LeaveRoom`.
