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
//! - the small reliable relay scenario (the C2 acceptance gate),
//! - the latest/volatile delivery-class cells: policy loss with exact gap
//!   accounting over real sockets, in server-counter agreement,
//! - the reconnect-burst cell: a mid-run disconnect/rejoin storm whose
//!   streams must complete exactly once across the victims' new
//!   incarnations,
//! - the negative controls: missing, duplicate, misrouted, and out-of-order
//!   deliveries, unreported lossy-class holes, gap-report violations,
//!   stale-epoch and below-tail deliveries across a storm, an unperformed
//!   storm, a paused generator, generator saturation, server termination,
//!   and a slow reader — each must invalidate the run with its explicit
//!   reason.
//!
//! Standalone use on a capacity host (release profile, external server):
//! `CAPACITY_RUNNER_*` environment variables shape a run — see
//! `config::RunConfig::from_env`.

#[path = "../websocket_test_helpers/mod.rs"]
mod websocket_test_helpers;

mod artifacts;
mod config;
mod diagnostics;
mod oracle;
mod records;
mod runner;
mod schedule;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use config::{ChurnSchedule, DeliveryClass, Encoding, RunConfig};
use records::{EventLog, GapEvent, ReceiptEvent, RunRecords, SentEvent};
use schedule::{build_run_shape, ChurnPlan, SenderPlan};
use signal_fish_server::protocol::DeliveryGapReason;

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
    // faults — those would mean the generator accounted the kill wrongly.
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
                epoch: 0,
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
// Latest/volatile delivery-class cells (issue #648, second runner PR).
// ---------------------------------------------------------------------------

/// The server overlay for the lossy-class pressure cells: a tiny outbound
/// queue and a bounded kernel handoff make the paused reader's pressure
/// phase deterministic (the burst exceeds kernel absorption several times
/// over), while the control lane grows to hold every gap report the pause
/// produces. All overrides are recorded in the manifest and hashed.
fn pressure_overlay() -> serde_json::Value {
    serde_json::json!({
        "session": { "default_topology": "relay" },
        "rate_limit": { "max_room_creations": 1_000_000 },
        "security": {
            "max_connections": 100_000,
            "max_connections_per_ip": 100_000
        },
        "websocket": {
            "send_queue_capacity": 8,
            "control_queue_capacity": 4_096,
            "socket_send_buffer_bytes": 16_384
        }
    })
}

/// One sender at 1000 messages/second, one recipient that does not read for
/// 600 ms on a clamped socket: the newest-value (`latest`, one key) cell.
/// Supersession must be observable, every omitted sequence must carry its
/// exact gap report, the run must stay valid, and the server's superseded
/// counter must agree with the oracle's gap coverage exactly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn latest_pressure_supersedes_with_exact_gap_accounting_in_server_counter_agreement() {
    let output = tempfile::tempdir().expect("create output tempdir");
    let mut config = scenario_config(Encoding::V3Json);
    config.output_dir = output.path().to_path_buf();
    config.players_per_room = 2;
    config.delivery_class = DeliveryClass::Latest;
    config.latest_keys_per_sender = 1;
    config.send_rate_per_sender = 1_000.0;
    config.warmup = Duration::from_millis(50);
    config.duration = Duration::from_millis(800);
    config.pause_reads = Some(Duration::from_millis(600));
    config.generator_lag_bound = Duration::from_millis(500);
    config.drain_grace = Duration::from_secs(2);
    config.server_overlay = pressure_overlay();

    let outcome = runner::run(config).await.expect("latest run completes");
    assert!(
        outcome.summary.valid,
        "supersession with exact gap accounting is contract-legal: {:?}",
        outcome.summary.reasons
    );
    // The pressure phase happened and was loud: many omissions, each
    // gap-covered.
    assert!(
        outcome.summary.totals.gap_covered >= 100,
        "a 600 ms read pause at 1000/s must supersede far more than the \
         ~130-frame kernel handoff, got {:?}",
        outcome.summary.totals
    );
    assert!(
        outcome.summary.totals.receipts < outcome.summary.totals.scheduled,
        "policy loss must be visible in the totals: {:?}",
        outcome.summary.totals
    );
    let reader = outcome
        .summary
        .per_recipient
        .iter()
        .find(|recipient| recipient.recipient == "r0p0")
        .expect("the paused reader is on the roster");
    assert!(
        reader.gap_covered >= 100,
        "the paused reader absorbs the policy loss, got {reader:?}"
    );
    assert_eq!(
        reader.missing, 0,
        "every omission must be gap-covered, not silent"
    );

    // Server-side accounting agrees exactly: the spawned server serves only
    // this run, and one supersession emits exactly one gap report.
    let counters = outcome
        .final_counters
        .as_ref()
        .expect("the run recorded at least one server scrape");
    let superseded = counters
        .get("class_outcome_superseded")
        .and_then(serde_json::Value::as_u64)
        .expect("class outcomes are recorded for a latest run");
    assert_eq!(
        superseded, outcome.summary.totals.gap_covered,
        "the server's superseded counter must equal the validated gap coverage"
    );

    let replayed = artifacts::replay(output.path()).expect("replay artifacts");
    assert_eq!(
        serde_json::to_value(&replayed).expect("serialize replay"),
        serde_json::to_value(&outcome.summary).expect("serialize summary"),
    );
}

