#[path = "../examples/embedded_server.rs"]
mod embedded_server;
mod test_helpers;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::{config::Config, protocol::ServerMessage, websocket};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

fn starter_config() -> Config {
    let mut cfg = Config::default();
    cfg.security.enforce_app_id_allowlist = false;
    cfg.security.metrics_auth_token = Some("library-starter-test-metrics-token".into());
    cfg
}

async fn connect_v3(addr: SocketAddr) -> WebSocketStream<MaybeTlsStream<TcpStream>> {
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
    socket
}

async fn send_http2_preface(stream: &mut TcpStream) {
    stream
        .write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n\x00\x00\x00\x04\x00\x00\x00\x00\x00")
        .await
        .expect("send HTTP/2 preface and SETTINGS");
}

async fn wait_for_http2_frame(stream: &mut TcpStream, frame_type: u8, flags: u8) -> u64 {
    loop {
        let mut header = [0; 9];
        stream.read_exact(&mut header).await.expect("frame header");
        let [high, middle, low, observed_type, observed_flags, ..] = header;
        let length = u64::from(u32::from_be_bytes([0, high, middle, low]));
        let read = tokio::io::copy(&mut (&mut *stream).take(length), &mut tokio::io::sink())
            .await
            .expect("frame body");
        assert_eq!(read, length, "complete HTTP/2 frame");
        if observed_type == frame_type && observed_flags == flags {
            return length;
        }
    }
}

