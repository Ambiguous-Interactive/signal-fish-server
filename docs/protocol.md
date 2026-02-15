# Protocol Reference

Signal Fish Server uses a JSON-based WebSocket protocol. All messages are JSON objects with a `type` field and optional `data` field.

MessagePack encoding is also supported for game data when `enable_message_pack_game_data` is enabled.

## Client Messages

### Authenticate

Authenticate with app credentials (required when auth is enabled). App ID is a public identifier that identifies the game application.

```json
{
  "type": "Authenticate",
  "data": {
    "app_id": "my-game"
  }
}
```

Optional fields:
- `sdk_version` - SDK version for debugging and analytics
- `platform` - Platform information (e.g., "unity", "godot", "unreal")
- `game_data_format` - Preferred game data encoding (defaults to JSON text frames)

### JoinRoom

Join or create a room for a specific game. If no `room_code` is provided, a new room will be created.

```json
{
  "type": "JoinRoom",
  "data": {
    "game_name": "my-game",
    "player_name": "Player1"
  }
}
```

Required fields:
- `game_name` - Name of the game
- `player_name` - Name for the player

Optional fields:
- `room_code` - Code of existing room to join (if not provided, creates new room)
- `max_players` - Maximum players for the room (only used when creating new room)
- `supports_authority` - Whether the room supports authority system (only used when creating new room)
- `relay_transport` - Preferred relay transport protocol (TCP, UDP, or Auto)

### GameData

Send arbitrary game data to other players in the room.

```json
{
  "type": "GameData",
  "data": {
    "action": "move",
    "x": 100,
    "y": 200
  }
}
```

The `data` field can be any JSON-serializable object.

### PlayerReady

Signal readiness to start the game in lobby. Drives lobby state transitions.

```json
{
  "type": "PlayerReady"
}
```

This message has no data payload.

### AuthorityRequest

Request or release game authority.

```json
{
  "type": "AuthorityRequest",
  "data": {
    "become_authority": true
  }
}
```

### LeaveRoom

Leave the current room.

```json
{
  "type": "LeaveRoom"
}
```

### Ping

Heartbeat ping. Server responds with `Pong`.

```json
{
  "type": "Ping"
}
```

### Reconnect

Reconnect to a room after disconnection using authentication token.

```json
{
  "type": "Reconnect",
  "data": {
    "player_id": "player-id",
    "room_id": "room-id",
    "auth_token": "token-string"
  }
}
```

The `auth_token` is provided in the `RoomJoined` response when initially joining a room.

### ProvideConnectionInfo

Provide connection info for P2P establishment.

```json
{
  "type": "ProvideConnectionInfo",
  "data": {
    "connection_info": {
      "type": "offer",
      "sdp": "..."
    }
  }
}
```

### JoinAsSpectator

Join a room as a spectator (read-only observer).

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

Required fields:
- `game_name` - Name of the game
- `room_code` - Code of the room to spectate
- `spectator_name` - Name for the spectator

### LeaveSpectator

Leave spectator mode.

```json
{
  "type": "LeaveSpectator"
}
```

This message has no data payload.

## Server Messages

### Authenticated

Authentication successful. Includes app information and rate limits.

```json
{
  "type": "Authenticated",
  "data": {
    "app_name": "my-game",
    "organization": "My Organization",
    "rate_limits": {
      "per_minute": 60,
      "per_hour": 3600,
      "per_day": 86400
    }
  }
}
```

Optional fields:
- `organization` - Organization name (if any)

### ProtocolInfo

SDK/protocol compatibility details advertised after authentication.

```json
{
  "type": "ProtocolInfo",
  "data": {
    "protocol_version": "2.0",
    "server_version": "1.0.0",
    "features": ["reconnection", "spectators", "authority"]
  }
}
```

### AuthenticationError

Authentication failed.

```json
{
  "type": "AuthenticationError",
  "data": {
    "error": "Invalid app_id",
    "error_code": "INVALID_CREDENTIALS"
  }
}
```

### RoomJoined

Successfully joined or created a room. This message is sent both when creating a new room and when joining an existing room.

