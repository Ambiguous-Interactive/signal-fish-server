use super::*;
use crate::config::{
    AuthMaintenanceConfig, CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig,
    SessionConfig, TransportSecurityConfig,
};
use crate::database::DatabaseConfig;
use crate::protocol::{ErrorCode, PlayerId, ServerMessage};
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
        .register_client(sender, addr, server.instance_id)
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
        .register_client(sender, addr, server.instance_id)
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
        .register_disconnection(player_id, room_id, false, None)
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
