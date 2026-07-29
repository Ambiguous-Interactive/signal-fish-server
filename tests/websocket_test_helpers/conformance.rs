//! Protocol-v3 delivery conformance layered over the payload-level delivery ledger.
//!
//! [`DeliveryLedger`] proves that test-authored payload sequences are complete (or
//! end at a loud disconnect). `ConformanceAuditor` keeps that check and adds the
//! server-stamped `(epoch, seq)` contract, including lifecycle boundaries and
//! reconnect watermarks. Suites feed each decoded [`ServerMessage`] through one
//! entry point so protocol and payload accounting cannot drift apart.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use signal_fish_server::metrics::{DeliveryClassMetrics, DeliveryMetricsByClass, ServerMetrics};
pub use signal_fish_server::protocol::V3BinaryGameDataFrame as RecordedBinaryGameData;
use signal_fish_server::protocol::{
    decode_v3_binary_game_data, DeliveryClass, DeliveryCountersByClass, DeliveryGap,
    DeliveryGapReason, DeliveryReportPayload, GameDataEncoding, PlayerId, PlayerInfo,
    ReconnectedPayload, ServerMessage, DELIVERY_REPORT_MAX_GAPS,
};

use super::delivery_ledger::{extract, DeliveryLedger, DisconnectReason, ReceiverExpectation};

/// Delivery verification shared by the real-socket conformance suites.
pub struct ConformanceAuditor {
    default_mode: ReceiverProtocolMode,
    ledger: DeliveryLedger,
    state: Mutex<AuditorState>,
}

/// Wire contract expected for one physical receiver identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverProtocolMode {
    V2,
    V3,
}

#[derive(Default)]
struct AuditorState {
    receivers: BTreeMap<String, ReceiverState>,
    receiver_modes: BTreeMap<String, ReceiverProtocolMode>,
}

