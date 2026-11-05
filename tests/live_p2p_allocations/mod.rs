//! Complete live host operations with exact plans and varied legacy metadata.

mod routing;
pub(super) use routing::{
    bus_exclusions_do_not_grow_recipient_storage, game_start_shares_configuration,
    roster_snapshot_reserves_once, signaling_cost_is_independent_of_room_size,
    transport_report_delivery_cost,
};

use super::*;
use signal_fish_server::protocol::{DirectEndpoint, SessionPlanPayload};
use signal_fish_server::server::allocation_benchmark::negotiate_direct_for_measurement;

fn take_plan(
    receiver: &mut tokio::sync::mpsc::Receiver<std::sync::Arc<ServerMessage>>,
) -> SessionPlanPayload {
    let message = receiver.try_recv().unwrap();
    let ServerMessage::SessionPlan(plan) = message.as_ref() else {
        panic!("{message:?}");
    };
    plan.as_ref().clone()
}

/// Check complete host updates and departures against varied unused metadata.
pub(super) fn live_host_operation_does_not_copy_legacy_metadata(
    direct: bool,
    direct_metadata: bool,
) {
    let mut profiles = Vec::new();
    for bytes in [0, 1024, 64 * 1024] {
        allocation_harness::block_on(async {
            allocation_harness::warm_current_thread_runtime().await;
            let server = EnhancedGameServer::new(
                ServerConfig::default(),
                ProtocolConfig::default(),
                RelayTypeConfig::default(),
                SessionConfig {
                    default_topology: Topology::Host,
                    enable_webrtc: !direct,
                    enable_direct: direct,
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
            let host = Uuid::from_u128(1);
            let successor = Uuid::from_u128(2);
            let guest = Uuid::from_u128(3);
            let (host_tx, mut host_rx) = tokio::sync::mpsc::channel(64);
            let (successor_tx, mut successor_rx) = tokio::sync::mpsc::channel(64);
            let (guest_tx, mut guest_rx) = tokio::sync::mpsc::channel(64);
            for (player, sender, name) in [
                (host, host_tx, "Host"),
                (successor, successor_tx, "Next"),
                (guest, guest_tx, "Guest"),
            ] {
                server.connect_client(player, sender).await;
                if direct {
                    negotiate_direct_for_measurement(&server, &player);
                } else {
                    negotiate_webrtc_for_measurement(&server, &player);
                }
                server
                    .handle_join_room(
                        &player,
                        "host-profile".into(),
                        Some("HOST01".into()),
                        name.into(),
                        Some(4),
                        Some(true),
                        None,
                        None,
                        None,
                    )
                    .await;
                server.handle_player_ready(&player).await;
            }
            let room_id = server.get_client_room(&host).await.unwrap();
            let endpoint = ConnectionInfo::Direct {
                host: "192.0.2.1".into(),
                port: 7777,
            };
            if direct {
                server.handle_provide_connection_info(&host, endpoint).await;
            }
            server.handle_start_game(&host).await;
            let old_plan = {
                let mut plan = None;
                loop {
                    match host_rx.try_recv() {
                        Ok(message) => match message.as_ref() {
                            ServerMessage::SessionPlan(value) => {
                                assert!(plan.replace(value.clone()).is_none());
                            }
                            ServerMessage::RoomJoined(_)
                            | ServerMessage::PlayerJoined { .. }
                            | ServerMessage::LobbyStateChanged { .. }
                            | ServerMessage::AuthorityChanged { .. }
                            | ServerMessage::GameStarting { .. } => {}
                            other => panic!("unexpected setup message: {other:?}"),
                        },
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                        Err(error) => panic!("host setup channel closed: {error}"),
                    }
                }
                plan.unwrap()
            };
            assert_eq!(old_plan.host, Some(host));
            assert_eq!(
                old_plan.transport,
                if direct {
                    Transport::Direct
                } else {
                    Transport::WebRtc
                }
            );
            drain_membership_setup(&mut successor_rx, false);
            drain_membership_setup(&mut guest_rx, false);
            let metadata = serde_json::json!({"nested": ["x".repeat(bytes)]});
            let connection_info = if direct_metadata {
                let host = match bytes {
                    0 => "127.0.0.1".to_string(),
                    1024 => "a".repeat(63),
                    _ => [
                        "a".repeat(63),
                        "b".repeat(63),
                        "c".repeat(63),
                        "d".repeat(61),
                    ]
                    .join("."),
                };
                let info = ConnectionInfo::Direct { host, port: 7777 };
                assert!(DirectEndpoint::from_connection_info(&info).is_some());
                info
            } else {
                ConnectionInfo::Custom {
                    data: metadata.clone(),
                }
            };
            assert!(server
                .database()
                .update_player_connection_info(&room_id, &guest, connection_info.clone(),)
                .await
                .unwrap());
            let region = stats_alloc::Region::new(ALLOCATOR);
            if direct {
                server
                    .handle_provide_connection_info(
                        &host,
                        ConnectionInfo::Direct {
                            host: "192.0.2.9".into(),
                            port: 9999,
                        },
                    )
                    .await;
            } else {
                server.leave_room(&host).await;
            }
            let stats = region.change();
            let endpoint_host_bytes = match &connection_info {
                ConnectionInfo::Direct { host, .. } => host.len(),
                _ => 0,
            };
            println!("LIVE_HOST_PROFILE direct={direct} direct_metadata={direct_metadata} bytes={bytes} endpoint_host_bytes={endpoint_host_bytes} stats={stats:?}");
            profiles.push(stats);
            if !direct {
                for receiver in [&mut successor_rx, &mut guest_rx] {
                    let left = receiver.try_recv().unwrap();
                    assert!(
                        matches!(left.as_ref(), ServerMessage::PlayerLeft { player_id, epoch: Some(_), final_seq: Some(_), .. } if *player_id == host)
                    );
                    let authority = receiver.try_recv().unwrap();
                    assert!(matches!(
                        authority.as_ref(),
                        ServerMessage::AuthorityChanged {
                            authority_player: None,
                            you_are_authority: false
                        }
                    ));
                }
            }
            let successor_plan = take_plan(&mut successor_rx);
            let guest_plan = take_plan(&mut guest_rx);
            let selected = if direct {
                host
            } else {
                successor_plan.host.unwrap()
            };
            assert!(direct || [successor, guest].contains(&selected));
            let members = if direct {
                vec![host, successor, guest]
            } else {
                vec![successor, guest]
            };
            for (recipient, plan) in [(successor, &successor_plan), (guest, &guest_plan)] {
                assert_eq!(plan.generation, successor_plan.generation);
                assert_ne!(plan.generation, old_plan.generation);
                assert_eq!(plan.host, Some(selected));
                assert_eq!(plan.topology, Topology::Host);
                assert_eq!(plan.transport, old_plan.transport);
                assert_eq!(plan.fallback, Transport::Relay);
                let expected: std::collections::BTreeSet<_> = members
                    .iter()
                    .copied()
                    .filter(|id| *id != recipient && (recipient == selected || *id == selected))
                    .collect();
                assert_eq!(
                    plan.peers
                        .iter()
                        .map(|peer| peer.player_id)
                        .collect::<std::collections::BTreeSet<_>>(),
                    expected
                );
                assert!(plan
                    .peers
                    .iter()
                    .all(|peer| peer.initiate == (recipient != selected)));
                assert_eq!(
                    plan.direct_endpoint,
                    direct.then(|| DirectEndpoint {
                        host: "192.0.2.9".into(),
                        port: 9999
                    })
                );
            }
            if direct {
                let plan = take_plan(&mut host_rx);
                assert_eq!(plan.generation, successor_plan.generation);
                assert_eq!(plan.host, Some(host));
                assert_eq!(plan.peers.len(), 2);
            }
            for receiver in [&mut successor_rx, &mut guest_rx] {
                assert_membership_queue_empty(receiver);
            }
            let stored = server
                .database()
                .get_room_by_id(&room_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::to_value(&stored.players[&guest].connection_info).unwrap(),
                serde_json::to_value(Some(&connection_info)).unwrap(),
            );
            if direct {
                assert!(
                    matches!(&stored.players[&host].connection_info, Some(ConnectionInfo::Direct { host, port }) if host == "192.0.2.9" && *port == 9999)
                );
            } else {
                assert!(!stored.players.contains_key(&host));
            }
        });
    }
    for (bytes, stats) in [0, 1024, 64 * 1024].into_iter().zip(&profiles) {
        assert!(stats.bytes_allocated <= profiles[0].bytes_allocated + if direct_metadata { 128 } else { 512 },
            "host operation copied unrelated player metadata: direct={direct} bytes={bytes} profiles={profiles:?}");
    }
}
