use hdrhistogram::Histogram;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Comprehensive metrics collection for in-memory signaling server
#[derive(Debug)]
pub struct ServerMetrics {
    // Connection metrics
    pub total_connections: AtomicU64,
    pub active_connections: AtomicU64,
    pub disconnections: AtomicU64,
    pub connection_errors: AtomicU64,
    pub websocket_messages_dropped: AtomicU64,

    // Room operation metrics
    pub rooms_created: AtomicU64,
    pub rooms_joined: AtomicU64,
    pub room_creation_failures: AtomicU64,
    pub room_join_failures: AtomicU64,
    pub rooms_deleted: AtomicU64,
    pub room_cap_lock_acquisitions: AtomicU64,
    pub room_cap_lock_failures: AtomicU64,
    pub room_cap_denials: AtomicU64,

    // Race condition and retry metrics
    pub room_capacity_conflicts: AtomicU64,
    pub room_code_collisions: AtomicU64,
    pub authority_transfer_conflicts: AtomicU64,
    pub retry_attempts: AtomicU64,
    pub retry_successes: AtomicU64,

    // Cross-instance communication metrics
    pub cross_instance_messages: AtomicU64,
    pub dedup_cache_hits: AtomicU64,
    pub dedup_cache_misses: AtomicU64,
    pub dedup_cache_evictions: AtomicU64,
    pub dedup_cache_size: AtomicU64,
    pub membership_cache_hits: AtomicU64,
    pub membership_cache_misses: AtomicU64,
    pub remote_membership_updates_published: AtomicU64,
    pub remote_membership_updates_received: AtomicU64,
    pub remote_membership_known_broadcasts: AtomicU64,
    pub remote_membership_forced_broadcasts: AtomicU64,
    pub remote_membership_skipped_broadcasts: AtomicU64,

    // Performance metrics
    pub query_count: AtomicU64,
    pub average_response_times: Arc<RwLock<ResponseTimeTracker>>,
    pub dashboard_cache_last_refresh_epoch: AtomicU64,
    pub dashboard_cache_refresh_failures: AtomicU64,
    pub latency_histogram_clamped_samples: AtomicU64,

    // Rate limiting metrics
    pub rate_limit_rejections: AtomicU64,
    pub rate_limit_resets: AtomicU64,
    pub rate_limit_minute_limit: AtomicU64,
    pub rate_limit_hour_limit: AtomicU64,
    pub rate_limit_day_limit: AtomicU64,
    pub rate_limit_minute_count: AtomicU64,
    pub rate_limit_hour_count: AtomicU64,
    pub rate_limit_day_count: AtomicU64,
    pub rate_limit_minute_checks: AtomicU64,
    pub rate_limit_hour_checks: AtomicU64,
    pub rate_limit_day_checks: AtomicU64,
    pub rate_limit_minute_rejections: AtomicU64,
    pub rate_limit_hour_rejections: AtomicU64,
    pub rate_limit_day_rejections: AtomicU64,
    pub rate_limit_cache_purged: AtomicU64,
    pub rate_limit_cache_rows: AtomicU64,

    // Player activity metrics
    pub players_joined: AtomicU64,
    pub players_left: AtomicU64,
    pub authority_transfers: AtomicU64,
    pub game_data_messages: AtomicU64,

    // Heartbeat throttling metrics
    /// Updates performed for player last_seen timestamps
    pub heartbeat_updates: AtomicU64,
    /// Updates skipped due to threshold-based throttling
    pub heartbeat_skipped: AtomicU64,

    // Reconnection metrics
    pub reconnection_tokens_issued: AtomicU64,
    pub reconnection_sessions_active: AtomicU64,
    pub reconnection_validations_failed: AtomicU64,
    pub reconnection_completions: AtomicU64,
    pub reconnection_events_buffered: AtomicU64,

    // Distributed lock metrics
    pub distributed_lock_release_failures: AtomicU64,
    pub distributed_lock_extend_failures: AtomicU64,
    pub distributed_lock_cleanup_runs: AtomicU64,
    pub distributed_lock_cleanup_removed: AtomicU64,

    // Error tracking
    pub validation_errors: AtomicU64,
    pub internal_errors: AtomicU64,
    pub websocket_errors: AtomicU64,

    // Cleanup metrics
    pub empty_rooms_cleaned: AtomicU64,
    pub inactive_rooms_cleaned: AtomicU64,
    pub expired_players_cleaned: AtomicU64,

