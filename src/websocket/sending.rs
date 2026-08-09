use crate::coordination::outbound_queue::{DataDeliveryMetadata, OutboundReceiver};
use crate::protocol::{ErrorCode, GameDataEncoding, PlayerId, ServerMessage};
use crate::server::EnhancedGameServer;
use axum::extract::ws::{Message, WebSocket};
use bytes::Bytes;
use futures_util::SinkExt;
use rmp_serde::{encode::write_named, from_slice};
use serde::Serialize;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::sync::watch;
use tokio::time::Instant;

use super::connection::{record_outbound_probe_activity, PingProbeState};

/// Whether this binary message is guaranteed to take the accounted
/// unsupported-format path for the recipient.
///
/// This synchronous preflight is used only to choose the writer deadline. The
/// send path still performs the conversion and authoritative accounting, so a
/// successful fallback remains reliable and every failure still emits its
/// exact report before any successor.
pub(super) enum BinaryFallbackPreflight {
    NotNeeded,
    Decoded {
        data: Arc<serde_json::Value>,
        message_pack_decodes: u64,
    },
    Undeliverable(String),
}

impl BinaryFallbackPreflight {
    pub(super) const fn is_unsupported(&self) -> bool {
        matches!(self, Self::Undeliverable(_))
    }
}

