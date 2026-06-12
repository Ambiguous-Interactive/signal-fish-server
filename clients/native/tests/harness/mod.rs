//! Multi-process interop harness: spawns the REAL `signal-fish-server` binary
//! plus N `signal-fish-reference-native` client binaries and asserts global
//! properties over the clients' JSONL stdout streams.
//!
//! # Infrastructure (mirrors the proven `tests/v3_multiprocess_e2e.rs` patterns)
//!
//! - **Server binary**: taken from the REQUIRED `SIGNAL_FISH_SERVER_BIN` env
//!   var (this crate is standalone, so `CARGO_BIN_EXE_*` only exists for its
//!   own binaries). Missing/invalid values panic with setup instructions.
//! - **Configuration**: each spawn writes a per-test temp config file and
//!   points the child at it via `SIGNAL_FISH_CONFIG_PATH` with the working
//!   directory set to the temp dir; the port is ALSO pinned via
//!   `SIGNAL_FISH__PORT` and every inherited `SIGNAL_FISH*` variable is
//!   scrubbed (env overrides merge last in the server's loader). The config
//!   keeps everything local: in-memory storage, WebSocket auth off, the
//!   per-scenario `session.default_topology`, generous rate limits, and —
//!   crucially for CI — `turn.enabled = false` with `turn.stun_urls = []`,
//!   which the server's validation permits and which yields WebRTC plans with
//!   an EMPTY `ice_servers` list: host (loopback) ICE candidates suffice and
//!   no test ever touches an external STUN server.
//! - **Ports**: reserved by binding `0.0.0.0:0` and released to the child;
//!   the reserve-release-spawn race is absorbed by up to 3 attempts with
//!   fresh ports.
//! - **Readiness**: `/v2/health` is polled with a hand-rolled HTTP GET over a
//!   plain `TcpStream` (no HTTP client dependency) under a hard deadline,
//!   detecting early child exits via `try_wait`.
//! - **Cleanup**: both server and client guards use `kill_on_drop(true)` plus
//!   explicit `start_kill` on drop, so panicking tests leak no processes.
//! - **Timeouts**: every await is deadline-bounded; timeouts panic with the
//!   child's recent events and captured stderr for diagnosis.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::TcpStream;
use tokio::process::{Child, ChildStdout, Command};
use tokio::time::Instant;

const HEALTH_DEADLINE: Duration = Duration::from_secs(15);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(100);
const HEALTH_IO_TIMEOUT: Duration = Duration::from_secs(2);
const SPAWN_ATTEMPTS: usize = 3;

/// Generous per-event ceiling: a CI scheduling budget, not an expected wait.
pub const EVENT_TIMEOUT: Duration = Duration::from_secs(30);
/// Ceiling for a client process to finish its whole scenario and exit.
pub const CLIENT_EXIT_TIMEOUT: Duration = Duration::from_secs(120);

/// Resolve the server binary path from the REQUIRED `SIGNAL_FISH_SERVER_BIN`
/// env var, with actionable failure messages.
pub fn server_binary_path() -> PathBuf {
    let Some(raw) = std::env::var_os("SIGNAL_FISH_SERVER_BIN") else {
        panic!(
            "SIGNAL_FISH_SERVER_BIN is not set. The interop tests drive the real server \
             binary: run `cargo build --bin signal-fish-server` at the repository root and \
             set SIGNAL_FISH_SERVER_BIN=<repo>/target/debug/signal-fish-server (or use the \
             runner script scripts/run-webrtc-interop.sh)."
        );
    };
    let path = PathBuf::from(raw);
    assert!(
        path.is_file(),
        "SIGNAL_FISH_SERVER_BIN points at {path:?}, which is not a file. Run \
         `cargo build --bin signal-fish-server` at the repository root first \
         (or use scripts/run-webrtc-interop.sh)."
    );
    path
}

/// Resolve the browser reference client's built CLI bundle from the REQUIRED
/// `SIGNAL_FISH_BROWSER_CLI` env var (browser interop cells only), with
/// actionable failure messages. The bundle is run via `node`.
pub fn browser_cli_path() -> PathBuf {
    let Some(raw) = std::env::var_os("SIGNAL_FISH_BROWSER_CLI") else {
        panic!(
            "SIGNAL_FISH_BROWSER_CLI is not set. The browser interop tests drive the \
             browser reference client via Node: run `npm ci && npm run build` in \
             clients/browser/ and set SIGNAL_FISH_BROWSER_CLI=<repo>/clients/browser/dist/cli.js \
             (or use the runner script scripts/run-browser-interop.sh)."
        );
    };
    let path = PathBuf::from(raw);
    assert!(
        path.is_file(),
        "SIGNAL_FISH_BROWSER_CLI points at {path:?}, which is not a file. Run \
         `npm ci && npm run build` in clients/browser/ first \
         (or use scripts/run-browser-interop.sh)."
    );
    path
}

