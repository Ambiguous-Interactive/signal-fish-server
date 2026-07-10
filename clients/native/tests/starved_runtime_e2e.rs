//! Starved-runtime conformance matrix: pins the documented "clients driving
//! async runtimes must continuously poll/drive their connection" requirement
//! (`docs/protocol.md`, Delivery reliability and backpressure) as an
//! EXECUTABLE boundary, using the native reference client's fault-injection
//! flags (`--runtime current`, `--tick-stall-ms`) — the #131 reporter's
//! actual client-side failure mode (a game loop hogging the runtime).
//!
//! Matrix cells:
//!
//! 1. `current_runtime_without_stall_completes_healthy_relay` — a properly
//!    driven current-thread runtime is fully conformant: the relay session
//!    completes, zero evictions.
//! 2. `stall_below_timeout_absorbs_with_zero_loss` — a stall well inside
//!    `websocket.slow_consumer_timeout_ms` is absorbed by the server's
//!    backpressure: the client's received stream is gap-free, nothing is
//!    dropped, nobody is evicted (backpressure MAY trigger; not asserted —
//!    whether the queue ever crosses full depends on scheduling).
//! 3. `stall_above_timeout_is_evicted_loudly_and_room_flows` — a stall far
//!    beyond the timeout under a flooding peer gets the client evicted
//!    LOUDLY (slow-consumer metric, `PlayerLeft`, the client's own error/exit)
//!    while the room keeps flowing gap-free for the healthy witness.
//!
//! This suite lives in the CLIENT crate (not the server repo's tests/)
//! deliberately: the client binary is reachable here via
//! `CARGO_BIN_EXE_signal-fish-reference-native`, and the proven interop
//! pattern for the server binary (`SIGNAL_FISH_SERVER_BIN`, see
//! `tests/harness/mod.rs`) already exists — no nested `cargo build`
//! machinery. The server is spawned locally (not through `harness::spawn_server`)
//! because these scenarios need per-test `websocket.*` delivery knobs the
//! shared harness config does not expose. Server-side metrics are asserted by
//! scraping `/metrics/prom` with a hand-rolled HTTP GET (this crate has no
//! HTTP-client dependency), mirroring the harness's health probe.
//!
//! `#[ignore]`: nightly-only (activated by the server repo's
//! `verification-nightly.yml` with `--ignored`) — each cell floods a real
//! server process for tens of seconds, which is deliberately kept out of the
//! PR-lane interop run. `#[serial_test::serial]` keeps the cells from
//! CPU-starving each other under plain `cargo test`. All waits are
//! event/metric-driven polls against generous ceilings (zero-flakiness
//! policy), never sleeps as synchronization.

mod harness;

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use signal_fish_server::protocol::{ClientMessage, PlayerId, ServerMessage};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::TcpStream;
use tokio::process::{Child, ChildStdout, Command};
use tokio::time::Instant;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, tokio_tungstenite::tungstenite::Message>;
type WsReceiver = futures_util::stream::SplitStream<WsStream>;

use tokio_tungstenite::tungstenite::Message;

const GAME_NAME: &str = "starved-runtime";
const HEALTH_DEADLINE: Duration = Duration::from_secs(30);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(100);
const HTTP_IO_TIMEOUT: Duration = Duration::from_secs(5);
const SPAWN_ATTEMPTS: usize = 3;
/// Generous per-event ceiling: a CI scheduling budget, not an expected wait.
const EVENT_DEADLINE: Duration = Duration::from_secs(30);
/// Whole-phase ceiling for flood/drain sections.
const PHASE_DEADLINE: Duration = Duration::from_secs(90);

/// Per-test websocket delivery knobs for the spawned server's temp config.
#[derive(Debug, Clone, Copy)]
struct DeliveryKnobs {
    send_queue_capacity: usize,
    slow_consumer_timeout_ms: u64,
}

// ---------------------------------------------------------------------------
// Server process (local spawn with delivery knobs; mirrors harness/mod.rs).
// ---------------------------------------------------------------------------

/// Guard around the spawned server binary (kill-on-drop, like the harness).
struct ServerProcess {
    child: Option<Child>,
    port: u16,
    stderr_path: PathBuf,
    _workdir: tempfile::TempDir,
}

impl ServerProcess {
    fn v3_ws_url(&self) -> String {
        format!("ws://127.0.0.1:{}/v3/ws", self.port)
    }

