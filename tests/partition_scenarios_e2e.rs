//! P10.C directional-partition experiments over real WebSockets.
//!
//! A connection being "up" is not a single fact under an asymmetric network
//! fault. These tests pin which side can still make progress, the semantic
//! close that makes the failure loud, and the room's recovery after eviction.

mod test_helpers;
mod websocket_test_helpers;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::config::{ProtocolConfig, WebSocketConfig};
use signal_fish_server::protocol::{ClientMessage, PlayerId, RoomJoinedPayload, ServerMessage};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::create_router;
use test_helpers::{create_test_server_with_config, RunningTestServer};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::chaos_proxy::{ChaosProxy, Direction};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const EVENT_DEADLINE: tokio::time::Duration = tokio::time::Duration::from_secs(30);
const GAME: &str = "partition_scenarios";

#[derive(Debug, Default)]
struct CloseObservation {
    code: Option<u16>,
    reason: Option<String>,
    saw_application_pong: bool,
    #[cfg(windows)]
    transport_reset_after_cause: bool,
}

async fn start_server(
    ping_interval_secs: u64,
    pong_timeout_secs: u64,
    slow_consumer_timeout_ms: u64,
) -> (RunningTestServer, Arc<EnhancedGameServer>) {
    let config = ServerConfig {
        // Keep the legacy activity reaper out of these E4 transport-probe
        // experiments. The socket-level Ping/Pong task owns 4003 here.
        ping_timeout: std::time::Duration::from_secs(600),
        heartbeat_throttle: std::time::Duration::ZERO,
        websocket_config: WebSocketConfig {
            send_queue_capacity: 8,
            slow_consumer_timeout_ms,
            server_ping_interval_secs: ping_interval_secs,
            pong_timeout_secs,
            ..WebSocketConfig::default()
        },
        ..ServerConfig::default()
    };
    let server = create_test_server_with_config(config, ProtocolConfig::default()).await;
    let router = create_router("http://localhost:3000").with_state(server.clone());
    let running = RunningTestServer::spawn(server.clone(), router).await;
    (running, server)
}

async fn connect(addr: std::net::SocketAddr) -> WsStream {
    let url = format!("ws://{addr}/ws");
    let (ws, _) = tokio::time::timeout(EVENT_DEADLINE, connect_async(url))
        .await
        .expect("websocket connect timed out")
        .expect("websocket connect failed");
    ws
}

async fn join_room(ws: &mut WsStream, room: &str, player_name: &str) -> Box<RoomJoinedPayload> {
    let join = ClientMessage::JoinRoom {
        game_name: GAME.to_string(),
        room_code: Some(room.to_string()),
        player_name: player_name.to_string(),
        max_players: Some(4),
        supports_authority: Some(false),
        relay_transport: None,
    };
    send_client_message(ws, &join).await;

    loop {
        match read_server_message(ws, "joining room").await {
            ServerMessage::RoomJoined(payload) => return payload,
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                panic!("room join failed for {player_name}: {reason} ({error_code:?})")
            }
            _ => {}
        }
    }
}

async fn send_client_message(ws: &mut WsStream, message: &ClientMessage) {
    let json = serde_json::to_string(message).expect("serialize client message");
    ws.send(Message::Text(json.into()))
        .await
        .expect("send client message");
}

async fn read_server_message(ws: &mut WsStream, context: &str) -> ServerMessage {
    loop {
        let frame = tokio::time::timeout(EVENT_DEADLINE, ws.next())
            .await
            .unwrap_or_else(|_| panic!("{context}: timed out waiting for server message"))
            .unwrap_or_else(|| panic!("{context}: connection closed"))
            .unwrap_or_else(|error| panic!("{context}: websocket read failed: {error}"));
        match frame {
            Message::Ping(payload) => ws
                .send(Message::Pong(payload))
                .await
                .unwrap_or_else(|error| panic!("{context}: failed to answer server Ping: {error}")),
            Message::Text(text) => {
                return serde_json::from_str(&text)
                    .unwrap_or_else(|error| panic!("{context}: invalid server message: {error}"));
            }
            _ => {}
        }
    }
}

async fn poll_until(context: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + EVENT_DEADLINE;
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{context}: condition never held within {EVENT_DEADLINE:?}"
        );
        tokio::task::yield_now().await;
    }
}

