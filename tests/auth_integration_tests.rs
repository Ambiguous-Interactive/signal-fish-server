//! Integration tests for the in-memory public app-ID allowlist
//!
//! Tests end-to-end app identification behavior via the server and the standalone
//! `AppIdAllowlist` API surface.

mod test_helpers;
mod websocket_test_helpers;

use futures_util::SinkExt;
use signal_fish_server::auth::{AppIdAllowlist, AuthError};
use signal_fish_server::config::AppRegistrationEntry;
use signal_fish_server::protocol::{ClientMessage, ErrorCode, ServerMessage};
use signal_fish_server::websocket::create_router;
use std::time::Duration;
use test_helpers::{create_test_server_with_config, test_server_config, RunningTestServer};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::{next_server_message_within, WsStream};

const SOCKET_DEADLINE: Duration = Duration::from_secs(10);

/// Default resolution source for tests not exercising the per-source
/// dimension; distinct sources are spelled out where it matters.
fn source(index: u32) -> std::net::IpAddr {
    let octet = u8::try_from(index + 1).expect("test source index fits u8");
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(198, 51, 100, octet))
}

// ---------------------------------------------------------------------------
// Helper factories
// ---------------------------------------------------------------------------

fn sample_app_entry() -> AppRegistrationEntry {
    AppRegistrationEntry {
        app_id: "test-game-1".to_string(),
        app_name: "Test Game".to_string(),
        max_rooms: Some(50),
        max_players_per_room: Some(8),
        rate_limit_per_minute: Some(60),
    }
}

fn secondary_app_entry() -> AppRegistrationEntry {
    AppRegistrationEntry {
        app_id: "test-game-2".to_string(),
        app_name: "Secondary Game".to_string(),
        max_rooms: None,
        max_players_per_room: None,
        rate_limit_per_minute: None,
    }
}

fn rate_limited_app_entry(limit: u32) -> AppRegistrationEntry {
    AppRegistrationEntry {
        app_id: "rate-limited-app".to_string(),
        app_name: "Rate Limited App".to_string(),
        max_rooms: Some(10),
        max_players_per_room: Some(4),
        rate_limit_per_minute: Some(limit),
    }
}

async fn send_client_message(ws: &mut WsStream, message: &ClientMessage) {
    let json = serde_json::to_string(message).expect("client message serializes");
    ws.send(Message::Text(json.into()))
        .await
        .expect("client message sends");
}

async fn connect_with_public_app_id(addr: std::net::SocketAddr, app_id: &str) -> WsStream {
    let url = format!("ws://{addr}/ws");
    let (mut ws, _) = tokio::time::timeout(SOCKET_DEADLINE, connect_async(url))
        .await
        .expect("WebSocket connect timed out")
        .expect("WebSocket connects");
    send_client_message(
        &mut ws,
        &ClientMessage::Authenticate {
            app_id: app_id.to_string(),
            sdk_version: None,
            platform: None,
            game_data_format: None,
            protocol_version: Some(2),
            supported_transports: None,
            supported_topologies: None,
            requested_capabilities: None,
        },
    )
    .await;
    assert!(matches!(
        next_server_message_within(&mut ws, SOCKET_DEADLINE, "app-ID accepted").await,
        ServerMessage::Authenticated { .. }
    ));
    assert!(matches!(
        next_server_message_within(&mut ws, SOCKET_DEADLINE, "protocol negotiated").await,
        ServerMessage::ProtocolInfo(_)
    ));
    ws
}

// ===========================================================================
// AppIdAllowlist unit-level integration tests
// ===========================================================================

#[tokio::test]
async fn test_registered_app_id_resolves_its_context() {
    let mw = AppIdAllowlist::new(vec![sample_app_entry(), secondary_app_entry()])
        .expect("unique app IDs");

    let info = mw
        .resolve_app_id("test-game-1", source(0))
        .await
        .expect("allowed app ID should resolve");

    assert_eq!(info.name, "Test Game");
    assert_eq!(info.max_rooms, Some(50));
    assert_eq!(info.max_players_per_room, Some(8));
    assert_eq!(info.rate_limit_per_minute, Some(60));
}

