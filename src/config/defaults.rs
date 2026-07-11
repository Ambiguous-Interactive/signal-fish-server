//! Default value functions for configuration fields.
//!
//! This module contains all the default value functions used by serde's `#[serde(default = ...)]`
//! attributes throughout the configuration system. Functions are organized by category for
//! easier maintenance.

use super::logging::LogFormat;
use super::security::ClientAuthMode;
use serde::{Deserialize, Serialize};
use std::fmt;

// =============================================================================
// Port & Root Config
// =============================================================================

pub const fn default_port() -> u16 {
    3536
}

// =============================================================================
// Server Defaults
// =============================================================================

pub const fn default_max_players() -> u8 {
    8
}

pub const fn default_ping_timeout() -> u64 {
    30
}

/// Default threshold for heartbeat throttling (seconds).
/// Controls how frequently heartbeat timestamps are recorded.
pub const fn default_heartbeat_throttle_secs() -> u64 {
    30
}

pub const fn default_room_cleanup_interval() -> u64 {
    60
}

pub const fn default_drain_grace_secs() -> u64 {
    30
}

pub const fn default_max_rooms_per_game() -> usize {
    1000
}

pub const fn default_empty_room_timeout() -> u64 {
    300 // 5 minutes
}

pub const fn default_inactive_room_timeout() -> u64 {
    3600 // 1 hour
}

pub const fn default_reconnection_window() -> u64 {
    300 // 5 minutes
}

pub const fn default_event_buffer_size() -> usize {
    100 // Buffer last 100 events per room
}

pub const fn default_enable_reconnection() -> bool {
    true // Enable reconnection by default
}

// =============================================================================
// Rate Limit Defaults
// =============================================================================

pub const fn default_max_room_creations() -> u32 {
    5
}

pub const fn default_rate_limit_time_window() -> u64 {
    60
}

pub const fn default_max_join_attempts() -> u32 {
    20
}

pub const fn default_max_signals() -> u32 {
    600
}

pub const fn default_max_signal_errors() -> u32 {
    60
}

// =============================================================================
// Protocol Defaults
// =============================================================================

pub const fn default_max_game_name_length() -> usize {
    64
}

pub const fn default_room_code_length() -> usize {
    6
}

pub const fn default_max_player_name_length() -> usize {
    32
}

pub const fn default_max_players_limit() -> u8 {
    100
}

pub const fn default_enable_message_pack_game_data() -> bool {
    true
}

/// SDK platform/version enforcement is OPT-IN (issue #136, F6): with the
/// old `true` default, the prepopulated platform list made a default-config
/// server reject every client that did not claim to be one of the known
/// engines — including `platform: None` (the reference clients) and any
/// custom/Rust client. Deployments that ship engine SDKs enable it
/// explicitly alongside their own version floors.
pub const fn default_sdk_enforce() -> bool {
    false
}

pub const fn default_min_protocol_version() -> u16 {
    super::protocol::SERVER_MIN_PROTOCOL_VERSION
}

pub const fn default_max_protocol_version() -> u16 {
    super::protocol::SERVER_MAX_PROTOCOL_VERSION
}

// =============================================================================
// Session Defaults
// =============================================================================

/// Default session topology: the relay floor (v2-equivalent, always available).
pub const fn default_session_topology() -> crate::protocol::Topology {
    crate::protocol::Topology::Relay
}

/// WebRTC transport is enabled by default (it is the only true browser P2P path).
pub const fn default_enable_webrtc() -> bool {
    true
}

/// Direct (LAN / routable) transport is enabled by default.
pub const fn default_enable_direct() -> bool {
    true
}

/// ICE pre-gather on `RoomJoined` / `Reconnected` is enabled by default: it only
/// fires for v3 WebRTC-capable joiners of non-relay-desired, non-finalized rooms,
/// so the v2 wire (and any relay deployment) is untouched either way.
pub const fn default_enable_ice_pregather() -> bool {
    true
}

// =============================================================================
// Player Name Validation Defaults
// =============================================================================

pub const fn default_allow_unicode_player_names() -> bool {
    true
}

pub const fn default_allow_spaces_in_player_names() -> bool {
    true
}

pub const fn default_allow_leading_trailing_whitespace() -> bool {
    false
}

pub fn default_allowed_player_name_symbols() -> Vec<char> {
    vec!['-', '_']
}

// =============================================================================
// Server Deployment Defaults
// =============================================================================

