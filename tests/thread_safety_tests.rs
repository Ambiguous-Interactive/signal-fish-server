//! Thread safety invariant tests for signal-fish-server.
//!
//! These integration tests verify that concurrent access to shared state
//! (database, distributed locks, circuit breakers, message coordinator)
//! never produces partial state, data corruption, or deadlocks.

use signal_fish_server::database::{GameDatabase, InMemoryDatabase};
use signal_fish_server::distributed::{
    CircuitBreaker, CircuitState, DistributedLock, InMemoryDistributedLock,
};
use signal_fish_server::protocol::{PlayerInfo, ServerMessage};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Barrier;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a `PlayerInfo` with sensible defaults for testing.
fn make_player(player_id: Uuid) -> PlayerInfo {
    PlayerInfo {
        id: player_id,
        name: format!("Player-{}", &player_id.to_string()[..8]),
        is_authority: false,
        is_ready: false,
        connected_at: chrono::Utc::now(),
        connection_info: None,
        epoch: None,
        seq: None,
        region_id: "us-east-1".to_string(),
    }
}

/// Create a room through the database with sensible defaults.
async fn create_room(
    db: &InMemoryDatabase,
    game_name: &str,
    room_code: &str,
    max_players: u8,
) -> signal_fish_server::protocol::Room {
    db.create_room(
        game_name.to_string(),
        Some(room_code.to_string()),
        max_players,
        true,
        Uuid::new_v4(),
        "relay".to_string(),
        "us-east-1".to_string(),
        None,
    )
    .await
    .expect("room creation should succeed")
}

// ===========================================================================
// A. InMemoryDatabase thread safety tests
// ===========================================================================

/// A1: Create rooms concurrently and verify get_room never sees partial state.
///
/// 20 tasks create rooms concurrently, each with a unique code.
/// 20 other tasks continuously call `get_room` for those codes.
/// Every successful `get_room` result must have a valid room_id that also
/// exists in `get_room_by_id`. No task should ever observe a room in one
/// map but not the other.
#[tokio::test]
async fn test_concurrent_create_and_get_room_no_partial_state() {
    let db = Arc::new(InMemoryDatabase::new());
    let task_count = 20;
    let barrier = Arc::new(Barrier::new(task_count * 2));

    let mut handles = Vec::with_capacity(task_count * 2);

    // Producer tasks: create rooms
    for i in 0..task_count {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            let code = format!("CR{i:04}");
            let _ = db
                .create_room(
                    "concurrent_game".to_string(),
                    Some(code),
                    4,
                    true,
                    Uuid::new_v4(),
                    "relay".to_string(),
                    "us-east-1".to_string(),
                    None,
                )
                .await;
        }));
    }

    // Reader tasks: continuously try get_room and cross-check
    for i in 0..task_count {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            let code = format!("CR{i:04}");
            // Multiple read attempts to increase chance of catching a partial state
            for _ in 0..50 {
                if let Ok(Some(room)) = db.get_room("concurrent_game", &code).await {
                    // Cross-check: the room must also be visible via get_room_by_id
                    let by_id = db
                        .get_room_by_id(&room.id)
                        .await
                        .expect("get_room_by_id should not error");
                    assert!(
                        by_id.is_some(),
                        "Room found by code but missing from rooms map (partial state): room_id={}",
                        room.id
                    );
                    assert_eq!(by_id.unwrap().id, room.id);
                }
                tokio::task::yield_now().await;
            }
        }));
    }

    for handle in handles {
        handle.await.expect("task should not panic");
    }

    // Post-join verification: confirm all 20 rooms are visible via both lookups.
    // This ensures the cross-check path was meaningfully exercised even if the
    // concurrent readers completed before the producers finished.
    for i in 0..task_count {
        let code = format!("CR{i:04}");
        let room = db
            .get_room("concurrent_game", &code)
            .await
            .expect("get_room should not error")
            .unwrap_or_else(|| panic!("Room with code {code} should exist after all tasks joined"));

        let by_id = db
            .get_room_by_id(&room.id)
            .await
            .expect("get_room_by_id should not error")
            .unwrap_or_else(|| {
                panic!(
                    "Room {} (code {code}) should exist in rooms map after all tasks joined",
                    room.id
                )
            });

        assert_eq!(
            by_id.id, room.id,
            "Room ID mismatch between get_room and get_room_by_id for code {code}"
        );
    }
}

/// A2: Create and delete rooms concurrently, verify no orphaned entries.
///
/// After all tasks complete, every room that exists in `get_room_by_id`
/// also exists in `get_room`, and vice versa.
#[tokio::test]
async fn test_concurrent_create_and_delete_room_consistency() {
    let db = Arc::new(InMemoryDatabase::new());

    // Pre-create 10 rooms
    let mut room_ids = Vec::new();
    let mut room_codes = Vec::new();
    for i in 0..10 {
        let code = format!("DEL{i:03}");
        let room = create_room(&db, "delete_game", &code, 4).await;
        room_ids.push(room.id);
        room_codes.push(code);
    }

    let barrier = Arc::new(Barrier::new(10));
    let mut handles = Vec::new();

    // 5 tasks delete rooms
    for room_id in room_ids.iter().take(5) {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        let room_id = *room_id;
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            let _ = db.delete_room(&room_id).await;
        }));
    }

    // 5 tasks try to get rooms from the *same* set being deleted (indices 0-4)
    // to exercise the concurrent read-while-delete path
    for code in room_codes.iter().take(5) {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        let code = code.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            for _ in 0..20 {
                let by_code = db.get_room("delete_game", &code).await.unwrap();
                if let Some(room) = by_code {
                    let by_id = db.get_room_by_id(&room.id).await.unwrap();
                    assert!(
                        by_id.is_some(),
                        "Room visible by code but missing from rooms map"
                    );
                }
                tokio::task::yield_now().await;
            }
        }));
    }

    for handle in handles {
        handle.await.expect("task should not panic");
    }

    // Final consistency check: every remaining room is in both maps
    for i in 0..10 {
        let by_id = db.get_room_by_id(&room_ids[i]).await.unwrap();
        let by_code = db.get_room("delete_game", &room_codes[i]).await.unwrap();

        match (&by_id, &by_code) {
            (Some(r1), Some(r2)) => {
                assert_eq!(r1.id, r2.id, "Mismatched room IDs between lookups");
            }
            (None, None) => { /* Room was deleted from both maps, consistent */ }
            (Some(_), None) => {
                panic!(
                    "Room {} exists by ID but not by code (orphaned in rooms map)",
                    room_ids[i]
                );
            }
            (None, Some(_)) => {
                panic!(
                    "Room {} exists by code but not by ID (orphaned in room_codes map)",
                    room_ids[i]
                );
            }
        }
    }
}