#[tokio::test]
async fn test_secondary_registered_app_id_resolves_its_context() {
    let mw = AppIdAllowlist::new(vec![sample_app_entry(), secondary_app_entry()])
        .expect("unique app IDs");

    let info = mw
        .resolve_app_id("test-game-2", source(0))
        .await
        .expect("secondary allowed app ID should resolve");

    assert_eq!(info.name, "Secondary Game");
    assert_eq!(info.max_rooms, None);
    assert_eq!(info.max_players_per_room, None);
    assert_eq!(info.rate_limit_per_minute, None);
}

#[tokio::test]
async fn test_public_app_id_can_be_replayed() {
    let mw = AppIdAllowlist::new(vec![sample_app_entry()]).expect("unique app IDs");

    let (first, replay) = tokio::join!(
        mw.resolve_app_id("test-game-1", source(0)),
        mw.resolve_app_id("test-game-1", source(0))
    );
    let first = first.expect("first public-ID use resolves");
    let replay = replay.expect("concurrent public-ID replay resolves");

    assert_eq!(first.id, replay.id);
    assert_eq!(first.name, replay.name);
}

#[tokio::test]
async fn test_reject_unknown_app_id() {
    let mw = AppIdAllowlist::new(vec![sample_app_entry()]).expect("unique app IDs");

    let err = mw
        .resolve_app_id("nonexistent-app", source(0))
        .await
        .expect_err("unknown app_id should fail");

    assert!(
        matches!(err, AuthError::InvalidAppId),
        "expected InvalidAppId, got: {err:?}"
    );
}

#[tokio::test]
async fn test_resolve_app_id_only() {
    let mw = AppIdAllowlist::new(vec![sample_app_entry()]).expect("unique app IDs");

    let info = mw
        .resolve_app_id("test-game-1", source(0))
        .await
        .expect("valid app_id should succeed");

    assert_eq!(info.name, "Test Game");
}

// ===========================================================================
// Rate limiting via AppIdAllowlist
// ===========================================================================

#[tokio::test]
async fn test_rate_limiting_enforced() {
    let limit = 5u32;
    let mw = AppIdAllowlist::new(vec![rate_limited_app_entry(limit)]).expect("unique app IDs");

    // Distinct sources exhaust the application-wide ceiling of 5.
    for i in 0..limit {
        let result = mw.resolve_app_id("rate-limited-app", source(i)).await;
        assert!(
            result.is_ok(),
            "request {i} of {limit} should succeed, got: {result:?}"
        );
    }

    // The next source is rejected by the app ceiling.
    let err = mw
        .resolve_app_id("rate-limited-app", source(limit))
        .await
        .expect_err("should be rate limited after exceeding per-minute cap");

    assert!(
        matches!(err, AuthError::RateLimitExceeded),
        "expected RateLimitExceeded, got: {err:?}"
    );
}

/// Pins the per-source share at the public API level (issue #502): one source
/// is bounded to half the configured app budget (min 1), and its rejections
/// leave the rest of the app budget available to other sources.
#[tokio::test]
async fn test_rate_limiting_bounds_one_source_to_its_share() {
    let limit = 5u32;
    let share = (limit / 2).max(1);
    let mw = AppIdAllowlist::new(vec![rate_limited_app_entry(limit)]).expect("unique app IDs");

    // One source spends exactly its share, then is rejected.
    for i in 0..share {
        assert!(
            mw.resolve_app_id("rate-limited-app", source(0))
                .await
                .is_ok(),
            "same-source request {i} of {share} should succeed"
        );
    }
    let err = mw
        .resolve_app_id("rate-limited-app", source(0))
        .await
        .expect_err("one source must not exceed its share of the app budget");
    assert!(
        matches!(err, AuthError::RateLimitExceeded),
        "expected RateLimitExceeded, got: {err:?}"
    );

    // The rejected attempts consumed no application-wide budget: other
    // sources can still spend the untouched remainder (5 - 2 = 3).
    for i in 0..(limit - share) {
        assert!(
            mw.resolve_app_id("rate-limited-app", source(i + 1))
                .await
                .is_ok(),
            "distinct source {i} should be admitted despite the exhausted abuser"
        );
    }
}

