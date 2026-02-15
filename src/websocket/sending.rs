use crate::protocol::{GameDataEncoding, PlayerId, ServerMessage};
use crate::server::EnhancedGameServer;
use axum::extract::ws::{Message, WebSocket};
use futures_util::SinkExt;
use rmp_serde::{from_slice, to_vec_named};
use serde::Serialize;
use std::sync::Arc;

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
                        if let Err(fallback_err) = send_binary_fallback(
                            sender,
                            *from_player,
                            *encoding,
                            payload,
                            player_id,
                        )
                        .await
                        {
                            tracing::warn!(
                                %player_id,
                                %from_player,
                                encoding = ?encoding,
                                error = %fallback_err,
                                "Dropping binary game data message after fallback failure"
                            );
                        }
                    }
                }
            } else if let Err(err) =
                send_binary_fallback(sender, *from_player, *encoding, payload, player_id).await
            {
                tracing::warn!(
                    %player_id,
                    %from_player,
                    encoding = ?encoding,
                    error = %err,
                    "Client does not support binary payloads; message dropped"
                );
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
) -> Result<(), String> {
    let data = decode_binary_to_json(encoding, payload)?;
    let fallback = ServerMessage::GameData { from_player, data };
    send_text_message(sender, &fallback, player_id)
        .await
        .map_err(|()| "failed to write JSON fallback frame".to_string())
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

#[derive(Serialize)]
struct BinaryGameDataFrame<'a> {
    from_player: PlayerId,
    encoding: GameDataEncoding,
    #[serde(with = "serde_bytes")]
    payload: &'a [u8],
}

fn encode_binary_game_data(
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
