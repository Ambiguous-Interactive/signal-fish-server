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
use crate::deadline::Deadline;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};

/// The concrete stream type produced by `tokio_tungstenite::connect_async`.
pub type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Time allowed for the initial TCP + WebSocket handshake.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Whole-command ceiling during the sequential handshake phase.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// Maximum time to send and flush one complete WebSocket frame.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(20);

/// Connect to the signaling endpoint (full `ws://.../v3/ws` URL).
/// The upgrade must complete before the original [`CONNECT_TIMEOUT`] deadline.
pub async fn connect(server_url: &str) -> Result<WsStream> {
    let deadline = Deadline::after(tokio::time::Instant::now(), CONNECT_TIMEOUT);
    let connection = deadline
        .timeout(tokio_tungstenite::connect_async(server_url))
        .await;
    if deadline.is_due(tokio::time::Instant::now()) {
        return Err(anyhow!("websocket connect to {server_url} timed out"));
    }
    let (stream, _response) = connection
        .map_err(|_elapsed| anyhow!("websocket connect to {server_url} timed out"))?
        .with_context(|| format!("websocket connect to {server_url} failed"))?;
    Ok(stream)
}

/// Serialize `message` and send it as one WebSocket text frame.
///
/// The write and flush share [`WRITE_TIMEOUT`]. Discard the connection on
/// failure: cancellation can leave a partial frame in the transport.
pub async fn send_client_message(ws: &mut WsStream, message: &ClientMessage) -> Result<()> {
    let json = serde_json::to_string(message).context("serialize ClientMessage")?;
    send_frame(ws, Message::Text(json.into())).await
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

/// Send an opaque game-data payload as one raw binary WebSocket frame.
///
/// Issue #627: with `rkyv`/`protobuf` negotiated, the payload bytes are the
/// game's business and relay untouched — the frame carries no JSON envelope.
/// The write and flush share [`WRITE_TIMEOUT`]. Discard the connection on failure.
/// Requires protocol v3: the strict v3 reply envelope is the only attributed
/// delivery shape (v2 passthrough has no sender attribution).
pub async fn send_game_data_binary(ws: &mut WsStream, payload: Vec<u8>) -> Result<()> {
    send_frame(ws, Message::Binary(payload.into())).await
}

async fn send_frame(ws: &mut WsStream, frame: Message) -> Result<()> {
    tokio::time::timeout(WRITE_TIMEOUT, ws.send(frame))
        .await
        .context("WebSocket write timed out; discard this connection")?
        .context("send WebSocket frame")
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

/// Decode text without copying secrets or attacker-controlled values into
/// diagnostics. Serde's displayed errors can contain the rejected value.
pub(crate) fn decode_server_text(text: &str) -> std::result::Result<ServerMessage, String> {
    serde_json::from_str(text).map_err(|error| {
        format!(
            "invalid ServerMessage text frame ({} bytes, {:?} error at line {}, column {})",
            text.len(),
            error.classify(),
            error.line(),
            error.column()
        )
    })
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
    let deadline = Deadline::after(tokio::time::Instant::now(), timeout);
    loop {
        let frame = deadline.timeout(ws.next()).await.map_err(|_elapsed| {
            ServerMessageReadError::Connection("timed out waiting for a ServerMessage".to_string())
        })?;
        match frame {
            Some(Ok(Message::Text(text))) => {
                return decode_server_text(&text).map_err(ServerMessageReadError::Protocol);
            }
            Some(Ok(other)) if is_transparent_transport_control(&other) => {
                tracing::debug!("skipping WebSocket control frame");
            }
            Some(Ok(Message::Close(frame))) => {
                return Err(ServerMessageReadError::Connection(format!(
                    "websocket closed by server: {frame:?}"
                )));
            }
            Some(Ok(Message::Binary(bytes))) => {
                return Err(ServerMessageReadError::Protocol(format!(
                    "unexpected {}-byte binary frame while waiting for ServerMessage",
                    bytes.len()
                )));
            }
            Some(Ok(_)) => {
                return Err(ServerMessageReadError::Protocol(
                    "unexpected non-text application frame while waiting for ServerMessage"
                        .to_string(),
                ));
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

    // Regression #981: a ready upgrade cannot bypass an expired timeout.
    #[tokio::test(start_paused = true)]
    async fn websocket_connect_rejects_a_ready_upgrade_at_the_deadline() {
        let clock_guard = tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        });
        let mut accepted_late = Vec::new();
        for elapsed in [
            CONNECT_TIMEOUT - Duration::from_millis(1),
            CONNECT_TIMEOUT,
            CONNECT_TIMEOUT + Duration::from_millis(1),
            CONNECT_TIMEOUT - Duration::from_millis(1),
        ] {
            let expired = elapsed >= CONNECT_TIMEOUT;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (ready_tx, mut ready_rx) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                ready_tx.send(()).unwrap();
                ws.next().await
            });
            let url = format!("ws://{address}");
            let connecting = connect(&url);
            tokio::pin!(connecting);
            let started = tokio::time::Instant::now();
            tokio::select! {
                biased;
                result = &mut ready_rx => result.unwrap(),
                result = &mut connecting => panic!("client settled before the server published its response: {result:?}"),
            }
            for _ in 0..128 {
                tokio::task::yield_now().await;
            }
            assert_eq!(
                tokio::time::Instant::now(),
                started,
                "socket I/O must not advance the clock"
            );
            tokio::time::advance(elapsed).await;
            let result = connecting.await;
            if expired {
                if result.is_ok() {
                    accepted_late.push(elapsed);
                } else {
                    assert!(result
                        .as_ref()
                        .unwrap_err()
                        .to_string()
                        .contains("timed out"));
                }
            } else {
                assert!(result.is_ok(), "accept an on-time upgrade: {result:?}");
            }
            drop(result);
            let terminal = tokio::time::timeout(Duration::from_secs(1), server)
                .await
                .unwrap()
                .unwrap();
            assert!(
                !matches!(terminal, Some(Ok(_))),
                "a retired connection sends no application frame: {terminal:?}"
            );
        }
        clock_guard.abort();
        assert!(
            accepted_late.is_empty(),
            "accepted an upgrade at expiry: {accepted_late:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn websocket_connect_times_out_without_an_upgrade_response() {
        use tokio::io::AsyncReadExt;

        let clock_guard = tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (ready_tx, mut ready_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                request.push(tcp.read_u8().await.unwrap());
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            ready_tx.send(()).unwrap();
            tcp.read(&mut [0_u8; 1]).await.unwrap()
        });
        let url = format!("ws://{address}");
        let connecting = connect(&url);
        tokio::pin!(connecting);
        tokio::select! {
            biased;
            result = &mut ready_rx => result.unwrap(),
            result = &mut connecting => panic!("connection settled without an upgrade: {result:?}"),
        }
        tokio::time::advance(CONNECT_TIMEOUT + Duration::from_millis(1)).await;
        let error = connecting.await.unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error:#}");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), server)
                .await
                .unwrap()
                .unwrap(),
            0,
            "timeout releases the TCP connection"
        );
        clock_guard.abort();
    }

    #[tokio::test]
    async fn websocket_connect_preserves_upgrade_refusal() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                request.push(tcp.read_u8().await.unwrap());
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            tcp.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        let error = connect(&format!("ws://{address}")).await.unwrap_err();
        assert!(error.to_string().contains("failed"), "{error:#}");
        assert!(format!("{error:#}").contains("503"), "{error:#}");
        server.await.unwrap();
    }

    // Regression #976: a nonreading endpoint must not hold the client forever.
    #[tokio::test]
    async fn websocket_writes_expire_with_a_nonreading_endpoint() {
        use futures_util::FutureExt;
        use tokio_tungstenite::MaybeTlsStream;

        let mut overdue = Vec::new();
        for binary in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                tokio_tungstenite::accept_async(tcp).await.unwrap()
            });
            let socket = tokio::net::TcpSocket::new_v4().unwrap();
            socket.set_send_buffer_size(1024).unwrap();
            let tcp = socket.connect(address).await.unwrap();
            let (mut ws, _) = tokio_tungstenite::client_async(
                format!("ws://{address}"),
                MaybeTlsStream::Plain(tcp),
            )
            .await
            .unwrap();
            let _nonreading_endpoint = server.await.unwrap();
            let payload = "x".repeat(8 * 1024 * 1024);
            let mut write = Box::pin(async {
                if binary {
                    send_game_data_binary(&mut ws, payload.into_bytes()).await
                } else {
                    send_client_message(&mut ws, &game_data_message(serde_json::json!(payload)))
                        .await
                }
            });
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut write)
                    .await
                    .is_err(),
                "binary={binary}: establish actual socket backpressure before advancing time"
            );
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(20)).await;
            let result = write.as_mut().now_or_never();
            tokio::time::resume();
            if let Some(result) = result {
                let error = result.unwrap_err();
                assert!(error.to_string().contains("write timed out"), "{error:#}");
            } else {
                overdue.push(binary);
            }
        }
        assert!(
            overdue.is_empty(),
            "blocked writes outlive their whole-write budget (binary): {overdue:?}"
        );
    }

    #[tokio::test]
    async fn websocket_writes_preserve_frames_with_a_reading_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let mut frames = Vec::new();
            for _ in 0..4 {
                frames.push(ws.next().await.unwrap().unwrap());
            }
            frames
        });
        let mut ws = connect(&format!("ws://{address}")).await.unwrap();
        send_client_message(&mut ws, &ClientMessage::Ping)
            .await
            .unwrap();
        send_game_data(&mut ws, json!({"move": 7})).await.unwrap();
        send_game_data_with_delivery(&mut ws, json!({"move": 8}), DeliveryClass::Latest, Some(2))
            .await
            .unwrap();
        send_game_data_binary(&mut ws, vec![0, 1, 255])
            .await
            .unwrap();
        let frames = server.await.unwrap();
        for (frame, expected) in frames[..3].iter().zip([
            json!({"type": "Ping"}),
            json!({"type": "GameData", "data": {"data": {"move": 7}}}),
            json!({"type": "GameData", "data": {"data": {"move": 8}, "class": "latest", "key": 2}}),
        ]) {
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(frame.to_text().unwrap()).unwrap(),
                expected
            );
        }
        assert_eq!(frames[3], Message::Binary(vec![0, 1, 255].into()));
    }

    #[tokio::test]
    async fn unrepresentable_message_wait_does_not_expire_immediately() {
        let mut stream =
            futures_util::stream::pending::<std::result::Result<Message, WebSocketError>>();

        let outcome = tokio::time::timeout(
            Duration::from_millis(20),
            next_server_message(&mut stream, Duration::from_secs(u64::MAX)),
        )
        .await;

        assert!(
            outcome.is_err(),
            "the finite outer test deadline must expire before the distant message wait"
        );
    }

    // Regression #819: malformed snapshots must not copy bearer tokens to logs.
    #[tokio::test]
    async fn malformed_room_snapshots_never_expose_tokens_in_diagnostics() {
        for kind in ["RoomJoined", "Reconnected"] {
            // A data error can echo an invalid enum value too. Neither the
            // raw frame nor serde's error text is safe to emit.
            let text = json!({"type": kind, "data": {
                "room_id": Uuid::nil(), "room_code": "ABC123", "player_id": Uuid::nil(),
                "game_name": "restore", "max_players": 2, "supports_authority": false,
                "current_players": [], "is_authority": false,
                "lobby_state": "private-token-in-invalid-field", "ready_players": [],
                "relay_type": "none", "missed_events": [],
                "reconnection_token": "private-room-token"
            }})
            .to_string();
            let mut stream = futures_util::stream::iter([Ok(Message::Text(text.clone().into()))]);
            let ServerMessageReadError::Protocol(error) =
                next_server_message(&mut stream, Duration::from_secs(1))
                    .await
                    .unwrap_err()
            else {
                panic!("malformed snapshot must fail as a protocol error")
            };
            assert!(
                !error.contains("private-room-token"),
                "{kind} leaked room token: {error}"
            );
            assert!(
                !error.contains("private-token-in-invalid-field"),
                "{kind} echoed invalid value: {error}"
            );
            assert!(error.contains(&format!("{} bytes", text.len())), "{error}");
            assert!(error.contains("Data error at line 1, column"), "{error}");
        }
    }

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
            GameDataEncoding::Protobuf,
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
