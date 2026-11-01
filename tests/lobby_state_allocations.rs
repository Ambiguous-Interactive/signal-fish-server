//! Allocation bound for the ready-toggle room-state projection.

use signal_fish_server::coordination::{
    ClientDeliveryHandle, ConnectionCloseSignal, InMemoryRoomOperationCoordinator,
    MessageCoordinator, RoomOperationCoordinatorTrait,
};
use signal_fish_server::database::{GameDatabase, InMemoryDatabase};
use signal_fish_server::protocol::{LobbyState, ServerMessage, SpectatorInfo};
use signal_fish_server::server::InMemoryMessageCoordinator;
use std::sync::Arc;
use uuid::Uuid;

#[global_allocator]
static ALLOCATOR: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

/// A ready-state read must not copy spectator names or the room's member map.
#[tokio::test]
async fn lobby_state_projection_allocates_only_its_async_result() {
    // Exclude the current-thread scheduler's first deferred-waker allocation.
    tokio::task::yield_now().await;
    native_readiness_lock_initialization_control().await;
    for spectators in [0_u128, 16, 255] {
        let database = InMemoryDatabase::new();
        let room = database
            .create_room(
                "state-game".into(),
                Some("STATE1".into()),
                4,
                true,
                Uuid::from_u128(1),
                "relay".into(),
                "test".into(),
                None,
            )
            .await
            .unwrap();
        for index in 0..spectators {
            assert!(database
                .add_spectator_to_room(
                    &room.id,
                    SpectatorInfo {
                        id: Uuid::from_u128(index + 2),
                        name: format!("Spectator{index}"),
                        connected_at: None,
                    }
                )
                .await
                .unwrap());
        }
        for expected in [Some(LobbyState::Waiting), Some(LobbyState::Lobby), None] {
            match expected {
                Some(LobbyState::Lobby) => {
                    database.transition_room_to_lobby(&room.id).await.unwrap()
                }
                None => assert!(database.delete_room(&room.id).await.unwrap()),
                _ => {}
            }
            let baseline_region = stats_alloc::Region::new(ALLOCATOR);
            let baseline = database
                .get_room_by_id(&room.id)
                .await
                .unwrap()
                .map(|room| room.lobby_state);
            let baseline_stats = baseline_region.change();
            let region = stats_alloc::Region::new(ALLOCATOR);
            let actual = database.get_room_lobby_state(&room.id).await.unwrap();
            let stats = region.change();
            assert_eq!(actual, expected);
            assert_eq!(actual, baseline);
            println!(
                "LOBBY_STATE_PROFILE spectators={spectators} state={actual:?} full_snapshot={baseline_stats:?} projection={stats:?}"
            );
            assert_eq!(stats.allocations, 1, "spectators={spectators}: {stats:?}");
            assert_eq!(stats.reallocations, 0);
        }
    }
    untouched_ready_snapshot_allocations_do_not_grow_with_spectator_rosters().await;
    ready_toggle_allocations_do_not_grow_with_spectator_rosters().await;
    lobby_promotion_allocations_do_not_grow_with_spectator_rosters().await;
    readiness_allocations_do_not_copy_player_details().await;
}

/// Bound readiness storage independently of the stored member's name size.
async fn readiness_allocations_do_not_copy_player_details() {
    let mut profiles = Vec::new();
    for name_bytes in [1, 4096, 65536] {
        let database = Arc::new(InMemoryDatabase::new());
        let member = Uuid::from_u128(1);
        let room = database
            .create_room(
                "details".into(),
                None,
                4,
                false,
                member,
                "relay".into(),
                "test".into(),
                None,
            )
            .await
            .unwrap();
        let mut player = database
            .get_room_players(&room.id)
            .await
            .unwrap()
            .pop()
            .unwrap();
        player.name = "x".repeat(name_bytes);
        assert!(database
            .remove_player_from_room(&room.id, &member)
            .await
            .unwrap()
            .is_some());
        assert!(database.add_player_to_room(&room.id, player).await.unwrap());
        let messages = Arc::new(InMemoryMessageCoordinator::new());
        let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
        messages
            .register_local_client(
                member,
                Some(room.id),
                ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
            )
            .await
            .unwrap();
        let coordinator = InMemoryRoomOperationCoordinator::new(messages, database, None);
        coordinator
            .handle_player_ready(&room.id, &member, None)
            .await
            .unwrap();
        assert_ready_publication(&receiver.try_recv().unwrap(), member, true);
        let region = stats_alloc::Region::new(ALLOCATOR);
        let ready = coordinator.current_ready_players(&room.id).await;
        let snapshot = region.change();
        assert_eq!(ready, vec![member]);
        let region = stats_alloc::Region::new(ALLOCATOR);
        coordinator
            .handle_player_ready(&room.id, &member, None)
            .await
            .unwrap();
        let toggle = region.change();
        assert_ready_publication(&receiver.try_recv().unwrap(), member, false);
        println!(
            "READY_DETAILS_PROFILE name_bytes={name_bytes} snapshot={snapshot:?} toggle={toggle:?}"
        );
        profiles.push((snapshot.bytes_allocated, toggle.bytes_allocated));
    }
    assert!(
        profiles.iter().all(|profile| *profile == profiles[0]),
        "readiness copied player names: {profiles:?}"
    );
}

