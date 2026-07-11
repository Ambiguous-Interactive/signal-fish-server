use hdrhistogram::Histogram;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Per-connection delivery bookkeeping backing the protocol v3 `RelayStats`
/// frame (`ServerMessage::RelayStats`).
///
/// Counters are cumulative for the lifetime of one logical connection
/// (carried across a reconnection's player-id reassignment) and are only
/// tracked when `websocket.delivery_stats_interval_secs > 0` — a disabled
/// deployment keeps the registry empty so the delivery hot path pays one
/// cheap miss per attempt. All access is `Ordering::Relaxed`: these are
/// monotonic diagnostics, never synchronization.
#[derive(Debug, Default)]
pub struct ConnectionDeliveryStats {
    /// Messages the reliable delivery paths enqueued for this connection.
    pub sent_to_you: AtomicU64,
    /// Messages abandoned for this connection (slow-consumer timeout drops
    /// and undeliverable-encoding replacements).
    pub dropped_for_you: AtomicU64,
    /// Deliveries that had to wait on this connection's full outbound queue.
    pub backpressure_events: AtomicU64,
}

#[derive(Debug)]
struct DeliveryClassAtomicCounters {
    attempted: AtomicU64,
    delivered: AtomicU64,
    superseded: AtomicU64,
    dropped_full: AtomicU64,
    dropped: AtomicU64,
    abandoned: AtomicU64,
    unsupported_format: AtomicU64,
}

