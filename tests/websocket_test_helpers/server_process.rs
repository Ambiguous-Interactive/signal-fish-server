//! Shared real-binary server harness for the true multi-process suites.
//!
//! `tests/v3_multiprocess_e2e.rs` and `tests/multiprocess_delivery_e2e.rs` both
//! spawn the compiled `signal-fish-server` binary
//! (`env!("CARGO_BIN_EXE_signal-fish-server")`) as a REAL child OS process and
//! drive it over real TCP — a genuine process boundary, unlike the in-process
//! suites. They used to carry byte-identical copies of this harness (a
//! maintenance bug: a fix to one silently skipped the other); this module is
//! the single copy.
//!
//! # Infrastructure
//!
//! - **Configuration**: the binary has no config-file CLI flag (`-c` is
//!   `--validate-config`); it discovers configuration through the environment
//!   (`src/config/loader.rs`). Each spawn writes a per-test temp config file
//!   (tempfile) and points the child at it via `SIGNAL_FISH_CONFIG_PATH`, with
//!   the child's working directory set to the same temp dir so no stray
//!   `config.json` in the repo can interfere. Because later config sources
//!   merge over earlier ones and `SIGNAL_FISH__*` env overrides always win,
//!   the port is ALSO pinned via `SIGNAL_FISH__PORT` (belt and braces), and
//!   every inherited `SIGNAL_FISH*` variable is scrubbed from the child env.
//!   The base config keeps the binary zero-dependency: in-memory storage (the
//!   only backend), `enforce_app_id_allowlist: false`, `require_metrics_auth:
//!   false` (the delivery suite scrapes `/metrics/prom`), SDK enforcement off,
//!   reconnection enabled, and file logging off.
//! - **Per-suite config overlay**: the two suites need DIFFERENT config
//!   (v3 sets `session.default_topology` + `session.enable_webrtc`; the
//!   delivery suite sets `session.default_topology` + `websocket.*` knobs), so
//!   [`spawn_server`] / [`spawn_server_on_fixed_port`] take a
//!   [`serde_json::Value`] overlay that is DEEP-merged over [`base_config`]
//!   (see [`merge_config`]): any test can set any key — nested keys merge with
//!   their base siblings rather than replacing the whole subtree — without this
//!   harness knowing the schema.
//! - **Ports**: a `std::net::TcpListener` bound to `0.0.0.0:0` (matching the
//!   server's bind address) reserves a free port which is then released and
//!   passed to the child. The reserve-release-spawn race is absorbed by up to
//!   [`SPAWN_ATTEMPTS`] spawns with fresh ports ([`spawn_server`]); the restart
//!   scenario retries on its FIXED port instead
//!   ([`spawn_server_on_fixed_port`]).
//! - **Readiness**: the spawned process is polled on `/v2/health` (reqwest)
//!   until it answers 200, with a hard deadline; early child exit is detected
//!   via `try_wait`. Failures report the child's captured stdout/stderr
//!   (piped to files in the temp dir).
//! - **Cleanup**: [`ServerProcess`] is a child guard — `Drop` issues
//!   `start_kill()` and the spawn sets `kill_on_drop(true)`, so the server
//!   process dies even when a test panics; no orphans survive CI. Tests that
//!   need a deterministic kill call [`ServerProcess::kill_and_wait`] (SIGKILL +
//!   reap).
//!
//! The parent module (`tests/websocket_test_helpers/mod.rs`) carries
//! `#![allow(dead_code)]`, which covers this child module: a helper only one
//! suite uses is not a dead-code warning in the other.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

