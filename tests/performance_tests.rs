//! Performance tests for the signal-fish-server optimizations
//!
//! This module contains comprehensive tests to verify:
//! - Zero-copy broadcast message cloning
//! - SmallVec stack allocation for player lists
//! - Bytes-based binary payload handling
//! - Multi-client broadcast scenarios

use bytes::Bytes;
use smallvec::SmallVec;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

// Re-export types we're testing
type PlayerId = Uuid;

/// Typical room size constant matching production
const TYPICAL_ROOM_SIZE: usize = 8;

/// Type alias for player ID lists that stay on the stack for typical rooms
type PlayerIdList = SmallVec<[PlayerId; TYPICAL_ROOM_SIZE]>;

/// Mock ServerMessage for testing
#[derive(Debug, Clone)]
#[allow(dead_code)]
enum MockServerMessage {
    Pong,
    GameData {
        from_player: PlayerId,
        data: String,
    },
    GameDataBinary {
        from_player: PlayerId,
        payload: Bytes,
    },
    PlayerJoined {
        player_id: PlayerId,
        player_name: String,
    },
}

/// Arc-wrapped broadcast message for zero-cost cloning
#[derive(Debug, Clone)]
struct BroadcastMessage {
    inner: Arc<MockServerMessage>,
}

impl BroadcastMessage {
    fn new(message: MockServerMessage) -> Self {
        Self {
            inner: Arc::new(message),
        }
    }

    fn arc_clone(&self) -> Arc<MockServerMessage> {
        self.inner.clone()
    }
}

// ============================================================================
// SmallVec Allocation Tests
// ============================================================================

#[test]
fn test_player_id_list_stays_on_stack_for_typical_rooms() {
    // For rooms with up to 8 players, no heap allocation should occur
    for room_size in 1..=TYPICAL_ROOM_SIZE {
        let mut players: PlayerIdList = SmallVec::new();
        for _ in 0..room_size {
            players.push(Uuid::new_v4());
        }

        assert!(
            !players.spilled(),
            "PlayerIdList should stay on stack for {room_size} players"
        );
    }
}

#[test]
fn test_player_id_list_spills_to_heap_for_large_rooms() {
    let mut players: PlayerIdList = SmallVec::new();

    // Fill up to capacity
    for _ in 0..TYPICAL_ROOM_SIZE {
        players.push(Uuid::new_v4());
    }
    assert!(!players.spilled(), "Should stay on stack at capacity");

    // Add one more to trigger heap allocation
    players.push(Uuid::new_v4());
    assert!(
        players.spilled(),
        "Should spill to heap when exceeding capacity"
    );
}

#[test]
fn test_player_id_list_collect_efficiency() {
    // Simulate collecting players from a room membership set
    let player_ids: Vec<PlayerId> = (0..6).map(|_| Uuid::new_v4()).collect();

    // Using SmallVec collect - should stay on stack
    let collected: PlayerIdList = player_ids.iter().copied().collect();
    assert!(!collected.spilled(), "Collected list should stay on stack");
    assert_eq!(collected.len(), 6);
}

// ============================================================================
// Bytes Zero-Copy Tests
// ============================================================================

#[test]
fn test_bytes_zero_copy_clone() {
    let original_data = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
    let bytes = Bytes::from(original_data);

    // Clone should be cheap (just Arc increment)
    let clone1 = bytes.clone();
    let clone2 = bytes.clone();

    // All clones should point to the same underlying buffer
    assert_eq!(bytes.as_ptr(), clone1.as_ptr());
    assert_eq!(bytes.as_ptr(), clone2.as_ptr());
}

#[test]
fn test_bytes_slicing_is_zero_copy() {
    let original_data = vec![0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9];
    let bytes = Bytes::from(original_data);

    // Slicing should not allocate
    let slice1 = bytes.slice(0..5);
    let slice2 = bytes.slice(5..10);

    // Slices should point into the same buffer (just offset)
    assert!(slice1.as_ptr() < slice2.as_ptr());
    assert_eq!(slice1.len(), 5);
    assert_eq!(slice2.len(), 5);
}

#[test]
fn test_game_data_binary_with_bytes() {
    let payload = Bytes::from(vec![1u8, 2, 3, 4, 5]);
    let player_id = Uuid::new_v4();

    let message = MockServerMessage::GameDataBinary {
        from_player: player_id,
        payload,
    };

    // Wrapping in Arc for broadcast
    let broadcast = BroadcastMessage::new(message);

    // Cloning broadcast should be cheap
    let _clone1 = broadcast.clone();
    let _clone2 = broadcast.clone();

    // Reference count should reflect clones
    assert_eq!(Arc::strong_count(&broadcast.inner), 3);
}