/// Keep the ceiling on the actual ready-toggle path, including its queued publication.
async fn ready_toggle_allocations_do_not_grow_with_spectator_rosters() {
    let mut baseline = None;
    let mut bounded = true;
    for spectators in [0_u128, 16, 255] {
        let database = Arc::new(InMemoryDatabase::new());
        let member = Uuid::from_u128(1);
        let room = database
            .create_room(
                "ready-game".into(),
                Some("READY1".into()),
                4,
                true,
                member,
                "relay".into(),
                "test".into(),
                None,
            )
            .await
            .unwrap();
        for index in 0..spectators {
            assert!(database
                .add_spectator_to_room(
                    &room.id,
                    SpectatorInfo {
                        id: Uuid::from_u128(index + 2),
                        name: format!("Spectator{index}"),
                        connected_at: None,
                    }
                )
                .await
                .unwrap());
        }
        let messages = Arc::new(InMemoryMessageCoordinator::new());
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        messages
            .register_local_client(
                member,
                Some(room.id),
                ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
            )
            .await
            .unwrap();
        let coordinator = InMemoryRoomOperationCoordinator::new(messages, database, None);
        for ready in [true, false] {
            coordinator
                .handle_player_ready(&room.id, &member, None)
                .await
                .unwrap();
            let message = receiver.try_recv().unwrap();
            assert_ready_publication(&message, member, ready);
        }
        let mut allocations = [(0, 0); 2];
        for (index, count) in allocations.iter_mut().enumerate() {
            let region = stats_alloc::Region::new(ALLOCATOR);
            coordinator
                .handle_player_ready(&room.id, &member, None)
                .await
                .unwrap();
            let stats = region.change();
            *count = (stats.allocations, stats.bytes_allocated);
            let message = receiver.try_recv().unwrap();
            assert_ready_publication(&message, member, index == 0);
            println!("READY_TOGGLE_PROFILE spectators={spectators} stats={stats:?}");
        }
        if let Some(expected) = baseline {
            bounded &= allocations == expected;
        } else {
            baseline = Some(allocations);
        }
    }
    assert!(
        bounded,
        "ready-toggle allocations must not grow with spectator rosters"
    );
}

/// Check the exact lobby event outside the measured allocation window.
fn assert_ready_publication(message: &ServerMessage, member: Uuid, ready: bool) {
    let ServerMessage::LobbyStateChanged {
        ready_players,
        all_ready,
        ..
    } = message
    else {
        panic!("expected lobby readiness event, got {message:?}");
    };
    assert_eq!(*all_ready, ready);
    assert_eq!(
        ready_players.as_slice(),
        if ready {
            std::slice::from_ref(&member)
        } else {
            &[]
        }
    );
}

