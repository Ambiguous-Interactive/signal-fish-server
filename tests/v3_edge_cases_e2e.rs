//! End-to-end protocol v3 (P2/P3) EDGE-CASE tests through the real WebSocket
//! stack, targeting under-covered corners of the room/validation contract:
//!
//! 1. `solo_room_max_players_1_*` — under the explicit-start contract a solo
//!    (max_players=1) v3 mesh+webrtc room enters the lobby immediately (a room
//!    enters Lobby with >=1 player; `max_players` is a ceiling, not a required
//!    count, `src/protocol/room_state.rs:342-352`). The lone player may ready,
//!    and an explicit `StartGame` finalizes it (solo is allowed). A `StartGame`
//!    sent before the player is ready is rejected `GAME_START_NOT_READY`.
//! 2. `max_players_boundary_*` — a 2-seat room finalizes when both ready AND an
//!    explicit `StartGame` is sent, and a solo-create with the DEFAULT capacity
//!    (test config `default_max_players = 4`) may ready before the room is full
//!    and the lone, only-ready creator can `StartGame` to finalize.
//! 3. `player_name_validation_*` — empty / whitespace-only / max / max+1 (BYTE
//!    length) / unicode-emoji / leading-trailing-space player names, asserting
//!    the server's actual accept/reject + error code for each.
//! 4. `game_name_validation_*` — empty / too-long / invalid-char game names.
//! 5. `room_code_case_insensitivity_*` — a lowercase create plus uppercase and
//!    mixed-case joins all land in the SAME room (codes are upper-cased in
//!    `src/server/room_service.rs:150`).
//! 6. `empty_room_cleanup_*` — what a creator leaving an empty room leaves
//!    behind, and whether a later same-code join rejoins the lingering room or
//!    creates a fresh one (cleanup is timeout-driven, not synchronous on
//!    leave).
//!
//! These reuse the production-router harness from the other v3 e2e suites and
//! the shared deadline readers in `tests/websocket_test_helpers/mod.rs`.

mod test_helpers;
mod v3_conformance_helpers;
mod websocket_test_helpers;

use std::sync::Arc;

use signal_fish_server::config::{AppRegistrationEntry, SessionConfig};
use signal_fish_server::protocol::{
    ClientMessage, ErrorCode, LobbyState, RoomJoinedPayload, ServerMessage, Topology, Transport,
};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::{create_router, websocket_handler_v3};
use test_helpers::{test_protocol_config, test_server_config, RunningTestServer};
use tokio_tungstenite::connect_async;
use v3_conformance_helpers::{ready, send, start_game, SERVER_MESSAGE_TIMEOUT};
use websocket_test_helpers::{next_matching_server_message_within, WsStream};

const APP_ID: &str = "v3-edge-cases-app";

fn app_entry() -> AppRegistrationEntry {
    AppRegistrationEntry {
        app_id: APP_ID.to_string(),
        app_name: "V3 Edge Cases App".to_string(),
        max_rooms: Some(10),
        max_players_per_room: Some(8),
        rate_limit_per_minute: Some(600),
    }
}

/// A mesh-preferring `SessionConfig` (so a finalized v3+webrtc room resolves to
/// a mesh `SessionPlan` instead of the default relay floor). Mirrors the
/// multipeer suite's `session_config_with_topology(Mesh)`; the default
/// `SessionConfig.default_topology` is `Relay` (`src/config/session.rs:146`).
fn mesh_session_config() -> SessionConfig {
    SessionConfig {
        default_topology: Topology::Mesh,
        ..SessionConfig::default()
    }
}

/// Boot the production router around an allowlist-enabled in-memory server using the
/// DEFAULT (relay-floor) `SessionConfig`. Validation / room-lifecycle tests that
/// never finalize a non-relay session use this.
async fn start_server() -> (RunningTestServer, Arc<EnhancedGameServer>) {
    start_server_with_session(SessionConfig::default()).await
}