#[tokio::test]
async fn test_no_rate_limiting_when_not_configured() {
    let mw = AppIdAllowlist::new(vec![secondary_app_entry()]).expect("unique app IDs");

    // Should succeed many times without hitting a limit
    for _ in 0..200 {
        assert!(mw.resolve_app_id("test-game-2", source(0)).await.is_ok());
    }
}

#[tokio::test]
async fn test_rate_limits_are_per_app() {
    let entries = vec![
        rate_limited_app_entry(2),
        AppRegistrationEntry {
            app_id: "other-limited-app".to_string(),
            app_name: "Other App".to_string(),
            max_rooms: None,
            max_players_per_room: None,
            rate_limit_per_minute: Some(2),
        },
    ];
    let mw = AppIdAllowlist::new(entries).expect("unique app IDs");

    // Exhaust rate limit for first app (2 admissions across distinct sources;
    // a third source tips the app window).
    for i in 0..2 {
        mw.resolve_app_id("rate-limited-app", source(i))
            .await
            .unwrap();
    }
    assert!(mw
        .resolve_app_id("rate-limited-app", source(9))
        .await
        .is_err());

    // Second app should still be fine
    assert!(mw
        .resolve_app_id("other-limited-app", source(0))
        .await
        .is_ok());
}

// ===========================================================================
// Open app-ID policy
// ===========================================================================

#[tokio::test]
async fn test_open_policy_accepts_any_app_id() {
    let mw = AppIdAllowlist::disabled();

    let info = mw
        .resolve_app_id("anything", source(0))
        .await
        .expect("open policy should accept any app ID");

    assert_eq!(info.name, "default");
}

#[tokio::test]
async fn test_open_policy_returns_legacy_default_rate_limits() {
    let mw = AppIdAllowlist::disabled();

    let info = mw.resolve_app_id("x", source(0)).await.unwrap();

    assert_eq!(info.rate_limits.per_minute, 1000);
    assert_eq!(info.rate_limits.per_hour, 10000);
    assert_eq!(info.rate_limits.per_day, 100_000);
}

// ===========================================================================
// AppContext field assertions
// ===========================================================================

#[tokio::test]
async fn test_app_context_rate_limits_are_computed_correctly() {
    let entry = AppRegistrationEntry {
        app_id: "computed-limits".to_string(),
        app_name: "Computed".to_string(),
        max_rooms: None,
        max_players_per_room: None,
        rate_limit_per_minute: Some(10),
    };
    let mw = AppIdAllowlist::new(vec![entry]).expect("unique app IDs");

    let info = mw
        .resolve_app_id("computed-limits", source(0))
        .await
        .unwrap();

    assert_eq!(info.rate_limits.per_minute, 10);
    assert_eq!(info.rate_limits.per_hour, 600); // 10 * 60
    assert_eq!(info.rate_limits.per_day, 14400); // 10 * 60 * 24
}

#[tokio::test]
async fn test_deterministic_uuid_for_same_app_id() {
    let mw = AppIdAllowlist::new(vec![sample_app_entry()]).expect("unique app IDs");

    let info1 = mw.resolve_app_id("test-game-1", source(0)).await.unwrap();
    let info2 = mw.resolve_app_id("test-game-1", source(0)).await.unwrap();

    assert_eq!(
        info1.id, info2.id,
        "same app_id should always produce the same UUID"
    );
}

// ===========================================================================
// Server-level auth integration
// ===========================================================================

