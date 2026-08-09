#![cfg_attr(not(test), deny(clippy::panic))]

use axum::extract::Request;
use axum::http::HeaderMap;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::Router;
use clap::Parser;
use signal_fish_server::config;
use signal_fish_server::database::DatabaseConfig;
use signal_fish_server::logging;
use signal_fish_server::security::{
    ClientCertificateFingerprint, OriginPolicy, CLIENT_FINGERPRINT_HEADER_CANDIDATES,
};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket;
use std::{convert::Infallible, net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::watch;

/// Signal Fish -- lightweight WebSocket signaling server for P2P game networking
#[derive(Parser, Debug)]
#[command(name = "signal-fish-server")]
#[command(about = "A lightweight, in-memory WebSocket signaling server for P2P game networking")]
#[command(version)]
struct Cli {
    /// Validate configuration and exit without starting the server.
    /// Useful for CI/CD pipelines and pre-deployment checks.
    #[arg(long, short = 'c', conflicts_with = "print_config")]
    validate_config: bool,

    /// Print the loaded configuration to stdout (as JSON) and exit.
    /// Useful for debugging configuration loading from multiple sources.
    #[arg(long, conflicts_with = "validate_config")]
    print_config: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Load configuration from config.json if present; otherwise use code defaults.
    let cfg = Arc::new(config::load());

    // Handle --print-config: output the loaded configuration as JSON. Secrets
    // (TURN secrets, metrics tokens, ICE credentials) are redacted so credential
    // material never reaches stdout — see Config::redacted_for_display.
    if cli.print_config {
        let json = serde_json::to_string_pretty(&cfg.redacted_for_display())
            .map_err(|e| anyhow::anyhow!("Failed to serialize config: {e}"))?;
        println!("{json}");
        return Ok(());
    }

    // Validate configuration security. Note: config::load() already calls validate_config_security()
    // but only logs errors to stderr and continues. Here we capture the result to:
    // 1. Provide proper exit code for --validate-config mode
    // 2. Fail startup in production if critical settings are missing
    let validation_result = config::validate_config_security(&cfg);

    // Handle --validate-config: exit after validation
    if cli.validate_config {
        match validation_result {
            Ok(()) => {
                println!("Configuration validation passed");
                println!();
                println!("Configuration summary:");
                println!("  Port: {}", cfg.port);
                println!("  Storage backend: InMemory");
                println!("  TLS enabled: {}", cfg.security.transport.tls.enabled);
                println!(
                    "  Metrics auth required: {}",
                    cfg.security.require_metrics_auth
                );
                println!("  Reconnection enabled: {}", cfg.server.enable_reconnection);
                println!("  Max players per room: {}", cfg.server.default_max_players);
                println!("  Deployment region: {}", cfg.server.region_id);
                return Ok(());
            }
            Err(e) => {
                eprintln!("Configuration validation failed:\n{e}");
                std::process::exit(1);
            }
        }
    }

    // In normal operation, propagate validation errors
    validation_result?;

    // Initialize logging from config.
    logging::init_with_config(&cfg.logging);

    // DTLS fingerprints travel inside the SDP that `Signal`
    // relays, so a server actively brokering WebRTC (TURN enabled) should have
    // its signaling terminated over wss://. Reverse-proxy TLS termination is
    // the common deployment, so this is a once-at-startup warning, never a
    // hard error. Emitted here (after logging init) so it actually reaches the
    // configured tracing subscriber.
    if config::should_warn_missing_signaling_tls(&cfg) {
        tracing::warn!(
            "TURN is enabled but built-in TLS is disabled: serve signaling over wss:// in \
             production (enable security.transport.tls, or terminate TLS at a reverse proxy). \
             DTLS fingerprints travel in SDP, so plaintext ws:// signaling allows \
             man-in-the-middle of the WebRTC peer connections."
        );
    }

    let port: u16 = cfg.port;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    tracing::info!(%addr, "Starting Signal Fish server");
    tracing::info!(
        deployment_mode = "single_instance",
        room_state = "in_memory",
        room_affinity_required = true,
        session_handoff = false,
        "Deployment contract: one process owns each room; losing the process loses its rooms"
    );

    // Create server configuration from loaded config
    let server_config = ServerConfig {
        default_max_players: cfg.server.default_max_players,
        ping_timeout: tokio::time::Duration::from_secs(cfg.server.ping_timeout),
        room_cleanup_interval: tokio::time::Duration::from_secs(cfg.server.room_cleanup_interval),
        drain_grace: tokio::time::Duration::from_secs(cfg.server.drain_grace_secs),
        max_rooms_per_game: cfg.server.max_rooms_per_game,
        rate_limit_config: signal_fish_server::rate_limit::RateLimitConfig {
            max_room_creations: cfg.rate_limit.max_room_creations,
            time_window: tokio::time::Duration::from_secs(cfg.rate_limit.time_window),
            max_join_attempts: cfg.rate_limit.max_join_attempts,
            max_signals: cfg.rate_limit.max_signals,
            max_signal_errors: cfg.rate_limit.max_signal_errors,
        },
        empty_room_timeout: tokio::time::Duration::from_secs(cfg.server.empty_room_timeout),
        inactive_room_timeout: tokio::time::Duration::from_secs(cfg.server.inactive_room_timeout),
        max_message_size: cfg.security.max_message_size,
        max_signal_bytes: cfg.security.max_signal_bytes,
        max_connections_per_ip: cfg.security.max_connections_per_ip,
        require_metrics_auth: cfg.security.require_metrics_auth,
        metrics_auth_token: cfg.security.metrics_auth_token.clone(),
        reconnection_window: tokio::time::Duration::from_secs(cfg.server.reconnection_window),
        event_buffer_size: cfg.server.event_buffer_size,
        enable_reconnection: cfg.server.enable_reconnection,
        websocket_config: cfg.websocket.clone(),
        app_id_allowlist_enabled: cfg.security.enforce_app_id_allowlist,
        heartbeat_throttle: tokio::time::Duration::from_secs(cfg.server.heartbeat_throttle_secs),
        region_id: cfg.server.region_id.clone(),
        room_code_prefix: cfg.server.room_code_prefix.clone(),
    };

    // Always use in-memory storage
    let database_config = DatabaseConfig::InMemory;

    // Create the enhanced game server
    let game_server = EnhancedGameServer::new(
        server_config,
        cfg.protocol.clone(),
        cfg.relay_types.clone(),
        cfg.session.clone(),
        cfg.turn.clone(),
        database_config,
        cfg.metrics.clone(),
        cfg.coordination.clone(),
        cfg.security.transport.clone(),
        cfg.security.allowed_apps.clone(),
    )
    .await?;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Start cleanup task
    let cleanup_server = game_server.clone();
    let cleanup_shutdown_rx = shutdown_rx.clone();
    let cleanup_task = tokio::spawn(async move {
        cleanup_server
            .cleanup_task_until(wait_for_shutdown(cleanup_shutdown_rx))
            .await;
    });

    let shutdown_server = game_server.clone();
    let shutdown_task = tokio::spawn(run_shutdown_drain(shutdown_server, shutdown_tx.clone()));

    // One parsed policy governs HTTP CORS responses and both WebSocket
    // upgrades, so the browser-facing allowlist cannot drift between layers.
    let origin_policy = OriginPolicy::parse(&cfg.security.cors_origins)?;
    let cors = origin_policy.cors_layer();
    let enhanced_router = websocket::create_router_with_origin_policy(origin_policy.clone())
        .with_state(game_server.clone());

    use axum::routing::get;

    // Build base router with metrics endpoints
    #[allow(unused_mut)]
    let mut combined_router = Router::new()
        .route("/v1/metrics", get(websocket::metrics_handler))
        .route("/metrics", get(websocket::metrics_handler))
        .route(
            "/v1/metrics/prom",
            get(websocket::prometheus_metrics_handler),
        )
        .route("/metrics/prom", get(websocket::prometheus_metrics_handler));

    // Spawn legacy full-mesh signaling on a separate port if enabled
    #[cfg(feature = "legacy-fullmesh")]
    {
        let legacy_port = port.saturating_add(1);
        let legacy_addr = SocketAddr::from(([0, 0, 0, 0], legacy_port));
        let legacy_server = matchbox_signaling::SignalingServer::full_mesh_builder(legacy_addr)
            .cors()
            .trace()
            .build();

        tokio::spawn(async move {
            if let Err(e) = legacy_server.serve().await {
                tracing::error!(error = %e, "Legacy full-mesh signaling server stopped");
            }
        });
        tracing::info!(
            %legacy_addr,
            "Legacy full-mesh signaling mode enabled on separate port"
        );
    }

    // Complete the router.
    //
    // PATH NESTING: the enhanced protocol router (`/ws`, `/health`, ...) is
    // nested under `/v2`, so its WebSocket entry point is `/v2/ws`. The `/v3/ws`
    // alias is mounted directly at the top level and shares the SAME connection
    // handler, differing only in the fallback protocol version (3 vs 2) applied
    // when the client omits `Authenticate.protocol_version`. `/v2/ws` behavior is
    // byte-for-byte unchanged.
    let combined_router = combined_router
        .nest("/v2", enhanced_router) // Enhanced protocol under /v2
        .route(
            "/v3/ws",
            websocket::websocket_route_v3_with_origin_policy(origin_policy.clone()),
        ) // v3 alias, shared handler
        .fallback(|| async {
            "Signal Fish Server. Use /v2/ws (or /v3/ws) for WebSocket protocol, /v1/metrics for metrics, /metrics/prom for Prometheus."
        })
        .layer(middleware::from_fn(capture_client_fingerprint))
        .with_state(game_server)
        .layer(cors);

    let make_service = combined_router.into_make_service_with_connect_info::<SocketAddr>();

    #[cfg(feature = "tls")]
    if cfg.security.transport.tls.enabled {
        let tls_config =
            signal_fish_server::security::build_rustls_config(&cfg.security.transport.tls)
                .map_err(|err| anyhow::anyhow!("failed to initialize TLS configuration: {err}"))?;

        tracing::info!(
            %addr,
            client_auth = ?cfg.security.transport.tls.client_auth,
            "Server started over HTTPS with TLS enabled - Enhanced protocol: /v2/ws, Metrics: /v1/metrics"
        );

        let tls_handle = axum_server::Handle::new();
        let tls_shutdown_handle = tls_handle.clone();
        let tls_shutdown_rx = shutdown_rx.clone();
        tokio::spawn(async move {
            wait_for_shutdown(tls_shutdown_rx).await;
            tls_shutdown_handle.graceful_shutdown(None);
        });

        let listener = websocket::bind_tcp_listener(addr, cfg.websocket.socket_send_buffer_bytes)?
            .into_std()?;
        let serve_result = axum_server::from_tcp_rustls(listener, tls_config)?
            // Disable Nagle on the raw TCP stream before the TLS handshake (#197).
            .map(|rustls| rustls.acceptor(websocket::ConfiguredAcceptor))
            .handle(tls_handle)
            .serve(make_service)
            .await;

        finish_background_shutdown(shutdown_tx, shutdown_rx, shutdown_task, cleanup_task).await;
        serve_result?;

        return Ok(());
    }

    // Start the server over plain TCP (typically behind a reverse proxy).
    // Accepted sockets are configured for low-latency relay (#197).
    let listener = websocket::bind_serve_listener(addr, cfg.websocket.socket_send_buffer_bytes)?;
    tracing::info!(
        %addr,
        cors_origins = %cfg.security.cors_origins,
        "Server started over HTTP - Enhanced protocol: /v2/ws, Metrics: /v1/metrics"
    );

    let serve_result = axum::serve(listener, make_service)
        .with_graceful_shutdown(wait_for_shutdown(shutdown_rx.clone()))
        .await;

    finish_background_shutdown(shutdown_tx, shutdown_rx, shutdown_task, cleanup_task).await;
    serve_result?;

    Ok(())
}

async fn run_shutdown_drain(server: Arc<EnhancedGameServer>, shutdown_tx: watch::Sender<bool>) {
    shutdown_signal().await;

    let drain_started_at = tokio::time::Instant::now();
    let drain = server.begin_shutdown_drain();
    tracing::info!(
        deadline_ms = drain.deadline_ms,
        grace_ms = drain.grace.as_millis() as u64,
        started_by_this_call = drain.started_by_this_call,
        "Server shutdown drain started"
    );
    let going_away_sent = server.announce_shutdown_drain(drain).await;
    tracing::info!(
        going_away_sent,
        started_by_this_call = drain.started_by_this_call,
        "Shutdown GoingAway advisories enqueued"
    );

    let _ = shutdown_tx.send(true);

    let wait_before_close = drain.wait_before_close(drain_started_at.elapsed());
    if wait_before_close > std::time::Duration::ZERO {
        tokio::time::sleep(wait_before_close).await;
    }

    let close_requests = server.close_connections_for_shutdown();
    tracing::info!(close_requests, "Shutdown close requests issued");

    let settle_timeout = shutdown_connection_settle_timeout();
    let remaining_connections = server.wait_for_shutdown_connections(settle_timeout).await;
    if remaining_connections > 0 {
        tracing::warn!(
            remaining_connections,
            settle_ms = settle_timeout.as_millis() as u64,
            "Shutdown drain ended with connections still registered"
        );
    }
}

fn shutdown_connection_settle_timeout() -> Duration {
    websocket::registered_connection_shutdown_settle_timeout()
}

async fn wait_for_shutdown(mut shutdown_rx: watch::Receiver<bool>) {
    loop {
        if *shutdown_rx.borrow() {
            return;
        }
        if shutdown_rx.changed().await.is_err() {
            return;
        }
    }
}

async fn finish_background_shutdown(
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    shutdown_task: tokio::task::JoinHandle<()>,
    cleanup_task: tokio::task::JoinHandle<()>,
) {
    let shutdown_started = *shutdown_rx.borrow();
    let _ = shutdown_tx.send(true);
    if shutdown_started {
        let _ = shutdown_task.await;
    } else {
        shutdown_task.abort();
    }
    let _ = cleanup_task.await;
}

async fn shutdown_signal() {
    let ctrl_c = wait_for_ctrl_c_shutdown(tokio::signal::ctrl_c());

    #[cfg(unix)]
    {
        let terminate = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut signal) => {
                    signal.recv().await;
                }
                Err(err) => {
                    tracing::warn!(error = %err, "Failed to install SIGTERM shutdown handler");
                    std::future::pending::<()>().await;
                }
            }
        };
        tokio::select! {
            () = ctrl_c => {}
            () = terminate => {}
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await;
    }
}

