use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use signal_fish_server::database::{GameDatabase, InMemoryDatabase};
use signal_fish_server::protocol::PlayerId;
use std::hint::black_box;
use uuid::Uuid;

fn application_room_count(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("benchmark runtime");
    let mut group = c.benchmark_group("application_room_count");
    for total in [1_usize, 1_000, 10_000] {
        let database = InMemoryDatabase::new();
        let target = Uuid::from_u128(100);
        let other = Uuid::from_u128(200);
        let missing = Uuid::from_u128(300);
        let measured_room = runtime.block_on(async {
            let mut measured_room = None;
            for room in 0..total {
                let created = database
                    .create_room(
                        "game".into(),
                        Some(format!("{room:06}")),
                        4,
                        true,
                        PlayerId::from_u128(1),
                        "relay".into(),
                        "test".into(),
                        Some(if room % 2 == 0 { target } else { other }),
                    )
                    .await
                    .expect("benchmark room");
                measured_room = Some(created.id);
            }
            assert_eq!(
                database.get_application_room_count(&target).await.unwrap(),
                total.div_ceil(2)
            );
            assert_eq!(
                database.get_application_room_count(&missing).await.unwrap(),
                0
            );
            measured_room.expect("nonempty fixture")
        });
        let original_owner = if total % 2 == 1 { target } else { other };
        let mut owner = original_owner;
        group.bench_with_input(
            BenchmarkId::new("transfer", total),
            &measured_room,
            |b, room| {
                b.iter(|| {
                    owner = if owner == target { other } else { target };
                    runtime
                        .block_on(database.set_room_application_id(room, black_box(owner)))
                        .expect("transfer room");
                });
            },
        );
        runtime.block_on(async {
            database
                .set_room_application_id(&measured_room, original_owner)
                .await
                .expect("restore fixture ownership");
            assert_eq!(
                database.get_application_room_count(&target).await.unwrap(),
                total.div_ceil(2)
            );
            assert_eq!(
                database.get_application_room_count(&missing).await.unwrap(),
                0
            );
        });
        for (name, application) in [("target", target), ("missing", missing)] {
            group.bench_with_input(
                BenchmarkId::new(name, total),
                &application,
                |b, application| {
                    b.iter(|| {
                        black_box(
                            runtime.block_on(
                                database.get_application_room_count(black_box(application)),
                            ),
                        )
                        .expect("count rooms")
                    });
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, application_room_count);
criterion_main!(benches);
