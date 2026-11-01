use super::*;

fn sample(rooms: usize) -> DashboardMetricsSnapshot {
    DashboardMetricsSnapshot {
        rooms_by_game: HashMap::from([("game".to_string(), rooms)]),
        player_percentiles: HashMap::from([("p50".to_string(), 2.0)]),
        game_percentiles: HashMap::from([(
            "game".to_string(),
            HashMap::from([("p95".to_string(), 4.0)]),
        )]),
        active_rooms: rooms,
        fetched_at: chrono::DateTime::from_timestamp(i64::try_from(rooms).unwrap(), 0).unwrap(),
        fetched_at_instant: tokio::time::Instant::now(),
    }
}

/// Reject separate-query cache routing through a deterministic room deletion.
#[tokio::test]
async fn cache_snapshot_uses_consistent_storage_during_room_deletion() {
    use crate::database::{GameDatabase, InMemoryDatabase};
    let database = Arc::new(InMemoryDatabase::new());
    let room = database
        .create_room(
            "cache-sample".into(),
            Some("CACHE1".into()),
            4,
            true,
            crate::protocol::PlayerId::from_u128(1),
            "relay".into(),
            "test".into(),
            None,
        )
        .await
        .unwrap();
    database.pause_next_get_room_by_id_for_test();
    let mut collection = Box::pin(DashboardMetricsCache::fetch_snapshot(database.clone()));
    assert!(futures_util::poll!(&mut collection).is_pending());
    database.delete_room(&room.id).await.unwrap();
    database.release_paused_get_room_by_id_for_test();
    let snapshot = collection.await.unwrap();
    assert_eq!(snapshot.active_rooms, 0);
    assert!(snapshot.rooms_by_game.is_empty());
    assert!(snapshot.player_percentiles.is_empty());
    assert!(snapshot.game_percentiles.is_empty());
}

/// Preserve selected fields, FIFO history and the last successful sample across errors.
#[tokio::test(start_paused = true)]
async fn refresh_keeps_selected_history_and_last_success_after_error() {
    for capacity in [0, 1, 4] {
        for full_history in [false, true] {
            let fields = if full_history {
                vec![
                    DashboardHistoryField::ActiveRooms,
                    DashboardHistoryField::RoomsByGame,
                    DashboardHistoryField::PlayerPercentiles,
                    DashboardHistoryField::GamePercentiles,
                ]
            } else {
                vec![DashboardHistoryField::ActiveRooms]
            };
            let cache = DashboardMetricsCache::new(
                Duration::from_secs(5),
                Duration::from_secs(10),
                Arc::new(crate::metrics::ServerMetrics::new()),
                capacity,
                &fields,
            );
            let retained = capacity.max(1);
            for rooms in 1..=8 {
                cache.record_refresh_result(Ok(sample(rooms))).await;
                let view = cache.view().await;
                assert_eq!(view.active_rooms, rooms);
                assert_eq!(view.rooms_by_game["game"], rooms);
                assert_eq!(view.player_percentiles["p50"], 2.0);
                assert_eq!(view.game_percentiles["game"]["p95"], 4.0);
                assert_eq!(view.history.len(), rooms.min(retained));
                for (index, entry) in view.history.iter().enumerate() {
                    assert_eq!(
                        entry.active_rooms,
                        Some(rooms - view.history.len() + index + 1)
                    );
                    assert_eq!(entry.rooms_by_game.is_some(), full_history);
                    assert_eq!(entry.player_percentiles.is_some(), full_history);
                    assert_eq!(entry.game_percentiles.is_some(), full_history);
                    if full_history {
                        assert_eq!(
                            entry.rooms_by_game.as_ref().unwrap()["game"],
                            entry.active_rooms.unwrap()
                        );
                        assert_eq!(entry.game_percentiles.as_ref().unwrap()["game"]["p95"], 4.0);
                    }
                }
                assert!(!view.stale);
                assert!(view.last_error.is_none());
                assert!(
                    cache.inner.read().await.history.capacity() <= retained,
                    "history must not grow beyond {retained} samples"
                );
            }
            let before = cache.view().await;
            cache
                .record_refresh_result(Err(anyhow::anyhow!("storage unavailable")))
                .await;
            tokio::time::advance(Duration::from_secs(11)).await;
            let failed = cache.view().await;
            assert_eq!(failed.fetched_at, before.fetched_at);
            assert_eq!(failed.active_rooms, before.active_rooms);
            assert_eq!(
                serde_json::to_value(&failed.history).unwrap(),
                serde_json::to_value(&before.history).unwrap()
            );
            assert_eq!(failed.last_error.as_deref(), Some("storage unavailable"));
            assert!(failed.stale);
            cache.record_refresh_result(Ok(sample(9))).await;
            let recovered = cache.view().await;
            assert_eq!(recovered.active_rooms, 9);
            assert!(recovered.last_error.is_none());
            assert!(!recovered.stale);
            assert_eq!(recovered.history.len(), retained);
            assert_eq!(recovered.history.last().unwrap().active_rooms, Some(9));
        }
    }
}

/// Measure full-history rollover without copying returned dashboard views.
#[test]
#[ignore = "isolated dashboard history rollover profile"]
fn dashboard_history_rollover_profile() {
    let fields = HistoryFields::from_fields(&[DashboardHistoryField::ActiveRooms]);
    let snapshot = sample(1);
    for capacity in [1, 100, 10_000] {
        let mut state = DashboardMetricsCacheState::new(capacity);
        for _ in 0..capacity {
            state.push_history(&snapshot, &fields);
        }
        let started = tokio::time::Instant::now();
        for _ in 0..30_000 {
            state.push_history(std::hint::black_box(&snapshot), &fields);
        }
        assert_eq!(state.history.len(), capacity);
        println!(
            "HISTORY_ROLLOVER capacity={capacity} pushes=30000 elapsed_us={}",
            started.elapsed().as_micros()
        );
        std::hint::black_box(state);
    }
}
