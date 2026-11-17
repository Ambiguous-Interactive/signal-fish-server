use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use signal_fish_server::database::{GameDatabase, InMemoryDatabase};
use signal_fish_server::protocol::{PlayerId, PlayerInfo, RoomId};
use std::hint::black_box;

fn player_removal(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("benchmark runtime");
    let mut group = c.benchmark_group("player_removal");
    for total in [1_usize, 16] {
        let database = InMemoryDatabase::new();
        let member = PlayerId::from_u128(1);
        let room = runtime.block_on(async {
            let room = database
                .create_room(
                    "removal".into(),
                    Some("REMOVE".into()),
                    16,
                    true,
                    member,
                    "relay".into(),
                    "test".into(),
                    None,
                )
                .await
                .expect("benchmark room");
            let template = room.players[&member].clone();
            for index in 1..total {
                let mut player = template.clone();
                player.id = PlayerId::from_u128(u128::try_from(index).unwrap() + 1);
                assert!(database.add_player_to_room(&room.id, player).await.unwrap());
            }
            database.get_room_by_id(&room.id).await.unwrap().unwrap()
        });
        for case in ["missing_room", "absent_member", "removed"] {
            let target_room = if case == "missing_room" {
                RoomId::nil()
            } else {
                room.id
            };
            let target_member = if case == "removed" {
                member
            } else {
                PlayerId::nil()
            };
            group.bench_function(BenchmarkId::new(case, total), |b| {
                if case == "removed" {
                    let player: PlayerInfo = room.players[&member].clone();
                    b.iter_batched(
                        || {
                            if !runtime
                                .block_on(database.get_room_by_id(&room.id))
                                .unwrap()
                                .unwrap()
                                .players
                                .contains_key(&member)
                            {
                                assert!(runtime
                                    .block_on(database.add_player_to_room(&room.id, player.clone()))
                                    .unwrap());
                            }
                        },
                        |()| {
                            let removed = runtime
                                .block_on(
                                    database.remove_player_from_room(&target_room, &target_member),
                                )
                                .unwrap()
                                .unwrap();
                            assert_eq!(removed.id, member);
                            black_box(removed)
                        },
                        BatchSize::PerIteration,
                    );
                    assert!(runtime
                        .block_on(database.add_player_to_room(&room.id, player))
                        .unwrap());
                } else {
                    b.iter(|| {
                        let removed = runtime
                            .block_on(database.remove_player_from_room(
                                black_box(&target_room),
                                black_box(&target_member),
                            ))
                            .unwrap();
                        assert!(removed.is_none());
                        black_box(removed)
                    });
                }
            });
            let current = runtime
                .block_on(database.get_room_by_id(&room.id))
                .unwrap()
                .unwrap();
            assert_eq!(current.players.len(), total);
        }
    }
    group.finish();
}

criterion_group!(benches, player_removal);
criterion_main!(benches);
