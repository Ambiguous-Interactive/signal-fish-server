use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{Duration, Instant};
use uuid::Uuid;

/// Rate limiting configuration
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Maximum number of room creation requests per time window
    pub max_room_creations: u32,
    /// Time window for rate limiting
    pub time_window: Duration,
    /// Maximum number of join attempts per time window (including existing rooms)
    pub max_join_attempts: u32,
    /// Maximum number of WebRTC signaling messages per time window
    pub max_signals: u32,
    /// Maximum number of rejected WebRTC signaling attempts per time window
    pub max_signal_errors: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_room_creations: 5, // 5 room creations per minute
            time_window: Duration::from_secs(60),
            max_join_attempts: 20, // 20 join attempts per minute
            max_signals: 600,      // generous for trickle-ICE (~10/sec over the 60s window)
            max_signal_errors: 60, // bound malformed/unsupported signal spam separately
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
    /// Number of rejected WebRTC signaling attempts in current window
    signal_errors: u32,
    /// Window start time
    window_start: Instant,
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

    /// Reset the rate limit window if enough time has passed
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
    fn try_room_creation(&mut self, config: &RateLimitConfig) -> bool {
        self.maybe_reset_window(config);
        if self.room_creations < config.max_room_creations {
            self.room_creations += 1;
            self.join_attempts += 1;
            true
        } else {
            false
        }
    }

    /// Check if join attempt is allowed and increment counter
    fn try_join_attempt(&mut self, config: &RateLimitConfig) -> bool {
        self.maybe_reset_window(config);
        if self.join_attempts < config.max_join_attempts {
            self.join_attempts += 1;
            true
        } else {
            false
        }
    }

    /// Check if a signaling message is allowed and increment counter
    fn try_signal(&mut self, config: &RateLimitConfig) -> bool {
        self.maybe_reset_window(config);
        if self.signals < config.max_signals {
            self.signals += 1;
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

    /// Check if a rejected signaling attempt is allowed and increment counter
    fn try_signal_error(&mut self, config: &RateLimitConfig) -> bool {
        self.maybe_reset_window(config);
        if self.signal_errors < config.max_signal_errors {
            self.signal_errors += 1;
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

/// Rate limiter for room operations
pub struct RoomRateLimiter {
    config: RateLimitConfig,
    /// Rate limit entries by player ID
    entries: Arc<RwLock<HashMap<Uuid, RateLimitEntry>>>,
}

impl RoomRateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            entries: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Check if a room creation request is allowed for the given player
    pub async fn check_room_creation(&self, player_id: &Uuid) -> Result<(), RateLimitError> {
        let mut entries = self.entries.write().await;
        let entry = entries
            .entry(*player_id)
            .or_insert_with(RateLimitEntry::new);

        if entry.try_room_creation(&self.config) {
            Ok(())
        } else {
            let reset_time = entry.time_until_reset(&self.config);
            Err(RateLimitError::RoomCreationLimitExceeded {
                retry_after: reset_time,
            })
        }
    }

    /// Check if a join attempt is allowed for the given player
    pub async fn check_join_attempt(&self, player_id: &Uuid) -> Result<(), RateLimitError> {
        let mut entries = self.entries.write().await;
        let entry = entries
            .entry(*player_id)
            .or_insert_with(RateLimitEntry::new);

        if entry.try_join_attempt(&self.config) {
            Ok(())
        } else {
            let reset_time = entry.time_until_reset(&self.config);
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
            Err(RateLimitError::SignalLimitExceeded {
                retry_after: reset_time,
            })
        }
    }

    /// Check if a WebRTC signaling message would be allowed without consuming a slot.
    ///
    /// This is only a preflight; callers must still use [`Self::check_signal`]
    /// immediately before dispatch because another task can consume the final
    /// slot between the preflight and the send.
    pub async fn check_signal_available(&self, player_id: &Uuid) -> Result<(), RateLimitError> {
        let mut entries = self.entries.write().await;
        let entry = entries
            .entry(*player_id)
            .or_insert_with(RateLimitEntry::new);

        if entry.signal_available(&self.config) {
            Ok(())
        } else {
            let reset_time = entry.time_until_reset(&self.config);
            Err(RateLimitError::SignalLimitExceeded {
                retry_after: reset_time,
            })
        }
    }

    /// Check if a rejected WebRTC signaling attempt is allowed for the given player
    pub async fn check_signal_error(&self, player_id: &Uuid) -> Result<(), RateLimitError> {
        let mut entries = self.entries.write().await;
        let entry = entries
            .entry(*player_id)
            .or_insert_with(RateLimitEntry::new);

        if entry.try_signal_error(&self.config) {
            Ok(())
        } else {
            let reset_time = entry.time_until_reset(&self.config);
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
        let cleanup_threshold = self.config.time_window * 2;
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
    pub fn start_cleanup_task(self: Arc<Self>) {
        let rate_limiter = Arc::clone(&self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(rate_limiter.cleanup_interval());
            loop {
                interval.tick().await;
                rate_limiter.cleanup_old_entries().await;
            }
        });
    }

    /// Get current stats for a player (for debugging/monitoring)
    pub async fn get_player_stats(&self, player_id: &Uuid) -> Option<PlayerRateStats> {
        let entries = self.entries.read().await;
        entries.get(player_id).map(|entry| PlayerRateStats {
            room_creations: entry.room_creations,
            join_attempts: entry.join_attempts,
            signals: entry.signals,
            signal_errors: entry.signal_errors,
            time_until_reset: entry.time_until_reset(&self.config),
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
        match self {
            Self::RoomCreationLimitExceeded { retry_after } => {
                write!(
                    f,
                    "Room creation rate limit exceeded. Try again in {} seconds.",
                    retry_after.as_secs()
                )
            }
            Self::JoinLimitExceeded { retry_after } => {
                write!(
                    f,
                    "Join attempt rate limit exceeded. Try again in {} seconds.",
                    retry_after.as_secs()
                )
            }
            Self::SignalLimitExceeded { retry_after } => {
                write!(
                    f,
                    "Signaling rate limit exceeded. Try again in {} seconds.",
                    retry_after.as_secs()
                )
            }
            Self::SignalErrorLimitExceeded { retry_after } => {
                write!(
                    f,
                    "Signaling error rate limit exceeded. Try again in {} seconds.",
                    retry_after.as_secs()
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

    #[tokio::test]
    async fn test_room_creation_rate_limit() {
        let limiter = RoomRateLimiter::new(create_test_config());
        let player_id = Uuid::new_v4();

        // First two creations should succeed
        assert!(limiter.check_room_creation(&player_id).await.is_ok());
        assert!(limiter.check_room_creation(&player_id).await.is_ok());

        // Third should fail
        assert!(limiter.check_room_creation(&player_id).await.is_err());

        // Wait for window to reset
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Should work again
        assert!(limiter.check_room_creation(&player_id).await.is_ok());
    }

    #[tokio::test]
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

    #[tokio::test]
    async fn test_signal_rate_limit() {
        let limiter = RoomRateLimiter::new(create_test_config());
        let player_id = Uuid::new_v4();

        // First two signals should succeed (max_signals = 2).
        assert!(limiter.check_signal(&player_id).await.is_ok());
        assert!(limiter.check_signal(&player_id).await.is_ok());

        // Third should fail.
        assert!(limiter.check_signal(&player_id).await.is_err());

        // Wait for window to reset, then it should work again.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(limiter.check_signal(&player_id).await.is_ok());
    }

    #[tokio::test]
    async fn test_signal_available_preflight_does_not_consume_signal_budget() {
        let limiter = RoomRateLimiter::new(create_test_config());
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
    }

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
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

    #[tokio::test]
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
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Run cleanup
        limiter.cleanup_old_entries().await;

        // Entry should be cleaned up
        assert!(limiter.get_player_stats(&player_id).await.is_none());
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

    #[tokio::test]
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
}