/// How long a client socket may take to connect to the spawned binary.
///
/// A saturation-tolerant CEILING, not an expected wait (zero-flakiness policy,
/// .llm/context-testing.md). Each test spawns/drives a REAL child server
/// process; on an oversubscribed runner the child can be CPU-starved and merely
/// slow, so the deadline is generous enough that a starved-but-progressing
/// child still completes. It only bites under pathological load — the happy
/// path returns the instant the socket connects — so a large ceiling never
/// slows a passing run. (nextest also runs these binaries in a bounded
/// `process-spawning` test-group; see .config/nextest.toml.)
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Hard ceiling for the child to answer `/v2/health` (same rationale as
/// [`CONNECT_TIMEOUT`]).
const HEALTH_DEADLINE: Duration = Duration::from_secs(60);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Fresh-port spawn attempts. Each retry reserves a NEW port, so this absorbs
/// the reserve-release-spawn race cheaply; a few attempts make a stolen port
/// vanishingly unlikely to fail a run.
const SPAWN_ATTEMPTS: usize = 5;
/// Fixed-port (restart) spawn attempts. This path CANNOT dodge a transient race
/// by picking a fresh port — it must reuse the SIGKILL'd port — so it gets more
/// attempts and exponential backoff (see [`spawn_server_on_fixed_port`]).
const FIXED_PORT_SPAWN_ATTEMPTS: usize = 6;

/// Guard around the spawned server binary. Dropping it kills the child
/// (`start_kill` here plus `kill_on_drop(true)` at spawn), so a panicking test
/// never leaks an orphan server process into CI.
pub struct ServerProcess {
    child: Option<tokio::process::Child>,
    /// The port the child is listening on (the reserved fresh port, or the
    /// fixed restart port).
    pub port: u16,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    /// Owns the temp config file, the captured output files, and the child's
    /// working directory; removed from disk when the guard drops.
    _workdir: tempfile::TempDir,
}

impl ServerProcess {
    /// OS process id of the live server child (for resource diagnostics).
    pub fn pid(&self) -> u32 {
        self.child
            .as_ref()
            .and_then(tokio::process::Child::id)
            .expect("server process was already killed")
    }

    /// SIGKILL the child (TerminateProcess on Windows) and reap it.
    pub async fn kill_and_wait(&mut self) {
        let mut child = self
            .child
            .take()
            .expect("server process was already killed");
        child.kill().await.expect("kill the server process");
    }

    /// Send SIGTERM to the child without waiting for it to exit.
    #[cfg(unix)]
    pub fn send_sigterm(&mut self) {
        let child = self
            .child
            .as_mut()
            .expect("server process was already killed");
        let pid = child.id().expect("server child has a process id");
        let status = std::process::Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .status()
            .expect("spawn kill -TERM");
        assert!(status.success(), "kill -TERM exited with {status}");
    }

    /// Wait for the child to exit after an externally-sent signal.
    pub async fn wait_for_exit(&mut self) -> std::process::ExitStatus {
        let mut child = self
            .child
            .take()
            .expect("server process was already killed");
        child.wait().await.expect("wait for server process")
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

/// Reserve a currently-free port by binding `0.0.0.0:0` (the same address the
/// server binds), reading the assignment, and releasing the listener.
fn reserve_port() -> u16 {
    let probe = std::net::TcpListener::bind(("0.0.0.0", 0)).expect("bind port probe");
    let port = probe.local_addr().expect("port probe local_addr").port();
    drop(probe);
    port
}

/// The suite-independent base config: in-memory, auth-off, zero-dependency. Each
/// suite's per-scenario config is layered on top via a [`merge_config`] overlay
/// (`session.default_topology`, `websocket.*`, `server.*`, …).
fn base_config(port: u16) -> Value {
    json!({
        "port": port,
        "server": {
            "enable_reconnection": true,
            "reconnection_window": 300
        },
        "security": {
            "enforce_app_id_allowlist": false,
            "require_metrics_auth": false,
            "cors_origins": "*"
        },
        "protocol": {
            "sdk_compatibility": { "enforce": false }
        },
        "logging": {
            "enable_file_logging": false
        }
    })
}

/// Recursively merge `overlay` into `base`: when both sides are JSON objects the
/// keys are merged (so an overlay can set a single nested key — e.g.
/// `server.ping_timeout` — without dropping its base siblings); otherwise the
/// overlay value replaces the base value.
fn merge_config(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Object(base_map), Value::Object(overlay_map)) => {
            for (key, overlay_value) in overlay_map {
                merge_config(
                    base_map.entry(key.clone()).or_insert(Value::Null),
                    overlay_value,
                );
            }
        }
        (base_slot, overlay_value) => {
            *base_slot = overlay_value.clone();
        }
    }
}

/// Spawn the server binary on a fresh free port, retrying with a NEW port on
/// failure (absorbs the reserve-release-spawn race). `overlay` is deep-merged
/// over [`base_config`] to produce this test's effective config.
pub async fn spawn_server(overlay: Value) -> ServerProcess {
    let mut failures = Vec::new();
    for attempt in 1..=SPAWN_ATTEMPTS {
        let port = reserve_port();
        match try_spawn_server(port, &overlay).await {
            Ok(server) => return server,
            Err(failure) => failures.push(format!("attempt {attempt} (port {port}): {failure}")),
        }
    }
    panic!(
        "server binary failed to become healthy after {SPAWN_ATTEMPTS} attempts:\n{}",
        failures.join("\n")
    );
}

/// Spawn the server binary on a FIXED port (restart-on-same-port semantics),
/// retrying the same port with exponential backoff to absorb transient
/// socket-teardown races. After a SIGKILL the kernel may briefly hold the
/// listening port (the dead server's accepted connections linger in TIME_WAIT;
/// Windows' `TerminateProcess` tears the socket down more slowly than a Unix
/// signal), so a single flat retry can expire before the port frees under load.
/// Backoff — not a bigger fixed sleep — is the right tool: the happy path
/// rebinds on attempt 1 and returns immediately, while a genuinely slow teardown
/// gets progressively more time without inflating the common case.
pub async fn spawn_server_on_fixed_port(port: u16, overlay: Value) -> ServerProcess {
    let mut failures = Vec::new();
    let mut backoff = Duration::from_millis(100);
    for attempt in 1..=FIXED_PORT_SPAWN_ATTEMPTS {
        match try_spawn_server(port, &overlay).await {
            Ok(server) => return server,
            Err(failure) => failures.push(format!("attempt {attempt}: {failure}")),
        }
        if attempt < FIXED_PORT_SPAWN_ATTEMPTS {
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_millis(1600));
        }
    }
    panic!(
        "server binary failed to restart on port {port} after {FIXED_PORT_SPAWN_ATTEMPTS} \
         attempts:\n{}",
        failures.join("\n")
    );
}

