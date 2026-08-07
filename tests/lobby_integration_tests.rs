mod test_helpers;

use signal_fish_server::protocol::*;
use std::sync::Arc;
use test_helpers::create_test_server;
use tokio::sync::mpsc;
use uuid::Uuid;

#[tokio::test]
async fn test_lobby_integration_full_flow() {
    let server = create_test_server().await;

    // Create 3 players
    let player_ids: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
    let mut channels = Vec::new();

    // Connect all players
    for &player_id in &player_ids {
        let (tx, rx) = mpsc::channel(64);
        server.connect_client(player_id, tx).await;
        channels.push(rx);
    }

    // Create a 3-player room
    for (i, &player_id) in player_ids.iter().enumerate() {
        server
            .handle_join_room(
                &player_id,
                "integration_game".to_string(),
                Some("INT001".to_string()),
                format!("Player{}", i + 1),
                Some(3),
                Some(true),
                None,
            )
            .await;
    }

    // A room enters the `Lobby` state EXACTLY ONCE — when its creator (the first
    // player, p1) joins. The creator's `RoomJoined` still reports
    // `lobby_state: Waiting` (built before the transition), followed by ONE
    // own-join `LobbyStateChanged`. Every SUBSEQUENT joiner's `RoomJoined`
    // already reports `lobby_state: Lobby`, so NO `LobbyStateChanged` is
    // broadcast on their join, and existing members see ONLY `PlayerJoined`.
    // The per-receiver message stream is therefore:
    //   p1: RoomJoined, LSC(own-join), PlayerJoined(p2), PlayerJoined(p3)
    //   p2: RoomJoined, PlayerJoined(p3)
    //   p3: RoomJoined
    expect_room_joined(&mut channels[0], "player 1 initial join");
    expect_lobby_state_changed_with_ready(
        &mut channels[0],
        "player 1 own-join lobby update",
        0,
        false,
    );
    expect_player_joined(
        &mut channels[0],
        player_ids[1],
        "player 1 sees player 2 join",
    );
    expect_player_joined(
        &mut channels[0],
        player_ids[2],
        "player 1 sees player 3 join",
    );

    expect_room_joined(&mut channels[1], "player 2 initial join");
    expect_player_joined(
        &mut channels[1],
        player_ids[2],
        "player 2 sees player 3 join",
    );

    expect_room_joined(&mut channels[2], "player 3 initial join");

    for (i, rx) in channels.iter_mut().enumerate() {
        assert_no_pending_message(rx, &format!("player {} post-join drain", i + 1));
    }

    // Players signal ready one by one
    for (i, &player_id) in player_ids.iter().enumerate() {
        server.handle_player_ready(&player_id).await;

        // All players should receive LobbyStateChanged
        for rx in channels.iter_mut() {
            let msg = rx.try_recv().unwrap();
            match msg.as_ref() {
                ServerMessage::LobbyStateChanged {
                    lobby_state,
                    ready_players,
                    all_ready,
                } => {
                    assert_eq!(*lobby_state, LobbyState::Lobby);
                    assert_eq!(ready_players.len(), i + 1);
                    assert_eq!(*all_ready, i == 2); // All ready when last player signals
                }
                _ => panic!("Expected LobbyStateChanged message"),
            }
        }

        // Finalization is now explicit: once every current player is ready,
        // the room creator (the authority, player 1) sends StartGame to finalize
        // the lobby and trigger the GameStarting broadcast. Readiness alone no
        // longer auto-starts the game.
        if i == 2 {
            server.handle_start_game(&player_ids[0]).await;

            for rx in channels.iter_mut() {
                let msg = rx.try_recv().unwrap();
                match msg.as_ref() {
                    ServerMessage::GameStarting { peer_connections } => {
                        assert_eq!(peer_connections.len(), 3);
                        // Verify authority assignment
                        let auth_count = peer_connections.iter().filter(|p| p.is_authority).count();
                        assert_eq!(auth_count, 1);
                    }
                    _ => panic!("Expected GameStarting message"),
                }
            }
        }
    }
}