pub(super) fn preflight_binary_fallback(
    message: &ServerMessage,
    recipient_format: GameDataEncoding,
    relay_cache: Option<&RelayFrameCache>,
) -> BinaryFallbackPreflight {
    let ServerMessage::GameDataBinary {
        encoding, payload, ..
    } = message
    else {
        return BinaryFallbackPreflight::NotNeeded;
    };
    if *encoding == recipient_format {
        return BinaryFallbackPreflight::NotNeeded;
    }
    if let Some(cache) = relay_cache.filter(|cache| cache.kind == RelayFrameKind::Binary) {
        return match cache.decoded_fallback.get_or_init(|| {
            let decoded = decode_binary_to_json(*encoding, payload).map(Arc::new);
            if decoded.is_ok() && *encoding == GameDataEncoding::MessagePack {
                cache
                    .message_pack_decode_unreported
                    .store(true, Ordering::Release);
            }
            decoded
        }) {
            Ok(data) => BinaryFallbackPreflight::Decoded {
                data: Arc::clone(data),
                message_pack_decodes: 0,
            },
            Err(reason) => BinaryFallbackPreflight::Undeliverable(reason.clone()),
        };
    }
    match decode_binary_to_json(*encoding, payload) {
        Ok(data) => BinaryFallbackPreflight::Decoded {
            data: Arc::new(data),
            message_pack_decodes: u64::from(*encoding == GameDataEncoding::MessagePack),
        },
        Err(reason) => BinaryFallbackPreflight::Undeliverable(reason),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct SerializationWork {
    pub(super) json_encodes: u64,
    pub(super) message_pack_encodes: u64,
    pub(super) message_pack_decodes: u64,
}

#[derive(Debug, Clone)]
pub(super) struct MaterializedFrame {
    pub(super) frame: Message,
    pub(super) work: SerializationWork,
    binary_encode_error: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) enum GameDataMaterializationError {
    InvalidV3Stamp,
    Serialization(String),
    Undeliverable(String),
}

#[derive(Debug, Clone, Copy)]
enum RelayFrameCohort {
    TextV2,
    TextV3,
    BinaryDirectV2,
    BinaryDirectV3,
    BinaryFallbackV2,
    BinaryFallbackV3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelayFrameKind {
    Text,
    Binary,
}

impl RelayFrameCohort {
    const fn kind(self) -> RelayFrameKind {
        match self {
            Self::TextV2 | Self::TextV3 => RelayFrameKind::Text,
            Self::BinaryDirectV2
            | Self::BinaryDirectV3
            | Self::BinaryFallbackV2
            | Self::BinaryFallbackV3 => RelayFrameKind::Binary,
        }
    }
}

#[derive(Debug)]
pub(crate) struct RelayFrameCache {
    kind: RelayFrameKind,
    // Text and binary relays are mutually exclusive for one logical message,
    // so their v2/v3 slots can share storage instead of reserving six full
    // MaterializedFrame results for every shared relay.
    primary_v2: OnceLock<Result<MaterializedFrame, GameDataMaterializationError>>,
    primary_v3: OnceLock<Result<MaterializedFrame, GameDataMaterializationError>>,
    fallback_v2: OnceLock<Result<MaterializedFrame, GameDataMaterializationError>>,
    fallback_v3: OnceLock<Result<MaterializedFrame, GameDataMaterializationError>>,
    decoded_fallback: OnceLock<Result<Arc<serde_json::Value>, String>>,
    message_pack_decode_unreported: AtomicBool,
}

impl RelayFrameCache {
    pub(crate) fn for_message(message: &ServerMessage) -> Option<Self> {
        let kind = match message {
            ServerMessage::GameData { .. } => RelayFrameKind::Text,
            ServerMessage::GameDataBinary { .. } => RelayFrameKind::Binary,
            _ => return None,
        };
        Some(Self {
            kind,
            primary_v2: OnceLock::new(),
            primary_v3: OnceLock::new(),
            fallback_v2: OnceLock::new(),
            fallback_v3: OnceLock::new(),
            decoded_fallback: OnceLock::new(),
            message_pack_decode_unreported: AtomicBool::new(false),
        })
    }

    fn supports(&self, cohort: RelayFrameCohort) -> bool {
        self.kind == cohort.kind()
    }

    fn slot(
        &self,
        cohort: RelayFrameCohort,
    ) -> &OnceLock<Result<MaterializedFrame, GameDataMaterializationError>> {
        match cohort {
            RelayFrameCohort::TextV2 | RelayFrameCohort::BinaryDirectV2 => &self.primary_v2,
            RelayFrameCohort::TextV3 | RelayFrameCohort::BinaryDirectV3 => &self.primary_v3,
            RelayFrameCohort::BinaryFallbackV2 => &self.fallback_v2,
            RelayFrameCohort::BinaryFallbackV3 => &self.fallback_v3,
        }
    }

    fn materialize(
        &self,
        cohort: RelayFrameCohort,
        initialize: impl FnOnce() -> Result<MaterializedFrame, GameDataMaterializationError>,
    ) -> Result<MaterializedFrame, GameDataMaterializationError> {
        let mut initialized = false;
        let cached = self.slot(cohort).get_or_init(|| {
            initialized = true;
            initialize()
        });
        let mut materialized = cached.clone()?;
        if initialized {
            if matches!(
                cohort,
                RelayFrameCohort::BinaryFallbackV2 | RelayFrameCohort::BinaryFallbackV3
            ) && self
                .message_pack_decode_unreported
                .swap(false, Ordering::AcqRel)
            {
                materialized.work.message_pack_decodes =
                    materialized.work.message_pack_decodes.saturating_add(1);
            }
        } else {
            materialized.work = SerializationWork::default();
            materialized.binary_encode_error = None;
        }
        Ok(materialized)
    }
}

/// Apply the exact per-recipient game-data wire projection synchronously.
///
/// Production calls this immediately before the socket write. The allocation
/// benchmark sends the same returned Axum frame to an in-memory ledger, so the
/// measurement cannot drift from v2 field stripping, the v3 binary envelope,
/// or MessagePack-to-JSON fallback.
pub(super) fn materialize_game_data_frame(
    message: &ServerMessage,
    recipient_supports_v3: bool,
    recipient_format: GameDataEncoding,
    fallback_preflight: BinaryFallbackPreflight,
    relay_cache: Option<&RelayFrameCache>,
) -> Result<MaterializedFrame, GameDataMaterializationError> {
    let cohort = match message {
        ServerMessage::GameData { .. } if recipient_supports_v3 => RelayFrameCohort::TextV3,
        ServerMessage::GameData { .. } => RelayFrameCohort::TextV2,
        ServerMessage::GameDataBinary { encoding, .. }
            if *encoding == recipient_format && recipient_supports_v3 =>
        {
            RelayFrameCohort::BinaryDirectV3
        }
        ServerMessage::GameDataBinary { encoding, .. } if *encoding == recipient_format => {
            RelayFrameCohort::BinaryDirectV2
        }
        ServerMessage::GameDataBinary { .. } if recipient_supports_v3 => {
            RelayFrameCohort::BinaryFallbackV3
        }
        ServerMessage::GameDataBinary { .. } => RelayFrameCohort::BinaryFallbackV2,
        _ => {
            return Err(GameDataMaterializationError::Serialization(
                "game-data materializer received a non-game-data message".to_string(),
            ));
        }
    };
    if let Some(cache) = relay_cache.filter(|cache| cache.supports(cohort)) {
        return cache.materialize(cohort, || {
            materialize_game_data_frame_uncached(
                message,
                recipient_supports_v3,
                recipient_format,
                fallback_preflight,
            )
        });
    }
    materialize_game_data_frame_uncached(
        message,
        recipient_supports_v3,
        recipient_format,
        fallback_preflight,
    )
}

fn materialize_game_data_frame_uncached(
    message: &ServerMessage,
    recipient_supports_v3: bool,
    recipient_format: GameDataEncoding,
    fallback_preflight: BinaryFallbackPreflight,
) -> Result<MaterializedFrame, GameDataMaterializationError> {
    match message {
        ServerMessage::GameData {
            from_player,
            data,
            seq,
            epoch,
            class,
            key,
        } => {
            let frame = if !recipient_supports_v3
                && (seq.is_some() || epoch.is_some() || class.is_some() || key.is_some())
            {
                serialize_json_relay_frame(&LegacyGameDataEnvelope::new(*from_player, data), data)
            } else {
                serialize_json_relay_frame(message, data)
            }
            .map_err(|error| GameDataMaterializationError::Serialization(error.to_string()))?;
            Ok(MaterializedFrame {
                frame,
                work: SerializationWork {
                    json_encodes: 1,
                    ..SerializationWork::default()
                },
                binary_encode_error: None,
            })
        }
        ServerMessage::GameDataBinary {
            from_player,
            encoding,
            payload,
            seq,
            epoch,
        } => {
            let (seq, epoch) = if recipient_supports_v3 {
                match (*seq, *epoch) {
                    (Some(seq), Some(epoch)) => (Some(seq), Some(epoch)),
                    _ => return Err(GameDataMaterializationError::InvalidV3Stamp),
                }
            } else {
                (None, None)
            };

            let binary_encode_error = if recipient_format == *encoding {
                match encode_binary_game_data(*from_player, *encoding, payload, seq, epoch) {
                    Ok(frame_bytes) => {
                        return Ok(MaterializedFrame {
                            frame: Message::Binary(frame_bytes),
                            work: SerializationWork {
                                message_pack_encodes: u64::from(
                                    seq.is_some() || *encoding == GameDataEncoding::MessagePack,
                                ),
                                ..SerializationWork::default()
                            },
                            binary_encode_error: None,
                        });
                    }
                    Err(error) => Some(error),
                }
            } else {
                None
            };

            let (data, message_pack_decodes) = match fallback_preflight {
                BinaryFallbackPreflight::NotNeeded => (
                    decode_binary_to_json(*encoding, payload)
                        .map(Arc::new)
                        .map_err(GameDataMaterializationError::Undeliverable)?,
                    u64::from(*encoding == GameDataEncoding::MessagePack),
                ),
                BinaryFallbackPreflight::Decoded {
                    data,
                    message_pack_decodes,
                } => (data, message_pack_decodes),
                BinaryFallbackPreflight::Undeliverable(reason) => {
                    return Err(GameDataMaterializationError::Undeliverable(reason));
                }
            };
            let fallback = BorrowedGameDataEnvelope::new(*from_player, data.as_ref(), seq, epoch);
            let frame = serialize_json_fallback_frame(&fallback, payload.len())
                .map_err(GameDataMaterializationError::Serialization)?;
            Ok(MaterializedFrame {
                frame,
                work: SerializationWork {
                    json_encodes: 1,
                    message_pack_decodes,
                    ..SerializationWork::default()
                },
                binary_encode_error,
            })
        }
        _ => Err(GameDataMaterializationError::Serialization(
            "game-data materializer received a non-game-data message".to_string(),
        )),
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ImmediateSendError {
    #[error("failed to serialize server message: {0}")]
    Serialization(#[source] serde_json::Error),
    #[error("failed to write server message: {0}")]
    Socket(#[source] axum::Error),
}

pub(super) async fn send_immediate_server_message(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    message: &ServerMessage,
) -> Result<(), ImmediateSendError> {
    let payload = serialize_json_text(message).map_err(|err| {
        tracing::error!(error = %err, "Failed to serialize server message");
        ImmediateSendError::Serialization(err)
    })?;

    sender
        .send(Message::Text(payload.into()))
        .await
        .map_err(ImmediateSendError::Socket)
}

pub(super) async fn send_single_message(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    message: Arc<ServerMessage>,
    relay_cache: Option<&RelayFrameCache>,
    player_id: &PlayerId,
    recipient_supports_v3: bool,
    recipient_format: GameDataEncoding,
    fallback_preflight: BinaryFallbackPreflight,
    metadata: Option<DataDeliveryMetadata>,
    accounting: &mut SendAccounting<'_>,
) -> Result<SendDisposition, ()> {
    send_single_message_ref(
        sender,
        message.as_ref(),
        relay_cache,
        player_id,
        recipient_supports_v3,
        recipient_format,
        fallback_preflight,
        metadata,
        accounting,
    )
    .await
}

pub(super) async fn send_single_message_ref(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    message: &ServerMessage,
    relay_cache: Option<&RelayFrameCache>,
    player_id: &PlayerId,
    recipient_supports_v3: bool,
    recipient_format: GameDataEncoding,
    fallback_preflight: BinaryFallbackPreflight,
    metadata: Option<DataDeliveryMetadata>,
    accounting: &mut SendAccounting<'_>,
) -> Result<SendDisposition, ()> {
    let mut disposition = SendDisposition::Written;
    match message {
        ServerMessage::GameDataBinary {
            from_player,
            encoding,
            seq,
            epoch,
            ..
        } => {
            match materialize_game_data_frame(
                message,
                recipient_supports_v3,
                recipient_format,
                fallback_preflight,
                relay_cache,
            ) {
                Ok(materialized) => {
                    if let Some(error) = materialized.binary_encode_error {
                        tracing::error!(
                            %player_id,
                            %from_player,
                            encoding = ?encoding,
                            %error,
                            "Failed to encode binary game data; attempting JSON fallback"
                        );
                    }
                    if sender.send(materialized.frame).await.is_err() {
                        tracing::warn!(
                            %player_id,
                            "Failed to send binary game data, connection closed"
                        );
                        return Err(());
                    }
                }
                Err(GameDataMaterializationError::InvalidV3Stamp) => {
                    tracing::error!(
                        %player_id,
                        %from_player,
                        seq = ?seq,
                        epoch = ?epoch,
                        "Protocol-v3 binary game data lacked a complete delivery stamp; closing fail-closed"
                    );
                    return Err(());
                }
                Err(GameDataMaterializationError::Serialization(error)) => {
                    tracing::error!(
                        %player_id,
                        %from_player,
                        encoding = ?encoding,
                        %error,
                        "Failed to serialize binary game data fallback"
                    );
                    return Err(());
                }
                Err(GameDataMaterializationError::Undeliverable(reason)) => {
                    disposition = notify_on_undeliverable(
                        sender,
                        *from_player,
                        *encoding,
                        reason,
                        player_id,
                        recipient_supports_v3,
                        metadata,
                        accounting,
                    )
                    .await?;
                }
            }
        }
        ServerMessage::GameData { .. } => {
            let materialized = materialize_game_data_frame(
                message,
                recipient_supports_v3,
                recipient_format,
                fallback_preflight,
                relay_cache,
            )
            .map_err(|error| {
                tracing::error!(%player_id, ?error, "Failed to serialize game data");
            })?;
            if sender.send(materialized.frame).await.is_err() {
                tracing::warn!(%player_id, "Failed to send message, connection closed");
                return Err(());
            }
        }
        // Broadcast room-snapshot frames carry a v3 incarnation `epoch` that
        // must never reach a pre-v3 (v2) recipient (their bytes stay
        // byte-identical to the frozen v2 wire). Only the rare snapshot
        // broadcasts hit this; the common relay path above is untouched.
        ServerMessage::PlayerJoined { player }
            if (player.epoch.is_some() || player.seq.is_some()) && !recipient_supports_v3 =>
        {
            let mut player = player.clone();
            player.epoch = None;
            player.seq = None;
            send_text_message(sender, &ServerMessage::PlayerJoined { player }, player_id).await?;
        }
        ServerMessage::PlayerReconnected {
            player_id: reconnected,
            epoch: Some(_),
        } if !recipient_supports_v3 => {
            let stripped = ServerMessage::PlayerReconnected {
                player_id: *reconnected,
                epoch: None,
            };
            send_text_message(sender, &stripped, player_id).await?;
        }
        ServerMessage::PlayerLeft {
            player_id: departed,
            epoch,
            final_seq,
        } if (epoch.is_some() || final_seq.is_some()) && !recipient_supports_v3 => {
            let stripped = ServerMessage::PlayerLeft {
                player_id: *departed,
                epoch: None,
                final_seq: None,
            };
            send_text_message(sender, &stripped, player_id).await?;
        }
        ServerMessage::SpectatorJoined(payload)
            if payload
                .current_players
                .iter()
                .any(|player| player.epoch.is_some() || player.seq.is_some())
                && !recipient_supports_v3 =>
        {
            let mut payload = payload.as_ref().clone();
            for player in &mut payload.current_players {
                player.epoch = None;
                player.seq = None;
            }
            send_text_message(
                sender,
                &ServerMessage::SpectatorJoined(Box::new(payload)),
                player_id,
            )
            .await?;
        }
        other => {
            send_text_message(sender, other, player_id).await?;
        }
    }

    Ok(disposition)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SendDisposition {
    Written,
    AccountedDrop,
}

/// Cancellation-safe terminal accounting for the one queue item actively
/// owned by a socket write. Dropping the write future records abandonment, so
/// the outer close `select!` cannot create an untracked one-message hole.
pub(super) struct SendAccounting<'a> {
    receiver: &'a OutboundReceiver,
    server: &'a Arc<EnhancedGameServer>,
    ping_probe_state: &'a watch::Sender<PingProbeState>,
    player_id: PlayerId,
    class: Option<crate::protocol::DeliveryClass>,
    resolved: bool,
    /// An omission counted here whose exact range has not been held yet.
    /// Cleared by [`Self::hold_unsupported`]; a drop while it is set means the
    /// write that would have made room was cancelled, leaving a hole nothing
    /// describes.
    unheld_omission: bool,
}

impl<'a> SendAccounting<'a> {
    pub(super) fn new(
        receiver: &'a OutboundReceiver,
        server: &'a Arc<EnhancedGameServer>,
        ping_probe_state: &'a watch::Sender<PingProbeState>,
        player_id: PlayerId,
        class: Option<crate::protocol::DeliveryClass>,
    ) -> Self {
        Self {
            receiver,
            server,
            ping_probe_state,
            player_id,
            class,
            resolved: false,
            unheld_omission: false,
        }
    }

    fn record_outbound_progress(&self) {
        record_outbound_probe_activity(self.ping_probe_state, Instant::now());
    }

    pub(super) fn complete_written(&mut self) {
        if let Some(class) = self.class {
            self.receiver.record_written(class);
        }
        self.resolved = true;
    }

    /// Account one undeliverable payload, coalescing its exact range. `false`
    /// means the pending report has to be written before this range can be held
    /// (see [`Self::hold_unsupported`]); the omission itself is counted either
    /// way.
    fn complete_unsupported(&mut self, metadata: Option<DataDeliveryMetadata>) -> bool {
        let coalesced =
            metadata.is_none_or(|metadata| self.receiver.record_unsupported_format(metadata));
        if metadata.is_none() {
            if let Some(class) = self.class {
                self.receiver.record_unsupported_class(class);
            }
        }
        self.record_drop_metrics();
        self.resolved = true;
        self.unheld_omission = !coalesced;
        coalesced
    }

    /// Hold an omission whose range only fits once the pending report is written.
    ///
    /// The omission stays outstanding — and therefore still fences the queue on
    /// drop — unless the range is genuinely stored.
    fn hold_unsupported(&mut self, metadata: Option<DataDeliveryMetadata>) {
        let held = match metadata {
            Some(metadata) => self.receiver.hold_unsupported_format(metadata),
            None => true,
        };
        if !held {
            tracing::error!(
                player_id = %self.player_id,
                "Undeliverable range did not fit the emptied report; fencing the queue"
            );
        }
        self.unheld_omission = !held;
    }

    fn record_drop_metrics(&self) {
        let metrics = self.server.metrics();
        metrics.increment_websocket_messages_dropped();
        if let Some(stats) = metrics.connection_delivery_stats(&self.player_id) {
            stats
                .dropped_for_you
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn unsupported_notice(&self, sender: PlayerId) -> Option<u64> {
        self.receiver.unsupported_notice(sender)
    }
}

impl Drop for SendAccounting<'_> {
    fn drop(&mut self) {
        if self.resolved {
            if self.unheld_omission {
                // The omission was counted but its exact range never reached the
                // pending report, because the write that would have made room
                // was cancelled. The counter is clamped to the written frontier,
                // so nothing would ever describe this hole: fence the queue so
                // teardown abandons the frames behind it instead of showing the
                // recipient a sequence that skips an unreported omission.
                self.receiver.record_abandoned_in_flight_write();
            }
            return;
        }
        // The payload was owned by a socket write that never resolved, so the
        // sink may or may not have accepted its frame. Fence the connection's
        // remaining queue before counting the loss: whatever is still queued
        // sits *behind* an item whose wire position is unknown, and writing it
        // would show the recipient a delivered sequence that skips one it was
        // never told about. `finalize_closed_connection` reads this and
        // abandons the remainder instead of flushing it.
        self.receiver.record_abandoned_in_flight_write();
        if let Some(class) = self.class {
            self.receiver.record_abandoned(class, 1);
        }
        self.record_drop_metrics();
    }
}

/// Handle a failed binary fallback without ever silently dropping game data:
/// a payload that genuinely cannot be represented for this recipient is
/// counted as dropped and replaced by an explicit error frame so the recipient
/// knows it is missing data (e.g. an rkyv-encoded room relayed to a JSON-only
/// client).
async fn notify_on_undeliverable(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    from_player: PlayerId,
    encoding: GameDataEncoding,
    reason: String,
    player_id: &PlayerId,
    recipient_supports_v3: bool,
    metadata: Option<DataDeliveryMetadata>,
    accounting: &mut SendAccounting<'_>,
) -> Result<SendDisposition, ()> {
    tracing::warn!(
        %player_id,
        %from_player,
        encoding = ?encoding,
        reason = %reason,
        "Game data undeliverable to this recipient; sending an error notice instead"
    );
    let coalesced = accounting.complete_unsupported(metadata);
    if recipient_supports_v3 && metadata.is_none() {
        tracing::error!(
            %player_id,
            %from_player,
            "Stamped v3 binary fallback lacked delivery metadata; closing fail-closed"
        );
        return Err(());
    }
    if !coalesced {
        // The pending report is full, or this range cannot join it:
        // write what is pending, then hold this range in the emptied
        // report. Holding only after the write keeps the two sets
        // disjoint if the write is cancelled.
        if write_pending_unsupported_report(sender, accounting.receiver, player_id).await? {
            accounting.record_outbound_progress();
        }
        accounting.hold_unsupported(metadata);
    }
    if let Some(suppressed) = accounting.unsupported_notice(from_player) {
        // The advisory must never precede the exact report that
        // explains it, so the rate-limited notice is also the pending
        // report's flush cadence: at most one report per sender per
        // second instead of one per omitted message.
        if write_pending_unsupported_report(sender, accounting.receiver, player_id).await? {
            accounting.record_outbound_progress();
        }
        let suppressed = if suppressed == 0 {
            String::new()
        } else {
            format!("; {suppressed} similar advisories suppressed since the last notice")
        };
        let notice = ServerMessage::Error {
            message: format!(
                "Undeliverable game data from player {from_player} \
                         ({} payload cannot be converted for this connection): {reason}{suppressed}",
                encoding.as_wire_str()
            ),
            error_code: Some(ErrorCode::UnsupportedGameDataFormat),
        };
        send_text_message(sender, &notice, player_id).await?;
        accounting.record_outbound_progress();
    }
    Ok(SendDisposition::AccountedDrop)
}

/// Write the recipient's coalesced unsupported-format report, if any, and retire
/// its ranges only once the frame is on the wire.
///
/// Peek-write-commit rather than take-then-write: the send task's close
/// `select!` can cancel this at its await, and losing the only copy of a
/// coalesced burst would silently drop accountability for every omission in it.
/// A cancelled write leaves the ranges pending for the next flush or for the
/// connection's teardown.
pub(super) async fn write_pending_unsupported_report(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    receiver: &OutboundReceiver,
    player_id: &PlayerId,
) -> Result<bool, ()> {
    let Some(report) = receiver.pending_unsupported_report() else {
        return Ok(false);
    };
    send_text_message(
        sender,
        &ServerMessage::DeliveryReport(Box::new(report.clone())),
        player_id,
    )
    .await?;
    receiver.commit_pending_unsupported_report(&report);
    Ok(true)
}

pub(super) async fn send_text_message(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    message: &ServerMessage,
    player_id: &PlayerId,
) -> Result<(), ()> {
    send_serialized_text(sender, message, player_id).await
}

/// Serialize any wire-shaped value to a JSON text frame and send it. Shared by
/// the `ServerMessage` path and the borrowed pre-v3 GameData shadow
/// ([`LegacyGameDataEnvelope`]); `Err(())` means no frame was produced or the
/// connection closed.
async fn send_serialized_text<T: Serialize>(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    message: &T,
    player_id: &PlayerId,
) -> Result<(), ()> {
    let json_message = match serialize_json_text(message) {
        Ok(json) => json,
        Err(e) => {
            tracing::error!(%player_id, "Failed to serialize message: {}", e);
            return Err(());
        }
    };

    if sender
        .send(Message::Text(json_message.into()))
        .await
        .is_err()
    {
        tracing::warn!(%player_id, "Failed to send message, connection closed");
        return Err(());
    }

    Ok(())
}

fn serialize_json_text<T: Serialize>(message: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(message)
}

#[cfg(test)]
fn serialize_json_frame<T: Serialize>(message: &T) -> Result<Message, serde_json::Error> {
    serialize_json_text(message).map(|json| Message::Text(json.into()))
}

/// Serialize a JSON game-data relay into one pre-sized output allocation.
///
/// The value estimate counts its unescaped wire structure and the relay
/// headroom covers the tagged envelope, UUID, delivery stamps, and ordinary
/// string escaping. Inputs with unusually escape-heavy strings may still grow
/// through `Vec`'s normal allocator-abort path. Excess capacity from that
/// growth is compacted only when it exceeds the same fixed headroom,
/// preserving exact wire bytes without making a fixed-size assumption part of
/// the protocol contract.
const JSON_RELAY_HEADROOM: usize = 256;
const JSON_ESTIMATE_WORK_BUDGET: usize = 256;
// A float's one-byte lower bound can miss at most 23 bytes of its ordinary
// serde_json representation. Charge that uncertainty as sizing work so a
// float-dense value falls back before a second traversal plus buffer
// grow/compact cycle costs more than the allocation it was meant to avoid.
const JSON_FLOAT_ESTIMATE_UNCERTAINTY: usize = 23;

fn serialize_json_relay_frame<T: Serialize>(
    message: &T,
    data: &serde_json::Value,
) -> Result<Message, String> {
    let Some(estimated_value_len) = estimated_json_value_len(data) else {
        return serialize_json_text(message)
            .map(|json| Message::Text(json.into()))
            .map_err(|error| error.to_string());
    };
    let capacity = estimated_value_len
        .checked_add(JSON_RELAY_HEADROOM)
        .filter(|capacity| *capacity <= isize::MAX as usize)
        .ok_or_else(|| "JSON relay frame capacity overflow".to_string())?;
    // The old `serde_json::to_string` path was already infallible on allocator
    // exhaustion. Keep Vec's direct `Write` implementation here: wrapping
    // every serializer write in a fallibility check costs more than the three
    // growth reallocations this path removes. Any underestimated growth keeps
    // those same allocator-abort semantics; the fallback converter below has
    // a separate explicit fallible allocation contract.
    let mut bytes = Vec::with_capacity(capacity);
    serde_json::to_writer(&mut bytes, message).map_err(|error| error.to_string())?;
    let maximum_retained_capacity = bytes.len().saturating_add(JSON_RELAY_HEADROOM);
    if bytes.capacity() > capacity && bytes.capacity() > maximum_retained_capacity {
        bytes.shrink_to(maximum_retained_capacity);
    }
    String::from_utf8(bytes)
        .map(|json| Message::Text(json.into()))
        .map_err(|error| format!("JSON serializer emitted invalid UTF-8: {error}"))
}

/// Lower-bound serialized length that is cheap to derive from an existing
/// `serde_json::Value`. String escaping may expand beyond this estimate; the
/// caller deliberately permits ordinary `Vec` growth for that case. Returning
/// `None` caps sizing work for structurally dense inputs, which then use the
/// reference serializer without paying for a second complete traversal.
fn estimated_json_value_len(value: &serde_json::Value) -> Option<usize> {
    let mut remaining_work = JSON_ESTIMATE_WORK_BUDGET;
    estimated_json_value_len_bounded(value, &mut remaining_work)
}

fn estimated_json_value_len_bounded(
    value: &serde_json::Value,
    remaining_work: &mut usize,
) -> Option<usize> {
    *remaining_work = remaining_work.checked_sub(1)?;
    match value {
        serde_json::Value::Null => Some(4),
        serde_json::Value::Bool(value) => {
            if *value {
                Some(4)
            } else {
                Some(5)
            }
        }
        serde_json::Value::Number(value) => {
            let (estimated_len, uncertainty) = estimated_json_number_len(value);
            *remaining_work = remaining_work.checked_sub(uncertainty)?;
            Some(estimated_len)
        }
        serde_json::Value::String(value) => Some(value.len().saturating_add(2)),
        serde_json::Value::Array(values) => values.iter().try_fold(
            2usize.saturating_add(values.len().saturating_sub(1)),
            |size, value| {
                Some(size.saturating_add(estimated_json_value_len_bounded(value, remaining_work)?))
            },
        ),
        serde_json::Value::Object(values) => values.iter().try_fold(
            2usize.saturating_add(values.len().saturating_sub(1)),
            |size, (key, value)| {
                Some(
                    size.saturating_add(key.len().saturating_add(3))
                        .saturating_add(estimated_json_value_len_bounded(value, remaining_work)?),
                )
            },
        ),
    }
}

fn estimated_json_number_len(value: &serde_json::Number) -> (usize, usize) {
    if let Some(value) = value.as_i64() {
        (
            decimal_digit_count(value.unsigned_abs())
                .saturating_add(usize::from(value.is_negative())),
            0,
        )
    } else if let Some(value) = value.as_u64() {
        (decimal_digit_count(value), 0)
    } else {
        // Formatting a float here duplicates serde_json's comparatively
        // expensive conversion. One byte is a safe lower bound; ordinary Vec
        // growth handles the remainder and the caller trims excess capacity.
        (1, JSON_FLOAT_ESTIMATE_UNCERTAINTY)
    }
}

fn decimal_digit_count(value: u64) -> usize {
    if value == 0 {
        1
    } else {
        (value.ilog10() as usize).saturating_add(1)
    }
}

/// Serialize a decoded binary fallback without repeatedly growing the JSON
/// output buffer. MessagePack is normally denser than JSON, so its opaque
/// payload length is a useful lower bound; fixed headroom covers the relay
/// envelope and common representation expansion. Unusually compact inputs can
/// still grow through the fallible writer without changing wire bytes.
fn serialize_json_fallback_frame<T: Serialize>(
    message: &T,
    payload_len: usize,
) -> Result<Message, String> {
    const JSON_FALLBACK_HEADROOM: usize = 256;

    let capacity = payload_len
        .checked_add(JSON_FALLBACK_HEADROOM)
        .filter(|capacity| *capacity <= isize::MAX as usize)
        .ok_or_else(|| "JSON fallback frame capacity overflow".to_string())?;
    serialize_json_frame_with_capacity(message, capacity, "JSON fallback")
}

fn serialize_json_frame_with_capacity<T: Serialize>(
    message: &T,
    capacity: usize,
    frame_kind: &'static str,
) -> Result<Message, String> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|error| format!("failed to reserve {frame_kind} frame capacity: {error}"))?;
    serde_json::to_writer(&mut FallibleVecWriter::new(&mut bytes, frame_kind), message)
        .map_err(|error| error.to_string())?;
    String::from_utf8(bytes)
        .map(|json| Message::Text(json.into()))
        .map_err(|error| format!("JSON serializer emitted invalid UTF-8: {error}"))
}

/// Borrowed wire shadow of a `ServerMessage::GameData` frame WITHOUT the v3
/// `seq` stamp, used to serialize a stamped shared-`Arc` relay message for a
/// pre-v3 recipient without cloning the payload.
///
/// IMPORTANT: this must serialize byte-identically to
/// `ServerMessage::GameData { from_player, data, seq: None }` — the adjacently
/// tagged `{"type":"GameData","data":{"from_player":..,"data":..}}` envelope
/// (v2-frozen in `tests/v2_wire_golden.rs`). The unit tests below pin that
/// equivalence, mirroring how [`LegacyBinaryGameDataFrame`] freezes the v2
/// MessagePack frame.
#[derive(Serialize)]
struct LegacyGameDataEnvelope<'a> {
    r#type: &'static str,
    data: LegacyGameDataBody<'a>,
}

#[derive(Serialize)]
struct LegacyGameDataBody<'a> {
    from_player: PlayerId,
    data: &'a serde_json::Value,
}

impl<'a> LegacyGameDataEnvelope<'a> {
    fn new(from_player: PlayerId, data: &'a serde_json::Value) -> Self {
        Self {
            r#type: "GameData",
            data: LegacyGameDataBody { from_player, data },
        }
    }
}

/// Borrowed JSON fallback envelope for a binary source. Borrowing the decoded
/// value lets one relay cache share both the MessagePack decode and the
/// serialized text frame without cloning the ~payload-sized JSON tree.
#[derive(Serialize)]
struct BorrowedGameDataEnvelope<'a> {
    r#type: &'static str,
    data: BorrowedGameDataBody<'a>,
}

#[derive(Serialize)]
struct BorrowedGameDataBody<'a> {
    from_player: PlayerId,
    data: &'a serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    epoch: Option<u32>,
}

impl<'a> BorrowedGameDataEnvelope<'a> {
    fn new(
        from_player: PlayerId,
        data: &'a serde_json::Value,
        seq: Option<u64>,
        epoch: Option<u32>,
    ) -> Self {
        Self {
            r#type: "GameData",
            data: BorrowedGameDataBody {
                from_player,
                data,
                seq,
                epoch,
            },
        }
    }
}

/// The exact legacy struct serialized onto the wire for v2 MessagePack
/// game-data frames.
///
/// IMPORTANT: binary frames do NOT travel through the `ServerMessage` enum's
/// `{type, data}` envelope. The `ServerMessage::GameDataBinary` variant is only
/// an *in-memory* carrier used to route the payload through the broadcast layer;
/// `send_single_message` intercepts it and instead serializes this bare struct
/// via named MessagePack encoding (see `encode_binary_game_data`). V2 JSON and
/// rkyv frames remain raw payload passthrough; the legacy MessagePack frame is
/// retained only to preserve the frozen v2 wire contract.
#[derive(Serialize)]
struct LegacyBinaryGameDataFrame<'a> {
    from_player: PlayerId,
    encoding: GameDataEncoding,
    #[serde(with = "serde_bytes")]
    payload: &'a [u8],
}

/// Protocol-v3 binary relay envelope. Its MessagePack encoding is independent
/// of the opaque payload's declared encoding, so every binary recipient sees
/// the same mandatory accountability metadata without the server inspecting or
/// rewriting the payload bytes.
#[derive(Serialize)]
struct V3BinaryGameDataFrame<'a> {
    from_player: PlayerId,
    encoding: GameDataEncoding,
    #[serde(with = "serde_bytes")]
    payload: &'a [u8],
    seq: u64,
    epoch: u32,
}

