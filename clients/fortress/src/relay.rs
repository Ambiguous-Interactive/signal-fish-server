use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fortress_rollback::network::codec;
use fortress_rollback::{Message, NonBlockingSocket};
use signal_fish_client::protocol::GameDataEncoding;
use uuid::Uuid;

const DESTINATION_BYTES: usize = 16;
const NONCE_BYTES: usize = 16;
const SEQUENCE_BYTES: usize = 8;
const ENVELOPE_BYTES: usize = DESTINATION_BYTES + NONCE_BYTES + SEQUENCE_BYTES;
const MAX_OUTBOUND_FRAMES: usize = 256;
const MAX_INBOUND_FRAMES: usize = 256;
const MAX_INBOUND_PER_POLL: usize = 256;

#[derive(Debug, Clone, Copy, Default)]
pub struct RelayCounters {
    pub enqueued_outbound: u64,
    pub accepted_inbound: u64,
    pub malformed_inbound: u64,
    pub wrong_destination: u64,
    pub unknown_sender: u64,
    pub outbound_overflow: u64,
    pub inbound_overflow: u64,
    pub encode_failures: u64,
    pub completion_underflow: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RelayLedger {
    pub count: u64,
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub sequence_hash: u64,
}

pub struct InboundRelayFrame<'a> {
    pub local: Uuid,
    pub known_remote: Uuid,
    pub from: Uuid,
    pub encoding: GameDataEncoding,
    pub seq: Option<u64>,
    pub epoch: Option<u32>,
    pub payload: &'a [u8],
}

#[derive(Debug)]
pub struct OutboundRelayFrame {
    pub payload: Vec<u8>,
    enqueued_at: Instant,
    sequence: u64,
}

#[derive(Debug, Default)]
struct Shared {
    outbound: VecDeque<OutboundRelayFrame>,
    admitted: VecDeque<Instant>,
    inbound: VecDeque<(Uuid, Message)>,
    counters: RelayCounters,
    observed_client_sent: u64,
    peak_queue_depth: usize,
    peak_oldest_queue_age: Duration,
    local_instance_nonce: Option<Uuid>,
    expected_remote_nonce: Option<Uuid>,
    next_outbound_sequence: u64,
    sent_ledger: RelayLedger,
    received_ledger: RelayLedger,
}

/// Fortress's UDP-like socket boundary backed by Signal Fish binary relay frames.
///
/// `send_to` only serializes and admits work to a bounded local FIFO. The game
/// loop drains that FIFO into `SignalFishPollingClient`, retaining the refused
/// head on backpressure. Accepted timestamps remain tracked until the client's
/// cumulative sent counter proves that the WebSocket write completed.
#[derive(Debug, Clone, Default)]
pub struct RelaySocket {
    shared: Arc<Mutex<Shared>>,
}

