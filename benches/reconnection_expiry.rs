use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use signal_fish_server::metrics::ServerMetrics;
use signal_fish_server::protocol::{PlayerId, PlayerInfo, RoomId};
use signal_fish_server::reconnection::ReconnectionManager;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

async fn fixture(total: u128, expired: u128, shared_room: bool) -> ReconnectionManager {
    let manager = ReconnectionManager::new(300, 16, Arc::new(ServerMetrics::new()));
    for index in 0..total {
        if index == expired && expired > 0 {
            tokio::time::advance(Duration::from_secs(301)).await;
        }
        manager
            .register_disconnection(
                PlayerId::from_u128(index + 1),
                RoomId::from_u128(if shared_room { 1 } else { index + 1 }),
                false,
                None,
                0,
            )
            .await;
    }
    if total == expired && expired > 0 {
        tokio::time::advance(Duration::from_secs(301)).await;
    }
    manager
}

fn reconnection_expiry(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .expect("benchmark runtime");
    let mut group = c.benchmark_group("reconnection_expiry");
    for total in [0, 1, 64, 1024, 10_000] {
        let manager = runtime.block_on(fixture(total, 0, false));
        group.bench_function(BenchmarkId::new("unchanged", total), |b| {
            b.iter(|| {
                let removed = runtime.block_on(manager.cleanup_expired());
                assert_eq!(removed, 0);
                black_box(removed)
            });
        });
    }
    for (name, expired, shared_room) in [
        ("one_expired", 1, false),
        ("shared_room", 1, true),
        ("all_expired", 128, false),
    ] {
        group.bench_function(name, |b| {
            b.iter_batched(
                || runtime.block_on(fixture(128, expired, shared_room)),
                |manager| {
                    let removed = runtime.block_on(manager.cleanup_expired());
                    assert_eq!(removed, usize::try_from(expired).unwrap());
                    black_box(manager)
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

fn claimed_registration(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("benchmark runtime");
    let mut group = c.benchmark_group("claimed_registration");
    for name_bytes in [0, 64, 1024, 65536] {
        let manager = ReconnectionManager::new(300, 16, Arc::new(ServerMetrics::new()));
        let player = PlayerId::from_u128(1);
        let room = RoomId::from_u128(2);
        let (token, claim, rotated) = runtime.block_on(async {
            let token = manager
                .register_disconnection(
                    player,
                    room,
                    true,
                    Some(PlayerInfo {
                        id: player,
                        name: "n".repeat(name_bytes),
                        is_authority: true,
                        is_ready: false,
                        connected_at: None,
                        connection_info: None,
                        epoch: Some(7),
                        seq: Some(9),
                        region_id: "test".into(),
                    }),
                    7,
                )
                .await;
            let claim = manager
                .claim_reconnection(&PlayerId::from_u128(3), &player, &room, &token)
                .await
                .unwrap();
            let rotated = manager.pre_issue_token(player, room).await;
            (token, claim, rotated)
        });
        group.bench_function(BenchmarkId::new("retained_name_bytes", name_bytes), |b| {
            b.iter(|| {
                black_box(
                    runtime.block_on(manager.register_disconnection(player, room, false, None, 0)),
                )
            });
        });
        runtime.block_on(async {
            assert_eq!(
                manager
                    .register_disconnection(player, room, false, None, 0)
                    .await,
                token
            );
            assert!(manager.complete_claimed_reconnection(&claim).await);
            assert_eq!(
                manager
                    .register_disconnection(player, room, false, None, 0)
                    .await,
                rotated
            );
        });
    }
    group.finish();
}

criterion_group!(benches, reconnection_expiry, claimed_registration);
criterion_main!(benches);
