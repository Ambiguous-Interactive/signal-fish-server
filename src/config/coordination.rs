//! Coordination and cross-instance configuration types.

use super::defaults::{
    default_dedup_cache_capacity, default_dedup_cache_cleanup_interval_secs,
    default_dedup_cache_ttl_secs, default_membership_snapshot_interval_secs,
};
use serde::{Deserialize, Serialize};

/// Coordination configuration for cross-instance messaging.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct CoordinationConfig {
    #[serde(default)]
    pub dedup_cache: DedupCacheConfig,
    /// Interval between cross-instance membership snapshots (seconds).
    #[serde(default = "default_membership_snapshot_interval_secs")]
    pub membership_snapshot_interval_secs: u64,
}

/// Deduplication cache configuration.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DedupCacheConfig {
    #[serde(default = "default_dedup_cache_capacity")]
    pub capacity: usize,
    #[serde(default = "default_dedup_cache_ttl_secs")]
    pub ttl_secs: u64,
    #[serde(default = "default_dedup_cache_cleanup_interval_secs")]
    pub cleanup_interval_secs: u64,
}

impl Default for DedupCacheConfig {
    fn default() -> Self {
        Self {
            capacity: default_dedup_cache_capacity(),
            ttl_secs: default_dedup_cache_ttl_secs(),
            cleanup_interval_secs: default_dedup_cache_cleanup_interval_secs(),
        }
    }
}