/// A3: Capacity enforcement under concurrent load.
///
/// Create a room with max_players=3 (1 creator = 2 open slots).
/// 20 tasks concurrently call add_player_to_room.
/// Exactly 2 should succeed (total 3 including creator).
#[tokio::test]
async fn test_concurrent_add_player_respects_capacity() {
    let db = Arc::new(InMemoryDatabase::new());
    let room = create_room(&db, "cap_game", "CAP001", 3).await;
    let room_id = room.id;

    let task_count = 20;
    let barrier = Arc::new(Barrier::new(task_count));
    let mut handles = Vec::with_capacity(task_count);

    for _ in 0..task_count {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            let player = make_player(Uuid::new_v4());
            db.add_player_to_room(&room_id, player).await
        }));
    }

    let mut successes = 0usize;
    for handle in handles {
        if let Ok(true) = handle.await.expect("task should not panic") {
            successes += 1;
        }
    }

    // 2 slots open (3 max - 1 creator)
    assert_eq!(
        successes, 2,
        "Expected exactly 2 successful adds (3 max - 1 creator), got {successes}"
    );

    // Verify final player count
    let final_room = db
        .get_room_by_id(&room_id)
        .await
        .unwrap()
        .expect("room should exist");
    assert_eq!(
        final_room.players.len(),
        3,
        "Room should have exactly 3 players"
    );
}

/// A4: Only one player gets authority atomically.
///
/// Create a room, release authority from creator.
/// 10 tasks concurrently request authority. Exactly 1 should succeed.
#[tokio::test]
async fn test_concurrent_authority_request_exactly_one_wins() {
    let db = Arc::new(InMemoryDatabase::new());
    let room = create_room(&db, "auth_game", "AUTH01", 12).await;
    let room_id = room.id;
    let creator_id = *room.players.keys().next().unwrap();

    // Add 10 more players
    let mut player_ids = Vec::new();
    for _ in 0..10 {
        let pid = Uuid::new_v4();
        let player = make_player(pid);
        db.add_player_to_room(&room_id, player)
            .await
            .expect("add player should succeed");
        player_ids.push(pid);
    }

    // Release authority from creator
    let released = db
        .request_room_authority(&room_id, &creator_id, false)
        .await
        .expect("release should not error");
    assert!(
        released.granted(),
        "Creator should be able to release authority"
    );

    // All 10 players concurrently request authority
    let task_count = player_ids.len();
    let barrier = Arc::new(Barrier::new(task_count));
    let mut handles = Vec::with_capacity(task_count);

    for pid in &player_ids {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        let pid = *pid;
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            db.request_room_authority(&room_id, &pid, true).await
        }));
    }

    let mut winners = 0usize;
    for handle in handles {
        let result = handle.await.expect("task should not panic");
        if result.is_ok_and(|outcome| outcome.granted()) {
            winners += 1;
        }
    }

    assert_eq!(
        winners, 1,
        "Exactly 1 player should win the authority race, got {winners}"
    );

    // Verify final room state
    let final_room = db
        .get_room_by_id(&room_id)
        .await
        .unwrap()
        .expect("room should exist");
    assert!(
        final_room.authority_player.is_some(),
        "Room should have an authority player"
    );
    let authority_count = final_room
        .players
        .values()
        .filter(|p| p.is_authority)
        .count();
    assert_eq!(
        authority_count, 1,
        "Exactly 1 player should have is_authority=true"
    );
}

/// A5: Cleanup and creation do not interfere with each other.
///
/// Create a room, empty it (remove all players).
/// 5 tasks concurrently call cleanup_expired_rooms while 5 tasks each create
/// a new room with a unique code under the same game name.
/// After joining, verify all newly created rooms are consistent (exist in both maps).
#[tokio::test]
async fn test_cleanup_rooms_atomic_with_creation() {
    let db = Arc::new(InMemoryDatabase::new());

    // Create a room and remove all players (make it empty and eligible for cleanup)
    let room = create_room(&db, "cleanup_game", "CLN001", 4).await;
    let creator_id = *room.players.keys().next().unwrap();
    db.remove_player_from_room(&room.id, &creator_id)
        .await
        .expect("remove should succeed");

    let total_tasks = 10; // 5 cleanup + 5 creation
    let barrier = Arc::new(Barrier::new(total_tasks));
    let mut handles = Vec::with_capacity(total_tasks);

    // 5 cleanup tasks
    for _ in 0..5 {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            let protected = std::collections::HashSet::new();
            db.cleanup_expired_rooms(
                chrono::Duration::zero(),
                chrono::Duration::hours(1),
                &protected,
            )
            .await
            .expect("cleanup should not error");
        }));
    }

    // 5 creation tasks, each creating a room with a unique code
    let room_codes: Vec<String> = (2..=6).map(|i| format!("CLN{i:03}")).collect();
    for code in &room_codes {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        let code = code.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            db.create_room(
                "cleanup_game".to_string(),
                Some(code),
                4,
                true,
                Uuid::new_v4(),
                "relay".to_string(),
                "us-east-1".to_string(),
                None,
            )
            .await
            .expect("room creation should succeed");
        }));
    }

    for handle in handles {
        handle.await.expect("task should not panic");
    }

    // Verify the original empty room was actually cleaned up
    let cleaned = db.get_room("cleanup_game", "CLN001").await.unwrap();
    assert!(
        cleaned.is_none(),
        "Empty room CLN001 should have been cleaned up"
    );

    // Verify all newly created rooms are intact and consistent in both maps
    for code in &room_codes {
        let by_code = db
            .get_room("cleanup_game", code)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("Room with code {code} should exist by code"));
        let by_id = db
            .get_room_by_id(&by_code.id)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("Room {} (code {code}) should exist by ID", by_code.id));
        assert_eq!(by_id.id, by_code.id, "Room ID mismatch for code {code}");
    }
}

// ===========================================================================
// B. InMemoryDistributedLock tests
// ===========================================================================