/// The same pressure cell over `volatile`: the oldest queued message is
/// evicted, every eviction carries its `volatile_dropped` gap report, and
/// the server's dropped counter agrees with the validated coverage.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn volatile_pressure_evicts_the_oldest_with_exact_gap_accounting_in_counter_agreement() {
    let output = tempfile::tempdir().expect("create output tempdir");
    let mut config = scenario_config(Encoding::V3Json);
    config.output_dir = output.path().to_path_buf();
    config.players_per_room = 2;
    config.delivery_class = DeliveryClass::Volatile;
    config.send_rate_per_sender = 1_000.0;
    config.warmup = Duration::from_millis(50);
    config.duration = Duration::from_millis(800);
    config.pause_reads = Some(Duration::from_millis(600));
    config.generator_lag_bound = Duration::from_millis(500);
    config.drain_grace = Duration::from_secs(2);
    config.server_overlay = pressure_overlay();

    let outcome = runner::run(config).await.expect("volatile run completes");
    assert!(
        outcome.summary.valid,
        "eviction with exact gap accounting is contract-legal: {:?}",
        outcome.summary.reasons
    );
    assert!(outcome.summary.totals.gap_covered >= 100);
    assert!(outcome.summary.totals.receipts < outcome.summary.totals.scheduled);
    let reader = outcome
        .summary
        .per_recipient
        .iter()
        .find(|recipient| recipient.recipient == "r0p0")
        .expect("the paused reader is on the roster");
    assert!(reader.gap_covered >= 100);
    assert_eq!(reader.missing, 0);

    let counters = outcome
        .final_counters
        .as_ref()
        .expect("the run recorded at least one server scrape");
    let dropped = counters
        .get("class_outcome_dropped")
        .and_then(serde_json::Value::as_u64)
        .expect("class outcomes are recorded for a volatile run");
    assert_eq!(
        dropped, outcome.summary.totals.gap_covered,
        "the server's volatile dropped counter must equal the validated gap coverage"
    );

    let replayed = artifacts::replay(output.path()).expect("replay artifacts");
    assert_eq!(
        serde_json::to_value(&replayed).expect("serialize replay"),
        serde_json::to_value(&outcome.summary).expect("serialize summary"),
    );
}

