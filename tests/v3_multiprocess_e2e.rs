//! True multi-process protocol v3 conformance tests: each test spawns the
//! compiled `signal-fish-server` binary (`env!("CARGO_BIN_EXE_signal-fish-server")`)
//! as a REAL child OS process and drives it over real TCP from this test
//! process — a genuine process boundary, unlike the in-process suites.
//!
//! # Infrastructure
//!
//! - **Configuration**: the binary has no config-file CLI flag (`-c` is
//!   `--validate-config`); it discovers configuration through the environment
//!   (`src/config/loader.rs`). Each spawn writes a per-test temp config file
//!   (tempfile) and points the child at it via `SIGNAL_FISH_CONFIG_PATH`, with
//!   the child's working directory set to the same temp dir so no stray
//!   `config.json` in the repo can interfere. Because later config sources
//!   merge over earlier ones and `SIGNAL_FISH__*` env overrides always win,
//!   the port is ALSO pinned via `SIGNAL_FISH__PORT` (belt and braces), and
//!   every inherited `SIGNAL_FISH*` variable is scrubbed from the child env.
//!   The config keeps the binary zero-dependency: in-memory storage (the only
//!   backend), `require_websocket_auth: false`, SDK enforcement off, and the
//!   per-scenario `session.default_topology`.
//! - **Ports**: a `std::net::TcpListener` bound to `0.0.0.0:0` (matching the
//!   server's bind address) reserves a free port which is then released and
//!   passed to the child. The reserve-release-spawn race is absorbed by up to
//!   3 spawn attempts with fresh ports (`spawn_server`); the restart scenario
//!   retries on its FIXED port instead (`spawn_server_on_fixed_port`).
//! - **Readiness**: the spawned process is polled on `/v2/health` (reqwest)
//!   until it answers 200, with a hard deadline; early child exit is detected
//!   via `try_wait`. Failures report the child's captured stdout/stderr
//!   (piped to files in the temp dir).
//! - **Cleanup**: `ServerProcess` is a child guard — `Drop` issues
//!   `start_kill()` and the spawn sets `kill_on_drop(true)`, so the server
//!   process dies even when a test panics; no orphans survive CI. Tests that
//!   need a deterministic kill call `kill_and_wait()` (SIGKILL + reap).
//!
//! # Scenarios
//!
//! 1. `multiprocess_mesh_n3_full_session_over_real_tcp` — the headline
//!    conformance test: 3 WebSocket clients against the real binary complete
//!    the full lobby -> `GameStarting` -> `SessionPlan` flow with the global
//!    glare matrix pinned, then relay distinct opaque signals across all 6
//!    ordered pairs byte-identically.
//! 2. `multiprocess_server_restart_invalidates_reconnect_tokens` — SIGKILL the
//!    server mid-session: both clients observe the close, a NEW process on the
//!    SAME port rejects the old reconnect identity (`ReconnectionFailed`, the
//!    in-memory registry died with the old process), and a fresh session works.
//!
//! A graceful-shutdown scenario is deliberately ABSENT: `src/main.rs` installs
//! no signal handler (`axum::serve` runs without `with_graceful_shutdown`, and
//! the crate contains no `ctrl_c`/SIGTERM handling), so there is no documented
//! graceful-exit behavior to pin.

mod v3_conformance_helpers;
mod websocket_test_helpers;

