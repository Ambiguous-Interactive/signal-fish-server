//! Server-initiated RFC 6455 ping/pong liveness checks.

mod test_helpers;
mod websocket_test_helpers;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::config::{ProtocolConfig, WebSocketConfig};
use signal_fish_server::protocol::{ClientMessage, ServerMessage};
use signal_fish_server::server::ServerConfig;
use signal_fish_server::websocket::create_router;
use test_helpers::{create_test_server_with_config, RunningTestServer};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::chaos_proxy::{ChaosProxy, Direction};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(addr: std::net::SocketAddr) -> WsStream {
    let url = format!("ws://{addr}/ws");
    let (ws, _) = tokio::time::timeout(tokio::time::Duration::from_secs(10), connect_async(url))
        .await
        .expect("websocket connect timed out")
        .expect("websocket connect failed");
    ws
}

async fn start_server(
    ping_interval_secs: u64,
    pong_timeout_secs: u64,
) -> (
    RunningTestServer,
    std::sync::Arc<signal_fish_server::server::EnhancedGameServer>,
) {
    let config = ServerConfig {
        app_id_allowlist_enabled: false,
        ping_timeout: std::time::Duration::from_secs(600),
        websocket_config: WebSocketConfig {
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

async fn join_room(ws: &mut WsStream, room: &str, name: &str) {
    ws.send(Message::Text(
        serde_json::to_string(&ClientMessage::JoinRoom {
            game_name: "outbound-progress".to_string(),
            room_code: Some(room.to_string()),
            player_name: name.to_string(),
            max_players: Some(2),
            supports_authority: Some(true),
            relay_transport: None,
        })
        .expect("serialize JoinRoom")
        .into(),
    ))
    .await
    .expect("send JoinRoom");
    loop {
        let frame = tokio::time::timeout(tokio::time::Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for RoomJoined")
            .expect("connection closed while joining")
            .expect("websocket failed while joining");
        if let Message::Text(text) = frame {
            let message: ServerMessage =
                serde_json::from_str(&text).expect("decode server message while joining");
            if matches!(message, ServerMessage::RoomJoined(_)) {
                return;
            }
        }
    }
}

#[tokio::test]
async fn matching_pong_keeps_connection_alive_and_records_rtt() {
    let (running, server) = start_server(1, 2).await;
    let mut ws = connect(running.addr()).await;

    let payload = loop {
        let frame = tokio::time::timeout(tokio::time::Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for server Ping")
            .expect("connection closed before server Ping")
            .expect("websocket read failed before server Ping");
        if let Message::Ping(payload) = frame {
            break payload;
        }
    };
    ws.send(Message::Pong(payload))
        .await
        .expect("send matching Pong");

    let rtt_metric = async {
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        loop {
            let snapshot = server.metrics().snapshot().await;
            if snapshot.connections.websocket_ping_rtt.sample_count >= 1 {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "matching Pong never produced an RTT sample"
            );
            tokio::task::yield_now().await;
        }
    };
    tokio::pin!(rtt_metric);

    loop {
        tokio::select! {
            () = &mut rtt_metric => break,
            frame = ws.next() => {
                let frame = frame
                    .expect("connection closed while waiting for RTT metric")
                    .expect("websocket read failed while waiting for RTT metric");
                if let Message::Ping(payload) = frame {
                    ws.send(Message::Pong(payload))
                        .await
                        .expect("send matching Pong while waiting for RTT metric");
                }
            }
        }
    }

    ws.send(Message::Text(
        serde_json::to_string(&ClientMessage::Ping)
            .expect("serialize application Ping")
            .into(),
    ))
    .await
    .expect("send application Ping after RTT metric");
    let liveness_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    loop {
        let frame = tokio::time::timeout_at(liveness_deadline, ws.next())
            .await
            .expect("timed out proving connection remained alive")
            .expect("connection closed after matching Pong")
            .expect("websocket read failed after matching Pong");
        match frame {
            Message::Ping(payload) => ws
                .send(Message::Pong(payload))
                .await
                .expect("send matching Pong during liveness check"),
            Message::Text(text) => {
                let message: ServerMessage =
                    serde_json::from_str(&text).expect("decode application Pong response");
                if matches!(message, ServerMessage::Pong) {
                    break;
                }
            }
            _ => {}
        }
    }

    running.shutdown().await;
}

#[tokio::test]
async fn missing_pong_closes_with_activity_timeout() {
    let (running, server) = start_server(1, 1).await;
    let proxy = ChaosProxy::spawn(running.addr()).await;
    let mut ws = connect(proxy.addr()).await;
    // An unsolicited guessed Pong sent before the random server probe must not
    // satisfy that later probe.
    ws.send(Message::Pong(1_u64.to_be_bytes().to_vec().into()))
        .await
        .expect("send unsolicited pre-probe Pong");
    ws.send(Message::Text(
        serde_json::to_string(&ClientMessage::Ping)
            .expect("serialize application Ping")
            .into(),
    ))
    .await
    .expect("send application Ping behind unsolicited Pong");
    loop {
        let frame = tokio::time::timeout(tokio::time::Duration::from_secs(1), ws.next())
            .await
            .expect("timed out proving guessed Pong reached the server")
            .expect("connection closed while proving guessed Pong ordering")
            .expect("websocket read failed while proving guessed Pong ordering");
        if let Message::Text(text) = frame {
            let message: ServerMessage =
                serde_json::from_str(&text).expect("decode application Pong response");
            if matches!(message, ServerMessage::Pong) {
                break;
            }
        }
    }
    proxy.pause(Direction::ClientToServer);

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    loop {
        let timeout_count = server
            .metrics()
            .websocket_ping_timeouts
            .load(std::sync::atomic::Ordering::Relaxed);
        if timeout_count == 1 {
            break;
        }
        assert_eq!(timeout_count, 0, "missed Pong was counted more than once");
        assert!(
            tokio::time::Instant::now() < deadline,
            "guessed Pong incorrectly satisfied the server probe"
        );
        tokio::task::yield_now().await;
    }

    // The timeout is now authoritative. Resume the client direction before
    // reading the close so tungstenite can flush its queued automatic Pong.
    // Windows can still turn this teardown into WSAECONNRESET before
    // tungstenite exposes the already-forwarded semantic close frame, so that
    // platform accepts only that specific transport error after the server's
    // timeout metric has proved the close cause.
    proxy.resume(Direction::ClientToServer);

    let close_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    loop {
        let next = tokio::time::timeout_at(close_deadline, ws.next())
            .await
            .expect("timed out waiting for missed-Pong close")
            .expect("connection ended without a close frame");
        let frame = match next {
            Ok(frame) => frame,
            #[cfg(windows)]
            Err(tokio_tungstenite::tungstenite::Error::Io(error))
                if error.kind() == std::io::ErrorKind::ConnectionReset =>
            {
                break;
            }
            Err(error) => panic!("websocket read failed before close frame: {error}"),
        };
        if let Message::Close(Some(frame)) = frame {
            assert_eq!(u16::from(frame.code), 4003);
            assert_eq!(frame.reason, "activity_timeout");
            break;
        }
    }

    assert_eq!(
        server
            .metrics()
            .websocket_ping_timeouts
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "missed Pong must increment the timeout counter exactly once"
    );

    running.shutdown().await;
}

#[tokio::test]
async fn inbound_activity_skips_probes_until_the_connection_becomes_idle() {
    let (running, server) = start_server(1, 1).await;
    let ws = connect(running.addr()).await;
    let (mut sink, mut stream) = ws.split();
    let mut traffic = tokio::time::interval(tokio::time::Duration::from_millis(200));
    let active_until = tokio::time::Instant::now() + tokio::time::Duration::from_millis(2_500);

    while tokio::time::Instant::now() < active_until {
        tokio::select! {
            _ = traffic.tick() => {
                sink.send(Message::Text(
                    serde_json::to_string(&ClientMessage::Ping)
                        .expect("serialize application Ping")
                        .into(),
                ))
                .await
                .expect("send active application traffic");
            }
            frame = stream.next() => {
                let frame = frame
                    .expect("connection closed during active traffic")
                    .expect("websocket read failed during active traffic");
                assert!(
                    !matches!(frame, Message::Ping(_)),
                    "an active connection received an unnecessary liveness probe"
                );
            }
        }
    }

    let payload = loop {
        let frame = tokio::time::timeout(tokio::time::Duration::from_secs(3), stream.next())
            .await
            .expect("idle connection never received a liveness probe")
            .expect("connection closed before idle liveness probe")
            .expect("websocket read failed before idle liveness probe");
        if let Message::Ping(payload) = frame {
            break payload;
        }
    };
    sink.send(Message::Pong(payload))
        .await
        .expect("answer idle liveness probe");
    assert!(
        server
            .metrics()
            .websocket_ping_probes_skipped_activity
            .load(std::sync::atomic::Ordering::Relaxed)
            >= 2,
        "active inbound traffic did not skip scheduled probes"
    );
    assert_eq!(
        server
            .metrics()
            .websocket_ping_timeouts
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "active inbound traffic caused a false liveness timeout"
    );

    running.shutdown().await;
}

#[tokio::test]
async fn non_pong_activity_cancels_an_outstanding_probe() {
    let (running, server) = start_server(1, 2).await;
    let mut ws = connect(running.addr()).await;

    // Do not read the server Ping: tungstenite would otherwise queue its
    // automatic Pong. Wait until the first probe is outstanding, then prove a
    // separate decoded application frame cancels it.
    tokio::time::sleep(tokio::time::Duration::from_millis(1_200)).await;
    ws.send(Message::Text(
        serde_json::to_string(&ClientMessage::Ping)
            .expect("serialize application Ping")
            .into(),
    ))
    .await
    .expect("send post-probe application activity");

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(2);
    loop {
        if server
            .metrics()
            .websocket_ping_probes_cancelled_activity
            .load(std::sync::atomic::Ordering::Relaxed)
            == 1
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "post-probe activity never cancelled the liveness probe"
        );
        tokio::task::yield_now().await;
    }
    let survive_until = tokio::time::Instant::now() + tokio::time::Duration::from_millis(2_200);
    let mut traffic = tokio::time::interval(tokio::time::Duration::from_millis(200));
    let mut saw_application_pong = false;
    while tokio::time::Instant::now() < survive_until {
        tokio::select! {
            _ = traffic.tick() => {
                ws.send(Message::Text(
                    serde_json::to_string(&ClientMessage::Ping)
                        .expect("serialize follow-up application Ping")
                        .into(),
                ))
                .await
                .expect("connection died after probe cancellation");
            }
            frame = ws.next() => {
                let frame = frame
                    .expect("connection closed after probe cancellation")
                    .expect("websocket read failed after probe cancellation");
                if let Message::Text(text) = frame {
                    let message: ServerMessage = serde_json::from_str(&text)
                        .expect("decode post-cancellation application response");
                    saw_application_pong |= matches!(message, ServerMessage::Pong);
                }
            }
        }
    }
    assert!(
        saw_application_pong,
        "connection did not process application traffic beyond the old probe deadline"
    );
    assert_eq!(
        server
            .metrics()
            .websocket_ping_timeouts
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );

    let payload = loop {
        let frame = tokio::time::timeout(tokio::time::Duration::from_secs(3), ws.next())
            .await
            .expect("idle connection never received a later probe")
            .expect("connection closed before later probe")
            .expect("websocket read failed before later probe");
        if let Message::Ping(payload) = frame {
            break payload;
        }
    };
    ws.send(Message::Pong(payload))
        .await
        .expect("answer later idle probe");

    running.shutdown().await;
}

/// Issue #217: successful application writes are reverse-path progress. A
/// recipient steadily accepting relayed data must not be disconnected merely
/// because the probe Pong is queued behind that same data. The Ping itself is
/// still written so a read-only client can refresh inbound activity. Once
/// writes stop, the ordinary deadline must still detect a client that never
/// reads.
#[tokio::test]
async fn outbound_progress_supersedes_deadlines_until_the_connection_stops_draining() {
    let (running, server) = start_server(1, 1).await;
    let mut sender = connect(running.addr()).await;
    let mut recipient = connect(running.addr()).await;
    join_room(&mut sender, "OUT001", "Sender").await;
    join_room(&mut recipient, "OUT001", "Recipient").await;

    // Keep the sender conforming while deliberately never polling the
    // recipient after its join. The recipient therefore cannot answer a Ping,
    // but its server-to-client path is visibly accepting each relay write.
    let (mut sender_sink, mut sender_stream) = sender.split();
    let sender_reader = tokio::spawn(async move {
        while let Some(frame) = sender_stream.next().await {
            frame.expect("sender websocket failed during outbound-progress test");
        }
    });
    let mut cadence = tokio::time::interval(tokio::time::Duration::from_millis(100));
    let active_until = tokio::time::Instant::now() + tokio::time::Duration::from_millis(2_500);
    let mut sequence = 0u64;
    while tokio::time::Instant::now() < active_until {
        cadence.tick().await;
        sender_sink
            .send(Message::Text(
                serde_json::to_string(&ClientMessage::GameData {
                    class: None,
                    key: None,
                    data: serde_json::json!({ "sequence": sequence }),
                })
                .expect("serialize relayed GameData")
                .into(),
            ))
            .await
            .expect("send relayed GameData");
        sequence += 1;
    }

    assert_eq!(
        server
            .metrics()
            .websocket_ping_timeouts
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "successful relay writes must prevent a false Pong timeout"
    );
    assert!(
        server
            .metrics()
            .websocket_ping_probes_cancelled_activity
            .load(std::sync::atomic::Ordering::Relaxed)
            >= 1,
        "outbound progress did not supersede scheduled probe deadlines"
    );

    // Stop writing. With no inbound or outbound evidence left, the next probe
    // must time out: progress supersedes stale deadlines, not idle detection.
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(4);
    loop {
        if server
            .metrics()
            .websocket_ping_timeouts
            .load(std::sync::atomic::Ordering::Relaxed)
            == 1
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "idle recipient was not reclaimed after outbound progress stopped"
        );
        tokio::task::yield_now().await;
    }

    drop(recipient);
    drop(sender_sink);
    sender_reader.abort();
    running.shutdown().await;
}

/// Issue #217: outbound progress that occurs after a Ping write must cancel the
/// old Pong deadline, not merely suppress the next scheduled probe.
#[tokio::test]
async fn outbound_write_cancels_an_outstanding_probe() {
    let (running, server) = start_server(1, 2).await;
    let proxy = ChaosProxy::spawn(running.addr()).await;
    let mut sender = connect(running.addr()).await;
    let mut recipient = connect(proxy.addr()).await;
    join_room(&mut sender, "OUT002", "Sender").await;
    join_room(&mut recipient, "OUT002", "Recipient").await;

    // Observe the exact recipient probe, but prevent tungstenite's automatic
    // Pong from reaching the server. This synchronizes on the active probe
    // instead of guessing its phase from wall-clock time.
    proxy.pause(Direction::ClientToServer);
    let probe_observed_at = loop {
        let frame = tokio::time::timeout(tokio::time::Duration::from_secs(5), recipient.next())
            .await
            .expect("recipient never observed a server Ping")
            .expect("recipient closed before the server Ping")
            .expect("recipient read failed before the server Ping");
        if matches!(frame, Message::Ping(_)) {
            break tokio::time::Instant::now();
        }
    };

    // Cancel any contemporaneous sender-side probe first. The subsequent
    // cancellation delta can then only come from the recipient's relay write.
    sender
        .send(Message::Text(
            serde_json::to_string(&ClientMessage::Ping)
                .expect("serialize sender activity")
                .into(),
        ))
        .await
        .expect("send sender activity");
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    let cancellation_baseline = server
        .metrics()
        .websocket_ping_probes_cancelled_activity
        .load(std::sync::atomic::Ordering::Relaxed);

    sender
        .send(Message::Text(
            serde_json::to_string(&ClientMessage::GameData {
                class: None,
                key: None,
                data: serde_json::json!({ "sequence": 1 }),
            })
            .expect("serialize relayed GameData")
            .into(),
        ))
        .await
        .expect("send relayed GameData");

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(1);
    loop {
        if server
            .metrics()
            .websocket_ping_probes_cancelled_activity
            .load(std::sync::atomic::Ordering::Relaxed)
            > cancellation_baseline
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "outbound relay write did not cancel the outstanding probe"
        );
        tokio::task::yield_now().await;
    }
    assert_eq!(
        server
            .metrics()
            .websocket_ping_timeouts
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "cancelled outbound-progress probe counted as a timeout"
    );

    // Keep application output progressing past the cancelled probe's original
    // two-second deadline. A counter increment alone is not enough: the old
    // deadline must be unable to close the connection after cancellation.
    let survive_until = probe_observed_at + tokio::time::Duration::from_millis(2_200);
    let mut cadence = tokio::time::interval(tokio::time::Duration::from_millis(100));
    let mut sequence = 2_u64;
    while tokio::time::Instant::now() < survive_until {
        cadence.tick().await;
        sender
            .send(Message::Text(
                serde_json::to_string(&ClientMessage::GameData {
                    class: None,
                    key: None,
                    data: serde_json::json!({ "sequence": sequence }),
                })
                .expect("serialize post-cancellation GameData")
                .into(),
            ))
            .await
            .expect("connection died after outbound probe cancellation");
        sequence += 1;
    }
    assert_eq!(
        server
            .metrics()
            .websocket_ping_timeouts
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "connection did not survive the cancelled probe's old deadline"
    );

    proxy.resume(Direction::ClientToServer);
    drop(recipient);
    running.shutdown().await;
}

#[tokio::test]
async fn zero_interval_disables_server_ping_frames() {
    let (running, _server) = start_server(0, 1).await;
    let mut ws = connect(running.addr()).await;

    let result = tokio::time::timeout(tokio::time::Duration::from_millis(1200), ws.next()).await;
    assert!(
        result.is_err(),
        "disabled server pings emitted a frame: {result:?}"
    );

    running.shutdown().await;
}
