//! WebSocket wire layer: JSON text frames carrying the server crate's own
//! `ClientMessage` / `ServerMessage` enums.
//!
//! Envelope types are NEVER hand-rolled here — they come straight from
//! `signal_fish_server::protocol` via the path dependency, so the reference
//! client cannot drift from the server's wire contract. Only the opaque
//! `signal` payload (matchbox `PeerSignal` shape per ADR-0002) is built by the
//! client, because the server deliberately does not model it.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, Stream, StreamExt};
use signal_fish_server::protocol::{ClientMessage, DeliveryClass, ServerMessage};

pub use signal_fish_server::protocol::{decode_v3_binary_game_data, V3BinaryGameDataFrame};

use crate::accountability::validate_class_key;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};

/// The concrete stream type produced by `tokio_tungstenite::connect_async`.
pub type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Time allowed for the initial TCP + WebSocket handshake.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-message ceiling during the sequential handshake phase (authenticate /
/// join). Generous: a CI scheduling budget, not an expected wait.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// Connect to the signaling endpoint (full `ws://.../v3/ws` URL).
pub async fn connect(server_url: &str) -> Result<WsStream> {
    let (stream, _response) = tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async(server_url),
    )
    .await
    .map_err(|_elapsed| anyhow!("websocket connect to {server_url} timed out"))?
    .with_context(|| format!("websocket connect to {server_url} failed"))?;
    Ok(stream)
}

/// Serialize `message` and send it as one WebSocket text frame.
pub async fn send_client_message(ws: &mut WsStream, message: &ClientMessage) -> Result<()> {
    let json = serde_json::to_string(message).context("serialize ClientMessage")?;
    ws.send(Message::Text(json.into()))
        .await
        .context("send ClientMessage frame")?;
    Ok(())
}

/// Send game data with reliable delivery semantics and no coalescing key.
///
/// This preserves the pre-E6 API behavior: omitting `class` selects the
/// protocol's reliable default and keeps the legacy wire shape unchanged.
pub async fn send_game_data(ws: &mut WsStream, data: serde_json::Value) -> Result<()> {
    send_client_message(ws, &game_data_message(data)).await
}

/// Send game data with an explicit protocol-v3 delivery class and key.
pub async fn send_game_data_with_delivery(
    ws: &mut WsStream,
    data: serde_json::Value,
    class: DeliveryClass,
    key: Option<u32>,
) -> Result<()> {
    let message = game_data_message_with_delivery(data, class, key)?;
    send_client_message(ws, &message).await
}

pub(crate) fn game_data_message(data: serde_json::Value) -> ClientMessage {
    ClientMessage::GameData {
        data,
        class: None,
        key: None,
    }
}

fn game_data_message_with_delivery(
    data: serde_json::Value,
    class: DeliveryClass,
    key: Option<u32>,
) -> Result<ClientMessage> {
    validate_class_key(Some(class), key).map_err(anyhow::Error::msg)?;
    Ok(ClientMessage::GameData {
        data,
        class: Some(class),
        key,
    })
}

/// WebSocket Ping/Pong can interleave application frames without affecting
/// their order. Every other non-text frame is application traffic or a
/// terminal outcome and must not be silently skipped.
pub(crate) fn is_transparent_transport_control(frame: &Message) -> bool {
    matches!(frame, Message::Ping(_) | Message::Pong(_))
}

#[derive(Debug)]
pub enum ServerMessageReadError {
    /// A server frame violated the negotiated JSON protocol.
    Protocol(String),
    /// The socket timed out, closed, or reported a transport failure.
    Connection(String),
}

