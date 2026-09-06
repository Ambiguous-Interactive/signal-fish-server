//! Semantic WebSocket close codes: real-socket end-to-end tests (issue #136,
//! F1 / proposal C).
//!
//! The farewell `Error` frame is best-effort — on the congested socket a
//! slow-consumer eviction escapes, it frequently cannot be delivered at all.
//! The close frame's code travels in the closing handshake itself, so it is
//! the one attribution signal a client can always read. Contract pinned here:
//! standard RFC 6455 codes plus documented private-range assignments that must
//! never be renumbered.
//!
//! - `4001 auth_timeout` — no app-ID handshake input within
//!   `websocket.auth_timeout_secs`;
//! - `4002 slow_consumer` — evicted by the delivery contract;
//! - `4003 activity_timeout` — server Ping write timed out, the matching Pong
//!   missed its deadline, or the `server.ping_timeout` reaper evicted it;
//! - `4004 idle_timeout` — no inbound frame within
//!   `websocket.idle_timeout_secs`.
//! - `4005 room_inactive` — the assigned room was deleted after exceeding
//!   `server.inactive_room_timeout`.
//! - `4006 inbound_rate_limited` — the connection exhausted its per-window
//!   inbound application-message budget (`rate_limit.max_inbound_messages`).
//! - `1009 outbound_message_too_large` — a complete encoded server message
//!   exceeded the deployment's advertised aggregate outbound payload limit.
//!
//! (`4000 server_shutdown` is defined in the contract but has no in-process
//! trigger today; `CloseReason::Unregistered` closes with a normal `1000`.)

mod test_helpers;
mod websocket_test_helpers;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::config::ProtocolConfig;
use signal_fish_server::protocol::{ClientMessage, RoomJoinedPayload, ServerMessage};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::create_router;
use std::sync::Arc;
use test_helpers::{create_test_server_with_config, RunningTestServer};
use tokio_tungstenite::{connect_async, tungstenite::Message};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Generous ceiling on every "read until the close frame arrives" wait; the
/// per-test timeouts under test are all ≤5s.
const CLOSE_DEADLINE: tokio::time::Duration = tokio::time::Duration::from_secs(30);

async fn start_server(server: Arc<EnhancedGameServer>) -> RunningTestServer {
    let router = create_router("http://localhost:3000").with_state(server.clone());
    RunningTestServer::spawn(server, router).await
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
        requested_capabilities: None,
    };
    let json = serde_json::to_string(&auth).expect("serialize Authenticate");
    ws.send(Message::Text(json.into()))
        .await
        .expect("send Authenticate");
}

async fn authenticate_v3(ws: &mut WsStream) {
    let auth = ClientMessage::Authenticate {
        app_id: "close-code-test".to_string(),
        sdk_version: None,
        platform: None,
        game_data_format: None,
        protocol_version: Some(3),
        supported_transports: None,
        supported_topologies: None,
        requested_capabilities: None,
    };
    let json = serde_json::to_string(&auth).expect("serialize Authenticate");
    ws.send(Message::Text(json.into()))
        .await
        .expect("send Authenticate");
}

