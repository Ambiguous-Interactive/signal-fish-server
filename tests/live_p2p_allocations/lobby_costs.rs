//! Bound lobby work independently of unrelated stored text and unready peers.
use super::*;
use signal_fish_server::coordination::{
    ClientDeliveryHandle, ConnectionCloseSignal, InMemoryRoomOperationCoordinator,
    MessageCoordinator, RoomOperationCoordinatorTrait,
};
use signal_fish_server::database::{GameDatabase, InMemoryDatabase};
use signal_fish_server::protocol::{ErrorCode, PlayerInfo};
use signal_fish_server::server::InMemoryMessageCoordinator;
use std::sync::Arc;

async fn server() -> Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
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
    .unwrap()
}

pub(crate) fn unready_start_skips_player_metadata() {
    let mut profiles = Vec::new();
    for bytes in [0, 1024, 65536] {
        allocation_harness::block_on(async {
            allocation_harness::warm_current_thread_runtime().await;
            let server = server().await;
            let mut receivers = Vec::new();
            for index in 1..=2 {
                let id = Uuid::from_u128(index);
                let (sender, receiver) = tokio::sync::mpsc::channel(32);
                receivers.push(receiver);
                server.connect_client(id, sender).await;
                negotiate_webrtc_for_measurement(&server, &id);
                server
                    .handle_join_room(
                        &id,
                        "unready".into(),
                        Some("UNRDY1".into()),
                        format!("Player{index}"),
                        Some(4),
                        Some(false),
                        None,
                        None,
                        None,
                    )
                    .await;
            }
            let actor = Uuid::from_u128(1);
            let room = server.get_client_room(&actor).await.unwrap();
            for index in 1..=2 {
                assert!(server
                    .database()
                    .update_player_connection_info(
                        &room,
                        &Uuid::from_u128(index),
                        ConnectionInfo::Custom {
                            data: serde_json::json!({"nested":["x".repeat(bytes)]})
                        }
                    )
                    .await
                    .unwrap());
            }
            for receiver in &mut receivers {
                drain_membership_setup(receiver, false);
            }
            for ready_actor in [false, true] {
                if ready_actor {
                    server.handle_player_ready(&actor).await;
                    for receiver in &mut receivers {
                        let message = receiver.try_recv().unwrap();
                        assert!(
                            matches!(message.as_ref(), ServerMessage::LobbyStateChanged {
                            lobby_state: signal_fish_server::protocol::LobbyState::Lobby,
                            ready_players, all_ready: false,
                        } if ready_players == &[actor])
                        );
                        assert_membership_queue_empty(receiver);
                    }
                }
                let region = stats_alloc::Region::new(ALLOCATOR);
                server.handle_start_game(&actor).await;
                let stats = region.change();
                println!("UNREADY_START bytes={bytes} ready_actor={ready_actor} stats={stats:?}");
                profiles.push((ready_actor, stats.bytes_allocated));
                let reply = receivers[0].try_recv().unwrap();
                assert!(
                    matches!(
                        reply.as_ref(),
                        ServerMessage::Error {
                            error_code: Some(ErrorCode::GameStartNotReady),
                            ..
                        }
                    ),
                    "{reply:?}"
                );
                for receiver in &mut receivers {
                    assert_membership_queue_empty(receiver);
                }
                assert_ne!(
                    server.database().get_room_lobby_state(&room).await.unwrap(),
                    Some(signal_fish_server::protocol::LobbyState::Finalized)
                );
            }
        });
    }
    for chunk in profiles.chunks_exact(2) {
        assert_eq!(
            chunk,
            &profiles[..2],
            "rejected start copied player metadata: {profiles:?}"
        );
    }
}

pub(crate) fn ready_snapshot_scales_with_ready_members() {
    let mut profiles = Vec::new();
    for members in [2_u128, 16, 255] {
        allocation_harness::block_on(async {
            tokio::task::yield_now().await;
            let database = Arc::new(InMemoryDatabase::new());
            let actor = Uuid::from_u128(1);
            let room = database
                .create_room(
                    "ready".into(),
                    None,
                    255,
                    false,
                    actor,
                    "relay".into(),
                    "test".into(),
                    None,
                )
                .await
                .unwrap();
            for index in 2..=members {
                assert!(database
                    .add_player_to_room(
                        &room.id,
                        PlayerInfo {
                            id: Uuid::from_u128(index),
                            name: format!("Player{index}"),
                            is_authority: false,
                            is_ready: false,
                            connected_at: None,
                            connection_info: None,
                            epoch: None,
                            seq: None,
                            region_id: "test".into(),
                        }
                    )
                    .await
                    .unwrap());
            }
            let messages = Arc::new(InMemoryMessageCoordinator::new());
            let mut receivers = Vec::new();
            for index in 1..=members {
                let (sender, receiver) = tokio::sync::mpsc::channel(8);
                receivers.push(receiver);
                messages
                    .register_local_client(
                        Uuid::from_u128(index),
                        Some(room.id),
                        ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
                    )
                    .await
                    .unwrap();
            }
            let coordinator =
                InMemoryRoomOperationCoordinator::new(messages, database.clone(), None);
            coordinator
                .handle_player_ready(&room.id, &actor, None)
                .await
                .unwrap();
            for receiver in &mut receivers {
                let message = receiver.try_recv().unwrap();
                assert!(
                    matches!(message.as_ref(), ServerMessage::LobbyStateChanged { ready_players, all_ready: false, .. } if ready_players == &[actor])
                );
                assert_membership_queue_empty(receiver);
            }
            assert_eq!(
                coordinator.current_ready_players(&room.id).await,
                vec![actor]
            );
            let region = stats_alloc::Region::new(ALLOCATOR);
            let ready = coordinator.current_ready_players(&room.id).await;
            let stats = region.change();
            println!("READY_FILTER members={members} stats={stats:?}");
            profiles.push(stats.bytes_allocated);
            assert_eq!(ready, vec![actor]);
            database
                .remove_player_from_room(&room.id, &actor)
                .await
                .unwrap();
            assert!(coordinator.current_ready_players(&room.id).await.is_empty());
        });
    }
    assert!(
        profiles.iter().all(|profile| *profile == profiles[0]),
        "ready snapshot copied unready membership: {profiles:?}"
    );
}