/// One spawn attempt: write the temp config (base merged with `overlay`),
/// launch the child with a scrubbed environment, and wait for `/v2/health`. On
/// failure the guard (and its `kill_on_drop` child) is dropped and the captured
/// output is returned.
async fn try_spawn_server(port: u16, overlay: &Value) -> Result<ServerProcess, String> {
    let workdir = tempfile::tempdir().expect("create temp workdir");
    let config_path = workdir.path().join("server-config.json");
    let mut config = base_config(port);
    merge_config(&mut config, overlay);
    std::fs::write(
        &config_path,
        serde_json::to_vec_pretty(&config).expect("serialize server config"),
    )
    .expect("write server config");

    let stdout_path = workdir.path().join("server-stdout.log");
    let stderr_path = workdir.path().join("server-stderr.log");
    let stdout_file = std::fs::File::create(&stdout_path).expect("create stdout capture");
    let stderr_file = std::fs::File::create(&stderr_path).expect("create stderr capture");

    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_signal-fish-server"));
    command
        .current_dir(workdir.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(stdout_file))
        .stderr(std::process::Stdio::from(stderr_file))
        .kill_on_drop(true);
    // Scrub every inherited SIGNAL_FISH* variable: ambient config JSON, config
    // paths, or SIGNAL_FISH__* field overrides would silently override the
    // temp config (env overrides are applied last by the loader).
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
/// exits early).
async fn wait_until_healthy(server: &mut ServerProcess) -> Result<(), String> {
    let url = format!("http://127.0.0.1:{}/v2/health", server.port);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("build health-poll client");
    let start = std::time::Instant::now();
    let deadline = start + HEALTH_DEADLINE;

    loop {
        let child = server
            .child
            .as_mut()
            .expect("health poll requires a live child");
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(format!("server process exited early with {status}"));
            }
            Ok(None) => {}
            Err(error) => return Err(format!("failed to poll server process: {error}")),
        }

        match client.get(&url).send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            // Not up yet (connection refused / non-200): keep polling.
            Ok(_) | Err(_) => {}
        }

        if std::time::Instant::now() >= deadline {
            let elapsed = start.elapsed();
            return Err(format!(
                "health endpoint {url} (port {}) never answered 200 within {HEALTH_DEADLINE:?} \
                 (waited {elapsed:?}); the child stayed alive but unready",
                server.port
            ));
        }
        tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
    }
}