#[tokio::test]
async fn test_server_with_app_id_allowlist_enabled_creates_successfully() {
    let mut config = test_server_config();
    config.app_id_allowlist_enabled = true;

    let entries = vec![sample_app_entry()];

    let server = signal_fish_server::server::EnhancedGameServer::new(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
        signal_fish_server::config::RelayTypeConfig::default(),
        signal_fish_server::config::SessionConfig::default(),
        signal_fish_server::config::TurnConfig::default(),
        signal_fish_server::database::DatabaseConfig::InMemory,
        signal_fish_server::config::MetricsConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        entries,
    )
    .await;

    assert!(
        server.is_ok(),
        "server with an enforced allowlist and apps should start"
    );
}

#[tokio::test]
async fn test_server_with_app_id_allowlist_enabled_no_apps_still_starts() {
    // Per the server code, this logs a warning but does not fail.
    let mut config = test_server_config();
    config.app_id_allowlist_enabled = true;

    let server = signal_fish_server::server::EnhancedGameServer::new(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
        signal_fish_server::config::RelayTypeConfig::default(),
        signal_fish_server::config::SessionConfig::default(),
        signal_fish_server::config::TurnConfig::default(),
        signal_fish_server::database::DatabaseConfig::InMemory,
        signal_fish_server::config::MetricsConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        vec![],
    )
    .await;

    assert!(
        server.is_ok(),
        "server with an empty enforced allowlist should still start (just warns)"
    );
}

#[tokio::test]
async fn test_server_with_open_app_id_policy_creates_successfully() {
    let config = test_server_config(); // app_id_allowlist_enabled defaults to false

    let server = create_test_server_with_config(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
    )
    .await;

    // Server should be usable
    assert!(server.health_check().await, "health check should pass");
}

#[tokio::test]
async fn test_server_app_id_allowlist_is_accessible() {
    // Verify the server wires the allowlist correctly by creating a server
    // with enforcement enabled and checking that it remains healthy.
    let mut config = test_server_config();
    config.app_id_allowlist_enabled = true;

    let entries = vec![sample_app_entry()];

    let server = signal_fish_server::server::EnhancedGameServer::new(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
        signal_fish_server::config::RelayTypeConfig::default(),
        signal_fish_server::config::SessionConfig::default(),
        signal_fish_server::config::TurnConfig::default(),
        signal_fish_server::database::DatabaseConfig::InMemory,
        signal_fish_server::config::MetricsConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        entries,
    )
    .await
    .expect("server should start");

    // The app_id_allowlist field is pub(crate) so we can only indirectly verify
    // it by confirming the server starts and passes health checks.
    assert!(server.health_check().await);
}