#[tokio::test]
async fn test_lobby_player_leaves_during_ready_phase() {
    let server = create_test_server().await;

    let player1_id = Uuid::new_v4();
    let player2_id = Uuid::new_v4();

    let (tx1, mut rx1) = mpsc::channel(64);
    let (tx2, mut rx2) = mpsc::channel(64);

    server.connect_client(player1_id, tx1).await;
    server.connect_client(player2_id, tx2).await;

    // Create 2-player room and enter lobby
    for (id, name) in [(player1_id, "P1"), (player2_id, "P2")] {
        server
            .handle_join_room(
                &id,
                "leave_test".to_string(),
                Some("LEAVE1".to_string()),
                name.to_string(),
                Some(2),
                Some(true),
                None,
            )
            .await;
    }

    // The room enters the lobby EXACTLY ONCE, on the creator (player 1) joining:
    // player 1 sees its own-join LobbyStateChanged. Player 2 is a subsequent
    // joiner, so its RoomJoined already reports `lobby_state: Lobby` and no
    // LobbyStateChanged is broadcast on its join — player 1 sees only
    // PlayerJoined, and player 2 sees nothing further.
    expect_room_joined(&mut rx1, "player 1 leave-test room join");
    expect_lobby_state_changed(&mut rx1, "player 1 leave-test own-join lobby update");
    expect_room_joined(&mut rx2, "player 2 leave-test room join");
    expect_player_joined(
        &mut rx1,
        player2_id,
        "player 1 leave-test join notification",
    );

    // Player 1 signals ready
    server.handle_player_ready(&player1_id).await;

    expect_lobby_state_changed(&mut rx1, "player 1 ready update before leave");
    expect_lobby_state_changed(&mut rx2, "player 2 ready update before leave");

    // Player 2 leaves the room
    server.leave_room(&player2_id).await;

    // Player 1 should receive PlayerLeft notification
    let msg = rx1.try_recv().unwrap();
    match msg.as_ref() {
        ServerMessage::PlayerLeft { player_id, .. } => {
            assert_eq!(*player_id, player2_id);
        }
        _ => panic!("Expected PlayerLeft message"),
    }

    // Player 2 should receive RoomLeft confirmation
    let msg = rx2.try_recv().unwrap();
    match msg.as_ref() {
        ServerMessage::RoomLeft => {}
        _ => panic!("Expected RoomLeft message"),
    }

    // A departure no longer regresses a partial lobby back to Waiting: the room
    // stays a valid lobby with its remaining member, who keeps their readiness.
    // The leave emits only PlayerLeft (asserted above) — no LobbyStateChanged.
    assert_no_pending_message(&mut rx1, "player 1 after player 2 leaves");

    // Player 1 is the room's only current member and is already ready
    // (`all_ready` over the live membership). `max_players` is a ceiling, not a
    // required count, so a solo lobby may start: an explicit StartGame from
    // player 1 finalizes the room and broadcasts GameStarting.
    server.handle_start_game(&player1_id).await;

    let msg = rx1.try_recv().unwrap();
    match msg.as_ref() {
        ServerMessage::GameStarting { peer_connections } => {
            assert_eq!(
                peer_connections.len(),
                1,
                "solo lobby finalizes with its single remaining member"
            );
            assert_eq!(peer_connections[0].player_id, player1_id);
        }
        _ => panic!("Expected GameStarting message, but received: {msg:?}"),
    }
}

#[tokio::test]
async fn test_lobby_ready_toggle_resets_ready_state() {
    let server = create_test_server().await;

    let player1_id = Uuid::new_v4();
    let player2_id = Uuid::new_v4();

    let (tx1, mut rx1) = mpsc::channel(64);
    let (tx2, mut rx2) = mpsc::channel(64);

    server.connect_client(player1_id, tx1).await;
    server.connect_client(player2_id, tx2).await;

    for (id, name) in [(player1_id, "ToggleP1"), (player2_id, "ToggleP2")] {
        server
            .handle_join_room(
                &id,
                "toggle_test".to_string(),
                Some("TOGGLE".to_string()),
                name.to_string(),
                Some(2),
                Some(true),
                None,
            )
            .await;
    }

    // A room enters the lobby EXACTLY ONCE, on the creator (player 1) joining:
    // player 1's join triggers the single Waiting->Lobby LobbyStateChanged.
    // Player 2 is a subsequent joiner, so its RoomJoined already reports
    // `lobby_state: Lobby` and no LobbyStateChanged is broadcast on its join —
    // player 1 sees only PlayerJoined, and player 2 sees nothing further.
    expect_room_joined(&mut rx1, "toggle player 1 room join");
    expect_lobby_state_changed_with_ready(&mut rx1, "toggle player 1 lobby entry", 0, false);
    expect_room_joined(&mut rx2, "toggle player 2 room join");
    expect_player_joined(&mut rx1, player2_id, "toggle player 1 sees player 2 join");

    server.handle_player_ready(&player1_id).await;

    expect_lobby_state_changed_with_ready(&mut rx1, "toggle player 1 first ready update", 1, false);
    expect_lobby_state_changed_with_ready(&mut rx2, "toggle player 2 first ready update", 1, false);

    server.handle_player_ready(&player1_id).await;

    expect_lobby_state_changed_with_ready(&mut rx1, "toggle player 1 ready reset update", 0, false);
    expect_lobby_state_changed_with_ready(&mut rx2, "toggle player 2 ready reset update", 0, false);
    assert_no_pending_message(&mut rx1, "toggle player 1 final drain");
    assert_no_pending_message(&mut rx2, "toggle player 2 final drain");
}

