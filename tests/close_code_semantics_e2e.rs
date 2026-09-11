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
//!   inbound error-reply budget (`rate_limit.max_inbound_error_replies`).
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

            password: None,
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

/// Application-level `Ping` frames each buy a `Pong` reply. A Ping flood is
/// the same one-write-one-reply amplification channel the 4006 gate exists to
/// bound, so the replies must charge the same per-connection budget and the
/// exhausting frame must close with `4006 inbound_rate_limited` (issue #396).
#[tokio::test]
async fn application_ping_flood_exhausting_the_reply_budget_closes_with_4006() {
    let mut config = base_config();
    config.rate_limit_config.max_inbound_error_replies = 3;
    let server = create_test_server_with_config(config, ProtocolConfig::default()).await;
    let running_server = start_server(server).await;
    let addr = running_server.addr();

    let mut ws = connect(addr).await;
    authenticate(&mut ws).await;

    let ping = Message::Text(
        serde_json::to_string(&ClientMessage::Ping)
            .expect("serialize Ping")
            .into(),
    );
    for _ in 0..4 {
        ws.send(ping.clone()).await.expect("send application Ping");
    }

    // Exactly the three budgeted Pongs may be produced; the fourth Ping's
    // reply is withheld and the connection closes instead.
    let deadline = tokio::time::Instant::now() + CLOSE_DEADLINE;
    let mut pongs = 0;
    let (code, reason) = loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_else(|| panic!("timed out waiting for the 4006 close"));
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                if matches!(
                    serde_json::from_str::<ServerMessage>(&text),
                    Ok(ServerMessage::Pong)
                ) {
                    pongs += 1;
                }
            }
            Ok(Some(Ok(Message::Close(Some(frame))))) => {
                break (u16::from(frame.code), frame.reason.to_string());
            }
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(error))) => panic!("transport error during Ping flood: {error}"),
            Ok(None) => panic!("stream ended with no close frame during Ping flood"),
            Err(_elapsed) => panic!("timed out waiting for the 4006 close"),
        }
    };
    assert_eq!(
        pongs, 3,
        "only the budgeted Pong replies may be produced before the close"
    );
    assert_eq!(
        code, 4006,
        "Ping-flood reply exhaustion must close with 4006 ({reason})"
    );
    assert_eq!(reason, "inbound_rate_limited");
    running_server.shutdown().await;
}

/// Roomless `Signal` frames are refused with a polite `NotInRoom` reply each.
/// The refusal replies must charge the 4006 budget, or a single open-mode
/// connection can buy unbounded amplified replies at line rate forever
/// (issue #396).
#[tokio::test]
async fn roomless_signal_flood_exhausting_the_reply_budget_closes_with_4006() {
    let mut config = base_config();
    config.rate_limit_config.max_inbound_error_replies = 3;
    let server = create_test_server_with_config(config, ProtocolConfig::default()).await;
    let running_server = start_server(server).await;
    let addr = running_server.addr();

    let mut ws = connect(addr).await;
    authenticate(&mut ws).await;

    let signal = Message::Text(
        serde_json::to_string(&ClientMessage::Signal {
            to: uuid::Uuid::new_v4(),
            generation: uuid::Uuid::new_v4(),
            signal: serde_json::Value::Null,
        })
        .expect("serialize Signal")
        .into(),
    );
    for _ in 0..4 {
        ws.send(signal.clone()).await.expect("send roomless Signal");
    }

    let (code, reason) = read_close_frame(&mut ws, "roomless Signal flood").await;
    assert_eq!(
        code, 4006,
        "roomless Signal refusal exhaustion must close with 4006 ({reason})"
    );
    assert_eq!(reason, "inbound_rate_limited");
    running_server.shutdown().await;
}

/// Oversized binary relay payloads on a binary-negotiated connection buy a
/// polite `MessageTooLarge` reply each — the exact asymmetric twin of the
/// (already charged) oversized-text path. The refusal replies must charge the
/// 4006 budget (issue #396).
#[tokio::test]
async fn oversized_binary_flood_exhausting_the_reply_budget_closes_with_4006() {
    let mut config = base_config();
    config.rate_limit_config.max_inbound_error_replies = 3;
    // The inbound cap must admit the Authenticate frame itself (~114 bytes)
    // while staying under the 2x transport cap for the oversized payloads.
    // The signal/connection-info caps must follow `max_message_size` down so
    // the construction-time security validation stays satisfied; the
    // game-data oversize check itself reads `max_message_size` only.
    config.max_message_size = 256;
    config.max_signal_bytes = 256;
    config.max_connection_info_bytes = 256;
    let mut protocol = ProtocolConfig::default();
    protocol.sdk_compatibility.enforce = false;
    let server = create_test_server_with_config(config, protocol).await;
    let running_server = start_server(server).await;
    let addr = running_server.addr();

    let mut ws = connect(addr).await;
    // Negotiate the binary relay lane so the oversized payloads reach the
    // game-data handler instead of the (already charged) binary-on-JSON
    // refusal in the receive loop.
    let auth = ClientMessage::Authenticate {
        app_id: "close-code-test".to_string(),
        sdk_version: None,
        platform: None,
        game_data_format: Some(signal_fish_server::protocol::GameDataEncoding::MessagePack),
        protocol_version: Some(3),
        supported_transports: None,
        supported_topologies: None,
        requested_capabilities: None,
    };
    ws.send(Message::Text(
        serde_json::to_string(&auth)
            .expect("serialize Authenticate")
            .into(),
    ))
    .await
    .expect("send Authenticate");

    let oversized = Message::Binary(vec![0u8; 300].into());
    for _ in 0..4 {
        ws.send(oversized.clone())
            .await
            .expect("send oversized binary frame");
    }

    let (code, reason) = read_close_frame(&mut ws, "oversized binary flood").await;
    assert_eq!(
        code, 4006,
        "oversized binary refusal exhaustion must close with 4006 ({reason})"
    );
    assert_eq!(reason, "inbound_rate_limited");
    running_server.shutdown().await;
}

