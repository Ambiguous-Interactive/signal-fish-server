use signal_fish_server::{
    config::{ProtocolConfig, RelayTypeConfig},
    database::DatabaseConfig,
    server::{EnhancedGameServer, ServerConfig},
};
use std::sync::Arc;
use tokio::time::Duration;

/// Scoped in-process Axum server for real-socket integration tests.
///
/// Tests must call [`Self::shutdown`] so upgraded WebSocket tasks finish before
/// the Tokio runtime and LeakSanitizer inspect process teardown. [`Drop`] only
/// provides best-effort cancellation for panic paths because it cannot await
/// socket or server-task completion.
#[allow(dead_code)]
pub struct RunningTestServer {
    addr: std::net::SocketAddr,
    server: Arc<EnhancedGameServer>,
    shutdown_tx: Option<tokio::sync::watch::Sender<bool>>,
    serve_task: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    shutdown_complete: bool,
}

#[allow(dead_code)]
impl RunningTestServer {
    pub async fn spawn(server: Arc<EnhancedGameServer>, router: axum::Router) -> Self {
        // Serve through the production plain-TCP path (issue #197 accepted-
        // socket configuration, issue #518 armed HTTP header-read deadline),
        // so latency- and deadline-sensitive e2e tests observe production
        // semantics.
        let listener = signal_fish_server::websocket::bind_tcp_listener(
            "127.0.0.1:0".parse().expect("parse test listener address"),
            server.config().websocket_config.socket_send_buffer_bytes,
        )
        .expect("bind test listener");
        let addr = listener.local_addr().expect("read test listener address");
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let make_service = router.into_make_service_with_connect_info::<std::net::SocketAddr>();
        let timeouts = signal_fish_server::websocket::HttpServeTimeouts::production(
            std::time::Duration::from_secs(
                server
                    .config()
                    .websocket_config
                    .http_header_read_timeout_secs,
            ),
        );
        let serve_task = tokio::spawn(
            signal_fish_server::websocket::serve_with_http_header_deadline(
                listener,
                make_service,
                timeouts,
                shutdown_rx,
            ),
        );

        Self {
            addr,
            server,
            shutdown_tx: Some(shutdown_tx),
            serve_task: Some(serve_task),
            shutdown_complete: false,
        }
    }

    pub fn addr(&self) -> std::net::SocketAddr {
        self.addr
    }

    pub async fn shutdown(mut self) {
        self.server.begin_shutdown_drain();
        let _ = self
            .shutdown_tx
            .take()
            .expect("test server shutdown signal missing")
            .send(true);
        self.server.close_connections_for_shutdown();

        let settle_timeout =
            signal_fish_server::websocket::registered_connection_shutdown_settle_timeout();
        let remaining = self
            .server
            .wait_for_shutdown_connections(settle_timeout)
            .await;
        assert_eq!(
            remaining, 0,
            "test server retained {remaining} WebSocket handler(s) after shutdown"
        );

        let mut serve_task = self
            .serve_task
            .take()
            .expect("test server serve task missing");
        match tokio::time::timeout(settle_timeout, &mut serve_task).await {
            Ok(result) => result
                .expect("test server Axum task panicked")
                .expect("test server Axum task failed"),
            Err(_) => {
                serve_task.abort();
                let _ = serve_task.await;
                panic!("test server Axum task did not stop after connection drain");
            }
        }
        self.shutdown_complete = true;
    }
}

impl Drop for RunningTestServer {
    fn drop(&mut self) {
        // A destructor cannot await the normal bounded drain. Still reject new
        // upgrades and ask registered socket tasks to close before stopping the
        // listener, so a test panic leaves the runtime as little work as
        // possible to cancel.
        self.server.begin_shutdown_drain();
        self.server.close_connections_for_shutdown();
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(true);
        }
        if let Some(serve_task) = self.serve_task.take() {
            serve_task.abort();
        }
        assert!(
            self.shutdown_complete || std::thread::panicking(),
            "RunningTestServer dropped without awaiting shutdown()"
        );
    }
}

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
    create_test_server_with_transport_security(
        server_config,
        protocol_config,
        signal_fish_server::config::TransportSecurityConfig::default(),
    )
    .await
}

/// Create a test server with custom application and transport-security configuration.
#[allow(dead_code)]
pub async fn create_test_server_with_transport_security(
    server_config: ServerConfig,
    protocol_config: ProtocolConfig,
    transport_security: signal_fish_server::config::TransportSecurityConfig,
) -> Arc<EnhancedGameServer> {
    build_test_server(
        server_config,
        protocol_config,
        RelayTypeConfig::default(),
        DatabaseConfig::InMemory,
        transport_security,
    )
    .await
}