use std::path::PathBuf;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::json;
use signal_fish_server::protocol::{
    ClientMessage, ErrorCode, PlayerId, RoomJoinedPayload, ServerMessage, SessionPlanPayload,
    Topology, Transport,
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use v3_conformance_helpers::{
    assert_full_mesh_glare_matrix, await_ready_count, expect_finalize_plan, ordered_pairs, ready,
    relay_one_signal, send, SERVER_MESSAGE_TIMEOUT,
};
use websocket_test_helpers::{deadline_after, next_matching_server_message_within, WsStream};

/// Arbitrary app id: the spawned server runs with WebSocket auth disabled.
const APP_ID: &str = "multiprocess-conformance-app";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a client socket may take to observe the death of the server.
const SOCKET_CLOSE_TIMEOUT: Duration = Duration::from_secs(10);
const HEALTH_DEADLINE: Duration = Duration::from_secs(15);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(100);
const SPAWN_ATTEMPTS: usize = 3;

/// Guard around the spawned server binary. Dropping it kills the child
/// (`start_kill` here plus `kill_on_drop(true)` at spawn), so a panicking test
/// never leaks an orphan server process into CI.
struct ServerProcess {
    child: Option<tokio::process::Child>,
    port: u16,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    /// Owns the temp config file, the captured output files, and the child's
    /// working directory; removed from disk when the guard drops.
    _workdir: tempfile::TempDir,
}

impl ServerProcess {
    /// SIGKILL the child (TerminateProcess on Windows) and reap it.
    async fn kill_and_wait(&mut self) {
        let mut child = self
            .child
            .take()
            .expect("server process was already killed");
        child.kill().await.expect("kill the server process");
    }

    /// The child's captured stdout/stderr, for spawn-failure diagnostics.
    fn captured_output(&self) -> String {
        let read = |label: &str, path: &PathBuf| {
            let content = std::fs::read_to_string(path)
                .unwrap_or_else(|error| format!("<failed to read {label}: {error}>"));
            format!("--- {label} ---\n{content}")
        };
        format!(
            "{}\n{}",
            read("stdout", &self.stdout_path),
            read("stderr", &self.stderr_path)
        )
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if let Err(error) = child.start_kill() {
                eprintln!("failed to kill server process on drop: {error}");
            }
        }
    }
}

/// Reserve a currently-free port by binding `0.0.0.0:0` (the same address the
/// server binds), reading the assignment, and releasing the listener.
fn reserve_port() -> u16 {
    let probe = std::net::TcpListener::bind(("0.0.0.0", 0)).expect("bind port probe");
    let port = probe.local_addr().expect("port probe local_addr").port();
    drop(probe);
    port
}

/// Spawn the server binary on a fresh free port, retrying with a NEW port on
/// failure (absorbs the reserve-release-spawn race).
async fn spawn_server(default_topology: &str) -> ServerProcess {
    let mut failures = Vec::new();
    for attempt in 1..=SPAWN_ATTEMPTS {
        let port = reserve_port();
        match try_spawn_server(port, default_topology).await {
            Ok(server) => return server,
            Err(failure) => failures.push(format!("attempt {attempt} (port {port}): {failure}")),
        }
    }
    panic!(
        "server binary failed to become healthy after {SPAWN_ATTEMPTS} attempts:\n{}",
        failures.join("\n")
    );
}