/// A retryable handshake refusal is a polite per-frame reply: an enforced
/// SDK-compatibility failure answers every retried `Authenticate` with an
/// `AuthenticationError`. The refusal loop must charge the 4006 budget — the
/// exhausting refusal is withheld and the connection closes with
/// `4006 inbound_rate_limited` (issue #396).
#[tokio::test]
async fn sdk_refusal_loop_exhausting_the_reply_budget_closes_with_4006() {
    let mut config = base_config();
    config.rate_limit_config.max_inbound_error_replies = 3;
    let mut protocol = ProtocolConfig::default();
    protocol.sdk_compatibility.enforce = true;
    let server = create_test_server_with_config(config, protocol).await;
    let running_server = start_server(server).await;
    let addr = running_server.addr();

    let mut ws = connect(addr).await;

    // platform "unity" with an SDK below the enforced minimum (1.10.0)
    // fails the compatibility check on every attempt and stays retryable.
    let auth = ClientMessage::Authenticate {
        app_id: "close-code-test".to_string(),
        sdk_version: Some("0.0.1".to_string()),
        platform: Some("unity".to_string()),
        game_data_format: None,
        protocol_version: None,
        supported_transports: None,
        supported_topologies: None,
        requested_capabilities: None,
    };
    let auth_frame = Message::Text(
        serde_json::to_string(&auth)
            .expect("serialize Authenticate")
            .into(),
    );
    for _ in 0..4 {
        ws.send(auth_frame.clone())
            .await
            .expect("send refusing Authenticate");
    }

    let (code, reason) = read_close_frame(&mut ws, "SDK refusal loop").await;
    assert_eq!(
        code, 4006,
        "SDK-refusal exhaustion must close with 4006 ({reason})"
    );
    assert_eq!(reason, "inbound_rate_limited");
    running_server.shutdown().await;
}

