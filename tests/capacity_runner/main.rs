//! C2 capacity runner (`tests/capacity_runner/`): a standalone,
//! delivery-aware runner per the registered contract in
//! `docs/development/arm-capacity-audit.md` ("C2 first runner PR") and
//! issue #648.
//!
//! It spawns the real server binary as a separate process (or connects to an
//! external endpoint), drives deterministic scheduled relay traffic over
//! real WebSockets on ONE monotonic clock, and writes machine-readable
//! artifacts whose replay reproduces the outcome summary exactly.
//!
//! The tests here are the runner's own regression suite:
//!
//! - the small reliable relay scenario (the C2 acceptance gate), plus
//! - the negative controls: missing, duplicate, misrouted, and out-of-order
//!   deliveries, a paused generator, generator saturation, server
//!   termination, and a slow reader — each must invalidate the run with its
//!   explicit reason.
//!
//! Standalone use on a capacity host (release profile, external server):
//! `SIGNAL_FISH_CAPACITY_*` environment variables shape a run — see
//! `config::RunConfig::from_env`.
//!
//! Reconnect/churn schedules and latest/volatile delivery classes are later
//! C2 slices; their input surface already exists in [`config::RunConfig`].

#[path = "../websocket_test_helpers/mod.rs"]
mod websocket_test_helpers;

mod artifacts;
mod config;
mod diagnostics;
mod oracle;
mod records;
mod runner;
mod schedule;

use std::path::PathBuf;
use std::time::Duration;

use config::{ChurnSchedule, DeliveryClass, Encoding, RunConfig};
use records::{EventLog, ReceiptEvent, RunRecords, SentEvent};
use schedule::{build_plans, SenderPlan};

/// The small scenario at the heart of the C2 acceptance gate: one room, four
/// v3 clients, reliable relay traffic over real sockets, artifacts written,
/// and a replay of those artifacts reproducing the recorded summary exactly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn small_reliable_relay_scenario_passes_and_artifacts_replay_to_the_same_summary() {
    let output = tempfile::tempdir().expect("create output tempdir");
    let config = RunConfig {
        output_dir: output.path().to_path_buf(),
        ..scenario_config(Encoding::V3Json)
    };

    let outcome = runner::run(config).await.expect("scenario run completes");
    assert_eq!(outcome.output_dir, output.path());
    assert!(
        outcome.summary.valid,
        "the small relay scenario must be valid, got reasons {:?}",
        outcome.summary.reasons
    );
    assert_eq!(
        outcome.summary.totals.scheduled,
        outcome.summary.totals.sent
    );
    assert_eq!(outcome.summary.totals.unsent, 0);
    assert_eq!(outcome.summary.totals.outstanding, 0);
    assert!(
        outcome.summary.latency_us.samples > 0,
        "the measured window must produce latency samples"
    );
    for recipient in &outcome.summary.per_recipient {
        assert!(recipient.connected_through);
        assert_eq!(recipient.missing, 0);
        assert_eq!(recipient.duplicates, 0);
        assert_eq!(recipient.misrouted, 0);
        assert_eq!(recipient.out_of_order, 0);
    }

    // Every contract artifact is on disk.
    for file in [
        artifacts::MANIFEST_FILE,
        artifacts::DELIVERIES_FILE,
        artifacts::INTERVALS_FILE,
        artifacts::SUMMARY_FILE,
        artifacts::HISTOGRAM_FILE,
    ] {
        assert!(output.path().join(file).exists(), "missing artifact {file}");
    }
    // The manifest pins the run's identity and inputs.
    let manifest = artifacts::read_manifest(output.path()).expect("read manifest");
    assert_eq!(manifest.run_id, outcome.run_id);
    assert_eq!(manifest.config.seed, 1);
    assert_eq!(manifest.workload.senders, 4);
    assert!(manifest.server.binary_sha256.is_some());
    assert!(manifest.build.toolchain.is_some());

    // A replay of the raw events reproduces the recorded summary exactly.
    let replayed = artifacts::replay(output.path()).expect("replay artifacts");
    let recorded = serde_json::to_value(&outcome.summary).expect("serialize summary");
    let replayed = serde_json::to_value(&replayed).expect("serialize replay");
    assert_eq!(
        recorded, replayed,
        "replaying the artifacts must reproduce the outcome summary"
    );
}

