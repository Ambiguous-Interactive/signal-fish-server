mod websocket_test_helpers;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use signal_fish_server::protocol::{LobbyState, RoomJoinedPayload, ServerMessage};
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::{expect_no_server_message_within, next_server_message_within};

const SERVER_MESSAGE_TIMEOUT: Duration = Duration::from_secs(5);

// Note: These tests require a running signal-fish-server instance
// They are marked with #[ignore] by default to avoid running in normal test suite

#[tokio::test]
#[ignore = "requires running server"]
async fn test_lobby_e2e_websocket_flow() {
    let server_url = "ws://127.0.0.1:3536/v2/ws";

    // Connect two clients
    let (ws_stream1, _) = connect_async(server_url).await.unwrap();
    let (ws_stream2, _) = connect_async(server_url).await.unwrap();

    let (mut write1, mut read1) = ws_stream1.split();
    let (mut write2, mut read2) = ws_stream2.split();

    // Client 1 joins room
    let join_msg1 = json!({
        "type": "JoinRoom",
        "data": {
            "game_name": "e2e_lobby_test",
            "room_code": "E2E001",
            "player_name": "E2EPlayer1",
            "max_players": 2,
            "supports_authority": true
        }
    });

    write1
        .send(Message::Text(join_msg1.to_string().into()))
        .await
        .unwrap();

    let payload = expect_room_joined(&mut read1, "player 1 initial room join").await;
    assert_eq!(payload.lobby_state, LobbyState::Waiting);
    assert_eq!(payload.ready_players.len(), 0);

    // Client 2 joins the same room
    let join_msg2 = json!({
        "type": "JoinRoom",
        "data": {
            "game_name": "e2e_lobby_test",
            "room_code": "E2E001",
            "player_name": "E2EPlayer2",
            "max_players": 2,
            "supports_authority": true
        }
    });

    write2
        .send(Message::Text(join_msg2.to_string().into()))
        .await
        .unwrap();

    expect_room_joined(&mut read2, "player 2 initial room join").await;

    expect_player_joined(&mut read1, "player 1 sees player 2 join").await;

    // Both clients should receive LobbyStateChanged (room is now full)
    expect_lobby_state_changed(&mut read1, "player 1 sees full lobby", 0, false).await;
    expect_lobby_state_changed(&mut read2, "player 2 sees full lobby", 0, false).await;

    // Client 1 signals ready
    let ready_msg = json!({
        "type": "PlayerReady"
    });

    write1
        .send(Message::Text(ready_msg.to_string().into()))
        .await
        .unwrap();

    expect_lobby_state_changed(&mut read1, "player 1 sees first ready", 1, false).await;
    expect_lobby_state_changed(&mut read2, "player 2 sees first ready", 1, false).await;

    // Client 2 signals ready
    write2
        .send(Message::Text(ready_msg.to_string().into()))
        .await
        .unwrap();

    expect_lobby_state_changed(&mut read1, "player 1 sees all ready", 2, true).await;
    expect_lobby_state_changed(&mut read2, "player 2 sees all ready", 2, true).await;

    // Both clients should receive GameStarting message
    expect_game_starting(&mut read1, "player 1 game start").await;
    expect_game_starting(&mut read2, "player 2 game start").await;
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_lobby_e2e_ready_toggle() {
    let server_url = "ws://127.0.0.1:3536/v2/ws";

    // Connect two clients
    let (ws_stream1, _) = connect_async(server_url).await.unwrap();
    let (ws_stream2, _) = connect_async(server_url).await.unwrap();

    let (mut write1, mut read1) = ws_stream1.split();
    let (mut write2, mut read2) = ws_stream2.split();

    // Join room (same as previous test but abbreviated)
    let join_msg1 = json!({
        "type": "JoinRoom",
        "data": {
            "game_name": "e2e_toggle_test",
            "room_code": "TOGGLE",
            "player_name": "TogglePlayer1",
            "max_players": 2,
            "supports_authority": true
        }
    });

    let join_msg2 = json!({
        "type": "JoinRoom",
        "data": {
            "game_name": "e2e_toggle_test",
            "room_code": "TOGGLE",
            "player_name": "TogglePlayer2",
            "max_players": 2,
            "supports_authority": true
        }
    });

    write1
        .send(Message::Text(join_msg1.to_string().into()))
        .await
        .unwrap();
    expect_room_joined(&mut read1, "player 1 initial room join").await;

    write2
        .send(Message::Text(join_msg2.to_string().into()))
        .await
        .unwrap();
    expect_room_joined(&mut read2, "player 2 initial room join").await;
    expect_player_joined(&mut read1, "player 1 sees player 2 join").await;
    expect_lobby_state_changed(&mut read1, "player 1 sees full lobby", 0, false).await;
    expect_lobby_state_changed(&mut read2, "player 2 sees full lobby", 0, false).await;

    // Player 1 signals ready
    let ready_msg = json!({"type": "PlayerReady"});
    write1
        .send(Message::Text(ready_msg.to_string().into()))
        .await
        .unwrap();

    expect_lobby_state_changed(&mut read1, "player 1 sees first ready toggle", 1, false).await;
    expect_lobby_state_changed(&mut read2, "player 2 sees first ready toggle", 1, false).await;

    // Player 1 signals ready again (should toggle to not ready)
    write1
        .send(Message::Text(ready_msg.to_string().into()))
        .await
        .unwrap();

    expect_lobby_state_changed(&mut read1, "player 1 sees ready toggle reset", 0, false).await;
    expect_lobby_state_changed(&mut read2, "player 2 sees ready toggle reset", 0, false).await;
}

#[tokio::test]
#[ignore = "requires running server"]
async fn test_lobby_e2e_error_cases() {
    let server_url = "ws://127.0.0.1:3536/v2/ws";

    // Connect client
    let (ws_stream, _) = connect_async(server_url).await.unwrap();
    let (mut write, mut read) = ws_stream.split();

    // Try to signal ready without being in a room
    let ready_msg = json!({"type": "PlayerReady"});
    write
        .send(Message::Text(ready_msg.to_string().into()))
        .await
        .unwrap();

    expect_error_contains(&mut read, "ready without room error", "Not in a room").await;

    // Join a single-player room
    let join_msg = json!({
        "type": "JoinRoom",
        "data": {
            "game_name": "single_game",
            "room_code": "SINGLE",
            "player_name": "Solo",
            "max_players": 1,
            "supports_authority": true
        }
    });

    write
        .send(Message::Text(join_msg.to_string().into()))
        .await
        .unwrap();

    let payload = expect_room_joined(&mut read, "single player room join").await;
    assert_eq!(payload.lobby_state, LobbyState::Waiting);

    // Try to signal ready in non-lobby room
    write
        .send(Message::Text(ready_msg.to_string().into()))
        .await
        .unwrap();

    expect_no_server_message_within(
        &mut read,
        Duration::from_secs(2),
        "single-player ready should not emit a server message",
    )
    .await;
}

async fn expect_room_joined<S>(read: &mut S, context: &str) -> Box<RoomJoinedPayload>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    match next_server_message_within(read, SERVER_MESSAGE_TIMEOUT, context).await {
        ServerMessage::RoomJoined(payload) => payload,
        other => panic!("{context}: expected RoomJoined, got {other:?}"),
    }
}

