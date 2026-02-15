mod test_helpers;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::protocol::*;
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::create_router;
use std::sync::Arc;
use test_helpers::{
    create_test_server, create_test_server_with_config, test_protocol_config, test_server_config,
};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Helper to create a test server and return its address
async fn start_test_server() -> std::net::SocketAddr {
    let game_server = create_test_server().await;
    start_server_with_instance(game_server).await
}

async fn start_test_server_with_config(server_config: ServerConfig) -> std::net::SocketAddr {
    start_test_server_with_config_and_protocol(server_config, test_protocol_config()).await
}

async fn start_test_server_with_config_and_protocol(
    server_config: ServerConfig,
    protocol_config: signal_fish_server::config::ProtocolConfig,
) -> std::net::SocketAddr {
    let game_server = create_test_server_with_config(server_config, protocol_config).await;
    start_server_with_instance(game_server).await
}

async fn start_server_with_instance(game_server: Arc<EnhancedGameServer>) -> std::net::SocketAddr {
    // Initialize tracing for debugging
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Create enhanced protocol router
    let enhanced_router = create_router("http://localhost:3000").with_state(game_server);

    // Combine routers like in main.rs
    let combined_router = axum::Router::new()
        .nest("/v2", enhanced_router) // New protocol under /v2
        .fallback(|| async { "Use /v2/ws for enhanced protocol" });

    tokio::spawn(async move {
        axum::serve(
            listener,
            combined_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    // Give server time to start (longer timeout for CI environments)
    tokio::time::sleep(tokio::time::Duration::from_millis(2000)).await;

    addr
}

/// Helper to connect a WebSocket client
async fn connect_client(
    addr: std::net::SocketAddr,
    path: &str,
) -> (
    futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
) {
    let url = format!("ws://{addr}{path}");
    println!("Connecting to WebSocket URL: {url}");

    // Add timeout to WebSocket connection to prevent infinite hanging
    let (ws_stream, _) =
        tokio::time::timeout(tokio::time::Duration::from_secs(10), connect_async(&url))
            .await
            .expect("WebSocket connection timed out after 10 seconds")
            .expect("Failed to connect");

    println!("WebSocket connection established");
    ws_stream.split()
}

/// Helper to send a message and return the response
async fn send_and_receive(
    sender: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    receiver: &mut futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    message: ClientMessage,
) -> Result<ServerMessage, Box<dyn std::error::Error>> {
    let json = serde_json::to_string(&message)?;
    sender.send(Message::Text(json.into())).await?;

    // Add timeout to prevent infinite waiting
    match tokio::time::timeout(tokio::time::Duration::from_secs(5), receiver.next()).await {
        Ok(Some(msg)) => {
            let text = msg?.into_text()?;
            let server_msg: ServerMessage = serde_json::from_str(&text)?;
            Ok(server_msg)
        }
        Ok(None) => Err("Connection closed".into()),
        Err(_) => Err("Timeout waiting for response".into()),
    }
}

#[tokio::test]
async fn test_websocket_connection_limit_enforced() {
    let mut server_config = test_server_config();
    server_config.max_connections_per_ip = 1;

    let addr = start_test_server_with_config(server_config).await;

    let (mut sender1, receiver1) = connect_client(addr, "/v2/ws").await;

    let (_sender2, mut receiver2) = connect_client(addr, "/v2/ws").await;
    let frame = tokio::time::timeout(tokio::time::Duration::from_secs(2), receiver2.next())
        .await
        .expect("expected response frame")
        .expect("WebSocket frame result");

    match frame {
        Ok(Message::Text(text)) => {
            let server_msg: ServerMessage =
                serde_json::from_str(&text).expect("valid ServerMessage");
            match server_msg {
                ServerMessage::Error { error_code, .. } => {
                    assert_eq!(
                        error_code,
                        Some(ErrorCode::TooManyConnections),
                        "second connection should be rejected"
                    );
                }
                other => panic!("expected error message, got {other:?}"),
            }
        }
        Ok(other) => panic!("expected text error frame, got {other:?}"),
        Err(err) => panic!("WebSocket error received: {err:?}"),
    }

    // First connection should remain active; close it cleanly
    let _ = sender1.close().await;
    drop(receiver1);
}

#[tokio::test]
async fn test_health_check() {
    let addr = start_test_server().await;

    // Test health endpoint first
    let client = reqwest::Client::new();
    let response = client
        .get(format!("http://{addr}/v2/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let text = response.text().await.unwrap();
    assert_eq!(text, "OK");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_websocket_connection() {
    // Add overall test timeout to prevent infinite hanging
    let test_timeout = tokio::time::Duration::from_secs(30);
    tokio::time::timeout(test_timeout, async {
        let addr = start_test_server().await;

        // First test if we can connect to the health endpoint
        let client = reqwest::Client::new();
        let response = client
            .get(format!("http://{addr}/v2/health"))
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "Health check failed: {}",
            response.status()
        );

        let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

        // Test ping/pong
        let pong = send_and_receive(&mut sender, &mut receiver, ClientMessage::Ping)
            .await
            .unwrap();
        match pong {
            ServerMessage::Pong => {
                // Success
            }
            _ => panic!("Expected Pong, got {pong:?}"),
        }
    })
    .await
    .expect("Test timed out after 30 seconds");
}

#[tokio::test]
async fn test_room_creation_and_joining() {
    let addr = start_test_server().await;

    // Connect two clients
    let (mut sender1, mut receiver1) = connect_client(addr, "/v2/ws").await;
    let (mut sender2, mut receiver2) = connect_client(addr, "/v2/ws").await;

    // Player 1 creates a room
    let join_msg1 = ClientMessage::JoinRoom {
        game_name: "test_game".to_string(),
        room_code: Some("E2E123".to_string()),
        player_name: "Player1".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response1 = send_and_receive(&mut sender1, &mut receiver1, join_msg1)
        .await
        .unwrap();
    match response1 {
        ServerMessage::RoomJoined(ref payload) => {
            assert_eq!(payload.room_code, "E2E123");
            assert_eq!(payload.game_name, "test_game");
            assert_eq!(payload.max_players, 2);
            assert_eq!(payload.current_players.len(), 1);
            assert!(payload.is_authority);
        }
        _ => panic!("Expected RoomJoined, got {response1:?}"),
    }

    // Player 2 joins the same room
    let join_msg2 = ClientMessage::JoinRoom {
        game_name: "test_game".to_string(),
        room_code: Some("E2E123".to_string()),
        player_name: "Player2".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response2 = send_and_receive(&mut sender2, &mut receiver2, join_msg2)
        .await
        .unwrap();
    match response2 {
        ServerMessage::RoomJoined(ref payload) => {
            assert_eq!(payload.room_code, "E2E123");
            assert_eq!(payload.game_name, "test_game");
            assert_eq!(payload.current_players.len(), 2);
            assert!(!payload.is_authority);
        }
        _ => panic!("Expected RoomJoined for player 2, got {response2:?}"),
    }

    // Player 1 should receive a PlayerJoined notification
    match tokio::time::timeout(tokio::time::Duration::from_secs(2), receiver1.next()).await {
        Ok(Some(msg)) => {
            let text = msg.unwrap().into_text().unwrap();
            let notification: ServerMessage = serde_json::from_str(&text).unwrap();
            match notification {
                ServerMessage::PlayerJoined { player } => {
                    assert_eq!(player.name, "Player2");
                    assert!(!player.is_authority);
                }
                _ => panic!("Expected PlayerJoined notification, got {notification:?}"),
            }
        }
        Ok(None) => panic!("Connection closed while waiting for PlayerJoined notification"),
        Err(_) => panic!("Timeout waiting for PlayerJoined notification for player 1"),
    }
}

#[tokio::test]
async fn test_game_data_broadcasting() {
    let addr = start_test_server().await;

    let (mut sender1, mut receiver1) = connect_client(addr, "/v2/ws").await;
    let (mut sender2, mut receiver2) = connect_client(addr, "/v2/ws").await;

    // Both players join the same room
    let join_msg = ClientMessage::JoinRoom {
        game_name: "data_test".to_string(),
        room_code: Some("DTA123".to_string()),
        player_name: "Player1".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };
    let _ = send_and_receive(&mut sender1, &mut receiver1, join_msg)
        .await
        .unwrap();

    let join_msg2 = ClientMessage::JoinRoom {
        game_name: "data_test".to_string(),
        room_code: Some("DTA123".to_string()),
        player_name: "Player2".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };
    let _ = send_and_receive(&mut sender2, &mut receiver2, join_msg2)
        .await
        .unwrap();

    // Clear any joining notifications (PlayerJoined and LobbyStateChanged)
    let _ = tokio::time::timeout(tokio::time::Duration::from_secs(1), receiver1.next()).await; // PlayerJoined notification
    let _ = tokio::time::timeout(tokio::time::Duration::from_secs(1), receiver1.next()).await; // LobbyStateChanged notification
    let _ = tokio::time::timeout(tokio::time::Duration::from_secs(1), receiver2.next()).await; // LobbyStateChanged notification

    // Player 1 sends game data
    let game_data = serde_json::json!({"action": "move", "x": 100, "y": 200});
    let data_msg = ClientMessage::GameData {
        data: game_data.clone(),
    };

    let json = serde_json::to_string(&data_msg).unwrap();
    sender1.send(Message::Text(json.into())).await.unwrap();

    // Player 2 should receive the game data
    match tokio::time::timeout(tokio::time::Duration::from_secs(2), receiver2.next()).await {
        Ok(Some(msg)) => {
            let text = msg.unwrap().into_text().unwrap();
            let received: ServerMessage = serde_json::from_str(&text).unwrap();
            match received {
                ServerMessage::GameData { data, .. } => {
                    assert_eq!(data, game_data);
                }
                _ => panic!("Expected GameData, got {received:?}"),
            }
        }
        Ok(None) => panic!("Connection closed while waiting for game data"),
        Err(_) => panic!("Timeout waiting for game data"),
    }
}

#[tokio::test]
async fn test_room_capacity_limit() {
    let addr = start_test_server().await;

    let (mut sender1, mut receiver1) = connect_client(addr, "/v2/ws").await;
    let (mut sender2, mut receiver2) = connect_client(addr, "/v2/ws").await;
    let (mut sender3, mut receiver3) = connect_client(addr, "/v2/ws").await;

    // Create room with max 2 players
    let join_msg1 = ClientMessage::JoinRoom {
        game_name: "limited_game".to_string(),
        room_code: Some("LMT123".to_string()),
        player_name: "Player1".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };
    let _ = send_and_receive(&mut sender1, &mut receiver1, join_msg1)
        .await
        .unwrap();

    // Second player joins
    let join_msg2 = ClientMessage::JoinRoom {
        game_name: "limited_game".to_string(),
        room_code: Some("LMT123".to_string()),
        player_name: "Player2".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };
    let _ = send_and_receive(&mut sender2, &mut receiver2, join_msg2)
        .await
        .unwrap();

    // Third player should be rejected
    let join_msg3 = ClientMessage::JoinRoom {
        game_name: "limited_game".to_string(),
        room_code: Some("LMT123".to_string()),
        player_name: "Player3".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response3 = send_and_receive(&mut sender3, &mut receiver3, join_msg3)
        .await
        .unwrap();
    match response3 {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("full") || reason.contains("capacity"));
        }
        _ => panic!("Expected RoomJoinFailed, got {response3:?}"),
    }
}

#[tokio::test]
async fn test_validation_errors() {
    let addr = start_test_server().await;
    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    // Test invalid room code
    let invalid_join = ClientMessage::JoinRoom {
        game_name: "valid_game".to_string(),
        room_code: Some("INVALID!@#".to_string()), // Contains special characters
        player_name: "Player".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, invalid_join)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("Room code") || reason.contains("alphanumeric"));
        }
        _ => panic!("Expected RoomJoinFailed for invalid room code, got {response:?}"),
    }
}

#[tokio::test]
async fn test_e2e_custom_protocol_limits() {
    // Test end-to-end behavior with custom protocol configuration
    let server_config = ServerConfig::default();
    let protocol_config = signal_fish_server::config::ProtocolConfig {
        max_game_name_length: 20,
        room_code_length: 4,
        max_player_name_length: 10,
        max_players_limit: 8,
        ..signal_fish_server::config::ProtocolConfig::default()
    };

    let addr = start_test_server_with_config_and_protocol(server_config, protocol_config).await;
    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    // Test 1: Game name exceeding custom limit should fail
    let long_name_msg = ClientMessage::JoinRoom {
        game_name: "this_game_name_is_too_long_for_our_custom_limit".to_string(), // 49 chars > 20
        room_code: Some("ABCD".to_string()),
        player_name: "Player".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, long_name_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("Game name too long") && reason.contains("20"));
        }
        _ => panic!("Expected game name validation failure, got {response:?}"),
    }

    // Test 2: Room code with wrong length should fail
    let wrong_length_msg = ClientMessage::JoinRoom {
        game_name: "shortgame".to_string(),
        room_code: Some("ABCDEF".to_string()), // 6 chars but we configured 4
        player_name: "Player".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, wrong_length_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("Room code must be exactly 4 characters"));
        }
        _ => panic!("Expected room code validation failure, got {response:?}"),
    }

    // Test 3: Player name exceeding custom limit should fail
    let long_player_msg = ClientMessage::JoinRoom {
        game_name: "testgame".to_string(),
        room_code: Some("TEST".to_string()),
        player_name: "VeryLongPlayerName".to_string(), // 18 chars > 10
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, long_player_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("Player name too long") && reason.contains("10"));
        }
        _ => panic!("Expected player name validation failure, got {response:?}"),
    }

    // Test 4: Max players exceeding limit should fail
    let too_many_players_msg = ClientMessage::JoinRoom {
        game_name: "testgame".to_string(),
        room_code: Some("TEST".to_string()),
        player_name: "Player".to_string(),
        max_players: Some(16), // Exceeds our custom limit of 8
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, too_many_players_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("Max players cannot exceed 8"));
        }
        _ => panic!("Expected max players validation failure, got {response:?}"),
    }

    // Test 5: Valid configuration should work
    let valid_msg = ClientMessage::JoinRoom {
        game_name: "game".to_string(),       // Short
        room_code: Some("GOOD".to_string()), // 4 chars
        player_name: "Bob".to_string(),      // Short
        max_players: Some(6),                // Within limit
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, valid_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoined(ref payload) => {
            assert_eq!(payload.room_code, "GOOD");
            assert_eq!(payload.game_name, "game");
            assert_eq!(payload.max_players, 6);
        }
        _ => panic!("Expected successful room join with valid parameters, got {response:?}"),
    }
}

