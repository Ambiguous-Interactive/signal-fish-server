//! End-to-end protocol v3 (P2) targeted-signal-relay tests through the real
//! WebSocket stack. Two v3 + WebRTC peers join the same room, are paired by the
//! late-join `NewPeer` handshake (exactly one offerer per pair), and relay a full
//! opaque offer -> answer -> ICE sequence through the server with the payloads
//! byte-preserved. Complements the handler-level tests in
//! `src/server/signaling_tests.rs` by exercising axum's WebSocket framing.

mod test_helpers;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use signal_fish_server::config::AppAuthEntry;
use signal_fish_server::protocol::{ClientMessage, PlayerId, ServerMessage, Topology, Transport};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::{create_router, websocket_handler_v3};
use std::sync::Arc;
use test_helpers::{test_protocol_config, test_server_config};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const APP_ID: &str = "v3-signaling-app";

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

async fn start_auth_server() -> std::net::SocketAddr {
    let mut server_config: ServerConfig = test_server_config();
    server_config.auth_enabled = true;

    let mut protocol_config = test_protocol_config();
    protocol_config.sdk_compatibility.enforce = false;

    let game_server = EnhancedGameServer::new(
        server_config,
        protocol_config,
        signal_fish_server::config::RelayTypeConfig::default(),
        signal_fish_server::database::DatabaseConfig::InMemory,
        signal_fish_server::config::MetricsConfig::default(),
        signal_fish_server::config::AuthMaintenanceConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        vec![app_entry()],
    )
    .await
    .expect("server builds");

    start_server(game_server).await
}

async fn start_server(game_server: Arc<EnhancedGameServer>) -> std::net::SocketAddr {
    use axum::routing::get;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let enhanced_router = create_router("http://localhost:3000").with_state(game_server.clone());
    let combined_router = axum::Router::new()
        .nest("/v2", enhanced_router)
        .route("/v3/ws", get(websocket_handler_v3))
        .fallback(|| async { "Use /v2/ws or /v3/ws" })
        .with_state(game_server);

    tokio::spawn(async move {
        axum::serve(
            listener,
            combined_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    addr
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

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
    loop {
        let frame = tokio::time::timeout(tokio::time::Duration::from_secs(5), ws.next())
            .await
            .expect("recv timeout")
            .expect("stream closed")
            .expect("ws error");
        if let Message::Text(text) = frame {
            return serde_json::from_str(&text).expect("valid ServerMessage");
        }
    }
}

/// Read messages until one matches `pick`, returning the mapped value. Skips
/// interleaved housekeeping messages (e.g. `PlayerJoined`).
async fn next_matching<T>(
    ws: &mut WsStream,
    mut pick: impl FnMut(&ServerMessage) -> Option<T>,
) -> T {
    loop {
        let msg = next_server_message(ws).await;
        if let Some(value) = pick(&msg) {
            return value;
        }
    }
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

/// Join (or create) a room, returning `(room_code, our_player_id)`.
async fn join_room(
    ws: &mut WsStream,
    room_code: Option<String>,
    player_name: &str,
) -> (String, PlayerId) {
    send(
        ws,
        &ClientMessage::JoinRoom {
            game_name: "signaling-game".to_string(),
            room_code,
            player_name: player_name.to_string(),
            // Keep capacity above the member count so the room does not enter the
            // lobby/finalization flow — P2 late-join fires on the plain join path.
            max_players: Some(4),
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await;

    next_matching(ws, |msg| match msg {
        ServerMessage::RoomJoined(p) => Some((p.room_code.clone(), p.player_id)),
        ServerMessage::RoomJoinFailed { reason, error_code } => {
            panic!("room join failed: {reason} ({error_code:?})")
        }
        _ => None,
    })
    .await
}

async fn next_signal(ws: &mut WsStream) -> (PlayerId, serde_json::Value) {
    next_matching(ws, |msg| match msg {
        ServerMessage::Signal { from, signal } => Some((*from, signal.clone())),
        _ => None,
    })
    .await
}

async fn next_new_peer(ws: &mut WsStream) -> (PlayerId, bool) {
    next_matching(ws, |msg| match msg {
        ServerMessage::NewPeer {
            peer_id,
            you_initiate,
        } => Some((*peer_id, *you_initiate)),
        _ => None,
    })
    .await
}

#[tokio::test]
async fn two_v3_peers_relay_offer_answer_ice_and_are_paired_by_new_peer() {
    let addr = start_auth_server().await;

    // Peer 1 creates the room.
    let mut peer1 = connect(addr).await;
    authenticate_v3(&mut peer1).await;
    let (room_code, peer1_id) = join_room(&mut peer1, None, "PeerOne").await;

    // Peer 2 joins the same room, which triggers the late-join NewPeer handshake.
    let mut peer2 = connect(addr).await;
    authenticate_v3(&mut peer2).await;
    let (_code, peer2_id) = join_room(&mut peer2, Some(room_code), "PeerTwo").await;
    assert_ne!(peer1_id, peer2_id);

    // Both sides are told about each other, and exactly one is the offerer.
    let (p1_sees, p1_initiate) = next_new_peer(&mut peer1).await;
    let (p2_sees, p2_initiate) = next_new_peer(&mut peer2).await;
    assert_eq!(p1_sees, peer2_id, "peer1's NewPeer should name peer2");
    assert_eq!(p2_sees, peer1_id, "peer2's NewPeer should name peer1");
    assert_ne!(
        p1_initiate, p2_initiate,
        "exactly one side of the pair must initiate (glare avoidance)"
    );
    // The offerer is the lexicographically smaller UUID (Appendix E).
    assert_eq!(p1_initiate, peer1_id < peer2_id);

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
}