async fn wait_for_player_left(ws: &mut WsStream, departed: PlayerId, context: &str) {
    loop {
        match read_server_message(ws, context).await {
            ServerMessage::PlayerLeft { player_id, .. } if player_id == departed => return,
            ServerMessage::Error {
                message,
                error_code,
            } => panic!("{context}: unexpected server error: {message} ({error_code:?})"),
            _ => {}
        }
    }
}

async fn wait_for_ping_timeout_while_servicing_healthy(
    server: &EnhancedGameServer,
    healthy: &mut WsStream,
    departed: PlayerId,
) -> bool {
    let deadline = tokio::time::Instant::now() + EVENT_DEADLINE;
    let mut saw_player_left = false;
    loop {
        let timeout_count = server
            .metrics()
            .websocket_ping_timeouts
            .load(Ordering::Relaxed);
        if timeout_count == 1 {
            return saw_player_left;
        }
        assert_eq!(timeout_count, 0, "partition timed out more than once");
        assert!(
            tokio::time::Instant::now() < deadline,
            "partition never produced its expected Ping timeout"
        );

        match tokio::time::timeout(tokio::time::Duration::from_millis(50), healthy.next()).await {
            Ok(Some(Ok(Message::Ping(payload)))) => healthy
                .send(Message::Pong(payload))
                .await
                .expect("healthy peer sends matching Pong"),
            Ok(Some(Ok(Message::Text(text)))) => {
                let message: ServerMessage =
                    serde_json::from_str(&text).expect("decode healthy peer server message");
                if matches!(
                    message,
                    ServerMessage::PlayerLeft { player_id, .. } if player_id == departed
                ) {
                    saw_player_left = true;
                }
            }
            Ok(Some(Ok(_))) | Err(_) => {}
            Ok(Some(Err(error))) => panic!("healthy peer websocket failed: {error}"),
            Ok(None) => panic!("healthy peer closed during another client's partition"),
        }
    }
}

async fn wait_for_marker(ws: &mut WsStream, marker: &str) {
    loop {
        if let ServerMessage::GameData { data, .. } =
            read_server_message(ws, "waiting for traffic across one-way partition").await
        {
            if data.get("marker").and_then(serde_json::Value::as_str) == Some(marker) {
                return;
            }
        }
    }
}

async fn service_healthy_frame(
    healthy: &mut WsStream,
    frame: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
    context: &str,
) {
    match frame {
        Some(Ok(Message::Ping(payload))) => healthy
            .send(Message::Pong(payload))
            .await
            .unwrap_or_else(|error| panic!("{context}: failed to answer healthy Ping: {error}")),
        Some(Ok(Message::Close(frame))) => {
            panic!("{context}: healthy peer closed during recovery: {frame:?}")
        }
        Some(Ok(_)) => {}
        Some(Err(error)) => panic!("{context}: healthy peer websocket failed: {error}"),
        None => panic!("{context}: healthy peer ended during recovery"),
    }
}

async fn complete_while_servicing_healthy<T>(
    healthy: &mut WsStream,
    operation: impl std::future::Future<Output = T>,
    context: &str,
) -> T {
    tokio::pin!(operation);
    loop {
        tokio::select! {
            // If recovery and a socket frame become ready in the same poll,
            // consume the frame first. In particular, never return while a
            // ready protocol Ping is still waiting for its Pong.
            biased;
            frame = healthy.next() => service_healthy_frame(healthy, frame, context).await,
            output = &mut operation => return output,
        }
    }
}

