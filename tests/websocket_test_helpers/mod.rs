#![allow(dead_code)]

use std::collections::VecDeque;
use std::time::Duration;

use futures_util::{Stream, StreamExt};
use signal_fish_server::protocol::ServerMessage;
use tokio::time::{timeout_at, Instant};
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};

pub type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const SKIPPED_MESSAGE_LIMIT: usize = 8;

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
    }
}
