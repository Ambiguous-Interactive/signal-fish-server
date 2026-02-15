use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

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

    // Receive RoomJoined message
    let msg1 = timeout(Duration::from_secs(5), read1.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    if let Message::Text(text) = msg1 {
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["type"], "RoomJoined");
        assert_eq!(parsed["data"]["lobby_state"], "waiting");
        assert_eq!(parsed["data"]["ready_players"].as_array().unwrap().len(), 0);
    }

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

    // Client 2 receives RoomJoined
    let msg2 = timeout(Duration::from_secs(5), read2.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    if let Message::Text(text) = msg2 {
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["type"], "RoomJoined");
    }

    // Client 1 receives PlayerJoined notification
    let msg1 = timeout(Duration::from_secs(5), read1.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    if let Message::Text(text) = msg1 {
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["type"], "PlayerJoined");
    }

    // Both clients should receive LobbyStateChanged (room is now full)
    let msg1 = timeout(Duration::from_secs(5), read1.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let msg2 = timeout(Duration::from_secs(5), read2.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    for msg in [msg1, msg2] {
        if let Message::Text(text) = msg {
            let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(parsed["type"], "LobbyStateChanged");
            assert_eq!(parsed["data"]["lobby_state"], "lobby");
            assert_eq!(parsed["data"]["ready_players"].as_array().unwrap().len(), 0);
            assert_eq!(parsed["data"]["all_ready"], false);
        }
    }

    // Client 1 signals ready
    let ready_msg = json!({
        "type": "PlayerReady"
    });

    write1
        .send(Message::Text(ready_msg.to_string().into()))
        .await
        .unwrap();

    // Both clients should receive LobbyStateChanged with 1 ready player
    let msg1 = timeout(Duration::from_secs(5), read1.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let msg2 = timeout(Duration::from_secs(5), read2.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    for msg in [msg1, msg2] {
        if let Message::Text(text) = msg {
            let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(parsed["type"], "LobbyStateChanged");
            assert_eq!(parsed["data"]["lobby_state"], "lobby");
            assert_eq!(parsed["data"]["ready_players"].as_array().unwrap().len(), 1);
            assert_eq!(parsed["data"]["all_ready"], false);
        }
    }

    // Client 2 signals ready
    write2
        .send(Message::Text(ready_msg.to_string().into()))
        .await
        .unwrap();

    // Both clients should receive LobbyStateChanged with all_ready = true
    let msg1 = timeout(Duration::from_secs(5), read1.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let msg2 = timeout(Duration::from_secs(5), read2.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    for msg in [msg1, msg2] {
        if let Message::Text(text) = msg {
            let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(parsed["type"], "LobbyStateChanged");
            assert_eq!(parsed["data"]["lobby_state"], "lobby");
            assert_eq!(parsed["data"]["ready_players"].as_array().unwrap().len(), 2);
            assert_eq!(parsed["data"]["all_ready"], true);
        }
    }

    // Both clients should receive GameStarting message
    let msg1 = timeout(Duration::from_secs(5), read1.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let msg2 = timeout(Duration::from_secs(5), read2.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    for msg in [msg1, msg2] {
        if let Message::Text(text) = msg {
            let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(parsed["type"], "GameStarting");
            assert_eq!(
                parsed["data"]["peer_connections"].as_array().unwrap().len(),
                2
            );

            let peers = parsed["data"]["peer_connections"].as_array().unwrap();
            let auth_count = peers
                .iter()
                .filter(|p| p["is_authority"].as_bool().unwrap())
                .count();
            assert_eq!(auth_count, 1);
        }
    }
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
    write2
        .send(Message::Text(join_msg2.to_string().into()))
        .await
        .unwrap();

    // Clear initial messages
    for _ in 0..2 {
        let _ = read1.next().await;
    }
    for _ in 0..2 {
        let _ = read2.next().await;
    }

    // Player 1 signals ready
    let ready_msg = json!({"type": "PlayerReady"});
    write1
        .send(Message::Text(ready_msg.to_string().into()))
        .await
        .unwrap();

    // Clear LobbyStateChanged messages
    let _ = read1.next().await;
    let _ = read2.next().await;

    // Player 1 signals ready again (should toggle to not ready)
    write1
        .send(Message::Text(ready_msg.to_string().into()))
        .await
        .unwrap();

    // Should receive LobbyStateChanged with 0 ready players
    let msg1 = timeout(Duration::from_secs(5), read1.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    if let Message::Text(text) = msg1 {
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["type"], "LobbyStateChanged");
        assert_eq!(parsed["data"]["ready_players"].as_array().unwrap().len(), 0);
        assert_eq!(parsed["data"]["all_ready"], false);
    }

    // Player 2 should also receive the same message
    let msg2 = timeout(Duration::from_secs(5), read2.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    if let Message::Text(text) = msg2 {
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["type"], "LobbyStateChanged");
        assert_eq!(parsed["data"]["ready_players"].as_array().unwrap().len(), 0);
    }
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

    // Should receive error message
    let msg = timeout(Duration::from_secs(5), read.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    if let Message::Text(text) = msg {
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["type"], "Error");
        assert!(parsed["data"]["message"]
            .as_str()
            .unwrap()
            .contains("Not in a room"));
    }

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

    // Receive RoomJoined (should be in waiting state)
    let msg = timeout(Duration::from_secs(5), read.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    if let Message::Text(text) = msg {
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["type"], "RoomJoined");
        assert_eq!(parsed["data"]["lobby_state"], "waiting");
    }

    // Try to signal ready in non-lobby room
    write
        .send(Message::Text(ready_msg.to_string().into()))
        .await
        .unwrap();

    // Should not receive any response (no lobby transition for single-player rooms)
    let result = timeout(Duration::from_secs(2), read.next()).await;
    assert!(result.is_err()); // Timeout means no message received
}
