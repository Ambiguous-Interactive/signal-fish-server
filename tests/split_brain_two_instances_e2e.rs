//! Split-brain documentation experiment: prove the failure modes produced when two
//! independent Signal Fish processes receive members of the same logical room.
//!
//! This is not a supported topology or a test expected to "pass" multi-node
//! operation. Its product is the executable failure catalog consumed by the
//! single-instance deployment contract:
//!
//! - the same `(game_name, room_code)` silently creates one room per instance;
//! - reconnect state is stranded on the instance that issued the token; and
//! - a cross-instance WebRTC target is explicitly rejected, never relayed.

mod v3_conformance_helpers;
mod websocket_test_helpers;

use serde_json::json;
use signal_fish_server::protocol::{
    ClientMessage, ErrorCode, LobbyState, PlayerId, RoomJoinedPayload, ServerMessage, Topology,
    Transport,
};
use tokio_tungstenite::connect_async;
use v3_conformance_helpers::{send, SERVER_MESSAGE_TIMEOUT};
use websocket_test_helpers::server_process::{spawn_server, CONNECT_TIMEOUT};
use websocket_test_helpers::{next_server_message_within, WsStream};

const APP_ID: &str = "split-brain-catalog-app";
const GAME_NAME: &str = "split-brain-catalog-game";

fn config_overlay() -> serde_json::Value {
    json!({
        "session": {
            "default_topology": "mesh",
            "enable_webrtc": true
        }
    })
}

async fn connect_v3(port: u16) -> WsStream {
    let url = format!("ws://127.0.0.1:{port}/v3/ws");
    let (mut ws, _) = tokio::time::timeout(CONNECT_TIMEOUT, connect_async(&url))
        .await
        .expect("websocket connect timeout")
        .expect("websocket connect");

    send(
        &mut ws,
        &ClientMessage::Authenticate {
            app_id: APP_ID.to_string(),
            sdk_version: None,
            platform: None,
            game_data_format: None,
            protocol_version: Some(3),
            supported_transports: Some(vec![Transport::Relay, Transport::WebRtc]),
            supported_topologies: Some(vec![Topology::Relay, Topology::Mesh]),
            requested_capabilities: None,
        },
    )
    .await;

    let handshake_response = next_server_message_within(
        &mut ws,
        SERVER_MESSAGE_TIMEOUT,
        "split-brain authentication",
    )
    .await;
    assert!(
        matches!(handshake_response, ServerMessage::Authenticated { .. }),
        "expected Authenticated, got {handshake_response:?}"
    );

    let protocol_info = next_server_message_within(
        &mut ws,
        SERVER_MESSAGE_TIMEOUT,
        "split-brain protocol negotiation",
    )
    .await;
    let ServerMessage::ProtocolInfo(info) = protocol_info else {
        panic!("expected ProtocolInfo, got {protocol_info:?}");
    };
    assert_eq!(info.protocol_version, Some(3));

    ws
}

async fn join_room(
    ws: &mut WsStream,
    room_code: Option<String>,
    player_name: &str,
) -> Box<RoomJoinedPayload> {
    send(
        ws,
        &ClientMessage::JoinRoom {
            game_name: GAME_NAME.to_string(),
            room_code,
            player_name: player_name.to_string(),
            max_players: Some(2),
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await;

    let message =
        next_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "split-brain room join").await;
    let payload = match message {
        ServerMessage::RoomJoined(payload) => payload,
        ServerMessage::RoomJoinFailed { reason, error_code } => {
            panic!("split-brain room join failed: {reason} ({error_code:?})")
        }
        other => panic!("expected RoomJoined, got {other:?}"),
    };

    let lobby_transition = next_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "split-brain first-player lobby transition",
    )
    .await;
    match lobby_transition {
        ServerMessage::LobbyStateChanged {
            lobby_state,
            ready_players,
            all_ready,
        } => {
            assert_eq!(lobby_state, LobbyState::Lobby);
            assert!(ready_players.is_empty());
            assert!(!all_ready);
        }
        other => panic!("expected LobbyStateChanged, got {other:?}"),
    }

    payload
}

async fn expect_signal_rejection(ws: &mut WsStream, target: PlayerId) {
    send(
        ws,
        &ClientMessage::Signal {
            to: target,
            generation: uuid::Uuid::nil(),
            signal: json!({"Offer": "cross-instance-offer"}),
        },
    )
    .await;

    let rejection = next_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "cross-instance signal rejection",
    )
    .await;
    match rejection {
        ServerMessage::Error {
            message,
            error_code,
        } => {
            assert_eq!(error_code, Some(ErrorCode::SignalTargetNotFound));
            assert_eq!(message, "Signal target is not in any room");
        }
        ServerMessage::Signal { .. } => panic!("a cross-instance Signal must never be relayed"),
        other => panic!("expected SignalTargetNotFound, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "nightly-only documentation experiment (verification-nightly.yml): spawns two real server processes"]
async fn two_instances_produce_the_documented_split_brain_catalog() {
    let instance_a = spawn_server(config_overlay()).await;
    let instance_b = spawn_server(config_overlay()).await;

    let mut peer_a = connect_v3(instance_a.port).await;
    let joined_a = join_room(&mut peer_a, None, "InstanceAPlayer").await;
    let reconnect_token = joined_a
        .reconnection_token
        .clone()
        .expect("v3 RoomJoined must issue a reconnect token");

    // Joining the same logical key on another process does not discover the
    // first room. Join-by-code creates a second local room without warning.
    let mut peer_b = connect_v3(instance_b.port).await;
    let joined_b = join_room(
        &mut peer_b,
        Some(joined_a.room_code.clone()),
        "InstanceBPlayer",
    )
    .await;
    assert_eq!(joined_b.room_code, joined_a.room_code);
    assert_eq!(joined_b.game_name, joined_a.game_name);
    assert_ne!(
        joined_b.room_id, joined_a.room_id,
        "the two processes must expose the silent split as distinct room identities"
    );
    assert_eq!(joined_a.current_players.len(), 1);
    assert_eq!(joined_b.current_players.len(), 1);

    // The second process has neither the player identity nor the pending
    // disconnection registry entry, even when presented with the real token.
    let mut cross_instance_reconnector = connect_v3(instance_b.port).await;
    send(
        &mut cross_instance_reconnector,
        &ClientMessage::Reconnect {
            player_id: joined_a.player_id,
            room_id: joined_a.room_id,
            auth_token: reconnect_token,
        },
    )
    .await;
    let reconnect_rejection = next_server_message_within(
        &mut cross_instance_reconnector,
        SERVER_MESSAGE_TIMEOUT,
        "cross-instance reconnect rejection",
    )
    .await;
    let (reason, error_code) = match reconnect_rejection {
        ServerMessage::ReconnectionFailed { reason, error_code } => (reason, error_code),
        ServerMessage::Reconnected(payload) => panic!(
            "a different process must not reconnect instance A player {}",
            payload.player_id
        ),
        other => panic!("expected ReconnectionFailed, got {other:?}"),
    };
    assert_eq!(error_code, ErrorCode::ReconnectionFailed);
    assert_eq!(reason, "No disconnection record found");

    // Instance A cannot see instance B's player registry. The rejection is
    // explicit and deterministic, but it cannot identify this as CrossRoom:
    // the target is entirely absent from A's local world.
    expect_signal_rejection(&mut peer_a, joined_b.player_id).await;
}
