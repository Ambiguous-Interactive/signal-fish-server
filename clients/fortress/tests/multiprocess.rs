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
        .arg(mode)
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
        .arg(mode)
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
}
