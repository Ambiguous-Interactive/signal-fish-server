//! Protocol-v3 delivery conformance layered over the payload-level delivery ledger.
//!
//! [`DeliveryLedger`] proves that test-authored payload sequences are complete (or
//! end at a loud disconnect). `ConformanceAuditor` keeps that check and adds the
//! server-stamped `(epoch, seq)` contract, including lifecycle boundaries and
//! reconnect watermarks. Suites feed each decoded [`ServerMessage`] through one
//! entry point so protocol and payload accounting cannot drift apart.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use signal_fish_server::metrics::ServerMetrics;
use signal_fish_server::protocol::{
    ErrorCode, GameDataEncoding, PlayerId, PlayerInfo, ReconnectedPayload, ServerMessage,
};

use super::delivery_ledger::{extract, DeliveryLedger, DisconnectReason, ReceiverExpectation};

/// Delivery verification shared by the real-socket conformance suites.
#[derive(Default)]
pub struct ConformanceAuditor {
    ledger: DeliveryLedger,
    state: Mutex<AuditorState>,
}

#[derive(Default)]
struct AuditorState {
    receivers: BTreeMap<String, ReceiverState>,
}

#[derive(Default)]
struct ReceiverState {
    senders: BTreeMap<PlayerId, SenderState>,
    /// One in-stream unsupported-format error authorizes exactly one skipped
    /// server sequence. The current wire error identifies the sender only in
    /// human-readable prose, so this budget is necessarily receiver-scoped.
    format_skips: u64,
    disconnect_cause: Option<ReceiverDisconnectCause>,
    last_relay_stats: Option<RelayStatsSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiverDisconnectCause {
    SlowConsumer,
    ActivityTimeout,
    ServerRestart,
    ServerClose { code: u16, reason: String },
    InjectedFault(String),
}

#[derive(Default)]
struct SenderState {
    /// Last observed sequence for every epoch. Retaining old epochs makes a
    /// late duplicate/backward frame unambiguously fatal.
    epochs: BTreeMap<u32, u64>,
    active_epoch: Option<u32>,
    /// Epoch announced by RoomJoined/PlayerJoined/PlayerReconnected before
    /// the first frame of that incarnation.
    lifecycle_epoch: Option<u32>,
    /// `RoomJoined` is a snapshot taken after this receiver entered the room.
    /// Existing senders may already be beyond seq 1, so their first observed
    /// frame establishes the receiver-local baseline.
    allow_first_seq_baseline: bool,
    departed: bool,
}

#[derive(Clone, Copy)]
struct RelayStatsSnapshot {
    interval_ms: u64,
    sent_to_you: u64,
    dropped_for_you: u64,
    backpressure_events: u64,
}

/// The bare MessagePack game-data envelope emitted on binary WebSocket frames.
///
/// This mirrors the private production wire type solely for test decoding. Raw
/// JSON/Rkyv binary passthrough frames have no envelope or v3 stamp and cannot
/// be audited through this entry point.
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecordedBinaryGameData {
    pub from_player: PlayerId,
    pub encoding: GameDataEncoding,
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
    #[serde(default)]
    pub seq: Option<u64>,
    #[serde(default)]
    pub epoch: Option<u32>,
}

impl ConformanceAuditor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse and record one text frame, returning the parsed message for any
    /// scenario-specific assertion. Callers therefore never deserialize a
    /// frame once for the ledger and again for protocol checks.
    pub fn record_text_frame(&self, receiver: &str, text: &str) -> ServerMessage {
        let message: ServerMessage = serde_json::from_str(text).unwrap_or_else(|error| {
            panic!("{receiver}: invalid ServerMessage text frame: {error}; text={text:?}")
        });
        self.record_message(receiver, &message);
        message
    }

    /// Decode and record one bare MessagePack game-data WebSocket frame.
    pub fn record_binary_frame(&self, receiver: &str, wire: &[u8]) -> RecordedBinaryGameData {
        let frame: RecordedBinaryGameData = rmp_serde::from_slice(wire).unwrap_or_else(|error| {
            panic!("{receiver}: invalid MessagePack game-data frame: {error}; bytes={wire:?}")
        });
        assert_eq!(
            frame.encoding,
            GameDataEncoding::MessagePack,
            "{receiver}: stamped binary envelope must declare message_pack encoding"
        );
        self.record_stamp(receiver, frame.from_player, frame.seq, frame.epoch);

        if let Ok(data) = rmp_serde::from_slice::<serde_json::Value>(&frame.payload) {
            self.record_ledger_payload(receiver, &data);
        }
        frame
    }

