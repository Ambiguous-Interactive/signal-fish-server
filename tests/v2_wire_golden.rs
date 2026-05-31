//! GOLDEN v2 WIRE SNAPSHOTS — these lock the v2 protocol.
//!
//! Any diff is a BREAKING CHANGE and must be intentional. v3 additions use
//! `#[serde(skip_serializing_if)]` / `#[serde(default)]` so these snapshots MUST
//! remain byte-identical after v3 fields are added.
//!
//! Two wire formats are locked here, matching production serialization:
//! - JSON via `serde_json` (the default text-frame encoding).
//! - MessagePack via `rmp_serde::to_vec_named` (matches the binary send path in
//!   `src/websocket/sending.rs`, which uses `to_vec_named` so map keys are
//!   field-named, not positional).
//!
//! Assertion strategy:
//! - JSON: assert structural equality against a `serde_json::Value` built with
//!   `json!` (robust to key ordering), AND keep at least one raw-string
//!   assertion per message family to catch field-name / casing drift.
//! - MessagePack: assert exact bytes via a hex helper that prints both sides on
//!   mismatch for debuggability.

use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use signal_fish_server::protocol::{
    ClientMessage, ConnectionInfo, ErrorCode, GameDataEncoding, LobbyState, PeerConnectionInfo,
    PlayerInfo, ProtocolInfoPayload, RateLimitInfo, ReconnectedPayload, RelayTransport,
    RoomJoinedPayload, ServerMessage, SpectatorInfo, SpectatorJoinedPayload,
    SpectatorStateChangeReason,
};
// Production binary-frame send path. Binary game data is NOT enveloped in the
// `ServerMessage` enum on the wire — see `golden_server_game_data_binary`.
use signal_fish_server::websocket::{encode_binary_game_data, BinaryGameDataFrame};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Deterministic fixtures — NO Uuid::new_v4() / Utc::now() in golden values.
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

fn fixed_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2024-01-02T03:04:05Z")
        .expect("valid RFC3339 fixture")
        .with_timezone(&Utc)
}

const TIME_STR: &str = "2024-01-02T03:04:05Z";

fn player_info_a() -> PlayerInfo {
    PlayerInfo {
        id: player_a(),
        name: "Alice".to_string(),
        is_authority: true,
        is_ready: false,
        connected_at: fixed_time(),
        connection_info: None,
        region_id: String::new(),
    }
}

fn player_info_b() -> PlayerInfo {
    PlayerInfo {
        id: player_b(),
        name: "Bob".to_string(),
        is_authority: false,
        is_ready: true,
        connected_at: fixed_time(),
        connection_info: Some(ConnectionInfo::Direct {
            host: "10.0.0.5".to_string(),
            port: 7777,
        }),
        region_id: String::new(),
    }
}

fn spectator() -> SpectatorInfo {
    SpectatorInfo {
        id: player_b(),
        name: "Watcher".to_string(),
        connected_at: fixed_time(),
    }
}

fn rate_limits() -> RateLimitInfo {
    RateLimitInfo {
        per_minute: 60,
        per_hour: 3600,
        per_day: 86400,
    }
}

// ---------------------------------------------------------------------------
// Assertion helpers.
// ---------------------------------------------------------------------------

/// Assert that `value` serializes to the exact JSON `Value` `expected`
/// (structural, order-independent) AND to the exact raw JSON string `raw`.
fn assert_json<T: Serialize>(value: &T, expected: Value, raw: &str) {
    let actual_value = serde_json::to_value(value).expect("json value");
    assert_eq!(
        actual_value, expected,
        "JSON structural mismatch (BREAKING v2 wire change?)"
    );
    let actual_raw = serde_json::to_string(value).expect("json string");
    assert_eq!(
        actual_raw, raw,
        "JSON raw-string mismatch — field name/casing/ordering drift (BREAKING v2 wire change?)"
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
        "MessagePack byte mismatch (BREAKING v2 wire change?)\n  actual: {actual_hex}\n  golden: {expected_hex}"
    );
}

// ===========================================================================
// ClientMessage golden snapshots
// ===========================================================================

#[test]
fn golden_client_authenticate() {
    let msg = ClientMessage::Authenticate {
        app_id: "mb_app_test".to_string(),
        sdk_version: Some("1.2.3".to_string()),
        platform: Some("godot".to_string()),
        game_data_format: Some(GameDataEncoding::MessagePack),
        protocol_version: None,
        supported_transports: None,
        supported_topologies: None,
    };
    assert_json(
        &msg,
        json!({
            "type": "Authenticate",
            "data": {
                "app_id": "mb_app_test",
                "sdk_version": "1.2.3",
                "platform": "godot",
                "game_data_format": "message_pack"
            }
        }),
        r#"{"type":"Authenticate","data":{"app_id":"mb_app_test","sdk_version":"1.2.3","platform":"godot","game_data_format":"message_pack"}}"#,
    );
    assert_msgpack(&msg, "82a474797065ac41757468656e746963617465a46461746184a66170705f6964ab6d625f6170705f74657374ab73646b5f76657273696f6ea5312e322e33a8706c6174666f726da5676f646f74b067616d655f646174615f666f726d6174ac6d6573736167655f7061636b");
}

#[test]
fn golden_client_authenticate_minimal() {
    // All optional fields absent — pure v2 minimal shape.
    let msg = ClientMessage::Authenticate {
        app_id: "mb_app_test".to_string(),
        sdk_version: None,
        platform: None,
        game_data_format: None,
        protocol_version: None,
        supported_transports: None,
        supported_topologies: None,
    };
    assert_json(
        &msg,
        json!({ "type": "Authenticate", "data": { "app_id": "mb_app_test" } }),
        r#"{"type":"Authenticate","data":{"app_id":"mb_app_test"}}"#,
    );
    assert_msgpack(
        &msg,
        "82a474797065ac41757468656e746963617465a46461746181a66170705f6964ab6d625f6170705f74657374",
    );
}