#[tokio::test]
async fn real_websocket_handshake_binds_room_and_spectator_policy_to_public_app_id() {
    let mut config = test_server_config();
    config.app_id_allowlist_enabled = true;
    let apps = vec![
        AppRegistrationEntry {
            app_id: "app-a".to_string(),
            app_name: "App A".to_string(),
            max_rooms: Some(10),
            max_players_per_room: Some(8),
            rate_limit_per_minute: None,
        },
        AppRegistrationEntry {
            app_id: "app-b".to_string(),
            app_name: "App B".to_string(),
            max_rooms: Some(10),
            max_players_per_room: Some(8),
            rate_limit_per_minute: None,
        },
    ];
    let server = signal_fish_server::server::EnhancedGameServer::new(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
        signal_fish_server::config::RelayTypeConfig::default(),
        signal_fish_server::config::SessionConfig::default(),
        signal_fish_server::config::TurnConfig::default(),
        signal_fish_server::database::DatabaseConfig::InMemory,
        signal_fish_server::config::MetricsConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        apps,
    )
    .await
    .expect("construct allowlisted server");
    let router = create_router("http://localhost:3000").with_state(server.clone());
    let running = RunningTestServer::spawn(server, router).await;

    let mut creator = connect_with_public_app_id(running.addr(), "app-a").await;
    send_client_message(
        &mut creator,
        &ClientMessage::JoinRoom {
            game_name: "trust-boundary".to_string(),
            room_code: Some("BOUND1".to_string()),
            player_name: "Creator".to_string(),
            max_players: Some(4),
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await;
    assert!(matches!(
        next_server_message_within(&mut creator, SOCKET_DEADLINE, "app A creates room").await,
        ServerMessage::RoomJoined(_)
    ));

    let mut other_app = connect_with_public_app_id(running.addr(), "app-b").await;
    send_client_message(
        &mut other_app,
        &ClientMessage::JoinRoom {
            game_name: "trust-boundary".to_string(),
            room_code: Some("BOUND1".to_string()),
            player_name: "Other".to_string(),
            max_players: Some(4),
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await;
    assert!(matches!(
        next_server_message_within(&mut other_app, SOCKET_DEADLINE, "different app cannot join")
            .await,
        ServerMessage::RoomJoinFailed {
            error_code: Some(ErrorCode::RoomNotFound),
            ..
        }
    ));

    send_client_message(
        &mut other_app,
        &ClientMessage::JoinAsSpectator {
            game_name: "trust-boundary".to_string(),
            room_code: "BOUND1".to_string(),
            spectator_name: "Observer".to_string(),
        },
    )
    .await;
    let spectator_rejection = next_server_message_within(
        &mut other_app,
        SOCKET_DEADLINE,
        "different app cannot spectate",
    )
    .await;
    assert!(
        matches!(
            spectator_rejection,
            ServerMessage::SpectatorJoinFailed {
                error_code: Some(ErrorCode::RoomNotFound),
                ..
            }
        ),
        "cross-app spectator rejection must be non-enumerating: {spectator_rejection:?}"
    );

    let mut replay = connect_with_public_app_id(running.addr(), "app-a").await;
    send_client_message(
        &mut replay,
        &ClientMessage::JoinRoom {
            game_name: "trust-boundary".to_string(),
            room_code: Some("BOUND1".to_string()),
            player_name: "Replay".to_string(),
            max_players: Some(4),
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await;
    assert!(matches!(
        next_server_message_within(&mut replay, SOCKET_DEADLINE, "replayed app A joins").await,
        ServerMessage::RoomJoined(_)
    ));

    running.shutdown().await;
}

/// Socket-level contract for the log-safety gate: an app ID that could forge
/// or bloat operator-facing log lines must fail authentication with
/// `INVALID_APP_ID` in BOTH policy modes, before any room surface is reachable.
#[tokio::test]
async fn test_unloggable_app_id_fails_authentication_with_invalid_app_id() {
    let config = test_server_config(); // open policy
    let server = create_test_server_with_config(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
    )
    .await;
    let router = create_router("http://localhost:3000").with_state(server.clone());
    let running = RunningTestServer::spawn(server, router).await;

    let rejected_ids: Vec<String> = vec![
        "game\n2026-08-23T00:00:00Z WARN forged event \u{1b}[31mRED".to_string(),
        "a".repeat(signal_fish_server::auth::MAX_APP_ID_LENGTH + 1),
        "id\u{7f}".to_string(),
    ];
    for app_id in rejected_ids {
        let url = format!("ws://{}/ws", running.addr());
        let (mut ws, _) = tokio::time::timeout(SOCKET_DEADLINE, connect_async(url))
            .await
            .expect("WebSocket connect timed out")
            .expect("WebSocket connects");
        send_client_message(
            &mut ws,
            &ClientMessage::Authenticate {
                app_id,
                sdk_version: None,
                platform: None,
                game_data_format: None,
                protocol_version: Some(2),
                supported_transports: None,
                supported_topologies: None,
                requested_capabilities: None,
            },
        )
        .await;
        match next_server_message_within(&mut ws, SOCKET_DEADLINE, "rejected app ID").await {
            ServerMessage::AuthenticationError {
                error_code: ErrorCode::InvalidAppId,
                ..
            } => {}
            other => panic!("expected INVALID_APP_ID rejection, got {other:?}"),
        }
    }

    // A well-formed ID still authenticates on the same deployment.
    let _ = connect_with_public_app_id(running.addr(), "legitimate-app").await;

    running.shutdown().await;
}
