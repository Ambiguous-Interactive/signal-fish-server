//! Default value functions for configuration fields.
//!
//! This module contains all the default value functions used by serde's `#[serde(default = ...)]`
//! attributes throughout the configuration system. Functions are organized by category for
//! easier maintenance.

use super::logging::LogFormat;
use super::security::ClientAuthMode;

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

pub const fn default_sdk_enforce() -> bool {
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

pub const fn default_max_connections_per_ip() -> usize {
    10
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum DashboardHistoryField {
    ActiveRooms,
    RoomsByGame,
    PlayerPercentiles,
    GamePercentiles,
    ActiveConnections,
    RoomsCreated,
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
