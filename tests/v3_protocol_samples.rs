//! Enforceable proof that the v3 wire samples match the real Rust types.
//!
//! Every non-blank line of the v3 JSONL sample files MUST deserialize
//! into the production [`ClientMessage`] / [`ServerMessage`] enums via
//! `serde_json::from_str` AND round-trip back to a `serde_json::Value` that is
//! **exactly equal** to the parsed source line. This is the acceptance test for
//! PLAN §P6 ("wire samples exactly match the actual Rust types").
//!
//! The value-equality check is what makes the guarantee real for *optional*
//! fields. `ClientMessage` / `ServerMessage` and their payloads do NOT use
//! `#[serde(deny_unknown_fields)]`, and the v3 fields are `Option` /
//! `#[serde(default)]`. A misspelled or renamed optional field in a sample
//! (e.g. `supported_transport` instead of `supported_transports`) would silently
//! deserialize to `None` and be dropped on re-serialize. By comparing the source
//! `Value` with the round-tripped `Value`, any field the type does not recognize
//! is absent from the round-trip ⇒ `source != roundtrip` ⇒ the test fails and
//! pinpoints the drifting line. (JSON object key order is normalized by `Value`
//! comparison, so field reordering is not a false positive.)
//!
//! For this to hold, sample lines must contain ONLY real wire fields with values
//! that round-trip exactly — including not carrying any field that the type would
//! drop via `skip_serializing_if` (e.g. an empty vec or a `None`).
//!
//! NOTE: only the v3 samples are checked here. The v2 samples are intentionally
//! *partial* — they use `"..."` placeholders and abbreviate nested payloads (e.g.
//! `"data": {}`, `{"id": "...", "name": "..."}`) for documentation brevity, so
//! they are not required to deserialize into a full message. The v2 wire contract
//! is already frozen byte-for-byte by `tests/v2_wire_golden.rs`. The v3 samples,
//! by contrast, are fully-populated concrete frames and MUST deserialize.

use serde_json::Value;
use signal_fish_server::protocol::{ClientMessage, PlayerInfo, ServerMessage};

/// Absolute path to a sample file, anchored at the crate manifest dir so the
/// test is independent of the process working directory.
fn sample_path(relative: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// Read a JSONL sample file and return its non-blank lines paired with their
/// 1-based line numbers (so a failure can point at the exact offending line).
fn numbered_nonblank_lines(relative: &str) -> Vec<(usize, String)> {
    let path = sample_path(relative);
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read sample file {}: {err}", path.display()));
    content
        .lines()
        .enumerate()
        .map(|(idx, line)| (idx + 1, line.to_string()))
        .filter(|(_, line)| !line.trim().is_empty())
        .collect()
}

/// Extract the `type` tag from a raw JSONL line (the externally-tagged
/// discriminant our message enums serialize under `#[serde(tag = "type")]`).
fn type_tag(line: &str) -> String {
    let value: Value = serde_json::from_str(line)
        .unwrap_or_else(|err| panic!("sample line is not valid JSON: {line}\n  error: {err}"));
    value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("sample line is missing a string `type` tag: {line}"))
        .to_string()
}

/// Deserialize every line of `relative` into `T`, asserting that:
/// 1. deserialization succeeds,
/// 2. the round-tripped value preserves the `type` tag, and
/// 3. the round-tripped value is **exactly equal** to the parsed source line.
///
/// Assertion (3) is the strong guard: any sample field the type fails to
/// recognize (a renamed/misspelled optional field) is dropped on round-trip and
/// surfaces as an inequality, naming the offending line.
fn assert_samples_deserialize<T>(relative: &str)
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let lines = numbered_nonblank_lines(relative);
    assert!(
        !lines.is_empty(),
        "sample file {relative} must contain at least one non-blank line"
    );

    for (line_no, line) in lines {
        let expected_tag = type_tag(&line);

        // The source line as a structural JSON value, used for exact comparison.
        let source: Value = serde_json::from_str(&line)
            .unwrap_or_else(|err| panic!("sample line is not valid JSON: {line}\n  error: {err}"));

        let message: T = serde_json::from_str(&line).unwrap_or_else(|err| {
            panic!(
                "sample {relative}:{line_no} did not deserialize into the real type \
                 (field name/casing/shape drift?)\n  line: {line}\n  error: {err}"
            )
        });

        let roundtrip = serde_json::to_value(&message)
            .unwrap_or_else(|err| panic!("re-serialization failed at {relative}:{line_no}: {err}"));
        let actual_tag = roundtrip
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("re-serialized value at {relative}:{line_no} lost its `type` tag")
            });

        assert_eq!(
            actual_tag, expected_tag,
            "round-trip changed the `type` tag at {relative}:{line_no} \
             (expected {expected_tag}, got {actual_tag})"
        );

        assert_eq!(
            source, roundtrip,
            "sample {relative}:{line_no} did not round-trip exactly — a field the \
             real type does not recognize was dropped, or a value re-serialized \
             differently (optional-field drift?)\n  source:    {source}\n  roundtrip: {roundtrip}"
        );
    }
}

#[test]
fn v3_client_message_samples_deserialize_into_client_message() {
    assert_samples_deserialize::<ClientMessage>(
        ".llm/code-samples/protocol/v3-client-messages.jsonl",
    );
}

#[test]
fn v3_server_message_samples_deserialize_into_server_message() {
    assert_samples_deserialize::<ServerMessage>(
        ".llm/code-samples/protocol/v3-server-messages.jsonl",
    );
}

fn assert_runtime_v3_snapshot_epochs(players: &[PlayerInfo], line_no: usize, kind: &str) {
    for player in players {
        assert!(
            player.epoch.is_some_and(|epoch| epoch > 0),
            "v3 sample line {line_no} has runtime-impossible {kind}.current_players epoch for {}: {:?}",
            player.id,
            player.epoch
        );
    }
}

/// Structural serde round trips cannot distinguish a legal optional wire field
/// from one production always populates after v3 negotiation. Pin the runtime
/// snapshot invariant explicitly so canonical v3 examples cannot omit epochs.
#[test]
fn v3_server_snapshot_samples_include_runtime_epochs() {
    let relative = ".llm/code-samples/protocol/v3-server-messages.jsonl";
    let mut saw_room_joined = false;

    for (line_no, line) in numbered_nonblank_lines(relative) {
        let message: ServerMessage = serde_json::from_str(&line).unwrap_or_else(|error| {
            panic!("invalid server sample at {relative}:{line_no}: {error}")
        });
        match message {
            ServerMessage::RoomJoined(payload) => {
                saw_room_joined = true;
                assert_runtime_v3_snapshot_epochs(&payload.current_players, line_no, "RoomJoined");
            }
            ServerMessage::SpectatorJoined(payload) => assert_runtime_v3_snapshot_epochs(
                &payload.current_players,
                line_no,
                "SpectatorJoined",
            ),
            ServerMessage::Reconnected(payload) => {
                assert_runtime_v3_snapshot_epochs(&payload.current_players, line_no, "Reconnected")
            }
            _ => {}
        }
    }

    assert!(saw_room_joined, "v3 server samples must include RoomJoined");
}