/// An oversized server application message is rejected before the WebSocket
/// sink sees any prefix, and the connection closes with RFC 6455's standard
/// message-too-big code rather than silently truncating protocol state.
///
/// The trigger must respect the relay-envelope headroom guard (`outbound ≥
/// inbound + 256`): the fixed relay envelope can no longer push any single
/// admitted frame past a validated pairing, and value-level re-serialization
/// growth (number normalization, fallback escaping) is not attacker-shaped
/// here. The oversized frame is therefore the aggregate `RoomJoined` roster,
/// which grows by roughly one `PlayerInfo` entry per member and eventually
/// crosses the small outbound cap. Which joiner first overflows depends on
/// wire details, so the test walks the member sequence and asserts the
/// contract on the first close: code `1009`, reason
/// `outbound_message_too_large`. (The old auth-response trigger is
/// unreachable under any validated pairing: the ~155-byte response can never
/// exceed the minimum legal outbound cap.)
#[tokio::test]
async fn outbound_message_over_configured_limit_closes_with_1009() {
    /// Bounded per-frame wait for the join walk, so a stalled server fails
    /// the walk in minutes rather than accumulating 30-second deadlines.
    const JOIN_WALK_DEADLINE: tokio::time::Duration = tokio::time::Duration::from_secs(5);

    let mut config = base_config();
    // Pairing-legal small caps: the handshake and join frames fit the inbound
    // cap, while the aggregate roster snapshot grows past the outbound cap
    // after a few members join. The metadata cap is lowered to keep its
    // roster aggregate under the outbound cap (issue #524 constructor guard).
    config.max_message_size = 200;
    config.max_signal_bytes = 200;
    config.max_outbound_message_size = config.max_message_size
        + 4 * signal_fish_server::config::defaults::RELAY_ENVELOPE_HEADROOM_BYTES;
    config.max_connection_info_bytes = 8;
    let server = create_test_server_with_config(config, ProtocolConfig::default()).await;
    let running_server = start_server(server).await;
    let addr = running_server.addr();

    fn join_frame(room_code: Option<String>, player_name: String) -> Message {
        let join = ClientMessage::JoinRoom {
            game_name: "overflow-close".to_string(),
            room_code,
            player_name,
            max_players: Some(24),
            supports_authority: None,
            relay_transport: None,
        };
        Message::Text(
            serde_json::to_string(&join)
                .expect("serialize JoinRoom")
                .into(),
        )
    }

    // The creator mints the room; later joiners reuse its room code.
    let mut creator = connect(addr).await;
    authenticate(&mut creator).await;
    creator
        .send(join_frame(None, "player-00000".to_string()))
        .await
        .expect("send JoinRoom");
    let mut room_code = None;
    let mut seen_frames = Vec::new();
    for _ in 0..8 {
        let frame = tokio::time::timeout(JOIN_WALK_DEADLINE, creator.next())
            .await
            .expect("timed out waiting for the creator's RoomJoined")
            .expect("creator connection closed while joining")
            .expect("websocket error while joining");
        let Message::Text(text) = frame else {
            continue;
        };
        match serde_json::from_str::<ServerMessage>(&text) {
            Ok(ServerMessage::RoomJoined(payload)) => {
                room_code = Some(payload.room_code);
                break;
            }
            other => seen_frames.push(format!("{other:?}")),
        }
    }
    let room_code = room_code.unwrap_or_else(|| {
        panic!("creator must receive RoomJoined with the room code; saw {seen_frames:?}")
    });

    // Walk the member sequence until a roster snapshot crosses the outbound
    // cap. Joiners whose own RoomJoined arrives stay open (their sockets are
    // held so the roster keeps growing); the first joiner whose snapshot
    // fails its flush must close with exactly 1009.
    let mut held_sockets = Vec::new();
    let mut first_close = None;
    for index in 1..16 {
        let mut ws = connect(addr).await;
        authenticate(&mut ws).await;
        ws.send(join_frame(
            Some(room_code.clone()),
            format!("player-{index:05}"),
        ))
        .await
        .expect("send JoinRoom");
        for _ in 0..8 {
            match tokio::time::timeout(JOIN_WALK_DEADLINE, ws.next()).await {
                Ok(Some(Ok(Message::Close(Some(frame))))) => {
                    assert_eq!(
                        u16::from(frame.code),
                        1009,
                        "oversized outbound message must close with 1009"
                    );
                    assert_eq!(
                        frame.reason.as_str(),
                        "outbound_message_too_large",
                        "the oversize close must carry its documented reason"
                    );
                    first_close = Some(());
                    break;
                }
                Ok(Some(Ok(Message::Text(text)))) => {
                    if text.contains("\"RoomJoined\"") {
                        // The join landed; hold the socket so this member
                        // stays in the roster for the next joiner.
                        held_sockets.push(ws);
                        break;
                    }
                }
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(error))) => panic!("joiner {index}: transport error: {error}"),
                Ok(None) => panic!("joiner {index}: stream ended with no close frame"),
                Err(_elapsed) => panic!("joiner {index}: timed out waiting for join response"),
            }
        }
        if first_close.is_some() {
            break;
        }
    }

    assert!(
        first_close.is_some(),
        "the growing roster snapshot must eventually exceed the outbound cap and close \
         its joiner with 1009"
    );

    running_server.shutdown().await;
}

