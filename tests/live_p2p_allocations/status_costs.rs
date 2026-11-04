//! Unknown-routing peer status selection must not copy legacy player metadata.
use super::*;
use signal_fish_server::protocol::PlayerInfo;
use signal_fish_server::server::allocation_benchmark::transport_status_fallback_recipients_for_measurement;

/// Compare status recipient selection with the previous full-player snapshot on identical rooms.
pub(crate) fn transport_status_fallback_skips_player_metadata() {
    for members in [2_u128, 16, 255] {
        let mut profiles = Vec::new();
        let mut reference_profiles = Vec::new();
        for bytes in [0, 1024, 64 * 1024] {
            allocation_harness::block_on(async {
                let server = EnhancedGameServer::new(
                    ServerConfig {
                        max_connections_per_ip: 512,
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
                .unwrap();
                let reporter = Uuid::from_u128(1);
                let room = server
                    .database()
                    .create_room(
                        "status-cost".into(),
                        None,
                        255,
                        false,
                        reporter,
                        "relay".into(),
                        "test".into(),
                        None,
                    )
                    .await
                    .unwrap();
                let metadata = serde_json::json!({"nested": ["x".repeat(bytes)]});
                let mut receivers = Vec::new();
                for index in 1..=members + 1 {
                    let id = Uuid::from_u128(index);
                    let (sender, receiver) = tokio::sync::mpsc::channel(4);
                    receivers.push(receiver);
                    server.connect_client(id, sender).await;
                    if index != 2 {
                        negotiate_webrtc_for_measurement(&server, &id);
                    }
                    if index > 1 && index <= members {
                        assert!(server
                            .database()
                            .add_player_to_room(
                                &room.id,
                                PlayerInfo {
                                    id,
                                    name: format!("member-{index}"),
                                    is_authority: false,
                                    is_ready: false,
                                    connected_at: None,
                                    connection_info: None,
                                    epoch: None,
                                    seq: None,
                                    region_id: "test".into(),
                                }
                            )
                            .await
                            .unwrap());
                    }
                    if index <= members {
                        assert!(server
                            .database()
                            .update_player_connection_info(
                                &room.id,
                                &id,
                                ConnectionInfo::Custom {
                                    data: metadata.clone()
                                }
                            )
                            .await
                            .unwrap());
                    }
                }
                let reference_region = stats_alloc::Region::new(ALLOCATOR);
                let mut reference: Vec<_> = server
                    .database()
                    .get_room_players(&room.id)
                    .await
                    .unwrap()
                    .into_iter()
                    .filter(|member| member.id != reporter && server.client_supports_v3(&member.id))
                    .map(|member| member.id)
                    .collect();
                let reference_stats = reference_region.change();
                println!("STATUS_FALLBACK_REFERENCE members={members} bytes={bytes} stats={reference_stats:?}");
                reference_profiles.push(reference_stats);
                reference.sort_unstable();
                let region = stats_alloc::Region::new(ALLOCATOR);
                let mut recipients = transport_status_fallback_recipients_for_measurement(
                    &server, &reporter, &room.id,
                )
                .await
                .unwrap();
                let stats = region.change();
                println!("STATUS_FALLBACK members={members} bytes={bytes} stats={stats:?}");
                profiles.push(stats);
                recipients.sort_unstable();
                assert_eq!(recipients, reference);
                assert_eq!(
                    recipients,
                    (3..=members).map(Uuid::from_u128).collect::<Vec<_>>()
                );
                for receiver in &mut receivers {
                    assert_membership_queue_empty(receiver);
                }
                let stored = server
                    .database()
                    .get_room_by_id(&room.id)
                    .await
                    .unwrap()
                    .unwrap();
                for member in stored.players.values() {
                    assert!(
                        matches!(&member.connection_info, Some(ConnectionInfo::Custom { data }) if *data == metadata)
                    );
                }
                // A missing room has no stored status recipients.
                assert!(transport_status_fallback_recipients_for_measurement(
                    &server,
                    &reporter,
                    &Uuid::from_u128(999)
                )
                .await
                .unwrap()
                .is_empty());
            });
        }
        assert!(reference_profiles[2].bytes_allocated >= reference_profiles[0].bytes_allocated
            + usize::try_from(members).unwrap() * 64 * 1024,
            "the previous snapshot control did not copy every stored member's metadata: {reference_profiles:?}");
        for profile in &profiles {
            assert!(
                profile.bytes_allocated <= profiles[0].bytes_allocated + 128,
                "status fallback copied player metadata: members={members} profiles={profiles:?}"
            );
        }
    }
}
