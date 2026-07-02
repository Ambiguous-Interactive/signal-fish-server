//! Handler tests for the P2 targeted signal relay (`signaling.rs`).
//!
//! Mirrors the `message_router_tests` harness: register clients, set their
//! negotiated protocol, drive `handle_signal` /
//! `handle_active_session_late_join`, and assert on what each client receives.
//! Covers the happy path, every rejection branch, glare determinism, late-join
//! plan delivery + offerer designation against the stored `ActiveSessionPlan`,
//! and v2 gating (Appendix K).

use crate::config::{
    AuthMaintenanceConfig, CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig,
    SessionConfig, TransportSecurityConfig, TurnConfig,
};
use crate::database::{DatabaseConfig, InMemoryDatabase};
use crate::protocol::{
    ClientMessage, ErrorCode, IceServer, LobbyState, PlayerId, PlayerInfo, Room, ServerMessage,
    Topology, Transport,
};
use crate::rate_limit::RateLimitConfig;
use crate::server::{EnhancedGameServer, NegotiatedProtocol, ServerConfig};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

use super::session_policy::{ActiveSessionPlan, SessionPlanDecision};
use super::signaling::local_initiates;

const STATIC_STUN_URL: &str = "stun:static.example.com:3478";
const TURN_STUN_URL: &str = "stun:stun.l.google.com:19302";
const TURN_URL: &str = "turn:turn.example.com:3478";
const TURN_CREDENTIAL_TTL_SECS: u64 = 3600;

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
    create_test_server_with_signal_policy(max_signals, max_signal_errors, 16384).await
}

