use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{Duration, Instant};
use uuid::Uuid;

/// Rate limiting configuration
///
/// All budgets use **fixed-window** accounting: each player's counters reset
/// to zero together whenever `time_window` has elapsed since that player's
/// window started. A player can therefore spend the entire budget at the end
/// of one window and again immediately at the start of the next — up to twice
/// the configured count across a window boundary. This differs from the
/// handshake limiter (`crate::auth::rate_limiter`), which trims individual
/// timestamps and so enforces a true sliding "per minute" rate.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Maximum number of room creation requests per fixed time window
    pub max_room_creations: u32,
    /// Time window for rate limiting
    pub time_window: Duration,
    /// Shared maximum room-creation, seated-join, and spectator-join attempts
    /// per fixed time window.
    pub max_join_attempts: u32,
    /// Maximum number of WebRTC signaling messages per fixed time window
    pub max_signals: u32,
    /// Detailed rejected-signal responses per fixed time window before
    /// generic rate-limit errors.
    pub max_signal_errors: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_room_creations: 5, // per fixed 60-second window
            time_window: Duration::from_secs(60),
            max_join_attempts: 20, // per fixed 60-second window
            max_signals: 600,      // generous for trickle-ICE (~10/sec over the 60s window)
            max_signal_errors: 60, // detailed rejection responses before generic errors
        }
    }
}

/// Rate limiter entry for tracking requests
#[derive(Debug, Clone)]
struct RateLimitEntry {
    /// Number of room creation requests in current window
    room_creations: u32,
    /// Number of total join attempts in current window
    join_attempts: u32,
    /// Number of WebRTC signaling messages in current window
    signals: u32,
    /// Number of detailed rejected-signal responses in the current window.
    signal_errors: u32,
    /// Window start time
    window_start: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoomCreationLimit {
    Creations,
    Joins,
}

impl RateLimitEntry {
    fn new() -> Self {
        Self {
            room_creations: 0,
            join_attempts: 0,
            signals: 0,
            signal_errors: 0,
            window_start: Instant::now(),
        }
    }

    /// Reset the rate limit window if enough time has passed.
    ///
    /// Fixed-window semantics: every budget resets together, so a client can
    /// burst up to twice the configured count across a window boundary
    /// (documented trade-off, not a bug).
    fn maybe_reset_window(&mut self, config: &RateLimitConfig) {
        if self.window_start.elapsed() >= config.time_window {
            self.room_creations = 0;
            self.join_attempts = 0;
            self.signals = 0;
            self.signal_errors = 0;
            self.window_start = Instant::now();
        }
    }

    /// Check if room creation is allowed and increment counter
    fn try_room_creation(&mut self, config: &RateLimitConfig) -> Result<(), RoomCreationLimit> {
        self.maybe_reset_window(config);
        if self.room_creations >= config.max_room_creations {
            return Err(RoomCreationLimit::Creations);
        }
        if self.join_attempts >= config.max_join_attempts {
            return Err(RoomCreationLimit::Joins);
        }

        self.room_creations = self.room_creations.saturating_add(1);
        self.join_attempts = self.join_attempts.saturating_add(1);
        Ok(())
    }

    /// Check if join attempt is allowed and increment counter
    fn try_join_attempt(&mut self, config: &RateLimitConfig) -> bool {
        self.maybe_reset_window(config);
        if self.join_attempts < config.max_join_attempts {
            self.join_attempts = self.join_attempts.saturating_add(1);
            true
        } else {
            false
        }
    }

    /// Check if a signaling message is allowed and increment counter
    fn try_signal(&mut self, config: &RateLimitConfig) -> bool {
        self.maybe_reset_window(config);
        if self.signals < config.max_signals {
            self.signals = self.signals.saturating_add(1);
            true
        } else {
            false
        }
    }

    /// Check if a signaling message would be allowed without incrementing.
    fn signal_available(&mut self, config: &RateLimitConfig) -> bool {
        self.maybe_reset_window(config);
        self.signals < config.max_signals
    }

    /// Reserve one detailed rejected-signal response.
    fn try_signal_error(&mut self, config: &RateLimitConfig) -> bool {
        self.maybe_reset_window(config);
        if self.signal_errors < config.max_signal_errors {
            self.signal_errors = self.signal_errors.saturating_add(1);
            true
        } else {
            false
        }
    }

