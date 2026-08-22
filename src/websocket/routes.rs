use crate::database::DatabaseConfig;
use crate::security::{OriginPolicy, OriginPolicyError};
use crate::server::{EnhancedGameServer, ServerConfig};
use axum::extract::{Extension, State};
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::serve::ListenerExt;
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpSocket, TcpStream};

#[cfg(not(feature = "tls"))]
use super::handler::{websocket_handler, websocket_handler_v3};
#[cfg(feature = "tls")]
use super::handler::{
    websocket_handler_v3_with_verified_certificate as websocket_handler_v3,
    websocket_handler_with_verified_certificate as websocket_handler,
};
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

/// Disable Nagle's algorithm on an accepted connection so small,
/// latency-sensitive relay frames are flushed immediately instead of being held
/// by the Nagle × delayed-ACK interaction (~40-90 ms on loopback).
///
/// `TCP_NODELAY` is a per-connection option and is **not** reliably inherited
/// from the listening socket on Linux, so it must be applied to every accepted
/// stream rather than once in [`bind_tcp_listener`]. This is the single place
/// that configures an accepted socket: the plain `axum::serve` path reaches it
/// through [`bind_serve_listener`] (`tap_io`) and the TLS path through
/// [`ConfiguredAcceptor`], so both stacks share identical semantics. A socket
/// that refuses `TCP_NODELAY` is still served — we warn rather than drop it. See
/// the "accepted sockets set TCP_NODELAY" Architectural Invariant in `.llm/`.
pub(crate) fn configure_accepted_socket(stream: &TcpStream) {
    if let Err(err) = stream.set_nodelay(true) {
        tracing::warn!(%err, "failed to set TCP_NODELAY on accepted socket");
    }
}

/// Bind a plain-TCP listener whose accepted sockets are configured for
/// low-latency relay: a bounded send buffer (via [`bind_tcp_listener`]) plus
/// `TCP_NODELAY` (via the crate-internal `configure_accepted_socket`).
///
/// This is the single seam every plain `axum::serve` path uses — the production
/// server, the `run_server` convenience entry point, and the integration-test
/// harness — so they share identical accepted-socket semantics and a regression
/// in the nodelay wiring fails tests instead of silently shipping (issue #197).
pub fn bind_serve_listener(
    addr: SocketAddr,
    socket_send_buffer_bytes: u32,
) -> std::io::Result<axum::serve::TapIo<TcpListener, fn(&mut TcpStream)>> {
    Ok(bind_tcp_listener(addr, socket_send_buffer_bytes)?
        .tap_io(configure_accepted_socket_io as fn(&mut TcpStream)))
}

/// `tap_io` adapter for [`configure_accepted_socket`]: `tap_io` hands a
/// `&mut TcpStream`, and using a named `fn` (rather than a closure) keeps the
/// returned listener type nameable so [`bind_serve_listener`] can hand back a
/// concrete `TapIo`.
fn configure_accepted_socket_io(stream: &mut TcpStream) {
    configure_accepted_socket(stream);
}

/// `axum_server` acceptor that applies `configure_accepted_socket` to the raw
/// TCP stream before the TLS handshake, so the TLS serve path shares the exact
/// accepted-socket configuration (and warn-and-continue semantics) of the plain
/// `tap_io` path (issue #197).
#[cfg(feature = "tls")]
#[derive(Clone, Copy, Debug, Default)]
pub struct ConfiguredAcceptor;

#[cfg(feature = "tls")]
impl<S> axum_server::accept::Accept<TcpStream, S> for ConfiguredAcceptor {
    type Stream = TcpStream;
    type Service = S;
    type Future = std::future::Ready<std::io::Result<(TcpStream, S)>>;

    fn accept(&self, stream: TcpStream, service: S) -> Self::Future {
        configure_accepted_socket(&stream);
        std::future::ready(Ok((stream, service)))
    }
}

/// Create the nestable Axum router with WebSocket support.
///
/// This router is safe to mount under `/v2`: it exposes `/ws`, `/health`, and
/// metrics routes only. The protocol-v3 alias is intentionally added by
/// [`create_standalone_router`] or by the top-level production router so nesting
/// never creates an undocumented `/v2/v3/ws` endpoint.
pub fn create_router(cors_origins: &str) -> axum::Router<Arc<EnhancedGameServer>> {
    create_router_with_origin_policy(parse_origin_policy_or_deny(cors_origins))
}

