#![allow(dead_code)]

pub mod chaos_proxy;
pub mod conformance;
pub mod delivery_ledger;
pub mod prometheus_scrape;
pub mod room16;
pub mod server_process;

use std::collections::VecDeque;
use std::time::Duration;

use futures_util::{Stream, StreamExt};
use signal_fish_server::metrics::ServerMetrics;
use signal_fish_server::protocol::ServerMessage;
use tokio::time::{timeout_at, Instant};
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};

pub type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const SKIPPED_MESSAGE_LIMIT: usize = 8;

/// Assert the server's delivery-conservation law over its metrics.
///
/// Every attempt routed through the reliable server delivery paths resolves as
/// exactly one of: enqueued on the recipient's outbound queue, unroutable
/// because the recipient's channel already closed, canceled before enqueue
/// because a conditional commit condition no longer held, or abandoned as a
/// slow-consumer drop. `websocket_messages_dropped` additionally counts
/// messages abandoned with a closing connection *after* they were enqueued
/// (a slow consumer's undrained queue, a failed close-time flush), so the law
/// over the exported counters is two-sided rather than an exact equality:
///
/// `enqueued + channel_closed + canceled <= attempts <= enqueued + channel_closed + canceled + dropped`
///
/// Self-stabilizing: an in-flight delivery has its attempt counted before its
/// outcome exists, so a snapshot can transiently read `attempts` one (or a
/// few) ahead of the resolved counters even in a healthy server — e.g. a
/// background broadcast the test did not explicitly await. Rather than
/// requiring every caller to reason about total quiescence (the fragile
/// contract that made this fail on loaded CI runners), the assertion polls
/// until the law balances, bounded by a generous deadline. A GENUINE
/// violation — a delivery whose outcome was never recorded, the #131 bug
/// shape — never balances, so the deadline converts it into the same loud
/// failure with the final counter values.
pub async fn assert_message_conservation(metrics: &ServerMetrics) {
    use std::sync::atomic::Ordering;

    const QUIESCENCE_DEADLINE: Duration = Duration::from_secs(10);
    const POLL_INTERVAL: Duration = Duration::from_millis(25);

    let deadline = Instant::now() + QUIESCENCE_DEADLINE;
    let (mut attempts, mut enqueued, mut channel_closed, mut canceled, mut dropped);
    loop {
        attempts = metrics.websocket_delivery_attempts.load(Ordering::Relaxed);
        enqueued = metrics
            .websocket_deliveries_enqueued
            .load(Ordering::Relaxed);
        channel_closed = metrics
            .websocket_deliveries_channel_closed
            .load(Ordering::Relaxed);
        canceled = metrics
            .websocket_deliveries_canceled
            .load(Ordering::Relaxed);
        dropped = metrics.websocket_messages_dropped.load(Ordering::Relaxed);

        let resolved = enqueued + channel_closed + canceled;
        if resolved <= attempts && attempts <= resolved + dropped {
            return;
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!(
        "delivery conservation violated (stable past the {QUIESCENCE_DEADLINE:?} quiescence \
         deadline): expected \
         enqueued + channel_closed + canceled <= attempts <= enqueued + channel_closed + \
         canceled + dropped, got attempts={attempts} enqueued={enqueued} \
         channel_closed={channel_closed} canceled={canceled} dropped={dropped}"
    );
}

/// Connect a real WebSocket client to `ws://{addr}/ws` over a TCP socket whose
/// kernel receive buffer is clamped to `recv_buffer_bytes` BEFORE the connect
/// (the kernel typically doubles the requested value, and it cannot shrink an
/// established connection's window — hence pre-connect).
///
/// A client built this way that simply stops reading wedges the server's
/// socket writes after only a few kilobytes, which is exactly the
/// slow-consumer state the delivery contract must preempt. Regular clients
/// (default buffers) should use their suite's normal `connect` helper.
pub async fn connect_with_small_recv_buffer(
    addr: std::net::SocketAddr,
    recv_buffer_bytes: u32,
) -> WsStream {
    let socket = tokio::net::TcpSocket::new_v4().expect("create IPv4 TCP socket");
    socket
        .set_recv_buffer_size(recv_buffer_bytes)
        .expect("clamp SO_RCVBUF before connect");
    let stream = tokio::time::timeout(Duration::from_secs(10), socket.connect(addr))
        .await
        .expect("small-rcvbuf TCP connect timed out")
        .expect("small-rcvbuf TCP connect failed");

    let url = format!("ws://{addr}/ws");
    let (ws_stream, _response) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::client_async(url, tokio_tungstenite::MaybeTlsStream::Plain(stream)),
    )
    .await
    .expect("small-rcvbuf websocket handshake timed out")
    .expect("small-rcvbuf websocket handshake failed");
    ws_stream
}

enum ServerReadEvent {
    ServerMessage(Box<ServerMessage>),
    SkippedFrame(&'static str),
}

pub fn deadline_after(timeout: Duration) -> Instant {
    Instant::now() + timeout
}

pub async fn next_server_message_within<S>(
    ws: &mut S,
    timeout: Duration,
    context: &str,
) -> ServerMessage
where
    S: Stream<Item = Result<Message, WebSocketError>> + Unpin,
{
    expect_next_server_message_before(ws, deadline_after(timeout), context).await
}

pub async fn expect_next_server_message_before<S>(
    ws: &mut S,
    deadline: Instant,
    context: &str,
) -> ServerMessage
where
    S: Stream<Item = Result<Message, WebSocketError>> + Unpin,
{
    let mut skipped = VecDeque::new();

    loop {
        match next_server_read_event_before(ws, deadline, context).await {
            Some(ServerReadEvent::ServerMessage(message)) => return *message,
            Some(ServerReadEvent::SkippedFrame(name)) => remember_skipped(&mut skipped, name),
            None => panic!(
                "{context}: timed out waiting for ServerMessage; skipped: {}",
                skipped_message_summary(&skipped)
            ),
        }
    }
}

pub async fn next_server_message_before<S>(
    ws: &mut S,
    deadline: Instant,
    context: &str,
) -> Option<ServerMessage>
where
    S: Stream<Item = Result<Message, WebSocketError>> + Unpin,
{
    loop {
        match next_server_read_event_before(ws, deadline, context).await {
            Some(ServerReadEvent::ServerMessage(message)) => return Some(*message),
            Some(ServerReadEvent::SkippedFrame(_)) => {}
            None => return None,
        }
    }
}

pub async fn expect_no_server_message_within<S>(ws: &mut S, timeout: Duration, context: &str)
where
    S: Stream<Item = Result<Message, WebSocketError>> + Unpin,
{
    let deadline = deadline_after(timeout);
    let mut skipped = VecDeque::new();

    loop {
        match next_server_read_event_before(ws, deadline, context).await {
            Some(ServerReadEvent::ServerMessage(message)) => {
                let name = server_message_name(&message);
                panic!(
                    "{context}: expected no ServerMessage, got {name}; skipped: {}",
                    skipped_message_summary(&skipped)
                );
            }
            Some(ServerReadEvent::SkippedFrame(name)) => remember_skipped(&mut skipped, name),
            None => return,
        }
    }
}

async fn next_server_read_event_before<S>(
    ws: &mut S,
    deadline: Instant,
    context: &str,
) -> Option<ServerReadEvent>
where
    S: Stream<Item = Result<Message, WebSocketError>> + Unpin,
{
    if Instant::now() >= deadline {
        return None;
    }

    match timeout_at(deadline, ws.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => Some(ServerReadEvent::ServerMessage(Box::new(
            serde_json::from_str(&text).unwrap_or_else(|error| {
                panic!("{context}: invalid ServerMessage text frame: {error}; text={text:?}")
            }),
        ))),
        Ok(Some(Ok(frame))) => Some(ServerReadEvent::SkippedFrame(websocket_frame_name(&frame))),
        Ok(Some(Err(error))) => {
            panic!("{context}: websocket error while waiting for ServerMessage: {error}")
        }
        Ok(None) => {
            panic!("{context}: websocket stream closed while waiting for ServerMessage")
        }
        Err(_) => None,
    }
}

pub async fn next_matching_server_message_within<S, T>(
    ws: &mut S,
    timeout: Duration,
    context: &str,
    mut pick: impl FnMut(ServerMessage) -> Option<T>,
) -> T
where
    S: Stream<Item = Result<Message, WebSocketError>> + Unpin,
{
    let deadline = deadline_after(timeout);
    let mut skipped = VecDeque::new();

    loop {
        match next_server_read_event_before(ws, deadline, context).await {
            Some(ServerReadEvent::ServerMessage(message)) => {
                let name = server_message_name(&message);
                if let Some(value) = pick(*message) {
                    return value;
                }
                remember_skipped(&mut skipped, name);
            }
            Some(ServerReadEvent::SkippedFrame(name)) => remember_skipped(&mut skipped, name),
            None => panic!(
                "{context}: timed out waiting for matching ServerMessage; skipped: {}",
                skipped_message_summary(&skipped)
            ),
        }
    }
}

pub async fn maybe_next_matching_server_message_until<S, T>(
    ws: &mut S,
    deadline: Instant,
    context: &str,
    pick: impl FnMut(ServerMessage) -> Option<T>,
) -> Option<T>
where
    S: Stream<Item = Result<Message, WebSocketError>> + Unpin,
{
    maybe_next_matching_server_message_with_skipped_until(ws, deadline, context, pick)
        .await
        .0
}

pub async fn maybe_next_matching_server_message_with_skipped_until<S, T>(
    ws: &mut S,
    deadline: Instant,
    context: &str,
    mut pick: impl FnMut(ServerMessage) -> Option<T>,
) -> (Option<T>, String)
where
    S: Stream<Item = Result<Message, WebSocketError>> + Unpin,
{
    let mut skipped = VecDeque::new();

    loop {
        match next_server_read_event_before(ws, deadline, context).await {
            Some(ServerReadEvent::ServerMessage(message)) => {
                let name = server_message_name(&message);
                if let Some(value) = pick(*message) {
                    return (Some(value), skipped_message_summary(&skipped));
                }
                remember_skipped(&mut skipped, name);
            }
            Some(ServerReadEvent::SkippedFrame(name)) => remember_skipped(&mut skipped, name),
            None => return (None, skipped_message_summary(&skipped)),
        }
    }
}

pub async fn maybe_next_matching_server_message_within<S, T>(
    ws: &mut S,
    timeout: Duration,
    context: &str,
    pick: impl FnMut(ServerMessage) -> Option<T>,
) -> Option<T>
where
    S: Stream<Item = Result<Message, WebSocketError>> + Unpin,
{
    maybe_next_matching_server_message_until(ws, deadline_after(timeout), context, pick).await
}

fn remember_skipped(skipped: &mut VecDeque<&'static str>, message_name: &'static str) {
    if skipped.len() == SKIPPED_MESSAGE_LIMIT {
        skipped.pop_front();
    }
    skipped.push_back(message_name);
}

fn skipped_message_summary(skipped: &VecDeque<&'static str>) -> String {
    if skipped.is_empty() {
        "<none>".to_string()
    } else {
        skipped.iter().copied().collect::<Vec<_>>().join(", ")
    }
}

fn websocket_frame_name(frame: &Message) -> &'static str {
    match frame {
        Message::Text(_) => "Text",
        Message::Binary(_) => "BinaryFrame",
        Message::Ping(_) => "PingFrame",
        Message::Pong(_) => "PongFrame",
        Message::Close(_) => "CloseFrame",
        Message::Frame(_) => "RawFrame",
    }
}

fn server_message_name(message: &ServerMessage) -> &'static str {
    match message {
        ServerMessage::Authenticated { .. } => "Authenticated",
        ServerMessage::ProtocolInfo(_) => "ProtocolInfo",
        ServerMessage::AuthenticationError { .. } => "AuthenticationError",
        ServerMessage::RoomJoined(_) => "RoomJoined",
        ServerMessage::RoomJoinFailed { .. } => "RoomJoinFailed",
        ServerMessage::RoomLeft => "RoomLeft",
        ServerMessage::PlayerJoined { .. } => "PlayerJoined",
        ServerMessage::PlayerLeft { .. } => "PlayerLeft",
        ServerMessage::GameData { .. } => "GameData",
        ServerMessage::GameDataBinary { .. } => "GameDataBinary",
        ServerMessage::AuthorityChanged { .. } => "AuthorityChanged",
        ServerMessage::AuthorityResponse { .. } => "AuthorityResponse",
        ServerMessage::LobbyStateChanged { .. } => "LobbyStateChanged",
        ServerMessage::GameStarting { .. } => "GameStarting",
        ServerMessage::Signal { .. } => "Signal",
        ServerMessage::NewPeer { .. } => "NewPeer",
        ServerMessage::SessionPlan(_) => "SessionPlan",
        ServerMessage::Pong => "Pong",
        ServerMessage::Reconnected(_) => "Reconnected",
        ServerMessage::ReconnectionFailed { .. } => "ReconnectionFailed",
        ServerMessage::PlayerReconnected { .. } => "PlayerReconnected",
        ServerMessage::SpectatorJoined(_) => "SpectatorJoined",
        ServerMessage::SpectatorJoinFailed { .. } => "SpectatorJoinFailed",
        ServerMessage::SpectatorLeft { .. } => "SpectatorLeft",
        ServerMessage::NewSpectatorJoined { .. } => "NewSpectatorJoined",
        ServerMessage::SpectatorDisconnected { .. } => "SpectatorDisconnected",
        ServerMessage::Error { .. } => "Error",
        ServerMessage::PeerTransportStatus { .. } => "PeerTransportStatus",
        ServerMessage::RelayStats { .. } => "RelayStats",
        ServerMessage::GoingAway { .. } => "GoingAway",
        ServerMessage::DeliveryReport(_) => "DeliveryReport",
    }
}
