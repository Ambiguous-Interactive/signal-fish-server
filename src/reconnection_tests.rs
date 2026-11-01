use super::*;
use crate::metrics::ServerMetrics;
use crate::protocol::ConnectionInfo;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::{Barrier, Notify};

#[test]
fn reconnection_error_maps_each_variant_to_its_client_code() {
    // A bad/mismatched token is a token error; only an elapsed *window* is
    // expired. Guards the prior latent bug where the "token is invalid or
    // expired" reason matched `contains("expired")` and was mislabeled as
    // RECONNECTION_EXPIRED.
    assert_eq!(
        ReconnectionError::NoRecord.error_code(),
        ErrorCode::ReconnectionFailed
    );
    assert_eq!(
        ReconnectionError::AlreadyInProgress.error_code(),
        ErrorCode::ReconnectionFailed
    );
    assert_eq!(
        ReconnectionError::TokenMismatch.error_code(),
        ErrorCode::ReconnectionTokenInvalid
    );
    assert_eq!(
        ReconnectionError::TokenInvalid.error_code(),
        ErrorCode::ReconnectionTokenInvalid
    );
    assert_eq!(
        ReconnectionError::WindowExpired.error_code(),
        ErrorCode::ReconnectionExpired
    );
}

#[test]
fn reconnection_error_display_preserves_wire_reason_strings() {
    // The `Display` text is the client-facing wire `reason`; keep it stable.
    assert_eq!(
        ReconnectionError::NoRecord.to_string(),
        "No disconnection record found"
    );
    assert_eq!(
        ReconnectionError::AlreadyInProgress.to_string(),
        "Reconnection already in progress"
    );
    assert_eq!(
        ReconnectionError::TokenMismatch.to_string(),
        "Invalid reconnection token"
    );
    assert_eq!(
        ReconnectionError::TokenInvalid.to_string(),
        "Reconnection token is invalid or expired"
    );
    assert_eq!(
        ReconnectionError::WindowExpired.to_string(),
        "Reconnection window has expired"
    );
}

#[test]
fn test_reconnection_token_creation() {
    let player_id = Uuid::new_v4();
    let room_id = Uuid::new_v4();
    let validity_seconds: i64 = 300;
    let token = ReconnectionToken::new(player_id, room_id, validity_seconds);

    assert_eq!(token.player_id, player_id);
    assert_eq!(token.room_id, room_id);
    assert!(!token.is_expired());
    assert!(token.is_valid(&player_id, &room_id));

    let now = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .expect("fixed test timestamp must parse")
        .with_timezone(&Utc);

    assert_eq!(
        expiration_from_unsigned(now, 300),
        now.checked_add_signed(Duration::seconds(300))
            .expect("ordinary window fits")
    );
    for window in [i64::MAX as u64, (i64::MAX as u64) + 1, u64::MAX] {
        assert_eq!(
            expiration_from_unsigned(now, window),
            DateTime::<Utc>::MAX_UTC,
            "window {window} must saturate at Chrono's upper bound"
        );
    }

    let token = ReconnectionToken::new_with_unsigned_window(player_id, room_id, u64::MAX);

    assert_eq!(token.expires_at, DateTime::<Utc>::MAX_UTC);
    assert!(!token.is_expired());
    assert!(token.is_valid(&player_id, &room_id));
}

// Regression #836: Debug must not expose a bearer credential.
#[cfg(test)]
#[test]
fn reconnection_token_debug_never_exposes_the_credential() {
    let token = ReconnectionToken {
        token: "private-reconnect-credential".to_string(),
        player_id: Uuid::from_u128(1),
        room_id: Uuid::from_u128(2),
        ..ReconnectionToken::new(Uuid::from_u128(1), Uuid::from_u128(2), 300)
    };
    let disconnected = DisconnectedPlayer {
        player_id: token.player_id,
        room_id: token.room_id,
        disconnected_at: Utc::now(),
        token: token.clone(),
        last_sequence: 4,
        was_authority: false,
        player_info: None,
        last_epoch: 2,
    };
    for diagnostic in [
        format!("{token:?}"),
        format!("{token:#?}"),
        format!("{disconnected:?}"),
        format!("{disconnected:#?}"),
    ] {
        assert!(
            !diagnostic.contains(&token.token[..8]),
            "credential prefix leaked"
        );
        assert!(diagnostic.contains(&token.player_id.to_string()));
        assert!(diagnostic.contains(&token.room_id.to_string()));
    }
    assert_eq!(
        token.clone().token,
        token.token,
        "redaction must not change credentials"
    );
    assert!(token.is_valid(&token.player_id, &token.room_id));
}

#[test]
fn test_reconnection_token_validation() {
    let player_id = Uuid::new_v4();
    let room_id = Uuid::new_v4();
    let other_player = Uuid::new_v4();
    let other_room = Uuid::new_v4();

    let token = ReconnectionToken::new(player_id, room_id, 300);

    // Should be valid for correct player and room
    assert!(token.is_valid(&player_id, &room_id));

    // Should be invalid for wrong player
    assert!(!token.is_valid(&other_player, &room_id));

    // Should be invalid for wrong room
    assert!(!token.is_valid(&player_id, &other_room));
}

#[test]
fn test_event_buffer_push() {
    let room_id = Uuid::new_v4();
    let mut buffer = EventBuffer::new(room_id, 3);

    use crate::protocol::ServerMessage;

    // Add 5 events (buffer size is 3)
    for i in 0..5 {
        buffer.push(ServerMessage::Pong, i);
    }

    // Should only keep last 3 events
    assert_eq!(buffer.events.len(), 3);
    assert_eq!(buffer.events[0].sequence, 2); // Oldest kept
    assert_eq!(buffer.events[2].sequence, 4); // Newest
}

#[test]
fn extreme_event_buffer_size_does_not_panic_during_construction() {
    let buffer = std::panic::catch_unwind(|| EventBuffer::new(Uuid::new_v4(), usize::MAX));
    assert!(buffer.is_ok());
}

#[test]
fn cleared_events_preserve_replay_loss_evidence() {
    for prior_watermark in [None, Some(3), Some(100)] {
        let mut buffer = EventBuffer::new(Uuid::new_v4(), 4);
        buffer.evicted_watermark = prior_watermark;
        for sequence in [8, 20, 13] {
            buffer.push(ServerMessage::Pong, sequence);
        }
        buffer.clear();
        let expected = Some(prior_watermark.unwrap_or(0).max(20));
        assert!(buffer.events.is_empty());
        assert_eq!(
            buffer.evicted_watermark, expected,
            "#1347: clear loses replay"
        );
        buffer.clear();
        assert_eq!(buffer.evicted_watermark, expected);
        buffer.push(ServerMessage::Pong, 101);
        assert_eq!(buffer.get_events_after(20).len(), 1);
        assert_eq!(buffer.evicted_watermark, expected);
    }
    let mut empty = EventBuffer::new(Uuid::new_v4(), 0);
    empty.clear();
    assert_eq!(empty.evicted_watermark, None);
}

#[tokio::test]
async fn replay_cursor_reports_events_discarded_by_a_ring_clear() {
    let manager = ReconnectionManager::new(300, 4, Arc::new(ServerMetrics::new()));
    let room = Uuid::new_v4();
    for _ in 0..3 {
        manager.buffer_event(&room, ServerMessage::Pong).await;
    }
    manager
        .replay_state
        .write()
        .await
        .event_buffers
        .get_mut(&room)
        .unwrap()
        .clear();
    manager.buffer_event(&room, ServerMessage::Pong).await;
    for (cursor, truncated, events) in [(0, true, 1), (2, true, 1), (3, false, 1), (4, false, 0)] {
        let replay = manager.get_missed_events(&room, cursor).await;
        assert_eq!(replay.truncated, truncated, "#1347: cursor {cursor}");
        assert_eq!(replay.events.len(), events);
    }
}

