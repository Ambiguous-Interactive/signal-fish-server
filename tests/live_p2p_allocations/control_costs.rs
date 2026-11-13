//! Complete room-event and repeated Direct endpoint publication costs.
use super::*;
use signal_fish_server::coordination::{MessageCoordinator, RoomEventJob};
use signal_fish_server::server::InMemoryMessageCoordinator;

/// #636: measure complete authority refusals without room-publication work.
pub(crate) fn authority_denial_avoids_room_publication() {
    use signal_fish_server::coordination::{
        ClientDeliveryHandle, ConnectionCloseSignal, InMemoryRoomOperationCoordinator,
        RoomOperationCoordinatorTrait,
    };
    use signal_fish_server::database::{
        AuthorityDenial, AuthorityOutcome, GameDatabase, InMemoryDatabase,
    };
    use std::sync::Arc;
    for supported in [false, true] {
        allocation_harness::block_on(async {
            allocation_harness::warm_current_thread_runtime().await;
            let actor = Uuid::from_u128(1);
            let database = Arc::new(InMemoryDatabase::new());
            let room = database
                .create_room(
                    "denial-cost".into(),
                    None,
                    2,
                    supported,
                    actor,
                    "relay".into(),
                    "test".into(),
                    None,
                )
                .await
                .unwrap();
            let messages = Arc::new(InMemoryMessageCoordinator::new());
            let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
            messages
                .register_local_client(
                    actor,
                    Some(room.id),
                    ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
                )
                .await
                .unwrap();
            let coordinator =
                InMemoryRoomOperationCoordinator::new(messages, database.clone(), None);
            let denial = if supported {
                AuthorityDenial::AlreadyHeld
            } else {
                AuthorityDenial::NotSupported
            };
            for _ in 0..8 {
                assert_eq!(
                    coordinator
                        .handle_authority_request(&room.id, &actor, true)
                        .await
                        .unwrap(),
                    AuthorityOutcome::Denied(denial)
                );
                let message = receiver.try_recv().unwrap();
                assert!(
                    matches!(message.as_ref(), ServerMessage::AuthorityResponse { granted: false, error_code: Some(code), .. } if *code == denial.error_code())
                );
            }
            let before = database.get_room_by_id(&room.id).await.unwrap().unwrap();
            let region = stats_alloc::Region::new(ALLOCATOR);
            for _ in 0..128 {
                assert_eq!(
                    coordinator
                        .handle_authority_request(&room.id, &actor, true)
                        .await
                        .unwrap(),
                    AuthorityOutcome::Denied(denial)
                );
                let message = receiver.try_recv().unwrap();
                assert!(
                    matches!(message.as_ref(), ServerMessage::AuthorityResponse { granted: false, reason: Some(reason), error_code: Some(code) } if reason == denial.reason() && *code == denial.error_code())
                );
            }
            let stats = region.change();
            println!("AUTHORITY_DENIAL_COST supported={supported} requests=128 stats={stats:?}");
            assert!(
                stats.allocations <= 128 * 12 + 8,
                "authority refusals allocated room-publication work: {stats:?}"
            );
            assert!(
                stats.bytes_allocated <= 128 * 1900 + 1024,
                "authority refusals retained room-publication work: {stats:?}"
            );
            let after = database.get_room_by_id(&room.id).await.unwrap().unwrap();
            assert_eq!(after.authority_player, before.authority_player);
            assert_eq!(after.last_activity, before.last_activity);
            let pending = receiver.try_recv();
            assert!(matches!(
                pending,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ));
        });
    }
}