    /// Record one already-decoded server message exactly once.
    pub fn record_message(&self, receiver: &str, message: &ServerMessage) {
        match message {
            ServerMessage::GameData {
                from_player,
                data,
                seq,
                epoch,
            } => {
                self.record_stamp(receiver, *from_player, *seq, *epoch);
                self.record_ledger_payload(receiver, data);
            }
            ServerMessage::GameDataBinary {
                from_player,
                seq,
                epoch,
                ..
            } => self.record_stamp(receiver, *from_player, *seq, *epoch),
            ServerMessage::RoomJoined(payload) => {
                self.record_room_joined(receiver, &payload.current_players)
            }
            ServerMessage::PlayerJoined { player } => {
                self.record_lifecycle_epoch(receiver, player.id, player.epoch, "PlayerJoined");
            }
            ServerMessage::PlayerLeft { player_id } => {
                let mut state = self.state.lock().expect("conformance auditor poisoned");
                state
                    .receivers
                    .entry(receiver.to_string())
                    .or_default()
                    .senders
                    .entry(*player_id)
                    .or_default()
                    .departed = true;
            }
            ServerMessage::PlayerReconnected { player_id, epoch } => {
                self.record_lifecycle_epoch(receiver, *player_id, *epoch, "PlayerReconnected");
            }
            ServerMessage::Reconnected(payload) => self.record_reconnected(receiver, payload),
            ServerMessage::Error {
                error_code: Some(ErrorCode::UnsupportedGameDataFormat),
                ..
            } => {
                let mut state = self.state.lock().expect("conformance auditor poisoned");
                let receiver_state = state.receivers.entry(receiver.to_string()).or_default();
                receiver_state.format_skips = receiver_state
                    .format_skips
                    .checked_add(1)
                    .expect("unsupported-format skip budget overflowed");
            }
            ServerMessage::RelayStats {
                interval_ms,
                sent_to_you,
                dropped_for_you,
                backpressure_events,
                ..
            } => self.record_relay_stats(
                receiver,
                RelayStatsSnapshot {
                    interval_ms: *interval_ms,
                    sent_to_you: *sent_to_you,
                    dropped_for_you: *dropped_for_you,
                    backpressure_events: *backpressure_events,
                },
            ),
            _ => {}
        }
    }

    /// Record a terminal WebSocket close without collapsing server-owned close
    /// causes into test-authored transport faults.
    pub fn record_close(&self, receiver: &str, code: u16, reason: &str) {
        let (cause, disconnect_reason) = match code {
            4000 => (
                ReceiverDisconnectCause::ServerRestart,
                DisconnectReason::InjectedFault(format!("server restart close {code}: {reason}")),
            ),
            4002 => (
                ReceiverDisconnectCause::SlowConsumer,
                DisconnectReason::SlowConsumerEviction,
            ),
            4003 => (
                ReceiverDisconnectCause::ActivityTimeout,
                DisconnectReason::InjectedFault(format!("activity-timeout close {code}: {reason}")),
            ),
            _ => (
                ReceiverDisconnectCause::ServerClose {
                    code,
                    reason: reason.to_string(),
                },
                DisconnectReason::InjectedFault(format!("server websocket close {code}: {reason}")),
            ),
        };
        self.record_disconnect(receiver, cause, disconnect_reason);
    }

    /// Record a test-authored transport fault (RST, SIGKILL, proxy cut, etc.).
    pub fn note_injected_fault(&self, receiver: &str, description: impl Into<String>) {
        let description = description.into();
        self.record_disconnect(
            receiver,
            ReceiverDisconnectCause::InjectedFault(description.clone()),
            DisconnectReason::InjectedFault(description),
        );
    }

    /// Record a process restart/SIGKILL that cannot produce a WebSocket close
    /// frame but is still an explicit, loud terminal delivery cause.
    pub fn note_server_restart(&self, receiver: &str) {
        self.record_disconnect(
            receiver,
            ReceiverDisconnectCause::ServerRestart,
            DisconnectReason::InjectedFault("server restart".to_string()),
        );
    }

    pub fn received_count(&self, receiver: &str, sender: &str) -> u64 {
        self.ledger.received_count(receiver, sender)
    }

    pub fn ledger(&self) -> &DeliveryLedger {
        &self.ledger
    }

    /// Recorded terminal cause for diagnostics, if this receiver disconnected.
    pub fn disconnect_cause(&self, receiver: &str) -> Option<ReceiverDisconnectCause> {
        self.state
            .lock()
            .expect("conformance auditor poisoned")
            .receivers
            .get(receiver)
            .and_then(|state| state.disconnect_cause.clone())
    }

    /// Terminal payload completeness plus the server-wide conservation law.
    pub async fn assert_conformance(
        &self,
        metrics: &ServerMetrics,
        expectations: &[ReceiverExpectation],
    ) {
        self.ledger
            .assert_zero_loss_or_loud_disconnect(metrics, expectations);
        super::assert_message_conservation(metrics).await;
    }