/// Read the next `ServerMessage`, skipping only transparent WebSocket control
/// frames and bounding the wait.
///
/// Failures retain their transport or frame context for the handshake
/// diagnostic emitted by the caller.
pub async fn next_server_message<S>(
    ws: &mut S,
    timeout: Duration,
) -> std::result::Result<ServerMessage, ServerMessageReadError>
where
    S: Stream<Item = std::result::Result<Message, WebSocketError>> + Unpin,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let frame = tokio::time::timeout_at(deadline, ws.next())
            .await
            .map_err(|_elapsed| {
                ServerMessageReadError::Connection(
                    "timed out waiting for a ServerMessage".to_string(),
                )
            })?;
        match frame {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(&text).map_err(|error| {
                    ServerMessageReadError::Protocol(format!(
                        "invalid ServerMessage text frame: {text}: {error}"
                    ))
                });
            }
            Some(Ok(other)) if is_transparent_transport_control(&other) => {
                tracing::debug!(frame = ?other, "skipping non-text frame");
            }
            Some(Ok(Message::Close(frame))) => {
                return Err(ServerMessageReadError::Connection(format!(
                    "websocket closed by server: {frame:?}"
                )));
            }
            Some(Ok(other)) => {
                return Err(ServerMessageReadError::Protocol(format!(
                    "unexpected non-text application frame while waiting for ServerMessage: {other:?}"
                )));
            }
            Some(Err(error)) => {
                return Err(ServerMessageReadError::Connection(format!(
                    "websocket transport error: {error}"
                )));
            }
            None => {
                return Err(ServerMessageReadError::Connection(
                    "websocket closed by server".to_string(),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::Serialize;
    use serde_json::json;
    use signal_fish_server::protocol::GameDataEncoding;
    use uuid::Uuid;

    use super::*;

    const PATTERNED_PLAYER_ID: u128 = 0x0011_2233_4455_6677_8899_aabb_ccdd_eeff;

    #[test]
    fn reliable_game_data_preserves_the_legacy_wire_shape() {
        let encoded = serde_json::to_value(game_data_message(json!({ "x": 1 }))).unwrap();
        assert_eq!(
            encoded,
            json!({ "type": "GameData", "data": { "data": { "x": 1 } } })
        );
    }

    #[test]
    fn explicit_delivery_api_validates_class_and_key() {
        let cases = [
            (DeliveryClass::Reliable, None, true),
            (DeliveryClass::Latest, Some(7), true),
            (DeliveryClass::Volatile, None, true),
            (DeliveryClass::Reliable, Some(7), false),
            (DeliveryClass::Latest, None, false),
            (DeliveryClass::Volatile, Some(7), false),
        ];
        for (class, key, valid) in cases {
            let result = game_data_message_with_delivery(json!(null), class, key);
            assert_eq!(result.is_ok(), valid, "{class:?}/{key:?}");
        }
    }

    #[test]
    fn v3_binary_envelope_decodes_every_opaque_payload_encoding() {
        let from_player = Uuid::from_u128(PATTERNED_PLAYER_ID);
        for encoding in [
            GameDataEncoding::Json,
            GameDataEncoding::MessagePack,
            GameDataEncoding::Rkyv,
        ] {
            let expected = V3BinaryGameDataFrame {
                from_player,
                encoding,
                payload: vec![0, 1, 2, 0xff],
                seq: 9,
                epoch: 3,
            };
            let wire = rmp_serde::to_vec_named(&expected).expect("serialize fixture");
            assert_eq!(
                decode_v3_binary_game_data(&wire).expect("decode v3 envelope"),
                expected
            );
        }
    }

    #[test]
    fn v3_binary_envelope_rejects_noncanonical_message_pack() {
        type Entry = (Vec<u8>, Vec<u8>);

        fn encoded<T: Serialize + ?Sized>(value: &T) -> Vec<u8> {
            rmp_serde::to_vec(value).expect("serialize fixture value")
        }

        fn valid_entries() -> Vec<Entry> {
            vec![
                (
                    encoded("from_player"),
                    encoded(&Uuid::from_u128(PATTERNED_PLAYER_ID)),
                ),
                (encoded("encoding"), encoded(&GameDataEncoding::Json)),
                (
                    encoded("payload"),
                    encoded(&serde_bytes::Bytes::new(b"opaque")),
                ),
                (encoded("seq"), encoded(&9u64)),
                (encoded("epoch"), encoded(&3u32)),
            ]
        }

        fn map(entries: &[Entry]) -> Vec<u8> {
            let mut wire = Vec::new();
            rmp::encode::write_map_len(&mut wire, entries.len() as u32)
                .expect("serialize fixture map length");
            for (key, value) in entries {
                wire.extend_from_slice(key);
                wire.extend_from_slice(value);
            }
            wire
        }

        let canonical = map(&valid_entries());
        assert!(decode_v3_binary_game_data(&canonical).is_ok());

        let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
        cases.push((
            "positional array",
            encoded(&(
                Uuid::from_u128(PATTERNED_PLAYER_ID),
                "json",
                b"opaque",
                9,
                3,
            )),
        ));

        let mut entries = valid_entries();
        entries[0].0 = encoded(&7u8);
        cases.push(("non-string key", map(&entries)));

        let mut entries = valid_entries();
        entries[1].1 = encoded(&7u8);
        cases.push(("numeric encoding", map(&entries)));

        let mut entries = valid_entries();
        entries[0].1 = encoded(&vec![0u8; 16]);
        cases.push(("array UUID", map(&entries)));

        let mut entries = valid_entries();
        entries[0].1 = encoded(&serde_bytes::Bytes::new(&[0u8; 15]));
        cases.push(("short binary UUID", map(&entries)));

        let mut entries = valid_entries();
        entries[2].1 = encoded(&vec![1u8, 2, 3]);
        cases.push(("array payload", map(&entries)));

        for missing in 0..5 {
            let mut entries = valid_entries();
            entries.remove(missing);
            cases.push((
                [
                    "missing from_player",
                    "missing encoding",
                    "missing payload",
                    "missing seq",
                    "missing epoch",
                ][missing],
                map(&entries),
            ));
        }

        let mut entries = valid_entries();
        entries.push(entries[3].clone());
        cases.push(("duplicate key", map(&entries)));

        let mut entries = valid_entries();
        entries[4].0 = encoded("unexpected");
        cases.push(("unknown key", map(&entries)));

        let mut entries = valid_entries();
        entries[3].1 = encoded(&0u8);
        cases.push(("zero seq", map(&entries)));

        let mut entries = valid_entries();
        entries[4].1 = encoded(&0u8);
        cases.push(("zero epoch", map(&entries)));

        let mut entries = valid_entries();
        entries[4].1 = encoded(&(u64::from(u32::MAX) + 1));
        cases.push(("epoch overflow", map(&entries)));

        cases.push(("truncated map", canonical[..canonical.len() - 1].to_vec()));

        let mut trailing_scalar = canonical.clone();
        trailing_scalar.extend(encoded(&1u8));
        cases.push(("trailing scalar", trailing_scalar));

        let mut concatenated_map = canonical.clone();
        concatenated_map.extend(&canonical);
        cases.push(("concatenated map", concatenated_map));

        for (name, wire) in cases {
            assert!(
                decode_v3_binary_game_data(&wire).is_err(),
                "noncanonical {name} envelope was accepted: {wire:?}"
            );
        }
    }
}
