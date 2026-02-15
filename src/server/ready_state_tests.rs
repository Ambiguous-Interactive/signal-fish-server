use crate::config::{
    AuthMaintenanceConfig, CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig,
    TransportSecurityConfig,
};
use crate::database::DatabaseConfig;
use crate::protocol::{ErrorCode, ServerMessage};
use crate::server::{EnhancedGameServer, ServerConfig};
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
async fn handle_player_ready_without_room_returns_not_in_room_error() {
    let server = create_test_server().await;
    let (sender, mut receiver) = mpsc::channel(4);
    let addr: SocketAddr = "127.0.0.1:49000".parse().unwrap();
    let player_id = server
        .connection_manager
        .register_client(sender, addr, server.instance_id)
        .await
        .expect("client registration succeeds");

    server.handle_player_ready(&player_id).await;

    let response = timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("channel still open")
        .expect("message present");

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
