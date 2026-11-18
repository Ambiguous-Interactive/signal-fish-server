use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use signal_fish_server::database::{GameDatabase, InMemoryDatabase};
use signal_fish_server::protocol::PlayerId;
use std::hint::black_box;

#[path = "support/player_removal.rs"]
mod player_removal;
#[path = "support/room_finalization.rs"]
mod room_finalization;

fn game_room_count(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("benchmark runtime");
    let mut group = c.benchmark_group("game_room_count");
    for total in [1_usize, 1_000, 10_000] {
        let database = InMemoryDatabase::new();
        runtime.block_on(async {
            for room in 0..total {
                database
                    .create_room(
                        if room % 2 == 0 { "target" } else { "other" }.into(),
                        Some(format!("{room:06}")),
                        4,
                        true,
                        PlayerId::from_u128(1),
                        "relay".into(),
                        "test".into(),
                        None,
                    )
                    .await
                    .expect("benchmark room");
            }
            assert_eq!(
                database.get_game_room_count("target").await.unwrap(),
                total.div_ceil(2)
            );
            assert_eq!(database.get_game_room_count("missing").await.unwrap(), 0);
        });
        for game in ["target", "missing"] {
            group.bench_with_input(BenchmarkId::new(game, total), &game, |b, game| {
                b.iter(|| {
                    black_box(runtime.block_on(database.get_game_room_count(black_box(game))))
                        .expect("count rooms")
                });
            });
        }
    }
    group.finish();
}

criterion_group!(
    benches,
    game_room_count,
    player_removal::player_removal,
    room_finalization::room_finalization
);
criterion_main!(benches);