#[tokio::test]
async fn test_lobby_room_authority_preservation() {
    let server = create_test_server().await;

    let player1_id = Uuid::new_v4();
    let player2_id = Uuid::new_v4();

    let (tx1, mut rx1) = mpsc::channel(64);
    let (tx2, mut rx2) = mpsc::channel(64);

    server.connect_client(player1_id, tx1).await;
    server.connect_client(player2_id, tx2).await;

    // Create room with authority support
    server
        .handle_join_room(
            &player1_id,
            "auth_test".to_string(),
            Some("AUTH01".to_string()),
            "Authority".to_string(),
            Some(2),
            Some(true),
            None,
        )
        .await;

    server
        .handle_join_room(
            &player2_id,
            "auth_test".to_string(),
            Some("AUTH01".to_string()),
            "Regular".to_string(),
            Some(2),
            Some(true),
            None,
        )
        .await;

    let room_joined_msg1 = expect_room_joined(&mut rx1, "authority player room join");
    let room_joined_msg2 = expect_room_joined(&mut rx2, "regular player room join");

    // Verify initial authority assignment
    match room_joined_msg1.as_ref() {
        ServerMessage::RoomJoined(ref payload) => {
            assert!(payload.is_authority); // First player should be authority
        }
        _ => panic!("Expected RoomJoined message"),
    }

    match room_joined_msg2.as_ref() {
        ServerMessage::RoomJoined(ref payload) => {
            assert!(!payload.is_authority); // Second player should not be authority
        }
        _ => panic!("Expected RoomJoined message"),
    }

    // The room enters the lobby EXACTLY ONCE, on the authority (creator)
    // joining: the authority sees its own-join LobbyStateChanged. The regular
    // player is a subsequent joiner, so its RoomJoined already reports
    // `lobby_state: Lobby` and no LobbyStateChanged is broadcast on its join —
    // the authority sees only PlayerJoined, and the regular player sees nothing
    // further.
    expect_lobby_state_changed(&mut rx1, "authority player own-join lobby update");
    expect_player_joined(&mut rx1, player2_id, "authority player sees regular join");

    // Both players signal ready
    server.handle_player_ready(&player1_id).await;
    expect_lobby_state_changed(&mut rx1, "authority player first ready update");
    expect_lobby_state_changed(&mut rx2, "regular player first ready update");

    server.handle_player_ready(&player2_id).await;
    expect_lobby_state_changed(&mut rx1, "authority player all-ready update");
    expect_lobby_state_changed(&mut rx2, "regular player all-ready update");

    // Finalization is now explicit: with every current player ready, the room's
    // authority (player 1) sends StartGame to finalize and trigger GameStarting.
    server.handle_start_game(&player1_id).await;

    // Check GameStarting message preserves authority
    let game_start_msg1 = rx1.try_recv().unwrap();
    let game_start_msg2 = rx2.try_recv().unwrap();

    for msg in [game_start_msg1, game_start_msg2] {
        match msg.as_ref() {
            ServerMessage::GameStarting { peer_connections } => {
                assert_eq!(peer_connections.len(), 2);

                let auth_peer = peer_connections.iter().find(|p| p.is_authority).unwrap();
                let regular_peer = peer_connections.iter().find(|p| !p.is_authority).unwrap();

                assert_eq!(auth_peer.player_id, player1_id);
                assert_eq!(auth_peer.player_name, "Authority");
                assert_eq!(regular_peer.player_id, player2_id);
                assert_eq!(regular_peer.player_name, "Regular");
            }
            _ => panic!("Expected GameStarting message"),
        }
    }
}

