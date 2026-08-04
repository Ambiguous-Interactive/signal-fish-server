//! Nightly schedule-exploration stress for `coordination::deliver_or_disconnect`
//! and the connection close signal — the loom-substitute lane.
//!
//! The unit tests in `src/coordination/mod.rs` pin each interleaving class
//! deterministically under a paused clock; this binary complements them by
//! racing the SAME primitives thousands of times on a real multi-thread
//! runtime, with per-iteration phase offsets (derived from the iteration
//! index — no RNG) shifting when the receiver drops and when an explicit
//! close lands relative to the racing deliveries. True loom-style exhaustive
//! model checking would require instrumenting tokio's own primitives; this
//! lane instead samples real scheduler interleavings at volume and asserts
//! the invariants that must hold on EVERY schedule:
//!
//! - all tasks terminate within a deadline (no schedule can wedge delivery);
//! - exact conservation per iteration:
//!   `attempts == enqueued + channel_closed + dropped`, cross-checked against
//!   the per-task outcomes;
//! - at most one slow-consumer disconnect per connection, no matter how many
//!   deliveries time out against it;
//! - first-reason-wins: the close listener resolves to exactly the reason
//!   whose `request_close` call won the race.
//!
//! `#[ignore]`d: this is a minutes-scale stress executed by the nightly
//! verification lane (`.github/workflows/verification-nightly.yml`,
//! `--run-ignored all`), not by PR CI — a scheduling decision, not a
//! flakiness mask. The run is bounded both by iteration count and by a
//! wall-clock budget so a slow runner exits early rather than overrunning.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use signal_fish_server::coordination::{
    deliver_or_disconnect, ClientDeliveryHandle, CloseReason, ConnectionCloseSignal,
    DeliveryOutcome,
};
use signal_fish_server::metrics::ServerMetrics;
use signal_fish_server::protocol::{PlayerId, ServerMessage};

/// Upper bound on explored schedules; the wall-clock budget below usually
/// binds first on loaded runners.
const MAX_ITERATIONS: u64 = 3_000;

/// Wall-clock budget for the exploration loop (the whole test stays well
/// under the nightly job's timeout even on a starved runner).
const WALL_CLOCK_BUDGET: Duration = Duration::from_secs(45);

/// Racing deliveries per iteration.
const DELIVERY_TASKS: u64 = 3;

/// Slow-consumer grace per delivery: tiny but real time, so timeouts race
/// genuinely against the receiver drop and the explicit close.
const SLOW_CONSUMER_TIMEOUT: Duration = Duration::from_millis(2);

/// Per-iteration ceiling for every task to terminate: a generous bound that
/// only a genuinely wedged schedule can spend (the deliveries themselves are
/// bounded by `SLOW_CONSUMER_TIMEOUT`).
const ITERATION_JOIN_DEADLINE: Duration = Duration::from_secs(10);

fn test_player() -> PlayerId {
    PlayerId::from_u128(0xC0DE_D0C5_0000_0000_0000_0000_0000_0131)
}

fn test_message() -> Arc<ServerMessage> {
    Arc::new(ServerMessage::Pong)
}

/// Yield `phase` times: a deterministic, iteration-derived scheduling offset
/// (cooperative yields, not timers) that shifts this task's actions relative
/// to its racers so successive iterations sample different interleavings.
async fn phase_offset(phase: u64) {
    for _ in 0..phase {
        tokio::task::yield_now().await;
    }
}

