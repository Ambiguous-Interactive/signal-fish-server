//! GOLDEN v3 WIRE SNAPSHOTS — these lock the protocol v3 delivery-reliability
//! additions.
//!
//! v3 is additive over the frozen v2 floor: the server-stamped `GameData.seq` /
//! `GameDataBinary.seq` relay sequence + incarnation `epoch`, and the opt-in
//! `RelayStats` frame. The pre-v3 (v2) forms (`seq: None`, no RelayStats) are
//! frozen byte-for-byte in `tests/v2_wire_golden.rs`, which MUST keep passing
//! unchanged; this file freezes the v3-recipient forms with the same assertion
//! strategy:
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
use signal_fish_server::protocol::{PlayerInfo, ServerMessage};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Deterministic fixtures — NO Uuid::new_v4() in golden values.
// ---------------------------------------------------------------------------

fn player_a() -> Uuid {
    Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0000_000a)
}

const PLAYER_A_STR: &str = "00000000-0000-0000-0000-00000000000a";

/// Deterministic timestamp for `PlayerInfo` snapshot goldens (matches the
/// `tests/v2_wire_golden.rs` fixture so the two files agree on the wire form).
fn fixed_time() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2024-01-02T03:04:05Z")
        .expect("valid RFC3339 fixture")
        .with_timezone(&chrono::Utc)
}

const FIXED_TIME_STR: &str = "2024-01-02T03:04:05Z";

// ---------------------------------------------------------------------------
// Assertion helpers (mirroring tests/v2_wire_golden.rs).
// ---------------------------------------------------------------------------

/// Assert that `value` serializes to the exact JSON `Value` `expected`
/// (structural, order-independent) AND to the exact raw JSON string `raw`.
fn assert_json<T: Serialize>(value: &T, expected: Value, raw: &str) {
    let actual_value = serde_json::to_value(value).expect("json value");
    assert_eq!(
        actual_value, expected,
        "JSON structural mismatch (BREAKING v3 wire change?)"
    );
    let actual_raw = serde_json::to_string(value).expect("json string");
    assert_eq!(
        actual_raw, raw,
        "JSON raw-string mismatch — field name/casing/ordering drift (BREAKING v3 wire change?)"
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
        "MessagePack byte mismatch (BREAKING v3 wire change?)\n  actual: {actual_hex}\n  golden: {expected_hex}"
    );
}

// ===========================================================================
// GameData with the server-stamped seq (v3 recipients).
// ===========================================================================

