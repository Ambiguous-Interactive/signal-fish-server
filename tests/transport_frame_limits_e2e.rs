//! Transport-layer frame-size cap: real-socket end-to-end tests.
//!
//! The application-level `security.max_message_size` check (in the connection
//! receive loop) can only run AFTER the WebSocket library has buffered the
//! entire inbound message in memory. Without a transport-layer cap on the
//! upgrade, the library's defaults (16 MiB frames / 64 MiB messages) let an
//! unauthenticated peer force the server to buffer megabytes per connection
//! before the polite `MessageTooLarge` rejection ever executes — a memory
//! amplification window.
//!
//! Contract pinned here:
//!
//! - a grossly oversized frame (well past the transport cap, derived as
//!   `2 * max_message_size`) is killed at the transport layer: the connection
//!   terminates without the server buffering or politely acknowledging the
//!   frame;
//! - a slightly oversized message (over `max_message_size` but under the
//!   transport cap) keeps today's polite UX: an explicit `MessageTooLarge`
//!   error frame, and the connection remains fully usable.

mod test_helpers;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::protocol::{ClientMessage, ErrorCode, ServerMessage};
use signal_fish_server::server::EnhancedGameServer;
use signal_fish_server::websocket::create_router;
use std::sync::Arc;
use test_helpers::{create_test_server, RunningTestServer};
use tokio_tungstenite::{connect_async, tungstenite::Message};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
type WsReceiver = futures_util::stream::SplitStream<WsStream>;

/// Default `security.max_message_size` (bytes) used by the test server config.
const MAX_MESSAGE_SIZE: usize = 64 * 1024;

/// Well past any sane transport cap (and past the 2x headroom derived from the
/// default `max_message_size`), but under the library's own 16 MiB frame
/// default so that WITHOUT a configured transport cap the frame is accepted,
/// fully buffered, and politely rejected — which is exactly the failure this
/// suite exists to forbid.
const GROSSLY_OVERSIZED_BYTES: usize = 8 * 1024 * 1024;

/// Per-read deadline. Generous for oversubscribed CI runners; every wait in
/// this suite is a ceiling on an event-driven read, never an expected wait.
const READ_DEADLINE: tokio::time::Duration = tokio::time::Duration::from_secs(20);

async fn start_server(server: Arc<EnhancedGameServer>) -> RunningTestServer {
    let router = create_router("http://localhost:3000").with_state(server.clone());
    RunningTestServer::spawn(server, router).await
}

async fn connect(addr: std::net::SocketAddr) -> (WsSink, WsReceiver) {
    let url = format!("ws://{addr}/ws");
    let (stream, _response) =
        tokio::time::timeout(tokio::time::Duration::from_secs(10), connect_async(&url))
            .await
            .expect("websocket connect timed out")
            .expect("websocket connect failed");
    stream.split()
}

/// Prove the connection is still alive and being served: a `Ping` must come
/// back as a `Pong` (skipping unrelated broadcast chatter).
async fn assert_ping_pong_roundtrip(sink: &mut WsSink, receiver: &mut WsReceiver) {
    let ping = serde_json::to_string(&ClientMessage::Ping).expect("serialize Ping");
    sink.send(Message::Text(ping.into()))
        .await
        .expect("send Ping on a connection that must still be usable");

    loop {
        let frame = tokio::time::timeout(READ_DEADLINE, receiver.next())
            .await
            .expect("timed out waiting for Pong")
            .expect("connection closed while waiting for Pong")
            .expect("websocket error while waiting for Pong");
        let Message::Text(text) = frame else {
            continue;
        };
        let message: ServerMessage = serde_json::from_str(&text).expect("valid ServerMessage");
        if matches!(message, ServerMessage::Pong) {
            return;
        }
    }
}

/// A frame well past the transport cap must terminate the connection at the
/// WebSocket layer — before the server buffers the whole payload and long
/// before the application-level size check would politely reject it.
///
/// Termination may surface to the client as a `Close` frame, an error, an
/// end-of-stream, or a failure of the oversized send itself (the server tears
/// the socket down mid-frame): all are acceptable. What is NOT acceptable is
/// today's pre-fix behavior — the server buffers all 8 MiB, replies with a
/// polite `MessageTooLarge` error frame, and keeps serving the connection.
#[tokio::test]
async fn grossly_oversized_frame_is_killed_at_the_transport_layer() {
    let server = create_test_server().await;
    let running_server = start_server(server).await;
    let addr = running_server.addr();
    let (mut sink, mut receiver) = connect(addr).await;

    let oversized = "x".repeat(GROSSLY_OVERSIZED_BYTES);
    let send_result = sink.send(Message::Text(oversized.into())).await;
    if send_result.is_err() {
        // The server tore the connection down while the frame was still being
        // written — transport-layer rejection observed on the send side.
        running_server.shutdown().await;
        return;
    }

    // The send was accepted by the kernel; the server must now terminate the
    // connection rather than politely acknowledge the frame.
    loop {
        let frame = tokio::time::timeout(READ_DEADLINE, receiver.next())
            .await
            .expect("timed out waiting for the server to terminate the connection");
        match frame {
            // Acceptable terminations: close frame, transport error, EOF.
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                running_server.shutdown().await;
                return;
            }
            Some(Ok(Message::Text(text))) => {
                let message: ServerMessage =
                    serde_json::from_str(&text).expect("valid ServerMessage");
                if let ServerMessage::Error {
                    error_code: Some(ErrorCode::MessageTooLarge),
                    ..
                } = message
                {
                    // The polite rejection means the server buffered the whole
                    // grossly oversized frame first. Prove the connection was
                    // left open (the full pre-fix failure mode), then fail.
                    assert_ping_pong_roundtrip(&mut sink, &mut receiver).await;
                    panic!(
                        "an {GROSSLY_OVERSIZED_BYTES}-byte frame was fully buffered and \
                         politely rejected with MessageTooLarge on a still-open connection: \
                         the transport-layer frame cap is missing"
                    );
                }
                // Unrelated chatter (none is expected pre-join, but tolerate it).
            }
            Some(Ok(_)) => {}
        }
    }
}

/// A message just over `max_message_size` (but under the transport cap's 2x
/// headroom) keeps the polite application-level UX: an explicit
/// `MessageTooLarge` error frame, and the connection survives and keeps
/// being served.
#[tokio::test]
async fn slightly_oversized_message_gets_polite_error_and_connection_survives() {
    let server = create_test_server().await;
    let running_server = start_server(server).await;
    let addr = running_server.addr();
    let (mut sink, mut receiver) = connect(addr).await;

    let slightly_oversized = "x".repeat(MAX_MESSAGE_SIZE + 1);
    sink.send(Message::Text(slightly_oversized.into()))
        .await
        .expect("send a message the transport cap must still admit");

    // The polite rejection must arrive as an explicit error frame.
    loop {
        let frame = tokio::time::timeout(READ_DEADLINE, receiver.next())
            .await
            .expect("timed out waiting for the MessageTooLarge error frame")
            .expect("connection closed instead of politely rejecting the message")
            .expect("websocket error instead of a polite MessageTooLarge rejection");
        let Message::Text(text) = frame else {
            continue;
        };
        let message: ServerMessage = serde_json::from_str(&text).expect("valid ServerMessage");
        if let ServerMessage::Error {
            error_code: Some(ErrorCode::MessageTooLarge),
            ..
        } = message
        {
            break;
        }
    }

    // And the connection must remain fully usable afterwards.
    assert_ping_pong_roundtrip(&mut sink, &mut receiver).await;
    running_server.shutdown().await;
}
