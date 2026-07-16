//! Model-based state-machine tests: real subsystems vs perfect-recall
//! reference models, driven by proptest op sequences on the STABLE toolchain
//! (so they run in normal CI and serve as mutation oracles alongside the
//! nightly coverage-guided fuzz targets in fuzz/).
//!
//! Two subsystems, matching two of the TLA+ models in formal/tla/:
//!
//! 1. The reconnection replay ring (`reconnection::EventBuffer` +
//!    `ReconnectionManager::get_missed_events`, src/reconnection.rs) against
//!    a Vec-based reference with perfect recall — the executable twin of
//!    `formal/tla/ReconnectReplay.tla`. The reference knows every event ever
//!    recorded and which were evicted, so the assertions kill watermark
//!    mutants directly: `events` must be exactly the retained-window tail
//!    after `last_sequence`, and `truncated` must hold exactly when the
//!    reference knows an EVICTED needed event (never for benign cross-room
//!    sequence gaps — global sequence numbers are shared across rooms).
//!
//! 2. The delivery contract (`coordination::deliver_or_disconnect` +
//!    `ConnectionCloseSignal`, src/coordination/mod.rs) on a REAL
//!    `ClientDeliveryHandle` against a reference conservation ledger — the
//!    executable twin of `formal/tla/DeliveryContract.tla`'s `Conservation`
//!    invariant. Every op sequence must resolve each delivery attempt as
//!    exactly one of enqueued / channel-closed / dropped, with the exact
//!    outcome, backpressure count, single per-connection disconnect, and
//!    first-close-reason-wins all matching the model.
//!
//! Async cases build a fresh current-thread runtime per proptest case with a
//! PAUSED clock (`start_paused`), so the slow-consumer grace window elapses
//! instantly and deterministically the moment a delivery parks on a full
//! queue with no reader — no wall-clock waiting, no scheduling luck.
//!
//! All proptest tests carry `#[cfg_attr(miri, ignore)]` per repository policy
//! (tests/ci_config_tests.rs::test_proptest_tests_ignored_under_miri; the
//! policy rationale is documented in tests/protocol_fuzz_hardening.rs).

use proptest::prelude::*;
use signal_fish_server::coordination::{
    deliver_or_disconnect, ClientDeliveryHandle, CloseReason, ConnectionCloseListener,
    ConnectionCloseSignal, DeliveryOutcome,
};
use signal_fish_server::metrics::ServerMetrics;
use signal_fish_server::protocol::ServerMessage;
use signal_fish_server::reconnection::{EventBuffer, ReconnectionManager};
#[cfg(feature = "trace-validation")]
use signal_fish_server::trace_validation::DeliveryTraceRecorder;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

async fn closed_with_timeout(
    listener: &mut ConnectionCloseListener,
    context: &str,
) -> Option<CloseReason> {
    tokio::time::timeout(Duration::from_secs(1), listener.closed())
        .await
        .unwrap_or_else(|_| panic!("{context}: close listener never resolved"))
}

// ---------------------------------------------------------------------------
// Part 1a — EventBuffer vs a perfect-recall reference (pure, no runtime)
// ---------------------------------------------------------------------------

/// Perfect-recall reference for one room's replay ring: remembers EVERY
/// recorded sequence in order; the retained window is the last `capacity`
/// entries, everything older was evicted.
struct ReferenceRing {
    capacity: usize,
    recorded: Vec<u64>,
}

impl ReferenceRing {
    fn retained(&self) -> &[u64] {
        let start = self.recorded.len().saturating_sub(self.capacity);
        &self.recorded[start..]
    }

    fn evicted(&self) -> &[u64] {
        let end = self.recorded.len().saturating_sub(self.capacity);
        &self.recorded[..end]
    }

    /// The spec-level truncation truth: an EVICTED event the player needed.
    fn needed_evicted(&self, last_sequence: u64) -> bool {
        self.evicted().iter().any(|&seq| seq > last_sequence)
    }