/// Guard around the spawned server binary. Dropping it kills the child
/// (`start_kill` plus `kill_on_drop(true)` at spawn).
pub struct ServerProcess {
    child: Option<Child>,
    pub port: u16,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    /// Owns the temp config + captured output + child working directory.
    _workdir: tempfile::TempDir,
}

impl ServerProcess {
    /// `ws://127.0.0.1:PORT/v3/ws` — the endpoint clients connect to.
    pub fn v3_ws_url(&self) -> String {
        format!("ws://127.0.0.1:{}/v3/ws", self.port)
    }

    /// `ws://127.0.0.1:PORT/v2/ws` — the legacy endpoint; a faithful v2
    /// client connects here (it would not know `/v3/ws` exists). Rooms are
    /// shared across both endpoints (same server state).
    pub fn v2_ws_url(&self) -> String {
        format!("ws://127.0.0.1:{}/v2/ws", self.port)
    }

    /// The child's captured stdout/stderr, for spawn-failure diagnostics.
    fn captured_output(&self) -> String {
        let read = |label: &str, path: &PathBuf| {
            let content = std::fs::read_to_string(path)
                .unwrap_or_else(|error| format!("<failed to read {label}: {error}>"));
            format!("--- server {label} ---\n{content}")
        };
        format!(
            "{}\n{}",
            read("stdout", &self.stdout_path),
            read("stderr", &self.stderr_path)
        )
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

/// Reserve a currently-free port by binding `0.0.0.0:0` (the address the
/// server binds), reading the assignment, and releasing the listener.
fn reserve_port() -> u16 {
    let probe = std::net::TcpListener::bind(("0.0.0.0", 0)).expect("bind port probe");
    let port = probe.local_addr().expect("port probe local_addr").port();
    drop(probe);
    port
}

/// Spawn the server binary on a fresh free port with the given default
/// topology, retrying with a NEW port on failure (absorbs the
/// reserve-release-spawn race).
pub async fn spawn_server(default_topology: &str) -> ServerProcess {
    let mut failures = Vec::new();
    for attempt in 1..=SPAWN_ATTEMPTS {
        let port = reserve_port();
        match try_spawn_server(port, default_topology).await {
            Ok(server) => return server,
            Err(failure) => failures.push(format!("attempt {attempt} (port {port}): {failure}")),
        }
    }
    panic!(
        "server binary failed to become healthy after {SPAWN_ATTEMPTS} attempts:\n{}",
        failures.join("\n")
    );
}

/// One spawn attempt: write the temp config, launch the child with a scrubbed
/// environment, and wait for `/v2/health`.
async fn try_spawn_server(port: u16, default_topology: &str) -> Result<ServerProcess, String> {
    let workdir = tempfile::tempdir().expect("create temp workdir");
    let config_path = workdir.path().join("server-config.json");
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
            "default_topology": default_topology,
            "enable_webrtc": true
        },
        // No TURN, and an EMPTY public-STUN list: WebRTC plans carry zero ICE
        // servers, so candidate gathering is host-interface-only (loopback) and
        // CI never performs external network access. The server's TurnConfig
        // validation explicitly permits an empty stun_urls list while disabled.
        "turn": {
            "enabled": false,
            "stun_urls": []
        },
        // Generous rate limits: three clients trickle ICE simultaneously.
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

    let mut command = Command::new(server_binary_path());
    command
        .current_dir(workdir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .kill_on_drop(true);
    // Scrub every inherited SIGNAL_FISH* variable: ambient config JSON, config
    // paths, or SIGNAL_FISH__* field overrides would silently override the
    // temp config (env overrides are applied last by the server's loader).
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
        stdout_path,
        stderr_path,
        _workdir: workdir,
    };

    match wait_until_healthy(&mut server).await {
        Ok(()) => Ok(server),
        Err(reason) => Err(format!(
            "{reason}; captured child output:\n{}",
            server.captured_output()
        )),
    }
}

/// Poll `/v2/health` until it answers 200 (or the deadline passes / the child
/// exits early). Hand-rolled HTTP GET over a plain TcpStream: the readiness
/// probe needs no HTTP client dependency.
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

