//! End-to-end protocol v3 (P3) session-plan tests through the real WebSocket
//! stack. A server configured for `mesh` topology finalizes a two-player room:
//! each v3 client must receive `GameStarting` THEN a per-recipient `SessionPlan`
//! (mesh + webrtc, fallback relay, naming the other peer with the single-offerer
//! `initiate`). A mixed room (one v3 + one relay-only client) resolves to the
//! relay floor and neither member receives a `SessionPlan` (Appendix K).
//!
//! Models the harness in `tests/v3_signaling_e2e.rs` but injects a chosen
//! `SessionConfig` so finalization can pick a non-relay plan.

mod test_helpers;
mod websocket_test_helpers;

use futures_util::SinkExt;
use signal_fish_server::config::{AppAuthEntry, SessionConfig, TurnConfig};
use signal_fish_server::protocol::{
    ClientMessage, IceServer, PlayerId, RoomId, ServerMessage, Topology, Transport,
};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::{create_router, websocket_handler_v3};
use std::sync::Arc;
use test_helpers::{test_protocol_config, test_server_config};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::{
    deadline_after, maybe_next_matching_server_message_with_skipped_until,
    next_matching_server_message_within, next_server_message_within, WsStream,
};

const APP_ID: &str = "v3-session-plan-app";
const SERVER_MESSAGE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(20);
const POST_GAME_STARTING_SESSION_PLAN_WINDOW: tokio::time::Duration =
    tokio::time::Duration::from_secs(5);
const STATIC_STUN_URL: &str = "stun:static.example.com:3478";
const TURN_STUN_URL: &str = "stun:stun.l.google.com:19302";

fn app_entry() -> AppAuthEntry {
    AppAuthEntry {
        app_id: APP_ID.to_string(),
        app_secret: "secret".to_string(),
        app_name: "V3 Session Plan App".to_string(),
        max_rooms: Some(10),
        max_players_per_room: Some(8),
        rate_limit_per_minute: Some(600),
    }
}

#[test]
fn ice_ordering_fixtures_are_source_distinguishable() {
    assert_ne!(
        STATIC_STUN_URL, TURN_STUN_URL,
        "static and default [turn] STUN fixture URLs must stay distinct so ordering assertions catch swaps"
    );
    assert_eq!(
        TurnConfig::default().stun_urls,
        vec![TURN_STUN_URL.to_string()],
        "this e2e fixture intentionally exercises the default [turn] STUN URL"
    );
}

fn mesh_session_config() -> SessionConfig {
    SessionConfig {
        default_topology: Topology::Mesh,
        ice_servers: vec![IceServer {
            urls: vec![STATIC_STUN_URL.to_string()],
            username: None,
            credential: None,
        }],
        ..SessionConfig::default()
    }
}

/// Start a server with the given `SessionConfig` and the **default** `[turn]`
/// block (TURN disabled, but the default public STUN advertised), exercising the
/// production default ICE wiring end to end.
async fn start_server_with_session(session: SessionConfig) -> std::net::SocketAddr {
    let mut server_config: ServerConfig = test_server_config();
    server_config.auth_enabled = true;

    let mut protocol_config = test_protocol_config();
    protocol_config.sdk_compatibility.enforce = false;

    let game_server = EnhancedGameServer::new(
        server_config,
        protocol_config,
        signal_fish_server::config::RelayTypeConfig::default(),
        session,
        signal_fish_server::config::TurnConfig::default(),
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

    // No startup sleep: the listener is already bound above, so connections
    // issued immediately are accepted by the kernel and served once the
    // spawned `axum::serve` task polls them.
    addr
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
    // Keep reads bounded while allowing for CI scheduling delays around the
    // finalization burst (`LobbyStateChanged`, `GameStarting`, `SessionPlan`).
    next_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "next server message").await
}