    fn captured_stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr_path)
            .unwrap_or_else(|error| format!("<failed to read server stderr: {error}>"))
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if let Err(error) = child.start_kill() {
                eprintln!("failed to kill server process on drop: {error}");
            }
        }
    }
}

fn reserve_port() -> u16 {
    let probe = std::net::TcpListener::bind(("0.0.0.0", 0)).expect("bind port probe");
    let port = probe.local_addr().expect("port probe local_addr").port();
    drop(probe);
    port
}

/// Spawn the server with this suite's delivery knobs, retrying with fresh
/// ports (absorbs the reserve-release-spawn race).
async fn spawn_server_with_knobs(knobs: DeliveryKnobs) -> ServerProcess {
    let mut failures = Vec::new();
    for attempt in 1..=SPAWN_ATTEMPTS {
        let port = reserve_port();
        match try_spawn_server(port, knobs).await {
            Ok(server) => return server,
            Err(failure) => failures.push(format!("attempt {attempt} (port {port}): {failure}")),
        }
    }
    panic!(
        "server binary failed to become healthy after {SPAWN_ATTEMPTS} attempts:\n{}",
        failures.join("\n")
    );
}

async fn try_spawn_server(port: u16, knobs: DeliveryKnobs) -> Result<ServerProcess, String> {
    let workdir = tempfile::tempdir().expect("create temp workdir");
    let config_path = workdir.path().join("server-config.json");
    // The harness config (tests/harness/mod.rs) plus the delivery knobs and
    // metrics auth off (this suite scrapes /metrics/prom).
    let config = json!({
        "port": port,
        "server": {
            "enable_reconnection": true,
            "reconnection_window": 300
        },
        "security": {
            "require_websocket_auth": false,
            "require_metrics_auth": false,
            "cors_origins": "*"
        },
        "protocol": {
            "sdk_compatibility": { "enforce": false }
        },
        "session": {
            "default_topology": "relay"
        },
        "websocket": {
            "send_queue_capacity": knobs.send_queue_capacity,
            "slow_consumer_timeout_ms": knobs.slow_consumer_timeout_ms
        },
        "rate_limit": {
            "max_room_creations": 100,
            "time_window": 60,
            "max_join_attempts": 100,
            "max_signals": 100000,
            "max_signal_errors": 1000
        },
        "logging": {
            "enable_file_logging": false
        }
    });
    std::fs::write(
        &config_path,
        serde_json::to_vec_pretty(&config).expect("serialize server config"),
    )
    .expect("write server config");

    let stdout_path = workdir.path().join("server-stdout.log");
    let stderr_path = workdir.path().join("server-stderr.log");
    let stdout_file = std::fs::File::create(&stdout_path).expect("create stdout capture");
    let stderr_file = std::fs::File::create(&stderr_path).expect("create stderr capture");

    let mut command = Command::new(harness::server_binary_path());
    command
        .current_dir(workdir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .kill_on_drop(true);
    // Scrub inherited SIGNAL_FISH* config overrides (loader merges env last).
    for (key, _) in std::env::vars_os() {
        if key
            .to_str()
            .is_some_and(|key| key.starts_with("SIGNAL_FISH"))
        {
            command.env_remove(&key);
        }
    }
    command.env("SIGNAL_FISH_CONFIG_PATH", &config_path);
    command.env("SIGNAL_FISH__PORT", port.to_string());

    let child = command.spawn().expect("spawn the server binary");
    let mut server = ServerProcess {
        child: Some(child),
        port,
        stderr_path,
        _workdir: workdir,
    };

    match wait_until_healthy(&mut server).await {
        Ok(()) => Ok(server),
        Err(reason) => Err(format!(
            "{reason}; server stderr:\n{}",
            server.captured_stderr()
        )),
    }
}

async fn wait_until_healthy(server: &mut ServerProcess) -> Result<(), String> {
    let deadline = Instant::now() + HEALTH_DEADLINE;
    loop {
        let child = server
            .child
            .as_mut()
            .ok_or_else(|| "health poll requires a live child".to_string())?;
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(format!("server process exited early with {status}"));
            }
            Ok(None) => {}
            Err(error) => return Err(format!("failed to poll server process: {error}")),
        }

        if http_get_ok(server.port, "/v2/health").await.is_some() {
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Err(format!(
                "health endpoint on port {} not ready within {HEALTH_DEADLINE:?}",
                server.port
            ));
        }
        tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
    }
}

