//! End-to-end protocol v2/v3 interop and capability-downgrade edge-case tests
//! through the real WebSocket stack (red-green sweep of the highest-risk
//! untested late-join / re-plan / reconnect paths).
//!
//! These suites complement `tests/v3_multipeer_e2e.rs` (mixed-room relay floor,
//! seat-fill late join, reconnect) and `tests/v3_replan_e2e.rs` (host failover,
//! sticky relay floor) by hammering the *interop boundaries* between v2 and v3
//! members and the *capability-downgrade* paths that `docs/protocol.md`'s
//! "Late-join decision table" and "SessionPlan re-issue" sections pin but which
//! have no end-to-end coverage:
//!
//! 1. `v2_late_join_into_finalized_mesh_webrtc_room_no_v3_leakage` — a v2
//!    (relay-only) member joins a FINALIZED `mesh + webrtc` room: it receives
//!    `RoomJoined` but never a `SessionPlan`/`NewPeer`/`Signal`, existing v3
//!    members receive complete plans that omit it, and its `GameData` still
//!    relays to the v3 members (the relay floor is its shared data path).
//! 2. `sticky_relay_floor_survives_v2_leave_then_v3_full_join` — a room that
//!    finalized to the relay floor (a v2 member at finalize) stays relay after
//!    the v2 member leaves and a fresh v3-full member fills the seat: the ladder
//!    is NOT re-run, and every v3 member receives an explicit relay refresh.
//! 3. `host_downgrade_reconnect_reelects_and_empties_downgraded_plan` — the
//!    elected host of a `host + webrtc` session reconnects advertising
//!    relay-only capabilities (protocol 3, no webrtc): the server detects the
//!    host is no longer session-capable, re-elects among the remaining capable
//!    members, emits fresh `SessionPlan`s, and the downgraded client receives a
//!    plan with an EMPTY peers list.
//! 4. `mesh_member_reconnects_as_v2_without_plan_or_peer_leakage` — a member of
//!    a finalized `mesh + webrtc` session reconnects on a fresh v2 socket: its
//!    frozen v2 reconnect wire receives no `SessionPlan`, capable incumbents'
//!    refreshed plans exclude it, and relay `GameData` remains available.
//! 5. `v2_member_leaves_relay_floored_session_no_replan` — a v2 member leaves a
//!    mixed (relay-floored) finalized session: no departure replan is emitted
//!    and the remaining members keep relaying.
//! 6. `non_mesh_v3_member_floors_room_to_relay` — a v3 member that negotiated
//!    `webrtc` but only `topologies: ["relay"]` is in a mesh-preferring room:
//!    the ladder requires ALL members to support the rung, so the room floors to
//!    relay (an explicit no-peer plan for every v3 member), which is the TRUE
//!    contract verified from `src/server/session_policy.rs::all_support`.

mod test_helpers;
mod v3_conformance_helpers;
mod websocket_test_helpers;

use std::{collections::BTreeSet, sync::Arc};

use futures_util::StreamExt;
use serde_json::json;
use signal_fish_server::config::{AppRegistrationEntry, SessionConfig, TurnConfig};
use signal_fish_server::protocol::{
    ClientMessage, IceServer, LobbyState, PlayerId, RoomJoinedPayload, ServerMessage,
    SessionPlanPayload, Topology, Transport,
};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::{create_router, websocket_route_v3};
use test_helpers::{test_protocol_config, test_server_config, RunningTestServer};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use v3_conformance_helpers::{await_ready_count, expect_session_plan_strict, ready, send};
use websocket_test_helpers::{
    deadline_after, expect_no_server_message_within, next_matching_server_message_within, WsStream,
};

const APP_ID: &str = "v3-interop-downgrade-app";
const STATIC_STUN_URL: &str = "stun:static.example.com:3478";
const TURN_STUN_URL: &str = "stun:stun.l.google.com:19302";
const SERVER_MESSAGE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(20);
/// Window in which a forbidden message would have arrived if it were going to.
const SILENCE_WINDOW: tokio::time::Duration = tokio::time::Duration::from_secs(2);

fn app_entry() -> AppRegistrationEntry {
    AppRegistrationEntry {
        app_id: APP_ID.to_string(),
        app_name: "V3 Interop Downgrade App".to_string(),
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
        "interop/downgrade e2e fixtures intentionally exercise the default [turn] STUN URL"
    );
}

fn session_config_with_topology(default_topology: Topology) -> SessionConfig {
    SessionConfig {
        default_topology,
        ice_servers: vec![IceServer {
            urls: vec![STATIC_STUN_URL.to_string()],
            username: None,
            credential: None,
        }],
        ..SessionConfig::default()
    }
}

fn mesh_session_config() -> SessionConfig {
    session_config_with_topology(Topology::Mesh)
}

fn host_session_config() -> SessionConfig {
    session_config_with_topology(Topology::Host)
}

/// Boot the production router (`/v2` nest + `/v3/ws` alias) around a server
/// built with the given `SessionConfig`, returning the bound address and the
/// server handle (the reconnect test mints the token in-process via the handle
/// rather than receiving it over the wire, like `tests/v3_multipeer_e2e.rs`).
async fn start_server_with_session(
    session: SessionConfig,
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
        signal_fish_server::config::TurnConfig::default(),
        signal_fish_server::database::DatabaseConfig::InMemory,
        signal_fish_server::config::MetricsConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        vec![app_entry()],
    )
    .await
    .expect("server builds");

    let enhanced_router = create_router("http://localhost:3000").with_state(game_server.clone());
    let combined_router = axum::Router::new()
        .nest("/v2", enhanced_router)
        .route("/v3/ws", websocket_route_v3("http://localhost:3000"))
        .fallback(|| async { "Use /v2/ws or /v3/ws" })
        .with_state(game_server.clone());

    let running_server = RunningTestServer::spawn(game_server.clone(), combined_router).await;
    (running_server, game_server)
}

