//! Delivery ledger: zero-loss-or-loud-disconnect verification for the relay
//! soak and chaos suites.
//!
//! The delivery contract under test (issue #131) is that the relay **never
//! silently drops a message**: every room-scoped payload is either delivered
//! to every connected peer, or the unresponsive peer is loudly disconnected
//! (slow-consumer metrics + close). The ledger turns that contract into a
//! terminal assertion over real-socket traffic:
//!
//! - Senders embed a monotone per-sender sequence in each `GameData` payload
//!   via [`LedgerPayload`] (`{"ledger_sender": <name>, "seq": <n>,
//!   "padding": "..."}`), so any loss, reorder, or duplication is observable
//!   per (receiver, sender) stream.
//! - Receiver drain tasks call [`DeliveryLedger::record`] for every payload
//!   they read; a receiver deliberately (or chaotically) torn down is marked
//!   with [`DeliveryLedger::note_receiver_disconnected`].
//! - [`DeliveryLedger::assert_zero_loss_or_loud_disconnect`] then requires,
//!   per receiver, either (a) the exact gap-free, in-order `0..total` stream
//!   from every sender, or (b) a recorded disconnect — with slow-consumer
//!   evictions additionally accounted for by the server's
//!   `websocket_slow_consumer_disconnects` metric. Failure messages print the
//!   exact deficit (first gap, missing count, last seen), making a silent
//!   "~298-packet deficit" of the kind reported in #131 directly observable.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use signal_fish_server::metrics::ServerMetrics;

/// Builder for ledger-tracked `GameData` payloads: a monotone per-sender
/// sequence plus fixed-size padding (so bursts cannot hide inside kernel
/// socket buffers — the server's queueing must absorb them).
pub struct LedgerPayload {
    sender: String,
    next_seq: u64,
    padding: String,
}

impl LedgerPayload {
    /// Create a payload builder for `sender`, padding every payload with
    /// `padding_bytes` bytes of filler.
    pub fn new(sender: &str, padding_bytes: usize) -> Self {
        Self {
            sender: sender.to_string(),
            next_seq: 0,
            padding: "x".repeat(padding_bytes),
        }
    }

    /// Produce the next payload in the sequence.
    pub fn next(&mut self) -> serde_json::Value {
        let seq = self.next_seq;
        self.next_seq += 1;
        serde_json::json!({
            "ledger_sender": self.sender.as_str(),
            "seq": seq,
            "padding": self.padding.as_str(),
        })
    }

    /// How many payloads have been produced so far (the exclusive upper bound
    /// of the emitted `0..sent()` sequence range).
    pub fn sent(&self) -> u64 {
        self.next_seq
    }

    /// The sender name embedded in every payload.
    pub fn sender(&self) -> &str {
        &self.sender
    }
}

/// Extract the `(ledger_sender, seq)` pair from a received `GameData`
/// payload, or `None` for payloads that do not carry ledger fields.
pub fn extract(data: &serde_json::Value) -> Option<(String, u64)> {
    let sender = data.get("ledger_sender")?.as_str()?.to_string();
    let seq = data.get("seq")?.as_u64()?;
    Some((sender, seq))
}

/// Why a receiver stopped being a delivery target before the flood ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisconnectReason {
    /// The server evicted this receiver as a slow consumer. The terminal
    /// assertion requires `websocket_slow_consumer_disconnects` to account
    /// for every receiver recorded with this reason.
    SlowConsumerEviction,
    /// The test itself severed the receiver's link (RST/FIN fault injection,
    /// deliberate socket drop). No slow-consumer accounting is expected; the
    /// receiver must still hold a gap-free prefix of everything it read.
    InjectedFault(String),
}

/// One sender's contribution to a receiver's expected stream: the sender
/// emitted exactly the sequences `0..total_sent`.
pub struct SenderExpectation {
    pub sender: String,
    pub total_sent: u64,
}

/// Everything one receiver was expected to observe while connected.
pub struct ReceiverExpectation {
    pub receiver: String,
    pub senders: Vec<SenderExpectation>,
}

#[derive(Default)]
struct LedgerState {
    /// receiver -> sender -> seqs in arrival order.
    received: BTreeMap<String, BTreeMap<String, Vec<u64>>>,
    /// receiver -> last server-stamped sequence (see [`DeliveryLedger::record`]).
    last_server_seq: BTreeMap<String, u64>,
    /// receiver -> why it stopped receiving.
    disconnected: BTreeMap<String, DisconnectReason>,
}

/// Shared, thread-safe record of everything every receiver actually read.
///
/// Guarded by a `std::sync::Mutex` (never held across an `.await`) so
/// concurrent drain tasks can record through an `Arc<DeliveryLedger>`.
#[derive(Default)]
pub struct DeliveryLedger {
    state: Mutex<LedgerState>,
}