/// A connection that never authenticates is closed with `4001 auth_timeout`
/// once `websocket.auth_timeout_secs` (validated floor: 5s) elapses.
#[tokio::test]
async fn auth_timeout_closes_with_4001() {
    let mut config = base_config();
    // The test helpers disable auth (connections auto-authenticate and the
    // pre-auth deadline never arms); this scenario is ABOUT that deadline.
    config.app_id_allowlist_enabled = true;
    config.websocket_config.auth_timeout_secs = 5;
    let server = create_test_server_with_config(config, ProtocolConfig::default()).await;
    let running_server = start_server(server).await;
    let addr = running_server.addr();

    let mut ws = connect(addr).await;
    let (code, reason) = read_close_frame(&mut ws, "auth timeout").await;
    assert_eq!(code, 4001, "auth timeout must close with 4001 ({reason})");
    assert_eq!(reason, "auth_timeout");
    running_server.shutdown().await;
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
    let running_server = start_server(server).await;
    let addr = running_server.addr();

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
            class: None,
            key: None,
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
    running_server.shutdown().await;
}

/// A client the activity reaper evicts (`server.ping_timeout`) is closed with
/// `4003 activity_timeout`.
#[tokio::test]
async fn activity_reaper_eviction_closes_with_4003() {
    let mut config = base_config();
    config.ping_timeout = std::time::Duration::from_secs(1);
    // Constructor validation rejects a slow-consumer park that can outlast
    // the ping deadline (timeout inversion); keep the cap under the reaper.
    config.websocket_config.slow_consumer_timeout_ms = 500;
    config.room_cleanup_interval = std::time::Duration::from_secs(1);
    let server = create_test_server_with_config(config, ProtocolConfig::default()).await;
    // The test router does not run the maintenance loop (production wiring
    // starts it separately); this scenario is ABOUT the reaper, so start it.
    let reaper = server.clone();
    tokio::spawn(async move { reaper.cleanup_task().await });
    let running_server = start_server(server).await;
    let addr = running_server.addr();

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
    running_server.shutdown().await;
}

/// A connection idle past `websocket.idle_timeout_secs` is closed with
/// `4004 idle_timeout`.
#[tokio::test]
async fn idle_timeout_closes_with_4004() {
    let mut config = base_config();
    config.websocket_config.idle_timeout_secs = 1;
    let server = create_test_server_with_config(config, ProtocolConfig::default()).await;
    let running_server = start_server(server).await;
    let addr = running_server.addr();

    let mut ws = connect(addr).await;
    authenticate(&mut ws).await;

    let (code, reason) = read_close_frame(&mut ws, "idle timeout").await;
    assert_eq!(code, 4004, "idle timeout must close with 4004 ({reason})");
    assert_eq!(reason, "idle_timeout");
    running_server.shutdown().await;
}