/// One bounded hand-rolled `GET` (no HTTP-client dependency, mirroring the
/// harness health probe); `Some(body)` on a 200, `None` on any failure.
async fn http_get_ok(port: u16, path: &str) -> Option<String> {
    let connect = tokio::time::timeout(HTTP_IO_TIMEOUT, TcpStream::connect(("127.0.0.1", port)));
    let Ok(Ok(mut stream)) = connect.await else {
        return None;
    };
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    let write = tokio::time::timeout(HTTP_IO_TIMEOUT, stream.write_all(request.as_bytes()));
    if !matches!(write.await, Ok(Ok(()))) {
        return None;
    }
    let mut response = Vec::new();
    let read = tokio::time::timeout(HTTP_IO_TIMEOUT, stream.read_to_end(&mut response));
    if !matches!(read.await, Ok(Ok(_))) {
        return None;
    }
    let response = String::from_utf8_lossy(&response).into_owned();
    if !(response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200")) {
        return None;
    }
    let (_headers, body) = response.split_once("\r\n\r\n")?;
    Some(body.to_string())
}

// ---------------------------------------------------------------------------
// Prometheus scraping (the server crate's tests have a shared helper for
// this; this crate cannot import it, so the parse is mirrored here).
// ---------------------------------------------------------------------------

/// Fetch `/metrics/prom` and parse the single un-labelled sample `name`.
/// Panics on a missing endpoint or sample — a silently-defaulted counter
/// would let a contract violation pass unnoticed.
async fn scrape_counter(port: u16, name: &str) -> u64 {
    let body = http_get_ok(port, "/metrics/prom")
        .await
        .unwrap_or_else(|| panic!("scraping /metrics/prom on port {port} failed"));
    for line in body.lines() {
        if line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(sample_name), Some(raw_value)) = (parts.next(), parts.next()) else {
            continue;
        };
        if sample_name != name {
            continue;
        }
        let value: f64 = raw_value
            .parse()
            .unwrap_or_else(|error| panic!("sample {name} has non-numeric value: {error}"));
        assert!(
            value >= 0.0 && value.fract() == 0.0,
            "sample {name} must be a non-negative integer, got {value}"
        );
        return value as u64;
    }
    panic!("sample {name} not found in the scraped exposition:\n{body}");
}

// ---------------------------------------------------------------------------
// Native client process (local spawn: this suite's flag surface — runtime,
// tick-stall, run windows — is wider than harness::ClientSpec exposes).
// ---------------------------------------------------------------------------

/// Everything one starved-runtime client invocation needs.
struct StarvedClientSpec<'a> {
    name: &'a str,
    /// `None` creates the room; `Some(code)` joins by code.
    join_code: Option<&'a str>,
    peers: usize,
    relay_payload: Option<&'a str>,
    /// `--runtime` token (`multi` / `current`).
    runtime: &'a str,
    tick_stall_ms: u64,
    run_for_secs: u64,
}

/// Guard around one spawned reference client; collects its JSONL events.
struct ClientProcess {
    name: String,
    child: Option<Child>,
    lines: Lines<BufReader<ChildStdout>>,
    events: Vec<Value>,
    stderr_path: PathBuf,
}

impl Drop for ClientProcess {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if let Err(error) = child.start_kill() {
                eprintln!("failed to kill client {} on drop: {error}", self.name);
            }
        }
    }
}

fn spawn_starved_client(
    server: &ServerProcess,
    spec: &StarvedClientSpec<'_>,
    workdir: &std::path::Path,
) -> ClientProcess {
    let stderr_path = workdir.join(format!("client-{}-stderr.log", spec.name));
    let stderr_file = std::fs::File::create(&stderr_path).expect("create client stderr capture");

    let mut command = Command::new(env!("CARGO_BIN_EXE_signal-fish-reference-native"));
    command
        .arg("--server-url")
        .arg(server.v3_ws_url())
        .arg("--game-name")
        .arg(GAME_NAME)
        .arg("--player-name")
        .arg(spec.name)
        .arg("--peers")
        .arg(spec.peers.to_string())
        .arg("--runtime")
        .arg(spec.runtime)
        .arg("--tick-stall-ms")
        .arg(spec.tick_stall_ms.to_string())
        .arg("--run-for-secs")
        .arg(spec.run_for_secs.to_string())
        .arg("--max-runtime-secs")
        .arg((spec.run_for_secs + 60).to_string());
    match spec.join_code {
        Some(code) => {
            command.arg("--join-code").arg(code);
        }
        None => {
            command.arg("--create-room");
        }
    }
    if let Some(payload) = spec.relay_payload {
        command.arg("--relay-payload").arg(payload);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr_file))
        .env("RUST_LOG", "info")
        .kill_on_drop(true);

    let mut child = command.spawn().expect("spawn the reference client binary");
    let stdout = child.stdout.take().expect("client stdout is piped");
    ClientProcess {
        name: spec.name.to_string(),
        child: Some(child),
        lines: BufReader::new(stdout).lines(),
        events: Vec::new(),
        stderr_path,
    }
}