async fn connect(addr: std::net::SocketAddr) -> WsStream {
    let url = format!("ws://{addr}/v3/ws");
    let (ws, _) = tokio::time::timeout(tokio::time::Duration::from_secs(10), connect_async(&url))
        .await
        .expect("connect timeout")
        .expect("connect");
    ws
}

/// Authenticate, advertising the given protocol version + capabilities, and
/// assert the negotiated version advertised in `ProtocolInfo`.
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

    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "Authenticated", |message| {
        matches!(message, ServerMessage::Authenticated { .. }).then_some(())
    })
    .await;
    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "ProtocolInfo", |message| {
        match message {
            ServerMessage::ProtocolInfo(info) => {
                assert_eq!(info.protocol_version, expected_version);
                Some(())
            }
            _ => None,
        }
    })
    .await;
}

/// Authenticate as a v3 client supporting webrtc + every topology.
async fn authenticate_v3_full(ws: &mut WsStream) {
    authenticate(
        ws,
        Some(3),
        Some(vec![Transport::Relay, Transport::WebRtc]),
        Some(vec![Topology::Relay, Topology::Host, Topology::Mesh]),
        Some(3),
    )
    .await;
}

/// Authenticate as a v3 client that negotiated the webrtc transport but only
/// the relay topology — session-incapable of any WebRTC selection rung.
async fn authenticate_v3_relay_topology_only(ws: &mut WsStream) {
    authenticate(
        ws,
        Some(3),
        Some(vec![Transport::Relay, Transport::WebRtc]),
        Some(vec![Topology::Relay]),
        Some(3),
    )
    .await;
}

/// Authenticate as a v3 client that is relay-ONLY on both axes (a downgraded
/// reconnector: protocol 3 but no webrtc transport, relay topology only).
async fn authenticate_v3_relay_only(ws: &mut WsStream) {
    authenticate(
        ws,
        Some(3),
        Some(vec![Transport::Relay]),
        Some(vec![Topology::Relay]),
        Some(3),
    )
    .await;
}

/// Authenticate as a pure v2 client on the v3 endpoint (relay-only).
async fn authenticate_v2(ws: &mut WsStream) {
    authenticate(ws, Some(2), None, None, None).await;
}

/// Join (or create) a room for `game_name`, returning the full payload.
async fn join_room(
    ws: &mut WsStream,
    game_name: &str,
    room_code: Option<String>,
    player_name: &str,
    max_players: u8,
) -> Box<RoomJoinedPayload> {
    send(
        ws,
        &ClientMessage::JoinRoom {
            game_name: game_name.to_string(),
            room_code,
            player_name: player_name.to_string(),
            max_players: Some(max_players),
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

/// Skip to `GameStarting`, then return the `SessionPlan` that must immediately
/// follow it (panics on any other interleaved `ServerMessage`).
async fn expect_finalize_plan(ws: &mut WsStream, who: &str) -> Box<SessionPlanPayload> {
    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "finalization GameStarting",
        |message| match message {
            ServerMessage::GameStarting { .. } => Some(()),
            ServerMessage::SessionPlan(_) => {
                panic!("{who}: SessionPlan must not arrive before GameStarting")
            }
            _ => None,
        },
    )
    .await;
    expect_session_plan_strict(ws, who).await
}

fn assert_relay_floor_plan(plan: &SessionPlanPayload, who: &str) {
    assert_eq!(plan.topology, Topology::Relay, "{who} topology");
    assert_eq!(plan.transport, Transport::Relay, "{who} transport");
    assert_eq!(plan.host, None, "{who} relay plan must not name a host");
    assert!(plan.peers.is_empty(), "{who} relay plan must have no peers");
    assert!(
        plan.ice_servers.is_empty(),
        "{who} relay plan must not carry ICE"
    );
    assert_eq!(plan.fallback, Transport::Relay, "{who} fallback");
}

/// Drive the paced all-ready handshake: every ready is observed by every member
/// before the next fires. Readiness no longer auto-starts, so once every member
/// is ready an explicit `StartGame` from the creator (sockets[0]) finalizes the
/// room (these rooms are `supports_authority: false`, so any member may start).
async fn finalize_room(sockets: &mut [WsStream]) {
    let member_count = sockets.len();
    for ready_index in 0..member_count - 1 {
        ready(&mut sockets[ready_index]).await;
        for socket in sockets.iter_mut() {
            await_ready_count(socket, ready_index + 1).await;
        }
    }
    ready(&mut sockets[member_count - 1]).await;
    // Pace the final ready past every member before starting.
    for socket in sockets.iter_mut() {
        await_ready_count(socket, member_count).await;
    }
    send(&mut sockets[0], &ClientMessage::StartGame).await;
}

/// Assert the exact next message is `PlayerLeft(left)`.
async fn expect_player_left(ws: &mut WsStream, left: PlayerId, who: &str) {
    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "PlayerLeft", |message| {
        match message {
            ServerMessage::PlayerLeft { player_id, .. } => {
                assert_eq!(player_id, left, "{who} saw the wrong player leave");
                Some(())
            }
            other => panic!("{who} expected PlayerLeft, got {other:?}"),
        }
    })
    .await;
}

/// Assert the exact next message is `PlayerJoined(joined)`.
async fn expect_player_joined(ws: &mut WsStream, joined: PlayerId, who: &str) {
    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "PlayerJoined", |message| {
        match message {
            ServerMessage::PlayerJoined { player } => {
                assert_eq!(player.id, joined, "{who} saw the wrong player join");
                Some(())
            }
            other => panic!("{who} expected PlayerJoined, got {other:?}"),
        }
    })
    .await;
}

/// Panic if a message a v2 client received is a v3-only variant.
fn forbid_v3_leakage(message: &ServerMessage, who: &str) {
    if matches!(
        message,
        ServerMessage::Signal { .. }
            | ServerMessage::NewPeer { .. }
            | ServerMessage::SessionPlan(_)
            | ServerMessage::PeerTransportStatus { .. }
    ) {
        panic!("{who}: a v2 client must never observe v3-only traffic, got {message:?}");
    }
}

