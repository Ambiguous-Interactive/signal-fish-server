use bytes::Bytes;
use serde::{Deserialize, Serialize};

use super::error_codes::ErrorCode;
use super::room_state::LobbyState;
use super::types::{
    ConnectionInfo, GameDataEncoding, IceServer, PeerConnectionInfo, PlayerId, PlayerInfo,
    ProtocolInfoPayload, RateLimitInfo, RelayTransport, RoomId, SessionPlanPayload, SpectatorInfo,
    SpectatorStateChangeReason, Topology, Transport,
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
        /// Highest protocol version the client speaks.
        ///
        /// When absent, the endpoint default is used (`/v2/ws` => v2,
        /// `/v3/ws` => v3), then clamped by server protocol configuration.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protocol_version: Option<u16>,
        /// Data-path transports the client supports.
        ///
        /// Absent means a relay-only capability set, even if `/v3/ws` defaulted
        /// the omitted `protocol_version` to v3.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        supported_transports: Option<Vec<Transport>>,
        /// Session topologies the client supports.
        ///
        /// Absent means a relay-only capability set, even if `/v3/ws` defaulted
        /// the omitted `protocol_version` to v3.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        supported_topologies: Option<Vec<Topology>>,
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
    /// Relay an opaque WebRTC signal to a specific peer in the same room (v3 only).
    ///
    /// The `signal` payload is never parsed by the server — it is forwarded
    /// verbatim. By convention it is matchbox-compatible, i.e. one of
    /// `{"Offer":"..."}`, `{"Answer":"..."}`, or `{"IceCandidate":"..."}`.
    Signal {
        to: PlayerId,
        signal: serde_json::Value,
    },
    /// Request to become or connect to authoritative server
    AuthorityRequest { become_authority: bool },
    /// Toggle this player's readiness in the lobby.
    ///
    /// Readiness can be toggled at any time while the room is open (not yet
    /// `Finalized`); the room need not be full. The server broadcasts
    /// `LobbyStateChanged` after each toggle, with `all_ready` set once every
    /// *current* player is ready. Readiness alone no longer starts the game — an
    /// explicit [`ClientMessage::StartGame`] is required (see its docs).
    PlayerReady,
    /// Explicitly start the game, finalizing the lobby with its *current*
    /// members (`max_players` is a ceiling, not a required count).
    ///
    /// Accepted only when **every current player is ready** (`all_ready`). The
    /// sender must be permitted to start: if the room has a designated authority
    /// player, only that authority may start; if no authority is set, **any**
    /// player in the room may start. On success the server transitions the room
    /// to `Finalized` and broadcasts `GameStarting` (and, for a negotiated v3
    /// non-relay room, the per-recipient `SessionPlan`). A room with a single
    /// ready player may start (solo is allowed).
    StartGame,
    /// Provide legacy, self-declared v2/back-compat connection metadata.
    ///
    /// Stored for `GameStarting.peer_connections[*].connection_info`; not used
    /// to negotiate v3 transport capability and not proof of P2P reachability.
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
    /// Report this client's current data-path transport state to the server (v3 only).
    /// Lets the server distinguish P2P-connected peers from relay-fallback peers
    /// (drives metrics and, in future, targeted relay for stuck peers). Purely
    /// informational — the relay floor never closes regardless of what is reported.
    TransportStatus {
        transport: Transport,
        connected: bool,
    },
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
    /// ICE (STUN/TURN) servers for early candidate gathering during the lobby
    /// wait (v3 only — PLAN §P4's deferred "RoomJoined ICE pre-gather"
    /// refinement). Populated only under the pre-gather gate
    /// (`session.enable_ice_pregather` + WebRTC enabled + non-relay desired
    /// topology + non-finalized room + a v3 recipient that negotiated the
    /// WebRTC transport and whose negotiated topologies contain the game's
    /// desired topology); empty — and absent from the wire via
    /// `skip_serializing_if`, keeping the v2 JSON and MessagePack bytes
    /// identical — otherwise. The `SessionPlan` ICE list supersedes this one:
    /// clients should apply the most recent set, because pre-gather TURN
    /// credentials may expire during a long lobby and fresh ones always arrive
    /// in the `SessionPlan`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ice_servers: Vec<IceServer>,
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
    /// ICE (STUN/TURN) servers for early candidate gathering during the lobby
    /// wait (v3 only — PLAN §P4's deferred "RoomJoined ICE pre-gather"
    /// refinement). Populated only under the pre-gather gate
    /// (`session.enable_ice_pregather` + WebRTC enabled + non-relay desired
    /// topology + non-finalized room + a v3 recipient that negotiated the
    /// WebRTC transport and whose negotiated topologies contain the game's
    /// desired topology); empty — and absent from the wire via
    /// `skip_serializing_if`, keeping the v2 JSON and MessagePack bytes
    /// identical — otherwise. The `SessionPlan` ICE list supersedes this one:
    /// clients should apply the most recent set, because pre-gather TURN
    /// credentials may expire during a long lobby and fresh ones always arrive
    /// in the `SessionPlan` (a reconnect into an active session gets its fresh
    /// ICE from the late-join `SessionPlan`, never from here).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ice_servers: Vec<IceServer>,
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
    /// Binary game data payload from another player.
    ///
    /// This is an in-memory broadcast carrier. Negotiated MessagePack clients
    /// receive a bare binary WebSocket frame from `websocket::sending`, not this
    /// enum variant serialized through the `{type, data}` envelope. Uses `Bytes`
    /// for zero-copy cloning during broadcast.
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
    /// Game is starting with legacy peer metadata.
    ///
    /// `peer_connections` may include self-declared `ConnectionInfo` from
    /// `ProvideConnectionInfo`. v3 topology/transport/ICE/fallback directives
    /// are carried by [`ServerMessage::SessionPlan`], not this message.
    GameStarting {
        peer_connections: Vec<PeerConnectionInfo>,
    },
    /// Relayed opaque WebRTC signal from another peer in the same room (v3 only).
    ///
    /// The `signal` payload is forwarded verbatim from the sender's
    /// [`ClientMessage::Signal`] and is never inspected by the server. By
    /// convention it is matchbox-compatible (`{"Offer":"..."}` |
    /// `{"Answer":"..."}` | `{"IceCandidate":"..."}`).
    Signal {
        from: PlayerId,
        signal: serde_json::Value,
    },
    /// A new peer is available for a WebRTC peer connection (v3 only).
    ///
    /// `you_initiate` designates exactly one side of each pair as the offerer,
    /// avoiding glare. In `mesh` topology the recipient initiates iff its id is
    /// the lesser of the two UUIDs (Appendix E glare rule); in `host` topology
    /// the direction is fixed — the client initiates to the host and the host
    /// answers, regardless of UUID order.
    NewPeer {
        peer_id: PlayerId,
        you_initiate: bool,
    },
    /// Per-recipient session directive emitted at lobby finalization (v3 only).
    ///
    /// Sent alongside the unchanged [`ServerMessage::GameStarting`] (and only to
    /// v3-capable members) when a room negotiates a non-relay plan. It carries
    /// the chosen topology/transport, the host (for `host` topology), the
    /// recipient's peer list with per-recipient `initiate` flags, ICE servers,
    /// and the relay `fallback`. A relay-only room emits no `SessionPlan`, so v2
    /// clients never observe it (boxed to keep the enum small, mirroring
    /// [`ServerMessage::RoomJoined`]).
    SessionPlan(Box<SessionPlanPayload>),
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
    /// A same-room peer's reported data-path transport state changed (v3 only).
    ///
    /// Fan-out of an accepted [`ClientMessage::TransportStatus`]: when a v3
    /// client's report is recorded as a real per-connection state change (the
    /// first report, or a `(transport, connected)` transition — duplicates are
    /// dropped at the handler), every **other** member of its current room that
    /// negotiated v3 is told the new state, e.g. "the host's WebRTC path died,
    /// expect relay-path traffic from it". Delivery is gated on the recipient's
    /// negotiated protocol version only — deliberately NOT on the recipient's
    /// own transport capabilities, because this is informational status about a
    /// peer, not an instruction to use that transport. Like the report itself it
    /// is purely informational: the relay floor never closes.
    ///
    /// NOTE: appended at the END of the enum (after `Error`) so any future
    /// positional/discriminant-sensitive encoding (e.g. the rkyv path sketched
    /// in `src/broadcast.rs`) keeps prior discriminants stable. The current wire
    /// encodings (`serde_json`, `rmp_serde::to_vec_named`) are name-based.
    PeerTransportStatus {
        peer_id: PlayerId,
        transport: Transport,
        connected: bool,
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
