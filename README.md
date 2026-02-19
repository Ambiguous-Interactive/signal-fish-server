<p align="center">
  <a href="https://ambiguous-interactive.github.io/signal-fish-server/">
    <img src="docs/assets/logo-banner.svg" alt="Signal Fish Server" width="600">
  </a>
</p>

A lightweight, zero-dependency WebSocket signaling server for peer-to-peer
game networking. Run locally with Rust or Docker -- no database, no cloud
services required.

Built by [Ambiguous Interactive](https://github.com/Ambiguous-Interactive).

## Quick Start

### Rust

```bash
cargo run
```

The server starts on port 3536 by default.

### Docker

```bash
docker run -p 3536:3536 ghcr.io/ambiguousinteractive/signal-fish-server:latest
```

### Docker Compose

```bash
docker compose up
```

### Connect

Point your WebSocket client at:

```text
ws://localhost:3536/v2/ws
```

## Features

- **Room management** -- create and join rooms with auto-generated 6-character room codes
- **Lobby state machine** -- waiting, countdown, and playing states with automatic transitions
- **Player ready-state** -- per-player ready toggles that drive lobby state progression
- **Authority management** -- request and grant game authority to specific players
- **Spectator mode** -- join rooms as a spectator without participating in gameplay
- **Reconnection** -- token-based reconnection with event replay within a configurable window
- **Message batching** -- configurable batching for high-throughput game data delivery
- **Rate limiting** -- in-memory rate limiting for room creation and join attempts
- **Metrics** -- Prometheus-compatible metrics at `/metrics/prom` and JSON metrics at `/metrics`
- **Flexible configuration** -- JSON config file with environment variable overrides
- **Optional authentication** -- config-file-backed app authentication with per-app rate limits
- **Zero external dependencies** -- everything runs in-memory; no database, no message broker, no cloud services

## Endpoints

| Path               | Method    | Description                                |
| ------------------ | --------- | ------------------------------------------ |
| `/v2/ws`           | WebSocket | Signaling WebSocket endpoint               |
| `/v2/health`       | GET       | Health check (returns 200 OK)              |
| `/metrics`         | GET       | JSON server metrics                        |
| `/v1/metrics`      | GET       | JSON server metrics (alias)                |
| `/metrics/prom`    | GET       | Prometheus text format metrics             |
| `/v1/metrics/prom` | GET       | Prometheus text format metrics (alias)     |

## Configuration

Signal Fish Server is configured through a JSON config file and environment variable overrides.
On startup the server looks for `config.json` in the working directory. See
[`config.example.json`](config.example.json) for a complete reference with all default values.

### Example Configuration

```json
{
  "port": 3536,
  "server": {
    "default_max_players": 8,
    "ping_timeout": 30,
    "room_cleanup_interval": 60,
    "max_rooms_per_game": 1000,
    "empty_room_timeout": 300,
    "inactive_room_timeout": 3600,
    "reconnection_window": 300,
    "event_buffer_size": 100,
    "enable_reconnection": true,
    "heartbeat_throttle_secs": 30,
    "region_id": "default"
  },
  "rate_limit": {
    "max_room_creations": 5,
    "time_window": 60,
    "max_join_attempts": 20
  },
  "protocol": {
    "max_game_name_length": 64,
    "room_code_length": 6,
    "max_player_name_length": 32,
    "max_players_limit": 100,
    "enable_message_pack_game_data": true
  },
  "logging": {
    "dir": "logs",
    "filename": "server.log",
    "rotation": "daily",
    "enable_file_logging": true,
    "format": "Json"
  },
  "security": {
    "cors_origins": "*",
    "require_websocket_auth": false,
    "require_metrics_auth": false,
    "max_message_size": 65536,
    "max_connections_per_ip": 10,
    "transport": {
      "tls": { "enabled": false },
      "token_binding": { "enabled": false }
    },
    "authorized_apps": [
      {
        "app_id": "my-game",
        "app_secret": "CHANGE_ME_BEFORE_PRODUCTION",
        "app_name": "My Awesome Game",
        "max_rooms": 100,
        "max_players_per_room": 16,
        "rate_limit_per_minute": 60
      }
    ]
  },
  "coordination": {
    "dedup_cache": {
      "capacity": 100000,
      "ttl_secs": 60,
      "cleanup_interval_secs": 30
    },
    "membership_snapshot_interval_secs": 30
  },
  "metrics": {
    "dashboard_cache_refresh_interval_secs": 5,
    "dashboard_cache_ttl_secs": 30,
    "dashboard_cache_history_window_secs": 300
  },
  "relay_types": {
    "default_relay_type": "matchbox",
    "game_relay_mappings": {}
  },
  "websocket": {
    "enable_batching": true,
    "batch_size": 10,
    "batch_interval_ms": 16,
    "auth_timeout_secs": 10
  }
}
```

### Environment Variable Overrides

Any configuration field can be overridden with an environment variable using the
`SIGNAL_FISH_` prefix. Nested fields use double underscores (`__`) as separators.
Values are parsed as the type expected by the corresponding config field.

| Environment Variable                             | Config Path                        | Default   | Description                                         |
| ------------------------------------------------ | ---------------------------------- | --------- | --------------------------------------------------- |
| `SIGNAL_FISH_PORT`                               | `port`                             | `3536`    | Server listen port                                  |
| `SIGNAL_FISH_SERVER__DEFAULT_MAX_PLAYERS`        | `server.default_max_players`       | `8`       | Default max players per room                        |
| `SIGNAL_FISH_SERVER__PING_TIMEOUT`               | `server.ping_timeout`              | `30`      | Seconds before a silent client is dropped           |
| `SIGNAL_FISH_SERVER__ROOM_CLEANUP_INTERVAL`      | `server.room_cleanup_interval`     | `60`      | Seconds between room cleanup sweeps                 |
| `SIGNAL_FISH_SERVER__MAX_ROOMS_PER_GAME`         | `server.max_rooms_per_game`        | `1000`    | Max rooms allowed per game name                     |
| `SIGNAL_FISH_SERVER__EMPTY_ROOM_TIMEOUT`         | `server.empty_room_timeout`        | `300`     | Seconds before an empty room is removed             |
| `SIGNAL_FISH_SERVER__INACTIVE_ROOM_TIMEOUT`      | `server.inactive_room_timeout`     | `3600`    | Seconds before an inactive room is removed          |
| `SIGNAL_FISH_SERVER__RECONNECTION_WINDOW`        | `server.reconnection_window`       | `300`     | Seconds a reconnection token stays valid            |
| `SIGNAL_FISH_SERVER__EVENT_BUFFER_SIZE`          | `server.event_buffer_size`         | `100`     | Max events buffered for reconnection replay         |
| `SIGNAL_FISH_SERVER__ENABLE_RECONNECTION`        | `server.enable_reconnection`       | `true`    | Enable reconnection support                         |
| `SIGNAL_FISH_SERVER__HEARTBEAT_THROTTLE_SECS`    | `server.heartbeat_throttle_secs`   | `30`      | Min seconds between heartbeat logs                  |
| `SIGNAL_FISH_SERVER__REGION_ID`                  | `server.region_id`                 | `default` | Region identifier for metrics                       |
| `SIGNAL_FISH_RATE_LIMIT__MAX_ROOM_CREATIONS`     | `rate_limit.max_room_creations`    | `5`       | Max room creations per IP per window                |
| `SIGNAL_FISH_RATE_LIMIT__TIME_WINDOW`            | `rate_limit.time_window`           | `60`      | Rate limit window in seconds                        |
| `SIGNAL_FISH_RATE_LIMIT__MAX_JOIN_ATTEMPTS`      | `rate_limit.max_join_attempts`     | `20`      | Max join attempts per IP per window                 |
| `SIGNAL_FISH_PROTOCOL__MAX_GAME_NAME_LENGTH`     | `protocol.max_game_name_length`    | `64`      | Max characters in a game name                       |
| `SIGNAL_FISH_PROTOCOL__ROOM_CODE_LENGTH`         | `protocol.room_code_length`        | `6`       | Length of generated room codes                      |
| `SIGNAL_FISH_PROTOCOL__MAX_PLAYER_NAME_LENGTH`   | `protocol.max_player_name_length`  | `32`      | Max characters in a player name                     |
| `SIGNAL_FISH_PROTOCOL__MAX_PLAYERS_LIMIT`        | `protocol.max_players_limit`       | `100`     | Hard ceiling on players per room                    |
| `SIGNAL_FISH_SECURITY__CORS_ORIGINS`             | `security.cors_origins`            | `*`       | Allowed CORS origins (comma-separated or `*`)       |
| `SIGNAL_FISH_SECURITY__REQUIRE_WEBSOCKET_AUTH`   | `security.require_websocket_auth`  | `false`   | Require app authentication on WebSocket connect     |
| `SIGNAL_FISH_SECURITY__REQUIRE_METRICS_AUTH`     | `security.require_metrics_auth`    | `false`   | Require auth token for metrics endpoints            |
| `SIGNAL_FISH_SECURITY__MAX_MESSAGE_SIZE`         | `security.max_message_size`        | `65536`   | Max WebSocket message size in bytes                 |
| `SIGNAL_FISH_SECURITY__MAX_CONNECTIONS_PER_IP`   | `security.max_connections_per_ip`  | `10`      | Max concurrent connections from one IP              |
| `SIGNAL_FISH_WEBSOCKET__ENABLE_BATCHING`         | `websocket.enable_batching`        | `true`    | Enable outbound message batching                    |
| `SIGNAL_FISH_WEBSOCKET__BATCH_SIZE`              | `websocket.batch_size`             | `10`      | Max messages per batch                              |
| `SIGNAL_FISH_WEBSOCKET__BATCH_INTERVAL_MS`       | `websocket.batch_interval_ms`      | `16`      | Batch flush interval in milliseconds                |
| `SIGNAL_FISH_WEBSOCKET__AUTH_TIMEOUT_SECS`       | `websocket.auth_timeout_secs`      | `10`      | Seconds to wait for auth after connect              |
| `RUST_LOG`                                       | --                                 | `info`    | Standard `tracing` log filter                       |

### CLI Flags

```text
signal-fish-server [OPTIONS]

Options:
      --validate-config    Validate config and exit
      --print-config       Print resolved config as JSON and exit
  -h, --help               Print help
  -V, --version            Print version
```

Note: The server automatically loads `config.json` from the working
directory if it exists. Use environment variables to override specific
configuration values.

## Protocol Reference

Signal Fish Server uses a JSON-based WebSocket protocol (v2). Messages are JSON
objects with a `type` field and an optional `data` field. MessagePack encoding
is also supported for game data when `enable_message_pack_game_data` is enabled.

### Client Messages

```json
{"type": "Authenticate", "data": {"app_id": "...", "app_secret": "..."}}
{"type": "CreateRoom", "data": {"game_name": "...", "max_players": 8}}
{"type": "JoinRoom", "data": {"game_name": "...", "room_code": "ABC123"}}
{"type": "GameData", "data": {"action": "move", "x": 10}}
{"type": "AuthorityRequest", "data": {"become_authority": true}}
{"type": "SetReady", "data": {"ready": true}}
{"type": "LeaveRoom"}
{"type": "Ping"}
```

| Message            | Description                                                                      |
| ------------------ | -------------------------------------------------------------------------------- |
| `Authenticate`     | Authenticate with app credentials (required when auth is enabled)                |
| `CreateRoom`       | Create a new room for the given game name                                        |
| `JoinRoom`         | Join an existing room by game name and room code                                 |
| `GameData`         | Send arbitrary game data to other players in the room                            |
| `AuthorityRequest` | Request or release game authority                                                |
| `SetReady`         | Toggle ready state (drives lobby state transitions)                              |
| `LeaveRoom`        | Leave the current room                                                           |
| `Ping`             | Heartbeat ping (server responds with `Pong`)                                     |

### Server Messages

```json
{"type": "Authenticated", "data": {"server_version": "2.0.0"}}
{"type": "RoomCreated", "data": {"room_id": "...", "room_code": "ABC123"}}
{"type": "RoomJoined", "data": {"room_id": "...", "room_code": "ABC123"}}
{"type": "PlayerJoined", "data": {"player": {"id": "...", "name": "..."}}}
{"type": "PlayerLeft", "data": {"player_id": "..."}}
{"type": "GameData", "data": {"from_player": "...", "data": {}}}
{"type": "LobbyStateChanged", "data": {"state": "Playing"}}
{"type": "AuthorityGranted", "data": {"player_id": "..."}}
{"type": "Error", "data": {"reason": "Room is full", "code": "ROOM_FULL"}}
{"type": "Pong"}
```

| Message              | Description                                              |
| -------------------- | -------------------------------------------------------- |
| `Authenticated`      | Auth succeeded; includes server version                  |
| `RoomCreated`        | Room created successfully; includes room ID and code     |
| `RoomJoined`         | Successfully joined a room                               |
| `PlayerJoined`       | Another player joined the room                           |
| `PlayerLeft`         | A player left the room                                   |
| `GameData`           | Game data relayed from another player                    |
| `LobbyStateChanged`  | Lobby state transitioned (Waiting, Countdown, Playing)   |
| `AuthorityGranted`   | Authority was granted to a player                        |
| `Error`              | An error occurred; includes reason and error code        |
| `Pong`               | Response to a client `Ping`                              |

### Typical Session Flow

```text
Client                              Server
  |                                    |
  |--- Authenticate ------------------>|
  |<-- Authenticated ------------------|
  |                                    |
  |--- CreateRoom -------------------->|
  |<-- RoomCreated --------------------|
  |                                    |
  |         (other client joins)       |
  |<-- PlayerJoined -------------------|
  |                                    |
  |--- SetReady ---------------------->|
  |<-- LobbyStateChanged (Playing) ----|
  |                                    |
  |--- GameData ---------------------->|
  |<-- GameData (from other player) ---|
  |                                    |
  |--- LeaveRoom --------------------->|
  |<-- PlayerLeft ---------------------|
```

## Optional Features

Signal Fish Server supports two optional Cargo features that are disabled by
default to keep the dependency tree minimal.

### `legacy-fullmesh`

Enables the upstream [matchbox](https://github.com/johanhelsing/matchbox)
full-mesh signaling mode. When activated, set `MATCHBOX_ENHANCED_MODE=false` to
run in legacy mode. The legacy signaling server listens on port+1.

```bash
cargo run --features legacy-fullmesh
```

### `tls`

Adds built-in TLS and mutual TLS (mTLS) support via
[rustls](https://github.com/rustls/rustls). When enabled, configure TLS
through the `security.transport.tls` section of the config file. Most
deployments should use a reverse proxy (nginx, Caddy, cloud load balancer)
instead of built-in TLS.

```bash
cargo build --features tls
```

Build with all optional features:

```bash
cargo build --all-features
```

## Library Usage

Signal Fish Server is published as both a binary (`signal-fish-server`) and a
library crate (`signal_fish_server`). You can embed the signaling server into
your own Rust application:

```rust
use signal_fish_server::{
    config,
    database::DatabaseConfig,
    server::{EnhancedGameServer, ServerConfig},
    websocket,
};
use std::{net::SocketAddr, sync::Arc};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load configuration from config.json + environment variables
    let cfg = Arc::new(config::load());

    // Build the server configuration (see main.rs for the full field mapping)
    let server_config = ServerConfig {
        default_max_players: cfg.server.default_max_players,
        ..Default::default()
    };

    // Create the game server with in-memory storage
    let game_server = EnhancedGameServer::new(
        server_config,
        cfg.protocol.clone(),
        cfg.relay_types.clone(),
        DatabaseConfig::InMemory,
        cfg.metrics.clone(),
        cfg.auth.clone(),
        cfg.coordination.clone(),
        cfg.security.transport.clone(),
        cfg.security.authorized_apps.clone(),
    )
    .await?;

    // Start background cleanup task
    let cleanup = game_server.clone();
    tokio::spawn(async move { cleanup.cleanup_task().await });

    // Build the Axum router
    let router = websocket::create_router(&cfg.security.cors_origins)
        .with_state(game_server);

    // Start listening
    let addr = SocketAddr::from(([0, 0, 0, 0], cfg.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
```

The `GameDatabase` trait is public, so you can implement your own storage
backend if you need persistence beyond the built-in `InMemoryDatabase`.

## Building from Source

### Prerequisites

- Rust 1.88.0 or later (see `rust-version` in `Cargo.toml`)
- No system libraries required for the default build

### Build

```bash
# Debug build
cargo build

# Release build (optimized, stripped)
cargo build --release

# With all optional features
cargo build --release --all-features
```

### Test

```bash
cargo test
cargo test --all-features
```

### Lint

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --all-features -- -D warnings
```

### Docker

```bash
# Build the image
docker build -t signal-fish-server .

# Run it
docker run -p 3536:3536 signal-fish-server

# With a custom config
docker run -p 3536:3536 -v ./config.json:/app/config.json:ro signal-fish-server

# Verify it is running
curl http://localhost:3536/v2/health
```

The Docker image uses a multi-stage build with `cargo-chef` for dependency
caching and `mold` for fast linking. The final runtime image is based on
`debian:bookworm-slim` and runs as a non-root user.

## Project Structure

```text
signal-fish-server/
├── src/
│   ├── main.rs                  # Binary entry point
│   ├── lib.rs                   # Library crate root
│   ├── server.rs                # EnhancedGameServer core
│   ├── broadcast.rs             # Zero-copy broadcast primitives
│   ├── distributed.rs           # In-memory distributed locking
│   ├── logging.rs               # tracing-subscriber initialization
│   ├── metrics.rs               # Atomic counters + HDR histograms
│   ├── rate_limit.rs            # In-memory rate limiter
│   ├── reconnection.rs          # Token-based reconnection manager
│   ├── retry.rs                 # Exponential backoff utility
│   ├── rkyv_utils.rs            # Zero-copy serialization helpers
│   ├── auth/                    # In-memory app authentication
│   ├── config/                  # JSON + env var configuration
│   ├── coordination/            # Room coordination and dedup cache
│   ├── database/                # GameDatabase trait + InMemoryDatabase
│   ├── protocol/                # Message types, room state, error codes
│   ├── security/                # TLS (optional) and crypto utilities
│   ├── server/                  # Room service, messaging, authority, etc.
│   └── websocket/               # WebSocket handler, routes, batching
├── tests/                       # Integration, e2e, concurrency, load tests
├── benches/                     # Criterion benchmarks
├── third_party/rmp/             # Patched rmp crate (removes paste dep)
├── Cargo.toml
├── Dockerfile
├── docker-compose.yml
├── config.example.json
└── clippy.toml
```

## Authentication

Authentication is **disabled by default**. To enable it, set
`security.require_websocket_auth` to `true` in your config file and add
entries to the `security.authorized_apps` array.

When authentication is enabled, clients must send an `Authenticate` message
immediately after connecting. The server validates the `app_id` and
`app_secret` against the configured authorized apps. Per-app rate limiting
is enforced based on the `rate_limit_per_minute` field.

```json
{
  "security": {
    "require_websocket_auth": true,
    "authorized_apps": [
      {
        "app_id": "my-game",
        "app_secret": "a-strong-secret-here",
        "app_name": "My Game",
        "max_rooms": 100,
        "max_players_per_room": 16,
        "rate_limit_per_minute": 60
      }
    ]
  }
}
```

**Important:** Change the default `app_secret` value before deploying to
production. The example value `CHANGE_ME_BEFORE_PRODUCTION` in
`config.example.json` is intentionally insecure.

## MSRV

The minimum supported Rust version is **1.88.0**.

## License

MIT -- [Ambiguous Interactive](https://github.com/Ambiguous-Interactive)

See [LICENSE](LICENSE) for the full license text.