/// B6: Only one task can hold a lock at a time.
#[tokio::test]
async fn test_distributed_lock_mutual_exclusion() {
    let lock = Arc::new(InMemoryDistributedLock::new());

    let task_count = 10;
    let barrier = Arc::new(Barrier::new(task_count));
    let mut handles = Vec::with_capacity(task_count);

    for _ in 0..task_count {
        let lock = Arc::clone(&lock);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            lock.try_acquire("mutex_key", Duration::from_secs(10)).await
        }));
    }

    let mut acquired_count = 0usize;
    let mut winning_handle = None;
    for handle in handles {
        let result = handle.await.expect("task should not panic");
        if let Ok(Some(h)) = result {
            acquired_count += 1;
            winning_handle = Some(h);
        }
    }

    assert_eq!(
        acquired_count, 1,
        "Exactly 1 task should acquire the lock, got {acquired_count}"
    );

    // After release, another can acquire
    let handle = winning_handle.unwrap();
    lock.release(&handle)
        .await
        .expect("release should not error");

    let reacquired = lock
        .try_acquire("mutex_key", Duration::from_secs(10))
        .await
        .expect("try_acquire should not error");
    assert!(
        reacquired.is_some(),
        "Lock should be acquirable after release"
    );
}

/// B7: Expired locks are cleaned up.
#[tokio::test]
async fn test_distributed_lock_expired_lock_released() {
    let lock = InMemoryDistributedLock::new();

    // Acquire with very short TTL; hold handle to prevent immediate release
    let handle = lock
        .try_acquire("expire_key", Duration::from_millis(1))
        .await
        .expect("acquire should not error")
        .expect("should acquire lock");

    // Wait for expiry
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(handle); // Explicitly drop after expiry

    // Another acquire should succeed because the original expired
    let new_handle = lock
        .try_acquire("expire_key", Duration::from_secs(10))
        .await
        .expect("try_acquire should not error");
    assert!(
        new_handle.is_some(),
        "Should acquire lock after previous one expired"
    );
}

/// B8: Extending a lock keeps it alive.
#[tokio::test]
async fn test_distributed_lock_extend_prevents_expiry() {
    let lock = InMemoryDistributedLock::new();

    let handle = lock
        .try_acquire("extend_key", Duration::from_millis(100))
        .await
        .expect("acquire should not error")
        .expect("should acquire lock");

    // Extend to 10 seconds
    let extended = lock
        .extend(&handle, Duration::from_secs(10))
        .await
        .expect("extend should not error");
    assert!(extended, "Lock extension should succeed");

    // Another task should NOT be able to acquire it
    let attempt = lock
        .try_acquire("extend_key", Duration::from_secs(1))
        .await
        .expect("try_acquire should not error");
    assert!(
        attempt.is_none(),
        "Lock should still be held after extension"
    );
}

/// B9: Release lets others acquire.
#[tokio::test]
async fn test_distributed_lock_release_allows_reacquire() {
    let lock = InMemoryDistributedLock::new();

    let handle = lock
        .try_acquire("release_key", Duration::from_secs(60))
        .await
        .expect("acquire should not error")
        .expect("should acquire lock");

    let released = lock
        .release(&handle)
        .await
        .expect("release should not error");
    assert!(released, "Release should succeed");

    let reacquired = lock
        .try_acquire("release_key", Duration::from_secs(60))
        .await
        .expect("try_acquire should not error");
    assert!(
        reacquired.is_some(),
        "Should immediately acquire after release"
    );
}

/// B10: Barrier-synchronized concurrent acquire - exactly 1 wins.
#[tokio::test]
async fn test_distributed_lock_concurrent_acquire_exactly_one() {
    let lock = Arc::new(InMemoryDistributedLock::new());
    let task_count = 20;
    let barrier = Arc::new(Barrier::new(task_count));
    let mut handles = Vec::with_capacity(task_count);

    for _ in 0..task_count {
        let lock = Arc::clone(&lock);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            lock.try_acquire("race_key", Duration::from_secs(10)).await
        }));
    }

    let mut some_count = 0usize;
    let mut none_count = 0usize;
    for handle in handles {
        match handle
            .await
            .expect("task should not panic")
            .expect("try_acquire should not error")
        {
            Some(_) => some_count += 1,
            None => none_count += 1,
        }
    }

    assert_eq!(some_count, 1, "Exactly 1 task should acquire the lock");
    assert_eq!(none_count, task_count - 1, "All others should get None");
}

/// B11: is_locked returns false for expired locks.
#[tokio::test]
async fn test_is_locked_returns_false_for_expired() {
    let lock = InMemoryDistributedLock::new();

    // Acquire with very short TTL; hold handle to prevent immediate release
    let handle = lock
        .try_acquire("expiry_check", Duration::from_millis(1))
        .await
        .expect("acquire should not error")
        .expect("should acquire lock");

    // Wait for expiry
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(handle); // Explicitly drop after expiry

    let locked = lock
        .is_locked("expiry_check")
        .await
        .expect("is_locked should not error");
    assert!(!locked, "is_locked should return false for expired lock");
}

// ===========================================================================
// C. CircuitBreaker tests
// ===========================================================================

/// Wait until the breaker enters the half-open state (a probe was admitted).
async fn wait_for_half_open(breaker: &CircuitBreaker) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while breaker.get_state().await != CircuitState::HalfOpen {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the breaker should reach the half-open state");
}

/// C12: Circuit opens after threshold failures.
#[tokio::test]
async fn test_circuit_breaker_opens_after_threshold() {
    let breaker = CircuitBreaker::new(3, Duration::from_secs(1));

    // 3 failures
    for _ in 0..3 {
        let result: Result<(), anyhow::Error> = breaker
            .call(async { Err(anyhow::anyhow!("simulated failure")) })
            .await;
        assert!(result.is_err());
    }

    // Circuit should now be open
    assert_eq!(breaker.get_state().await, CircuitState::Open);

    // 4th call should get "Circuit breaker is open" immediately
    let result: Result<(), anyhow::Error> = breaker.call(async { Ok(()) }).await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Circuit breaker is open"),
        "Expected 'Circuit breaker is open', got: {err_msg}"
    );
}

/// C13: Circuit transitions to HalfOpen after timeout.
#[tokio::test]
async fn test_circuit_breaker_half_open_after_timeout() {
    let breaker = CircuitBreaker::new(3, Duration::from_millis(50));

    // Open the circuit
    for _ in 0..3 {
        let _: Result<(), anyhow::Error> = breaker
            .call(async { Err(anyhow::anyhow!("failure")) })
            .await;
    }
    assert_eq!(breaker.get_state().await, CircuitState::Open);

    // Sleep past timeout
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Next call should execute (half-open allows probe)
    let result: Result<(), anyhow::Error> = breaker.call(async { Ok(()) }).await;
    assert!(
        result.is_ok(),
        "Probe call should succeed in half-open state"
    );

    // After successful probe, circuit should close
    assert_eq!(breaker.get_state().await, CircuitState::Closed);
}

