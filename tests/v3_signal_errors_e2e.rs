//! End-to-end protocol v3 (P2/P3) `Signal` REJECTION conformance tests through
//! the real WebSocket stack. Each test boots an in-process axum server on
//! `127.0.0.1:0` (the harness shared with `tests/v3_signaling_e2e.rs` /
//! `tests/v3_multipeer_e2e.rs`), drives real `tokio-tungstenite` v3 clients, and
//! exercises every rejection gate in [`EnhancedGameServer::handle_signal`]:
//!
//! 0. payload size cap (`security.max_signal_bytes`) → `SignalTooLarge`, with a
//!    boundary pair (at-cap relays, one-byte-over rejects);
//! 1. self-signal (`to == from`) → `SignalTargetNotFound`, no echo;
//! 2. sender not in a room → `NotInRoom`;
//! 3. target not in any room (random UUID) → `SignalTargetNotFound`;
//! 4. cross-room (two clients, different rooms) → `CrossRoomSignal`;
//! 5. relay-only v3 sender (WebRTC transport not negotiated) →
//!    `UnsupportedTransport` (the relay-floor invariant: a relay-only v3 client
//!    must NOT be able to relay `Signal`s);
//! 6. target left the room after pairing → `SignalTargetNotFound`, no panic;
//! 7. per-connection rate-limit exhaustion → `SignalRateLimited` on the
//!    over-budget signal, within-budget signals still relay.
//!
//! Every test asserts the EXACT `ErrorCode` the server returns to the sender AND
//! that the would-be target receives NOTHING (the rejected signal is not
//! relayed). This is the over-the-wire counterpart to the handler-level
//! rejection tests in `src/server/signaling_tests.rs`.
//!
//! As with `tests/v3_signaling_e2e.rs`, the room is created with capacity 2 (or
//! more) and the peers never mark ready, so the lobby never finalizes — no
//! `SessionPlan`/`NewPeer` fires and the peers relay over the unchanged
//! `handle_signal` path.

mod test_helpers;
mod v3_conformance_helpers;
mod websocket_test_helpers;

use std::sync::Arc;

use serde_json::json;
use signal_fish_server::config::AppAuthEntry;
use signal_fish_server::protocol::{
    ClientMessage, ErrorCode, PlayerId, RoomId, ServerMessage, Topology, Transport,
};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::{create_router, websocket_handler_v3};
use test_helpers::{test_protocol_config, test_server_config};
use tokio::net::TcpListener;
use tokio_tungstenite::connect_async;
use v3_conformance_helpers::{send, SERVER_MESSAGE_TIMEOUT};
use websocket_test_helpers::{
    expect_no_server_message_within, next_matching_server_message_within, WsStream,
};

const APP_ID: &str = "v3-signal-errors-app";

/// Window in which a forbidden (wrongly-relayed) `Signal` would have arrived at
/// the target if the gate were broken. Mirrors `v3_multipeer_e2e`'s silence
/// window: long enough that a real relay would land, short enough to keep the
/// suite brisk.
const SILENCE_WINDOW: tokio::time::Duration = tokio::time::Duration::from_secs(2);

fn app_entry() -> AppAuthEntry {
    AppAuthEntry {
        app_id: APP_ID.to_string(),
        app_secret: "secret".to_string(),
        app_name: "V3 Signal Errors App".to_string(),
        max_rooms: Some(10),
        max_players_per_room: Some(8),
        rate_limit_per_minute: Some(600),
    }
}

