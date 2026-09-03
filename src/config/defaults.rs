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

/// Default per-sender game-data relay byte budget per `rate_limit.time_window`
/// (256 MiB per 60-second window, about 4.5 MB/s sustained).
///
/// The relayed game-data fan-out previously had no sender-side budget at all
/// (issue #519): one player could stream 64 KiB frames to a full roster at
/// line rate on the operator's egress bill. The default sits comfortably
/// above demonstrated real sessions (a full roster relaying 64 KiB at 30 Hz
/// submits ~2 MB/s inbound) while bounding a flooding sender to a fixed,
/// metered share. Fixed-window accounting shares the documented 2x
/// boundary-burst trade-off of the other budgets.
pub const fn default_max_relay_bytes() -> u64 {
    256 * 1024 * 1024 // 256 MiB per window
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
    true // Enforce the app-ID allowlist by default; opt out only for explicit development scenarios
}

pub const fn default_max_message_size() -> usize {
    65536 // 64KB
}

/// Default maximum aggregate application payload for one server-to-client
/// WebSocket message. The limit is intentionally independent of the inbound
/// cap because snapshots and replay messages aggregate multiple accepted
/// client values.
pub const fn default_max_outbound_message_size() -> usize {
    8 * 1024 * 1024 // 8 MiB
}

/// Largest configurable outbound WebSocket application payload.
///
/// Keeping the deployment value at or below 64 MiB gives JavaScript and
/// 32-bit clients one exact, portable integer range while also bounding the
/// server's fallible materialization buffer.
pub const MAX_OUTBOUND_MESSAGE_SIZE: usize = 64 * 1024 * 1024;

/// Minimum headroom the outbound cap must keep above the inbound frame cap.
///
/// An admitted inbound frame is re-emitted to roommates with the relay
/// envelope attached, so its relayed form is strictly larger than what the
/// inbound cap admitted, by at least the fixed envelope. Bounded per carrier:
///
/// - JSON `GameData`: `"from_player":"<hyphenated-uuid>"` (~54 bytes) plus
///   v3 delivery stamps (`"seq":<u64>,"epoch":<u32>`, ~40 bytes max) and
///   envelope punctuation.
/// - Binary `GameData`: the fixed MessagePack envelope (16-byte sender id,
///   encoding, delivery stamps, framing header, ~50 bytes).
/// - Relayed `Signal`: the sender id field, ~70 bytes with envelope.
///
/// Worst fixed-envelope case is ~150 bytes; 256 keeps roughly 1.7x margin.
///
/// This headroom covers only the fixed envelope. Value-level
/// re-serialization can still grow a frame beyond it — JSON number
/// normalization (`9e9` becomes `9000000000.0`) and the MessagePack→JSON
/// fallback's escaping being the known cases — and such a frame remains
/// enforced fail-closed at write time (`1009 outbound_message_too_large`).
/// The headroom exists so that ordinary, envelope-only growth can never
/// close a recipient; deployments that relay adversarial payloads should
/// keep a proportionally larger gap.
pub const RELAY_ENVELOPE_HEADROOM_BYTES: usize = 256;

/// Default cap on the serialized size of a v3 `Signal` payload (16 KiB):
/// generously above any real SDP/ICE payload, well under the 64 KiB
/// `max_message_size` frame cap.
pub const fn default_max_signal_bytes() -> usize {
    16384 // 16KB
}

/// Default cap on the serialized size of one self-declared peer-metadata
/// entry (`ProvideConnectionInfo`, 8 KiB).
///
/// The entry is stored and broadcast verbatim to every room member (in
/// `GameStarting.peer_connections` and room snapshots), so an unbounded entry
/// is a peer-eviction primitive: a full roster of oversized entries can push
/// the aggregate payload past `security.max_outbound_message_size`, closing
/// every recipient with `1009 outbound_message_too_large` (issue #524). The
/// default keeps the worst-case aggregate at default ceilings tiny:
/// 8 KiB x `protocol.max_players_limit` (100) = 800 KiB, a fraction of the
/// 8 MiB outbound cap.
pub const fn default_max_connection_info_bytes() -> usize {
    8192 // 8KB
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

/// Default server-wide concurrent-connection ceiling (10,000).
///
/// The per-IP cap bounds one source; total ownership is still multiplied by
/// the number of distinct source IPs (a botnet or a large NAT pool can
/// multiply it, issue #502 item 2). 10,000 concurrent connections is
/// generous headroom above any demonstrated single-node session while
/// bounding worst-case per-connection queue memory; operators serving more
/// should raise it deliberately with capacity evidence.
pub const fn default_max_connections() -> usize {
    10_000
}

pub const fn default_client_auth_mode() -> ClientAuthMode {
    ClientAuthMode::None
}

pub fn default_token_binding_subprotocol() -> String {
    crate::security::token_binding::TOKEN_BINDING_SUBPROTOCOL_V2.to_string()
}

// =============================================================================
// Metrics Defaults
// =============================================================================

/// Hard memory bound on the dashboard metrics history: at most this many
/// samples are retained regardless of the configured history window.
pub const DASHBOARD_CACHE_HISTORY_MAX_SAMPLES: usize = 720;

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

pub const fn default_membership_snapshot_interval_secs() -> u64 {
    30
}

// =============================================================================
// Relay Type Defaults
// =============================================================================

pub fn default_relay_type() -> String {
    "matchbox".to_string() // Legacy integration label; not a transport selector.
}

// =============================================================================
// WebSocket Defaults
// =============================================================================

pub const fn default_enable_batching() -> bool {
    // Latency-first default. Batching holds a latency-sensitive frame for up to
    // `batch_interval_ms`, adding a per-hop delay to real-time relay traffic
    // (issue #198). Opt in (`SIGNAL_FISH__WEBSOCKET__ENABLE_BATCHING=true`) for
    // bulk/throughput deployments that prefer fewer, larger writes.
    false
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

/// Default post-handshake idle timeout (5 minutes; `0` disables).
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

/// Default requested TCP send-buffer size for accepted HTTP/WebSocket sockets.
///
/// Keeping the kernel handoff bounded prevents megabytes of already-accepted
/// game data from sitting ahead of a WebSocket Ping or delivery report. Linux
/// reports approximately twice this value because `SO_SNDBUF` includes kernel
/// bookkeeping; 64 KiB therefore leaves about four seconds of data ahead of
/// control traffic on a 256-kbps downstream, below the default 5-second Pong
/// deadline while preserving more bandwidth-delay-product headroom.
pub const fn default_socket_send_buffer_bytes() -> u32 {
    64 * 1_024
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

/// Default reliable/control queue sojourn and socket-write progress time.
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
