//! Mesh departure retains the live plan without projecting survivor details.
use super::*;
use signal_fish_server::server::allocation_benchmark::{
    prepare_departure_for_measurement, session_departure_for_measurement,
};

/// #1418: room size must not change a mesh departure policy read cost.
pub(crate) fn mesh_departure_skips_survivor_selection() {
    let mut profiles = Vec::new();
    for count in [2_u128, 16, 64] {
        allocation_harness::block_on(async {
            allocation_harness::warm_current_thread_runtime().await;
            let server = EnhancedGameServer::new(
                ServerConfig::default(),
                ProtocolConfig::default(),
                RelayTypeConfig::default(),
                SessionConfig {
                    default_topology: Topology::Mesh,
                    enable_webrtc: true,
                    ..Default::default()
                },
                TurnConfig::default(),
                DatabaseConfig::InMemory,
                MetricsConfig::default(),
                CoordinationConfig::default(),
                TransportSecurityConfig::default(),
                vec![],
            )
            .await
            .unwrap();
            let mut receivers = Vec::new();
            for index in 1..=count {
                let id = Uuid::from_u128(index);
                let (sender, receiver) = tokio::sync::mpsc::channel(512);
                receivers.push(receiver);
                server.connect_client(id, sender).await;
                negotiate_webrtc_for_measurement(&server, &id);
                server
                    .handle_join_room(
                        &id,
                        "departure-cost".into(),
                        Some("LEAVE1".into()),
                        format!("Player{index}"),
                        Some(64),
                        Some(true),
                        None,
                        None,
                        None,
                    )
                    .await;
                server.handle_player_ready(&id).await;
            }
            let owner = Uuid::from_u128(1);
            let room = server.get_client_room(&owner).await.unwrap();
            for receiver in &mut receivers {
                drain_membership_setup(receiver, false);
            }
            server.handle_start_game(&owner).await;
            let mut generation = None;
            for receiver in &mut receivers {
                let starting = receiver.try_recv().unwrap();
                assert!(matches!(
                    starting.as_ref(),
                    ServerMessage::GameStarting { .. }
                ));
                let ServerMessage::SessionPlan(plan) =
                    receiver.try_recv().unwrap().as_ref().clone()
                else {
                    panic!("missing plan");
                };
                assert_eq!(plan.topology, Topology::Mesh);
                assert_eq!(plan.peers.len(), usize::try_from(count - 1).unwrap());
                if let Some(expected) = generation {
                    assert_eq!(plan.generation, expected);
                }
                generation = Some(plan.generation);
                assert_membership_queue_empty(receiver);
            }
            let departed = Uuid::from_u128(count);
            prepare_departure_for_measurement(&server, &room, &departed)
                .await
                .unwrap();
            for measured in [false, true] {
                let region = stats_alloc::Region::new(ALLOCATOR);
                let unchanged = session_departure_for_measurement(&server, &room, &departed).await;
                let stats = region.change();
                assert!(unchanged);
                if measured {
                    println!("MESH_DEPARTURE members={count} stats={stats:?}");
                    profiles.push(stats);
                }
                for receiver in &mut receivers {
                    assert_membership_queue_empty(receiver);
                }
            }
            assert_eq!(
                server
                    .database()
                    .get_room_player_ids(&room)
                    .await
                    .unwrap()
                    .len(),
                usize::try_from(count - 1).unwrap()
            );
        });
    }
    assert!(
        profiles
            .iter()
            .all(|stats| stats.bytes_allocated <= profiles[0].bytes_allocated + 128),
        "mesh departure copied survivor selection fields: {profiles:?}"
    );
}