/// C14: Reset returns to Closed with zero failures.
#[tokio::test]
async fn test_circuit_breaker_reset_clears_state() {
    let breaker = CircuitBreaker::new(3, Duration::from_secs(60));

    // Open the circuit
    for _ in 0..3 {
        let _: Result<(), anyhow::Error> = breaker
            .call(async { Err(anyhow::anyhow!("failure")) })
            .await;
    }
    assert_eq!(breaker.get_state().await, CircuitState::Open);

    // Reset
    breaker.reset().await;
    assert_eq!(breaker.get_state().await, CircuitState::Closed);

    // Operations should proceed normally
    let result: Result<(), anyhow::Error> = breaker.call(async { Ok(()) }).await;
    assert!(result.is_ok(), "Operations should work after reset");
}

/// C15: Concurrent calls do not corrupt state.
///
/// 20 tasks call the circuit breaker concurrently. Half intend to succeed,
/// half intend to fail. The circuit may open mid-flight, causing some
/// tasks to be rejected by an open breaker. With consecutive-failure
/// accounting, a success resets the streak, so the terminal state depends on
/// scheduling; the invariant is that all tasks complete (no deadlock), no
/// panics occur, and the final state is a valid circuit state.
#[tokio::test]
async fn test_circuit_breaker_concurrent_calls_safe() {
    let breaker = Arc::new(CircuitBreaker::new(10, Duration::from_secs(60)));
    let task_count = 20;
    let barrier = Arc::new(Barrier::new(task_count));
    let mut handles = Vec::with_capacity(task_count);

    for i in 0..task_count {
        let breaker = Arc::clone(&breaker);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            if i.is_multiple_of(2) {
                // Even tasks try a successful operation (may be rejected if breaker is open)
                let _result: Result<i32, anyhow::Error> = breaker.call(async { Ok(42) }).await;
            } else {
                // Odd tasks try a failing operation (may be rejected if breaker is open)
                let _result: Result<i32, anyhow::Error> = breaker
                    .call(async { Err(anyhow::anyhow!("failure")) })
                    .await;
            }
        }));
    }

    // All tasks should complete without panic or deadlock
    let timeout_result = tokio::time::timeout(Duration::from_secs(5), async {
        for handle in handles {
            handle.await.expect("task should not panic");
        }
    })
    .await;

    assert!(
        timeout_result.is_ok(),
        "All concurrent circuit breaker calls should complete within 5 seconds"
    );

    // Mixed concurrent traffic yields a scheduling-dependent terminal state:
    // successes reset the consecutive-failure streak while it is open.
    let state = breaker.get_state().await;
    assert!(
        matches!(state, CircuitState::Closed | CircuitState::Open),
        "Expected a valid terminal circuit state under mixed traffic, got {state:?}"
    );
}

/// C15b: Concurrent failures trip the threshold deterministically.
#[tokio::test]
async fn test_circuit_breaker_concurrent_failures_open() {
    let breaker = Arc::new(CircuitBreaker::new(5, Duration::from_secs(60)));
    let task_count = 10;
    let barrier = Arc::new(Barrier::new(task_count));
    let mut handles = Vec::with_capacity(task_count);

    for _ in 0..task_count {
        let breaker = Arc::clone(&breaker);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            let _result: Result<(), anyhow::Error> = breaker
                .call(async { Err(anyhow::anyhow!("failure")) })
                .await;
        }));
    }

    for handle in handles {
        handle.await.expect("task should not panic");
    }

    let state = breaker.get_state().await;
    assert_eq!(
        state,
        CircuitState::Open,
        "Expected Open state after 10 concurrent failures with threshold=5, got {state:?}"
    );
}

/// C16: HalfOpen state transitions back to Open on a failing probe.
///
/// Opens the circuit (3 failures with threshold=3, timeout=50ms),
/// sleeps past the timeout so the next call enters HalfOpen,
/// sends a failing probe, and asserts the circuit is back to Open.
#[tokio::test]
async fn test_circuit_breaker_half_open_failure_reopens() {
    let breaker = CircuitBreaker::new(3, Duration::from_millis(50));

    // Open the circuit with 3 failures
    for _ in 0..3 {
        let _: Result<(), anyhow::Error> = breaker
            .call(async { Err(anyhow::anyhow!("failure")) })
            .await;
    }
    assert_eq!(breaker.get_state().await, CircuitState::Open);

    // Sleep past the timeout so the breaker transitions to HalfOpen on next call
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send a failing probe call (should transition HalfOpen -> Open)
    let result: Result<(), anyhow::Error> = breaker
        .call(async { Err(anyhow::anyhow!("probe failure")) })
        .await;
    assert!(result.is_err(), "Probe call should fail");

    // Circuit should be back to Open after the failed probe
    assert_eq!(
        breaker.get_state().await,
        CircuitState::Open,
        "Circuit should reopen after a failed probe in HalfOpen state"
    );
}

/// C17: Closed-state failures must be consecutive to open the circuit.
///
/// A success between failures resets the failure streak, so isolated
/// failures never trip the threshold (issue #403).
#[tokio::test]
async fn test_circuit_breaker_requires_consecutive_failures() {
    let breaker = CircuitBreaker::new(2, Duration::from_secs(60));

    // failure, success, failure: the success must reset the streak.
    let _: Result<(), anyhow::Error> = breaker
        .call(async { Err(anyhow::anyhow!("failure")) })
        .await;
    let _: Result<(), anyhow::Error> = breaker.call(async { Ok(()) }).await;
    let _: Result<(), anyhow::Error> = breaker
        .call(async { Err(anyhow::anyhow!("failure")) })
        .await;
    assert_eq!(
        breaker.get_state().await,
        CircuitState::Closed,
        "non-consecutive failures must not open the circuit"
    );

    // Two consecutive failures counted from the reset streak trip the threshold.
    let _: Result<(), anyhow::Error> = breaker
        .call(async { Err(anyhow::anyhow!("failure")) })
        .await;
    let _: Result<(), anyhow::Error> = breaker
        .call(async { Err(anyhow::anyhow!("failure")) })
        .await;
    assert_eq!(
        breaker.get_state().await,
        CircuitState::Open,
        "consecutive failures must still open the circuit"
    );
}