/// Create the nestable router, returning invalid Origin configuration to the
/// library caller instead of installing the compatibility deny-all policy.
pub fn try_create_router(
    cors_origins: &str,
) -> Result<axum::Router<Arc<EnhancedGameServer>>, OriginPolicyError> {
    OriginPolicy::parse(cors_origins).map(create_router_with_origin_policy)
}

/// Create the nestable router from a policy already validated by the caller.
pub fn create_router_with_origin_policy(
    origin_policy: OriginPolicy,
) -> axum::Router<Arc<EnhancedGameServer>> {
    create_router_inner(origin_policy, false)
}

/// Create a standalone router for library users that serve Signal Fish at the
/// HTTP root rather than nesting [`create_router`] under `/v2`.
pub fn create_standalone_router(cors_origins: &str) -> axum::Router<Arc<EnhancedGameServer>> {
    create_standalone_router_with_origin_policy(parse_origin_policy_or_deny(cors_origins))
}

/// Create the standalone router or return invalid Origin configuration.
pub fn try_create_standalone_router(
    cors_origins: &str,
) -> Result<axum::Router<Arc<EnhancedGameServer>>, OriginPolicyError> {
    OriginPolicy::parse(cors_origins).map(create_standalone_router_with_origin_policy)
}

/// Create the standalone router from a policy already validated by the caller.
pub fn create_standalone_router_with_origin_policy(
    origin_policy: OriginPolicy,
) -> axum::Router<Arc<EnhancedGameServer>> {
    create_router_inner(origin_policy, true)
}

/// Build the top-level `/v3/ws` route with its required Origin policy.
pub fn websocket_route_v3(
    cors_origins: &str,
) -> axum::routing::MethodRouter<Arc<EnhancedGameServer>> {
    websocket_route_v3_with_origin_policy(parse_origin_policy_or_deny(cors_origins))
}

/// Build the `/v3/ws` route or return invalid Origin configuration.
pub fn try_websocket_route_v3(
    cors_origins: &str,
) -> Result<axum::routing::MethodRouter<Arc<EnhancedGameServer>>, OriginPolicyError> {
    OriginPolicy::parse(cors_origins).map(websocket_route_v3_with_origin_policy)
}

/// Build the top-level `/v3/ws` route from a pre-validated Origin policy.
pub fn websocket_route_v3_with_origin_policy(
    origin_policy: OriginPolicy,
) -> axum::routing::MethodRouter<Arc<EnhancedGameServer>> {
    get(websocket_handler_v3).layer(Extension(origin_policy))
}

/// Build the version-neutral browser-readable client configuration route.
pub fn client_config_route() -> axum::routing::MethodRouter<Arc<EnhancedGameServer>> {
    get(client_config)
}

fn create_router_inner(
    origin_policy: OriginPolicy,
    include_v3_alias: bool,
) -> axum::Router<Arc<EnhancedGameServer>> {
    use tower_http::trace::TraceLayer;

    let cors = origin_policy.cors_layer();

    let router = axum::Router::new()
        .route("/ws", get(websocket_handler))
        .route("/client-config", client_config_route())
        .route("/health", get(health_check))
        .route("/metrics", get(metrics_handler))
        .route("/metrics/prom", get(prometheus_metrics_handler));

    let router = if include_v3_alias {
        router
            .route("/v3/ws", get(websocket_handler_v3))
            .route("/v3/client-config", client_config_route())
    } else {
        router
    };

    router
        .layer(Extension(origin_policy))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
}

#[derive(Debug, Serialize)]
struct ClientConfigResponse {
    max_outbound_message_size: usize,
}

/// Browser-readable pre-connect metadata shared by every protocol version.
async fn client_config(State(server): State<Arc<EnhancedGameServer>>) -> Response {
    (
        [(CACHE_CONTROL, "no-store")],
        Json(ClientConfigResponse {
            max_outbound_message_size: server.config().max_outbound_message_size,
        }),
    )
        .into_response()
}