    /// Get remaining time until window resets
    fn time_until_reset(&self, config: &RateLimitConfig) -> Duration {
        let elapsed = self.window_start.elapsed();
        // Use saturating_sub to handle potential Duration underflow safely
        config.time_window.saturating_sub(elapsed)
    }
}

/// Rate limiter for room operations.
///
/// Fixed-window accounting per [`RateLimitConfig`]: all of a player's
/// budgets reset together when the window elapses, so a boundary burst can
/// spend up to twice the configured count across adjacent windows. The
/// handshake limiter in `crate::auth::rate_limiter` is separately enforced
/// as a sliding window.
pub struct RoomRateLimiter {
    config: RateLimitConfig,
    /// Rate limit entries by player ID
    entries: Arc<RwLock<HashMap<Uuid, RateLimitEntry>>>,
    metrics: Option<Arc<crate::metrics::ServerMetrics>>,
}

impl RoomRateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self::new_inner(config, None)
    }

    fn new_inner(
        mut config: RateLimitConfig,
        metrics: Option<Arc<crate::metrics::ServerMetrics>>,
    ) -> Self {
        // This type is a public library API, so preserve a usable invariant
        // when callers bypass the binary configuration validator.
        if config.time_window.is_zero() {
            config.time_window = Duration::from_secs(1);
        }
        Self {
            config,
            entries: Arc::new(RwLock::new(HashMap::new())),
            metrics,
        }
    }

    pub(crate) fn with_metrics(
        config: RateLimitConfig,
        metrics: Arc<crate::metrics::ServerMetrics>,
    ) -> Self {
        Self::new_inner(config, Some(metrics))
    }

    fn record_rejection(&self, kind: crate::metrics::RateLimitRejection) {
        if let Some(metrics) = &self.metrics {
            metrics.record_rate_limit_rejection(kind);
        }
    }

    /// Check if a room creation request is allowed for the given player
    pub async fn check_room_creation(&self, player_id: &Uuid) -> Result<(), RateLimitError> {
        let mut entries = self.entries.write().await;
        let entry = entries
            .entry(*player_id)
            .or_insert_with(RateLimitEntry::new);

        match entry.try_room_creation(&self.config) {
            Ok(()) => Ok(()),
            Err(limit) => {
                let retry_after = entry.time_until_reset(&self.config);
                let (kind, error) = match limit {
                    RoomCreationLimit::Creations => (
                        crate::metrics::RateLimitRejection::RoomCreation,
                        RateLimitError::RoomCreationLimitExceeded { retry_after },
                    ),
                    RoomCreationLimit::Joins => (
                        crate::metrics::RateLimitRejection::JoinAttempt,
                        RateLimitError::JoinLimitExceeded { retry_after },
                    ),
                };
                self.record_rejection(kind);
                Err(error)
            }
        }
    }

    /// Check if a seated or spectator join attempt is allowed for the player.
    pub async fn check_join_attempt(&self, player_id: &Uuid) -> Result<(), RateLimitError> {
        let mut entries = self.entries.write().await;
        let entry = entries
            .entry(*player_id)
            .or_insert_with(RateLimitEntry::new);

        if entry.try_join_attempt(&self.config) {
            Ok(())
        } else {
            let reset_time = entry.time_until_reset(&self.config);
            self.record_rejection(crate::metrics::RateLimitRejection::JoinAttempt);
            Err(RateLimitError::JoinLimitExceeded {
                retry_after: reset_time,
            })
        }
    }

    /// Check if a WebRTC signaling message is allowed for the given player
    pub async fn check_signal(&self, player_id: &Uuid) -> Result<(), RateLimitError> {
        let mut entries = self.entries.write().await;
        let entry = entries
            .entry(*player_id)
            .or_insert_with(RateLimitEntry::new);

        if entry.try_signal(&self.config) {
            Ok(())
        } else {
            let reset_time = entry.time_until_reset(&self.config);
            self.record_rejection(crate::metrics::RateLimitRejection::Signal);
            Err(RateLimitError::SignalLimitExceeded {
                retry_after: reset_time,
            })
        }
    }

    /// Check if a WebRTC signaling message would be allowed without consuming a slot.
    ///
    /// This is only a preflight; callers must still use [`Self::check_signal`]
    /// immediately before dispatch because another task can consume the final
    /// slot between the preflight and the send. It consumes no budget slot,
    /// but it DOES record its rejection: a failed preflight means the caller
    /// drops the event right here (the consuming check is never reached), so
    /// this gate owns that drop's attribution. Whichever gate fires, a dropped
    /// fan-out is counted exactly once.
    pub async fn check_signal_available(&self, player_id: &Uuid) -> Result<(), RateLimitError> {
        let mut entries = self.entries.write().await;
        let entry = entries
            .entry(*player_id)
            .or_insert_with(RateLimitEntry::new);

        if entry.signal_available(&self.config) {
            Ok(())
        } else {
            let reset_time = entry.time_until_reset(&self.config);
            self.record_rejection(crate::metrics::RateLimitRejection::Signal);
            Err(RateLimitError::SignalLimitExceeded {
                retry_after: reset_time,
            })
        }
    }

    /// Reserve a detailed rejected-signal response for the given player.
    pub async fn check_signal_error(&self, player_id: &Uuid) -> Result<(), RateLimitError> {
        let mut entries = self.entries.write().await;
        let entry = entries
            .entry(*player_id)
            .or_insert_with(RateLimitEntry::new);

        if entry.try_signal_error(&self.config) {
            Ok(())
        } else {
            let reset_time = entry.time_until_reset(&self.config);
            self.record_rejection(crate::metrics::RateLimitRejection::SignalError);
            Err(RateLimitError::SignalErrorLimitExceeded {
                retry_after: reset_time,
            })
        }
    }

    /// Clean up old entries to prevent memory leaks
    pub async fn cleanup_old_entries(&self) {
        let mut entries = self.entries.write().await;
        let now = Instant::now();

        // Remove entries that haven't been used for 2x the time window
        let cleanup_threshold = self.config.time_window.saturating_mul(2);
        entries.retain(|_, entry| now.duration_since(entry.window_start) < cleanup_threshold);
    }

    /// Period of the background cleanup sweep.
    ///
    /// Clamped to a 1-second floor. `time_window` is validated `> 0` at startup
    /// (`validate_config_security`), but `RoomRateLimiter` is part of the public
    /// API and may be constructed directly (tests, library embedders) with a
    /// zero window, and `tokio::time::interval` panics on a zero period. Mirrors
    /// the dashboard-cache `.max(..)` zero-guard.
    fn cleanup_interval(&self) -> Duration {
        self.config.time_window.max(Duration::from_secs(1))
    }

    /// Start a background task to periodically clean up old entries
    pub fn start_cleanup_task(
        self: Arc<Self>,
    ) -> Result<tokio::task::JoinHandle<()>, tokio::runtime::TryCurrentError> {
        let cleanup_interval = self.cleanup_interval();
        let rate_limiter = Arc::downgrade(&self);
        let runtime = tokio::runtime::Handle::try_current()?;
        Ok(runtime.spawn(async move {
            let mut interval = tokio::time::interval(cleanup_interval);
            loop {
                interval.tick().await;
                let Some(rate_limiter) = rate_limiter.upgrade() else {
                    break;
                };
                rate_limiter.cleanup_old_entries().await;
            }
        }))
    }

    /// Get current stats for a player (for debugging/monitoring)
    pub async fn get_player_stats(&self, player_id: &Uuid) -> Option<PlayerRateStats> {
        let entries = self.entries.read().await;
        entries.get(player_id).map(|entry| {
            if entry.window_start.elapsed() >= self.config.time_window {
                PlayerRateStats {
                    room_creations: 0,
                    join_attempts: 0,
                    signals: 0,
                    signal_errors: 0,
                    time_until_reset: Duration::ZERO,
                }
            } else {
                PlayerRateStats {
                    room_creations: entry.room_creations,
                    join_attempts: entry.join_attempts,
                    signals: entry.signals,
                    signal_errors: entry.signal_errors,
                    time_until_reset: entry.time_until_reset(&self.config),
                }
            }
        })
    }
}