/// Spawn the server binary on a FIXED port (restart-on-same-port semantics),
/// retrying the same port to absorb transient socket-teardown races.
async fn spawn_server_on_fixed_port(port: u16, default_topology: &str) -> ServerProcess {
    let mut failures = Vec::new();
    for attempt in 1..=SPAWN_ATTEMPTS {
        match try_spawn_server(port, default_topology).await {
            Ok(server) => return server,
            Err(failure) => failures.push(format!("attempt {attempt}: {failure}")),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!(
        "server binary failed to restart on port {port} after {SPAWN_ATTEMPTS} attempts:\n{}",
        failures.join("\n")
    );
}

/// One spawn attempt: write the temp config, launch the child with a scrubbed
/// environment, and wait for `/v2/health`. On failure the guard (and its
/// `kill_on_drop` child) is dropped and the captured output is returned.
async fn try_spawn_server(port: u16, default_topology: &str) -> Result<ServerProcess, String> {
    let workdir = tempfile::tempdir().expect("create temp workdir");
    let config_path = workdir.path().join("server-config.json");
    let config = json!({
        "port": port,
        "server": {
            "enable_reconnection": true,
            "reconnection_window": 300
        },
        "security": {
            "require_websocket_auth": false,
            "require_metrics_auth": false,
            "cors_origins": "*"
        },
        "protocol": {
            "sdk_compatibility": { "enforce": false }
        },
        "session": {
            "default_topology": default_topology,
            "enable_webrtc": true
        },
        "logging": {
            "enable_file_logging": false
        }
    });
    std::fs::write(
        &config_path,
        serde_json::to_vec_pretty(&config).expect("serialize server config"),
    )
    .expect("write server config");

    let stdout_path = workdir.path().join("server-stdout.log");
    let stderr_path = workdir.path().join("server-stderr.log");
    let stdout_file = std::fs::File::create(&stdout_path).expect("create stdout capture");
    let stderr_file = std::fs::File::create(&stderr_path).expect("create stderr capture");

    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_signal-fish-server"));
    command
        .current_dir(workdir.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(stdout_file))
        .stderr(std::process::Stdio::from(stderr_file))
        .kill_on_drop(true);
    // Scrub every inherited SIGNAL_FISH* variable: ambient config JSON, config
    // paths, or SIGNAL_FISH__* field overrides would silently override the
    // temp config (env overrides are applied last by the loader).
    for (key, _) in std::env::vars_os() {
        if key
            .to_str()
            .is_some_and(|key| key.starts_with("SIGNAL_FISH"))
        {
            command.env_remove(&key);
        }
    }
    command.env("SIGNAL_FISH_CONFIG_PATH", &config_path);
    command.env("SIGNAL_FISH__PORT", port.to_string());

    let child = command.spawn().expect("spawn the server binary");
    let mut server = ServerProcess {
        child: Some(child),
        port,
        stdout_path,
        stderr_path,
        _workdir: workdir,
    };

    match wait_until_healthy(&mut server).await {
        Ok(()) => Ok(server),
        Err(reason) => Err(format!(
            "{reason}; captured child output:\n{}",
            server.captured_output()
        )),
    }
}

/// Poll `/v2/health` until it answers 200 (or the deadline passes / the child
/// exits early).
async fn wait_until_healthy(server: &mut ServerProcess) -> Result<(), String> {
    let url = format!("http://127.0.0.1:{}/v2/health", server.port);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("build health-poll client");
    let deadline = std::time::Instant::now() + HEALTH_DEADLINE;

    loop {
        let child = server
            .child
            .as_mut()
            .expect("health poll requires a live child");
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(format!("server process exited early with {status}"));
            }
            Ok(None) => {}
            Err(error) => return Err(format!("failed to poll server process: {error}")),
        }

        match client.get(&url).send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            // Not up yet (connection refused / non-200): keep polling.
            Ok(_) | Err(_) => {}
        }

        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "health endpoint {url} not ready within {HEALTH_DEADLINE:?}"
            ));
        }
        tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
    }
}

async fn connect_client(port: u16) -> WsStream {
    let url = format!("ws://127.0.0.1:{port}/v3/ws");
    let (ws, _) = tokio::time::timeout(CONNECT_TIMEOUT, connect_async(&url))
        .await
        .expect("websocket connect timeout")
        .expect("websocket connect");
    ws
}

/// Authenticate as a v3 client supporting webrtc + every topology and assert
/// the binary negotiates protocol v3 (its default `[protocol]` allows 2..=3).
async fn authenticate_v3(ws: &mut WsStream) {
    send(
        ws,
        &ClientMessage::Authenticate {
            app_id: APP_ID.to_string(),
            sdk_version: None,
            platform: None,
            game_data_format: None,
            protocol_version: Some(3),
            supported_transports: Some(vec![Transport::Relay, Transport::WebRtc]),
            supported_topologies: Some(vec![Topology::Relay, Topology::Host, Topology::Mesh]),
        },
    )
    .await;

    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "Authenticated", |message| {
        matches!(message, ServerMessage::Authenticated { .. }).then_some(())
    })
    .await;
    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "ProtocolInfo", |message| {
        match message {
            ServerMessage::ProtocolInfo(info) => {
                assert_eq!(info.protocol_version, Some(3), "v3 must be negotiated");
                Some(())
            }
            _ => None,
        }
    })
    .await;
}

/// Join (or create) a room for `game_name`, returning the full payload.
async fn join_room(
    ws: &mut WsStream,
    game_name: &str,
    room_code: Option<String>,
    player_name: &str,
    max_players: u8,
) -> Box<RoomJoinedPayload> {
    send(
        ws,
        &ClientMessage::JoinRoom {
            game_name: game_name.to_string(),
            room_code,
            player_name: player_name.to_string(),
            max_players: Some(max_players),
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await;

    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "room join response",
        |message| match message {
            ServerMessage::RoomJoined(payload) => Some(payload),
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                panic!("room join failed: {reason} ({error_code:?})")
            }
            _ => None,
        },
    )
    .await
}

