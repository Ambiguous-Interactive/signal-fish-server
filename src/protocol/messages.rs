use bytes::Bytes;
use serde::{Deserialize, Serialize};

use super::error_codes::ErrorCode;
use super::room_state::LobbyState;
use super::types::{
    ConnectionInfo, GameDataEncoding, PeerConnectionInfo, PlayerId, PlayerInfo,
    ProtocolInfoPayload, RateLimitInfo, RelayTransport, RoomId, SpectatorInfo,
    SpectatorStateChangeReason,
};

/// Message types sent from client to server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ClientMessage {
    /// Authenticate with App ID (MUST be first message)
    /// App ID is a public identifier (not a secret!) that identifies the game application
    Authenticate {
        /// Public App ID (safe to embed in game builds, e.g., "mb_app_abc123...")
        app_id: String,
        /// SDK version for debugging and analytics
        #[serde(skip_serializing_if = "Option::is_none")]
        sdk_version: Option<String>,
        /// Platform information (e.g., "unity", "godot", "unreal")
        #[serde(skip_serializing_if = "Option::is_none")]
        platform: Option<String>,
        /// Preferred game data encoding (defaults to JSON text frames)
        #[serde(skip_serializing_if = "Option::is_none")]
        game_data_format: Option<GameDataEncoding>,
    },
    /// Join or create a room for a specific game
    JoinRoom {
        game_name: String,
        room_code: Option<String>,
        player_name: String,
        max_players: Option<u8>,
        supports_authority: Option<bool>,
        /// Preferred relay transport protocol (TCP, UDP, or Auto)
        /// If not specified, defaults to Auto
        #[serde(default)]
        relay_transport: Option<RelayTransport>,
    },
    /// Leave the current room
    LeaveRoom,
    /// Send game data to other players in the room
    GameData { data: serde_json::Value },
    /// Request to become or connect to authoritative server
    AuthorityRequest { become_authority: bool },
    /// Signal readiness to start the game in lobby
    PlayerReady,
    /// Provide connection info for P2P establishment
    ProvideConnectionInfo { connection_info: ConnectionInfo },
    /// Heartbeat to maintain connection
    Ping,
    /// Reconnect to a room after disconnection
    Reconnect {
        player_id: PlayerId,
        room_id: RoomId,
        /// Authentication token generated on initial join
        auth_token: String,
    },
    /// Join a room as a spectator (read-only observer)
    JoinAsSpectator {
        game_name: String,
        room_code: String,
        spectator_name: String,
    },
    /// Leave spectator mode
    LeaveSpectator,
}

/// Payload for the RoomJoined server message.
/// Boxed in ServerMessage to reduce enum size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomJoinedPayload {
    pub room_id: RoomId,
    pub room_code: String,
    pub player_id: PlayerId,
    pub game_name: String,
    pub max_players: u8,
    pub supports_authority: bool,
    pub current_players: Vec<PlayerInfo>,
    pub is_authority: bool,
    pub lobby_state: LobbyState,
    pub ready_players: Vec<PlayerId>,
    pub relay_type: String,
    /// List of spectators currently watching (if any)
    #[serde(default)]
    pub current_spectators: Vec<SpectatorInfo>,
}

/// Payload for the Reconnected server message.
/// Boxed in ServerMessage to reduce enum size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconnectedPayload {
    pub room_id: RoomId,
    pub room_code: String,
    pub player_id: PlayerId,
    pub game_name: String,
    pub max_players: u8,
    pub supports_authority: bool,
    pub current_players: Vec<PlayerInfo>,
    pub is_authority: bool,
    pub lobby_state: LobbyState,
    pub ready_players: Vec<PlayerId>,
    pub relay_type: String,
    /// List of spectators currently watching (if any)
    #[serde(default)]
    pub current_spectators: Vec<SpectatorInfo>,
    /// Events that occurred while disconnected
    pub missed_events: Vec<ServerMessage>,
}

/// Payload for the SpectatorJoined server message.
/// Boxed in ServerMessage to reduce enum size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectatorJoinedPayload {
    pub room_id: RoomId,
    pub room_code: String,
    pub spectator_id: PlayerId,
    pub game_name: String,
    pub current_players: Vec<PlayerInfo>,
    pub current_spectators: Vec<SpectatorInfo>,
    pub lobby_state: LobbyState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<SpectatorStateChangeReason>,
}