pub(crate) fn room_event_admission_cost() {
    allocation_harness::block_on(async {
        allocation_harness::warm_current_thread_runtime().await;
        let coordinator = InMemoryMessageCoordinator::new();
        let room = Uuid::from_u128(733);
        let native_lock = std::sync::Mutex::new(());
        let lock_region = stats_alloc::Region::new(ALLOCATOR);
        drop(native_lock.lock().unwrap());
        let native_lock_initialization = lock_region.change();
        println!("ROOM_EVENT_NATIVE_LOCK_INITIALIZATION {native_lock_initialization:?}");
        // Exclude lane registration and lease creation from both samples.
        let lease = coordinator.lock_room_event_mutation(&room).await;
        for attempt in 0..2 {
            let job: RoomEventJob = Box::new(|| Box::pin(async { Ok(true) }));
            let region = stats_alloc::Region::new(ALLOCATOR);
            let completion = coordinator.enqueue_room_event(lease.clone(), job);
            let admitted = region.change();
            assert!(completion.await.unwrap());
            let complete = region.change();
            println!(
                "ROOM_EVENT_COST attempt={attempt} admitted={admitted:?} complete={complete:?}"
            );
            let lock_allocations = if attempt == 0 {
                native_lock_initialization.allocations
            } else {
                0
            };
            assert!(
                admitted.allocations <= 3 + lock_allocations,
                "room event admission allocated queue storage or a wrapper: {admitted:?}"
            );
            assert!(
                complete.allocations <= 5 + lock_allocations,
                "room event completion retained a wrapper: {complete:?}"
            );
            tokio::task::yield_now().await;
        }
        for count in [2, 16] {
            let next = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let jobs: Vec<RoomEventJob> = (0..count)
                .map(|index| {
                    let next = next.clone();
                    let job: RoomEventJob = Box::new(move || {
                        Box::pin(async move {
                            assert_eq!(
                                next.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                                index
                            );
                            Ok(true)
                        })
                    });
                    job
                })
                .collect();
            let mut completions = Vec::with_capacity(count);
            let region = stats_alloc::Region::new(ALLOCATOR);
            for job in jobs {
                completions.push(coordinator.enqueue_room_event(lease.clone(), job));
            }
            let admitted = region.change();
            assert_eq!(next.load(std::sync::atomic::Ordering::Relaxed), 0);
            for completion in completions {
                assert!(completion.await.unwrap());
            }
            let complete = region.change();
            println!("ROOM_EVENT_BURST count={count} admitted={admitted:?} complete={complete:?}");
            assert_eq!(next.load(std::sync::atomic::Ordering::Relaxed), count);
            tokio::task::yield_now().await;
        }
    });
}