async fn wait_for_ctrl_c_shutdown(ctrl_c: impl std::future::Future<Output = std::io::Result<()>>) {
    if let Err(err) = ctrl_c.await {
        tracing::error!(error = %err, "Failed to install Ctrl+C shutdown handler");
        std::future::pending::<()>().await;
    }
}

async fn capture_client_fingerprint(mut req: Request, next: Next) -> Result<Response, Infallible> {
    if let Some(fingerprint) = extract_client_fingerprint(req.headers()) {
        req.extensions_mut().insert(fingerprint);
    }

    Ok(next.run(req).await)
}

fn extract_client_fingerprint(headers: &HeaderMap) -> Option<ClientCertificateFingerprint> {
    for header_name in CLIENT_FINGERPRINT_HEADER_CANDIDATES {
        if let Some(value) = headers
            .get(*header_name)
            .and_then(|value| value.to_str().ok())
        {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                continue;
            }
            return Some(ClientCertificateFingerprint {
                fingerprint: Arc::<str>::from(trimmed.to_owned()),
                source_header: header_name,
            });
        }
    }

    None
}

#[cfg(test)]
mod cli_tests {
    use super::{shutdown_connection_settle_timeout, wait_for_ctrl_c_shutdown, websocket, Cli};
    use clap::Parser;
    use std::io;
    use std::time::Duration;