impl DeliveryLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `receiver` read `seq` from `sender`.
    ///
    /// `server_seq` is a forward-compatibility hook: the server does not yet
    /// stamp a per-connection delivery sequence, but when it does, drain
    /// tasks should pass it here and the ledger will cross-check it as
    /// strictly monotone per receiver — catching relay-side reordering or
    /// duplication that per-sender sequences alone cannot distinguish from
    /// legitimate multi-sender interleaving. Pass `None` until then.
    pub fn record(&self, receiver: &str, sender: &str, seq: u64, server_seq: Option<u64>) {
        let mut state = self.state.lock().expect("delivery ledger poisoned");
        if let Some(server_seq) = server_seq {
            if let Some(previous) = state.last_server_seq.get(receiver) {
                assert!(
                    server_seq > *previous,
                    "{receiver}: server-stamped sequence must be strictly monotone \
                     (got {server_seq} after {previous})"
                );
            }
            state
                .last_server_seq
                .insert(receiver.to_string(), server_seq);
        }
        state
            .received
            .entry(receiver.to_string())
            .or_default()
            .entry(sender.to_string())
            .or_default()
            .push(seq);
    }

    /// Mark `receiver` as disconnected. Recorded once; a second call for the
    /// same receiver fails loudly (a double disconnect is a test bug).
    pub fn note_receiver_disconnected(&self, receiver: &str, reason: DisconnectReason) {
        let mut state = self.state.lock().expect("delivery ledger poisoned");
        let previous = state.disconnected.insert(receiver.to_string(), reason);
        assert!(
            previous.is_none(),
            "{receiver}: disconnect noted twice (previous reason: {previous:?})"
        );
    }

    /// How many payloads `receiver` has recorded from `sender` so far. Used
    /// by drain loops as their metric-driven completion condition.
    pub fn received_count(&self, receiver: &str, sender: &str) -> u64 {
        let state = self.state.lock().expect("delivery ledger poisoned");
        state
            .received
            .get(receiver)
            .and_then(|per_sender| per_sender.get(sender))
            .map_or(0, |seqs| seqs.len() as u64)
    }

    /// Terminal assertion: every receiver either observed the exact,
    /// gap-free, in-order `0..total_sent` stream from every sender, or was
    /// recorded as disconnected — in which case it must still hold a gap-free
    /// in-order **prefix** of each stream (loss is only ever the tail cut off
    /// with the connection, never a silent hole), and every
    /// [`DisconnectReason::SlowConsumerEviction`] must be accounted for by
    /// the server's `websocket_slow_consumer_disconnects` counter.
    ///
    /// Call only at a quiescent point (all sends finished, all drains done).
    pub fn assert_zero_loss_or_loud_disconnect(
        &self,
        metrics: &ServerMetrics,
        expectations: &[ReceiverExpectation],
    ) {
        let state = self.state.lock().expect("delivery ledger poisoned");
        let empty = BTreeMap::new();

        // Any recorded receiver missing from the expectations is a test bug:
        // it would silently exempt that receiver from the loss check.
        for recorded_receiver in state.received.keys().chain(state.disconnected.keys()) {
            assert!(
                expectations
                    .iter()
                    .any(|expectation| expectation.receiver == *recorded_receiver),
                "ledger recorded data for receiver {recorded_receiver} but no expectation \
                 covers it — every recorded receiver must be asserted on"
            );
        }

        let mut evicted_receivers = 0u64;
        for expectation in expectations {
            let disconnect = state.disconnected.get(&expectation.receiver);
            if matches!(disconnect, Some(DisconnectReason::SlowConsumerEviction)) {
                evicted_receivers += 1;
            }
            let per_sender = state.received.get(&expectation.receiver).unwrap_or(&empty);

            // A sender the expectation does not cover is a test bug too.
            for recorded_sender in per_sender.keys() {
                assert!(
                    expectation
                        .senders
                        .iter()
                        .any(|sender| sender.sender == *recorded_sender),
                    "{}: recorded payloads from unexpected sender {recorded_sender}",
                    expectation.receiver
                );
            }

            for sender in &expectation.senders {
                let seqs = per_sender
                    .get(&sender.sender)
                    .map_or(&[][..], Vec::as_slice);
                assert_stream(
                    &expectation.receiver,
                    &sender.sender,
                    seqs,
                    sender.total_sent,
                    disconnect,
                );
            }
        }

        let slow_consumer_disconnects = metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed);
        assert!(
            slow_consumer_disconnects >= evicted_receivers,
            "{evicted_receivers} receiver(s) were recorded as slow-consumer evictions but the \
             server only counted {slow_consumer_disconnects} slow-consumer disconnect(s) — an \
             eviction the metrics do not account for is a silent disconnect"
        );
    }
}

/// Assert one (receiver, sender) stream: gap-free and in order always; the
/// complete `0..total_sent` range unless the receiver was disconnected, in
/// which case any gap-free prefix is acceptable. Failure messages carry the
/// exact deficit: first gap, missing count, and last seen sequence.
fn assert_stream(
    receiver: &str,
    sender: &str,
    seqs: &[u64],
    total_sent: u64,
    disconnect: Option<&DisconnectReason>,
) {
    let last_seen = seqs
        .last()
        .map_or_else(|| "<none>".to_string(), u64::to_string);

    // In-order + gap-free: arrival position i must carry sequence i. The
    // first mismatch is the first gap (or reorder/duplicate).
    for (position, seq) in seqs.iter().enumerate() {
        assert_eq!(
            *seq,
            position as u64,
            "{receiver} <- {sender}: stream is not a gap-free in-order prefix: first gap at \
             position {position} (got seq {seq}), received {} of {total_sent} sent \
             ({} missing), last seen seq {last_seen}",
            seqs.len(),
            total_sent.saturating_sub(seqs.len() as u64),
        );
    }

    match disconnect {
        // Connected throughout: the prefix must be the whole stream.
        None => assert_eq!(
            seqs.len() as u64,
            total_sent,
            "{receiver} <- {sender}: {}-packet deficit with no recorded disconnect: received \
             {} of {total_sent} sent, first gap at seq {}, last seen seq {last_seen} — the \
             relay silently dropped messages",
            total_sent.saturating_sub(seqs.len() as u64),
            seqs.len(),
            seqs.len(),
        ),
        // Disconnected: the tail died with the connection — loudly, per the
        // metric accounting in the caller — but never more than sent.
        Some(reason) => assert!(
            (seqs.len() as u64) <= total_sent,
            "{receiver} <- {sender}: received {} payloads but only {total_sent} were sent \
             (disconnect reason: {reason:?}) — duplicated delivery",
            seqs.len(),
        ),
    }
}
