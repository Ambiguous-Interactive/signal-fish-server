//! Golden negotiated room-operation envelopes (issue #395).

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use signal_fish_server::protocol::{
    ClientMessage, ErrorCode, LobbyState, ReconnectedPayload, ReplayStatus, RoomJoinedPayload,
    RoomOperationRequest, RoomOperationResult, ServerMessage, SpectatorJoinedPayload,
    SpectatorStateChangeReason,
};
use uuid::Uuid;

const OPERATION_ID: Uuid = Uuid::from_u128(0xaaaa_aaaa_aaaa_aaaa_aaaa_aaaa_aaaa_aaaa);
const PLAYER_ID: Uuid = Uuid::from_u128(0xbbbb_bbbb_bbbb_bbbb_bbbb_bbbb_bbbb_bbbb);
const ROOM_ID: Uuid = Uuid::from_u128(0xcccc_cccc_cccc_cccc_cccc_cccc_cccc_cccc);
const OPERATION_ID_STR: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const PLAYER_ID_STR: &str = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
const ROOM_ID_STR: &str = "cccccccc-cccc-cccc-cccc-cccccccccccc";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn assert_json_and_dump_msgpack<T: Serialize + DeserializeOwned>(
    name: &str,
    value: &T,
    expected: Value,
    expected_msgpack: &str,
) {
    assert_eq!(serde_json::to_value(value).expect("JSON value"), expected);
    let encoded = rmp_serde::to_vec_named(value).expect("MessagePack value");
    let decoded: T = rmp_serde::from_slice(&encoded).expect("MessagePack round trip");
    assert_eq!(
        serde_json::to_value(decoded).expect("round-tripped JSON value"),
        expected,
        "{name} MessagePack shape"
    );
    assert_eq!(hex(&encoded), expected_msgpack, "{name} MessagePack bytes");
}

fn room_joined() -> RoomJoinedPayload {
    RoomJoinedPayload {
        room_id: ROOM_ID,
        room_code: "ABC123".to_string(),
        player_id: PLAYER_ID,
        game_name: "game".to_string(),
        max_players: 4,
        supports_authority: true,
        current_players: Vec::new(),
        is_authority: true,
        lobby_state: LobbyState::Lobby,
        ready_players: Vec::new(),
        relay_type: "websocket".to_string(),
        current_spectators: Vec::new(),
        ice_servers: Vec::new(),
        reconnection_token: Some("join-token".to_string()),
    }
}

fn reconnected() -> ReconnectedPayload {
    ReconnectedPayload {
        room_id: ROOM_ID,
        room_code: "ABC123".to_string(),
        player_id: PLAYER_ID,
        game_name: "game".to_string(),
        max_players: 4,
        supports_authority: true,
        current_players: Vec::new(),
        is_authority: true,
        lobby_state: LobbyState::Lobby,
        ready_players: Vec::new(),
        relay_type: "websocket".to_string(),
        current_spectators: Vec::new(),
        ice_servers: Vec::new(),
        missed_events: Vec::new(),
        replay: Some(ReplayStatus::Complete),
        sender_watermarks: Vec::new(),
        reconnection_token: Some("next-token".to_string()),
    }
}

fn spectator_joined() -> SpectatorJoinedPayload {
    SpectatorJoinedPayload {
        room_id: ROOM_ID,
        room_code: "ABC123".to_string(),
        spectator_id: PLAYER_ID,
        game_name: "game".to_string(),
        current_players: Vec::new(),
        current_spectators: Vec::new(),
        lobby_state: LobbyState::Lobby,
        reason: Some(SpectatorStateChangeReason::Joined),
    }
}

