//! End-to-end protocol v3 ICE pre-gather tests through the real WebSocket stack
//! (PLAN §P4's deferred "RoomJoined ICE pre-gather" refinement).
//!
//! An eligible v3 client — one that negotiated the WebRTC transport and the
//! game's desired topology — joining (or reconnecting into) a non-finalized
//! room of a non-relay-desired game must receive the composed ICE list (static
//! `session.ice_servers` first, then the `[turn]` STUN entry, then a freshly
//! minted per-player TURN credential) directly on `RoomJoined` / `Reconnected`,
//! so it can pre-gather ICE candidates during the lobby wait. Every other case
//! must omit the field entirely — asserted on the **raw JSON frame** where the
//! scenario is about wire bytes — keeping the v2 wire byte-identical. A
//! late-join/reconnect into an active session pre-gathers nothing (the fresh
//! late-join `SessionPlan` is the only ICE/TURN issuance site there, so one
//! logical join event never mints twice).
//!
//! Models the harness in `tests/v3_session_plan_e2e.rs` but injects both a
//! chosen `SessionConfig` and a chosen `TurnConfig`, and keeps the
//! `EnhancedGameServer` handle so metrics can be asserted.

mod test_helpers;
mod websocket_test_helpers;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use signal_fish_server::config::{AppRegistrationEntry, SessionConfig, TurnConfig};
use signal_fish_server::protocol::{
    ClientMessage, IceServer, PlayerId, RoomId, ServerMessage, Topology, Transport,
};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::{create_router, websocket_route_v3};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use test_helpers::{test_protocol_config, test_server_config, RunningTestServer};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::{
    deadline_after, maybe_next_matching_server_message_with_skipped_until,
    next_matching_server_message_within, next_server_message_within, WsStream,
};

const APP_ID: &str = "v3-ice-pregather-app";
const SERVER_MESSAGE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(20);
const POST_GAME_STARTING_SESSION_PLAN_WINDOW: tokio::time::Duration =
    tokio::time::Duration::from_secs(5);
const STATIC_STUN_URL: &str = "stun:static.example.com:3478";
const TURN_STUN_URL: &str = "stun:stun.l.google.com:19302";
const TURN_URL: &str = "turn:turn.example.com:3478";
const TURN_TTL_SECS: u64 = 3600;

fn app_entry() -> AppRegistrationEntry {
    AppRegistrationEntry {
        app_id: APP_ID.to_string(),
        app_name: "V3 ICE Pre-Gather App".to_string(),
        max_rooms: Some(10),
        max_players_per_room: Some(8),
        rate_limit_per_minute: Some(600),
    }
}

/// Mesh-desired session config with one static (operator) ICE server, plus a
/// relay-mapped game for the per-game gate scenario.
fn mesh_session_config() -> SessionConfig {
    let mut mappings = std::collections::HashMap::new();
    mappings.insert("relay-game".to_string(), Topology::Relay);
    SessionConfig {
        default_topology: Topology::Mesh,
        game_topology_mappings: mappings,
        ice_servers: vec![IceServer {
            urls: vec![STATIC_STUN_URL.to_string()],
            username: None,
            credential: None,
        }],
        ..SessionConfig::default()
    }
}

/// Enabled static-secret `[turn]` block (mints per-player credentials).
fn enabled_turn_config() -> TurnConfig {
    TurnConfig {
        enabled: true,
        static_auth_secret: "super-secret".to_string(),
        urls: vec![TURN_URL.to_string()],
        stun_urls: vec![TURN_STUN_URL.to_string()],
        credential_ttl_secs: TURN_TTL_SECS,
    }
}

#[test]
fn ice_ordering_fixtures_are_source_distinguishable() {
    assert_ne!(
        STATIC_STUN_URL, TURN_STUN_URL,
        "static and [turn] STUN fixture URLs must stay distinct so ordering assertions catch swaps"
    );
}

