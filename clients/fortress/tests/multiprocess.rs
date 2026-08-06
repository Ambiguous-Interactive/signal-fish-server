#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

const CHILD_DEADLINE: Duration = Duration::from_secs(40);
const SERVER_READY_DEADLINE: Duration = Duration::from_secs(10);
const SERVER_SPAWN_ATTEMPTS: usize = 3;

#[derive(Debug, Deserialize)]
struct Report {
    player_id: String,
    current_frame: i32,
    confirmed_frame: i32,
    game_frame: i32,
    game_checksum: u64,
    frames_advanced: u64,
    rollback_count: u64,
    max_rollback_depth: u32,
    stall_count: u64,
    wait_recommendations: u64,
    confirmation_lag_current: u64,
    confirmation_lag_max: u64,
    checksums_mismatched: u64,
    checksums_compared: u64,
    checksums_matched: u64,
    events_discarded_total: u64,
    client_game_data_sent: u64,
    client_game_data_sent_during_run: u64,
    client_game_data_received: u64,
    client_messages_undecodable: u64,
    final_pipeline_queue_depth: usize,
    peak_pipeline_queue_depth: usize,
    peak_oldest_queue_age_us: u128,
    relay_frames_enqueued: u64,
    relay_frames_enqueued_during_run: u64,
    relay_frames_received: u64,
    relay_malformed: u64,
    relay_wrong_destination: u64,
    relay_unknown_sender: u64,
    relay_outbound_overflow: u64,
    relay_inbound_overflow: u64,
    relay_encode_failures: u64,
    relay_completion_underflow: u64,
    relay_send_retries: u64,
    running_elapsed_ms: u128,
    polling_callbacks_during_run: u64,
    relay_sent_sequence_count: u64,
    relay_sent_first_sequence: u64,
    relay_sent_last_sequence: u64,
    relay_sent_sequence_hash: u64,
    relay_received_sequence_count: u64,
    relay_received_first_sequence: u64,
    relay_received_last_sequence: u64,
    relay_received_sequence_hash: u64,
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("0.0.0.0:0").expect("reserve port");
    listener.local_addr().expect("ephemeral address").port()
}

fn temp_room_file() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "signal-fish-fortress-room-{}-{stamp}",
        std::process::id()
    ))
}

fn wait_for(mut predicate: impl FnMut() -> bool, deadline: Duration) -> bool {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        if predicate() {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    false
}

fn wait_for_server(child: &mut Child, port: u16) -> Result<(), String> {
    let end = Instant::now() + SERVER_READY_DEADLINE;
    while Instant::now() < end {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("query server process: {error}"))?
        {
            return Err(format!("server exited before readiness with {status}"));
        }
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(format!(
        "server did not bind 127.0.0.1:{port} within {SERVER_READY_DEADLINE:?}"
    ))
}

fn spawn_server(server_bin: &str) -> (Server, u16) {
    let mut failures = Vec::new();
    for attempt in 1..=SERVER_SPAWN_ATTEMPTS {
        let port = free_port();
        let mut command = Command::new(server_bin);
        command.stdout(Stdio::null()).stderr(Stdio::inherit());

        // Config loading applies environment overrides last. Scrub the whole
        // namespace so ambient developer/runner settings cannot change auth,
        // TURN, rate limits, or any other behavior under this regression.
        for (key, _) in std::env::vars_os() {
            if key
                .to_str()
                .is_some_and(|key| key.starts_with("SIGNAL_FISH"))
            {
                command.env_remove(&key);
            }
        }
        command
            .env("SIGNAL_FISH__PORT", port.to_string())
            .env("SIGNAL_FISH__LOGGING__LEVEL", "warn")
            .env("SIGNAL_FISH__LOGGING__ENABLE_FILE_LOGGING", "false")
            .env("SIGNAL_FISH__TURN__ENABLED", "false")
            .env("SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH", "false")
            .env("SIGNAL_FISH__SECURITY__ENFORCE_APP_ID_ALLOWLIST", "false")
            .env("SIGNAL_FISH__PROTOCOL__SDK_COMPATIBILITY__ENFORCE", "false");

        match command.spawn() {
            Ok(mut child) => match wait_for_server(&mut child, port) {
                Ok(()) => return (Server(child), port),
                Err(reason) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    failures.push(format!("attempt {attempt} (port {port}): {reason}"));
                }
            },
            Err(error) => failures.push(format!(
                "attempt {attempt} (port {port}): spawn server: {error}"
            )),
        }
    }
    panic!(
        "server failed to become ready after {SERVER_SPAWN_ATTEMPTS} attempts:\n{}",
        failures.join("\n")
    );
}

