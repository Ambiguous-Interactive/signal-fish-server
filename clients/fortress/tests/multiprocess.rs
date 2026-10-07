#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

static LIVE_CELLS: std::sync::Mutex<()> = std::sync::Mutex::new(());

const CHILD_DEADLINE: Duration = Duration::from_secs(40);
const SERVER_READY_DEADLINE: Duration = Duration::from_secs(10);
const SERVER_SPAWN_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Default, Deserialize)]
struct Report {
    player_id: String,
    run_mode: String,
    max_active_admissions_per_callback: u64,
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

struct Peer(Option<Child>);
impl Peer {
    fn child(&mut self) -> &mut Child {
        self.0.as_mut().expect("live child")
    }
    fn output(mut self) -> std::io::Result<Output> {
        self.0
            .take()
            .expect("collect child once")
            .wait_with_output()
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
struct RoomFiles(PathBuf);
impl Drop for RoomFiles {
    fn drop(&mut self) {
        for path in [
            self.0.clone(),
            self.0.with_extension("active-creator"),
            self.0.with_extension("active-joiner"),
            self.0.with_extension("active-creator-tmp"),
            self.0.with_extension("active-joiner-tmp"),
        ] {
            let _ = fs::remove_file(path);
        }
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

fn server_command(server_bin: &str, port: u16) -> Command {
    let mut command = Command::new(server_bin);
    command.stdout(Stdio::null()).stderr(Stdio::inherit());

    /*
        Config loading applies environment overrides last. Scrub the whole
        namespace so ambient developer/runner settings cannot change auth,
        TURN, rate limits, or any other behavior under this regression.
    */
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
    command.env("SIGNAL_FISH__SERVER__DRAIN_GRACE_SECS", "1");
    command
}

fn spawn_server(server_bin: &str) -> (Server, u16) {
    let mut failures = Vec::new();
    for attempt in 1..=SERVER_SPAWN_ATTEMPTS {
        let port = free_port();
        let mut command = server_command(server_bin, port);

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

fn wait_outputs(mut first: Peer, mut second: Peer) -> (Output, Output) {
    if !wait_for(
        || {
            first.child().try_wait().expect("query creator").is_some()
                && second.child().try_wait().expect("query joiner").is_some()
        },
        CHILD_DEADLINE,
    ) {
        let _ = first.child().kill();
        let _ = second.child().kill();
        let first_output = first.output().expect("collect creator timeout");
        let second_output = second.output().expect("collect joiner timeout");
        panic!(
            "timed out waiting for game processes\ncreator stdout={}\ncreator stderr={}\njoiner stdout={}\njoiner stderr={}",
            String::from_utf8_lossy(&first_output.stdout),
            String::from_utf8_lossy(&first_output.stderr),
            String::from_utf8_lossy(&second_output.stdout),
            String::from_utf8_lossy(&second_output.stderr)
        );
    }
    (
        first.output().expect("collect creator output"),
        second.output().expect("collect joiner output"),
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

fn healthy_violations(report: &Report) -> Vec<&'static str> {
    let enqueued_rate =
        report.relay_frames_enqueued_during_run as f64 * 1000.0 / report.running_elapsed_ms as f64;
    let completed_rate =
        report.client_game_data_sent_during_run as f64 * 1000.0 / report.running_elapsed_ms as f64;
    let checks = [
        ("current_frame", report.current_frame >= 600),
        ("confirmed_frame", report.confirmed_frame >= 600),
        ("game_frame", report.game_frame >= 600),
        ("frames_advanced", report.frames_advanced >= 600),
        ("total_sent", report.client_game_data_sent >= 1200),
        (
            "active_sent",
            report.client_game_data_sent_during_run >= 1200,
        ),
        ("total_received", report.client_game_data_received >= 1200),
        ("total_enqueued", report.relay_frames_enqueued >= 1200),
        (
            "active_enqueued",
            report.relay_frames_enqueued_during_run >= 1200,
        ),
        ("relay_received", report.relay_frames_received >= 1200),
        (
            "send_conservation",
            report.relay_frames_enqueued == report.client_game_data_sent,
        ),
        (
            "receive_conservation",
            report.relay_frames_received == report.client_game_data_received,
        ),
        ("final_queue", report.final_pipeline_queue_depth == 0),
        ("peak_queue", report.peak_pipeline_queue_depth <= 64),
        ("queue_age", report.peak_oldest_queue_age_us <= 500000),
        ("relay_malformed", report.relay_malformed == 0),
        (
            "relay_wrong_destination",
            report.relay_wrong_destination == 0,
        ),
        ("relay_unknown_sender", report.relay_unknown_sender == 0),
        (
            "relay_outbound_overflow",
            report.relay_outbound_overflow == 0,
        ),
        ("relay_inbound_overflow", report.relay_inbound_overflow == 0),
        ("relay_encode_failures", report.relay_encode_failures == 0),
        (
            "relay_completion_underflow",
            report.relay_completion_underflow == 0,
        ),
        (
            "client_messages_undecodable",
            report.client_messages_undecodable == 0,
        ),
        ("checksums_mismatched", report.checksums_mismatched == 0),
        ("events_discarded_total", report.events_discarded_total == 0),
        ("stall_count", report.stall_count == 0),
        ("wait_recommendations", report.wait_recommendations == 0),
        ("checksum_samples", report.checksums_compared >= 8),
        (
            "checksum_matches",
            report.checksums_matched == report.checksums_compared,
        ),
        (
            "confirmation_lag",
            report.confirmation_lag_current <= 8 && report.confirmation_lag_max <= 8,
        ),
        (
            "active_wall_time",
            report.running_elapsed_ms >= 9000 && report.running_elapsed_ms <= 15000,
        ),
        ("enqueued_rate", enqueued_rate >= 120.0),
        ("completed_rate", completed_rate >= 120.0),
        (
            "sends_per_callback",
            report.client_game_data_sent_during_run > report.polling_callbacks_during_run * 2,
        ),
        ("game_checksum", report.game_checksum != 0),
        ("rollback_exercised", report.rollback_count > 0),
        ("rollback_depth", report.max_rollback_depth <= 8),
        ("relay_send_retries", report.relay_send_retries <= 8),
    ];
    checks
        .into_iter()
        .filter_map(|(name, passed)| (!passed).then_some(name))
        .collect()
}

fn assert_healthy(name: &str, report: &Report) {
    let violations = healthy_violations(report);
    assert!(
        violations.is_empty(),
        "{name}: healthy violations={violations:?}, {report:?}"
    );
    assert!(
        report.max_active_admissions_per_callback > 1,
        "{name}: healthy workload never admitted multiple frames in a callback"
    );
}

fn assert_expected_negative(name: &str, report: &Report) {
    assert_eq!(report.run_mode, "negative_one_admission_per_callback");
    assert_eq!(report.polling_callbacks_during_run, 600);
    assert_eq!(report.max_active_admissions_per_callback, 1);
    assert!(
        report.client_game_data_sent_during_run * 10 >= report.polling_callbacks_during_run * 9,
        "{name}: vacuous completed workload: {report:?}"
    );
    assert!(
        report.relay_frames_enqueued_during_run * 10 >= report.polling_callbacks_during_run * 9,
        "{name}: vacuous enqueued workload: {report:?}"
    );
    assert!(
        report.confirmed_frame > 0 && report.frames_advanced > 0 && report.checksums_compared > 0
    );
    let violations = healthy_violations(report);
    for required in ["completed_rate", "sends_per_callback"] {
        assert!(
            violations.contains(&required),
            "{name}: negative unexpectedly passes {required}: {violations:?}, {report:?}"
        );
    }
    let permitted = [
        "current_frame",
        "confirmed_frame",
        "game_frame",
        "frames_advanced",
        "total_sent",
        "active_sent",
        "total_received",
        "total_enqueued",
        "active_enqueued",
        "relay_received",
        "queue_age",
        "stall_count",
        "wait_recommendations",
        "checksum_samples",
        "confirmation_lag",
        "enqueued_rate",
        "completed_rate",
        "sends_per_callback",
        "rollback_exercised",
        "rollback_depth",
    ];
    assert!(
        violations
            .iter()
            .all(|violation| permitted.contains(violation)),
        "{name}: unrelated failure: {violations:?}, {report:?}"
    );
}

fn healthy_report_fixture() -> Report {
    Report {
        current_frame: 600,
        confirmed_frame: 600,
        game_frame: 600,
        frames_advanced: 600,
        client_game_data_sent: 1800,
        client_game_data_sent_during_run: 1800,
        client_game_data_received: 1800,
        relay_frames_enqueued: 1800,
        relay_frames_enqueued_during_run: 1800,
        relay_frames_received: 1800,
        checksums_compared: 8,
        checksums_matched: 8,
        running_elapsed_ms: 10000,
        polling_callbacks_during_run: 600,
        game_checksum: 1,
        rollback_count: 1,
        ..Report::default()
    }
}

#[test]
fn healthy_validator_rejects_capped_throughput_and_unrelated_corruption() {
    let baseline = healthy_report_fixture();
    assert!(healthy_violations(&baseline).is_empty());
    for (completed, elapsed, expected) in [
        (1800, 10000, false),
        (600, 10000, true),
        (1199, 10000, true),
        (1201, 10000, false),
    ] {
        let report = Report {
            client_game_data_sent_during_run: completed,
            running_elapsed_ms: elapsed,
            ..baseline.clone()
        };
        assert_eq!(
            healthy_violations(&report).contains(&"completed_rate"),
            expected
        );
    }
    let corrupt = Report {
        relay_wrong_destination: 1,
        ..baseline
    };
    assert!(healthy_violations(&corrupt).contains(&"relay_wrong_destination"));
}

#[test]
fn expected_negative_requires_real_work_clean_telemetry_and_throughput_rejection() {
    let baseline = Report {
        run_mode: "negative_one_admission_per_callback".to_string(),
        max_active_admissions_per_callback: 1,
        client_game_data_sent_during_run: 600,
        relay_frames_enqueued_during_run: 600,
        ..healthy_report_fixture()
    };
    for (name, report, accepted) in [
        ("full driven budget", baseline.clone(), true),
        (
            "ninety percent boundary",
            Report {
                client_game_data_sent_during_run: 540,
                relay_frames_enqueued_during_run: 540,
                ..baseline.clone()
            },
            true,
        ),
        (
            "vacuous completions",
            Report {
                client_game_data_sent_during_run: 539,
                ..baseline.clone()
            },
            false,
        ),
        (
            "incomplete callbacks",
            Report {
                polling_callbacks_during_run: 599,
                ..baseline.clone()
            },
            false,
        ),
        (
            "transport corruption",
            Report {
                relay_wrong_destination: 1,
                ..baseline.clone()
            },
            false,
        ),
        (
            "no game advancement",
            Report {
                frames_advanced: 0,
                ..baseline.clone()
            },
            false,
        ),
        (
            "no checksum comparison",
            Report {
                checksums_compared: 0,
                checksums_matched: 0,
                ..baseline.clone()
            },
            false,
        ),
        (
            "healthy completion rate",
            Report {
                client_game_data_sent_during_run: 1201,
                ..baseline.clone()
            },
            false,
        ),
        (
            "slow callback pacing",
            Report {
                running_elapsed_ms: 20000,
                ..baseline.clone()
            },
            false,
        ),
        (
            "too-fast callback pacing",
            Report {
                running_elapsed_ms: 4000,
                ..baseline.clone()
            },
            false,
        ),
    ] {
        assert_eq!(
            std::panic::catch_unwind(|| assert_expected_negative(name, &report)).is_ok(),
            accepted,
            "{name}"
        );
    }
}

#[test]
fn two_fortress_game_processes_sustain_60fps_through_real_server() {
    run_cell("healthy");
}

#[test]
fn one_admission_per_callback_is_rejected_by_the_healthy_throughput_validator() {
    run_cell("negative-one-admission-per-callback");
}

fn run_cell(mode: &str) {
    let _serial = LIVE_CELLS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server_bin = std::env::var("SIGNAL_FISH_SERVER_BIN")
        .expect("SIGNAL_FISH_SERVER_BIN must point to a freshly built Signal Fish Server binary");
    assert!(
        Path::new(&server_bin).is_absolute(),
        "SIGNAL_FISH_SERVER_BIN must be absolute so child cwd changes cannot select another binary"
    );
    let (mut server, port) = spawn_server(&server_bin);
    run_pair_on_server(mode, &mut server, port);
}

fn run_pair_on_server(mode: &str, server: &mut Server, port: u16) -> [String; 2] {
    let peer_bin = env!("CARGO_BIN_EXE_fortress-relay-peer");
    let room_file = temp_room_file();
    let _files = RoomFiles(room_file.clone());
    assert!(
        server.0.try_wait().expect("query server").is_none(),
        "server exited early"
    );

    let url = format!("ws://127.0.0.1:{port}/v2/ws");
    let creator = Peer(Some(
        Command::new(peer_bin)
            .args([&url, "creator"])
            .arg(&room_file)
            .arg(mode)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn creator game process"),
    ));
    if !wait_for(
        || fs::metadata(&room_file).is_ok_and(|metadata| metadata.len() > 0),
        Duration::from_secs(10),
    ) {
        let mut creator = creator;
        let _ = creator.child().kill();
        let output = creator.output().expect("collect creator timeout");
        panic!(
            "timed out waiting for creator room code\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let room_code = fs::read_to_string(&room_file).expect("read room code");
    let joiner = Peer(Some(
        Command::new(peer_bin)
            .args([&url, "joiner"])
            .arg(&room_file)
            .arg(room_code.trim())
            .arg(mode)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn joiner game process"),
    ));

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
    for (name, report) in [("creator", &creator_report), ("joiner", &joiner_report)] {
        if mode == "healthy" {
            assert_eq!(report.run_mode, "healthy");
            assert_healthy(name, report);
        } else {
            assert_expected_negative(name, report);
        }
    }
    if mode != "healthy" {
        println!("BUSTED fortress-native expected negative control: completed-rate and per-callback healthy gates rejected both clean peers");
    }
    [creator_report.player_id, joiner_report.player_id]
}

#[cfg(unix)]
fn assert_drain_ready(value: &serde_json::Value, role: &str) -> String {
    assert_eq!(value["role"].as_str(), Some(role));
    let id = value["player_id"].as_str().expect("barrier player id");
    assert!(uuid::Uuid::parse_str(id).is_ok(), "valid player identity");
    assert!(
        value["confirmed_frame"]
            .as_i64()
            .is_some_and(|frame| (120..600).contains(&frame)),
        "actual unfinished game progress: {value}"
    );
    for key in [
        "frames_advanced",
        "rollback_count",
        "checksums_compared",
        "sent",
        "received",
        "sent_ledger",
        "received_ledger",
    ] {
        assert!(
            value[key].as_u64().is_some_and(|count| count > 0),
            "nonvacuous {key}: {value}"
        );
    }
    assert_eq!(
        value["checksums_compared"], value["checksums_matched"],
        "matching checksums"
    );
    for key in [
        "checksums_mismatched",
        "malformed",
        "wrong_destination",
        "unknown_sender",
        "inbound_overflow",
        "outbound_overflow",
        "encode_failures",
        "completion_underflow",
    ] {
        assert_eq!(
            value[key].as_u64(),
            Some(0),
            "clean pre-fault {key}: {value}"
        );
    }
    id.to_string()
}

#[cfg(unix)]
fn shutdown_failure(output: &Output) -> Result<(), &'static str> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        return Err("accepted shutdown as success");
    }
    if output.status.code() != Some(1) {
        return Err("abnormal process exit");
    }
    if !output.stdout.is_empty() {
        return Err("healthy report after shutdown");
    }
    if !stderr.contains("server going away: deadline_ms=") {
        return Err("missing GoingAway");
    }
    if !stderr.contains("server disconnected peer:")
        || !stderr.contains("server_shutdown")
        || !stderr.contains("code=Some(4000)")
    {
        return Err("wrong failure cause");
    }
    if stderr.contains("deadline expired") || stderr.contains("panicked") {
        return Err("deadline or panic");
    }
    Ok(())
}

#[cfg(unix)]
fn assert_shutdown_failure(name: &str, output: &Output) {
    assert_eq!(
        shutdown_failure(output),
        Ok(()),
        "{name}: status={}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn shutdown_outcome_requires_the_advisory_and_authoritative_close() {
    use std::os::unix::process::ExitStatusExt;
    let valid =
        "server going away: deadline_ms=123\nError: server disconnected peer: code=Some(4000), server_shutdown";
    for (name, status, stdout, stderr, expected) in [
        ("causal abort", 1, "", valid, Ok(())),
        ("success", 0, "", valid, Err("accepted shutdown as success")),
        ("other exit code", 2, "", valid, Err("abnormal process exit")),
        ("signal termination", -9, "", valid, Err("abnormal process exit")),
        ("healthy report", 1, "{}", valid, Err("healthy report after shutdown")),
        ("missing notice", 1, "", "server disconnected peer: server_shutdown", Err("missing GoingAway")),
        ("peer left", 1, "", "server going away: deadline_ms=123\nSignal Fish peer left before final ack", Err("wrong failure cause")),
        ("wrong close", 1, "", "server going away: deadline_ms=123\nserver disconnected peer: protocol_error", Err("wrong failure cause")),
        ("wrong close code", 1, "", "server going away: deadline_ms=123\nserver disconnected peer: code=Some(1001), server_shutdown", Err("wrong failure cause")),
        ("panic", 1, "", "server going away: deadline_ms=123\nserver disconnected peer: code=Some(4000), server_shutdown\npanicked", Err("deadline or panic")),
        ("deadline", 1, "", "server going away: deadline_ms=123\nserver disconnected peer: code=Some(4000), server_shutdown\npeer deadline expired", Err("deadline or panic")),
    ] {
        let output = Output { status: std::process::ExitStatus::from_raw(if status < 0 { -status } else { status << 8 }),
            stdout: stdout.as_bytes().to_vec(), stderr: stderr.as_bytes().to_vec() };
        assert_eq!(shutdown_failure(&output), expected, "{name}");
    }
}

#[cfg(unix)]
#[test]
fn graceful_server_drain_fails_active_games_and_same_port_restart_completes_new_games() {
    let _serial = LIVE_CELLS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let server_bin = std::env::var("SIGNAL_FISH_SERVER_BIN").expect("fresh real server binary");
    assert!(Path::new(&server_bin).is_absolute());
    let (mut server, port) = spawn_server(&server_bin);
    let room = temp_room_file();
    let _files = RoomFiles(room.clone());
    let url = format!("ws://127.0.0.1:{port}/v2/ws");
    let peer_bin = env!("CARGO_BIN_EXE_fortress-relay-peer");
    let mut creator = Peer(Some(
        Command::new(peer_bin)
            .args([&url, "creator"])
            .arg(&room)
            .arg("drain-probe")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn drain creator"),
    ));
    assert!(
        wait_for(
            || fs::metadata(&room).is_ok_and(|m| m.len() > 0),
            Duration::from_secs(10)
        ),
        "room readiness"
    );
    let code = fs::read_to_string(&room).expect("room code");
    let mut joiner = Peer(Some(
        Command::new(peer_bin)
            .args([&url, "joiner"])
            .arg(&room)
            .arg(code.trim())
            .arg("drain-probe")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn drain joiner"),
    ));
    let paths = [
        room.with_extension("active-creator"),
        room.with_extension("active-joiner"),
    ];
    assert!(
        wait_for(
            || paths.iter().all(|path| path.exists()),
            Duration::from_secs(10)
        ),
        "both active game barriers"
    );
    let old_ids: Vec<_> = paths
        .iter()
        .zip(["creator", "joiner"])
        .map(|(path, role)| {
            let value = serde_json::from_slice(&fs::read(path).expect("barrier bytes"))
                .expect("atomic barrier JSON");
            println!("pre-drain {role}: {value}");
            assert_drain_ready(&value, role)
        })
        .collect();
    assert_ne!(old_ids[0], old_ids[1]);
    assert!(creator.child().try_wait().expect("creator live").is_none());
    assert!(joiner.child().try_wait().expect("joiner live").is_none());
    assert!(server.0.try_wait().expect("server live").is_none());
    let signal_at = Instant::now();
    assert!(Command::new("kill")
        .args(["-TERM", &server.0.id().to_string()])
        .status()
        .expect("deliver SIGTERM")
        .success());
    assert!(
        wait_for(
            || creator
                .child()
                .try_wait()
                .expect("creator status")
                .is_some()
                && joiner.child().try_wait().expect("joiner status").is_some(),
            Duration::from_secs(10)
        ),
        "peers must abort promptly after actual drain"
    );
    let creator_output = creator.output().expect("creator output");
    let joiner_output = joiner.output().expect("joiner output");
    assert_shutdown_failure("creator", &creator_output);
    assert_shutdown_failure("joiner", &joiner_output);
    assert!(
        wait_for(
            || server.0.try_wait().expect("server drain status").is_some(),
            Duration::from_secs(15).saturating_sub(signal_at.elapsed())
        ),
        "bounded graceful server exit"
    );
    assert!(
        server.0.wait().expect("reap drained server").success(),
        "graceful server exits successfully"
    );
    let mut restarted = server_command(&server_bin, port)
        .spawn()
        .expect("restart exact same port");
    if let Err(error) = wait_for_server(&mut restarted, port) {
        let _ = restarted.kill();
        let _ = restarted.wait();
        panic!("restart readiness: {error}");
    }
    let mut restarted = Server(restarted);
    let new_ids = run_pair_on_server("healthy", &mut restarted, port);
    assert!(
        new_ids
            .iter()
            .all(|id| uuid::Uuid::parse_str(id).is_ok() && !old_ids.contains(id)),
        "new sessions after restart"
    );
    println!("DRAIN_RESTART fortress-native: active peers failed with server_shutdown; fresh games completed on the same port");
}
