# Configuration

Signal Fish Server uses a JSON config file with environment variable overrides.

## Config File

On startup, the server looks for `config.json` in the working directory.

See [`config.example.json`](../config.example.json) for all available options.

## Essential Settings

### Port

```json
{
  "port": 3536
}
```

Environment override:

```bash
SIGNAL_FISH_PORT=8080 cargo run
```

### Max Players

```json
{
  "server": {
    "default_max_players": 8
  }
}
```

Environment override:

```bash
SIGNAL_FISH_SERVER__DEFAULT_MAX_PLAYERS=16 cargo run
```

### Room Limits

```json
{
  "server": {
    "max_rooms_per_game": 1000,
    "empty_room_timeout": 300,
    "inactive_room_timeout": 3600
  }
}
```

- `max_rooms_per_game` - Maximum concurrent rooms per game name
- `empty_room_timeout` - Seconds before an empty room is cleaned up (default: 300)
- `inactive_room_timeout` - Seconds before an inactive room is removed (default: 3600)

### Reconnection

```json
{
  "server": {
    "enable_reconnection": true,
    "reconnection_window": 300,
    "event_buffer_size": 100
  }
}
```

- `enable_reconnection` - Enable token-based reconnection (default: true)
- `reconnection_window` - Seconds a reconnection token stays valid (default: 300)
- `event_buffer_size` - Max events buffered for replay (default: 100)

## Environment Variable Format

All config fields use the `SIGNAL_FISH_` prefix. Nested fields use double underscores (`__`).

Examples:

```bash
# Top-level field
SIGNAL_FISH_PORT=3536

# Nested field: server.default_max_players
SIGNAL_FISH_SERVER__DEFAULT_MAX_PLAYERS=8

# Nested field: rate_limit.max_room_creations
SIGNAL_FISH_RATE_LIMIT__MAX_ROOM_CREATIONS=10
```

## Configuration Reference

Complete reference of all configuration options with environment variable overrides:

| Environment Variable                                | Config Path                                  | Default   | Description                              |
| --------------------------------------------------- | -------------------------------------------- | --------- | ---------------------------------------- |
| `SIGNAL_FISH_PORT`                                  | `port`                                       | `3536`    | Server listen port                       |
| `SIGNAL_FISH_SERVER__DEFAULT_MAX_PLAYERS`            | `server.default_max_players`                 | `8`       | Default max players per room             |
| `SIGNAL_FISH_SERVER__PING_TIMEOUT`                   | `server.ping_timeout`                        | `30`      | Seconds before a silent client is dropped |
| `SIGNAL_FISH_SERVER__ROOM_CLEANUP_INTERVAL`          | `server.room_cleanup_interval`               | `60`      | Seconds between room cleanup sweeps      |
| `SIGNAL_FISH_SERVER__MAX_ROOMS_PER_GAME`             | `server.max_rooms_per_game`                  | `1000`    | Max rooms allowed per game name          |
| `SIGNAL_FISH_SERVER__EMPTY_ROOM_TIMEOUT`             | `server.empty_room_timeout`                  | `300`     | Seconds before an empty room is removed  |
| `SIGNAL_FISH_SERVER__INACTIVE_ROOM_TIMEOUT`          | `server.inactive_room_timeout`               | `3600`    | Seconds before an inactive room is removed |
| `SIGNAL_FISH_SERVER__RECONNECTION_WINDOW`            | `server.reconnection_window`                 | `300`     | Seconds a reconnection token stays valid |
| `SIGNAL_FISH_SERVER__EVENT_BUFFER_SIZE`              | `server.event_buffer_size`                   | `100`     | Max events buffered for reconnection replay |
| `SIGNAL_FISH_SERVER__ENABLE_RECONNECTION`            | `server.enable_reconnection`                 | `true`    | Enable reconnection support              |
| `SIGNAL_FISH_SERVER__HEARTBEAT_THROTTLE_SECS`        | `server.heartbeat_throttle_secs`             | `30`      | Min seconds between heartbeat logs       |
| `SIGNAL_FISH_SERVER__REGION_ID`                      | `server.region_id`                           | `default` | Region identifier for metrics            |
| `SIGNAL_FISH_RATE_LIMIT__MAX_ROOM_CREATIONS`         | `rate_limit.max_room_creations`              | `5`       | Max room creations per IP per window     |
| `SIGNAL_FISH_RATE_LIMIT__TIME_WINDOW`                | `rate_limit.time_window`                     | `60`      | Rate limit window in seconds             |
| `SIGNAL_FISH_RATE_LIMIT__MAX_JOIN_ATTEMPTS`          | `rate_limit.max_join_attempts`               | `20`      | Max join attempts per IP per window      |
| `SIGNAL_FISH_PROTOCOL__MAX_GAME_NAME_LENGTH`         | `protocol.max_game_name_length`              | `64`      | Max characters in a game name            |
| `SIGNAL_FISH_PROTOCOL__ROOM_CODE_LENGTH`             | `protocol.room_code_length`                  | `6`       | Length of generated room codes           |
| `SIGNAL_FISH_PROTOCOL__MAX_PLAYER_NAME_LENGTH`       | `protocol.max_player_name_length`            | `32`      | Max characters in a player name          |
| `SIGNAL_FISH_PROTOCOL__MAX_PLAYERS_LIMIT`            | `protocol.max_players_limit`                 | `100`     | Hard ceiling on players per room         |
| `SIGNAL_FISH_SECURITY__CORS_ORIGINS`                 | `security.cors_origins`                      | `*`       | Allowed CORS origins (comma-separated or `*`) |
| `SIGNAL_FISH_SECURITY__REQUIRE_WEBSOCKET_AUTH`       | `security.require_websocket_auth`            | `false`   | Require app authentication on WebSocket connect |
| `SIGNAL_FISH_SECURITY__REQUIRE_METRICS_AUTH`         | `security.require_metrics_auth`              | `false`   | Require auth token for metrics endpoints |
| `SIGNAL_FISH_SECURITY__MAX_MESSAGE_SIZE`             | `security.max_message_size`                  | `65536`   | Max WebSocket message size in bytes      |
| `SIGNAL_FISH_SECURITY__MAX_CONNECTIONS_PER_IP`       | `security.max_connections_per_ip`            | `10`      | Max concurrent connections from one IP   |
| `SIGNAL_FISH_WEBSOCKET__ENABLE_BATCHING`             | `websocket.enable_batching`                  | `true`    | Enable outbound message batching         |
| `SIGNAL_FISH_WEBSOCKET__BATCH_SIZE`                  | `websocket.batch_size`                       | `10`      | Max messages per batch                   |
| `SIGNAL_FISH_WEBSOCKET__BATCH_INTERVAL_MS`           | `websocket.batch_interval_ms`                | `16`      | Batch flush interval in milliseconds     |
| `SIGNAL_FISH_WEBSOCKET__AUTH_TIMEOUT_SECS`           | `websocket.auth_timeout_secs`                | `10`      | Seconds to wait for auth after connect   |
| `RUST_LOG`                                           | --                                           | `info`    | Standard `tracing` log filter            |

