//! Finalized readiness must not copy unrelated room metadata.

mod allocation_harness;

use signal_fish_server::database::{
    FinalizeRoomGameExpectation, FinalizeRoomGameOutcome, GameDatabase, InMemoryDatabase,
};
use signal_fish_server::protocol::{ConnectionInfo, LobbyState, PlayerInfo, SpectatorInfo};
use uuid::Uuid;

#[global_allocator]
static ALLOCATOR: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

/// Copy exact ready identities with fixed allocation cost as unrelated spectator metadata grows.
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

/// #636: complete game starts reuse prepared readiness storage at both buffer boundaries.
async fn finalization_reuses_readiness_storage_and_inline_members() {
    let mut excess = Vec::new();
    for count in [1_u8, 2, 15, 16, 17, 64, 255] {
        for prepared in [false, true] {
            let database = InMemoryDatabase::new();
            let room = database
                .create_room(
                    "finalization".into(),
                    None,
                    count,
                    true,
                    Uuid::from_u128(1),
                    "relay".into(),
                    "region".into(),
                    None,
                )
                .await
                .unwrap();
            let template = room.players.values().next().unwrap();
            for id in 2..=u128::from(count) {
                let mut player = template.clone();
                player.id = Uuid::from_u128(id);
                assert!(database.add_player_to_room(&room.id, player).await.unwrap());
            }
            if prepared {
                assert!(database.transition_room_to_lobby(&room.id).await.unwrap());
                for id in 1..=u128::from(count) {
                    assert!(database
                        .toggle_player_ready(&room.id, &Uuid::from_u128(id))
                        .await
                        .unwrap()
                        .is_some());
                }
            }
            let before = database.get_room_by_id(&room.id).await.unwrap().unwrap();
            let expected_order: Vec<_> = before.players.keys().copied().collect();
            let mut expectation = FinalizeRoomGameExpectation::from_room(&before);
            expectation.members.reverse();
            let original = expectation.clone();
            let region = stats_alloc::Region::new(ALLOCATOR);
            let outcome = database
                .finalize_room_game(&room.id, &expectation)
                .await
                .unwrap();
            let stats = region.change();
            println!("FINALIZATION_PROFILE count={count} prepared={prepared} stats={stats:?}");
            assert_eq!(outcome, FinalizeRoomGameOutcome::Finalized);
            assert_eq!(expectation, original);
            let after = database.get_room_by_id(&room.id).await.unwrap().unwrap();
            assert_eq!(after.ready_players, expected_order);
            assert_eq!(after.lobby_state, LobbyState::Finalized);
            assert!(after.players.values().all(|player| player.is_ready));
            assert!(after.game_finalized_at.is_some());
            assert!(after.last_activity >= before.last_activity);
            let repeated = stats_alloc::Region::new(ALLOCATOR);
            assert_eq!(
                database
                    .finalize_room_game(&room.id, &expectation)
                    .await
                    .unwrap(),
                FinalizeRoomGameOutcome::AlreadyFinalized
            );
            let repeated_stats = repeated.change();
            assert_eq!(
                database
                    .get_room_by_id(&room.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .game_finalized_at,
                after.game_finalized_at
            );
            let ceiling =
                repeated_stats.allocations + usize::from(count > 16) + usize::from(!prepared);
            let byte_ceiling = repeated_stats.bytes_allocated
                + (usize::from(count > 16) + usize::from(!prepared))
                    * usize::from(count)
                    * std::mem::size_of::<Uuid>();
            assert_eq!(stats.reallocations, 0, "count={count} prepared={prepared}");
            if stats.allocations > ceiling || stats.bytes_allocated > byte_ceiling {
                excess.push((count, prepared, ceiling, stats));
            }
        }
    }
    assert!(
        excess.is_empty(),
        "finalization allocated temporary member/readiness storage: {excess:?}"
    );
}

/// Keep each original history and diagnostic discoverable on the measuring thread.
fn main() {
    allocation_harness::run_trials(vec![
        allocation_harness::trial(
            "finalized_readiness_projection_copies_only_ready_identities",
            || {
                allocation_harness::block_on(
                    finalized_readiness_projection_copies_only_ready_identities(),
                )
            },
        ),
        allocation_harness::trial(
            "finalization_reuses_readiness_storage_and_inline_members",
            || {
                allocation_harness::block_on(
                    finalization_reuses_readiness_storage_and_inline_members(),
                )
            },
        ),
    ]);
}