#[test]
fn full_replay_ring_reuses_storage_and_preserves_exact_tail() {
    for limit in [0, 1, 4, 16, 64] {
        let mut buffer = EventBuffer::new(Uuid::new_v4(), limit);
        for sequence in 1..=limit as u64 {
            assert_eq!(buffer.push(ServerMessage::Pong, sequence), 0);
        }
        let capacity = buffer.events.capacity();
        for sequence in limit as u64 + 1..=limit as u64 + 128 {
            assert_eq!(buffer.push(ServerMessage::Pong, sequence), 1);
            assert_eq!(buffer.events.len(), limit);
            assert_eq!(
                buffer.events.capacity(),
                capacity,
                "#636: full ring grew at limit={limit}"
            );
            assert_eq!(buffer.evicted_watermark, Some(sequence - limit as u64));
            assert_eq!(
                buffer
                    .events
                    .iter()
                    .map(|event| event.sequence)
                    .collect::<Vec<_>>(),
                (sequence - limit as u64 + 1..=sequence).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn replay_capacity_changes_keep_exact_evictions_and_watermarks() {
    let mut buffer = EventBuffer::new(Uuid::new_v4(), 8);
    for sequence in [10, 30, 20, 40] {
        assert_eq!(buffer.push(ServerMessage::Pong, sequence), 0);
    }
    buffer.max_size = 2;
    assert_eq!(buffer.push(ServerMessage::Pong, 50), 3);
    assert_eq!(buffer.evicted_watermark, Some(30));
    assert_eq!(
        buffer
            .events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![40, 50]
    );
    buffer.max_size = 0;
    assert_eq!(buffer.push(ServerMessage::Pong, 5), 3);
    assert!(buffer.events.is_empty());
    assert_eq!(buffer.evicted_watermark, Some(50));
    buffer.max_size = 1;
    assert_eq!(buffer.push(ServerMessage::Pong, 60), 0);
    assert_eq!(buffer.events[0].sequence, 60);
    assert_eq!(buffer.evicted_watermark, Some(50));
}

#[test]
fn test_event_buffer_get_events_after() {
    let room_id = Uuid::new_v4();
    let mut buffer = EventBuffer::new(room_id, 10);

    use crate::protocol::ServerMessage;

    for i in 0..5 {
        buffer.push(ServerMessage::Pong, i);
    }

    // Get events after sequence 2
    let events = buffer.get_events_after(2);
    assert_eq!(events.len(), 2); // Sequences 3 and 4
}

#[tokio::test]
async fn test_reconnection_manager_flow() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = Arc::new(ReconnectionManager::new(300, 100, metrics));
    let player_id = Uuid::new_v4();
    let room_id = Uuid::new_v4();

    // Register disconnection
    let token = manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;

    // Validate reconnection
    let result = manager
        .validate_reconnection(&player_id, &room_id, &token)
        .await;
    assert!(result.is_ok());

    // Complete reconnection
    manager.complete_reconnection(&player_id).await;

    // Should no longer have pending reconnection
    assert!(!manager.has_pending_reconnection(&player_id).await);
}

/// `rooms_with_active_reconnections` reports the rooms that room GC must
/// spare. A registered (unexpired) disconnection lists its room; an empty
/// manager lists nothing; completing the reconnection drops the room.
/// Per-record expiry is decided by the manager's monotonic
/// `window_closed_at` deadline, not the wall-clock
/// `DisconnectedPlayer::is_expired` diagnostic (which the manager never
/// consults for eligibility).
#[tokio::test]
async fn rooms_with_active_reconnections_tracks_protected_rooms() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);

    assert!(
        manager.rooms_with_active_reconnections().await.is_empty(),
        "a fresh manager protects no rooms"
    );

    let player_id = Uuid::new_v4();
    let room_id = Uuid::new_v4();
    manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;

    let protected = manager.rooms_with_active_reconnections().await;
    assert!(
        protected.contains(&room_id),
        "a live reconnection record must protect its room from GC"
    );
    assert_eq!(protected.len(), 1);

    manager.complete_reconnection(&player_id).await;
    assert!(
        manager.rooms_with_active_reconnections().await.is_empty(),
        "completing the reconnection releases the room"
    );
}

/// Issue #890: stale or ineligible records never run the durable removal.
#[tokio::test(start_paused = true)]
async fn expired_membership_cleanup_rejects_ineligible_records_without_storage_work() {
    for case in ["unexpired", "claimed", "wrong_room", "missing"] {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(1, 100, Arc::clone(&metrics));
        let player = PlayerId::new_v4();
        let room = RoomId::new_v4();
        let token = manager
            .register_disconnection(player, room, false, None, 0)
            .await;
        manager.record_room_event(&room, &control_event()).await;
        let claim = if case == "claimed" {
            Some(
                manager
                    .claim_reconnection(&PlayerId::new_v4(), &player, &room, &token)
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        if case != "unexpired" {
            tokio::time::advance(StdDuration::from_secs(2)).await;
        }
        let candidate_room = if case == "wrong_room" {
            RoomId::new_v4()
        } else {
            room
        };
        let candidate_player = if case == "missing" {
            PlayerId::new_v4()
        } else {
            player
        };
        assert!(manager
            .cleanup_expired_membership(&candidate_player, &candidate_room, async {
                panic!("ineligible cleanup polled the durable removal: {case}");
            })
            .await
            .unwrap()
            .is_none());
        assert_eq!(manager.pending_reconnection_room(&player).await, Some(room));
        assert_eq!(
            metrics.reconnection_sessions_active.load(Ordering::Relaxed),
            1
        );
        assert_eq!(manager.get_missed_events(&room, 0).await.events.len(), 1);
        if let Some(claim) = claim {
            assert!(manager.release_reconnection_claim(&claim).await);
        }
    }
}

/// Issue #890: cleanup must own the expired record through a suspended write.
#[tokio::test(start_paused = true)]
async fn expired_membership_cleanup_blocks_record_replacement_until_commit() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(1, 100, metrics);
    let player = PlayerId::new_v4();
    let room = RoomId::new_v4();
    let replacement_room = RoomId::new_v4();
    manager
        .register_disconnection(player, room, false, None, 0)
        .await;
    tokio::time::advance(StdDuration::from_secs(2)).await;
    let release = tokio::sync::Notify::new();
    let mut cleanup = Box::pin(manager.cleanup_expired_membership(&player, &room, async {
        release.notified().await;
        Ok(None)
    }));
    assert!(futures_util::poll!(&mut cleanup).is_pending());
    let mut replacement =
        Box::pin(manager.register_disconnection(player, replacement_room, false, None, 1));
    assert!(
        futures_util::poll!(&mut replacement).is_pending(),
        "registration must not replace a record whose durable removal is in flight"
    );
    release.notify_one();
    assert!(cleanup.await.unwrap().is_some());
    replacement.await;
    assert_eq!(
        manager.pending_reconnection_room(&player).await,
        Some(replacement_room)
    );
}

/// Issue #890: failed or cancelled storage work releases its pin and retains retry state.
#[tokio::test(start_paused = true)]
async fn expired_membership_cleanup_failure_and_cancellation_retain_retry_state() {
    for cancel in [false, true] {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(1, 100, metrics);
        let player = PlayerId::new_v4();
        let room = RoomId::new_v4();
        manager
            .register_disconnection(player, room, false, None, 0)
            .await;
        tokio::time::advance(StdDuration::from_secs(2)).await;
        if cancel {
            let mut cleanup = Box::pin(manager.cleanup_expired_membership(
                &player,
                &room,
                std::future::pending(),
            ));
            assert!(futures_util::poll!(&mut cleanup).is_pending());
            drop(cleanup);
        } else {
            assert!(manager
                .cleanup_expired_membership(&player, &room, async {
                    anyhow::bail!("injected durable removal failure")
                })
                .await
                .is_err());
        }
        assert_eq!(manager.pending_reconnection_room(&player).await, Some(room));
        assert!(manager
            .cleanup_expired_membership(&player, &room, async { Ok(None) })
            .await
            .unwrap()
            .is_some());
        assert!(!manager.has_pending_reconnection(&player).await);
    }
}

/// Issue #257: once a reconnect is admitted, its exclusive claim owns the
/// restore attempt even if the original wall-clock window expires while
/// storage or room-lane work is still in flight. Room GC must therefore
/// continue protecting the empty room until the claim completes or is
/// released.
#[tokio::test(start_paused = true)]
async fn claimed_reconnection_remains_gc_protected_after_original_window() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let player_id = Uuid::new_v4();
    let room_id = Uuid::new_v4();
    let token = manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;
    let claimant = Uuid::new_v4();

    manager
        .claim_reconnection(&claimant, &player_id, &room_id, &token)
        .await
        .expect("the still-valid reconnect is admitted and claimed");

    tokio::time::advance(StdDuration::from_secs(301)).await;

    let protected = manager.rooms_with_active_reconnections().await;
    assert!(
        protected.contains(&room_id),
        "an in-flight claim must keep its room alive after the admission window crosses"
    );
}

/// Issue #257 sweep-side corollary: every expiry surface must skip a
/// record whose reconnection is claimed, so an admitted attempt can never
/// be vanished mid-flight by maintenance (a vanished record would fail
/// completion silently and a late teardown would open a fresh window
/// around a still-running restore). The unclaimed sibling expires
/// normally, and once the winner releases its claim past the original
/// deadline, the abandoned record becomes ordinary sweepable residue.
#[tokio::test(start_paused = true)]
async fn expired_but_claimed_reconnection_survives_every_expiry_surface() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let claimed_player = Uuid::new_v4();
    let expired_player = Uuid::new_v4();
    let room_id = Uuid::new_v4();
    let token = manager
        .register_disconnection(claimed_player, room_id, false, None, 0)
        .await;
    manager
        .register_disconnection(expired_player, room_id, false, None, 0)
        .await;
    let claimant = Uuid::new_v4();
    let claim = manager
        .claim_reconnection(&claimant, &claimed_player, &room_id, &token)
        .await
        .expect("the still-valid reconnect is admitted and claimed");

    tokio::time::advance(StdDuration::from_secs(301)).await;

    assert_eq!(
        manager.expired_cleanup_candidates().await,
        vec![(expired_player, room_id)],
        "only the unclaimed record may be nominated for durable cleanup"
    );
    assert_eq!(
        manager.cleanup_expired().await,
        1,
        "the sweep must expire exactly the unclaimed record"
    );
    assert!(!manager.has_pending_reconnection(&expired_player).await);
    assert!(
        manager.has_pending_reconnection(&claimed_player).await,
        "an admitted claim keeps its record alive mid-flight"
    );
    assert!(
        !manager.remove_expired_reconnection(&claimed_player).await,
        "the targeted removal must also refuse to drop a claimed record"
    );
    assert!(
        matches!(
            manager
                .claim_reconnection(&Uuid::new_v4(), &claimed_player, &room_id, &token)
                .await,
            Err(ReconnectionError::AlreadyInProgress)
        ),
        "the in-flight reservation still excludes duplicate winners"
    );

    // Once the winner gives up past the expired deadline, the released
    // record is ordinary expired residue: releasable-and-swept, never
    // resurrected with a fresh window.
    assert!(manager.release_reconnection_claim(&claim).await);
    assert_eq!(manager.cleanup_expired().await, 1);
    assert!(!manager.has_pending_reconnection(&claimed_player).await);
}