pub fn default_region_id() -> String {
    "default".to_string()
}

// =============================================================================
// Logging Defaults
// =============================================================================

pub fn default_log_dir() -> String {
    "logs".to_string()
}

pub fn default_log_filename() -> String {
    "server.log".to_string()
}

pub fn default_rotation() -> String {
    "daily".to_string()
}

pub const fn default_enable_file_logging() -> bool {
    true
}

pub const fn default_log_format() -> LogFormat {
    LogFormat::Json
}

// =============================================================================
// Security Defaults
// =============================================================================

pub fn default_cors_origins() -> String {
    "http://localhost:3000,http://localhost:5173".to_string()
}

pub const fn default_require_auth() -> bool {
    true // Enforce authentication by default; opt-out only for explicit development scenarios
}

pub const fn default_max_message_size() -> usize {
    65536 // 64KB
}

/// Default cap on the serialized size of a v3 `Signal` payload (16 KiB):
/// generously above any real SDP/ICE payload, well under the 64 KiB
/// `max_message_size` frame cap.
pub const fn default_max_signal_bytes() -> usize {
    16384 // 16KB
}

/// Default cap on concurrent connections from one IP.
///
/// 24, not 10: a 16-player session behind one NAT (LAN party, office, venue) is
/// a first-class use case, and 10 silently refused the 11th same-IP client
/// (GAP-3). 24 covers 16 players plus spectators and reconnect churn with slack.
/// (This is the connection limiter only; the number of players in a single room
/// is bounded separately by that room's `max_players`, default 8.)
pub const fn default_max_connections_per_ip() -> usize {
    24
}

pub const fn default_client_auth_mode() -> ClientAuthMode {
    ClientAuthMode::None
}

pub fn default_token_binding_subprotocol() -> String {
    "signalfish.tokenbinding.v1".to_string()
}

// =============================================================================
// Auth Maintenance Defaults
// =============================================================================

pub const fn default_rate_limit_cache_cleanup_interval_secs() -> u64 {
    300
}

pub const fn default_rate_limit_cache_retention_secs() -> u64 {
    172_800
}

pub const fn default_rate_limit_cache_alert_rows() -> u64 {
    100_000
}

// =============================================================================
// Metrics Defaults
// =============================================================================

pub const fn default_dashboard_cache_refresh_interval_secs() -> u64 {
    5
}

pub const fn default_dashboard_cache_ttl_secs() -> u64 {
    30
}

pub const fn default_dashboard_cache_history_window_secs() -> u64 {
    300
}

pub fn default_dashboard_history_fields() -> Vec<DashboardHistoryField> {
    vec![
        DashboardHistoryField::ActiveRooms,
        DashboardHistoryField::RoomsByGame,
        DashboardHistoryField::PlayerPercentiles,
        DashboardHistoryField::GamePercentiles,
    ]
}

/// Dashboard history field (used by metrics config defaults).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DashboardHistoryField {
    ActiveRooms,
    RoomsByGame,
    PlayerPercentiles,
    GamePercentiles,
    ActiveConnections,
    RoomsCreated,
}

impl DashboardHistoryField {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ActiveRooms => "active_rooms",
            Self::RoomsByGame => "rooms_by_game",
            Self::PlayerPercentiles => "player_percentiles",
            Self::GamePercentiles => "game_percentiles",
            Self::ActiveConnections => "active_connections",
            Self::RoomsCreated => "rooms_created",
        }
    }
}

impl<'de> Deserialize<'de> for DashboardHistoryField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        let token = raw.trim();

        if token.eq_ignore_ascii_case("active_rooms") || token.eq_ignore_ascii_case("ActiveRooms") {
            Ok(Self::ActiveRooms)
        } else if token.eq_ignore_ascii_case("rooms_by_game")
            || token.eq_ignore_ascii_case("RoomsByGame")
        {
            Ok(Self::RoomsByGame)
        } else if token.eq_ignore_ascii_case("player_percentiles")
            || token.eq_ignore_ascii_case("PlayerPercentiles")
        {
            Ok(Self::PlayerPercentiles)
        } else if token.eq_ignore_ascii_case("game_percentiles")
            || token.eq_ignore_ascii_case("GamePercentiles")
        {
            Ok(Self::GamePercentiles)
        } else if token.eq_ignore_ascii_case("active_connections")
            || token.eq_ignore_ascii_case("ActiveConnections")
        {
            Ok(Self::ActiveConnections)
        } else if token.eq_ignore_ascii_case("rooms_created")
            || token.eq_ignore_ascii_case("RoomsCreated")
        {
            Ok(Self::RoomsCreated)
        } else {
            Err(serde::de::Error::custom(format!(
                "invalid dashboard history field '{raw}', expected one of: active_rooms, rooms_by_game, player_percentiles, game_percentiles, active_connections, rooms_created"
            )))
        }
    }
}

