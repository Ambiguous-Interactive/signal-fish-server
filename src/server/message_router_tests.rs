use crate::config::{
    CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig, SessionConfig,
    TransportSecurityConfig, TurnConfig,
};
use crate::database::{DatabaseConfig, InMemoryDatabase};
use crate::protocol::{
    ClientMessage, PlayerId, RoomId, RoomOperationRequest, RoomOperationResult, ServerMessage,
    Topology, Transport,
};
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
        CoordinationConfig::default(),
        TransportSecurityConfig::default(),
        Vec::new(),
    )
    .await
    .expect("failed to construct test server")
}

async fn next_routed_test_message(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    context: &str,
) -> Arc<ServerMessage> {
    timeout(Duration::from_secs(1), receiver.recv())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {context}"))
        .unwrap_or_else(|| panic!("channel closed waiting for {context}"))
}

async fn next_room_operation_result(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    operation_id: uuid::Uuid,
    context: &str,
) -> Arc<ServerMessage> {
    loop {
        let message = next_routed_test_message(receiver, context).await;
        if matches!(
            message.as_ref(),
            ServerMessage::RoomOperationResult {
                operation_id: received,
                ..
            } if *received == operation_id
        ) {
            return message;
        }
    }
}

async fn drain_until_routed_player_joined(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    player_id: PlayerId,
    context: &str,
) {
    loop {
        let message = next_routed_test_message(receiver, context).await;
        match message.as_ref() {
            ServerMessage::PlayerJoined { player } if player.id == player_id => return,
            ServerMessage::PeerTransportStatus { .. } => {
                panic!("unexpected PeerTransportStatus while waiting for {context}: {message:?}")
            }
            _ => {}
        }
    }
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
                requested_capabilities: None,
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
async fn transport_status_dedup_is_scoped_to_membership_generation() {
    let server = create_test_server().await;
    let (sender, _receiver) = mpsc::channel(4);
    let addr: SocketAddr = "127.0.0.1:50063".parse().unwrap();
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

    let status = (Transport::WebRtc, true);
    assert_eq!(
        server.set_client_transport_status(&player_id, status.0, status.1),
        TransportStatusUpdate::Changed
    );
    assert_eq!(
        server.set_client_transport_status(&player_id, status.0, status.1),
        TransportStatusUpdate::Duplicate,
        "same-generation duplicate must remain suppressed"
    );

    let room_a = RoomId::new_v4();
    let (_, prepared_stamp) = server
        .connection_manager
        .prepare_client_to_room(&player_id, room_a)
        .expect("prepare room A membership");
    assert_eq!(
        server.client_transport_status(&player_id),
        None,
        "a prepared membership must not expose the prior generation's status"
    );
    server
        .connection_manager
        .rollback_prepared_room_assignment(&player_id, room_a, prepared_stamp.epoch)
        .expect("roll back unpublished room A membership");
    assert_eq!(
        server.client_transport_status(&player_id),
        Some(status),
        "a failed prepared membership must restore the prior dedup generation"
    );
    assert_eq!(
        server.set_client_transport_status(&player_id, status.0, status.1),
        TransportStatusUpdate::Duplicate
    );

    server
        .connection_manager
        .assign_client_to_room(&player_id, room_a)
        .await;
    assert_eq!(
        server.set_client_transport_status(&player_id, status.0, status.1),
        TransportStatusUpdate::Changed,
        "room A's first report must be fresh"
    );
    assert_eq!(
        server.set_client_transport_status(&player_id, status.0, status.1),
        TransportStatusUpdate::Duplicate
    );

    server
        .connection_manager
        .clear_room_assignment(&player_id)
        .expect("leave room A");
    assert_eq!(server.client_transport_status(&player_id), None);
    assert_eq!(
        server.set_client_transport_status(&player_id, status.0, status.1),
        TransportStatusUpdate::Changed,
        "roomless membership generation starts with no dedup baseline"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(miri, ignore)]
async fn transport_status_is_ordered_before_concurrent_leave() {
    let server = create_test_server().await;
    let protocol = NegotiatedProtocol {
        version: 3,
        transports: vec![Transport::Relay, Transport::WebRtc],
        topologies: vec![Topology::Relay, Topology::Mesh],
    };

    let (observer_sender, mut observer_rx) = mpsc::channel(16);
    let observer = server
        .connection_manager
        .register_client(
            observer_sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:50064".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("observer registration succeeds");
    server.set_client_protocol(&observer, protocol.clone());
    server
        .handle_client_message(
            &observer,
            ClientMessage::JoinRoom {
                game_name: "transport-ordering".to_string(),
                room_code: None,
                player_name: "Observer".to_string(),
                max_players: Some(3),
                supports_authority: Some(false),
                relay_transport: None,
            },
        )
        .await;
    let room_code = match next_routed_test_message(&mut observer_rx, "observer RoomJoined")
        .await
        .as_ref()
    {
        ServerMessage::RoomJoined(payload) => payload.room_code.clone(),
        message => panic!("expected observer RoomJoined, got {message:?}"),
    };

    let (reporter_sender, mut reporter_rx) = mpsc::channel(16);
    let reporter = server
        .connection_manager
        .register_client(
            reporter_sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:50065".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("reporter registration succeeds");
    server.set_client_protocol(&reporter, protocol);
    let join_message = || ClientMessage::JoinRoom {
        game_name: "transport-ordering".to_string(),
        room_code: Some(room_code.clone()),
        player_name: "Reporter".to_string(),
        max_players: Some(3),
        supports_authority: Some(false),
        relay_transport: None,
    };
    server
        .handle_client_message(&reporter, join_message())
        .await;
    assert!(matches!(
        next_routed_test_message(&mut reporter_rx, "reporter RoomJoined")
            .await
            .as_ref(),
        ServerMessage::RoomJoined(_)
    ));
    drain_until_routed_player_joined(&mut observer_rx, reporter, "observer PlayerJoined").await;
    server
        .handle_client_message(
            &reporter,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: true,
            },
        )
        .await;
    assert!(matches!(
        next_routed_test_message(&mut observer_rx, "initial PeerTransportStatus").await.as_ref(),
        ServerMessage::PeerTransportStatus { peer_id, connected: true, .. } if *peer_id == reporter
    ));

    // Poll both production handlers to their lifecycle wait point in a known
    // order. The lifecycle gate spans only handler processing (transport
    // status releases it before fan-out dispatch), so this oracle rests on
    // `join!`'s in-order poll discipline: after the guard drops, the status
    // future completes its (uncontended, healthy-queue) publication within
    // its poll, before leave is first polled to clear membership.
    let lifecycle = server
        .connection_manager
        .client_lifecycle(&reporter)
        .expect("reporter lifecycle");
    let lifecycle_probe = super::message_router::arm_transport_status_lifecycle_probe(reporter);
    let lifecycle_guard = lifecycle.lock().await;
    let status_before_leave = server.handle_client_message(
        &reporter,
        ClientMessage::TransportStatus {
            transport: Transport::WebRtc,
            connected: false,
        },
    );
    tokio::pin!(status_before_leave);
    assert!(matches!(
        futures_util::poll!(&mut status_before_leave),
        std::task::Poll::Pending
    ));
    assert!(
        lifecycle_probe.load(Ordering::Acquire),
        "status handler must reach its lifecycle-lock request before leave is polled"
    );
    let leave_after_status = server.handle_client_message(&reporter, ClientMessage::LeaveRoom);
    tokio::pin!(leave_after_status);
    assert!(matches!(
        futures_util::poll!(&mut leave_after_status),
        std::task::Poll::Pending
    ));
    drop(lifecycle_guard);
    tokio::join!(status_before_leave, leave_after_status);
    super::message_router::disarm_transport_status_lifecycle_probe(&reporter);

    assert!(matches!(
        next_routed_test_message(&mut observer_rx, "ordered PeerTransportStatus").await.as_ref(),
        ServerMessage::PeerTransportStatus { peer_id, connected: false, .. } if *peer_id == reporter
    ));
    assert!(matches!(
        next_routed_test_message(&mut observer_rx, "ordered PlayerLeft").await.as_ref(),
        ServerMessage::PlayerLeft { player_id, .. } if *player_id == reporter
    ));
    assert!(matches!(
        next_routed_test_message(&mut reporter_rx, "ordered RoomLeft")
            .await
            .as_ref(),
        ServerMessage::RoomLeft
    ));
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

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn correlated_room_operations_echo_ids_and_reject_stale_responses() {
    let server = create_test_server().await;
    let (sender, mut receiver) = mpsc::channel(16);
    let injection_sender = sender.clone();
    let player_id = server
        .connection_manager
        .register_client(
            sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:50070".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("client registration succeeds");
    server.set_client_room_operation_ids(&player_id, true);

    let old_id = uuid::Uuid::from_u128(1);
    server
        .handle_client_message(
            &player_id,
            ClientMessage::RoomOperation {
                operation_id: old_id,
                operation: Box::new(RoomOperationRequest::JoinRoom {
                    game_name: "game".to_string(),
                    room_code: None,
                    player_name: String::new(),
                    max_players: Some(2),
                    supports_authority: Some(true),
                    relay_transport: None,
                }),
            },
        )
        .await;
    let stale = next_routed_test_message(&mut receiver, "old correlated join failure").await;
    assert!(matches!(
        stale.as_ref(),
        ServerMessage::RoomOperationResult { operation_id, result }
            if *operation_id == old_id
                && matches!(result.as_ref(), RoomOperationResult::RoomJoinFailed { .. })
    ));

    let pending_id = uuid::Uuid::from_u128(2);
    let observe_pending_response = |pending: &mut Option<uuid::Uuid>, message: &ServerMessage| {
        let matches_pending = matches!(
            message,
            ServerMessage::RoomOperationResult { operation_id, .. }
                if Some(*operation_id) == *pending
        );
        if matches_pending {
            *pending = None;
        }
        matches_pending
    };
    let database = server
        .database()
        .as_any()
        .downcast_ref::<InMemoryDatabase>()
        .expect("test server uses in-memory database");
    database.pause_next_get_room_by_id_for_test();
    let pending_join = {
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            server
                .handle_client_message(
                    &player_id,
                    ClientMessage::RoomOperation {
                        operation_id: pending_id,
                        operation: Box::new(RoomOperationRequest::JoinRoom {
                            game_name: "game".to_string(),
                            room_code: None,
                            player_name: "Alice".to_string(),
                            max_players: Some(2),
                            supports_authority: Some(true),
                            relay_transport: None,
                        }),
                    },
                )
                .await;
        })
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        database.wait_for_paused_get_room_by_id_for_test(),
    )
    .await
    .expect("newer join reaches the deterministic pending gate");
    let mut pending_operation = Some(pending_id);
    let membership_before_injection = server.get_client_room(&player_id).await;
    injection_sender
        .send(Arc::clone(&stale))
        .await
        .expect("inject duplicate old response through the client channel");
    let injected =
        next_routed_test_message(&mut receiver, "old response while newer join is pending").await;
    assert!(
        !observe_pending_response(&mut pending_operation, injected.as_ref()),
        "an injected old same-kind response must not clear the newer operation fence"
    );
    assert_eq!(pending_operation, Some(pending_id));
    assert_eq!(
        server.get_client_room(&player_id).await,
        membership_before_injection,
        "the injected stale response must not mutate the pending join's provisional membership"
    );
    assert!(
        !pending_join.is_finished(),
        "the newer join remains pending"
    );

    database.release_paused_get_room_by_id_for_test();
    pending_join
        .await
        .expect("newer join task must not panic after gate release");
    let current = next_routed_test_message(&mut receiver, "new correlated join result").await;
    assert!(matches!(
        current.as_ref(),
        ServerMessage::RoomOperationResult { operation_id, result }
            if *operation_id == pending_id
                && matches!(result.as_ref(), RoomOperationResult::RoomJoined(_))
    ));
    assert!(observe_pending_response(
        &mut pending_operation,
        current.as_ref()
    ));
    assert_eq!(pending_operation, None);

    let leave_id = uuid::Uuid::from_u128(3);
    server
        .handle_client_message(
            &player_id,
            ClientMessage::RoomOperation {
                operation_id: leave_id,
                operation: Box::new(RoomOperationRequest::LeaveRoom),
            },
        )
        .await;
    assert!(matches!(
        next_room_operation_result(&mut receiver, leave_id, "correlated RoomLeft")
            .await
            .as_ref(),
        ServerMessage::RoomOperationResult { operation_id, result }
            if *operation_id == leave_id
                && matches!(result.as_ref(), RoomOperationResult::RoomLeft)
    ));

    let second_leave_id = uuid::Uuid::from_u128(4);
    server
        .handle_client_message(
            &player_id,
            ClientMessage::RoomOperation {
                operation_id: second_leave_id,
                operation: Box::new(RoomOperationRequest::LeaveRoom),
            },
        )
        .await;
    assert!(matches!(
        next_room_operation_result(&mut receiver, second_leave_id, "correlated leave failure")
            .await
            .as_ref(),
        ServerMessage::RoomOperationResult { operation_id, result }
            if *operation_id == second_leave_id
                && matches!(result.as_ref(), RoomOperationResult::OperationFailed { .. })
    ));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn room_operation_requires_explicit_capability_negotiation() {
    let server = create_test_server().await;
    let (sender, mut receiver) = mpsc::channel(4);
    let player_id = server
        .connection_manager
        .register_client(
            sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:50071".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("client registration succeeds");
    server
        .handle_client_message(
            &player_id,
            ClientMessage::RoomOperation {
                operation_id: uuid::Uuid::from_u128(9),
                operation: Box::new(RoomOperationRequest::LeaveRoom),
            },
        )
        .await;
    assert!(matches!(
        next_routed_test_message(&mut receiver, "unnegotiated operation error")
            .await
            .as_ref(),
        ServerMessage::Error {
            error_code: Some(crate::protocol::ErrorCode::UnsupportedProtocolVersion),
            ..
        }
    ));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn correlated_spectator_operations_echo_success_and_failure_ids() {
    let server = create_test_server().await;
    let (creator_tx, mut creator_rx) = mpsc::channel(16);
    let creator = server
        .connection_manager
        .register_client(
            creator_tx,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:50072".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("creator registration succeeds");
    server
        .handle_client_message(
            &creator,
            ClientMessage::JoinRoom {
                game_name: "spectator-operations".to_string(),
                room_code: None,
                player_name: "Creator".to_string(),
                max_players: Some(2),
                supports_authority: Some(true),
                relay_transport: None,
            },
        )
        .await;
    let room_code = match next_routed_test_message(&mut creator_rx, "creator RoomJoined")
        .await
        .as_ref()
    {
        ServerMessage::RoomJoined(payload) => payload.room_code.clone(),
        message => panic!("expected RoomJoined, got {message:?}"),
    };

    let (spectator_tx, mut spectator_rx) = mpsc::channel(16);
    let spectator = server
        .connection_manager
        .register_client(
            spectator_tx,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:50073".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("spectator registration succeeds");
    server.set_client_room_operation_ids(&spectator, true);

    let missing_id = uuid::Uuid::from_u128(10);
    server
        .handle_client_message(
            &spectator,
            ClientMessage::RoomOperation {
                operation_id: missing_id,
                operation: Box::new(RoomOperationRequest::JoinAsSpectator {
                    game_name: "spectator-operations".to_string(),
                    room_code: "ZZZZZZ".to_string(),
                    spectator_name: "Watcher".to_string(),
                }),
            },
        )
        .await;
    assert!(matches!(
        next_routed_test_message(&mut spectator_rx, "correlated spectator join failure").await.as_ref(),
        ServerMessage::RoomOperationResult { operation_id, result }
            if *operation_id == missing_id
                && matches!(result.as_ref(), RoomOperationResult::SpectatorJoinFailed { .. })
    ));

    let join_id = uuid::Uuid::from_u128(11);
    server
        .handle_client_message(
            &spectator,
            ClientMessage::RoomOperation {
                operation_id: join_id,
                operation: Box::new(RoomOperationRequest::JoinAsSpectator {
                    game_name: "spectator-operations".to_string(),
                    room_code,
                    spectator_name: "Watcher".to_string(),
                }),
            },
        )
        .await;
    assert!(matches!(
        next_routed_test_message(&mut spectator_rx, "correlated SpectatorJoined").await.as_ref(),
        ServerMessage::RoomOperationResult { operation_id, result }
            if *operation_id == join_id
                && matches!(result.as_ref(), RoomOperationResult::SpectatorJoined(_))
    ));

    let leave_id = uuid::Uuid::from_u128(12);
    server
        .handle_client_message(
            &spectator,
            ClientMessage::RoomOperation {
                operation_id: leave_id,
                operation: Box::new(RoomOperationRequest::LeaveSpectator),
            },
        )
        .await;
    assert!(matches!(
        next_routed_test_message(&mut spectator_rx, "correlated SpectatorLeft").await.as_ref(),
        ServerMessage::RoomOperationResult { operation_id, result }
            if *operation_id == leave_id
                && matches!(result.as_ref(), RoomOperationResult::SpectatorLeft { .. })
    ));

    let second_leave_id = uuid::Uuid::from_u128(13);
    server
        .handle_client_message(
            &spectator,
            ClientMessage::RoomOperation {
                operation_id: second_leave_id,
                operation: Box::new(RoomOperationRequest::LeaveSpectator),
            },
        )
        .await;
    assert!(matches!(
        next_routed_test_message(&mut spectator_rx, "correlated spectator leave failure").await.as_ref(),
        ServerMessage::RoomOperationResult { operation_id, result }
            if *operation_id == second_leave_id
                && matches!(result.as_ref(), RoomOperationResult::OperationFailed { .. })
    ));
}