impl ClientProcess {
    /// Read events until one named `event_name` arrives; panics with
    /// diagnostics on timeout, EOF, or a non-JSONL line.
    async fn await_event(&mut self, event_name: &str, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            match self
                .next_event_before(deadline, &format!("awaiting `{event_name}`"))
                .await
            {
                Some(event) => {
                    if event.get("event").and_then(Value::as_str) == Some(event_name) {
                        return event;
                    }
                }
                None => panic!(
                    "client {}: stdout ended before `{event_name}`;\n{}",
                    self.name,
                    self.diagnostics()
                ),
            }
        }
    }

    async fn next_event_before(&mut self, deadline: Instant, context: &str) -> Option<Value> {
        let read = tokio::time::timeout_at(deadline, self.lines.next_line()).await;
        let line = read
            .unwrap_or_else(|_elapsed| {
                panic!(
                    "client {}: timed out {context};\n{}",
                    self.name,
                    self.diagnostics()
                )
            })
            .unwrap_or_else(|error| {
                panic!(
                    "client {}: stdout read error: {error};\n{}",
                    self.name,
                    self.diagnostics()
                )
            })?;
        let event: Value = serde_json::from_str(&line).unwrap_or_else(|error| {
            panic!(
                "client {}: stdout line is not a JSON event ({error}): {line}",
                self.name
            )
        });
        self.events.push(event.clone());
        Some(event)
    }

    /// Drain every remaining event to EOF, reap the child, and return its
    /// exit code (panics on signal-termination — nothing in this suite kills
    /// clients).
    async fn drain_to_exit(&mut self, timeout: Duration) -> i32 {
        let deadline = Instant::now() + timeout;
        while let Some(_event) = self
            .next_event_before(deadline, "draining events to process exit")
            .await
        {
            // Recorded by next_event_before.
        }
        let mut child = self.child.take().expect("client child already reaped");
        let status = tokio::time::timeout_at(deadline, child.wait())
            .await
            .unwrap_or_else(|_elapsed| {
                panic!(
                    "client {}: stdout closed but the process did not exit;\n{}",
                    self.name,
                    self.diagnostics()
                )
            })
            .unwrap_or_else(|error| {
                panic!("client {}: failed to reap process: {error}", self.name)
            });
        status.code().unwrap_or_else(|| {
            panic!(
                "client {}: terminated by signal ({status});\n{}",
                self.name,
                self.diagnostics()
            )
        })
    }

    /// Assert the startup `connected` event echoed the intended runtime /
    /// stall configuration (the fault shape was actually in effect).
    fn assert_connected_configuration(&self, runtime: &str, tick_stall_ms: u64) {
        let connected = self
            .events
            .iter()
            .find(|event| event.get("event").and_then(Value::as_str) == Some("connected"))
            .unwrap_or_else(|| {
                panic!(
                    "client {}: no `connected` event recorded;\n{}",
                    self.name,
                    self.diagnostics()
                )
            });
        assert_eq!(
            connected.get("runtime").and_then(Value::as_str),
            Some(runtime),
            "client {}: connected event must echo --runtime",
            self.name
        );
        assert_eq!(
            connected.get("tick_stall_ms").and_then(Value::as_u64),
            Some(tick_stall_ms),
            "client {}: connected event must echo --tick-stall-ms",
            self.name
        );
    }

    /// All `error`-event messages recorded so far.
    fn error_messages(&self) -> Vec<String> {
        self.events
            .iter()
            .filter(|event| event.get("event").and_then(Value::as_str) == Some("error"))
            .filter_map(|event| event.get("message").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    }

    /// Ledger seqs of every recorded `game_data_received` payload, in event
    /// order.
    fn received_ledger_seqs(&self) -> Vec<u64> {
        self.events
            .iter()
            .filter(|event| {
                event.get("event").and_then(Value::as_str) == Some("game_data_received")
            })
            .map(|event| {
                event
                    .get("payload")
                    .and_then(|payload| payload.get("seq"))
                    .and_then(Value::as_u64)
                    .unwrap_or_else(|| {
                        panic!(
                            "client {}: game_data_received without a ledger seq: {event}",
                            self.name
                        )
                    })
            })
            .collect()
    }

    fn diagnostics(&self) -> String {
        const EVENT_TAIL: usize = 12;
        let tail_start = self.events.len().saturating_sub(EVENT_TAIL);
        let recent: Vec<String> = self.events[tail_start..]
            .iter()
            .map(|event| event.to_string())
            .collect();
        let stderr = std::fs::read_to_string(&self.stderr_path)
            .unwrap_or_else(|error| format!("<failed to read stderr: {error}>"));
        format!(
            "last {} events:\n{}\n--- client {} stderr ---\n{}",
            recent.len(),
            recent.join("\n"),
            self.name,
            stderr
        )
    }
}

