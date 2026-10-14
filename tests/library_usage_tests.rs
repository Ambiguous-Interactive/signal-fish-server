#[path = "../examples/embedded_server.rs"]
mod embedded_server;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::{config::Config, protocol::ServerMessage};
use std::time::Duration;
use tokio_tungstenite::{connect_async, tungstenite::Message};

fn starter_config() -> Config {
    let mut cfg = Config::default();
    cfg.security.enforce_app_id_allowlist = false;
    cfg.security.metrics_auth_token = Some("library-starter-test-metrics-token".into());
    cfg
}

#[test]
fn documented_starter_matches_compiled_example() {
    let guide = include_str!("../docs/library-usage.md");
    let starter = guide
        .split("## Basic Embedded Server")
        .nth(1)
        .expect("starter section")
        .split("```rust\n")
        .nth(1)
        .expect("starter Rust block")
        .split("```")
        .next()
        .expect("starter code");
    assert_eq!(
        starter.trim(),
        include_str!("../examples/embedded_server.rs").trim()
    );
}

#[tokio::test]
async fn starter_rejects_invalid_configuration_before_startup() {
    let mut cfg = starter_config();
    cfg.server.max_rooms = 0;
    assert!(embedded_server::build_server(&cfg).await.is_err());
}

#[tokio::test]
async fn starter_maps_loaded_limits_and_drains_live_connection() {
    let mut cfg = starter_config();
    cfg.server.drain_grace_secs = 0;
    cfg.server.default_max_spectators = Some(7);
    cfg.security.max_signal_bytes = 1234;
    cfg.security.max_connection_info_bytes = 2345;
    cfg.rate_limit.max_relay_bytes = 3456;
    cfg.rate_limit.max_room_relay_bytes = 4567;
    let server = embedded_server::build_server(&cfg)
        .await
        .expect("build starter");
    assert_eq!(server.config().drain_grace, Duration::ZERO);
    assert_eq!(server.config().default_max_spectators, Some(7));
    assert_eq!(server.config().max_signal_bytes, 1234);
    assert_eq!(server.config().max_connection_info_bytes, 2345);
    assert_eq!(server.config().rate_limit_config.max_relay_bytes, 3456);
    assert_eq!(server.config().rate_limit_config.max_room_relay_bytes, 4567);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("address");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let running_server = server.clone();
    let task = tokio::spawn(async move {
        embedded_server::serve_until(listener, running_server, "*", async {
            shutdown_rx.await.expect("shutdown trigger");
        })
        .await
    });
    let lifecycle_result = tokio::time::timeout(Duration::from_secs(10), async {
        let (mut socket, _) = connect_async(format!("ws://{addr}/ws"))
            .await
            .expect("connect");
        socket
            .send(Message::Text(
                serde_json::json!({
                    "type": "Authenticate",
                    "data": {"app_id": "00000000-0000-4000-8000-000000000951", "protocol_version": 3}
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("authenticate");
        for expected in ["Authenticated", "ProtocolInfo"] {
            let frame = socket
                .next()
                .await
                .expect("handshake frame")
                .expect("handshake transport");
            let message: serde_json::Value =
                serde_json::from_str(frame.to_text().expect("text")).expect("JSON");
            assert_eq!(message["type"], expected, "handshake: {message}");
        }
        shutdown_tx.send(()).expect("request shutdown");
        let advisory = socket
            .next()
            .await
            .expect("advisory")
            .expect("advisory transport");
        let message: ServerMessage =
            serde_json::from_str(advisory.to_text().expect("advisory text"))
                .expect("advisory JSON");
        assert!(
            matches!(message, ServerMessage::GoingAway { .. }),
            "advisory: {message:?}"
        );
        let close = socket
            .next()
            .await
            .expect("close")
            .expect("close transport");
        match close {
            Message::Close(Some(frame)) => {
                assert_eq!(u16::from(frame.code), 4000);
                assert_eq!(frame.reason, "server_shutdown");
            }
            other => panic!("expected coded shutdown close, got {other:?}"),
        }
        socket.flush().await.expect("close acknowledgement");
        task.await.expect("server task").expect("serve result");
    })
    .await;
    lifecycle_result.expect("starter startup and drain must finish");
    assert!(server.is_draining());
    assert_eq!(
        server.wait_for_shutdown_connections(Duration::ZERO).await,
        0
    );
}

#[tokio::test]
async fn starter_shutdown_without_clients_finishes() {
    let server = embedded_server::build_server(&starter_config())
        .await
        .expect("build starter");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    tokio::time::timeout(
        Duration::from_secs(10),
        embedded_server::serve_until(listener, server.clone(), "*", std::future::ready(())),
    )
    .await
    .expect("idle drain deadline")
    .expect("serve result");
    assert!(server.is_draining());
}
