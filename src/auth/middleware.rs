//! In-memory application-ID allowlist for Signal Fish Server.
//!
//! Resolves public application IDs against static configuration loaded at
//! startup. This module does not authenticate a client or validate a client
//! secret: any client can replay a known app ID. When enforcement is disabled,
//! every app ID receives a default [`AppContext`].
//!
//! A configured per-minute limit is enforced as two sliding windows: the
//! application-wide ceiling and a per-source (IP) share (see
//! [`source_rate_limit`]), so one source that knows a configured app ID cannot
//! continuously exhaust that app's handshake budget and lock out legitimate
//! handshakes (issue #502).

use super::error::AuthError;
use super::rate_limiter::InMemoryRateLimiter;
use crate::config::AppRegistrationEntry;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Per-application rate limit information returned to clients.
///
/// With allowlist enforcement on, `per_minute` is the application-wide
/// handshake ceiling per 60-second window; enforcement additionally bounds
/// each source (IP) to half that budget (at least one — see
/// [`source_rate_limit`]) so one source cannot lock out the app (issue #502).
/// Limits apply only to allowlist entries that configure an explicit
/// `rate_limit_per_minute`; an entry without one advertises the default
/// projections and enforces nothing — omitting the field is the "unlimited"
/// configuration. The `per_hour` and `per_day` fields are advisory
/// projections communicated to clients and are not enforced. Open mode uses
/// fixed legacy values (`1000`, `10000`, `100000`) and enforces none of them.
#[derive(Debug, Clone)]
pub struct RateLimits {
    /// Known-ID handshake attempts allowed per minute — enforced only when
    /// the allowlist entry configures an explicit limit.
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
    /// Optional per-sender relay byte budget override (issue #530); `None`
    /// falls back to the server-wide `rate_limit.max_relay_bytes`.
    pub max_relay_bytes: Option<u64>,
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

/// The per-source (IP) share of an application's configured per-minute
/// handshake budget: half the app ceiling, at least one.
///
/// Splitting the budget this way closes the lockout amplification of issue
/// #502: a single source can spend at most its own share per 60-second
/// window, so one abuser that knows a configured `app_id` can never push the
/// application-wide window to its limit alone — at least half the budget
/// stays reachable to every other source. A botnet spanning many sources is
/// bounded by the application-wide ceiling itself, the same documented way
/// `security.max_connections_per_ip` bounds (but does not eliminate)
/// distributed connection abuse. An application limited to 1 handshake per
/// minute keeps a share of 1: that budget is trivially exhausted by any
/// single admitted handshake, abuser or not.
///
/// Memory bound of the per-source windows: a source only reaches the limiter
/// through an admitted WebSocket connection, so distinct pair keys are
/// bounded by the server's own recent connection churn (one window per
/// source address seen, swept by the limiter cleanup task once expired) —
/// the same order as the connection table itself, not attacker-multiplied
/// beyond it.
#[must_use]
pub fn source_rate_limit(app_limit: u32) -> u32 {
    (app_limit / 2).max(1)
}

/// Sliding-window key for one source's share of `app_id`'s budget.
///
/// NUL can never appear in an app ID (the log-safety gate rejects control
/// characters before any limiter path), so this pair encoding is
/// collision-free: an app key can never alias a (app, source) key of another
/// app, and distinct sources of the same app never alias each other.
fn source_window_key(app_id: &str, source: IpAddr) -> String {
    format!("{app_id}\0{source}")
}

/// Whether an app ID can be accepted and logged safely.
///
/// Rejects control characters (newlines, ANSI escapes, C1 controls) — the
/// classic log-forging vector on `%`-formatted `tracing` fields — and
/// unbounded lengths. Configured allowlist entries are held to the same gate
/// at startup, so a configured ID either resolves exactly as before or the
/// server refuses to start; resolution-time behavior for every accepted
/// configuration is unchanged.
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
    /// Sliding-window rate limiter holding both the application-wide windows
    /// and the per-(app, source) share windows (see [`source_rate_limit`]).
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
        for (index, entry) in entries.into_iter().enumerate() {
            // Fail fast on entries the resolution gate could never accept:
            // otherwise one misconfigured ID silently fails every handshake
            // for that app with INVALID_APP_ID, indistinguishable from
            // unknown-ID probing. Startup is the only place this can be loud.
            if !app_id_is_log_safe(&entry.app_id) {
                tracing::error!(
                    entry_index = index,
                    app_id_length = entry.app_id.len(),
                    "Configured app ID is not acceptable (control characters or over \
                     MAX_APP_ID_LENGTH bytes); it could never authenticate"
                );
                return Err(AuthError::InvalidAppId);
            }
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
                max_relay_bytes: entry.max_relay_bytes,
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

    /// Resolve a public app ID from the connection's source address. This is
    /// the method called by the WebSocket `Authenticate` handshake; the legacy
    /// wire name does not imply proof of client identity.
    ///
    /// When the matched application configures an explicit
    /// `rate_limit_per_minute`, admission spends the per-source (IP) share
    /// first and the application-wide ceiling last, so a rejection can never
    /// consume the application-wide budget (see `enforce_rate_limits_at`).
    ///
    /// This method is `async` for interface compatibility so that future
    /// implementations (e.g., database-backed auth) can perform I/O without
    /// changing the call-site.
    pub async fn resolve_app_id(
        &self,
        app_id: &str,
        source: IpAddr,
    ) -> Result<AppContext, AuthError> {
        self.resolve_app_id_at(app_id, source, Instant::now()).await
    }

    /// Injected-time variant of [`Self::resolve_app_id`]: the caller supplies
    /// the current monotonic timestamp so window admission is deterministic
    /// (the same convention as
    /// [`InMemoryRateLimiter::check_rate_limit_at`]).
    pub async fn resolve_app_id_at(
        &self,
        app_id: &str,
        source: IpAddr,
        now: Instant,
    ) -> Result<AppContext, AuthError> {
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

        // Enforce per-app + per-source rate limits if configured.
        if let Some(limit) = info.rate_limit_per_minute {
            self.enforce_rate_limits_at(app_id, source, limit, now)?;
        }

        Ok(info.clone())
    }

    /// Enforce the configured per-minute budget as two sliding windows.
    ///
    /// Both windows are probed before either commits, so a request either
    /// window would reject is stamped nowhere — rejections stay free, exactly
    /// as in the pre-split single-window contract (and an app-ceiling
    /// rejection can no longer burn the source's own share). The commits then
    /// run source share first and application-wide ceiling last: the worst
    /// case is at most one self-tightening stamp in the offending source's
    /// own window, reachable under a probe/commit race or an out-of-order
    /// injected timestamp (the `resolve_app_id_at` seam does not sort the
    /// deque), never a cross-source effect.
    fn enforce_rate_limits_at(
        &self,
        app_id: &str,
        source: IpAddr,
        limit: u32,
        now: Instant,
    ) -> Result<(), AuthError> {
        let pair_key = source_window_key(app_id, source);
        let share = source_rate_limit(limit);
        if !self.rate_limiter.would_admit_at(&pair_key, share, now)
            || !self.rate_limiter.would_admit_at(app_id, limit, now)
        {
            if let Some(metrics) = &self.metrics {
                metrics.record_rate_limit_rejection(crate::metrics::RateLimitRejection::Auth);
            }
            return Err(AuthError::RateLimitExceeded);
        }
        self.check_rate_limit_at(&pair_key, share, now)?;
        self.check_rate_limit_at(app_id, limit, now)
    }

    fn check_rate_limit_at(&self, key: &str, limit: u32, now: Instant) -> Result<(), AuthError> {
        self.rate_limiter
            .check_rate_limit_at(key, limit, now)
            .inspect_err(|_| {
                if let Some(metrics) = &self.metrics {
                    metrics.record_rate_limit_rejection(crate::metrics::RateLimitRejection::Auth);
                }
            })
    }

    /// Build a default context for use when allowlist enforcement is disabled.
    ///
    /// Unlike enforced mode, which always derives the UUID deterministically
    /// from the app ID string, an open-policy client that sends a well-formed
    /// UUID as its `app_id` chooses the application UUID verbatim. Open-mode
    /// application identity scopes room admission (issue #520): rooms are
    /// stamped with the creator's application and owned rooms admit only
    /// same-application members — but the ID remains a client-chosen,
    /// unauthenticated label, so this boundary must not be treated as
    /// unspoofable. Deployment tenancy guarantees require allowlist
    /// enforcement (or the credential-auth story of issue #517).
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
            // Open-mode identity is a client-chosen label (see below), so it
            // can never carry a per-app relay budget override (issue #530):
            // a spoofable context must not be able to raise or lower the
            // server-wide budget.
            max_relay_bytes: None,
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
    use std::net::{Ipv4Addr, Ipv6Addr};

    /// Default resolution source for tests not exercising the per-source
    /// dimension; distinct sources are spelled out where it matters.
    const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    /// A second fixed source distinct from every [`source_for`] index.
    const OTHER: IpAddr = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7));

    /// Deterministic distinct source addresses (TEST-NET-2), index 0 = .1.
    fn source_for(index: usize) -> IpAddr {
        let octet = u8::try_from(index + 1).expect("test source index fits u8");
        IpAddr::V4(Ipv4Addr::new(198, 51, 100, octet))
    }

    fn distinct_sources(n: usize) -> Vec<IpAddr> {
        (0..n).map(source_for).collect()
    }

    fn sample_entries() -> Vec<AppRegistrationEntry> {
        vec![
            AppRegistrationEntry {
                app_id: "game-1".to_string(),
                app_name: "Test Game".to_string(),
                max_rooms: Some(50),
                max_players_per_room: Some(8),
                rate_limit_per_minute: Some(60),
                max_relay_bytes: None,
            },
            AppRegistrationEntry {
                app_id: "game-2".to_string(),
                app_name: "Another Game".to_string(),
                max_rooms: None,
                max_players_per_room: None,
                rate_limit_per_minute: None,
                max_relay_bytes: None,
            },
        ]
    }

    #[tokio::test]
    async fn disabled_middleware_always_succeeds() {
        let mw = AppIdAllowlist::disabled();
        let result = mw.resolve_app_id("anything", LOCALHOST).await;
        assert!(result.is_ok());
        let info = result.unwrap();
        assert_eq!(info.name, "default");
    }

    /// The per-app relay budget override (#530) resolves from the allowlist
    /// entry into the connection's [`AppContext`]; an entry without an
    /// override — and every open-policy context — resolves with `None` so
    /// those senders keep the server-wide budget.
    #[tokio::test]
    async fn relay_budget_override_resolves_into_the_app_context() {
        let tiered = AppRegistrationEntry {
            app_id: "tiered-game".to_string(),
            app_name: "Tiered Game".to_string(),
            max_rooms: None,
            max_players_per_room: None,
            rate_limit_per_minute: None,
            max_relay_bytes: Some(4096),
        };
        let untiered = AppRegistrationEntry {
            app_id: "game-1".to_string(),
            app_name: "Test Game".to_string(),
            max_rooms: None,
            max_players_per_room: None,
            rate_limit_per_minute: None,
            max_relay_bytes: None,
        };

        let mw = AppIdAllowlist::new(vec![tiered, untiered]).expect("unique app IDs");
        let resolved = mw
            .resolve_app_id("tiered-game", LOCALHOST)
            .await
            .expect("tiered app is configured");
        assert_eq!(resolved.max_relay_bytes, Some(4096));

        let resolved = mw
            .resolve_app_id("game-1", LOCALHOST)
            .await
            .expect("untiered app is configured");
        assert_eq!(resolved.max_relay_bytes, None);

        let open = AppIdAllowlist::disabled();
        let resolved = open
            .resolve_app_id("tiered-game", LOCALHOST)
            .await
            .expect("open policy accepts any log-safe ID");
        assert_eq!(
            resolved.max_relay_bytes, None,
            "open-mode contexts never carry an override: the label is spoofable"
        );
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
                    open.resolve_app_id(app_id, LOCALHOST).await,
                    Err(AuthError::InvalidAppId)
                ),
                "open policy must reject {app_id:?}"
            );
            // The constructor fails fast on the same gate, so a misconfigured
            // entry can never silently fail every later handshake.
            let constructed = AppIdAllowlist::new(vec![AppRegistrationEntry {
                app_id: (*app_id).to_string(),
                app_name: "Configured".to_string(),
                max_rooms: None,
                max_players_per_room: None,
                rate_limit_per_minute: None,
                max_relay_bytes: None,
            }]);
            assert!(
                matches!(constructed, Err(AuthError::InvalidAppId)),
                "constructor must reject {app_id:?}"
            );
        }

        let accepted: &[&str] = &["anything", "my-game/v1.2", &at_limit, "café-ünïcode"];
        for app_id in accepted {
            let open = AppIdAllowlist::disabled();
            assert!(
                open.resolve_app_id(app_id, LOCALHOST).await.is_ok(),
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
            max_relay_bytes: None,
        });

        assert!(matches!(
            AppIdAllowlist::new(entries),
            Err(AuthError::DuplicateAppId)
        ));
    }

    #[tokio::test]
    async fn valid_app_id_returns_info() {
        let mw = AppIdAllowlist::new(sample_entries()).expect("unique app IDs");
        let result = mw.resolve_app_id("game-1", LOCALHOST).await;
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
        let result = mw.resolve_app_id("nonexistent", LOCALHOST).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AuthError::InvalidAppId));
    }

    #[tokio::test]
    async fn public_app_id_is_replayable_and_resolves_the_same_context() {
        let mw = AppIdAllowlist::new(sample_entries()).expect("unique app IDs");
        let first = mw.resolve_app_id("game-1", LOCALHOST).await.unwrap();
        let replay = mw.resolve_app_id("game-1", LOCALHOST).await.unwrap();
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
            max_relay_bytes: None,
        }];
        let mw = AppIdAllowlist::new(entries).expect("unique app IDs");

        // Three distinct sources exhaust the application-wide ceiling of 3
        // (each spends 1 of its own 3/2→1-per-source share).
        for source in distinct_sources(3) {
            assert!(mw.resolve_app_id("limited", source).await.is_ok());
        }
        // The 4th source is rejected by the app ceiling.
        let result = mw.resolve_app_id("limited", OTHER).await;
        assert!(matches!(result.unwrap_err(), AuthError::RateLimitExceeded));
    }

    /// The split-budget contract (issue #502), data-driven over configured
    /// app ceilings: each source may spend at most its share (half the app
    /// budget, min 1) per window, total admissions never exceed the app
    /// ceiling, and — the lockout-amplification fix — rejected requests
    /// consume no application-wide budget, so one source can never lock the
    /// app out alone. The window boundary itself is pinned at the limiter
    /// layer (`window_admits_up_to_limit_then_expires_at_the_inclusive_boundary`).
    #[tokio::test]
    async fn per_source_share_caps_one_source_while_the_app_ceiling_conserves_the_budget() {
        // (configured app limit, [source indices]: one designated abuser "a",
        // then enough distinct sources to potentially fill the app window)
        for limit in [2u32, 3, 4, 5, 9, 60] {
            let entries = vec![AppRegistrationEntry {
                app_id: "limited".to_string(),
                app_name: "Limited App".to_string(),
                max_rooms: None,
                max_players_per_room: None,
                rate_limit_per_minute: Some(limit),
                max_relay_bytes: None,
            }];
            let mw = AppIdAllowlist::new(entries).expect("unique app IDs");
            let abuser = source_for(0);
            let share = source_rate_limit(limit);

            // The abuser spends exactly its share, then is rejected.
            for i in 0..share {
                assert!(
                    mw.resolve_app_id("limited", abuser).await.is_ok(),
                    "limit {limit}: abuser request {i}/{share} must be admitted"
                );
            }
            assert!(
                matches!(
                    mw.resolve_app_id("limited", abuser).await,
                    Err(AuthError::RateLimitExceeded)
                ),
                "limit {limit}: abuser past its share of {share} must be rejected"
            );

            // Every abuser rejection consumed no application-wide budget: the
            // remaining distinct sources can still spend the untouched rest.
            let remaining = limit - share;
            for i in 0..remaining {
                let source = source_for(i as usize + 1);
                assert!(
                    mw.resolve_app_id("limited", source).await.is_ok(),
                    "limit {limit}: source {i} of the remaining {remaining} must be admitted \
                     despite the exhausted abuser"
                );
            }

            // Budget conservation: total admissions across ALL sources equal
            // exactly the configured ceiling, and any further source is
            // rejected by the app window.
            let overflow = source_for(remaining as usize + 1);
            assert!(
                matches!(
                    mw.resolve_app_id("limited", overflow).await,
                    Err(AuthError::RateLimitExceeded)
                ),
                "limit {limit}: the app ceiling must reject once {limit} admissions are spent"
            );

            // The rejected handshake consumed nothing in either window: the
            // overflow source's share window was never even created, and the
            // abuser's repeated retries add no stamps (they are refused by
            // the source probe — its share is spent — before the app probe
            // is even consulted; rejections are free, the pre-split
            // single-window contract, now held across both dimensions).
            assert_eq!(
                mw.rate_limiter
                    .window_len(&source_window_key("limited", overflow)),
                0,
                "limit {limit}: an app-rejected handshake must not create the source window"
            );
            let abuser_stamps_before = mw
                .rate_limiter
                .window_len(&source_window_key("limited", source_for(0)));
            for _ in 0..3 {
                assert!(matches!(
                    mw.resolve_app_id("limited", source_for(0)).await,
                    Err(AuthError::RateLimitExceeded)
                ));
            }
            assert_eq!(
                mw.rate_limiter
                    .window_len(&source_window_key("limited", source_for(0))),
                abuser_stamps_before,
                "limit {limit}: rejected retries must not stamp the source window"
            );
        }
    }

    /// Sources and apps key independent windows: one source exhausting its
    /// share for one app leaves every other source AND every other app's
    /// window untouched, and a pair key can never alias an app key (NUL is
    /// unreachable inside an app ID — the log-safety gate rejects it).
    #[tokio::test]
    async fn source_and_app_windows_are_independent_and_alias_free() {
        let entries = vec![
            AppRegistrationEntry {
                app_id: "limited".to_string(),
                app_name: "Limited App".to_string(),
                max_rooms: None,
                max_players_per_room: None,
                rate_limit_per_minute: Some(4),
                max_relay_bytes: None,
            },
            AppRegistrationEntry {
                app_id: "other".to_string(),
                app_name: "Other App".to_string(),
                max_rooms: None,
                max_players_per_room: None,
                rate_limit_per_minute: Some(2),
                max_relay_bytes: None,
            },
        ];
        let mw = AppIdAllowlist::new(entries).expect("unique app IDs");

        // One source exhausts its share (4/2 → 2) for `limited`.
        assert!(mw.resolve_app_id("limited", LOCALHOST).await.is_ok());
        assert!(mw.resolve_app_id("limited", LOCALHOST).await.is_ok());
        assert!(matches!(
            mw.resolve_app_id("limited", LOCALHOST).await,
            Err(AuthError::RateLimitExceeded)
        ));

        // A different source of the same app is unaffected (app ceiling 4 has
        // room for it).
        assert!(mw.resolve_app_id("limited", OTHER).await.is_ok());
        // The same exhausted source on a DIFFERENT app is unaffected.
        assert!(mw.resolve_app_id("other", LOCALHOST).await.is_ok());
        // IPv6 sources key their own windows too (app still has headroom).
        assert!(mw
            .resolve_app_id("limited", IpAddr::V6(Ipv6Addr::LOCALHOST))
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn rate_limit_rejection_updates_server_metrics() {
        let entries = vec![AppRegistrationEntry {
            app_id: "limited".to_string(),
            app_name: "Limited App".to_string(),
            max_rooms: None,
            max_players_per_room: None,
            rate_limit_per_minute: Some(1),
            max_relay_bytes: None,
        }];
        let metrics = Arc::new(crate::metrics::ServerMetrics::new());
        let mw = AppIdAllowlist::with_metrics(entries, metrics.clone()).expect("unique app IDs");

        assert!(mw.resolve_app_id("limited", LOCALHOST).await.is_ok());
        assert!(matches!(
            mw.resolve_app_id("limited", LOCALHOST).await,
            Err(AuthError::RateLimitExceeded)
        ));

        let snapshot = metrics.snapshot().await.rate_limiting;
        assert_eq!(snapshot.rate_limit_rejections, 1);
        assert_eq!(snapshot.auth_rejections, 1);
        assert_eq!(snapshot.room_creation_rejections, 0);
    }

    /// Pins the advertised-vs-enforced pairing for a default-limit app: the
    /// `Authenticated.rate_limits` projection advertises the default
    /// 1000/min, while enforcement stays off — omitting
    /// `rate_limit_per_minute` is the "unlimited" configuration. Both halves
    /// of that contract are pinned together so a future change to either
    /// side must consciously update this test.
    #[tokio::test]
    async fn default_limit_app_advertises_projection_but_enforces_nothing() {
        let entries = vec![AppRegistrationEntry {
            app_id: "unlimited".to_string(),
            app_name: "Unlimited App".to_string(),
            max_rooms: None,
            max_players_per_room: None,
            rate_limit_per_minute: None,
            max_relay_bytes: None,
        }];
        let mw = AppIdAllowlist::new(entries).expect("unique app IDs");

        let info = mw.resolve_app_id("unlimited", LOCALHOST).await.unwrap();
        assert_eq!(info.rate_limits.per_minute, DEFAULT_RATE_LIMIT_PER_MINUTE);
        assert_eq!(
            info.rate_limits.per_hour,
            DEFAULT_RATE_LIMIT_PER_MINUTE * 60
        );
        assert_eq!(
            info.rate_limits.per_day,
            DEFAULT_RATE_LIMIT_PER_MINUTE * 1440
        );
        assert_eq!(
            info.rate_limit_per_minute, None,
            "enforcement stays off for an entry without an explicit limit"
        );

        // More resolves than the advertised budget: none are limited.
        for _ in 0..(DEFAULT_RATE_LIMIT_PER_MINUTE + 4) {
            assert!(mw.resolve_app_id("unlimited", LOCALHOST).await.is_ok());
        }
    }

    #[tokio::test]
    async fn deterministic_uuid_for_same_app_id() {
        let mw = AppIdAllowlist::new(sample_entries()).expect("unique app IDs");
        let info1 = mw.resolve_app_id("game-1", LOCALHOST).await.unwrap();
        let info2 = mw.resolve_app_id("game-1", LOCALHOST).await.unwrap();
        assert_eq!(info1.id, info2.id);
    }

    #[tokio::test]
    async fn default_rate_limits_for_app_without_explicit_limit() {
        let mw = AppIdAllowlist::new(sample_entries()).expect("unique app IDs");
        let info = mw.resolve_app_id("game-2", LOCALHOST).await.unwrap();
        assert_eq!(info.rate_limits.per_minute, DEFAULT_RATE_LIMIT_PER_MINUTE);
    }

    /// Pins the documented open-policy divergence: a valid UUID sent as
    /// `app_id` is used verbatim, so the client chooses the application UUID.
    /// Enforced mode never does this (the UUID is always derived from the app
    /// ID string). Acceptable only because nothing consumes open-mode
    /// application identity as an authority; a future consumer must revisit.
    #[tokio::test]
    async fn disabled_app_id_parsed_as_uuid_when_valid() {
        let mw = AppIdAllowlist::disabled();
        let uuid_str = "550e8400-e29b-41d4-a716-446655440000";
        let info = mw.resolve_app_id(uuid_str, LOCALHOST).await.unwrap();
        assert_eq!(info.id.to_string(), uuid_str);
    }

    #[tokio::test]
    async fn disabled_non_uuid_app_id_gets_deterministic_id() {
        let mw = AppIdAllowlist::disabled();
        let info1 = mw.resolve_app_id("my-game", LOCALHOST).await.unwrap();
        let info2 = mw.resolve_app_id("my-game", LOCALHOST).await.unwrap();
        assert_eq!(info1.id, info2.id);
    }
}