// ---------------------------------------------------------------------------
// Raw in-test WebSocket helpers (auth is disabled in the temp config, so —
// like the server repo's relay suites — raw clients join without an
// Authenticate handshake).
// ---------------------------------------------------------------------------

async fn connect_raw(port: u16) -> (WsSink, WsReceiver) {
    let url = format!("ws://127.0.0.1:{port}/v2/ws");
    let (stream, _response) =
        tokio::time::timeout(EVENT_DEADLINE, tokio_tungstenite::connect_async(&url))
            .await
            .expect("websocket connect timed out")
            .expect("websocket connect failed");
    stream.split()
}

async fn join_room_raw(
    sink: &mut WsSink,
    receiver: &mut WsReceiver,
    room_code: &str,
    player_name: &str,
) -> PlayerId {
    let join = ClientMessage::JoinRoom {
        game_name: GAME_NAME.to_string(),
        room_code: Some(room_code.to_string()),
        player_name: player_name.to_string(),
        // Match the native creator's capacity (it sets max_players = --peers);
        // the server keeps the creator's capacity for join-by-code.
        max_players: Some(8),
        supports_authority: Some(false),
        relay_transport: None,
    };
    let json = serde_json::to_string(&join).expect("serialize JoinRoom");
    sink.send(Message::Text(json.into()))
        .await
        .expect("send JoinRoom");

    loop {
        let frame = tokio::time::timeout(EVENT_DEADLINE, receiver.next())
            .await
            .expect("timed out waiting for RoomJoined")
            .expect("connection closed while joining room")
            .expect("websocket error while joining room");
        let Message::Text(text) = frame else {
            continue;
        };
        let message: ServerMessage = serde_json::from_str(&text).expect("valid ServerMessage");
        match message {
            ServerMessage::RoomJoined(payload) => return payload.player_id,
            ServerMessage::RoomJoinFailed { reason, .. } => {
                panic!("room join failed for {player_name}: {reason}")
            }
            _ => continue,
        }
    }
}

/// Send `count` ledger-shaped GameData messages (`{"ledger_sender","seq",
/// "padding"}` — the same shape the server repo's DeliveryLedger uses) and
/// return the exclusive upper bound of the emitted seq range.
async fn send_ledger_burst(
    sink: &mut WsSink,
    next_seq: &mut u64,
    count: u64,
    padding_bytes: usize,
) {
    let padding = "x".repeat(padding_bytes);
    for _ in 0..count {
        let message = ClientMessage::GameData {
            class: None,
            key: None,
            data: json!({
                "ledger_sender": "RawSender",
                "seq": *next_seq,
                "padding": padding.as_str(),
            }),
        };
        *next_seq += 1;
        let json = serde_json::to_string(&message).expect("serialize GameData");
        sink.send(Message::Text(json.into()))
            .await
            .expect("send GameData burst frame");
    }
}

fn assert_gap_free_complete(seqs: &[u64], expected: u64, who: &str) {
    assert_eq!(
        seqs.len() as u64,
        expected,
        "{who}: expected the complete {expected}-message stream, got {}",
        seqs.len()
    );
    for (position, seq) in seqs.iter().enumerate() {
        assert_eq!(
            *seq, position as u64,
            "{who}: stream is not gap-free in order (position {position} carries seq {seq})"
        );
    }
}

