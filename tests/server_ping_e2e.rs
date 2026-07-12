//! Server-initiated RFC 6455 ping/pong liveness checks (P10.E4).

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
        auth_enabled: false,
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
