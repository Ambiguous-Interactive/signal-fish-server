use bytes::Bytes;
use serde::{Deserialize, Deserializer, Serialize};

use super::delivery::{DeliveryClass, DeliveryReportPayload};
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
    /// Send game data to other players in the room.
    GameData {
        data: serde_json::Value,
        /// Protocol-v3 delivery policy. Omission preserves the reliable v2
        /// contract; `latest` requires `key`, while the other classes forbid it.
        #[serde(
            default,
            deserialize_with = "deserialize_present_optional",
            skip_serializing_if = "Option::is_none"
        )]
        class: Option<DeliveryClass>,
        /// Sender-defined coalescing key, present exactly for `class: latest`.
        #[serde(
            default,
            deserialize_with = "deserialize_present_optional",
            skip_serializing_if = "Option::is_none"
        )]
        key: Option<u32>,
    },
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
    /// to `Finalized` and broadcasts `GameStarting`. Every negotiated-v3 member
    /// then receives a per-recipient `SessionPlan`, including an explicit
    /// `relay`/`relay` plan with no peers when the room stays on the floor. A
    /// room with a single ready player may start (solo is allowed).
    StartGame,
    /// Provide legacy, self-declared v2/back-compat connection metadata.
    ///
    /// Stored for `GameStarting.peer_connections[*].connection_info`. A usable
    /// Direct endpoint may make an otherwise capability-compatible v3
    /// `host + direct` plan executable and is then repeated in `SessionPlan`;
    /// it does not negotiate capability or prove P2P reachability.
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
    /// wait (v3 only — the deferred "RoomJoined ICE pre-gather"
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
    /// Reconnection token for THIS room, minted at join (v3+ recipients only;
    /// absent on the v2 wire via `skip_serializing_if`). Present the token in
    /// a later `Reconnect` after an unexpected disconnect. The token string
    /// is stable from join through the disconnect, but it only becomes
    /// claimable for `server.reconnection_window` seconds counted from the
    /// DISCONNECT — holding it early does not widen the window. Rotated on
    /// every join and on every successful reconnect (see
    /// `ReconnectedPayload::reconnection_token`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconnection_token: Option<String>,
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
    /// wait (v3 only — the deferred "RoomJoined ICE pre-gather"
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
    /// Completeness of `missed_events` (v3+ only). Populated only for a
    /// recipient that negotiated protocol v3+; `None` — and absent from the
    /// wire via `skip_serializing_if`, keeping the v2 JSON and MessagePack
    /// bytes identical — otherwise. See [`ReplayStatus`] for the contract each
    /// value places on the client (a truncated/unavailable replay requires a
    /// resync from this payload's snapshot fields).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<ReplayStatus>,
    /// Authoritative per-sender relay baseline for this room (v3+ only).
    ///
    /// A reconnecting client never receives missed `GameData`; it resyncs from
    /// the room snapshot. These watermarks tell it the current `(epoch, seq)`
    /// tail for every current room member so any post-reconnect gap can be
    /// attributed to its own absence or replay truncation, not silent relay
    /// loss. Empty — and absent from the wire — for pre-v3 recipients.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sender_watermarks: Vec<SenderWatermark>,
    /// Fresh (rotated) reconnection token for this room, replacing the one
    /// just used (v3+ recipients only; absent on the v2 wire via
    /// `skip_serializing_if`). Store it for the NEXT unexpected disconnect —
    /// the previous token was consumed by this reconnect and is no longer
    /// claimable. Same window semantics as
    /// `RoomJoinedPayload::reconnection_token`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconnection_token: Option<String>,
}

/// A v3 reconnect baseline for one current room member's relayed game-data
/// stream.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct SenderWatermark {
    pub player_id: PlayerId,
    pub epoch: u32,
    pub seq: u64,
}