#[tokio::test]
async fn plain_shutdown_releases_listener_before_signaling_accepted_http2() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("address");
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let serving = websocket::serve_with_http_header_deadline(
        listener,
        axum::Router::new().into_make_service_with_connect_info::<SocketAddr>(),
        websocket::HttpServeTimeouts::production(Duration::from_secs(5)),
        shutdown,
    );
    tokio::pin!(serving);
    let mut http2 = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .expect("TCP connect deadline")
        .expect("connect HTTP/2");
    send_http2_preface(&mut http2).await;
    assert!(futures_util::poll!(&mut serving).is_pending());
    let settings = tokio::time::timeout(
        Duration::from_secs(5),
        wait_for_http2_frame(&mut http2, 4, 1),
    )
    .await
    .expect("HTTP/2 task starts while the accept loop is parked");
    assert_eq!(settings, 0, "SETTINGS ACK body");

    stop.send(true).expect("request shutdown");
    // Keep the accept loop unpolled while the accepted task can run.
    let early_goaway = tokio::time::timeout(
        Duration::from_millis(100),
        wait_for_http2_frame(&mut http2, 7, 0),
    )
    .await;
    assert!(
        early_goaway.is_err(),
        "GOAWAY must wait for the accept loop to close the listening socket"
    );

    assert!(futures_util::poll!(&mut serving).is_pending());
    let goaway = tokio::time::timeout(
        Duration::from_secs(5),
        wait_for_http2_frame(&mut http2, 7, 0),
    )
    .await
    .expect("GOAWAY follows shutdown of the listener");
    assert!(goaway >= 8, "GOAWAY body");
    let dial = tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(addr))
        .await
        .expect("loopback connection attempt finishes")
        .expect_err("the old listener must refuse connections after GOAWAY");
    assert_eq!(dial.kind(), std::io::ErrorKind::ConnectionRefused);

    drop(http2);
    tokio::time::timeout(Duration::from_secs(5), serving)
        .await
        .expect("HTTP/2 task finishes after its peer disconnects")
        .expect("HTTP serve result");
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
            Ok(())
        })
        .await
    });
    let lifecycle_result = tokio::time::timeout(Duration::from_secs(10), async {
        let mut socket = connect_v3(addr).await;
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
async fn starter_finishes_an_existing_drain_after_its_original_deadline() {
    let mut cfg = starter_config();
    cfg.server.drain_grace_secs = 30;
    cfg.server.ping_timeout = 120;
    cfg.websocket.server_ping_interval_secs = 0;
    let server = embedded_server::build_server(&cfg)
        .await
        .expect("build starter");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("address");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let running_server = server.clone();
    let task = tokio::spawn(async move {
        embedded_server::serve_until(listener, running_server, "*", async {
            shutdown_rx.await.expect("shutdown trigger");
            Ok(())
        })
        .await
    });
    let mut socket = tokio::time::timeout(Duration::from_secs(5), connect_v3(addr))
        .await
        .expect("authenticate before starting drain");

    // Advance the server's local clock without advancing the advisory clock.
    // Resume before polling the real TCP socket to avoid timer auto-advance.
    tokio::time::pause();
    let drain = server.begin_shutdown_drain();
    assert_eq!(server.announce_shutdown_drain(drain).await, 1);
    tokio::time::advance(drain.grace + Duration::from_secs(1)).await;
    tokio::time::resume();
    shutdown_tx.send(()).expect("request shutdown");

    let drain_result = tokio::time::timeout(Duration::from_secs(5), async {
        let advisory = socket.next().await.expect("advisory").expect("transport");
        let advisory: ServerMessage =
            serde_json::from_str(advisory.to_text().expect("advisory text"))
                .expect("advisory JSON");
        assert!(
            matches!(advisory, ServerMessage::GoingAway { deadline_ms, .. }
            if deadline_ms == drain.deadline_ms)
        );
        let close = socket.next().await.expect("close").expect("transport");
        let Message::Close(Some(frame)) = close else {
            panic!("expected one advisory followed by the coded close, got {close:?}");
        };
        assert_eq!(u16::from(frame.code), 4000);
        assert_eq!(frame.reason, "server_shutdown");
        socket.flush().await.expect("close acknowledgement");
        task.await.expect("server task").expect("serve result");
    })
    .await;
    drain_result.expect("expired grace must not restart when the starter takes over");
}

#[tokio::test]
async fn starter_shutdown_bounds_unresponsive_http2_peer() {
    let mut cfg = starter_config();
    cfg.server.drain_grace_secs = 12;
    cfg.websocket.server_ping_interval_secs = 0;
    let server = embedded_server::build_server(&cfg)
        .await
        .expect("build starter");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("address");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(embedded_server::serve_until(listener, server, "*", async {
        shutdown_rx.await.expect("shutdown trigger");
        Ok(())
    }));

    let startup = tokio::time::timeout(Duration::from_secs(5), async {
        let mut http2 = TcpStream::connect(addr).await.expect("connect HTTP/2");
        send_http2_preface(&mut http2).await;
        assert_eq!(wait_for_http2_frame(&mut http2, 4, 1).await, 0);
        (http2, connect_v3(addr).await)
    })
    .await;
    let (mut http2, mut socket) = startup.expect("both protocol handshakes finish");
    shutdown_tx.send(()).expect("request shutdown");

    let advisory_result = tokio::time::timeout(Duration::from_secs(5), socket.next()).await;
    let advisory = advisory_result
        .expect("shutdown advisory deadline")
        .expect("advisory")
        .expect("transport");
    let advisory: ServerMessage =
        serde_json::from_str(advisory.to_text().expect("advisory text")).expect("advisory JSON");
    assert!(matches!(advisory, ServerMessage::GoingAway { .. }));

    // A settle timer started with the shutdown signal would fire during grace.
    let settle = signal_fish_server::websocket::registered_connection_shutdown_settle_timeout();
    tokio::time::pause();
    tokio::time::advance(settle + Duration::from_secs(1)).await;
    tokio::time::resume();
    let ping = Message::Ping(b"drain grace".as_slice().into());
    let grace_probe = tokio::time::timeout(Duration::from_secs(3), async {
        socket.send(ping).await.expect("ping during shutdown grace");
        socket.next().await.expect("pong").expect("transport")
    })
    .await;
    assert_eq!(
        grace_probe.expect("WebSocket stays live after a full settle period during grace"),
        Message::Pong(b"drain grace".as_slice().into())
    );

    // The existing HTTP/2 task has entered graceful shutdown. Keep it
    // parked while a fresh connection attempts to reach the old listener.
    tokio::time::timeout(
        Duration::from_secs(3),
        wait_for_http2_frame(&mut http2, 7, 0),
    )
    .await
    .expect("the existing HTTP/2 peer receives GOAWAY");
    let reconnect = tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(addr))
        .await
        .expect("loopback connection attempt finishes");
    assert_eq!(
        reconnect.as_ref().err().map(std::io::Error::kind),
        Some(std::io::ErrorKind::ConnectionRefused),
        "the listener must refuse fresh TCP connections while the drain is active: {reconnect:?}"
    );

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(7)).await;
    tokio::time::resume();
    let websocket_drain = tokio::time::timeout(Duration::from_secs(5), async {
        let close = socket.next().await.expect("close").expect("transport");
        let Message::Close(Some(frame)) = close else {
            panic!("expected the coded shutdown close, got {close:?}");
        };
        assert_eq!(u16::from(frame.code), 4000);
        assert_eq!(frame.reason, "server_shutdown");
        socket.flush().await.expect("close acknowledgement");
    })
    .await;
    websocket_drain.expect("WebSocket drain finishes while the HTTP/2 peer stays open");

    // Keep the TCP connection open while the HTTP/2 peer stops responding.
    let served = tokio::time::timeout(settle + Duration::from_secs(2), tasks.join_next()).await;
    let error = served
        .expect("embedded shutdown must finish after the post-drain budget")
        .expect("server task")
        .expect("server task join")
        .expect_err("forced HTTP connection closure must report an error");
    assert_eq!(
        error
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
        Some(std::io::ErrorKind::TimedOut)
    );
    let mut final_frames = Vec::new();
    let closed =
        tokio::time::timeout(Duration::from_secs(2), http2.read_to_end(&mut final_frames)).await;
    closed
        .expect("the HTTP/2 connection closes after the settle period")
        .expect("read HTTP/2 EOF");
    let restart_result = tokio::time::timeout(Duration::from_secs(5), async {
        let restarted_server = embedded_server::build_server(&cfg)
            .await
            .expect("build replacement server");
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .expect("rebind the same address");
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        tasks.spawn(embedded_server::serve_until(
            listener,
            restarted_server,
            "*",
            async {
                stop_rx.await.expect("replacement shutdown trigger");
                Ok(())
            },
        ));
        let mut fresh = connect_v3(addr).await;
        fresh.close(None).await.expect("close fresh socket");
        stop_tx.send(()).expect("stop replacement server");
        tasks
            .join_next()
            .await
            .expect("replacement server task")
            .expect("task join")
            .expect("replacement server exits cleanly");
    })
    .await;
    restart_result.expect("fresh authentication and shutdown on the same address");
}

