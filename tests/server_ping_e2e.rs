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

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    loop {
        let snapshot = server.metrics().snapshot().await;
        if snapshot.connections.websocket_ping_rtt.sample_count == 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "matching Pong never produced an RTT sample"
        );
        tokio::task::yield_now().await;
    }

    running.shutdown().await;
}

#[tokio::test]
async fn missing_pong_closes_with_activity_timeout() {
    let (running, server) = start_server(1, 1).await;
    let proxy = ChaosProxy::spawn(running.addr()).await;
    let mut ws = connect(proxy.addr()).await;
    // The first server nonce is 1. A stale/unsolicited Pong sent before the
    // corresponding Ping must not satisfy the later probe.
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
            .expect("timed out proving unsolicited Pong reached the server")
            .expect("connection closed while proving unsolicited Pong ordering")
            .expect("websocket read failed while proving unsolicited Pong ordering");
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
        let frame = tokio::time::timeout_at(deadline, ws.next())
            .await
            .expect("timed out waiting for missed-Pong close")
            .expect("connection ended without a close frame")
            .expect("websocket read failed before close frame");
        if let Message::Close(Some(frame)) = frame {
            assert_eq!(u16::from(frame.code), 4003);
            assert_eq!(frame.reason, "activity_timeout");
            assert_eq!(
                server
                    .metrics()
                    .websocket_ping_timeouts
                    .load(std::sync::atomic::Ordering::Relaxed),
                1,
                "missed Pong must increment the timeout counter exactly once"
            );
            break;
        }
    }

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