#[derive(Default)]
struct ReceiverState {
    room_state: ReceiverRoomState,
    senders: BTreeMap<PlayerId, SenderState>,
    /// Logical membership seen during this room lifecycle. Relay state may be
    /// retired once its terminal watermark is fully accounted, but a later
    /// reconnect must still prove the player was previously a room member.
    membership_history: BTreeSet<PlayerId>,
    /// Causally prior, exact omissions announced on the control lane. Keeping
    /// ranges partitioned by sender and epoch prevents one cause from paying
    /// for an unrelated stream's gap.
    pending_gaps: BTreeMap<(PlayerId, u32), BTreeMap<u64, PendingGap>>,
    last_delivery_counters: Option<DeliveryCountersByClass>,
    disconnect_cause: Option<ReceiverDisconnectCause>,
    last_relay_stats: Option<RelayStatsSnapshot>,
    unadvised_unsupported_gap: Option<DeliveryGap>,
    ledger_payloads: u64,
    observed_delivered: DeliveryCountersByClass,
    abandoned_requires_disconnect: bool,
    abandoned_advisory_seen: bool,
    had_room_lifecycle: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum ReceiverRoomState {
    #[default]
    Outside,
    Player,
    Spectator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeerLifecycleKind {
    Joined,
    Reconnected,
}

#[derive(Debug, Clone, Copy)]
struct PendingGap {
    to_seq: u64,
    reason: DeliveryGapReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiverDisconnectCause {
    SlowConsumer,
    ActivityTimeout,
    IdleTimeout,
    ServerShutdown,
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
    /// Lifecycle epochs announced before their data lane drains. Strict
    /// control priority permits several lifecycle messages to overtake older
    /// queued data, so announced epochs remain admissible until data actually
    /// advances past them.
    announced_epochs: BTreeSet<u32>,
    latest_lifecycle_epoch: Option<u32>,
    present: bool,
    known_member: bool,
    terminals: BTreeMap<u32, u64>,
}

#[derive(Clone, Copy)]
struct RelayStatsSnapshot {
    interval_ms: u64,
    sent_to_you: u64,
    dropped_for_you: u64,
    backpressure_events: u64,
}

impl ConformanceAuditor {
    pub fn new(mode: ReceiverProtocolMode) -> Self {
        Self {
            default_mode: mode,
            ledger: DeliveryLedger::new(),
            state: Mutex::new(AuditorState::default()),
        }
    }

    /// Register the protocol mode for one physical receiver identity before
    /// recording its traffic. Re-registering the same mode is harmless.
    pub fn register_receiver_mode(&self, receiver: &str, mode: ReceiverProtocolMode) {
        let mut state = self.state.lock().expect("conformance auditor poisoned");
        if let Some(receiver_state) = state.receivers.get(receiver) {
            assert!(
                receiver_state_is_reconnect_preface(receiver_state),
                "{receiver}: protocol mode cannot change after receiver traffic or room lifecycle"
            );
        }
        if let Some(previous) = state.receiver_modes.get(receiver).copied() {
            assert_eq!(
                previous, mode,
                "{receiver}: receiver protocol mode changed from {previous:?} to {mode:?}"
            );
        } else {
            state.receiver_modes.insert(receiver.to_string(), mode);
        }
    }

    /// Register the membership from a v2 `RoomJoined` frame that a socket
    /// helper consumed before handing the stream to the auditor.
    pub fn record_consumed_v2_room_snapshot(&self, receiver: &str, players: &[PlayerId]) {
        self.register_receiver_mode(receiver, ReceiverProtocolMode::V2);
        let mut state = self.state.lock().expect("conformance auditor poisoned");
        let receiver_state = state.receivers.entry(receiver.to_string()).or_default();
        assert_eq!(
            receiver_state.room_state,
            ReceiverRoomState::Outside,
            "{receiver}: consumed RoomJoined snapshot duplicated an active room lifecycle"
        );
        receiver_state.room_state = ReceiverRoomState::Player;
        receiver_state.had_room_lifecycle = true;
        for player in players {
            receiver_state.membership_history.insert(*player);
            assert!(
                receiver_state
                    .senders
                    .insert(
                        *player,
                        SenderState {
                            present: true,
                            known_member: true,
                            ..SenderState::default()
                        },
                    )
                    .is_none(),
                "{receiver}: consumed RoomJoined snapshot contains duplicate player {player}"
            );
        }
    }

    fn receiver_mode(&self, receiver: &str) -> ReceiverProtocolMode {
        let mut state = self.state.lock().expect("conformance auditor poisoned");
        *state
            .receiver_modes
            .entry(receiver.to_string())
            .or_insert(self.default_mode)
    }

    /// Parse and record one text frame, returning the parsed message for any
    /// scenario-specific assertion. Callers therefore never deserialize a
    /// frame once for the ledger and again for protocol checks.
    pub fn record_text_frame(&self, receiver: &str, text: &str) -> ServerMessage {
        let message: ServerMessage = serde_json::from_str(text).unwrap_or_else(|error| {
            panic!("{receiver}: invalid ServerMessage text frame: {error}; text={text:?}")
        });
        assert!(
            !matches!(message, ServerMessage::GameDataBinary { .. }),
            "{receiver}: GameDataBinary is a physical binary frame and cannot use the text ServerMessage envelope"
        );
        self.record_message(receiver, &message);
        message
    }

    /// Decode and record one stamped MessagePack metadata envelope without
    /// interpreting its opaque payload unless it is a generic JSON/MessagePack
    /// ledger fixture.
    pub fn record_binary_frame(&self, receiver: &str, wire: &[u8]) -> RecordedBinaryGameData {
        self.validate_non_message_frame_precondition(receiver, "binary game-data frame");
        let frame = decode_v3_binary_game_data(wire).unwrap_or_else(|error| {
            panic!("{receiver}: invalid MessagePack game-data frame: {error}; bytes={wire:?}")
        });
        self.validate_game_data_shape(
            receiver,
            frame.from_player,
            Some(frame.seq),
            Some(frame.epoch),
            None,
            None,
        );
        self.record_stamp(
            receiver,
            frame.from_player,
            Some(frame.seq),
            Some(frame.epoch),
        );
        self.record_observed_delivery(receiver, DeliveryClass::Reliable);

        let data = match frame.encoding {
            GameDataEncoding::Json => serde_json::from_slice(&frame.payload).ok(),
            GameDataEncoding::MessagePack => rmp_serde::from_slice(&frame.payload).ok(),
            GameDataEncoding::Rkyv => None,
        };
        if let Some(data) = data {
            self.record_ledger_payload(receiver, &data);
        }
        frame
    }

    /// Record one already-decoded server message exactly once.
    pub fn record_message(&self, receiver: &str, message: &ServerMessage) {
        self.validate_message_precondition(receiver, message);
        match message {
            ServerMessage::GameData {
                from_player,
                data,
                seq,
                epoch,
                class,
                key,
            } => {
                self.validate_game_data_shape(receiver, *from_player, *seq, *epoch, *class, *key);
                self.record_stamp(receiver, *from_player, *seq, *epoch);
                self.record_observed_delivery(receiver, class.unwrap_or(DeliveryClass::Reliable));
                self.record_ledger_payload(receiver, data);
            }
            ServerMessage::GameDataBinary {
                from_player,
                seq,
                epoch,
                ..
            } => {
                self.validate_game_data_shape(
                    receiver,
                    *from_player,
                    *seq,
                    *epoch,
                    None,
                    None,
                );
                self.record_stamp(receiver, *from_player, *seq, *epoch);
                self.record_observed_delivery(receiver, DeliveryClass::Reliable);
            }
            ServerMessage::RoomJoined(payload) => {
                self.record_room_snapshot(
                    receiver,
                    &payload.current_players,
                    "RoomJoined",
                    ReceiverRoomState::Player,
                )
            }
            ServerMessage::RoomLeft => {
                self.record_room_exit(receiver, ReceiverRoomState::Player, "RoomLeft")
            }
            ServerMessage::PlayerJoined { player } => {
                self.record_lifecycle_epoch(
                    receiver,
                    player.id,
                    player.epoch,
                    player.seq,
                    "PlayerJoined",
                    PeerLifecycleKind::Joined,
                );
            }
            ServerMessage::PlayerLeft {
                player_id,
                epoch,
                final_seq,
            } => self.record_player_left(receiver, *player_id, *epoch, *final_seq),
            ServerMessage::PlayerReconnected { player_id, epoch } => {
                self.record_lifecycle_epoch(
                    receiver,
                    *player_id,
                    *epoch,
                    epoch.map(|_| 0),
                    "PlayerReconnected",
                    PeerLifecycleKind::Reconnected,
                );
            }
            ServerMessage::Reconnected(_) => panic!(
                "{receiver}: record Reconnected through record_reconnect so the old and new ledger identities cannot be conflated"
            ),
            ServerMessage::SpectatorJoined(payload) => {
                self.record_room_snapshot(
                    receiver,
                    &payload.current_players,
                    "SpectatorJoined",
                    ReceiverRoomState::Spectator,
                );
            }
            ServerMessage::SpectatorLeft { .. } => self.record_room_exit(
                receiver,
                ReceiverRoomState::Spectator,
                "SpectatorLeft",
            ),
            ServerMessage::DeliveryReport(payload) => {
                self.record_delivery_report(receiver, payload)
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
            4000 => {
                assert_eq!(
                    reason, "server_shutdown",
                    "{receiver}: close code 4000 must carry the server_shutdown reason"
                );
                (
                    ReceiverDisconnectCause::ServerShutdown,
                    DisconnectReason::InjectedFault("graceful server shutdown".to_string()),
                )
            }
            4002 => {
                assert_eq!(
                    reason, "slow_consumer",
                    "{receiver}: close code 4002 must carry the slow_consumer reason"
                );
                (
                    ReceiverDisconnectCause::SlowConsumer,
                    DisconnectReason::SlowConsumerEviction,
                )
            }
            4003 => {
                assert_eq!(
                    reason, "activity_timeout",
                    "{receiver}: close code 4003 must carry the activity_timeout reason"
                );
                (
                    ReceiverDisconnectCause::ActivityTimeout,
                    DisconnectReason::InjectedFault(format!(
                        "activity-timeout close {code}: {reason}"
                    )),
                )
            }
            4004 => {
                assert_eq!(
                    reason, "idle_timeout",
                    "{receiver}: close code 4004 must carry the idle_timeout reason"
                );
                (
                    ReceiverDisconnectCause::IdleTimeout,
                    DisconnectReason::InjectedFault(format!("idle-timeout close {code}: {reason}")),
                )
            }
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

    /// Number of sender lifecycle entries retained for one receiver.
    pub fn tracked_sender_count(&self, receiver: &str) -> usize {
        self.state
            .lock()
            .expect("conformance auditor poisoned")
            .receivers
            .get(receiver)
            .map_or(0, |state| state.senders.len())
    }

    /// Record a reconnect across two distinct socket/ledger identities.
    pub fn record_reconnect(
        &self,
        disconnected_receiver: &str,
        reconnected_receiver: &str,
        payload: &ReconnectedPayload,
    ) {
        assert_ne!(
            disconnected_receiver, reconnected_receiver,
            "a reconnected socket must use a fresh auditor/ledger receiver identity"
        );
        {
            let state = self.state.lock().expect("conformance auditor poisoned");
            let previous = state
                .receivers
                .get(disconnected_receiver)
                .unwrap_or_else(|| {
                    panic!("{disconnected_receiver}: reconnect source identity was never recorded")
                });
            assert!(
                matches!(
                    previous.disconnect_cause,
                    Some(
                        ReceiverDisconnectCause::SlowConsumer
                            | ReceiverDisconnectCause::ActivityTimeout
                            | ReceiverDisconnectCause::IdleTimeout
                            | ReceiverDisconnectCause::InjectedFault(_)
                    )
                ),
                "{disconnected_receiver}: disconnect cause {:?} is not reconnect-eligible",
                previous.disconnect_cause
            );
            assert_eq!(
                previous.room_state,
                ReceiverRoomState::Player,
                "{disconnected_receiver}: reconnect source must still be in its player-room lifecycle, got {:?}",
                previous.room_state
            );
            if let Some(next) = state.receivers.get(reconnected_receiver) {
                assert!(
                    receiver_state_is_reconnect_preface(next),
                    "{reconnected_receiver}: reconnect target identity already owns non-preface protocol or ledger state"
                );
            }

            for watermark in &payload.sender_watermarks {
                let Some(sender) = previous.senders.get(&watermark.player_id) else {
                    continue;
                };
                let Some(active_epoch) = sender.active_epoch else {
                    continue;
                };
                assert!(
                    watermark.epoch >= active_epoch,
                    "{reconnected_receiver} <- {}: reconnect watermark epoch {} moved backward from {active_epoch}",
                    watermark.player_id,
                    watermark.epoch
                );
                if watermark.epoch == active_epoch {
                    let last = sender.epochs.get(&active_epoch).copied().unwrap_or(0);
                    assert!(
                        watermark.seq >= last,
                        "{reconnected_receiver} <- {}: reconnect watermark seq {} moved backward from {last} in epoch {active_epoch}",
                        watermark.player_id,
                        watermark.seq
                    );
                }
            }
        }

        self.validate_message_precondition(
            reconnected_receiver,
            &ServerMessage::Reconnected(Box::new(payload.clone())),
        );
        self.record_reconnected(reconnected_receiver, payload);
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
        {
            let state = self.state.lock().expect("conformance auditor poisoned");
            for (receiver, receiver_state) in &state.receivers {
                assert!(
                    !receiver_state.abandoned_requires_disconnect
                        || receiver_state.disconnect_cause.is_some(),
                    "{receiver}: DeliveryReport exposed abandoned deliveries but the stream continued without a terminal disconnect"
                );
            }
        }
        self.ledger
            .assert_zero_loss_or_loud_disconnect(metrics, expectations);
        super::assert_message_conservation(metrics).await;
        assert_delivery_class_metrics_conserve(metrics).await;
    }

    fn assert_receiver_active(receiver: &str, receiver_state: &ReceiverState) {
        assert!(
            receiver_state.disconnect_cause.is_none(),
            "{receiver}: received a frame after terminal disconnect {:?}; reconnects must use a fresh auditor receiver identity",
            receiver_state.disconnect_cause
        );
    }

    fn validate_message_precondition(&self, receiver: &str, message: &ServerMessage) {
        let mut state = self.state.lock().expect("conformance auditor poisoned");
        let mode = *state
            .receiver_modes
            .entry(receiver.to_string())
            .or_insert(self.default_mode);
        let receiver_state = state.receivers.entry(receiver.to_string()).or_default();
        Self::assert_receiver_active(receiver, receiver_state);

        if receiver_state.abandoned_requires_disconnect {
            assert!(
                !receiver_state.abandoned_advisory_seen
                    && matches!(message, ServerMessage::Error { .. }),
                "{receiver}: only a terminal Error advisory may follow a DeliveryReport with abandoned deliveries; got {message:?}"
            );
            receiver_state.abandoned_advisory_seen = true;
        }

        if matches!(
            message,
            ServerMessage::Error {
                error_code: Some(
                    signal_fish_server::protocol::ErrorCode::UnsupportedGameDataFormat
                ),
                ..
            }
        ) {
            assert!(
                receiver_state.unadvised_unsupported_gap.take().is_some(),
                "{receiver}: unsupported-format Error lacked a prior causal DeliveryReport"
            );
        }

        if mode == ReceiverProtocolMode::V2 {
            assert!(
                !matches!(message, ServerMessage::DeliveryReport(_) | ServerMessage::RelayStats { .. }),
                "{receiver}: v2 receiver observed a protocol-v3 accountability message: {message:?}"
            );
        }
    }

    fn validate_non_message_frame_precondition(&self, receiver: &str, frame: &str) {
        let mut state = self.state.lock().expect("conformance auditor poisoned");
        state
            .receiver_modes
            .entry(receiver.to_string())
            .or_insert(self.default_mode);
        let receiver_state = state.receivers.entry(receiver.to_string()).or_default();
        Self::assert_receiver_active(receiver, receiver_state);
        assert!(
            !receiver_state.abandoned_requires_disconnect,
            "{receiver}: {frame} continued after DeliveryReport exposed abandoned deliveries"
        );
    }

    fn record_observed_delivery(&self, receiver: &str, class: DeliveryClass) {
        let mut state = self.state.lock().expect("conformance auditor poisoned");
        let receiver_state = state.receivers.entry(receiver.to_string()).or_default();
        let delivered = match class {
            DeliveryClass::Reliable => &mut receiver_state.observed_delivered.reliable.delivered,
            DeliveryClass::Latest => &mut receiver_state.observed_delivered.latest.delivered,
            DeliveryClass::Volatile => &mut receiver_state.observed_delivered.volatile.delivered,
        };
        *delivered = delivered
            .checked_add(1)
            .expect("observed delivery counter overflowed");
    }

    fn validate_game_data_shape(
        &self,
        receiver: &str,
        sender: PlayerId,
        seq: Option<u64>,
        epoch: Option<u32>,
        class: Option<DeliveryClass>,
        key: Option<u32>,
    ) {
        match self.receiver_mode(receiver) {
            ReceiverProtocolMode::V2 => assert!(
                seq.is_none() && epoch.is_none() && class.is_none() && key.is_none(),
                "{receiver} <- {sender}: v2 GameData leaked v3 fields (seq={seq:?}, epoch={epoch:?}, class={class:?}, key={key:?})"
            ),
            ReceiverProtocolMode::V3 => {
                assert!(
                    seq.is_some() && epoch.is_some(),
                    "{receiver} <- {sender}: v3 GameData must carry seq and epoch (seq={seq:?}, epoch={epoch:?})"
                );
                let valid_class_key = matches!(
                    (class, key),
                    (None, None)
                        | (Some(DeliveryClass::Reliable), None)
                        | (Some(DeliveryClass::Latest), Some(_))
                        | (Some(DeliveryClass::Volatile), None)
                );
                assert!(
                    valid_class_key,
                    "{receiver} <- {sender}: invalid v3 delivery class/key shape (class={class:?}, key={key:?})"
                );
            }
        }
    }

    fn validate_lifecycle_stamp_shape(
        &self,
        receiver: &str,
        sender: PlayerId,
        epoch: Option<u32>,
        seq: Option<u64>,
        source: &str,
    ) {
        match (self.receiver_mode(receiver), epoch, seq) {
            (ReceiverProtocolMode::V2, None, None) => {}
            (ReceiverProtocolMode::V2, epoch, seq) => {
                panic!(
                    "{receiver} <- {sender}: v2 {source} leaked v3 baseline ({epoch:?}, {seq:?})"
                )
            }
            (ReceiverProtocolMode::V3, Some(epoch), Some(_)) if epoch > 0 => {}
            (ReceiverProtocolMode::V3, epoch, seq) => {
                panic!(
                    "{receiver} <- {sender}: v3 {source} has invalid baseline ({epoch:?}, {seq:?})"
                )
            }
        }
    }

    fn record_disconnect(
        &self,
        receiver: &str,
        cause: ReceiverDisconnectCause,
        ledger_reason: DisconnectReason,
    ) {
        {
            let mut state = self.state.lock().expect("conformance auditor poisoned");
            state
                .receiver_modes
                .entry(receiver.to_string())
                .or_insert(self.default_mode);
            let receiver_state = state.receivers.entry(receiver.to_string()).or_default();
            Self::assert_receiver_active(receiver, receiver_state);
            // The report is the causal accountability record. The supplemental
            // Error is optional and may be absent when rate-limited or when
            // its write is overtaken by terminal disconnect.
            receiver_state.unadvised_unsupported_gap = None;
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
        {
            let mut state = self.state.lock().expect("conformance auditor poisoned");
            let receiver_state = state.receivers.entry(receiver.to_string()).or_default();
            receiver_state.ledger_payloads = receiver_state
                .ledger_payloads
                .checked_add(1)
                .expect("conformance ledger payload counter overflowed");
        }
        // Protocol seq is per sender, not per receiver, so it must not be fed
        // to DeliveryLedger's per-receiver hook.
        self.ledger.record(receiver, &sender, payload_seq, None);
    }

    fn record_room_snapshot(
        &self,
        receiver: &str,
        current_players: &[PlayerInfo],
        source: &str,
        next_room_state: ReceiverRoomState,
    ) {
        let mode = self.receiver_mode(receiver);
        let mut state = self.state.lock().expect("conformance auditor poisoned");
        let receiver_state = state.receivers.entry(receiver.to_string()).or_default();
        assert_eq!(
            receiver_state.room_state,
            ReceiverRoomState::Outside,
            "{receiver}: duplicate/illegal {source} while in {:?} state",
            receiver_state.room_state
        );
        receiver_state.room_state = next_room_state;
        receiver_state.had_room_lifecycle = true;

        // A room/spectator snapshot starts a new receiver-local view with an
        // exact per-sender relay baseline. No cause from the prior view may
        // cross the boundary.
        receiver_state.senders.clear();
        receiver_state.membership_history.clear();
        receiver_state.pending_gaps.clear();
        receiver_state.unadvised_unsupported_gap = None;
        for player in current_players {
            assert!(
                receiver_state.membership_history.insert(player.id),
                "{receiver}: {source} snapshot contains duplicate player {}",
                player.id
            );
            match (mode, player.epoch, player.seq) {
                (ReceiverProtocolMode::V2, None, None) => {
                    let sender = SenderState {
                        present: true,
                        known_member: true,
                        ..SenderState::default()
                    };
                    assert!(
                        receiver_state.senders.insert(player.id, sender).is_none(),
                        "{receiver}: {source} snapshot contains duplicate player {}",
                        player.id
                    );
                }
                (ReceiverProtocolMode::V2, epoch, seq) => panic!(
                    "{receiver} <- {}: v2 {source} leaked v3 baseline ({epoch:?}, {seq:?})",
                    player.id
                ),
                (ReceiverProtocolMode::V3, None, None) => panic!(
                    "{receiver} <- {}: v3 {source} omitted sender epoch/seq baseline",
                    player.id
                ),
                (ReceiverProtocolMode::V3, None, Some(_))
                | (ReceiverProtocolMode::V3, Some(_), None) => panic!(
                    "{receiver} <- {}: v3 {source} must carry epoch and seq together",
                    player.id
                ),
                (ReceiverProtocolMode::V3, Some(0), Some(_)) => {
                    panic!("{receiver} <- {}: {source} advertised epoch 0", player.id)
                }
                (ReceiverProtocolMode::V3, Some(epoch), Some(seq)) => {
                    let mut sender = SenderState::default();
                    sender.announced_epochs.insert(epoch);
                    sender.latest_lifecycle_epoch = Some(epoch);
                    sender.active_epoch = Some(epoch);
                    sender.epochs.insert(epoch, seq);
                    sender.present = true;
                    sender.known_member = true;
                    assert!(
                        receiver_state.senders.insert(player.id, sender).is_none(),
                        "{receiver}: {source} snapshot contains duplicate player {}",
                        player.id
                    );
                }
            }
        }
    }

    fn record_room_exit(&self, receiver: &str, expected_state: ReceiverRoomState, source: &str) {
        let mut state = self.state.lock().expect("conformance auditor poisoned");
        let receiver_state = state.receivers.entry(receiver.to_string()).or_default();
        assert_eq!(
            receiver_state.room_state, expected_state,
            "{receiver}: duplicate/illegal {source} while in {:?} state",
            receiver_state.room_state
        );
        receiver_state.room_state = ReceiverRoomState::Outside;
        receiver_state.senders.clear();
        receiver_state.membership_history.clear();
        receiver_state.pending_gaps.clear();
        receiver_state.unadvised_unsupported_gap = None;
    }

    fn record_lifecycle_epoch(
        &self,
        receiver: &str,
        sender: PlayerId,
        epoch: Option<u32>,
        seq: Option<u64>,
        source: &str,
        kind: PeerLifecycleKind,
    ) {
        let mode = self.receiver_mode(receiver);
        let mut state = self.state.lock().expect("conformance auditor poisoned");
        let receiver_state = state.receivers.entry(receiver.to_string()).or_default();
        assert!(
            receiver_state.room_state != ReceiverRoomState::Outside,
            "{receiver} <- {sender}: {source} arrived while receiver was outside a room"
        );

        let stamp = match (mode, epoch, seq) {
            (ReceiverProtocolMode::V2, None, None) => None,
            (ReceiverProtocolMode::V2, epoch, seq) => {
                panic!(
                    "{receiver} <- {sender}: v2 {source} leaked v3 baseline ({epoch:?}, {seq:?})"
                )
            }
            (ReceiverProtocolMode::V3, None, None) => {
                panic!("{receiver} <- {sender}: v3 {source} omitted sender epoch/seq baseline")
            }
            (ReceiverProtocolMode::V3, None, Some(_))
            | (ReceiverProtocolMode::V3, Some(_), None) => {
                panic!("{receiver} <- {sender}: v3 {source} must carry epoch and seq together")
            }
            (ReceiverProtocolMode::V3, Some(0), Some(_)) => {
                panic!("{receiver} <- {sender}: {source} advertised epoch 0")
            }
            (ReceiverProtocolMode::V3, Some(epoch), Some(seq)) => Some((epoch, seq)),
        };
        let epoch = stamp.map(|(epoch, _)| epoch);

        let sender_state = match kind {
            PeerLifecycleKind::Joined => {
                receiver_state.membership_history.insert(sender);
                receiver_state.senders.entry(sender).or_default()
            }
            PeerLifecycleKind::Reconnected => {
                assert!(
                    receiver_state.membership_history.contains(&sender),
                    "{receiver} <- {sender}: PlayerReconnected arrived before membership"
                );
                receiver_state.senders.entry(sender).or_default()
            }
        };
        if mode == ReceiverProtocolMode::V2 && sender_state.present {
            return;
        }
        if let Some(epoch) = epoch {
            if let Some(previous_epoch) = sender_state.latest_lifecycle_epoch {
                if sender_state.present && epoch == previous_epoch {
                    return;
                }
                assert!(
                    epoch > previous_epoch,
                    "{receiver} <- {sender}: {source} advertised stale/duplicate epoch {epoch} after epoch {previous_epoch}; same-epoch overlap is valid only while the peer remains present"
                );
            }
        }
        if kind == PeerLifecycleKind::Joined {
            assert!(
                !sender_state.present,
                "{receiver} <- {sender}: duplicate PlayerJoined while peer is already present"
            );
        }
        if let Some(epoch) = epoch {
            sender_state.announced_epochs.insert(epoch);
            sender_state.latest_lifecycle_epoch = Some(epoch);
            if sender_state.active_epoch.is_none() {
                let seq = stamp.expect("v3 lifecycle stamp validated above").1;
                sender_state.active_epoch = Some(epoch);
                sender_state.epochs.insert(epoch, seq);
            }
        }
        sender_state.present = true;
        sender_state.known_member = true;
    }

    fn record_player_left(
        &self,
        receiver: &str,
        sender: PlayerId,
        epoch: Option<u32>,
        final_seq: Option<u64>,
    ) {
        let mode = self.receiver_mode(receiver);
        let mut state = self.state.lock().expect("conformance auditor poisoned");
        let receiver_state = state.receivers.entry(receiver.to_string()).or_default();
        assert!(
            receiver_state.room_state != ReceiverRoomState::Outside,
            "{receiver} <- {sender}: PlayerLeft arrived while receiver was outside a room"
        );
        let sender_state = receiver_state.senders.get_mut(&sender).unwrap_or_else(|| {
            panic!("{receiver} <- {sender}: PlayerLeft arrived before membership")
        });
        assert!(
            sender_state.known_member,
            "{receiver} <- {sender}: PlayerLeft arrived without sender membership"
        );
        let terminal_epoch = match (mode, epoch, final_seq) {
            (ReceiverProtocolMode::V2, None, None) => None,
            (ReceiverProtocolMode::V2, _, _) => {
                panic!("{receiver} <- {sender}: v2 PlayerLeft leaked terminal watermark fields")
            }
            (ReceiverProtocolMode::V3, Some(epoch), Some(final_seq)) => {
                assert!(
                    epoch > 0,
                    "{receiver} <- {sender}: PlayerLeft epoch must be positive"
                );
                assert!(
                    sender_state.announced_epochs.contains(&epoch),
                    "{receiver} <- {sender}: PlayerLeft terminal epoch {epoch} was never announced"
                );
                let observed = sender_state.epochs.get(&epoch).copied().unwrap_or(0);
                assert!(
                    final_seq >= observed,
                    "{receiver} <- {sender}: PlayerLeft final_seq {final_seq} moved backward from observed {observed} in epoch {epoch}"
                );
                if let Some(existing) = sender_state.terminals.get(&epoch) {
                    assert_eq!(
                        final_seq, *existing,
                        "{receiver} <- {sender}: PlayerLeft terminal watermark changed in epoch {epoch}"
                    );
                }
                assert!(
                    sender_state
                        .terminals
                        .keys()
                        .all(|terminal_epoch| *terminal_epoch <= epoch),
                    "{receiver} <- {sender}: PlayerLeft terminal epoch {epoch} arrived after a newer leave"
                );
                if let Some(ranges) = receiver_state.pending_gaps.get(&(sender, epoch)) {
                    assert!(
                        ranges.values().all(|gap| gap.to_seq <= final_seq),
                        "{receiver} <- {sender}: prior gap extends beyond PlayerLeft final_seq {final_seq}"
                    );
                }
                sender_state.terminals.insert(epoch, final_seq);
                Some(epoch)
            }
            (ReceiverProtocolMode::V3, _, _) => {
                panic!("{receiver} <- {sender}: v3 PlayerLeft omitted epoch/final_seq")
            }
        };
        sender_state.present = false;
        if let Some(epoch) = terminal_epoch {
            Self::try_retire_sender(receiver, receiver_state, sender, epoch);
        }
    }

    fn record_reconnected(&self, receiver: &str, payload: &ReconnectedPayload) {
        let mode = self.receiver_mode(receiver);
        match (mode, payload.replay) {
            (ReceiverProtocolMode::V2, None) | (ReceiverProtocolMode::V3, Some(_)) => {}
            (ReceiverProtocolMode::V2, Some(replay)) => {
                panic!("{receiver}: v2 Reconnected leaked v3 replay marker {replay:?}")
            }
            (ReceiverProtocolMode::V3, None) => {
                panic!("{receiver}: v3 Reconnected omitted replay completeness marker")
            }
        }
        for event in &payload.missed_events {
            match event {
                ServerMessage::GameData { .. } | ServerMessage::GameDataBinary { .. } => {
                    panic!("{receiver}: Reconnected.missed_events must never replay game data")
                }
                ServerMessage::PlayerJoined { player } => self.validate_lifecycle_stamp_shape(
                    receiver,
                    player.id,
                    player.epoch,
                    player.seq,
                    "Reconnected.missed_events.PlayerJoined",
                ),
                ServerMessage::PlayerReconnected { player_id, epoch } => self
                    .validate_lifecycle_stamp_shape(
                        receiver,
                        *player_id,
                        *epoch,
                        epoch.map(|_| 0),
                        "Reconnected.missed_events.PlayerReconnected",
                    ),
                _ => {}
            }
        }

        let mut player_stamps = BTreeMap::new();
        let mut snapshot_player_ids = BTreeSet::new();
        for player in &payload.current_players {
            assert!(
                snapshot_player_ids.insert(player.id),
                "{receiver}: Reconnected snapshot contains duplicate player {}",
                player.id
            );
            let stamp = match (mode, player.epoch, player.seq) {
                (ReceiverProtocolMode::V2, None, None) => continue,
                (ReceiverProtocolMode::V2, epoch, seq) => panic!(
                    "{receiver} <- {}: v2 Reconnected snapshot leaked v3 baseline ({epoch:?}, {seq:?})",
                    player.id
                ),
                (ReceiverProtocolMode::V3, None, None) => panic!(
                    "{receiver} <- {}: v3 Reconnected snapshot omitted sender epoch/seq baseline",
                    player.id
                ),
                (ReceiverProtocolMode::V3, None, Some(_))
                | (ReceiverProtocolMode::V3, Some(_), None) => panic!(
                    "{receiver} <- {}: v3 Reconnected snapshot must carry epoch and seq together",
                    player.id
                ),
                (ReceiverProtocolMode::V3, Some(0), Some(_)) => panic!(
                    "{receiver} <- {}: Reconnected snapshot advertised epoch 0",
                    player.id
                ),
                (ReceiverProtocolMode::V3, Some(epoch), Some(seq)) => (epoch, seq),
            };
            player_stamps.insert(player.id, stamp);
        }

        if mode == ReceiverProtocolMode::V2 {
            assert!(
                payload.sender_watermarks.is_empty(),
                "{receiver}: v2 Reconnected leaked v3 sender watermarks"
            );
        }

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
            let snapshot_stamp = player_stamps.get(&watermark.player_id).unwrap_or_else(|| {
                panic!(
                    "{receiver}: reconnect watermark names player {} absent from the snapshot",
                    watermark.player_id
                )
            });
            assert_eq!(
                (watermark.epoch, watermark.seq),
                *snapshot_stamp,
                "{receiver}: reconnect watermark/snapshot stamp mismatch for {}",
                watermark.player_id
            );
        }

        if mode == ReceiverProtocolMode::V3 {
            assert_eq!(
                seen, snapshot_player_ids,
                "{receiver}: reconnect watermarks must cover every current player exactly once"
            );
        }

        let mut state = self.state.lock().expect("conformance auditor poisoned");
        let receiver_state = state.receivers.entry(receiver.to_string()).or_default();
        assert!(
            receiver_state_is_reconnect_preface(receiver_state),
            "{receiver}: Reconnected must initialize a fresh socket/ledger identity after only an optional zero-valued preface"
        );
        receiver_state.room_state = ReceiverRoomState::Player;
        receiver_state.had_room_lifecycle = true;

        // Reconnected is a new socket/outbound queue. Initialize its
        // authoritative snapshot and per-sender baselines from scratch.

        for player in &payload.current_players {
            receiver_state.membership_history.insert(player.id);
            if let (Some(epoch), Some(seq)) = (player.epoch, player.seq) {
                let mut sender = SenderState::default();
                sender.announced_epochs.insert(epoch);
                sender.latest_lifecycle_epoch = Some(epoch);
                sender.active_epoch = Some(epoch);
                sender.epochs.insert(epoch, seq);
                sender.present = true;
                sender.known_member = true;
                receiver_state.senders.insert(player.id, sender);
            } else {
                receiver_state.senders.insert(
                    player.id,
                    SenderState {
                        present: true,
                        known_member: true,
                        ..SenderState::default()
                    },
                );
            }
        }

        for watermark in &payload.sender_watermarks {
            receiver_state
                .membership_history
                .insert(watermark.player_id);
            let sender_state = receiver_state
                .senders
                .entry(watermark.player_id)
                .or_default();
            sender_state.active_epoch = Some(watermark.epoch);
            sender_state.epochs.insert(watermark.epoch, watermark.seq);
            sender_state.announced_epochs.insert(watermark.epoch);
            sender_state.latest_lifecycle_epoch = Some(watermark.epoch);
            sender_state.present = true;
            sender_state.known_member = true;
        }
    }

    fn record_delivery_report(&self, receiver: &str, report: &DeliveryReportPayload) {
        assert!(
            report.gaps.len() <= DELIVERY_REPORT_MAX_GAPS,
            "{receiver}: DeliveryReport carried {} gaps, exceeding canonical maximum {DELIVERY_REPORT_MAX_GAPS}",
            report.gaps.len()
        );
        let mut state = self.state.lock().expect("conformance auditor poisoned");
        let receiver_state = state.receivers.entry(receiver.to_string()).or_default();
        let previous = receiver_state.last_delivery_counters.unwrap_or_default();

        for (class, reported, observed) in [
            (
                "reliable",
                report.per_class.reliable.delivered,
                receiver_state.observed_delivered.reliable.delivered,
            ),
            (
                "latest",
                report.per_class.latest.delivered,
                receiver_state.observed_delivered.latest.delivered,
            ),
            (
                "volatile",
                report.per_class.volatile.delivered,
                receiver_state.observed_delivered.volatile.delivered,
            ),
        ] {
            assert!(
                reported <= observed,
                "{receiver}: DeliveryReport {class}.delivered={reported} exceeds {observed} classed GameData frame(s) already written on this physical receiver stream"
            );
        }

        for ((name, previous), (_, next)) in delivery_counter_values(&previous)
            .into_iter()
            .zip(delivery_counter_values(&report.per_class))
        {
            assert!(
                next >= previous,
                "{receiver}: cumulative DeliveryReport counter `{name}` moved backward ({previous} -> {next})"
            );
        }

        let mut causal_gap_counts = [0u64; 4];
        let mut unsupported_gap = None;
        for gap in &report.gaps {
            Self::validate_and_record_gap(receiver, receiver_state, gap);
            let count = gap
                .to_seq
                .checked_sub(gap.from_seq)
                .and_then(|length| length.checked_add(1))
                .expect("validated DeliveryReport range length overflowed");
            let index = match gap.reason {
                DeliveryGapReason::LatestSuperseded => 0,
                DeliveryGapReason::LatestDroppedFull => 1,
                DeliveryGapReason::VolatileDropped => 2,
                // Undeliverable payloads are reported as coalesced ranges, so a
                // report may carry several of them (issue #212): one frame per
                // omitted message cost the recipient least able to afford it
                // ~5.4x the bytes of the payload it replaced. Exactness is
                // enforced by `validate_and_record_gap` (no overlap, no hole)
                // and by the counter-delta assertions below, not by frame count.
                DeliveryGapReason::UnsupportedFormat => {
                    unsupported_gap = Some(gap.clone());
                    3
                }
            };
            causal_gap_counts[index] = causal_gap_counts[index]
                .checked_add(count)
                .expect("DeliveryReport causal gap count overflowed");
        }

        let delta = |next: u64, prior: u64| next - prior;
        assert_eq!(
            delta(
                report.per_class.latest.superseded,
                previous.latest.superseded,
            ),
            causal_gap_counts[0],
            "{receiver}: latest.superseded counter delta must equal newly reported exact gaps"
        );
        assert_eq!(
            delta(
                report.per_class.latest.dropped_full,
                previous.latest.dropped_full,
            ),
            causal_gap_counts[1],
            "{receiver}: latest.dropped_full counter delta must equal newly reported exact gaps"
        );
        assert_eq!(
            delta(report.per_class.volatile.dropped, previous.volatile.dropped,),
            causal_gap_counts[2],
            "{receiver}: volatile.dropped counter delta must equal newly reported exact gaps"
        );
        let unsupported_delta = delta(
            report.per_class.reliable.unsupported_format,
            previous.reliable.unsupported_format,
        )
        .checked_add(delta(
            report.per_class.latest.unsupported_format,
            previous.latest.unsupported_format,
        ))
        .and_then(|sum| {
            sum.checked_add(delta(
                report.per_class.volatile.unsupported_format,
                previous.volatile.unsupported_format,
            ))
        })
        .expect("DeliveryReport unsupported-format delta overflowed");
        assert_eq!(
            unsupported_delta, causal_gap_counts[3],
            "{receiver}: unsupported-format counter delta must equal newly reported exact gaps"
        );

        if unsupported_gap.is_some() {
            receiver_state.unadvised_unsupported_gap = unsupported_gap;
        }
        receiver_state.abandoned_requires_disconnect = report.per_class.reliable.abandoned > 0
            || report.per_class.latest.abandoned > 0
            || report.per_class.volatile.abandoned > 0;
        receiver_state.last_delivery_counters = Some(report.per_class);
        for gap in &report.gaps {
            Self::try_retire_sender(receiver, receiver_state, gap.from_player, gap.epoch);
        }
    }

    fn validate_and_record_gap(
        receiver: &str,
        receiver_state: &mut ReceiverState,
        gap: &DeliveryGap,
    ) {
        assert!(
            gap.epoch > 0,
            "{receiver} <- {}: DeliveryReport gap has epoch 0",
            gap.from_player
        );
        assert!(
            gap.from_seq > 0 && gap.from_seq <= gap.to_seq,
            "{receiver} <- {}: invalid DeliveryReport range [{}..={}] in epoch {}",
            gap.from_player,
            gap.from_seq,
            gap.to_seq,
            gap.epoch
        );

        let sender_state = receiver_state
            .senders
            .get(&gap.from_player)
            .unwrap_or_else(|| {
                panic!(
                    "{receiver} <- {}: DeliveryReport names a sender outside the current room view",
                    gap.from_player
                )
            });
        assert!(
            sender_state.announced_epochs.contains(&gap.epoch),
            "{receiver} <- {}: DeliveryReport epoch {} was never announced by a lifecycle message",
            gap.from_player,
            gap.epoch
        );
        if let Some(final_seq) = sender_state.terminals.get(&gap.epoch) {
            assert!(
                gap.to_seq <= *final_seq,
                "{receiver} <- {}: DeliveryReport range [{}..={}] extends beyond PlayerLeft terminal ({}, {})",
                gap.from_player,
                gap.from_seq,
                gap.to_seq,
                gap.epoch,
                final_seq
            );
        }
        if let Some(active_epoch) = sender_state.active_epoch {
            assert!(
                gap.epoch >= active_epoch,
                "{receiver} <- {}: DeliveryReport epoch {} arrived after data advanced to epoch {active_epoch}",
                gap.from_player,
                gap.epoch
            );
        }
        let last_observed = sender_state.epochs.get(&gap.epoch).copied().unwrap_or(0);
        assert!(
            gap.from_seq > last_observed,
            "{receiver} <- {}: late DeliveryReport range [{}..={}] overlaps observed tail {last_observed} in epoch {}",
            gap.from_player,
            gap.from_seq,
            gap.to_seq,
            gap.epoch
        );

        let ranges = receiver_state
            .pending_gaps
            .entry((gap.from_player, gap.epoch))
            .or_default();
        if let Some((from_seq, previous)) = ranges.range(..=gap.from_seq).next_back() {
            assert!(
                previous.to_seq < gap.from_seq,
                "{receiver} <- {}: overlapping/duplicate DeliveryReport ranges [{}..={}] ({:?}) and [{}..={}] ({:?}) in epoch {}",
                gap.from_player,
                from_seq,
                previous.to_seq,
                previous.reason,
                gap.from_seq,
                gap.to_seq,
                gap.reason,
                gap.epoch
            );
        }
        if let Some((next_from, next)) = ranges.range(gap.from_seq..).next() {
            assert!(
                gap.to_seq < *next_from,
                "{receiver} <- {}: overlapping/duplicate DeliveryReport ranges [{}..={}] ({:?}) and [{}..={}] ({:?}) in epoch {}",
                gap.from_player,
                gap.from_seq,
                gap.to_seq,
                gap.reason,
                next_from,
                next.to_seq,
                next.reason,
                gap.epoch
            );
        }
        ranges.insert(
            gap.from_seq,
            PendingGap {
                to_seq: gap.to_seq,
                reason: gap.reason,
            },
        );
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
        let mode = self.receiver_mode(receiver);
        let mut state = self.state.lock().expect("conformance auditor poisoned");
        let receiver_state = state.receivers.entry(receiver.to_string()).or_default();
        Self::assert_receiver_active(receiver, receiver_state);
        assert!(
            receiver_state.room_state != ReceiverRoomState::Outside,
            "{receiver} <- {sender}: GameData arrived while receiver was outside a room"
        );
        let sender_state = receiver_state.senders.get(&sender).unwrap_or_else(|| {
            panic!("{receiver} <- {sender}: GameData arrived before sender membership")
        });
        assert!(
            sender_state.known_member,
            "{receiver} <- {sender}: GameData followed only an idempotent PlayerLeft tombstone, not sender membership"
        );

        let (seq, epoch) = match (mode, seq, epoch) {
            (ReceiverProtocolMode::V2, None, None) => {
                let _ = sender_state;
                return;
            }
            (ReceiverProtocolMode::V3, Some(seq), Some(epoch)) => (seq, epoch),
            (ReceiverProtocolMode::V2, _, _) => panic!(
                "{receiver} <- {sender}: v2 GameData leaked a delivery stamp (seq={seq:?}, epoch={epoch:?})"
            ),
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

        if let Some(final_seq) = receiver_state
            .senders
            .get(&sender)
            .and_then(|state| state.terminals.get(&epoch))
        {
            assert!(
                seq <= *final_seq,
                "{receiver} <- {sender}: GameData ({epoch}, {seq}) advanced beyond PlayerLeft terminal ({epoch}, {final_seq})"
            );
        }

        let active_epoch = receiver_state
            .senders
            .get(&sender)
            .and_then(|state| state.active_epoch);
        if active_epoch.is_some_and(|active| epoch > active) {
            let older_terminals: Vec<_> = receiver_state
                .senders
                .get(&sender)
                .expect("sender checked above")
                .terminals
                .keys()
                .copied()
                .filter(|terminal_epoch| *terminal_epoch < epoch)
                .collect();
            for terminal_epoch in older_terminals {
                Self::try_retire_sender(receiver, receiver_state, sender, terminal_epoch);
            }
            assert!(
                receiver_state
                    .senders
                    .get(&sender)
                    .is_none_or(|state| state.terminals.keys().all(|value| *value >= epoch)),
                "{receiver} <- {sender}: GameData advanced to epoch {epoch} before older PlayerLeft tails retired"
            );
        }

        let sender_state = receiver_state
            .senders
            .get(&sender)
            .expect("sender retained across terminal checks");

        let key = (sender, epoch);
        let expected = {
            assert!(
                sender_state.announced_epochs.contains(&epoch),
                "{receiver} <- {sender}: GameData epoch {epoch} was never announced by a lifecycle message"
            );

            match sender_state.active_epoch {
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
                    last.checked_add(1).unwrap_or_else(|| {
                        panic!(
                            "{receiver} <- {sender}: sequence advanced past u64::MAX in epoch {epoch}"
                        )
                    })
                }
                Some(_) => 1,
                None => 1,
            }
        };

        if seq > expected {
            Self::consume_exact_gap(receiver, receiver_state, key, expected, seq);
        } else {
            Self::assert_stamp_not_reported(receiver, receiver_state, key, seq);
        }

        let sender_state = receiver_state
            .senders
            .get_mut(&sender)
            .expect("sender state was initialized above");
        sender_state.active_epoch = Some(epoch);
        sender_state.epochs.insert(epoch, seq);
        Self::try_retire_sender(receiver, receiver_state, sender, epoch);
    }

    fn assert_stamp_not_reported(
        receiver: &str,
        receiver_state: &ReceiverState,
        (sender, epoch): (PlayerId, u32),
        seq: u64,
    ) {
        let Some(ranges) = receiver_state.pending_gaps.get(&(sender, epoch)) else {
            return;
        };
        if let Some((from_seq, gap)) = ranges.range(..=seq).next_back() {
            assert!(
                gap.to_seq < seq,
                "{receiver} <- {sender}: delivered seq {seq} in epoch {epoch}, but prior DeliveryReport declared it omitted in range [{}..={}] ({:?})",
                from_seq,
                gap.to_seq,
                gap.reason
            );
        }
    }

    /// Consume a union of prior exact ranges covering `[expected, observed)`.
    /// Ranges may be adjacent and carry different causes, but may not leave a
    /// hole or extend over the sequence that was actually delivered.
    fn consume_exact_gap(
        receiver: &str,
        receiver_state: &mut ReceiverState,
        (sender, epoch): (PlayerId, u32),
        expected: u64,
        observed: u64,
    ) {
        let ranges = receiver_state
            .pending_gaps
            .get(&(sender, epoch))
            .unwrap_or_else(|| {
                panic!(
                    "{receiver} <- {sender}: unexplained seq gap in epoch {epoch}: expected {expected}, got {observed}"
                )
            });

        let mut cursor = expected;
        let mut consumed = Vec::new();
        while cursor < observed {
            let (from_seq, gap) = ranges.range(cursor..).next().unwrap_or_else(|| {
                panic!(
                    "{receiver} <- {sender}: DeliveryReport ranges stop before seq {cursor} in epoch {epoch} (observed {observed})"
                )
            });
            assert_eq!(
                *from_seq, cursor,
                "{receiver} <- {sender}: DeliveryReport leaves an unexplained hole [{}..{}) in epoch {epoch}",
                cursor, from_seq
            );
            assert!(
                gap.to_seq < observed,
                "{receiver} <- {sender}: DeliveryReport range [{}..={}] ({:?}) claims delivered seq {observed} was omitted in epoch {epoch}",
                from_seq,
                gap.to_seq,
                gap.reason
            );
            consumed.push(*from_seq);
            cursor = gap
                .to_seq
                .checked_add(1)
                .expect("reported range ended at u64::MAX");
        }

        let ranges = receiver_state
            .pending_gaps
            .get_mut(&(sender, epoch))
            .expect("ranges validated above");
        for from_seq in consumed {
            ranges.remove(&from_seq);
        }
        if ranges.is_empty() {
            receiver_state.pending_gaps.remove(&(sender, epoch));
        }
    }

    fn try_retire_sender(
        receiver: &str,
        receiver_state: &mut ReceiverState,
        sender: PlayerId,
        epoch: u32,
    ) {
        let Some(sender_state) = receiver_state.senders.get(&sender) else {
            return;
        };
        let Some(final_seq) = sender_state.terminals.get(&epoch).copied() else {
            return;
        };

        let mut next = if final_seq == 0 {
            1
        } else {
            match sender_state.active_epoch {
                Some(active_epoch) if active_epoch < epoch => 1,
                Some(active_epoch) if active_epoch == epoch => {
                    let last = sender_state.epochs.get(&epoch).copied().unwrap_or(0);
                    if last >= final_seq {
                        Self::retire_sender(receiver_state, sender, epoch);
                        return;
                    }
                    last + 1
                }
                Some(active_epoch) => panic!(
                    "{receiver} <- {sender}: sender advanced to epoch {active_epoch} before PlayerLeft terminal epoch {epoch} was resolved"
                ),
                None => 1,
            }
        };

        let ranges = receiver_state.pending_gaps.get(&(sender, epoch));
        let mut consumed = Vec::new();
        let mut covered = final_seq == 0;
        while next <= final_seq {
            let Some((from_seq, gap)) = ranges.and_then(|ranges| ranges.range(next..).next())
            else {
                return;
            };
            if *from_seq != next || gap.to_seq > final_seq {
                return;
            }
            consumed.push(*from_seq);
            if gap.to_seq == final_seq {
                covered = true;
                break;
            }
            next = gap.to_seq + 1;
        }
        if !covered {
            return;
        }
        if !consumed.is_empty() {
            let remove_key = {
                let ranges = receiver_state
                    .pending_gaps
                    .get_mut(&(sender, epoch))
                    .expect("ranges read above");
                for from_seq in consumed {
                    ranges.remove(&from_seq);
                }
                ranges.is_empty()
            };
            if remove_key {
                receiver_state.pending_gaps.remove(&(sender, epoch));
            }
        }
        Self::retire_sender(receiver_state, sender, epoch);
    }

    fn retire_sender(receiver_state: &mut ReceiverState, sender: PlayerId, epoch: u32) {
        receiver_state.pending_gaps.remove(&(sender, epoch));
        let sender_state = receiver_state
            .senders
            .get_mut(&sender)
            .expect("terminal sender exists");
        sender_state.terminals.remove(&epoch);
        sender_state.announced_epochs.remove(&epoch);
        if !sender_state.present && sender_state.terminals.is_empty() {
            receiver_state.senders.remove(&sender);
        }
    }
}

fn delivery_counter_values(counters: &DeliveryCountersByClass) -> [(&'static str, u64); 12] {
    [
        ("reliable.delivered", counters.reliable.delivered),
        ("reliable.abandoned", counters.reliable.abandoned),
        (
            "reliable.unsupported_format",
            counters.reliable.unsupported_format,
        ),
        ("latest.delivered", counters.latest.delivered),
        ("latest.superseded", counters.latest.superseded),
        ("latest.dropped_full", counters.latest.dropped_full),
        ("latest.abandoned", counters.latest.abandoned),
        (
            "latest.unsupported_format",
            counters.latest.unsupported_format,
        ),
        ("volatile.delivered", counters.volatile.delivered),
        ("volatile.dropped", counters.volatile.dropped),
        ("volatile.abandoned", counters.volatile.abandoned),
        (
            "volatile.unsupported_format",
            counters.volatile.unsupported_format,
        ),
    ]
}

fn receiver_state_is_reconnect_preface(state: &ReceiverState) -> bool {
    state.room_state == ReceiverRoomState::Outside
        && state.senders.is_empty()
        && state.pending_gaps.is_empty()
        && state
            .last_delivery_counters
            .is_none_or(|counters| counters == DeliveryCountersByClass::default())
        && state.disconnect_cause.is_none()
        && state.last_relay_stats.is_none_or(|stats| {
            stats.sent_to_you == 0 && stats.dropped_for_you == 0 && stats.backpressure_events == 0
        })
        && state.unadvised_unsupported_gap.is_none()
        && state.ledger_payloads == 0
        && state.observed_delivered == DeliveryCountersByClass::default()
        && !state.abandoned_requires_disconnect
        && !state.abandoned_advisory_seen
        && !state.had_room_lifecycle
}

/// Assert the exact server-wide conservation law for every delivery class.
pub fn assert_delivery_class_snapshot_conserves(snapshot: DeliveryMetricsByClass) {
    assert_eq!(
        snapshot.reliable.superseded, 0,
        "reliable delivery cannot terminate as superseded"
    );
    assert_eq!(
        snapshot.reliable.dropped_full, 0,
        "reliable delivery cannot terminate as dropped_full"
    );
    assert_eq!(
        snapshot.reliable.dropped, 0,
        "reliable delivery cannot terminate as dropped"
    );
    assert_eq!(
        snapshot.latest.dropped, 0,
        "latest delivery cannot terminate as volatile dropped"
    );
    assert_eq!(
        snapshot.volatile.superseded, 0,
        "volatile delivery cannot terminate as superseded"
    );
    assert_eq!(
        snapshot.volatile.dropped_full, 0,
        "volatile delivery cannot terminate as dropped_full"
    );

    for (class, counters) in [
        ("reliable", snapshot.reliable),
        ("latest", snapshot.latest),
        ("volatile", snapshot.volatile),
    ] {
        let terminal = delivery_class_terminal_total(counters);
        assert_eq!(
            counters.attempted, terminal,
            "{class} delivery metrics do not conserve: attempted={}, terminal={terminal}, counters={counters:?}",
            counters.attempted
        );
    }
}

fn delivery_class_terminal_total(counters: DeliveryClassMetrics) -> u64 {
    [
        counters.delivered,
        counters.superseded,
        counters.dropped_full,
        counters.dropped,
        counters.abandoned,
        counters.unsupported_format,
    ]
    .into_iter()
    .try_fold(0u64, u64::checked_add)
    .expect("delivery-class terminal counter sum overflowed")
}

async fn assert_delivery_class_metrics_conserve(metrics: &ServerMetrics) {
    const QUIESCENCE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

    let deadline = tokio::time::Instant::now() + QUIESCENCE_DEADLINE;
    loop {
        let snapshot = metrics.delivery_metrics_by_class();
        let balances = [snapshot.reliable, snapshot.latest, snapshot.volatile]
            .into_iter()
            .all(|counters| counters.attempted == delivery_class_terminal_total(counters));
        if balances || tokio::time::Instant::now() >= deadline {
            assert_delivery_class_snapshot_conserves(snapshot);
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
