//! Allocation bounds for live room admission and reconnect protocol reads.
use super::*;
use signal_fish_server::server::allocation_benchmark::{
    membership_version_for_measurement, pre_issue_token_for_measurement,
    sticky_admission_for_measurement,
};

async fn setup() -> std::sync::Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
        ServerConfig {
            enable_reconnection: true,
            ..Default::default()
        },
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        SessionConfig::default(),
        TurnConfig::default(),
        DatabaseConfig::InMemory,
        MetricsConfig::default(),
        CoordinationConfig::default(),
        TransportSecurityConfig::default(),
        vec![],
    )
    .await
    .unwrap()
}

pub(crate) fn membership_version_does_not_copy_capabilities() {
    allocation_harness::block_on(async {
        let server = setup().await;
        let id = Uuid::from_u128(1);
        let missing = Uuid::from_u128(2);
        let (sender, _receiver) = tokio::sync::mpsc::channel(16);
        server.connect_client(id, sender).await;
        let mut profiles = Vec::new();
        for version in [2, 3] {
            if version == 3 {
                negotiate_webrtc_for_measurement(&server, &id);
            }
            for player in [id, missing] {
                let region = stats_alloc::Region::new(ALLOCATOR);
                let actual = membership_version_for_measurement(&server, &player);
                let stats = region.change();
                assert_eq!(actual, if player == missing { 2 } else { version });
                println!(
                    "MEMBERSHIP_VERSION version={version} missing={} stats={stats:?}",
                    player == missing
                );
                profiles.push(stats);
            }
        }
        assert!(
            profiles
                .iter()
                .all(|stats| stats.allocations == 0 && stats.reallocations == 0),
            "membership attribution copied capability lists: {profiles:?}"
        );
    });
}

pub(crate) fn sticky_admission_does_not_copy_capabilities() {
    allocation_harness::block_on(async {
        let server = setup().await;
        let id = Uuid::from_u128(1);
        let missing = Uuid::from_u128(2);
        let (sender, _receiver) = tokio::sync::mpsc::channel(16);
        server.connect_client(id, sender).await;
        let mut profiles = Vec::new();
        for webrtc in [false, true] {
            if webrtc {
                negotiate_webrtc_for_measurement(&server, &id);
            }
            for player in [id, missing] {
                for (topology, transport) in [
                    (Topology::Mesh, Transport::WebRtc),
                    (Topology::Host, Transport::Direct),
                ] {
                    let region = stats_alloc::Region::new(ALLOCATOR);
                    let admitted =
                        sticky_admission_for_measurement(&server, &player, topology, transport);
                    let stats = region.change();
                    assert_eq!(
                        admitted,
                        webrtc && player == id && transport == Transport::WebRtc
                    );
                    println!("STICKY_ADMISSION webrtc={webrtc} missing={} topology={topology:?} transport={transport:?} stats={stats:?}", player == missing);
                    profiles.push(stats);
                }
            }
        }
        assert!(
            profiles
                .iter()
                .all(|stats| stats.allocations == 0 && stats.reallocations == 0),
            "seat-fill admission copied capability lists: {profiles:?}"
        );
    });
}

pub(crate) fn legacy_token_gate_does_not_copy_capabilities() {
    allocation_harness::block_on(async {
        tokio::task::yield_now().await;
        let server = setup().await;
        let id = Uuid::from_u128(1);
        let missing = Uuid::from_u128(2);
        let room = Uuid::from_u128(3);
        let (sender, _receiver) = tokio::sync::mpsc::channel(16);
        server.connect_client(id, sender).await;
        let mut profiles = Vec::new();
        for player in [id, missing] {
            let region = stats_alloc::Region::new(ALLOCATOR);
            let token = pre_issue_token_for_measurement(&server, &player, room).await;
            let stats = region.change();
            assert_eq!(token, None);
            println!(
                "LEGACY_TOKEN_GATE missing={} stats={stats:?}",
                player == missing
            );
            profiles.push(stats);
        }
        negotiate_webrtc_for_measurement(&server, &id);
        let first = pre_issue_token_for_measurement(&server, &id, room)
            .await
            .unwrap();
        let second = pre_issue_token_for_measurement(&server, &id, room)
            .await
            .unwrap();
        assert!(!first.is_empty());
        assert!(!second.is_empty());
        assert_ne!(first, second, "v3 token issuance must still rotate tokens");
        assert!(
            profiles
                .iter()
                .all(|stats| stats.allocations == 0 && stats.reallocations == 0),
            "legacy token gate copied capability lists: {profiles:?}"
        );
    });
}
