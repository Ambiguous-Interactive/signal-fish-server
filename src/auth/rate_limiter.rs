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
///
/// The monotonic clock is read only inside the thin [`Self::check_rate_limit`]
/// and `cleanup` wrappers; the `*_at` variants take an explicit
/// timestamp so time-driven behavior is testable deterministically (see
/// `.llm/context-testing.md`, "Injectable time").
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

    /// Check whether `app_id` has exceeded `limit_per_minute` requests in the
    /// last 60 seconds. If the request is allowed, the current timestamp is
    /// recorded and `Ok(())` is returned. Otherwise
    /// `Err(AuthError::RateLimitExceeded)` is returned.
    pub fn check_rate_limit(&self, app_id: &str, limit_per_minute: u32) -> Result<(), AuthError> {
        self.check_rate_limit_at(app_id, limit_per_minute, Instant::now())
    }

    /// Injected-time variant of [`Self::check_rate_limit`]: the caller supplies
    /// the current monotonic timestamp so window expiry is deterministic.
    ///
    /// A recorded timestamp stops counting against the limit once
    /// `now - recorded >= window_duration` (the boundary is inclusive: a
    /// request at exactly `recorded + window_duration` no longer observes it).
    pub fn check_rate_limit_at(
        &self,
        app_id: &str,
        limit_per_minute: u32,
        now: Instant,
    ) -> Result<(), AuthError> {
        let window = self.window_duration;

        let mut entry = self.windows.entry(app_id.to_owned()).or_default();
        let timestamps = entry.value_mut();

        // Trim expired entries from the front of the deque.
        while let Some(&front) = timestamps.front() {
            if now.duration_since(front) >= window {
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
    pub fn start_cleanup_task(
        self: Arc<Self>,
    ) -> Result<tokio::task::JoinHandle<()>, tokio::runtime::TryCurrentError> {
        // The limiter is public and may be embedded without the binary's
        // configuration validation. Keep that path from silently killing its
        // maintenance task on Tokio's zero-period panic.
        let interval = self.cleanup_interval.max(Duration::from_secs(1));
        let limiter = Arc::downgrade(&self);
        let runtime = tokio::runtime::Handle::try_current()?;
        Ok(runtime.spawn(async move {
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let Some(limiter) = limiter.upgrade() else {
                    break;
                };
                limiter.cleanup();
            }
        }))
    }

    /// Remove entries whose sliding windows are completely empty (all
    /// timestamps have expired).
    pub(crate) fn cleanup(&self) {
        self.cleanup_at(Instant::now())
    }

    /// Injected-time variant of [`Self::cleanup`].
    pub(crate) fn cleanup_at(&self, now: Instant) {
        let window = self.window_duration;

        self.windows.retain(|_key, timestamps| {
            // Trim expired entries.
            while let Some(&front) = timestamps.front() {
                if now.duration_since(front) >= window {
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

    /// One stop on an injected timeline: a request placed `elapsed_ms` after
    /// the origin, expected to be admitted or rejected.
    struct Stop {
        elapsed_ms: u64,
        admitted: bool,
    }

    fn window_admission_timeline(limit: u32, stops: &[Stop]) {
        let limiter = InMemoryRateLimiter::new(Duration::from_secs(60));
        let origin = Instant::now();
        for stop in stops {
            let now = origin + Duration::from_millis(stop.elapsed_ms);
            let result = limiter.check_rate_limit_at("app", limit, now);
            assert_eq!(
                result.is_ok(),
                stop.admitted,
                "request at +{}ms (limit {limit}) should be {}",
                stop.elapsed_ms,
                if stop.admitted {
                    "admitted"
                } else {
                    "rejected"
                }
            );
        }
    }

    #[test]
    fn window_admits_up_to_limit_then_expires_at_the_inclusive_boundary() {
        let window_ms = 60_000u64;
        window_admission_timeline(
            3,
            &[
                // Stagger the initial stamps so expiry frees one slot at a time.
                Stop {
                    elapsed_ms: 0,
                    admitted: true,
                },
                Stop {
                    elapsed_ms: 1_000,
                    admitted: true,
                },
                Stop {
                    elapsed_ms: 2_000,
                    admitted: true,
                },
                // Window is full.
                Stop {
                    elapsed_ms: 2_100,
                    admitted: false,
                },
                Stop {
                    elapsed_ms: 30_000,
                    admitted: false,
                },
                // One millisecond before the first stamp expires the window is
                // still full.
                Stop {
                    elapsed_ms: window_ms - 1,
                    admitted: false,
                },
                // At exactly `first + window` the first timestamp expires
                // (trim is `>=`), freeing exactly one slot.
                Stop {
                    elapsed_ms: window_ms,
                    admitted: true,
                },
                Stop {
                    elapsed_ms: window_ms,
                    admitted: false,
                },
                // The second stamp expires one tick later.
                Stop {
                    elapsed_ms: window_ms + 1_000,
                    admitted: true,
                },
                Stop {
                    elapsed_ms: window_ms + 1_000,
                    admitted: false,
                },
                // The third stamp expires too.
                Stop {
                    elapsed_ms: window_ms + 2_000,
                    admitted: true,
                },
                Stop {
                    elapsed_ms: window_ms + 2_000,
                    admitted: false,
                },
            ],
        );
    }

    #[test]
    fn zero_limit_always_rejects() {
        let limiter = InMemoryRateLimiter::new(Duration::from_secs(60));
        let result = limiter.check_rate_limit_at("app", 0, Instant::now());
        assert!(matches!(result.unwrap_err(), AuthError::RateLimitExceeded));
    }

    #[test]
    fn limits_are_independent_per_app_on_the_same_timeline() {
        let limiter = InMemoryRateLimiter::new(Duration::from_secs(60));
        let now = Instant::now();
        for _ in 0..5 {
            limiter.check_rate_limit_at("app1", 5, now).unwrap();
        }
        // app1 is at limit; app2 shares only the clock, not the budget.
        assert!(limiter.check_rate_limit_at("app1", 5, now).is_err());
        assert!(limiter.check_rate_limit_at("app2", 5, now).is_ok());
    }

    #[test]
    fn cleanup_removes_only_fully_expired_windows() {
        let limiter = InMemoryRateLimiter::new(Duration::from_secs(60));
        let window = Duration::from_secs(60);
        // Build the whole timeline by ADDING offsets to an origin: subtracting
        // from `Instant::now()` panics on a host whose monotonic clock is
        // younger than the offsets (fresh VM/container boots).
        let origin = Instant::now();
        let now = origin + 3 * window;

        // Fully expired: single timestamp from two windows ago.
        limiter.check_rate_limit_at("stale", 100, origin).unwrap();
        // Partially expired: one timestamp expired, one stamped exactly now.
        limiter
            .check_rate_limit_at("partial", 100, origin + 2 * window)
            .unwrap();
        limiter.check_rate_limit_at("partial", 100, now).unwrap();
        // Fully live.
        limiter.check_rate_limit_at("live", 100, now).unwrap();

        limiter.cleanup_at(now);

        // DashMap iteration order is hash-based, so compare as a set.
        let mut keys: Vec<_> = limiter
            .windows
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        keys.sort();
        assert_eq!(keys, ["live".to_owned(), "partial".to_owned()]);
        assert!(limiter.windows.get("stale").is_none());
        assert_eq!(limiter.windows.get("partial").unwrap().len(), 1);
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

    #[tokio::test(start_paused = true)]
    async fn zero_cleanup_interval_does_not_kill_the_background_task() {
        let limiter = Arc::new(InMemoryRateLimiter::new(Duration::ZERO));
        let handle = Arc::clone(&limiter)
            .start_cleanup_task()
            .unwrap_or_else(|error| panic!("test runtime must be available: {error}"));

        tokio::task::yield_now().await;
        assert!(
            !handle.is_finished(),
            "a public zero cleanup interval must be clamped instead of panicking its task"
        );

        handle.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn cleanup_task_does_not_keep_limiter_alive() {
        let limiter = Arc::new(InMemoryRateLimiter::new(Duration::from_secs(1)));
        let weak = Arc::downgrade(&limiter);
        let handle = Arc::clone(&limiter)
            .start_cleanup_task()
            .unwrap_or_else(|error| panic!("test runtime must be available: {error}"));

        drop(limiter);
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;

        assert!(weak.upgrade().is_none());
        assert!(handle.is_finished());
    }

    #[test]
    fn cleanup_task_without_runtime_returns_error_instead_of_panicking() {
        let limiter = Arc::new(InMemoryRateLimiter::new(Duration::from_secs(1)));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            limiter.start_cleanup_task()
        }));
        assert!(result.is_ok());
        assert!(result.ok().is_some_and(|task| task.is_err()));
    }
}
