//! Allocation contract for complete spectator departure and both terminal rosters.

mod test_helpers;

use signal_fish_server::protocol::{ServerMessage, SpectatorInfo, SpectatorStateChangeReason};
use uuid::Uuid;

#[global_allocator]
static ALLOCATOR: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

#[test]
#[serial_test::serial]
fn spectator_departure_copies_only_the_two_published_rosters() {
    let mut profiles = Vec::new();
    for spectators in [0_u128, 16, 254] {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::task::yield_now().await;
            let server = test_helpers::create_test_server().await;
            let host = Uuid::from_u128(1);
            let spectator = Uuid::from_u128(2);
            let (host_sender, mut host_receiver) = tokio::sync::mpsc::channel(16);
            let (spectator_sender, mut spectator_receiver) = tokio::sync::mpsc::channel(16);
            server.connect_client(host, host_sender).await;
            server.connect_client(spectator, spectator_sender).await;
            server
                .handle_join_room(
                    &host,
                    "departure-profile".into(),
                    Some("DEPART".into()),
                    "Host".into(),
                    Some(4),
                    Some(true),
                    None,
                    None,
                    None,
                )
                .await;
            let joined = host_receiver.try_recv().unwrap();
            assert!(matches!(joined.as_ref(), ServerMessage::RoomJoined(_)));
            let lobby = host_receiver.try_recv().unwrap();
            assert!(matches!(
                lobby.as_ref(),
                ServerMessage::LobbyStateChanged { .. }
            ));
            let room_id = server.get_client_room(&host).await.unwrap();
            server
                .database()
                .set_room_max_spectators(&room_id, Some(255))
                .await
                .unwrap();
            for index in 0..spectators {
                assert!(server
                    .database()
                    .add_spectator_to_room(
                        &room_id,
                        SpectatorInfo {
                            id: Uuid::from_u128(index + 3),
                            name: format!("Viewer-{index:023}"),
                            connected_at: Some(chrono::DateTime::from_timestamp(1234, 0).unwrap()),
                        }
                    )
                    .await
                    .unwrap());
            }
            for warm in [true, false] {
                server
                    .handle_join_as_spectator(
                        &spectator,
                        "departure-profile".into(),
                        "DEPART".into(),
                        "Viewer".into(),
                        None,
                    )
                    .await;
                let baseline = spectator_receiver.try_recv().unwrap();
                assert!(matches!(
                    baseline.as_ref(),
                    ServerMessage::SpectatorJoined(_)
                ));
                let joined = host_receiver.try_recv().unwrap();
                assert!(matches!(
                    joined.as_ref(),
                    ServerMessage::NewSpectatorJoined { .. }
                ));
                let region = stats_alloc::Region::new(ALLOCATOR);
                server.handle_leave_spectator(&spectator).await;
                let stats = region.change();
                if !warm {
                    println!("SPECTATOR_DEPARTURE_PROFILE spectators={spectators} stats={stats:?}");
                    profiles.push((spectators, stats));
                }
                let left = spectator_receiver.try_recv().unwrap();
                let ServerMessage::SpectatorLeft {
                    room_id: actual_room,
                    room_code,
                    reason,
                    current_spectators,
                } = left.as_ref()
                else {
                    panic!("expected departure acknowledgement: {left:?}");
                };
                assert_eq!(*actual_room, Some(room_id));
                assert_eq!(room_code.as_deref(), Some("DEPART"));
                assert_eq!(*reason, Some(SpectatorStateChangeReason::VoluntaryLeave));
                assert_eq!(
                    current_spectators.len(),
                    usize::try_from(spectators).unwrap()
                );
                for index in 0..spectators {
                    let entry = current_spectators
                        .iter()
                        .find(|entry| entry.id == Uuid::from_u128(index + 3))
                        .unwrap();
                    assert_eq!(entry.name, format!("Viewer-{index:023}"));
                    assert_eq!(
                        entry.connected_at,
                        Some(chrono::DateTime::from_timestamp(1234, 0).unwrap())
                    );
                }
                let notification = host_receiver.try_recv().unwrap();
                let ServerMessage::SpectatorDisconnected {
                    spectator_id,
                    reason,
                    spectator_count,
                    current_spectators: notified,
                } = notification.as_ref()
                else {
                    panic!("expected departure notification: {notification:?}");
                };
                assert_eq!(*spectator_id, spectator);
                assert_eq!(*reason, Some(SpectatorStateChangeReason::VoluntaryLeave));
                assert_eq!(*spectator_count, Some(u32::try_from(spectators).unwrap()));
                assert_eq!(
                    serde_json::to_value(notified).unwrap(),
                    serde_json::to_value(current_spectators).unwrap()
                );
                let spectator_trailing = spectator_receiver.try_recv();
                assert!(matches!(
                    spectator_trailing,
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                ));
                let host_trailing = host_receiver.try_recv();
                assert!(matches!(
                    host_trailing,
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                ));
                assert_eq!(
                    server
                        .database()
                        .get_room_spectators(&room_id)
                        .await
                        .unwrap()
                        .len(),
                    usize::try_from(spectators).unwrap()
                );
            }
        });
    }
    let baseline = profiles[0].1;
    for (spectators, stats) in &profiles {
        let count = usize::try_from(*spectators).unwrap();
        assert!(
            stats.allocations <= baseline.allocations + 2 * count + 4,
            "only two published roster copies: {profiles:?}"
        );
        assert!(
            stats.bytes_allocated
                <= baseline.bytes_allocated
                    + count * (2 * (std::mem::size_of::<SpectatorInfo>() + 30))
                    + 512,
            "only two published roster buffers and names: {profiles:?}"
        );
    }
}