/// Materializes the binary bytes exactly as production puts them on the wire.
///
/// This is the single source of truth for the binary send path. Keep this
/// private to the websocket module so the wire frame layout can be tested
/// without becoming public library API.
///
/// `seq`/`epoch` must already be gated per recipient: both present for v3, both
/// absent for v2. V3 always uses the MessagePack metadata envelope while keeping
/// `payload` opaque. V2 retains its historical representation byte-for-byte:
/// the legacy MessagePack envelope or raw JSON/rkyv passthrough. Raw passthrough
/// clones the shared `Bytes` handle, not the payload buffer.
pub(super) fn encode_binary_game_data(
    from_player: PlayerId,
    encoding: GameDataEncoding,
    payload: &Bytes,
    seq: Option<u64>,
    epoch: Option<u32>,
) -> Result<Bytes, String> {
    match (seq, epoch) {
        (Some(seq), Some(epoch)) => {
            if seq == 0 || epoch == 0 {
                return Err("protocol-v3 binary delivery stamps must be non-zero".to_string());
            }
            let frame = V3BinaryGameDataFrame {
                from_player,
                encoding,
                payload,
                seq,
                epoch,
            };
            encode_named_binary_frame(&frame, payload.len()).map(Bytes::from)
        }
        (None, None) => match encoding {
            GameDataEncoding::MessagePack => encode_named_binary_frame(
                &LegacyBinaryGameDataFrame {
                    from_player,
                    encoding,
                    payload,
                },
                payload.len(),
            )
            .map(Bytes::from),
            GameDataEncoding::Json | GameDataEncoding::Rkyv => Ok(payload.clone()),
        },
        _ => Err("binary delivery seq and epoch must be present or absent together".to_string()),
    }
}