#[test]
fn correlated_client_operations_have_exact_nested_shapes() {
    let cases = [
        (
            "join_room",
            ClientMessage::RoomOperation {
                operation_id: OPERATION_ID,
                operation: Box::new(RoomOperationRequest::JoinRoom {
                    game_name: "game".to_string(),
                    room_code: Some("ABC123".to_string()),
                    player_name: "Alice".to_string(),
                    max_players: Some(4),
                    supports_authority: Some(true),
                    relay_transport: None,
                }),
            },
            json!({"type":"RoomOperation","data":{"operation_id":OPERATION_ID_STR,"operation":{"type":"JoinRoom","data":{"game_name":"game","room_code":"ABC123","player_name":"Alice","max_players":4,"supports_authority":true,"relay_transport":null}}}}),
            "82a474797065ad526f6f6d4f7065726174696f6ea46461746182ac6f7065726174696f6e5f6964c410aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa96f7065726174696f6e82a474797065a84a6f696e526f6f6da46461746186a967616d655f6e616d65a467616d65a9726f6f6d5f636f6465a6414243313233ab706c617965725f6e616d65a5416c696365ab6d61785f706c617965727304b2737570706f7274735f617574686f72697479c3af72656c61795f7472616e73706f7274c0",
        ),
        (
            "leave_room",
            ClientMessage::RoomOperation {
                operation_id: OPERATION_ID,
                operation: Box::new(RoomOperationRequest::LeaveRoom),
            },
            json!({"type":"RoomOperation","data":{"operation_id":OPERATION_ID_STR,"operation":{"type":"LeaveRoom"}}}),
            "82a474797065ad526f6f6d4f7065726174696f6ea46461746182ac6f7065726174696f6e5f6964c410aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa96f7065726174696f6e81a474797065a94c65617665526f6f6d",
        ),
        (
            "reconnect",
            ClientMessage::RoomOperation {
                operation_id: OPERATION_ID,
                operation: Box::new(RoomOperationRequest::Reconnect {
                    player_id: PLAYER_ID,
                    room_id: ROOM_ID,
                    auth_token: "token".to_string(),
                }),
            },
            json!({"type":"RoomOperation","data":{"operation_id":OPERATION_ID_STR,"operation":{"type":"Reconnect","data":{"player_id":PLAYER_ID_STR,"room_id":ROOM_ID_STR,"auth_token":"token"}}}}),
            "82a474797065ad526f6f6d4f7065726174696f6ea46461746182ac6f7065726174696f6e5f6964c410aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa96f7065726174696f6e82a474797065a95265636f6e6e656374a46461746183a9706c617965725f6964c410bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbba7726f6f6d5f6964c410ccccccccccccccccccccccccccccccccaa617574685f746f6b656ea5746f6b656e",
        ),
        (
            "join_as_spectator",
            ClientMessage::RoomOperation {
                operation_id: OPERATION_ID,
                operation: Box::new(RoomOperationRequest::JoinAsSpectator {
                    game_name: "game".to_string(),
                    room_code: "ABC123".to_string(),
                    spectator_name: "Watcher".to_string(),
                }),
            },
            json!({"type":"RoomOperation","data":{"operation_id":OPERATION_ID_STR,"operation":{"type":"JoinAsSpectator","data":{"game_name":"game","room_code":"ABC123","spectator_name":"Watcher"}}}}),
            "82a474797065ad526f6f6d4f7065726174696f6ea46461746182ac6f7065726174696f6e5f6964c410aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa96f7065726174696f6e82a474797065af4a6f696e4173537065637461746f72a46461746183a967616d655f6e616d65a467616d65a9726f6f6d5f636f6465a6414243313233ae737065637461746f725f6e616d65a757617463686572",
        ),
        (
            "leave_spectator",
            ClientMessage::RoomOperation {
                operation_id: OPERATION_ID,
                operation: Box::new(RoomOperationRequest::LeaveSpectator),
            },
            json!({"type":"RoomOperation","data":{"operation_id":OPERATION_ID_STR,"operation":{"type":"LeaveSpectator"}}}),
            "82a474797065ad526f6f6d4f7065726174696f6ea46461746182ac6f7065726174696f6e5f6964c410aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa96f7065726174696f6e81a474797065ae4c65617665537065637461746f72",
        ),
    ];

    for (name, message, expected, expected_msgpack) in cases {
        assert_json_and_dump_msgpack(name, &message, expected, expected_msgpack);
    }
}