/// A paused generator shows up as scheduled-send latency — the pause lands
/// in the send lag, not as reduced offered load: every scheduled message is
/// still emitted, and the run stays valid.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn injected_send_pause_lands_in_scheduled_send_latency_without_reducing_offered_load() {
    let output = tempfile::tempdir().expect("create output tempdir");
    let mut config = scenario_config(Encoding::V2Json);
    config.output_dir = output.path().to_path_buf();
    config.players_per_room = 3;
    config.pause_sends = Some(config::SendPause {
        after_seq: 5,
        duration: Duration::from_millis(300),
    });
    // The pause must stay inside the generator bound for the run to remain
    // a measurement; a bigger pause is the saturation control's job.
    config.generator_lag_bound = Duration::from_secs(2);

    let outcome = runner::run(config).await.expect("pause run completes");
    assert!(
        outcome.summary.valid,
        "a bounded pause is not a fault: {:?}",
        outcome.summary.reasons
    );
    assert_eq!(
        outcome.summary.totals.unsent, 0,
        "a pause must not reduce offered load"
    );
    assert!(
        outcome.summary.generator_lag_us.max_us >= 250_000,
        "the pause must appear in scheduled-send lag, got max {:?}",
        outcome.summary.generator_lag_us
    );
}

/// A generator that falls past its schedule invalidates the run with the
/// recorded lag, bound, and unsent work — saturation is loud, never a
/// silently stretched schedule.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generator_saturation_invalidates_the_run_with_the_recorded_lag_and_unsent_work() {
    let output = tempfile::tempdir().expect("create output tempdir");
    let mut config = scenario_config(Encoding::V2Json);
    config.output_dir = output.path().to_path_buf();
    config.players_per_room = 3;
    config.stall_senders = Some(Duration::from_millis(800));
    config.generator_lag_bound = Duration::from_millis(100);

    let outcome = runner::run(config).await.expect("stall run completes");
    assert!(!outcome.summary.valid);
    let saturated = outcome
        .summary
        .reasons
        .iter()
        .any(|reason| matches!(reason, oracle::InvalidReason::GeneratorSaturated { max_lag_us, bound_us: _ } if *max_lag_us >= 800_000));
    assert!(
        saturated,
        "expected a generator-saturation reason naming the stall lag, got {:?}",
        outcome.summary.reasons
    );
    assert!(
        outcome.summary.totals.unsent > 0,
        "stalled senders must leave explicit unsent work"
    );
    let replayed = artifacts::replay(output.path()).expect("replay artifacts");
    assert_eq!(
        serde_json::to_value(&replayed).expect("serialize replay"),
        serde_json::to_value(&outcome.summary).expect("serialize summary"),
    );
}

/// A terminated server invalidates the run with its explicit reason, and
/// every recipient keeps a gap-free in-order prefix (loss is only ever the
/// loud tail cut off with the connection).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn server_termination_invalidates_the_run_and_preserves_gap_free_prefixes() {
    let output = tempfile::tempdir().expect("create output tempdir");
    let mut config = scenario_config(Encoding::V3Json);
    config.output_dir = output.path().to_path_buf();
    config.kill_server_after = Some(Duration::from_millis(300));

    let outcome = runner::run(config).await.expect("kill run completes");
    assert!(!outcome.summary.valid);
    // The declared termination is the root fault. Its consequences are
    // disconnect tails (permitted), never silent holes or spurious extra
    // faults — those would mean the generator mis-accounted the kill.
    assert!(outcome
        .summary
        .reasons
        .contains(&oracle::InvalidReason::ServerTerminated));
    for forbidden in [
        oracle::InvalidReason::MissingDeliveries {
            count: 0,
            first: oracle::DeliveryKey {
                recipient: String::new(),
                sender: String::new(),
                seq: 0,
            },
        },
        oracle::InvalidReason::UnexpectedDisconnect { recipients: vec![] },
        oracle::InvalidReason::SendFailed {
            sender: String::new(),
            detail: String::new(),
        },
        oracle::InvalidReason::UnsentWork { count: 0 },
    ] {
        assert!(
            !outcome.summary.reasons.iter().any(|reason| {
                std::mem::discriminant(reason) == std::mem::discriminant(&forbidden)
            }),
            "termination must not surface as a silent hole or a spurious fault: {:?}",
            outcome.summary.reasons
        );
    }
    for recipient in &outcome.summary.per_recipient {
        assert!(
            !recipient.connected_through,
            "{}: every stream ends with the killed server",
            recipient.recipient
        );
        // The observed prefix is the whole truth: no missing (hole) count.
        assert_eq!(recipient.missing, 0);
    }
    assert!(
        outcome.summary.totals.receipts > 0,
        "deliveries before the kill must be recorded"
    );
    let replayed = artifacts::replay(output.path()).expect("replay artifacts");
    assert_eq!(
        serde_json::to_value(&replayed).expect("serialize replay"),
        serde_json::to_value(&outcome.summary).expect("serialize summary"),
    );
}