/// Authenticate, advertising the given protocol version + capabilities.
async fn authenticate(
    ws: &mut WsStream,
    protocol_version: Option<u16>,
    transports: Option<Vec<Transport>>,
    topologies: Option<Vec<Topology>>,
    expected_version: Option<u16>,
) {
    send(
        ws,
        &ClientMessage::Authenticate {
            app_id: APP_ID.to_string(),
            sdk_version: None,
            platform: None,
            game_data_format: None,
            protocol_version,
            supported_transports: transports,
            supported_topologies: topologies,
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
        ServerMessage::ProtocolInfo(info) => assert_eq!(info.protocol_version, expected_version),
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
}

/// Authenticate as a v3 mesh+webrtc client.
async fn authenticate_v3_mesh(ws: &mut WsStream) {
    authenticate(
        ws,
        Some(3),
        Some(vec![Transport::Relay, Transport::WebRtc]),
        Some(vec![Topology::Relay, Topology::Mesh]),
        Some(3),
    )
    .await;
}

/// Authenticate as a pure v2 client on the v3 endpoint (relay-only).
async fn authenticate_v2(ws: &mut WsStream) {
    authenticate(ws, Some(2), None, None, None).await;
}

/// Join (or create) a 2-player room, returning `(room_id, room_code, our_id)`.
async fn join_room(
    ws: &mut WsStream,
    room_code: Option<String>,
    player_name: &str,
) -> (RoomId, String, PlayerId) {
    send(
        ws,
        &ClientMessage::JoinRoom {
            game_name: "plan-game".to_string(),
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
        "room join response",
        |message| match message {
            ServerMessage::RoomJoined(p) => Some((p.room_id, p.room_code, p.player_id)),
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                panic!("room join failed: {reason} ({error_code:?})")
            }
            _ => None,
        },
    )
    .await
}

async fn ready(ws: &mut WsStream) {
    send(ws, &ClientMessage::PlayerReady).await;
}

/// Drain messages until a `LobbyStateChanged` reports exactly `count` ready
/// players. Paces the ready handshake so each `PlayerReady` is fully processed
/// (and reflected in `ready_players`) before the next is sent, removing the
/// back-to-back-ready race.
async fn await_ready_count(ws: &mut WsStream, count: usize) {
    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "lobby ready count update",
        |message| match message {
            ServerMessage::LobbyStateChanged { ready_players, .. }
                if ready_players.len() == count =>
            {
                Some(())
            }
            _ => None,
        },
    )
    .await;
}

/// The `SessionPlan` (if any) a client receives after `GameStarting`. Reaching
/// this point at all means `GameStarting` was observed, in order.
type SessionPlan = Box<signal_fish_server::protocol::SessionPlanPayload>;

#[derive(Debug)]
struct Finalization {
    session_plan: Option<SessionPlan>,
    skipped_after_game_starting: String,
}

impl Finalization {
    fn expect_session_plan(self, context: &str) -> SessionPlan {
        self.session_plan.unwrap_or_else(|| {
            panic!(
                "{context} must receive a SessionPlan after GameStarting; skipped after GameStarting: {}",
                self.skipped_after_game_starting
            )
        })
    }
}

/// Read until `GameStarting` is observed (asserting no `SessionPlan` precedes
/// it), then collect a `SessionPlan` if one arrives within a short window.
async fn read_finalization(ws: &mut WsStream) -> Finalization {
    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "finalization GameStarting",
        |message| match message {
            ServerMessage::GameStarting { .. } => Some(()),
            ServerMessage::SessionPlan(_) => {
                panic!("SessionPlan must not arrive before GameStarting")
            }
            _ => None,
        },
    )
    .await;

    // After GameStarting, a SessionPlan may or may not follow. Use one absolute
    // deadline so unrelated frames cannot extend this optional window.
    let (session_plan, skipped_after_game_starting) =
        maybe_next_matching_server_message_with_skipped_until(
            ws,
            deadline_after(POST_GAME_STARTING_SESSION_PLAN_WINDOW),
            "post-GameStarting SessionPlan",
            |message| match message {
                ServerMessage::SessionPlan(plan) => Some(plan),
                _ => None,
            },
        )
        .await;

    Finalization {
        session_plan,
        skipped_after_game_starting,
    }
}