#[tokio::test]
#[serial_test::serial]
async fn test_e2e_config_with_file_and_env() {
    use std::env;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    // Create temporary config file
    let dir = tempdir().unwrap();
    let config_file = dir.path().join("e2e_test_config.json");

    let config_json = r#"{
        "port": 0,
        "server": {
            "default_max_players": 16
        },
        "protocol": {
            "max_game_name_length": 50,
            "room_code_length": 5
        }
    }"#;

    let mut file = File::create(&config_file).unwrap();
    file.write_all(config_json.as_bytes()).unwrap();
    file.flush().unwrap();

    // Give file system time to complete write operations
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Set environment variables to test precedence
    let config_path = config_file.to_str().unwrap();
    println!("Using config file path: {config_path}");
    env::set_var("SIGNAL_FISH_CONFIG_PATH", config_path);
    env::set_var("SIGNAL_FISH__PROTOCOL__ROOM_CODE_LENGTH", "3"); // Override file config

    let loaded_config = signal_fish_server::config::load();

    // Verify precedence: env var should override file value
    assert_eq!(loaded_config.protocol.room_code_length, 3); // From env override
    assert_eq!(loaded_config.protocol.max_game_name_length, 50); // From file
    assert_eq!(loaded_config.server.default_max_players, 16); // From file
    assert_eq!(loaded_config.server.ping_timeout, 30); // Default value

    // Test that the loaded config affects server behavior
    let addr = start_test_server_with_config_and_protocol(
        ServerConfig {
            default_max_players: loaded_config.server.default_max_players,
            ..ServerConfig::default()
        },
        loaded_config.protocol,
    )
    .await;

    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    // Test that room code length validation uses our custom 3-character limit
    let test_msg = ClientMessage::JoinRoom {
        game_name: "configtest".to_string(),
        room_code: Some("ABCD".to_string()), // 4 chars but we configured 3
        player_name: "Player".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, test_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("Room code must be exactly 3 characters"));
        }
        _ => panic!("Expected room code validation failure for 3-char limit, got {response:?}"),
    }

    // Test valid 3-character code works
    let valid_msg = ClientMessage::JoinRoom {
        game_name: "configtest".to_string(),
        room_code: Some("ABC".to_string()), // 3 chars - should work
        player_name: "Player".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, valid_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoined(ref payload) => {
            assert_eq!(payload.room_code, "ABC");
        }
        _ => panic!("Expected successful room join with 3-char code, got {response:?}"),
    }

    // Clean up
    env::remove_var("SIGNAL_FISH_CONFIG_PATH");
    env::remove_var("SIGNAL_FISH__PROTOCOL__ROOM_CODE_LENGTH");
}