#[tokio::test]
async fn starter_shutdown_allows_responsive_http2_peer_to_finish() {
    let server = embedded_server::build_server(&starter_config())
        .await
        .expect("build starter");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("address");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(embedded_server::serve_until(listener, server, "*", async {
        shutdown_rx.await.expect("shutdown trigger");
        Ok(())
    }));

    let lifecycle_result = tokio::time::timeout(Duration::from_secs(10), async {
        let stream = TcpStream::connect(addr).await.expect("connect HTTP/2");
        let (mut sender, _driver) = test_helpers::open_h2(stream).await;
        let request = hyper::Request::get(format!("http://{addr}/health"))
            .body(axum::body::Body::empty())
            .expect("health request");
        let response = sender.send_request(request).await.expect("health response");
        assert_eq!(response.status(), hyper::StatusCode::OK);
        shutdown_tx.send(()).expect("request shutdown");
        tasks
            .join_next()
            .await
            .expect("server task")
            .expect("task join")
            .expect("a responsive HTTP/2 peer permits clean shutdown");
    })
    .await;
    lifecycle_result.expect("responsive HTTP/2 startup and shutdown must finish");
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
        embedded_server::serve_until(listener, server.clone(), "*", std::future::ready(Ok(()))),
    )
    .await
    .expect("idle drain deadline")
    .expect("serve result");
    assert!(server.is_draining());
}

#[tokio::test]
async fn starter_returns_signal_failure_after_cleanup() {
    let server = embedded_server::build_server(&starter_config())
        .await
        .expect("build starter");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        embedded_server::serve_until(
            listener,
            server.clone(),
            "*",
            std::future::ready(Err(std::io::Error::other("injected signal setup failure"))),
        ),
    )
    .await
    .expect("signal failure cleanup deadline");
    let error = result.expect_err("signal setup failure must not report a successful shutdown");
    let signal_error = error
        .downcast_ref::<std::io::Error>()
        .expect("original signal error");
    assert_eq!(signal_error.kind(), std::io::ErrorKind::Other);
    assert_eq!(signal_error.to_string(), "injected signal setup failure");
    assert!(server.is_draining());
    assert_eq!(
        server.wait_for_shutdown_connections(Duration::ZERO).await,
        0
    );
}
