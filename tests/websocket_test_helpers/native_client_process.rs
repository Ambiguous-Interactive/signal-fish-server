//! Shared real native-reference-client process harness.
//!
//! Root integration tests cannot use the standalone `clients/native` crate's
//! test harness directly. This module owns the one root-side implementation:
//! locate (or build) the binary, spawn it with an arbitrary CLI argument list,
//! consume its JSONL stdout without blocking stderr, and kill it on drop.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::ChildStdout;

/// Locate the `signal-fish-reference-native` binary: `SIGNAL_FISH_CLIENT_BIN`
/// when set (CI pre-builds it), else a once-per-process standalone-crate build.
pub fn native_client_binary() -> PathBuf {
    if let Some(raw) = std::env::var_os("SIGNAL_FISH_CLIENT_BIN") {
        let path = PathBuf::from(raw);
        assert!(
            path.is_file(),
            "SIGNAL_FISH_CLIENT_BIN points at {path:?}, which is not a file. Run \
             `cargo build --locked --manifest-path clients/native/Cargo.toml --bin \
             signal-fish-reference-native` and point the variable at the built binary."
        );
        return path;
    }

    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let manifest = root.join("clients").join("native").join("Cargo.toml");
            let mut command = std::process::Command::new(env!("CARGO"));
            command
                .arg("build")
                .arg("--locked")
                .arg("--manifest-path")
                .arg(&manifest)
                .args(["--bin", "signal-fish-reference-native"]);
            // Instrumented parent cargo processes must not leak flags into the
            // nested standalone-workspace build.
            for var in [
                "RUSTFLAGS",
                "CARGO_ENCODED_RUSTFLAGS",
                "RUSTDOCFLAGS",
                "CARGO_TARGET_DIR",
                "ASAN_OPTIONS",
                "LSAN_OPTIONS",
                "UBSAN_OPTIONS",
                "TSAN_OPTIONS",
                "MIRIFLAGS",
            ] {
                command.env_remove(var);
            }
            let status = command
                .status()
                .expect("run `cargo build` for the native reference client");
            assert!(
                status.success(),
                "building the native reference client failed ({status}); build it manually with \
                 `cargo build --locked --manifest-path clients/native/Cargo.toml --bin \
                 signal-fish-reference-native` or set SIGNAL_FISH_CLIENT_BIN"
            );
            let path = root
                .join("clients")
                .join("native")
                .join("target")
                .join("debug")
                .join("signal-fish-reference-native");
            assert!(
                path.is_file(),
                "the nested client build succeeded but {path:?} does not exist"
            );
            path
        })
        .clone()
}

/// Guard around one spawned reference-client process and its JSONL event log.
pub struct NativeClientProcess {
    pub name: String,
    child: Option<tokio::process::Child>,
    stdout: BufReader<ChildStdout>,
    pub events: Vec<Value>,
    stderr_path: PathBuf,
}

impl Drop for NativeClientProcess {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if let Err(error) = child.start_kill() {
                eprintln!("failed to kill client {} on drop: {error}", self.name);
            }
        }
    }
}

/// Spawn one native reference client with the exact supplied CLI arguments.
pub fn spawn_native_client(name: &str, args: &[String], workdir: &Path) -> NativeClientProcess {
    let stderr_path = workdir.join(format!("client-{name}-stderr.log"));
    let stderr_file = std::fs::File::create(&stderr_path).expect("create client stderr capture");

    let mut command = tokio::process::Command::new(native_client_binary());
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::from(stderr_file))
        .env("RUST_LOG", "info")
        .kill_on_drop(true);

    let mut child = command.spawn().expect("spawn the native reference client");
    let stdout = child.stdout.take().expect("client stdout is piped");
    NativeClientProcess {
        name: name.to_string(),
        child: Some(child),
        stdout: BufReader::new(stdout),
        events: Vec::new(),
        stderr_path,
    }
}

impl NativeClientProcess {
    /// OS pid of the still-running client (panics once reaped).
    pub fn pid(&self) -> u32 {
        self.child
            .as_ref()
            .and_then(tokio::process::Child::id)
            .expect("client process already exited or was reaped")
    }