/// A connection that exhausts `rate_limit.max_inbound_error_replies` inside
/// one window is closed with `4006 inbound_rate_limited` (issue #518). Only
/// frames the server answers with a polite `Error` reply charge the gate, so
/// one attacker write can no longer buy unbounded 1:1 error replies — while
/// admitted traffic (which carries its own budgets) is never gated.
#[tokio::test]
async fn inbound_rate_limit_exhaustion_closes_with_4006() {
    let mut config = base_config();
    config.rate_limit_config.max_inbound_error_replies = 3;
    let server = create_test_server_with_config(config, ProtocolConfig::default()).await;
    let running_server = start_server(server).await;
    let addr = running_server.addr();

    let mut ws = connect(addr).await;
    // A successful Authenticate produces no error reply, so it does NOT
    // charge the gate; only rejected frames do.
    authenticate(&mut ws).await;

    let garbage = Message::Text("not json at all".into());
    for _ in 0..3 {
        ws.send(garbage.clone())
            .await
            .expect("send garbage frame while budget admits it");
    }
    ws.send(garbage)
        .await
        .expect("send the budget-exhausting frame");

    let (code, reason) = read_close_frame(&mut ws, "inbound rate limit").await;
    assert_eq!(
        code, 4006,
        "inbound rate-limit exhaustion must close with 4006 ({reason})"
    );
    assert_eq!(reason, "inbound_rate_limited");
    running_server.shutdown().await;
}

/// A connection under its error-reply budget is never disconnected by the
/// gate: the cap bounds amplified rejections, not honest clients.
#[tokio::test]
async fn inbound_frames_under_the_budget_leave_the_connection_open() {
    let mut config = base_config();
    config.rate_limit_config.max_inbound_error_replies = 5;
    let server = create_test_server_with_config(config, ProtocolConfig::default()).await;
    let running_server = start_server(server).await;
    let addr = running_server.addr();

    let mut ws = connect(addr).await;
    authenticate(&mut ws).await;

    // Spend four more slots (five total, budget five), then prove the
    // connection still answers a Ping after the malformed frames.
    let garbage = Message::Text("not json at all".into());
    for _ in 0..4 {
        ws.send(garbage.clone())
            .await
            .expect("send garbage frame under budget");
    }
    ws.send(Message::Ping(b"still-here".as_ref().into()))
        .await
        .expect("send liveness Ping");
    let pong_deadline = tokio::time::Instant::now() + CLOSE_DEADLINE;
    let mut saw_pong = false;
    while tokio::time::Instant::now() < pong_deadline {
        match tokio::time::timeout(
            pong_deadline.saturating_duration_since(tokio::time::Instant::now()),
            ws.next(),
        )
        .await
        {
            Ok(Some(Ok(Message::Pong(_)))) => {
                saw_pong = true;
                break;
            }
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(error))) => panic!("transport error under budget: {error}"),
            Ok(None) => panic!("stream ended while under the inbound budget"),
            Err(_elapsed) => break,
        }
    }
    assert!(
        saw_pong,
        "a connection within its inbound budget must stay open and answer Pings"
    );
    running_server.shutdown().await;
}

/// A seated client whose room exceeds `server.inactive_room_timeout` is
/// terminally unrouted and closed with `4005 room_inactive`.
#[tokio::test]
async fn inactive_room_cleanup_closes_with_4005() {
    let mut config = base_config();
    config.room_cleanup_interval = std::time::Duration::from_secs(1);
    config.inactive_room_timeout = std::time::Duration::from_secs(1);
    // Constructor validation requires the heartbeat throttle to stay below
    // the inactive-room deadline; keep it under the 1s test window.
    config.heartbeat_throttle = std::time::Duration::from_millis(500);
    let server = create_test_server_with_config(config, ProtocolConfig::default()).await;
    let cleanup = server.clone();
    tokio::spawn(async move { cleanup.cleanup_task().await });
    let running_server = start_server(server).await;

    let mut ws = connect(running_server.addr()).await;
    authenticate(&mut ws).await;
    join(&mut ws, "inactive member").await;

    let (code, reason) = read_close_frame(&mut ws, "inactive room cleanup").await;
    assert_eq!(
        code, 4005,
        "inactive room cleanup must close with 4005 ({reason})"
    );
    assert_eq!(reason, "room_inactive");
    running_server.shutdown().await;
}

