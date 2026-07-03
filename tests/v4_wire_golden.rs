//! GOLDEN v4 WIRE SNAPSHOTS — these lock the protocol v4 additions.
//!
//! v4 is additive over v2/v3: the server-stamped `GameData.seq` /
//! `GameDataBinary.seq` relay sequence and the opt-in `RelayStats` frame. The
//! pre-v4 forms (`seq: None`, no RelayStats) are frozen byte-for-byte in
//! `tests/v2_wire_golden.rs`, which MUST keep passing unchanged; this file
//! freezes the v4-recipient forms with the same assertion strategy:
//!
//! - JSON: structural equality against a `json!` value AND a raw-string
//!   assertion to catch field-name / casing / ordering drift.
//! - MessagePack via `rmp_serde::to_vec_named` (the production binary path):
//!   exact bytes via a hex helper.
//!
//! The bare binary `BinaryGameDataFrame` (which is NOT this enum's envelope)
//! is frozen separately by the unit tests in `src/websocket/sending.rs`.

use serde::Serialize;
use serde_json::{json, Value};
use signal_fish_server::protocol::ServerMessage;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Deterministic fixtures — NO Uuid::new_v4() in golden values.
// ---------------------------------------------------------------------------

fn player_a() -> Uuid {
    Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0000_000a)
}

const PLAYER_A_STR: &str = "00000000-0000-0000-0000-00000000000a";

// ---------------------------------------------------------------------------
// Assertion helpers (mirroring tests/v2_wire_golden.rs).
// ---------------------------------------------------------------------------