        if health_endpoint_answers_ok(server.port).await {
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

/// One bounded `GET /v2/health` probe; any failure means "not ready yet".
async fn health_endpoint_answers_ok(port: u16) -> bool {
    let connect = tokio::time::timeout(HEALTH_IO_TIMEOUT, TcpStream::connect(("127.0.0.1", port)));
    let Ok(Ok(mut stream)) = connect.await else {
        return false;
    };
    let request =
        format!("GET /v2/health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    let write = tokio::time::timeout(HEALTH_IO_TIMEOUT, stream.write_all(request.as_bytes()));
    if !matches!(write.await, Ok(Ok(()))) {
        return false;
    }
    let mut response = Vec::new();
    let read = tokio::time::timeout(HEALTH_IO_TIMEOUT, stream.read_to_end(&mut response));
    if !matches!(read.await, Ok(Ok(_))) {
        return false;
    }
    let response = String::from_utf8_lossy(&response);
    response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200")
}

/// Guard around one spawned reference-client binary; collects every stdout
/// JSONL event it emits into [`ClientProcess::events`].
pub struct ClientProcess {
    pub name: String,
    child: Option<Child>,
    lines: Lines<BufReader<ChildStdout>>,
    /// Every parsed stdout event, in emission order (grows as events are read).
    pub events: Vec<Value>,
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

/// Builder for one reference-client invocation (keeps scenario code terse).
pub struct ClientSpec<'a> {
    pub name: &'a str,
    pub server_url: &'a str,
    pub game_name: &'a str,
    /// `None` creates the room; `Some(code)` joins by code.
    pub join_code: Option<&'a str>,
    pub peers: usize,
    pub exchange: bool,
    pub relay_payload: Option<&'a str>,
    pub extra_args: &'a [&'a str],
}

/// Spawn one reference-client binary; its stderr goes to a file under
/// `workdir`, stdout is piped for JSONL event reading.
pub fn spawn_client(spec: &ClientSpec<'_>, workdir: &Path) -> ClientProcess {
    let stderr_path = workdir.join(format!("client-{}-stderr.log", spec.name));
    let stderr_file = std::fs::File::create(&stderr_path).expect("create client stderr capture");

    let mut command = Command::new(env!("CARGO_BIN_EXE_signal-fish-reference-native"));
    command
        .arg("--server-url")
        .arg(spec.server_url)
        .arg("--game-name")
        .arg(spec.game_name)
        .arg("--player-name")
        .arg(spec.name)
        .arg("--peers")
        .arg(spec.peers.to_string())
        // Soft/hard windows sized for slow CI machines; clients exit as soon
        // as their criteria are met, so these are ceilings, not durations.
        .arg("--run-for-secs")
        .arg("45")
        .arg("--max-runtime-secs")
        .arg("90");
    match spec.join_code {
        Some(code) => {
            command.arg("--join-code").arg(code);
        }
        None => {
            command.arg("--create-room");
        }
    }
    if spec.exchange {
        command.arg("--exchange");
    }
    if let Some(payload) = spec.relay_payload {
        command.arg("--relay-payload").arg(payload);
    }
    command.args(spec.extra_args);
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

/// Spawn one BROWSER reference client (`node <SIGNAL_FISH_BROWSER_CLI> …`);
/// same flag surface, JSONL event contract, and exit codes as the native
/// client, so the resulting [`ClientProcess`] is interchangeable with
/// [`spawn_client`]'s. The Node CLI reaps its Chromium child on every exit
/// path (including this harness's kill-on-drop SIGKILL, via a detached
/// reaper), so dropping the guard leaks no browser processes.
pub fn spawn_browser_client(spec: &ClientSpec<'_>, workdir: &Path) -> ClientProcess {
    let stderr_path = workdir.join(format!("client-{}-stderr.log", spec.name));
    let stderr_file = std::fs::File::create(&stderr_path).expect("create client stderr capture");

    let mut command = Command::new("node");
    command
        .arg(browser_cli_path())
        .arg("--server-url")
        .arg(spec.server_url)
        .arg("--game-name")
        .arg(spec.game_name)
        .arg("--player-name")
        .arg(spec.name)
        .arg("--peers")
        .arg(spec.peers.to_string())
        // Same ceilings as the native spawn (Chromium launch eats ~1-2 s of
        // the soft window; clients still exit as soon as criteria are met).
        .arg("--run-for-secs")
        .arg("45")
        .arg("--max-runtime-secs")
        .arg("90");
    match spec.join_code {
        Some(code) => {
            command.arg("--join-code").arg(code);
        }
        None => {
            command.arg("--create-room");
        }
    }
    if spec.exchange {
        command.arg("--exchange");
    }
    if let Some(payload) = spec.relay_payload {
        command.arg("--relay-payload").arg(payload);
    }
    command.args(spec.extra_args);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr_file))
        .kill_on_drop(true);

    let mut child = command
        .spawn()
        .expect("spawn the browser reference client (node)");
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
    /// OS pid of the still-running client process (panics once reaped).
    pub fn pid(&self) -> u32 {
        self.child
            .as_ref()
            .and_then(Child::id)
            .expect("client process already exited or was reaped")
    }

    /// Read events until one named `event_name` arrives; panics (with
    /// diagnostics) on timeout, EOF, or a non-JSONL stdout line.
    pub async fn await_event(&mut self, event_name: &str, timeout: Duration) -> Value {
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

    /// Like [`Self::await_event`], but first satisfied by an ALREADY recorded
    /// event. Use when several awaited events have no fixed relative order
    /// (e.g. a pair connecting vs. a sibling departing): plain `await_event`
    /// only scans forward, so awaiting them in the wrong order would wait
    /// forever for an event that was already consumed.
    pub async fn await_event_or_recorded(&mut self, event_name: &str, timeout: Duration) -> Value {
        if let Some(event) = self
            .events
            .iter()
            .find(|event| event.get("event").and_then(Value::as_str) == Some(event_name))
        {
            return event.clone();
        }
        self.await_event(event_name, timeout).await
    }

    /// Read and record the next event line before `deadline`; `None` on EOF.
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

    /// Drain every remaining event to EOF, reap the child, and return its exit
    /// code. All reads and the reap share one deadline.
    pub async fn drain_to_exit(&mut self, timeout: Duration) -> i32 {
        let deadline = Instant::now() + timeout;
        while let Some(_event) = self
            .next_event_before(deadline, "draining events to process exit")
            .await
        {
            // Every event is recorded by next_event_before; nothing else to do.
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

    /// Recent events plus captured stderr, for failure messages.
    pub fn diagnostics(&self) -> String {
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
// Event-log query helpers (operate on fully drained `events` vectors).
// ---------------------------------------------------------------------------

/// All events with the given `event` tag.
pub fn events_named<'a>(events: &'a [Value], name: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| event.get("event").and_then(Value::as_str) == Some(name))
        .collect()
}

/// Exactly one event with the tag, or panic.
pub fn single_event<'a>(events: &'a [Value], name: &str, who: &str) -> &'a Value {
    let matches = events_named(events, name);
    assert_eq!(
        matches.len(),
        1,
        "{who}: expected exactly one `{name}` event, got {}: {matches:?}",
        matches.len()
    );
    matches[0]
}

/// The required string field of an event, or panic.
pub fn str_field<'a>(event: &'a Value, field: &str) -> &'a str {
    event
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("event missing string field `{field}`: {event}"))
}

