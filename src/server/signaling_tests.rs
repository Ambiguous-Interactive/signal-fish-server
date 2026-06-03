//! Handler tests for the P2 targeted signal relay (`signaling.rs`).
//!
//! Mirrors the `message_router_tests` harness: register clients, set their
//! negotiated protocol, drive `handle_signal` / `handle_webrtc_late_join`, and
//! assert on what each client receives. Covers the happy path, every rejection
//! branch, glare determinism, late-join offerer designation, and v2 gating
//! (Appendix K).

use crate::config::{
    AuthMaintenanceConfig, CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig,
    SessionConfig, TransportSecurityConfig,
};
use crate::database::DatabaseConfig;
use crate::protocol::{
    ErrorCode, IceServer, LobbyState, PlayerId, PlayerInfo, Room, ServerMessage, Topology,
    Transport,
};
use crate::rate_limit::RateLimitConfig;
use crate::server::{EnhancedGameServer, NegotiatedProtocol, ServerConfig};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

use super::signaling::local_initiates;

/// Allocate a unique loopback address per registered client so tests never
/// collide on the same `SocketAddr`.
static PORT: AtomicU16 = AtomicU16::new(52000);

fn next_addr() -> SocketAddr {
    let port = PORT.fetch_add(1, Ordering::Relaxed);
    format!("127.0.0.1:{port}").parse().expect("valid addr")
}

async fn create_test_server() -> Arc<EnhancedGameServer> {
    create_test_server_with_signals(600).await
}

async fn create_test_server_with_signals(max_signals: u32) -> Arc<EnhancedGameServer> {
    create_test_server_with_signal_limits(max_signals, 60).await
}

async fn create_test_server_with_signal_limits(
    max_signals: u32,
    max_signal_errors: u32,
) -> Arc<EnhancedGameServer> {
    let config = ServerConfig {
        rate_limit_config: RateLimitConfig {
            max_signals,
            max_signal_errors,
            ..RateLimitConfig::default()
        },
        ..ServerConfig::default()
    };
    EnhancedGameServer::new(
        config,
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        SessionConfig::default(),
        DatabaseConfig::InMemory,
        MetricsConfig::default(),
        AuthMaintenanceConfig::default(),
        CoordinationConfig::default(),
        TransportSecurityConfig::default(),
        Vec::new(),
    )
    .await
    .expect("failed to construct test server")
}

/// Build a server whose session policy is the given `SessionConfig`, so the
/// finalization-gated late-join path can resolve to a non-relay topology.
async fn create_test_server_with_session(session: SessionConfig) -> Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
        ServerConfig::default(),
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        session,
        DatabaseConfig::InMemory,
        MetricsConfig::default(),
        AuthMaintenanceConfig::default(),
        CoordinationConfig::default(),
        TransportSecurityConfig::default(),
        Vec::new(),
    )
    .await
    .expect("failed to construct test server")
}

/// A STUN-only `SessionConfig` preferring `mesh` (so an all-v3+webrtc room
/// resolves to `mesh + webrtc`).
fn mesh_session_config() -> SessionConfig {
    SessionConfig {
        default_topology: Topology::Mesh,
        ice_servers: vec![IceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_string()],
            username: None,
            credential: None,
        }],
        ..SessionConfig::default()
    }
}

/// A STUN-only `SessionConfig` preferring `host` (so an all-v3+webrtc room
/// resolves to `host + webrtc`).
fn host_session_config() -> SessionConfig {
    SessionConfig {
        default_topology: Topology::Host,
        ice_servers: vec![IceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_string()],
            username: None,
            credential: None,
        }],
        ..SessionConfig::default()
    }
}

/// A `host`-preferring `SessionConfig` with WebRTC disabled, so an all-v3 room
/// whose members support host+direct resolves to `host + direct` (LAN) — a
/// non-relay *topology* whose *transport* is not WebRTC.
fn host_direct_session_config() -> SessionConfig {
    SessionConfig {
        default_topology: Topology::Host,
        enable_webrtc: false,
        enable_direct: true,
        ice_servers: Vec::new(),
        ..SessionConfig::default()
    }
}

/// Fetch a room and mark it `Finalized` so late-join pairing engages (it only
/// fires for an active, finalized session). Storage state is irrelevant to
/// `handle_webrtc_late_join`, which reads `lobby_state` from the passed `Room`.
async fn finalized_room(server: &EnhancedGameServer, room_id: &uuid::Uuid) -> Room {
    let mut room = server
        .database
        .get_room_by_id(room_id)
        .await
        .expect("room lookup")
        .expect("room exists");
    room.lobby_state = LobbyState::Finalized;
    room
}

/// Fetch a room without changing its (default `Waiting`) lobby state.
async fn waiting_room(server: &EnhancedGameServer, room_id: &uuid::Uuid) -> Room {
    server
        .database
        .get_room_by_id(room_id)
        .await
        .expect("room lookup")
        .expect("room exists")
}

/// Drive a full (player count == max) room through lobby → all-ready → finalize
/// in the database, so the reconnect path's fresh room read observes
/// `LobbyState::Finalized` and re-pairing engages. `players` must list every
/// member id currently in the room.
async fn finalize_db_room(server: &EnhancedGameServer, room_id: &uuid::Uuid, players: &[PlayerId]) {
    server
        .database
        .transition_room_to_lobby(room_id)
        .await
        .expect("transition to lobby");
    for player in players {
        server
            .database
            .toggle_player_ready(room_id, player)
            .await
            .expect("toggle ready");
    }
    server
        .database
        .finalize_room_game(room_id)
        .await
        .expect("finalize room");
    let room = server
        .database
        .get_room_by_id(room_id)
        .await
        .expect("room lookup")
        .expect("room exists");
    assert_eq!(
        room.lobby_state,
        LobbyState::Finalized,
        "room must reach Finalized for the reconnect re-pairing test"
    );
}