impl RelaySocket {
    pub fn configure_identity(
        &self,
        local_instance_nonce: Uuid,
        expected_remote_nonce: Uuid,
    ) -> Result<(), &'static str> {
        let Ok(mut shared) = self.shared.lock() else {
            return Err("relay identity mutex poisoned");
        };
        match (shared.local_instance_nonce, shared.expected_remote_nonce) {
            (None, None) => {
                shared.local_instance_nonce = Some(local_instance_nonce);
                shared.expected_remote_nonce = Some(expected_remote_nonce);
                shared.next_outbound_sequence = 1;
                Ok(())
            }
            (Some(local), Some(remote))
                if local == local_instance_nonce && remote == expected_remote_nonce =>
            {
                Ok(())
            }
            _ => Err("relay identity cannot change after configuration"),
        }
    }

    pub fn take_outbound(&self) -> Option<OutboundRelayFrame> {
        self.shared.lock().ok()?.outbound.pop_front()
    }

    pub fn return_outbound_front(&self, frame: OutboundRelayFrame) {
        if let Ok(mut shared) = self.shared.lock() {
            if shared.outbound.len() >= MAX_OUTBOUND_FRAMES {
                shared.outbound.pop_back();
                shared.counters.outbound_overflow =
                    shared.counters.outbound_overflow.saturating_add(1);
            }
            shared.outbound.push_front(frame);
            sample_queue(&mut shared, Instant::now());
        }
    }

    pub fn mark_admitted(&self, frame: OutboundRelayFrame) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.admitted.push_back(frame.enqueued_at);
            record_sequence(&mut shared.sent_ledger, frame.sequence);
            sample_queue(&mut shared, Instant::now());
        }
    }

    /// Reconcile completed game-data writes with frames previously admitted to
    /// the client. The client counter is cumulative and advances only after the
    /// transport reports a successful write.
    pub fn record_client_sent(&self, client_sent: u64) {
        if let Ok(mut shared) = self.shared.lock() {
            let completed = client_sent.saturating_sub(shared.observed_client_sent);
            shared.observed_client_sent = client_sent;
            for _ in 0..completed {
                if shared.admitted.pop_front().is_none() {
                    shared.counters.completion_underflow =
                        shared.counters.completion_underflow.saturating_add(1);
                }
            }
            sample_queue(&mut shared, Instant::now());
        }
    }

    pub fn sample_queue(&self) {
        if let Ok(mut shared) = self.shared.lock() {
            sample_queue(&mut shared, Instant::now());
        }
    }

    pub fn reset_queue_peak(&self) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.peak_queue_depth = queue_depth(&shared);
            shared.peak_oldest_queue_age = oldest_queue_age(&shared, Instant::now());
        }
    }

    pub fn queue_depth(&self) -> usize {
        self.shared
            .lock()
            .map_or(usize::MAX, |shared| queue_depth(&shared))
    }

    pub fn peak_queue_depth(&self) -> usize {
        self.shared
            .lock()
            .map_or(usize::MAX, |shared| shared.peak_queue_depth)
    }

    pub fn peak_oldest_queue_age(&self) -> Duration {
        self.shared
            .lock()
            .map_or(Duration::MAX, |shared| shared.peak_oldest_queue_age)
    }

    pub fn counters(&self) -> RelayCounters {
        self.shared
            .lock()
            .map_or_else(|_| RelayCounters::default(), |shared| shared.counters)
    }

    pub fn sent_ledger(&self) -> RelayLedger {
        self.shared
            .lock()
            .map_or_else(|_| RelayLedger::default(), |shared| shared.sent_ledger)
    }

    pub fn received_ledger(&self) -> RelayLedger {
        self.shared
            .lock()
            .map_or_else(|_| RelayLedger::default(), |shared| shared.received_ledger)
    }

    pub fn admit_inbound(&self, frame: InboundRelayFrame<'_>) {
        let InboundRelayFrame {
            local,
            known_remote,
            from,
            encoding,
            seq,
            epoch,
            payload,
        } = frame;
        let Ok(mut shared) = self.shared.lock() else {
            return;
        };
        if encoding != GameDataEncoding::MessagePack
            || seq.is_none_or(|value| value == 0)
            || epoch.is_none_or(|value| value == 0)
            || payload.len() <= ENVELOPE_BYTES
        {
            shared.counters.malformed_inbound = shared.counters.malformed_inbound.saturating_add(1);
            return;
        }
        if from != known_remote {
            shared.counters.unknown_sender = shared.counters.unknown_sender.saturating_add(1);
            return;
        }

        let Some(destination_bytes) = payload.get(..DESTINATION_BYTES) else {
            shared.counters.malformed_inbound = shared.counters.malformed_inbound.saturating_add(1);
            return;
        };
        let Ok(destination) = Uuid::from_slice(destination_bytes) else {
            shared.counters.malformed_inbound = shared.counters.malformed_inbound.saturating_add(1);
            return;
        };
        if destination != local {
            shared.counters.wrong_destination = shared.counters.wrong_destination.saturating_add(1);
            return;
        }

        let Some(nonce_bytes) = payload.get(DESTINATION_BYTES..DESTINATION_BYTES + NONCE_BYTES)
        else {
            shared.counters.malformed_inbound = shared.counters.malformed_inbound.saturating_add(1);
            return;
        };
        let Ok(sender_nonce) = Uuid::from_slice(nonce_bytes) else {
            shared.counters.malformed_inbound = shared.counters.malformed_inbound.saturating_add(1);
            return;
        };
        if shared.expected_remote_nonce != Some(sender_nonce) {
            shared.counters.unknown_sender = shared.counters.unknown_sender.saturating_add(1);
            return;
        }
        let Some(sequence_bytes) = payload
            .get(DESTINATION_BYTES + NONCE_BYTES..DESTINATION_BYTES + NONCE_BYTES + SEQUENCE_BYTES)
        else {
            shared.counters.malformed_inbound = shared.counters.malformed_inbound.saturating_add(1);
            return;
        };
        let Ok(sequence_array) = <[u8; SEQUENCE_BYTES]>::try_from(sequence_bytes) else {
            shared.counters.malformed_inbound = shared.counters.malformed_inbound.saturating_add(1);
            return;
        };
        let application_sequence = u64::from_be_bytes(sequence_array);
        if application_sequence == 0
            || (shared.received_ledger.count > 0
                && application_sequence != shared.received_ledger.last_sequence.saturating_add(1))
        {
            shared.counters.malformed_inbound = shared.counters.malformed_inbound.saturating_add(1);
            return;
        }

        let message_bytes = &payload[ENVELOPE_BYTES..];
        match codec::decode_message(message_bytes) {
            Ok((message, consumed)) if consumed == message_bytes.len() => {
                if shared.inbound.len() >= MAX_INBOUND_FRAMES {
                    shared.counters.inbound_overflow =
                        shared.counters.inbound_overflow.saturating_add(1);
                } else {
                    shared.inbound.push_back((from, message));
                    shared.counters.accepted_inbound =
                        shared.counters.accepted_inbound.saturating_add(1);
                    record_sequence(&mut shared.received_ledger, application_sequence);
                }
            }
            Ok(_) | Err(_) => {
                shared.counters.malformed_inbound =
                    shared.counters.malformed_inbound.saturating_add(1);
            }
        }
    }

    fn enqueue_outbound(&self, destination: &Uuid, encoded: Vec<u8>) {
        if let Ok(mut shared) = self.shared.lock() {
            if shared.outbound.len() >= MAX_OUTBOUND_FRAMES {
                shared.counters.outbound_overflow =
                    shared.counters.outbound_overflow.saturating_add(1);
                return;
            }
            let Some(local_instance_nonce) = shared.local_instance_nonce else {
                shared.counters.encode_failures = shared.counters.encode_failures.saturating_add(1);
                return;
            };
            let sequence = shared.next_outbound_sequence;
            shared.next_outbound_sequence = shared.next_outbound_sequence.saturating_add(1);
            let mut payload = Vec::with_capacity(ENVELOPE_BYTES.saturating_add(encoded.len()));
            payload.extend_from_slice(destination.as_bytes());
            payload.extend_from_slice(local_instance_nonce.as_bytes());
            payload.extend_from_slice(&sequence.to_be_bytes());
            payload.extend_from_slice(&encoded);
            shared.outbound.push_back(OutboundRelayFrame {
                payload,
                enqueued_at: Instant::now(),
                sequence,
            });
            shared.counters.enqueued_outbound = shared.counters.enqueued_outbound.saturating_add(1);
            sample_queue(&mut shared, Instant::now());
        }
    }
}