/// Lobby promotion must not copy spectator names when it reads the room state.
async fn lobby_promotion_allocations_do_not_grow_with_spectator_rosters() {
    let mut profiles = Vec::new();
    for spectators in [0_u128, 16, 255] {
        let database = Arc::new(InMemoryDatabase::new());
        let member = Uuid::from_u128(1);
        let room = database
            .create_room(
                "promotion-game".into(),
                Some("PROMO1".into()),
                4,
                true,
                member,
                "relay".into(),
                "test".into(),
                None,
            )
            .await
            .unwrap();
        for index in 0..spectators {
            assert!(database
                .add_spectator_to_room(
                    &room.id,
                    SpectatorInfo {
                        id: Uuid::from_u128(index + 2),
                        name: format!("Spectator{index}"),
                        connected_at: None,
                    },
                )
                .await
                .unwrap());
        }
        let messages = Arc::new(InMemoryMessageCoordinator::new());
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        messages
            .register_local_client(
                member,
                Some(room.id),
                ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
            )
            .await
            .unwrap();
        let coordinator = InMemoryRoomOperationCoordinator::new(messages, database.clone(), None);
        let region = stats_alloc::Region::new(ALLOCATOR);
        assert!(coordinator
            .transition_room_to_lobby(&room.id)
            .await
            .unwrap());
        let stats = region.change();
        profiles.push((stats.allocations, stats.bytes_allocated));
        println!("LOBBY_PROMOTION_PROFILE spectators={spectators} stats={stats:?}");
        let message = receiver.try_recv().unwrap();
        assert_ready_publication(&message, member, false);
        assert!(matches!(
            message.as_ref(),
            ServerMessage::LobbyStateChanged {
                lobby_state: LobbyState::Lobby,
                ..
            }
        ));
        assert_eq!(
            database.get_room_lobby_state(&room.id).await.unwrap(),
            Some(LobbyState::Lobby)
        );
        assert!(!coordinator
            .transition_room_to_lobby(&room.id)
            .await
            .unwrap());
        let trailing = receiver.try_recv();
        assert!(matches!(
            trailing,
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(
            database.get_room_spectators(&room.id).await.unwrap().len(),
            usize::try_from(spectators).unwrap()
        );
    }
    assert!(
        profiles.iter().all(|profile| *profile == profiles[0]),
        "lobby promotion copied spectator state: {profiles:?}"
    );
}

async fn untouched_ready_snapshot_allocations_do_not_grow_with_spectator_rosters() {
    let mut profiles = Vec::new();
    for spectators in [0_u128, 16, 255] {
        let database = Arc::new(InMemoryDatabase::new());
        let room = database
            .create_room(
                "snapshot-game".into(),
                None,
                4,
                true,
                Uuid::from_u128(1),
                "relay".into(),
                "test".into(),
                None,
            )
            .await
            .unwrap();
        for index in 0..spectators {
            assert!(database
                .add_spectator_to_room(
                    &room.id,
                    SpectatorInfo {
                        id: Uuid::from_u128(index + 2),
                        name: format!("Spectator{index}"),
                        connected_at: None
                    }
                )
                .await
                .unwrap());
        }
        let coordinator = InMemoryRoomOperationCoordinator::new(
            Arc::new(InMemoryMessageCoordinator::new()),
            database.clone(),
            None,
        );
        // Initialize the native wait-queue mutex without creating readiness entries.
        assert!(coordinator.ready_player_room_ids().await.is_empty());
        for state in [LobbyState::Waiting, LobbyState::Lobby] {
            if state == LobbyState::Lobby {
                database.transition_room_to_lobby(&room.id).await.unwrap();
            }
            let region = stats_alloc::Region::new(ALLOCATOR);
            let ready = coordinator.current_ready_players(&room.id).await;
            let stats = region.change();
            assert!(ready.is_empty());
            println!("UNTOUCHED_READY_SNAPSHOT_PROFILE spectators={spectators} state={state:?} stats={stats:?}");
            profiles.push((stats.allocations, stats.bytes_allocated));
        }
    }
    if !profiles.iter().all(|profile| *profile == profiles[0]) {
        diagnose_untouched_snapshot_allocations().await;
    }
    assert!(
        profiles.iter().all(|profile| *profile == profiles[0]),
        "untouched ready snapshots copied spectator state: {profiles:?}"
    );
}

/// Attribute the first yielding readiness read to scheduler initialization.
#[test]
#[ignore = "isolated runtime allocation attribution control"]
fn cooperative_runtime_allocation_profile() {
    let mut profiles = Vec::new();
    for warm_scheduler in [false, true] {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (coordinator, room) = tokio::task::unconstrained(async {
                let database = Arc::new(InMemoryDatabase::new());
                let room = database
                    .create_room(
                        "scheduler".into(),
                        None,
                        4,
                        true,
                        Uuid::from_u128(1),
                        "relay".into(),
                        "test".into(),
                        None,
                    )
                    .await
                    .unwrap();
                (
                    InMemoryRoomOperationCoordinator::new(
                        Arc::new(InMemoryMessageCoordinator::new()),
                        database,
                        None,
                    ),
                    room,
                )
            })
            .await;
            if warm_scheduler {
                tokio::task::yield_now().await;
            }
            for _ in 0..127 {
                tokio::task::consume_budget().await;
            }
            let region = stats_alloc::Region::new(ALLOCATOR);
            assert!(coordinator.current_ready_players(&room.id).await.is_empty());
            let stats = region.change();
            profiles.push(stats);
            println!("RUNTIME_READY_SNAPSHOT warm_scheduler={warm_scheduler} stats={stats:?}");
        });
    }
    assert_eq!(profiles[0].allocations, profiles[1].allocations + 1);
    assert_eq!(
        profiles[0].bytes_allocated,
        profiles[1].bytes_allocated + 64
    );
}

