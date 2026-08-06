//! Handler tests for the P2 targeted signal relay (`signaling.rs`).
//!
//! Mirrors the `message_router_tests` harness: register clients, set their
//! negotiated protocol, drive targeted signaling and phased membership
//! publication, and assert on what each client receives. Covers relay security,
//! glare determinism, reconnect plans, transport-status fan-out, and v2 gating.

use crate::config::{
    CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig, SessionConfig,
    TransportSecurityConfig, TurnConfig, WebSocketConfig,
};
use crate::database::{DatabaseConfig, GameDatabase, InMemoryDatabase};
use crate::protocol::{
    ClientMessage, ErrorCode, IceServer, LobbyState, PlayerId, PlayerInfo, ServerMessage, Topology,
    Transport,
};
use crate::rate_limit::RateLimitConfig;
use crate::server::{EnhancedGameServer, NegotiatedProtocol, ServerConfig};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

use super::session_policy::ActiveSessionPlan;
use super::signaling::local_initiates;

const STATIC_STUN_URL: &str = "stun:static.example.com:3478";
const TURN_STUN_URL: &str = "stun:stun.l.google.com:19302";
const TURN_URL: &str = "turn:turn.example.com:3478";
const TURN_CREDENTIAL_TTL_SECS: u64 = 3600;

/// Allocate a unique loopback address per registered client so tests never
/// collide on the same `SocketAddr`.
static PORT: AtomicU16 = AtomicU16::new(52000);

fn next_addr() -> SocketAddr {
    let port = PORT.fetch_add(1, Ordering::Relaxed);
    format!("127.0.0.1:{port}").parse().expect("valid addr")
}

async fn create_test_server() -> Arc<EnhancedGameServer> {
    create_test_server_with_signals(600).await
}

async fn create_test_server_with_config(config: ServerConfig) -> Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
        config,
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        SessionConfig::default(),
        TurnConfig::default(),
        DatabaseConfig::InMemory,
        MetricsConfig::default(),
        CoordinationConfig::default(),
        TransportSecurityConfig::default(),
        Vec::new(),
    )
    .await
    .expect("failed to construct test server")
}

async fn create_test_server_with_signals(max_signals: u32) -> Arc<EnhancedGameServer> {
    create_test_server_with_signal_limits(max_signals, 60).await
}

async fn create_test_server_with_signal_limits(
    max_signals: u32,
    max_signal_errors: u32,
) -> Arc<EnhancedGameServer> {
    create_test_server_with_signal_policy(max_signals, max_signal_errors, 16384).await
}

/// Build a server with full control over the per-connection signal budget AND
/// the serialized-payload size cap (`security.max_signal_bytes`).
async fn create_test_server_with_signal_policy(
    max_signals: u32,
    max_signal_errors: u32,
    max_signal_bytes: usize,
) -> Arc<EnhancedGameServer> {
    let config = ServerConfig {
        max_signal_bytes,
        rate_limit_config: RateLimitConfig {
            max_signals,
            max_signal_errors,
            ..RateLimitConfig::default()
        },
        ..ServerConfig::default()
    };
    create_test_server_with_config(config).await
}

/// Build a server whose session policy is the given `SessionConfig`, so the
/// finalization-gated late-join path can resolve to a non-relay topology.
async fn create_test_server_with_session(session: SessionConfig) -> Arc<EnhancedGameServer> {
    create_test_server_with_session_and_turn(session, TurnConfig::default()).await
}

/// Build a server with the given session **and** TURN config, so the late-join
/// ICE-minting path can be exercised with active TURN credentials.
async fn create_test_server_with_session_and_turn(
    session: SessionConfig,
    turn: TurnConfig,
) -> Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
        ServerConfig::default(),
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        session,
        turn,
        DatabaseConfig::InMemory,
        MetricsConfig::default(),
        CoordinationConfig::default(),
        TransportSecurityConfig::default(),
        Vec::new(),
    )
    .await
    .expect("failed to construct test server")
}

/// Record `room_id`'s sticky session decision directly, isolating membership
/// refresh from finalize emission (which is covered by `session_policy_tests`).
fn store_active_plan(
    server: &EnhancedGameServer,
    room_id: uuid::Uuid,
    topology: Topology,
    transport: Transport,
    host: Option<PlayerId>,
) {
    server.active_session_plans.insert(
        room_id,
        ActiveSessionPlan {
            topology,
            transport,
            host,
        },
    );
}

async fn create_db_room(server: &EnhancedGameServer, owner: PlayerId) -> uuid::Uuid {
    create_db_room_with_max(server, owner, 8).await
}

