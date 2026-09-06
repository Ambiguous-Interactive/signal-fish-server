//! End-to-end reconnection event-replay tests through the real WebSocket stack.
//!
//! `Reconnected.missed_events` must actually carry the room-uniform control
//! events (PlayerJoined / PlayerLeft / LobbyStateChanged / ...) that were
//! broadcast while the reconnecting player was away — in broadcast order — and
//! the v3-only `replay` field must state whether that list is complete,
//! truncated by the bounded per-room replay ring, or unavailable. High-rate
//! data-path messages (GameData) and per-recipient payloads (GameStarting) are
//! deliberately never replayed: reconnectors resync from the `Reconnected`
//! snapshot and, for started sessions, the dedicated late-join `SessionPlan`.
//!
//! Models the harness in `tests/v3_ice_pregather_e2e.rs` (which already drives
//! real reconnects): same connect/authenticate/join helpers, same
//! `register_reconnect_token` pattern, and raw-JSON frame assertions where the
//! scenario is about wire shape (the v2 wire must never grow a `replay` key).

mod test_helpers;
mod websocket_test_helpers;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::config::AppRegistrationEntry;
use signal_fish_server::protocol::{ClientMessage, PlayerId, ReplayStatus, RoomId, ServerMessage};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::{create_router, websocket_route_v3};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use test_helpers::{test_protocol_config, test_server_config, RunningTestServer};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::{
    assert_message_conservation, deadline_after, next_matching_server_message_within,
    next_server_message_within, WsStream,
};

const APP_ID: &str = "reconnection-replay-app";
const SERVER_MESSAGE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(20);

fn app_entry() -> AppRegistrationEntry {
    AppRegistrationEntry {
        app_id: APP_ID.to_string(),
        app_name: "Reconnection Replay App".to_string(),
        max_rooms: Some(10),
        max_players_per_room: Some(8),
        rate_limit_per_minute: Some(600),
        max_relay_bytes: None,
    }
}