    /// The expected replay: retained events after `last_sequence`, in order.
    fn expected_replay(&self, last_sequence: u64) -> Vec<u64> {
        self.retained()
            .iter()
            .copied()
            .filter(|&seq| seq > last_sequence)
            .collect()
    }
}

/// A replayable control event whose identity encodes the sequence number the
/// test assigned, so returned messages can be matched back to the reference.
fn tagged_event(seq: u64) -> ServerMessage {
    ServerMessage::PlayerLeft {
        player_id: Uuid::from_u128(u128::from(seq)),
        epoch: None,
        final_seq: None,
    }
}

/// Extract the tag from a replayed event.
fn event_tag(message: &ServerMessage) -> u64 {
    match message {
        ServerMessage::PlayerLeft { player_id, .. } => player_id.as_u128() as u64,
        other => panic!("replay returned a message the test never recorded: {other:?}"),
    }
}

#[derive(Debug, Clone)]
enum RingOp {
    /// Record an event with the given (arbitrary, possibly non-contiguous)
    /// sequence number — non-contiguity models the cross-room global counter.
    Push { seq: u64 },
    /// Compare `get_events_after` + the watermark truncation predicate with
    /// the reference at an arbitrary cutoff.
    Query { last_sequence: u64 },
}

fn arb_ring_op() -> impl Strategy<Value = RingOp> {
    prop_oneof![
        3 => (0u64..64).prop_map(|seq| RingOp::Push { seq }),
        1 => (0u64..80).prop_map(|last_sequence| RingOp::Query { last_sequence }),
    ]
}