/// C18: Only one probe may execute while the circuit is half-open.
///
/// While a half-open probe is blocked inside its operation, a second call is
/// rejected without executing its operation (issue #403).
#[tokio::test]
async fn test_circuit_breaker_admits_single_half_open_probe() {
    let breaker = Arc::new(CircuitBreaker::new(1, Duration::from_millis(50)));

    // Open the circuit with one failure.
    let _: Result<(), anyhow::Error> = breaker
        .call(async { Err(anyhow::anyhow!("failure")) })
        .await;
    assert_eq!(breaker.get_state().await, CircuitState::Open);

    // Sleep past the timeout so the next call is admitted as the probe.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The probe blocks inside its operation until released.
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let breaker_for_probe = Arc::clone(&breaker);
    let probe = tokio::spawn(async move {
        breaker_for_probe
            .call(async move {
                let _ = release_rx.await;
                Result::<(), anyhow::Error>::Ok(())
            })
            .await
    });

    // Deterministically wait for the probe to enter the half-open state.
    wait_for_half_open(&breaker).await;

    // A concurrent call must be rejected without executing its operation.
    let second_executed = Arc::new(AtomicBool::new(false));
    let executed_flag = Arc::clone(&second_executed);
    let second: Result<(), anyhow::Error> = breaker
        .call(async move {
            executed_flag.store(true, Ordering::SeqCst);
            Err(anyhow::anyhow!("second call must not run"))
        })
        .await;
    assert!(
        second.is_err(),
        "half-open breaker must reject calls while a probe is in flight"
    );
    let error_message = second.unwrap_err().to_string();
    assert!(
        error_message.contains("half-open"),
        "rejection must cite the half-open state, got: {error_message}"
    );
    assert!(
        !second_executed.load(Ordering::SeqCst),
        "a rejected call's operation must not execute"
    );

    // Releasing the probe lets it succeed and close the circuit.
    release_tx
        .send(())
        .expect("the blocked probe must still be waiting");
    let probe_result = probe.await.expect("probe task should join");
    assert!(probe_result.is_ok(), "the released probe should succeed");
    assert_eq!(breaker.get_state().await, CircuitState::Closed);
}

/// C19: An abandoned half-open probe must not wedge the breaker.
///
/// Dropping a call future mid-operation releases the probe slot so a later
/// call can become a fresh probe and recover the circuit.
#[tokio::test]
async fn test_circuit_breaker_recovers_from_abandoned_half_open_probe() {
    let breaker = Arc::new(CircuitBreaker::new(1, Duration::from_millis(50)));

    // Open the circuit with one failure.
    let _: Result<(), anyhow::Error> = breaker
        .call(async { Err(anyhow::anyhow!("failure")) })
        .await;
    assert_eq!(breaker.get_state().await, CircuitState::Open);

    // Sleep past the timeout so the next call is admitted as the probe.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Start a probe whose operation starts but never resolves.
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let breaker_for_probe = Arc::clone(&breaker);
    let probe = tokio::spawn(async move {
        breaker_for_probe
            .call(async move {
                let _ = started_tx.send(());
                std::future::pending::<Result<(), anyhow::Error>>().await
            })
            .await
    });
    started_rx.await.expect("the probe operation should start");

    // Abandon the probe: aborting the task drops the call future mid-operation.
    probe.abort();
    let _ = probe.await;

    // The next call must be admitted as a fresh probe and recover the circuit.
    let result: Result<(), anyhow::Error> = breaker.call(async { Ok(()) }).await;
    assert!(
        result.is_ok(),
        "a fresh probe must be admitted after the previous probe was abandoned"
    );
    assert_eq!(
        breaker.get_state().await,
        CircuitState::Closed,
        "a successful fresh probe must close the circuit"
    );
}

/// C20: A successful probe stays authoritative over concurrent stragglers.
///
/// A straggler call admitted while the circuit was closed fails while the
/// half-open probe is in flight; it counts toward the streak but must not
/// reopen the circuit or veto the probe, and the successful probe must close
/// the circuit (PR #405 review finding).
#[tokio::test]
async fn test_circuit_breaker_probe_success_overrides_straggler_failure() {
    let breaker = Arc::new(CircuitBreaker::new(1, Duration::from_millis(50)));

    // Admit a straggler while Closed that blocks inside its operation and then
    // fails only when released.
    let (straggler_started_tx, straggler_started_rx) = tokio::sync::oneshot::channel::<()>();
    let (straggler_release_tx, straggler_release_rx) = tokio::sync::oneshot::channel::<()>();
    let breaker_for_straggler = Arc::clone(&breaker);
    let straggler = tokio::spawn(async move {
        breaker_for_straggler
            .call(async move {
                let _ = straggler_started_tx.send(());
                let _ = straggler_release_rx.await;
                Result::<(), anyhow::Error>::Err(anyhow::anyhow!("straggler failure"))
            })
            .await
    });
    straggler_started_rx
        .await
        .expect("the straggler should be admitted while closed");

    // Trip the threshold while the straggler is still pending.
    let _: Result<(), anyhow::Error> = breaker
        .call(async { Err(anyhow::anyhow!("failure")) })
        .await;
    assert_eq!(breaker.get_state().await, CircuitState::Open);

    // Sleep past the timeout so the next call is admitted as the probe.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The probe blocks inside its operation until released.
    let (probe_release_tx, probe_release_rx) = tokio::sync::oneshot::channel::<()>();
    let breaker_for_probe = Arc::clone(&breaker);
    let probe = tokio::spawn(async move {
        breaker_for_probe
            .call(async move {
                let _ = probe_release_rx.await;
                Result::<(), anyhow::Error>::Ok(())
            })
            .await
    });
    wait_for_half_open(&breaker).await;

    // The straggler fails while the probe is outstanding: it may not steal the
    // half-open transition from the probe.
    straggler_release_tx
        .send(())
        .expect("the blocked straggler must still be waiting");
    let straggler_result = straggler.await.expect("straggler task should join");
    assert!(straggler_result.is_err(), "the straggler should fail");
    assert_eq!(
        breaker.get_state().await,
        CircuitState::HalfOpen,
        "a straggler failure must not resolve another caller's probe"
    );

    // The successful probe closes the circuit despite the straggler failure.
    probe_release_tx
        .send(())
        .expect("the blocked probe must still be waiting");
    let probe_result = probe.await.expect("probe task should join");
    assert!(probe_result.is_ok(), "the released probe should succeed");
    assert_eq!(
        breaker.get_state().await,
        CircuitState::Closed,
        "a successful probe must close the circuit even after straggler failures"
    );
}

