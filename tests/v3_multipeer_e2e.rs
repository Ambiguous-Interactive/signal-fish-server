//! End-to-end protocol v3 multi-peer (N >= 3) signaling conformance tests
//! through the real WebSocket stack (the in-repo signaling conformance suite,
//! extended past the existing two-peer suites).
//!
//! Each test boots an in-process axum server on `127.0.0.1:0` (the harness from
//! `tests/v3_replan_e2e.rs`), drives N real `tokio-tungstenite` clients through
//! the full v3 flow (Authenticate -> JoinRoom -> paced PlayerReady ->
//! GameStarting -> SessionPlan), and pins the exact payload contracts:
//!
//! 1. `mesh_n3_full_glare_matrix_and_pairwise_signaling` — 3-peer mesh plans
//!    carry the full antisymmetric glare matrix (smaller UUID offers) and every
//!    ordered pair can relay a distinct opaque signal byte-identically.
//! 2. `mesh_n4_glare_matrix` — the same matrix property at N=4 (6 unordered
//!    pairs, exactly one offerer each).
//! 3. `host_n4_star_property` — a 4-peer host room is a strict star: the host
//!    answers all 3 clients, each client offers only to the host, clients never
//!    appear in each other's plans.
//! 4. `mixed_v2_v3_n3_relay_floor_is_explicit_only_for_v3` — one v2 member
//!    floors the room to relay: everyone gets `GameStarting`, each v3 member
//!    gets an explicit no-peer relay plan, and v2 sees no v3 variant.
//! 5. `host_n4_failover_full_matrix` — the host socket dying re-elects the
//!    earliest-joined remaining member and re-issues exactly one fresh
//!    star-correct plan to each survivor.
//! 6. `mesh_n3_seat_fill_late_join` — a seat-filling joiner of a live mesh
//!    session gets `RoomJoined(finalized)` + a tailored plan, and existing
//!    members get `PlayerJoined` + complete glare-correct plan refreshes.
//! 7. `mesh_n3_reconnect_full_flow` — a dropped member reconnects over a fresh
//!    socket (wire `Reconnect` flow): `Reconnected` + fresh plan for the
//!    reconnector, `PlayerReconnected` + refreshed plans for the survivors, and
//!    post-reconnect signals run under the restored player id.
//! 8. `host_n4_cascade_failover` — two consecutive host deaths: each wave
//!    re-elects the earliest-joined survivor and re-issues exactly one fresh
//!    star-correct plan per survivor, and the star stays live through the
//!    third host.
//! 9. `mesh_n3_transport_status_fan_out_and_dedup` — a v3 member's
//!    `TransportStatus` state change is fanned out to both other v3 members as
//!    `PeerTransportStatus` (never echoed to the reporter); a byte-identical
//!    duplicate fans out nothing, and the next real transition fans out again.
//! 10. `mixed_v2_v3_n3_transport_status_v2_member_hears_nothing` — in a mixed
//!     room the v3 peer receives the `PeerTransportStatus` fan-out while the v2
//!     member observes strict silence and stays fully functional.

mod test_helpers;
mod v3_conformance_helpers;
mod websocket_test_helpers;

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::json;
use signal_fish_server::config::{AppRegistrationEntry, SessionConfig, TurnConfig};
use signal_fish_server::protocol::{
    ClientMessage, IceServer, LobbyState, PlayerId, RoomJoinedPayload, ServerMessage,
    SessionPlanPayload, Topology, Transport,
};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::{create_router, websocket_route_v3};
use test_helpers::{test_protocol_config, test_server_config, RunningTestServer};
use tokio_tungstenite::connect_async;
use v3_conformance_helpers::{
    assert_full_mesh_glare_matrix, await_ready_count, expect_finalize_plan,
    expect_session_plan_strict, expect_signal, ordered_pairs, ready, relay_one_signal, send,
    start_game, SERVER_MESSAGE_TIMEOUT,
};
use websocket_test_helpers::{
    expect_no_server_message_within, maybe_next_matching_server_message_within,
    next_matching_server_message_within, WsStream,
};

const APP_ID: &str = "v3-multipeer-app";
const STATIC_STUN_URL: &str = "stun:static.example.com:3478";
const TURN_STUN_URL: &str = "stun:stun.l.google.com:19302";
/// Window in which a forbidden message would have arrived if it were going to.
const SILENCE_WINDOW: tokio::time::Duration = tokio::time::Duration::from_secs(2);