#[test]
fn human_readable_room_operation_id_requires_canonical_text_and_echoes_exactly() {
    let canonical = format!(
        r#"{{"type":"RoomOperation","data":{{"operation_id":"{OPERATION_ID_STR}","operation":{{"type":"LeaveRoom"}}}}}}"#
    );
    let parsed: ClientMessage =
        serde_json::from_str(&canonical).expect("canonical room operation id");
    let ClientMessage::RoomOperation { operation_id, .. } = parsed else {
        panic!("expected RoomOperation");
    };
    assert_eq!(operation_id, OPERATION_ID);

    let echoed = ServerMessage::RoomOperationResult {
        operation_id,
        result: Box::new(RoomOperationResult::RoomLeft),
    };
    assert_eq!(
        serde_json::to_string(&echoed).expect("serialize canonical echo"),
        format!(
            r#"{{"type":"RoomOperationResult","data":{{"operation_id":"{OPERATION_ID_STR}","result":{{"type":"RoomLeft"}}}}}}"#
        )
    );

    let cases = [
        ("uppercase", "AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA"),
        ("compact", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        ("braced", "{aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa}"),
        ("urn", "urn:uuid:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"),
    ];
    for (description, noncanonical) in cases {
        let json = format!(
            r#"{{"type":"RoomOperation","data":{{"operation_id":"{noncanonical}","operation":{{"type":"LeaveRoom"}}}}}}"#
        );
        let error = serde_json::from_str::<ClientMessage>(&json)
            .expect_err("noncanonical room operation id must be rejected");
        assert!(
            error.to_string().contains("lowercase hyphenated UUID text"),
            "{description} input produced an unexpected error: {error}"
        );
    }
}

#[test]
fn new_room_operation_shapes_ignore_unknown_json_fields_like_serde() {
    let client: ClientMessage = serde_json::from_value(json!({
        "type": "RoomOperation",
        "future_envelope_field": true,
        "data": {
            "operation_id": OPERATION_ID_STR,
            "future_data_field": true,
            "operation": {
                "type": "JoinRoom",
                "future_operation_field": true,
                "data": {
                    "game_name": "game",
                    "room_code": "ABC123",
                    "player_name": "Alice",
                    "max_players": 4,
                    "supports_authority": true,
                    "future_operation_data_field": true
                }
            }
        }
    }))
    .expect("RoomOperation serde shape accepts additive fields");
    assert!(matches!(
        client,
        ClientMessage::RoomOperation {
            operation_id: OPERATION_ID,
            operation,
        } if matches!(operation.as_ref(), RoomOperationRequest::JoinRoom { .. })
    ));

    let server: ServerMessage = serde_json::from_value(json!({
        "type": "RoomOperationResult",
        "future_envelope_field": true,
        "data": {
            "operation_id": OPERATION_ID_STR,
            "future_data_field": true,
            "result": {
                "type": "OperationFailed",
                "future_result_field": true,
                "data": {
                    "reason": "not in room",
                    "error_code": "NOT_IN_ROOM",
                    "future_result_data_field": true
                }
            }
        }
    }))
    .expect("RoomOperationResult serde shape accepts additive fields");
    assert!(matches!(
        server,
        ServerMessage::RoomOperationResult {
            operation_id: OPERATION_ID,
            result,
        } if matches!(result.as_ref(), RoomOperationResult::OperationFailed { .. })
    ));
}

#[test]
fn correlated_server_results_have_exact_nested_shapes() {
    let room_joined = room_joined();
    let reconnected = reconnected();
    let spectator_joined = spectator_joined();
    let cases = [
        (
            "room_joined",
            RoomOperationResult::RoomJoined(Box::new(room_joined)),
            json!({"type":"RoomJoined","data":{"room_id":ROOM_ID_STR,"room_code":"ABC123","player_id":PLAYER_ID_STR,"game_name":"game","max_players":4,"supports_authority":true,"current_players":[],"is_authority":true,"lobby_state":"lobby","ready_players":[],"relay_type":"websocket","current_spectators":[],"reconnection_token":"join-token"}}),
            "82a474797065b3526f6f6d4f7065726174696f6e526573756c74a46461746182ac6f7065726174696f6e5f6964c410aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa6726573756c7482a474797065aa526f6f6d4a6f696e6564a4646174618da7726f6f6d5f6964c410cccccccccccccccccccccccccccccccca9726f6f6d5f636f6465a6414243313233a9706c617965725f6964c410bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbba967616d655f6e616d65a467616d65ab6d61785f706c617965727304b2737570706f7274735f617574686f72697479c3af63757272656e745f706c617965727390ac69735f617574686f72697479c3ab6c6f6262795f7374617465a56c6f626279ad72656164795f706c617965727390aa72656c61795f74797065a9776562736f636b6574b263757272656e745f737065637461746f727390b27265636f6e6e656374696f6e5f746f6b656eaa6a6f696e2d746f6b656e",
        ),
        (
            "room_join_failed",
            RoomOperationResult::RoomJoinFailed {
                reason: "Room full".to_string(),
                error_code: Some(ErrorCode::RoomFull),
            },
            json!({"type":"RoomJoinFailed","data":{"reason":"Room full","error_code":"ROOM_FULL"}}),
            "82a474797065b3526f6f6d4f7065726174696f6e526573756c74a46461746182ac6f7065726174696f6e5f6964c410aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa6726573756c7482a474797065ae526f6f6d4a6f696e4661696c6564a46461746182a6726561736f6ea9526f6f6d2066756c6caa6572726f725f636f6465a9524f4f4d5f46554c4c",
        ),
        ("room_left", RoomOperationResult::RoomLeft, json!({"type":"RoomLeft"}), "82a474797065b3526f6f6d4f7065726174696f6e526573756c74a46461746182ac6f7065726174696f6e5f6964c410aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa6726573756c7481a474797065a8526f6f6d4c656674"),
        (
            "reconnected",
            RoomOperationResult::Reconnected(Box::new(reconnected)),
            json!({"type":"Reconnected","data":{"room_id":ROOM_ID_STR,"room_code":"ABC123","player_id":PLAYER_ID_STR,"game_name":"game","max_players":4,"supports_authority":true,"current_players":[],"is_authority":true,"lobby_state":"lobby","ready_players":[],"relay_type":"websocket","current_spectators":[],"missed_events":[],"replay":"complete","reconnection_token":"next-token"}}),
            "82a474797065b3526f6f6d4f7065726174696f6e526573756c74a46461746182ac6f7065726174696f6e5f6964c410aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa6726573756c7482a474797065ab5265636f6e6e6563746564a4646174618fa7726f6f6d5f6964c410cccccccccccccccccccccccccccccccca9726f6f6d5f636f6465a6414243313233a9706c617965725f6964c410bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbba967616d655f6e616d65a467616d65ab6d61785f706c617965727304b2737570706f7274735f617574686f72697479c3af63757272656e745f706c617965727390ac69735f617574686f72697479c3ab6c6f6262795f7374617465a56c6f626279ad72656164795f706c617965727390aa72656c61795f74797065a9776562736f636b6574b263757272656e745f737065637461746f727390ad6d69737365645f6576656e747390a67265706c6179a8636f6d706c657465b27265636f6e6e656374696f6e5f746f6b656eaa6e6578742d746f6b656e",
        ),
        (
            "reconnection_failed",
            RoomOperationResult::ReconnectionFailed {
                reason: "Expired".to_string(),
                error_code: ErrorCode::ReconnectionExpired,
            },
            json!({"type":"ReconnectionFailed","data":{"reason":"Expired","error_code":"RECONNECTION_EXPIRED"}}),
            "82a474797065b3526f6f6d4f7065726174696f6e526573756c74a46461746182ac6f7065726174696f6e5f6964c410aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa6726573756c7482a474797065b25265636f6e6e656374696f6e4661696c6564a46461746182a6726561736f6ea745787069726564aa6572726f725f636f6465b45245434f4e4e454354494f4e5f45585049524544",
        ),
        (
            "spectator_joined",
            RoomOperationResult::SpectatorJoined(Box::new(spectator_joined)),
            json!({"type":"SpectatorJoined","data":{"room_id":ROOM_ID_STR,"room_code":"ABC123","spectator_id":PLAYER_ID_STR,"game_name":"game","current_players":[],"current_spectators":[],"lobby_state":"lobby","reason":"joined"}}),
            "82a474797065b3526f6f6d4f7065726174696f6e526573756c74a46461746182ac6f7065726174696f6e5f6964c410aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa6726573756c7482a474797065af537065637461746f724a6f696e6564a46461746188a7726f6f6d5f6964c410cccccccccccccccccccccccccccccccca9726f6f6d5f636f6465a6414243313233ac737065637461746f725f6964c410bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbba967616d655f6e616d65a467616d65af63757272656e745f706c617965727390b263757272656e745f737065637461746f727390ab6c6f6262795f7374617465a56c6f626279a6726561736f6ea66a6f696e6564",
        ),
        (
            "spectator_join_failed",
            RoomOperationResult::SpectatorJoinFailed {
                reason: "Missing".to_string(),
                error_code: Some(ErrorCode::RoomNotFound),
            },
            json!({"type":"SpectatorJoinFailed","data":{"reason":"Missing","error_code":"ROOM_NOT_FOUND"}}),
            "82a474797065b3526f6f6d4f7065726174696f6e526573756c74a46461746182ac6f7065726174696f6e5f6964c410aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa6726573756c7482a474797065b3537065637461746f724a6f696e4661696c6564a46461746182a6726561736f6ea74d697373696e67aa6572726f725f636f6465ae524f4f4d5f4e4f545f464f554e44",
        ),
        (
            "spectator_left",
            RoomOperationResult::SpectatorLeft {
                room_id: Some(ROOM_ID),
                room_code: Some("ABC123".to_string()),
                reason: Some(SpectatorStateChangeReason::VoluntaryLeave),
                current_spectators: Vec::new(),
            },
            json!({"type":"SpectatorLeft","data":{"room_id":ROOM_ID_STR,"room_code":"ABC123","reason":"voluntary_leave","current_spectators":[]}}),
            "82a474797065b3526f6f6d4f7065726174696f6e526573756c74a46461746182ac6f7065726174696f6e5f6964c410aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa6726573756c7482a474797065ad537065637461746f724c656674a46461746184a7726f6f6d5f6964c410cccccccccccccccccccccccccccccccca9726f6f6d5f636f6465a6414243313233a6726561736f6eaf766f6c756e746172795f6c65617665b263757272656e745f737065637461746f727390",
        ),
        (
            "operation_failed",
            RoomOperationResult::OperationFailed {
                reason: "Not in room".to_string(),
                error_code: Some(ErrorCode::NotInRoom),
            },
            json!({"type":"OperationFailed","data":{"reason":"Not in room","error_code":"NOT_IN_ROOM"}}),
            "82a474797065b3526f6f6d4f7065726174696f6e526573756c74a46461746182ac6f7065726174696f6e5f6964c410aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa6726573756c7482a474797065af4f7065726174696f6e4661696c6564a46461746182a6726561736f6eab4e6f7420696e20726f6f6daa6572726f725f636f6465ab4e4f545f494e5f524f4f4d",
        ),
    ];

    for (name, result, expected_result, expected_msgpack) in cases {
        let message = ServerMessage::RoomOperationResult {
            operation_id: OPERATION_ID,
            result: Box::new(result),
        };
        assert_json_and_dump_msgpack(
            name,
            &message,
            json!({"type":"RoomOperationResult","data":{"operation_id":OPERATION_ID_STR,"result":expected_result}}),
            expected_msgpack,
        );
    }
}

#[test]
fn autonomous_spectator_left_is_never_a_correlated_response() {
    let autonomous = ServerMessage::SpectatorLeft {
        room_id: Some(ROOM_ID),
        room_code: Some("ABC123".to_string()),
        reason: Some(SpectatorStateChangeReason::RoomClosed),
        current_spectators: Vec::new(),
    };
    assert_json_and_dump_msgpack(
        "autonomous_spectator_left",
        &autonomous,
        json!({"type":"SpectatorLeft","data":{"room_id":ROOM_ID_STR,"room_code":"ABC123","reason":"room_closed","current_spectators":[]}}),
        "82a474797065ad537065637461746f724c656674a46461746184a7726f6f6d5f6964c410cccccccccccccccccccccccccccccccca9726f6f6d5f636f6465a6414243313233a6726561736f6eab726f6f6d5f636c6f736564b263757272656e745f737065637461746f727390",
    );
}