/// Disabled `[turn]` block that still advertises public STUN.
fn stun_only_turn_config() -> TurnConfig {
    TurnConfig {
        enabled: false,
        ..enabled_turn_config()
    }
}

/// Start a server with the given session + turn config, keeping the server
/// handle for metrics assertions.
async fn start_server_with(
    session: SessionConfig,
    turn: TurnConfig,
) -> (RunningTestServer, Arc<EnhancedGameServer>) {
    let mut server_config: ServerConfig = test_server_config();
    server_config.app_id_allowlist_enabled = true;

    let mut protocol_config = test_protocol_config();
    protocol_config.sdk_compatibility.enforce = false;

    let game_server = EnhancedGameServer::new(
        server_config,
        protocol_config,
        signal_fish_server::config::RelayTypeConfig::default(),
        session,
        turn,
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
    let enhanced_router = create_router("http://localhost:3000").with_state(game_server.clone());
    let combined_router = axum::Router::new()
        .nest("/v2", enhanced_router)
        .route("/v3/ws", websocket_route_v3("http://localhost:3000"))
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

/// Read raw text frames until the next `ServerMessage`-shaped frame and assert
/// its `type` tag is `expected_type`, returning the **raw JSON text** so wire
/// shape (field presence/absence) can be asserted without deserialization.
/// Non-text control frames are skipped; an unexpected message type is a hard
/// failure (no silent skipping of protocol frames).
async fn next_raw_server_message_of_type(
    ws: &mut WsStream,
    expected_type: &str,
    context: &str,
) -> String {
    let deadline = deadline_after(SERVER_MESSAGE_TIMEOUT);
    loop {
        let frame = tokio::time::timeout_at(deadline, ws.next())
            .await
            .unwrap_or_else(|_| {
                panic!("{context}: timed out waiting for a {expected_type} text frame")
            })
            .unwrap_or_else(|| panic!("{context}: websocket stream closed"))
            .unwrap_or_else(|err| panic!("{context}: websocket error: {err}"));
        let Message::Text(text) = frame else {
            // Control frames (ping/pong) are transport noise, not protocol drift.
            continue;
        };
        let value: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|err| panic!("{context}: invalid JSON text frame: {err}; {text:?}"));
        let actual_type = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("{context}: text frame without a type tag: {text:?}"));
        assert_eq!(
            actual_type, expected_type,
            "{context}: expected the next protocol frame to be {expected_type}, got {actual_type}: {text}"
        );
        return text.to_string();
    }
}

fn raw_server_message_value(raw: &str, context: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap_or_else(|err| {
        panic!("{context}: invalid JSON server frame: {err}; raw frame: {raw}")
    })
}

fn assert_no_ice_servers_data_field(raw: &str, context: &str) {
    let value = raw_server_message_value(raw, context);
    let data = value
        .get("data")
        .and_then(serde_json::Value::as_object)
        .unwrap_or_else(|| panic!("{context}: expected a JSON object at /data: {raw}"));
    assert!(
        !data.contains_key("ice_servers"),
        "{context}: /data/ice_servers must be absent: {raw}"
    );
}

#[test]
fn ice_server_absence_check_ignores_nested_missed_events() {
    let raw = r#"{
        "type": "Reconnected",
        "data": {
            "player_id": "00000000-0000-0000-0000-000000000001",
            "missed_events": [
                {
                    "type": "SessionPlan",
                    "data": {
                        "ice_servers": [
                            { "urls": ["stun:nested.example.com:3478"] }
                        ]
                    }
                }
            ]
        }
    }"#;

    assert_no_ice_servers_data_field(raw, "nested missed_events fixture");
}

/// Authenticate, advertising the given protocol version + capabilities.
async fn authenticate(
    ws: &mut WsStream,
    protocol_version: Option<u16>,
    transports: Option<Vec<Transport>>,
    topologies: Option<Vec<Topology>>,
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
    assert!(
        matches!(info, ServerMessage::ProtocolInfo(_)),
        "expected ProtocolInfo, got {info:?}"
    );
}

