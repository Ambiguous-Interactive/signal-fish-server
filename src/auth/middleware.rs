//! In-memory application-ID allowlist for Signal Fish Server.
//!
//! Resolves public application IDs against static configuration loaded at
//! startup. This module does not authenticate a client or validate a client
//! secret: any client can replay a known app ID. When enforcement is disabled,
//! every app ID receives a default [`AppContext`].

use super::error::AuthError;
use super::rate_limiter::InMemoryRateLimiter;
use crate::config::AppRegistrationEntry;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Per-application rate limit information returned to clients.
///
/// With allowlist enforcement on, `per_minute` limits handshake resolution for
/// a known public ID. The `per_hour` and `per_day` fields are advisory
/// projections communicated to clients and are not enforced. Open mode uses
/// fixed legacy values (`1000`, `10000`, `100000`) and enforces none of them.
#[derive(Debug, Clone)]
pub struct RateLimits {
    /// Known-ID handshake attempts allowed per minute in enforced mode.
    pub per_minute: u32,
    /// Advisory hourly projection; a fixed legacy value in open mode.
    pub per_hour: u32,
    /// Advisory daily projection; a fixed legacy value in open mode.
    pub per_day: u32,
}

/// Application context attached after a public app ID is accepted.
#[derive(Debug, Clone)]
pub struct AppContext {
    pub id: Uuid,
    pub name: String,
    pub organization: Option<String>,
    pub max_rooms: Option<u32>,
    pub max_players_per_room: Option<u8>,
    pub rate_limit_per_minute: Option<u32>,
    pub rate_limits: RateLimits,
}

/// Default rate limits applied when allowlisting is disabled or an application
/// has no explicit per-minute limit configured.
const DEFAULT_RATE_LIMIT_PER_MINUTE: u32 = 1000;
const DEFAULT_RATE_LIMIT_PER_HOUR: u32 = 10000;
const DEFAULT_RATE_LIMIT_PER_DAY: u32 = 100_000;

/// Maximum accepted app-ID byte length.
///
/// App IDs are attacker-chosen strings that reach operator-facing log lines,
/// error messages, and derived-UUID keys. A generous bound keeps any single
/// handshake from amplifying log volume while leaving room for realistic IDs.
pub const MAX_APP_ID_LENGTH: usize = 256;

/// Whether an app ID can be accepted and logged safely.
///
/// Rejects control characters (newlines, ANSI escapes, C1 controls) — the
/// classic log-forging vector on `%`-formatted `tracing` fields — and
/// unbounded lengths. Every other string is accepted verbatim so existing
/// allowlist configurations keep resolving exactly as before.
#[must_use]
pub fn app_id_is_log_safe(app_id: &str) -> bool {
    app_id.len() <= MAX_APP_ID_LENGTH && app_id.chars().all(|ch| !ch.is_control())
}

/// Derive a deterministic UUID from a string key using SHA-256. The first 16
/// bytes of the hash are used as the UUID value with the version nibble set
/// to 4 (random) and the variant to RFC 4122.
fn deterministic_uuid(key: &str) -> Uuid {
    let hash = Sha256::digest(key.as_bytes());
    let mut bytes = [0u8; 16];
    let Some(prefix) = hash.get(..16) else {
        tracing::error!("SHA-256 produced an invalid digest length");
        return Uuid::nil();
    };
    bytes.copy_from_slice(prefix);
    // Set version to 4 (bits 48..51)
    if let Some(version) = bytes.get_mut(6) {
        *version = (*version & 0x0F) | 0x40;
    }
    // Set variant to RFC 4122 (bits 64..65)
    if let Some(variant) = bytes.get_mut(8) {
        *variant = (*variant & 0x3F) | 0x80;
    }
    Uuid::from_bytes(bytes)
}

/// In-memory public app-ID allowlist backed by configured application entries.
pub struct AppIdAllowlist {
    /// Map of public app ID to its accounting and quota context.
    apps: HashMap<String, AppContext>,
    /// Per-app sliding-window rate limiter.
    rate_limiter: Arc<InMemoryRateLimiter>,
    /// Whether allowlist enforcement is enabled.
    enforce: bool,
    /// Shared server metrics when constructed by `EnhancedGameServer`.
    metrics: Option<Arc<crate::metrics::ServerMetrics>>,
}

