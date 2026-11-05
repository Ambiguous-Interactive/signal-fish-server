//! Exact P2P start publications retain legacy metadata without copying it into selection.

#[path = "live_p2p_allocations/admission_costs.rs"]
mod admission_costs;
mod allocation_harness;
#[path = "live_p2p_allocations/control_costs.rs"]
mod control_costs;
#[path = "live_p2p_allocations/departure_costs.rs"]
mod departure_costs;
#[path = "live_p2p_allocations/endpoint_costs.rs"]
mod endpoint_costs;
mod live_p2p_allocations;
#[path = "live_p2p_allocations/lobby_costs.rs"]
mod lobby_costs;
#[path = "live_p2p_allocations/publication_costs.rs"]
mod publication_costs;
#[path = "live_p2p_allocations/readiness_costs.rs"]
mod readiness_costs;
#[path = "live_p2p_allocations/reconnect_costs.rs"]
mod reconnect_costs;
#[path = "live_p2p_allocations/setup_costs.rs"]
mod setup_costs;
#[path = "live_p2p_allocations/snapshot_costs.rs"]
mod snapshot_costs;
#[path = "live_p2p_allocations/status_costs.rs"]
mod status_costs;
#[path = "live_p2p_allocations/targeted_costs.rs"]
mod targeted_costs;

use signal_fish_server::config::*;
use signal_fish_server::database::DatabaseConfig;
use signal_fish_server::protocol::{ConnectionInfo, ServerMessage, Topology, Transport};
use signal_fish_server::server::allocation_benchmark::negotiate_webrtc_for_measurement;
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use uuid::Uuid;

#[global_allocator]
static ALLOCATOR: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

/// #636/#826: exactly two legacy payload copies suffice for a complete mesh start.
fn session_plans_do_not_copy_custom_connection_metadata() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut profiles = Vec::new();
        for bytes in [0, 1024, 64 * 1024] {
            let server = EnhancedGameServer::new(
                ServerConfig::default(), ProtocolConfig::default(), RelayTypeConfig::default(),
                SessionConfig { default_topology: Topology::Mesh, enable_webrtc: true, ..Default::default() },
                TurnConfig::default(), DatabaseConfig::InMemory, MetricsConfig::default(),
                CoordinationConfig::default(), TransportSecurityConfig::default(), vec![],
            ).await.unwrap();
            let host = Uuid::from_u128(1);
            let guest = Uuid::from_u128(2);
            let (host_sender, mut host_receiver) = tokio::sync::mpsc::channel(32);
            let (guest_sender, mut guest_receiver) = tokio::sync::mpsc::channel(32);
            server.connect_client(host, host_sender).await;
            server.connect_client(guest, guest_sender).await;
            for (player, name) in [(host, "Host"), (guest, "Guest")] {
                negotiate_webrtc_for_measurement(&server, &player);
                server.handle_join_room(&player, "mesh-profile".into(), Some("MESH01".into()),
                    name.into(), Some(4), Some(true), None, None, None).await;
                server.handle_player_ready(&player).await;
            }
            let room_id = server.get_client_room(&host).await.unwrap();
            let metadata = serde_json::json!({"nested": ["x".repeat(bytes)]});
            for player in [host, guest] {
                assert!(server.database().update_player_connection_info(&room_id, &player,
                    ConnectionInfo::Custom { data: metadata.clone() }).await.unwrap());
            }
            for receiver in [&mut host_receiver, &mut guest_receiver] {
                loop {
                    match receiver.try_recv() {
                        Ok(message) => assert!(matches!(message.as_ref(),
                            ServerMessage::RoomJoined(_) | ServerMessage::PlayerJoined { .. }
                            | ServerMessage::LobbyStateChanged { .. } | ServerMessage::AuthorityChanged { .. }),
                            "unexpected setup message: {message:?}"),
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                        Err(error) => panic!("setup channel closed: {error}"),
                    }
                }
            }
            let region = stats_alloc::Region::new(ALLOCATOR);
            server.handle_start_game(&host).await;
            let stats = region.change();
            println!("SESSION_PLAN_PROFILE bytes={bytes} stats={stats:?}");
            profiles.push(stats);
            let mut generation = None;
            for (player, peer, receiver) in [(host, guest, &mut host_receiver), (guest, host, &mut guest_receiver)] {
                let starting = receiver.try_recv().unwrap();
                let ServerMessage::GameStarting { peer_connections } = starting.as_ref() else { panic!("{starting:?}"); };
                assert_eq!(peer_connections.len(), 2);
                for entry in peer_connections {
                    assert!(matches!(&entry.connection_info, Some(ConnectionInfo::Custom { data }) if *data == metadata));
                }
                let plan = receiver.try_recv().unwrap();
                let ServerMessage::SessionPlan(plan) = plan.as_ref() else { panic!("{plan:?}"); };
                assert_eq!(plan.topology, Topology::Mesh);
                assert_eq!(plan.transport, Transport::WebRtc);
                assert_eq!(plan.host, None);
                assert_eq!(plan.peers.len(), 1);
                assert_eq!(plan.peers[0].player_id, peer);
                assert_eq!(plan.peers[0].initiate, player < peer);
                assert_eq!(plan.direct_endpoint, None);
                if let Some(expected) = generation { assert_eq!(plan.generation, expected); }
                generation = Some(plan.generation);
                let trailing = receiver.try_recv();
                assert!(matches!(trailing, Err(tokio::sync::mpsc::error::TryRecvError::Empty)),
                    "unexpected trailing message: {trailing:?}");
            }
            let stored = server.database().get_room_by_id(&room_id).await.unwrap().unwrap();
            for player in [host, guest] {
                assert!(matches!(&stored.players[&player].connection_info, Some(ConnectionInfo::Custom { data }) if *data == metadata));
            }
        }
        let baseline = profiles[0].bytes_allocated;
        for (bytes, profile) in [0, 1024, 64 * 1024].into_iter().zip(profiles) {
            assert!(profile.bytes_allocated <= baseline + 4 * bytes + 512,
                "session selection copied legacy metadata: bytes={bytes} baseline={baseline} stats={profile:?}");
        }
    });
}