#[tokio::test]
async fn test_reconnection_claim_is_single_use_under_concurrency() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = Arc::new(ReconnectionManager::new(300, 100, metrics));
    let player_id = Uuid::new_v4();
    let room_id = Uuid::new_v4();
    let token = manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;
    let current_a = Uuid::new_v4();
    let current_b = Uuid::new_v4();

    let barrier = Arc::new(Barrier::new(2));
    let task_a = {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        let token = token.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            manager
                .claim_reconnection(&current_a, &player_id, &room_id, &token)
                .await
                .is_ok()
        })
    };
    let task_b = {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        let token = token.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            manager
                .claim_reconnection(&current_b, &player_id, &room_id, &token)
                .await
                .is_ok()
        })
    };

    let (claimed_a, claimed_b) = tokio::join!(task_a, task_b);
    let successes = [claimed_a.unwrap(), claimed_b.unwrap()]
        .into_iter()
        .filter(|claimed| *claimed)
        .count();
    assert_eq!(successes, 1, "exactly one same-token claim may succeed");
    assert!(
        manager.has_pending_reconnection(&player_id).await,
        "a claimed record remains pending until completed"
    );
    assert!(manager
        .claim_reconnection(&Uuid::new_v4(), &player_id, &room_id, &token)
        .await
        .is_err());
}

#[tokio::test]
async fn discard_pending_reconnection_never_removes_a_claimed_record() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let player_id = Uuid::new_v4();
    let room_id = Uuid::new_v4();
    let token = manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;
    let claim = manager
        .claim_reconnection(&Uuid::new_v4(), &player_id, &room_id, &token)
        .await
        .expect("claim succeeds");

    assert!(
        !manager.discard_pending_reconnection(&player_id).await,
        "a discard must not remove a record claimed by an in-flight reconnect"
    );
    assert!(
        manager.has_pending_reconnection(&player_id).await,
        "the claimed record must survive the discard attempt"
    );
    assert!(
        manager.complete_claimed_reconnection(&claim).await,
        "the in-flight transaction still owns and can complete the record"
    );
    assert!(
        !manager.has_pending_reconnection(&player_id).await,
        "completion removes the record the discard could not"
    );
}

/// A fresh same-room seat supersedes the pending record of the dropped
/// prior membership, other-room records keep their replacement semantics,
/// and a claimed record stays owned by its in-flight restore.
#[tokio::test]
async fn discard_pending_reconnection_in_room_is_same_room_and_unclaimed_only() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let player_id = Uuid::new_v4();
    let room_id = Uuid::new_v4();
    let other_room_id = Uuid::new_v4();

    // Other-room record: untouched by a same-room discard.
    manager
        .register_disconnection(player_id, other_room_id, false, None, 0)
        .await;
    assert!(
        !manager
            .discard_pending_reconnection_in_room(&player_id, &room_id)
            .await,
        "a record for another room must not be discarded"
    );
    assert!(manager.has_pending_reconnection(&player_id).await);
    assert!(
        manager
            .discard_pending_reconnection_in_room(&player_id, &other_room_id)
            .await,
        "the room-scoped discard removes its own room's record"
    );
    assert!(!manager.has_pending_reconnection(&player_id).await);

    // Same-room unclaimed record: discarded.
    manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;
    assert!(
        manager
            .discard_pending_reconnection_in_room(&player_id, &room_id)
            .await
    );
    assert!(!manager.has_pending_reconnection(&player_id).await);

    // Same-room claimed record: owned by its in-flight restore.
    let token = manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;
    let claim = manager
        .claim_reconnection(&Uuid::new_v4(), &player_id, &room_id, &token)
        .await
        .expect("claim succeeds");
    assert!(
        !manager
            .discard_pending_reconnection_in_room(&player_id, &room_id)
            .await,
        "a claimed record must not be discarded"
    );
    assert!(
        manager.has_pending_reconnection(&player_id).await,
        "the claimed record survives the discard attempt"
    );
    assert!(manager.complete_claimed_reconnection(&claim).await);
}

