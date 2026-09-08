use super::*;
use crate::config::{
    CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig, SessionConfig,
    TransportSecurityConfig, TurnConfig,
};
use crate::coordination::{CloseReason, ConnectionCloseSignal};
use crate::protocol::{PlayerId, RoomOperationId, RoomOperationResult, ServerMessage};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

async fn create_test_server_with(config: ServerConfig) -> Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
        config,
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        SessionConfig::default(),
        TurnConfig::default(),
        crate::database::DatabaseConfig::InMemory,
        MetricsConfig::default(),
        CoordinationConfig::default(),
        TransportSecurityConfig::default(),
        Vec::new(),
    )
    .await
    .expect("failed to construct test server")
}

async fn register_client_with_close_listener(
    server: &EnhancedGameServer,
    addr: SocketAddr,
) -> (
    PlayerId,
    mpsc::Receiver<Arc<ServerMessage>>,
    crate::coordination::ConnectionCloseListener,
) {
    let (sender, receiver) = mpsc::channel(8);
    let (close_signal, close_listener) = ConnectionCloseSignal::channel();
    let player_id = server
        .connection_manager
        .register_client(sender, close_signal, addr, server.instance_id)
        .await
        .expect("client registration succeeds");
    (player_id, receiver, close_listener)
}

async fn register_client(
    server: &EnhancedGameServer,
    addr: SocketAddr,
) -> (PlayerId, mpsc::Receiver<Arc<ServerMessage>>) {
    let (sender, receiver) = mpsc::channel(8);
    let player_id = server
        .connection_manager
        .register_client(
            sender,
            ConnectionCloseSignal::detached(),
            addr,
            server.instance_id,
        )
        .await
        .expect("client registration succeeds");
    (player_id, receiver)
}

async fn join_seated_player(
    server: &Arc<EnhancedGameServer>,
    player_id: &PlayerId,
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    room_code: &str,
    name: &str,
) {
    server
        .handle_join_room(
            player_id,
            "moderation-game".to_string(),
            Some(room_code.to_string()),
            name.to_string(),
            Some(4),
            Some(true),
            None,
            None,
        )
        .await;
    let joined = timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("join should finish")
        .expect("join should respond");
    assert!(
        matches!(joined.as_ref(), ServerMessage::RoomJoined(_)),
        "expected RoomJoined, got {joined:?}"
    );
}

/// Receive until the wanted message arrives, skipping interleaved
/// room-lifecycle broadcasts (e.g. `LobbyStateChanged` from earlier joins).
async fn recv_until(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    predicate: impl Fn(&ServerMessage) -> bool,
) -> Arc<ServerMessage> {
    loop {
        let message = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("message should arrive in time")
            .expect("channel stays open");
        if predicate(message.as_ref()) {
            return message;
        }
    }
}

