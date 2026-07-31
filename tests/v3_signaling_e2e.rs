//! End-to-end protocol v3 (P2/P3) targeted-signal-relay tests through the real
//! WebSocket stack. Two v3 + WebRTC peers join the same room and relay a full
//! opaque offer -> answer -> ICE sequence through the server with the payloads
//! byte-preserved, exercising axum's WebSocket framing on the unchanged
//! `handle_signal` path.
//!
//! Initial pairing is delivered by `SessionPlan` at finalize. Finalized-room
//! membership changes also publish complete authoritative plans; `NewPeer`
//! remains a compatibility wire shape. Over-the-wire pairing is therefore
//! covered by `tests/v3_session_plan_e2e.rs` (SessionPlan) and the handler-level
//! unit tests in `src/server/signaling_tests.rs`; this file does not depend on
//! additive peer directives.

mod test_helpers;
mod websocket_test_helpers;

use futures_util::SinkExt;
use serde_json::json;
use signal_fish_server::config::AppAuthEntry;
use signal_fish_server::protocol::{
    ClientMessage, PlayerId, RoomId, ServerMessage, Topology, Transport,
};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::{create_router, websocket_handler_v3};
use std::sync::Arc;
use test_helpers::{test_protocol_config, test_server_config, RunningTestServer};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::{
    next_matching_server_message_within, next_server_message_within, WsStream,
};

const APP_ID: &str = "v3-signaling-app";
const SERVER_MESSAGE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(5);

fn app_entry() -> AppAuthEntry {
    AppAuthEntry {
        app_id: APP_ID.to_string(),
        app_secret: "secret".to_string(),
        app_name: "V3 Signaling App".to_string(),
        max_rooms: Some(10),
        max_players_per_room: Some(8),
        rate_limit_per_minute: Some(600),
    }
}

async fn start_auth_server() -> RunningTestServer {
    start_auth_server_with_handle().await.0
}

async fn start_auth_server_with_handle() -> (RunningTestServer, Arc<EnhancedGameServer>) {
    start_auth_server_with_config(test_server_config()).await
}

async fn start_auth_server_with_config(
    mut server_config: ServerConfig,
) -> (RunningTestServer, Arc<EnhancedGameServer>) {
    server_config.auth_enabled = true;

    let mut protocol_config = test_protocol_config();
    protocol_config.sdk_compatibility.enforce = false;

    let game_server = EnhancedGameServer::new(
        server_config,
        protocol_config,
        signal_fish_server::config::RelayTypeConfig::default(),
        signal_fish_server::config::SessionConfig::default(),
        signal_fish_server::config::TurnConfig::default(),
        signal_fish_server::database::DatabaseConfig::InMemory,
        signal_fish_server::config::MetricsConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        vec![app_entry()],
    )
    .await
    .expect("server builds");

    let running_server = start_server(game_server.clone()).await;
    (running_server, game_server)
}

async fn start_server(game_server: Arc<EnhancedGameServer>) -> RunningTestServer {
    use axum::routing::get;

    let enhanced_router = create_router("http://localhost:3000").with_state(game_server.clone());
    let combined_router = axum::Router::new()
        .nest("/v2", enhanced_router)
        .route("/v3/ws", get(websocket_handler_v3))
        .fallback(|| async { "Use /v2/ws or /v3/ws" })
        .with_state(game_server.clone());

    RunningTestServer::spawn(game_server, combined_router).await
}

async fn connect(addr: std::net::SocketAddr) -> WsStream {
    let url = format!("ws://{addr}/v3/ws");
    let (ws, _) = tokio::time::timeout(tokio::time::Duration::from_secs(10), connect_async(&url))
        .await
        .expect("connect timeout")
        .expect("connect");
    ws
}

async fn send(ws: &mut WsStream, msg: &ClientMessage) {
    let json = serde_json::to_string(msg).unwrap();
    ws.send(Message::Text(json.into())).await.unwrap();
}

async fn next_server_message(ws: &mut WsStream) -> ServerMessage {
    next_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "next server message").await
}

/// Read messages until one matches `pick`, returning the mapped value. Skips
/// interleaved housekeeping messages (e.g. `PlayerJoined`).
async fn next_matching<T>(
    ws: &mut WsStream,
    context: &str,
    pick: impl FnMut(ServerMessage) -> Option<T>,
) -> T {
    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, context, pick).await
}