impl AppIdAllowlist {
    /// Create an enforced allowlist populated from config entries.
    ///
    /// A background rate-limiter cleanup task is started only when at least one
    /// configured application has a `rate_limit_per_minute` set.
    pub fn new(entries: Vec<AppRegistrationEntry>) -> Result<Self, AuthError> {
        Self::new_inner(entries, None)
    }

    pub(crate) fn with_metrics(
        entries: Vec<AppRegistrationEntry>,
        metrics: Arc<crate::metrics::ServerMetrics>,
    ) -> Result<Self, AuthError> {
        Self::new_inner(entries, Some(metrics))
    }

    fn new_inner(
        entries: Vec<AppRegistrationEntry>,
        metrics: Option<Arc<crate::metrics::ServerMetrics>>,
    ) -> Result<Self, AuthError> {
        let has_rate_limited_app = entries.iter().any(|e| e.rate_limit_per_minute.is_some());

        let mut apps = HashMap::with_capacity(entries.len());
        for entry in entries {
            let per_minute = entry
                .rate_limit_per_minute
                .unwrap_or(DEFAULT_RATE_LIMIT_PER_MINUTE);
            let info = AppContext {
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
            if apps.insert(entry.app_id, info).is_some() {
                return Err(AuthError::DuplicateAppId);
            }
        }

        let rate_limiter = Arc::new(InMemoryRateLimiter::new(Duration::from_secs(60)));

        if has_rate_limited_app {
            if let Err(error) = rate_limiter.clone().start_cleanup_task() {
                tracing::warn!(%error, "App-ID rate-limit cleanup requires an active Tokio runtime");
            }
        }

        Ok(Self {
            apps,
            rate_limiter,
            enforce: true,
            metrics,
        })
    }

    /// Create an open policy that accepts every app ID with default context.
    pub fn disabled() -> Self {
        Self {
            apps: HashMap::new(),
            rate_limiter: Arc::new(InMemoryRateLimiter::new(Duration::from_secs(60))),
            enforce: false,
            metrics: None,
        }
    }

    /// Resolve a public app ID. This is the method called by the WebSocket
    /// `Authenticate` handshake; the legacy wire name does not imply proof of
    /// client identity.
    ///
    /// This method is `async` for interface compatibility so that future
    /// implementations (e.g., database-backed auth) can perform I/O without
    /// changing the call-site.
    pub async fn resolve_app_id(&self, app_id: &str) -> Result<AppContext, AuthError> {
        // Vet before any policy path (including the open-policy early return):
        // both modes feed the ID into logs and derived keys, so neither may
        // admit a string that forges log lines or unboundedly bloats them.
        if !app_id_is_log_safe(app_id) {
            return Err(AuthError::InvalidAppId);
        }

        if !self.enforce {
            return Ok(self.default_app_context(app_id));
        }

        let info = self.apps.get(app_id).ok_or(AuthError::InvalidAppId)?;

        // Enforce per-app rate limit if configured.
        if let Some(limit) = info.rate_limit_per_minute {
            self.check_rate_limit(app_id, limit)?;
        }

        Ok(info.clone())
    }

    fn check_rate_limit(&self, app_id: &str, limit: u32) -> Result<(), AuthError> {
        self.rate_limiter
            .check_rate_limit(app_id, limit)
            .inspect_err(|_| {
                if let Some(metrics) = &self.metrics {
                    metrics.record_rate_limit_rejection(crate::metrics::RateLimitRejection::Auth);
                }
            })
    }

    /// Build a default context for use when allowlist enforcement is disabled.
    fn default_app_context(&self, app_id: &str) -> AppContext {
        let id = app_id
            .parse::<Uuid>()
            .unwrap_or_else(|_| deterministic_uuid(app_id));
        AppContext {
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

    fn sample_entries() -> Vec<AppRegistrationEntry> {
        vec![
            AppRegistrationEntry {
                app_id: "game-1".to_string(),
                app_name: "Test Game".to_string(),
                max_rooms: Some(50),
                max_players_per_room: Some(8),
                rate_limit_per_minute: Some(60),
            },
            AppRegistrationEntry {
                app_id: "game-2".to_string(),
                app_name: "Another Game".to_string(),
                max_rooms: None,
                max_players_per_room: None,
                rate_limit_per_minute: None,
            },
        ]
    }

    #[tokio::test]
    async fn disabled_middleware_always_succeeds() {
        let mw = AppIdAllowlist::disabled();
        let result = mw.resolve_app_id("anything").await;
        assert!(result.is_ok());
        let info = result.unwrap();
        assert_eq!(info.name, "default");
    }

    /// Data-driven: app IDs that could forge or bloat operator-facing log
    /// lines are rejected in BOTH policy modes, while ordinary IDs (including
    /// spaces, unicode, and every allowlisted ID) keep resolving.
    #[tokio::test]
    async fn unloggable_app_ids_are_rejected_before_any_policy_evaluation() {
        let over_limit = "a".repeat(MAX_APP_ID_LENGTH + 1);
        let at_limit = "a".repeat(MAX_APP_ID_LENGTH);
        let rejected: &[&str] = &[
            "game\n2026-01-01 WARN forged log line", // newline injection
            "\u{1b}[31mred\u{1b}[0m",                // ANSI escape injection
            "id\u{7f}",                              // DEL
            "id\u{9}tab",                            // C0 tab
            "\u{85}",                                // C1 next line
            over_limit.as_str(),                     // unbounded log amplification
        ];
        for app_id in rejected {
            let open = AppIdAllowlist::disabled();
            assert!(
                matches!(
                    open.resolve_app_id(app_id).await,
                    Err(AuthError::InvalidAppId)
                ),
                "open policy must reject {app_id:?}"
            );
            let enforcing = AppIdAllowlist::new(vec![AppRegistrationEntry {
                app_id: (*app_id).to_string(),
                app_name: "Configured".to_string(),
                max_rooms: None,
                max_players_per_room: None,
                rate_limit_per_minute: None,
            }])
            .expect("constructor does not vet charset; resolution does");
            assert!(
                matches!(
                    enforcing.resolve_app_id(app_id).await,
                    Err(AuthError::InvalidAppId)
                ),
                "enforcing policy must reject {app_id:?}"
            );
        }

        let accepted: &[&str] = &["anything", "my-game/v1.2", &at_limit, "café-ünïcode"];
        for app_id in accepted {
            let open = AppIdAllowlist::disabled();
            assert!(
                open.resolve_app_id(app_id).await.is_ok(),
                "open policy must accept {app_id:?}"
            );
        }
    }

    #[test]
    fn rate_limited_middleware_can_be_constructed_without_a_runtime() {
        let middleware = AppIdAllowlist::new(sample_entries()).expect("unique app IDs");
        assert!(middleware.enforce);
    }

    #[test]
    fn duplicate_public_app_ids_are_rejected_by_the_policy_constructor() {
        let mut entries = sample_entries();
        entries.push(AppRegistrationEntry {
            app_id: "game-1".to_string(),
            app_name: "Conflicting Game".to_string(),
            max_rooms: Some(999),
            max_players_per_room: None,
            rate_limit_per_minute: Some(1),
        });

        assert!(matches!(
            AppIdAllowlist::new(entries),
            Err(AuthError::DuplicateAppId)
        ));
    }

    #[tokio::test]
    async fn valid_app_id_returns_info() {
        let mw = AppIdAllowlist::new(sample_entries()).expect("unique app IDs");
        let result = mw.resolve_app_id("game-1").await;
        assert!(result.is_ok());
        let info = result.unwrap();
        assert_eq!(info.name, "Test Game");
        assert_eq!(info.max_rooms, Some(50));
        assert_eq!(info.max_players_per_room, Some(8));
        assert_eq!(info.rate_limit_per_minute, Some(60));
    }

    #[tokio::test]
    async fn invalid_app_id_returns_error() {
        let mw = AppIdAllowlist::new(sample_entries()).expect("unique app IDs");
        let result = mw.resolve_app_id("nonexistent").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AuthError::InvalidAppId));
    }

    #[tokio::test]
    async fn public_app_id_is_replayable_and_resolves_the_same_context() {
        let mw = AppIdAllowlist::new(sample_entries()).expect("unique app IDs");
        let first = mw.resolve_app_id("game-1").await.unwrap();
        let replay = mw.resolve_app_id("game-1").await.unwrap();
        assert_eq!(first.id, replay.id);
        assert_eq!(first.name, replay.name);
    }

    #[tokio::test]
    async fn rate_limit_enforced_on_resolve_app_id() {
        let entries = vec![AppRegistrationEntry {
            app_id: "limited".to_string(),
            app_name: "Limited App".to_string(),
            max_rooms: None,
            max_players_per_room: None,
            rate_limit_per_minute: Some(3),
        }];
        let mw = AppIdAllowlist::new(entries).expect("unique app IDs");

        // First 3 should succeed
        for _ in 0..3 {
            assert!(mw.resolve_app_id("limited").await.is_ok());
        }
        // 4th should fail
        let result = mw.resolve_app_id("limited").await;
        assert!(matches!(result.unwrap_err(), AuthError::RateLimitExceeded));
    }

    #[tokio::test]
    async fn rate_limit_rejection_updates_server_metrics() {
        let entries = vec![AppRegistrationEntry {
            app_id: "limited".to_string(),
            app_name: "Limited App".to_string(),
            max_rooms: None,
            max_players_per_room: None,
            rate_limit_per_minute: Some(1),
        }];
        let metrics = Arc::new(crate::metrics::ServerMetrics::new());
        let mw = AppIdAllowlist::with_metrics(entries, metrics.clone()).expect("unique app IDs");

        assert!(mw.resolve_app_id("limited").await.is_ok());
        assert!(matches!(
            mw.resolve_app_id("limited").await,
            Err(AuthError::RateLimitExceeded)
        ));

        let snapshot = metrics.snapshot().await.rate_limiting;
        assert_eq!(snapshot.rate_limit_rejections, 1);
        assert_eq!(snapshot.auth_rejections, 1);
        assert_eq!(snapshot.room_creation_rejections, 0);
    }

    #[tokio::test]
    async fn no_rate_limit_when_none_configured() {
        let entries = vec![AppRegistrationEntry {
            app_id: "unlimited".to_string(),
            app_name: "Unlimited App".to_string(),
            max_rooms: None,
            max_players_per_room: None,
            rate_limit_per_minute: None,
        }];
        let mw = AppIdAllowlist::new(entries).expect("unique app IDs");

        // Should succeed many times without rate limit
        for _ in 0..100 {
            assert!(mw.resolve_app_id("unlimited").await.is_ok());
        }
    }

    #[tokio::test]
    async fn deterministic_uuid_for_same_app_id() {
        let mw = AppIdAllowlist::new(sample_entries()).expect("unique app IDs");
        let info1 = mw.resolve_app_id("game-1").await.unwrap();
        let info2 = mw.resolve_app_id("game-1").await.unwrap();
        assert_eq!(info1.id, info2.id);
    }

    #[tokio::test]
    async fn default_rate_limits_for_app_without_explicit_limit() {
        let mw = AppIdAllowlist::new(sample_entries()).expect("unique app IDs");
        let info = mw.resolve_app_id("game-2").await.unwrap();
        assert_eq!(info.rate_limits.per_minute, DEFAULT_RATE_LIMIT_PER_MINUTE);
    }

    #[tokio::test]
    async fn disabled_app_id_parsed_as_uuid_when_valid() {
        let mw = AppIdAllowlist::disabled();
        let uuid_str = "550e8400-e29b-41d4-a716-446655440000";
        let info = mw.resolve_app_id(uuid_str).await.unwrap();
        assert_eq!(info.id.to_string(), uuid_str);
    }

    #[tokio::test]
    async fn disabled_non_uuid_app_id_gets_deterministic_id() {
        let mw = AppIdAllowlist::disabled();
        let info1 = mw.resolve_app_id("my-game").await.unwrap();
        let info2 = mw.resolve_app_id("my-game").await.unwrap();
        assert_eq!(info1.id, info2.id);
    }
}
