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
                tokio::task::yield_now().await;
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
            stats.allocations <= 8,
            "a signal allocated exclusive room-publication work: {profiles:?}"
        );
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

/// Compare complete steady-state delivery after exercising both report states.
/// Retain first-use profiles: macOS can add one retained allocation on first use.
pub(crate) fn transport_report_delivery_cost() {
    let counts = [2_u128, 3, 4, 16, 31, 32, 64, 255];
    let mut profiles = Vec::new();
    for warm_owned_scheduler in [false, true] {
        for count in counts {
            allocation_harness::block_on(async {
                allocation_harness::warm_current_thread_runtime().await;
                let server = EnhancedGameServer::new(
                    ServerConfig::default(),
                    ProtocolConfig {
                        max_players_limit: 255,
                        ..ProtocolConfig::default()
                    },
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
                            Some(255),
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
                for sample in 0..4 {
                    let warm_region = stats_alloc::Region::new(ALLOCATOR);
                    if warm_owned_scheduler {
                        tokio::spawn(async { tokio::task::yield_now().await })
                            .await
                            .unwrap();
                    } else {
                        tokio::spawn(std::future::ready(())).await.unwrap();
                    }
                    let warm_stats = warm_region.change();
                    println!("TRANSPORT_RUNTIME_CONTROL owned_yield={warm_owned_scheduler} count={count} sample={sample} stats={warm_stats:?}");
                }
                let reporter = Uuid::from_u128(1);
                for (phase, connected) in [false, true, false, true].into_iter().enumerate() {
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
                    println!("TRANSPORT_REPORT_PHASE owned_yield={warm_owned_scheduler} count={count} phase={phase} connected={connected} stats={stats:?}");
                    if phase == 3 {
                        println!("TRANSPORT_REPORT_PROFILE count={count} stats={stats:?}");
                        profiles.push(stats);
                    }
                    if phase == 1 && count == 2 {
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
    for profiles in profiles.chunks_exact(counts.len()) {
        let per_recipient = profiles[2]
            .allocations
            .checked_sub(profiles[1].allocations)
            .expect("the generic delivery cost grows with recipients");
        for (count, profile) in counts
            .into_iter()
            .zip(profiles)
            .filter(|(count, _)| *count >= 32)
        {
            let count = usize::try_from(count).unwrap();
            let groups = (count - 1).div_ceil(16);
            let projection = profiles[2].allocations + (count - 4) * per_recipient;
            assert!(profile.allocations <= projection + 2 * groups + 4,
                "large report fan-out allocates one scheduling node per recipient: count={count} projection={projection} profiles={profiles:?}");
        }
        let generic_single_recipient = profiles[1]
            .allocations
            .checked_mul(2)
            .and_then(|twice| twice.checked_sub(profiles[2].allocations))
            .expect("full-delivery cohorts have a common fixed cost");
        assert!(
            profiles[0].allocations + 2 <= generic_single_recipient,
            "single-recipient delivery builds fan-out collections: {profiles:?}"
        );
    }
}

/// #1470: excluded IDs must not cause recipient-vector growth under routing locks.
pub(crate) fn bus_exclusions_do_not_grow_recipient_storage() {
    allocation_harness::block_on(async {
        allocation_harness::warm_current_thread_runtime().await;
        let coordinator = InMemoryMessageCoordinator::new();
        let room = Uuid::from_u128(1000);
        let mut receivers = Vec::new();
        for id in 1..=7 {
            let (sender, receiver) = tokio::sync::mpsc::channel(32);
            receivers.push(receiver);
            coordinator
                .register_local_client(
                    Uuid::from_u128(id),
                    Some(room),
                    ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
                )
                .await
                .unwrap();
        }
        let cases = [
            vec![],
            vec![Uuid::from_u128(1)],
            vec![Uuid::from_u128(1); 7],
            vec![Uuid::from_u128(999); 7],
            (1..=7).map(Uuid::from_u128).collect(),
            vec![Uuid::from_u128(1), Uuid::from_u128(1), Uuid::from_u128(999)],
        ];
        for excluded in cases {
            for measured in [false, true] {
                let message = signal_fish_server::distributed::SequencedMessage {
                    sequence_id: 1,
                    instance_id: Uuid::from_u128(2000),
                    timestamp: chrono::Utc::now(),
                    message: ServerMessage::Pong,
                    room_id: Some(room),
                    target_player: None,
                    excluded_players: excluded.clone(),
                };
                let region = stats_alloc::Region::new(ALLOCATOR);
                coordinator.handle_bus_message(message).await.unwrap();
                let stats = region.change();
                println!(
                    "BUS_EXCLUSION_COST measured={measured} excluded={excluded:?} stats={stats:?}"
                );
                for (index, receiver) in receivers.iter_mut().enumerate() {
                    let player = Uuid::from_u128(u128::try_from(index).unwrap() + 1);
                    if !excluded.contains(&player) {
                        let received = receiver
                            .try_recv()
                            .expect("non-excluded peer receives Pong");
                        assert!(matches!(received.as_ref(), ServerMessage::Pong));
                    }
                    let remaining = receiver.try_recv();
                    assert!(matches!(
                        remaining,
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                    ));
                }
                if measured {
                    assert_eq!(stats.reallocations, 0, "recipient storage grew: {stats:?}");
                }
            }
        }
    });
}
