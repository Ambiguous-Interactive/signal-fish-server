use super::*;
use crate::config::{
    CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig, SessionConfig,
    TransportSecurityConfig, TurnConfig,
};
use crate::coordination::{CloseReason, ConnectionCloseSignal};
use crate::protocol::{PlayerId, RoomOperationId, RoomOperationResult, ServerMessage};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

/// #826: admitted roomless replies must survive canceled observers.
#[tokio::test(start_paused = true)]
async fn canceled_ping_retains_response_and_source_order() {
    qualify_canceled_roomless_reply("ping").await;
}

#[tokio::test(start_paused = true)]
async fn canceled_roomless_ready_retains_response_and_source_order() {
    qualify_canceled_roomless_reply("ready").await;
}

#[tokio::test(start_paused = true)]
async fn canceled_roomless_start_retains_response_and_source_order() {
    qualify_canceled_roomless_reply("start").await;
}

#[tokio::test(start_paused = true)]
async fn canceled_roomless_authority_retains_response_and_source_order() {
    qualify_canceled_roomless_reply("authority").await;
}

#[tokio::test(start_paused = true)]
async fn canceled_roomless_signal_retains_response_and_source_order() {
    qualify_canceled_roomless_reply("signal").await;
}

#[tokio::test(start_paused = true)]
async fn canceled_roomless_relay_retains_response_and_source_order() {
    qualify_canceled_roomless_reply("relay").await;
}

#[tokio::test(start_paused = true)]
async fn canceled_invalid_relay_preserves_live_mesh_signaling() {
    qualify_canceled_control_preserves_live_mesh("relay-invalid").await;
}

#[tokio::test(start_paused = true)]
async fn canceled_roomless_binary_relay_retains_response_and_source_order() {
    qualify_canceled_roomless_reply("relay-binary").await;
}

#[tokio::test(start_paused = true)]
async fn canceled_invalid_binary_relay_preserves_live_mesh_signaling() {
    qualify_canceled_control_preserves_live_mesh("relay-invalid-binary").await;
}

async fn qualify_canceled_roomless_reply(command: &'static str) {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (actor, mut receiver) = register_client(&server, "127.0.0.1:49305".parse().unwrap()).await;
    for _ in 0..8 {
        server
            .message_coordinator
            .send_to_player(&actor, Arc::new(ServerMessage::Pong))
            .await
            .unwrap();
    }
    let caller = tokio::spawn({
        let server = Arc::clone(&server);
        async move {
            match command {
                "relay-binary" => {
                    server
                        .handle_game_data_binary(
                            &actor,
                            crate::protocol::GameDataEncoding::MessagePack,
                            bytes::Bytes::from_static(b"exact-binary-refusal"),
                        )
                        .await;
                }
                "relay" => {
                    server
                        .handle_game_data(&actor, serde_json::json!({"tick": 1}), None, None)
                        .await;
                }
                "ping" => server.handle_ping(&actor).await,
                "ready" => server.handle_player_ready(&actor).await,
                "start" => server.handle_start_game(&actor).await,
                "authority" => server.handle_authority_request(&actor, true).await,
                "signal" => {
                    server
                        .handle_signal(
                            &actor,
                            PlayerId::new_v4(),
                            serde_json::json!({"candidate": "exact"}),
                        )
                        .await
                }
                _ => unreachable!(),
            }
        }
    });
    timeout(Duration::from_secs(1), async {
        while server
            .metrics
            .websocket_backpressure_events
            .load(Ordering::Acquire)
            == 0
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("roomless response reaches actual backpressure");
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    let lifecycle = server.connection_manager.client_lifecycle(&actor).unwrap();
    let mut next = Box::pin(lifecycle.lock());
    assert!(
        futures_util::poll!(&mut next).is_pending(),
        "{command}: cancellation released source order before the response"
    );
    drop(next);
    let ping = tokio::spawn({
        let server = Arc::clone(&server);
        async move { server.handle_ping(&actor).await }
    });
    for _ in 0..8 {
        assert!(matches!(
            receiver.recv().await.unwrap().as_ref(),
            ServerMessage::Pong
        ));
    }
    let result = timeout(Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    match command {
        "ping" => assert!(matches!(result.as_ref(), ServerMessage::Pong)),
        "authority" => assert!(matches!(
            result.as_ref(),
            ServerMessage::AuthorityResponse {
                granted: false,
                error_code: Some(ErrorCode::NotInRoom),
                ..
            }
        )),
        _ => assert!(matches!(
            result.as_ref(),
            ServerMessage::Error {
                error_code: Some(ErrorCode::NotInRoom),
                ..
            }
        )),
    }
    timeout(Duration::from_secs(1), ping)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        receiver.recv().await.unwrap().as_ref(),
        ServerMessage::Pong
    ));
    let pending = receiver.try_recv();
    assert!(
        matches!(pending, Err(mpsc::error::TryRecvError::Empty)),
        "{command}: exactly one response per command"
    );
    let _guard = lifecycle.lock().await;
}

/// #826: a refused transfer retains its result after observer cancellation.
#[tokio::test(start_paused = true)]
async fn canceled_transfer_refusal_retains_result_and_source_order() {
    for expected in [
        ErrorCode::InvalidInput,
        ErrorCode::NotInRoom,
        ErrorCode::NotRoomAuthority,
        ErrorCode::TransferTargetNotFound,
        ErrorCode::RoomNotFound,
        ErrorCode::StorageError,
    ] {
        let server = create_test_server_with(ServerConfig::default()).await;
        let (actor, mut receiver) =
            register_client(&server, "127.0.0.1:49304".parse().unwrap()).await;
        if expected != ErrorCode::NotInRoom {
            join_seated_player(&server, &actor, &mut receiver, "TRFCAN", "host").await;
            if expected == ErrorCode::NotRoomAuthority {
                server.handle_authority_request(&actor, false).await;
            }
            if expected == ErrorCode::RoomNotFound {
                server
                    .database
                    .delete_room(&server.get_client_room(&actor).await.unwrap())
                    .await
                    .unwrap();
            }
        }
        drain_receiver(&mut receiver);
        let database = server
            .database
            .as_any()
            .downcast_ref::<crate::database::InMemoryDatabase>()
            .unwrap();
        if expected == ErrorCode::StorageError {
            database.fail_get_room_by_id_for_test(true);
        }
        let target = if expected == ErrorCode::InvalidInput {
            actor
        } else {
            PlayerId::new_v4()
        };
        for _ in 0..8 {
            server
                .message_coordinator
                .send_to_player(&actor, Arc::new(ServerMessage::Pong))
                .await
                .unwrap();
        }
        let operation_id = RoomOperationId::new_v4();
        let caller = tokio::spawn({
            let server = Arc::clone(&server);
            async move {
                server
                    .handle_transfer_authority_operation(&actor, operation_id, target)
                    .await
            }
        });
        timeout(Duration::from_secs(1), async {
            while server
                .metrics
                .websocket_backpressure_events
                .load(Ordering::Acquire)
                == 0
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("transfer refusal reaches real queue backpressure");
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        let lifecycle = server.connection_manager.client_lifecycle(&actor).unwrap();
        let mut next_operation = Box::pin(lifecycle.lock());
        assert!(
            futures_util::poll!(&mut next_operation).is_pending(),
            "the refused transfer must retain its result and source order"
        );
        drop(next_operation);
        let ping = tokio::spawn({
            let server = Arc::clone(&server);
            async move { server.handle_ping(&actor).await }
        });
        for _ in 0..8 {
            assert!(matches!(
                receiver.recv().await.unwrap().as_ref(),
                ServerMessage::Pong
            ));
        }
        let result = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(result.as_ref(), ServerMessage::RoomOperationResult {operation_id: actual, result}
        if *actual == operation_id && matches!(result.as_ref(), RoomOperationResult::OperationFailed {
            error_code: Some(actual), ..} if *actual == expected))
        );
        timeout(Duration::from_secs(1), ping)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            receiver.recv().await.unwrap().as_ref(),
            ServerMessage::Pong
        ));
        assert!(drain_receiver(&mut receiver).is_empty());
        database.fail_get_room_by_id_for_test(false);
    }
}

/// #826: cancellation during validation preserves the storage result and committed role.
#[tokio::test]
async fn canceled_transfer_finishes_its_storage_decision() {
    for storage_error in [false, true] {
        let server = create_test_server_with(ServerConfig::default()).await;
        let (actor, mut actor_rx) =
            register_client(&server, "127.0.0.1:49305".parse().unwrap()).await;
        let (target, mut target_rx) =
            register_client(&server, "127.0.0.1:49306".parse().unwrap()).await;
        join_seated_player(&server, &actor, &mut actor_rx, "TRWAIT", "host").await;
        join_seated_player(&server, &target, &mut target_rx, "TRWAIT", "guest").await;
        drain_receiver(&mut actor_rx);
        drain_receiver(&mut target_rx);
        let room = server.get_client_room(&actor).await.unwrap();
        let database = server
            .database
            .as_any()
            .downcast_ref::<crate::database::InMemoryDatabase>()
            .unwrap();
        database.pause_next_get_room_by_id_for_test();
        let operation_id = RoomOperationId::new_v4();
        let caller = tokio::spawn({
            let server = Arc::clone(&server);
            async move {
                server
                    .handle_transfer_authority_operation(&actor, operation_id, target)
                    .await
            }
        });
        timeout(
            Duration::from_secs(1),
            database.wait_for_paused_get_room_by_id_for_test(),
        )
        .await
        .unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        database.fail_get_room_by_id_for_test(storage_error);
        database.release_paused_get_room_by_id_for_test();
        let response = recv_until(&mut actor_rx, |message| {
            matches!(message, ServerMessage::RoomOperationResult { .. })
        })
        .await;
        assert!(
            matches!(response.as_ref(), ServerMessage::RoomOperationResult {operation_id: actual, result}
            if *actual == operation_id && if storage_error {
                matches!(result.as_ref(), RoomOperationResult::OperationFailed {error_code: Some(ErrorCode::StorageError), ..})
            } else { matches!(result.as_ref(), RoomOperationResult::AuthorityTransferred {player_id} if *player_id == target) })
        );
        database.fail_get_room_by_id_for_test(false);
        assert_eq!(
            server
                .database
                .get_room_by_id(&room)
                .await
                .unwrap()
                .unwrap()
                .authority_player,
            Some(if storage_error { actor } else { target })
        );
        let target_events = drain_receiver(&mut target_rx);
        assert_eq!(target_events.len(), usize::from(!storage_error));
        if !storage_error {
            assert!(
                matches!(target_events[0].as_ref(), ServerMessage::AuthorityChanged {
                authority_player: Some(actual), you_are_authority: true } if *actual == target)
            );
        }
    }
}

/// #826: a canceled refusal must not hold healthy mesh signaling behind its reply.
#[tokio::test(start_paused = true)]
async fn canceled_transfer_refusal_preserves_live_mesh_signaling() {
    qualify_canceled_control_preserves_live_mesh("transfer").await;
}

/// #1633: a retained Pong must leave healthy peers free to signal.
#[tokio::test(start_paused = true)]
async fn canceled_ping_preserves_live_mesh_signaling() {
    qualify_canceled_control_preserves_live_mesh("ping").await;
}

#[tokio::test(start_paused = true)]
async fn canceled_signal_refusal_preserves_live_mesh_signaling() {
    qualify_canceled_control_preserves_live_mesh("signal-self").await;
}

#[tokio::test(start_paused = true)]
async fn canceled_signal_limit_refusal_preserves_live_mesh_signaling() {
    qualify_canceled_control_preserves_live_mesh("signal-rate").await;
}

