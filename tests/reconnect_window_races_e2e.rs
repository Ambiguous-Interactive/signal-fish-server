//! **H6** — reconnect window / claim edge races (falsification experiment).
//!
//! Falsifies two deterministic edges of the reconnection state machine through
//! the real WebSocket stack. H6's third edge (reconnect-during-teardown) is
//! covered at the handler/unregister boundary by the deterministic test-only
//! gate in
//! `server::signaling_tests::reconnect_during_teardown_preserves_token_for_retry`.
//!
//! # Reconnect-window race contract
//!
//! > Boundary (inside/outside window, independent of cleanup tick — prove with
//! > cleanup interval 3600s) and duplicate-claim (exactly one winner) pass;
//! > reconnect-during-teardown requires client retry (`PlayerAlreadyConnected`;
//! > token NOT consumed) → F3 SDK rule.
//!
//! Prediction for all three facets: **PASS.**
//!
//! # Facet 1 — window boundary, independent of the cleanup tick
//!
//! The reconnection window is enforced by the *reconnect handler itself*
//! (`ReconnectionManager::claim_reconnection`, which checks the token's embedded
//! expiry and `DisconnectedPlayer::is_expired`), NOT by the background cleanup
//! task deleting the record. To prove that, every server here runs the REAL
//! `cleanup_task` but with `room_cleanup_interval` set to 3600s: `tokio`'s
//! interval fires one immediate startup tick (harmless — nothing is pending
//! yet) and then not again for an hour, so no reconnection record can be
//! deleted during a few-second test. With the record provably still present
//! (asserted via `has_pending_reconnection`), a post-window reconnect that is
//! rejected can only have been rejected by the window/token check.
//!
//! - Inside the window: reconnect IMMEDIATELY (no sleep) ⇒ `Reconnected`.
//! - After the window: sleep past it ⇒ `ReconnectionFailed` with a
//!   time-expiry code (`ReconnectionExpired` or `ReconnectionTokenInvalid`),
//!   NOT a `NoRecord`/`RoomNotFound`.
//!
//! # Facet 2 — duplicate claim, exactly one winner
//!
//! Two fresh, handshake-complete sockets contend for the SAME token/player
//! concurrently (released together by a barrier on a multi-thread runtime).
//! The claim is lock-serialized in `claim_reconnection`, so the OUTCOME COUNT
//! is deterministic: EXACTLY ONE gets `Reconnected` and the other gets a
//! failure. We assert the counts, never which socket wins.
//!
//! # Zero-flakiness design (the whole risk of H6)
//!
//! The window uses `chrono::Utc` wall-clock, not `tokio` time, so real time is
//! required. That is safe here only where a sleep can merely OVERSHOOT the
//! assertion:
//! - Outside-window leg sleeps 2× the window, then asserts EXPIRED — more
//!   elapsed time only makes it "more expired", so it cannot flake into "not
//!   yet expired". (A standard `#[tokio::test]` runtime is not time-paused, so
//!   `tokio::time::sleep` elapses real wall-clock time.)
//! - Inside-window leg reconnects IMMEDIATELY after pre-authenticating the
//!   replacement socket, so the only work between `disconnected_at` and the
//!   claim is one frame send + server processing (sub-millisecond on loopback)
//!   against a 2s window — a ~1000× margin, so "still inside" cannot flake.
//! - No negative "not yet expired after a fixed sleep" assertion is ever made.
//! - The duplicate-claim outcome is deterministic by construction (lock
//!   serialization), independent of timing.
//!
//! Non-vacuity: the inside leg (immediate ⇒ success) and the outside leg
//! (sleep-past ⇒ failure) share an IDENTICAL server config and both keep the
//! record present (cleanup disabled), so the ONLY variable flipping the outcome
//! is elapsed-time-past-window — i.e. the window check is load-bearing. This
//! was additionally confirmed empirically during development by temporarily
//! widening the window to 3600s, after which the very same post-sleep call
//! returns `Reconnected` instead of `ReconnectionFailed` (reverted).
//!
//! # Facet 3 — reconnect during teardown
//!
//! A `cfg(test)` gate pauses the real unregister transaction after it arms the
//! pending record and before it removes the old connection. A replacement then
//! receives `PlayerAlreadyConnected`; direct manager validation proves the
//! token remains valid. Releasing the gate completes teardown, and the same
//! replacement's successful use of that token for `Reconnected` proves it was
//! not consumed. The gate replaces the former timing-narrow race with explicit
//! events and no sleeps.
//!
//! # Result
//!
//! CONFIRMED — all three facets pass; prediction upheld. (Observed detail: the
//! post-window rejection surfaces as `ReconnectionTokenInvalid`, because the
//! token's own embedded expiry — armed to `disconnect + window` — is checked a
//! hair before the dedicated `WindowExpired` gate.)

