//! Handler tests for the P2 targeted signal relay (`signaling.rs`).
//!
//! Mirrors the `message_router_tests` harness: register clients, set their
//! negotiated protocol, drive `handle_signal` / `handle_webrtc_late_join`, and
//! assert on what each client receives. Covers the happy path, every rejection
//! branch, glare determinism, late-join offerer designation, and v2 gating
//! (Appendix K).

use crate::config::{
    AuthMaintenanceConfig, CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig,
    TransportSecurityConfig,
};
use crate::database::DatabaseConfig;
use crate::protocol::{ErrorCode, PlayerId, PlayerInfo, ServerMessage, Topology, Transport};
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
    let config = ServerConfig {
        rate_limit_config: RateLimitConfig {
            max_signals,
            ..RateLimitConfig::default()
        },
        ..ServerConfig::default()
    };
    EnhancedGameServer::new(
        config,
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
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

fn v3_relay_only() -> NegotiatedProtocol {
    NegotiatedProtocol {
        version: 3,
        transports: vec![Transport::Relay],
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

// ---------------------------------------------------------------------------
// Late join (offerer designation).
// ---------------------------------------------------------------------------

/// Build a real DB-backed room owned by `owner`, returning its id.
async fn create_db_room(server: &EnhancedGameServer, owner: PlayerId) -> uuid::Uuid {
    let room = server
        .database
        .create_room(
            "webrtc-game".to_string(),
            None,
            8,
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
async fn late_join_designates_exactly_one_offerer() {
    let server = create_test_server().await;
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
    server.handle_webrtc_late_join(&joiner, &members).await;

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

    // Exactly one side initiates, consistent with local_initiates.
    assert_ne!(existing_flag, joiner_flag);
    assert_eq!(existing_flag, local_initiates(existing, joiner));
    assert_eq!(joiner_flag, local_initiates(joiner, existing));

    assert_silent(&mut existing_rx).await;
    assert_silent(&mut joiner_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_skips_v2_members() {
    // Appendix K gating: a v2 (relay-only) member in the room receives no
    // NewPeer, and the joiner is not paired with it.
    let server = create_test_server().await;
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
    server.handle_webrtc_late_join(&joiner, &members).await;

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
    let server = create_test_server().await;
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
    server.handle_webrtc_late_join(&joiner, &members).await;

    // A relay-only joiner triggers no NewPeer in either direction.
    assert_silent(&mut existing_rx).await;
    assert_silent(&mut joiner_rx).await;
}
