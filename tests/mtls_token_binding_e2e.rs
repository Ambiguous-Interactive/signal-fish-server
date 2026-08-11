#![cfg(feature = "tls")]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, KeyInit, Mac};
use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer, ServerName};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use signal_fish_server::config::ClientAuthMode;
use signal_fish_server::protocol::{ClientMessage, GameDataEncoding};
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, TlsConnector};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use tokio_tungstenite::WebSocketStream;

const TOKEN_BINDING_PROTOCOL: &str = "signalfish.tokenbinding.v1";
const CONNECT_DEADLINE: Duration = Duration::from_secs(30);

struct RunningServer {
    child: tokio::process::Child,
    port: u16,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    _workdir: tempfile::TempDir,
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        if std::thread::panicking() {
            let stdout = std::fs::read_to_string(&self.stdout_path).unwrap_or_default();
            let stderr = std::fs::read_to_string(&self.stderr_path).unwrap_or_default();
            eprintln!("TLS server stdout:\n{stdout}\nTLS server stderr:\n{stderr}");
        }
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tls")
        .join(name)
}

fn reserve_port() -> u16 {
    let listener = std::net::TcpListener::bind(("0.0.0.0", 0)).expect("bind port probe");
    listener.local_addr().expect("read port probe").port()
}

async fn tls_listener_is_ready(port: u16, client_auth: ClientAuthMode) -> bool {
    let identity = if client_auth == ClientAuthMode::Require {
        Some((
            fixture("client-101-cert.pem"),
            fixture("client-101-key.pem"),
        ))
    } else {
        None
    };
    let config = match identity.as_ref() {
        Some((certificate, key)) => client_config(Some((certificate.as_path(), key.as_path()))),
        None => client_config(None),
    };
    let ready = async {
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.ok()?;
        TlsConnector::from(config)
            .connect(ServerName::try_from("localhost").ok()?, tcp)
            .await
            .ok()
    };
    tokio::time::timeout(Duration::from_secs(2), ready)
        .await
        .ok()
        .flatten()
        .is_some()
}

