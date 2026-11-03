//! Allocation controls for live P2P setup with caller-owned inputs.
use super::*;
use signal_fish_server::protocol::PlayerInfo;
use signal_fish_server::server::allocation_benchmark::{
    pregather_for_measurement, session_members_for_measurement,
};

async fn setup() -> std::sync::Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
        ServerConfig::default(),
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        SessionConfig {
            enable_webrtc: true,
            default_topology: Topology::Mesh,
            ..Default::default()
        },
        TurnConfig {
            enabled: false,
            stun_urls: vec![],
            ..Default::default()
        },
        DatabaseConfig::InMemory,
        MetricsConfig::default(),
        CoordinationConfig::default(),
        TransportSecurityConfig::default(),
        vec![],
    )
    .await
    .unwrap()
}

pub(crate) fn ice_eligibility_does_not_copy_capabilities() {
    allocation_harness::block_on(async {
        let server = setup().await;
        let id = Uuid::from_u128(1);
        let (sender, _receiver) = tokio::sync::mpsc::channel(16);
        server.connect_client(id, sender).await;
        let mut room = server
            .database()
            .create_room(
                "ice-cost".into(),
                Some("ICE001".into()),
                4,
                true,
                id,
                "relay".into(),
                "test".into(),
                None,
            )
            .await
            .unwrap();
        let mut profiles = Vec::new();
        for webrtc in [false, true] {
            if webrtc {
                negotiate_webrtc_for_measurement(&server, &id);
            }
            for state in [
                signal_fish_server::protocol::LobbyState::Waiting,
                signal_fish_server::protocol::LobbyState::Finalized,
            ] {
                room.lobby_state = state;
                let region = stats_alloc::Region::new(ALLOCATOR);
                let ice = pregather_for_measurement(&server, &room, &id);
                let stats = region.change();
                println!(
                    "ICE_ELIGIBILITY webrtc={webrtc} state={:?} stats={stats:?}",
                    room.lobby_state
                );
                assert!(ice.is_empty());
                profiles.push(stats);
            }
        }
        assert!(
            profiles.iter().all(|stats| stats.allocations == 0),
            "ICE eligibility copied capabilities: {profiles:?}"
        );
    });
}

pub(crate) fn session_selection_keeps_capabilities_inline() {
    allocation_harness::block_on(async {
        let server = setup().await;
        let mut profiles = Vec::new();
        for members in [0, 2, 16, 255] {
            let mut players = Vec::new();
            let mut receivers = Vec::new();
            for index in 1..=members {
                let id = Uuid::from_u128(index);
                let (sender, receiver) = tokio::sync::mpsc::channel(8);
                receivers.push(receiver);
                server.connect_client(id, sender).await;
                negotiate_webrtc_for_measurement(&server, &id);
                players.push(PlayerInfo {
                    id,
                    name: "P".into(),
                    is_authority: false,
                    is_ready: false,
                    connected_at: Some(chrono::DateTime::UNIX_EPOCH),
                    connection_info: None,
                    epoch: None,
                    seq: None,
                    region_id: String::new(),
                });
            }
            let region = stats_alloc::Region::new(ALLOCATOR);
            let count = session_members_for_measurement(&server, &players);
            let stats = region.change();
            println!("SESSION_CAPABILITIES members={members} stats={stats:?}");
            assert_eq!(count, players.len());
            profiles.push((players.len(), stats));
            for player in players {
                server.disconnect_client(&player.id).await;
            }
        }
        for (members, stats) in &profiles {
            assert_eq!(
                stats.allocations,
                if *members == 0 { 0 } else { members + 1 },
                "selection allocated per-member capability lists: {profiles:?}"
            );
        }
    });
}

pub(crate) fn ice_composition_reserves_combined_list() {
    use signal_fish_server::protocol::IceServer;
    for static_count in [0, 1, 4, 16] {
        allocation_harness::block_on(async {
            let static_servers: Vec<_> = (0..static_count)
                .map(|index| IceServer {
                    urls: vec![format!("stun:static-{index}.example:3478")],
                    username: None,
                    credential: None,
                })
                .collect();
            let server = EnhancedGameServer::new(
                ServerConfig::default(),
                ProtocolConfig::default(),
                RelayTypeConfig::default(),
                SessionConfig {
                    enable_webrtc: true,
                    default_topology: Topology::Mesh,
                    ice_servers: static_servers.clone(),
                    ..Default::default()
                },
                TurnConfig {
                    enabled: false,
                    stun_urls: vec!["stun:derived.example:3478".into()],
                    ..Default::default()
                },
                DatabaseConfig::InMemory,
                MetricsConfig::default(),
                CoordinationConfig::default(),
                TransportSecurityConfig::default(),
                vec![],
            )
            .await
            .unwrap();
            let id = Uuid::from_u128(1);
            let (sender, _receiver) = tokio::sync::mpsc::channel(8);
            server.connect_client(id, sender).await;
            negotiate_webrtc_for_measurement(&server, &id);
            let room = server
                .database()
                .create_room(
                    "ice-capacity".into(),
                    None,
                    2,
                    false,
                    id,
                    "relay".into(),
                    "test".into(),
                    None,
                )
                .await
                .unwrap();
            let region = stats_alloc::Region::new(ALLOCATOR);
            let ice = pregather_for_measurement(&server, &room, &id);
            let stats = region.change();
            println!("ICE_COMPOSITION static_count={static_count} stats={stats:?}");
            assert_eq!(
                serde_json::to_value(&ice[..static_count]).unwrap(),
                serde_json::to_value(&static_servers).unwrap()
            );
            assert_eq!(ice.len(), static_count + 1);
            assert_eq!(ice[static_count].urls, ["stun:derived.example:3478"]);
            assert!(ice
                .iter()
                .all(|entry| entry.username.is_none() && entry.credential.is_none()));
            assert_eq!(
                stats.reallocations, 0,
                "ICE list grew after copying static entries: {stats:?}"
            );
        });
    }
}