/// A reader that stops reading is evicted by the server's slow-consumer
/// path, the eviction is accounted by the server's counter, and the run is
/// invalidated as a declared non-measurement rather than silently
/// reporting partial delivery as capacity evidence.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_reader_is_evicted_recorded_and_accounted_by_the_server_counter() {
    let output = tempfile::tempdir().expect("create output tempdir");
    let mut config = scenario_config(Encoding::V2Json);
    config.output_dir = output.path().to_path_buf();
    // Enough volume to wedge a 4 KiB-window reader well inside the 300 ms
    // eviction grace (the same recipe as the slow-consumer suites).
    config.payload_bytes = 16 * 1_024;
    config.send_rate_per_sender = 25.0;
    config.slow_reader = true;
    // Saturation is not under test here: the stalled room's fan-out parks
    // on the wedged recipient until eviction.
    config.generator_lag_bound = Duration::from_secs(5);
    config.server_overlay = serde_json::json!({
        "session": { "default_topology": "relay" },
        "rate_limit": { "max_room_creations": 1_000_000 },
        "security": {
            "max_connections": 100_000,
            "max_connections_per_ip": 100_000
        },
        "websocket": {
            "send_queue_capacity": 8,
            "slow_consumer_timeout_ms": 300
        }
    });

    let outcome = runner::run(config)
        .await
        .expect("slow-reader run completes");
    assert!(!outcome.summary.valid);
    assert_eq!(
        outcome
            .summary
            .reasons
            .iter()
            .filter(|reason| matches!(reason, oracle::InvalidReason::SlowConsumerDisconnect { .. }))
            .count(),
        1,
        "the declared slow-reader fault is the hook's reason: {:?}",
        outcome.summary.reasons
    );
    let slow_reader = outcome
        .summary
        .per_recipient
        .iter()
        .find(|recipient| recipient.recipient == "r0p0")
        .expect("slow reader is on the roster");
    assert_eq!(
        slow_reader.received.values().sum::<u64>(),
        0,
        "a reader that never reads receives nothing"
    );

    // The eviction is real and accounted: the server counted it (the run's
    // own last scrape, so the evidence dies with the server process).
    let counters = outcome
        .final_counters
        .as_ref()
        .expect("the run recorded at least one server scrape");
    let accounted = counters
        .get("signal_fish_websocket_slow_consumer_disconnects_total")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    assert!(
        accounted >= 1,
        "the slow-consumer eviction must be accounted by the server counter, got {counters}"
    );

    let replayed = artifacts::replay(output.path()).expect("replay artifacts");
    assert_eq!(
        serde_json::to_value(&replayed).expect("serialize replay"),
        serde_json::to_value(&outcome.summary).expect("serialize summary"),
    );
}

// ---------------------------------------------------------------------------
// Oracle negative controls (deterministic, no server): the detector must
// catch each contract violation with its exact, named reason.
// ---------------------------------------------------------------------------

/// A missing delivery invalidates the run naming the exact first gap.
#[test]
fn a_missing_delivery_invalidates_the_run_with_the_exact_first_gap() {
    let context = unit_context();
    let mut records = complete_records(&context.plans);
    drop_receipt(&mut records, "r0p0", "r0p1", 2);
    let summary = oracle::summarize(&context.plans, &context.roster, &records, 1_000);
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .contains(&oracle::InvalidReason::MissingDeliveries {
                count: 1,
                first: oracle::DeliveryKey {
                    recipient: "r0p0".to_string(),
                    sender: "r0p1".to_string(),
                    seq: 2,
                },
            }),
        "expected the exact first gap, got {:?}",
        summary.reasons
    );
    // The pristine baseline these mutations start from is valid.
    let baseline = oracle::summarize(
        &context.plans,
        &context.roster,
        &complete_records(&context.plans),
        1_000,
    );
    assert!(
        baseline.valid,
        "baseline must be valid: {:?}",
        baseline.reasons
    );
}

