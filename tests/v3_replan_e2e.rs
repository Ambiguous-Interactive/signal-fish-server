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
//!    the seat. NOBODY may receive a `SessionPlan` or `NewPeer` — the running
//!    session is relay, and the old recompute-on-late-join behavior (which
//!    wrongly emitted `NewPeer` here) must stay dead.
//!
//! Plus the late-join contract for an active mesh session: the joiner gets a
//! tailored `SessionPlan` (and NO `NewPeer`), the existing member gets the
//! `NewPeer` delta.
//!
//! Models the harness in `tests/v3_session_plan_e2e.rs` (chosen `SessionConfig`,
//! real `TcpListener` on 127.0.0.1:0).

mod test_helpers;
mod websocket_test_helpers;

use futures_util::SinkExt;
use signal_fish_server::config::{AppAuthEntry, SessionConfig};
use signal_fish_server::protocol::{
    ClientMessage, IceServer, LobbyState, PlayerId, RoomJoinedPayload, ServerMessage, Topology,
    Transport,
};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::{create_router, websocket_handler_v3};
use test_helpers::{test_protocol_config, test_server_config};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::{
    expect_no_server_message_within, next_matching_server_message_within, WsStream,
};

const APP_ID: &str = "v3-replan-app";
const SERVER_MESSAGE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(20);
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

fn host_session_config() -> SessionConfig {
    SessionConfig {
        default_topology: Topology::Host,
        ice_servers: vec![IceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_string()],
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
            urls: vec!["stun:stun.l.google.com:19302".to_string()],
            username: None,
            credential: None,
        }],
        ..SessionConfig::default()
    }
}

async fn start_server_with_session(session: SessionConfig) -> std::net::SocketAddr {
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
        signal_fish_server::config::AuthMaintenanceConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        vec![app_entry()],
    )
    .await
    .expect("server builds");

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
            ServerMessage::PlayerLeft { player_id } => {
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
    let addr = start_server_with_session(host_session_config()).await;

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

    // Finalize: everyone gets GameStarting then a host-topology SessionPlan
    // naming the creator as host.
    let host_plan = expect_finalize_plan(&mut host, "host").await;
    let plan_a = expect_finalize_plan(&mut peer_a, "peer_a").await;
    let plan_b = expect_finalize_plan(&mut peer_b, "peer_b").await;
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

    for plan in [&replan_a, &replan_b] {
        assert_eq!(plan.topology, Topology::Host, "topology is sticky");
        assert_eq!(plan.transport, Transport::WebRtc, "transport is sticky");
        assert_eq!(
            plan.host,
            Some(peer_a_id),
            "both fresh plans name the re-elected host (earliest remaining joiner)"
        );
        assert_eq!(plan.fallback, Transport::Relay);
        assert!(
            !plan.ice_servers.is_empty(),
            "a webrtc re-plan carries (fresh) ICE"
        );
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
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_room_departure_then_v3_late_join_emits_no_plan_or_new_peer() {
    // Problem-2 regression over real sockets: A(v3)+B(v2) finalize on a
    // mesh-preferring server => relay floor (no plans). B leaves; C(v3+webrtc)
    // fills the seat in the still-Finalized room. The running session is relay
    // and sticky: NOBODY receives a SessionPlan or NewPeer.
    let addr = start_server_with_session(mesh_session_config()).await;

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

    // Both observe GameStarting; the relay floor emits no SessionPlan, so the
    // next interesting event for peer_a is the departure below.
    for (ws, who) in [(&mut peer_a, "peer_a"), (&mut legacy, "legacy")] {
        next_matching_server_message_within(
            ws,
            SERVER_MESSAGE_TIMEOUT,
            "finalization GameStarting",
            |message| match message {
                ServerMessage::GameStarting { .. } => Some(()),
                ServerMessage::SessionPlan(_) => {
                    panic!("{who} must not receive a SessionPlan in a relay-resolved room")
                }
                _ => None,
            },
        )
        .await;
    }

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

    // peer_a sees the join, then NOTHING v3: no NewPeer, no SessionPlan.
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
    expect_no_server_message_within(
        &mut peer_a,
        SILENCE_WINDOW,
        "peer_a after a v3 joiner entered the relay-floor room",
    )
    .await;
    // The joiner likewise gets nothing after RoomJoined.
    expect_no_server_message_within(
        &mut joiner,
        SILENCE_WINDOW,
        "v3 joiner of a relay-floor room",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mesh_late_join_sends_joiner_plan_and_existing_member_new_peer() {
    // Late-join into an ACTIVE mesh session over real sockets: after a member
    // departs the finalized 2-seat mesh room, the seat-filling v3 joiner
    // receives a tailored SessionPlan (and NO NewPeer); the existing member
    // receives only the NewPeer delta (and no second plan). A mesh member
    // departure itself triggers no re-plan (sticky parameters unchanged).
    let addr = start_server_with_session(mesh_session_config()).await;

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

    let plan_a = expect_finalize_plan(&mut peer_a, "peer_a").await;
    assert_eq!(plan_a.topology, Topology::Mesh);
    let _plan_b = expect_finalize_plan(&mut peer_b, "peer_b").await;

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
    assert!(!joiner_plan.ice_servers.is_empty());

    // The existing member sees PlayerJoined then the antisymmetric NewPeer
    // delta — and no second SessionPlan.
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
    next_matching_server_message_within(
        &mut peer_a,
        SERVER_MESSAGE_TIMEOUT,
        "peer_a NewPeer delta",
        |message| match message {
            ServerMessage::NewPeer {
                peer_id,
                you_initiate,
            } => {
                assert_eq!(peer_id, joiner_id);
                assert_eq!(
                    you_initiate,
                    peer_a_id < joiner_id,
                    "exactly one side of the pair initiates"
                );
                Some(())
            }
            ServerMessage::SessionPlan(_) => {
                panic!("the existing member must not receive a plan on a late join")
            }
            other => panic!("peer_a expected NewPeer, got {other:?}"),
        },
    )
    .await;

    expect_no_server_message_within(&mut joiner, SILENCE_WINDOW, "joiner after its plan").await;
    expect_no_server_message_within(&mut peer_a, SILENCE_WINDOW, "peer_a after the NewPeer").await;
}
