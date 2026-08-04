//! End-to-end protocol v3 mid-session re-planning tests through the real
//! WebSocket stack.
//!
//! Covers the two halves of the stored-`ActiveSessionPlan` feature:
//!
//! 1. **Host failover.** A 3-player `host`-topology room finalizes (everyone
//!    receives a `SessionPlan` naming the host); the host's socket then closes.
//!    Each remaining client must receive `PlayerLeft` AND a fresh `SessionPlan`
//!    naming the re-elected host with correct `initiate` flags — same (sticky)
//!    topology/transport.
//! 2. **Sticky relay floor.** A mixed v3+v2 room finalizes to the relay floor
//!    (no plan is stored); the v2 member leaves and a v3+webrtc member fills
//!    the seat. Every v3 member receives an explicit relay reset without
//!    rerunning the upgrade ladder; v2 remains plan-free.
//!
//! Plus the late-join contract for an active mesh session: the joiner gets a
//! tailored `SessionPlan`, and the existing member gets a complete refreshed
//! plan after `PlayerJoined`.
//!
//! Models the harness in `tests/v3_session_plan_e2e.rs` (chosen `SessionConfig`,
//! real `TcpListener` on 127.0.0.1:0).

mod test_helpers;
mod websocket_test_helpers;

use futures_util::SinkExt;
use signal_fish_server::config::{AppAuthEntry, SessionConfig, TurnConfig};
use signal_fish_server::protocol::{
    ClientMessage, IceServer, LobbyState, PlayerId, RoomJoinedPayload, ServerMessage, Topology,
    Transport,
};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::{create_router, websocket_handler_v3};
use test_helpers::{test_protocol_config, test_server_config, RunningTestServer};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::{
    expect_no_server_message_within, next_matching_server_message_within, WsStream,
};

const APP_ID: &str = "v3-replan-app";
const SERVER_MESSAGE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(20);
const STATIC_STUN_URL: &str = "stun:static.example.com:3478";
const TURN_STUN_URL: &str = "stun:stun.l.google.com:19302";
/// Window in which a forbidden message would have arrived if it were going to.
const SILENCE_WINDOW: tokio::time::Duration = tokio::time::Duration::from_secs(2);

fn app_entry() -> AppAuthEntry {
    AppAuthEntry {
        app_id: APP_ID.to_string(),
        app_secret: "secret".to_string(),
        app_name: "V3 Replan App".to_string(),
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
        "replan e2e fixtures intentionally exercise the default [turn] STUN URL"
    );
}

fn host_session_config() -> SessionConfig {
    SessionConfig {
        default_topology: Topology::Host,
        ice_servers: vec![IceServer {
            urls: vec![STATIC_STUN_URL.to_string()],
            username: None,
            credential: None,
        }],
        ..SessionConfig::default()
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

async fn start_server_with_session(session: SessionConfig) -> RunningTestServer {
    use axum::routing::get;

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
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        vec![app_entry()],
    )
    .await
    .expect("server builds");

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

    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "Authenticated", |message| {
        matches!(message, ServerMessage::Authenticated { .. }).then_some(())
    })
    .await;
    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "ProtocolInfo", |message| {
        matches!(message, ServerMessage::ProtocolInfo(_)).then_some(())
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
    )
    .await;
}

/// Authenticate as a pure v2 client on the v3 endpoint (relay-only).
async fn authenticate_v2(ws: &mut WsStream) {
    authenticate(ws, Some(2), None, None).await;
}