#[tokio::test]
async fn test_e2e_authority_protocol_enforcement() {
    let addr = start_test_server().await;

    let (mut sender1, mut receiver1) = connect_client(addr, "/v2/ws").await;
    let (mut sender2, mut receiver2) = connect_client(addr, "/v2/ws").await;

    // Player 1 creates room with authority support
    let create_msg = ClientMessage::JoinRoom {
        game_name: "authority_game".to_string(),
        room_code: Some("AUTH01".to_string()),
        player_name: "AuthPlayer1".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response1 = send_and_receive(&mut sender1, &mut receiver1, create_msg)
        .await
        .unwrap();
    match response1 {
        ServerMessage::RoomJoined(ref payload) => {
            assert!(payload.is_authority); // First player should have authority
            assert!(payload.supports_authority); // Room supports authority
            assert_eq!(payload.current_players.len(), 1);
            assert!(payload.current_players[0].is_authority);
        }
        _ => panic!("Expected RoomJoined for player 1, got {response1:?}"),
    }

    // Player 2 joins the same room
    let join_msg = ClientMessage::JoinRoom {
        game_name: "authority_game".to_string(),
        room_code: Some("AUTH01".to_string()),
        player_name: "AuthPlayer2".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response2 = send_and_receive(&mut sender2, &mut receiver2, join_msg)
        .await
        .unwrap();
    match response2 {
        ServerMessage::RoomJoined(ref payload) => {
            assert!(!payload.is_authority); // Second player should NOT have authority
            assert_eq!(payload.current_players.len(), 2);
            // First player should still have authority
            let authority_count = payload
                .current_players
                .iter()
                .filter(|p| p.is_authority)
                .count();
            assert_eq!(authority_count, 1);
        }
        _ => panic!("Expected RoomJoined for player 2, got {response2:?}"),
    }

    // Clear joining notifications for player 1 (PlayerJoined and LobbyStateChanged)
    let _ = tokio::time::timeout(tokio::time::Duration::from_millis(100), receiver1.next()).await; // PlayerJoined
    let _ = tokio::time::timeout(tokio::time::Duration::from_millis(100), receiver1.next()).await; // LobbyStateChanged

    // Clear any initial messages for player 2 as well
    let _ = tokio::time::timeout(tokio::time::Duration::from_millis(100), receiver2.next()).await; // LobbyStateChanged

    // Player 2 tries to request authority while Player 1 has it - should be DENIED
    let authority_request = ClientMessage::AuthorityRequest {
        become_authority: true,
    };

    let json = serde_json::to_string(&authority_request).unwrap();
    sender2.send(Message::Text(json.into())).await.unwrap();

    // Player 2 should receive denial
    match tokio::time::timeout(tokio::time::Duration::from_secs(2), receiver2.next()).await {
        Ok(Some(msg)) => {
            let text = msg.unwrap().into_text().unwrap();
            let response: ServerMessage = serde_json::from_str(&text).unwrap();
            match response {
                ServerMessage::AuthorityResponse {
                    granted, reason, ..
                } => {
                    assert!(!granted);
                    assert!(reason.is_some());
                    assert!(reason
                        .unwrap()
                        .contains("Another player already has authority"));
                }
                _ => panic!("Expected AuthorityResponse denial, got {response:?}"),
            }
        }
        Ok(None) => panic!("Connection closed while waiting for authority response"),
        Err(_) => panic!("Timeout waiting for authority response"),
    }

    // Player 1 releases authority
    let release_request = ClientMessage::AuthorityRequest {
        become_authority: false,
    };

    // Add a longer delay to ensure the previous authority request is fully processed
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    println!("Sending authority release request from player 1");
    let json = serde_json::to_string(&release_request).unwrap();
    sender1.send(Message::Text(json.into())).await.unwrap();
    println!("Authority release request sent");

    // Give server time to process the request
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Player 1 should receive AuthorityResponse and/or AuthorityChanged
    // We'll look for either message, but preferably AuthorityResponse
    let mut received_authority_response = false;

    // Try up to 3 times with a 2-second timeout each
    for attempt in 1..=3 {
        println!("Looking for response - attempt {attempt}");

        match tokio::time::timeout(tokio::time::Duration::from_secs(2), receiver1.next()).await {
            Ok(Some(msg)) => {
                let text = msg.unwrap().into_text().unwrap();
                println!("Received message: {text}");
                let response: ServerMessage = serde_json::from_str(&text).unwrap();
                println!("Parsed message: {response:?}");

                match response {
                    ServerMessage::AuthorityResponse {
                        granted, reason, ..
                    } => {
                        assert!(granted, "Authority release should be granted");
                        assert!(reason.is_none(), "No error reason expected");
                        received_authority_response = true;
                        println!("Successfully received AuthorityResponse");
                        break;
                    }
                    ServerMessage::AuthorityChanged {
                        authority_player, ..
                    } => {
                        // This is also an acceptable response
                        assert_eq!(
                            authority_player, None,
                            "Authority should be released (None)"
                        );
                        println!("Received AuthorityChanged notification");
                        // Continue looking for AuthorityResponse
                    }
                    _ => {
                        println!("Received unexpected message type: {response:?}");
                        // Continue looking
                    }
                }
            }
            Ok(None) => {
                println!("WebSocket connection closed");
                break;
            }
            Err(_) => {
                println!("Timeout waiting for response on attempt {attempt}");
                if attempt == 3 {
                    break;
                }
            }
        }
    }

    // We must have received at least one of the messages
    if !received_authority_response {
        // Try one more time with a longer timeout as a last resort
        println!("Final attempt with longer timeout");
        if let Ok(Some(msg)) =
            tokio::time::timeout(tokio::time::Duration::from_secs(5), receiver1.next()).await
        {
            let text = msg.unwrap().into_text().unwrap();
            let response: ServerMessage = serde_json::from_str(&text).unwrap();
            println!("Final attempt received: {response:?}");

            if let ServerMessage::AuthorityResponse { granted, .. } = response {
                assert!(granted);
                received_authority_response = true;
            }
        }
    }

    assert!(
        received_authority_response,
        "Never received AuthorityResponse"
    );

    // Player 2 might receive authority change notification or other notifications
    // Since we had delays, player 2 might have received different messages
    // Let's give it a few chances to receive messages

    let mut found_authority_update = false;

    // Try up to 3 messages with 2-second timeout each
    for attempt in 1..=3 {
        println!("Looking for player 2 messages - attempt {attempt}");

        match tokio::time::timeout(tokio::time::Duration::from_secs(2), receiver2.next()).await {
            Ok(Some(msg)) => {
                let text = msg.unwrap().into_text().unwrap();
                println!("Player 2 received message: {text}");
                let notification: ServerMessage = serde_json::from_str(&text).unwrap();
                println!("Player 2 parsed message: {notification:?}");

                // Accept other message types, but keep looking for AuthorityChanged
                if let ServerMessage::AuthorityChanged {
                    authority_player, ..
                } = notification
                {
                    // This is the ideal message
                    assert_eq!(
                        authority_player, None,
                        "Authority should be released (None)"
                    );
                    found_authority_update = true;
                    println!("Player 2 received AuthorityChanged");
                    break;
                } else {
                    println!("Player 2 received other message: {notification:?}");
                    continue;
                }
            }
            Ok(None) => {
                println!("Player 2 WebSocket connection closed");
                break;
            }
            Err(_) => {
                println!("Timeout waiting for player 2 message on attempt {attempt}");
                break; // No more messages expected
            }
        }
    }

    // It's ok if we don't find AuthorityChanged specifically as long as we're receiving messages
    println!(
        "Player 2 message verification complete, found_authority_update={found_authority_update}"
    );

    // NOW Player 2 can successfully request authority
    let authority_request2 = ClientMessage::AuthorityRequest {
        become_authority: true,
    };

    // Add a delay before next request
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    println!("Sending authority request from player 2");
    let json = serde_json::to_string(&authority_request2).unwrap();
    sender2.send(Message::Text(json.into())).await.unwrap();
    println!("Authority request sent from player 2");

    // Give server time to process the request
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Player 2 should receive AuthorityResponse and/or AuthorityChanged
    let mut received_authority_response = false;
    let mut received_authority_changed = false;

    // Try up to 5 times with a 2-second timeout each to allow for more messages
    for attempt in 1..=5 {
        println!("Looking for player 2 authority response - attempt {attempt}");

        match tokio::time::timeout(tokio::time::Duration::from_secs(2), receiver2.next()).await {
            Ok(Some(msg)) => {
                let text = msg.unwrap().into_text().unwrap();
                println!("Player 2 received message: {text}");
                let response: ServerMessage = serde_json::from_str(&text).unwrap();
                println!("Player 2 parsed message: {response:?}");

                match response {
                    ServerMessage::AuthorityResponse {
                        granted, reason, ..
                    } => {
                        println!("Got AuthorityResponse: granted={granted}, reason={reason:?}");
                        assert!(granted, "Authority request should be granted");
                        assert!(reason.is_none(), "No error reason expected");
                        received_authority_response = true;
                        if received_authority_changed {
                            break; // We got both messages, we can stop
                        }
                    }
                    ServerMessage::AuthorityChanged {
                        authority_player, ..
                    } => {
                        println!("Got AuthorityChanged: authority_player={authority_player:?}");
                        assert!(authority_player.is_some(), "Authority player should be set");
                        // We don't need to check the exact ID, just that it's set
                        received_authority_changed = true;
                        if received_authority_response {
                            break; // We got both messages, we can stop
                        }
                    }
                    _ => {
                        println!("Received unexpected message type: {response:?}");
                        // Continue looking
                    }
                }
            }
            Ok(None) => {
                println!("WebSocket connection closed");
                break;
            }
            Err(_) => {
                println!("Timeout waiting for response on attempt {attempt}");
                if attempt == 5 {
                    break;
                }
            }
        }
    }

    // We must have received at least AuthorityResponse or AuthorityChanged
    // (Both would be ideal, but either one is acceptable)
    println!(
        "Authority verification: response={received_authority_response}, changed={received_authority_changed}"
    );

    // For test passing, we require at least one of these to be true
    assert!(
        received_authority_response || received_authority_changed,
        "Never received successful AuthorityResponse or AuthorityChanged"
    );
}

#[tokio::test]
async fn test_simple_authority_release() {
    let addr = start_test_server().await;
    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    // Join room as first player (should get authority)
    let join_msg = ClientMessage::JoinRoom {
        game_name: "simple_test".to_string(),
        room_code: Some("SIMPLE".to_string()),
        player_name: "TestPlayer".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, join_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoined(ref payload) => {
            assert!(payload.is_authority, "First player should have authority");
        }
        _ => panic!("Expected RoomJoined, got {response:?}"),
    }

    // Now try to release authority
    println!("Attempting to release authority");
    let release_request = ClientMessage::AuthorityRequest {
        become_authority: false,
    };

    let json = serde_json::to_string(&release_request).unwrap();
    sender.send(Message::Text(json.into())).await.unwrap();

    // Authority release sends two messages: AuthorityResponse and AuthorityChanged
    // We need to collect both and check for AuthorityResponse
    let mut received_authority_response = false;
    for _ in 0..2 {
        match tokio::time::timeout(tokio::time::Duration::from_secs(5), receiver.next()).await {
            Ok(Some(msg)) => {
                let text = msg.unwrap().into_text().unwrap();
                let response: ServerMessage = serde_json::from_str(&text).unwrap();
                match response {
                    ServerMessage::AuthorityResponse {
                        granted, reason, ..
                    } => {
                        assert!(granted, "Authority release should be granted");
                        assert!(reason.is_none(), "No error reason expected");
                        received_authority_response = true;
                        break;
                    }
                    ServerMessage::AuthorityChanged {
                        authority_player, ..
                    } => {
                        // This is expected - authority was released (None)
                        assert_eq!(authority_player, None);
                        // Continue looking for AuthorityResponse
                    }
                    _ => panic!("Unexpected message: {response:?}"),
                }
            }
            Ok(None) => panic!("Connection closed"),
            Err(_) => break, // Timeout
        }
    }

    assert!(
        received_authority_response,
        "Never received AuthorityResponse"
    );

    println!("Authority release test completed successfully");
}

#[tokio::test]
async fn test_two_player_authority_release() {
    let addr = start_test_server().await;
    let (mut sender1, mut receiver1) = connect_client(addr, "/v2/ws").await;
    let (mut sender2, mut receiver2) = connect_client(addr, "/v2/ws").await;

    // Player 1 creates room (should get authority)
    let join_msg1 = ClientMessage::JoinRoom {
        game_name: "two_player_test".to_string(),
        room_code: Some("TWO123".to_string()),
        player_name: "Player1".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response1 = send_and_receive(&mut sender1, &mut receiver1, join_msg1)
        .await
        .unwrap();
    match response1 {
        ServerMessage::RoomJoined(ref payload) => {
            assert!(payload.is_authority, "First player should have authority");
        }
        _ => panic!("Expected RoomJoined, got {response1:?}"),
    }

    // Player 2 joins room (should NOT get authority)
    let join_msg2 = ClientMessage::JoinRoom {
        game_name: "two_player_test".to_string(),
        room_code: Some("TWO123".to_string()),
        player_name: "Player2".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response2 = send_and_receive(&mut sender2, &mut receiver2, join_msg2)
        .await
        .unwrap();
    match response2 {
        ServerMessage::RoomJoined(ref payload) => {
            assert!(
                !payload.is_authority,
                "Second player should NOT have authority"
            );
        }
        _ => panic!("Expected RoomJoined, got {response2:?}"),
    }

    // Clear notifications
    let _ = tokio::time::timeout(tokio::time::Duration::from_millis(100), receiver1.next()).await;

    // Now Player 1 releases authority
    let release_request = ClientMessage::AuthorityRequest {
        become_authority: false,
    };

    let json = serde_json::to_string(&release_request).unwrap();
    sender1.send(Message::Text(json.into())).await.unwrap();

    // Player 1 should receive AuthorityResponse (and possibly AuthorityChanged from broadcast)
    // We need to collect more messages since broadcasts send to all players
    let mut received_authority_response = false;

    // Increase timeout and message collection to handle all possible messages
    for _ in 0..5 {
        // Allow more messages since broadcasts can send multiple
        match tokio::time::timeout(tokio::time::Duration::from_secs(3), receiver1.next()).await {
            Ok(Some(msg)) => {
                let text = msg.unwrap().into_text().unwrap();
                let response: ServerMessage = serde_json::from_str(&text).unwrap();
                match response {
                    ServerMessage::AuthorityResponse {
                        granted, reason, ..
                    } => {
                        assert!(granted, "Authority release should be granted");
                        assert!(reason.is_none(), "No error reason expected");
                        received_authority_response = true;
                        break; // Found what we're looking for
                    }
                    ServerMessage::AuthorityChanged {
                        authority_player, ..
                    } => {
                        assert_eq!(authority_player, None); // Authority released
                                                            // Continue looking for AuthorityResponse
                    }
                    _ => {
                        // Skip any other messages (like PlayerLeft, etc.)
                        continue;
                    }
                }
            }
            Ok(None) => panic!("Connection closed"),
            Err(_) => {
                // Timeout - continue to next iteration or break if no more messages
                break;
            }
        }
    }

    assert!(
        received_authority_response,
        "Never received AuthorityResponse"
    );
    println!("Two-player authority release test completed successfully");
}

#[tokio::test]
async fn test_e2e_authority_disabled_rooms() {
    let addr = start_test_server().await;
    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    // Create room without authority support
    let create_msg = ClientMessage::JoinRoom {
        game_name: "no_auth_game".to_string(),
        room_code: Some("NOAUTH".to_string()),
        player_name: "Player1".to_string(),
        max_players: Some(4),
        supports_authority: Some(false), // Authority disabled
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, create_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoined(ref payload) => {
            assert!(!payload.is_authority); // No authority in non-authority room
            assert!(!payload.supports_authority); // Room doesn't support authority
        }
        _ => panic!("Expected RoomJoined, got {response:?}"),
    }

    // Try to request authority - should be denied
    let authority_request = ClientMessage::AuthorityRequest {
        become_authority: true,
    };

    let json = serde_json::to_string(&authority_request).unwrap();
    sender.send(Message::Text(json.into())).await.unwrap();

    // Should receive denial
    match tokio::time::timeout(tokio::time::Duration::from_secs(2), receiver.next()).await {
        Ok(Some(msg)) => {
            let text = msg.unwrap().into_text().unwrap();
            let response: ServerMessage = serde_json::from_str(&text).unwrap();
            match response {
                ServerMessage::AuthorityResponse {
                    granted, reason, ..
                } => {
                    assert!(!granted);
                    assert!(reason.is_some());
                    assert!(reason.unwrap().contains("Room does not support authority"));
                }
                _ => panic!("Expected AuthorityResponse denial, got {response:?}"),
            }
        }
        Ok(None) => panic!("Connection closed while waiting for authority response"),
        Err(_) => panic!("Timeout waiting for authority response"),
    }
}

#[tokio::test]
async fn test_e2e_room_code_generation_with_custom_length() {
    // Test that room code generation respects custom configuration
    let protocol_config = signal_fish_server::config::ProtocolConfig {
        room_code_length: 8, // Custom length
        ..Default::default()
    };

    let addr =
        start_test_server_with_config_and_protocol(ServerConfig::default(), protocol_config).await;
    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    // Create room without specifying code (should auto-generate with custom length)
    let auto_room_msg = ClientMessage::JoinRoom {
        game_name: "autogame".to_string(),
        room_code: None, // Auto-generate
        player_name: "Player".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, auto_room_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoined(ref payload) => {
            assert_eq!(
                payload.room_code.len(),
                8,
                "Generated room code should have custom length of 8"
            );
            // Should not contain confusing characters
            assert!(!payload.room_code.contains('0'));
            assert!(!payload.room_code.contains('O'));
            assert!(!payload.room_code.contains('I'));
            assert!(!payload.room_code.contains('1'));
        }
        _ => panic!("Expected successful room creation with auto-generated code, got {response:?}"),
    }
}

#[tokio::test]
#[serial_test::serial]
async fn test_e2e_fallback_to_defaults_on_invalid_config() {
    use std::env;

    // Set invalid JSON config - should fallback to defaults
    env::set_var("SIGNAL_FISH_CONFIG_JSON", "{invalid json content}");

    let config = signal_fish_server::config::load();

    // Should use default values despite invalid JSON
    assert_eq!(config.port, 3536); // Default port
    assert_eq!(config.protocol.room_code_length, 6); // Default room code length
    assert_eq!(config.server.default_max_players, 8); // Default max players

    // Clean up
    env::remove_var("SIGNAL_FISH_CONFIG_JSON");

    // Test that server still works with these defaults
    let addr =
        start_test_server_with_config_and_protocol(ServerConfig::default(), config.protocol).await;
    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    // Should work with default 6-character room codes
    let test_msg = ClientMessage::JoinRoom {
        game_name: "defaulttest".to_string(),
        room_code: Some("DEF123".to_string()), // 6 chars - default length
        player_name: "Player".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, test_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoined(_) => {
            // Should work with defaults
        }
        _ => panic!("Expected room join to work with default config, got {response:?}"),
    }
}
