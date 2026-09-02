//! Deterministic event capture for delivery-contract trace refinement.
//!
//! The module exists only with the internal `trace-validation` feature. Merely
//! enabling that feature is inert: a test or diagnostic harness must explicitly
//! attach a
//! [`DeliveryTraceRecorder`](crate::trace_validation::DeliveryTraceRecorder)
//! to one connection. Keeping the recorder per connection avoids global-state
//! contamination when Rust tests run in parallel. The recorder's mutex gives
//! recorded events a total order; if a
//! dequeue overlaps the producer's post-send observation, the trace is marked
//! `Unsupported` instead of pretending that observation order was queue order.
//! The same fail-closed rule applies when a producer outcome overlaps the
//! finalizer's `QueueClose` observation, or when a `SendFull` outcome is
//! observed outside the projected full boundary (the filling enqueue's success
//! unprojected, or a later dequeue projected first). Any divergence label
//! also ends the projection: the recorder can no longer know the physical
//! queue state, so instead of evaluating guards against a provably wrong
//! projection (cascading imprecise labels onto innocent subsequent events),
//! it records every later observation verbatim — one precise divergence
//! label followed by an honest raw tail, with correlation retained so
//! physically enqueued items stay attributable.

use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

const TRACE_SCHEMA: &str = "signal-fish.delivery-contract/v1";

/// One transition understood by `formal/tla/DeliveryContractTrace.tla`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DeliveryTraceAction {
    SendFast,
    SendFull,
    ParkedEnqueue,
    GraceExpired,
    SendChannelClosed,
    ParkedChannelClosed,
    LifecycleClose,
    QueueClose,
    WriterStart,
    WriterDrain,
    CloseFlushStart,
    CloseFlushDrain,
    CloseFinish,
    /// The running implementation took a branch outside the pilot's declared
    /// reliable, single-FIFO abstraction. The generator rejects this action.
    Unsupported,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JsonlRecord<'a> {
    Header {
        schema: &'static str,
        trace_id: &'a str,
        queue_kind: &'static str,
        queue_capacity: usize,
    },
    Event {
        schema: &'static str,
        trace_id: &'a str,
        seq: u64,
        action: DeliveryTraceAction,
        #[serde(skip_serializing_if = "Option::is_none")]
        delivery_id: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<&'static str>,
    },
    Footer {
        schema: &'static str,
        trace_id: &'a str,
        event_count: u64,
    },
}

#[derive(Debug, Clone, Copy)]
struct DeliveryTraceEvent {
    seq: u64,
    action: DeliveryTraceAction,
    delivery_id: Option<u64>,
    detail: Option<&'static str>,
}

#[derive(Debug, Default)]
struct RecorderState {
    next_delivery_id: u64,
    events: Vec<DeliveryTraceEvent>,
    message_attempts: HashMap<usize, VecDeque<u64>>,
    attempt_messages: HashMap<u64, usize>,
    enqueued_attempts: HashMap<u64, bool>,
    recorded_queue: VecDeque<u64>,
    in_flight_write: Option<(u64, bool)>,
    close_requested: bool,
    slow_consumer_close: bool,
    queue_closed: bool,
    projection_closed: bool,
    projection_stopped: bool,
    projection_diverged: bool,
}

static NEXT_TRACE_ID: AtomicU64 = AtomicU64::new(1);
static TRACE_FILE_LOCK: Mutex<()> = Mutex::new(());

/// Per-connection recorder shared by delivery, writer, and teardown tasks.
#[derive(Debug)]
pub struct DeliveryTraceRecorder {
    trace_id: String,
    queue_capacity: usize,
    state: Mutex<RecorderState>,
}