#[tokio::test]
async fn test_reconnection_claim_release_allows_retry() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let player_id = Uuid::new_v4();
    let room_id = Uuid::new_v4();
    let token = manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;

    let first_claim = manager
        .claim_reconnection(&Uuid::new_v4(), &player_id, &room_id, &token)
        .await
        .expect("first claim succeeds");
    assert!(manager
        .claim_reconnection(&Uuid::new_v4(), &player_id, &room_id, &token)
        .await
        .is_err());

    assert!(manager.release_reconnection_claim(&first_claim).await);
    let second_claim = manager
        .claim_reconnection(&Uuid::new_v4(), &player_id, &room_id, &token)
        .await
        .expect("released claim can be retried");
    assert!(manager.complete_claimed_reconnection(&second_claim).await);
    assert!(!manager.has_pending_reconnection(&player_id).await);
}

/// A released claim handle may outlive its restore attempt. If another
/// socket subsequently reserves the same record, the stale handle must be
/// unable to release or complete that newer reservation.
#[tokio::test]
async fn stale_reconnection_claim_handle_cannot_mutate_retry() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let player_id = Uuid::new_v4();
    let room_id = Uuid::new_v4();
    let token = manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;

    let stale_claim = manager
        .claim_reconnection(&Uuid::new_v4(), &player_id, &room_id, &token)
        .await
        .expect("first claim succeeds");
    assert!(manager.release_reconnection_claim(&stale_claim).await);

    let active_claim = manager
        .claim_reconnection(&Uuid::new_v4(), &player_id, &room_id, &token)
        .await
        .expect("released credential can be claimed again");

    assert!(
        !manager.release_reconnection_claim(&stale_claim).await,
        "a stale handle must not release the active retry"
    );
    assert!(
        !manager.complete_claimed_reconnection(&stale_claim).await,
        "a stale handle must not consume the active retry"
    );
    assert!(
        manager.has_pending_reconnection(&player_id).await,
        "rejecting stale mutations must preserve the active record"
    );

    assert!(manager.complete_claimed_reconnection(&active_claim).await);
    assert!(!manager.has_pending_reconnection(&player_id).await);
}

#[tokio::test(start_paused = true)]
async fn test_reconnection_cleanup_updates_active_session_gauge() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(1, 100, Arc::clone(&metrics));
    let player_id = Uuid::new_v4();
    let room_id = Uuid::new_v4();
    let _token = manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;
    tokio::time::advance(StdDuration::from_secs(5)).await;

    assert_eq!(
        metrics.reconnection_sessions_active.load(Ordering::Relaxed),
        1
    );
    assert_eq!(manager.cleanup_expired().await, 1);
    assert_eq!(
        metrics.reconnection_sessions_active.load(Ordering::Relaxed),
        0
    );
}

#[tokio::test]
async fn test_event_buffering() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let room_id = Uuid::new_v4();

    use crate::protocol::ServerMessage;

    // Buffer some events
    manager.buffer_event(&room_id, ServerMessage::Pong).await;
    manager.buffer_event(&room_id, ServerMessage::Pong).await;
    manager.buffer_event(&room_id, ServerMessage::Pong).await;

    // Get all events
    let missed = manager.get_missed_events(&room_id, 0).await;
    assert_eq!(missed.events.len(), 3);
    assert!(!missed.truncated, "nothing was evicted");
}

/// A room-uniform control event for the replay-path tests.
fn control_event() -> ServerMessage {
    ServerMessage::PlayerLeft {
        player_id: Uuid::new_v4(),
        epoch: None,
        final_seq: None,
    }
}

#[tokio::test]
async fn overflowed_ring_reports_truncation_and_counts_evictions() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 3, Arc::clone(&metrics));
    let player_id = Uuid::new_v4();
    let room_id = Uuid::new_v4();
    manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;

    // 5 events into a 3-slot ring: 2 evictions.
    for _ in 0..5 {
        manager.record_room_event(&room_id, &control_event()).await;
    }

    let missed = manager.get_missed_events(&room_id, 0).await;
    assert_eq!(missed.events.len(), 3, "the ring keeps only the newest 3");
    assert!(
        missed.truncated,
        "events the player needed were evicted, so the replay must report truncation"
    );
    assert_eq!(
        metrics.reconnection_events_evicted.load(Ordering::Relaxed),
        2,
        "each ring eviction advances the eviction metric"
    );
}

#[tokio::test]
async fn cross_room_sequence_gaps_are_not_truncation() {
    // Sequence numbers are GLOBAL across rooms, so another room's events
    // create benign gaps in this room's buffered sequence numbers. Only
    // the explicit eviction watermark may report truncation — this pins
    // why "gap in sequence numbers" is not a valid truncation predicate.
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, Arc::clone(&metrics));
    let room_a = Uuid::new_v4();
    let room_b = Uuid::new_v4();
    manager
        .register_disconnection(Uuid::new_v4(), room_a, false, None, 0)
        .await;
    manager
        .register_disconnection(Uuid::new_v4(), room_b, false, None, 0)
        .await;

    // Interleave: room B consumes global sequence numbers between room
    // A's events, so room A's buffered sequences are non-contiguous.
    manager.record_room_event(&room_a, &control_event()).await;
    manager.record_room_event(&room_b, &control_event()).await;
    manager.record_room_event(&room_b, &control_event()).await;
    manager.record_room_event(&room_a, &control_event()).await;

    let missed = manager.get_missed_events(&room_a, 0).await;
    assert_eq!(missed.events.len(), 2);
    assert!(
        !missed.truncated,
        "nothing was evicted from room A; room B's global sequence numbers must not mark it truncated"
    );
    assert_eq!(
        metrics.reconnection_events_evicted.load(Ordering::Relaxed),
        0
    );
}

#[tokio::test]
async fn record_room_event_buffers_only_while_a_reconnection_is_pending() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let player_id = Uuid::new_v4();
    let room_id = Uuid::new_v4();

    // No pending disconnection: the gate is closed, nothing buffers.
    manager.record_room_event(&room_id, &control_event()).await;
    assert!(
        manager
            .get_missed_events(&room_id, 0)
            .await
            .events
            .is_empty(),
        "a room with nobody pending must not accumulate events"
    );

    // Pending: the gate is open.
    manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;
    manager.record_room_event(&room_id, &control_event()).await;
    assert_eq!(manager.get_missed_events(&room_id, 0).await.events.len(), 1);

    // Last pending player completed: the buffer is released and the gate
    // closes again.
    manager.complete_reconnection(&player_id).await;
    assert!(
        !manager
            .replay_state
            .read()
            .await
            .event_buffers
            .contains_key(&room_id),
        "completing the last pending reconnection must release the room buffer"
    );
    manager.record_room_event(&room_id, &control_event()).await;
    assert!(manager
        .get_missed_events(&room_id, 0)
        .await
        .events
        .is_empty());
}

