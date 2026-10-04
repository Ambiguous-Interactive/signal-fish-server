//! Raw run events — the artifact-level truth a run replays from.
//!
//! Everything the oracle consumes is recorded as three append-only event
//! lists (sends, receipts, disconnects) plus join failures. The summary is a
//! pure function of these events plus the plans, so "replay the artifacts"
//! and "summarize the run" are literally the same code path.

use std::sync::Mutex;

use crate::oracle::InvalidReason;
use crate::schedule::Phase;

/// One send a sender task actually completed (it left this process).
///
/// `intended_us` vs `sent_us` is the scheduled-send lag: a pause must appear
/// here (generator-side), never as reduced offered load.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SentEvent {
    pub sender: String,
    pub room: u32,
    pub seq: u64,
    pub intended_us: u64,
    pub sent_us: u64,
    pub phase: Phase,
}

/// One delivery a recipient task actually read off the socket.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReceiptEvent {
    pub recipient: String,
    pub sender: String,
    pub seq: u64,
    pub received_us: u64,
}

/// Why a recipient's stream ended before quiescence.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DisconnectObservation {
    /// The server sent a WebSocket close frame (code recorded when present).
    ServerClosed(Option<u16>),
    /// The socket errored or ended without a close frame.
    StreamEnded,
}

/// A recipient that stopped being a delivery target before quiescence.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DisconnectEvent {
    pub recipient: String,
    pub observation: DisconnectObservation,
}

/// Every event one run recorded. Collectors push through an
/// `Arc<EventLog>`; the mutex is never held across an `.await`.
#[derive(Default)]
pub struct EventLog {
    state: Mutex<EventState>,
}

#[derive(Default)]
struct EventState {
    sent: Vec<SentEvent>,
    receipts: Vec<ReceiptEvent>,
    disconnects: Vec<DisconnectEvent>,
    join_failures: Vec<String>,
    /// Faults the runner itself observed (hook firings, send failures,
    /// malformed frames). The oracle folds these into the verdict, so a
    /// replay reproduces it exactly from the same events.
    faults: Vec<InvalidReason>,
    server_terminated: bool,
}

impl EventLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_sent(&self, event: SentEvent) {
        self.state
            .lock()
            .expect("event log poisoned")
            .sent
            .push(event);
    }

    pub fn push_receipt(&self, event: ReceiptEvent) {
        self.state
            .lock()
            .expect("event log poisoned")
            .receipts
            .push(event);
    }

    pub fn push_disconnect(&self, event: DisconnectEvent) {
        self.state
            .lock()
            .expect("event log poisoned")
            .disconnects
            .push(event);
    }

    pub fn push_join_failure(&self, failure: String) {
        self.state
            .lock()
            .expect("event log poisoned")
            .join_failures
            .push(failure);
    }

    /// Record a runner-observed fault (hook firing, send failure, malformed
    /// frame, wedged generator). Declared hooks and per-sender saturation
    /// are deduplicated: repeated kills/evictions are the same fault, and
    /// repeated saturation keeps the worst observed lag.
    pub fn push_fault(&self, reason: InvalidReason) {
        let mut state = self.state.lock().expect("event log poisoned");
        match &reason {
            InvalidReason::ServerTerminated => {
                if state.server_terminated {
                    return;
                }
                state.server_terminated = true;
            }
            InvalidReason::GeneratorSaturated {
                max_lag_us,
                bound_us,
            } => {
                for existing in &mut state.faults {
                    if let InvalidReason::GeneratorSaturated {
                        max_lag_us: recorded,
                        ..
                    } = existing
                    {
                        *recorded = (*recorded).max(*max_lag_us);
                        let _ = bound_us;
                        return;
                    }
                }
            }
            InvalidReason::SlowConsumerDisconnect { .. } => {
                if state.faults.iter().any(|existing| {
                    matches!(existing, InvalidReason::SlowConsumerDisconnect { .. })
                }) {
                    return;
                }
            }
            _ => {}
        }
        state.faults.push(reason);
    }

    /// Whether the server-termination fault has been declared (senders stop
    /// treating their own socket errors as independent faults).
    pub fn was_server_terminated(&self) -> bool {
        self.state
            .lock()
            .expect("event log poisoned")
            .server_terminated
    }

    /// Snapshot every recorded event (deterministic order: sends by
    /// `(sender, seq)`; receipts, disconnects, join failures, and faults in
    /// recorded arrival order — the oracle checks per-stream ARRIVAL order,
    /// so receipts must never be reordered).
    pub fn snapshot(&self) -> RunRecords {
        let mut state = self.state.lock().expect("event log poisoned");
        let mut records = RunRecords {
            sent: std::mem::take(&mut state.sent),
            receipts: std::mem::take(&mut state.receipts),
            disconnects: std::mem::take(&mut state.disconnects),
            join_failures: std::mem::take(&mut state.join_failures),
            faults: std::mem::take(&mut state.faults),
        };
        records
            .sent
            .sort_by(|a, b| (&a.sender, a.seq).cmp(&(&b.sender, b.seq)));
        records
    }
}

/// The replayable event set of one run (the contents of
/// `deliveries.jsonl`).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RunRecords {
    pub sent: Vec<SentEvent>,
    pub receipts: Vec<ReceiptEvent>,
    pub disconnects: Vec<DisconnectEvent>,
    pub join_failures: Vec<String>,
    pub faults: Vec<InvalidReason>,
}

/// One tagged line of `deliveries.jsonl`: the raw events of a run, in
/// recording-independent tagged form so a replay can rebuild
/// [`RunRecords`] exactly.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event_kind", rename_all = "snake_case")]
pub enum DeliveryEvent {
    Sent(SentEvent),
    Receipt(ReceiptEvent),
    Disconnect(DisconnectEvent),
    JoinFailure { detail: String },
    Fault(InvalidReason),
}

impl RunRecords {
    /// Every event as a tagged JSONL line, in the canonical order
    /// (sends, receipts, disconnects, join failures, faults).
    pub fn events(&self) -> impl Iterator<Item = DeliveryEvent> + '_ {
        self.sent
            .iter()
            .cloned()
            .map(DeliveryEvent::Sent)
            .chain(self.receipts.iter().cloned().map(DeliveryEvent::Receipt))
            .chain(
                self.disconnects
                    .iter()
                    .cloned()
                    .map(DeliveryEvent::Disconnect),
            )
            .chain(
                self.join_failures
                    .iter()
                    .cloned()
                    .map(|detail| DeliveryEvent::JoinFailure { detail }),
            )
            .chain(self.faults.iter().cloned().map(DeliveryEvent::Fault))
    }
}