// ============================================================================
// Arc Broadcast Message Tests
// ============================================================================

#[test]
fn test_broadcast_message_arc_cloning() {
    let player_id = Uuid::new_v4();
    let message = MockServerMessage::GameData {
        from_player: player_id,
        data: "test game state with some payload data".to_string(),
    };

    let broadcast = BroadcastMessage::new(message);

    // Simulate broadcasting to 8 players
    let clones: Vec<_> = (0..8).map(|_| broadcast.clone()).collect();

    // All clones should share the same underlying data
    for clone in &clones {
        assert!(Arc::ptr_eq(&broadcast.inner, &clone.inner));
    }

    // Reference count should be 9 (original + 8 clones)
    assert_eq!(Arc::strong_count(&broadcast.inner), 9);
}

#[test]
fn test_arc_message_memory_efficiency() {
    // Large message that would be expensive to clone
    let large_data = "x".repeat(10000);
    let message = MockServerMessage::GameData {
        from_player: Uuid::new_v4(),
        data: large_data,
    };

    let broadcast = BroadcastMessage::new(message);

    // Even with many clones, memory usage stays constant
    let _clones: Vec<_> = (0..100).map(|_| broadcast.clone()).collect();

    // Only one copy of the message exists (plus Arc overhead)
    assert_eq!(Arc::strong_count(&broadcast.inner), 101);
}

// ============================================================================
// Multi-Client Broadcast Simulation Tests
// ============================================================================

#[test]
fn test_room_broadcast_simulation() {
    // Simulate a room with 8 players
    let room_players: PlayerIdList = (0..8).map(|_| Uuid::new_v4()).collect();
    assert!(!room_players.spilled(), "Room players should be on stack");

    // Create a message to broadcast
    let message = MockServerMessage::GameData {
        from_player: room_players[0],
        data: "player move: x=100, y=200".to_string(),
    };

    let broadcast = BroadcastMessage::new(message);

    // Simulate sending to all players except sender
    // Store the clones to keep them alive (simulating queued messages)
    let sender = room_players[0];
    let sent_messages: Vec<_> = room_players
        .iter()
        .filter(|&player_id| *player_id != sender)
        .map(|_| broadcast.arc_clone())
        .collect();

    assert_eq!(sent_messages.len(), 7, "Should send to 7 other players");
    assert_eq!(
        Arc::strong_count(&broadcast.inner),
        8,
        "Should have 8 references (1 original + 7 sent)"
    );
}

#[test]
fn test_large_room_broadcast_simulation() {
    // Simulate a large room with 20 players (exceeds SmallVec capacity)
    let room_players: PlayerIdList = (0..20).map(|_| Uuid::new_v4()).collect();
    assert!(
        room_players.spilled(),
        "Large room should spill to heap, but still work"
    );

    // Create a binary game data message
    let payload = Bytes::from(vec![0u8; 1024]); // 1KB payload
    let message = MockServerMessage::GameDataBinary {
        from_player: room_players[0],
        payload,
    };

    let broadcast = BroadcastMessage::new(message);

    // Broadcast to all players
    let _sent: Vec<_> = room_players
        .iter()
        .skip(1) // Skip sender
        .map(|_| broadcast.arc_clone())
        .collect();

    // 19 recipients + original
    assert_eq!(Arc::strong_count(&broadcast.inner), 20);
}

// ============================================================================
// Performance Benchmarks (Basic Timing)
// ============================================================================

#[test]
fn test_broadcast_clone_performance() {
    let message = MockServerMessage::GameData {
        from_player: Uuid::new_v4(),
        data: "x".repeat(1000), // 1KB payload
    };

    let broadcast = BroadcastMessage::new(message);

    // Measure time for 10000 clones
    let start = Instant::now();
    for _ in 0..10000 {
        let _ = broadcast.clone();
    }
    let arc_duration = start.elapsed();

    // Arc clones should be very fast (sub-microsecond each)
    // This is a sanity check, not a strict benchmark
    assert!(
        arc_duration < Duration::from_millis(100),
        "Arc clones should be fast, took {arc_duration:?}"
    );

    println!(
        "10000 Arc clones took {:?} ({:?} per clone)",
        arc_duration,
        arc_duration / 10000
    );
}

