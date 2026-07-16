# Signal Fish — File Reference

> **Complete file tables for the Signal Fish Server codebase.**
> See [Project Context](project-context.md) for the architecture overview and cross-domain routing.

## Core Server Files

| File               | Purpose            | When to Modify       |
| ------------------ | ------------------ | -------------------- |
| `src/main.rs`      | Entry point        | CLI args, startup    |
| `src/lib.rs`       | Module exports     | Adding new modules   |
| `src/server.rs`    | EnhancedGameServer | Room/player logic    |

## Configuration

| File                         | Purpose              | When to Modify          |
| ---------------------------- | -------------------- | ----------------------- |
| `src/config/mod.rs`          | Config module root   | Config structure        |
| `src/config/types.rs`        | Root Config struct   | Adding config sections  |
| `src/config/server.rs`       | ServerConfig         | Server settings         |
| `src/config/protocol.rs`     | ProtocolConfig       | Protocol settings       |
| `src/config/security.rs`     | SecurityConfig       | Security settings       |
| `src/config/websocket.rs`    | WebSocketConfig      | WS settings             |
| `src/config/logging.rs`      | LoggingConfig        | Logging settings        |
| `src/config/relay.rs`        | RelayTypeConfig      | Relay type mapping      |
| `src/config/defaults.rs`     | Default values       | Changing defaults       |
| `src/config/loader.rs`       | JSON + env loading   | Config loading logic    |
| `src/config/validation.rs`   | Config validation    | Validation rules        |
| `src/config/coordination.rs` | CoordinationConfig   | Coordination settings   |
| `src/config/metrics.rs`      | MetricsConfig        | Metrics settings        |

## Protocol

| File                         | Purpose              | When to Modify          |
| ---------------------------- | -------------------- | ----------------------- |
| `src/protocol/mod.rs`        | Module re-exports    | Adding protocol modules |
| `src/protocol/messages.rs`   | Client/ServerMessage | Adding message types    |
| `src/protocol/types.rs`      | PlayerId, RoomId etc | Adding domain types     |
| `src/protocol/room_state.rs` | Room, LobbyState     | Room state changes      |
| `src/protocol/room_codes.rs` | Room code generation | Code format changes     |
| `src/protocol/error_codes.rs`| ErrorCode enum       | Adding error codes      |
| `src/protocol/validation.rs` | Input validation     | Validation rules        |

## WebSocket

| File                            | Purpose              | When to Modify         |
| ------------------------------- | -------------------- | ---------------------- |
| `src/websocket/mod.rs`          | Module root          | WS module structure    |
| `src/websocket/handler.rs`      | WebSocket upgrade    | Upgrade logic          |
| `src/websocket/connection.rs`   | Socket lifecycle     | Connection handling    |
| `src/websocket/batching.rs`     | Message batching     | Batch behavior         |
| `src/websocket/sending.rs`      | Serialization + send | Wire format changes    |
| `src/websocket/token_binding.rs`| Token binding        | Security binding       |
| `src/websocket/routes.rs`       | Axum router          | Adding routes          |
| `src/websocket/metrics.rs`      | /metrics endpoint    | Metrics output         |
| `src/websocket/prometheus.rs`   | Prometheus format    | Metrics format         |

## Auth, Database, Coordination and Infrastructure

| File                                  | Purpose                           |
| ------------------------------------- | --------------------------------- |
| `src/auth/mod.rs`                     | Auth module root                  |
| `src/auth/middleware.rs`              | InMemoryAuthBackend               |
| `src/auth/rate_limiter.rs`            | Per-app rate limiter              |
| `src/auth/error.rs`                   | AuthError types                   |
| `src/database/mod.rs`                 | GameDatabase trait + InMemory     |
| `src/coordination/mod.rs`             | Coordination module root          |
| `src/coordination/room_coordinator.rs`| InMemoryRoomOperationCoordinator  |
| `src/coordination/dedup.rs`           | DedupCache (LRU)                  |
| `src/distributed.rs`                  | InMemoryDistributedLock           |
| `src/metrics.rs`                      | AtomicU64 + HDR histograms        |
| `src/broadcast.rs`                    | Zero-copy broadcast primitives    |
| `src/rate_limit.rs`                   | In-memory RoomRateLimiter         |
| `src/reconnection.rs`                 | In-memory ReconnectionManager     |
| `src/security/mod.rs`                 | Security module root              |
| `src/security/tls.rs`                 | TLS support (feature-gated)       |
| `src/security/crypto.rs`              | AES-GCM envelope encryption       |
| `src/security/token_binding.rs`       | Channel-bound tokens              |
| `src/logging.rs`                      | Structured logging init           |
| `src/retry.rs`                        | Exponential backoff utility       |
| `src/rkyv_utils.rs`                   | Zero-copy serialization helpers   |

## Agent Guidance Assets

| File/Directory                         | Purpose                                      | When to Modify              |
| -------------------------------------- | -------------------------------------------- | --------------------------- |
| `AGENTS.md` | Always-on repository policy | Updating agent guidance |
| `.agents/skills/*/SKILL.md` | Discoverable task workflows | Adding or updating workflows |
| `.agents/skills/*/references/` | Progressively loaded details | Shared guidance and samples |
| `.agents/skills/websocket-protocol/references/*.jsonl` | Protocol samples | Protocol documentation changes |
