//! Session replanning reads must not copy spectator metadata.

use signal_fish_server::database::{GameDatabase, InMemoryDatabase};
use signal_fish_server::protocol::SpectatorInfo;
use uuid::Uuid;

#[global_allocator]
static ALLOCATOR: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

/// #636: retain exact members while keeping read cost independent of spectators.
#[test]
#[serial_test::serial]
fn session_snapshot_cost_does_not_grow_with_spectator_rosters() {
    let mut profiles = Vec::new();
    for spectators in [0_u128, 16, 254] {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::task::yield_now().await;
            let database = InMemoryDatabase::new();
            let room = database
                .create_room(
                    "session-profile".into(),
                    Some("SESS01".into()),
                    4,
                    true,
                    Uuid::from_u128(1),
                    "relay".into(),
                    "test".into(),
                    None,
                )
                .await
                .unwrap();
            for index in 0..spectators {
                assert!(database
                    .add_spectator_to_room(
                        &room.id,
                        SpectatorInfo {
                            id: Uuid::from_u128(index + 2),
                            name: format!("Viewer-{index}-{}", "x".repeat(64)),
                            connected_at: None,
                        }
                    )
                    .await
                    .unwrap());
            }
            let region = stats_alloc::Region::new(ALLOCATOR);
            let snapshot = database
                .get_room_session_snapshot(&room.id)
                .await
                .unwrap()
                .unwrap();
            let stats = region.change();
            println!("SESSION_SNAPSHOT_PROFILE spectators={spectators} stats={stats:?}");
            profiles.push(stats);
            assert_eq!(snapshot.authority_player, room.authority_player);
            assert_eq!(snapshot.lobby_state, room.lobby_state);
            assert_eq!(snapshot.players.len(), room.players.len());
            for player in &snapshot.players {
                let expected = &room.players[&player.id];
                assert_eq!(
                    serde_json::to_value(player).unwrap(),
                    serde_json::to_value(expected).unwrap()
                );
                assert_eq!(player.region_id, expected.region_id);
            }
            assert_eq!(
                database.get_room_spectators(&room.id).await.unwrap().len(),
                usize::try_from(spectators).unwrap()
            );
        });
    }
    let baseline = profiles[0];
    for profile in profiles {
        assert_eq!(profile.allocations, baseline.allocations);
        assert_eq!(profile.bytes_allocated, baseline.bytes_allocated);
    }
}