fn player_id_from_room_joined(event: &Value, who: &str) -> PlayerId {
    let raw = event
        .get("player_id")
        .cloned()
        .unwrap_or_else(|| panic!("{who}: room_joined event without player_id: {event}"));
    serde_json::from_value(raw)
        .unwrap_or_else(|error| panic!("{who}: room_joined player_id is not a PlayerId: {error}"))
}

// ---------------------------------------------------------------------------
// Matrix cells.
// ---------------------------------------------------------------------------

/// Cell 1 — `{runtime = current, stall = 0}`: a current-thread runtime that
/// is continuously driven is fully conformant. Two native clients complete a
/// whole relay session (lobby -> ready -> StartGame -> GameStarting ->
/// mutual `--relay-payload` exchange) and exit 0; the server records zero
/// slow-consumer disconnects.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
#[ignore = "nightly starved-runtime matrix (verification-nightly.yml); floods/holds a real server process"]
async fn current_runtime_without_stall_completes_healthy_relay() {
    let server = spawn_server_with_knobs(DeliveryKnobs {
        send_queue_capacity: 1024,
        slow_consumer_timeout_ms: 5_000,
    })
    .await;
    let workdir = tempfile::tempdir().expect("create client workdir");

    let mut creator = spawn_starved_client(
        &server,
        &StarvedClientSpec {
            name: "CurrentA",
            join_code: None,
            peers: 2,
            relay_payload: Some("hello-from-a"),
            runtime: "current",
            tick_stall_ms: 0,
            run_for_secs: 45,
        },
        workdir.path(),
    );
    let created = creator.await_event("room_created", EVENT_DEADLINE).await;
    let room_code = created
        .get("room_code")
        .and_then(Value::as_str)
        .expect("room_created carries the code")
        .to_string();

    let mut joiner = spawn_starved_client(
        &server,
        &StarvedClientSpec {
            name: "CurrentB",
            join_code: Some(&room_code),
            peers: 2,
            relay_payload: Some("hello-from-b"),
            runtime: "current",
            tick_stall_ms: 0,
            run_for_secs: 45,
        },
        workdir.path(),
    );

    let creator_code = creator.drain_to_exit(PHASE_DEADLINE).await;
    let joiner_code = joiner.drain_to_exit(PHASE_DEADLINE).await;
    assert_eq!(
        creator_code,
        0,
        "a properly driven current-thread client must complete the session; {}",
        creator.diagnostics()
    );
    assert_eq!(
        joiner_code,
        0,
        "a properly driven current-thread client must complete the session; {}",
        joiner.diagnostics()
    );
    creator.assert_connected_configuration("current", 0);
    joiner.assert_connected_configuration("current", 0);

    let evictions = scrape_counter(
        server.port,
        "signal_fish_websocket_slow_consumer_disconnects_total",
    )
    .await;
    assert_eq!(
        evictions, 0,
        "a healthy current-thread session must record zero evictions"
    );
}