/// Distinct coalescing keys never coalesce: a latest run whose keys rotate
/// past every send delivers the complete stream — the key-composition half
/// of the latest contract, over real sockets.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn latest_with_distinct_keys_delivers_every_message_without_policy_loss() {
    let output = tempfile::tempdir().expect("create output tempdir");
    let mut config = scenario_config(Encoding::V3Json);
    config.output_dir = output.path().to_path_buf();
    config.delivery_class = DeliveryClass::Latest;
    config.latest_keys_per_sender = 1_000_000;

    // Every scheduled send must reach every co-room peer exactly once.
    let expected_per_recipient =
        config.warmup_sends_per_sender() + config.measured_sends_per_sender();
    let expected_receipts = expected_per_recipient
        * u64::from(config.players_per_room)
        * u64::from(config.players_per_room - 1);

    let outcome = runner::run(config).await.expect("latest run completes");
    assert!(
        outcome.summary.valid,
        "distinct keys must coalesce nothing: {:?}",
        outcome.summary.reasons
    );
    assert_eq!(outcome.summary.totals.gap_covered, 0);
    assert_eq!(outcome.summary.totals.outstanding, 0);
    assert_eq!(outcome.summary.totals.receipts, expected_receipts);
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
    let summary = oracle::summarize(
        &context.plans,
        &context.roster,
        &records,
        1_000,
        context.delivery_class,
        &ChurnPlan::default(),
    );
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .contains(&oracle::InvalidReason::MissingDeliveries {
                count: 1,
                first: oracle::DeliveryKey {
                    recipient: "r0p0".to_string(),
                    sender: "r0p1".to_string(),
                    epoch: 1,
                    seq: 3,
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
        context.delivery_class,
        &ChurnPlan::default(),
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
        epoch: 1,
        server_seq: 2,
        received_us: 999,
    };
    records.receipts.push(duplicate);
    let summary = oracle::summarize(
        &context.plans,
        &context.roster,
        &records,
        1_000,
        context.delivery_class,
        &ChurnPlan::default(),
    );
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .contains(&oracle::InvalidReason::DuplicateDeliveries {
                count: 1,
                first: oracle::DeliveryKey {
                    recipient: "r0p0".to_string(),
                    sender: "r0p1".to_string(),
                    epoch: 1,
                    seq: 2,
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
        epoch: 1,
        server_seq: 1,
        received_us: 999,
    });
    let summary = oracle::summarize(
        &context.plans,
        &context.roster,
        &records,
        1_000,
        context.delivery_class,
        &ChurnPlan::default(),
    );
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .contains(&oracle::InvalidReason::MisroutedDeliveries {
                count: 1,
                first: oracle::DeliveryKey {
                    recipient: "r0p0".to_string(),
                    sender: "r9p9".to_string(),
                    epoch: 1,
                    seq: 1,
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
    let summary = oracle::summarize(
        &context.plans,
        &context.roster,
        &records,
        1_000,
        context.delivery_class,
        &ChurnPlan::default(),
    );
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .contains(&oracle::InvalidReason::OutOfOrderDeliveries {
                count: 1,
                first: oracle::DeliveryKey {
                    recipient: "r0p0".to_string(),
                    sender: "r0p1".to_string(),
                    epoch: 1,
                    seq: 2,
                },
            }),
        "expected the out-of-order reason at the first inversion, got {:?}",
        summary.reasons
    );
}

// ---------------------------------------------------------------------------
// Lossy-class oracle controls (deterministic, no server): the coverage
// detector must require exact gap accounting and reject every violation.
// ---------------------------------------------------------------------------

/// A helper: one exact single-sequence gap for `recipient` missing `seq`
/// from `sender` (the server stamps 1-based, so ledger `seq` is `seq + 1`).
fn gap_for(recipient: &str, sender: &str, seq: u64, reason: DeliveryGapReason) -> GapEvent {
    GapEvent {
        recipient: recipient.to_string(),
        sender: sender.to_string(),
        epoch: 1,
        from_seq: seq + 1,
        to_seq: seq + 1,
        reason,
    }
}

/// In a latest run, an omission with no gap report is silent loss.
#[test]
fn a_latest_omission_without_a_gap_report_is_missing_work() {
    let context = unit_context_with_class(DeliveryClass::Latest);
    let mut records = complete_records(&context.plans);
    drop_receipt(&mut records, "r0p0", "r0p1", 2);
    let summary = oracle::summarize(
        &context.plans,
        &context.roster,
        &records,
        1_000,
        context.delivery_class,
        &ChurnPlan::default(),
    );
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .contains(&oracle::InvalidReason::MissingDeliveries {
                count: 1,
                first: oracle::DeliveryKey {
                    recipient: "r0p0".to_string(),
                    sender: "r0p1".to_string(),
                    epoch: 1,
                    seq: 3,
                },
            }),
        "expected the exact first hole, got {:?}",
        summary.reasons
    );
}

/// The same omission with its exact gap report is contract-legal, and the
/// coverage lands in the totals.
#[test]
fn a_gap_reported_latest_omission_is_valid_and_accounted() {
    let context = unit_context_with_class(DeliveryClass::Latest);
    let mut records = complete_records(&context.plans);
    drop_receipt(&mut records, "r0p0", "r0p1", 2);
    records.gaps.push(gap_for(
        "r0p0",
        "r0p1",
        2,
        DeliveryGapReason::LatestSuperseded,
    ));
    let summary = oracle::summarize(
        &context.plans,
        &context.roster,
        &records,
        1_000,
        context.delivery_class,
        &ChurnPlan::default(),
    );
    assert!(
        summary.valid,
        "supersession with its exact report is legal: {:?}",
        summary.reasons
    );
    assert_eq!(summary.totals.gap_covered, 1);
    let reader = summary
        .per_recipient
        .iter()
        .find(|recipient| recipient.recipient == "r0p0")
        .expect("recipient on roster");
    assert_eq!(reader.gap_covered, 1);
    assert_eq!(reader.missing, 0);
}

/// A gap range overlapping an already-delivered sequence double-covers the
/// stream and is a violation.
#[test]
fn a_gap_overlapping_a_delivery_is_invalid() {
    let context = unit_context_with_class(DeliveryClass::Latest);
    let mut records = complete_records(&context.plans);
    records.gaps.push(gap_for(
        "r0p0",
        "r0p1",
        2,
        DeliveryGapReason::LatestSuperseded,
    ));
    let summary = oracle::summarize(
        &context.plans,
        &context.roster,
        &records,
        1_000,
        context.delivery_class,
        &ChurnPlan::default(),
    );
    assert!(!summary.valid);
    assert!(
        summary.reasons.iter().any(|reason| matches!(
            reason,
            oracle::InvalidReason::InvalidGapReports { count: 1, .. }
        )),
        "expected the invalid-gap reason, got {:?}",
        summary.reasons
    );

    // A rejected range covers nothing: drop seq 3 and report a range
    // spanning the delivered seq 2 and the missing seq 3 — the overlap
    // rejects the whole range, so seq 3 stays an uncovered hole.
    let mut records = complete_records(&context.plans);
    drop_receipt(&mut records, "r0p0", "r0p1", 3);
    let mut spanning = gap_for("r0p0", "r0p1", 2, DeliveryGapReason::LatestSuperseded);
    spanning.to_seq = 4; // server range 3..=4 = ledger seqs 2..=3
    records.gaps.push(spanning);
    let summary = oracle::summarize(
        &context.plans,
        &context.roster,
        &records,
        1_000,
        context.delivery_class,
        &ChurnPlan::default(),
    );
    assert!(!summary.valid);
    assert_eq!(summary.totals.gap_covered, 0);
    assert!(
        summary
            .reasons
            .contains(&oracle::InvalidReason::MissingDeliveries {
                count: 1,
                first: oracle::DeliveryKey {
                    recipient: "r0p0".to_string(),
                    sender: "r0p1".to_string(),
                    epoch: 1,
                    seq: 4,
                },
            }),
        "the rejected range must not cover seq 3, got {:?}",
        summary.reasons
    );
}

/// A reason the run's class cannot produce is a violation: volatile
/// eviction never supersedes, so a volatile run cannot carry
/// `latest_superseded`.
#[test]
fn a_gap_reason_the_class_cannot_produce_is_invalid() {
    let context = unit_context_with_class(DeliveryClass::Volatile);
    let mut records = complete_records(&context.plans);
    drop_receipt(&mut records, "r0p0", "r0p1", 2);
    records.gaps.push(gap_for(
        "r0p0",
        "r0p1",
        2,
        DeliveryGapReason::LatestSuperseded,
    ));
    let summary = oracle::summarize(
        &context.plans,
        &context.roster,
        &records,
        1_000,
        context.delivery_class,
        &ChurnPlan::default(),
    );
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .iter()
            .any(|reason| matches!(reason, oracle::InvalidReason::InvalidGapReports { .. })),
        "expected the invalid-gap reason, got {:?}",
        summary.reasons
    );
}

/// A gap range reaching beyond the sender's sent stream is a violation.
#[test]
fn a_gap_beyond_the_sent_stream_is_invalid() {
    let context = unit_context_with_class(DeliveryClass::Volatile);
    let mut records = complete_records(&context.plans);
    let mut beyond = gap_for("r0p0", "r0p1", 2, DeliveryGapReason::VolatileDropped);
    beyond.to_seq = 900; // ledger seqs only reach 3
    records.gaps.push(beyond);
    let summary = oracle::summarize(
        &context.plans,
        &context.roster,
        &records,
        1_000,
        context.delivery_class,
        &ChurnPlan::default(),
    );
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .iter()
            .any(|reason| matches!(reason, oracle::InvalidReason::InvalidGapReports { .. })),
        "expected the invalid-gap reason, got {:?}",
        summary.reasons
    );
}

/// Reliable delivery permits no loss: any gap report at all is a violation,
/// even for an otherwise complete stream.
#[test]
fn any_gap_in_a_reliable_run_is_invalid() {
    let context = unit_context();
    let mut records = complete_records(&context.plans);
    records.gaps.push(gap_for(
        "r0p0",
        "r0p1",
        2,
        DeliveryGapReason::VolatileDropped,
    ));
    let summary = oracle::summarize(
        &context.plans,
        &context.roster,
        &records,
        1_000,
        context.delivery_class,
        &ChurnPlan::default(),
    );
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .iter()
            .any(|reason| matches!(reason, oracle::InvalidReason::InvalidGapReports { .. })),
        "expected the invalid-gap reason, got {:?}",
        summary.reasons
    );
}

