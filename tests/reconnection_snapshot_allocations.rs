//! Reconnect capture must copy only the departing member's metadata.

use signal_fish_server::database::{GameDatabase, InMemoryDatabase};
use signal_fish_server::protocol::{ConnectionInfo, PlayerInfo, SpectatorInfo};
use uuid::Uuid;

#[global_allocator]
static ALLOCATOR: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

/// Bound reconnect capture allocations independently of unrelated room metadata.
#[tokio::test]
async fn room_player_projection_preserves_metadata_without_copying_other_members() {
    let mut profiles = Vec::new();
    for (members, spectators) in [(1_u128, 0_u128), (4, 16), (8, 255)] {
        let database = InMemoryDatabase::new();
        let creator = Uuid::from_u128(1);
        let room = database
            .create_room(
                "snapshot".into(),
                None,
                8,
                true,
                creator,
                "relay".into(),
                "region".into(),
                None,
            )
            .await
            .unwrap();
        database
            .update_player_connection_info(
                &room.id,
                &creator,
                ConnectionInfo::Custom {
                    data: serde_json::json!({"nested": ["retained metadata"]}),
                },
            )
            .await
            .unwrap();
        for index in 2..=members {
            assert!(database
                .add_player_to_room(
                    &room.id,
                    PlayerInfo {
                        id: Uuid::from_u128(index),
                        name: "member".repeat(1024),
                        is_authority: false,
                        is_ready: true,
                        connected_at: Some(chrono::Utc::now()),
                        connection_info: Some(ConnectionInfo::Custom {
                            data: serde_json::json!({"nested": ["payload".repeat(1024)]}),
                        }),
                        epoch: Some(7),
                        seq: Some(23),
                        region_id: "another region".into(),
                    },
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
                        connected_at: Some(chrono::Utc::now()),
                    },
                )
                .await
                .unwrap());
        }
        let baseline_region = stats_alloc::Region::new(ALLOCATOR);
        let baseline = database.get_room_by_id(&room.id).await.unwrap().unwrap();
        let expected = baseline.players.get(&creator).cloned().unwrap();
        let baseline_stats = baseline_region.change();
        let region = stats_alloc::Region::new(ALLOCATOR);
        let actual = database
            .get_room_player_snapshot(&room.id, &creator)
            .await
            .unwrap()
            .unwrap();
        let stats = region.change();
        println!(
            "RECONNECT_CAPTURE_PROFILE members={members} spectators={spectators} full_capture={baseline_stats:?} projection={stats:?}"
        );
        assert_eq!(actual.authority_player, baseline.authority_player);
        let actual_player = actual.player.unwrap();
        assert_eq!(
            serde_json::to_value(&actual_player).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        assert_eq!(actual_player.region_id, expected.region_id);
        profiles.push((stats.allocations, stats.bytes_allocated));
        for expected in baseline.players.values() {
            let actual = database
                .get_room_player_snapshot(&room.id, &expected.id)
                .await
                .unwrap()
                .unwrap()
                .player
                .unwrap();
            assert_eq!(
                serde_json::to_value(&actual).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
            assert_eq!(actual.region_id, expected.region_id);
        }

        let absent_member = database
            .get_room_player_snapshot(&room.id, &Uuid::nil())
            .await
            .unwrap()
            .unwrap();
        assert!(absent_member.player.is_none());
        assert_eq!(absent_member.authority_player, baseline.authority_player);
        database
            .update_room_authority(&room.id, Some(creator))
            .await
            .unwrap();
        assert_eq!(
            database
                .get_room_player_snapshot(&room.id, &creator)
                .await
                .unwrap()
                .unwrap()
                .authority_player,
            Some(creator)
        );
        database
            .update_room_authority(&room.id, None)
            .await
            .unwrap();
        assert_eq!(
            database
                .get_room_player_snapshot(&room.id, &creator)
                .await
                .unwrap()
                .unwrap()
                .authority_player,
            None
        );
        assert!(database.delete_room(&room.id).await.unwrap());
        assert!(database
            .get_room_player_snapshot(&room.id, &creator)
            .await
            .unwrap()
            .is_none());
    }
    assert!(
        profiles.iter().all(|profile| *profile == profiles[0]),
        "reconnect capture copied other members: {profiles:?}"
    );
    let database = InMemoryDatabase::new();
    let creator = Uuid::from_u128(1);
    let pending = database
        .create_pending_room_classified(
            "pending".into(),
            None,
            4,
            true,
            creator,
            "relay".into(),
            "region".into(),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(database
        .get_room(&pending.game_name, &pending.code)
        .await
        .unwrap()
        .is_none());
    let snapshot = database
        .get_room_player_snapshot(&pending.id, &creator)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.player.unwrap().id, creator);
    assert_eq!(snapshot.authority_player, pending.authority_player);
}