/// Rate limiting errors
#[derive(Debug, Clone)]
pub enum RateLimitError {
    RoomCreationLimitExceeded { retry_after: Duration },
    JoinLimitExceeded { retry_after: Duration },
    SignalLimitExceeded { retry_after: Duration },
    SignalErrorLimitExceeded { retry_after: Duration },
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn retry_after_secs(duration: &Duration) -> u64 {
            duration
                .as_secs()
                .saturating_add(u64::from(duration.subsec_nanos() > 0))
        }

        match self {
            Self::RoomCreationLimitExceeded { retry_after } => {
                write!(
                    f,
                    "Room creation rate limit exceeded. Try again in {} seconds.",
                    retry_after_secs(retry_after)
                )
            }
            Self::JoinLimitExceeded { retry_after } => {
                write!(
                    f,
                    "Join attempt rate limit exceeded. Try again in {} seconds.",
                    retry_after_secs(retry_after)
                )
            }
            Self::SignalLimitExceeded { retry_after } => {
                write!(
                    f,
                    "Signaling rate limit exceeded. Try again in {} seconds.",
                    retry_after_secs(retry_after)
                )
            }
            Self::SignalErrorLimitExceeded { retry_after } => {
                write!(
                    f,
                    "Too many rejected signaling messages; further rejections are reported \
                     without detail until the window resets. Valid signals are still relayed. \
                     Try again in {} seconds.",
                    retry_after_secs(retry_after)
                )
            }
        }
    }
}

