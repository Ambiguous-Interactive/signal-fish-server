//! Contract tests for the strict JSONL -> TLA+ trace compiler.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const VALID_TRACE: &str = concat!(
    "{\"kind\":\"header\",\"schema\":\"signal-fish.delivery-contract/v1\",",
    "\"trace_id\":\"case-1\",\"queue_kind\":\"v2_legacy_reliable_fifo\",",
    "\"queue_capacity\":2}\n",
    "{\"kind\":\"event\",\"schema\":\"signal-fish.delivery-contract/v1\",",
    "\"trace_id\":\"case-1\",\"seq\":1,\"action\":\"SendFast\",\"delivery_id\":1}\n",
    "{\"kind\":\"event\",\"schema\":\"signal-fish.delivery-contract/v1\",",
    "\"trace_id\":\"case-1\",\"seq\":2,\"action\":\"WriterStart\",\"delivery_id\":1}\n",
    "{\"kind\":\"event\",\"schema\":\"signal-fish.delivery-contract/v1\",",
    "\"trace_id\":\"case-1\",\"seq\":3,\"action\":\"WriterDrain\",\"delivery_id\":1}\n",
    "{\"kind\":\"footer\",\"schema\":\"signal-fish.delivery-contract/v1\",",
    "\"trace_id\":\"case-1\",\"event_count\":3}\n",
);

const PRODUCTION_TRACE: &str = concat!(
    "{\"kind\":\"header\",\"schema\":\"signal-fish.delivery-contract/v1\",",
    "\"trace_id\":\"socket-test-1\",\"queue_kind\":\"v2_legacy_reliable_fifo\",",
    "\"queue_capacity\":2}\n",
    "{\"kind\":\"event\",\"schema\":\"signal-fish.delivery-contract/v1\",",
    "\"trace_id\":\"socket-test-1\",\"seq\":1,\"action\":\"SendFast\",\"delivery_id\":1}\n",
    "{\"kind\":\"event\",\"schema\":\"signal-fish.delivery-contract/v1\",",
    "\"trace_id\":\"socket-test-1\",\"seq\":2,\"action\":\"WriterStart\",\"delivery_id\":1}\n",
    "{\"kind\":\"event\",\"schema\":\"signal-fish.delivery-contract/v1\",",
    "\"trace_id\":\"socket-test-1\",\"seq\":3,\"action\":\"WriterDrain\",\"delivery_id\":1}\n",
    "{\"kind\":\"event\",\"schema\":\"signal-fish.delivery-contract/v1\",",
    "\"trace_id\":\"socket-test-1\",\"seq\":4,\"action\":\"LifecycleClose\"}\n",
    "{\"kind\":\"event\",\"schema\":\"signal-fish.delivery-contract/v1\",",
    "\"trace_id\":\"socket-test-1\",\"seq\":5,\"action\":\"QueueClose\"}\n",
    "{\"kind\":\"event\",\"schema\":\"signal-fish.delivery-contract/v1\",",
    "\"trace_id\":\"socket-test-1\",\"seq\":6,\"action\":\"CloseFinish\"}\n",
    "{\"kind\":\"footer\",\"schema\":\"signal-fish.delivery-contract/v1\",",
    "\"trace_id\":\"socket-test-1\",\"event_count\":6}\n",
);

fn run_generator(
    input: &Path,
    output_dir: &Path,
    seeded_bug: bool,
    require_production_socket: bool,
) -> Output {
    let mut command = Command::new("python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/generate-delivery-contract-trace.py"
        ))
        .arg("--input")
        .arg(input)
        .arg("--output-dir")
        .arg(output_dir);
    if seeded_bug {
        command.arg("--seeded-bug");
    }
    if require_production_socket {
        command.arg("--require-production-socket");
    }
    command.output().expect("run Python trace generator")
}