/// A duplicate delivery invalidates the run naming the duplicate.
#[test]
fn a_duplicate_delivery_invalidates_the_run_naming_the_duplicate() {
    let context = unit_context();
    let mut records = complete_records(&context.plans);
    let duplicate = ReceiptEvent {
        recipient: "r0p0".to_string(),
        sender: "r0p1".to_string(),
        seq: 1,
        received_us: 999,
    };
    records.receipts.push(duplicate);
    let summary = oracle::summarize(&context.plans, &context.roster, &records, 1_000);
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .contains(&oracle::InvalidReason::DuplicateDeliveries {
                count: 1,
                first: oracle::DeliveryKey {
                    recipient: "r0p0".to_string(),
                    sender: "r0p1".to_string(),
                    seq: 1,
                },
            }),
        "expected the duplicate reason, got {:?}",
        summary.reasons
    );
}

/// A cross-room (out-of-roster) delivery is a misroute with a named key.
#[test]
fn a_misrouted_cross_room_delivery_invalidates_the_run() {
    let context = unit_context();
    let mut records = complete_records(&context.plans);
    records.receipts.push(ReceiptEvent {
        recipient: "r0p0".to_string(),
        sender: "r9p9".to_string(),
        seq: 0,
        received_us: 999,
    });
    let summary = oracle::summarize(&context.plans, &context.roster, &records, 1_000);
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .contains(&oracle::InvalidReason::MisroutedDeliveries {
                count: 1,
                first: oracle::DeliveryKey {
                    recipient: "r0p0".to_string(),
                    sender: "r9p9".to_string(),
                    seq: 0,
                },
            }),
        "expected the misroute reason, got {:?}",
        summary.reasons
    );
}

/// An out-of-order per-sender stream invalidates the run naming the first
/// inversion.
#[test]
fn an_out_of_order_stream_invalidates_the_run() {
    let context = unit_context();
    let mut records = complete_records(&context.plans);
    // Swap the arrival order of one stream's seqs 1 and 2.
    let swap = |records: &mut RunRecords| {
        let mut first: Option<usize> = None;
        let mut second: Option<usize> = None;
        for (index, receipt) in records.receipts.iter().enumerate() {
            let key = (
                receipt.recipient.as_str(),
                receipt.sender.as_str(),
                receipt.seq,
            );
            if key == ("r0p0", "r0p1", 1) {
                first = Some(index);
            }
            if key == ("r0p0", "r0p1", 2) {
                second = Some(index);
            }
        }
        let (first, second) = (
            first.expect("seq 1 receipt exists"),
            second.expect("seq 2 receipt exists"),
        );
        records.receipts.swap(first, second);
    };
    swap(&mut records);
    let summary = oracle::summarize(&context.plans, &context.roster, &records, 1_000);
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .contains(&oracle::InvalidReason::OutOfOrderDeliveries {
                count: 1,
                first: oracle::DeliveryKey {
                    recipient: "r0p0".to_string(),
                    sender: "r0p1".to_string(),
                    seq: 1,
                },
            }),
        "expected the out-of-order reason at the first inversion, got {:?}",
        summary.reasons
    );
}

/// Standalone invocation path: the environment parser accepts the small
/// default scenario and rejects unknown enum values loudly.
#[test]
fn the_environment_parser_shapes_a_run_and_rejects_unknown_values() {
    // Defaults parse (no env vars set in the test process for these keys).
    let config = RunConfig::from_env().expect("default env config parses");
    assert!(config.rooms >= 1);
    assert!(config.players_per_room >= 2);
    assert!(config.send_rate_per_sender > 0.0);

    // Unknown enum values fail loudly, naming the accepted values.
    std::env::set_var("SIGNAL_FISH_CAPACITY_ENCODING", "cbor");
    assert!(RunConfig::from_env().is_err());
    std::env::set_var("SIGNAL_FISH_CAPACITY_ENCODING", "v3-json");
    std::env::set_var("SIGNAL_FISH_CAPACITY_CLASS", "lossy");
    assert!(RunConfig::from_env().is_err());
    std::env::set_var("SIGNAL_FISH_CAPACITY_CLASS", "reliable");
    std::env::set_var("SIGNAL_FISH_CAPACITY_CHURN", "rejoin-storm");
    assert!(RunConfig::from_env().is_err());
    std::env::remove_var("SIGNAL_FISH_CAPACITY_ENCODING");
    std::env::remove_var("SIGNAL_FISH_CAPACITY_CLASS");
    std::env::remove_var("SIGNAL_FISH_CAPACITY_CHURN");
    assert!(RunConfig::from_env().is_ok());
}