async fn spawn_server(client_auth: ClientAuthMode, require_fingerprint: bool) -> RunningServer {
    let mut failures = Vec::new();
    for attempt in 1..=5 {
        let port = reserve_port();
        let workdir = tempfile::tempdir().expect("create server workdir");
        let config_path = workdir.path().join("config.json");
        let stdout_path = workdir.path().join("server.stdout.log");
        let stderr_path = workdir.path().join("server.stderr.log");
        let config = json!({
        "port": port,
        "security": {
            "enforce_app_id_allowlist": false,
            "require_metrics_auth": false,
            "cors_origins": "*",
            "transport": {
                "tls": {
                    "enabled": true,
                    "certificate_path": fixture("server-cert.pem"),
                    "private_key_path": fixture("server-key.pem"),
                    "client_ca_cert_path": fixture("cert.pem"),
                    "client_auth": client_auth.as_str()
                },
                "token_binding": {
                    "enabled": true,
                    "required": true,
                    "require_client_fingerprint": require_fingerprint,
                    "subprotocol": TOKEN_BINDING_PROTOCOL
                }
            }
        },
        "logging": { "enable_file_logging": false }
        });
        std::fs::write(
            &config_path,
            serde_json::to_vec_pretty(&config).expect("serialize server config"),
        )
        .expect("write server config");

        let stdout_file = std::fs::File::create(&stdout_path).expect("create server stdout log");
        let stderr_file = std::fs::File::create(&stderr_path).expect("create server stderr log");
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_signal-fish-server"));
        command
            .current_dir(workdir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file))
            .kill_on_drop(true);
        for (key, _) in std::env::vars_os() {
            if key
                .to_str()
                .is_some_and(|key| key.starts_with("SIGNAL_FISH"))
            {
                command.env_remove(key);
            }
        }
        command.env("SIGNAL_FISH_CONFIG_PATH", &config_path);
        command.env("SIGNAL_FISH__PORT", port.to_string());
        let child = command.spawn().expect("spawn TLS server");
        let mut server = RunningServer {
            child,
            port,
            stdout_path,
            stderr_path,
            _workdir: workdir,
        };

        let deadline = tokio::time::Instant::now() + CONNECT_DEADLINE;
        loop {
            if let Some(status) = server.child.try_wait().expect("poll TLS server") {
                let stdout = std::fs::read_to_string(&server.stdout_path).unwrap_or_default();
                let stderr = std::fs::read_to_string(&server.stderr_path).unwrap_or_default();
                failures.push(format!(
                    "attempt {attempt} on port {port} exited {status}\nstdout:\n{stdout}\nstderr:\n{stderr}"
                ));
                break;
            }
            if tls_listener_is_ready(port, client_auth).await {
                return server;
            }
            if tokio::time::Instant::now() >= deadline {
                let _ = server.child.start_kill();
                let stdout = std::fs::read_to_string(&server.stdout_path).unwrap_or_default();
                let stderr = std::fs::read_to_string(&server.stderr_path).unwrap_or_default();
                failures.push(format!(
                    "attempt {attempt} on port {port} timed out\nstdout:\n{stdout}\nstderr:\n{stderr}"
                ));
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    panic!(
        "TLS server did not bind after 5 fresh-port attempts (each bounded by {CONNECT_DEADLINE:?}):\n{}",
        failures.join("\n\n")
    );
}

fn client_config(identity: Option<(&Path, &Path)>) -> Arc<ClientConfig> {
    let server_certificate =
        CertificateDer::from_pem_file(fixture("cert.pem")).expect("parse server certificate");
    let mut roots = RootCertStore::empty();
    roots
        .add(server_certificate)
        .expect("trust test server certificate");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("configure client TLS versions")
        .with_root_certificates(roots);

    let config = if let Some((certificate_path, key_path)) = identity {
        let certificate =
            CertificateDer::from_pem_file(certificate_path).expect("parse client certificate");
        let key = PrivateKeyDer::from_pem_file(key_path).expect("parse client private key");
        builder
            .with_client_auth_cert(vec![certificate], key)
            .expect("configure client certificate")
    } else {
        builder.with_no_client_auth()
    };

    Arc::new(config)
}

type TestSocket = WebSocketStream<TlsStream<TcpStream>>;

async fn connect(
    port: u16,
    identity: Option<(&Path, &Path)>,
    spoofed_fingerprint: Option<&str>,
) -> Result<(TestSocket, String), WebSocketError> {
    let tcp = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect TLS socket");
    let tls = TlsConnector::from(client_config(identity))
        .connect(
            ServerName::try_from("localhost").expect("valid test server name"),
            tcp,
        )
        .await
        .expect("complete TLS handshake");
    let mut request = format!("wss://localhost:{port}/v2/ws")
        .into_client_request()
        .expect("build WebSocket request");
    request.headers_mut().insert(
        axum::http::header::SEC_WEBSOCKET_PROTOCOL,
        axum::http::HeaderValue::from_static(TOKEN_BINDING_PROTOCOL),
    );
    if let Some(spoofed) = spoofed_fingerprint {
        request.headers_mut().insert(
            "x-signalfish-client-cert-sha256",
            axum::http::HeaderValue::from_str(spoofed).expect("valid spoofed fingerprint header"),
        );
    }
    let handshake_key = request
        .headers()
        .get(axum::http::header::SEC_WEBSOCKET_KEY)
        .expect("generated WebSocket key")
        .to_str()
        .expect("ASCII WebSocket key")
        .to_string();
    tokio_tungstenite::client_async(request, tls)
        .await
        .map(|(socket, _)| (socket, handshake_key))
}

fn certificate_fingerprint(path: &Path) -> String {
    let certificate = CertificateDer::from_pem_file(path).expect("parse fingerprint certificate");
    let digest = Sha256::digest(certificate.as_ref());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn signed_message(
    handshake_key: &str,
    message: &ClientMessage,
    fingerprint: Option<&str>,
) -> String {
    let secret = BASE64_STANDARD
        .decode(handshake_key)
        .expect("decode WebSocket key");
    let mut value = serde_json::to_value(message).expect("serialize client message");
    value.sort_all_objects();
    let canonical = serde_json::to_vec(&value).expect("serialize canonical client message");
    let mut mac = Hmac::<Sha256>::new_from_slice(&secret).expect("create token-binding HMAC");
    mac.update(&canonical);
    if let Some(fingerprint) = fingerprint {
        mac.update(fingerprint.as_bytes());
    }
    let mut proof = json!({
        "scheme": "sec_websocket_key_sha256",
        "signature": BASE64_STANDARD.encode(mac.finalize().into_bytes())
    });
    if let (Some(fingerprint), Some(proof)) = (fingerprint, proof.as_object_mut()) {
        proof.insert("fingerprint".to_string(), json!(fingerprint));
    }
    value
        .as_object_mut()
        .expect("client message envelope")
        .insert("token_binding".to_string(), proof);
    value.to_string()
}

async fn authenticate(
    socket: &mut TestSocket,
    handshake_key: &str,
    fingerprint: Option<&str>,
) -> Value {
    let message = ClientMessage::Authenticate {
        app_id: "mtls_e2e".to_string(),
        sdk_version: None,
        platform: None,
        game_data_format: None,
        protocol_version: None,
        supported_transports: None,
        supported_topologies: None,
    };
    socket
        .send(Message::Text(
            signed_message(handshake_key, &message, fingerprint).into(),
        ))
        .await
        .expect("send token-bound authentication");
    let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("server response deadline")
        .expect("server response frame")
        .expect("read server response");
    serde_json::from_str(frame.to_text().expect("text server response"))
        .expect("parse server response")
}

#[tokio::test]
async fn verified_mtls_fingerprint_is_bound_end_to_end_and_headers_cannot_override_it() {
    let client_101_cert = fixture("client-101-cert.pem");
    let client_101_key = fixture("client-101-key.pem");
    let client_102_cert = fixture("client-102-cert.pem");
    let client_102_key = fixture("client-102-key.pem");
    let fingerprint_101 = certificate_fingerprint(&client_101_cert);
    let fingerprint_102 = certificate_fingerprint(&client_102_cert);
    assert_ne!(
        fingerprint_101, fingerprint_102,
        "rotation fixture must change identity"
    );

    let server = spawn_server(ClientAuthMode::Require, true).await;

    let (mut client_101, key_101) = connect(
        server.port,
        Some((&client_101_cert, &client_101_key)),
        Some(&fingerprint_102),
    )
    .await
    .expect("certificate-authenticated WebSocket upgrade");
    let authenticated = authenticate(&mut client_101, &key_101, Some(&fingerprint_101)).await;
    assert_eq!(
        authenticated.get("type").and_then(Value::as_str),
        Some("Authenticated"),
        "the rustls leaf certificate must win over a conflicting spoofed header: {authenticated}"
    );

    let (mut rotated_with_old_proof, rotated_key) =
        connect(server.port, Some((&client_102_cert, &client_102_key)), None)
            .await
            .expect("rotated certificate WebSocket upgrade");
    let rejection = authenticate(
        &mut rotated_with_old_proof,
        &rotated_key,
        Some(&fingerprint_101),
    )
    .await;
    assert_eq!(
        rejection.get("type").and_then(Value::as_str),
        Some("Error"),
        "a proof bound to the old certificate must fail after rotation: {rejection}"
    );

    let (mut rotated, rotated_key) =
        connect(server.port, Some((&client_102_cert, &client_102_key)), None)
            .await
            .expect("rotated certificate WebSocket upgrade");
    let authenticated = authenticate(&mut rotated, &rotated_key, Some(&fingerprint_102)).await;
    assert_eq!(
        authenticated.get("type").and_then(Value::as_str),
        Some("Authenticated"),
        "the proof must accept the new certificate fingerprint: {authenticated}"
    );
}

#[tokio::test]
async fn optional_mtls_without_a_certificate_cannot_be_satisfied_by_a_header() {
    let server = spawn_server(ClientAuthMode::Optional, true).await;
    let spoofed = certificate_fingerprint(&fixture("client-101-cert.pem"));
    let error = connect(server.port, None, Some(&spoofed))
        .await
        .expect_err("a request header must not substitute for a verified peer certificate");
    let WebSocketError::Http(response) = error else {
        panic!("expected HTTP rejection after optional-mTLS handshake, got {error}");
    };
    assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn ordinary_tls_token_binding_remains_usable_without_fingerprint_binding() {
    let server = spawn_server(ClientAuthMode::Optional, false).await;
    let (mut socket, handshake_key) = connect(server.port, None, None)
        .await
        .expect("certificate-optional WebSocket upgrade");
    let authenticated = authenticate(&mut socket, &handshake_key, None).await;
    assert_eq!(
        authenticated.get("type").and_then(Value::as_str),
        Some("Authenticated"),
        "non-fingerprint token binding must retain its existing behavior: {authenticated}"
    );
}

#[tokio::test]
async fn fingerprint_bound_connections_reject_unsigned_binary_frames() {
    let client_cert = fixture("client-101-cert.pem");
    let client_key = fixture("client-101-key.pem");
    let fingerprint = certificate_fingerprint(&client_cert);
    let server = spawn_server(ClientAuthMode::Require, true).await;
    let (mut socket, handshake_key) = connect(server.port, Some((&client_cert, &client_key)), None)
        .await
        .expect("fingerprint-bound WebSocket upgrade");
    let authenticated = authenticate(&mut socket, &handshake_key, Some(&fingerprint)).await;
    assert_eq!(
        authenticated.get("type").and_then(Value::as_str),
        Some("Authenticated")
    );
    socket
        .send(Message::Binary(vec![1, 2, 3].into()))
        .await
        .expect("send unsigned binary frame");

    let response = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = socket
                .next()
                .await
                .expect("server response frame")
                .expect("read server response");
            let response: Value =
                serde_json::from_str(frame.to_text().expect("text server response"))
                    .expect("parse server response");
            if response.get("type").and_then(Value::as_str) == Some("Error") {
                return response;
            }
        }
    })
    .await
    .expect("server error response deadline");
    assert_eq!(response.get("type").and_then(Value::as_str), Some("Error"));
    assert_eq!(
        response
            .get("data")
            .and_then(|data| data.get("error_code"))
            .and_then(Value::as_str),
        Some("UNAUTHORIZED")
    );
}

#[tokio::test]
async fn fingerprint_bound_authentication_rejects_messagepack_negotiation() {
    let client_cert = fixture("client-101-cert.pem");
    let client_key = fixture("client-101-key.pem");
    let fingerprint = certificate_fingerprint(&client_cert);
    let server = spawn_server(ClientAuthMode::Require, true).await;
    let (mut socket, handshake_key) = connect(server.port, Some((&client_cert, &client_key)), None)
        .await
        .expect("fingerprint-bound WebSocket upgrade");
    let message = ClientMessage::Authenticate {
        app_id: "mtls_e2e".to_string(),
        sdk_version: None,
        platform: None,
        game_data_format: Some(GameDataEncoding::MessagePack),
        protocol_version: None,
        supported_transports: None,
        supported_topologies: None,
    };
    socket
        .send(Message::Text(
            signed_message(&handshake_key, &message, Some(&fingerprint)).into(),
        ))
        .await
        .expect("send MessagePack negotiation request");

    let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("server response deadline")
        .expect("server response frame")
        .expect("read server response");
    let response: Value = serde_json::from_str(frame.to_text().expect("text server response"))
        .expect("parse server response");
    assert_eq!(response.get("type").and_then(Value::as_str), Some("Error"));
    assert_eq!(
        response
            .get("data")
            .and_then(|data| data.get("error_code"))
            .and_then(Value::as_str),
        Some("UNSUPPORTED_GAME_DATA_FORMAT")
    );

    let (authenticated, advertised_formats) = tokio::time::timeout(Duration::from_secs(5), async {
        let mut authenticated = false;
        loop {
            let frame = socket
                .next()
                .await
                .expect("server response frame")
                .expect("read server response");
            let response: Value =
                serde_json::from_str(frame.to_text().expect("text server response"))
                    .expect("parse server response");
            match response.get("type").and_then(Value::as_str) {
                Some("Authenticated") => authenticated = true,
                Some("ProtocolInfo") => {
                    let formats = response
                        .get("data")
                        .and_then(|data| data.get("game_data_formats"))
                        .and_then(Value::as_array)
                        .expect("ProtocolInfo game_data_formats")
                        .clone();
                    break (authenticated, formats);
                }
                _ => {}
            }
        }
    })
    .await
    .expect("authentication and ProtocolInfo deadline");
    assert!(authenticated, "fallback authentication must still complete");
    assert_eq!(advertised_formats, vec![json!("json")]);
}