/// Authenticate as a v3 mesh+webrtc client.
async fn authenticate_v3_mesh(ws: &mut WsStream) {
    authenticate(
        ws,
        Some(3),
        Some(vec![Transport::Relay, Transport::WebRtc]),
        Some(vec![Topology::Relay, Topology::Mesh]),
    )
    .await;
}

/// Authenticate as a v3 client that advertises ONLY the relay transport.
async fn authenticate_v3_relay_only_transport(ws: &mut WsStream) {
    authenticate(
        ws,
        Some(3),
        Some(vec![Transport::Relay]),
        Some(vec![Topology::Relay, Topology::Mesh]),
    )
    .await;
}

/// Authenticate as a v3 client that advertises the WebRTC transport but ONLY
/// the relay topology.
async fn authenticate_v3_relay_only_topology(ws: &mut WsStream) {
    authenticate(
        ws,
        Some(3),
        Some(vec![Transport::Relay, Transport::WebRtc]),
        Some(vec![Topology::Relay]),
    )
    .await;
}

/// Authenticate as a pure v2 client (no v3 fields beyond the version).
async fn authenticate_v2(ws: &mut WsStream) {
    authenticate(ws, Some(2), None, None).await;
}

fn join_message(game_name: &str, room_code: Option<String>, player_name: &str) -> ClientMessage {
    ClientMessage::JoinRoom {
        game_name: game_name.to_string(),
        room_code,
        player_name: player_name.to_string(),
        max_players: Some(2),
        supports_authority: Some(false),
        relay_transport: None,
    }
}

/// Join (or create) a 2-player room, returning the typed payload.
async fn join_room(
    ws: &mut WsStream,
    game_name: &str,
    room_code: Option<String>,
    player_name: &str,
) -> Box<signal_fish_server::protocol::RoomJoinedPayload> {
    send(ws, &join_message(game_name, room_code, player_name)).await;

    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "room join response",
        |message| match message {
            ServerMessage::RoomJoined(payload) => Some(payload),
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                panic!("room join failed: {reason} ({error_code:?})")
            }
            _ => None,
        },
    )
    .await
}

/// Join and capture the RoomJoined response as a raw JSON frame.
async fn join_room_raw(
    ws: &mut WsStream,
    game_name: &str,
    room_code: Option<String>,
    player_name: &str,
) -> String {
    send(ws, &join_message(game_name, room_code, player_name)).await;
    next_raw_server_message_of_type(ws, "RoomJoined", "raw room join response").await
}

async fn ready(ws: &mut WsStream) {
    send(ws, &ClientMessage::PlayerReady).await;
}

/// Drain messages until a `LobbyStateChanged` reports exactly `count` ready
/// players (paces the ready handshake; mirrors `v3_session_plan_e2e.rs`).
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

type SessionPlan = Box<signal_fish_server::protocol::SessionPlanPayload>;

/// Read until `GameStarting`, then collect the following `SessionPlan` (if
/// any) within a short window. Mirrors `v3_session_plan_e2e.rs`.
async fn read_finalization(ws: &mut WsStream) -> Option<SessionPlan> {
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

    let (session_plan, _skipped) = maybe_next_matching_server_message_with_skipped_until(
        ws,
        deadline_after(POST_GAME_STARTING_SESSION_PLAN_WINDOW),
        "post-GameStarting SessionPlan",
        |message| match message {
            ServerMessage::SessionPlan(plan) => Some(plan),
            _ => None,
        },
    )
    .await;
    session_plan
}