/// Start a server with the given `ServerConfig` (lets a test shrink the replay
/// ring via `event_buffer_size`), keeping the handle for metrics assertions.
async fn start_server_with_config(
    server_config: ServerConfig,
) -> (RunningTestServer, Arc<EnhancedGameServer>) {
    let mut server_config = server_config;
    server_config.app_id_allowlist_enabled = true;

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

async fn start_server_default() -> (RunningTestServer, Arc<EnhancedGameServer>) {
    start_server_with_config(test_server_config()).await
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

/// Authenticate, advertising the given protocol version (relay-only capability
/// set — replay is about control events, not the data path).
async fn authenticate(ws: &mut WsStream, protocol_version: Option<u16>) {
    send(
        ws,
        &ClientMessage::Authenticate {
            app_id: APP_ID.to_string(),
            sdk_version: None,
            platform: None,
            game_data_format: None,
            protocol_version,
            supported_transports: None,
            supported_topologies: None,
            requested_capabilities: None,
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

async fn authenticate_v3(ws: &mut WsStream) {
    authenticate(ws, Some(3)).await;
}

/// Authenticate as a pure v2 client (no v3 fields beyond the version).
async fn authenticate_v2(ws: &mut WsStream) {
    authenticate(ws, Some(2)).await;
}

/// Join (or create) a 4-player room, returning the typed payload.
async fn join_room(
    ws: &mut WsStream,
    game_name: &str,
    room_code: Option<String>,
    player_name: &str,
) -> Box<signal_fish_server::protocol::RoomJoinedPayload> {
    send(
        ws,
        &ClientMessage::JoinRoom {
            game_name: game_name.to_string(),
            room_code,
            player_name: player_name.to_string(),
            max_players: Some(4),
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
            ServerMessage::RoomJoined(payload) => Some(payload),
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                panic!("room join failed: {reason} ({error_code:?})")
            }
            _ => None,
        },
    )
    .await
}

/// Disconnect `player_id` server-side and mint a reconnect token for it
/// (mirrors `tests/v3_ice_pregather_e2e.rs`).
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
        // `last_epoch = 0`: `disconnect_client` above already removed the
        // connection, so its incarnation epoch is no longer reachable via
        // `game_data_epoch` — the documented fallback (resume at epoch 1). This
        // e2e reconnects over a fresh socket and asserts replay/snapshot
        // behavior, not cross-disconnect epoch monotonicity.
        .register_disconnection(player_id, room_id, false, Some(player_info), 0)
        .await
}

/// Send `Reconnect` on `ws` and return the typed `Reconnected` payload.
async fn reconnect(
    ws: &mut WsStream,
    player_id: PlayerId,
    room_id: RoomId,
    auth_token: String,
) -> Box<signal_fish_server::protocol::ReconnectedPayload> {
    send(
        ws,
        &ClientMessage::Reconnect {
            player_id,
            room_id,
            auth_token,
        },
    )
    .await;

    next_matching_server_message_within(
        ws,
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
    .await
}

/// Drain `ws` until it has observed `PlayerJoined` then `PlayerLeft` for
/// `expected`: once a connected member saw the broadcasts, the server has
/// already recorded them for any pending reconnector (events are buffered
/// before they are broadcast), so this is the event-driven sync point.
async fn await_join_leave_cycle(ws: &mut WsStream, expected: PlayerId, context: &str) {
    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        context,
        |message| match message {
            ServerMessage::PlayerJoined { player } if player.id == expected => Some(()),
            _ => None,
        },
    )
    .await;
    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        context,
        |message| match message {
            ServerMessage::PlayerLeft { player_id, .. } if player_id == expected => Some(()),
            _ => None,
        },
    )
    .await;
}

/// Join `churn_name` into the room and immediately leave, waiting for both the
/// leaver's own `RoomLeft` and the observer's broadcast pair, so every control
/// event of the cycle is fully processed before the caller proceeds.
async fn churn_join_leave(
    addr: std::net::SocketAddr,
    room_code: &str,
    churn_name: &str,
    observer: &mut WsStream,
) -> PlayerId {
    let mut churner = connect(addr).await;
    authenticate_v3(&mut churner).await;
    let joined = join_room(
        &mut churner,
        "replay-game",
        Some(room_code.to_string()),
        churn_name,
    )
    .await;
    send(&mut churner, &ClientMessage::LeaveRoom).await;
    next_matching_server_message_within(
        &mut churner,
        SERVER_MESSAGE_TIMEOUT,
        "churner's RoomLeft",
        |message| matches!(message, ServerMessage::RoomLeft).then_some(()),
    )
    .await;
    await_join_leave_cycle(observer, joined.player_id, "observer's churn broadcasts").await;
    joined.player_id
}

// ---------------------------------------------------------------------------
// 1. Happy path: every control event broadcast while away is replayed, in
//    order, and the replay is reported complete.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn missed_control_events_are_replayed_completely() {
    let (running_server, game_server) = start_server_default().await;
    let addr = running_server.addr();

    let mut peer_a = connect(addr).await;
    authenticate_v3(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, "replay-game", None, "PeerA").await;

    let mut peer_b = connect(addr).await;
    authenticate_v3(&mut peer_b).await;
    let joined_b = join_room(
        &mut peer_b,
        "replay-game",
        Some(joined_a.room_code.clone()),
        "PeerB",
    )
    .await;

    // B drops; a reconnection is now pending, so room control events buffer.
    let token = register_reconnect_token(&game_server, joined_b.player_id, joined_b.room_id).await;
    let _ = peer_b.close(None).await;

    // While B is away, a third player joins and leaves.
    let churner_id = churn_join_leave(addr, &joined_a.room_code, "Churner", &mut peer_a).await;

    // B reconnects on a fresh v3 socket.
    let mut replacement = connect(addr).await;
    authenticate_v3(&mut replacement).await;
    let reconnected = reconnect(
        &mut replacement,
        joined_b.player_id,
        joined_b.room_id,
        token,
    )
    .await;

    // The missed control events arrive verbatim, in broadcast order.
    assert_eq!(
        reconnected.missed_events.len(),
        2,
        "exactly the churn cycle's two control events must be replayed: {:?}",
        reconnected.missed_events
    );
    assert!(
        matches!(
            &reconnected.missed_events[0],
            ServerMessage::PlayerJoined { player } if player.id == churner_id
        ),
        "first missed event must be the churner's PlayerJoined: {:?}",
        reconnected.missed_events
    );
    // The replay ring stores the churner's PlayerJoined in its live-broadcast
    // form, carrying the v3 incarnation `epoch`. This reconnector negotiated v3,
    // so that epoch is PRESERVED in the replay (a v3 client wants it). The pre-v3
    // (v2) strip — which keeps a v2 reconnector's `Reconnected` byte-identical to
    // the frozen v2 wire — is exercised by the companion test below.
    let ServerMessage::PlayerJoined { player } = &reconnected.missed_events[0] else {
        unreachable!("asserted PlayerJoined above");
    };
    assert_eq!(
        player.epoch,
        Some(1),
        "a v3 reconnector's replayed PlayerJoined carries the churner's epoch (first join ⇒ 1)"
    );
    assert!(
        matches!(
            &reconnected.missed_events[1],
            ServerMessage::PlayerLeft { player_id, .. } if *player_id == churner_id
        ),
        "second missed event must be the churner's PlayerLeft: {:?}",
        reconnected.missed_events
    );
    assert_eq!(
        reconnected.replay,
        Some(ReplayStatus::Complete),
        "nothing was evicted, so the v3 recipient is told the replay is complete"
    );

    assert_message_conservation(&game_server.metrics()).await;
    running_server.shutdown().await;
}

/// Regression (missed_events epoch leak) + non-vacuity for the previous test:
/// the replay ring stores the churner's PlayerJoined in its live-broadcast form,
/// carrying the v3 incarnation `epoch`. `missed_events` is embedded in the
/// `Reconnected` payload and bypasses the per-recipient strip in
/// `websocket::sending`, so a PRE-v3 (v2) reconnector must have that `epoch`
/// stripped server-side — otherwise its `Reconnected` wire is no longer
/// byte-identical to the frozen v2 wire. (A v2 socket reaches this path by
/// reconnecting with a token minted for its earlier v3 incarnation.) Deserialized
/// `None` proves wire-absence; that the v3 reconnector above KEEPS the same
/// epoch proves the strip is CONDITIONED on the reconnector being pre-v3, not an
/// unconditional wipe.
#[tokio::test]
async fn missed_events_strip_epoch_for_pre_v3_reconnector() {
    let (running_server, game_server) = start_server_default().await;
    let addr = running_server.addr();

    let mut peer_a = connect(addr).await;
    authenticate(&mut peer_a, Some(3)).await;
    let joined_a = join_room(&mut peer_a, "replay-game", None, "PeerA").await;

    let mut peer_b = connect(addr).await;
    authenticate(&mut peer_b, Some(3)).await;
    let joined_b = join_room(
        &mut peer_b,
        "replay-game",
        Some(joined_a.room_code.clone()),
        "PeerB",
    )
    .await;

    let token = register_reconnect_token(&game_server, joined_b.player_id, joined_b.room_id).await;
    let _ = peer_b.close(None).await;

    // While B is away, a churner joins+leaves; its PlayerJoined is buffered with
    // the churner's incarnation epoch (first join ⇒ 1).
    let churner_id = churn_join_leave(addr, &joined_a.room_code, "Churner", &mut peer_a).await;

    // Reconnect on a PRE-v3 (v2) socket: the replayed epoch must be stripped so
    // the v2 wire stays byte-identical.
    let mut replacement = connect(addr).await;
    authenticate(&mut replacement, Some(2)).await;
    let reconnected = reconnect(
        &mut replacement,
        joined_b.player_id,
        joined_b.room_id,
        token,
    )
    .await;

    let ServerMessage::PlayerJoined { player } = &reconnected.missed_events[0] else {
        panic!(
            "first missed event must be the churner's PlayerJoined: {:?}",
            reconnected.missed_events
        );
    };
    assert_eq!(player.id, churner_id, "first missed event is the churner");
    assert_eq!(
        player.epoch, None,
        "a pre-v3 (v2) reconnector's replayed PlayerJoined must omit the v3 epoch"
    );

    assert_message_conservation(&game_server.metrics()).await;
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 2. Overflow: more control events than the ring holds => the replay is a
//    suffix and is reported truncated; the eviction metric records the loss.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn overflowed_replay_ring_reports_truncated() {
    let mut config = test_server_config();
    config.event_buffer_size = 3;
    let (running_server, game_server) = start_server_with_config(config).await;
    let addr = running_server.addr();

    let mut peer_a = connect(addr).await;
    authenticate_v3(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, "replay-game", None, "PeerA").await;

    let mut peer_b = connect(addr).await;
    authenticate_v3(&mut peer_b).await;
    let joined_b = join_room(
        &mut peer_b,
        "replay-game",
        Some(joined_a.room_code.clone()),
        "PeerB",
    )
    .await;

    let token = register_reconnect_token(&game_server, joined_b.player_id, joined_b.room_id).await;
    let _ = peer_b.close(None).await;

    // Three join/leave cycles = 6 control events into a 3-slot ring.
    let mut churner_ids = Vec::new();
    for i in 0..3 {
        churner_ids.push(
            churn_join_leave(
                addr,
                &joined_a.room_code,
                &format!("Churner{i}"),
                &mut peer_a,
            )
            .await,
        );
    }

    let mut replacement = connect(addr).await;
    authenticate_v3(&mut replacement).await;
    let reconnected = reconnect(
        &mut replacement,
        joined_b.player_id,
        joined_b.room_id,
        token,
    )
    .await;

    // The ring keeps only the newest 3 of the 6 events: the suffix
    // [PlayerLeft(c1), PlayerJoined(c2), PlayerLeft(c2)] (0-indexed churners).
    assert_eq!(
        reconnected.missed_events.len(),
        3,
        "the replay is capped at the ring size: {:?}",
        reconnected.missed_events
    );
    assert!(
        matches!(
            &reconnected.missed_events[0],
            ServerMessage::PlayerLeft { player_id, .. } if *player_id == churner_ids[1]
        ),
        "oldest surviving event must be the second churner's PlayerLeft: {:?}",
        reconnected.missed_events
    );
    assert!(
        matches!(
            &reconnected.missed_events[1],
            ServerMessage::PlayerJoined { player } if player.id == churner_ids[2]
        ) && matches!(
            &reconnected.missed_events[2],
            ServerMessage::PlayerLeft { player_id, .. } if *player_id == churner_ids[2]
        ),
        "the newest churn cycle must survive intact: {:?}",
        reconnected.missed_events
    );
    assert_eq!(
        reconnected.replay,
        Some(ReplayStatus::Truncated),
        "evicted events the player needed => truncated"
    );
    // 7 events entered the ring, not 6: the production disconnect path
    // (`unregister_client`) opens the replay gate BEFORE `leave_room`
    // broadcasts the leaver's own PlayerLeft, so B's departure is captured
    // too (any OTHER pending reconnector must see it; B itself filters it
    // out via its `last_sequence`). 7 buffered - 3 ring slots = 4 evictions.
    assert_eq!(
        game_server
            .metrics()
            .reconnection_events_evicted
            .load(Ordering::Relaxed),
        4,
        "7 buffered control events minus the 3-slot ring = 4 evictions"
    );

    assert_message_conservation(&game_server.metrics()).await;
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// H7: a truncated replay still hands the reconnector a COMPLETE room
//    snapshot, so it can resync app state even though the event log was lost.
//    Pre-registered prediction: "membership + authority recoverable; ready-state
//    and spectator roster are LIKELY HOLES in ReconnectedPayload." Result:
//    REFUTED — the snapshot already carries `current_players` (each with
//    `is_ready`/`is_authority`), an explicit `ready_players` list, and
//    `current_spectators`. This test pins that: with a ready peer and a live
//    spectator established BEFORE the disconnect (so they are room state, not
//    replay-ring events), the reconnector's TRUNCATED snapshot must still show
//    both. Guards against a future regression that drops those fields.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn truncated_replay_still_carries_full_room_snapshot() {
    let mut config = test_server_config();
    config.event_buffer_size = 3;
    let (running_server, game_server) = start_server_with_config(config).await;
    let addr = running_server.addr();

    let mut peer_a = connect(addr).await;
    authenticate_v3(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, "replay-game", None, "PeerA").await;

    let mut peer_b = connect(addr).await;
    authenticate_v3(&mut peer_b).await;
    let joined_b = join_room(
        &mut peer_b,
        "replay-game",
        Some(joined_a.room_code.clone()),
        "PeerB",
    )
    .await;

    // Establish ready-state + a spectator BEFORE B disconnects, so both are
    // captured in the room snapshot rather than the (truncated) replay ring.
    // PeerA stays connected throughout, so its ready-state is unambiguous — the
    // coordinator ready set is not cleared by membership churn
    // (`room_service.rs`), so the later churn cannot wipe it.
    send(&mut peer_a, &ClientMessage::PlayerReady).await;
    next_matching_server_message_within(
        &mut peer_a,
        SERVER_MESSAGE_TIMEOUT,
        "LobbyStateChanged after PeerA readies",
        |message| matches!(message, ServerMessage::LobbyStateChanged { .. }).then_some(()),
    )
    .await;

    let mut spectator = connect(addr).await;
    authenticate_v3(&mut spectator).await;
    send(
        &mut spectator,
        &ClientMessage::JoinAsSpectator {
            game_name: "replay-game".to_string(),
            room_code: joined_a.room_code.clone(),
            spectator_name: "Spec".to_string(),
        },
    )
    .await;
    let spectator_id = next_matching_server_message_within(
        &mut spectator,
        SERVER_MESSAGE_TIMEOUT,
        "SpectatorJoined ack",
        |message| match message {
            ServerMessage::SpectatorJoined(payload) => {
                assert!(
                    !payload.current_players.is_empty()
                        && payload
                            .current_players
                            .iter()
                            .all(|player| player.epoch.is_some()),
                    "a v3 spectator snapshot must baseline every current sender epoch"
                );
                Some(payload.spectator_id)
            }
            ServerMessage::SpectatorJoinFailed { reason, error_code } => {
                panic!("spectator join failed: {reason} ({error_code:?})")
            }
            _ => None,
        },
    )
    .await;

    // B disconnects with a valid token, then the ring overflows (3 cycles = 6+
    // events into a 3-slot ring) so B's replay is TRUNCATED.
    let token = register_reconnect_token(&game_server, joined_b.player_id, joined_b.room_id).await;
    let _ = peer_b.close(None).await;
    for i in 0..3 {
        churn_join_leave(
            addr,
            &joined_a.room_code,
            &format!("Churner{i}"),
            &mut peer_a,
        )
        .await;
    }

    let mut replacement = connect(addr).await;
    authenticate_v3(&mut replacement).await;
    let reconnected = reconnect(
        &mut replacement,
        joined_b.player_id,
        joined_b.room_id,
        token,
    )
    .await;

    // Precondition: the scenario really is the truncated one this test is about.
    assert_eq!(
        reconnected.replay,
        Some(ReplayStatus::Truncated),
        "the ring overflowed, so the replay must be truncated"
    );

    // Membership: both live players are in the snapshot (churners left).
    let member_ids: Vec<PlayerId> = reconnected.current_players.iter().map(|p| p.id).collect();
    assert!(
        member_ids.contains(&joined_a.player_id) && member_ids.contains(&joined_b.player_id),
        "snapshot membership must include both live players, got {member_ids:?}"
    );

    // Ready-state (the doubted field): PeerA's readiness survives in BOTH the
    // explicit list and the per-player flag.
    assert!(
        reconnected.ready_players.contains(&joined_a.player_id),
        "ready_players must list the ready peer after a truncated reconnect"
    );
    let peer_a_entry = reconnected
        .current_players
        .iter()
        .find(|p| p.id == joined_a.player_id)
        .expect("PeerA present in snapshot");
    assert!(
        peer_a_entry.is_ready,
        "the snapshot's per-player is_ready must reflect readiness"
    );

    // Spectator roster (the other doubted field): the live spectator is present.
    assert!(
        reconnected
            .current_spectators
            .iter()
            .any(|s| s.id == spectator_id),
        "current_spectators must carry the live spectator after a truncated reconnect"
    );

    // The game has not started — a truncated *lobby* reconnect. `PlayerReady`
    // (with no `StartGame`) puts the room in `Lobby`, so pin that exact state
    // rather than merely "not Finalized".
    assert_eq!(
        reconnected.lobby_state,
        signal_fish_server::protocol::LobbyState::Lobby,
        "a readied-but-not-started room must reconnect in the Lobby state"
    );

    assert_message_conservation(&game_server.metrics()).await;
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 3. v2 wire freeze: a v2 recipient's Reconnected frame must not grow a
//    `replay` key (raw-JSON assertion; the events themselves still replay).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn v2_recipient_gets_no_replay_key() {
    let (running_server, game_server) = start_server_default().await;
    let addr = running_server.addr();

    let mut peer_a = connect(addr).await;
    authenticate_v3(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, "replay-game", None, "PeerA").await;

    // B is a pure v2 client.
    let mut peer_b = connect(addr).await;
    authenticate_v2(&mut peer_b).await;
    let joined_b = join_room(
        &mut peer_b,
        "replay-game",
        Some(joined_a.room_code.clone()),
        "PeerB",
    )
    .await;

    let token = register_reconnect_token(&game_server, joined_b.player_id, joined_b.room_id).await;
    let _ = peer_b.close(None).await;

    // A churn cycle proves the replay itself is version-agnostic.
    let _churner_id = churn_join_leave(addr, &joined_a.room_code, "Churner", &mut peer_a).await;

    let mut replacement = connect(addr).await;
    authenticate_v2(&mut replacement).await;
    send(
        &mut replacement,
        &ClientMessage::Reconnect {
            player_id: joined_b.player_id,
            room_id: joined_b.room_id,
            auth_token: token,
        },
    )
    .await;

    let raw =
        next_raw_server_message_of_type(&mut replacement, "Reconnected", "raw v2 reconnect").await;
    let value: serde_json::Value =
        serde_json::from_str(&raw).expect("Reconnected frame is valid JSON");
    let data = value
        .get("data")
        .and_then(serde_json::Value::as_object)
        .unwrap_or_else(|| panic!("expected a JSON object at /data: {raw}"));
    assert!(
        !data.contains_key("replay"),
        "the v2 wire must stay byte-identical: no replay key allowed: {raw}"
    );
    assert!(
        !data.contains_key("sender_watermarks"),
        "the v2 wire must stay byte-identical: no sender_watermarks key allowed: {raw}"
    );
    assert!(
        data.get("missed_events")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|events| !events.is_empty()),
        "the replayed events themselves are not v3-gated: {raw}"
    );

    assert_message_conservation(&game_server.metrics()).await;
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 4. GameData is never replayed: the data path is high-rate and would purge
//    the control events that matter; reconnectors resync via the snapshot.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn game_data_is_never_replayed() {
    let (running_server, game_server) = start_server_default().await;
    let addr = running_server.addr();

    let mut peer_a = connect(addr).await;
    authenticate_v3(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, "replay-game", None, "PeerA").await;

    let mut peer_b = connect(addr).await;
    authenticate_v3(&mut peer_b).await;
    let joined_b = join_room(
        &mut peer_b,
        "replay-game",
        Some(joined_a.room_code.clone()),
        "PeerB",
    )
    .await;

    let token = register_reconnect_token(&game_server, joined_b.player_id, joined_b.room_id).await;
    let _ = peer_b.close(None).await;

    // A streams game data while B is away; a Ping/Pong round-trip on the same
    // socket proves the server processed the sends (same-socket ordering).
    for _ in 0..3 {
        send(
            &mut peer_a,
            &ClientMessage::GameData {
                class: None,
                key: None,
                data: serde_json::json!({ "tick": 1 }),
            },
        )
        .await;
    }
    send(&mut peer_a, &ClientMessage::Ping).await;
    next_matching_server_message_within(
        &mut peer_a,
        SERVER_MESSAGE_TIMEOUT,
        "post-GameData pong",
        |message| matches!(message, ServerMessage::Pong).then_some(()),
    )
    .await;

    // A churn cycle gives the replay a real control event to carry.
    let churner_id = churn_join_leave(addr, &joined_a.room_code, "Churner", &mut peer_a).await;

    let mut replacement = connect(addr).await;
    authenticate_v3(&mut replacement).await;
    let reconnected = reconnect(
        &mut replacement,
        joined_b.player_id,
        joined_b.room_id,
        token,
    )
    .await;

    assert!(
        !reconnected.missed_events.iter().any(|event| matches!(
            event,
            ServerMessage::GameData { .. } | ServerMessage::GameDataBinary { .. }
        )),
        "GameData must never be replayed: {:?}",
        reconnected.missed_events
    );
    assert!(
        reconnected.missed_events.iter().any(|event| matches!(
            event,
            ServerMessage::PlayerJoined { player } if player.id == churner_id
        )),
        "the churn cycle's control events must still be replayed: {:?}",
        reconnected.missed_events
    );
    assert_eq!(
        reconnected.replay,
        Some(ReplayStatus::Complete),
        "skipping GameData is by design, not truncation"
    );
    let watermark_for_a = reconnected
        .sender_watermarks
        .iter()
        .find(|watermark| watermark.player_id == joined_a.player_id)
        .unwrap_or_else(|| {
            panic!(
                "v3 reconnect must baseline the sender whose GameData was skipped: {:?}",
                reconnected.sender_watermarks
            )
        });
    assert_eq!(
        (watermark_for_a.epoch, watermark_for_a.seq),
        (1, 3),
        "the reconnect watermark must report PeerA's authoritative tail after the three skipped GameData frames"
    );
    let watermark_for_b = reconnected
        .sender_watermarks
        .iter()
        .find(|watermark| watermark.player_id == joined_b.player_id)
        .unwrap_or_else(|| {
            panic!(
                "v3 reconnect must include the restored player's fresh baseline: {:?}",
                reconnected.sender_watermarks
            )
        });
    assert_eq!(
        watermark_for_b.seq, 0,
        "the restored player has not relayed GameData in its new incarnation yet"
    );

    assert_message_conservation(&game_server.metrics()).await;
    running_server.shutdown().await;
}

