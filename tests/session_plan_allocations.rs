//! Exact P2P start publications retain legacy metadata without copying it into selection.

mod allocation_harness;

use signal_fish_server::config::*;
use signal_fish_server::database::DatabaseConfig;
use signal_fish_server::protocol::{ConnectionInfo, ServerMessage, Topology, Transport};
use signal_fish_server::server::allocation_benchmark::negotiate_webrtc_for_measurement;
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use uuid::Uuid;

#[global_allocator]
static ALLOCATOR: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

/// #636/#826: exactly two legacy payload copies suffice for a complete mesh start.
fn session_plans_do_not_copy_custom_connection_metadata() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut profiles = Vec::new();
        for bytes in [0, 1024, 64 * 1024] {
            let server = EnhancedGameServer::new(
                ServerConfig::default(), ProtocolConfig::default(), RelayTypeConfig::default(),
                SessionConfig { default_topology: Topology::Mesh, enable_webrtc: true, ..Default::default() },
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
                server.handle_join_room(&player, "mesh-profile".into(), Some("MESH01".into()),
                    name.into(), Some(4), Some(true), None, None, None).await;
                server.handle_player_ready(&player).await;
            }
            let room_id = server.get_client_room(&host).await.unwrap();
            let metadata = serde_json::json!({"nested": ["x".repeat(bytes)]});
            for player in [host, guest] {
                assert!(server.database().update_player_connection_info(&room_id, &player,
                    ConnectionInfo::Custom { data: metadata.clone() }).await.unwrap());
            }
            for receiver in [&mut host_receiver, &mut guest_receiver] {
                loop {
                    match receiver.try_recv() {
                        Ok(message) => assert!(matches!(message.as_ref(),
                            ServerMessage::RoomJoined(_) | ServerMessage::PlayerJoined { .. }
                            | ServerMessage::LobbyStateChanged { .. } | ServerMessage::AuthorityChanged { .. }),
                            "unexpected setup message: {message:?}"),
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                        Err(error) => panic!("setup channel closed: {error}"),
                    }
                }
            }
            let region = stats_alloc::Region::new(ALLOCATOR);
            server.handle_start_game(&host).await;
            let stats = region.change();
            println!("SESSION_PLAN_PROFILE bytes={bytes} stats={stats:?}");
            profiles.push(stats);
            let mut generation = None;
            for (player, peer, receiver) in [(host, guest, &mut host_receiver), (guest, host, &mut guest_receiver)] {
                let starting = receiver.try_recv().unwrap();
                let ServerMessage::GameStarting { peer_connections } = starting.as_ref() else { panic!("{starting:?}"); };
                assert_eq!(peer_connections.len(), 2);
                for entry in peer_connections {
                    assert!(matches!(&entry.connection_info, Some(ConnectionInfo::Custom { data }) if *data == metadata));
                }
                let plan = receiver.try_recv().unwrap();
                let ServerMessage::SessionPlan(plan) = plan.as_ref() else { panic!("{plan:?}"); };
                assert_eq!(plan.topology, Topology::Mesh);
                assert_eq!(plan.transport, Transport::WebRtc);
                assert_eq!(plan.host, None);
                assert_eq!(plan.peers.len(), 1);
                assert_eq!(plan.peers[0].player_id, peer);
                assert_eq!(plan.peers[0].initiate, player < peer);
                assert_eq!(plan.direct_endpoint, None);
                if let Some(expected) = generation { assert_eq!(plan.generation, expected); }
                generation = Some(plan.generation);
                let trailing = receiver.try_recv();
                assert!(matches!(trailing, Err(tokio::sync::mpsc::error::TryRecvError::Empty)),
                    "unexpected trailing message: {trailing:?}");
            }
            let stored = server.database().get_room_by_id(&room_id).await.unwrap().unwrap();
            for player in [host, guest] {
                assert!(matches!(&stored.players[&player].connection_info, Some(ConnectionInfo::Custom { data }) if *data == metadata));
            }
        }
        let baseline = profiles[0].bytes_allocated;
        for (bytes, profile) in [0, 1024, 64 * 1024].into_iter().zip(profiles) {
            assert!(profile.bytes_allocated <= baseline + 4 * bytes + 512,
                "session selection copied legacy metadata: bytes={bytes} baseline={baseline} stats={profile:?}");
        }
    });
}

/// Run the measurement on the same thread as harness bookkeeping.
fn main() {
    allocation_harness::run(
        "session_plans_do_not_copy_custom_connection_metadata",
        session_plans_do_not_copy_custom_connection_metadata,
    );
}