```json
{
  "type": "RoomJoined",
  "data": {
    "room_id": "uuid-string",
    "room_code": "ABC123",
    "player_id": "your-player-id",
    "game_name": "my-game",
    "max_players": 8,
    "supports_authority": true,
    "current_players": [
      {
        "id": "player-id",
        "name": "Player 1",
        "is_authority": false,
        "is_ready": false,
        "connected_at": "2024-01-01T00:00:00Z"
      }
    ],
    "is_authority": false,
    "lobby_state": "Waiting",
    "ready_players": [],
    "relay_type": "WebRTC",
    "current_spectators": []
  }
}
```

Note: The server also provides an `auth_token` field (not shown above) that should be stored for reconnection purposes.

### PlayerJoined

Another player joined the room.

```json
{
  "type": "PlayerJoined",
  "data": {
    "player": {
      "id": "player-id",
      "name": "Player 2",
      "is_authority": false,
      "is_ready": false,
      "connected_at": "2024-01-01T00:00:00Z"
    }
  }
}
```

### PlayerLeft

A player left the room.

```json
{
  "type": "PlayerLeft",
  "data": {
    "player_id": "player-id"
  }
}
```

### RoomJoinFailed

Failed to join room.

```json
{
  "type": "RoomJoinFailed",
  "data": {
    "reason": "Room is full",
    "error_code": "ROOM_FULL"
  }
}
```

Note: The `error_code` field is optional.

### RoomLeft

Successfully left room.

```json
{
  "type": "RoomLeft"
}
```

This message has no data payload.

### GameData

Game data relayed from another player.

```json
{
  "type": "GameData",
  "data": {
    "from_player": "player-id",
    "data": {
      "action": "move",
      "x": 100,
      "y": 200
    }
  }
}
```

### GameDataBinary

Binary game data payload from another player. Uses bytes for zero-copy cloning during broadcast.

```json
{
  "type": "GameDataBinary",
  "data": {
    "from_player": "player-id",
    "encoding": "MessagePack",
    "payload": "<base64-encoded-bytes>"
  }
}
```

### LobbyStateChanged

Lobby state transitioned.

```json
{
  "type": "LobbyStateChanged",
  "data": {
    "lobby_state": "Playing",
    "ready_players": ["player-id-1", "player-id-2"],
    "all_ready": true
  }
}
```

Possible states:
- `Waiting` - Waiting for players
- `Countdown` - Ready countdown in progress
- `Playing` - Game in progress

### AuthorityChanged

Authority status changed in the room.

```json
{
  "type": "AuthorityChanged",
  "data": {
    "authority_player": "player-id",
    "you_are_authority": false
  }
}
```

The `authority_player` field can be `null` if no player currently has authority.

### AuthorityResponse

Authority request response.

```json
{
  "type": "AuthorityResponse",
  "data": {
    "granted": true,
    "reason": "Authority granted",
    "error_code": "ALREADY_AUTHORITY"
  }
}
```

Note: The `reason` and `error_code` fields are optional.

### GameStarting

Game is starting with peer connection information.

```json
{
  "type": "GameStarting",
  "data": {
    "peer_connections": [
      {
        "player_id": "player-id-1",
        "connection_info": {
          "type": "offer",
          "sdp": "..."
        }
      }
    ]
  }
}
```

### Error

An error occurred.

```json
{
  "type": "Error",
  "data": {
    "message": "Room is full",
    "error_code": "ROOM_FULL"
  }
}
```

Note: The `error_code` field is optional.

Common error codes:
- `ROOM_FULL` - Room has reached max players
- `ROOM_NOT_FOUND` - Room code does not exist
- `INVALID_GAME_NAME` - Game name validation failed
- `RATE_LIMITED` - Too many requests
- `AUTH_REQUIRED` - Authentication required
- `INVALID_CREDENTIALS` - Invalid app_id or app_secret

### Pong

Response to client `Ping`.

```json
{
  "type": "Pong"
}
```

### Reconnected

Reconnection successful. Includes current room state and missed events.