/// C21: A straggler success cannot resolve someone else's half-open probe.
///
/// Only the admitted probe owns the half-open transition: a closed-epoch
/// success resolving during the probe leaves the state untouched, and the
/// probe's own failure still reopens the circuit.
#[tokio::test]
async fn test_circuit_breaker_straggler_success_does_not_resolve_half_open() {
    let breaker = Arc::new(CircuitBreaker::new(2, Duration::from_millis(50)));

    // Admit a straggler while Closed that blocks and then succeeds on release.
    let (straggler_started_tx, straggler_started_rx) = tokio::sync::oneshot::channel::<()>();
    let (straggler_release_tx, straggler_release_rx) = tokio::sync::oneshot::channel::<()>();
    let breaker_for_straggler = Arc::clone(&breaker);
    let straggler = tokio::spawn(async move {
        breaker_for_straggler
            .call(async move {
                let _ = straggler_started_tx.send(());
                let _ = straggler_release_rx.await;
                Result::<(), anyhow::Error>::Ok(())
            })
            .await
    });
    straggler_started_rx
        .await
        .expect("the straggler should be admitted while closed");

    // Open the circuit with two consecutive fast failures.
    for _ in 0..2 {
        let _: Result<(), anyhow::Error> = breaker
            .call(async { Err(anyhow::anyhow!("failure")) })
            .await;
    }
    assert_eq!(breaker.get_state().await, CircuitState::Open);

    // Sleep past the timeout so the next call is admitted as the probe.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The failing probe blocks inside its operation until released.
    let (probe_release_tx, probe_release_rx) = tokio::sync::oneshot::channel::<()>();
    let breaker_for_probe = Arc::clone(&breaker);
    let probe = tokio::spawn(async move {
        breaker_for_probe
            .call(async move {
                let _ = probe_release_rx.await;
                Result::<(), anyhow::Error>::Err(anyhow::anyhow!("probe failure"))
            })
            .await
    });
    wait_for_half_open(&breaker).await;

    // The straggler succeeds mid-probe: it must leave the half-open decision
    // to the probe instead of closing the circuit.
    straggler_release_tx
        .send(())
        .expect("the blocked straggler must still be waiting");
    let straggler_result = straggler.await.expect("straggler task should join");
    assert!(straggler_result.is_ok(), "the straggler should succeed");
    assert_eq!(
        breaker.get_state().await,
        CircuitState::HalfOpen,
        "a straggler success must not close the circuit while a probe is outstanding"
    );

    // The failed probe reopens the circuit.
    probe_release_tx
        .send(())
        .expect("the blocked probe must still be waiting");
    let probe_result = probe.await.expect("probe task should join");
    assert!(probe_result.is_err(), "the failing probe should fail");
    assert_eq!(
        breaker.get_state().await,
        CircuitState::Open,
        "a failed probe must reopen the circuit even after straggler successes"
    );
}

/// Wait until the breaker reaches `target`, driving the paused clock forward
/// while yielding so spawned admission tasks get scheduled.
async fn wait_for_state_paused(breaker: &CircuitBreaker, target: &CircuitState) {
    for _ in 0..10_000 {
        if &breaker.get_state().await == target {
            return;
        }
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(1)).await;
    }
    panic!("the breaker never reached {target:?}");
}

/// C22: A straggler failure resolving while the circuit is already open must
/// not restart the open-state window.
///
/// Each late closed-epoch failure that restamped `last_failure_time` would
/// push the half-open probe a full timeout further out, letting an endless
/// trickle of stragglers starve recovery. The window starts when the circuit
/// transitions into Open and stays anchored there.
#[tokio::test(start_paused = true)]
async fn test_circuit_breaker_straggler_failure_does_not_extend_open_window() {
    let breaker = Arc::new(CircuitBreaker::new(2, Duration::from_secs(60)));

    // Admit three gated operations while the circuit is still closed.
    let mut started_rxs = Vec::new();
    let mut release_txs = Vec::new();
    let mut tasks = Vec::new();
    for i in 0..3 {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let breaker_for_task = Arc::clone(&breaker);
        tasks.push(tokio::spawn(async move {
            let _: Result<(), anyhow::Error> = breaker_for_task
                .call(async move {
                    let _ = started_tx.send(());
                    let _ = release_rx.await;
                    Err(anyhow::anyhow!("straggler {i} failure"))
                })
                .await;
        }));
        started_rxs.push(started_rx);
        release_txs.push(release_tx);
    }
    for rx in started_rxs {
        rx.await.expect("each operation should be admitted");
    }

    // First failure: counted, circuit still closed.
    release_txs
        .remove(0)
        .send(())
        .expect("operation 0 must still be waiting");
    tasks.remove(0).await.expect("task should join");

    // Second failure at t=30s: trips the threshold and anchors the window.
    tokio::time::advance(Duration::from_secs(30)).await;
    release_txs
        .remove(0)
        .send(())
        .expect("operation 1 must still be waiting");
    tasks.remove(0).await.expect("task should join");
    assert_eq!(breaker.get_state().await, CircuitState::Open);

    // Third failure at t=60s: a late straggler resolving while already open.
    // Without the fix it would restamp the window here.
    tokio::time::advance(Duration::from_secs(30)).await;
    release_txs
        .remove(0)
        .send(())
        .expect("operation 2 must still be waiting");
    tasks
        .pop()
        .expect("task should join")
        .await
        .expect("task should join");
    assert_eq!(breaker.get_state().await, CircuitState::Open);

    // At t=91s the anchored window (opened at t=30s) has elapsed.
    tokio::time::advance(Duration::from_secs(31)).await;
    let recovery: Result<(), anyhow::Error> = breaker.call(async { Ok(()) }).await;
    assert!(
        recovery.is_ok(),
        "the probe must be admitted when the anchored window elapses; got {:?}",
        recovery.err().map(|e| e.to_string())
    );
    assert_eq!(
        breaker.get_state().await,
        CircuitState::Closed,
        "a successful recovery probe must close the circuit"
    );
}