#[tokio::test]
async fn reconnected_baseline_precedes_room_data_during_reconnect() {
    let (running_server, game_server) = start_server_default().await;
    let addr = running_server.addr();

    let mut peer_a = connect(addr).await;
    authenticate_v3(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, "reconnect-order-game", None, "PeerA").await;

    let mut peer_b = connect(addr).await;
    authenticate_v3(&mut peer_b).await;
    let joined_b = join_room(
        &mut peer_b,
        "reconnect-order-game",
        Some(joined_a.room_code.clone()),
        "PeerB",
    )
    .await;

    let token = register_reconnect_token(&game_server, joined_b.player_id, joined_b.room_id).await;
    let _ = peer_b.close(None).await;

    let sender_task = tokio::spawn(async move {
        for n in 0..64u64 {
            send(
                &mut peer_a,
                &ClientMessage::GameData {
                    class: None,
                    key: None,
                    data: serde_json::json!({ "phase": "during-reconnect", "n": n }),
                },
            )
            .await;
            tokio::task::yield_now().await;
        }
        peer_a
    });

    let mut replacement = connect(addr).await;
    authenticate_v3(&mut replacement).await;
    send(
        &mut replacement,
        &ClientMessage::Reconnect {
            player_id: joined_b.player_id,
            room_id: joined_b.room_id,
            auth_token: token,
        },
    )
    .await;

    let first = next_server_message(&mut replacement).await;
    let ServerMessage::Reconnected(reconnected) = first else {
        panic!("Reconnected must be the first room frame after Reconnect, got {first:?}");
    };
    let sender_watermark = reconnected
        .sender_watermarks
        .iter()
        .find(|watermark| watermark.player_id == joined_a.player_id)
        .unwrap_or_else(|| {
            panic!(
                "Reconnected must baseline PeerA before room data resumes: {:?}",
                reconnected.sender_watermarks
            )
        });
    let baseline_seq = sender_watermark.seq;

    let mut peer_a = sender_task.await.expect("sender task panicked");
    send(
        &mut peer_a,
        &ClientMessage::GameData {
            class: None,
            key: None,
            data: serde_json::json!({ "phase": "post-baseline-marker" }),
        },
    )
    .await;

    let first_data_seq = next_matching_server_message_within(
        &mut replacement,
        SERVER_MESSAGE_TIMEOUT,
        "first data after reconnect baseline",
        |message| match message {
            ServerMessage::GameData {
                from_player, seq, ..
            } if from_player == joined_a.player_id => seq,
            ServerMessage::Error {
                message,
                error_code,
            } => panic!("replacement got server error: {message} ({error_code:?})"),
            _ => None,
        },
    )
    .await;
    assert_eq!(
        first_data_seq,
        baseline_seq + 1,
        "the first room data after Reconnected must continue from sender_watermarks"
    );

    assert_message_conservation(&game_server.metrics()).await;
    running_server.shutdown().await;
}

