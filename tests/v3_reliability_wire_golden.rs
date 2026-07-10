//! GOLDEN v3 WIRE SNAPSHOTS — these lock the protocol v3 delivery-reliability
//! additions.
//!
//! v3 is additive over the frozen v2 floor: game-data delivery classes, the
//! server-stamped `GameData.seq` / `GameDataBinary.seq` relay sequence +
//! incarnation `epoch`, `PlayerLeft` terminal watermarks, exact
//! `DeliveryReport` accountability, the opt-in `RelayStats` frame, and the
//! shutdown-drain `GoingAway` advisory. The pre-v3
//! (v2) forms are frozen byte-for-byte in `tests/v2_wire_golden.rs`, which MUST
//! keep passing unchanged; this file freezes the v3 forms with the same
//! assertion strategy:
//!
//! - JSON: structural equality against a `json!` value AND a raw-string
//!   assertion to catch field-name / casing / ordering drift.
//! - MessagePack via `rmp_serde::to_vec_named` (the production binary path):
//!   exact bytes via a hex helper.
//!
//! The physical v3 binary metadata envelope (which is NOT this enum's envelope)
//! is frozen for every payload encoding by unit tests in
//! `src/websocket/sending.rs`.

use serde::Serialize;
use serde_json::{json, Value};
use signal_fish_server::protocol::{
    ClientMessage, DeliveryClass, DeliveryCountersByClass, DeliveryGap, DeliveryGapReason,
    DeliveryReportPayload, LatestDeliveryCounters, LobbyState, PlayerInfo, ReconnectedPayload,
    ReliableDeliveryCounters, ReplayStatus, SenderWatermark, ServerMessage,
    VolatileDeliveryCounters,
};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Deterministic fixtures — NO Uuid::new_v4() in golden values.
// ---------------------------------------------------------------------------

fn player_a() -> Uuid {
    Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0000_000a)
}

fn player_b() -> Uuid {
    Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0000_000b)
}

fn room() -> Uuid {
    Uuid::from_u128(0x1111_1111_1111_1111_1111_1111_1111_1111)
}

const PLAYER_A_STR: &str = "00000000-0000-0000-0000-00000000000a";
const PLAYER_B_STR: &str = "00000000-0000-0000-0000-00000000000b";
const ROOM_STR: &str = "11111111-1111-1111-1111-111111111111";

/// Deterministic timestamp for `PlayerInfo` snapshot goldens (matches the
/// `tests/v2_wire_golden.rs` fixture so the two files agree on the wire form).
fn fixed_time() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2024-01-02T03:04:05Z")
        .expect("valid RFC3339 fixture")
        .with_timezone(&chrono::Utc)
}

const FIXED_TIME_STR: &str = "2024-01-02T03:04:05Z";

fn player_info_a_with_epoch() -> PlayerInfo {
    PlayerInfo {
        id: player_a(),
        name: "Alice".to_string(),
        is_authority: true,
        is_ready: false,
        connected_at: fixed_time(),
        connection_info: None,
        epoch: Some(4),
        seq: Some(42),
        region_id: String::new(),
    }
}

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
// Client delivery-class requests and classified server GameData (v3).
// ===========================================================================

#[test]
fn golden_client_delivery_class_requests() {
    let cases = [
        (
            ClientMessage::GameData {
                data: json!({ "state": "newest" }),
                class: Some(DeliveryClass::Latest),
                key: Some(7),
            },
            json!({
                "type": "GameData",
                "data": { "data": { "state": "newest" }, "class": "latest", "key": 7 }
            }),
            r#"{"type":"GameData","data":{"data":{"state":"newest"},"class":"latest","key":7}}"#,
            "82a474797065a847616d6544617461a46461746183a46461746181a57374617465a66e6577657374a5636c617373a66c6174657374a36b657907",
        ),
        (
            ClientMessage::GameData {
                data: json!({ "effect": "spark" }),
                class: Some(DeliveryClass::Volatile),
                key: None,
            },
            json!({
                "type": "GameData",
                "data": { "data": { "effect": "spark" }, "class": "volatile" }
            }),
            r#"{"type":"GameData","data":{"data":{"effect":"spark"},"class":"volatile"}}"#,
            "82a474797065a847616d6544617461a46461746182a46461746181a6656666656374a5737061726ba5636c617373a8766f6c6174696c65",
        ),
    ];

    for (message, expected_json, expected_raw, expected_msgpack) in cases {
        assert_json(&message, expected_json, expected_raw);
        assert_msgpack(&message, expected_msgpack);
    }
}