/// C23: reset() discards outcomes from calls admitted before the reset.
///
/// A probe in flight when an administrative reset lands must not reopen the
/// freshly cleared circuit when it later fails; its outcome belongs to a
/// superseded era.
#[tokio::test(start_paused = true)]
async fn test_circuit_breaker_reset_discards_outstanding_probe_outcome() {
    let breaker = Arc::new(CircuitBreaker::new(1, Duration::from_secs(60)));

    // Open the circuit with one fast failure.
    let _: Result<(), anyhow::Error> = breaker
        .call(async { Err(anyhow::anyhow!("failure")) })
        .await;
    assert_eq!(breaker.get_state().await, CircuitState::Open);

    // A gated failing call becomes the probe once it starts AFTER the window
    // elapses. The explicit enter gate guarantees the call is admitted under
    // the already-elapsed window instead of racing the paused clock.
    let (probe_enter_tx, probe_enter_rx) = tokio::sync::oneshot::channel::<()>();
    let (probe_release_tx, probe_release_rx) = tokio::sync::oneshot::channel::<()>();
    let breaker_for_probe = Arc::clone(&breaker);
    let probe = tokio::spawn(async move {
        let _ = probe_enter_rx.await;
        breaker_for_probe
            .call(async move {
                let _ = probe_release_rx.await;
                Result::<(), anyhow::Error>::Err(anyhow::anyhow!("stale probe failure"))
            })
            .await
    });
    tokio::time::advance(Duration::from_secs(61)).await;
    probe_enter_tx
        .send(())
        .expect("the probe task must be waiting to enter");
    wait_for_state_paused(&breaker, &CircuitState::HalfOpen).await;

    // Administrative reset clears the state while the probe is outstanding.
    breaker.reset().await;
    assert_eq!(breaker.get_state().await, CircuitState::Closed);

    // The stale probe fails on release; its outcome must be discarded.
    probe_release_tx
        .send(())
        .expect("the blocked probe must still be waiting");
    let probe_result = probe.await.expect("probe task should join");
    assert!(
        probe_result.is_err(),
        "the stale probe's caller sees its error"
    );
    assert_eq!(
        breaker.get_state().await,
        CircuitState::Closed,
        "a stale probe failure must not reopen a freshly reset circuit"
    );

    // Normal operation continues after the reset.
    let result: Result<(), anyhow::Error> = breaker.call(async { Ok(()) }).await;
    assert!(result.is_ok(), "operations should work after reset");
}

// ===========================================================================
// F. InMemoryMessageCoordinator lock ordering tests
// ===========================================================================

/// F22: Broadcast and register/unregister do not deadlock, and final state is consistent.
///
/// 10 tasks concurrently broadcast to a room while 10 tasks
/// concurrently register and unregister clients. All tasks should complete
/// within 5 seconds (no deadlock). After joining, the initial 5 clients
/// should still be registered (register/unregister tasks only touch their own
/// ephemeral clients), verified by broadcasting and checking receipt.
#[tokio::test]
async fn test_concurrent_broadcast_and_register_no_deadlock() {
    use signal_fish_server::coordination::MessageCoordinator;
    use signal_fish_server::protocol::ServerMessage;
    use signal_fish_server::server::InMemoryMessageCoordinator;

    let coordinator = Arc::new(InMemoryMessageCoordinator::new());
    let room_id = Uuid::new_v4();

    // Pre-register 5 clients in the room, keeping receivers alive for post-check
    let mut initial_player_ids = Vec::new();
    let mut initial_receivers = Vec::new();
    for _ in 0..5 {
        let pid = Uuid::new_v4();
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        coordinator
            .register_local_client(
                pid,
                Some(room_id),
                signal_fish_server::coordination::ClientDeliveryHandle {
                    sender: tx.into(),
                    close: signal_fish_server::coordination::ConnectionCloseSignal::detached(),
                },
            )
            .await
            .expect("register should succeed");
        initial_player_ids.push(pid);
        initial_receivers.push(rx);
    }

    // Healthy consumers: continuously drain the initial receivers during the
    // storm. The delivery contract applies backpressure (and, after
    // `slow_consumer_timeout`, a disconnect) when a recipient's queue stays
    // full, so leaving these 16-slot queues undrained under 200 broadcasts
    // would correctly slow the storm down — and this test would misreport
    // that intended pacing as a lock deadlock. The property under test is
    // unchanged: broadcast and register/unregister must interleave freely
    // without deadlocking on the coordinator's internal locks.
    let (stop_draining, _stop_rx) = tokio::sync::watch::channel(false);
    let mut drain_handles = Vec::with_capacity(initial_receivers.len());
    for mut rx in initial_receivers {
        let mut stop = stop_draining.subscribe();
        drain_handles.push(tokio::spawn(async move {
            loop {
                tokio::select! {
                    message = rx.recv() => {
                        assert!(
                            message.is_some(),
                            "initial receiver channel closed during the storm"
                        );
                    }
                    changed = stop.changed() => {
                        changed.expect("stop signal sender dropped");
                        return rx;
                    }
                }
            }
        }));
    }

    let task_count = 20;
    let barrier = Arc::new(Barrier::new(task_count));
    let mut handles = Vec::with_capacity(task_count);

    // 10 tasks broadcast to the room
    for _ in 0..10 {
        let coordinator = Arc::clone(&coordinator);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            for _ in 0..20 {
                let msg = Arc::new(ServerMessage::Pong);
                let _ = coordinator.broadcast_to_room(&room_id, msg).await;
                tokio::task::yield_now().await;
            }
        }));
    }

    // 10 tasks register/unregister clients
    for _ in 0..10 {
        let coordinator = Arc::clone(&coordinator);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            for _ in 0..10 {
                let pid = Uuid::new_v4();
                let (tx, _rx) = tokio::sync::mpsc::channel(16);
                let _ = coordinator
                    .register_local_client(
                        pid,
                        Some(room_id),
                        signal_fish_server::coordination::ClientDeliveryHandle {
                            sender: tx.into(),
                            close:
                                signal_fish_server::coordination::ConnectionCloseSignal::detached(),
                        },
                    )
                    .await;
                tokio::task::yield_now().await;
                let _ = coordinator.unregister_local_client(&pid).await;
                tokio::task::yield_now().await;
            }
        }));
    }

    let timeout_result = tokio::time::timeout(Duration::from_secs(5), async {
        for handle in handles {
            handle.await.expect("task should not panic");
        }
    })
    .await;

    assert!(
        timeout_result.is_ok(),
        "Deadlock detected: concurrent broadcast + register/unregister did not complete within 5 seconds"
    );

    // Stop the drain tasks and reclaim the receivers for verification.
    stop_draining
        .send(true)
        .expect("drain tasks should still be listening for the stop signal");
    let mut initial_receivers = Vec::with_capacity(drain_handles.len());
    for handle in drain_handles {
        initial_receivers.push(handle.await.expect("drain task should not panic"));
    }

    // Correctness check: the initial 5 clients should still be registered.
    // Drain any buffered messages, then send a fresh broadcast and verify
    // that exactly 5 receivers get the new message.
    for rx in &mut initial_receivers {
        drain_pending_messages(rx, "pre-verification broadcast drain");
    }

    let verification_msg = Arc::new(ServerMessage::Pong);
    coordinator
        .broadcast_to_room(&room_id, verification_msg)
        .await
        .expect("verification broadcast should succeed");

    let mut received_count = 0usize;
    for rx in &mut initial_receivers {
        match rx.try_recv() {
            Ok(message) => match message.as_ref() {
                ServerMessage::Pong => received_count += 1,
                other => panic!("expected verification Pong broadcast, got {other:?}"),
            },
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                panic!("verification receiver disconnected")
            }
        }
    }

    assert_eq!(
        received_count, 5,
        "All 5 initial clients should still be registered and receive the verification broadcast, but only {received_count} received it"
    );
}