#[test]
fn golden_server_game_data_with_seq() {
    let msg = ServerMessage::GameData {
        from_player: player_a(),
        data: json!({ "move": "up" }),
        seq: Some(42),
        epoch: Some(3),
    };
    assert_json(
        &msg,
        json!({
            "type": "GameData",
            "data": { "from_player": PLAYER_A_STR, "data": { "move": "up" }, "seq": 42, "epoch": 3 }
        }),
        &format!(
            r#"{{"type":"GameData","data":{{"from_player":"{PLAYER_A_STR}","data":{{"move":"up"}},"seq":42,"epoch":3}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065a847616d6544617461a46461746184ab66726f6d5f706c61796572c4100000000000000000000000000000000aa46461746181a46d6f7665a27570a37365712aa565706f636803");
}

/// The seq stamp starts at 1 within an epoch, and the first incarnation is
/// epoch 1 — freeze the first-stamp form explicitly so the "starts at 1"
/// contract has a wire-level witness for BOTH counters.
#[test]
fn golden_server_game_data_with_first_seq_and_epoch() {
    let msg = ServerMessage::GameData {
        from_player: player_a(),
        data: json!({ "move": "up" }),
        seq: Some(1),
        epoch: Some(1),
    };
    assert_json(
        &msg,
        json!({
            "type": "GameData",
            "data": { "from_player": PLAYER_A_STR, "data": { "move": "up" }, "seq": 1, "epoch": 1 }
        }),
        &format!(
            r#"{{"type":"GameData","data":{{"from_player":"{PLAYER_A_STR}","data":{{"move":"up"}},"seq":1,"epoch":1}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065a847616d6544617461a46461746184ab66726f6d5f706c61796572c4100000000000000000000000000000000aa46461746181a46d6f7665a27570a373657101a565706f636801");
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
        epoch: Some(3),
    };
    assert_json(
        &msg,
        json!({
            "type": "GameDataBinary",
            "data": {
                "from_player": PLAYER_A_STR,
                "encoding": "message_pack",
                "payload": [1, 2, 3, 4],
                "seq": 7,
                "epoch": 3
            }
        }),
        &format!(
            r#"{{"type":"GameDataBinary","data":{{"from_player":"{PLAYER_A_STR}","encoding":"message_pack","payload":[1,2,3,4],"seq":7,"epoch":3}}}}"#
        ),
    );
}

// ===========================================================================
// RelayStats (v3-only, config-gated).
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
// Round-trips: v3 fields survive both wire encodings, and the unstamped form
// still decodes with `seq: None` (backward decode compatibility).
// ===========================================================================

#[test]
fn v3_game_data_seq_and_epoch_round_trip_json_and_msgpack() {
    // seq and epoch are stamped together; also cover each independently absent
    // (backward-decode compatibility) and their max values.
    for (seq, epoch) in [
        (None, None),
        (Some(1), Some(1)),
        (Some(u64::MAX), Some(u32::MAX)),
    ] {
        let msg = ServerMessage::GameData {
            from_player: player_a(),
            data: json!({ "k": "v" }),
            seq,
            epoch,
        };

        let json = serde_json::to_string(&msg).expect("json");
        match serde_json::from_str::<ServerMessage>(&json).expect("json round-trip") {
            ServerMessage::GameData {
                seq: rt_seq,
                epoch: rt_epoch,
                ..
            } => {
                assert_eq!(rt_seq, seq);
                assert_eq!(rt_epoch, epoch);
            }
            other => panic!("expected GameData, got {other:?}"),
        }

        let mp = rmp_serde::to_vec_named(&msg).expect("msgpack");
        match rmp_serde::from_slice::<ServerMessage>(&mp).expect("msgpack round-trip") {
            ServerMessage::GameData {
                seq: rt_seq,
                epoch: rt_epoch,
                ..
            } => {
                assert_eq!(rt_seq, seq);
                assert_eq!(rt_epoch, epoch);
            }
            other => panic!("expected GameData, got {other:?}"),
        }
    }
}

// ===========================================================================
// Epoch carriage on room snapshots (E1): PlayerReconnected + PlayerInfo.
// ===========================================================================

/// `PlayerReconnected` gains the reconnector's new incarnation epoch (v3). The
/// pre-v3 form (`epoch: None`) is frozen byte-identically in
/// `tests/v2_wire_golden.rs`; this freezes the v3-recipient form.
#[test]
fn golden_player_reconnected_with_epoch() {
    let msg = ServerMessage::PlayerReconnected {
        player_id: player_a(),
        epoch: Some(2),
    };
    assert_json(
        &msg,
        json!({
            "type": "PlayerReconnected",
            "data": { "player_id": PLAYER_A_STR, "epoch": 2 }
        }),
        &format!(
            r#"{{"type":"PlayerReconnected","data":{{"player_id":"{PLAYER_A_STR}","epoch":2}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065b1506c617965725265636f6e6e6563746564a46461746182a9706c617965725f6964c4100000000000000000000000000000000aa565706f636802");
}

/// A `PlayerInfo` inside a `PlayerJoined` snapshot carries the joiner's epoch
/// (v3). Freeze the exact placement (trailing `epoch` key, after
/// `connection_info` which is omitted here) so the snapshot wire cannot drift.
#[test]
fn golden_player_joined_player_info_with_epoch() {
    let msg = ServerMessage::PlayerJoined {
        player: PlayerInfo {
            id: player_a(),
            name: "P".to_string(),
            is_authority: false,
            is_ready: false,
            connected_at: fixed_time(),
            connection_info: None,
            epoch: Some(4),
            region_id: String::new(),
        },
    };
    assert_json(
        &msg,
        json!({
            "type": "PlayerJoined",
            "data": { "player": {
                "id": PLAYER_A_STR,
                "name": "P",
                "is_authority": false,
                "is_ready": false,
                "connected_at": FIXED_TIME_STR,
                "epoch": 4
            } }
        }),
        &format!(
            r#"{{"type":"PlayerJoined","data":{{"player":{{"id":"{PLAYER_A_STR}","name":"P","is_authority":false,"is_ready":false,"connected_at":"{FIXED_TIME_STR}","epoch":4}}}}}}"#
        ),
    );
}

#[test]
fn v3_relay_stats_round_trips_json_and_msgpack() {
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