fn app_entry() -> AppRegistrationEntry {
    AppRegistrationEntry {
        app_id: APP_ID.to_string(),
        app_name: "V3 Multipeer App".to_string(),
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
        "multipeer e2e fixtures intentionally exercise the default [turn] STUN URL"
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

fn assert_static_then_default_stun_ice(ice_servers: &[IceServer]) {
    assert_eq!(
        ice_servers.len(),
        2,
        "webrtc plans carry static ICE followed by the default [turn] STUN entry"
    );
    assert_eq!(ice_servers[0].urls, vec![STATIC_STUN_URL]);
    assert_eq!(ice_servers[1].urls, vec![TURN_STUN_URL]);
    assert!(
        ice_servers
            .iter()
            .all(|server| server.username.is_none() && server.credential.is_none()),
        "no TURN credentials are minted when [turn] is disabled"
    );
}

/// Boot the production router (`/v2` nest + `/v3/ws` alias) around a server
/// built with the given `SessionConfig`, returning the bound address and the
/// server handle (the reconnect test mints the token in-process via the handle
/// rather than receiving it over the wire, like `tests/v3_signaling_e2e.rs`).
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

/// Drive the paced all-ready handshake, then explicitly start the game: every
/// ready is observed by every member before the next fires, and once every
/// current player is ready an explicit `StartGame` (from the creator socket)
/// triggers finalization. Readiness alone no longer auto-starts the room.
async fn finalize_room(sockets: &mut [WsStream]) {
    let member_count = sockets.len();
    for ready_index in 0..member_count {
        ready(&mut sockets[ready_index]).await;
        for socket in sockets.iter_mut() {
            await_ready_count(socket, ready_index + 1).await;
        }
    }
    // Every current player is ready; finalize with one explicit StartGame from
    // the creator socket (conformance rooms use `supports_authority: false`, so
    // any member may start).
    start_game(&mut sockets[0]).await;
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

/// Assert the strict star property of a host-topology plan set: the host
/// answers every client (`initiate: false`, peers not authority), every client
/// lists EXACTLY the host (`initiate: true`, `is_authority: true`), and every
/// plan names the same elected host.
fn assert_host_star_plans(
    host_id: PlayerId,
    host_plan: &SessionPlanPayload,
    client_plans: &[(PlayerId, &SessionPlanPayload)],
) {
    let client_ids: BTreeSet<PlayerId> = client_plans.iter().map(|(id, _)| *id).collect();
    assert_eq!(
        client_ids.len(),
        client_plans.len(),
        "duplicate client ids in plans"
    );

    assert_eq!(host_plan.topology, Topology::Host);
    assert_eq!(host_plan.transport, Transport::WebRtc);
    assert_eq!(host_plan.fallback, Transport::Relay);
    assert_eq!(
        host_plan.host,
        Some(host_id),
        "the host's own plan must name it as the elected host"
    );
    assert_static_then_default_stun_ice(&host_plan.ice_servers);
    let host_peer_ids: BTreeSet<PlayerId> =
        host_plan.peers.iter().map(|peer| peer.player_id).collect();
    assert_eq!(
        host_peer_ids, client_ids,
        "the host's plan must list exactly the clients"
    );
    assert_eq!(
        host_plan.peers.len(),
        client_ids.len(),
        "the host's plan must not repeat clients"
    );
    for peer in &host_plan.peers {
        assert!(
            !peer.initiate,
            "the host initiates to none — clients offer to it"
        );
        assert!(
            !peer.is_authority,
            "the host's peers are plain clients, never the authority"
        );
    }

    for (client_id, plan) in client_plans {
        assert_eq!(plan.topology, Topology::Host);
        assert_eq!(plan.transport, Transport::WebRtc);
        assert_eq!(plan.fallback, Transport::Relay);
        assert_eq!(
            plan.host,
            Some(host_id),
            "client {client_id} plan must name the elected host"
        );
        assert_static_then_default_stun_ice(&plan.ice_servers);
        assert_eq!(
            plan.peers.len(),
            1,
            "client {client_id} must list exactly the host — clients never appear in each other's plans"
        );
        let host_peer = &plan.peers[0];
        assert_eq!(host_peer.player_id, host_id);
        assert!(
            host_peer.initiate,
            "client {client_id} offers to the host (fixed star direction)"
        );
        assert!(
            host_peer.is_authority,
            "the host peer is the session authority in a star"
        );
    }
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

#[tokio::test(flavor = "multi_thread")]
async fn mesh_n3_full_glare_matrix_and_pairwise_signaling() {
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "multipeer-mesh3";

    // Three v3+webrtc clients fill a 3-seat mesh room.
    let mut peer_a = connect(addr).await;
    authenticate_v3_full(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, game, None, "PeerA", 3).await;
    let (room_code, id_a) = (joined_a.room_code, joined_a.player_id);

    let mut peer_b = connect(addr).await;
    authenticate_v3_full(&mut peer_b).await;
    let id_b = join_room(&mut peer_b, game, Some(room_code.clone()), "PeerB", 3)
        .await
        .player_id;

    let mut peer_c = connect(addr).await;
    authenticate_v3_full(&mut peer_c).await;
    let id_c = join_room(&mut peer_c, game, Some(room_code), "PeerC", 3)
        .await
        .player_id;

    let ids = [id_a, id_b, id_c];
    let mut sockets = [peer_a, peer_b, peer_c];
    finalize_room(&mut sockets).await;

    // Everyone receives GameStarting then a SessionPlan with exactly 2 peers.
    let mut plans = Vec::new();
    for (index, socket) in sockets.iter_mut().enumerate() {
        let plan = expect_finalize_plan(socket, &format!("member {}", ids[index])).await;
        assert_eq!(plan.topology, Topology::Mesh);
        assert_eq!(plan.transport, Transport::WebRtc);
        assert_eq!(plan.fallback, Transport::Relay);
        assert!(plan.host.is_none(), "mesh plans elect no host");
        assert_eq!(plan.peers.len(), 2, "3-peer mesh lists exactly 2 peers");
        assert_static_then_default_stun_ice(&plan.ice_servers);
        plans.push(plan);
    }
    let plan_refs: Vec<(PlayerId, &SessionPlanPayload)> = ids
        .iter()
        .copied()
        .zip(plans.iter().map(|plan| &**plan))
        .collect();
    assert_full_mesh_glare_matrix(&plan_refs);

    // Relay a distinct opaque signal across EVERY ordered pair (6 sends).
    for (from_idx, to_idx) in ordered_pairs(3) {
        relay_one_signal(&mut sockets, &ids, from_idx, to_idx).await;
    }
    running_server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mesh_n4_glare_matrix() {
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "multipeer-mesh4";

    let mut creator = connect(addr).await;
    authenticate_v3_full(&mut creator).await;
    let joined = join_room(&mut creator, game, None, "Peer1", 4).await;
    let room_code = joined.room_code;

    let mut ids = vec![joined.player_id];
    let mut sockets = vec![creator];
    for name in ["Peer2", "Peer3", "Peer4"] {
        let mut socket = connect(addr).await;
        authenticate_v3_full(&mut socket).await;
        ids.push(
            join_room(&mut socket, game, Some(room_code.clone()), name, 4)
                .await
                .player_id,
        );
        sockets.push(socket);
    }

    finalize_room(&mut sockets).await;

    // 4 plans x 3 peers; 6 unordered pairs, each with exactly one offerer
    // (the smaller UUID).
    let mut plans = Vec::new();
    for (index, socket) in sockets.iter_mut().enumerate() {
        let plan = expect_finalize_plan(socket, &format!("member {}", ids[index])).await;
        assert_eq!(plan.topology, Topology::Mesh);
        assert_eq!(plan.transport, Transport::WebRtc);
        assert_eq!(plan.peers.len(), 3, "4-peer mesh lists exactly 3 peers");
        plans.push(plan);
    }
    let plan_refs: Vec<(PlayerId, &SessionPlanPayload)> = ids
        .iter()
        .copied()
        .zip(plans.iter().map(|plan| &**plan))
        .collect();
    assert_full_mesh_glare_matrix(&plan_refs);

    // One spot-check pair proves the pairing is live end to end.
    relay_one_signal(&mut sockets, &ids, 0, 1).await;
    running_server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn host_n4_star_property() {
    let (running_server, _server) = start_server_with_session(host_session_config()).await;
    let addr = running_server.addr();
    let game = "multipeer-host4";

    // First joiner (room creator; the room has no designated authority) wins
    // the host election at finalize.
    let mut creator = connect(addr).await;
    authenticate_v3_full(&mut creator).await;
    let joined = join_room(&mut creator, game, None, "Host", 4).await;
    let (room_code, host_id) = (joined.room_code, joined.player_id);

    let mut ids = vec![host_id];
    let mut sockets = vec![creator];
    for name in ["ClientA", "ClientB", "ClientC"] {
        let mut socket = connect(addr).await;
        authenticate_v3_full(&mut socket).await;
        ids.push(
            join_room(&mut socket, game, Some(room_code.clone()), name, 4)
                .await
                .player_id,
        );
        sockets.push(socket);
    }

    finalize_room(&mut sockets).await;

    let mut plans = Vec::new();
    for (index, socket) in sockets.iter_mut().enumerate() {
        plans.push(expect_finalize_plan(socket, &format!("member {}", ids[index])).await);
    }
    let client_plans: Vec<(PlayerId, &SessionPlanPayload)> = ids
        .iter()
        .copied()
        .zip(plans.iter().map(|plan| &**plan))
        .skip(1)
        .collect();
    assert_host_star_plans(host_id, &plans[0], &client_plans);

    // Spot-check signaling along both star directions: client -> host and
    // host -> client.
    relay_one_signal(&mut sockets, &ids, 1, 0).await;
    relay_one_signal(&mut sockets, &ids, 0, 2).await;
    running_server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mixed_v2_v3_n3_relay_floor_is_explicit_only_for_v3() {
    // A mesh-preferring server, but one pure-v2 member floors the whole room
    // to relay: everyone receives GameStarting and NOBODY receives a
    // SessionPlan or NewPeer (the per-recipient v3 gate).
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "multipeer-mixed3";
    let legacy_who = "legacy (v2)";

    let mut peer_a = connect(addr).await;
    authenticate_v3_full(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, game, None, "PeerA", 3).await;
    let room_code = joined_a.room_code;

    let mut peer_b = connect(addr).await;
    authenticate_v3_full(&mut peer_b).await;
    let _id_b = join_room(&mut peer_b, game, Some(room_code.clone()), "PeerB", 3)
        .await
        .player_id;

    // The v2 client: every message it reads from here on is checked against
    // the known-v2 variant set.
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
    next_matching_v2_only(
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

    // Paced readies; the v2 client's lobby updates run through the v2-only
    // guard as well.
    ready(&mut peer_a).await;
    await_ready_count(&mut peer_a, 1).await;
    await_ready_count(&mut peer_b, 1).await;
    next_matching_v2_only(
        &mut legacy,
        "v2 lobby ready count 1",
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
        "v2 lobby ready count 2",
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
    // Drain the all-ready (3/3) lobby update on the v3 sockets before the
    // explicit start; the v2 client's copy runs through the v2-only guard.
    await_ready_count(&mut peer_a, 3).await;
    await_ready_count(&mut peer_b, 3).await;
    next_matching_v2_only(
        &mut legacy,
        "v2 lobby ready count 3",
        legacy_who,
        |message| match message {
            ServerMessage::LobbyStateChanged { ready_players, .. } if ready_players.len() == 3 => {
                Some(())
            }
            _ => None,
        },
    )
    .await;
    // Readiness no longer auto-starts: the creator explicitly starts the game.
    send(&mut peer_a, &ClientMessage::StartGame).await;

    // Everyone gets GameStarting; each v3 member then gets the authoritative
    // relay reset, while v2 remains plan-free.
    for (ws, who) in [(&mut peer_a, "peer_a"), (&mut peer_b, "peer_b")] {
        next_matching_server_message_within(
            ws,
            SERVER_MESSAGE_TIMEOUT,
            "relay-floor GameStarting",
            |message| match message {
                ServerMessage::GameStarting { .. } => Some(()),
                ServerMessage::NewPeer { .. } => {
                    panic!("{who} must not receive NewPeer in a relay-floor room")
                }
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

    // Exactly one plan per v3 member and none for v2.
    expect_no_server_message_within(&mut peer_a, SILENCE_WINDOW, "peer_a after relay floor").await;
    expect_no_server_message_within(&mut peer_b, SILENCE_WINDOW, "peer_b after relay floor").await;
    expect_no_server_message_within(&mut legacy, SILENCE_WINDOW, "v2 client after relay floor")
        .await;
    running_server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn host_n4_failover_full_matrix() {
    let (running_server, _server) = start_server_with_session(host_session_config()).await;
    let addr = running_server.addr();
    let game = "multipeer-failover4";

    let mut creator = connect(addr).await;
    authenticate_v3_full(&mut creator).await;
    let joined = join_room(&mut creator, game, None, "Host", 4).await;
    let (room_code, host_id) = (joined.room_code, joined.player_id);

    let mut ids = vec![host_id];
    let mut sockets = vec![creator];
    for name in ["ClientA", "ClientB", "ClientC"] {
        let mut socket = connect(addr).await;
        authenticate_v3_full(&mut socket).await;
        ids.push(
            join_room(&mut socket, game, Some(room_code.clone()), name, 4)
                .await
                .player_id,
        );
        sockets.push(socket);
    }

    finalize_room(&mut sockets).await;
    for (index, socket) in sockets.iter_mut().enumerate() {
        let plan = expect_finalize_plan(socket, &format!("member {}", ids[index])).await;
        assert_eq!(plan.host, Some(host_id), "initial plans name the creator");
    }

    // The HOST socket closes abruptly (no WebSocket close frame): drop the
    // stream so only the TCP teardown reaches the server.
    let host_socket = sockets.remove(0);
    drop(host_socket);
    let remaining_ids = [ids[1], ids[2], ids[3]];
    let new_host_id = remaining_ids[0]; // earliest-joined remaining member

    // Each remaining client receives PlayerLeft(host) AND exactly one fresh
    // SessionPlan naming the re-elected host.
    let mut replans = Vec::new();
    for (index, socket) in sockets.iter_mut().enumerate() {
        let who = format!("survivor {}", remaining_ids[index]);
        expect_player_left(socket, host_id, &who).await;
        replans.push(expect_session_plan_strict(socket, &who).await);
    }

    let client_replans: Vec<(PlayerId, &SessionPlanPayload)> = remaining_ids
        .iter()
        .copied()
        .zip(replans.iter().map(|plan| &**plan))
        .skip(1)
        .collect();
    assert_host_star_plans(new_host_id, &replans[0], &client_replans);

    // Exactly one re-plan each: strict silence afterwards.
    for (index, socket) in sockets.iter_mut().enumerate() {
        expect_no_server_message_within(
            socket,
            SILENCE_WINDOW,
            &format!(
                "survivor {} after the failover re-plan",
                remaining_ids[index]
            ),
        )
        .await;
    }
    running_server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn host_n4_cascade_failover() {
    // Two consecutive host deaths in one running session: each wave must
    // re-elect the earliest-joined survivor and re-issue exactly one fresh
    // star-correct plan per survivor. The second wave exercises failover from
    // the post-failover session state (host-1 long gone, host-2 elected at
    // wave 1) rather than the finalize-time election. Note the stored-host
    // *rewrite* itself is discriminated at the unit layer
    // (`non_host_departure_heals_already_missing_host`); end-to-end, a
    // re-plan that re-elected without rewriting the stored host would also
    // pass this test, since the dead host stays absent either way.
    let (running_server, _server) = start_server_with_session(host_session_config()).await;
    let addr = running_server.addr();
    let game = "multipeer-cascade4";

    let mut creator = connect(addr).await;
    authenticate_v3_full(&mut creator).await;
    let joined = join_room(&mut creator, game, None, "Host1", 4).await;
    let (room_code, host1_id) = (joined.room_code, joined.player_id);

    let mut ids = vec![host1_id];
    let mut sockets = vec![creator];
    for name in ["ClientA", "ClientB", "ClientC"] {
        let mut socket = connect(addr).await;
        authenticate_v3_full(&mut socket).await;
        ids.push(
            join_room(&mut socket, game, Some(room_code.clone()), name, 4)
                .await
                .player_id,
        );
        sockets.push(socket);
    }

    // Finalize and assert the initial 4-member star around host-1.
    finalize_room(&mut sockets).await;
    let mut plans = Vec::new();
    for (index, socket) in sockets.iter_mut().enumerate() {
        plans.push(expect_finalize_plan(socket, &format!("member {}", ids[index])).await);
    }
    let client_plans: Vec<(PlayerId, &SessionPlanPayload)> = ids
        .iter()
        .copied()
        .zip(plans.iter().map(|plan| &**plan))
        .skip(1)
        .collect();
    assert_host_star_plans(host1_id, &plans[0], &client_plans);

    // WAVE 1: HOST-1's socket drops abruptly (no close frame).
    drop(sockets.remove(0));
    let wave1_ids = [ids[1], ids[2], ids[3]];
    let host2_id = wave1_ids[0]; // earliest-joined survivor

    // Each survivor receives PlayerLeft(host-1) AND exactly one fresh plan
    // naming host-2; the 3-member star matrix must hold in full.
    let mut wave1_plans = Vec::new();
    for (index, socket) in sockets.iter_mut().enumerate() {
        let who = format!("wave-1 survivor {}", wave1_ids[index]);
        expect_player_left(socket, host1_id, &who).await;
        wave1_plans.push(expect_session_plan_strict(socket, &who).await);
    }
    let wave1_client_plans: Vec<(PlayerId, &SessionPlanPayload)> = wave1_ids
        .iter()
        .copied()
        .zip(wave1_plans.iter().map(|plan| &**plan))
        .skip(1)
        .collect();
    assert_host_star_plans(host2_id, &wave1_plans[0], &wave1_client_plans);

    // Exactly one re-plan each: strict silence after wave 1.
    for (index, socket) in sockets.iter_mut().enumerate() {
        expect_no_server_message_within(
            socket,
            SILENCE_WINDOW,
            &format!(
                "wave-1 survivor {} after the first re-plan",
                wave1_ids[index]
            ),
        )
        .await;
    }

    // WAVE 2: HOST-2 (the freshly re-elected host) drops too.
    drop(sockets.remove(0));
    let wave2_ids = [wave1_ids[1], wave1_ids[2]];
    let host3_id = wave2_ids[0]; // earliest-joined of the final pair

    // The remaining 2 receive PlayerLeft(host-2) AND a SECOND fresh plan
    // naming host-3; the 2-member star matrix must hold in full.
    let mut wave2_plans = Vec::new();
    for (index, socket) in sockets.iter_mut().enumerate() {
        let who = format!("wave-2 survivor {}", wave2_ids[index]);
        expect_player_left(socket, host2_id, &who).await;
        wave2_plans.push(expect_session_plan_strict(socket, &who).await);
    }
    let wave2_client_plans: Vec<(PlayerId, &SessionPlanPayload)> = wave2_ids
        .iter()
        .copied()
        .zip(wave2_plans.iter().map(|plan| &**plan))
        .skip(1)
        .collect();
    assert_host_star_plans(host3_id, &wave2_plans[0], &wave2_client_plans);

    // Exactly one re-plan each: strict silence after wave 2 (no duplicate or
    // stale plans from the first failover may trail in).
    for (index, socket) in sockets.iter_mut().enumerate() {
        expect_no_server_message_within(
            socket,
            SILENCE_WINDOW,
            &format!(
                "wave-2 survivor {} after the second re-plan",
                wave2_ids[index]
            ),
        )
        .await;
    }

    // Spot-check the surviving star edge both ways: client -> host-3 and
    // host-3 -> client.
    relay_one_signal(&mut sockets, &wave2_ids, 1, 0).await;
    relay_one_signal(&mut sockets, &wave2_ids, 0, 1).await;
    running_server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mesh_n3_seat_fill_late_join() {
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "multipeer-seatfill3";

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
            assert_eq!(plan.topology, Topology::Mesh);
        }
        [peer_a, peer_b, leaver] = sockets;
    }

    // One member leaves the live session explicitly (RoomLeft); a mesh
    // departure re-plans nothing for the survivors.
    send(&mut leaver, &ClientMessage::LeaveRoom).await;
    next_matching_server_message_within(
        &mut leaver,
        SERVER_MESSAGE_TIMEOUT,
        "RoomLeft",
        |message| match message {
            ServerMessage::RoomLeft => Some(()),
            other => panic!("leaver expected RoomLeft, got {other:?}"),
        },
    )
    .await;
    expect_player_left(&mut peer_a, leaver_id, "peer_a").await;
    expect_player_left(&mut peer_b, leaver_id, "peer_b").await;

    // A NEW v3 client fills the seat in the still-finalized room.
    let mut joiner = connect(addr).await;
    authenticate_v3_full(&mut joiner).await;
    let joiner_joined = join_room(&mut joiner, game, Some(room_code), "Joiner", 3).await;
    let joiner_id = joiner_joined.player_id;
    assert_eq!(
        joiner_joined.lobby_state,
        LobbyState::Finalized,
        "a seat-filling joiner of a finalized room must see lobby_state: finalized"
    );

    // The joiner's very next message is its tailored SessionPlan — never a
    // NewPeer — listing the 2 remaining peers with glare-correct flags.
    let joiner_plan = next_matching_server_message_within(
        &mut joiner,
        SERVER_MESSAGE_TIMEOUT,
        "joiner late-join SessionPlan",
        |message| match message {
            ServerMessage::SessionPlan(plan) => Some(plan),
            ServerMessage::NewPeer { .. } => {
                panic!("the joiner must receive its pairing via SessionPlan, not NewPeer")
            }
            other => panic!("joiner expected SessionPlan, got {other:?}"),
        },
    )
    .await;
    assert_eq!(joiner_plan.topology, Topology::Mesh);
    assert_eq!(joiner_plan.transport, Transport::WebRtc);
    assert_static_then_default_stun_ice(&joiner_plan.ice_servers);
    let joiner_peer_ids: BTreeSet<PlayerId> = joiner_plan
        .peers
        .iter()
        .map(|peer| peer.player_id)
        .collect();
    assert_eq!(
        joiner_peer_ids,
        BTreeSet::from([id_a, id_b]),
        "the joiner's plan lists exactly the 2 remaining members"
    );
    assert_eq!(joiner_plan.peers.len(), 2);
    for peer in &joiner_plan.peers {
        assert_eq!(
            peer.initiate,
            joiner_id < peer.player_id,
            "the joiner's initiate flag toward {} follows the UUID glare rule",
            peer.player_id
        );
    }

    // Each incumbent receives PlayerJoined followed by its complete refreshed
    // plan. Validate the global glare matrix across all three views.
    let mut incumbent_plans = Vec::new();
    for (socket, member_id, who) in [(&mut peer_a, id_a, "peer_a"), (&mut peer_b, id_b, "peer_b")] {
        expect_player_joined(socket, joiner_id, who).await;
        let plan = expect_session_plan_strict(socket, who).await;
        incumbent_plans.push((member_id, plan));
    }
    let refreshed = [
        (joiner_id, &*joiner_plan),
        (incumbent_plans[0].0, &*incumbent_plans[0].1),
        (incumbent_plans[1].0, &*incumbent_plans[1].1),
    ];
    assert_full_mesh_glare_matrix(&refreshed);

    // Exactly one authoritative plan each: strict silence windows.
    expect_no_server_message_within(&mut joiner, SILENCE_WINDOW, "joiner after its plan").await;
    expect_no_server_message_within(&mut peer_a, SILENCE_WINDOW, "peer_a after its refresh").await;
    expect_no_server_message_within(&mut peer_b, SILENCE_WINDOW, "peer_b after its refresh").await;

    // Prove the new pairing is live: signal joiner <-> one existing member.
    let signal_to_a = json!({ "Offer": "v=0\r\no=- seatfill joiner->a\r\n" });
    send(
        &mut joiner,
        &ClientMessage::Signal {
            to: id_a,
            generation: joiner_plan.generation,
            signal: signal_to_a.clone(),
        },
    )
    .await;
    let (from, signal) = expect_signal(&mut peer_a, "peer_a").await;
    assert_eq!(from, joiner_id);
    assert_eq!(signal, signal_to_a);

    let answer_to_joiner = json!({ "Answer": "v=0\r\no=- seatfill a->joiner\r\n" });
    send(
        &mut peer_a,
        &ClientMessage::Signal {
            to: joiner_id,
            generation: joiner_plan.generation,
            signal: answer_to_joiner.clone(),
        },
    )
    .await;
    let (from, signal) = expect_signal(&mut joiner, "joiner").await;
    assert_eq!(from, id_a);
    assert_eq!(signal, answer_to_joiner);
    running_server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mesh_n3_reconnect_full_flow() {
    let (running_server, server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "multipeer-reconnect3";

    let mut peer_a = connect(addr).await;
    authenticate_v3_full(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, game, None, "PeerA", 3).await;
    let (room_code, id_a) = (joined_a.room_code, joined_a.player_id);

    let mut peer_b = connect(addr).await;
    authenticate_v3_full(&mut peer_b).await;
    let id_b = join_room(&mut peer_b, game, Some(room_code.clone()), "PeerB", 3)
        .await
        .player_id;

    let mut dropper = connect(addr).await;
    authenticate_v3_full(&mut dropper).await;
    let dropper_joined = join_room(&mut dropper, game, Some(room_code), "Dropper", 3).await;
    let (dropper_id, room_id) = (dropper_joined.player_id, dropper_joined.room_id);

    {
        let mut sockets = [peer_a, peer_b, dropper];
        finalize_room(&mut sockets).await;
        for (socket, who) in sockets.iter_mut().zip(["peer_a", "peer_b", "dropper"]) {
            let plan = expect_finalize_plan(socket, who).await;
            assert_eq!(plan.topology, Topology::Mesh);
        }
        [peer_a, peer_b, dropper] = sockets;
    }

    // Snapshot the member's room info before its socket drops (the reconnect
    // registration needs it, mirroring `v3_signaling_e2e.rs`).
    let room = server
        .database()
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup")
        .expect("room exists");
    let dropper_info = room
        .players
        .get(&dropper_id)
        .cloned()
        .expect("dropper in room before disconnect");

    // The member's socket drops abruptly; the remaining 2 observe PlayerLeft.
    drop(dropper);
    expect_player_left(&mut peer_a, dropper_id, "peer_a").await;
    expect_player_left(&mut peer_b, dropper_id, "peer_b").await;

    // Mint a reconnection token through the server handle. `expect_player_left`
    // above means the socket-drop path already auto-registered the disconnect
    // with the dropper's pre-issued (join-time) token; this same-room
    // re-registration PRESERVES that token (see
    // `ReconnectionManager::register_disconnection`) rather than replacing it,
    // so `token` is exactly what the dropper received in `RoomJoined` and would
    // reconnect with — faithful to a real client. (The client-held-token
    // invariant itself is locked at the unit level:
    // `same_room_reregistration_keeps_pre_issued_token_claimable`.) This is the
    // sanctioned in-process pattern from `v3_signaling_e2e.rs`.
    let token = server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(dropper_id, room_id, false, Some(dropper_info), 0)
        .await;

    // Reconnect over a FRESH socket using the real wire flow.
    let mut reconnector = connect(addr).await;
    authenticate_v3_full(&mut reconnector).await;
    send(
        &mut reconnector,
        &ClientMessage::Reconnect {
            player_id: dropper_id,
            room_id,
            auth_token: token,
        },
    )
    .await;

    // The reconnector receives Reconnected (restored id, still-finalized room)
    // then its fresh tailored SessionPlan.
    next_matching_server_message_within(
        &mut reconnector,
        SERVER_MESSAGE_TIMEOUT,
        "Reconnected",
        |message| match message {
            ServerMessage::Reconnected(payload) => {
                assert_eq!(payload.player_id, dropper_id, "restored player id");
                assert_eq!(payload.room_id, room_id);
                assert_eq!(
                    payload.lobby_state,
                    LobbyState::Finalized,
                    "the running session stays finalized across the reconnect"
                );
                Some(())
            }
            ServerMessage::ReconnectionFailed { reason, error_code } => {
                panic!("reconnect failed: {reason} ({error_code:?})")
            }
            other => panic!("reconnector expected Reconnected, got {other:?}"),
        },
    )
    .await;
    let reconnect_plan = expect_session_plan_strict(&mut reconnector, "reconnector").await;
    assert_eq!(reconnect_plan.topology, Topology::Mesh);
    assert_eq!(reconnect_plan.transport, Transport::WebRtc);
    let reconnect_peer_ids: BTreeSet<PlayerId> = reconnect_plan
        .peers
        .iter()
        .map(|peer| peer.player_id)
        .collect();
    assert_eq!(
        reconnect_peer_ids,
        BTreeSet::from([id_a, id_b]),
        "the fresh plan lists exactly the 2 remaining members"
    );
    assert_eq!(reconnect_plan.peers.len(), 2);
    for peer in &reconnect_plan.peers {
        assert_eq!(
            peer.initiate,
            dropper_id < peer.player_id,
            "the reconnector's initiate flag toward {} follows the UUID glare rule",
            peer.player_id
        );
    }
    assert_static_then_default_stun_ice(&reconnect_plan.ice_servers);

    // Each incumbent receives PlayerReconnected followed by a complete plan
    // refresh. Validate all three views as one mesh matrix.
    let mut incumbent_plans = Vec::new();
    for (socket, member_id, who) in [(&mut peer_a, id_a, "peer_a"), (&mut peer_b, id_b, "peer_b")] {
        next_matching_server_message_within(
            socket,
            SERVER_MESSAGE_TIMEOUT,
            "PlayerReconnected",
            |message| match message {
                ServerMessage::PlayerReconnected { player_id, .. } => {
                    assert_eq!(player_id, dropper_id, "{who} saw the wrong reconnector");
                    Some(())
                }
                other => panic!("{who} expected PlayerReconnected, got {other:?}"),
            },
        )
        .await;
        let plan = expect_session_plan_strict(socket, who).await;
        incumbent_plans.push((member_id, plan));
    }
    let refreshed = [
        (dropper_id, &*reconnect_plan),
        (incumbent_plans[0].0, &*incumbent_plans[0].1),
        (incumbent_plans[1].0, &*incumbent_plans[1].1),
    ];
    assert_full_mesh_glare_matrix(&refreshed);

    // Exactly one authoritative plan each.
    expect_no_server_message_within(
        &mut reconnector,
        SILENCE_WINDOW,
        "reconnector after its plan",
    )
    .await;
    expect_no_server_message_within(&mut peer_a, SILENCE_WINDOW, "peer_a after its refresh").await;
    expect_no_server_message_within(&mut peer_b, SILENCE_WINDOW, "peer_b after its refresh").await;

    // Post-reconnect signaling runs under the RESTORED player id, both ways.
    let offer = json!({ "Offer": "v=0\r\no=- post-reconnect\r\n" });
    send(
        &mut reconnector,
        &ClientMessage::Signal {
            to: id_a,
            generation: reconnect_plan.generation,
            signal: offer.clone(),
        },
    )
    .await;
    let (from, signal) = expect_signal(&mut peer_a, "peer_a").await;
    assert_eq!(
        from, dropper_id,
        "post-reconnect signal must be routed under the restored player id"
    );
    assert_eq!(signal, offer);

    let answer = json!({ "Answer": "v=0\r\no=- post-reconnect-answer\r\n" });
    send(
        &mut peer_a,
        &ClientMessage::Signal {
            to: dropper_id,
            generation: reconnect_plan.generation,
            signal: answer.clone(),
        },
    )
    .await;
    let (from, signal) = expect_signal(&mut reconnector, "reconnector").await;
    assert_eq!(from, id_a);
    assert_eq!(signal, answer);
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// PeerTransportStatus fan-out (P5 refinement) over real WebSockets.
// ---------------------------------------------------------------------------

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

/// Await a `PeerTransportStatus` naming exactly `(peer, transport, connected)`,
/// skipping unrelated backlog (PlayerJoined / LobbyStateChanged from the join
/// phase).
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

/// Assert no `PeerTransportStatus` arrives while draining known join-phase
/// backlog. The duplicate-status contract is about no fan-out, not global
/// silence from previously queued `PlayerJoined` / `LobbyStateChanged` events.
async fn expect_no_peer_transport_status(ws: &mut WsStream, context: &str) {
    assert!(
        maybe_next_matching_server_message_within(
            ws,
            SILENCE_WINDOW,
            context,
            |message| match message {
                ServerMessage::PeerTransportStatus { .. } => Some(()),
                ServerMessage::PlayerJoined { .. } | ServerMessage::LobbyStateChanged { .. } => {
                    None
                }
                other => panic!(
                    "{context}: unexpected ServerMessage while checking no PeerTransportStatus: {other:?}"
                ),
            },
        )
        .await
        .is_none(),
        "{context}: duplicate TransportStatus must not fan out PeerTransportStatus"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mesh_n3_transport_status_fan_out_and_dedup() {
    // One v3 member of an N=3 room reports TransportStatus { webrtc, true }:
    // both OTHER v3 members receive PeerTransportStatus naming the reporter,
    // the reporter itself never hears an echo, a byte-identical duplicate fans
    // out nothing, and the next real transition (connected: false) fans out
    // again. No finalization needed — the fan-out is a room-membership
    // contract, not a session-plan one.
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "multipeer-transport-status3";

    let mut peer_a = connect(addr).await;
    authenticate_v3_full(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, game, None, "PeerA", 3).await;
    let id_a = joined_a.player_id;
    let room_code = joined_a.room_code;

    let mut peer_b = connect(addr).await;
    authenticate_v3_full(&mut peer_b).await;
    join_room(&mut peer_b, game, Some(room_code.clone()), "PeerB", 3).await;

    let mut peer_c = connect(addr).await;
    authenticate_v3_full(&mut peer_c).await;
    join_room(&mut peer_c, game, Some(room_code), "PeerC", 3).await;

    // First report IS a state change: both other members hear it.
    report_transport_status(&mut peer_a, Transport::WebRtc, true).await;
    expect_peer_transport_status(&mut peer_b, "peer_b", id_a, Transport::WebRtc, true).await;
    expect_peer_transport_status(&mut peer_c, "peer_c", id_a, Transport::WebRtc, true).await;

    // The reporter is excluded from its own fan-out.
    expect_no_peer_transport_status(&mut peer_a, "reporter echo check").await;

    // A byte-identical duplicate is dropped at the dedup gate: no
    // PeerTransportStatus fans out to any member.
    report_transport_status(&mut peer_a, Transport::WebRtc, true).await;
    expect_no_peer_transport_status(&mut peer_b, "peer_b after duplicate").await;
    expect_no_peer_transport_status(&mut peer_c, "peer_c after duplicate").await;
    expect_no_peer_transport_status(&mut peer_a, "reporter after duplicate").await;

    // The next REAL transition fans out again (the dedup gate is per-state,
    // not once-per-connection).
    report_transport_status(&mut peer_a, Transport::WebRtc, false).await;
    expect_peer_transport_status(&mut peer_b, "peer_b", id_a, Transport::WebRtc, false).await;
    expect_peer_transport_status(&mut peer_c, "peer_c", id_a, Transport::WebRtc, false).await;
    running_server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mixed_v2_v3_n3_transport_status_v2_member_hears_nothing() {
    // In a mixed v2/v3 room a v3 member's TransportStatus change reaches the
    // OTHER v3 member as PeerTransportStatus, while the v2 member observes
    // strict v3-message silence and stays fully functional afterwards.
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "multipeer-tstatus-mixed3";
    let legacy_who = "legacy (v2)";

    let mut peer_a = connect(addr).await;
    authenticate_v3_full(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, game, None, "PeerA", 3).await;
    let id_a = joined_a.player_id;
    let room_code = joined_a.room_code;

    let mut peer_b = connect(addr).await;
    authenticate_v3_full(&mut peer_b).await;
    join_room(&mut peer_b, game, Some(room_code.clone()), "PeerB", 3).await;

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
    next_matching_v2_only(
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
    // The room entered Lobby EXACTLY ONCE, on the creator (PeerA) joining; the
    // legacy member is a subsequent joiner, so its RoomJoined already reports the
    // Lobby state and NO LobbyStateChanged is broadcast on its join — there is
    // nothing further to drain for the v2 member before the silence window below.

    report_transport_status(&mut peer_a, Transport::WebRtc, true).await;

    // The v3 peer hears the change…
    expect_peer_transport_status(&mut peer_b, "peer_b", id_a, Transport::WebRtc, true).await;
    // …the v2 member hears NOTHING (strict window, every frame checked).
    expect_no_server_message_within(&mut legacy, SILENCE_WINDOW, "v2 member after fan-out").await;

    // And the v2 member is still fully functional: Ping round-trips (any
    // late-leaking v3 frame would be caught by the v2-only guard first).
    send(&mut legacy, &ClientMessage::Ping).await;
    next_matching_v2_only(&mut legacy, "v2 Pong", legacy_who, |message| {
        matches!(message, ServerMessage::Pong).then_some(())
    })
    .await;
    running_server.shutdown().await;
}
