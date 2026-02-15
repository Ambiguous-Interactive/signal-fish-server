//! In-memory per-application rate limiter using a sliding-window counter.

use super::error::AuthError;
use dashmap::DashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Sliding-window rate limiter backed by `DashMap`.
///
/// Each application ID maps to a `VecDeque<Instant>` that records the
/// timestamps of recent requests. When `check_rate_limit` is called the
/// window is trimmed to the last 60 seconds before comparing the count
/// against the configured limit.
pub struct InMemoryRateLimiter {
    windows: DashMap<String, VecDeque<Instant>>,
    cleanup_interval: Duration,
    window_duration: Duration,
}

impl InMemoryRateLimiter {
    /// Create a new rate limiter with the given cleanup interval.
    pub fn new(cleanup_interval: Duration) -> Self {
        Self {
            windows: DashMap::new(),
            cleanup_interval,
            window_duration: Duration::from_secs(60),
        }
    }

    /// Override the sliding-window duration (default 60s). Only intended for
    /// tests that need to verify expiration with a very short window.
    #[cfg(test)]
    pub fn with_window_duration(mut self, window: Duration) -> Self {
        self.window_duration = window;
        self
    }

    /// Check whether `app_id` has exceeded `limit_per_minute` requests in the
    /// last 60 seconds. If the request is allowed, the current timestamp is
    /// recorded and `Ok(())` is returned. Otherwise
    /// `Err(AuthError::RateLimitExceeded)` is returned.
    pub fn check_rate_limit(&self, app_id: &str, limit_per_minute: u32) -> Result<(), AuthError> {
        let now = Instant::now();
        let window = self.window_duration;

        let mut entry = self.windows.entry(app_id.to_owned()).or_default();
        let timestamps = entry.value_mut();

        // Trim expired entries from the front of the deque.
        while let Some(&front) = timestamps.front() {
            if now.duration_since(front) > window {
                timestamps.pop_front();
            } else {
                break;
            }
        }

        if timestamps.len() >= limit_per_minute as usize {
            return Err(AuthError::RateLimitExceeded);
        }

        timestamps.push_back(now);
        Ok(())
    }

    /// Spawn a background task that periodically removes stale entries from
    /// the rate-limit map so memory usage stays bounded.
    ///
    /// Returns the `JoinHandle` so callers can abort the task during shutdown.
    pub fn start_cleanup_task(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        let interval = self.cleanup_interval;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                self.cleanup();
            }
        })
    }

    /// Remove entries whose sliding windows are completely empty (all
    /// timestamps have expired).
    pub(crate) fn cleanup(&self) {
        let now = Instant::now();
        let window = self.window_duration;

        self.windows.retain(|_key, timestamps| {
            // Trim expired entries.
            while let Some(&front) = timestamps.front() {
                if now.duration_since(front) > window {
                    timestamps.pop_front();
                } else {
                    break;
                }
            }
            // Keep the entry only if there are remaining timestamps.
            !timestamps.is_empty()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_requests_under_limit() {
        let limiter = InMemoryRateLimiter::new(Duration::from_secs(60));
        for _ in 0..5 {
            assert!(limiter.check_rate_limit("app1", 10).is_ok());
        }
    }

    #[test]
    fn rejects_requests_over_limit() {
        let limiter = InMemoryRateLimiter::new(Duration::from_secs(60));
        for _ in 0..10 {
            limiter.check_rate_limit("app1", 10).unwrap();
        }
        let result = limiter.check_rate_limit("app1", 10);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AuthError::RateLimitExceeded));
    }

    #[test]
    fn independent_limits_per_app() {
        let limiter = InMemoryRateLimiter::new(Duration::from_secs(60));
        for _ in 0..5 {
            limiter.check_rate_limit("app1", 5).unwrap();
        }
        // app1 is now at limit
        assert!(limiter.check_rate_limit("app1", 5).is_err());
        // app2 should still be fine
        assert!(limiter.check_rate_limit("app2", 5).is_ok());
    }

    #[test]
    fn cleanup_removes_empty_entries() {
        let limiter = InMemoryRateLimiter::new(Duration::from_secs(60));
        // Insert an entry then immediately make it empty by not exceeding the window
        limiter.check_rate_limit("app1", 100).unwrap();
        assert!(!limiter.windows.is_empty());

        // After cleanup, entry should still exist (timestamp is recent)
        limiter.cleanup();
        assert!(!limiter.windows.is_empty());
    }

    #[test]
    fn zero_limit_always_rejects() {
        let limiter = InMemoryRateLimiter::new(Duration::from_secs(60));
        let result = limiter.check_rate_limit("app1", 0);
        assert!(matches!(result.unwrap_err(), AuthError::RateLimitExceeded));
    }

    #[test]
    fn limit_of_one_allows_single_request() {
        let limiter = InMemoryRateLimiter::new(Duration::from_secs(60));
        assert!(limiter.check_rate_limit("app1", 1).is_ok());
        assert!(limiter.check_rate_limit("app1", 1).is_err());
    }

    #[tokio::test]
    async fn cleanup_removes_expired_entries() {
        let limiter = InMemoryRateLimiter::new(Duration::from_secs(60))
            .with_window_duration(Duration::from_millis(1));

        limiter.check_rate_limit("app1", 100).unwrap();
        assert!(!limiter.windows.is_empty());

        // Wait for the window to expire.
        tokio::time::sleep(Duration::from_millis(5)).await;

        limiter.cleanup();
        assert!(
            limiter.windows.is_empty(),
            "expired entry should have been removed by cleanup"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_rate_limit_enforcement() {
        let limiter = Arc::new(InMemoryRateLimiter::new(Duration::from_secs(60)));
        let limit: u32 = 30;
        let num_tasks: usize = 60;

        let mut handles = Vec::with_capacity(num_tasks);
        for _ in 0..num_tasks {
            let limiter = limiter.clone();
            handles.push(tokio::spawn(async move {
                limiter.check_rate_limit("contended-app", limit).is_ok()
            }));
        }

        let mut accepted = 0u32;
        for handle in handles {
            if handle.await.unwrap() {
                accepted += 1;
            }
        }

        assert_eq!(
            accepted, limit,
            "exactly {limit} requests should have been accepted, but {accepted} were"
        );
    }
}
