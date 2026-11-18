//! Relay decisions must not retain names that only P2P peer lists consume.
use super::*;
use signal_fish_server::coordination::FinalizedRoom;
use signal_fish_server::protocol::{PeerConnectionInfo, PlayerInfo};
use signal_fish_server::server::allocation_benchmark::{
    negotiate_direct_for_measurement, start_game_builder_for_measurement,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// #636: preserve exact publications while excluding unused relay selection names.
pub(crate) fn relay_start_skips_player_names() {
    let mut profiles = Vec::new();
    for count in [2_u128, 16, 255] {
        for case in [
            "v2",
            "ceiling",
            "disabled",
            "missing-endpoint",
            "mixed",
            "host",
            "mesh",
            "direct",
        ] {
            allocation_harness::block_on(async {
                let desired = if case == "ceiling" {
                    Topology::Relay
                } else if case == "mesh" {
                    Topology::Mesh
                } else {
                    Topology::Host
                };
                let server = EnhancedGameServer::new(
                    ServerConfig {
                        max_connections_per_ip: 512,
                        ..Default::default()
                    },
                    ProtocolConfig::default(),
                    RelayTypeConfig::default(),
                    SessionConfig {
                        default_topology: desired,
                        enable_webrtc: case != "disabled",
                        enable_direct: true,
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
                .unwrap();
                let mut receivers = Vec::new();
                let mut players = Vec::new();
                let authority = Uuid::from_u128(1);
                for index in 1..=count {
                    let id = Uuid::from_u128(index);
                    let (sender, receiver) = tokio::sync::mpsc::channel(4);
                    receivers.push(receiver);
                    server.connect_client(id, sender).await;
                    if case == "missing-endpoint"
                        || case == "direct"
                        || (case == "mixed" && index == 1)
                    {
                        negotiate_direct_for_measurement(&server, &id);
                    } else if case != "v2" {
                        negotiate_webrtc_for_measurement(&server, &id);
                    }
                    players.push(PlayerInfo {
                        id,
                        name: String::new(),
                        is_authority: index == 1,
                        is_ready: true,
                        connected_at: Some(jiff::Timestamp::UNIX_EPOCH),
                        connection_info: (case == "direct").then(|| ConnectionInfo::Direct {
                            host: "192.0.2.1".into(),
                            port: 7777,
                        }),
                        epoch: Some(1),
                        seq: Some(0),
                        region_id: "test".into(),
                    });
                }
                players.rotate_left(1);
                let p2p = matches!(case, "host" | "mesh" | "direct");
                let mut measurements = Vec::new();
                for suffix_bytes in [1, 29] {
                    for player in &mut players {
                        player.name =
                            format!("{:03}{}", player.id.as_u128(), "N".repeat(suffix_bytes));
                    }
                    let name_bytes = suffix_bytes + 3;
                    let finalized = FinalizedRoom {
                        game_name: "name-cost".into(),
                        authority_player: Some(authority),
                        members: players.clone(),
                    };
                    let starting = Arc::new(ServerMessage::GameStarting {
                        peer_connections: PeerConnectionInfo::from_players(
                            &finalized.members,
                            "relay",
                        ),
                    });
                    let original = serde_json::to_value(starting.as_ref()).unwrap();
                    let builder =
                        start_game_builder_for_measurement(&server, Uuid::from_u128(1000));
                    let region = stats_alloc::Region::new(ALLOCATOR);
                    let publication = builder(&finalized, Arc::clone(&starting));
                    let stats = region.change();
                    println!("START_NAME_COST case={case} count={count} name_bytes={name_bytes} stats={stats:?}");
                    measurements.push(stats);
                    assert_eq!(publication.recipient_messages.len(), players.len());
                    let mut generation = None;
                    for (batch, player) in publication.recipient_messages.iter().zip(&players) {
                        assert_eq!(batch.player_id, player.id);
                        assert_eq!(batch.first_phase, 0);
                        assert_eq!(batch.messages.len(), if case == "v2" { 1 } else { 2 });
                        assert!(Arc::ptr_eq(&batch.messages[0], &starting));
                        assert_eq!(
                            serde_json::to_value(batch.messages[0].as_ref()).unwrap(),
                            original
                        );
                        if case == "v2" {
                            continue;
                        }
                        let ServerMessage::SessionPlan(plan) = batch.messages[1].as_ref() else {
                            panic!("expected plan");
                        };
                        let expected_topology = if p2p { desired } else { Topology::Relay };
                        assert_eq!(plan.topology, expected_topology);
                        assert_eq!(
                            plan.transport,
                            if case == "direct" {
                                Transport::Direct
                            } else if p2p {
                                Transport::WebRtc
                            } else {
                                Transport::Relay
                            }
                        );
                        assert_eq!(
                            plan.host,
                            (expected_topology == Topology::Host).then_some(authority)
                        );
                        assert_eq!(plan.fallback, Transport::Relay);
                        assert!(plan.ice_servers.is_empty());
                        assert_eq!(
                            plan.direct_endpoint
                                .as_ref()
                                .map(|endpoint| (endpoint.host.as_str(), endpoint.port)),
                            (case == "direct").then_some(("192.0.2.1", 7777))
                        );
                        if let Some(expected) = generation {
                            assert_eq!(plan.generation, expected);
                        }
                        generation = Some(plan.generation);
                        let peers: Vec<_> = players
                            .iter()
                            .filter(|peer| {
                                peer.id != player.id
                                    && p2p
                                    && (case == "mesh"
                                        || player.id == authority
                                        || peer.id == authority)
                            })
                            .collect();
                        assert_eq!(plan.peers.len(), peers.len());
                        for (actual, expected) in plan.peers.iter().zip(peers) {
                            assert_eq!(actual.player_id, expected.id);
                            assert_eq!(actual.player_name, expected.name);
                            assert_eq!(actual.is_authority, expected.is_authority);
                            assert_eq!(
                                actual.initiate,
                                if case == "mesh" {
                                    player.id < expected.id
                                } else {
                                    player.id != authority
                                }
                            );
                        }
                    }
                    let metrics = server.metrics();
                    let emitted_before = metrics.session_plans_emitted.load(Ordering::Relaxed);
                    (publication.after_game_starting)();
                    assert_eq!(
                        metrics.session_plans_emitted.load(Ordering::Relaxed),
                        emitted_before + u64::from(case != "v2")
                    );
                    for receiver in &mut receivers {
                        assert_membership_queue_empty(receiver);
                    }
                }
                profiles.push((count, case, measurements));
            });
        }
    }
    for (count, case, measurements) in &profiles {
        let members = usize::try_from(*count).unwrap();
        let copies = match *case {
            "mesh" => members * members,
            "host" | "direct" => members + 2 * (members - 1),
            _ => 0,
        };
        assert_eq!(
            measurements[1].allocations, measurements[0].allocations,
            "case={case} count={count} measurements={measurements:?}"
        );
        assert_eq!(
            measurements[1].bytes_allocated,
            measurements[0].bytes_allocated + copies * 28,
            "unused relay selection names: case={case} count={count} measurements={measurements:?}"
        );
    }
}
