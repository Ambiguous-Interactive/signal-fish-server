//! Complete reconnect baselines retain their owned player roster (#1454).
use super::*;

/// Compare warmed complete reconnects with exact rosters, readiness, and ordered watermarks.
pub(crate) fn reconnect_baseline_reuses_player_roster() {
    allocation_harness::block_on(async {
        let mut profiles = Vec::new();
        for members in [2_u8, 16, 255] {
            for v3 in [false, true] {
                tokio::task::yield_now().await;
                let server = EnhancedGameServer::new(
                    ServerConfig {
                        enable_reconnection: true,
                        ..Default::default()
                    },
                    ProtocolConfig {
                        max_players_limit: 255,
                        ..Default::default()
                    },
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
                .unwrap();
                let actor = Uuid::from_u128(1);
                let mut receivers = Vec::new();
                for index in 1..=members {
                    let player = Uuid::from_u128(u128::from(index));
                    let (sender, receiver) = tokio::sync::mpsc::channel(usize::from(members) + 16);
                    receivers.push(receiver);
                    server.connect_client(player, sender).await;
                    server
                        .handle_join_room(
                            &player,
                            "reconnect-roster".into(),
                            Some("RECON1".into()),
                            format!("Player{index}"),
                            Some(members),
                            Some(false),
                            None,
                            None,
                            None,
                        )
                        .await;
                }
                let room_id = server.get_client_room(&actor).await.unwrap();
                // Public embedders can pre-issue tokens for legacy connections too.
                let token = server
                    .reconnection_manager()
                    .unwrap()
                    .pre_issue_token(actor, room_id)
                    .await;
                let ready = Uuid::from_u128(2);
                server.handle_player_ready(&ready).await;
                server.unregister_client(&actor).await;
                for receiver in &mut receivers {
                    drain_membership_setup(receiver, true);
                }
                let temporary = Uuid::from_u128(1000);
                let (sender, mut receiver) = tokio::sync::mpsc::channel(16);
                server.connect_client(temporary, sender).await;
                if v3 {
                    negotiate_webrtc_for_measurement(&server, &temporary);
                }
                // Exercise the same identity swap before comparing roster cost.
                // Separate fresh servers have different hash-table layouts;
                // a first reconnect can grow a tombstone-filled connection shard.
                let initialization = stats_alloc::Region::new(ALLOCATOR);
                assert!(
                    server
                        .handle_reconnect(&temporary, &actor, &room_id, &token)
                        .await
                );
                let initialization = initialization.change();
                println!(
                    "RECONNECT_ROSTER_INITIALIZATION members={members} v3={v3} stats={initialization:?}"
                );
                let baseline = receiver.try_recv().unwrap();
                assert!(
                    matches!(baseline.as_ref(), ServerMessage::Reconnected(payload)
                    if payload.player_id == actor && payload.current_players.len() == usize::from(members))
                );
                assert_membership_queue_empty(&mut receiver);
                for receiver in receivers.iter_mut().skip(1) {
                    let opening = receiver.try_recv().unwrap();
                    assert!(
                        matches!(opening.as_ref(), ServerMessage::PlayerReconnected { player_id, .. }
                        if *player_id == actor)
                    );
                    assert_membership_queue_empty(receiver);
                }
                let token = server
                    .reconnection_manager()
                    .unwrap()
                    .pre_issue_token(actor, room_id)
                    .await;
                server.unregister_client(&actor).await;
                for receiver in &mut receivers {
                    drain_membership_setup(receiver, true);
                }
                let (sender, mut receiver) = tokio::sync::mpsc::channel(16);
                server.connect_client(temporary, sender).await;
                if v3 {
                    negotiate_webrtc_for_measurement(&server, &temporary);
                }
                tokio::task::yield_now().await;
                let region = stats_alloc::Region::new(ALLOCATOR);
                assert!(
                    server
                        .handle_reconnect(&temporary, &actor, &room_id, &token)
                        .await
                );
                let stats = region.change();
                println!("RECONNECT_ROSTER members={members} v3={v3} stats={stats:?}");
                profiles.push((members, v3, stats));
                let baseline = receiver.try_recv().unwrap();
                let ServerMessage::Reconnected(payload) = baseline.as_ref() else {
                    panic!("{baseline:?}");
                };
                assert_eq!(payload.player_id, actor);
                assert_eq!(payload.current_players.len(), usize::from(members));
                assert_eq!(payload.ready_players, vec![ready]);
                assert_eq!(
                    payload.sender_watermarks.len(),
                    if v3 { usize::from(members) } else { 0 }
                );
                let mut player_ids: Vec<_> = payload
                    .current_players
                    .iter()
                    .map(|player| player.id)
                    .collect();
                player_ids.sort_unstable();
                assert_eq!(
                    player_ids,
                    (1..=members)
                        .map(|index| Uuid::from_u128(u128::from(index)))
                        .collect::<Vec<_>>()
                );
                if v3 {
                    assert!(payload
                        .current_players
                        .iter()
                        .zip(&payload.sender_watermarks)
                        .all(|(player, stamp)| player.id == stamp.player_id));
                }
                for player in &payload.current_players {
                    assert_eq!(player.name, format!("Player{}", player.id.as_u128()));
                    assert_eq!(player.is_ready, player.id == ready);
                    assert_eq!(player.epoch.is_some(), v3);
                    assert_eq!(player.seq.is_some(), v3);
                    if v3 {
                        let watermark = payload
                            .sender_watermarks
                            .iter()
                            .find(|stamp| stamp.player_id == player.id)
                            .unwrap();
                        assert_eq!(Some(watermark.epoch), player.epoch);
                        assert_eq!(Some(watermark.seq), player.seq);
                    }
                }
                assert_membership_queue_empty(&mut receiver);
            }
        }
        for pair in profiles.chunks_exact(2) {
            let (members, _, legacy) = pair[0];
            let (_, _, v3) = pair[1];
            let watermarks = usize::from(members)
                * std::mem::size_of::<signal_fish_server::protocol::SenderWatermark>();
            // Only watermarks grow with membership. The fixed budget covers
            // v3 replay and credential bookkeeping across native platforms.
            assert!(
                v3.bytes_allocated <= legacy.bytes_allocated + watermarks + 2048,
                "v3 baseline rebuilt its owned roster: members={members} legacy={legacy:?} v3={v3:?}"
            );
        }
    });
}

/// #636/#826/#1650: compare the same warmed reconnect identity and room, not fresh setups.
pub(crate) fn webrtc_reconnect_skips_unused_direct_endpoints() {
    allocation_harness::block_on(async {
        let server = EnhancedGameServer::new(
            ServerConfig {
                enable_reconnection: true,
                ..Default::default()
            },
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
        let host = Uuid::from_u128(1);
        let guest = Uuid::from_u128(2);
        let temporary = Uuid::from_u128(3);
        let (host_sender, mut host_receiver) = tokio::sync::mpsc::channel(64);
        let (guest_sender, mut guest_receiver) = tokio::sync::mpsc::channel(64);
        server.connect_client(host, host_sender).await;
        server.connect_client(guest, guest_sender).await;
        for (player, name) in [(host, "Host"), (guest, "Guest")] {
            negotiate_webrtc_for_measurement(&server, &player);
            server
                .handle_join_room(
                    &player,
                    "reconnect-endpoints".into(),
                    Some("RECON1".into()),
                    name.into(),
                    Some(2),
                    Some(true),
                    None,
                    None,
                    None,
                )
                .await;
            server.handle_player_ready(&player).await;
        }
        let room = server.get_client_room(&host).await.unwrap();
        server.handle_start_game(&host).await;
        drain_membership_setup(&mut host_receiver, false);
        drain_membership_setup(&mut guest_receiver, false);
        allocation_harness::warm_current_thread_runtime().await;
        let mut profiles = Vec::with_capacity(4);
        let mut prior_generation = None;
        for (cycle, length) in [0, 63, 253, 63, 253, 63, 253].into_iter().enumerate() {
            let endpoint = (length > 0).then(|| ConnectionInfo::Direct {
                host: if length == 253 {
                    [
                        "a".repeat(63),
                        "b".repeat(63),
                        "c".repeat(63),
                        "d".repeat(61),
                    ]
                    .join(".")
                } else {
                    "a".repeat(length)
                },
                port: 7777,
            });
            if let Some(endpoint) = &endpoint {
                assert!(server
                    .database()
                    .update_player_connection_info(&room, &host, endpoint.clone())
                    .await
                    .unwrap());
            }
            let token = server
                .reconnection_manager()
                .unwrap()
                .pre_issue_token(guest, room)
                .await;
            server.unregister_client(&guest).await;
            drain_membership_setup(&mut host_receiver, false);
            drain_membership_setup(&mut guest_receiver, true);
            let (sender, receiver) = tokio::sync::mpsc::channel(64);
            server.connect_client(temporary, sender).await;
            negotiate_webrtc_for_measurement(&server, &temporary);
            let region = stats_alloc::Region::new(ALLOCATOR);
            assert!(
                server
                    .handle_reconnect(&temporary, &guest, &room, &token)
                    .await
            );
            let stats = region.change();
            println!(
                "WARM_RECONNECT_ENDPOINT cycle={cycle} length={length} room={room} stats={stats:?}"
            );
            guest_receiver = receiver;
            let baseline = guest_receiver.try_recv().unwrap();
            let ServerMessage::Reconnected(payload) = baseline.as_ref() else {
                panic!("{baseline:?}");
            };
            assert_eq!(payload.player_id, guest);
            assert_eq!(payload.current_players.len(), 2);
            assert_eq!(payload.sender_watermarks.len(), 2);
            assert!(payload.reconnection_token.is_some());
            for (player, name) in [(host, "Host"), (guest, "Guest")] {
                let member = payload
                    .current_players
                    .iter()
                    .find(|member| member.id == player)
                    .unwrap();
                assert_eq!(member.name, name);
                assert!(member.connection_info.is_none());
                assert!(member.epoch.is_some());
            }
            let lifecycle = host_receiver.try_recv().unwrap();
            assert!(
                matches!(lifecycle.as_ref(), ServerMessage::PlayerReconnected {
                player_id, player: Some(player), epoch: Some(_),
            } if *player_id == guest && player.id == guest && player.name == "Guest")
            );
            let mut generation = None;
            for (recipient, peer, receiver) in [
                (host, guest, &mut host_receiver),
                (guest, host, &mut guest_receiver),
            ] {
                let message = receiver.try_recv().unwrap();
                let ServerMessage::SessionPlan(plan) = message.as_ref() else {
                    panic!("{message:?}");
                };
                assert_eq!(
                    (plan.topology, plan.transport, plan.host),
                    (Topology::Mesh, Transport::WebRtc, None)
                );
                assert!(plan.direct_endpoint.is_none());
                assert_eq!(plan.fallback, Transport::Relay);
                assert_eq!(plan.peers.len(), 1);
                assert_eq!(plan.peers[0].player_id, peer);
                assert_eq!(plan.peers[0].initiate, recipient < peer);
                assert_eq!(
                    plan.peers[0].player_name,
                    if peer == host { "Host" } else { "Guest" }
                );
                if let Some(expected) = generation {
                    assert_eq!(plan.generation, expected);
                }
                generation = Some(plan.generation);
                assert_membership_queue_empty(receiver);
            }
            assert_ne!(generation, prior_generation);
            prior_generation = generation;
            let stored = server
                .database()
                .get_room_by_id(&room)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(stored.players[&host].connection_info, endpoint);
            assert!(stored.players[&guest].connection_info.is_none());
            if cycle >= 3 {
                profiles.push(stats);
            }
        }
        assert_eq!(profiles.len(), 4);
        for pair in profiles.chunks_exact(2) {
            assert_eq!(pair[1].bytes_allocated - pair[0].bytes_allocated, 3 * (253 - 63),
                "the same warmed WebRTC recovery must copy each address only into its three legacy snapshots");
        }
    });
}