/// Authenticate as a v3 + WebRTC client and drain the `Authenticated` +
/// `ProtocolInfo` handshake, asserting v3 was negotiated.
async fn authenticate_v3(ws: &mut WsStream) {
    send(
        ws,
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

    let authed = next_server_message(ws).await;
    assert!(
        matches!(authed, ServerMessage::Authenticated { .. }),
        "expected Authenticated, got {authed:?}"
    );
    let info = next_server_message(ws).await;
    match info {
        ServerMessage::ProtocolInfo(info) => assert_eq!(info.protocol_version, Some(3)),
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
}

/// Join (or create) a room, returning `(room_id, room_code, our_player_id)`.
async fn join_room(
    ws: &mut WsStream,
    room_code: Option<String>,
    player_name: &str,
) -> (RoomId, String, PlayerId) {
    send(
        ws,
        &ClientMessage::JoinRoom {
            game_name: "signaling-game".to_string(),
            room_code,
            player_name: player_name.to_string(),
            // Capacity 2: the two peers fill the room but never mark ready, so it
            // never finalizes — no SessionPlan/NewPeer fires and the peers relay
            // signals directly over the unchanged `handle_signal` path.
            max_players: Some(2),
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await;

    next_matching(ws, "room join response", |msg| match msg {
        ServerMessage::RoomJoined(p) => Some((p.room_id, p.room_code, p.player_id)),
        ServerMessage::RoomJoinFailed { reason, error_code } => {
            panic!("room join failed: {reason} ({error_code:?})")
        }
        _ => None,
    })
    .await
}

async fn next_signal(ws: &mut WsStream) -> (PlayerId, serde_json::Value) {
    next_matching(ws, "relayed signal", |msg| match msg {
        ServerMessage::Signal { from, signal } => Some((from, signal)),
        _ => None,
    })
    .await
}

#[tokio::test]
async fn two_v3_peers_relay_offer_answer_ice_byte_preserved() {
    let running_server = start_auth_server().await;
    let addr = running_server.addr();

    // Peer 1 creates the room (capacity 2).
    let mut peer1 = connect(addr).await;
    authenticate_v3(&mut peer1).await;
    let (_room_id, room_code, peer1_id) = join_room(&mut peer1, None, "PeerOne").await;

    // Peer 2 joins the same room. No NewPeer fires on the plain join path (initial
    // pairing is the SessionPlan's job at finalize); the peers exchange signals
    // directly over the unchanged `handle_signal` relay path.
    let mut peer2 = connect(addr).await;
    authenticate_v3(&mut peer2).await;
    let (_room_id, _code, peer2_id) = join_room(&mut peer2, Some(room_code), "PeerTwo").await;
    assert_ne!(peer1_id, peer2_id);

    // Full opaque offer -> answer -> ICE relay, byte-preserved end to end.
    let offer = json!({ "Offer": "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\n" });
    send(
        &mut peer1,
        &ClientMessage::Signal {
            to: peer2_id,
            signal: offer.clone(),
        },
    )
    .await;
    let (from, signal) = next_signal(&mut peer2).await;
    assert_eq!(from, peer1_id);
    assert_eq!(signal, offer, "offer payload must be byte-preserved");

    let answer = json!({ "Answer": "v=0\r\no=- 2 2 IN IP4 0.0.0.0\r\n" });
    send(
        &mut peer2,
        &ClientMessage::Signal {
            to: peer1_id,
            signal: answer.clone(),
        },
    )
    .await;
    let (from, signal) = next_signal(&mut peer1).await;
    assert_eq!(from, peer2_id);
    assert_eq!(signal, answer, "answer payload must be byte-preserved");

    let ice = json!({ "IceCandidate": "candidate:1 1 UDP 2130706431 192.0.2.1 54321 typ host" });
    send(
        &mut peer1,
        &ClientMessage::Signal {
            to: peer2_id,
            signal: ice.clone(),
        },
    )
    .await;
    let (from, signal) = next_signal(&mut peer2).await;
    assert_eq!(from, peer1_id);
    assert_eq!(signal, ice, "ICE payload must be byte-preserved");
    running_server.shutdown().await;
}

#[tokio::test]
async fn reconnected_websocket_uses_restored_player_id_for_later_signals() {
    let (running_server, game_server) = start_auth_server_with_handle().await;
    let addr = running_server.addr();

    let mut peer1 = connect(addr).await;
    authenticate_v3(&mut peer1).await;
    let (_room_id, room_code, peer1_id) = join_room(&mut peer1, None, "PeerOne").await;

    let mut old_peer2 = connect(addr).await;
    authenticate_v3(&mut old_peer2).await;
    let (room_id, _code, peer2_id) = join_room(&mut old_peer2, Some(room_code), "PeerTwo").await;

    // No NewPeer fires on the plain join path (the room never finalizes), so
    // there is nothing to drain before the post-reconnect signal assertion.

    let room = game_server
        .database()
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup")
        .expect("room exists");
    let peer2_info = room
        .players
        .get(&peer2_id)
        .cloned()
        .expect("peer2 in room before disconnect");

    game_server.disconnect_client(&peer2_id).await;
    let _ = old_peer2.close(None).await;

    // `disconnect_client` already auto-registered the disconnect with peer2's
    // pre-issued (join-time) token. This re-registration of the same
    // still-pending room only surfaces that token to the test: it is idempotent
    // on the token (a same-room re-registration preserves the client-held one),
    // so `token` is exactly what peer2 received in `RoomJoined` and would
    // reconnect with — the reconnect below stays faithful to a real client.
    let token = game_server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(peer2_id, room_id, false, Some(peer2_info), 0)
        .await;

    let mut replacement = connect(addr).await;
    authenticate_v3(&mut replacement).await;
    send(
        &mut replacement,
        &ClientMessage::Reconnect {
            player_id: peer2_id,
            room_id,
            auth_token: token,
        },
    )
    .await;

    next_matching(&mut replacement, "reconnect response", |msg| match msg {
        ServerMessage::Reconnected(payload) => {
            assert_eq!(payload.player_id, peer2_id);
            Some(())
        }
        ServerMessage::ReconnectionFailed { reason, error_code } => {
            panic!("reconnect failed: {reason} ({error_code:?})")
        }
        _ => None,
    })
    .await;

    let ice = json!({ "IceCandidate": "candidate:restored-id" });
    send(
        &mut replacement,
        &ClientMessage::Signal {
            to: peer1_id,
            signal: ice.clone(),
        },
    )
    .await;

    let (from, relayed_signal) = next_signal(&mut peer1).await;
    assert_eq!(
        from, peer2_id,
        "post-reconnect signal must be routed under the restored player id"
    );
    assert_eq!(relayed_signal, ice);
    running_server.shutdown().await;
}

#[tokio::test]
async fn oversized_signal_is_rejected_over_the_wire_and_small_signal_still_relays() {
    // Cap the serialized `signal` payload at 256 bytes (`security.max_signal_bytes`).
    let mut server_config = test_server_config();
    server_config.max_signal_bytes = 256;
    let (running_server, _server) = start_auth_server_with_config(server_config).await;
    let addr = running_server.addr();

    let mut peer1 = connect(addr).await;
    authenticate_v3(&mut peer1).await;
    let (_room_id, room_code, _peer1_id) = join_room(&mut peer1, None, "PeerOne").await;

    let mut peer2 = connect(addr).await;
    authenticate_v3(&mut peer2).await;
    let (_room_id, _code, peer2_id) = join_room(&mut peer2, Some(room_code), "PeerTwo").await;

    // Over the cap (serialized length > 256): the sender gets SIGNAL_TOO_LARGE
    // and the target receives nothing.
    let oversized = json!({ "Offer": "x".repeat(512) });
    send(
        &mut peer1,
        &ClientMessage::Signal {
            to: peer2_id,
            signal: oversized,
        },
    )
    .await;
    next_matching(&mut peer1, "oversized signal rejection", |msg| match msg {
        ServerMessage::Error {
            error_code: Some(signal_fish_server::protocol::ErrorCode::SignalTooLarge),
            ..
        } => Some(()),
        ServerMessage::Signal { .. } => panic!("oversized signal must not be relayed"),
        _ => None,
    })
    .await;

    // A small signal on the same connection still relays: the rejection did not
    // poison the connection or consume the valid-signal budget.
    let small = json!({ "IceCandidate": "candidate:ok" });
    send(
        &mut peer1,
        &ClientMessage::Signal {
            to: peer2_id,
            signal: small.clone(),
        },
    )
    .await;
    let (_from, relayed) = next_signal(&mut peer2).await;
    assert_eq!(
        relayed, small,
        "small signal must relay after a size rejection"
    );
    running_server.shutdown().await;
}