```json
{
  "type": "Reconnected",
  "data": {
    "room_id": "uuid-string",
    "room_code": "ABC123",
    "player_id": "your-player-id",
    "game_name": "my-game",
    "max_players": 8,
    "supports_authority": true,
    "current_players": [
      {
        "id": "player-id",
        "name": "Player 1",
        "is_authority": false,
        "is_ready": false,
        "connected_at": "2024-01-01T00:00:00Z"
      }
    ],
    "is_authority": false,
    "lobby_state": "Playing",
    "ready_players": ["player-id-1"],
    "relay_type": "WebRTC",
    "current_spectators": [],
    "missed_events": [
      {
        "type": "GameData",
        "data": {
          "from_player": "player-id",
          "data": {"action": "move"}
        }
      }
    ]
  }
}
```

### ReconnectionFailed

Reconnection failed.

```json
{
  "type": "ReconnectionFailed",
  "data": {
    "reason": "Invalid auth token",
    "error_code": "INVALID_TOKEN"
  }
}
```

### PlayerReconnected

Another player reconnected to the room.

```json
{
  "type": "PlayerReconnected",
  "data": {
    "player_id": "player-id"
  }
}
```

### SpectatorJoined

Successfully joined a room as spectator.

```json
{
  "type": "SpectatorJoined",
  "data": {
    "room_id": "uuid-string",
    "room_code": "ABC123",
    "spectator_id": "your-spectator-id",
    "game_name": "my-game",
    "current_players": [
      {
        "id": "player-id",
        "name": "Player 1",
        "is_authority": false,
        "is_ready": false,
        "connected_at": "2024-01-01T00:00:00Z"
      }
    ],
    "current_spectators": [
      {
        "id": "spectator-id",
        "name": "Observer1"
      }
    ],
    "lobby_state": "Playing",
    "reason": "joined"
  }
}
```

Note: The `reason` field is optional.

### SpectatorJoinFailed

Failed to join as spectator.

```json
{
  "type": "SpectatorJoinFailed",
  "data": {
    "reason": "Room not found",
    "error_code": "ROOM_NOT_FOUND"
  }
}
```

Note: The `error_code` field is optional.

### SpectatorLeft

Successfully left spectator mode.

```json
{
  "type": "SpectatorLeft",
  "data": {
    "room_id": "uuid-string",
    "room_code": "ABC123",
    "reason": "voluntary",
    "current_spectators": []
  }
}
```

Note: All fields are optional.

### NewSpectatorJoined

Another spectator joined the room.

```json
{
  "type": "NewSpectatorJoined",
  "data": {
    "spectator": {
      "id": "spectator-id",
      "name": "Observer2"
    },
    "current_spectators": [
      {
        "id": "spectator-id-1",
        "name": "Observer1"
      },
      {
        "id": "spectator-id-2",
        "name": "Observer2"
      }
    ],
    "reason": "joined"
  }
}
```

Note: The `reason` field is optional.

### SpectatorDisconnected

Another spectator left the room.

```json
{
  "type": "SpectatorDisconnected",
  "data": {
    "spectator_id": "spectator-id",
    "reason": "disconnected",
    "current_spectators": []
  }
}
```

Note: The `reason` field is optional.

## Session Flow

```
Client                              Server
  |                                    |
  |--- Authenticate ------------------>|
  |<-- Authenticated ------------------|
  |                                    |
  |--- JoinRoom (no room_code) ------->|
  |<-- RoomJoined ---------------------|
  |                                    |
  |         (other client joins)       |
  |<-- PlayerJoined -------------------|
  |                                    |
  |--- PlayerReady ------------------->|
  |<-- LobbyStateChanged (Playing) ----|
  |                                    |
  |--- GameData ---------------------->|
  |<-- GameData (from other player) ---|
  |                                    |
  |--- LeaveRoom --------------------->|
  |<-- RoomLeft -----------------------|
```

## Reconnection Flow

When a client initially joins a room, the server provides an `auth_token` in the `RoomJoined` response. This token should be stored by the client for reconnection purposes.

If the connection is lost, the client can reconnect using the stored information:

```json
{
  "type": "Reconnect",
  "data": {
    "player_id": "your-player-id",
    "room_id": "your-room-id",
    "auth_token": "stored-token"
  }
}
```

On successful reconnection, the server sends a `Reconnected` message with the current room state and any missed events that occurred during the disconnection.

## Next Steps

- [Getting Started](getting-started.md) - Basic usage examples
- [Features](features.md) - Complete feature overview