/// Register a client and return its id plus the receiving half of its channel.
async fn register_client(
    server: &EnhancedGameServer,
) -> (PlayerId, mpsc::Receiver<Arc<ServerMessage>>) {
    let (sender, receiver) = mpsc::channel(16);
    let player_id = server
        .connection_manager
        .register_client(sender, next_addr(), server.instance_id)
        .await
        .expect("client registration succeeds");
    (player_id, receiver)
}

fn v3_webrtc() -> NegotiatedProtocol {
    NegotiatedProtocol {
        version: 3,
        transports: vec![Transport::Relay, Transport::WebRtc],
        topologies: vec![Topology::Relay, Topology::Mesh],
    }
}

/// A v3 + WebRTC client that also advertises the `host` topology, so a
/// host-preferring room resolves to `host + webrtc` rather than downgrading.
fn v3_webrtc_host() -> NegotiatedProtocol {
    NegotiatedProtocol {
        version: 3,
        transports: vec![Transport::Relay, Transport::WebRtc],
        topologies: vec![Topology::Relay, Topology::Host, Topology::Mesh],
    }
}

/// A v3 client advertising WebRTC *and* Direct transports plus the `host`
/// topology. It clears the late-join WebRTC gate (it supports WebRTC), so a room
/// that nonetheless resolves to `host + direct` (the deployment disabled WebRTC)
/// isolates the plan's *transport* gate rather than the per-peer capability gate.
fn v3_webrtc_direct_host() -> NegotiatedProtocol {
    NegotiatedProtocol {
        version: 3,
        transports: vec![Transport::Relay, Transport::WebRtc, Transport::Direct],
        topologies: vec![Topology::Relay, Topology::Host],
    }
}

fn v3_relay_only() -> NegotiatedProtocol {
    NegotiatedProtocol {
        version: 3,
        transports: vec![Transport::Relay],
        topologies: vec![Topology::Relay],
    }
}

fn v2_with_webrtc_transport() -> NegotiatedProtocol {
    NegotiatedProtocol {
        version: 2,
        transports: vec![Transport::Relay, Transport::WebRtc],
        topologies: vec![Topology::Relay],
    }
}

/// Receive the next message or fail if none arrives promptly.
async fn recv(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>) -> Arc<ServerMessage> {
    timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("channel still open")
        .expect("message present")
}

/// Assert that no message is pending within a short window.
async fn assert_silent(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>) {
    assert!(
        timeout(Duration::from_millis(100), receiver.recv())
            .await
            .unwrap_or(None)
            .is_none(),
        "expected no message to be delivered"
    );
}

