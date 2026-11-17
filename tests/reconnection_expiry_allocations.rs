//! Unchanged reconnect-expiry sweeps must not allocate survivor room sets (#636).

mod allocation_harness;

use signal_fish_server::metrics::ServerMetrics;
use signal_fish_server::protocol::{PlayerId, PlayerInfo, RoomId, ServerMessage};
use signal_fish_server::reconnection::ReconnectionManager;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

#[global_allocator]
static ALLOCATOR: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

async fn unchanged_reconnect_expiry_preserves_replay_without_allocating() {
    allocation_harness::warm_current_thread_runtime().await;
    let mut profiles = Vec::with_capacity(4);
    for total in [0_u128, 1, 64, 1024] {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 2, Arc::clone(&metrics));
        let room = RoomId::from_u128(1);
        for index in 1..=total {
            manager
                .register_disconnection(
                    PlayerId::from_u128(index),
                    RoomId::from_u128(index),
                    false,
                    None,
                    0,
                )
                .await;
        }
        // Standalone buffers also survive a sweep with no expired records.
        manager.buffer_event(&room, ServerMessage::Pong).await;
        let before = manager.replay_sample().await;
        let region = stats_alloc::Region::new(ALLOCATOR);
        let removed = manager.cleanup_expired().await;
        let allocations = region.change();
        println!("EXPIRY_PROFILE total={total} stats={allocations:?}");
        assert_eq!(removed, 0);
        profiles.push((total, allocations));
        let after = manager.replay_sample().await;
        assert_eq!(after.pending_reconnections, before.pending_reconnections);
        assert_eq!(after.replay_rooms, before.replay_rooms);
        assert_eq!(after.replay_events, before.replay_events);
        assert_eq!(
            metrics.reconnection_sessions_active.load(Ordering::Relaxed),
            u64::try_from(total).unwrap()
        );
        let missed = manager.get_missed_events(&room, 0).await;
        assert!(!missed.truncated);
        assert!(matches!(missed.events.as_slice(), [ServerMessage::Pong]));
    }
    for (total, allocations) in profiles {
        assert_eq!(allocations.allocations, 0, "total={total}: {allocations:?}");
        assert_eq!(
            allocations.reallocations, 0,
            "total={total}: {allocations:?}"
        );
        assert_eq!(
            allocations.bytes_allocated, 0,
            "total={total}: {allocations:?}"
        );
    }
}

async fn reconnect_expiry_keeps_shared_room_replay_until_the_last_record_expires() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 2, Arc::clone(&metrics));
    let shared = RoomId::from_u128(1);
    let expired = RoomId::from_u128(2);
    let standalone = RoomId::from_u128(3);
    for (player, room) in [(1, shared), (2, shared), (3, expired)] {
        manager
            .register_disconnection(PlayerId::from_u128(player), room, false, None, 0)
            .await;
    }
    tokio::time::advance(Duration::from_secs(301)).await;
    manager
        .register_disconnection(PlayerId::from_u128(4), shared, false, None, 0)
        .await;
    for room in [shared, expired, standalone] {
        for _ in 0..3 {
            manager.buffer_event(&room, ServerMessage::Pong).await;
        }
    }
    assert_eq!(manager.cleanup_expired().await, 3);
    assert_eq!(
        metrics.reconnection_sessions_active.load(Ordering::Relaxed),
        1
    );
    for (room, events, truncated) in [
        (shared, 2, true),
        (expired, 0, false),
        (standalone, 2, true),
    ] {
        let missed = manager.get_missed_events(&room, 0).await;
        assert_eq!(missed.events.len(), events);
        assert_eq!(missed.truncated, truncated);
        assert!(missed
            .events
            .iter()
            .all(|event| matches!(event, ServerMessage::Pong)));
    }
    assert_eq!(manager.cleanup_expired().await, 0);
    tokio::time::advance(Duration::from_secs(301)).await;
    assert_eq!(manager.cleanup_expired().await, 1);
    assert_eq!(
        metrics.reconnection_sessions_active.load(Ordering::Relaxed),
        0
    );
    assert!(manager
        .get_missed_events(&shared, 0)
        .await
        .events
        .is_empty());
    assert_eq!(
        manager.get_missed_events(&standalone, 0).await.events.len(),
        2
    );
}

