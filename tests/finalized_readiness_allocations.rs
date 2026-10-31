//! Finalized readiness must not copy unrelated room metadata.

use signal_fish_server::database::{
    FinalizeRoomGameExpectation, FinalizeRoomGameOutcome, GameDatabase, InMemoryDatabase,
};
use signal_fish_server::protocol::{ConnectionInfo, LobbyState, PlayerInfo, SpectatorInfo};
use uuid::Uuid;

#[global_allocator]
static ALLOCATOR: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

#[tokio::test]
async fn finalized_readiness_projection_copies_only_ready_identities() {
    let mut profiles = Vec::new();
    for spectators in [0_u128, 16, 255] {
        let database = InMemoryDatabase::new();
        let creator = Uuid::from_u128(1);
        let room = database
            .create_room(
                "readiness".into(),
                None,
                8,
                false,
                creator,
                "relay".into(),
                "region".into(),
                None,
            )
            .await
            .unwrap();
        let members: Vec<_> = (1..=8).map(Uuid::from_u128).collect();
        for &id in members.iter().skip(1) {
            assert!(database
                .add_player_to_room(
                    &room.id,
                    PlayerInfo {
                        id,
                        name: "member".repeat(1024),
                        is_authority: false,
                        is_ready: false,
                        connected_at: None,
                        connection_info: Some(ConnectionInfo::Custom {
                            data: serde_json::json!({"nested": ["metadata".repeat(1024)]}),
                        }),
                        epoch: Some(3),
                        seq: Some(7),
                        region_id: "region".into(),
                    }
                )
                .await
                .unwrap());
        }
        for index in 0..spectators {
            assert!(database
                .add_spectator_to_room(
                    &room.id,
                    SpectatorInfo {
                        id: Uuid::from_u128(index + 100),
                        name: "spectator".repeat(1024),
                        connected_at: None,
                    }
                )
                .await
                .unwrap());
        }
        assert_eq!(
            database
                .get_finalized_room_ready_players(&room.id)
                .await
                .unwrap(),
            Some(vec![])
        );
        assert_eq!(
            database
                .finalize_room_game(
                    &room.id,
                    &FinalizeRoomGameExpectation {
                        members: members.clone(),
                        authority_player: None,
                        lobby_state: LobbyState::Waiting,
                    }
                )
                .await
                .unwrap(),
            FinalizeRoomGameOutcome::Finalized
        );
        let baseline_region = stats_alloc::Region::new(ALLOCATOR);
        let baseline = database.get_room_by_id(&room.id).await.unwrap().unwrap();
        let baseline_stats = baseline_region.change();
        let region = stats_alloc::Region::new(ALLOCATOR);
        let actual = database
            .get_finalized_room_ready_players(&room.id)
            .await
            .unwrap()
            .unwrap();
        let stats = region.change();
        assert_eq!(actual, baseline.ready_players);
        let mut sorted = actual;
        sorted.sort_unstable();
        assert_eq!(sorted, members);
        assert!(
            stats.bytes_allocated <= 256,
            "projection copied metadata: {stats:?}"
        );
        profiles.push((stats.allocations, stats.bytes_allocated));
        println!("FINALIZED_READINESS_PROFILE spectators={spectators} old={baseline_stats:?} new={stats:?}");
        database.delete_room(&room.id).await.unwrap();
        assert_eq!(
            database
                .get_finalized_room_ready_players(&room.id)
                .await
                .unwrap(),
            None
        );
    }
    assert!(
        profiles.iter().all(|profile| *profile == profiles[0]),
        "profiles={profiles:?}"
    );
}