#[test]
fn test_bytes_clone_performance() {
    let payload = Bytes::from(vec![0u8; 4096]); // 4KB payload

    // Measure time for 10000 Bytes clones
    let start = Instant::now();
    for _ in 0..10000 {
        let _ = payload.clone();
    }
    let duration = start.elapsed();

    // Bytes clones should be very fast (just Arc increment)
    assert!(
        duration < Duration::from_millis(50),
        "Bytes clones should be fast, took {duration:?}"
    );

    println!(
        "10000 Bytes clones took {:?} ({:?} per clone)",
        duration,
        duration / 10000
    );
}

#[test]
fn test_smallvec_vs_vec_allocation() {
    // SmallVec for typical room
    let start = Instant::now();
    for _ in 0..10000 {
        let mut players: PlayerIdList = SmallVec::new();
        for _ in 0..8 {
            players.push(Uuid::new_v4());
        }
        assert!(!players.spilled());
    }
    let smallvec_duration = start.elapsed();

    // Regular Vec for comparison
    let start = Instant::now();
    for _ in 0..10000 {
        let mut players: Vec<PlayerId> = Vec::new();
        for _ in 0..8 {
            players.push(Uuid::new_v4());
        }
    }
    let vec_duration = start.elapsed();

    println!("SmallVec (8 players): {smallvec_duration:?}, Vec: {vec_duration:?}");

    // SmallVec should generally be faster or comparable for small sizes
    // (The main benefit is reduced allocator pressure, not raw speed)
}

// ============================================================================
// Concurrent Access Simulation
// ============================================================================