    // Relay health metrics
    pub relay_client_id_reuse_events: AtomicU64,
    pub relay_client_id_exhaustion_events: AtomicU64,
    pub relay_session_timeouts: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitWindow {
    Minute,
    Hour,
    Day,
}

#[derive(Debug, Clone)]
pub struct ResponseTimeTracker {
    operations: HashMap<String, OperationLatencyHistogram>,
    lowest_discernible_micros: u64,
    highest_trackable_micros: u64,
    significant_figures: u8,
}

const DEFAULT_LOWEST_DISCERNIBLE_MICROS: u64 = 1;
const DEFAULT_HIGHEST_TRACKABLE_MICROS: u64 = 300_000_000; // 5 minutes in microseconds
const DEFAULT_SIGNIFICANT_FIGURES: u8 = 3;

#[derive(Debug, Clone)]
struct OperationLatencyHistogram {
    /// Optional histogram - None if all creation attempts failed (should be rare)
    histogram: Option<Histogram<u64>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MetricsSnapshot {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub connections: ConnectionMetrics,
    pub rooms: RoomMetrics,
    pub race_conditions: RaceConditionMetrics,
    pub cross_instance: CrossInstanceMetrics,
    pub performance: PerformanceMetrics,
    pub dashboard_cache: DashboardCacheMetrics,
    pub rate_limiting: RateLimitingMetrics,
    pub players: PlayerMetrics,
    pub errors: ErrorMetrics,
    pub cleanup: CleanupMetrics,
    pub reconnection: ReconnectionMetrics,
    pub distributed_lock: DistributedLockMetrics,
    pub relay_health: RelayHealthMetrics,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConnectionMetrics {
    pub total_connections: u64,
    pub active_connections: u64,
    pub disconnections: u64,
    pub connection_errors: u64,
    pub websocket_messages_dropped: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RoomMetrics {
    pub rooms_created: u64,
    pub rooms_joined: u64,
    pub room_creation_failures: u64,
    pub room_join_failures: u64,
    pub rooms_deleted: u64,
    pub room_cap_lock_acquisitions: u64,
    pub room_cap_lock_failures: u64,
    pub room_cap_denials: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RaceConditionMetrics {
    pub room_capacity_conflicts: u64,
    pub room_code_collisions: u64,
    pub authority_transfer_conflicts: u64,
    pub retry_attempts: u64,
    pub retry_successes: u64,
    pub retry_success_rate: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CrossInstanceMetrics {
    pub cross_instance_messages: u64,
    pub dedup_cache_hits: u64,
    pub dedup_cache_misses: u64,
    pub dedup_cache_evictions: u64,
    pub dedup_cache_size: u64,
    pub membership_cache_hits: u64,
    pub membership_cache_misses: u64,
    pub remote_membership_updates_published: u64,
    pub remote_membership_updates_received: u64,
    pub remote_membership_known_broadcasts: u64,
    pub remote_membership_forced_broadcasts: u64,
    pub remote_membership_skipped_broadcasts: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PerformanceMetrics {
    pub query_count: u64,
    pub average_room_creation_ms: Option<f64>,
    pub average_room_join_ms: Option<f64>,
    pub average_query_ms: Option<f64>,
    pub room_creation_latency: OperationLatencyMetrics,
    pub room_join_latency: OperationLatencyMetrics,
    pub query_latency: OperationLatencyMetrics,
    pub latency_histogram_clamped_samples: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, Default)]
pub struct OperationLatencyMetrics {
    pub average_ms: Option<f64>,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub p99_ms: Option<f64>,
    pub min_ms: Option<f64>,
    pub max_ms: Option<f64>,
    pub sample_count: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RateLimitingMetrics {
    pub rate_limit_rejections: u64,
    pub rate_limit_resets: u64,
    pub minute_limit: u64,
    pub hour_limit: u64,
    pub day_limit: u64,
    pub minute_count: u64,
    pub hour_count: u64,
    pub day_count: u64,
    pub minute_checks: u64,
    pub hour_checks: u64,
    pub day_checks: u64,
    pub minute_rejections: u64,
    pub hour_rejections: u64,
    pub day_rejections: u64,
    pub cache_rows: u64,
    pub cache_purged: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PlayerMetrics {
    pub players_joined: u64,
    pub players_left: u64,
    pub authority_transfers: u64,
    pub game_data_messages: u64,
    /// Updates performed for player last_seen timestamps
    pub heartbeat_updates: u64,
    /// Updates skipped due to threshold-based throttling
    pub heartbeat_skipped: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ReconnectionMetrics {
    pub tokens_issued: u64,
    pub sessions_active: u64,
    pub validations_failed: u64,
    pub completions: u64,
    pub events_buffered: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DistributedLockMetrics {
    pub release_failures: u64,
    pub extend_failures: u64,
    pub cleanup_runs: u64,
    pub cleanup_removed: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RelayHealthMetrics {
    pub client_id_reuse_events: u64,
    pub client_id_exhaustion_events: u64,
    pub session_timeouts: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ErrorMetrics {
    pub validation_errors: u64,
    pub internal_errors: u64,
    pub websocket_errors: u64,
    pub total_errors: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CleanupMetrics {
    pub empty_rooms_cleaned: u64,
    pub inactive_rooms_cleaned: u64,
    pub expired_players_cleaned: u64,
}

impl Default for ServerMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerMetrics {
    pub fn new() -> Self {
        Self {
            total_connections: AtomicU64::new(0),
            active_connections: AtomicU64::new(0),
            disconnections: AtomicU64::new(0),
            connection_errors: AtomicU64::new(0),
            websocket_messages_dropped: AtomicU64::new(0),
            rooms_created: AtomicU64::new(0),
            rooms_joined: AtomicU64::new(0),
            room_creation_failures: AtomicU64::new(0),
            room_join_failures: AtomicU64::new(0),
            rooms_deleted: AtomicU64::new(0),
            room_cap_lock_acquisitions: AtomicU64::new(0),
            room_cap_lock_failures: AtomicU64::new(0),
            room_cap_denials: AtomicU64::new(0),
            room_capacity_conflicts: AtomicU64::new(0),
            room_code_collisions: AtomicU64::new(0),
            authority_transfer_conflicts: AtomicU64::new(0),
            retry_attempts: AtomicU64::new(0),
            retry_successes: AtomicU64::new(0),
            cross_instance_messages: AtomicU64::new(0),
            dedup_cache_hits: AtomicU64::new(0),
            dedup_cache_misses: AtomicU64::new(0),
            dedup_cache_evictions: AtomicU64::new(0),
            dedup_cache_size: AtomicU64::new(0),
            membership_cache_hits: AtomicU64::new(0),
            membership_cache_misses: AtomicU64::new(0),
            remote_membership_updates_published: AtomicU64::new(0),
            remote_membership_updates_received: AtomicU64::new(0),
            remote_membership_known_broadcasts: AtomicU64::new(0),
            remote_membership_forced_broadcasts: AtomicU64::new(0),
            remote_membership_skipped_broadcasts: AtomicU64::new(0),
            query_count: AtomicU64::new(0),
            average_response_times: Arc::new(RwLock::new(ResponseTimeTracker::new())),
            dashboard_cache_last_refresh_epoch: AtomicU64::new(0),
            dashboard_cache_refresh_failures: AtomicU64::new(0),
            latency_histogram_clamped_samples: AtomicU64::new(0),
            rate_limit_rejections: AtomicU64::new(0),
            rate_limit_resets: AtomicU64::new(0),
            rate_limit_minute_limit: AtomicU64::new(0),
            rate_limit_hour_limit: AtomicU64::new(0),
            rate_limit_day_limit: AtomicU64::new(0),
            rate_limit_minute_count: AtomicU64::new(0),
            rate_limit_hour_count: AtomicU64::new(0),
            rate_limit_day_count: AtomicU64::new(0),
            rate_limit_minute_checks: AtomicU64::new(0),
            rate_limit_hour_checks: AtomicU64::new(0),
            rate_limit_day_checks: AtomicU64::new(0),
            rate_limit_minute_rejections: AtomicU64::new(0),
            rate_limit_hour_rejections: AtomicU64::new(0),
            rate_limit_day_rejections: AtomicU64::new(0),
            rate_limit_cache_purged: AtomicU64::new(0),
            rate_limit_cache_rows: AtomicU64::new(0),
            players_joined: AtomicU64::new(0),
            players_left: AtomicU64::new(0),
            authority_transfers: AtomicU64::new(0),
            game_data_messages: AtomicU64::new(0),
            heartbeat_updates: AtomicU64::new(0),
            heartbeat_skipped: AtomicU64::new(0),
            reconnection_tokens_issued: AtomicU64::new(0),
            reconnection_sessions_active: AtomicU64::new(0),
            reconnection_validations_failed: AtomicU64::new(0),
            reconnection_completions: AtomicU64::new(0),
            reconnection_events_buffered: AtomicU64::new(0),
            distributed_lock_release_failures: AtomicU64::new(0),
            distributed_lock_extend_failures: AtomicU64::new(0),
            distributed_lock_cleanup_runs: AtomicU64::new(0),
            distributed_lock_cleanup_removed: AtomicU64::new(0),
            validation_errors: AtomicU64::new(0),
            internal_errors: AtomicU64::new(0),
            websocket_errors: AtomicU64::new(0),
            empty_rooms_cleaned: AtomicU64::new(0),
            inactive_rooms_cleaned: AtomicU64::new(0),
            expired_players_cleaned: AtomicU64::new(0),
            relay_client_id_reuse_events: AtomicU64::new(0),
            relay_client_id_exhaustion_events: AtomicU64::new(0),
            relay_session_timeouts: AtomicU64::new(0),
        }
    }

    // Connection metrics
    pub fn increment_connections(&self) {
        self.total_connections.fetch_add(1, Ordering::Relaxed);
        self.active_connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn decrement_active_connections(&self) {
        // Use fetch_update for atomic check-then-decrement to prevent underflow
        let _ =
            self.active_connections
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    if current > 0 {
                        Some(current - 1)
                    } else {
                        None
                    }
                });
        self.disconnections.fetch_add(1, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub fn increment_connection_errors(&self) {
        self.connection_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_websocket_messages_dropped(&self) {
        self.websocket_messages_dropped
            .fetch_add(1, Ordering::Relaxed);
    }

    // Room operation metrics
    pub fn increment_rooms_created(&self) {
        self.rooms_created.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_rooms_joined(&self) {
        self.rooms_joined.fetch_add(1, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub fn increment_room_creation_failures(&self) {
        self.room_creation_failures.fetch_add(1, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub fn increment_room_join_failures(&self) {
        self.room_join_failures.fetch_add(1, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub fn increment_rooms_deleted(&self) {
        self.rooms_deleted.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_room_cap_lock_acquisitions(&self) {
        self.room_cap_lock_acquisitions
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_room_cap_lock_failures(&self) {
        self.room_cap_lock_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_room_cap_denials(&self) {
        self.room_cap_denials.fetch_add(1, Ordering::Relaxed);
    }

    // Race condition metrics
    #[allow(dead_code)]
    pub fn increment_room_capacity_conflicts(&self) {
        self.room_capacity_conflicts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_room_code_collisions(&self) {
        self.room_code_collisions.fetch_add(1, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub fn increment_authority_transfer_conflicts(&self) {
        self.authority_transfer_conflicts
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_retry_attempts(&self) {
        self.retry_attempts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_retry_successes(&self) {
        self.retry_successes.fetch_add(1, Ordering::Relaxed);
    }

    // Cross-instance communication metrics
    #[allow(dead_code)]
    pub fn increment_cross_instance_messages(&self) {
        self.cross_instance_messages.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_dedup_cache_hit(&self) {
        self.dedup_cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_dedup_cache_miss(&self) {
        self.dedup_cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_membership_cache_hit(&self) {
        self.membership_cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_membership_cache_miss(&self) {
        self.membership_cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_remote_membership_update_published(&self) {
        self.remote_membership_updates_published
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_remote_membership_update_received(&self) {
        self.remote_membership_updates_received
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_remote_membership_known_broadcast(&self) {
        self.remote_membership_known_broadcasts
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_remote_membership_forced_broadcast(&self) {
        self.remote_membership_forced_broadcasts
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_remote_membership_skipped_broadcast(&self) {
        self.remote_membership_skipped_broadcasts
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_dedup_cache_evictions(&self, count: u64) {
        if count > 0 {
            self.dedup_cache_evictions
                .fetch_add(count, Ordering::Relaxed);
        }
    }

    pub fn set_dedup_cache_size(&self, size: u64) {
        self.dedup_cache_size.store(size, Ordering::Relaxed);
    }

    // Performance metrics
    pub fn increment_query_count(&self) {
        self.query_count.fetch_add(1, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub async fn record_response_time(&self, operation: &str, duration: Duration) {
        let mut tracker = self.average_response_times.write().await;
        let clamped = tracker.add_sample(operation, duration);
        drop(tracker);
        if clamped {
            self.increment_latency_histogram_clamps();
        }
    }

    pub fn set_dashboard_cache_last_refresh(&self, timestamp: chrono::DateTime<chrono::Utc>) {
        let epoch = timestamp.timestamp().max(0) as u64;
        self.dashboard_cache_last_refresh_epoch
            .store(epoch, Ordering::Relaxed);
    }

    pub fn increment_dashboard_cache_refresh_failures(&self) {
        self.dashboard_cache_refresh_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_latency_histogram_clamps(&self) {
        self.latency_histogram_clamped_samples
            .fetch_add(1, Ordering::Relaxed);
    }

    // Rate limiting metrics
    #[allow(dead_code)]
    pub fn increment_rate_limit_rejections(&self) {
        self.rate_limit_rejections.fetch_add(1, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub fn increment_rate_limit_resets(&self) {
        self.rate_limit_resets.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_rate_limit_limit(&self, window: RateLimitWindow, limit: u32) {
        let limit = u64::from(limit);
        match window {
            RateLimitWindow::Minute => {
                self.rate_limit_minute_limit.store(limit, Ordering::Relaxed);
            }
            RateLimitWindow::Hour => {
                self.rate_limit_hour_limit.store(limit, Ordering::Relaxed);
            }
            RateLimitWindow::Day => {
                self.rate_limit_day_limit.store(limit, Ordering::Relaxed);
            }
        }
    }

    pub fn record_rate_limit_usage(&self, window: RateLimitWindow, count: u32) {
        let count = u64::from(count);
        match window {
            RateLimitWindow::Minute => {
                self.rate_limit_minute_count.store(count, Ordering::Relaxed);
            }
            RateLimitWindow::Hour => {
                self.rate_limit_hour_count.store(count, Ordering::Relaxed);
            }
            RateLimitWindow::Day => {
                self.rate_limit_day_count.store(count, Ordering::Relaxed);
            }
        }
    }

    pub fn record_rate_limit_check(&self, window: RateLimitWindow) {
        match window {
            RateLimitWindow::Minute => {
                self.rate_limit_minute_checks
                    .fetch_add(1, Ordering::Relaxed);
            }
            RateLimitWindow::Hour => {
                self.rate_limit_hour_checks.fetch_add(1, Ordering::Relaxed);
            }
            RateLimitWindow::Day => {
                self.rate_limit_day_checks.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn record_rate_limit_rejection(&self, window: RateLimitWindow) {
        self.rate_limit_rejections.fetch_add(1, Ordering::Relaxed);
        match window {
            RateLimitWindow::Minute => {
                self.rate_limit_minute_rejections
                    .fetch_add(1, Ordering::Relaxed);
            }
            RateLimitWindow::Hour => {
                self.rate_limit_hour_rejections
                    .fetch_add(1, Ordering::Relaxed);
            }
            RateLimitWindow::Day => {
                self.rate_limit_day_rejections
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn add_rate_limit_cache_purged(&self, count: u64) {
        if count > 0 {
            self.rate_limit_cache_purged
                .fetch_add(count, Ordering::Relaxed);
        }
    }

    pub fn set_rate_limit_cache_rows(&self, rows: u64) {
        self.rate_limit_cache_rows.store(rows, Ordering::Relaxed);
    }

    // Player activity metrics
    pub fn increment_players_joined(&self) {
        self.players_joined.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_players_left(&self) {
        self.players_left.fetch_add(1, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub fn increment_authority_transfers(&self) {
        self.authority_transfers.fetch_add(1, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub fn increment_game_data_messages(&self) {
        self.game_data_messages.fetch_add(1, Ordering::Relaxed);
    }

    // Heartbeat throttling metrics
    pub fn increment_heartbeat_updates(&self) {
        self.heartbeat_updates.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_heartbeat_skipped(&self) {
        self.heartbeat_skipped.fetch_add(1, Ordering::Relaxed);
    }

    // Reconnection metrics
    pub fn increment_reconnection_tokens_issued(&self) {
        self.reconnection_tokens_issued
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_reconnection_sessions_active(&self) {
        self.reconnection_sessions_active
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_reconnection_sessions_active(&self, value: u64) {
        self.reconnection_sessions_active
            .store(value, Ordering::Relaxed);
    }

    pub fn decrement_reconnection_sessions_active(&self) {
        // Use fetch_update for atomic check-then-decrement to prevent underflow
        // when two threads race to decrement the same counter
        let _ = self.reconnection_sessions_active.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |current| {
                if current > 0 {
                    Some(current - 1)
                } else {
                    None
                }
            },
        );
    }

    pub fn increment_reconnection_validation_failure(&self) {
        self.reconnection_validations_failed
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_reconnection_completions(&self) {
        self.reconnection_completions
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_reconnection_events_buffered(&self, count: u64) {
        if count > 0 {
            self.reconnection_events_buffered
                .fetch_add(count, Ordering::Relaxed);
        }
    }

    // Distributed lock metrics
    pub fn increment_distributed_lock_release_failures(&self) {
        self.distributed_lock_release_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_distributed_lock_extend_failures(&self) {
        self.distributed_lock_extend_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_distributed_lock_cleanup(&self, removed: usize) {
        self.distributed_lock_cleanup_runs
            .fetch_add(1, Ordering::Relaxed);
        if removed > 0 {
            self.distributed_lock_cleanup_removed
                .fetch_add(removed as u64, Ordering::Relaxed);
        }
    }

    // Error tracking
    #[allow(dead_code)]
    pub fn increment_validation_errors(&self) {
        self.validation_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_internal_errors(&self) {
        self.internal_errors.fetch_add(1, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub fn increment_websocket_errors(&self) {
        self.websocket_errors.fetch_add(1, Ordering::Relaxed);
    }

    // Cleanup metrics
    #[allow(dead_code)]
    pub fn add_empty_rooms_cleaned(&self, count: u64) {
        self.empty_rooms_cleaned.fetch_add(count, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub fn add_inactive_rooms_cleaned(&self, count: u64) {
        self.inactive_rooms_cleaned
            .fetch_add(count, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub fn add_expired_players_cleaned(&self, count: u64) {
        self.expired_players_cleaned
            .fetch_add(count, Ordering::Relaxed);
    }

    // Relay health metrics
    pub fn increment_relay_client_id_reuse(&self) {
        self.relay_client_id_reuse_events
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_relay_client_id_exhaustion(&self) {
        self.relay_client_id_exhaustion_events
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_relay_session_timeouts(&self, count: u64) {
        if count == 0 {
            return;
        }
        self.relay_session_timeouts
            .fetch_add(count, Ordering::Relaxed);
    }

    // Snapshot generation
    pub async fn snapshot(&self) -> MetricsSnapshot {
        let tracker = self.average_response_times.read().await;
        let room_creation_latency = tracker
            .get_latency_metrics("room_creation")
            .unwrap_or_default();
        let room_join_latency = tracker.get_latency_metrics("room_join").unwrap_or_default();
        let query_latency = tracker.get_latency_metrics("query").unwrap_or_default();

        let retry_attempts = self.retry_attempts.load(Ordering::Relaxed);
        let retry_successes = self.retry_successes.load(Ordering::Relaxed);
        let retry_success_rate = if retry_attempts > 0 {
            (retry_successes as f64) / (retry_attempts as f64)
        } else {
            1.0
        };

        let validation_errors = self.validation_errors.load(Ordering::Relaxed);
        let internal_errors = self.internal_errors.load(Ordering::Relaxed);
        let websocket_errors = self.websocket_errors.load(Ordering::Relaxed);
        let total_errors = validation_errors + internal_errors + websocket_errors;

        MetricsSnapshot {
            timestamp: chrono::Utc::now(),
            connections: ConnectionMetrics {
                total_connections: self.total_connections.load(Ordering::Relaxed),
                active_connections: self.active_connections.load(Ordering::Relaxed),
                disconnections: self.disconnections.load(Ordering::Relaxed),
                connection_errors: self.connection_errors.load(Ordering::Relaxed),
                websocket_messages_dropped: self.websocket_messages_dropped.load(Ordering::Relaxed),
            },
            rooms: RoomMetrics {
                rooms_created: self.rooms_created.load(Ordering::Relaxed),
                rooms_joined: self.rooms_joined.load(Ordering::Relaxed),
                room_creation_failures: self.room_creation_failures.load(Ordering::Relaxed),
                room_join_failures: self.room_join_failures.load(Ordering::Relaxed),
                rooms_deleted: self.rooms_deleted.load(Ordering::Relaxed),
                room_cap_lock_acquisitions: self.room_cap_lock_acquisitions.load(Ordering::Relaxed),
                room_cap_lock_failures: self.room_cap_lock_failures.load(Ordering::Relaxed),
                room_cap_denials: self.room_cap_denials.load(Ordering::Relaxed),
            },
            race_conditions: RaceConditionMetrics {
                room_capacity_conflicts: self.room_capacity_conflicts.load(Ordering::Relaxed),
                room_code_collisions: self.room_code_collisions.load(Ordering::Relaxed),
                authority_transfer_conflicts: self
                    .authority_transfer_conflicts
                    .load(Ordering::Relaxed),
                retry_attempts,
                retry_successes,
                retry_success_rate,
            },
            cross_instance: CrossInstanceMetrics {
                cross_instance_messages: self.cross_instance_messages.load(Ordering::Relaxed),
                dedup_cache_hits: self.dedup_cache_hits.load(Ordering::Relaxed),
                dedup_cache_misses: self.dedup_cache_misses.load(Ordering::Relaxed),
                dedup_cache_evictions: self.dedup_cache_evictions.load(Ordering::Relaxed),
                dedup_cache_size: self.dedup_cache_size.load(Ordering::Relaxed),
                membership_cache_hits: self.membership_cache_hits.load(Ordering::Relaxed),
                membership_cache_misses: self.membership_cache_misses.load(Ordering::Relaxed),
                remote_membership_updates_published: self
                    .remote_membership_updates_published
                    .load(Ordering::Relaxed),
                remote_membership_updates_received: self
                    .remote_membership_updates_received
                    .load(Ordering::Relaxed),
                remote_membership_known_broadcasts: self
                    .remote_membership_known_broadcasts
                    .load(Ordering::Relaxed),
                remote_membership_forced_broadcasts: self
                    .remote_membership_forced_broadcasts
                    .load(Ordering::Relaxed),
                remote_membership_skipped_broadcasts: self
                    .remote_membership_skipped_broadcasts
                    .load(Ordering::Relaxed),
            },
            performance: PerformanceMetrics {
                query_count: self.query_count.load(Ordering::Relaxed),
                average_room_creation_ms: room_creation_latency.average_ms,
                average_room_join_ms: room_join_latency.average_ms,
                average_query_ms: query_latency.average_ms,
                room_creation_latency,
                room_join_latency,
                query_latency,
                latency_histogram_clamped_samples: self
                    .latency_histogram_clamped_samples
                    .load(Ordering::Relaxed),
            },
            dashboard_cache: DashboardCacheMetrics {
                refresh_count: 0,
                refresh_errors: 0,
                last_refresh_timestamp: self
                    .dashboard_cache_last_refresh_epoch
                    .load(Ordering::Relaxed),
                refresh_failures: self
                    .dashboard_cache_refresh_failures
                    .load(Ordering::Relaxed),
            },
            rate_limiting: RateLimitingMetrics {
                rate_limit_rejections: self.rate_limit_rejections.load(Ordering::Relaxed),
                rate_limit_resets: self.rate_limit_resets.load(Ordering::Relaxed),
                minute_limit: self.rate_limit_minute_limit.load(Ordering::Relaxed),
                hour_limit: self.rate_limit_hour_limit.load(Ordering::Relaxed),
                day_limit: self.rate_limit_day_limit.load(Ordering::Relaxed),
                minute_count: self.rate_limit_minute_count.load(Ordering::Relaxed),
                hour_count: self.rate_limit_hour_count.load(Ordering::Relaxed),
                day_count: self.rate_limit_day_count.load(Ordering::Relaxed),
                minute_checks: self.rate_limit_minute_checks.load(Ordering::Relaxed),
                hour_checks: self.rate_limit_hour_checks.load(Ordering::Relaxed),
                day_checks: self.rate_limit_day_checks.load(Ordering::Relaxed),
                minute_rejections: self.rate_limit_minute_rejections.load(Ordering::Relaxed),
                hour_rejections: self.rate_limit_hour_rejections.load(Ordering::Relaxed),
                day_rejections: self.rate_limit_day_rejections.load(Ordering::Relaxed),
                cache_rows: self.rate_limit_cache_rows.load(Ordering::Relaxed),
                cache_purged: self.rate_limit_cache_purged.load(Ordering::Relaxed),
            },
            players: PlayerMetrics {
                players_joined: self.players_joined.load(Ordering::Relaxed),
                players_left: self.players_left.load(Ordering::Relaxed),
                authority_transfers: self.authority_transfers.load(Ordering::Relaxed),
                game_data_messages: self.game_data_messages.load(Ordering::Relaxed),
                heartbeat_updates: self.heartbeat_updates.load(Ordering::Relaxed),
                heartbeat_skipped: self.heartbeat_skipped.load(Ordering::Relaxed),
            },
            errors: ErrorMetrics {
                validation_errors,
                internal_errors,
                websocket_errors,
                total_errors,
            },
            cleanup: CleanupMetrics {
                empty_rooms_cleaned: self.empty_rooms_cleaned.load(Ordering::Relaxed),
                inactive_rooms_cleaned: self.inactive_rooms_cleaned.load(Ordering::Relaxed),
                expired_players_cleaned: self.expired_players_cleaned.load(Ordering::Relaxed),
            },
            reconnection: ReconnectionMetrics {
                tokens_issued: self.reconnection_tokens_issued.load(Ordering::Relaxed),
                sessions_active: self.reconnection_sessions_active.load(Ordering::Relaxed),
                validations_failed: self.reconnection_validations_failed.load(Ordering::Relaxed),
                completions: self.reconnection_completions.load(Ordering::Relaxed),
                events_buffered: self.reconnection_events_buffered.load(Ordering::Relaxed),
            },
            distributed_lock: DistributedLockMetrics {
                release_failures: self
                    .distributed_lock_release_failures
                    .load(Ordering::Relaxed),
                extend_failures: self
                    .distributed_lock_extend_failures
                    .load(Ordering::Relaxed),
                cleanup_runs: self.distributed_lock_cleanup_runs.load(Ordering::Relaxed),
                cleanup_removed: self
                    .distributed_lock_cleanup_removed
                    .load(Ordering::Relaxed),
            },
            relay_health: RelayHealthMetrics {
                client_id_reuse_events: self.relay_client_id_reuse_events.load(Ordering::Relaxed),
                client_id_exhaustion_events: self
                    .relay_client_id_exhaustion_events
                    .load(Ordering::Relaxed),
                session_timeouts: self.relay_session_timeouts.load(Ordering::Relaxed),
            },
        }
    }

    /// Get a human-readable health status based on metrics
    #[allow(dead_code)]
    pub async fn health_status(&self) -> HealthStatus {
        let snapshot = self.snapshot().await;

        let mut issues = Vec::new();
        let mut warnings = Vec::new();

        // Check error rates
        let total_operations = snapshot.rooms.rooms_created + snapshot.rooms.rooms_joined;
        let total_failures =
            snapshot.rooms.room_creation_failures + snapshot.rooms.room_join_failures;

        if total_operations > 0 {
            let failure_rate = (total_failures as f64) / (total_operations as f64);
            if failure_rate > 0.1 {
                issues.push(format!("High failure rate: {:.1}%", failure_rate * 100.0));
            } else if failure_rate > 0.05 {
                warnings.push(format!(
                    "Elevated failure rate: {:.1}%",
                    failure_rate * 100.0
                ));
            }
        }

        // Check race condition frequency
        if snapshot.race_conditions.room_capacity_conflicts > 0 {
            warnings.push(format!(
                "Room capacity conflicts: {}",
                snapshot.race_conditions.room_capacity_conflicts
            ));
        }

        if snapshot.race_conditions.room_code_collisions > 0 {
            warnings.push(format!(
                "Room code collisions: {}",
                snapshot.race_conditions.room_code_collisions
            ));
        }

        // Check retry success rate
        if snapshot.race_conditions.retry_success_rate < 0.9
            && snapshot.race_conditions.retry_attempts > 0
        {
            warnings.push(format!(
                "Retry issues: {:.1}% success rate",
                snapshot.race_conditions.retry_success_rate * 100.0
            ));
        }

        let status = if !issues.is_empty() {
            HealthStatusLevel::Unhealthy
        } else if !warnings.is_empty() {
            HealthStatusLevel::Degraded
        } else {
            HealthStatusLevel::Healthy
        };

        HealthStatus {
            status,
            issues,
            warnings,
            metrics: snapshot,
        }
    }
}

impl Default for ResponseTimeTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ResponseTimeTracker {
    pub fn new() -> Self {
        Self {
            operations: HashMap::new(),
            lowest_discernible_micros: DEFAULT_LOWEST_DISCERNIBLE_MICROS,
            highest_trackable_micros: DEFAULT_HIGHEST_TRACKABLE_MICROS,
            significant_figures: DEFAULT_SIGNIFICANT_FIGURES,
        }
    }

    #[cfg(test)]
    pub fn with_bounds(
        lowest_discernible_micros: u64,
        highest_trackable_micros: u64,
        significant_figures: u8,
    ) -> Self {
        Self {
            operations: HashMap::new(),
            lowest_discernible_micros: lowest_discernible_micros.max(1),
            highest_trackable_micros: highest_trackable_micros
                .max(lowest_discernible_micros.max(1)),
            significant_figures: significant_figures.clamp(1, 5),
        }
    }

    #[allow(dead_code)]
    pub fn add_sample(&mut self, operation: &str, duration: Duration) -> bool {
        let micros = duration_to_micros(duration);
        let lowest = self.lowest_discernible_micros;
        let highest = self.highest_trackable_micros;
        let significant = self.significant_figures;
        let histogram = self
            .operations
            .entry(operation.to_string())
            .or_insert_with(|| OperationLatencyHistogram::new(lowest, highest, significant));

        histogram.record(micros, highest)
    }

    pub fn get_average(&self, operation: &str) -> Option<f64> {
        self.get_latency_metrics(operation)
            .and_then(|metrics| metrics.average_ms)
    }

    pub fn get_latency_metrics(&self, operation: &str) -> Option<OperationLatencyMetrics> {
        let histogram = self.operations.get(operation)?;
        histogram.metrics()
    }
}

impl OperationLatencyHistogram {
    fn new(
        lowest_discernible_micros: u64,
        highest_trackable_micros: u64,
        significant_figures: u8,
    ) -> Self {
        let lowest = lowest_discernible_micros.max(1);
        let highest = highest_trackable_micros.max(lowest);
        let sig_figs = significant_figures.clamp(1, 5);

        // Try to create histogram with requested bounds; fall back to unbounded if that fails.
        // Histogram::new(2) creates an auto-resizing histogram without fixed bounds.
        let histogram = Histogram::new_with_bounds(lowest, highest, sig_figs)
            .or_else(|e| {
                tracing::warn!(
                    target: "metrics",
                    error = %e,
                    lowest,
                    highest,
                    sig_figs,
                    "Failed to create histogram with requested bounds, using unbounded fallback"
                );
                // Fallback: unbounded auto-resizing histogram with 2 significant figures
                Histogram::new(2)
            })
            .or_else(|_| {
                tracing::error!(target: "metrics", "Histogram::new(2) failed, trying sig_figs=1");
                Histogram::new(1)
            })
            .ok(); // Convert to Option - None means all attempts failed

        if histogram.is_none() {
            tracing::error!(target: "metrics", "All histogram creation attempts failed - metrics will not be recorded");
        }

        Self { histogram }
    }

    fn record(&mut self, micros: u64, highest_trackable_micros: u64) -> bool {
        let was_clamped = micros > highest_trackable_micros;
        let value = if was_clamped {
            highest_trackable_micros
        } else {
            micros
        };
        if let Some(ref mut histogram) = self.histogram {
            if let Err(error) = histogram.record(value) {
                tracing::warn!(
                    target: "metrics",
                    %error,
                    clamped_value = value,
                    highest_trackable_micros,
                    "failed to record latency sample"
                );
            }
        }
        was_clamped
    }

    fn metrics(&self) -> Option<OperationLatencyMetrics> {
        let histogram = self.histogram.as_ref()?;
        if histogram.is_empty() {
            return None;
        }

        Some(OperationLatencyMetrics {
            average_ms: Some(histogram.mean() / MICROS_PER_MS),
            p50_ms: Some(self.percentile(50.0)),
            p95_ms: Some(self.percentile(95.0)),
            p99_ms: Some(self.percentile(99.0)),
            min_ms: Some(histogram.min() as f64 / MICROS_PER_MS),
            max_ms: Some(histogram.max() as f64 / MICROS_PER_MS),
            sample_count: histogram.len(),
        })
    }

    fn percentile(&self, percentile: f64) -> f64 {
        self.histogram
            .as_ref()
            .map(|h| h.value_at_percentile(percentile) as f64 / MICROS_PER_MS)
            .unwrap_or(0.0)
    }
}

const MICROS_PER_MS: f64 = 1000.0;

fn duration_to_micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HealthStatus {
    pub status: HealthStatusLevel,
    pub issues: Vec<String>,
    pub warnings: Vec<String>,
    pub metrics: MetricsSnapshot,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub enum HealthStatusLevel {
    Healthy,
    Degraded,
    Unhealthy,
}

/// Utility struct for timing operations
#[allow(dead_code)]
pub struct OperationTimer {
    #[allow(dead_code)]
    operation: String,
    #[allow(dead_code)]
    start: Instant,
    #[allow(dead_code)]
    metrics: Arc<ServerMetrics>,
}

impl OperationTimer {
    pub fn new(operation: &str, metrics: Arc<ServerMetrics>) -> Self {
        Self {
            operation: operation.to_string(),
            start: Instant::now(),
            metrics,
        }
    }

    #[allow(dead_code)]
    pub async fn finish(self) {
        let duration = self.start.elapsed();
        self.metrics
            .record_response_time(&self.operation, duration)
            .await;
    }

    #[allow(dead_code)]
    pub async fn finish_with_result<T, E>(self, result: &Result<T, E>) {
        let duration = self.start.elapsed();
        self.metrics
            .record_response_time(&self.operation, duration)
            .await;

        if result.is_err() {
            match self.operation.as_str() {
                "room_creation" => self.metrics.increment_room_creation_failures(),
                "room_join" => self.metrics.increment_room_join_failures(),
                "query" => self.metrics.increment_internal_errors(),
                _ => {}
            }
        }
    }
}

/// Stub for DashboardCacheMetrics (not used in signal-fish-server)
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct DashboardCacheMetrics {
    pub refresh_count: u64,
    pub refresh_errors: u64,
    pub last_refresh_timestamp: u64,
    pub refresh_failures: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // -----------------------------------------------------------------------
    // E. Metrics atomic tests
    // -----------------------------------------------------------------------

    /// E20: Decrement from 0 stays at 0, not u64::MAX (underflow prevention).
    #[tokio::test]
    async fn test_decrement_active_connections_no_underflow() {
        let metrics = ServerMetrics::new();

        // Decrement 10 times from 0
        for _ in 0..10 {
            metrics.decrement_active_connections();
        }

        let value = metrics.active_connections.load(Ordering::Relaxed);
        assert_eq!(
            value, 0,
            "active_connections should remain 0 after decrement from 0, got {value}"
        );

        // The production code always increments disconnections even when the
        // active_connections decrement is a no-op, so verify the side-effect.
        assert_eq!(
            metrics.disconnections.load(Ordering::Relaxed),
            10,
            "disconnections should still be incremented 10 times even when active_connections was already 0"
        );
    }

    /// E21: Decrement reconnection_sessions_active from 0 stays at 0.
    #[tokio::test]
    async fn test_decrement_reconnection_sessions_no_underflow() {
        let metrics = ServerMetrics::new();

        // Decrement 10 times from 0
        for _ in 0..10 {
            metrics.decrement_reconnection_sessions_active();
        }

        let value = metrics.reconnection_sessions_active.load(Ordering::Relaxed);
        assert_eq!(
            value, 0,
            "reconnection_sessions_active should remain 0 after decrement from 0, got {value}"
        );
    }

    /// E22: Sequential phases of concurrent operations yield correct count.
    ///
    /// Phase 1: Increment connections 100 times concurrently.
    /// Phase 2: (after all increments complete) Decrement 50 times concurrently.
    /// Final active_connections should be 50.
    ///
    /// Note: this tests sequential phases of concurrent operations, not
    /// simultaneous increments and decrements.
    #[tokio::test]
    async fn test_concurrent_increment_decrement_active_connections() {
        let metrics = Arc::new(ServerMetrics::new());

        // Phase 1: 100 concurrent increments
        let inc_barrier = Arc::new(tokio::sync::Barrier::new(100));
        let mut handles = Vec::with_capacity(100);
        for _ in 0..100 {
            let metrics = Arc::clone(&metrics);
            let barrier = Arc::clone(&inc_barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                metrics.increment_connections();
            }));
        }
        for handle in handles {
            handle.await.expect("increment task should not panic");
        }

        let after_inc = metrics.active_connections.load(Ordering::Relaxed);
        assert_eq!(
            after_inc, 100,
            "After 100 increments, active_connections should be 100, got {after_inc}"
        );

        // Phase 2: 50 concurrent decrements
        let dec_barrier = Arc::new(tokio::sync::Barrier::new(50));
        let mut handles = Vec::with_capacity(50);
        for _ in 0..50 {
            let metrics = Arc::clone(&metrics);
            let barrier = Arc::clone(&dec_barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                metrics.decrement_active_connections();
            }));
        }
        for handle in handles {
            handle.await.expect("decrement task should not panic");
        }

        let final_value = metrics.active_connections.load(Ordering::Relaxed);
        assert_eq!(
            final_value, 50,
            "After 100 increments and 50 decrements, active_connections should be 50, got {final_value}"
        );

        // total_connections is monotonic (only incremented, never decremented)
        let total = metrics.total_connections.load(Ordering::Relaxed);
        assert_eq!(
            total, 100,
            "total_connections should be 100 (never decremented), got {total}"
        );
    }
}