/// Boot the production router (`/v2` nest + `/v3/ws` alias) around a server
/// built from the given `ServerConfig`, returning the bound address and handle.
/// Same shape as `tests/v3_signaling_e2e.rs::start_auth_server_with_config`.
async fn start_server_with_config(
    mut server_config: ServerConfig,
) -> (std::net::SocketAddr, Arc<EnhancedGameServer>) {
    use axum::routing::get;

    server_config.auth_enabled = true;

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
        .with_state(game_server.clone());

    tokio::spawn(async move {
        axum::serve(
            listener,
            combined_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    // The listener is already bound, so immediate connections are accepted by
    // the kernel and served once the spawned task polls them (no startup sleep).
    (addr, game_server)
}

/// Boot a server with the default test config (16KB signal cap, generous 600
/// signal budget).
async fn start_server() -> std::net::SocketAddr {
    start_server_with_config(test_server_config()).await.0
}

async fn connect(addr: std::net::SocketAddr) -> WsStream {
    let url = format!("ws://{addr}/v3/ws");
    let (ws, _) = tokio::time::timeout(tokio::time::Duration::from_secs(10), connect_async(&url))
        .await
        .expect("connect timeout")
        .expect("connect");
    ws
}

/// Authenticate, advertising the given v3 capabilities, asserting the negotiated
/// version reported in `ProtocolInfo`.
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

/// Authenticate as a v3 + WebRTC client (relay + mesh topologies).
async fn authenticate_v3_webrtc(ws: &mut WsStream) {
    authenticate(
        ws,
        Some(3),
        Some(vec![Transport::Relay, Transport::WebRtc]),
        Some(vec![Topology::Relay, Topology::Mesh]),
        Some(3),
    )
    .await;
}

/// Authenticate as a v3 RELAY-ONLY client: it negotiates v3 (so the v3 gate
/// passes) but advertises only the `Relay` transport, so the WebRTC-signaling
/// transport gate must reject any `Signal` it sends — the relay-floor invariant.
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

/// Join (or create) a room, returning `(room_id, room_code, our_player_id)`.
/// Capacity defaults to 2 (the two peers fill but never ready, so the room never
/// finalizes — the unchanged `handle_signal` relay path is exercised directly).
async fn join_room(
    ws: &mut WsStream,
    room_code: Option<String>,
    player_name: &str,
    max_players: u8,
) -> (RoomId, String, PlayerId) {
    send(
        ws,
        &ClientMessage::JoinRoom {
            game_name: "signaling-game".to_string(),
            room_code,
            player_name: player_name.to_string(),
            max_players: Some(max_players),
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await;

    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "room join response", |msg| {
        match msg {
            ServerMessage::RoomJoined(p) => Some((p.room_id, p.room_code, p.player_id)),
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                panic!("room join failed: {reason} ({error_code:?})")
            }
            _ => None,
        }
    })
    .await
}

/// Send one `Signal { to, signal }` from `ws`.
async fn send_signal(ws: &mut WsStream, to: PlayerId, signal: serde_json::Value) {
    send(ws, &ClientMessage::Signal { to, signal }).await;
}

/// Assert the next *relevant* message the sender receives is an `Error` carrying
/// exactly `expected`, never a relayed `Signal` echo. Housekeeping messages
/// (e.g. `PlayerJoined`/`PlayerLeft`) are skipped.
async fn expect_signal_error(ws: &mut WsStream, who: &str, expected: ErrorCode) {
    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "signal rejection error",
        |message| match message {
            ServerMessage::Error { error_code, .. } => {
                assert_eq!(
                    error_code,
                    Some(expected.clone()),
                    "{who} expected {expected:?}"
                );
                Some(())
            }
            ServerMessage::Signal { .. } => {
                panic!("{who}: a rejected signal must not be relayed back to the sender")
            }
            _ => None,
        },
    )
    .await;
}

/// Assert the next relayed `Signal` matches `(from, payload)`.
async fn expect_relayed_signal(
    ws: &mut WsStream,
    who: &str,
    from: PlayerId,
    payload: &serde_json::Value,
) {
    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "relayed signal", |message| {
        match message {
            ServerMessage::Signal { from: got, signal } => {
                assert_eq!(got, from, "{who}: relayed Signal must carry the sender id");
                assert_eq!(
                    &signal, payload,
                    "{who}: signal payload must be byte-preserved"
                );
                Some(())
            }
            _ => None,
        }
    })
    .await;
}

/// Assert the target receives NO `Signal` within the silence window. Any
/// non-`Signal` housekeeping frame is tolerated; a relayed `Signal` fails.
async fn expect_no_relayed_signal(ws: &mut WsStream, who: &str) {
    // `expect_no_server_message_within` would fail on benign housekeeping
    // (`PlayerJoined`/`PlayerLeft`), so filter to the forbidden `Signal` only.
    if let Some(()) = websocket_test_helpers::maybe_next_matching_server_message_within(
        ws,
        SILENCE_WINDOW,
        "no relayed signal",
        |message| match message {
            ServerMessage::Signal { .. } => Some(()),
            _ => None,
        },
    )
    .await
    {
        panic!("{who}: target must not receive a relayed Signal for a rejected request");
    }
}

/// Serialized length of an opaque signal payload, exactly as the cap measures it
/// (canonical `serde_json` bytes — see `signaling::canonical_signal_len`).
fn payload_len(signal: &serde_json::Value) -> usize {
    serde_json::to_vec(signal).expect("signal serializes").len()
}

/// Authenticate two v3+WebRTC peers into one fresh room of capacity 2, returning
/// `(peer1, peer1_id, peer2, peer2_id)`.
async fn webrtc_pair(addr: std::net::SocketAddr) -> (WsStream, PlayerId, WsStream, PlayerId) {
    let mut peer1 = connect(addr).await;
    authenticate_v3_webrtc(&mut peer1).await;
    let (_room_id, room_code, peer1_id) = join_room(&mut peer1, None, "PeerOne", 2).await;

    let mut peer2 = connect(addr).await;
    authenticate_v3_webrtc(&mut peer2).await;
    let (_room_id, _code, peer2_id) = join_room(&mut peer2, Some(room_code), "PeerTwo", 2).await;
    assert_ne!(peer1_id, peer2_id);

    (peer1, peer1_id, peer2, peer2_id)
}

// ---------------------------------------------------------------------------
// 1. Payload size cap (`security.max_signal_bytes`) + boundary pair.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn oversized_signal_is_rejected_and_not_relayed() {
    // Cap the serialized `signal` payload at 256 bytes.
    let mut server_config = test_server_config();
    server_config.max_signal_bytes = 256;
    let (addr, _server) = start_server_with_config(server_config).await;

    let (mut peer1, _peer1_id, mut peer2, peer2_id) = webrtc_pair(addr).await;

    // Serialized length comfortably over the cap.
    let oversized = json!({ "Offer": "x".repeat(512) });
    assert!(payload_len(&oversized) > 256);
    send_signal(&mut peer1, peer2_id, oversized).await;

    expect_signal_error(&mut peer1, "sender", ErrorCode::SignalTooLarge).await;
    expect_no_relayed_signal(&mut peer2, "oversized target").await;
}

