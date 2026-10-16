//! WebSocket wire layer: JSON text frames carrying the server crate's own
//! `ClientMessage` / `ServerMessage` enums.
//!
//! JSON messages and the strict v3 binary decoder come from
//! `signal_fish_server::protocol`. The legacy v2 MessagePack envelope is
//! decoded locally. The opaque `signal` payload uses the matchbox
//! `PeerSignal` shape from ADR-0002.

use std::io::Cursor;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use signal_fish_server::protocol::{
    ClientMessage, DeliveryClass, GameDataEncoding, PlayerId, ServerMessage,
};

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
    send_client_message_until(
        ws,
        message,
        Deadline::after(tokio::time::Instant::now(), WRITE_TIMEOUT),
    )
    .await
}

pub(crate) async fn send_client_message_until<S>(
    ws: &mut S,
    message: &ClientMessage,
    deadline: Deadline,
) -> Result<()>
where
    S: Sink<Message, Error = WebSocketError> + Unpin,
{
    let json = serde_json::to_string(message).context("serialize ClientMessage")?;
    send_frame_until(ws, Message::Text(json.into()), deadline).await
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

/// Send a JSON-compatible value as a raw MessagePack payload.
pub async fn send_message_pack_game_data(ws: &mut WsStream, data: serde_json::Value) -> Result<()> {
    send_game_data_binary(ws, rmp_serde::to_vec_named(&data)?).await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyMessagePackFrame {
    from_player: PlayerId,
    encoding: GameDataEncoding,
    payload: serde_bytes::ByteBuf,
}

fn decode_exact_message_pack<T: serde::de::DeserializeOwned>(wire: &[u8]) -> Result<T> {
    let mut cursor = Cursor::new(wire);
    let value = rmp_serde::from_read(&mut cursor)?;
    if cursor.position() != u64::try_from(wire.len())? {
        return Err(anyhow!("MessagePack value has trailing bytes"));
    }
    Ok(value)
}

/// Decode a MessagePack relay into the normal application event path.
pub(crate) fn decode_message_pack_game_data(wire: &[u8], version: u16) -> Result<ServerMessage> {
    let (from_player, encoding, payload, seq, epoch) = if version >= 3 {
        let frame = decode_v3_binary_game_data(wire).map_err(anyhow::Error::msg)?;
        (
            frame.from_player,
            frame.encoding,
            frame.payload,
            Some(frame.seq),
            Some(frame.epoch),
        )
    } else {
        if rmp::decode::read_map_len(&mut &*wire)? != 3 {
            return Err(anyhow!("v2 MessagePack relay requires three fields"));
        }
        let frame: LegacyMessagePackFrame = decode_exact_message_pack(wire)?;
        (
            frame.from_player,
            frame.encoding,
            frame.payload.into_vec(),
            None,
            None,
        )
    };
    if encoding != GameDataEncoding::MessagePack {
        return Err(anyhow!("expected MessagePack payload encoding"));
    }
    Ok(ServerMessage::GameData {
        from_player,
        data: decode_exact_message_pack(&payload)?,
        seq,
        epoch,
        class: None,
        key: None,
    })
}

const OPAQUE_RELAY_PROBE_MAGIC: &[u8; 4] = b"SFRP";
const OPAQUE_RELAY_PROBE_HEADER_LENGTH: usize = 21;

/// Frame a reference relay probe with application-level sender attribution.
///
/// Layout: `SFRP`, version byte 1, sender UUID in 16 network-order bytes,
/// then the exact application payload. This header is inside the server's
/// opaque payload on both protocol versions and counts toward its size limit.
pub fn encode_opaque_relay_probe(from: PlayerId, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::new();
    frame.extend_from_slice(OPAQUE_RELAY_PROBE_MAGIC);
    frame.push(1);
    frame.extend_from_slice(from.as_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// Decode reference probe attribution without inventing v2 delivery stamps.
///
/// A v3 server sender permits old untagged payloads. A tagged probe must agree
/// with that sender. V2 requires the application header because the server
/// relays only raw bytes; its sender ID is application metadata.
pub fn decode_opaque_relay_probe(
    bytes: &[u8],
    server_sender: Option<PlayerId>,
) -> Result<(PlayerId, &[u8])> {
    if !bytes.starts_with(OPAQUE_RELAY_PROBE_MAGIC) {
        return server_sender
            .map(|from| (from, bytes))
            .ok_or_else(|| anyhow!("v2 opaque relay requires a reference probe header"));
    }
    if bytes.get(4) != Some(&1) || bytes.len() < OPAQUE_RELAY_PROBE_HEADER_LENGTH {
        return Err(anyhow!("invalid opaque reference probe header"));
    }
    let sender_bytes = bytes
        .get(5..OPAQUE_RELAY_PROBE_HEADER_LENGTH)
        .ok_or_else(|| anyhow!("invalid opaque reference probe header"))?;
    let from = PlayerId::from_slice(sender_bytes)?;
    if server_sender.is_some_and(|sender| sender != from) {
        return Err(anyhow!(
            "opaque reference probe sender differs from server sender"
        ));
    }
    let payload = bytes
        .get(OPAQUE_RELAY_PROBE_HEADER_LENGTH..)
        .ok_or_else(|| anyhow!("invalid opaque reference probe header"))?;
    Ok((from, payload))
}

/// Send an opaque game-data payload as one raw binary WebSocket frame.
///
/// Issue #627: with `rkyv`/`protobuf` negotiated, the payload bytes are the
/// game's business and relay untouched — the frame carries no JSON envelope.
/// The write and flush share [`WRITE_TIMEOUT`]. Discard the connection on failure.
pub async fn send_game_data_binary(ws: &mut WsStream, payload: Vec<u8>) -> Result<()> {
    send_frame(ws, Message::Binary(payload.into())).await
}

async fn send_frame<S>(ws: &mut S, frame: Message) -> Result<()>
where
    S: Sink<Message, Error = WebSocketError> + Unpin,
{
    send_frame_until(
        ws,
        frame,
        Deadline::after(tokio::time::Instant::now(), WRITE_TIMEOUT),
    )
    .await
}

async fn send_frame_until<S>(ws: &mut S, frame: Message, deadline: Deadline) -> Result<()>
where
    S: Sink<Message, Error = WebSocketError> + Unpin,
{
    let now = tokio::time::Instant::now();
    let budget = deadline.finite().map_or(WRITE_TIMEOUT, |cutoff| {
        cutoff.saturating_duration_since(now).min(WRITE_TIMEOUT)
    });
    let deadline = Deadline::after(now, budget);
    let timed_out = || anyhow!("WebSocket write timed out; discard this connection");
    if deadline.is_due(tokio::time::Instant::now()) {
        return Err(timed_out());
    }
    let result = deadline.timeout(ws.send(frame)).await;
    if deadline.is_due(tokio::time::Instant::now()) {
        return Err(timed_out());
    }
    result
        .map_err(|_| timed_out())?
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
    next_server_message_until(ws, Deadline::after(tokio::time::Instant::now(), timeout)).await
}

pub(crate) async fn next_server_message_until<S>(
    ws: &mut S,
    deadline: Deadline,
) -> std::result::Result<ServerMessage, ServerMessageReadError>
where
    S: Stream<Item = std::result::Result<Message, WebSocketError>> + Unpin,
{
    let timed_out =
        || ServerMessageReadError::Connection("timed out waiting for a ServerMessage".to_string());
    loop {
        if deadline.is_due(tokio::time::Instant::now()) {
            return Err(timed_out());
        }
        let frame = deadline.timeout(ws.next()).await.map_err(|_| timed_out())?;
        if deadline.is_due(tokio::time::Instant::now()) {
            return Err(timed_out());
        }
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
                if let Err(error) = &result {
                    assert!(error.to_string().contains("timed out"));
                } else {
                    accepted_late.push(elapsed);
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
            let receiving_socket = tokio::net::TcpSocket::new_v4().unwrap();
            receiving_socket.set_recv_buffer_size(1024).unwrap();
            receiving_socket
                .bind("127.0.0.1:0".parse().unwrap())
                .unwrap();
            let listener = receiving_socket.listen(1).unwrap();
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
            let observation = tokio::time::timeout(Duration::from_millis(50), &mut write).await;
            assert!(
                observation.is_err(),
                "binary={binary}: establish actual socket backpressure before advancing time: {observation:?}"
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

    // Regression #985: a ready sink completion cannot cancel an expired timer.
    #[tokio::test(start_paused = true)]
    async fn websocket_writes_reject_ready_flushes_at_the_deadline() {
        let mut accepted_late = Vec::new();
        for binary in [false, true] {
            for elapsed in [
                WRITE_TIMEOUT - Duration::from_millis(1),
                WRITE_TIMEOUT,
                WRITE_TIMEOUT + Duration::from_millis(1),
            ] {
                let (release, released) = tokio::sync::oneshot::channel::<()>();
                let frame = if binary {
                    Message::Binary(vec![0, 1, 255].into())
                } else {
                    Message::Text("test".into())
                };
                let mut frames = Vec::new();
                let mut sink = Box::pin(futures_util::sink::unfold(
                    Some(released),
                    |mut released, frame| {
                        frames.push(frame);
                        async move {
                            released.take().unwrap().await.unwrap();
                            Ok::<_, WebSocketError>(released)
                        }
                    },
                ));
                let result = {
                    let write = send_frame(&mut sink, frame.clone());
                    tokio::pin!(write);
                    assert!(futures_util::poll!(write.as_mut()).is_pending());
                    tokio::time::advance(elapsed).await;
                    release.send(()).unwrap();
                    write.await
                };
                drop(sink);
                assert_eq!(frames, [frame], "one original frame only");
                if elapsed >= WRITE_TIMEOUT {
                    if let Err(error) = result {
                        assert!(error.to_string().contains("write timed out"));
                    } else {
                        accepted_late.push((binary, elapsed));
                    }
                } else {
                    assert!(result.is_ok(), "on-time flush: {result:?}");
                }
            }
        }
        assert!(
            accepted_late.is_empty(),
            "late flushes reported success: {accepted_late:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn websocket_writes_bound_pending_flushes_and_preserve_sink_errors() {
        for (budget, expected) in [
            (Duration::from_secs(2), Duration::from_secs(2)),
            (Duration::from_secs(30), WRITE_TIMEOUT),
            (Duration::from_secs(u64::MAX), WRITE_TIMEOUT),
        ] {
            let mut sink = Box::pin(futures_util::sink::unfold((), |(), _frame: Message| {
                std::future::pending::<Result<(), WebSocketError>>()
            }));
            let started = tokio::time::Instant::now();
            let error = send_client_message_until(
                &mut sink,
                &ClientMessage::Ping,
                Deadline::after(started, budget),
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains("write timed out"), "{error:#}");
            let elapsed = started.elapsed();
            assert!(
                elapsed >= expected && elapsed <= expected + Duration::from_millis(1),
                "budget={budget:?}: {elapsed:?}"
            );
        }

        let mut sink = Box::pin(futures_util::sink::unfold((), |(), _frame: Message| {
            std::future::ready(Err::<(), _>(WebSocketError::ConnectionClosed))
        }));
        let error = send_frame(&mut sink, Message::Text("test".into()))
            .await
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<WebSocketError>(),
            Some(WebSocketError::ConnectionClosed)
        ));
        assert!(error.to_string().contains("send WebSocket frame"));
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

    // Regression #983: ready frames cannot bypass the receive cutoff.
    #[tokio::test(start_paused = true)]
    async fn server_message_wait_rejects_ready_frames_at_expiry() {
        use std::task::Poll;

        let budget = Duration::from_secs(1);
        for elapsed in [
            budget - Duration::from_millis(1),
            budget,
            budget + Duration::from_millis(1),
        ] {
            let mut available = false;
            let mut input = futures_util::stream::poll_fn(|_| {
                if available {
                    Poll::Ready(Some(Ok(Message::Text(r#"{"type":"Pong"}"#.into()))))
                } else {
                    available = true;
                    Poll::Pending
                }
            });
            let read = next_server_message(&mut input, budget);
            tokio::pin!(read);
            assert!(futures_util::poll!(read.as_mut()).is_pending());
            tokio::time::advance(elapsed).await;
            let result = read.await;
            if elapsed < budget {
                assert!(matches!(result, Ok(ServerMessage::Pong)), "{result:?}");
            } else {
                assert!(
                    matches!(result, Err(ServerMessageReadError::Connection(ref error)) if error.contains("timed out")),
                    "accepted a ready frame after {elapsed:?}: {result:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn server_message_wait_checks_time_between_buffered_controls() {
        use std::task::Poll;

        /* A finite ready stream crosses the cutoff in one poll and still
        terminates on the baseline implementation. */
        for pong in [false, true] {
            let mut polls = 0;
            let mut input = futures_util::stream::poll_fn(|_| {
                polls += 1;
                if polls <= 8 {
                    std::thread::sleep(Duration::from_millis(5));
                    let frame = if pong {
                        Message::Pong(vec![].into())
                    } else {
                        Message::Ping(vec![].into())
                    };
                    Poll::Ready(Some(Ok(frame)))
                } else {
                    Poll::Ready(Some(Ok(Message::Text(r#"{"type":"Pong"}"#.into()))))
                }
            });
            let result = next_server_message(&mut input, Duration::from_millis(10)).await;
            assert!(
                matches!(result, Err(ServerMessageReadError::Connection(ref error)) if error.contains("timed out")),
                "buffered controls postponed the cutoff: pong={pong}, result={result:?}"
            );
            assert!(polls < 8, "stop reading controls once time is due");
        }
    }

    #[tokio::test]
    async fn server_message_wait_rejects_a_read_that_finishes_after_expiry() {
        let mut input = futures_util::stream::poll_fn(|_| {
            std::thread::sleep(Duration::from_millis(10));
            std::task::Poll::Ready(Some(Ok(Message::Text(r#"{"type":"Pong"}"#.into()))))
        });
        let result = next_server_message(&mut input, Duration::from_millis(5)).await;
        assert!(
            matches!(result, Err(ServerMessageReadError::Connection(ref error)) if error.contains("timed out")),
            "{result:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn server_message_wait_preserves_controls_errors_and_zero_budget() {
        let mut polls = 0;
        let mut input = futures_util::stream::poll_fn(|_| {
            polls += 1;
            std::task::Poll::Ready(Some(Ok(Message::Text(r#"{"type":"Pong"}"#.into()))))
        });
        assert!(matches!(
            next_server_message(&mut input, Duration::ZERO).await,
            Err(ServerMessageReadError::Connection(_))
        ));
        assert_eq!(polls, 0, "an expired wait must not consume input");

        for budget in [Duration::from_secs(1), Duration::from_secs(u64::MAX)] {
            let mut controls = futures_util::stream::iter([
                Ok(Message::Ping(vec![1].into())),
                Ok(Message::Pong(vec![2].into())),
                Ok(Message::Text(r#"{"type":"Pong"}"#.into())),
            ]);
            assert!(matches!(
                next_server_message(&mut controls, budget).await,
                Ok(ServerMessage::Pong)
            ));
            for (frame, protocol_error) in [
                (Some(Ok(Message::Text("{".into()))), true),
                (Some(Ok(Message::Binary(vec![1].into()))), true),
                (Some(Ok(Message::Close(None))), false),
                (Some(Err(WebSocketError::ConnectionClosed)), false),
                (None, false),
            ] {
                let mut input = futures_util::stream::iter(frame);
                let result = next_server_message(&mut input, budget).await;
                if protocol_error {
                    assert!(
                        matches!(result, Err(ServerMessageReadError::Protocol(_))),
                        "{result:?}"
                    );
                } else {
                    assert!(
                        matches!(result, Err(ServerMessageReadError::Connection(_))),
                        "{result:?}"
                    );
                }
            }
        }
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
    fn opaque_reference_probe_shared_vectors_and_version_boundaries() {
        let sender = PlayerId::parse_str("00112233-4455-6677-8899-aabbccddeeff").unwrap();
        for payload in [
            vec![],
            vec![0, 255, 1],
            "雪\nSFRP\0".as_bytes().to_vec(),
            vec![42; 65536],
        ] {
            let mut expected = vec![
                83, 70, 82, 80, 1, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99,
                0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
            ];
            expected.extend_from_slice(&payload);
            let bytes = encode_opaque_relay_probe(sender, &payload);
            assert_eq!(bytes, expected);
            for server_sender in [None, Some(sender)] {
                assert_eq!(
                    decode_opaque_relay_probe(&bytes, server_sender).unwrap(),
                    (sender, payload.as_slice())
                );
            }
        }
        let valid = encode_opaque_relay_probe(sender, &[7]);
        for length in 0..21 {
            assert!(
                decode_opaque_relay_probe(&valid[..length], None).is_err(),
                "length={length}"
            );
        }
        for version in [0, 2, 255] {
            let mut invalid = valid.clone();
            invalid[4] = version;
            assert!(decode_opaque_relay_probe(&invalid, Some(sender)).is_err());
        }
        assert!(decode_opaque_relay_probe(&valid, Some(PlayerId::new_v4())).is_err());
        let legacy = [0, 255];
        assert_eq!(
            decode_opaque_relay_probe(&legacy, Some(sender)).unwrap(),
            (sender, legacy.as_slice())
        );
        assert!(decode_opaque_relay_probe(&legacy, None).is_err());
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
    #[test]
    fn message_pack_relay_decodes_both_versions_and_rejects_wrong_shapes() {
        use signal_fish_server::protocol::{GameDataEncoding, PlayerId, ServerMessage};
        let sender = PlayerId::from_u128(1);
        let payload =
            serde_json::json!({ "relay_msg": "hello 世界", "nested": [null, true, 1.25, -17] });
        for version in [2, 3] {
            #[derive(serde::Serialize)]
            struct Frame {
                from_player: PlayerId,
                encoding: GameDataEncoding,
                #[serde(with = "serde_bytes")]
                payload: Vec<u8>,
                #[serde(skip_serializing_if = "Option::is_none")]
                seq: Option<u64>,
                #[serde(skip_serializing_if = "Option::is_none")]
                epoch: Option<u32>,
            }
            let mut frame = Frame {
                from_player: sender,
                encoding: GameDataEncoding::MessagePack,
                payload: rmp_serde::to_vec_named(&payload).unwrap(),
                seq: (version == 3).then_some(7),
                epoch: (version == 3).then_some(3),
            };
            let bytes = rmp_serde::to_vec_named(&frame).unwrap();
            let decoded = super::decode_message_pack_game_data(&bytes, version).unwrap();
            let ServerMessage::GameData {
                from_player,
                data,
                seq,
                epoch,
                class,
                key,
            } = decoded
            else {
                panic!("expected GameData")
            };
            assert_eq!(from_player, sender);
            assert_eq!(data, payload);
            assert_eq!(seq, frame.seq);
            assert_eq!(epoch, frame.epoch);
            assert!(class.is_none() && key.is_none());
            assert!(
                super::decode_message_pack_game_data(&bytes, if version == 2 { 3 } else { 2 })
                    .is_err()
            );
            let mut trailing = bytes;
            trailing.push(0);
            assert!(super::decode_message_pack_game_data(&trailing, version).is_err());
            frame.encoding = GameDataEncoding::Protobuf;
            assert!(super::decode_message_pack_game_data(
                &rmp_serde::to_vec_named(&frame).unwrap(),
                version
            )
            .is_err());
            frame.encoding = GameDataEncoding::MessagePack;
            frame.payload.push(0);
            assert!(super::decode_message_pack_game_data(
                &rmp_serde::to_vec_named(&frame).unwrap(),
                version
            )
            .is_err());
        }
    }
}