/// `next_matching_server_message_within`, with every read message checked
/// against the v2-only variant set first (for pure-v2 clients).
async fn next_matching_v2_only<T>(
    ws: &mut WsStream,
    context: &str,
    who: &str,
    mut pick: impl FnMut(ServerMessage) -> Option<T>,
) -> T {
    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, context, move |message| {
        forbid_v3_leakage(&message, who);
        pick(message)
    })
    .await
}

/// Assert the exact next protocol frame type while preserving raw JSON so v2
/// field absence can be checked before deserialization fills defaults.
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
            .unwrap_or_else(|error| panic!("{context}: websocket error: {error}"));
        let Message::Text(text) = frame else {
            continue;
        };
        let value: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{context}: invalid JSON frame: {error}; {text:?}"));
        let actual_type = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("{context}: text frame lacks a type tag: {text:?}"));
        assert_eq!(
            actual_type, expected_type,
            "{context}: expected {expected_type}, got {actual_type}: {text}"
        );
        return text.to_string();
    }
}

/// Send one `GameData` payload and assert byte-identical relay delivery with the
/// correct `from_player`.
async fn relay_one_game_data(from: &mut WsStream, to: &mut WsStream, from_id: PlayerId, who: &str) {
    let payload = json!({ "tick": 7, "marker": format!("game-data from {from_id}") });
    send(
        from,
        &ClientMessage::GameData {
            class: None,
            key: None,
            data: payload.clone(),
        },
    )
    .await;
    next_matching_server_message_within(
        to,
        SERVER_MESSAGE_TIMEOUT,
        "relayed GameData",
        |message| match message {
            ServerMessage::GameData {
                from_player, data, ..
            } => {
                assert_eq!(from_player, from_id, "{who} saw the wrong GameData sender");
                assert_eq!(
                    data, payload,
                    "{who} GameData payload must relay byte-identically"
                );
                Some(())
            }
            other => panic!("{who} expected GameData as the next message, got {other:?}"),
        },
    )
    .await;
}

