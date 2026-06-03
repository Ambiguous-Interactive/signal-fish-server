use crate::config::{
    AuthMaintenanceConfig, CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig,
    TransportSecurityConfig,
};
use crate::database::DatabaseConfig;
use crate::protocol::{ClientMessage, ServerMessage};
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
#[cfg_attr(miri, ignore)]
async fn delayed_authenticate_is_rejected_with_warning_only() {
    let server = create_test_server().await;
    let (sender, mut receiver) = mpsc::channel(4);
    let addr: SocketAddr = "127.0.0.1:50000".parse().unwrap();
    let player_id = server
        .connection_manager
        .register_client(sender, addr, server.instance_id)
        .await
        .expect("client registration succeeds");

    server
        .handle_client_message(
            &player_id,
            ClientMessage::Authenticate {
                app_id: "ignored".to_string(),
                sdk_version: None,
                platform: None,
                game_data_format: None,
                protocol_version: None,
                supported_transports: None,
                supported_topologies: None,
            },
        )
        .await;

    assert!(
        timeout(Duration::from_millis(100), receiver.recv())
            .await
            .unwrap_or(None)
            .is_none(),
        "authenticate after registration should not send a response"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn client_protocol_round_trips_through_server() {
    use crate::protocol::{Topology, Transport};
    use crate::server::NegotiatedProtocol;

    let server = create_test_server().await;
    let (sender, _receiver) = mpsc::channel(4);
    let addr: SocketAddr = "127.0.0.1:50050".parse().unwrap();
    let player_id = server
        .connection_manager
        .register_client(sender, addr, server.instance_id)
        .await
        .expect("client registration succeeds");

    // Defaults: pure v2, relay-only.
    assert!(!server.client_supports_v3(&player_id));
    let default_proto = server.client_protocol(&player_id);
    assert_eq!(default_proto.version, 2);
    assert!(server.client_supports_transport(&player_id, Transport::Relay));
    assert!(!server.client_supports_transport(&player_id, Transport::WebRtc));

    // After negotiating v3 + webrtc the pass-throughs reflect it.
    server.set_client_protocol(
        &player_id,
        NegotiatedProtocol {
            version: 3,
            transports: vec![Transport::Relay, Transport::WebRtc],
            topologies: vec![Topology::Relay, Topology::Mesh],
        },
    );
    assert!(server.client_supports_v3(&player_id));
    assert!(server.client_supports_transport(&player_id, Transport::WebRtc));
    let proto = server.client_protocol(&player_id);
    assert_eq!(proto.version, 3);
    assert_eq!(proto.topologies, vec![Topology::Relay, Topology::Mesh]);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn join_room_request_is_forwarded_to_room_service() {
    let server = create_test_server().await;
    let (sender, mut receiver) = mpsc::channel(4);
    let addr: SocketAddr = "127.0.0.1:50001".parse().unwrap();
    let player_id = server
        .connection_manager
        .register_client(sender, addr, server.instance_id)
        .await
        .expect("client registration succeeds");

    server
        .handle_client_message(
            &player_id,
            ClientMessage::JoinRoom {
                game_name: "game".to_string(),
                room_code: None,
                player_name: "Player".to_string(),
                max_players: Some(2),
                supports_authority: Some(true),
                relay_transport: None,
            },
        )
        .await;

    let response = timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("channel still open")
        .expect("room join response present");

    match response.as_ref() {
        ServerMessage::RoomJoined(ref p) => {
            assert_eq!(p.player_id, player_id);
        }
        other => panic!("unexpected join response: {other:?}"),
    }
}
