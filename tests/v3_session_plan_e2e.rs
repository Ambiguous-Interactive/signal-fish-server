//! End-to-end protocol v3 (P3) session-plan tests through the real WebSocket
//! stack. A server configured for `mesh` topology finalizes a two-player room:
//! each v3 client must receive `GameStarting` THEN a per-recipient `SessionPlan`
//! (mesh + webrtc, fallback relay, naming the other peer with the single-offerer
//! `initiate`). A mixed room (one v3 + one relay-only client) resolves to the
//! relay floor: the v3 member receives an explicit no-peer `relay` plan while
//! the v2 member receives no v3-only frame.
//!
//! Models the harness in `tests/v3_signaling_e2e.rs` but injects a chosen
//! `SessionConfig` so finalization can pick a non-relay plan.

mod test_helpers;
mod websocket_test_helpers;

use futures_util::SinkExt;
use signal_fish_server::config::{AppRegistrationEntry, SessionConfig, TurnConfig};
use signal_fish_server::protocol::{
    ClientMessage, ConnectionInfo, DirectEndpoint, IceServer, PlayerId, RoomId, ServerMessage,
    Topology, Transport,
};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::{create_router, websocket_route_v3};
use std::sync::Arc;
use test_helpers::{test_protocol_config, test_server_config, RunningTestServer};
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