/// Encode a named MessagePack envelope without repeatedly growing its output
/// buffer. The opaque payload length is known and dominates relay frame size;
/// 128 bytes covers the fixed field names, UUID, encoding, stamps, and
/// MessagePack container headers.
fn encode_named_binary_frame<T: Serialize>(
    frame: &T,
    payload_len: usize,
) -> Result<Vec<u8>, String> {
    const ENVELOPE_HEADROOM: usize = 128;

    let capacity = payload_len
        .checked_add(ENVELOPE_HEADROOM)
        .filter(|capacity| *capacity <= isize::MAX as usize)
        .ok_or_else(|| "binary frame capacity overflow".to_string())?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|error| format!("failed to reserve binary frame capacity: {error}"))?;
    write_named(&mut FallibleVecWriter::new(&mut bytes, "binary"), frame)
        .map_err(|err| err.to_string())?;
    Ok(bytes)
}

/// Preserve rmp-serde's fallible allocation behavior while supplying a
/// pre-sized output vector. Writing directly to `Vec<u8>` would use its
/// infallible `Write` implementation if fixed envelope headroom ever became
/// insufficient.
struct FallibleVecWriter<'a> {
    bytes: &'a mut Vec<u8>,
    frame_kind: &'static str,
}

impl FallibleVecWriter<'_> {
    fn new<'a>(bytes: &'a mut Vec<u8>, frame_kind: &'static str) -> FallibleVecWriter<'a> {
        FallibleVecWriter { bytes, frame_kind }
    }

    fn reserve_for_write(&mut self, additional_len: usize) -> std::io::Result<()> {
        let required_len = self
            .bytes
            .len()
            .checked_add(additional_len)
            .filter(|required_len| *required_len <= isize::MAX as usize)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("{} frame capacity overflow", self.frame_kind),
                )
            })?;
        if required_len > self.bytes.capacity() {
            self.bytes
                .try_reserve_exact(additional_len)
                .map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::OutOfMemory,
                        format!("failed to grow {} frame buffer: {error}", self.frame_kind),
                    )
                })?;
        }
        Ok(())
    }
}

