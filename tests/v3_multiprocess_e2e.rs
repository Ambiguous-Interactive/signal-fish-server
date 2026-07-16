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
//! A graceful-shutdown scenario sends SIGTERM to the real binary and requires
//! v3 `GoingAway`, close code `4000 server_shutdown`, and clean process exit.

mod v3_conformance_helpers;
mod websocket_test_helpers;

use std::time::Duration;
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use serde_json::json;
use signal_fish_server::protocol::{
    ClientMessage, ErrorCode, PlayerId, RoomJoinedPayload, ServerMessage, SessionPlanPayload,
    Topology, Transport,
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use v3_conformance_helpers::{
    assert_full_mesh_glare_matrix, await_ready_count, expect_finalize_plan, ordered_pairs, ready,
    relay_one_signal, send, start_game, SERVER_MESSAGE_TIMEOUT,
};
use websocket_test_helpers::server_process::{
    spawn_server, spawn_server_on_fixed_port, CONNECT_TIMEOUT,
};
use websocket_test_helpers::{deadline_after, next_matching_server_message_within, WsStream};

/// Arbitrary app id: the spawned server runs with WebSocket auth disabled.
const APP_ID: &str = "multiprocess-conformance-app";
/// How long a client socket may take to observe the death of the server.
///
/// A saturation-tolerant CEILING, not an expected wait (zero-flakiness policy,
/// .agents/skills/testing-rust/references/project-testing.md): the happy path returns the instant the close is
/// observed, so the large ceiling never slows a passing run and only bites
/// under pathological load. (The server-spawn / connect / health ceilings live
/// with the harness in `websocket_test_helpers::server_process`.)
const SOCKET_CLOSE_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(unix)]
fn now_unix_ms() -> u64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// This suite's config overlay, deep-merged over the shared harness's base
/// config (`websocket_test_helpers::server_process`): in-memory + webrtc, with
/// the per-scenario default topology.
fn config_overlay(default_topology: &str) -> serde_json::Value {
    json!({
        "session": {
            "default_topology": default_topology,
            "enable_webrtc": true
        }
    })
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

#[cfg(unix)]
async fn expect_close_frame_within(
    ws: &mut WsStream,
    timeout: Duration,
    context: &str,
) -> (u16, String) {
    let deadline = deadline_after(timeout);
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Ok(Some(Ok(Message::Close(Some(frame))))) => {
                return (frame.code.into(), frame.reason.to_string());
            }
            Ok(Some(Ok(Message::Close(None)))) => {
                panic!("{context}: close frame had no code")
            }
            Ok(Some(Ok(frame @ (Message::Text(_) | Message::Binary(_))))) => {
                panic!(
                    "{context}: no ServerMessage may be pending while awaiting \
                     the close frame, got {frame:?}"
                )
            }
            Ok(Some(Ok(_control_frame))) => {}
            Ok(Some(Err(error))) => {
                panic!("{context}: websocket error before close frame: {error}")
            }
            Ok(None) => panic!("{context}: stream ended before close frame"),
            Err(_elapsed) => {
                panic!("{context}: socket did not observe a close frame within {timeout:?}")
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn multiprocess_mesh_n3_full_session_over_real_tcp() {
    let server = spawn_server(config_overlay("mesh")).await;
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
    for socket in sockets.iter_mut() {
        await_ready_count(socket, 3).await;
    }

    // Readiness no longer auto-starts: the creator sends an explicit StartGame
    // (any member may start; the room is supports_authority: false).
    start_game(&mut sockets[0]).await;

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

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn multiprocess_sigterm_gracefully_drains_with_goingaway_and_4000() {
    let mut server = spawn_server(json!({
        "server": {
            "drain_grace_secs": 1
        },
        "session": {
            "default_topology": "mesh",
            "enable_webrtc": true
        }
    }))
    .await;
    let game = "mp-drain";

    let mut peer = connect_client(server.port).await;
    authenticate_v3(&mut peer).await;
    let joined = join_room(&mut peer, game, None, "DrainPeer", 2).await;
    assert!(
        joined.reconnection_token.is_some(),
        "v3 join should expose the pre-issued reconnect token before shutdown"
    );

    let before_deadline_ms = now_unix_ms();
    server.send_sigterm();

    let (deadline_ms, retry_after_secs) = next_matching_server_message_within(
        &mut peer,
        SOCKET_CLOSE_TIMEOUT,
        "shutdown GoingAway",
        |message| match message {
            ServerMessage::GoingAway {
                deadline_ms,
                retry_after_secs,
            } => Some((deadline_ms, retry_after_secs)),
            _ => None,
        },
    )
    .await;
    assert!(
        deadline_ms >= before_deadline_ms,
        "GoingAway deadline must be an absolute future-ish unix ms timestamp"
    );
    assert_eq!(retry_after_secs, Some(1));

    let (code, reason) =
        expect_close_frame_within(&mut peer, SOCKET_CLOSE_TIMEOUT, "shutdown close").await;
    assert_eq!(code, 4000, "SIGTERM drain must close with 4000 ({reason})");
    assert_eq!(reason, "server_shutdown");

    let status = tokio::time::timeout(SOCKET_CLOSE_TIMEOUT, server.wait_for_exit())
        .await
        .expect("server process did not exit after graceful drain");
    assert!(
        status.success(),
        "server should exit cleanly after SIGTERM drain, got {status}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn multiprocess_server_restart_invalidates_reconnect_tokens() {
    let mut server = spawn_server(config_overlay("mesh")).await;
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
    await_ready_count(&mut peer_a, 2).await;
    await_ready_count(&mut peer_b, 2).await;

    // Readiness no longer auto-starts: an explicit StartGame finalizes.
    start_game(&mut peer_a).await;

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
    let restarted = spawn_server_on_fixed_port(port, config_overlay("mesh")).await;

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