fn wait_outputs(mut first: Child, mut second: Child) -> (Output, Output) {
    if !wait_for(
        || {
            first.try_wait().expect("query creator").is_some()
                && second.try_wait().expect("query joiner").is_some()
        },
        CHILD_DEADLINE,
    ) {
        let _ = first.kill();
        let _ = second.kill();
        let first_output = first.wait_with_output().expect("collect creator timeout");
        let second_output = second.wait_with_output().expect("collect joiner timeout");
        panic!(
            "timed out waiting for game processes\ncreator stdout={}\ncreator stderr={}\njoiner stdout={}\njoiner stderr={}",
            String::from_utf8_lossy(&first_output.stdout),
            String::from_utf8_lossy(&first_output.stderr),
            String::from_utf8_lossy(&second_output.stdout),
            String::from_utf8_lossy(&second_output.stderr)
        );
    }
    (
        first.wait_with_output().expect("collect creator output"),
        second.wait_with_output().expect("collect joiner output"),
    )
}

fn parse_report(name: &str, output: Output) -> Report {
    assert!(
        output.status.success(),
        "{name} failed: status={}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{name} emitted invalid report: {error}\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn assert_healthy(name: &str, report: &Report) {
    assert!(report.current_frame >= 600, "{name}: {report:?}");
    assert!(report.confirmed_frame >= 600, "{name}: {report:?}");
    assert!(report.game_frame >= 600, "{name}: {report:?}");
    assert!(report.frames_advanced >= 600, "{name}: {report:?}");
    assert!(report.client_game_data_sent >= 1_200, "{name}: {report:?}");
    assert!(
        report.client_game_data_sent_during_run >= 1_200,
        "{name}: {report:?}"
    );
    assert!(
        report.client_game_data_received >= 1_200,
        "{name}: {report:?}"
    );
    assert!(report.relay_frames_enqueued >= 1_200, "{name}: {report:?}");
    assert!(
        report.relay_frames_enqueued_during_run >= 1_200,
        "{name}: {report:?}"
    );
    assert!(report.relay_frames_received >= 1_200, "{name}: {report:?}");
    assert_eq!(
        report.relay_frames_enqueued, report.client_game_data_sent,
        "{name}: every Fortress frame must complete its client write: {report:?}"
    );
    assert_eq!(
        report.relay_frames_received, report.client_game_data_received,
        "{name}: every client-delivered binary event must reach Fortress: {report:?}"
    );
    assert_eq!(report.final_pipeline_queue_depth, 0, "{name}: {report:?}");
    assert!(report.peak_pipeline_queue_depth <= 64, "{name}: {report:?}");
    assert!(
        report.peak_oldest_queue_age_us <= 500_000,
        "{name}: {report:?}"
    );
    assert_eq!(report.relay_malformed, 0, "{name}: {report:?}");
    assert_eq!(report.relay_wrong_destination, 0, "{name}: {report:?}");
    assert_eq!(report.relay_unknown_sender, 0, "{name}: {report:?}");
    assert_eq!(report.relay_outbound_overflow, 0, "{name}: {report:?}");
    assert_eq!(report.relay_inbound_overflow, 0, "{name}: {report:?}");
    assert_eq!(report.relay_encode_failures, 0, "{name}: {report:?}");
    assert_eq!(report.relay_completion_underflow, 0, "{name}: {report:?}");
    assert_eq!(report.client_messages_undecodable, 0, "{name}: {report:?}");
    assert_eq!(report.checksums_mismatched, 0, "{name}: {report:?}");
    assert!(report.checksums_compared >= 8, "{name}: {report:?}");
    assert_eq!(
        report.checksums_matched, report.checksums_compared,
        "{name}: {report:?}"
    );
    assert_eq!(report.events_discarded_total, 0, "{name}: {report:?}");
    assert!(report.confirmation_lag_current <= 8, "{name}: {report:?}");
    assert!(report.confirmation_lag_max <= 8, "{name}: {report:?}");
    assert_eq!(report.stall_count, 0, "{name}: {report:?}");
    assert_eq!(report.wait_recommendations, 0, "{name}: {report:?}");
    assert!(report.running_elapsed_ms >= 9_000, "{name}: {report:?}");
    assert!(report.running_elapsed_ms <= 15_000, "{name}: {report:?}");
    let relay_rate_hz =
        report.relay_frames_enqueued_during_run as f64 * 1_000.0 / report.running_elapsed_ms as f64;
    assert!(
        relay_rate_hz >= 120.0,
        "{name}: rate={relay_rate_hz:.1}, {report:?}"
    );
    assert!(
        report.client_game_data_sent_during_run > report.polling_callbacks_during_run * 2,
        "{name}: the issue-242 load must require multiple sends per 60 Hz poll: {report:?}"
    );
}

#[test]
fn two_fortress_game_processes_sustain_60fps_through_real_server() {
    let server_bin = std::env::var("SIGNAL_FISH_SERVER_BIN")
        .expect("SIGNAL_FISH_SERVER_BIN must point to a freshly built Signal Fish Server binary");
    assert!(
        Path::new(&server_bin).is_absolute(),
        "SIGNAL_FISH_SERVER_BIN must be absolute so child cwd changes cannot select another binary"
    );
    let peer_bin = env!("CARGO_BIN_EXE_fortress-relay-peer");
    let room_file = temp_room_file();
    let (mut server, port) = spawn_server(&server_bin);
    assert!(
        server.0.try_wait().expect("query server").is_none(),
        "server exited early"
    );

    let url = format!("ws://127.0.0.1:{port}/v2/ws");
    let creator = Command::new(peer_bin)
        .args([&url, "creator"])
        .arg(&room_file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn creator game process");
    if !wait_for(
        || fs::metadata(&room_file).is_ok_and(|metadata| metadata.len() > 0),
        Duration::from_secs(10),
    ) {
        let mut creator = creator;
        let _ = creator.kill();
        let output = creator.wait_with_output().expect("collect creator timeout");
        panic!(
            "timed out waiting for creator room code\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let room_code = fs::read_to_string(&room_file).expect("read room code");
    let joiner = Command::new(peer_bin)
        .args([&url, "joiner"])
        .arg(&room_file)
        .arg(room_code.trim())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn joiner game process");

    let (creator_output, joiner_output) = wait_outputs(creator, joiner);
    let creator_report = parse_report("creator", creator_output);
    let joiner_report = parse_report("joiner", joiner_output);
    let _ = fs::remove_file(Path::new(&room_file));

    println!("creator report: {creator_report:#?}");
    println!("joiner report: {joiner_report:#?}");

    assert_ne!(creator_report.player_id, joiner_report.player_id);
    assert_eq!(
        creator_report.relay_sent_sequence_count,
        joiner_report.relay_received_sequence_count
    );
    assert_eq!(
        creator_report.relay_sent_first_sequence,
        joiner_report.relay_received_first_sequence
    );
    assert_eq!(
        creator_report.relay_sent_last_sequence,
        joiner_report.relay_received_last_sequence
    );
    assert_eq!(
        creator_report.relay_sent_sequence_hash,
        joiner_report.relay_received_sequence_hash
    );
    assert_eq!(
        joiner_report.relay_sent_sequence_count,
        creator_report.relay_received_sequence_count
    );
    assert_eq!(
        joiner_report.relay_sent_first_sequence,
        creator_report.relay_received_first_sequence
    );
    assert_eq!(
        joiner_report.relay_sent_last_sequence,
        creator_report.relay_received_last_sequence
    );
    assert_eq!(
        joiner_report.relay_sent_sequence_hash,
        creator_report.relay_received_sequence_hash
    );
    assert_healthy("creator", &creator_report);
    assert_healthy("joiner", &joiner_report);
    assert_ne!(creator_report.game_checksum, 0);
    assert_ne!(joiner_report.game_checksum, 0);
    assert!(creator_report.rollback_count > 0, "{creator_report:?}");
    assert!(joiner_report.rollback_count > 0, "{joiner_report:?}");
    assert!(creator_report.max_rollback_depth <= 8, "{creator_report:?}");
    assert!(joiner_report.max_rollback_depth <= 8, "{joiner_report:?}");
    assert!(creator_report.relay_send_retries <= 8, "{creator_report:?}");
    assert!(joiner_report.relay_send_retries <= 8, "{joiner_report:?}");
}