fn operation_failed_code(message: &ServerMessage) -> ErrorCode {
    let ServerMessage::RoomOperationResult { result, .. } = message else {
        panic!("expected RoomOperationResult, got {message:?}");
    };
    let RoomOperationResult::OperationFailed { error_code, .. } = result.as_ref() else {
        panic!("expected OperationFailed, got {result:?}");
    };
    error_code
        .clone()
        .expect("moderation refusals carry an error code")
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn authority_kick_removes_seat_closes_target_and_reports_to_authority() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48130".parse().unwrap()).await;
    let (target, mut target_rx, target_close_listener) =
        register_client_with_close_listener(&server, "127.0.0.1:48131".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "KICK01", "host").await;
    join_seated_player(&server, &target, &mut target_rx, "KICK01", "guest").await;

    server
        .handle_kick_player_operation(&authority, RoomOperationId::new_v4(), target)
        .await;

    // The authority observes the roster delta and then the correlated success.
    let roster_delta = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::PlayerLeft { .. })
    })
    .await;
    let ServerMessage::PlayerLeft { player_id, .. } = roster_delta.as_ref() else {
        panic!("expected PlayerLeft for the kicked seat, got {roster_delta:?}");
    };
    assert_eq!(*player_id, target);

    let result = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    let ServerMessage::RoomOperationResult { result, .. } = result.as_ref() else {
        panic!("expected correlated result, got {result:?}");
    };
    let RoomOperationResult::PlayerKicked { player_id: kicked } = result.as_ref() else {
        panic!("expected PlayerKicked, got {result:?}");
    };
    assert_eq!(*kicked, target);

    // The kicked connection receives the farewell error and the 4007 close.
    let farewell = recv_until(&mut target_rx, |message| {
        matches!(message, ServerMessage::Error { .. })
    })
    .await;
    let ServerMessage::Error { error_code, .. } = farewell.as_ref() else {
        panic!("expected farewell Error, got {farewell:?}");
    };
    assert_eq!(*error_code, Some(ErrorCode::Kicked));
    assert_eq!(
        target_close_listener.requested_reason(),
        Some(CloseReason::Kicked),
        "a kicked seat must close with the dedicated 4007 reason"
    );

    // The seat is gone from routing and from durable room state.
    assert!(server.get_client_room(&target).await.is_none());
    let room = server
        .database
        .get_room("moderation-game", "KICK01")
        .await
        .expect("room lookup succeeds")
        .expect("room survives the kick");
    assert!(!room.players.contains_key(&target));
    assert!(room.players.contains_key(&authority));
    assert_eq!(
        room.authority_player,
        Some(authority),
        "kicking a non-authority member must not disturb the role"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn kick_refusals_are_classified_per_failure() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48132".parse().unwrap()).await;
    let (outsider, mut outsider_rx) =
        register_client(&server, "127.0.0.1:48133".parse().unwrap()).await;
    let (target, mut target_rx) =
        register_client(&server, "127.0.0.1:48134".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "KICK02", "host").await;
    join_seated_player(&server, &target, &mut target_rx, "KICK02", "guest").await;

    // The outsider is seated in its own unrelated room, so a kick attempt
    // exercises the authority refusal rather than a missing-room refusal.
    let outsider_room = server
        .database
        .create_room(
            "moderation-game".to_string(),
            Some("OTHERX".to_string()),
            4,
            // No authority in the outsider's room, so the outsider holds no
            // authority anywhere and the kick attempt must hit the
            // NotRoomAuthority refusal.
            false,
            outsider,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("outsider room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&outsider, outsider_room.id)
        .await;

    // Refusals are sent to the actor that attempted the kick. The outsider
    // attempt exercises the authority refusal; the authority attempts
    // exercise the target and self-kick refusals.
    let refusals = [
        (
            outsider,
            target,
            ErrorCode::NotRoomAuthority,
            "non-authority member must be refused",
        ),
        (
            authority,
            outsider,
            ErrorCode::KickTargetNotFound,
            "a target outside the room must be refused",
        ),
        (
            authority,
            authority,
            ErrorCode::InvalidInput,
            "self-kick must be refused",
        ),
    ];

    for (actor, kicked, expected, description) in refusals {
        server
            .handle_kick_player_operation(&actor, RoomOperationId::new_v4(), kicked)
            .await;
        let response = if actor == outsider {
            recv_until(&mut outsider_rx, |message| {
                matches!(message, ServerMessage::RoomOperationResult { .. })
            })
            .await
        } else {
            recv_until(&mut authority_rx, |message| {
                matches!(message, ServerMessage::RoomOperationResult { .. })
            })
            .await
        };
        assert_eq!(
            operation_failed_code(&response),
            expected,
            "{description}: got {response:?}"
        );
    }

    // None of the refusals removed any seat.
    let room = server
        .database
        .get_room("moderation-game", "KICK02")
        .await
        .expect("room lookup succeeds")
        .expect("room survives refusals");
    assert!(room.players.contains_key(&target));
    assert!(room.players.contains_key(&authority));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn regenerate_room_code_rotates_registry_and_drops_the_old_code() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48135".parse().unwrap()).await;
    join_seated_player(&server, &authority, &mut authority_rx, "OLDCOD", "host").await;
    server.script_room_codes_for_test(["NEWCOD"]);

    server
        .handle_regenerate_room_code_operation(&authority, RoomOperationId::new_v4())
        .await;

    let response = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    let ServerMessage::RoomOperationResult { result, .. } = response.as_ref() else {
        panic!("expected correlated result, got {response:?}");
    };
    let RoomOperationResult::RoomCodeRegenerated { room_code } = result.as_ref() else {
        panic!("expected RoomCodeRegenerated, got {result:?}");
    };
    assert_eq!(room_code, "NEWCOD");

    let old_lookup = server
        .database
        .get_room("moderation-game", "OLDCOD")
        .await
        .expect("old-code lookup succeeds");
    assert!(
        old_lookup.is_none(),
        "the old code must stop resolving to the room"
    );
    let room = server
        .database
        .get_room("moderation-game", "NEWCOD")
        .await
        .expect("new-code lookup succeeds")
        .expect("the new code resolves to the room");
    assert_eq!(room.code, "NEWCOD");
    assert!(
        room.players.contains_key(&authority),
        "rotation must not disturb membership"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn regenerate_retries_colliding_candidates_within_the_budget() {
    let server = create_test_server_with(ServerConfig::default()).await;
    // Pre-occupy the first scripted candidate in the same game namespace.
    server
        .database
        .create_room(
            "moderation-game".to_string(),
            Some("COLL10".to_string()),
            4,
            false,
            uuid::Uuid::new_v4(),
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("colliding room creation succeeds");
    server.script_room_codes_for_test(["COLL10", "FRESH9"]);

    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48136".parse().unwrap()).await;
    join_seated_player(&server, &authority, &mut authority_rx, "MINE10", "host").await;

    server
        .handle_regenerate_room_code_operation(&authority, RoomOperationId::new_v4())
        .await;

    let response = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    let ServerMessage::RoomOperationResult { result, .. } = response.as_ref() else {
        panic!("expected correlated result, got {response:?}");
    };
    let RoomOperationResult::RoomCodeRegenerated { room_code } = result.as_ref() else {
        panic!("expected RoomCodeRegenerated, got {result:?}");
    };
    assert_eq!(room_code, "FRESH9", "a colliding candidate must be retried");

    let room = server
        .database
        .get_room("moderation-game", "FRESH9")
        .await
        .expect("new-code lookup succeeds")
        .expect("rotated room exists");
    assert!(room.players.contains_key(&authority));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn creation_applies_the_default_spectator_capacity() {
    struct Case {
        name: &'static str,
        configured: Option<u8>,
        max_players: u8,
        expected: Option<u8>,
    }
    let cases = [
        Case {
            name: "auto derives 2x the player ceiling",
            configured: None,
            max_players: 4,
            expected: Some(8),
        },
        Case {
            name: "a positive value fixes the cap",
            configured: Some(2),
            max_players: 4,
            expected: Some(2),
        },
        Case {
            name: "zero restores unlimited spectators",
            configured: Some(0),
            max_players: 4,
            expected: None,
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let config = ServerConfig {
            default_max_spectators: case.configured,
            ..ServerConfig::default()
        };
        let server = create_test_server_with(config).await;
        let (creator, mut rx) = register_client(
            &server,
            format!("127.0.0.1:481{}", 40 + index).parse().unwrap(),
        )
        .await;
        server
            .handle_join_room(
                &creator,
                "spectator-cap-game".to_string(),
                None,
                "creator".to_string(),
                Some(case.max_players),
                Some(false),
                None,
                None,
            )
            .await;
        let joined = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("join should finish")
            .expect("join should respond");
        assert!(
            matches!(joined.as_ref(), ServerMessage::RoomJoined(_)),
            "expected RoomJoined, got {joined:?}"
        );

        let room = server
            .database
            .get_room_by_id(&server.get_client_room(&creator).await.expect("seated"))
            .await
            .expect("room lookup succeeds")
            .expect("room exists");
        assert_eq!(
            room.max_spectators, case.expected,
            "{} (configured {:?})",
            case.name, case.configured
        );
        assert_eq!(
            room.can_spectate(),
            case.expected.is_none_or(|cap| cap > 0),
            "{}: an empty room must admit spectators under any positive cap",
            case.name
        );
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn kicked_disconnected_seat_is_removed_and_never_reconnectable() {
    use crate::reconnection::ReconnectionError;

    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48150".parse().unwrap()).await;
    let (target, mut target_rx) =
        register_client(&server, "127.0.0.1:48151".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "KICK10", "host").await;
    join_seated_player(&server, &target, &mut target_rx, "KICK10", "guest").await;
    let room_id = server
        .get_client_room(&target)
        .await
        .expect("target seated");

    // The target's socket drops: the real unregister path arms a pending
    // reconnection record (the durable member row is removed; only the
    // record holds the seat) and clears local routing.
    server.unregister_client(&target).await;
    assert!(server.get_client_room(&target).await.is_none());
    let pending_room = match &server.reconnection_manager {
        Some(manager) => manager.pending_reconnection_room(&target).await,
        None => panic!("reconnection manager must be active for this test"),
    };
    assert_eq!(
        pending_room,
        Some(room_id),
        "the disconnect must leave a claimable record for the seat"
    );

    // The authority kicks the vacated seat.
    server
        .handle_kick_player_operation(&authority, RoomOperationId::new_v4(), target)
        .await;

    // The seat is gone from durable state and the pending record is
    // tombstoned: the claim path refuses with the dedicated error.
    let room = server
        .database
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup succeeds")
        .expect("room survives the kick");
    assert!(!room.players.contains_key(&target));

    if let Some(manager) = &server.reconnection_manager {
        let outcome = manager
            .claim_reconnection(&authority, &target, &room_id, "any-token")
            .await;
        assert!(
            matches!(outcome, Err(ReconnectionError::Kicked)),
            "a kicked seat must refuse its reconnection claim, got {outcome:?}"
        );
    } else {
        panic!("reconnection manager must be active for this test");
    }

    let response = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    let ServerMessage::RoomOperationResult { result, .. } = response.as_ref() else {
        panic!("expected correlated result, got {response:?}");
    };
    assert!(
        matches!(result.as_ref(), RoomOperationResult::PlayerKicked { player_id } if *player_id == target),
        "expected PlayerKicked for the vacated seat, got {result:?}"
    );
}

/// Drive a seated join that may carry a join password and assert the
/// terminal `RoomJoinFailed` classification when the join is refused.
async fn join_with_password(
    server: &Arc<EnhancedGameServer>,
    player_id: &PlayerId,
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    room_code: &str,
    name: &str,
    password: Option<&str>,
) -> Result<(), ErrorCode> {
    server
        .handle_join_room(
            player_id,
            "moderation-game".to_string(),
            Some(room_code.to_string()),
            name.to_string(),
            Some(4),
            Some(true),
            None,
            password.map(str::to_string),
        )
        .await;
    let terminal = recv_until(receiver, |message| {
        matches!(
            message,
            ServerMessage::RoomJoined(_) | ServerMessage::RoomJoinFailed { .. }
        )
    })
    .await;
    match terminal.as_ref() {
        ServerMessage::RoomJoined(_) => Ok(()),
        ServerMessage::RoomJoinFailed { error_code, .. } => {
            Err(error_code.clone().expect("join refusals carry a code"))
        }
        other => panic!("expected a join terminal, got {other:?}"),
    }
}

async fn room_access_result(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
) -> RoomOperationResult {
    let response = recv_until(receiver, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    let ServerMessage::RoomOperationResult { result, .. } = response.as_ref() else {
        panic!("expected correlated result, got {response:?}");
    };
    result.as_ref().clone()
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn set_room_access_seals_and_reopens_the_room() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48140".parse().unwrap()).await;
    let (latecomer, mut latecomer_rx) =
        register_client(&server, "127.0.0.1:48141".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "ACCES1", "host").await;

    // Seal the room.
    server
        .handle_set_room_access_operation(
            &authority,
            RoomOperationId::new_v4(),
            Some("open sesame".to_string()),
        )
        .await;
    assert!(
        matches!(
            room_access_result(&mut authority_rx).await,
            RoomOperationResult::RoomAccessUpdated {
                requires_password: true
            }
        ),
        "sealing must report requires_password=true"
    );
    let room = server
        .database
        .get_room("moderation-game", "ACCES1")
        .await
        .expect("room lookup succeeds")
        .expect("room exists");
    assert!(room.password.is_some(), "storage must hold the credential");

    // A join without the password is refused with the non-enumerating code;
    // a wrong password is indistinguishable.
    assert_eq!(
        join_with_password(
            &server,
            &latecomer,
            &mut latecomer_rx,
            "ACCES1",
            "late",
            None
        )
        .await
        .expect_err("password-less join must be refused"),
        ErrorCode::PasswordRequired
    );
    assert_eq!(
        join_with_password(
            &server,
            &latecomer,
            &mut latecomer_rx,
            "ACCES1",
            "late",
            Some("wrong")
        )
        .await
        .expect_err("wrong-password join must be refused"),
        ErrorCode::PasswordRequired
    );
    assert!(
        !server
            .database
            .get_room_by_id(&room.id)
            .await
            .expect("ok")
            .expect("room exists")
            .players
            .contains_key(&latecomer),
        "a refused join must not seat the player"
    );

    // The correct password admits.
    join_with_password(
        &server,
        &latecomer,
        &mut latecomer_rx,
        "ACCES1",
        "late",
        Some("open sesame"),
    )
    .await
    .expect("correct password must admit");

    // Reopening restores open admission.
    server
        .handle_set_room_access_operation(&authority, RoomOperationId::new_v4(), None)
        .await;
    assert!(
        matches!(
            room_access_result(&mut authority_rx).await,
            RoomOperationResult::RoomAccessUpdated {
                requires_password: false
            }
        ),
        "reopening must report requires_password=false"
    );
    let (another, mut another_rx) =
        register_client(&server, "127.0.0.1:48142".parse().unwrap()).await;
    join_with_password(&server, &another, &mut another_rx, "ACCES1", "fresh", None)
        .await
        .expect("an open room must admit without a password");
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn creation_time_password_seals_the_room_from_birth() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (creator, mut creator_rx) =
        register_client(&server, "127.0.0.1:48143".parse().unwrap()).await;
    let (latecomer, mut latecomer_rx) =
        register_client(&server, "127.0.0.1:48144".parse().unwrap()).await;

    // The creating join carries the password: there is no unlocked window.
    join_with_password(
        &server,
        &creator,
        &mut creator_rx,
        "BIRTH1",
        "host",
        Some("secret"),
    )
    .await
    .expect("creating join succeeds");
    let room = server
        .database
        .get_room("moderation-game", "BIRTH1")
        .await
        .expect("room lookup succeeds")
        .expect("room exists");
    assert!(room.password.is_some(), "creation must seal the room");

    assert_eq!(
        join_with_password(
            &server,
            &latecomer,
            &mut latecomer_rx,
            "BIRTH1",
            "late",
            None
        )
        .await
        .expect_err("uninvited join must be refused"),
        ErrorCode::PasswordRequired
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn set_room_access_refusals_are_classified_per_failure() {
    let cases: &[(&str, Option<&str>, ErrorCode)] = &[
        ("empty password", Some(""), ErrorCode::InvalidInput),
        (
            "oversized password",
            Some(&"x".repeat(crate::protocol::MAX_ROOM_PASSWORD_LENGTH + 1)),
            ErrorCode::InvalidInput,
        ),
    ];
    for (name, password, expected) in cases {
        let server = create_test_server_with(ServerConfig::default()).await;
        let (authority, mut authority_rx) =
            register_client(&server, "127.0.0.1:48145".parse().unwrap()).await;
        join_seated_player(&server, &authority, &mut authority_rx, "REFUS1", "host").await;

        server
            .handle_set_room_access_operation(
                &authority,
                RoomOperationId::new_v4(),
                password.map(str::to_string),
            )
            .await;
        let response = recv_until(&mut authority_rx, |message| {
            matches!(message, ServerMessage::RoomOperationResult { .. })
        })
        .await;
        assert_eq!(
            operation_failed_code(&response),
            *expected,
            "{name}: got {response:?}"
        );
        let room = server
            .database
            .get_room("moderation-game", "REFUS1")
            .await
            .expect("room lookup succeeds")
            .expect("room exists");
        assert!(
            room.password.is_none(),
            "{name}: a refused access update must not change the policy"
        );
    }

    // A non-authority member is refused regardless of the payload.
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48146".parse().unwrap()).await;
    let (member, mut member_rx) =
        register_client(&server, "127.0.0.1:48147".parse().unwrap()).await;
    join_seated_player(&server, &authority, &mut authority_rx, "REFUS2", "host").await;
    join_seated_player(&server, &member, &mut member_rx, "REFUS2", "guest").await;
    server
        .handle_set_room_access_operation(
            &member,
            RoomOperationId::new_v4(),
            Some("sneaky".to_string()),
        )
        .await;
    let response = recv_until(&mut member_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    assert_eq!(
        operation_failed_code(&response),
        ErrorCode::NotRoomAuthority,
        "non-authority access update must be refused: got {response:?}"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn ban_evicts_blocks_rejoin_and_lifts_on_unban() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48148".parse().unwrap()).await;
    let (target, mut target_rx, target_close_listener) =
        register_client_with_close_listener(&server, "127.0.0.1:48149".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "BAN001", "host").await;
    join_seated_player(&server, &target, &mut target_rx, "BAN001", "guest").await;

    server
        .handle_ban_player_operation(&authority, RoomOperationId::new_v4(), target)
        .await;

    // The banned seat is evicted through the same machinery as a kick.
    let _ = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::PlayerLeft { .. })
    })
    .await;
    assert!(
        matches!(
            room_access_result(&mut authority_rx).await,
            RoomOperationResult::PlayerBanned { player_id } if player_id == target
        ),
        "ban must report PlayerBanned"
    );
    assert_eq!(
        target_close_listener.requested_reason(),
        Some(CloseReason::Kicked),
        "a banned seat must close with the dedicated 4007 reason"
    );
    let room = server
        .database
        .get_room("moderation-game", "BAN001")
        .await
        .expect("room lookup succeeds")
        .expect("room exists");
    assert!(room.is_banned(&target), "storage must record the ban");

    // The banned id cannot rejoin as a player...
    assert_eq!(
        join_with_password(&server, &target, &mut target_rx, "BAN001", "guest", None)
            .await
            .expect_err("banned rejoin must be refused"),
        ErrorCode::Banned
    );
    // ...nor as a spectator.
    let spectator_err = server
        .spectator_service
        .join(
            &target,
            "moderation-game".to_string(),
            "BAN001".to_string(),
            "guest".to_string(),
        )
        .await
        .expect_err("banned spectator join must be refused");
    assert_eq!(
        spectator_err.code,
        Some(ErrorCode::Banned),
        "banned spectator join must be refused: got {spectator_err:?}"
    );

    // Lifting the ban restores admission.
    server
        .handle_unban_player_operation(&authority, RoomOperationId::new_v4(), target)
        .await;
    assert!(
        matches!(
            room_access_result(&mut authority_rx).await,
            RoomOperationResult::PlayerUnbanned { player_id } if player_id == target
        ),
        "unban must report PlayerUnbanned"
    );
    assert!(
        !server
            .database
            .get_room("moderation-game", "BAN001")
            .await
            .expect("ok")
            .expect("room exists")
            .is_banned(&target),
        "storage must clear the ban"
    );
    join_with_password(&server, &target, &mut target_rx, "BAN001", "guest", None)
        .await
        .expect("unbanned id must be able to rejoin");
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn ban_refusals_mirror_the_kick_classification() {
    let cases: &[(&str, ErrorCode)] = &[
        ("non-member target", ErrorCode::KickTargetNotFound),
        ("self-target", ErrorCode::InvalidInput),
    ];
    for (name, expected) in cases {
        let server = create_test_server_with(ServerConfig::default()).await;
        let (authority, mut authority_rx) =
            register_client(&server, "127.0.0.1:48150".parse().unwrap()).await;
        join_seated_player(&server, &authority, &mut authority_rx, "BANR01", "host").await;

        let target = match *name {
            "self-target" => authority,
            _ => PlayerId::new_v4(),
        };
        server
            .handle_ban_player_operation(&authority, RoomOperationId::new_v4(), target)
            .await;
        let response = recv_until(&mut authority_rx, |message| {
            matches!(message, ServerMessage::RoomOperationResult { .. })
        })
        .await;
        assert_eq!(
            operation_failed_code(&response),
            *expected,
            "{name}: got {response:?}"
        );
        let room = server
            .database
            .get_room("moderation-game", "BANR01")
            .await
            .expect("room lookup succeeds")
            .expect("room exists");
        assert!(
            !room.is_banned(&target),
            "{name}: a refused ban must not record the ban"
        );
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transfer_authority_moves_the_role_and_notifies_members() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48151".parse().unwrap()).await;
    let (successor, mut successor_rx) =
        register_client(&server, "127.0.0.1:48152".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "XFER01", "host").await;
    join_seated_player(&server, &successor, &mut successor_rx, "XFER01", "guest").await;

    server
        .handle_transfer_authority_operation(&authority, RoomOperationId::new_v4(), successor)
        .await;

    // Both members observe the personalized AuthorityChanged announcement.
    let authority_view = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::AuthorityChanged { .. })
    })
    .await;
    let ServerMessage::AuthorityChanged {
        authority_player,
        you_are_authority,
    } = authority_view.as_ref()
    else {
        panic!("expected AuthorityChanged, got {authority_view:?}");
    };
    assert_eq!(*authority_player, Some(successor));
    assert!(
        !you_are_authority,
        "the former authority must learn it lost the role"
    );

    let successor_view = recv_until(&mut successor_rx, |message| {
        matches!(message, ServerMessage::AuthorityChanged { .. })
    })
    .await;
    let ServerMessage::AuthorityChanged {
        you_are_authority, ..
    } = successor_view.as_ref()
    else {
        panic!("expected AuthorityChanged, got {successor_view:?}");
    };
    assert!(
        *you_are_authority,
        "the new authority must learn it holds the role"
    );

    // Durable truth and the correlated terminal.
    let room = server
        .database
        .get_room("moderation-game", "XFER01")
        .await
        .expect("room lookup succeeds")
        .expect("room exists");
    assert_eq!(
        room.authority_player,
        Some(successor),
        "storage must hold the new authority"
    );
    assert!(matches!(
        room.players.get(&authority),
        Some(info) if !info.is_authority
    ));
    assert!(matches!(
        room.players.get(&successor),
        Some(info) if info.is_authority
    ));

    assert!(matches!(
        room_access_result(&mut authority_rx).await,
        RoomOperationResult::AuthorityTransferred { player_id } if player_id == successor
    ));

    // The transferred-away authority can no longer moderate.
    server
        .handle_set_room_access_operation(
            &authority,
            RoomOperationId::new_v4(),
            Some("no longer mine".to_string()),
        )
        .await;
    let response = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::RoomOperationResult { .. })
    })
    .await;
    assert_eq!(
        operation_failed_code(&response),
        ErrorCode::NotRoomAuthority,
        "the former authority must lose its moderation surface"
    );

    // The new authority can transfer the role back.
    server
        .handle_transfer_authority_operation(&successor, RoomOperationId::new_v4(), authority)
        .await;
    let room = server
        .database
        .get_room("moderation-game", "XFER01")
        .await
        .expect("ok")
        .expect("room exists");
    assert_eq!(room.authority_player, Some(authority));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transfer_authority_refusals_are_classified_per_failure() {
    let cases: &[(&str, ErrorCode)] = &[
        ("non-member target", ErrorCode::TransferTargetNotFound),
        ("self-transfer", ErrorCode::InvalidInput),
    ];
    for (name, expected) in cases {
        let server = create_test_server_with(ServerConfig::default()).await;
        let (authority, mut authority_rx) =
            register_client(&server, "127.0.0.1:48153".parse().unwrap()).await;
        join_seated_player(&server, &authority, &mut authority_rx, "XFER02", "host").await;

        let target = match *name {
            "self-transfer" => authority,
            _ => PlayerId::new_v4(),
        };
        server
            .handle_transfer_authority_operation(&authority, RoomOperationId::new_v4(), target)
            .await;
        let response = recv_until(&mut authority_rx, |message| {
            matches!(message, ServerMessage::RoomOperationResult { .. })
        })
        .await;
        assert_eq!(
            operation_failed_code(&response),
            *expected,
            "{name}: got {response:?}"
        );
        let room = server
            .database
            .get_room("moderation-game", "XFER02")
            .await
            .expect("room lookup succeeds")
            .expect("room exists");
        assert_eq!(
            room.authority_player,
            Some(authority),
            "{name}: a refused transfer must not disturb the role"
        );
    }
}
