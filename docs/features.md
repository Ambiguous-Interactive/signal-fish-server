# Features

Complete overview of Signal Fish Server capabilities.

## Room Management

### Automatic Room Codes

Rooms are identified by auto-generated 6-character codes (configurable length).

Create a room by joining without a room code:

```json
{
  "type": "JoinRoom",
  "data": {
    "game_name": "my-game",
    "player_name": "Player1",
    "max_players": 8
  }
}
```

Response:

```json
{
  "type": "RoomJoined",
  "data": {
    "room_code": "ABC123",
    "room_id": "uuid-string",
    "player_id": "your-player-id",
    "game_name": "my-game",
    "max_players": 8,
    "supports_authority": true,
    "current_players": [...],
    "is_authority": false,
    "lobby_state": "Waiting",
    "ready_players": [],
    "relay_type": "WebRTC",
    "current_spectators": []
  }
}
```

Players join using the room code:

```json
{
  "type": "JoinRoom",
  "data": {
    "game_name": "my-game",
    "room_code": "ABC123",
    "player_name": "Player2"
  }
}
```

### Room Limits

Configure per-game room limits:

```json
{
  "server": {
    "max_rooms_per_game": 1000
  }
}
```

When auth is enabled, per-app limits apply:

```json
{
  "security": {
    "authorized_apps": [
      {
        "app_id": "my-game",
        "max_rooms": 100,
        "max_players_per_room": 16
      }
    ]
  }
}
```

## Lobby State Machine

Rooms transition through three states based on player ready status:

### Waiting

Initial state. Waiting for players to join and mark ready.

### Countdown

All players are ready. Countdown to game start.

### Playing

Game in progress.

### State Transitions

```
Waiting --> Countdown (all players ready)
Countdown --> Waiting (player unready)
Countdown --> Playing (countdown complete)
Playing --> Waiting (manual reset or all players leave)
```

Clients are notified of state changes:

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

## Player Ready State

Players signal their ready state:

```json
{
  "type": "PlayerReady"
}
```

When all players are ready, the lobby transitions to Countdown, then Playing.

## Authority Management

Players can request game authority (e.g., for server-authoritative gameplay):

```json
{
  "type": "AuthorityRequest",
  "data": {
    "become_authority": true
  }
}
```

When granted, all players are notified:

```json
{
  "type": "AuthorityChanged",
  "data": {
    "authority_player": "player-id",
    "you_are_authority": false
  }
}
```

Only one player can hold authority at a time.

## Spectator Mode

Join rooms as a spectator without participating in gameplay:

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

Spectators:
- Don't count toward max_players
- Can't mark ready
- Receive all game data
- Don't participate in authority decisions

## Reconnection

Token-based reconnection with event replay.

### Initial Join

When a player joins a room, the server provides an authentication token in the `RoomJoined` response. This token should be stored by the client for reconnection purposes.

### Reconnecting

If the connection is lost, reconnect using the stored credentials:

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

### Event Replay

On successful reconnection, the server sends a `Reconnected` message with the current room state and replays any events that occurred during the disconnection window.

### Configuration

```json
{
  "server": {
    "enable_reconnection": true,
    "reconnection_window": 300,
    "event_buffer_size": 100
  }
}
```

## Message Batching

Batch outbound messages for improved throughput.

```json
{
  "websocket": {
    "enable_batching": true,
    "batch_size": 10,
    "batch_interval_ms": 16
  }
}
```

- `batch_size` - Max messages per batch
- `batch_interval_ms` - Max time to wait before flushing

Batching is transparent to clients.

## Rate Limiting

In-memory rate limiting for room creation and join attempts.

```json
{
  "rate_limit": {
    "max_room_creations": 5,
    "time_window": 60,
    "max_join_attempts": 20
  }
}
```

- `max_room_creations` - Max rooms per IP per time window
- `time_window` - Window duration in seconds
- `max_join_attempts` - Max join attempts per IP per window

When auth is enabled, per-app rate limits apply:

```json
{
  "security": {
    "authorized_apps": [
      {
        "app_id": "my-game",
        "rate_limit_per_minute": 60
      }
    ]
  }
}
```

## Metrics

### JSON Metrics

```bash
curl http://localhost:3536/metrics
```

Returns:

```json
{
  "active_rooms": 42,
  "active_players": 156,
  "total_rooms_created": 1024,
  "total_messages_sent": 50000,
  "uptime_seconds": 3600
}
```

### Prometheus Metrics

```bash
curl http://localhost:3536/metrics/prom
```

Returns Prometheus text format for scraping.

### Metrics Authentication

Protect metrics endpoints:

```json
{
  "security": {
    "require_metrics_auth": true
  }
}
```

Access with:

```bash
curl -H "Authorization: Bearer app_id:app_secret" \
  http://localhost:3536/metrics
```

## Authentication

Optional app-based authentication with per-app limits.

```json
{
  "security": {
    "require_websocket_auth": true,
    "authorized_apps": [
      {
        "app_id": "my-game",
        "app_secret": "secret-key",
        "max_rooms": 100,
        "max_players_per_room": 16,
        "rate_limit_per_minute": 60
      }
    ]
  }
}
```

See [Authentication](authentication.md) for full details.

## MessagePack Support

Enable MessagePack encoding for game data:

```json
{
  "protocol": {
    "enable_message_pack_game_data": true
  }
}
```

Game data messages can be sent in MessagePack format for reduced bandwidth.

## CORS Support

Configure allowed origins:

```json
{
  "security": {
    "cors_origins": "https://yourgame.com"
  }
}
```

Multiple origins (comma-separated):

```json
{
  "security": {
    "cors_origins": "https://game.com,https://beta.game.com"
  }
}
```

Allow all (development only):

```json
{
  "security": {
    "cors_origins": "*"
  }
}
```

## Connection Limits

Limit concurrent connections per IP:

```json
{
  "security": {
    "max_connections_per_ip": 10
  }
}
```

## Message Size Limits

Limit maximum WebSocket message size:

```json
{
  "security": {
    "max_message_size": 65536
  }
}
```

Messages exceeding this size are rejected.

## Room Cleanup

Automatic cleanup of empty and inactive rooms:

```json
{
  "server": {
    "room_cleanup_interval": 60,
    "empty_room_timeout": 300,
    "inactive_room_timeout": 3600
  }
}
```

- `room_cleanup_interval` - Seconds between cleanup sweeps
- `empty_room_timeout` - Seconds before empty room removal
- `inactive_room_timeout` - Seconds before inactive room removal

## Ping/Pong

Keep-alive mechanism to detect dead connections:

```json
{
  "server": {
    "ping_timeout": 30
  }
}
```

Clients should send periodic `Ping` messages. Server disconnects clients that are silent for longer than `ping_timeout`.

## Structured Logging

JSON-formatted structured logs for production observability:

```json
{
  "logging": {
    "enable_file_logging": true,
    "dir": "logs",
    "filename": "server.log",
    "rotation": "daily",
    "format": "Json"
  }
}
```

## Zero External Dependencies

Everything runs in-memory:
- No database required
- No message broker
- No cloud services
- No external runtime dependencies

Perfect for:
- Local development
- LAN games
- Self-hosted deployments
- Embedded systems

## Next Steps

- [Getting Started](getting-started.md) - Quick start guide
- [Protocol Reference](protocol.md) - Complete message documentation
- [Configuration](configuration.md) - Full configuration options
