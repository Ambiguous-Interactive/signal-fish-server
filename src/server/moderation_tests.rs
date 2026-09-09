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

/// Drain a receiver without discarding results: every drained frame stays
/// available for assertions (same shape as the room-service drain helper).
fn drain_receiver(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>) -> Vec<Arc<ServerMessage>> {
    let mut messages = Vec::new();
    loop {
        match receiver.try_recv() {
            Ok(message) => messages.push(message),
            Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                return messages;
            }
        }
    }
}

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
async fn kick_evicts_only_the_authoritys_room_not_a_rerouted_target() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48170".parse().unwrap()).await;
    let (target, mut target_rx, target_close_listener) =
        register_client_with_close_listener(&server, "127.0.0.1:48171".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "WRONG1", "host").await;
    join_seated_player(&server, &target, &mut target_rx, "WRONG1", "guest").await;

    let stale_room = server
        .database
        .get_room("moderation-game", "WRONG1")
        .await
        .expect("room lookup succeeds")
        .expect("authority room exists");
    let target_row = server
        .database
        .get_room_players(&stale_room.id)
        .await
        .expect("roster readable")
        .into_iter()
        .find(|player| player.id == target)
        .expect("target seated in the authority room");

    // Build the stale-residue state a storage-failed detach leaves behind:
    // the target departs the authority's room (row removed, route gone), its
    // connection then joins another room, and the authority's room row is
    // re-planted before the detach backlog repair can run.
    assert!(
        server
            .database
            .remove_player_from_room(&stale_room.id, &target)
            .await
            .expect("durable removal succeeds")
            .is_some(),
        "test setup removes the target's seat"
    );
    let live_room = server
        .database
        .create_room(
            "moderation-game".to_string(),
            Some("WRONG2".to_string()),
            4,
            false,
            target,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("target's live room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&target, live_room.id)
        .await;
    server
        .database
        .add_player_to_room(&stale_room.id, target_row)
        .await
        .expect("stale row planted");

    server
        .handle_kick_player_operation(&authority, RoomOperationId::new_v4(), target)
        .await;

    // The authority's room loses its stale row...
    let stale_room_after = server
        .database
        .get_room("moderation-game", "WRONG1")
        .await
        .expect("room lookup succeeds")
        .expect("authority room survives the kick");
    assert!(
        !stale_room_after.players.contains_key(&target),
        "the kick must remove the stale seat from the authority's room"
    );
    assert!(matches!(
        room_access_result(&mut authority_rx).await,
        RoomOperationResult::PlayerKicked { player_id } if player_id == target
    ));

    // ...and nothing else: the target's live membership, route, and
    // connection in the other room are not the authority's to remove.
    assert_eq!(
        server.get_client_room(&target).await,
        Some(live_room.id),
        "the target's route in its live room must survive the kick"
    );
    let live_room_after = server
        .database
        .get_room("moderation-game", "WRONG2")
        .await
        .expect("room lookup succeeds")
        .expect("live room survives the kick");
    assert!(
        live_room_after.players.contains_key(&target),
        "the target's live seat in the other room must survive the kick"
    );
    assert_eq!(
        target_close_listener.requested_reason(),
        None,
        "a kick of stale residue must not close the target's live connection"
    );

    // The farewell is part of the same contract: a target whose live
    // membership is untouched receives no kicked `Error` frame. Every
    // eviction send happens before the correlated result, so draining the
    // channel after the result observes the final state.
    let unexpected_farewell = drain_receiver(&mut target_rx).into_iter().find(|message| {
        matches!(
            message.as_ref(),
            ServerMessage::Error {
                error_code: Some(ErrorCode::Kicked),
                ..
            }
        )
    });
    assert!(
        unexpected_farewell.is_none(),
        "a kick of stale residue must not send the kicked farewell: got {unexpected_farewell:?}"
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

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn banned_players_pending_record_cannot_restore_the_seat() {
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut _authority_rx) =
        register_client(&server, "127.0.0.1:48152".parse().unwrap()).await;
    let (target, mut _target_rx) =
        register_client(&server, "127.0.0.1:48153".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut _authority_rx, "BANR10", "host").await;
    join_seated_player(&server, &target, &mut _target_rx, "BANR10", "guest").await;
    let room_id = server
        .get_client_room(&target)
        .await
        .expect("target seated");

    // The target's socket drops and its record is armed through the same
    // teardown path a real disconnect uses.
    let seat_info = server
        .database
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup succeeds")
        .expect("room remains present")
        .players
        .get(&target)
        .cloned()
        .expect("target holds a durable seat");
    let token = server
        .reconnection_manager()
        .expect("reconnection is enabled")
        .register_disconnection(
            target,
            room_id,
            false,
            Some(seat_info),
            server
                .connection_manager
                .game_data_epoch(&target)
                .unwrap_or(0),
        )
        .await;
    server
        .database
        .remove_player_from_room(&room_id, &target)
        .await
        .expect("disconnect removes the durable seat");
    server.connection_manager.remove_client(&target);
    server
        .message_coordinator
        .unregister_local_client(&target)
        .await
        .expect("disconnect removes the coordinator route");

    // The authority bans the vacated seat through the same durable write
    // the `BanPlayer` operation performs, in the state a claim that won the
    // room-mutation gate leaves behind: the ban is committed, but the
    // record is not tombstoned (the tombstone mark lands in a later gate
    // hold, and a teardown re-arm after a raced eviction carries a fresh
    // record that no tombstone can cover).
    server
        .database
        .set_room_ban(&room_id, &target, true)
        .await
        .expect("ban write succeeds");

    // The banned player's fresh socket must not restore the seat through
    // its pending record: the ban's only path back into the room is a
    // future fresh join after an unban. The fresh socket registers with the
    // connection manager and the coordinator the same way a real
    // reconnecting handshake does before its claim.
    use crate::coordination::ClientDeliveryHandle;

    let (sender, mut socket_rx) = mpsc::channel(8);
    let socket = server
        .connection_manager
        .register_client(
            sender.clone(),
            ConnectionCloseSignal::detached(),
            "127.0.0.1:48154".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("socket registration succeeds");
    server
        .message_coordinator
        .register_local_client(
            socket,
            None,
            ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
        )
        .await
        .expect("socket coordinator route registers");
    let effective_player_id = Arc::new(tokio::sync::RwLock::new(socket));
    let operation_id = RoomOperationId::new_v4();
    let restored = server
        .handle_reconnect_with_identity_operation(
            &socket,
            &target,
            &room_id,
            &token,
            Arc::clone(&effective_player_id),
            Some(operation_id),
        )
        .await;
    let seat_restored = restored;
    assert!(
        !seat_restored,
        "a banned player's reconnection claim must not restore the seat"
    );
    let failure = recv_until(&mut socket_rx, |message| {
        matches!(
            message,
            ServerMessage::ReconnectionFailed { .. } | ServerMessage::RoomOperationResult { .. }
        )
    })
    .await;
    let refused_code = match failure.as_ref() {
        ServerMessage::ReconnectionFailed { error_code, .. } => Some(error_code.clone()),
        ServerMessage::RoomOperationResult { result, .. } => match result.as_ref() {
            RoomOperationResult::ReconnectionFailed { error_code, .. } => Some(error_code.clone()),
            other => panic!("expected a reconnect refusal result, got {other:?}"),
        },
        other => panic!("expected a reconnect refusal, got {other:?}"),
    };
    assert!(
        matches!(refused_code.as_ref(), Some(ErrorCode::Banned)),
        "the ban refusal must surface the BANNED classification, got {refused_code:?}"
    );
    assert_eq!(
        *effective_player_id.read().await,
        socket,
        "a refused claim must leave the websocket's transient identity alone"
    );
    let room = server
        .database
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup succeeds")
        .expect("room remains present");
    assert!(
        !room.players.contains_key(&target),
        "a banned player must stay unseated after the refused claim"
    );

    // The un-tombstoned record stays claimable, so a later unban during
    // the reconnect window can still honor the credential; until then the
    // ban must refuse every claim, not just the first.
    if let Some(manager) = &server.reconnection_manager {
        let outcome = manager
            .claim_reconnection(&socket, &target, &room_id, &token)
            .await;
        assert!(
            outcome.is_ok(),
            "a ban refusal must leave the record claimable, not consume it with \
             a kick tombstone or delete it: {outcome:?}"
        );
    } else {
        panic!("reconnection manager must be active for this test");
    }
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

    join_seated_player(&server, &authority, &mut authority_rx, "SECRET", "host").await;

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
        .get_room("moderation-game", "SECRET")
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
            "SECRET",
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
            "SECRET",
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
        "SECRET",
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
    join_with_password(&server, &another, &mut another_rx, "SECRET", "fresh", None)
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
async fn stray_password_join_into_open_room_fails_closed() {
    // Issue #546 (authority squat): a password-carrying join states the
    // intent to enter a sealed room. When the code already exists as an open
    // room, the room under that code was created by someone else (the
    // squatter), so seating the requester would deliver them to the
    // squatter's authority. The refusal is the same non-enumerating
    // `PASSWORD_REQUIRED` outcome a sealed-room mismatch produces.
    let server = create_test_server_with(ServerConfig::default()).await;
    let (squatter, mut squatter_rx) =
        register_client(&server, "127.0.0.1:48150".parse().unwrap()).await;
    let (victim, mut victim_rx) =
        register_client(&server, "127.0.0.1:48151".parse().unwrap()).await;

    join_seated_player(&server, &squatter, &mut squatter_rx, "SQUAT1", "squatter").await;

    // Seated: the password-carrying join must not seat the victim into the
    // squatter's open room.
    assert_eq!(
        join_with_password(
            &server,
            &victim,
            &mut victim_rx,
            "SQUAT1",
            "victim",
            Some("open sesame")
        )
        .await
        .expect_err("stray-password join must be refused"),
        ErrorCode::PasswordRequired
    );
    assert!(
        !server
            .database
            .get_room("moderation-game", "SQUAT1")
            .await
            .expect("room lookup succeeds")
            .expect("room exists")
            .players
            .contains_key(&victim),
        "a refused join must not seat the player"
    );

    // The spectator path fails closed the same way.
    let spectator_error = server
        .spectator_service
        .join_operation(
            &victim,
            None,
            "moderation-game".to_string(),
            "SQUAT1".to_string(),
            "victim".to_string(),
            Some("open sesame".to_string()),
        )
        .await
        .expect_err("stray-password spectator join must be refused");
    assert_eq!(
        spectator_error.code,
        Some(ErrorCode::PasswordRequired),
        "spectator stray-password join must share the seated refusal: {spectator_error:?}"
    );

    // The open-room contract without a password is unchanged.
    let (friend, mut friend_rx) =
        register_client(&server, "127.0.0.1:48152".parse().unwrap()).await;
    join_with_password(&server, &friend, &mut friend_rx, "SQUAT1", "friend", None)
        .await
        .expect("open rooms still admit password-less joins");
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
async fn sealed_room_password_check_precedes_the_ban_refusal() {
    // Non-enumeration (issue #525): the join password is the outermost
    // admission perimeter on both paths. A banned caller that has not
    // presented the room credential learns only PASSWORD_REQUIRED; with the
    // credential, the refusal names the ban.
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48155".parse().unwrap()).await;
    let (target, mut target_rx) =
        register_client(&server, "127.0.0.1:48156".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "SEALB1", "host").await;
    join_seated_player(&server, &target, &mut target_rx, "SEALB1", "guest").await;

    server
        .handle_ban_player_operation(&authority, RoomOperationId::new_v4(), target)
        .await;
    let _ = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::PlayerLeft { .. })
    })
    .await;
    let _ = room_access_result(&mut authority_rx).await;

    server
        .handle_set_room_access_operation(
            &authority,
            RoomOperationId::new_v4(),
            Some("secret".to_string()),
        )
        .await;
    let _ = room_access_result(&mut authority_rx).await;

    // Seated path: without the credential the sealed perimeter answers
    // first; with it, the refusal names the ban.
    assert_eq!(
        join_with_password(&server, &target, &mut target_rx, "SEALB1", "guest", None)
            .await
            .expect_err("banned join without the credential must be refused"),
        ErrorCode::PasswordRequired,
        "the sealed perimeter must answer before the ban does"
    );
    assert_eq!(
        join_with_password(
            &server,
            &target,
            &mut target_rx,
            "SEALB1",
            "guest",
            Some("secret")
        )
        .await
        .expect_err("banned join with the credential must be refused"),
        ErrorCode::Banned
    );

    // The spectator path shares the ordering.
    let spectator_error = server
        .spectator_service
        .join_operation(
            &target,
            None,
            "moderation-game".to_string(),
            "SEALB1".to_string(),
            "guest".to_string(),
            None,
        )
        .await
        .expect_err("banned spectator join without the credential must be refused");
    assert_eq!(
        spectator_error.code,
        Some(ErrorCode::PasswordRequired),
        "the sealed perimeter must answer before the ban does: {spectator_error:?}"
    );
    let spectator_error = server
        .spectator_service
        .join_operation(
            &target,
            None,
            "moderation-game".to_string(),
            "SEALB1".to_string(),
            "guest".to_string(),
            Some("secret".to_string()),
        )
        .await
        .expect_err("banned spectator join with the credential must be refused");
    assert_eq!(spectator_error.code, Some(ErrorCode::Banned));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn rotation_and_transfer_preserve_the_ban_list_and_the_join_password() {
    // Pins the in-place room-write design (issue #525 sweep): rotating the
    // room code and moving the authority role must never rewrite the room
    // in a way that drops the ban list or the join password.
    let server = create_test_server_with(ServerConfig::default()).await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48157".parse().unwrap()).await;
    let (successor, mut successor_rx) =
        register_client(&server, "127.0.0.1:48158".parse().unwrap()).await;
    let (target, mut target_rx) =
        register_client(&server, "127.0.0.1:48159".parse().unwrap()).await;

    join_seated_player(&server, &authority, &mut authority_rx, "KEEPB1", "host").await;
    join_seated_player(&server, &successor, &mut successor_rx, "KEEPB1", "second").await;
    join_seated_player(&server, &target, &mut target_rx, "KEEPB1", "guest").await;
    let room_id = server
        .get_client_room(&authority)
        .await
        .expect("authority seated");

    server
        .handle_ban_player_operation(&authority, RoomOperationId::new_v4(), target)
        .await;
    let _ = recv_until(&mut authority_rx, |message| {
        matches!(message, ServerMessage::PlayerLeft { .. })
    })
    .await;
    let _ = room_access_result(&mut authority_rx).await;
    server
        .handle_set_room_access_operation(
            &authority,
            RoomOperationId::new_v4(),
            Some("secret".to_string()),
        )
        .await;
    let _ = room_access_result(&mut authority_rx).await;

    server
        .handle_regenerate_room_code_operation(&authority, RoomOperationId::new_v4())
        .await;
    let new_code = match room_access_result(&mut authority_rx).await {
        RoomOperationResult::RoomCodeRegenerated { room_code } => room_code,
        other => panic!("expected RoomCodeRegenerated, got {other:?}"),
    };
    assert_ne!(new_code, "KEEPB1", "rotation must mint a fresh code");

    server
        .handle_transfer_authority_operation(&authority, RoomOperationId::new_v4(), successor)
        .await;
    let _ = room_access_result(&mut authority_rx).await;
    let room = server
        .database
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup succeeds")
        .expect("room remains present");
    assert_eq!(
        room.authority_player,
        Some(successor),
        "the transfer must have moved the authority role"
    );
    assert!(
        room.is_banned(&target),
        "the ban list must survive rotation and transfer"
    );

    // The preserved policy is live, not just stored: the fresh code refuses
    // a passwordless join with the sealed perimeter, and the banned id with
    // the correct credential is told BANNED under the new authority.
    let (fresh, mut fresh_rx) = register_client(&server, "127.0.0.1:48160".parse().unwrap()).await;
    assert_eq!(
        join_with_password(&server, &fresh, &mut fresh_rx, &new_code, "late", None)
            .await
            .expect_err("passwordless join into the rotated sealed room must be refused"),
        ErrorCode::PasswordRequired
    );
    assert_eq!(
        join_with_password(
            &server,
            &target,
            &mut target_rx,
            &new_code,
            "guest",
            Some("secret")
        )
        .await
        .expect_err("banned join must stay refused after rotation and transfer"),
        ErrorCode::Banned
    );
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

/// Build a server with the given session policy, so the finalize path can
/// resolve to a host-topology plan (same shape as `ready_state_tests`).
async fn create_test_server_with_session(session: SessionConfig) -> Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
        ServerConfig::default(),
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        session,
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

/// Issue #554, decided semantics ("the role follows the plan, not vice
/// versa"): a mid-game `TransferAuthority` moves the moderation and
/// start-game role, but the running session's transport host stays pinned to
/// its finalize-time election. Only a departure (host failover) re-plans the
/// session. Documented in `docs/concepts/authority.md` ("The Role and the
/// Transport Host Are Not the Same Thing").
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn mid_game_transfer_moves_the_role_but_not_the_session_host() {
    let server = create_test_server_with_session(SessionConfig {
        default_topology: crate::protocol::Topology::Host,
        ..SessionConfig::default()
    })
    .await;
    let (authority, mut authority_rx) =
        register_client(&server, "127.0.0.1:48161".parse().unwrap()).await;
    let (successor, mut successor_rx) =
        register_client(&server, "127.0.0.1:48162".parse().unwrap()).await;
    let v3_host = crate::server::NegotiatedProtocol {
        version: 3,
        transports: vec![
            crate::protocol::Transport::Relay,
            crate::protocol::Transport::WebRtc,
        ],
        topologies: vec![
            crate::protocol::Topology::Relay,
            crate::protocol::Topology::Host,
        ],
    };
    server.set_client_protocol(&authority, v3_host.clone());
    server.set_client_protocol(&successor, v3_host);

    // A 2-seat authority room, driven through the REAL finalize flow: both
    // members join, the room moves to the lobby, both toggle ready, and an
    // explicit StartGame finalizes with a host-topology session plan.
    join_seated_player(&server, &authority, &mut authority_rx, "MIDGM1", "host").await;
    join_seated_player(&server, &successor, &mut successor_rx, "MIDGM1", "guest").await;
    let room = server
        .database
        .get_room("moderation-game", "MIDGM1")
        .await
        .expect("room lookup succeeds")
        .expect("room exists");
    server
        .database
        .transition_room_to_lobby(&room.id)
        .await
        .expect("lobby transition succeeds");
    server.handle_player_ready(&authority).await;
    server.handle_player_ready(&successor).await;
    server.handle_start_game(&authority).await;

    let plan_before = server
        .active_session_plan(&room.id)
        .expect("a finalized host-topology room must hold an active session plan");
    assert_eq!(
        plan_before.topology,
        crate::protocol::Topology::Host,
        "this test pins host-topology semantics"
    );
    assert!(
        plan_before.host.is_some(),
        "a host-topology plan must have elected a transport host"
    );

    drain_receiver(&mut authority_rx);
    drain_receiver(&mut successor_rx);

    // Mid-game transfer of the authority role.
    server
        .handle_transfer_authority_operation(&authority, RoomOperationId::new_v4(), successor)
        .await;

    // The moderation role moved, and the room is still mid-game.
    let room_after = server
        .database
        .get_room_by_id(&room.id)
        .await
        .expect("room lookup succeeds")
        .expect("room exists");
    assert_eq!(
        room_after.authority_player,
        Some(successor),
        "the transfer must grant the successor the authority role"
    );
    assert_eq!(
        room_after.lobby_state,
        crate::protocol::LobbyState::Finalized,
        "the room must stay finalized across the transfer"
    );

    // The session plan did NOT follow the role: identical sticky decision.
    let plan_after = server
        .active_session_plan(&room.id)
        .expect("the active plan must survive the transfer");
    assert_eq!(
        plan_after.topology, plan_before.topology,
        "the session topology must not change on a transfer"
    );
    assert_eq!(
        plan_after.transport, plan_before.transport,
        "the session transport must not change on a transfer"
    );
    assert_eq!(
        plan_after.host, plan_before.host,
        "the transport host must not follow the moderation role"
    );

    // No re-plan traffic: members learn only the role change.
    for (receiver, who) in [
        (&mut authority_rx, "authority"),
        (&mut successor_rx, "successor"),
    ] {
        let messages = drain_receiver(receiver);
        assert!(
            messages
                .iter()
                .any(|message| matches!(message.as_ref(), ServerMessage::AuthorityChanged { .. })),
            "{who} must learn the role change"
        );
        assert!(
            !messages
                .iter()
                .any(|message| matches!(message.as_ref(), ServerMessage::SessionPlan(_))),
            "{who} must not receive a fresh SessionPlan on a mid-game transfer"
        );
        assert!(
            !messages
                .iter()
                .any(|message| matches!(message.as_ref(), ServerMessage::GameStarting { .. })),
            "{who} must not observe a re-start on a mid-game transfer"
        );
    }
}
