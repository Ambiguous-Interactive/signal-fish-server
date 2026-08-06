use crate::config::{
    CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig, SessionConfig,
    TransportSecurityConfig, TurnConfig,
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
        epoch: None,
        seq: None,
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

/// Drain every message already queued for a receiver (used to skip join/lobby
/// traffic that is not the subject of a test).
async fn drain_pending(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>) {
    loop {
        let pending = timeout(Duration::from_millis(50), receiver.recv()).await;
        match pending {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return,
        }
    }
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
async fn handle_player_ready_with_missing_room_returns_room_not_found_error() {
    // Regression guard for error-code classification: a player assigned to a
    // room that no longer exists in storage is an infrastructure/lookup failure,
    // NOT the `Finalized` business rejection. It must surface as
    // `ROOM_NOT_FOUND` — never `INVALID_ROOM_STATE` (which would mislead clients
    // into treating a transient lookup miss as "the game already started").
    let server = create_test_server().await;
    let (player_id, mut receiver) = register_client(&server).await;

    // Assign the client to a room id that was never persisted.
    let ghost_room: crate::protocol::RoomId = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&player_id, ghost_room)
        .await;

    server.handle_player_ready(&player_id).await;

    match recv(&mut receiver).await.as_ref() {
        ServerMessage::Error { error_code, .. } => {
            assert_eq!(
                *error_code,
                Some(ErrorCode::RoomNotFound),
                "a missing room must map to ROOM_NOT_FOUND, not INVALID_ROOM_STATE"
            );
        }
        other => panic!("unexpected response from handle_player_ready: {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn prune_ready_players_drops_only_dead_rooms() {
    // The all-paths leak backstop: the maintenance sweep removes coordinator
    // ready entries whose room no longer exists in storage (e.g. rooms reaped by
    // `cleanup_expired_rooms`, which reports no ids) and keeps live rooms' entries.
    let server = create_test_server().await;
    let (live_player, _live_rx) = register_client(&server).await;
    let (dead_player, _dead_rx) = register_client(&server).await;

    let make_room = |name: &str, creator: PlayerId| {
        let name = name.to_string();
        let database = std::sync::Arc::clone(&server.database);
        async move {
            database
                .create_room(
                    name,
                    None,
                    2,
                    true,
                    creator,
                    "udp".to_string(),
                    "region-a".to_string(),
                    None,
                )
                .await
                .expect("room creation succeeds")
        }
    };

    let live_room = make_room("prune-ready-live", live_player).await;
    let dead_room = make_room("prune-ready-dead", dead_player).await;

    // Give each room a coordinator ready entry, then delete the dead room from
    // storage so its entry is orphaned (a room-removal path that bypasses the
    // per-room empty-cleanup clear).
    server
        .room_coordinator
        .handle_player_ready(&live_room.id, &live_player, None)
        .await
        .expect("ready toggle on live room");
    server
        .room_coordinator
        .handle_player_ready(&dead_room.id, &dead_player, None)
        .await
        .expect("ready toggle on dead room");
    assert!(
        server
            .database
            .delete_room(&dead_room.id)
            .await
            .expect("delete dead room"),
        "dead room should have existed before deletion"
    );

    let before = server.room_coordinator.ready_player_room_ids().await;
    assert!(before.contains(&live_room.id) && before.contains(&dead_room.id));

    let removed = server.prune_ready_players().await;
    assert_eq!(removed, 1, "exactly the dead room's ready entry is pruned");

    let after = server.room_coordinator.ready_player_room_ids().await;
    assert!(
        after.contains(&live_room.id),
        "live room entry must be retained"
    );
    assert!(
        !after.contains(&dead_room.id),
        "dead room entry must be pruned"
    );
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

    // Real finalize flow: ready toggles broadcast lobby updates but NEVER start
    // the game on their own. The first toggle is a non-final update...
    server.handle_player_ready(&player_a).await;
    for (rx, who) in [(&mut rx_a, "player_a"), (&mut rx_b, "player_b")] {
        expect_lobby_state_changed(rx, false, who).await;
    }
    // ...the second toggle makes every player ready (`all_ready: true`) but the
    // game still does not start — no GameStarting/SessionPlan yet.
    server.handle_player_ready(&player_b).await;
    for (rx, who) in [(&mut rx_a, "player_a"), (&mut rx_b, "player_b")] {
        expect_lobby_state_changed(rx, true, who).await;
    }
    // An explicit StartGame finalizes: GameStarting, then the per-recipient mesh
    // SessionPlan (both members are v3+webrtc). No authority is set, so any
    // member may start.
    server.handle_start_game(&player_a).await;
    for (rx, who) in [(&mut rx_a, "player_a"), (&mut rx_b, "player_b")] {
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
                message.contains("already started"),
                "the rejection should explain the game already started: {message}"
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

/// Readiness belongs to a membership, not to a player id. A member who leaves
/// and joins again is a new, unready member: resurrecting the previous
/// readiness would both invert their next toggle and let the remaining members
/// reach `all_ready` (and therefore `StartGame`) without them.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn rejoining_a_room_does_not_restore_stale_readiness() {
    let server = create_test_server().await;
    let (player_a, mut rx_a) = register_client(&server).await;
    let (player_b, mut rx_b) = register_client(&server).await;

    server
        .handle_join_room(
            &player_a,
            "ready-rejoin-game".to_string(),
            None,
            "PlayerA".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;
    let (room_id, room_code) = match recv(&mut rx_a).await.as_ref() {
        ServerMessage::RoomJoined(payload) => (payload.room_id, payload.room_code.clone()),
        other => panic!("player_a expected RoomJoined, got {other:?}"),
    };
    server
        .handle_join_room(
            &player_b,
            "ready-rejoin-game".to_string(),
            Some(room_code.clone()),
            "PlayerB".to_string(),
            None,
            None,
            None,
        )
        .await;
    match recv(&mut rx_b).await.as_ref() {
        ServerMessage::RoomJoined(_) => {}
        other => panic!("player_b expected RoomJoined, got {other:?}"),
    }
    // The second join fills the lobby: drain the join/lobby traffic both members
    // observe before the readiness sequence under test.
    drain_pending(&mut rx_a).await;
    drain_pending(&mut rx_b).await;
    assert_eq!(
        server
            .database
            .get_room_by_id(&room_id)
            .await
            .expect("room lookup")
            .expect("room exists")
            .lobby_state,
        LobbyState::Lobby,
        "fixture precondition: the room accepts ready toggles"
    );

    // A readies, then leaves the room entirely.
    server.handle_player_ready(&player_a).await;
    for (rx, who) in [(&mut rx_a, "player_a"), (&mut rx_b, "player_b")] {
        expect_lobby_state_changed(rx, false, who).await;
    }
    server.leave_room(&player_a).await;
    match recv(&mut rx_a).await.as_ref() {
        ServerMessage::RoomLeft => {}
        other => panic!("player_a expected RoomLeft, got {other:?}"),
    }
    match recv(&mut rx_b).await.as_ref() {
        ServerMessage::PlayerLeft { .. } => {}
        other => panic!("player_b expected PlayerLeft, got {other:?}"),
    }
    // The departing member created the room, so its authority is cleared too.
    match recv(&mut rx_b).await.as_ref() {
        ServerMessage::AuthorityChanged {
            authority_player: None,
            ..
        } => {}
        other => panic!("player_b expected the cleared authority, got {other:?}"),
    }

    // A joins the same room again: a fresh membership starts unready.
    server
        .handle_join_room(
            &player_a,
            "ready-rejoin-game".to_string(),
            Some(room_code),
            "PlayerA".to_string(),
            None,
            None,
            None,
        )
        .await;
    match recv(&mut rx_a).await.as_ref() {
        ServerMessage::RoomJoined(payload) => {
            assert_eq!(
                payload.room_id, room_id,
                "the rejoin must land in the original room"
            );
            assert!(
                payload.ready_players.is_empty(),
                "a rejoining member must not be reported ready: {:?}",
                payload.ready_players
            );
            let rejoined = payload
                .current_players
                .iter()
                .find(|player| player.id == player_a)
                .expect("the rejoining member is present");
            assert!(
                !rejoined.is_ready,
                "a rejoining member's snapshot must show them unready"
            );
        }
        other => panic!("player_a expected RoomJoined on rejoin, got {other:?}"),
    }
    match recv(&mut rx_b).await.as_ref() {
        ServerMessage::PlayerJoined { .. } => {}
        other => panic!("player_b expected PlayerJoined on rejoin, got {other:?}"),
    }

    // B is now the only ready member, so the room is NOT all-ready.
    server.handle_player_ready(&player_b).await;
    for (rx, who) in [(&mut rx_a, "player_a"), (&mut rx_b, "player_b")] {
        expect_lobby_state_changed(rx, false, who).await;
    }
}
