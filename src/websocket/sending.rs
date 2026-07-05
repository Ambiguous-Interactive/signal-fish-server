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

pub(super) async fn send_immediate_server_message(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    message: &ServerMessage,
) -> Result<(), axum::Error> {
    let payload = match serde_json::to_string(message) {
        Ok(payload) => payload,
        Err(err) => {
            tracing::error!(error = %err, "Failed to serialize server message");
            "{\"type\":\"error\",\"data\":{\"message\":\"Internal error\"}}".to_string()
        }
    };

    sender.send(Message::Text(payload.into())).await
}

pub(super) async fn send_single_message(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    message: Arc<ServerMessage>,
    player_id: &PlayerId,
    server: &Arc<EnhancedGameServer>,
) -> Result<(), ()> {
    match message.as_ref() {
        ServerMessage::GameDataBinary {
            from_player,
            encoding,
            payload,
            seq,
            epoch,
        } => {
            // Per-recipient v4 gate: the relay stamp (seq + incarnation epoch)
            // reaches only recipients that negotiated protocol v4+ — pre-v4
            // recipients' bytes stay byte-identical to the frozen v2/v3 wire.
            let v4 = server.client_supports_v4(player_id);
            let seq = seq.filter(|_| v4);
            let epoch = epoch.filter(|_| v4);
            if server.prefers_encoding(player_id, *encoding) {
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
                        notify_or_close_on_fallback_failure(
                            sender,
                            fallback,
                            *from_player,
                            *encoding,
                            player_id,
                            server,
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
                notify_or_close_on_fallback_failure(
                    sender,
                    fallback,
                    *from_player,
                    *encoding,
                    player_id,
                    server,
                )
                .await?;
            }
        }
        // A stamped text relay bound for a pre-v4 recipient: serialize the
        // borrowed legacy shadow (seq + epoch stripped) instead of cloning the
        // payload, keeping pre-v4 bytes identical at ~the same cost as the
        // pre-v4 serialization itself. v4 recipients (and unstamped messages)
        // fall through to the shared-Arc path below untouched. (seq and epoch
        // are always stamped together, so gating on `seq: Some(_)` also catches
        // the epoch.)
        ServerMessage::GameData {
            from_player,
            data,
            seq: Some(_),
            ..
        } if !server.client_supports_v4(player_id) => {
            let legacy = LegacyGameDataEnvelope::new(*from_player, data);
            send_serialized_text(sender, &legacy, player_id).await?;
        }
        // Broadcast room-snapshot frames carry a v4 incarnation `epoch` that
        // must never reach a pre-v4 recipient (their bytes stay byte-identical
        // to the frozen v2/v3 wire). Only the rare snapshot broadcasts hit this;
        // the common relay path above is untouched.
        ServerMessage::PlayerJoined { player }
            if player.epoch.is_some() && !server.client_supports_v4(player_id) =>
        {
            let mut player = player.clone();
            player.epoch = None;
            send_text_message(sender, &ServerMessage::PlayerJoined { player }, player_id).await?;
        }
        ServerMessage::PlayerReconnected {
            player_id: reconnected,
            epoch: Some(_),
        } if !server.client_supports_v4(player_id) => {
            let stripped = ServerMessage::PlayerReconnected {
                player_id: *reconnected,
                epoch: None,
            };
            send_text_message(sender, &stripped, player_id).await?;
        }
        other => {
            send_text_message(sender, other, player_id).await?;
        }
    }

    Ok(())
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
    // form serializes correctly for v4 (present) and pre-v4 (absent) alike.
    let fallback = ServerMessage::GameData {
        from_player,
        data,
        seq,
        epoch,
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
    server: &Arc<EnhancedGameServer>,
) -> Result<(), ()> {
    match fallback {
        Ok(()) => Ok(()),
        Err(BinaryFallbackError::ConnectionClosed) => {
            tracing::warn!(
                %player_id,
                %from_player,
                "Failed to write binary game data fallback, connection closed"
            );
            Err(())
        }
        Err(BinaryFallbackError::Undeliverable(reason)) => {
            let metrics = server.metrics();
            metrics.increment_websocket_messages_dropped();
            // Per-connection RelayStats ledger (populated only when
            // `websocket.delivery_stats_interval_secs` enabled tracking): an
            // undeliverable payload is the one drop a still-healthy
            // connection can accumulate, so it must be attributable.
            if let Some(stats) = metrics.connection_delivery_stats(player_id) {
                stats
                    .dropped_for_you
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            tracing::warn!(
                %player_id,
                %from_player,
                encoding = ?encoding,
                reason = %reason,
                "Game data undeliverable to this recipient; sending an error notice instead"
            );
            let notice = ServerMessage::Error {
                message: format!(
                    "Undeliverable game data from player {from_player} \
                     ({} payload cannot be converted for this connection): {reason}",
                    encoding.as_wire_str()
                ),
                error_code: Some(ErrorCode::UnsupportedGameDataFormat),
            };
            send_text_message(sender, &notice, player_id).await
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
/// the `ServerMessage` path and the borrowed pre-v4 GameData shadow
/// ([`LegacyGameDataEnvelope`]); `Err(())` means the connection closed.
async fn send_serialized_text<T: Serialize>(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    message: &T,
    player_id: &PlayerId,
) -> Result<(), ()> {
    let json_message = match serde_json::to_string(message) {
        Ok(json) => json,
        Err(e) => {
            tracing::error!(%player_id, "Failed to serialize message: {}", e);
            return Ok(());
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

/// Borrowed wire shadow of a `ServerMessage::GameData` frame WITHOUT the v4
/// `seq` stamp, used to serialize a stamped shared-`Arc` relay message for a
/// pre-v4 recipient without cloning the payload.
///
/// IMPORTANT: this must serialize byte-identically to
/// `ServerMessage::GameData { from_player, data, seq: None }` — the adjacently
/// tagged `{"type":"GameData","data":{"from_player":..,"data":..}}` envelope
/// (v2-frozen in `tests/v2_wire_golden.rs`). The unit tests below pin that
/// equivalence, mirroring how [`BinaryGameDataFrame`] freezes the bare binary
/// frame.
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

/// The exact struct serialized onto the wire for binary game-data frames.
///
/// IMPORTANT: binary frames do NOT travel through the `ServerMessage` enum's
/// `{type, data}` envelope. The `ServerMessage::GameDataBinary` variant is only
/// an *in-memory* carrier used to route the payload through the broadcast layer;
/// `send_single_message` intercepts it and instead serializes this bare struct
/// via `rmp_serde::to_vec_named` (see `encode_binary_game_data`). The map keys
/// are therefore `from_player`/`encoding`/`payload` with NO `type`/`data`
/// wrapper. Golden wire tests must freeze the bytes produced from this struct,
/// not the enum variant.
#[derive(Serialize)]
struct BinaryGameDataFrame<'a> {
    from_player: PlayerId,
    encoding: GameDataEncoding,
    #[serde(with = "serde_bytes")]
    payload: &'a [u8],
    /// Server-stamped relay sequence (protocol v4): same counter and
    /// semantics as `ServerMessage::GameData::seq`. Present as a trailing
    /// `seq` map key only when the recipient negotiated v4 (the caller gates
    /// it), so pre-v4 recipients' frames stay byte-identical to the frozen
    /// vectors below.
    #[serde(skip_serializing_if = "Option::is_none")]
    seq: Option<u64>,
    /// Server-tracked incarnation epoch (protocol v4): same counter and
    /// semantics as `ServerMessage::GameData::epoch`, riding beside `seq` as a
    /// trailing `epoch` map key. Gated per recipient by the caller (present
    /// only for v4), so pre-v4 frames stay byte-identical to the frozen vectors.
    #[serde(skip_serializing_if = "Option::is_none")]
    epoch: Option<u32>,
}

/// Encodes a binary game-data frame exactly as production puts it on the wire.
///
/// This is the single source of truth for the binary send path. Keep this
/// private to the websocket module so the wire frame layout can be tested
/// without becoming public library API.
///
/// `seq`/`epoch` must already be gated per recipient (v4+ only). Only the
/// MessagePack frame has an envelope to carry them; the `Json` / `Rkyv`
/// passthrough paths forward the sender's raw bytes and therefore cannot attach
/// a stamp — recipients of those encodings rely on the text-relay stamp instead.
pub(super) fn encode_binary_game_data(
    from_player: PlayerId,
    encoding: GameDataEncoding,
    payload: &[u8],
    seq: Option<u64>,
    epoch: Option<u32>,
) -> Result<Vec<u8>, String> {
    match encoding {
        GameDataEncoding::MessagePack => {
            let frame = BinaryGameDataFrame {
                from_player,
                encoding,
                payload,
                seq,
                epoch,
            };
            to_vec_named(&frame).map_err(|err| err.to_string())
        }
        GameDataEncoding::Json => Ok(payload.to_vec()),
        GameDataEncoding::Rkyv => {
            // Rkyv data is already in zero-copy binary format, pass through directly
            // The payload contains the rkyv-serialized data from the client
            Ok(payload.to_vec())
        }
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
    use serde::Deserialize;
    use uuid::Uuid;

    const PLAYER_A_STR: &str = "00000000-0000-0000-0000-00000000000a";

    fn player_a() -> Uuid {
        Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0000_000a)
    }

    fn hex(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push_str(&format!("{b:02x}"));
        }
        out
    }

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct DecodedBinaryGameDataFrame {
        from_player: PlayerId,
        encoding: GameDataEncoding,
        #[serde(with = "serde_bytes")]
        payload: Vec<u8>,
        #[serde(default)]
        seq: Option<u64>,
        #[serde(default)]
        epoch: Option<u32>,
    }

    #[test]
    fn binary_game_data_encoder_emits_bare_message_pack_frame() {
        let payload: &[u8] = &[0x01, 0x02, 0x03, 0x04];
        // Pre-v4 form (`seq`/`epoch` both None): bytes are FROZEN — they must
        // never drift, v4 or not (pre-v4 recipients keep receiving exactly this
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
            "83ab66726f6d5f706c61796572c4100000000000000000000000000000000aa8656e636f64696e67ac6d6573736167655f7061636ba77061796c6f6164c40401020304",
            "binary wire frame drift (BREAKING v2 wire change?)"
        );

        let decoded: DecodedBinaryGameDataFrame =
            rmp_serde::from_slice(&wire).expect("bare binary frame decodes");
        assert_eq!(
            decoded,
            DecodedBinaryGameDataFrame {
                from_player: player_a(),
                encoding: GameDataEncoding::MessagePack,
                payload: payload.to_vec(),
                seq: None,
                epoch: None,
            }
        );

        let frame = BinaryGameDataFrame {
            from_player: player_a(),
            encoding: GameDataEncoding::MessagePack,
            payload,
            seq: None,
            epoch: None,
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

    /// v4 form: the stamp rides as trailing `seq` + `epoch` map keys (production
    /// always pairs them). Frozen so the v4 binary wire cannot drift either.
    #[test]
    fn binary_game_data_encoder_appends_stamp_for_v4_recipients() {
        let payload: &[u8] = &[0x01, 0x02, 0x03, 0x04];
        let wire = encode_binary_game_data(
            player_a(),
            GameDataEncoding::MessagePack,
            payload,
            Some(7),
            Some(3),
        )
        .expect("production binary encode with stamp");

        assert_eq!(
            hex(&wire),
            "85ab66726f6d5f706c61796572c4100000000000000000000000000000000aa8656e636f64696e67ac6d6573736167655f7061636ba77061796c6f6164c40401020304a373657107a565706f636803",
            "v4 binary wire frame drift (BREAKING v4 wire change?)"
        );

        let decoded: DecodedBinaryGameDataFrame =
            rmp_serde::from_slice(&wire).expect("v4 bare binary frame decodes");
        assert_eq!(
            decoded,
            DecodedBinaryGameDataFrame {
                from_player: player_a(),
                encoding: GameDataEncoding::MessagePack,
                payload: payload.to_vec(),
                seq: Some(7),
                epoch: Some(3),
            }
        );

        let frame = BinaryGameDataFrame {
            from_player: player_a(),
            encoding: GameDataEncoding::MessagePack,
            payload,
            seq: Some(7),
            epoch: Some(3),
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
            "v4 binary frame field-name/casing drift (BREAKING v4 wire change?)"
        );
    }

    #[test]
    fn passthrough_binary_encodings_return_payload_unchanged() {
        let payload: &[u8] = br#"{"move":"up"}"#;

        // The passthrough paths have no envelope, so a stamp (gated or not)
        // can never alter the bytes.
        for (seq, epoch) in [(None, None), (Some(9), Some(3))] {
            assert_eq!(
                encode_binary_game_data(player_a(), GameDataEncoding::Json, payload, seq, epoch)
                    .expect("json passthrough"),
                payload.to_vec()
            );
            assert_eq!(
                encode_binary_game_data(player_a(), GameDataEncoding::Rkyv, payload, seq, epoch)
                    .expect("rkyv passthrough"),
                payload.to_vec()
            );
        }
    }

    /// The borrowed pre-v4 shadow must serialize byte-identically to the enum
    /// with `seq: None` — this is what keeps pre-v4 recipients' text frames
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
        };

        let legacy_json = serde_json::to_string(&legacy).expect("legacy json");
        let enum_json = serde_json::to_string(&enum_form).expect("enum json");
        assert_eq!(
            legacy_json, enum_json,
            "pre-v4 shadow must be byte-identical to the unstamped enum form"
        );
        assert_eq!(
            legacy_json,
            format!(
                r#"{{"type":"GameData","data":{{"from_player":"{PLAYER_A_STR}","data":{{"move":"up","n":3}}}}}}"#
            ),
            "pre-v4 GameData text frame drift (BREAKING v2 wire change?)"
        );

        // And the stamped enum form differs ONLY by the trailing seq + epoch
        // keys (production always pairs them for a v4 recipient).
        let stamped = crate::protocol::ServerMessage::GameData {
            from_player: player_a(),
            data,
            seq: Some(42),
            epoch: Some(3),
        };
        let stamped_json = serde_json::to_string(&stamped).expect("stamped json");
        assert_eq!(
            stamped_json,
            format!(
                r#"{{"type":"GameData","data":{{"from_player":"{PLAYER_A_STR}","data":{{"move":"up","n":3}},"seq":42,"epoch":3}}}}"#
            ),
            "v4 GameData text frame drift (BREAKING v4 wire change?)"
        );
    }
}