#[test]
fn test_concurrent_broadcast_simulation() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    let message = MockServerMessage::GameData {
        from_player: Uuid::new_v4(),
        data: "concurrent test data".to_string(),
    };

    let broadcast = Arc::new(BroadcastMessage::new(message));
    let send_count = Arc::new(AtomicUsize::new(0));

    // Simulate 4 concurrent senders (like multiple rooms)
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let broadcast = broadcast.clone();
            let send_count = send_count.clone();

            thread::spawn(move || {
                // Each thread sends to 8 recipients
                for _ in 0..8 {
                    let _ = broadcast.arc_clone();
                    send_count.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    assert_eq!(send_count.load(Ordering::Relaxed), 32);
}

// ============================================================================
// Edge Case Tests
// ============================================================================

#[test]
fn test_empty_room_broadcast() {
    let room_players: PlayerIdList = SmallVec::new();
    assert!(room_players.is_empty());
    assert!(!room_players.spilled());

    // Broadcasting to empty room should handle gracefully
    let message = MockServerMessage::Pong;
    let broadcast = BroadcastMessage::new(message);

    let sent: Vec<_> = room_players.iter().map(|_| broadcast.arc_clone()).collect();

    assert!(sent.is_empty());
    assert_eq!(Arc::strong_count(&broadcast.inner), 1);
}

#[test]
fn test_single_player_room() {
    let room_players: PlayerIdList = SmallVec::from_elem(Uuid::new_v4(), 1);
    assert!(!room_players.spilled());

    // Broadcasting in single-player room (edge case)
    let message = MockServerMessage::PlayerJoined {
        player_id: room_players[0],
        player_name: "Solo Player".to_string(),
    };

    let broadcast = BroadcastMessage::new(message);

    // No recipients except the player themselves
    assert_eq!(Arc::strong_count(&broadcast.inner), 1);
}

#[test]
fn test_maximum_smallvec_capacity() {
    // Test at exactly the SmallVec capacity boundary
    let mut players: PlayerIdList = SmallVec::new();

    // Fill to exact capacity
    for _ in 0..TYPICAL_ROOM_SIZE {
        players.push(Uuid::new_v4());
    }

    assert_eq!(players.len(), TYPICAL_ROOM_SIZE);
    assert!(!players.spilled(), "Should be at capacity but not spilled");

    // Verify we can still iterate
    let count = players.iter().count();
    assert_eq!(count, TYPICAL_ROOM_SIZE);
}

#[test]
fn test_bytes_from_vec_conversion() {
    // Test the conversion path from incoming WebSocket binary data
    let incoming_data: Vec<u8> = vec![1, 2, 3, 4, 5, 6, 7, 8];
    let bytes = Bytes::from(incoming_data);

    assert_eq!(bytes.len(), 8);

    // Verify we can slice without allocation
    let header = bytes.slice(0..2);
    let payload = bytes.slice(2..);

    assert_eq!(header.len(), 2);
    assert_eq!(payload.len(), 6);
}

// ============================================================================
// Tests using real crate types (signal_fish_server::broadcast, protocol)
// ============================================================================

#[test]
fn test_real_broadcast_message_zero_cost_cloning() {
    use signal_fish_server::broadcast::BroadcastMessage;
    use signal_fish_server::protocol::ServerMessage;

    let message = ServerMessage::GameData {
        from_player: Uuid::new_v4(),
        data: serde_json::json!({"action": "move", "x": 100, "y": 200}),
    };

    let broadcast = BroadcastMessage::new(message);

    // Simulate broadcasting to 8 players via Arc clone
    let clones: Vec<_> = (0..8).map(|_| broadcast.arc_clone()).collect();
    assert_eq!(clones.len(), 8);

    // Verify the message content is intact through the Arc
    match broadcast.message() {
        ServerMessage::GameData { data, .. } => {
            assert_eq!(data["action"], "move");
        }
        _ => panic!("Expected GameData message"),
    }
}

#[test]
fn test_real_broadcast_message_json_serialization() {
    use signal_fish_server::broadcast::BroadcastMessage;
    use signal_fish_server::protocol::ServerMessage;

    let message = ServerMessage::Pong;
    let mut broadcast = BroadcastMessage::new(message);

    // Exercise the JSON serialization path
    let json_bytes = broadcast
        .get_or_serialize_json()
        .expect("JSON serialization should succeed");

    // Second call should return cached bytes
    let json_bytes2 = broadcast
        .get_or_serialize_json()
        .expect("cached JSON should succeed");

    // Both should be identical Arc references
    assert_eq!(json_bytes.len(), json_bytes2.len());

    // Verify it is valid JSON that deserializes back to Pong
    let text = std::str::from_utf8(&json_bytes).expect("valid UTF-8");
    let parsed: ServerMessage =
        serde_json::from_str(text).expect("should deserialize back to ServerMessage");
    match parsed {
        ServerMessage::Pong => {} // expected
        other => panic!("Expected Pong, got {other:?}"),
    }
}

#[test]
fn test_real_broadcast_message_performance() {
    use signal_fish_server::broadcast::BroadcastMessage;
    use signal_fish_server::protocol::ServerMessage;

    let message = ServerMessage::GameData {
        from_player: Uuid::new_v4(),
        data: serde_json::json!({"state": "x".repeat(1000)}),
    };

    let broadcast = BroadcastMessage::new(message);

    // Measure time for 10000 Arc clones of a real BroadcastMessage
    let start = Instant::now();
    for _ in 0..10_000 {
        let _ = broadcast.clone();
    }
    let duration = start.elapsed();

    assert!(
        duration < Duration::from_millis(100),
        "Real BroadcastMessage Arc clones should be fast, took {duration:?}"
    );
}

#[test]
fn test_real_server_message_serialization_roundtrip() {
    use signal_fish_server::protocol::{PlayerInfo, ServerMessage};

    let player_id = Uuid::new_v4();
    let messages = vec![
        ServerMessage::Pong,
        ServerMessage::RoomLeft,
        ServerMessage::PlayerLeft {
            player_id: Uuid::new_v4(),
        },
        ServerMessage::GameData {
            from_player: player_id,
            data: serde_json::json!({"x": 1, "y": 2}),
        },
        ServerMessage::PlayerJoined {
            player: PlayerInfo {
                id: player_id,
                name: "TestPlayer".to_string(),
                is_authority: false,
                is_ready: false,
                connected_at: chrono::Utc::now(),
                connection_info: None,
                region_id: "test".to_string(),
            },
        },
    ];

    for msg in &messages {
        let json = serde_json::to_string(msg).expect("serialization should succeed");
        let _parsed: ServerMessage =
            serde_json::from_str(&json).expect("deserialization should succeed");
    }
}

#[test]
fn test_real_broadcast_room_simulation() {
    use signal_fish_server::broadcast::{BroadcastMessage, TYPICAL_ROOM_SIZE};
    use signal_fish_server::protocol::ServerMessage;

    // Simulate a full room of TYPICAL_ROOM_SIZE players
    let room_players: SmallVec<[Uuid; 8]> =
        (0..TYPICAL_ROOM_SIZE).map(|_| Uuid::new_v4()).collect();
    assert!(
        !room_players.spilled(),
        "Room player list should stay on stack"
    );

    let message = ServerMessage::GameData {
        from_player: room_players[0],
        data: serde_json::json!({"move": "forward", "speed": 5}),
    };

    let broadcast = BroadcastMessage::new(message);

    // Broadcast to all players except the sender, using arc_clone
    let sender = room_players[0];
    let sent: Vec<_> = room_players
        .iter()
        .filter(|&&id| id != sender)
        .map(|_| broadcast.arc_clone())
        .collect();

    assert_eq!(
        sent.len(),
        TYPICAL_ROOM_SIZE - 1,
        "Should send to all players except sender"
    );
}
