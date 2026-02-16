# Architecture

System design and project structure overview.

## System Overview

```text
┌─────────────────────────────────────────────────────┐
│              Game Clients (WebSocket)                │
│  Browser | Unity | Godot | Custom                    │
└────────────────────┬────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────────────┐
│            Signal Fish Server (Rust)                 │
│  ┌───────────────────────────────────────────────┐  │
│  │  WebSocket Layer (axum)                       │  │
│  │  - Upgrade handling                           │  │
│  │  - Message batching                           │  │
│  │  - Auth enforcement                           │  │
│  └───────────────┬───────────────────────────────┘  │
│                  ▼                                   │
│  ┌───────────────────────────────────────────────┐  │
│  │  EnhancedGameServer (Core Logic)              │  │
│  │  - Room management                            │  │
│  │  - Player state                               │  │
│  │  - Authority management                       │  │
│  │  - Message routing                            │  │
│  └───────────────┬───────────────────────────────┘  │
│                  ▼                                   │
│  ┌───────────────────────────────────────────────┐  │
│  │  In-Memory Storage                            │  │
│  │  - Rooms (DashMap)                            │  │
│  │  - Players (DashMap)                          │  │
│  │  - Rate limits                                │  │
│  │  - Reconnection tokens                        │  │
│  └───────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────┘
```

## Core Components

### WebSocket Layer

**Location:** `src/websocket/`

Handles WebSocket upgrade, message serialization, and connection lifecycle.

Key modules:
- `handler.rs` - WebSocket upgrade and initialization
- `connection.rs` - Connection lifecycle management
- `batching.rs` - Outbound message batching
- `sending.rs` - Message serialization (JSON/MessagePack)
- `routes.rs` - Axum router configuration

### EnhancedGameServer

**Location:** `src/server.rs`

Core server logic coordinating all operations.

Responsibilities:
- Room creation and lifecycle
- Player join/leave handling
- Message routing between players
- Authority management
- Lobby state transitions
- Cleanup tasks

### Protocol Layer

**Location:** `src/protocol/`

Defines message types, validation, and domain types.

Key modules:
- `messages.rs` - `ClientMessage` and `ServerMessage` enums
- `types.rs` - `PlayerId`, `RoomId`, domain types
- `room_state.rs` - `Room`, `Player`, `LobbyState`
- `validation.rs` - Input validation functions
- `room_codes.rs` - Room code generation

### Configuration

**Location:** `src/config/`

Configuration loading, validation, and defaults.

Structure:
- `loader.rs` - JSON file + environment variable loading
- `validation.rs` - Config validation rules
- `defaults.rs` - Default values
- Type-specific configs: `server.rs`, `security.rs`, `protocol.rs`, etc.

### Database

**Location:** `src/database/`

Storage abstraction with in-memory implementation.

- `GameDatabase` trait - Abstract storage interface
- `InMemoryDatabase` - Default implementation using `DashMap`

Designed for extension with custom backends (Redis, PostgreSQL, etc.).

### Authentication

**Location:** `src/auth/`

App-based authentication and per-app rate limiting.

- `middleware.rs` - `InMemoryAuthBackend`
- `rate_limiter.rs` - Per-app token bucket rate limiter
- `error.rs` - Auth error types

### Coordination

**Location:** `src/coordination/`

Distributed coordination primitives for multi-instance deployments.

- `room_coordinator.rs` - Room operation coordination
- `dedup.rs` - Deduplication cache (LRU)

### Metrics

**Location:** `src/metrics.rs`, `src/websocket/metrics.rs`, `src/websocket/prometheus.rs`

Metrics collection and export.

- Atomic counters for room/player counts
- HDR histograms for latency tracking
- JSON and Prometheus export formats

## Data Flow

### Room Creation

```text
Client                    WebSocket Handler         EnhancedGameServer        Database
  |                              |                         |                      |
  |-- CreateRoom -------------->|                         |                      |
  |                              |-- handle_message ------>|                      |
  |                              |                         |-- create_room ------>|
  |                              |                         |<-- store room -------|
  |                              |<-- RoomCreated ---------|                      |
  |<-- RoomCreated --------------|                         |                      |
```

### Player Join

```text
Client                    WebSocket Handler         EnhancedGameServer        Database
  |                              |                         |                      |
  |-- JoinRoom ---------------->|                         |                      |
  |                              |-- handle_message ------>|                      |
  |                              |                         |-- get_room --------->|
  |                              |                         |<-- room --------------|
  |                              |                         |-- add_player ------->|
  |                              |<-- RoomJoined ----------|                      |
  |<-- RoomJoined ---------------|                         |                      |
  |                              |                         |                      |
  |<-- PlayerJoined -------------|<-- broadcast to room ---|                      |
(other clients)                                                                   |
```

### Game Data Relay

```text
Client A                 EnhancedGameServer                    Client B
  |                              |                                  |
  |-- GameData ----------------->|                                  |
  |                              |-- broadcast to room ------------->|
  |                              |                                  |<-- GameData
```

## Concurrency Model

### Async Runtime

Uses **Tokio** for async I/O and task scheduling.

### Shared State

- `Arc<DashMap>` for concurrent room/player access
- `RwLock` for infrequent writes (config, auth state)
- Atomic counters for metrics

### Message Passing

- Bounded channels for player message queues
- Backpressure via channel capacity limits

### Locking Strategy

- Fine-grained locking per room
- Never hold locks across `.await` points
- Use `tokio::spawn` to avoid blocking

## Scalability

### Single Instance Limits

Recommended per instance:
- **Rooms:** < 500 active rooms
- **Players:** < 2000 concurrent players
- **Messages:** < 10,000 messages/second

### Multi-Instance Deployment