    /// Read events until one named event arrives.
    pub async fn await_event(&mut self, event_name: &str, timeout: Duration) -> Value {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match self
                .next_event_before(deadline, &format!("awaiting `{event_name}`"))
                .await
            {
                Some(event) if event.get("event").and_then(Value::as_str) == Some(event_name) => {
                    return event;
                }
                Some(_) => {}
                None => panic!(
                    "client {}: stdout ended before `{event_name}`;\n{}",
                    self.name,
                    self.diagnostics()
                ),
            }
        }
    }

    /// Read until `count` events with this tag have been recorded in total.
    pub async fn await_event_count(&mut self, event_name: &str, count: usize, timeout: Duration) {
        self.await_event_counts(&[(event_name, count)], timeout)
            .await;
    }

    /// Read until every `(tag, total count)` requirement is satisfied against
    /// one absolute deadline. This lets held-process barriers settle concurrent
    /// event streams without accidentally granting one full timeout per tag.
    pub async fn await_event_counts(&mut self, expected: &[(&str, usize)], timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut observed: Vec<usize> = expected
            .iter()
            .map(|(event_name, _count)| self.recorded_event_count(event_name))
            .collect();
        while observed
            .iter()
            .zip(expected)
            .any(|(seen, (_event_name, count))| seen < count)
        {
            let context = expected
                .iter()
                .zip(&observed)
                .map(|((event_name, count), seen)| format!("{seen}/{count} `{event_name}` events"))
                .collect::<Vec<_>>()
                .join(", ");
            let event = self.next_event_before(deadline, &context).await;
            if let Some(event_name) = event
                .as_ref()
                .and_then(|item| item.get("event"))
                .and_then(Value::as_str)
            {
                for ((expected_name, _count), seen) in expected.iter().zip(&mut observed) {
                    if event_name == *expected_name {
                        *seen += 1;
                    }
                }
            }
            assert!(
                event.is_some(),
                "client {}: stdout ended before all event counts settled ({context});\n{}",
                self.name,
                self.diagnostics()
            );
        }
    }

    fn recorded_event_count(&self, event_name: &str) -> usize {
        self.events
            .iter()
            .filter(|event| event.get("event").and_then(Value::as_str) == Some(event_name))
            .count()
    }

    async fn next_event_before(
        &mut self,
        deadline: tokio::time::Instant,
        context: &str,
    ) -> Option<Value> {
        let mut bytes = Vec::new();
        let read =
            tokio::time::timeout_at(deadline, self.stdout.read_until(b'\n', &mut bytes)).await;
        let bytes_read = read
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
            });
        if bytes_read == 0 {
            return None;
        }
        let terminated = bytes.ends_with(b"\n");
        if terminated {
            bytes.pop();
            if bytes.ends_with(b"\r") {
                bytes.pop();
            }
        }
        let signaled_trailing_fragment = !terminated && self.child_exited_by_signal();
        let line = match String::from_utf8(bytes) {
            Ok(line) => line,
            Err(error) if signaled_trailing_fragment => {
                eprintln!(
                    "client {}: ignoring trailing non-UTF-8 stdout fragment after signal: {error}",
                    self.name
                );
                return None;
            }
            Err(error) => panic!(
                "client {}: stdout line is not UTF-8 ({error});\n{}",
                self.name,
                self.diagnostics()
            ),
        };
        let event: Value = match serde_json::from_str(&line) {
            Ok(event) => event,
            Err(_error) if signaled_trailing_fragment => {
                eprintln!(
                    "client {}: ignoring trailing partial JSONL after signal: {line}",
                    self.name
                );
                return None;
            }
            Err(error) => panic!(
                "client {}: stdout line is not a JSON event ({error}): {line}",
                self.name
            ),
        };
        self.events.push(event.clone());
        Some(event)
    }

    fn child_exited_by_signal(&mut self) -> bool {
        use std::os::unix::process::ExitStatusExt;

        self.child
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten())
            .is_some_and(|status| status.signal().is_some())
    }

    /// Drain stdout to EOF, reap the child, and return its exact exit status.
    pub async fn drain_to_termination(&mut self, timeout: Duration) -> std::process::ExitStatus {
        let deadline = tokio::time::Instant::now() + timeout;
        while let Some(_event) = self
            .next_event_before(deadline, "draining events to process exit")
            .await
        {}
        let mut child = self.child.take().expect("client child already reaped");
        tokio::time::timeout_at(deadline, child.wait())
            .await
            .unwrap_or_else(|_elapsed| {
                panic!(
                    "client {}: stdout closed but the process did not exit;\n{}",
                    self.name,
                    self.diagnostics()
                )
            })
            .unwrap_or_else(|error| panic!("client {}: failed to reap process: {error}", self.name))
    }

    pub fn error_messages(&self) -> Vec<String> {
        self.events
            .iter()
            .filter(|event| event.get("event").and_then(Value::as_str) == Some("error"))
            .filter_map(|event| event.get("message").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    }

    pub fn diagnostics(&self) -> String {
        const EVENT_TAIL: usize = 12;
        let tail_start = self.events.len().saturating_sub(EVENT_TAIL);
        let recent: Vec<String> = self.events[tail_start..]
            .iter()
            .map(Value::to_string)
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
