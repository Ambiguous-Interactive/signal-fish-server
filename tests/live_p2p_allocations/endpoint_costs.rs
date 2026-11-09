//! Game-start selection must copy only endpoints that can affect Direct selection.
use super::*;
use signal_fish_server::coordination::FinalizedRoom;
use signal_fish_server::protocol::{DirectEndpoint, PeerConnectionInfo, PlayerInfo};
use signal_fish_server::server::allocation_benchmark::{
    negotiate_direct_for_measurement, start_game_builder_for_measurement,
};
use std::sync::Arc;

/// Unavailable Direct metadata cannot add work to P2P publication preparation.
pub(crate) fn disabled_direct_start_skips_endpoints() {
    let mut disabled_profiles = Vec::new();
    for members in [2_u128, 16, 255] {
        for (enable_direct, direct_members) in [(false, 0), (true, 0), (true, 1), (true, members)] {
            let direct = direct_members == members;
            let mixed = direct_members > 0 && !direct;
            allocation_harness::block_on(async {
                let server = EnhancedGameServer::new(
                    ServerConfig {
                        max_connections_per_ip: 512,
                        ..Default::default()
                    },
                    ProtocolConfig::default(),
                    RelayTypeConfig::default(),
                    SessionConfig {
                        default_topology: Topology::Host,
                        enable_webrtc: !direct,
                        enable_direct,
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
                for index in 1..=members {
                    let id = Uuid::from_u128(index);
                    let (sender, receiver) = tokio::sync::mpsc::channel(4);
                    receivers.push(receiver);
                    server.connect_client(id, sender).await;
                    if index <= direct_members {
                        negotiate_direct_for_measurement(&server, &id);
                    } else {
                        negotiate_webrtc_for_measurement(&server, &id);
                    }
                    players.push(PlayerInfo {
                        id,
                        name: format!("Player-{index}"),
                        is_authority: index == 1,
                        is_ready: true,
                        connected_at: Some(chrono::DateTime::UNIX_EPOCH),
                        connection_info: None,
                        epoch: Some(1),
                        seq: Some(0),
                        region_id: "test".into(),
                    });
                }
                let mut profiles = Vec::new();
                // None and unusable metadata are controls for the no-endpoint path.
                let long_host = [
                    "a".repeat(63),
                    "b".repeat(63),
                    "c".repeat(63),
                    "d".repeat(61),
                ]
                .join(".");
                for host in [None, Some("0.0.0.0"), Some(long_host.as_str())] {
                    for player in &mut players {
                        player.connection_info = host.map(|host| ConnectionInfo::Direct {
                            host: host.into(),
                            port: 7777,
                        });
                    }
                    let finalized = FinalizedRoom {
                        game_name: "endpoint-cost".into(),
                        authority_player: Some(players[0].id),
                        members: players.clone(),
                    };
                    let starting = Arc::new(ServerMessage::GameStarting {
                        peer_connections: PeerConnectionInfo::from_players(
                            &finalized.members,
                            "relay",
                        ),
                    });
                    let builder =
                        start_game_builder_for_measurement(&server, Uuid::from_u128(1000));
                    let region = stats_alloc::Region::new(ALLOCATOR);
                    let publication = builder(&finalized, starting.clone());
                    let stats = region.change();
                    println!("START_ENDPOINT_COST members={members} enable_direct={enable_direct} direct_members={direct_members} host_len={:?} stats={stats:?}", host.map(str::len));
                    profiles.push(stats);
                    let valid_direct = direct && host == Some(long_host.as_str());
                    let expected_transport = if valid_direct {
                        Transport::Direct
                    } else if direct || mixed {
                        Transport::Relay
                    } else {
                        Transport::WebRtc
                    };
                    let expected_topology = if (direct || mixed) && !valid_direct {
                        Topology::Relay
                    } else {
                        Topology::Host
                    };
                    assert_eq!(publication.recipient_messages.len(), players.len());
                    let mut generation = None;
                    for (batch, player) in publication.recipient_messages.iter().zip(&players) {
                        assert_eq!(batch.player_id, player.id);
                        assert_eq!(batch.first_phase, 0);
                        assert_eq!(batch.messages.len(), 2);
                        assert!(Arc::ptr_eq(&batch.messages[0], &starting));
                        let ServerMessage::SessionPlan(plan) = batch.messages[1].as_ref() else {
                            panic!("expected plan");
                        };
                        assert_eq!(plan.transport, expected_transport);
                        assert_eq!(plan.topology, expected_topology);
                        assert_eq!(
                            plan.host,
                            (expected_topology == Topology::Host).then_some(players[0].id)
                        );
                        assert_eq!(
                            plan.direct_endpoint,
                            valid_direct.then(|| DirectEndpoint {
                                host: long_host.clone(),
                                port: 7777
                            })
                        );
                        assert_eq!(plan.fallback, Transport::Relay);
                        assert!(plan.ice_servers.is_empty());
                        if let Some(expected) = generation {
                            assert_eq!(plan.generation, expected);
                        }
                        generation = Some(plan.generation);
                        let expected_peers: Vec<_> = players
                            .iter()
                            .filter(|peer| {
                                expected_topology == Topology::Host
                                    && peer.id != player.id
                                    && (player.id == players[0].id || peer.id == players[0].id)
                            })
                            .collect();
                        assert_eq!(plan.peers.len(), expected_peers.len());
                        for (peer, expected) in plan.peers.iter().zip(expected_peers) {
                            assert_eq!(peer.player_id, expected.id);
                            assert_eq!(peer.player_name, expected.name);
                            assert_eq!(peer.is_authority, player.id != players[0].id);
                            assert_eq!(peer.initiate, player.id != players[0].id);
                        }
                    }
                    (publication.after_game_starting)();
                    for receiver in &mut receivers {
                        assert_membership_queue_empty(receiver);
                    }
                }
                if direct_members == 0 {
                    disabled_profiles.push((members, enable_direct, profiles));
                }
            });
        }
    }
    for (members, enable_direct, profiles) in &disabled_profiles {
        assert_eq!(profiles[2].allocations, profiles[0].allocations,
            "unavailable Direct copied one endpoint per member: members={members} enable_direct={enable_direct} profiles={disabled_profiles:?}");
        assert_eq!(
            profiles[2].bytes_allocated, profiles[0].bytes_allocated,
            "unavailable Direct copied hostnames: members={members} enable_direct={enable_direct} profiles={disabled_profiles:?}"
        );
        assert_eq!(profiles[1].allocations, profiles[0].allocations);
    }
}

/// WebRTC refreshes must preserve player fields without copying Direct hosts.
pub(crate) fn webrtc_refresh_skips_direct_endpoints() {
    use signal_fish_server::database::{GameDatabase, InMemoryDatabase};
    for count in [2_u128, 16, 255] {
        allocation_harness::block_on(async {
            allocation_harness::warm_current_thread_runtime().await;
            let database = InMemoryDatabase::new();
            let actor = Uuid::from_u128(1);
            let room = database
                .create_room(
                    "refresh".into(),
                    None,
                    255,
                    true,
                    actor,
                    "relay".into(),
                    "test".into(),
                    None,
                )
                .await
                .unwrap();
            let host = [
                "a".repeat(63),
                "b".repeat(63),
                "c".repeat(63),
                "d".repeat(61),
            ]
            .join(".");
            for index in 1..=count {
                let id = Uuid::from_u128(index);
                if index > 1 {
                    assert!(database
                        .add_player_to_room(
                            &room.id,
                            PlayerInfo {
                                id,
                                name: format!("Player{index}"),
                                is_authority: false,
                                is_ready: false,
                                connected_at: Some(chrono::DateTime::UNIX_EPOCH),
                                connection_info: None,
                                epoch: Some(1),
                                seq: Some(0),
                                region_id: "test".into(),
                            }
                        )
                        .await
                        .unwrap());
                }
                assert!(database
                    .update_player_connection_info(
                        &room.id,
                        &id,
                        ConnectionInfo::Direct {
                            host: host.clone(),
                            port: 7777
                        }
                    )
                    .await
                    .unwrap());
            }
            tokio::task::yield_now().await;
            let baseline_region = stats_alloc::Region::new(ALLOCATOR);
            let baseline = database
                .get_room_session_selection_snapshot(&room.id)
                .await
                .unwrap()
                .unwrap();
            let baseline_stats = baseline_region.change();
            tokio::task::yield_now().await;
            let region = stats_alloc::Region::new(ALLOCATOR);
            let snapshot = database
                .get_room_session_selection_snapshot_for_transport(&room.id, false)
                .await
                .unwrap()
                .unwrap();
            let stats = region.change();
            println!(
                "WEBRTC_REFRESH_COST count={count} baseline={baseline_stats:?} stats={stats:?}"
            );
            assert_eq!(snapshot.authority_player, baseline.authority_player);
            assert_eq!(snapshot.lobby_state, baseline.lobby_state);
            assert_eq!(snapshot.players.len(), baseline.players.len());
            for (actual, expected) in snapshot.players.iter().zip(&baseline.players) {
                let mut expected = expected.clone();
                expected.direct_endpoint = None;
                assert_eq!(actual, &expected);
            }
            let direct = database
                .get_room_session_selection_snapshot_for_transport(&room.id, true)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(direct.players, baseline.players);
            assert!(baseline.players.iter().all(|player| player.direct_endpoint
                == Some(DirectEndpoint {
                    host: host.clone(),
                    port: 7777
                })));
            assert!(database
                .get_room_session_selection_snapshot_for_transport(&Uuid::nil(), false)
                .await
                .unwrap()
                .is_none());
            assert!(
                stats.bytes_allocated + usize::try_from(count).unwrap() * host.len()
                    <= baseline_stats.bytes_allocated,
                "WebRTC refresh copied unused endpoints: {stats:?}, baseline={baseline_stats:?}"
            );
        });
    }
}
