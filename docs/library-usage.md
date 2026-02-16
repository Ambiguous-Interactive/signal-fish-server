# Library Usage

Signal Fish Server is published as both a binary and a library crate. Embed the signaling server into your own
Rust application.

## Add Dependency

```toml
[dependencies]
signal-fish-server = "0.1"
tokio = { version = "1", features = ["full"] }
anyhow = "1"

```

## Basic Embedded Server

```rust

use signal_fish_server::{
    config,
    database::DatabaseConfig,
    server::{EnhancedGameServer, ServerConfig},
    websocket,
};
use std::{net::SocketAddr, sync::Arc, time::Duration};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load configuration
    let cfg = config::load();

    // Build server configuration
    // Note: Only key fields shown for brevity - see src/server.rs ServerConfig for all required fields
    let server_config = ServerConfig {
        default_max_players: cfg.server.default_max_players,
        ping_timeout: Duration::from_secs(cfg.server.ping_timeout),
        room_cleanup_interval: Duration::from_secs(cfg.server.room_cleanup_interval),
        max_rooms_per_game: cfg.server.max_rooms_per_game,
        rate_limit_config: cfg.server.rate_limit_config.clone(),
        empty_room_timeout: Duration::from_secs(cfg.server.empty_room_timeout),
        inactive_room_timeout: Duration::from_secs(cfg.server.inactive_room_timeout),
        max_message_size: cfg.server.max_message_size,
        max_connections_per_ip: cfg.server.max_connections_per_ip,
        require_metrics_auth: cfg.server.require_metrics_auth,
        metrics_auth_token: cfg.server.metrics_auth_token.clone(),
        reconnection_window: Duration::from_secs(cfg.server.reconnection_window),
        event_buffer_size: cfg.server.event_buffer_size,
        enable_reconnection: cfg.server.enable_reconnection,
        websocket_config: cfg.server.websocket_config.clone(),
        auth_enabled: cfg.server.auth_enabled,
        heartbeat_throttle: Duration::from_secs(cfg.server.heartbeat_throttle_secs),
        region_id: cfg.server.region_id.clone(),
        room_code_prefix: cfg.server.room_code_prefix.clone(),
    };

    // Create the game server
    let game_server = Arc::new(
        EnhancedGameServer::new(
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
        .await?
    );

    // Start background cleanup task
    let cleanup = game_server.clone();
    tokio::spawn(async move {
        cleanup.cleanup_task().await
    });

    // Build the Axum router
    let router = websocket::create_router(&cfg.security.cors_origins)
        .with_state(game_server);

    // Start listening
    let addr = SocketAddr::from(([0, 0, 0, 0], cfg.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    println!("Server listening on {}", addr);

    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

```

## Custom Storage Backend

Implement the `GameDatabase` trait for custom persistence:

```rust

use signal_fish_server::database::GameDatabase;
use signal_fish_server::protocol::{Room, RoomId, PlayerId, PlayerInfo, SpectatorInfo, ConnectionInfo, LobbyState};
use async_trait::async_trait;
use anyhow::Result;
use std::collections::HashMap;
use std::any::Any;
use uuid::Uuid;

pub struct MyCustomDatabase {
    // Your storage implementation (e.g., Redis client, PostgreSQL pool, etc.)
}

#[async_trait]
impl GameDatabase for MyCustomDatabase {
    async fn initialize(&self) -> Result<()> {
        // Initialize database connection and run migrations
        Ok(())
    }

    async fn create_room(
        &self,
        game_name: String,
        room_code: Option<String>,
        max_players: u8,
        supports_authority: bool,
        creator_id: PlayerId,
        relay_type: String,
        region_id: String,
        application_id: Option<Uuid>,
    ) -> Result<Room> {
        // Create and store room in your database
        todo!("Implement room creation")
    }

    async fn get_room(&self, game_name: &str, room_code: &str) -> Result<Option<Room>> {
        // Retrieve room from your database by game name and room code
        Ok(None)
    }

    async fn get_room_by_id(&self, room_id: &RoomId) -> Result<Option<Room>> {
        // Retrieve room from your database by ID
        Ok(None)
    }

    async fn add_player_to_room(&self, room_id: &RoomId, player: PlayerInfo) -> Result<bool> {
        // Add player to room (returns false if room is full)
        Ok(false)
    }

    async fn remove_player_from_room(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
    ) -> Result<Option<PlayerInfo>> {
        // Remove player from room
        Ok(None)
    }

    async fn get_room_players(&self, room_id: &RoomId) -> Result<Vec<PlayerInfo>> {
        // Get all players in a room
        Ok(Vec::new())
    }

    async fn delete_room(&self, room_id: &RoomId) -> Result<bool> {
        // Delete a specific room by ID
        Ok(false)
    }

    async fn health_check(&self) -> bool {
        // Health check for your database
        true
    }

    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }

    // ... implement all other required methods from the GameDatabase trait
    // See src/database/mod.rs for the complete trait definition
}

```

