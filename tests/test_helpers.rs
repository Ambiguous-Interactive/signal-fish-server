use signal_fish_server::{
    config::{ProtocolConfig, RelayTypeConfig},
    database::DatabaseConfig,
    server::{EnhancedGameServer, ServerConfig},
};
use std::sync::Arc;
use tokio::time::Duration;

/// Create a test server with in-memory backend for integration tests
#[allow(dead_code)]
pub async fn create_test_server() -> Arc<EnhancedGameServer> {
    create_test_server_with_config(test_server_config(), ProtocolConfig::default()).await
}

/// Create a test server with custom configuration and in-memory backend
#[allow(dead_code)]
pub async fn create_test_server_with_config(
    server_config: ServerConfig,
    protocol_config: ProtocolConfig,
) -> Arc<EnhancedGameServer> {
    build_test_server(
        server_config,
        protocol_config,
        RelayTypeConfig::default(),
        DatabaseConfig::InMemory,
    )
    .await
}

async fn build_test_server(
    server_config: ServerConfig,
    protocol_config: ProtocolConfig,
    relay_type_config: RelayTypeConfig,
    database_config: DatabaseConfig,
) -> Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
        server_config,
        protocol_config,
        relay_type_config,
        database_config,
        signal_fish_server::config::MetricsConfig::default(),
        signal_fish_server::config::AuthMaintenanceConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        vec![],
    )
    .await
    .expect("Failed to create test server")
}

/// Default server configuration optimized for testing
#[allow(dead_code)]
pub fn test_server_config() -> ServerConfig {
    ServerConfig {
        default_max_players: 4,
        ping_timeout: Duration::from_secs(10),
        room_cleanup_interval: Duration::from_secs(1), // Fast cleanup for tests
        max_rooms_per_game: 100,
        rate_limit_config: signal_fish_server::rate_limit::RateLimitConfig {
            max_room_creations: 10,
            time_window: Duration::from_secs(60),
            max_join_attempts: 20,
        },
        empty_room_timeout: Duration::from_secs(5), // Fast timeout for tests
        inactive_room_timeout: Duration::from_secs(10),
        max_message_size: 65536,     // 64KB default
        max_connections_per_ip: 100, // Generous for tests
        require_metrics_auth: false, // No auth for tests
        metrics_auth_token: None,
        reconnection_window: Duration::from_secs(300), // 5 minutes for tests
        event_buffer_size: 100,                        // Buffer 100 events
        enable_reconnection: true,                     // Enable reconnection in tests
        websocket_config: signal_fish_server::config::WebSocketConfig::default(),
        auth_enabled: false,                // Disable auth for tests
        heartbeat_throttle: Duration::ZERO, // No throttling in tests for predictable behavior
        region_id: "test".to_string(),
        room_code_prefix: None,
    }
}

/// Default protocol configuration for testing
#[allow(dead_code)]
pub fn test_protocol_config() -> ProtocolConfig {
    ProtocolConfig {
        room_code_length: 4, // Shorter codes for tests
        max_game_name_length: 32,
        max_player_name_length: 16,
        max_players_limit: 8,
        ..ProtocolConfig::default()
    }
}
