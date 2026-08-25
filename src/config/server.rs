//! Server behavior configuration types.

use super::defaults::{
    default_drain_grace_secs, default_empty_room_timeout, default_enable_reconnection,
    default_event_buffer_size, default_heartbeat_throttle_secs, default_inactive_room_timeout,
    default_max_join_attempts, default_max_players, default_max_room_creations,
    default_max_rooms_per_game, default_max_signal_errors, default_max_signals,
    default_ping_timeout, default_rate_limit_time_window, default_reconnection_window,
    default_region_id, default_room_cleanup_interval,
};
use serde::{Deserialize, Serialize};

/// Operational ceiling for a room's reconnection replay ring.
pub const MAX_EVENT_BUFFER_SIZE: usize = 65_536;

/// Server configuration for room and player management.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ServerConfig {
    /// Default maximum players per room
    #[serde(default = "default_max_players")]
    pub default_max_players: u8,
    /// Timeout for client ping responses (seconds)
    #[serde(default = "default_ping_timeout")]
    pub ping_timeout: u64,
    /// Interval for room cleanup task (seconds)
    #[serde(default = "default_room_cleanup_interval")]
    pub room_cleanup_interval: u64,
    /// Grace period between shutdown drain start and forced close 4000 (seconds).
    /// Set to 0 to retain legacy immediate shutdown behavior.
    #[serde(default = "default_drain_grace_secs")]
    pub drain_grace_secs: u64,
    /// Maximum number of rooms per game. Must be `> 0`: a zero cap rejects
    /// every room creation (startup validation rejects it).
    #[serde(default = "default_max_rooms_per_game")]
    pub max_rooms_per_game: usize,
    /// Time after creation when empty rooms expire (seconds)
    #[serde(default = "default_empty_room_timeout")]
    pub empty_room_timeout: u64,
    /// Time after last activity when rooms with players or spectators expire (seconds)
    #[serde(default = "default_inactive_room_timeout")]
    pub inactive_room_timeout: u64,
    /// Time window for reconnection after disconnection (seconds)
    #[serde(default = "default_reconnection_window")]
    pub reconnection_window: u64,
    /// Number of events to buffer per room for reconnection
    #[serde(default = "default_event_buffer_size")]
    pub event_buffer_size: usize,
    /// Enable player reconnection after disconnection
    #[serde(default = "default_enable_reconnection")]
    pub enable_reconnection: bool,
    /// Threshold for heartbeat throttling (seconds).
    /// Controls how frequently heartbeat timestamps are recorded.
    /// Set to 0 to disable throttling (update on every heartbeat).
    #[serde(
        default = "default_heartbeat_throttle_secs",
        alias = "heartbeat_db_throttle_secs"
    )]
    pub heartbeat_throttle_secs: u64,
    /// Identifier for the deployment region (used in player info and room codes).
    #[serde(default = "default_region_id")]
    pub region_id: String,
    /// Optional prefix prepended to generated room codes.
    #[serde(default)]
    pub room_code_prefix: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            default_max_players: default_max_players(),
            ping_timeout: default_ping_timeout(),
            room_cleanup_interval: default_room_cleanup_interval(),
            drain_grace_secs: default_drain_grace_secs(),
            max_rooms_per_game: default_max_rooms_per_game(),
            empty_room_timeout: default_empty_room_timeout(),
            inactive_room_timeout: default_inactive_room_timeout(),
            reconnection_window: default_reconnection_window(),
            event_buffer_size: default_event_buffer_size(),
            enable_reconnection: default_enable_reconnection(),
            heartbeat_throttle_secs: default_heartbeat_throttle_secs(),
            region_id: default_region_id(),
            room_code_prefix: None,
        }
    }
}

/// Rate limiting configuration.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RateLimitConfig {
    /// Maximum number of room creation requests per time window. Must be
    /// `> 0`: a zero budget rejects every creation (startup validation rejects it).
    #[serde(default = "default_max_room_creations")]
    pub max_room_creations: u32,
    /// Time window for rate limiting (seconds)
    #[serde(default = "default_rate_limit_time_window")]
    pub time_window: u64,
    /// Shared maximum room-creation, seated-join, and spectator-join attempts
    /// per time window. Must be `> 0`: a zero budget rejects every attempt
    /// (startup validation rejects it).
    #[serde(default = "default_max_join_attempts")]
    pub max_join_attempts: u32,
    /// Maximum number of WebRTC signaling messages per time window. Must be
    /// `> 0`: a zero budget rejects every Signal (startup validation rejects it).
    #[serde(default = "default_max_signals")]
    pub max_signals: u32,
    /// Detailed rejected-signal responses before generic rate-limit errors.
    #[serde(default = "default_max_signal_errors")]
    pub max_signal_errors: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_room_creations: default_max_room_creations(),
            time_window: default_rate_limit_time_window(),
            max_join_attempts: default_max_join_attempts(),
            max_signals: default_max_signals(),
            max_signal_errors: default_max_signal_errors(),
        }
    }
}