mod test_helpers;
mod websocket_test_helpers;

use futures_util::SinkExt;
use signal_fish_server::config::AppRegistrationEntry;
use signal_fish_server::protocol::{
    ClientMessage, ErrorCode, PlayerId, ReconnectedPayload, RoomId, ServerMessage,
};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::{create_router, websocket_route_v3};
use std::sync::Arc;
use std::time::Duration;
use test_helpers::{test_protocol_config, test_server_config, RunningTestServer};
use tokio::sync::Barrier;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::{
    assert_message_conservation, next_matching_server_message_within, next_server_message_within,
    WsStream,
};

const APP_ID: &str = "reconnect-window-races-app";
const SERVER_MESSAGE_TIMEOUT: Duration = Duration::from_secs(20);

/// Small reconnection window for the boundary tests: whole seconds, because the
/// server passes `reconnection_window.as_secs()` to the manager (a sub-second
/// `Duration` would truncate to 0). Small enough to keep the test fast, large
/// enough that an immediate inside-window reconnect cannot lose the race.
const WINDOW: Duration = Duration::from_secs(2);
/// Overshoot the window by 2× — the robust direction (more elapsed ⇒ still
/// expired), never a boundary-grazing "just past" sleep.
const PAST_WINDOW_SLEEP: Duration = Duration::from_secs(4);
/// Cleanup interval so large the `cleanup_task` never fires again after its
/// immediate startup tick, so no reconnection record is deleted during a test.
const HUGE_CLEANUP_INTERVAL: Duration = Duration::from_secs(3600);
/// A comfortably large window for the duplicate-claim facet: that facet is
/// about the lock, not timing, so both attempts must be well inside the window.
const DUPLICATE_CLAIM_WINDOW: Duration = Duration::from_secs(300);

fn app_entry() -> AppRegistrationEntry {
    AppRegistrationEntry {
        app_id: APP_ID.to_string(),
        app_name: "Reconnect Window Races App".to_string(),
        max_rooms: Some(10),
        max_players_per_room: Some(8),
        rate_limit_per_minute: Some(600),
    }
}

/// Build a v3-capable server with the given reconnection `window` and
/// `cleanup_interval`, spawn the REAL room-cleanup task (so the window boundary
/// is proven with a live cleanup task present, not merely absent), start
/// serving, and return the address plus the handle for direct manager
/// inspection (`has_pending_reconnection`).
async fn start_server(
    window: Duration,
    cleanup_interval: Duration,
) -> (RunningTestServer, Arc<EnhancedGameServer>) {
    let mut server_config: ServerConfig = test_server_config();
    server_config.app_id_allowlist_enabled = true;
    server_config.reconnection_window = window;
    server_config.room_cleanup_interval = cleanup_interval;

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

    // Live cleanup task: with a 3600s interval it fires one immediate startup
    // tick (nothing pending yet ⇒ harmless) and then not again for an hour, so
    // any reconnection record registered during the test survives untouched —
    // any later reconnect rejection is the handler's window check, not deletion.
    let cleanup_server = game_server.clone();
    tokio::spawn(async move {
        cleanup_server.cleanup_task().await;
    });

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
    let (ws, _) = tokio::time::timeout(Duration::from_secs(10), connect_async(&url))
        .await
        .expect("connect timeout")
        .expect("connect");
    ws
}

async fn send(ws: &mut WsStream, msg: &ClientMessage) {
    let json = serde_json::to_string(msg).unwrap();
    ws.send(Message::Text(json.into())).await.unwrap();
}

/// Authenticate as a v3 client (mirrors `tests/reconnection_replay_e2e.rs`):
/// `Authenticated` then `ProtocolInfo`.
async fn authenticate_v3(ws: &mut WsStream) {
    send(
        ws,
        &ClientMessage::Authenticate {
            app_id: APP_ID.to_string(),
            sdk_version: None,
            platform: None,
            game_data_format: None,
            protocol_version: Some(3),
            supported_transports: None,
            supported_topologies: None,
        },
    )
    .await;

    let authed = next_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "Authenticated").await;
    assert!(
        matches!(authed, ServerMessage::Authenticated { .. }),
        "expected Authenticated, got {authed:?}"
    );
    let info = next_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "ProtocolInfo").await;
    assert!(
        matches!(info, ServerMessage::ProtocolInfo(_)),
        "expected ProtocolInfo, got {info:?}"
    );
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
/// (mirrors `tests/reconnection_replay_e2e.rs`). After this returns the
/// server has removed the client, so `has_client(player_id)` is false and a
/// fresh socket may immediately claim the token.
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
        .register_disconnection(player_id, room_id, false, Some(player_info), 0)
        .await
}

