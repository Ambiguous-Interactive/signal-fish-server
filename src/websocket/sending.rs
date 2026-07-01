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
        } => {
            if server.prefers_encoding(player_id, *encoding) {
                match encode_binary_game_data(*from_player, *encoding, payload) {
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
                let fallback =
                    send_binary_fallback(sender, *from_player, *encoding, payload, player_id).await;
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
    player_id: &PlayerId,
) -> Result<(), BinaryFallbackError> {
    let data =
        decode_binary_to_json(encoding, payload).map_err(BinaryFallbackError::Undeliverable)?;
    let fallback = ServerMessage::GameData { from_player, data };
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
            server.metrics().increment_websocket_messages_dropped();
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
}

/// Encodes a binary game-data frame exactly as production puts it on the wire.
///
/// This is the single source of truth for the binary send path. Keep this
/// private to the websocket module so the wire frame layout can be tested
/// without becoming public library API.
pub(super) fn encode_binary_game_data(
    from_player: PlayerId,
    encoding: GameDataEncoding,
    payload: &[u8],
) -> Result<Vec<u8>, String> {
    match encoding {
        GameDataEncoding::MessagePack => {
            let frame = BinaryGameDataFrame {
                from_player,
                encoding,
                payload,
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
    }

    #[test]
    fn binary_game_data_encoder_emits_bare_message_pack_frame() {
        let payload: &[u8] = &[0x01, 0x02, 0x03, 0x04];
        let wire = encode_binary_game_data(player_a(), GameDataEncoding::MessagePack, payload)
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
            }
        );

        let frame = BinaryGameDataFrame {
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

    #[test]
    fn passthrough_binary_encodings_return_payload_unchanged() {
        let payload: &[u8] = br#"{"move":"up"}"#;

        assert_eq!(
            encode_binary_game_data(player_a(), GameDataEncoding::Json, payload)
                .expect("json passthrough"),
            payload.to_vec()
        );
        assert_eq!(
            encode_binary_game_data(player_a(), GameDataEncoding::Rkyv, payload)
                .expect("rkyv passthrough"),
            payload.to_vec()
        );
    }
}