#[tokio::test]
async fn registration_and_first_room_event_are_one_ordered_replay_transition() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = Arc::new(ReconnectionManager::new(300, 100, metrics));
    let player_id = Uuid::new_v4();
    let room_id = Uuid::new_v4();
    manager.pre_issue_token(player_id, room_id).await;

    // Queue registration first and event capture second behind the one
    // ReplayState write lock. Tokio's fair write-lock queue makes this a
    // deterministic version of the old next-sequence/buffer-creation gap.
    let state_guard = manager.replay_state.write().await;
    let register_started = Arc::new(Notify::new());
    let registration = {
        let manager = Arc::clone(&manager);
        let started = Arc::clone(&register_started);
        tokio::spawn(async move {
            started.notify_one();
            manager
                .register_disconnection(player_id, room_id, false, None, 0)
                .await
        })
    };
    register_started.notified().await;
    tokio::task::yield_now().await;

    let event_started = Arc::new(Notify::new());
    let event = {
        let manager = Arc::clone(&manager);
        let started = Arc::clone(&event_started);
        tokio::spawn(async move {
            started.notify_one();
            manager.record_room_event(&room_id, &control_event()).await;
        })
    };
    event_started.notified().await;
    tokio::task::yield_now().await;
    drop(state_guard);

    registration
        .await
        .expect("registration task should not panic");
    event.await.expect("event task should not panic");
    let last_sequence = manager.replay_state.read().await.disconnected_players[&player_id]
        .disconnected
        .last_sequence;
    let missed = manager.get_missed_events(&room_id, last_sequence).await;
    assert_eq!(
        missed.events.len(),
        1,
        "an event ordered after registration must be included in replay"
    );
    assert!(!missed.truncated);
}

#[tokio::test(start_paused = true)]
async fn last_expiry_cleanup_cannot_delete_a_new_registration_gate() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = Arc::new(ReconnectionManager::new(1, 100, Arc::clone(&metrics)));
    let expired_player = Uuid::new_v4();
    let new_player = Uuid::new_v4();
    let room_id = Uuid::new_v4();
    manager
        .register_disconnection(expired_player, room_id, false, None, 0)
        .await;
    tokio::time::advance(StdDuration::from_secs(5)).await;

    // Hold the write lock so the spawned sweep parks on it and the new
    // registration can be ordered against it deterministically.
    let state_guard = manager.replay_state.write().await;

    let cleanup_started = Arc::new(Notify::new());
    let cleanup = {
        let manager = Arc::clone(&manager);
        let started = Arc::clone(&cleanup_started);
        tokio::spawn(async move {
            started.notify_one();
            manager.cleanup_expired().await
        })
    };
    cleanup_started.notified().await;
    tokio::task::yield_now().await;

    let registration_started = Arc::new(Notify::new());
    let registration = {
        let manager = Arc::clone(&manager);
        let started = Arc::clone(&registration_started);
        tokio::spawn(async move {
            started.notify_one();
            manager
                .register_disconnection(new_player, room_id, false, None, 0)
                .await
        })
    };
    registration_started.notified().await;
    tokio::task::yield_now().await;
    drop(state_guard);

    assert_eq!(cleanup.await.expect("cleanup task should not panic"), 1);
    registration
        .await
        .expect("registration task should not panic");
    manager.record_room_event(&room_id, &control_event()).await;
    let state = manager.replay_state.read().await;
    assert!(state.disconnected_players.contains_key(&new_player));
    assert_eq!(
        state.event_buffers[&room_id].events.len(),
        1,
        "new registration must retain a live replay gate after old cleanup"
    );
    assert_eq!(
        metrics.reconnection_sessions_active.load(Ordering::Relaxed),
        1,
        "active-session gauge must match the one surviving registration"
    );
}

/// Issue #136 (F4): the token surfaced at join is the SAME string the
/// disconnect arms — reusing it re-stamps only the expiry (window counted
/// from the disconnect, not the join), so pre-issuing never widens the
/// reconnect window.
#[tokio::test]
async fn pre_issued_token_is_reused_at_disconnect_with_restamped_expiry() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics.clone());
    let player = Uuid::new_v4();
    let room = Uuid::new_v4();

    let wire_token = manager.pre_issue_token(player, room).await;
    assert_eq!(
        metrics
            .reconnection_tokens_issued
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );

    let armed_token = manager
        .register_disconnection(player, room, false, None, 0)
        .await;
    assert_eq!(
        armed_token, wire_token,
        "the disconnect must arm the SAME token the client already holds"
    );
    // Reuse must not double-count the mint.
    assert_eq!(
        metrics
            .reconnection_tokens_issued
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    // A SECOND same-room disconnect while STILL PENDING (no reconnect in
    // between) must PRESERVE the client-held token: the first registration
    // already consumed the pre-issued entry, so re-minting here would
    // overwrite the record with a token the client never received (delivered
    // at join) and silently break its reconnect. No new mint is counted.
    let re_armed = manager
        .register_disconnection(player, room, false, None, 0)
        .await;
    assert_eq!(
        re_armed, wire_token,
        "a still-pending same-room re-registration keeps the client's token"
    );
    assert_eq!(
        metrics
            .reconnection_tokens_issued
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "preserving the token must not count a new mint"
    );
}

/// Regression: a same-room re-registration while still pending must keep the
/// client's pre-issued token CLAIMABLE. Before the fix, the second
/// registration found the pre-issued entry already consumed, minted a fresh
/// token, and overwrote the record — so a client reconnecting with the token
/// it received at join (the only token it ever held) was rejected.
#[tokio::test]
async fn same_room_reregistration_keeps_pre_issued_token_claimable() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let player = Uuid::new_v4();
    let room = Uuid::new_v4();

    let wire_token = manager.pre_issue_token(player, room).await;
    manager
        .register_disconnection(player, room, false, None, 0)
        .await;
    // A benign second registration for the same still-pending room (e.g. a
    // concurrent teardown path re-invoking the disconnect handler).
    manager
        .register_disconnection(player, room, false, None, 0)
        .await;

    // The client only ever held `wire_token`; it must still reconnect.
    let claim = manager
        .claim_reconnection(&Uuid::new_v4(), &player, &room, &wire_token)
        .await;
    assert!(
        claim.is_ok(),
        "the join-time token must stay claimable after re-registration: {claim:?}"
    );
}

/// An unrepresentable reconnection window saturates to a deadline beyond
/// any realistic horizon instead of inverting into an already-expired
/// instant (the same failure class `deadline::after` exists to prevent).
/// The saturated result must also exceed the retired fixed 100-year
/// fallback so this pin distinguishes the two saturation strategies.
#[test]
fn absurd_reconnect_windows_never_expire_immediately() {
    let now = Instant::now();
    let saturated = monotonic_deadline(now, u64::MAX);
    assert!(
        saturated > now,
        "an unrepresentable window is beyond the process lifetime, not elapsed"
    );
    assert!(
        saturated
            > now
                .checked_add(StdDuration::from_secs(150 * 365 * 24 * 60 * 60))
                .expect("150-year horizon is representable on every supported platform"),
        "saturation must reach past the retired 100-year fallback"
    );
    assert_eq!(
        monotonic_deadline(now, 300),
        now + StdDuration::from_secs(300),
        "representable windows keep their exact absolute instant"
    );
}