/// The two terminal outcomes of a `Reconnect` attempt on the wire.
enum ReconnectOutcome {
    Reconnected(Box<ReconnectedPayload>),
    Failed(ErrorCode),
}

async fn send_reconnect(
    ws: &mut WsStream,
    player_id: PlayerId,
    room_id: RoomId,
    auth_token: String,
) {
    send(
        ws,
        &ClientMessage::Reconnect {
            player_id,
            room_id,
            auth_token,
        },
    )
    .await;
}

/// Read the next `Reconnected` / `ReconnectionFailed` frame as a typed outcome.
async fn next_reconnect_outcome(ws: &mut WsStream, context: &str) -> ReconnectOutcome {
    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        context,
        |message| match message {
            ServerMessage::Reconnected(payload) => Some(ReconnectOutcome::Reconnected(payload)),
            ServerMessage::ReconnectionFailed { error_code, .. } => {
                Some(ReconnectOutcome::Failed(error_code))
            }
            _ => None,
        },
    )
    .await
}

/// One contender in the duplicate-claim race: park on the barrier, then send
/// and read its own outcome, so both attempts are released together.
async fn contended_reconnect(
    mut ws: WsStream,
    player_id: PlayerId,
    room_id: RoomId,
    token: String,
    barrier: Arc<Barrier>,
    context: &'static str,
) -> ReconnectOutcome {
    barrier.wait().await;
    send_reconnect(&mut ws, player_id, room_id, token).await;
    next_reconnect_outcome(&mut ws, context).await
}

