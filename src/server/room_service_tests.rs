use super::*;
use crate::config::{
    AuthMaintenanceConfig, CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig,
    SessionConfig, TransportSecurityConfig, TurnConfig,
};
use crate::database::DatabaseConfig;
use crate::protocol::{ErrorCode, PlayerId, PlayerInfo, ServerMessage};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

async fn create_test_server() -> Arc<EnhancedGameServer> {
    create_test_server_with_config(ServerConfig::default()).await
}

async fn create_test_server_with_config(config: ServerConfig) -> Arc<EnhancedGameServer> {
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

async fn register_client(
    server: &EnhancedGameServer,
    addr: SocketAddr,
) -> (PlayerId, mpsc::Receiver<Arc<ServerMessage>>) {
    let (sender, receiver) = mpsc::channel(8);
    let player_id = server
        .connection_manager
        .register_client(
            sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            addr,
            server.instance_id,
        )
        .await
        .expect("client registration succeeds");
    (player_id, receiver)
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn leave_room_sends_confirmation_and_clears_membership() {
    let server = create_test_server().await;
    let (sender, mut receiver) = mpsc::channel(8);
    let addr: SocketAddr = "127.0.0.1:48000".parse().unwrap();
    let player_id = server
        .connection_manager
        .register_client(
            sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            addr,
            server.instance_id,
        )
        .await
        .expect("client registration succeeds");

    let room = server
        .database
        .create_room(
            "test-game".to_string(),
            Some("ABCD".to_string()),
            4,
            true,
            player_id,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");

    server
        .connection_manager
        .assign_client_to_room(&player_id, room.id)
        .await;

    server.leave_room(&player_id).await;

    let confirmation = timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("channel still open")
        .expect("room left message present");
    assert!(
        matches!(*confirmation, ServerMessage::RoomLeft),
        "expected RoomLeft confirmation"
    );

    assert!(
        server.get_client_room(&player_id).await.is_none(),
        "room assignment should be cleared"
    );

    let room_after = server
        .database
        .get_room_by_id(&room.id)
        .await
        .expect("room lookup succeeds")
        .expect("room still exists");
    assert!(
        !room_after.players.contains_key(&player_id),
        "player should be removed from room state"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_unregister_removes_membership_without_roomleft_noise() {
    let server = create_test_server().await;
    let (player_id, mut receiver) =
        register_client(&server, "127.0.0.1:48011".parse().unwrap()).await;
    let (survivor_id, mut survivor_receiver) =
        register_client(&server, "127.0.0.1:48012".parse().unwrap()).await;

    let room = server
        .database
        .create_room(
            "test-game".to_string(),
            Some("DRAIN1".to_string()),
            4,
            true,
            player_id,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");

    server
        .connection_manager
        .assign_client_to_room(&player_id, room.id)
        .await;
    server
        .database
        .add_player_to_room(
            &room.id,
            PlayerInfo {
                id: survivor_id,
                name: "survivor".to_string(),
                is_authority: false,
                is_ready: false,
                connected_at: chrono::Utc::now(),
                connection_info: None,
                epoch: None,
                region_id: "region-a".to_string(),
            },
        )
        .await
        .expect("survivor insert succeeds");
    server
        .connection_manager
        .assign_client_to_room(&survivor_id, room.id)
        .await;

    assert!(
        server.begin_shutdown_drain().started_by_this_call,
        "test must transition the server into draining"
    );

    server.unregister_client(&player_id).await;

    match receiver.try_recv() {
        Ok(message) => panic!("shutdown unregister must not enqueue room traffic: {message:?}"),
        Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {}
    }
    match survivor_receiver.try_recv() {
        Ok(message) => panic!("shutdown unregister must not broadcast room traffic: {message:?}"),
        Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {}
    }

    assert!(
        server.get_client_room(&player_id).await.is_none(),
        "room assignment should be cleared"
    );

    let room_after = server
        .database
        .get_room_by_id(&room.id)
        .await
        .expect("room lookup succeeds")
        .expect("room still exists");
    assert!(
        !room_after.players.contains_key(&player_id),
        "player should be removed from room state"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn max_room_cap_denial_releases_join_coordination_locks() {
    let server = create_test_server_with_config(ServerConfig {
        max_rooms_per_game: 0,
        ..ServerConfig::default()
    })
    .await;
    let (player_id, mut receiver) =
        register_client(&server, "127.0.0.1:48001".parse().unwrap()).await;

    server
        .handle_join_room(
            &player_id,
            "test-game".to_string(),
            Some("ABCDEF".to_string()),
            "player".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;

    let response = timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("channel still open")
        .expect("join failure message present");
    match response.as_ref() {
        ServerMessage::RoomJoinFailed { error_code, .. } => {
            assert_eq!(
                *error_code,
                Some(ErrorCode::MaxRoomsPerGameExceeded),
                "max room cap denial should be reported"
            );
        }
        other => panic!("expected RoomJoinFailed, got {other:?}"),
    }

    assert!(
        !server
            .distributed_lock
            .is_locked("room_join:test-game:ABCDEF")
            .await
            .expect("room join lock check succeeds"),
        "room join lock must be released after max-room-cap denial"
    );
    assert!(
        !server
            .distributed_lock
            .is_locked("game_room_cap:test-game")
            .await
            .expect("room cap lock check succeeds"),
        "room cap lock must be released after max-room-cap denial"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_server_rejects_room_creation_without_consuming_join_locks() {
    let server = create_test_server().await;

    let drain = server.begin_shutdown_drain();
    assert!(
        drain.started_by_this_call,
        "test must transition the server into draining"
    );

    for (room_code, port) in [(None, 48009), (Some("ABSENT"), 48010)] {
        let (player_id, mut receiver) =
            register_client(&server, format!("127.0.0.1:{port}").parse().unwrap()).await;

        server
            .handle_join_room(
                &player_id,
                "test-game".to_string(),
                room_code.map(str::to_string),
                "player".to_string(),
                Some(4),
                Some(true),
                None,
            )
            .await;

        let response = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("channel still open")
            .expect("join failure message present");
        match response.as_ref() {
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                assert_eq!(
                    *error_code,
                    Some(ErrorCode::ServerDraining),
                    "room creation during drain should be reported as SERVER_DRAINING"
                );
                assert!(
                    reason.contains("draining"),
                    "rejection reason should mention draining: {reason}"
                );
            }
            other => panic!("expected RoomJoinFailed, got {other:?}"),
        }

        if let Some(code) = room_code {
            assert!(
                server
                    .database
                    .get_room("test-game", code)
                    .await
                    .expect("room lookup succeeds")
                    .is_none(),
                "provided room code must not be created during drain"
            );
            assert!(
                !server
                    .distributed_lock
                    .is_locked(&format!("room_join:test-game:{code}"))
                    .await
                    .expect("room join lock check succeeds"),
                "early drain rejection should happen before room-join lock acquisition"
            );
        }
        assert!(
            !server
                .distributed_lock
                .is_locked("game_room_cap:test-game")
                .await
                .expect("room cap lock check succeeds"),
            "drain rejection should happen before room-cap lock acquisition"
        );
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_server_rejects_late_client_registration() {
    let server = create_test_server().await;
    let drain = server.begin_shutdown_drain();
    assert!(
        drain.started_by_this_call,
        "test must transition the server into draining"
    );

    let (sender, _receiver) = mpsc::channel(1);
    let result = server
        .register_client_with_close(
            sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:48013".parse().unwrap(),
        )
        .await;

    assert!(
        matches!(result, Err(RegisterClientError::ServerDraining)),
        "late WebSocket registration after drain must be rejected before entering the connection manager"
    );
    assert!(
        server.connection_manager.client_ids().is_empty(),
        "drain-rejected registration must not leave a client in the connection manager"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_server_allows_existing_room_join() {
    let server = create_test_server().await;
    let (creator, _creator_rx) = register_client(&server, "127.0.0.1:48011".parse().unwrap()).await;
    server
        .database
        .create_room(
            "test-game".to_string(),
            Some("EXIST1".to_string()),
            4,
            true,
            creator,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");

    let drain = server.begin_shutdown_drain();
    assert!(
        drain.started_by_this_call,
        "test must transition the server into draining"
    );

    let (joiner, mut joiner_rx) =
        register_client(&server, "127.0.0.1:48012".parse().unwrap()).await;
    server
        .handle_join_room(
            &joiner,
            "test-game".to_string(),
            Some("EXIST1".to_string()),
            "joiner".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;

    let response = timeout(Duration::from_secs(1), joiner_rx.recv())
        .await
        .expect("channel still open")
        .expect("join response present");
    match response.as_ref() {
        ServerMessage::RoomJoined(payload) => {
            assert_eq!(payload.room_code, "EXIST1");
            assert_eq!(payload.player_id, joiner);
        }
        other => panic!("expected RoomJoined, got {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn join_into_full_room_classifies_as_room_full_not_creation_failed() {
    // Regression guard for error-code classification (the same class fixed in
    // `ready_state.rs`/`PlayerReadyError`): a join rejected because the room is
    // at capacity is a BUSINESS rejection and MUST surface as `ROOM_FULL` — so a
    // client knows to try a different room — never the catch-all
    // `ROOM_CREATION_FAILED`, which signals a transient/infra fault a client
    // would (wrongly) retry against the same full room. This also keeps the
    // join path consistent with the reconnection path, which already maps a full
    // room to `ROOM_FULL`. See `JoinRoomError`.
    let server = create_test_server().await;

    // Player 1 creates a room capped at a single seat → full once the creator is
    // seated (room creation seats the creator).
    let (creator, mut creator_rx) =
        register_client(&server, "127.0.0.1:48002".parse().unwrap()).await;
    server
        .handle_join_room(
            &creator,
            "test-game".to_string(),
            Some("FULLRM".to_string()),
            "creator".to_string(),
            Some(1),
            Some(false),
            None,
        )
        .await;
    match timeout(Duration::from_secs(1), creator_rx.recv())
        .await
        .expect("channel still open")
        .expect("creator join response present")
        .as_ref()
    {
        ServerMessage::RoomJoined(_) => {}
        other => panic!("creator expected RoomJoined, got {other:?}"),
    }

    // Player 2 attempts to join the now-full room by its code.
    let (joiner, mut joiner_rx) =
        register_client(&server, "127.0.0.1:48003".parse().unwrap()).await;
    server
        .handle_join_room(
            &joiner,
            "test-game".to_string(),
            Some("FULLRM".to_string()),
            "joiner".to_string(),
            Some(1),
            Some(false),
            None,
        )
        .await;

    match timeout(Duration::from_secs(1), joiner_rx.recv())
        .await
        .expect("channel still open")
        .expect("joiner failure message present")
        .as_ref()
    {
        ServerMessage::RoomJoinFailed { error_code, .. } => {
            assert_eq!(
                *error_code,
                Some(ErrorCode::RoomFull),
                "a full-room join must classify as ROOM_FULL, not ROOM_CREATION_FAILED"
            );
        }
        other => panic!("joiner expected RoomJoinFailed, got {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn maintenance_cleanup_removes_expired_reconnections() {
    let server = create_test_server_with_config(ServerConfig {
        reconnection_window: Duration::ZERO,
        ..ServerConfig::default()
    })
    .await;
    let player_id = PlayerId::new_v4();
    let room_id = uuid::Uuid::new_v4();
    let reconnection_manager = server
        .reconnection_manager()
        .expect("reconnection enabled for test server");

    let _token = reconnection_manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;
    assert!(
        reconnection_manager
            .has_pending_reconnection(&player_id)
            .await,
        "test setup should create a pending reconnection"
    );
    assert_eq!(
        server
            .metrics
            .reconnection_sessions_active
            .load(Ordering::Relaxed),
        1
    );

    let cleaned = server.cleanup_expired_reconnections().await;

    assert_eq!(cleaned, 1);
    assert!(
        !reconnection_manager
            .has_pending_reconnection(&player_id)
            .await,
        "maintenance cleanup should remove expired reconnection records"
    );
    assert_eq!(
        server
            .metrics
            .reconnection_sessions_active
            .load(Ordering::Relaxed),
        0
    );
}
