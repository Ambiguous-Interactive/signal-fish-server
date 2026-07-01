use crate::config::{
    AuthMaintenanceConfig, CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig,
    SessionConfig, TransportSecurityConfig, TurnConfig,
};
use crate::database::DatabaseConfig;
use crate::protocol::{ClientMessage, PlayerId, ServerMessage, Topology, Transport};
use crate::server::{EnhancedGameServer, NegotiatedProtocol, ServerConfig, TransportStatusUpdate};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

async fn create_test_server() -> Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
        ServerConfig::default(),
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

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn delayed_authenticate_is_rejected_with_warning_only() {
    let server = create_test_server().await;
    let (sender, mut receiver) = mpsc::channel(4);
    let addr: SocketAddr = "127.0.0.1:50000".parse().unwrap();
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

    match timeout(Duration::from_millis(100), receiver.recv()).await {
        Err(_) => {}
        Ok(Some(message)) => {
            panic!("authenticate after registration should not send a response, got {message:?}")
        }
        Ok(None) => {
            panic!("channel closed while checking authenticate-after-registration silence")
        }
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn client_protocol_round_trips_through_server() {
    let server = create_test_server().await;
    let (sender, _receiver) = mpsc::channel(4);
    let addr: SocketAddr = "127.0.0.1:50050".parse().unwrap();
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
async fn duplicate_transport_status_reports_do_not_inflate_metrics() {
    let server = create_test_server().await;
    let (sender, _receiver) = mpsc::channel(4);
    let addr: SocketAddr = "127.0.0.1:50060".parse().unwrap();
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

    server.set_client_protocol(
        &player_id,
        NegotiatedProtocol {
            version: 3,
            transports: vec![Transport::Relay, Transport::WebRtc],
            topologies: vec![Topology::Relay, Topology::Mesh],
        },
    );

    for connected in [true, true, false, false, true] {
        server
            .handle_client_message(
                &player_id,
                ClientMessage::TransportStatus {
                    transport: Transport::WebRtc,
                    connected,
                },
            )
            .await;
    }

    assert_eq!(
        server.client_transport_status(&player_id),
        Some((Transport::WebRtc, true)),
        "the last reported transport state should remain available"
    );
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        2,
        "only first connected report and the reconnect transition count as P2P events"
    );
    assert_eq!(
        server.metrics.relay_fallback.load(Ordering::Relaxed),
        1,
        "duplicate fallback reports must not inflate relay fallback events"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_for_unnegotiated_transport_is_ignored() {
    let server = create_test_server().await;
    let (sender, mut receiver) = mpsc::channel(4);
    let addr: SocketAddr = "127.0.0.1:50062".parse().unwrap();
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

    server.set_client_protocol(
        &player_id,
        NegotiatedProtocol {
            version: 3,
            transports: vec![Transport::Relay],
            topologies: vec![Topology::Relay],
        },
    );

    server
        .handle_client_message(
            &player_id,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: true,
            },
        )
        .await;

    match timeout(Duration::from_millis(100), receiver.recv()).await {
        Err(_) => {}
        Ok(Some(message)) => {
            panic!("unnegotiated TransportStatus should not send a response, got {message:?}")
        }
        Ok(None) => panic!("channel closed while checking unnegotiated TransportStatus silence"),
    }

    assert_eq!(
        server.client_transport_status(&player_id),
        None,
        "an unnegotiated transport report must not update per-connection state"
    );
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        0,
        "an unnegotiated p2p report must not move p2p_established"
    );
    assert_eq!(
        server.metrics.relay_fallback.load(Ordering::Relaxed),
        0,
        "an unnegotiated transport report must not move relay_fallback"
    );

    server
        .handle_client_message(
            &player_id,
            ClientMessage::TransportStatus {
                transport: Transport::Relay,
                connected: true,
            },
        )
        .await;

    assert_eq!(
        server.client_transport_status(&player_id),
        Some((Transport::Relay, true)),
        "a negotiated relay report should still update per-connection state"
    );

    server
        .handle_client_message(
            &player_id,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: false,
            },
        )
        .await;

    match timeout(Duration::from_millis(100), receiver.recv()).await {
        Err(_) => {}
        Ok(Some(message)) => {
            panic!("unnegotiated fallback report should not send a response, got {message:?}")
        }
        Ok(None) => panic!("channel closed while checking unnegotiated fallback silence"),
    }

    assert_eq!(
        server.client_transport_status(&player_id),
        Some((Transport::Relay, true)),
        "an unnegotiated fallback report must not replace the last valid transport state"
    );
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        0,
        "an unnegotiated fallback report must not move p2p_established"
    );
    assert_eq!(
        server.metrics.relay_fallback.load(Ordering::Relaxed),
        0,
        "an unnegotiated fallback report must not move relay_fallback"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_update_results_are_distinct() {
    let server = create_test_server().await;
    let missing_player_id = PlayerId::new_v4();

    assert_eq!(
        server.set_client_transport_status(&missing_player_id, Transport::WebRtc, true),
        TransportStatusUpdate::MissingConnection
    );

    let (sender, _receiver) = mpsc::channel(4);
    let addr: SocketAddr = "127.0.0.1:50061".parse().unwrap();
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

    assert_eq!(
        server.set_client_transport_status(&player_id, Transport::WebRtc, true),
        TransportStatusUpdate::UnsupportedProtocolVersion
    );

    server.set_client_protocol(
        &player_id,
        NegotiatedProtocol {
            version: 3,
            transports: vec![Transport::Relay],
            topologies: vec![Topology::Relay],
        },
    );

    assert_eq!(
        server.set_client_transport_status(&player_id, Transport::WebRtc, true),
        TransportStatusUpdate::UnsupportedTransport
    );
    assert_eq!(
        server.set_client_transport_status(&player_id, Transport::Relay, true),
        TransportStatusUpdate::Changed
    );

    server.set_client_protocol(
        &player_id,
        NegotiatedProtocol {
            version: 3,
            transports: vec![Transport::Relay, Transport::WebRtc],
            topologies: vec![Topology::Relay, Topology::Mesh],
        },
    );

    assert_eq!(
        server.set_client_transport_status(&player_id, Transport::WebRtc, true),
        TransportStatusUpdate::Changed
    );
    assert_eq!(
        server.set_client_transport_status(&player_id, Transport::WebRtc, true),
        TransportStatusUpdate::Duplicate
    );
    assert_eq!(
        server.set_client_transport_status(&player_id, Transport::WebRtc, false),
        TransportStatusUpdate::Changed
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn join_room_request_is_forwarded_to_room_service() {
    let server = create_test_server().await;
    let (sender, mut receiver) = mpsc::channel(4);
    let addr: SocketAddr = "127.0.0.1:50001".parse().unwrap();
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
