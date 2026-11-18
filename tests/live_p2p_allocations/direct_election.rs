//! Caller correspondence for Direct host election and endpoint projection.
use super::*;
use signal_fish_server::coordination::FinalizedRoom;
use signal_fish_server::protocol::{DirectEndpoint, PeerConnectionInfo, PlayerInfo};
use signal_fish_server::server::allocation_benchmark::{
    negotiate_direct_for_measurement, negotiate_p2p_for_measurement,
    start_game_builder_for_measurement,
};
use std::sync::Arc;

fn expected_host(
    players: &[PlayerInfo],
    authority: Option<Uuid>,
    eligible: impl Fn(&PlayerInfo) -> bool,
) -> Option<&PlayerInfo> {
    let mut candidates: Vec<_> = players.iter().filter(|player| eligible(player)).collect();
    candidates.sort_by_key(|player| {
        (
            player.connected_at.unwrap_or(jiff::Timestamp::MAX),
            player.id,
        )
    });
    authority
        .and_then(|id| candidates.iter().copied().find(|player| player.id == id))
        .or_else(|| candidates.first().copied())
}

/// #1464: the publication adapter must preserve independently ordered host identity.
pub(crate) fn direct_start_matches_borrowed_election_oracle() {
    for capability_case in ["direct", "mixed", "dual"] {
        for desired in [Topology::Relay, Topology::Host, Topology::Mesh] {
            allocation_harness::block_on(async {
                let server = EnhancedGameServer::new(
                    ServerConfig::default(),
                    ProtocolConfig::default(),
                    RelayTypeConfig::default(),
                    SessionConfig {
                        default_topology: desired,
                        enable_webrtc: true,
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
                let identities = [10, 20, 30, 40].map(Uuid::from_u128);
                let mut receivers = Vec::new();
                for (index, id) in identities.iter().enumerate() {
                    let (sender, receiver) = tokio::sync::mpsc::channel(4);
                    server.connect_client(*id, sender).await;
                    receivers.push(receiver);
                    match capability_case {
                        "dual" => negotiate_p2p_for_measurement(&server, id, true),
                        "mixed" if index == 0 => negotiate_webrtc_for_measurement(&server, id),
                        _ => negotiate_direct_for_measurement(&server, id),
                    }
                }
                for clock_case in 0..4 {
                    for valid_mask in 0u8..16 {
                        let original: Vec<_> = identities
                            .iter()
                            .enumerate()
                            .map(|(index, id)| {
                                let valid = valid_mask & (1 << index) != 0;
                                let connected_at = match clock_case {
                                    0 => None,
                                    1 => Some(jiff::Timestamp::UNIX_EPOCH),
                                    2 if index == 0 => None,
                                    2 => Some(
                                        jiff::Timestamp::new(
                                            i64::try_from(4 - index).unwrap(),
                                            0,
                                        )
                                        .unwrap(),
                                    ),
                                    _ if index == 0 => Some(jiff::Timestamp::MIN),
                                    _ if index == 1 => Some(jiff::Timestamp::MAX),
                                    _ => Some(jiff::Timestamp::UNIX_EPOCH),
                                };
                                let connection_info = if valid {
                                    Some(ConnectionInfo::Direct {
                                        host: format!("player-{index}.example.test"),
                                        port: 7000 + u16::try_from(index).unwrap(),
                                    })
                                } else {
                                    match index {
                                        0 => None,
                                        1 => Some(ConnectionInfo::Direct {
                                            host: "0.0.0.0".into(),
                                            port: 7001,
                                        }),
                                        2 => Some(ConnectionInfo::Direct {
                                            host: "valid.example.test".into(),
                                            port: 0,
                                        }),
                                        _ => Some(ConnectionInfo::Custom {
                                            data: serde_json::json!({"host": "custom.example.test"}),
                                        }),
                                    }
                                };
                                PlayerInfo {
                                    id: *id,
                                    name: format!("Player-{index}"),
                                    is_authority: index % 2 == 0,
                                    is_ready: true,
                                    connected_at,
                                    connection_info,
                                    epoch: Some(1),
                                    seq: Some(0),
                                    region_id: "test".into(),
                                }
                            })
                            .collect();
                        for authority in [None, Some(Uuid::max())]
                            .into_iter()
                            .chain(identities.map(Some))
                        {
                            for permutation in 0..8 {
                                let mut players = original.clone();
                                players.rotate_left(permutation % 4);
                                if permutation >= 4 {
                                    players.reverse();
                                }
                                let transport = match (desired, capability_case, valid_mask) {
                                    (Topology::Relay, _, _) | (_, "mixed", _) => Transport::Relay,
                                    (_, "dual", _) => Transport::WebRtc,
                                    (_, _, 0) => Transport::Relay,
                                    _ => Transport::Direct,
                                };
                                let topology = match transport {
                                    Transport::Relay => Topology::Relay,
                                    Transport::WebRtc if desired == Topology::Mesh => {
                                        Topology::Mesh
                                    }
                                    _ => Topology::Host,
                                };
                                let host = (topology == Topology::Host).then(|| {
                                    expected_host(&players, authority, |player| {
                                        transport != Transport::Direct
                                            || identities
                                                .iter()
                                                .position(|id| *id == player.id)
                                                .is_some_and(|index| valid_mask & (1 << index) != 0)
                                    })
                                    .unwrap()
                                });
                                let endpoint = if transport == Transport::Direct {
                                    let Some(ConnectionInfo::Direct { host, port }) =
                                        &host.unwrap().connection_info
                                    else {
                                        panic!("oracle selected invalid endpoint")
                                    };
                                    Some(DirectEndpoint {
                                        host: host.clone(),
                                        port: *port,
                                    })
                                } else {
                                    None
                                };
                                let expected_host_id = host.map(|player| player.id);
                                let finalized = FinalizedRoom {
                                    game_name: "direct-election".into(),
                                    authority_player: authority,
                                    members: players,
                                };
                                let starting = Arc::new(ServerMessage::GameStarting {
                                    peer_connections: PeerConnectionInfo::from_players(
                                        &finalized.members,
                                        "relay",
                                    ),
                                });
                                let publication = start_game_builder_for_measurement(
                                    &server,
                                    Uuid::from_u128(1000),
                                )(
                                    &finalized, starting.clone()
                                );
                                assert_eq!(publication.recipient_messages.len(), 4);
                                let mut generation = None;
                                for (batch, player) in publication
                                    .recipient_messages
                                    .iter()
                                    .zip(&finalized.members)
                                {
                                    assert_eq!(batch.player_id, player.id);
                                    assert_eq!(batch.first_phase, 0);
                                    assert_eq!(batch.messages.len(), 2);
                                    assert!(Arc::ptr_eq(&batch.messages[0], &starting));
                                    let ServerMessage::SessionPlan(plan) =
                                        batch.messages[1].as_ref()
                                    else {
                                        panic!("missing SessionPlan")
                                    };
                                    assert_eq!((plan.topology, plan.transport, plan.host), (topology, transport, expected_host_id), "caps={capability_case}, desired={desired:?}, mask={valid_mask}, clock={clock_case}, authority={authority:?}, permutation={permutation}");
                                    assert_eq!(plan.direct_endpoint, endpoint);
                                    assert_eq!(plan.fallback, Transport::Relay);
                                    assert!(plan.ice_servers.is_empty());
                                    if let Some(expected) = generation {
                                        assert_eq!(plan.generation, expected);
                                    }
                                    generation = Some(plan.generation);
                                    let peers: Vec<_> = finalized
                                        .members
                                        .iter()
                                        .filter(|peer| {
                                            peer.id != player.id
                                                && (topology == Topology::Mesh
                                                    || topology == Topology::Host
                                                        && (Some(player.id) == expected_host_id
                                                            || Some(peer.id) == expected_host_id))
                                        })
                                        .collect();
                                    assert_eq!(plan.peers.len(), peers.len());
                                    for (actual, expected) in plan.peers.iter().zip(peers) {
                                        assert_eq!(actual.player_id, expected.id);
                                        assert_eq!(actual.player_name, expected.name);
                                        assert_eq!(
                                            actual.is_authority,
                                            if topology == Topology::Host {
                                                Some(expected.id) == expected_host_id
                                            } else {
                                                expected.is_authority
                                            }
                                        );
                                        assert_eq!(
                                            actual.initiate,
                                            if topology == Topology::Mesh {
                                                player.id < expected.id
                                            } else {
                                                Some(player.id) != expected_host_id
                                            }
                                        );
                                    }
                                }
                                (publication.after_game_starting)();
                            }
                        }
                    }
                }
                for receiver in &mut receivers {
                    assert_membership_queue_empty(receiver);
                }
            });
        }
    }
}
