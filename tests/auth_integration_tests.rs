//! Auth integration tests for the in-memory auth backend
//!
//! Tests end-to-end authentication behaviour via the server and the standalone
//! `AuthMiddleware` API surface.

mod test_helpers;

use signal_fish_server::auth::{AuthError, AuthMiddleware};
use signal_fish_server::config::AppAuthEntry;
use test_helpers::{create_test_server_with_config, test_server_config};

// ---------------------------------------------------------------------------
// Helper factories
// ---------------------------------------------------------------------------

fn sample_app_entry() -> AppAuthEntry {
    AppAuthEntry {
        app_id: "test-game-1".to_string(),
        app_secret: "super-secret-1".to_string(),
        app_name: "Test Game".to_string(),
        max_rooms: Some(50),
        max_players_per_room: Some(8),
        rate_limit_per_minute: Some(60),
    }
}

fn secondary_app_entry() -> AppAuthEntry {
    AppAuthEntry {
        app_id: "test-game-2".to_string(),
        app_secret: "super-secret-2".to_string(),
        app_name: "Secondary Game".to_string(),
        max_rooms: None,
        max_players_per_room: None,
        rate_limit_per_minute: None,
    }
}

fn rate_limited_app_entry(limit: u32) -> AppAuthEntry {
    AppAuthEntry {
        app_id: "rate-limited-app".to_string(),
        app_secret: "rate-secret".to_string(),
        app_name: "Rate Limited App".to_string(),
        max_rooms: Some(10),
        max_players_per_room: Some(4),
        rate_limit_per_minute: Some(limit),
    }
}

// ===========================================================================
// AuthMiddleware unit-level integration tests
// ===========================================================================

#[tokio::test]
async fn test_validate_correct_credentials() {
    let mw = AuthMiddleware::new(vec![sample_app_entry(), secondary_app_entry()]);

    let info = mw
        .validate_app_credentials("test-game-1", "super-secret-1")
        .await
        .expect("valid credentials should succeed");

    assert_eq!(info.name, "Test Game");
    assert_eq!(info.max_rooms, Some(50));
    assert_eq!(info.max_players_per_room, Some(8));
    assert_eq!(info.rate_limit_per_minute, Some(60));
}

#[tokio::test]
async fn test_validate_correct_credentials_secondary_app() {
    let mw = AuthMiddleware::new(vec![sample_app_entry(), secondary_app_entry()]);

    let info = mw
        .validate_app_credentials("test-game-2", "super-secret-2")
        .await
        .expect("valid credentials for secondary app should succeed");

    assert_eq!(info.name, "Secondary Game");
    assert_eq!(info.max_rooms, None);
    assert_eq!(info.max_players_per_room, None);
    assert_eq!(info.rate_limit_per_minute, None);
}

#[tokio::test]
async fn test_reject_wrong_secret() {
    let mw = AuthMiddleware::new(vec![sample_app_entry()]);

    let err = mw
        .validate_app_credentials("test-game-1", "wrong-secret")
        .await
        .expect_err("wrong secret should fail");

    assert!(
        matches!(err, AuthError::InvalidCredentials),
        "expected InvalidCredentials, got: {err:?}"
    );
}

#[tokio::test]
async fn test_reject_unknown_app_id() {
    let mw = AuthMiddleware::new(vec![sample_app_entry()]);

    let err = mw
        .validate_app_credentials("nonexistent-app", "any-secret")
        .await
        .expect_err("unknown app_id should fail");

    assert!(
        matches!(err, AuthError::InvalidAppId),
        "expected InvalidAppId, got: {err:?}"
    );
}

#[tokio::test]
async fn test_validate_app_id_only() {
    let mw = AuthMiddleware::new(vec![sample_app_entry()]);

    let info = mw
        .validate_app_id("test-game-1")
        .await
        .expect("valid app_id should succeed");

    assert_eq!(info.name, "Test Game");
}