    fn record_disconnect(
        &self,
        receiver: &str,
        cause: ReceiverDisconnectCause,
        ledger_reason: DisconnectReason,
    ) {
        {
            let mut state = self.state.lock().expect("conformance auditor poisoned");
            let receiver_state = state.receivers.entry(receiver.to_string()).or_default();
            assert!(
                receiver_state.disconnect_cause.is_none(),
                "{receiver}: conformance disconnect recorded twice (previous cause: {:?})",
                receiver_state.disconnect_cause
            );
            receiver_state.disconnect_cause = Some(cause);
        }
        self.ledger
            .note_receiver_disconnected(receiver, ledger_reason);
    }

    fn record_ledger_payload(&self, receiver: &str, data: &serde_json::Value) {
        if data.get("ledger_sender").is_none() && data.get("seq").is_none() {
            return;
        }
        let (sender, payload_seq) = extract(data).unwrap_or_else(|| {
            panic!("{receiver}: ledger-shaped GameData has invalid ledger_sender/seq: {data}")
        });
        // Protocol seq is per sender, not per receiver, so it must not be fed
        // to DeliveryLedger's per-receiver hook.
        self.ledger.record(receiver, &sender, payload_seq, None);
    }

    fn record_room_joined(&self, receiver: &str, current_players: &[PlayerInfo]) {
        let mut state = self.state.lock().expect("conformance auditor poisoned");
        let receiver_state = state.receivers.entry(receiver.to_string()).or_default();

        // RoomJoined starts a new receiver-local room view. A sender that was
        // already active can legitimately first appear at any positive seq.
        receiver_state.senders.clear();
        receiver_state.format_skips = 0;
        for player in current_players {
            let Some(epoch) = player.epoch else { continue };
            assert!(
                epoch > 0,
                "{receiver} <- {}: RoomJoined advertised epoch 0",
                player.id
            );
            receiver_state.senders.insert(
                player.id,
                SenderState {
                    lifecycle_epoch: Some(epoch),
                    allow_first_seq_baseline: true,
                    ..SenderState::default()
                },
            );
        }
    }

    fn record_lifecycle_epoch(
        &self,
        receiver: &str,
        sender: PlayerId,
        epoch: Option<u32>,
        source: &str,
    ) {
        let Some(epoch) = epoch else { return };
        assert!(
            epoch > 0,
            "{receiver} <- {sender}: {source} advertised epoch 0"
        );

        let mut state = self.state.lock().expect("conformance auditor poisoned");
        let sender_state = state
            .receivers
            .entry(receiver.to_string())
            .or_default()
            .senders
            .entry(sender)
            .or_default();
        if let Some(active_epoch) = sender_state.active_epoch {
            assert!(
                epoch > active_epoch,
                "{receiver} <- {sender}: {source} advertised stale/duplicate epoch {epoch} after epoch {active_epoch}"
            );
        }
        sender_state.lifecycle_epoch = Some(epoch);
        sender_state.allow_first_seq_baseline = false;
        sender_state.departed = false;
    }

    fn record_reconnected(&self, receiver: &str, payload: &ReconnectedPayload) {
        let player_epochs: BTreeMap<PlayerId, u32> = payload
            .current_players
            .iter()
            .filter_map(|player| player.epoch.map(|epoch| (player.id, epoch)))
            .collect();
        let mut seen = BTreeSet::new();
        for watermark in &payload.sender_watermarks {
            assert!(
                seen.insert(watermark.player_id),
                "{receiver}: duplicate reconnect watermark for {}",
                watermark.player_id
            );
            assert!(
                watermark.epoch > 0,
                "{receiver}: reconnect watermark for {} has epoch 0",
                watermark.player_id
            );
            if let Some(snapshot_epoch) = player_epochs.get(&watermark.player_id) {
                assert_eq!(
                    watermark.epoch, *snapshot_epoch,
                    "{receiver}: reconnect watermark/snapshot epoch mismatch for {}",
                    watermark.player_id
                );
            } else if !player_epochs.is_empty() {
                panic!(
                    "{receiver}: reconnect watermark names player {} absent from the snapshot",
                    watermark.player_id
                );
            }
        }

        if !payload.sender_watermarks.is_empty() {
            let snapshot_players: BTreeSet<_> =
                payload.current_players.iter().map(|p| p.id).collect();
            assert_eq!(
                seen, snapshot_players,
                "{receiver}: reconnect watermarks must cover every current player exactly once"
            );
        }

        let mut state = self.state.lock().expect("conformance auditor poisoned");
        let receiver_state = state.receivers.entry(receiver.to_string()).or_default();
        receiver_state.disconnect_cause = None;
        for watermark in &payload.sender_watermarks {
            let sender_state = receiver_state
                .senders
                .entry(watermark.player_id)
                .or_default();
            if let Some(active_epoch) = sender_state.active_epoch {
                assert!(
                    watermark.epoch >= active_epoch,
                    "{receiver} <- {}: reconnect watermark epoch {} moved backward from {active_epoch}",
                    watermark.player_id,
                    watermark.epoch
                );
                if watermark.epoch == active_epoch {
                    let last = sender_state.epochs.get(&active_epoch).copied().unwrap_or(0);
                    assert!(
                        watermark.seq >= last,
                        "{receiver} <- {}: reconnect watermark seq {} moved backward from {last} in epoch {active_epoch}",
                        watermark.player_id,
                        watermark.seq
                    );
                }
            }
            sender_state.active_epoch = Some(watermark.epoch);
            sender_state.epochs.insert(watermark.epoch, watermark.seq);
            sender_state.lifecycle_epoch = Some(watermark.epoch);
            sender_state.allow_first_seq_baseline = false;
            sender_state.departed = false;
        }
    }