proptest! {
    /// EventBuffer == reference: replay contents, per-push eviction counts,
    /// the max-evicted watermark, and the two-sided truncation predicate all
    /// match perfect recall for every op sequence and ring capacity.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn event_buffer_matches_perfect_recall_reference(
        capacity in 1usize..5,
        ops in proptest::collection::vec(arb_ring_op(), 1..48),
    ) {
        let room_id = Uuid::from_u128(0xA11CE);
        let mut buffer = EventBuffer::new(room_id, capacity);
        let mut reference = ReferenceRing { capacity, recorded: Vec::new() };

        for op in ops {
            match op {
                RingOp::Push { seq } => {
                    let evicted_before = reference.evicted().len();
                    let evicted_now = buffer.push(tagged_event(seq), seq);
                    reference.recorded.push(seq);
                    prop_assert_eq!(
                        evicted_now,
                        reference.evicted().len() - evicted_before,
                        "push must report exactly the events it evicted"
                    );
                }
                RingOp::Query { last_sequence } => {
                    let replayed: Vec<u64> = buffer
                        .get_events_after(last_sequence)
                        .iter()
                        .map(event_tag)
                        .collect();
                    prop_assert_eq!(
                        replayed,
                        reference.expected_replay(last_sequence),
                        "replay must be exactly the retained tail after last_sequence"
                    );

                    // The production truncation predicate
                    // (ReconnectionManager::get_missed_events) over the
                    // buffer's watermark, checked two-sided against the
                    // spec-level truth. This kills watermark mutants: a
                    // last-instead-of-max watermark, a >=, or a
                    // gap-based predicate all diverge from the reference.
                    let truncated = buffer
                        .evicted_watermark
                        .is_some_and(|watermark| watermark > last_sequence);
                    prop_assert_eq!(
                        truncated,
                        reference.needed_evicted(last_sequence),
                        "truncated must hold exactly when a needed event was evicted"
                    );
                }
            }

            // Watermark structural invariant (the TLA+ WatermarkIsMaxEvicted
            // ghost coupling): the watermark is precisely the maximum evicted
            // sequence, None while nothing was evicted.
            prop_assert_eq!(
                buffer.evicted_watermark,
                reference.evicted().iter().max().copied(),
                "evicted_watermark must be the maximum evicted sequence"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Part 1b — ReconnectionManager end-to-end: two rooms, ONE global counter
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum ReplayOp {
    /// A replayable control event in the pending player's room (recorded).
    RecordRoomA,
    /// A replayable control event in the OTHER pending room: consumes a
    /// global sequence number, creating a benign gap in room A's sequences.
    RecordRoomB,
    /// get_missed_events for room A at an arbitrary last_sequence cutoff.
    Query { last_sequence: u64 },
}

fn arb_replay_op() -> impl Strategy<Value = ReplayOp> {
    prop_oneof![
        2 => Just(ReplayOp::RecordRoomA),
        2 => Just(ReplayOp::RecordRoomB),
        1 => (0u64..40).prop_map(|last_sequence| ReplayOp::Query { last_sequence }),
    ]
}

proptest! {
    /// The full production path (`record_room_event` + `get_missed_events`)
    /// matches perfect recall under interleaved rooms sharing the global
    /// sequence counter: cross-room gaps are never reported truncated, real
    /// ring evictions above the cutoff always are, and the replay is exactly
    /// the retained needed events. The executable ReconnectReplay.tla.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn reconnection_replay_matches_reference_across_interleaved_rooms(
        capacity in 1usize..4,
        ops in proptest::collection::vec(arb_replay_op(), 1..32),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime must build");
        runtime.block_on(async move {
            let manager =
                ReconnectionManager::new(3600, capacity, Arc::new(ServerMetrics::new()));
            let room_a = Uuid::from_u128(0xA001);
            let room_b = Uuid::from_u128(0xB002);

            // One pending player per room opens both replay gates, so BOTH
            // rooms' events consume global sequence numbers (the interleaving
            // that makes gap-based truncation predicates lie).
            manager
                .register_disconnection(Uuid::from_u128(1), room_a, false, None, 0)
                .await;
            manager
                .register_disconnection(Uuid::from_u128(2), room_b, false, None, 0)
                .await;

            let mut next_global_seq = 0u64;
            let mut reference = ReferenceRing { capacity, recorded: Vec::new() };

            for op in ops {
                match op {
                    ReplayOp::RecordRoomA => {
                        next_global_seq += 1;
                        manager
                            .record_room_event(&room_a, &tagged_event(next_global_seq))
                            .await;
                        reference.recorded.push(next_global_seq);
                    }
                    ReplayOp::RecordRoomB => {
                        next_global_seq += 1;
                        manager
                            .record_room_event(&room_b, &tagged_event(next_global_seq))
                            .await;
                    }
                    ReplayOp::Query { last_sequence } => {
                        let missed = manager.get_missed_events(&room_a, last_sequence).await;
                        let replayed: Vec<u64> =
                            missed.events.iter().map(event_tag).collect();
                        assert_eq!(
                            replayed,
                            reference.expected_replay(last_sequence),
                            "replay must be exactly room A's retained tail after last_sequence"
                        );
                        assert_eq!(
                            missed.truncated,
                            reference.needed_evicted(last_sequence),
                            "truncated must track room A's real evictions — never room B's \
                             interleaved global sequence numbers"
                        );
                    }
                }
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Part 2 — deliver_or_disconnect vs a reference conservation ledger
// ---------------------------------------------------------------------------

/// Slow-consumer grace used by the delivery model. Under a paused clock the
/// window elapses instantly and deterministically the moment a delivery is
/// parked on a full queue with no reader — and can never elapse while the
/// model is actively draining (drains are synchronous `try_recv`s).
const MODEL_TIMEOUT: Duration = Duration::from_secs(5);

/// Reference ledger mirroring DeliveryContract.tla's counters.
#[derive(Default)]
struct DeliveryModel {
    queue_len: usize,
    receiver_alive: bool,
    attempts: u64,
    enqueued: u64,
    channel_closed: u64,
    dropped: u64,
    backpressure_events: u64,
    expected_reason: Option<CloseReason>,
    slow_consumer_disconnects: u64,
}

#[derive(Debug, Clone)]
enum DeliveryOp {
    /// One deliver_or_disconnect call against the real handle.
    Deliver,
    /// The consumer drains one message (a healthy reader).
    Drain,
    /// The consumer's receiver is dropped (disconnect race).
    DropReceiver,
    /// A lifecycle eviction requests the close (first-reason-wins race
    /// against a later slow-consumer close).
    RequestLifecycleClose,
}

fn arb_delivery_op() -> impl Strategy<Value = DeliveryOp> {
    prop_oneof![
        4 => Just(DeliveryOp::Deliver),
        2 => Just(DeliveryOp::Drain),
        1 => Just(DeliveryOp::DropReceiver),
        1 => Just(DeliveryOp::RequestLifecycleClose),
    ]
}

proptest! {
    /// Every op sequence against a REAL ClientDeliveryHandle resolves exactly
    /// as the reference model predicts: per-call outcomes, the exact
    /// conservation law (attempts = enqueued + channel_closed + dropped at
    /// quiescence), one backpressure event per full-queue park, at most ONE
    /// slow-consumer disconnect per connection, and first-close-reason-wins.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn delivery_contract_matches_reference_ledger(
        capacity in 1usize..4,
        ops in proptest::collection::vec(arb_delivery_op(), 1..32),
    ) {
        // Paused clock: the slow-consumer window elapses deterministically
        // whenever a delivery parks with no reader (see MODEL_TIMEOUT).
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .expect("current-thread paused runtime must build");
        runtime.block_on(async move {
            let metrics = ServerMetrics::new();
            let player_id = Uuid::from_u128(0xDE11);
            let (tx, rx) = mpsc::channel::<Arc<ServerMessage>>(capacity);
            #[cfg(feature = "trace-validation")]
            let trace_path = std::env::var_os("SIGNAL_FISH_DELIVERY_TRACE_PATH");
            #[cfg(feature = "trace-validation")]
            let trace = if trace_path.is_some() {
                Some(Arc::new(
                    DeliveryTraceRecorder::new(
                        DeliveryTraceRecorder::next_trace_id("proptest"),
                        capacity,
                    )
                        .expect("generated trace header must be valid"),
                ))
            } else {
                None
            };
            #[cfg(feature = "trace-validation")]
            let (close, mut listener) = if let Some(trace) = &trace {
                ConnectionCloseSignal::channel_with_trace(Arc::clone(trace))
            } else {
                ConnectionCloseSignal::channel()
            };
            #[cfg(not(feature = "trace-validation"))]
            let (close, mut listener) = ConnectionCloseSignal::channel();
            let handle = ClientDeliveryHandle {
                sender: tx.into(),
                close,
            };
            let mut rx = Some(rx);
            let mut model = DeliveryModel {
                receiver_alive: true,
                ..DeliveryModel::default()
            };

            for op in ops {
                match op {
                    DeliveryOp::Deliver => {
                        // Predict the outcome from the model BEFORE the call.
                        let expected = if !model.receiver_alive {
                            DeliveryOutcome::ChannelClosed
                        } else if model.queue_len < capacity {
                            DeliveryOutcome::Delivered
                        } else {
                            DeliveryOutcome::SlowConsumer
                        };
                        model.attempts += 1;
                        match expected {
                            DeliveryOutcome::Delivered => {
                                model.enqueued += 1;
                                model.queue_len += 1;
                            }
                            DeliveryOutcome::ChannelClosed => model.channel_closed += 1,
                            DeliveryOutcome::SlowConsumer => {
                                model.backpressure_events += 1;
                                model.dropped += 1;
                                if model.expected_reason.is_none() {
                                    model.expected_reason = Some(CloseReason::SlowConsumer);
                                    model.slow_consumer_disconnects += 1;
                                }
                            }
                            DeliveryOutcome::AccountedDrop => {
                                unreachable!("legacy reliable model cannot accountably drop")
                            }
                            DeliveryOutcome::Canceled => {
                                unreachable!("legacy reliable model has no routing generations")
                            }
                        }

                        let outcome = deliver_or_disconnect(
                            &metrics,
                            MODEL_TIMEOUT,
                            &player_id,
                            &handle,
                            Arc::new(ServerMessage::Pong),
                        )
                        .await;
                        assert_eq!(
                            outcome, expected,
                            "delivery outcome diverged from the reference model"
                        );
                    }
                    DeliveryOp::Drain => {
                        #[cfg(feature = "trace-validation")]
                        if let Some(trace) = &trace {
                            // This property owns a raw mpsc receiver, not the
                            // production socket task. Preserve the real
                            // producer prefix and stop before the harness-only
                            // consumer mutation; real writer/finalizer hooks
                            // are exercised by the traced socket E2E case.
                            trace.stop_projection();
                        }
                        if let Some(rx) = rx.as_mut() {
                            match rx.try_recv() {
                                Ok(message) => {
                                    assert!(
                                        matches!(message.as_ref(), ServerMessage::Pong),
                                        "unexpected message on the recipient queue: {message:?}"
                                    );
                                    assert!(
                                        model.queue_len > 0,
                                        "the real queue held a message the model did not"
                                    );
                                    model.queue_len -= 1;
                                }
                                Err(mpsc::error::TryRecvError::Empty) => {
                                    assert_eq!(
                                        model.queue_len, 0,
                                        "the model held a message the real queue did not"
                                    );
                                }
                                Err(mpsc::error::TryRecvError::Disconnected) => {
                                    panic!(
                                        "the sender half is held for the whole case; \
                                         the channel can never disconnect under the receiver"
                                    );
                                }
                            }
                        }
                    }
                    DeliveryOp::DropReceiver => {
                        #[cfg(feature = "trace-validation")]
                        if let Some(trace) = &trace {
                            trace.stop_projection();
                        }
                        if rx.take().is_some() {
                            model.receiver_alive = false;
                            // Queued messages die with the receiver; they were
                            // already counted enqueued (matching the server,
                            // where the send task's finalize accounts them).
                            model.queue_len = 0;
                        }
                    }
                    DeliveryOp::RequestLifecycleClose => {
                        let initiated =
                            handle.close.request_close(CloseReason::Unregistered);
                        assert_eq!(
                            initiated,
                            model.expected_reason.is_none(),
                            "request_close must succeed exactly when no reason is set"
                        );
                        if model.expected_reason.is_none() {
                            model.expected_reason = Some(CloseReason::Unregistered);
                        }
                    }
                }
            }

            // Exact conservation + per-counter equality at quiescence: every
            // attempt resolved as exactly one of enqueued / channel-closed /
            // dropped (deliver_or_disconnect is the only writer here).
            let attempts = metrics.websocket_delivery_attempts.load(Ordering::Relaxed);
            let enqueued = metrics
                .websocket_deliveries_enqueued
                .load(Ordering::Relaxed);
            let channel_closed = metrics
                .websocket_deliveries_channel_closed
                .load(Ordering::Relaxed);
            let dropped = metrics.websocket_messages_dropped.load(Ordering::Relaxed);
            assert_eq!(attempts, model.attempts, "attempts diverged");
            assert_eq!(enqueued, model.enqueued, "enqueued diverged");
            assert_eq!(channel_closed, model.channel_closed, "channel_closed diverged");
            assert_eq!(dropped, model.dropped, "dropped diverged");
            assert_eq!(
                attempts,
                enqueued + channel_closed + dropped,
                "conservation violated at quiescence"
            );
            assert_eq!(
                metrics
                    .websocket_backpressure_events
                    .load(Ordering::Relaxed),
                model.backpressure_events,
                "backpressure events diverged"
            );
            assert_eq!(
                metrics
                    .websocket_slow_consumer_disconnects
                    .load(Ordering::Relaxed),
                model.slow_consumer_disconnects,
                "the disconnect metric must count the CONNECTION once, not per attempt"
            );

            // First reason wins: the listener observes exactly the first
            // requested reason (when any was requested).
            if let Some(expected_reason) = model.expected_reason {
                assert_eq!(
                    closed_with_timeout(&mut listener, "model delivery close").await,
                    Some(expected_reason),
                    "the close listener must observe the FIRST requested reason"
                );
            }

            #[cfg(feature = "trace-validation")]
            if let (Some(trace), Some(path)) = (&trace, trace_path) {
                if trace.has_delivery_attempts() {
                    trace
                        .append_jsonl(path)
                        .expect("append delivery-contract trace case");
                }
            }
        });
    }
}