pub(crate) fn app_identity_lookup_does_not_copy_text() {
    for bytes in [0, 1024, 65536] {
        allocation_harness::block_on(async {
            let server = server().await;
            let player = Uuid::from_u128(1);
            let app = Uuid::from_u128(20);
            assert_eq!(server.client_app_id(&player), None);
            let (sender, _receiver) = tokio::sync::mpsc::channel(8);
            server.connect_client(player, sender).await;
            assert_eq!(server.client_app_id(&player), None);
            server.set_client_app_context(
                &player,
                signal_fish_server::auth::AppContext {
                    id: app,
                    name: "n".repeat(bytes),
                    organization: Some("o".repeat(bytes)),
                    max_rooms: None,
                    max_players_per_room: None,
                    rate_limit_per_minute: None,
                    max_relay_bytes: None,
                    require_connect_token: None,
                    rate_limits: signal_fish_server::auth::middleware::RateLimits {
                        per_minute: 1000,
                        per_hour: 10000,
                        per_day: 100000,
                    },
                },
            );
            let region = stats_alloc::Region::new(ALLOCATOR);
            let actual = server.client_app_id(&player);
            let stats = region.change();
            println!("APP_ID bytes={bytes} stats={stats:?}");
            assert_eq!(actual, Some(app));
            assert_eq!(
                stats.allocations, 0,
                "app identity copied unrelated text: {stats:?}"
            );
            let mut context = server.client_app_context(&player).unwrap();
            assert_eq!(context.name.len(), bytes);
            assert_eq!(context.organization.as_ref().unwrap().len(), bytes);
            context.id = Uuid::from_u128(21);
            server.set_client_app_context(&player, context);
            assert_eq!(server.client_app_id(&player), Some(Uuid::from_u128(21)));
        });
    }
}

/// Keep cold task-registry setup visible while checking the warmed measurement boundary.
pub(crate) fn runtime_registry_warmup_retains_exact_spawn_cost() {
    let mut cold = Vec::with_capacity(8);
    let mut warm = Vec::with_capacity(8);
    for (warmed, samples) in [(false, &mut cold), (true, &mut warm)] {
        allocation_harness::block_on(async {
            if warmed {
                allocation_harness::warm_current_thread_runtime().await;
            } else {
                tokio::task::yield_now().await;
            }
            for _ in 0..8 {
                let region = stats_alloc::Region::new(ALLOCATOR);
                let task = tokio::spawn(std::future::ready(()));
                let id = task.id();
                task.await.unwrap();
                let stats = region.change();
                samples.push((id, stats));
            }
        });
    }
    println!("RUNTIME_REGISTRY cold={cold:?} warm={warm:?}");
    let baseline = &warm[0].1;
    for (_, stats) in &warm {
        assert_eq!(stats.allocations, baseline.allocations);
        assert_eq!(stats.bytes_allocated, baseline.bytes_allocated);
        assert_eq!(stats.reallocations, 0);
    }
}

/// #636: bounded cache filtering removes per-room future allocations without copying rows.
pub(crate) fn room_cache_filter_batches_storage_allocations() {
    allocation_harness::block_on(async {
        allocation_harness::warm_current_thread_runtime().await;
        let database = InMemoryDatabase::new();
        let live = database
            .create_pending_room_classified(
                "cache-cost".into(),
                Some("COST01".into()),
                2,
                false,
                Uuid::from_u128(1),
                "relay".into(),
                "test".into(),
                None,
                None,
            )
            .await
            .unwrap();
        let storage: &dyn GameDatabase = std::hint::black_box(&database);
        assert!(storage.room_exists(&live.id).await.unwrap());
        for count in [2_usize, 16, 255, 256, 257, 4_096] {
            let mut candidates: Vec<_> = (0..count)
                .map(|index| {
                    if index % 2 == 0 {
                        live.id
                    } else {
                        Uuid::from_u128(2)
                    }
                })
                .collect();
            let mut individually_missing = Vec::with_capacity(count);
            let region = stats_alloc::Region::new(ALLOCATOR);
            for room in &candidates {
                if !storage.room_exists(room).await.unwrap() {
                    individually_missing.push(*room);
                }
            }
            let individual = region.change();
            let pointer = candidates.as_ptr();
            let capacity = candidates.capacity();
            let region = stats_alloc::Region::new(ALLOCATOR);
            storage
                .retain_missing_room_ids(&mut candidates)
                .await
                .unwrap();
            let batch = region.change();
            assert_eq!(candidates, individually_missing);
            assert_eq!(candidates.as_ptr(), pointer);
            assert_eq!(candidates.capacity(), capacity);
            assert!(
                batch.allocations < individual.allocations,
                "count={count}, individual={individual:?}, batch={batch:?}"
            );
            assert!(
                batch.bytes_allocated < individual.bytes_allocated,
                "count={count}, individual={individual:?}, batch={batch:?}"
            );
            assert_eq!(batch.reallocations, 0);
            println!("ROOM_CACHE count={count} individual={individual:?} batch={batch:?}");
        }
    });
}
