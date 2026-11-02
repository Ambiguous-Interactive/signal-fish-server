//! Allocation contract for the room-code read used after player departure.

use signal_fish_server::database::{GameDatabase, InMemoryDatabase};
use signal_fish_server::protocol::SpectatorInfo;
use uuid::Uuid;

#[global_allocator]
static ALLOCATOR: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

/// Room-code reads must not copy unrelated spectator metadata.
#[test]
#[serial_test::serial]
fn room_code_read_cost_does_not_grow_with_spectator_rosters() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::task::yield_now().await;
        let database = InMemoryDatabase::new();
        let room = database
            .create_room(
                "code-profile".into(),
                Some("CODE01".into()),
                4,
                true,
                Uuid::from_u128(1),
                "relay".into(),
                "test".into(),
                None,
            )
            .await
            .unwrap();
        database
            .set_room_max_spectators(&room.id, Some(255))
            .await
            .unwrap();
        let mut profiles = Vec::new();
        for spectators in [0_u128, 16, 254] {
            for index in profiles.last().map_or(0, |(count, _)| *count)..spectators {
                assert!(database
                    .add_spectator_to_room(
                        &room.id,
                        SpectatorInfo {
                            id: Uuid::from_u128(index + 2),
                            name: format!("Viewer-{index}-{}", "x".repeat(64)),
                            connected_at: None,
                        }
                    )
                    .await
                    .unwrap());
            }
            let region = stats_alloc::Region::new(ALLOCATOR);
            for _ in 0..64 {
                assert_eq!(
                    database.get_room_code(&room.id).await.unwrap().as_deref(),
                    Some("CODE01")
                );
            }
            let stats = region.change();
            println!("ROOM_CODE_PROFILE spectators={spectators} stats={stats:?}");
            profiles.push((spectators, stats));
        }
        let baseline = profiles[0].1;
        for (spectators, profile) in &profiles {
            assert_eq!(
                profile.allocations, baseline.allocations,
                "spectators={spectators}"
            );
            assert_eq!(
                profile.bytes_allocated, baseline.bytes_allocated,
                "spectators={spectators}"
            );
        }
    });
}

mod test_helpers;

/// The complete departure keeps its confirmation and peer event without name copies.
#[test]
#[serial_test::serial]
fn player_leave_does_not_copy_stored_spectator_names() {
    use signal_fish_server::protocol::ServerMessage;

    let mut profiles = Vec::new();
    for spectators in [0_u128, 16, 254] {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::task::yield_now().await;
            let server = test_helpers::create_test_server().await;
            let host = Uuid::from_u128(1);
            let guest = Uuid::from_u128(2);
            let (host_sender, mut host_receiver) = tokio::sync::mpsc::channel(16);
            let (guest_sender, mut guest_receiver) = tokio::sync::mpsc::channel(16);
            server.connect_client(host, host_sender).await;
            server.connect_client(guest, guest_sender).await;
            for (player, name) in [(host, "Host"), (guest, "Guest")] {
                server.handle_join_room(
                    &player, "leave-profile".into(), Some("LEAVE1".into()), name.into(),
                    Some(4), Some(true), None, None, None,
                ).await;
            }
            while host_receiver.try_recv().is_ok() {}
            while guest_receiver.try_recv().is_ok() {}
            let room_id = server.get_client_room(&host).await.unwrap();
            server.database().set_room_max_spectators(&room_id, Some(255)).await.unwrap();
            for index in 0..spectators {
                assert!(server.database().add_spectator_to_room(&room_id, SpectatorInfo {
                    id: Uuid::from_u128(index + 3),
                    name: format!("Viewer-{index}-{}", "x".repeat(64)),
                    connected_at: None,
                }).await.unwrap());
            }
            let region = stats_alloc::Region::new(ALLOCATOR);
            server.leave_room(&guest).await;
            let stats = region.change();
            println!("PLAYER_LEAVE_PROFILE spectators={spectators} stats={stats:?}");
            profiles.push((spectators, stats.allocations));
            assert!(matches!(guest_receiver.try_recv().unwrap().as_ref(), ServerMessage::RoomLeft));
            assert!(matches!(host_receiver.try_recv().unwrap().as_ref(), ServerMessage::PlayerLeft { player_id, .. } if *player_id == guest));
            assert!(guest_receiver.try_recv().is_err());
            assert!(host_receiver.try_recv().is_err());
            assert_eq!(server.get_client_room(&guest).await, None);
            assert_eq!(server.get_client_room(&host).await, Some(room_id));
            let room = server.database().get_room_by_id(&room_id).await.unwrap().unwrap();
            assert!(!room.players.contains_key(&guest));
            assert_eq!(room.players.len(), 1);
            assert_eq!(room.spectators.len(), usize::try_from(spectators).unwrap());
            for index in 0..spectators {
                assert_eq!(room.spectators[&Uuid::from_u128(index + 3)].name,
                    format!("Viewer-{index}-{}", "x".repeat(64)));
            }
        });
    }
    let baseline = profiles[0].1;
    for (spectators, allocations) in profiles {
        assert!(
            allocations <= baseline + 4,
            "spectators={spectators}: allocations={allocations}, baseline={baseline}"
        );
    }
}