fn error_code(message: &ServerMessage) -> Option<ErrorCode> {
    match message {
        ServerMessage::Error { error_code, .. } => error_code.clone(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Glare determinism (Appendix E).
// ---------------------------------------------------------------------------

#[test]
fn local_initiates_is_antisymmetric_and_irreflexive() {
    for _ in 0..256 {
        let a = PlayerId::new_v4();
        let b = PlayerId::new_v4();
        if a == b {
            continue;
        }
        // Exactly one side initiates.
        assert_ne!(local_initiates(a, b), local_initiates(b, a));
        // Lesser id initiates.
        if a < b {
            assert!(local_initiates(a, b));
            assert!(!local_initiates(b, a));
        } else {
            assert!(local_initiates(b, a));
            assert!(!local_initiates(a, b));
        }
        // Self never initiates.
        assert!(!local_initiates(a, a));
        assert!(!local_initiates(b, b));
    }
}

// ---------------------------------------------------------------------------
// handle_signal happy path + ordering.
// ---------------------------------------------------------------------------

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_delivered_to_same_room_peer_preserving_payload() {
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&bob, room_id)
        .await;

    // Drive a full offer -> answer -> ICE sequence and assert in-order delivery
    // with byte-identical payloads.
    let offer = json!({ "Offer": "v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\n" });
    let answer = json!({ "Answer": "v=0\r\no=- 3 4 IN IP4 0.0.0.0\r\n" });
    let ice = json!({ "IceCandidate": "candidate:1 1 UDP 2130706431 1.2.3.4 5000 typ host" });

    server.handle_signal(&alice, bob, offer.clone()).await;
    server.handle_signal(&bob, alice, answer.clone()).await;
    server.handle_signal(&alice, bob, ice.clone()).await;

    match recv(&mut bob_rx).await.as_ref() {
        ServerMessage::Signal { from, signal } => {
            assert_eq!(*from, alice);
            assert_eq!(*signal, offer);
        }
        other => panic!("expected Signal(offer), got {other:?}"),
    }
    match recv(&mut alice_rx).await.as_ref() {
        ServerMessage::Signal { from, signal } => {
            assert_eq!(*from, bob);
            assert_eq!(*signal, answer);
        }
        other => panic!("expected Signal(answer), got {other:?}"),
    }
    match recv(&mut bob_rx).await.as_ref() {
        ServerMessage::Signal { from, signal } => {
            assert_eq!(*from, alice);
            assert_eq!(*signal, ice);
        }
        other => panic!("expected Signal(ice), got {other:?}"),
    }

    // The sender never receives its own signal echoed back.
    assert_silent(&mut alice_rx).await;
    assert_silent(&mut bob_rx).await;
}

// ---------------------------------------------------------------------------
// Rejection branches.
// ---------------------------------------------------------------------------

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_from_player_not_in_room_is_rejected() {
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    // Neither is in a room.
    server
        .handle_signal(&alice, bob, json!({ "Offer": "x" }))
        .await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::NotInRoom));
    assert_silent(&mut bob_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_to_unknown_target_is_rejected() {
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;

    // Target is a random, unregistered player id.
    server
        .handle_signal(&alice, PlayerId::new_v4(), json!({ "Offer": "x" }))
        .await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalTargetNotFound));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_across_rooms_is_rejected() {
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    server
        .connection_manager
        .assign_client_to_room(&alice, uuid::Uuid::new_v4())
        .await;
    server
        .connection_manager
        .assign_client_to_room(&bob, uuid::Uuid::new_v4())
        .await;

    server
        .handle_signal(&alice, bob, json!({ "Offer": "x" }))
        .await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::CrossRoomSignal));
    assert_silent(&mut bob_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_from_non_webrtc_sender_is_rejected() {
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    // v3 but relay-only -> no WebRTC transport negotiated.
    server.set_client_protocol(&alice, v3_relay_only());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&bob, room_id)
        .await;

    server
        .handle_signal(&alice, bob, json!({ "Offer": "x" }))
        .await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::UnsupportedTransport));
    assert_silent(&mut bob_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_sender_must_be_v3_even_if_webrtc_transport_is_present() {
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v2_with_webrtc_transport());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&bob, room_id)
        .await;

    server
        .handle_signal(&alice, bob, json!({ "Offer": "x" }))
        .await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::UnsupportedTransport));
    assert_silent(&mut bob_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_to_v2_peer_reports_target_not_found() {
    // Appendix K gating: a v3 sender targeting a v2 (relay-only) peer in the
    // same room must NOT deliver — it is reported as target-not-found.
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (legacy, mut legacy_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    // legacy uses the default v2 / relay-only protocol (no set_client_protocol).

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&legacy, room_id)
        .await;

    server
        .handle_signal(&alice, legacy, json!({ "Offer": "x" }))
        .await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalTargetNotFound));
    assert_silent(&mut legacy_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_to_v3_relay_only_peer_reports_target_not_found() {
    // Appendix K gating: the target must have negotiated BOTH v3 AND the WebRTC
    // transport. A v3 peer that advertised only `relay` (a valid v3_relay_only
    // state) must NOT be delivered a `Signal` it never opted into; the sender is
    // told the target was not found. This locks the webrtc-transport gate that
    // the v2-target test passes vacuously via the v3 check.
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (relay_only, mut relay_only_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&relay_only, v3_relay_only());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&relay_only, room_id)
        .await;

    server
        .handle_signal(&alice, relay_only, json!({ "Offer": "x" }))
        .await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalTargetNotFound));
    assert_silent(&mut relay_only_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn self_signal_is_rejected() {
    // A peer cannot WebRTC to itself: a self-targeted signal is rejected and the
    // sender never receives its own echoed `Signal`.
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;

    server
        .handle_signal(&alice, alice, json!({ "Offer": "x" }))
        .await;

    // The sender receives a rejection, not its own echoed signal.
    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalTargetNotFound));
    assert!(
        !matches!(msg.as_ref(), ServerMessage::Signal { .. }),
        "sender must not receive its own echoed Signal"
    );
    assert_silent(&mut alice_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_rate_limit_trips_after_budget() {
    let server = create_test_server_with_signals(2).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&bob, room_id)
        .await;

    // First two succeed and reach bob.
    server
        .handle_signal(&alice, bob, json!({ "IceCandidate": "1" }))
        .await;
    server
        .handle_signal(&alice, bob, json!({ "IceCandidate": "2" }))
        .await;
    assert!(matches!(
        recv(&mut bob_rx).await.as_ref(),
        ServerMessage::Signal { .. }
    ));
    assert!(matches!(
        recv(&mut bob_rx).await.as_ref(),
        ServerMessage::Signal { .. }
    ));

    // Third trips the limit: sender gets an error, bob receives nothing more.
    server
        .handle_signal(&alice, bob, json!({ "IceCandidate": "3" }))
        .await;
    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalRateLimited));
    assert_silent(&mut bob_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn rejected_signal_attempts_are_rate_limited() {
    let server = create_test_server_with_signal_limits(600, 1).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;

    server
        .handle_signal(&alice, PlayerId::new_v4(), json!({ "Offer": "bad-1" }))
        .await;
    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalTargetNotFound));

    server
        .handle_signal(&alice, PlayerId::new_v4(), json!({ "Offer": "bad-2" }))
        .await;
    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalRateLimited));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn rejected_signals_do_not_consume_valid_signal_budget() {
    let server = create_test_server_with_signal_limits(1, 4).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (legacy, mut legacy_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    // legacy stays default v2 / relay-only.
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    for player in [&alice, &legacy, &bob] {
        server
            .connection_manager
            .assign_client_to_room(player, room_id)
            .await;
    }

    server
        .handle_signal(&alice, legacy, json!({ "Offer": "bad-target" }))
        .await;
    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalTargetNotFound));
    assert_silent(&mut legacy_rx).await;

    server
        .handle_signal(&alice, bob, json!({ "IceCandidate": "valid" }))
        .await;
    assert!(matches!(
        recv(&mut bob_rx).await.as_ref(),
        ServerMessage::Signal { .. }
    ));

    server
        .handle_signal(&alice, bob, json!({ "IceCandidate": "over-budget" }))
        .await;
    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalRateLimited));
}

