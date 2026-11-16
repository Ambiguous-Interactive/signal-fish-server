//! Bound connection discovery scratch independently of unselected connections.
use super::*;
use signal_fish_server::server::allocation_benchmark::{
    assign_room_for_maintenance_measurement, maintenance_candidates_for_measurement,
};
use std::hint::black_box;
use std::time::Instant;

/// #636: a blocked cleanup attempt needs room identity, not retained gameplay metadata.
pub(crate) fn abandoned_room_discovery_skips_member_metadata() {
    use signal_fish_server::server::allocation_benchmark::{
        cleanup_abandoned_room_for_measurement, room_code_lock_for_measurement,
    };

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
        let creator = Uuid::from_u128(1);
        let room = server
            .database()
            .create_pending_room_classified(
                "cleanup-profile".into(),
                Some("CLEAN1".into()),
                2,
                false,
                creator,
                "relay".into(),
                "test".into(),
                None,
                None,
            )
            .await
            .unwrap();
        server
            .database()
            .abandon_room(&room.id, &creator)
            .await
            .unwrap();
        let locks = room_code_lock_for_measurement(&server);
        let held = locks
            .acquire(
                &format!("room_join:{}:{}", room.game_name, room.code),
                std::time::Duration::from_secs(3600),
            )
            .await
            .unwrap();
        let mut profiles = Vec::new();
        for bytes in [0, 1024, 65536, 0, 65536] {
            let metadata = serde_json::json!({"nested": ["x".repeat(bytes)]});
            assert!(server
                .database()
                .update_player_connection_info(
                    &room.id,
                    &creator,
                    ConnectionInfo::Custom {
                        data: metadata.clone()
                    },
                )
                .await
                .unwrap());
            for round in 0..3 {
                let region = stats_alloc::Region::new(ALLOCATOR);
                let deleted = cleanup_abandoned_room_for_measurement(&server, room.id).await;
                let stats = region.change();
                assert!(
                    !deleted,
                    "cleanup must preserve a room with its code locked"
                );
                println!("ABANDONED_DISCOVERY bytes={bytes} round={round} stats={stats:?}");
                if round > 0 {
                    profiles.push((bytes, stats));
                }
            }
            let stored = server
                .database()
                .get_room_by_id(&room.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(stored.game_name, room.game_name);
            assert_eq!(stored.code, room.code);
            assert_eq!(stored.players.len(), 1);
            assert!(matches!(&stored.players[&creator].connection_info,
                Some(ConnectionInfo::Custom { data }) if *data == metadata));
            assert_eq!(
                server.database().pending_room_ids().await.unwrap(),
                vec![room.id]
            );
        }
        assert!(locks.release(&held).await.unwrap());
        assert!(cleanup_abandoned_room_for_measurement(&server, room.id).await);
        assert!(!server.database().room_exists(&room.id).await.unwrap());
        assert!(!cleanup_abandoned_room_for_measurement(&server, room.id).await);
        for (bytes, stats) in &profiles {
            assert_eq!(
                stats.allocations, profiles[0].1.allocations,
                "room discovery copied member metadata: bytes={bytes} profiles={profiles:?}"
            );
            assert_eq!(
                stats.bytes_allocated, profiles[0].1.bytes_allocated,
                "room discovery copied member metadata: bytes={bytes} profiles={profiles:?}"
            );
        }
    });
}

pub(crate) fn maintenance_discovery_skips_unselected_connections() {
    allocation_harness::block_on(async {
        let mut empty_profiles = Vec::new();
        for count in [0_u128, 16, 256, 4096] {
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
                let (sender, receiver) = tokio::sync::mpsc::channel(4);
                receivers.push(receiver);
                server.connect_client(Uuid::from_u128(index), sender).await;
            }
            for unregister in [false, true] {
                let region = stats_alloc::Region::new(ALLOCATOR);
                let candidates = maintenance_candidates_for_measurement(&server, unregister);
                let stats = region.change();
                assert!(candidates.is_empty());
                println!("MAINTENANCE_EMPTY count={count} unregister={unregister} stats={stats:?}");
                empty_profiles.push((count, unregister, stats));
                record_discovery_times(&server, count, unregister, false);
            }
            let mut expected = Vec::new();
            for index in (1..=count).step_by(2) {
                let player = Uuid::from_u128(index);
                let room = Uuid::from_u128(10_000 + index % 7);
                assign_room_for_maintenance_measurement(&server, &player, room).await;
                expected.push((room, player));
            }
            expected.sort_unstable();
            let region = stats_alloc::Region::new(ALLOCATOR);
            let mut actual = maintenance_candidates_for_measurement(&server, false);
            let stats = region.change();
            actual.sort_unstable();
            assert_eq!(
                actual, expected,
                "mixed room assignments at {count} clients"
            );
            println!(
                "MAINTENANCE_ASSIGNED count={count} selected={} stats={stats:?}",
                actual.len()
            );
            let fixed = empty_profiles
                .iter()
                .find(|(size, unregister, _)| *size == 0 && !unregister)
                .unwrap()
                .2;
            assert!(stats.allocations <= fixed.allocations + usize::from(!actual.is_empty()));
            let output_bytes = actual.len() * std::mem::size_of::<(Uuid, Uuid)>();
            assert!(
                stats.bytes_allocated <= fixed.bytes_allocated + output_bytes,
                "assigned discovery retained client-ID scratch: count={count} stats={stats:?}"
            );
            record_discovery_times(&server, count, false, true);
            assert!(maintenance_candidates_for_measurement(&server, true).is_empty());
        }
        for unregister in [false, true] {
            let baseline = empty_profiles
                .iter()
                .find(|(count, mode, _)| *count == 0 && *mode == unregister)
                .unwrap()
                .2;
            for (count, mode, stats) in &empty_profiles {
                if *mode == unregister {
                    assert_eq!(stats.allocations, baseline.allocations,
                        "unselected connections added scratch: count={count} unregister={unregister} stats={stats:?} baseline={baseline:?}");
                    assert_eq!(stats.reallocations, baseline.reallocations);
                    assert_eq!(stats.bytes_allocated, baseline.bytes_allocated);
                }
            }
        }
    });
}

fn record_discovery_times(
    server: &EnhancedGameServer,
    count: u128,
    unregister: bool,
    assigned: bool,
) {
    for round in 0..5 {
        let started = Instant::now();
        for _ in 0..100 {
            black_box(maintenance_candidates_for_measurement(server, unregister));
        }
        println!("MAINTENANCE_TIME count={count} unregister={unregister} assigned={assigned} round={round} iterations=100 elapsed_ns={}", started.elapsed().as_nanos());
    }
}
