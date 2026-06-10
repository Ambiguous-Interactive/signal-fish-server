use crate::config::{
    AuthMaintenanceConfig, CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig,
    SessionConfig, TransportSecurityConfig, TurnConfig,
};
use crate::database::DatabaseConfig;
use crate::protocol::{
    ErrorCode, LobbyState, PlayerId, PlayerInfo, ServerMessage, Topology, Transport,
};
use crate::server::{EnhancedGameServer, NegotiatedProtocol, ServerConfig};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

/// Allocate a unique loopback address per registered client so tests never
/// collide on the same `SocketAddr` (mirrors `signaling_tests`).
static PORT: AtomicU16 = AtomicU16::new(49000);

fn next_addr() -> SocketAddr {
    let port = PORT.fetch_add(1, Ordering::Relaxed);
    format!("127.0.0.1:{port}").parse().expect("valid addr")
}

async fn create_test_server() -> Arc<EnhancedGameServer> {
    create_test_server_with_session(SessionConfig::default()).await
}

/// Build a server with the given session policy, so the finalize path can
/// resolve to a non-relay plan and emit real `SessionPlan`s.
async fn create_test_server_with_session(session: SessionConfig) -> Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
        ServerConfig::default(),
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        session,
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

/// A mesh-preferring session config (STUN-less), so an all-v3+webrtc room
/// finalizes to `mesh + webrtc` and emits one `SessionPlan` per member.
fn mesh_session_config() -> SessionConfig {
    SessionConfig {
        default_topology: Topology::Mesh,
        ..SessionConfig::default()
    }
}

/// A v3 client supporting WebRTC and the mesh topology.
fn v3_webrtc() -> NegotiatedProtocol {
    NegotiatedProtocol {
        version: 3,
        transports: vec![Transport::Relay, Transport::WebRtc],
        topologies: vec![Topology::Relay, Topology::Mesh],
    }
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

/// Expect the next message to be a `LobbyStateChanged` with the given
/// `all_ready` flag.
async fn expect_lobby_state_changed(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    expect_all_ready: bool,
    who: &str,
) {
    match recv(receiver).await.as_ref() {
        ServerMessage::LobbyStateChanged { all_ready, .. } => {
            assert_eq!(
                *all_ready, expect_all_ready,
                "{who} saw an unexpected all_ready flag"
            );
        }
        other => panic!("{who} expected LobbyStateChanged, got {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn handle_player_ready_without_room_returns_not_in_room_error() {
    let server = create_test_server().await;
    let (player_id, mut receiver) = register_client(&server).await;

    server.handle_player_ready(&player_id).await;

    let response = recv(&mut receiver).await;

    match response.as_ref() {
        ServerMessage::Error { error_code, .. } => {
            assert_eq!(
                *error_code,
                Some(ErrorCode::NotInRoom),
                "ready handler should emit NotInRoom error when player lacks assignment"
            );
        }
        other => panic!("unexpected response from handle_player_ready: {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn handle_player_ready_after_finalize_returns_invalid_room_state_error() {
    // Finalized is terminal for ready toggles: drive the REAL finalize flow (a
    // full room enters the lobby, every member toggles ready through
    // `handle_player_ready`, the coordinator persists `Finalized` and
    // broadcasts `GameStarting`, and the server emits the v3 `SessionPlan`s),
    // then toggle `PlayerReady` once more. The toggling player must receive
    // exactly `Error { error_code: INVALID_ROOM_STATE }` — with NO
    // `LobbyStateChanged` and NO second `GameStarting`/`SessionPlan` to anyone
    // — and the stored room must stay `Finalized`.
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (player_a, mut rx_a) = register_client(&server).await;
    let (player_b, mut rx_b) = register_client(&server).await;
    server.set_client_protocol(&player_a, v3_webrtc());
    server.set_client_protocol(&player_b, v3_webrtc());

    // A 2-seat room holding both players, transitioned to the lobby so ready
    // toggles are accepted.
    let room = server
        .database
        .create_room(
            "post-finalize-ready-game".to_string(),
            None,
            2,
            true,
            player_a,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");
    assert!(server
        .database
        .add_player_to_room(&room.id, player_info(player_b, "PlayerB"))
        .await
        .expect("adding second player succeeds"));
    server
        .database
        .transition_room_to_lobby(&room.id)
        .await
        .expect("lobby transition succeeds");
    for id in [&player_a, &player_b] {
        server
            .connection_manager
            .assign_client_to_room(id, room.id)
            .await;
    }

    // Real finalize flow: first toggle broadcasts a non-final lobby update...
    server.handle_player_ready(&player_a).await;
    for (rx, who) in [(&mut rx_a, "player_a"), (&mut rx_b, "player_b")] {
        expect_lobby_state_changed(rx, false, who).await;
    }
    // ...and the last toggle finalizes: final lobby update, GameStarting, then
    // the per-recipient mesh SessionPlan (both members are v3+webrtc).
    server.handle_player_ready(&player_b).await;
    for (rx, who) in [(&mut rx_a, "player_a"), (&mut rx_b, "player_b")] {
        expect_lobby_state_changed(rx, true, who).await;
        match recv(rx).await.as_ref() {
            ServerMessage::GameStarting { .. } => {}
            other => panic!("{who} expected GameStarting, got {other:?}"),
        }
        match recv(rx).await.as_ref() {
            ServerMessage::SessionPlan(plan) => {
                assert_eq!(plan.topology, Topology::Mesh);
                assert_eq!(plan.transport, Transport::WebRtc);
            }
            other => panic!("{who} expected SessionPlan after GameStarting, got {other:?}"),
        }
    }
    let stored = server
        .database
        .get_room_by_id(&room.id)
        .await
        .expect("room lookup")
        .expect("room exists");
    assert_eq!(
        stored.lobby_state,
        LobbyState::Finalized,
        "the real ready flow must persist the finalized lobby state"
    );

    // The post-finalize toggle is rejected with INVALID_ROOM_STATE...
    server.handle_player_ready(&player_a).await;
    match recv(&mut rx_a).await.as_ref() {
        ServerMessage::Error {
            error_code,
            message,
        } => {
            assert_eq!(
                *error_code,
                Some(ErrorCode::InvalidRoomState),
                "post-finalize ready toggles must be rejected with INVALID_ROOM_STATE"
            );
            assert!(
                message.contains("lobby state"),
                "the rejection should explain the lobby-state requirement: {message}"
            );
        }
        other => panic!("expected Error after a post-finalize ready toggle, got {other:?}"),
    }

    // ...and mutates nothing: no LobbyStateChanged, no second GameStarting or
    // SessionPlan, to either member; the room stays Finalized.
    assert_silent(&mut rx_a).await;
    assert_silent(&mut rx_b).await;
    let after = server
        .database
        .get_room_by_id(&room.id)
        .await
        .expect("room lookup")
        .expect("room exists");
    assert_eq!(
        after.lobby_state,
        LobbyState::Finalized,
        "a rejected ready toggle must not regress the finalized room"
    );
}
