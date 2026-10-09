//! Pre-upgrade HTTP header-read deadline: real-socket end-to-end tests
//! (issue #518).
//!
//! hyper's header-read timeout stays silently inert unless a `Timer` is
//! explicitly armed — `axum::serve` and `axum-server` both leave it unset, so
//! a raw-HTTP client could park a partial request forever before any
//! application handler, auth deadline, or idle deadline exists to see it.
//! The serve paths arm `websocket.http_header_read_timeout_secs` explicitly;
//! these tests prove a parked partial request is actually closed.

mod test_helpers;

use signal_fish_server::config::ProtocolConfig;
use std::time::Duration;
use test_helpers::RunningTestServer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn base_config() -> signal_fish_server::server::ServerConfig {
    signal_fish_server::server::ServerConfig {
        websocket_config: signal_fish_server::config::WebSocketConfig {
            http_header_read_timeout_secs: 1,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// A raw TCP client that transmits only part of its request headers is closed
/// by the server within the configured deadline instead of being parked
/// forever.
#[tokio::test]
async fn parked_partial_request_headers_are_closed_within_the_deadline() {
    let server = signal_fish_server::server::EnhancedGameServer::new(
        base_config(),
        ProtocolConfig::default(),
        signal_fish_server::config::RelayTypeConfig::default(),
        signal_fish_server::config::SessionConfig::default(),
        signal_fish_server::config::TurnConfig::default(),
        signal_fish_server::database::DatabaseConfig::default(),
        signal_fish_server::config::MetricsConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        Vec::new(),
    )
    .await
    .expect("test server constructs");
    let router = signal_fish_server::websocket::create_router("http://localhost:3000")
        .with_state(server.clone());
    let running_server = RunningTestServer::spawn(server, router).await;

    let mut stream = TcpStream::connect(running_server.addr())
        .await
        .expect("raw TCP connect");
    // Send only part of the request headers: no blank line, so the request
    // never completes and no handler ever runs.
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: parked\r\n")
        .await
        .expect("write partial headers");
    stream.flush().await.expect("flush partial headers");

    // The upgrade path's own deadlines (auth/idle) are far longer than the
    // header deadline, so any close observed here is attributable to the
    // header-read deadline alone.
    let mut buf = [0u8; 64];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await;
    match read {
        Ok(Ok(0)) => { /* EOF: the parked connection was closed. */ }
        Ok(Ok(n)) => {
            // Some servers answer with a 408 before closing; either way the
            // response must terminate, not hang.
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(
                text.contains("408") || text.contains("Request Timeout"),
                "unexpected partial response instead of a header timeout: {text}"
            );
            let _ = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await;
        }
        Ok(Err(error)) => panic!("transport error instead of header timeout close: {error}"),
        Err(_elapsed) => {
            panic!("a client parked mid-headers was not closed within the header-read deadline")
        }
    }

    // The socket must be unusable afterwards: the server closed it.
    let tail = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await;
    assert!(
        matches!(tail, Ok(Ok(0)) | Err(_)),
        "connection must stay closed after the header-read deadline"
    );

    running_server.shutdown().await;
}

/// A client that completes its headers promptly is unaffected: the deadline
/// bounds parked pre-upgrade requests, not honest clients.
#[tokio::test]
async fn prompt_requests_are_unaffected_by_the_header_deadline() {
    let server = signal_fish_server::server::EnhancedGameServer::new(
        base_config(),
        ProtocolConfig::default(),
        signal_fish_server::config::RelayTypeConfig::default(),
        signal_fish_server::config::SessionConfig::default(),
        signal_fish_server::config::TurnConfig::default(),
        signal_fish_server::database::DatabaseConfig::default(),
        signal_fish_server::config::MetricsConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        Vec::new(),
    )
    .await
    .expect("test server constructs");
    let router = signal_fish_server::websocket::create_router("http://localhost:3000")
        .with_state(server.clone());
    let running_server = RunningTestServer::spawn(server, router).await;

    let url = format!("ws://{}/ws", running_server.addr());
    let (ws, _response) = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async(&url),
    )
    .await
    .expect("websocket connect timed out")
    .expect("websocket connect failed");

    // Complete HTTP requests work fine after the handshake too: the header
    // deadline only parks-incomplete requests.
    let http_addr = running_server.addr();
    let mut stream = TcpStream::connect(http_addr)
        .await
        .expect("raw TCP connect");
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: prompt\r\nConnection: close\r\n\r\n")
        .await
        .expect("write complete request");
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut response))
        .await
        .expect("health response completes within the deadline")
        .expect("health response reads");
    let text = String::from_utf8_lossy(&response);
    assert!(
        text.contains("200"),
        "a prompt complete request must be served normally, got: {text}"
    );

    drop(ws);
    running_server.shutdown().await;
}