/// Drive a fresh 2-player room to finalization. Returns
/// `(room_id, room_code, peer1_id, peer2_id)`; both sockets have consumed
/// their `GameStarting` + `SessionPlan`.
async fn finalize_two_player_room(
    peer1: &mut WsStream,
    peer2: &mut WsStream,
    game_name: &str,
) -> (RoomId, String, PlayerId, PlayerId) {
    let joined1 = join_room(peer1, game_name, None, "PeerOne").await;
    let joined2 = join_room(peer2, game_name, Some(joined1.room_code.clone()), "PeerTwo").await;

    ready(peer1).await;
    await_ready_count(peer1, 1).await;
    await_ready_count(peer2, 1).await;
    ready(peer2).await;
    await_ready_count(peer1, 2).await;
    await_ready_count(peer2, 2).await;

    // Readiness no longer auto-starts: an explicit StartGame (any member, since
    // these rooms are created with supports_authority: false) finalizes once all
    // current players are ready.
    send(peer1, &ClientMessage::StartGame).await;

    let (plan1, plan2) = tokio::join!(read_finalization(peer1), read_finalization(peer2));
    assert!(
        plan1.is_some() && plan2.is_some(),
        "mesh room must finalize to a WebRTC SessionPlan for both members"
    );

    (
        joined1.room_id,
        joined1.room_code.clone(),
        joined1.player_id,
        joined2.player_id,
    )
}

/// Poll a counter until it reaches `expected` (bounded): counters on emission
/// paths are incremented just after the message send, so the client can observe
/// the frame a beat before the server task resumes. Timing out = real bug.
async fn assert_counter_eventually(read: impl Fn() -> u64, expected: u64, context: &str) {
    let deadline = deadline_after(tokio::time::Duration::from_secs(5));
    loop {
        let actual = read();
        if actual == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{context}: expected counter to reach {expected}, still {actual}"
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }
}

/// Disconnect `player_id` server-side and mint a reconnect token for it
/// (mirrors `v3_signaling_e2e.rs`).
async fn register_reconnect_token(
    game_server: &Arc<EnhancedGameServer>,
    player_id: PlayerId,
    room_id: RoomId,
) -> String {
    let room = game_server
        .database()
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup")
        .expect("room exists");
    let player_info = room
        .players
        .get(&player_id)
        .cloned()
        .expect("player in room before disconnect");

    game_server.disconnect_client(&player_id).await;

    game_server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(player_id, room_id, false, Some(player_info), 0)
        .await
}

// ---------------------------------------------------------------------------
// 1. Happy path: composed list + per-player TURN credential + metrics.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn v3_webrtc_join_carries_composed_ice_servers_with_minted_turn_credential() {
    let (running_server, game_server) =
        start_server_with(mesh_session_config(), enabled_turn_config()).await;
    let addr = running_server.addr();

    let mut peer = connect(addr).await;
    authenticate_v3_mesh(&mut peer).await;
    let now_before = chrono::Utc::now().timestamp();
    let joined = join_room(&mut peer, "mesh-game", None, "PeerOne").await;
    let now_after = chrono::Utc::now().timestamp();

    let ice = &joined.ice_servers;
    assert_eq!(ice.len(), 3, "static + STUN + TURN expected, got {ice:?}");

    // Operator's static entry first, verbatim and credential-less.
    assert_eq!(ice[0].urls, vec![STATIC_STUN_URL]);
    assert!(ice[0].username.is_none() && ice[0].credential.is_none());

    // [turn] STUN entry next, credential-less.
    assert_eq!(ice[1].urls, vec![TURN_STUN_URL]);
    assert!(ice[1].username.is_none() && ice[1].credential.is_none());

    // Freshly minted TURN entry last: username is "{expiry}:{player_id}" for
    // THIS player, credential decodes as standard base64.
    assert_eq!(ice[2].urls, vec![TURN_URL]);
    let username = ice[2].username.as_deref().expect("TURN username");
    let (expiry, id) = username.split_once(':').expect("expiry:player_id shape");
    assert_eq!(id, joined.player_id.to_string());
    let expiry: i64 = expiry.parse().expect("expiry is unix seconds");
    let ttl = i64::try_from(TURN_TTL_SECS).expect("ttl fits i64");
    assert!(
        (now_before + ttl..=now_after + ttl).contains(&expiry),
        "expiry {expiry} must be join-time + ttl"
    );
    let credential = ice[2].credential.as_deref().expect("TURN credential");
    assert!(
        BASE64.decode(credential).is_ok(),
        "credential must be valid standard base64: {credential:?}"
    );

    // Metrics: one carrying payload => one pre-gather emission; the minted
    // TURN credential counts on the shared issuance total.
    let metrics = game_server.metrics();
    assert_eq!(metrics.ice_pregather_emitted.load(Ordering::Relaxed), 1);
    assert_eq!(metrics.turn_credentials_issued.load(Ordering::Relaxed), 1);
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 2.-7. Gating: every ineligible joiner sees NO ice_servers key at all.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn v2_client_room_joined_raw_json_has_no_ice_servers_key() {
    let (running_server, _server) =
        start_server_with(mesh_session_config(), enabled_turn_config()).await;
    let addr = running_server.addr();

    let mut peer = connect(addr).await;
    authenticate_v2(&mut peer).await;
    let raw = join_room_raw(&mut peer, "mesh-game", None, "LegacyPeer").await;

    assert_no_ice_servers_data_field(
        &raw,
        "v2 RoomJoined must stay byte-identical (no ice_servers key)",
    );
    running_server.shutdown().await;
}