/// The departure read must not retain unrelated seated-player metadata.
#[test]
#[serial_test::serial]
fn spectator_snapshot_cost_is_independent_of_seated_player_metadata() {
    use signal_fish_server::database::{GameDatabase, InMemoryDatabase};
    use signal_fish_server::protocol::ConnectionInfo;
    for spectators in [0_u128, 16, 254] {
        let mut profiles = Vec::new();
        for players in [1_u128, 16] {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                tokio::task::yield_now().await;
                let database = InMemoryDatabase::new();
                let room = database.create_room("snapshot-profile".into(), Some("SNAP01".into()),
                    16, true, Uuid::from_u128(1), "relay".into(), "region".into(), None).await.unwrap();
                let mut member = room.players[&Uuid::from_u128(1)].clone();
                for index in 1..players {
                    member.id = Uuid::from_u128(index + 1);
                    member.name = format!("Player-{index:023}");
                    member.connection_info = Some(ConnectionInfo::Custom {
                        data: serde_json::json!({"metadata": "x".repeat(64 * 1024)}),
                    });
                    assert!(database.add_player_to_room(&room.id, member.clone()).await.unwrap());
                }
                for index in 0..spectators {
                    assert!(database.add_spectator_to_room(&room.id, SpectatorInfo {
                        id: Uuid::from_u128(index + 17), name: format!("Viewer-{index:023}"), connected_at: None,
                    }).await.unwrap());
                }
                let region = stats_alloc::Region::new(ALLOCATOR);
                let snapshot = database.get_room_spectator_snapshot(&room.id).await.unwrap().unwrap();
                let stats = region.change();
                println!("SPECTATOR_SNAPSHOT_PROFILE spectators={spectators} players={players} stats={stats:?}");
                profiles.push(stats);
                assert_eq!(snapshot.room_code, room.code);
                assert_eq!(snapshot.spectators.len(), usize::try_from(spectators).unwrap());
                for spectator in snapshot.spectators {
                    let index = spectator.id.as_u128() - 17;
                    assert_eq!(spectator.name, format!("Viewer-{index:023}"));
                    assert_eq!(spectator.connected_at, None);
                }
                assert_eq!(database.get_room_players(&room.id).await.unwrap().len(), usize::try_from(players).unwrap());
            });
        }
        assert_eq!(profiles[0].allocations, profiles[1].allocations);
        assert_eq!(profiles[0].bytes_allocated, profiles[1].bytes_allocated);
    }
}