impl NonBlockingSocket<Uuid> for RelaySocket {
    fn send_to(&mut self, message: &Message, destination: &Uuid) {
        let encoded = match codec::encode(message) {
            Ok(encoded) => encoded,
            Err(_) => {
                if let Ok(mut shared) = self.shared.lock() {
                    shared.counters.encode_failures =
                        shared.counters.encode_failures.saturating_add(1);
                }
                return;
            }
        };
        self.enqueue_outbound(destination, encoded);
    }

    fn receive_all_messages(&mut self) -> Vec<(Uuid, Message)> {
        let Ok(mut shared) = self.shared.lock() else {
            return Vec::new();
        };
        let count = shared.inbound.len().min(MAX_INBOUND_PER_POLL);
        shared.inbound.drain(..count).collect()
    }
}

fn queue_depth(shared: &Shared) -> usize {
    shared.outbound.len().saturating_add(shared.admitted.len())
}

fn oldest_queue_age(shared: &Shared, now: Instant) -> Duration {
    shared
        .admitted
        .front()
        .into_iter()
        .chain(shared.outbound.front().map(|frame| &frame.enqueued_at))
        .map(|started| now.saturating_duration_since(*started))
        .max()
        .unwrap_or_default()
}

fn sample_queue(shared: &mut Shared, now: Instant) {
    shared.peak_queue_depth = shared.peak_queue_depth.max(queue_depth(shared));
    shared.peak_oldest_queue_age = shared
        .peak_oldest_queue_age
        .max(oldest_queue_age(shared, now));
}

