//! Healthy targeted sends borrow routing handles and retain exact queue behavior.
use super::*;
use signal_fish_server::coordination::{
    allocation_benchmark as outbound_queue, ClientDeliveryHandle, ConnectionCloseSignal,
    MessageCoordinator,
};
use signal_fish_server::server::InMemoryMessageCoordinator;
use std::sync::Arc;

/// #1455: a healthy scalar reply needs only its async-trait future.
pub(crate) fn targeted_reply_avoids_recipient_storage() {
    allocation_harness::block_on(async {
        allocation_harness::warm_current_thread_runtime().await;
        let metrics = Arc::new(signal_fish_server::metrics::ServerMetrics::new());
        let coordinator = InMemoryMessageCoordinator::new();
        let player = Uuid::from_u128(1);
        let (sender, mut receiver) = outbound_queue::channel_with_metrics(4, 4, metrics);
        coordinator
            .register_local_client(
                player,
                None,
                ClientDeliveryHandle::classified_for_allocation_benchmark(
                    sender.clone(),
                    0,
                    ConnectionCloseSignal::detached(),
                ),
            )
            .await
            .unwrap();
        let message = Arc::new(ServerMessage::Pong);
        for measured in [false, true] {
            let clones = sender.clone_operations_for_allocation_benchmark();
            let region = stats_alloc::Region::new(ALLOCATOR);
            coordinator
                .send_to_player(&player, message.clone())
                .await
                .unwrap();
            let stats = region.change();
            let copies = sender.clone_operations_for_allocation_benchmark() - clones;
            println!(
                "TARGETED_REPLY_COST measured={measured} stats={stats:?} sender_clones={copies}"
            );
            let queued = receiver.recv().await.unwrap().unwrap();
            let outbound_queue::OutboundPayload::Message(received) = &queued.payload else {
                panic!("expected control frame: {queued:?}");
            };
            assert!(Arc::ptr_eq(received, &message));
            drop(queued);
            if measured {
                assert!(
                    stats.allocations <= 1,
                    "healthy reply allocated recipient storage: {stats:?}"
                );
                assert_eq!(copies, 0, "healthy reply retained a sender");
            }
        }
        for authority in [Some(player), Some(Uuid::from_u128(99)), None] {
            let expected = authority == Some(player);
            coordinator
                .send_to_player(
                    &player,
                    Arc::new(ServerMessage::AuthorityChanged {
                        authority_player: authority,
                        you_are_authority: !expected,
                    }),
                )
                .await
                .unwrap();
            let queued = receiver.try_recv().unwrap();
            assert!(
                matches!(queued.payload, outbound_queue::OutboundPayload::Message(message)
                if matches!(message.as_ref(), ServerMessage::AuthorityChanged {
                    authority_player, you_are_authority
                } if *authority_player == authority && *you_are_authority == expected))
            );
        }
    });
}

