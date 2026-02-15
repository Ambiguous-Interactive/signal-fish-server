//! Message deduplication cache for cross-instance coordination
//!
//! This module provides an LRU-based cache for deduplicating cross-instance messages,
//! ensuring that messages are only processed once even when delivered via multiple paths.

#![allow(dead_code)]

use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time::{interval, MissedTickBehavior};

use crate::metrics::ServerMetrics;
use crate::protocol::RoomId;

/// Cache key for message deduplication
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(crate) struct DedupCacheKey {
    pub room_id: Option<RoomId>,
    pub sequence_id: u64,
}

/// Configuration settings for the deduplication cache
#[derive(Debug, Clone, Copy)]
pub struct DedupCacheSettings {
    /// Maximum number of entries in the cache
    pub capacity: usize,
    /// Time-to-live for cache entries
    pub ttl: Duration,
    /// Interval for running cache cleanup
    pub cleanup_interval: Duration,
}

impl Default for DedupCacheSettings {
    fn default() -> Self {
        Self {
            capacity: 100_000,
            ttl: Duration::from_secs(60),
            cleanup_interval: Duration::from_secs(30),
        }
    }
}

/// Shared deduplication cache
#[derive(Clone)]
pub(crate) struct DedupCache {
    inner: Arc<Mutex<DedupCacheInner>>,
}

/// Inner cache implementation
pub(crate) struct DedupCacheInner {
    cache: LruCache<DedupCacheKey, Instant>,
    ttl: Duration,
}

/// Result of checking the cache
pub(crate) struct DedupCacheCheckOutcome {
    pub hit: bool,
    pub evicted: usize,
}

/// Result of inserting into the cache
pub(crate) struct DedupCacheInsertOutcome {
    pub evicted: usize,
}

impl DedupCache {
    /// Create a new deduplication cache
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        let effective_capacity = if capacity == 0 { 1 } else { capacity };
        let cache =
            LruCache::new(NonZeroUsize::new(effective_capacity).unwrap_or(NonZeroUsize::MIN));

        Self {
            inner: Arc::new(Mutex::new(DedupCacheInner { cache, ttl })),
        }
    }

    /// Check if a key is in the cache
    pub async fn check(&self, key: &DedupCacheKey) -> DedupCacheCheckOutcome {
        let mut inner = self.inner.lock().await;
        inner.check(key)
    }

    /// Insert a key into the cache
    pub async fn insert(&self, key: DedupCacheKey) -> DedupCacheInsertOutcome {
        let mut inner = self.inner.lock().await;
        inner.insert(key)
    }

    /// Spawn a background task to maintain the cache
    pub fn spawn_maintenance(
        &self,
        sweep_interval: Duration,
        metrics: Arc<ServerMetrics>,
        capacity: usize,
    ) {
        let cache = self.clone();
        let interval_duration = if sweep_interval.is_zero() {
            Duration::from_secs(1)
        } else {
            sweep_interval
        };

        tokio::spawn(async move {
            let mut ticker = interval(interval_duration);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

            loop {
                ticker.tick().await;

                let (expired, size) = cache.cleanup_expired().await;
                if expired > 0 {
                    metrics.add_dedup_cache_evictions(expired as u64);
                }
                metrics.set_dedup_cache_size(size as u64);

                if capacity > 0 {
                    let ninety_percent = (capacity as f64 * 0.9).ceil() as usize;
                    if size >= ninety_percent {
                        tracing::warn!(
                            cache_size = size,
                            capacity,
                            "dedup cache utilization above 90%; consider increasing capacity or reducing sweep interval"
                        );
                    }
                }
            }
        });
    }

    /// Clean up expired entries and return (expired_count, current_size)
    async fn cleanup_expired(&self) -> (usize, usize) {
        let mut inner = self.inner.lock().await;
        let expired = inner.evict_expired(Instant::now());
        let size = inner.cache.len();
        (expired, size)
    }
}

