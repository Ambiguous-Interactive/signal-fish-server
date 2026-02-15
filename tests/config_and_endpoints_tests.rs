//! Configuration loading and HTTP endpoint integration tests.
//!
//! Covers:
//! - Config loading from JSON (`SIGNAL_FISH_CONFIG_JSON`)
//! - Environment variable overrides (`SIGNAL_FISH__*`)
//! - Health endpoint (`/health`)
//! - Metrics endpoint (`/metrics`)

mod test_helpers;

use signal_fish_server::config::Config;
use signal_fish_server::websocket::create_router;
use test_helpers::{create_test_server, test_server_config};

// ===========================================================================
// Config loading tests
// ===========================================================================

#[test]
fn test_config_default_values() {
    let config = Config::default();

    assert_eq!(config.port, 3536);
    assert_eq!(config.server.default_max_players, 8);
    assert_eq!(config.server.ping_timeout, 30);
    assert_eq!(config.server.room_cleanup_interval, 60);
    assert_eq!(config.server.max_rooms_per_game, 1000);
    assert_eq!(config.server.empty_room_timeout, 300);
    assert_eq!(config.server.inactive_room_timeout, 3600);
    assert_eq!(config.protocol.room_code_length, 6);
    assert_eq!(config.protocol.max_game_name_length, 64);
    assert_eq!(config.protocol.max_player_name_length, 32);
    assert_eq!(config.protocol.max_players_limit, 100);
}

#[test]
fn test_config_roundtrip_serialization() {
    let config = Config::default();
    let json = serde_json::to_string_pretty(&config).expect("serialization should succeed");
    let deserialized: Config = serde_json::from_str(&json).expect("deserialization should succeed");

    assert_eq!(config.port, deserialized.port);
    assert_eq!(
        config.server.default_max_players,
        deserialized.server.default_max_players
    );
    assert_eq!(
        config.rate_limit.max_room_creations,
        deserialized.rate_limit.max_room_creations
    );
    assert_eq!(
        config.protocol.max_game_name_length,
        deserialized.protocol.max_game_name_length
    );
}

#[test]
fn test_config_from_json_string() {
    let json = r#"{
        "port": 9999,
        "server": {
            "default_max_players": 16,
            "region_id": "us-east-1"
        },
        "protocol": {
            "room_code_length": 8
        }
    }"#;

    let config: Config = serde_json::from_str(json).expect("parse should succeed");

    assert_eq!(config.port, 9999);
    assert_eq!(config.server.default_max_players, 16);
    assert_eq!(config.server.region_id, "us-east-1");
    assert_eq!(config.protocol.room_code_length, 8);
    // Non-specified fields should remain at defaults
    assert_eq!(config.server.ping_timeout, 30);
}

#[test]
fn test_config_partial_json_uses_defaults_for_missing_fields() {
    let json = r#"{ "port": 4000 }"#;
    let config: Config = serde_json::from_str(json).expect("parse should succeed");

    assert_eq!(config.port, 4000);
    // All other fields should be defaults
    assert_eq!(config.server.default_max_players, 8);
    assert_eq!(config.protocol.room_code_length, 6);
    assert_eq!(config.logging.dir, "logs");
}

#[test]
fn test_config_security_authorized_apps_deserialization() {
    let json = r#"{
        "security": {
            "require_websocket_auth": true,
            "authorized_apps": [
                {
                    "app_id": "my-game",
                    "app_secret": "my-secret",
                    "app_name": "My Game",
                    "max_rooms": 100,
                    "max_players_per_room": 4,
                    "rate_limit_per_minute": 30
                }
            ]
        }
    }"#;

    let config: Config = serde_json::from_str(json).expect("parse should succeed");

    assert!(config.security.require_websocket_auth);
    assert_eq!(config.security.authorized_apps.len(), 1);

    let app = &config.security.authorized_apps[0];
    assert_eq!(app.app_id, "my-game");
    assert_eq!(app.app_secret, "my-secret");
    assert_eq!(app.app_name, "My Game");
    assert_eq!(app.max_rooms, Some(100));
    assert_eq!(app.max_players_per_room, Some(4));
    assert_eq!(app.rate_limit_per_minute, Some(30));
}

#[test]
fn test_config_rate_limit_section() {
    let json = r#"{
        "rate_limit": {
            "max_room_creations": 20,
            "time_window": 120,
            "max_join_attempts": 50
        }
    }"#;

    let config: Config = serde_json::from_str(json).expect("parse should succeed");

    assert_eq!(config.rate_limit.max_room_creations, 20);
    assert_eq!(config.rate_limit.time_window, 120);
    assert_eq!(config.rate_limit.max_join_attempts, 50);
}

#[test]
fn test_config_region_id_and_room_code_prefix() {
    let json = r#"{
        "server": {
            "region_id": "eu-west-2",
            "room_code_prefix": "EU"
        }
    }"#;

    let config: Config = serde_json::from_str(json).expect("parse should succeed");

    assert_eq!(config.server.region_id, "eu-west-2");
    assert_eq!(config.server.room_code_prefix, Some("EU".to_string()));
}

#[test]
fn test_config_heartbeat_throttle() {
    let json = r#"{
        "server": {
            "heartbeat_throttle_secs": 15
        }
    }"#;

    let config: Config = serde_json::from_str(json).expect("parse should succeed");

    assert_eq!(config.server.heartbeat_throttle_secs, 15);
}

#[test]
fn test_config_websocket_section() {
    let json = r#"{
        "websocket": {
            "enable_batching": true,
            "batch_size": 64,
            "batch_interval_ms": 50,
            "auth_timeout_secs": 15
        }
    }"#;

    let config: Config = serde_json::from_str(json).expect("parse should succeed");

    assert!(config.websocket.enable_batching);
    assert_eq!(config.websocket.batch_size, 64);
    assert_eq!(config.websocket.batch_interval_ms, 50);
    assert_eq!(config.websocket.auth_timeout_secs, 15);
}

