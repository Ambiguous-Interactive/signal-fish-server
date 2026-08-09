//! End-to-end protocol v3 `TransportStatus` / `PeerTransportStatus` lifecycle
//! tests through the real WebSocket stack, plus a finalization race.
//!
//! These complement the two fan-out cases already living in
//! `tests/v3_multipeer_e2e.rs`
//! (`mesh_n3_transport_status_fan_out_and_dedup` and
//! `mixed_v2_v3_n3_transport_status_v2_member_hears_nothing`) by pinning the
//! *rejection* and *reset* edges of the contract documented in
//! `docs/protocol.md` ("TransportStatus" / "PeerTransportStatus"):
//!
//! 1. `v2_transport_status_report_is_ignored` — a relay-only v2 member's
//!    `TransportStatus` is dropped (v3-gated *sender* side): no fan-out, and
//!    the v2 client neither crashes nor leaks a v3 frame.
//! 2. `unsupported_transport_report_is_ignored` — a v3 client that negotiated
//!    webrtc only reports `direct` (a transport NOT in its negotiated set):
//!    ignored, nothing fans out.
//! 3. `duplicate_report_fans_out_once` — the first `{webrtc,true}` fans out; a
//!    byte-identical duplicate fans out nothing.
//! 4. `state_transitions_each_fan_out_once` — `{webrtc,true}` -> `{webrtc,false}`
//!    -> `{webrtc,true}` yields exactly three ordered `PeerTransportStatus`.
//! 5. `room_change_resets_transport_status_generation` — leaving and rejoining
//!    the same room, then changing rooms on one socket, lets the same first
//!    state fan out in every seated membership.
//! 6. `spectator_transitions_reset_status_without_room_fan_out` — real
//!    spectator entry/leave resets accepted state while remaining roomless;
//!    a later seated join starts fresh and fans out.
//! 7. `reconnect_clears_stored_transport_status` — a re-report of the same state
//!    after a drop+reconnect fans out AGAIN (reconnect cleared stored state).
//! 8. `peer_transport_status_v3_gated_not_capability_gated` — in a mixed room a
//!    v3 reporter's accepted report reaches a relay-only v3 member but NOT the
//!    v2 member.
//! 9. `reporter_excluded_and_roomless_report_is_noop` — the reporter never
//!    receives its own status; a client not in a room records but fans out
//!    nothing (no panic).
//! 10. `finalization_race_single_plan_per_member` — both members of a 2-seat
//!     room send `PlayerReady` concurrently; finalization happens exactly once
//!     and both receive identical (mirrored) `SessionPlan`s.

mod test_helpers;
mod v3_conformance_helpers;
mod websocket_test_helpers;

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use signal_fish_server::config::{AppRegistrationEntry, SessionConfig, TurnConfig};
use signal_fish_server::protocol::{
    ClientMessage, IceServer, PlayerId, RoomJoinedPayload, ServerMessage, SessionPlanPayload,
    Topology, Transport,
};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::{create_router, websocket_route_v3};
use test_helpers::{test_protocol_config, test_server_config, RunningTestServer};
use tokio_tungstenite::connect_async;
use v3_conformance_helpers::{
    await_ready_count, expect_finalize_plan, ready, send, start_game, SERVER_MESSAGE_TIMEOUT,
};
use websocket_test_helpers::{
    expect_no_server_message_within, maybe_next_matching_server_message_within,
    next_matching_server_message_within, WsStream,
};

const APP_ID: &str = "v3-transport-status-app";
const STATIC_STUN_URL: &str = "stun:static.example.com:3478";
/// Window in which a forbidden message would have arrived if it were going to.
const SILENCE_WINDOW: tokio::time::Duration = tokio::time::Duration::from_secs(2);

fn app_entry() -> AppRegistrationEntry {
    AppRegistrationEntry {
        app_id: APP_ID.to_string(),
        app_name: "V3 Transport Status App".to_string(),
        max_rooms: Some(10),
        max_players_per_room: Some(8),
        rate_limit_per_minute: Some(600),
    }
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
        TurnConfig::default(),
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

/// Authenticate as a v3 client that negotiated WebRTC but NOT `direct`
/// (`relay` is always appended by the server). Used to exercise the
/// "report for an unnegotiated transport is ignored" edge.
async fn authenticate_v3_webrtc_only(ws: &mut WsStream) {
    authenticate(
        ws,
        Some(3),
        Some(vec![Transport::WebRtc]),
        Some(vec![Topology::Mesh]),
        Some(3),
    )
    .await;
}

/// Authenticate as a v3 client that negotiated only the relay floor (no
/// non-relay transport, but a real v3 negotiation). Such a member must still
/// RECEIVE `PeerTransportStatus` (delivery is not gated on recipient transport
/// caps) even though its own non-relay reports are unnegotiated.
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

/// Send a `TransportStatus` report from `ws`.
async fn report_transport_status(ws: &mut WsStream, transport: Transport, connected: bool) {
    send(
        ws,
        &ClientMessage::TransportStatus {
            transport,
            connected,
        },
    )
    .await;
}

/// Put a protocol `Ping` behind the preceding client frame and wait for its
/// `Pong`, proving the server processed that frame before state is inspected.
async fn await_client_message_barrier(ws: &mut WsStream) {
    send(ws, &ClientMessage::Ping).await;
    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "Pong barrier", |message| {
        matches!(message, ServerMessage::Pong).then_some(())
    })
    .await;
}