impl std::error::Error for RateLimitError {}

/// Statistics for a player's rate limiting
#[derive(Debug, Clone)]
pub struct PlayerRateStats {
    pub room_creations: u32,
    pub join_attempts: u32,
    pub signals: u32,
    pub signal_errors: u32,
    pub time_until_reset: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_config() -> RateLimitConfig {
        RateLimitConfig {
            max_room_creations: 2,
            time_window: Duration::from_millis(100),
            max_join_attempts: 3,
            max_signals: 2,
            max_signal_errors: 2,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_room_creation_rate_limit() {
        let limiter = RoomRateLimiter::new(create_test_config());
        let player_id = Uuid::new_v4();

        // First two creations should succeed
        assert!(limiter.check_room_creation(&player_id).await.is_ok());
        assert!(limiter.check_room_creation(&player_id).await.is_ok());

        // Third should fail
        assert!(limiter.check_room_creation(&player_id).await.is_err());

        // Wait for window to reset
        tokio::time::advance(Duration::from_millis(150)).await;

        // Should work again
        assert!(limiter.check_room_creation(&player_id).await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn test_join_attempt_rate_limit() {
        let limiter = RoomRateLimiter::new(create_test_config());
        let player_id = Uuid::new_v4();

        // First three attempts should succeed
        assert!(limiter.check_join_attempt(&player_id).await.is_ok());
        assert!(limiter.check_join_attempt(&player_id).await.is_ok());
        assert!(limiter.check_join_attempt(&player_id).await.is_ok());

        // Fourth should fail
        assert!(limiter.check_join_attempt(&player_id).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn test_signal_rate_limit() {
        let limiter = RoomRateLimiter::new(create_test_config());
        let player_id = Uuid::new_v4();

        // First two signals should succeed (max_signals = 2).
        assert!(limiter.check_signal(&player_id).await.is_ok());
        assert!(limiter.check_signal(&player_id).await.is_ok());

        // Third should fail.
        assert!(limiter.check_signal(&player_id).await.is_err());

        // Wait for window to reset, then it should work again.
        tokio::time::advance(Duration::from_millis(150)).await;
        assert!(limiter.check_signal(&player_id).await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn test_signal_available_preflight_does_not_consume_signal_budget() {
        let metrics = Arc::new(crate::metrics::ServerMetrics::new());
        let limiter = RoomRateLimiter::with_metrics(create_test_config(), Arc::clone(&metrics));
        let player_id = Uuid::new_v4();

        assert!(limiter.check_signal_available(&player_id).await.is_ok());
        assert_eq!(
            limiter
                .get_player_stats(&player_id)
                .await
                .expect("preflight creates stats entry")
                .signals,
            0,
            "preflight must not consume a valid-signal slot"
        );

        assert!(limiter.check_signal(&player_id).await.is_ok());
        assert!(limiter.check_signal(&player_id).await.is_ok());
        assert!(limiter.check_signal_available(&player_id).await.is_err());
        assert_eq!(
            limiter
                .get_player_stats(&player_id)
                .await
                .expect("stats remain available")
                .signals,
            2,
            "over-budget preflight must still leave consumed count unchanged"
        );
        // The preflight consumes no slot, but a failed preflight IS a drop:
        // the caller returns immediately and its consuming check is never
        // reached, so this gate owns the attribution. Whichever gate fires,
        // each dropped event counts exactly once.
        assert_eq!(
            metrics.snapshot().await.rate_limiting.signal_rejections,
            1,
            "a preflight-dropped fan-out is attributed exactly once"
        );
        assert!(limiter.check_signal(&player_id).await.is_err());
        assert_eq!(
            metrics.snapshot().await.rate_limiting.signal_rejections,
            2,
            "a consuming-check-dropped event adds its own single attribution"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_signal_limit_independent_per_player() {
        let limiter = RoomRateLimiter::new(create_test_config());
        let player1 = Uuid::new_v4();
        let player2 = Uuid::new_v4();

        // Exhaust player1's signal budget.
        assert!(limiter.check_signal(&player1).await.is_ok());
        assert!(limiter.check_signal(&player1).await.is_ok());
        assert!(limiter.check_signal(&player1).await.is_err());

        // player2 is unaffected.
        assert!(limiter.check_signal(&player2).await.is_ok());
        assert!(limiter.check_signal(&player2).await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn test_signal_limit_independent_of_join_budget() {
        // Signals do not consume the join/creation budget and vice versa.
        let limiter = RoomRateLimiter::new(create_test_config());
        let player_id = Uuid::new_v4();

        assert!(limiter.check_signal(&player_id).await.is_ok());
        assert!(limiter.check_signal(&player_id).await.is_ok());
        assert!(limiter.check_signal(&player_id).await.is_err());

        // Join budget is untouched.
        assert!(limiter.check_join_attempt(&player_id).await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn test_signal_error_limit_is_separate_from_valid_signal_budget() {
        let limiter = RoomRateLimiter::new(create_test_config());
        let player_id = Uuid::new_v4();

        assert!(limiter.check_signal_error(&player_id).await.is_ok());
        assert!(limiter.check_signal_error(&player_id).await.is_ok());
        assert!(limiter.check_signal_error(&player_id).await.is_err());

        assert!(limiter.check_signal(&player_id).await.is_ok());
        assert!(limiter.check_signal(&player_id).await.is_ok());
        assert!(limiter.check_signal(&player_id).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn test_different_players_independent_limits() {
        let limiter = RoomRateLimiter::new(create_test_config());
        let player1 = Uuid::new_v4();
        let player2 = Uuid::new_v4();

        // Exhaust player1's limit
        assert!(limiter.check_room_creation(&player1).await.is_ok());
        assert!(limiter.check_room_creation(&player1).await.is_ok());
        assert!(limiter.check_room_creation(&player1).await.is_err());

        // Player2 should still be able to create rooms
        assert!(limiter.check_room_creation(&player2).await.is_ok());
        assert!(limiter.check_room_creation(&player2).await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn test_room_creation_counts_as_join_attempt() {
        let limiter = RoomRateLimiter::new(create_test_config());
        let player_id = Uuid::new_v4();

        // Create 2 rooms (which also count as join attempts)
        assert!(limiter.check_room_creation(&player_id).await.is_ok());
        assert!(limiter.check_room_creation(&player_id).await.is_ok());

        // Should have 1 more join attempt available
        assert!(limiter.check_join_attempt(&player_id).await.is_ok());

        // Now join attempts should be exhausted
        assert!(limiter.check_join_attempt(&player_id).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn room_creation_respects_creation_and_join_budgets_atomically() {
        let limiter = RoomRateLimiter::new(RateLimitConfig {
            max_room_creations: 3,
            max_join_attempts: 1,
            ..create_test_config()
        });
        let player_id = Uuid::new_v4();

        assert!(limiter.check_room_creation(&player_id).await.is_ok());
        assert!(matches!(
            limiter.check_room_creation(&player_id).await,
            Err(RateLimitError::JoinLimitExceeded { .. })
        ));

        let stats = limiter
            .get_player_stats(&player_id)
            .await
            .expect("the accepted creation creates one stats entry");
        assert_eq!(
            (stats.room_creations, stats.join_attempts),
            (1, 1),
            "a rejected compound operation must not partially advance either budget"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn room_creation_cannot_overflow_an_exhausted_join_counter() {
        let limiter = RoomRateLimiter::new(RateLimitConfig {
            max_room_creations: u32::MAX,
            max_join_attempts: u32::MAX,
            ..create_test_config()
        });
        let player_id = Uuid::new_v4();
        limiter.entries.write().await.insert(
            player_id,
            RateLimitEntry {
                room_creations: 0,
                join_attempts: u32::MAX,
                window_start: Instant::now(),
                ..RateLimitEntry::new()
            },
        );

        assert!(matches!(
            limiter.check_room_creation(&player_id).await,
            Err(RateLimitError::JoinLimitExceeded { .. })
        ));
        let stats = limiter
            .get_player_stats(&player_id)
            .await
            .expect("the preloaded stats entry remains present");
        assert_eq!(
            (stats.room_creations, stats.join_attempts),
            (0, u32::MAX),
            "an exhausted counter must reject instead of wrapping or partially advancing"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn zero_window_does_not_disable_enforcement_for_library_callers() {
        let limiter = RoomRateLimiter::new(RateLimitConfig {
            max_room_creations: 1,
            max_join_attempts: 1,
            time_window: Duration::ZERO,
            ..create_test_config()
        });
        let player_id = Uuid::new_v4();

        assert!(limiter.check_room_creation(&player_id).await.is_ok());
        assert!(matches!(
            limiter.check_room_creation(&player_id).await,
            Err(RateLimitError::RoomCreationLimitExceeded { .. })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn rejected_budgets_are_reported_by_actual_source() {
        let metrics = Arc::new(crate::metrics::ServerMetrics::new());
        let player_id = Uuid::new_v4();

        let room_creation = RoomRateLimiter::with_metrics(
            RateLimitConfig {
                max_room_creations: 0,
                ..create_test_config()
            },
            metrics.clone(),
        );
        assert!(room_creation.check_room_creation(&player_id).await.is_err());

        let join_attempt = RoomRateLimiter::with_metrics(
            RateLimitConfig {
                max_room_creations: 1,
                max_join_attempts: 0,
                ..create_test_config()
            },
            metrics.clone(),
        );
        assert!(matches!(
            join_attempt.check_room_creation(&player_id).await,
            Err(RateLimitError::JoinLimitExceeded { .. })
        ));

        let signal = RoomRateLimiter::with_metrics(
            RateLimitConfig {
                max_signals: 0,
                max_signal_errors: 0,
                ..create_test_config()
            },
            metrics.clone(),
        );
        assert!(signal.check_signal(&player_id).await.is_err());
        assert!(signal.check_signal_error(&player_id).await.is_err());

        let snapshot = metrics.snapshot().await.rate_limiting;
        assert_eq!(snapshot.rate_limit_rejections, 4);
        assert_eq!(snapshot.room_creation_rejections, 1);
        assert_eq!(snapshot.join_attempt_rejections, 1);
        assert_eq!(snapshot.signal_rejections, 1);
        assert_eq!(snapshot.signal_error_rejections, 1);
        assert_eq!(snapshot.auth_rejections, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn test_cleanup_old_entries() {
        let config = RateLimitConfig {
            max_room_creations: 1,
            time_window: Duration::from_millis(50),
            max_join_attempts: 1,
            max_signals: 1,
            max_signal_errors: 1,
        };
        let limiter = RoomRateLimiter::new(config);
        let player_id = Uuid::new_v4();

        // Create an entry
        let _ = limiter.check_room_creation(&player_id).await;

        // Entry should exist
        assert!(limiter.get_player_stats(&player_id).await.is_some());

        // Wait for cleanup threshold (2x time window)
        tokio::time::advance(Duration::from_millis(150)).await;

        // Run cleanup
        limiter.cleanup_old_entries().await;

        // Entry should be cleaned up
        assert!(limiter.get_player_stats(&player_id).await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn cleanup_threshold_saturates_for_maximum_window() {
        let limiter = RoomRateLimiter::new(RateLimitConfig {
            time_window: Duration::MAX,
            ..create_test_config()
        });
        let player_id = Uuid::new_v4();
        assert!(limiter.check_room_creation(&player_id).await.is_ok());

        limiter.cleanup_old_entries().await;

        assert!(limiter.get_player_stats(&player_id).await.is_some());
    }

    #[test]
    fn cleanup_interval_clamps_zero_window_to_nonzero() {
        // A zero `time_window` is rejected by config validation, but the limiter
        // is publicly constructible; the cleanup interval must still be non-zero
        // so the background `tokio::time::interval` can never panic.
        let config = RateLimitConfig {
            time_window: Duration::ZERO,
            ..create_test_config()
        };
        let limiter = RoomRateLimiter::new(config);
        assert!(
            !limiter.cleanup_interval().is_zero(),
            "a zero rate-limit window must clamp to a non-zero cleanup interval"
        );
    }

    #[test]
    fn cleanup_task_without_runtime_returns_error_instead_of_panicking() {
        let limiter = Arc::new(RoomRateLimiter::new(create_test_config()));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            limiter.start_cleanup_task()
        }));
        assert!(result.is_ok());
        assert!(result.ok().is_some_and(|task| task.is_err()));
    }

    #[tokio::test(start_paused = true)]
    async fn test_player_stats() {
        let limiter = RoomRateLimiter::new(create_test_config());
        let player_id = Uuid::new_v4();

        // Initially no stats
        assert!(limiter.get_player_stats(&player_id).await.is_none());

        // After some activity, stats should be available
        let _ = limiter.check_room_creation(&player_id).await;
        let _ = limiter.check_join_attempt(&player_id).await;
        let _ = limiter.check_signal(&player_id).await;
        let _ = limiter.check_signal_error(&player_id).await;

        let stats = limiter.get_player_stats(&player_id).await.unwrap();
        assert_eq!(stats.room_creations, 1);
        assert_eq!(stats.join_attempts, 2); // Room creation counts as join attempt too
        assert_eq!(stats.signals, 1);
        assert_eq!(stats.signal_errors, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn player_stats_reset_when_the_window_expires() {
        let limiter = RoomRateLimiter::new(create_test_config());
        let player_id = Uuid::new_v4();
        assert!(limiter.check_room_creation(&player_id).await.is_ok());

        tokio::time::advance(Duration::from_millis(101)).await;
        let stats = limiter
            .get_player_stats(&player_id)
            .await
            .expect("the entry remains until cleanup");

        assert_eq!(stats.room_creations, 0);
        assert_eq!(stats.join_attempts, 0);
        assert_eq!(stats.time_until_reset, Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn player_stats_observation_does_not_move_the_enforcement_window() {
        let limiter = RoomRateLimiter::new(RateLimitConfig {
            max_room_creations: 1,
            max_join_attempts: 2,
            ..create_test_config()
        });
        let player_id = Uuid::new_v4();
        assert!(limiter.check_room_creation(&player_id).await.is_ok());

        tokio::time::advance(Duration::from_millis(101)).await;
        assert_eq!(
            limiter
                .get_player_stats(&player_id)
                .await
                .expect("stats entry remains")
                .room_creations,
            0
        );

        tokio::time::advance(Duration::from_millis(49)).await;
        assert!(limiter.check_room_creation(&player_id).await.is_ok());
        tokio::time::advance(Duration::from_millis(52)).await;
        assert!(
            limiter.check_room_creation(&player_id).await.is_err(),
            "the accepted request, not the earlier stats read, must anchor the new window"
        );
    }

    #[test]
    fn signal_error_budget_guidance_is_truthful_about_detail_suppression() {
        // #454: the error budget suppresses only the *detail* of rejection
        // responses. A player whose peers flap burns this budget on routine
        // validation failures, so the guidance must not borrow the
        // valid-signal-budget wording ("Signaling rate limit exceeded") that
        // would tell a healthy client to back off its trickle-ICE.
        let error = RateLimitError::SignalErrorLimitExceeded {
            retry_after: Duration::from_secs(30),
        };
        let text = error.to_string();

        assert!(
            text.contains("rejected signaling messages"),
            "guidance must describe suppression of rejected-signal details, got: {text}"
        );
        assert!(
            text.contains("Valid signals are still relayed"),
            "guidance must state that valid signaling is unaffected, got: {text}"
        );
        assert!(
            text.contains("Try again in 30 seconds"),
            "guidance must keep the retry-after advice, got: {text}"
        );
        assert!(
            !text.contains("Signaling rate limit exceeded"),
            "guidance must not borrow the valid-signal-budget wording, got: {text}"
        );
    }

    #[test]
    fn retry_after_display_rounds_a_live_subsecond_window_up() {
        let error = RateLimitError::JoinLimitExceeded {
            retry_after: Duration::from_millis(1),
        };

        assert_eq!(
            error.to_string(),
            "Join attempt rate limit exceeded. Try again in 1 seconds."
        );
    }
}