impl Write for FallibleVecWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.write_all(buffer)?;
        Ok(buffer.len())
    }

    fn write_all(&mut self, buffer: &[u8]) -> std::io::Result<()> {
        self.reserve_for_write(buffer.len())?;
        self.bytes.extend_from_slice(buffer);
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn decode_binary_to_json(
    encoding: GameDataEncoding,
    payload: &[u8],
) -> Result<serde_json::Value, String> {
    match encoding {
        GameDataEncoding::MessagePack => from_slice(payload).map_err(|err| err.to_string()),
        GameDataEncoding::Json => serde_json::from_slice(payload).map_err(|err| err.to_string()),
        GameDataEncoding::Rkyv => {
            // Rkyv data cannot be directly converted to JSON without knowing the type.
            // Return an opaque representation with the raw bytes.
            // Clients using Rkyv should NOT fall back to JSON - they should use native rkyv decoding.
            Err("Rkyv payloads cannot be converted to JSON - use native rkyv decoding".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        decode_v3_binary_game_data, V3BinaryGameDataFrame as DecodedV3BinaryGameDataFrame,
    };
    use serde::{Deserialize, Serializer};
    use uuid::Uuid;

    const PLAYER_A_STR: &str = "00112233-4455-6677-8899-aabbccddeeff";

    fn player_a() -> Uuid {
        Uuid::from_u128(0x0011_2233_4455_6677_8899_aabb_ccdd_eeff)
    }

    /// Issue #274: only an unresolved socket write fences the queue behind it.
    ///
    /// The fence stops a closing connection from writing past a payload whose
    /// wire position is unknown. It must therefore fire for exactly one
    /// terminal state — the write future dropped before resolving — because a
    /// false positive would make every healthy teardown abandon its queue.
    #[tokio::test]
    async fn only_an_unresolved_socket_write_fences_the_queue_behind_it() {
        use crate::coordination::outbound_queue::{channel, DataDeliveryMetadata};
        use crate::database::DatabaseConfig;
        use crate::protocol::RoomId;
        use crate::server::ServerConfig;

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Resolution {
            Written,
            UnsupportedFormat,
            AbandonedInFlight,
        }

        for (resolution, expected_fence) in [
            (Resolution::Written, false),
            (Resolution::UnsupportedFormat, false),
            (Resolution::AbandonedInFlight, true),
        ] {
            let server = EnhancedGameServer::new(
                ServerConfig::default(),
                crate::config::ProtocolConfig::default(),
                crate::config::RelayTypeConfig::default(),
                crate::config::SessionConfig::default(),
                crate::config::TurnConfig::default(),
                DatabaseConfig::InMemory,
                crate::config::MetricsConfig::default(),
                crate::config::CoordinationConfig::default(),
                crate::config::TransportSecurityConfig::default(),
                Vec::new(),
            )
            .await
            .expect("construct accounting-fence test server");
            let (_tx, rx) = channel(4, 4);
            let (probe_state, _probe_updates) = watch::channel(PingProbeState::default());
            let from_player = PlayerId::from_u128(3);
            let metadata = DataDeliveryMetadata {
                class: crate::protocol::DeliveryClass::Reliable,
                key: None,
                from_player,
                room_id: RoomId::from_u128(5),
                epoch: 1,
                seq: 7,
            };

            {
                let mut accounting = SendAccounting::new(
                    &rx,
                    &server,
                    &probe_state,
                    from_player,
                    Some(crate::protocol::DeliveryClass::Reliable),
                );
                match resolution {
                    Resolution::Written => accounting.complete_written(),
                    Resolution::UnsupportedFormat => {
                        let _coalesced = accounting.complete_unsupported(Some(metadata));
                    }
                    Resolution::AbandonedInFlight => {}
                }
            }

            assert_eq!(
                rx.abandoned_in_flight_write(),
                expected_fence,
                "{resolution:?}: the teardown fence must reflect exactly this terminal state"
            );
        }
    }

    /// An omission whose exact range did not fit the pending report is only
    /// safe once that range is held. Until then the recipient has a hole that
    /// nothing describes: the counter is clamped to the written frontier, so a
    /// teardown that flushed the rest of the queue would show a sequence
    /// skipping an omission it was never told about.
    #[tokio::test]
    async fn an_unheld_undeliverable_range_fences_the_queue() {
        use crate::coordination::outbound_queue::{channel, DataDeliveryMetadata};
        use crate::database::DatabaseConfig;
        use crate::protocol::{RoomId, DELIVERY_REPORT_MAX_GAPS};
        use crate::server::ServerConfig;

        let server = EnhancedGameServer::new(
            ServerConfig::default(),
            crate::config::ProtocolConfig::default(),
            crate::config::RelayTypeConfig::default(),
            crate::config::SessionConfig::default(),
            crate::config::TurnConfig::default(),
            DatabaseConfig::InMemory,
            crate::config::MetricsConfig::default(),
            crate::config::CoordinationConfig::default(),
            crate::config::TransportSecurityConfig::default(),
            Vec::new(),
        )
        .await
        .expect("construct unheld-range test server");
        let (tx, rx) = channel(4, 4);
        tx.set_protocol_version(3);
        let (probe_state, _probe_updates) = watch::channel(PingProbeState::default());
        let room_id = RoomId::from_u128(5);
        let metadata = |sender: u128| DataDeliveryMetadata {
            class: crate::protocol::DeliveryClass::Reliable,
            key: None,
            from_player: PlayerId::from_u128(sender),
            room_id,
            epoch: 1,
            seq: 7,
        };

        // Fill the pending report so the next omission cannot coalesce: each
        // range comes from a different sender, so none of them merge.
        for sender in 0..DELIVERY_REPORT_MAX_GAPS as u128 {
            assert!(
                rx.record_unsupported_format(metadata(sender)),
                "range {sender} should fit the pending report"
            );
        }
        let overflow = metadata(DELIVERY_REPORT_MAX_GAPS as u128);

        {
            let mut accounting = SendAccounting::new(
                &rx,
                &server,
                &probe_state,
                overflow.from_player,
                Some(crate::protocol::DeliveryClass::Reliable),
            );
            assert!(
                !accounting.complete_unsupported(Some(overflow)),
                "the overflow range must require the pending report to be written first"
            );
            // The write that would make room is cancelled here: the send task's
            // close `select!` can drop it at any await, so `hold_unsupported`
            // never runs.
        }

        assert!(
            rx.abandoned_in_flight_write(),
            "an omission whose range was never held must fence the remaining queue"
        );
    }

    #[test]
    fn unsupported_binary_fallback_preflight_is_exact() {
        let message = |encoding, payload: &'static [u8]| ServerMessage::GameDataBinary {
            from_player: player_a(),
            encoding,
            payload: bytes::Bytes::from_static(payload),
            seq: Some(1),
            epoch: Some(1),
        };

        assert!(preflight_binary_fallback(
            &message(GameDataEncoding::MessagePack, &[0xc1]),
            GameDataEncoding::Json,
            None,
        )
        .is_unsupported());
        assert!(preflight_binary_fallback(
            &message(GameDataEncoding::Rkyv, &[0x01]),
            GameDataEncoding::Json,
            None,
        )
        .is_unsupported());
        assert!(!preflight_binary_fallback(
            &message(GameDataEncoding::MessagePack, &[0xc1]),
            GameDataEncoding::MessagePack,
            None,
        )
        .is_unsupported());
        assert!(!preflight_binary_fallback(
            &message(GameDataEncoding::MessagePack, &[0x01]),
            GameDataEncoding::Json,
            None,
        )
        .is_unsupported());
    }

    #[test]
    fn materializer_projects_exact_recipient_cohorts_and_codec_work() {
        let data = serde_json::json!({"move": "up", "n": 3});
        let text = ServerMessage::GameData {
            from_player: player_a(),
            data: data.clone(),
            seq: Some(42),
            epoch: Some(3),
            class: Some(crate::protocol::DeliveryClass::Reliable),
            key: None,
        };

        let v2_text = materialize_game_data_frame(
            &text,
            false,
            GameDataEncoding::Json,
            BinaryFallbackPreflight::NotNeeded,
            None,
        )
        .expect("v2 text projection");
        assert_eq!(
            v2_text.frame,
            serialize_json_frame(&ServerMessage::GameData {
                from_player: player_a(),
                data: data.clone(),
                seq: None,
                epoch: None,
                class: None,
                key: None,
            })
            .expect("expected v2 text")
        );
        assert_eq!(
            v2_text.work,
            SerializationWork {
                json_encodes: 1,
                ..SerializationWork::default()
            }
        );

        let v3_text = materialize_game_data_frame(
            &text,
            true,
            GameDataEncoding::Json,
            BinaryFallbackPreflight::NotNeeded,
            None,
        )
        .expect("v3 text projection");
        assert_eq!(
            v3_text.frame,
            serialize_json_frame(&text).expect("expected v3 text")
        );
        assert_eq!(
            v3_text.work,
            SerializationWork {
                json_encodes: 1,
                ..SerializationWork::default()
            }
        );

        let payload: Bytes = rmp_serde::to_vec_named(&data)
            .expect("MessagePack fixture")
            .into();
        let binary = ServerMessage::GameDataBinary {
            from_player: player_a(),
            encoding: GameDataEncoding::MessagePack,
            payload: payload.clone(),
            seq: Some(42),
            epoch: Some(3),
        };
        for supports_v3 in [false, true] {
            let direct = materialize_game_data_frame(
                &binary,
                supports_v3,
                GameDataEncoding::MessagePack,
                BinaryFallbackPreflight::NotNeeded,
                None,
            )
            .expect("direct MessagePack projection");
            let stamp = supports_v3.then_some((42, 3));
            assert_eq!(
                direct.frame,
                Message::Binary(
                    encode_binary_game_data(
                        player_a(),
                        GameDataEncoding::MessagePack,
                        &payload,
                        stamp.map(|value| value.0),
                        stamp.map(|value| value.1),
                    )
                    .expect("expected direct binary")
                )
            );
            assert_eq!(
                direct.work,
                SerializationWork {
                    message_pack_encodes: 1,
                    ..SerializationWork::default()
                }
            );

            let fallback = materialize_game_data_frame(
                &binary,
                supports_v3,
                GameDataEncoding::Json,
                preflight_binary_fallback(&binary, GameDataEncoding::Json, None),
                None,
            )
            .expect("MessagePack-to-JSON fallback");
            let expected = ServerMessage::GameData {
                from_player: player_a(),
                data: data.clone(),
                seq: stamp.map(|value| value.0),
                epoch: stamp.map(|value| value.1),
                class: None,
                key: None,
            };
            assert_eq!(
                fallback.frame,
                serialize_json_frame(&expected).expect("expected fallback text")
            );
            assert_eq!(
                fallback.work,
                SerializationWork {
                    json_encodes: 1,
                    message_pack_decodes: 1,
                    ..SerializationWork::default()
                }
            );
        }

        assert!(matches!(
            materialize_game_data_frame(
                &ServerMessage::GameDataBinary {
                    from_player: player_a(),
                    encoding: GameDataEncoding::MessagePack,
                    payload,
                    seq: Some(42),
                    epoch: None,
                },
                true,
                GameDataEncoding::MessagePack,
                BinaryFallbackPreflight::NotNeeded,
                None,
            ),
            Err(GameDataMaterializationError::InvalidV3Stamp)
        ));
    }

    #[test]
    fn relay_cache_reuses_wire_cohorts_and_decodes_message_pack_once() {
        let data = serde_json::json!({"move": "up", "n": 3});
        let text = ServerMessage::GameData {
            from_player: player_a(),
            data: data.clone(),
            seq: Some(42),
            epoch: Some(3),
            class: Some(crate::protocol::DeliveryClass::Reliable),
            key: None,
        };
        let text_cache = RelayFrameCache::for_message(&text).expect("text relay cache");
        assert!(text_cache.supports(RelayFrameCohort::TextV2));
        assert!(text_cache.supports(RelayFrameCohort::TextV3));
        assert!(!text_cache.supports(RelayFrameCohort::BinaryDirectV3));
        for supports_v3 in [false, true] {
            let first = materialize_game_data_frame(
                &text,
                supports_v3,
                GameDataEncoding::Json,
                BinaryFallbackPreflight::NotNeeded,
                Some(&text_cache),
            )
            .expect("first text cohort projection");
            let reused = materialize_game_data_frame(
                &text,
                supports_v3,
                GameDataEncoding::Json,
                BinaryFallbackPreflight::NotNeeded,
                Some(&text_cache),
            )
            .expect("reused text cohort projection");
            assert_eq!(reused.frame, first.frame);
            assert_eq!(first.work.json_encodes, 1);
            assert_eq!(reused.work, SerializationWork::default());
        }

        let payload = rmp_serde::to_vec_named(&data).expect("MessagePack fixture");
        let binary = ServerMessage::GameDataBinary {
            from_player: player_a(),
            encoding: GameDataEncoding::MessagePack,
            payload: payload.into(),
            seq: Some(42),
            epoch: Some(3),
        };
        let binary_cache = RelayFrameCache::for_message(&binary).expect("binary relay cache");
        assert!(binary_cache.supports(RelayFrameCohort::BinaryDirectV2));
        assert!(binary_cache.supports(RelayFrameCohort::BinaryFallbackV3));
        assert!(!binary_cache.supports(RelayFrameCohort::TextV3));
        assert!(RelayFrameCache::for_message(&ServerMessage::RoomLeft).is_none());
        for (index, supports_v3) in [false, true].into_iter().enumerate() {
            let preflight =
                preflight_binary_fallback(&binary, GameDataEncoding::Json, Some(&binary_cache));
            let first = materialize_game_data_frame(
                &binary,
                supports_v3,
                GameDataEncoding::Json,
                preflight,
                Some(&binary_cache),
            )
            .expect("first fallback cohort projection");
            assert_eq!(first.work.json_encodes, 1);
            assert_eq!(first.work.message_pack_decodes, u64::from(index == 0));

            let preflight =
                preflight_binary_fallback(&binary, GameDataEncoding::Json, Some(&binary_cache));
            let reused = materialize_game_data_frame(
                &binary,
                supports_v3,
                GameDataEncoding::Json,
                preflight,
                Some(&binary_cache),
            )
            .expect("reused fallback cohort projection");
            assert_eq!(reused.frame, first.frame);
            assert_eq!(reused.work, SerializationWork::default());
        }

        for supports_v3 in [false, true] {
            let first = materialize_game_data_frame(
                &binary,
                supports_v3,
                GameDataEncoding::MessagePack,
                BinaryFallbackPreflight::NotNeeded,
                Some(&binary_cache),
            )
            .expect("first direct binary cohort projection");
            let reused = materialize_game_data_frame(
                &binary,
                supports_v3,
                GameDataEncoding::MessagePack,
                BinaryFallbackPreflight::NotNeeded,
                Some(&binary_cache),
            )
            .expect("reused direct binary cohort projection");
            assert_eq!(reused.frame, first.frame);
            assert_eq!(first.work.message_pack_encodes, 1);
            assert_eq!(reused.work, SerializationWork::default());
        }
    }

    #[test]
    fn wrong_kind_cache_bypasses_binary_preflight_and_materialization() {
        let text = ServerMessage::GameData {
            from_player: player_a(),
            data: serde_json::json!({"kind": "text"}),
            seq: Some(1),
            epoch: Some(1),
            class: None,
            key: None,
        };
        let text_cache = RelayFrameCache::for_message(&text).expect("text relay cache");

        for (index, data) in [
            serde_json::json!({"move": "up", "n": 3}),
            serde_json::json!({"move": "down", "n": 9}),
        ]
        .into_iter()
        .enumerate()
        {
            let payload = rmp_serde::to_vec_named(&data).expect("MessagePack fixture");
            let binary = ServerMessage::GameDataBinary {
                from_player: player_a(),
                encoding: GameDataEncoding::MessagePack,
                payload: payload.into(),
                seq: Some(index as u64 + 1),
                epoch: Some(1),
            };
            let expected_preflight =
                preflight_binary_fallback(&binary, GameDataEncoding::Json, None);
            let expected = materialize_game_data_frame(
                &binary,
                true,
                GameDataEncoding::Json,
                expected_preflight,
                None,
            )
            .expect("uncached fallback projection");

            let mismatched_preflight =
                preflight_binary_fallback(&binary, GameDataEncoding::Json, Some(&text_cache));
            let actual = materialize_game_data_frame(
                &binary,
                true,
                GameDataEncoding::Json,
                mismatched_preflight,
                Some(&text_cache),
            )
            .expect("wrong-kind cache must use the uncached projection");

            assert_eq!(actual.frame, expected.frame, "message {index} wire frame");
            assert_eq!(actual.work, expected.work, "message {index} codec work");
        }
    }

    #[test]
    fn relay_cache_attributes_one_decode_across_concurrent_fallback_cohorts() {
        let data = serde_json::json!({"move": "up", "n": 3});
        let payload = rmp_serde::to_vec_named(&data).expect("MessagePack fixture");
        let binary = Arc::new(ServerMessage::GameDataBinary {
            from_player: player_a(),
            encoding: GameDataEncoding::MessagePack,
            payload: payload.into(),
            seq: Some(42),
            epoch: Some(3),
        });
        let cache = Arc::new(RelayFrameCache::for_message(&binary).expect("binary relay cache"));
        let (preflight_ready_tx, preflight_ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();

        let delayed_binary = Arc::clone(&binary);
        let delayed_cache = Arc::clone(&cache);
        let delayed = std::thread::spawn(move || {
            let preflight = preflight_binary_fallback(
                delayed_binary.as_ref(),
                GameDataEncoding::Json,
                Some(&delayed_cache),
            );
            preflight_ready_tx
                .send(())
                .expect("test coordinator must observe decoded preflight");
            release_rx
                .recv()
                .expect("test coordinator must release delayed cohort");
            materialize_game_data_frame(
                delayed_binary.as_ref(),
                false,
                GameDataEncoding::Json,
                preflight,
                Some(&delayed_cache),
            )
            .expect("delayed fallback projection")
        });

        preflight_ready_rx
            .recv()
            .expect("first thread must initialize the shared decode");
        let winning_preflight =
            preflight_binary_fallback(&binary, GameDataEncoding::Json, Some(&cache));
        let winning = materialize_game_data_frame(
            &binary,
            true,
            GameDataEncoding::Json,
            winning_preflight,
            Some(&cache),
        )
        .expect("winning fallback projection");
        release_tx
            .send(())
            .expect("delayed thread must still be waiting");
        let delayed = delayed.join().expect("delayed projection must not panic");

        assert_ne!(
            winning.frame, delayed.frame,
            "v3 and frozen-v2 fallbacks must initialize distinct wire cohorts"
        );
        assert_eq!(
            winning.work.message_pack_decodes + delayed.work.message_pack_decodes,
            1,
            "the shared MessagePack decode must be attributed exactly once across \
             concurrent v2/v3 fallback initialization"
        );
        assert_eq!(
            winning.work.json_encodes + delayed.work.json_encodes,
            2,
            "both exact fallback cohorts must serialize once"
        );
    }

    #[test]
    fn relay_cache_initializes_one_frame_across_same_fallback_cohort_race() {
        let data = serde_json::json!({"move": "up", "n": 3});
        let payload = rmp_serde::to_vec_named(&data).expect("MessagePack fixture");
        let binary = Arc::new(ServerMessage::GameDataBinary {
            from_player: player_a(),
            encoding: GameDataEncoding::MessagePack,
            payload: payload.into(),
            seq: Some(42),
            epoch: Some(3),
        });
        let cache = Arc::new(RelayFrameCache::for_message(&binary).expect("binary relay cache"));
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut threads = Vec::new();

        for _ in 0..2 {
            let binary = Arc::clone(&binary);
            let cache = Arc::clone(&cache);
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                let preflight = preflight_binary_fallback(
                    binary.as_ref(),
                    GameDataEncoding::Json,
                    Some(&cache),
                );
                barrier.wait();
                materialize_game_data_frame(
                    binary.as_ref(),
                    false,
                    GameDataEncoding::Json,
                    preflight,
                    Some(&cache),
                )
                .expect("racing fallback projection")
            }));
        }

        barrier.wait();
        let frames: [MaterializedFrame; 2] = threads
            .into_iter()
            .map(|thread| thread.join().expect("fallback projection must not panic"))
            .collect::<Vec<_>>()
            .try_into()
            .expect("exactly two fallback projections");
        let [left, right] = frames;
        assert_eq!(left.frame, right.frame);
        assert_eq!(
            [&left, &right]
                .into_iter()
                .map(|frame| frame.work.json_encodes)
                .sum::<u64>(),
            1,
            "one same-cohort racer must initialize the JSON frame"
        );
        assert_eq!(
            [&left, &right]
                .into_iter()
                .map(|frame| frame.work.message_pack_decodes)
                .sum::<u64>(),
            1,
            "the shared MessagePack decode must be attributed exactly once"
        );
    }

    fn hex(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push_str(&format!("{b:02x}"));
        }
        out
    }

    struct SerializationFailure;

    impl Serialize for SerializationFailure {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            Err(serde::ser::Error::custom(
                "intentional serialization failure",
            ))
        }
    }

    #[test]
    fn json_serialization_failure_is_not_a_successful_frame() {
        let error = serialize_json_text(&SerializationFailure)
            .expect_err("a serializer failure must propagate to the send path");
        assert!(error
            .to_string()
            .contains("intentional serialization failure"));
    }

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct DecodedLegacyBinaryGameDataFrame {
        from_player: PlayerId,
        encoding: GameDataEncoding,
        #[serde(with = "serde_bytes")]
        payload: Vec<u8>,
    }

    #[test]
    fn binary_game_data_encoder_emits_bare_message_pack_frame() {
        let payload = Bytes::from_static(&[0x01, 0x02, 0x03, 0x04]);
        // Pre-v3 form (`seq`/`epoch` both None): bytes are FROZEN — they must
        // never drift, v3 or not (pre-v3 recipients keep receiving exactly this
        // frame).
        let wire = encode_binary_game_data(
            player_a(),
            GameDataEncoding::MessagePack,
            &payload,
            None,
            None,
        )
        .expect("production binary encode");

        assert_eq!(
            hex(&wire),
            "83ab66726f6d5f706c61796572c41000112233445566778899aabbccddeeffa8656e636f64696e67ac6d6573736167655f7061636ba77061796c6f6164c40401020304",
            "binary wire frame drift (BREAKING v2 wire change?)"
        );

        let decoded: DecodedLegacyBinaryGameDataFrame =
            rmp_serde::from_slice(&wire).expect("bare binary frame decodes");
        assert_eq!(
            decoded,
            DecodedLegacyBinaryGameDataFrame {
                from_player: player_a(),
                encoding: GameDataEncoding::MessagePack,
                payload: payload.to_vec(),
            }
        );

        let frame = LegacyBinaryGameDataFrame {
            from_player: player_a(),
            encoding: GameDataEncoding::MessagePack,
            payload: &payload,
        };
        assert_eq!(
            serde_json::to_value(frame).expect("json value"),
            serde_json::json!({
                "from_player": PLAYER_A_STR,
                "encoding": "message_pack",
                "payload": [1, 2, 3, 4]
            }),
            "binary frame field-name/casing drift (BREAKING v2 wire change?)"
        );
    }

    /// V3 uses the same mandatory envelope for every opaque payload encoding.
    /// MessagePack remains byte-identical to its previously stamped form.
    #[test]
    fn binary_game_data_encoder_envelopes_every_v3_encoding() {
        let payload = Bytes::from_static(&[0x01, 0x02, 0x03, 0x04]);
        let cases = [
            (
                GameDataEncoding::Json,
                "85ab66726f6d5f706c61796572c41000112233445566778899aabbccddeeffa8656e636f64696e67a46a736f6ea77061796c6f6164c40401020304a373657107a565706f636803",
            ),
            (
                GameDataEncoding::MessagePack,
                "85ab66726f6d5f706c61796572c41000112233445566778899aabbccddeeffa8656e636f64696e67ac6d6573736167655f7061636ba77061796c6f6164c40401020304a373657107a565706f636803",
            ),
            (
                GameDataEncoding::Rkyv,
                "85ab66726f6d5f706c61796572c41000112233445566778899aabbccddeeffa8656e636f64696e67a4726b7976a77061796c6f6164c40401020304a373657107a565706f636803",
            ),
        ];

        for (encoding, expected_hex) in cases {
            let wire = encode_binary_game_data(player_a(), encoding, &payload, Some(7), Some(3))
                .expect("production binary encode with stamp");
            assert_eq!(
                hex(&wire),
                expected_hex,
                "v3 {encoding:?} binary wire frame drift (BREAKING v3 wire change?)"
            );

            let decoded =
                decode_v3_binary_game_data(&wire).expect("strict v3 binary envelope decodes");
            assert_eq!(
                decoded,
                DecodedV3BinaryGameDataFrame {
                    from_player: player_a(),
                    encoding,
                    payload: payload.to_vec(),
                    seq: 7,
                    epoch: 3,
                }
            );
        }

        let frame = V3BinaryGameDataFrame {
            from_player: player_a(),
            encoding: GameDataEncoding::MessagePack,
            payload: &payload,
            seq: 7,
            epoch: 3,
        };
        assert_eq!(
            serde_json::to_value(frame).expect("json value"),
            serde_json::json!({
                "from_player": PLAYER_A_STR,
                "encoding": "message_pack",
                "payload": [1, 2, 3, 4],
                "seq": 7,
                "epoch": 3
            }),
            "v3 binary frame field-name/casing drift (BREAKING v3 wire change?)"
        );
    }

    #[test]
    fn binary_game_data_encoder_matches_named_encoding_at_bin_boundaries() {
        for payload_len in [0, 255, 256, 65_535, 65_536] {
            let payload = Bytes::from(vec![0xa5; payload_len]);
            let legacy = LegacyBinaryGameDataFrame {
                from_player: player_a(),
                encoding: GameDataEncoding::MessagePack,
                payload: &payload,
            };
            assert_eq!(
                encode_binary_game_data(
                    player_a(),
                    GameDataEncoding::MessagePack,
                    &payload,
                    None,
                    None,
                )
                .expect("preallocated legacy binary encode")
                .as_ref(),
                rmp_serde::to_vec_named(&legacy)
                    .expect("reference legacy binary encode")
                    .as_slice(),
                "legacy payload length {payload_len} changed named MessagePack bytes"
            );

            for encoding in [
                GameDataEncoding::Json,
                GameDataEncoding::MessagePack,
                GameDataEncoding::Rkyv,
            ] {
                let v3 = V3BinaryGameDataFrame {
                    from_player: player_a(),
                    encoding,
                    payload: &payload,
                    seq: u64::MAX,
                    epoch: u32::MAX,
                };
                assert_eq!(
                    encode_binary_game_data(
                        player_a(),
                        encoding,
                        &payload,
                        Some(u64::MAX),
                        Some(u32::MAX),
                    )
                    .expect("preallocated v3 binary encode")
                    .as_ref(),
                    rmp_serde::to_vec_named(&v3)
                        .expect("reference v3 binary encode")
                        .as_slice(),
                    "v3 {encoding:?} payload length {payload_len} changed named MessagePack bytes"
                );
            }
        }
    }

    #[test]
    fn json_fallback_preallocation_preserves_wire_bytes_across_growth_paths() {
        let data = serde_json::json!({
            "escaped": "\"\\\n".repeat(300),
            "values": [0, 1, 127, 128, 255, 256, u32::MAX],
        });
        let fallback = BorrowedGameDataEnvelope::new(player_a(), &data, Some(7), Some(3));
        let expected = serialize_json_frame(&fallback).expect("reference JSON fallback encode");

        for estimated_payload_len in [0, 1, 127, 128, 255, 256, 1_024, 4_096] {
            assert_eq!(
                serialize_json_fallback_frame(&fallback, estimated_payload_len)
                    .expect("preallocated JSON fallback encode"),
                expected,
                "estimated payload length {estimated_payload_len} changed JSON fallback bytes"
            );
        }
    }

    #[test]
    fn json_relay_preallocation_preserves_wire_bytes_across_value_shapes() {
        let cases = [
            ("null", serde_json::Value::Null),
            ("empty object", serde_json::json!({})),
            ("empty array", serde_json::json!([])),
            (
                "nested gameplay state",
                serde_json::json!({
                    "entity": 42,
                    "position": [123.25, -87.5, 9.75],
                    "active": true,
                    "children": [{"id": 1}, {"id": 2}],
                }),
            ),
            (
                "escape-heavy unicode",
                serde_json::json!({"text": "\"\\\n\u{0000}🦀".repeat(300)}),
            ),
        ];

        for (description, data) in cases {
            let message = ServerMessage::GameData {
                from_player: player_a(),
                data: data.clone(),
                seq: Some(u64::MAX),
                epoch: Some(u32::MAX),
                class: Some(crate::protocol::DeliveryClass::Reliable),
                key: None,
            };
            assert_eq!(
                serialize_json_relay_frame(&message, &data)
                    .expect("preallocated JSON relay encode"),
                serialize_json_frame(&message).expect("reference JSON relay encode"),
                "{description} changed JSON relay wire bytes"
            );
        }
    }

    #[test]
    fn json_relay_estimate_budget_charges_float_uncertainty() {
        let representative_float_count =
            serde_json::Value::Array(vec![serde_json::json!(123.25); 7]);
        assert!(
            estimated_json_value_len(&representative_float_count).is_some(),
            "the representative relay's seven floats must retain pre-sizing"
        );

        let last_pre_sized_float_array =
            serde_json::Value::Array(vec![serde_json::json!(123.25); 10]);
        assert!(
            estimated_json_value_len(&last_pre_sized_float_array).is_some(),
            "ten floats fit the uncertainty-weighted work budget"
        );

        let first_fallback_float_array =
            serde_json::Value::Array(vec![serde_json::json!(123.25); 11]);
        assert!(
            estimated_json_value_len(&first_fallback_float_array).is_none(),
            "eleven floats must use the one-pass reference serializer"
        );
    }

    #[test]
    fn numeric_dense_near_limit_json_relay_does_not_retain_amplified_capacity() {
        const DEFAULT_MESSAGE_LIMIT: usize = 64 * 1_024;

        let data = serde_json::Value::Array(vec![serde_json::json!(0); 30_000]);
        let message = ServerMessage::GameData {
            from_player: player_a(),
            data: data.clone(),
            seq: Some(u64::MAX),
            epoch: Some(u32::MAX),
            class: Some(crate::protocol::DeliveryClass::Reliable),
            key: None,
        };
        let expected = serialize_json_frame(&message).expect("reference JSON relay encode");
        let actual =
            serialize_json_relay_frame(&message, &data).expect("preallocated JSON relay encode");
        assert_eq!(actual, expected, "numeric-dense relay changed wire bytes");

        let Message::Text(text) = actual else {
            panic!("JSON relay must produce a text frame");
        };
        let wire_len = text.len();
        assert!(
            (60_000..=DEFAULT_MESSAGE_LIMIT).contains(&wire_len),
            "fixture must remain near the default message limit, got {wire_len} bytes"
        );
        let retained_capacity = Bytes::from(text)
            .try_into_mut()
            .expect("sole-owner text frame must expose its retained allocation")
            .capacity();
        assert!(
            retained_capacity <= DEFAULT_MESSAGE_LIMIT,
            "numeric-dense {wire_len}-byte relay retained {retained_capacity} bytes"
        );
    }

    #[test]
    fn escape_dense_near_limit_json_relay_does_not_retain_growth_slack() {
        const DEFAULT_MESSAGE_LIMIT: usize = 64 * 1_024;

        let data = serde_json::json!({"text": "\u{0000}".repeat(10_000)});
        let message = ServerMessage::GameData {
            from_player: player_a(),
            data: data.clone(),
            seq: Some(u64::MAX),
            epoch: Some(u32::MAX),
            class: Some(crate::protocol::DeliveryClass::Reliable),
            key: None,
        };
        let expected = serialize_json_frame(&message).expect("reference JSON relay encode");
        let actual =
            serialize_json_relay_frame(&message, &data).expect("preallocated JSON relay encode");
        assert_eq!(actual, expected, "escape-dense relay changed wire bytes");

        let Message::Text(text) = actual else {
            panic!("JSON relay must produce a text frame");
        };
        let wire_len = text.len();
        assert!(
            (60_000..=DEFAULT_MESSAGE_LIMIT).contains(&wire_len),
            "fixture must remain near the default message limit, got {wire_len} bytes"
        );
        let retained_capacity = Bytes::from(text)
            .try_into_mut()
            .expect("sole-owner text frame must expose its retained allocation")
            .capacity();
        assert!(
            retained_capacity <= wire_len.saturating_add(JSON_RELAY_HEADROOM),
            "escape-dense {wire_len}-byte relay retained {retained_capacity} bytes"
        );
    }

    #[test]
    fn json_fallback_capacity_overflow_is_reported() {
        for payload_len in [isize::MAX as usize, usize::MAX] {
            let error = serialize_json_fallback_frame(&0_u8, payload_len)
                .expect_err("capacity overflow must fail before allocation");
            assert!(error.contains("capacity overflow"));
        }
    }

    #[test]
    fn binary_frame_capacity_overflow_is_reported() {
        for payload_len in [isize::MAX as usize, usize::MAX] {
            let error = encode_named_binary_frame(&0_u8, payload_len)
                .expect_err("capacity overflow must fail before allocation");
            assert!(error.contains("capacity overflow"));
        }

        let mut bytes = Vec::new();
        let error = FallibleVecWriter::new(&mut bytes, "binary")
            .reserve_for_write(usize::MAX)
            .expect_err("writer length overflow must not be reported as allocation failure");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("capacity overflow"));
    }

    #[test]
    fn binary_frame_growth_preserves_named_encoding() {
        let body = "x".repeat(256);
        assert_eq!(
            encode_named_binary_frame(&body, 0).expect("fallible growth encode"),
            rmp_serde::to_vec_named(&body).expect("reference growth encode")
        );
    }

    #[test]
    fn v2_raw_binary_encodings_return_payload_unchanged() {
        let payload = Bytes::from_static(br#"{"move":"up"}"#);

        for encoding in [GameDataEncoding::Json, GameDataEncoding::Rkyv] {
            assert_eq!(
                encode_binary_game_data(player_a(), encoding, &payload, None, None)
                    .expect("v2 raw passthrough"),
                payload
            );
            let projected = encode_binary_game_data(player_a(), encoding, &payload, None, None)
                .expect("v2 raw passthrough");
            assert_eq!(
                projected.as_ptr(),
                payload.as_ptr(),
                "v2 raw passthrough must reuse the shared payload allocation"
            );
        }
    }

    #[test]
    fn binary_delivery_stamp_must_be_complete_and_nonzero() {
        let payload = Bytes::from_static(b"opaque");
        for (seq, epoch) in [
            (Some(1), None),
            (None, Some(1)),
            (Some(0), Some(1)),
            (Some(1), Some(0)),
        ] {
            assert!(
                encode_binary_game_data(player_a(), GameDataEncoding::Json, &payload, seq, epoch,)
                    .is_err(),
                "invalid stamp {seq:?}/{epoch:?} was accepted"
            );
        }
    }

    /// The borrowed pre-v3 shadow must serialize byte-identically to the enum
    /// with `seq: None` — this is what keeps pre-v3 recipients' text frames
    /// unchanged when the shared broadcast `Arc` carries a stamp.
    #[test]
    fn legacy_game_data_envelope_matches_enum_bytes_without_seq() {
        let data = serde_json::json!({ "move": "up", "n": 3 });
        let legacy = LegacyGameDataEnvelope::new(player_a(), &data);
        let enum_form = crate::protocol::ServerMessage::GameData {
            from_player: player_a(),
            data: data.clone(),
            seq: None,
            epoch: None,
            class: None,
            key: None,
        };

        let legacy_json = serde_json::to_string(&legacy).expect("legacy json");
        let enum_json = serde_json::to_string(&enum_form).expect("enum json");
        assert_eq!(
            legacy_json, enum_json,
            "pre-v3 shadow must be byte-identical to the unstamped enum form"
        );
        assert_eq!(
            legacy_json,
            format!(
                r#"{{"type":"GameData","data":{{"from_player":"{PLAYER_A_STR}","data":{{"move":"up","n":3}}}}}}"#
            ),
            "pre-v3 GameData text frame drift (BREAKING v2 wire change?)"
        );

        // And the stamped enum form differs ONLY by the trailing seq + epoch
        // keys (production always pairs them for a v3 recipient).
        let stamped = crate::protocol::ServerMessage::GameData {
            from_player: player_a(),
            data,
            seq: Some(42),
            epoch: Some(3),
            class: None,
            key: None,
        };
        let stamped_json = serde_json::to_string(&stamped).expect("stamped json");
        assert_eq!(
            stamped_json,
            format!(
                r#"{{"type":"GameData","data":{{"from_player":"{PLAYER_A_STR}","data":{{"move":"up","n":3}},"seq":42,"epoch":3}}}}"#
            ),
            "v3 GameData text frame drift (BREAKING v3 wire change?)"
        );
    }
}