/// A disconnected recipient's uncovered tail is permitted, but a hole below
/// the covered prefix is silent loss even for a disconnected recipient.
#[test]
fn a_disconnected_recipient_may_lose_only_the_uncovered_tail() {
    let context = unit_context_with_class(DeliveryClass::Volatile);
    // Permitted: seq 2 is gap-covered, seq 3 is the loud tail after the
    // disconnect.
    let tail_permitted = |records: &RunRecords| {
        oracle::summarize(
            &context.plans,
            &context.roster,
            records,
            1_000,
            context.delivery_class,
            &ChurnPlan::default(),
        )
    };
    let mut records = complete_records(&context.plans);
    drop_receipt(&mut records, "r0p0", "r0p1", 2);
    drop_receipt(&mut records, "r0p0", "r0p1", 3);
    records.gaps.push(gap_for(
        "r0p0",
        "r0p1",
        2,
        DeliveryGapReason::VolatileDropped,
    ));
    records.disconnects.push(records::DisconnectEvent {
        recipient: "r0p0".to_string(),
        observation: records::DisconnectObservation::StreamEnded,
    });
    let permitted = tail_permitted(&records);
    // r0p1's own stream (from r0p0, r0p2, r0p3) is untouched and
    // connected-through; only r0p0's stream ends.
    let reader = permitted
        .per_recipient
        .iter()
        .find(|outcome| outcome.recipient == "r0p0")
        .expect("recipient on roster");
    assert_eq!(reader.undelivered_at_disconnect, 1);
    assert_eq!(reader.missing, 0);

    // Forbidden: a second, UNreported hole below the covered position.
    let mut holed = complete_records(&context.plans);
    drop_receipt(&mut holed, "r0p0", "r0p1", 1);
    drop_receipt(&mut holed, "r0p0", "r0p1", 3);
    holed.gaps.push(gap_for(
        "r0p0",
        "r0p1",
        2,
        DeliveryGapReason::VolatileDropped,
    ));
    holed.disconnects.push(records::DisconnectEvent {
        recipient: "r0p0".to_string(),
        observation: records::DisconnectObservation::StreamEnded,
    });
    let summary = tail_permitted(&holed);
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .contains(&oracle::InvalidReason::MissingDeliveries {
                count: 1,
                first: oracle::DeliveryKey {
                    recipient: "r0p0".to_string(),
                    sender: "r0p1".to_string(),
                    epoch: 1,
                    seq: 2,
                },
            }),
        "expected the hole at seq 1, got {:?}",
        summary.reasons
    );

    // Forbidden: an unreported hole at the HEAD of the stream, named exactly
    // at seq 0 (the head is below every covered value, so the span scan must
    // not miss it).
    let mut head = complete_records(&context.plans);
    drop_receipt(&mut head, "r0p0", "r0p1", 0);
    head.disconnects.push(records::DisconnectEvent {
        recipient: "r0p0".to_string(),
        observation: records::DisconnectObservation::StreamEnded,
    });
    let summary = tail_permitted(&head);
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .contains(&oracle::InvalidReason::MissingDeliveries {
                count: 1,
                first: oracle::DeliveryKey {
                    recipient: "r0p0".to_string(),
                    sender: "r0p1".to_string(),
                    epoch: 1,
                    seq: 1,
                },
            }),
        "expected the head hole at seq 0, got {:?}",
        summary.reasons
    );
}

/// A gap naming the recipient as its own sender is a violation in every
/// class: the server never reports a connection's own sends to itself.
#[test]
fn a_self_referential_gap_is_invalid() {
    let context = unit_context_with_class(DeliveryClass::Volatile);
    let mut records = complete_records(&context.plans);
    records.gaps.push(gap_for(
        "r0p1",
        "r0p1",
        2,
        DeliveryGapReason::VolatileDropped,
    ));
    let summary = oracle::summarize(
        &context.plans,
        &context.roster,
        &records,
        1_000,
        context.delivery_class,
        &ChurnPlan::default(),
    );
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .iter()
            .any(|reason| matches!(reason, oracle::InvalidReason::InvalidGapReports { .. })),
        "expected the invalid-gap reason, got {:?}",
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
    std::env::set_var("CAPACITY_RUNNER_ENCODING", "cbor");
    assert!(RunConfig::from_env().is_err());
    std::env::set_var("CAPACITY_RUNNER_ENCODING", "v3-json");
    std::env::set_var("CAPACITY_RUNNER_CLASS", "lossy");
    assert!(RunConfig::from_env().is_err());
    std::env::set_var("CAPACITY_RUNNER_CLASS", "reliable");
    std::env::set_var("CAPACITY_RUNNER_CHURN", "rejoin-storm");
    assert!(RunConfig::from_env().is_err());
    std::env::remove_var("CAPACITY_RUNNER_ENCODING");
    std::env::remove_var("CAPACITY_RUNNER_CLASS");
    std::env::remove_var("CAPACITY_RUNNER_CHURN");
    assert!(RunConfig::from_env().is_ok());

    // The lossy classes and their key knob parse.
    std::env::set_var("CAPACITY_RUNNER_CLASS", "latest");
    std::env::set_var("CAPACITY_RUNNER_LATEST_KEYS", "8");
    let config = RunConfig::from_env().expect("latest env config parses");
    assert_eq!(config.delivery_class, DeliveryClass::Latest);
    assert_eq!(config.latest_keys_per_sender, 8);
    std::env::set_var("CAPACITY_RUNNER_CLASS", "volatile");
    std::env::remove_var("CAPACITY_RUNNER_LATEST_KEYS");
    let config = RunConfig::from_env().expect("volatile env config parses");
    assert_eq!(config.delivery_class, DeliveryClass::Volatile);
    std::env::remove_var("CAPACITY_RUNNER_CLASS");
    assert!(RunConfig::from_env().is_ok());
}