#[test]
fn golden_client_join_room() {
    let msg = ClientMessage::JoinRoom {
        game_name: "test_game".to_string(),
        room_code: Some("ABC123".to_string()),
        player_name: "Alice".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: Some(RelayTransport::Udp),
    };
    assert_json(
        &msg,
        json!({
            "type": "JoinRoom",
            "data": {
                "game_name": "test_game",
                "room_code": "ABC123",
                "player_name": "Alice",
                "max_players": 4,
                "supports_authority": true,
                "relay_transport": "udp"
            }
        }),
        r#"{"type":"JoinRoom","data":{"game_name":"test_game","room_code":"ABC123","player_name":"Alice","max_players":4,"supports_authority":true,"relay_transport":"udp"}}"#,
    );
    assert_msgpack(&msg, "82a474797065a84a6f696e526f6f6da46461746186a967616d655f6e616d65a9746573745f67616d65a9726f6f6d5f636f6465a6414243313233ab706c617965725f6e616d65a5416c696365ab6d61785f706c617965727304b2737570706f7274735f617574686f72697479c3af72656c61795f7472616e73706f7274a3756470");
}

#[test]
fn golden_client_join_room_minimal() {
    // No JoinRoom field has `skip_serializing_if`, so every optional field
    // serializes explicitly as `null` — uniquely freezing the `relay_transport:
    // None` form (and the other `None` optionals) that production emits.
    let msg = ClientMessage::JoinRoom {
        game_name: "test_game".to_string(),
        room_code: None,
        player_name: "Alice".to_string(),
        max_players: None,
        supports_authority: None,
        relay_transport: None,
    };
    assert_json(
        &msg,
        json!({
            "type": "JoinRoom",
            "data": {
                "game_name": "test_game",
                "room_code": null,
                "player_name": "Alice",
                "max_players": null,
                "supports_authority": null,
                "relay_transport": null
            }
        }),
        r#"{"type":"JoinRoom","data":{"game_name":"test_game","room_code":null,"player_name":"Alice","max_players":null,"supports_authority":null,"relay_transport":null}}"#,
    );
    assert_msgpack(&msg, "82a474797065a84a6f696e526f6f6da46461746186a967616d655f6e616d65a9746573745f67616d65a9726f6f6d5f636f6465c0ab706c617965725f6e616d65a5416c696365ab6d61785f706c6179657273c0b2737570706f7274735f617574686f72697479c0af72656c61795f7472616e73706f7274c0");
}

#[test]
fn golden_client_leave_room() {
    let msg = ClientMessage::LeaveRoom;
    assert_json(
        &msg,
        json!({ "type": "LeaveRoom" }),
        r#"{"type":"LeaveRoom"}"#,
    );
    assert_msgpack(&msg, "81a474797065a94c65617665526f6f6d");
}

#[test]
fn golden_client_game_data() {
    let msg = ClientMessage::GameData {
        data: json!({ "x": 1, "y": 2 }),
    };
    assert_json(
        &msg,
        json!({ "type": "GameData", "data": { "data": { "x": 1, "y": 2 } } }),
        r#"{"type":"GameData","data":{"data":{"x":1,"y":2}}}"#,
    );
    assert_msgpack(
        &msg,
        "82a474797065a847616d6544617461a46461746181a46461746182a17801a17902",
    );
}

#[test]
fn golden_client_authority_request() {
    let msg = ClientMessage::AuthorityRequest {
        become_authority: true,
    };
    assert_json(
        &msg,
        json!({ "type": "AuthorityRequest", "data": { "become_authority": true } }),
        r#"{"type":"AuthorityRequest","data":{"become_authority":true}}"#,
    );
    assert_msgpack(&msg, "82a474797065b0417574686f7269747952657175657374a46461746181b06265636f6d655f617574686f72697479c3");
}

#[test]
fn golden_client_player_ready() {
    let msg = ClientMessage::PlayerReady;
    assert_json(
        &msg,
        json!({ "type": "PlayerReady" }),
        r#"{"type":"PlayerReady"}"#,
    );
    assert_msgpack(&msg, "81a474797065ab506c617965725265616479");
}

#[test]
fn golden_client_provide_connection_info() {
    let msg = ClientMessage::ProvideConnectionInfo {
        connection_info: ConnectionInfo::Direct {
            host: "192.168.1.2".to_string(),
            port: 7777,
        },
    };
    assert_json(
        &msg,
        json!({
            "type": "ProvideConnectionInfo",
            "data": {
                "connection_info": { "type": "direct", "host": "192.168.1.2", "port": 7777 }
            }
        }),
        r#"{"type":"ProvideConnectionInfo","data":{"connection_info":{"type":"direct","host":"192.168.1.2","port":7777}}}"#,
    );
    assert_msgpack(&msg, "82a474797065b550726f76696465436f6e6e656374696f6e496e666fa46461746181af636f6e6e656374696f6e5f696e666f83a474797065a6646972656374a4686f7374ab3139322e3136382e312e32a4706f7274cd1e61");
}