/// Completeness of `Reconnected.missed_events` (v3+ recipients only; the field
/// is absent on the v2 wire).
///
/// Only room-uniform control events (`PlayerJoined`, `PlayerLeft`,
/// `PlayerReconnected`, `NewSpectatorJoined`, `SpectatorDisconnected`,
/// `LobbyStateChanged`, `AuthorityChanged`) are ever replayed; GameData,
/// `Signal`, and the per-recipient `GameStarting` never are — reconnectors
/// resync from the `Reconnected` snapshot and, for started sessions, the
/// late-join `SessionPlan` flow.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplayStatus {
    /// Every replayable control event since disconnect is in `missed_events`.
    Complete,
    /// Events were evicted from the bounded replay ring; `missed_events` is a
    /// suffix. Resync from the `Reconnected` snapshot fields.
    Truncated,
    /// Event replay is not active on this deployment (`event_buffer_size` 0);
    /// treat reconnection as a full resync from the snapshot.
    Unavailable,
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
    /// Another player left the room. Protocol-v3 recipients also receive the
    /// terminal relay watermark for the departed player's final incarnation;
    /// these fields are stripped from the frozen v2 wire representation.
    PlayerLeft {
        player_id: PlayerId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        epoch: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_seq: Option<u64>,
    },
    /// Game data from another player
    GameData {
        from_player: PlayerId,
        data: serde_json::Value,
        /// Server-stamped relay sequence number (v3 only). Per-(sender, room),
        /// it starts at 1 and is strictly increasing within an `epoch`.
        /// Lossy delivery classes may create gaps on one continuing recipient;
        /// each such gap is legal only after an exact, causally prior
        /// [`ServerMessage::DeliveryReport`] range for the same sender and
        /// epoch. Aggregate counters and supplemental `Error` frames are not
        /// gap authorization. A loud close instead terminates this physical
        /// connection's observable stream.
        ///
        /// The counter restarts at 1 in a new incarnation, identified directly
        /// by the accompanying `epoch`. Stamped at relay time in
        /// `server::game_data`; stripped per recipient in `websocket::sending`
        /// for pre-v3 recipients, keeping their bytes byte-identical.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        /// Server-tracked incarnation epoch (v3 only), stamped beside `seq`. It
        /// increments once per `(sender, room)` incarnation — a join-after-leave
        /// or a reconnect — and `seq` restarts at 1 within each epoch, so
        /// `(epoch, seq)` is strictly lexicographically increasing across data
        /// frames from one sender on one physical recipient connection. Priority
        /// lifecycle control for a newer epoch may overtake already-queued data
        /// from the prior epoch; clients tolerate that trailing data until the
        /// first newer-epoch data frame, while suppressing it from application
        /// state after the lifecycle announcement. This makes the `seq` restart
        /// self-describing instead of inferred from separately ordered control.
        /// Gated exactly like `seq`: present only for v3 recipients and absent
        /// (bytes byte-identical to pre-v3) below.
        /// Precedent: Aeron image sessionId, Kafka producer epoch (KIP-98).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        epoch: Option<u32>,
        /// Echoed protocol-v3 delivery class when the sender supplied one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        class: Option<DeliveryClass>,
        /// Echoed sender-defined coalescing key for `class: latest`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<u32>,
    },
    /// Binary game data payload from another player.
    ///
    /// This is an in-memory broadcast carrier. Every v3 binary recipient
    /// receives a MessagePack metadata envelope from `websocket::sending`, not
    /// this enum variant serialized through the `{type, data}` envelope. The
    /// envelope leaves `payload` opaque and uses `encoding` only to tag those
    /// bytes. Uses `Bytes` for zero-copy cloning during broadcast.
    GameDataBinary {
        from_player: PlayerId,
        encoding: GameDataEncoding,
        #[serde(with = "bytes_serde")]
        payload: Bytes,
        /// Server-stamped relay sequence number (v3 only): the same counter,
        /// semantics, and per-recipient gating as [`ServerMessage::GameData::seq`]
        /// — text and binary relay share one per-(sender, room) stream. This
        /// in-memory field is optional for v2 recipient projection; on a v3
        /// binary wire frame, `V3BinaryGameDataFrame` (see
        /// `websocket::sending`) carries `seq` as a mandatory non-zero key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        /// Server-tracked incarnation epoch (v3 only): the same per-(sender,
        /// room) counter, semantics, and per-recipient gating as
        /// [`ServerMessage::GameData::epoch`] — text and binary relay share the
        /// one epoch on the sender's `ClientConnection`. This in-memory field is
        /// optional for v2 recipient projection; on a v3 binary wire frame,
        /// `V3BinaryGameDataFrame` carries `epoch` beside `seq` as a mandatory
        /// non-zero key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        epoch: Option<u32>,
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
    /// `ProvideConnectionInfo`. v3 topology/transport/endpoint/ICE/fallback
    /// directives are carried by [`ServerMessage::SessionPlan`], not this
    /// message.
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
    /// Sent after the unchanged [`ServerMessage::GameStarting`] to every
    /// v3-capable member. It carries the chosen topology/transport, the host
    /// (for `host` topology), the validated host endpoint for Direct, the
    /// recipient's peer list with per-recipient `initiate` flags, ICE servers,
    /// and the relay `fallback`. Relay-resolved
    /// rooms send an explicit `relay`/`relay` plan with no host, peers, or ICE so
    /// the latest plan is always an authoritative reset. Protocol-v2 clients
    /// never observe this variant (boxed to keep the enum small, mirroring
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
    /// Another player reconnected to the room.
    ///
    /// `epoch` (v3 only) is the reconnector's new incarnation epoch — the same
    /// value now stamped on that player's relayed [`ServerMessage::GameData`] —
    /// so a recipient can re-baseline the per-sender `(epoch, seq)` stream
    /// immediately, before the first post-reconnect frame arrives. Stripped for
    /// pre-v3 recipients, keeping their bytes byte-identical to the frozen
    /// v2 wire.
    PlayerReconnected {
        player_id: PlayerId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        epoch: Option<u32>,
    },
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
    PeerTransportStatus {
        peer_id: PlayerId,
        transport: Transport,
        connected: bool,
    },
    /// Periodic per-connection relay-delivery statistics (v3 only).
    ///
    /// Emitted to a connection only when it negotiated protocol v3+ AND the
    /// deployment enabled `websocket.delivery_stats_interval_secs` (> 0;
    /// default 0 = disabled — enforcement happens at emission, so a pre-v3
    /// recipient can never observe this message). Counters are CUMULATIVE
    /// since the connection registered, so a frame skipped under load loses
    /// nothing — the next one carries the totals. These aggregates are
    /// diagnostic only: they cannot authorize or identify a `GameData.seq`
    /// gap; only an exact prior `DeliveryReport` can do that.
    ///
    /// The frame itself is advisory: it is enqueued best-effort on the
    /// connection's own queue and never counted in the statistics it reports.
    RelayStats {
        /// The configured emission interval in milliseconds
        /// (`websocket.delivery_stats_interval_secs * 1000`).
        interval_ms: u64,
        /// Messages the delivery layer accepted (enqueued) for this
        /// connection since it registered. Excludes the advisory `RelayStats`
        /// frames themselves.
        sent_to_you: u64,
        /// Messages omitted for this connection since it registered, including
        /// class-policy loss, undeliverable-encoding replacements, and messages
        /// abandoned by a slow-consumer close. This is an aggregate legacy
        /// diagnostic; `DeliveryReport` carries the exact v3 causes.
        dropped_for_you: u64,
        /// Deliveries that had to wait (true backpressure) on this connection's
        /// momentarily full outbound queue since it registered. V3 lossy
        /// classes never wait; reliable traffic (and all pre-v3 traffic) may.
        backpressure_events: u64,
    },
    /// Server shutdown advisory (v3 only).
    ///
    /// Emitted best-effort when the process starts a graceful drain. Clients
    /// should stop creating new rooms, prepare to reconnect after the close,
    /// and expect the server to close the WebSocket with private close code
    /// `4000` (`server_shutdown`) at or before `deadline_ms`. Pre-v3
    /// recipients never receive this message; every recipient still receives
    /// the semantic close frame.
    GoingAway {
        /// Unix epoch millisecond deadline when the server will force-close
        /// remaining sockets with close code 4000.
        deadline_ms: u64,
        /// Optional operator hint for client retry backoff.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_secs: Option<u64>,
    },
    /// Exact protocol-v3 accountability for data omitted on this connection.
    ///
    /// Gap-bearing reports are queued atomically with the corresponding lossy
    /// operation on the priority control lane, before any later data can expose
    /// the sequence gap. Counter-only reports may be emitted periodically.
    DeliveryReport(Box<DeliveryReportPayload>),
}

/// Deserialize an optional wire field while distinguishing omission from an
/// explicit `null`. The v3 schema makes class/key optional but non-nullable.
fn deserialize_present_optional<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
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
