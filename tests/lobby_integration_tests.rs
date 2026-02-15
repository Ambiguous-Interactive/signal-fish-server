mod test_helpers;

use signal_fish_server::protocol::*;
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

    // Clear initial messages
    for (i, rx) in channels.iter_mut().enumerate() {
        let _ = rx.try_recv().unwrap(); // RoomJoined

        // Only the last player (when room becomes full) gets LobbyStateChanged
        if i == 2 {
            let _ = rx.try_recv().unwrap(); // LobbyStateChanged
        }
    }

    // Clear PlayerJoined notifications that existing players received
    for (i, rx) in channels.iter_mut().enumerate() {
        if i == 0 {
            // Player 1 should have received PlayerJoined for Player 2 and Player 3
            let _ = rx.try_recv().unwrap(); // PlayerJoined for Player 2
            let _ = rx.try_recv().unwrap(); // PlayerJoined for Player 3
            let _ = rx.try_recv().unwrap(); // LobbyStateChanged when room became full
        } else if i == 1 {
            // Player 2 should have received PlayerJoined for Player 3
            let _ = rx.try_recv().unwrap(); // PlayerJoined for Player 3
            let _ = rx.try_recv().unwrap(); // LobbyStateChanged when room became full
        }
        // Player 3 doesn't receive any PlayerJoined messages (already covered above)
    }

    // All LobbyStateChanged messages should have been cleared above
    // Verify no unexpected messages remain
    for (i, rx) in channels.iter_mut().enumerate() {
        if let Ok(msg) = rx.try_recv() {
            panic!("Player {} has unexpected message: {:?}", i + 1, msg);
        }
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

        // If all players are ready, should receive GameStarting
        if i == 2 {
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

    // Clear initial messages
    let _ = rx1.try_recv(); // RoomJoined
    let _ = rx2.try_recv(); // RoomJoined
    let _ = rx1.try_recv(); // PlayerJoined
    let _ = rx1.try_recv(); // LobbyStateChanged
    let _ = rx2.try_recv(); // LobbyStateChanged

    // Player 1 signals ready
    server.handle_player_ready(&player1_id).await;

    // Clear LobbyStateChanged messages
    let _ = rx1.try_recv();
    let _ = rx2.try_recv();

    // Player 2 leaves the room
    server.leave_room(&player2_id).await;

    // Player 1 should receive PlayerLeft notification
    let msg = rx1.try_recv().unwrap();
    match msg.as_ref() {
        ServerMessage::PlayerLeft { player_id } => {
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

    // Room should no longer be in lobby state (only 1 player left)
    // Player 1 should not be able to start the game alone
    server.handle_player_ready(&player1_id).await;

    // Should receive an error message since room is no longer in lobby state
    let msg = rx1.try_recv().unwrap();
    match msg.as_ref() {
        ServerMessage::Error { message, .. } => {
            assert!(message.contains("Room must be in lobby state"));
        }
        _ => panic!("Expected error message, but received: {msg:?}"),
    }
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

    // Clear initial messages
    let room_joined_msg1 = rx1.try_recv().unwrap();
    let room_joined_msg2 = rx2.try_recv().unwrap();

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

    // Clear remaining messages
    let _ = rx1.try_recv(); // PlayerJoined
    let _ = rx1.try_recv(); // LobbyStateChanged
    let _ = rx2.try_recv(); // LobbyStateChanged

    // Both players signal ready
    server.handle_player_ready(&player1_id).await;
    let _ = rx1.try_recv(); // LobbyStateChanged
    let _ = rx2.try_recv(); // LobbyStateChanged

    server.handle_player_ready(&player2_id).await;
    let _ = rx1.try_recv(); // LobbyStateChanged
    let _ = rx2.try_recv(); // LobbyStateChanged

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
    let _ = spectator_rx.try_recv();
    let _ = player_rx.try_recv();

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
