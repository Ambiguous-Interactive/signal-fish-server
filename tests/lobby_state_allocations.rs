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
    ready_toggle_allocations_do_not_grow_with_spectator_rosters().await;
    lobby_promotion_allocations_do_not_grow_with_spectator_rosters().await;
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
        coordinator
            .handle_player_ready(&room.id, &member, None)
            .await
            .unwrap();
        assert_ready_publication(&receiver.try_recv().unwrap(), member, true);
        let region = stats_alloc::Region::new(ALLOCATOR);
        assert!(coordinator
            .transition_room_to_lobby(&room.id)
            .await
            .unwrap());
        let stats = region.change();
        profiles.push((stats.allocations, stats.bytes_allocated));
        println!("LOBBY_PROMOTION_PROFILE spectators={spectators} stats={stats:?}");
        let message = receiver.try_recv().unwrap();
        assert_ready_publication(&message, member, true);
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