#[tokio::test]
async fn mesh_room_finalization_sends_game_starting_then_session_plan() {
    let addr = start_server_with_session(mesh_session_config()).await;

    let mut peer1 = connect(addr).await;
    authenticate_v3_mesh(&mut peer1).await;
    let (_room_id, room_code, peer1_id) = join_room(&mut peer1, None, "PeerOne").await;

    let mut peer2 = connect(addr).await;
    authenticate_v3_mesh(&mut peer2).await;
    let (_room_id, _code, peer2_id) = join_room(&mut peer2, Some(room_code), "PeerTwo").await;
    assert_ne!(peer1_id, peer2_id);

    // Pace the readies: peer1 readies and both observe the (not-yet-all-ready)
    // lobby update before peer2 readies and triggers finalization.
    ready(&mut peer1).await;
    await_ready_count(&mut peer1, 1).await;
    await_ready_count(&mut peer2, 1).await;
    ready(&mut peer2).await;
    // Both members are ready; readiness no longer auto-starts, so the creator
    // explicitly starts the game once the all-ready (2/2) lobby update lands.
    await_ready_count(&mut peer1, 2).await;
    await_ready_count(&mut peer2, 2).await;
    send(&mut peer1, &ClientMessage::StartGame).await;

    // Reaching past read_finalization means each saw GameStarting first, in order.
    let (plan1, plan2) = tokio::join!(read_finalization(&mut peer1), read_finalization(&mut peer2));
    let plan1 = plan1.expect_session_plan("peer1");
    let plan2 = plan2.expect_session_plan("peer2");

    for plan in [&plan1, &plan2] {
        assert_eq!(plan.topology, Topology::Mesh);
        assert_eq!(plan.transport, Transport::WebRtc);
        assert_eq!(plan.fallback, Transport::Relay);
        assert!(plan.host.is_none());
        assert_eq!(
            plan.peers.len(),
            1,
            "two-player mesh lists exactly one peer"
        );
        // Two ICE entries: the operator's static `session.ice_servers` STUN
        // (preserved verbatim, first) followed by the default `[turn]` block's
        // public STUN (TURN is disabled by default, so no credentials are minted).
        assert_eq!(
            plan.ice_servers.len(),
            2,
            "webrtc plan carries the static STUN plus the default TURN-block STUN"
        );
        assert_eq!(plan.ice_servers[0].urls, vec![STATIC_STUN_URL]);
        assert_eq!(plan.ice_servers[1].urls, vec![TURN_STUN_URL]);
        assert!(
            plan.ice_servers
                .iter()
                .all(|s| s.username.is_none() && s.credential.is_none()),
            "no TURN credentials are minted when the [turn] block is disabled"
        );
    }

    // Each plan names the other peer, with exactly one offerer (Appendix E).
    assert_eq!(plan1.peers[0].player_id, peer2_id);
    assert_eq!(plan2.peers[0].player_id, peer1_id);
    assert_ne!(plan1.peers[0].initiate, plan2.peers[0].initiate);
    assert_eq!(plan1.peers[0].initiate, peer1_id < peer2_id);
}

#[tokio::test]
async fn mixed_v2_v3_room_finalization_sends_no_session_plan() {
    let addr = start_server_with_session(mesh_session_config()).await;

    // Peer 1 is v3 mesh; peer 2 authenticates as pure v2 (relay-only) on /v3/ws.
    let mut peer1 = connect(addr).await;
    authenticate_v3_mesh(&mut peer1).await;
    let (_room_id, room_code, peer1_id) = join_room(&mut peer1, None, "PeerOne").await;

    let mut peer2 = connect(addr).await;
    authenticate_v2(&mut peer2).await;
    let (_room_id, _code, peer2_id) = join_room(&mut peer2, Some(room_code), "PeerTwo").await;
    assert_ne!(peer1_id, peer2_id);

    ready(&mut peer1).await;
    await_ready_count(&mut peer1, 1).await;
    await_ready_count(&mut peer2, 1).await;
    ready(&mut peer2).await;
    // Both members are ready; the creator explicitly starts the game (readiness
    // no longer auto-starts) once the all-ready (2/2) lobby update lands.
    await_ready_count(&mut peer1, 2).await;
    await_ready_count(&mut peer2, 2).await;
    send(&mut peer1, &ClientMessage::StartGame).await;

    // Both receive GameStarting (reaching past read_finalization proves it,
    // in order), exactly like v2. The room resolved to the relay floor, so
    // NEITHER receives a SessionPlan.
    let (plan1, plan2) = tokio::join!(read_finalization(&mut peer1), read_finalization(&mut peer2));
    assert!(
        plan1.session_plan.is_none(),
        "v3 peer must not receive a SessionPlan in a relay-resolved room; skipped after GameStarting: {}",
        plan1.skipped_after_game_starting
    );
    assert!(
        plan2.session_plan.is_none(),
        "v2 peer must never receive a SessionPlan; skipped after GameStarting: {}",
        plan2.skipped_after_game_starting
    );
}