/// Every eligibility surface reads the same monotonic deadline, so all of
/// them are live at exactly the deadline and all of them are closed one
/// tick later. A record still claimable at its exact deadline must not be
/// nominated for cleanup or dropped from room-GC protection, and the
/// elapsed window must surface as `WindowExpired` — not as the
/// token's own wall-clock expiry, which lands at the same instant.
#[tokio::test(start_paused = true)]
async fn reconnect_eligibility_flips_once_at_the_monotonic_deadline() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let player_id = Uuid::new_v4();
    let room_id = Uuid::new_v4();
    let token = manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;

    for (label, step) in [
        (
            "one second before the deadline",
            StdDuration::from_secs(299),
        ),
        ("exactly at the deadline", StdDuration::from_secs(1)),
    ] {
        tokio::time::advance(step).await;
        assert!(
            manager
                .validate_reconnection(&player_id, &room_id, &token)
                .await
                .is_ok(),
            "{label}: validation must still admit the record"
        );
        assert!(
            manager
                .rooms_with_active_reconnections()
                .await
                .contains(&room_id),
            "{label}: room GC must still spare the room"
        );
        assert!(
            manager.expired_cleanup_candidates().await.is_empty(),
            "{label}: cleanup must not nominate the record"
        );
    }

    tokio::time::advance(StdDuration::from_millis(1)).await;
    assert_eq!(
        manager
            .validate_reconnection(&player_id, &room_id, &token)
            .await
            .expect_err("the window closed"),
        ReconnectionError::WindowExpired,
        "an elapsed window is RECONNECTION_EXPIRED, not a token rejection"
    );
    assert!(
        !manager
            .rooms_with_active_reconnections()
            .await
            .contains(&room_id),
        "past the deadline the room loses reconnection protection"
    );
    assert_eq!(
        manager.expired_cleanup_candidates().await,
        vec![(player_id, room_id)],
        "past the deadline the record is a cleanup candidate"
    );
    assert_eq!(manager.cleanup_expired().await, 1);
}

/// The deadline is monotonic, so no wall-clock adjustment can move it in
/// either direction. Both cases rewrite every UTC field the eligibility
/// decision used to read.
#[cfg(test)]
#[tokio::test(start_paused = true)]
async fn wall_clock_jumps_cannot_open_or_close_the_reconnect_window() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let player_id = Uuid::new_v4();
    let room_id = Uuid::new_v4();
    let token = manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;

    // A forward jump of a day ages both UTC captures far past the window.
    {
        let mut state = manager.replay_state.write().await;
        let disconnected = &mut state
            .disconnected_players
            .get_mut(&player_id)
            .expect("pending record")
            .disconnected;
        disconnected.disconnected_at = Utc::now() - Duration::days(1);
        disconnected.token.expires_at = Utc::now() - Duration::days(1);
    }
    assert!(
        manager
            .validate_reconnection(&player_id, &room_id, &token)
            .await
            .is_ok(),
        "a forward wall-clock jump must not close a live reconnect window"
    );
    assert!(
        manager.expired_cleanup_candidates().await.is_empty(),
        "a forward wall-clock jump must not make the record collectable"
    );

    // A backward jump cannot resurrect a genuinely elapsed window.
    tokio::time::advance(StdDuration::from_secs(301)).await;
    {
        let mut state = manager.replay_state.write().await;
        let disconnected = &mut state
            .disconnected_players
            .get_mut(&player_id)
            .expect("pending record")
            .disconnected;
        disconnected.disconnected_at = Utc::now() + Duration::days(1);
        disconnected.token.expires_at = Utc::now() + Duration::days(1);
    }
    assert_eq!(
        manager
            .validate_reconnection(&player_id, &room_id, &token)
            .await
            .expect_err("the monotonic window elapsed"),
        ReconnectionError::WindowExpired,
        "a backward wall-clock jump must not reopen an elapsed window"
    );
}

/// Registration only opens a NEW monotonic window for a genuine disconnect.
/// A duplicate same-room teardown carries the original deadline forward; a
/// different-room registration is a new disconnect and starts a fresh one.
#[tokio::test(start_paused = true)]
async fn only_a_genuine_disconnect_opens_a_new_monotonic_window() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let player_id = Uuid::new_v4();
    let room_id = Uuid::new_v4();
    let other_room = Uuid::new_v4();
    let token = manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;

    tokio::time::advance(StdDuration::from_secs(200)).await;
    let duplicate = manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;
    assert_eq!(duplicate, token, "a duplicate teardown reuses the token");

    tokio::time::advance(StdDuration::from_secs(100)).await;
    assert!(
        manager
            .validate_reconnection(&player_id, &room_id, &token)
            .await
            .is_ok(),
        "the original deadline is exactly here, so the record is still live"
    );
    tokio::time::advance(StdDuration::from_millis(1)).await;
    assert_eq!(
        manager
            .validate_reconnection(&player_id, &room_id, &token)
            .await
            .expect_err("the original window elapsed"),
        ReconnectionError::WindowExpired,
        "a duplicate teardown must not extend the original window"
    );

    let replacement = manager
        .register_disconnection(player_id, other_room, false, None, 0)
        .await;
    tokio::time::advance(StdDuration::from_secs(300)).await;
    assert!(
        manager
            .validate_reconnection(&player_id, &other_room, &replacement)
            .await
            .is_ok(),
        "a different-room registration is a genuine disconnect with a fresh window"
    );
    tokio::time::advance(StdDuration::from_millis(1)).await;
    assert_eq!(
        manager
            .validate_reconnection(&player_id, &other_room, &replacement)
            .await
            .expect_err("the replacement window elapsed"),
        ReconnectionError::WindowExpired,
        "the fresh window closes exactly one window after the replacement"
    );
}

/// A duplicate teardown is not a new disconnect and therefore cannot
/// extend either deadline exposed by the pending record.
#[cfg(test)]
#[tokio::test]
async fn same_room_reregistration_preserves_original_disconnect_deadline() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let player = Uuid::new_v4();
    let room = Uuid::new_v4();

    manager
        .register_disconnection(player, room, false, None, 0)
        .await;
    let (original_disconnected_at, original_token_expiry) = {
        let mut state = manager.replay_state.write().await;
        let disconnected = &mut state
            .disconnected_players
            .get_mut(&player)
            .expect("pending disconnect")
            .disconnected;
        disconnected.disconnected_at = Utc::now() - Duration::seconds(120);
        disconnected.token.expires_at = expiration_from_unsigned(disconnected.disconnected_at, 300);
        (disconnected.disconnected_at, disconnected.token.expires_at)
    };

    manager
        .register_disconnection(player, room, false, None, 0)
        .await;

    let state = manager.replay_state.read().await;
    let disconnected = &state.disconnected_players[&player].disconnected;
    assert_eq!(
        disconnected.disconnected_at, original_disconnected_at,
        "same-room re-registration must not restart the window clock"
    );
    assert_eq!(
        disconnected.token.expires_at, original_token_expiry,
        "same-room re-registration must not extend token expiry"
    );
}

