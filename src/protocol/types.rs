use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Default constants for validation (can be overridden by config)
/// These are used when no config is available
#[allow(dead_code)]
pub const DEFAULT_MAX_GAME_NAME_LENGTH: usize = 64;
#[allow(dead_code)]
pub const DEFAULT_ROOM_CODE_LENGTH: usize = 6;
#[allow(dead_code)]
pub const DEFAULT_MAX_PLAYER_NAME_LENGTH: usize = 32;
#[allow(dead_code)]
pub const DEFAULT_MAX_PLAYERS_LIMIT: u8 = 100;
/// Default deployment region identifier when one is not configured.
pub const DEFAULT_REGION_ID: &str = "default";

/// Unique identifier for players
pub type PlayerId = Uuid;
/// Unique identifier for rooms
pub type RoomId = Uuid;

/// Relay transport protocol selection
#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Default,
    Archive,
    RkyvSerialize,
    RkyvDeserialize,
)]
#[rkyv(compare(PartialEq))]
#[serde(rename_all = "lowercase")]
pub enum RelayTransport {
    /// TCP transport (reliable, ordered delivery)
    /// Recommended for: Turn-based games, lobby systems, RPGs
    Tcp,
    /// UDP transport (low-latency, unreliable)
    /// Recommended for: FPS, racing games, real-time action
    Udp,
    /// WebSocket transport (reliable, browser-compatible)
    /// Recommended for: WebGL builds, browser games, cross-platform
    Websocket,
    /// Automatic selection based on room size and game type
    /// Default: UDP for 2-4 players, TCP for 5+ players, WebSocket for browser builds
    #[default]
    Auto,
}

/// Encoding format for sequenced game data payloads.
#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Default,
    Archive,
    RkyvSerialize,
    RkyvDeserialize,
)]
#[rkyv(compare(PartialEq))]
#[serde(rename_all = "snake_case")]
pub enum GameDataEncoding {
    /// JSON payloads delivered over text frames.
    #[default]
    Json,
    /// MessagePack payloads delivered over binary frames.
    #[serde(rename = "message_pack")]
    MessagePack,
    /// Rkyv zero-copy binary format for maximum performance.
    /// Recommended for: High-frequency updates, large player counts, latency-sensitive games.
    #[serde(rename = "rkyv")]
    Rkyv,
}

/// Connection information for P2P establishment
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ConnectionInfo {
    /// Direct IP:port connection (for Mirror, FishNet, Unity NetCode direct)
    #[serde(rename = "direct")]
    Direct { host: String, port: u16 },
    /// Unity Relay allocation (for Unity NetCode via Unity Relay)
    #[serde(rename = "unity_relay")]
    UnityRelay {
        allocation_id: String,
        connection_data: String,
        key: String,
    },
    /// Built-in relay server (for Unity NetCode, FishNet, Mirror)
    #[serde(rename = "relay")]
    Relay {
        /// Relay server host
        host: String,
        /// Relay server port (TCP or UDP depending on transport)
        port: u16,
        /// Transport protocol (TCP, UDP, or Auto)
        #[serde(default)]
        transport: RelayTransport,
        /// Allocation ID (room ID)
        allocation_id: String,
        /// Client authentication token (opaque server-issued value)
        token: String,
        /// Assigned client ID (set by server after connection)
        #[serde(skip_serializing_if = "Option::is_none")]
        client_id: Option<u16>,
    },
    /// WebRTC connection info (for matchbox relay)
    #[serde(rename = "webrtc")]
    WebRTC {
        sdp: Option<String>,
        ice_candidates: Vec<String>,
    },
    /// Custom connection data (extensible for other types)
    #[serde(rename = "custom")]
    Custom { data: serde_json::Value },
}

/// Information about a player in a room
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerInfo {
    pub id: PlayerId,
    pub name: String,
    pub is_authority: bool,
    pub is_ready: bool,
    pub connected_at: chrono::DateTime<chrono::Utc>,
    /// Connection info for P2P establishment (provided when player is ready)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_info: Option<ConnectionInfo>,
    /// Deployment region that currently hosts this player (internal only).
    #[serde(skip_serializing, skip_deserializing, default)]
    pub region_id: String,
}

/// Information about a spectator watching a room
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectatorInfo {
    pub id: PlayerId,
    pub name: String,
    pub connected_at: chrono::DateTime<chrono::Utc>,
}

/// Describes why a spectator state change occurred.
#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Default,
    Archive,
    RkyvSerialize,
    RkyvDeserialize,
)]
#[rkyv(compare(PartialEq))]
#[serde(rename_all = "snake_case")]
pub enum SpectatorStateChangeReason {
    #[default]
    Joined,
    VoluntaryLeave,
    Disconnected,
    Removed,
    RoomClosed,
}

/// Peer connection information for game start
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerConnectionInfo {
    pub player_id: PlayerId,
    pub player_name: String,
    pub is_authority: bool,
    pub relay_type: String,
    /// Connection info provided by the peer for P2P establishment
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_info: Option<ConnectionInfo>,
}

/// Rate limit information for an application
#[derive(Debug, Clone, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize)]
pub struct RateLimitInfo {
    /// Requests allowed per minute
    pub per_minute: u32,
    /// Requests allowed per hour
    pub per_hour: u32,
    /// Requests allowed per day
    pub per_day: u32,
}

/// Describes negotiated protocol capabilities for a specific SDK.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolInfoPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended_version: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default)]
    pub game_data_formats: Vec<GameDataEncoding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player_name_rules: Option<PlayerNameRulesPayload>,
}

/// Describes the characters your deployment allows inside `player_name`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerNameRulesPayload {
    pub max_length: usize,
    pub min_length: usize,
    pub allow_unicode_alphanumeric: bool,
    pub allow_spaces: bool,
    pub allow_leading_trailing_whitespace: bool,
    #[serde(default)]
    pub allowed_symbols: Vec<char>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_allowed_characters: Option<String>,
}

impl PlayerNameRulesPayload {
    pub fn from_protocol_config(config: &crate::config::ProtocolConfig) -> Self {
        let rules = &config.player_name_validation;
        Self {
            max_length: config.max_player_name_length,
            min_length: 1,
            allow_unicode_alphanumeric: rules.allow_unicode_alphanumeric,
            allow_spaces: rules.allow_spaces,
            allow_leading_trailing_whitespace: rules.allow_leading_trailing_whitespace,
            allowed_symbols: rules.allowed_symbols.clone(),
            additional_allowed_characters: rules.additional_allowed_characters.clone(),
        }
    }
}
