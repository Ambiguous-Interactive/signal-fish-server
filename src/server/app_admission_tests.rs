use super::*;
use crate::config::{
    AppRegistrationEntry, CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig,
    SessionConfig, TransportSecurityConfig, TurnConfig,
};
use crate::database::{DatabaseConfig, InMemoryDatabase};
use crate::distributed::InMemoryDistributedLock;
use crate::protocol::{ErrorCode, PlayerInfo, ServerMessage};
use std::collections::HashSet;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

const APP_A: &str = "app-a";
const APP_B: &str = "app-b";

fn app_entry(
    app_id: &str,
    max_rooms: Option<u32>,
    max_players_per_room: Option<u8>,
) -> AppRegistrationEntry {
    AppRegistrationEntry {
        app_id: app_id.to_string(),
        app_name: app_id.to_string(),
        max_rooms,
        max_players_per_room,
        rate_limit_per_minute: None,
    }
}

async fn create_server(
    app_id_allowlist_enabled: bool,
    apps: Vec<AppRegistrationEntry>,
) -> Arc<EnhancedGameServer> {
    let config = ServerConfig {
        app_id_allowlist_enabled,
        max_connections_per_ip: 100,
        ..ServerConfig::default()
    };
    EnhancedGameServer::new(
        config,
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        SessionConfig::default(),
        TurnConfig::default(),
        DatabaseConfig::InMemory,
        MetricsConfig::default(),
        CoordinationConfig::default(),
        TransportSecurityConfig::default(),
        apps,
    )
    .await
    .expect("construct app-admission test server")
}