/// A late teardown from the replaced socket can overlap a successful
/// reconnect after the claim is reserved and the next token is rotated.
/// It must be a no-op for both pieces of state: the active claim stays
/// single-owner, and the fresh token remains armed for the next genuine
/// disconnect.
#[tokio::test]
async fn same_room_reregistration_preserves_active_claim_and_rotated_token() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let player = Uuid::new_v4();
    let room = Uuid::new_v4();
    let first_token = manager.pre_issue_token(player, room).await;
    manager
        .register_disconnection(player, room, false, None, 0)
        .await;
    let claim = manager
        .claim_reconnection(&Uuid::new_v4(), &player, &room, &first_token)
        .await
        .expect("first socket reserves the reconnect record");
    let next_token = manager.pre_issue_token(player, room).await;

    manager
        .register_disconnection(player, room, false, None, 0)
        .await;

    assert!(matches!(
        manager
            .claim_reconnection(&Uuid::new_v4(), &player, &room, &first_token)
            .await,
        Err(ReconnectionError::AlreadyInProgress)
    ));
    assert!(
        manager.complete_claimed_reconnection(&claim).await,
        "duplicate teardown must not invalidate the active claim handle"
    );
    assert_eq!(
        manager
            .register_disconnection(player, room, false, None, 0)
            .await,
        next_token,
        "duplicate teardown must not consume the replacement connection's token"
    );
}

#[tokio::test]
async fn certificate_bound_claim_is_atomic_and_identity_preserving() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = Arc::new(ReconnectionManager::new(300, 100, metrics));
    let player = Uuid::new_v4();
    let room = Uuid::new_v4();
    let identity_a: Arc<str> = Arc::from("aa".repeat(32));
    let identity_b: Arc<str> = Arc::from("bb".repeat(32));

    let token = manager
        .pre_issue_token_with_identity(player, room, Some(Arc::clone(&identity_a)))
        .await;
    manager
        .register_disconnection_with_identity(
            player,
            room,
            false,
            None,
            0,
            Some(Arc::clone(&identity_a)),
        )
        .await;
    // A duplicate teardown cannot strip or rotate the original issuance
    // identity, even when its transient connection metadata differs.
    manager
        .register_disconnection_with_identity(
            player,
            room,
            false,
            None,
            0,
            Some(Arc::clone(&identity_b)),
        )
        .await;

    for rejected in [None, Some(identity_b.as_ref())] {
        let result = manager
            .claim_reconnection_with_identity(&Uuid::new_v4(), &player, &room, &token, rejected)
            .await;
        assert!(matches!(result, Err(ReconnectionError::TokenMismatch)));
    }

    assert!(matches!(
        manager.validate_reconnection(&player, &room, &token).await,
        Err(ReconnectionError::TokenMismatch)
    ));

    // Race a valid A claim with B. B can either lose to A's reservation or
    // check its identity first, but it can never reserve the credential;
    // A must still win exactly once.
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let spawn_claim = |identity: Arc<str>| {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        let token = token.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            manager
                .claim_reconnection_with_identity(
                    &Uuid::new_v4(),
                    &player,
                    &room,
                    &token,
                    Some(identity.as_ref()),
                )
                .await
        })
    };
    let claim_a = spawn_claim(Arc::clone(&identity_a));
    let claim_b = spawn_claim(Arc::clone(&identity_b));
    barrier.wait().await;
    let (claim_a, claim_b) = tokio::join!(claim_a, claim_b);
    let claim = claim_a
        .expect("A claim task")
        .expect("matching certificate identity claims the original token");
    assert!(matches!(
        claim_b.expect("B claim task"),
        Err(ReconnectionError::TokenMismatch | ReconnectionError::AlreadyInProgress)
    ));
    assert_eq!(claim.disconnected.player_id, player);
}

/// Regression: a same-room re-registration must NOT clobber the disconnect
/// snapshot captured at the FIRST disconnect. A racing second call with
/// `player_info: None` (and a stale `was_authority`) would otherwise erase
/// the membership snapshot — and `reconnection_service` rejects a reconnect
/// whose stored `player_info` is `None`.
#[cfg(test)]
#[tokio::test]
async fn same_room_reregistration_preserves_disconnect_snapshot() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let player = Uuid::new_v4();
    let room = Uuid::new_v4();
    let info = PlayerInfo {
        id: player,
        name: "Player".to_string(),
        is_authority: true,
        is_ready: false,
        connected_at: Some(Utc::now()),
        connection_info: Some(ConnectionInfo::Direct {
            host: "old-network.example".to_string(),
            port: 7777,
        }),
        epoch: None,
        seq: None,
        region_id: "test".to_string(),
    };

    let token = manager.pre_issue_token(player, room).await;
    // First disconnect captures the real snapshot: authority + membership.
    manager
        .register_disconnection(player, room, true, Some(info), 5)
        .await;
    // A racing second registration carries None + a stale authority flag.
    manager
        .register_disconnection(player, room, false, None, 0)
        .await;

    let claim = manager
        .claim_reconnection(&Uuid::new_v4(), &player, &room, &token)
        .await
        .expect("claim succeeds with the preserved token");
    let saved_info = claim
        .disconnected
        .player_info
        .as_ref()
        .expect("the first disconnect's player_info snapshot survives");
    assert_eq!(saved_info.name, "Player");
    assert!(
        saved_info.connection_info.is_none(),
        "the old socket's endpoint must not survive either registration"
    );
    assert!(
        claim.disconnected.was_authority,
        "the first disconnect's was_authority must survive re-registration"
    );
    assert_eq!(
        claim.disconnected.last_epoch, 5,
        "last_epoch preserved (max) across re-registration"
    );
}

/// Issue #525 sibling of the snapshot pin above: the authority-kick
/// tombstone is one of the preserved fields of a same-room
/// re-registration. A kick that landed on the pending record must keep
/// `kicked` across a duplicate teardown re-registration, or a later
/// reconnect claim would resurrect the removed seat from its membership
/// snapshot.
#[tokio::test]
async fn same_room_reregistration_preserves_the_kick_tombstone() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let player = Uuid::new_v4();
    let room = Uuid::new_v4();

    let token = manager
        .register_disconnection(player, room, false, None, 0)
        .await;
    assert!(
        manager.mark_reconnection_kicked(&player).await,
        "the kick must tombstone the pending record"
    );
    assert!(
        manager.is_reconnection_kicked(&player).await,
        "the tombstone must be observable before the re-registration"
    );

    // A duplicate teardown for the same still-pending room (e.g. a
    // racing second disconnect of the replaced socket) must not wipe
    // the tombstone.
    manager
        .register_disconnection(player, room, false, None, 0)
        .await;
    assert!(
        manager.is_reconnection_kicked(&player).await,
        "the kick tombstone must survive a same-room re-registration"
    );
    assert!(
        matches!(
            manager
                .claim_reconnection(&Uuid::new_v4(), &player, &room, &token)
                .await,
            Err(ReconnectionError::Kicked)
        ),
        "the preserved tombstone must refuse the later reconnect claim with Kicked"
    );
}

/// A pre-issued token bound to a DIFFERENT room is not reused (the player
/// joined elsewhere without a clean leave): the disconnect mints fresh.
#[tokio::test]
async fn pre_issued_token_for_another_room_is_not_reused() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let player = Uuid::new_v4();

    let stale = manager.pre_issue_token(player, Uuid::new_v4()).await;
    let armed = manager
        .register_disconnection(player, Uuid::new_v4(), false, None, 0)
        .await;
    assert_ne!(armed, stale, "a wrong-room pre-issue must not be armed");
}

/// Voluntary leave discards the pre-issued token: it never becomes
/// claimable, and the map stays bounded by currently-joined players.
#[tokio::test]
async fn discarded_pre_issued_token_is_not_reused() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let player = Uuid::new_v4();
    let room = Uuid::new_v4();

    let wire_token = manager.pre_issue_token(player, room).await;
    manager.discard_pre_issued(&player).await;
    let armed = manager
        .register_disconnection(player, room, false, None, 0)
        .await;
    assert_ne!(armed, wire_token, "a discarded token must never be armed");
}

