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
    let router = websocket::create_standalone_router(cors_origins).with_state(game_server.clone());
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let cleanup_server = game_server.clone();
    let cleanup = tokio::spawn(async move {
        cleanup_server
            .cleanup_task_until(async move {
                let _ = shutdown_rx.wait_for(|stopping| *stopping).await;
            })
            .await;
    });
    let drain_server = game_server.clone();
    let drain_shutdown_tx = shutdown_tx.clone();
    let (signal_result_tx, signal_result_rx) = tokio::sync::oneshot::channel();
    let result = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let signal_result = shutdown.await;
        run_drain_choreography(&drain_server, drain_shutdown_tx).await;
        let _ = signal_result_tx.send(signal_result);
    })
    .await;
    if !game_server.is_draining() {
        run_drain_choreography(&game_server, shutdown_tx).await;
    }
    cleanup.await?;
    result?;
    signal_result_rx.await??;
    Ok(())
}
