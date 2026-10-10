#![cfg(feature = "tls")]

mod test_helpers;

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
use signal_fish_server::security::{
    derive_server_nonce_secret, TokenBindingProof, TokenBoundBinaryFrame,
    TOKEN_BINDING_BINARY_DOMAIN, TOKEN_BINDING_JSON_DOMAIN, TOKEN_BINDING_VERSION,
};
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, TlsConnector};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use tokio_tungstenite::WebSocketStream;

const TOKEN_BINDING_PROTOCOL: &str = "signalfish.tokenbinding.v2";
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
    spawn_server_with_options(client_auth, require_fingerprint, 24, true).await
}

async fn spawn_server_with_max_connections(
    client_auth: ClientAuthMode,
    require_fingerprint: bool,
    max_connections_per_ip: usize,
) -> RunningServer {
    spawn_server_with_options(
        client_auth,
        require_fingerprint,
        max_connections_per_ip,
        true,
    )
    .await
}

async fn spawn_server_with_options(
    client_auth: ClientAuthMode,
    require_fingerprint: bool,
    max_connections_per_ip: usize,
    token_binding_enabled: bool,
) -> RunningServer {
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
            "max_connections_per_ip": max_connections_per_ip,
            "transport": {
                "tls": {
                    "enabled": true,
                    "certificate_path": fixture("server-cert.pem"),
                    "private_key_path": fixture("server-key.pem"),
                    "client_ca_cert_path": fixture("cert.pem"),
                    "client_auth": client_auth.as_str()
                },
                "token_binding": {
                    "enabled": token_binding_enabled,
                    "required": token_binding_enabled,
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

#[derive(Debug)]
struct TokenBindingClient {
    secret: Arc<[u8]>,
    next_sequence: u64,
}

async fn connect(
    port: u16,
    identity: Option<(&Path, &Path)>,
    spoofed_fingerprint: Option<&str>,
) -> Result<(TestSocket, TokenBindingClient), WebSocketError> {
    let (mut socket, handshake_key) =
        connect_upgraded(port, identity, spoofed_fingerprint, true).await?;
    let challenge_frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("token-binding challenge deadline")
        .expect("token-binding challenge frame")?;
    let challenge: Value = serde_json::from_str(
        challenge_frame
            .to_text()
            .expect("token-binding challenge is text"),
    )
    .expect("parse token-binding challenge");
    assert_eq!(
        challenge.get("type").and_then(Value::as_str),
        Some("TokenBindingChallenge")
    );
    let data = challenge.get("data").expect("challenge data");
    let nonce = BASE64_STANDARD
        .decode(
            data.get("nonce")
                .and_then(Value::as_str)
                .expect("challenge nonce"),
        )
        .expect("decode challenge nonce");
    let first_sequence = data
        .get("first_sequence")
        .and_then(Value::as_u64)
        .expect("challenge first sequence");
    let secret = derive_server_nonce_secret(&handshake_key, &nonce)
        .expect("derive token-binding session key");
    Ok((
        socket,
        TokenBindingClient {
            secret,
            next_sequence: first_sequence,
        },
    ))
}

async fn connect_upgraded(
    port: u16,
    identity: Option<(&Path, &Path)>,
    spoofed_fingerprint: Option<&str>,
    offer_token_binding: bool,
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
    if offer_token_binding {
        request.headers_mut().insert(
            axum::http::header::SEC_WEBSOCKET_PROTOCOL,
            axum::http::HeaderValue::from_static(TOKEN_BINDING_PROTOCOL),
        );
    }
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
    let (socket, _) = tokio_tungstenite::client_async(request, tls).await?;
    Ok((socket, handshake_key))
}

fn certificate_fingerprint(path: &Path) -> String {
    let certificate = CertificateDer::from_pem_file(path).expect("parse fingerprint certificate");
    let digest = Sha256::digest(certificate.as_ref());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn signed_message(
    binding: &mut TokenBindingClient,
    message: &ClientMessage,
    fingerprint: Option<&str>,
) -> String {
    let mut value = serde_json::to_value(message).expect("serialize client message");
    // These real-wire fixtures use ASCII property names and portable integers,
    // for which serde_json's sorted compact Value encoding is the JCS form.
    // The independent unit goldens cover UTF-16 ordering and number rendering.
    let canonical = serde_json::to_vec(&value).expect("canonicalize client message fixture");
    let sequence = binding.next_sequence;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(binding.secret.as_ref()).expect("create token-binding HMAC");
    mac.update(TOKEN_BINDING_JSON_DOMAIN);
    mac.update(&sequence.to_be_bytes());
    mac.update(&canonical);
    if let Some(fingerprint) = fingerprint {
        mac.update(fingerprint.as_bytes());
    }
    let mut proof = serde_json::to_value(TokenBindingProof {
        version: TOKEN_BINDING_VERSION,
        scheme:
            signal_fish_server::security::token_binding::TokenBindingScheme::ServerNonceHkdfSha256,
        sequence,
        signature: BASE64_STANDARD.encode(mac.finalize().into_bytes()),
        fingerprint: None,
    })
    .expect("serialize proof");
    if let (Some(fingerprint), Some(proof)) = (fingerprint, proof.as_object_mut()) {
        proof.insert("fingerprint".to_string(), json!(fingerprint));
    }
    value
        .as_object_mut()
        .expect("client message envelope")
        .insert("token_binding".to_string(), proof);
    binding.next_sequence = binding.next_sequence.saturating_add(1);
    value.to_string()
}

fn signed_binary_message(
    binding: &mut TokenBindingClient,
    payload: Vec<u8>,
    fingerprint: Option<&str>,
) -> Vec<u8> {
    let sequence = binding.next_sequence;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(binding.secret.as_ref()).expect("create token-binding HMAC");
    mac.update(TOKEN_BINDING_BINARY_DOMAIN);
    mac.update(&sequence.to_be_bytes());
    mac.update(&payload);
    if let Some(fingerprint) = fingerprint {
        mac.update(fingerprint.as_bytes());
    }
    let frame = TokenBoundBinaryFrame {
        token_binding: TokenBindingProof {
            version: TOKEN_BINDING_VERSION,
            scheme: signal_fish_server::security::token_binding::TokenBindingScheme::ServerNonceHkdfSha256,
            sequence,
            signature: BASE64_STANDARD.encode(mac.finalize().into_bytes()),
            fingerprint: fingerprint.map(str::to_string),
        },
        payload,
    };
    binding.next_sequence = binding.next_sequence.saturating_add(1);
    rmp_serde::to_vec_named(&frame).expect("encode signed binary envelope")
}

async fn authenticate<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
    binding: &mut TokenBindingClient,
    fingerprint: Option<&str>,
) -> Value {
    let message = ClientMessage::Authenticate {
        app_id: "mtls_e2e".to_string(),
        connect_token: None,
        sdk_version: None,
        platform: None,
        game_data_format: None,
        protocol_version: Some(3),
        supported_transports: None,
        supported_topologies: None,
        requested_capabilities: None,
    };
    socket
        .send(Message::Text(
            signed_message(binding, &message, fingerprint).into(),
        ))
        .await
        .expect("send token-bound authentication");
    tokio::time::timeout(Duration::from_secs(5), next_json_message(socket))
        .await
        .expect("server response deadline")
        .expect("authentication response before close")
}

async fn send_signed<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
    binding: &mut TokenBindingClient,
    message: &ClientMessage,
    fingerprint: Option<&str>,
) {
    socket
        .send(Message::Text(
            signed_message(binding, message, fingerprint).into(),
        ))
        .await
        .expect("send token-bound client message");
}

/// Next JSON application message from the server, skipping interleaved
/// non-JSON control traffic: ping probes carry an opaque nonce payload and
/// close frames carry a plain reason string, and loaded sanitizer builds can
/// surface either ahead of the awaited frame. Returns `None` once the socket
/// closes or errors.
async fn next_json_message<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
) -> Option<Value> {
    loop {
        let frame = socket.next().await?.ok()?;
        let Ok(text) = frame.to_text() else {
            continue;
        };
        if let Ok(value) = serde_json::from_str::<Value>(text) {
            return Some(value);
        }
    }
}

async fn next_message_of_type<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
    expected: &str,
) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match next_json_message(socket).await {
                Some(value) if value.get("type").and_then(Value::as_str) == Some(expected) => {
                    return value;
                }
                Some(_) => {}
                None => panic!("server closed before sending {expected}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("server did not send {expected} before deadline"))
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

    let (mut client_101, mut binding_101) = connect(
        server.port,
        Some((&client_101_cert, &client_101_key)),
        Some(&fingerprint_102),
    )
    .await
    .expect("certificate-authenticated WebSocket upgrade");
    let authenticated =
        authenticate(&mut client_101, &mut binding_101, Some(&fingerprint_101)).await;
    assert_eq!(
        authenticated.get("type").and_then(Value::as_str),
        Some("Authenticated"),
        "the rustls leaf certificate must win over a conflicting spoofed header: {authenticated}"
    );

    let (mut rotated_with_old_proof, mut rotated_binding) =
        connect(server.port, Some((&client_102_cert, &client_102_key)), None)
            .await
            .expect("rotated certificate WebSocket upgrade");
    let rejection = authenticate(
        &mut rotated_with_old_proof,
        &mut rotated_binding,
        Some(&fingerprint_101),
    )
    .await;
    assert_eq!(
        rejection.get("type").and_then(Value::as_str),
        Some("Error"),
        "a proof bound to the old certificate must fail after rotation: {rejection}"
    );

    let (mut rotated, mut rotated_binding) =
        connect(server.port, Some((&client_102_cert, &client_102_key)), None)
            .await
            .expect("rotated certificate WebSocket upgrade");
    let authenticated =
        authenticate(&mut rotated, &mut rotated_binding, Some(&fingerprint_102)).await;
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
    let (mut socket, mut binding) = connect(server.port, None, None)
        .await
        .expect("certificate-optional WebSocket upgrade");
    let authenticated = authenticate(&mut socket, &mut binding, None).await;
    assert_eq!(
        authenticated.get("type").and_then(Value::as_str),
        Some("Authenticated"),
        "non-fingerprint token binding must retain its existing behavior: {authenticated}"
    );
    let payload = rmp_serde::to_vec_named(&json!({"unsigned_mode": "still authenticated"}))
        .expect("encode non-fingerprint MessagePack payload");
    socket
        .send(Message::Binary(
            signed_binary_message(&mut binding, payload, None).into(),
        ))
        .await
        .expect("send signed non-fingerprint binary envelope");
    send_signed(&mut socket, &mut binding, &ClientMessage::Ping, None).await;
    let pong = next_message_of_type(&mut socket, "Pong").await;
    assert_eq!(pong.get("type").and_then(Value::as_str), Some("Pong"));
}

#[tokio::test]
async fn required_mtls_without_token_binding_keeps_unsigned_json_and_binary_paths() {
    let client_cert = fixture("client-101-cert.pem");
    let client_key = fixture("client-101-key.pem");
    let server = spawn_server_with_options(ClientAuthMode::Require, false, 24, false).await;
    let (mut socket, _) =
        connect_upgraded(server.port, Some((&client_cert, &client_key)), None, false)
            .await
            .expect("ordinary mTLS WebSocket upgrade without token-binding subprotocol");

    let authentication = ClientMessage::Authenticate {
        app_id: "mtls_without_binding".to_string(),
        connect_token: None,
        sdk_version: None,
        platform: None,
        game_data_format: Some(GameDataEncoding::MessagePack),
        protocol_version: Some(3),
        supported_transports: None,
        supported_topologies: None,
        requested_capabilities: None,
    };
    socket
        .send(Message::Text(
            serde_json::to_string(&authentication)
                .expect("serialize ordinary authentication")
                .into(),
        ))
        .await
        .expect("send unsigned authentication");
    let first = tokio::time::timeout(Duration::from_secs(5), next_json_message(&mut socket))
        .await
        .expect("ordinary authentication deadline")
        .expect("authentication response before close");
    assert_eq!(
        first.get("type").and_then(Value::as_str),
        Some("Authenticated"),
        "disabled token binding must not prepend a challenge: {first}"
    );

    let join = ClientMessage::JoinRoom {
        game_name: "ordinary-mtls".to_string(),
        room_code: None,
        player_name: "unsigned".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,

        password: None,
        join_only: None,
    };
    socket
        .send(Message::Text(
            serde_json::to_string(&join)
                .expect("serialize ordinary join")
                .into(),
        ))
        .await
        .expect("send unsigned join");
    next_message_of_type(&mut socket, "RoomJoined").await;

    let binary = rmp_serde::to_vec_named(&json!({"ordinary": "binary"}))
        .expect("encode ordinary binary payload");
    socket
        .send(Message::Binary(binary.into()))
        .await
        .expect("send unsigned binary payload");
    socket
        .send(Message::Text(
            serde_json::to_string(&ClientMessage::Ping)
                .expect("serialize ordinary ping")
                .into(),
        ))
        .await
        .expect("send unsigned ping after binary");
    let pong = next_message_of_type(&mut socket, "Pong").await;
    assert_eq!(pong.get("type").and_then(Value::as_str), Some("Pong"));
}

#[tokio::test]
async fn challenge_precedes_post_upgrade_registration_rejection() {
    let server = spawn_server_with_max_connections(ClientAuthMode::Optional, false, 1).await;
    let (mut incumbent, mut incumbent_binding) = connect(server.port, None, None)
        .await
        .expect("open incumbent token-bound connection");
    let authenticated = authenticate(&mut incumbent, &mut incumbent_binding, None).await;
    assert_eq!(
        authenticated.get("type").and_then(Value::as_str),
        Some("Authenticated")
    );

    let (mut rejected, _) = connect_upgraded(server.port, None, None, true)
        .await
        .expect("post-upgrade IP-limit rejection socket");
    let first = tokio::time::timeout(Duration::from_secs(5), next_json_message(&mut rejected))
        .await
        .expect("challenge deadline")
        .expect("challenge response before close");
    assert_eq!(
        first.get("type").and_then(Value::as_str),
        Some("TokenBindingChallenge"),
        "the negotiated challenge must be the first application frame: {first}"
    );

    let second = tokio::time::timeout(Duration::from_secs(5), next_json_message(&mut rejected))
        .await
        .expect("registration error deadline")
        .expect("registration error before close");
    assert_eq!(second.get("type").and_then(Value::as_str), Some("Error"));
    assert_eq!(
        second
            .get("data")
            .and_then(|data| data.get("error_code"))
            .and_then(Value::as_str),
        Some("TOO_MANY_CONNECTIONS")
    );
}

#[tokio::test]
async fn reconnect_token_is_bound_to_issuing_certificate_without_consuming_mismatch() {
    let cert_a = fixture("client-101-cert.pem");
    let key_a = fixture("client-101-key.pem");
    let cert_b = fixture("client-102-cert.pem");
    let key_b = fixture("client-102-key.pem");
    let fingerprint_a = certificate_fingerprint(&cert_a);
    let fingerprint_b = certificate_fingerprint(&cert_b);
    let server = spawn_server(ClientAuthMode::Require, true).await;

    let (mut original, mut original_binding) = connect(server.port, Some((&cert_a, &key_a)), None)
        .await
        .expect("connect original certificate");
    let authenticated =
        authenticate(&mut original, &mut original_binding, Some(&fingerprint_a)).await;
    assert_eq!(
        authenticated.get("type").and_then(Value::as_str),
        Some("Authenticated")
    );
    send_signed(
        &mut original,
        &mut original_binding,
        &ClientMessage::JoinRoom {
            game_name: "reconnect-binding".to_string(),
            room_code: None,
            player_name: "original".to_string(),
            max_players: Some(2),
            supports_authority: Some(true),
            relay_transport: None,

            password: None,
            join_only: None,
        },
        Some(&fingerprint_a),
    )
    .await;
    let joined = next_message_of_type(&mut original, "RoomJoined").await;
    let joined_data = joined.get("data").expect("RoomJoined data");
    let player_id = joined_data.get("player_id").cloned().expect("player id");
    let room_id = joined_data.get("room_id").cloned().expect("room id");
    let room_code = joined_data.get("room_code").cloned().expect("room code");
    let auth_token = joined_data
        .get("reconnection_token")
        .cloned()
        .expect("certificate-bound reconnect token");
    original.close(None).await.expect("close original socket");

    let reconnect = |player_id: &Value, room_id: &Value, auth_token: &Value| {
        serde_json::from_value::<ClientMessage>(json!({
            "type": "Reconnect",
            "data": {
                "player_id": player_id,
                "room_id": room_id,
                "auth_token": auth_token
            }
        }))
        .expect("construct reconnect message")
    };

    // Poll the observable reconnect state rather than sleeping: close-frame
    // delivery and disconnect registration are distinct asynchronous steps.
    let attack_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (mut attacker, mut attacker_binding) =
            connect(server.port, Some((&cert_b, &key_b)), None)
                .await
                .expect("connect alternate valid certificate");
        authenticate(&mut attacker, &mut attacker_binding, Some(&fingerprint_b)).await;
        send_signed(
            &mut attacker,
            &mut attacker_binding,
            &reconnect(&player_id, &room_id, &auth_token),
            Some(&fingerprint_b),
        )
        .await;
        let rejected = next_message_of_type(&mut attacker, "ReconnectionFailed").await;
        let error_code = rejected
            .get("data")
            .and_then(|data| data.get("error_code"))
            .and_then(Value::as_str);
        if error_code == Some("RECONNECTION_TOKEN_INVALID") {
            break;
        }
        assert!(
            matches!(
                error_code,
                Some("RECONNECTION_FAILED" | "PLAYER_ALREADY_CONNECTED")
            ),
            "{rejected}"
        );
        assert!(
            tokio::time::Instant::now() < attack_deadline,
            "disconnect record did not become observable before deadline"
        );
        attacker.close(None).await.expect("close polling attacker");
        tokio::task::yield_now().await;
    }

    let (mut legitimate, mut legitimate_binding) =
        connect(server.port, Some((&cert_a, &key_a)), None)
            .await
            .expect("reconnect original certificate");
    authenticate(
        &mut legitimate,
        &mut legitimate_binding,
        Some(&fingerprint_a),
    )
    .await;
    send_signed(
        &mut legitimate,
        &mut legitimate_binding,
        &reconnect(&player_id, &room_id, &auth_token),
        Some(&fingerprint_a),
    )
    .await;
    let restored = next_message_of_type(&mut legitimate, "Reconnected").await;
    assert_eq!(
        restored.get("data").and_then(|data| data.get("player_id")),
        Some(&player_id),
        "identity mismatch must not consume the valid token"
    );

    // Exercise the documented rotation recovery: B joins normally, receives a
    // B-bound credential, disconnects, and uses it from a fresh B connection.
    let (mut rotated, mut rotated_binding) = connect(server.port, Some((&cert_b, &key_b)), None)
        .await
        .expect("connect rotated certificate for normal join");
    authenticate(&mut rotated, &mut rotated_binding, Some(&fingerprint_b)).await;
    send_signed(
        &mut rotated,
        &mut rotated_binding,
        &ClientMessage::JoinRoom {
            game_name: "reconnect-binding".to_string(),
            room_code: room_code.as_str().map(str::to_string),
            player_name: "rotated".to_string(),
            max_players: None,
            supports_authority: Some(true),
            relay_transport: None,

            password: None,
            join_only: None,
        },
        Some(&fingerprint_b),
    )
    .await;
    let rotated_join = next_message_of_type(&mut rotated, "RoomJoined").await;
    let rotated_data = rotated_join.get("data").expect("rotated RoomJoined data");
    let rotated_player = rotated_data
        .get("player_id")
        .cloned()
        .expect("rotated player");
    let rotated_room = rotated_data.get("room_id").cloned().expect("rotated room");
    let rotated_token = rotated_data
        .get("reconnection_token")
        .cloned()
        .expect("B-bound reconnect token");
    rotated.close(None).await.expect("close rotated socket");

    let recovery_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (mut recovered, mut recovered_binding) =
            connect(server.port, Some((&cert_b, &key_b)), None)
                .await
                .expect("connect rotated recovery socket");
        authenticate(&mut recovered, &mut recovered_binding, Some(&fingerprint_b)).await;
        send_signed(
            &mut recovered,
            &mut recovered_binding,
            &reconnect(&rotated_player, &rotated_room, &rotated_token),
            Some(&fingerprint_b),
        )
        .await;
        let response = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match next_json_message(&mut recovered).await {
                    Some(value)
                        if matches!(
                            value.get("type").and_then(Value::as_str),
                            Some("Reconnected" | "ReconnectionFailed")
                        ) =>
                    {
                        break value;
                    }
                    Some(_) => {}
                    None => panic!("server closed before the rotation recovery response"),
                }
            }
        })
        .await
        .expect("rotation recovery deadline");
        if response.get("type").and_then(Value::as_str) == Some("Reconnected") {
            break;
        }
        let error_code = response
            .get("data")
            .and_then(|data| data.get("error_code"))
            .and_then(Value::as_str);
        assert!(
            matches!(
                error_code,
                Some("RECONNECTION_FAILED" | "PLAYER_ALREADY_CONNECTED")
            ),
            "{response}"
        );
        assert!(
            tokio::time::Instant::now() < recovery_deadline,
            "B-bound disconnect record did not become observable before deadline"
        );
        recovered.close(None).await.expect("close recovery poll");
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn fingerprint_bound_connections_reject_unsigned_binary_frames() {
    let client_cert = fixture("client-101-cert.pem");
    let client_key = fixture("client-101-key.pem");
    let fingerprint = certificate_fingerprint(&client_cert);
    let server = spawn_server(ClientAuthMode::Require, true).await;
    let (mut socket, mut binding) = connect(server.port, Some((&client_cert, &client_key)), None)
        .await
        .expect("fingerprint-bound WebSocket upgrade");
    let authenticated = authenticate(&mut socket, &mut binding, Some(&fingerprint)).await;
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
            match next_json_message(&mut socket).await {
                Some(response) if response.get("type").and_then(Value::as_str) == Some("Error") => {
                    break response;
                }
                Some(_) => {}
                None => panic!("server closed without an error response"),
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
async fn non_fingerprint_token_binding_also_rejects_unsigned_binary_frames() {
    let server = spawn_server(ClientAuthMode::Optional, false).await;
    let (mut socket, mut binding) = connect(server.port, None, None)
        .await
        .expect("token-bound WebSocket upgrade");
    authenticate(&mut socket, &mut binding, None).await;
    socket
        .send(Message::Binary(vec![1, 2, 3].into()))
        .await
        .expect("send unsigned binary frame");
    // The Error JSON farewell is advisory by contract (see
    // `enqueue_farewell_message`): the close itself is the authoritative
    // rejection signal, and when the close lands while the send task is mid-
    // write, the final drain legally abandons queued advisories instead of
    // writing past a payload with an unknown wire position. Under load the
    // socket can therefore close without the farewell having been observed,
    // so the farewell is asserted only when it is observed.
    loop {
        let Some(response) = next_json_message(&mut socket).await else {
            break;
        };
        if response.get("type").and_then(Value::as_str) != Some("Error") {
            continue;
        }
        assert_eq!(
            response
                .get("data")
                .and_then(|data| data.get("error_code"))
                .and_then(Value::as_str),
            Some("UNAUTHORIZED")
        );
        break;
    }
}

#[tokio::test]
async fn fingerprint_bound_authentication_advertises_signed_messagepack() {
    let client_cert = fixture("client-101-cert.pem");
    let client_key = fixture("client-101-key.pem");
    let fingerprint = certificate_fingerprint(&client_cert);
    let server = spawn_server(ClientAuthMode::Require, true).await;
    let (mut socket, mut binding) = connect(server.port, Some((&client_cert, &client_key)), None)
        .await
        .expect("fingerprint-bound WebSocket upgrade");
    let message = ClientMessage::Authenticate {
        app_id: "mtls_e2e".to_string(),
        connect_token: None,
        sdk_version: None,
        platform: None,
        game_data_format: Some(GameDataEncoding::MessagePack),
        protocol_version: None,
        supported_transports: None,
        supported_topologies: None,
        requested_capabilities: None,
    };
    send_signed(&mut socket, &mut binding, &message, Some(&fingerprint)).await;

    let (authenticated, advertised_formats) = tokio::time::timeout(Duration::from_secs(5), async {
        let mut authenticated = false;
        loop {
            let Some(response) = next_json_message(&mut socket).await else {
                panic!("server closed before ProtocolInfo");
            };
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
    assert!(authenticated, "MessagePack authentication must complete");
    assert_eq!(
        advertised_formats,
        vec![json!("json"), json!("message_pack")]
    );

    let binary_payload = rmp_serde::to_vec_named(&json!({"frame": "accepted"}))
        .expect("encode inner MessagePack payload");
    socket
        .send(Message::Binary(
            signed_binary_message(&mut binding, binary_payload, Some(&fingerprint)).into(),
        ))
        .await
        .expect("send signed MessagePack envelope");
    // A valid binary proof does not terminate the connection. The player is
    // not in a room, so the payload itself may yield a normal application
    // error; a subsequent proof on the shared sequence must still be accepted.
    send_signed(
        &mut socket,
        &mut binding,
        &ClientMessage::Ping,
        Some(&fingerprint),
    )
    .await;
    let pong = next_message_of_type(&mut socket, "Pong").await;
    assert_eq!(pong.get("type").and_then(Value::as_str), Some("Pong"));
}

/* RFC 8441 omits Sec-WebSocket-Key; the test client must derive its key
 * independently so a shared server derivation bug cannot pass the handshake. */
type H2Socket = WebSocketStream<hyper_util::rt::TokioIo<hyper::upgrade::Upgraded>>;
struct H2Client {
    sender: hyper::client::conn::http2::SendRequest<axum::body::Body>,
    _driver: tokio::task::JoinSet<Result<(), hyper::Error>>,
    port: u16,
}

impl H2Client {
    async fn new(port: u16, certificate: &Path, key: &Path) -> Self {
        let mut config = (*client_config(Some((certificate, key)))).clone();
        config.alpn_protocols = vec![b"h2".to_vec()];
        let tcp = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("h2 TCP");
        let tls = TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("localhost").unwrap(), tcp)
            .await
            .expect("h2 mTLS handshake");
        assert_eq!(tls.get_ref().1.alpn_protocol(), Some(b"h2".as_slice()));
        let (sender, driver) = test_helpers::open_h2(tls).await;
        Self {
            sender,
            _driver: driver,
            port,
        }
    }

    async fn connect(
        &mut self,
        path: &str,
        seed: &str,
        spoof: Option<&str>,
    ) -> (H2Socket, TokenBindingClient, Value) {
        let mut request = hyper::Request::builder()
            .method(hyper::Method::CONNECT)
            .uri(format!("https://localhost:{}{path}", self.port))
            .header("sec-websocket-version", "13")
            .header("sec-websocket-protocol", TOKEN_BINDING_PROTOCOL)
            .header("x-signalfish-token-binding-key", seed)
            .extension(hyper::ext::Protocol::from_static("websocket"));
        if let Some(spoof) = spoof {
            request = request.header("x-signalfish-client-cert-sha256", spoof);
        }
        let request = request.body(axum::body::Body::empty()).unwrap();
        assert!(!request.headers().contains_key("sec-websocket-key"));
        let response = self
            .sender
            .send_request(request)
            .await
            .expect("h2 CONNECT response");
        assert_eq!(
            response.status(),
            hyper::StatusCode::OK,
            "{path}: application seed must support RFC 8441"
        );
        assert_eq!(
            response.headers().get("sec-websocket-protocol").unwrap(),
            TOKEN_BINDING_PROTOCOL
        );
        assert!(!response.headers().contains_key("sec-websocket-accept"));
        let upgraded = hyper::upgrade::on(response).await.expect("h2 upgrade");
        let mut socket = WebSocketStream::from_raw_socket(
            hyper_util::rt::TokioIo::new(upgraded),
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await;
        // The first application frame must be the challenge, before any registration reply.
        let challenge = next_json_message(&mut socket)
            .await
            .expect("first h2 application frame");
        assert_eq!(challenge["type"], "TokenBindingChallenge");
        assert_eq!(challenge["data"]["version"], 2);
        assert_eq!(challenge["data"]["scheme"], "server_nonce_hkdf_sha256");
        assert_eq!(challenge["data"]["first_sequence"], 1);
        let nonce = BASE64_STANDARD
            .decode(challenge["data"]["nonce"].as_str().unwrap())
            .unwrap();
        assert_eq!(nonce.len(), 32);
        let client_seed = BASE64_STANDARD.decode(seed).unwrap();
        let hkdf = hkdf::Hkdf::<Sha256>::new(Some(&nonce), &client_seed);
        let mut secret = [0u8; 32];
        hkdf.expand(b"signalfish.tokenbinding.v2/session-key", &mut secret)
            .unwrap();
        (
            socket,
            TokenBindingClient {
                secret: Arc::from(secret),
                next_sequence: 1,
            },
            challenge,
        )
    }
}

fn h2_authentication() -> ClientMessage {
    ClientMessage::Authenticate {
        app_id: "h2-token-binding".into(),
        connect_token: None,
        sdk_version: None,
        platform: None,
        game_data_format: Some(GameDataEncoding::MessagePack),
        protocol_version: None,
        supported_transports: None,
        supported_topologies: None,
        requested_capabilities: None,
    }
}

async fn assert_binding_rejection(socket: &mut H2Socket, case: &str) {
    /* A farewell is advisory; closure is the authoritative rejection. A valid
     * Pong or Authenticated reply would prove the invalid frame was accepted. */
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, socket.next())
            .await
            .unwrap_or_else(|_| {
                panic!("{case}: server did not reject before the 5-second deadline")
            }) {
            None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
            Some(Ok(Message::Text(text))) => {
                let reply: Value = serde_json::from_str(&text).expect("rejection JSON");
                assert_eq!(reply["type"], "Error", "{case}: {reply}");
                assert_eq!(
                    reply["data"]["error_code"], "UNAUTHORIZED",
                    "{case}: {reply}"
                );
            }
            Some(Ok(_)) => {}
        }
    }
}

#[tokio::test]
async fn http2_token_binding_authenticates_rooms_and_shares_json_binary_sequence() {
    let certificate = fixture("client-101-cert.pem");
    let key = fixture("client-101-key.pem");
    let fingerprint = certificate_fingerprint(&certificate);
    let spoof = certificate_fingerprint(&fixture("client-102-cert.pem"));
    let server = spawn_server(ClientAuthMode::Require, true).await;
    tokio::time::timeout(CONNECT_DEADLINE, async {
        let mut h2 = H2Client::new(server.port, &certificate, &key).await;
        for (path, version) in [("/v2/ws", Value::Null), ("/v3/ws", json!(3))] {
            let (mut socket, mut binding, _) = h2
                .connect(path, "AQEBAQEBAQEBAQEBAQEBAQ==", Some(&spoof))
                .await;
            send_signed(
                &mut socket,
                &mut binding,
                &h2_authentication(),
                Some(&fingerprint),
            )
            .await;
            next_message_of_type(&mut socket, "Authenticated").await;
            let info = next_message_of_type(&mut socket, "ProtocolInfo").await;
            assert_eq!(info["data"]["protocol_version"], version, "{path}");
            assert_eq!(
                info["data"]["game_data_formats"],
                json!(["json", "message_pack"])
            );
            let join = ClientMessage::JoinRoom {
                game_name: "h2-bound-room".into(),
                room_code: None,
                player_name: "h2-player".into(),
                max_players: Some(2),
                supports_authority: Some(true),
                relay_transport: None,
                password: None,
                join_only: None,
            };
            send_signed(&mut socket, &mut binding, &join, Some(&fingerprint)).await;
            let joined = next_message_of_type(&mut socket, "RoomJoined").await;
            let (mut recipient, mut recipient_binding, _) =
                h2.connect(path, "AwMDAwMDAwMDAwMDAwMDAw==", None).await;
            send_signed(
                &mut recipient,
                &mut recipient_binding,
                &h2_authentication(),
                Some(&fingerprint),
            )
            .await;
            next_message_of_type(&mut recipient, "Authenticated").await;
            next_message_of_type(&mut recipient, "ProtocolInfo").await;
            let mut recipient_join = join.clone();
            if let ClientMessage::JoinRoom {
                room_code,
                player_name,
                ..
            } = &mut recipient_join
            {
                *room_code = Some(joined["data"]["room_code"].as_str().unwrap().to_string());
                *player_name = "h2-recipient".into();
            }
            send_signed(
                &mut recipient,
                &mut recipient_binding,
                &recipient_join,
                Some(&fingerprint),
            )
            .await;
            next_message_of_type(&mut recipient, "RoomJoined").await;
            let payload = rmp_serde::to_vec_named(&json!({"frame": "h2-game"})).unwrap();
            let frame = signed_binary_message(&mut binding, payload.clone(), Some(&fingerprint));
            socket.send(Message::Binary(frame.into())).await.unwrap();
            loop {
                let frame = recipient
                    .next()
                    .await
                    .expect("game relay frame")
                    .expect("game relay socket");
                match frame {
                    Message::Binary(wire) => {
                        #[derive(serde::Deserialize)]
                        struct Relay {
                            #[serde(with = "serde_bytes")]
                            payload: Vec<u8>,
                        }
                        let relay: Relay =
                            rmp_serde::from_slice(&wire).expect("MessagePack relay envelope");
                        assert_eq!(
                            relay.payload, payload,
                            "{path}: gameplay payload must reach the other peer"
                        );
                        break;
                    }
                    Message::Text(text) => {
                        let value: Value = serde_json::from_str(&text).unwrap();
                        assert_ne!(value["type"], "Error", "{path}: {value}");
                    }
                    Message::Close(_) => panic!("{path}: recipient closed before gameplay relay"),
                    _ => {}
                }
            }
            send_signed(
                &mut socket,
                &mut binding,
                &ClientMessage::Ping,
                Some(&fingerprint),
            )
            .await;
            loop {
                let value = next_json_message(&mut socket)
                    .await
                    .expect("Pong after binary");
                assert_ne!(value["type"], "Error", "{path}: {value}");
                if value["type"] == "Pong" {
                    break;
                }
            }
            recipient.close(None).await.unwrap();
            socket.close(None).await.unwrap();
        }
    })
    .await
    .expect("h2 token-binding usability deadline");
}

#[tokio::test]
async fn http2_token_binding_rejects_replay_bad_proofs_and_unsigned_frames() {
    let certificate = fixture("client-101-cert.pem");
    let key = fixture("client-101-key.pem");
    let fingerprint = certificate_fingerprint(&certificate);
    let spoof = certificate_fingerprint(&fixture("client-102-cert.pem"));
    let server = spawn_server(ClientAuthMode::Require, true).await;
    tokio::time::timeout(Duration::from_secs(60), async {
        let mut h2 = H2Client::new(server.port, &certificate, &key).await;
        for path in ["/v2/ws", "/v3/ws"] {
            for case in [
                "bad signature",
                "wrong sequence",
                "unsigned JSON",
                "unsigned binary",
                "certificate mismatch",
                "cross-stream replay",
                "binary sequence replay",
                "JSON replay",
                "missing fingerprint",
            ] {
                // Reuse both the TLS connection and seed: freshness must be per stream.
                let (mut socket, mut binding, challenge) = h2
                    .connect(path, "AgICAgICAgICAgICAgICAg==", Some(&spoof))
                    .await;
                if case == "cross-stream replay" {
                    let proof =
                        signed_message(&mut binding, &h2_authentication(), Some(&fingerprint));
                    let (mut other, other_binding, other_challenge) = h2
                        .connect(path, "AgICAgICAgICAgICAgICAg==", Some(&spoof))
                        .await;
                    assert_ne!(challenge["data"]["nonce"], other_challenge["data"]["nonce"]);
                    assert_ne!(binding.secret, other_binding.secret);
                    other.send(Message::Text(proof.into())).await.unwrap();
                    assert_binding_rejection(&mut other, case).await;
                    socket.close(None).await.unwrap();
                    continue;
                }
                if case == "certificate mismatch" {
                    let proof = signed_message(&mut binding, &h2_authentication(), Some(&spoof));
                    socket.send(Message::Text(proof.into())).await.unwrap();
                    assert_binding_rejection(&mut socket, case).await;
                    continue;
                }
                send_signed(
                    &mut socket,
                    &mut binding,
                    &h2_authentication(),
                    Some(&fingerprint),
                )
                .await;
                next_message_of_type(&mut socket, "Authenticated").await;
                next_message_of_type(&mut socket, "ProtocolInfo").await;
                send_signed(
                    &mut socket,
                    &mut binding,
                    &ClientMessage::JoinRoom {
                        game_name: "h2-rejection-controls".into(),
                        room_code: None,
                        player_name: "h2-control".into(),
                        max_players: Some(2),
                        supports_authority: Some(true),
                        relay_transport: None,
                        password: None,
                        join_only: None,
                    },
                    Some(&fingerprint),
                )
                .await;
                next_message_of_type(&mut socket, "RoomJoined").await;
                let valid_payload =
                    rmp_serde::to_vec_named(&json!({"frame": "valid-binary-control"})).unwrap();
                // Prove this exact payload is usable before varying only its proof.
                socket
                    .send(Message::Binary(
                        signed_binary_message(
                            &mut binding,
                            valid_payload.clone(),
                            Some(&fingerprint),
                        )
                        .into(),
                    ))
                    .await
                    .unwrap();
                send_signed(
                    &mut socket,
                    &mut binding,
                    &ClientMessage::Ping,
                    Some(&fingerprint),
                )
                .await;
                next_message_of_type(&mut socket, "Pong").await;
                let frame = match case {
                    "missing fingerprint" => Message::Text(
                        signed_message(&mut binding, &ClientMessage::Ping, None).into(),
                    ),
                    "JSON replay" => {
                        let proof =
                            signed_message(&mut binding, &ClientMessage::Ping, Some(&fingerprint));
                        socket
                            .send(Message::Text(proof.clone().into()))
                            .await
                            .unwrap();
                        next_message_of_type(&mut socket, "Pong").await;
                        Message::Text(proof.into())
                    }
                    "unsigned JSON" => {
                        Message::Text(serde_json::to_string(&ClientMessage::Ping).unwrap().into())
                    }
                    "unsigned binary" => Message::Binary(valid_payload.clone().into()),
                    "binary sequence replay" => {
                        // Authentication consumed sequence 1 across both encodings.
                        binding.next_sequence = 1;
                        Message::Binary(
                            signed_binary_message(&mut binding, valid_payload, Some(&fingerprint))
                                .into(),
                        )
                    }
                    _ => {
                        if case == "wrong sequence" {
                            binding.next_sequence = 99;
                        }
                        let signed =
                            signed_message(&mut binding, &ClientMessage::Ping, Some(&fingerprint));
                        let mut value: Value = serde_json::from_str(&signed).unwrap();
                        if case == "bad signature" {
                            value["token_binding"]["signature"] =
                                json!(BASE64_STANDARD.encode([0u8; 32]));
                        }
                        Message::Text(value.to_string().into())
                    }
                };
                socket.send(frame).await.unwrap();
                assert_binding_rejection(&mut socket, case).await;
            }
        }
    })
    .await
    .expect("h2 token-binding rejection deadline");
}

#[tokio::test]
async fn http2_token_binding_rejects_missing_and_malformed_application_seeds() {
    let certificate = fixture("client-101-cert.pem");
    let key = fixture("client-101-key.pem");
    let server = spawn_server(ClientAuthMode::Require, true).await;
    tokio::time::timeout(CONNECT_DEADLINE, async {
        let mut h2 = H2Client::new(server.port, &certificate, &key).await;
        for path in ["/v2/ws", "/v3/ws"] {
            for seed in [None, Some("invalid-base64")] {
                let mut request = hyper::Request::builder()
                    .method(hyper::Method::CONNECT)
                    .uri(format!("https://localhost:{}{path}", server.port))
                    .header("sec-websocket-version", "13")
                    .header("sec-websocket-protocol", TOKEN_BINDING_PROTOCOL)
                    .extension(hyper::ext::Protocol::from_static("websocket"));
                if let Some(seed) = seed {
                    request = request.header("x-signalfish-token-binding-key", seed);
                }
                let request = request.body(axum::body::Body::empty()).unwrap();
                assert!(!request.headers().contains_key("sec-websocket-key"));
                let response = h2.sender.send_request(request).await.unwrap();
                assert_eq!(
                    response.status(),
                    hyper::StatusCode::BAD_REQUEST,
                    "{path}: {seed:?}"
                );
            }
        }
    })
    .await
    .expect("h2 invalid seed rejection deadline");
}