fn record_sequence(ledger: &mut RelayLedger, sequence: u64) {
    if ledger.count == 0 {
        ledger.first_sequence = sequence;
        ledger.sequence_hash = 0xcbf2_9ce4_8422_2325;
    }
    ledger.count = ledger.count.saturating_add(1);
    ledger.last_sequence = sequence;
    for byte in sequence.to_be_bytes() {
        ledger.sequence_hash ^= u64::from(byte);
        ledger.sequence_hash = ledger.sequence_hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn outbound(payload: Vec<u8>, enqueued_at: Instant, sequence: u64) -> OutboundRelayFrame {
        OutboundRelayFrame {
            payload,
            enqueued_at,
            sequence,
        }
    }

    #[test]
    fn rejected_outbound_is_restored_without_reordering() {
        let socket = RelaySocket::default();
        let now = Instant::now();
        socket.return_outbound_front(outbound(vec![2], now, 2));
        socket.return_outbound_front(outbound(vec![1], now, 1));
        let refused = socket.take_outbound().expect("first");
        socket.return_outbound_front(refused);
        assert_eq!(
            socket.take_outbound().map(|frame| frame.payload),
            Some(vec![1])
        );
        assert_eq!(
            socket.take_outbound().map(|frame| frame.payload),
            Some(vec![2])
        );
    }

    #[test]
    fn accepted_frame_remains_outstanding_until_client_confirms_write() {
        let socket = RelaySocket::default();
        let frame = outbound(vec![1], Instant::now() - Duration::from_millis(20), 1);
        socket.mark_admitted(frame);
        socket.sample_queue();
        assert_eq!(socket.queue_depth(), 1);
        assert!(socket.peak_oldest_queue_age() >= Duration::from_millis(20));
        socket.record_client_sent(1);
        assert_eq!(socket.queue_depth(), 0);
        assert_eq!(socket.counters().completion_underflow, 0);
    }

    #[test]
    fn completion_underflow_is_observable() {
        let socket = RelaySocket::default();
        socket.record_client_sent(1);
        assert_eq!(socket.counters().completion_underflow, 1);
    }

    #[test]
    fn outbound_admission_is_bounded_and_observable() {
        let socket = RelaySocket::default();
        let destination = Uuid::new_v4();
        socket
            .configure_identity(Uuid::new_v4(), Uuid::new_v4())
            .expect("configure relay identity");
        for byte in 0..MAX_OUTBOUND_FRAMES {
            socket.enqueue_outbound(&destination, vec![byte as u8]);
        }
        socket.enqueue_outbound(&destination, vec![0xFF]);
        assert_eq!(socket.queue_depth(), MAX_OUTBOUND_FRAMES);
        assert_eq!(
            socket.counters().enqueued_outbound,
            MAX_OUTBOUND_FRAMES as u64
        );
        assert_eq!(socket.counters().outbound_overflow, 1);
    }
}
