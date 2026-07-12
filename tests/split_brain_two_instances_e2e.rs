//! P10.H5 documentation experiment: prove the failure modes produced when two
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
    ClientMessage, ErrorCode, PlayerId, RoomJoinedPayload, ServerMessage, Topology, Transport,
};
use tokio_tungstenite::connect_async;
use v3_conformance_helpers::{send, SERVER_MESSAGE_TIMEOUT};
use websocket_test_helpers::server_process::{spawn_server, CONNECT_TIMEOUT};
use websocket_test_helpers::{next_matching_server_message_within, WsStream};

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
        },
    )
    .await;

    next_matching_server_message_within(
        &mut ws,
        SERVER_MESSAGE_TIMEOUT,
        "split-brain authentication",
        |message| matches!(message, ServerMessage::Authenticated { .. }).then_some(()),
    )
    .await;
    next_matching_server_message_within(
        &mut ws,
        SERVER_MESSAGE_TIMEOUT,
        "split-brain protocol negotiation",
        |message| match message {
            ServerMessage::ProtocolInfo(info) => {
                assert_eq!(info.protocol_version, Some(3));
                Some(())
            }
            _ => None,
        },
    )
    .await;

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

    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "split-brain room join",
        |message| match message {
            ServerMessage::RoomJoined(payload) => Some(payload),
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                panic!("split-brain room join failed: {reason} ({error_code:?})")
            }
            _ => None,
        },
    )
    .await
}

async fn expect_signal_rejection(ws: &mut WsStream, target: PlayerId) {
    send(
        ws,
        &ClientMessage::Signal {
            to: target,
            signal: json!({"Offer": "cross-instance-offer"}),
        },
    )
    .await;

    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "cross-instance signal rejection",
        |message| match message {
            ServerMessage::Error {
                message,
                error_code,
            } => {
                assert_eq!(error_code, Some(ErrorCode::SignalTargetNotFound));
                assert_eq!(message, "Signal target is not in any room");
                Some(())
            }
            ServerMessage::Signal { .. } => {
                panic!("a cross-instance Signal must never be relayed")
            }
            _ => None,
        },
    )
    .await;
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
    let (reason, error_code) = next_matching_server_message_within(
        &mut cross_instance_reconnector,
        SERVER_MESSAGE_TIMEOUT,
        "cross-instance reconnect rejection",
        |message| match message {
            ServerMessage::ReconnectionFailed { reason, error_code } => Some((reason, error_code)),
            ServerMessage::Reconnected(payload) => panic!(
                "a different process must not reconnect instance A player {}",
                payload.player_id
            ),
            _ => None,
        },
    )
    .await;
    assert_eq!(error_code, ErrorCode::ReconnectionFailed);
    assert_eq!(reason, "No disconnection record found");

    // Instance A cannot see instance B's player registry. The rejection is
    // explicit and deterministic, but it cannot identify this as CrossRoom:
    // the target is entirely absent from A's local world.
    expect_signal_rejection(&mut peer_a, joined_b.player_id).await;
}