/// Boot the production router (`/v2` nest + `/v3/ws` alias) around an
/// allowlist-enabled in-memory server built with the given `SessionConfig`,
/// returning the bound address and the handle (mirrors
/// `tests/v3_signaling_e2e.rs` / `tests/v3_multipeer_e2e.rs`).
async fn start_server_with_session(
    session: SessionConfig,
) -> (RunningTestServer, Arc<EnhancedGameServer>) {
    use axum::routing::get;

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
        .route("/v3/ws", get(websocket_handler_v3))
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

/// Authenticate as a v3 client supporting relay + webrtc and every topology,
/// asserting v3 was negotiated (mirrors `authenticate_v3_full` in the multipeer
/// suite).
async fn authenticate_v3_full(ws: &mut WsStream) {
    send(
        ws,
        &ClientMessage::Authenticate {
            app_id: APP_ID.to_string(),
            sdk_version: None,
            platform: None,
            game_data_format: None,
            protocol_version: Some(3),
            supported_transports: Some(vec![Transport::Relay, Transport::WebRtc]),
            supported_topologies: Some(vec![Topology::Relay, Topology::Host, Topology::Mesh]),
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
                assert_eq!(info.protocol_version, Some(3), "must negotiate v3");
                Some(())
            }
            _ => None,
        }
    })
    .await;
}

/// Outcome of a `JoinRoom`: either the accepted payload or the rejection's
/// reason + error code.
enum JoinOutcome {
    Joined(Box<RoomJoinedPayload>),
    Failed {
        reason: String,
        error_code: Option<ErrorCode>,
    },
}

/// Drive a raw `JoinRoom` and return the FIRST join verdict (`RoomJoined` or
/// `RoomJoinFailed`) without panicking on rejection — the validation tests
/// assert on the rejection directly.
async fn join_room_outcome(
    ws: &mut WsStream,
    game_name: &str,
    room_code: Option<String>,
    player_name: &str,
    max_players: Option<u8>,
) -> JoinOutcome {
    send(
        ws,
        &ClientMessage::JoinRoom {
            game_name: game_name.to_string(),
            room_code,
            player_name: player_name.to_string(),
            max_players,
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await;

    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "join verdict", |message| {
        match message {
            ServerMessage::RoomJoined(payload) => Some(JoinOutcome::Joined(payload)),
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                Some(JoinOutcome::Failed { reason, error_code })
            }
            _ => None,
        }
    })
    .await
}

/// Join (or create) a room, panicking on rejection (the happy-path helper).
async fn join_room(
    ws: &mut WsStream,
    game_name: &str,
    room_code: Option<String>,
    player_name: &str,
    max_players: Option<u8>,
) -> Box<RoomJoinedPayload> {
    match join_room_outcome(ws, game_name, room_code, player_name, max_players).await {
        JoinOutcome::Joined(payload) => payload,
        JoinOutcome::Failed { reason, error_code } => {
            panic!("room join unexpectedly failed: {reason} ({error_code:?})")
        }
    }
}

// ---------------------------------------------------------------------------
// 1. N=1 solo room.
// ---------------------------------------------------------------------------

