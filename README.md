<p align="center">
  <img src="docs/assets/logo-banner.svg" alt="Signal Fish Server" width="600">
</p>

<p align="center">
  <a href="https://crates.io/crates/signal-fish-server">
    <img src="https://img.shields.io/crates/v/signal-fish-server?style=for-the-badge"
         alt="crates.io">
  </a>
  <a href="https://ambiguous-interactive.github.io/signal-fish-server/">
    <img src="https://img.shields.io/badge/docs-GitHub%20Pages-blue?style=for-the-badge"
         alt="Documentation">
  </a>
  <a href="rust-toolchain.toml">
    <img src="https://img.shields.io/badge/MSRV-1.89.0-blue.svg?style=for-the-badge"
         alt="MSRV">
  </a>
  <a href="LICENSE">
    <img src="https://img.shields.io/badge/license-MIT-blue.svg?style=for-the-badge"
         alt="License: MIT">
  </a>
</p>

A lightweight, zero-external-dependency WebSocket signaling server for
peer-to-peer game networking. Run locally with Rust or Docker -- no database, no cloud
services required.

Built by [Ambiguous Interactive](https://github.com/Ambiguous-Interactive).

---

> **🤖 AI Disclosure**
>
> This project was developed with **substantial AI assistance**. The protocol
> design and core technology concepts were created entirely by humans, but the
> vast majority of the code, documentation, and tests were written with the
> help of **Claude Opus 4.6** and **Codex 5.3**. Human oversight covered code
> review and architectural decisions, but day-to-day implementation was
> primarily AI-driven. This transparency is provided so users can make informed
> decisions about using this crate.

---

## Quick Start

### Rust

```bash
cargo run
```

The server starts on port 3536 by default.

### Docker

```bash
docker run -p 3536:3536 ghcr.io/ambiguous-interactive/signal-fish-server:latest
```

The published image is a multi-architecture manifest, so the same tag pulls the
right build automatically on `linux/amd64`, `linux/arm64` (AWS Graviton, Ampere,
Raspberry Pi 64-bit, Apple Silicon under Docker), and `linux/arm/v7` (32-bit
ARM / Raspberry Pi).

### Docker Compose

```bash
docker compose up
```

### Prebuilt binaries

Each [GitHub Release](https://github.com/Ambiguous-Interactive/signal-fish-server/releases)
ships standalone, self-contained executables — no Rust toolchain or Docker
required — for:

| OS | Architectures |
| --- | --- |
| Linux | x86_64, aarch64 (ARM64) |
| macOS | x86_64 (Intel), aarch64 (Apple Silicon) |
| Windows | x86_64, aarch64 (ARM64) |

Every archive is accompanied by a `.sha256` checksum for verification. Download
the archive for your platform, extract it, and run the `signal-fish-server`
binary.

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
- **Delivery classes** -- v3 reliable, keyed-latest, and volatile JSON relay with exact gap accountability
- **Rate limiting** -- in-memory rate limiting for room creation and join attempts
- **Metrics** -- Prometheus-compatible metrics at `/metrics/prom` and JSON metrics at `/metrics`
- **Flexible configuration** -- JSON config file with environment variable overrides
- **Optional authentication** -- config-file-backed app authentication with per-app rate limits
- **Zero external dependencies** -- everything runs in-memory; no database, no message broker, no cloud services

## Endpoints

| Path               | Method    | Description                                |
| ------------------ | --------- | ------------------------------------------ |
| `/v2/ws`           | WebSocket | Signaling WebSocket endpoint               |
| `/v3/ws`           | WebSocket | Protocol v3 WebSocket alias                |
| `/v2/health`       | GET       | Health check (returns 200 OK)              |
| `/metrics`         | GET       | JSON server metrics                        |
| `/v1/metrics`      | GET       | JSON server metrics (alias)                |
| `/metrics/prom`    | GET       | Prometheus text format metrics             |
| `/v1/metrics/prom` | GET       | Prometheus text format metrics (alias)     |

## Configuration

Signal Fish Server is configured through a JSON config file and environment variable overrides.
On startup the server looks for `config.json` in the working directory. See
[`config.example.json`](config.example.json) for a complete local-development reference.

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
    "max_join_attempts": 20,
    "max_signals": 600,
    "max_signal_errors": 60
  },
  "protocol": {
    "max_game_name_length": 64,
    "room_code_length": 6,
    "max_player_name_length": 32,
    "max_players_limit": 100,
    "enable_message_pack_game_data": true,
    "min_protocol_version": 2,
    "max_protocol_version": 3
  },
  "logging": {
    "dir": "logs",
    "filename": "server.log",
    "rotation": "daily",
    "enable_file_logging": true,
    "format": "json"
  },
  "security": {
    "cors_origins": "*",
    "require_websocket_auth": false,
    "require_metrics_auth": false,
    "max_message_size": 65536,
    "max_signal_bytes": 16384,
    "max_connections_per_ip": 24,
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
  "session": {
    "default_topology": "relay",
    "game_topology_mappings": {},
    "enable_webrtc": true,
    "enable_direct": true,
    "enable_ice_pregather": true,
    "ice_servers": []
  },
  "turn": {
    "enabled": false,
    "static_auth_secret": "",
    "urls": [],
    "stun_urls": ["stun:stun.l.google.com:19302"],
    "credential_ttl_secs": 3600
  },
  "websocket": {
    "enable_batching": false,
    "batch_size": 10,
    "batch_interval_ms": 16,
    "auth_timeout_secs": 10,
    "idle_timeout_secs": 300,
    "server_ping_interval_secs": 10,
    "pong_timeout_secs": 5,
    "socket_send_buffer_bytes": 65536,
    "send_queue_capacity": 1024,
    "control_queue_capacity": 128,
    "slow_consumer_timeout_ms": 5000,
    "max_sojourn_ms": 15000,
    "delivery_stats_interval_secs": 0
  }
}
```

### Environment Variable Overrides

Any configuration field can be overridden with an environment variable using the
`SIGNAL_FISH__` prefix. Nested fields use double underscores (`__`) as separators.
Values are parsed as the type expected by the corresponding config field. Common
overrides are listed below; see [Configuration](docs/configuration.md) for the
complete reference.

| Environment Variable                             | Config Path                        | Default   | Description                                         |
| ------------------------------------------------ | ---------------------------------- | --------- | --------------------------------------------------- |
| `SIGNAL_FISH__PORT`                               | `port`                             | `3536`    | Server listen port                                  |
| `SIGNAL_FISH__SERVER__DEFAULT_MAX_PLAYERS`        | `server.default_max_players`       | `8`       | Default max players per room                        |
| `SIGNAL_FISH__SERVER__PING_TIMEOUT`               | `server.ping_timeout`              | `30`      | Seconds before a silent client is dropped           |
| `SIGNAL_FISH__SERVER__ROOM_CLEANUP_INTERVAL`      | `server.room_cleanup_interval`     | `60`      | Seconds between room cleanup sweeps                 |
| `SIGNAL_FISH__SERVER__MAX_ROOMS_PER_GAME`         | `server.max_rooms_per_game`        | `1000`    | Max rooms allowed per game name                     |
| `SIGNAL_FISH__SERVER__EMPTY_ROOM_TIMEOUT`         | `server.empty_room_timeout`        | `300`     | Seconds before an empty room is removed             |
| `SIGNAL_FISH__SERVER__INACTIVE_ROOM_TIMEOUT`      | `server.inactive_room_timeout`     | `3600`    | Seconds before an inactive room is removed          |
| `SIGNAL_FISH__SERVER__RECONNECTION_WINDOW`        | `server.reconnection_window`       | `300`     | Seconds a reconnection token stays valid            |
| `SIGNAL_FISH__SERVER__EVENT_BUFFER_SIZE`          | `server.event_buffer_size`         | `100`     | Max events buffered for reconnection replay         |
| `SIGNAL_FISH__SERVER__ENABLE_RECONNECTION`        | `server.enable_reconnection`       | `true`    | Enable reconnection support                         |
| `SIGNAL_FISH__SERVER__HEARTBEAT_THROTTLE_SECS`    | `server.heartbeat_throttle_secs`   | `30`      | Min seconds between `last_seen` heartbeat writes    |
| `SIGNAL_FISH__SERVER__REGION_ID`                  | `server.region_id`                 | `default` | Region identifier for metrics                       |
| `SIGNAL_FISH__RATE_LIMIT__MAX_ROOM_CREATIONS`     | `rate_limit.max_room_creations`    | `5`       | Max room creations per player per window            |
| `SIGNAL_FISH__RATE_LIMIT__TIME_WINDOW`            | `rate_limit.time_window`           | `60`      | Rate limit window in seconds                        |
| `SIGNAL_FISH__RATE_LIMIT__MAX_JOIN_ATTEMPTS`      | `rate_limit.max_join_attempts`     | `20`      | Max join attempts per player per window             |
| `SIGNAL_FISH__RATE_LIMIT__MAX_SIGNALS`            | `rate_limit.max_signals`           | `600`     | Max validated WebRTC Signal dispatch attempts per player per window |
| `SIGNAL_FISH__RATE_LIMIT__MAX_SIGNAL_ERRORS`      | `rate_limit.max_signal_errors`     | `60`      | Max rejected WebRTC signals per player per window   |
| `SIGNAL_FISH__PROTOCOL__MAX_GAME_NAME_LENGTH`     | `protocol.max_game_name_length`    | `64`      | Max characters in a game name                       |
| `SIGNAL_FISH__PROTOCOL__ROOM_CODE_LENGTH`         | `protocol.room_code_length`        | `6`       | Length of generated room codes                      |
| `SIGNAL_FISH__PROTOCOL__MAX_PLAYER_NAME_LENGTH`   | `protocol.max_player_name_length`  | `32`      | Max characters in a player name                     |
| `SIGNAL_FISH__PROTOCOL__MAX_PLAYERS_LIMIT`        | `protocol.max_players_limit`       | `100`     | Hard ceiling on players per room                    |
| `SIGNAL_FISH__PROTOCOL__ENABLE_MESSAGE_PACK_GAME_DATA` | `protocol.enable_message_pack_game_data` | `true` | Enable MessagePack game-data frames                 |
| `SIGNAL_FISH__PROTOCOL__MIN_PROTOCOL_VERSION`     | `protocol.min_protocol_version`    | `2`       | Lowest accepted protocol version                    |
| `SIGNAL_FISH__PROTOCOL__MAX_PROTOCOL_VERSION`     | `protocol.max_protocol_version`    | `3`       | Highest negotiated protocol version                 |
| `SIGNAL_FISH__LOGGING__LEVEL`                     | `logging.level`                    | `null`    | Log level override (`trace`, `debug`, `info`, `warn`, `error`) |
| `SIGNAL_FISH__LOGGING__ENABLE_FILE_LOGGING`       | `logging.enable_file_logging`      | `true`    | Enable rolling file logs                            |
| `SIGNAL_FISH__LOGGING__FORMAT`                    | `logging.format`                   | `json`    | Log output format (`json` or `text`)                |
| `SIGNAL_FISH__SECURITY__CORS_ORIGINS`             | `security.cors_origins`            | `http://localhost:3000,http://localhost:5173` | Allowed CORS origins (comma-separated or `*`)       |
| `SIGNAL_FISH__SECURITY__REQUIRE_WEBSOCKET_AUTH`   | `security.require_websocket_auth`  | `true`    | Require app authentication on WebSocket connect     |
| `SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH`     | `security.require_metrics_auth`    | `true`    | Require auth token for metrics endpoints            |
| `SIGNAL_FISH__SECURITY__MAX_MESSAGE_SIZE`         | `security.max_message_size`        | `65536`   | Max WebSocket message size in bytes                 |
| `SIGNAL_FISH__SECURITY__MAX_SIGNAL_BYTES`         | `security.max_signal_bytes`        | `16384`   | Max serialized size of a v3 `Signal` payload        |
| `SIGNAL_FISH__SECURITY__MAX_CONNECTIONS_PER_IP`   | `security.max_connections_per_ip`  | `24`      | Max concurrent connections from one IP              |
| `SIGNAL_FISH__WEBSOCKET__ENABLE_BATCHING`         | `websocket.enable_batching`        | `false`   | Opt-in outbound batching (off keeps relay latency low) |
| `SIGNAL_FISH__WEBSOCKET__BATCH_SIZE`              | `websocket.batch_size`             | `10`      | Max messages per batch                              |
| `SIGNAL_FISH__WEBSOCKET__BATCH_INTERVAL_MS`       | `websocket.batch_interval_ms`      | `16`      | Batch flush interval in milliseconds                |
| `SIGNAL_FISH__WEBSOCKET__AUTH_TIMEOUT_SECS`       | `websocket.auth_timeout_secs`      | `10`      | Seconds to wait for auth after connect              |
| `SIGNAL_FISH__WEBSOCKET__IDLE_TIMEOUT_SECS`       | `websocket.idle_timeout_secs`      | `300`     | Post-auth idle timeout in seconds (`0` disables)    |
| `SIGNAL_FISH__WEBSOCKET__SERVER_PING_INTERVAL_SECS` | `websocket.server_ping_interval_secs` | `10` | Server RFC 6455 Ping cadence (`0` disables)       |
| `SIGNAL_FISH__WEBSOCKET__PONG_TIMEOUT_SECS`       | `websocket.pong_timeout_secs`      | `5`       | Matching Pong deadline before close `4003`          |
| `SIGNAL_FISH__WEBSOCKET__SOCKET_SEND_BUFFER_BYTES` | `websocket.socket_send_buffer_bytes` | `65536` | TCP handoff bound ahead of control (`0` = OS default) |
| `SIGNAL_FISH__WEBSOCKET__SEND_QUEUE_CAPACITY`     | `websocket.send_queue_capacity`    | `1024`    | Per-connection data queue capacity                  |
| `SIGNAL_FISH__WEBSOCKET__CONTROL_QUEUE_CAPACITY`  | `websocket.control_queue_capacity` | `128`     | Per-connection v3 control queue capacity            |
| `SIGNAL_FISH__WEBSOCKET__SLOW_CONSUMER_TIMEOUT_MS` | `websocket.slow_consumer_timeout_ms` | `5000`  | Reliable capacity wait before close `4002`          |
| `SIGNAL_FISH__WEBSOCKET__MAX_SOJOURN_MS`          | `websocket.max_sojourn_ms`         | `15000`   | Reliable/control sojourn and socket-write deadline  |
| `SIGNAL_FISH__WEBSOCKET__DELIVERY_STATS_INTERVAL_SECS` | `websocket.delivery_stats_interval_secs` | `0` | Periodic v3 counter snapshots (`0` disables)        |
| `RUST_LOG`                                       | --                                 | `info`    | Standard `tracing` log filter                       |

### CLI Flags

```text
signal-fish-server [OPTIONS]

Options:
  -c, --validate-config    Validate config and exit
      --print-config       Print resolved config as JSON and exit
  -h, --help               Print help
  -V, --version            Print version
```

Note: The server automatically loads `config.json` from the working
directory if it exists. Use environment variables to override specific
configuration values.

## Protocol Reference

Signal Fish Server uses a JSON-based WebSocket protocol with a frozen v2 relay
floor and additive v3 capabilities. Messages are JSON objects with a `type`
field and an optional `data` field. MessagePack encoding is also supported for
game data when `enable_message_pack_game_data` is enabled.

Building a client? See the [Platform Integration Guide](docs/guides/platform-integration.md)
for which WebRTC stack to use per platform (browser, native, mobile, Steam, Godot,
Unity, Unreal), and the [Rust Client Guide](docs/guides/rust-client.md) for a complete
relay-floor client walkthrough.

### Client Messages

Canonical sample: [.llm/code-samples/protocol/v2-client-messages.jsonl](.llm/code-samples/protocol/v2-client-messages.jsonl)
(v3 additions: [.llm/code-samples/protocol/v3-client-messages.jsonl](.llm/code-samples/protocol/v3-client-messages.jsonl))

| Message            | Description                                                                      |
| ------------------ | -------------------------------------------------------------------------------- |
| `Authenticate`     | Authenticate with app ID (required when auth is enabled)                         |
| `JoinRoom`         | Join or create a room (implicit create with no `room_code`, explicit join/create with `room_code`) |
| `GameData`         | Send arbitrary game data; negotiated v3 JSON may select reliable, keyed-latest, or volatile delivery |
| `AuthorityRequest` | Request or release game authority                                                |
| `PlayerReady`      | Toggle your ready/unready state in the lobby                                     |
| `ProvideConnectionInfo` | Share legacy self-declared metadata for v2/back-compat; not v3 capability proof |
| `Reconnect`        | Reconnect after disconnect using `player_id`, `room_id`, and `auth_token`        |
| `JoinAsSpectator`  | Join a room as a spectator (read-only observer)                                  |
| `LeaveSpectator`   | Leave spectator mode                                                             |
| `LeaveRoom`        | Leave the current room                                                           |
| `Ping`             | Heartbeat ping (server responds with `Pong`)                                     |

#### `JoinRoom` Behavior (Implicit vs Explicit)

`JoinRoom` is the only room-entry message. The server resolves it using
`game_name` and `room_code`:

1. Omit `room_code`: create a new room with a generated room code.
2. Provide `room_code` and room exists for that `game_name`: join that room.
3. Provide `room_code` and no room exists for that `game_name`: create a new
   room with that room code.

In all successful cases, the caller receives `RoomJoined`. When joining an
existing room, current members also receive `PlayerJoined`.

#### `PlayerReady` Behavior

`PlayerReady` has no payload and works as a toggle:

1. Send once in `lobby` state: you become ready.
2. Send again in `lobby` state: you become unready.
3. Each toggle broadcasts `LobbyStateChanged` with `ready_players` and
   `all_ready`.
4. When `all_ready` becomes `true`, the server sends `GameStarting`.

### Server Messages

Canonical sample: [.llm/code-samples/protocol/v2-server-messages.jsonl](.llm/code-samples/protocol/v2-server-messages.jsonl)
(v3 additions: [.llm/code-samples/protocol/v3-server-messages.jsonl](.llm/code-samples/protocol/v3-server-messages.jsonl))

| Message              | Description                                              |
| -------------------- | -------------------------------------------------------- |
| `Authenticated`      | Auth succeeded; includes app metadata and rate limits    |
| `ProtocolInfo`       | SDK/protocol compatibility details and capabilities      |
| `RoomJoined`         | Successfully joined a room                               |
| `PlayerJoined`       | Another player joined the room                           |
| `PlayerLeft`         | A player left the room                                   |
| `GameData`           | Game data relayed from another player                    |
| `DeliveryReport`     | V3 cumulative per-class outcomes and exact prior gap ranges |
| `LobbyStateChanged`  | Lobby state transitioned (`waiting`, `lobby`, `finalized`) |
| `AuthorityResponse`  | Authority request result                                 |
| `Error`              | An error occurred; includes message and optional code    |
| `Pong`               | Response to a client `Ping`                              |

Delivery class policy is per recipient. A classified v3 send can use
latest/volatile policy for a v3 peer while the same message remains reliable
FIFO, with v3 fields stripped, for a v2 peer. Raw binary game data is always
reliable. On a continuing v3 stream, only the union of causally prior exact
`DeliveryReport` ranges authorizes a sequence hole; aggregate counters do not.

Operators can inspect raw JSON
`metricsSnapshot.connections.delivery_by_class` from
`/metrics?includeSnapshot=true`, or Prometheus
`signal_fish_websocket_delivery_class_outcomes_total{class,outcome}`. At
quiescence, each class's `attempted` value equals the sum of its terminal
outcomes. See [Delivery semantics](docs/protocol.md#delivery-semantics).

### Typical Session Flow

```text
Client                              Server
  |                                    |
  |--- Authenticate ------------------>|
  |<-- Authenticated ------------------|
  |<-- ProtocolInfo -------------------|
  |                                    |
  |--- JoinRoom (no room_code) ------->|
  |<-- RoomJoined ---------------------|
  |                                    |
  |         (other client joins)       |
  |<-- PlayerJoined -------------------|
  |                                    |
  |--- PlayerReady ------------------->|
  |<-- LobbyStateChanged (lobby) ------|
  |                                    |
  |--- GameData ---------------------->|
  |<-- GameData (from other player) ---|
  |                                    |
  |--- LeaveRoom --------------------->|
  |<-- RoomLeft -----------------------|
  |                                    |
  |        (other clients receive      |
  |         PlayerLeft)                |
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
        cfg.session.clone(),
        cfg.turn.clone(),
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
    let router = websocket::create_standalone_router(&cfg.security.cors_origins)
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
backend for room-record storage beyond the built-in `InMemoryDatabase`. That
does not make live rooms restartable: connection routes, reconnect claims,
replay buffers, relay counters, and session plans remain process-local. See the
[consistency and durability contract](docs/architecture/consistency-and-durability.md).

## Building from Source

### Prerequisites

- Rust 1.89.0 or later (see `rust-version` in `Cargo.toml`)
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
│   ├── distributed.rs           # In-memory coordination extension seams
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
├── Cargo.toml
├── Dockerfile
├── docker-compose.yml
├── config.example.json
└── clippy.toml
```

## Authentication

The compiled config default requires WebSocket authentication. The development
example config opts out with `security.require_websocket_auth: false`; remove
that override or set it to `true`, then add entries to the
`security.authorized_apps` array.

When authentication is enabled, clients must send an `Authenticate` message
immediately after connecting. The server validates the `app_id` against the
configured authorized apps and enforces per-app rate limiting based on the
`rate_limit_per_minute` field.

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
`config.example.json` is intentionally insecure configuration.

## MSRV

The minimum supported Rust version is **1.89.0**.

## License

MIT -- [Ambiguous Interactive](https://github.com/Ambiguous-Interactive)

See [LICENSE](LICENSE) for the full license text.
