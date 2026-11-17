use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use signal_fish_server::database::{GameDatabase, InMemoryDatabase};
use signal_fish_server::protocol::{ConnectionInfo, PlayerId, RoomId};
use std::hint::black_box;

#[derive(Clone, Copy, Debug)]
enum Operation {
    Name,
    Connection,
    Clear,
}

impl Operation {
    async fn apply(
        self,
        database: &InMemoryDatabase,
        room: &RoomId,
        player: &PlayerId,
        alternate: bool,
    ) -> bool {
        match self {
            Self::Name => {
                database
                    .update_player_name(room, player, if alternate { "other" } else { "player" })
                    .await
            }
            Self::Connection => {
                database
                    .update_player_connection_info(room, player, connection(alternate))
                    .await
            }
            Self::Clear => database.clear_player_connection_info(room, player).await,
        }
        .expect("metadata result")
    }
}

fn connection(alternate: bool) -> ConnectionInfo {
    ConnectionInfo::Direct {
        host: "localhost".into(),
        port: if alternate { 4321 } else { 1234 },
    }
}

fn player_metadata(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("benchmark runtime");
    let mut group = c.benchmark_group("player_metadata");
    for operation in [Operation::Name, Operation::Connection, Operation::Clear] {
        for case in ["unchanged", "missing_member", "missing_room", "changed"] {
            let database = InMemoryDatabase::new();
            let member = PlayerId::from_u128(1);
            let room = runtime.block_on(async {
                let room = database
                    .create_room(
                        "metadata".into(),
                        Some("BENCH".into()),
                        4,
                        true,
                        member,
                        "relay".into(),
                        "test".into(),
                        None,
                    )
                    .await
                    .expect("benchmark room");
                assert!(operation.apply(&database, &room.id, &member, false).await);
                room
            });
            let target_room = if case == "missing_room" {
                RoomId::nil()
            } else {
                room.id
            };
            let target_member = if case == "missing_member" {
                PlayerId::nil()
            } else {
                member
            };
            let before = runtime
                .block_on(database.get_room_by_id(&room.id))
                .unwrap()
                .unwrap();
            group.bench_function(BenchmarkId::new(format!("{operation:?}"), case), |b| {
                if matches!(operation, Operation::Clear) && case == "changed" {
                    b.iter_batched(
                        || {
                            assert!(runtime
                                .block_on(database.update_player_connection_info(
                                    &room.id,
                                    &member,
                                    connection(false)
                                ))
                                .unwrap());
                        },
                        |()| {
                            black_box(runtime.block_on(operation.apply(
                                &database,
                                &target_room,
                                &target_member,
                                false,
                            )))
                        },
                        BatchSize::PerIteration,
                    );
                } else {
                    let mut alternate = false;
                    b.iter(|| {
                        alternate = case == "changed" && !alternate;
                        black_box(runtime.block_on(operation.apply(
                            &database,
                            &target_room,
                            &target_member,
                            alternate,
                        )))
                    });
                }
            });
            let stored = runtime
                .block_on(database.get_room_by_id(&room.id))
                .unwrap()
                .unwrap();
            assert_eq!(stored.players.len(), 1);
            assert_eq!(
                stored.players[&member].connection_info.is_none(),
                !matches!(operation, Operation::Connection)
            );
            if case != "changed" {
                assert_eq!(stored.last_activity, before.last_activity);
                assert_eq!(
                    serde_json::to_value(&stored.players).unwrap(),
                    serde_json::to_value(&before.players).unwrap()
                );
            }
        }
    }
    group.finish();
}

criterion_group!(benches, player_metadata);
criterion_main!(benches);