/// Under the explicit-start contract a solo (`max_players = 1`) v3 mesh+webrtc
/// room enters the lobby immediately (a room enters Lobby with >=1 player), so
/// the lone player may ready. A `StartGame` sent BEFORE the player is ready is
/// rejected `GAME_START_NOT_READY`; once the player readies, an explicit
/// `StartGame` finalizes the solo room (solo is allowed) and the player receives
/// `GameStarting`.
#[tokio::test(flavor = "multi_thread")]
async fn solo_room_max_players_1_starts_via_explicit_start_game() {
    let (running_server, _server) = start_server().await;
    let addr = running_server.addr();
    let game = "edge-solo1";

    let mut solo = connect(addr).await;
    authenticate_v3_full(&mut solo).await;
    let joined = join_room(&mut solo, game, None, "Solo", Some(1)).await;
    assert_eq!(joined.max_players, 1);

    // The room enters the lobby as soon as the creator joins (max_players is a
    // ceiling, not a required count): a LobbyStateChanged(Lobby) follows the
    // RoomJoined snapshot.
    next_matching_server_message_within(
        &mut solo,
        SERVER_MESSAGE_TIMEOUT,
        "solo promotion to Lobby",
        |message| match message {
            ServerMessage::LobbyStateChanged {
                lobby_state: LobbyState::Lobby,
                ..
            } => Some(()),
            _ => None,
        },
    )
    .await;

    // A StartGame before the lone player is ready is rejected: not every current
    // player is ready yet.
    start_game(&mut solo).await;
    next_matching_server_message_within(
        &mut solo,
        SERVER_MESSAGE_TIMEOUT,
        "pre-ready StartGame verdict",
        |message| match message {
            ServerMessage::Error { error_code, .. } => {
                assert_eq!(
                    error_code,
                    Some(ErrorCode::GameStartNotReady),
                    "a StartGame sent before the player readies must be GAME_START_NOT_READY"
                );
                Some(())
            }
            ServerMessage::GameStarting { .. } => {
                panic!("a not-all-ready StartGame must not finalize (no GameStarting)")
            }
            other => panic!("expected an Error for pre-ready StartGame, got {other:?}"),
        },
    )
    .await;

    // The lone player readies (allowed in a non-finalized room); the server
    // broadcasts a LobbyStateChanged reporting it ready.
    ready(&mut solo).await;
    next_matching_server_message_within(
        &mut solo,
        SERVER_MESSAGE_TIMEOUT,
        "solo ready count 1",
        |message| match message {
            ServerMessage::LobbyStateChanged { ready_players, .. } if ready_players.len() == 1 => {
                Some(())
            }
            _ => None,
        },
    )
    .await;

    // Now an explicit StartGame finalizes the solo room: the player receives
    // GameStarting (solo, min 1, is allowed).
    start_game(&mut solo).await;
    next_matching_server_message_within(
        &mut solo,
        SERVER_MESSAGE_TIMEOUT,
        "solo GameStarting",
        |message| match message {
            ServerMessage::GameStarting { .. } => Some(()),
            other => panic!("expected GameStarting after a solo StartGame, got {other:?}"),
        },
    )
    .await;
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 2. max_players boundary.
// ---------------------------------------------------------------------------

/// A 2-seat mesh+webrtc room finalizes normally: filling both seats promotes it
/// to Lobby, and both readies trigger `GameStarting` + a `SessionPlan` for each
/// member (the baseline this whole suite contrasts the N=1 case against).
#[tokio::test(flavor = "multi_thread")]
async fn max_players_2_room_finalizes_when_both_ready() {
    // Mesh-preferring server so the finalized v3+webrtc room yields a mesh
    // SessionPlan rather than the default explicit relay-floor plan.
    let (running_server, _server) = start_server_with_session(mesh_session_config()).await;
    let addr = running_server.addr();
    let game = "edge-boundary2";

    let mut peer_a = connect(addr).await;
    authenticate_v3_full(&mut peer_a).await;
    let joined_a = join_room(&mut peer_a, game, None, "PeerA", Some(2)).await;
    let room_code = joined_a.room_code;
    // NOTE (snapshot ordering): the creator's `RoomJoined` reply is built and
    // sent BEFORE the room is promoted to Lobby (the transition runs afterward),
    // so the creator's OWN `RoomJoined.lobby_state` is still `Waiting`. The room
    // enters Lobby EXACTLY ONCE — on this first (creator) join — observed via the
    // single own-join `LobbyStateChanged(Lobby)` asserted below.
    assert_eq!(
        joined_a.lobby_state,
        LobbyState::Waiting,
        "the creator's RoomJoined snapshot predates the Lobby promotion"
    );
    // The creator (and only the creator) receives the single own-join promotion
    // to Lobby, delivered as a LobbyStateChanged after its RoomJoined.
    next_matching_server_message_within(
        &mut peer_a,
        SERVER_MESSAGE_TIMEOUT,
        "creator own-join promotion to Lobby",
        |message| match message {
            ServerMessage::LobbyStateChanged {
                lobby_state: LobbyState::Lobby,
                ..
            } => Some(()),
            _ => None,
        },
    )
    .await;

    let mut peer_b = connect(addr).await;
    authenticate_v3_full(&mut peer_b).await;
    let joined_b = join_room(&mut peer_b, game, Some(room_code), "PeerB", Some(2)).await;
    // The room already entered Lobby on the creator's join, so a SUBSEQUENT
    // joiner's `RoomJoined.lobby_state` already reports `Lobby` and NO
    // `LobbyStateChanged` is broadcast on its join (existing members see only
    // `PlayerJoined`).
    assert_eq!(
        joined_b.lobby_state,
        LobbyState::Lobby,
        "a subsequent joiner's RoomJoined snapshot already reports the Lobby state"
    );

    // Paced readies. Readiness no longer auto-starts the room.
    ready(&mut peer_a).await;
    for socket in [&mut peer_a, &mut peer_b] {
        next_matching_server_message_within(
            socket,
            SERVER_MESSAGE_TIMEOUT,
            "lobby ready count 1",
            |message| match message {
                ServerMessage::LobbyStateChanged { ready_players, .. }
                    if ready_players.len() == 1 =>
                {
                    Some(())
                }
                _ => None,
            },
        )
        .await;
    }
    ready(&mut peer_b).await;
    for socket in [&mut peer_a, &mut peer_b] {
        next_matching_server_message_within(
            socket,
            SERVER_MESSAGE_TIMEOUT,
            "lobby ready count 2",
            |message| match message {
                ServerMessage::LobbyStateChanged { ready_players, .. }
                    if ready_players.len() == 2 =>
                {
                    Some(())
                }
                _ => None,
            },
        )
        .await;
    }

    // With both members ready, an explicit StartGame finalizes the room.
    start_game(&mut peer_a).await;

    // Both members get GameStarting (and, being v3+webrtc mesh, a SessionPlan).
    for (socket, who) in [(&mut peer_a, "peer_a"), (&mut peer_b, "peer_b")] {
        next_matching_server_message_within(
            socket,
            SERVER_MESSAGE_TIMEOUT,
            "GameStarting",
            |message| match message {
                ServerMessage::GameStarting { .. } => Some(()),
                ServerMessage::SessionPlan(_) => {
                    panic!("{who}: SessionPlan must not arrive before GameStarting")
                }
                _ => None,
            },
        )
        .await;
        next_matching_server_message_within(
            socket,
            SERVER_MESSAGE_TIMEOUT,
            "SessionPlan",
            |message| match message {
                ServerMessage::SessionPlan(plan) => {
                    assert_eq!(
                        plan.peers.len(),
                        1,
                        "{who}: a 2-peer plan lists exactly 1 peer"
                    );
                    Some(())
                }
                other => panic!("{who} expected SessionPlan, got {other:?}"),
            },
        )
        .await;
    }
    running_server.shutdown().await;
}

/// A solo create that OMITS `max_players` defaults to the server's
/// `default_max_players` (test config = 4) — NOT 1. Under the explicit-start
/// contract `max_players` is a ceiling, so the room enters the lobby immediately
/// with one player and readiness is ACCEPTED before the room is full. The lone
/// (and so only) member is then all-ready, so an explicit `StartGame` from it
/// finalizes the under-full room — `GameStarting` arrives.
#[tokio::test(flavor = "multi_thread")]
async fn solo_create_with_default_max_players_readies_and_starts_before_full() {
    let (running_server, _server) = start_server().await;
    let addr = running_server.addr();
    let game = "edge-default-cap";

    let mut creator = connect(addr).await;
    authenticate_v3_full(&mut creator).await;
    // Omit max_players entirely: the server applies default_max_players (4).
    let joined = join_room(&mut creator, game, None, "Creator", None).await;
    assert_eq!(
        joined.max_players, 4,
        "an omitted max_players must default to the server's default_max_players (test config = 4)"
    );

    // The room enters the lobby immediately (one player is enough; max_players is
    // a ceiling): a LobbyStateChanged(Lobby) follows the RoomJoined snapshot.
    next_matching_server_message_within(
        &mut creator,
        SERVER_MESSAGE_TIMEOUT,
        "promotion to Lobby on an under-full room",
        |message| match message {
            ServerMessage::LobbyStateChanged {
                lobby_state: LobbyState::Lobby,
                ..
            } => Some(()),
            _ => None,
        },
    )
    .await;

    // PlayerReady BEFORE the room is full is now ACCEPTED: the server broadcasts
    // a LobbyStateChanged reporting the lone member ready (no INVALID_ROOM_STATE).
    ready(&mut creator).await;
    next_matching_server_message_within(
        &mut creator,
        SERVER_MESSAGE_TIMEOUT,
        "under-full PlayerReady verdict",
        |message| match message {
            ServerMessage::LobbyStateChanged {
                ready_players,
                all_ready,
                ..
            } if ready_players.len() == 1 => {
                assert!(
                    all_ready,
                    "the lone member is the only current player, so all_ready is true"
                );
                Some(())
            }
            ServerMessage::Error { error_code, .. } => {
                panic!("PlayerReady before full must now be accepted, got error {error_code:?}")
            }
            _ => None,
        },
    )
    .await;

    // The lone member is all-ready, so an explicit StartGame finalizes the
    // under-full room (solo is allowed): GameStarting arrives.
    start_game(&mut creator).await;
    next_matching_server_message_within(
        &mut creator,
        SERVER_MESSAGE_TIMEOUT,
        "under-full GameStarting",
        |message| match message {
            ServerMessage::GameStarting { .. } => Some(()),
            other => {
                panic!("expected GameStarting after the lone member's StartGame, got {other:?}")
            }
        },
    )
    .await;
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 3. Player-name validation e2e.
// ---------------------------------------------------------------------------

/// Drive real `JoinRoom`s with edge-case player names and assert the server's
/// actual accept/reject + error code. The test protocol config sets
/// `max_player_name_length = 16` and the default name rules allow spaces and
/// unicode-alphanumerics but FORBID leading/trailing whitespace
/// (`src/config/defaults.rs:160-166`, `src/protocol/validation.rs:41-88`).
#[tokio::test(flavor = "multi_thread")]
async fn player_name_validation_accept_reject_matrix() {
    let (running_server, _server) = start_server().await;
    let addr = running_server.addr();
    let game = "edge-name";

    // Each case uses a FRESH connection + a distinct room so accepted names do
    // not collide on per-room name uniqueness and rejections cannot poison a
    // shared socket.
    async fn attempt(addr: std::net::SocketAddr, game: &str, name: &str) -> JoinOutcome {
        let mut ws = connect(addr).await;
        authenticate_v3_full(&mut ws).await;
        // Fresh room each time (a generated code) at a comfortable capacity.
        join_room_outcome(&mut ws, game, None, name, Some(4)).await
    }

    // (a) Empty name -> rejected with the dedicated `INVALID_PLAYER_NAME` code.
    //
    // A player-name failure carries the dedicated `INVALID_PLAYER_NAME` code
    // (src/protocol/error_codes.rs:29), the analogue of the `INVALID_GAME_NAME` /
    // `INVALID_ROOM_CODE` codes the sibling validators use, so a client switching
    // on error_code can distinguish a bad name from any other malformed input.
    match attempt(addr, game, "").await {
        JoinOutcome::Failed { error_code, .. } => assert_eq!(
            error_code,
            Some(ErrorCode::InvalidPlayerName),
            "an empty player name must be rejected with the dedicated INVALID_PLAYER_NAME"
        ),
        JoinOutcome::Joined(_) => panic!("an empty player name must be rejected"),
    }

    // (b) Whitespace-only name -> rejected (trim() is empty -> "cannot be
    // blank"). A whitespace-only name is NOT silently accepted.
    match attempt(addr, game, "     ").await {
        JoinOutcome::Failed { error_code, .. } => assert_eq!(
            error_code,
            Some(ErrorCode::InvalidPlayerName),
            "a whitespace-only player name must be rejected with INVALID_PLAYER_NAME"
        ),
        JoinOutcome::Joined(_) => {
            panic!("a whitespace-only player name must not be silently accepted")
        }
    }

    // (c) Max-length name (exactly 16 ASCII bytes) -> accepted.
    let max_name = "a".repeat(16);
    match attempt(addr, game, &max_name).await {
        JoinOutcome::Joined(payload) => assert_eq!(
            payload.current_players.len(),
            1,
            "a 16-byte name (exactly the max) must be accepted"
        ),
        JoinOutcome::Failed { reason, error_code } => {
            panic!("a 16-byte name (the max) must be accepted, got {reason} ({error_code:?})")
        }
    }

    // (d) Max+1 name (17 ASCII bytes) -> rejected with INVALID_PLAYER_NAME.
    let too_long = "a".repeat(17);
    match attempt(addr, game, &too_long).await {
        JoinOutcome::Failed { error_code, .. } => assert_eq!(
            error_code,
            Some(ErrorCode::InvalidPlayerName),
            "a 17-byte name (max+1) must be rejected with INVALID_PLAYER_NAME"
        ),
        JoinOutcome::Joined(_) => panic!("a 17-byte name (max+1) must be rejected"),
    }

    // (e) Unicode/emoji name. Length is checked in BYTES: a single emoji is 4
    // UTF-8 bytes and `is_alphanumeric()` is false for it, so under the default
    // rules (allow_unicode_alphanumeric=true, allowed_symbols=['-','_']) an
    // emoji is NOT an allowed character and the name is rejected. We assert the
    // ACTUAL behavior rather than presuming acceptance.
    let emoji = "🐟"; // 4 UTF-8 bytes, not alphanumeric, not an allowed symbol
    match attempt(addr, game, emoji).await {
        JoinOutcome::Failed { error_code, .. } => assert_eq!(
            error_code,
            Some(ErrorCode::InvalidPlayerName),
            "an emoji is neither alphanumeric nor an allowed symbol, so it is rejected \
             with INVALID_PLAYER_NAME"
        ),
        JoinOutcome::Joined(_) => {
            panic!("emoji acceptance changed: the default rules reject non-alphanumeric symbols")
        }
    }

    // A unicode ALPHANUMERIC name (allow_unicode_alphanumeric=true) IS accepted.
    match attempt(addr, game, "Ünïcödé").await {
        JoinOutcome::Joined(_) => {}
        JoinOutcome::Failed { reason, error_code } => panic!(
            "a unicode-alphanumeric name must be accepted under the default rules, got {reason} ({error_code:?})"
        ),
    }

    // (f) Leading/trailing spaces -> rejected (allow_leading_trailing_whitespace
    // defaults to false). The inner space is allowed; only the surrounding
    // whitespace trips the rule.
    match attempt(addr, game, " Padded Name ").await {
        JoinOutcome::Failed { error_code, .. } => assert_eq!(
            error_code,
            Some(ErrorCode::InvalidPlayerName),
            "leading/trailing whitespace must be rejected with INVALID_PLAYER_NAME \
             (allow_leading_trailing_whitespace=false)"
        ),
        JoinOutcome::Joined(_) => {
            panic!("a name with leading/trailing spaces must be rejected by default")
        }
    }

    // …but an INNER space alone is fine (allow_spaces defaults to true).
    match attempt(addr, game, "Padded Name").await {
        JoinOutcome::Joined(_) => {}
        JoinOutcome::Failed { reason, error_code } => panic!(
            "an internal space is allowed by default (allow_spaces=true), got {reason} ({error_code:?})"
        ),
    }
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 4. game_name validation e2e.
// ---------------------------------------------------------------------------

/// Drive real `JoinRoom`s with edge-case game names. The test protocol config
/// sets `max_game_name_length = 32`; the rules allow alphanumerics plus
/// `- _ <space>` only (`src/protocol/validation.rs:6-23`). A bad game name is
/// rejected with `INVALID_GAME_NAME`.
#[tokio::test(flavor = "multi_thread")]
async fn game_name_validation_accept_reject_matrix() {
    let (running_server, _server) = start_server().await;
    let addr = running_server.addr();

    async fn attempt(addr: std::net::SocketAddr, game: &str) -> JoinOutcome {
        let mut ws = connect(addr).await;
        authenticate_v3_full(&mut ws).await;
        join_room_outcome(&mut ws, game, None, "Namer", Some(4)).await
    }

    // (a) Empty game name -> rejected, INVALID_GAME_NAME.
    match attempt(addr, "").await {
        JoinOutcome::Failed { error_code, .. } => assert_eq!(
            error_code,
            Some(ErrorCode::InvalidGameName),
            "an empty game name must be rejected with INVALID_GAME_NAME"
        ),
        JoinOutcome::Joined(_) => panic!("an empty game name must be rejected"),
    }

    // (b) Too-long game name (33 bytes, max is 32) -> rejected.
    let too_long = "g".repeat(33);
    match attempt(addr, &too_long).await {
        JoinOutcome::Failed { error_code, .. } => assert_eq!(
            error_code,
            Some(ErrorCode::InvalidGameName),
            "a 33-byte game name (max+1) must be rejected with INVALID_GAME_NAME"
        ),
        JoinOutcome::Joined(_) => panic!("a 33-byte game name must be rejected"),
    }

    // (c) Invalid character (a '!' is not alphanumeric / '-' / '_' / space) ->
    // rejected.
    match attempt(addr, "bad!name").await {
        JoinOutcome::Failed { error_code, .. } => assert_eq!(
            error_code,
            Some(ErrorCode::InvalidGameName),
            "a game name with an invalid char must be rejected with INVALID_GAME_NAME"
        ),
        JoinOutcome::Joined(_) => panic!("a game name with '!' must be rejected"),
    }

    // (d) A 32-byte name with only allowed chars (letters, digits, '-', '_',
    // space) IS accepted — the boundary length and the full allowed-char set.
    let ok = format!("{}-_ 1", "g".repeat(27)); // 27 + 4 ("-_ 1") = exactly 31 bytes
    assert!(ok.len() <= 32);
    match attempt(addr, &ok).await {
        JoinOutcome::Joined(_) => {}
        JoinOutcome::Failed { reason, error_code } => panic!(
            "an in-bounds game name with allowed chars must be accepted, got {reason} ({error_code:?})"
        ),
    }
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 5. Room-code case-insensitivity.
// ---------------------------------------------------------------------------

/// A lowercase explicit room_code at create-time is upper-cased
/// (`src/server/room_service.rs:150`); joins that supply the uppercase OR a
/// mixed-case spelling of the same code therefore land in the SAME room. This
/// verifies the case-normalization claim end to end. Test config sets
/// `room_code_length = 4`.
#[tokio::test(flavor = "multi_thread")]
async fn room_code_case_insensitive_join_lands_in_same_room() {
    let (running_server, _server) = start_server().await;
    let addr = running_server.addr();
    let game = "edge-case-code";

    // Player A creates with an explicit LOWERCASE 4-char code; capacity 3 keeps
    // the room open for B and C.
    let mut peer_a = connect(addr).await;
    authenticate_v3_full(&mut peer_a).await;
    let joined_a = join_room(
        &mut peer_a,
        game,
        Some("abcd".to_string()),
        "PeerA",
        Some(3),
    )
    .await;
    let room_id = joined_a.room_id;
    assert_eq!(
        joined_a.room_code, "ABCD",
        "the explicit lowercase code is normalized to uppercase on create"
    );

    // Player B joins with the UPPERCASE spelling.
    let mut peer_b = connect(addr).await;
    authenticate_v3_full(&mut peer_b).await;
    let joined_b = join_room(
        &mut peer_b,
        game,
        Some("ABCD".to_string()),
        "PeerB",
        Some(3),
    )
    .await;
    assert_eq!(
        joined_b.room_id, room_id,
        "an uppercase join must land in the same room as the lowercase create"
    );
    assert_eq!(joined_b.room_code, "ABCD");

    // Player C joins with a MIXED-case spelling.
    let mut peer_c = connect(addr).await;
    authenticate_v3_full(&mut peer_c).await;
    let joined_c = join_room(
        &mut peer_c,
        game,
        Some("aBcD".to_string()),
        "PeerC",
        Some(3),
    )
    .await;
    assert_eq!(
        joined_c.room_id, room_id,
        "a mixed-case join must land in the same room (codes are case-normalized)"
    );
    assert_eq!(joined_c.room_code, "ABCD");

    // All three are now members of the single room: the third join filled the
    // 3-seat room, so its RoomJoined reports all 3 current players.
    assert_eq!(
        joined_c.current_players.len(),
        3,
        "all three case spellings resolved to one room with three members"
    );
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// 6. Empty-room cleanup.
// ---------------------------------------------------------------------------

/// A creator leaving its only room empties it. Empty-room reclamation is
/// timeout-driven (the background cleanup task, `empty_room_timeout`), NOT
/// synchronous on `LeaveRoom`, so a re-join with the SAME code immediately
/// after leaving rejoins the LINGERING room (same `room_id`) — it does NOT
/// create a fresh one. This pins the real contract rather than the presumed
/// "leave => fresh room" behavior.
#[tokio::test(flavor = "multi_thread")]
async fn empty_room_after_leave_is_rejoined_not_recreated_immediately() {
    let (running_server, _server) = start_server().await;
    let addr = running_server.addr();
    let game = "edge-cleanup";

    let mut creator = connect(addr).await;
    authenticate_v3_full(&mut creator).await;
    let joined = join_room(
        &mut creator,
        game,
        Some("zzzz".to_string()),
        "Creator",
        Some(2),
    )
    .await;
    let original_room_id = joined.room_id;

    // The creator leaves, emptying the room. The room entered the lobby on
    // create (one player is enough now), so a LobbyStateChanged(Lobby) precedes
    // the RoomLeft — skip it and assert the RoomLeft below.
    send(&mut creator, &ClientMessage::LeaveRoom).await;
    next_matching_server_message_within(
        &mut creator,
        SERVER_MESSAGE_TIMEOUT,
        "RoomLeft",
        |message| match message {
            ServerMessage::RoomLeft => Some(()),
            ServerMessage::LobbyStateChanged { .. } => None,
            other => panic!("expected RoomLeft, got {other:?}"),
        },
    )
    .await;

    // Re-join the SAME code immediately on a fresh connection. Because cleanup
    // is asynchronous, the empty room still exists and is rejoined under the
    // SAME room_id (no fresh room is minted on the leave itself).
    let mut rejoiner = connect(addr).await;
    authenticate_v3_full(&mut rejoiner).await;
    let rejoined = join_room(
        &mut rejoiner,
        game,
        Some("ZZZZ".to_string()),
        "Rejoiner",
        Some(2),
    )
    .await;

    assert_eq!(
        rejoined.room_id, original_room_id,
        "an immediate same-code re-join rejoins the lingering empty room (cleanup is async, \
         not synchronous on leave)"
    );
    // The lingering room had no members after the leave, so the re-joiner is the
    // sole occupant again.
    assert_eq!(
        rejoined.current_players.len(),
        1,
        "the re-joiner is the only member of the reclaimed-on-rejoin empty room"
    );
    // The lingering room already entered Lobby on the original creator's join and
    // PERSISTS that state (the transition is idempotent, and a departure does not
    // regress a lobby to Waiting), so the re-joiner's RoomJoined snapshot reports
    // the persisted `Lobby` state.
    assert_eq!(rejoined.lobby_state, LobbyState::Lobby);
    running_server.shutdown().await;
}
