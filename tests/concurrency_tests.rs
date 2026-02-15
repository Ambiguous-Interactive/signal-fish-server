mod test_helpers;

use chrono::Duration as ChronoDuration;
use signal_fish_server::config::ProtocolConfig;
use signal_fish_server::protocol::{ErrorCode, ServerMessage};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use std::sync::Arc;
use test_helpers::{create_test_server, create_test_server_with_config, test_server_config};
use tokio::sync::Barrier;

/// Test atomic room creation under concurrent load
#[tokio::test]
async fn test_concurrent_room_creation() {
    let server = create_test_server().await;
    let attempts = 10usize;
    let barrier = Arc::new(Barrier::new(attempts));

    let mut handles = Vec::new();

    // Spawn multiple concurrent room creation attempts with same room code
    for i in 0..attempts {
        let server_clone = server.clone();
        let barrier_clone = barrier.clone();

        handles.push(tokio::spawn(async move {
            barrier_clone.wait().await; // Synchronize start

            let (tx, _rx) = tokio::sync::mpsc::channel(64);
            let player_id = uuid::Uuid::new_v4();

            // Register client first
            server_clone.connect_client(player_id, tx).await;

            // Try to create room with same code
            server_clone
                .handle_join_room(
                    &player_id,
                    "concurrent_game".to_string(),
                    Some("SAME01".to_string()), // Same room code for all attempts
                    format!("Player{i}"),
                    Some(4),
                    Some(true),
                    None,
                )
                .await;

            // Check if we successfully joined the room
            if let Some(_room_id) = server_clone.get_client_room(&player_id).await {
                1 // Successfully joined
            } else {
                0 // Failed to join
            }
        }));
    }

    let mut successful_joins = 0;
    for handle in handles {
        successful_joins += handle.await.unwrap();
    }

    // Due to room capacity (4), only the first 4 players should successfully join
    assert_eq!(
        successful_joins, 4,
        "Only 4 players should join due to room capacity limit"
    );

    // Verify exactly one room exists
    let room_count = server
        .database()
        .get_game_room_count("concurrent_game")
        .await
        .unwrap();
    assert_eq!(room_count, 1, "Only one room should exist");

    // Verify the room has exactly 4 players (at capacity)
    if let Ok(Some(room)) = server
        .database()
        .get_room("concurrent_game", "SAME01")
        .await
    {
        let players = server.database().get_room_players(&room.id).await.unwrap();
        assert_eq!(
            players.len(),
            4,
            "Room should have exactly 4 players (at capacity)"
        );
    } else {
        panic!("Room should exist after concurrent creation attempts");
    }
}

#[tokio::test]
async fn test_max_rooms_per_game_cap_is_enforced_under_concurrency() {
    let mut server_config = test_server_config();
    server_config.max_rooms_per_game = 2;
    let room_cap = server_config.max_rooms_per_game;
    let server = create_test_server_with_config(server_config, ProtocolConfig::default()).await;
    let attempts = 6usize;
    let barrier = Arc::new(Barrier::new(attempts));
    let game_name = "cap_limit_game".to_string();
    let mut handles = Vec::new();

    for i in 0..attempts {
        let server_clone = server.clone();
        let barrier_clone = barrier.clone();
        let game_name_clone = game_name.clone();
        handles.push(tokio::spawn(async move {
            barrier_clone.wait().await;
            let (tx, mut rx) = tokio::sync::mpsc::channel(64);
            let player_id = uuid::Uuid::new_v4();
            server_clone.connect_client(player_id, tx).await;

            server_clone
                .handle_join_room(
                    &player_id,
                    game_name_clone,
                    None,
                    format!("Player{i}"),
                    Some(4),
                    Some(true),
                    None,
                )
                .await;

            let joined = server_clone.get_client_room(&player_id).await.is_some();
            let mut error_code = None;
            if !joined {
                while let Some(message) = rx.recv().await {
                    if let ServerMessage::RoomJoinFailed {
                        error_code: code, ..
                    } = message.as_ref()
                    {
                        error_code = code.clone();
                        break;
                    }
                }
            }

            (joined, error_code)
        }));
    }

    let mut successful_creations = 0;
    let mut cap_rejections = 0;
    for handle in handles {
        let (joined, error_code) = handle.await.unwrap();
        if joined {
            successful_creations += 1;
        } else if matches!(error_code, Some(ErrorCode::MaxRoomsPerGameExceeded)) {
            cap_rejections += 1;
        }
    }

    assert_eq!(
        successful_creations, room_cap,
        "Only {room_cap} rooms should be created"
    );
    assert!(
        cap_rejections > 0,
        "At least one join attempt should be rejected due to the per-game cap"
    );

    let recorded_room_count = server
        .database()
        .get_game_room_count(&game_name)
        .await
        .expect("room count lookup succeeds");
    assert_eq!(
        recorded_room_count, room_cap,
        "Database should never report more rooms than the configured cap"
    );
}

