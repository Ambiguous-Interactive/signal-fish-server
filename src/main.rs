#![cfg_attr(not(test), deny(clippy::panic))]

use axum::Router;
use clap::Parser;
use signal_fish_server::config;
use signal_fish_server::database::DatabaseConfig;
use signal_fish_server::logging;
use signal_fish_server::security::OriginPolicy;
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket;
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::watch;

use std::fmt::Write as _;

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

/// Write one line to an `io::Write` sink: the value, then a newline.
fn write_line_to<W: std::io::Write>(out: &mut W, value: &str) -> std::io::Result<()> {
    out.write_all(value.as_bytes())?;
    out.write_all(b"\n")
}

/// Write one line to stdout, treating an unwritable stdout as a clean CLI
/// error instead of a panic: `--print-config`/`--validate-config` run in
/// pipelines and containers where stdout can be a full disk, a closed file
/// descriptor, or a dead pipe, and the resulting `println!` panic (exit 101)
/// would both violate the zero-panic bootstrap policy and misreport the
/// failure to callers.
fn emit_to_stdout_or_exit(value: &str) {
    if let Err(error) = write_line_to(&mut std::io::stdout().lock(), value) {
        // The stderr fallback is best-effort too (`let _ =`): if stderr is
        // broken there is nowhere left to report, and a panic here would
        // replace the intended exit(1) with an abort.
        let _ = write_line_to(
            &mut std::io::stderr().lock(),
            &format!("error: cannot write to stdout: {error}"),
        );
        std::process::exit(1);
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Load configuration from config.json if present; otherwise use code defaults.
    // A present-but-invalid source (unparsable JSON, type-mismatched value) is a
    // hard error here: booting on defaults would silently revert every operator
    // setting while the process appears healthy.
    let cfg = Arc::new(config::load()?);

    // Handle --print-config: output the loaded configuration as JSON. Secrets
    // (TURN secrets, metrics tokens, ICE credentials) are redacted so credential
    // material never reaches stdout — see Config::redacted_for_display.
    if cli.print_config {
        let json = serde_json::to_string_pretty(&cfg.redacted_for_display())
            .map_err(|e| anyhow::anyhow!("Failed to serialize config: {e}"))?;
        emit_to_stdout_or_exit(&json);
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
                // `write!` to a `String` is infallible; the results are
                // discarded deliberately (the buffer always accepts).
                let mut summary =
                    String::from("Configuration validation passed\n\nConfiguration summary:\n");
                let _ = writeln!(summary, "  Port: {}", cfg.port);
                summary.push_str("  Storage backend: InMemory\n");
                let _ = writeln!(
                    summary,
                    "  TLS enabled: {}",
                    cfg.security.transport.tls.enabled
                );
                let _ = writeln!(
                    summary,
                    "  Metrics auth required: {}",
                    cfg.security.require_metrics_auth
                );
                let _ = writeln!(
                    summary,
                    "  App-ID allowlist enforced: {}",
                    cfg.security.enforce_app_id_allowlist
                );
                let _ = writeln!(
                    summary,
                    "  Registered applications: {}",
                    cfg.security.allowed_apps.len()
                );
                let _ = writeln!(
                    summary,
                    "  Reconnection enabled: {}",
                    cfg.server.enable_reconnection
                );
                let _ = writeln!(
                    summary,
                    "  Max players per room: {}",
                    cfg.server.default_max_players
                );
                let _ = writeln!(summary, "  Deployment region: {}", cfg.server.region_id);
                emit_to_stdout_or_exit(&summary);
                return Ok(());
            }
            Err(e) => {
                // Best-effort stderr (see `emit_to_stdout_or_exit`): a broken
                // stderr must not panic, only a truthful exit code remains.
                let _ = write_line_to(
                    &mut std::io::stderr().lock(),
                    &format!("Configuration validation failed:\n{e}"),
                );
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

    // Token binding's connection key derives from the handshake key plus a
    // server challenge. Over plaintext ws:// both are wire-visible, so proofs
    // authenticate nothing there; `required=true` already fails closed without
    // TLS, so only the optional mode needs this once-at-startup warning.
    if config::should_warn_unauthenticated_token_binding(&cfg) {
        tracing::warn!(
            "Token binding is enabled and optional but built-in TLS is disabled: over \
             plaintext ws:// the connection key is publicly derivable, so proofs provide \
             replay ordering only, not authentication (enable security.transport.tls, \
             terminate TLS at a reverse proxy, or set token_binding.required=true)."
        );
    }

    // Each liveness knob is legitimate to disable alone, but all three at once
    // means nothing ever reaps a silently-dead peer. Once-at-startup warning so
    // operators get the diagnostic before a stale seat explains it.
    if config::should_warn_all_liveness_disabled(&cfg) {
        tracing::warn!(
            "All liveness mechanisms are disabled (server.ping_timeout=0, \
              websocket.idle_timeout_secs=0, websocket.server_ping_interval_secs=0): a \
              silently-dead client keeps its connection, per-IP slot, and room seat \
              indefinitely with no reaping signal."
        );
    }

    // Effective security posture (issue #515): one info line always, so the
    // mode a deployment actually runs under is visible in `docker logs`, plus
    // a warn line per disabled gate. The shipped image used to hard-code both
    // gates off via ENV; with that gone the remaining fail-open risk is an
    // operator config that disables them unknowingly, so say it out loud.
    tracing::info!(
        app_id_allowlist_enforced = cfg.security.enforce_app_id_allowlist,
        allowed_apps = cfg.security.allowed_apps.len(),
        metrics_auth_required = cfg.security.require_metrics_auth,
        "Effective security mode"
    );
    for warning in config::security_posture_warnings(&cfg) {
        tracing::warn!("{warning}");
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
        max_rooms: cfg.server.max_rooms,
        rate_limit_config: signal_fish_server::rate_limit::RateLimitConfig {
            max_room_creations: cfg.rate_limit.max_room_creations,
            time_window: tokio::time::Duration::from_secs(cfg.rate_limit.time_window),
            max_join_attempts: cfg.rate_limit.max_join_attempts,
            max_signals: cfg.rate_limit.max_signals,
            max_signal_errors: cfg.rate_limit.max_signal_errors,
            max_inbound_error_replies: cfg.rate_limit.max_inbound_error_replies,
            max_relay_bytes: cfg.rate_limit.max_relay_bytes,
            max_room_relay_bytes: cfg.rate_limit.max_room_relay_bytes,
        },
        empty_room_timeout: tokio::time::Duration::from_secs(cfg.server.empty_room_timeout),
        inactive_room_timeout: tokio::time::Duration::from_secs(cfg.server.inactive_room_timeout),
        max_message_size: cfg.security.max_message_size,
        max_outbound_message_size: cfg.security.max_outbound_message_size,
        max_signal_bytes: cfg.security.max_signal_bytes,
        max_connection_info_bytes: cfg.security.max_connection_info_bytes,
        max_connections_per_ip: cfg.security.max_connections_per_ip,
        max_connections: cfg.security.max_connections,
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
    // Signals choreography COMPLETION, not the process watch: the
    // choreography flips the process watch before its grace wait and coded
    // closes, so post-drain bounds must anchor here instead or they fire
    // mid-grace. The sender lives in the drain task, so a panic mid-drain
    // also releases the watchers (sender drop without a send).
    let (drain_done_tx, drain_done_rx) = watch::channel(false);
    let shutdown_task = tokio::spawn(run_shutdown_drain(
        shutdown_server,
        shutdown_tx.clone(),
        drain_done_tx,
    ));

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

    // Spawn legacy full-mesh signaling on a separate port if enabled.
    //
    // Shutdown semantics (deliberate, #454): `matchbox_signaling` 0.14 offers
    // no graceful-shutdown API, and the legacy protocol has no close-code
    // contract, so the task is left unwired and stops with the process at
    // runtime drop. Clients see an abrupt socket close either way; wiring an
    // abort here would add code without changing any observable behavior.
    #[cfg(feature = "legacy-fullmesh")]
    {
        // A main port of 65535 saturates to itself: the "sibling" listener
        // would collide with the main bind, and whichever lost the race would
        // either kill startup from a config that passed validation or run
        // without the mode the startup log claimed enabled. Refuse the top
        // port with a loud error and a working main server instead.
        match legacy_fullmesh_addr(port) {
            Some(legacy_addr) => {
                // Guardrail (#526): the legacy plane carries none of the main
                // service's admission controls. Say so at startup so an
                // operator who compiled it in cannot miss what they exposed.
                tracing::warn!(
                    %legacy_addr,
                    "legacy fullmesh: UNAUTHENTICATED full-mesh relay on a permissive CORS \
                     surface — no app allowlist, no rate limits, no telemetry, no graceful \
                     shutdown; it must never face the public internet in hosted deployments"
                );
                let legacy_server =
                    matchbox_signaling::SignalingServer::full_mesh_builder(legacy_addr)
                        .cors()
                        .trace()
                        .build();

                tokio::spawn(async move {
                    if let Err(e) = legacy_server.serve().await {
                        tracing::error!(error = %e, "Legacy full-mesh signaling server stopped");
                    }
                });
                // Truthful voice: the legacy task binds inside `serve()` (the
                // #454 rationale above keeps it unwired), so this announces
                // the attempt, not a bound listener.
                tracing::info!(
                    %legacy_addr,
                    "Starting legacy full-mesh signaling server on separate port"
                );
            }
            None => {
                tracing::error!(
                    port,
                    "Cannot enable legacy full-mesh signaling: the main port is 65535, so \
                     the derived sibling port would collide with the main listener"
                );
            }
        }
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
        .route("/v3/client-config", websocket::client_config_route())
        // The conventional top-level probe paths: without these routes a
        // prober hitting `/health` would get the 200-OK fallback banner
        // regardless of backend state, and `/readyz` gives orchestrators the
        // drain-aware readiness verdict (/health is liveness-only, #521).
        .route("/health", websocket::health_route())
        .route("/readyz", websocket::readyz_route())
        .fallback(|| async {
            "Signal Fish Server. Use /v2/ws (or /v3/ws) for WebSocket protocol, /v2/client-config (or /v3/client-config) for client limits, /v1/metrics for metrics, /metrics/prom for Prometheus, /readyz for readiness."
        })
        // Cloned, not moved: the original handle stays available for the
        // post-serve shutdown join, which must observe the drain state.
        .with_state(game_server.clone())
        .layer(cors);

    let make_service = combined_router.into_make_service_with_connect_info::<SocketAddr>();

    // The settle budget the drain choreography grants registered connections
    // for their close frames. It is also the bound on how long the process
    // will keep the listener alive after the drain completes: a client parked
    // mid-HTTP-request (or mid-TLS handshake) can otherwise hold the serve
    // future open forever, hanging the process after a *successful* drain.
    let post_drain_settle_budget = websocket::registered_connection_shutdown_settle_timeout();

    #[cfg(feature = "tls")]
    if cfg.security.transport.tls.enabled {
        let tls_config =
            signal_fish_server::security::build_rustls_config(&cfg.security.transport.tls)
                .map_err(|err| anyhow::anyhow!("failed to initialize TLS configuration: {err}"))?;

        let tls_handle = axum_server::Handle::new();
        let tls_shutdown_handle = tls_handle.clone();
        let tls_drain_done_rx = drain_done_rx.clone();
        tokio::spawn(async move {
            // Arm the bounded graceful shutdown only when the choreography
            // has COMPLETED (sender drop on a panicked drain releases this
            // too): arming at the process-watch flip would force-close every
            // connection before the choreography's grace wait and coded
            // `4000` close step even run.
            wait_for_shutdown(tls_drain_done_rx).await;
            // Bounded, not unbounded: `None` waits for *every* connection to
            // end, and a stalled client (e.g. one parked mid-handshake with
            // partial bytes buffered) never ends, so the serve future — and
            // the process — would hang forever after a completed drain.
            tls_shutdown_handle.graceful_shutdown(Some(post_drain_settle_budget));
        });

        let listener = websocket::bind_tcp_listener(addr, cfg.websocket.socket_send_buffer_bytes)?
            .into_std()?;
        let mut server = axum_server::from_tcp_rustls(listener, tls_config)?
            // Disable Nagle on the raw TCP stream before the TLS handshake (#197).
            .map(|rustls| {
                signal_fish_server::security::VerifiedClientCertificateAcceptor::new(
                    rustls.acceptor(websocket::ConfiguredAcceptor),
                )
            })
            .handle(tls_handle);
        // Arm the pre-upgrade header-read deadline on the TLS path too:
        // axum-server also leaves hyper's Timer unset, so its header-read
        // timeout would otherwise stay inert (issue #518).
        {
            use hyper_util::rt::TokioTimer;
            server
                .http_builder()
                .http1()
                .timer(TokioTimer::new())
                .header_read_timeout(std::time::Duration::from_secs(
                    cfg.websocket.http_header_read_timeout_secs,
                ));
        }
        // Log "started" only after the bind and TLS setup have actually
        // succeeded: a log scraper reading the earlier placement would see a
        // successful start that never happened whenever the port was taken or
        // the material invalid. The plain path below already had this order.
        tracing::info!(
            %addr,
            client_auth = ?cfg.security.transport.tls.client_auth,
            "Server started over HTTPS with TLS enabled - Enhanced protocol: /v2/ws, Metrics: /v1/metrics"
        );
        let serve_result = server.serve(make_service).await;

        finish_background_shutdown(&game_server, shutdown_tx, shutdown_task, cleanup_task).await?;
        serve_result?;

        return Ok(());
    }

    // Start the server over plain TCP (typically behind a reverse proxy).
    // Accepted sockets are configured for low-latency relay (#197), and the
    // HTTP header-read deadline is armed explicitly: axum::serve leaves
    // hyper's Timer unset, silently disabling hyper's header-read timeout
    // (issue #518).
    let listener = websocket::bind_tcp_listener(addr, cfg.websocket.socket_send_buffer_bytes)?;
    let http_header_read_timeout =
        std::time::Duration::from_secs(cfg.websocket.http_header_read_timeout_secs);
    tracing::info!(
        %addr,
        cors_origins = %cfg.security.cors_origins,
        http_header_read_timeout_secs = cfg.websocket.http_header_read_timeout_secs,
        "Server started over HTTP - Enhanced protocol: /v2/ws, Metrics: /v1/metrics"
    );

    let serve_result = serve_with_post_drain_bound(
        websocket::serve_with_http_header_deadline(
            listener,
            make_service,
            http_header_read_timeout,
            shutdown_rx.clone(),
        ),
        drain_done_rx,
        post_drain_settle_budget,
    )
    .await;

    finish_background_shutdown(&game_server, shutdown_tx, shutdown_task, cleanup_task).await?;
    match serve_result {
        Some(result) => result.map_err(|error| anyhow::anyhow!("server error: {error}"))?,
        None => anyhow::bail!(
            "forced exit: the listener did not stop within {post_drain_settle_budget:?} after \
             the drain completed (a client likely holds a stalled partial request)"
        ),
    }

    Ok(())
}

/// Resolves once the drain choreography has **completed** — or its task has
/// died without completing (sender drop) — and the settle budget has elapsed:
/// the moment a still-running serve future has outlived every close-frame
/// budget it was granted. Deliberately NOT anchored to the process watch: the
/// choreography flips that watch before its grace wait and coded closes, so a
/// watch-anchored bound would fire mid-drain on every healthy shutdown.
async fn post_drain_settle_bound(
    drain_done_rx: watch::Receiver<bool>,
    settle: std::time::Duration,
) {
    wait_for_shutdown(drain_done_rx).await;
    tokio::time::sleep(settle).await;
}

/// Await the serve future, but never unboundedly: once the drain choreography
/// has finished and `settle` has elapsed without the serve future returning (a
/// client parked mid-HTTP-request keeps hyper's connection task alive, so
/// axum's graceful shutdown can wait forever), return `None` so the caller can
/// log and exit instead of hanging the process after a *successful* drain.
async fn serve_with_post_drain_bound(
    serve: impl std::future::Future<Output = std::io::Result<()>>,
    drain_done_rx: watch::Receiver<bool>,
    settle: std::time::Duration,
) -> Option<std::io::Result<()>> {
    tokio::pin!(serve);
    tokio::select! {
        result = &mut serve => Some(result),
        () = post_drain_settle_bound(drain_done_rx, settle) => {
            tracing::warn!(
                settle = ?settle,
                "drain completed but the listener did not stop within the settle budget; \
                 forcing exit"
            );
            None
        }
    }
}

#[cfg(feature = "legacy-fullmesh")]
/// The legacy full-mesh signaling address: the main port + 1. `None` when the
/// main port is the top of the range and no sibling port exists.
fn legacy_fullmesh_addr(port: u16) -> Option<SocketAddr> {
    Some(SocketAddr::from(([0, 0, 0, 0], port.checked_add(1)?)))
}

async fn run_shutdown_drain(
    server: Arc<EnhancedGameServer>,
    shutdown_tx: watch::Sender<bool>,
    drain_done_tx: watch::Sender<bool>,
) {
    shutdown_signal().await;
    signal_fish_server::server::run_drain_choreography(&server, shutdown_tx).await;
    // Signal choreography completion for the post-drain bounds. `let _ =`:
    // no receiver can be left (main holds one for the process lifetime), and
    // if the drop happens first the bounds still resolve via sender-drop.
    let _ = drain_done_tx.send(true);
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
    server: &EnhancedGameServer,
    shutdown_tx: watch::Sender<bool>,
    shutdown_task: tokio::task::JoinHandle<()>,
    cleanup_task: tokio::task::JoinHandle<()>,
) -> anyhow::Result<()> {
    // Decide on the server's drain state, not the process watch: the drain
    // task flips the watch only after the drain begins and the GoingAway
    // fan-out completes. If the serve future returns inside that window, a
    // watch-based guard aborts a committed drain and every connected client
    // is dropped with no close frame at all. Once `begin_shutdown_drain`
    // has CAS'd the deadline the task is past its signal wait and will run
    // the full choreography to completion, so it must be joined. With no
    // drain begun the task is parked in the signal wait and could never
    // complete, so aborting lets a spontaneous serve-loop failure exit.
    //
    // Residual window, accepted as unfixable without heavier coordination:
    // a signal delivered after this decision but before exit — with the
    // drain task not yet polled past its signal wait — is still lost. That
    // requires a spontaneous serve failure to coincide with signal delivery
    // inside one scheduling tick; the demonstrated failure this replaces
    // spanned the whole GoingAway fan-out.
    let drain_begun = server.is_draining();
    let _ = shutdown_tx.send(true);
    if drain_begun {
        // A panicked drain task is not a clean shutdown: the choreography
        // may have stopped mid-way (clients closed without their coded
        // 4000 frames, background work skipped), so the process must not
        // report success. Surface the JoinError instead of swallowing it.
        if let Err(error) = shutdown_task.await {
            return Err(anyhow::anyhow!("shutdown drain task failed: {error}"));
        }
    } else {
        shutdown_task.abort();
    }
    // Same truthfulness for the reaper: a panic here means the process ran
    // with cleanup silently dead (the library supervisor already refuses
    // this — see the serve supervisor's cleanup-task propagation).
    if let Err(error) = cleanup_task.await {
        return Err(anyhow::anyhow!("background cleanup task failed: {error}"));
    }
    Ok(())
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

#[cfg(test)]
mod cli_tests {
    use super::{wait_for_ctrl_c_shutdown, websocket, Cli};
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
            websocket::registered_connection_shutdown_settle_timeout(),
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

    /// The CLI summary writer must place the value and the newline on the wire
    /// byte-for-byte (the `--validate-config` summary is machine-scraped).
    #[test]
    fn cli_line_writer_writes_the_value_and_a_newline() {
        let mut buffer = Vec::new();
        super::write_line_to(&mut buffer, "Configuration validation passed")
            .expect("in-memory writer cannot fail");
        assert_eq!(buffer, b"Configuration validation passed\n");
    }

    /// Regression pin for the `println!`-panics-on-unwritable-stdout defect
    /// (exit 101 from `--print-config > /dev/full`): the CLI writer reports a
    /// broken sink as an `io::Error`, never a panic.
    #[test]
    fn cli_line_writer_reports_a_failing_writer_instead_of_panicking() {
        struct BrokenWriter;

        impl std::io::Write for BrokenWriter {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(io::Error::other("synthetic broken stdout"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let result = super::write_line_to(&mut BrokenWriter, "payload");
        assert!(
            result.is_err(),
            "a broken stdout must surface as an error the CLI maps to a clean exit, \
             not as a panic"
        );
    }

    #[cfg(feature = "legacy-fullmesh")]
    #[test]
    fn legacy_fullmesh_addr_derives_the_sibling_port_and_refuses_the_top_port() {
        use std::net::SocketAddr;
        assert_eq!(
            super::legacy_fullmesh_addr(8080),
            Some(SocketAddr::from(([0, 0, 0, 0], 8081))),
            "the legacy listener must bind one port above the main listener"
        );
        assert_eq!(
            super::legacy_fullmesh_addr(u16::MAX),
            None,
            "a 65535 main port has no sibling port: saturating derivation would collide \
             with the main listener, so it must be refused instead"
        );
    }
}

#[cfg(test)]
mod shutdown_drain_tests {
    use super::{finish_background_shutdown, post_drain_settle_bound, serve_with_post_drain_bound};
    use signal_fish_server::config::{
        CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig, SessionConfig,
        TransportSecurityConfig, TurnConfig,
    };
    use signal_fish_server::database::DatabaseConfig;
    use signal_fish_server::server::{run_drain_choreography, EnhancedGameServer, ServerConfig};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::sync::watch;

    async fn test_server_with_grace(grace: std::time::Duration) -> Arc<EnhancedGameServer> {
        let server_config = ServerConfig {
            drain_grace: grace,
            ..ServerConfig::default()
        };
        EnhancedGameServer::new(
            server_config,
            ProtocolConfig::default(),
            RelayTypeConfig::default(),
            SessionConfig::default(),
            TurnConfig::default(),
            DatabaseConfig::InMemory,
            MetricsConfig::default(),
            CoordinationConfig::default(),
            TransportSecurityConfig::default(),
            Vec::new(),
        )
        .await
        .expect("failed to construct test server")
    }

    /// Regression pin: once `begin_shutdown_drain` has committed the server to
    /// draining, `finish_background_shutdown` must join the drain task even if
    /// the process watch has not been flipped yet. The choreography only sends
    /// the watch after the GoingAway fan-out; a watch-based guard aborts the
    /// committed drain when the serve future returns inside that window, and
    /// every connected client is dropped with no close frame at all.
    #[tokio::test(start_paused = true)]
    async fn finish_background_shutdown_joins_a_begun_drain_before_the_watch_flips() {
        let server = test_server_with_grace(std::time::Duration::from_secs(10)).await;
        server.begin_shutdown_drain();

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let choreography_ran = Arc::new(AtomicBool::new(false));
        let drain_task = {
            let server = Arc::clone(&server);
            let choreography_ran = Arc::clone(&choreography_ran);
            let shutdown_tx = shutdown_tx.clone();
            tokio::spawn(async move {
                run_drain_choreography(&server, shutdown_tx).await;
                choreography_ran.store(true, Ordering::Release);
            })
        };
        let cleanup_task = tokio::spawn(async {});

        finish_background_shutdown(&server, shutdown_tx, drain_task, cleanup_task)
            .await
            .expect("a joined, healthy drain finish must succeed");

        assert!(
            choreography_ran.load(Ordering::Acquire),
            "a begun drain must be joined to completion, not aborted: aborting drops \
             every client's GoingAway advisory and coded 4000 close frame"
        );
        assert!(
            *shutdown_rx.borrow(),
            "the joined choreography must still flip the process watch for other watchers"
        );
    }

    /// The inverse contract: with no drain begun, the drain task is parked in
    /// the signal wait and could never complete, so it must be aborted to let
    /// a spontaneous serve-loop failure exit instead of hanging the process.
    /// A regression to joining unconditionally fails the timeout below, which
    /// is the only alternative to aborting in `finish_background_shutdown`.
    #[tokio::test(start_paused = true)]
    async fn finish_background_shutdown_aborts_the_parked_task_when_no_drain_begun() {
        let server = test_server_with_grace(std::time::Duration::from_secs(10)).await;
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);

        let drain_task = tokio::spawn(async {
            // Stands in for the real drain task parked in the signal wait.
            std::future::pending::<()>().await;
        });
        let cleanup_task = tokio::spawn(async {});

        let finished = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            finish_background_shutdown(&server, shutdown_tx, drain_task, cleanup_task),
        )
        .await;

        assert!(
            finished.is_ok(),
            "shutdown finish must abort a drain task that never began draining instead \
             of blocking process exit on the parked signal wait"
        );
        assert!(
            finished.expect("timeout guard").is_ok(),
            "a healthy abort-path finish must not report an error"
        );
    }

    /// A drain task that panics mid-choreography is not a clean shutdown:
    /// clients may have been dropped without their coded close frames and
    /// background work skipped, so the JoinError must surface as an error
    /// instead of being swallowed into an `Ok(())` exit.
    #[tokio::test(start_paused = true)]
    async fn finish_background_shutdown_surfaces_a_panicked_drain_task() {
        let server = test_server_with_grace(std::time::Duration::from_secs(10)).await;
        server.begin_shutdown_drain();

        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let drain_task = tokio::spawn(async {
            panic!("synthetic drain panic");
        });
        let cleanup_task = tokio::spawn(async {});

        let result =
            finish_background_shutdown(&server, shutdown_tx, drain_task, cleanup_task).await;

        assert!(
            result.is_err(),
            "a panicked drain task must not report a clean shutdown"
        );
    }

    /// The inverse seat: a cleanup-task panic means the process ran with its
    /// reaper dead, so the finish must surface the failure rather than exit 0
    /// (parity with the library supervisor's cleanup-task propagation).
    #[tokio::test(start_paused = true)]
    async fn finish_background_shutdown_surfaces_a_panicked_cleanup_task() {
        let server = test_server_with_grace(std::time::Duration::from_secs(10)).await;
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);

        let drain_task = tokio::spawn(async {
            // Stands in for the real drain task parked in the signal wait.
            std::future::pending::<()>().await;
        });
        let cleanup_task = tokio::spawn(async {
            panic!("synthetic cleanup panic");
        });

        let result =
            finish_background_shutdown(&server, shutdown_tx, drain_task, cleanup_task).await;

        assert!(
            result.is_err(),
            "a panicked cleanup task must not report a clean exit"
        );
    }

    /// Regression pin for the mid-drain forced exit: the choreography flips
    /// the PROCESS watch *before* its grace wait and coded closes
    /// (shutdown.rs sends at the top of the choreography), so the bound must
    /// ignore that flip entirely and anchor on choreography completion.
    #[tokio::test(start_paused = true)]
    async fn post_drain_settle_bound_ignores_the_process_watch_and_waits_for_choreography_completion(
    ) {
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let (drain_done_tx, drain_done_rx) = watch::channel(false);
        let settle = std::time::Duration::from_secs(5);

        let bound = tokio::spawn(post_drain_settle_bound(drain_done_rx, settle));

        shutdown_tx
            .send(true)
            .expect("process watch receiver is held");
        tokio::time::advance(std::time::Duration::from_secs(60)).await;
        assert!(
            !bound.is_finished(),
            "a process-watch flip (which happens mid-drain) must not fire the bound: \
             firing there force-exits healthy drains during the grace wait"
        );

        drain_done_tx
            .send(true)
            .expect("bound task still holds the receiver");
        tokio::time::advance(settle).await;
        bound
            .await
            .expect("the bound fires after choreography completion and the settle");
    }

    /// A panicked drain task drops its sender without signaling: the bound
    /// must still fire after one settle, so a stalled client cannot hang the
    /// process behind a choreography that will never complete.
    #[tokio::test(start_paused = true)]
    async fn post_drain_settle_bound_fires_when_the_drain_task_dies_without_signaling() {
        let (drain_done_tx, drain_done_rx) = watch::channel(false);
        let settle = std::time::Duration::from_secs(5);

        let bound = tokio::spawn(post_drain_settle_bound(drain_done_rx, settle));
        drop(drain_done_tx);

        tokio::time::advance(settle).await;
        bound
            .await
            .expect("sender drop releases the bound after the settle");
    }

    #[tokio::test(start_paused = true)]
    async fn bounded_serve_returns_the_result_when_the_serve_future_completes_first() {
        let (_drain_done_tx, drain_done_rx) = watch::channel(false);

        let result = serve_with_post_drain_bound(
            std::future::ready(Ok(())),
            drain_done_rx,
            std::time::Duration::from_secs(10),
        )
        .await;

        assert_eq!(
            result
                .expect("a completing serve future must yield its result")
                .expect("serve must succeed"),
            (),
            "a serve future that finishes inside the budget must pass through unchanged"
        );
    }

    /// The core regression pin for the unbounded-serve hang: a client parked
    /// mid-HTTP-request keeps hyper's connection task alive forever, so the
    /// serve future never returns even though the drain completed. The bound
    /// must convert that hang into a forced exit after one settle budget.
    #[tokio::test(start_paused = true)]
    async fn bounded_serve_forces_exit_when_the_listener_never_settles() {
        let (drain_done_tx, drain_done_rx) = watch::channel(false);
        let settle = std::time::Duration::from_secs(30);

        let bounded = tokio::spawn(serve_with_post_drain_bound(
            std::future::pending::<std::io::Result<()>>(),
            drain_done_rx,
            settle,
        ));

        drain_done_tx
            .send(true)
            .expect("bounded serve still holds the receiver");
        tokio::time::advance(settle).await;

        let result = bounded
            .await
            .expect("the bounded serve returns after the bound fires");
        assert!(
            result.is_none(),
            "a serve future that outlives the post-drain settle budget must be reported \
             as a forced exit, not awaited forever"
        );
    }

    /// With no live socket handler nothing can ever receive the coded `4000`
    /// close, so the idle drain must not wait out the grace window before
    /// exiting: that delay is pure restart time against the operator's
    /// termination budget.
    #[tokio::test]
    async fn idle_shutdown_drain_does_not_wait_out_the_grace() {
        let grace = std::time::Duration::from_millis(300);
        let server = test_server_with_grace(grace).await;
        assert!(!server.has_active_socket_tasks());

        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        let started = std::time::Instant::now();
        run_drain_choreography(&server, shutdown_tx).await;

        assert!(
            started.elapsed() < grace,
            "an idle drain must skip the grace wait: no handler remains to receive \
             the shutdown close frames"
        );
    }
}
