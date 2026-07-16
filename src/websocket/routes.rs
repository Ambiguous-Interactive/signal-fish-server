use crate::database::DatabaseConfig;
use crate::server::{EnhancedGameServer, ServerConfig};
use axum::extract::State;
use axum::routing::get;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpSocket};

use super::handler::{websocket_handler, websocket_handler_v3};
use super::metrics::{metrics_handler, prometheus_metrics_handler};

const LISTENER_BACKLOG: u32 = 1_024;

/// Bind a TCP listener whose accepted sockets inherit a bounded send buffer.
///
/// A bounded kernel handoff is part of the WebSocket control-priority
/// contract: once a data frame has been accepted by TCP, application-level
/// queue priority cannot move a later Ping or delivery report ahead of it.
pub fn bind_tcp_listener(
    addr: SocketAddr,
    socket_send_buffer_bytes: u32,
) -> std::io::Result<TcpListener> {
    let socket = if addr.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    socket.set_reuseaddr(true)?;
    if socket_send_buffer_bytes > 0 {
        socket.set_send_buffer_size(socket_send_buffer_bytes)?;
    }
    let effective_send_buffer_bytes = socket.send_buffer_size()?;
    socket.bind(addr)?;
    let listener = socket.listen(LISTENER_BACKLOG)?;
    tracing::info!(
        requested_send_buffer_bytes = socket_send_buffer_bytes,
        effective_send_buffer_bytes,
        "Bound TCP listener with bounded WebSocket kernel handoff"
    );
    Ok(listener)
}

/// Create the nestable Axum router with WebSocket support.
///
/// This router is safe to mount under `/v2`: it exposes `/ws`, `/health`, and
/// metrics routes only. The protocol-v3 alias is intentionally added by
/// [`create_standalone_router`] or by the top-level production router so nesting
/// never creates an undocumented `/v2/v3/ws` endpoint.
pub fn create_router(cors_origins: &str) -> axum::Router<Arc<EnhancedGameServer>> {
    create_router_inner(cors_origins, false)
}

/// Create a standalone router for library users that serve Signal Fish at the
/// HTTP root rather than nesting [`create_router`] under `/v2`.
pub fn create_standalone_router(cors_origins: &str) -> axum::Router<Arc<EnhancedGameServer>> {
    create_router_inner(cors_origins, true)
}

fn create_router_inner(
    cors_origins: &str,
    include_v3_alias: bool,
) -> axum::Router<Arc<EnhancedGameServer>> {
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

    let router = axum::Router::new()
        .route("/ws", get(websocket_handler))
        .route("/health", get(health_check))
        .route("/metrics", get(metrics_handler))
        .route("/metrics/prom", get(prometheus_metrics_handler));

    let router = if include_v3_alias {
        router.route("/v3/ws", get(websocket_handler_v3))
    } else {
        router
    };

    router.layer(cors).layer(TraceLayer::new_for_http())
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

/// Start the server with both the WebSocket protocol and legacy relay support.
#[allow(dead_code)]
pub async fn run_server(
    addr: std::net::SocketAddr,
    server_config: ServerConfig,
    cors_origins: String,
) -> anyhow::Result<()> {
    let socket_send_buffer_bytes = server_config.websocket_config.socket_send_buffer_bytes;
    // Create storage configuration
    let database_config = DatabaseConfig::from_env()?;

    let game_server = EnhancedGameServer::new(
        server_config.clone(),
        crate::config::ProtocolConfig::default(),
        crate::config::RelayTypeConfig::default(),
        crate::config::SessionConfig::default(),
        crate::config::TurnConfig::default(),
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
    let app = create_standalone_router(&cors_origins).with_state(game_server);

    // Start server
    let listener = bind_tcp_listener(addr, socket_send_buffer_bytes)?;
    tracing::info!(%addr, "Starting enhanced Signal Fish server");
    tracing::info!(
        deployment_mode = "single_instance",
        room_state = "in_memory",
        room_affinity_required = true,
        session_handoff = false,
        "Deployment contract: one process owns each room; losing the process loses its rooms"
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn accepted_socket_preserves_listener_send_buffer_bound() {
        const REQUESTED_BYTES: u32 = 32 * 1_024;
        // Linux commonly reports exactly twice the request. macOS applies its
        // own accepted-socket rounding (for example, 65,328 for a 32-KiB
        // request). The cross-platform contract is the bounded kernel handoff,
        // not byte-for-byte equality with a pre-connect probe socket.
        const MAX_EFFECTIVE_BYTES: u32 = REQUESTED_BYTES * 2;

        let listener = bind_tcp_listener(
            "127.0.0.1:0".parse().expect("parse loopback address"),
            REQUESTED_BYTES,
        )
        .expect("bind bounded listener");
        let addr = listener.local_addr().expect("read listener address");
        let accept = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept loopback client");
            let stream = stream.into_std().expect("convert accepted stream");
            TcpSocket::from_std_stream(stream)
                .send_buffer_size()
                .expect("read accepted send buffer")
        });

        let client = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect loopback client");
        let accepted = accept.await.expect("accept task panicked");
        drop(client);

        assert!(
            accepted <= MAX_EFFECTIVE_BYTES,
            "accepted socket SO_SNDBUF {accepted} exceeded the configured \
             two-times effective ceiling {MAX_EFFECTIVE_BYTES}"
        );
    }
}