// ---------------------------------------------------------------------------
// Facet 1a — a reconnect well inside the window succeeds.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reconnect_inside_window_succeeds() {
    let (running_server, game_server) = start_server(WINDOW, HUGE_CLEANUP_INTERVAL).await;
    let addr = running_server.addr();

    let mut anchor = connect(addr).await;
    authenticate_v3(&mut anchor).await;
    let anchor_room = join_room(&mut anchor, "window-game", None, "Anchor").await;

    let mut dropper = connect(addr).await;
    authenticate_v3(&mut dropper).await;
    let dropped = join_room(
        &mut dropper,
        "window-game",
        Some(anchor_room.room_code.clone()),
        "Dropper",
    )
    .await;

    // Pre-authenticate the replacement BEFORE the disconnection clock starts, so
    // the only work between `disconnected_at` and the claim is one frame send.
    let mut replacement = connect(addr).await;
    authenticate_v3(&mut replacement).await;

    let token = register_reconnect_token(&game_server, dropped.player_id, dropped.room_id).await;

    // Immediate (no sleep): microseconds into a 2s window ⇒ still inside.
    send_reconnect(&mut replacement, dropped.player_id, dropped.room_id, token).await;

    match next_reconnect_outcome(&mut replacement, "inside-window reconnect").await {
        ReconnectOutcome::Reconnected(payload) => {
            assert_eq!(
                payload.player_id, dropped.player_id,
                "the restored socket must own the original player id"
            );
            assert_eq!(
                payload.room_id, dropped.room_id,
                "the restored socket must rejoin the original room"
            );
        }
        ReconnectOutcome::Failed(error_code) => {
            panic!("a reconnect well inside the window must succeed, got ReconnectionFailed({error_code:?})")
        }
    }

    drop(dropper);
    assert_message_conservation(&game_server.metrics()).await;
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// Facet 1b — after the window elapses the reconnect is rejected by the handler,
// NOT because the cleanup task deleted the record (cleanup is at 3600s).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reconnect_after_window_expires_is_rejected_by_the_handler_not_cleanup() {
    let (running_server, game_server) = start_server(WINDOW, HUGE_CLEANUP_INTERVAL).await;
    let addr = running_server.addr();

    let mut anchor = connect(addr).await;
    authenticate_v3(&mut anchor).await;
    let anchor_room = join_room(&mut anchor, "window-game", None, "Anchor").await;

    let mut dropper = connect(addr).await;
    authenticate_v3(&mut dropper).await;
    let dropped = join_room(
        &mut dropper,
        "window-game",
        Some(anchor_room.room_code.clone()),
        "Dropper",
    )
    .await;

    let token = register_reconnect_token(&game_server, dropped.player_id, dropped.room_id).await;
    drop(dropper);

    let manager = game_server
        .reconnection_manager()
        .expect("reconnection enabled");

    // Sleep PAST the window (2× overshoot; real wall-clock — the runtime is not
    // time-paused). "More time elapsed ⇒ still expired", so this cannot flake.
    tokio::time::sleep(PAST_WINDOW_SLEEP).await;

    // The record is STILL present: the 3600s cleanup task has not fired again
    // since its startup tick, so nothing deleted it during the sleep. Whatever
    // rejects the reconnect below is therefore the handler's window/token check,
    // never a NoRecord from deletion — the crux of the H6 boundary claim.
    assert!(
        manager.has_pending_reconnection(&dropped.player_id).await,
        "with cleanup at 3600s the expired record must persist — proving the window is \
         enforced by the reconnect handler, not by the cleanup task deleting the record"
    );

    let mut replacement = connect(addr).await;
    authenticate_v3(&mut replacement).await;
    send_reconnect(&mut replacement, dropped.player_id, dropped.room_id, token).await;

    match next_reconnect_outcome(&mut replacement, "post-window reconnect").await {
        ReconnectOutcome::Reconnected(_) => {
            panic!("a reconnect after the window elapsed must be rejected")
        }
        ReconnectOutcome::Failed(error_code) => {
            assert!(
                matches!(
                    error_code,
                    ErrorCode::ReconnectionExpired | ErrorCode::ReconnectionTokenInvalid
                ),
                "the rejection must be a window/token time-expiry code (NOT a NoRecord or \
                 RoomNotFound, which would mean the record was deleted), got {error_code:?}"
            );
        }
    }

    // A rejected claim never consumes the record — reconfirming the failure was
    // a validation reject, not deletion.
    assert!(
        manager.has_pending_reconnection(&dropped.player_id).await,
        "a window/token rejection must leave the pending record intact"
    );

    assert_message_conservation(&game_server.metrics()).await;
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// Facet 2 — two concurrent same-token claims: EXACTLY one winner.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_duplicate_reconnect_has_exactly_one_winner() {
    let (running_server, game_server) =
        start_server(DUPLICATE_CLAIM_WINDOW, HUGE_CLEANUP_INTERVAL).await;
    let addr = running_server.addr();

    let mut anchor = connect(addr).await;
    authenticate_v3(&mut anchor).await;
    let anchor_room = join_room(&mut anchor, "claim-game", None, "Anchor").await;

    let mut dropper = connect(addr).await;
    authenticate_v3(&mut dropper).await;
    let dropped = join_room(
        &mut dropper,
        "claim-game",
        Some(anchor_room.room_code.clone()),
        "Dropper",
    )
    .await;

    let token = register_reconnect_token(&game_server, dropped.player_id, dropped.room_id).await;
    drop(dropper);

    // Two fresh, handshake-complete sockets, released together by the barrier so the
    // two claims genuinely contend on a multi-thread runtime.
    let mut first = connect(addr).await;
    authenticate_v3(&mut first).await;
    let mut second = connect(addr).await;
    authenticate_v3(&mut second).await;

    let barrier = Arc::new(Barrier::new(2));
    let task_first = tokio::spawn(contended_reconnect(
        first,
        dropped.player_id,
        dropped.room_id,
        token.clone(),
        Arc::clone(&barrier),
        "first duplicate claim",
    ));
    let task_second = tokio::spawn(contended_reconnect(
        second,
        dropped.player_id,
        dropped.room_id,
        token,
        Arc::clone(&barrier),
        "second duplicate claim",
    ));

    let outcomes = [
        task_first.await.expect("first claim task panicked"),
        task_second.await.expect("second claim task panicked"),
    ];

    // Assert the COUNTS, not which socket won (the claim is lock-serialized, so
    // exactly one wins — but WHICH one is a genuine race we must not pin).
    let winners = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, ReconnectOutcome::Reconnected(_)))
        .count();
    let losers = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, ReconnectOutcome::Failed(_)))
        .count();

    assert_eq!(
        winners, 1,
        "exactly one concurrent same-token claim may win"
    );
    assert_eq!(
        losers, 1,
        "the other concurrent same-token claim must be rejected"
    );

    assert_message_conservation(&game_server.metrics()).await;
    running_server.shutdown().await;
}
