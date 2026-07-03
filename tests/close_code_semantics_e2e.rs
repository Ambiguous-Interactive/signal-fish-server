//! Semantic WebSocket close codes: real-socket end-to-end tests (issue #136,
//! F1 / proposal C).
//!
//! The farewell `Error` frame is best-effort — on the congested socket a
//! slow-consumer eviction escapes, it frequently cannot be delivered at all.
//! The close frame's code travels in the closing handshake itself, so it is
//! the one attribution signal a client can always read. Contract pinned here
//! (RFC 6455 private range; the assignments are documented protocol surface
//! and must never be renumbered):
//!
//! - `4001 auth_timeout` — never authenticated within
//!   `websocket.auth_timeout_secs`;
//! - `4002 slow_consumer` — evicted by the delivery contract;
//! - `4003 activity_timeout` — evicted by the `server.ping_timeout` reaper;
//! - `4004 idle_timeout` — no inbound frame within
//!   `websocket.idle_timeout_secs`.
//!
//! (`4000 server_shutdown` is defined in the contract but has no in-process
//! trigger today; `CloseReason::Unregistered` closes with a normal `1000`.)

mod test_helpers;
mod websocket_test_helpers;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::config::ProtocolConfig;
use signal_fish_server::protocol::ClientMessage;
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::create_router;
use std::sync::Arc;
use test_helpers::create_test_server_with_config;
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Generous ceiling on every "read until the close frame arrives" wait; the
/// per-test timeouts under test are all ≤5s.
const CLOSE_DEADLINE: tokio::time::Duration = tokio::time::Duration::from_secs(30);

async fn start_server(server: Arc<EnhancedGameServer>) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("read listener address");
    let router = create_router("http://localhost:3000").with_state(server);
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("test server serve loop");
    });
    addr
}

async fn connect(addr: std::net::SocketAddr) -> WsStream {
    let url = format!("ws://{addr}/ws");
    let (ws, _) = tokio::time::timeout(tokio::time::Duration::from_secs(10), connect_async(&url))
        .await
        .expect("websocket connect timed out")
        .expect("websocket connect failed");
    ws
}

/// Drain frames until the server's close frame (or EOF) arrives; return the
/// observed `(code, reason)`. Panics loudly if the stream terminates without
/// any close frame — a bare termination is exactly the anti-pattern this
/// suite exists to forbid.
async fn read_close_frame(ws: &mut WsStream, context: &str) -> (u16, String) {
    let deadline = tokio::time::Instant::now() + CLOSE_DEADLINE;
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_else(|| panic!("{context}: timed out waiting for the close frame"));
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(Message::Close(Some(frame))))) => {
                return (frame.code.into(), frame.reason.to_string());
            }
            Ok(Some(Ok(Message::Close(None)))) => {
                panic!("{context}: server closed with NO close code (bare close frame)")
            }
            Ok(Some(Ok(_other_frame))) => continue,
            Ok(Some(Err(error))) => {
                panic!("{context}: transport error instead of a semantic close: {error}")
            }
            Ok(None) => panic!("{context}: stream ended with no close frame at all"),
            Err(_elapsed) => panic!("{context}: timed out waiting for the close frame"),
        }
    }
}

fn base_config() -> ServerConfig {
    ServerConfig {
        // Long reaper window by default so individual tests opt IN to the
        // reaper.
        ping_timeout: std::time::Duration::from_secs(600),
        ..ServerConfig::default()
    }
}

async fn authenticate(ws: &mut WsStream) {
    let auth = ClientMessage::Authenticate {
        app_id: "close-code-test".to_string(),
        sdk_version: None,
        platform: None,
        game_data_format: None,
        protocol_version: Some(2),
        supported_transports: None,
        supported_topologies: None,
    };
    let json = serde_json::to_string(&auth).expect("serialize Authenticate");
    ws.send(Message::Text(json.into()))
        .await
        .expect("send Authenticate");
}

/// A connection that never authenticates is closed with `4001 auth_timeout`
/// once `websocket.auth_timeout_secs` (validated floor: 5s) elapses.
#[tokio::test]
async fn auth_timeout_closes_with_4001() {
    let mut config = base_config();
    // The test helpers disable auth (connections auto-authenticate and the
    // pre-auth deadline never arms); this scenario is ABOUT that deadline.
    config.auth_enabled = true;
    config.websocket_config.auth_timeout_secs = 5;
    let server = create_test_server_with_config(config, ProtocolConfig::default()).await;
    let addr = start_server(server).await;

    let mut ws = connect(addr).await;
    let (code, reason) = read_close_frame(&mut ws, "auth timeout").await;
    assert_eq!(code, 4001, "auth timeout must close with 4001 ({reason})");
    assert_eq!(reason, "auth_timeout");
}

