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
///
/// Every operation these tests drain after is fully awaited, so the frames are
/// already enqueued: draining what is present is deterministic, where waiting on
/// a silence window would be a wall-clock heuristic.
fn drain_pending(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>) {
    loop {
        match receiver.try_recv() {
            Ok(_) => {}
            Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                return;
            }
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

/// The coordinator's `StartGameOutcome` rejections must reach the wire with
/// their exact, distinct `ErrorCode`s through
/// [`EnhancedGameServer::handle_start_game`] — `NotReady` =>
/// `GAME_START_NOT_READY`, `Forbidden` => `GAME_START_FORBIDDEN`,
/// `AlreadyStarted` => `INVALID_ROOM_STATE` — each with no state mutation and
/// no traffic to any other member.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn start_game_rejections_map_to_exact_wire_error_codes() {
    let server = create_test_server().await;

    // NotReady: an open lobby with an unready member rejects the start.
    let (player_a, mut rx_a) = register_client(&server).await;
    let (player_b, mut rx_b) = register_client(&server).await;
    server
        .handle_join_room(
            &player_a,
            "start-not-ready".to_string(),
            None,
            "PlayerA".to_string(),
            Some(4),
            Some(false),
            None,
        )
        .await;
    let room_code = match recv(&mut rx_a).await.as_ref() {
        ServerMessage::RoomJoined(payload) => payload.room_code.clone(),
        other => panic!("player_a expected RoomJoined, got {other:?}"),
    };
    server
        .handle_join_room(
            &player_b,
            "start-not-ready".to_string(),
            Some(room_code),
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
    drain_pending(&mut rx_a);
    drain_pending(&mut rx_b);
    server.handle_player_ready(&player_a).await;
    drain_pending(&mut rx_a);
    drain_pending(&mut rx_b);

    server.handle_start_game(&player_a).await;
    match recv(&mut rx_a).await.as_ref() {
        ServerMessage::Error { error_code, .. } => {
            assert_eq!(
                *error_code,
                Some(ErrorCode::GameStartNotReady),
                "a start with an unready member must be GAME_START_NOT_READY"
            );
        }
        other => panic!("expected NotReady rejection, got {other:?}"),
    }
    assert_silent(&mut rx_b).await;
    let not_ready_room_id = server.get_client_room(&player_a).await.expect("room");
    assert_eq!(
        server
            .database
            .get_room_by_id(&not_ready_room_id)
            .await
            .expect("room lookup")
            .expect("room exists")
            .lobby_state,
        LobbyState::Lobby,
        "a NotReady rejection must not mutate the room"
    );

    // Forbidden: an authority-designated, all-ready room rejects a
    // non-authority sender.
    let (owner, mut rx_owner) = register_client(&server).await;
    let (member, mut rx_member) = register_client(&server).await;
    server
        .handle_join_room(
            &owner,
            "start-forbidden".to_string(),
            None,
            "Owner".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;
    let authority_code = match recv(&mut rx_owner).await.as_ref() {
        ServerMessage::RoomJoined(payload) => payload.room_code.clone(),
        other => panic!("owner expected RoomJoined, got {other:?}"),
    };
    server
        .handle_join_room(
            &member,
            "start-forbidden".to_string(),
            Some(authority_code),
            "Member".to_string(),
            None,
            None,
            None,
        )
        .await;
    match recv(&mut rx_member).await.as_ref() {
        ServerMessage::RoomJoined(_) => {}
        other => panic!("member expected RoomJoined, got {other:?}"),
    }
    drain_pending(&mut rx_owner);
    drain_pending(&mut rx_member);
    server.handle_player_ready(&owner).await;
    server.handle_player_ready(&member).await;
    drain_pending(&mut rx_owner);
    drain_pending(&mut rx_member);

    server.handle_start_game(&member).await;
    match recv(&mut rx_member).await.as_ref() {
        ServerMessage::Error { error_code, .. } => {
            assert_eq!(
                *error_code,
                Some(ErrorCode::GameStartForbidden),
                "a non-authority start must be GAME_START_FORBIDDEN"
            );
        }
        other => panic!("expected Forbidden rejection, got {other:?}"),
    }
    assert_silent(&mut rx_owner).await;
    let forbidden_room_id = server.get_client_room(&owner).await.expect("room");
    assert_eq!(
        server
            .database
            .get_room_by_id(&forbidden_room_id)
            .await
            .expect("room lookup")
            .expect("room exists")
            .lobby_state,
        LobbyState::Lobby,
        "a Forbidden rejection must not mutate the room"
    );

    // AlreadyStarted: the authority's start succeeds; the next start is
    // INVALID_ROOM_STATE.
    server.handle_start_game(&owner).await;
    for (rx, who) in [(&mut rx_owner, "owner"), (&mut rx_member, "member")] {
        match recv(rx).await.as_ref() {
            ServerMessage::GameStarting { .. } => {}
            other => panic!("{who} expected GameStarting, got {other:?}"),
        }
    }
    server.handle_start_game(&member).await;
    match recv(&mut rx_member).await.as_ref() {
        ServerMessage::Error { error_code, .. } => {
            assert_eq!(
                *error_code,
                Some(ErrorCode::InvalidRoomState),
                "a start on a finalized room must be INVALID_ROOM_STATE"
            );
        }
        other => panic!("expected AlreadyStarted rejection, got {other:?}"),
    }
    assert_silent(&mut rx_owner).await;
    assert_eq!(
        server
            .database
            .get_room_by_id(&forbidden_room_id)
            .await
            .expect("room lookup")
            .expect("room exists")
            .lobby_state,
        LobbyState::Finalized,
        "an AlreadyStarted rejection must not regress the finalized room"
    );
}

/// Pin of the documented `all_ready` semantics (issue #447 F1, decision b):
/// `LobbyStateChanged` fires on readiness toggles only. A later join — always
/// unready — emits `PlayerJoined` with NO corrective broadcast, so peers'
/// cached `all_ready: true` goes stale while the authoritative `StartGame`
/// gate rejects with `GAME_START_NOT_READY`. The next real toggle restores an
/// `all_ready: true` that a retrying client can start on.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn join_breaks_cached_all_ready_without_a_corrective_broadcast() {
    let server = create_test_server().await;
    let (player_a, mut rx_a) = register_client(&server).await;
    let (player_b, mut rx_b) = register_client(&server).await;
    let (latecomer, mut rx_late) = register_client(&server).await;

    server
        .handle_join_room(
            &player_a,
            "all-ready-staleness".to_string(),
            None,
            "PlayerA".to_string(),
            Some(4),
            Some(false),
            None,
        )
        .await;
    let room_code = match recv(&mut rx_a).await.as_ref() {
        ServerMessage::RoomJoined(payload) => payload.room_code.clone(),
        other => panic!("player_a expected RoomJoined, got {other:?}"),
    };
    let latecomer_code = room_code.clone();
    server
        .handle_join_room(
            &player_b,
            "all-ready-staleness".to_string(),
            Some(room_code),
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
    drain_pending(&mut rx_a);
    drain_pending(&mut rx_b);

    server.handle_player_ready(&player_a).await;
    for (rx, who) in [(&mut rx_a, "player_a"), (&mut rx_b, "player_b")] {
        expect_lobby_state_changed(rx, false, who).await;
    }
    server.handle_player_ready(&player_b).await;
    for (rx, who) in [(&mut rx_a, "player_a"), (&mut rx_b, "player_b")] {
        expect_lobby_state_changed(rx, true, who).await;
    }

    // The latecomer joins the still-open lobby: the incumbents' next (and
    // only) frame is `PlayerJoined` — no corrective `LobbyStateChanged`.
    server
        .handle_join_room(
            &latecomer,
            "all-ready-staleness".to_string(),
            Some(latecomer_code),
            "Latecomer".to_string(),
            None,
            None,
            None,
        )
        .await;
    match recv(&mut rx_late).await.as_ref() {
        ServerMessage::RoomJoined(payload) => {
            let mut ready = payload.ready_players.clone();
            ready.sort_unstable();
            let mut expected_ready = vec![player_a, player_b];
            expected_ready.sort_unstable();
            assert_eq!(
                ready, expected_ready,
                "the joiner's snapshot shows the two ready incumbents: {:?}",
                payload.ready_players
            );
            assert_eq!(
                payload.current_players.len(),
                3,
                "the joiner's snapshot shows three current members"
            );
        }
        other => panic!("latecomer expected RoomJoined, got {other:?}"),
    }
    for (rx, who) in [(&mut rx_a, "player_a"), (&mut rx_b, "player_b")] {
        match recv(rx).await.as_ref() {
            ServerMessage::PlayerJoined { player } => {
                assert_eq!(player.id, latecomer);
            }
            other => panic!("{who} expected PlayerJoined, got {other:?}"),
        }
        assert_silent(rx).await;
    }

    // The cached `all_ready: true` is stale: the authoritative gate rejects.
    server.handle_start_game(&player_a).await;
    match recv(&mut rx_a).await.as_ref() {
        ServerMessage::Error {
            error_code,
            message,
        } => {
            assert_eq!(*error_code, Some(ErrorCode::GameStartNotReady));
            assert!(
                message.contains("ready"),
                "rejection explains why: {message}"
            );
        }
        other => panic!("expected GAME_START_NOT_READY after the late join, got {other:?}"),
    }
    assert_silent(&mut rx_b).await;
    assert_silent(&mut rx_late).await;

    // The latecomer's toggle is a real readiness change: everyone — including
    // the joiner — sees `all_ready: true` again.
    server.handle_player_ready(&latecomer).await;
    for (rx, who) in [
        (&mut rx_a, "player_a"),
        (&mut rx_b, "player_b"),
        (&mut rx_late, "latecomer"),
    ] {
        expect_lobby_state_changed(rx, true, who).await;
    }
    // ...but the latecomer un-readies immediately (a second real toggle):
    // readiness is whole again only after it leaves, so a retried start is
    // rejected once more.
    server.handle_player_ready(&latecomer).await;
    for (rx, who) in [
        (&mut rx_a, "player_a"),
        (&mut rx_b, "player_b"),
        (&mut rx_late, "latecomer"),
    ] {
        expect_lobby_state_changed(rx, false, who).await;
    }
    server.handle_start_game(&player_a).await;
    match recv(&mut rx_a).await.as_ref() {
        ServerMessage::Error { error_code, .. } => {
            assert_eq!(*error_code, Some(ErrorCode::GameStartNotReady));
        }
        other => panic!("expected GAME_START_NOT_READY after the un-ready, got {other:?}"),
    }
    assert_silent(&mut rx_b).await;
    assert_silent(&mut rx_late).await;

    // The unready latecomer leaves instead of readying: no readiness
    // broadcast fires (departures are not toggles), yet the authoritative
    // gate is whole again — a retried start finalizes the lobby.
    server.leave_room(&latecomer).await;
    match recv(&mut rx_late).await.as_ref() {
        ServerMessage::RoomLeft => {}
        other => panic!("latecomer expected RoomLeft, got {other:?}"),
    }
    for rx in [&mut rx_a, &mut rx_b] {
        match recv(rx).await.as_ref() {
            ServerMessage::PlayerLeft { .. } => {}
            other => panic!("incumbent expected PlayerLeft, got {other:?}"),
        }
        assert_silent(rx).await;
    }
    server.handle_start_game(&player_a).await;
    for (rx, who) in [(&mut rx_a, "player_a"), (&mut rx_b, "player_b")] {
        match recv(rx).await.as_ref() {
            ServerMessage::GameStarting { .. } => {}
            other => panic!("{who} expected GameStarting, got {other:?}"),
        }
    }
    assert_silent(&mut rx_late).await;
}

/// Old-generation room operations must be silent no-ops after a reconnect
/// reclaim: the reclaim renames the transient incarnation's lifecycle to the
/// restored identity, so an in-flight `PlayerReady`/`StartGame` parked on that
/// lifecycle belongs to a generation that no longer exists. The fence
/// (`ready_state.rs`) must return without emitting anything — notably without
/// the `NotInRoom` error an unfenced handler would send — and without mutating
/// readiness or the room.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn stale_generation_ready_and_start_game_are_silent_noops_after_reclaim() {
    let server = create_test_server().await;
    let (player_a, mut rx_a) = register_client(&server).await;
    let (player_b, mut rx_b) = register_client(&server).await;
    server
        .handle_join_room(
            &player_a,
            "stale-generation-fence".to_string(),
            None,
            "PlayerA".to_string(),
            Some(4),
            Some(false),
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
            "stale-generation-fence".to_string(),
            Some(room_code),
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
    drain_pending(&mut rx_a);
    drain_pending(&mut rx_b);
    server.handle_player_ready(&player_a).await;
    drain_pending(&mut rx_a);
    drain_pending(&mut rx_b);
    assert!(server
        .room_coordinator
        .current_ready_players(&room_id)
        .await
        .contains(&player_a));
    let ready_before = server
        .room_coordinator
        .current_ready_players(&room_id)
        .await;

    // Player A's connection drops; a transient incarnation connects and is
    // about to be reclaimed as the restored identity.
    server.connection_manager.remove_client(&player_a);
    let (transient, mut rx_transient) = register_client(&server).await;

    // Two stale in-flight operations for the transient incarnation park on its
    // lifecycle gate (the handler fetches the lifecycle, then takes the gate).
    let lifecycle = server
        .connection_manager
        .client_lifecycle(&transient)
        .expect("transient incarnation has a lifecycle");
    let _parked_gate = lifecycle.lock().await;
    let ready_task = {
        let server = Arc::clone(&server);
        async move { server.handle_player_ready(&transient).await }
    };
    let start_task = {
        let server = Arc::clone(&server);
        async move { server.handle_start_game(&transient).await }
    };
    let (mut ready_task, mut start_task) = (tokio::spawn(ready_task), tokio::spawn(start_task));
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }

    // The reclaim renames the parked lifecycle to the restored identity. The
    // epoch mirrors production's `last_epoch + 1` from the surviving
    // reconnection record (nothing here reads game-data stamps).
    match server
        .connection_manager
        .reassign_connection(&transient, &player_a, room_id, 1)
    {
        crate::server::connection_manager::ReassignmentOutcome::Reassigned(_) => {}
        other => panic!("reconnect reclaim succeeds, got {other:?}"),
    }
    drop(_parked_gate);
    tokio::time::timeout(Duration::from_secs(1), &mut ready_task)
        .await
        .expect("stale ready handler completes")
        .expect("stale ready handler does not panic");
    tokio::time::timeout(Duration::from_secs(1), &mut start_task)
        .await
        .expect("stale start handler completes")
        .expect("stale start handler does not panic");

    // The fenced handlers stayed silent: no error frame to the restored
    // connection, its incumbent, or anyone else.
    assert_silent(&mut rx_transient).await;
    assert_silent(&mut rx_b).await;

    // A start attempt for the departed incarnation id finds no lifecycle at
    // all and is equally silent.
    server.handle_start_game(&transient).await;
    assert_silent(&mut rx_transient).await;

    // Nothing mutated: A's readiness is intact and its room assignment was
    // restored by the reclaim.
    assert_eq!(
        server
            .room_coordinator
            .current_ready_players(&room_id)
            .await,
        ready_before,
        "a stale-generation operation must not mutate readiness"
    );
    assert_eq!(
        server.get_client_room(&player_a).await,
        Some(room_id),
        "the reclaim restores the room assignment"
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
    drain_pending(&mut rx_a);
    drain_pending(&mut rx_b);
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
    // The departing member created the room, so the departure also clears the
    // authority role. That contract belongs to `room_service_tests`; drain it
    // here so a regression there cannot masquerade as a readiness failure.
    drain_pending(&mut rx_b);

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

/// A member that reconnects into a running game is still one of the members
/// that started it. Removal prunes the departing id from the finalized room's
/// ready list, so the restored membership carries the only surviving evidence —
/// and readiness cannot be re-established by hand, because a finalized room
/// rejects `PlayerReady`.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reconnecting_into_a_finalized_room_restores_that_members_readiness() {
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (player_a, mut rx_a) = register_client(&server).await;
    let (player_b, mut rx_b) = register_client(&server).await;
    let (returning, mut returning_rx) = register_client(&server).await;
    for player in [player_a, player_b, returning] {
        server.set_client_protocol(&player, v3_webrtc());
    }

    server
        .handle_join_room(
            &player_a,
            "finalized-reconnect".to_string(),
            Some("FINAL2".to_string()),
            "PlayerA".to_string(),
            Some(2),
            Some(true),
            None,
        )
        .await;
    server
        .handle_join_room(
            &player_b,
            "finalized-reconnect".to_string(),
            Some("FINAL2".to_string()),
            "PlayerB".to_string(),
            None,
            None,
            None,
        )
        .await;
    drain_pending(&mut rx_a);
    drain_pending(&mut rx_b);
    server.handle_player_ready(&player_a).await;
    server.handle_player_ready(&player_b).await;
    server.handle_start_game(&player_a).await;
    drain_pending(&mut rx_a);
    drain_pending(&mut rx_b);

    let room_id = server
        .get_client_room(&player_b)
        .await
        .expect("member has a room");
    let player_b_info = server
        .database
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup")
        .expect("room exists")
        .players
        .get(&player_b)
        .cloned()
        .expect("member is stored");
    assert!(
        player_b_info.is_ready,
        "premise: finalization marks every member of the started game ready in \
         the record the disconnect capture reads"
    );
    let manager = server.reconnection_manager().expect("reconnection enabled");
    let token = manager
        .register_disconnection(
            player_b,
            room_id,
            false,
            Some(player_b_info),
            server
                .connection_manager
                .game_data_epoch(&player_b)
                .unwrap_or(0),
        )
        .await;
    server
        .database
        .remove_player_from_room(&room_id, &player_b)
        .await
        .expect("remove disconnected member")
        .expect("member was present");
    server.connection_manager.remove_client(&player_b);
    server
        .message_coordinator
        .unregister_local_client(&player_b)
        .await
        .expect("unroute disconnected member");

    assert!(
        server
            .handle_reconnect(&returning, &player_b, &room_id, &token)
            .await
    );

    match recv(&mut returning_rx).await.as_ref() {
        ServerMessage::Reconnected(payload) => {
            assert!(
                payload.ready_players.contains(&player_b),
                "the restored member is one of the members that started the game: {:?}",
                payload.ready_players
            );
            let restored = payload
                .current_players
                .iter()
                .find(|player| player.id == player_b)
                .expect("the restored member is in its own snapshot");
            assert!(
                restored.is_ready,
                "a member of a running game is not unready: {:?}",
                payload.current_players
            );
        }
        other => panic!("expected Reconnected, got {other:?}"),
    }
}

/// Finalization moves readiness: the coordinator's entry is dropped and the
/// final set is written into the room record. A snapshot taken after the game
/// starts must read the record, or every member of a running game is reported
/// unready — the state a spectator joining a live game sees.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn snapshots_of_a_finalized_room_report_its_final_readiness() {
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (player_a, mut rx_a) = register_client(&server).await;
    let (player_b, mut rx_b) = register_client(&server).await;
    let (observer, mut observer_rx) = register_client(&server).await;
    for player in [player_a, player_b] {
        server.set_client_protocol(&player, v3_webrtc());
    }

    server
        .handle_join_room(
            &player_a,
            "finalized-spectate".to_string(),
            Some("FINAL1".to_string()),
            "PlayerA".to_string(),
            Some(2),
            Some(true),
            None,
        )
        .await;
    server
        .handle_join_room(
            &player_b,
            "finalized-spectate".to_string(),
            Some("FINAL1".to_string()),
            "PlayerB".to_string(),
            None,
            None,
            None,
        )
        .await;
    drain_pending(&mut rx_a);
    drain_pending(&mut rx_b);

    server.handle_player_ready(&player_a).await;
    server.handle_player_ready(&player_b).await;
    server.handle_start_game(&player_a).await;
    drain_pending(&mut rx_a);
    drain_pending(&mut rx_b);

    let room_id = server
        .get_client_room(&player_a)
        .await
        .expect("member has a room");
    assert_eq!(
        server
            .database
            .get_room_by_id(&room_id)
            .await
            .expect("room lookup")
            .expect("room exists")
            .lobby_state,
        LobbyState::Finalized,
        "fixture precondition: the game has started"
    );

    server
        .handle_join_as_spectator(
            &observer,
            "finalized-spectate".to_string(),
            "FINAL1".to_string(),
            "Observer".to_string(),
        )
        .await;

    match recv(&mut observer_rx).await.as_ref() {
        ServerMessage::SpectatorJoined(payload) => {
            assert_eq!(payload.current_players.len(), 2);
            for player in &payload.current_players {
                assert!(
                    player.is_ready,
                    "a finalized room's members are ready: {:?}",
                    payload.current_players
                );
            }
        }
        other => panic!("observer expected SpectatorJoined, got {other:?}"),
    }
}

/// A spectator's room snapshot must report readiness from the same source the
/// members' own snapshots use. Readiness lives in the coordinator, never in the
/// stored player record during the lobby, so projecting the stored flag shows a
/// spectator an all-unready lobby no matter what the members did — and the
/// spectator receives no lobby broadcasts that could correct it.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn spectator_snapshot_reports_live_readiness() {
    let server = create_test_server().await;
    let (player_a, mut rx_a) = register_client(&server).await;
    let (player_b, mut rx_b) = register_client(&server).await;
    let (observer, mut observer_rx) = register_client(&server).await;

    server
        .handle_join_room(
            &player_a,
            "spectated-game".to_string(),
            Some("SPECT1".to_string()),
            "PlayerA".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;
    server
        .handle_join_room(
            &player_b,
            "spectated-game".to_string(),
            Some("SPECT1".to_string()),
            "PlayerB".to_string(),
            None,
            None,
            None,
        )
        .await;
    drain_pending(&mut rx_a);
    drain_pending(&mut rx_b);

    server.handle_player_ready(&player_a).await;
    server.handle_player_ready(&player_b).await;
    drain_pending(&mut rx_a);
    drain_pending(&mut rx_b);

    server
        .handle_join_as_spectator(
            &observer,
            "spectated-game".to_string(),
            "SPECT1".to_string(),
            "Observer".to_string(),
        )
        .await;

    match recv(&mut observer_rx).await.as_ref() {
        ServerMessage::SpectatorJoined(payload) => {
            assert_eq!(
                payload.lobby_state,
                LobbyState::Lobby,
                "fixture precondition: the observed room is still in its lobby"
            );
            assert_eq!(payload.current_players.len(), 2);
            for player in &payload.current_players {
                assert!(
                    player.is_ready,
                    "the spectator snapshot must report the live ready state: {:?}",
                    payload.current_players
                );
            }
        }
        other => panic!("observer expected SpectatorJoined, got {other:?}"),
    }
}