async fn observe_until_close(ws: &mut WsStream) -> CloseObservation {
    let deadline = tokio::time::Instant::now() + EVENT_DEADLINE;
    let mut observation = CloseObservation::default();
    loop {
        let next = tokio::time::timeout_at(deadline, ws.next())
            .await
            .expect("timed out waiting for semantic partition close")
            .expect("partitioned connection ended without a close frame");
        let frame = match next {
            Ok(frame) => frame,
            #[cfg(windows)]
            Err(tokio_tungstenite::tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::ConnectionReset =>
            {
                // ChaosProxy deliberately tears down both pumps when either
                // half observes EOF. Windows may surface WSAECONNRESET before
                // tungstenite exposes the already-forwarded close frame (the
                // same platform behavior covered by server_ping_e2e). Every
                // caller has already observed the authoritative server cause
                // metric before resuming the paused direction.
                observation.transport_reset_after_cause = true;
                return observation;
            }
            Err(error) => panic!("partitioned websocket failed before its close frame: {error}"),
        };
        match frame {
            Message::Close(Some(frame)) => {
                observation.code = Some(frame.code.into());
                observation.reason = Some(frame.reason.to_string());
                return observation;
            }
            Message::Close(None) => panic!("partitioned connection received a bare close frame"),
            Message::Text(text) => {
                let message: ServerMessage =
                    serde_json::from_str(&text).expect("decode partition server message");
                if matches!(message, ServerMessage::Pong) {
                    observation.saw_application_pong = true;
                }
            }
            _ => {}
        }
    }
}

async fn assert_room_healed(
    healthy: &mut WsStream,
    healthy_id: PlayerId,
    departed_id: PlayerId,
    server_addr: std::net::SocketAddr,
    room: &str,
    departed_already_observed: bool,
) {
    if !departed_already_observed {
        wait_for_player_left(
            healthy,
            departed_id,
            "waiting for partitioned peer to leave",
        )
        .await;
    }

    let replacement = connect(server_addr);
    let mut replacement =
        complete_while_servicing_healthy(healthy, replacement, "connecting replacement").await;
    let replacement_join = join_room(&mut replacement, room, "Replacement");
    let snapshot = complete_while_servicing_healthy(
        healthy,
        replacement_join,
        "joining replacement to healed room",
    )
    .await;
    assert!(
        snapshot
            .current_players
            .iter()
            .any(|player| player.id == healthy_id),
        "replacement snapshot must retain the healthy room member"
    );

    let marker = format!("healed-{departed_id}");
    send_client_message(
        healthy,
        &ClientMessage::GameData {
            data: serde_json::json!({ "marker": marker }),
            class: None,
            key: None,
        },
    )
    .await;
    let relay_proof = async {
        loop {
            if let ServerMessage::GameData {
                from_player, data, ..
            } = read_server_message(&mut replacement, "proving healed room relay").await
            {
                if from_player == healthy_id
                    && data.get("marker").and_then(serde_json::Value::as_str)
                        == Some(marker.as_str())
                {
                    break;
                }
            }
        }
    };
    complete_while_servicing_healthy(healthy, relay_proof, "proving healed room relay").await;
}

fn assert_close(observation: &CloseObservation, code: u16, reason: &str) {
    #[cfg(windows)]
    if observation.transport_reset_after_cause {
        assert_eq!(observation.code, None);
        assert_eq!(observation.reason, None);
        return;
    }
    assert_eq!(
        observation.code,
        Some(code),
        "partition close must use semantic code {code}: {observation:?}"
    );
    assert_eq!(observation.reason.as_deref(), Some(reason));
}

fn assert_application_pong_or_windows_reset(observation: &CloseObservation) {
    #[cfg(windows)]
    assert!(
        observation.saw_application_pong || observation.transport_reset_after_cause,
        "the processed application Ping must either yield its Pong or hit the documented \
         Windows teardown reset: {observation:?}"
    );
    #[cfg(not(windows))]
    assert!(
        observation.saw_application_pong,
        "a Pong queued before eviction must prove client-to-server traffic stayed live"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn symmetric_blackhole_closes_4003_and_room_heals() {
    let (running, server) = start_server(1, 1, 500).await;
    let proxy = ChaosProxy::spawn(running.addr()).await;
    let mut healthy = connect(running.addr()).await;
    let mut partitioned = connect(proxy.addr()).await;
    let healthy_id = join_room(&mut healthy, "CAPSYM", "Healthy").await.player_id;
    let partitioned_id = join_room(&mut partitioned, "CAPSYM", "Partitioned")
        .await
        .player_id;

    proxy.pause(Direction::ClientToServer);
    proxy.pause(Direction::ServerToClient);
    let saw_player_left =
        wait_for_ping_timeout_while_servicing_healthy(&server, &mut healthy, partitioned_id).await;
    proxy.resume(Direction::ServerToClient);
    proxy.resume(Direction::ClientToServer);

    let observation = observe_until_close(&mut partitioned).await;
    assert_close(&observation, 4003, "activity_timeout");
    assert_eq!(
        server
            .metrics()
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        0,
        "an idle symmetric blackhole must be a liveness timeout, not queue pressure"
    );
    assert_room_healed(
        &mut healthy,
        healthy_id,
        partitioned_id,
        running.addr(),
        "CAPSYM",
        saw_player_left,
    )
    .await;
    running.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn server_to_client_blackhole_closes_4002_while_inbound_ping_is_live() {
    // Put the first socket-level probe beyond this short pressure experiment:
    // this cell isolates reliable delivery's response to a one-way receive
    // outage. The symmetric/client->server cells exercise the E4 probe path.
    let (running, server) = start_server(30, 5, 300).await;
    let proxy = ChaosProxy::spawn(running.addr()).await;
    let mut sender = connect(running.addr()).await;
    let mut partitioned = connect(proxy.addr()).await;
    let sender_id = join_room(&mut sender, "CAPS2C", "Sender").await.player_id;
    let partitioned_id = join_room(&mut partitioned, "CAPS2C", "Partitioned")
        .await
        .player_id;

    proxy.pause(Direction::ServerToClient);
    let heartbeat_before = server.metrics().heartbeat_updates.load(Ordering::Relaxed);
    send_client_message(&mut partitioned, &ClientMessage::Ping).await;
    poll_until("client Ping crosses the server-to-client blackhole", || {
        server.metrics().heartbeat_updates.load(Ordering::Relaxed) > heartbeat_before
    })
    .await;

    let padding = "x".repeat(32 * 1_024);
    let pressure = async {
        while server
            .metrics()
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed)
            == 0
        {
            send_client_message(
                &mut sender,
                &ClientMessage::GameData {
                    data: serde_json::json!({ "padding": padding.as_str() }),
                    class: None,
                    key: None,
                },
            )
            .await;
        }
    };
    tokio::time::timeout(EVENT_DEADLINE, pressure)
        .await
        .expect("server-to-client pressure never evicted the blocked receiver");
    proxy.resume(Direction::ServerToClient);

    let observation = observe_until_close(&mut partitioned).await;
    assert_close(&observation, 4002, "slow_consumer");
    assert_application_pong_or_windows_reset(&observation);
    assert_eq!(
        server
            .metrics()
            .websocket_ping_timeouts
            .load(Ordering::Relaxed),
        0,
        "the isolated receive outage must be attributed to reliable queue pressure"
    );
    assert_room_healed(
        &mut sender,
        sender_id,
        partitioned_id,
        running.addr(),
        "CAPS2C",
        false,
    )
    .await;
    running.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn client_to_server_blackhole_receives_data_then_closes_4003() {
    let (running, server) = start_server(1, 1, 500).await;
    let proxy = ChaosProxy::spawn(running.addr()).await;
    let mut sender = connect(running.addr()).await;
    let mut partitioned = connect(proxy.addr()).await;
    let sender_id = join_room(&mut sender, "CAPC2S", "Sender").await.player_id;
    let partitioned_id = join_room(&mut partitioned, "CAPC2S", "Partitioned")
        .await
        .player_id;

    proxy.pause(Direction::ClientToServer);
    send_client_message(
        &mut sender,
        &ClientMessage::GameData {
            data: serde_json::json!({ "marker": "received-during-client-blackhole" }),
            class: None,
            key: None,
        },
    )
    .await;

    wait_for_marker(&mut partitioned, "received-during-client-blackhole").await;
    let saw_player_left =
        wait_for_ping_timeout_while_servicing_healthy(&server, &mut sender, partitioned_id).await;
    proxy.resume(Direction::ClientToServer);

    let observation = observe_until_close(&mut partitioned).await;
    assert_close(&observation, 4003, "activity_timeout");
    assert_eq!(
        server
            .metrics()
            .websocket_ping_timeouts
            .load(Ordering::Relaxed),
        1,
        "the unreachable automatic Pong must time out exactly once"
    );
    assert_eq!(
        server
            .metrics()
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        0,
        "a client-to-server outage must not masquerade as a slow reader"
    );
    assert_room_healed(
        &mut sender,
        sender_id,
        partitioned_id,
        running.addr(),
        "CAPC2S",
        saw_player_left,
    )
    .await;
    running.shutdown().await;
}