    #[test]
    fn test_cli_default_no_flags() {
        let cli = Cli::try_parse_from(["signal-fish-server"]).unwrap();
        assert!(!cli.validate_config);
        assert!(!cli.print_config);
    }

    #[test]
    fn test_cli_validate_config_long() {
        let cli = Cli::try_parse_from(["signal-fish-server", "--validate-config"]).unwrap();
        assert!(cli.validate_config);
        assert!(!cli.print_config);
    }

    #[test]
    fn test_cli_validate_config_short() {
        let cli = Cli::try_parse_from(["signal-fish-server", "-c"]).unwrap();
        assert!(cli.validate_config);
        assert!(!cli.print_config);
    }

    #[test]
    fn test_cli_print_config() {
        let cli = Cli::try_parse_from(["signal-fish-server", "--print-config"]).unwrap();
        assert!(!cli.validate_config);
        assert!(cli.print_config);
    }

    #[test]
    fn test_cli_validate_and_print_config_conflict() {
        // --validate-config and --print-config are mutually exclusive
        let result =
            Cli::try_parse_from(["signal-fish-server", "--validate-config", "--print-config"]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("cannot be used with"));
    }

    #[test]
    fn test_cli_help_contains_flags() {
        // Verify help text mentions our flags
        let result = Cli::try_parse_from(["signal-fish-server", "--help"]);
        assert!(result.is_err()); // --help causes early exit which is an "error"
        let err = result.unwrap_err();
        let help_text = err.to_string();
        assert!(help_text.contains("--validate-config"));
        assert!(help_text.contains("--print-config"));
        assert!(help_text.contains("-c"));
    }