#[tokio::test]
async fn signal_size_boundary_at_cap_relays_just_over_rejects() {
    // A fixed payload whose canonical length is the cap. The same payload relays
    // when the cap equals its length and is rejected when the cap is one byte
    // smaller — the boundary pair around `max_signal_bytes`.
    let payload = json!({ "Offer": "v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\n" });
    let exact = payload_len(&payload);

    // (a) Cap == length: relays byte-preserved.
    {
        let mut server_config = test_server_config();
        server_config.max_signal_bytes = exact;
        let (addr, _server) = start_server_with_config(server_config).await;
        let (mut peer1, peer1_id, mut peer2, peer2_id) = webrtc_pair(addr).await;

        send_signal(&mut peer1, peer2_id, payload.clone()).await;
        expect_relayed_signal(&mut peer2, "at-cap target", peer1_id, &payload).await;
    }

    // (b) Cap == length - 1: the same payload is one byte over and is rejected.
    {
        let mut server_config = test_server_config();
        server_config.max_signal_bytes = exact - 1;
        let (addr, _server) = start_server_with_config(server_config).await;
        let (mut peer1, _peer1_id, mut peer2, peer2_id) = webrtc_pair(addr).await;

        send_signal(&mut peer1, peer2_id, payload.clone()).await;
        expect_signal_error(&mut peer1, "over-cap sender", ErrorCode::SignalTooLarge).await;
        expect_no_relayed_signal(&mut peer2, "over-cap target").await;
    }
}