async fn qualify_canceled_control_preserves_live_mesh(command: &'static str) {
    let server = create_test_server_with_session(SessionConfig {
        default_topology: crate::protocol::Topology::Mesh,
        ..SessionConfig::default()
    })
    .await;
    let (room, mut peers, generation) = live_moderation_room(&server, "TRMESH", 3).await;
    let actor = peers[1].id;
    let target = peers[0].id;
    for _ in 0..8 {
        server
            .message_coordinator
            .send_to_player(&actor, Arc::new(ServerMessage::Pong))
            .await
            .unwrap();
    }
    if command == "signal-rate" {
        for _ in 0..server.config.rate_limit_config.max_signals {
            server.rate_limiter.check_signal(&actor).await.unwrap();
        }
    }
    let operation_id = RoomOperationId::new_v4();
    let caller = tokio::spawn({
        let server = Arc::clone(&server);
        async move {
            match command {
                "relay-invalid-binary" => {
                    let lifecycle = server.connection_manager.client_lifecycle(&actor).unwrap();
                    server
                        .handle_game_data_binary_from_lifecycle(
                            &actor,
                            crate::protocol::GameDataEncoding::MessagePack,
                            bytes::Bytes::from(vec![0; server.config.max_message_size + 1]),
                            lifecycle,
                        )
                        .await;
                }
                "relay-invalid" => {
                    server
                        .handle_game_data(
                            &actor,
                            serde_json::json!({"tick": 1}),
                            Some(crate::protocol::DeliveryClass::Latest),
                            None,
                        )
                        .await;
                }
                "ping" => server.handle_ping(&actor).await,
                "signal-self" | "signal-rate" => {
                    server
                        .handle_signal_in_generation(
                            &actor,
                            if command == "signal-self" {
                                actor
                            } else {
                                target
                            },
                            generation,
                            serde_json::json!({"candidate": "exact"}),
                        )
                        .await
                }
                _ => {
                    server
                        .handle_transfer_authority_operation(&actor, operation_id, target)
                        .await
                }
            }
        }
    });
    timeout(Duration::from_secs(1), async {
        while server
            .metrics
            .websocket_backpressure_events
            .load(Ordering::Acquire)
            == 0
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    let lifecycle = server.connection_manager.client_lifecycle(&actor).unwrap();
    let mut lock = Box::pin(lifecycle.lock());
    assert!(futures_util::poll!(&mut lock).is_pending());
    drop(lock);
    let room_guard = timeout(
        Duration::from_secs(1),
        server.message_coordinator.lock_room_event_mutation(&room),
    )
    .await
    .unwrap();
    drop(room_guard);
    assert_live_moderation_signal(&server, target, &mut peers[2], generation).await;
    let sender = peers[2].id;
    assert_live_moderation_signal(&server, sender, &mut peers[0], generation).await;
    for _ in 0..8 {
        assert!(matches!(
            peers[1].receiver.recv().await.unwrap().as_ref(),
            ServerMessage::Pong
        ));
    }
    let response = timeout(Duration::from_secs(1), peers[1].receiver.recv())
        .await
        .unwrap()
        .unwrap();
    if command == "ping" {
        assert!(matches!(response.as_ref(), ServerMessage::Pong));
    } else if command.starts_with("relay-invalid") {
        let expected = if command == "relay-invalid-binary" {
            ErrorCode::MessageTooLarge
        } else {
            ErrorCode::InvalidDeliveryClass
        };
        assert!(
            matches!(response.as_ref(), ServerMessage::Error {error_code: Some(actual), ..} if *actual == expected)
        );
    } else if command.starts_with("signal") {
        let expected = if command == "signal-self" {
            ErrorCode::SignalTargetNotFound
        } else {
            ErrorCode::SignalRateLimited
        };
        assert!(
            matches!(response.as_ref(), ServerMessage::Error { error_code: Some(actual), .. }
            if *actual == expected)
        );
    } else {
        assert!(
            matches!(response.as_ref(), ServerMessage::RoomOperationResult {operation_id: actual, result}
            if *actual == operation_id && matches!(result.as_ref(), RoomOperationResult::OperationFailed {
                error_code: Some(ErrorCode::NotRoomAuthority), ..}))
        );
    }
    let _released = timeout(Duration::from_secs(1), lifecycle.lock())
        .await
        .unwrap();
    assert_eq!(
        server
            .database
            .get_room_by_id(&room)
            .await
            .unwrap()
            .unwrap()
            .authority_player,
        Some(target)
    );
}

/// #826: admitted ready publication retains source order after observer cancellation.
#[tokio::test(start_paused = true)]
async fn canceled_ready_publication_retains_source_order() {
    qualify_canceled_lobby_publication(false, false).await;
}

/// #826: game-start plans retain source order after observer cancellation.
#[tokio::test(start_paused = true)]
async fn canceled_game_start_publication_retains_source_order() {
    qualify_canceled_lobby_publication(true, false).await;
}

/// #1630: refused ready/start outcomes survive canceled observers too.
#[tokio::test(start_paused = true)]
async fn canceled_lobby_refusals_retain_their_results() {
    for starting in [false, true] {
        qualify_canceled_lobby_publication(starting, true).await;
    }
}

async fn qualify_canceled_lobby_publication(starting: bool, refused: bool) {
    let server = create_test_server_with_session(SessionConfig {
        default_topology: crate::protocol::Topology::Mesh,
        ..SessionConfig::default()
    })
    .await;
    let (actor, mut receiver) = register_client(&server, "127.0.0.1:49303".parse().unwrap()).await;
    server.set_client_protocol(
        &actor,
        NegotiatedProtocol {
            version: 3,
            transports: vec![
                crate::protocol::Transport::Relay,
                crate::protocol::Transport::WebRtc,
            ],
            topologies: vec![
                crate::protocol::Topology::Relay,
                crate::protocol::Topology::Mesh,
            ],
        },
    );
    join_seated_player(&server, &actor, &mut receiver, "LOBBYC", "host").await;
    drain_receiver(&mut receiver);
    if starting != refused {
        server.handle_player_ready(&actor).await;
        drain_receiver(&mut receiver);
    }
    if !starting && refused {
        server.handle_start_game(&actor).await;
        drain_receiver(&mut receiver);
    }
    for _ in 0..8 {
        server
            .message_coordinator
            .send_to_player(&actor, Arc::new(ServerMessage::Pong))
            .await
            .unwrap();
    }
    let caller = tokio::spawn({
        let server = Arc::clone(&server);
        async move {
            if starting {
                server.handle_start_game(&actor).await;
            } else {
                server.handle_player_ready(&actor).await;
            }
        }
    });
    timeout(Duration::from_secs(1), async {
        while server
            .metrics
            .websocket_backpressure_events
            .load(Ordering::Acquire)
            == 0
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("lobby publication reaches real queue backpressure");
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    let lifecycle = server.connection_manager.client_lifecycle(&actor).unwrap();
    let mut next_operation = Box::pin(lifecycle.lock());
    assert!(
        futures_util::poll!(&mut next_operation).is_pending(),
        "admitted lobby publication must retain source order: starting={starting}"
    );
    drop(next_operation);
    let ping = tokio::spawn({
        let server = Arc::clone(&server);
        async move { server.handle_ping(&actor).await }
    });
    for _ in 0..8 {
        assert!(matches!(
            receiver.recv().await.unwrap().as_ref(),
            ServerMessage::Pong
        ));
    }
    let first = timeout(Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    if refused {
        let expected = if starting {
            ErrorCode::GameStartNotReady
        } else {
            ErrorCode::InvalidRoomState
        };
        assert!(
            matches!(first.as_ref(), ServerMessage::Error {error_code: Some(actual), ..} if *actual == expected)
        );
    } else if starting {
        assert!(matches!(first.as_ref(), ServerMessage::GameStarting { .. }));
        let plan = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(plan.as_ref(), ServerMessage::SessionPlan(_)));
    } else {
        assert!(matches!(
            first.as_ref(),
            ServerMessage::LobbyStateChanged {
                all_ready: true,
                ..
            }
        ));
    }
    timeout(Duration::from_secs(1), ping)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        receiver.recv().await.unwrap().as_ref(),
        ServerMessage::Pong
    ));
    let _guard = timeout(Duration::from_secs(1), lifecycle.lock())
        .await
        .unwrap();
}

/// #826: an admitted authority decision must retain its source lifecycle after cancellation.
#[tokio::test(start_paused = true)]
async fn canceled_authority_request_retains_source_order() {
    for granted in [false, true] {
        let server = create_test_server_with(ServerConfig::default()).await;
        let (actor, mut receiver) =
            register_client(&server, "127.0.0.1:49302".parse().unwrap()).await;
        join_seated_player(&server, &actor, &mut receiver, "AUTHRQ", "host").await;
        drain_receiver(&mut receiver);
        for _ in 0..8 {
            server
                .message_coordinator
                .send_to_player(&actor, Arc::new(ServerMessage::Pong))
                .await
                .unwrap();
        }
        let caller = tokio::spawn({
            let server = Arc::clone(&server);
            async move { server.handle_authority_request(&actor, !granted).await }
        });
        timeout(Duration::from_secs(1), async {
            while server
                .metrics
                .websocket_backpressure_events
                .load(Ordering::Acquire)
                == 0
            {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("authority response reaches queue backpressure");
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        let lifecycle = server.connection_manager.client_lifecycle(&actor).unwrap();
        let mut next_operation = Box::pin(lifecycle.lock());
        assert!(
            futures_util::poll!(&mut next_operation).is_pending(),
            "authority completion must retain source order after caller cancellation"
        );
        drop(next_operation);
        let ping = tokio::spawn({
            let server = Arc::clone(&server);
            async move { server.handle_ping(&actor).await }
        });
        for _ in 0..8 {
            assert!(matches!(
                receiver.recv().await.unwrap().as_ref(),
                ServerMessage::Pong
            ));
        }
        let response = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(response.as_ref(), ServerMessage::AuthorityResponse {granted: actual, ..} if *actual == granted)
        );
        if granted {
            let changed = receiver.recv().await.unwrap();
            assert!(matches!(
                changed.as_ref(),
                ServerMessage::AuthorityChanged {
                    authority_player: None,
                    ..
                }
            ));
        }
        timeout(Duration::from_secs(1), ping)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            receiver.recv().await.unwrap().as_ref(),
            ServerMessage::Pong
        ));
        let _guard = timeout(Duration::from_secs(1), lifecycle.lock())
            .await
            .expect("completion releases the source lifecycle");
    }
}

/// #1628: retain the source through a committed storage wait and live mesh publication.
#[tokio::test]
async fn canceled_committed_authority_request_preserves_live_mesh_order() {
    let server = create_test_server_with_session(SessionConfig {
        default_topology: crate::protocol::Topology::Mesh,
        ..SessionConfig::default()
    })
    .await;
    let (room_id, mut peers, generation) = live_moderation_room(&server, "AUTHCM", 3).await;
    let actor = peers[0].id;
    let database = server
        .database
        .as_any()
        .downcast_ref::<crate::database::InMemoryDatabase>()
        .unwrap();
    database.pause_authority_request_after_commit_for_test();
    let caller = tokio::spawn({
        let server = Arc::clone(&server);
        async move { server.handle_authority_request(&actor, false).await }
    });
    timeout(
        Duration::from_secs(1),
        database.wait_for_authority_request_commit_for_test(),
    )
    .await
    .unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    let lifecycle = server.connection_manager.client_lifecycle(&actor).unwrap();
    let mut next_operation = Box::pin(lifecycle.lock());
    assert!(
        futures_util::poll!(&mut next_operation).is_pending(),
        "committed authority storage must retain the source lifecycle"
    );
    drop(next_operation);
    let ping = tokio::spawn({
        let server = Arc::clone(&server);
        async move { server.handle_ping(&actor).await }
    });
    database.release_authority_request_commit_for_test();
    timeout(Duration::from_secs(1), ping)
        .await
        .unwrap()
        .unwrap();
    let messages = drain_receiver(&mut peers[0].receiver);
    assert_eq!(messages.len(), 3, "{messages:?}");
    assert!(matches!(
        messages[0].as_ref(),
        ServerMessage::AuthorityResponse { granted: true, .. }
    ));
    assert!(matches!(
        messages[1].as_ref(),
        ServerMessage::AuthorityChanged {
            authority_player: None,
            ..
        }
    ));
    assert!(matches!(messages[2].as_ref(), ServerMessage::Pong));
    for peer in peers.iter_mut().skip(1) {
        let messages = drain_receiver(&mut peer.receiver);
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(matches!(
            messages[0].as_ref(),
            ServerMessage::AuthorityChanged {
                authority_player: None,
                ..
            }
        ));
    }
    assert_eq!(
        server
            .database
            .get_room_by_id(&room_id)
            .await
            .unwrap()
            .unwrap()
            .authority_player,
        None
    );
    let sender = peers[1].id;
    assert_live_moderation_signal(&server, sender, &mut peers[2], generation).await;
    let sender = peers[2].id;
    assert_live_moderation_signal(&server, sender, &mut peers[1], generation).await;
    assert!(peers
        .iter()
        .all(|peer| peer.close.requested_reason().is_none()));
}

/// #826: canceling an admitted setting must retain its exact result and source order.
#[tokio::test(start_paused = true)]
async fn canceled_room_settings_retain_their_correlated_results() {
    for unban in [false, true] {
        for refused in [false, true] {
            let server = create_test_server_with(ServerConfig::default()).await;
            let (actor, mut receiver) =
                register_client(&server, "127.0.0.1:49300".parse().unwrap()).await;
            join_seated_player(&server, &actor, &mut receiver, "CANCEL", "host").await;
            drain_receiver(&mut receiver);
            let room = server.get_client_room(&actor).await.unwrap();
            let target = PlayerId::new_v4();
            server
                .database
                .set_room_ban(&room, &target, true)
                .await
                .unwrap();
            if refused {
                server.handle_authority_request(&actor, false).await;
                drain_receiver(&mut receiver);
            }
            for _ in 0..8 {
                server
                    .message_coordinator
                    .send_to_player(&actor, Arc::new(ServerMessage::Pong))
                    .await
                    .unwrap();
            }
            let operation_id = RoomOperationId::new_v4();
            let caller = tokio::spawn({
                let server = Arc::clone(&server);
                async move {
                    if unban {
                        server
                            .handle_unban_player_operation(&actor, operation_id, target)
                            .await;
                    } else {
                        server
                            .handle_set_room_access_operation(
                                &actor,
                                operation_id,
                                Some("join-code".into()),
                            )
                            .await;
                    }
                }
            });
            timeout(Duration::from_secs(1), async {
                while server
                    .metrics
                    .websocket_backpressure_events
                    .load(Ordering::Acquire)
                    == 0
                {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .expect("the setting result reaches real queue backpressure");
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            let lifecycle = server.connection_manager.client_lifecycle(&actor).unwrap();
            let mut next_operation = Box::pin(lifecycle.lock());
            assert!(
                futures_util::poll!(&mut next_operation).is_pending(),
                "the admitted operation must retain source ordering after caller cancellation"
            );
            drop(next_operation);
            let ping = tokio::spawn({
                let server = Arc::clone(&server);
                async move { server.handle_ping(&actor).await }
            });
            for _ in 0..8 {
                assert!(matches!(
                    receiver.recv().await.unwrap().as_ref(),
                    ServerMessage::Pong
                ));
            }
            let response = timeout(Duration::from_secs(1), receiver.recv())
                .await
                .expect("canceling the caller must not lose the admitted setting result")
                .unwrap();
            let ServerMessage::RoomOperationResult {
                operation_id: actual,
                result,
            } = response.as_ref()
            else {
                panic!("expected correlated setting result: {response:?}");
            };
            assert_eq!(*actual, operation_id);
            if refused {
                assert!(matches!(
                    result.as_ref(),
                    RoomOperationResult::OperationFailed {
                        error_code: Some(ErrorCode::NotRoomAuthority),
                        ..
                    }
                ));
            } else if unban {
                assert!(
                    matches!(result.as_ref(), RoomOperationResult::PlayerUnbanned { player_id } if *player_id == target)
                );
            } else {
                assert!(matches!(
                    result.as_ref(),
                    RoomOperationResult::RoomAccessUpdated {
                        requires_password: true
                    }
                ));
            }
            let stored = server
                .database
                .get_room_by_id(&room)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(stored.is_banned(&target), refused || !unban);
            assert_eq!(stored.password.is_some(), !refused && !unban);
            timeout(Duration::from_secs(1), ping)
                .await
                .unwrap()
                .unwrap();
            let next_reply = receiver.recv().await.unwrap();
            assert!(
                matches!(next_reply.as_ref(), ServerMessage::Pong),
                "the next source operation must follow the setting result"
            );
            assert!(drain_receiver(&mut receiver).is_empty());
        }
    }
}

/// #1626: observer cancellation cannot interrupt an admitted storage decision.
#[tokio::test]
async fn canceled_room_settings_finish_storage_decisions() {
    for unban in [false, true] {
        for storage_error in [false, true] {
            let server = create_test_server_with(ServerConfig::default()).await;
            let (actor, mut receiver) =
                register_client(&server, "127.0.0.1:49301".parse().unwrap()).await;
            join_seated_player(&server, &actor, &mut receiver, "STWAIT", "host").await;
            drain_receiver(&mut receiver);
            let room = server.get_client_room(&actor).await.unwrap();
            let target = PlayerId::new_v4();
            server
                .database
                .set_room_ban(&room, &target, true)
                .await
                .unwrap();
            let database = server
                .database
                .as_any()
                .downcast_ref::<crate::database::InMemoryDatabase>()
                .unwrap();
            database.pause_next_get_room_by_id_for_test();
            let operation_id = RoomOperationId::new_v4();
            let caller = tokio::spawn({
                let server = Arc::clone(&server);
                async move {
                    if unban {
                        server
                            .handle_unban_player_operation(&actor, operation_id, target)
                            .await;
                    } else {
                        server
                            .handle_set_room_access_operation(
                                &actor,
                                operation_id,
                                Some("join-code".into()),
                            )
                            .await;
                    }
                }
            });
            timeout(
                Duration::from_secs(1),
                database.wait_for_paused_get_room_by_id_for_test(),
            )
            .await
            .unwrap();
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            database.fail_get_room_by_id_for_test(storage_error);
            database.release_paused_get_room_by_id_for_test();
            let response = timeout(Duration::from_secs(1), receiver.recv())
                .await
                .expect("the owned storage decision must reach its correlated reply")
                .unwrap();
            let ServerMessage::RoomOperationResult {
                operation_id: actual,
                result,
            } = response.as_ref()
            else {
                panic!("expected room setting result: {response:?}");
            };
            assert_eq!(*actual, operation_id);
            if storage_error {
                assert!(matches!(
                    result.as_ref(),
                    RoomOperationResult::OperationFailed {
                        error_code: Some(ErrorCode::StorageError),
                        ..
                    }
                ));
            } else if unban {
                assert!(
                    matches!(result.as_ref(), RoomOperationResult::PlayerUnbanned {player_id} if *player_id == target)
                );
            } else {
                assert!(matches!(
                    result.as_ref(),
                    RoomOperationResult::RoomAccessUpdated {
                        requires_password: true
                    }
                ));
            }
            database.fail_get_room_by_id_for_test(false);
            let stored = server
                .database
                .get_room_by_id(&room)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(stored.is_banned(&target), storage_error || !unban);
            assert_eq!(stored.password.is_some(), !storage_error && !unban);
            assert!(drain_receiver(&mut receiver).is_empty());
        }
    }
}

/// Drain a receiver without discarding results: every drained frame stays
/// available for assertions (same shape as the room-service drain helper).
fn drain_receiver(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>) -> Vec<Arc<ServerMessage>> {
    let mut messages = Vec::new();
    loop {
        match receiver.try_recv() {
            Ok(message) => messages.push(message),
            Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                return messages;
            }
        }
    }
}

async fn create_test_server_with(config: ServerConfig) -> Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
        config,
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        SessionConfig::default(),
        TurnConfig::default(),
        crate::database::DatabaseConfig::InMemory,
        MetricsConfig::default(),
        CoordinationConfig::default(),
        TransportSecurityConfig::default(),
        Vec::new(),
    )
    .await
    .expect("failed to construct test server")
}

async fn register_client_with_close_listener(
    server: &EnhancedGameServer,
    addr: SocketAddr,
) -> (
    PlayerId,
    mpsc::Receiver<Arc<ServerMessage>>,
    crate::coordination::ConnectionCloseListener,
) {
    let (sender, receiver) = mpsc::channel(8);
    let (close_signal, close_listener) = ConnectionCloseSignal::channel();
    let player_id = server
        .connection_manager
        .register_client(sender, close_signal, addr, server.instance_id)
        .await
        .expect("client registration succeeds");
    (player_id, receiver, close_listener)
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
            ConnectionCloseSignal::detached(),
            addr,
            server.instance_id,
        )
        .await
        .expect("client registration succeeds");
    (player_id, receiver)
}

/// Copy a live player's durable row into another room to model crosswise
/// stale storage state. Returns the destination room id for assertions.
async fn copy_stale_row(
    server: &EnhancedGameServer,
    source_code: &str,
    destination_code: &str,
    player_id: PlayerId,
) -> RoomId {
    let source = server
        .database
        .get_room("moderation-game", source_code)
        .await
        .unwrap()
        .unwrap();
    let destination = server
        .database
        .get_room("moderation-game", destination_code)
        .await
        .unwrap()
        .unwrap();
    let row = server
        .database
        .get_room_players(&source.id)
        .await
        .unwrap()
        .into_iter()
        .find(|player| player.id == player_id)
        .unwrap();
    server
        .database
        .add_player_to_room(&destination.id, row)
        .await
        .unwrap();
    destination.id
}

async fn join_seated_player(
    server: &Arc<EnhancedGameServer>,
    player_id: &PlayerId,
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    room_code: &str,
    name: &str,
) {
    server
        .handle_join_room(
            player_id,
            "moderation-game".to_string(),
            Some(room_code.to_string()),
            name.to_string(),
            Some(4),
            Some(true),
            None,
            None,
            None,
        )
        .await;
    let joined = timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("join should finish")
        .expect("join should respond");
    assert!(
        matches!(joined.as_ref(), ServerMessage::RoomJoined(_)),
        "expected RoomJoined, got {joined:?}"
    );
}

/// Receive until the wanted message arrives, skipping interleaved
/// room-lifecycle broadcasts (e.g. `LobbyStateChanged` from earlier joins).
async fn recv_until(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    predicate: impl Fn(&ServerMessage) -> bool,
) -> Arc<ServerMessage> {
    loop {
        let message = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("message should arrive in time")
            .expect("channel stays open");
        if predicate(message.as_ref()) {
            return message;
        }
    }
}

fn operation_failed_code(message: &ServerMessage) -> ErrorCode {
    let ServerMessage::RoomOperationResult { result, .. } = message else {
        panic!("expected RoomOperationResult, got {message:?}");
    };
    let RoomOperationResult::OperationFailed { error_code, .. } = result.as_ref() else {
        panic!("expected OperationFailed, got {result:?}");
    };
    error_code
        .clone()
        .expect("moderation refusals carry an error code")
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn authority_kick_removes_seat_closes_target_and_reports_to_authority() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48130".parse().unwrap()).await;
    let (target, mut target_rx, target_close_listener) =
        register_client_with_close_listener(&server, "127.0.0.1:48131".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "KICK01", "host").await;
    join_seated_player(&server, &target, &mut target_rx, "KICK01", "guest").await;

    server
        .handle_kick_player_operation(&authority, RoomOperationId::new_v4(), target)
        .await;

    // The authority observes the roster delta and then the correlated success.
    let roster_delta = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::PlayerLeft { .. })
    })
    .await;
    let ServerMessage::PlayerLeft { player_id, .. } = roster_delta.as_ref() else {
        panic!("expected PlayerLeft for the kicked seat, got {roster_delta:?}");
    };
    assert_eq!(*player_id, target);

    let result = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    let ServerMessage::RoomOperationResult { result, .. } = result.as_ref() else {
        panic!("expected correlated result, got {result:?}");
    };
    let RoomOperationResult::PlayerKicked { player_id: kicked } = result.as_ref() else {
        panic!("expected PlayerKicked, got {result:?}");
    };
    assert_eq!(*kicked, target);

    // The kicked connection receives the farewell error and the 4007 close.
    let farewell = recv_until(&mut target_rx, |message| {
        matches!(message, ServerMessage::Error { .. })
    })
    .await;
    let ServerMessage::Error { error_code, .. } = farewell.as_ref() else {
        panic!("expected farewell Error, got {farewell:?}");
    };
    assert_eq!(*error_code, Some(ErrorCode::Kicked));
    assert_eq!(
        target_close_listener.requested_reason(),
        Some(CloseReason::Kicked),
        "a kicked seat must close with the dedicated 4007 reason"
    );

    // The seat is gone from routing and from durable room state.
    assert!(server.get_client_room(&target).await.is_none());
    let room = server
        .database
        .get_room("moderation-game", "KICK01")
        .await
        .expect("room lookup succeeds")
        .expect("room survives the kick");
    assert!(!room.players.contains_key(&target));
    assert!(room.players.contains_key(&authority));
    assert_eq!(
        room.authority_player,
        Some(authority),
        "kicking a non-authority member must not disturb the role"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn kick_evicts_only_the_authoritys_room_not_a_rerouted_target() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48170".parse().unwrap()).await;
    let (target, mut target_rx, target_close_listener) =
        register_client_with_close_listener(&server, "127.0.0.1:48171".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "WRONG1", "host").await;
    join_seated_player(&server, &target, &mut target_rx, "WRONG1", "guest").await;

    let stale_room = server
        .database
        .get_room("moderation-game", "WRONG1")
        .await
        .expect("room lookup succeeds")
        .expect("authority room exists");
    let target_row = server
        .database
        .get_room_players(&stale_room.id)
        .await
        .expect("roster readable")
        .into_iter()
        .find(|player| player.id == target)
        .expect("target seated in the authority room");

    // Build the stale-residue state a storage-failed detach leaves behind:
    // the target departs the authority's room (row removed, route gone), its
    // connection then joins another room, and the authority's room row is
    // re-planted before the detach backlog repair can run.
    assert!(
        server
            .database
            .remove_player_from_room(&stale_room.id, &target)
            .await
            .expect("durable removal succeeds")
            .is_some(),
        "test setup removes the target's seat"
    );
    let live_room = server
        .database
        .create_room(
            "moderation-game".to_string(),
            Some("WRONG2".to_string()),
            4,
            false,
            target,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("target's live room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&target, live_room.id)
        .await;
    server
        .database
        .add_player_to_room(&stale_room.id, target_row)
        .await
        .expect("stale row planted");

    server
        .handle_kick_player_operation(&authority, RoomOperationId::new_v4(), target)
        .await;

    // The authority's room loses its stale row...
    let stale_room_after = server
        .database
        .get_room("moderation-game", "WRONG1")
        .await
        .expect("room lookup succeeds")
        .expect("authority room survives the kick");
    assert!(
        !stale_room_after.players.contains_key(&target),
        "the kick must remove the stale seat from the authority's room"
    );
    assert!(matches!(
        room_access_result(&mut authority_rx).await,
        RoomOperationResult::PlayerKicked { player_id } if player_id == target
    ));

    // ...and nothing else: the target's live membership, route, and
    // connection in the other room are not the authority's to remove.
    assert_eq!(
        server.get_client_room(&target).await,
        Some(live_room.id),
        "the target's route in its live room must survive the kick"
    );
    let live_room_after = server
        .database
        .get_room("moderation-game", "WRONG2")
        .await
        .expect("room lookup succeeds")
        .expect("live room survives the kick");
    assert!(
        live_room_after.players.contains_key(&target),
        "the target's live seat in the other room must survive the kick"
    );
    assert_eq!(
        target_close_listener.requested_reason(),
        None,
        "a kick of stale residue must not close the target's live connection"
    );

    // The farewell is part of the same contract: a target whose live
    // membership is untouched receives no kicked `Error` frame. Every
    // eviction send happens before the correlated result, so draining the
    // channel after the result observes the final state.
    let unexpected_farewell = drain_receiver(&mut target_rx).into_iter().find(|message| {
        matches!(
            message.as_ref(),
            ServerMessage::Error {
                error_code: Some(ErrorCode::Kicked),
                ..
            }
        )
    });
    assert!(
        unexpected_farewell.is_none(),
        "a kick of stale residue must not send the kicked farewell: got {unexpected_farewell:?}"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn crossed_stale_seat_moderation_completes_without_lifecycle_deadlock() {
    for ban_second in [false, true] {
        let server = create_test_server_with(ServerConfig::default()).await;
        let (a, mut a_rx, a_close) =
            register_client_with_close_listener(&server, "127.0.0.1:48580".parse().unwrap()).await;
        let (b, mut b_rx, b_close) =
            register_client_with_close_listener(&server, "127.0.0.1:48581".parse().unwrap()).await;
        join_seated_player(&server, &a, &mut a_rx, "CROSSA", "a").await;
        join_seated_player(&server, &b, &mut b_rx, "CROSSB", "b").await;

        // Synthetic crosswise stale rows make the two target lookups cross
        // lifecycle gates while both clients remain live authorities.
        let room_a = copy_stale_row(&server, "CROSSB", "CROSSA", b).await;
        let room_b = copy_stale_row(&server, "CROSSA", "CROSSB", a).await;

        let gate = server.install_moderation_lifecycle_test_gate();
        let start = Arc::new(tokio::sync::Barrier::new(3));
        let first_server = Arc::clone(&server);
        let first_start = Arc::clone(&start);
        let first = tokio::spawn(async move {
            first_start.wait().await;
            first_server
                .handle_kick_player_operation(&a, RoomOperationId::new_v4(), b)
                .await;
        });
        let second_server = Arc::clone(&server);
        let second_start = Arc::clone(&start);
        let second = tokio::spawn(async move {
            second_start.wait().await;
            if ban_second {
                second_server
                    .handle_ban_player_operation(&b, RoomOperationId::new_v4(), a)
                    .await;
            } else {
                second_server
                    .handle_kick_player_operation(&b, RoomOperationId::new_v4(), a)
                    .await;
            }
        });

        start.wait().await;

        timeout(Duration::from_secs(1), gate.wait_until_acquired(1))
            .await
            .unwrap();
        if timeout(Duration::from_secs(1), gate.wait_until_acquired(2))
            .await
            .is_ok()
        {
            gate.release_one();
            gate.release_one();
        } else {
            gate.release_one();
            timeout(Duration::from_secs(1), gate.wait_until_acquired(2))
                .await
                .unwrap();
            gate.release_one();
        }
        timeout(Duration::from_secs(1), first)
            .await
            .expect("first moderation must finish")
            .unwrap();
        timeout(Duration::from_secs(1), second)
            .await
            .expect("second moderation must finish")
            .unwrap();
        assert!(matches!(room_access_result(&mut a_rx).await,
            RoomOperationResult::PlayerKicked { player_id } if player_id == b));
        let second_result = room_access_result(&mut b_rx).await;
        if ban_second {
            assert!(matches!(second_result,
                RoomOperationResult::PlayerBanned { player_id } if player_id == a));
        } else {
            assert!(matches!(second_result,
                RoomOperationResult::PlayerKicked { player_id } if player_id == a));
        }

        let room_a_after = server
            .database
            .get_room_by_id(&room_a)
            .await
            .unwrap()
            .unwrap();
        let room_b_after = server
            .database
            .get_room_by_id(&room_b)
            .await
            .unwrap()
            .unwrap();
        assert!(room_a_after.players.contains_key(&a));
        assert!(room_b_after.players.contains_key(&b));
        assert!(!room_a_after.players.contains_key(&b));
        assert!(!room_b_after.players.contains_key(&a));
        assert_eq!(server.get_client_room(&a).await, Some(room_a));
        assert_eq!(server.get_client_room(&b).await, Some(room_b));
        assert_eq!(a_close.requested_reason(), None);
        assert_eq!(b_close.requested_reason(), None);
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn three_way_stale_seat_moderation_completes_without_lifecycle_deadlock() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (a, mut a_rx) = register_client(&server, "127.0.0.1:48582".parse().unwrap()).await;
    let (b, mut b_rx) = register_client(&server, "127.0.0.1:48583".parse().unwrap()).await;
    let (c, mut c_rx) = register_client(&server, "127.0.0.1:48584".parse().unwrap()).await;
    join_seated_player(&server, &a, &mut a_rx, "CYCLEA", "a").await;
    join_seated_player(&server, &b, &mut b_rx, "CYCLEB", "b").await;
    join_seated_player(&server, &c, &mut c_rx, "CYCLEC", "c").await;
    let room_a = copy_stale_row(&server, "CYCLEB", "CYCLEA", b).await;
    let room_b = copy_stale_row(&server, "CYCLEC", "CYCLEB", c).await;
    let room_c = copy_stale_row(&server, "CYCLEA", "CYCLEC", a).await;

    let gate = server.install_moderation_lifecycle_test_gate();
    let start = Arc::new(tokio::sync::Barrier::new(4));
    let mut tasks = Vec::new();
    for (authority, target) in [(a, b), (b, c), (c, a)] {
        let server = Arc::clone(&server);
        let start = Arc::clone(&start);
        tasks.push(tokio::spawn(async move {
            start.wait().await;
            server
                .handle_kick_player_operation(&authority, RoomOperationId::new_v4(), target)
                .await;
        }));
    }
    start.wait().await;
    timeout(Duration::from_secs(1), gate.wait_until_acquired(1))
        .await
        .unwrap();
    if timeout(Duration::from_secs(1), gate.wait_until_acquired(3))
        .await
        .is_ok()
    {
        for _ in 0..3 {
            gate.release_one();
        }
    } else {
        for reached in 2..=3 {
            gate.release_one();
            timeout(Duration::from_secs(1), gate.wait_until_acquired(reached))
                .await
                .unwrap();
        }
        gate.release_one();
    }
    for task in tasks {
        timeout(Duration::from_secs(1), task)
            .await
            .expect("moderation cycle must finish")
            .unwrap();
    }
    assert!(
        matches!(room_access_result(&mut a_rx).await, RoomOperationResult::PlayerKicked { player_id } if player_id == b)
    );
    assert!(
        matches!(room_access_result(&mut b_rx).await, RoomOperationResult::PlayerKicked { player_id } if player_id == c)
    );
    assert!(
        matches!(room_access_result(&mut c_rx).await, RoomOperationResult::PlayerKicked { player_id } if player_id == a)
    );
    for (room, authority, stale) in [(room_a, a, b), (room_b, b, c), (room_c, c, a)] {
        let stored = server
            .database
            .get_room_by_id(&room)
            .await
            .unwrap()
            .unwrap();
        assert!(stored.players.contains_key(&authority));
        assert!(!stored.players.contains_key(&stale));
        assert_eq!(server.get_client_room(&authority).await, Some(room));
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn kick_refusals_are_classified_per_failure() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48132".parse().unwrap()).await;
    let (outsider, mut outsider_rx) =
        register_client(&server, "127.0.0.1:48133".parse().unwrap()).await;
    let (target, mut target_rx) =
        register_client(&server, "127.0.0.1:48134".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "KICK02", "host").await;
    join_seated_player(&server, &target, &mut target_rx, "KICK02", "guest").await;

    // The outsider is seated in its own unrelated room, so a kick attempt
    // exercises the authority refusal rather than a missing-room refusal.
    let outsider_room = server
        .database
        .create_room(
            "moderation-game".to_string(),
            Some("OTHERX".to_string()),
            4,
            // No authority in the outsider's room, so the outsider holds no
            // authority anywhere and the kick attempt must hit the
            // NotRoomAuthority refusal.
            false,
            outsider,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("outsider room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&outsider, outsider_room.id)
        .await;

    // Refusals are sent to the actor that attempted the kick. The outsider
    // attempt exercises the authority refusal; the authority attempts
    // exercise the target and self-kick refusals.
    let refusals = [
        (
            outsider,
            target,
            ErrorCode::NotRoomAuthority,
            "non-authority member must be refused",
        ),
        (
            authority,
            outsider,
            ErrorCode::KickTargetNotFound,
            "a target outside the room must be refused",
        ),
        (
            authority,
            authority,
            ErrorCode::InvalidInput,
            "self-kick must be refused",
        ),
    ];

    for (actor, kicked, expected, description) in refusals {
        server
            .handle_kick_player_operation(&actor, RoomOperationId::new_v4(), kicked)
            .await;
        let response = if actor == outsider {
            recv_until(&mut outsider_rx, |message| {
                matches!(message, ServerMessage::RoomOperationResult { .. })
            })
            .await
        } else {
            recv_until(&mut authority_rx, |message| {
                matches!(message, ServerMessage::RoomOperationResult { .. })
            })
            .await
        };
        assert_eq!(
            operation_failed_code(&response),
            expected,
            "{description}: got {response:?}"
        );
    }

    // None of the refusals removed any seat.
    let room = server
        .database
        .get_room("moderation-game", "KICK02")
        .await
        .expect("room lookup succeeds")
        .expect("room survives refusals");
    assert!(room.players.contains_key(&target));
    assert!(room.players.contains_key(&authority));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn regenerate_room_code_rotates_registry_and_drops_the_old_code() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48135".parse().unwrap()).await;
    join_seated_player(&server, &authority, &mut authority_rx, "OLDCOD", "host").await;
    server.script_room_codes_for_test(["NEWCOD"]);

    server
        .handle_regenerate_room_code_operation(&authority, RoomOperationId::new_v4())
        .await;

    let response = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    let ServerMessage::RoomOperationResult { result, .. } = response.as_ref() else {
        panic!("expected correlated result, got {response:?}");
    };
    let RoomOperationResult::RoomCodeRegenerated { room_code } = result.as_ref() else {
        panic!("expected RoomCodeRegenerated, got {result:?}");
    };
    assert_eq!(room_code, "NEWCOD");

    let old_lookup = server
        .database
        .get_room("moderation-game", "OLDCOD")
        .await
        .expect("old-code lookup succeeds");
    assert!(
        old_lookup.is_none(),
        "the old code must stop resolving to the room"
    );
    let room = server
        .database
        .get_room("moderation-game", "NEWCOD")
        .await
        .expect("new-code lookup succeeds")
        .expect("the new code resolves to the room");
    assert_eq!(room.code, "NEWCOD");
    assert!(
        room.players.contains_key(&authority),
        "rotation must not disturb membership"
    );
}

/// Issue #550 class, room-code rotation leg: the rotation holds
/// `room_join:{game}:{candidate}` across the storage swap, so the lease must
/// stay renewed while the swap stalls — the candidate-code mutex must still
/// exclude a racing joiner after the raw TTL has elapsed.
///
/// The lease TTL is shrunk to 3 s (test-only override): the probe sleeps
/// 3.5 s, past one raw TTL, instead of a 10.4 s sleep past the production
/// 10 s lease that red-waved the weekly mutation baseline against the
/// mutants profile's 10 s hang budget (issue #604). The test runs on the
/// paused tokio clock (`start_paused`): the sleep is virtual, renewal ticks
/// still fire in deadline order, and the runner-load jitter the real sleep
/// was sized against is gone.
#[tokio::test(start_paused = true)]
#[cfg_attr(miri, ignore)]
async fn stalled_room_code_rotation_keeps_its_mutex_lease_alive() {
    let server = create_test_server_with(ServerConfig::default()).await;
    server.coordination_lock_ttl_override_ms_for_test(3_000);
    let database = server
        .database
        .as_any()
        .downcast_ref::<crate::database::InMemoryDatabase>()
        .expect("in-memory test database");
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48137".parse().unwrap()).await;
    join_seated_player(&server, &authority, &mut authority_rx, "OLDCOD", "host").await;
    server.script_room_codes_for_test(["NEWCOD"]);

    database.pause_next_update_room_code_for_test();
    let rotation_server = Arc::clone(&server);
    let rotation_task = tokio::spawn(async move {
        rotation_server
            .handle_regenerate_room_code_operation(&authority, RoomOperationId::new_v4())
            .await;
    });
    database.wait_for_paused_update_room_code_for_test().await;

    // Outlast the room-join lease TTL with the swap stalled. An expired lease
    // would leave the candidate-code mutex free to take; a renewed lease must
    // still report it busy.
    tokio::time::sleep(Duration::from_millis(3_500)).await;
    let stolen = server
        .distributed_lock_for_test()
        .try_acquire("room_join:moderation-game:NEWCOD", Duration::from_secs(1))
        .await
        .expect("lock probe succeeds")
        .is_some();
    assert!(
        !stolen,
        "the rotation's mutex lease must outlive its stalled storage hold"
    );

    database.release_paused_update_room_code_for_test();
    rotation_task.await.expect("rotation task completes");
    let response = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    let ServerMessage::RoomOperationResult { result, .. } = response.as_ref() else {
        panic!("expected correlated result, got {response:?}");
    };
    let RoomOperationResult::RoomCodeRegenerated { room_code } = result.as_ref() else {
        panic!("expected RoomCodeRegenerated, got {result:?}");
    };
    assert_eq!(room_code, "NEWCOD");
    assert_eq!(
        server
            .metrics
            .snapshot()
            .await
            .distributed_lock
            .renewal_failures,
        0,
        "a lease that stayed renewed must never report a loss"
    );
}

/// Issue #396 spectator-target pin: kick and ban evict a *seated* member (or
/// a pending-record holder). A live spectator of the same room is neither,
/// so both operations refuse with `KICK_TARGET_NOT_FOUND` and leave the
/// spectator session, the roster, and the ban list untouched.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn kick_and_ban_refuse_a_spectator_target() {
    for operation in ["kick", "ban"] {
        let server = create_test_server_with(ServerConfig::default()).await;
        let (authority, mut authority_rx) =
            register_client(&server, "127.0.0.1:48163".parse().unwrap()).await;
        let (watcher, _watcher_rx) =
            register_client(&server, "127.0.0.1:48164".parse().unwrap()).await;

        join_seated_player(&server, &authority, &mut authority_rx, "SPTGT1", "host").await;
        server
            .spectator_service
            .join(
                &watcher,
                "moderation-game".to_string(),
                "SPTGT1".to_string(),
                "watcher".to_string(),
            )
            .await
            .expect("spectator join succeeds");
        assert!(server.spectator_service.is_spectating(&watcher));

        if operation == "kick" {
            server
                .handle_kick_player_operation(&authority, RoomOperationId::new_v4(), watcher)
                .await;
        } else {
            server
                .handle_ban_player_operation(&authority, RoomOperationId::new_v4(), watcher)
                .await;
        }
        let response = recv_until(&mut authority_rx, |message| {
            matches!(message, ServerMessage::RoomOperationResult { .. })
        })
        .await;
        assert_eq!(
            operation_failed_code(&response),
            ErrorCode::KickTargetNotFound,
            "{operation}: a spectator is not a moderation target: got {response:?}"
        );
        assert!(
            server.spectator_service.is_spectating(&watcher),
            "{operation}: the refused moderation must not disturb the spectator session"
        );
        let room = server
            .database
            .get_room("moderation-game", "SPTGT1")
            .await
            .expect("room lookup succeeds")
            .expect("room exists");
        assert!(!room.is_banned(&watcher), "{operation}: no ban is recorded");
        assert_eq!(room.spectators.len(), 1, "{operation}: roster unchanged");
    }
}

/// Issue #396 cross-room protection, pending-record class: a moderation
/// eviction of a pending-record holder must not reach into another room.
/// The holder's live spectator session in a different room is a live
/// membership elsewhere — it receives neither the farewell nor the `4007`
/// close; only the record (tombstoned) and, for a ban, the room A ban bit
/// are removed. Red-first: `get_client_room` saw only seated routes, so the
/// close fired on the unrelated spectator socket.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn evicting_a_record_holder_leaves_their_other_room_spectator_session_alive() {
    for operation in ["kick", "ban"] {
        let server = create_test_server_with(ServerConfig {
            enable_reconnection: true,
            ..ServerConfig::default()
        })
        .await;
        let (authority, mut authority_rx) =
            register_client(&server, "127.0.0.1:48168".parse().unwrap()).await;
        let (target, mut _target_rx, target_close_listener) =
            register_client_with_close_listener(&server, "127.0.0.1:48169".parse().unwrap()).await;

        join_seated_player(&server, &authority, &mut authority_rx, "EVICT1", "host").await;
        join_seated_player(&server, &target, &mut _target_rx, "EVICT1", "guest").await;
        let room_a_id = server
            .get_client_room(&target)
            .await
            .expect("target seated");
        let reconnection_manager = server
            .reconnection_manager()
            .expect("reconnection is enabled");

        // The disconnect arms the record through the teardown-window path,
        // keeping the connection registered for the spectator re-admission.
        let seat_info = server
            .database
            .get_room_by_id(&room_a_id)
            .await
            .expect("room lookup succeeds")
            .expect("room remains present")
            .players
            .get(&target)
            .cloned()
            .expect("target holds a durable seat");
        reconnection_manager
            .register_disconnection(
                target,
                room_a_id,
                false,
                Some(seat_info),
                server
                    .connection_manager
                    .game_data_epoch(&target)
                    .unwrap_or(0),
            )
            .await;
        server.leave_room_locked(&target, false).await;
        assert!(reconnection_manager.has_pending_reconnection(&target).await);

        // The holder's live role elsewhere: a spectator session in room B.
        server
            .database
            .create_room(
                "moderation-game".to_string(),
                Some("OTHER9".to_string()),
                4,
                false,
                uuid::Uuid::new_v4(),
                "udp".to_string(),
                "region-a".to_string(),
                None,
            )
            .await
            .expect("other-room creation succeeds");
        server
            .spectator_service
            .join(
                &target,
                "moderation-game".to_string(),
                "OTHER9".to_string(),
                "WatcherElsewhere".to_string(),
            )
            .await
            .expect("the holder spectates the other room");
        assert!(server.spectator_service.is_spectating(&target));

        if operation == "kick" {
            server
                .handle_kick_player_operation(&authority, RoomOperationId::new_v4(), target)
                .await;
        } else {
            server
                .handle_ban_player_operation(&authority, RoomOperationId::new_v4(), target)
                .await;
        }
        let result = room_access_result(&mut authority_rx).await;
        match (operation, &result) {
            ("kick", RoomOperationResult::PlayerKicked { player_id }) => {
                assert_eq!(*player_id, target)
            }
            ("ban", RoomOperationResult::PlayerBanned { player_id }) => {
                assert_eq!(*player_id, target)
            }
            other => panic!("expected the {operation} to succeed, got {other:?}"),
        }

        // The unrelated spectator session keeps its socket: no farewell, no
        // `4007` close, no detach.
        assert_eq!(
            target_close_listener.requested_reason(),
            None,
            "{operation}: a record holder spectating another room must keep its session"
        );
        assert!(
            server.spectator_service.is_spectating(&target),
            "{operation}: the spectator role must survive the eviction"
        );

        // The room A credential is still terminated: the record is
        // tombstoned, and a ban additionally records the ban bit.
        let claim = reconnection_manager
            .claim_reconnection(&uuid::Uuid::new_v4(), &target, &room_a_id, "any-token")
            .await;
        assert!(
            matches!(claim, Err(crate::reconnection::ReconnectionError::Kicked)),
            "{operation}: the record must be tombstoned, got {claim:?}"
        );
        let room_a = server
            .database
            .get_room_by_id(&room_a_id)
            .await
            .expect("room lookup succeeds")
            .expect("room remains present");
        assert_eq!(
            room_a.is_banned(&target),
            operation == "ban",
            "{operation}: the ban bit must match the operation"
        );
        let room_b_spectators = server
            .database
            .get_room_spectators(
                &server
                    .database
                    .get_room("moderation-game", "OTHER9")
                    .await
                    .expect("room lookup succeeds")
                    .expect("other room remains present")
                    .id,
            )
            .await
            .expect("fetch spectators");
        assert!(
            room_b_spectators.iter().any(|info| info.id == target),
            "{operation}: the other room's roster must keep the spectator"
        );
    }
}

/// Issue #561 resolution pin: a rotated-away code is an unknown code. The
/// spectator path resolves nothing and refuses `ROOM_NOT_FOUND`; the seated
/// path has no tombstone either, so join-creates-room opens a fresh,
/// unrelated room under the stale code and first-claim authority seats the
/// joiner. Pinned end-to-end so the documented contract cannot change
/// silently (a future tombstone registry would be a deliberate wire-visible
/// design change).
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn a_stale_rotated_code_behaves_like_an_unknown_code() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48165".parse().unwrap()).await;
    let (watcher, _watcher_rx) = register_client(&server, "127.0.0.1:48166".parse().unwrap()).await;
    let (newcomer, mut newcomer_rx) =
        register_client(&server, "127.0.0.1:48167".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "STALE1", "host").await;
    let rotated_room_id = server
        .get_client_room(&authority)
        .await
        .expect("authority seated");
    server.script_room_codes_for_test(["FRESH1"]);
    server
        .handle_regenerate_room_code_operation(&authority, RoomOperationId::new_v4())
        .await;
    let response = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    let ServerMessage::RoomOperationResult { result, .. } = response.as_ref() else {
        panic!("expected correlated result, got {response:?}");
    };
    let RoomOperationResult::RoomCodeRegenerated { room_code } = result.as_ref() else {
        panic!("expected RoomCodeRegenerated, got {result:?}");
    };
    assert_eq!(room_code, "FRESH1");

    // Spectator path: the stale code resolves to nothing (no creation
    // branch), so the admission is refused.
    let spectator_error = server
        .spectator_service
        .join(
            &watcher,
            "moderation-game".to_string(),
            "STALE1".to_string(),
            "watcher".to_string(),
        )
        .await
        .expect_err("a stale code must not seat a spectator");
    assert_eq!(
        spectator_error.code,
        Some(ErrorCode::RoomNotFound),
        "the stale code must not resolve: {spectator_error:?}"
    );

    // Seated path: the stale code is an unknown code, so join-creates-room
    // opens a fresh room and first-claim authority seats the joiner.
    join_seated_player(&server, &newcomer, &mut newcomer_rx, "STALE1", "newcomer").await;
    let fresh_room_id = server
        .get_client_room(&newcomer)
        .await
        .expect("the stale-code join seats its own room");
    assert_ne!(
        fresh_room_id, rotated_room_id,
        "the stale code must not lead back into the rotated room"
    );
    let fresh_room = server
        .database
        .get_room_by_id(&fresh_room_id)
        .await
        .expect("room lookup succeeds")
        .expect("fresh room exists");
    assert_eq!(fresh_room.code, "STALE1");
    assert_eq!(
        fresh_room.authority_player,
        Some(newcomer),
        "first-claim authority seats the stale-code joiner"
    );
    let rotated_room = server
        .database
        .get_room_by_id(&rotated_room_id)
        .await
        .expect("room lookup succeeds")
        .expect("rotated room exists");
    assert_eq!(rotated_room.code, "FRESH1");
    assert_eq!(rotated_room.authority_player, Some(authority));
}

/// Issue #396 sweep, rotation × old-code admission seam: a joiner holding
/// `room_join:{game}:{old}` is mid-admission. Rotation must take the old
/// code's lock before the swap so the joiner resolves the room pre-swap;
/// otherwise it observes the dropped code as free and resurrects it as a
/// duplicate room, seated as creator. The held lock hard-blocks the fixed
/// rotation, so "the swap did not run while the lock was held" cannot pass
/// by timing — an unfixed rotation reaches the swap within microseconds.
/// The 2 s wait bound runs on the paused tokio clock (`start_paused`), so
/// the provably-blocked wait costs no wall time.
#[tokio::test(start_paused = true)]
#[cfg_attr(miri, ignore)]
async fn rotation_waits_for_an_in_flight_old_code_admission_lock() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let database = server
        .database
        .as_any()
        .downcast_ref::<crate::database::InMemoryDatabase>()
        .expect("in-memory test database");
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48138".parse().unwrap()).await;
    join_seated_player(&server, &authority, &mut authority_rx, "OLDCOD", "host").await;
    server.script_room_codes_for_test(["NEWCOD"]);

    // The simulated mid-flight old-code joiner: its admission holds the old
    // code's lock with the join path's TTL, exactly like the admission
    // critical section (a short probe TTL would expire under rotation's
    // bounded retry and void the serialization this test pins).
    let joiner_lock = server
        .distributed_lock_for_test()
        .try_acquire("room_join:moderation-game:OLDCOD", Duration::from_secs(10))
        .await
        .expect("lock probe succeeds")
        .expect("the old-code lock is free before the simulated joiner");

    database.pause_next_update_room_code_for_test();
    let rotation_server = Arc::clone(&server);
    let rotation_task = tokio::spawn(async move {
        rotation_server
            .handle_regenerate_room_code_operation(&authority, RoomOperationId::new_v4())
            .await;
    });

    let reached_swap = timeout(
        Duration::from_secs(2),
        database.wait_for_paused_update_room_code_for_test(),
    )
    .await
    .is_ok();
    assert!(
        !reached_swap,
        "rotation must not swap the code while an old-code admission holds its lock"
    );

    // The mid-flight admission completes against the pre-swap registry; only
    // then does rotation acquire the freed lock and swap.
    server
        .distributed_lock_for_test()
        .release(&joiner_lock)
        .await
        .expect("simulated joiner releases its admission lock");
    database.release_paused_update_room_code_for_test();
    rotation_task.await.expect("rotation task completes");
    let response = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    let ServerMessage::RoomOperationResult { result, .. } = response.as_ref() else {
        panic!("expected correlated result, got {response:?}");
    };
    let RoomOperationResult::RoomCodeRegenerated { room_code } = result.as_ref() else {
        panic!("expected RoomCodeRegenerated, got {result:?}");
    };
    assert_eq!(room_code, "NEWCOD");
    let old_lookup = server
        .database
        .get_room("moderation-game", "OLDCOD")
        .await
        .expect("old-code lookup succeeds");
    assert!(
        old_lookup.is_none(),
        "the old code must stop resolving after the swap"
    );
}

/// A spectator can pass the old-code lock and wait inside the room event lane.
/// Rotation must wait for that admission before it drops the old code.
#[tokio::test(start_paused = true)]
#[cfg_attr(miri, ignore)]
async fn rotation_waits_for_in_flight_old_code_spectator_admission() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let database = server
        .database
        .as_any()
        .downcast_ref::<crate::database::InMemoryDatabase>()
        .expect("in-memory test database");
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48151".parse().unwrap()).await;
    join_seated_player(&server, &authority, &mut authority_rx, "OLDCOD", "host").await;
    let (spectator, _spectator_rx) =
        register_client(&server, "127.0.0.1:48152".parse().unwrap()).await;
    let spectator_id = spectator;
    let (late_spectator, _late_rx) =
        register_client(&server, "127.0.0.1:48153".parse().unwrap()).await;
    server.script_room_codes_for_test(["NEWCOD"]);

    database.pause_next_get_room_by_id_for_test();
    let spectator_service = server.spectator_service.clone();
    let spectator_join = tokio::spawn(async move {
        spectator_service
            .join(
                &spectator,
                "moderation-game".to_string(),
                "OLDCOD".to_string(),
                "watcher".to_string(),
            )
            .await
    });
    timeout(
        Duration::from_secs(1),
        database.wait_for_paused_get_room_by_id_for_test(),
    )
    .await
    .expect("spectator reaches the in-lane room refresh");
    assert!(
        !server
            .distributed_lock_for_test()
            .is_locked("room_join:moderation-game:OLDCOD")
            .await
            .expect("old-code lock state is readable"),
        "the spectator has released the code lock while holding the room lane"
    );

    let rotation_server = Arc::clone(&server);
    let mut rotation = tokio::spawn(async move {
        rotation_server
            .handle_regenerate_room_code_operation(&authority, RoomOperationId::new_v4())
            .await;
    });
    assert!(
        timeout(Duration::from_millis(100), &mut rotation)
            .await
            .is_err(),
        "rotation must wait for the admitted old-code spectator"
    );

    database.release_paused_get_room_by_id_for_test();
    spectator_join
        .await
        .expect("spectator task completes")
        .expect("old-code spectator joins before rotation");
    rotation.await.expect("rotation task completes");
    let response = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    assert!(matches!(
        response.as_ref(),
        ServerMessage::RoomOperationResult { result, .. }
            if matches!(result.as_ref(), RoomOperationResult::RoomCodeRegenerated { room_code }
                if room_code == "NEWCOD")
    ));
    let refusal = server
        .spectator_service
        .join(
            &late_spectator,
            "moderation-game".to_string(),
            "OLDCOD".to_string(),
            "late".to_string(),
        )
        .await
        .expect_err("old code must stop admitting spectators after rotation");
    assert_eq!(refusal.code, Some(ErrorCode::RoomNotFound));
    let room = server
        .database
        .get_room("moderation-game", "NEWCOD")
        .await
        .expect("room lookup succeeds")
        .expect("new code resolves the room");
    let spectators = room.get_spectators();
    assert_eq!(spectators.len(), 1);
    assert_eq!(spectators[0].id, spectator_id);
    server
        .spectator_service
        .join(
            &late_spectator,
            "moderation-game".to_string(),
            "NEWCOD".to_string(),
            "late".to_string(),
        )
        .await
        .expect("new code admits a spectator after rotation");
    let room = server
        .database
        .get_room("moderation-game", "NEWCOD")
        .await
        .expect("room lookup succeeds")
        .expect("new code resolves the room");
    assert_eq!(
        room.get_spectators().len(),
        2,
        "both spectators remain in the rotated room"
    );
    for code in ["OLDCOD", "NEWCOD"] {
        assert!(
            !server
                .distributed_lock_for_test()
                .is_locked(&format!("room_join:moderation-game:{code}"))
                .await
                .expect("code lock state is readable"),
            "rotation and admission must release the {code} lock"
        );
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn regenerate_retries_colliding_candidates_within_the_budget() {
    let server = create_test_server_with(ServerConfig::default()).await;
    // Pre-occupy the first scripted candidate in the same game namespace.
    server
        .database
        .create_room(
            "moderation-game".to_string(),
            Some("COLL10".to_string()),
            4,
            false,
            uuid::Uuid::new_v4(),
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("colliding room creation succeeds");
    server.script_room_codes_for_test(["COLL10", "FRESH9"]);

    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48136".parse().unwrap()).await;
    join_seated_player(&server, &authority, &mut authority_rx, "MINE10", "host").await;

    server
        .handle_regenerate_room_code_operation(&authority, RoomOperationId::new_v4())
        .await;

    let response = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    let ServerMessage::RoomOperationResult { result, .. } = response.as_ref() else {
        panic!("expected correlated result, got {response:?}");
    };
    let RoomOperationResult::RoomCodeRegenerated { room_code } = result.as_ref() else {
        panic!("expected RoomCodeRegenerated, got {result:?}");
    };
    assert_eq!(room_code, "FRESH9", "a colliding candidate must be retried");

    let room = server
        .database
        .get_room("moderation-game", "FRESH9")
        .await
        .expect("new-code lookup succeeds")
        .expect("rotated room exists");
    assert!(room.players.contains_key(&authority));
}

#[tokio::test(start_paused = true)]
#[cfg_attr(miri, ignore)]
async fn regenerate_skips_own_or_busy_code_before_waiting_for_a_second_lock() {
    for (first_candidate, occupied_by_other) in [("OLDCOD", false), ("BUSY01", true)] {
        let server = create_test_server_with(ServerConfig::default()).await;
        let (authority, mut authority_rx) =
            register_client(&server, "127.0.0.1:48154".parse().unwrap()).await;
        join_seated_player(&server, &authority, &mut authority_rx, "OLDCOD", "host").await;
        server.script_room_codes_for_test([first_candidate, "NEWCOD"]);

        let busy_lock = if occupied_by_other {
            Some(
                server
                    .distributed_lock_for_test()
                    .try_acquire("room_join:moderation-game:BUSY01", Duration::from_secs(10))
                    .await
                    .expect("candidate lock probe succeeds")
                    .expect("candidate lock starts free"),
            )
        } else {
            None
        };

        timeout(
            Duration::from_secs(1),
            server.handle_regenerate_room_code_operation(&authority, RoomOperationId::new_v4()),
        )
        .await
        .unwrap_or_else(|_| panic!("rotation must skip {first_candidate} without waiting"));
        let response = recv_until(&mut authority_rx, |message| {
            matches!(message, ServerMessage::RoomOperationResult { .. })
        })
        .await;
        assert!(matches!(
            response.as_ref(),
            ServerMessage::RoomOperationResult { result, .. }
                if matches!(result.as_ref(), RoomOperationResult::RoomCodeRegenerated { room_code }
                    if room_code == "NEWCOD")
        ));

        if let Some(lock) = busy_lock {
            server
                .distributed_lock_for_test()
                .release(&lock)
                .await
                .expect("candidate lock release succeeds");
        }
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn creation_applies_the_default_spectator_capacity() {
    struct Case {
        name: &'static str,
        configured: Option<u8>,
        max_players: u8,
        expected: Option<u8>,
    }
    let cases = [
        Case {
            name: "auto derives 2x the player ceiling",
            configured: None,
            max_players: 4,
            expected: Some(8),
        },
        Case {
            name: "a positive value fixes the cap",
            configured: Some(2),
            max_players: 4,
            expected: Some(2),
        },
        Case {
            name: "zero restores unlimited spectators",
            configured: Some(0),
            max_players: 4,
            expected: None,
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let config = ServerConfig {
            default_max_spectators: case.configured,
            ..ServerConfig::default()
        };
        let server = create_test_server_with(config).await;
        let (creator, mut rx) = register_client(
            &server,
            format!("127.0.0.1:481{}", 40 + index).parse().unwrap(),
        )
        .await;
        server
            .handle_join_room(
                &creator,
                "spectator-cap-game".to_string(),
                None,
                "creator".to_string(),
                Some(case.max_players),
                Some(false),
                None,
                None,
                None,
            )
            .await;
        let joined = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("join should finish")
            .expect("join should respond");
        assert!(
            matches!(joined.as_ref(), ServerMessage::RoomJoined(_)),
            "expected RoomJoined, got {joined:?}"
        );

        let room = server
            .database
            .get_room_by_id(&server.get_client_room(&creator).await.expect("seated"))
            .await
            .expect("room lookup succeeds")
            .expect("room exists");
        assert_eq!(
            room.max_spectators, case.expected,
            "{} (configured {:?})",
            case.name, case.configured
        );
        assert_eq!(
            room.can_spectate(),
            case.expected.is_none_or(|cap| cap > 0),
            "{}: an empty room must admit spectators under any positive cap",
            case.name
        );
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn kicked_disconnected_seat_is_removed_and_never_reconnectable() {
    use crate::reconnection::ReconnectionError;

    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48150".parse().unwrap()).await;
    let (target, mut target_rx) =
        register_client(&server, "127.0.0.1:48151".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "KICK10", "host").await;
    join_seated_player(&server, &target, &mut target_rx, "KICK10", "guest").await;
    let room_id = server
        .get_client_room(&target)
        .await
        .expect("target seated");

    // The target's socket drops: the real unregister path arms a pending
    // reconnection record (the durable member row is removed; only the
    // record holds the seat) and clears local routing.
    server.unregister_client(&target).await;
    assert!(server.get_client_room(&target).await.is_none());
    let pending_room = match &server.reconnection_manager {
        Some(manager) => manager.pending_reconnection_room(&target).await,
        None => panic!("reconnection manager must be active for this test"),
    };
    assert_eq!(
        pending_room,
        Some(room_id),
        "the disconnect must leave a claimable record for the seat"
    );

    // The authority kicks the vacated seat.
    server
        .handle_kick_player_operation(&authority, RoomOperationId::new_v4(), target)
        .await;

    // The seat is gone from durable state and the pending record is
    // tombstoned: the claim path refuses with the dedicated error.
    let room = server
        .database
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup succeeds")
        .expect("room survives the kick");
    assert!(!room.players.contains_key(&target));

    if let Some(manager) = &server.reconnection_manager {
        let outcome = manager
            .claim_reconnection(&authority, &target, &room_id, "any-token")
            .await;
        assert!(
            matches!(outcome, Err(ReconnectionError::Kicked)),
            "a kicked seat must refuse its reconnection claim, got {outcome:?}"
        );
    } else {
        panic!("reconnection manager must be active for this test");
    }

    let response = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    let ServerMessage::RoomOperationResult { result, .. } = response.as_ref() else {
        panic!("expected correlated result, got {response:?}");
    };
    assert!(
        matches!(result.as_ref(), RoomOperationResult::PlayerKicked { player_id } if *player_id == target),
        "expected PlayerKicked for the vacated seat, got {result:?}"
    );
}

/// A target that reconnects after kick validation can disconnect again before
/// the kick removes its newly routed seat. The second record must not survive.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn kick_cannot_leave_a_fresh_reconnect_record_after_target_disconnect() {
    use crate::coordination::ClientDeliveryHandle;

    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48170".parse().unwrap()).await;
    let (target, mut target_rx) =
        register_client(&server, "127.0.0.1:48171".parse().unwrap()).await;
    join_seated_player(&server, &authority, &mut authority_rx, "KICK11", "host").await;
    join_seated_player(&server, &target, &mut target_rx, "KICK11", "guest").await;
    let room_id = server
        .get_client_room(&target)
        .await
        .expect("target seated");
    let seat = server
        .database
        .get_room_by_id(&room_id)
        .await
        .unwrap()
        .unwrap()
        .players
        .get(&target)
        .cloned()
        .expect("seat exists");
    let manager = server.reconnection_manager().expect("reconnection enabled");
    let token = manager
        .register_disconnection(target, room_id, false, Some(seat), 0)
        .await;
    server
        .database
        .remove_player_from_room(&room_id, &target)
        .await
        .unwrap();
    server.connection_manager.remove_client(&target);
    server
        .message_coordinator
        .unregister_local_client(&target)
        .await
        .unwrap();

    let gate = server.install_moderation_eviction_test_gate();
    let kick_server = Arc::clone(&server);
    let kick = tokio::spawn(async move {
        kick_server
            .handle_kick_player_operation(&authority, RoomOperationId::new_v4(), target)
            .await;
    });
    timeout(Duration::from_secs(1), gate.wait_until_resolved())
        .await
        .expect("kick reached post-validation pause");

    let (sender, _receiver) = mpsc::channel(8);
    let socket = server
        .connection_manager
        .register_client(
            sender.clone(),
            ConnectionCloseSignal::detached(),
            "127.0.0.1:48172".parse().unwrap(),
            server.instance_id,
        )
        .await
        .unwrap();
    server
        .message_coordinator
        .register_local_client(
            socket,
            None,
            ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
        )
        .await
        .unwrap();
    let effective_id = Arc::new(tokio::sync::RwLock::new(socket));
    assert!(
        server
            .handle_reconnect_with_identity_operation(
                &socket,
                &target,
                &room_id,
                &token,
                effective_id,
                Some(RoomOperationId::new_v4()),
            )
            .await
    );
    assert_eq!(server.get_client_room(&target).await, Some(room_id));

    gate.release_resolved();
    timeout(Duration::from_secs(1), gate.wait_until_routed())
        .await
        .expect("kick read the restored route");
    let disconnect_gate = server.install_reconnect_teardown_test_gate();
    let disconnect_server = Arc::clone(&server);
    let disconnect = tokio::spawn(async move {
        disconnect_server.unregister_client(&target).await;
    });
    assert!(
        timeout(Duration::from_secs(1), disconnect_gate.wait_until_armed())
            .await
            .is_err(),
        "target disconnect armed a new record before the kick removed its seat"
    );
    gate.release_routed();
    timeout(Duration::from_secs(1), kick)
        .await
        .expect("kick must finish")
        .expect("kick task must not panic");
    timeout(Duration::from_secs(1), disconnect)
        .await
        .expect("disconnect must finish")
        .expect("disconnect task must not panic");

    let pending = manager.pending_reconnection_room(&target).await;
    assert!(
        pending.is_none() || manager.is_reconnection_kicked(&target).await,
        "kick reported success but left a claimable new reconnect record"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn kick_does_not_tombstone_a_new_pending_record_in_another_room() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48173".parse().unwrap()).await;
    let (target, mut target_rx) =
        register_client(&server, "127.0.0.1:48174".parse().unwrap()).await;
    join_seated_player(&server, &authority, &mut authority_rx, "KICK12", "host").await;
    join_seated_player(&server, &target, &mut target_rx, "KICK12", "guest").await;
    let room_a = server.get_client_room(&target).await.unwrap();
    let seat = server
        .database
        .get_room_by_id(&room_a)
        .await
        .unwrap()
        .unwrap()
        .players
        .get(&target)
        .cloned()
        .unwrap();
    let manager = server.reconnection_manager().unwrap();
    manager
        .register_disconnection(target, room_a, false, Some(seat), 0)
        .await;
    server
        .database
        .remove_player_from_room(&room_a, &target)
        .await
        .unwrap();
    server.connection_manager.remove_client(&target);
    server
        .message_coordinator
        .unregister_local_client(&target)
        .await
        .unwrap();

    let gate = server.install_moderation_eviction_test_gate();
    let kick_server = Arc::clone(&server);
    let kick = tokio::spawn(async move {
        kick_server
            .handle_kick_player_operation(&authority, RoomOperationId::new_v4(), target)
            .await;
    });
    timeout(Duration::from_secs(1), gate.wait_until_resolved())
        .await
        .expect("kick validated room A's pending seat");

    // Model the completed A-to-B role transition while the kick is paused:
    // the same player now holds only a pending seat in B. The kick may evict
    // A's old seat, but it must leave B's new credential alone.
    let room_b = server
        .database
        .create_room(
            "moderation-game".into(),
            Some("OTHER12".into()),
            4,
            true,
            target,
            "relay".into(),
            "local".into(),
            None,
        )
        .await
        .unwrap();
    let seat_b = room_b.players.get(&target).cloned().unwrap();
    manager
        .register_disconnection(target, room_b.id, false, Some(seat_b), 0)
        .await;
    server
        .database
        .remove_player_from_room(&room_b.id, &target)
        .await
        .unwrap();
    gate.release_resolved();
    timeout(Duration::from_secs(1), gate.wait_until_routed())
        .await
        .expect("kick checked current route");
    gate.release_routed();
    timeout(Duration::from_secs(1), kick)
        .await
        .expect("kick finishes")
        .expect("kick task does not panic");

    assert_eq!(
        manager.pending_reconnection_room(&target).await,
        Some(room_b.id)
    );
    assert!(!manager.is_reconnection_kicked(&target).await);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn banned_players_pending_record_cannot_restore_the_seat() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut _authority_rx) =
        register_client(&server, "127.0.0.1:48152".parse().unwrap()).await;
    let (target, mut _target_rx) =
        register_client(&server, "127.0.0.1:48153".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut _authority_rx, "BANR10", "host").await;
    join_seated_player(&server, &target, &mut _target_rx, "BANR10", "guest").await;
    let room_id = server
        .get_client_room(&target)
        .await
        .expect("target seated");

    // The target's socket drops and its record is armed through the same
    // teardown path a real disconnect uses.
    let seat_info = server
        .database
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup succeeds")
        .expect("room remains present")
        .players
        .get(&target)
        .cloned()
        .expect("target holds a durable seat");
    let token = server
        .reconnection_manager()
        .expect("reconnection is enabled")
        .register_disconnection(
            target,
            room_id,
            false,
            Some(seat_info),
            server
                .connection_manager
                .game_data_epoch(&target)
                .unwrap_or(0),
        )
        .await;
    server
        .database
        .remove_player_from_room(&room_id, &target)
        .await
        .expect("disconnect removes the durable seat");
    server.connection_manager.remove_client(&target);
    server
        .message_coordinator
        .unregister_local_client(&target)
        .await
        .expect("disconnect removes the coordinator route");

    // The authority bans the vacated seat through the same durable write
    // the `BanPlayer` operation performs, in the state a claim that won the
    // room-mutation gate leaves behind: the ban is committed, but the
    // record is not tombstoned (the tombstone mark lands in a later gate
    // hold, and a teardown re-arm after a raced eviction carries a fresh
    // record that no tombstone can cover).
    server
        .database
        .set_room_ban(&room_id, &target, true)
        .await
        .expect("ban write succeeds");

    // The banned player's fresh socket must not restore the seat through
    // its pending record: the ban's only path back into the room is a
    // future fresh join after an unban. The fresh socket registers with the
    // connection manager and the coordinator the same way a real
    // reconnecting handshake does before its claim.
    use crate::coordination::ClientDeliveryHandle;

    let (sender, mut socket_rx) = mpsc::channel(8);
    let socket = server
        .connection_manager
        .register_client(
            sender.clone(),
            ConnectionCloseSignal::detached(),
            "127.0.0.1:48154".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("socket registration succeeds");
    server
        .message_coordinator
        .register_local_client(
            socket,
            None,
            ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
        )
        .await
        .expect("socket coordinator route registers");
    let effective_player_id = Arc::new(tokio::sync::RwLock::new(socket));
    let operation_id = RoomOperationId::new_v4();
    let restored = server
        .handle_reconnect_with_identity_operation(
            &socket,
            &target,
            &room_id,
            &token,
            Arc::clone(&effective_player_id),
            Some(operation_id),
        )
        .await;
    let seat_restored = restored;
    assert!(
        !seat_restored,
        "a banned player's reconnection claim must not restore the seat"
    );
    let failure = recv_until(&mut socket_rx, |message| {
        matches!(
            message,
            ServerMessage::ReconnectionFailed { .. } | ServerMessage::RoomOperationResult { .. }
        )
    })
    .await;
    let refused_code = match failure.as_ref() {
        ServerMessage::ReconnectionFailed { error_code, .. } => Some(error_code.clone()),
        ServerMessage::RoomOperationResult { result, .. } => match result.as_ref() {
            RoomOperationResult::ReconnectionFailed { error_code, .. } => Some(error_code.clone()),
            other => panic!("expected a reconnect refusal result, got {other:?}"),
        },
        other => panic!("expected a reconnect refusal, got {other:?}"),
    };
    assert!(
        matches!(refused_code.as_ref(), Some(ErrorCode::Banned)),
        "the ban refusal must surface the BANNED classification, got {refused_code:?}"
    );
    assert_eq!(
        *effective_player_id.read().await,
        socket,
        "a refused claim must leave the websocket's transient identity alone"
    );
    let room = server
        .database
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup succeeds")
        .expect("room remains present");
    assert!(
        !room.players.contains_key(&target),
        "a banned player must stay unseated after the refused claim"
    );

    // The un-tombstoned record stays claimable, so a later unban during
    // the reconnect window can still honor the credential; until then the
    // ban must refuse every claim, not just the first.
    if let Some(manager) = &server.reconnection_manager {
        let outcome = manager
            .claim_reconnection(&socket, &target, &room_id, &token)
            .await;
        assert!(
            outcome.is_ok(),
            "a ban refusal must leave the record claimable, not consume it with \
             a kick tombstone or delete it: {outcome:?}"
        );
    } else {
        panic!("reconnection manager must be active for this test");
    }
}

/// A room seal gates fresh admissions only: a pending reconnection record
/// armed before the seal still restores its holder into the sealed room,
/// with no password and no fresh-join perimeter. This is the pinned contract
/// (issue #396 cross-feature seam sweep): the seal's password perimeter is an
/// admission check, and `Reconnect` carries no password field. Resuming a
/// prior membership through a live bearer seat credential is not a fresh
/// admission — the same distinction that makes bans the tool that refuses
/// restores while seals do not. "Current members and their reconnection
/// tokens are unaffected" in the `SetRoomAccess` contract covers the
/// disconnected member holding a live record.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn sealed_room_restore_honors_a_pre_seal_record() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut _authority_rx) =
        register_client(&server, "127.0.0.1:48155".parse().unwrap()).await;
    let (target, mut _target_rx) =
        register_client(&server, "127.0.0.1:48156".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut _authority_rx, "SEAL10", "host").await;
    join_seated_player(&server, &target, &mut _target_rx, "SEAL10", "guest").await;
    let room_id = server
        .get_client_room(&target)
        .await
        .expect("target seated");

    // The guest's socket drops and its record is armed through the same
    // teardown path a real disconnect uses.
    let seat_info = server
        .database
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup succeeds")
        .expect("room remains present")
        .players
        .get(&target)
        .cloned()
        .expect("target holds a durable seat");
    let token = server
        .reconnection_manager()
        .expect("reconnection is enabled")
        .register_disconnection(
            target,
            room_id,
            false,
            Some(seat_info),
            server
                .connection_manager
                .game_data_epoch(&target)
                .unwrap_or(0),
        )
        .await;
    server
        .database
        .remove_player_from_room(&room_id, &target)
        .await
        .expect("disconnect removes the durable seat");
    server.connection_manager.remove_client(&target);
    server
        .message_coordinator
        .unregister_local_client(&target)
        .await
        .expect("disconnect removes the coordinator route");

    // The authority seals the vacated room while the record is live: the
    // same hashed-credential write the `SetRoomAccess` operation performs.
    server
        .database
        .set_room_password(
            &room_id,
            Some(crate::protocol::RoomPasswordCredential::new("seal-secret")),
        )
        .await
        .expect("seal write succeeds");

    // The pre-seal record restores its holder without any password: the
    // `Reconnect` message carries no password field, and the restore path is
    // not a fresh admission.
    use crate::coordination::ClientDeliveryHandle;

    let (sender, mut socket_rx) = mpsc::channel(8);
    let socket = server
        .connection_manager
        .register_client(
            sender.clone(),
            ConnectionCloseSignal::detached(),
            "127.0.0.1:48157".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("socket registration succeeds");
    server
        .message_coordinator
        .register_local_client(
            socket,
            None,
            ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
        )
        .await
        .expect("socket coordinator route registers");
    let effective_player_id = Arc::new(tokio::sync::RwLock::new(socket));
    let operation_id = RoomOperationId::new_v4();
    let restored = server
        .handle_reconnect_with_identity_operation(
            &socket,
            &target,
            &room_id,
            &token,
            Arc::clone(&effective_player_id),
            Some(operation_id),
        )
        .await;
    assert!(
        restored,
        "a pre-seal record must restore into the now-sealed room"
    );
    let room = server
        .database
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup succeeds")
        .expect("room remains present");
    assert!(
        room.players.contains_key(&target),
        "the restored holder is seated again"
    );
    let baseline = socket_rx.try_recv().expect("restore responds");
    let restored_payload = match baseline.as_ref() {
        ServerMessage::Reconnected(payload) => Some(payload),
        ServerMessage::RoomOperationResult { result, .. } => match result.as_ref() {
            RoomOperationResult::Reconnected(payload) => Some(payload),
            other => panic!("expected a reconnect result, got {other:?}"),
        },
        other => panic!("expected a reconnect response, got {other:?}"),
    };
    assert_eq!(
        restored_payload.expect("reconnected payload").player_id,
        target,
        "the restore delivers the Reconnected baseline for the recorded player"
    );
}

/// Drive a seated join that may carry a join password and assert the
/// terminal `RoomJoinFailed` classification when the join is refused.
async fn join_with_password(
    server: &Arc<EnhancedGameServer>,
    player_id: &PlayerId,
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    room_code: &str,
    name: &str,
    password: Option<&str>,
) -> Result<(), ErrorCode> {
    server
        .handle_join_room(
            player_id,
            "moderation-game".to_string(),
            Some(room_code.to_string()),
            name.to_string(),
            Some(4),
            Some(true),
            None,
            password.map(str::to_string),
            None,
        )
        .await;
    let terminal = recv_until(receiver, |message| {
        matches!(
            message,
            ServerMessage::RoomJoined(_) | ServerMessage::RoomJoinFailed { .. }
        )
    })
    .await;
    match terminal.as_ref() {
        ServerMessage::RoomJoined(_) => Ok(()),
        ServerMessage::RoomJoinFailed { error_code, .. } => {
            Err(error_code.clone().expect("join refusals carry a code"))
        }
        other => panic!("expected a join terminal, got {other:?}"),
    }
}

async fn room_access_result(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
) -> RoomOperationResult {
    let response = recv_until(receiver, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    let ServerMessage::RoomOperationResult { result, .. } = response.as_ref() else {
        panic!("expected correlated result, got {response:?}");
    };
    result.as_ref().clone()
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn set_room_access_seals_and_reopens_the_room() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48140".parse().unwrap()).await;
    let (latecomer, mut latecomer_rx) =
        register_client(&server, "127.0.0.1:48141".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "SECRET", "host").await;

    // Seal the room.
    server
        .handle_set_room_access_operation(
            &authority,
            RoomOperationId::new_v4(),
            Some("open sesame".to_string()),
        )
        .await;
    assert!(
        matches!(
            room_access_result(&mut authority_rx).await,
            RoomOperationResult::RoomAccessUpdated {
                requires_password: true
            }
        ),
        "sealing must report requires_password=true"
    );
    let room = server
        .database
        .get_room("moderation-game", "SECRET")
        .await
        .expect("room lookup succeeds")
        .expect("room exists");
    assert!(room.password.is_some(), "storage must hold the credential");

    // A join without the password is refused with the non-enumerating code;
    // a wrong password is indistinguishable.
    assert_eq!(
        join_with_password(
            &server,
            &latecomer,
            &mut latecomer_rx,
            "SECRET",
            "late",
            None
        )
        .await
        .expect_err("password-less join must be refused"),
        ErrorCode::PasswordRequired
    );
    assert_eq!(
        join_with_password(
            &server,
            &latecomer,
            &mut latecomer_rx,
            "SECRET",
            "late",
            Some("wrong")
        )
        .await
        .expect_err("wrong-password join must be refused"),
        ErrorCode::PasswordRequired
    );
    assert!(
        !server
            .database
            .get_room_by_id(&room.id)
            .await
            .expect("ok")
            .expect("room exists")
            .players
            .contains_key(&latecomer),
        "a refused join must not seat the player"
    );

    // The correct password admits.
    join_with_password(
        &server,
        &latecomer,
        &mut latecomer_rx,
        "SECRET",
        "late",
        Some("open sesame"),
    )
    .await
    .expect("correct password must admit");

    // Reopening restores open admission.
    server
        .handle_set_room_access_operation(&authority, RoomOperationId::new_v4(), None)
        .await;
    assert!(
        matches!(
            room_access_result(&mut authority_rx).await,
            RoomOperationResult::RoomAccessUpdated {
                requires_password: false
            }
        ),
        "reopening must report requires_password=false"
    );
    let (another, mut another_rx) =
        register_client(&server, "127.0.0.1:48142".parse().unwrap()).await;
    join_with_password(&server, &another, &mut another_rx, "SECRET", "fresh", None)
        .await
        .expect("an open room must admit without a password");
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn creation_time_password_seals_the_room_from_birth() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (creator, mut creator_rx) =
        register_client(&server, "127.0.0.1:48143".parse().unwrap()).await;
    let (latecomer, mut latecomer_rx) =
        register_client(&server, "127.0.0.1:48144".parse().unwrap()).await;

    // The creating join carries the password: there is no unlocked window.
    join_with_password(
        &server,
        &creator,
        &mut creator_rx,
        "BIRTH1",
        "host",
        Some("secret"),
    )
    .await
    .expect("creating join succeeds");
    let room = server
        .database
        .get_room("moderation-game", "BIRTH1")
        .await
        .expect("room lookup succeeds")
        .expect("room exists");
    assert!(room.password.is_some(), "creation must seal the room");

    assert_eq!(
        join_with_password(
            &server,
            &latecomer,
            &mut latecomer_rx,
            "BIRTH1",
            "late",
            None
        )
        .await
        .expect_err("uninvited join must be refused"),
        ErrorCode::PasswordRequired
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn stray_password_join_into_open_room_fails_closed() {
    // Issue #546 (authority squat): a password-carrying join states the
    // intent to enter a sealed room. When the code already exists as an open
    // room, the room under that code was created by someone else (the
    // squatter), so seating the requester would deliver them to the
    // squatter's authority. The refusal is the same non-enumerating
    // `PASSWORD_REQUIRED` outcome a sealed-room mismatch produces.
    let server = create_test_server_with(ServerConfig::default()).await;
    let (squatter, mut squatter_rx) =
        register_client(&server, "127.0.0.1:48150".parse().unwrap()).await;
    let (victim, mut victim_rx) =
        register_client(&server, "127.0.0.1:48151".parse().unwrap()).await;

    join_seated_player(&server, &squatter, &mut squatter_rx, "SQUAT1", "squatter").await;

    // Seated: the password-carrying join must not seat the victim into the
    // squatter's open room.
    assert_eq!(
        join_with_password(
            &server,
            &victim,
            &mut victim_rx,
            "SQUAT1",
            "victim",
            Some("open sesame")
        )
        .await
        .expect_err("stray-password join must be refused"),
        ErrorCode::PasswordRequired
    );
    assert!(
        !server
            .database
            .get_room("moderation-game", "SQUAT1")
            .await
            .expect("room lookup succeeds")
            .expect("room exists")
            .players
            .contains_key(&victim),
        "a refused join must not seat the player"
    );

    // The spectator path fails closed the same way.
    let spectator_error = server
        .spectator_service
        .join_operation(
            &victim,
            None,
            "moderation-game".to_string(),
            "SQUAT1".to_string(),
            "victim".to_string(),
            Some("open sesame".to_string()),
        )
        .await
        .expect_err("stray-password spectator join must be refused");
    assert_eq!(
        spectator_error.code,
        Some(ErrorCode::PasswordRequired),
        "spectator stray-password join must share the seated refusal: {spectator_error:?}"
    );

    // The open-room contract without a password is unchanged.
    let (friend, mut friend_rx) =
        register_client(&server, "127.0.0.1:48152".parse().unwrap()).await;
    join_with_password(&server, &friend, &mut friend_rx, "SQUAT1", "friend", None)
        .await
        .expect("open rooms still admit password-less joins");
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn set_room_access_refusals_are_classified_per_failure() {
    let cases: &[(&str, Option<&str>, ErrorCode)] = &[
        ("empty password", Some(""), ErrorCode::InvalidInput),
        (
            "oversized password",
            Some(&"x".repeat(crate::protocol::MAX_ROOM_PASSWORD_LENGTH + 1)),
            ErrorCode::InvalidInput,
        ),
    ];
    for (name, password, expected) in cases {
        let server = create_test_server_with(ServerConfig::default()).await;
        let (authority, mut authority_rx) =
            register_client(&server, "127.0.0.1:48145".parse().unwrap()).await;
        join_seated_player(&server, &authority, &mut authority_rx, "REFUS1", "host").await;

        server
            .handle_set_room_access_operation(
                &authority,
                RoomOperationId::new_v4(),
                password.map(str::to_string),
            )
            .await;
        let response = recv_until(&mut authority_rx, |message| {
            matches!(message, ServerMessage::RoomOperationResult { .. })
        })
        .await;
        assert_eq!(
            operation_failed_code(&response),
            *expected,
            "{name}: got {response:?}"
        );
        let room = server
            .database
            .get_room("moderation-game", "REFUS1")
            .await
            .expect("room lookup succeeds")
            .expect("room exists");
        assert!(
            room.password.is_none(),
            "{name}: a refused access update must not change the policy"
        );
    }

    // A non-authority member is refused regardless of the payload.
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48146".parse().unwrap()).await;
    let (member, mut member_rx) =
        register_client(&server, "127.0.0.1:48147".parse().unwrap()).await;
    join_seated_player(&server, &authority, &mut authority_rx, "REFUS2", "host").await;
    join_seated_player(&server, &member, &mut member_rx, "REFUS2", "guest").await;
    server
        .handle_set_room_access_operation(
            &member,
            RoomOperationId::new_v4(),
            Some("sneaky".to_string()),
        )
        .await;
    let response = recv_until(&mut member_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    assert_eq!(
        operation_failed_code(&response),
        ErrorCode::NotRoomAuthority,
        "non-authority access update must be refused: got {response:?}"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn ban_evicts_blocks_rejoin_and_lifts_on_unban() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48148".parse().unwrap()).await;
    let (target, mut target_rx, target_close_listener) =
        register_client_with_close_listener(&server, "127.0.0.1:48149".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "BAN001", "host").await;
    join_seated_player(&server, &target, &mut target_rx, "BAN001", "guest").await;

    server
        .handle_ban_player_operation(&authority, RoomOperationId::new_v4(), target)
        .await;

    // The banned seat is evicted through the same machinery as a kick.
    let _ = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::PlayerLeft { .. })
    })
    .await;
    assert!(
        matches!(
            room_access_result(&mut authority_rx).await,
            RoomOperationResult::PlayerBanned { player_id } if player_id == target
        ),
        "ban must report PlayerBanned"
    );
    assert_eq!(
        target_close_listener.requested_reason(),
        Some(CloseReason::Kicked),
        "a banned seat must close with the dedicated 4007 reason"
    );
    let room = server
        .database
        .get_room("moderation-game", "BAN001")
        .await
        .expect("room lookup succeeds")
        .expect("room exists");
    assert!(room.is_banned(&target), "storage must record the ban");

    // A kicked socket cannot join again. Use the arbitrary-id test seam to
    // isolate ban policy for the same identity on a fresh transport.
    server.unregister_client(&target).await;
    let (fresh_sender, fresh_receiver) = mpsc::channel(8);
    server.connect_client(target, fresh_sender).await;
    target_rx = fresh_receiver;
    let fresh_close = server
        .connection_manager
        .close_signal_for(&target)
        .expect("fresh target transport exists");
    assert_eq!(fresh_close.requested_reason(), None);
    assert_eq!(
        target_close_listener.requested_reason(),
        Some(CloseReason::Kicked),
        "fresh registration must not reset the old socket's terminal close"
    );

    // The banned id cannot rejoin as a player...
    assert_eq!(
        join_with_password(&server, &target, &mut target_rx, "BAN001", "guest", None)
            .await
            .expect_err("banned rejoin must be refused"),
        ErrorCode::Banned
    );
    // ...nor as a spectator.
    let spectator_err = server
        .spectator_service
        .join(
            &target,
            "moderation-game".to_string(),
            "BAN001".to_string(),
            "guest".to_string(),
        )
        .await
        .expect_err("banned spectator join must be refused");
    assert_eq!(
        spectator_err.code,
        Some(ErrorCode::Banned),
        "banned spectator join must be refused: got {spectator_err:?}"
    );

    // Lifting the ban restores admission.
    server
        .handle_unban_player_operation(&authority, RoomOperationId::new_v4(), target)
        .await;
    assert!(
        matches!(
            room_access_result(&mut authority_rx).await,
            RoomOperationResult::PlayerUnbanned { player_id } if player_id == target
        ),
        "unban must report PlayerUnbanned"
    );
    assert!(
        !server
            .database
            .get_room("moderation-game", "BAN001")
            .await
            .expect("ok")
            .expect("room exists")
            .is_banned(&target),
        "storage must clear the ban"
    );
    join_with_password(&server, &target, &mut target_rx, "BAN001", "guest", None)
        .await
        .expect("unbanned id must be able to rejoin");
    assert_eq!(fresh_close.requested_reason(), None);
    assert_eq!(
        target_close_listener.requested_reason(),
        Some(CloseReason::Kicked)
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn sealed_room_password_check_precedes_the_ban_refusal() {
    // Non-enumeration (issue #525): the join password is the outermost
    // admission perimeter on both paths. A banned caller that has not
    // presented the room credential learns only PASSWORD_REQUIRED; with the
    // credential, the refusal names the ban.
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48155".parse().unwrap()).await;
    let (target, mut target_rx) =
        register_client(&server, "127.0.0.1:48156".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "SEALB1", "host").await;
    join_seated_player(&server, &target, &mut target_rx, "SEALB1", "guest").await;

    server
        .handle_ban_player_operation(&authority, RoomOperationId::new_v4(), target)
        .await;
    let _ = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::PlayerLeft { .. })
    })
    .await;
    let _ = room_access_result(&mut authority_rx).await;

    server
        .handle_set_room_access_operation(
            &authority,
            RoomOperationId::new_v4(),
            Some("secret".to_string()),
        )
        .await;
    let _ = room_access_result(&mut authority_rx).await;

    // Seated path: without the credential the sealed perimeter answers
    // first; with it, the refusal names the ban.
    assert_eq!(
        join_with_password(&server, &target, &mut target_rx, "SEALB1", "guest", None)
            .await
            .expect_err("banned join without the credential must be refused"),
        ErrorCode::PasswordRequired,
        "the sealed perimeter must answer before the ban does"
    );
    assert_eq!(
        join_with_password(
            &server,
            &target,
            &mut target_rx,
            "SEALB1",
            "guest",
            Some("secret")
        )
        .await
        .expect_err("banned join with the credential must be refused"),
        ErrorCode::Banned
    );

    // The spectator path shares the ordering.
    let spectator_error = server
        .spectator_service
        .join_operation(
            &target,
            None,
            "moderation-game".to_string(),
            "SEALB1".to_string(),
            "guest".to_string(),
            None,
        )
        .await
        .expect_err("banned spectator join without the credential must be refused");
    assert_eq!(
        spectator_error.code,
        Some(ErrorCode::PasswordRequired),
        "the sealed perimeter must answer before the ban does: {spectator_error:?}"
    );
    let spectator_error = server
        .spectator_service
        .join_operation(
            &target,
            None,
            "moderation-game".to_string(),
            "SEALB1".to_string(),
            "guest".to_string(),
            Some("secret".to_string()),
        )
        .await
        .expect_err("banned spectator join with the credential must be refused");
    assert_eq!(spectator_error.code, Some(ErrorCode::Banned));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn rotation_and_transfer_preserve_the_ban_list_and_the_join_password() {
    // Pins the in-place room-write design (issue #525 sweep): rotating the
    // room code and moving the authority role must never rewrite the room
    // in a way that drops the ban list or the join password.
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48157".parse().unwrap()).await;
    let (successor, mut successor_rx) =
        register_client(&server, "127.0.0.1:48158".parse().unwrap()).await;
    let (target, mut target_rx) =
        register_client(&server, "127.0.0.1:48159".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "KEEPB1", "host").await;
    join_seated_player(&server, &successor, &mut successor_rx, "KEEPB1", "second").await;
    join_seated_player(&server, &target, &mut target_rx, "KEEPB1", "guest").await;
    let room_id = server
        .get_client_room(&authority)
        .await
        .expect("authority seated");

    server
        .handle_ban_player_operation(&authority, RoomOperationId::new_v4(), target)
        .await;
    let _ = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::PlayerLeft { .. })
    })
    .await;
    let _ = room_access_result(&mut authority_rx).await;
    server
        .handle_set_room_access_operation(
            &authority,
            RoomOperationId::new_v4(),
            Some("secret".to_string()),
        )
        .await;
    let _ = room_access_result(&mut authority_rx).await;

    server
        .handle_regenerate_room_code_operation(&authority, RoomOperationId::new_v4())
        .await;
    let new_code = match room_access_result(&mut authority_rx).await {
        RoomOperationResult::RoomCodeRegenerated { room_code } => room_code,
        other => panic!("expected RoomCodeRegenerated, got {other:?}"),
    };
    assert_ne!(new_code, "KEEPB1", "rotation must mint a fresh code");

    server
        .handle_transfer_authority_operation(&authority, RoomOperationId::new_v4(), successor)
        .await;
    let _ = room_access_result(&mut authority_rx).await;
    let room = server
        .database
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup succeeds")
        .expect("room remains present");
    assert_eq!(
        room.authority_player,
        Some(successor),
        "the transfer must have moved the authority role"
    );
    assert!(
        room.is_banned(&target),
        "the ban list must survive rotation and transfer"
    );

    // The preserved policy is live, not just stored: the fresh code refuses
    // a passwordless join with the sealed perimeter, and the banned id with
    // the correct credential is told BANNED under the new authority.
    let (fresh, mut fresh_rx) = register_client(&server, "127.0.0.1:48160".parse().unwrap()).await;
    assert_eq!(
        join_with_password(&server, &fresh, &mut fresh_rx, &new_code, "late", None)
            .await
            .expect_err("passwordless join into the rotated sealed room must be refused"),
        ErrorCode::PasswordRequired
    );
    assert_eq!(
        join_with_password(
            &server,
            &target,
            &mut target_rx,
            &new_code,
            "guest",
            Some("secret")
        )
        .await
        .expect_err("banned join must stay refused after rotation and transfer"),
        ErrorCode::Banned
    );
}

/// Issue #566 documented-contract pin: `RoomCodeRegenerated` is delivered
/// only to the acting authority and the transfer announcements
/// (`AuthorityChanged` for the room, `AuthorityTransferred` to the sender)
/// carry no room code, so a successor who inherits the role after a rotation
/// holds only their join-time code. The documented operator contract is to
/// share the new code out of band before or with the transfer. If a future
/// wire change adds a code query or a code field, this pin must be a
/// conscious edit. (The rotation's absence from the reconnection replay ring
/// is enforced by construction — the rotation path calls no
/// `record_room_event` — and is not pinned here.)
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn rotation_then_transfer_delivers_no_room_code_to_the_successor() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48173".parse().unwrap()).await;
    let (successor, mut successor_rx) =
        register_client(&server, "127.0.0.1:48174".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "ROTTR1", "host").await;
    join_seated_player(&server, &successor, &mut successor_rx, "ROTTR1", "heir").await;

    server
        .handle_regenerate_room_code_operation(&authority, RoomOperationId::new_v4())
        .await;
    let new_code = match room_access_result(&mut authority_rx).await {
        RoomOperationResult::RoomCodeRegenerated { room_code } => room_code,
        other => panic!("expected RoomCodeRegenerated, got {other:?}"),
    };
    assert_ne!(new_code, "ROTTR1", "rotation must mint a fresh code");
    // Every frame the successor holds after the rotation; the handlers
    // enqueue before returning, so a try_recv drain cannot miss delivery.
    let after_rotation = drain_receiver(&mut successor_rx);

    server
        .handle_transfer_authority_operation(&authority, RoomOperationId::new_v4(), successor)
        .await;
    let _ = room_access_result(&mut authority_rx).await;
    let mut delivered = drain_receiver(&mut successor_rx);
    // The role grant itself proves the successor's channel was live: a
    // code-bearing announcement would have arrived on it too, and the
    // non-vacuity guard below keeps this pin from passing on a dead channel.
    assert!(
        delivered
            .iter()
            .any(|message| matches!(message.as_ref(), ServerMessage::AuthorityChanged { .. })),
        "the successor must receive the AuthorityChanged role grant, got {delivered:?}"
    );
    delivered.extend(after_rotation);

    for message in &delivered {
        let frame = serde_json::to_string(message.as_ref()).expect("server messages serialize");
        assert!(
            !frame.contains(&new_code),
            "the successor must never receive the current code, got {frame}"
        );
        assert!(
            !matches!(
                message.as_ref(),
                ServerMessage::RoomOperationResult { result, .. }
                    if matches!(result.as_ref(), RoomOperationResult::RoomCodeRegenerated { .. })
            ),
            "the rotation result must never reach a non-acting member, got {message:?}"
        );
    }

    // The divergence is real and bounded: storage holds the fresh code while
    // the successor's only in-protocol view is their join-time code.
    let room = server
        .database
        .get_room("moderation-game", &new_code)
        .await
        .expect("new-code lookup succeeds")
        .expect("the room resolves under the new code");
    assert_eq!(room.authority_player, Some(successor));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn ban_refusals_mirror_the_kick_classification() {
    let cases: &[(&str, ErrorCode)] = &[
        ("non-member target", ErrorCode::KickTargetNotFound),
        ("self-target", ErrorCode::InvalidInput),
    ];
    for (name, expected) in cases {
        let server = create_test_server_with(ServerConfig::default()).await;
        let (authority, mut authority_rx) =
            register_client(&server, "127.0.0.1:48150".parse().unwrap()).await;
        join_seated_player(&server, &authority, &mut authority_rx, "BANR01", "host").await;

        let target = match *name {
            "self-target" => authority,
            _ => PlayerId::new_v4(),
        };
        server
            .handle_ban_player_operation(&authority, RoomOperationId::new_v4(), target)
            .await;
        let response = recv_until(&mut authority_rx, |message| {
            matches!(message, ServerMessage::RoomOperationResult { .. })
        })
        .await;
        assert_eq!(
            operation_failed_code(&response),
            *expected,
            "{name}: got {response:?}"
        );
        let room = server
            .database
            .get_room("moderation-game", "BANR01")
            .await
            .expect("room lookup succeeds")
            .expect("room exists");
        assert!(
            !room.is_banned(&target),
            "{name}: a refused ban must not record the ban"
        );
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transfer_authority_moves_the_role_and_notifies_members() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48151".parse().unwrap()).await;
    let (successor, mut successor_rx) =
        register_client(&server, "127.0.0.1:48152".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "XFER01", "host").await;
    join_seated_player(&server, &successor, &mut successor_rx, "XFER01", "guest").await;

    server
        .handle_transfer_authority_operation(&authority, RoomOperationId::new_v4(), successor)
        .await;

    // Both members observe the personalized AuthorityChanged announcement.
    let authority_view = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::AuthorityChanged { .. })
    })
    .await;
    let ServerMessage::AuthorityChanged {
        authority_player,
        you_are_authority,
    } = authority_view.as_ref()
    else {
        panic!("expected AuthorityChanged, got {authority_view:?}");
    };
    assert_eq!(*authority_player, Some(successor));
    assert!(
        !you_are_authority,
        "the former authority must learn it lost the role"
    );

    let successor_view = recv_until(&mut successor_rx, |message| {
        matches!(message, ServerMessage::AuthorityChanged { .. })
    })
    .await;
    let ServerMessage::AuthorityChanged {
        you_are_authority, ..
    } = successor_view.as_ref()
    else {
        panic!("expected AuthorityChanged, got {successor_view:?}");
    };
    assert!(
        *you_are_authority,
        "the new authority must learn it holds the role"
    );

    // Durable truth and the correlated terminal.
    let room = server
        .database
        .get_room("moderation-game", "XFER01")
        .await
        .expect("room lookup succeeds")
        .expect("room exists");
    assert_eq!(
        room.authority_player,
        Some(successor),
        "storage must hold the new authority"
    );
    assert!(matches!(
        room.players.get(&authority),
        Some(info) if !info.is_authority
    ));
    assert!(matches!(
        room.players.get(&successor),
        Some(info) if info.is_authority
    ));

    assert!(matches!(
        room_access_result(&mut authority_rx).await,
        RoomOperationResult::AuthorityTransferred { player_id } if player_id == successor
    ));

    // The transferred-away authority can no longer moderate.
    server
        .handle_set_room_access_operation(
            &authority,
            RoomOperationId::new_v4(),
            Some("no longer mine".to_string()),
        )
        .await;
    let response = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    assert_eq!(
        operation_failed_code(&response),
        ErrorCode::NotRoomAuthority,
        "the former authority must lose its moderation surface"
    );

    // The new authority can transfer the role back.
    server
        .handle_transfer_authority_operation(&successor, RoomOperationId::new_v4(), authority)
        .await;
    let room = server
        .database
        .get_room("moderation-game", "XFER01")
        .await
        .expect("ok")
        .expect("room exists");
    assert_eq!(room.authority_player, Some(authority));
}

#[tokio::test(start_paused = true)]
#[cfg_attr(miri, ignore)]
/// #1602: a departing peer cannot accept a new authority role.
async fn transfer_authority_refuses_a_target_with_an_armed_disconnect() {
    let server = create_test_server_with_session(SessionConfig {
        default_topology: crate::protocol::Topology::Mesh,
        ..SessionConfig::default()
    })
    .await;
    let (room_id, peers, _) = live_moderation_room(&server, "CLOSE1", 3).await;
    let [authority_peer, target_peer, mut survivor]: [LiveModerationPeer; 3] =
        peers.try_into().ok().unwrap();
    let authority = authority_peer.id;
    let target = target_peer.id;
    let mut authority_rx = authority_peer.receiver;
    let _target_rx = target_peer.receiver;
    let manager = server.reconnection_manager().unwrap();
    let token = manager.pre_issue_token(target, room_id).await;
    let gate = server.install_reconnect_teardown_test_gate();
    let disconnect = tokio::spawn({
        let server = server.clone();
        async move { server.unregister_client(&target).await }
    });
    timeout(Duration::from_secs(1), gate.wait_until_armed())
        .await
        .unwrap();
    let operation_id = RoomOperationId::new_v4();
    server
        .handle_transfer_authority_operation(&authority, operation_id, target)
        .await;
    let messages = drain_receiver(&mut authority_rx);
    gate.release();
    timeout(Duration::from_secs(1), disconnect)
        .await
        .unwrap()
        .unwrap();
    let terminal = messages
        .iter()
        .find(|message| matches!(message.as_ref(), ServerMessage::RoomOperationResult { operation_id: id, .. } if *id == operation_id))
        .expect("transfer must return one correlated result");
    assert_eq!(
        operation_failed_code(terminal),
        ErrorCode::TransferTargetNotFound
    );
    assert!(!messages
        .iter()
        .any(|message| matches!(message.as_ref(), ServerMessage::AuthorityChanged { .. })));
    let claim = manager
        .claim_reconnection(&PlayerId::new_v4(), &target, &room_id, &token)
        .await
        .unwrap();
    assert!(!claim.disconnected.was_authority);
    assert!(manager.release_reconnection_claim(&claim).await);
    assert_eq!(
        server
            .database
            .get_room_by_id(&room_id)
            .await
            .unwrap()
            .unwrap()
            .authority_player,
        Some(authority)
    );
    let generation =
        assert_transfer_target_reconnect(&server, target, room_id, &token, authority).await;
    drain_receiver(&mut survivor.receiver);
    assert_live_moderation_signal(&server, target, &mut survivor, generation).await;
    assert_live_moderation_signal(&server, authority, &mut survivor, generation).await;
    assert_eq!(survivor.close.requested_reason(), None);
}

#[tokio::test(start_paused = true)]
#[cfg_attr(miri, ignore)]
/// #1603: an admitted role change precedes the disconnect snapshot.
async fn disconnect_snapshot_waits_for_an_admitted_authority_transfer() {
    for before_commit in [true, false] {
        assert_disconnect_snapshot_waits_for_transfer(before_commit).await;
    }
}

async fn assert_disconnect_snapshot_waits_for_transfer(before_commit: bool) {
    let server = create_test_server_with_session(SessionConfig {
        default_topology: crate::protocol::Topology::Mesh,
        ..SessionConfig::default()
    })
    .await;
    let (room_id, peers, _) = live_moderation_room(&server, "CLOSE2", 3).await;
    let [authority_peer, target_peer, mut survivor]: [LiveModerationPeer; 3] =
        peers.try_into().ok().unwrap();
    let authority = authority_peer.id;
    let target = target_peer.id;
    let mut authority_rx = authority_peer.receiver;
    let _target_rx = target_peer.receiver;
    let manager = server.reconnection_manager().unwrap();
    let token = manager.pre_issue_token(target, room_id).await;
    let database = server
        .database
        .as_any()
        .downcast_ref::<crate::database::InMemoryDatabase>()
        .unwrap();
    let before_commit_pause =
        before_commit.then(|| super::moderation::arm_transfer_commit_pause(target));
    if !before_commit {
        database.pause_authority_request_after_commit_for_test();
    }
    let transfer = tokio::spawn({
        let server = server.clone();
        async move {
            server
                .handle_transfer_authority_operation(&authority, RoomOperationId::new_v4(), target)
                .await
        }
    });
    if let Some(pause) = &before_commit_pause {
        timeout(Duration::from_secs(1), pause.reached.notified())
            .await
            .unwrap();
    } else {
        timeout(
            Duration::from_secs(1),
            database.wait_for_authority_request_commit_for_test(),
        )
        .await
        .unwrap();
    }
    let mut disconnect = Box::pin(server.unregister_client(&target));
    assert!(futures_util::poll!(&mut disconnect).is_pending());
    timeout(Duration::from_secs(1), async {
        while server
            .connection_manager
            .close_signal_for(&target)
            .unwrap()
            .requested_reason()
            .is_none()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    if !before_commit {
        assert!(
            !manager.has_pending_reconnection(&target).await,
            "a snapshot cannot overtake an admitted room mutation"
        );
    }
    if let Some(pause) = before_commit_pause {
        pause.release.notify_one();
    } else {
        database.release_authority_request_commit_for_test();
    }
    timeout(Duration::from_secs(1), transfer)
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(1), &mut disconnect)
        .await
        .unwrap();
    let claim = manager
        .claim_reconnection(&PlayerId::new_v4(), &target, &room_id, &token)
        .await
        .unwrap();
    assert!(claim.disconnected.was_authority);
    assert!(manager.release_reconnection_claim(&claim).await);
    drain_receiver(&mut authority_rx);
    let generation =
        assert_transfer_target_reconnect(&server, target, room_id, &token, target).await;
    drain_receiver(&mut survivor.receiver);
    assert_live_moderation_signal(&server, target, &mut survivor, generation).await;
    assert_live_moderation_signal(&server, authority, &mut survivor, generation).await;
    assert_eq!(survivor.close.requested_reason(), None);
}

async fn assert_transfer_target_reconnect(
    server: &Arc<EnhancedGameServer>,
    target: PlayerId,
    room_id: RoomId,
    token: &str,
    expected_authority: PlayerId,
) -> crate::protocol::SessionGeneration {
    let (sender, mut receiver) = mpsc::channel(16);
    let socket = server
        .connection_manager
        .register_client(
            sender.clone(),
            ConnectionCloseSignal::detached(),
            "127.0.0.1:48625".parse().unwrap(),
            server.instance_id,
        )
        .await
        .unwrap();
    server
        .message_coordinator
        .register_local_client(
            socket,
            None,
            ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
        )
        .await
        .unwrap();
    server.set_client_protocol(
        &socket,
        NegotiatedProtocol {
            version: 3,
            transports: vec![
                crate::protocol::Transport::Relay,
                crate::protocol::Transport::WebRtc,
            ],
            topologies: vec![
                crate::protocol::Topology::Relay,
                crate::protocol::Topology::Mesh,
            ],
        },
    );
    assert!(
        server
            .handle_reconnect_with_identity_operation(
                &socket,
                &target,
                &room_id,
                token,
                Arc::new(tokio::sync::RwLock::new(socket)),
                Some(RoomOperationId::new_v4())
            )
            .await
    );
    let messages = drain_receiver(&mut receiver);
    let payload = messages
        .iter()
        .find_map(|message| match message.as_ref() {
            ServerMessage::RoomOperationResult { result, .. } => match result.as_ref() {
                RoomOperationResult::Reconnected(payload) => Some(payload),
                _ => None,
            },
            _ => None,
        })
        .expect("reconnect must return its authoritative baseline");
    assert_eq!(payload.player_id, target);
    assert_eq!(payload.is_authority, expected_authority == target);
    assert_eq!(
        payload
            .current_players
            .iter()
            .find(|player| player.id == expected_authority)
            .map(|player| player.is_authority),
        Some(true)
    );
    let room = server
        .database
        .get_room_by_id(&room_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(room.authority_player, Some(expected_authority));
    assert_eq!(
        room.players.get(&target).unwrap().is_authority,
        expected_authority == target
    );
    messages
        .iter()
        .find_map(|message| match message.as_ref() {
            ServerMessage::SessionPlan(plan) => {
                assert_eq!(plan.transport, crate::protocol::Transport::WebRtc);
                assert_eq!(plan.topology, crate::protocol::Topology::Mesh);
                Some(plan.generation)
            }
            _ => None,
        })
        .expect("restored peer must receive its live WebRTC plan")
}

/// Issue #396 sweep, same class as the kick path's rerouted-target handling:
/// a durable row can outlive its membership as residue from a storage-failed
/// detach while the player is routed elsewhere. Granting the role onto such a
/// row would announce an authority that can never act for this room and
/// later lose the role silently when the residue is repaired — a stale view
/// no event would ever correct. The transfer must refuse it.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transfer_authority_refuses_a_stale_residue_row() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48153".parse().unwrap()).await;
    let (target, mut target_rx) =
        register_client(&server, "127.0.0.1:48154".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "XFER02", "host").await;
    join_seated_player(&server, &target, &mut target_rx, "XFER02", "guest").await;

    let stale_room = server
        .database
        .get_room("moderation-game", "XFER02")
        .await
        .expect("room lookup succeeds")
        .expect("authority room exists");
    let target_row = server
        .database
        .get_room_players(&stale_room.id)
        .await
        .expect("roster readable")
        .into_iter()
        .find(|player| player.id == target)
        .expect("target seated in the authority room");

    // Build the stale-residue state a storage-failed detach leaves behind:
    // the target's live connection is routed in another room while its row
    // in the authority's room is still present.
    assert!(
        server
            .database
            .remove_player_from_room(&stale_room.id, &target)
            .await
            .expect("durable removal succeeds")
            .is_some(),
        "test setup removes the target's seat"
    );
    let live_room = server
        .database
        .create_room(
            "moderation-game".to_string(),
            Some("XFER99".to_string()),
            4,
            false,
            target,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("target's live room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&target, live_room.id)
        .await;
    server
        .database
        .add_player_to_room(&stale_room.id, target_row)
        .await
        .expect("stale row planted");

    server
        .handle_transfer_authority_operation(&authority, RoomOperationId::new_v4(), target)
        .await;

    // The refusal is the same non-member classification the routed check
    // already produces, and the role never moves.
    let response = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    assert_eq!(
        operation_failed_code(&response),
        ErrorCode::TransferTargetNotFound,
        "a residue row is not a transferable member"
    );
    let room_after = server
        .database
        .get_room("moderation-game", "XFER02")
        .await
        .expect("room lookup succeeds")
        .expect("room survives the refused transfer");
    assert_eq!(
        room_after.authority_player,
        Some(authority),
        "the role must stay with the authority"
    );
    let stray_announcements = drain_receiver(&mut authority_rx)
        .into_iter()
        .filter(|message| matches!(message.as_ref(), ServerMessage::AuthorityChanged { .. }))
        .count();
    assert_eq!(
        stray_announcements, 0,
        "a refused transfer must not announce any AuthorityChanged"
    );

    // The target's live membership elsewhere is untouched.
    assert_eq!(
        server.get_client_room(&target).await,
        Some(live_room.id),
        "the refused transfer must not disturb the target's live route"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transfer_authority_refusals_are_classified_per_failure() {
    let cases: &[(&str, ErrorCode)] = &[
        ("non-member target", ErrorCode::TransferTargetNotFound),
        ("self-transfer", ErrorCode::InvalidInput),
    ];
    for (name, expected) in cases {
        let server = create_test_server_with(ServerConfig::default()).await;
        let (authority, mut authority_rx) =
            register_client(&server, "127.0.0.1:48153".parse().unwrap()).await;
        join_seated_player(&server, &authority, &mut authority_rx, "XFER02", "host").await;

        let target = match *name {
            "self-transfer" => authority,
            _ => PlayerId::new_v4(),
        };
        server
            .handle_transfer_authority_operation(&authority, RoomOperationId::new_v4(), target)
            .await;
        let response = recv_until(&mut authority_rx, |message| {
            matches!(message, ServerMessage::RoomOperationResult { .. })
        })
        .await;
        assert_eq!(
            operation_failed_code(&response),
            *expected,
            "{name}: got {response:?}"
        );
        let room = server
            .database
            .get_room("moderation-game", "XFER02")
            .await
            .expect("room lookup succeeds")
            .expect("room exists");
        assert_eq!(
            room.authority_player,
            Some(authority),
            "{name}: a refused transfer must not disturb the role"
        );
    }
}

/// Build a server with the given session policy, so the finalize path can
/// resolve to a host-topology plan (same shape as `ready_state_tests`).
async fn create_test_server_with_session(session: SessionConfig) -> Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
        ServerConfig::default(),
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        session,
        TurnConfig::default(),
        crate::database::DatabaseConfig::InMemory,
        MetricsConfig::default(),
        CoordinationConfig::default(),
        TransportSecurityConfig::default(),
        Vec::new(),
    )
    .await
    .expect("failed to construct test server")
}

/// Issue #554, decided semantics ("the role follows the plan, not vice
/// versa"): a mid-game `TransferAuthority` moves the moderation and
/// start-game role, but the running session's transport host stays pinned to
/// its finalize-time election. Only a departure (host failover) re-plans the
/// session. Documented in `docs/concepts/authority.md` ("The Role and the
/// Transport Host Are Not the Same Thing").
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn mid_game_transfer_moves_the_role_but_not_the_session_host() {
    let server = create_test_server_with_session(SessionConfig {
        default_topology: crate::protocol::Topology::Host,
        ..SessionConfig::default()
    })
    .await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48161".parse().unwrap()).await;
    let (successor, mut successor_rx) =
        register_client(&server, "127.0.0.1:48162".parse().unwrap()).await;
    let v3_host = crate::server::NegotiatedProtocol {
        version: 3,
        transports: vec![
            crate::protocol::Transport::Relay,
            crate::protocol::Transport::WebRtc,
        ],
        topologies: vec![
            crate::protocol::Topology::Relay,
            crate::protocol::Topology::Host,
        ],
    };
    server.set_client_protocol(&authority, v3_host.clone());
    server.set_client_protocol(&successor, v3_host);

    // A 2-seat authority room, driven through the REAL finalize flow: both
    // members join, the room moves to the lobby, both toggle ready, and an
    // explicit StartGame finalizes with a host-topology session plan.
    join_seated_player(&server, &authority, &mut authority_rx, "MIDGM1", "host").await;
    join_seated_player(&server, &successor, &mut successor_rx, "MIDGM1", "guest").await;
    let room = server
        .database
        .get_room("moderation-game", "MIDGM1")
        .await
        .expect("room lookup succeeds")
        .expect("room exists");
    server
        .database
        .transition_room_to_lobby(&room.id)
        .await
        .expect("lobby transition succeeds");
    server.handle_player_ready(&authority).await;
    server.handle_player_ready(&successor).await;
    server.handle_start_game(&authority).await;

    let plan_before = server
        .active_session_plan(&room.id)
        .expect("a finalized host-topology room must hold an active session plan");
    assert_eq!(
        plan_before.topology,
        crate::protocol::Topology::Host,
        "this test pins host-topology semantics"
    );
    assert!(
        plan_before.host.is_some(),
        "a host-topology plan must have elected a transport host"
    );

    drain_receiver(&mut authority_rx);
    drain_receiver(&mut successor_rx);

    // Mid-game transfer of the authority role.
    server
        .handle_transfer_authority_operation(&authority, RoomOperationId::new_v4(), successor)
        .await;

    // The moderation role moved, and the room is still mid-game.
    let room_after = server
        .database
        .get_room_by_id(&room.id)
        .await
        .expect("room lookup succeeds")
        .expect("room exists");
    assert_eq!(
        room_after.authority_player,
        Some(successor),
        "the transfer must grant the successor the authority role"
    );
    assert_eq!(
        room_after.lobby_state,
        crate::protocol::LobbyState::Finalized,
        "the room must stay finalized across the transfer"
    );

    // The session plan did NOT follow the role: identical sticky decision.
    let plan_after = server
        .active_session_plan(&room.id)
        .expect("the active plan must survive the transfer");
    assert_eq!(
        plan_after.topology, plan_before.topology,
        "the session topology must not change on a transfer"
    );
    assert_eq!(
        plan_after.transport, plan_before.transport,
        "the session transport must not change on a transfer"
    );
    assert_eq!(
        plan_after.host, plan_before.host,
        "the transport host must not follow the moderation role"
    );

    // No re-plan traffic: members learn only the role change.
    for (receiver, who) in [
        (&mut authority_rx, "authority"),
        (&mut successor_rx, "successor"),
    ] {
        let messages = drain_receiver(receiver);
        assert!(
            messages
                .iter()
                .any(|message| matches!(message.as_ref(), ServerMessage::AuthorityChanged { .. })),
            "{who} must learn the role change"
        );
        assert!(
            !messages
                .iter()
                .any(|message| matches!(message.as_ref(), ServerMessage::SessionPlan(_))),
            "{who} must not receive a fresh SessionPlan on a mid-game transfer"
        );
        assert!(
            !messages
                .iter()
                .any(|message| matches!(message.as_ref(), ServerMessage::GameStarting { .. })),
            "{who} must not observe a re-start on a mid-game transfer"
        );
    }
}

/// Refused-operation spam is the same one-write-one-reply amplification
/// channel as malformed-frame refusals, so the correlated failure envelopes
/// charge the per-connection error-reply budget (issue #518). An exhausted
/// budget withholds the next envelope and closes the connection with the
/// semantic `4006 inbound_rate_limited` reason instead.
#[tokio::test]
async fn moderation_refusal_envelopes_exhaust_the_error_reply_budget() {
    let mut config = ServerConfig::default();
    config.rate_limit_config.max_inbound_error_replies = 2;
    let server = create_test_server_with(config).await;
    let (player, mut receiver, close_listener) =
        register_client_with_close_listener(&server, "127.0.0.1:48260".parse().unwrap()).await;

    // A roomless sender refuses every SetRoomAccess with a correlated
    // failure envelope: the budgeted replies.
    for _ in 0..2 {
        server
            .handle_set_room_access_operation(&player, RoomOperationId::new_v4(), None)
            .await;
        let envelope = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("refusal envelope should arrive in time")
            .expect("channel stays open");
        assert!(
            matches!(
                envelope.as_ref(),
                ServerMessage::RoomOperationResult {
                    result,
                    ..
                } if matches!(result.as_ref(), RoomOperationResult::OperationFailed { .. })
            ),
            "the budgeted refusal must still produce its envelope, got {envelope:?}"
        );
    }

    // The budget-exhausting refusal produces no envelope; the farewell rides
    // instead and the semantic close reason is pinned.
    server
        .handle_set_room_access_operation(&player, RoomOperationId::new_v4(), None)
        .await;
    let drained = drain_receiver(&mut receiver);
    assert!(
        !drained
            .iter()
            .any(|message| matches!(message.as_ref(), ServerMessage::RoomOperationResult { .. })),
        "an exhausted budget must withhold the refusal envelope, got {drained:?}"
    );
    assert!(
        drained
            .iter()
            .any(|message| matches!(message.as_ref(), ServerMessage::Error { .. })),
        "the budget-exhaustion farewell must be sent"
    );
    assert_eq!(
        close_listener.requested_reason(),
        Some(CloseReason::InboundRateLimited),
        "the exhausted budget must close with the 4006 reason"
    );
}

/// A canceled caller must not hide a durable transfer from live peers or replay.
#[tokio::test]
async fn canceled_transfer_publishes_committed_authority_before_later_transfer() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48201".parse().unwrap()).await;
    let (successor, mut successor_rx) =
        register_client(&server, "127.0.0.1:48202".parse().unwrap()).await;
    join_seated_player(&server, &authority, &mut authority_rx, "CANCEL", "host").await;
    join_seated_player(&server, &successor, &mut successor_rx, "CANCEL", "guest").await;
    drain_receiver(&mut authority_rx);
    drain_receiver(&mut successor_rx);
    let room_id = server.get_client_room(&authority).await.unwrap();
    let replay = server.reconnection_manager.as_ref().unwrap();
    replay
        .register_disconnection(PlayerId::new_v4(), room_id, false, None, 0)
        .await;
    let before = replay.get_missed_events(&room_id, 0).await;
    let database = server
        .database
        .as_any()
        .downcast_ref::<crate::database::InMemoryDatabase>()
        .unwrap();
    database.pause_authority_request_after_commit_for_test();
    let operation_id = RoomOperationId::new_v4();
    let transfer_server = Arc::clone(&server);
    let caller = tokio::spawn(async move {
        transfer_server
            .handle_transfer_authority_operation(&authority, operation_id, successor)
            .await;
    });
    timeout(
        Duration::from_secs(2),
        database.wait_for_authority_request_commit_for_test(),
    )
    .await
    .unwrap();
    assert_eq!(
        database
            .get_room_by_id(&room_id)
            .await
            .unwrap()
            .unwrap()
            .authority_player,
        Some(successor)
    );
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    let next_server = Arc::clone(&server);
    let later = tokio::spawn(async move {
        next_server
            .handle_transfer_authority_operation(&successor, RoomOperationId::new_v4(), authority)
            .await;
    });
    tokio::task::yield_now().await;
    assert!(
        !later.is_finished(),
        "later transfer must not complete before the committed event"
    );
    database.release_authority_request_commit_for_test();
    let transferred = recv_until(&mut successor_rx, |_| true).await;
    assert!(
        matches!(transferred.as_ref(), ServerMessage::AuthorityChanged {
        authority_player: Some(id), you_are_authority: true,
    } if *id == successor)
    );
    let previous_view = recv_until(&mut authority_rx, |_| true).await;
    assert!(
        matches!(previous_view.as_ref(), ServerMessage::AuthorityChanged {
        authority_player: Some(id), you_are_authority: false,
    } if *id == successor)
    );
    let result = recv_until(&mut authority_rx, |_| true).await;
    assert!(
        matches!(result.as_ref(), ServerMessage::RoomOperationResult {
        operation_id: id, result,
    } if *id == operation_id && matches!(result.as_ref(), RoomOperationResult::AuthorityTransferred { player_id } if *player_id == successor))
    );
    timeout(Duration::from_secs(2), later)
        .await
        .unwrap()
        .unwrap();
    let returned = recv_until(&mut successor_rx, |_| true).await;
    assert!(
        matches!(returned.as_ref(), ServerMessage::AuthorityChanged {
        authority_player: Some(id), you_are_authority: false,
    } if *id == authority)
    );
    let after = replay.get_missed_events(&room_id, 0).await;
    assert_eq!(after.events.len(), before.events.len() + 2);
    assert!(
        matches!(after.events.get(before.events.len()), Some(ServerMessage::AuthorityChanged {
        authority_player: Some(id), ..
    }) if *id == successor)
    );
    assert!(
        matches!(after.events.last(), Some(ServerMessage::AuthorityChanged {
        authority_player: Some(id), ..
    }) if *id == authority)
    );
}

/// #1554: retained assignments cannot substitute for either live transfer route.
#[tokio::test]
async fn transfer_authority_refuses_pruned_source_or_target_routes() {
    for prune_target in [false, true] {
        let server = create_test_server_with(ServerConfig::default()).await;
        let (authority, mut authority_rx) =
            register_client(&server, "127.0.0.1:48201".parse().unwrap()).await;
        let (target, mut target_rx) =
            register_client(&server, "127.0.0.1:48202".parse().unwrap()).await;
        join_seated_player(&server, &authority, &mut authority_rx, "PRUNE1", "host").await;
        join_seated_player(&server, &target, &mut target_rx, "PRUNE1", "peer").await;
        drain_receiver(&mut authority_rx);
        drain_receiver(&mut target_rx);
        let room_id = server.get_client_room(&authority).await.unwrap();
        let guard = server
            .message_coordinator
            .lock_room_event_mutation(&room_id)
            .await;
        let mut transfer = Box::pin(server.handle_transfer_authority_operation(
            &authority,
            RoomOperationId::new_v4(),
            target,
        ));
        assert!(futures_util::poll!(&mut transfer).is_pending());
        let pruned = if prune_target { target } else { authority };
        server
            .message_coordinator
            .unregister_local_client(&pruned)
            .await
            .unwrap();
        assert_eq!(server.get_client_room(&pruned).await, Some(room_id));
        let before = server
            .database
            .get_room_by_id(&room_id)
            .await
            .unwrap()
            .unwrap();
        drop(guard);
        transfer.await;
        let after = server
            .database
            .get_room_by_id(&room_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            after.authority_player,
            Some(authority),
            "prune_target={prune_target}"
        );
        assert_eq!(after.last_activity, before.last_activity);
        let messages = drain_receiver(&mut authority_rx);
        assert!(messages
            .iter()
            .all(|message| !matches!(message.as_ref(), ServerMessage::AuthorityChanged { .. })));
        if prune_target {
            assert_eq!(messages.len(), 1);
            assert_eq!(
                operation_failed_code(&messages[0]),
                ErrorCode::TransferTargetNotFound
            );
        }
        assert!(drain_receiver(&mut target_rx).is_empty());
    }
}

/// #1554: route loss waits until the role update has returned from storage.
#[tokio::test]
async fn transfer_authority_fences_routes_through_the_storage_commit() {
    for remove_target in [false, true] {
        let server = create_test_server_with(ServerConfig::default()).await;
        let (authority, mut authority_rx) =
            register_client(&server, "127.0.0.1:48211".parse().unwrap()).await;
        let (target, mut target_rx) =
            register_client(&server, "127.0.0.1:48212".parse().unwrap()).await;
        join_seated_player(&server, &authority, &mut authority_rx, "FENCE1", "host").await;
        join_seated_player(&server, &target, &mut target_rx, "FENCE1", "peer").await;
        drain_receiver(&mut authority_rx);
        drain_receiver(&mut target_rx);
        let room_id = server.get_client_room(&authority).await.unwrap();
        let database = server
            .database
            .as_any()
            .downcast_ref::<crate::database::InMemoryDatabase>()
            .unwrap();
        database.pause_authority_request_after_commit_for_test();
        let caller = {
            let server = server.clone();
            tokio::spawn(async move {
                server
                    .handle_transfer_authority_operation(
                        &authority,
                        RoomOperationId::new_v4(),
                        target,
                    )
                    .await;
            })
        };
        timeout(
            Duration::from_secs(1),
            database.wait_for_authority_request_commit_for_test(),
        )
        .await
        .unwrap();
        let removed = if remove_target { target } else { authority };
        let mut removal = Box::pin(server.message_coordinator.unregister_local_client(&removed));
        assert!(futures_util::poll!(&mut removal).is_pending());
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert!(
            futures_util::poll!(&mut removal).is_pending(),
            "owned transfer must retain the fence after caller cancellation"
        );
        database.release_authority_request_commit_for_test();
        timeout(Duration::from_secs(1), removal)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            server
                .database
                .get_room_by_id(&room_id)
                .await
                .unwrap()
                .unwrap()
                .authority_player,
            Some(target)
        );
        timeout(Duration::from_secs(1), async {
            let survivor_rx = if remove_target { &mut authority_rx } else { &mut target_rx };
            loop {
                let message = survivor_rx.recv().await.unwrap();
                if matches!(message.as_ref(), ServerMessage::AuthorityChanged { authority_player: Some(id), .. } if *id == target) { break; }
            }
        }).await.expect("committed authority must still be announced to the survivor");
    }
}

/// #1554: a non-authority request keeps its role refusal before target errors.
#[tokio::test]
async fn transfer_authority_preserves_non_authority_error_precedence() {
    for retained_target in [false, true] {
        let server = create_test_server_with(ServerConfig::default()).await;
        let (authority, mut authority_rx) =
            register_client(&server, "127.0.0.1:48221".parse().unwrap()).await;
        let (member, mut member_rx) =
            register_client(&server, "127.0.0.1:48222".parse().unwrap()).await;
        join_seated_player(&server, &authority, &mut authority_rx, "ORDER1", "host").await;
        join_seated_player(&server, &member, &mut member_rx, "ORDER1", "peer").await;
        drain_receiver(&mut authority_rx);
        drain_receiver(&mut member_rx);
        let target = if retained_target {
            server
                .message_coordinator
                .unregister_local_client(&authority)
                .await
                .unwrap();
            authority
        } else {
            PlayerId::new_v4()
        };
        let room_id = server.get_client_room(&member).await.unwrap();
        let before = server
            .database
            .get_room_by_id(&room_id)
            .await
            .unwrap()
            .unwrap();
        server
            .handle_transfer_authority_operation(&member, RoomOperationId::new_v4(), target)
            .await;
        let response = member_rx.try_recv().unwrap();
        assert_eq!(
            operation_failed_code(&response),
            ErrorCode::NotRoomAuthority,
            "retained_target={retained_target}"
        );
        let after = server
            .database
            .get_room_by_id(&room_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.authority_player, before.authority_player);
        assert_eq!(after.last_activity, before.last_activity);
        assert_eq!(
            server
                .metrics
                .authority_transfer_conflicts
                .load(Ordering::Relaxed),
            0
        );
        let trailing = member_rx.try_recv();
        assert!(matches!(trailing, Err(mpsc::error::TryRecvError::Empty)));
    }
}

/// A caller can disappear after storage removes the target but before routing changes.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn canceled_kick_finishes_committed_departure_and_close() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:49201".parse().unwrap()).await;
    let (target, mut target_rx, close) =
        register_client_with_close_listener(&server, "127.0.0.1:49202".parse().unwrap()).await;
    join_seated_player(&server, &authority, &mut authority_rx, "KCANCL", "host").await;
    join_seated_player(&server, &target, &mut target_rx, "KCANCL", "peer").await;
    drain_receiver(&mut authority_rx);
    let room_id = server.get_client_room(&authority).await.unwrap();
    let database = server
        .database
        .as_any()
        .downcast_ref::<crate::database::InMemoryDatabase>()
        .unwrap();
    database.pause_next_removed_player_for_test();
    let operation_id = RoomOperationId::new_v4();
    let caller = tokio::spawn({
        let server = Arc::clone(&server);
        async move {
            server
                .handle_kick_player_operation(&authority, operation_id, target)
                .await
        }
    });
    timeout(
        Duration::from_secs(1),
        database.wait_for_removed_player_for_test(),
    )
    .await
    .unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    database.release_removed_player_for_test();
    let departed = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::PlayerLeft { .. })
    })
    .await;
    assert!(
        matches!(departed.as_ref(), ServerMessage::PlayerLeft { player_id, epoch: Some(_), final_seq: Some(_)} if *player_id == target)
    );
    let result = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    assert!(
        matches!(result.as_ref(), ServerMessage::RoomOperationResult { operation_id: actual, result } if *actual == operation_id && matches!(result.as_ref(), RoomOperationResult::PlayerKicked { player_id } if *player_id == target))
    );
    assert_eq!(server.get_client_room(&target).await, None);
    assert!(!server
        .database
        .get_room_by_id(&room_id)
        .await
        .unwrap()
        .unwrap()
        .players
        .contains_key(&target));
    assert_eq!(close.requested_reason(), Some(CloseReason::Kicked));
}

/// Canceling a storage-waiting rotation must still return the new join code and release both locks.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn canceled_room_code_rotation_finishes_and_releases_join_locks() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:49203".parse().unwrap()).await;
    join_seated_player(&server, &authority, &mut authority_rx, "RCOLD1", "host").await;
    drain_receiver(&mut authority_rx);
    let room_id = server.get_client_room(&authority).await.unwrap();
    server.script_room_codes_for_test(["RCNEW1"]);
    let database = server
        .database
        .as_any()
        .downcast_ref::<crate::database::InMemoryDatabase>()
        .unwrap();
    database.pause_next_update_room_code_for_test();
    let operation_id = RoomOperationId::new_v4();
    let caller = tokio::spawn({
        let server = Arc::clone(&server);
        async move {
            server
                .handle_regenerate_room_code_operation(&authority, operation_id)
                .await
        }
    });
    timeout(
        Duration::from_secs(1),
        database.wait_for_paused_update_room_code_for_test(),
    )
    .await
    .unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    database.release_paused_update_room_code_for_test();
    let result = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    assert!(
        matches!(result.as_ref(), ServerMessage::RoomOperationResult { operation_id: actual, result } if *actual == operation_id && matches!(result.as_ref(), RoomOperationResult::RoomCodeRegenerated { room_code } if room_code == "RCNEW1"))
    );
    assert!(server
        .database
        .get_room("moderation-game", "RCOLD1")
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        server
            .database
            .get_room("moderation-game", "RCNEW1")
            .await
            .unwrap()
            .unwrap()
            .id,
        room_id
    );
    for code in ["RCOLD1", "RCNEW1"] {
        assert!(!server
            .distributed_lock
            .is_locked(&format!("room_join:moderation-game:{code}"))
            .await
            .unwrap());
    }
}

/// A busy target in one match must not block removal in another match.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn busy_kick_target_does_not_block_an_unrelated_room() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let mut members = Vec::new();
    for (index, code) in ["ISOLA1", "ISOLA1", "ISOLA2", "ISOLA2"].iter().enumerate() {
        let (player, mut receiver) = register_client(
            &server,
            format!("127.0.0.1:{}", 49210 + index).parse().unwrap(),
        )
        .await;
        join_seated_player(
            &server,
            &player,
            &mut receiver,
            code,
            &format!("peer-{index}"),
        )
        .await;
        members.push((player, receiver));
    }
    let authority = members[0].0;
    let target = members[1].0;
    let other_authority = members[2].0;
    let other_target = members[3].0;
    let lifecycle = server.connection_manager.client_lifecycle(&target).unwrap();
    let held = lifecycle.lock().await;
    let attempts = lifecycle.lock_attempt_count_for_test();
    let blocked = tokio::spawn({
        let server = Arc::clone(&server);
        async move {
            server
                .handle_kick_player_operation(&authority, RoomOperationId::new_v4(), target)
                .await
        }
    });
    timeout(
        Duration::from_secs(1),
        lifecycle.wait_for_lock_attempt_after_for_test(attempts),
    )
    .await
    .unwrap();
    let other = timeout(
        Duration::from_secs(1),
        server.handle_kick_player_operation(
            &other_authority,
            RoomOperationId::new_v4(),
            other_target,
        ),
    )
    .await;
    drop(held);
    timeout(Duration::from_secs(1), blocked)
        .await
        .unwrap()
        .unwrap();
    assert!(
        other.is_ok(),
        "another room must complete while the first target is busy"
    );
    assert_eq!(server.get_client_room(&other_target).await, None);
}

struct LiveModerationPeer {
    id: PlayerId,
    receiver: mpsc::Receiver<Arc<ServerMessage>>,
    close: crate::coordination::ConnectionCloseListener,
}

/// Finalize a real server-side WebRTC mesh and retain each peer's opening generation.
async fn live_moderation_room(
    server: &Arc<EnhancedGameServer>,
    code: &str,
    count: usize,
) -> (
    RoomId,
    Vec<LiveModerationPeer>,
    crate::protocol::SessionGeneration,
) {
    let mut peers: Vec<LiveModerationPeer> = Vec::new();
    for index in 0..count {
        let (id, mut receiver, close) = register_client_with_close_listener(
            server,
            format!("127.0.0.1:{}", 49400 + index).parse().unwrap(),
        )
        .await;
        server.set_client_protocol(
            &id,
            NegotiatedProtocol {
                version: 3,
                transports: vec![
                    crate::protocol::Transport::Relay,
                    crate::protocol::Transport::WebRtc,
                ],
                topologies: vec![
                    crate::protocol::Topology::Relay,
                    crate::protocol::Topology::Mesh,
                ],
            },
        );
        join_seated_player(server, &id, &mut receiver, code, &format!("peer-{index}")).await;
        peers.push(LiveModerationPeer {
            id,
            receiver,
            close,
        });
        for peer in &mut peers {
            drain_receiver(&mut peer.receiver);
        }
    }
    for index in 0..peers.len() {
        server.handle_player_ready(&peers[index].id).await;
        for peer in &mut peers {
            drain_receiver(&mut peer.receiver);
        }
    }
    server.handle_start_game(&peers[0].id).await;
    let room_id = server.get_client_room(&peers[0].id).await.unwrap();
    let mut generation = None;
    for peer in &mut peers {
        let messages = drain_receiver(&mut peer.receiver);
        let plan = messages
            .iter()
            .find_map(|message| match message.as_ref() {
                ServerMessage::SessionPlan(plan) => Some(plan),
                _ => None,
            })
            .expect("every finalized v3 peer receives its plan");
        assert_eq!(plan.topology, crate::protocol::Topology::Mesh);
        assert_eq!(plan.transport, crate::protocol::Transport::WebRtc);
        assert_eq!(plan.peers.len(), count - 1);
        if let Some(generation) = generation {
            assert_eq!(plan.generation, generation);
        }
        generation = Some(plan.generation);
    }
    (room_id, peers, generation.unwrap())
}

async fn assert_live_moderation_signal(
    server: &EnhancedGameServer,
    sender: PlayerId,
    recipient: &mut LiveModerationPeer,
    generation: crate::protocol::SessionGeneration,
) {
    let signal = serde_json::json!({"candidate": "live-room-control"});
    server
        .handle_signal_in_generation(&sender, recipient.id, generation, signal.clone())
        .await;
    let received = recv_until(&mut recipient.receiver, |message| {
        matches!(message, ServerMessage::Signal { .. })
    })
    .await;
    assert!(
        matches!(received.as_ref(), ServerMessage::Signal { from, generation: actual, signal: actual_signal } if *from == sender && *actual == generation && *actual_signal == signal)
    );
}

fn assert_moderation_players_released(server: &EnhancedGameServer) {
    assert!(server
        .moderation_player_gates
        .inner
        .gates
        .lock()
        .unwrap()
        .is_empty());
    assert!(server
        .moderation_player_gates
        .inner
        .active
        .lock()
        .unwrap()
        .is_empty());
}

/// Remaining mesh peers keep their plan and signaling after a canceled, committed kick.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn canceled_kick_preserves_live_mesh_survivors() {
    let server = create_test_server_with_session(SessionConfig {
        default_topology: crate::protocol::Topology::Mesh,
        ..SessionConfig::default()
    })
    .await;
    let (room_id, mut peers, generation) = live_moderation_room(&server, "LIVEK1", 3).await;
    let authority = peers[0].id;
    let target = peers[1].id;
    let replay = server.reconnection_manager.as_ref().unwrap();
    replay
        .register_disconnection(PlayerId::new_v4(), room_id, false, None, 0)
        .await;
    for sequence in 1..=3 {
        server
            .handle_game_data(
                &target,
                serde_json::json!({"sequence": sequence}),
                None,
                None,
            )
            .await;
    }
    for index in [0, 2] {
        let frames = drain_receiver(&mut peers[index].receiver);
        assert_eq!(frames.iter().filter(|message| matches!(message.as_ref(), ServerMessage::GameData { from_player, .. } if *from_player == target)).count(), 3);
    }
    let expected_tail = server
        .connection_manager
        .current_relay_stamp_in_room(&target, &room_id)
        .unwrap();
    assert_eq!(expected_tail.seq, 3);
    let replay_before = replay.get_missed_events(&room_id, 0).await;
    let operation_id = RoomOperationId::new_v4();
    let database = server
        .database
        .as_any()
        .downcast_ref::<crate::database::InMemoryDatabase>()
        .unwrap();
    database.pause_next_removed_player_for_test();
    let caller = tokio::spawn({
        let server = Arc::clone(&server);
        async move {
            server
                .handle_kick_player_operation(&authority, operation_id, target)
                .await
        }
    });
    timeout(
        Duration::from_secs(1),
        database.wait_for_removed_player_for_test(),
    )
    .await
    .unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    database.release_removed_player_for_test();
    let departed = recv_until(&mut peers[0].receiver, |message| {
        matches!(message, ServerMessage::PlayerLeft { .. })
    })
    .await;
    assert!(
        matches!(departed.as_ref(), ServerMessage::PlayerLeft { player_id, epoch: Some(epoch), final_seq: Some(final_seq) } if *player_id == target && *epoch == expected_tail.epoch && *final_seq == expected_tail.seq)
    );
    let result = recv_until(&mut peers[0].receiver, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    assert!(
        matches!(result.as_ref(), ServerMessage::RoomOperationResult { operation_id: actual, result } if *actual == operation_id && matches!(result.as_ref(), RoomOperationResult::PlayerKicked { player_id } if *player_id == target))
    );
    let survivor_messages = drain_receiver(&mut peers[2].receiver);
    assert_eq!(survivor_messages.iter().filter(|message| matches!(message.as_ref(), ServerMessage::PlayerLeft { player_id, .. } if *player_id == target)).count(), 1);
    assert_eq!(peers[1].close.requested_reason(), Some(CloseReason::Kicked));
    assert_eq!(peers[0].close.requested_reason(), None);
    assert_eq!(peers[2].close.requested_reason(), None);
    assert_eq!(
        server.active_session_plan(&room_id).unwrap().transport,
        crate::protocol::Transport::WebRtc
    );
    assert_live_moderation_signal(&server, authority, &mut peers[2], generation).await;
    let replay_after = replay.get_missed_events(&room_id, 0).await;
    assert_eq!(replay_after.events.len(), replay_before.events.len() + 1);
    assert!(
        matches!(replay_after.events.last(), Some(ServerMessage::PlayerLeft { player_id, epoch: Some(epoch), final_seq: Some(final_seq) }) if *player_id == target && *epoch == expected_tail.epoch && *final_seq == expected_tail.seq)
    );
    assert!(!drain_receiver(&mut peers[0].receiver).iter().any(|message| matches!(message.as_ref(), ServerMessage::PlayerLeft { player_id, .. } if *player_id == target)));
    assert_moderation_players_released(&server);
}

/// A known committed code change remains observable and admits a fresh live mesh peer.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn canceled_committed_room_code_change_preserves_live_mesh_admission() {
    let server = create_test_server_with_session(SessionConfig {
        default_topology: crate::protocol::Topology::Mesh,
        ..SessionConfig::default()
    })
    .await;
    let (room_id, mut peers, _) = live_moderation_room(&server, "LIVEC1", 2).await;
    let authority = peers[0].id;
    let operation_id = RoomOperationId::new_v4();
    server.script_room_codes_for_test(["LIVEC2"]);
    let database = server
        .database
        .as_any()
        .downcast_ref::<crate::database::InMemoryDatabase>()
        .unwrap();
    database.pause_after_update_room_code_commit_for_test();
    let caller = tokio::spawn({
        let server = Arc::clone(&server);
        async move {
            server
                .handle_regenerate_room_code_operation(&authority, operation_id)
                .await
        }
    });
    timeout(
        Duration::from_secs(1),
        database.wait_for_paused_update_room_code_for_test(),
    )
    .await
    .unwrap();
    assert_eq!(
        server
            .database
            .get_room("moderation-game", "LIVEC2")
            .await
            .unwrap()
            .unwrap()
            .id,
        room_id
    );
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    database.release_paused_update_room_code_for_test();
    let response = recv_until(&mut peers[0].receiver, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    assert!(
        matches!(response.as_ref(), ServerMessage::RoomOperationResult { operation_id: actual, result } if *actual == operation_id && matches!(result.as_ref(), RoomOperationResult::RoomCodeRegenerated { room_code } if room_code == "LIVEC2"))
    );
    for code in ["LIVEC1", "LIVEC2"] {
        assert!(!server
            .distributed_lock
            .is_locked(&format!("room_join:moderation-game:{code}"))
            .await
            .unwrap());
    }
    let (joiner, mut receiver) = register_client(&server, "127.0.0.1:49403".parse().unwrap()).await;
    server.set_client_protocol(
        &joiner,
        NegotiatedProtocol {
            version: 3,
            transports: vec![
                crate::protocol::Transport::Relay,
                crate::protocol::Transport::WebRtc,
            ],
            topologies: vec![
                crate::protocol::Topology::Relay,
                crate::protocol::Topology::Mesh,
            ],
        },
    );
    join_seated_player(&server, &joiner, &mut receiver, "LIVEC2", "new-peer").await;
    assert_eq!(server.get_client_room(&joiner).await, Some(room_id));
    let mut generation = None;
    for peer in &mut peers {
        let message = recv_until(&mut peer.receiver, |message| {
            matches!(message, ServerMessage::SessionPlan(_))
        })
        .await;
        let ServerMessage::SessionPlan(plan) = message.as_ref() else {
            unreachable!()
        };
        assert_eq!(plan.transport, crate::protocol::Transport::WebRtc);
        assert_eq!(plan.peers.len(), 2);
        if let Some(generation) = generation {
            assert_eq!(plan.generation, generation);
        }
        generation = Some(plan.generation);
    }
    let message = recv_until(&mut receiver, |message| {
        matches!(message, ServerMessage::SessionPlan(_))
    })
    .await;
    assert!(
        matches!(message.as_ref(), ServerMessage::SessionPlan(plan) if Some(plan.generation) == generation && plan.peers.len() == 2)
    );
    assert_live_moderation_signal(&server, joiner, &mut peers[1], generation.unwrap()).await;
    assert!(server
        .database
        .get_room("moderation-game", "LIVEC1")
        .await
        .unwrap()
        .is_none());
}

/// One held lifecycle cannot stop an unrelated live mesh from removing a peer and signaling.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn busy_kick_keeps_an_unrelated_live_mesh_running() {
    let server = create_test_server_with_session(SessionConfig {
        default_topology: crate::protocol::Topology::Mesh,
        ..SessionConfig::default()
    })
    .await;
    let (_, first, _) = live_moderation_room(&server, "LIVEI1", 2).await;
    let (room_id, mut other, generation) = live_moderation_room(&server, "LIVEI2", 3).await;
    let authority = first[0].id;
    let target = first[1].id;
    let lifecycle = server.connection_manager.client_lifecycle(&target).unwrap();
    let held = lifecycle.lock().await;
    let attempts = lifecycle.lock_attempt_count_for_test();
    let blocked = tokio::spawn({
        let server = Arc::clone(&server);
        async move {
            server
                .handle_kick_player_operation(&authority, RoomOperationId::new_v4(), target)
                .await
        }
    });
    timeout(
        Duration::from_secs(1),
        lifecycle.wait_for_lock_attempt_after_for_test(attempts),
    )
    .await
    .unwrap();
    let other_authority = other[0].id;
    let other_target = other[1].id;
    let result = timeout(
        Duration::from_secs(1),
        server.handle_kick_player_operation(
            &other_authority,
            RoomOperationId::new_v4(),
            other_target,
        ),
    )
    .await;
    if result.is_ok() {
        assert_eq!(server.get_client_room(&other_target).await, None);
        assert_eq!(
            server.get_client_room(&target).await,
            server.get_client_room(&authority).await
        );
        assert_eq!(
            server.active_session_plan(&room_id).unwrap().transport,
            crate::protocol::Transport::WebRtc
        );
        assert_live_moderation_signal(&server, other_authority, &mut other[2], generation).await;
        assert_eq!(other[1].close.requested_reason(), Some(CloseReason::Kicked));
    }
    drop(held);
    timeout(Duration::from_secs(1), blocked)
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.is_ok(),
        "unrelated live mesh must finish its own kick"
    );
    assert_moderation_players_released(&server);
}

/// A canceled observer still receives the storage refusal and releases both code locks.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn canceled_room_code_change_reports_storage_failure_and_releases_locks() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut receiver) =
        register_client(&server, "127.0.0.1:49501".parse().unwrap()).await;
    join_seated_player(&server, &authority, &mut receiver, "FAILC1", "host").await;
    drain_receiver(&mut receiver);
    let room_id = server.get_client_room(&authority).await.unwrap();
    server.script_room_codes_for_test(["FAILC2"]);
    let database = server
        .database
        .as_any()
        .downcast_ref::<crate::database::InMemoryDatabase>()
        .unwrap();
    database.pause_next_update_room_code_for_test();
    let operation_id = RoomOperationId::new_v4();
    let caller = tokio::spawn({
        let server = Arc::clone(&server);
        async move {
            server
                .handle_regenerate_room_code_operation(&authority, operation_id)
                .await
        }
    });
    timeout(
        Duration::from_secs(1),
        database.wait_for_paused_update_room_code_for_test(),
    )
    .await
    .unwrap();
    server.database.delete_room(&room_id).await.unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    database.release_paused_update_room_code_for_test();
    let response = recv_until(&mut receiver, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    assert!(
        matches!(response.as_ref(), ServerMessage::RoomOperationResult { operation_id: actual, .. } if *actual == operation_id)
    );
    assert_eq!(
        operation_failed_code(response.as_ref()),
        ErrorCode::StorageError
    );
    for code in ["FAILC1", "FAILC2"] {
        assert!(!server
            .distributed_lock
            .is_locked(&format!("room_join:moderation-game:{code}"))
            .await
            .unwrap());
        assert!(server
            .database
            .get_room("moderation-game", code)
            .await
            .unwrap()
            .is_none());
    }
}