impl DeliveryTraceRecorder {
    /// Create a recorder for one reliable outbound queue.
    ///
    /// `queue_capacity` must be the physical capacity used by the traced queue;
    /// zero can never represent a live Tokio channel and is rejected.
    pub fn new(trace_id: impl Into<String>, queue_capacity: usize) -> io::Result<Self> {
        if queue_capacity == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "delivery trace queue capacity must be nonzero",
            ));
        }
        let trace_id = trace_id.into();
        if trace_id.is_empty()
            || trace_id.len() > 128
            || !trace_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "delivery trace id must match [A-Za-z0-9._-]{1,128}",
            ));
        }
        Ok(Self {
            trace_id,
            queue_capacity,
            state: Mutex::new(RecorderState::default()),
        })
    }

    /// Allocate a process-unique, generator-safe trace identifier.
    pub fn next_trace_id(prefix: &str) -> String {
        let sequence = NEXT_TRACE_ID.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{}-{sequence}", std::process::id())
    }

    pub(crate) fn begin_delivery(&self, message: &crate::protocol::ServerMessage) -> u64 {
        let key = std::ptr::from_ref(message) as usize;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.next_delivery_id = state.next_delivery_id.saturating_add(1);
        let delivery_id = state.next_delivery_id;
        state
            .message_attempts
            .entry(key)
            .or_default()
            .push_back(delivery_id);
        state.attempt_messages.insert(delivery_id, key);
        delivery_id
    }

    /// Whether this trace observed at least one delivery attempt.
    pub fn has_delivery_attempts(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .events
            .iter()
            .any(|event| event.delivery_id.is_some())
    }

    pub(crate) fn record(
        &self,
        action: DeliveryTraceAction,
        delivery_id: Option<u64>,
        detail: Option<&'static str>,
    ) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.projection_stopped {
            return;
        }
        // A watch sender can still accept a reason after its queue receiver has
        // disappeared. Once the trace has observed CloseFinish, that late
        // lifecycle request cannot change the terminal delivery projection and
        // is intentionally omitted; disconnect-race delivery attempts remain
        // visible as SendChannelClosed/ParkedChannelClosed.
        if state.projection_closed && action == DeliveryTraceAction::LifecycleClose {
            return;
        }
        // After the first divergence label the projection is provably wrong,
        // so guard decisions would only cascade imprecise labels onto
        // innocent events. Record the raw observation verbatim instead.
        if state.projection_diverged {
            push_event(&mut state, action, delivery_id, detail);
            return;
        }
        // The receiver closes the physical channel before the socket task can
        // record QueueClose. Concurrent producers can therefore observe either
        // side of that boundary in the opposite order from this mutex's event
        // order. Every producer transition has a queueOpen guard in TLA+;
        // reject the overlap instead of turning scheduler timing into a false
        // replay divergence.
        let requires_open_queue = matches!(
            action,
            DeliveryTraceAction::SendFast
                | DeliveryTraceAction::SendFull
                | DeliveryTraceAction::ParkedEnqueue
                | DeliveryTraceAction::GraceExpired
        );
        let requires_closed_queue = matches!(
            action,
            DeliveryTraceAction::SendChannelClosed | DeliveryTraceAction::ParkedChannelClosed
        );
        if (state.queue_closed && requires_open_queue)
            || (!state.queue_closed && requires_closed_queue)
        {
            push_event(
                &mut state,
                DeliveryTraceAction::Unsupported,
                delivery_id,
                Some("queue-close-observation-order-race"),
            );
            // Retain correlation: the raced attempt's item may still be
            // physically written, and the diverged raw tail attributes it.
            state.projection_diverged = true;
            return;
        }
        // Tokio frees channel capacity in the receiver poll before the socket
        // task can call start_write. If a producer fills that physical slot
        // and records its success first, the projected queue is still full:
        // emitting SendFast/ParkedEnqueue would create a false TLC divergence.
        // Reject this observation-order overlap just like the inverse overlap
        // handled by start_write, rather than pretending either order is the
        // physical FIFO transition order.
        if matches!(
            action,
            DeliveryTraceAction::SendFast | DeliveryTraceAction::ParkedEnqueue
        ) && state.recorded_queue.len() >= self.queue_capacity
        {
            push_event(
                &mut state,
                DeliveryTraceAction::Unsupported,
                delivery_id,
                Some("enqueue-completed-before-dequeue-observed"),
            );
            // The item is physically in the queue; retain its correlation so
            // the diverged raw tail keeps its write attributed.
            state.projection_diverged = true;
            return;
        }
        // The inverse enqueue overlap: another producer's Full observation (or
        // a subsequent dequeue observation) can land before the physically
        // filling enqueue's success record does, leaving the projected queue
        // below capacity. Emitting SendFull would fail the model's
        // exact-full guard and read as an implementation occupancy bug
        // instead of an observation-order race, so label it like the
        // sibling overlaps above.
        if action == DeliveryTraceAction::SendFull
            && state.recorded_queue.len() < self.queue_capacity
        {
            push_event(
                &mut state,
                DeliveryTraceAction::Unsupported,
                delivery_id,
                Some("full-observation-order-race"),
            );
            // The parked producer retries with the same delivery id; retaining
            // correlation keeps that retry's write attributed on the diverged
            // raw tail instead of reading as untraced occupancy.
            state.projection_diverged = true;
            return;
        }
        push_event(&mut state, action, delivery_id, detail);
        match action {
            DeliveryTraceAction::SendFast | DeliveryTraceAction::ParkedEnqueue => {
                if let Some(delivery_id) = delivery_id {
                    state.enqueued_attempts.insert(delivery_id, true);
                    state.recorded_queue.push_back(delivery_id);
                }
            }
            DeliveryTraceAction::GraceExpired => {
                state.close_requested = true;
                // Grace expiration always abandons this delivery, but it only
                // makes the connection a slow-consumer close when this timeout
                // won the first-reason race. A prior lifecycle close retains
                // its healthy final-flush behavior in both Rust and TLA+.
                state.slow_consumer_close |= detail != Some("close-already-requested");
                if let Some(delivery_id) = delivery_id {
                    remove_attempt(&mut state, delivery_id);
                }
            }
            DeliveryTraceAction::SendChannelClosed
            | DeliveryTraceAction::ParkedChannelClosed
            | DeliveryTraceAction::Unsupported => {
                if let Some(delivery_id) = delivery_id {
                    remove_attempt(&mut state, delivery_id);
                }
            }
            DeliveryTraceAction::LifecycleClose => state.close_requested = true,
            _ => {}
        }
        if action == DeliveryTraceAction::CloseFinish {
            state.projection_closed = true;
        }
    }

    /// Record the dequeue of a projected delivery and return its correlation
    /// id. Untraced socket-internal/control traffic is intentionally ignored.
    pub(crate) fn start_write(
        &self,
        message: &crate::protocol::ServerMessage,
        close_flush: bool,
    ) -> Option<u64> {
        let key = std::ptr::from_ref(message) as usize;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.projection_stopped {
            return None;
        }
        if state.projection_diverged {
            // Raw-log tail: attribute the physical write by message identity
            // when the correlation survives, but evaluate no guards and
            // mutate no queue-membership state.
            let delivery_id = state
                .message_attempts
                .get_mut(&key)
                .and_then(VecDeque::pop_front);
            if let Some(delivery_id) = delivery_id {
                if state
                    .message_attempts
                    .get(&key)
                    .is_some_and(VecDeque::is_empty)
                {
                    state.message_attempts.remove(&key);
                }
                state.attempt_messages.remove(&delivery_id);
            }
            push_event(
                &mut state,
                if close_flush {
                    DeliveryTraceAction::CloseFlushStart
                } else {
                    DeliveryTraceAction::WriterStart
                },
                delivery_id,
                None,
            );
            return delivery_id;
        }
        let Some(delivery_id) = state
            .message_attempts
            .get_mut(&key)
            .and_then(VecDeque::pop_front)
        else {
            push_event(
                &mut state,
                DeliveryTraceAction::Unsupported,
                None,
                Some("untraced-v2-queue-item"),
            );
            // A physical queue item the projection never knew: subsequent
            // FIFO and fullness guards would misfire against it.
            state.projection_diverged = true;
            return None;
        };
        if state
            .message_attempts
            .get(&key)
            .is_some_and(VecDeque::is_empty)
        {
            state.message_attempts.remove(&key);
        }
        state.attempt_messages.remove(&delivery_id);
        if !state
            .enqueued_attempts
            .remove(&delivery_id)
            .unwrap_or(false)
        {
            push_event(
                &mut state,
                DeliveryTraceAction::Unsupported,
                Some(delivery_id),
                Some("overlapping-enqueue-dequeue"),
            );
            // The item's projected occupancy is now wrong in an unknown
            // direction; stop projecting instead of cascading.
            state.projection_diverged = true;
            return None;
        }
        if state.recorded_queue.front().copied() != Some(delivery_id) {
            push_event(
                &mut state,
                DeliveryTraceAction::Unsupported,
                Some(delivery_id),
                Some("enqueue-observation-order-diverged-from-fifo"),
            );
            state.projection_diverged = true;
            return None;
        }
        state.recorded_queue.pop_front();
        if (!close_flush && state.close_requested)
            || (close_flush
                && (!state.close_requested || !state.queue_closed || state.slow_consumer_close))
        {
            push_event(
                &mut state,
                DeliveryTraceAction::Unsupported,
                Some(delivery_id),
                Some(if close_flush {
                    "invalid-close-flush-phase"
                } else {
                    "live-write-after-close-request"
                }),
            );
            return None;
        }
        push_event(
            &mut state,
            if close_flush {
                DeliveryTraceAction::CloseFlushStart
            } else {
                DeliveryTraceAction::WriterStart
            },
            Some(delivery_id),
            None,
        );
        state.in_flight_write = Some((delivery_id, close_flush));
        Some(delivery_id)
    }

    pub(crate) fn finish_write(&self, delivery_id: u64, close_flush: bool) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.projection_stopped {
            return;
        }
        if state.projection_diverged {
            push_event(
                &mut state,
                if close_flush {
                    DeliveryTraceAction::CloseFlushDrain
                } else {
                    DeliveryTraceAction::WriterDrain
                },
                Some(delivery_id),
                None,
            );
            return;
        }
        if state.in_flight_write != Some((delivery_id, close_flush)) {
            push_event(
                &mut state,
                DeliveryTraceAction::Unsupported,
                Some(delivery_id),
                Some("write-finish-without-matching-start"),
            );
            return;
        }
        state.in_flight_write = None;
        push_event(
            &mut state,
            if close_flush {
                DeliveryTraceAction::CloseFlushDrain
            } else {
                DeliveryTraceAction::WriterDrain
            },
            Some(delivery_id),
            None,
        );
    }

    pub(crate) fn queue_closed(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.projection_stopped {
            return;
        }
        if state.projection_diverged {
            push_event(&mut state, DeliveryTraceAction::QueueClose, None, None);
            return;
        }
        if !state.close_requested {
            push_event(
                &mut state,
                DeliveryTraceAction::Unsupported,
                None,
                Some("queue-close-before-lifecycle-close"),
            );
            return;
        }
        push_event(&mut state, DeliveryTraceAction::QueueClose, None, None);
        state.queue_closed = true;
        let cancelled_live_write = match state.in_flight_write {
            Some((delivery_id, false)) => Some(delivery_id),
            Some((_, true)) | None => None,
        };
        if let Some(delivery_id) = cancelled_live_write {
            state.in_flight_write = None;
            // The send task's close-select cancelled the live socket-write
            // future after WriterStart. The base model has no transition for
            // a partially written frame, so reject this production schedule
            // before a CloseFlushStart could look like a replay divergence.
            push_event(
                &mut state,
                DeliveryTraceAction::Unsupported,
                Some(delivery_id),
                Some("live-write-cancelled-by-close"),
            );
        }
    }

    /// End a partial producer-only projection before a test harness mutates
    /// the receiver outside the production socket task.
    #[doc(hidden)]
    pub fn stop_projection(&self) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .projection_stopped = true;
    }

    /// Serialize one complete, self-describing JSONL trace.
    pub fn write_jsonl(&self, path: impl AsRef<Path>) -> io::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut writer = io::BufWriter::new(file);
        self.write_jsonl_to(&mut writer)?;
        writer.flush()
    }

    /// Append this trace to a multi-case JSONL corpus.
    pub fn append_jsonl(&self, path: impl AsRef<Path>) -> io::Result<()> {
        let _file_guard = TRACE_FILE_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let mut writer = io::BufWriter::new(file);
        self.write_jsonl_to(&mut writer)?;
        writer.flush()
    }

    /// Serialize to an arbitrary writer, primarily for focused unit tests.
    pub fn write_jsonl_to(&self, mut writer: impl Write) -> io::Result<()> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        write_record(
            &mut writer,
            &JsonlRecord::Header {
                schema: TRACE_SCHEMA,
                trace_id: &self.trace_id,
                queue_kind: "v2_legacy_reliable_fifo",
                queue_capacity: self.queue_capacity,
            },
        )?;
        for event in &state.events {
            write_record(
                &mut writer,
                &JsonlRecord::Event {
                    schema: TRACE_SCHEMA,
                    trace_id: &self.trace_id,
                    seq: event.seq,
                    action: event.action,
                    delivery_id: event.delivery_id,
                    detail: event.detail,
                },
            )?;
        }
        write_record(
            &mut writer,
            &JsonlRecord::Footer {
                schema: TRACE_SCHEMA,
                trace_id: &self.trace_id,
                event_count: state.events.len() as u64,
            },
        )
    }
}