/// Standalone entry point for a capacity host: configure the run entirely
/// through `SIGNAL_FISH_CAPACITY_*` environment variables (see
/// `config::RunConfig::from_env`), e.g. against a release-profile server:
///
/// ```text
/// SIGNAL_FISH_CAPACITY_OUTPUT_DIR=/tmp/cap-run \
/// cargo test --release --test capacity_runner \
///   a_standalone_env_configured_run_writes_artifacts_and_replays -- --ignored
/// ```
///
/// In CI the default configuration runs the same small scenario as the
/// acceptance gate; on a capacity host the variables shape the real cells.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "standalone capacity-host entry point: shape via SIGNAL_FISH_CAPACITY_*"]
async fn a_standalone_env_configured_run_writes_artifacts_and_replays() {
    let config = RunConfig::from_env().expect("SIGNAL_FISH_CAPACITY_* env is valid");
    let outcome = runner::run(config).await.expect("run completes");
    println!("run {} valid: {}", outcome.run_id, outcome.summary.valid);
    for reason in &outcome.summary.reasons {
        println!("reason: {reason:?}");
    }
    println!(
        "latency_us: {:#?}\ntotals: {:#?}",
        outcome.summary.latency_us, outcome.summary.totals
    );
    let replayed = artifacts::replay(&outcome.output_dir).expect("replay artifacts");
    assert_eq!(
        serde_json::to_value(&replayed).expect("serialize replay"),
        serde_json::to_value(&outcome.summary).expect("serialize summary"),
        "replaying the artifacts must reproduce the outcome summary"
    );
}

// ---------------------------------------------------------------------------
// Shared test scaffolding.
// ---------------------------------------------------------------------------

/// The small scenario config: one room, four clients, 96-byte payloads at 20
/// messages per sender-second, 200 ms warm-up, 1.2 s measurement.
fn scenario_config(encoding: Encoding) -> RunConfig {
    RunConfig {
        endpoint: None,
        seed: 1,
        rooms: 1,
        players_per_room: 4,
        encoding,
        payload_bytes: 96,
        send_rate_per_sender: 20.0,
        delivery_class: DeliveryClass::Reliable,
        warmup: Duration::from_millis(200),
        duration: Duration::from_millis(1_200),
        churn: ChurnSchedule::None,
        output_dir: PathBuf::from("."),
        generator_lag_bound: Duration::from_millis(250),
        drain_grace: Duration::from_millis(1_500),
        sample_interval: Duration::from_millis(250),
        server_overlay: RunConfig::default_server_overlay(),
        pause_sends: None,
        stall_senders: None,
        slow_reader: false,
        kill_server_after: None,
        room_code_prefix: None,
    }
}

/// Plans, roster, and complete event records for one small deterministic
/// workload — the baseline every oracle negative control mutates.
struct UnitContext {
    plans: Vec<SenderPlan>,
    roster: Vec<(String, u32)>,
}

fn unit_context() -> UnitContext {
    let mut config = scenario_config(Encoding::V2Json);
    config.warmup = Duration::ZERO;
    config.duration = Duration::from_millis(400);
    config.send_rate_per_sender = 10.0;
    let plans = build_plans(&config);
    let roster = plans
        .iter()
        .map(|plan| (plan.name.clone(), plan.room))
        .collect();
    UnitContext { plans, roster }
}

/// The complete, valid event set for `plans`: every scheduled send emitted
/// 5 µs late, delivered to every co-room peer 3 µs later, in order.
fn complete_records(plans: &[SenderPlan]) -> RunRecords {
    let log = EventLog::new();
    for plan in plans {
        for send in &plan.sends {
            log.push_sent(SentEvent {
                sender: plan.name.clone(),
                room: plan.room,
                seq: send.seq,
                intended_us: send.intended_us,
                sent_us: send.intended_us + 5,
                phase: send.phase,
            });
        }
    }
    for plan in plans {
        for other in plans {
            if other.room != plan.room || other.name == plan.name {
                continue;
            }
            for send in &other.sends {
                log.push_receipt(ReceiptEvent {
                    recipient: plan.name.clone(),
                    sender: other.name.clone(),
                    seq: send.seq,
                    received_us: send.intended_us + 8,
                });
            }
        }
    }
    log.snapshot()
}

fn drop_receipt(records: &mut RunRecords, recipient: &str, sender: &str, seq: u64) {
    let index = records
        .receipts
        .iter()
        .position(|receipt| {
            receipt.recipient == recipient && receipt.sender == sender && receipt.seq == seq
        })
        .expect("receipt to drop exists");
    records.receipts.remove(index);
}