async fn connect_as(
    server: &Arc<EnhancedGameServer>,
    app_id: &str,
    port: u16,
) -> (PlayerId, mpsc::Receiver<Arc<ServerMessage>>) {
    let (sender, receiver) = mpsc::channel(32);
    let player_id = server
        .register_client(
            sender,
            format!("127.0.0.1:{port}")
                .parse()
                .expect("parse test client address"),
        )
        .await
        .expect("register test client");
    let app_context = server
        .app_id_allowlist
        .resolve_app_id(app_id, std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
        .await
        .expect("test application is configured");
    server.set_client_app_context(&player_id, app_context);
    server.set_client_protocol(
        &player_id,
        NegotiatedProtocol {
            version: 3,
            ..NegotiatedProtocol::default()
        },
    );
    (player_id, receiver)
}

async fn join_room(
    server: &Arc<EnhancedGameServer>,
    player_id: &PlayerId,
    game_name: &str,
    room_code: Option<&str>,
    player_name: &str,
    max_players: u8,
) {
    server
        .handle_join_room(
            player_id,
            game_name.to_string(),
            room_code.map(str::to_string),
            player_name.to_string(),
            Some(max_players),
            Some(true),
            None,
        )
        .await;
}

async fn receive(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>) -> Arc<ServerMessage> {
    timeout(Duration::from_secs(2), receiver.recv())
        .await
        .expect("server response timed out")
        .expect("server response channel closed")
}

fn joined_room(message: &ServerMessage) -> (RoomId, String, Option<String>) {
    let ServerMessage::RoomJoined(payload) = message else {
        panic!("expected RoomJoined, got {message:?}");
    };
    (
        payload.room_id,
        payload.room_code.clone(),
        payload.reconnection_token.clone(),
    )
}

fn assert_join_failed(message: &ServerMessage, expected: ErrorCode) {
    let ServerMessage::RoomJoinFailed { error_code, .. } = message else {
        panic!("expected RoomJoinFailed, got {message:?}");
    };
    assert_eq!(*error_code, Some(expected));
}

#[tokio::test]
async fn generated_code_retry_preserves_application_ownership_and_quota() {
    let server = create_server(
        true,
        vec![
            app_entry(APP_A, Some(1), Some(8)),
            app_entry(APP_B, Some(10), Some(8)),
        ],
    )
    .await;
    let app_b = server
        .app_id_allowlist
        .resolve_app_id(APP_B, std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
        .await
        .expect("app B is configured");
    let occupied = server
        .database
        .create_room(
            "owned-collision".to_string(),
            Some("TAKEN1".to_string()),
            8,
            true,
            PlayerId::new_v4(),
            "relay".to_string(),
            "default".to_string(),
            Some(app_b.id),
        )
        .await
        .expect("occupied app-B room should be created");
    server.script_room_codes_for_test(["TAKEN1", "FRESH1"]);

    let (creator, mut creator_rx) = connect_as(&server, APP_A, 40990).await;
    join_room(&server, &creator, "owned-collision", None, "Creator", 8).await;
    let created = receive(&mut creator_rx).await;
    let (created_id, created_code, _) = joined_room(created.as_ref());
    assert_eq!(created_code, "FRESH1");
    assert_ne!(created_id, occupied.id);

    let app_a = server
        .client_app_id(&creator)
        .expect("handshake-complete creator retains app identity");
    let created_room = server
        .database
        .get_room_by_id(&created_id)
        .await
        .expect("created room lookup should succeed")
        .expect("created room should remain persisted");
    assert_eq!(created_room.application_id, Some(app_a));
    assert_eq!(
        server
            .database
            .get_application_room_count(&app_a)
            .await
            .expect("app-A room count should succeed"),
        1,
        "the collision must not consume an application quota slot"
    );
    assert_eq!(
        server
            .database
            .get_application_room_count(&app_b.id)
            .await
            .expect("app-B room count should succeed"),
        1,
        "the occupied room must remain owned by app B"
    );
}

#[tokio::test]
async fn app_bound_room_owner_gates_seated_spectator_and_reconnect_admission() {
    let server = create_server(
        true,
        vec![
            app_entry(APP_A, Some(10), Some(8)),
            app_entry(APP_B, Some(10), Some(8)),
        ],
    )
    .await;

    let (creator, mut creator_rx) = connect_as(&server, APP_A, 41001).await;
    join_room(&server, &creator, "owner-game", None, "Creator", 8).await;
    let creator_joined = receive(&mut creator_rx).await;
    let (room_id, room_code, reconnect_token) = joined_room(creator_joined.as_ref());
    let reconnect_token = reconnect_token.expect("reconnection token is issued");

    // Persistence, not the process cache, must authorize a same-app join after
    // a restart-shaped cache loss.
    server.room_applications.clear();
    let (same_app, mut same_app_rx) = connect_as(&server, APP_A, 41002).await;
    join_room(
        &server,
        &same_app,
        "owner-game",
        Some(&room_code),
        "SameApp",
        8,
    )
    .await;
    let same_app_result = receive(&mut same_app_rx).await;
    assert!(matches!(
        same_app_result.as_ref(),
        ServerMessage::RoomJoined(_)
    ));

    server.room_applications.clear();
    let (other_app, mut other_app_rx) = connect_as(&server, APP_B, 41003).await;
    join_room(
        &server,
        &other_app,
        "owner-game",
        Some(&room_code),
        "OtherApp",
        8,
    )
    .await;
    assert_join_failed(
        receive(&mut other_app_rx).await.as_ref(),
        ErrorCode::RoomNotFound,
    );

    server.room_applications.clear();
    let (same_spectator, mut same_spectator_rx) = connect_as(&server, APP_A, 41004).await;
    server
        .handle_join_as_spectator(
            &same_spectator,
            "owner-game".to_string(),
            room_code.clone(),
            "SameSpectator".to_string(),
        )
        .await;
    assert!(matches!(
        receive(&mut same_spectator_rx).await.as_ref(),
        ServerMessage::SpectatorJoined(_)
    ));

    server.room_applications.clear();
    let (other_spectator, mut other_spectator_rx) = connect_as(&server, APP_B, 41005).await;
    server
        .handle_join_as_spectator(
            &other_spectator,
            "owner-game".to_string(),
            room_code,
            "OtherSpectator".to_string(),
        )
        .await;
    let other_spectator_result = receive(&mut other_spectator_rx).await;
    let ServerMessage::SpectatorJoinFailed { error_code, .. } = other_spectator_result.as_ref()
    else {
        panic!("expected SpectatorJoinFailed, got {other_spectator_result:?}");
    };
    assert_eq!(*error_code, Some(ErrorCode::RoomNotFound));

    // Disconnect the creator, then prove a wrong-app reconnect is rejected
    // without consuming the token. The original app can still claim it.
    server.unregister_client(&creator).await;
    server.room_applications.clear();
    let (wrong_reconnect, mut wrong_reconnect_rx) = connect_as(&server, APP_B, 41006).await;
    assert!(
        !server
            .handle_reconnect(&wrong_reconnect, &creator, &room_id, &reconnect_token)
            .await
    );
    let wrong_reconnect_result = receive(&mut wrong_reconnect_rx).await;
    let ServerMessage::ReconnectionFailed { error_code, .. } = wrong_reconnect_result.as_ref()
    else {
        panic!("expected ReconnectionFailed, got {wrong_reconnect_result:?}");
    };
    assert_eq!(*error_code, ErrorCode::RoomNotFound);

    let (right_reconnect, _right_reconnect_rx) = connect_as(&server, APP_A, 41007).await;
    assert!(
        server
            .handle_reconnect(&right_reconnect, &creator, &room_id, &reconnect_token)
            .await
    );
}

#[tokio::test]
async fn successful_reconnect_but_not_spectator_adopts_pending_ownership_claim() {
    let server = create_server(true, vec![app_entry(APP_A, Some(10), Some(8))]).await;
    let (creator, mut creator_rx) = connect_as(&server, APP_A, 41501).await;
    let application_id = server
        .client_app_id(&creator)
        .expect("app context attached");
    join_room(&server, &creator, "adopt-reconnect", None, "Creator", 8).await;
    let (room_id, room_code, reconnect_token) =
        joined_room(receive(&mut creator_rx).await.as_ref());
    let reconnect_token = reconnect_token.expect("reconnection token is issued");

    let failed_player = PlayerId::new_v4();
    server
        .database
        .add_player_to_room(
            &room_id,
            PlayerInfo {
                id: failed_player,
                name: "FailedClaimant".to_string(),
                is_authority: false,
                is_ready: false,
                connected_at: chrono::Utc::now(),
                connection_info: None,
                epoch: None,
                seq: None,
                region_id: "test".to_string(),
            },
        )
        .await
        .expect("insert failed claimant")
        .then_some(())
        .expect("room has capacity");
    server.pending_durable_player_detaches.insert(
        (room_id, failed_player),
        Some(PendingApplicationClaimRollback { application_id }),
    );

    let (spectator, mut spectator_rx) = connect_as(&server, APP_A, 41502).await;
    server
        .handle_join_as_spectator(
            &spectator,
            "adopt-reconnect".to_string(),
            room_code,
            "Spectator".to_string(),
        )
        .await;
    assert!(matches!(
        receive(&mut spectator_rx).await.as_ref(),
        ServerMessage::SpectatorJoined(_)
    ));
    assert!(server
        .pending_durable_player_detaches
        .get(&(room_id, failed_player))
        .is_some_and(|entry| entry.value().is_some()));

    server.unregister_client(&creator).await;
    let (current, _current_rx) = connect_as(&server, APP_A, 41503).await;
    assert!(
        server
            .handle_reconnect(&current, &creator, &room_id, &reconnect_token)
            .await
    );
    assert!(server
        .pending_durable_player_detaches
        .get(&(room_id, failed_player))
        .is_some_and(|entry| entry.value().is_none()));
}

/// A rejected reconnect that cannot undo its own restored membership must
/// re-queue the detach carrying the ownership rollback the retry it displaced
/// still owed. Re-queueing a bare detach would leave the room's application
/// claim held forever: the repair would delete the row and never clear it.
#[tokio::test]
async fn rejected_reconnect_requeues_the_ownership_rollback_it_inherited() {
    let server = create_server(true, vec![app_entry(APP_A, Some(10), Some(8))]).await;
    let database = server
        .database
        .as_any()
        .downcast_ref::<InMemoryDatabase>()
        .expect("in-memory test database");
    let (creator, mut creator_rx) = connect_as(&server, APP_A, 41601).await;
    let application_id = server
        .client_app_id(&creator)
        .expect("app context attached");
    join_room(&server, &creator, "requeue-rollback", None, "Creator", 8).await;
    let (room_id, _room_code, reconnect_token) =
        joined_room(receive(&mut creator_rx).await.as_ref());
    let reconnect_token = reconnect_token.expect("reconnection token is issued");

    // The disconnect removes the row. A detach queued behind it still owes the
    // ownership rollback an earlier repair could not perform.
    server.unregister_client(&creator).await;
    server.pending_durable_player_detaches.insert(
        (room_id, creator),
        Some(PendingApplicationClaimRollback { application_id }),
    );

    // The reconnect restores the membership and takes that retry over, then
    // fails on an undeliverable baseline — and its own rollback removal fails
    // too, so the row it restored survives.
    let (current, current_rx) = connect_as(&server, APP_A, 41602).await;
    drop(current_rx);
    database.fail_remove_player_from_room_for_test(true);
    assert!(
        !server
            .handle_reconnect(&current, &creator, &room_id, &reconnect_token)
            .await
    );
    database.fail_remove_player_from_room_for_test(false);

    let queued = server
        .pending_durable_player_detaches
        .get(&(room_id, creator))
        .expect("a rejection that could not remove the row owes a detach retry");
    assert_eq!(
        queued
            .value()
            .as_ref()
            .map(|rollback| rollback.application_id),
        Some(application_id),
        "the re-queued retry must keep the ownership rollback it still owes"
    );
}

#[tokio::test]
async fn legacy_room_claims_only_on_app_bound_seated_admission() {
    let server = create_server(true, vec![app_entry(APP_A, Some(10), Some(8))]).await;
    let legacy_creator = PlayerId::new_v4();
    let legacy_room = server
        .database
        .create_room(
            "legacy-game".to_string(),
            Some("LEGACY".to_string()),
            8,
            true,
            legacy_creator,
            "udp".to_string(),
            "test".to_string(),
            None,
        )
        .await
        .expect("create legacy unowned room");

    let (spectator, mut spectator_rx) = connect_as(&server, APP_A, 42001).await;
    server
        .handle_join_as_spectator(
            &spectator,
            "legacy-game".to_string(),
            "LEGACY".to_string(),
            "Spectator".to_string(),
        )
        .await;
    assert!(matches!(
        receive(&mut spectator_rx).await.as_ref(),
        ServerMessage::SpectatorJoined(_)
    ));
    assert_eq!(
        server
            .database
            .get_room_by_id(&legacy_room.id)
            .await
            .expect("read legacy room")
            .expect("legacy room remains")
            .application_id,
        None,
        "a spectator must never establish room ownership"
    );

    let (seated, mut seated_rx) = connect_as(&server, APP_A, 42002).await;
    let app_id = server.client_app_id(&seated).expect("app context attached");
    join_room(
        &server,
        &seated,
        "legacy-game",
        Some("LEGACY"),
        "Claimer",
        8,
    )
    .await;
    assert!(matches!(
        receive(&mut seated_rx).await.as_ref(),
        ServerMessage::RoomJoined(_)
    ));
    assert_eq!(
        server
            .database
            .get_room_by_id(&legacy_room.id)
            .await
            .expect("read claimed room")
            .expect("claimed room remains")
            .application_id,
        Some(app_id)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_room_claims_share_the_atomic_application_room_cap() {
    let server = create_server(true, vec![app_entry(APP_A, Some(1), Some(8))]).await;
    let database = server
        .database
        .as_any()
        .downcast_ref::<InMemoryDatabase>()
        .expect("in-memory test database");
    for (game, code) in [("legacy-cap-a", "LCAPA1"), ("legacy-cap-b", "LCAPB1")] {
        server
            .database
            .create_room(
                game.to_string(),
                Some(code.to_string()),
                8,
                true,
                PlayerId::new_v4(),
                "udp".to_string(),
                "test".to_string(),
                None,
            )
            .await
            .expect("create legacy room");
    }
    let (first, mut first_rx) = connect_as(&server, APP_A, 42101).await;
    let (second, mut second_rx) = connect_as(&server, APP_A, 42102).await;
    let app_id = server.client_app_id(&first).expect("app context attached");

    database.pause_next_get_application_room_count_for_test();
    let first_server = Arc::clone(&server);
    let first_task = tokio::spawn(async move {
        join_room(
            &first_server,
            &first,
            "legacy-cap-a",
            Some("LCAPA1"),
            "First",
            8,
        )
        .await;
    });
    database
        .wait_for_paused_get_application_room_count_for_test()
        .await;
    let second_server = Arc::clone(&server);
    let second_task = tokio::spawn(async move {
        join_room(
            &second_server,
            &second,
            "legacy-cap-b",
            Some("LCAPB1"),
            "Second",
            8,
        )
        .await;
    });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(database.get_application_room_count_calls_for_test(), 1);
    assert!(
        !second_task.is_finished(),
        "second claim must wait on app lock"
    );
    database.release_paused_get_application_room_count_for_test();
    first_task.await.expect("first claim task completes");
    second_task.await.expect("second claim task completes");
    let first_result = receive(&mut first_rx).await;
    let second_result = receive(&mut second_rx).await;
    match (first_result.as_ref(), second_result.as_ref()) {
        (ServerMessage::RoomJoined(_), ServerMessage::RoomJoinFailed { error_code, .. })
        | (ServerMessage::RoomJoinFailed { error_code, .. }, ServerMessage::RoomJoined(_)) => {
            assert_eq!(*error_code, Some(ErrorCode::MaxRoomsPerGameExceeded));
        }
        outcomes => panic!("expected one legacy claim and one app-cap denial, got {outcomes:?}"),
    }
    assert_eq!(
        server
            .database
            .get_application_room_count(&app_id)
            .await
            .expect("count claimed legacy rooms"),
        1
    );
}

#[tokio::test]
async fn unpublished_legacy_admission_rolls_back_ownership_claim() {
    let server = create_server(true, vec![app_entry(APP_A, Some(10), Some(8))]).await;
    let database = server
        .database
        .as_any()
        .downcast_ref::<InMemoryDatabase>()
        .expect("in-memory test database");
    let legacy_creator = PlayerId::new_v4();
    let legacy_room = server
        .database
        .create_room(
            "rollback-game".to_string(),
            Some("ROLLBK".to_string()),
            8,
            true,
            legacy_creator,
            "udp".to_string(),
            "test".to_string(),
            None,
        )
        .await
        .expect("create legacy rollback room");
    let (joiner, joiner_rx) = connect_as(&server, APP_A, 42501).await;
    drop(joiner_rx);
    database.fail_remove_player_from_room_for_test(true);
    database.fail_clear_room_application_id_for_test(true);

    join_room(
        &server,
        &joiner,
        "rollback-game",
        Some("ROLLBK"),
        "Unpublished",
        8,
    )
    .await;
    let room_during_detach_failure = server
        .database
        .get_room_by_id(&legacy_room.id)
        .await
        .expect("read rollback room")
        .expect("rollback room remains");
    assert!(room_during_detach_failure.application_id.is_some());
    assert!(room_during_detach_failure.players.contains_key(&joiner));

    database.fail_remove_player_from_room_for_test(false);
    assert!(database
        .remove_player_from_room(&legacy_room.id, &legacy_creator)
        .await
        .expect("remove unrelated legacy occupant")
        .is_some());
    assert_eq!(server.cleanup_pending_durable_player_detaches().await, 0);
    let room_during_clear_failure = server
        .database
        .get_room_by_id(&legacy_room.id)
        .await
        .expect("read legacy room during clear failure")
        .expect("legacy room remains");
    assert!(room_during_clear_failure.application_id.is_some());
    assert!(!room_during_clear_failure.players.contains_key(&joiner));

    database.fail_clear_room_application_id_for_test(false);
    assert_eq!(server.cleanup_pending_durable_player_detaches().await, 1);
    let repaired_room = server
        .database
        .get_room_by_id(&legacy_room.id)
        .await
        .expect("read repaired legacy room")
        .expect("legacy room remains");
    assert_eq!(repaired_room.application_id, None);
    assert!(!repaired_room.players.contains_key(&joiner));
}

#[tokio::test]
async fn delayed_legacy_rollback_preserves_a_later_published_admission() {
    let server = create_server(true, vec![app_entry(APP_A, Some(10), Some(8))]).await;
    let database = server
        .database
        .as_any()
        .downcast_ref::<InMemoryDatabase>()
        .expect("in-memory test database");
    let legacy_room = server
        .database
        .create_room(
            "rollback-adoption".to_string(),
            Some("ADOPT1".to_string()),
            8,
            true,
            PlayerId::new_v4(),
            "udp".to_string(),
            "test".to_string(),
            None,
        )
        .await
        .expect("create legacy adoption room");
    let (failed_joiner, failed_rx) = connect_as(&server, APP_A, 42521).await;
    drop(failed_rx);
    database.fail_remove_player_from_room_for_test(true);
    join_room(
        &server,
        &failed_joiner,
        "rollback-adoption",
        Some("ADOPT1"),
        "Failed",
        8,
    )
    .await;

    let (adopter, mut adopter_rx) = connect_as(&server, APP_A, 42522).await;
    join_room(
        &server,
        &adopter,
        "rollback-adoption",
        Some("ADOPT1"),
        "Adopter",
        8,
    )
    .await;
    assert!(matches!(
        receive(&mut adopter_rx).await.as_ref(),
        ServerMessage::RoomJoined(_)
    ));

    database.fail_remove_player_from_room_for_test(false);
    server.leave_room(&adopter).await;
    assert!(!server
        .database
        .get_room_by_id(&legacy_room.id)
        .await
        .expect("read room after adopter leaves")
        .expect("legacy room remains")
        .players
        .contains_key(&adopter));
    assert_eq!(server.cleanup_pending_durable_player_detaches().await, 1);
    let repaired_room = server
        .database
        .get_room_by_id(&legacy_room.id)
        .await
        .expect("read adopted legacy room")
        .expect("adopted legacy room remains");
    assert!(repaired_room.application_id.is_some());
    assert!(!repaired_room.players.contains_key(&adopter));
    assert!(!repaired_room.players.contains_key(&failed_joiner));
}

#[tokio::test]
async fn deleted_room_terminates_pending_ownership_rollback() {
    let server = create_server(true, vec![app_entry(APP_A, Some(10), Some(8))]).await;
    let database = server
        .database
        .as_any()
        .downcast_ref::<InMemoryDatabase>()
        .expect("in-memory test database");
    let legacy_room = server
        .database
        .create_room(
            "deleted-rollback".to_string(),
            Some("DELROL".to_string()),
            8,
            true,
            PlayerId::new_v4(),
            "udp".to_string(),
            "test".to_string(),
            None,
        )
        .await
        .expect("create legacy rollback room");
    let (joiner, joiner_rx) = connect_as(&server, APP_A, 42531).await;
    drop(joiner_rx);
    database.fail_remove_player_from_room_for_test(true);
    join_room(
        &server,
        &joiner,
        "deleted-rollback",
        Some("DELROL"),
        "Failed",
        8,
    )
    .await;
    assert!(server
        .database
        .delete_room(&legacy_room.id)
        .await
        .expect("delete pending rollback room"));

    database.fail_remove_player_from_room_for_test(false);
    assert_eq!(server.cleanup_pending_durable_player_detaches().await, 1);
    assert_eq!(server.cleanup_pending_durable_player_detaches().await, 0);
}

#[tokio::test]
async fn unpublished_room_creation_rolls_back_room_and_frees_application_capacity() {
    let server = create_server(true, vec![app_entry(APP_A, Some(1), Some(8))]).await;
    let (failed_creator, failed_rx) = connect_as(&server, APP_A, 42511).await;
    let app_id = server
        .client_app_id(&failed_creator)
        .expect("app context attached");
    drop(failed_rx);

    join_room(
        &server,
        &failed_creator,
        "failed-create",
        None,
        "Undeliverable",
        8,
    )
    .await;
    assert_eq!(
        server
            .database
            .get_application_room_count(&app_id)
            .await
            .expect("count rooms after unpublished creation"),
        0,
        "a room the creator never observes must not consume its app quota"
    );

    let (retry, mut retry_rx) = connect_as(&server, APP_A, 42512).await;
    join_room(&server, &retry, "failed-create", None, "Retry", 8).await;
    assert!(matches!(
        receive(&mut retry_rx).await.as_ref(),
        ServerMessage::RoomJoined(_)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_room_claim_is_atomic_between_applications() {
    let server = create_server(
        true,
        vec![
            app_entry(APP_A, Some(10), Some(8)),
            app_entry(APP_B, Some(10), Some(8)),
        ],
    )
    .await;
    let legacy_room = server
        .database
        .create_room(
            "claim-race".to_string(),
            Some("CLAIMR".to_string()),
            8,
            true,
            PlayerId::new_v4(),
            "udp".to_string(),
            "test".to_string(),
            None,
        )
        .await
        .expect("create claim-race room");
    let (app_a, mut app_a_rx) = connect_as(&server, APP_A, 42601).await;
    let (app_b, mut app_b_rx) = connect_as(&server, APP_B, 42602).await;
    let app_a_id = server.client_app_id(&app_a).expect("app A attached");
    let app_b_id = server.client_app_id(&app_b).expect("app B attached");

    tokio::join!(
        join_room(&server, &app_a, "claim-race", Some("CLAIMR"), "AppA", 8,),
        join_room(&server, &app_b, "claim-race", Some("CLAIMR"), "AppB", 8,),
    );
    let app_a_result = receive(&mut app_a_rx).await;
    let app_b_result = receive(&mut app_b_rx).await;
    let expected_owner = match (app_a_result.as_ref(), app_b_result.as_ref()) {
        (ServerMessage::RoomJoined(_), ServerMessage::RoomJoinFailed { error_code, .. }) => {
            assert_eq!(*error_code, Some(ErrorCode::RoomNotFound));
            app_a_id
        }
        (ServerMessage::RoomJoinFailed { error_code, .. }, ServerMessage::RoomJoined(_)) => {
            assert_eq!(*error_code, Some(ErrorCode::RoomNotFound));
            app_b_id
        }
        outcomes => {
            panic!("expected one claim winner and one hidden-room denial, got {outcomes:?}")
        }
    };
    assert_eq!(
        server
            .database
            .get_room_by_id(&legacy_room.id)
            .await
            .expect("read claim-race room")
            .expect("claim-race room remains")
            .application_id,
        Some(expected_owner)
    );
}

#[tokio::test]
async fn open_policy_room_creation_ignores_default_app_context() {
    let server = create_server(false, Vec::new()).await;
    let (creator, mut creator_rx) = connect_as(&server, "public-label", 43001).await;
    join_room(&server, &creator, "public-game", None, "Creator", 4).await;
    let result = receive(&mut creator_rx).await;
    let (room_id, _, _) = joined_room(result.as_ref());
    assert_eq!(
        server
            .database
            .get_room_by_id(&room_id)
            .await
            .expect("read public room")
            .expect("public room exists")
            .application_id,
        None
    );
}

#[tokio::test]
async fn application_player_cap_rejects_oversized_creation_and_lowered_legacy_capacity() {
    let server = create_server(true, vec![app_entry(APP_A, Some(10), Some(2))]).await;
    let (oversized, mut oversized_rx) = connect_as(&server, APP_A, 44001).await;
    join_room(&server, &oversized, "capacity-game", None, "Oversized", 3).await;
    assert_join_failed(
        receive(&mut oversized_rx).await.as_ref(),
        ErrorCode::InvalidMaxPlayers,
    );

    join_room(&server, &oversized, "capacity-game", None, "Creator", 2).await;
    let created = receive(&mut oversized_rx).await;
    let (room_id, room_code, _) = joined_room(created.as_ref());

    let (second, mut second_rx) = connect_as(&server, APP_A, 44002).await;
    join_room(
        &server,
        &second,
        "capacity-game",
        Some(&room_code),
        "Second",
        8,
    )
    .await;
    assert!(matches!(
        receive(&mut second_rx).await.as_ref(),
        ServerMessage::RoomJoined(_)
    ));

    // Model a restart with a lower app cap by attaching the newly loaded app
    // policy to a fresh connection. Existing members remain; new seats close.
    let (third, mut third_rx) = connect_as(&server, APP_A, 44003).await;
    let mut lowered_policy = server
        .client_app_context(&third)
        .expect("app policy attached");
    lowered_policy.max_players_per_room = Some(1);
    server.set_client_app_context(&third, lowered_policy);
    join_room(
        &server,
        &third,
        "capacity-game",
        Some(&room_code),
        "Third",
        8,
    )
    .await;
    assert_join_failed(receive(&mut third_rx).await.as_ref(), ErrorCode::RoomFull);
    assert_eq!(
        server
            .database
            .get_room_by_id(&room_id)
            .await
            .expect("read capacity room")
            .expect("capacity room exists")
            .players
            .len(),
        2,
        "lowering an app cap must not eject existing members"
    );
}

#[tokio::test]
async fn reconnect_respects_current_application_player_cap_without_consuming_token() {
    let server = create_server(true, vec![app_entry(APP_A, Some(10), Some(2))]).await;
    let (original, mut original_rx) = connect_as(&server, APP_A, 44501).await;
    join_room(&server, &original, "reconnect-cap", None, "Original", 2).await;
    let joined = receive(&mut original_rx).await;
    let (room_id, room_code, reconnect_token) = joined_room(joined.as_ref());
    let reconnect_token = reconnect_token.expect("reconnection token is issued");

    server.unregister_client(&original).await;

    let (replacement, mut replacement_rx) = connect_as(&server, APP_A, 44502).await;
    join_room(
        &server,
        &replacement,
        "reconnect-cap",
        Some(&room_code),
        "Replacement",
        2,
    )
    .await;
    assert!(matches!(
        receive(&mut replacement_rx).await.as_ref(),
        ServerMessage::RoomJoined(_)
    ));

    let (blocked, mut blocked_rx) = connect_as(&server, APP_A, 44503).await;
    let mut lowered_policy = server
        .client_app_context(&blocked)
        .expect("app policy attached");
    lowered_policy.max_players_per_room = Some(1);
    server.set_client_app_context(&blocked, lowered_policy);
    assert!(
        !server
            .handle_reconnect(&blocked, &original, &room_id, &reconnect_token)
            .await
    );
    let blocked_result = receive(&mut blocked_rx).await;
    let ServerMessage::ReconnectionFailed { error_code, .. } = blocked_result.as_ref() else {
        panic!("expected ReconnectionFailed, got {blocked_result:?}");
    };
    assert_eq!(*error_code, ErrorCode::RoomFull);

    server.unregister_client(&replacement).await;

    let (retry, _retry_rx) = connect_as(&server, APP_A, 44504).await;
    let mut lowered_policy = server
        .client_app_context(&retry)
        .expect("app policy attached");
    lowered_policy.max_players_per_room = Some(1);
    server.set_client_app_context(&retry, lowered_policy);
    assert!(
        server
            .handle_reconnect(&retry, &original, &room_id, &reconnect_token)
            .await,
        "a cap denial must release rather than consume the reconnection token"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn application_room_cap_is_atomic_across_games_and_independent_between_apps() {
    let server = create_server(
        true,
        vec![
            app_entry(APP_A, Some(1), Some(8)),
            app_entry(APP_B, Some(1), Some(8)),
        ],
    )
    .await;
    let database = server
        .database
        .as_any()
        .downcast_ref::<InMemoryDatabase>()
        .expect("in-memory test database");
    let (app_a_one, mut app_a_one_rx) = connect_as(&server, APP_A, 45001).await;
    let (app_a_two, mut app_a_two_rx) = connect_as(&server, APP_A, 45002).await;

    database.pause_next_get_application_room_count_for_test();
    let first_server = Arc::clone(&server);
    let first_task = tokio::spawn(async move {
        join_room(&first_server, &app_a_one, "game-one", None, "One", 8).await;
    });
    database
        .wait_for_paused_get_application_room_count_for_test()
        .await;
    let second_server = Arc::clone(&server);
    let second_task = tokio::spawn(async move {
        join_room(&second_server, &app_a_two, "game-two", None, "Two", 8).await;
    });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(database.get_application_room_count_calls_for_test(), 1);
    assert!(
        !second_task.is_finished(),
        "second create must wait on app lock"
    );
    database.release_paused_get_application_room_count_for_test();
    first_task.await.expect("first create task completes");
    second_task.await.expect("second create task completes");
    let first = receive(&mut app_a_one_rx).await;
    let second = receive(&mut app_a_two_rx).await;
    let (winning_room, winning_player, losing_player, losing_rx) =
        match (first.as_ref(), second.as_ref()) {
            (
                ServerMessage::RoomJoined(payload),
                ServerMessage::RoomJoinFailed { error_code, .. },
            ) => {
                assert_eq!(*error_code, Some(ErrorCode::MaxRoomsPerGameExceeded));
                (payload.room_id, app_a_one, app_a_two, &mut app_a_two_rx)
            }
            (
                ServerMessage::RoomJoinFailed { error_code, .. },
                ServerMessage::RoomJoined(payload),
            ) => {
                assert_eq!(*error_code, Some(ErrorCode::MaxRoomsPerGameExceeded));
                (payload.room_id, app_a_two, app_a_one, &mut app_a_one_rx)
            }
            outcomes => panic!("expected one app-cap winner and one denial, got {outcomes:?}"),
        };
    let app_a_id = server
        .client_app_id(&losing_player)
        .expect("app A attached");
    assert_eq!(
        server
            .database
            .get_application_room_count(&app_a_id)
            .await
            .expect("count app A rooms"),
        1
    );

    let (app_b, mut app_b_rx) = connect_as(&server, APP_B, 45003).await;
    join_room(&server, &app_b, "game-three", None, "Independent", 8).await;
    assert!(matches!(
        receive(&mut app_b_rx).await.as_ref(),
        ServerMessage::RoomJoined(_)
    ));

    assert!(server
        .database
        .remove_player_from_room(&winning_room, &winning_player)
        .await
        .expect("remove winning creator")
        .is_some());
    let cleaned = server
        .database
        .cleanup_empty_rooms(chrono::Duration::zero(), &HashSet::new())
        .await
        .expect("clean empty application room");
    assert!(cleaned.contains(&winning_room));
    assert_eq!(server.prune_room_applications().await, 1);
    assert_eq!(server.room_application_id(&winning_room), None);
    assert_eq!(
        server
            .database
            .get_application_room_count(&app_a_id)
            .await
            .expect("count app rooms after cleanup"),
        0
    );
    join_room(&server, &losing_player, "game-four", None, "Retry", 8).await;
    assert!(matches!(
        receive(losing_rx).await.as_ref(),
        ServerMessage::RoomJoined(_)
    ));
}

#[tokio::test]
async fn application_room_count_failure_denies_creation_without_side_effects() {
    let server = create_server(true, vec![app_entry(APP_A, Some(1), Some(8))]).await;
    let database = server
        .database
        .as_any()
        .downcast_ref::<InMemoryDatabase>()
        .expect("in-memory test database");
    database.fail_get_application_room_count_for_test(true);

    let (creator, mut creator_rx) = connect_as(&server, APP_A, 46001).await;
    let app_id = server
        .client_app_id(&creator)
        .expect("app context attached");
    join_room(&server, &creator, "failure-game", None, "Creator", 8).await;
    assert_join_failed(
        receive(&mut creator_rx).await.as_ref(),
        ErrorCode::RoomCreationFailed,
    );
    database.fail_get_application_room_count_for_test(false);
    assert_eq!(
        server
            .database
            .get_application_room_count(&app_id)
            .await
            .expect("count rooms after failed create"),
        0
    );

    join_room(&server, &creator, "failure-game", None, "Creator", 8).await;
    assert!(matches!(
        receive(&mut creator_rx).await.as_ref(),
        ServerMessage::RoomJoined(_)
    ));
}

#[tokio::test]
async fn legacy_claim_persistence_failure_denies_admission_without_cache_authority() {
    let server = create_server(true, vec![app_entry(APP_A, Some(1), Some(8))]).await;
    let database = server
        .database
        .as_any()
        .downcast_ref::<InMemoryDatabase>()
        .expect("in-memory test database");
    let legacy_room = server
        .database
        .create_room(
            "claim-persistence".to_string(),
            Some("PRSIST".to_string()),
            8,
            true,
            PlayerId::new_v4(),
            "udp".to_string(),
            "test".to_string(),
            None,
        )
        .await
        .expect("create unowned room");
    database.fail_set_room_application_id_for_test(true);
    let (joiner, mut joiner_rx) = connect_as(&server, APP_A, 46501).await;

    join_room(
        &server,
        &joiner,
        "claim-persistence",
        Some("PRSIST"),
        "Joiner",
        8,
    )
    .await;
    assert_join_failed(
        receive(&mut joiner_rx).await.as_ref(),
        ErrorCode::RoomCreationFailed,
    );
    assert_eq!(
        server
            .database
            .get_room_by_id(&legacy_room.id)
            .await
            .expect("read unowned room")
            .expect("unowned room remains")
            .application_id,
        None
    );
    assert_eq!(server.room_application_id(&legacy_room.id), None);
}

#[tokio::test]
async fn application_room_lock_failure_denies_creation_without_side_effects() {
    let server = create_server(true, vec![app_entry(APP_A, Some(1), Some(8))]).await;
    let (creator, mut creator_rx) = connect_as(&server, APP_A, 47001).await;
    let app_id = server
        .client_app_id(&creator)
        .expect("app context attached");
    let lock_key = format!("application_room_cap:{app_id}");
    let distributed_lock = server
        .distributed_lock
        .as_any()
        .downcast_ref::<InMemoryDistributedLock>()
        .expect("in-memory test lock");
    distributed_lock.fail_acquire_for_test(Some(lock_key)).await;

    join_room(&server, &creator, "lock-failure", None, "Creator", 8).await;
    assert_join_failed(
        receive(&mut creator_rx).await.as_ref(),
        ErrorCode::RoomCreationFailed,
    );
    distributed_lock.fail_acquire_for_test(None).await;
    assert_eq!(
        server
            .database
            .get_application_room_count(&app_id)
            .await
            .expect("count rooms after lock failure"),
        0
    );

    join_room(&server, &creator, "lock-failure", None, "Creator", 8).await;
    assert!(matches!(
        receive(&mut creator_rx).await.as_ref(),
        ServerMessage::RoomJoined(_)
    ));
}