impl DedupCacheInner {
    /// Check if a key is in the cache and evict expired entries
    fn check(&mut self, key: &DedupCacheKey) -> DedupCacheCheckOutcome {
        let now = Instant::now();
        let mut evicted = self.evict_expired(now);

        let hit = if let Some(&stored_at) = self.cache.get(key) {
            if now.duration_since(stored_at) <= self.ttl {
                true
            } else {
                self.cache.pop(key);
                evicted += 1;
                false
            }
        } else {
            false
        };

        DedupCacheCheckOutcome { hit, evicted }
    }

    /// Insert a key into the cache
    fn insert(&mut self, key: DedupCacheKey) -> DedupCacheInsertOutcome {
        let now = Instant::now();
        let mut evicted = self.evict_expired(now);

        if self.cache.len() == self.cache.cap().get() && self.cache.pop_lru().is_some() {
            evicted += 1;
        }

        self.cache.put(key, now);

        DedupCacheInsertOutcome { evicted }
    }

    /// Evict all expired entries
    fn evict_expired(&mut self, now: Instant) -> usize {
        let mut evicted = 0;
        while let Some((_, stored_at)) = self.cache.peek_lru() {
            if now.duration_since(*stored_at) > self.ttl {
                self.cache.pop_lru();
                evicted += 1;
            } else {
                break;
            }
        }
        evicted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, Duration as TokioDuration};
    use uuid::Uuid;

    #[tokio::test]
    async fn test_dedup_cache_hit_and_expiration() {
        let cache = DedupCache::new(8, Duration::from_millis(50));
        let key = DedupCacheKey {
            room_id: Some(Uuid::new_v4()),
            sequence_id: 1,
        };

        let initial = cache.check(&key).await;
        assert!(!initial.hit);

        cache.insert(key.clone()).await;
        let second = cache.check(&key).await;
        assert!(second.hit);

        sleep(TokioDuration::from_millis(60)).await;
        let after_expiration = cache.check(&key).await;
        assert!(!after_expiration.hit);
    }

    #[tokio::test]
    async fn test_dedup_cache_capacity_eviction() {
        let cache = DedupCache::new(1, Duration::from_secs(5));

        let first_key = DedupCacheKey {
            room_id: Some(Uuid::new_v4()),
            sequence_id: 1,
        };
        let second_key = DedupCacheKey {
            room_id: Some(Uuid::new_v4()),
            sequence_id: 2,
        };

        let insert_first = cache.insert(first_key.clone()).await;
        assert_eq!(insert_first.evicted, 0);

        let insert_second = cache.insert(second_key.clone()).await;
        assert_eq!(insert_second.evicted, 1);

        let first_lookup = cache.check(&first_key).await;
        assert!(!first_lookup.hit);

        let second_lookup = cache.check(&second_key).await;
        assert!(second_lookup.hit);
    }

    #[tokio::test]
    async fn test_dedup_cache_concurrent_inserts() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache = DedupCache::new(8, Duration::from_secs(1));
        let key = DedupCacheKey {
            room_id: Some(Uuid::new_v4()),
            sequence_id: 42,
        };

        let hits = Arc::new(AtomicUsize::new(0));
        let misses = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..16 {
            let cache_clone = cache.clone();
            let key_clone = key.clone();
            let hits = hits.clone();
            let misses = misses.clone();
            handles.push(tokio::spawn(async move {
                let check = cache_clone.check(&key_clone).await;
                if check.hit {
                    hits.fetch_add(1, Ordering::Relaxed);
                } else {
                    misses.fetch_add(1, Ordering::Relaxed);
                    cache_clone.insert(key_clone).await;
                }
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        assert_eq!(
            misses.load(Ordering::Relaxed),
            1,
            "only the first concurrent access should miss"
        );
        assert_eq!(
            hits.load(Ordering::Relaxed),
            15,
            "all subsequent accesses should hit the cache"
        );
    }
}