For horizontal scaling:
1. **Session affinity** - Route by game_name or room_code at load balancer
2. **Room sharding** - Distribute rooms across instances
3. **Coordination** - Use `RoomOperationCoordinator` for distributed locking

### Storage Options

- **In-Memory** (default) - Fast, ephemeral, no external dependencies
- **Redis** - Implement `GameDatabase` trait with Redis backend
- **PostgreSQL** - Implement `GameDatabase` trait with PostgreSQL backend

## Security Architecture

### Layers

1. **Transport** - Optional TLS (feature-gated)
2. **Authentication** - App-based auth (optional)
3. **Rate Limiting** - Per-IP and per-app limits
4. **Validation** - Input validation at protocol boundaries
5. **Connection Limits** - Max connections per IP

### Trust Model

- **Untrusted clients** - All input validated
- **Semi-trusted apps** - App authentication + rate limits
- **Trusted admin** - Metrics endpoints (optional auth)

## Module Dependencies

```text
main.rs
  └── lib.rs
       ├── server.rs (EnhancedGameServer)
       │    ├── server/admin.rs
       │    ├── server/authority.rs
       │    ├── server/connection_manager.rs
       │    ├── server/dashboard_cache.rs
       │    ├── server/game_data.rs
       │    ├── server/heartbeat.rs
       │    ├── server/maintenance.rs
       │    ├── server/message_router.rs
       │    ├── server/messaging.rs
       │    ├── server/ready_state.rs
       │    ├── server/reconnection_service.rs
       │    ├── server/relay_policy.rs
       │    ├── server/room_service.rs
       │    ├── server/spectator_handlers.rs
       │    └── server/spectator_service.rs
       ├── protocol/
       │    ├── messages.rs (ClientMessage, ServerMessage)
       │    ├── types.rs (PlayerId, RoomId, etc.)
       │    ├── room_state.rs (Room, Player, LobbyState)
       │    ├── room_codes.rs (Room code generation)
       │    ├── validation.rs (Input validation)
       │    └── error_codes.rs (ErrorCode enum)
       ├── database/
       │    └── mod.rs (GameDatabase trait, InMemoryDatabase)
       ├── coordination/
       │    ├── mod.rs (MessageCoordinator trait)
       │    ├── room_coordinator.rs (RoomOperationCoordinator)
       │    └── dedup.rs (DedupCache)
       ├── auth/
       │    ├── middleware.rs (InMemoryAuthBackend)
       │    ├── rate_limiter.rs (Per-app rate limiter)
       │    └── error.rs (AuthError types)
       ├── websocket/
       │    ├── handler.rs (WebSocket upgrade)
       │    ├── connection.rs (Connection lifecycle)
       │    ├── batching.rs (Message batching)
       │    ├── sending.rs (Serialization + send)
       │    ├── token_binding.rs (Token binding)
       │    ├── routes.rs (Axum router)
       │    ├── metrics.rs (/metrics endpoint)
       │    └── prometheus.rs (Prometheus format)
       ├── config/
       │    ├── types.rs (Root Config struct)
       │    ├── server.rs (ServerConfig)
       │    ├── protocol.rs (ProtocolConfig)
       │    ├── security.rs (SecurityConfig)
       │    ├── websocket.rs (WebSocketConfig)
       │    ├── logging.rs (LoggingConfig)
       │    ├── relay.rs (RelayTypeConfig)
       │    ├── coordination.rs (CoordinationConfig)
       │    ├── metrics.rs (MetricsConfig)
       │    ├── defaults.rs (Default values)
       │    ├── loader.rs (JSON + env loading)
       │    └── validation.rs (Config validation)
       ├── security/
       │    ├── tls.rs (TLS support, feature-gated)
       │    ├── crypto.rs (AES-GCM envelope encryption)
       │    └── token_binding.rs (Channel-bound tokens)
       ├── metrics.rs (AtomicU64 + HDR histograms)
       ├── logging.rs (Structured logging init)
       ├── distributed.rs (InMemoryDistributedLock)
       ├── broadcast.rs (Zero-copy broadcast primitives)
       ├── rate_limit.rs (In-memory RoomRateLimiter)
       ├── reconnection.rs (In-memory ReconnectionManager)
       ├── retry.rs (Exponential backoff utility)
       └── rkyv_utils.rs (Zero-copy serialization helpers)
```

## Architecture Decision Records

Key architectural decisions are documented in [Architecture Decision Records (ADRs)](adr/).

Current ADRs:

- [ADR-001: Reconnection Protocol](adr/reconnection-protocol.md) - WebSocket reconnection and event replay

## Design Principles

### Zero-Cost Abstractions

- Prefer value types over heap allocations
- Use `Bytes` for network data (zero-copy)
- Borrow instead of clone where possible

### Fail-Fast Validation

- Validate at system boundaries
- Return typed errors (`Result<T, E>`)
- Never `.unwrap()` in production code

### Graceful Degradation

- Handle partial failures
- Implement backpressure
- Timeout on slow operations

### Observable by Default

- Structured logging with `tracing`
- Metrics for all operations
- Correlation IDs for request tracking

## Testing Strategy

### Unit Tests

Module-level tests in the same file:

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_room_code_validation() {
        // ...
    }
}
```

### Integration Tests

Multi-component tests in `tests/`:

```rust
#[tokio::test]
async fn test_room_lifecycle() {
    // ...
}
```

### E2E Tests

Full WebSocket session tests:

```rust
#[tokio::test]
async fn test_websocket_flow() {
    // ...
}
```

## Next Steps

- [Development](development.md) - Building and testing
- [Library Usage](library-usage.md) - Embedding the server
- [Protocol Reference](protocol.md) - Message types
