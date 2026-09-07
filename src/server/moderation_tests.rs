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
            )
            .await;
        timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("join should finish")
            .expect("join should respond");

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
