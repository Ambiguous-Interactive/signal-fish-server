//! TLS deployment boundary regressions over the real binary (issue #740,
//! ledger ARM-C035 / ARM-C036).
//!
//! - The shutdown drain choreography (v3 `GoingAway` advisory, coded
//!   `4000 server_shutdown` close, bounded process exit) is pinned over plain
//!   sockets in `close_code_semantics_e2e.rs`. Here the same contract is
//!   pinned over `wss://` against the real TLS server process.
//! - A fallible startup step must abort with a non-zero exit, no
//!   "Server started" announcement, and no listener. The invariant holds by
//!   construction (every fallible step precedes the bind and the start logs
//!   follow it); these regressions pin it.
//!
//! Unix-only: the drain trigger is SIGTERM, and the bind-conflict outcome
//! rests on Unix bind semantics (the server listener sets `SO_REUSEADDR`,
//! which on Windows can bind over a blocker without `SO_EXCLUSIVEADDRUSE`).
//! Windows CI keeps the plain-socket drain and startup pins.

#![cfg(all(feature = "tls", unix))]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::{pem::PemObject, CertificateDer, ServerName};
use serde_json::{json, Value};
use signal_fish_server::protocol::{ClientMessage, ServerMessage};
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, TlsConnector};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

const CONNECT_DEADLINE: Duration = Duration::from_secs(30);
/// Ceiling on every single-frame wait inside the drain test; the bounds under
/// test (1 s grace, 5 s settle) are far below it.
const FRAME_DEADLINE: Duration = Duration::from_secs(15);
/// Ceiling from SIGTERM to process exit: 1 s grace + 5 s settle + TLS
/// shutdown, with runner slack. A default-grace drain (30 s) times this out.
const EXIT_BOUND: Duration = Duration::from_secs(20);
/// Ceiling on a startup-failure run: the binary must abort promptly, not hang.
const FAILURE_EXIT_BOUND: Duration = Duration::from_secs(30);

type TestSocket = WebSocketStream<TlsStream<TcpStream>>;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tls")
        .join(name)
}

fn reserve_port() -> u16 {
    let listener = std::net::TcpListener::bind(("0.0.0.0", 0)).expect("bind port probe");
    listener.local_addr().expect("read port probe").port()
}

/// A spawned server process bound to its workdir. The config, logs, and any
/// PEM material live in one temp directory that dies with the struct.
struct SpawnedServer {
    child: tokio::process::Child,
    port: u16,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    _workdir: tempfile::TempDir,
}

impl SpawnedServer {
    fn stdout(&self) -> String {
        std::fs::read_to_string(&self.stdout_path).unwrap_or_default()
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }

    /// Poll until the process exits or `bound` elapses.
    async fn wait_for_exit(&mut self, bound: Duration) -> std::process::ExitStatus {
        let deadline = tokio::time::Instant::now() + bound;
        loop {
            if let Some(status) = self.child.try_wait().expect("poll server process") {
                return status;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "server process did not exit within {bound:?}\nstdout:\n{}\nstderr:\n{}",
                self.stdout(),
                self.stderr()
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

impl Drop for SpawnedServer {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        if std::thread::panicking() {
            eprintln!(
                "server stdout:\n{}\nserver stderr:\n{}",
                self.stdout(),
                self.stderr()
            );
        }
    }
}

/// Spawn the real binary with the given config JSON in a fresh workdir.
fn spawn_server(config: Value) -> SpawnedServer {
    let port = config
        .get("port")
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .expect("config carries a valid port");
    let workdir = tempfile::tempdir().expect("create server workdir");
    let config_path = workdir.path().join("config.json");
    let stdout_path = workdir.path().join("server.stdout.log");
    let stderr_path = workdir.path().join("server.stderr.log");
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
    // Inherited SIGNAL_FISH* variables (config path, env overrides) must not
    // leak into the spawned server's configuration.
    for (key, _) in std::env::vars_os() {
        if key
            .to_str()
            .is_some_and(|key| key.starts_with("SIGNAL_FISH"))
        {
            command.env_remove(key);
        }
    }
    command.env("SIGNAL_FISH_CONFIG_PATH", &config_path);
    let child = command.spawn().expect("spawn server binary");
    SpawnedServer {
        child,
        port,
        stdout_path,
        stderr_path,
        _workdir: workdir,
    }
}

fn tls_server_config(certificate: &Path, private_key: &Path) -> Value {
    json!({
        "port": reserve_port(),
        "server": { "drain_grace_secs": 1 },
        "security": {
            "enforce_app_id_allowlist": false,
            "require_metrics_auth": false,
            "cors_origins": "*",
            "transport": {
                "tls": {
                    "enabled": true,
                    "certificate_path": certificate,
                    "private_key_path": private_key
                }
            }
        },
        "logging": { "enable_file_logging": false }
    })
}

fn plain_server_config(port: u16) -> Value {
    json!({
        "port": port,
        "security": {
            "enforce_app_id_allowlist": false,
            "require_metrics_auth": false,
            "cors_origins": "*"
        },
        "logging": { "enable_file_logging": false }
    })
}

fn client_config() -> Arc<ClientConfig> {
    let server_certificate =
        CertificateDer::from_pem_file(fixture("cert.pem")).expect("parse server certificate");
    let mut roots = RootCertStore::empty();
    roots
        .add(server_certificate)
        .expect("trust test server certificate");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("configure client TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(config)
}

async fn tls_ready(port: u16) -> bool {
    let ready = async {
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.ok()?;
        TlsConnector::from(client_config())
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

/// Spawn the TLS server and wait until its listener answers a TLS handshake.
/// Each attempt gets a fresh port: the probed port can be stolen between the
/// probe bind and the server bind on a loaded runner.
async fn spawn_ready_tls_server(make_config: impl Fn() -> Value) -> SpawnedServer {
    let mut failures = Vec::new();
    for attempt in 1..=5 {
        let mut server = spawn_server(make_config());
        let deadline = tokio::time::Instant::now() + CONNECT_DEADLINE;
        loop {
            if let Some(status) = server.child.try_wait().expect("poll TLS server") {
                failures.push(format!(
                    "attempt {attempt} on port {} exited {status}\nstdout:\n{}\nstderr:\n{}",
                    server.port,
                    server.stdout(),
                    server.stderr()
                ));
                break;
            }
            if tls_ready(server.port).await {
                return server;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "attempt {attempt}: TLS server did not bind within {CONNECT_DEADLINE:?}\
                 \nstdout:\n{}\nstderr:\n{}",
                server.stdout(),
                server.stderr()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    panic!(
        "TLS server did not bind after 5 fresh-port attempts:\n{}",
        failures.join("\n\n")
    );
}

async fn connect_wss(port: u16) -> TestSocket {
    let tcp = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect TLS socket");
    let tls = TlsConnector::from(client_config())
        .connect(
            ServerName::try_from("localhost").expect("valid test server name"),
            tcp,
        )
        .await
        .expect("complete TLS handshake");
    let request = format!("wss://localhost:{port}/v2/ws")
        .into_client_request()
        .expect("build WebSocket request");
    let (socket, _) = tokio_tungstenite::client_async(request, tls)
        .await
        .expect("WebSocket upgrade over TLS");
    socket
}

/// Next server protocol message, skipping non-text control frames.
async fn next_server_message(socket: &mut TestSocket) -> ServerMessage {
    loop {
        let frame = tokio::time::timeout(FRAME_DEADLINE, socket.next())
            .await
            .expect("timed out waiting for a server frame")
            .expect("connection closed while waiting for a server frame")
            .expect("transport error while waiting for a server frame");
        let Message::Text(text) = frame else {
            continue;
        };
        break serde_json::from_str(&text).expect("valid ServerMessage");
    }
}

/// Drain frames until the server's close frame arrives; return the observed
/// `(code, reason)`. A bare termination is exactly the anti-pattern this
/// suite exists to forbid.
async fn read_close_frame(socket: &mut TestSocket, context: &str) -> (u16, String) {
    loop {
        let frame = tokio::time::timeout(FRAME_DEADLINE, socket.next())
            .await
            .unwrap_or_else(|_| panic!("{context}: timed out waiting for the close frame"))
            .unwrap_or_else(|| panic!("{context}: stream ended with no close frame at all"))
            .unwrap_or_else(|error| {
                panic!("{context}: transport error instead of a semantic close: {error}")
            });
        match frame {
            Message::Close(Some(close)) => {
                return (u16::from(close.code), close.reason.to_string());
            }
            Message::Close(None) => panic!("{context}: server closed with NO close code"),
            _ => continue,
        }
    }
}

/// Drain frames until the v3 `GoingAway` advisory arrives. Unrelated
/// application traffic (pings, relay stats) may interleave; a close before
/// the advisory is a contract violation.
async fn read_going_away(socket: &mut TestSocket) -> (u64, Option<u64>) {
    loop {
        let frame = tokio::time::timeout(FRAME_DEADLINE, socket.next())
            .await
            .expect("timed out waiting for GoingAway")
            .expect("connection closed before GoingAway")
            .expect("transport error while waiting for GoingAway");
        let Message::Text(text) = frame else {
            continue;
        };
        if let Ok(ServerMessage::GoingAway {
            deadline_ms,
            retry_after_secs,
        }) = serde_json::from_str(&text)
        {
            return (deadline_ms, retry_after_secs);
        }
    }
}

async fn authenticate_v3(socket: &mut TestSocket) {
    let auth = ClientMessage::Authenticate {
        app_id: "tls-drain-test".to_string(),
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
            serde_json::to_string(&auth)
                .expect("serialize Authenticate")
                .into(),
        ))
        .await
        .expect("send Authenticate");
    let reply = next_server_message(socket).await;
    assert!(
        matches!(reply, ServerMessage::Authenticated { .. }),
        "authenticate must succeed over wss: {reply:?}"
    );
}

async fn join_room(socket: &mut TestSocket) {
    let join = ClientMessage::JoinRoom {
        game_name: "tls-drain-game".to_string(),
        room_code: Some("TLSDRN".to_string()),
        player_name: "drain-peer".to_string(),
        max_players: Some(4),
        supports_authority: Some(false),
        relay_transport: None,
        password: None,
        join_only: None,
    };
    socket
        .send(Message::Text(
            serde_json::to_string(&join)
                .expect("serialize JoinRoom")
                .into(),
        ))
        .await
        .expect("send JoinRoom");
    loop {
        match next_server_message(socket).await {
            ServerMessage::RoomJoined(_) => return,
            ServerMessage::RoomJoinFailed { reason, .. } => {
                panic!("join failed over wss: {reason}")
            }
            _ => continue,
        }
    }
}

/// The drain choreography holds identically over TLS (ARM-C035): a seated v3
/// client receives the `GoingAway` advisory, then the coded
/// `4000 server_shutdown` close, and the process exits cleanly within the
/// bounded drain budget.
#[tokio::test]
async fn shutdown_drain_over_tls_advises_then_closes_4000_and_exits_bounded() {
    let mut server = spawn_ready_tls_server(|| {
        tls_server_config(&fixture("server-cert.pem"), &fixture("server-key.pem"))
    })
    .await;

    let mut socket = connect_wss(server.port).await;
    authenticate_v3(&mut socket).await;
    join_room(&mut socket).await;

    let sigterm_unix_ms = unix_epoch_ms_now();
    // tokio's process API offers only SIGKILL, and `unsafe` is forbidden in
    // this crate, so deliver SIGTERM through the system `kill` binary.
    let pid = server.child.id().expect("server child pid");
    let delivered = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .expect("run kill -TERM");
    assert!(
        delivered.success(),
        "SIGTERM must be delivered to the server process"
    );

    // 1. The best-effort v3 advisory arrives first, anchored at the drain.
    let (deadline_ms, retry_after_secs) = read_going_away(&mut socket).await;
    assert!(
        deadline_ms > sigterm_unix_ms,
        "the GoingAway deadline must be anchored after the drain signal \
         (signal at {sigterm_unix_ms}, deadline {deadline_ms})"
    );
    assert!(
        deadline_ms.saturating_sub(sigterm_unix_ms) < 15_000,
        "the GoingAway deadline must track the 1 s test grace, not a \
         default (signal at {sigterm_unix_ms}, deadline {deadline_ms})"
    );
    assert_eq!(
        retry_after_secs,
        Some(1),
        "retry_after must mirror the configured 1 s drain grace"
    );

    // 2. The authoritative signal: the coded close itself.
    let (code, reason) = read_close_frame(&mut socket, "TLS shutdown drain").await;
    assert_eq!(code, 4000, "shutdown must close with 4000 ({reason})");
    assert_eq!(reason, "server_shutdown");
    drop(socket);

    // 3. The process must exit cleanly within the bounded budget.
    let status = server.wait_for_exit(EXIT_BOUND).await;
    assert!(
        status.success(),
        "a completed drain must exit cleanly (status {status})"
    );
}

/// A bind conflict aborts with a non-zero exit and no "Server started"
/// announcement (ARM-C036): a server that lost the bind must never announce
/// a successful start.
#[tokio::test]
async fn port_bind_conflict_exits_nonzero_without_start_announcement() {
    let port = reserve_port();
    let blocker = std::net::TcpListener::bind(("127.0.0.1", port)).expect("occupy the port");
    let mut server = spawn_server(plain_server_config(port));

    let status = server.wait_for_exit(FAILURE_EXIT_BOUND).await;
    assert!(
        !status.success(),
        "a bind conflict must abort the process with a non-zero exit"
    );
    let combined = format!("{}{}", server.stdout(), server.stderr());
    assert!(
        combined.contains("Address already in use"),
        "the abort must be attributed to the occupied port:\n{combined}"
    );
    assert!(
        !combined.contains("Server started"),
        "a server that failed to bind must not announce a successful start:\n{combined}"
    );
    drop(blocker);
}

/// TLS material that fails to parse aborts before the bind (ARM-C036): the
/// process exits non-zero, never announces a start, and leaves no listener
/// reachable on the port.
#[tokio::test]
async fn invalid_tls_pem_exits_nonzero_without_announcement_or_listener() {
    let workdir = tempfile::tempdir().expect("create workdir");
    let bad_cert = workdir.path().join("bad-cert.pem");
    let bad_key = workdir.path().join("bad-key.pem");
    std::fs::write(&bad_cert, "this is not a PEM certificate\n").expect("write bad cert");
    std::fs::write(&bad_key, "this is not a PEM key\n").expect("write bad key");

    let mut server = spawn_server(tls_server_config(&bad_cert, &bad_key));

    let status = server.wait_for_exit(FAILURE_EXIT_BOUND).await;
    assert!(
        !status.success(),
        "invalid TLS material must abort the process with a non-zero exit"
    );
    let combined = format!("{}{}", server.stdout(), server.stderr());
    assert!(
        !combined.contains("Server started"),
        "a server whose TLS material failed must not announce a successful start:\n{combined}"
    );
    assert!(
        combined.contains("failed to initialize TLS configuration"),
        "the abort must be attributed to the TLS material:\n{combined}"
    );

    // No half-started server stays reachable on the port.
    let probe = TcpStream::connect(("127.0.0.1", server.port)).await;
    assert!(
        probe.is_err(),
        "a server aborted before bind must leave no listener on its port"
    );
}

fn unix_epoch_ms_now() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