#[tokio::test]
/// Issue #845: both library router constructors accept GET and extended CONNECT.
async fn http2_extended_connect_authenticates_on_library_router_aliases() {
    for standalone in [false, true] {
        let server = test_helpers::create_test_server().await;
        let router = if standalone {
            signal_fish_server::websocket::create_standalone_router("https://allowed.example")
        } else {
            signal_fish_server::websocket::create_router("https://allowed.example").route(
                "/v3/ws",
                signal_fish_server::websocket::websocket_route_v3("https://allowed.example"),
            )
        }
        .with_state(server.clone());
        let running = test_helpers::RunningTestServer::spawn(server, router).await;
        for path in ["/ws", "/v3/ws"] {
            let (mut ws, response) =
                tokio_tungstenite::connect_async(format!("ws://{}{path}", running.addr()))
                    .await
                    .expect("HTTP/1 GET WebSocket remains supported");
            assert_eq!(
                response.status(),
                axum::http::StatusCode::SWITCHING_PROTOCOLS
            );
            ws.close(None).await.expect("close HTTP/1 control");
            let stream = TcpStream::connect(running.addr())
                .await
                .expect("library h2 connect");
            test_helpers::authenticate_over_h2(stream, running.addr().port(), path, "http").await;
        }
        running.shutdown().await;
    }
}

/// Extending the method route must preserve all admission gates.
#[tokio::test]
async fn http2_extended_connect_preserves_origin_drain_and_method_rejections() {
    use axum::http::{Method, StatusCode};

    let cases = [
        (
            "blocked Origin",
            Method::CONNECT,
            Some("https://blocked.example"),
            false,
            StatusCode::FORBIDDEN,
            "websocket",
        ),
        (
            "drain",
            Method::CONNECT,
            Some("https://allowed.example"),
            true,
            StatusCode::SERVICE_UNAVAILABLE,
            "websocket",
        ),
        (
            "POST",
            Method::POST,
            Some("https://allowed.example"),
            false,
            StatusCode::METHOD_NOT_ALLOWED,
            "websocket",
        ),
        (
            "wrong protocol",
            Method::CONNECT,
            Some("https://allowed.example"),
            false,
            StatusCode::BAD_REQUEST,
            "other-protocol",
        ),
    ];
    for (name, method, origin, draining, expected, protocol) in cases {
        let server = test_helpers::create_test_server().await;
        if draining {
            server.begin_shutdown_drain();
        }
        let router =
            signal_fish_server::websocket::create_standalone_router("https://allowed.example")
                .with_state(server.clone());
        let running = RunningTestServer::spawn(server, router).await;
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let stream = TcpStream::connect(running.addr())
                .await
                .expect("rejection socket");
            let (mut sender, mut driver) = test_helpers::open_h2(stream).await;
            for path in ["/ws", "/v3/ws"] {
                let mut request = hyper::Request::builder()
                    .method(method.clone())
                    .uri(format!("http://{}{path}", running.addr()))
                    .header("sec-websocket-version", "13");
                if method == Method::CONNECT {
                    request = request.extension(hyper::ext::Protocol::from_static(protocol));
                }
                if let Some(origin) = origin {
                    request = request.header("origin", origin);
                }
                let response = sender
                    .send_request(request.body(axum::body::Body::empty()).unwrap())
                    .await
                    .expect("rejection response");
                assert_eq!(response.status(), expected, "{name} {path}");
            }
            driver.abort_all();
            while driver.join_next().await.is_some() {}
        })
        .await;
        running.shutdown().await;
        result.expect("rejection deadline");
    }
}