fn drain_membership_setup(
    receiver: &mut tokio::sync::mpsc::Receiver<std::sync::Arc<ServerMessage>>,
    allow_disconnected: bool,
) -> Option<String> {
    let mut token = None;
    loop {
        match receiver.try_recv() {
            Ok(message) => match message.as_ref() {
                ServerMessage::RoomJoined(payload) => token = payload.reconnection_token.clone(),
                other => assert!(
                    matches!(
                        other,
                        ServerMessage::PlayerJoined { .. }
                            | ServerMessage::LobbyStateChanged { .. }
                            | ServerMessage::AuthorityChanged { .. }
                            | ServerMessage::GameStarting { .. }
                            | ServerMessage::SessionPlan(_)
                            | ServerMessage::PlayerLeft { .. }
                    ),
                    "unexpected setup message: {other:?}"
                ),
            },
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) if allow_disconnected => {
                break
            }
            Err(error) => panic!("setup channel closed: {error}"),
        }
    }
    token
}

fn assert_membership_queue_empty(
    receiver: &mut tokio::sync::mpsc::Receiver<std::sync::Arc<ServerMessage>>,
) {
    let trailing = receiver.try_recv();
    assert!(
        matches!(trailing, Err(tokio::sync::mpsc::error::TryRecvError::Empty)),
        "unexpected trailing message: {trailing:?}"
    );
}