async fn build_test_server(
    server_config: ServerConfig,
    protocol_config: ProtocolConfig,
    relay_type_config: RelayTypeConfig,
    database_config: DatabaseConfig,
    transport_security: signal_fish_server::config::TransportSecurityConfig,
) -> Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
        server_config,
        protocol_config,
        relay_type_config,
        signal_fish_server::config::SessionConfig::default(),
        signal_fish_server::config::TurnConfig::default(),
        database_config,
        signal_fish_server::config::MetricsConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        transport_security,
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
        drain_grace: Duration::from_secs(30),
        max_rooms_per_game: 100,
        max_rooms: 10_000, // Generous server-wide ceiling for tests
        rate_limit_config: signal_fish_server::rate_limit::RateLimitConfig {
            max_room_creations: 10,
            time_window: Duration::from_secs(60),
            max_join_attempts: 20,
            max_signals: 600,
            max_signal_errors: 60,
            max_inbound_error_replies: 10_000, // Generous for tests
            max_relay_bytes: 256 * 1024 * 1024,
            max_room_relay_bytes: 1024 * 1024 * 1024,
        },
        empty_room_timeout: Duration::from_secs(5), // Fast timeout for tests
        inactive_room_timeout: Duration::from_secs(10),
        max_message_size: 65536, // 64KB default
        max_outbound_message_size: 8 * 1024 * 1024,
        max_signal_bytes: 16384,         // 16KB default
        max_connection_info_bytes: 8192, // 8KB default
        max_game_data_bytes: None,       // Per-encoding caps off by default (#634)
        max_connections_per_ip: 100,     // Generous for tests
        max_connections: 10_000,         // Generous server-wide ceiling for tests
        require_metrics_auth: false,     // No auth for tests
        metrics_auth_token: None,
        reconnection_window: Duration::from_secs(300), // 5 minutes for tests
        event_buffer_size: 100,                        // Buffer 100 events
        enable_reconnection: true,                     // Enable reconnection in tests
        websocket_config: signal_fish_server::config::WebSocketConfig::default(),
        app_id_allowlist_enabled: false, // Keep the app-ID policy open for tests
        heartbeat_throttle: Duration::ZERO, // No throttling in tests for predictable behavior
        region_id: "test".to_string(),
        room_code_prefix: None,
        default_max_spectators: None,
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

/// Open an h2 client only after the server advertises RFC 8441 support.
#[allow(dead_code)]
pub async fn open_h2<S>(
    stream: S,
) -> (
    hyper::client::conn::http2::SendRequest<axum::body::Body>,
    tokio::task::JoinSet<Result<(), hyper::Error>>,
)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use hyper_util::rt::{TokioExecutor, TokioIo};
    let (sender, connection) = hyper::client::conn::http2::handshake::<_, _, axum::body::Body>(
        TokioExecutor::new(),
        TokioIo::new(stream),
    )
    .await
    .expect("h2 handshake");
    // JoinSet cancels the driver if an assertion or timeout drops this future.
    let mut driver = tokio::task::JoinSet::new();
    let (settings_tx, settings_rx) = tokio::sync::oneshot::channel();
    driver.spawn(async move {
        tokio::pin!(connection);
        let mut ticks = tokio::time::interval(Duration::from_millis(10));
        loop {
            tokio::select! {
                result = &mut connection => return result,
                _ = ticks.tick() => {
                    if connection.is_extended_connect_protocol_enabled() {
                        let _ = settings_tx.send(());
                        return connection.await;
                    }
                }
            }
        }
    });
    settings_rx
        .await
        .expect("server must advertise extended CONNECT");
    (sender, driver)
}

#[allow(dead_code)]
pub async fn authenticate_over_h2<S>(stream: S, port: u16, path: &str, scheme: &str)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use futures_util::{SinkExt, StreamExt};
    use hyper_util::rt::TokioIo;
    use serde_json::{json, Value};
    use tokio_tungstenite::{
        tungstenite::{protocol::Role, Message},
        WebSocketStream,
    };
    let operation = async {
        let (mut sender, mut driver) = open_h2(stream).await;
        let request = hyper::Request::builder()
            .method(hyper::Method::CONNECT)
            .uri(format!("{scheme}://localhost:{port}{path}"))
            .header("sec-websocket-version", "13")
            .header("origin", "https://allowed.example")
            .extension(hyper::ext::Protocol::from_static("websocket"))
            .body(axum::body::Body::empty())
            .expect("extended CONNECT request");
        let response = sender.send_request(request).await.expect("h2 response");
        assert_eq!(response.status(), hyper::StatusCode::OK, "{scheme} {path}");
        let upgraded = hyper::upgrade::on(response).await.expect("h2 upgrade");
        let mut socket =
            WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Client, None).await;
        socket
            .send(Message::Text(
                json!({
                    "type": "Authenticate",
                    "data": { "app_id": "h2-route-test" }
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("h2 Authenticate");
        let mut authenticated = false;
        loop {
            let frame = socket
                .next()
                .await
                .expect("h2 auth frame")
                .expect("valid h2 websocket frame");
            if let Message::Text(text) = frame {
                let reply: Value = serde_json::from_str(&text).expect("h2 server JSON");
                match reply["type"].as_str() {
                    Some("Authenticated") => authenticated = true,
                    Some("ProtocolInfo") => {
                        assert!(authenticated, "Authenticate must precede ProtocolInfo");
                        let expected = if path == "/v3/ws" {
                            json!(3)
                        } else {
                            Value::Null
                        };
                        assert_eq!(
                            reply["data"]["protocol_version"], expected,
                            "{scheme} {path}"
                        );
                        break;
                    }
                    _ => panic!("unexpected h2 authentication reply: {reply}"),
                }
            }
        }
        socket.send(Message::Text(json!({
            "type": "JoinRoom",
            "data": { "game_name": "h2-test-game", "player_name": "h2-peer", "max_players": 4 }
        }).to_string().into())).await.expect("h2 JoinRoom");
        loop {
            let frame = socket
                .next()
                .await
                .expect("h2 room frame")
                .expect("valid h2 room frame");
            if let Message::Text(text) = frame {
                let reply: Value = serde_json::from_str(&text).expect("h2 room JSON");
                assert_eq!(reply["type"], "RoomJoined", "{scheme} {path}: {reply}");
                break;
            }
        }
        socket.close(None).await.expect("close h2 WebSocket");
        driver.abort_all();
        while driver.join_next().await.is_some() {}
    };
    tokio::time::timeout(Duration::from_secs(15), operation)
        .await
        .expect("h2 authentication deadline");
}