fn parse_origin_policy_or_deny(cors_origins: &str) -> OriginPolicy {
    OriginPolicy::parse(cors_origins).unwrap_or_else(|error| {
        tracing::error!(%error, "invalid Origin policy; rejecting all WebSocket upgrades");
        OriginPolicy::deny_all_upgrades()
    })
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
    let origin_policy = OriginPolicy::parse(&cors_origins)?;
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
    let app = create_standalone_router_with_origin_policy(origin_policy).with_state(game_server);

    // Accepted sockets are configured for low-latency relay (issue #197).
    let listener = bind_serve_listener(addr, socket_send_buffer_bytes)?;
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

    #[test]
    fn fallible_router_constructors_return_invalid_origin_configuration() {
        for result in [
            try_create_router("https://game.example/path").map(|_| ()),
            try_create_standalone_router("https://game.example/path").map(|_| ()),
            try_websocket_route_v3("https://game.example/path").map(|_| ()),
        ] {
            let error = result.expect_err("invalid Origin configuration must be returned");
            assert!(
                error
                    .to_string()
                    .contains("not a canonical serialized browser origin"),
                "unexpected validation error: {error}"
            );
        }
    }

    #[tokio::test]
    async fn run_server_rejects_invalid_origin_configuration_before_side_effects() {
        let error = run_server(
            "127.0.0.1:0".parse().expect("parse loopback address"),
            ServerConfig::default(),
            "https://game.example/path".to_string(),
        )
        .await
        .expect_err("invalid Origin configuration must stop server startup");

        assert!(
            error.to_string().contains("invalid security.cors_origins"),
            "unexpected startup error: {error}"
        );
    }

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

    /// Regression: issue #197 — accepted WebSocket sockets must disable Nagle's
    /// algorithm (`TCP_NODELAY`) so small bidirectional relay frames are not
    /// stalled ~40-90 ms by the Nagle × delayed-ACK interaction on loopback.
    ///
    /// Exercises the real production seam [`bind_serve_listener`] (used by the
    /// server, `run_server`, and the test harness), so deleting the nodelay
    /// wiring fails this test rather than shipping silently.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn regression_197_accepted_socket_disables_nagle() {
        use axum::serve::Listener;

        let listener =
            bind_serve_listener("127.0.0.1:0".parse().expect("parse loopback address"), 0)
                .expect("bind listener");
        let addr = listener.local_addr().expect("read listener address");
        let accept = tokio::spawn(async move {
            let mut listener = listener;
            let (io, _) = listener.accept().await;
            io.nodelay().expect("read TCP_NODELAY on accepted socket")
        });

        let client = TcpStream::connect(addr)
            .await
            .expect("connect loopback client");
        let nodelay = accept.await.expect("accept task panicked");
        drop(client);

        assert!(
            nodelay,
            "accepted socket must have TCP_NODELAY enabled to avoid Nagle × \
             delayed-ACK relay stalls (issue #197)"
        );
    }

    /// Regression: issue #197 — the TLS serve path must disable Nagle on the raw
    /// TCP stream before the handshake. Exercises [`ConfiguredAcceptor`], the
    /// acceptor `main` installs on the `axum_server` TLS stack.
    #[cfg(feature = "tls")]
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn regression_197_tls_acceptor_disables_nagle() {
        use axum_server::accept::Accept;

        let listener = bind_tcp_listener("127.0.0.1:0".parse().expect("parse loopback address"), 0)
            .expect("bind listener");
        let addr = listener.local_addr().expect("read listener address");
        let accept = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept loopback client");
            let (stream, ()) = ConfiguredAcceptor
                .accept(stream, ())
                .await
                .expect("configured acceptor");
            stream
                .nodelay()
                .expect("read TCP_NODELAY on accepted socket")
        });

        let client = TcpStream::connect(addr)
            .await
            .expect("connect loopback client");
        let nodelay = accept.await.expect("accept task panicked");
        drop(client);

        assert!(
            nodelay,
            "TLS ConfiguredAcceptor must enable TCP_NODELAY on the raw stream (issue #197)"
        );
    }
}