/// Hook/class mismatches are refused before anything is spawned or written:
/// each fault hook belongs to exactly one delivery contract.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hook_and_class_mismatches_are_refused_before_any_spawn() {
    let probe = std::env::temp_dir().join("signal-fish-capacity-refusal-probe");
    let mut config = scenario_config(Encoding::V3Json);
    config.output_dir = probe.clone();
    let _ = std::fs::remove_dir_all(&probe);

    let mut mismatch = config.clone();
    mismatch.pause_reads = Some(Duration::from_millis(10));
    assert!(
        runner::run(mismatch).await.is_err(),
        "pause_reads is a latest/volatile hook"
    );

    let mut mismatch = config.clone();
    mismatch.delivery_class = DeliveryClass::Latest;
    mismatch.slow_reader = true;
    assert!(
        runner::run(mismatch).await.is_err(),
        "slow_reader is a reliable hook"
    );

    let mut mismatch = config.clone();
    mismatch.delivery_class = DeliveryClass::Latest;
    mismatch.latest_keys_per_sender = 0;
    assert!(
        runner::run(mismatch).await.is_err(),
        "latest needs at least one key"
    );

    let mut mismatch = config;
    mismatch.delivery_class = DeliveryClass::Reliable;
    mismatch.latest_keys_per_sender = 4;
    assert!(
        runner::run(mismatch).await.is_err(),
        "a reliable run coalesces nothing"
    );

    let mut mismatch = scenario_config(Encoding::V2Json);
    mismatch.output_dir = probe.clone();
    mismatch.delivery_class = DeliveryClass::Volatile;
    assert!(
        runner::run(mismatch).await.is_err(),
        "delivery classes require the v3 wire"
    );

    // Churn runs on the v3 wire alone, and never composes with the hooks
    // that own the designated peer's socket or the generator's timing.
    let churn = ChurnSchedule::ReconnectBurst {
        fraction_percent: 50,
        start: Duration::from_millis(300),
        window: Duration::from_millis(300),
    };
    let mut mismatch = scenario_config(Encoding::V2Json);
    mismatch.output_dir = probe.clone();
    mismatch.churn = churn;
    assert!(
        runner::run(mismatch).await.is_err(),
        "churn requires the v3 wire's per-epoch stamps"
    );

    let mut mismatch = scenario_config(Encoding::V2Json);
    mismatch.output_dir = probe.clone();
    mismatch.encoding = Encoding::V3Json;
    mismatch.churn = churn;
    mismatch.slow_reader = true;
    assert!(
        runner::run(mismatch).await.is_err(),
        "slow_reader and churn both own the designated socket"
    );

    let mut mismatch = scenario_config(Encoding::V2Json);
    mismatch.output_dir = probe.clone();
    mismatch.encoding = Encoding::V3Json;
    mismatch.churn = churn;
    mismatch.pause_reads = Some(Duration::from_millis(10));
    assert!(
        runner::run(mismatch).await.is_err(),
        "pause_reads and churn both own the designated socket"
    );

    let mut mismatch = scenario_config(Encoding::V2Json);
    mismatch.output_dir = probe.clone();
    mismatch.encoding = Encoding::V3Json;
    mismatch.churn = churn;
    mismatch.pause_sends = Some(config::SendPause {
        after_seq: 1,
        duration: Duration::from_millis(10),
    });
    assert!(
        runner::run(mismatch).await.is_err(),
        "generator-latency hooks do not compose with churn"
    );

    let mut mismatch = scenario_config(Encoding::V2Json);
    mismatch.output_dir = probe.clone();
    mismatch.encoding = Encoding::V3Json;
    mismatch.churn = churn;
    mismatch.kill_server_after = Some(Duration::from_millis(500));
    assert!(
        runner::run(mismatch).await.is_err(),
        "kill_server_after and churn are both run-level faults"
    );

    let mut mismatch = scenario_config(Encoding::V2Json);
    mismatch.output_dir = probe.clone();
    mismatch.encoding = Encoding::V3Json;
    mismatch.churn = ChurnSchedule::ReconnectBurst {
        fraction_percent: 50,
        start: Duration::from_millis(300),
        window: Duration::from_millis(400),
    };
    // The default lag bound is 250 ms: a 400 ms stagger window would let a
    // boundary race inflate a send's lag past it.
    assert!(
        runner::run(mismatch).await.is_err(),
        "the stagger window must stay below the generator-lag bound"
    );

    // A storm outside the scheduled-send span would strand its reconnects
    // past quiescence.
    let mut mismatch = scenario_config(Encoding::V2Json);
    mismatch.output_dir = probe.clone();
    mismatch.encoding = Encoding::V3Json;
    mismatch.churn = ChurnSchedule::ReconnectBurst {
        fraction_percent: 50,
        start: Duration::from_secs(2),
        window: Duration::from_secs(2),
    };
    assert!(
        runner::run(mismatch).await.is_err(),
        "the storm must complete inside the scheduled-send span"
    );

    assert!(
        !probe.exists(),
        "a refused config must not poison an output directory"
    );
}