#[test]
fn valid_trace_generates_self_contained_positive_and_negative_bundles() {
    for (seeded_bug, expected_constant) in [
        (false, "CONSTANT TraceActionBug = FALSE"),
        (true, "CONSTANT TraceActionBug = TRUE"),
    ] {
        let temp = tempfile::tempdir().expect("temporary directory");
        let input = temp.path().join("trace.jsonl");
        let output_dir = temp.path().join("bundle");
        fs::write(&input, VALID_TRACE).expect("write input trace");

        let output = run_generator(&input, &output_dir, seeded_bug, false);
        assert!(
            output.status.success(),
            "generator failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let generated = fs::read_to_string(output_dir.join("GeneratedDeliveryContractTrace.tla"))
            .expect("read generated TLA input");
        assert!(generated.contains("TraceIds == {\"case-1\"}"));
        assert!(generated.contains("[action |-> \"SendFast\", sender |-> \"d1\"]"));
        assert!(output_dir.join("DeliveryContractTrace.tla").is_file());
        let config = fs::read_to_string(output_dir.join("DeliveryContractTrace_Generated.cfg"))
            .expect("read generated config");
        assert!(config.contains(expected_constant));
    }
}

#[test]
fn production_socket_requirement_is_explicit_and_non_vacuous() {
    for (description, trace, expected_success) in [
        ("model-only corpus", VALID_TRACE, false),
        ("production socket corpus", PRODUCTION_TRACE, true),
    ] {
        let temp = tempfile::tempdir().expect("temporary directory");
        let input = temp.path().join("trace.jsonl");
        let output_dir = temp.path().join("bundle");
        fs::write(&input, trace).expect("write input trace");
        let output = run_generator(&input, &output_dir, false, true);
        assert_eq!(
            output.status.success(),
            expected_success,
            "{description}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn malformed_or_out_of_domain_traces_fail_closed() {
    let cases = [
        (
            "unknown action",
            VALID_TRACE.replace("WriterDrain", "InventedAction"),
            "unknown action",
        ),
        (
            "unsupported production branch",
            VALID_TRACE.replace("WriterDrain", "Unsupported"),
            "outside v2_legacy_reliable_fifo",
        ),
        (
            "mismatched write phase",
            VALID_TRACE.replace("WriterDrain", "CloseFlushDrain"),
            "started by CloseFlushStart",
        ),
        (
            "sequence gap",
            VALID_TRACE.replacen("\"seq\":2", "\"seq\":9", 1),
            "seq must be contiguous",
        ),
        (
            "missing delivery id",
            VALID_TRACE.replace(",\"delivery_id\":1", ""),
            "requires a positive delivery_id",
        ),
        (
            "wrong footer count",
            VALID_TRACE.replace("\"event_count\":3", "\"event_count\":2"),
            "event_count must equal 3",
        ),
        (
            "unsafe trace id",
            VALID_TRACE.replace("case-1", "case 1"),
            "trace_id must match",
        ),
        (
            "non-string trace id",
            VALID_TRACE.replace("\"case-1\"", "[]"),
            "trace_id must match",
        ),
        (
            "control character trace id",
            VALID_TRACE.replace("case-1", "case\\n1"),
            "trace_id must match",
        ),
        (
            "boolean sequence",
            VALID_TRACE.replacen("\"seq\":1", "\"seq\":true", 1),
            "seq must be contiguous",
        ),
        (
            "non-string action",
            VALID_TRACE.replacen("\"action\":\"SendFast\"", "\"action\":[]", 1),
            "action must be a string",
        ),
        (
            "non-string detail",
            VALID_TRACE.replacen(
                "\"action\":\"SendFast\"",
                "\"action\":\"SendFast\",\"detail\":[]",
                1,
            ),
            "detail must be a string",
        ),
        (
            "boolean footer count",
            VALID_TRACE.replace("\"event_count\":3", "\"event_count\":true"),
            "event_count must equal 3",
        ),
    ];

    for (description, input_text, expected_error) in cases {
        let temp = tempfile::tempdir().expect("temporary directory");
        let input = temp.path().join("trace.jsonl");
        let output_dir = temp.path().join("bundle");
        fs::write(&input, input_text).expect("write malformed trace");

        let output = run_generator(&input, &output_dir, false, false);
        assert!(
            !output.status.success(),
            "{description}: malformed trace unexpectedly succeeded"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(expected_error),
            "{description}: expected {expected_error:?}, got {stderr:?}"
        );
    }
}

const SEQUENCED_TRACE: &str = include_str!("../formal/traces/sequenced-relay-replay.jsonl");

const RECONNECT_MEMBERSHIP_TRACE: &str = concat!(
    "{\"kind\":\"header\",\"schema\":\"signal-fish.sequenced-relay/v1\",",
    "\"trace_id\":\"membership-1\",\"protocol_version\":3}\n",
    "{\"kind\":\"event\",\"schema\":\"signal-fish.sequenced-relay/v1\",",
    "\"trace_id\":\"membership-1\",\"seq\":1,\"action\":\"ReceiverSnapshot\",",
    "\"receiver\":\"r1\",\"sender_count\":2}\n",
    "{\"kind\":\"event\",\"schema\":\"signal-fish.sequenced-relay/v1\",",
    "\"trace_id\":\"membership-1\",\"seq\":2,\"action\":\"ReceiverBaseline\",",
    "\"receiver\":\"r1\",\"sender\":\"s1\",\"epoch\":1,\"baseline_seq\":0}\n",
    "{\"kind\":\"event\",\"schema\":\"signal-fish.sequenced-relay/v1\",",
    "\"trace_id\":\"membership-1\",\"seq\":3,\"action\":\"ReceiverBaseline\",",
    "\"receiver\":\"r1\",\"sender\":\"s2\",\"epoch\":1,\"baseline_seq\":0}\n",
    "{\"kind\":\"event\",\"schema\":\"signal-fish.sequenced-relay/v1\",",
    "\"trace_id\":\"membership-1\",\"seq\":4,\"action\":\"PlayerLeft\",",
    "\"receiver\":\"r1\",\"sender\":\"s2\",\"epoch\":1,\"final_seq\":0}\n",
    "{\"kind\":\"event\",\"schema\":\"signal-fish.sequenced-relay/v1\",",
    "\"trace_id\":\"membership-1\",\"seq\":5,\"action\":\"ReceiverReconnect\",",
    "\"receiver\":\"r1\",\"sender_count\":1}\n",
    "{\"kind\":\"event\",\"schema\":\"signal-fish.sequenced-relay/v1\",",
    "\"trace_id\":\"membership-1\",\"seq\":6,\"action\":\"ReceiverBaseline\",",
    "\"receiver\":\"r1\",\"sender\":\"s1\",\"epoch\":1,\"baseline_seq\":0}\n",
    "{\"kind\":\"event\",\"schema\":\"signal-fish.sequenced-relay/v1\",",
    "\"trace_id\":\"membership-1\",\"seq\":7,\"action\":\"Data\",",
    "\"receiver\":\"r1\",\"sender\":\"s1\",\"epoch\":1,\"data_seq\":1}\n",
    "{\"kind\":\"footer\",\"schema\":\"signal-fish.sequenced-relay/v1\",",
    "\"trace_id\":\"membership-1\",\"event_count\":7}\n",
);

fn run_sequenced_generator(input: &Path, output_dir: &Path, seeded_bug: Option<&str>) -> Output {
    let mut command = Command::new("python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/generate-sequenced-relay-trace.py"
        ))
        .arg("--input")
        .arg(input)
        .arg("--output-dir")
        .arg(output_dir);
    if let Some(bug) = seeded_bug {
        command.arg("--seeded-bug").arg(bug);
    }
    command.output().expect("run sequenced-relay generator")
}

#[test]
fn sequenced_relay_trace_generates_a_self_contained_replay_bundle() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("trace.jsonl");
    let output_dir = temp.path().join("bundle");
    fs::write(&input, SEQUENCED_TRACE).expect("write input trace");

    let output = run_sequenced_generator(&input, &output_dir, None);
    assert!(
        output.status.success(),
        "generator failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let generated = fs::read_to_string(output_dir.join("GeneratedSequencedRelayTrace.tla"))
        .expect("read generated TLA input");
    assert!(generated.contains("TraceIds == {\"relay-1\"}"));
    assert!(generated.contains("action |-> \"DeliveryGap\""));
    assert!(generated.contains("receiver |-> \"r1\", sender |-> \"s1\""));
    assert!(output_dir.join("SequencedRelayTrace.tla").is_file());
    assert!(output_dir
        .join("SequencedRelayTrace_Generated.cfg")
        .is_file());
}

#[test]
fn sequenced_relay_seed_modes_emit_independent_negative_bundles() {
    for bug in [
        "duplicate-data",
        "silent-gap",
        "backward-epoch",
        "late-lifecycle",
    ] {
        let temp = tempfile::tempdir().expect("temporary directory");
        let input = temp.path().join("trace.jsonl");
        let output_dir = temp.path().join("bundle");
        fs::write(&input, SEQUENCED_TRACE).expect("write input trace");
        let output = run_sequenced_generator(&input, &output_dir, Some(bug));
        assert!(
            output.status.success(),
            "{bug}: generator failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(bug),
            "{bug}: generator did not identify the seeded mode"
        );
        if bug == "duplicate-data" {
            let generated = fs::read_to_string(output_dir.join("GeneratedSequencedRelayTrace.tla"))
                .expect("read duplicate-data TLA input");
            let repeated_positive_data = concat!(
                "[action |-> \"Data\", receiver |-> \"r1\", sender |-> \"s1\", ",
                "epoch |-> 1, value1 |-> 1,"
            );
            assert_eq!(
                generated.matches(repeated_positive_data).count(),
                2,
                "duplicate-data must repeat a prior positive Data sequence"
            );
        }
    }
}

#[test]
fn reconnect_baselines_authoritatively_replace_offline_membership() {
    for (description, trace) in [
        (
            "departed sender omitted",
            RECONNECT_MEMBERSHIP_TRACE.to_string(),
        ),
        (
            "offline membership replaced",
            RECONNECT_MEMBERSHIP_TRACE.replacen(
                "\"seq\":6,\"action\":\"ReceiverBaseline\",\"receiver\":\"r1\",\"sender\":\"s1\"",
                "\"seq\":6,\"action\":\"ReceiverBaseline\",\"receiver\":\"r1\",\"sender\":\"s2\"",
                1,
            ),
        ),
    ] {
        let temp = tempfile::tempdir().expect("temporary directory");
        let input = temp.path().join("trace.jsonl");
        let output_dir = temp.path().join("bundle");
        fs::write(&input, trace).expect("write reconnect trace");
        let output = run_sequenced_generator(&input, &output_dir, None);
        assert!(
            output.status.success(),
            "{description}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn counted_empty_receiver_snapshot_is_representable() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("trace.jsonl");
    let output_dir = temp.path().join("bundle");
    fs::write(
        &input,
        concat!(
            "{\"kind\":\"header\",\"schema\":\"signal-fish.sequenced-relay/v1\",",
            "\"trace_id\":\"empty-1\",\"protocol_version\":3}\n",
            "{\"kind\":\"event\",\"schema\":\"signal-fish.sequenced-relay/v1\",",
            "\"trace_id\":\"empty-1\",\"seq\":1,\"action\":\"ReceiverSnapshot\",",
            "\"receiver\":\"r1\",\"sender_count\":0}\n",
            "{\"kind\":\"footer\",\"schema\":\"signal-fish.sequenced-relay/v1\",",
            "\"trace_id\":\"empty-1\",\"event_count\":1}\n",
        ),
    )
    .expect("write empty snapshot trace");

    let output = run_sequenced_generator(&input, &output_dir, None);
    assert!(
        output.status.success(),
        "empty snapshot failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn aggregate_formal_domain_complexity_fails_closed() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("trace.jsonl");
    let output_dir = temp.path().join("bundle");
    let mut trace = concat!(
        "{\"kind\":\"header\",\"schema\":\"signal-fish.sequenced-relay/v1\",",
        "\"trace_id\":\"dense-1\",\"protocol_version\":3}\n",
        "{\"kind\":\"event\",\"schema\":\"signal-fish.sequenced-relay/v1\",",
        "\"trace_id\":\"dense-1\",\"seq\":1,\"action\":\"ReceiverSnapshot\",",
        "\"receiver\":\"r1\",\"sender_count\":17}\n",
    )
    .to_string();
    for sender in 1..=17 {
        trace.push_str(&format!(
            "{{\"kind\":\"event\",\"schema\":\"signal-fish.sequenced-relay/v1\",\"trace_id\":\"dense-1\",\"seq\":{},\"action\":\"ReceiverBaseline\",\"receiver\":\"r1\",\"sender\":\"s{sender}\",\"epoch\":4096,\"baseline_seq\":0}}\n",
            sender + 1
        ));
    }
    trace.push_str(concat!(
        "{\"kind\":\"footer\",\"schema\":\"signal-fish.sequenced-relay/v1\",",
        "\"trace_id\":\"dense-1\",\"event_count\":18}\n",
    ));
    fs::write(&input, trace).expect("write dense-domain trace");

    let output = run_sequenced_generator(&input, &output_dir, None);
    assert!(
        !output.status.success(),
        "dense replay unexpectedly succeeded"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "17 sender/receiver pair(s) x max epoch 4096 = 69632 dense cells; limit is 65536"
        ),
        "unexpected dense-domain diagnostic: {stderr}"
    );
}

#[test]
fn reconnect_requires_a_continuing_logical_receiver_view() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let input = temp.path().join("trace.jsonl");
    let output_dir = temp.path().join("bundle");
    fs::write(
        &input,
        SEQUENCED_TRACE
            .replace("\"receiver\":\"r1\"", "\"receiver\":\"r2\"")
            .replacen(
                "\"action\":\"ReceiverReconnect\",\"receiver\":\"r2\"",
                "\"action\":\"ReceiverReconnect\",\"receiver\":\"r1\"",
                1,
            ),
    )
    .expect("write disconnected logical receiver trace");

    let output = run_sequenced_generator(&input, &output_dir, None);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("active receiver view"),
        "unexpected diagnostic: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn malformed_sequenced_relay_traces_fail_closed() {
    let cases = [
        (
            "identity leakage",
            SEQUENCED_TRACE.replacen("\"receiver\":\"r1\"", "\"receiver\":\"alice\"", 1),
            "receiver must match",
        ),
        (
            "unknown field",
            SEQUENCED_TRACE.replacen(
                "\"baseline_seq\":0",
                "\"baseline_seq\":0,\"payload\":\"secret\"",
                1,
            ),
            "unknown field(s): payload",
        ),
        (
            "invalid gap reason",
            SEQUENCED_TRACE.replace("latest_superseded", "unclassified"),
            "reason must be one of",
        ),
        (
            "incomplete reconnect baseline",
            SEQUENCED_TRACE.replace(
                "\"seq\":9,\"action\":\"ReceiverBaseline\",\"receiver\":\"r1\",\"sender\":\"s1\",\"epoch\":2,\"baseline_seq\":1",
                "\"seq\":9,\"action\":\"Data\",\"receiver\":\"r1\",\"sender\":\"s1\",\"epoch\":2,\"data_seq\":1",
            ),
            "declares sender_count 1 but is followed by 0",
        ),
        (
            "mid-view baseline",
            SEQUENCED_TRACE.replacen(
                "\"seq\":5,\"action\":\"PlayerLeft\",\"receiver\":\"r1\",\"sender\":\"s1\",\"epoch\":1,\"final_seq\":2",
                "\"seq\":5,\"action\":\"ReceiverBaseline\",\"receiver\":\"r1\",\"sender\":\"s2\",\"epoch\":1,\"baseline_seq\":0",
                1,
            ),
            "ReceiverBaseline is legal only inside",
        ),
        (
            "oversized sequence",
            SEQUENCED_TRACE.replacen("\"data_seq\":1", "\"data_seq\":4097", 1),
            "data_seq must be a positive integer no greater than 4096",
        ),
        (
            "oversized epoch",
            SEQUENCED_TRACE.replacen("\"epoch\":1", "\"epoch\":4097", 1),
            "epoch must be a positive integer no greater than 4096",
        ),
        (
            "oversized sender count",
            SEQUENCED_TRACE.replacen("\"sender_count\":1", "\"sender_count\":4097", 1),
            "sender_count must be a nonnegative integer no greater than 4096",
        ),
        (
            "backward reconnect baseline",
            SEQUENCED_TRACE.replace(
                "\"seq\":9,\"action\":\"ReceiverBaseline\",\"receiver\":\"r1\",\"sender\":\"s1\",\"epoch\":2,\"baseline_seq\":1",
                "\"seq\":9,\"action\":\"ReceiverBaseline\",\"receiver\":\"r1\",\"sender\":\"s1\",\"epoch\":1,\"baseline_seq\":1",
            ),
            "moved backward from epoch/seq 2/1 to 1/1",
        ),
        (
            "sequence gap",
            SEQUENCED_TRACE.replacen("\"seq\":2", "\"seq\":99", 1),
            "seq must be contiguous",
        ),
        (
            "duplicate JSON field",
            SEQUENCED_TRACE.replacen(
                "\"protocol_version\":3",
                "\"protocol_version\":3,\"protocol_version\":3",
                1,
            ),
            "duplicate JSON field",
        ),
    ];

    for (description, trace, expected_error) in cases {
        let temp = tempfile::tempdir().expect("temporary directory");
        let input = temp.path().join("trace.jsonl");
        let output_dir = temp.path().join("bundle");
        fs::write(&input, trace).expect("write malformed trace");
        let output = run_sequenced_generator(&input, &output_dir, None);
        assert!(
            !output.status.success(),
            "{description} unexpectedly succeeded"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(expected_error),
            "{description}: expected {expected_error:?}, got {stderr:?}"
        );
    }
}