async fn create_db_room_with_max(
    server: &EnhancedGameServer,
    owner: PlayerId,
    max_players: u8,
) -> uuid::Uuid {
    server
        .database
        .create_room(
            "webrtc-game".to_string(),
            None,
            max_players,
            true,
            owner,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds")
        .id
}

fn player_info(id: PlayerId, name: &str) -> PlayerInfo {
    PlayerInfo {
        id,
        name: name.to_string(),
        is_authority: false,
        is_ready: false,
        connected_at: chrono::Utc::now(),
        connection_info: None,
        epoch: None,
        seq: None,
        region_id: "region-a".to_string(),
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
        "late-join signaling fixtures intentionally exercise the default [turn] STUN URL"
    );
}

/// A STUN-only `SessionConfig` preferring `mesh` (so an all-v3+webrtc room
/// resolves to `mesh + webrtc`).
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

/// Drive a full (player count == max) room through lobby → all-ready → finalize
/// in the database, so the reconnect path's fresh room read observes
/// `LobbyState::Finalized` and re-pairing engages. `players` must list every
/// member id currently in the room.
async fn finalize_db_room(server: &EnhancedGameServer, room_id: &uuid::Uuid, players: &[PlayerId]) {
    let initial = server
        .database
        .get_room_by_id(room_id)
        .await
        .expect("room lookup before lobby transition")
        .expect("room exists before lobby transition");
    if initial.lobby_state == LobbyState::Waiting {
        server
            .database
            .transition_room_to_lobby(room_id)
            .await
            .expect("transition to lobby");
    }
    for player in players {
        server
            .database
            .toggle_player_ready(room_id, player)
            .await
            .expect("toggle ready");
    }
    let start_snapshot = server
        .database
        .get_room_by_id(room_id)
        .await
        .expect("room lookup")
        .expect("room exists before finalize");
    let expectation = crate::database::FinalizeRoomGameExpectation::from_room(&start_snapshot);
    server
        .database
        .finalize_room_game(room_id, &expectation)
        .await
        .expect("finalize room");
    let room = server
        .database
        .get_room_by_id(room_id)
        .await
        .expect("room lookup")
        .expect("room exists");
    assert_eq!(
        room.lobby_state,
        LobbyState::Finalized,
        "room must reach Finalized for the reconnect re-pairing test"
    );
}

/// Register a client and return its id plus the receiving half of its channel.
async fn register_client(
    server: &EnhancedGameServer,
) -> (PlayerId, mpsc::Receiver<Arc<ServerMessage>>) {
    register_client_with_queue_capacity(server, 16).await
}

async fn register_client_with_queue_capacity(
    server: &EnhancedGameServer,
    capacity: usize,
) -> (PlayerId, mpsc::Receiver<Arc<ServerMessage>>) {
    let (sender, receiver) = mpsc::channel(capacity);
    let player_id = server
        .connection_manager
        .register_client(
            sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            next_addr(),
            server.instance_id,
        )
        .await
        .expect("client registration succeeds");
    (player_id, receiver)
}

struct FinalizedJoinPublicationFixture {
    joiner: PlayerId,
    room: crate::protocol::Room,
    joined_player: PlayerInfo,
    incumbent_rx: mpsc::Receiver<Arc<ServerMessage>>,
    joiner_rx: mpsc::Receiver<Arc<ServerMessage>>,
    reconnect_token: String,
}

async fn setup_finalized_join_publication(
    server: &Arc<EnhancedGameServer>,
) -> FinalizedJoinPublicationFixture {
    let (incumbent, incumbent_rx) = register_client(server).await;
    let (joiner, joiner_rx) = register_client(server).await;
    server.set_client_protocol(&incumbent, v3_webrtc());
    server.set_client_protocol(&joiner, v3_webrtc());

    let room_id = create_db_room_with_max(server, incumbent, 2).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(joiner, "joiner"))
        .await
        .expect("add finalized joiner");
    for player_id in [incumbent, joiner] {
        server
            .connection_manager
            .assign_client_to_room(&player_id, room_id)
            .await;
    }
    finalize_db_room(server, &room_id, &[incumbent, joiner]).await;
    store_active_plan(server, room_id, Topology::Mesh, Transport::WebRtc, None);

    let room = server
        .database
        .get_room_by_id(&room_id)
        .await
        .expect("finalized room lookup")
        .expect("finalized room remains present");
    let stamp = server
        .connection_manager
        .current_relay_stamp_in_room(&joiner, &room_id)
        .expect("joiner has a routed incarnation");
    let mut joined_player = room
        .players
        .get(&joiner)
        .cloned()
        .expect("joiner remains in finalized snapshot");
    joined_player.epoch = Some(stamp.epoch);
    joined_player.seq = Some(stamp.seq);
    let reconnect_token = server
        .pre_issue_reconnection_token_for(&joiner, room_id)
        .await
        .expect("v3 finalized joiner receives a reconnect token");

    FinalizedJoinPublicationFixture {
        joiner,
        room,
        joined_player,
        incumbent_rx,
        joiner_rx,
        reconnect_token,
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn finalized_join_uses_delivered_baseline_when_refresh_fails() {
    let server = create_test_server_with_session(mesh_session_config()).await;
    let mut fixture = setup_finalized_join_publication(&server).await;
    let db = server
        .database()
        .as_any()
        .downcast_ref::<InMemoryDatabase>()
        .expect("test server uses in-memory database");
    db.fail_get_room_by_id_for_test(true);

    let guard = server
        .message_coordinator
        .lock_room_event_mutation(&fixture.room.id)
        .await;
    assert!(
        server
            .publish_finalized_join_membership(
                &fixture.room,
                fixture.joiner,
                fixture.joined_player.clone(),
                guard,
            )
            .await,
        "transient refresh failure must not degrade a live finalized actor to lifecycle-only"
    );
    db.fail_get_room_by_id_for_test(false);

    assert!(matches!(
        recv(&mut fixture.joiner_rx).await.as_ref(),
        ServerMessage::SessionPlan(plan)
            if plan.topology == Topology::Mesh && plan.transport == Transport::WebRtc
    ));
    assert!(matches!(
        recv(&mut fixture.incumbent_rx).await.as_ref(),
        ServerMessage::PlayerJoined { player } if player.id == fixture.joiner
    ));
    assert!(matches!(
        recv(&mut fixture.incumbent_rx).await.as_ref(),
        ServerMessage::SessionPlan(plan)
            if plan.topology == Topology::Mesh && plan.transport == Transport::WebRtc
    ));
    assert!(server.connection_manager.has_client(&fixture.joiner));
    assert_silent(&mut fixture.joiner_rx).await;
    assert_silent(&mut fixture.incumbent_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn finalized_join_rejects_a_refresh_that_lost_the_joiner() {
    let server = create_test_server_with_session(mesh_session_config()).await;
    let mut fixture = setup_finalized_join_publication(&server).await;
    let incumbent = fixture
        .room
        .players
        .keys()
        .copied()
        .find(|player_id| *player_id != fixture.joiner)
        .expect("fixture has one incumbent");
    assert!(server
        .database
        .remove_player_from_room(&fixture.room.id, &fixture.joiner)
        .await
        .expect("remove joiner from refreshed snapshot")
        .is_some());

    let guard = server
        .message_coordinator
        .lock_room_event_mutation(&fixture.room.id)
        .await;
    assert!(
        server
            .publish_finalized_join_membership(
                &fixture.room,
                fixture.joiner,
                fixture.joined_player.clone(),
                guard,
            )
            .await,
        "the delivered baseline remains authoritative when refresh loses the actor"
    );

    match recv(&mut fixture.joiner_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => {
            assert_eq!(plan.peers.len(), 1);
            assert_eq!(plan.peers[0].player_id, incumbent);
        }
        other => panic!("joiner expected its baseline SessionPlan, got {other:?}"),
    }
    assert!(matches!(
        recv(&mut fixture.incumbent_rx).await.as_ref(),
        ServerMessage::PlayerJoined { player } if player.id == fixture.joiner
    ));
    assert!(matches!(
        recv(&mut fixture.incumbent_rx).await.as_ref(),
        ServerMessage::SessionPlan(_)
    ));
    assert!(server.connection_manager.has_client(&fixture.joiner));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn finalized_join_uses_a_valid_newer_membership_refresh() {
    let server = create_test_server_with_session(mesh_session_config()).await;
    let mut fixture = setup_finalized_join_publication(&server).await;
    let incumbent = fixture
        .room
        .players
        .keys()
        .copied()
        .find(|player_id| *player_id != fixture.joiner)
        .expect("fixture has one incumbent");
    assert!(server
        .database
        .remove_player_from_room(&fixture.room.id, &incumbent)
        .await
        .expect("remove stale incumbent")
        .is_some());
    server.connection_manager.clear_room_assignment(&incumbent);
    server
        .message_coordinator
        .unregister_local_client(&incumbent)
        .await
        .expect("unroute stale incumbent");

    let (replacement, mut replacement_rx) = register_client(&server).await;
    server.set_client_protocol(&replacement, v3_webrtc());
    assert!(server
        .database
        .add_player_to_room(&fixture.room.id, player_info(replacement, "replacement"),)
        .await
        .expect("add replacement incumbent"));
    server
        .connection_manager
        .assign_client_to_room(&replacement, fixture.room.id)
        .await;

    let guard = server
        .message_coordinator
        .lock_room_event_mutation(&fixture.room.id)
        .await;
    assert!(
        server
            .publish_finalized_join_membership(
                &fixture.room,
                fixture.joiner,
                fixture.joined_player.clone(),
                guard,
            )
            .await
    );

    match recv(&mut fixture.joiner_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => {
            assert_eq!(plan.peers.len(), 1);
            assert_eq!(plan.peers[0].player_id, replacement);
        }
        other => panic!("joiner expected refreshed SessionPlan, got {other:?}"),
    }
    assert!(matches!(
        recv(&mut replacement_rx).await.as_ref(),
        ServerMessage::PlayerJoined { player } if player.id == fixture.joiner
    ));
    match recv(&mut replacement_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => {
            assert_eq!(plan.peers.len(), 1);
            assert_eq!(plan.peers[0].player_id, fixture.joiner);
        }
        other => panic!("replacement expected refreshed SessionPlan, got {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn finalized_join_counts_every_committed_turn_plan_once() {
    let turn = TurnConfig {
        enabled: true,
        static_auth_secret: "super-secret".to_string(),
        urls: vec![TURN_URL.to_string()],
        stun_urls: vec![TURN_STUN_URL.to_string()],
        credential_ttl_secs: TURN_CREDENTIAL_TTL_SECS,
    };
    let server = create_test_server_with_session_and_turn(mesh_session_config(), turn).await;
    let mut fixture = setup_finalized_join_publication(&server).await;
    let late_before = server
        .metrics
        .session_plans_late_join
        .load(Ordering::Relaxed);
    let credentials_before = server
        .metrics
        .turn_credentials_issued
        .load(Ordering::Relaxed);

    let guard = server
        .message_coordinator
        .lock_room_event_mutation(&fixture.room.id)
        .await;
    assert!(
        server
            .publish_finalized_join_membership(
                &fixture.room,
                fixture.joiner,
                fixture.joined_player.clone(),
                guard,
            )
            .await
    );

    let actor_plan = recv(&mut fixture.joiner_rx).await;
    assert!(
        matches!(actor_plan.as_ref(), ServerMessage::SessionPlan(plan)
        if plan.ice_servers.iter().any(|server| server.username.is_some()))
    );
    assert!(matches!(
        recv(&mut fixture.incumbent_rx).await.as_ref(),
        ServerMessage::PlayerJoined { .. }
    ));
    let incumbent_plan = recv(&mut fixture.incumbent_rx).await;
    assert!(
        matches!(incumbent_plan.as_ref(), ServerMessage::SessionPlan(plan)
        if plan.ice_servers.iter().any(|server| server.username.is_some()))
    );
    assert_eq!(
        server
            .metrics
            .session_plans_late_join
            .load(Ordering::Relaxed),
        late_before + 1
    );
    assert_eq!(
        server
            .metrics
            .turn_credentials_issued
            .load(Ordering::Relaxed),
        credentials_before + 2
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn finalized_v2_join_refreshes_v3_incumbents_without_counting_actor_plan() {
    let server = create_test_server_with_session(mesh_session_config()).await;
    let mut fixture = setup_finalized_join_publication(&server).await;
    server.set_client_protocol(&fixture.joiner, NegotiatedProtocol::default());
    let late_before = server
        .metrics
        .session_plans_late_join
        .load(Ordering::Relaxed);

    let guard = server
        .message_coordinator
        .lock_room_event_mutation(&fixture.room.id)
        .await;
    assert!(
        server
            .publish_finalized_join_membership(
                &fixture.room,
                fixture.joiner,
                fixture.joined_player.clone(),
                guard,
            )
            .await
    );

    assert!(matches!(
        recv(&mut fixture.incumbent_rx).await.as_ref(),
        ServerMessage::PlayerJoined { player } if player.id == fixture.joiner
    ));
    match recv(&mut fixture.incumbent_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => {
            assert_eq!(plan.topology, Topology::Mesh);
            assert!(
                plan.peers.is_empty(),
                "v2 actor is not a P2P pairing target"
            );
        }
        other => panic!("v3 incumbent expected authoritative refresh, got {other:?}"),
    }
    assert_silent(&mut fixture.joiner_rx).await;
    assert_eq!(
        server
            .metrics
            .session_plans_late_join
            .load(Ordering::Relaxed),
        late_before,
        "late-join metric counts an actor plan, not incumbent refresh frames"
    );
}

/// `turn_credentials_issued` is the total-issuance counter operators size TURN
/// capacity from, so it must move for every credential the server actually
/// hands out. A v2 joiner gets no plan of its own, but each v3 incumbent is
/// re-issued a fresh credential in its refreshed plan.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn finalized_join_counts_incumbent_credentials_without_an_actor_plan() {
    let turn = TurnConfig {
        enabled: true,
        static_auth_secret: "super-secret".to_string(),
        urls: vec![TURN_URL.to_string()],
        stun_urls: vec![TURN_STUN_URL.to_string()],
        credential_ttl_secs: TURN_CREDENTIAL_TTL_SECS,
    };
    let server = create_test_server_with_session_and_turn(mesh_session_config(), turn).await;
    let mut fixture = setup_finalized_join_publication(&server).await;
    server.set_client_protocol(&fixture.joiner, NegotiatedProtocol::default());
    let late_before = server
        .metrics
        .session_plans_late_join
        .load(Ordering::Relaxed);
    let credentials_before = server
        .metrics
        .turn_credentials_issued
        .load(Ordering::Relaxed);

    let guard = server
        .message_coordinator
        .lock_room_event_mutation(&fixture.room.id)
        .await;
    assert!(
        server
            .publish_finalized_join_membership(
                &fixture.room,
                fixture.joiner,
                fixture.joined_player.clone(),
                guard,
            )
            .await
    );

    assert!(matches!(
        recv(&mut fixture.incumbent_rx).await.as_ref(),
        ServerMessage::PlayerJoined { player } if player.id == fixture.joiner
    ));
    match recv(&mut fixture.incumbent_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => assert!(
            plan.ice_servers
                .iter()
                .any(|server| server.username.is_some()),
            "the incumbent's refreshed plan carries a freshly minted credential"
        ),
        other => panic!("v3 incumbent expected authoritative refresh, got {other:?}"),
    }
    assert_eq!(
        server
            .metrics
            .session_plans_late_join
            .load(Ordering::Relaxed),
        late_before,
        "late-join metric counts an actor plan, not incumbent refresh frames"
    );
    assert_eq!(
        server
            .metrics
            .turn_credentials_issued
            .load(Ordering::Relaxed),
        credentials_before + 1,
        "every committed credential is counted, actor plan or not"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn finalized_join_transaction_failure_emits_terminal_boundary() {
    let server = create_test_server_with_session(mesh_session_config()).await;
    let mut fixture = setup_finalized_join_publication(&server).await;
    let db = server
        .database()
        .as_any()
        .downcast_ref::<InMemoryDatabase>()
        .expect("test server uses in-memory database");
    db.fail_get_room_by_id_for_test(true);
    db.fail_remove_player_from_room_for_test(true);
    server
        .message_coordinator
        .fail_room_transactions_for_test(true);

    let guard = server
        .message_coordinator
        .lock_room_event_mutation(&fixture.room.id)
        .await;
    assert!(
        server
            .publish_finalized_join_membership(
                &fixture.room,
                fixture.joiner,
                fixture.joined_player.clone(),
                guard,
            )
            .await,
        "opening lifecycle fallback must commit before terminal teardown"
    );
    server
        .message_coordinator
        .fail_room_transactions_for_test(false);

    assert!(matches!(
        recv(&mut fixture.incumbent_rx).await.as_ref(),
        ServerMessage::PlayerJoined { player } if player.id == fixture.joiner
    ));
    assert!(matches!(
        recv(&mut fixture.incumbent_rx).await.as_ref(),
        ServerMessage::PlayerLeft { player_id, epoch: Some(_), final_seq: Some(_) }
            if *player_id == fixture.joiner
    ));
    assert!(
        !server.connection_manager.has_client(&fixture.joiner),
        "explicit teardown must not depend on a real socket task"
    );
    assert_eq!(server.get_client_room(&fixture.joiner).await, None);
    assert!(db
        .get_room_players(&fixture.room.id)
        .await
        .expect("membership remains readable during removal outage")
        .iter()
        .any(|player| player.id == fixture.joiner));
    db.fail_get_room_by_id_for_test(false);
    db.fail_remove_player_from_room_for_test(false);
    assert_eq!(server.cleanup_pending_durable_player_detaches().await, 1);
    assert!(!db
        .get_room_players(&fixture.room.id)
        .await
        .expect("membership remains readable after recovery")
        .iter()
        .any(|player| player.id == fixture.joiner));
    let disconnected = server
        .reconnection_manager()
        .expect("reconnection enabled")
        .validate_reconnection(&fixture.joiner, &fixture.room.id, &fixture.reconnect_token)
        .await
        .expect("fail-closed teardown preserves the client-visible reconnect token");
    assert_eq!(disconnected.player_id, fixture.joiner);
    assert_eq!(disconnected.room_id, fixture.room.id);
    assert!(
        !disconnected.was_authority,
        "a failed non-authority join must not gain authority in its reconnect snapshot"
    );
    assert!(
        disconnected.player_info.is_some(),
        "pending cleanup/reconnect record retains the complete membership snapshot"
    );
    let terminated = fixture.joiner_rx.try_recv();
    assert!(matches!(
        terminated,
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn finalized_join_exhausts_a_zero_budget_routing_retry() {
    let server = create_test_server_with_session(mesh_session_config()).await;
    let fixture = setup_finalized_join_publication(&server).await;
    let mut zero_budget_baseline = fixture.room.clone();
    zero_budget_baseline.players.clear();

    // Keep one routed id out of the database snapshot so every exact-membership
    // commit reports RoutingChanged. The empty delivered baseline gives the
    // publication one attempt and exercises the saturating zero boundary.
    let (ghost, _ghost_rx) = register_client(&server).await;
    server.set_client_protocol(&ghost, v3_webrtc());
    server
        .connection_manager
        .assign_client_to_room(&ghost, fixture.room.id)
        .await;
    let guard = server
        .message_coordinator
        .lock_room_event_mutation(&fixture.room.id)
        .await;

    let published = timeout(
        Duration::from_secs(1),
        server.publish_finalized_join_membership(
            &zero_budget_baseline,
            fixture.joiner,
            fixture.joined_player,
            guard,
        ),
    )
    .await
    .expect("exhausted routing retries must terminate instead of spinning");
    assert!(published, "the lifecycle fallback still publishes");
    timeout(Duration::from_secs(1), async {
        while server.connection_manager.has_client(&fixture.joiner) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the failed actor is terminalized after retry exhaustion");
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn missing_join_publication_snapshot_preserves_open_then_terminal_order() {
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (incumbent, mut incumbent_rx) = register_client(&server).await;
    server.set_client_protocol(&incumbent, v3_webrtc());
    server
        .handle_join_room(
            &incumbent,
            "snapshot-order".to_string(),
            Some("SNP001".to_string()),
            "incumbent".to_string(),
            Some(2),
            Some(false),
            None,
        )
        .await;
    let room_id = server
        .get_client_room(&incumbent)
        .await
        .expect("incumbent joined snapshot-order room");
    assert!(matches!(
        recv(&mut incumbent_rx).await.as_ref(),
        ServerMessage::RoomJoined(_)
    ));
    assert!(matches!(
        recv(&mut incumbent_rx).await.as_ref(),
        ServerMessage::LobbyStateChanged { .. }
    ));
    finalize_db_room(&server, &room_id, &[incumbent]).await;
    store_active_plan(&server, room_id, Topology::Mesh, Transport::WebRtc, None);

    let (joiner, mut joiner_rx) = register_client(&server).await;
    server.set_client_protocol(&joiner, v3_webrtc());
    server.fail_retain_room_publication_snapshot_for_test(true);
    server
        .handle_join_room(
            &joiner,
            "snapshot-order".to_string(),
            Some("SNP001".to_string()),
            "joiner".to_string(),
            Some(2),
            Some(false),
            None,
        )
        .await;
    server.fail_retain_room_publication_snapshot_for_test(false);

    assert!(matches!(
        recv(&mut joiner_rx).await.as_ref(),
        ServerMessage::RoomJoined(payload)
            if payload.lobby_state == LobbyState::Finalized
    ));
    let opening_epoch = match recv(&mut incumbent_rx).await.as_ref() {
        ServerMessage::PlayerJoined { player } if player.id == joiner => {
            player.epoch.expect("opening lifecycle carries epoch")
        }
        other => panic!("incumbent expected PlayerJoined, got {other:?}"),
    };
    match recv(&mut incumbent_rx).await.as_ref() {
        ServerMessage::PlayerLeft {
            player_id,
            epoch: Some(epoch),
            final_seq: Some(_),
        } => {
            assert_eq!(*player_id, joiner);
            assert_eq!(*epoch, opening_epoch);
        }
        other => panic!("incumbent expected matching PlayerLeft, got {other:?}"),
    }
    assert!(!server.connection_manager.has_client(&joiner));
    let terminal_channel = timeout(Duration::from_secs(1), joiner_rx.recv())
        .await
        .expect("terminated join channel closes promptly");
    assert!(terminal_channel.is_none());
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn coordinator_pruned_join_actor_is_automatically_terminalized() {
    let server = create_test_server_with_session(mesh_session_config()).await;
    let FinalizedJoinPublicationFixture {
        joiner,
        room,
        joined_player,
        mut incumbent_rx,
        joiner_rx,
        ..
    } = setup_finalized_join_publication(&server).await;
    drop(joiner_rx);

    let guard = server
        .message_coordinator
        .lock_room_event_mutation(&room.id)
        .await;
    assert!(
        server
            .publish_finalized_join_membership(&room, joiner, joined_player, guard)
            .await
    );

    let opening_epoch = match recv(&mut incumbent_rx).await.as_ref() {
        ServerMessage::PlayerJoined { player } if player.id == joiner => {
            player.epoch.expect("opening lifecycle carries epoch")
        }
        other => panic!("incumbent expected PlayerJoined, got {other:?}"),
    };
    match recv(&mut incumbent_rx).await.as_ref() {
        ServerMessage::PlayerLeft {
            player_id,
            epoch: Some(epoch),
            final_seq: Some(_),
        } => {
            assert_eq!(*player_id, joiner);
            assert_eq!(*epoch, opening_epoch);
        }
        other => panic!("incumbent expected matching PlayerLeft, got {other:?}"),
    }
    assert!(!server.connection_manager.has_client(&joiner));
    assert_eq!(server.get_client_room(&joiner).await, None);
}

struct FinalizedReconnectFixture {
    existing: PlayerId,
    reconnecting: PlayerId,
    current: PlayerId,
    room_id: uuid::Uuid,
    token: String,
    existing_rx: mpsc::Receiver<Arc<ServerMessage>>,
    current_rx: mpsc::Receiver<Arc<ServerMessage>>,
}

async fn setup_mesh_reconnect(
    server: &Arc<EnhancedGameServer>,
    existing_capacity: usize,
    current_capacity: usize,
    finalized: bool,
) -> FinalizedReconnectFixture {
    let (existing, existing_rx) =
        register_client_with_queue_capacity(server, existing_capacity).await;
    let (reconnecting, old_rx) = register_client(server).await;
    let (current, current_rx) = register_client_with_queue_capacity(server, current_capacity).await;
    drop(old_rx);
    server.set_client_protocol(&existing, v3_webrtc());
    server.set_client_protocol(&reconnecting, v3_webrtc());
    server.set_client_protocol(&current, v3_webrtc());

    let room_id = create_db_room_with_max(server, existing, 2).await;
    store_active_plan(server, room_id, Topology::Mesh, Transport::WebRtc, None);
    let reconnecting_info = player_info(reconnecting, "reconnecting");
    server
        .database
        .add_player_to_room(&room_id, reconnecting_info.clone())
        .await
        .expect("add reconnecting player");
    server
        .connection_manager
        .assign_client_to_room(&existing, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&reconnecting, room_id)
        .await;
    if finalized {
        finalize_db_room(server, &room_id, &[existing, reconnecting]).await;
    }

    let token = server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(
            reconnecting,
            room_id,
            false,
            Some(reconnecting_info),
            server
                .connection_manager
                .game_data_epoch(&reconnecting)
                .unwrap_or(0),
        )
        .await;
    server
        .database
        .remove_player_from_room(&room_id, &reconnecting)
        .await
        .expect("remove reconnecting player");
    server.connection_manager.remove_client(&reconnecting);
    server
        .message_coordinator
        .unregister_local_client(&reconnecting)
        .await
        .expect("unroute disconnected player");

    FinalizedReconnectFixture {
        existing,
        reconnecting,
        current,
        room_id,
        token,
        existing_rx,
        current_rx,
    }
}

fn v3_webrtc() -> NegotiatedProtocol {
    NegotiatedProtocol {
        version: 3,
        transports: vec![Transport::Relay, Transport::WebRtc],
        topologies: vec![Topology::Relay, Topology::Mesh],
    }
}

/// A v3 + WebRTC client that also advertises the `host` topology, so a
/// host-preferring room resolves to `host + webrtc` rather than downgrading.
fn v3_webrtc_host() -> NegotiatedProtocol {
    NegotiatedProtocol {
        version: 3,
        transports: vec![Transport::Relay, Transport::WebRtc],
        topologies: vec![Topology::Relay, Topology::Host, Topology::Mesh],
    }
}

/// A v3 client advertising WebRTC *and* Direct transports plus the `host`
/// topology. It clears the late-join WebRTC gate (it supports WebRTC), so a room
/// that nonetheless resolves to `host + direct` (the deployment disabled WebRTC)
/// isolates the plan's *transport* gate rather than the per-peer capability gate.
fn v3_webrtc_direct_host() -> NegotiatedProtocol {
    NegotiatedProtocol {
        version: 3,
        transports: vec![Transport::Relay, Transport::WebRtc, Transport::Direct],
        topologies: vec![Topology::Relay, Topology::Host],
    }
}

fn v3_relay_only() -> NegotiatedProtocol {
    NegotiatedProtocol {
        version: 3,
        transports: vec![Transport::Relay],
        topologies: vec![Topology::Relay],
    }
}

fn v2_with_webrtc_transport() -> NegotiatedProtocol {
    NegotiatedProtocol {
        version: 2,
        transports: vec![Transport::Relay, Transport::WebRtc],
        topologies: vec![Topology::Relay],
    }
}

/// Receive the next message or fail if none arrives promptly.
async fn recv(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>) -> Arc<ServerMessage> {
    timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("channel still open")
        .expect("message present")
}

/// Assert that no message is pending within a short window.
async fn assert_silent(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>) {
    match timeout(Duration::from_millis(100), receiver.recv()).await {
        Err(_) => {}
        Ok(Some(message)) => panic!("expected no message to be delivered, got {message:?}"),
        Ok(None) => panic!("channel closed while checking for silence"),
    }
}

async fn valid_signal_budget_used(server: &EnhancedGameServer, player_id: &PlayerId) -> u32 {
    server
        .rate_limiter
        .get_player_stats(player_id)
        .await
        .map_or(0, |stats| stats.signals)
}

fn error_code(message: &ServerMessage) -> Option<ErrorCode> {
    match message {
        ServerMessage::Error { error_code, .. } => error_code.clone(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Glare determinism (Appendix E).
// ---------------------------------------------------------------------------

#[test]
fn local_initiates_is_antisymmetric_and_irreflexive() {
    for _ in 0..256 {
        let a = PlayerId::new_v4();
        let b = PlayerId::new_v4();
        if a == b {
            continue;
        }
        // Exactly one side initiates.
        assert_ne!(local_initiates(a, b), local_initiates(b, a));
        // Lesser id initiates.
        if a < b {
            assert!(local_initiates(a, b));
            assert!(!local_initiates(b, a));
        } else {
            assert!(local_initiates(b, a));
            assert!(!local_initiates(a, b));
        }
        // Self never initiates.
        assert!(!local_initiates(a, a));
        assert!(!local_initiates(b, b));
    }
}

// ---------------------------------------------------------------------------
// handle_signal happy path + ordering.
// ---------------------------------------------------------------------------

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_delivered_to_same_room_peer_preserving_payload() {
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&bob, room_id)
        .await;

    // Drive a full offer -> answer -> ICE sequence and assert in-order delivery
    // with byte-identical payloads.
    let offer = json!({ "Offer": "v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\n" });
    let answer = json!({ "Answer": "v=0\r\no=- 3 4 IN IP4 0.0.0.0\r\n" });
    let ice = json!({ "IceCandidate": "candidate:1 1 UDP 2130706431 1.2.3.4 5000 typ host" });

    server.handle_signal(&alice, bob, offer.clone()).await;
    server.handle_signal(&bob, alice, answer.clone()).await;
    server.handle_signal(&alice, bob, ice.clone()).await;

    match recv(&mut bob_rx).await.as_ref() {
        ServerMessage::Signal { from, signal, .. } => {
            assert_eq!(*from, alice);
            assert_eq!(*signal, offer);
        }
        other => panic!("expected Signal(offer), got {other:?}"),
    }
    match recv(&mut alice_rx).await.as_ref() {
        ServerMessage::Signal { from, signal, .. } => {
            assert_eq!(*from, bob);
            assert_eq!(*signal, answer);
        }
        other => panic!("expected Signal(answer), got {other:?}"),
    }
    match recv(&mut bob_rx).await.as_ref() {
        ServerMessage::Signal { from, signal, .. } => {
            assert_eq!(*from, alice);
            assert_eq!(*signal, ice);
        }
        other => panic!("expected Signal(ice), got {other:?}"),
    }

    // The sender never receives its own signal echoed back.
    assert_silent(&mut alice_rx).await;
    assert_silent(&mut bob_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_from_a_replaced_connection_lifecycle_is_discarded() {
    let server = create_test_server().await;
    let (alice, _alice_rx, bob, mut bob_rx) = webrtc_pair_in_room(&server).await;
    let room_id = server
        .get_client_room(&alice)
        .await
        .expect("pair is room-routed");
    let old_lifecycle = server
        .connection_manager
        .client_lifecycle(&alice)
        .expect("sender has a lifecycle");
    let old_guard = old_lifecycle.lock().await;
    let lifecycle_refs_before_signal = Arc::strong_count(&old_lifecycle);
    let signal_task = {
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            server
                .handle_signal(&alice, bob, json!({ "Offer": "stale-lifecycle" }))
                .await;
        })
    };
    timeout(Duration::from_secs(1), async {
        while Arc::strong_count(&old_lifecycle) <= lifecycle_refs_before_signal {
            assert!(
                !signal_task.is_finished(),
                "signal handler exited before capturing the old lifecycle"
            );
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("signal handler must capture the old lifecycle before replacement");

    let (replacement_tx, _replacement_rx) = mpsc::channel(16);
    server
        .connection_manager
        .connect_test_client(alice, replacement_tx, next_addr())
        .await;
    server.set_client_protocol(&alice, v3_webrtc());
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;
    drop(old_guard);
    signal_task.await.expect("stale signal task must not panic");

    assert_silent(&mut bob_rx).await;
}

// ---------------------------------------------------------------------------
// Rejection branches.
// ---------------------------------------------------------------------------

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_from_player_not_in_room_is_rejected() {
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    // Neither is in a room.
    server
        .handle_signal(&alice, bob, json!({ "Offer": "x" }))
        .await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::NotInRoom));
    assert_silent(&mut bob_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_to_unknown_target_is_rejected() {
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;

    // Target is a random, unregistered player id.
    server
        .handle_signal(&alice, PlayerId::new_v4(), json!({ "Offer": "x" }))
        .await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalTargetNotFound));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_across_rooms_is_rejected() {
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    server
        .connection_manager
        .assign_client_to_room(&alice, uuid::Uuid::new_v4())
        .await;
    server
        .connection_manager
        .assign_client_to_room(&bob, uuid::Uuid::new_v4())
        .await;

    server
        .handle_signal(&alice, bob, json!({ "Offer": "x" }))
        .await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::CrossRoomSignal));
    assert_silent(&mut bob_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_from_non_webrtc_sender_is_rejected() {
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    // v3 but relay-only -> no WebRTC transport negotiated.
    server.set_client_protocol(&alice, v3_relay_only());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&bob, room_id)
        .await;

    server
        .handle_signal(&alice, bob, json!({ "Offer": "x" }))
        .await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::UnsupportedTransport));
    assert_silent(&mut bob_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_sender_must_be_v3_even_if_webrtc_transport_is_present() {
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v2_with_webrtc_transport());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&bob, room_id)
        .await;

    server
        .handle_signal(&alice, bob, json!({ "Offer": "x" }))
        .await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::UnsupportedTransport));
    assert_silent(&mut bob_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_to_v2_peer_reports_target_not_found() {
    // Appendix K gating: a v3 sender targeting a v2 (relay-only) peer in the
    // same room must NOT deliver — it is reported as target-not-found.
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (legacy, mut legacy_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    // legacy uses the default v2 / relay-only protocol (no set_client_protocol).

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&legacy, room_id)
        .await;

    server
        .handle_signal(&alice, legacy, json!({ "Offer": "x" }))
        .await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalTargetNotFound));
    assert_silent(&mut legacy_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_to_v3_relay_only_peer_reports_target_not_found() {
    // Appendix K gating: the target must have negotiated BOTH v3 AND the WebRTC
    // transport. A v3 peer that advertised only `relay` (a valid v3_relay_only
    // state) must NOT be delivered a `Signal` it never opted into; the sender is
    // told the target was not found. This locks the webrtc-transport gate that
    // the v2-target test passes vacuously via the v3 check.
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (relay_only, mut relay_only_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&relay_only, v3_relay_only());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&relay_only, room_id)
        .await;

    server
        .handle_signal(&alice, relay_only, json!({ "Offer": "x" }))
        .await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalTargetNotFound));
    assert_silent(&mut relay_only_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn self_signal_is_rejected() {
    // A peer cannot WebRTC to itself: a self-targeted signal is rejected and the
    // sender never receives its own echoed `Signal`.
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;

    server
        .handle_signal(&alice, alice, json!({ "Offer": "x" }))
        .await;

    // The sender receives a rejection, not its own echoed signal.
    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalTargetNotFound));
    assert!(
        !matches!(msg.as_ref(), ServerMessage::Signal { .. }),
        "sender must not receive its own echoed Signal"
    );
    assert_silent(&mut alice_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_rate_limit_trips_after_budget() {
    let server = create_test_server_with_signals(2).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&bob, room_id)
        .await;

    // First two succeed and reach bob.
    server
        .handle_signal(&alice, bob, json!({ "IceCandidate": "1" }))
        .await;
    server
        .handle_signal(&alice, bob, json!({ "IceCandidate": "2" }))
        .await;
    assert!(matches!(
        recv(&mut bob_rx).await.as_ref(),
        ServerMessage::Signal { .. }
    ));
    assert!(matches!(
        recv(&mut bob_rx).await.as_ref(),
        ServerMessage::Signal { .. }
    ));

    // Third trips the limit: sender gets an error, bob receives nothing more.
    server
        .handle_signal(&alice, bob, json!({ "IceCandidate": "3" }))
        .await;
    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalRateLimited));
    assert_silent(&mut bob_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn rejected_signal_attempts_are_rate_limited() {
    let server = create_test_server_with_signal_limits(600, 1).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;

    server
        .handle_signal(&alice, PlayerId::new_v4(), json!({ "Offer": "bad-1" }))
        .await;
    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalTargetNotFound));

    server
        .handle_signal(&alice, PlayerId::new_v4(), json!({ "Offer": "bad-2" }))
        .await;
    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalRateLimited));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn rejected_signals_do_not_consume_valid_signal_budget() {
    let server = create_test_server_with_signal_limits(1, 4).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (legacy, mut legacy_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    // legacy stays default v2 / relay-only.
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    for player in [&alice, &legacy, &bob] {
        server
            .connection_manager
            .assign_client_to_room(player, room_id)
            .await;
    }

    server
        .handle_signal(&alice, legacy, json!({ "Offer": "bad-target" }))
        .await;
    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalTargetNotFound));
    assert_silent(&mut legacy_rx).await;

    server
        .handle_signal(&alice, bob, json!({ "IceCandidate": "valid" }))
        .await;
    assert!(matches!(
        recv(&mut bob_rx).await.as_ref(),
        ServerMessage::Signal { .. }
    ));

    server
        .handle_signal(&alice, bob, json!({ "IceCandidate": "over-budget" }))
        .await;
    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalRateLimited));
}

// ---------------------------------------------------------------------------
// Signal payload size cap (`security.max_signal_bytes`).
// ---------------------------------------------------------------------------

/// Register a v3+WebRTC pair sharing one room on `server`.
async fn webrtc_pair_in_room(
    server: &EnhancedGameServer,
) -> (
    PlayerId,
    mpsc::Receiver<Arc<ServerMessage>>,
    PlayerId,
    mpsc::Receiver<Arc<ServerMessage>>,
) {
    let (alice, alice_rx) = register_client(server).await;
    let (bob, bob_rx) = register_client(server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&bob, room_id)
        .await;

    (alice, alice_rx, bob, bob_rx)
}

/// Serialized length of an opaque signal payload, exactly as the cap measures it.
fn payload_len(signal: &serde_json::Value) -> usize {
    serde_json::to_vec(signal).expect("signal serializes").len()
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_exactly_at_size_cap_is_relayed() {
    let payload = json!({ "Offer": "v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\n" });
    // Boundary: the cap equals the payload's canonical serialized length.
    let server = create_test_server_with_signal_policy(600, 60, payload_len(&payload)).await;
    let (alice, mut alice_rx, bob, mut bob_rx) = webrtc_pair_in_room(&server).await;

    server.handle_signal(&alice, bob, payload.clone()).await;

    match recv(&mut bob_rx).await.as_ref() {
        ServerMessage::Signal { from, signal, .. } => {
            assert_eq!(*from, alice);
            assert_eq!(*signal, payload, "at-cap payload must relay byte-preserved");
        }
        other => panic!("expected Signal, got {other:?}"),
    }
    assert_silent(&mut alice_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_one_byte_over_size_cap_is_rejected() {
    let payload = json!({ "Offer": "v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\n" });
    // Boundary: cap is one byte below the payload's serialized length.
    let server = create_test_server_with_signal_policy(600, 60, payload_len(&payload) - 1).await;
    let (alice, mut alice_rx, bob, mut bob_rx) = webrtc_pair_in_room(&server).await;

    server.handle_signal(&alice, bob, payload).await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalTooLarge));
    if let ServerMessage::Error { message, .. } = msg.as_ref() {
        assert!(
            message.contains("bytes"),
            "rejection should name the sizes: {message}"
        );
    }
    assert_silent(&mut bob_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn oversized_signal_is_rejected_before_any_other_check() {
    // The size cap is step 0: even a sender that is not in any room gets the
    // size rejection, not NotInRoom, proving no relay work precedes the cap.
    let server = create_test_server_with_signal_policy(600, 60, 8).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());

    server
        .handle_signal(
            &alice,
            PlayerId::new_v4(),
            json!({ "Offer": "definitely-longer-than-eight-bytes" }),
        )
        .await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalTooLarge));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn oversized_signal_rejection_does_not_consume_valid_signal_budget() {
    let small = json!({ "IceCandidate": "ok" });
    let oversized = json!({ "Offer": "x".repeat(64) });
    // Valid-signal budget of exactly 1, cap sized to admit only `small`.
    let server = create_test_server_with_signal_policy(1, 60, payload_len(&small)).await;
    let (alice, mut alice_rx, bob, mut bob_rx) = webrtc_pair_in_room(&server).await;

    // Oversized first: rejected without touching the valid-signal budget.
    server.handle_signal(&alice, bob, oversized).await;
    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalTooLarge));
    assert_silent(&mut bob_rx).await;

    // The single valid-signal budget slot is still available...
    server.handle_signal(&alice, bob, small.clone()).await;
    match recv(&mut bob_rx).await.as_ref() {
        ServerMessage::Signal { signal, .. } => assert_eq!(*signal, small),
        other => panic!("expected Signal, got {other:?}"),
    }

    // ...and was budget slot #1 of 1, proving the oversized attempt did not
    // consume it.
    server.handle_signal(&alice, bob, small).await;
    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalRateLimited));
    assert_silent(&mut bob_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_dispatch_waits_for_room_plan_publication_gate() {
    let server = create_test_server().await;
    let (alice, mut alice_rx, bob, mut bob_rx) = webrtc_pair_in_room(&server).await;
    let room_id = server
        .get_client_room(&alice)
        .await
        .expect("pair is room-routed");
    let publication_guard = server
        .message_coordinator
        .lock_room_event_mutation(&room_id)
        .await;
    let mut signal_task = {
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            server
                .handle_signal(&alice, bob, json!({ "Offer": "plan-gated" }))
                .await;
        })
    };

    assert!(
        timeout(Duration::from_millis(20), &mut signal_task)
            .await
            .is_err(),
        "signal must wait while a room plan publication owns the gate"
    );
    assert_silent(&mut bob_rx).await;
    drop(publication_guard);
    signal_task.await.expect("signal task must not panic");

    assert!(matches!(
        recv(&mut bob_rx).await.as_ref(),
        ServerMessage::Signal { from, signal, .. }
            if *from == alice && signal == &json!({ "Offer": "plan-gated" })
    ));
    assert_silent(&mut alice_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_waiting_on_plan_gate_cannot_cross_target_incarnations() {
    let server = create_test_server().await;
    let (alice, mut alice_rx, bob, mut bob_rx) = webrtc_pair_in_room(&server).await;
    let room_id = server
        .get_client_room(&alice)
        .await
        .expect("pair is room-routed");
    let publication_guard = server
        .message_coordinator
        .lock_room_event_mutation(&room_id)
        .await;
    let mut signal_task = {
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            server
                .handle_signal(&alice, bob, json!({ "IceCandidate": "old-epoch" }))
                .await;
        })
    };
    assert!(timeout(Duration::from_millis(20), &mut signal_task)
        .await
        .is_err());
    let old_epoch = server
        .connection_manager
        .game_data_epoch(&bob)
        .expect("target has an incarnation");
    server
        .connection_manager
        .set_game_data_epoch(&bob, old_epoch.saturating_add(1));
    drop(publication_guard);
    signal_task.await.expect("signal task must not panic");

    assert_eq!(
        error_code(recv(&mut alice_rx).await.as_ref()),
        Some(ErrorCode::SignalTargetNotFound)
    );
    assert_silent(&mut bob_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn signal_target_fallback_requires_the_current_room_assignment() {
    let server = create_test_server().await;
    let (alice, _alice_rx, bob, _bob_rx) = webrtc_pair_in_room(&server).await;
    let signal_room = server
        .get_client_room(&alice)
        .await
        .expect("pair is room-routed");

    for (target_room, expected) in [(signal_room, true), (uuid::Uuid::new_v4(), false)] {
        server
            .connection_manager
            .assign_client_to_room(&bob, target_room)
            .await;
        assert_eq!(
            server
                .signal_target_is_routed_after_gate(None, &bob, signal_room)
                .await,
            expected,
            "fallback routing must compare the target's post-gate room"
        );
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reconnect_restores_room_membership_plan_and_webrtc_pairing() {
    // Reconnect re-entry consults the sticky session and refreshes every v3
    // member. The actor gets its plan in phase zero; incumbents get the
    // lifecycle boundary in phase zero and their authoritative plan in phase
    // one. Fresh TURN credentials prove this is a new plan, not cached state.
    let turn = TurnConfig {
        enabled: true,
        static_auth_secret: "super-secret".to_string(),
        urls: vec![TURN_URL.to_string()],
        stun_urls: vec![TURN_STUN_URL.to_string()],
        credential_ttl_secs: TURN_CREDENTIAL_TTL_SECS,
    };
    let server = create_test_server_with_session_and_turn(mesh_session_config(), turn).await;
    let mut fixture = setup_mesh_reconnect(&server, 16, 16, true).await;
    let late_before = server
        .metrics
        .session_plans_late_join
        .load(Ordering::Relaxed);
    let credentials_before = server
        .metrics
        .turn_credentials_issued
        .load(Ordering::Relaxed);

    let reconnected = server
        .handle_reconnect(
            &fixture.current,
            &fixture.reconnecting,
            &fixture.room_id,
            &fixture.token,
        )
        .await;
    assert!(reconnected, "valid reconnect should report success");

    // The incarnation epoch resumes at `last_epoch + 1` (the pre-disconnect
    // value 1 captured for `register_disconnection` above), so a recipient that
    // stayed connected sees the reconnector's `(epoch, seq)` stream keep
    // increasing across the reconnect instead of colliding at the first
    // incarnation's `(1, …)`.
    assert_eq!(
        server
            .connection_manager
            .game_data_epoch(&fixture.reconnecting),
        Some(2),
        "reconnect must resume the incarnation epoch at last_epoch + 1"
    );

    match recv(&mut fixture.current_rx).await.as_ref() {
        ServerMessage::Reconnected(payload) => {
            assert_eq!(payload.player_id, fixture.reconnecting);
            assert!(
                payload
                    .current_players
                    .iter()
                    .any(|player| player.id == fixture.reconnecting),
                "reconnected payload should include restored player membership"
            );
        }
        other => panic!("expected Reconnected, got {other:?}"),
    }

    match recv(&mut fixture.existing_rx).await.as_ref() {
        ServerMessage::PlayerReconnected { player_id, .. } => {
            assert_eq!(*player_id, fixture.reconnecting)
        }
        other => panic!("expected PlayerReconnected, got {other:?}"),
    }

    let reconnecting_flag = match recv(&mut fixture.current_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => {
            assert_eq!(plan.topology, Topology::Mesh);
            assert_eq!(plan.transport, Transport::WebRtc);
            assert_eq!(plan.peers.len(), 1);
            assert_eq!(plan.peers[0].player_id, fixture.existing);
            let turn = plan
                .ice_servers
                .iter()
                .find(|server| server.username.is_some())
                .expect("reconnector gets a freshly minted TURN credential");
            assert!(turn
                .username
                .as_deref()
                .is_some_and(|username| username.ends_with(&fixture.reconnecting.to_string())));
            plan.peers[0].initiate
        }
        other => panic!("reconnecting expected SessionPlan after reconnect, got {other:?}"),
    };
    // The incumbent's phase-one plan follows its phase-zero lifecycle event.
    let existing_flag = match recv(&mut fixture.existing_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => {
            assert_eq!(plan.topology, Topology::Mesh);
            assert_eq!(plan.transport, Transport::WebRtc);
            assert_eq!(plan.peers.len(), 1);
            assert_eq!(plan.peers[0].player_id, fixture.reconnecting);
            let turn = plan
                .ice_servers
                .iter()
                .find(|server| server.username.is_some())
                .expect("incumbent gets a freshly minted TURN credential");
            assert!(turn
                .username
                .as_deref()
                .is_some_and(|username| username.ends_with(&fixture.existing.to_string())));
            plan.peers[0].initiate
        }
        other => panic!("existing expected refreshed SessionPlan, got {other:?}"),
    };
    assert_ne!(existing_flag, reconnecting_flag);
    assert_eq!(
        server
            .metrics
            .session_plans_late_join
            .load(Ordering::Relaxed),
        late_before + 1,
        "one reconnect refresh publication is counted once"
    );
    assert_eq!(
        server
            .metrics
            .turn_credentials_issued
            .load(Ordering::Relaxed),
        credentials_before + 2,
        "fresh per-recipient TURN credentials are counted for both v3 members"
    );

    assert_silent(&mut fixture.current_rx).await;
    assert_silent(&mut fixture.existing_rx).await;

    let members = server
        .database
        .get_room_players(&fixture.room_id)
        .await
        .expect("room players");
    assert!(
        members
            .iter()
            .any(|player| player.id == fixture.reconnecting),
        "reconnected player must be restored in room storage for future pairing"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reconnect_authority_restore_is_live_and_replay_visible() {
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (authority, _old_authority_rx) = register_client(&server).await;
    let (existing, mut existing_rx) = register_client(&server).await;
    let (current, mut current_rx) = register_client(&server).await;
    for player in [authority, existing, current] {
        server.set_client_protocol(&player, v3_webrtc());
    }
    let room_id = create_db_room_with_max(&server, authority, 2).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(existing, "existing"))
        .await
        .expect("add incumbent");
    for player in [authority, existing] {
        server
            .connection_manager
            .assign_client_to_room(&player, room_id)
            .await;
    }
    finalize_db_room(&server, &room_id, &[authority, existing]).await;
    store_active_plan(&server, room_id, Topology::Mesh, Transport::WebRtc, None);
    let authority_info = server
        .database
        .get_room_by_id(&room_id)
        .await
        .expect("read authority room")
        .expect("room exists")
        .players
        .get(&authority)
        .cloned()
        .expect("authority member exists");
    let manager = server.reconnection_manager().expect("reconnection enabled");
    let replay_waiter = PlayerId::new_v4();
    manager
        .register_disconnection(
            replay_waiter,
            room_id,
            false,
            Some(player_info(replay_waiter, "replay-waiter")),
            0,
        )
        .await;
    let token = manager
        .register_disconnection(
            authority,
            room_id,
            true,
            Some(authority_info),
            server
                .connection_manager
                .game_data_epoch(&authority)
                .unwrap_or(0),
        )
        .await;
    server
        .database
        .remove_player_from_room(&room_id, &authority)
        .await
        .expect("remove disconnected authority")
        .expect("authority was present");
    server.connection_manager.remove_client(&authority);
    server
        .message_coordinator
        .unregister_local_client(&authority)
        .await
        .expect("unroute disconnected authority");

    assert!(
        server
            .handle_reconnect(&current, &authority, &room_id, &token)
            .await
    );
    assert!(matches!(
        recv(&mut current_rx).await.as_ref(),
        ServerMessage::Reconnected(payload) if payload.is_authority
    ));
    assert!(matches!(
        recv(&mut existing_rx).await.as_ref(),
        ServerMessage::PlayerReconnected { player_id, .. } if *player_id == authority
    ));
    assert!(matches!(
        recv(&mut current_rx).await.as_ref(),
        ServerMessage::SessionPlan(_)
    ));
    assert!(matches!(
        recv(&mut existing_rx).await.as_ref(),
        ServerMessage::SessionPlan(_)
    ));
    assert!(matches!(
        recv(&mut current_rx).await.as_ref(),
        ServerMessage::AuthorityChanged { authority_player: Some(player), you_are_authority: true } if *player == authority
    ));
    assert!(matches!(
        recv(&mut existing_rx).await.as_ref(),
        ServerMessage::AuthorityChanged { authority_player: Some(player), you_are_authority: false } if *player == authority
    ));
    assert_eq!(
        server
            .database
            .get_room_by_id(&room_id)
            .await
            .expect("read restored room")
            .expect("room remains")
            .authority_player,
        Some(authority)
    );
    let replay = manager.get_missed_events(&room_id, 0).await;
    assert!(replay.events.iter().any(|message| matches!(
        message,
        ServerMessage::AuthorityChanged { authority_player: Some(player), you_are_authority: false } if *player == authority
    )));
}

/// A reconnecting member's stored snapshot is a pre-disconnect record, not a
/// live authority claim: the room's `authority_player` decides who is flagged.
/// Restoring the snapshot verbatim while a successor holds authority would put
/// two `is_authority` members in every membership payload.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reconnect_does_not_restore_authority_taken_by_a_successor() {
    let server = create_test_server_with_session(mesh_session_config()).await;
    let (authority, _old_authority_rx) = register_client(&server).await;
    let (successor, mut _successor_rx) = register_client(&server).await;
    let (current, _current_rx) = register_client(&server).await;
    for player in [authority, successor, current] {
        server.set_client_protocol(&player, v3_webrtc());
    }
    let room_id = create_db_room_with_max(&server, authority, 2).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(successor, "successor"))
        .await
        .expect("add successor");
    for player in [authority, successor] {
        server
            .connection_manager
            .assign_client_to_room(&player, room_id)
            .await;
    }
    let authority_info = server
        .database
        .get_room_by_id(&room_id)
        .await
        .expect("read authority room")
        .expect("room exists")
        .players
        .get(&authority)
        .cloned()
        .expect("authority member exists");
    assert!(
        authority_info.is_authority,
        "fixture precondition: the disconnecting member is the stored authority"
    );
    let manager = server.reconnection_manager().expect("reconnection enabled");
    let token = manager
        .register_disconnection(
            authority,
            room_id,
            true,
            Some(authority_info),
            server
                .connection_manager
                .game_data_epoch(&authority)
                .unwrap_or(0),
        )
        .await;
    server
        .database
        .remove_player_from_room(&room_id, &authority)
        .await
        .expect("remove disconnected authority")
        .expect("authority was present");
    server.connection_manager.remove_client(&authority);
    server
        .message_coordinator
        .unregister_local_client(&authority)
        .await
        .expect("unroute disconnected authority");

    // The successor claims the vacant authority before the original returns.
    let (granted, reason) = server
        .database
        .request_room_authority(&room_id, &successor, true)
        .await
        .expect("successor authority request");
    assert!(granted, "successor must take vacant authority: {reason:?}");

    assert!(
        server
            .handle_reconnect(&current, &authority, &room_id, &token)
            .await
    );

    let room = server
        .database
        .get_room_by_id(&room_id)
        .await
        .expect("read room after reconnect")
        .expect("room remains");
    assert_eq!(
        room.authority_player,
        Some(successor),
        "the successor keeps authority across the original member's reconnect"
    );
    let flagged: Vec<PlayerId> = room
        .players
        .values()
        .filter(|player| player.is_authority)
        .map(|player| player.id)
        .collect();
    assert_eq!(
        flagged,
        vec![successor],
        "exactly the room's authority_player may carry the stored is_authority flag"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn aborted_reconnect_publication_finishes_both_ordered_phases() {
    let server = create_test_server_with_session(mesh_session_config()).await;
    let mut fixture = setup_mesh_reconnect(&server, 16, 1, true).await;
    let caller = {
        let server = Arc::clone(&server);
        let current = fixture.current;
        let reconnecting = fixture.reconnecting;
        let room_id = fixture.room_id;
        let token = fixture.token.clone();
        tokio::spawn(async move {
            server
                .handle_reconnect(&current, &reconnecting, &room_id, &token)
                .await
        })
    };
    timeout(Duration::from_secs(1), async {
        while server
            .metrics
            .websocket_backpressure_events
            .load(Ordering::Relaxed)
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actor plan waits behind its one-slot Reconnected baseline");
    caller.abort();
    caller
        .await
        .expect_err("test aborts only the caller awaiting the owned reconnect");

    assert!(matches!(
        recv(&mut fixture.current_rx).await.as_ref(),
        ServerMessage::Reconnected(_)
    ));
    assert!(matches!(
        recv(&mut fixture.current_rx).await.as_ref(),
        ServerMessage::SessionPlan(_)
    ));
    assert!(matches!(
        recv(&mut fixture.existing_rx).await.as_ref(),
        ServerMessage::PlayerReconnected { player_id, .. }
            if *player_id == fixture.reconnecting
    ));
    assert!(matches!(
        recv(&mut fixture.existing_rx).await.as_ref(),
        ServerMessage::SessionPlan(_)
    ));
    assert!(server.connection_manager.has_client(&fixture.reconnecting));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reconnect_retries_after_slow_incumbent_with_current_routed_members() {
    let server = create_test_server_with_config(ServerConfig {
        websocket_config: WebSocketConfig {
            slow_consumer_timeout_ms: 10,
            ..WebSocketConfig::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let mut fixture = setup_mesh_reconnect(&server, 1, 16, true).await;
    assert!(server
        .message_coordinator
        .try_send_to_player(&fixture.existing, Arc::new(ServerMessage::Pong))
        .await
        .expect("prefill incumbent queue"));

    assert!(
        server
            .handle_reconnect(
                &fixture.current,
                &fixture.reconnecting,
                &fixture.room_id,
                &fixture.token,
            )
            .await
    );

    assert!(matches!(
        recv(&mut fixture.current_rx).await.as_ref(),
        ServerMessage::Reconnected(_)
    ));
    match recv(&mut fixture.current_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => {
            assert_eq!(plan.topology, Topology::Mesh);
            assert!(
                plan.peers.is_empty(),
                "retry must rebuild against the routed actor-only membership"
            );
        }
        other => panic!("actor expected rebuilt SessionPlan, got {other:?}"),
    }
    assert!(matches!(
        recv(&mut fixture.existing_rx).await.as_ref(),
        ServerMessage::Pong
    ));
    assert_silent(&mut fixture.existing_rx).await;
    let routed = server
        .message_coordinator
        .routed_player_ids(&fixture.room_id)
        .await
        .expect("routing lookup")
        .expect("actor remains routed");
    assert_eq!(routed, vec![fixture.reconnecting]);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reconnect_transaction_failure_emits_terminal_boundary() {
    let server = create_test_server_with_session(mesh_session_config()).await;
    let mut fixture = setup_mesh_reconnect(&server, 16, 16, true).await;
    server
        .message_coordinator
        .fail_room_transactions_for_test(true);

    assert!(
        server
            .handle_reconnect(
                &fixture.current,
                &fixture.reconnecting,
                &fixture.room_id,
                &fixture.token,
            )
            .await,
        "restored baseline remains observable before publication fails closed"
    );
    server
        .message_coordinator
        .fail_room_transactions_for_test(false);

    let next_token = match recv(&mut fixture.current_rx).await.as_ref() {
        ServerMessage::Reconnected(payload) => payload
            .reconnection_token
            .clone()
            .expect("restored v3 actor receives its next reconnect token"),
        other => panic!("expected restored baseline, got {other:?}"),
    };
    assert!(matches!(
        recv(&mut fixture.existing_rx).await.as_ref(),
        ServerMessage::PlayerReconnected { player_id, .. }
            if *player_id == fixture.reconnecting
    ));
    assert!(matches!(
        recv(&mut fixture.existing_rx).await.as_ref(),
        ServerMessage::PlayerLeft { player_id, epoch: Some(_), final_seq: Some(_) }
            if *player_id == fixture.reconnecting
    ));
    assert!(
        !server.connection_manager.has_client(&fixture.reconnecting),
        "reconnect publication failure explicitly unregisters detached test connections"
    );
    assert_eq!(server.get_client_room(&fixture.reconnecting).await, None);
    let disconnected = server
        .reconnection_manager()
        .expect("reconnection enabled")
        .validate_reconnection(&fixture.reconnecting, &fixture.room_id, &next_token)
        .await
        .expect("the newly advertised reconnect token remains claimable");
    assert_eq!(disconnected.player_id, fixture.reconnecting);
    assert!(disconnected.player_info.is_some());
    let terminated = fixture.current_rx.try_recv();
    assert!(matches!(
        terminated,
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn missing_reconnect_publication_snapshot_preserves_open_then_terminal_order() {
    let server = create_test_server_with_session(mesh_session_config()).await;
    let mut fixture = setup_mesh_reconnect(&server, 16, 16, true).await;
    server.fail_retain_room_publication_snapshot_for_test(true);

    assert!(
        server
            .handle_reconnect(
                &fixture.current,
                &fixture.reconnecting,
                &fixture.room_id,
                &fixture.token,
            )
            .await
    );
    server.fail_retain_room_publication_snapshot_for_test(false);

    assert!(matches!(
        recv(&mut fixture.current_rx).await.as_ref(),
        ServerMessage::Reconnected(payload)
            if payload.lobby_state == LobbyState::Finalized
    ));
    let opening_epoch = match recv(&mut fixture.existing_rx).await.as_ref() {
        ServerMessage::PlayerReconnected {
            player_id,
            epoch: Some(epoch),
        } if *player_id == fixture.reconnecting => *epoch,
        other => panic!("incumbent expected PlayerReconnected, got {other:?}"),
    };
    match recv(&mut fixture.existing_rx).await.as_ref() {
        ServerMessage::PlayerLeft {
            player_id,
            epoch: Some(epoch),
            final_seq: Some(_),
        } => {
            assert_eq!(*player_id, fixture.reconnecting);
            assert_eq!(*epoch, opening_epoch);
        }
        other => panic!("incumbent expected matching PlayerLeft, got {other:?}"),
    }
    assert!(!server.connection_manager.has_client(&fixture.reconnecting));
    let terminal_channel = timeout(Duration::from_secs(1), fixture.current_rx.recv())
        .await
        .expect("terminated reconnect channel closes promptly");
    assert!(terminal_channel.is_none());
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn closed_actor_before_commit_still_publishes_canonical_lifecycle() {
    let server = create_test_server_with_session(mesh_session_config()).await;
    let mut fixture = setup_mesh_reconnect(&server, 16, 1, true).await;
    server.set_client_protocol(&fixture.existing, v3_webrtc_host());
    server.set_client_protocol(&fixture.current, v3_webrtc_host());
    store_active_plan(
        &server,
        fixture.room_id,
        Topology::Host,
        Transport::WebRtc,
        Some(fixture.reconnecting),
    );
    let replans_before = server
        .metrics
        .session_replans_emitted
        .load(Ordering::Relaxed);
    let replay = server.reconnection_manager().expect("reconnection enabled");
    replay
        .register_disconnection(PlayerId::new_v4(), fixture.room_id, false, None, 0)
        .await;
    let reconnect = {
        let server = Arc::clone(&server);
        let current = fixture.current;
        let reconnecting = fixture.reconnecting;
        let room_id = fixture.room_id;
        let token = fixture.token.clone();
        tokio::spawn(async move {
            server
                .handle_reconnect(&current, &reconnecting, &room_id, &token)
                .await
        })
    };
    timeout(Duration::from_secs(1), async {
        while server
            .metrics
            .websocket_backpressure_events
            .load(Ordering::Relaxed)
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actor plan reaches reservation backpressure");
    drop(fixture.current_rx);

    assert!(reconnect.await.expect("reconnect task does not panic"));
    assert!(matches!(
        recv(&mut fixture.existing_rx).await.as_ref(),
        ServerMessage::PlayerReconnected { player_id, .. }
            if *player_id == fixture.reconnecting
    ));
    assert!(matches!(
        recv(&mut fixture.existing_rx).await.as_ref(),
        ServerMessage::PlayerLeft { player_id, .. }
            if *player_id == fixture.reconnecting
    ));
    match recv(&mut fixture.existing_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => {
            assert_eq!(plan.topology, Topology::Host);
            assert_eq!(plan.host, Some(fixture.existing));
        }
        other => panic!("terminal departure must trigger incumbent replan, got {other:?}"),
    }
    let missed = replay.get_missed_events(&fixture.room_id, 0).await;
    assert_eq!(
        missed
            .events
            .iter()
            .filter(|event| matches!(
                event,
                ServerMessage::PlayerReconnected { player_id, .. }
                    if *player_id == fixture.reconnecting
            ))
            .count(),
        1,
        "fallback records the opening lifecycle boundary exactly once"
    );
    assert_eq!(
        missed
            .events
            .iter()
            .filter(|event| matches!(
                event,
                ServerMessage::PlayerLeft { player_id, .. }
                    if *player_id == fixture.reconnecting
            ))
            .count(),
        1,
        "automatic teardown records the matching terminal boundary exactly once"
    );
    assert_eq!(
        server
            .active_session_plan(&fixture.room_id)
            .expect("replan stores the replacement host")
            .host,
        Some(fixture.existing)
    );
    assert_eq!(
        server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        replans_before + 1
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn actor_close_after_commit_does_not_suppress_incumbent_plan_phase() {
    let server = create_test_server_with_session(mesh_session_config()).await;
    let mut fixture = setup_mesh_reconnect(&server, 16, 16, true).await;
    server.connection_manager.remove_client(&fixture.current);
    server
        .message_coordinator
        .unregister_local_client(&fixture.current)
        .await
        .expect("remove legacy temporary route");
    drop(fixture.current_rx);
    let (classified_tx, mut classified_rx) = crate::coordination::outbound_queue::channel(1, 2);
    fixture.current = server
        .connection_manager
        .register_classified_client(
            crate::coordination::DeliverySender::classified(classified_tx),
            crate::coordination::ConnectionCloseSignal::detached(),
            next_addr(),
            server.instance_id,
        )
        .await
        .expect("register classified reconnect actor");
    server.set_client_protocol(&fixture.current, v3_webrtc());
    let replay = server.reconnection_manager().expect("reconnection enabled");
    replay
        .register_disconnection(PlayerId::new_v4(), fixture.room_id, false, None, 0)
        .await;
    replay.pause_record_room_event_for_test();
    let reconnect = {
        let server = Arc::clone(&server);
        let current = fixture.current;
        let reconnecting = fixture.reconnecting;
        let room_id = fixture.room_id;
        let token = fixture.token.clone();
        tokio::spawn(async move {
            server
                .handle_reconnect(&current, &reconnecting, &room_id, &token)
                .await
        })
    };
    timeout(
        Duration::from_secs(1),
        replay.wait_for_record_room_event_for_test(),
    )
    .await
    .expect("transaction reaches the final replay hook after reservations");
    classified_rx.close();
    replay.release_record_room_event_for_test();

    assert!(reconnect.await.expect("reconnect task does not panic"));
    assert!(matches!(
        recv(&mut fixture.existing_rx).await.as_ref(),
        ServerMessage::PlayerReconnected { player_id, .. }
            if *player_id == fixture.reconnecting
    ));
    assert!(matches!(
        recv(&mut fixture.existing_rx).await.as_ref(),
        ServerMessage::SessionPlan(_)
    ));
    assert_eq!(
        replay
            .get_missed_events(&fixture.room_id, 0)
            .await
            .events
            .iter()
            .filter(|event| matches!(event, ServerMessage::PlayerReconnected { .. }))
            .count(),
        1
    );
    assert!(
        server
            .metrics
            .websocket_deliveries_channel_closed
            .load(Ordering::Relaxed)
            >= 1,
        "the actor plan is the degraded post-hook frame"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn finalized_reconnect_without_sticky_plan_emits_explicit_relay_refresh() {
    let server = create_test_server().await;
    let mut fixture = setup_mesh_reconnect(&server, 16, 16, true).await;
    server.active_session_plans.remove(&fixture.room_id);

    assert!(
        server
            .handle_reconnect(
                &fixture.current,
                &fixture.reconnecting,
                &fixture.room_id,
                &fixture.token,
            )
            .await
    );
    assert!(matches!(
        recv(&mut fixture.current_rx).await.as_ref(),
        ServerMessage::Reconnected(_)
    ));
    let actor_plan = match recv(&mut fixture.current_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("actor expected relay SessionPlan, got {other:?}"),
    };
    assert!(matches!(
        recv(&mut fixture.existing_rx).await.as_ref(),
        ServerMessage::PlayerReconnected { .. }
    ));
    let incumbent_plan = match recv(&mut fixture.existing_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("incumbent expected relay SessionPlan, got {other:?}"),
    };
    for plan in [actor_plan, incumbent_plan] {
        assert_eq!(plan.topology, Topology::Relay);
        assert_eq!(plan.transport, Transport::Relay);
        assert!(plan.peers.is_empty());
        assert!(plan.ice_servers.is_empty());
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn waiting_reconnect_publishes_lifecycle_without_session_plan() {
    let server = create_test_server_with_session(mesh_session_config()).await;
    let mut fixture = setup_mesh_reconnect(&server, 16, 16, false).await;
    let late_before = server
        .metrics
        .session_plans_late_join
        .load(Ordering::Relaxed);

    assert!(
        server
            .handle_reconnect(
                &fixture.current,
                &fixture.reconnecting,
                &fixture.room_id,
                &fixture.token,
            )
            .await
    );
    match recv(&mut fixture.current_rx).await.as_ref() {
        ServerMessage::Reconnected(payload) => {
            assert_eq!(payload.lobby_state, LobbyState::Waiting)
        }
        other => panic!("actor expected Reconnected, got {other:?}"),
    }
    assert!(matches!(
        recv(&mut fixture.existing_rx).await.as_ref(),
        ServerMessage::PlayerReconnected { .. }
    ));
    assert_silent(&mut fixture.current_rx).await;
    assert_silent(&mut fixture.existing_rx).await;
    assert_eq!(
        server
            .metrics
            .session_plans_late_join
            .load(Ordering::Relaxed),
        late_before,
        "pre-finalized reconnects do not emit or count SessionPlan"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reconnect_room_full_failure_releases_claim_for_retry() {
    let server = create_test_server().await;
    let (existing, _existing_rx) = register_client(&server).await;
    let (reconnecting, _old_rx) = register_client(&server).await;
    let (filler, _filler_rx) = register_client(&server).await;
    let (current, mut current_rx) = register_client(&server).await;
    server.set_client_protocol(&existing, v3_webrtc());
    server.set_client_protocol(&current, v3_webrtc());

    let room_id = create_db_room_with_max(&server, existing, 2).await;
    let reconnecting_info = player_info(reconnecting, "reconnecting");
    server
        .database
        .add_player_to_room(&room_id, reconnecting_info.clone())
        .await
        .expect("add reconnecting player");
    server
        .connection_manager
        .assign_client_to_room(&existing, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&reconnecting, room_id)
        .await;

    let token = server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(
            reconnecting,
            room_id,
            false,
            Some(reconnecting_info),
            // Mirror production: capture the connection's real pre-disconnect
            // incarnation epoch (non-zero once it has joined a room), so the
            // reconnect resumes at `epoch + 1` instead of colliding at 1.
            server
                .connection_manager
                .game_data_epoch(&reconnecting)
                .unwrap_or(0),
        )
        .await;
    server
        .database
        .remove_player_from_room(&room_id, &reconnecting)
        .await
        .expect("remove reconnecting player");
    server.connection_manager.remove_client(&reconnecting);
    let _ = server
        .message_coordinator
        .unregister_local_client(&reconnecting)
        .await;

    server
        .database
        .add_player_to_room(&room_id, player_info(filler, "filler"))
        .await
        .expect("add filler player");
    server
        .connection_manager
        .assign_client_to_room(&filler, room_id)
        .await;

    let first_attempt = server
        .handle_reconnect(&current, &reconnecting, &room_id, &token)
        .await;
    assert!(!first_attempt, "full room reconnect attempt must fail");
    match recv(&mut current_rx).await.as_ref() {
        ServerMessage::ReconnectionFailed { error_code, .. } => {
            assert_eq!(*error_code, ErrorCode::RoomFull);
        }
        other => panic!("expected ReconnectionFailed(RoomFull), got {other:?}"),
    }
    server
        .reconnection_manager()
        .expect("reconnection enabled")
        .validate_reconnection(&reconnecting, &room_id, &token)
        .await
        .expect("failed room-full attempt must release claim for retry");

    server
        .database
        .remove_player_from_room(&room_id, &filler)
        .await
        .expect("remove filler player");
    server.connection_manager.clear_room_assignment(&filler);

    let second_attempt = server
        .handle_reconnect(&current, &reconnecting, &room_id, &token)
        .await;
    assert!(
        second_attempt,
        "same token should succeed after room has space"
    );
    match recv(&mut current_rx).await.as_ref() {
        ServerMessage::Reconnected(payload) => {
            assert_eq!(payload.player_id, reconnecting);
        }
        other => panic!("expected Reconnected after retry, got {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reconnect_during_teardown_preserves_token_for_retry() {
    let server = create_test_server().await;
    let (existing, _existing_rx) = register_client(&server).await;
    let (reconnecting, _old_rx) = register_client(&server).await;
    let (current, mut current_rx) = register_client(&server).await;

    let room_id = create_db_room(&server, existing).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(reconnecting, "reconnecting"))
        .await
        .expect("add reconnecting player");
    for player_id in [existing, reconnecting] {
        server
            .connection_manager
            .assign_client_to_room(&player_id, room_id)
            .await;
    }

    // Production v3 joins pre-issue the token that disconnect later arms. Seed
    // that same state directly so the test knows the exact wire token before
    // opening the deterministic teardown gate.
    let manager = server.reconnection_manager().expect("reconnection enabled");
    let token = manager.pre_issue_token(reconnecting, room_id).await;
    let gate = server.install_reconnect_teardown_test_gate();
    let teardown = {
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            server.disconnect_client(&reconnecting).await;
        })
    };

    timeout(Duration::from_secs(5), gate.wait_until_armed())
        .await
        .expect("disconnect must arm the reconnect record before the deterministic gate");
    assert!(
        manager.has_pending_reconnection(&reconnecting).await,
        "the gated teardown must have armed a pending reconnect record"
    );
    assert!(
        server.connection_manager.has_client(&reconnecting),
        "the old connection must still be registered at the teardown boundary"
    );

    let first_attempt = server
        .handle_reconnect(&current, &reconnecting, &room_id, &token)
        .await;
    assert!(
        !first_attempt,
        "reconnect during teardown must reject while the old connection is registered"
    );
    match recv(&mut current_rx).await.as_ref() {
        ServerMessage::ReconnectionFailed { error_code, .. } => {
            assert_eq!(*error_code, ErrorCode::PlayerAlreadyConnected);
        }
        other => panic!("expected PlayerAlreadyConnected during teardown, got {other:?}"),
    }
    manager
        .validate_reconnection(&reconnecting, &room_id, &token)
        .await
        .expect("PlayerAlreadyConnected must leave the reconnect token valid");

    gate.release();
    timeout(Duration::from_secs(5), teardown)
        .await
        .expect("released teardown must complete")
        .expect("teardown task must not panic");
    assert!(
        !server.connection_manager.has_client(&reconnecting),
        "released teardown must remove the old connection"
    );
    assert!(
        manager.has_pending_reconnection(&reconnecting).await,
        "completed teardown must leave the unconsumed record pending"
    );

    let second_attempt = server
        .handle_reconnect(&current, &reconnecting, &room_id, &token)
        .await;
    assert!(
        second_attempt,
        "the same token must succeed after teardown completes"
    );
    match recv(&mut current_rx).await.as_ref() {
        ServerMessage::Reconnected(payload) => {
            assert_eq!(payload.player_id, reconnecting);
            assert_eq!(payload.room_id, room_id);
        }
        other => panic!("expected Reconnected after teardown retry, got {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reconnect_reassign_failure_rolls_back_membership_and_releases_claim() {
    let server = create_test_server().await;
    let (existing, _existing_rx) = register_client(&server).await;
    let (reconnecting, _old_rx) = register_client(&server).await;
    let (current, _current_rx) = register_client(&server).await;

    let room_id = create_db_room(&server, existing).await;
    let reconnecting_info = player_info(reconnecting, "reconnecting");
    server
        .database
        .add_player_to_room(&room_id, reconnecting_info.clone())
        .await
        .expect("add reconnecting player");
    server
        .connection_manager
        .assign_client_to_room(&existing, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&reconnecting, room_id)
        .await;

    let token = server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(
            reconnecting,
            room_id,
            false,
            Some(reconnecting_info),
            // Mirror production: capture the connection's real pre-disconnect
            // incarnation epoch (non-zero once it has joined a room), so the
            // reconnect resumes at `epoch + 1` instead of colliding at 1.
            server
                .connection_manager
                .game_data_epoch(&reconnecting)
                .unwrap_or(0),
        )
        .await;
    server
        .database
        .remove_player_from_room(&room_id, &reconnecting)
        .await
        .expect("remove reconnecting player");
    server.connection_manager.remove_client(&reconnecting);
    let _ = server
        .message_coordinator
        .unregister_local_client(&reconnecting)
        .await;

    server.connection_manager.remove_client(&current);
    let first_attempt = server
        .handle_reconnect(&current, &reconnecting, &room_id, &token)
        .await;
    assert!(
        !first_attempt,
        "reconnect should fail when temporary connection disappears"
    );

    server
        .reconnection_manager()
        .expect("reconnection enabled")
        .validate_reconnection(&reconnecting, &room_id, &token)
        .await
        .expect("reassign failure must release claim for retry");
    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    assert!(
        !members.iter().any(|player| player.id == reconnecting),
        "failed reassign must roll back restored room membership"
    );

    let (replacement, mut replacement_rx) = register_client(&server).await;
    let second_attempt = server
        .handle_reconnect(&replacement, &reconnecting, &room_id, &token)
        .await;
    assert!(second_attempt, "same token should retry successfully");
    match recv(&mut replacement_rx).await.as_ref() {
        ServerMessage::Reconnected(payload) => {
            assert_eq!(payload.player_id, reconnecting);
        }
        other => panic!("expected Reconnected after retry, got {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reconnect_baseline_delivery_failure_rolls_back_and_releases_claim_for_retry() {
    let server = create_test_server_with_config(ServerConfig {
        websocket_config: WebSocketConfig {
            slow_consumer_timeout_ms: 1,
            ..WebSocketConfig::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let (existing, _existing_rx) = register_client(&server).await;
    let (reconnecting, _old_rx) = register_client(&server).await;
    let (current, mut current_rx) = register_client_with_queue_capacity(&server, 1).await;
    server.set_client_protocol(&current, v3_webrtc());

    let room_id = create_db_room(&server, existing).await;
    let reconnecting_info = player_info(reconnecting, "reconnecting");
    server
        .database
        .add_player_to_room(&room_id, reconnecting_info.clone())
        .await
        .expect("add reconnecting player");
    server
        .connection_manager
        .assign_client_to_room(&existing, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&reconnecting, room_id)
        .await;

    let token = server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(
            reconnecting,
            room_id,
            false,
            Some(reconnecting_info),
            server
                .connection_manager
                .game_data_epoch(&reconnecting)
                .unwrap_or(0),
        )
        .await;
    server
        .database
        .remove_player_from_room(&room_id, &reconnecting)
        .await
        .expect("remove reconnecting player");
    server.connection_manager.remove_client(&reconnecting);
    let _ = server
        .message_coordinator
        .unregister_local_client(&reconnecting)
        .await;

    assert!(
        server
            .message_coordinator
            .try_send_to_player(&current, Arc::new(ServerMessage::Pong))
            .await
            .expect("prefill current queue"),
        "test setup must fill the one-slot reconnect response queue"
    );

    let first_attempt = server
        .handle_reconnect(&current, &reconnecting, &room_id, &token)
        .await;
    assert!(
        !first_attempt,
        "reconnect must fail when the Reconnected baseline is not queued"
    );
    assert!(
        server.connection_manager.has_client(&current),
        "failed baseline delivery must restore the temporary connection id"
    );
    assert!(
        !server.connection_manager.has_client(&reconnecting),
        "failed baseline delivery must not leave the restored id registered"
    );
    server
        .reconnection_manager()
        .expect("reconnection enabled")
        .validate_reconnection(&reconnecting, &room_id, &token)
        .await
        .expect("baseline delivery failure must release claim for retry");
    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    assert!(
        !members.iter().any(|player| player.id == reconnecting),
        "failed baseline delivery must roll back restored room membership"
    );
    match recv(&mut current_rx).await.as_ref() {
        ServerMessage::Pong => {}
        other => panic!("expected the prefilled Pong, got {other:?}"),
    }

    let (replacement, mut replacement_rx) = register_client(&server).await;
    server.set_client_protocol(&replacement, v3_webrtc());
    let second_attempt = server
        .handle_reconnect(&replacement, &reconnecting, &room_id, &token)
        .await;
    assert!(
        second_attempt,
        "same token should retry successfully after baseline delivery failure"
    );
    match recv(&mut replacement_rx).await.as_ref() {
        ServerMessage::Reconnected(payload) => {
            assert_eq!(payload.player_id, reconnecting);
        }
        other => panic!("expected Reconnected after retry, got {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reconnect_from_roomed_temporary_connection_is_rejected_without_ghost_membership() {
    let server = create_test_server().await;
    let (existing, _existing_rx) = register_client(&server).await;
    let (reconnecting, _old_rx) = register_client(&server).await;
    let (current, mut current_rx) = register_client(&server).await;

    let target_room_id = create_db_room(&server, existing).await;
    let reconnecting_info = player_info(reconnecting, "reconnecting");
    server
        .database
        .add_player_to_room(&target_room_id, reconnecting_info.clone())
        .await
        .expect("add reconnecting player");
    server
        .connection_manager
        .assign_client_to_room(&reconnecting, target_room_id)
        .await;

    let token = server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(
            reconnecting,
            target_room_id,
            false,
            Some(reconnecting_info),
            // Mirror production: pass the real pre-disconnect incarnation epoch
            // (non-zero after the room assignment above), not 0.
            server
                .connection_manager
                .game_data_epoch(&reconnecting)
                .unwrap_or(0),
        )
        .await;
    server
        .database
        .remove_player_from_room(&target_room_id, &reconnecting)
        .await
        .expect("remove reconnecting player");
    server.connection_manager.remove_client(&reconnecting);
    let _ = server
        .message_coordinator
        .unregister_local_client(&reconnecting)
        .await;

    let current_room_id = create_db_room(&server, current).await;
    server
        .connection_manager
        .assign_client_to_room(&current, current_room_id)
        .await;

    let reconnected = server
        .handle_reconnect(&current, &reconnecting, &target_room_id, &token)
        .await;
    assert!(
        !reconnected,
        "reconnect from an already-roomed temporary client must fail"
    );

    match recv(&mut current_rx).await.as_ref() {
        ServerMessage::ReconnectionFailed { error_code, .. } => {
            assert_eq!(*error_code, ErrorCode::ReconnectionFailed);
        }
        other => panic!("expected ReconnectionFailed, got {other:?}"),
    }

    let current_room_members = server
        .database
        .get_room_players(&current_room_id)
        .await
        .expect("current room players");
    assert!(
        current_room_members
            .iter()
            .any(|player| player.id == current),
        "failed reconnect must not orphan the temporary player's existing room membership"
    );
    assert!(
        server.connection_manager.has_client(&current),
        "failed reconnect must leave the temporary connection registered"
    );
    assert!(
        !server.connection_manager.has_client(&reconnecting),
        "failed reconnect must not claim the disconnected player's id"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn concurrent_reconnect_attempts_with_same_token_allow_exactly_one_winner() {
    let server = create_test_server().await;
    let (existing, _existing_rx) = register_client(&server).await;
    let (reconnecting, _old_rx) = register_client(&server).await;
    let (current_a, _current_a_rx) = register_client(&server).await;
    let (current_b, _current_b_rx) = register_client(&server).await;

    let room_id = create_db_room(&server, existing).await;
    let reconnecting_info = player_info(reconnecting, "reconnecting");
    server
        .database
        .add_player_to_room(&room_id, reconnecting_info.clone())
        .await
        .expect("add reconnecting player");
    server
        .connection_manager
        .assign_client_to_room(&reconnecting, room_id)
        .await;

    let token = server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(
            reconnecting,
            room_id,
            false,
            Some(reconnecting_info),
            // Mirror production: capture the connection's real pre-disconnect
            // incarnation epoch (non-zero once it has joined a room), so the
            // reconnect resumes at `epoch + 1` instead of colliding at 1.
            server
                .connection_manager
                .game_data_epoch(&reconnecting)
                .unwrap_or(0),
        )
        .await;
    server
        .database
        .remove_player_from_room(&room_id, &reconnecting)
        .await
        .expect("remove reconnecting player");
    server.connection_manager.remove_client(&reconnecting);
    let _ = server
        .message_coordinator
        .unregister_local_client(&reconnecting)
        .await;

    let server_a = Arc::clone(&server);
    let server_b = Arc::clone(&server);
    let token_a = token.clone();
    let token_b = token.clone();
    let reconnect_a = async move {
        server_a
            .handle_reconnect(&current_a, &reconnecting, &room_id, &token_a)
            .await
    };
    let reconnect_b = async move {
        server_b
            .handle_reconnect(&current_b, &reconnecting, &room_id, &token_b)
            .await
    };

    let (success_a, success_b) = tokio::join!(reconnect_a, reconnect_b);
    let success_count = [success_a, success_b]
        .into_iter()
        .filter(|success| *success)
        .count();
    assert_eq!(
        success_count, 1,
        "exactly one concurrent reconnect may claim a single-use token"
    );
    assert!(
        server.connection_manager.has_client(&reconnecting),
        "the winning reconnect should own the restored player id"
    );
    let remaining_temporary_connections = [current_a, current_b]
        .into_iter()
        .filter(|player_id| server.connection_manager.has_client(player_id))
        .count();
    assert_eq!(
        remaining_temporary_connections, 1,
        "the losing fresh connection should remain temporary; the winner should be reassigned"
    );

    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    assert_eq!(
        members
            .iter()
            .filter(|player| player.id == reconnecting)
            .count(),
        1,
        "same-token concurrent reconnects must not duplicate room membership"
    );
}

// ---------------------------------------------------------------------------
// signals_relayed metric (P5).
// ---------------------------------------------------------------------------

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn handle_signal_increments_signals_relayed_on_accepted_dispatch() {
    let server = create_test_server().await;
    let (alice, _alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&bob, room_id)
        .await;

    assert_eq!(
        server.metrics.signals_relayed.load(Ordering::Relaxed),
        0,
        "no signals relayed before any handle_signal call"
    );

    server
        .handle_signal(&alice, bob, json!({ "Offer": "x" }))
        .await;

    // Sanity: the best-effort dispatch reached this receiver in the available-channel case.
    match recv(&mut bob_rx).await.as_ref() {
        ServerMessage::Signal { from, .. } => assert_eq!(*from, alice),
        other => panic!("expected Signal, got {other:?}"),
    }
    assert_eq!(
        server.metrics.signals_relayed.load(Ordering::Relaxed),
        1,
        "an accepted dispatch must increment signals_relayed exactly once"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn handle_signal_counts_valid_dispatch_when_receiver_is_closed() {
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&bob, room_id)
        .await;

    drop(bob_rx);

    server
        .handle_signal(&alice, bob, json!({ "Offer": "x" }))
        .await;

    assert_silent(&mut alice_rx).await;
    assert_eq!(
        server.metrics.signals_relayed.load(Ordering::Relaxed),
        1,
        "signals_relayed counts accepted best-effort dispatch, not receiver delivery"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn handle_signal_does_not_count_rejected_cross_room_signal() {
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    // Alice and Bob are in DIFFERENT rooms => cross-room signal is rejected.
    server
        .connection_manager
        .assign_client_to_room(&alice, uuid::Uuid::new_v4())
        .await;
    server
        .connection_manager
        .assign_client_to_room(&bob, uuid::Uuid::new_v4())
        .await;

    server
        .handle_signal(&alice, bob, json!({ "Offer": "x" }))
        .await;

    // Alice gets a rejection error; Bob never receives the signal.
    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::CrossRoomSignal));
    assert_silent(&mut bob_rx).await;

    assert_eq!(
        server.metrics.signals_relayed.load(Ordering::Relaxed),
        0,
        "a rejected cross-room signal must NOT count as relayed"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn handle_signal_does_not_count_rate_limited_signal() {
    // A 0-budget signal limiter rejects every relay attempt; none may count.
    let server = create_test_server_with_signals(0).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&bob, room_id)
        .await;

    server
        .handle_signal(&alice, bob, json!({ "Offer": "x" }))
        .await;

    let msg = recv(&mut alice_rx).await;
    assert_eq!(error_code(&msg), Some(ErrorCode::SignalRateLimited));
    assert_silent(&mut bob_rx).await;
    assert_eq!(
        server.metrics.signals_relayed.load(Ordering::Relaxed),
        0,
        "a rate-limited signal must NOT count as relayed"
    );
}

// ---------------------------------------------------------------------------
// TransportStatus handler (P5): per-connection state + p2p/relay metrics, v3 gating.
// ---------------------------------------------------------------------------

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_webrtc_connected_records_p2p_and_state() {
    let server = create_test_server().await;
    let (alice, _alice_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: true,
            },
        )
        .await;

    assert_eq!(
        server.client_transport_status(&alice),
        Some((Transport::WebRtc, true)),
        "the per-connection transport status must be recorded"
    );
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        1,
        "webrtc + connected must record one p2p_established"
    );
    assert_eq!(
        server.metrics.relay_fallback.load(Ordering::Relaxed),
        0,
        "a connected p2p report must not count as relay_fallback"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_direct_connected_records_p2p() {
    let server = create_test_server().await;
    let (alice, _alice_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc_direct_host());

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::Direct,
                connected: true,
            },
        )
        .await;

    assert_eq!(
        server.client_transport_status(&alice),
        Some((Transport::Direct, true))
    );
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        1,
        "direct + connected is a P2P establishment"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_disconnected_records_relay_fallback() {
    let server = create_test_server().await;
    let (alice, _alice_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: false,
            },
        )
        .await;

    assert_eq!(
        server.client_transport_status(&alice),
        Some((Transport::WebRtc, false))
    );
    assert_eq!(
        server.metrics.relay_fallback.load(Ordering::Relaxed),
        1,
        "connected=false must record one relay_fallback"
    );
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        0,
        "a fallback report must not count as p2p_established"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_relay_connected_moves_no_counter_but_records_state() {
    // `connected: true` with `transport: relay` means "still on the floor": it is
    // neither a P2P establishment nor a fallback, so it moves no metric — only the
    // per-connection state is updated.
    let server = create_test_server().await;
    let (alice, _alice_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::Relay,
                connected: true,
            },
        )
        .await;

    assert_eq!(
        server.client_transport_status(&alice),
        Some((Transport::Relay, true)),
        "the report is still recorded as per-connection state"
    );
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        0,
        "relay + connected is not a P2P establishment"
    );
    assert_eq!(
        server.metrics.relay_fallback.load(Ordering::Relaxed),
        0,
        "relay + connected is not a fallback event"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_from_non_v3_client_is_ignored() {
    // A v2 client can never legitimately send TransportStatus; the report must be
    // dropped — no per-connection state, no metric movement (defense-in-depth).
    let server = create_test_server().await;
    let (legacy, mut legacy_rx) = register_client(&server).await;
    server.set_client_protocol(&legacy, v2_with_webrtc_transport());

    server
        .handle_client_message(
            &legacy,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: true,
            },
        )
        .await;

    // The ignored report must also leak no message back to the v2 client.
    assert_silent(&mut legacy_rx).await;

    assert_eq!(
        server.client_transport_status(&legacy),
        None,
        "a non-v3 client's TransportStatus must NOT update per-connection state"
    );
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        0,
        "a non-v3 client's TransportStatus must not move p2p_established"
    );
    assert_eq!(
        server.metrics.relay_fallback.load(Ordering::Relaxed),
        0,
        "a non-v3 client's TransportStatus must not move relay_fallback"
    );
}

// ---------------------------------------------------------------------------
// PeerTransportStatus fan-out (P5 refinement): an accepted TransportStatus
// state change is fanned out to the sender's current room — v3 recipients only,
// sender excluded, duplicates never re-fan-out, no room ⇒ no fan-out.
// ---------------------------------------------------------------------------

/// Assert the next message is `PeerTransportStatus` with the given contents.
async fn expect_peer_transport_status(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    expected_peer: PlayerId,
    expected_transport: Transport,
    expected_connected: bool,
) {
    match recv(receiver).await.as_ref() {
        ServerMessage::PeerTransportStatus {
            peer_id,
            transport,
            connected,
        } => {
            assert_eq!(*peer_id, expected_peer, "fan-out must name the reporter");
            assert_eq!(*transport, expected_transport);
            assert_eq!(*connected, expected_connected);
        }
        other => panic!("expected PeerTransportStatus, got {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_change_fans_out_to_v3_room_peers_only() {
    // Alice (v3 + webrtc) reports a state change in a room with Bob (v3
    // RELAY-ONLY — must still receive: the fan-out is deliberately NOT gated on
    // the recipient's transport capabilities, unlike the session predicate) and
    // Carol (v2 — must NEVER receive a v3-only message, Appendix K). The
    // reporter itself hears nothing.
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    let (carol, mut carol_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_relay_only());
    server.set_client_protocol(&carol, v2_with_webrtc_transport());

    let room_id = create_db_room(&server, alice).await;
    for (id, name) in [(bob, "bob"), (carol, "carol")] {
        server
            .database
            .add_player_to_room(&room_id, player_info(id, name))
            .await
            .expect("add member");
    }
    for id in [alice, bob, carol] {
        server
            .connection_manager
            .assign_client_to_room(&id, room_id)
            .await;
    }

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: true,
            },
        )
        .await;

    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, true).await;
    assert_silent(&mut bob_rx).await;
    // The v2 member observes nothing, and the sender is excluded.
    assert_silent(&mut carol_rx).await;
    assert_silent(&mut alice_rx).await;
    assert_eq!(
        server
            .metrics
            .transport_status_fanout
            .load(Ordering::Relaxed),
        1,
        "one fan-out EVENT (not per recipient)"
    );
}

#[tokio::test(start_paused = true)]
async fn transport_status_slow_peers_share_one_timeout_window() {
    let server = create_test_server_with_config(ServerConfig {
        websocket_config: WebSocketConfig {
            slow_consumer_timeout_ms: 5_000,
            ..WebSocketConfig::default()
        },
        ..ServerConfig::default()
    })
    .await;
    let (reporter, _reporter_rx) = register_client(&server).await;
    let (slow_a, _slow_a_rx) = register_client_with_queue_capacity(&server, 1).await;
    let (slow_b, _slow_b_rx) = register_client_with_queue_capacity(&server, 1).await;
    for player in [reporter, slow_a, slow_b] {
        server.set_client_protocol(&player, v3_webrtc());
    }
    let room_id = create_db_room_with_max(&server, reporter, 3).await;
    for (player, name) in [(slow_a, "slow-a"), (slow_b, "slow-b")] {
        server
            .database
            .add_player_to_room(&room_id, player_info(player, name))
            .await
            .expect("add slow member");
    }
    for player in [reporter, slow_a, slow_b] {
        server
            .connection_manager
            .assign_client_to_room(&player, room_id)
            .await;
    }
    for player in [slow_a, slow_b] {
        server
            .message_coordinator
            .send_to_player(&player, Arc::new(ServerMessage::Pong))
            .await
            .expect("fill slow peer queue");
    }

    let fanout = {
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            server
                .handle_client_message(
                    &reporter,
                    ClientMessage::TransportStatus {
                        transport: Transport::WebRtc,
                        connected: true,
                    },
                )
                .await;
        })
    };
    for _ in 0..10_000 {
        if server
            .metrics
            .websocket_backpressure_events
            .load(Ordering::Relaxed)
            == 2
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        server
            .metrics
            .websocket_backpressure_events
            .load(Ordering::Relaxed),
        2
    );
    tokio::time::advance(Duration::from_millis(4_999)).await;
    assert_eq!(
        server
            .metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        0
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    for _ in 0..10_000 {
        if server
            .metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed)
            == 2
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        server
            .metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        2,
        "both fan-out waits expire in the same timeout window"
    );
    fanout.await.expect("fan-out task must not panic");
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn duplicate_transport_status_does_not_refan_out() {
    let server = create_test_server().await;
    let (alice, _alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = create_db_room(&server, alice).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(bob, "bob"))
        .await
        .expect("add bob");
    for id in [alice, bob] {
        server
            .connection_manager
            .assign_client_to_room(&id, room_id)
            .await;
    }

    let report = ClientMessage::TransportStatus {
        transport: Transport::WebRtc,
        connected: true,
    };
    // First report IS a state change and fans out…
    server.handle_client_message(&alice, report.clone()).await;
    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, true).await;

    // …the byte-identical duplicate is dropped at the dedup gate: no fan-out,
    // no counter movement.
    server.handle_client_message(&alice, report).await;
    assert_silent(&mut bob_rx).await;
    assert_eq!(
        server
            .metrics
            .transport_status_fanout
            .load(Ordering::Relaxed),
        1,
        "a duplicate report must not re-fan-out"
    );
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        1,
        "the duplicate must not inflate p2p_established either"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_flap_fans_out_each_transition() {
    // true ⇒ false ⇒ true: every report is a real transition, so the peer sees
    // all three states in order (eventually-consistent peer view of a flapping
    // data path).
    let server = create_test_server().await;
    let (alice, _alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = create_db_room(&server, alice).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(bob, "bob"))
        .await
        .expect("add bob");
    for id in [alice, bob] {
        server
            .connection_manager
            .assign_client_to_room(&id, room_id)
            .await;
    }

    for connected in [true, false, true] {
        server
            .handle_client_message(
                &alice,
                ClientMessage::TransportStatus {
                    transport: Transport::WebRtc,
                    connected,
                },
            )
            .await;
    }

    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, true).await;
    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, false).await;
    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, true).await;
    assert_silent(&mut bob_rx).await;
    assert_eq!(
        server
            .metrics
            .transport_status_fanout
            .load(Ordering::Relaxed),
        3,
        "each real transition is one fan-out event"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_fanout_is_bounded_by_signal_budget() {
    // `TransportStatus` is v3 WebRTC control-plane traffic, and an accepted state
    // change triggers a 1→N `PeerTransportStatus` fan-out to the room. A client
    // that alternates `connected` to force a `Changed` on every frame (defeating
    // the dedup gate) must not be able to use the tiny message as an unbounded
    // room amplifier: the accepted-change path consumes the same per-connection
    // budget as `Signal` (`max_signals`). Over-budget changes are dropped
    // SILENTLY (no error frame — `TransportStatus` is informational), but the
    // per-connection state is still recorded. Budget = 2 here.
    let server = create_test_server_with_signals(2).await;
    let (alice, _alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = create_db_room(&server, alice).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(bob, "bob"))
        .await
        .expect("add bob");
    for id in [alice, bob] {
        server
            .connection_manager
            .assign_client_to_room(&id, room_id)
            .await;
    }

    // Five real transitions, but only two fit the budget.
    for connected in [true, false, true, false, true] {
        server
            .handle_client_message(
                &alice,
                ClientMessage::TransportStatus {
                    transport: Transport::WebRtc,
                    connected,
                },
            )
            .await;
    }

    // Bob sees exactly the two budgeted transitions, in order, then nothing.
    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, true).await;
    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, false).await;
    assert_silent(&mut bob_rx).await;
    assert_eq!(
        server
            .metrics
            .transport_status_fanout
            .load(Ordering::Relaxed),
        2,
        "fan-out events are capped at the per-connection control-plane budget"
    );
    // Only the 1→N fan-out is budget-bounded. The O(1) local bookkeeping is NOT:
    // every accepted transition still updates the per-connection state and the
    // p2p/relay observability counters, so the budget cannot distort metrics.
    // The five transitions are [true, false, true, false, true] over WebRtc:
    // three `true` ⇒ p2p_established += 3, two `false` ⇒ relay_fallback += 2.
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        3,
        "every accepted P2P-up transition is counted, even when its fan-out is dropped"
    );
    assert_eq!(
        server.metrics.relay_fallback.load(Ordering::Relaxed),
        2,
        "every accepted fallback transition is counted, even when its fan-out is dropped"
    );
    // The latest reported state is still recorded even though its fan-out was
    // dropped (per-connection truth is never rate-limited).
    assert_eq!(
        server.client_transport_status(&alice),
        Some((Transport::WebRtc, true)),
        "the connection's own transport state tracks the last report regardless of fan-out budget"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn over_budget_transport_status_does_not_load_room_members() {
    // Once the sender is already over the fan-out budget, the handler should
    // drop the informational peer notice before taking the fallible/O(room)
    // membership snapshot. The local state/metrics still update because they
    // are O(1) bookkeeping, not fan-out work.
    let server = create_test_server_with_signals(1).await;
    let (alice, _alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = create_db_room(&server, alice).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(bob, "bob"))
        .await
        .expect("add bob");
    for id in [alice, bob] {
        server
            .connection_manager
            .assign_client_to_room(&id, room_id)
            .await;
    }

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: true,
            },
        )
        .await;
    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, true).await;
    assert_eq!(valid_signal_budget_used(&server, &alice).await, 1);

    let db = server
        .database()
        .as_any()
        .downcast_ref::<InMemoryDatabase>()
        .expect("test server uses in-memory database");
    let member_lookups_after_budgeted_fanout = db.get_room_players_calls_for_test();
    db.fail_get_room_players_for_test(true);

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: false,
            },
        )
        .await;

    assert_silent(&mut bob_rx).await;
    assert_eq!(
        db.get_room_players_calls_for_test(),
        member_lookups_after_budgeted_fanout,
        "over-budget fan-out must be dropped before loading room members"
    );
    assert_eq!(
        valid_signal_budget_used(&server, &alice).await,
        1,
        "over-budget preflight must not consume an additional slot"
    );
    assert_eq!(
        server.client_transport_status(&alice),
        Some((Transport::WebRtc, false)),
        "local transport truth still tracks the over-budget report"
    );
    assert_eq!(
        server.metrics.relay_fallback.load(Ordering::Relaxed),
        1,
        "over-budget reports still update O(1) observability"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_without_room_records_state_but_fans_out_nothing() {
    // A room-less reporter still gets its per-connection state recorded (the
    // pre-fan-out behavior is preserved), but there is no room to notify.
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    // Bob sits in a room alice is NOT in — he must hear nothing.
    let room_id = create_db_room(&server, bob).await;
    server
        .connection_manager
        .assign_client_to_room(&bob, room_id)
        .await;

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: true,
            },
        )
        .await;

    assert_eq!(
        server.client_transport_status(&alice),
        Some((Transport::WebRtc, true)),
        "the state is still recorded for a room-less reporter"
    );
    assert_silent(&mut bob_rx).await;
    assert_silent(&mut alice_rx).await;
    assert_eq!(
        server
            .metrics
            .transport_status_fanout
            .load(Ordering::Relaxed),
        0,
        "no room ⇒ no fan-out event"
    );
    assert_eq!(
        server.metrics.p2p_established.load(Ordering::Relaxed),
        1,
        "the accepted report still moves p2p_established"
    );
    // The fan-out rate-limit gate sits AFTER the no-room early return, so a
    // room-less reporter spends no signal budget on a fan-out that can't happen.
    assert_eq!(
        valid_signal_budget_used(&server, &alice).await,
        0,
        "a room-less TransportStatus must not consume the sender's signal budget"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_uses_exact_routing_when_database_lookup_fails() {
    // Production owns an exact coordinator routing snapshot, so informational
    // fan-out neither needs nor consults the fallible database membership path.
    // This keeps a transient storage outage from suppressing live control
    // traffic while preserving the same per-sender fan-out budget.
    let server = create_test_server_with_signals(1).await;
    let (alice, _alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = create_db_room(&server, alice).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(bob, "bob"))
        .await
        .expect("add bob");
    for id in [alice, bob] {
        server
            .connection_manager
            .assign_client_to_room(&id, room_id)
            .await;
    }

    let db = server
        .database()
        .as_any()
        .downcast_ref::<InMemoryDatabase>()
        .expect("test server uses in-memory database");
    db.fail_get_room_players_for_test(true);

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: true,
            },
        )
        .await;

    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, true).await;
    assert_eq!(
        valid_signal_budget_used(&server, &alice).await,
        1,
        "the routed fan-out consumes the one valid-signal budget slot"
    );
    assert_eq!(
        server
            .metrics
            .transport_status_fanout
            .load(Ordering::Relaxed),
        1,
        "exact routing makes this a successful fan-out event"
    );
    assert_eq!(
        server.client_transport_status(&alice),
        Some((Transport::WebRtc, true)),
        "the local transport state is recorded with the routed fan-out"
    );

    db.fail_get_room_players_for_test(false);
    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: false,
            },
        )
        .await;

    assert_silent(&mut bob_rx).await;
    assert_eq!(
        valid_signal_budget_used(&server, &alice).await,
        1,
        "the over-budget transition cannot consume another slot"
    );
    assert_eq!(
        server
            .metrics
            .transport_status_fanout
            .load(Ordering::Relaxed),
        1,
        "only the first routed fan-out is counted"
    );
    assert_eq!(
        server.client_transport_status(&alice),
        Some((Transport::WebRtc, false)),
        "over-budget fan-out still records the sender's latest local state"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_sender_only_room_does_not_consume_signal_budget() {
    let server = create_test_server().await;
    let (alice, mut alice_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());

    let room_id = create_db_room(&server, alice).await;
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: true,
            },
        )
        .await;

    assert_silent(&mut alice_rx).await;
    assert_eq!(
        valid_signal_budget_used(&server, &alice).await,
        0,
        "a sender-only room has no fan-out recipients to charge for"
    );
    assert_eq!(
        server
            .metrics
            .transport_status_fanout
            .load(Ordering::Relaxed),
        0,
        "a sender-only room is not a fan-out event"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn transport_status_room_with_only_v2_peers_does_not_consume_signal_budget() {
    let server = create_test_server().await;
    let (alice, _alice_rx) = register_client(&server).await;
    let (legacy, mut legacy_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&legacy, v2_with_webrtc_transport());

    let room_id = create_db_room(&server, alice).await;
    server
        .database
        .add_player_to_room(&room_id, player_info(legacy, "legacy"))
        .await
        .expect("add legacy peer");
    for id in [alice, legacy] {
        server
            .connection_manager
            .assign_client_to_room(&id, room_id)
            .await;
    }

    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: true,
            },
        )
        .await;

    assert_silent(&mut legacy_rx).await;
    assert_eq!(
        valid_signal_budget_used(&server, &alice).await,
        0,
        "legacy-only recipient sets must not consume v3 control-plane budget"
    );
    assert_eq!(
        server
            .metrics
            .transport_status_fanout
            .load(Ordering::Relaxed),
        0,
        "no eligible v3 recipients means no fan-out event"
    );
}

// ---------------------------------------------------------------------------
// Relay floor never closes (P5): GameData still relays after a P2P-failure report.
// ---------------------------------------------------------------------------

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn game_data_still_relays_after_transport_status_disconnected() {
    let server = create_test_server().await;
    let (alice, _alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    server
        .connection_manager
        .assign_client_to_room(&alice, room_id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&bob, room_id)
        .await;

    // Alice reports her P2P path failed and she fell back to the relay floor.
    server
        .handle_client_message(
            &alice,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: false,
            },
        )
        .await;

    expect_peer_transport_status(&mut bob_rx, alice, Transport::WebRtc, false).await;

    // The server keeps relaying GameData unconditionally — the floor never closes.
    let payload = json!({ "tick": 42 });
    server
        .handle_game_data(&alice, payload.clone(), None, None)
        .await;

    match recv(&mut bob_rx).await.as_ref() {
        ServerMessage::GameData {
            from_player, data, ..
        } => {
            assert_eq!(*from_player, alice);
            assert_eq!(*data, payload);
        }
        other => panic!("expected relayed GameData after fallback, got {other:?}"),
    }
}