Use your custom database:

```rust

let game_server = EnhancedGameServer::new(
    server_config,
    protocol_config,
    relay_types_config,
    DatabaseConfig::Custom(Arc::new(MyCustomDatabase::new())),
    metrics_config,
    auth_config,
    coordination_config,
    transport_config,
    authorized_apps,
)
.await?;

```

## Custom Router Integration

Integrate Signal Fish into an existing Axum application:

```rust

use axum::{Router, routing::get};
use signal_fish_server::websocket;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Your existing routes
    let app = Router::new()
        .route("/", get(|| async { "Hello, world!" }))
        .route("/api/status", get(api_status));

    // Add Signal Fish routes
    let signal_fish_routes = websocket::create_router("*")
        .with_state(game_server);

    // Merge routers
    let app = app.merge(signal_fish_routes);

    // Start server
    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

```

## Programmatic Configuration

Build configuration programmatically instead of from files:

```rust

use signal_fish_server::config::{
    Config, ServerConfig, ProtocolConfig, SecurityConfig,
};

let config = Config {
    port: 3536,
    server: ServerConfig {
        default_max_players: 8,
        ping_timeout: 30,
        room_cleanup_interval: 60,
        ..Default::default()
    },
    protocol: ProtocolConfig {
        max_game_name_length: 64,
        room_code_length: 6,
        ..Default::default()
    },
    security: SecurityConfig {
        cors_origins: "*".to_string(),
        require_websocket_auth: false,
        ..Default::default()
    },
    ..Default::default()
};

```

## Message Handling

> **Note:** The `EnhancedGameServer` does not currently expose public APIs for directly sending messages to
> players. The server automatically handles message routing based on client connections and room membership. This
> is an internal implementation detail.
>
> If you need to send custom server-initiated messages, you would need to extend the server implementation or use
> the internal message coordinator interfaces.

## Event Hooks

> **Note:** The `EnhancedGameServer` does not currently expose a public event subscription API. Server events are
> handled internally for metrics, logging, and coordination.
>
> If you need to monitor server events, consider:
>
> - Using the metrics endpoints (`/metrics` or `/metrics/prom`) to track room and player counts
> - Implementing custom logging by extending the server modules
> - Monitoring structured logs with a log aggregation system (all events are logged with `tracing`)

## Feature Flags

Enable optional features:

```toml
[dependencies]
signal-fish-server = { version = "0.1", features = ["tls", "legacy-fullmesh"] }

```

Available features:

- `tls` - Built-in TLS/mTLS support
- `legacy-fullmesh` - Upstream matchbox full-mesh signaling mode

## Testing

Use the library in your tests:

```rust
#[cfg(test)]
mod tests {
    use signal_fish_server::{
        server::EnhancedGameServer,
        protocol::{ClientMessage, ServerMessage},
    };

    #[tokio::test]
    async fn test_room_creation() {
        // Note: This is a conceptual example. The actual server does not expose
        // a public handle_message API. Message handling is internal to the WebSocket
        // connection lifecycle. For testing, see tests/integration_tests.rs for
        // examples of how to test via the WebSocket interface.

        let server = EnhancedGameServer::new(
            /* config */
        ).await.unwrap();

        // Example: Test via database state instead
        let room = server.database().get_room("test-game", "ABC123").await.unwrap();
        assert!(room.is_some());
    }
}

```

## API Documentation

Generate full API docs:

```bash

cargo doc --open --no-deps

```

## Next Steps

- [Protocol Reference](protocol.md) - Message types and flow
- [Development](development.md) - Building and testing