fn push_event(
    state: &mut RecorderState,
    action: DeliveryTraceAction,
    delivery_id: Option<u64>,
    detail: Option<&'static str>,
) {
    let seq = (state.events.len() as u64).saturating_add(1);
    state.events.push(DeliveryTraceEvent {
        seq,
        action,
        delivery_id,
        detail,
    });
}

fn remove_attempt(state: &mut RecorderState, delivery_id: u64) {
    state.enqueued_attempts.remove(&delivery_id);
    let Some(key) = state.attempt_messages.remove(&delivery_id) else {
        return;
    };
    if let Some(attempts) = state.message_attempts.get_mut(&key) {
        attempts.retain(|candidate| *candidate != delivery_id);
        if attempts.is_empty() {
            state.message_attempts.remove(&key);
        }
    }
}

fn write_record(writer: &mut impl Write, record: &JsonlRecord<'_>) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, record).map_err(io::Error::other)?;
    writer.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_emits_ordered_self_describing_jsonl() {
        let recorder = DeliveryTraceRecorder::new("case-1", 2).expect("valid recorder");
        let message = std::sync::Arc::new(crate::protocol::ServerMessage::Pong);
        let delivery = recorder.begin_delivery(&message);
        recorder.record(DeliveryTraceAction::SendFast, Some(delivery), None);
        let write = recorder
            .start_write(&message, false)
            .expect("correlated traced write");
        recorder.finish_write(write, false);

        let mut bytes = Vec::new();
        recorder
            .write_jsonl_to(&mut bytes)
            .expect("serialize trace");
        let records = String::from_utf8(bytes).expect("JSONL is UTF-8");
        let lines: Vec<serde_json::Value> = records
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid JSON record"))
            .collect();

        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0]["kind"], "header");
        assert_eq!(lines[0]["queue_capacity"], 2);
        assert_eq!(lines[1]["action"], "SendFast");
        assert_eq!(lines[1]["delivery_id"], 1);
        assert_eq!(lines[2]["seq"], 2);
        assert_eq!(lines[4]["kind"], "footer");
        assert_eq!(lines[4]["event_count"], 3);
    }

    #[test]
    fn recorder_rejects_overlapping_enqueue_dequeue_instead_of_misordering() {
        let recorder = DeliveryTraceRecorder::new("race", 1).expect("valid recorder");
        let message = std::sync::Arc::new(crate::protocol::ServerMessage::Pong);
        let delivery = recorder.begin_delivery(&message);
        assert_eq!(recorder.start_write(&message, false), None);
        recorder.record(DeliveryTraceAction::SendFast, Some(delivery), None);

        let mut bytes = Vec::new();
        recorder
            .write_jsonl_to(&mut bytes)
            .expect("serialize trace");
        let output = String::from_utf8(bytes).expect("UTF-8 trace");
        assert!(output.contains("overlapping-enqueue-dequeue"));
        assert!(!output.contains("WriterStart"));
    }

    #[test]
    fn recorder_rejects_enqueue_observed_before_capacity_freeing_dequeue() {
        for parked in [false, true] {
            let recorder =
                DeliveryTraceRecorder::new(if parked { "late-parked" } else { "late-fast" }, 1)
                    .expect("valid recorder");
            let first = std::sync::Arc::new(crate::protocol::ServerMessage::Pong);
            let first_delivery = recorder.begin_delivery(&first);
            recorder.record(DeliveryTraceAction::SendFast, Some(first_delivery), None);

            let second = std::sync::Arc::new(crate::protocol::ServerMessage::Pong);
            let second_delivery = recorder.begin_delivery(&second);
            if parked {
                recorder.record(DeliveryTraceAction::SendFull, Some(second_delivery), None);
            }
            recorder.record(
                if parked {
                    DeliveryTraceAction::ParkedEnqueue
                } else {
                    DeliveryTraceAction::SendFast
                },
                Some(second_delivery),
                None,
            );

            let mut bytes = Vec::new();
            recorder
                .write_jsonl_to(&mut bytes)
                .expect("serialize trace");
            let output = String::from_utf8(bytes).expect("UTF-8 trace");
            assert!(output.contains("enqueue-completed-before-dequeue-observed"));
            assert_eq!(output.matches("\"action\":\"SendFast\"").count(), 1);
            assert!(!output.contains("ParkedEnqueue"));
        }
    }

    #[test]
    fn recorder_labels_full_observed_outside_the_projected_full_boundary() {
        // The inverse enqueue overlap: the physically-filling enqueue's
        // success record lands after another producer's Full observation, or
        // after a subsequent dequeue observation. The projected queue is
        // below capacity when the Full is recorded, so emitting SendFull
        // would fail the model's exact-full guard and read as an
        // implementation occupancy bug instead of an observation-order race.
        for (trace_id, dequeue_observed_first) in [
            ("late-full-before-enqueue", false),
            ("late-full-after-dequeue", true),
        ] {
            let recorder = DeliveryTraceRecorder::new(trace_id, 1).expect("valid recorder");
            let first = std::sync::Arc::new(crate::protocol::ServerMessage::Pong);
            let filling = recorder.begin_delivery(&first);
            let second = std::sync::Arc::new(crate::protocol::ServerMessage::Pong);
            let full = recorder.begin_delivery(&second);
            if dequeue_observed_first {
                recorder.record(DeliveryTraceAction::SendFast, Some(filling), None);
                assert_eq!(recorder.start_write(&first, false), Some(filling));
            }
            recorder.record(DeliveryTraceAction::SendFull, Some(full), None);
            if !dequeue_observed_first {
                recorder.record(DeliveryTraceAction::SendFast, Some(filling), None);
            }

            let mut bytes = Vec::new();
            recorder
                .write_jsonl_to(&mut bytes)
                .expect("serialize trace");
            let output = String::from_utf8(bytes).expect("UTF-8 trace");
            assert!(
                output.contains("full-observation-order-race"),
                "{trace_id}: unlabeled SendFull divergence: {output}"
            );
            assert_eq!(
                output.matches("\"action\":\"SendFull\"").count(),
                0,
                "{trace_id}: SendFull must not be emitted outside the projected full boundary"
            );
        }
    }

    #[test]
    fn divergence_label_ends_projection_instead_of_cascading_ghost_labels() {
        // The parked producer's retry after a full-observation-order-race:
        // stripping the raced attempt's correlation used to let the verbatim
        // ParkedEnqueue push an uncorrelated ghost into the projected queue,
        // so the writer's dequeue of it was mislabeled untraced and the next
        // innocent write cascaded a FIFO-divergence label. One precise label
        // plus a verbatim attributed tail is the diagnostic contract.
        let recorder = DeliveryTraceRecorder::new("diverged-full-race", 1).expect("valid recorder");
        let first = std::sync::Arc::new(crate::protocol::ServerMessage::Pong);
        let filling = recorder.begin_delivery(&first);
        recorder.record(DeliveryTraceAction::SendFast, Some(filling), None);
        assert_eq!(recorder.start_write(&first, false), Some(filling));
        recorder.finish_write(filling, false);

        let parked = std::sync::Arc::new(crate::protocol::ServerMessage::Pong);
        let parked_delivery = recorder.begin_delivery(&parked);
        recorder.record(DeliveryTraceAction::SendFull, Some(parked_delivery), None);
        recorder.record(
            DeliveryTraceAction::ParkedEnqueue,
            Some(parked_delivery),
            None,
        );
        assert_eq!(recorder.start_write(&parked, false), Some(parked_delivery));
        recorder.finish_write(parked_delivery, false);

        let third = std::sync::Arc::new(crate::protocol::ServerMessage::Pong);
        let third_delivery = recorder.begin_delivery(&third);
        recorder.record(DeliveryTraceAction::SendFast, Some(third_delivery), None);
        assert_eq!(recorder.start_write(&third, false), Some(third_delivery));
        recorder.finish_write(third_delivery, false);

        let mut bytes = Vec::new();
        recorder
            .write_jsonl_to(&mut bytes)
            .expect("serialize trace");
        let output = String::from_utf8(bytes).expect("UTF-8 trace");
        assert!(
            output.contains("full-observation-order-race"),
            "the race must keep its precise label: {output}"
        );
        assert_eq!(
            output.matches("\"action\":\"Unsupported\"").count(),
            1,
            "exactly one divergence label; no cascading imprecise labels: {output}"
        );
        assert_eq!(
            output.matches("\"action\":\"ParkedEnqueue\"").count(),
            1,
            "the parked retry stays visible verbatim: {output}"
        );
        for ghost_detail in [
            "untraced-v2-queue-item",
            "enqueue-observation-order-diverged-from-fifo",
            "enqueue-completed-before-dequeue-observed",
        ] {
            assert!(
                !output.contains(ghost_detail),
                "{ghost_detail} must not cascade onto the post-divergence tail: {output}"
            );
        }
        assert_eq!(
            output.matches("\"action\":\"WriterStart\"").count(),
            3,
            "every physical write stays attributed after divergence: {output}"
        );
    }

    #[test]
    fn late_enqueue_survives_divergence_with_correlation_intact() {
        // The enqueue-completed-before-dequeue-observed window: stripping the
        // physically-enqueued item's correlation used to make its writer
        // dequeue read as untraced hidden occupancy. Retaining correlation
        // keeps the write attributed on the diverged raw tail.
        let recorder =
            DeliveryTraceRecorder::new("diverged-late-enqueue", 1).expect("valid recorder");
        let first = std::sync::Arc::new(crate::protocol::ServerMessage::Pong);
        let first_delivery = recorder.begin_delivery(&first);
        recorder.record(DeliveryTraceAction::SendFast, Some(first_delivery), None);

        let late = std::sync::Arc::new(crate::protocol::ServerMessage::Pong);
        let late_delivery = recorder.begin_delivery(&late);
        recorder.record(DeliveryTraceAction::SendFast, Some(late_delivery), None);

        assert_eq!(recorder.start_write(&first, false), Some(first_delivery));
        recorder.finish_write(first_delivery, false);
        assert_eq!(recorder.start_write(&late, false), Some(late_delivery));
        recorder.finish_write(late_delivery, false);

        let state = recorder
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(
            state.recorded_queue.front().copied(),
            Some(first_delivery),
            "the diverged projection must not claim the late enqueue's occupancy"
        );
        let unsupported = state
            .events
            .iter()
            .filter(|event| event.action == DeliveryTraceAction::Unsupported)
            .count();
        assert_eq!(unsupported, 1, "exactly one divergence label");
        assert!(
            state
                .events
                .iter()
                .any(|event| event.action == DeliveryTraceAction::WriterStart
                    && event.delivery_id == Some(late_delivery)),
            "the late enqueue's physical write must stay attributed"
        );
    }

    #[test]
    fn recorder_rejects_open_queue_outcomes_observed_after_queue_close() {
        for action in [
            DeliveryTraceAction::SendFast,
            DeliveryTraceAction::SendFull,
            DeliveryTraceAction::ParkedEnqueue,
            DeliveryTraceAction::GraceExpired,
        ] {
            let recorder =
                DeliveryTraceRecorder::new("late-open-outcome", 1).expect("valid recorder");
            let message = std::sync::Arc::new(crate::protocol::ServerMessage::Pong);
            let filler = recorder.begin_delivery(&message);
            recorder.record(DeliveryTraceAction::SendFast, Some(filler), None);
            let delivery = recorder.begin_delivery(&message);
            if matches!(
                action,
                DeliveryTraceAction::ParkedEnqueue | DeliveryTraceAction::GraceExpired
            ) {
                // Stage the parked sender at the projected full boundary so
                // the parking SendFull itself stays replayable.
                recorder.record(DeliveryTraceAction::SendFull, Some(delivery), None);
            }
            recorder.record(DeliveryTraceAction::LifecycleClose, None, None);
            recorder.queue_closed();
            recorder.record(action, Some(delivery), None);

            let state = recorder
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let last = state.events.last().expect("race marker");
            assert_eq!(last.action, DeliveryTraceAction::Unsupported);
            assert_eq!(last.detail, Some("queue-close-observation-order-race"));
            // The race label ends the projection and retains correlation, so
            // the diverged raw tail can still attribute the attempt's write
            // instead of mislabeling it untraced.
            assert!(state.attempt_messages.contains_key(&delivery));
        }
    }

    #[test]
    fn recorder_rejects_closed_queue_outcomes_observed_before_queue_close() {
        for action in [
            DeliveryTraceAction::SendChannelClosed,
            DeliveryTraceAction::ParkedChannelClosed,
        ] {
            let recorder =
                DeliveryTraceRecorder::new("early-closed-outcome", 1).expect("valid recorder");
            let message = std::sync::Arc::new(crate::protocol::ServerMessage::Pong);
            let filler = recorder.begin_delivery(&message);
            recorder.record(DeliveryTraceAction::SendFast, Some(filler), None);
            let delivery = recorder.begin_delivery(&message);
            if action == DeliveryTraceAction::ParkedChannelClosed {
                // Stage the parked sender at the projected full boundary so
                // the parking SendFull itself stays replayable.
                recorder.record(DeliveryTraceAction::SendFull, Some(delivery), None);
            }
            recorder.record(action, Some(delivery), None);

            let state = recorder
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let last = state.events.last().expect("race marker");
            assert_eq!(last.action, DeliveryTraceAction::Unsupported);
            assert_eq!(last.detail, Some("queue-close-observation-order-race"));
            // Race labels retain correlation for the diverged raw tail.
            assert!(state.attempt_messages.contains_key(&delivery));
        }
    }

    #[test]
    fn recorder_rejects_live_write_cancelled_by_close_before_final_flush() {
        let recorder =
            DeliveryTraceRecorder::new("cancelled-live-write", 2).expect("valid recorder");
        let first = std::sync::Arc::new(crate::protocol::ServerMessage::Pong);
        let first_delivery = recorder.begin_delivery(&first);
        recorder.record(DeliveryTraceAction::SendFast, Some(first_delivery), None);
        let second = std::sync::Arc::new(crate::protocol::ServerMessage::Pong);
        let second_delivery = recorder.begin_delivery(&second);
        recorder.record(DeliveryTraceAction::SendFast, Some(second_delivery), None);

        assert_eq!(recorder.start_write(&first, false), Some(first_delivery));
        recorder.record(DeliveryTraceAction::LifecycleClose, None, None);
        recorder.queue_closed();
        assert_eq!(
            recorder.start_write(&second, true),
            Some(second_delivery),
            "the recorder may continue collecting context after failing closed"
        );

        let state = recorder
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let unsupported = state
            .events
            .iter()
            .find(|event| event.action == DeliveryTraceAction::Unsupported)
            .expect("cancelled live write must fail closed");
        assert_eq!(unsupported.delivery_id, Some(first_delivery));
        assert_eq!(unsupported.detail, Some("live-write-cancelled-by-close"));
        assert!(
            state
                .events
                .iter()
                .position(|event| event.action == DeliveryTraceAction::Unsupported)
                < state
                    .events
                    .iter()
                    .position(|event| event.action == DeliveryTraceAction::CloseFlushStart),
            "unsupported cancellation must precede final-flush replay events"
        );
    }

    #[test]
    fn recorder_accepts_live_write_drained_before_final_flush() {
        let recorder = DeliveryTraceRecorder::new("drained-live-write", 2).expect("valid recorder");
        let first = std::sync::Arc::new(crate::protocol::ServerMessage::Pong);
        let first_delivery = recorder.begin_delivery(&first);
        recorder.record(DeliveryTraceAction::SendFast, Some(first_delivery), None);
        let second = std::sync::Arc::new(crate::protocol::ServerMessage::Pong);
        let second_delivery = recorder.begin_delivery(&second);
        recorder.record(DeliveryTraceAction::SendFast, Some(second_delivery), None);

        let live_write = recorder
            .start_write(&first, false)
            .expect("live write starts");
        recorder.finish_write(live_write, false);
        recorder.record(DeliveryTraceAction::LifecycleClose, None, None);
        recorder.queue_closed();
        let close_write = recorder
            .start_write(&second, true)
            .expect("close flush starts");
        recorder.finish_write(close_write, true);

        let state = recorder
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert!(
            state
                .events
                .iter()
                .all(|event| event.action != DeliveryTraceAction::Unsupported),
            "a completed live write must remain replayable across lifecycle close"
        );
    }

    #[test]
    fn recorder_rejects_hidden_v2_queue_occupancy() {
        let recorder = DeliveryTraceRecorder::new("hidden", 2).expect("valid recorder");
        let untraced = std::sync::Arc::new(crate::protocol::ServerMessage::Pong);
        assert_eq!(recorder.start_write(&untraced, false), None);

        let mut bytes = Vec::new();
        recorder
            .write_jsonl_to(&mut bytes)
            .expect("serialize trace");
        let output = String::from_utf8(bytes).expect("UTF-8 trace");
        assert!(output.contains("untraced-v2-queue-item"));
    }

    #[test]
    fn recorder_rejects_invalid_headers() {
        for (trace_id, capacity, expected) in [
            ("", 1, "trace id"),
            ("unsafe id", 1, "trace id"),
            ("valid", 0, "queue capacity"),
        ] {
            let error = DeliveryTraceRecorder::new(trace_id, capacity)
                .expect_err("invalid recorder must be rejected");
            assert!(
                error.to_string().contains(expected),
                "unexpected error for trace_id={trace_id:?}, capacity={capacity}: {error}"
            );
        }
    }
}