fn app_entry() -> AppRegistrationEntry {
    AppRegistrationEntry {
        app_id: APP_ID.to_string(),
        app_name: "V3 Session Plan App".to_string(),
        max_rooms: Some(10),
        max_players_per_room: Some(8),
        rate_limit_per_minute: Some(600),
        max_relay_bytes: None,
        require_connect_token: None,
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
async fn start_server_with_session(session: SessionConfig) -> RunningTestServer {
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

    start_server(game_server).await
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
            connect_token: None,
            sdk_version: None,
            platform: None,
            game_data_format: None,
            protocol_version,
            supported_transports: transports,
            supported_topologies: topologies,
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

/// Authenticate as a v3 host+direct client.
async fn authenticate_v3_direct(ws: &mut WsStream) {
    authenticate(
        ws,
        Some(3),
        Some(vec![Transport::Relay, Transport::Direct]),
        Some(vec![Topology::Relay, Topology::Host]),
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

            password: None,
            join_only: None,
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
    peer_connections: Vec<signal_fish_server::protocol::PeerConnectionInfo>,
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
    let peer_connections = next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "finalization GameStarting",
        |message| match message {
            ServerMessage::GameStarting { peer_connections } => Some(peer_connections),
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
        peer_connections,
        session_plan,
        skipped_after_game_starting,
    }
}

#[tokio::test]
async fn mesh_room_finalization_sends_game_starting_then_session_plan() {
    let running_server = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();

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

    // Each plan names the other peer, with exactly one deterministic offerer.
    assert_eq!(plan1.peers[0].player_id, peer2_id);
    assert_eq!(plan2.peers[0].player_id, peer1_id);
    assert_ne!(plan1.peers[0].initiate, plan2.peers[0].initiate);
    assert_eq!(plan1.peers[0].initiate, peer1_id < peer2_id);
    running_server.shutdown().await;
}

/// #1658: unspecified connect targets must never become executable Direct endpoints.
#[test]
fn direct_endpoints_reject_all_numeric_unspecified_ipv4_aliases() {
    for components in 1..=4 {
        for zero in ["0", "00", "000", "0x0", "0X000"] {
            let host = vec![zero; components].join(".");
            for host in [host.clone(), format!("{host}.")] {
                assert_eq!(
                    DirectEndpoint::from_connection_info(&ConnectionInfo::Direct {
                        host: host.clone(),
                        port: 7777,
                    }),
                    None,
                    "unspecified IPv4 alias {host:?} cannot anchor Direct gameplay"
                );
            }
        }
    }
    for host in [
        "0x0.00.000.0X0",
        "0.0x000",
        "0.0.0x0",
        "0x0.0.0.0",
        "::ffff:0.0.0.0",
        "::ffff:0:0",
        "0:0:0:0:0:ffff:0:0",
        "::0.0.0.0",
        "::0000",
    ] {
        assert!(
            DirectEndpoint::from_connection_info(&ConnectionInfo::Direct {
                host: host.into(),
                port: 7777,
            })
            .is_none(),
            "unspecified connect target alias {host:?}"
        );
    }
    for host in [
        "127.0.0.1",
        "127.1",
        "0x7f000001",
        "2130706433",
        "::ffff:192.0.2.10",
        "::ffff:c000:20a",
        "::192.0.2.10",
        "0.example",
        "host.0",
        "0x0.example",
        "0.0.0.0.example",
        "0.0.0.0.0",
        "host.example.",
    ] {
        let endpoint = DirectEndpoint::from_connection_info(&ConnectionInfo::Direct {
            host: host.into(),
            port: 7777,
        });
        assert_eq!(
            endpoint,
            Some(DirectEndpoint {
                host: host.into(),
                port: 7777,
            }),
            "usable or DNS endpoint {host:?} must retain its exact spelling"
        );
    }
}

/// #1663: group IP literals cannot anchor a point-to-point Direct session.
#[test]
fn direct_endpoints_reject_group_address_literals() {
    for host in [
        "224.0.0.1",
        "239.255.255.255",
        "255.255.255.255",
        "ff02::1",
        "ff0e::1",
        "::ffff:224.0.0.1",
        "::ffff:255.255.255.255",
        "224.1",
        "239.65535",
        "4009754625",
        "4294967295",
        "0xef000001",
        "0XFFFFFFFF",
        "0357.0.0.1",
        "037777777777",
        "0xef.0.1",
        "255.0xffffff",
        "239.1.",
    ] {
        assert!(
            DirectEndpoint::from_connection_info(&ConnectionInfo::Direct {
                host: host.into(),
                port: 7777,
            })
            .is_none(),
            "group destination {host} cannot host a Direct session"
        );
    }
    for host in [
        "223.255.255.255",
        "240.0.0.1",
        "4026531841",
        "192.0.2.255",
        "::1",
        "2001:db8::1",
        "::ffff:192.0.2.1",
        "host.example",
    ] {
        assert_eq!(
            DirectEndpoint::from_connection_info(&ConnectionInfo::Direct {
                host: host.into(),
                port: 7777,
            }),
            Some(DirectEndpoint {
                host: host.into(),
                port: 7777
            })
        );
    }
}

/// #1658/#1663: finalization, host election and withdrawal share endpoint readiness.
#[tokio::test]
async fn direct_room_unusable_hosts_use_an_executable_plan() {
    for (host_endpoint, client_endpoint) in [
        ("0", None),
        ("0x0", Some("192.0.2.20")),
        ("0.00.0", None),
        ("224.0.0.1", None),
        ("ff02::1", Some("192.0.2.20")),
        ("255.255.255.255", None),
        ("::ffff:239.1.2.3", Some("192.0.2.20")),
        ("0xef000001", None),
        ("4294967295", Some("192.0.2.20")),
    ] {
        let running_server = start_server_with_session(SessionConfig {
            default_topology: Topology::Host,
            enable_webrtc: false,
            enable_direct: true,
            ..SessionConfig::default()
        })
        .await;
        let mut host = connect(running_server.addr()).await;
        authenticate_v3_direct(&mut host).await;
        let (_, room_code, host_id) = join_room(&mut host, None, "Host").await;
        let mut client = connect(running_server.addr()).await;
        authenticate_v3_direct(&mut client).await;
        let (_, _, client_id) = join_room(&mut client, Some(room_code), "Client").await;
        for (socket, endpoint) in [
            (&mut host, Some(host_endpoint)),
            (&mut client, client_endpoint),
        ] {
            if let Some(endpoint) = endpoint {
                send(
                    socket,
                    &ClientMessage::ProvideConnectionInfo {
                        connection_info: ConnectionInfo::Direct {
                            host: endpoint.into(),
                            port: 7777,
                        },
                    },
                )
                .await;
            }
        }
        ready(&mut host).await;
        await_ready_count(&mut host, 1).await;
        await_ready_count(&mut client, 1).await;
        ready(&mut client).await;
        await_ready_count(&mut host, 2).await;
        await_ready_count(&mut client, 2).await;
        send(&mut host, &ClientMessage::StartGame).await;
        let (host_plan, client_plan) =
            tokio::join!(read_finalization(&mut host), read_finalization(&mut client));
        for publication in [&host_plan, &client_plan] {
            assert_eq!(publication.peer_connections.len(), 2);
            for (id, endpoint) in [(host_id, Some(host_endpoint)), (client_id, client_endpoint)] {
                let peer = publication
                    .peer_connections
                    .iter()
                    .find(|peer| peer.player_id == id)
                    .unwrap();
                assert_eq!(
                    peer.connection_info,
                    endpoint.map(|host| ConnectionInfo::Direct {
                        host: host.into(),
                        port: 7777,
                    }),
                    "legacy metadata must retain its exact value"
                );
            }
        }
        let host_plan = host_plan.expect_session_plan("host");
        let client_plan = client_plan.expect_session_plan("client");
        let generation = host_plan.generation;
        for (plan, recipient) in [(host_plan, host_id), (client_plan, client_id)] {
            assert_eq!(plan.generation, generation);
            let expected = if client_endpoint.is_some() {
                (Topology::Host, Transport::Direct, Some(client_id))
            } else {
                (Topology::Relay, Transport::Relay, None)
            };
            assert_eq!(
                (plan.topology, plan.transport, plan.host),
                expected,
                "host={host_endpoint:?}, client={client_endpoint:?}"
            );
            assert_eq!(
                plan.direct_endpoint,
                client_endpoint.map(|endpoint| DirectEndpoint {
                    host: endpoint.into(),
                    port: 7777,
                })
            );
            assert_eq!(plan.peers.len(), usize::from(client_endpoint.is_some()));
            if client_endpoint.is_some() {
                let peer = &plan.peers[0];
                assert_eq!(
                    peer.player_id,
                    if recipient == host_id {
                        client_id
                    } else {
                        host_id
                    }
                );
                assert_eq!(
                    peer.player_name,
                    if recipient == host_id {
                        "Client"
                    } else {
                        "Host"
                    }
                );
                assert_eq!(peer.initiate, recipient != client_id);
                assert_eq!(peer.is_authority, peer.player_id == client_id);
            }
            assert_eq!(plan.fallback, Transport::Relay);
            assert!(plan.ice_servers.is_empty());
        }
        if client_endpoint.is_some() {
            send(
                &mut client,
                &ClientMessage::ProvideConnectionInfo {
                    connection_info: ConnectionInfo::Direct {
                        host: host_endpoint.into(),
                        port: 7777,
                    },
                },
            )
            .await;
            let mut reset_generation = None;
            for socket in [&mut host, &mut client] {
                let reset = next_matching_server_message_within(
                    socket,
                    SERVER_MESSAGE_TIMEOUT,
                    "Direct host withdrew its executable endpoint",
                    |message| match message {
                        ServerMessage::SessionPlan(plan) => Some(plan),
                        _ => None,
                    },
                )
                .await;
                assert_eq!(
                    (reset.topology, reset.transport, reset.host),
                    (Topology::Relay, Transport::Relay, None)
                );
                assert_eq!(reset.direct_endpoint, None);
                assert!(reset.peers.is_empty());
                assert!(reset.ice_servers.is_empty());
                assert_eq!(reset.fallback, Transport::Relay);
                assert_ne!(reset.generation, generation);
                assert_eq!(
                    *reset_generation.get_or_insert(reset.generation),
                    reset.generation
                );
            }
        }
        running_server.shutdown().await;
    }
}

#[tokio::test]
async fn direct_room_selects_and_delivers_the_validated_host_endpoint() {
    let running_server = start_server_with_session(SessionConfig {
        default_topology: Topology::Host,
        enable_webrtc: false,
        enable_direct: true,
        ..SessionConfig::default()
    })
    .await;
    let addr = running_server.addr();

    let mut host = connect(addr).await;
    authenticate_v3_direct(&mut host).await;
    let (_room_id, room_code, host_id) = join_room(&mut host, None, "Host").await;
    send(
        &mut host,
        &ClientMessage::ProvideConnectionInfo {
            connection_info: ConnectionInfo::Direct {
                host: "192.0.2.10".to_string(),
                port: 7777,
            },
        },
    )
    .await;
    send(&mut host, &ClientMessage::Ping).await;
    next_matching_server_message_within(
        &mut host,
        SERVER_MESSAGE_TIMEOUT,
        "ProvideConnectionInfo processing barrier",
        |message| matches!(message, ServerMessage::Pong).then_some(()),
    )
    .await;

    let mut client = connect(addr).await;
    authenticate_v3_direct(&mut client).await;
    let (_room_id, _code, client_id) = join_room(&mut client, Some(room_code), "Client").await;

    ready(&mut host).await;
    await_ready_count(&mut host, 1).await;
    await_ready_count(&mut client, 1).await;
    ready(&mut client).await;
    await_ready_count(&mut host, 2).await;
    await_ready_count(&mut client, 2).await;
    send(&mut host, &ClientMessage::StartGame).await;

    let (host_plan, client_plan) =
        tokio::join!(read_finalization(&mut host), read_finalization(&mut client));
    let host_plan = host_plan.expect_session_plan("Direct host");
    let client_plan = client_plan.expect_session_plan("Direct client");
    let endpoint = Some(DirectEndpoint {
        host: "192.0.2.10".to_string(),
        port: 7777,
    });

    for plan in [&host_plan, &client_plan] {
        assert_eq!(plan.topology, Topology::Host);
        assert_eq!(plan.transport, Transport::Direct);
        assert_eq!(plan.host, Some(host_id));
        assert_eq!(plan.direct_endpoint, endpoint);
        assert!(plan.ice_servers.is_empty());
        assert_eq!(plan.fallback, Transport::Relay);
    }
    assert_eq!(host_plan.peers.len(), 1);
    assert_eq!(host_plan.peers[0].player_id, client_id);
    assert!(!host_plan.peers[0].initiate);
    assert_eq!(client_plan.peers.len(), 1);
    assert_eq!(client_plan.peers[0].player_id, host_id);
    assert!(client_plan.peers[0].initiate);

    running_server.shutdown().await;
}

#[tokio::test]
async fn mixed_v2_v3_room_finalization_sends_relay_plan_only_to_v3() {
    let running_server = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();

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
    // in order). The room resolved to the relay floor, so the v3 member gets
    // an explicit authoritative reset while the v2 member remains plan-free.
    let (plan1, plan2) = tokio::join!(read_finalization(&mut peer1), read_finalization(&mut peer2));
    let plan1 = plan1.expect_session_plan("v3 peer in relay-resolved room");
    assert_eq!(plan1.topology, Topology::Relay);
    assert_eq!(plan1.transport, Transport::Relay);
    assert_eq!(plan1.host, None);
    assert!(plan1.peers.is_empty());
    assert!(plan1.ice_servers.is_empty());
    assert_eq!(plan1.fallback, Transport::Relay);
    assert!(
        plan2.session_plan.is_none(),
        "v2 peer must never receive a SessionPlan; skipped after GameStarting: {}",
        plan2.skipped_after_game_starting
    );
    running_server.shutdown().await;
}
