use criterion::{BatchSize, BenchmarkId, Criterion};
use signal_fish_server::database::{
    FinalizeRoomGameExpectation, FinalizeRoomGameOutcome, GameDatabase, InMemoryDatabase,
};
use signal_fish_server::protocol::{PlayerId, RoomId};
use std::hint::black_box;
use tokio::runtime::Runtime;

fn fixture(
    runtime: &Runtime,
    count: u8,
) -> (InMemoryDatabase, RoomId, FinalizeRoomGameExpectation) {
    runtime.block_on(async {
        let database = InMemoryDatabase::new();
        let creator = PlayerId::from_u128(1);
        let room = database
            .create_room(
                "finalization".into(),
                Some("FINAL".into()),
                count,
                true,
                creator,
                "relay".into(),
                "test".into(),
                None,
            )
            .await
            .unwrap();
        let template = &room.players[&creator];
        for index in 2..=count {
            let mut player = template.clone();
            player.id = PlayerId::from_u128(u128::from(index));
            assert!(database.add_player_to_room(&room.id, player).await.unwrap());
        }
        assert!(database.transition_room_to_lobby(&room.id).await.unwrap());
        let room = database.get_room_by_id(&room.id).await.unwrap().unwrap();
        let mut expected = FinalizeRoomGameExpectation::from_room(&room);
        expected.members.sort_unstable();
        (database, room.id, expected)
    })
}

pub(super) fn room_finalization(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut group = c.benchmark_group("room_finalization");
    for count in [16, 255] {
        let (database, room_id, mut stale) = fixture(&runtime, count);
        stale.authority_player = None;
        group.bench_function(BenchmarkId::new("stale_authority", count), |b| {
            b.iter(|| {
                let outcome = runtime
                    .block_on(database.finalize_room_game(&room_id, black_box(&stale)))
                    .unwrap();
                assert_eq!(outcome, FinalizeRoomGameOutcome::SnapshotChanged);
                black_box(outcome)
            });
        });
        for (case, unsorted) in [("sorted", false), ("unsorted", true)] {
            group.bench_function(BenchmarkId::new(case, count), |b| {
                b.iter_batched_ref(
                    || {
                        let (db, room, mut expected) = fixture(&runtime, count);
                        if unsorted {
                            expected.members.rotate_left(1);
                        }
                        (db, room, expected)
                    },
                    |(db, room, expected)| {
                        let outcome = runtime
                            .block_on(db.finalize_room_game(room, black_box(expected)))
                            .unwrap();
                        assert_eq!(outcome, FinalizeRoomGameOutcome::Finalized);
                        black_box(outcome)
                    },
                    BatchSize::PerIteration,
                );
            });
        }
    }
    group.finish();
}