#[test]
fn golden_client_ping() {
    let msg = ClientMessage::Ping;
    assert_json(&msg, json!({ "type": "Ping" }), r#"{"type":"Ping"}"#);
    assert_msgpack(&msg, "81a474797065a450696e67");
}

#[test]
fn golden_client_reconnect() {
    let msg = ClientMessage::Reconnect {
        player_id: player_a(),
        room_id: room(),
        auth_token: "tok-123".to_string(),
    };
    assert_json(
        &msg,
        json!({
            "type": "Reconnect",
            "data": {
                "player_id": PLAYER_A_STR,
                "room_id": ROOM_STR,
                "auth_token": "tok-123"
            }
        }),
        &format!(
            r#"{{"type":"Reconnect","data":{{"player_id":"{PLAYER_A_STR}","room_id":"{ROOM_STR}","auth_token":"tok-123"}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065a95265636f6e6e656374a46461746183a9706c617965725f6964c4100000000000000000000000000000000aa7726f6f6d5f6964c41011111111111111111111111111111111aa617574685f746f6b656ea7746f6b2d313233");
}

#[test]
fn golden_client_join_as_spectator() {
    let msg = ClientMessage::JoinAsSpectator {
        game_name: "test_game".to_string(),
        room_code: "ABC123".to_string(),
        spectator_name: "Watcher".to_string(),
    };
    assert_json(
        &msg,
        json!({
            "type": "JoinAsSpectator",
            "data": {
                "game_name": "test_game",
                "room_code": "ABC123",
                "spectator_name": "Watcher"
            }
        }),
        r#"{"type":"JoinAsSpectator","data":{"game_name":"test_game","room_code":"ABC123","spectator_name":"Watcher"}}"#,
    );
    assert_msgpack(&msg, "82a474797065af4a6f696e4173537065637461746f72a46461746183a967616d655f6e616d65a9746573745f67616d65a9726f6f6d5f636f6465a6414243313233ae737065637461746f725f6e616d65a757617463686572");
}

#[test]
fn golden_client_leave_spectator() {
    let msg = ClientMessage::LeaveSpectator;
    assert_json(
        &msg,
        json!({ "type": "LeaveSpectator" }),
        r#"{"type":"LeaveSpectator"}"#,
    );
    assert_msgpack(&msg, "81a474797065ae4c65617665537065637461746f72");
}

// ===========================================================================
// ServerMessage golden snapshots
// ===========================================================================

#[test]
fn golden_server_authenticated() {
    let msg = ServerMessage::Authenticated {
        app_name: "Test App".to_string(),
        organization: Some("Ambiguous Interactive".to_string()),
        rate_limits: rate_limits(),
    };
    assert_json(
        &msg,
        json!({
            "type": "Authenticated",
            "data": {
                "app_name": "Test App",
                "organization": "Ambiguous Interactive",
                "rate_limits": { "per_minute": 60, "per_hour": 3600, "per_day": 86400 }
            }
        }),
        r#"{"type":"Authenticated","data":{"app_name":"Test App","organization":"Ambiguous Interactive","rate_limits":{"per_minute":60,"per_hour":3600,"per_day":86400}}}"#,
    );
    assert_msgpack(&msg, "82a474797065ad41757468656e74696361746564a46461746183a86170705f6e616d65a85465737420417070ac6f7267616e697a6174696f6eb5416d626967756f757320496e746572616374697665ab726174655f6c696d69747383aa7065725f6d696e7574653ca87065725f686f7572cd0e10a77065725f646179ce00015180");
}

#[test]
fn golden_server_protocol_info() {
    let msg = ServerMessage::ProtocolInfo(ProtocolInfoPayload {
        platform: Some("godot".to_string()),
        sdk_version: Some("1.2.3".to_string()),
        minimum_version: Some("1.0.0".to_string()),
        recommended_version: Some("1.2.3".to_string()),
        capabilities: vec!["reconnection".to_string()],
        notes: Some("note".to_string()),
        game_data_formats: vec![GameDataEncoding::Json, GameDataEncoding::MessagePack],
        player_name_rules: None,
        protocol_version: None,
        min_protocol_version: None,
        max_protocol_version: None,
    });
    assert_json(
        &msg,
        json!({
            "type": "ProtocolInfo",
            "data": {
                "platform": "godot",
                "sdk_version": "1.2.3",
                "minimum_version": "1.0.0",
                "recommended_version": "1.2.3",
                "capabilities": ["reconnection"],
                "notes": "note",
                "game_data_formats": ["json", "message_pack"]
            }
        }),
        r#"{"type":"ProtocolInfo","data":{"platform":"godot","sdk_version":"1.2.3","minimum_version":"1.0.0","recommended_version":"1.2.3","capabilities":["reconnection"],"notes":"note","game_data_formats":["json","message_pack"]}}"#,
    );
    assert_msgpack(&msg, "82a474797065ac50726f746f636f6c496e666fa46461746187a8706c6174666f726da5676f646f74ab73646b5f76657273696f6ea5312e322e33af6d696e696d756d5f76657273696f6ea5312e302e30b37265636f6d6d656e6465645f76657273696f6ea5312e322e33ac6361706162696c697469657391ac7265636f6e6e656374696f6ea56e6f746573a46e6f7465b167616d655f646174615f666f726d61747392a46a736f6eac6d6573736167655f7061636b");
}

#[test]
fn golden_server_authentication_error() {
    let msg = ServerMessage::AuthenticationError {
        error: "bad app id".to_string(),
        error_code: ErrorCode::InvalidAppId,
    };
    assert_json(
        &msg,
        json!({
            "type": "AuthenticationError",
            "data": { "error": "bad app id", "error_code": "INVALID_APP_ID" }
        }),
        r#"{"type":"AuthenticationError","data":{"error":"bad app id","error_code":"INVALID_APP_ID"}}"#,
    );
    assert_msgpack(&msg, "82a474797065b341757468656e7469636174696f6e4572726f72a46461746182a56572726f72aa62616420617070206964aa6572726f725f636f6465ae494e56414c49445f4150505f4944");
}

#[test]
fn golden_server_room_joined() {
    let msg = ServerMessage::RoomJoined(Box::new(RoomJoinedPayload {
        room_id: room(),
        room_code: "ABC123".to_string(),
        player_id: player_a(),
        game_name: "test_game".to_string(),
        max_players: 4,
        supports_authority: true,
        current_players: vec![player_info_a()],
        is_authority: true,
        lobby_state: LobbyState::Waiting,
        ready_players: vec![player_a()],
        relay_type: "matchbox".to_string(),
        current_spectators: vec![spectator()],
    }));
    assert_json(
        &msg,
        json!({
            "type": "RoomJoined",
            "data": {
                "room_id": ROOM_STR,
                "room_code": "ABC123",
                "player_id": PLAYER_A_STR,
                "game_name": "test_game",
                "max_players": 4,
                "supports_authority": true,
                "current_players": [{
                    "id": PLAYER_A_STR,
                    "name": "Alice",
                    "is_authority": true,
                    "is_ready": false,
                    "connected_at": TIME_STR
                }],
                "is_authority": true,
                "lobby_state": "waiting",
                "ready_players": [PLAYER_A_STR],
                "relay_type": "matchbox",
                "current_spectators": [{
                    "id": PLAYER_B_STR,
                    "name": "Watcher",
                    "connected_at": TIME_STR
                }]
            }
        }),
        &format!(
            r#"{{"type":"RoomJoined","data":{{"room_id":"{ROOM_STR}","room_code":"ABC123","player_id":"{PLAYER_A_STR}","game_name":"test_game","max_players":4,"supports_authority":true,"current_players":[{{"id":"{PLAYER_A_STR}","name":"Alice","is_authority":true,"is_ready":false,"connected_at":"{TIME_STR}"}}],"is_authority":true,"lobby_state":"waiting","ready_players":["{PLAYER_A_STR}"],"relay_type":"matchbox","current_spectators":[{{"id":"{PLAYER_B_STR}","name":"Watcher","connected_at":"{TIME_STR}"}}]}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065aa526f6f6d4a6f696e6564a4646174618ca7726f6f6d5f6964c41011111111111111111111111111111111a9726f6f6d5f636f6465a6414243313233a9706c617965725f6964c4100000000000000000000000000000000aa967616d655f6e616d65a9746573745f67616d65ab6d61785f706c617965727304b2737570706f7274735f617574686f72697479c3af63757272656e745f706c61796572739185a26964c4100000000000000000000000000000000aa46e616d65a5416c696365ac69735f617574686f72697479c3a869735f7265616479c2ac636f6e6e65637465645f6174b4323032342d30312d30325430333a30343a30355aac69735f617574686f72697479c3ab6c6f6262795f7374617465a777616974696e67ad72656164795f706c617965727391c4100000000000000000000000000000000aaa72656c61795f74797065a86d61746368626f78b263757272656e745f737065637461746f72739183a26964c4100000000000000000000000000000000ba46e616d65a757617463686572ac636f6e6e65637465645f6174b4323032342d30312d30325430333a30343a30355a");
}

#[test]
fn golden_server_room_join_failed() {
    let msg = ServerMessage::RoomJoinFailed {
        reason: "room full".to_string(),
        error_code: Some(ErrorCode::RoomFull),
    };
    assert_json(
        &msg,
        json!({
            "type": "RoomJoinFailed",
            "data": { "reason": "room full", "error_code": "ROOM_FULL" }
        }),
        r#"{"type":"RoomJoinFailed","data":{"reason":"room full","error_code":"ROOM_FULL"}}"#,
    );
    assert_msgpack(&msg, "82a474797065ae526f6f6d4a6f696e4661696c6564a46461746182a6726561736f6ea9726f6f6d2066756c6caa6572726f725f636f6465a9524f4f4d5f46554c4c");
}

#[test]
fn golden_server_room_left() {
    let msg = ServerMessage::RoomLeft;
    assert_json(
        &msg,
        json!({ "type": "RoomLeft" }),
        r#"{"type":"RoomLeft"}"#,
    );
    assert_msgpack(&msg, "81a474797065a8526f6f6d4c656674");
}

#[test]
fn golden_server_player_joined() {
    let msg = ServerMessage::PlayerJoined {
        player: player_info_b(),
    };
    assert_json(
        &msg,
        json!({
            "type": "PlayerJoined",
            "data": {
                "player": {
                    "id": PLAYER_B_STR,
                    "name": "Bob",
                    "is_authority": false,
                    "is_ready": true,
                    "connected_at": TIME_STR,
                    "connection_info": { "type": "direct", "host": "10.0.0.5", "port": 7777 }
                }
            }
        }),
        &format!(
            r#"{{"type":"PlayerJoined","data":{{"player":{{"id":"{PLAYER_B_STR}","name":"Bob","is_authority":false,"is_ready":true,"connected_at":"{TIME_STR}","connection_info":{{"type":"direct","host":"10.0.0.5","port":7777}}}}}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065ac506c617965724a6f696e6564a46461746181a6706c6179657286a26964c4100000000000000000000000000000000ba46e616d65a3426f62ac69735f617574686f72697479c2a869735f7265616479c3ac636f6e6e65637465645f6174b4323032342d30312d30325430333a30343a30355aaf636f6e6e656374696f6e5f696e666f83a474797065a6646972656374a4686f7374a831302e302e302e35a4706f7274cd1e61");
}

#[test]
fn golden_server_player_left() {
    let msg = ServerMessage::PlayerLeft {
        player_id: player_a(),
    };
    assert_json(
        &msg,
        json!({ "type": "PlayerLeft", "data": { "player_id": PLAYER_A_STR } }),
        &format!(r#"{{"type":"PlayerLeft","data":{{"player_id":"{PLAYER_A_STR}"}}}}"#),
    );
    assert_msgpack(&msg, "82a474797065aa506c617965724c656674a46461746181a9706c617965725f6964c4100000000000000000000000000000000a");
}

#[test]
fn golden_server_game_data() {
    let msg = ServerMessage::GameData {
        from_player: player_a(),
        data: json!({ "move": "up" }),
    };
    assert_json(
        &msg,
        json!({
            "type": "GameData",
            "data": { "from_player": PLAYER_A_STR, "data": { "move": "up" } }
        }),
        &format!(
            r#"{{"type":"GameData","data":{{"from_player":"{PLAYER_A_STR}","data":{{"move":"up"}}}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065a847616d6544617461a46461746182ab66726f6d5f706c61796572c4100000000000000000000000000000000aa46461746181a46d6f7665a27570");
}

/// GameDataBinary is special: it does NOT travel through the `{type,data}`
/// envelope on the wire. `src/websocket/sending.rs::send_single_message`
/// intercepts `ServerMessage::GameDataBinary` and, for MessagePack-capable
/// clients, serializes a BARE `BinaryGameDataFrame { from_player, encoding,
/// payload }` via `rmp_serde::to_vec_named` (no `type`/`data` wrapper). For
/// binary-incapable (JSON/text) clients it decodes the payload and emits a
/// `ServerMessage::GameData` text frame instead. This test freezes BOTH real
/// production wire forms, by calling the exact production encoder.
#[test]
fn golden_server_game_data_binary_wire_frame() {
    let payload: &[u8] = &[0x01, 0x02, 0x03, 0x04];

    // (1) Binary wire form for MessagePack-capable clients: the BARE frame
    // bytes produced by the production encoder. Note there is NO leading
    // `82a474797065ae47616d654461746142696e617279...` enum envelope here.
    let wire = encode_binary_game_data(player_a(), GameDataEncoding::MessagePack, payload)
        .expect("production binary encode");
    assert_eq!(
        hex(&wire),
        "83ab66726f6d5f706c61796572c4100000000000000000000000000000000aa8656e636f64696e67ac6d6573736167655f7061636ba77061796c6f6164c40401020304",
        "binary wire frame drift (BREAKING v2 wire change?)"
    );

    // The same bytes, asserted against the bare struct serialized exactly as
    // production does it, to lock the field-name/key surface of the frame.
    let frame = BinaryGameDataFrame {
        from_player: player_a(),
        encoding: GameDataEncoding::MessagePack,
        payload,
    };
    assert_msgpack(
        &frame,
        "83ab66726f6d5f706c61796572c4100000000000000000000000000000000aa8656e636f64696e67ac6d6573736167655f7061636ba77061796c6f6164c40401020304",
    );
    // The bare frame's JSON shape (serde_bytes -> array). This is the on-wire
    // struct, NOT an enum envelope.
    assert_json(
        &frame,
        json!({
            "from_player": PLAYER_A_STR,
            "encoding": "message_pack",
            "payload": [1, 2, 3, 4]
        }),
        &format!(
            r#"{{"from_player":"{PLAYER_A_STR}","encoding":"message_pack","payload":[1,2,3,4]}}"#
        ),
    );
}

/// JSON-fallback wire form: for binary-incapable clients, production decodes the
/// payload and emits a `ServerMessage::GameData` text frame (the standard
/// enveloped form). We freeze that exact fallback frame here.
#[test]
fn golden_server_game_data_binary_json_fallback() {
    // Production's `send_binary_fallback` produces `ServerMessage::GameData`
    // with `data` = the decoded payload. Using a representative decoded value.
    let fallback = ServerMessage::GameData {
        from_player: player_a(),
        data: json!({ "move": "up" }),
    };
    assert_json(
        &fallback,
        json!({
            "type": "GameData",
            "data": { "from_player": PLAYER_A_STR, "data": { "move": "up" } }
        }),
        &format!(
            r#"{{"type":"GameData","data":{{"from_player":"{PLAYER_A_STR}","data":{{"move":"up"}}}}}}"#
        ),
    );
}

/// In-memory representation ONLY — NOT the wire form. The `ServerMessage::
/// GameDataBinary` enum variant is never serialized to the wire as-is (see
/// `golden_server_game_data_binary_wire_frame`). Frozen purely to detect
/// accidental changes to the in-memory enum's serde shape, e.g. if someone
/// removes the interception in `send_single_message`.
#[test]
fn golden_server_game_data_binary_in_memory_repr_not_wire() {
    let msg = ServerMessage::GameDataBinary {
        from_player: player_a(),
        encoding: GameDataEncoding::MessagePack,
        payload: Bytes::from_static(&[0x01, 0x02, 0x03, 0x04]),
    };
    assert_json(
        &msg,
        json!({
            "type": "GameDataBinary",
            "data": {
                "from_player": PLAYER_A_STR,
                "encoding": "message_pack",
                "payload": [1, 2, 3, 4]
            }
        }),
        &format!(
            r#"{{"type":"GameDataBinary","data":{{"from_player":"{PLAYER_A_STR}","encoding":"message_pack","payload":[1,2,3,4]}}}}"#
        ),
    );
}

#[test]
fn golden_server_authority_changed() {
    let msg = ServerMessage::AuthorityChanged {
        authority_player: Some(player_a()),
        you_are_authority: true,
    };
    assert_json(
        &msg,
        json!({
            "type": "AuthorityChanged",
            "data": { "authority_player": PLAYER_A_STR, "you_are_authority": true }
        }),
        &format!(
            r#"{{"type":"AuthorityChanged","data":{{"authority_player":"{PLAYER_A_STR}","you_are_authority":true}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065b0417574686f726974794368616e676564a46461746182b0617574686f726974795f706c61796572c4100000000000000000000000000000000ab1796f755f6172655f617574686f72697479c3");
}

#[test]
fn golden_server_authority_response() {
    let msg = ServerMessage::AuthorityResponse {
        granted: false,
        reason: Some("conflict".to_string()),
        error_code: Some(ErrorCode::AuthorityConflict),
    };
    assert_json(
        &msg,
        json!({
            "type": "AuthorityResponse",
            "data": { "granted": false, "reason": "conflict", "error_code": "AUTHORITY_CONFLICT" }
        }),
        r#"{"type":"AuthorityResponse","data":{"granted":false,"reason":"conflict","error_code":"AUTHORITY_CONFLICT"}}"#,
    );
    assert_msgpack(&msg, "82a474797065b1417574686f72697479526573706f6e7365a46461746183a76772616e746564c2a6726561736f6ea8636f6e666c696374aa6572726f725f636f6465b2415554484f524954595f434f4e464c494354");
}

#[test]
fn golden_server_lobby_state_changed() {
    let msg = ServerMessage::LobbyStateChanged {
        lobby_state: LobbyState::Lobby,
        ready_players: vec![player_a(), player_b()],
        all_ready: false,
    };
    assert_json(
        &msg,
        json!({
            "type": "LobbyStateChanged",
            "data": {
                "lobby_state": "lobby",
                "ready_players": [PLAYER_A_STR, PLAYER_B_STR],
                "all_ready": false
            }
        }),
        &format!(
            r#"{{"type":"LobbyStateChanged","data":{{"lobby_state":"lobby","ready_players":["{PLAYER_A_STR}","{PLAYER_B_STR}"],"all_ready":false}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065b14c6f62627953746174654368616e676564a46461746183ab6c6f6262795f7374617465a56c6f626279ad72656164795f706c617965727392c4100000000000000000000000000000000ac4100000000000000000000000000000000ba9616c6c5f7265616479c2");
}

#[test]
fn golden_server_game_starting() {
    let msg = ServerMessage::GameStarting {
        peer_connections: vec![PeerConnectionInfo {
            player_id: player_a(),
            player_name: "Alice".to_string(),
            is_authority: true,
            relay_type: "matchbox".to_string(),
            connection_info: None,
        }],
    };
    assert_json(
        &msg,
        json!({
            "type": "GameStarting",
            "data": {
                "peer_connections": [{
                    "player_id": PLAYER_A_STR,
                    "player_name": "Alice",
                    "is_authority": true,
                    "relay_type": "matchbox"
                }]
            }
        }),
        &format!(
            r#"{{"type":"GameStarting","data":{{"peer_connections":[{{"player_id":"{PLAYER_A_STR}","player_name":"Alice","is_authority":true,"relay_type":"matchbox"}}]}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065ac47616d655374617274696e67a46461746181b0706565725f636f6e6e656374696f6e739184a9706c617965725f6964c4100000000000000000000000000000000aab706c617965725f6e616d65a5416c696365ac69735f617574686f72697479c3aa72656c61795f74797065a86d61746368626f78");
}

#[test]
fn golden_server_pong() {
    let msg = ServerMessage::Pong;
    assert_json(&msg, json!({ "type": "Pong" }), r#"{"type":"Pong"}"#);
    assert_msgpack(&msg, "81a474797065a4506f6e67");
}

#[test]
fn golden_server_reconnected() {
    let msg = ServerMessage::Reconnected(Box::new(ReconnectedPayload {
        room_id: room(),
        room_code: "ABC123".to_string(),
        player_id: player_a(),
        game_name: "test_game".to_string(),
        max_players: 4,
        supports_authority: true,
        current_players: vec![player_info_a()],
        is_authority: true,
        lobby_state: LobbyState::Lobby,
        ready_players: vec![player_a()],
        relay_type: "matchbox".to_string(),
        current_spectators: vec![],
        missed_events: vec![ServerMessage::Pong],
    }));
    assert_json(
        &msg,
        json!({
            "type": "Reconnected",
            "data": {
                "room_id": ROOM_STR,
                "room_code": "ABC123",
                "player_id": PLAYER_A_STR,
                "game_name": "test_game",
                "max_players": 4,
                "supports_authority": true,
                "current_players": [{
                    "id": PLAYER_A_STR,
                    "name": "Alice",
                    "is_authority": true,
                    "is_ready": false,
                    "connected_at": TIME_STR
                }],
                "is_authority": true,
                "lobby_state": "lobby",
                "ready_players": [PLAYER_A_STR],
                "relay_type": "matchbox",
                "current_spectators": [],
                "missed_events": [{ "type": "Pong" }]
            }
        }),
        &format!(
            r#"{{"type":"Reconnected","data":{{"room_id":"{ROOM_STR}","room_code":"ABC123","player_id":"{PLAYER_A_STR}","game_name":"test_game","max_players":4,"supports_authority":true,"current_players":[{{"id":"{PLAYER_A_STR}","name":"Alice","is_authority":true,"is_ready":false,"connected_at":"{TIME_STR}"}}],"is_authority":true,"lobby_state":"lobby","ready_players":["{PLAYER_A_STR}"],"relay_type":"matchbox","current_spectators":[],"missed_events":[{{"type":"Pong"}}]}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065ab5265636f6e6e6563746564a4646174618da7726f6f6d5f6964c41011111111111111111111111111111111a9726f6f6d5f636f6465a6414243313233a9706c617965725f6964c4100000000000000000000000000000000aa967616d655f6e616d65a9746573745f67616d65ab6d61785f706c617965727304b2737570706f7274735f617574686f72697479c3af63757272656e745f706c61796572739185a26964c4100000000000000000000000000000000aa46e616d65a5416c696365ac69735f617574686f72697479c3a869735f7265616479c2ac636f6e6e65637465645f6174b4323032342d30312d30325430333a30343a30355aac69735f617574686f72697479c3ab6c6f6262795f7374617465a56c6f626279ad72656164795f706c617965727391c4100000000000000000000000000000000aaa72656c61795f74797065a86d61746368626f78b263757272656e745f737065637461746f727390ad6d69737365645f6576656e74739181a474797065a4506f6e67");
}

#[test]
fn golden_server_reconnection_failed() {
    let msg = ServerMessage::ReconnectionFailed {
        reason: "expired".to_string(),
        error_code: ErrorCode::ReconnectionExpired,
    };
    assert_json(
        &msg,
        json!({
            "type": "ReconnectionFailed",
            "data": { "reason": "expired", "error_code": "RECONNECTION_EXPIRED" }
        }),
        r#"{"type":"ReconnectionFailed","data":{"reason":"expired","error_code":"RECONNECTION_EXPIRED"}}"#,
    );
    assert_msgpack(&msg, "82a474797065b25265636f6e6e656374696f6e4661696c6564a46461746182a6726561736f6ea765787069726564aa6572726f725f636f6465b45245434f4e4e454354494f4e5f45585049524544");
}

#[test]
fn golden_server_player_reconnected() {
    let msg = ServerMessage::PlayerReconnected {
        player_id: player_a(),
    };
    assert_json(
        &msg,
        json!({ "type": "PlayerReconnected", "data": { "player_id": PLAYER_A_STR } }),
        &format!(r#"{{"type":"PlayerReconnected","data":{{"player_id":"{PLAYER_A_STR}"}}}}"#),
    );
    assert_msgpack(&msg, "82a474797065b1506c617965725265636f6e6e6563746564a46461746181a9706c617965725f6964c4100000000000000000000000000000000a");
}

#[test]
fn golden_server_spectator_joined() {
    let msg = ServerMessage::SpectatorJoined(Box::new(SpectatorJoinedPayload {
        room_id: room(),
        room_code: "ABC123".to_string(),
        spectator_id: player_b(),
        game_name: "test_game".to_string(),
        current_players: vec![player_info_a()],
        current_spectators: vec![spectator()],
        lobby_state: LobbyState::Waiting,
        reason: Some(SpectatorStateChangeReason::Joined),
    }));
    assert_json(
        &msg,
        json!({
            "type": "SpectatorJoined",
            "data": {
                "room_id": ROOM_STR,
                "room_code": "ABC123",
                "spectator_id": PLAYER_B_STR,
                "game_name": "test_game",
                "current_players": [{
                    "id": PLAYER_A_STR,
                    "name": "Alice",
                    "is_authority": true,
                    "is_ready": false,
                    "connected_at": TIME_STR
                }],
                "current_spectators": [{
                    "id": PLAYER_B_STR,
                    "name": "Watcher",
                    "connected_at": TIME_STR
                }],
                "lobby_state": "waiting",
                "reason": "joined"
            }
        }),
        &format!(
            r#"{{"type":"SpectatorJoined","data":{{"room_id":"{ROOM_STR}","room_code":"ABC123","spectator_id":"{PLAYER_B_STR}","game_name":"test_game","current_players":[{{"id":"{PLAYER_A_STR}","name":"Alice","is_authority":true,"is_ready":false,"connected_at":"{TIME_STR}"}}],"current_spectators":[{{"id":"{PLAYER_B_STR}","name":"Watcher","connected_at":"{TIME_STR}"}}],"lobby_state":"waiting","reason":"joined"}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065af537065637461746f724a6f696e6564a46461746188a7726f6f6d5f6964c41011111111111111111111111111111111a9726f6f6d5f636f6465a6414243313233ac737065637461746f725f6964c4100000000000000000000000000000000ba967616d655f6e616d65a9746573745f67616d65af63757272656e745f706c61796572739185a26964c4100000000000000000000000000000000aa46e616d65a5416c696365ac69735f617574686f72697479c3a869735f7265616479c2ac636f6e6e65637465645f6174b4323032342d30312d30325430333a30343a30355ab263757272656e745f737065637461746f72739183a26964c4100000000000000000000000000000000ba46e616d65a757617463686572ac636f6e6e65637465645f6174b4323032342d30312d30325430333a30343a30355aab6c6f6262795f7374617465a777616974696e67a6726561736f6ea66a6f696e6564");
}

#[test]
fn golden_server_spectator_join_failed() {
    let msg = ServerMessage::SpectatorJoinFailed {
        reason: "not allowed".to_string(),
        error_code: Some(ErrorCode::SpectatorNotAllowed),
    };
    assert_json(
        &msg,
        json!({
            "type": "SpectatorJoinFailed",
            "data": { "reason": "not allowed", "error_code": "SPECTATOR_NOT_ALLOWED" }
        }),
        r#"{"type":"SpectatorJoinFailed","data":{"reason":"not allowed","error_code":"SPECTATOR_NOT_ALLOWED"}}"#,
    );
    assert_msgpack(&msg, "82a474797065b3537065637461746f724a6f696e4661696c6564a46461746182a6726561736f6eab6e6f7420616c6c6f776564aa6572726f725f636f6465b5535045435441544f525f4e4f545f414c4c4f574544");
}

#[test]
fn golden_server_spectator_left() {
    let msg = ServerMessage::SpectatorLeft {
        room_id: Some(room()),
        room_code: Some("ABC123".to_string()),
        reason: Some(SpectatorStateChangeReason::VoluntaryLeave),
        current_spectators: vec![],
    };
    assert_json(
        &msg,
        json!({
            "type": "SpectatorLeft",
            "data": {
                "room_id": ROOM_STR,
                "room_code": "ABC123",
                "reason": "voluntary_leave",
                "current_spectators": []
            }
        }),
        &format!(
            r#"{{"type":"SpectatorLeft","data":{{"room_id":"{ROOM_STR}","room_code":"ABC123","reason":"voluntary_leave","current_spectators":[]}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065ad537065637461746f724c656674a46461746184a7726f6f6d5f6964c41011111111111111111111111111111111a9726f6f6d5f636f6465a6414243313233a6726561736f6eaf766f6c756e746172795f6c65617665b263757272656e745f737065637461746f727390");
}

#[test]
fn golden_server_new_spectator_joined() {
    let msg = ServerMessage::NewSpectatorJoined {
        spectator: spectator(),
        current_spectators: vec![spectator()],
        reason: Some(SpectatorStateChangeReason::Joined),
    };
    assert_json(
        &msg,
        json!({
            "type": "NewSpectatorJoined",
            "data": {
                "spectator": { "id": PLAYER_B_STR, "name": "Watcher", "connected_at": TIME_STR },
                "current_spectators": [{ "id": PLAYER_B_STR, "name": "Watcher", "connected_at": TIME_STR }],
                "reason": "joined"
            }
        }),
        &format!(
            r#"{{"type":"NewSpectatorJoined","data":{{"spectator":{{"id":"{PLAYER_B_STR}","name":"Watcher","connected_at":"{TIME_STR}"}},"current_spectators":[{{"id":"{PLAYER_B_STR}","name":"Watcher","connected_at":"{TIME_STR}"}}],"reason":"joined"}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065b24e6577537065637461746f724a6f696e6564a46461746183a9737065637461746f7283a26964c4100000000000000000000000000000000ba46e616d65a757617463686572ac636f6e6e65637465645f6174b4323032342d30312d30325430333a30343a30355ab263757272656e745f737065637461746f72739183a26964c4100000000000000000000000000000000ba46e616d65a757617463686572ac636f6e6e65637465645f6174b4323032342d30312d30325430333a30343a30355aa6726561736f6ea66a6f696e6564");
}

#[test]
fn golden_server_spectator_disconnected() {
    let msg = ServerMessage::SpectatorDisconnected {
        spectator_id: player_b(),
        reason: Some(SpectatorStateChangeReason::Disconnected),
        current_spectators: vec![],
    };
    assert_json(
        &msg,
        json!({
            "type": "SpectatorDisconnected",
            "data": {
                "spectator_id": PLAYER_B_STR,
                "reason": "disconnected",
                "current_spectators": []
            }
        }),
        &format!(
            r#"{{"type":"SpectatorDisconnected","data":{{"spectator_id":"{PLAYER_B_STR}","reason":"disconnected","current_spectators":[]}}}}"#
        ),
    );
    assert_msgpack(&msg, "82a474797065b5537065637461746f72446973636f6e6e6563746564a46461746183ac737065637461746f725f6964c4100000000000000000000000000000000ba6726561736f6eac646973636f6e6e6563746564b263757272656e745f737065637461746f727390");
}

#[test]
fn golden_server_error() {
    let msg = ServerMessage::Error {
        message: "boom".to_string(),
        error_code: Some(ErrorCode::InternalError),
    };
    assert_json(
        &msg,
        json!({
            "type": "Error",
            "data": { "message": "boom", "error_code": "INTERNAL_ERROR" }
        }),
        r#"{"type":"Error","data":{"message":"boom","error_code":"INTERNAL_ERROR"}}"#,
    );
    assert_msgpack(&msg, "82a474797065a54572726f72a46461746182a76d657373616765a4626f6f6daa6572726f725f636f6465ae494e5445524e414c5f4552524f52");
}

// ===========================================================================
// Enum variant casing snapshots — locks the `rename_all` / `rename` surface.
// Exhaustive over every variant so a casing change anywhere is a hard failure.
// ===========================================================================

/// Helper: assert a value serializes to the exact JSON string `expected`.
fn assert_json_str<T: Serialize>(value: &T, expected: &str) {
    let actual = serde_json::to_string(value).expect("json string");
    assert_eq!(
        actual, expected,
        "enum JSON casing drift (BREAKING v2 wire change?)"
    );
}

#[test]
fn golden_enum_relay_transport_all_variants() {
    // `#[serde(rename_all = "lowercase")]`
    assert_json_str(&RelayTransport::Tcp, r#""tcp""#);
    assert_json_str(&RelayTransport::Udp, r#""udp""#);
    assert_json_str(&RelayTransport::Websocket, r#""websocket""#);
    assert_json_str(&RelayTransport::Auto, r#""auto""#);
}

#[test]
fn golden_enum_game_data_encoding_all_variants() {
    // `#[serde(rename_all = "snake_case")]` with explicit renames.
    assert_json_str(&GameDataEncoding::Json, r#""json""#);
    assert_json_str(&GameDataEncoding::MessagePack, r#""message_pack""#);
    assert_json_str(&GameDataEncoding::Rkyv, r#""rkyv""#);
}

#[test]
fn golden_enum_lobby_state_all_variants() {
    // `#[serde(rename_all = "snake_case")]`
    assert_json_str(&LobbyState::Waiting, r#""waiting""#);
    assert_json_str(&LobbyState::Lobby, r#""lobby""#);
    assert_json_str(&LobbyState::Finalized, r#""finalized""#);
}

#[test]
fn golden_enum_spectator_state_change_reason_all_variants() {
    // `#[serde(rename_all = "snake_case")]`
    assert_json_str(&SpectatorStateChangeReason::Joined, r#""joined""#);
    assert_json_str(
        &SpectatorStateChangeReason::VoluntaryLeave,
        r#""voluntary_leave""#,
    );
    assert_json_str(
        &SpectatorStateChangeReason::Disconnected,
        r#""disconnected""#,
    );
    assert_json_str(&SpectatorStateChangeReason::Removed, r#""removed""#);
    assert_json_str(&SpectatorStateChangeReason::RoomClosed, r#""room_closed""#);
}

#[test]
fn golden_enum_error_code_all_variants() {
    // `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]` — exhaustive.
    let cases: &[(ErrorCode, &str)] = &[
        (ErrorCode::Unauthorized, r#""UNAUTHORIZED""#),
        (ErrorCode::InvalidToken, r#""INVALID_TOKEN""#),
        (
            ErrorCode::AuthenticationRequired,
            r#""AUTHENTICATION_REQUIRED""#,
        ),
        (ErrorCode::InvalidAppId, r#""INVALID_APP_ID""#),
        (ErrorCode::AppIdExpired, r#""APP_ID_EXPIRED""#),
        (ErrorCode::AppIdRevoked, r#""APP_ID_REVOKED""#),
        (ErrorCode::AppIdSuspended, r#""APP_ID_SUSPENDED""#),
        (ErrorCode::MissingAppId, r#""MISSING_APP_ID""#),
        (
            ErrorCode::AuthenticationTimeout,
            r#""AUTHENTICATION_TIMEOUT""#,
        ),
        (
            ErrorCode::SdkVersionUnsupported,
            r#""SDK_VERSION_UNSUPPORTED""#,
        ),
        (
            ErrorCode::UnsupportedGameDataFormat,
            r#""UNSUPPORTED_GAME_DATA_FORMAT""#,
        ),
        (ErrorCode::InvalidInput, r#""INVALID_INPUT""#),
        (ErrorCode::InvalidGameName, r#""INVALID_GAME_NAME""#),
        (ErrorCode::InvalidRoomCode, r#""INVALID_ROOM_CODE""#),
        (ErrorCode::InvalidPlayerName, r#""INVALID_PLAYER_NAME""#),
        (ErrorCode::InvalidMaxPlayers, r#""INVALID_MAX_PLAYERS""#),
        (ErrorCode::MessageTooLarge, r#""MESSAGE_TOO_LARGE""#),
        (ErrorCode::RoomNotFound, r#""ROOM_NOT_FOUND""#),
        (ErrorCode::RoomFull, r#""ROOM_FULL""#),
        (ErrorCode::AlreadyInRoom, r#""ALREADY_IN_ROOM""#),
        (ErrorCode::NotInRoom, r#""NOT_IN_ROOM""#),
        (ErrorCode::RoomCreationFailed, r#""ROOM_CREATION_FAILED""#),
        (
            ErrorCode::MaxRoomsPerGameExceeded,
            r#""MAX_ROOMS_PER_GAME_EXCEEDED""#,
        ),
        (ErrorCode::InvalidRoomState, r#""INVALID_ROOM_STATE""#),
        (
            ErrorCode::AuthorityNotSupported,
            r#""AUTHORITY_NOT_SUPPORTED""#,
        ),
        (ErrorCode::AuthorityConflict, r#""AUTHORITY_CONFLICT""#),
        (ErrorCode::AuthorityDenied, r#""AUTHORITY_DENIED""#),
        (ErrorCode::RateLimitExceeded, r#""RATE_LIMIT_EXCEEDED""#),
        (ErrorCode::TooManyConnections, r#""TOO_MANY_CONNECTIONS""#),
        (ErrorCode::ReconnectionFailed, r#""RECONNECTION_FAILED""#),
        (
            ErrorCode::ReconnectionTokenInvalid,
            r#""RECONNECTION_TOKEN_INVALID""#,
        ),
        (ErrorCode::ReconnectionExpired, r#""RECONNECTION_EXPIRED""#),
        (
            ErrorCode::PlayerAlreadyConnected,
            r#""PLAYER_ALREADY_CONNECTED""#,
        ),
        (ErrorCode::SpectatorNotAllowed, r#""SPECTATOR_NOT_ALLOWED""#),
        (ErrorCode::TooManySpectators, r#""TOO_MANY_SPECTATORS""#),
        (ErrorCode::NotASpectator, r#""NOT_A_SPECTATOR""#),
        (ErrorCode::SpectatorJoinFailed, r#""SPECTATOR_JOIN_FAILED""#),
        (ErrorCode::InternalError, r#""INTERNAL_ERROR""#),
        (ErrorCode::StorageError, r#""STORAGE_ERROR""#),
        (ErrorCode::ServiceUnavailable, r#""SERVICE_UNAVAILABLE""#),
    ];
    for (code, expected) in cases {
        assert_json_str(code, expected);
    }
}

#[test]
fn golden_enum_connection_info_all_variants() {
    // Internally-tagged `#[serde(tag = "type")]` with explicit `rename`s.
    assert_json_str(
        &ConnectionInfo::Direct {
            host: "10.0.0.5".to_string(),
            port: 7777,
        },
        r#"{"type":"direct","host":"10.0.0.5","port":7777}"#,
    );
    assert_json_str(
        &ConnectionInfo::UnityRelay {
            allocation_id: "alloc".to_string(),
            connection_data: "cdata".to_string(),
            key: "k".to_string(),
        },
        r#"{"type":"unity_relay","allocation_id":"alloc","connection_data":"cdata","key":"k"}"#,
    );
    assert_json_str(
        &ConnectionInfo::Relay {
            host: "10.0.0.5".to_string(),
            port: 7777,
            transport: RelayTransport::Udp,
            allocation_id: "alloc".to_string(),
            token: "tok".to_string(),
            client_id: Some(3),
        },
        r#"{"type":"relay","host":"10.0.0.5","port":7777,"transport":"udp","allocation_id":"alloc","token":"tok","client_id":3}"#,
    );
    assert_json_str(
        &ConnectionInfo::WebRTC {
            sdp: Some("sdp".to_string()),
            ice_candidates: vec!["cand".to_string()],
        },
        r#"{"type":"webrtc","sdp":"sdp","ice_candidates":["cand"]}"#,
    );
    assert_json_str(
        &ConnectionInfo::Custom {
            data: json!({ "k": "v" }),
        },
        r#"{"type":"custom","data":{"k":"v"}}"#,
    );
}
