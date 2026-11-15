//! Complete fallback publications retain exact plans without unused endpoint copies.
use super::*;
use signal_fish_server::server::allocation_benchmark::{
    negotiate_direct_for_measurement, publish_finalized_baseline_for_measurement,
};
use std::sync::Arc;

/// #636/#826: reuse one captured baseline after its storage row disappears.
pub(crate) fn finalized_fallback_copies_only_required_endpoints() {
    for count in [2_u128, 16] {
        for (topology, transport) in [
            (Topology::Mesh, Transport::WebRtc),
            (Topology::Host, Transport::WebRtc),
            (Topology::Host, Transport::Direct),
        ] {
            allocation_harness::block_on(async {
                allocation_harness::warm_current_thread_runtime().await;
                let server = EnhancedGameServer::new(
                    ServerConfig {
                        max_connections_per_ip: 512,
                        ..Default::default()
                    },
                    ProtocolConfig {
                        max_players_limit: 255,
                        ..Default::default()
                    },
                    RelayTypeConfig::default(),
                    SessionConfig {
                        default_topology: topology,
                        enable_webrtc: transport == Transport::WebRtc,
                        enable_direct: transport == Transport::Direct,
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
                let host = Uuid::from_u128(1);
                let joiner = Uuid::from_u128(count);
                let mut receivers = Vec::new();
                for index in 1..=count {
                    let id = Uuid::from_u128(index);
                    let (sender, receiver) = tokio::sync::mpsc::channel(1024);
                    receivers.push((id, receiver));
                    server.connect_client(id, sender).await;
                    if transport == Transport::Direct {
                        negotiate_direct_for_measurement(&server, &id);
                    } else {
                        negotiate_webrtc_for_measurement(&server, &id);
                    }
                    server
                        .handle_join_room(
                            &id,
                            "fallback".into(),
                            Some("FALL01".into()),
                            format!("Player{index}"),
                            Some(255),
                            Some(true),
                            None,
                            None,
                            None,
                        )
                        .await;
                    server.handle_player_ready(&id).await;
                }
                for (id, _) in &receivers {
                    server
                        .handle_provide_connection_info(
                            id,
                            ConnectionInfo::Direct {
                                host: "192.0.2.17".into(),
                                port: 7777,
                            },
                        )
                        .await;
                }
                server
                    .handle_provide_connection_info(
                        &host,
                        ConnectionInfo::Direct {
                            host: "192.0.2.1".into(),
                            port: 7777,
                        },
                    )
                    .await;
                server.handle_start_game(&host).await;
                let room_id = server.get_client_room(&host).await.unwrap();
                let mut room = server
                    .database()
                    .get_room_by_id(&room_id)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    room.lobby_state,
                    signal_fish_server::protocol::LobbyState::Finalized
                );
                for (_, receiver) in &mut receivers {
                    drain_membership_setup(receiver, false);
                }
                assert!(server.database().delete_room(&room_id).await.unwrap());
                let short = "a".repeat(63);
                let long = [
                    "a".repeat(63),
                    "b".repeat(63),
                    "c".repeat(63),
                    "d".repeat(61),
                ]
                .join(".");
                let mut profiles = Vec::new();
                let mut previous_generation = None;
                for (attempt, hostname) in [&short, &long, &short, &long, &short, &long]
                    .into_iter()
                    .enumerate()
                {
                    room.players.get_mut(&host).unwrap().connection_info =
                        Some(ConnectionInfo::Direct {
                            host: hostname.clone(),
                            port: 7777,
                        });
                    let baseline = Arc::new(room.clone());
                    let region = stats_alloc::Region::new(ALLOCATOR);
                    assert!(
                        publish_finalized_baseline_for_measurement(&server, baseline, joiner).await
                    );
                    let stats = region.change();
                    println!("FINALIZED_FALLBACK_SAMPLE count={count} topology={topology:?} transport={transport:?} attempt={attempt} stats={stats:?}");
                    if attempt >= 2 {
                        profiles.push(stats);
                    }
                    let mut generation = None;
                    for (recipient, receiver) in &mut receivers {
                        if *recipient != joiner {
                            let opening = receiver.try_recv().unwrap();
                            assert!(
                                matches!(opening.as_ref(), ServerMessage::PlayerJoined { player }
                                if serde_json::to_value(player).unwrap() == serde_json::to_value(room.players.get(&joiner).unwrap()).unwrap())
                            );
                        }
                        let message = receiver.try_recv().unwrap();
                        let ServerMessage::SessionPlan(plan) = message.as_ref() else {
                            panic!("{message:?}");
                        };
                        assert_eq!((plan.topology, plan.transport), (topology, transport));
                        assert_eq!(plan.host, (topology == Topology::Host).then_some(host));
                        assert_eq!(
                            plan.direct_endpoint,
                            (transport == Transport::Direct).then(|| {
                                signal_fish_server::protocol::DirectEndpoint {
                                    host: hostname.clone(),
                                    port: 7777,
                                }
                            })
                        );
                        assert_eq!(plan.fallback, Transport::Relay);
                        assert!(plan.ice_servers.is_empty());
                        assert_ne!(Some(plan.generation), previous_generation);
                        if let Some(expected) = generation {
                            assert_eq!(plan.generation, expected);
                        }
                        generation = Some(plan.generation);
                        let mut expected: Vec<_> = room
                            .players
                            .values()
                            .filter(|peer| {
                                peer.id != *recipient
                                    && (topology == Topology::Mesh
                                        || *recipient == host
                                        || peer.id == host)
                            })
                            .map(|peer| {
                                (
                                    peer.id,
                                    peer.name.clone(),
                                    if topology == Topology::Host {
                                        peer.id == host
                                    } else {
                                        peer.is_authority
                                    },
                                    if topology == Topology::Mesh {
                                        *recipient < peer.id
                                    } else {
                                        *recipient != host
                                    },
                                )
                            })
                            .collect();
                        let mut actual: Vec<_> = plan
                            .peers
                            .iter()
                            .map(|peer| {
                                (
                                    peer.player_id,
                                    peer.player_name.clone(),
                                    peer.is_authority,
                                    peer.initiate,
                                )
                            })
                            .collect();
                        actual.sort_unstable();
                        expected.sort_unstable();
                        assert_eq!(actual, expected);
                        assert_membership_queue_empty(receiver);
                    }
                    previous_generation = generation;
                    assert!(server
                        .database()
                        .get_room_by_id(&room_id)
                        .await
                        .unwrap()
                        .is_none());
                    assert_eq!(server.get_client_room(&joiner).await, Some(room_id));
                }
                println!("FINALIZED_FALLBACK count={count} topology={topology:?} transport={transport:?} profiles={profiles:?}");
                let copies = if transport == Transport::Direct {
                    usize::try_from(count).unwrap() + 1
                } else {
                    0
                };
                for pair in profiles.chunks_exact(2) {
                    assert_eq!(
                        pair[1].bytes_allocated - pair[0].bytes_allocated,
                        copies * (long.len() - short.len()),
                        "fallback copied endpoints beyond the selected transport: {profiles:?}"
                    );
                    assert_eq!(pair[1].allocations, pair[0].allocations);
                }

                // #1464: test caller arguments, sticky repair, and every published edge.
                let mut incapable = Vec::new();
                let mut saved_host = (topology == Topology::Host).then_some(host);
                for step in 0..count {
                    let retiring = saved_host.unwrap_or(Uuid::from_u128(step + 1));
                    incapable.push(retiring);
                    if transport == Transport::Direct {
                        negotiate_webrtc_for_measurement(&server, &retiring);
                    } else {
                        negotiate_direct_for_measurement(&server, &retiring);
                    }
                    room.authority_player = if step % 2 == 0 {
                        Some(retiring)
                    } else {
                        Some(joiner)
                    };
                    let mut candidates: Vec<_> = room
                        .players
                        .values()
                        .filter(|p| !incapable.contains(&p.id))
                        .collect();
                    candidates.sort_by_key(|p| {
                        (
                            Some(p.id) != room.authority_player,
                            p.connected_at
                                .unwrap_or(chrono::DateTime::<chrono::Utc>::MAX_UTC),
                            p.id,
                        )
                    });
                    if topology == Topology::Host {
                        saved_host = candidates.first().map(|p| p.id);
                    }
                    let reset = topology == Topology::Host && saved_host.is_none();
                    let expected_topology = if reset { Topology::Relay } else { topology };
                    let expected_transport = if reset { Transport::Relay } else { transport };
                    assert!(
                        publish_finalized_baseline_for_measurement(
                            &server,
                            Arc::new(room.clone()),
                            joiner
                        )
                        .await
                    );
                    let mut generation = None;
                    for (recipient, receiver) in &mut receivers {
                        if *recipient != joiner {
                            assert!(matches!(receiver.try_recv().unwrap().as_ref(),
                                ServerMessage::PlayerJoined { player } if player.id == joiner));
                        }
                        let message = receiver.try_recv().unwrap();
                        let ServerMessage::SessionPlan(plan) = message.as_ref() else {
                            panic!("{message:?}");
                        };
                        assert_eq!(
                            (plan.topology, plan.transport, plan.host),
                            (expected_topology, expected_transport, saved_host)
                        );
                        let endpoint = if transport == Transport::Direct && !reset {
                            match room.players[&saved_host.unwrap()]
                                .connection_info
                                .as_ref()
                                .unwrap()
                            {
                                ConnectionInfo::Direct { host, port } => {
                                    Some(signal_fish_server::protocol::DirectEndpoint {
                                        host: host.clone(),
                                        port: *port,
                                    })
                                }
                                other => panic!("{other:?}"),
                            }
                        } else {
                            None
                        };
                        assert_eq!(plan.direct_endpoint, endpoint);
                        assert_eq!(plan.fallback, Transport::Relay);
                        assert!(plan.ice_servers.is_empty());
                        assert_ne!(Some(plan.generation), previous_generation);
                        if let Some(expected) = generation {
                            assert_eq!(plan.generation, expected);
                        }
                        generation = Some(plan.generation);
                        let mut actual = plan
                            .peers
                            .iter()
                            .map(|p| {
                                (
                                    p.player_id,
                                    p.player_name.clone(),
                                    p.is_authority,
                                    p.initiate,
                                )
                            })
                            .collect::<Vec<_>>();
                        actual.sort_unstable();
                        let mut expected = candidates
                            .iter()
                            .filter(|p| {
                                !reset
                                    && !incapable.contains(recipient)
                                    && p.id != *recipient
                                    && (topology == Topology::Mesh
                                        || saved_host == Some(*recipient)
                                        || saved_host == Some(p.id))
                            })
                            .map(|p| {
                                (
                                    p.id,
                                    p.name.clone(),
                                    if topology == Topology::Host {
                                        saved_host == Some(p.id)
                                    } else {
                                        p.is_authority
                                    },
                                    if topology == Topology::Mesh {
                                        *recipient < p.id
                                    } else {
                                        saved_host != Some(*recipient)
                                    },
                                )
                            })
                            .collect::<Vec<_>>();
                        expected.sort_unstable();
                        assert_eq!(actual, expected, "count={count} topology={topology:?} transport={transport:?} step={step} recipient={recipient}");
                        assert_membership_queue_empty(receiver);
                    }
                    previous_generation = generation;
                }
                if topology == Topology::Host {
                    for (player, _) in &receivers {
                        if transport == Transport::Direct {
                            negotiate_direct_for_measurement(&server, player);
                        } else {
                            negotiate_webrtc_for_measurement(&server, player);
                        }
                    }
                    assert!(
                        publish_finalized_baseline_for_measurement(
                            &server,
                            Arc::new(room.clone()),
                            joiner
                        )
                        .await
                    );
                    let mut generation = None;
                    for (recipient, receiver) in &mut receivers {
                        if *recipient != joiner {
                            assert!(matches!(receiver.try_recv().unwrap().as_ref(),
                                ServerMessage::PlayerJoined { player } if player.id == joiner));
                        }
                        let message = receiver.try_recv().unwrap();
                        let ServerMessage::SessionPlan(plan) = message.as_ref() else {
                            panic!("{message:?}");
                        };
                        assert_eq!(
                            (plan.topology, plan.transport, plan.host),
                            (Topology::Relay, Transport::Relay, None),
                            "restored capabilities must not restart a cleared sticky session"
                        );
                        assert!(plan.peers.is_empty());
                        assert!(plan.direct_endpoint.is_none());
                        assert!(plan.ice_servers.is_empty());
                        assert_eq!(plan.fallback, Transport::Relay);
                        assert_ne!(Some(plan.generation), previous_generation);
                        if let Some(expected) = generation {
                            assert_eq!(plan.generation, expected);
                        }
                        generation = Some(plan.generation);
                        assert_membership_queue_empty(receiver);
                    }
                }
            });
        }
    }
}