pub(crate) fn direct_repeat_cost_does_not_copy_other_members() {
    let mut profiles = Vec::new();
    for count in [2_u128, 16, 255] {
        allocation_harness::block_on(async {
            allocation_harness::warm_current_thread_runtime().await;
            let server = EnhancedGameServer::new(
                ServerConfig {
                    max_connections_per_ip: 512,
                    ..Default::default()
                },
                ProtocolConfig {
                    max_players_limit: 255,
                    ..Default::default()
                },
                RelayTypeConfig::default(),
                SessionConfig {
                    default_topology: Topology::Host,
                    enable_direct: true,
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
            let host = Uuid::from_u128(1);
            let mut receivers = Vec::new();
            for index in 1..=count {
                let player = Uuid::from_u128(index);
                let (sender, receiver) = tokio::sync::mpsc::channel(1024);
                receivers.push(receiver);
                server.connect_client(player, sender).await;
                signal_fish_server::server::allocation_benchmark::negotiate_direct_for_measurement(
                    &server, &player,
                );
                server
                    .handle_join_room(
                        &player,
                        "direct-repeat".into(),
                        Some("REPEAT".into()),
                        format!("member-{index:03}-{}", "x".repeat(16)),
                        Some(255),
                        Some(true),
                        None,
                        None,
                        None,
                    )
                    .await;
                server.handle_player_ready(&player).await;
            }
            let endpoint = ConnectionInfo::Direct {
                host: "192.0.2.1".into(),
                port: 7777,
            };
            server
                .handle_provide_connection_info(&host, endpoint.clone())
                .await;
            server.handle_start_game(&host).await;
            let mut generation = None;
            for (index, receiver) in receivers.iter_mut().enumerate() {
                let mut found_plan = false;
                loop {
                    match receiver.try_recv() {
                        Ok(message) => match message.as_ref() {
                            ServerMessage::SessionPlan(plan) => {
                                assert_eq!(plan.topology, Topology::Host);
                                assert_eq!(plan.transport, Transport::Direct);
                                assert_eq!(plan.host, Some(host));
                                assert_eq!(plan.direct_endpoint.as_ref().unwrap().port, 7777);
                                if index == 0 {
                                    assert_eq!(
                                        plan.peers.len(),
                                        usize::try_from(count - 1).unwrap()
                                    );
                                    assert!(plan
                                        .peers
                                        .iter()
                                        .all(|peer| !peer.initiate && peer.player_id != host));
                                } else {
                                    assert_eq!(plan.peers.len(), 1);
                                    assert_eq!(plan.peers[0].player_id, host);
                                    assert!(plan.peers[0].initiate);
                                }
                                if let Some(expected) = generation {
                                    assert_eq!(plan.generation, expected);
                                }
                                generation = Some(plan.generation);
                                found_plan = true;
                            }
                            ServerMessage::RoomJoined(_)
                            | ServerMessage::PlayerJoined { .. }
                            | ServerMessage::LobbyStateChanged { .. }
                            | ServerMessage::AuthorityChanged { .. }
                            | ServerMessage::GameStarting { .. } => {}
                            other => panic!("unexpected setup message: {other:?}"),
                        },
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                        Err(error) => panic!("setup receiver closed: {error}"),
                    }
                }
                assert!(found_plan);
            }
            let room = server.get_client_room(&host).await.unwrap();
            let endpoint = endpoint.clone();
            let region = stats_alloc::Region::new(ALLOCATOR);
            server.handle_provide_connection_info(&host, endpoint).await;
            let stats = region.change();
            println!("DIRECT_REPEAT_COST members={count} stats={stats:?}");
            profiles.push(stats);
            for receiver in &mut receivers {
                assert_membership_queue_empty(receiver);
            }
            assert_eq!(
                server
                    .database()
                    .get_room_by_id(&room)
                    .await
                    .unwrap()
                    .unwrap()
                    .players[&host]
                    .connection_info,
                Some(ConnectionInfo::Direct {
                    host: "192.0.2.1".into(),
                    port: 7777
                })
            );
        });
    }
    for profile in &profiles {
        assert!(
            profile.allocations <= profiles[0].allocations + 2
                && profile.bytes_allocated <= profiles[0].bytes_allocated + 256,
            "an unchanged endpoint copied other members: {profiles:?}"
        );
    }
}

/// #1466: replacing a socket in the same room must reuse its membership storage.
pub(crate) fn same_room_registration_keeps_membership_storage() {
    use signal_fish_server::coordination::{ClientDeliveryHandle, ConnectionCloseSignal};
    allocation_harness::block_on(async {
        allocation_harness::warm_current_thread_runtime().await;
        // Each transient player gate initializes a native semaphore mutex.
        // macOS allocates its backing storage on first use; measure that cost
        // separately, as room_event_admission_cost does for its fresh lane.
        let native_lock = std::sync::Mutex::new(());
        let lock_region = stats_alloc::Region::new(ALLOCATOR);
        drop(native_lock.lock().unwrap());
        let native_lock_initialization = lock_region.change();
        println!("SAME_ROOM_NATIVE_LOCK_INITIALIZATION {native_lock_initialization:?}");
        let unused_notification = tokio::sync::Notify::new();
        for attempt in 0..2 {
            let region = stats_alloc::Region::new(ALLOCATOR);
            unused_notification.notify_waiters();
            let notification = region.change();
            println!("SAME_ROOM_UNUSED_NOTIFICATION attempt={attempt} stats={notification:?}");
            assert_eq!(
                notification.allocations,
                if attempt == 0 {
                    native_lock_initialization.allocations
                } else {
                    0
                }
            );
            assert_eq!(
                notification.bytes_allocated,
                if attempt == 0 {
                    native_lock_initialization.bytes_allocated
                } else {
                    0
                }
            );
        }
        let coordinator = InMemoryMessageCoordinator::new();
        let room = Uuid::from_u128(1);
        let player = Uuid::from_u128(2);
        let (sender, _receiver) = tokio::sync::mpsc::channel(4);
        let delivery = ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached());
        coordinator
            .register_local_client(player, Some(room), delivery.clone())
            .await
            .unwrap();
        for attempt in 0..2 {
            let region = stats_alloc::Region::new(ALLOCATOR);
            coordinator
                .register_local_client(player, Some(room), delivery.clone())
                .await
                .unwrap();
            let stats = region.change();
            println!("SAME_ROOM_REGISTRATION attempt={attempt} stats={stats:?}");
            assert_eq!(
                coordinator.routed_player_ids(&room).await.unwrap(),
                Some(vec![player])
            );
            assert!(
                stats.allocations <= 3 + native_lock_initialization.allocations,
                "same-room replacement rebuilt membership storage: {stats:?}"
            );
        }
    });
}

/// Refused room settings do not copy stored peer connection metadata.
pub(crate) fn room_setting_refusal_skips_player_metadata() {
    use signal_fish_server::protocol::{
        ClientMessage, ErrorCode, PlayerInfo, RoomOperationId, RoomOperationRequest,
        RoomOperationResult,
    };
    use signal_fish_server::server::allocation_benchmark::enable_room_operations_for_measurement;
    for unban in [false, true] {
        for count in [2_u128, 16, 64] {
            let mut profiles = Vec::new();
            for bytes in [0, 1024, 64 * 1024] {
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
                    let authority = Uuid::from_u128(1);
                    let requester = Uuid::from_u128(2);
                    let room = server
                        .database()
                        .create_room(
                            "setting-cost".into(),
                            None,
                            255,
                            true,
                            authority,
                            "relay".into(),
                            "test".into(),
                            None,
                        )
                        .await
                        .unwrap();
                    let metadata = serde_json::json!({"nested":["x".repeat(bytes)]});
                    for index in 2..=count {
                        assert!(server
                            .database()
                            .add_player_to_room(
                                &room.id,
                                PlayerInfo {
                                    id: Uuid::from_u128(index),
                                    name: format!("player-{index}"),
                                    connection_info: Some(ConnectionInfo::Custom {
                                        data: metadata.clone()
                                    }),
                                    is_authority: false,
                                    is_ready: false,
                                    connected_at: None,
                                    epoch: None,
                                    seq: None,
                                    region_id: "test".into(),
                                }
                            )
                            .await
                            .unwrap());
                    }
                    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
                    server.connect_client(requester, sender).await;
                    negotiate_webrtc_for_measurement(&server, &requester);
                    enable_room_operations_for_measurement(&server, &requester);
                    server.assign_client_to_room(&requester, room.id).await;
                    for measured in [false, true] {
                        let operation_id = RoomOperationId::from_u128(999);
                        let operation = if unban {
                            RoomOperationRequest::UnbanPlayer {
                                player_id: authority,
                            }
                        } else {
                            RoomOperationRequest::SetRoomAccess { password: None }
                        };
                        let message = ClientMessage::RoomOperation {
                            operation_id,
                            operation: Box::new(operation),
                        };
                        let region = stats_alloc::Region::new(ALLOCATOR);
                        server.handle_client_message(&requester, message).await;
                        let stats = region.change();
                        let response = receiver.try_recv().unwrap();
                        assert!(
                            matches!(response.as_ref(), ServerMessage::RoomOperationResult {operation_id: actual, result}
                            if *actual == operation_id && matches!(result.as_ref(), RoomOperationResult::OperationFailed {error_code: Some(ErrorCode::NotRoomAuthority), ..}))
                        );
                        assert!(matches!(
                            receiver.try_recv(),
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                        ));
                        if measured {
                            println!("ROOM_SETTING_REFUSAL unban={unban} members={count} bytes={bytes} stats={stats:?}");
                            profiles.push(stats);
                        }
                    }
                    let stored = server
                        .database()
                        .get_room_by_id(&room.id)
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(stored.authority_player, Some(authority));
                    assert!(stored.password.is_none());
                    for player in stored
                        .players
                        .values()
                        .filter(|player| player.id != authority)
                    {
                        assert!(
                            matches!(&player.connection_info, Some(ConnectionInfo::Custom {data}) if *data == metadata)
                        );
                    }
                });
            }
            for profile in &profiles {
                assert!(profile.bytes_allocated <= profiles[0].bytes_allocated + 256,
                    "room-setting refusal copied peer metadata: unban={unban} count={count} profiles={profiles:?}");
            }
        }
    }
}