/// Cell 2 — `{stall = 50ms, timeout = 5000ms}`: a stall well inside the
/// grace window is absorbed by backpressure. The stalled client's received
/// stream is gap-free and complete, nothing is dropped, nobody is evicted.
/// Backpressure may or may not have triggered (scheduling-dependent) and is
/// deliberately NOT asserted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
#[ignore = "nightly starved-runtime matrix (verification-nightly.yml); floods/holds a real server process"]
async fn stall_below_timeout_absorbs_with_zero_loss() {
    const BURST_MESSAGES: u64 = 100;
    const PADDING_BYTES: usize = 256;
    /// The client's soft window: long enough to drain the whole burst at
    /// ~50ms per input with generous margin (a ceiling — it exits at the
    /// window regardless, and the burst is complete long before it).
    const RUN_FOR_SECS: u64 = 25;

    let server = spawn_server_with_knobs(DeliveryKnobs {
        send_queue_capacity: 16,
        slow_consumer_timeout_ms: 5_000,
    })
    .await;
    let workdir = tempfile::tempdir().expect("create client workdir");

    let mut stalled = spawn_starved_client(
        &server,
        &StarvedClientSpec {
            name: "MildStall",
            join_code: None,
            peers: 2,
            relay_payload: None,
            runtime: "current",
            tick_stall_ms: 50,
            run_for_secs: RUN_FOR_SECS,
        },
        workdir.path(),
    );
    let created = stalled.await_event("room_created", EVENT_DEADLINE).await;
    let room_code = created
        .get("room_code")
        .and_then(Value::as_str)
        .expect("room_created carries the code")
        .to_string();

    let (mut sender_sink, mut sender_rx) = connect_raw(server.port).await;
    let _sender_id = join_room_raw(&mut sender_sink, &mut sender_rx, &room_code, "RawSender").await;

    // Unpaced burst into the 16-slot queue: the 50ms-per-input consumer
    // repeatedly fills it, but always drains far inside the 5s window.
    let mut next_seq = 0u64;
    send_ledger_burst(
        &mut sender_sink,
        &mut next_seq,
        BURST_MESSAGES,
        PADDING_BYTES,
    )
    .await;

    // The client exits at its soft window with criteria unmet (no game ever
    // starts here) — exit 1 is the EXPECTED loud outcome, asserted exactly.
    let exit_code = stalled.drain_to_exit(PHASE_DEADLINE).await;
    assert_eq!(
        exit_code,
        1,
        "the stalled client exits via its --run-for-secs window; {}",
        stalled.diagnostics()
    );
    stalled.assert_connected_configuration("current", 50);

    // Zero loss: the full burst surfaced, gap-free and in order, in the
    // stalled client's own event stream.
    let seqs = stalled.received_ledger_seqs();
    assert_gap_free_complete(&seqs, BURST_MESSAGES, "stalled client");

    // Absorbed means absorbed: nothing dropped, nobody evicted.
    let evictions = scrape_counter(
        server.port,
        "signal_fish_websocket_slow_consumer_disconnects_total",
    )
    .await;
    let dropped = scrape_counter(server.port, "signal_fish_websocket_messages_dropped_total").await;
    assert_eq!(
        evictions, 0,
        "a consumer draining inside the grace window must never be evicted"
    );
    assert_eq!(
        dropped, 0,
        "nothing may be dropped while the consumer keeps draining"
    );
}