/// Await a `PeerTransportStatus` naming exactly `(peer, transport, connected)`,
/// skipping unrelated join-phase backlog.
async fn expect_peer_transport_status(
    ws: &mut WsStream,
    who: &str,
    peer: PlayerId,
    transport: Transport,
    connected: bool,
) {
    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "PeerTransportStatus",
        |message| match message {
            ServerMessage::PeerTransportStatus {
                peer_id,
                transport: got_transport,
                connected: got_connected,
            } => {
                assert_eq!(peer_id, peer, "{who} saw the wrong reporter");
                assert_eq!(got_transport, transport, "{who} saw the wrong transport");
                assert_eq!(got_connected, connected, "{who} saw the wrong state");
                Some(())
            }
            _ => None,
        },
    )
    .await;
}

/// Drain a v3 observer's join-phase backlog by reading until it has seen
/// `PlayerJoined(last_joiner)` — the join of the LAST member to enter the room.
/// Every earlier `PlayerJoined` / `LobbyStateChanged` is skipped on the way, so
/// after this returns the observer's stream is at the post-fill baseline and a
/// following `expect_no_server_message_within` is a strict assertion about the
/// `TransportStatus` path alone (a v3-only fan-out would NOT match the
/// `PlayerJoined` predicate and would surface as a later forbidden message).
async fn drain_until_player_joined(ws: &mut WsStream, last_joiner: PlayerId, who: &str) {
    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "join-phase backlog drain",
        |message| match message {
            ServerMessage::PlayerJoined { player } if player.id == last_joiner => Some(()),
            // A v3-only fan-out must never appear during the pure join phase; if
            // it does, fail loudly rather than silently skipping it.
            ServerMessage::PeerTransportStatus { .. } => {
                panic!("{who}: unexpected PeerTransportStatus during join-phase drain: {message:?}")
            }
            _ => None,
        },
    )
    .await;

    // The room-full `PlayerJoined` is followed by a `LobbyStateChanged`
    // room-full transition; sweep it (and any trailing lobby updates) so the
    // stream sits exactly at the post-fill baseline. The helper consumes every
    // frame it inspects, so a v3-only fan-out is rejected explicitly above.
    while maybe_next_matching_server_message_within(
        ws,
        SILENCE_WINDOW,
        "trailing lobby update drain",
        |message| match message {
            ServerMessage::LobbyStateChanged { .. } => Some(()),
            ServerMessage::PeerTransportStatus { .. } => {
                panic!("{who}: unexpected PeerTransportStatus during lobby drain: {message:?}")
            }
            _ => None,
        },
    )
    .await
    .is_some()
    {}
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