/// Standalone entry point for a capacity host: configure the run entirely
/// through `CAPACITY_RUNNER_*` environment variables (see
/// `config::RunConfig::from_env`), e.g. against a release-profile server:
///
/// ```text
/// CAPACITY_RUNNER_OUTPUT_DIR=/tmp/cap-run \
/// cargo test --release --test capacity_runner \
///   a_standalone_env_configured_run_writes_artifacts_and_replays -- --ignored
/// ```
///
/// In CI the default configuration runs the same small scenario as the
/// acceptance gate; on a capacity host the variables shape the real cells.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "standalone capacity-host entry point: shape via CAPACITY_RUNNER_*"]
async fn a_standalone_env_configured_run_writes_artifacts_and_replays() {
    let config = RunConfig::from_env().expect("CAPACITY_RUNNER_* env is valid");
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
// Churn schedule controls (deterministic, no server): the offline-window
// shift is the mechanism that keeps a scheduled gap out of the generator's
// lag accounting, so it is pinned exactly.
// ---------------------------------------------------------------------------

/// Every victim's post-disconnect sends move past its reconnect instant with
/// count and spacing preserved; non-victims keep their exact timeline; and
/// the whole storm lands inside the scheduled-send span.
#[test]
fn the_churn_shift_moves_victim_sends_past_their_reconnect_without_changing_the_workload() {
    let mut config = scenario_config(Encoding::V3Json);
    config.rooms = 2;
    config.warmup = Duration::from_millis(200);
    config.duration = Duration::from_millis(1_000);
    config.churn = ChurnSchedule::ReconnectBurst {
        fraction_percent: 50,
        start: Duration::from_millis(400),
        window: Duration::from_millis(200),
    };

    // The unshifted timeline is the reference workload shape.
    let mut plain = config.clone();
    plain.churn = ChurnSchedule::None;
    let (reference, _) = build_run_shape(&plain).expect("reference shape");

    let (plans, churn) = build_run_shape(&config).expect("churn shape");
    assert_eq!(churn.cycles.len(), 1);
    let cycle = &churn.cycles[0];
    assert_eq!(cycle.peers.len(), 4, "half of the eight peers");

    let ChurnSchedule::ReconnectBurst { start, window, .. } = config.churn else {
        panic!("the burst shape is set");
    };
    let storm_end_us = crate::config::micros(start + window);
    let span_us = reference
        .iter()
        .map(|plan| plan.sends.last().map(|send| send.intended_us).unwrap_or(0))
        .max()
        .unwrap_or(0);
    assert!(
        storm_end_us <= span_us,
        "the storm must complete inside the scheduled-send span"
    );

    for plan in &plans {
        let reference = reference
            .iter()
            .find(|other| other.name == plan.name)
            .expect("same roster");
        assert_eq!(
            plan.sends.len(),
            reference.sends.len(),
            "{}: the shift preserves the offered workload",
            plan.name
        );
        match cycle.reconnects_us.get(&plan.name) {
            Some(&reconnect_us) => {
                let mut previous = None;
                for (send, reference) in plan.sends.iter().zip(&reference.sends) {
                    assert_eq!(send.seq, reference.seq);
                    if reference.intended_us >= cycle.disconnect_us {
                        assert!(
                            send.intended_us >= reconnect_us,
                            "{} send {}: a shifted send must fire at or after its                              reconnect instant",
                            plan.name,
                            send.seq
                        );
                        // Spacing is preserved: every shifted send moved by
                        // the same offline duration.
                        let shift = send.intended_us - reference.intended_us;
                        assert_eq!(
                            shift,
                            reconnect_us - cycle.disconnect_us,
                            "{} send {}: the offline window shifts every late send                              by the same amount",
                            plan.name,
                            send.seq
                        );
                    } else {
                        assert_eq!(
                            send.intended_us, reference.intended_us,
                            "{} send {}: early sends keep their timeline",
                            plan.name, send.seq
                        );
                    }
                    if let Some(previous) = previous {
                        assert!(
                            send.intended_us > previous,
                            "{}: the shifted schedule never folds sends together",
                            plan.name
                        );
                    }
                    previous = Some(send.intended_us);
                }
            }
            None => {
                assert_eq!(
                    plan.sends, reference.sends,
                    "{}: a non-victim keeps its exact timeline",
                    plan.name
                );
            }
        }
    }
}

/// A storm that would outlive the scheduled-send span is refused before
/// anything is spawned: its reconnects could land past quiescence.
#[test]
fn a_churn_storm_beyond_the_scheduled_span_is_refused() {
    let mut config = scenario_config(Encoding::V3Json);
    config.churn = ChurnSchedule::ReconnectBurst {
        fraction_percent: 50,
        start: Duration::from_secs(5),
        window: Duration::from_secs(5),
    };
    assert!(
        build_run_shape(&config).is_err(),
        "the storm must complete inside the scheduled-send span"
    );
    // Absurd env scalars are refused, not panics.
    let mut config = scenario_config(Encoding::V3Json);
    config.churn = ChurnSchedule::ReconnectBurst {
        fraction_percent: 50,
        start: Duration::from_secs(u64::MAX / 2_000_000),
        window: Duration::from_secs(u64::MAX / 2_000_000),
    };
    assert!(
        build_run_shape(&config).is_err(),
        "start + window overflow must be a refusal"
    );
}

// ---------------------------------------------------------------------------
// Churn oracle controls (deterministic, no server): the reconnect contract
// — new epochs are distinct streams, the away window is loud in snapshot
// tails, and a storm that never fires is not a churn measurement.
// ---------------------------------------------------------------------------

/// A unit context whose sender `r0p1` churns mid-run: sends ledger 0..=1 in
/// epoch 1, disconnects at 300 µs, rejoins at 400 µs (epoch 2), and sends
/// ledger 2..=3 in epoch 2. The recipient `r0p0` stays seated; `r0p2` and
/// `r0p3` are passive roster members.
fn churn_context(delivery_class: DeliveryClass) -> (UnitContext, ChurnPlan) {
    let mut config = scenario_config(Encoding::V3Json);
    config.players_per_room = 4;
    config.warmup = Duration::ZERO;
    config.duration = Duration::from_millis(400);
    config.send_rate_per_sender = 10.0;
    config.delivery_class = delivery_class;
    config.churn = ChurnSchedule::ReconnectBurst {
        fraction_percent: 25,
        start: Duration::from_millis(250),
        window: Duration::from_millis(100),
    };
    let (mut plans, churn) = build_run_shape(&config).expect("churn shape");
    // Keep only the churning sender's schedule for send events; the other
    // plans stay in the roster for room membership.
    plans.retain(|plan| plan.name == "r0p1");
    let roster = vec![
        ("r0p0".to_string(), 0),
        ("r0p1".to_string(), 0),
        ("r0p2".to_string(), 0),
        ("r0p3".to_string(), 0),
    ];
    (
        UnitContext {
            plans,
            roster,
            delivery_class,
        },
        churn,
    )
}

/// The complete valid event set for [`churn_context`]: the sender's
/// epoch-1 stream (ledger 0..=1 -> server 1..=2), its epoch-2 stream
/// (ledger 2..=3 -> server 1..=2), the recipient's receipts, and the
/// sender's disconnect/rejoin churn events.
fn churned_records(context: &UnitContext, churn: &ChurnPlan) -> RunRecords {
    let log = EventLog::new();
    let plan = &context.plans[0];
    let disconnect_us = churn.cycles[0].disconnect_us;
    let reconnect_us = churn.cycles[0].reconnects_us["r0p1"];
    log.push_churn(records::ChurnEvent {
        recipient: "r0p1".to_string(),
        phase: records::ChurnPhase::Disconnect,
        at_us: disconnect_us,
        epoch: None,
        tails: BTreeMap::new(),
    });
    log.push_churn(records::ChurnEvent {
        recipient: "r0p1".to_string(),
        phase: records::ChurnPhase::Rejoined,
        at_us: reconnect_us,
        epoch: Some(2),
        tails: BTreeMap::from([
            ("r0p0".to_string(), (1, 0)),
            ("r0p1".to_string(), (2, 0)),
            ("r0p2".to_string(), (1, 0)),
            ("r0p3".to_string(), (1, 0)),
        ]),
    });
    let mut epoch_positions: BTreeMap<u32, u64> = BTreeMap::new();
    for send in &plan.sends {
        let epoch = u32::from(send.intended_us >= reconnect_us) + 1;
        let epoch_ledger = *epoch_positions.get(&epoch).unwrap_or(&0);
        *epoch_positions.entry(epoch).or_insert(0) += 1;
        log.push_sent(SentEvent {
            sender: plan.name.clone(),
            room: plan.room,
            seq: send.seq,
            epoch,
            intended_us: send.intended_us,
            sent_us: send.intended_us + 5,
            phase: send.phase,
        });
        // Every co-room peer receives every send exactly once, one after the
        // next, stamped with the sender's per-epoch stream coordinates.
        for recipient in ["r0p0", "r0p2", "r0p3"] {
            log.push_receipt(ReceiptEvent {
                recipient: recipient.to_string(),
                sender: plan.name.clone(),
                seq: send.seq,
                epoch,
                server_seq: epoch_ledger + 1,
                received_us: send.intended_us + 8,
            });
        }
    }
    log.snapshot()
}

/// Across a sender's rejoin, every stream must complete exactly once: the
/// epoch-1 prefix and the epoch-2 resumption are distinct, complete streams.
#[test]
fn a_reconnect_run_completes_every_stream_exactly_once_across_epochs() {
    let (context, churn) = churn_context(DeliveryClass::Reliable);
    let records = churned_records(&context, &churn);
    let summary = oracle::summarize(
        &context.plans,
        &context.roster,
        &records,
        1_000,
        context.delivery_class,
        &churn,
    );
    assert!(
        summary.valid,
        "a clean reconnect run is contract-legal: {:?}",
        summary.reasons
    );
    // Both epochs of the churning sender arrived at the seated recipient.
    let reader = summary
        .per_recipient
        .iter()
        .find(|outcome| outcome.recipient == "r0p0")
        .expect("recipient on roster");
    assert_eq!(reader.received.get("r0p1"), Some(&4));
    assert_eq!(reader.missing, 0);
    assert_eq!(reader.duplicates, 0);
    assert_eq!(reader.misrouted, 0);
}

/// A delivery for a stream its sender already rejoined past (a stale epoch
/// arriving after the rejoin) is a misroute.
#[test]
fn a_stale_epoch_delivery_after_the_senders_rejoin_is_a_misroute() {
    let (context, churn) = churn_context(DeliveryClass::Reliable);
    let mut records = churned_records(&context, &churn);
    let reconnect_us = churn.cycles[0].reconnects_us["r0p1"];
    records.receipts.push(ReceiptEvent {
        recipient: "r0p0".to_string(),
        sender: "r0p1".to_string(),
        seq: 0,
        epoch: 1,
        server_seq: 1,
        received_us: reconnect_us + 10,
    });
    let summary = oracle::summarize(
        &context.plans,
        &context.roster,
        &records,
        1_000,
        context.delivery_class,
        &churn,
    );
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .iter()
            .any(|reason| matches!(reason, oracle::InvalidReason::MisroutedDeliveries { .. })),
        "the stale-epoch redelivery must be a misroute, got {:?}",
        summary.reasons
    );
}