async fn claimed_disconnect_skips_retained_metadata_copies() {
    allocation_harness::warm_current_thread_runtime().await;
    let mut profiles = Vec::with_capacity(4);
    for name_bytes in [0, 64, 1024, 65536] {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 2, Arc::clone(&metrics));
        let player = PlayerId::from_u128(1);
        let room = RoomId::from_u128(2);
        let info = PlayerInfo {
            id: player,
            name: "n".repeat(name_bytes),
            is_authority: true,
            is_ready: false,
            connected_at: Some(chrono::Utc::now()),
            connection_info: None,
            epoch: Some(7),
            seq: Some(9),
            region_id: "test".into(),
        };
        let token = manager
            .register_disconnection(player, room, true, Some(info.clone()), 7)
            .await;
        let claim = manager
            .claim_reconnection(&PlayerId::from_u128(3), &player, &room, &token)
            .await
            .unwrap();
        let rotated = manager.pre_issue_token(player, room).await;
        manager.buffer_event(&room, ServerMessage::Pong).await;
        let before = manager.replay_sample().await;
        let region = stats_alloc::Region::new(ALLOCATOR);
        let returned = manager
            .register_disconnection(player, room, false, None, 0)
            .await;
        let stats = region.change();
        profiles.push((name_bytes, stats, token.len()));
        println!("CLAIMED_DISCONNECT_PROFILE name_bytes={name_bytes} stats={stats:?}");
        assert_eq!(returned, token);
        assert_eq!(manager.replay_sample().await, before);
        assert_eq!(
            metrics.reconnection_sessions_active.load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics.reconnection_tokens_issued.load(Ordering::Relaxed),
            2
        );
        manager.release_reconnection_claim(&claim).await;
        let retry = manager
            .claim_reconnection(&PlayerId::from_u128(4), &player, &room, &token)
            .await
            .unwrap();
        assert_eq!(retry.disconnected.last_epoch, 7);
        assert!(retry.disconnected.was_authority);
        assert_eq!(
            serde_json::to_value(retry.disconnected.player_info.as_ref().unwrap()).unwrap(),
            serde_json::to_value(&info).unwrap()
        );
        assert!(manager.complete_claimed_reconnection(&retry).await);
        assert_eq!(
            manager
                .register_disconnection(player, room, false, None, 0)
                .await,
            rotated
        );
    }
    for (name_bytes, stats, token_bytes) in profiles {
        assert_eq!(stats.allocations, 1, "name_bytes={name_bytes}: {stats:?}");
        assert_eq!(stats.reallocations, 0, "name_bytes={name_bytes}: {stats:?}");
        assert_eq!(
            stats.bytes_allocated, token_bytes,
            "name_bytes={name_bytes}: {stats:?}"
        );
    }
}

fn main() {
    allocation_harness::run_trials(vec![
        allocation_harness::trial("claimed_disconnect_skips_retained_metadata_copies", || {
            allocation_harness::block_on(claimed_disconnect_skips_retained_metadata_copies());
        }),
        allocation_harness::trial(
            "unchanged_reconnect_expiry_preserves_replay_without_allocating",
            || {
                allocation_harness::block_on(
                    unchanged_reconnect_expiry_preserves_replay_without_allocating(),
                );
            },
        ),
        allocation_harness::trial(
            "reconnect_expiry_keeps_shared_room_replay_until_the_last_record_expires",
            || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .start_paused(true)
                    .build()
                    .unwrap()
                    .block_on(
                        reconnect_expiry_keeps_shared_room_replay_until_the_last_record_expires(),
                    );
            },
        ),
    ]);
}
