//! In-memory authentication middleware for signal-fish-server.
//!
//! Validates application credentials against a static configuration loaded at
//! startup. When auth is disabled the middleware returns a default `AppInfo` for
//! every request.

use super::error::AuthError;
use super::rate_limiter::InMemoryRateLimiter;
use crate::config::AppAuthEntry;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use subtle::ConstantTimeEq;
use uuid::Uuid;

/// Per-application rate limit information returned to clients.
///
/// Only `per_minute` is actively enforced server-side by the
/// `InMemoryRateLimiter`. The `per_hour` and `per_day` fields are advisory
/// projections (computed as multiples of `per_minute`) communicated to clients
/// so they can implement their own budgeting; they are not enforced by the
/// server.
#[derive(Debug, Clone)]
pub struct RateLimits {
    /// Requests allowed per minute. This is the only limit actively enforced
    /// by the server-side rate limiter.
    pub per_minute: u32,
    /// Advisory projection: `per_minute * 60`. Communicated to clients for
    /// budgeting purposes but not enforced server-side.
    pub per_hour: u32,
    /// Advisory projection: `per_minute * 1440`. Communicated to clients for
    /// budgeting purposes but not enforced server-side.
    pub per_day: u32,
}

/// Application information returned after successful authentication.
#[derive(Debug, Clone)]
pub struct AppInfo {
    pub id: Uuid,
    pub name: String,
    pub organization: Option<String>,
    pub max_rooms: Option<u32>,
    pub max_players_per_room: Option<u8>,
    pub rate_limit_per_minute: Option<u32>,
    pub rate_limits: RateLimits,
}

/// Default rate limits applied when auth is disabled or an application has no
/// explicit per-minute limit configured.
const DEFAULT_RATE_LIMIT_PER_MINUTE: u32 = 1000;
const DEFAULT_RATE_LIMIT_PER_HOUR: u32 = 10000;
const DEFAULT_RATE_LIMIT_PER_DAY: u32 = 100_000;