/// #636/#826: a complete host departure retains failover without spectator copies.
#[cfg(feature = "allocation-tracking")]
#[test]
#[serial_test::serial]
fn host_departure_replans_without_copying_spectator_rosters() {
    use signal_fish_server::config::*;
    use signal_fish_server::database::DatabaseConfig;
    use signal_fish_server::protocol::{ServerMessage, Topology, Transport};
    use signal_fish_server::server::allocation_benchmark::negotiate_webrtc_for_measurement;
    use signal_fish_server::server::{EnhancedGameServer, ServerConfig};

    let mut profiles = Vec::new();
    for spectators in [0_u128, 16, 254] {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::task::yield_now().await;
            let server = EnhancedGameServer::new(
                ServerConfig::default(), ProtocolConfig::default(), RelayTypeConfig::default(),
                SessionConfig { default_topology: Topology::Host, enable_webrtc: true, ..Default::default() },
                TurnConfig::default(), DatabaseConfig::InMemory, MetricsConfig::default(),
                CoordinationConfig::default(), TransportSecurityConfig::default(), vec![],
            ).await.unwrap();
            let host = Uuid::from_u128(1);
            let guest = Uuid::from_u128(2);
            let (host_sender, mut host_receiver) = tokio::sync::mpsc::channel(32);
            let (guest_sender, mut guest_receiver) = tokio::sync::mpsc::channel(32);
            server.connect_client(host, host_sender).await;
            server.connect_client(guest, guest_sender).await;
            for (player, name) in [(host, "Host"), (guest, "Guest")] {
                negotiate_webrtc_for_measurement(&server, &player);
                server.handle_join_room(&player, "failover-profile".into(), Some("FAIL01".into()),
                    name.into(), Some(4), Some(true), None, None, None).await;
            }
            server.handle_player_ready(&host).await;
            server.handle_player_ready(&guest).await;
            server.handle_start_game(&host).await;
            let mut initial_generation = None;
            for receiver in [&mut host_receiver, &mut guest_receiver] {
                let mut saw_plan = false;
                loop {
                    match receiver.try_recv() {
                        Ok(message) => match message.as_ref() {
                            ServerMessage::SessionPlan(plan) => {
                                if let Some(generation) = initial_generation {
                                    assert_eq!(plan.generation, generation);
                                } else {
                                    initial_generation = Some(plan.generation);
                                }
                                assert_eq!(plan.host, Some(host));
                                assert_eq!(plan.topology, Topology::Host);
                                assert_eq!(plan.transport, Transport::WebRtc);
                                saw_plan = true;
                            }
                            ServerMessage::RoomJoined(_) | ServerMessage::PlayerJoined { .. }
                            | ServerMessage::LobbyStateChanged { .. } | ServerMessage::GameStarting { .. }
                            | ServerMessage::AuthorityChanged { .. } => {}
                            unexpected => panic!("unexpected setup message: {unexpected:?}"),
                        },
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                        Err(error) => panic!("setup channel closed: {error}"),
                    }
                }
                assert!(saw_plan);
            }
            let room_id = server.get_client_room(&host).await.unwrap();
            server.database().set_room_max_spectators(&room_id, Some(255)).await.unwrap();
            for index in 0..spectators {
                assert!(server.database().add_spectator_to_room(&room_id, SpectatorInfo {
                    id: Uuid::from_u128(index + 3),
                    name: format!("Viewer-{index:023}"), connected_at: None,
                }).await.unwrap());
            }
            let region = stats_alloc::Region::new(ALLOCATOR);
            server.leave_room(&host).await;
            let stats = region.change();
            println!("HOST_DEPARTURE_PROFILE spectators={spectators} stats={stats:?}");
            profiles.push(stats);
            let confirmation = host_receiver.try_recv().unwrap();
            assert!(matches!(confirmation.as_ref(), ServerMessage::RoomLeft));
            let departure = guest_receiver.try_recv().unwrap();
            assert!(matches!(departure.as_ref(), ServerMessage::PlayerLeft { player_id, .. } if *player_id == host));
            let mut saw_plan = false;
            loop {
                match guest_receiver.try_recv() {
                    Ok(message) => match message.as_ref() {
                        ServerMessage::SessionPlan(plan) => {
                            assert_ne!(Some(plan.generation), initial_generation);
                            assert_eq!(plan.host, Some(guest));
                            assert_eq!(plan.topology, Topology::Host);
                            assert_eq!(plan.transport, Transport::WebRtc);
                            assert!(plan.peers.is_empty());
                            assert!(!saw_plan);
                            saw_plan = true;
                        }
                        ServerMessage::AuthorityChanged { .. } => {}
                        unexpected => panic!("unexpected departure message: {unexpected:?}"),
                    },
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(error) => panic!("guest channel closed: {error}"),
                }
            }
            assert!(saw_plan);
            let host_trailing = host_receiver.try_recv();
            assert!(matches!(host_trailing, Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
            assert_eq!(server.get_client_room(&host).await, None);
            assert_eq!(server.get_client_room(&guest).await, Some(room_id));
            let remaining = server.database().get_room_by_id(&room_id).await.unwrap().unwrap();
            assert_eq!(remaining.players.len(), 1);
            assert!(remaining.players.contains_key(&guest));
            assert_eq!(remaining.spectators.len(), usize::try_from(spectators).unwrap());
            for index in 0..spectators {
                assert_eq!(remaining.spectators[&Uuid::from_u128(index + 3)].name, format!("Viewer-{index:023}"));
            }
        });
    }
    let baseline = profiles[0];
    for profile in &profiles {
        assert!(
            profile.allocations <= baseline.allocations + 4,
            "{profiles:?}"
        );
        assert!(
            profile.bytes_allocated <= baseline.bytes_allocated + 512,
            "{profiles:?}"
        );
    }
}