/// Drive the room-full v2 join and drain the lobby transition so a following
/// silence window is a strict assertion about the fan-out alone.
async fn v2_join_and_settle(
    ws: &mut WsStream,
    game: &str,
    room_code: String,
    player_name: &str,
    max_players: u8,
    who: &str,
) -> PlayerId {
    send(
        ws,
        &ClientMessage::JoinRoom {
            game_name: game.to_string(),
            room_code: Some(room_code),
            player_name: player_name.to_string(),
            max_players: Some(max_players),
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await;
    let payload =
        next_matching_v2_only(ws, "v2 room join response", who, |message| match message {
            ServerMessage::RoomJoined(payload) => Some(payload),
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                panic!("v2 room join failed: {reason} ({error_code:?})")
            }
            _ => None,
        })
        .await;
    payload.player_id
}

// ---------------------------------------------------------------------------
// 1. A v2 sender's TransportStatus is ignored (v3-gated on the SENDER side).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn v2_transport_status_report_is_ignored() {
    // A mixed room: two v3 members and one v2 member. The v2 member sends a
    // `TransportStatus` (a v3-only message). The server must drop it
    // (`UnsupportedProtocolVersion`): no `PeerTransportStatus` fan-out to the v3
    // members, and the v2 sender neither crashes nor receives a v3 frame.
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "tstatus-v2-ignored";
    let legacy_who = "legacy (v2)";

    let mut peer_a = connect(addr).await;
    authenticate_v3_full(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, game, None, "PeerA", 3).await;
    let room_code = joined_a.room_code;

    let mut peer_b = connect(addr).await;
    authenticate_v3_full(&mut peer_b).await;
    join_room(&mut peer_b, game, Some(room_code.clone()), "PeerB", 3).await;

    let mut legacy = connect(addr).await;
    authenticate_v2(&mut legacy).await;
    let legacy_id = v2_join_and_settle(&mut legacy, game, room_code, "Legacy", 3, legacy_who).await;
    // The room entered Lobby EXACTLY ONCE, on the creator (PeerA) joining; the
    // legacy member is a subsequent joiner, so its RoomJoined already reports the
    // Lobby state and NO LobbyStateChanged is broadcast on its join — there is
    // nothing further to drain for the v2 member.

    // Drain the v3 observers' join-phase backlog (they see the v2 member join
    // last) so the silence windows below are strict assertions about the report.
    drain_until_player_joined(&mut peer_a, legacy_id, "peer_a").await;
    drain_until_player_joined(&mut peer_b, legacy_id, "peer_b").await;

    // The v2 member reports a transport state. This is a v3-only message; the
    // server must ignore it entirely.
    report_transport_status(&mut legacy, Transport::Relay, true).await;

    // No v3 member hears any PeerTransportStatus.
    expect_no_server_message_within(&mut peer_a, SILENCE_WINDOW, "peer_a after v2 report").await;
    expect_no_server_message_within(&mut peer_b, SILENCE_WINDOW, "peer_b after v2 report").await;

    // The v2 sender stays alive and leaks no v3 frame: a Ping round-trips.
    send(&mut legacy, &ClientMessage::Ping).await;
    next_matching_v2_only(&mut legacy, "v2 Pong", legacy_who, |message| {
        matches!(message, ServerMessage::Pong).then_some(())
    })
    .await;
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 2. A report for a transport NOT in the negotiated set is ignored.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn unsupported_transport_report_is_ignored() {
    // The reporter negotiated WebRTC only (server still appends `relay`), so
    // `direct` is NOT in its negotiated set. A `{direct, true}` report must be
    // dropped (`UnsupportedTransport`): no fan-out to the other v3 member.
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "tstatus-unsupported-transport";

    let mut reporter = connect(addr).await;
    authenticate_v3_webrtc_only(&mut reporter).await;
    let joined = join_room(&mut reporter, game, None, "Reporter", 3).await;
    let room_code = joined.room_code;

    let mut observer = connect(addr).await;
    authenticate_v3_full(&mut observer).await;
    join_room(&mut observer, game, Some(room_code.clone()), "Observer", 3).await;

    let mut filler = connect(addr).await;
    authenticate_v3_full(&mut filler).await;
    let filler_id = join_room(&mut filler, game, Some(room_code), "Filler", 3)
        .await
        .player_id;

    // Drain the observer's join-phase backlog (it sees the filler join last) so
    // the silence window below is strict.
    drain_until_player_joined(&mut observer, filler_id, "observer").await;

    // `direct` was never negotiated by the reporter -> ignored, no fan-out.
    report_transport_status(&mut reporter, Transport::Direct, true).await;
    expect_no_server_message_within(
        &mut observer,
        SILENCE_WINDOW,
        "observer after unsupported-transport report",
    )
    .await;

    // Sanity: a NEGOTIATED transport from the same reporter DOES fan out, proving
    // the silence above was caused by the unnegotiated transport, not a dead path.
    let reporter_id = joined.player_id;
    report_transport_status(&mut reporter, Transport::WebRtc, true).await;
    expect_peer_transport_status(
        &mut observer,
        "observer",
        reporter_id,
        Transport::WebRtc,
        true,
    )
    .await;
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 3. Duplicate report fans out exactly once.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_report_fans_out_once() {
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "tstatus-dup";

    let mut reporter = connect(addr).await;
    authenticate_v3_full(&mut reporter).await;
    let joined = join_room(&mut reporter, game, None, "Reporter", 3).await;
    let reporter_id = joined.player_id;
    let room_code = joined.room_code;

    let mut observer = connect(addr).await;
    authenticate_v3_full(&mut observer).await;
    join_room(&mut observer, game, Some(room_code.clone()), "Observer", 3).await;

    let mut filler = connect(addr).await;
    authenticate_v3_full(&mut filler).await;
    join_room(&mut filler, game, Some(room_code), "Filler", 3).await;

    // First report is a real state change: it fans out.
    report_transport_status(&mut reporter, Transport::WebRtc, true).await;
    expect_peer_transport_status(
        &mut observer,
        "observer",
        reporter_id,
        Transport::WebRtc,
        true,
    )
    .await;

    // A byte-identical duplicate is dropped at the dedup gate: strict silence.
    report_transport_status(&mut reporter, Transport::WebRtc, true).await;
    expect_no_server_message_within(&mut observer, SILENCE_WINDOW, "observer after duplicate")
        .await;
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 4. Each real state transition fans out exactly once, in order.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn state_transitions_each_fan_out_once() {
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "tstatus-transitions";

    let mut reporter = connect(addr).await;
    authenticate_v3_full(&mut reporter).await;
    let joined = join_room(&mut reporter, game, None, "Reporter", 3).await;
    let reporter_id = joined.player_id;
    let room_code = joined.room_code;

    let mut observer = connect(addr).await;
    authenticate_v3_full(&mut observer).await;
    join_room(&mut observer, game, Some(room_code.clone()), "Observer", 3).await;

    let mut filler = connect(addr).await;
    authenticate_v3_full(&mut filler).await;
    join_room(&mut filler, game, Some(room_code), "Filler", 3).await;

    // {webrtc,true} -> {webrtc,false} -> {webrtc,true}: each REAL transition
    // fans out once, so the observer receives exactly three PeerTransportStatus
    // in order.
    let expected = [true, false, true];
    for &connected in &expected {
        report_transport_status(&mut reporter, Transport::WebRtc, connected).await;
        expect_peer_transport_status(
            &mut observer,
            "observer",
            reporter_id,
            Transport::WebRtc,
            connected,
        )
        .await;
    }

    // Exactly three: no fourth (stale or duplicated) fan-out trails in.
    expect_no_server_message_within(
        &mut observer,
        SILENCE_WINDOW,
        "observer after three transitions",
    )
    .await;
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 5. A room-membership change resets deduplication for the new generation.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn room_change_resets_transport_status_generation() {
    // Issue #260: one physical socket reports {webrtc,true} in room A, leaves,
    // then joins room B under the same player id. Room B must not inherit room
    // A's dedup state: its first identical report is a fresh observation and
    // fans out. The room-B observer must also receive no old-room report before
    // that fresh report, pinning the lifecycle/room-event ordering boundary.
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();

    let mut reporter = connect(addr).await;
    authenticate_v3_full(&mut reporter).await;
    let joined_a = join_room(&mut reporter, "tstatus-room-a", None, "Reporter", 2).await;
    let reporter_id = joined_a.player_id;
    let room_a_code = joined_a.room_code.clone();

    let mut observer_a = connect(addr).await;
    authenticate_v3_full(&mut observer_a).await;
    join_room(
        &mut observer_a,
        "tstatus-room-a",
        Some(room_a_code.clone()),
        "ObserverA",
        2,
    )
    .await;

    report_transport_status(&mut reporter, Transport::WebRtc, true).await;
    expect_peer_transport_status(
        &mut observer_a,
        "observer_a initial membership",
        reporter_id,
        Transport::WebRtc,
        true,
    )
    .await;

    send(&mut reporter, &ClientMessage::LeaveRoom).await;
    next_matching_server_message_within(
        &mut reporter,
        SERVER_MESSAGE_TIMEOUT,
        "RoomLeft(room A)",
        |message| matches!(message, ServerMessage::RoomLeft).then_some(()),
    )
    .await;
    next_matching_server_message_within(
        &mut observer_a,
        SERVER_MESSAGE_TIMEOUT,
        "PlayerLeft(reporter from room A)",
        |message| match message {
            ServerMessage::PlayerLeft { player_id, .. } if player_id == reporter_id => Some(()),
            _ => None,
        },
    )
    .await;

    let rejoined_a = join_room(
        &mut reporter,
        "tstatus-room-a",
        Some(room_a_code),
        "Reporter",
        2,
    )
    .await;
    assert_eq!(rejoined_a.player_id, reporter_id);
    drain_until_player_joined(&mut observer_a, reporter_id, "observer_a rejoin").await;
    report_transport_status(&mut reporter, Transport::WebRtc, true).await;
    expect_peer_transport_status(
        &mut observer_a,
        "observer_a after same-room rejoin",
        reporter_id,
        Transport::WebRtc,
        true,
    )
    .await;

    send(&mut reporter, &ClientMessage::LeaveRoom).await;
    next_matching_server_message_within(
        &mut reporter,
        SERVER_MESSAGE_TIMEOUT,
        "RoomLeft(room A after rejoin)",
        |message| matches!(message, ServerMessage::RoomLeft).then_some(()),
    )
    .await;
    next_matching_server_message_within(
        &mut observer_a,
        SERVER_MESSAGE_TIMEOUT,
        "PlayerLeft(reporter after room A rejoin)",
        |message| match message {
            ServerMessage::PlayerLeft { player_id, .. } if player_id == reporter_id => Some(()),
            _ => None,
        },
    )
    .await;

    let mut observer_b = connect(addr).await;
    authenticate_v3_full(&mut observer_b).await;
    let joined_b = join_room(&mut observer_b, "tstatus-room-b", None, "ObserverB", 2).await;
    let rejoined = join_room(
        &mut reporter,
        "tstatus-room-b",
        Some(joined_b.room_code),
        "Reporter",
        2,
    )
    .await;
    assert_eq!(
        rejoined.player_id, reporter_id,
        "changing rooms on one socket must preserve the connection identity"
    );
    drain_until_player_joined(&mut observer_b, reporter_id, "observer_b").await;

    expect_no_server_message_within(
        &mut observer_b,
        SILENCE_WINDOW,
        "observer_b before its generation's first TransportStatus",
    )
    .await;

    report_transport_status(&mut reporter, Transport::WebRtc, true).await;
    expect_peer_transport_status(
        &mut observer_b,
        "observer_b",
        reporter_id,
        Transport::WebRtc,
        true,
    )
    .await;

    // A new-room report must never be attributed to peers left behind in A.
    expect_no_server_message_within(
        &mut observer_a,
        SILENCE_WINDOW,
        "observer_a after reporter joined room B",
    )
    .await;
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 6. Spectator role changes reset accepted state but remain roomless for fan-out.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn spectator_transitions_reset_status_without_room_fan_out() {
    let (running_server, server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();

    let mut seated = connect(addr).await;
    authenticate_v3_full(&mut seated).await;
    let room = join_room(&mut seated, "tstatus-spectator", None, "Seated", 2).await;

    let mut spectator = connect(addr).await;
    authenticate_v3_full(&mut spectator).await;

    // Establish an identical roomless baseline before the real spectator
    // workflow. Each subsequent accepted report increments this counter once.
    report_transport_status(&mut spectator, Transport::WebRtc, true).await;
    await_client_message_barrier(&mut spectator).await;
    assert_eq!(server.metrics().p2p_established.load(Ordering::Relaxed), 1);

    send(
        &mut spectator,
        &ClientMessage::JoinAsSpectator {
            game_name: "tstatus-spectator".to_string(),
            room_code: room.room_code.clone(),
            spectator_name: "Watcher".to_string(),
        },
    )
    .await;
    let spectator_id = next_matching_server_message_within(
        &mut spectator,
        SERVER_MESSAGE_TIMEOUT,
        "SpectatorJoined",
        |message| match message {
            ServerMessage::SpectatorJoined(payload) => Some(payload.spectator_id),
            ServerMessage::SpectatorJoinFailed { reason, error_code } => {
                panic!("spectator join failed: {reason} ({error_code:?})")
            }
            _ => None,
        },
    )
    .await;
    next_matching_server_message_within(
        &mut seated,
        SERVER_MESSAGE_TIMEOUT,
        "NewSpectatorJoined",
        |message| matches!(message, ServerMessage::NewSpectatorJoined { .. }).then_some(()),
    )
    .await;

    report_transport_status(&mut spectator, Transport::WebRtc, true).await;
    await_client_message_barrier(&mut spectator).await;
    assert_eq!(
        server.metrics().p2p_established.load(Ordering::Relaxed),
        2,
        "successful spectator entry starts a fresh accepted-status generation"
    );
    report_transport_status(&mut spectator, Transport::WebRtc, true).await;
    await_client_message_barrier(&mut spectator).await;
    assert_eq!(
        server.metrics().p2p_established.load(Ordering::Relaxed),
        2,
        "same-spectator-generation duplicates remain suppressed"
    );
    expect_no_server_message_within(
        &mut seated,
        SILENCE_WINDOW,
        "seated member after spectator TransportStatus",
    )
    .await;

    send(&mut spectator, &ClientMessage::LeaveSpectator).await;
    next_matching_server_message_within(
        &mut spectator,
        SERVER_MESSAGE_TIMEOUT,
        "SpectatorLeft",
        |message| matches!(message, ServerMessage::SpectatorLeft { .. }).then_some(()),
    )
    .await;
    next_matching_server_message_within(
        &mut seated,
        SERVER_MESSAGE_TIMEOUT,
        "SpectatorDisconnected",
        |message| match message {
            ServerMessage::SpectatorDisconnected {
                spectator_id: departed,
                ..
            } if departed == spectator_id => Some(()),
            _ => None,
        },
    )
    .await;

    report_transport_status(&mut spectator, Transport::WebRtc, true).await;
    await_client_message_barrier(&mut spectator).await;
    assert_eq!(
        server.metrics().p2p_established.load(Ordering::Relaxed),
        3,
        "successful spectator departure starts a fresh roomless generation"
    );
    expect_no_server_message_within(
        &mut seated,
        SILENCE_WINDOW,
        "seated member after roomless TransportStatus",
    )
    .await;

    let seated_join = join_room(
        &mut spectator,
        "tstatus-spectator",
        Some(room.room_code),
        "FormerWatcher",
        2,
    )
    .await;
    assert_eq!(seated_join.player_id, spectator_id);
    drain_until_player_joined(&mut seated, spectator_id, "seated observer").await;
    report_transport_status(&mut spectator, Transport::WebRtc, true).await;
    expect_peer_transport_status(
        &mut seated,
        "seated observer after former spectator joins",
        spectator_id,
        Transport::WebRtc,
        true,
    )
    .await;
    assert_eq!(server.metrics().p2p_established.load(Ordering::Relaxed), 4);

    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 7. A reconnect clears stored transport status -> a re-report fans out again.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_clears_stored_transport_status() {
    // The reporter establishes {webrtc,true} (fans out), then its socket drops
    // and it reconnects as the SAME player via the wire `Reconnect` flow. A
    // re-report of the very same {webrtc,true} must fan out AGAIN, because the
    // reconnect advanced the membership generation. If suppressed, the dedup
    // gate is leaking pre-reconnect state -> RED.
    let (running_server, server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "tstatus-reconnect";

    let mut observer = connect(addr).await;
    authenticate_v3_full(&mut observer).await;
    let joined_obs = join_room(&mut observer, game, None, "Observer", 3).await;
    let room_code = joined_obs.room_code;

    let mut filler = connect(addr).await;
    authenticate_v3_full(&mut filler).await;
    join_room(&mut filler, game, Some(room_code.clone()), "Filler", 3).await;

    let mut reporter = connect(addr).await;
    authenticate_v3_full(&mut reporter).await;
    let reporter_joined = join_room(&mut reporter, game, Some(room_code), "Reporter", 3).await;
    let reporter_id = reporter_joined.player_id;
    let room_id = reporter_joined.room_id;

    // Pre-drop: {webrtc,true} fans out to the observer.
    report_transport_status(&mut reporter, Transport::WebRtc, true).await;
    expect_peer_transport_status(
        &mut observer,
        "observer",
        reporter_id,
        Transport::WebRtc,
        true,
    )
    .await;

    // Snapshot the member's room info before its socket drops (the reconnect
    // registration needs it, mirroring `v3_multipeer_e2e.rs`).
    let room = server
        .database()
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup")
        .expect("room exists");
    let reporter_info = room
        .players
        .get(&reporter_id)
        .cloned()
        .expect("reporter in room before disconnect");

    // The reporter's socket drops abruptly; the survivors observe PlayerLeft.
    drop(reporter);
    next_matching_server_message_within(
        &mut observer,
        SERVER_MESSAGE_TIMEOUT,
        "PlayerLeft(reporter)",
        |message| match message {
            ServerMessage::PlayerLeft { player_id, .. } if player_id == reporter_id => Some(()),
            _ => None,
        },
    )
    .await;

    // PlayerLeft commits before the disconnect path physically removes the old
    // connection. Join that idempotent teardown before an immediate reconnect,
    // which would otherwise race the server's `has_client` guard.
    server.disconnect_client(&reporter_id).await;

    // Mint a reconnection token through the server handle — obtained in-process
    // here rather than over the wire (same sanctioned pattern as
    // `v3_multipeer_e2e.rs`).
    let token = server
        .reconnection_manager()
        .expect("reconnection enabled")
        // The reporter negotiated v3 and joined once, so its true pre-disconnect
        // incarnation epoch is 1 (not 0 — that would model a sender with no v3
        // stream and make the reconnect resume at epoch 1, colliding with the
        // first incarnation).
        .register_disconnection(reporter_id, room_id, false, Some(reporter_info), 1)
        .await;

    // Reconnect over a FRESH socket using the real wire flow.
    let mut reconnector = connect(addr).await;
    authenticate_v3_full(&mut reconnector).await;
    send(
        &mut reconnector,
        &ClientMessage::Reconnect {
            player_id: reporter_id,
            room_id,
            auth_token: token,
        },
    )
    .await;
    next_matching_server_message_within(
        &mut reconnector,
        SERVER_MESSAGE_TIMEOUT,
        "Reconnected",
        |message| match message {
            ServerMessage::Reconnected(payload) => {
                assert_eq!(payload.player_id, reporter_id, "restored player id");
                Some(())
            }
            ServerMessage::ReconnectionFailed { reason, error_code } => {
                panic!("reconnect failed: {reason} ({error_code:?})")
            }
            _ => None,
        },
    )
    .await;

    // Drain the survivors' PlayerReconnected so the observer's stream is at the
    // post-reconnect baseline before the re-report.
    next_matching_server_message_within(
        &mut observer,
        SERVER_MESSAGE_TIMEOUT,
        "PlayerReconnected",
        |message| match message {
            ServerMessage::PlayerReconnected { player_id, .. } if player_id == reporter_id => {
                Some(())
            }
            _ => None,
        },
    )
    .await;

    // RED: suspected bug — if the dedup gate retained the pre-reconnect
    // {webrtc,true}, this re-report would be suppressed and the observer would
    // hear nothing. The contract (docs/protocol.md: "A reconnect clears the
    // stored state, so a reconnected client's first re-report fans out again")
    // requires a fresh fan-out.
    report_transport_status(&mut reconnector, Transport::WebRtc, true).await;
    expect_peer_transport_status(
        &mut observer,
        "observer",
        reporter_id,
        Transport::WebRtc,
        true,
    )
    .await;
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 8. PeerTransportStatus is v3-gated per recipient but NOT capability-gated:
//    a relay-only v3 member still receives it; a v2 member never does.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn peer_transport_status_v3_gated_not_capability_gated() {
    // Mixed room: a webrtc v3 reporter, a RELAY-ONLY v3 member, and a v2 member.
    // The reporter's accepted {webrtc,true}:
    //   - MUST reach the relay-only v3 member (delivery is not gated on the
    //     recipient's own transport caps — it is informational about a PEER's
    //     data path), AND
    //   - MUST NOT reach the v2 member (v3-gated per recipient, Appendix K).
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "tstatus-gating";
    let legacy_who = "legacy (v2)";

    let mut reporter = connect(addr).await;
    authenticate_v3_full(&mut reporter).await;
    let joined = join_room(&mut reporter, game, None, "Reporter", 3).await;
    let reporter_id = joined.player_id;
    let room_code = joined.room_code;

    // A v3 member that negotiated ONLY the relay floor (still a real v3
    // connection): it must hear the peer's status despite lacking webrtc.
    let mut relay_only_v3 = connect(addr).await;
    authenticate_v3_relay_only(&mut relay_only_v3).await;
    join_room(
        &mut relay_only_v3,
        game,
        Some(room_code.clone()),
        "RelayOnlyV3",
        3,
    )
    .await;

    let mut legacy = connect(addr).await;
    authenticate_v2(&mut legacy).await;
    v2_join_and_settle(&mut legacy, game, room_code, "Legacy", 3, legacy_who).await;
    // The room entered Lobby EXACTLY ONCE, on the creator (Reporter) joining; the
    // legacy member is a subsequent joiner, so its RoomJoined already reports the
    // Lobby state and NO LobbyStateChanged is broadcast on its join — there is
    // nothing further to drain for the v2 member.

    report_transport_status(&mut reporter, Transport::WebRtc, true).await;

    // Half 1: the relay-only v3 member DOES receive the fan-out.
    expect_peer_transport_status(
        &mut relay_only_v3,
        "relay_only_v3",
        reporter_id,
        Transport::WebRtc,
        true,
    )
    .await;

    // Half 2: the v2 member hears NOTHING (strict window, every frame checked
    // against the v2-only variant set) and stays functional.
    expect_no_server_message_within(&mut legacy, SILENCE_WINDOW, "v2 member after fan-out").await;
    send(&mut legacy, &ClientMessage::Ping).await;
    next_matching_v2_only(&mut legacy, "v2 Pong", legacy_who, |message| {
        matches!(message, ServerMessage::Pong).then_some(())
    })
    .await;
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 9. The reporter is excluded from its own fan-out; a room-less report is a
//    recorded no-op (fans out nothing, no panic).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn reporter_excluded_and_roomless_report_is_noop() {
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "tstatus-exclusion";

    // --- Room-less reporter: recorded but fans out nothing, never panics. ---
    let mut roomless = connect(addr).await;
    authenticate_v3_full(&mut roomless).await;
    // No JoinRoom: this connection is not in any room.
    report_transport_status(&mut roomless, Transport::WebRtc, true).await;
    // It hears nothing back (it is the only — and excluded — party) and the
    // server keeps serving it: a Ping round-trips.
    expect_no_server_message_within(&mut roomless, SILENCE_WINDOW, "roomless reporter").await;
    send(&mut roomless, &ClientMessage::Ping).await;
    next_matching_server_message_within(&mut roomless, SERVER_MESSAGE_TIMEOUT, "Pong", |message| {
        matches!(message, ServerMessage::Pong).then_some(())
    })
    .await;

    // --- In-room reporter excluded from its OWN fan-out. ---
    let mut reporter = connect(addr).await;
    authenticate_v3_full(&mut reporter).await;
    let joined = join_room(&mut reporter, game, None, "Reporter", 3).await;
    let reporter_id = joined.player_id;
    let room_code = joined.room_code;

    let mut observer = connect(addr).await;
    authenticate_v3_full(&mut observer).await;
    join_room(&mut observer, game, Some(room_code.clone()), "Observer", 3).await;

    let mut filler = connect(addr).await;
    authenticate_v3_full(&mut filler).await;
    join_room(&mut filler, game, Some(room_code), "Filler", 3).await;

    report_transport_status(&mut reporter, Transport::WebRtc, true).await;
    // The observer hears it (drains the reporter's join-phase backlog too via
    // the fan-out arriving)…
    expect_peer_transport_status(
        &mut observer,
        "observer",
        reporter_id,
        Transport::WebRtc,
        true,
    )
    .await;

    // …but the reporter NEVER receives its own PeerTransportStatus.
    assert!(
        maybe_next_matching_server_message_within(
            &mut reporter,
            SILENCE_WINDOW,
            "reporter self-echo check",
            |message| matches!(message, ServerMessage::PeerTransportStatus { .. }).then_some(()),
        )
        .await
        .is_none(),
        "the reporter must never receive its own PeerTransportStatus"
    );
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 10. Finalization race: both members ready concurrently -> finalize exactly
//    once, identical (mirrored) plans.
// ---------------------------------------------------------------------------

/// Project a plan to the comparable surface: topology, transport, fallback,
/// host, and the set of peer ids (names/flags are per-recipient and asymmetric).
fn plan_shape(plan: &SessionPlanPayload) -> (Topology, Transport, Transport, BTreeSet<PlayerId>) {
    (
        plan.topology,
        plan.transport,
        plan.fallback,
        plan.peers.iter().map(|peer| peer.player_id).collect(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn finalization_race_single_plan_per_member() {
    // A 2-seat room finalizes the instant the second PlayerReady lands. Both
    // members fire PlayerReady CONCURRENTLY (tokio::join!) so the finalize toggle
    // races. The room must finalize EXACTLY once: each member receives exactly
    // one GameStarting + one SessionPlan, and the two plans are mutually
    // consistent (each lists the other as its sole peer with a glare-correct,
    // antisymmetric initiate flag). A double finalize would surface as a second
    // plan in either silence window below.
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "tstatus-finalize-race";

    let mut peer_a = connect(addr).await;
    authenticate_v3_full(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, game, None, "PeerA", 2).await;
    let (room_code, id_a) = (joined_a.room_code, joined_a.player_id);

    let mut peer_b = connect(addr).await;
    authenticate_v3_full(&mut peer_b).await;
    let id_b = join_room(&mut peer_b, game, Some(room_code), "PeerB", 2)
        .await
        .player_id;

    // Both readies fire concurrently — readiness no longer auto-starts, so this
    // races the ready toggles, not the finalize.
    tokio::join!(ready(&mut peer_a), ready(&mut peer_b));

    // Pace the lobby-ready backlog past both toggles before starting, so a
    // straggling LobbyStateChanged cannot masquerade as a forbidden second
    // message in the silence windows below.
    await_ready_count(&mut peer_a, 2).await;
    await_ready_count(&mut peer_b, 2).await;

    // With both members ready, an explicit StartGame finalizes the room exactly
    // once; each member receives exactly one GameStarting + one SessionPlan.
    start_game(&mut peer_a).await;

    let plan_a = expect_finalize_plan(&mut peer_a, "peer_a").await;
    let plan_b = expect_finalize_plan(&mut peer_b, "peer_b").await;

    // Exactly one finalize: each side gets exactly one plan, no second one.
    expect_no_server_message_within(&mut peer_a, SILENCE_WINDOW, "peer_a after its plan").await;
    expect_no_server_message_within(&mut peer_b, SILENCE_WINDOW, "peer_b after its plan").await;

    // Both plans describe the SAME finalized session (same topology/transport/
    // fallback and mirrored peer sets).
    assert_eq!(
        plan_a.topology,
        Topology::Mesh,
        "race must finalize to mesh"
    );
    assert_eq!(plan_a.transport, Transport::WebRtc);
    let (shape_a_t, shape_a_x, shape_a_f, peers_a) = plan_shape(&plan_a);
    let (shape_b_t, shape_b_x, shape_b_f, peers_b) = plan_shape(&plan_b);
    assert_eq!(
        (shape_a_t, shape_a_x, shape_a_f),
        (shape_b_t, shape_b_x, shape_b_f),
        "both members must see the same room-wide topology/transport/fallback"
    );
    assert_eq!(
        peers_a,
        BTreeSet::from([id_b]),
        "peer_a's plan lists exactly peer_b"
    );
    assert_eq!(
        peers_b,
        BTreeSet::from([id_a]),
        "peer_b's plan lists exactly peer_a"
    );

    // Glare is antisymmetric across the SAME pair regardless of finalize order
    // (the smaller UUID offers) — a double finalize with re-derived flags could
    // disagree here.
    assert_eq!(plan_a.peers.len(), 1);
    assert_eq!(plan_b.peers.len(), 1);
    assert_eq!(
        plan_a.peers[0].initiate,
        id_a < id_b,
        "peer_a's initiate flag toward peer_b follows the UUID glare rule"
    );
    assert_eq!(
        plan_b.peers[0].initiate,
        id_b < id_a,
        "peer_b's initiate flag toward peer_a follows the UUID glare rule"
    );
    assert_ne!(
        plan_a.peers[0].initiate, plan_b.peers[0].initiate,
        "exactly one side of the pair offers (antisymmetric glare)"
    );
    running_server.shutdown().await;
}
