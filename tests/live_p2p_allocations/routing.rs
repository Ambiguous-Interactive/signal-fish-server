use super::*;
use signal_fish_server::coordination::{
    ClientDeliveryHandle, ConnectionCloseSignal, MessageCoordinator,
};
use signal_fish_server::server::InMemoryMessageCoordinator;

/// #1393: retain exact live identities without vector growth.
pub(crate) fn roster_snapshot_reserves_once() {
    for count in [0_u128, 2, 16, 64, 255] {
        allocation_harness::block_on(async {
            tokio::task::yield_now().await;
            let coordinator = InMemoryMessageCoordinator::new();
            let room = Uuid::from_u128(1000);
            let mut receivers = Vec::new();
            for index in 0..count {
                let (sender, receiver) = tokio::sync::mpsc::channel(4);
                receivers.push(receiver);
                coordinator
                    .register_local_client(
                        Uuid::from_u128(index + 1),
                        Some(room),
                        ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
                    )
                    .await
                    .unwrap();
            }
            drop(coordinator.routed_player_ids(&room).await.unwrap());
            let region = stats_alloc::Region::new(ALLOCATOR);
            let members = coordinator.routed_player_ids(&room).await.unwrap().unwrap();
            let stats = region.change();
            println!("ROUTED_ROSTER_PROFILE count={count} stats={stats:?}");
            assert_eq!(
                members,
                (1..=count).map(Uuid::from_u128).collect::<Vec<_>>()
            );
            assert_eq!(stats.reallocations, 0, "count={count}: {stats:?}");
        });
    }
}

/// #1392: relay exact ICE to one peer in a finalized Mesh session.
pub(crate) fn signaling_cost_is_independent_of_room_size() {
    let mut profiles = Vec::new();
    for count in [2_u128, 16, 64] {
        allocation_harness::block_on(async {
            tokio::task::yield_now().await;
            let server = EnhancedGameServer::new(
                ServerConfig::default(),
                ProtocolConfig::default(),
                RelayTypeConfig::default(),
                SessionConfig {
                    default_topology: Topology::Mesh,
                    enable_webrtc: true,
                    ..Default::default()
                },
                TurnConfig::default(),
                DatabaseConfig::InMemory,
                MetricsConfig::default(),
                CoordinationConfig::default(),
                TransportSecurityConfig::default(),
                vec![],
            )
            .await
            .unwrap();
            let mut receivers = Vec::new();
            for index in 0..count {
                let player = Uuid::from_u128(index + 1);
                let (sender, receiver) = tokio::sync::mpsc::channel(1024);
                receivers.push(receiver);
                server.connect_client(player, sender).await;
                negotiate_webrtc_for_measurement(&server, &player);
                server
                    .handle_join_room(
                        &player,
                        "signal-profile".into(),
                        Some("SIGN01".into()),
                        format!("Player{index}"),
                        Some(64),
                        Some(true),
                        None,
                        None,
                        None,
                    )
                    .await;
                server.handle_player_ready(&player).await;
            }
            let from = Uuid::from_u128(1);
            let room = server.get_client_room(&from).await.unwrap();
            for index in 0..count {
                assert_eq!(
                    server.get_client_room(&Uuid::from_u128(index + 1)).await,
                    Some(room)
                );
            }
            for receiver in &mut receivers {
                drain_membership_setup(receiver, false);
            }
            server.handle_start_game(&from).await;
            let mut generation = None;
            for receiver in &mut receivers {
                let starting = receiver.try_recv().unwrap();
                assert!(
                    matches!(starting.as_ref(), ServerMessage::GameStarting { peer_connections }
                    if peer_connections.len() == usize::try_from(count).unwrap())
                );
                let plan = take_plan(receiver);
                assert_eq!(plan.topology, Topology::Mesh);
                assert_eq!(plan.transport, Transport::WebRtc);
                assert_eq!(plan.peers.len(), usize::try_from(count - 1).unwrap());
                if let Some(expected) = generation {
                    assert_eq!(plan.generation, expected);
                }
                generation = Some(plan.generation);
                assert_membership_queue_empty(receiver);
            }
            let generation = generation.unwrap();
            let to = Uuid::from_u128(2);
            for measured in [false, true] {
                let signal = serde_json::json!({"candidate":"candidate:exact"});
                let region = stats_alloc::Region::new(ALLOCATOR);
                server
                    .handle_signal_in_generation(&from, to, generation, signal.clone())
                    .await;
                let stats = region.change();
                if measured {
                    println!("LIVE_SIGNAL_PROFILE count={count} stats={stats:?}");
                    profiles.push(stats);
                }
                for (index, receiver) in receivers.iter_mut().enumerate() {
                    if index == 1 {
                        let message = receiver.try_recv().unwrap();
                        assert!(
                            matches!(message.as_ref(), ServerMessage::Signal { from: actual, generation: actual_generation, signal: actual_signal }
                            if *actual == from && *actual_generation == generation && *actual_signal == signal)
                        );
                    }
                    assert_membership_queue_empty(receiver);
                }
            }
        });
    }
    for stats in &profiles {
        assert!(
            stats.bytes_allocated <= profiles[0].bytes_allocated + 256,
            "targeted signaling allocates for unrelated room peers: {profiles:?}"
        );
    }
}

