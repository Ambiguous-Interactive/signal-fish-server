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
    ClientCertificateFingerprint, CLIENT_FINGERPRINT_HEADER_CANDIDATES,
};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket;
use std::{convert::Infallible, net::SocketAddr, sync::Arc};

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

    // Handle --print-config: output the loaded configuration as JSON
    if cli.print_config {
        let json = serde_json::to_string_pretty(&*cfg)
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

    let port: u16 = cfg.port;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    tracing::info!(%addr, "Starting Signal Fish server");

    // Create server configuration from loaded config
    let server_config = ServerConfig {
        default_max_players: cfg.server.default_max_players,
        ping_timeout: tokio::time::Duration::from_secs(cfg.server.ping_timeout),
        room_cleanup_interval: tokio::time::Duration::from_secs(cfg.server.room_cleanup_interval),
        max_rooms_per_game: cfg.server.max_rooms_per_game,
        rate_limit_config: signal_fish_server::rate_limit::RateLimitConfig {
            max_room_creations: cfg.rate_limit.max_room_creations,
            time_window: tokio::time::Duration::from_secs(cfg.rate_limit.time_window),
            max_join_attempts: cfg.rate_limit.max_join_attempts,
        },
        empty_room_timeout: tokio::time::Duration::from_secs(cfg.server.empty_room_timeout),
        inactive_room_timeout: tokio::time::Duration::from_secs(cfg.server.inactive_room_timeout),
        max_message_size: cfg.security.max_message_size,
        max_connections_per_ip: cfg.security.max_connections_per_ip,
        require_metrics_auth: cfg.security.require_metrics_auth,
        metrics_auth_token: cfg.security.metrics_auth_token.clone(),
        reconnection_window: tokio::time::Duration::from_secs(cfg.server.reconnection_window),
        event_buffer_size: cfg.server.event_buffer_size,
        enable_reconnection: cfg.server.enable_reconnection,
        websocket_config: cfg.websocket.clone(),
        auth_enabled: cfg.security.require_websocket_auth,
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
        database_config,
        cfg.metrics.clone(),
        cfg.auth.clone(),
        cfg.coordination.clone(),
        cfg.security.transport.clone(),
        cfg.security.authorized_apps.clone(),
    )
    .await?;

    // Start cleanup task
    let cleanup_server = game_server.clone();
    tokio::spawn(async move {
        cleanup_server.cleanup_task().await;
    });

    // Create enhanced protocol router with CORS configuration
    let enhanced_router =
        websocket::create_router(&cfg.security.cors_origins).with_state(game_server.clone());

    // Parse CORS origins for top-level router
    use tower_http::cors::{Any, CorsLayer};

    let cors = if cfg.security.cors_origins == "*" {
        CorsLayer::permissive()
    } else {
        let origins: Vec<_> = cfg
            .security
            .cors_origins
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

    // Complete the router
    let combined_router = combined_router
        .nest("/v2", enhanced_router) // Enhanced protocol under /v2
        .fallback(|| async {
            "Signal Fish Server. Use /v2/ws for WebSocket protocol, /v1/metrics for metrics, /metrics/prom for Prometheus."
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

        axum_server::bind_rustls(addr, tls_config)
            .serve(make_service)
            .await?;

        return Ok(());
    }

    // Start the server over plain TCP (typically behind a reverse proxy).
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(
        %addr,
        cors_origins = %cfg.security.cors_origins,
        "Server started over HTTP - Enhanced protocol: /v2/ws, Metrics: /v1/metrics"
    );

    axum::serve(listener, make_service).await?;

    Ok(())
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
    use super::Cli;
    use clap::Parser;

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
}
