use super::*;
use crate::config::{
    AuthMaintenanceConfig, CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig,
    TransportSecurityConfig,
};
use crate::database::DatabaseConfig;
use crate::protocol::ServerMessage;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

async fn create_test_server() -> Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
        ServerConfig::default(),
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

#[tokio::test]
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