// ---------------------------------------------------------------------------
// Late join (offerer designation).
// ---------------------------------------------------------------------------

/// Build a real DB-backed room owned by `owner`, returning its id.
async fn create_db_room(server: &EnhancedGameServer, owner: PlayerId) -> uuid::Uuid {
    create_db_room_with_max(server, owner, 8).await
}

async fn create_db_room_with_max(
    server: &EnhancedGameServer,
    owner: PlayerId,
    max_players: u8,
) -> uuid::Uuid {
    let room = server
        .database
        .create_room(
            "webrtc-game".to_string(),
            None,
            max_players,
            true,
            owner,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");
    room.id
}

fn player_info(id: PlayerId, name: &str) -> PlayerInfo {
    PlayerInfo {
        id,
        name: name.to_string(),
        is_authority: false,
        is_ready: false,
        connected_at: chrono::Utc::now(),
        connection_info: None,
        region_id: "region-a".to_string(),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_into_unfinalized_room_emits_no_new_peer() {
    // Premature-pairing suppression: a join while the room is still filling
    // (lobby_state != Finalized) must emit NO `NewPeer` — the SessionPlan owns
    // finalize-time initial pairing. Receivers are registered and asserted silent.
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (existing, mut existing_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&existing, v3_webrtc());
    server.set_client_protocol(&joiner, v3_webrtc());

    let room_id = create_db_room(&server, existing).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(joiner, "joiner"))
        .await
        .expect("add joiner");
    server
        .connection_manager
        .assign_client_to_room(&existing, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&joiner, room_id)
        .await;

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    // Room is in the default `Waiting` state.
    let room = waiting_room(&server, &room_id).await;
    server
        .handle_webrtc_late_join(&room, &joiner, &members)
        .await;

    assert_silent(&mut existing_rx).await;
    assert_silent(&mut joiner_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_finalized_mesh_room_designates_exactly_one_offerer() {
    // A Finalized room resolving to mesh pairs the joiner with each existing
    // webrtc member (one offerer per pair, UUID glare rule).
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (existing, mut existing_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&existing, v3_webrtc());
    server.set_client_protocol(&joiner, v3_webrtc());

    let room_id = create_db_room(&server, existing).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(joiner, "joiner"))
        .await
        .expect("add joiner");
    server
        .connection_manager
        .assign_client_to_room(&existing, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&joiner, room_id)
        .await;

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let room = finalized_room(&server, &room_id).await;
    server
        .handle_webrtc_late_join(&room, &joiner, &members)
        .await;

    // Both sides receive a NewPeer naming the other.
    let existing_flag = match recv(&mut existing_rx).await.as_ref() {
        ServerMessage::NewPeer {
            peer_id,
            you_initiate,
        } => {
            assert_eq!(*peer_id, joiner);
            *you_initiate
        }
        other => panic!("existing expected NewPeer, got {other:?}"),
    };
    let joiner_flag = match recv(&mut joiner_rx).await.as_ref() {
        ServerMessage::NewPeer {
            peer_id,
            you_initiate,
        } => {
            assert_eq!(*peer_id, existing);
            *you_initiate
        }
        other => panic!("joiner expected NewPeer, got {other:?}"),
    };

    // Exactly one side initiates, consistent with local_initiates (mesh rule).
    assert_ne!(existing_flag, joiner_flag);
    assert_eq!(existing_flag, local_initiates(existing, joiner));
    assert_eq!(joiner_flag, local_initiates(joiner, existing));

    assert_silent(&mut existing_rx).await;
    assert_silent(&mut joiner_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_finalized_host_room_pairs_client_with_host_only() {
    // A Finalized room resolving to host topology: a client joiner is told to
    // offer to the host (you_initiate=true) and the host is told to answer the
    // client (you_initiate=false). No client is told to initiate to another
    // client. The owner (existing) joined first, so it is the elected host.
    let server = create_test_server_with_session(host_session_config()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (client, mut client_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_webrtc_host());
    server.set_client_protocol(&client, v3_webrtc_host());

    let room_id = create_db_room(&server, host).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(client, "client"))
        .await
        .expect("add client");
    server
        .connection_manager
        .assign_client_to_room(&host, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&client, room_id)
        .await;

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    // Make the host explicit and finalize the room.
    let mut room = finalized_room(&server, &room_id).await;
    room.authority_player = Some(host);
    server
        .handle_webrtc_late_join(&room, &client, &members)
        .await;

    // The client offers to the host.
    match recv(&mut client_rx).await.as_ref() {
        ServerMessage::NewPeer {
            peer_id,
            you_initiate,
        } => {
            assert_eq!(*peer_id, host, "client's only peer is the host");
            assert!(*you_initiate, "client offers to the host");
        }
        other => panic!("client expected NewPeer(host, initiate=true), got {other:?}"),
    }
    // The host answers the client.
    match recv(&mut host_rx).await.as_ref() {
        ServerMessage::NewPeer {
            peer_id,
            you_initiate,
        } => {
            assert_eq!(*peer_id, client);
            assert!(!*you_initiate, "host answers (never offers in a star)");
        }
        other => panic!("host expected NewPeer(client, initiate=false), got {other:?}"),
    }

    assert_silent(&mut client_rx).await;
    assert_silent(&mut host_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_finalized_host_direct_room_emits_no_new_peer() {
    // Regression (Copilot): a Finalized room that resolves to `host + direct`
    // (WebRTC disabled in config) must emit NO `NewPeer`. `NewPeer` is a
    // WebRTC-signaling control message and this room's transport is Direct, not
    // WebRTC. Both members advertise the WebRTC transport, so the per-peer gate
    // (`supports_webrtc_signaling`) passes — only the plan's *transport* gate
    // suppresses pairing. Before the fix this wrongly emitted WebRTC `NewPeer`
    // for a non-WebRTC session because the match keyed on topology alone.
    let server = create_test_server_with_session(host_direct_session_config()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (client, mut client_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_webrtc_direct_host());
    server.set_client_protocol(&client, v3_webrtc_direct_host());

    let room_id = create_db_room(&server, host).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(client, "client"))
        .await
        .expect("add client");
    server
        .connection_manager
        .assign_client_to_room(&host, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&client, room_id)
        .await;

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let mut room = finalized_room(&server, &room_id).await;
    room.authority_player = Some(host);
    server
        .handle_webrtc_late_join(&room, &client, &members)
        .await;

    // host + direct ⇒ no WebRTC signaling ⇒ neither side is paired.
    assert_silent(&mut client_rx).await;
    assert_silent(&mut host_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_finalized_relay_room_emits_no_new_peer() {
    // The default-config contradiction is gone: a Finalized room that resolves
    // to the relay floor (default SessionConfig keeps relay even for all-v3
    // rooms) emits NO `NewPeer` — the active session is relay.
    let server = create_test_server().await; // default SessionConfig => relay floor
    let (existing, mut existing_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&existing, v3_webrtc());
    server.set_client_protocol(&joiner, v3_webrtc());

    let room_id = create_db_room(&server, existing).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(joiner, "joiner"))
        .await
        .expect("add joiner");
    server
        .connection_manager
        .assign_client_to_room(&existing, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&joiner, room_id)
        .await;

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let room = finalized_room(&server, &room_id).await;
    server
        .handle_webrtc_late_join(&room, &joiner, &members)
        .await;

    assert_silent(&mut existing_rx).await;
    assert_silent(&mut joiner_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn pair_webrtc_peer_with_members_skips_v2_members() {
    // Appendix K gating at the mesh-pairing primitive: a v2 (relay-only) member
    // receives no NewPeer, and the joiner is not paired with it. (The room-wide
    // gate is covered by `late_join_*` tests; here we drive the primitive
    // directly so a v2 member can be present without forcing the plan to relay.)
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (webrtc_peer, mut webrtc_rx) = register_client(&server).await;
    let (legacy, mut legacy_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&webrtc_peer, v3_webrtc());
    // legacy stays on default v2 / relay-only.
    server.set_client_protocol(&joiner, v3_webrtc());

    let room_id = create_db_room(&server, webrtc_peer).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(legacy, "legacy"))
        .await
        .expect("add legacy");
    server
        .database
        .add_player_to_room(&room_id, player_info(joiner, "joiner"))
        .await
        .expect("add joiner");
    server
        .connection_manager
        .assign_client_to_room(&webrtc_peer, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&legacy, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&joiner, room_id)
        .await;

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    // A relay-only member forces the room-wide plan to relay, which would emit
    // no NewPeer at all. To exercise the per-member skip while still resolving
    // to mesh, drive the mesh primitive directly with the legacy member present.
    server
        .pair_webrtc_peer_with_members(&joiner, &members)
        .await;

    // The WebRTC peer and the joiner are paired.
    match recv(&mut webrtc_rx).await.as_ref() {
        ServerMessage::NewPeer { peer_id, .. } => assert_eq!(*peer_id, joiner),
        other => panic!("expected NewPeer, got {other:?}"),
    }
    match recv(&mut joiner_rx).await.as_ref() {
        ServerMessage::NewPeer { peer_id, .. } => assert_eq!(*peer_id, webrtc_peer),
        other => panic!("expected NewPeer, got {other:?}"),
    }

    // The legacy member is never told about the joiner, and the joiner is only
    // paired with the single WebRTC peer (no NewPeer for legacy).
    assert_silent(&mut legacy_rx).await;
    assert_silent(&mut joiner_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_noop_when_joiner_is_relay_only() {
    // The joiner gate fires before finalization/topology is even considered: a
    // relay-only joiner is never paired, regardless of room state.
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (existing, mut existing_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&existing, v3_webrtc());
    server.set_client_protocol(&joiner, v3_relay_only());

    let room_id = create_db_room(&server, existing).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(joiner, "joiner"))
        .await
        .expect("add joiner");
    server
        .connection_manager
        .assign_client_to_room(&existing, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&joiner, room_id)
        .await;

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let room = finalized_room(&server, &room_id).await;
    server
        .handle_webrtc_late_join(&room, &joiner, &members)
        .await;

    // A relay-only joiner triggers no NewPeer in either direction.
    assert_silent(&mut existing_rx).await;
    assert_silent(&mut joiner_rx).await;
}

// ---------------------------------------------------------------------------
// Pairing primitives (direct).
// ---------------------------------------------------------------------------

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn pair_webrtc_peer_with_members_designates_one_offerer_per_pair() {
    // The mesh primitive: pair one peer with every other webrtc member, exactly
    // one offerer per pair (UUID glare rule).
    let server = create_test_server().await;
    let (peer, mut peer_rx) = register_client(&server).await;
    let (other, mut other_rx) = register_client(&server).await;
    server.set_client_protocol(&peer, v3_webrtc());
    server.set_client_protocol(&other, v3_webrtc());

    let members = vec![player_info(peer, "peer"), player_info(other, "other")];
    server.pair_webrtc_peer_with_members(&peer, &members).await;

    let other_flag = match recv(&mut other_rx).await.as_ref() {
        ServerMessage::NewPeer {
            peer_id,
            you_initiate,
        } => {
            assert_eq!(*peer_id, peer);
            *you_initiate
        }
        other => panic!("other expected NewPeer, got {other:?}"),
    };
    let peer_flag = match recv(&mut peer_rx).await.as_ref() {
        ServerMessage::NewPeer {
            peer_id,
            you_initiate,
        } => {
            assert_eq!(*peer_id, other);
            *you_initiate
        }
        other => panic!("peer expected NewPeer, got {other:?}"),
    };
    assert_ne!(peer_flag, other_flag);
    assert_eq!(peer_flag, local_initiates(peer, other));

    assert_silent(&mut peer_rx).await;
    assert_silent(&mut other_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn pair_webrtc_peer_with_host_client_offers_host_answers() {
    // The star primitive: a client joiner offers to the host; the host answers.
    let server = create_test_server().await;
    let (host, mut host_rx) = register_client(&server).await;
    let (client, mut client_rx) = register_client(&server).await;
    let (other_client, mut other_client_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_webrtc());
    server.set_client_protocol(&client, v3_webrtc());
    server.set_client_protocol(&other_client, v3_webrtc());

    let members = vec![
        player_info(host, "host"),
        player_info(client, "client"),
        player_info(other_client, "other"),
    ];
    server
        .pair_webrtc_peer_with_host(&client, host, &members)
        .await;

    // Client offers to the host.
    match recv(&mut client_rx).await.as_ref() {
        ServerMessage::NewPeer {
            peer_id,
            you_initiate,
        } => {
            assert_eq!(*peer_id, host);
            assert!(*you_initiate);
        }
        other => panic!("client expected NewPeer(host, true), got {other:?}"),
    }
    // Host answers the client.
    match recv(&mut host_rx).await.as_ref() {
        ServerMessage::NewPeer {
            peer_id,
            you_initiate,
        } => {
            assert_eq!(*peer_id, client);
            assert!(!*you_initiate);
        }
        other => panic!("host expected NewPeer(client, false), got {other:?}"),
    }

    // The other client is never paired with this client (clients never mesh).
    assert_silent(&mut other_client_rx).await;
    assert_silent(&mut client_rx).await;
    assert_silent(&mut host_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn pair_webrtc_peer_with_host_host_joiner_pairs_every_client() {
    // When the host itself (re)joins, it is paired with every webrtc client; each
    // client offers, the host answers.
    let server = create_test_server().await;
    let (host, mut host_rx) = register_client(&server).await;
    let (client_a, mut client_a_rx) = register_client(&server).await;
    let (client_b, mut client_b_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_webrtc());
    server.set_client_protocol(&client_a, v3_webrtc());
    server.set_client_protocol(&client_b, v3_webrtc());

    let members = vec![
        player_info(host, "host"),
        player_info(client_a, "a"),
        player_info(client_b, "b"),
    ];
    server
        .pair_webrtc_peer_with_host(&host, host, &members)
        .await;

    // Each client is told to offer to the host.
    for rx in [&mut client_a_rx, &mut client_b_rx] {
        match recv(rx).await.as_ref() {
            ServerMessage::NewPeer {
                peer_id,
                you_initiate,
            } => {
                assert_eq!(*peer_id, host);
                assert!(*you_initiate, "clients offer to the host");
            }
            other => panic!("client expected NewPeer(host, true), got {other:?}"),
        }
    }

    // The host answers both clients (two NewPeer, both initiate=false).
    let mut answered = std::collections::HashSet::new();
    for _ in 0..2 {
        match recv(&mut host_rx).await.as_ref() {
            ServerMessage::NewPeer {
                peer_id,
                you_initiate,
            } => {
                assert!(!*you_initiate, "host answers every client");
                answered.insert(*peer_id);
            }
            other => panic!("host expected NewPeer, got {other:?}"),
        }
    }
    assert!(answered.contains(&client_a));
    assert!(answered.contains(&client_b));

    assert_silent(&mut host_rx).await;
    assert_silent(&mut client_a_rx).await;
    assert_silent(&mut client_b_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reconnect_restores_room_membership_and_webrtc_pairing() {
    // Reconnect re-pairing is now finalization + topology gated: only a Finalized
    // room with a non-relay plan re-pairs via NewPeer. Set up a mesh server and a
    // finalized 2-player room so the reconnecting peer is re-meshed with the
    // existing peer.
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (existing, mut existing_rx) = register_client(&server).await;
    let (reconnecting, _old_rx) = register_client(&server).await;
    let (current, mut current_rx) = register_client(&server).await;
    server.set_client_protocol(&existing, v3_webrtc());
    server.set_client_protocol(&reconnecting, v3_webrtc());
    server.set_client_protocol(&current, v3_webrtc());

    let room_id = create_db_room_with_max(&server, existing, 2).await;
    let reconnecting_info = player_info(reconnecting, "reconnecting");
    server
        .database
        .add_player_to_room(&room_id, reconnecting_info.clone())
        .await
        .expect("add reconnecting player");
    server
        .connection_manager
        .assign_client_to_room(&existing, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&reconnecting, room_id)
        .await;

    // Finalize the (now full) room so the post-reconnect re-pairing engages.
    finalize_db_room(&server, &room_id, &[existing, reconnecting]).await;

    let token = server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(reconnecting, room_id, false, Some(reconnecting_info))
        .await;
    server
        .database
        .remove_player_from_room(&room_id, &reconnecting)
        .await
        .expect("remove reconnecting player");
    server.connection_manager.remove_client(&reconnecting);
    let _ = server
        .message_coordinator
        .unregister_local_client(&reconnecting)
        .await;

    let reconnected = server
        .handle_reconnect(&current, &reconnecting, &room_id, &token)
        .await;
    assert!(reconnected, "valid reconnect should report success");

    match recv(&mut current_rx).await.as_ref() {
        ServerMessage::Reconnected(payload) => {
            assert_eq!(payload.player_id, reconnecting);
            assert!(
                payload
                    .current_players
                    .iter()
                    .any(|player| player.id == reconnecting),
                "reconnected payload should include restored player membership"
            );
        }
        other => panic!("expected Reconnected, got {other:?}"),
    }

    match recv(&mut existing_rx).await.as_ref() {
        ServerMessage::PlayerReconnected { player_id } => assert_eq!(*player_id, reconnecting),
        other => panic!("expected PlayerReconnected, got {other:?}"),
    }

    let existing_flag = match recv(&mut existing_rx).await.as_ref() {
        ServerMessage::NewPeer {
            peer_id,
            you_initiate,
        } => {
            assert_eq!(*peer_id, reconnecting);
            *you_initiate
        }
        other => panic!("existing expected NewPeer after reconnect, got {other:?}"),
    };
    let reconnecting_flag = match recv(&mut current_rx).await.as_ref() {
        ServerMessage::NewPeer {
            peer_id,
            you_initiate,
        } => {
            assert_eq!(*peer_id, existing);
            *you_initiate
        }
        other => panic!("reconnecting expected NewPeer after reconnect, got {other:?}"),
    };
    assert_ne!(existing_flag, reconnecting_flag);

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    assert!(
        members.iter().any(|player| player.id == reconnecting),
        "reconnected player must be restored in room storage for future pairing"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reconnect_room_full_failure_releases_claim_for_retry() {
    let server = create_test_server().await;
    let (existing, _existing_rx) = register_client(&server).await;
    let (reconnecting, _old_rx) = register_client(&server).await;
    let (filler, _filler_rx) = register_client(&server).await;
    let (current, mut current_rx) = register_client(&server).await;
    server.set_client_protocol(&existing, v3_webrtc());
    server.set_client_protocol(&current, v3_webrtc());

    let room_id = create_db_room_with_max(&server, existing, 2).await;
    let reconnecting_info = player_info(reconnecting, "reconnecting");
    server
        .database
        .add_player_to_room(&room_id, reconnecting_info.clone())
        .await
        .expect("add reconnecting player");
    server
        .connection_manager
        .assign_client_to_room(&existing, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&reconnecting, room_id)
        .await;

    let token = server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(reconnecting, room_id, false, Some(reconnecting_info))
        .await;
    server
        .database
        .remove_player_from_room(&room_id, &reconnecting)
        .await
        .expect("remove reconnecting player");
    server.connection_manager.remove_client(&reconnecting);
    let _ = server
        .message_coordinator
        .unregister_local_client(&reconnecting)
        .await;

    server
        .database
        .add_player_to_room(&room_id, player_info(filler, "filler"))
        .await
        .expect("add filler player");
    server
        .connection_manager
        .assign_client_to_room(&filler, room_id)
        .await;

    let first_attempt = server
        .handle_reconnect(&current, &reconnecting, &room_id, &token)
        .await;
    assert!(!first_attempt, "full room reconnect attempt must fail");
    match recv(&mut current_rx).await.as_ref() {
        ServerMessage::ReconnectionFailed { error_code, .. } => {
            assert_eq!(*error_code, ErrorCode::RoomFull);
        }
        other => panic!("expected ReconnectionFailed(RoomFull), got {other:?}"),
    }
    server
        .reconnection_manager()
        .expect("reconnection enabled")
        .validate_reconnection(&reconnecting, &room_id, &token)
        .await
        .expect("failed room-full attempt must release claim for retry");

    server
        .database
        .remove_player_from_room(&room_id, &filler)
        .await
        .expect("remove filler player");
    server.connection_manager.clear_room_assignment(&filler);

    let second_attempt = server
        .handle_reconnect(&current, &reconnecting, &room_id, &token)
        .await;
    assert!(
        second_attempt,
        "same token should succeed after room has space"
    );
    match recv(&mut current_rx).await.as_ref() {
        ServerMessage::Reconnected(payload) => {
            assert_eq!(payload.player_id, reconnecting);
        }
        other => panic!("expected Reconnected after retry, got {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reconnect_reassign_failure_rolls_back_membership_and_releases_claim() {
    let server = create_test_server().await;
    let (existing, _existing_rx) = register_client(&server).await;
    let (reconnecting, _old_rx) = register_client(&server).await;
    let (current, _current_rx) = register_client(&server).await;

    let room_id = create_db_room(&server, existing).await;
    let reconnecting_info = player_info(reconnecting, "reconnecting");
    server
        .database
        .add_player_to_room(&room_id, reconnecting_info.clone())
        .await
        .expect("add reconnecting player");
    server
        .connection_manager
        .assign_client_to_room(&existing, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&reconnecting, room_id)
        .await;

    let token = server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(reconnecting, room_id, false, Some(reconnecting_info))
        .await;
    server
        .database
        .remove_player_from_room(&room_id, &reconnecting)
        .await
        .expect("remove reconnecting player");
    server.connection_manager.remove_client(&reconnecting);
    let _ = server
        .message_coordinator
        .unregister_local_client(&reconnecting)
        .await;

    server.connection_manager.remove_client(&current);
    let first_attempt = server
        .handle_reconnect(&current, &reconnecting, &room_id, &token)
        .await;
    assert!(
        !first_attempt,
        "reconnect should fail when temporary connection disappears"
    );

    server
        .reconnection_manager()
        .expect("reconnection enabled")
        .validate_reconnection(&reconnecting, &room_id, &token)
        .await
        .expect("reassign failure must release claim for retry");
    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    assert!(
        !members.iter().any(|player| player.id == reconnecting),
        "failed reassign must roll back restored room membership"
    );

    let (replacement, mut replacement_rx) = register_client(&server).await;
    let second_attempt = server
        .handle_reconnect(&replacement, &reconnecting, &room_id, &token)
        .await;
    assert!(second_attempt, "same token should retry successfully");
    match recv(&mut replacement_rx).await.as_ref() {
        ServerMessage::Reconnected(payload) => {
            assert_eq!(payload.player_id, reconnecting);
        }
        other => panic!("expected Reconnected after retry, got {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reconnect_from_roomed_temporary_connection_is_rejected_without_ghost_membership() {
    let server = create_test_server().await;
    let (existing, _existing_rx) = register_client(&server).await;
    let (reconnecting, _old_rx) = register_client(&server).await;
    let (current, mut current_rx) = register_client(&server).await;

    let target_room_id = create_db_room(&server, existing).await;
    let reconnecting_info = player_info(reconnecting, "reconnecting");
    server
        .database
        .add_player_to_room(&target_room_id, reconnecting_info.clone())
        .await
        .expect("add reconnecting player");
    server
        .connection_manager
        .assign_client_to_room(&reconnecting, target_room_id)
        .await;

    let token = server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(reconnecting, target_room_id, false, Some(reconnecting_info))
        .await;
    server
        .database
        .remove_player_from_room(&target_room_id, &reconnecting)
        .await
        .expect("remove reconnecting player");
    server.connection_manager.remove_client(&reconnecting);
    let _ = server
        .message_coordinator
        .unregister_local_client(&reconnecting)
        .await;

    let current_room_id = create_db_room(&server, current).await;
    server
        .connection_manager
        .assign_client_to_room(&current, current_room_id)
        .await;

    let reconnected = server
        .handle_reconnect(&current, &reconnecting, &target_room_id, &token)
        .await;
    assert!(
        !reconnected,
        "reconnect from an already-roomed temporary client must fail"
    );

    match recv(&mut current_rx).await.as_ref() {
        ServerMessage::ReconnectionFailed { error_code, .. } => {
            assert_eq!(*error_code, ErrorCode::ReconnectionFailed);
        }
        other => panic!("expected ReconnectionFailed, got {other:?}"),
    }

    let current_room_members = server
        .database
        .get_room_players(&current_room_id)
        .await
        .expect("current room players");
    assert!(
        current_room_members
            .iter()
            .any(|player| player.id == current),
        "failed reconnect must not orphan the temporary player's existing room membership"
    );
    assert!(
        server.connection_manager.has_client(&current),
        "failed reconnect must leave the temporary connection registered"
    );
    assert!(
        !server.connection_manager.has_client(&reconnecting),
        "failed reconnect must not claim the disconnected player's id"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn concurrent_reconnect_attempts_with_same_token_allow_exactly_one_winner() {
    let server = create_test_server().await;
    let (existing, _existing_rx) = register_client(&server).await;
    let (reconnecting, _old_rx) = register_client(&server).await;
    let (current_a, _current_a_rx) = register_client(&server).await;
    let (current_b, _current_b_rx) = register_client(&server).await;

    let room_id = create_db_room(&server, existing).await;
    let reconnecting_info = player_info(reconnecting, "reconnecting");
    server
        .database
        .add_player_to_room(&room_id, reconnecting_info.clone())
        .await
        .expect("add reconnecting player");
    server
        .connection_manager
        .assign_client_to_room(&reconnecting, room_id)
        .await;

    let token = server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(reconnecting, room_id, false, Some(reconnecting_info))
        .await;
    server
        .database
        .remove_player_from_room(&room_id, &reconnecting)
        .await
        .expect("remove reconnecting player");
    server.connection_manager.remove_client(&reconnecting);
    let _ = server
        .message_coordinator
        .unregister_local_client(&reconnecting)
        .await;

    let server_a = Arc::clone(&server);
    let server_b = Arc::clone(&server);
    let token_a = token.clone();
    let token_b = token.clone();
    let reconnect_a = async move {
        server_a
            .handle_reconnect(&current_a, &reconnecting, &room_id, &token_a)
            .await
    };
    let reconnect_b = async move {
        server_b
            .handle_reconnect(&current_b, &reconnecting, &room_id, &token_b)
            .await
    };

    let (success_a, success_b) = tokio::join!(reconnect_a, reconnect_b);
    let success_count = [success_a, success_b]
        .into_iter()
        .filter(|success| *success)
        .count();
    assert_eq!(
        success_count, 1,
        "exactly one concurrent reconnect may claim a single-use token"
    );
    assert!(
        server.connection_manager.has_client(&reconnecting),
        "the winning reconnect should own the restored player id"
    );
    let remaining_temporary_connections = [current_a, current_b]
        .into_iter()
        .filter(|player_id| server.connection_manager.has_client(player_id))
        .count();
    assert_eq!(
        remaining_temporary_connections, 1,
        "the losing fresh connection should remain temporary; the winner should be reassigned"
    );

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    assert_eq!(
        members
            .iter()
            .filter(|player| player.id == reconnecting)
            .count(),
        1,
        "same-token concurrent reconnects must not duplicate room membership"
    );
}