/// A slow consumer evicted by the delivery contract is closed with
/// `4002 slow_consumer` — readable even though the farewell `Error` frame may
/// be buried behind the congested queue.
#[tokio::test]
async fn slow_consumer_eviction_closes_with_4002() {
    let mut config = base_config();
    config.websocket_config.send_queue_capacity = 8;
    config.websocket_config.slow_consumer_timeout_ms = 300;
    let server = create_test_server_with_config(config, ProtocolConfig::default()).await;
    let metrics = server.metrics();
    let addr = start_server(server).await;

    let mut sender = connect(addr).await;
    let stalled = websocket_test_helpers::connect_with_small_recv_buffer(addr, 4_096).await;
    let (mut stalled_sink, mut stalled_rx) = stalled.split();

    join(&mut sender, "CloseSender").await;
    join_split(&mut stalled_sink, &mut stalled_rx, "CloseStalled").await;

    // Flood until the eviction is recorded; the stalled client reads nothing.
    let padding = "x".repeat(12 * 1024);
    let flood_deadline = tokio::time::Instant::now() + CLOSE_DEADLINE;
    while metrics
        .websocket_slow_consumer_disconnects
        .load(std::sync::atomic::Ordering::Relaxed)
        == 0
    {
        assert!(
            tokio::time::Instant::now() < flood_deadline,
            "slow-consumer eviction never happened"
        );
        let message = ClientMessage::GameData {
            data: serde_json::json!({ "padding": padding.as_str() }),
        };
        let json = serde_json::to_string(&message).expect("serialize GameData");
        sender
            .send(Message::Text(json.into()))
            .await
            .expect("send GameData");
    }

    // The stalled client resumes reading: buried GameData first, then the
    // semantic close frame.
    let mut stalled_ws = stalled_rx.reunite(stalled_sink).expect("reunite halves");
    let (code, reason) = read_close_frame(&mut stalled_ws, "slow consumer").await;
    assert_eq!(code, 4002, "slow consumer must close with 4002 ({reason})");
    assert_eq!(reason, "slow_consumer");
}

/// A client the activity reaper evicts (`server.ping_timeout`) is closed with
/// `4003 activity_timeout`.
#[tokio::test]
async fn activity_reaper_eviction_closes_with_4003() {
    let mut config = base_config();
    config.ping_timeout = std::time::Duration::from_secs(1);
    config.room_cleanup_interval = std::time::Duration::from_secs(1);
    let server = create_test_server_with_config(config, ProtocolConfig::default()).await;
    // The test router does not run the maintenance loop (production wiring
    // starts it separately); this scenario is ABOUT the reaper, so start it.
    let reaper = server.clone();
    tokio::spawn(async move { reaper.cleanup_task().await });
    let addr = start_server(server).await;

    let mut ws = connect(addr).await;
    // Authenticate so the (5s-floor) auth deadline cannot race the 1s reaper.
    authenticate(&mut ws).await;

    // Send nothing further: the reaper sweep (1s cadence, 1s window) evicts.
    let (code, reason) = read_close_frame(&mut ws, "activity reaper").await;
    assert_eq!(
        code, 4003,
        "reaper eviction must close with 4003 ({reason})"
    );
    assert_eq!(reason, "activity_timeout");
}

/// A connection idle past `websocket.idle_timeout_secs` is closed with
/// `4004 idle_timeout`.
#[tokio::test]
async fn idle_timeout_closes_with_4004() {
    let mut config = base_config();
    config.websocket_config.idle_timeout_secs = 1;
    let server = create_test_server_with_config(config, ProtocolConfig::default()).await;
    let addr = start_server(server).await;

    let mut ws = connect(addr).await;
    authenticate(&mut ws).await;

    let (code, reason) = read_close_frame(&mut ws, "idle timeout").await;
    assert_eq!(code, 4004, "idle timeout must close with 4004 ({reason})");
    assert_eq!(reason, "idle_timeout");
}

/// Join a room over a whole `WsStream` (drains until `RoomJoined`).
async fn join(ws: &mut WsStream, player_name: &str) {
    let join = ClientMessage::JoinRoom {
        game_name: "close_code_game".to_string(),
        room_code: Some("CLOSE1".to_string()),
        player_name: player_name.to_string(),
        max_players: Some(4),
        supports_authority: Some(false),
        relay_transport: None,
    };
    let json = serde_json::to_string(&join).expect("serialize JoinRoom");
    ws.send(Message::Text(json.into()))
        .await
        .expect("send JoinRoom");
    wait_for_room_joined(ws, player_name).await;
}

async fn wait_for_room_joined(ws: &mut WsStream, player_name: &str) {
    loop {
        let frame = tokio::time::timeout(CLOSE_DEADLINE, ws.next())
            .await
            .expect("timed out waiting for RoomJoined")
            .expect("connection closed while joining")
            .expect("websocket error while joining");
        let Message::Text(text) = frame else { continue };
        let message: signal_fish_server::protocol::ServerMessage =
            serde_json::from_str(&text).expect("valid ServerMessage");
        match message {
            signal_fish_server::protocol::ServerMessage::RoomJoined(_) => return,
            signal_fish_server::protocol::ServerMessage::RoomJoinFailed { reason, .. } => {
                panic!("join failed for {player_name}: {reason}")
            }
            _ => continue,
        }
    }
}

/// Join over split halves (the stalled client's stream is already split).
async fn join_split(
    sink: &mut futures_util::stream::SplitSink<WsStream, Message>,
    rx: &mut futures_util::stream::SplitStream<WsStream>,
    player_name: &str,
) {
    let join = ClientMessage::JoinRoom {
        game_name: "close_code_game".to_string(),
        room_code: Some("CLOSE1".to_string()),
        player_name: player_name.to_string(),
        max_players: Some(4),
        supports_authority: Some(false),
        relay_transport: None,
    };
    let json = serde_json::to_string(&join).expect("serialize JoinRoom");
    sink.send(Message::Text(json.into()))
        .await
        .expect("send JoinRoom");
    loop {
        let frame = tokio::time::timeout(CLOSE_DEADLINE, rx.next())
            .await
            .expect("timed out waiting for RoomJoined")
            .expect("connection closed while joining")
            .expect("websocket error while joining");
        let Message::Text(text) = frame else { continue };
        let message: signal_fish_server::protocol::ServerMessage =
            serde_json::from_str(&text).expect("valid ServerMessage");
        match message {
            signal_fish_server::protocol::ServerMessage::RoomJoined(_) => return,
            signal_fish_server::protocol::ServerMessage::RoomJoinFailed { reason, .. } => {
                panic!("join failed for {player_name}: {reason}")
            }
            _ => continue,
        }
    }
}
