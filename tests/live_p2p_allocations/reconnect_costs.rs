//! Complete reconnect baselines retain their owned player roster (#1454).
use super::*;

/// Compare complete v2/v3 reconnects with exact rosters, readiness, and ordered watermarks.
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