    fn record_relay_stats(&self, receiver: &str, next: RelayStatsSnapshot) {
        assert!(
            next.interval_ms > 0,
            "{receiver}: RelayStats interval_ms must be positive"
        );
        let mut state = self.state.lock().expect("conformance auditor poisoned");
        let receiver_state = state.receivers.entry(receiver.to_string()).or_default();
        if let Some(previous) = receiver_state.last_relay_stats {
            assert_eq!(
                next.interval_ms, previous.interval_ms,
                "{receiver}: RelayStats interval_ms changed within one connection"
            );
            assert!(
                next.sent_to_you >= previous.sent_to_you
                    && next.dropped_for_you >= previous.dropped_for_you
                    && next.backpressure_events >= previous.backpressure_events,
                "{receiver}: cumulative RelayStats counters moved backward"
            );
        }
        receiver_state.last_relay_stats = Some(next);
    }

    fn record_stamp(&self, receiver: &str, sender: PlayerId, seq: Option<u64>, epoch: Option<u32>) {
        let (seq, epoch) = match (seq, epoch) {
            (None, None) => return,
            (Some(seq), Some(epoch)) => (seq, epoch),
            _ => panic!(
                "{receiver} <- {sender}: v3 GameData must carry seq and epoch together (seq={seq:?}, epoch={epoch:?})"
            ),
        };
        assert!(
            seq > 0,
            "{receiver} <- {sender}: v3 sequence must start at 1"
        );
        assert!(
            epoch > 0,
            "{receiver} <- {sender}: v3 epoch must start at 1"
        );

        let mut state = self.state.lock().expect("conformance auditor poisoned");
        let receiver_state = state.receivers.entry(receiver.to_string()).or_default();
        let sender_state = receiver_state.senders.entry(sender).or_default();
        assert!(
            !sender_state.departed,
            "{receiver} <- {sender}: received GameData after PlayerLeft without a new lifecycle boundary"
        );

        let expected = match sender_state.active_epoch {
            Some(active_epoch) if epoch < active_epoch => panic!(
                "{receiver} <- {sender}: epoch moved backward from {active_epoch} to {epoch}"
            ),
            Some(active_epoch) if epoch == active_epoch => {
                let last = sender_state.epochs.get(&epoch).copied().unwrap_or(0);
                if seq <= last {
                    panic!(
                        "{receiver} <- {sender}: duplicate/backward sequence {seq} after {last} in epoch {epoch}"
                    );
                }
                last + 1
            }
            Some(active_epoch) => {
                assert_eq!(
                    sender_state.lifecycle_epoch,
                    Some(epoch),
                    "{receiver} <- {sender}: epoch advanced from {active_epoch} to {epoch} before a matching lifecycle boundary"
                );
                1
            }
            None => {
                assert_eq!(
                    sender_state.lifecycle_epoch,
                    Some(epoch),
                    "{receiver} <- {sender}: first epoch {epoch} arrived before a matching lifecycle boundary"
                );
                if sender_state.allow_first_seq_baseline {
                    seq
                } else {
                    1
                }
            }
        };

        if seq > expected {
            let gap = seq - expected;
            assert!(
                receiver_state.format_skips >= gap,
                "{receiver} <- {sender}: unexplained seq gap in epoch {epoch}: expected {expected}, got {seq} (gap {gap}, prior format-error budget {})",
                receiver_state.format_skips
            );
            receiver_state.format_skips -= gap;
        }
        sender_state.active_epoch = Some(epoch);
        sender_state.epochs.insert(epoch, seq);
        sender_state.lifecycle_epoch = Some(epoch);
        sender_state.allow_first_seq_baseline = false;
        sender_state.departed = false;
    }
}