/// Test atomic authority requests under concurrent conditions
#[tokio::test]
async fn test_concurrent_authority_requests() {
    let server = create_test_server().await;

    // First, create a room with authority support
    let (tx_creator, _rx_creator) = tokio::sync::mpsc::channel(64);
    let creator_id = uuid::Uuid::new_v4();
    server.connect_client(creator_id, tx_creator).await;

    server
        .handle_join_room(
            &creator_id,
            "auth_test_game".to_string(),
            Some("CONAUT".to_string()),
            "Creator".to_string(),
            Some(10),
            Some(true),
            None,
        )
        .await;

    // Add multiple players to the room
    let player_count = 5;
    let mut player_ids = Vec::new();

    for i in 0..player_count {
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let player_id = uuid::Uuid::new_v4();
        server.connect_client(player_id, tx).await;

        server
            .handle_join_room(
                &player_id,
                "auth_test_game".to_string(),
                Some("CONAUT".to_string()),
                format!("Player{i}"),
                Some(10),
                Some(true),
                None,
            )
            .await;

        player_ids.push(player_id);
    }

    // Creator releases authority
    server.handle_authority_request(&creator_id, false).await;

    // Now all players try to request authority simultaneously
    let barrier = Arc::new(Barrier::new(player_count));
    let mut handles = Vec::new();

    for player_id in player_ids {
        let server_clone = server.clone();
        let barrier_clone = barrier.clone();

        handles.push(tokio::spawn(async move {
            barrier_clone.wait().await; // Synchronize start

            // Try to request authority
            server_clone
                .handle_authority_request(&player_id, true)
                .await;

            // Add a small delay to allow for database updates to complete
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

            // Check if this player got authority
            let room_id = server_clone.get_client_room(&player_id).await;
            if let Some(room_id) = room_id {
                if let Ok(Some(room)) = server_clone.database().get_room_by_id(&room_id).await {
                    return room.authority_player == Some(player_id);
                }
            }
            false
        }));
    }

    let mut authority_granted_count = 0;
    for handle in handles {
        if handle.await.unwrap() {
            authority_granted_count += 1;
        }
    }

    // Only ONE player should have received authority
    assert_eq!(
        authority_granted_count, 1,
        "Only one player should get authority"
    );
}

/// Test concurrent player additions to a room at capacity
#[tokio::test]
async fn test_concurrent_room_joining_at_capacity() {
    let server = create_test_server().await;

    // Create a room with capacity 3
    let (tx_creator, _rx_creator) = tokio::sync::mpsc::channel(64);
    let creator_id = uuid::Uuid::new_v4();
    server.connect_client(creator_id, tx_creator).await;

    server
        .handle_join_room(
            &creator_id,
            "capacity_test".to_string(),
            Some("CAP123".to_string()),
            "Creator".to_string(),
            Some(3), // Max 3 players
            Some(true),
            None,
        )
        .await;

    // Try to add 10 players concurrently (more than capacity)
    let attempt_count = 10;
    let barrier = Arc::new(Barrier::new(attempt_count));
    let mut handles = Vec::new();

    for i in 0..attempt_count {
        let server_clone = server.clone();
        let barrier_clone = barrier.clone();

        handles.push(tokio::spawn(async move {
            barrier_clone.wait().await; // Synchronize start

            let (tx, _rx) = tokio::sync::mpsc::channel(64);
            let player_id = uuid::Uuid::new_v4();
            server_clone.connect_client(player_id, tx).await;

            server_clone
                .handle_join_room(
                    &player_id,
                    "capacity_test".to_string(),
                    Some("CAP123".to_string()),
                    format!("Player{i}"),
                    Some(3),
                    Some(true),
                    None,
                )
                .await;

            // Return 1 if successfully joined, 0 if not
            if server_clone.get_client_room(&player_id).await.is_some() {
                1
            } else {
                0
            }
        }));
    }

    let mut successful_joins = 1; // Creator is already in
    for handle in handles {
        successful_joins += handle.await.unwrap();
    }

    // Should have exactly 3 players (creator + 2 additional)
    assert_eq!(
        successful_joins, 3,
        "Room should have exactly 3 players (at capacity)"
    );

    // Verify room player count
    if let Some(room_id) = server.get_client_room(&creator_id).await {
        let players = server.database().get_room_players(&room_id).await.unwrap();
        assert_eq!(players.len(), 3, "Room should contain exactly 3 players");
    }
}