// ---------------------------------------------------------------------------
// 2. Self-signal (`to == from`): documented behavior is SignalTargetNotFound.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn self_signal_is_rejected_and_not_echoed() {
    let addr = start_server().await;

    let mut peer1 = connect(addr).await;
    authenticate_v3_webrtc(&mut peer1).await;
    let (_room_id, _room_code, peer1_id) = join_room(&mut peer1, None, "PeerOne", 2).await;

    // `to == from`: a peer cannot WebRTC to itself. The server documents this as
    // SignalTargetNotFound (the self-signal guard in `handle_signal`).
    send_signal(&mut peer1, peer1_id, json!({ "Offer": "x" })).await;

    // The sender must receive a rejection, never its own echoed `Signal`.
    expect_signal_error(
        &mut peer1,
        "self-signaller",
        ErrorCode::SignalTargetNotFound,
    )
    .await;
    expect_no_relayed_signal(&mut peer1, "self-signaller echo").await;
}

// ---------------------------------------------------------------------------
// 3. Sender authenticated but never joined a room → NotInRoom.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn signal_from_sender_not_in_a_room_is_rejected() {
    let addr = start_server().await;

    // A v3+WebRTC target sitting in a room (so the target is a real, in-room id).
    let mut target = connect(addr).await;
    authenticate_v3_webrtc(&mut target).await;
    let (_room_id, _room_code, target_id) = join_room(&mut target, None, "Target", 2).await;

    // The sender authenticates as v3+WebRTC but never joins any room.
    let mut roomless = connect(addr).await;
    authenticate_v3_webrtc(&mut roomless).await;

    send_signal(&mut roomless, target_id, json!({ "Offer": "x" })).await;

    expect_signal_error(&mut roomless, "roomless sender", ErrorCode::NotInRoom).await;
    expect_no_relayed_signal(&mut target, "target of roomless sender").await;
}

// ---------------------------------------------------------------------------
// 4. Target is a random UUID not present anywhere → SignalTargetNotFound.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn signal_to_unknown_target_is_rejected() {
    let addr = start_server().await;

    let mut peer1 = connect(addr).await;
    authenticate_v3_webrtc(&mut peer1).await;
    let (_room_id, _room_code, _peer1_id) = join_room(&mut peer1, None, "PeerOne", 2).await;

    // Target is a random, unregistered player id.
    let ghost = PlayerId::new_v4();
    send_signal(&mut peer1, ghost, json!({ "Offer": "x" })).await;

    expect_signal_error(&mut peer1, "sender", ErrorCode::SignalTargetNotFound).await;
}

// ---------------------------------------------------------------------------
// 5. Cross-room: two v3 peers in DIFFERENT rooms → CrossRoomSignal.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cross_room_signal_is_rejected_and_not_relayed() {
    let addr = start_server().await;

    // Peer A creates room A.
    let mut peer_a = connect(addr).await;
    authenticate_v3_webrtc(&mut peer_a).await;
    let (_room_a, _code_a, _a_id) = join_room(&mut peer_a, None, "RoomAPeer", 2).await;

    // Peer B creates a SEPARATE room B (no shared room code).
    let mut peer_b = connect(addr).await;
    authenticate_v3_webrtc(&mut peer_b).await;
    let (_room_b, _code_b, b_id) = join_room(&mut peer_b, None, "RoomBPeer", 2).await;

    // A signals B across rooms.
    send_signal(&mut peer_a, b_id, json!({ "Offer": "cross" })).await;

    expect_signal_error(&mut peer_a, "cross-room sender", ErrorCode::CrossRoomSignal).await;
    expect_no_relayed_signal(&mut peer_b, "cross-room target").await;
}