/// Review-found (Bugbot): a second same-room disconnection while still
/// pending must NOT advance `last_sequence` — the player never reconnected,
/// so it still has not seen the events buffered since its first
/// disconnect. Advancing it would drop those events from a later replay
/// while `replay` still reported `complete`.
#[tokio::test]
async fn same_room_re_registration_preserves_the_original_replay_snapshot() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let player = Uuid::new_v4();
    let room = Uuid::new_v4();

    // First disconnect: snapshot taken here. Then a control event is
    // buffered (the player is pending and never sees it).
    manager
        .register_disconnection(player, room, false, None, 0)
        .await;
    manager.record_room_event(&room, &control_event()).await;

    // Second disconnect for the SAME room while still pending (e.g. a
    // replaced/racing connection): the snapshot must not jump past the
    // buffered event.
    manager
        .register_disconnection(player, room, false, None, 0)
        .await;
    manager.record_room_event(&room, &control_event()).await;

    // Both events must still replay — neither is silently excluded — and
    // the replay is honestly complete.
    let missed = manager.get_missed_events(&room, {
        let state = manager.replay_state.read().await;
        state.disconnected_players[&player]
            .disconnected
            .last_sequence
    });
    let missed = missed.await;
    assert_eq!(
        missed.events.len(),
        2,
        "both buffered control events must survive a same-room re-registration"
    );
    assert!(
        !missed.truncated,
        "nothing was evicted, so the replay is complete"
    );
}

/// Fuzz-found (fuzz_reconnect_tokens): re-registering a player from a NEW
/// room replaces its pending record, and if it was the old room's last
/// pending reconnector nothing else ever cleans that room's buffer — it
/// would keep capturing control events forever and replay ghosts to a
/// room with nobody pending.
#[tokio::test]
async fn re_registration_from_a_new_room_releases_the_orphaned_old_room_buffer() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let player = Uuid::new_v4();
    let other_player = Uuid::new_v4();
    let room_a = Uuid::new_v4();
    let room_b = Uuid::new_v4();

    // P pends in room A; the gate opens and captures events there.
    manager
        .register_disconnection(player, room_a, false, None, 0)
        .await;
    manager.record_room_event(&room_a, &control_event()).await;

    // P disconnects again from room B: the re-registration REPLACES its
    // record. Nobody pends in room A anymore, so its buffer must go.
    manager
        .register_disconnection(player, room_b, false, None, 0)
        .await;
    assert!(
        !manager
            .replay_state
            .read()
            .await
            .event_buffers
            .contains_key(&room_a),
        "re-registering the last pending player from a new room must \
         release the old room's buffer"
    );
    let missed = manager.get_missed_events(&room_a, 0).await;
    assert!(
        missed.events.is_empty() && !missed.truncated,
        "an orphaned room must replay nothing"
    );

    // But while ANOTHER player still pends in the old room, a sibling's
    // re-registration must leave that room's buffer untouched.
    manager
        .register_disconnection(other_player, room_a, false, None, 0)
        .await;
    manager.record_room_event(&room_a, &control_event()).await;
    manager
        .register_disconnection(player, room_a, false, None, 0)
        .await;
    manager
        .register_disconnection(player, room_b, false, None, 0)
        .await;
    assert!(
        manager
            .replay_state
            .read()
            .await
            .event_buffers
            .contains_key(&room_a),
        "the old room's buffer must survive while another player pends there"
    );
    assert_eq!(manager.get_missed_events(&room_a, 0).await.events.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn expired_cleanup_releases_the_room_buffer_when_last_pending_player_expires() {
    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(1, 100, metrics);
    let player_id = Uuid::new_v4();
    let room_id = Uuid::new_v4();
    manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;
    assert!(
        manager
            .replay_state
            .read()
            .await
            .event_buffers
            .contains_key(&room_id),
        "registering a disconnection opens the room's replay gate"
    );
    tokio::time::advance(StdDuration::from_secs(5)).await;

    assert_eq!(manager.cleanup_expired().await, 1);
    assert!(
        !manager
            .replay_state
            .read()
            .await
            .event_buffers
            .contains_key(&room_id),
        "expiring the last pending player must release the room buffer"
    );
    manager.record_room_event(&room_id, &control_event()).await;
    assert!(
        manager
            .get_missed_events(&room_id, 0)
            .await
            .events
            .is_empty(),
        "an expired-out room must stop capturing events"
    );
}

#[cfg(test)]
#[tokio::test]
async fn only_room_uniform_control_events_are_replayable() {
    use crate::protocol::{LobbyState, PlayerInfo, SpectatorInfo};

    let metrics = Arc::new(ServerMetrics::new());
    let manager = ReconnectionManager::new(300, 100, metrics);
    let room_id = Uuid::new_v4();
    let player_id = Uuid::new_v4();
    manager
        .register_disconnection(Uuid::new_v4(), room_id, false, None, 0)
        .await;

    let player_info = PlayerInfo {
        id: player_id,
        name: "Player".to_string(),
        is_authority: false,
        is_ready: false,
        connected_at: Some(Utc::now()),
        connection_info: None,
        epoch: None,
        seq: None,
        region_id: "test".to_string(),
    };
    let spectator = SpectatorInfo {
        id: player_id,
        name: "Spectator".to_string(),
        connected_at: Some(Utc::now()),
    };

    // Every room-uniform control event buffers.
    let replayable = [
        ServerMessage::PlayerJoined {
            player: player_info,
        },
        ServerMessage::PlayerLeft {
            player_id,
            epoch: None,
            final_seq: None,
        },
        ServerMessage::PlayerReconnected {
            player_id,
            epoch: None,
            player: None,
        },
        ServerMessage::NewSpectatorJoined {
            spectator: spectator.clone(),
            current_spectators: vec![spectator.clone()],
            spectator_count: None,
            reason: None,
        },
        ServerMessage::SpectatorDisconnected {
            spectator_id: player_id,
            reason: None,
            current_spectators: Vec::new(),
            spectator_count: None,
        },
        ServerMessage::LobbyStateChanged {
            lobby_state: LobbyState::Lobby,
            ready_players: Vec::new(),
            all_ready: false,
        },
        ServerMessage::AuthorityChanged {
            authority_player: Some(player_id),
            you_are_authority: false,
        },
    ];
    for message in &replayable {
        manager.record_room_event(&room_id, message).await;
    }
    assert_eq!(
        manager.get_missed_events(&room_id, 0).await.events.len(),
        replayable.len(),
        "every replayable control variant must buffer"
    );

    // High-rate data-path, per-recipient, and directed messages never do.
    let not_replayable = [
        ServerMessage::GameData {
            from_player: player_id,
            data: serde_json::json!({ "tick": 1 }),
            seq: None,
            epoch: None,
            class: None,
            key: None,
        },
        ServerMessage::GameStarting {
            peer_connections: Vec::new(),
        },
        ServerMessage::Signal {
            from: player_id,
            generation: uuid::Uuid::nil(),
            signal: serde_json::Value::Null,
        },
        ServerMessage::Pong,
        ServerMessage::Error {
            message: "directed".to_string(),
            error_code: None,
        },
    ];
    for message in &not_replayable {
        manager.record_room_event(&room_id, message).await;
    }
    assert_eq!(
        manager.get_missed_events(&room_id, 0).await.events.len(),
        replayable.len(),
        "GameData/GameStarting/Signal/directed messages must never buffer"
    );
}