// ===========================================================================
// 1. v2 late-join into a FINALIZED mesh+webrtc room: no v3 leakage either way,
//    relay floor still carries the v2 member's GameData.
// ===========================================================================
#[tokio::test(flavor = "multi_thread")]
async fn v2_late_join_into_finalized_mesh_webrtc_room_no_v3_leakage() {
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "interop-v2-into-mesh";
    let legacy_who = "legacy (v2)";

    // A room finalizes only when it is FULL and all present members ready
    // (`Room::should_enter_lobby`), so a v2 cannot late-join a 2-seat mesh room
    // (already full). Finalize a FULL 3-seat mesh+webrtc room with three v3
    // members, then have one leave to open a seat for the v2 joiner — the same
    // seat-fill shape as `mesh_n3_seat_fill_late_join`.
    let mut peer_a = connect(addr).await;
    authenticate_v3_full(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, game, None, "PeerA", 3).await;
    let (room_code, id_a) = (joined_a.room_code, joined_a.player_id);

    let mut peer_b = connect(addr).await;
    authenticate_v3_full(&mut peer_b).await;
    let id_b = join_room(&mut peer_b, game, Some(room_code.clone()), "PeerB", 3)
        .await
        .player_id;

    let mut leaver = connect(addr).await;
    authenticate_v3_full(&mut leaver).await;
    let leaver_id = join_room(&mut leaver, game, Some(room_code.clone()), "Leaver", 3)
        .await
        .player_id;

    {
        let mut sockets = [peer_a, peer_b, leaver];
        finalize_room(&mut sockets).await;
        for (socket, who) in sockets.iter_mut().zip(["peer_a", "peer_b", "leaver"]) {
            let plan = expect_finalize_plan(socket, who).await;
            assert_eq!(plan.topology, Topology::Mesh, "{who} finalized to mesh");
            assert_eq!(plan.transport, Transport::WebRtc);
            assert_eq!(plan.peers.len(), 2, "{who} sees the other two v3 members");
        }
        [peer_a, peer_b, leaver] = sockets;
    }

    // One v3 member leaves the live session, opening a seat (a mesh departure
    // re-plans nothing for the survivors).
    leaver.close(None).await.expect("close leaver socket");
    expect_player_left(&mut peer_a, leaver_id, "peer_a").await;
    expect_player_left(&mut peer_b, leaver_id, "peer_b").await;

    // A v2 (relay-only) client fills the freed seat in the finalized room.
    let mut legacy = connect(addr).await;
    authenticate_v2(&mut legacy).await;
    send(
        &mut legacy,
        &ClientMessage::JoinRoom {
            game_name: game.to_string(),
            room_code: Some(room_code),
            player_name: "Legacy".to_string(),
            max_players: Some(3),
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await;
    let legacy_joined = next_matching_v2_only(
        &mut legacy,
        "v2 room join response",
        legacy_who,
        |message| match message {
            ServerMessage::RoomJoined(payload) => Some(payload),
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                panic!("v2 room join failed: {reason} ({error_code:?})")
            }
            _ => None,
        },
    )
    .await;
    let legacy_id = legacy_joined.player_id;
    // The seat-filling v2 joiner sees the running session's persisted state.
    assert_eq!(
        legacy_joined.lobby_state,
        LobbyState::Finalized,
        "a seat-filling joiner of a finalized room must see lobby_state: finalized"
    );

    // The v2 joiner NEVER receives a SessionPlan/NewPeer/Signal (the v2-only
    // guard would panic on any of them); strict silence after RoomJoined.
    expect_no_server_message_within(&mut legacy, SILENCE_WINDOW, "v2 joiner after RoomJoined")
        .await;

    // Existing v3 members see PlayerJoined(legacy), then complete refreshed
    // mesh plans that omit the v2 joiner from both peer lists.
    for (socket, member_id, other_id, who) in [
        (&mut peer_a, id_a, id_b, "peer_a"),
        (&mut peer_b, id_b, id_a, "peer_b"),
    ] {
        expect_player_joined(socket, legacy_id, who).await;
        let plan = expect_session_plan_strict(socket, who).await;
        assert_eq!(plan.topology, Topology::Mesh);
        assert_eq!(plan.transport, Transport::WebRtc);
        assert_eq!(plan.peers.len(), 1, "{who} sees only the capable v3 peer");
        assert_eq!(plan.peers[0].player_id, other_id);
        assert_eq!(plan.peers[0].initiate, member_id < other_id);
        assert!(plan.peers.iter().all(|peer| peer.player_id != legacy_id));
        expect_no_server_message_within(
            socket,
            SILENCE_WINDOW,
            &format!("{who} after its v2-join membership refresh"),
        )
        .await;
    }

    // The relay floor is the v2 member's data path: its GameData still reaches
    // the v3 members byte-identically (and vice versa).
    relay_one_game_data(&mut legacy, &mut peer_a, legacy_id, "peer_a").await;
    relay_one_game_data(&mut peer_b, &mut legacy, id_b, legacy_who).await;
    let _ = id_a;
    running_server.shutdown().await;
}

// ===========================================================================
// 2. Sticky relay floor: finalized-to-relay (v2 present at finalize), v2 leaves,
//    fresh v3-full member joins — the ladder is NOT re-run, room STAYS relay.
// ===========================================================================
#[tokio::test(flavor = "multi_thread")]
async fn sticky_relay_floor_survives_v2_leave_then_v3_full_join() {
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "interop-sticky-relay";
    let legacy_who = "legacy (v2)";

    // A(v3-full) + Legacy(v2) finalize a mesh-preferring 2-seat room => relay
    // floor (a single v2 member floors the whole room; no plan is stored).
    let mut peer_a = connect(addr).await;
    authenticate_v3_full(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, game, None, "PeerA", 2).await;
    let (room_code, id_a) = (joined_a.room_code, joined_a.player_id);

    let mut legacy = connect(addr).await;
    authenticate_v2(&mut legacy).await;
    send(
        &mut legacy,
        &ClientMessage::JoinRoom {
            game_name: game.to_string(),
            room_code: Some(room_code.clone()),
            player_name: "Legacy".to_string(),
            max_players: Some(2),
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await;
    let legacy_id = next_matching_v2_only(
        &mut legacy,
        "v2 room join response",
        legacy_who,
        |message| match message {
            ServerMessage::RoomJoined(payload) => Some(payload.player_id),
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                panic!("v2 room join failed: {reason} ({error_code:?})")
            }
            _ => None,
        },
    )
    .await;

    ready(&mut peer_a).await;
    await_ready_count(&mut peer_a, 1).await;
    next_matching_v2_only(
        &mut legacy,
        "v2 ready count 1",
        legacy_who,
        |message| match message {
            ServerMessage::LobbyStateChanged { ready_players, .. } if ready_players.len() == 1 => {
                Some(())
            }
            _ => None,
        },
    )
    .await;
    ready(&mut legacy).await;
    await_ready_count(&mut peer_a, 2).await;
    next_matching_v2_only(
        &mut legacy,
        "v2 ready count 2",
        legacy_who,
        |message| match message {
            ServerMessage::LobbyStateChanged { ready_players, .. } if ready_players.len() == 2 => {
                Some(())
            }
            _ => None,
        },
    )
    .await;

    // Readiness no longer auto-starts: the v3 creator sends StartGame to
    // finalize the relay-floored room.
    send(&mut peer_a, &ClientMessage::StartGame).await;

    // Both observe GameStarting; only the v3 member gets the explicit floor plan.
    next_matching_server_message_within(
        &mut peer_a,
        SERVER_MESSAGE_TIMEOUT,
        "relay-floor GameStarting",
        |message| match message {
            ServerMessage::GameStarting { .. } => Some(()),
            _ => None,
        },
    )
    .await;
    let initial_floor = expect_session_plan_strict(&mut peer_a, "peer_a").await;
    assert_relay_floor_plan(&initial_floor, "peer_a");
    next_matching_v2_only(&mut legacy, "v2 GameStarting", legacy_who, |message| {
        matches!(message, ServerMessage::GameStarting { .. }).then_some(())
    })
    .await;

    // The v2 member leaves, reopening a seat in the Finalized (relay) room.
    legacy.close(None).await.expect("close legacy socket");
    expect_player_left(&mut peer_a, legacy_id, "peer_a").await;

    // A fresh v3-FULL member fills the seat. Even though the room is now
    // all-v3-webrtc, the ladder is NOT re-run: the session stays relay (sticky).
    let mut joiner = connect(addr).await;
    authenticate_v3_full(&mut joiner).await;
    let joiner_joined = join_room(&mut joiner, game, Some(room_code), "Joiner", 2).await;
    let joiner_id = joiner_joined.player_id;
    assert_eq!(
        joiner_joined.lobby_state,
        LobbyState::Finalized,
        "a seat-filling joiner of a finalized room must see lobby_state: finalized"
    );

    // The now-all-v3 membership must not rerun the ladder. Both v3 members get
    // the authoritative relay reset derived from the absent sticky plan.
    expect_player_joined(&mut peer_a, joiner_id, "peer_a").await;
    let incumbent_floor = expect_session_plan_strict(&mut peer_a, "peer_a refresh").await;
    let joiner_floor = expect_session_plan_strict(&mut joiner, "joiner floor").await;
    assert_relay_floor_plan(&incumbent_floor, "peer_a refresh");
    assert_relay_floor_plan(&joiner_floor, "joiner floor");
    expect_no_server_message_within(&mut peer_a, SILENCE_WINDOW, "peer_a after floor refresh")
        .await;
    expect_no_server_message_within(&mut joiner, SILENCE_WINDOW, "joiner after floor plan").await;

    // The room still relays: GameData round-trips both ways over the floor.
    relay_one_game_data(&mut joiner, &mut peer_a, joiner_id, "peer_a").await;
    relay_one_game_data(&mut peer_a, &mut joiner, id_a, "joiner").await;
    running_server.shutdown().await;
}

// ===========================================================================
// 3. Capability-downgrade reconnect: the elected host of a host+webrtc session
//    reconnects as relay-only — server re-elects a capable host and the
//    downgraded client receives a plan with an EMPTY peers list.
// ===========================================================================
#[tokio::test(flavor = "multi_thread")]
async fn host_downgrade_reconnect_reelects_and_empties_downgraded_plan() {
    let (running_server, server) = start_server_with_session(host_session_config()).await;
    let addr = running_server.addr();
    let game = "interop-host-downgrade";

    // Three v3+webrtc clients finalize a host+webrtc room; the creator (earliest
    // joiner) wins the host election.
    let mut host = connect(addr).await;
    authenticate_v3_full(&mut host).await;
    let host_joined = join_room(&mut host, game, None, "Host", 3).await;
    let (room_code, host_id, room_id) = (
        host_joined.room_code,
        host_joined.player_id,
        host_joined.room_id,
    );

    let mut peer_a = connect(addr).await;
    authenticate_v3_full(&mut peer_a).await;
    let id_a = join_room(&mut peer_a, game, Some(room_code.clone()), "PeerA", 3)
        .await
        .player_id;

    let mut peer_b = connect(addr).await;
    authenticate_v3_full(&mut peer_b).await;
    let id_b = join_room(&mut peer_b, game, Some(room_code), "PeerB", 3)
        .await
        .player_id;

    {
        let mut sockets = [host, peer_a, peer_b];
        finalize_room(&mut sockets).await;
        for (socket, who) in sockets.iter_mut().zip(["host", "peer_a", "peer_b"]) {
            let plan = expect_finalize_plan(socket, who).await;
            assert_eq!(plan.topology, Topology::Host);
            assert_eq!(plan.transport, Transport::WebRtc);
            assert_eq!(
                plan.host,
                Some(host_id),
                "{who} initial plan names the creator"
            );
        }
        [host, peer_a, peer_b] = sockets;
    }

    // The new host among the remaining capable members is the earliest-joined
    // survivor: peer_a (joined before peer_b).
    let new_host_id = id_a;

    // Snapshot the host's room info, then disconnect it.
    let room = server
        .database()
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup")
        .expect("room exists");
    let host_info = room
        .players
        .get(&host_id)
        .cloned()
        .expect("host in room before disconnect");

    server.disconnect_client(&host_id).await;
    host.close(None).await.expect("close host socket");

    // The host departure already triggers a failover re-plan over the survivors
    // (the stored host is now missing). Drain it so the reconnect re-plan below
    // is asserted in isolation: each survivor sees PlayerLeft(host) then a fresh
    // plan naming peer_a.
    for (socket, who) in [(&mut peer_a, "peer_a"), (&mut peer_b, "peer_b")] {
        expect_player_left(socket, host_id, who).await;
        let plan = expect_session_plan_strict(socket, who).await;
        assert_eq!(
            plan.host,
            Some(new_host_id),
            "{who} departure-failover plan names the earliest-joined survivor"
        );
    }
    expect_no_server_message_within(
        &mut peer_a,
        SILENCE_WINDOW,
        "peer_a after departure failover",
    )
    .await;
    expect_no_server_message_within(
        &mut peer_b,
        SILENCE_WINDOW,
        "peer_b after departure failover",
    )
    .await;

    // Mint a reconnection token for the ex-host — obtained in-process here
    // rather than over the wire (the pattern from `v3_multipeer_e2e.rs`).
    let token = server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(host_id, room_id, false, Some(host_info), 0)
        .await;

    // The ex-host reconnects over a FRESH socket advertising RELAY-ONLY
    // capabilities (protocol 3, no webrtc). `reassign_connection` carries the
    // fresh socket's downgraded protocol onto the restored player id
    // (`src/server/connection_manager.rs:369`), so the server must honor the
    // new (downgraded) Authenticate.
    let mut reconnector = connect(addr).await;
    authenticate_v3_relay_only(&mut reconnector).await;
    send(
        &mut reconnector,
        &ClientMessage::Reconnect {
            player_id: host_id,
            room_id,
            auth_token: token,
        },
    )
    .await;

    // The reconnector receives Reconnected (restored id, still-finalized room).
    next_matching_server_message_within(
        &mut reconnector,
        SERVER_MESSAGE_TIMEOUT,
        "Reconnected",
        |message| match message {
            ServerMessage::Reconnected(payload) => {
                assert_eq!(payload.player_id, host_id, "restored player id");
                assert_eq!(payload.room_id, room_id);
                Some(())
            }
            ServerMessage::ReconnectionFailed { reason, error_code } => {
                panic!("reconnect failed: {reason} ({error_code:?})")
            }
            other => panic!("reconnector expected Reconnected, got {other:?}"),
        },
    )
    .await;

    // The downgraded reconnector is already a member, not the stored host (the
    // departure failover re-elected peer_a). The stored host (peer_a) is still
    // valid and capable, so the late-join path runs the normal branch, NOT a
    // self-heal. The reconnector is v3, so it receives a (v3-gated) late-join
    // SessionPlan describing the host+webrtc session — but with an EMPTY peers
    // list, since it can no longer run the session (relay-only).
    // RED: suspected bug — if the downgraded client is named host, kept as host,
    // or the server panics on the seated-but-incapable host, this fails.
    let downgraded_plan =
        expect_session_plan_strict(&mut reconnector, "downgraded reconnector").await;
    assert_eq!(
        downgraded_plan.topology,
        Topology::Host,
        "the sticky topology is unchanged for the downgraded member"
    );
    assert_eq!(
        downgraded_plan.transport,
        Transport::WebRtc,
        "sticky transport"
    );
    assert_eq!(
        downgraded_plan.host,
        Some(new_host_id),
        "the plan names the re-elected capable host, never the downgraded reconnector"
    );
    assert_ne!(
        downgraded_plan.host,
        Some(host_id),
        "the downgraded relay-only client must never be named host"
    );
    assert!(
        downgraded_plan.peers.is_empty(),
        "a session-incapable (relay-only) member receives an empty peers list; got {:?}",
        downgraded_plan.peers
    );

    // Each survivor receives PlayerReconnected followed by a complete plan
    // refresh. The downgraded member is omitted from both capable peer lists.
    for (socket, member_id, who) in [
        (&mut peer_a, id_a, "host (peer_a)"),
        (&mut peer_b, id_b, "peer_b"),
    ] {
        next_matching_server_message_within(
            socket,
            SERVER_MESSAGE_TIMEOUT,
            "PlayerReconnected",
            |message| match message {
                ServerMessage::PlayerReconnected { player_id, .. } => {
                    assert_eq!(player_id, host_id, "{who} saw the wrong reconnector");
                    Some(())
                }
                other => panic!("{who} expected PlayerReconnected, got {other:?}"),
            },
        )
        .await;
        let plan = expect_session_plan_strict(socket, who).await;
        assert_eq!(plan.topology, Topology::Host);
        assert_eq!(plan.transport, Transport::WebRtc);
        assert_eq!(plan.host, Some(new_host_id));
        assert!(plan.peers.iter().all(|peer| peer.player_id != host_id));
        if member_id == new_host_id {
            assert_eq!(plan.peers.len(), 1);
            assert_eq!(plan.peers[0].player_id, id_b);
            assert!(!plan.peers[0].initiate);
        } else {
            assert_eq!(plan.peers.len(), 1);
            assert_eq!(plan.peers[0].player_id, new_host_id);
            assert!(plan.peers[0].initiate);
        }
    }
    expect_no_server_message_within(
        &mut peer_a,
        SILENCE_WINDOW,
        "host (peer_a) after its reconnect refresh",
    )
    .await;
    expect_no_server_message_within(
        &mut peer_b,
        SILENCE_WINDOW,
        "peer_b after its reconnect refresh",
    )
    .await;
    expect_no_server_message_within(
        &mut reconnector,
        SILENCE_WINDOW,
        "downgraded reconnector after its empty plan",
    )
    .await;
    let _ = id_b;
    running_server.shutdown().await;
}

// ===========================================================================
// 4. A finalized mesh member reconnects on a fresh v2 socket: the replacement
//    receives frozen v2 state and no plan; incumbents exclude it from WebRTC.
// ===========================================================================
#[tokio::test(flavor = "multi_thread")]
async fn mesh_member_reconnects_as_v2_without_plan_or_peer_leakage() {
    let (running_server, server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "interop-mesh-reconnect-v2";
    let legacy_who = "v2 reconnector";

    let mut peer_a = connect(addr).await;
    authenticate_v3_full(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, game, None, "PeerA", 3).await;
    let (room_code, id_a, room_id) = (joined_a.room_code, joined_a.player_id, joined_a.room_id);

    let mut peer_b = connect(addr).await;
    authenticate_v3_full(&mut peer_b).await;
    let id_b = join_room(&mut peer_b, game, Some(room_code.clone()), "PeerB", 3)
        .await
        .player_id;

    let mut departing = connect(addr).await;
    authenticate_v3_full(&mut departing).await;
    let departing_id = join_room(&mut departing, game, Some(room_code), "Departing", 3)
        .await
        .player_id;

    {
        let mut sockets = [peer_a, peer_b, departing];
        finalize_room(&mut sockets).await;
        for (socket, who) in sockets.iter_mut().zip(["peer_a", "peer_b", "departing"]) {
            let plan = expect_finalize_plan(socket, who).await;
            assert_eq!(plan.topology, Topology::Mesh, "{who} topology");
            assert_eq!(plan.transport, Transport::WebRtc, "{who} transport");
            assert_eq!(plan.host, None, "{who} mesh plan has no host");
            assert_eq!(plan.peers.len(), 2, "{who} sees both v3 peers");
        }
        [peer_a, peer_b, departing] = sockets;
    }

    let room = server
        .database()
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup")
        .expect("room exists");
    let departing_info = room
        .players
        .get(&departing_id)
        .cloned()
        .expect("departing member exists before disconnect");

    server.disconnect_client(&departing_id).await;
    departing.close(None).await.expect("close departing socket");
    for (socket, who) in [(&mut peer_a, "peer_a"), (&mut peer_b, "peer_b")] {
        expect_player_left(socket, departing_id, who).await;
        expect_no_server_message_within(
            socket,
            SILENCE_WINDOW,
            &format!("{who} after mesh-member departure"),
        )
        .await;
    }

    let token = server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(departing_id, room_id, false, Some(departing_info), 0)
        .await;

    let mut reconnector = connect(addr).await;
    authenticate_v2(&mut reconnector).await;
    send(
        &mut reconnector,
        &ClientMessage::Reconnect {
            player_id: departing_id,
            room_id,
            auth_token: token,
        },
    )
    .await;

    let raw = next_raw_server_message_of_type(
        &mut reconnector,
        "Reconnected",
        "raw v2 finalized-room reconnect",
    )
    .await;
    let value: serde_json::Value =
        serde_json::from_str(&raw).expect("Reconnected frame is valid JSON");
    let data = value
        .get("data")
        .and_then(serde_json::Value::as_object)
        .unwrap_or_else(|| panic!("expected a JSON object at /data: {raw}"));
    assert_eq!(
        data.get("player_id").and_then(serde_json::Value::as_str),
        Some(departing_id.to_string().as_str()),
        "the fresh v2 connection restores the original identity: {raw}"
    );
    assert_eq!(
        data.get("lobby_state").and_then(serde_json::Value::as_str),
        Some("finalized"),
        "the reconnect baseline preserves the finalized room: {raw}"
    );
    let data_keys: BTreeSet<_> = data.keys().map(String::as_str).collect();
    assert_eq!(
        data_keys,
        BTreeSet::from([
            "room_id",
            "room_code",
            "player_id",
            "game_name",
            "max_players",
            "supports_authority",
            "current_players",
            "is_authority",
            "lobby_state",
            "ready_players",
            "relay_type",
            "current_spectators",
            "missed_events",
        ]),
        "v2 Reconnected must use the exact frozen /data key set: {raw}"
    );
    let current_players = data
        .get("current_players")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("expected /data/current_players array: {raw}"));
    assert_eq!(
        current_players.len(),
        3,
        "complete membership snapshot: {raw}"
    );
    let expected_player_keys =
        BTreeSet::from(["id", "name", "is_authority", "is_ready", "connected_at"]);
    for player in current_players {
        let player = player
            .as_object()
            .unwrap_or_else(|| panic!("expected PlayerInfo object: {raw}"));
        let player_keys: BTreeSet<_> = player.keys().map(String::as_str).collect();
        assert_eq!(
            player_keys, expected_player_keys,
            "v2 PlayerInfo must use the exact frozen key set: {raw}"
        );
    }

    for (socket, member_id, other_id, who) in [
        (&mut peer_a, id_a, id_b, "peer_a"),
        (&mut peer_b, id_b, id_a, "peer_b"),
    ] {
        next_matching_server_message_within(
            socket,
            SERVER_MESSAGE_TIMEOUT,
            "PlayerReconnected",
            |message| match message {
                ServerMessage::PlayerReconnected { player_id, .. } => {
                    assert_eq!(player_id, departing_id, "{who} saw the wrong reconnector");
                    Some(())
                }
                other => panic!("{who} expected PlayerReconnected, got {other:?}"),
            },
        )
        .await;
        let plan = expect_session_plan_strict(socket, who).await;
        assert_eq!(plan.topology, Topology::Mesh, "{who} sticky topology");
        assert_eq!(plan.transport, Transport::WebRtc, "{who} sticky transport");
        assert_eq!(plan.host, None, "{who} mesh plan has no host");
        assert_eq!(plan.peers.len(), 1, "{who} sees only the capable peer");
        assert_eq!(plan.peers[0].player_id, other_id, "{who} peer identity");
        assert_eq!(
            plan.peers[0].initiate,
            member_id < other_id,
            "{who} retains the deterministic mesh glare rule"
        );
        assert!(
            plan.peers.iter().all(|peer| peer.player_id != departing_id),
            "{who} must not attempt WebRTC with the v2 reconnector"
        );
    }

    // Observing both incumbent refreshes establishes that reconnect publication
    // has completed. Every socket must be quiet at that exact phase boundary:
    // incumbents receive one refresh each, and the fresh v2 generation remains
    // plan-free. The exact-next relay assertions below independently reject any
    // later duplicate or leaked message queued ahead of GameData.
    for (socket, who) in [(&mut peer_a, "peer_a"), (&mut peer_b, "peer_b")] {
        expect_no_server_message_within(
            socket,
            SILENCE_WINDOW,
            &format!("{who} after its single reconnect refresh"),
        )
        .await;
    }
    expect_no_server_message_within(
        &mut reconnector,
        SILENCE_WINDOW,
        "v2 reconnector after incumbent reconnect refreshes",
    )
    .await;

    relay_one_game_data(&mut reconnector, &mut peer_a, departing_id, "peer_a").await;
    relay_one_game_data(&mut peer_a, &mut reconnector, id_a, legacy_who).await;
    running_server.shutdown().await;
}

// ===========================================================================
// 5. v2 member leaves a mixed (relay-floored) session mid-session: no replan,
//    remaining members keep relaying.
// ===========================================================================
#[tokio::test(flavor = "multi_thread")]
async fn v2_member_leaves_relay_floored_session_no_replan() {
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "interop-v2-leaves-relay";
    let legacy_who = "legacy (v2)";

    // A(v3) + B(v3) + Legacy(v2) finalize a 3-seat mesh-preferring room => relay
    // floor (the single v2 member floors it).
    let mut peer_a = connect(addr).await;
    authenticate_v3_full(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, game, None, "PeerA", 3).await;
    let (room_code, id_a) = (joined_a.room_code, joined_a.player_id);

    let mut peer_b = connect(addr).await;
    authenticate_v3_full(&mut peer_b).await;
    let id_b = join_room(&mut peer_b, game, Some(room_code.clone()), "PeerB", 3)
        .await
        .player_id;

    let mut legacy = connect(addr).await;
    authenticate_v2(&mut legacy).await;
    send(
        &mut legacy,
        &ClientMessage::JoinRoom {
            game_name: game.to_string(),
            room_code: Some(room_code),
            player_name: "Legacy".to_string(),
            max_players: Some(3),
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await;
    let legacy_id = next_matching_v2_only(
        &mut legacy,
        "v2 room join response",
        legacy_who,
        |message| match message {
            ServerMessage::RoomJoined(payload) => Some(payload.player_id),
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                panic!("v2 room join failed: {reason} ({error_code:?})")
            }
            _ => None,
        },
    )
    .await;

    // Paced readies (the v2 lobby updates run through the v2-only guard).
    ready(&mut peer_a).await;
    await_ready_count(&mut peer_a, 1).await;
    await_ready_count(&mut peer_b, 1).await;
    next_matching_v2_only(
        &mut legacy,
        "v2 ready count 1",
        legacy_who,
        |message| match message {
            ServerMessage::LobbyStateChanged { ready_players, .. } if ready_players.len() == 1 => {
                Some(())
            }
            _ => None,
        },
    )
    .await;
    ready(&mut peer_b).await;
    await_ready_count(&mut peer_a, 2).await;
    await_ready_count(&mut peer_b, 2).await;
    next_matching_v2_only(
        &mut legacy,
        "v2 ready count 2",
        legacy_who,
        |message| match message {
            ServerMessage::LobbyStateChanged { ready_players, .. } if ready_players.len() == 2 => {
                Some(())
            }
            _ => None,
        },
    )
    .await;
    ready(&mut legacy).await;
    await_ready_count(&mut peer_a, 3).await;
    await_ready_count(&mut peer_b, 3).await;
    next_matching_v2_only(
        &mut legacy,
        "v2 ready count 3",
        legacy_who,
        |message| match message {
            ServerMessage::LobbyStateChanged { ready_players, .. } if ready_players.len() == 3 => {
                Some(())
            }
            _ => None,
        },
    )
    .await;

    // Readiness no longer auto-starts: the v3 creator sends StartGame to
    // finalize the relay-floored session.
    send(&mut peer_a, &ClientMessage::StartGame).await;

    // Everyone gets GameStarting; each v3 member gets an explicit floor plan,
    // while the v2 member remains plan-free.
    for (ws, who) in [(&mut peer_a, "peer_a"), (&mut peer_b, "peer_b")] {
        next_matching_server_message_within(
            ws,
            SERVER_MESSAGE_TIMEOUT,
            "relay-floor GameStarting",
            |message| match message {
                ServerMessage::GameStarting { .. } => Some(()),
                _ => None,
            },
        )
        .await;
        let plan = expect_session_plan_strict(ws, who).await;
        assert_relay_floor_plan(&plan, who);
    }
    next_matching_v2_only(&mut legacy, "v2 GameStarting", legacy_who, |message| {
        matches!(message, ServerMessage::GameStarting { .. }).then_some(())
    })
    .await;

    // The v2 member leaves the live relay-floored session.
    legacy.close(None).await.expect("close legacy socket");

    // Each surviving v3 member sees PlayerLeft(legacy) and then nothing: relay
    // has no sticky host state, so a departure does not trigger another plan.
    for (socket, who) in [(&mut peer_a, "peer_a"), (&mut peer_b, "peer_b")] {
        expect_player_left(socket, legacy_id, who).await;
        expect_no_server_message_within(
            socket,
            SILENCE_WINDOW,
            &format!("{who} after a v2 member left the relay-floored session"),
        )
        .await;
    }

    // The remaining members keep relaying over the floor, both ways.
    relay_one_game_data(&mut peer_a, &mut peer_b, id_a, "peer_b").await;
    relay_one_game_data(&mut peer_b, &mut peer_a, id_b, "peer_a").await;
    running_server.shutdown().await;
}

// ===========================================================================
// 6. Session-incapable v3 member (webrtc transport but topologies=["relay"]) in
//    a mesh-preferring room: the ladder requires ALL members to support the
//    rung, so the room floors to relay (verified from `all_support`).
// ===========================================================================
#[tokio::test(flavor = "multi_thread")]
async fn non_mesh_v3_member_floors_room_to_relay() {
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "interop-non-mesh-member";

    // Two mesh-capable v3 clients plus one v3 client that negotiated webrtc but
    // only the relay TOPOLOGY. `all_support(mesh, webrtc)` fails (the relay-only
    // member lacks the mesh topology), and `all_support(host, *)` fails too, so
    // the room floors to relay with an explicit v3 reset for everyone.
    let mut peer_a = connect(addr).await;
    authenticate_v3_full(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, game, None, "PeerA", 3).await;
    let (room_code, id_a) = (joined_a.room_code, joined_a.player_id);

    let mut peer_b = connect(addr).await;
    authenticate_v3_full(&mut peer_b).await;
    let id_b = join_room(&mut peer_b, game, Some(room_code.clone()), "PeerB", 3)
        .await
        .player_id;

    let mut relay_only = connect(addr).await;
    authenticate_v3_relay_topology_only(&mut relay_only).await;
    let relay_only_id = join_room(&mut relay_only, game, Some(room_code), "RelayOnly", 3)
        .await
        .player_id;

    let mut sockets = [peer_a, peer_b, relay_only];
    finalize_room(&mut sockets).await;

    // Everyone gets GameStarting and then the same no-peer relay plan. A single
    // member lacking the rung's topology floors the whole room (the real
    // `all_support` contract, not a per-member-gated mesh).
    for (socket, who) in sockets.iter_mut().zip(["peer_a", "peer_b", "relay_only"]) {
        next_matching_server_message_within(
            socket,
            SERVER_MESSAGE_TIMEOUT,
            "relay-floor GameStarting",
            |message| match message {
                ServerMessage::GameStarting { .. } => Some(()),
                ServerMessage::NewPeer { .. } => {
                    panic!("{who} must not receive a NewPeer in a relay-floored room")
                }
                _ => None,
            },
        )
        .await;
        let plan = expect_session_plan_strict(socket, who).await;
        assert_relay_floor_plan(&plan, who);
    }

    // Exactly one authoritative plan per v3 member.
    for (socket, who) in sockets.iter_mut().zip(["peer_a", "peer_b", "relay_only"]) {
        expect_no_server_message_within(
            socket,
            SILENCE_WINDOW,
            &format!("{who} after the relay floor (non-mesh member present)"),
        )
        .await;
    }

    // The room still relays game data across all three members (the floor never
    // closes): a single GameData broadcast from the non-mesh member reaches BOTH
    // v3 members byte-identically (GameData broadcasts to every other room
    // member, so this is asserted on both recipients of one send).
    let _ = (id_a, id_b);
    let [peer_a, peer_b, relay_only] = &mut sockets;
    let payload = json!({ "tick": 11, "marker": "floor-relays-all" });
    send(
        relay_only,
        &ClientMessage::GameData {
            class: None,
            key: None,
            data: payload.clone(),
        },
    )
    .await;
    for (socket, who) in [(peer_a, "peer_a"), (peer_b, "peer_b")] {
        next_matching_server_message_within(
            socket,
            SERVER_MESSAGE_TIMEOUT,
            "relayed GameData over the floor",
            |message| match message {
                ServerMessage::GameData {
                    from_player, data, ..
                } => {
                    assert_eq!(
                        from_player, relay_only_id,
                        "{who} saw the wrong GameData sender"
                    );
                    assert_eq!(data, payload, "{who} GameData must relay byte-identically");
                    Some(())
                }
                _ => None,
            },
        )
        .await;
    }
    running_server.shutdown().await;
}