#[tokio::test]
async fn test_validate_app_id_only_rejects_unknown() {
    let mw = AuthMiddleware::new(vec![sample_app_entry()]);

    let err = mw
        .validate_app_id("nonexistent-app")
        .await
        .expect_err("unknown app_id should fail");

    assert!(
        matches!(err, AuthError::InvalidAppId),
        "expected InvalidAppId, got: {err:?}"
    );
}

// ===========================================================================
// Rate limiting via AuthMiddleware
// ===========================================================================

#[tokio::test]
async fn test_rate_limiting_enforced() {
    let limit = 5u32;
    let mw = AuthMiddleware::new(vec![rate_limited_app_entry(limit)]);

    // Requests up to the limit should succeed
    for i in 0..limit {
        let result = mw
            .validate_app_credentials("rate-limited-app", "rate-secret")
            .await;
        assert!(
            result.is_ok(),
            "request {i} of {limit} should succeed, got: {result:?}"
        );
    }

    // The next request should be rejected
    let err = mw
        .validate_app_credentials("rate-limited-app", "rate-secret")
        .await
        .expect_err("should be rate limited after exceeding per-minute cap");

    assert!(
        matches!(err, AuthError::RateLimitExceeded),
        "expected RateLimitExceeded, got: {err:?}"
    );
}

#[tokio::test]
async fn test_rate_limiting_enforced_on_app_id_only() {
    let limit = 3u32;
    let mw = AuthMiddleware::new(vec![rate_limited_app_entry(limit)]);

    for _ in 0..limit {
        assert!(mw.validate_app_id("rate-limited-app").await.is_ok());
    }

    let err = mw
        .validate_app_id("rate-limited-app")
        .await
        .expect_err("should be rate limited after exceeding per-minute cap via app_id");

    assert!(
        matches!(err, AuthError::RateLimitExceeded),
        "expected RateLimitExceeded, got: {err:?}"
    );
}

#[tokio::test]
async fn test_no_rate_limiting_when_not_configured() {
    let mw = AuthMiddleware::new(vec![secondary_app_entry()]);

    // Should succeed many times without hitting a limit
    for _ in 0..200 {
        assert!(mw
            .validate_app_credentials("test-game-2", "super-secret-2")
            .await
            .is_ok());
    }
}

#[tokio::test]
async fn test_rate_limits_are_per_app() {
    let entries = vec![
        rate_limited_app_entry(2),
        AppAuthEntry {
            app_id: "other-limited-app".to_string(),
            app_secret: "other-secret".to_string(),
            app_name: "Other App".to_string(),
            max_rooms: None,
            max_players_per_room: None,
            rate_limit_per_minute: Some(2),
        },
    ];
    let mw = AuthMiddleware::new(entries);

    // Exhaust rate limit for first app
    for _ in 0..2 {
        mw.validate_app_id("rate-limited-app").await.unwrap();
    }
    assert!(mw.validate_app_id("rate-limited-app").await.is_err());

    // Second app should still be fine
    assert!(mw.validate_app_id("other-limited-app").await.is_ok());
}

// ===========================================================================
// Disabled auth middleware
// ===========================================================================

#[tokio::test]
async fn test_disabled_auth_accepts_any_credentials() {
    let mw = AuthMiddleware::disabled();

    let info = mw
        .validate_app_credentials("anything", "anything")
        .await
        .expect("disabled auth should accept any credentials");

    assert_eq!(info.name, "default");
}

#[tokio::test]
async fn test_disabled_auth_accepts_any_app_id() {
    let mw = AuthMiddleware::disabled();

    let info = mw
        .validate_app_id("anything")
        .await
        .expect("disabled auth should accept any app_id");

    assert_eq!(info.name, "default");
}

#[tokio::test]
async fn test_disabled_auth_returns_default_rate_limits() {
    let mw = AuthMiddleware::disabled();

    let info = mw.validate_app_id("x").await.unwrap();

    assert_eq!(info.rate_limits.per_minute, 1000);
    assert_eq!(info.rate_limits.per_hour, 10000);
    assert_eq!(info.rate_limits.per_day, 100_000);
}