/// Issue #136 (F4 / proposal D): the reconnection token must reach the client
/// on the WIRE at join time — a token minted only at disconnect can never be
/// legitimately obtained, making reconnection unusable outside tests that
/// reach into the server (as every test above this one historically did).
/// This is the full user-visible flow with NO server backdoor: join, capture
/// the token from `RoomJoined`, lose the socket, reconnect with that token.
#[tokio::test]
async fn reconnect_succeeds_with_only_the_wire_token() {
    let (running_server, game_server) = start_server_default().await;
    let addr = running_server.addr();

    let mut peer_a = connect(addr).await;
    authenticate_v3(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, "wire-token-game", None, "Anchor").await;

    let mut peer_b = connect(addr).await;
    authenticate_v3(&mut peer_b).await;
    let joined_b = join_room(
        &mut peer_b,
        "wire-token-game",
        Some(joined_a.room_code.clone()),
        "Dropper",
    )
    .await;
    let wire_token = joined_b
        .reconnection_token
        .clone()
        .expect("a v3 joiner must receive its reconnection token on RoomJoined");

    // Lose the socket abruptly (no LeaveRoom): the server registers the
    // disconnection for reconnect. Event-driven sync: the anchor observes the
    // PlayerLeft broadcast, which the disconnect flow emits after the
    // reconnection record was registered.
    drop(peer_b);
    await_player_left(&mut peer_a, joined_b.player_id, "anchor sees the drop").await;

    // Reconnect with ONLY what the wire provided.
    let mut replacement = connect(addr).await;
    authenticate_v3(&mut replacement).await;
    let reconnected = reconnect(
        &mut replacement,
        joined_b.player_id,
        joined_b.room_id,
        wire_token.clone(),
    )
    .await;

    // Rotation: the consumed token is replaced by a fresh one for the next
    // unexpected disconnect.
    let rotated = reconnected
        .reconnection_token
        .clone()
        .expect("a v3 reconnector must receive a rotated token on Reconnected");
    assert_ne!(
        rotated, wire_token,
        "the token used for this reconnect must be rotated, not re-issued"
    );

    // The rotated token is the live one: lose the socket again and reconnect
    // with it — while the consumed original must now be rejected.
    drop(replacement);
    await_player_left(&mut peer_a, joined_b.player_id, "anchor sees the re-drop").await;

    let mut second_replacement = connect(addr).await;
    authenticate_v3(&mut second_replacement).await;
    send(
        &mut second_replacement,
        &ClientMessage::Reconnect {
            player_id: joined_b.player_id,
            room_id: joined_b.room_id,
            auth_token: wire_token,
        },
    )
    .await;
    let rejection = next_matching_server_message_within(
        &mut second_replacement,
        SERVER_MESSAGE_TIMEOUT,
        "stale-token rejection",
        |message| match message {
            ServerMessage::ReconnectionFailed { error_code, .. } => Some(error_code),
            ServerMessage::Reconnected(_) => {
                panic!("a consumed token must never reconnect again")
            }
            _ => None,
        },
    )
    .await;
    assert_eq!(
        rejection,
        signal_fish_server::protocol::ErrorCode::ReconnectionTokenInvalid,
        "the pre-rotation token must be rejected as invalid"
    );

    let reconnected_again = reconnect(
        &mut second_replacement,
        joined_b.player_id,
        joined_b.room_id,
        rotated,
    )
    .await;
    assert!(
        reconnected_again.reconnection_token.is_some(),
        "every successful reconnect rotates in a fresh token"
    );

    assert_message_conservation(&game_server.metrics()).await;
    running_server.shutdown().await;
}

