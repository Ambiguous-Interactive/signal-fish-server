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