/// Join (or create) a room, returning the full `RoomJoined` payload so tests
/// can assert any field (e.g. a seat-filling joiner of an active session must
/// see `lobby_state: "finalized"`).
async fn join_room(
    ws: &mut WsStream,
    room_code: Option<String>,
    player_name: &str,
    max_players: u8,
) -> Box<RoomJoinedPayload> {
    send(
        ws,
        &ClientMessage::JoinRoom {
            game_name: "replan-game".to_string(),
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
            ServerMessage::RoomJoined(p) => Some(p),
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

fn assert_relay_floor_plan(plan: &signal_fish_server::protocol::SessionPlanPayload, who: &str) {
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

/// Skip to `GameStarting`, then return the `SessionPlan` that must follow.
async fn expect_finalize_plan(ws: &mut WsStream, who: &str) -> SessionPlan {
    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "finalization GameStarting",
        |message| matches!(message, ServerMessage::GameStarting { .. }).then_some(()),
    )
    .await;
    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "post-GameStarting SessionPlan",
        |message| match message {
            ServerMessage::SessionPlan(plan) => Some(plan),
            other => panic!("{who} expected SessionPlan after GameStarting, got {other:?}"),
        },
    )
    .await
}

/// Expect the exact next message to be `PlayerLeft(left)`.
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

#[tokio::test(flavor = "multi_thread")]
async fn host_disconnect_reelects_host_and_reissues_session_plans() {
    let running_server = start_server_with_session(host_session_config()).await;
    let addr = running_server.addr();

    // Three v3 host-capable players fill a 3-seat room. The creator (earliest
    // joiner; the room has no authority) is the finalize-time host.
    let mut host = connect(addr).await;
    authenticate_v3_full(&mut host).await;
    let host_joined = join_room(&mut host, None, "Host", 3).await;
    let (room_code, host_id) = (host_joined.room_code, host_joined.player_id);

    let mut peer_a = connect(addr).await;
    authenticate_v3_full(&mut peer_a).await;
    let peer_a_id = join_room(&mut peer_a, Some(room_code.clone()), "PeerA", 3)
        .await
        .player_id;

    let mut peer_b = connect(addr).await;
    authenticate_v3_full(&mut peer_b).await;
    let peer_b_id = join_room(&mut peer_b, Some(room_code), "PeerB", 3)
        .await
        .player_id;

    // Paced readies: each ready is observed by everyone before the next fires.
    ready(&mut host).await;
    for ws in [&mut host, &mut peer_a, &mut peer_b] {
        await_ready_count(ws, 1).await;
    }
    ready(&mut peer_a).await;
    for ws in [&mut host, &mut peer_a, &mut peer_b] {
        await_ready_count(ws, 2).await;
    }
    ready(&mut peer_b).await;
    for ws in [&mut host, &mut peer_a, &mut peer_b] {
        await_ready_count(ws, 3).await;
    }

    // Readiness no longer auto-starts: the creator (an ordinary member here,
    // supports_authority: false) sends an explicit StartGame to finalize.
    send(&mut host, &ClientMessage::StartGame).await;

    // Finalize: everyone gets GameStarting then a host-topology SessionPlan
    // naming the creator as host.
    let host_plan = expect_finalize_plan(&mut host, "host").await;
    let plan_a = expect_finalize_plan(&mut peer_a, "peer_a").await;
    let plan_b = expect_finalize_plan(&mut peer_b, "peer_b").await;
    assert_eq!(host_plan.generation, plan_a.generation);
    assert_eq!(host_plan.generation, plan_b.generation);
    for plan in [&host_plan, &plan_a, &plan_b] {
        assert_eq!(plan.topology, Topology::Host);
        assert_eq!(plan.transport, Transport::WebRtc);
        assert_eq!(plan.host, Some(host_id));
    }
    assert_eq!(host_plan.peers.len(), 2, "the host answers both clients");

    // The host's socket closes (disconnect path: unregister -> leave_room ->
    // PlayerLeft broadcast -> host-failover re-plan).
    host.close(None).await.expect("close host socket");

    // Each remaining client sees PlayerLeft(host) FOLLOWED BY a fresh
    // SessionPlan naming the re-elected host. peer_a joined before peer_b, so
    // it is the earliest remaining joiner and wins the re-election.
    expect_player_left(&mut peer_a, host_id, "peer_a").await;
    expect_player_left(&mut peer_b, host_id, "peer_b").await;

    let replan_a = next_matching_server_message_within(
        &mut peer_a,
        SERVER_MESSAGE_TIMEOUT,
        "peer_a failover SessionPlan",
        |message| match message {
            ServerMessage::SessionPlan(plan) => Some(plan),
            other => panic!("peer_a expected the failover SessionPlan, got {other:?}"),
        },
    )
    .await;
    let replan_b = next_matching_server_message_within(
        &mut peer_b,
        SERVER_MESSAGE_TIMEOUT,
        "peer_b failover SessionPlan",
        |message| match message {
            ServerMessage::SessionPlan(plan) => Some(plan),
            other => panic!("peer_b expected the failover SessionPlan, got {other:?}"),
        },
    )
    .await;

    assert_eq!(
        replan_a.generation, replan_b.generation,
        "one room publication must share one signaling generation"
    );
    assert_ne!(
        replan_a.generation, host_plan.generation,
        "a failover plan must fence the superseded physical generation"
    );

    for plan in [&replan_a, &replan_b] {
        assert_eq!(plan.topology, Topology::Host, "topology is sticky");
        assert_eq!(plan.transport, Transport::WebRtc, "transport is sticky");
        assert_eq!(
            plan.host,
            Some(peer_a_id),
            "both fresh plans name the re-elected host (earliest remaining joiner)"
        );
        assert_eq!(plan.fallback, Transport::Relay);
        assert_static_then_default_stun_ice(&plan.ice_servers);
    }
    // New host answers its one remaining client; the client offers to it.
    assert_eq!(replan_a.peers.len(), 1);
    assert_eq!(replan_a.peers[0].player_id, peer_b_id);
    assert!(
        !replan_a.peers[0].initiate,
        "the new host initiates to none"
    );
    assert_eq!(replan_b.peers.len(), 1);
    assert_eq!(replan_b.peers[0].player_id, peer_a_id);
    assert!(
        replan_b.peers[0].initiate,
        "the client offers to the new host"
    );

    // Exactly one re-plan each: no further plan/pairing traffic.
    expect_no_server_message_within(&mut peer_a, SILENCE_WINDOW, "peer_a after failover").await;
    expect_no_server_message_within(&mut peer_b, SILENCE_WINDOW, "peer_b after failover").await;
    running_server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_room_departure_then_v3_late_join_refreshes_explicit_floor_plan() {
    // Problem-2 regression over real sockets: A(v3)+B(v2) finalize on a
    // mesh-preferring server => relay floor. B leaves; C(v3+webrtc) fills the
    // seat in the still-Finalized room. The running session stays relay, and
    // each v3 membership boundary carries an explicit floor plan.
    let running_server = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();

    let mut peer_a = connect(addr).await;
    authenticate_v3_full(&mut peer_a).await;
    let room_code = join_room(&mut peer_a, None, "PeerA", 2).await.room_code;

    let mut legacy = connect(addr).await;
    authenticate_v2(&mut legacy).await;
    let legacy_id = join_room(&mut legacy, Some(room_code.clone()), "Legacy", 2)
        .await
        .player_id;

    ready(&mut peer_a).await;
    await_ready_count(&mut peer_a, 1).await;
    await_ready_count(&mut legacy, 1).await;
    ready(&mut legacy).await;
    await_ready_count(&mut peer_a, 2).await;
    await_ready_count(&mut legacy, 2).await;

    // Readiness no longer auto-starts: the v3 creator sends an explicit
    // StartGame to finalize the relay-floored room.
    send(&mut peer_a, &ClientMessage::StartGame).await;

    // Both observe GameStarting; only the v3 member gets the relay reset.
    for ws in [&mut peer_a, &mut legacy] {
        next_matching_server_message_within(
            ws,
            SERVER_MESSAGE_TIMEOUT,
            "finalization GameStarting",
            |message| matches!(message, ServerMessage::GameStarting { .. }).then_some(()),
        )
        .await;
    }
    let initial_floor = next_matching_server_message_within(
        &mut peer_a,
        SERVER_MESSAGE_TIMEOUT,
        "initial relay SessionPlan",
        |message| match message {
            ServerMessage::SessionPlan(plan) => Some(plan),
            other => panic!("peer_a expected relay SessionPlan, got {other:?}"),
        },
    )
    .await;
    assert_relay_floor_plan(&initial_floor, "peer_a initial floor");

    // The v2 member's socket closes, reopening a seat in the Finalized room.
    legacy.close(None).await.expect("close legacy socket");
    expect_player_left(&mut peer_a, legacy_id, "peer_a").await;

    // A v3+webrtc joiner fills the seat. Its RoomJoined reports the running
    // session's persisted lobby state — never a spurious re-lobby cycle.
    let mut joiner = connect(addr).await;
    authenticate_v3_full(&mut joiner).await;
    let joiner_joined = join_room(&mut joiner, Some(room_code), "Joiner", 2).await;
    let joiner_id = joiner_joined.player_id;
    assert_eq!(
        joiner_joined.lobby_state,
        LobbyState::Finalized,
        "a seat-filling joiner of a finalized room must see lobby_state: finalized"
    );

    // peer_a sees the join, then both v3 members receive the authoritative floor.
    next_matching_server_message_within(
        &mut peer_a,
        SERVER_MESSAGE_TIMEOUT,
        "peer_a PlayerJoined",
        |message| match message {
            ServerMessage::PlayerJoined { player } => {
                assert_eq!(player.id, joiner_id);
                Some(())
            }
            other => panic!("peer_a expected PlayerJoined, got {other:?}"),
        },
    )
    .await;
    let incumbent_floor = next_matching_server_message_within(
        &mut peer_a,
        SERVER_MESSAGE_TIMEOUT,
        "incumbent relay refresh",
        |message| match message {
            ServerMessage::SessionPlan(plan) => Some(plan),
            other => panic!("peer_a expected relay refresh, got {other:?}"),
        },
    )
    .await;
    let joiner_floor = next_matching_server_message_within(
        &mut joiner,
        SERVER_MESSAGE_TIMEOUT,
        "joiner relay plan",
        |message| match message {
            ServerMessage::SessionPlan(plan) => Some(plan),
            other => panic!("joiner expected relay plan, got {other:?}"),
        },
    )
    .await;
    assert_relay_floor_plan(&incumbent_floor, "peer_a refresh");
    assert_relay_floor_plan(&joiner_floor, "joiner floor");
    expect_no_server_message_within(&mut peer_a, SILENCE_WINDOW, "peer_a after refresh").await;
    expect_no_server_message_within(&mut joiner, SILENCE_WINDOW, "joiner after plan").await;
    running_server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mesh_late_join_refreshes_joiner_and_existing_member_plans() {
    // Late-join into an ACTIVE mesh session over real sockets: after a member
    // departs the finalized 2-seat mesh room, the seat-filling v3 joiner
    // receives a tailored SessionPlan; the existing member receives a complete
    // refreshed plan after PlayerJoined. A mesh member
    // departure itself triggers no re-plan (sticky parameters unchanged).
    let running_server = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();

    let mut peer_a = connect(addr).await;
    authenticate_v3_full(&mut peer_a).await;
    let peer_a_joined = join_room(&mut peer_a, None, "PeerA", 2).await;
    let (room_code, peer_a_id) = (peer_a_joined.room_code, peer_a_joined.player_id);

    let mut peer_b = connect(addr).await;
    authenticate_v3_full(&mut peer_b).await;
    let peer_b_id = join_room(&mut peer_b, Some(room_code.clone()), "PeerB", 2)
        .await
        .player_id;

    ready(&mut peer_a).await;
    await_ready_count(&mut peer_a, 1).await;
    await_ready_count(&mut peer_b, 1).await;
    ready(&mut peer_b).await;
    await_ready_count(&mut peer_a, 2).await;
    await_ready_count(&mut peer_b, 2).await;

    // Readiness no longer auto-starts: an explicit StartGame finalizes.
    send(&mut peer_a, &ClientMessage::StartGame).await;

    let plan_a = expect_finalize_plan(&mut peer_a, "peer_a").await;
    assert_eq!(plan_a.topology, Topology::Mesh);
    let plan_b = expect_finalize_plan(&mut peer_b, "peer_b").await;
    assert_eq!(plan_a.generation, plan_b.generation);

    // peer_b departs the live session; a mesh member departure re-plans nothing.
    peer_b.close(None).await.expect("close peer_b socket");
    expect_player_left(&mut peer_a, peer_b_id, "peer_a").await;

    let mut joiner = connect(addr).await;
    authenticate_v3_full(&mut joiner).await;
    let joiner_joined = join_room(&mut joiner, Some(room_code), "Joiner", 2).await;
    let joiner_id = joiner_joined.player_id;
    // The seat-filling joiner's RoomJoined reports the running session's
    // persisted lobby state (finalization is durable; no re-lobby cycle).
    assert_eq!(
        joiner_joined.lobby_state,
        LobbyState::Finalized,
        "a seat-filling joiner of a finalized room must see lobby_state: finalized"
    );

    // The joiner's very next message is its tailored SessionPlan for the
    // running mesh session (never a NewPeer).
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
    assert_eq!(joiner_plan.peers.len(), 1);
    assert_eq!(joiner_plan.peers[0].player_id, peer_a_id);
    assert_eq!(
        joiner_plan.peers[0].initiate,
        joiner_id < peer_a_id,
        "the joiner's initiate flag follows the UUID glare rule"
    );
    assert_static_then_default_stun_ice(&joiner_plan.ice_servers);

    // The existing member sees PlayerJoined then its complete refreshed plan.
    next_matching_server_message_within(
        &mut peer_a,
        SERVER_MESSAGE_TIMEOUT,
        "peer_a PlayerJoined",
        |message| match message {
            ServerMessage::PlayerJoined { player } => {
                assert_eq!(player.id, joiner_id);
                Some(())
            }
            other => panic!("peer_a expected PlayerJoined, got {other:?}"),
        },
    )
    .await;
    let incumbent_plan = next_matching_server_message_within(
        &mut peer_a,
        SERVER_MESSAGE_TIMEOUT,
        "peer_a SessionPlan refresh",
        |message| match message {
            ServerMessage::SessionPlan(plan) => Some(plan),
            other => panic!("peer_a expected SessionPlan refresh, got {other:?}"),
        },
    )
    .await;
    assert_eq!(
        incumbent_plan.generation, joiner_plan.generation,
        "joiner and incumbent plans must share one generation"
    );
    assert_ne!(
        incumbent_plan.generation, plan_a.generation,
        "late membership refresh must fence the retained physical pair"
    );
    assert_eq!(incumbent_plan.topology, Topology::Mesh);
    assert_eq!(incumbent_plan.transport, Transport::WebRtc);
    assert_eq!(incumbent_plan.peers.len(), 1);
    assert_eq!(incumbent_plan.peers[0].player_id, joiner_id);
    assert_eq!(incumbent_plan.peers[0].initiate, peer_a_id < joiner_id);
    assert_ne!(
        incumbent_plan.peers[0].initiate, joiner_plan.peers[0].initiate,
        "exactly one side of the mesh pair initiates"
    );

    expect_no_server_message_within(&mut joiner, SILENCE_WINDOW, "joiner after its plan").await;
    expect_no_server_message_within(&mut peer_a, SILENCE_WINDOW, "peer_a after its refresh").await;
    running_server.shutdown().await;
}