/// The token field is v3+ only: a pure-v2 joiner's raw `RoomJoined` JSON must
/// not contain a `reconnection_token` key (frozen v2 bytes).
#[tokio::test]
async fn v2_room_joined_has_no_reconnection_token_key() {
    let (running_server, _game_server) = start_server_default().await;
    let addr = running_server.addr();

    let mut peer = connect(addr).await;
    authenticate_v2(&mut peer).await;
    send(
        &mut peer,
        &ClientMessage::JoinRoom {
            game_name: "wire-token-v2-game".to_string(),
            room_code: None,
            player_name: "PureV2".to_string(),
            max_players: Some(4),
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await;
    let raw = next_raw_server_message_of_type(&mut peer, "RoomJoined", "v2 join").await;
    assert!(
        !raw.contains("reconnection_token"),
        "v2 RoomJoined bytes must not gain a reconnection_token key: {raw}"
    );
    running_server.shutdown().await;
}

/// Drain `ws` until it observes `PlayerLeft` for `expected` — the disconnect
/// flow broadcasts it after the reconnection record exists, so this is the
/// event-driven "the server is ready for the reconnect" sync point.
async fn await_player_left(ws: &mut WsStream, expected: PlayerId, context: &str) {
    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        context,
        |message| match message {
            ServerMessage::PlayerLeft { player_id, .. } if player_id == expected => Some(()),
            _ => None,
        },
    )
    .await;
}