// ---------------------------------------------------------------------------
// 6. Relay-only v3 sender (WebRTC transport not negotiated) →
//    UnsupportedTransport. Relay-floor invariant: a relay-only v3 client must
//    not be able to relay `Signal`s.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn signal_from_relay_only_v3_sender_is_rejected() {
    let addr = start_server().await;

    // The relay-only v3 sender creates the room (so it IS in a room: this
    // isolates the transport gate from the NotInRoom gate).
    let mut relay_only = connect(addr).await;
    authenticate_v3_relay_only(&mut relay_only).await;
    let (_room_id, room_code, _relay_id) = join_room(&mut relay_only, None, "RelayOnly", 2).await;

    // A v3+WebRTC target joins the same room.
    let mut target = connect(addr).await;
    authenticate_v3_webrtc(&mut target).await;
    let (_room_id, _code, target_id) = join_room(&mut target, Some(room_code), "Target", 2).await;

    send_signal(&mut relay_only, target_id, json!({ "Offer": "x" })).await;

    expect_signal_error(
        &mut relay_only,
        "relay-only sender",
        ErrorCode::UnsupportedTransport,
    )
    .await;
    expect_no_relayed_signal(&mut target, "target of relay-only sender").await;
}

// ---------------------------------------------------------------------------
// 7. Signal after the target left the room → SignalTargetNotFound, no panic.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn signal_after_target_leaves_is_rejected() {
    let addr = start_server().await;

    let (mut peer1, peer1_id, mut peer2, peer2_id) = webrtc_pair(addr).await;

    // Sanity: while both are in the room, a signal relays normally.
    let warmup = json!({ "IceCandidate": "candidate:before-leave" });
    send_signal(&mut peer1, peer2_id, warmup.clone()).await;
    expect_relayed_signal(&mut peer2, "peer2 (pre-leave)", peer1_id, &warmup).await;

    // Peer2 leaves the room; peer1 observes the departure, then signals the
    // now-departed peer. The server must reject, not panic.
    send(&mut peer2, &ClientMessage::LeaveRoom).await;
    next_matching_server_message_within(&mut peer1, SERVER_MESSAGE_TIMEOUT, "PlayerLeft", |msg| {
        match msg {
            ServerMessage::PlayerLeft { player_id, .. } if player_id == peer2_id => Some(()),
            _ => None,
        }
    })
    .await;

    send_signal(
        &mut peer1,
        peer2_id,
        json!({ "IceCandidate": "after-leave" }),
    )
    .await;
    expect_signal_error(
        &mut peer1,
        "sender after target left",
        ErrorCode::SignalTargetNotFound,
    )
    .await;
    expect_no_relayed_signal(&mut peer2, "departed target").await;
}

// ---------------------------------------------------------------------------
// 8. Per-connection rate-limit exhaustion → SignalRateLimited on the
//    over-budget signal; within-budget signals still relay.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn signal_rate_limit_exhaustion_rejects_over_budget() {
    // The default test budget (600) is too large to exhaust on the wire, so set
    // a small per-connection signal budget via the rate-limit config.
    let mut server_config = test_server_config();
    server_config.rate_limit_config.max_signals = 2;
    let (addr, _server) = start_server_with_config(server_config).await;

    let (mut peer1, peer1_id, mut peer2, peer2_id) = webrtc_pair(addr).await;

    // First two valid signals are within budget and relay.
    let first = json!({ "IceCandidate": "1" });
    let second = json!({ "IceCandidate": "2" });
    send_signal(&mut peer1, peer2_id, first.clone()).await;
    send_signal(&mut peer1, peer2_id, second.clone()).await;
    expect_relayed_signal(&mut peer2, "peer2 (#1)", peer1_id, &first).await;
    expect_relayed_signal(&mut peer2, "peer2 (#2)", peer1_id, &second).await;

    // The third signal exceeds the budget: the sender is rate-limited and the
    // target receives nothing more.
    send_signal(&mut peer1, peer2_id, json!({ "IceCandidate": "3" })).await;
    expect_signal_error(
        &mut peer1,
        "over-budget sender",
        ErrorCode::SignalRateLimited,
    )
    .await;
    expect_no_relayed_signal(&mut peer2, "over-budget target").await;
}

// Keep `expect_no_server_message_within` referenced even if every silence check
// uses the `Signal`-filtered variant above: a future strict-silence assertion
// can switch to it without re-importing.
#[allow(dead_code)]
async fn assert_total_silence(ws: &mut WsStream, who: &str) {
    expect_no_server_message_within(ws, SILENCE_WINDOW, who).await;
}