/// A shutdown drain sends the v3 `GoingAway` advisory, then closes with
/// `4000 server_shutdown`. The disconnect must not create a pending
/// reconnection record: a shutting-down single-process server cannot honor
/// instance-local reconnect state after exit.
#[tokio::test]
async fn shutdown_drain_sends_goingaway_and_closes_4000_without_reconnect_record() {
    let mut config = base_config();
    config.drain_grace = tokio::time::Duration::from_secs(1);
    let server = create_test_server_with_config(config, ProtocolConfig::default()).await;
    let reconnection_manager = server
        .reconnection_manager()
        .expect("test config enables reconnection");
    let running_server = start_server(server.clone()).await;
    let addr = running_server.addr();

    let mut ws = connect(addr).await;
    authenticate_v3(&mut ws).await;
    let joined = join_payload(&mut ws, "ShutdownPeer").await;
    assert!(
        joined.reconnection_token.is_some(),
        "v3 join should pre-issue a reconnect token so shutdown can prove it is discarded"
    );
    let player_id = joined.player_id;

    let drain = server.begin_shutdown_drain();
    assert!(
        drain.started_by_this_call,
        "test should be the first drain initiator"
    );
    assert_eq!(server.announce_shutdown_drain(drain).await, 1);
    assert_eq!(
        server.close_connections_for_shutdown(),
        1,
        "shutdown should request close for the connected peer"
    );

    let (deadline_ms, retry_after_secs) = read_going_away(&mut ws).await;
    assert_eq!(deadline_ms, drain.deadline_ms);
    assert_eq!(retry_after_secs, Some(1));

    let (code, reason) = read_close_frame(&mut ws, "shutdown drain").await;
    assert_eq!(code, 4000, "shutdown must close with 4000 ({reason})");
    assert_eq!(reason, "server_shutdown");

    let deadline = tokio::time::Instant::now() + CLOSE_DEADLINE;
    loop {
        if server.get_client_room(&player_id).await.is_none() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "shutdown disconnect did not unregister the player"
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    assert!(
        !reconnection_manager
            .has_pending_reconnection(&player_id)
            .await,
        "shutdown drain-close must not leave a claimable reconnection record"
    );
    running_server.shutdown().await;
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

async fn join_payload(ws: &mut WsStream, player_name: &str) -> Box<RoomJoinedPayload> {
    let join = ClientMessage::JoinRoom {
        game_name: "close_code_game".to_string(),
        room_code: Some("CLOSE2".to_string()),
        player_name: player_name.to_string(),
        max_players: Some(4),
        supports_authority: Some(false),
        relay_transport: None,
    };
    let json = serde_json::to_string(&join).expect("serialize JoinRoom");
    ws.send(Message::Text(json.into()))
        .await
        .expect("send JoinRoom");
    loop {
        let frame = tokio::time::timeout(CLOSE_DEADLINE, ws.next())
            .await
            .expect("timed out waiting for RoomJoined")
            .expect("connection closed while joining")
            .expect("websocket error while joining");
        let Message::Text(text) = frame else { continue };
        let message: ServerMessage = serde_json::from_str(&text).expect("valid ServerMessage");
        match message {
            ServerMessage::RoomJoined(payload) => return payload,
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                panic!("join failed for {player_name}: {reason} ({error_code:?})")
            }
            _ => continue,
        }
    }
}

async fn read_going_away(ws: &mut WsStream) -> (u64, Option<u64>) {
    loop {
        let frame = tokio::time::timeout(CLOSE_DEADLINE, ws.next())
            .await
            .expect("timed out waiting for GoingAway")
            .expect("connection closed before GoingAway")
            .expect("websocket error while waiting for GoingAway");
        let Message::Text(text) = frame else { continue };
        let message: ServerMessage = serde_json::from_str(&text).expect("valid ServerMessage");
        if let ServerMessage::GoingAway {
            deadline_ms,
            retry_after_secs,
        } = message
        {
            return (deadline_ms, retry_after_secs);
        }
    }
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