impl fmt::Display for DashboardHistoryField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// =============================================================================
// Coordination Defaults
// =============================================================================

pub const fn default_dedup_cache_capacity() -> usize {
    100_000
}

pub const fn default_dedup_cache_ttl_secs() -> u64 {
    60
}

pub const fn default_dedup_cache_cleanup_interval_secs() -> u64 {
    30
}

pub const fn default_membership_snapshot_interval_secs() -> u64 {
    30
}

// =============================================================================
// Relay Type Defaults
// =============================================================================

pub fn default_relay_type() -> String {
    "matchbox".to_string() // Default to matchbox WebRTC signaling
}

// =============================================================================
// WebSocket Defaults
// =============================================================================

pub const fn default_enable_batching() -> bool {
    true // Enable batching by default for better performance
}

pub const fn default_batch_size() -> usize {
    10 // Max 10 messages per batch
}

pub const fn default_batch_interval_ms() -> u64 {
    16 // One frame at 60fps for minimal latency
}

pub const fn default_auth_timeout_secs() -> u64 {
    10 // Default auth timeout: 10 seconds
}

/// Default post-authentication idle timeout (5 minutes; `0` disables).
///
/// Conservative by construction: the existing server-side activity reaper
/// (`server.ping_timeout`, default 30s) already unregisters any client that
/// stays silent (no `Ping`) for ~30s, so every healthy client heartbeats far
/// more often than this socket-level timeout. 300s therefore only closes
/// zombie sockets whose server-side state was already reaped.
pub const fn default_idle_timeout_secs() -> u64 {
    300
}

/// Default cadence for server-initiated RFC 6455 Ping frames.
pub const fn default_server_ping_interval_secs() -> u64 {
    10
}

/// Default deadline for the matching RFC 6455 Pong frame.
pub const fn default_pong_timeout_secs() -> u64 {
    5
}

/// Default per-connection outbound queue capacity (messages).
///
/// Sized to absorb realistic relay bursts (e.g. rollback-netcode input
/// catch-up at 60 Hz) without engaging backpressure: 1024 messages is ~17
/// seconds of headroom at 60 messages/second from a sending peer. Queue slots
/// hold `Arc` pointers, so a generous default costs almost nothing until
/// messages actually queue.
pub const fn default_send_queue_capacity() -> usize {
    1024
}

/// Default per-connection control-plane queue capacity (messages).
///
/// Control traffic is small and drained before data traffic. A dedicated
/// 128-slot lane prevents Pong, errors, and lifecycle messages from waiting
/// behind a saturated game-data queue without allowing unbounded buffering.
pub const fn default_control_queue_capacity() -> usize {
    128
}

/// Default slow-consumer disconnect timeout in milliseconds.
///
/// When a recipient's outbound queue is full, delivery waits up to this long
/// for capacity before the recipient is disconnected as a slow consumer.
/// Messages are never silently dropped: each is delivered, or the
/// unresponsive connection is closed loudly (metric + log + best-effort error
/// frame). 5s tolerates transient stalls (GC pauses, WiFi hiccups) on top of
/// the buffering the queue itself provides, while bounding how long one dead
/// connection can stall senders in the same room.
pub const fn default_slow_consumer_timeout_ms() -> u64 {
    5_000
}

/// Default maximum outbound queue sojourn time in milliseconds.
///
/// The 15-second default bounds stale queued state while leaving ample room for
/// transient client and network stalls.
pub const fn default_max_sojourn_ms() -> u64 {
    15_000
}

/// Default per-connection `RelayStats` emission interval in seconds
/// (`0` disables — the default posture).
///
/// The frame is protocol v3-only and purely diagnostic, so it is opt-in:
/// deployments that want delivery attribution (pairing `GameData.seq` gap
/// detection with `dropped_for_you` / `backpressure_events`) enable it
/// explicitly. Validated to at most 3600 (one hour) — a longer period is
/// indistinguishable from disabled and almost certainly a misconfiguration.
pub const fn default_delivery_stats_interval_secs() -> u64 {
    0
}