/// Build a server with full control over the per-connection signal budget AND
/// the serialized-payload size cap (`security.max_signal_bytes`).
async fn create_test_server_with_signal_policy(
    max_signals: u32,
    max_signal_errors: u32,
    max_signal_bytes: usize,
) -> Arc<EnhancedGameServer> {
    let config = ServerConfig {
        max_signal_bytes,
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
        TurnConfig::default(),
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
    create_test_server_with_session_and_turn(session, TurnConfig::default()).await
}

/// Build a server with the given session **and** TURN config, so the late-join
/// ICE-minting path can be exercised with active TURN credentials.
async fn create_test_server_with_session_and_turn(
    session: SessionConfig,
    turn: TurnConfig,
) -> Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
        ServerConfig::default(),
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        session,
        turn,
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

/// Rehydrate a [`SessionPlanDecision`] exactly as the late-join path does —
/// the sticky (topology, transport, host) over the current members with their
/// negotiated capabilities — so the `NewPeer` announce primitives can be
/// driven directly.
fn decision_for(
    server: &EnhancedGameServer,
    topology: Topology,
    transport: Transport,
    host: Option<PlayerId>,
    members: &[PlayerInfo],
) -> SessionPlanDecision {
    ActiveSessionPlan {
        topology,
        transport,
        host,
    }
    .decision_with(server.session_members_from(members))
}

/// Record `room_id`'s sticky session decision directly, isolating the late-join
/// path from finalize emission (which is covered by `session_policy_tests`).
fn store_active_plan(
    server: &EnhancedGameServer,
    room_id: uuid::Uuid,
    topology: Topology,
    transport: Transport,
    host: Option<PlayerId>,
) {
    server.active_session_plans.insert(
        room_id,
        ActiveSessionPlan {
            topology,
            transport,
            host,
        },
    );
}

#[test]
fn ice_ordering_fixtures_are_source_distinguishable() {
    assert_ne!(
        STATIC_STUN_URL, TURN_STUN_URL,
        "static and default [turn] STUN fixture URLs must stay distinct so ordering assertions catch swaps"
    );
    assert_eq!(
        TurnConfig::default().stun_urls,
        vec![TURN_STUN_URL.to_string()],
        "late-join signaling fixtures intentionally exercise the default [turn] STUN URL"
    );
}

/// A STUN-only `SessionConfig` preferring `mesh` (so an all-v3+webrtc room
/// resolves to `mesh + webrtc`).
fn mesh_session_config() -> SessionConfig {
    SessionConfig {
        default_topology: Topology::Mesh,
        ice_servers: vec![IceServer {
            urls: vec![STATIC_STUN_URL.to_string()],
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
            urls: vec![STATIC_STUN_URL.to_string()],
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

fn assert_static_then_default_stun_ice(ice_servers: &[IceServer]) {
    assert_eq!(
        ice_servers.len(),
        2,
        "webrtc plans carry static ICE followed by the default [turn] STUN entry"
    );
    assert_eq!(ice_servers[0].urls, vec![STATIC_STUN_URL]);
    assert_eq!(ice_servers[1].urls, vec![TURN_STUN_URL]);
    assert!(
        ice_servers
            .iter()
            .all(|server| server.username.is_none() && server.credential.is_none()),
        "no TURN credentials are minted when [turn] is disabled"
    );
}

/// Fetch a room and mark it `Finalized` so late-join handling engages (it only
/// fires for an active, finalized session). The stored lobby state is irrelevant
/// to `handle_active_session_late_join`, which reads `lobby_state` from the
/// passed `Room` (the stored ActiveSessionPlan is still looked up by room id).
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
        .register_client(
            sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            next_addr(),
            server.instance_id,
        )
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

/// A v3 client that negotiated the WebRTC *transport* but only the relay
/// *topology*: it passes the transport-level `supports_webrtc_signaling` gate
/// yet cannot run a mesh or star session — the discriminator proving `NewPeer`
/// pairing gates on the FULL session predicate (v3 + topology + transport),
/// not on the transport alone.
fn v3_webrtc_relay_topology_only() -> NegotiatedProtocol {
    NegotiatedProtocol {
        version: 3,
        transports: vec![Transport::Relay, Transport::WebRtc],
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
    match timeout(Duration::from_millis(100), receiver.recv()).await {
        Err(_) => {}
        Ok(Some(message)) => panic!("expected no message to be delivered, got {message:?}"),
        Ok(None) => panic!("channel closed while checking for silence"),
    }
}

async fn valid_signal_budget_used(server: &EnhancedGameServer, player_id: &PlayerId) -> u32 {
    server
        .rate_limiter
        .get_player_stats(player_id)
        .await
        .map_or(0, |stats| stats.signals)
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
// Signal payload size cap (`security.max_signal_bytes`).
// ---------------------------------------------------------------------------

/// Register a v3+WebRTC pair sharing one room on `server`.
async fn webrtc_pair_in_room(
    server: &EnhancedGameServer,
) -> (
    PlayerId,
    mpsc::Receiver<Arc<ServerMessage>>,
    PlayerId,
    mpsc::Receiver<Arc<ServerMessage>>,
) {
    let (alice, alice_rx) = register_client(server).await;
    let (bob, bob_rx) = register_client(server).await;
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

    (alice, alice_rx, bob, bob_rx)
}

/// Serialized length of an opaque signal payload, exactly as the cap measures it.
fn payload_len(signal: &serde_json::Value) -> usize {
    serde_json::to_vec(signal).expect("signal serializes").len()
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_exactly_at_size_cap_is_relayed() {
    let payload = json!({ "Offer": "v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\n" });
    // Boundary: the cap equals the payload's canonical serialized length.
    let server = create_test_server_with_signal_policy(600, 60, payload_len(&payload)).await;
    let (alice, mut alice_rx, bob, mut bob_rx) = webrtc_pair_in_room(&server).await;

    server.handle_signal(&alice, bob, payload.clone()).await;

    match recv(&mut bob_rx).await.as_ref() {
        ServerMessage::Signal { from, signal } => {
            assert_eq!(*from, alice);
            assert_eq!(*signal, payload, "at-cap payload must relay byte-preserved");
        }
        other => panic!("expected Signal, got {other:?}"),
    }
    assert_silent(&mut alice_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_one_byte_over_size_cap_is_rejected() {
    let payload = json!({ "Offer": "v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\n" });
    // Boundary: cap is one byte below the payload's serialized length.
    let server = create_test_server_with_signal_policy(600, 60, payload_len(&payload) - 1).await;
    let (alice, mut alice_rx, bob, mut bob_rx) = webrtc_pair_in_room(&server).await;

    server.handle_signal(&alice, bob, payload).await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalTooLarge));
    if let ServerMessage::Error { message, .. } = msg.as_ref() {
        assert!(
            message.contains("bytes"),
            "rejection should name the sizes: {message}"
        );
    }
    assert_silent(&mut bob_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn oversized_signal_is_rejected_before_any_other_check() {
    // The size cap is step 0: even a sender that is not in any room gets the
    // size rejection, not NotInRoom, proving no relay work precedes the cap.
    let server = create_test_server_with_signal_policy(600, 60, 8).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());

    server
        .handle_signal(
            &alice,
            PlayerId::new_v4(),
            json!({ "Offer": "definitely-longer-than-eight-bytes" }),
        )
        .await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalTooLarge));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn oversized_signal_rejection_does_not_consume_valid_signal_budget() {
    let small = json!({ "IceCandidate": "ok" });
    let oversized = json!({ "Offer": "x".repeat(64) });
    // Valid-signal budget of exactly 1, cap sized to admit only `small`.
    let server = create_test_server_with_signal_policy(1, 60, payload_len(&small)).await;
    let (alice, mut alice_rx, bob, mut bob_rx) = webrtc_pair_in_room(&server).await;

    // Oversized first: rejected without touching the valid-signal budget.
    server.handle_signal(&alice, bob, oversized).await;
    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalTooLarge));
    assert_silent(&mut bob_rx).await;

    // The single valid-signal budget slot is still available...
    server.handle_signal(&alice, bob, small.clone()).await;
    match recv(&mut bob_rx).await.as_ref() {
        ServerMessage::Signal { signal, .. } => assert_eq!(*signal, small),
        other => panic!("expected Signal, got {other:?}"),
    }

    // ...and was budget slot #1 of 1, proving the oversized attempt did not
    // consume it.
    server.handle_signal(&alice, bob, small).await;
    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalRateLimited));
    assert_silent(&mut bob_rx).await;
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
async fn late_join_into_unfinalized_room_emits_nothing() {
    // Premature-pairing suppression: a join while the room is still filling
    // (lobby_state != Finalized) must emit NO message — the SessionPlan owns
    // finalize-time initial pairing. A stored plan is present (anomalous for a
    // Waiting room) to prove the Finalized gate fires before the stored-plan
    // lookup. Receivers are registered and asserted silent.
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (existing, mut existing_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&existing, v3_webrtc());
    server.set_client_protocol(&joiner, v3_webrtc());

    let room_id = create_db_room(&server, existing).await;
    store_active_plan(&server, room_id, Topology::Mesh, Transport::WebRtc, None);
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
        .handle_active_session_late_join(&room, &joiner, &members)
        .await;

    assert_silent(&mut existing_rx).await;
    assert_silent(&mut joiner_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_stored_mesh_sends_joiner_plan_and_existing_new_peer() {
    // A Finalized room with a stored mesh+webrtc decision: the JOINER receives a
    // tailored SessionPlan (current members, glare-correct initiate, ICE) and NO
    // NewPeer; each EXISTING webrtc member receives the NewPeer delta naming the
    // joiner (one offerer per pair, UUID glare rule).
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (existing, mut existing_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&existing, v3_webrtc());
    server.set_client_protocol(&joiner, v3_webrtc());

    let room_id = create_db_room(&server, existing).await;
    store_active_plan(&server, room_id, Topology::Mesh, Transport::WebRtc, None);
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
        .handle_active_session_late_join(&room, &joiner, &members)
        .await;

    // The joiner gets its tailored view of the RUNNING session, not a NewPeer.
    let joiner_plan = match recv(&mut joiner_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("joiner expected SessionPlan, got {other:?}"),
    };
    assert_eq!(joiner_plan.topology, Topology::Mesh);
    assert_eq!(joiner_plan.transport, Transport::WebRtc);
    assert_eq!(joiner_plan.fallback, Transport::Relay);
    assert!(joiner_plan.host.is_none());
    assert_eq!(joiner_plan.peers.len(), 1, "one existing peer");
    assert_eq!(joiner_plan.peers[0].player_id, existing);
    assert_eq!(
        joiner_plan.peers[0].initiate,
        local_initiates(joiner, existing),
        "joiner's initiate flag follows the glare rule"
    );
    // WebRTC plan carries ICE: the static STUN from `mesh_session_config` plus
    // the default `[turn]` block's public STUN (TURN disabled => no creds).
    assert_static_then_default_stun_ice(&joiner_plan.ice_servers);

    // The existing member gets the antisymmetric NewPeer delta.
    match recv(&mut existing_rx).await.as_ref() {
        ServerMessage::NewPeer {
            peer_id,
            you_initiate,
        } => {
            assert_eq!(*peer_id, joiner);
            assert_eq!(*you_initiate, local_initiates(existing, joiner));
            assert_ne!(
                *you_initiate, joiner_plan.peers[0].initiate,
                "exactly one side of the pair initiates"
            );
        }
        other => panic!("existing expected NewPeer, got {other:?}"),
    }

    assert_silent(&mut existing_rx).await;
    assert_silent(&mut joiner_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_stored_host_client_joiner_gets_plan_host_gets_new_peer() {
    // A Finalized room with a stored host+webrtc decision: a CLIENT joiner gets
    // a SessionPlan targeting the STORED host only (initiate=true) and no
    // NewPeer; the host gets the NewPeer delta (you_initiate=false). No other
    // client hears anything (clients never signal each other in a star).
    let server = create_test_server_with_session(host_session_config()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (other_client, mut other_client_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_webrtc_host());
    server.set_client_protocol(&other_client, v3_webrtc_host());
    server.set_client_protocol(&joiner, v3_webrtc_host());

    let room_id = create_db_room(&server, host).await;
    store_active_plan(
        &server,
        room_id,
        Topology::Host,
        Transport::WebRtc,
        Some(host),
    );
    for (id, name) in [(other_client, "other"), (joiner, "joiner")] {
        server
            .database
            .add_player_to_room(&room_id, player_info(id, name))
            .await
            .expect("add member");
    }
    for id in [&host, &other_client, &joiner] {
        server
            .connection_manager
            .assign_client_to_room(id, room_id)
            .await;
    }

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let room = finalized_room(&server, &room_id).await;
    let replans_before = server
        .metrics
        .session_replans_emitted
        .load(Ordering::Relaxed);
    server
        .handle_active_session_late_join(&room, &joiner, &members)
        .await;

    // A normal late join — the stored host IS present — must never trigger the
    // self-heal re-plan.
    assert_eq!(
        server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        replans_before,
        "a late join with the stored host present is not a re-plan"
    );

    // The joiner's plan: star view, stored host, host is its only peer.
    let joiner_plan = match recv(&mut joiner_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("joiner expected SessionPlan, got {other:?}"),
    };
    assert_eq!(joiner_plan.topology, Topology::Host);
    assert_eq!(joiner_plan.transport, Transport::WebRtc);
    assert_eq!(joiner_plan.host, Some(host));
    assert_eq!(joiner_plan.peers.len(), 1, "a client targets the host only");
    assert_eq!(joiner_plan.peers[0].player_id, host);
    assert!(
        joiner_plan.peers[0].initiate,
        "the client offers to the host"
    );
    assert!(joiner_plan.peers[0].is_authority);

    // The host answers the joiner.
    match recv(&mut host_rx).await.as_ref() {
        ServerMessage::NewPeer {
            peer_id,
            you_initiate,
        } => {
            assert_eq!(*peer_id, joiner);
            assert!(!*you_initiate, "host answers (never offers in a star)");
        }
        other => panic!("host expected NewPeer(joiner, initiate=false), got {other:?}"),
    }

    assert_silent(&mut joiner_rx).await;
    assert_silent(&mut host_rx).await;
    assert_silent(&mut other_client_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_after_host_failover_pairs_ex_host_as_client_of_new_host() {
    // Host failover then ex-host rejoin: the stored entry now names the
    // re-elected host, so the returning ex-host is brought in as a CLIENT — its
    // plan targets the new host (initiate=true) and the new host answers it.
    // This pins that late-join uses the STORED host, not a re-election.
    let server = create_test_server_with_session(host_session_config()).await;
    let (new_host, mut new_host_rx) = register_client(&server).await;
    let (ex_host, mut ex_host_rx) = register_client(&server).await;
    server.set_client_protocol(&new_host, v3_webrtc_host());
    server.set_client_protocol(&ex_host, v3_webrtc_host());

    // The room was created by the ex-host (it would win a re-election as the
    // earliest joiner if late-join wrongly re-elected instead of using the
    // stored entry), but the stored post-failover host is `new_host`.
    let room_id = create_db_room(&server, ex_host).await;
    store_active_plan(
        &server,
        room_id,
        Topology::Host,
        Transport::WebRtc,
        Some(new_host),
    );
    server
        .database
        .add_player_to_room(&room_id, player_info(new_host, "new-host"))
        .await
        .expect("add new host");
    for id in [&new_host, &ex_host] {
        server
            .connection_manager
            .assign_client_to_room(id, room_id)
            .await;
    }

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let room = finalized_room(&server, &room_id).await;
    server
        .handle_active_session_late_join(&room, &ex_host, &members)
        .await;

    let ex_host_plan = match recv(&mut ex_host_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("ex-host expected SessionPlan, got {other:?}"),
    };
    assert_eq!(ex_host_plan.host, Some(new_host), "stored host wins");
    assert_eq!(ex_host_plan.peers.len(), 1);
    assert_eq!(ex_host_plan.peers[0].player_id, new_host);
    assert!(
        ex_host_plan.peers[0].initiate,
        "the ex-host is now a client and offers to the re-elected host"
    );

    match recv(&mut new_host_rx).await.as_ref() {
        ServerMessage::NewPeer {
            peer_id,
            you_initiate,
        } => {
            assert_eq!(*peer_id, ex_host);
            assert!(!*you_initiate, "the re-elected host answers the ex-host");
        }
        other => panic!("new host expected NewPeer, got {other:?}"),
    }

    assert_silent(&mut ex_host_rx).await;
    assert_silent(&mut new_host_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_with_missing_stored_host_heals_via_replan() {
    // Self-heal on late join: the stored host of a Finalized host+webrtc room
    // is no longer a member (wedge state — e.g. the departure hook was skipped
    // by a transient storage error before the seat refilled). The join must
    // trigger the same capability-aware re-election + full re-plan a host
    // departure does: EVERY current member — the joiner included — receives
    // exactly ONE fresh SessionPlan (the healed plan naming the re-elected
    // host, already carrying the joiner pairing), NO NewPeer fires to anyone,
    // the re-plan counter moves once, and the late-join counter does NOT move
    // (the joiner was served by the re-plan event, not a late-join plan).
    let server = create_test_server_with_session(host_session_config()).await;
    let (existing, mut existing_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&existing, v3_webrtc_host());
    server.set_client_protocol(&joiner, v3_webrtc_host());

    // `existing` owns the room (and holds authority); the stored host is a
    // ghost id that was never restored to the member list.
    let room_id = create_db_room(&server, existing).await;
    let ghost_host = PlayerId::new_v4();
    store_active_plan(
        &server,
        room_id,
        Topology::Host,
        Transport::WebRtc,
        Some(ghost_host),
    );
    server
        .database
        .add_player_to_room(&room_id, player_info(joiner, "joiner"))
        .await
        .expect("add joiner");
    for id in [&existing, &joiner] {
        server
            .connection_manager
            .assign_client_to_room(id, room_id)
            .await;
    }

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let room = finalized_room(&server, &room_id).await;
    let replans_before = server
        .metrics
        .session_replans_emitted
        .load(Ordering::Relaxed);
    let late_before = server
        .metrics
        .session_plans_late_join
        .load(Ordering::Relaxed);
    let plans_before = server.metrics.session_plans_emitted.load(Ordering::Relaxed);
    server
        .handle_active_session_late_join(&room, &joiner, &members)
        .await;

    // The authority (`existing`) qualifies and is healed in as host; its plan
    // is its host view of the star including the joiner.
    let existing_plan = match recv(&mut existing_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("existing expected the healed SessionPlan, got {other:?}"),
    };
    assert_eq!(existing_plan.topology, Topology::Host);
    assert_eq!(existing_plan.transport, Transport::WebRtc);
    assert_eq!(existing_plan.host, Some(existing), "authority healed in");
    assert_eq!(existing_plan.peers.len(), 1);
    assert_eq!(existing_plan.peers[0].player_id, joiner);
    assert!(
        !existing_plan.peers[0].initiate,
        "the healed host answers the joiner"
    );

    // The joiner's ONLY message is the same healed plan, tailored to it: it
    // offers to the healed host. No separate late-join plan, no NewPeer.
    let joiner_plan = match recv(&mut joiner_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("joiner expected the healed SessionPlan, got {other:?}"),
    };
    assert_eq!(joiner_plan.host, Some(existing));
    assert_eq!(joiner_plan.peers.len(), 1);
    assert_eq!(joiner_plan.peers[0].player_id, existing);
    assert!(
        joiner_plan.peers[0].initiate,
        "the joiner offers to the host"
    );

    // Exactly one plan each and zero NewPeer: the heal replaces the normal
    // joiner-plan + NewPeer emission.
    assert_silent(&mut existing_rx).await;
    assert_silent(&mut joiner_rx).await;

    assert_eq!(
        server.active_session_plan(&room_id),
        Some(ActiveSessionPlan {
            topology: Topology::Host,
            transport: Transport::WebRtc,
            host: Some(existing),
        }),
        "the wedged entry is healed in place"
    );
    assert_eq!(
        server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        replans_before + 1,
        "the heal is one re-plan event"
    );
    assert_eq!(
        server
            .metrics
            .session_plans_late_join
            .load(Ordering::Relaxed),
        late_before,
        "a heal-served joiner is NOT counted as a late-join plan"
    );
    assert_eq!(
        server.metrics.session_plans_emitted.load(Ordering::Relaxed),
        plans_before,
        "the heal is not a finalize emission"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_heal_can_elect_the_joiner_when_it_is_the_only_candidate() {
    // Degenerate wedge (stored host: None) + a v2-only existing member: the
    // v3+webrtc JOINER is the only electable member, so the heal elects the
    // joiner itself. The joiner receives exactly ONE plan naming itself host;
    // the v2 member (who also holds authority, which must not outrank the
    // capability filter) receives nothing; one re-plan event, no late-join
    // count.
    let server = create_test_server_with_session(host_session_config()).await;
    let (legacy_owner, mut legacy_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    // legacy_owner stays on the default v2 / relay-only protocol (and is the
    // room's authority as its creator).
    server.set_client_protocol(&joiner, v3_webrtc_host());

    let room_id = create_db_room(&server, legacy_owner).await;
    store_active_plan(&server, room_id, Topology::Host, Transport::WebRtc, None);
    server
        .database
        .add_player_to_room(&room_id, player_info(joiner, "joiner"))
        .await
        .expect("add joiner");
    for id in [&legacy_owner, &joiner] {
        server
            .connection_manager
            .assign_client_to_room(id, room_id)
            .await;
    }

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let room = finalized_room(&server, &room_id).await;
    let replans_before = server
        .metrics
        .session_replans_emitted
        .load(Ordering::Relaxed);
    let late_before = server
        .metrics
        .session_plans_late_join
        .load(Ordering::Relaxed);
    server
        .handle_active_session_late_join(&room, &joiner, &members)
        .await;

    let joiner_plan = match recv(&mut joiner_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("joiner expected the healed SessionPlan, got {other:?}"),
    };
    assert_eq!(
        joiner_plan.host,
        Some(joiner),
        "the joiner is the only electable member and becomes the host"
    );
    assert_eq!(
        server.active_session_plan(&room_id).and_then(|p| p.host),
        Some(joiner)
    );
    assert_eq!(
        server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        replans_before + 1
    );
    assert_eq!(
        server
            .metrics
            .session_plans_late_join
            .load(Ordering::Relaxed),
        late_before,
        "a heal-served joiner is NOT counted as a late-join plan"
    );
    // The v2 member never observes a SessionPlan; the joiner got exactly one.
    assert_silent(&mut legacy_rx).await;
    assert_silent(&mut joiner_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_stored_host_direct_sends_plan_but_no_new_peer() {
    // A stored `host + direct` decision is non-relay (it received SessionPlans
    // at finalize), so a late joiner still gets its plan view — with an EMPTY
    // ICE list and Direct transport — but NOBODY gets a NewPeer: `NewPeer` is a
    // WebRTC-signaling control message and this session's transport is Direct.
    // Both members advertise the WebRTC transport, so the per-peer capability
    // gate passes — only the stored plan's *transport* gate suppresses pairing.
    let server = create_test_server_with_session(host_direct_session_config()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_webrtc_direct_host());
    server.set_client_protocol(&joiner, v3_webrtc_direct_host());

    let room_id = create_db_room(&server, host).await;
    store_active_plan(
        &server,
        room_id,
        Topology::Host,
        Transport::Direct,
        Some(host),
    );
    server
        .database
        .add_player_to_room(&room_id, player_info(joiner, "joiner"))
        .await
        .expect("add joiner");
    for id in [&host, &joiner] {
        server
            .connection_manager
            .assign_client_to_room(id, room_id)
            .await;
    }

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let room = finalized_room(&server, &room_id).await;
    server
        .handle_active_session_late_join(&room, &joiner, &members)
        .await;

    let joiner_plan = match recv(&mut joiner_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("joiner expected SessionPlan, got {other:?}"),
    };
    assert_eq!(joiner_plan.topology, Topology::Host);
    assert_eq!(joiner_plan.transport, Transport::Direct);
    assert_eq!(joiner_plan.host, Some(host));
    assert!(
        joiner_plan.ice_servers.is_empty(),
        "a non-WebRTC plan carries no ICE"
    );

    // host + direct ⇒ no WebRTC signaling ⇒ no NewPeer to anyone.
    assert_silent(&mut joiner_rx).await;
    assert_silent(&mut host_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_without_stored_plan_emits_nothing() {
    // A Finalized room with NO stored decision (the relay floor stores none, as
    // does any pre-v3 room) emits neither SessionPlan nor NewPeer — even though
    // both current members are v3+webrtc and a recompute would now fit mesh.
    // The running session is relay; it is sticky.
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
        .handle_active_session_late_join(&room, &joiner, &members)
        .await;

    assert_silent(&mut existing_rx).await;
    assert_silent(&mut joiner_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_after_relay_finalize_and_departure_emits_nothing() {
    // Problem-2 regression pin: A(v3+webrtc)+B(v2) finalize on a mesh-preferring
    // server => relay floor => NO stored decision. B leaves; C(v3+webrtc) joins
    // the still-Finalized, no-longer-full room. Before the stored-plan fix the
    // late-join path RECOMPUTED the ladder over {A, C} (now all-v3) and wrongly
    // emitted `NewPeer` for a room whose running session is relay and which
    // never received any SessionPlan. Now: nothing is emitted to anyone.
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (legacy, _legacy_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    // legacy stays on the default v2 / relay-only protocol.
    server.set_client_protocol(&joiner, v3_webrtc());

    let room_id = create_db_room_with_max(&server, alice, 2).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(legacy, "legacy"))
        .await
        .expect("add legacy");
    for id in [&alice, &legacy] {
        server
            .connection_manager
            .assign_client_to_room(id, room_id)
            .await;
    }

    // Finalize through the real emission path: the mixed room resolves to the
    // relay floor, so emit_session_plan stores NO ActiveSessionPlan.
    finalize_db_room(&server, &room_id, &[alice, legacy]).await;
    let finalized = crate::coordination::FinalizedRoom {
        game_name: "webrtc-game".to_string(),
        authority_player: None,
        members: server
            .database
            .get_room_players(&room_id)
            .await
            .expect("room players"),
    };
    server.emit_session_plan(&room_id, &finalized).await;
    assert!(
        server.active_session_plan(&room_id).is_none(),
        "a relay-resolved finalize must store no active session plan"
    );

    // B (v2) departs, reopening a seat in the Finalized room.
    server.leave_room(&legacy).await;
    match recv(&mut alice_rx).await.as_ref() {
        ServerMessage::PlayerLeft { player_id } => assert_eq!(*player_id, legacy),
        other => panic!("alice expected PlayerLeft, got {other:?}"),
    }

    // C (v3+webrtc) joins the still-Finalized room.
    server
        .database
        .add_player_to_room(&room_id, player_info(joiner, "joiner"))
        .await
        .expect("add joiner");
    server
        .connection_manager
        .assign_client_to_room(&joiner, room_id)
        .await;
    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let room = server
        .database
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup")
        .expect("room exists");
    assert_eq!(room.lobby_state, LobbyState::Finalized);
    server
        .handle_active_session_late_join(&room, &joiner, &members)
        .await;

    // The room's running session is relay: no SessionPlan, no NewPeer, to anyone.
    assert_silent(&mut alice_rx).await;
    assert_silent(&mut joiner_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn announce_webrtc_peer_to_members_skips_v2_members() {
    // Appendix K gating at the mesh-announcement primitive: a v2 (relay-only)
    // member receives no NewPeer. (The room-wide gate is covered by the
    // `late_join_*` tests; here we drive the primitive directly so a v2 member
    // can be present.) The joiner itself receives nothing either — its pairing
    // arrives in its SessionPlan, never via NewPeer.
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
    let decision = decision_for(&server, Topology::Mesh, Transport::WebRtc, None, &members);
    server
        .announce_webrtc_peer_to_members(&decision, &joiner)
        .await;

    // Only the WebRTC-capable existing member learns of the joiner.
    match recv(&mut webrtc_rx).await.as_ref() {
        ServerMessage::NewPeer { peer_id, .. } => assert_eq!(*peer_id, joiner),
        other => panic!("expected NewPeer, got {other:?}"),
    }

    // The legacy member is never told about the joiner, and the joiner side is
    // suppressed entirely (its pairing belongs to its SessionPlan).
    assert_silent(&mut legacy_rx).await;
    assert_silent(&mut joiner_rx).await;
    assert_silent(&mut webrtc_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_relay_only_v3_joiner_gets_plan_but_no_new_peer_fires() {
    // A v3 joiner that negotiated only the relay transport still receives its
    // SessionPlan view of the running session (the plan is v3-gated, not
    // webrtc-gated; the relay floor stays its data path) — but with an EMPTY
    // peer list: it must not be instructed to attempt WebRTC pairs that
    // `handle_signal` would reject. And NO NewPeer fires in either direction:
    // existing members must never be told to WebRTC-pair with a peer that
    // cannot signal.
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (existing, mut existing_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&existing, v3_webrtc());
    server.set_client_protocol(&joiner, v3_relay_only());

    let room_id = create_db_room(&server, existing).await;
    store_active_plan(&server, room_id, Topology::Mesh, Transport::WebRtc, None);
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
        .handle_active_session_late_join(&room, &joiner, &members)
        .await;

    // The joiner (v3) gets its plan view of the running mesh session — with an
    // empty peer list (capability-filtered: it has no P2P peers; the plan's
    // relay fallback is its data path).
    match recv(&mut joiner_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => {
            assert_eq!(plan.topology, Topology::Mesh);
            assert_eq!(plan.transport, Transport::WebRtc);
            assert!(
                plan.peers.is_empty(),
                "a relay-only joiner must not be told to attempt WebRTC pairs"
            );
            assert_eq!(plan.fallback, Transport::Relay);
        }
        other => panic!("v3 joiner expected SessionPlan, got {other:?}"),
    }

    // But the WebRTC NewPeer delta is suppressed: the joiner cannot signal.
    assert_silent(&mut existing_rx).await;
    assert_silent(&mut joiner_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_capable_joiner_plan_excludes_relay_only_member() {
    // The other side of the capability filter: a CAPABLE v3+webrtc joiner
    // late-joins an active mesh+webrtc session whose membership contains a
    // relay-only seat-filler. The joiner's plan must list ONLY the capable
    // existing member — never the relay-only one (those offers would be doomed
    // at `handle_signal` and burn signal budget). NewPeer behavior is the
    // already-pinned skip rule: only the capable existing member is announced
    // to; the relay-only member hears nothing.
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (existing, mut existing_rx) = register_client(&server).await;
    let (seat_filler, mut seat_filler_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&existing, v3_webrtc());
    server.set_client_protocol(&seat_filler, v3_relay_only());
    server.set_client_protocol(&joiner, v3_webrtc());

    let room_id = create_db_room(&server, existing).await;
    store_active_plan(&server, room_id, Topology::Mesh, Transport::WebRtc, None);
    for (id, name) in [(seat_filler, "seat-filler"), (joiner, "joiner")] {
        server
            .database
            .add_player_to_room(&room_id, player_info(id, name))
            .await
            .expect("add member");
    }
    for id in [&existing, &seat_filler, &joiner] {
        server
            .connection_manager
            .assign_client_to_room(id, room_id)
            .await;
    }

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let room = finalized_room(&server, &room_id).await;
    server
        .handle_active_session_late_join(&room, &joiner, &members)
        .await;

    // The joiner's plan pairs it with the capable member only.
    let joiner_plan = match recv(&mut joiner_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("joiner expected SessionPlan, got {other:?}"),
    };
    assert_eq!(joiner_plan.topology, Topology::Mesh);
    assert_eq!(joiner_plan.transport, Transport::WebRtc);
    assert_eq!(
        joiner_plan.peers.len(),
        1,
        "the relay-only seat-filler must be filtered from the joiner's peers"
    );
    assert_eq!(joiner_plan.peers[0].player_id, existing);
    assert_eq!(
        joiner_plan.peers[0].initiate,
        local_initiates(joiner, existing)
    );

    // The capable existing member gets the NewPeer delta; the relay-only
    // seat-filler hears nothing at all.
    match recv(&mut existing_rx).await.as_ref() {
        ServerMessage::NewPeer { peer_id, .. } => assert_eq!(*peer_id, joiner),
        other => panic!("existing expected NewPeer, got {other:?}"),
    }
    assert_silent(&mut seat_filler_rx).await;
    assert_silent(&mut existing_rx).await;
    assert_silent(&mut joiner_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_topology_incapable_v3_joiner_gets_plan_but_no_new_peer_fires() {
    // The full-predicate discriminator: a v3 joiner that negotiated the WebRTC
    // TRANSPORT but not the session's mesh TOPOLOGY (transports=[relay,webrtc],
    // topologies=[relay]) seat-fills an active mesh+webrtc session. It passes
    // the transport-only `supports_webrtc_signaling` gate, yet the plan filter
    // excludes it everywhere — so `NewPeer` must stay silent in BOTH
    // directions too (one rule: the server never instructs a pair its own plan
    // contract excludes). The joiner still receives its v3-gated SessionPlan
    // with an EMPTY peer list (the relay floor is its data path).
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (existing, mut existing_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&existing, v3_webrtc());
    server.set_client_protocol(&joiner, v3_webrtc_relay_topology_only());

    let room_id = create_db_room(&server, existing).await;
    store_active_plan(&server, room_id, Topology::Mesh, Transport::WebRtc, None);
    server
        .database
        .add_player_to_room(&room_id, player_info(joiner, "joiner"))
        .await
        .expect("add joiner");
    for id in [&existing, &joiner] {
        server
            .connection_manager
            .assign_client_to_room(id, room_id)
            .await;
    }

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let room = finalized_room(&server, &room_id).await;
    server
        .handle_active_session_late_join(&room, &joiner, &members)
        .await;

    // The joiner (v3) gets its plan view of the running mesh session — with an
    // empty peer list: it never negotiated the mesh topology, so every WebRTC
    // pair with it is outside the session contract.
    match recv(&mut joiner_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => {
            assert_eq!(plan.topology, Topology::Mesh);
            assert_eq!(plan.transport, Transport::WebRtc);
            assert!(
                plan.peers.is_empty(),
                "a topology-incapable joiner must not be told to attempt WebRTC pairs"
            );
            assert_eq!(plan.fallback, Transport::Relay);
        }
        other => panic!("v3 joiner expected SessionPlan, got {other:?}"),
    }

    // No NewPeer in either direction: the existing member is never told to
    // pair with a peer the plan excludes, and the joiner gets no NewPeer.
    assert_silent(&mut existing_rx).await;
    assert_silent(&mut joiner_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_capable_joiner_plan_excludes_topology_incapable_member() {
    // The other side of the full-predicate filter: a CAPABLE v3+webrtc+mesh
    // joiner late-joins an active mesh+webrtc session seating a
    // topology-incapable member (webrtc transport, relay-only topologies). The
    // joiner's plan lists ONLY the capable existing member; NewPeer fires only
    // between the capable pair; the topology-incapable member hears nothing
    // (before the gate unification it was wrongly NewPeer-announced, since it
    // passes the transport-only check).
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (existing, mut existing_rx) = register_client(&server).await;
    let (seat_filler, mut seat_filler_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&existing, v3_webrtc());
    server.set_client_protocol(&seat_filler, v3_webrtc_relay_topology_only());
    server.set_client_protocol(&joiner, v3_webrtc());

    let room_id = create_db_room(&server, existing).await;
    store_active_plan(&server, room_id, Topology::Mesh, Transport::WebRtc, None);
    for (id, name) in [(seat_filler, "seat-filler"), (joiner, "joiner")] {
        server
            .database
            .add_player_to_room(&room_id, player_info(id, name))
            .await
            .expect("add member");
    }
    for id in [&existing, &seat_filler, &joiner] {
        server
            .connection_manager
            .assign_client_to_room(id, room_id)
            .await;
    }

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let room = finalized_room(&server, &room_id).await;
    server
        .handle_active_session_late_join(&room, &joiner, &members)
        .await;

    // The joiner's plan pairs it with the capable member only.
    let joiner_plan = match recv(&mut joiner_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("joiner expected SessionPlan, got {other:?}"),
    };
    assert_eq!(
        joiner_plan.peers.len(),
        1,
        "the topology-incapable seat-filler must be filtered from the joiner's peers"
    );
    assert_eq!(joiner_plan.peers[0].player_id, existing);

    // The capable existing member gets the NewPeer delta; the
    // topology-incapable seat-filler hears nothing at all.
    match recv(&mut existing_rx).await.as_ref() {
        ServerMessage::NewPeer { peer_id, .. } => assert_eq!(*peer_id, joiner),
        other => panic!("existing expected NewPeer, got {other:?}"),
    }
    assert_silent(&mut seat_filler_rx).await;
    assert_silent(&mut existing_rx).await;
    assert_silent(&mut joiner_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_star_topology_incapable_client_joiner_gets_plan_but_no_new_peer() {
    // Star variant of the full-predicate gate, client-join path: a v3 joiner
    // with the WebRTC transport but no `host` topology seat-fills an active
    // host+webrtc session whose stored host is present. The joiner gets its
    // plan with EMPTY peers (it must not be told to offer to the host); the
    // host gets NO NewPeer about it (before the gate unification it was
    // wrongly told to answer a transport-capable joiner); other clients stay
    // silent as always.
    let server = create_test_server_with_session(host_session_config()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (other_client, mut other_client_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_webrtc_host());
    server.set_client_protocol(&other_client, v3_webrtc_host());
    server.set_client_protocol(&joiner, v3_webrtc_relay_topology_only());

    let room_id = create_db_room(&server, host).await;
    store_active_plan(
        &server,
        room_id,
        Topology::Host,
        Transport::WebRtc,
        Some(host),
    );
    for (id, name) in [(other_client, "other"), (joiner, "joiner")] {
        server
            .database
            .add_player_to_room(&room_id, player_info(id, name))
            .await
            .expect("add member");
    }
    for id in [&host, &other_client, &joiner] {
        server
            .connection_manager
            .assign_client_to_room(id, room_id)
            .await;
    }

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let room = finalized_room(&server, &room_id).await;
    let replans_before = server
        .metrics
        .session_replans_emitted
        .load(Ordering::Relaxed);
    server
        .handle_active_session_late_join(&room, &joiner, &members)
        .await;

    // The stored host is present AND capable: no heal fires.
    assert_eq!(
        server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        replans_before,
        "a capable, present stored host must not trigger the self-heal"
    );

    // The joiner's plan: star view with the informational host, but no peers —
    // it cannot run a star session.
    let joiner_plan = match recv(&mut joiner_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("joiner expected SessionPlan, got {other:?}"),
    };
    assert_eq!(joiner_plan.topology, Topology::Host);
    assert_eq!(joiner_plan.transport, Transport::WebRtc);
    assert_eq!(joiner_plan.host, Some(host), "host stays informational");
    assert!(
        joiner_plan.peers.is_empty(),
        "a topology-incapable client must not be told to offer to the host"
    );

    // NO NewPeer to the host (or anyone): the joiner cannot run the session.
    assert_silent(&mut host_rx).await;
    assert_silent(&mut other_client_rx).await;
    assert_silent(&mut joiner_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_star_host_rejoin_announces_only_to_session_capable_clients() {
    // Star variant, host-(re)join path: the stored host rejoins a room seating
    // one capable client and one topology-incapable seat-filler (webrtc
    // transport, relay-only topologies). Only the capable client is told to
    // offer to the host; the seat-filler hears nothing (it passes the
    // transport-only gate, so this pins the per-member full predicate inside
    // the star announcement), and the host's own plan lists only the capable
    // client.
    let server = create_test_server_with_session(host_session_config()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (capable_client, mut capable_client_rx) = register_client(&server).await;
    let (seat_filler, mut seat_filler_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_webrtc_host());
    server.set_client_protocol(&capable_client, v3_webrtc_host());
    server.set_client_protocol(&seat_filler, v3_webrtc_relay_topology_only());

    let room_id = create_db_room(&server, host).await;
    store_active_plan(
        &server,
        room_id,
        Topology::Host,
        Transport::WebRtc,
        Some(host),
    );
    for (id, name) in [(capable_client, "capable"), (seat_filler, "seat-filler")] {
        server
            .database
            .add_player_to_room(&room_id, player_info(id, name))
            .await
            .expect("add member");
    }
    for id in [&host, &capable_client, &seat_filler] {
        server
            .connection_manager
            .assign_client_to_room(id, room_id)
            .await;
    }

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let room = finalized_room(&server, &room_id).await;
    server
        .handle_active_session_late_join(&room, &host, &members)
        .await;

    // The rejoining host's plan answers only the capable client.
    let host_plan = match recv(&mut host_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("host expected SessionPlan, got {other:?}"),
    };
    assert_eq!(host_plan.host, Some(host));
    assert_eq!(
        host_plan.peers.len(),
        1,
        "the host's star must exclude the topology-incapable seat-filler"
    );
    assert_eq!(host_plan.peers[0].player_id, capable_client);
    assert!(!host_plan.peers[0].initiate, "the host answers");

    // Only the capable client is told to offer to the rejoined host.
    match recv(&mut capable_client_rx).await.as_ref() {
        ServerMessage::NewPeer {
            peer_id,
            you_initiate,
        } => {
            assert_eq!(*peer_id, host);
            assert!(*you_initiate, "clients offer to the host");
        }
        other => panic!("capable client expected NewPeer(host, true), got {other:?}"),
    }

    // The topology-incapable seat-filler hears nothing.
    assert_silent(&mut seat_filler_rx).await;
    assert_silent(&mut capable_client_rx).await;
    assert_silent(&mut host_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_heal_with_topology_incapable_joiner_still_replans() {
    // Heal reachability: the heal is about the ROOM, not the joiner. A
    // topology-incapable v3 joiner (webrtc transport, relay-only topologies)
    // arriving into a host-missing room must still trigger the self-heal
    // re-plan: the capable existing member is healed in as host and gets its
    // fresh plan, and the joiner — served by the heal, not a late-join plan —
    // gets the same healed plan with EMPTY peers. No NewPeer fires anywhere;
    // one re-plan event; the late-join counter does not move.
    let server = create_test_server_with_session(host_session_config()).await;
    let (existing, mut existing_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&existing, v3_webrtc_host());
    server.set_client_protocol(&joiner, v3_webrtc_relay_topology_only());

    let room_id = create_db_room(&server, existing).await;
    let ghost_host = PlayerId::new_v4();
    store_active_plan(
        &server,
        room_id,
        Topology::Host,
        Transport::WebRtc,
        Some(ghost_host),
    );
    server
        .database
        .add_player_to_room(&room_id, player_info(joiner, "joiner"))
        .await
        .expect("add joiner");
    for id in [&existing, &joiner] {
        server
            .connection_manager
            .assign_client_to_room(id, room_id)
            .await;
    }

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let room = finalized_room(&server, &room_id).await;
    let replans_before = server
        .metrics
        .session_replans_emitted
        .load(Ordering::Relaxed);
    let late_before = server
        .metrics
        .session_plans_late_join
        .load(Ordering::Relaxed);
    server
        .handle_active_session_late_join(&room, &joiner, &members)
        .await;

    // The capable member is healed in as host; its star is empty (its only
    // would-be client cannot run the session).
    let existing_plan = match recv(&mut existing_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("existing expected the healed SessionPlan, got {other:?}"),
    };
    assert_eq!(
        existing_plan.host,
        Some(existing),
        "capable member healed in"
    );
    assert!(
        existing_plan.peers.is_empty(),
        "the healed host must not be told to pair with the incapable joiner"
    );

    // The heal delivers the joiner (v3) its plan too — empty peers, relay
    // fallback as its data path — and nothing else.
    let joiner_plan = match recv(&mut joiner_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("joiner expected the healed SessionPlan, got {other:?}"),
    };
    assert_eq!(joiner_plan.host, Some(existing));
    assert!(
        joiner_plan.peers.is_empty(),
        "a topology-incapable joiner gets the healed plan with no peers"
    );
    assert_eq!(joiner_plan.fallback, Transport::Relay);

    assert_eq!(
        server.active_session_plan(&room_id),
        Some(ActiveSessionPlan {
            topology: Topology::Host,
            transport: Transport::WebRtc,
            host: Some(existing),
        }),
        "the wedged entry is healed in place"
    );
    assert_eq!(
        server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        replans_before + 1,
        "an incapable joiner still triggers the heal (the heal is about the room)"
    );
    assert_eq!(
        server
            .metrics
            .session_plans_late_join
            .load(Ordering::Relaxed),
        late_before,
        "a heal-served joiner is NOT counted as a late-join plan"
    );

    // No NewPeer to anyone in the heal case (already pinned for capable
    // joiners; holds for incapable ones too).
    assert_silent(&mut existing_rx).await;
    assert_silent(&mut joiner_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_v2_joiner_gets_nothing() {
    // Appendix K: a pure v2 joiner into an active mesh+webrtc session receives
    // neither a SessionPlan nor a NewPeer, and existing members are not told to
    // pair with it.
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (existing, mut existing_rx) = register_client(&server).await;
    let (legacy_joiner, mut legacy_rx) = register_client(&server).await;
    server.set_client_protocol(&existing, v3_webrtc());
    server.set_client_protocol(&legacy_joiner, v2_with_webrtc_transport());

    let room_id = create_db_room(&server, existing).await;
    store_active_plan(&server, room_id, Topology::Mesh, Transport::WebRtc, None);
    server
        .database
        .add_player_to_room(&room_id, player_info(legacy_joiner, "legacy"))
        .await
        .expect("add legacy joiner");
    for id in [&existing, &legacy_joiner] {
        server
            .connection_manager
            .assign_client_to_room(id, room_id)
            .await;
    }

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let room = finalized_room(&server, &room_id).await;
    server
        .handle_active_session_late_join(&room, &legacy_joiner, &members)
        .await;

    assert_silent(&mut existing_rx).await;
    assert_silent(&mut legacy_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_counts_plan_and_turn_credentials() {
    // Metrics: a late-join plan counts session_plans_late_join once and the TURN
    // credentials it mints via the shared turn_credentials_issued counter, but
    // never touches session_plans_emitted (finalize-only) or
    // session_replans_emitted (departure-only).
    let turn = TurnConfig {
        enabled: true,
        static_auth_secret: "super-secret".to_string(),
        urls: vec![TURN_URL.to_string()],
        stun_urls: vec![TURN_STUN_URL.to_string()],
        credential_ttl_secs: TURN_CREDENTIAL_TTL_SECS,
    };
    let server = create_test_server_with_session_and_turn(mesh_session_config(), turn).await;
    let (existing, mut existing_rx) = register_client(&server).await;
    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&existing, v3_webrtc());
    server.set_client_protocol(&joiner, v3_webrtc());

    let room_id = create_db_room(&server, existing).await;
    store_active_plan(&server, room_id, Topology::Mesh, Transport::WebRtc, None);
    server
        .database
        .add_player_to_room(&room_id, player_info(joiner, "joiner"))
        .await
        .expect("add joiner");
    for id in [&existing, &joiner] {
        server
            .connection_manager
            .assign_client_to_room(id, room_id)
            .await;
    }

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let room = finalized_room(&server, &room_id).await;

    let plans_before = server.metrics.session_plans_emitted.load(Ordering::Relaxed);
    let replans_before = server
        .metrics
        .session_replans_emitted
        .load(Ordering::Relaxed);
    let late_before = server
        .metrics
        .session_plans_late_join
        .load(Ordering::Relaxed);
    let creds_before = server
        .metrics
        .turn_credentials_issued
        .load(Ordering::Relaxed);

    server
        .handle_active_session_late_join(&room, &joiner, &members)
        .await;

    // The joiner's plan carries a freshly minted TURN credential for ITS id.
    let plan = match recv(&mut joiner_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("joiner expected SessionPlan, got {other:?}"),
    };
    let turn_entry = plan
        .ice_servers
        .iter()
        .find(|server| server.username.is_some())
        .expect("late-join WebRTC plan must mint a TURN credential");
    let username = turn_entry.username.clone().expect("username present");
    assert!(
        username.ends_with(&joiner.to_string()),
        "the minted credential embeds the joiner's own id"
    );
    // Existing member got its NewPeer delta (drained so the channel stays clean).
    match recv(&mut existing_rx).await.as_ref() {
        ServerMessage::NewPeer { peer_id, .. } => assert_eq!(*peer_id, joiner),
        other => panic!("existing expected NewPeer, got {other:?}"),
    }

    assert_eq!(
        server
            .metrics
            .session_plans_late_join
            .load(Ordering::Relaxed),
        late_before + 1,
        "one late-join plan delivered => counted once"
    );
    assert_eq!(
        server
            .metrics
            .turn_credentials_issued
            .load(Ordering::Relaxed),
        creds_before + 1,
        "the joiner's minted TURN credential is counted"
    );
    assert_eq!(
        server.metrics.session_plans_emitted.load(Ordering::Relaxed),
        plans_before,
        "late-join must not count as a finalize emission"
    );
    assert_eq!(
        server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        replans_before,
        "late-join must not count as a departure re-plan"
    );
}

// ---------------------------------------------------------------------------
// Pairing primitives (direct).
// ---------------------------------------------------------------------------

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn announce_webrtc_peer_to_members_notifies_existing_members_only() {
    // The mesh primitive announces the joiner to every other webrtc member with
    // the glare-correct flag — and sends NOTHING to the joiner itself (its
    // pairing is delivered in its SessionPlan).
    let server = create_test_server().await;
    let (peer, mut peer_rx) = register_client(&server).await;
    let (other, mut other_rx) = register_client(&server).await;
    server.set_client_protocol(&peer, v3_webrtc());
    server.set_client_protocol(&other, v3_webrtc());

    let members = vec![player_info(peer, "peer"), player_info(other, "other")];
    let decision = decision_for(&server, Topology::Mesh, Transport::WebRtc, None, &members);
    server
        .announce_webrtc_peer_to_members(&decision, &peer)
        .await;

    match recv(&mut other_rx).await.as_ref() {
        ServerMessage::NewPeer {
            peer_id,
            you_initiate,
        } => {
            assert_eq!(*peer_id, peer);
            assert_eq!(
                *you_initiate,
                local_initiates(other, peer),
                "the existing member's flag follows the glare rule"
            );
        }
        other => panic!("other expected NewPeer, got {other:?}"),
    }

    // The joiner side is suppressed.
    assert_silent(&mut peer_rx).await;
    assert_silent(&mut other_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn announce_webrtc_peer_in_star_client_joiner_notifies_host_only() {
    // The star primitive for a client joiner: only the host is told (it answers,
    // you_initiate=false). The joiner gets nothing (its offer-to-host
    // instruction is in its SessionPlan); other clients never hear about it.
    let server = create_test_server().await;
    let (host, mut host_rx) = register_client(&server).await;
    let (client, mut client_rx) = register_client(&server).await;
    let (other_client, mut other_client_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_webrtc_host());
    server.set_client_protocol(&client, v3_webrtc_host());
    server.set_client_protocol(&other_client, v3_webrtc_host());

    let members = vec![
        player_info(host, "host"),
        player_info(client, "client"),
        player_info(other_client, "other"),
    ];
    let decision = decision_for(
        &server,
        Topology::Host,
        Transport::WebRtc,
        Some(host),
        &members,
    );
    server
        .announce_webrtc_peer_in_star(&decision, &client, host)
        .await;

    // Host answers the client.
    match recv(&mut host_rx).await.as_ref() {
        ServerMessage::NewPeer {
            peer_id,
            you_initiate,
        } => {
            assert_eq!(*peer_id, client);
            assert!(!*you_initiate, "the host answers (never offers in a star)");
        }
        other => panic!("host expected NewPeer(client, false), got {other:?}"),
    }

    // The joiner side is suppressed; other clients are never told (no client ⇄
    // client edges in a star).
    assert_silent(&mut client_rx).await;
    assert_silent(&mut other_client_rx).await;
    assert_silent(&mut host_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn announce_webrtc_peer_in_star_host_joiner_notifies_every_client() {
    // When the host itself (re)joins, every webrtc client is told to offer to
    // it; the (joining) host receives nothing — its answer-everyone view is in
    // its SessionPlan.
    let server = create_test_server().await;
    let (host, mut host_rx) = register_client(&server).await;
    let (client_a, mut client_a_rx) = register_client(&server).await;
    let (client_b, mut client_b_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_webrtc_host());
    server.set_client_protocol(&client_a, v3_webrtc_host());
    server.set_client_protocol(&client_b, v3_webrtc_host());

    let members = vec![
        player_info(host, "host"),
        player_info(client_a, "a"),
        player_info(client_b, "b"),
    ];
    let decision = decision_for(
        &server,
        Topology::Host,
        Transport::WebRtc,
        Some(host),
        &members,
    );
    server
        .announce_webrtc_peer_in_star(&decision, &host, host)
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

    // The joiner (host) side is suppressed.
    assert_silent(&mut host_rx).await;
    assert_silent(&mut client_a_rx).await;
    assert_silent(&mut client_b_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reconnect_restores_room_membership_plan_and_webrtc_pairing() {
    // Reconnect re-entry consults the stored ActiveSessionPlan: the reconnector
    // receives `Reconnected` then a fresh tailored `SessionPlan` (fresh ICE —
    // its original TURN credentials may have expired) and NO NewPeer; the
    // existing member receives `PlayerReconnected` then the NewPeer delta.
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (existing, mut existing_rx) = register_client(&server).await;
    let (reconnecting, _old_rx) = register_client(&server).await;
    let (current, mut current_rx) = register_client(&server).await;
    server.set_client_protocol(&existing, v3_webrtc());
    server.set_client_protocol(&reconnecting, v3_webrtc());
    server.set_client_protocol(&current, v3_webrtc());

    let room_id = create_db_room_with_max(&server, existing, 2).await;
    // The room runs an active mesh+webrtc session (as emit_session_plan would
    // have stored at finalize).
    store_active_plan(&server, room_id, Topology::Mesh, Transport::WebRtc, None);
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

    // Finalize the (now full) room so the post-reconnect re-entry engages.
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

    // The reconnector receives a fresh SessionPlan for the running session.
    let reconnecting_flag = match recv(&mut current_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => {
            assert_eq!(plan.topology, Topology::Mesh);
            assert_eq!(plan.transport, Transport::WebRtc);
            assert_eq!(plan.peers.len(), 1);
            assert_eq!(plan.peers[0].player_id, existing);
            assert_static_then_default_stun_ice(&plan.ice_servers);
            plan.peers[0].initiate
        }
        other => panic!("reconnecting expected SessionPlan after reconnect, got {other:?}"),
    };
    // The existing member receives the NewPeer delta (antisymmetric flags).
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
    assert_ne!(existing_flag, reconnecting_flag);

    // No NewPeer to the reconnector; no SessionPlan to the existing member.
    assert_silent(&mut current_rx).await;
    assert_silent(&mut existing_rx).await;

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

// ---------------------------------------------------------------------------
// signals_relayed metric (P5).
// ---------------------------------------------------------------------------

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn handle_signal_increments_signals_relayed_on_accepted_dispatch() {
    let server = create_test_server().await;
    let (alice, _alice_rx) = register_client(&server).await;
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

    assert_eq!(
        server.metrics.signals_relayed.load(Ordering::Relaxed),
        0,
        "no signals relayed before any handle_signal call"
    );

    server
        .handle_signal(&alice, bob, json!({ "Offer": "x" }))
        .await;

    // Sanity: the best-effort dispatch reached this receiver in the available-channel case.
    match recv(&mut bob_rx).await.as_ref() {
        ServerMessage::Signal { from, .. } => assert_eq!(*from, alice),
        other => panic!("expected Signal, got {other:?}"),
    }
    assert_eq!(
        server.metrics.signals_relayed.load(Ordering::Relaxed),
        1,
        "an accepted dispatch must increment signals_relayed exactly once"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn handle_signal_counts_valid_dispatch_when_receiver_is_closed() {
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, bob_rx) = register_client(&server).await;
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

    drop(bob_rx);

    server
        .handle_signal(&alice, bob, json!({ "Offer": "x" }))
        .await;

    assert_silent(&mut alice_rx).await;
    assert_eq!(
        server.metrics.signals_relayed.load(Ordering::Relaxed),
        1,
        "signals_relayed counts accepted best-effort dispatch, not receiver delivery"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn handle_signal_does_not_count_rejected_cross_room_signal() {
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    // Alice and Bob are in DIFFERENT rooms => cross-room signal is rejected.
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

    // Alice gets a rejection error; Bob never receives the signal.
    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::CrossRoomSignal));
    assert_silent(&mut bob_rx).await;

    assert_eq!(
        server.metrics.signals_relayed.load(Ordering::Relaxed),
        0,
        "a rejected cross-room signal must NOT count as relayed"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn handle_signal_does_not_count_rate_limited_signal() {
    // A 0-budget signal limiter rejects every relay attempt; none may count.
    let server = create_test_server_with_signals(0).await;
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

    server
        .handle_signal(&alice, bob, json!({ "Offer": "x" }))
        .await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalRateLimited));
    assert_silent(&mut bob_rx).await;
    assert_eq!(
        server.metrics.signals_relayed.load(Ordering::Relaxed),
        0,
        "a rate-limited signal must NOT count as relayed"
    );
}

// ---------------------------------------------------------------------------
// TransportStatus handler (P5): per-connection state + p2p/relay metrics, v3 gating.
// ---------------------------------------------------------------------------

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_webrtc_connected_records_p2p_and_state() {
    let server = create_test_server().await;
    let (alice, _alice_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: true,
            },
        )
        .await;

    assert_eq!(
        server.client_transport_status(&alice),
        Some((Transport::WebRtc, true)),
        "the per-connection transport status must be recorded"
    );
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        1,
        "webrtc + connected must record one p2p_established"
    );
    assert_eq!(
        server.metrics.relay_fallback.load(Ordering::Relaxed),
        0,
        "a connected p2p report must not count as relay_fallback"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_direct_connected_records_p2p() {
    let server = create_test_server().await;
    let (alice, _alice_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc_direct_host());

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::Direct,
                connected: true,
            },
        )
        .await;

    assert_eq!(
        server.client_transport_status(&alice),
        Some((Transport::Direct, true))
    );
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        1,
        "direct + connected is a P2P establishment"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_disconnected_records_relay_fallback() {
    let server = create_test_server().await;
    let (alice, _alice_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: false,
            },
        )
        .await;

    assert_eq!(
        server.client_transport_status(&alice),
        Some((Transport::WebRtc, false))
    );
    assert_eq!(
        server.metrics.relay_fallback.load(Ordering::Relaxed),
        1,
        "connected=false must record one relay_fallback"
    );
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        0,
        "a fallback report must not count as p2p_established"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_relay_connected_moves_no_counter_but_records_state() {
    // `connected: true` with `transport: relay` means "still on the floor": it is
    // neither a P2P establishment nor a fallback, so it moves no metric — only the
    // per-connection state is updated.
    let server = create_test_server().await;
    let (alice, _alice_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::Relay,
                connected: true,
            },
        )
        .await;

    assert_eq!(
        server.client_transport_status(&alice),
        Some((Transport::Relay, true)),
        "the report is still recorded as per-connection state"
    );
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        0,
        "relay + connected is not a P2P establishment"
    );
    assert_eq!(
        server.metrics.relay_fallback.load(Ordering::Relaxed),
        0,
        "relay + connected is not a fallback event"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_from_non_v3_client_is_ignored() {
    // A v2 client can never legitimately send TransportStatus; the report must be
    // dropped — no per-connection state, no metric movement (defense-in-depth).
    let server = create_test_server().await;
    let (legacy, mut legacy_rx) = register_client(&server).await;
    server.set_client_protocol(&legacy, v2_with_webrtc_transport());

    server
        .handle_client_message(
            &legacy,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: true,
            },
        )
        .await;

    // The ignored report must also leak no message back to the v2 client.
    assert_silent(&mut legacy_rx).await;

    assert_eq!(
        server.client_transport_status(&legacy),
        None,
        "a non-v3 client's TransportStatus must NOT update per-connection state"
    );
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        0,
        "a non-v3 client's TransportStatus must not move p2p_established"
    );
    assert_eq!(
        server.metrics.relay_fallback.load(Ordering::Relaxed),
        0,
        "a non-v3 client's TransportStatus must not move relay_fallback"
    );
}

// ---------------------------------------------------------------------------
// PeerTransportStatus fan-out (P5 refinement): an accepted TransportStatus
// state change is fanned out to the sender's current room — v3 recipients only,
// sender excluded, duplicates never re-fan-out, no room ⇒ no fan-out.
// ---------------------------------------------------------------------------

/// Assert the next message is `PeerTransportStatus` with the given contents.
async fn expect_peer_transport_status(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    expected_peer: PlayerId,
    expected_transport: Transport,
    expected_connected: bool,
) {
    match recv(receiver).await.as_ref() {
        ServerMessage::PeerTransportStatus {
            peer_id,
            transport,
            connected,
        } => {
            assert_eq!(*peer_id, expected_peer, "fan-out must name the reporter");
            assert_eq!(*transport, expected_transport);
            assert_eq!(*connected, expected_connected);
        }
        other => panic!("expected PeerTransportStatus, got {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_change_fans_out_to_v3_room_peers_only() {
    // Alice (v3 + webrtc) reports a state change in a room with Bob (v3
    // RELAY-ONLY — must still receive: the fan-out is deliberately NOT gated on
    // the recipient's transport capabilities, unlike the session predicate) and
    // Carol (v2 — must NEVER receive a v3-only message, Appendix K). The
    // reporter itself hears nothing.
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    let (carol, mut carol_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_relay_only());
    server.set_client_protocol(&carol, v2_with_webrtc_transport());

    let room_id = create_db_room(&server, alice).await;
    for (id, name) in [(bob, "bob"), (carol, "carol")] {
        server
            .database
            .add_player_to_room(&room_id, player_info(id, name))
            .await
            .expect("add member");
    }
    for id in [alice, bob, carol] {
        server
            .connection_manager
            .assign_client_to_room(&id, room_id)
            .await;
    }

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: true,
            },
        )
        .await;

    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, true).await;
    assert_silent(&mut bob_rx).await;
    // The v2 member observes nothing, and the sender is excluded.
    assert_silent(&mut carol_rx).await;
    assert_silent(&mut alice_rx).await;
    assert_eq!(
        server
            .metrics
            .transport_status_fanout
            .load(Ordering::Relaxed),
        1,
        "one fan-out EVENT (not per recipient)"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn duplicate_transport_status_does_not_refan_out() {
    let server = create_test_server().await;
    let (alice, _alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = create_db_room(&server, alice).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(bob, "bob"))
        .await
        .expect("add bob");
    for id in [alice, bob] {
        server
            .connection_manager
            .assign_client_to_room(&id, room_id)
            .await;
    }

    let report = ClientMessage::TransportStatus {
        transport: Transport::WebRtc,
        connected: true,
    };
    // First report IS a state change and fans out…
    server.handle_client_message(&alice, report.clone()).await;
    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, true).await;

    // …the byte-identical duplicate is dropped at the dedup gate: no fan-out,
    // no counter movement.
    server.handle_client_message(&alice, report).await;
    assert_silent(&mut bob_rx).await;
    assert_eq!(
        server
            .metrics
            .transport_status_fanout
            .load(Ordering::Relaxed),
        1,
        "a duplicate report must not re-fan-out"
    );
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        1,
        "the duplicate must not inflate p2p_established either"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_flap_fans_out_each_transition() {
    // true ⇒ false ⇒ true: every report is a real transition, so the peer sees
    // all three states in order (eventually-consistent peer view of a flapping
    // data path).
    let server = create_test_server().await;
    let (alice, _alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = create_db_room(&server, alice).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(bob, "bob"))
        .await
        .expect("add bob");
    for id in [alice, bob] {
        server
            .connection_manager
            .assign_client_to_room(&id, room_id)
            .await;
    }

    for connected in [true, false, true] {
        server
            .handle_client_message(
                &alice,
                ClientMessage::TransportStatus {
                    transport: Transport::WebRtc,
                    connected,
                },
            )
            .await;
    }

    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, true).await;
    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, false).await;
    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, true).await;
    assert_silent(&mut bob_rx).await;
    assert_eq!(
        server
            .metrics
            .transport_status_fanout
            .load(Ordering::Relaxed),
        3,
        "each real transition is one fan-out event"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_fanout_is_bounded_by_signal_budget() {
    // `TransportStatus` is v3 WebRTC control-plane traffic, and an accepted state
    // change triggers a 1→N `PeerTransportStatus` fan-out to the room. A client
    // that alternates `connected` to force a `Changed` on every frame (defeating
    // the dedup gate) must not be able to use the tiny message as an unbounded
    // room amplifier: the accepted-change path consumes the same per-connection
    // budget as `Signal` (`max_signals`). Over-budget changes are dropped
    // SILENTLY (no error frame — `TransportStatus` is informational), but the
    // per-connection state is still recorded. Budget = 2 here.
    let server = create_test_server_with_signals(2).await;
    let (alice, _alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = create_db_room(&server, alice).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(bob, "bob"))
        .await
        .expect("add bob");
    for id in [alice, bob] {
        server
            .connection_manager
            .assign_client_to_room(&id, room_id)
            .await;
    }

    // Five real transitions, but only two fit the budget.
    for connected in [true, false, true, false, true] {
        server
            .handle_client_message(
                &alice,
                ClientMessage::TransportStatus {
                    transport: Transport::WebRtc,
                    connected,
                },
            )
            .await;
    }

    // Bob sees exactly the two budgeted transitions, in order, then nothing.
    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, true).await;
    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, false).await;
    assert_silent(&mut bob_rx).await;
    assert_eq!(
        server
            .metrics
            .transport_status_fanout
            .load(Ordering::Relaxed),
        2,
        "fan-out events are capped at the per-connection control-plane budget"
    );
    // Only the 1→N fan-out is budget-bounded. The O(1) local bookkeeping is NOT:
    // every accepted transition still updates the per-connection state and the
    // p2p/relay observability counters, so the budget cannot distort metrics.
    // The five transitions are [true, false, true, false, true] over WebRtc:
    // three `true` ⇒ p2p_established += 3, two `false` ⇒ relay_fallback += 2.
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        3,
        "every accepted P2P-up transition is counted, even when its fan-out is dropped"
    );
    assert_eq!(
        server.metrics.relay_fallback.load(Ordering::Relaxed),
        2,
        "every accepted fallback transition is counted, even when its fan-out is dropped"
    );
    // The latest reported state is still recorded even though its fan-out was
    // dropped (per-connection truth is never rate-limited).
    assert_eq!(
        server.client_transport_status(&alice),
        Some((Transport::WebRtc, true)),
        "the connection's own transport state tracks the last report regardless of fan-out budget"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn over_budget_transport_status_does_not_load_room_members() {
    // Once the sender is already over the fan-out budget, the handler should
    // drop the informational peer notice before taking the fallible/O(room)
    // membership snapshot. The local state/metrics still update because they
    // are O(1) bookkeeping, not fan-out work.
    let server = create_test_server_with_signals(1).await;
    let (alice, _alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = create_db_room(&server, alice).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(bob, "bob"))
        .await
        .expect("add bob");
    for id in [alice, bob] {
        server
            .connection_manager
            .assign_client_to_room(&id, room_id)
            .await;
    }

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: true,
            },
        )
        .await;
    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, true).await;
    assert_eq!(valid_signal_budget_used(&server, &alice).await, 1);

    let db = server
        .database()
        .as_any()
        .downcast_ref::<InMemoryDatabase>()
        .expect("test server uses in-memory database");
    let member_lookups_after_budgeted_fanout = db.get_room_players_calls_for_test();
    db.fail_get_room_players_for_test(true);

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: false,
            },
        )
        .await;

    assert_silent(&mut bob_rx).await;
    assert_eq!(
        db.get_room_players_calls_for_test(),
        member_lookups_after_budgeted_fanout,
        "over-budget fan-out must be dropped before loading room members"
    );
    assert_eq!(
        valid_signal_budget_used(&server, &alice).await,
        1,
        "over-budget preflight must not consume an additional slot"
    );
    assert_eq!(
        server.client_transport_status(&alice),
        Some((Transport::WebRtc, false)),
        "local transport truth still tracks the over-budget report"
    );
    assert_eq!(
        server.metrics.relay_fallback.load(Ordering::Relaxed),
        1,
        "over-budget reports still update O(1) observability"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_without_room_records_state_but_fans_out_nothing() {
    // A room-less reporter still gets its per-connection state recorded (the
    // pre-fan-out behavior is preserved), but there is no room to notify.
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    // Bob sits in a room alice is NOT in — he must hear nothing.
    let room_id = create_db_room(&server, bob).await;
    server
        .connection_manager
        .assign_client_to_room(&bob, room_id)
        .await;

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: true,
            },
        )
        .await;

    assert_eq!(
        server.client_transport_status(&alice),
        Some((Transport::WebRtc, true)),
        "the state is still recorded for a room-less reporter"
    );
    assert_silent(&mut bob_rx).await;
    assert_silent(&mut alice_rx).await;
    assert_eq!(
        server
            .metrics
            .transport_status_fanout
            .load(Ordering::Relaxed),
        0,
        "no room ⇒ no fan-out event"
    );
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        1,
        "the accepted report still moves p2p_established"
    );
    // The fan-out rate-limit gate sits AFTER the no-room early return, so a
    // room-less reporter spends no signal budget on a fan-out that can't happen.
    assert_eq!(
        valid_signal_budget_used(&server, &alice).await,
        0,
        "a room-less TransportStatus must not consume the sender's signal budget"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_room_member_lookup_failure_does_not_consume_signal_budget() {
    // Regression for the budget-before-membership bug: if the room snapshot
    // cannot be loaded, the accepted local state and observability counters are
    // kept, but no fan-out can be attempted, so the sender's valid-signal
    // budget must remain available for the next real fan-out.
    let server = create_test_server_with_signals(1).await;
    let (alice, _alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = create_db_room(&server, alice).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(bob, "bob"))
        .await
        .expect("add bob");
    for id in [alice, bob] {
        server
            .connection_manager
            .assign_client_to_room(&id, room_id)
            .await;
    }

    let db = server
        .database()
        .as_any()
        .downcast_ref::<InMemoryDatabase>()
        .expect("test server uses in-memory database");
    db.fail_get_room_players_for_test(true);

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: true,
            },
        )
        .await;

    assert_silent(&mut bob_rx).await;
    assert_eq!(
        valid_signal_budget_used(&server, &alice).await,
        0,
        "failed membership lookup must not consume the valid-signal budget"
    );
    assert_eq!(
        server
            .metrics
            .transport_status_fanout
            .load(Ordering::Relaxed),
        0,
        "failed membership lookup is not a fan-out event"
    );
    assert_eq!(
        server.client_transport_status(&alice),
        Some((Transport::WebRtc, true)),
        "the local transport state is still recorded before the failed fan-out"
    );

    db.fail_get_room_players_for_test(false);
    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: false,
            },
        )
        .await;

    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, false).await;
    assert_eq!(
        valid_signal_budget_used(&server, &alice).await,
        1,
        "the single budget slot remains available for the next deliverable fan-out"
    );
    assert_eq!(
        server
            .metrics
            .transport_status_fanout
            .load(Ordering::Relaxed),
        1,
        "only the successful fan-out is counted"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_sender_only_room_does_not_consume_signal_budget() {
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());

    let room_id = create_db_room(&server, alice).await;
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: true,
            },
        )
        .await;

    assert_silent(&mut alice_rx).await;
    assert_eq!(
        valid_signal_budget_used(&server, &alice).await,
        0,
        "a sender-only room has no fan-out recipients to charge for"
    );
    assert_eq!(
        server
            .metrics
            .transport_status_fanout
            .load(Ordering::Relaxed),
        0,
        "a sender-only room is not a fan-out event"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_room_with_only_v2_peers_does_not_consume_signal_budget() {
    let server = create_test_server().await;
    let (alice, _alice_rx) = register_client(&server).await;
    let (legacy, mut legacy_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&legacy, v2_with_webrtc_transport());

    let room_id = create_db_room(&server, alice).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(legacy, "legacy"))
        .await
        .expect("add legacy peer");
    for id in [alice, legacy] {
        server
            .connection_manager
            .assign_client_to_room(&id, room_id)
            .await;
    }

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: true,
            },
        )
        .await;

    assert_silent(&mut legacy_rx).await;
    assert_eq!(
        valid_signal_budget_used(&server, &alice).await,
        0,
        "legacy-only recipient sets must not consume v3 control-plane budget"
    );
    assert_eq!(
        server
            .metrics
            .transport_status_fanout
            .load(Ordering::Relaxed),
        0,
        "no eligible v3 recipients means no fan-out event"
    );
}

// ---------------------------------------------------------------------------
// Relay floor never closes (P5): GameData still relays after a P2P-failure report.
// ---------------------------------------------------------------------------

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn game_data_still_relays_after_transport_status_disconnected() {
    let server = create_test_server().await;
    let (alice, _alice_rx) = register_client(&server).await;
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

    // Alice reports her P2P path failed and she fell back to the relay floor.
    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: false,
            },
        )
        .await;

    // The server keeps relaying GameData unconditionally — the floor never closes.
    let payload = json!({ "tick": 42 });
    server.handle_game_data(&alice, payload.clone()).await;

    match recv(&mut bob_rx).await.as_ref() {
        ServerMessage::GameData { from_player, data } => {
            assert_eq!(*from_player, alice);
            assert_eq!(*data, payload);
        }
        other => panic!("expected relayed GameData after fallback, got {other:?}"),
    }
}