/// Admitted relay traffic never charges the 4006 gate: the data planes carry
/// their own byte/signal budgets, so an honest high-throughput connection
/// stays open regardless of how small the reply budget is (issue #518).
#[tokio::test]
async fn relay_traffic_never_charges_the_error_reply_budget() {
    let mut config = base_config();
    config.rate_limit_config.max_inbound_error_replies = 3;
    let server = create_test_server_with_config(config, ProtocolConfig::default()).await;
    let running_server = start_server(server).await;
    let addr = running_server.addr();

    let mut sender = connect(addr).await;
    let mut receiver = connect(addr).await;
    authenticate(&mut sender).await;
    authenticate(&mut receiver).await;
    join(&mut sender, "RelaySender").await;
    join(&mut receiver, "RelayReceiver").await;

    // Ten times the reply budget of admitted relay frames: not one of them
    // buys a reply to the sender, so none may charge the gate.
    let relay = ClientMessage::GameData {
        class: None,
        key: None,
        data: serde_json::json!({ "tick": 1 }),
    };
    let json = serde_json::to_string(&relay).expect("serialize GameData");
    for _ in 0..10 {
        sender
            .send(Message::Text(json.clone().into()))
            .await
            .expect("send relay frame");
    }

    // The connection is still alive and answers a Ping: the relay flood left
    // the reply budget untouched.
    let probe = Message::Text(
        serde_json::to_string(&ClientMessage::Ping)
            .expect("serialize Ping")
            .into(),
    );
    sender.send(probe).await.expect("send liveness Ping");
    let pong_deadline = tokio::time::Instant::now() + CLOSE_DEADLINE;
    let mut saw_pong = false;
    while tokio::time::Instant::now() < pong_deadline {
        match tokio::time::timeout(
            pong_deadline.saturating_duration_since(tokio::time::Instant::now()),
            sender.next(),
        )
        .await
        {
            Ok(Some(Ok(Message::Text(text)))) => {
                if matches!(
                    serde_json::from_str::<ServerMessage>(&text),
                    Ok(ServerMessage::Pong)
                ) {
                    saw_pong = true;
                    break;
                }
            }
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(error))) => panic!("transport error after relay flood: {error}"),
            Ok(None) => panic!("relay flood must not close the connection"),
            Err(_elapsed) => break,
        }
    }
    assert!(
        saw_pong,
        "an honest relay connection must never touch the reply budget"
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

        password: None,
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

        password: None,
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

        password: None,
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

/// An authority-initiated `KickPlayer` room operation closes the kicked
/// seat with the dedicated `4007 kicked` code (issue #525). The kicked
/// connection first receives a best-effort farewell `Error` frame carrying
/// `KICKED`; the remaining members observe the ordinary `PlayerLeft` roster
/// delta; the authority receives the correlated `PlayerKicked` result.
#[tokio::test]
async fn authority_kick_closes_target_with_4007() {
    use signal_fish_server::protocol::{
        RoomOperationId, RoomOperationRequest, RoomOperationResult,
    };

    let server = create_test_server_with_config(base_config(), ProtocolConfig::default()).await;
    let running = start_server(server).await;
    let url = format!("ws://{}/ws", running.addr());

    async fn connect_v3(url: &str, request_operation_ids: bool) -> WsStream {
        let (mut ws, _) = tokio::time::timeout(CLOSE_DEADLINE, connect_async(url))
            .await
            .expect("connect timed out")
            .expect("connect failed");
        let auth = ClientMessage::Authenticate {
            app_id: "close-code-test".to_string(),
            sdk_version: None,
            platform: None,
            game_data_format: None,
            protocol_version: Some(3),
            supported_transports: None,
            supported_topologies: None,
            requested_capabilities: request_operation_ids
                .then(|| vec!["room_operation_ids".to_string()]),
        };
        let json = serde_json::to_string(&auth).expect("serialize Authenticate");
        ws.send(Message::Text(json.into()))
            .await
            .expect("send Authenticate");
        ws
    }

    async fn join_authority_room(ws: &mut WsStream, player_name: &str) -> Box<RoomJoinedPayload> {
        let join = ClientMessage::JoinRoom {
            game_name: "close_code_game".to_string(),
            room_code: Some("KICKED".to_string()),
            player_name: player_name.to_string(),
            max_players: Some(4),
            supports_authority: Some(true),
            relay_transport: None,

            password: None,
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

    let mut authority = connect_v3(&url, true).await;
    let mut target = connect_v3(&url, false).await;
    let authority_payload = join_authority_room(&mut authority, "host").await;
    let target_payload = join_authority_room(&mut target, "guest").await;
    assert!(
        authority_payload.is_authority,
        "the first joiner must hold authority"
    );

    // The authority kicks the target by player id.
    let kick = ClientMessage::RoomOperation {
        operation_id: RoomOperationId::new_v4(),
        operation: Box::new(RoomOperationRequest::KickPlayer {
            player_id: target_payload.player_id,
        }),
    };
    let json = serde_json::to_string(&kick).expect("serialize KickPlayer");
    authority
        .send(Message::Text(json.into()))
        .await
        .expect("send KickPlayer");

    // The kicked seat sees the farewell error and then the 4007 close frame.
    let mut saw_farewell = false;
    let (code, reason): (u16, String) = loop {
        let frame = tokio::time::timeout(CLOSE_DEADLINE, target.next())
            .await
            .expect("timed out waiting for the kicked close")
            .expect("kicked connection closed with no close frame")
            .expect("transport error on the kicked connection");
        if let Message::Text(text) = &frame {
            let message: ServerMessage = serde_json::from_str(text).expect("valid ServerMessage");
            if let ServerMessage::Error { error_code, .. } = message {
                assert_eq!(
                    error_code,
                    Some(signal_fish_server::protocol::ErrorCode::Kicked),
                    "the kicked seat must receive the KICKED farewell"
                );
                saw_farewell = true;
            }
            continue;
        }
        if let Message::Close(Some(frame)) = frame {
            break (frame.code.into(), frame.reason.to_string());
        }
    };
    assert!(
        saw_farewell,
        "the farewell Error frame must be flushed before the close"
    );
    assert_eq!(code, 4007, "a kicked seat must close with 4007");
    assert_eq!(reason, "kicked");

    // The authority receives the correlated success result.
    loop {
        let frame = tokio::time::timeout(CLOSE_DEADLINE, authority.next())
            .await
            .expect("timed out waiting for the kick result")
            .expect("authority connection closed early")
            .expect("transport error on the authority connection");
        let Message::Text(text) = frame else { continue };
        let message: ServerMessage = serde_json::from_str(&text).expect("valid ServerMessage");
        match message {
            ServerMessage::RoomOperationResult { result, .. } => {
                assert!(
                    matches!(result.as_ref(), RoomOperationResult::PlayerKicked { player_id }
                        if *player_id == target_payload.player_id),
                    "expected PlayerKicked for the target, got {result:?}"
                );
                break;
            }
            ServerMessage::PlayerLeft { player_id, .. } => {
                assert_eq!(player_id, target_payload.player_id);
            }
            _ => continue,
        }
    }

    running.shutdown().await;
}