## Common Configurations

### Development

```json
{
  "port": 3536,
  "server": {
    "default_max_players": 8,
    "enable_reconnection": true
  },
  "logging": {
    "enable_file_logging": false
  },
  "security": {
    "cors_origins": "*",
    "require_websocket_auth": false
  }
}
```

### Production

```json
{
  "port": 3536,
  "server": {
    "default_max_players": 8,
    "empty_room_timeout": 180,
    "inactive_room_timeout": 1800
  },
  "rate_limit": {
    "max_room_creations": 5,
    "time_window": 60,
    "max_join_attempts": 20
  },
  "logging": {
    "enable_file_logging": true,
    "rotation": "daily"
  },
  "security": {
    "cors_origins": "https://yourgame.com",
    "require_websocket_auth": true,
    "max_connections_per_ip": 10
  }
}
```

## Rate Limiting

```json
{
  "rate_limit": {
    "max_room_creations": 5,
    "time_window": 60,
    "max_join_attempts": 20
  }
}
```

- `max_room_creations` - Max room creations per IP per time window
- `time_window` - Rate limit window in seconds
- `max_join_attempts` - Max join attempts per IP per time window

## Protocol Settings

```json
{
  "protocol": {
    "max_game_name_length": 64,
    "room_code_length": 6,
    "max_player_name_length": 32,
    "max_players_limit": 100,
    "enable_message_pack_game_data": true
  }
}
```

## WebSocket Settings

```json
{
  "websocket": {
    "enable_batching": true,
    "batch_size": 10,
    "batch_interval_ms": 16,
    "auth_timeout_secs": 10
  }
}
```

- `enable_batching` - Batch outbound messages for better throughput
- `batch_size` - Max messages per batch
- `batch_interval_ms` - Batch flush interval
- `auth_timeout_secs` - Seconds to wait for auth after connect

## Validation

Validate your config without starting the server:

```bash
cargo run -- --validate-config
```

Print the resolved config (with environment overrides):

```bash
cargo run -- --print-config
```

## Next Steps

- [Authentication](authentication.md) - Set up app authentication
- [Deployment](deployment.md) - Production deployment guide
