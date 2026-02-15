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
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_room_creations: 5, // 5 room creations per minute
            time_window: Duration::from_secs(60),
            max_join_attempts: 20, // 20 join attempts per minute
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
    /// Window start time
    window_start: Instant,
}

impl RateLimitEntry {
    fn new() -> Self {
        Self {
            room_creations: 0,
            join_attempts: 0,
            window_start: Instant::now(),
        }
    }

    /// Reset the rate limit window if enough time has passed
    fn maybe_reset_window(&mut self, config: &RateLimitConfig) {
        if self.window_start.elapsed() >= config.time_window {
            self.room_creations = 0;
            self.join_attempts = 0;
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

    /// Clean up old entries to prevent memory leaks
    pub async fn cleanup_old_entries(&self) {
        let mut entries = self.entries.write().await;
        let now = Instant::now();

        // Remove entries that haven't been used for 2x the time window
        let cleanup_threshold = self.config.time_window * 2;
        entries.retain(|_, entry| now.duration_since(entry.window_start) < cleanup_threshold);
    }

    /// Start a background task to periodically clean up old entries
    pub fn start_cleanup_task(self: Arc<Self>) {
        let rate_limiter = Arc::clone(&self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(rate_limiter.config.time_window);
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
            time_until_reset: entry.time_until_reset(&self.config),
        })
    }
}

/// Rate limiting errors
#[derive(Debug, Clone)]
pub enum RateLimitError {
    RoomCreationLimitExceeded { retry_after: Duration },
    JoinLimitExceeded { retry_after: Duration },
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
        }
    }
}

impl std::error::Error for RateLimitError {}

/// Statistics for a player's rate limiting
#[derive(Debug, Clone)]
pub struct PlayerRateStats {
    pub room_creations: u32,
    pub join_attempts: u32,
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

    #[tokio::test]
    async fn test_player_stats() {
        let limiter = RoomRateLimiter::new(create_test_config());
        let player_id = Uuid::new_v4();

        // Initially no stats
        assert!(limiter.get_player_stats(&player_id).await.is_none());

        // After some activity, stats should be available
        let _ = limiter.check_room_creation(&player_id).await;
        let _ = limiter.check_join_attempt(&player_id).await;

        let stats = limiter.get_player_stats(&player_id).await.unwrap();
        assert_eq!(stats.room_creations, 1);
        assert_eq!(stats.join_attempts, 2); // Room creation counts as join attempt too
    }
}