/// #1394: unrelated game policies must not be copied at P2P start.
pub(crate) fn game_start_shares_configuration() {
    let mut profiles = Vec::new();
    for mappings in [0, 16, 256] {
        allocation_harness::block_on(async {
            tokio::task::yield_now().await;
            let config = SessionConfig {
                default_topology: Topology::Relay,
                enable_webrtc: true,
                game_topology_mappings: (0..mappings)
                    .map(|index| (format!("unrelated-game-{index}"), Topology::Relay))
                    .chain([("start-profile".into(), Topology::Mesh)])
                    .collect(),
                ..Default::default()
            };
            let server = EnhancedGameServer::new(
                ServerConfig::default(),
                ProtocolConfig::default(),
                RelayTypeConfig::default(),
                config,
                TurnConfig::default(),
                DatabaseConfig::InMemory,
                MetricsConfig::default(),
                CoordinationConfig::default(),
                TransportSecurityConfig::default(),
                vec![],
            )
            .await
            .unwrap();
            let mut receivers = Vec::new();
            for (index, name) in ["Host", "Guest"].into_iter().enumerate() {
                let player = Uuid::from_u128(u128::try_from(index).unwrap() + 1);
                let (sender, receiver) = tokio::sync::mpsc::channel(32);
                receivers.push(receiver);
                server.connect_client(player, sender).await;
                negotiate_webrtc_for_measurement(&server, &player);
                server
                    .handle_join_room(
                        &player,
                        "start-profile".into(),
                        Some("START1".into()),
                        name.into(),
                        Some(2),
                        Some(true),
                        None,
                        None,
                        None,
                    )
                    .await;
                server.handle_player_ready(&player).await;
            }
            for receiver in &mut receivers {
                drain_membership_setup(receiver, false);
            }
            let region = stats_alloc::Region::new(ALLOCATOR);
            server.handle_start_game(&Uuid::from_u128(1)).await;
            let stats = region.change();
            println!("START_CONFIG_PROFILE mappings={mappings} stats={stats:?}");
            profiles.push(stats);
            let mut generation = None;
            for (index, receiver) in receivers.iter_mut().enumerate() {
                let starting = receiver.try_recv().unwrap();
                assert!(
                    matches!(starting.as_ref(), ServerMessage::GameStarting { peer_connections } if peer_connections.len() == 2)
                );
                let plan = take_plan(receiver);
                assert_eq!(plan.topology, Topology::Mesh);
                assert_eq!(plan.transport, Transport::WebRtc);
                assert_eq!(plan.host, None);
                assert_eq!(plan.fallback, Transport::Relay);
                assert_eq!(plan.peers.len(), 1);
                assert_eq!(
                    plan.peers[0].player_id,
                    Uuid::from_u128(if index == 0 { 2 } else { 1 })
                );
                assert_eq!(plan.peers[0].initiate, index == 0);
                if let Some(expected) = generation {
                    assert_eq!(plan.generation, expected);
                }
                generation = Some(plan.generation);
                assert_membership_queue_empty(receiver);
            }
        });
    }
    for stats in &profiles {
        assert!(
            stats.bytes_allocated <= profiles[0].bytes_allocated + 256,
            "game start copies unrelated game configuration: {profiles:?}"
        );
    }
}

/// Measure full report acceptance and exact peer delivery after warm-up.
pub(crate) fn transport_report_delivery_cost() {
    for count in [2_u128, 16, 64] {
        allocation_harness::block_on(async {
            allocation_harness::warm_current_thread_runtime().await;
            let server = EnhancedGameServer::new(
                ServerConfig::default(),
                ProtocolConfig::default(),
                RelayTypeConfig::default(),
                SessionConfig::default(),
                TurnConfig::default(),
                DatabaseConfig::InMemory,
                MetricsConfig::default(),
                CoordinationConfig::default(),
                TransportSecurityConfig::default(),
                vec![],
            )
            .await
            .unwrap();
            let mut receivers = Vec::new();
            for index in 1..=count {
                let player = Uuid::from_u128(index);
                let (sender, receiver) = tokio::sync::mpsc::channel(512);
                receivers.push(receiver);
                server.connect_client(player, sender).await;
                negotiate_webrtc_for_measurement(&server, &player);
                server
                    .handle_join_room(
                        &player,
                        "status-profile".into(),
                        Some("STAT01".into()),
                        format!("Player{index}"),
                        Some(64),
                        Some(false),
                        None,
                        None,
                        None,
                    )
                    .await;
            }
            for receiver in &mut receivers {
                drain_membership_setup(receiver, false);
            }
            let reporter = Uuid::from_u128(1);
            for connected in [false, true] {
                let region = stats_alloc::Region::new(ALLOCATOR);
                server
                    .handle_client_message(
                        &reporter,
                        signal_fish_server::protocol::ClientMessage::TransportStatus {
                            transport: Transport::WebRtc,
                            connected,
                        },
                    )
                    .await;
                let stats = region.change();
                if connected {
                    println!("TRANSPORT_REPORT_PROFILE count={count} stats={stats:?}");
                }
                if connected && count == 2 {
                    let duplicate_region = stats_alloc::Region::new(ALLOCATOR);
                    server
                        .handle_client_message(
                            &reporter,
                            signal_fish_server::protocol::ClientMessage::TransportStatus {
                                transport: Transport::WebRtc,
                                connected,
                            },
                        )
                        .await;
                    let duplicate = duplicate_region.change();
                    println!("TRANSPORT_REPORT_DUPLICATE_PROFILE stats={duplicate:?}");
                    assert!(stats.allocations <= duplicate.allocations + 9,
                        "single-recipient delivery builds fan-out collections: changed={stats:?}, duplicate={duplicate:?}");
                }
                for (index, receiver) in receivers.iter_mut().enumerate() {
                    if index != 0 {
                        let message = receiver.try_recv().unwrap();
                        assert!(
                            matches!(message.as_ref(), ServerMessage::PeerTransportStatus {
                            peer_id, transport: Transport::WebRtc, connected: actual,
                        } if *peer_id == reporter && *actual == connected)
                        );
                    }
                    assert_membership_queue_empty(receiver);
                }
            }
        });
    }
}