#[tokio::test]
async fn v3_relay_only_transport_client_gets_no_ice_servers_key() {
    let (running_server, _server) =
        start_server_with(mesh_session_config(), enabled_turn_config()).await;
    let addr = running_server.addr();

    let mut peer = connect(addr).await;
    authenticate_v3_relay_only_transport(&mut peer).await;
    let raw = join_room_raw(&mut peer, "mesh-game", None, "RelayOnly").await;

    assert_no_ice_servers_data_field(
        &raw,
        "a relay-only transport set cannot use ICE; no key expected",
    );
    running_server.shutdown().await;
}

#[tokio::test]
async fn v3_relay_only_topology_client_gets_no_ice_servers_key() {
    let (running_server, _server) =
        start_server_with(mesh_session_config(), enabled_turn_config()).await;
    let addr = running_server.addr();

    let mut peer = connect(addr).await;
    authenticate_v3_relay_only_topology(&mut peer).await;
    let raw = join_room_raw(&mut peer, "mesh-game", None, "RelayTopology").await;

    assert_no_ice_servers_data_field(
        &raw,
        "a topology set missing the desired one can never seat the desired WebRTC rung; no key expected",
    );
    running_server.shutdown().await;
}

#[tokio::test]
async fn pregather_kill_switch_suppresses_ice_servers_key() {
    let session = SessionConfig {
        enable_ice_pregather: false,
        ..mesh_session_config()
    };
    let (running_server, _server) = start_server_with(session, enabled_turn_config()).await;
    let addr = running_server.addr();

    let mut peer = connect(addr).await;
    authenticate_v3_mesh(&mut peer).await;
    let raw = join_room_raw(&mut peer, "mesh-game", None, "PeerOne").await;

    assert_no_ice_servers_data_field(
        &raw,
        "enable_ice_pregather=false is the kill switch; no key expected",
    );
    running_server.shutdown().await;
}

#[tokio::test]
async fn webrtc_disabled_suppresses_ice_servers_key() {
    let session = SessionConfig {
        enable_webrtc: false,
        ..mesh_session_config()
    };
    let (running_server, _server) = start_server_with(session, enabled_turn_config()).await;
    let addr = running_server.addr();

    let mut peer = connect(addr).await;
    authenticate_v3_mesh(&mut peer).await;
    let raw = join_room_raw(&mut peer, "mesh-game", None, "PeerOne").await;

    assert_no_ice_servers_data_field(
        &raw,
        "with WebRTC disabled no WebRTC plan can ever be selected; no key expected",
    );
    running_server.shutdown().await;
}