/// This client's own player id (from its `room_joined` event).
pub fn player_id_of(events: &[Value], who: &str) -> String {
    str_field(single_event(events, "room_joined", who), "player_id").to_string()
}

/// The pre-teardown "scenario window" of a fully drained log: every event
/// before the client's first `player_left`.
///
/// Clients exit as soon as their own success criteria are met, so teardown is
/// staggered; once the first sibling departs, the server may legitimately
/// emit post-scenario traffic — most notably the session-007 host-failover
/// replan, which re-issues `SessionPlan`s with a freshly elected host when a
/// host-topology session's host exits first. Scenario assertions therefore
/// pin the window BEFORE any departure.
///
/// Why the asserted events land inside the window:
///
/// - **Server-enqueued WS events** (plans, `NewPeer`, signals, status
///   fan-outs, GameData): the server→client WebSocket is FIFO, and each such
///   event is enqueued before any sibling can have met its exit criteria
///   (which include receiving that same traffic), hence strictly before any
///   `PlayerLeft` is enqueued. For these the boundary is a true ordering
///   guarantee.
/// - **Data-channel events** (`channel_message`, delivered over SCTP outside
///   the WS FIFO): no cross-transport ordering guarantee exists, so the
///   claim is necessarily weaker. The argument is: a sibling's exit criteria
///   chain through RECEIPT of this client's exchange messages, so the
///   sibling's own sends preceded its exit by at least its post-criteria
///   exit linger; delivery is loopback SCTP (sub-millisecond), the
///   `reliable` channel retransmits while the association lives, and the
///   single `unreliable` send rides the same loopback. A late or lost
///   message would fail the window assertions loudly (missing event), never
///   pass silently.
pub fn scenario_window(events: &[Value]) -> &[Value] {
    let end = events
        .iter()
        .position(|event| event.get("event").and_then(Value::as_str) == Some("player_left"))
        .unwrap_or(events.len());
    &events[..end]
}