    #[test]
    fn test_cli_version() {
        let result = Cli::try_parse_from(["signal-fish-server", "--version"]);
        assert!(result.is_err()); // --version causes early exit
    }

    #[test]
    fn shutdown_connection_settle_timeout_covers_registered_close_sequence() {
        assert_eq!(
            shutdown_connection_settle_timeout(),
            websocket::CONNECTION_CLOSE_WRITE_TIMEOUT
                .saturating_mul(websocket::REGISTERED_SHUTDOWN_CLOSE_WRITE_STEPS)
                .saturating_add(websocket::REGISTERED_SHUTDOWN_SETTLE_MARGIN)
        );
        assert_eq!(
            websocket::REGISTERED_SHUTDOWN_CLOSE_WRITE_STEPS,
            4,
            "registered shutdown close uses flush, final delivery report, semantic close, \
             and sink close budgets"
        );
        assert!(
            websocket::REGISTERED_SHUTDOWN_SETTLE_MARGIN > Duration::ZERO,
            "handler cleanup needs scheduling margin after the final write budget"
        );
    }

    #[tokio::test]
    async fn ctrl_c_completion_allows_shutdown() {
        tokio::time::timeout(
            Duration::from_millis(100),
            wait_for_ctrl_c_shutdown(async { Ok(()) }),
        )
        .await
        .expect("successful Ctrl+C future should complete shutdown wait");
    }

    #[tokio::test]
    async fn ctrl_c_install_error_waits_forever() {
        let result = tokio::time::timeout(
            Duration::from_millis(25),
            wait_for_ctrl_c_shutdown(async {
                Err(io::Error::other("synthetic Ctrl+C installation failure"))
            }),
        )
        .await;

        assert!(
            result.is_err(),
            "Ctrl+C installation failure must not trigger shutdown"
        );
    }
}
