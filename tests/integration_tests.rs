mod test_helpers;

use signal_fish_server::protocol::*;
use signal_fish_server::server::ServerConfig;
use test_helpers::{create_test_server, create_test_server_with_config};
use tokio::sync::mpsc;

/// Test creating rooms and joining them with multiple players
#[tokio::test]
async fn test_multi_player_room_scenario() {
    let server = create_test_server().await;

    // Create channels first
    let (tx1, mut rx1) = mpsc::channel(64);
    let (tx2, mut rx2) = mpsc::channel(64);

    // Register clients properly
    let player1_id = server
        .register_client(tx1, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let player2_id = server
        .register_client(tx2, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();

    // Player 1 creates a room
    server
        .handle_join_room(
            &player1_id,
            "multiplayer_game".to_string(),
            Some("MLT123".to_string()),
            "Player1".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;

    // Player 1 should receive a RoomJoined message
    let msg1 = rx1.try_recv().unwrap();
    match msg1.as_ref() {
        ServerMessage::RoomJoined(ref payload) => {
            assert_eq!(payload.room_code, "MLT123");
            assert_eq!(payload.player_id, player1_id);
            assert_eq!(payload.game_name, "multiplayer_game");
            assert_eq!(payload.max_players, 4);
            assert_eq!(payload.current_players.len(), 1);
            assert!(payload.is_authority);
        }
        _ => panic!("Expected RoomJoined message for player 1"),
    }

    // Player 2 joins the same room
    server
        .handle_join_room(
            &player2_id,
            "multiplayer_game".to_string(),
            Some("MLT123".to_string()),
            "Player2".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;

    // Player 2 should receive a RoomJoined message
    let msg2 = rx2.try_recv().unwrap();
    match msg2.as_ref() {
        ServerMessage::RoomJoined(ref payload) => {
            assert_eq!(payload.room_code, "MLT123");
            assert_eq!(payload.player_id, player2_id);
            assert_eq!(payload.game_name, "multiplayer_game");
            assert_eq!(payload.current_players.len(), 2);
            assert!(!payload.is_authority); // Player 2 is not authority
        }
        _ => panic!("Expected RoomJoined message for player 2"),
    }

    // Player 1 should receive a PlayerJoined notification
    let notification = rx1.try_recv().unwrap();
    match notification.as_ref() {
        ServerMessage::PlayerJoined { player } => {
            assert_eq!(player.id, player2_id);
            assert_eq!(player.name, "Player2");
            assert!(!player.is_authority);
        }
        _ => panic!("Expected PlayerJoined notification for player 1"),
    }
}

/// Test game data exchange between players
#[tokio::test]
async fn test_game_data_exchange() {
    println!("Starting game data exchange test");

    let server = create_test_server().await;

    let (tx1, mut rx1) = mpsc::channel(64);
    let (tx2, mut rx2) = mpsc::channel(64);

    let player1_id = server
        .register_client(tx1, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let player2_id = server
        .register_client(tx2, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    println!("Registered players: {player1_id}, {player2_id}");

    // Both players join the same room
    server
        .handle_join_room(
            &player1_id,
            "data_game".to_string(),
            Some("DTA123".to_string()),
            "DataPlayer1".to_string(),
            Some(2),
            Some(true),
            None,
        )
        .await;

    // Add a short delay between joining players to ensure stable test
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    server
        .handle_join_room(
            &player2_id,
            "data_game".to_string(),
            Some("DTA123".to_string()),
            "DataPlayer2".to_string(),
            Some(2),
            Some(true),
            None,
        )
        .await;

    // Add delay after joining to ensure room state is stable
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    println!("Players joined room");

    // Clear joining messages
    println!("Clearing messages for player 1");
    while let Ok(msg) = rx1.try_recv() {
        println!("Player 1 received: {msg:?}");
    }

    println!("Clearing messages for player 2");
    while let Ok(msg) = rx2.try_recv() {
        println!("Player 2 received: {msg:?}");
    }

    // Player 1 sends game data
    println!("Player 1 sending game data");
    let test_data = serde_json::json!({"action": "move", "x": 10, "y": 20});
    server
        .handle_client_message(
            &player1_id,
            ClientMessage::GameData {
                data: test_data.clone(),
            },
        )
        .await;

    // Add a short delay to ensure message propagation
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Player 2 should receive the game data
    println!("Checking if player 2 received game data");
    match rx2.try_recv() {
        Ok(msg) => {
            println!("Player 2 received message: {msg:?}");
            match msg.as_ref() {
                ServerMessage::GameData { from_player, data } => {
                    assert_eq!(*from_player, player1_id);
                    assert_eq!(data, &test_data);
                    println!("Received correct game data message!");
                }
                _ => panic!("Expected GameData message, got: {msg:?}"),
            }
        }
        Err(e) => {
            println!("No message received: {e:?}");
            panic!("Expected GameData message");
        }
    }
}

/// Test room capacity limits
#[tokio::test]
async fn test_room_capacity() {
    let server = create_test_server().await;

    // Create a room with max 2 players
    let (tx1, _rx1) = mpsc::channel(64);
    let (tx2, _rx2) = mpsc::channel(64);
    let (tx3, mut rx3) = mpsc::channel(64);

    let player1_id = server
        .register_client(tx1, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let player2_id = server
        .register_client(tx2, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let player3_id = server
        .register_client(tx3, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();

    // First two players join successfully
    server
        .handle_join_room(
            &player1_id,
            "limited_game".to_string(),
            Some("LMT123".to_string()),
            "Player1".to_string(),
            Some(2),
            Some(true),
            None,
        )
        .await;
    server
        .handle_join_room(
            &player2_id,
            "limited_game".to_string(),
            Some("LMT123".to_string()),
            "Player2".to_string(),
            Some(2),
            Some(true),
            None,
        )
        .await;

    // Third player should be rejected
    server
        .handle_join_room(
            &player3_id,
            "limited_game".to_string(),
            Some("LMT123".to_string()),
            "Player3".to_string(),
            Some(2),
            Some(true),
            None,
        )
        .await;

    // Player 3 should receive RoomJoinFailed
    let rejection = rx3.try_recv().unwrap();
    match rejection.as_ref() {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("full") || reason.contains("capacity"));
        }
        _ => panic!("Expected RoomJoinFailed message"),
    }
}

/// Test authority management
#[tokio::test]
async fn test_authority_transfer() {
    println!("Starting authority transfer test");

    let server = create_test_server().await;

    let (tx1, mut rx1) = mpsc::channel(64);
    let (tx2, mut rx2) = mpsc::channel(64);

    let player1_id = server
        .register_client(tx1, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let player2_id = server
        .register_client(tx2, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    println!("Registered players: {player1_id}, {player2_id}");

    // Players join room (player 1 becomes authority)
    server
        .handle_join_room(
            &player1_id,
            "auth_game".to_string(),
            Some("ATH123".to_string()),
            "AuthPlayer1".to_string(),
            Some(2),
            Some(true),
            None,
        )
        .await;

    // Add a delay between joins to ensure stability
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    server
        .handle_join_room(
            &player2_id,
            "auth_game".to_string(),
            Some("ATH123".to_string()),
            "AuthPlayer2".to_string(),
            Some(2),
            Some(true),
            None,
        )
        .await;

    // Add delay after joining
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    println!("Players joined room");

    // Clear join messages
    println!("Clearing messages for player 1");
    while let Ok(msg) = rx1.try_recv() {
        println!("Player 1 received: {msg:?}");
    }

    println!("Clearing messages for player 2");
    while let Ok(msg) = rx2.try_recv() {
        println!("Player 2 received: {msg:?}");
    }

    // Player 1 first releases authority
    println!("Player 1 releasing authority");
    server
        .handle_client_message(
            &player1_id,
            ClientMessage::AuthorityRequest {
                become_authority: false,
            },
        )
        .await;

    // Add delay to ensure authority release is processed
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Clear authority release notifications
    println!("Clearing authority release messages for player 1");
    while let Ok(msg) = rx1.try_recv() {
        println!("Player 1 received after release: {msg:?}");
    }

    println!("Clearing authority release messages for player 2");
    while let Ok(msg) = rx2.try_recv() {
        println!("Player 2 received after release: {msg:?}");
    }

    // Now Player 2 requests authority (should succeed since no one has it)
    println!("Player 2 requesting authority");
    server
        .handle_client_message(
            &player2_id,
            ClientMessage::AuthorityRequest {
                become_authority: true,
            },
        )
        .await;

    // Add delay to ensure authority request is processed
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Player 2 should receive AuthorityResponse
    println!("Checking player 2 messages for AuthorityResponse");
    let response = match rx2.try_recv() {
        Ok(msg) => {
            println!("Player 2 received message: {msg:?}");
            msg
        }
        Err(e) => {
            println!("Error receiving message: {e:?}");
            panic!("Expected AuthorityResponse message for player 2");
        }
    };

    match response.as_ref() {
        ServerMessage::AuthorityResponse {
            granted, reason, ..
        } => {
            assert!(*granted, "Authority request should be granted");
            assert!(reason.is_none(), "No reason should be provided for success");
            println!("Player 2 received correct AuthorityResponse");
        }
        _ => panic!("Expected AuthorityResponse message for player 2, got: {response:?}"),
    }

    // Both players should receive authority change notifications
    println!("Checking for AuthorityChanged messages");

    // Player 1 should receive AuthorityChanged
    let auth1 = match rx1.try_recv() {
        Ok(msg) => {
            println!("Player 1 received message: {msg:?}");
            msg
        }
        Err(e) => {
            println!("Error receiving message for player 1: {e:?}");
            panic!("Expected AuthorityChanged message for player 1");
        }
    };

    match auth1.as_ref() {
        ServerMessage::AuthorityChanged {
            authority_player,
            you_are_authority,
        } => {
            assert_eq!(
                *authority_player,
                Some(player2_id),
                "Player 2 should be the new authority"
            );
            assert!(!*you_are_authority, "Player 1 should not be authority");
            println!("Player 1 received correct AuthorityChanged message");
        }
        _ => panic!("Expected AuthorityChanged message for player 1, got: {auth1:?}"),
    }

    // Player 2 should receive AuthorityChanged after AuthorityResponse
    let auth2 = match rx2.try_recv() {
        Ok(msg) => {
            println!("Player 2 received second message: {msg:?}");
            msg
        }
        Err(e) => {
            println!("Error receiving second message for player 2: {e:?}");
            panic!("Expected AuthorityChanged message for player 2");
        }
    };

    match auth2.as_ref() {
        ServerMessage::AuthorityChanged {
            authority_player,
            you_are_authority,
        } => {
            assert_eq!(
                *authority_player,
                Some(player2_id),
                "Player 2 should be the new authority"
            );
            assert!(*you_are_authority, "Player 2 should be authority");
            println!("Player 2 received correct AuthorityChanged message");
        }
        _ => panic!("Expected AuthorityChanged message for player 2, got: {auth2:?}"),
    }

    println!("Authority transfer test completed successfully");
}

/// Test player disconnection and cleanup
#[tokio::test]
async fn test_player_disconnection() {
    let server = create_test_server().await;

    let (tx1, _rx1) = mpsc::channel(64);
    let (tx2, mut rx2) = mpsc::channel(64);

    let player1_id = server
        .register_client(tx1, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let player2_id = server
        .register_client(tx2, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();

    // Both players join room
    server
        .handle_join_room(
            &player1_id,
            "disconnect_game".to_string(),
            Some("DSC123".to_string()),
            "Player1".to_string(),
            Some(2),
            Some(true),
            None,
        )
        .await;
    server
        .handle_join_room(
            &player2_id,
            "disconnect_game".to_string(),
            Some("DSC123".to_string()),
            "Player2".to_string(),
            Some(2),
            Some(true),
            None,
        )
        .await;

    // Clear join messages
    let _ = rx2.try_recv();

    // Player 1 disconnects
    println!("Player 1 disconnecting");
    server.unregister_client(&player1_id).await;

    // Add a delay to ensure messages are processed
    println!("Waiting for PlayerLeft notification");
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Try to receive messages and find PlayerLeft notification
    println!("Checking for notifications after player1 disconnect");
    let mut found_player_left = false;

    // Try to get messages for up to 3 attempts
    for i in 1..=3 {
        println!("Attempt #{i} to get messages");
        match rx2.try_recv() {
            Ok(msg) => {
                println!("Received message: {msg:?}");

                // Check if it's the PlayerLeft notification
                if let ServerMessage::PlayerLeft { player_id } = *msg {
                    println!("Found PlayerLeft notification");
                    assert_eq!(player_id, player1_id);
                    found_player_left = true;
                    break;
                } else {
                    println!("Skipping other message type, continuing to look for PlayerLeft");
                    // Continue to next message if it's not PlayerLeft
                }
            }
            Err(e) => {
                println!("No more messages: {e:?}");
                break;
            }
        }
    }

    // If we didn't find it in the first batch, add a delay and try again
    if !found_player_left {
        println!("PlayerLeft not found in first batch, waiting longer and trying again");
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

        // One more attempt
        match rx2.try_recv() {
            Ok(msg) => {
                println!("Received additional message: {msg:?}");
                if let ServerMessage::PlayerLeft { player_id } = *msg {
                    assert_eq!(player_id, player1_id);
                    found_player_left = true;
                }
            }
            Err(e) => {
                println!("No additional messages: {e:?}");
            }
        }
    }

    // Assert that we found the PlayerLeft notification
    assert!(found_player_left, "Never received PlayerLeft notification");
}

/// Test ping/pong functionality
#[tokio::test]
async fn test_ping_pong() {
    let server = create_test_server().await;

    let (tx, mut rx) = mpsc::channel(64);
    let player_id = server
        .register_client(tx, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();

    // Send ping
    server
        .handle_client_message(&player_id, ClientMessage::Ping)
        .await;

    // Should receive pong
    let pong = rx.try_recv().unwrap();
    match pong.as_ref() {
        ServerMessage::Pong => {
            // Success
        }
        _ => panic!("Expected Pong message"),
    }
}

/// Test validation errors
#[tokio::test]
async fn test_validation_errors() {
    let server = create_test_server().await;

    let (tx, mut rx) = mpsc::channel(64);
    let player_id = server
        .register_client(tx, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();

    // Test invalid room code (contains special characters)
    server
        .handle_join_room(
            &player_id,
            "valid_game".to_string(),
            Some("INVALID!@#".to_string()),
            "Player".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;

    let error = rx.try_recv().unwrap();
    match error.as_ref() {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("Room code") || reason.contains("alphanumeric"));
        }
        _ => panic!("Expected RoomJoinFailed for invalid room code"),
    }
}

/// Test config fallback scenarios and custom configurations
#[tokio::test]
#[serial_test::serial]
async fn test_config_fallback_integration() {
    use std::env;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    // Test 1: Environment variables should override file config
    let dir = tempdir().unwrap();
    let config_file = dir.path().join("test_config.json");

    let config_json = r#"{
        "port": 7777,
        "server": {
            "default_max_players": 4
        }
    }"#;

    let mut file = File::create(&config_file).unwrap();
    file.write_all(config_json.as_bytes()).unwrap();

    // Set file path and env override
    env::set_var("SIGNAL_FISH_CONFIG_PATH", config_file.to_str().unwrap());
    env::set_var("SIGNAL_FISH__SERVER__DEFAULT_MAX_PLAYERS", "12");

    let config = signal_fish_server::config::load();

    // File value should be used for port
    assert_eq!(config.port, 7777);
    // Environment override should take precedence for default_max_players
    assert_eq!(config.server.default_max_players, 12);
    // Other values should be defaults
    assert_eq!(config.server.ping_timeout, 30);

    // Clean up
    env::remove_var("SIGNAL_FISH_CONFIG_PATH");
    env::remove_var("SIGNAL_FISH__SERVER__DEFAULT_MAX_PLAYERS");
}

/// Test server behavior with custom protocol configuration
#[tokio::test]
async fn test_custom_protocol_config() {
    let server_config = ServerConfig::default();
    let protocol_config = signal_fish_server::config::ProtocolConfig {
        max_game_name_length: 32,   // Reduced from default 64
        room_code_length: 4,        // Reduced from default 6
        max_player_name_length: 16, // Reduced from default 32
        max_players_limit: 4,       // Reduced from default 100
        ..signal_fish_server::config::ProtocolConfig::default()
    };

    let server = create_test_server_with_config(server_config, protocol_config).await;
    let (tx, mut rx) = mpsc::channel(64);
    let player_id = server
        .register_client(tx, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();

    // Test 1: Game name longer than custom limit should fail
    let long_game_name = "a".repeat(40); // Longer than our custom limit of 32
    server
        .handle_join_room(
            &player_id,
            long_game_name,
            Some("ABC".to_string()),
            "Player".to_string(),
            Some(2),
            Some(true),
            None,
        )
        .await;

    let error = rx.try_recv().unwrap();
    match error.as_ref() {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("Game name too long") && reason.contains("32"));
        }
        _ => panic!("Expected game name validation error"),
    }

    // Test 2: Room code with wrong length should fail
    server
        .handle_join_room(
            &player_id,
            "testgame".to_string(),
            Some("ABCDEF".to_string()), // 6 chars but we configured 4
            "Player".to_string(),
            Some(2),
            Some(true),
            None,
        )
        .await;

    let error = rx.try_recv().unwrap();
    match error.as_ref() {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("Room code must be exactly 4 characters"));
        }
        _ => panic!("Expected room code length validation error"),
    }

    // Test 3: Player name longer than custom limit should fail
    let long_player_name = "b".repeat(20); // Longer than our custom limit of 16
    server
        .handle_join_room(
            &player_id,
            "testgame".to_string(),
            Some("ABCD".to_string()), // Correct 4-char code
            long_player_name,
            Some(2),
            Some(true),
            None,
        )
        .await;

    let error = rx.try_recv().unwrap();
    match error.as_ref() {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("Player name too long") && reason.contains("16"));
        }
        _ => panic!("Expected player name validation error"),
    }

    // Test 4: Max players exceeding custom limit should fail
    server
        .handle_join_room(
            &player_id,
            "testgame".to_string(),
            Some("ABCD".to_string()),
            "Player".to_string(),
            Some(8), // Exceeds our custom limit of 4
            Some(true),
            None,
        )
        .await;

    let error = rx.try_recv().unwrap();
    match error.as_ref() {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("Max players cannot exceed 4"));
        }
        _ => panic!("Expected max players validation error"),
    }

    // Test 5: Valid parameters within custom limits should succeed
    server
        .handle_join_room(
            &player_id,
            "game".to_string(),       // Short game name
            Some("TEST".to_string()), // 4-char code
            "Bob".to_string(),        // Short player name
            Some(3),                  // Within limit
            Some(true),
            None,
        )
        .await;

    let success = rx.try_recv().unwrap();
    match success.as_ref() {
        ServerMessage::RoomJoined(_) => {
            // Success!
        }
        _ => panic!("Expected successful room join with valid parameters"),
    }
}

/// Test server with custom rate limiting configuration
#[tokio::test]
async fn test_custom_rate_limiting() {
    use signal_fish_server::rate_limit::RateLimitConfig;
    use tokio::time::Duration;

    let server_config = ServerConfig {
        default_max_players: 8,
        ping_timeout: Duration::from_secs(30),
        room_cleanup_interval: Duration::from_secs(60),
        max_rooms_per_game: 1000,
        rate_limit_config: RateLimitConfig {
            max_room_creations: 1,               // Very restrictive for testing
            time_window: Duration::from_secs(5), // Longer window to ensure test stability
            max_join_attempts: 2,
        },
        empty_room_timeout: Duration::from_secs(300),
        inactive_room_timeout: Duration::from_secs(3600),
        max_message_size: 65536,
        max_connections_per_ip: 100,
        require_metrics_auth: false,
        metrics_auth_token: None,
        reconnection_window: Duration::from_secs(300), // 5 minutes
        event_buffer_size: 100,                        // Buffer 100 events
        enable_reconnection: true,                     // Enable reconnection
        websocket_config: signal_fish_server::config::WebSocketConfig::default(),
        auth_enabled: false,                // Disable auth for tests
        heartbeat_throttle: Duration::ZERO, // No throttling for tests
        region_id: "test".to_string(),
        room_code_prefix: None,
    };

    let server = create_test_server_with_config(
        server_config,
        signal_fish_server::config::ProtocolConfig::default(),
    )
    .await;

    let (tx, mut rx) = mpsc::channel(64);
    let player_id = server
        .register_client(tx, "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();

    // First room creation should succeed
    server
        .handle_join_room(
            &player_id,
            "game1".to_string(),
            None, // Auto-generate code (room creation)
            "Player".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;

    let success = rx.try_recv().unwrap();
    match success.as_ref() {
        ServerMessage::RoomJoined(_) => {
            // Expected
        }
        _ => panic!("Expected first room creation to succeed"),
    }

    // Leave the room to attempt another creation
    println!("Leaving the room");
    server.leave_room(&player_id).await;

    // Wait for and verify the RoomLeft message
    println!("Waiting for RoomLeft message");
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let leave_msg = rx.try_recv().unwrap();
    println!("Received message after leaving: {leave_msg:?}");
    match leave_msg.as_ref() {
        ServerMessage::RoomLeft => {
            println!("Successfully received RoomLeft message");
        }
        _ => panic!("Expected RoomLeft message, got: {leave_msg:?}"),
    }

    // Make sure rate limits don't expire by using a longer time window
    println!("Waiting before second room creation attempt");

    println!("Attempting second room creation which should fail due to rate limiting");
    // Second room creation should fail due to rate limiting
    server
        .handle_join_room(
            &player_id,
            "game2".to_string(),
            None, // Auto-generate code (room creation)
            "Player".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;

    // Add a longer delay to ensure the message is processed
    println!("Waiting for rate limit error message");
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    println!("Checking for rate limit error message");
    let rate_limit_error = match rx.try_recv() {
        Ok(msg) => {
            println!("Received message: {msg:?}");
            msg
        }
        Err(err) => {
            println!("Error receiving message: {err:?}");
            panic!("Failed to receive rate limit error message: {err:?}");
        }
    };
    match rate_limit_error.as_ref() {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("Room creation rate limit exceeded"));
        }
        _ => panic!("Expected rate limit error"),
    }
}