#[test]
fn golden_server_classified_game_data() {
    let message = ServerMessage::GameData {
        from_player: player_a(),
        data: json!({ "state": "fresh" }),
        seq: Some(9),
        epoch: Some(2),
        class: Some(DeliveryClass::Latest),
        key: Some(17),
    };

    assert_json(
        &message,
        json!({
            "type": "GameData",
            "data": {
                "from_player": PLAYER_A_STR,
                "data": { "state": "fresh" },
                "seq": 9,
                "epoch": 2,
                "class": "latest",
                "key": 17
            }
        }),
        &format!(
            r#"{{"type":"GameData","data":{{"from_player":"{PLAYER_A_STR}","data":{{"state":"fresh"}},"seq":9,"epoch":2,"class":"latest","key":17}}}}"#
        ),
    );
    assert_msgpack(&message, "82a474797065a847616d6544617461a46461746186ab66726f6d5f706c61796572c4100000000000000000000000000000000aa46461746181a57374617465a56672657368a373657109a565706f636802a5636c617373a66c6174657374a36b657911");
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
        class: None,
        key: None,
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
        class: None,
        key: None,
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
/// caveat: v3 binary clients receive the physical `V3BinaryGameDataFrame`,
/// frozen in `src/websocket/sending.rs`).
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
// DeliveryReport and RelayStats (v3-only accountability).
// ===========================================================================

#[test]
fn golden_server_delivery_report_with_exact_gap() {
    let message = ServerMessage::DeliveryReport(Box::new(DeliveryReportPayload {
        per_class: DeliveryCountersByClass {
            reliable: ReliableDeliveryCounters {
                delivered: 1,
                abandoned: 2,
                unsupported_format: 3,
            },
            latest: LatestDeliveryCounters {
                delivered: 4,
                superseded: 5,
                dropped_full: 6,
                abandoned: 7,
                unsupported_format: 8,
            },
            volatile: VolatileDeliveryCounters {
                delivered: 9,
                dropped: 10,
                abandoned: 11,
                unsupported_format: 12,
            },
        },
        gaps: vec![DeliveryGap {
            from_player: player_a(),
            epoch: 3,
            from_seq: 13,
            to_seq: 15,
            reason: DeliveryGapReason::LatestSuperseded,
        }],
    }));

    assert_json(
        &message,
        json!({
            "type": "DeliveryReport",
            "data": {
                "per_class": {
                    "reliable": { "delivered": 1, "abandoned": 2, "unsupported_format": 3 },
                    "latest": {
                        "delivered": 4,
                        "superseded": 5,
                        "dropped_full": 6,
                        "abandoned": 7,
                        "unsupported_format": 8
                    },
                    "volatile": {
                        "delivered": 9,
                        "dropped": 10,
                        "abandoned": 11,
                        "unsupported_format": 12
                    }
                },
                "gaps": [{
                    "from_player": PLAYER_A_STR,
                    "epoch": 3,
                    "from_seq": 13,
                    "to_seq": 15,
                    "reason": "latest_superseded"
                }]
            }
        }),
        &format!(
            r#"{{"type":"DeliveryReport","data":{{"per_class":{{"reliable":{{"delivered":1,"abandoned":2,"unsupported_format":3}},"latest":{{"delivered":4,"superseded":5,"dropped_full":6,"abandoned":7,"unsupported_format":8}},"volatile":{{"delivered":9,"dropped":10,"abandoned":11,"unsupported_format":12}}}},"gaps":[{{"from_player":"{PLAYER_A_STR}","epoch":3,"from_seq":13,"to_seq":15,"reason":"latest_superseded"}}]}}}}"#
        ),
    );
    assert_msgpack(&message, "82a474797065ae44656c69766572795265706f7274a46461746182a97065725f636c61737383a872656c6961626c6583a964656c69766572656401a96162616e646f6e656402b2756e737570706f727465645f666f726d617403a66c617465737485a964656c69766572656404aa7375706572736564656405ac64726f707065645f66756c6c06a96162616e646f6e656407b2756e737570706f727465645f666f726d617408a8766f6c6174696c6584a964656c69766572656409a764726f707065640aa96162616e646f6e65640bb2756e737570706f727465645f666f726d61740ca4676170739185ab66726f6d5f706c61796572c4100000000000000000000000000000000aa565706f636803a866726f6d5f7365710da6746f5f7365710fa6726561736f6eb16c61746573745f73757065727365646564");
}

#[test]
fn golden_server_delivery_report_omits_empty_gaps() {
    let message = ServerMessage::DeliveryReport(Box::default());
    assert_json(
        &message,
        json!({
            "type": "DeliveryReport",
            "data": {
                "per_class": {
                    "reliable": { "delivered": 0, "abandoned": 0, "unsupported_format": 0 },
                    "latest": {
                        "delivered": 0,
                        "superseded": 0,
                        "dropped_full": 0,
                        "abandoned": 0,
                        "unsupported_format": 0
                    },
                    "volatile": {
                        "delivered": 0,
                        "dropped": 0,
                        "abandoned": 0,
                        "unsupported_format": 0
                    }
                }
            }
        }),
        r#"{"type":"DeliveryReport","data":{"per_class":{"reliable":{"delivered":0,"abandoned":0,"unsupported_format":0},"latest":{"delivered":0,"superseded":0,"dropped_full":0,"abandoned":0,"unsupported_format":0},"volatile":{"delivered":0,"dropped":0,"abandoned":0,"unsupported_format":0}}}}"#,
    );
    assert_msgpack(&message, "82a474797065ae44656c69766572795265706f7274a46461746181a97065725f636c61737383a872656c6961626c6583a964656c69766572656400a96162616e646f6e656400b2756e737570706f727465645f666f726d617400a66c617465737485a964656c69766572656400aa7375706572736564656400ac64726f707065645f66756c6c00a96162616e646f6e656400b2756e737570706f727465645f666f726d617400a8766f6c6174696c6584a964656c69766572656400a764726f7070656400a96162616e646f6e656400b2756e737570706f727465645f666f726d617400");
}

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

#[test]
fn golden_server_going_away() {
    let msg = ServerMessage::GoingAway {
        deadline_ms: 1_700_000_000_000,
        retry_after_secs: Some(30),
    };
    assert_json(
        &msg,
        json!({
            "type": "GoingAway",
            "data": {
                "deadline_ms": 1700000000000_u64,
                "retry_after_secs": 30
            }
        }),
        r#"{"type":"GoingAway","data":{"deadline_ms":1700000000000,"retry_after_secs":30}}"#,
    );
    assert_msgpack(&msg, "82a474797065a9476f696e6741776179a46461746182ab646561646c696e655f6d73cf0000018bcfe56800b072657472795f61667465725f736563731e");
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
            class: None,
            key: None,
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

#[test]
fn golden_player_left_with_terminal_watermark() {
    let msg = ServerMessage::PlayerLeft {
        player_id: player_a(),
        epoch: Some(4),
        final_seq: Some(42),
    };
    assert_json(
        &msg,
        json!({
            "type": "PlayerLeft",
            "data": { "player_id": PLAYER_A_STR, "epoch": 4, "final_seq": 42 }
        }),
        &format!(
            r#"{{"type":"PlayerLeft","data":{{"player_id":"{PLAYER_A_STR}","epoch":4,"final_seq":42}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065aa506c617965724c656674a46461746183a9706c617965725f6964c4100000000000000000000000000000000aa565706f636804a966696e616c5f7365712a");
}

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

/// `Reconnected.sender_watermarks` is the v3 reconnect baseline: every current
/// room member's authoritative `(epoch, seq)` tail, including members that have
/// not relayed any GameData in the incarnation yet (`seq: 0`). The v2 form
/// (`sender_watermarks: []`) is frozen in `tests/v2_wire_golden.rs`.
#[test]
fn golden_reconnected_with_sender_watermarks() {
    let msg = ServerMessage::Reconnected(Box::new(ReconnectedPayload {
        room_id: room(),
        room_code: "ABC123".to_string(),
        player_id: player_b(),
        game_name: "test_game".to_string(),
        max_players: 4,
        supports_authority: true,
        current_players: vec![player_info_a_with_epoch()],
        is_authority: false,
        lobby_state: LobbyState::Lobby,
        ready_players: vec![player_a()],
        relay_type: "matchbox".to_string(),
        current_spectators: vec![],
        ice_servers: vec![],
        missed_events: vec![],
        replay: Some(ReplayStatus::Complete),
        sender_watermarks: vec![
            SenderWatermark {
                player_id: player_a(),
                epoch: 4,
                seq: 42,
            },
            SenderWatermark {
                player_id: player_b(),
                epoch: 1,
                seq: 0,
            },
        ],
        reconnection_token: None,
    }));
    assert_json(
        &msg,
        json!({
            "type": "Reconnected",
            "data": {
                "room_id": ROOM_STR,
                "room_code": "ABC123",
                "player_id": PLAYER_B_STR,
                "game_name": "test_game",
                "max_players": 4,
                "supports_authority": true,
                "current_players": [{
                    "id": PLAYER_A_STR,
                    "name": "Alice",
                    "is_authority": true,
                    "is_ready": false,
                    "connected_at": FIXED_TIME_STR,
                    "epoch": 4,
                    "seq": 42
                }],
                "is_authority": false,
                "lobby_state": "lobby",
                "ready_players": [PLAYER_A_STR],
                "relay_type": "matchbox",
                "current_spectators": [],
                "missed_events": [],
                "replay": "complete",
                "sender_watermarks": [{
                    "player_id": PLAYER_A_STR,
                    "epoch": 4,
                    "seq": 42
                }, {
                    "player_id": PLAYER_B_STR,
                    "epoch": 1,
                    "seq": 0
                }]
            }
        }),
        &format!(
            r#"{{"type":"Reconnected","data":{{"room_id":"{ROOM_STR}","room_code":"ABC123","player_id":"{PLAYER_B_STR}","game_name":"test_game","max_players":4,"supports_authority":true,"current_players":[{{"id":"{PLAYER_A_STR}","name":"Alice","is_authority":true,"is_ready":false,"connected_at":"{FIXED_TIME_STR}","epoch":4,"seq":42}}],"is_authority":false,"lobby_state":"lobby","ready_players":["{PLAYER_A_STR}"],"relay_type":"matchbox","current_spectators":[],"missed_events":[],"replay":"complete","sender_watermarks":[{{"player_id":"{PLAYER_A_STR}","epoch":4,"seq":42}},{{"player_id":"{PLAYER_B_STR}","epoch":1,"seq":0}}]}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065ab5265636f6e6e6563746564a4646174618fa7726f6f6d5f6964c41011111111111111111111111111111111a9726f6f6d5f636f6465a6414243313233a9706c617965725f6964c4100000000000000000000000000000000ba967616d655f6e616d65a9746573745f67616d65ab6d61785f706c617965727304b2737570706f7274735f617574686f72697479c3af63757272656e745f706c61796572739187a26964c4100000000000000000000000000000000aa46e616d65a5416c696365ac69735f617574686f72697479c3a869735f7265616479c2ac636f6e6e65637465645f6174b4323032342d30312d30325430333a30343a30355aa565706f636804a37365712aac69735f617574686f72697479c2ab6c6f6262795f7374617465a56c6f626279ad72656164795f706c617965727391c4100000000000000000000000000000000aaa72656c61795f74797065a86d61746368626f78b263757272656e745f737065637461746f727390ad6d69737365645f6576656e747390a67265706c6179a8636f6d706c657465b173656e6465725f77617465726d61726b739283a9706c617965725f6964c4100000000000000000000000000000000aa565706f636804a37365712a83a9706c617965725f6964c4100000000000000000000000000000000ba565706f636801a373657100");
}

/// A `PlayerInfo` inside a `PlayerJoined` snapshot carries the joiner's exact
/// `(epoch, seq)` baseline (v3).
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
            seq: Some(0),
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
                "epoch": 4,
                "seq": 0
            } }
        }),
        &format!(
            r#"{{"type":"PlayerJoined","data":{{"player":{{"id":"{PLAYER_A_STR}","name":"P","is_authority":false,"is_ready":false,"connected_at":"{FIXED_TIME_STR}","epoch":4,"seq":0}}}}}}"#
        ),
    );
    // Freeze the MessagePack encoding too (matching the other goldens here and
    // in `v2_wire_golden.rs`), so the production binary snapshot wire cannot
    // drift the paired baseline keys silently.
    assert_msgpack(&msg, "82a474797065ac506c617965724a6f696e6564a46461746181a6706c6179657287a26964c4100000000000000000000000000000000aa46e616d65a150ac69735f617574686f72697479c2a869735f7265616479c2ac636f6e6e65637465645f6174b4323032342d30312d30325430333a30343a30355aa565706f636804a373657100");
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

#[test]
fn v3_going_away_round_trips_json_and_msgpack() {
    for retry_after_secs in [Some(1), None] {
        let msg = ServerMessage::GoingAway {
            deadline_ms: 1_700_000_000_001,
            retry_after_secs,
        };

        let json = serde_json::to_string(&msg).expect("json");
        match serde_json::from_str::<ServerMessage>(&json).expect("json round-trip") {
            ServerMessage::GoingAway {
                deadline_ms,
                retry_after_secs: actual_retry_after_secs,
            } => {
                assert_eq!(deadline_ms, 1_700_000_000_001);
                assert_eq!(actual_retry_after_secs, retry_after_secs);
            }
            other => panic!("expected GoingAway, got {other:?}"),
        }

        let mp = rmp_serde::to_vec_named(&msg).expect("msgpack");
        match rmp_serde::from_slice::<ServerMessage>(&mp).expect("msgpack round-trip") {
            ServerMessage::GoingAway {
                retry_after_secs: actual_retry_after_secs,
                ..
            } => assert_eq!(actual_retry_after_secs, retry_after_secs),
            other => panic!("expected GoingAway, got {other:?}"),
        }
    }
}