/// Read the socket until it observes the server's death (transport error or
/// clean end-of-stream), tolerating buffered control frames (Close / Ping /
/// Pong); panics if the socket still looks alive at the deadline, or if a
/// buffered protocol frame (`Text` / `Binary`) is still pending — no
/// `ServerMessage` may be outstanding at this phase boundary, so silently
/// draining one would mask a sequencing bug.
async fn expect_socket_closed_within(ws: &mut WsStream, timeout: Duration, context: &str) {
    let deadline = deadline_after(timeout);
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            // End of stream: the close was observed.
            Ok(None) => return,
            // Transport error (e.g. reset without close handshake): observed.
            Ok(Some(Err(_error))) => return,
            // A buffered protocol frame means a ServerMessage was pending —
            // the test sequenced its reads incorrectly.
            Ok(Some(Ok(frame @ (Message::Text(_) | Message::Binary(_))))) => {
                panic!(
                    "{context}: no ServerMessage may be pending while awaiting \
                     the socket close, got {frame:?}"
                )
            }
            // Control frame (possibly the Close itself) — keep draining.
            Ok(Some(Ok(_control_frame))) => {}
            Err(_elapsed) => {
                panic!("{context}: socket did not observe the server death within {timeout:?}")
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn multiprocess_mesh_n3_full_session_over_real_tcp() {
    let server = spawn_server("mesh").await;
    let game = "mp-mesh3";

    // Three v3+webrtc clients connect to the real binary over real TCP.
    let mut creator = connect_client(server.port).await;
    authenticate_v3(&mut creator).await;
    let joined = join_room(&mut creator, game, None, "ProcPeer1", 3).await;
    let room_code = joined.room_code;

    let mut ids = vec![joined.player_id];
    let mut sockets = vec![creator];
    for name in ["ProcPeer2", "ProcPeer3"] {
        let mut socket = connect_client(server.port).await;
        authenticate_v3(&mut socket).await;
        ids.push(
            join_room(&mut socket, game, Some(room_code.clone()), name, 3)
                .await
                .player_id,
        );
        sockets.push(socket);
    }

    // Paced readies: every ready is observed by everyone before the next.
    ready(&mut sockets[0]).await;
    for socket in sockets.iter_mut() {
        await_ready_count(socket, 1).await;
    }
    ready(&mut sockets[1]).await;
    for socket in sockets.iter_mut() {
        await_ready_count(socket, 2).await;
    }
    ready(&mut sockets[2]).await;

    // Full lobby -> GameStarting -> SessionPlan x3 with the glare matrix.
    let mut plans = Vec::new();
    for (index, socket) in sockets.iter_mut().enumerate() {
        let plan = expect_finalize_plan(socket, &format!("member {}", ids[index])).await;
        assert_eq!(plan.topology, Topology::Mesh);
        assert_eq!(plan.transport, Transport::WebRtc);
        assert_eq!(plan.fallback, Transport::Relay);
        assert!(plan.host.is_none(), "mesh plans elect no host");
        assert_eq!(plan.peers.len(), 2, "3-peer mesh lists exactly 2 peers");
        // The binary's default [turn] block advertises its public STUN even
        // with TURN disabled, so the plan carries credential-less ICE.
        assert!(!plan.ice_servers.is_empty(), "webrtc plans carry ICE");
        assert!(
            plan.ice_servers
                .iter()
                .all(|server| server.username.is_none() && server.credential.is_none()),
            "no TURN credentials are minted when [turn] is disabled"
        );
        plans.push(plan);
    }
    let plan_refs: Vec<(PlayerId, &SessionPlanPayload)> = ids
        .iter()
        .copied()
        .zip(plans.iter().map(|plan| &**plan))
        .collect();
    assert_full_mesh_glare_matrix(&plan_refs);

    // Pairwise signal relay across all 6 ordered pairs, byte-identical.
    for (from_idx, to_idx) in ordered_pairs(3) {
        relay_one_signal(&mut sockets, &ids, from_idx, to_idx).await;
    }

    drop(server);
}

#[tokio::test(flavor = "multi_thread")]
async fn multiprocess_server_restart_invalidates_reconnect_tokens() {
    let mut server = spawn_server("mesh").await;
    let port = server.port;
    let game = "mp-restart";

    // Two v3 clients fill a 2-seat mesh room and finalize it.
    let mut peer_a = connect_client(port).await;
    authenticate_v3(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, game, None, "RestartA", 2).await;
    let room_code = joined_a.room_code;

    let mut peer_b = connect_client(port).await;
    authenticate_v3(&mut peer_b).await;
    let joined_b = join_room(&mut peer_b, game, Some(room_code), "RestartB", 2).await;
    let (peer_b_id, room_id) = (joined_b.player_id, joined_b.room_id);

    ready(&mut peer_a).await;
    await_ready_count(&mut peer_a, 1).await;
    await_ready_count(&mut peer_b, 1).await;
    ready(&mut peer_b).await;
    for (ws, who) in [(&mut peer_a, "peer_a"), (&mut peer_b, "peer_b")] {
        let plan = expect_finalize_plan(ws, who).await;
        assert_eq!(plan.topology, Topology::Mesh);
        assert_eq!(plan.transport, Transport::WebRtc);
    }

    // Reconnect-token wire surface (verified in src/server/reconnection_service.rs):
    // the server mints the token ONLY when a disconnection is registered and
    // never sends it to the client, so no pure-wire client can hold a valid
    // token — and the killed process minted none anyway. What this scenario
    // pins is the documented restart behavior: the reconnection registry is
    // in-memory, so a NEW process must reject the old identity outright
    // regardless of the presented token.
    server.kill_and_wait().await;

    // Both client sockets must observe the death as a close/error.
    expect_socket_closed_within(
        &mut peer_a,
        SOCKET_CLOSE_TIMEOUT,
        "peer_a after server SIGKILL",
    )
    .await;
    expect_socket_closed_within(
        &mut peer_b,
        SOCKET_CLOSE_TIMEOUT,
        "peer_b after server SIGKILL",
    )
    .await;
    drop(server);

    // Restart a NEW server process on the SAME port.
    let restarted = spawn_server_on_fixed_port(port, "mesh").await;

    // The previous reconnect flow must be rejected: the registry died with the
    // old process, so the identity is unknown (well-formed token or not).
    let mut reconnector = connect_client(port).await;
    authenticate_v3(&mut reconnector).await;
    send(
        &mut reconnector,
        &ClientMessage::Reconnect {
            player_id: peer_b_id,
            room_id,
            auth_token: "00000000-0000-4000-8000-000000000000".to_string(),
        },
    )
    .await;
    let (reason, error_code) = next_matching_server_message_within(
        &mut reconnector,
        SERVER_MESSAGE_TIMEOUT,
        "post-restart reconnect rejection",
        |message| match message {
            ServerMessage::ReconnectionFailed { reason, error_code } => Some((reason, error_code)),
            ServerMessage::Reconnected(payload) => panic!(
                "a restarted server must not honor a pre-restart reconnect identity, \
                 got Reconnected for {}",
                payload.player_id
            ),
            other => panic!("expected ReconnectionFailed, got {other:?}"),
        },
    )
    .await;
    assert_eq!(error_code, ErrorCode::ReconnectionFailed);
    assert_eq!(
        reason, "No disconnection record found",
        "the in-memory reconnection registry must be empty after a restart"
    );

    // A completely FRESH session (new auth + join) works against the new
    // process.
    let fresh_game = "mp-restart-fresh";
    let mut fresh_a = connect_client(port).await;
    authenticate_v3(&mut fresh_a).await;
    let fresh_joined = join_room(&mut fresh_a, fresh_game, None, "FreshA", 2).await;

    let mut fresh_b = connect_client(port).await;
    authenticate_v3(&mut fresh_b).await;
    let fresh_b_joined = join_room(
        &mut fresh_b,
        fresh_game,
        Some(fresh_joined.room_code.clone()),
        "FreshB",
        2,
    )
    .await;
    assert_eq!(
        fresh_b_joined.room_id, fresh_joined.room_id,
        "both fresh clients share the new room on the restarted process"
    );

    drop(restarted);
}