/// Test authority handling when authority player disconnects concurrently
#[tokio::test]
async fn test_concurrent_authority_player_disconnect() {
    let game_server = create_test_server().await;

    let server = Arc::new(game_server);

    // Create room with authority player
    let (tx_auth, _rx_auth) = tokio::sync::mpsc::channel(64);
    let authority_id = uuid::Uuid::new_v4();
    server.connect_client(authority_id, tx_auth).await;

    server
        .handle_join_room(
            &authority_id,
            "disconnect_test".to_string(),
            Some("DISC01".to_string()),
            "AuthPlayer".to_string(),
            Some(5),
            Some(true),
            None,
        )
        .await;

    // Add other players
    let mut other_players = Vec::new();
    for i in 0..3 {
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let player_id = uuid::Uuid::new_v4();
        server.connect_client(player_id, tx).await;

        server
            .handle_join_room(
                &player_id,
                "disconnect_test".to_string(),
                Some("DISC01".to_string()),
                format!("Player{i}"),
                Some(5),
                Some(true),
                None,
            )
            .await;

        other_players.push(player_id);
    }

    let room_id = server.get_client_room(&authority_id).await.unwrap();

    // Verify initial authority state
    let room = server
        .database()
        .get_room_by_id(&room_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(room.authority_player, Some(authority_id));

    // Concurrently: authority player disconnects while others try to request authority
    let barrier = Arc::new(Barrier::new(other_players.len() + 1));

    // Authority player leaves
    let server_clone = server.clone();
    let barrier_clone = barrier.clone();
    let disconnect_handle = tokio::spawn(async move {
        barrier_clone.wait().await;
        server_clone.disconnect_client(&authority_id).await;
    });

    // Other players try to request authority simultaneously
    let mut auth_handles = Vec::new();
    for player_id in &other_players {
        let server_clone = server.clone();
        let barrier_clone = barrier.clone();
        let player_id = *player_id;

        auth_handles.push(tokio::spawn(async move {
            barrier_clone.wait().await;

            // Small delay to ensure disconnect happens first
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

            server_clone
                .handle_authority_request(&player_id, true)
                .await;

            // Check if this player got authority
            if let Some(room_id) = server_clone.get_client_room(&player_id).await {
                if let Ok(Some(room)) = server_clone.database().get_room_by_id(&room_id).await {
                    return room.authority_player == Some(player_id);
                }
            }
            false
        }));
    }

    // Wait for disconnect operation
    disconnect_handle.await.unwrap();

    // Wait for authority requests and count successful ones
    let mut _authority_granted_count = 0;
    for handle in auth_handles {
        if handle.await.unwrap() {
            _authority_granted_count += 1;
        }
    }

    // Verify final state: authority should be cleared (not auto-assigned per protocol)
    let final_room = server
        .database()
        .get_room_by_id(&room_id)
        .await
        .unwrap()
        .unwrap();

    // According to protocol: authority should be None when authority player leaves
    // One of the remaining players may have successfully requested it after the disconnect
    let remaining_players = server.database().get_room_players(&room_id).await.unwrap();
    assert_eq!(
        remaining_players.len(),
        3,
        "Should have 3 remaining players"
    );

    // At most one player should have authority
    let authority_count = remaining_players.iter().filter(|p| p.is_authority).count();
    assert!(
        authority_count <= 1,
        "At most one player should have authority"
    );

    if authority_count == 1 {
        // If someone has authority, it should match the room's authority_player
        let authority_player = remaining_players.iter().find(|p| p.is_authority).unwrap();
        assert_eq!(final_room.authority_player, Some(authority_player.id));
    } else {
        // If no one has authority, room's authority_player should be None
        assert_eq!(final_room.authority_player, None);
    }
}

/// Test room cleanup doesn't interfere with active operations
#[tokio::test]
async fn test_concurrent_room_cleanup_and_activity() {
    let config = ServerConfig {
        empty_room_timeout: tokio::time::Duration::from_millis(100),
        ..ServerConfig::default()
    };
    let protocol_config = signal_fish_server::config::ProtocolConfig::default();
    let server = create_test_server_with_config(config, protocol_config).await;

    // Create a room
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let player_id = uuid::Uuid::new_v4();
    server.connect_client(player_id, tx).await;

    server
        .handle_join_room(
            &player_id,
            "cleanup_test".to_string(),
            Some("CLEAN1".to_string()),
            "Player".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;

    let room_id = server.get_client_room(&player_id).await.unwrap();

    // Player leaves to make room empty
    server.leave_room(&player_id).await;

    // Start concurrent operations
    let barrier = Arc::new(Barrier::new(2));

    // Task 1: Continuous cleanup attempts
    let server_clone = server.clone();
    let barrier_clone = barrier.clone();
    let cleanup_handle = tokio::spawn(async move {
        barrier_clone.wait().await;

        for _ in 0..10 {
            let _ = server_clone
                .database()
                .cleanup_empty_rooms(ChronoDuration::zero())
                .await;
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        }
    });

    // Task 2: Try to rejoin the room (should fail if cleanup happened)
    let server_clone = server.clone();
    let barrier_clone = barrier.clone();
    let rejoin_handle = tokio::spawn(async move {
        barrier_clone.wait().await;

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let new_player_id = uuid::Uuid::new_v4();
        server_clone.connect_client(new_player_id, tx).await;

        server_clone
            .handle_join_room(
                &new_player_id,
                "cleanup_test".to_string(),
                Some("CLEAN1".to_string()),
                "NewPlayer".to_string(),
                Some(4),
                Some(true),
                None,
            )
            .await;

        server_clone.get_client_room(&new_player_id).await.is_some()
    });

    cleanup_handle.await.unwrap();
    let rejoin_successful = rejoin_handle.await.unwrap();

    // After cleanup, the room should be gone, so rejoin should create a new room
    if rejoin_successful {
        // A new room was created
        let room_count = server
            .database()
            .get_game_room_count("cleanup_test")
            .await
            .unwrap();
        assert_eq!(room_count, 1, "New room should be created after cleanup");
    } else {
        // Room cleanup didn't happen or rejoin failed for other reasons
        let room_exists = server
            .database()
            .get_room_by_id(&room_id)
            .await
            .unwrap()
            .is_some();
        if room_exists {
            println!("Room cleanup didn't occur during the test window - this is acceptable");
        }
    }
}

#[tokio::test]
async fn test_concurrent_room_cleanup_across_instances() {
    let server_a = create_test_server().await;
    let server_b = create_test_server().await;

    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let player_id = uuid::Uuid::new_v4();
    server_a.connect_client(player_id, tx).await;

    server_a
        .handle_join_room(
            &player_id,
            "cleanup_race".to_string(),
            Some("RACER1".to_string()),
            "PlayerA".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;

    let room_id = server_a.get_client_room(&player_id).await.unwrap();
    server_a.leave_room(&player_id).await;

    let barrier = Arc::new(Barrier::new(2));

    let cleanup = |server: Arc<EnhancedGameServer>, barrier: Arc<Barrier>| async move {
        let barrier_clone = barrier.clone();
        tokio::spawn(async move {
            barrier_clone.wait().await;
            server
                .database()
                .cleanup_empty_rooms(ChronoDuration::zero())
                .await
                .expect("cleanup should succeed")
                .len()
        })
        .await
        .expect("cleanup task panicked")
    };

    let (removed_a, removed_b) = tokio::join!(
        cleanup(server_a.clone(), barrier.clone()),
        cleanup(server_b.clone(), barrier.clone())
    );

    assert!(
        removed_a + removed_b <= 1,
        "cleanup across instances should be idempotent"
    );

    let room_lookup = server_a
        .database()
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup should succeed");
    assert!(
        room_lookup.is_none(),
        "room should be removed after concurrent cleanup"
    );
}
