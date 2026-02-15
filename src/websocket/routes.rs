use crate::database::DatabaseConfig;
use crate::server::{EnhancedGameServer, ServerConfig};
use axum::extract::State;
use axum::routing::get;
use std::net::SocketAddr;
use std::sync::Arc;

use super::handler::websocket_handler;
use super::metrics::{metrics_handler, prometheus_metrics_handler};

/// Create the Axum router with WebSocket support
pub fn create_router(cors_origins: &str) -> axum::Router<Arc<EnhancedGameServer>> {
    use tower_http::cors::{Any, CorsLayer};
    use tower_http::trace::TraceLayer;

    // Parse CORS origins
    let cors = if cors_origins == "*" {
        CorsLayer::permissive()
    } else {
        let origins: Vec<_> = cors_origins
            .split(',')
            .filter_map(|s| s.trim().parse::<axum::http::HeaderValue>().ok())
            .collect();

        if origins.is_empty() {
            tracing::warn!("No valid CORS origins configured, using permissive CORS");
            CorsLayer::permissive()
        } else {
            CorsLayer::new()
                .allow_origin(origins)
                .allow_methods(Any)
                .allow_headers(Any)
        }
    };

    axum::Router::new()
        .route("/ws", get(websocket_handler))
        .route("/health", get(health_check))
        .route("/metrics", get(metrics_handler))
        .route("/metrics/prom", get(prometheus_metrics_handler))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
}

/// Health check endpoint
async fn health_check(
    State(server): State<Arc<EnhancedGameServer>>,
) -> axum::response::Result<&'static str> {
    if server.health_check().await {
        Ok("OK")
    } else {
        Err(axum::http::StatusCode::SERVICE_UNAVAILABLE.into())
    }
}

/// Start the server with both the new WebSocket protocol and legacy matchbox relay support
#[allow(dead_code)]
pub async fn run_server(
    addr: std::net::SocketAddr,
    server_config: ServerConfig,
    cors_origins: String,
) -> anyhow::Result<()> {
    // Create storage configuration
    let database_config = DatabaseConfig::from_env()?;

    let game_server = EnhancedGameServer::new(
        server_config.clone(),
        crate::config::ProtocolConfig::default(),
        crate::config::RelayTypeConfig::default(),
        database_config,
        crate::config::MetricsConfig::default(),
        crate::config::AuthMaintenanceConfig::default(),
        crate::config::CoordinationConfig::default(),
        crate::config::TransportSecurityConfig::default(),
        Vec::new(),
    )
    .await?;

    // Start cleanup task
    let cleanup_server = game_server.clone();
    tokio::spawn(async move {
        cleanup_server.cleanup_task().await;
    });

    // Create router with CORS configuration
    let app = create_router(&cors_origins).with_state(game_server);

    // Start server
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "Starting enhanced Signal Fish server");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