/// Message types sent from server to client
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ServerMessage {
    /// Authentication successful
    Authenticated {
        /// App name for confirmation
        app_name: String,
        /// Organization name (if any)
        #[serde(skip_serializing_if = "Option::is_none")]
        organization: Option<String>,
        /// Rate limits for this app
        rate_limits: RateLimitInfo,
    },
    /// SDK/protocol compatibility details advertised after authentication
    ProtocolInfo(ProtocolInfoPayload),
    /// Authentication failed
    AuthenticationError {
        /// Error message
        error: String,
        /// Error code for programmatic handling
        error_code: ErrorCode,
    },
    /// Successfully joined a room (boxed to reduce enum size)
    RoomJoined(Box<RoomJoinedPayload>),
    /// Failed to join room
    RoomJoinFailed {
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<ErrorCode>,
    },
    /// Successfully left room
    RoomLeft,
    /// Another player joined the room
    PlayerJoined { player: PlayerInfo },
    /// Another player left the room
    PlayerLeft { player_id: PlayerId },
    /// Game data from another player
    GameData {
        from_player: PlayerId,
        data: serde_json::Value,
    },
    /// Binary game data payload from another player
    /// Uses `Bytes` for zero-copy cloning during broadcast
    GameDataBinary {
        from_player: PlayerId,
        encoding: GameDataEncoding,
        #[serde(with = "bytes_serde")]
        payload: Bytes,
    },
    /// Authority status changed
    AuthorityChanged {
        authority_player: Option<PlayerId>,
        you_are_authority: bool,
    },
    /// Authority request response
    AuthorityResponse {
        granted: bool,
        reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<ErrorCode>,
    },
    /// Lobby state changed (room full, player readiness changed, etc.)
    LobbyStateChanged {
        lobby_state: LobbyState,
        ready_players: Vec<PlayerId>,
        all_ready: bool,
    },
    /// Game is starting with peer connection information
    GameStarting {
        peer_connections: Vec<PeerConnectionInfo>,
    },
    /// Pong response to ping
    Pong,
    /// Reconnection successful (boxed to reduce enum size)
    Reconnected(Box<ReconnectedPayload>),
    /// Reconnection failed
    ReconnectionFailed {
        reason: String,
        error_code: ErrorCode,
    },
    /// Another player reconnected to the room
    PlayerReconnected { player_id: PlayerId },
    /// Successfully joined a room as spectator (boxed to reduce enum size)
    SpectatorJoined(Box<SpectatorJoinedPayload>),
    /// Failed to join as spectator
    SpectatorJoinFailed {
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<ErrorCode>,
    },
    /// Successfully left spectator mode
    SpectatorLeft {
        #[serde(skip_serializing_if = "Option::is_none")]
        room_id: Option<RoomId>,
        #[serde(skip_serializing_if = "Option::is_none")]
        room_code: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<SpectatorStateChangeReason>,
        #[serde(default)]
        current_spectators: Vec<SpectatorInfo>,
    },
    /// Another spectator joined the room
    NewSpectatorJoined {
        spectator: SpectatorInfo,
        #[serde(default)]
        current_spectators: Vec<SpectatorInfo>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<SpectatorStateChangeReason>,
    },
    /// Another spectator left the room
    SpectatorDisconnected {
        spectator_id: PlayerId,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<SpectatorStateChangeReason>,
        #[serde(default)]
        current_spectators: Vec<SpectatorInfo>,
    },
    /// Error message
    Error {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<ErrorCode>,
    },
}

/// Custom serde module for `bytes::Bytes` serialization
///
/// This provides efficient serialization that works with both JSON (base64-like)
/// and binary formats (direct bytes).
mod bytes_serde {
    use bytes::Bytes;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(bytes: &Bytes, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Use serde_bytes for efficient byte serialization
        serde_bytes::Bytes::new(bytes.as_ref()).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Bytes, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Deserialize to Vec<u8> then convert to Bytes
        let vec: Vec<u8> = serde_bytes::ByteBuf::deserialize(deserializer)?.into_vec();
        Ok(Bytes::from(vec))
    }
}