impl DeliveryClassAtomicCounters {
    fn new() -> Self {
        Self {
            attempted: AtomicU64::new(0),
            delivered: AtomicU64::new(0),
            superseded: AtomicU64::new(0),
            dropped_full: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            abandoned: AtomicU64::new(0),
            unsupported_format: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> DeliveryClassMetrics {
        DeliveryClassMetrics {
            attempted: self.attempted.load(Ordering::Relaxed),
            delivered: self.delivered.load(Ordering::Relaxed),
            superseded: self.superseded.load(Ordering::Relaxed),
            dropped_full: self.dropped_full.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            abandoned: self.abandoned.load(Ordering::Relaxed),
            unsupported_format: self.unsupported_format.load(Ordering::Relaxed),
        }
    }
}

/// Comprehensive metrics collection for in-memory signaling server
#[derive(Debug)]
pub struct ServerMetrics {
    /// Per-connection delivery statistics keyed by player id (see
    /// [`ConnectionDeliveryStats`]). Lives here — beside the server-wide
    /// delivery counters — because the reliable server delivery paths already
    /// carry `(metrics, player_id)`, so the per-connection ledger needs no new
    /// plumbing through the delivery handles. Populated only when RelayStats
    /// emission is enabled.
    connection_delivery_stats:
        dashmap::DashMap<crate::protocol::PlayerId, Arc<ConnectionDeliveryStats>>,
    delivery_class_counters: [DeliveryClassAtomicCounters; 3],

    // Connection metrics
    pub total_connections: AtomicU64,
    pub active_connections: AtomicU64,
    pub disconnections: AtomicU64,
    pub connection_errors: AtomicU64,
    pub websocket_messages_dropped: AtomicU64,
    /// Times a full outbound queue forced delivery to wait for capacity
    /// (the send eventually succeeded; nothing was lost). A rising rate means
    /// clients are close to the slow-consumer limit.
    pub websocket_backpressure_events: AtomicU64,
    /// Connections force-closed because their outbound queue stayed full past
    /// `websocket.slow_consumer_timeout_ms`.
    pub websocket_slow_consumer_disconnects: AtomicU64,
    /// Server-initiated RFC 6455 WebSocket pings that did not receive their
    /// matching Pong before `websocket.pong_timeout_secs`.
    pub websocket_ping_timeouts: AtomicU64,
    /// Delivery attempts routed through the reliable server delivery and
    /// reservation paths: one per message per recipient, counted before the
    /// outcome is known. Together with
    /// `websocket_deliveries_enqueued`, `websocket_deliveries_channel_closed`,
    /// `websocket_deliveries_canceled`, and `websocket_messages_dropped` this
    /// carries the delivery conservation law — every attempt resolves as
    /// enqueued, channel-closed, canceled before enqueue, or slow-consumer
    /// drop, so at any quiescent point
    /// `enqueued + channel_closed + canceled <= attempts <= enqueued + channel_closed + canceled + dropped`
    /// (the drop counter also tallies messages abandoned with a closing
    /// connection *after* they were enqueued, hence the upper bound rather
    /// than exact equality).
    pub websocket_delivery_attempts: AtomicU64,
    /// Delivery attempts enqueued on the recipient's outbound queue — the
    /// try_send fast path and the post-backpressure success alike. Enqueued
    /// does not mean written to the socket yet: a message abandoned later with
    /// a closing connection is additionally counted in
    /// `websocket_messages_dropped`.
    pub websocket_deliveries_enqueued: AtomicU64,
    /// Delivery attempts that found the recipient's connection already
    /// closing (its queue receiver gone), whether up front or while
    /// backpressured. A normal disconnect race, not a delivery fault.
    pub websocket_deliveries_channel_closed: AtomicU64,
    /// Conditional delivery attempts canceled before enqueue because the
    /// commit condition no longer held: shutdown drain began, the caller
    /// predicate became false, or a reserved recipient snapshot went stale.
    /// These are intentional skips, not delivery drops and not per-connection
    /// loss.
    pub websocket_deliveries_canceled: AtomicU64,

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
    /// Control events evicted from a room's bounded replay ring while a
    /// reconnection was pending (the affected reconnector's `missed_events`
    /// arrives with `replay: "truncated"`). A sustained non-zero rate means
    /// `event_buffer_size` is too small for the room churn it serves.
    pub reconnection_events_evicted: AtomicU64,

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

    // Transport / session-plan metrics (Protocol v3)
    /// Finalization-time v3 `SessionPlan` publication events. Counted once per
    /// finalized room that has at least one v3 recipient, including an explicit
    /// relay-floor plan; this is not a per-recipient frame count. Mid-session
    /// re-plans and finalized-room joins/reconnects are counted separately.
    pub session_plans_emitted: AtomicU64,
    /// Mid-session host re-plan events (host failover / self-heal): one per
    /// re-plan **event**, NOT per recipient. Counted whenever an invalid
    /// stored host (absent from the room, or seated but no longer capable of
    /// the session) is replaced via re-election and fresh plans are emitted —
    /// after a departure, or healing a wedged entry during a late join. NOT
    /// counted when no remaining member can host the session (the stored plan
    /// is dropped and nothing is emitted — no re-plan happened).
    pub session_replans_emitted: AtomicU64,
    /// Late-join / reconnect plan publications: one per joining actor that
    /// received a plan for an already-active session. A reconnect refreshes all
    /// v3 incumbents in the same publication but still counts once. An actor
    /// served by a self-heal re-plan instead is part of that single
    /// `session_replans_emitted` event and is NOT counted here.
    pub session_plans_late_join: AtomicU64,
    /// Finalized rooms whose chosen topology was `mesh`.
    pub topology_mesh_selected: AtomicU64,
    /// Finalized rooms whose chosen topology was `host`.
    pub topology_host_selected: AtomicU64,
    /// Finalized rooms whose chosen topology was `relay` (the floor).
    pub topology_relay_selected: AtomicU64,
    /// Finalized rooms whose chosen data-path transport was `webrtc`.
    pub transport_webrtc_selected: AtomicU64,
    /// Finalized rooms whose chosen data-path transport was `direct`.
    pub transport_direct_selected: AtomicU64,
    /// Finalized rooms whose chosen data-path transport was `relay` (the floor).
    pub transport_relay_selected: AtomicU64,
    /// First reports or P2P data-path state transitions a client reported as
    /// established (`TransportStatus` with a P2P transport and `connected: true`).
    pub p2p_established: AtomicU64,
    /// First reports or relay-fallback state transitions a client reported
    /// (`TransportStatus` with `connected: false`).
    pub relay_fallback: AtomicU64,
    /// Opaque WebRTC `Signal` messages accepted for best-effort dispatch to a peer.
    pub signals_relayed: AtomicU64,
    /// TURN `IceServer` credentials minted, totaled across every issuance site:
    /// `SessionPlan`s (finalize, host re-plan, late join / reconnect) AND the
    /// ICE pre-gather lists on `RoomJoined` / `Reconnected`. This is the
    /// total-issuance counter used for TURN capacity planning. The pre-gather
    /// gate is off for `Finalized` rooms, so a late join / reconnect into an
    /// active WebRTC session mints ONLY via its `SessionPlan` — one logical
    /// join event never mints twice.
    pub turn_credentials_issued: AtomicU64,
    /// `RoomJoined` / `Reconnected` payloads that actually carried a non-empty
    /// ICE pre-gather list: exactly once per carrying payload (the deferred
    /// "RoomJoined ICE pre-gather" refinement). An eligible joiner
    /// whose composed list is empty (no static ICE, no STUN urls, TURN
    /// disabled) skips the field on the wire and is NOT counted.
    pub ice_pregather_emitted: AtomicU64,
    /// `PeerTransportStatus` fan-out events: accepted `TransportStatus` state
    /// changes (first report or a real transition — duplicates never fan out)
    /// from a client seated in a room, fanned out to the room's other v3
    /// members. One per **event**, not per recipient (mirroring
    /// `session_replans_emitted`), and counted even when no co-member
    /// negotiated v3 (the event still happened; zero deliveries).
    pub transport_status_fanout: AtomicU64,
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
    pub transport: TransportMetrics,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConnectionMetrics {
    pub total_connections: u64,
    pub active_connections: u64,
    pub disconnections: u64,
    pub connection_errors: u64,
    pub websocket_messages_dropped: u64,
    pub websocket_backpressure_events: u64,
    pub websocket_slow_consumer_disconnects: u64,
    pub websocket_ping_timeouts: u64,
    pub websocket_ping_rtt: OperationLatencyMetrics,
    pub websocket_delivery_attempts: u64,
    pub websocket_deliveries_enqueued: u64,
    pub websocket_deliveries_channel_closed: u64,
    pub websocket_deliveries_canceled: u64,
    pub delivery_by_class: DeliveryMetricsByClass,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryClassMetrics {
    pub attempted: u64,
    pub delivered: u64,
    pub superseded: u64,
    pub dropped_full: u64,
    pub dropped: u64,
    pub abandoned: u64,
    pub unsupported_format: u64,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryMetricsByClass {
    pub reliable: DeliveryClassMetrics,
    pub latest: DeliveryClassMetrics,
    pub volatile: DeliveryClassMetrics,
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
    /// Control events evicted from a replay ring while a reconnection was
    /// pending (that player's replay is reported truncated).
    pub events_evicted: u64,
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

/// Protocol v3 transport / session-plan observability.
///
/// Exposes the per-finalized-room topology/transport selection ratios, the
/// P2P-established-vs-relay-fallback first-report/transition split (reported by
/// clients via `TransportStatus`), the count of opaque WebRTC signals accepted
/// for best-effort dispatch, the number of TURN credentials minted, and the
/// number of `PeerTransportStatus` fan-out events — so dashboards can see how
/// often the relay floor is actually upgraded to a peer-to-peer path.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TransportMetrics {
    pub session_plans_emitted: u64,
    pub session_replans_emitted: u64,
    pub session_plans_late_join: u64,
    pub topology_mesh_selected: u64,
    pub topology_host_selected: u64,
    pub topology_relay_selected: u64,
    pub transport_webrtc_selected: u64,
    pub transport_direct_selected: u64,
    pub transport_relay_selected: u64,
    pub p2p_established: u64,
    pub relay_fallback: u64,
    pub signals_relayed: u64,
    pub turn_credentials_issued: u64,
    pub transport_status_fanout: u64,
    pub ice_pregather_emitted: u64,
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
            connection_delivery_stats: dashmap::DashMap::new(),
            delivery_class_counters: std::array::from_fn(|_| DeliveryClassAtomicCounters::new()),
            total_connections: AtomicU64::new(0),
            active_connections: AtomicU64::new(0),
            disconnections: AtomicU64::new(0),
            connection_errors: AtomicU64::new(0),
            websocket_messages_dropped: AtomicU64::new(0),
            websocket_backpressure_events: AtomicU64::new(0),
            websocket_slow_consumer_disconnects: AtomicU64::new(0),
            websocket_ping_timeouts: AtomicU64::new(0),
            websocket_delivery_attempts: AtomicU64::new(0),
            websocket_deliveries_enqueued: AtomicU64::new(0),
            websocket_deliveries_channel_closed: AtomicU64::new(0),
            websocket_deliveries_canceled: AtomicU64::new(0),
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
            reconnection_events_evicted: AtomicU64::new(0),
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
            session_plans_emitted: AtomicU64::new(0),
            session_replans_emitted: AtomicU64::new(0),
            session_plans_late_join: AtomicU64::new(0),
            topology_mesh_selected: AtomicU64::new(0),
            topology_host_selected: AtomicU64::new(0),
            topology_relay_selected: AtomicU64::new(0),
            transport_webrtc_selected: AtomicU64::new(0),
            transport_direct_selected: AtomicU64::new(0),
            transport_relay_selected: AtomicU64::new(0),
            p2p_established: AtomicU64::new(0),
            relay_fallback: AtomicU64::new(0),
            signals_relayed: AtomicU64::new(0),
            turn_credentials_issued: AtomicU64::new(0),
            transport_status_fanout: AtomicU64::new(0),
            ice_pregather_emitted: AtomicU64::new(0),
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

    /// Record several dropped messages at once (e.g. a slow consumer's
    /// abandoned queue at disconnect), keeping the drop counter honest.
    pub fn add_websocket_messages_dropped(&self, count: u64) {
        if count > 0 {
            self.websocket_messages_dropped
                .fetch_add(count, Ordering::Relaxed);
        }
    }

    pub fn increment_websocket_backpressure_events(&self) {
        self.websocket_backpressure_events
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_websocket_slow_consumer_disconnects(&self) {
        self.websocket_slow_consumer_disconnects
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_websocket_ping_timeouts(&self) {
        self.websocket_ping_timeouts.fetch_add(1, Ordering::Relaxed);
    }

    pub async fn record_websocket_ping_rtt(&self, duration: Duration) {
        self.record_response_time("websocket_ping_rtt", duration)
            .await;
    }

    /// Record one delivery attempt entering the reliable delivery paths,
    /// before its outcome is known (see the field doc for the conservation
    /// law this anchors).
    pub fn increment_websocket_delivery_attempts(&self) {
        self.websocket_delivery_attempts
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record one delivery attempt enqueued on the recipient's outbound queue
    /// (fast path or after backpressure).
    pub fn increment_websocket_deliveries_enqueued(&self) {
        self.websocket_deliveries_enqueued
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record one delivery attempt that found the recipient's connection
    /// already closing (queue receiver gone).
    pub fn increment_websocket_deliveries_channel_closed(&self) {
        self.websocket_deliveries_channel_closed
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record one conditional delivery attempt intentionally canceled before
    /// enqueue because the commit condition no longer held.
    pub fn increment_websocket_deliveries_canceled(&self) {
        self.websocket_deliveries_canceled
            .fetch_add(1, Ordering::Relaxed);
    }

    fn delivery_class_counter(
        &self,
        class: crate::protocol::DeliveryClass,
    ) -> &DeliveryClassAtomicCounters {
        let [reliable, latest, volatile] = &self.delivery_class_counters;
        match class {
            crate::protocol::DeliveryClass::Reliable => reliable,
            crate::protocol::DeliveryClass::Latest => latest,
            crate::protocol::DeliveryClass::Volatile => volatile,
        }
    }

    pub(crate) fn increment_delivery_class_attempted(&self, class: crate::protocol::DeliveryClass) {
        self.delivery_class_counter(class)
            .attempted
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn increment_delivery_class_delivered(&self, class: crate::protocol::DeliveryClass) {
        self.delivery_class_counter(class)
            .delivered
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn increment_delivery_class_superseded(&self) {
        self.delivery_class_counter(crate::protocol::DeliveryClass::Latest)
            .superseded
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn increment_delivery_class_dropped_full(&self) {
        self.delivery_class_counter(crate::protocol::DeliveryClass::Latest)
            .dropped_full
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn increment_delivery_class_dropped(&self, class: crate::protocol::DeliveryClass) {
        self.delivery_class_counter(class)
            .dropped
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn add_delivery_class_abandoned(
        &self,
        class: crate::protocol::DeliveryClass,
        count: u64,
    ) {
        self.delivery_class_counter(class)
            .abandoned
            .fetch_add(count, Ordering::Relaxed);
    }

    pub(crate) fn increment_delivery_class_unsupported_format(
        &self,
        class: crate::protocol::DeliveryClass,
    ) {
        self.delivery_class_counter(class)
            .unsupported_format
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn delivery_metrics_by_class(&self) -> DeliveryMetricsByClass {
        DeliveryMetricsByClass {
            reliable: self.delivery_class_counters[0].snapshot(),
            latest: self.delivery_class_counters[1].snapshot(),
            volatile: self.delivery_class_counters[2].snapshot(),
        }
    }

    // Per-connection delivery statistics (protocol v3 RelayStats)

    /// Start tracking per-connection delivery statistics for `player_id`,
    /// returning the (fresh) ledger. Called at connection registration only
    /// when RelayStats emission is enabled.
    pub fn register_connection_delivery_stats(
        &self,
        player_id: crate::protocol::PlayerId,
    ) -> Arc<ConnectionDeliveryStats> {
        let stats = Arc::new(ConnectionDeliveryStats::default());
        self.connection_delivery_stats
            .insert(player_id, Arc::clone(&stats));
        stats
    }

    /// The per-connection delivery ledger for `player_id`, or `None` when
    /// tracking is disabled or the connection is gone.
    pub fn connection_delivery_stats(
        &self,
        player_id: &crate::protocol::PlayerId,
    ) -> Option<Arc<ConnectionDeliveryStats>> {
        self.connection_delivery_stats
            .get(player_id)
            .map(|entry| Arc::clone(entry.value()))
    }

    /// Stop tracking `player_id` (connection removed).
    pub fn unregister_connection_delivery_stats(&self, player_id: &crate::protocol::PlayerId) {
        self.connection_delivery_stats.remove(player_id);
    }

    /// Re-key a connection's ledger across a reconnection reassignment so the
    /// cumulative counters follow the surviving connection.
    pub fn rekey_connection_delivery_stats(
        &self,
        current: &crate::protocol::PlayerId,
        reassigned: crate::protocol::PlayerId,
    ) {
        if let Some((_, stats)) = self.connection_delivery_stats.remove(current) {
            self.connection_delivery_stats.insert(reassigned, stats);
        }
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

    pub fn add_reconnection_events_evicted(&self, count: u64) {
        if count > 0 {
            self.reconnection_events_evicted
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

    // Transport / session-plan metrics (Protocol v3)

    /// Record the topology chosen for one finalized room. Called once per
    /// finalize (in `emit_session_plan`), including the relay-resolved floor, so
    /// the three topology counters together total the finalized-room count.
    pub fn record_topology_selected(&self, topology: crate::protocol::Topology) {
        let counter = match topology {
            crate::protocol::Topology::Mesh => &self.topology_mesh_selected,
            crate::protocol::Topology::Host => &self.topology_host_selected,
            crate::protocol::Topology::Relay => &self.topology_relay_selected,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Record the data-path transport chosen for one finalized room. Called once
    /// per finalize (in `emit_session_plan`), including the relay-resolved floor.
    pub fn record_transport_selected(&self, transport: crate::protocol::Transport) {
        let counter = match transport {
            crate::protocol::Transport::WebRtc => &self.transport_webrtc_selected,
            crate::protocol::Transport::Direct => &self.transport_direct_selected,
            crate::protocol::Transport::Relay => &self.transport_relay_selected,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one finalization-time v3 `SessionPlan` publication, including an
    /// explicit relay-floor result. Count once per finalized room with at least
    /// one v3 recipient, not once per frame. Mid-session re-plans and finalized
    /// joins/reconnects must NOT move this counter -- they have their own
    /// ([`Self::increment_session_replans_emitted`] /
    /// [`Self::increment_session_plans_late_join`]) so the three emission kinds
    /// stay independently observable.
    pub fn increment_session_plans_emitted(&self) {
        self.session_plans_emitted.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one mid-session host re-plan event — an invalid stored host
    /// (absent, or seated but no longer session-capable) was replaced via
    /// re-election and fresh plans emitted, whether triggered by a departure
    /// (host failover) or by a late join healing a wedged entry: once per
    /// **event**, not per recipient. Must NOT be called when no remaining
    /// member can host the session (the stored plan is dropped without any
    /// emission — that is not a re-plan).
    pub fn increment_session_replans_emitted(&self) {
        self.session_replans_emitted.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one late-join / reconnect plan publication for the joining actor.
    /// Reconnect may refresh every v3 incumbent in that same publication but
    /// still counts once. An actor served by a self-heal re-plan instead is
    /// covered by [`Self::increment_session_replans_emitted`].
    pub fn increment_session_plans_late_join(&self) {
        self.session_plans_late_join.fetch_add(1, Ordering::Relaxed);
    }

    /// Record that a client reported an established P2P data path for the first
    /// time or as a state transition (`TransportStatus` with a P2P transport and
    /// `connected: true`).
    pub fn record_p2p_established(&self) {
        self.p2p_established.fetch_add(1, Ordering::Relaxed);
    }

    /// Record that a client reported the relay floor for the first time or as a
    /// state transition (`TransportStatus` with `connected: false`).
    pub fn record_relay_fallback(&self) {
        self.relay_fallback.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one opaque WebRTC `Signal` accepted for best-effort dispatch to a peer.
    pub fn increment_signals_relayed(&self) {
        self.signals_relayed.fetch_add(1, Ordering::Relaxed);
    }

    /// Record `count` minted TURN `IceServer` credentials on the total-issuance
    /// counter. Called from every issuance site — `SessionPlan` emission
    /// (finalize, host re-plan, late join / reconnect) and the `RoomJoined` /
    /// `Reconnected` ICE pre-gather path — see the field doc for the
    /// no-double-count invariant between the last two.
    pub fn add_turn_credentials_issued(&self, count: u64) {
        if count > 0 {
            self.turn_credentials_issued
                .fetch_add(count, Ordering::Relaxed);
        }
    }

    /// Record one `RoomJoined` / `Reconnected` payload that actually carried a
    /// non-empty ICE pre-gather list: exactly once per carrying payload, never
    /// for an ineligible joiner and never for an eligible joiner whose composed
    /// list came out empty (the field is then skipped on the wire — nothing was
    /// emitted).
    pub fn increment_ice_pregather_emitted(&self) {
        self.ice_pregather_emitted.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one `PeerTransportStatus` fan-out event: an accepted
    /// `TransportStatus` state change from a client seated in a room was fanned
    /// out to the room's other v3 members. Once per **event**, not per
    /// recipient (mirroring [`Self::increment_session_replans_emitted`]);
    /// duplicate reports and reports from room-less clients never count.
    pub fn record_transport_status_fanout(&self) {
        self.transport_status_fanout.fetch_add(1, Ordering::Relaxed);
    }

    // Snapshot generation
    pub async fn snapshot(&self) -> MetricsSnapshot {
        let tracker = self.average_response_times.read().await;
        let room_creation_latency = tracker
            .get_latency_metrics("room_creation")
            .unwrap_or_default();
        let room_join_latency = tracker.get_latency_metrics("room_join").unwrap_or_default();
        let query_latency = tracker.get_latency_metrics("query").unwrap_or_default();
        let websocket_ping_rtt = tracker
            .get_latency_metrics("websocket_ping_rtt")
            .unwrap_or_default();

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
                websocket_backpressure_events: self
                    .websocket_backpressure_events
                    .load(Ordering::Relaxed),
                websocket_slow_consumer_disconnects: self
                    .websocket_slow_consumer_disconnects
                    .load(Ordering::Relaxed),
                websocket_ping_timeouts: self.websocket_ping_timeouts.load(Ordering::Relaxed),
                websocket_ping_rtt,
                websocket_delivery_attempts: self
                    .websocket_delivery_attempts
                    .load(Ordering::Relaxed),
                websocket_deliveries_enqueued: self
                    .websocket_deliveries_enqueued
                    .load(Ordering::Relaxed),
                websocket_deliveries_channel_closed: self
                    .websocket_deliveries_channel_closed
                    .load(Ordering::Relaxed),
                websocket_deliveries_canceled: self
                    .websocket_deliveries_canceled
                    .load(Ordering::Relaxed),
                delivery_by_class: self.delivery_metrics_by_class(),
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
                events_evicted: self.reconnection_events_evicted.load(Ordering::Relaxed),
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
            transport: TransportMetrics {
                session_plans_emitted: self.session_plans_emitted.load(Ordering::Relaxed),
                session_replans_emitted: self.session_replans_emitted.load(Ordering::Relaxed),
                session_plans_late_join: self.session_plans_late_join.load(Ordering::Relaxed),
                topology_mesh_selected: self.topology_mesh_selected.load(Ordering::Relaxed),
                topology_host_selected: self.topology_host_selected.load(Ordering::Relaxed),
                topology_relay_selected: self.topology_relay_selected.load(Ordering::Relaxed),
                transport_webrtc_selected: self.transport_webrtc_selected.load(Ordering::Relaxed),
                transport_direct_selected: self.transport_direct_selected.load(Ordering::Relaxed),
                transport_relay_selected: self.transport_relay_selected.load(Ordering::Relaxed),
                p2p_established: self.p2p_established.load(Ordering::Relaxed),
                relay_fallback: self.relay_fallback.load(Ordering::Relaxed),
                signals_relayed: self.signals_relayed.load(Ordering::Relaxed),
                turn_credentials_issued: self.turn_credentials_issued.load(Ordering::Relaxed),
                transport_status_fanout: self.transport_status_fanout.load(Ordering::Relaxed),
                ice_pregather_emitted: self.ice_pregather_emitted.load(Ordering::Relaxed),
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