#[tokio::test]
async fn test_spectator_state_updates_include_snapshots_and_reasons() {
    let server = create_test_server().await;

    let player_id = Uuid::new_v4();
    let spectator_id = Uuid::new_v4();
    let game_name = "spectator_flow";
    let room_code = "SPEC01";

    let (player_tx, mut player_rx) = mpsc::channel(64);
    let (spectator_tx, mut spectator_rx) = mpsc::channel(64);

    server.connect_client(player_id, player_tx).await;
    server.connect_client(spectator_id, spectator_tx).await;

    server
        .handle_join_room(
            &player_id,
            game_name.to_string(),
            Some(room_code.to_string()),
            "HostPlayer".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;

    match player_rx
        .try_recv()
        .expect("host RoomJoined message")
        .as_ref()
    {
        ServerMessage::RoomJoined(ref payload) => assert_eq!(payload.room_code, room_code),
        other => panic!("unexpected host message: {other:?}"),
    }

    // The room enters the lobby as soon as the host joins (`max_players` is a
    // ceiling, not a required count), so the host receives a LobbyStateChanged
    // right after RoomJoined. Drain it before asserting the spectator flow.
    expect_lobby_state_changed(&mut player_rx, "host lobby entry on first join");

    server
        .handle_join_as_spectator(
            &spectator_id,
            game_name.to_string(),
            room_code.to_string(),
            "ViewerOne".to_string(),
        )
        .await;

    match spectator_rx
        .try_recv()
        .expect("spectator join message")
        .as_ref()
    {
        ServerMessage::SpectatorJoined(ref p) => {
            assert_eq!(p.spectator_id, spectator_id);
            assert_eq!(
                p.reason,
                Some(SpectatorStateChangeReason::Joined),
                "spectator join reason missing"
            );
            assert_eq!(p.current_spectators.len(), 1);
            assert_eq!(p.current_spectators[0].id, spectator_id);
        }
        other => panic!("unexpected spectator join message: {other:?}"),
    }

    match player_rx
        .try_recv()
        .expect("host spectator notification")
        .as_ref()
    {
        ServerMessage::NewSpectatorJoined {
            spectator,
            current_spectators,
            reason,
        } => {
            assert_eq!(spectator.id, spectator_id);
            assert_eq!(
                *reason,
                Some(SpectatorStateChangeReason::Joined),
                "host did not see join reason"
            );
            assert_eq!(current_spectators.len(), 1);
            assert_eq!(current_spectators[0].id, spectator_id);
        }
        other => panic!("unexpected host spectator notification: {other:?}"),
    }

    server.handle_leave_spectator(&spectator_id).await;

    match spectator_rx
        .try_recv()
        .expect("spectator left message")
        .as_ref()
    {
        ServerMessage::SpectatorLeft {
            reason,
            current_spectators,
            ..
        } => {
            assert_eq!(*reason, Some(SpectatorStateChangeReason::VoluntaryLeave));
            assert!(current_spectators.is_empty());
        }
        other => panic!("unexpected spectator left message: {other:?}"),
    }

    match player_rx
        .try_recv()
        .expect("host spectator leave notification")
        .as_ref()
    {
        ServerMessage::SpectatorDisconnected {
            spectator_id: disconnected_id,
            reason,
            current_spectators,
        } => {
            assert_eq!(*disconnected_id, spectator_id);
            assert_eq!(*reason, Some(SpectatorStateChangeReason::VoluntaryLeave));
            assert!(current_spectators.is_empty());
        }
        other => panic!("unexpected host spectator disconnect message: {other:?}"),
    }

    server
        .handle_join_as_spectator(
            &spectator_id,
            game_name.to_string(),
            room_code.to_string(),
            "ViewerOne".to_string(),
        )
        .await;
    expect_spectator_joined(
        &mut spectator_rx,
        spectator_id,
        "spectator rejoin after voluntary leave",
    );
    expect_new_spectator_joined(&mut player_rx, spectator_id, "host sees spectator rejoin");

    server.disconnect_client(&spectator_id).await;

    match player_rx
        .try_recv()
        .expect("host spectator disconnect notification")
        .as_ref()
    {
        ServerMessage::SpectatorDisconnected {
            spectator_id: disconnected_id,
            reason,
            ..
        } => {
            assert_eq!(*disconnected_id, spectator_id);
            assert_eq!(*reason, Some(SpectatorStateChangeReason::Disconnected));
        }
        other => panic!("unexpected message after spectator disconnect: {other:?}"),
    }
}

/// Every rejected `JoinAsSpectator` answers with the terminal `SpectatorJoinFailed`
/// that `docs/protocol.md` and the AsyncAPI document pair with `SpectatorJoined`.
/// A client that awaits that pair — the documented contract — otherwise waits out
/// its own timeout while a generic `Error` frame goes unmatched.
#[tokio::test]
async fn spectator_join_failures_answer_with_spectator_join_failed() {
    let server = create_test_server().await;

    let host_id = Uuid::new_v4();
    let (host_tx, mut host_rx) = mpsc::channel(64);
    server.connect_client(host_id, host_tx).await;
    server
        .handle_join_room(
            &host_id,
            "spectator_failure".to_string(),
            Some("SPECF1".to_string()),
            "Host".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;
    expect_room_joined(&mut host_rx, "host join before spectator failures");
    expect_lobby_state_changed(&mut host_rx, "host own-join lobby entry");

    // One case per distinct rejection source: the room lookup, and the
    // role check that runs once a room is found.
    let cases: [(Uuid, &str, &str, ErrorCode); 2] = [
        (
            Uuid::new_v4(),
            "NOSUCH",
            "no such room code",
            ErrorCode::RoomNotFound,
        ),
        (
            host_id,
            "SPECF1",
            "a seated player cannot also spectate",
            ErrorCode::SpectatorNotAllowed,
        ),
    ];

    for (actor, room_code, context, expected_code) in cases {
        let (tx, mut rx) = mpsc::channel(64);
        if actor != host_id {
            server.connect_client(actor, tx).await;
        }
        server
            .handle_join_as_spectator(
                &actor,
                "spectator_failure".to_string(),
                room_code.to_string(),
                "Viewer".to_string(),
            )
            .await;
        let receiver = if actor == host_id {
            &mut host_rx
        } else {
            &mut rx
        };
        match recv_now(receiver, context).as_ref() {
            ServerMessage::SpectatorJoinFailed { error_code, reason } => {
                assert_eq!(*error_code, Some(expected_code), "{context}");
                assert!(!reason.is_empty(), "{context}: failure carries a reason");
            }
            other => panic!("{context}: expected SpectatorJoinFailed, got {other:?}"),
        }
    }
}

fn recv_now(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    context: &str,
) -> Arc<ServerMessage> {
    receiver
        .try_recv()
        .unwrap_or_else(|error| panic!("{context}: expected message, got {error:?}"))
}

fn expect_room_joined(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    context: &str,
) -> Arc<ServerMessage> {
    let message = recv_now(receiver, context);
    match message.as_ref() {
        ServerMessage::RoomJoined(_) => message,
        other => panic!("{context}: expected RoomJoined, got {other:?}"),
    }
}

fn expect_player_joined(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    expected_player_id: PlayerId,
    context: &str,
) {
    let message = recv_now(receiver, context);
    match message.as_ref() {
        ServerMessage::PlayerJoined { player } => {
            assert_eq!(player.id, expected_player_id, "{context}: joined player id");
        }
        other => panic!("{context}: expected PlayerJoined, got {other:?}"),
    }
}

fn expect_lobby_state_changed(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>, context: &str) {
    expect_lobby_state_changed_with(receiver, context, |_, _| {});
}

fn expect_lobby_state_changed_with_ready(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    context: &str,
    expected_ready_players: usize,
    expected_all_ready: bool,
) {
    expect_lobby_state_changed_with(receiver, context, |ready_players, all_ready| {
        assert_eq!(
            ready_players.len(),
            expected_ready_players,
            "{context}: ready player count"
        );
        assert_eq!(*all_ready, expected_all_ready, "{context}: all_ready flag");
    });
}

fn expect_lobby_state_changed_with(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    context: &str,
    assert_payload: impl FnOnce(&[PlayerId], &bool),
) {
    let message = recv_now(receiver, context);
    match message.as_ref() {
        ServerMessage::LobbyStateChanged {
            ready_players,
            all_ready,
            ..
        } => assert_payload(ready_players, all_ready),
        other => panic!("{context}: expected LobbyStateChanged, got {other:?}"),
    }
}

fn expect_spectator_joined(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    expected_spectator_id: PlayerId,
    context: &str,
) {
    let message = recv_now(receiver, context);
    match message.as_ref() {
        ServerMessage::SpectatorJoined(payload) => {
            assert_eq!(
                payload.spectator_id, expected_spectator_id,
                "{context}: spectator id"
            );
        }
        other => panic!("{context}: expected SpectatorJoined, got {other:?}"),
    }
}

fn expect_new_spectator_joined(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    expected_spectator_id: PlayerId,
    context: &str,
) {
    let message = recv_now(receiver, context);
    match message.as_ref() {
        ServerMessage::NewSpectatorJoined { spectator, .. } => {
            assert_eq!(
                spectator.id, expected_spectator_id,
                "{context}: spectator id"
            );
        }
        other => panic!("{context}: expected NewSpectatorJoined, got {other:?}"),
    }
}

fn assert_no_pending_message(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>, context: &str) {
    match receiver.try_recv() {
        Ok(message) => panic!("{context}: unexpected pending message: {message:?}"),
        Err(mpsc::error::TryRecvError::Empty) => {}
        Err(mpsc::error::TryRecvError::Disconnected) => {
            panic!("{context}: channel disconnected while checking for silence")
        }
    }
}