/// Cell 3 — `{stall = 800ms ≫ timeout = 300ms}` with a flooding peer: the
/// starved client is evicted LOUDLY (slow-consumer counter, `PlayerLeft` to
/// the room, the client's own error event and nonzero exit) while the
/// healthy witness keeps receiving the complete gap-free stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
#[ignore = "nightly starved-runtime matrix (verification-nightly.yml); floods/holds a real server process"]
async fn stall_above_timeout_is_evicted_loudly_and_room_flows() {
    /// Large frames so the flood saturates the starved client's kernel path
    /// with FEW messages (bounding how long its post-eviction drain of
    /// buffered frames can take at 800ms per input).
    const PADDING_BYTES: usize = 24 * 1024;
    const FLOOD_CHUNK: u64 = 10;
    /// Loud cap: reached only if the eviction path is broken.
    const FLOOD_CAP: u64 = 2_000;

    let server = spawn_server_with_knobs(DeliveryKnobs {
        send_queue_capacity: 4,
        slow_consumer_timeout_ms: 300,
    })
    .await;
    let workdir = tempfile::tempdir().expect("create client workdir");

    // The starved victim creates the room (capacity --peers = 3).
    let mut victim = spawn_starved_client(
        &server,
        &StarvedClientSpec {
            name: "Starved",
            join_code: None,
            peers: 3,
            relay_payload: None,
            runtime: "current",
            tick_stall_ms: 800,
            run_for_secs: 30,
        },
        workdir.path(),
    );
    let created = victim.await_event("room_created", EVENT_DEADLINE).await;
    let room_code = created
        .get("room_code")
        .and_then(Value::as_str)
        .expect("room_created carries the code")
        .to_string();
    let joined = victim.await_event("room_joined", EVENT_DEADLINE).await;
    let victim_id = player_id_from_room_joined(&joined, "Starved");

    let (mut sender_sink, mut sender_rx) = connect_raw(server.port).await;
    let (mut healthy_sink, mut healthy_rx) = connect_raw(server.port).await;
    let _sender_id = join_room_raw(&mut sender_sink, &mut sender_rx, &room_code, "RawSender").await;
    let _healthy_id =
        join_room_raw(&mut healthy_sink, &mut healthy_rx, &room_code, "Healthy").await;

    // Healthy witness drain: records seqs until the final total is known and
    // the victim's PlayerLeft was observed.
    let (totals_tx, mut totals_rx) = tokio::sync::watch::channel::<Option<u64>>(None);
    let healthy_drain = tokio::spawn(async move {
        let mut seqs: Vec<u64> = Vec::new();
        let mut saw_victim_leave = false;
        loop {
            let total = *totals_rx.borrow_and_update();
            if let Some(total) = total {
                if seqs.len() as u64 >= total && saw_victim_leave {
                    return seqs;
                }
            }
            tokio::select! {
                frame = healthy_rx.next() => {
                    let frame = frame
                        .expect("healthy witness closed mid-drain")
                        .expect("healthy witness websocket error mid-drain");
                    let Message::Text(text) = frame else { continue };
                    let message: ServerMessage =
                        serde_json::from_str(&text).expect("valid ServerMessage");
                    match message {
                        ServerMessage::GameData { data, .. } => {
                            let seq = data
                                .get("seq")
                                .and_then(Value::as_u64)
                                .expect("GameData payload carries a numeric seq");
                            seqs.push(seq);
                        }
                        ServerMessage::PlayerLeft { player_id, .. } if player_id == victim_id => {
                            saw_victim_leave = true;
                        }
                        ServerMessage::Error { message, error_code } => panic!(
                            "healthy witness got a server error: {message} ({error_code:?})"
                        ),
                        _ => {}
                    }
                }
                changed = totals_rx.changed() => {
                    changed.expect("sent-totals channel dropped before totals were set");
                }
            }
        }
    });

    // Metric-driven flood: pump until the server demonstrably evicted the
    // starved consumer (the cap only bites when eviction is broken).
    let mut next_seq = 0u64;
    let flood = async {
        loop {
            let evictions = scrape_counter(
                server.port,
                "signal_fish_websocket_slow_consumer_disconnects_total",
            )
            .await;
            if evictions >= 1 {
                return;
            }
            assert!(
                next_seq < FLOOD_CAP,
                "flood cap reached without a slow-consumer disconnect (sent {next_seq})"
            );
            send_ledger_burst(&mut sender_sink, &mut next_seq, FLOOD_CHUNK, PADDING_BYTES).await;
        }
    };
    tokio::time::timeout(PHASE_DEADLINE, flood)
        .await
        .expect("the starved client was never evicted within the phase deadline");
    let total_sent = next_seq;
    totals_tx.send_replace(Some(total_sent));

    // The room keeps flowing: the healthy witness holds the complete stream
    // and observed the victim's eviction as a PlayerLeft.
    let seqs = tokio::time::timeout(PHASE_DEADLINE, healthy_drain)
        .await
        .expect("healthy drain exceeded the phase deadline")
        .expect("healthy drain task panicked");
    assert_gap_free_complete(&seqs, total_sent, "healthy witness");

    // The starved client's own run ends loudly: a nonzero exit with at least
    // one error event. Which loud path wins is timing-dependent by nature —
    // it either observes the server-initiated close while draining its
    // backlog (exit 3) or its soft run window expires mid-backlog (exit 1);
    // both are attributable failures, never a hang or a silent 0.
    let exit_code = victim.drain_to_exit(PHASE_DEADLINE).await;
    assert!(
        exit_code == 1 || exit_code == 3,
        "the starved client must fail loudly (run-window exit 1 or connection exit 3), \
         got {exit_code}; {}",
        victim.diagnostics()
    );
    victim.assert_connected_configuration("current", 800);
    let errors = victim.error_messages();
    assert!(
        !errors.is_empty(),
        "the starved client must surface its demise as an error event; {}",
        victim.diagnostics()
    );
    // The distinct SLOW_CONSUMER farewell is best-effort by design (the
    // server's close-time write is bounded while the starved peer's window
    // is saturated): confirmed when observed, a printed notice when not —
    // the LOUD guarantees are the metric, the PlayerLeft, and the error/exit
    // above (mirrors relay_backpressure_e2e's farewell handling).
    if errors
        .iter()
        .any(|message| message.contains("slow consumer"))
    {
        println!("starved client received the distinct slow-consumer farewell");
    } else {
        eprintln!(
            "note: slow-consumer farewell did not survive the saturated socket \
             (best-effort by design); errors: {errors:?}"
        );
    }

    let evictions = scrape_counter(
        server.port,
        "signal_fish_websocket_slow_consumer_disconnects_total",
    )
    .await;
    assert!(
        evictions >= 1,
        "the starved client's eviction must be counted in the server's metrics"
    );
}