/// A recipient that rejoins owes nothing at or below its snapshot tail —
/// the away window is permitted and loud — but everything above the tail is
/// still owed exactly once.
#[test]
fn a_rejoining_recipient_owes_nothing_below_its_tail_and_all_above_it() {
    let (context, churn) = churn_context(DeliveryClass::Volatile);
    let mut records = churned_records(&context, &churn);
    // The recipient r0p0 disconnects at 250 µs, missing the rest of the
    // sender's epoch-1 stream (its snapshot tells it that stream is already
    // at tail 2), rejoins at 260 µs, and receives only the fresh epoch-2
    // stream.
    records
        .receipts
        .retain(|receipt| receipt.recipient != "r0p0" || receipt.epoch == 2);
    records.churn.push(records::ChurnEvent {
        recipient: "r0p0".to_string(),
        phase: records::ChurnPhase::Disconnect,
        at_us: 250_000,
        epoch: None,
        tails: BTreeMap::new(),
    });
    records.churn.push(records::ChurnEvent {
        recipient: "r0p0".to_string(),
        phase: records::ChurnPhase::Rejoined,
        at_us: 260_000,
        epoch: Some(2),
        tails: BTreeMap::from([
            ("r0p1".to_string(), (1, 2)),
            ("r0p0".to_string(), (2, 0)),
            ("r0p2".to_string(), (1, 0)),
            ("r0p3".to_string(), (1, 0)),
        ]),
    });
    let summary = oracle::summarize(
        &context.plans,
        &context.roster,
        &records,
        1_000,
        context.delivery_class,
        &churn,
    );
    assert!(
        summary.valid,
        "the away window (the whole epoch-1 stream) is accounted by the snapshot \
         tail, and the fresh epoch-2 stream delivered: {:?}",
        summary.reasons
    );
    let reader = summary
        .per_recipient
        .iter()
        .find(|outcome| outcome.recipient == "r0p0")
        .expect("recipient on roster");
    assert_eq!(reader.received.get("r0p1"), Some(&2));
    assert_eq!(reader.missing, 0);

    // Forbidden: an unreported hole in the fresh epoch-2 stream is silent
    // loss — the tail covers only what the snapshot accounted.
    let mut holed = churned_records(&context, &churn);
    holed.receipts.retain(|receipt| {
        receipt.recipient != "r0p0" || receipt.epoch == 2 && receipt.server_seq >= 2
    });
    holed.churn.push(records::ChurnEvent {
        recipient: "r0p0".to_string(),
        phase: records::ChurnPhase::Disconnect,
        at_us: 250_000,
        epoch: None,
        tails: BTreeMap::new(),
    });
    holed.churn.push(records::ChurnEvent {
        recipient: "r0p0".to_string(),
        phase: records::ChurnPhase::Rejoined,
        at_us: 260_000,
        epoch: Some(2),
        tails: BTreeMap::from([
            ("r0p1".to_string(), (1, 2)),
            ("r0p0".to_string(), (2, 0)),
            ("r0p2".to_string(), (1, 0)),
            ("r0p3".to_string(), (1, 0)),
        ]),
    });
    let summary = oracle::summarize(
        &context.plans,
        &context.roster,
        &holed,
        1_000,
        context.delivery_class,
        &churn,
    );
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .contains(&oracle::InvalidReason::MissingDeliveries {
                count: 1,
                first: oracle::DeliveryKey {
                    recipient: "r0p0".to_string(),
                    sender: "r0p1".to_string(),
                    epoch: 2,
                    seq: 1,
                },
            }),
        "the hole above the tail must be named at the first owed sequence, got {:?}",
        summary.reasons
    );
}