/// One schedule sample: a fresh metrics registry, a 1-slot delivery queue
/// (prefilled on alternating iterations), `DELIVERY_TASKS` racing deliveries,
/// one task requesting an explicit close, and one task dropping the receiver
/// — all phase-shifted by the iteration index. Asserts every invariant that
/// must hold regardless of how the scheduler interleaved them.
async fn run_iteration(iteration: u64) {
    let metrics = Arc::new(ServerMetrics::new());
    let (sender, receiver) = tokio::sync::mpsc::channel::<Arc<ServerMessage>>(1);
    let (close, mut listener) = ConnectionCloseSignal::channel();

    // Alternate between an empty and an already-full queue so both the
    // try_send fast path and the backpressured send race the close/drop.
    if iteration.is_multiple_of(2) {
        sender
            .try_send(test_message())
            .expect("prefill the empty single-slot queue");
    }
    let handle = ClientDeliveryHandle::new(sender, close);

    let deliveries: Vec<_> = (0..DELIVERY_TASKS)
        .map(|task_index| {
            let metrics = Arc::clone(&metrics);
            let handle = handle.clone();
            let phase = (iteration + task_index) % 3;
            tokio::spawn(async move {
                phase_offset(phase).await;
                deliver_or_disconnect(
                    &metrics,
                    SLOW_CONSUMER_TIMEOUT,
                    &test_player(),
                    &handle,
                    test_message(),
                )
                .await
            })
        })
        .collect();

    let close_signal = handle.close.clone();
    let close_phase = (iteration / 5) % 7;
    let closer = tokio::spawn(async move {
        phase_offset(close_phase).await;
        close_signal.request_close(CloseReason::Unregistered)
    });

    let drop_phase = iteration % 5;
    let dropper = tokio::spawn(async move {
        phase_offset(drop_phase).await;
        drop(receiver);
    });

    // Invariant 1: every task terminates within the deadline on EVERY
    // schedule — a wedged delivery here is the bug this lane exists to find.
    let (outcomes, close_task_won) = tokio::time::timeout(ITERATION_JOIN_DEADLINE, async {
        let mut outcomes = Vec::with_capacity(DELIVERY_TASKS as usize);
        for delivery in deliveries {
            outcomes.push(delivery.await.expect("delivery task must not panic"));
        }
        let close_task_won = closer.await.expect("close task must not panic");
        dropper.await.expect("drop task must not panic");
        (outcomes, close_task_won)
    })
    .await
    .unwrap_or_else(|_elapsed| {
        panic!("iteration {iteration}: tasks did not terminate within the deadline")
    });

    let attempts = metrics.websocket_delivery_attempts.load(Ordering::Relaxed);
    let enqueued = metrics
        .websocket_deliveries_enqueued
        .load(Ordering::Relaxed);
    let channel_closed = metrics
        .websocket_deliveries_channel_closed
        .load(Ordering::Relaxed);
    let dropped = metrics.websocket_messages_dropped.load(Ordering::Relaxed);
    let disconnects = metrics
        .websocket_slow_consumer_disconnects
        .load(Ordering::Relaxed);

    // Invariant 2: exact conservation at unit scope, where these deliveries
    // are the only counter writers — every attempt resolved exactly once —
    // cross-checked against what each task actually reported.
    assert_eq!(
        attempts, DELIVERY_TASKS,
        "iteration {iteration}: every delivery must count exactly one attempt"
    );
    assert_eq!(
        attempts,
        enqueued + channel_closed + dropped,
        "iteration {iteration}: conservation violated: attempts={attempts} != \
         enqueued={enqueued} + channel_closed={channel_closed} + dropped={dropped}"
    );
    let outcome_count = |wanted: DeliveryOutcome| {
        outcomes
            .iter()
            .filter(|outcome| **outcome == wanted)
            .count() as u64
    };
    assert_eq!(
        outcome_count(DeliveryOutcome::Delivered),
        enqueued,
        "iteration {iteration}: Delivered outcomes must match the enqueued counter"
    );
    assert_eq!(
        outcome_count(DeliveryOutcome::ChannelClosed),
        channel_closed,
        "iteration {iteration}: ChannelClosed outcomes must match the channel-closed counter"
    );
    assert_eq!(
        outcome_count(DeliveryOutcome::SlowConsumer),
        dropped,
        "iteration {iteration}: SlowConsumer outcomes must match the dropped counter"
    );

    // Invariant 3: the disconnect metric counts CONNECTIONS, not delivery
    // attempts — never more than one for this single connection.
    assert!(
        disconnects <= 1,
        "iteration {iteration}: one connection produced {disconnects} slow-consumer disconnects"
    );

    // Invariant 4: first-reason-wins. The close task always requests a close,
    // so the listener always resolves; the winning reason must be exactly the
    // one whose `request_close` won. The only competing setter is a
    // timing-out delivery, which increments the disconnect metric if and only
    // if it initiated the close.
    let resolved_reason = tokio::time::timeout(ITERATION_JOIN_DEADLINE, listener.closed())
        .await
        .unwrap_or_else(|_elapsed| panic!("iteration {iteration}: close listener never resolved"));
    if close_task_won {
        assert_eq!(
            resolved_reason,
            Some(CloseReason::Unregistered),
            "iteration {iteration}: the close task won the race but the listener \
             observed a different reason"
        );
        assert_eq!(
            disconnects, 0,
            "iteration {iteration}: no delivery initiated the close, so no \
             slow-consumer disconnect may be counted"
        );
    } else {
        assert_eq!(
            resolved_reason,
            Some(CloseReason::SlowConsumer),
            "iteration {iteration}: a timing-out delivery won the race but the \
             listener observed a different reason"
        );
        assert_eq!(
            disconnects, 1,
            "iteration {iteration}: the delivery that won the close race must have \
             counted exactly one slow-consumer disconnect"
        );
    }
}

/// See the module docs: sample real-runtime schedules at volume and require
/// the delivery contract's invariants on every one of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nightly verification lane: minutes-scale schedule exploration (see module docs)"]
async fn racing_deliveries_close_and_receiver_drop_hold_all_invariants() {
    let started = std::time::Instant::now();
    let mut iterations = 0u64;
    while iterations < MAX_ITERATIONS && started.elapsed() < WALL_CLOCK_BUDGET {
        run_iteration(iterations).await;
        iterations += 1;
    }

    // The wall-clock budget must never squeeze the run into vacuity: even a
    // heavily loaded runner explores a meaningful sample.
    assert!(
        iterations >= 100,
        "only {iterations} iterations completed within {WALL_CLOCK_BUDGET:?} — \
         the stress run is too small to mean anything"
    );
    println!(
        "delivery concurrency stress: explored {iterations} schedule samples in {:?}",
        started.elapsed()
    );
}