/// Assert that `value` serializes to the exact JSON `Value` `expected`
/// (structural, order-independent) AND to the exact raw JSON string `raw`.
fn assert_json<T: Serialize>(value: &T, expected: Value, raw: &str) {
    let actual_value = serde_json::to_value(value).expect("json value");
    assert_eq!(
        actual_value, expected,
        "JSON structural mismatch (BREAKING v4 wire change?)"
    );
    let actual_raw = serde_json::to_string(value).expect("json string");
    assert_eq!(
        actual_raw, raw,
        "JSON raw-string mismatch — field name/casing/ordering drift (BREAKING v4 wire change?)"
    );
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Assert that `value` serializes via `rmp_serde::to_vec_named` (production
/// binary path) to exactly the hex-encoded `expected_hex` bytes.
fn assert_msgpack<T: Serialize>(value: &T, expected_hex: &str) {
    let bytes = rmp_serde::to_vec_named(value).expect("msgpack bytes");
    let actual_hex = hex(&bytes);
    assert_eq!(
        actual_hex, expected_hex,
        "MessagePack byte mismatch (BREAKING v4 wire change?)\n  actual: {actual_hex}\n  golden: {expected_hex}"
    );
}

// ===========================================================================
// GameData with the server-stamped seq (v4 recipients).
// ===========================================================================

#[test]
fn golden_server_game_data_with_seq() {
    let msg = ServerMessage::GameData {
        from_player: player_a(),
        data: json!({ "move": "up" }),
        seq: Some(42),
    };
    assert_json(
        &msg,
        json!({
            "type": "GameData",
            "data": { "from_player": PLAYER_A_STR, "data": { "move": "up" }, "seq": 42 }
        }),
        &format!(
            r#"{{"type":"GameData","data":{{"from_player":"{PLAYER_A_STR}","data":{{"move":"up"}},"seq":42}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065a847616d6544617461a46461746183ab66726f6d5f706c61796572c4100000000000000000000000000000000aa46461746181a46d6f7665a27570a37365712a");
}

/// The seq stamp starts at 1 — freeze the first-stamp form explicitly so the
/// "starts at 1" contract has a wire-level witness.
#[test]
fn golden_server_game_data_with_first_seq() {
    let msg = ServerMessage::GameData {
        from_player: player_a(),
        data: json!({ "move": "up" }),
        seq: Some(1),
    };
    assert_json(
        &msg,
        json!({
            "type": "GameData",
            "data": { "from_player": PLAYER_A_STR, "data": { "move": "up" }, "seq": 1 }
        }),
        &format!(
            r#"{{"type":"GameData","data":{{"from_player":"{PLAYER_A_STR}","data":{{"move":"up"}},"seq":1}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065a847616d6544617461a46461746183ab66726f6d5f706c61796572c4100000000000000000000000000000000aa46461746181a46d6f7665a27570a373657101");
}

/// In-memory representation ONLY — NOT the wire form (mirrors the v2 golden's
/// caveat: negotiated MessagePack clients receive the bare
/// `BinaryGameDataFrame`, frozen in `src/websocket/sending.rs`).
#[test]
fn golden_server_game_data_binary_with_seq_in_memory_repr_not_wire() {
    let msg = ServerMessage::GameDataBinary {
        from_player: player_a(),
        encoding: signal_fish_server::protocol::GameDataEncoding::MessagePack,
        payload: bytes::Bytes::from_static(&[0x01, 0x02, 0x03, 0x04]),
        seq: Some(7),
    };
    assert_json(
        &msg,
        json!({
            "type": "GameDataBinary",
            "data": {
                "from_player": PLAYER_A_STR,
                "encoding": "message_pack",
                "payload": [1, 2, 3, 4],
                "seq": 7
            }
        }),
        &format!(
            r#"{{"type":"GameDataBinary","data":{{"from_player":"{PLAYER_A_STR}","encoding":"message_pack","payload":[1,2,3,4],"seq":7}}}}"#
        ),
    );
}

// ===========================================================================
// RelayStats (v4-only, config-gated).
// ===========================================================================

#[test]
fn golden_server_relay_stats() {
    let msg = ServerMessage::RelayStats {
        interval_ms: 1_000,
        sent_to_you: 128,
        dropped_for_you: 2,
        backpressure_events: 5,
    };
    assert_json(
        &msg,
        json!({
            "type": "RelayStats",
            "data": {
                "interval_ms": 1000,
                "sent_to_you": 128,
                "dropped_for_you": 2,
                "backpressure_events": 5
            }
        }),
        r#"{"type":"RelayStats","data":{"interval_ms":1000,"sent_to_you":128,"dropped_for_you":2,"backpressure_events":5}}"#,
    );
    assert_msgpack(&msg, "82a474797065aa52656c61795374617473a46461746184ab696e74657276616c5f6d73cd03e8ab73656e745f746f5f796f75cc80af64726f707065645f666f725f796f7502b36261636b70726573737572655f6576656e747305");
}

// ===========================================================================
// Round-trips: v4 fields survive both wire encodings, and the unstamped form
// still decodes with `seq: None` (backward decode compatibility).
// ===========================================================================

#[test]
fn v4_game_data_seq_round_trips_json_and_msgpack() {
    for seq in [None, Some(1), Some(u64::MAX)] {
        let msg = ServerMessage::GameData {
            from_player: player_a(),
            data: json!({ "k": "v" }),
            seq,
        };

        let json = serde_json::to_string(&msg).expect("json");
        match serde_json::from_str::<ServerMessage>(&json).expect("json round-trip") {
            ServerMessage::GameData { seq: rt_seq, .. } => assert_eq!(rt_seq, seq),
            other => panic!("expected GameData, got {other:?}"),
        }

        let mp = rmp_serde::to_vec_named(&msg).expect("msgpack");
        match rmp_serde::from_slice::<ServerMessage>(&mp).expect("msgpack round-trip") {
            ServerMessage::GameData { seq: rt_seq, .. } => assert_eq!(rt_seq, seq),
            other => panic!("expected GameData, got {other:?}"),
        }
    }
}

#[test]
fn v4_relay_stats_round_trips_json_and_msgpack() {
    let msg = ServerMessage::RelayStats {
        interval_ms: 60_000,
        sent_to_you: u64::MAX,
        dropped_for_you: 0,
        backpressure_events: 1,
    };

    let json = serde_json::to_string(&msg).expect("json");
    match serde_json::from_str::<ServerMessage>(&json).expect("json round-trip") {
        ServerMessage::RelayStats {
            interval_ms,
            sent_to_you,
            dropped_for_you,
            backpressure_events,
        } => {
            assert_eq!(interval_ms, 60_000);
            assert_eq!(sent_to_you, u64::MAX);
            assert_eq!(dropped_for_you, 0);
            assert_eq!(backpressure_events, 1);
        }
        other => panic!("expected RelayStats, got {other:?}"),
    }

    let mp = rmp_serde::to_vec_named(&msg).expect("msgpack");
    match rmp_serde::from_slice::<ServerMessage>(&mp).expect("msgpack round-trip") {
        ServerMessage::RelayStats { sent_to_you, .. } => assert_eq!(sent_to_you, u64::MAX),
        other => panic!("expected RelayStats, got {other:?}"),
    }
}