#[tokio::test]
async fn relay_desired_game_gets_no_ice_servers_key() {
    let (running_server, _server) =
        start_server_with(mesh_session_config(), enabled_turn_config()).await;
    let addr = running_server.addr();

    let mut peer = connect(addr).await;
    authenticate_v3_mesh(&mut peer).await;
    // "relay-game" is mapped to the relay topology in mesh_session_config().
    let raw = join_room_raw(&mut peer, "relay-game", None, "PeerOne").await;

    assert_no_ice_servers_data_field(
        &raw,
        "a relay-desired game can never select a WebRTC plan; no key expected",
    );
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 8. TURN disabled but STUN configured: STUN-only pre-gather (no credentials).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn turn_disabled_with_stun_urls_yields_stun_only_pregather() {
    let session = SessionConfig {
        ice_servers: Vec::new(),
        ..mesh_session_config()
    };
    let (running_server, game_server) = start_server_with(session, stun_only_turn_config()).await;
    let addr = running_server.addr();

    let mut peer = connect(addr).await;
    authenticate_v3_mesh(&mut peer).await;
    let joined = join_room(&mut peer, "mesh-game", None, "PeerOne").await;

    assert_eq!(
        joined.ice_servers.len(),
        1,
        "only the [turn] block's STUN entry expected: {:?}",
        joined.ice_servers
    );
    assert_eq!(joined.ice_servers[0].urls, vec![TURN_STUN_URL]);
    assert!(
        joined.ice_servers[0].username.is_none() && joined.ice_servers[0].credential.is_none(),
        "no credentials are minted when the [turn] block is disabled"
    );

    let metrics = game_server.metrics();
    assert_eq!(
        metrics.ice_pregather_emitted.load(Ordering::Relaxed),
        1,
        "a STUN-only list still counts as a carrying payload"
    );
    assert_eq!(
        metrics.turn_credentials_issued.load(Ordering::Relaxed),
        0,
        "nothing is minted for a STUN-only pre-gather"
    );
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 9. Late join into an ACTIVE session: pre-gather is OFF (Finalized); the
//    authoritative plan refresh issues one credential per v3 recipient.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn late_join_into_active_session_mints_only_via_the_session_plan() {
    let (running_server, game_server) =
        start_server_with(mesh_session_config(), enabled_turn_config()).await;
    let addr = running_server.addr();

    let mut peer1 = connect(addr).await;
    authenticate_v3_mesh(&mut peer1).await;
    let mut peer2 = connect(addr).await;
    authenticate_v3_mesh(&mut peer2).await;
    let (_room_id, room_code, peer1_id, _peer2_id) =
        finalize_two_player_room(&mut peer1, &mut peer2, "mesh-game").await;

    // Open a seat in the finalized room.
    send(&mut peer2, &ClientMessage::LeaveRoom).await;
    next_matching_server_message_within(
        &mut peer2,
        SERVER_MESSAGE_TIMEOUT,
        "leaver's RoomLeft",
        |message| matches!(message, ServerMessage::RoomLeft).then_some(()),
    )
    .await;
    next_matching_server_message_within(
        &mut peer1,
        SERVER_MESSAGE_TIMEOUT,
        "remaining member's PlayerLeft",
        |message| matches!(message, ServerMessage::PlayerLeft { .. }).then_some(()),
    )
    .await;

    let metrics = game_server.metrics();
    let emitted_before = metrics.ice_pregather_emitted.load(Ordering::Relaxed);
    let creds_before = metrics.turn_credentials_issued.load(Ordering::Relaxed);

    // Seat-fill the active session: RoomJoined must carry NO ice_servers key
    // (pre-gather is gated off by Finalized) ...
    let mut joiner = connect(addr).await;
    authenticate_v3_mesh(&mut joiner).await;
    let raw_joined = join_room_raw(&mut joiner, "mesh-game", Some(room_code), "SeatFiller").await;
    assert_no_ice_servers_data_field(
        &raw_joined,
        "a join into a Finalized room must not pre-gather",
    );
    let joiner_id: PlayerId = raw_server_message_value(&raw_joined, "raw RoomJoined")
        .pointer("/data/player_id")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .expect("player_id present");

    // ... while the immediately following late-join SessionPlan carries the
    // fresh ICE (including this joiner's own minted TURN credential).
    let plan = next_matching_server_message_within(
        &mut joiner,
        SERVER_MESSAGE_TIMEOUT,
        "late-join SessionPlan",
        |message| match message {
            ServerMessage::SessionPlan(plan) => Some(plan),
            _ => None,
        },
    )
    .await;
    assert_eq!(plan.transport, Transport::WebRtc);
    assert_eq!(plan.peers.len(), 1, "the remaining member is the only peer");
    assert_eq!(plan.peers[0].player_id, peer1_id);
    let turn_username = plan
        .ice_servers
        .iter()
        .find_map(|server| server.username.as_deref())
        .expect("late-join SessionPlan carries a minted TURN credential");
    assert!(
        turn_username.ends_with(&format!(":{joiner_id}")),
        "the SessionPlan credential embeds the joiner's id: {turn_username}"
    );

    // Finalized membership changes refresh every v3 member, so the incumbent
    // also receives a plan with a recipient-bound credential.
    let incumbent_plan = next_matching_server_message_within(
        &mut peer1,
        SERVER_MESSAGE_TIMEOUT,
        "incumbent late-join SessionPlan refresh",
        |message| match message {
            ServerMessage::SessionPlan(plan)
                if plan.peers.iter().any(|peer| peer.player_id == joiner_id) =>
            {
                Some(plan)
            }
            _ => None,
        },
    )
    .await;
    let incumbent_username = incumbent_plan
        .ice_servers
        .iter()
        .find_map(|server| server.username.as_deref())
        .expect("incumbent refresh carries a minted TURN credential");
    assert!(
        incumbent_username.ends_with(&format!(":{peer1_id}")),
        "the incumbent SessionPlan credential embeds its recipient id: {incumbent_username}"
    );

    // One logical join event => one fresh credential per v3 plan recipient and
    // no pre-gather emission.
    assert_counter_eventually(
        || metrics.turn_credentials_issued.load(Ordering::Relaxed),
        creds_before + 2,
        "late-join TURN issuance",
    )
    .await;
    assert_eq!(
        metrics.ice_pregather_emitted.load(Ordering::Relaxed),
        emitted_before,
        "pre-gather must not fire for a join into a Finalized room"
    );
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 10. Reconnects: lobby reconnect pre-gathers; active-session reconnect does
//     not (the fresh SessionPlan that follows carries the ICE).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reconnect_while_room_in_lobby_carries_pregathered_ice_servers() {
    let (running_server, game_server) =
        start_server_with(mesh_session_config(), enabled_turn_config()).await;
    let addr = running_server.addr();

    let mut peer1 = connect(addr).await;
    authenticate_v3_mesh(&mut peer1).await;
    let joined1 = join_room(&mut peer1, "mesh-game", None, "PeerOne").await;

    let token = register_reconnect_token(&game_server, joined1.player_id, joined1.room_id).await;
    let _ = peer1.close(None).await;

    let mut replacement = connect(addr).await;
    authenticate_v3_mesh(&mut replacement).await;
    send(
        &mut replacement,
        &ClientMessage::Reconnect {
            player_id: joined1.player_id,
            room_id: joined1.room_id,
            auth_token: token,
        },
    )
    .await;

    let reconnected = next_matching_server_message_within(
        &mut replacement,
        SERVER_MESSAGE_TIMEOUT,
        "reconnect response",
        |message| match message {
            ServerMessage::Reconnected(payload) => Some(payload),
            ServerMessage::ReconnectionFailed { reason, error_code } => {
                panic!("reconnect failed: {reason} ({error_code:?})")
            }
            _ => None,
        },
    )
    .await;

    // Non-finalized room: the reconnector can pre-gather again (its previous
    // credentials may have expired while it was away).
    assert_eq!(
        reconnected.ice_servers.len(),
        3,
        "lobby reconnect pre-gathers static + STUN + TURN: {:?}",
        reconnected.ice_servers
    );
    let username = reconnected
        .ice_servers
        .iter()
        .find_map(|server| server.username.as_deref())
        .expect("reconnect pre-gather carries a minted TURN credential");
    assert!(
        username.ends_with(&format!(":{}", joined1.player_id)),
        "credential embeds the reconnector's restored id: {username}"
    );
    running_server.shutdown().await;
}

#[tokio::test]
async fn reconnect_into_active_session_pregathers_nothing_but_plan_carries_ice() {
    let (running_server, game_server) =
        start_server_with(mesh_session_config(), enabled_turn_config()).await;
    let addr = running_server.addr();

    let mut peer1 = connect(addr).await;
    authenticate_v3_mesh(&mut peer1).await;
    let mut peer2 = connect(addr).await;
    authenticate_v3_mesh(&mut peer2).await;
    let (room_id, _room_code, peer1_id, peer2_id) =
        finalize_two_player_room(&mut peer1, &mut peer2, "mesh-game").await;

    let token = register_reconnect_token(&game_server, peer2_id, room_id).await;
    let _ = peer2.close(None).await;

    let metrics = game_server.metrics();
    let emitted_before = metrics.ice_pregather_emitted.load(Ordering::Relaxed);
    let creds_before = metrics.turn_credentials_issued.load(Ordering::Relaxed);

    let mut replacement = connect(addr).await;
    authenticate_v3_mesh(&mut replacement).await;
    send(
        &mut replacement,
        &ClientMessage::Reconnect {
            player_id: peer2_id,
            room_id,
            auth_token: token,
        },
    )
    .await;

    // Reconnected first, with NO ice_servers key (Finalized room), ...
    let raw_reconnected =
        next_raw_server_message_of_type(&mut replacement, "Reconnected", "raw reconnect response")
            .await;
    assert_no_ice_servers_data_field(
        &raw_reconnected,
        "a reconnect into a Finalized room must not pre-gather",
    );

    // ... then the fresh late-join SessionPlan carries the ICE.
    let plan = next_matching_server_message_within(
        &mut replacement,
        SERVER_MESSAGE_TIMEOUT,
        "post-reconnect SessionPlan",
        |message| match message {
            ServerMessage::SessionPlan(plan) => Some(plan),
            _ => None,
        },
    )
    .await;
    let username = plan
        .ice_servers
        .iter()
        .find_map(|server| server.username.as_deref())
        .expect("post-reconnect SessionPlan carries fresh TURN credentials");
    assert!(
        username.ends_with(&format!(":{peer2_id}")),
        "fresh credential embeds the reconnector's id: {username}"
    );

    // The incumbent receives the same authoritative membership refresh with a
    // credential bound to its own player id.
    let incumbent_plan = next_matching_server_message_within(
        &mut peer1,
        SERVER_MESSAGE_TIMEOUT,
        "incumbent reconnect SessionPlan refresh",
        |message| match message {
            ServerMessage::SessionPlan(plan)
                if plan.peers.iter().any(|peer| peer.player_id == peer2_id) =>
            {
                Some(plan)
            }
            _ => None,
        },
    )
    .await;
    let incumbent_username = incumbent_plan
        .ice_servers
        .iter()
        .find_map(|server| server.username.as_deref())
        .expect("incumbent reconnect refresh carries fresh TURN credentials");
    assert!(
        incumbent_username.ends_with(&format!(":{peer1_id}")),
        "incumbent credentials must be bound to the incumbent: {incumbent_username}"
    );

    // One logical reconnect event => one credential per v3 plan recipient and
    // zero pre-gather emissions.
    assert_counter_eventually(
        || metrics.turn_credentials_issued.load(Ordering::Relaxed),
        creds_before + 2,
        "reconnect TURN issuance",
    )
    .await;
    assert_eq!(
        metrics.ice_pregather_emitted.load(Ordering::Relaxed),
        emitted_before,
        "pre-gather must not fire for a reconnect into a Finalized room"
    );
    running_server.shutdown().await;
}