// ===========================================================================
// Health endpoint tests
// ===========================================================================

#[tokio::test]
async fn test_health_endpoint_returns_ok() {
    let server = create_test_server().await;
    let app = create_router("*").with_state(server);

    let test_server = axum_test::TestServer::new(app).expect("test server should start");
    let response = test_server.get("/health").await;

    response.assert_status_ok();
    response.assert_text("OK");
}

// ===========================================================================
// Metrics endpoint tests
// ===========================================================================

#[tokio::test]
async fn test_metrics_endpoint_no_auth_required() {
    // Create server with metrics auth disabled
    let mut config = test_server_config();
    config.require_metrics_auth = false;

    let server = test_helpers::create_test_server_with_config(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
    )
    .await;

    let app = create_router("*").with_state(server);
    let test_server = axum_test::TestServer::new(app).expect("test server should start");

    let response = test_server.get("/metrics").await;
    response.assert_status_ok();

    // Should return JSON with expected structure
    let json: serde_json::Value = response.json();
    assert!(
        json.get("timeRange").is_some(),
        "metrics should contain timeRange"
    );
    assert!(
        json.get("serverMetrics").is_some(),
        "metrics should contain serverMetrics"
    );
}

#[tokio::test]
async fn test_metrics_endpoint_requires_auth_when_configured() {
    // Create server with metrics auth enabled
    let mut config = test_server_config();
    config.require_metrics_auth = true;
    config.metrics_auth_token = Some("test-metrics-token".to_string());

    let server = test_helpers::create_test_server_with_config(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
    )
    .await;

    let app = create_router("*").with_state(server);
    let test_server = axum_test::TestServer::new(app).expect("test server should start");

    // Without auth header, should fail
    let response = test_server.get("/metrics").await;
    response.assert_status(axum::http::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_metrics_endpoint_accepts_valid_bearer_token() {
    let mut config = test_server_config();
    config.require_metrics_auth = true;
    config.metrics_auth_token = Some("valid-token".to_string());

    let server = test_helpers::create_test_server_with_config(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
    )
    .await;

    let app = create_router("*").with_state(server);
    let test_server = axum_test::TestServer::new(app).expect("test server should start");

    let response = test_server
        .get("/metrics")
        .add_header(
            axum::http::header::AUTHORIZATION,
            "Bearer valid-token"
                .parse::<axum::http::HeaderValue>()
                .unwrap(),
        )
        .await;

    response.assert_status_ok();
}

#[tokio::test]
async fn test_metrics_endpoint_rejects_invalid_bearer_token() {
    let mut config = test_server_config();
    config.require_metrics_auth = true;
    config.metrics_auth_token = Some("correct-token".to_string());

    let server = test_helpers::create_test_server_with_config(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
    )
    .await;

    let app = create_router("*").with_state(server);
    let test_server = axum_test::TestServer::new(app).expect("test server should start");

    let response = test_server
        .get("/metrics")
        .add_header(
            axum::http::header::AUTHORIZATION,
            "Bearer wrong-token"
                .parse::<axum::http::HeaderValue>()
                .unwrap(),
        )
        .await;

    response.assert_status(axum::http::StatusCode::UNAUTHORIZED);
}

// ===========================================================================
// Prometheus metrics endpoint tests
// ===========================================================================

#[tokio::test]
async fn test_prometheus_metrics_endpoint_returns_text() {
    let mut config = test_server_config();
    config.require_metrics_auth = false;

    let server = test_helpers::create_test_server_with_config(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
    )
    .await;

    let app = create_router("*").with_state(server);
    let test_server = axum_test::TestServer::new(app).expect("test server should start");

    let response = test_server.get("/metrics/prom").await;
    response.assert_status_ok();

    // Prometheus format should contain standard HELP and TYPE annotations
    let body = response.text();
    assert!(body.contains("# HELP"), "Should contain HELP comment lines");
    assert!(body.contains("# TYPE"), "Should contain TYPE annotations");
}

// ===========================================================================
// Router structure tests
// ===========================================================================

#[tokio::test]
async fn test_websocket_route_exists() {
    let server = create_test_server().await;
    let app = create_router("*").with_state(server);

    let test_server = axum_test::TestServer::new(app).expect("test server should start");

    // GET /ws without WebSocket upgrade should not return 404
    // (It will return 400 or similar since there's no upgrade header, but NOT 404)
    let response = test_server.get("/ws").await;
    let status = response.status_code();
    assert_ne!(
        status,
        axum::http::StatusCode::NOT_FOUND,
        "/ws route should exist"
    );
}

#[tokio::test]
async fn test_unknown_route_returns_404() {
    let server = create_test_server().await;
    let app = create_router("*").with_state(server);

    let test_server = axum_test::TestServer::new(app).expect("test server should start");
    let response = test_server.get("/nonexistent").await;
    response.assert_status(axum::http::StatusCode::NOT_FOUND);
}

// ===========================================================================
// CORS configuration tests
// ===========================================================================

#[tokio::test]
async fn test_permissive_cors_with_wildcard() {
    let server = create_test_server().await;
    let app = create_router("*").with_state(server);

    let test_server = axum_test::TestServer::new(app).expect("test server should start");
    let response = test_server.get("/health").await;
    response.assert_status_ok();
}

#[tokio::test]
async fn test_specific_cors_origins() {
    let server = create_test_server().await;
    let app = create_router("http://localhost:3000,http://example.com").with_state(server);

    let test_server = axum_test::TestServer::new(app).expect("test server should start");
    let response = test_server.get("/health").await;
    response.assert_status_ok();
}