/// Derive a deterministic UUID from a string key using SHA-256. The first 16
/// bytes of the hash are used as the UUID value with the version nibble set
/// to 4 (random) and the variant to RFC 4122.
fn deterministic_uuid(key: &str) -> Uuid {
    let hash = Sha256::digest(key.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    // Set version to 4 (bits 48..51)
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    // Set variant to RFC 4122 (bits 64..65)
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    Uuid::from_bytes(bytes)
}

/// Constant-time secret comparison to prevent timing attacks.
fn secrets_match(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// In-memory authentication middleware backed by a `HashMap` of configured
/// application entries.
pub struct AuthMiddleware {
    /// Map of app_id -> (app_secret, AppInfo). Empty when auth is disabled.
    apps: HashMap<String, (String, AppInfo)>,
    /// Per-app sliding-window rate limiter.
    rate_limiter: Arc<InMemoryRateLimiter>,
    /// Whether authentication is enabled.
    auth_enabled: bool,
}

impl AuthMiddleware {
    /// Create an auth middleware populated from a list of config entries.
    ///
    /// A background rate-limiter cleanup task is started only when at least one
    /// configured application has a `rate_limit_per_minute` set.
    pub fn new(entries: Vec<AppAuthEntry>) -> Self {
        let has_rate_limited_app = entries.iter().any(|e| e.rate_limit_per_minute.is_some());

        let mut apps = HashMap::with_capacity(entries.len());
        for entry in entries {
            let per_minute = entry
                .rate_limit_per_minute
                .unwrap_or(DEFAULT_RATE_LIMIT_PER_MINUTE);
            let info = AppInfo {
                // Deterministic UUID derived from the app_id string so that
                // the same config always produces the same UUID.
                id: deterministic_uuid(&entry.app_id),
                name: entry.app_name.clone(),
                organization: None,
                max_rooms: entry.max_rooms,
                max_players_per_room: entry.max_players_per_room,
                rate_limit_per_minute: entry.rate_limit_per_minute,
                rate_limits: RateLimits {
                    per_minute,
                    per_hour: per_minute.saturating_mul(60),
                    per_day: per_minute.saturating_mul(60).saturating_mul(24),
                },
            };
            apps.insert(entry.app_id, (entry.app_secret, info));
        }

        let rate_limiter = Arc::new(InMemoryRateLimiter::new(Duration::from_secs(60)));

        if has_rate_limited_app {
            let _cleanup_handle = rate_limiter.clone().start_cleanup_task();
        }

        Self {
            apps,
            rate_limiter,
            auth_enabled: true,
        }
    }

    /// Create a disabled auth middleware that accepts all connections with
    /// default `AppInfo` values.
    pub fn disabled() -> Self {
        Self {
            apps: HashMap::new(),
            rate_limiter: Arc::new(InMemoryRateLimiter::new(Duration::from_secs(60))),
            auth_enabled: false,
        }
    }

    /// Validate both app_id and app_secret. Returns `AppInfo` on success.
    ///
    /// This method is `async` for interface compatibility so that future
    /// implementations (e.g., database-backed auth) can perform I/O without
    /// changing the call-site.
    pub async fn validate_app_credentials(
        &self,
        app_id: &str,
        app_secret: &str,
    ) -> Result<AppInfo, AuthError> {
        if !self.auth_enabled {
            return Ok(self.default_app_info(app_id));
        }

        let (expected_secret, info) = self.apps.get(app_id).ok_or(AuthError::InvalidAppId)?;

        if !secrets_match(expected_secret, app_secret) {
            return Err(AuthError::InvalidCredentials);
        }

        // Enforce per-app rate limit if configured.
        if let Some(limit) = info.rate_limit_per_minute {
            self.rate_limiter.check_rate_limit(app_id, limit)?;
        }

        Ok(info.clone())
    }

    /// Validate app_id only (no secret required). This is the method called
    /// by `websocket/connection.rs` during the `Authenticate` handshake.
    ///
    /// This method is `async` for interface compatibility so that future
    /// implementations (e.g., database-backed auth) can perform I/O without
    /// changing the call-site.
    pub async fn validate_app_id(&self, app_id: &str) -> Result<AppInfo, AuthError> {
        if !self.auth_enabled {
            return Ok(self.default_app_info(app_id));
        }

        let (_secret, info) = self.apps.get(app_id).ok_or(AuthError::InvalidAppId)?;

        // Enforce per-app rate limit if configured.
        if let Some(limit) = info.rate_limit_per_minute {
            self.rate_limiter.check_rate_limit(app_id, limit)?;
        }

        Ok(info.clone())
    }

    /// Build a default `AppInfo` for use when auth is disabled.
    fn default_app_info(&self, app_id: &str) -> AppInfo {
        let id = app_id
            .parse::<Uuid>()
            .unwrap_or_else(|_| deterministic_uuid(app_id));
        AppInfo {
            id,
            name: "default".to_string(),
            organization: None,
            max_rooms: None,
            max_players_per_room: None,
            rate_limit_per_minute: None,
            rate_limits: RateLimits {
                per_minute: DEFAULT_RATE_LIMIT_PER_MINUTE,
                per_hour: DEFAULT_RATE_LIMIT_PER_HOUR,
                per_day: DEFAULT_RATE_LIMIT_PER_DAY,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entries() -> Vec<AppAuthEntry> {
        vec![
            AppAuthEntry {
                app_id: "game-1".to_string(),
                app_secret: "secret-1".to_string(),
                app_name: "Test Game".to_string(),
                max_rooms: Some(50),
                max_players_per_room: Some(8),
                rate_limit_per_minute: Some(60),
            },
            AppAuthEntry {
                app_id: "game-2".to_string(),
                app_secret: "secret-2".to_string(),
                app_name: "Another Game".to_string(),
                max_rooms: None,
                max_players_per_room: None,
                rate_limit_per_minute: None,
            },
        ]
    }

    #[tokio::test]
    async fn disabled_middleware_always_succeeds() {
        let mw = AuthMiddleware::disabled();
        let result = mw.validate_app_id("anything").await;
        assert!(result.is_ok());
        let info = result.unwrap();
        assert_eq!(info.name, "default");
    }

    #[tokio::test]
    async fn disabled_middleware_validate_credentials_succeeds() {
        let mw = AuthMiddleware::disabled();
        let result = mw.validate_app_credentials("any-id", "any-secret").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn valid_app_id_returns_info() {
        let mw = AuthMiddleware::new(sample_entries());
        let result = mw.validate_app_id("game-1").await;
        assert!(result.is_ok());
        let info = result.unwrap();
        assert_eq!(info.name, "Test Game");
        assert_eq!(info.max_rooms, Some(50));
        assert_eq!(info.max_players_per_room, Some(8));
        assert_eq!(info.rate_limit_per_minute, Some(60));
    }

    #[tokio::test]
    async fn invalid_app_id_returns_error() {
        let mw = AuthMiddleware::new(sample_entries());
        let result = mw.validate_app_id("nonexistent").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AuthError::InvalidAppId));
    }

    #[tokio::test]
    async fn valid_credentials_succeed() {
        let mw = AuthMiddleware::new(sample_entries());
        let result = mw.validate_app_credentials("game-1", "secret-1").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn wrong_secret_returns_invalid_credentials() {
        let mw = AuthMiddleware::new(sample_entries());
        let result = mw.validate_app_credentials("game-1", "wrong-secret").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AuthError::InvalidCredentials));
    }

    #[tokio::test]
    async fn rate_limit_enforced_on_validate_app_id() {
        let entries = vec![AppAuthEntry {
            app_id: "limited".to_string(),
            app_secret: "s".to_string(),
            app_name: "Limited App".to_string(),
            max_rooms: None,
            max_players_per_room: None,
            rate_limit_per_minute: Some(3),
        }];
        let mw = AuthMiddleware::new(entries);

        // First 3 should succeed
        for _ in 0..3 {
            assert!(mw.validate_app_id("limited").await.is_ok());
        }
        // 4th should fail
        let result = mw.validate_app_id("limited").await;
        assert!(matches!(result.unwrap_err(), AuthError::RateLimitExceeded));
    }

    #[tokio::test]
    async fn no_rate_limit_when_none_configured() {
        let entries = vec![AppAuthEntry {
            app_id: "unlimited".to_string(),
            app_secret: "s".to_string(),
            app_name: "Unlimited App".to_string(),
            max_rooms: None,
            max_players_per_room: None,
            rate_limit_per_minute: None,
        }];
        let mw = AuthMiddleware::new(entries);

        // Should succeed many times without rate limit
        for _ in 0..100 {
            assert!(mw.validate_app_id("unlimited").await.is_ok());
        }
    }

    #[tokio::test]
    async fn deterministic_uuid_for_same_app_id() {
        let mw = AuthMiddleware::new(sample_entries());
        let info1 = mw.validate_app_id("game-1").await.unwrap();
        let info2 = mw.validate_app_id("game-1").await.unwrap();
        assert_eq!(info1.id, info2.id);
    }

    #[tokio::test]
    async fn default_rate_limits_for_app_without_explicit_limit() {
        let mw = AuthMiddleware::new(sample_entries());
        let info = mw.validate_app_id("game-2").await.unwrap();
        assert_eq!(info.rate_limits.per_minute, DEFAULT_RATE_LIMIT_PER_MINUTE);
    }

    #[tokio::test]
    async fn disabled_app_id_parsed_as_uuid_when_valid() {
        let mw = AuthMiddleware::disabled();
        let uuid_str = "550e8400-e29b-41d4-a716-446655440000";
        let info = mw.validate_app_id(uuid_str).await.unwrap();
        assert_eq!(info.id.to_string(), uuid_str);
    }

    #[tokio::test]
    async fn disabled_non_uuid_app_id_gets_deterministic_id() {
        let mw = AuthMiddleware::disabled();
        let info1 = mw.validate_app_id("my-game").await.unwrap();
        let info2 = mw.validate_app_id("my-game").await.unwrap();
        assert_eq!(info1.id, info2.id);
    }
}