// ===========================================================================
// AppInfo field assertions
// ===========================================================================

#[tokio::test]
async fn test_app_info_rate_limits_are_computed_correctly() {
    let entry = AppAuthEntry {
        app_id: "computed-limits".to_string(),
        app_secret: "s".to_string(),
        app_name: "Computed".to_string(),
        max_rooms: None,
        max_players_per_room: None,
        rate_limit_per_minute: Some(10),
    };
    let mw = AuthMiddleware::new(vec![entry]);

    let info = mw.validate_app_id("computed-limits").await.unwrap();

    assert_eq!(info.rate_limits.per_minute, 10);
    assert_eq!(info.rate_limits.per_hour, 600); // 10 * 60
    assert_eq!(info.rate_limits.per_day, 14400); // 10 * 60 * 24
}

#[tokio::test]
async fn test_deterministic_uuid_for_same_app_id() {
    let mw = AuthMiddleware::new(vec![sample_app_entry()]);

    let info1 = mw.validate_app_id("test-game-1").await.unwrap();
    let info2 = mw.validate_app_id("test-game-1").await.unwrap();

    assert_eq!(
        info1.id, info2.id,
        "same app_id should always produce the same UUID"
    );
}

// ===========================================================================
// Server-level auth integration
// ===========================================================================

#[tokio::test]
async fn test_server_with_auth_enabled_creates_successfully() {
    let mut config = test_server_config();
    config.auth_enabled = true;

    let entries = vec![sample_app_entry()];

    let server = signal_fish_server::server::EnhancedGameServer::new(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
        signal_fish_server::config::RelayTypeConfig::default(),
        signal_fish_server::database::DatabaseConfig::InMemory,
        signal_fish_server::config::MetricsConfig::default(),
        signal_fish_server::config::AuthMaintenanceConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        entries,
    )
    .await;

    assert!(
        server.is_ok(),
        "server with auth enabled and apps should start"
    );
}

#[tokio::test]
async fn test_server_with_auth_enabled_no_apps_still_starts() {
    // Per the server code, this logs a warning but does not fail.
    let mut config = test_server_config();
    config.auth_enabled = true;

    let server = signal_fish_server::server::EnhancedGameServer::new(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
        signal_fish_server::config::RelayTypeConfig::default(),
        signal_fish_server::database::DatabaseConfig::InMemory,
        signal_fish_server::config::MetricsConfig::default(),
        signal_fish_server::config::AuthMaintenanceConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        vec![],
    )
    .await;

    assert!(
        server.is_ok(),
        "server with auth enabled but no apps should still start (just warns)"
    );
}

#[tokio::test]
async fn test_server_with_auth_disabled_creates_successfully() {
    let config = test_server_config(); // auth_enabled defaults to false

    let server = create_test_server_with_config(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
    )
    .await;

    // Server should be usable
    assert!(server.health_check().await, "health check should pass");
}

#[tokio::test]
async fn test_server_auth_middleware_is_accessible() {
    // Verify the server wires the auth middleware correctly by creating a
    // server with auth enabled and checking that it can validate.
    let mut config = test_server_config();
    config.auth_enabled = true;

    let entries = vec![sample_app_entry()];

    let server = signal_fish_server::server::EnhancedGameServer::new(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
        signal_fish_server::config::RelayTypeConfig::default(),
        signal_fish_server::database::DatabaseConfig::InMemory,
        signal_fish_server::config::MetricsConfig::default(),
        signal_fish_server::config::AuthMaintenanceConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        entries,
    )
    .await
    .expect("server should start");

    // The auth_middleware field is pub(crate) so we can only indirectly verify
    // it by confirming the server starts and passes health checks.
    assert!(server.health_check().await);
}