/// Measure complete join/reconnect publication while varying one retained field.
fn finalized_membership_reuses_owned_room_snapshots(
    reconnect: bool,
    actor_name: bool,
    direct_endpoint: bool,
) {
    let mut profiles = Vec::new();
    let sizes = if direct_endpoint {
        [0, 63, 253]
    } else {
        [0, 1024, 64 * 1024]
    };
    for bytes in sizes {
        allocation_harness::block_on(async {
            tokio::task::yield_now().await;
            let server = EnhancedGameServer::new(
                ServerConfig {
                    enable_reconnection: true,
                    ..Default::default()
                },
                ProtocolConfig {
                    max_player_name_length: if actor_name { 64 * 1024 + 5 } else { 32 },
                    ..Default::default()
                },
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
            let actor = if reconnect { guest } else { Uuid::from_u128(3) };
            let (host_tx, mut host_rx) = tokio::sync::mpsc::channel(64);
            let (guest_tx, mut guest_rx) = tokio::sync::mpsc::channel(64);
            server.connect_client(host, host_tx).await;
            server.connect_client(guest, guest_tx).await;
            for (player, name) in [(host, "Host"), (guest, "Guest")] {
                negotiate_webrtc_for_measurement(&server, &player);
                server
                    .handle_join_room(
                        &player,
                        "membership-profile".into(),
                        Some("MEMB01".into()),
                        if actor_name && player == actor {
                            "G".repeat(bytes + 5)
                        } else {
                            name.into()
                        },
                        Some(4),
                        Some(true),
                        None,
                        None,
                        None,
                    )
                    .await;
                server.handle_player_ready(&player).await;
            }
            let token = drain_membership_setup(&mut guest_rx, false);
            server.handle_start_game(&host).await;
            let room_id = server.get_client_room(&host).await.unwrap();
            let metadata =
                serde_json::json!({"nested": ["x".repeat(if actor_name { 0 } else { bytes })]});
            // Vary one owned field so each byte slope counts its copies.
            let host_info = if direct_endpoint {
                (bytes > 0).then(|| ConnectionInfo::Direct {
                    host: if bytes == 253 {
                        [
                            "a".repeat(63),
                            "b".repeat(63),
                            "c".repeat(63),
                            "d".repeat(61),
                        ]
                        .join(".")
                    } else {
                        "a".repeat(bytes)
                    },
                    port: 7777,
                })
            } else {
                Some(ConnectionInfo::Custom {
                    data: metadata.clone(),
                })
            };
            if let Some(info) = &host_info {
                assert!(server
                    .database()
                    .update_player_connection_info(&room_id, &host, info.clone())
                    .await
                    .unwrap());
            }
            if reconnect {
                server.unregister_client(&guest).await;
            }
            drain_membership_setup(&mut host_rx, false);
            drain_membership_setup(&mut guest_rx, reconnect);
            let temporary = Uuid::from_u128(4);
            let connection = if reconnect { temporary } else { actor };
            let (actor_tx, mut actor_rx) = tokio::sync::mpsc::channel(64);
            server.connect_client(connection, actor_tx).await;
            negotiate_webrtc_for_measurement(&server, &connection);
            let region = stats_alloc::Region::new(ALLOCATOR);
            if reconnect {
                assert!(
                    server
                        .handle_reconnect(&temporary, &guest, &room_id, token.as_deref().unwrap())
                        .await
                );
            } else {
                server
                    .handle_join_room(
                        &actor,
                        "membership-profile".into(),
                        Some("MEMB01".into()),
                        if actor_name {
                            "J".repeat(bytes + 5)
                        } else {
                            "Joiner".into()
                        },
                        Some(4),
                        Some(true),
                        None,
                        None,
                        None,
                    )
                    .await;
            }
            let stats = region.change();
            println!("MEMBERSHIP_PROFILE reconnect={reconnect} direct_endpoint={direct_endpoint} bytes={bytes} stats={stats:?}");
            profiles.push(stats);
            let baseline = actor_rx.try_recv().unwrap();
            let players = match baseline.as_ref() {
                ServerMessage::RoomJoined(payload) if !reconnect => &payload.current_players,
                ServerMessage::Reconnected(payload) if reconnect => {
                    assert_eq!(payload.player_id, guest);
                    assert!(payload.reconnection_token.is_some());
                    &payload.current_players
                }
                _ => panic!("unexpected baseline: {baseline:?}"),
            };
            assert_eq!(players.len(), if reconnect { 2 } else { 3 });
            let actor_snapshot = players.iter().find(|player| player.id == actor).unwrap();
            let expected_actor_name = if actor_name {
                if reconnect { "G" } else { "J" }.repeat(bytes + 5)
            } else if reconnect {
                "Guest".into()
            } else {
                "Joiner".into()
            };
            assert_eq!(actor_snapshot.name, expected_actor_name);
            assert!(actor_snapshot.epoch.is_some());
            let connection_info = &players
                .iter()
                .find(|player| player.id == host)
                .unwrap()
                .connection_info;
            if reconnect {
                assert!(connection_info.is_none());
            } else {
                assert_eq!(connection_info, &host_info);
            }
            let actor_plan = actor_rx.try_recv().unwrap();
            let ServerMessage::SessionPlan(actor_plan) = actor_plan.as_ref() else {
                panic!("{actor_plan:?}");
            };
            assert_eq!(actor_plan.topology, Topology::Mesh);
            assert_eq!(actor_plan.transport, Transport::WebRtc);
            assert_eq!(actor_plan.peers.len(), if reconnect { 1 } else { 2 });
            assert!(actor_plan
                .peers
                .iter()
                .all(|peer| peer.player_id != actor && peer.initiate == (actor < peer.player_id)));
            let lifecycle = host_rx.try_recv().unwrap();
            let lifecycle_actor = match lifecycle.as_ref() {
                ServerMessage::PlayerJoined { player } => player,
                ServerMessage::PlayerReconnected {
                    player: Some(player),
                    ..
                } => player.as_ref(),
                _ => panic!("missing actor snapshot: {lifecycle:?}"),
            };
            assert_eq!(lifecycle_actor.name, expected_actor_name);
            assert_eq!(lifecycle_actor.epoch, actor_snapshot.epoch);
            assert!(
                match lifecycle.as_ref() {
                    ServerMessage::PlayerJoined { player } => !reconnect && player.id == actor,
                    ServerMessage::PlayerReconnected {
                        player_id, epoch, ..
                    } => reconnect && *player_id == actor && epoch.is_some(),
                    _ => false,
                },
                "unexpected lifecycle: {lifecycle:?}"
            );
            let host_plan = host_rx.try_recv().unwrap();
            let ServerMessage::SessionPlan(host_plan) = host_plan.as_ref() else {
                panic!("{host_plan:?}");
            };
            assert_eq!(host_plan.generation, actor_plan.generation);
            assert_eq!(host_plan.peers.len(), actor_plan.peers.len());
            let members = if reconnect {
                vec![host, guest]
            } else {
                vec![host, guest, actor]
            };
            for (recipient, plan) in [(actor, actor_plan), (host, host_plan)] {
                let actual: std::collections::BTreeSet<_> =
                    plan.peers.iter().map(|peer| peer.player_id).collect();
                let expected: std::collections::BTreeSet<_> = members
                    .iter()
                    .copied()
                    .filter(|peer| *peer != recipient)
                    .collect();
                assert_eq!(actual, expected);
                assert_eq!(plan.host, None);
                assert_eq!(plan.direct_endpoint, None);
                assert_eq!(plan.fallback, Transport::Relay);
                assert!(plan
                    .peers
                    .iter()
                    .all(|peer| peer.initiate == (recipient < peer.player_id)));
            }
            if !reconnect {
                let lifecycle = guest_rx.try_recv().unwrap();
                assert!(
                    matches!(lifecycle.as_ref(), ServerMessage::PlayerJoined { player } if player.id == actor)
                );
                let plan = guest_rx.try_recv().unwrap();
                let ServerMessage::SessionPlan(plan) = plan.as_ref() else {
                    panic!("{plan:?}");
                };
                assert_eq!(plan.generation, actor_plan.generation);
                let peers: std::collections::BTreeSet<_> =
                    plan.peers.iter().map(|peer| peer.player_id).collect();
                assert_eq!(peers, [host, actor].into_iter().collect());
                assert_membership_queue_empty(&mut guest_rx);
            }
            assert_membership_queue_empty(&mut actor_rx);
            assert_membership_queue_empty(&mut host_rx);
            let stored = server
                .database()
                .get_room_by_id(&room_id)
                .await
                .unwrap()
                .unwrap();
            assert!(stored.players[&guest].connection_info.is_none());
            assert_eq!(
                stored.players[&guest].name,
                if actor_name && reconnect {
                    "G".repeat(bytes + 5)
                } else {
                    "Guest".into()
                }
            );
            assert_eq!(stored.players[&host].connection_info, host_info);
        });
    }
    let baseline = profiles[0].bytes_allocated;
    let allowed_copies = match (actor_name, reconnect) {
        (true, true) => 8,
        (true, false) => 9,
        (false, _) => 3,
    };
    // The join baseline needs three legacy endpoint copies; selection needs none.
    let fixed_allowance = if direct_endpoint { 64 } else { 1024 };
    for (bytes, stats) in sizes.into_iter().zip(profiles) {
        assert!(stats.bytes_allocated <= baseline + allowed_copies * bytes + fixed_allowance,
                "redundant membership snapshot: reconnect={reconnect} bytes={bytes} baseline={baseline} stats={stats:?}");
    }
}

/// Run each measurement on the thread that owns its harness bookkeeping.
fn main() {
    allocation_harness::run_trials(vec![
        allocation_harness::trial(
            "same_room_registration_keeps_membership_storage",
            control_costs::same_room_registration_keeps_membership_storage,
        ),
        allocation_harness::trial("production_webrtc_refresh_skips_direct_endpoints", || {
            live_p2p_allocations::live_host_operation_does_not_copy_legacy_metadata(false, true)
        }),
        allocation_harness::trial(
            "webrtc_refresh_skips_direct_endpoints",
            endpoint_costs::webrtc_refresh_skips_direct_endpoints,
        ),
        allocation_harness::trial(
            "readiness_sort_avoids_heap_storage",
            snapshot_costs::readiness_sort_avoids_heap_storage,
        ),
        allocation_harness::trial(
            "exact_membership_reply_avoids_current_roster_copy",
            snapshot_costs::exact_membership_reply_avoids_current_roster_copy,
        ),
        allocation_harness::trial(
            "disabled_direct_start_skips_endpoints",
            endpoint_costs::disabled_direct_start_skips_endpoints,
        ),
        allocation_harness::trial(
            "reconnect_baseline_reuses_player_roster",
            reconnect_costs::reconnect_baseline_reuses_player_roster,
        ),
        allocation_harness::trial(
            "targeted_reply_avoids_recipient_storage",
            targeted_costs::targeted_reply_avoids_recipient_storage,
        ),
        allocation_harness::trial(
            "targeted_room_delivery_borrows_healthy_sender",
            targeted_costs::targeted_room_delivery_borrows_healthy_sender,
        ),
        allocation_harness::trial(
            "uniform_publication_borrows_commit_routes",
            publication_costs::uniform_publication_borrows_commit_routes,
        ),
        allocation_harness::trial(
            "transport_status_fallback_skips_player_metadata",
            status_costs::transport_status_fallback_skips_player_metadata,
        ),
        allocation_harness::trial(
            "mesh_departure_skips_survivor_selection",
            departure_costs::mesh_departure_skips_survivor_selection,
        ),
        allocation_harness::trial(
            "observer_retry_reuses_owned_seated_batches",
            publication_costs::observer_retry_reuses_owned_seated_batches,
        ),
        allocation_harness::trial(
            "two_phase_batch_reserves_once",
            publication_costs::two_phase_batch_reserves_once,
        ),
        allocation_harness::trial(
            "transaction_reuses_owned_batches",
            publication_costs::transaction_reuses_owned_batches,
        ),
        allocation_harness::trial(
            "ice_composition_reserves_combined_list",
            setup_costs::ice_composition_reserves_combined_list,
        ),
        allocation_harness::trial("ready_toggle_reuses_membership", || {
            readiness_costs::readiness_reuses_membership(false)
        }),
        allocation_harness::trial("lobby_entry_reuses_membership", || {
            readiness_costs::readiness_reuses_membership(true)
        }),
        allocation_harness::trial(
            "membership_version_does_not_copy_capabilities",
            admission_costs::membership_version_does_not_copy_capabilities,
        ),
        allocation_harness::trial(
            "sticky_admission_does_not_copy_capabilities",
            admission_costs::sticky_admission_does_not_copy_capabilities,
        ),
        allocation_harness::trial(
            "legacy_token_gate_does_not_copy_capabilities",
            admission_costs::legacy_token_gate_does_not_copy_capabilities,
        ),
        allocation_harness::trial(
            "ice_eligibility_does_not_copy_capabilities",
            setup_costs::ice_eligibility_does_not_copy_capabilities,
        ),
        allocation_harness::trial(
            "session_selection_keeps_capabilities_inline",
            setup_costs::session_selection_keeps_capabilities_inline,
        ),
        allocation_harness::trial(
            "unready_start_skips_player_metadata",
            lobby_costs::unready_start_skips_player_metadata,
        ),
        allocation_harness::trial(
            "ready_snapshot_scales_with_ready_members",
            lobby_costs::ready_snapshot_scales_with_ready_members,
        ),
        allocation_harness::trial(
            "app_identity_lookup_does_not_copy_text",
            lobby_costs::app_identity_lookup_does_not_copy_text,
        ),
        allocation_harness::trial(
            "host_guest_departure_skips_survivor_selection",
            departure_costs::host_guest_departure_skips_survivor_selection,
        ),
        allocation_harness::trial(
            "room_event_admission_cost",
            control_costs::room_event_admission_cost,
        ),
        allocation_harness::trial(
            "direct_repeat_cost_does_not_copy_other_members",
            control_costs::direct_repeat_cost_does_not_copy_other_members,
        ),
        allocation_harness::trial(
            "transport_report_delivery_cost",
            live_p2p_allocations::transport_report_delivery_cost,
        ),
        allocation_harness::trial(
            "signaling_cost_is_independent_of_room_size",
            live_p2p_allocations::signaling_cost_is_independent_of_room_size,
        ),
        allocation_harness::trial(
            "roster_snapshot_reserves_once",
            live_p2p_allocations::roster_snapshot_reserves_once,
        ),
        allocation_harness::trial(
            "game_start_shares_configuration",
            live_p2p_allocations::game_start_shares_configuration,
        ),
        allocation_harness::trial("finalized_join_shares_recovery_player_name", || {
            finalized_membership_reuses_owned_room_snapshots(false, true, false)
        }),
        allocation_harness::trial("reconnect_claim_shares_retained_player_name", || {
            finalized_membership_reuses_owned_room_snapshots(true, true, false)
        }),
        allocation_harness::trial("host_departure_skips_legacy_player_metadata", || {
            live_p2p_allocations::live_host_operation_does_not_copy_legacy_metadata(false, false)
        }),
        allocation_harness::trial("direct_refresh_skips_legacy_player_metadata", || {
            live_p2p_allocations::live_host_operation_does_not_copy_legacy_metadata(true, false)
        }),
        allocation_harness::trial(
            "session_plans_do_not_copy_custom_connection_metadata",
            session_plans_do_not_copy_custom_connection_metadata,
        ),
        allocation_harness::trial("finalized_join_skips_unused_direct_endpoints", || {
            finalized_membership_reuses_owned_room_snapshots(false, false, true)
        }),
        allocation_harness::trial("finalized_join_reuses_owned_room_snapshots", || {
            finalized_membership_reuses_owned_room_snapshots(false, false, false)
        }),
        allocation_harness::trial("finalized_reconnect_reuses_owned_room_snapshot", || {
            finalized_membership_reuses_owned_room_snapshots(true, false, false)
        }),
    ]);
}
