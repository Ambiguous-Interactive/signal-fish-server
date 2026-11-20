use signal_fish_server::{
    config,
    database::DatabaseConfig,
    server::{run_drain_choreography, EnhancedGameServer, ServerConfig},
    websocket,
};
use std::{future::Future, net::SocketAddr, sync::Arc, time::Duration};

#[cfg(not(test))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = config::load()?;
    let game_server = build_server(&cfg).await?;
    let listener =
        tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], cfg.port))).await?;
    println!("Server listening on {}", listener.local_addr()?);
    serve_until(
        listener,
        game_server,
        &cfg.security.cors_origins,
        tokio::signal::ctrl_c(),
    )
    .await
}

pub async fn build_server(cfg: &config::Config) -> anyhow::Result<Arc<EnhancedGameServer>> {
    config::validate_config_security(cfg)?;
    let server_config = ServerConfig {
        default_max_players: cfg.server.default_max_players,
        ping_timeout: Duration::from_secs(cfg.server.ping_timeout),
        room_cleanup_interval: Duration::from_secs(cfg.server.room_cleanup_interval),
        drain_grace: Duration::from_secs(cfg.server.drain_grace_secs),
        max_rooms_per_game: cfg.server.max_rooms_per_game,
        max_rooms: cfg.server.max_rooms,
        rate_limit_config: signal_fish_server::rate_limit::RateLimitConfig {
            max_room_creations: cfg.rate_limit.max_room_creations,
            time_window: Duration::from_secs(cfg.rate_limit.time_window),
            max_join_attempts: cfg.rate_limit.max_join_attempts,
            max_signals: cfg.rate_limit.max_signals,
            max_signal_errors: cfg.rate_limit.max_signal_errors,
            max_inbound_error_replies: cfg.rate_limit.max_inbound_error_replies,
            max_relay_bytes: cfg.rate_limit.max_relay_bytes,
            max_room_relay_bytes: cfg.rate_limit.max_room_relay_bytes,
        },
        empty_room_timeout: Duration::from_secs(cfg.server.empty_room_timeout),
        inactive_room_timeout: Duration::from_secs(cfg.server.inactive_room_timeout),
        max_message_size: cfg.security.max_message_size,
        max_outbound_message_size: cfg.security.max_outbound_message_size,
        max_signal_bytes: cfg.security.max_signal_bytes,
        max_connection_info_bytes: cfg.security.max_connection_info_bytes,
        max_game_data_bytes: cfg.security.max_game_data_bytes.clone(),
        max_connections_per_ip: cfg.security.max_connections_per_ip,
        max_connections: cfg.security.max_connections,
        require_metrics_auth: cfg.security.require_metrics_auth,
        metrics_auth_token: cfg.security.metrics_auth_token.clone(),
        reconnection_window: Duration::from_secs(cfg.server.reconnection_window),
        event_buffer_size: cfg.server.event_buffer_size,
        enable_reconnection: cfg.server.enable_reconnection,
        websocket_config: cfg.websocket.clone(),
        app_id_allowlist_enabled: cfg.security.enforce_app_id_allowlist,
        heartbeat_throttle: Duration::from_secs(cfg.server.heartbeat_throttle_secs),
        region_id: cfg.server.region_id.clone(),
        room_code_prefix: cfg.server.room_code_prefix.clone(),
        default_max_spectators: cfg.server.default_max_spectators,
    };
    let game_server = EnhancedGameServer::new(
        server_config,
        cfg.protocol.clone(),
        cfg.relay_types.clone(),
        cfg.session.clone(),
        cfg.turn.clone(),
        DatabaseConfig::InMemory,
        cfg.metrics.clone(),
        cfg.coordination.clone(),
        cfg.security.transport.clone(),
        cfg.security.allowed_apps.clone(),
    )
    .await?;
    game_server.install_connect_token_key(&cfg.security)?;
    Ok(game_server)
}

pub async fn serve_until(
    listener: tokio::net::TcpListener,
    game_server: Arc<EnhancedGameServer>,
    cors_origins: &str,
    shutdown: impl Future<Output = std::io::Result<()>> + Send + 'static,
) -> anyhow::Result<()> {
    let timeouts = websocket::HttpServeTimeouts::production(Duration::from_secs(
        game_server
            .config()
            .websocket_config
            .http_header_read_timeout_secs,
    ));
    let router = websocket::create_standalone_router(cors_origins).with_state(game_server.clone());
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let mut cleanup_shutdown_rx = shutdown_rx.clone();
    let cleanup_server = game_server.clone();
    let mut cleanup = tokio::task::JoinSet::new();
    cleanup.spawn(async move {
        cleanup_server
            .cleanup_task_until(async move {
                let _ = cleanup_shutdown_rx.wait_for(|stopping| *stopping).await;
            })
            .await;
    });
    let result = async {
        let serve = websocket::serve_with_http_header_deadline(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
            timeouts,
            shutdown_rx,
        );
        let drain = async {
            let signal_result = shutdown.await;
            run_drain_choreography(&game_server, shutdown_tx).await;
            signal_result
        };
        tokio::pin!(serve, drain);
        tokio::select! {
            result = &mut serve => {
                // The listener can finish while upgraded WebSockets are still draining.
                result?;
                drain.await
            }
            signal_result = &mut drain => {
                // Arm this bound after the WebSocket grace and close writes.
                let settle = websocket::registered_connection_shutdown_settle_timeout();
                let served = tokio::time::timeout(settle, serve).await;
                signal_result?;
                served.map_err(|_| std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "HTTP connections did not close after the shutdown drain",
                ))?
            }
        }
    }
    .await;
    if let Some(joined) = cleanup.join_next().await {
        joined?;
    }
    result?;
    Ok(())
}