/// A delivery at or below the rejoin snapshot's tail must never arrive at
/// the new seat: replaying accounted sequences is a misroute.
#[test]
fn a_below_tail_delivery_after_the_recipients_rejoin_is_a_misroute() {
    let (context, churn) = churn_context(DeliveryClass::Volatile);
    let mut records = churned_records(&context, &churn);
    records.churn.push(records::ChurnEvent {
        recipient: "r0p0".to_string(),
        phase: records::ChurnPhase::Disconnect,
        at_us: 200,
        epoch: None,
        tails: BTreeMap::new(),
    });
    records.churn.push(records::ChurnEvent {
        recipient: "r0p0".to_string(),
        phase: records::ChurnPhase::Rejoined,
        at_us: 210,
        epoch: Some(2),
        tails: BTreeMap::from([
            ("r0p1".to_string(), (1, 2)),
            ("r0p0".to_string(), (2, 0)),
            ("r0p2".to_string(), (1, 0)),
            ("r0p3".to_string(), (1, 0)),
        ]),
    });
    // The server replays an already-accounted sequence to the new seat.
    records.receipts.push(ReceiptEvent {
        recipient: "r0p0".to_string(),
        sender: "r0p1".to_string(),
        seq: 1,
        epoch: 1,
        server_seq: 2,
        received_us: 300,
    });
    let summary = oracle::summarize(
        &context.plans,
        &context.roster,
        &records,
        1_000,
        context.delivery_class,
        &churn,
    );
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .iter()
            .any(|reason| matches!(reason, oracle::InvalidReason::MisroutedDeliveries { .. })),
        "the replayed below-tail delivery must be a misroute, got {:?}",
        summary.reasons
    );
}

/// A churn run whose storm never fires is not the churn measurement its
/// manifest claims: every planned victim must have acted.
#[test]
fn a_churn_run_whose_storm_never_fires_is_invalid() {
    let (context, churn) = churn_context(DeliveryClass::Reliable);
    let records = churned_records(&context, &churn);
    let mut records = records;
    records.churn.clear();
    let summary = oracle::summarize(
        &context.plans,
        &context.roster,
        &records,
        1_000,
        context.delivery_class,
        &churn,
    );
    assert!(!summary.valid);
    assert!(
        summary
            .reasons
            .contains(&oracle::InvalidReason::ChurnNotPerformed {
                peer: "r0p1".to_string(),
            }),
        "the unperformed storm must be named, got {:?}",
        summary.reasons
    );
}

// ---------------------------------------------------------------------------
// Reconnect-burst cell over real sockets (issue #648, third runner PR): the
// C3 reconnect storm with reliable delivery.
// ---------------------------------------------------------------------------

/// Half of a four-peer room disconnects mid-run and rejoins under fresh
/// incarnation epochs while the others keep sending. Every stream must
/// complete exactly once across the storm: the rejoining senders' new
/// epochs are fresh streams the seated peers owe in full, and the
/// rejoining peers' away windows are accounted by their rejoin snapshot
/// tails. The artifacts must replay to the same summary.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reconnect_burst_delivers_every_stream_exactly_once_across_the_storm() {
    let output = tempfile::tempdir().expect("create output tempdir");
    let mut config = scenario_config(Encoding::V3Json);
    config.output_dir = output.path().to_path_buf();
    config.warmup = Duration::from_millis(200);
    config.duration = Duration::from_millis(1_200);
    config.churn = ChurnSchedule::ReconnectBurst {
        fraction_percent: 50,
        start: Duration::from_millis(400),
        window: Duration::from_millis(300),
    };
    config.generator_lag_bound = Duration::from_millis(500);
    config.drain_grace = Duration::from_secs(2);

    let outcome = runner::run(config).await.expect("storm run completes");
    assert!(
        outcome.summary.valid,
        "the reconnect storm must deliver exactly once across epochs: {:?}",
        outcome.summary.reasons
    );
    assert_eq!(
        outcome.summary.totals.unsent, 0,
        "the shifted schedule must still fire every send"
    );
    assert_eq!(outcome.summary.totals.outstanding, 0);
    for recipient in &outcome.summary.per_recipient {
        assert!(
            recipient.connected_through,
            "{}: every peer is seated again after the storm",
            recipient.recipient
        );
        assert_eq!(recipient.missing, 0);
        assert_eq!(recipient.duplicates, 0);
        assert_eq!(recipient.misrouted, 0);
    }

    // The storm actually happened: two victims disconnected and rejoined
    // under a bumped incarnation epoch.
    let records = artifacts::read_records(output.path()).expect("read the run's event log");
    let rejoined: Vec<_> = records
        .churn
        .iter()
        .filter(|event| event.phase == records::ChurnPhase::Rejoined)
        .collect();
    assert_eq!(
        rejoined.len(),
        2,
        "half of the four peers rejoined: {:?}",
        records.churn
    );
    assert!(
        rejoined
            .iter()
            .all(|event| event.epoch.is_some_and(|epoch| epoch >= 2)),
        "a rejoin bumps the incarnation epoch: {rejoined:?}"
    );
    // At least one rejoining peer's new epoch produced deliveries the
    // others observed, and every rejoin snapshot named the member tails.
    assert!(rejoined.iter().all(|event| !event.tails.is_empty()));

    let replayed = artifacts::replay(output.path()).expect("replay artifacts");
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
        pause_reads: None,
        latest_keys_per_sender: 1,
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
    delivery_class: DeliveryClass,
}

fn unit_context() -> UnitContext {
    unit_context_with_class(DeliveryClass::Reliable)
}

fn unit_context_with_class(delivery_class: DeliveryClass) -> UnitContext {
    let mut config = scenario_config(Encoding::V2Json);
    config.warmup = Duration::ZERO;
    config.duration = Duration::from_millis(400);
    config.send_rate_per_sender = 10.0;
    config.delivery_class = delivery_class;
    let (plans, _churn) = build_run_shape(&config).expect("unit shape");
    let roster = plans
        .iter()
        .map(|plan| (plan.name.clone(), plan.room))
        .collect();
    UnitContext {
        plans,
        roster,
        delivery_class,
    }
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
                epoch: 1,
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
                    epoch: 1,
                    server_seq: send.seq + 1,
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
