use crate::coordination::outbound_queue::{DataDeliveryMetadata, OutboundReceiver};
use crate::protocol::{ErrorCode, GameDataEncoding, PlayerId, ServerMessage};
use crate::server::EnhancedGameServer;
use axum::extract::ws::{Message, WebSocket};
use futures_util::SinkExt;
use rmp_serde::{from_slice, to_vec_named};
use serde::Serialize;
use std::sync::Arc;

/// Why a binary game-data fallback could not be delivered.
enum BinaryFallbackError {
    /// The payload cannot be represented for this recipient (e.g. an rkyv
    /// payload relayed to a JSON-only client). The connection is healthy; the
    /// recipient must be told loudly instead of silently receiving nothing.
    Undeliverable(String),
    /// The socket write failed; the connection is closing.
    ConnectionClosed,
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
    player_id: &PlayerId,
    recipient_supports_v3: bool,
    recipient_format: GameDataEncoding,
    metadata: Option<DataDeliveryMetadata>,
    accounting: &mut SendAccounting<'_>,
) -> Result<SendDisposition, ()> {
    let mut disposition = SendDisposition::Written;
    match message.as_ref() {
        ServerMessage::GameDataBinary {
            from_player,
            encoding,
            payload,
            seq,
            epoch,
        } => {
            // Per-recipient v3 gate: the relay stamp (seq + incarnation epoch)
            // reaches only recipients that negotiated protocol v3+ — a pre-v3
            // (v2) recipient's bytes stay byte-identical to the frozen v2 wire.
            let (seq, epoch) = if recipient_supports_v3 {
                match (*seq, *epoch) {
                    (Some(seq), Some(epoch)) => (Some(seq), Some(epoch)),
                    _ => {
                        tracing::error!(
                            %player_id,
                            %from_player,
                            seq = ?seq,
                            epoch = ?epoch,
                            "Protocol-v3 binary game data lacked a complete delivery stamp; closing fail-closed"
                        );
                        return Err(());
                    }
                }
            } else {
                (None, None)
            };
            if recipient_format == *encoding {
                match encode_binary_game_data(*from_player, *encoding, payload, seq, epoch) {
                    Ok(frame_bytes) => {
                        if sender
                            .send(Message::Binary(frame_bytes.into()))
                            .await
                            .is_err()
                        {
                            tracing::warn!(
                                %player_id,
                                "Failed to send binary game data, connection closed"
                            );
                            return Err(());
                        }
                    }
                    Err(err) => {
                        tracing::error!(
                            %player_id,
                            %from_player,
                            encoding = ?encoding,
                            error = %err,
                            "Failed to encode binary game data; attempting JSON fallback"
                        );
                        let fallback = send_binary_fallback(
                            sender,
                            *from_player,
                            *encoding,
                            payload,
                            seq,
                            epoch,
                            player_id,
                        )
                        .await;
                        disposition = notify_or_close_on_fallback_failure(
                            sender,
                            fallback,
                            *from_player,
                            *encoding,
                            player_id,
                            recipient_supports_v3,
                            metadata,
                            accounting,
                        )
                        .await?;
                    }
                }
            } else {
                let fallback = send_binary_fallback(
                    sender,
                    *from_player,
                    *encoding,
                    payload,
                    seq,
                    epoch,
                    player_id,
                )
                .await;
                disposition = notify_or_close_on_fallback_failure(
                    sender,
                    fallback,
                    *from_player,
                    *encoding,
                    player_id,
                    recipient_supports_v3,
                    metadata,
                    accounting,
                )
                .await?;
            }
        }
        // A stamped text relay bound for a pre-v3 (v2) recipient: serialize the
        // borrowed legacy shadow (seq + epoch stripped) instead of cloning the
        // payload, keeping pre-v3 bytes identical at ~the same cost as the
        // pre-v3 serialization itself. v3 recipients (and unstamped messages)
        // fall through to the shared-Arc path below untouched. The guard fires
        // when EITHER stamp is present (they are stamped together today, but
        // keying on both — not just `seq` — keeps the strip robust if the fields
        // ever diverge; `seq`/`epoch` are referenced only by the guard).
        ServerMessage::GameData {
            from_player,
            data,
            seq,
            epoch,
            class,
            key,
        } if (seq.is_some() || epoch.is_some() || class.is_some() || key.is_some())
            && !recipient_supports_v3 =>
        {
            let legacy = LegacyGameDataEnvelope::new(*from_player, data);
            send_serialized_text(sender, &legacy, player_id).await?;
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
    player_id: PlayerId,
    class: Option<crate::protocol::DeliveryClass>,
    resolved: bool,
}

impl<'a> SendAccounting<'a> {
    pub(super) fn new(
        receiver: &'a OutboundReceiver,
        server: &'a Arc<EnhancedGameServer>,
        player_id: PlayerId,
        class: Option<crate::protocol::DeliveryClass>,
    ) -> Self {
        Self {
            receiver,
            server,
            player_id,
            class,
            resolved: false,
        }
    }

    pub(super) fn complete_written(&mut self) {
        if let Some(class) = self.class {
            self.receiver.record_written(class);
        }
        self.resolved = true;
    }

    fn complete_unsupported(
        &mut self,
        metadata: Option<DataDeliveryMetadata>,
    ) -> Option<crate::protocol::DeliveryReportPayload> {
        let report = metadata.map(|metadata| self.receiver.record_unsupported_format(metadata));
        if metadata.is_none() {
            if let Some(class) = self.class {
                self.receiver.record_unsupported_class(class);
            }
        }
        self.record_drop_metrics();
        self.resolved = true;
        report
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
}

impl Drop for SendAccounting<'_> {
    fn drop(&mut self) {
        if self.resolved {
            return;
        }
        if let Some(class) = self.class {
            self.receiver.record_abandoned(class, 1);
        }
        self.record_drop_metrics();
    }
}

async fn send_binary_fallback(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    from_player: PlayerId,
    encoding: GameDataEncoding,
    payload: &[u8],
    seq: Option<u64>,
    epoch: Option<u32>,
    player_id: &PlayerId,
) -> Result<(), BinaryFallbackError> {
    let data =
        decode_binary_to_json(encoding, payload).map_err(BinaryFallbackError::Undeliverable)?;
    // `seq`/`epoch` were already gated per recipient by the caller, so the enum
    // form serializes correctly for v3 (present) and pre-v3 (absent) alike.
    let fallback = ServerMessage::GameData {
        from_player,
        data,
        seq,
        epoch,
        class: None,
        key: None,
    };
    send_text_message(sender, &fallback, player_id)
        .await
        .map_err(|()| BinaryFallbackError::ConnectionClosed)
}

/// Handle a failed binary fallback without ever silently dropping game data:
/// a dead socket closes the connection (propagated as `Err`), while a payload
/// that genuinely cannot be represented for this recipient is counted as
/// dropped and replaced by an explicit error frame so the recipient knows it
/// is missing data (e.g. an rkyv-encoded room relayed to a JSON-only client).
async fn notify_or_close_on_fallback_failure(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    fallback: Result<(), BinaryFallbackError>,
    from_player: PlayerId,
    encoding: GameDataEncoding,
    player_id: &PlayerId,
    recipient_supports_v3: bool,
    metadata: Option<DataDeliveryMetadata>,
    accounting: &mut SendAccounting<'_>,
) -> Result<SendDisposition, ()> {
    match fallback {
        Ok(()) => Ok(SendDisposition::Written),
        Err(BinaryFallbackError::ConnectionClosed) => {
            tracing::warn!(
                %player_id,
                %from_player,
                "Failed to write binary game data fallback, connection closed"
            );
            Err(())
        }
        Err(BinaryFallbackError::Undeliverable(reason)) => {
            tracing::warn!(
                %player_id,
                %from_player,
                encoding = ?encoding,
                reason = %reason,
                "Game data undeliverable to this recipient; sending an error notice instead"
            );
            let report = accounting.complete_unsupported(metadata);
            if recipient_supports_v3 {
                let Some(report) = report else {
                    tracing::error!(
                        %player_id,
                        %from_player,
                        "Stamped v3 binary fallback lacked delivery metadata; closing fail-closed"
                    );
                    return Err(());
                };
                let report = ServerMessage::DeliveryReport(Box::new(report));
                send_text_message(sender, &report, player_id).await?;
            }
            let notice = ServerMessage::Error {
                message: format!(
                    "Undeliverable game data from player {from_player} \
                     ({} payload cannot be converted for this connection): {reason}",
                    encoding.as_wire_str()
                ),
                error_code: Some(ErrorCode::UnsupportedGameDataFormat),
            };
            send_text_message(sender, &notice, player_id).await?;
            Ok(SendDisposition::AccountedDrop)
        }
    }
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

/// The exact legacy struct serialized onto the wire for v2 MessagePack
/// game-data frames.
///
/// IMPORTANT: binary frames do NOT travel through the `ServerMessage` enum's
/// `{type, data}` envelope. The `ServerMessage::GameDataBinary` variant is only
/// an *in-memory* carrier used to route the payload through the broadcast layer;
/// `send_single_message` intercepts it and instead serializes this bare struct
/// via `rmp_serde::to_vec_named` (see `encode_binary_game_data`). V2 JSON and
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

/// Encodes a binary game-data frame exactly as production puts it on the wire.
///
/// This is the single source of truth for the binary send path. Keep this
/// private to the websocket module so the wire frame layout can be tested
/// without becoming public library API.
///
/// `seq`/`epoch` must already be gated per recipient: both present for v3, both
/// absent for v2. V3 always uses the MessagePack metadata envelope while keeping
/// `payload` opaque. V2 retains its historical representation byte-for-byte:
/// the legacy MessagePack envelope or raw JSON/rkyv passthrough.
pub(super) fn encode_binary_game_data(
    from_player: PlayerId,
    encoding: GameDataEncoding,
    payload: &[u8],
    seq: Option<u64>,
    epoch: Option<u32>,
) -> Result<Vec<u8>, String> {
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
            to_vec_named(&frame).map_err(|err| err.to_string())
        }
        (None, None) => match encoding {
            GameDataEncoding::MessagePack => to_vec_named(&LegacyBinaryGameDataFrame {
                from_player,
                encoding,
                payload,
            })
            .map_err(|err| err.to_string()),
            GameDataEncoding::Json | GameDataEncoding::Rkyv => Ok(payload.to_vec()),
        },
        _ => Err("binary delivery seq and epoch must be present or absent together".to_string()),
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
        let payload: &[u8] = &[0x01, 0x02, 0x03, 0x04];
        // Pre-v3 form (`seq`/`epoch` both None): bytes are FROZEN — they must
        // never drift, v3 or not (pre-v3 recipients keep receiving exactly this
        // frame).
        let wire = encode_binary_game_data(
            player_a(),
            GameDataEncoding::MessagePack,
            payload,
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
            payload,
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
        let payload: &[u8] = &[0x01, 0x02, 0x03, 0x04];
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
            let wire = encode_binary_game_data(player_a(), encoding, payload, Some(7), Some(3))
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
            payload,
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
    fn v2_raw_binary_encodings_return_payload_unchanged() {
        let payload: &[u8] = br#"{"move":"up"}"#;

        for encoding in [GameDataEncoding::Json, GameDataEncoding::Rkyv] {
            assert_eq!(
                encode_binary_game_data(player_a(), encoding, payload, None, None)
                    .expect("v2 raw passthrough"),
                payload.to_vec()
            );
        }
    }

    #[test]
    fn binary_delivery_stamp_must_be_complete_and_nonzero() {
        let payload = b"opaque";
        for (seq, epoch) in [
            (Some(1), None),
            (None, Some(1)),
            (Some(0), Some(1)),
            (Some(1), Some(0)),
        ] {
            assert!(
                encode_binary_game_data(player_a(), GameDataEncoding::Json, payload, seq, epoch,)
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