/// F23: Game-data message building is serialized with reconnect baseline capture.
///
/// A reconnect baseline must not observe a sender watermark while a broadcast
/// with that same stamp has not yet snapshotted recipients. This test blocks a
/// broadcast message builder while it holds the room-routing read lock, then
/// verifies that `register_local_client_with_initial_message` cannot build its
/// baseline until the broadcast releases that lock. The restored recipient then
/// receives only its initial baseline frame, proving it was not in the already
/// stamped broadcast's recipient snapshot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn game_data_builder_blocks_reconnect_baseline_until_recipient_snapshot_is_fixed() {
    use signal_fish_server::coordination::{
        ClientDeliveryHandle, ConnectionCloseSignal, DeliveryOutcome, MessageCoordinator,
    };
    use signal_fish_server::server::InMemoryMessageCoordinator;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    let coordinator = Arc::new(InMemoryMessageCoordinator::new());
    let room_id = Uuid::new_v4();
    let sender_id = Uuid::new_v4();
    let existing_recipient_id = Uuid::new_v4();
    let reconnecting_id = Uuid::new_v4();

    let (existing_tx, mut existing_rx) = tokio::sync::mpsc::channel(8);
    coordinator
        .register_local_client(
            existing_recipient_id,
            Some(room_id),
            ClientDeliveryHandle {
                sender: existing_tx.into(),
                close: ConnectionCloseSignal::detached(),
            },
        )
        .await
        .expect("existing recipient should register");

    let (builder_started_tx, builder_started_rx) = std::sync::mpsc::channel();
    let (release_builder_tx, release_builder_rx) = std::sync::mpsc::channel();
    let allocated_stamp = Arc::new(AtomicU64::new(0));

    let broadcast = {
        let coordinator = Arc::clone(&coordinator);
        let allocated_stamp = Arc::clone(&allocated_stamp);
        tokio::spawn(async move {
            let mut build_once = Some(move || {
                allocated_stamp.store(1, Ordering::SeqCst);
                builder_started_tx
                    .send(())
                    .expect("test should still wait for builder start");
                tokio::task::block_in_place(|| {
                    release_builder_rx
                        .recv()
                        .expect("test should release the broadcast builder");
                });
                Some(Arc::new(ServerMessage::GameData {
                    from_player: sender_id,
                    data: serde_json::json!({ "kind": "blocked-broadcast" }),
                    seq: Some(1),
                    epoch: Some(1),
                    class: None,
                    key: None,
                }))
            });
            let mut build_message = move || build_once.take().and_then(|build| build());
            coordinator
                .broadcast_to_room_except_with_borrowed_message(
                    &room_id,
                    &sender_id,
                    &mut build_message,
                )
                .await
                .expect("broadcast should complete");
        })
    };

    tokio::task::block_in_place(|| {
        builder_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("broadcast builder should start");
    });
    assert_eq!(
        allocated_stamp.load(Ordering::SeqCst),
        1,
        "broadcast builder should allocate its stamp before blocking"
    );

    let baseline_built = Arc::new(AtomicBool::new(false));
    let (reconnect_tx, mut reconnect_rx) = tokio::sync::mpsc::channel(8);
    let reconnect = {
        let coordinator = Arc::clone(&coordinator);
        let baseline_built = Arc::clone(&baseline_built);
        tokio::spawn(async move {
            coordinator
                .register_local_client_with_initial_message(
                    reconnecting_id,
                    room_id,
                    ClientDeliveryHandle {
                        sender: reconnect_tx.into(),
                        close: ConnectionCloseSignal::detached(),
                    },
                    Box::new(move || {
                        baseline_built.store(true, Ordering::SeqCst);
                        Arc::new(ServerMessage::Pong)
                    }),
                )
                .await
                .expect("reconnect registration should complete")
        })
    };

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !baseline_built.load(Ordering::SeqCst),
        "reconnect baseline was built while the stamped broadcast had not yet \
         fixed its recipient snapshot"
    );

    release_builder_tx
        .send(())
        .expect("broadcast builder should still be blocked");
    broadcast.await.expect("broadcast task should not panic");
    assert_eq!(
        reconnect.await.expect("reconnect task should not panic"),
        DeliveryOutcome::Delivered
    );
    assert!(
        baseline_built.load(Ordering::SeqCst),
        "reconnect baseline should build after the broadcast snapshot lock releases"
    );

    match reconnect_rx
        .recv()
        .await
        .expect("reconnected recipient should receive its baseline")
        .as_ref()
    {
        ServerMessage::Pong => {}
        other => panic!("expected initial Pong baseline, got {other:?}"),
    }
    match reconnect_rx.try_recv() {
        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
        Ok(message) => {
            panic!("reconnected recipient received already-snapshotted message: {message:?}")
        }
        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
            panic!("reconnected recipient queue disconnected before verification")
        }
    }

    match existing_rx
        .recv()
        .await
        .expect("existing recipient should receive the broadcast")
        .as_ref()
    {
        ServerMessage::GameData {
            from_player,
            seq,
            epoch,
            ..
        } => {
            assert_eq!(*from_player, sender_id);
            assert_eq!(*seq, Some(1));
            assert_eq!(*epoch, Some(1));
        }
        other => panic!("expected blocked GameData broadcast, got {other:?}"),
    }
}

fn drain_pending_messages(
    receiver: &mut tokio::sync::mpsc::Receiver<Arc<ServerMessage>>,
    context: &str,
) -> usize {
    let mut drained = 0;
    loop {
        match receiver.try_recv() {
            Ok(_) => drained += 1,
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => return drained,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                panic!("{context}: receiver disconnected while draining pending messages")
            }
        }
    }
}