/// #1456: a healthy Signal target does not need an owned routing handle.
pub(crate) fn targeted_room_delivery_borrows_healthy_sender() {
    allocation_harness::block_on(async {
        allocation_harness::warm_current_thread_runtime().await;
        let metrics = Arc::new(signal_fish_server::metrics::ServerMetrics::new());
        let coordinator = InMemoryMessageCoordinator::new();
        let player = Uuid::from_u128(1);
        let room = Uuid::from_u128(2);
        let other_room = Uuid::from_u128(3);
        let (sender, mut receiver) = outbound_queue::channel_with_metrics(4, 4, metrics);
        sender.set_protocol_version(3);
        sender
            .try_enqueue_transition(
                Arc::new(ServerMessage::RoomJoined(Box::new(
                    signal_fish_server::protocol::RoomJoinedPayload {
                        room_id: room,
                        room_code: "TARGET".into(),
                        player_id: player,
                        game_name: "targeted-cost".into(),
                        max_players: 2,
                        supports_authority: true,
                        current_players: vec![],
                        is_authority: true,
                        lobby_state: signal_fish_server::protocol::LobbyState::Lobby,
                        ready_players: vec![],
                        relay_type: "matchbox".into(),
                        current_spectators: vec![],
                        ice_servers: vec![],
                        reconnection_token: None,
                    },
                ))),
                1,
            )
            .unwrap();
        let baseline = receiver.try_recv().unwrap();
        assert!(
            matches!(baseline.payload, outbound_queue::OutboundPayload::Message(message)
            if matches!(message.as_ref(), ServerMessage::RoomJoined(payload)
                if payload.room_id == room && payload.player_id == player))
        );
        coordinator
            .register_local_client(
                player,
                Some(room),
                ClientDeliveryHandle::classified_for_allocation_benchmark(
                    sender.clone(),
                    1,
                    ConnectionCloseSignal::detached(),
                ),
            )
            .await
            .unwrap();
        let message = Arc::new(ServerMessage::Signal {
            from: Uuid::from_u128(4),
            generation: Uuid::from_u128(5),
            signal: serde_json::json!({"candidate": "candidate:exact"}),
        });
        for measured in [false, true] {
            let clones = sender.clone_operations_for_allocation_benchmark();
            let region = stats_alloc::Region::new(ALLOCATOR);
            assert!(coordinator
                .send_to_player_in_room(&player, &room, message.clone())
                .await
                .unwrap());
            let stats = region.change();
            let copies = sender.clone_operations_for_allocation_benchmark() - clones;
            println!(
                "TARGETED_ROOM_COST measured={measured} stats={stats:?} sender_clones={copies}"
            );
            let queued = receiver.recv().await.unwrap().unwrap();
            let outbound_queue::OutboundPayload::Message(received) = &queued.payload else {
                panic!("expected control frame: {queued:?}");
            };
            assert!(Arc::ptr_eq(received, &message));
            drop(queued);
            if measured {
                assert_eq!(copies, 0, "healthy room-targeted signal retained a sender");
            }
        }
        assert!(!coordinator
            .send_to_player_in_room(&player, &other_room, message.clone())
            .await
            .unwrap());
        assert!(!coordinator
            .send_to_player_in_room(&Uuid::from_u128(9), &room, message)
            .await
            .unwrap());
        drop(receiver);
        assert!(!coordinator
            .send_to_player_in_room(&player, &room, Arc::new(ServerMessage::Pong))
            .await
            .unwrap());
    });
}

/// #636: membership events serialize only the recipient's visible player fields.
pub(crate) fn player_events_do_not_copy_hidden_metadata() {
    use signal_fish_server::protocol::PlayerInfo;
    use signal_fish_server::websocket::allocation_benchmark::materialize_player_event;
    let mut profiles = Vec::new();
    for bytes in [0, 1024, 64 * 1024] {
        let player = PlayerInfo {
            id: Uuid::from_u128(1),
            name: "Player 🐟\n\"".into(),
            is_authority: true,
            is_ready: false,
            connected_at: Some(chrono::DateTime::UNIX_EPOCH),
            connection_info: Some(ConnectionInfo::Custom {
                data: serde_json::json!({"nested": ["x".repeat(bytes)]}),
            }),
            epoch: Some(7),
            seq: Some(19),
            region_id: "region".into(),
        };
        for reconnected in [false, true] {
            let message = if reconnected {
                ServerMessage::PlayerReconnected {
                    player_id: player.id,
                    epoch: Some(7),
                    player: Some(Box::new(player.clone())),
                }
            } else {
                ServerMessage::PlayerJoined {
                    player: player.clone(),
                }
            };
            for v3 in [false, true] {
                let mut projected_player = player.clone();
                if v3 {
                    projected_player.connected_at = None;
                    projected_player.connection_info = None;
                } else {
                    projected_player.epoch = None;
                    projected_player.seq = None;
                }
                let expected = if reconnected {
                    ServerMessage::PlayerReconnected {
                        player_id: player.id,
                        epoch: v3.then_some(7),
                        player: Some(Box::new(projected_player)),
                    }
                } else {
                    ServerMessage::PlayerJoined {
                        player: projected_player,
                    }
                };
                let expected = serde_json::to_string(&expected).unwrap();
                let region = stats_alloc::Region::new(ALLOCATOR);
                let frame = materialize_player_event(&message, v3, 1024 * 1024).unwrap();
                let stats = region.change();
                println!("PLAYER_EVENT_COST bytes={bytes} reconnect={reconnected} v3={v3} stats={stats:?}");
                assert_eq!(frame.to_text().unwrap(), expected);
                if v3 {
                    profiles.push((bytes, reconnected, stats));
                }
                assert!(materialize_player_event(&message, v3, expected.len()).is_ok());
                assert!(materialize_player_event(&message, v3, expected.len() - 1).is_err());
            }
        }
    }
    for (bytes, reconnected, stats) in &profiles {
        assert!(
            stats.allocations <= 2,
            "player event copied hidden fields: bytes={bytes} reconnect={reconnected}: {stats:?}"
        );
        assert!(
            stats.bytes_allocated <= 512,
            "player event copied hidden metadata: bytes={bytes} reconnect={reconnected}: {stats:?}"
        );
    }
}