/// Separate storage, first coordinator reads, repeated reads and scheduling on failure.
async fn diagnose_untouched_snapshot_allocations() {
    for state in [LobbyState::Waiting, LobbyState::Lobby] {
        let database = Arc::new(InMemoryDatabase::new());
        let room = database
            .create_room(
                "allocation-diagnostic".into(),
                None,
                4,
                true,
                Uuid::from_u128(1),
                "relay".into(),
                "test".into(),
                None,
            )
            .await
            .unwrap();
        if state == LobbyState::Lobby {
            database.transition_room_to_lobby(&room.id).await.unwrap();
        }
        let region = stats_alloc::Region::new(ALLOCATOR);
        let ready = database
            .get_finalized_room_ready_players(&room.id)
            .await
            .unwrap();
        let stats = region.change();
        assert_eq!(ready, Some(Vec::new()));
        println!("READY_ALLOCATION_DIAGNOSTIC state={state:?} stage=storage stats={stats:?}");
        for stage in ["first", "repeated", "unconstrained", "yield_before"] {
            let coordinator = InMemoryRoomOperationCoordinator::new(
                Arc::new(InMemoryMessageCoordinator::new()),
                database.clone(),
                None,
            );
            if stage == "repeated" {
                assert!(coordinator.current_ready_players(&room.id).await.is_empty());
            }
            if stage == "yield_before" {
                tokio::task::yield_now().await;
            }
            let region = stats_alloc::Region::new(ALLOCATOR);
            let ready = if stage == "unconstrained" {
                tokio::task::unconstrained(coordinator.current_ready_players(&room.id)).await
            } else {
                coordinator.current_ready_players(&room.id).await
            };
            let stats = region.change();
            assert!(ready.is_empty());
            println!("READY_ALLOCATION_DIAGNOSTIC state={state:?} stage={stage} stats={stats:?}");
        }
    }
}

/// Run the bounded failure diagnostics independently of the allocation assertion.
#[tokio::test]
#[ignore = "isolated allocation component diagnostics"]
async fn untouched_snapshot_component_diagnostics() {
    diagnose_untouched_snapshot_allocations().await;
}

/// Pin native lock initialization separately from readiness snapshot allocations.
async fn native_readiness_lock_initialization_control() {
    let coordinator = InMemoryRoomOperationCoordinator::new(
        Arc::new(InMemoryMessageCoordinator::new()),
        Arc::new(InMemoryDatabase::new()),
        None,
    );
    let cold = stats_alloc::Region::new(ALLOCATOR);
    tokio::task::unconstrained(async {
        assert!(coordinator.ready_player_room_ids().await.is_empty());
    })
    .await;
    let cold = cold.change();
    let warm = stats_alloc::Region::new(ALLOCATOR);
    tokio::task::unconstrained(async {
        assert!(coordinator.ready_player_room_ids().await.is_empty());
    })
    .await;
    let warm = warm.change();
    println!("NATIVE_READINESS_LOCK cold={cold:?} warm={warm:?}");
    #[cfg(target_os = "macos")]
    {
        assert_eq!(cold.allocations, warm.allocations + 1);
        assert_eq!(cold.bytes_allocated, warm.bytes_allocated + 64);
        assert_eq!(cold.deallocations, warm.deallocations);
    }
}