async fn expect_player_joined<S>(read: &mut S, context: &str)
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    match next_server_message_within(read, SERVER_MESSAGE_TIMEOUT, context).await {
        ServerMessage::PlayerJoined { .. } => {}
        other => panic!("{context}: expected PlayerJoined, got {other:?}"),
    }
}

async fn expect_lobby_state_changed<S>(
    read: &mut S,
    context: &str,
    expected_ready_players: usize,
    expected_all_ready: bool,
) where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    match next_server_message_within(read, SERVER_MESSAGE_TIMEOUT, context).await {
        ServerMessage::LobbyStateChanged {
            lobby_state,
            ready_players,
            all_ready,
            ..
        } => {
            assert_eq!(
                lobby_state,
                LobbyState::Lobby,
                "{context}: lobby state should be lobby"
            );
            assert_eq!(
                ready_players.len(),
                expected_ready_players,
                "{context}: ready player count"
            );
            assert_eq!(all_ready, expected_all_ready, "{context}: all_ready flag");
        }
        other => panic!("{context}: expected LobbyStateChanged, got {other:?}"),
    }
}

async fn expect_game_starting<S>(read: &mut S, context: &str)
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    match next_server_message_within(read, SERVER_MESSAGE_TIMEOUT, context).await {
        ServerMessage::GameStarting { peer_connections } => {
            assert_eq!(peer_connections.len(), 2, "{context}: peer count");
            let authority_count = peer_connections
                .iter()
                .filter(|peer| peer.is_authority)
                .count();
            assert_eq!(authority_count, 1, "{context}: authority peer count");
        }
        other => panic!("{context}: expected GameStarting, got {other:?}"),
    }
}

async fn expect_error_contains<S>(read: &mut S, context: &str, expected_message: &str)
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    match next_server_message_within(read, SERVER_MESSAGE_TIMEOUT, context).await {
        ServerMessage::Error { message, .. } => {
            assert!(
                message.contains(expected_message),
                "{context}: expected error to contain {expected_message:?}, got {message:?}"
            );
        }
        other => panic!("{context}: expected Error, got {other:?}"),
    }
}
