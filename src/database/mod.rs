use crate::protocol::{
    ConnectionInfo, PlayerId, PlayerInfo, Room, RoomId, RoomPasswordCredential, SpectatorInfo,
};
use anyhow::Result;
use async_trait::async_trait;
use std::any::Any;
use std::collections::{HashMap, HashSet};
use thiserror::Error;
use uuid::Uuid;

/// Classified room-creation failure used by callers that may safely recover
/// from a generated-code collision without retrying unrelated storage faults.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CreateRoomError {
    #[error("Room code {room_code} already exists for game {game_name}")]
    RoomCodeCollision {
        game_name: String,
        room_code: String,
    },
    #[error(transparent)]
    Storage(#[from] anyhow::Error),
}

/// Room creation result that keeps a uniqueness conflict distinct from other
/// database failures.
pub type CreateRoomResult = std::result::Result<Room, CreateRoomError>;

/// Classified room-code rotation failure (issue #525). Keeps a candidate-code
/// collision distinct from unrelated storage faults so the caller can retry
/// fresh candidates without masking a lost room.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum UpdateRoomCodeError {
    #[error("Room code {room_code} already exists for game {game_name}")]
    RoomCodeCollision {
        game_name: String,
        room_code: String,
    },
    #[error(transparent)]
    Storage(#[from] anyhow::Error),
}

/// Room-code rotation result.
pub type UpdateRoomCodeResult = std::result::Result<Room, UpdateRoomCodeError>;

/// Summary describing how many rooms were removed by the cleanup routine.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RoomCleanupOutcome {
    pub empty_rooms_cleaned: usize,
    pub inactive_rooms_cleaned: usize,
}

/// Result of atomically transitioning a room into the finalized state.
///
/// Callers use this compare-and-set result to ensure only the winner publishes
/// the one-time game-start event. An already-finalized room is a normal race
/// outcome, not a storage failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeRoomGameOutcome {
    Finalized,
    AlreadyFinalized,
    SnapshotChanged,
}

/// Why an authority request or release was refused.
///
/// Storage distinguishes these cases internally. Carrying the distinction out
/// is what lets `AuthorityResponse` report the documented error code
/// (`docs/reference/error-codes.md`) rather than flattening every refusal to
/// `AUTHORITY_DENIED`: a client that merely lost a race would otherwise read
/// "you do not have permission" and disable host migration for good, while a
/// client facing a room that can never grant the role would retry forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityDenial {
    /// The room was created with `supports_authority: false`.
    NotSupported,
    /// Another member currently holds the role.
    AlreadyHeld,
    /// The requester is not a member of the room.
    NotAMember,
    /// A release from a member that does not hold the role.
    NotHeld,
    /// The room no longer exists.
    RoomNotFound,
    /// Storage could not decide the request. Not a refusal by policy — the
    /// coordinator reports it so the one response a client is promised still
    /// carries an honest cause.
    StorageError,
}

impl AuthorityDenial {
    /// Client-facing `reason` text carried on `AuthorityResponse`.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::NotSupported => "Room does not support authority",
            Self::AlreadyHeld => "Another player already has authority",
            Self::NotAMember => "Player not found in room",
            Self::NotHeld => "You do not have authority to release",
            Self::RoomNotFound => "Room not found",
            Self::StorageError => "Storage error",
        }
    }

    /// Documented `ErrorCode` for this refusal. Membership and ownership
    /// refusals share `AUTHORITY_DENIED`; a missing room retains the distinct
    /// `ROOM_NOT_FOUND` lifecycle outcome.
    #[must_use]
    pub fn error_code(self) -> crate::protocol::ErrorCode {
        match self {
            Self::NotSupported => crate::protocol::ErrorCode::AuthorityNotSupported,
            Self::AlreadyHeld => crate::protocol::ErrorCode::AuthorityConflict,
            Self::NotAMember | Self::NotHeld => crate::protocol::ErrorCode::AuthorityDenied,
            Self::RoomNotFound => crate::protocol::ErrorCode::RoomNotFound,
            Self::StorageError => crate::protocol::ErrorCode::StorageError,
        }
    }
}

/// Result of an atomic authority request or release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityOutcome {
    Granted,
    Denied(AuthorityDenial),
}

impl AuthorityOutcome {
    /// Whether the requested transition was performed.
    #[must_use]
    pub fn granted(self) -> bool {
        matches!(self, Self::Granted)
    }

    /// The refusal, if this outcome is one.
    #[must_use]
    pub fn denial(self) -> Option<AuthorityDenial> {
        match self {
            Self::Granted => None,
            Self::Denied(denial) => Some(denial),
        }
    }
}

/// Exact room state from which a game-start publication was prepared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizeRoomGameExpectation {
    pub members: Vec<PlayerId>,
    pub authority_player: Option<PlayerId>,
    pub lobby_state: crate::protocol::LobbyState,
}

impl FinalizeRoomGameExpectation {
    #[must_use]
    pub fn from_room(room: &Room) -> Self {
        Self {
            members: room.players.keys().copied().collect(),
            authority_player: room.authority_player,
            lobby_state: room.lobby_state.clone(),
        }
    }
}

impl RoomCleanupOutcome {
    /// Total rooms removed (empty + inactive).
    pub fn total_cleaned(&self) -> usize {
        self.empty_rooms_cleaned
            .saturating_add(self.inactive_rooms_cleaned)
    }

    pub fn is_empty(&self) -> bool {
        self.total_cleaned() == 0
    }
}

/// Database abstraction trait for game server storage
#[async_trait]
pub trait GameDatabase: Send + Sync {
    /// Initialize the database connection and run migrations
    async fn initialize(&self) -> Result<()>;

    /// Create a new room with atomic room code generation
    /// Returns the created room or error if room code collision
    ///
    /// `room_code: None` asks the implementation to mint one. Production room
    /// creation always mints a configured-length code itself and passes
    /// `Some` (`protocol.room_code_length`); the built-in fallback for `None`
    /// mints a `ProtocolConfig::default()`-length (6-character) clean code that
    /// is NOT lengthened to a non-default `room_code_length`, so a joiner
    /// validating against a non-default configured length would refuse it.
    /// Pass an explicit code when a non-default length is configured.
    #[allow(clippy::too_many_arguments)]
    async fn create_room(
        &self,
        game_name: String,
        room_code: Option<String>,
        max_players: u8,
        supports_authority: bool,
        creator_id: PlayerId,
        relay_type: String,
        region_id: String,
        application_id: Option<Uuid>,
    ) -> Result<Room>;

    /// Create a room while preserving collision identity for bounded generated
    /// code retries. Implementations should override this method when they can
    /// classify their storage backend's uniqueness violation.
    ///
    /// `join_password` must be stored atomically with the room. The default
    /// refuses protected creation before writing anything. Backends that
    /// support protected rooms must override this method.
    #[allow(clippy::too_many_arguments)]
    async fn create_room_classified(
        &self,
        game_name: String,
        room_code: Option<String>,
        max_players: u8,
        supports_authority: bool,
        creator_id: PlayerId,
        relay_type: String,
        region_id: String,
        application_id: Option<Uuid>,
        join_password: Option<RoomPasswordCredential>,
    ) -> CreateRoomResult {
        if join_password.is_some() {
            return Err(CreateRoomError::Storage(anyhow::anyhow!(
                "atomic room password creation is not supported by this database"
            )));
        }
        let room = self
            .create_room(
                game_name,
                room_code,
                max_players,
                supports_authority,
                creator_id,
                relay_type,
                region_id,
                application_id,
            )
            .await
            .map_err(CreateRoomError::Storage)?;
        Ok(room)
    }

    /// Server admission creates a hidden room and publishes it with
    /// [`Self::publish_room`] as its first response commits. A backend that
    /// supports server room creation must override this method and insert the
    /// pending marker atomically with the room and code. The default refuses
    /// creation before writing anything (#658).
    #[allow(clippy::too_many_arguments)]
    async fn create_pending_room_classified(
        &self,
        _game_name: String,
        _room_code: Option<String>,
        _max_players: u8,
        _supports_authority: bool,
        _creator_id: PlayerId,
        _relay_type: String,
        _region_id: String,
        _application_id: Option<Uuid>,
        _join_password: Option<RoomPasswordCredential>,
    ) -> CreateRoomResult {
        Err(CreateRoomError::Storage(anyhow::anyhow!(
            "pending room creation is not supported by this database"
        )))
    }

    /// Create a pending room while its creator owns the supplied liveness
    /// token. Backends with a repairable Creating state should store a weak
    /// reference atomically with insertion. The default retains compatibility
    /// with adapters that implement the original pending-room method. Those
    /// adapters must override this method to make insertion accounting and
    /// abandoned-room repair atomic; the default cannot guarantee either.
    #[allow(clippy::too_many_arguments)]
    async fn create_pending_room_owned_classified(
        &self,
        game_name: String,
        room_code: Option<String>,
        max_players: u8,
        supports_authority: bool,
        creator_id: PlayerId,
        relay_type: String,
        region_id: String,
        application_id: Option<Uuid>,
        join_password: Option<RoomPasswordCredential>,
        _creation_owner: PendingRoomCreation,
    ) -> CreateRoomResult {
        self.create_pending_room_classified(
            game_name,
            room_code,
            max_players,
            supports_authority,
            creator_id,
            relay_type,
            region_id,
            application_id,
            join_password,
        )
        .await
    }
    async fn set_room_application_id(
        &self,
        _room_id: &RoomId,
        _application_id: Uuid,
    ) -> Result<()> {
        anyhow::bail!("room application ownership persistence is not supported")
    }

    async fn clear_room_application_id(&self, _room_id: &RoomId) -> Result<()> {
        anyhow::bail!("room application ownership persistence is not supported")
    }

    /// Clear an application claim only when its persisted owner still matches.
    /// A missing room is an idempotent terminal outcome (`Ok(false)`).
    async fn clear_room_application_id_if_matches(
        &self,
        _room_id: &RoomId,
        _application_id: Uuid,
    ) -> Result<bool> {
        anyhow::bail!("conditional room application ownership persistence is not supported")
    }

    /// Replace a room's code atomically (authority-initiated rotation,
    /// issue #525).
    ///
    /// Implementations must swap the stored `Room.code` and the
    /// `(game_name, room_code) -> RoomId` registry entry as one action under
    /// the same lock order as [`Self::create_room`] (rooms, then room_codes),
    /// and must classify a `new_code` that already resolves to another room
    /// as [`UpdateRoomCodeError::RoomCodeCollision`]. A missing room is a
    /// storage error; the caller treats it as a lost race.
    async fn update_room_code(&self, _room_id: &RoomId, _new_code: String) -> UpdateRoomCodeResult {
        Err(UpdateRoomCodeError::Storage(anyhow::anyhow!(
            "room code rotation persistence is not supported"
        )))
    }

    /// Apply a room's spectator capacity (creation-time default,
    /// issue #525).
    ///
    /// `None` means unlimited. A positive deployment cap must be persisted
    /// before the creator joins; a write error refuses room creation. The
    /// caller holds the room-code lock while applying the default.
    async fn set_room_max_spectators(
        &self,
        _room_id: &RoomId,
        _max_spectators: Option<u8>,
    ) -> Result<()> {
        anyhow::bail!("spectator capacity persistence is not supported")
    }

    /// Store a room's hashed join password (authority-initiated access
    /// control, issue #525).
    ///
    /// `None` opens the room. Callers pass the already-hashed credential —
    /// the plaintext password never reaches storage. Callers hold the room
    /// mutation gate across the write, so a missing room is an error but
    /// cannot otherwise race admission.
    async fn set_room_password(
        &self,
        _room_id: &RoomId,
        _password: Option<RoomPasswordCredential>,
    ) -> Result<()> {
        anyhow::bail!("room password persistence is not supported")
    }

    /// Add or lift a room-scoped player ban (authority-initiated moderation,
    /// issue #525).
    ///
    /// The ban lives on the room row, so it expires with the room. Callers
    /// hold the room mutation gate across the write, so a missing room is an
    /// error but cannot otherwise race admission.
    async fn set_room_ban(
        &self,
        _room_id: &RoomId,
        _player_id: &PlayerId,
        _banned: bool,
    ) -> Result<()> {
        anyhow::bail!("room ban persistence is not supported")
    }

    /// Get room by game name and room code
    async fn get_room(&self, game_name: &str, room_code: &str) -> Result<Option<Room>>;

    /// Get room by ID
    async fn get_room_by_id(&self, room_id: &RoomId) -> Result<Option<Room>>;

    /// Add player to room (atomic operation).
    ///
    /// Implementations must store `is_authority` derived from the room's
    /// `authority_player`, not from the supplied `player`: callers legitimately
    /// pass pre-disconnect snapshots whose flag can be stale, and a stored
    /// member that contradicts `authority_player` would surface two authorities
    /// in `RoomJoined` / `Reconnected` / `GameStarting` payloads.
    /// Return `false` without mutation when the room is full or already
    /// contains this ID as a player or spectator.
    async fn add_player_to_room(&self, room_id: &RoomId, player: PlayerInfo) -> Result<bool>;

    /// Remove player from room.
    ///
    /// The returned record reports whether this removal vacated the room's
    /// authority: implementations must return it with `is_authority` true
    /// exactly when they cleared `authority_player` for this member, because
    /// the departure path announces the cleared role from that flag (see
    /// `EnhancedGameServer::leave_room_locked`). Keeping the stored flag in
    /// lockstep with `authority_player` — as [`Self::add_player_to_room`]
    /// requires on the way in — satisfies this.
    async fn remove_player_from_room(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
    ) -> Result<Option<PlayerInfo>>;

    /// Assign a current room player as authority, or clear the role with `None`.
    ///
    /// Return `false` without changing the room when it is missing, does not
    /// support authority, or the requested player is absent from its roster.
    /// Membership validation and assignment form one atomic storage operation.
    /// This method permits replacing an existing authority; use
    /// [`Self::request_room_authority`] for the request/release policy instead.
    #[allow(dead_code)]
    async fn update_room_authority(
        &self,
        room_id: &RoomId,
        authority_player: Option<PlayerId>,
    ) -> Result<bool>;

    /// Atomically request room authority with proper protocol enforcement.
    ///
    /// A refusal must name its cause: the coordinator maps
    /// [`AuthorityDenial`] straight onto the wire `reason` and `error_code`,
    /// so a cause collapsed here is a cause the client cannot act on.
    async fn request_room_authority(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        become_authority: bool,
    ) -> Result<AuthorityOutcome>;

    /// Update a member name and renew room activity only when the value changes.
    async fn update_player_name(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        name: &str,
    ) -> Result<bool>;

    /// Update legacy self-declared peer metadata for `GameStarting`.
    /// Renew room activity only when the stored value changes.
    async fn update_player_connection_info(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        connection_info: ConnectionInfo,
    ) -> Result<bool>;

    /// Clear metadata from a disconnected socket before a retained room row
    /// is published by reconnect. Return false if the row no longer exists.
    /// Renew room activity only when metadata was present.
    async fn clear_player_connection_info(
        &self,
        _room_id: &RoomId,
        _player_id: &PlayerId,
    ) -> Result<bool> {
        anyhow::bail!("clearing player connection info is not supported")
    }

    /// Get all players in a room
    async fn get_room_players(&self, room_id: &RoomId) -> Result<Vec<PlayerInfo>>;

    /// Delete empty rooms and return their IDs for relay cleanup.
    ///
    /// Rooms in `protected` are never deleted regardless of age: they hold a
    /// still-valid reconnection record, so deleting them would strand a live
    /// reconnection token behind a `RoomNotFound` (BUG-1 corollary B).
    async fn cleanup_empty_rooms(
        &self,
        empty_timeout: chrono::Duration,
        protected: &HashSet<RoomId>,
    ) -> Result<Vec<RoomId>>;

    /// Delete expired rooms based on timeouts and return a summary of what was
    /// removed. Rooms in `protected` are never deleted (see
    /// [`Self::cleanup_empty_rooms`]).
    async fn cleanup_expired_rooms(
        &self,
        empty_timeout: chrono::Duration,
        inactive_timeout: chrono::Duration,
        protected: &HashSet<RoomId>,
    ) -> Result<RoomCleanupOutcome>;

    /// Update room activity timestamp
    async fn update_room_activity(&self, room_id: &RoomId) -> Result<()>;

    /// Delete a specific room by ID
    #[allow(dead_code)]
    async fn delete_room(&self, room_id: &RoomId) -> Result<bool>;

    /// Make a room visible while the creator holds its code and mutation
    /// locks, immediately before the first response queue commit. A canceled
    /// commit rolls the room back before either lock is released. Backends
    /// supporting room creation must implement the full pending lifecycle.
    async fn publish_room(&self, _room_id: &RoomId) -> Result<()> {
        anyhow::bail!("pending room publication is not supported by this database")
    }

    async fn abandon_room(&self, _room_id: &RoomId, _creator_id: &PlayerId) -> Result<()> {
        anyhow::bail!("pending room abandonment is not supported by this database")
    }

    /// Enumerate unpublished rooms eligible for repair. A creating room may be
    /// listed after its creator stops; deletion still takes the room-code lock
    /// and rechecks that state atomically.
    async fn pending_room_ids(&self) -> Result<Vec<RoomId>> {
        anyhow::bail!("pending room enumeration is not supported by this database")
    }

    /// Delete an unpublished room after taking its room-code lock. The state
    /// must be checked again atomically with deletion because publication may
    /// finish between enumeration and lock acquisition.
    async fn delete_room_if_pending(&self, _room_id: &RoomId) -> Result<bool> {
        anyhow::bail!("pending room deletion is not supported by this database")
    }

    /// Recheck admission visibility after taking the room mutation lane.
    async fn is_room_published(&self, _room_id: &RoomId) -> Result<bool> {
        anyhow::bail!("room publication state is not supported by this database")
    }

    /// Get room count for a specific game (for rate limiting)
    async fn get_game_room_count(&self, game_name: &str) -> Result<usize>;

    /// Get the authoritative live-room count for one application across every
    /// game name. Backends that cannot provide this query must return an error;
    /// configured application quotas fail closed rather than using a cache or
    /// silently bypassing the limit.
    async fn get_application_room_count(&self, _application_id: &Uuid) -> Result<usize> {
        anyhow::bail!("application room counting is not supported by this database")
    }

    /// Get the authoritative live-room count across every game name
    /// (server-wide ceiling). Backends that cannot provide this query must
    /// return an error; configured server-wide ceilings fail closed rather
    /// than using a cache or silently bypassing the limit.
    async fn get_total_room_count(&self) -> Result<usize> {
        anyhow::bail!("total room counting is not supported by this database")
    }

    /// Health check
    async fn health_check(&self) -> bool;

    /// Per-session record registry for the metrics surface (issues #708,
    /// #763). Backends that do not track session records return `None`; the
    /// `/metrics/sessions` surface then reports itself unavailable instead of
    /// showing an empty, misleading history.
    fn session_records(&self) -> Option<std::sync::Arc<crate::session_records::SessionRecords>> {
        None
    }

    /// Update a player's `last_seen` timestamp for local liveness and cleanup.
    async fn update_player_last_seen(&self, player_id: &PlayerId) -> Result<()>;

    /// Get room counts by game name for metrics
    async fn get_rooms_by_game(&self) -> Result<HashMap<String, usize>>;

    /// Get player count statistics for metrics
    async fn get_player_count_percentiles(&self) -> Result<HashMap<String, f64>>;

    /// Get player count statistics by game for metrics
    async fn get_game_player_percentiles(&self) -> Result<HashMap<String, HashMap<String, f64>>>;

    /// Transition a non-empty waiting room into its lobby state.
    async fn transition_room_to_lobby(&self, room_id: &RoomId) -> Result<()>;

    /// Toggle player ready state and return lobby information if successful
    /// Returns (lobby_state, ready_players, all_ready) for a present lobby member.
    /// Returns None without mutation for an absent player or a non-lobby room.
    ///
    /// Test-only parity surface today: the live ready path is the coordinator's
    /// own ready map under the room mutation gate (which accepts any
    /// non-finalized room), while this storage gate accepts exactly
    /// [`crate::protocol::LobbyState::Lobby`]. Do not wire this into
    /// production without
    /// reconciling that state-gate divergence.
    async fn toggle_player_ready(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
    ) -> Result<Option<(crate::protocol::LobbyState, Vec<PlayerId>, bool)>>;

    /// Atomically finalize a room game when all players are ready.
    async fn finalize_room_game(
        &self,
        room_id: &RoomId,
        expected: &FinalizeRoomGameExpectation,
    ) -> Result<FinalizeRoomGameOutcome>;

    /// Add spectator to room (atomic operation)
    /// Return false without mutation if the room is full, absent, or already
    /// contains this ID as a spectator or player.
    async fn add_spectator_to_room(
        &self,
        room_id: &RoomId,
        spectator: SpectatorInfo,
    ) -> Result<bool>;

    /// Remove spectator from room
    /// Returns the removed spectator info if they existed
    async fn remove_spectator_from_room(
        &self,
        room_id: &RoomId,
        spectator_id: &PlayerId,
    ) -> Result<Option<SpectatorInfo>>;

    /// Get all spectators in a room
    async fn get_room_spectators(&self, room_id: &RoomId) -> Result<Vec<SpectatorInfo>>;

    /// Try to claim a room cleanup operation for idempotency.
    /// Returns true if this cleanup path claimed the operation, or false if an
    /// earlier cleanup path in the process already claimed it.
    ///
    /// The shipped database is process-local, so this prevents duplicate
    /// post-cleanup operations within one process. A future shared backend would
    /// also need a separately verified room-authority and routing protocol.
    async fn try_claim_room_cleanup(
        &self,
        room_id: &RoomId,
        cleanup_type: &str,
        instance_id: &uuid::Uuid,
    ) -> Result<bool>;

    /// Cleanup old room cleanup events (called periodically)
    async fn cleanup_old_room_cleanup_events(&self) -> Result<u64>;

    /// Scrape-time live-object counts for the metrics endpoint: live rooms,
    /// seated occupants (players + spectators), and pending room
    /// publications (in-flight creation or awaiting repair). Sampled when the
    /// endpoint is hit, never maintained on the write path. The default
    /// reports unavailable (`None`) so embedders' backends are never
    /// guessed at; the shipped in-memory backend answers under short read
    /// locks.
    async fn live_room_counts(&self) -> Option<LiveRoomCounts> {
        None
    }

    /// Downcast helper to access backend-specific implementations
    fn as_any(&self) -> &(dyn Any + Send + Sync);
}

/// Capability marker traits that identify focused slices of the GameDatabase contract.
/// They allow call sites to depend on more precise bounds (e.g., RoomStore + MetricsStore)
/// while still using the existing GameDatabase implementations via blanket impls below.
pub trait DatabaseMaintenance: GameDatabase {}
impl<T: GameDatabase + ?Sized> DatabaseMaintenance for T {}

pub trait RoomStore: GameDatabase {}
impl<T: GameDatabase + ?Sized> RoomStore for T {}

pub trait PlayerStore: GameDatabase {}
impl<T: GameDatabase + ?Sized> PlayerStore for T {}

pub trait MetricsStore: GameDatabase {}
impl<T: GameDatabase + ?Sized> MetricsStore for T {}

pub trait AdminDirectory: GameDatabase {}
impl<T: GameDatabase + ?Sized> AdminDirectory for T {}

/// Database configuration — in-memory only for signal-fish-server.
#[derive(Debug, Clone, Default)]
pub enum DatabaseConfig {
    #[default]
    InMemory,
}

impl DatabaseConfig {
    /// Create database configuration from environment (always returns InMemory)
    pub fn from_env() -> Result<Self> {
        Ok(Self::InMemory)
    }
}

/// Create database instance based on configuration
pub async fn create_database(config: DatabaseConfig) -> Result<Box<dyn GameDatabase>> {
    match config {
        DatabaseConfig::InMemory => {
            let db = InMemoryDatabase::new();
            Ok(Box::new(db))
        }
    }
}

/// Scrape-time live-object counts behind the metrics endpoint (the capacity
/// campaign's live-state observables). All values are exact counts of live
/// server state at the scrape instant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiveRoomCounts {
    /// Rooms live in storage (players and/or spectators, or freshly created).
    pub rooms: u64,
    /// Seated occupants across all live rooms (players + spectators).
    pub occupants: u64,
    /// Rooms in the pending-publication lifecycle (in-flight creation or
    /// awaiting repair).
    pub pending_publications: u64,
}

/// How long after a claimed cleanup a room may be re-claimed for the same
/// cleanup type (the room was re-created and became empty again).
///
/// Strictly longer than the wall-clock bucket scheme this replaced, whose
/// phase-dependent roll let a re-claim through anywhere from 0 to 5 minutes
/// after a claim: the monotonic window is a uniform 5 minutes from the claim
/// itself, so a re-created room is re-cleanable at a known, wall-jump-proof
/// instant.
const CLEANUP_RECLAIM_WINDOW: chrono::Duration = chrono::Duration::minutes(5);

/// How long a claimed-cleanup entry is retained before the maintenance prune
/// forgets it.
const CLEANUP_EVENT_RETENTION: chrono::Duration = chrono::Duration::hours(1);

/// Tracks one server-owned pending room through cancellation and repair.
/// The database holds only a weak owner reference, so dropping the creator
/// makes its unfinished room eligible for cleanup.
#[derive(Clone)]
pub struct PendingRoomCreation {
    owner: std::sync::Arc<()>,
    accounting: std::sync::Arc<PendingRoomAccounting>,
}

struct PendingRoomAccounting {
    metrics: std::sync::Arc<crate::metrics::ServerMetrics>,
    inserted: std::sync::atomic::AtomicBool,
    creator_left: std::sync::atomic::AtomicBool,
}

impl PendingRoomCreation {
    pub fn new(metrics: std::sync::Arc<crate::metrics::ServerMetrics>) -> Self {
        Self {
            owner: std::sync::Arc::new(()),
            accounting: std::sync::Arc::new(PendingRoomAccounting {
                metrics,
                inserted: std::sync::atomic::AtomicBool::new(false),
                creator_left: std::sync::atomic::AtomicBool::new(false),
            }),
        }
    }

    /// Count the inserted room and its initial member once, before any later
    /// suspension can lose the creator operation.
    pub(crate) fn record_inserted(&self) {
        self.accounting.record_inserted();
    }

    pub(crate) fn record_creator_left(&self) {
        self.accounting.record_creator_left();
    }
}

impl PendingRoomAccounting {
    fn record_inserted(&self) {
        if !self
            .inserted
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            self.metrics.increment_rooms_created();
            self.metrics.increment_players_joined();
        }
    }

    fn record_creator_left(&self) {
        self.record_inserted();
        if !self
            .creator_left
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            self.metrics.increment_players_left();
        }
    }
}

enum PendingRoomState {
    Creating {
        creator_id: PlayerId,
        /// Legacy direct callers have no operation token and are not swept
        /// automatically; server admission uses an owned token.
        owner: Option<std::sync::Weak<()>>,
        accounting: Option<std::sync::Arc<PendingRoomAccounting>>,
    },
    Abandoned {
        creator_id: PlayerId,
        accounting: Option<std::sync::Arc<PendingRoomAccounting>>,
    },
}

/// Saturating `std::time::Duration` → `chrono::Duration` conversion for
/// monotonic elapsed values (an `Instant` elapsed cannot be negative, but it
/// can exceed the chrono range).
fn chrono_elapsed(duration: std::time::Duration) -> chrono::Duration {
    chrono::Duration::from_std(duration).unwrap_or(chrono::Duration::MAX)
}

/// Entry tracking a claimed room cleanup operation for idempotency
#[derive(Debug, Clone)]
struct CleanupEventEntry {
    #[allow(dead_code)]
    instance_id: uuid::Uuid,
    /// Monotonic instant of the claim. Both timing decisions — the
    /// re-claim window and the retention prune — read this stamp, never the
    /// wall clock, so an NTP step cannot shorten, extend, or revive a dedupe
    /// window, and the boundaries are deterministic to test under paused
    /// tokio time.
    claimed_at: tokio::time::Instant,
}

/// Simple in-memory database for testing and single-instance deployments
pub struct InMemoryDatabase {
    rooms: std::sync::Arc<tokio::sync::RwLock<HashMap<RoomId, Room>>>,
    /// Per-session lifecycle records (issues #708, #763): every create,
    /// join, leave, and close path below funnels through this registry.
    session_records: std::sync::Arc<crate::session_records::SessionRecords>,
    /// Maps (game_name, room_code) -> room_id to allow same room codes across different games
    room_codes: std::sync::Arc<tokio::sync::RwLock<HashMap<(String, String), RoomId>>>,
    /// Missing means published. Lock after rooms, codes, and liveness when
    /// those guards are also needed, so creation and deletion remain atomic.
    room_publication: std::sync::Arc<tokio::sync::RwLock<HashMap<RoomId, PendingRoomState>>>,
    /// Monotonic per-room activity used for garbage-collection decisions.
    ///
    /// A wall-clock step (NTP correction, manual clock change, host
    /// suspend/resume) must not reap occupied rooms whose members are
    /// monotonic-fresh, nor retain idle rooms past their timeout. This mirrors
    /// the discipline already pinned for reconnect windows and client pings:
    /// liveness decisions key off `tokio::time::Instant`, while the wall-clock
    /// `Room::last_activity` remains the operator-facing observability record.
    /// Entries are refreshed by every production path that refreshes
    /// `last_activity` and removed wherever the room row is removed.
    ///
    /// Lock ordering with the other maps is always `rooms`, then
    /// `room_codes`, then this map.
    room_liveness_monotonic: std::sync::Arc<tokio::sync::RwLock<HashMap<RoomId, RoomLiveness>>>,
    /// Tracks claimed cleanup operations for idempotency (cleanup_id -> entry)
    cleanup_events: std::sync::Arc<tokio::sync::RwLock<HashMap<String, CleanupEventEntry>>>,
    #[cfg(test)]
    fail_get_room_players: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_get_room_by_id: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_get_application_room_count: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_clear_room_application_id: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_set_room_application_id: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    get_application_room_count_calls: std::sync::atomic::AtomicU32,
    #[cfg(test)]
    pause_get_application_room_count: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    get_application_room_count_reached: tokio::sync::Notify,
    #[cfg(test)]
    release_get_application_room_count: tokio::sync::Notify,
    #[cfg(test)]
    pause_get_total_room_count: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    get_total_room_count_reached: tokio::sync::Notify,
    #[cfg(test)]
    release_get_total_room_count: tokio::sync::Notify,
    #[cfg(test)]
    pause_update_room_code: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    update_room_code_reached: tokio::sync::Notify,
    #[cfg(test)]
    release_update_room_code: tokio::sync::Notify,
    #[cfg(test)]
    get_room_by_id_calls: std::sync::atomic::AtomicU32,
    #[cfg(test)]
    pause_get_room_by_id: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    get_room_by_id_reached: tokio::sync::Notify,
    #[cfg(test)]
    release_get_room_by_id: tokio::sync::Notify,
    #[cfg(test)]
    fail_remove_player_from_room: std::sync::atomic::AtomicBool,
    /// One-shot injection: the next `add_player_to_room` first deletes the
    /// target room, reproducing inactive-room GC winning the race against a
    /// reconnect's membership restore — the exact storage state whose wire
    /// classification must not read as a storage fault.
    #[cfg(all(test, signal_fish_repository_tests))]
    delete_room_during_add_once: std::sync::atomic::AtomicBool,
    #[cfg(all(test, signal_fish_repository_tests))]
    fail_update_player_name: std::sync::atomic::AtomicBool,
    #[cfg(all(test, signal_fish_repository_tests))]
    miss_update_player_name_once: std::sync::atomic::AtomicBool,
    #[cfg(all(test, signal_fish_repository_tests))]
    fail_set_room_max_spectators_once: std::sync::atomic::AtomicBool,
    #[cfg(all(test, signal_fish_repository_tests))]
    panic_set_room_max_spectators_once: std::sync::atomic::AtomicBool,
    #[cfg(all(test, signal_fish_repository_tests))]
    panic_update_player_name_once: std::sync::atomic::AtomicBool,
    #[cfg(all(test, signal_fish_repository_tests))]
    panic_after_pending_insert_once: std::sync::atomic::AtomicBool,
    #[cfg(all(test, signal_fish_repository_tests))]
    fail_delete_room_once: std::sync::atomic::AtomicBool,
    /// One-shot storage failure for the next `add_player_to_room`: the
    /// reconnect membership-restore write fails while the room demonstrably
    /// still exists, so the attempt must be refused as a retryable storage
    /// fault (not reclassified `ROOM_NOT_FOUND`) and leave no restored state.
    #[cfg(all(test, signal_fish_repository_tests))]
    fail_add_player_to_room_once: std::sync::atomic::AtomicBool,
    /// One-shot storage failure for the next `request_room_authority`: the
    /// reconnect authority grant fails after membership restored, so the
    /// attempt must complete degraded (live member, vacant role) with the
    /// next reconnect re-running the grant.
    #[cfg(all(test, signal_fish_repository_tests))]
    fail_request_room_authority_once: std::sync::atomic::AtomicBool,
    #[cfg(all(test, signal_fish_repository_tests))]
    pause_publish_room_once: std::sync::atomic::AtomicBool,
    #[cfg(all(test, signal_fish_repository_tests))]
    publish_room_reached: tokio::sync::Notify,
    #[cfg(all(test, signal_fish_repository_tests))]
    release_publish_room: tokio::sync::Notify,
    #[cfg(all(test, signal_fish_repository_tests))]
    pause_update_player_connection_info_once: std::sync::atomic::AtomicBool,
    #[cfg(all(test, signal_fish_repository_tests))]
    update_player_connection_info_reached: tokio::sync::Notify,
    #[cfg(all(test, signal_fish_repository_tests))]
    release_update_player_connection_info: tokio::sync::Notify,
    #[cfg(test)]
    fail_remove_spectator_from_room: std::sync::atomic::AtomicBool,
    /// Join-race determinism gate: used only by repository-only test modules
    /// (`room_service_tests`), so it is gated out of packaged builds together
    /// with them.
    #[cfg(all(test, signal_fish_repository_tests))]
    pause_add_player_to_room: std::sync::atomic::AtomicBool,
    #[cfg(all(test, signal_fish_repository_tests))]
    add_player_reached: tokio::sync::Notify,
    #[cfg(all(test, signal_fish_repository_tests))]
    release_add_player: tokio::sync::Notify,
    #[cfg(test)]
    pause_authority_request_after_commit: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    authority_request_commit_reached: tokio::sync::Notify,
    #[cfg(test)]
    release_authority_request_commit: tokio::sync::Notify,
    #[cfg(test)]
    get_room_players_calls: std::sync::atomic::AtomicU32,
    /// One-shot test pause inside `get_room_players`: parks the caller at the
    /// read so a test can interleave (for example flip the shutdown drain)
    /// while a reconnect transaction is inside its baseline builder, before
    /// the builder's final rotation step.
    ///
    /// Gated `signal_fish_repository_tests` — like every seam whose only
    /// callers live in a repository-only `*_tests.rs` module — so the
    /// packaged crate (which strips those modules) never sees dead code
    /// under the verifier's `-D warnings` build.
    #[cfg(all(test, signal_fish_repository_tests))]
    pause_get_room_players: std::sync::atomic::AtomicBool,
    #[cfg(all(test, signal_fish_repository_tests))]
    get_room_players_reached: tokio::sync::Notify,
    #[cfg(all(test, signal_fish_repository_tests))]
    release_get_room_players: tokio::sync::Notify,
}

impl InMemoryDatabase {
    pub fn new() -> Self {
        Self {
            rooms: std::sync::Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            session_records: std::sync::Arc::new(crate::session_records::SessionRecords::new()),
            room_codes: std::sync::Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            room_publication: std::sync::Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            room_liveness_monotonic: std::sync::Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            cleanup_events: std::sync::Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            #[cfg(test)]
            fail_get_room_players: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_get_room_by_id: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_get_application_room_count: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_clear_room_application_id: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_set_room_application_id: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            get_application_room_count_calls: std::sync::atomic::AtomicU32::new(0),
            #[cfg(test)]
            pause_get_application_room_count: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            get_application_room_count_reached: tokio::sync::Notify::new(),
            #[cfg(test)]
            release_get_application_room_count: tokio::sync::Notify::new(),
            #[cfg(test)]
            pause_get_total_room_count: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            get_total_room_count_reached: tokio::sync::Notify::new(),
            #[cfg(test)]
            release_get_total_room_count: tokio::sync::Notify::new(),
            #[cfg(test)]
            pause_update_room_code: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            update_room_code_reached: tokio::sync::Notify::new(),
            #[cfg(test)]
            release_update_room_code: tokio::sync::Notify::new(),
            #[cfg(test)]
            get_room_by_id_calls: std::sync::atomic::AtomicU32::new(0),
            #[cfg(test)]
            pause_get_room_by_id: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            get_room_by_id_reached: tokio::sync::Notify::new(),
            #[cfg(test)]
            release_get_room_by_id: tokio::sync::Notify::new(),
            #[cfg(test)]
            fail_remove_player_from_room: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, signal_fish_repository_tests))]
            delete_room_during_add_once: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, signal_fish_repository_tests))]
            fail_update_player_name: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, signal_fish_repository_tests))]
            miss_update_player_name_once: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, signal_fish_repository_tests))]
            fail_set_room_max_spectators_once: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, signal_fish_repository_tests))]
            panic_set_room_max_spectators_once: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, signal_fish_repository_tests))]
            panic_update_player_name_once: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, signal_fish_repository_tests))]
            panic_after_pending_insert_once: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, signal_fish_repository_tests))]
            fail_delete_room_once: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, signal_fish_repository_tests))]
            fail_add_player_to_room_once: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, signal_fish_repository_tests))]
            fail_request_room_authority_once: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, signal_fish_repository_tests))]
            pause_publish_room_once: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, signal_fish_repository_tests))]
            publish_room_reached: tokio::sync::Notify::new(),
            #[cfg(all(test, signal_fish_repository_tests))]
            release_publish_room: tokio::sync::Notify::new(),
            #[cfg(all(test, signal_fish_repository_tests))]
            pause_update_player_connection_info_once: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, signal_fish_repository_tests))]
            update_player_connection_info_reached: tokio::sync::Notify::new(),
            #[cfg(all(test, signal_fish_repository_tests))]
            release_update_player_connection_info: tokio::sync::Notify::new(),
            #[cfg(test)]
            fail_remove_spectator_from_room: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, signal_fish_repository_tests))]
            pause_add_player_to_room: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, signal_fish_repository_tests))]
            add_player_reached: tokio::sync::Notify::new(),
            #[cfg(all(test, signal_fish_repository_tests))]
            release_add_player: tokio::sync::Notify::new(),
            #[cfg(test)]
            pause_authority_request_after_commit: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            authority_request_commit_reached: tokio::sync::Notify::new(),
            #[cfg(test)]
            release_authority_request_commit: tokio::sync::Notify::new(),
            #[cfg(test)]
            get_room_players_calls: std::sync::atomic::AtomicU32::new(0),
            #[cfg(all(test, signal_fish_repository_tests))]
            pause_get_room_players: std::sync::atomic::AtomicBool::new(false),
            #[cfg(all(test, signal_fish_repository_tests))]
            get_room_players_reached: tokio::sync::Notify::new(),
            #[cfg(all(test, signal_fish_repository_tests))]
            release_get_room_players: tokio::sync::Notify::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn fail_get_room_players_for_test(&self, fail: bool) {
        self.fail_get_room_players
            .store(fail, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub(crate) fn fail_get_room_by_id_for_test(&self, fail: bool) {
        self.fail_get_room_by_id
            .store(fail, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub(crate) fn fail_get_application_room_count_for_test(&self, fail: bool) {
        self.fail_get_application_room_count
            .store(fail, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub(crate) fn fail_clear_room_application_id_for_test(&self, fail: bool) {
        self.fail_clear_room_application_id
            .store(fail, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub(crate) fn fail_set_room_application_id_for_test(&self, fail: bool) {
        self.fail_set_room_application_id
            .store(fail, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub(crate) fn pause_next_get_application_room_count_for_test(&self) {
        self.get_application_room_count_calls
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.pause_get_application_room_count
            .store(true, std::sync::atomic::Ordering::Release);
    }

    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub(crate) async fn wait_for_paused_get_application_room_count_for_test(&self) {
        self.get_application_room_count_reached.notified().await;
    }

    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub(crate) fn release_paused_get_application_room_count_for_test(&self) {
        self.release_get_application_room_count.notify_one();
    }

    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub(crate) fn get_application_room_count_calls_for_test(&self) -> u32 {
        self.get_application_room_count_calls
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn pause_next_get_total_room_count_for_test(&self) {
        self.pause_get_total_room_count
            .store(true, std::sync::atomic::Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_paused_get_total_room_count_for_test(&self) {
        self.get_total_room_count_reached.notified().await;
    }

    #[cfg(test)]
    pub(crate) fn release_paused_get_total_room_count_for_test(&self) {
        self.release_get_total_room_count.notify_one();
    }

    /// Rotation-stall determinism gate: used only by repository-only test
    /// modules (`moderation_tests`), so it is gated out of packaged builds
    /// together with them.
    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn pause_next_update_room_code_for_test(&self) {
        self.pause_update_room_code
            .store(true, std::sync::atomic::Ordering::Release);
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) async fn wait_for_paused_update_room_code_for_test(&self) {
        self.update_room_code_reached.notified().await;
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn release_paused_update_room_code_for_test(&self) {
        self.release_update_room_code.notify_one();
    }

    #[cfg(test)]
    pub(crate) fn reset_get_room_by_id_calls_for_test(&self) {
        self.get_room_by_id_calls
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn get_room_by_id_calls_for_test(&self) -> u32 {
        self.get_room_by_id_calls
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn pause_next_get_room_by_id_for_test(&self) {
        self.pause_get_room_by_id
            .store(true, std::sync::atomic::Ordering::Release);
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn pause_next_add_player_for_test(&self) {
        self.pause_add_player_to_room
            .store(true, std::sync::atomic::Ordering::Release);
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn pause_next_get_room_players_for_test(&self) {
        // The arming is process-global: the next `get_room_players` call
        // from ANY task consumes it. Deterministic use requires a test with
        // no concurrent `get_room_players` callers (joins, ready toggles,
        // and finalize paths read membership through the room coordinator).
        self.pause_get_room_players
            .store(true, std::sync::atomic::Ordering::Release);
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) async fn wait_for_paused_get_room_players_for_test(&self) {
        self.get_room_players_reached.notified().await;
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn release_paused_get_room_players_for_test(&self) {
        self.release_get_room_players.notify_one();
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) async fn wait_for_paused_add_player_for_test(&self) {
        self.add_player_reached.notified().await;
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn release_paused_add_player_for_test(&self) {
        self.release_add_player.notify_one();
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_paused_get_room_by_id_for_test(&self) {
        self.get_room_by_id_reached.notified().await;
    }

    #[cfg(test)]
    pub(crate) fn release_paused_get_room_by_id_for_test(&self) {
        self.release_get_room_by_id.notify_one();
    }

    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub(crate) fn fail_remove_player_from_room_for_test(&self, fail: bool) {
        self.fail_remove_player_from_room
            .store(fail, std::sync::atomic::Ordering::Relaxed);
    }

    /// Arms the one-shot "delete the room at the next membership write"
    /// injection (see the `delete_room_during_add_once` field doc).
    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn delete_room_during_next_add_for_test(&self) {
        self.delete_room_during_add_once
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn fail_update_player_name_for_test(&self, fail: bool) {
        self.fail_update_player_name
            .store(fail, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn miss_next_update_player_name_for_test(&self) {
        self.miss_update_player_name_once
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn fail_next_set_room_max_spectators_for_test(&self) {
        self.fail_set_room_max_spectators_once
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn panic_next_set_room_max_spectators_for_test(&self) {
        self.panic_set_room_max_spectators_once
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn panic_next_update_player_name_for_test(&self) {
        self.panic_update_player_name_once
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn panic_after_next_pending_insert_for_test(&self) {
        self.panic_after_pending_insert_once
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn fail_next_delete_room_for_test(&self) {
        self.fail_delete_room_once
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Arms the one-shot storage-failure injection for the next
    /// `add_player_to_room` call (see the `fail_add_player_to_room_once`
    /// field doc).
    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn fail_next_add_player_to_room_for_test(&self) {
        self.fail_add_player_to_room_once
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Arms the one-shot storage-failure injection for the next
    /// `request_room_authority` call (see the `fail_request_room_authority_once`
    /// field doc).
    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn fail_next_request_room_authority_for_test(&self) {
        self.fail_request_room_authority_once
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn pause_next_publish_room_for_test(&self) {
        self.pause_publish_room_once
            .store(true, std::sync::atomic::Ordering::Release);
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) async fn wait_for_paused_publish_room_for_test(&self) {
        self.publish_room_reached.notified().await;
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn release_paused_publish_room_for_test(&self) {
        self.release_publish_room.notify_one();
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn pause_next_update_player_connection_info_for_test(&self) {
        self.pause_update_player_connection_info_once
            .store(true, std::sync::atomic::Ordering::Release);
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) async fn wait_for_update_player_connection_info_for_test(&self) {
        self.update_player_connection_info_reached.notified().await;
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn release_update_player_connection_info_for_test(&self) {
        self.release_update_player_connection_info.notify_one();
    }

    #[cfg(test)]
    pub(crate) fn fail_remove_spectator_from_room_for_test(&self, fail: bool) {
        self.fail_remove_spectator_from_room
            .store(fail, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn pause_authority_request_after_commit_for_test(&self) {
        self.pause_authority_request_after_commit
            .store(true, std::sync::atomic::Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_authority_request_commit_for_test(&self) {
        self.authority_request_commit_reached.notified().await;
    }

    #[cfg(test)]
    pub(crate) fn release_authority_request_commit_for_test(&self) {
        self.release_authority_request_commit.notify_one();
    }

    #[cfg(test)]
    async fn pause_after_authority_request_commit_for_test(&self) {
        if self
            .pause_authority_request_after_commit
            .load(std::sync::atomic::Ordering::Acquire)
        {
            self.authority_request_commit_reached.notify_one();
            self.release_authority_request_commit.notified().await;
        }
    }

    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub(crate) fn get_room_players_calls_for_test(&self) -> u32 {
        self.get_room_players_calls
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) async fn backdate_room_activity_for_test(
        &self,
        room_id: &RoomId,
        age: chrono::Duration,
    ) {
        let mut rooms = self.rooms.write().await;
        let room = rooms.get_mut(room_id).expect("room exists");
        room.last_activity = chrono::Utc::now() - age;
        // Emulate GENUINE inactivity: both the wall-clock observability record
        // and the monotonic GC stamp move together, exactly as they would if no
        // activity had occurred for `age`. The emulated idle duration is stored
        // directly rather than subtracted from the clock, which would panic on
        // hosts whose monotonic epoch is younger than `age` (fresh CI virtual
        // machines).
        self.room_liveness_monotonic.write().await.insert(
            *room_id,
            RoomLiveness::AgedFor(age.to_std().expect("test ages are positive")),
        );
    }

    /// Move only the monotonic GC liveness stamp forward to "now", leaving the
    /// wall-clock `last_activity` untouched.
    ///
    /// Test-only emulation of a wall-clock step: production activity keeps both
    /// stamps in lockstep, so this one-sided refresh reproduces the exact state
    /// in which an NTP correction or host resume has made every wall timestamp
    /// look stale while members are monotonic-fresh.
    #[cfg(test)]
    pub(crate) async fn refresh_room_monotonic_liveness_for_test(&self, room_id: &RoomId) {
        self.room_liveness_monotonic
            .write()
            .await
            .insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));
    }
}

impl Default for InMemoryDatabase {
    fn default() -> Self {
        Self::new()
    }
}

/// A room's monotonic GC liveness state.
#[derive(Debug, Clone)]
enum RoomLiveness {
    /// Real activity: the stamp of the last observed activity instant.
    Live(tokio::time::Instant),
    /// Test-only emulation of a room that has been idle for exactly this long.
    ///
    /// Emulating age by subtracting it from the current clock would panic on
    /// hosts whose monotonic epoch is younger than the simulated age (fresh
    /// CI virtual machines), so tests store the emulated idle duration
    /// directly; production refreshes replace this state wholesale.
    #[cfg(test)]
    AgedFor(std::time::Duration),
}

impl RoomLiveness {
    /// Elapsed idle time this state represents.
    fn idle_for(&self) -> chrono::Duration {
        match self {
            Self::Live(stamp) => {
                chrono::Duration::from_std(stamp.elapsed()).unwrap_or(chrono::Duration::MAX)
            }
            #[cfg(test)]
            Self::AgedFor(idle) => {
                chrono::Duration::from_std(*idle).unwrap_or(chrono::Duration::MAX)
            }
        }
    }
}

/// Elapsed time since a room's last activity without consulting the current
/// wall clock for the decision itself.
///
/// The elapsed value comes from the room's monotonic liveness stamp when one
/// exists. Wall-clock steps (NTP correction, manual clock change, host
/// suspend/resume) must not reap occupied rooms whose members are
/// monotonic-fresh, nor keep an idle room alive because its wall-clock stamp
/// happens to look fresh; the same discipline is pinned for reconnect windows
/// and client pings.
///
/// A missing stamp falls back to the wall-clock difference so rooms created by
/// paths that predate the stamp cannot become immortal; every shipped creation
/// path stamps at insert time.
fn room_idle_for(
    activity: Option<RoomLiveness>,
    last_activity_wall: chrono::DateTime<chrono::Utc>,
) -> chrono::Duration {
    match activity {
        Some(liveness) => liveness.idle_for(),
        // Defensive only: rooms whose row exists without a liveness stamp.
        // Every shipped creation path stamps at insert time under the same
        // lock, so this arm cannot occur through the shipped API; it keeps a
        // foreign row insertion from becoming an immortal room.
        // Wall clock (durable record): the only input available for a
        // pre-stamp row.
        None => chrono::Utc::now().signed_duration_since(last_activity_wall),
    }
}

impl InMemoryDatabase {
    async fn create_room_with_state(
        &self,
        game_name: String,
        room_code: Option<String>,
        max_players: u8,
        supports_authority: bool,
        creator_id: PlayerId,
        relay_type: String,
        region_id: String,
        application_id: Option<Uuid>,
        join_password: Option<RoomPasswordCredential>,
        pending: bool,
        creation_owner: Option<PendingRoomCreation>,
    ) -> CreateRoomResult {
        let room_code =
            room_code.unwrap_or_else(crate::protocol::room_codes::generate_clean_room_code);

        // One source of truth for creator authority: the room's
        // `authority_player` and the creator's stored `is_authority` flag both
        // derive from this condition, so the wire surfaces built from them
        // (`RoomJoined.is_authority` from `authority_player`; v2
        // `current_players` / `GameStarting` peers and v3 `SessionPeer` from
        // the stored flag) can never disagree. In a `supports_authority: false`
        // room nobody — including the creator — holds authority.
        let authority_player = supports_authority.then_some(creator_id);

        // Create creator player info before acquiring locks
        let creator_info = PlayerInfo {
            id: creator_id,
            name: "Creator".to_string(), // This will be updated later when we have the actual name
            is_authority: authority_player == Some(creator_id),
            is_ready: false,
            // Wall clock (durable record): membership stamp on a durable
            // room-state row.
            connected_at: Some(chrono::Utc::now()),
            connection_info: None,
            // Room-state record, not a wire snapshot: the v3 incarnation epoch
            // is filled at snapshot-send time, so this stays `None`.
            epoch: None,
            seq: None,
            region_id: region_id.clone(),
        };

        let mut players = HashMap::new();
        players.insert(creator_id, creator_info);

        // Lock ordering: rooms first, then room_codes, then liveness
        // (consistent with delete_room, cleanup_*). All three locks are held
        // simultaneously to ensure atomicity of the room creation: no other
        // task can observe a partial state where room_codes has an entry but
        // rooms does not, and the commit cannot be torn from the monotonic
        // liveness stamp by a dropped future.
        let mut rooms = self.rooms.write().await;
        let mut room_codes = self.room_codes.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;
        let mut publication = self.room_publication.write().await;

        // Check room code uniqueness under the write lock (no TOCTOU gap)
        let game_room_key = (game_name.clone(), room_code.clone());
        if room_codes.contains_key(&game_room_key) {
            return Err(CreateRoomError::RoomCodeCollision {
                game_name,
                room_code,
            });
        }

        // Generate a unique room ID
        let room_id = {
            let mut id = uuid::Uuid::new_v4();
            let mut attempts = 0u8;
            while rooms.contains_key(&id) {
                attempts = attempts.saturating_add(1);
                if attempts >= 16 {
                    return Err(CreateRoomError::Storage(anyhow::anyhow!(
                        "Failed to generate unique room ID after {attempts} attempts"
                    )));
                }
                id = uuid::Uuid::new_v4();
            }
            id
        };

        // Wall clock (durable record): `created_at`/`last_activity` key GC
        // windows that must survive process restarts; the in-process GC
        // decision pairs them with a monotonic liveness stamp (see
        // `room_idle_for`).
        let now = chrono::Utc::now();
        let room = Room {
            id: room_id,
            game_name: game_name.clone(),
            code: room_code.clone(),
            max_players,
            supports_authority,
            players,
            authority_player,
            lobby_state: crate::protocol::LobbyState::Waiting,
            ready_players: Vec::new(),
            lobby_started_at: None,
            game_finalized_at: None,
            relay_type,
            region_id,
            application_id,
            created_at: now,
            last_activity: now,
            spectators: HashMap::new(),
            max_spectators: None,
            password: join_password,
            banned_players: HashSet::new(),
        };

        // Insert into all three maps under one set of guards, so a dropped
        // future can never commit the room without its monotonic GC stamp
        // (which would silently degrade the room to the wall-clock fallback).
        // The join password rides the same insert: the room is born sealed,
        // so no admission can observe an unlocked row (issue #525).
        rooms.insert(room_id, room.clone());
        room_codes.insert(game_room_key, room_id);
        liveness.insert(room_id, RoomLiveness::Live(tokio::time::Instant::now()));
        if pending {
            publication.insert(
                room_id,
                PendingRoomState::Creating {
                    creator_id,
                    owner: creation_owner
                        .as_ref()
                        .map(|creation| std::sync::Arc::downgrade(&creation.owner)),
                    accounting: creation_owner
                        .as_ref()
                        .map(|creation| std::sync::Arc::clone(&creation.accounting)),
                },
            );
            if let Some(creation) = creation_owner.as_ref() {
                creation.record_inserted();
            }
            #[cfg(all(test, signal_fish_repository_tests))]
            if self
                .panic_after_pending_insert_once
                .swap(false, std::sync::atomic::Ordering::Relaxed)
            {
                panic!("injected panic after pending room insertion");
            }
        }
        self.session_records.record_created(&room, !pending);

        Ok(room)
    }
}

#[async_trait]
impl GameDatabase for InMemoryDatabase {
    fn session_records(&self) -> Option<std::sync::Arc<crate::session_records::SessionRecords>> {
        Some(std::sync::Arc::clone(&self.session_records))
    }

    async fn initialize(&self) -> Result<()> {
        Ok(())
    }

    async fn create_room(
        &self,
        game_name: String,
        room_code: Option<String>,
        max_players: u8,
        supports_authority: bool,
        creator_id: PlayerId,
        relay_type: String,
        region_id: String,
        application_id: Option<Uuid>,
    ) -> Result<Room> {
        self.create_room_with_state(
            game_name,
            room_code,
            max_players,
            supports_authority,
            creator_id,
            relay_type,
            region_id,
            application_id,
            None,
            false,
            None,
        )
        .await
        .map_err(anyhow::Error::new)
    }
    async fn create_room_classified(
        &self,
        game_name: String,
        room_code: Option<String>,
        max_players: u8,
        supports_authority: bool,
        creator_id: PlayerId,
        relay_type: String,
        region_id: String,
        application_id: Option<Uuid>,
        join_password: Option<RoomPasswordCredential>,
    ) -> CreateRoomResult {
        self.create_room_with_state(
            game_name,
            room_code,
            max_players,
            supports_authority,
            creator_id,
            relay_type,
            region_id,
            application_id,
            join_password,
            false,
            None,
        )
        .await
    }

    async fn create_pending_room_classified(
        &self,
        game_name: String,
        room_code: Option<String>,
        max_players: u8,
        supports_authority: bool,
        creator_id: PlayerId,
        relay_type: String,
        region_id: String,
        application_id: Option<Uuid>,
        join_password: Option<RoomPasswordCredential>,
    ) -> CreateRoomResult {
        self.create_room_with_state(
            game_name,
            room_code,
            max_players,
            supports_authority,
            creator_id,
            relay_type,
            region_id,
            application_id,
            join_password,
            true,
            None,
        )
        .await
    }

    async fn create_pending_room_owned_classified(
        &self,
        game_name: String,
        room_code: Option<String>,
        max_players: u8,
        supports_authority: bool,
        creator_id: PlayerId,
        relay_type: String,
        region_id: String,
        application_id: Option<Uuid>,
        join_password: Option<RoomPasswordCredential>,
        creation_owner: PendingRoomCreation,
    ) -> CreateRoomResult {
        self.create_room_with_state(
            game_name,
            room_code,
            max_players,
            supports_authority,
            creator_id,
            relay_type,
            region_id,
            application_id,
            join_password,
            true,
            Some(creation_owner),
        )
        .await
    }
    async fn update_room_code(&self, room_id: &RoomId, new_code: String) -> UpdateRoomCodeResult {
        #[cfg(test)]
        if self
            .pause_update_room_code
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            self.update_room_code_reached.notify_one();
            self.release_update_room_code.notified().await;
        }
        // Lock order matches `create_room`: rooms first, then room_codes,
        // then liveness. All writes commit under one set of guards so the
        // code registry can never disagree with the stored room about which
        // code owns this id, and the rotation's activity stamps are atomic
        // with the code swap.
        let mut rooms = self.rooms.write().await;
        let mut room_codes = self.room_codes.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;

        let room = rooms.get_mut(room_id).ok_or_else(|| {
            UpdateRoomCodeError::Storage(anyhow::anyhow!(
                "Room {room_id} not found during code rotation"
            ))
        })?;

        let old_key = (room.game_name.clone(), room.code.clone());
        let new_key = (room.game_name.clone(), new_code.clone());
        if room_codes
            .get(&new_key)
            .is_some_and(|owner| owner != room_id)
        {
            return Err(UpdateRoomCodeError::RoomCodeCollision {
                game_name: room.game_name.clone(),
                room_code: new_code,
            });
        }

        // A rotation is room activity on BOTH clocks: `last_activity` is the
        // durable wall-clock record, and the monotonic liveness stamp is what
        // the in-process GC decides idle time from when one exists —
        // refreshing only the wall clock would leave the reaper working from
        // a stale instant (BUG-1 clock pairing).
        room.code = new_code;
        room.last_activity = chrono::Utc::now();
        room_codes.remove(&old_key);
        room_codes.insert(new_key, *room_id);
        liveness.insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));
        self.session_records
            .record_room_code_changed(room_id, &room.code);

        Ok(room.clone())
    }

    async fn set_room_max_spectators(
        &self,
        room_id: &RoomId,
        max_spectators: Option<u8>,
    ) -> Result<()> {
        #[cfg(all(test, signal_fish_repository_tests))]
        if self
            .panic_set_room_max_spectators_once
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            panic!("injected spectator capacity write panic for test");
        }
        #[cfg(all(test, signal_fish_repository_tests))]
        if self
            .fail_set_room_max_spectators_once
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            anyhow::bail!("injected spectator capacity write failure for test");
        }
        let mut rooms = self.rooms.write().await;
        let room = rooms.get_mut(room_id).ok_or_else(|| {
            anyhow::anyhow!("Room {room_id} not found while setting spectator cap")
        })?;
        room.max_spectators = max_spectators;
        Ok(())
    }

    async fn set_room_password(
        &self,
        room_id: &RoomId,
        password: Option<RoomPasswordCredential>,
    ) -> Result<()> {
        let mut rooms = self.rooms.write().await;
        let room = rooms.get_mut(room_id).ok_or_else(|| {
            anyhow::anyhow!("Room {room_id} not found while setting room password")
        })?;
        room.password = password;
        Ok(())
    }

    async fn set_room_ban(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        banned: bool,
    ) -> Result<()> {
        let mut rooms = self.rooms.write().await;
        let room = rooms
            .get_mut(room_id)
            .ok_or_else(|| anyhow::anyhow!("Room {room_id} not found while setting room ban"))?;
        if banned {
            room.banned_players.insert(*player_id);
        } else {
            room.banned_players.remove(player_id);
        }
        Ok(())
    }

    async fn get_room(&self, game_name: &str, room_code: &str) -> Result<Option<Room>> {
        // Lock ordering: rooms first, then room_codes (consistent with write paths)
        let rooms = self.rooms.read().await;
        let room_codes = self.room_codes.read().await;
        let publication = self.room_publication.read().await;
        let game_room_key = (game_name.to_string(), room_code.to_string());
        if let Some(room_id) = room_codes.get(&game_room_key) {
            if let Some(room) = rooms.get(room_id) {
                if !publication.contains_key(room_id) {
                    return Ok(Some(room.clone()));
                }
            }
        }
        Ok(None)
    }

    async fn get_room_by_id(&self, room_id: &RoomId) -> Result<Option<Room>> {
        #[cfg(test)]
        {
            self.get_room_by_id_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if self
                .pause_get_room_by_id
                .swap(false, std::sync::atomic::Ordering::AcqRel)
            {
                self.get_room_by_id_reached.notify_one();
                self.release_get_room_by_id.notified().await;
            }
            if self
                .fail_get_room_by_id
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                anyhow::bail!("injected get_room_by_id failure for test");
            }
        }

        let rooms = self.rooms.read().await;
        Ok(rooms.get(room_id).cloned())
    }

    /// Admit one new player while preserving every existing membership.
    /// Refresh both activity clocks under the room lock after admission.
    /// Refusals preserve the clocks and session join count.
    async fn add_player_to_room(&self, room_id: &RoomId, player: PlayerInfo) -> Result<bool> {
        #[cfg(all(test, signal_fish_repository_tests))]
        if self
            .fail_add_player_to_room_once
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            anyhow::bail!("injected add_player_to_room failure for test");
        }
        #[cfg(all(test, signal_fish_repository_tests))]
        if self
            .delete_room_during_add_once
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            let _ = self.delete_room(room_id).await;
        }
        #[cfg(all(test, signal_fish_repository_tests))]
        if self
            .pause_add_player_to_room
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            self.add_player_reached.notify_one();
            self.release_add_player.notified().await;
        }
        let mut rooms = self.rooms.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            if room.add_player(player) {
                self.session_records.record_player_joined(room_id);
                // A join is activity: refresh the reaper clock so a room that
                // fills up long after creation is not GC'd mid-game (BUG-1).
                liveness.insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));
                Ok(true)
            } else {
                Ok(false)
            }
        } else {
            anyhow::bail!("Room not found")
        }
    }

    /// Remove membership through Room and record actual departures on the
    /// monotonic clock and session ledger. Absent members do not extend expiry.
    async fn remove_player_from_room(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
    ) -> Result<Option<PlayerInfo>> {
        #[cfg(test)]
        if self
            .fail_remove_player_from_room
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            anyhow::bail!("injected remove_player_from_room failure for test");
        }

        let mut rooms = self.rooms.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            let removed_player = room.remove_player(player_id);

            // A real departure is activity, and it starts the empty-room clock:
            // both cleanup paths time an empty room from `last_activity`, so
            // refreshing it gives a room emptied long after creation the full
            // `empty_room_timeout` window from the LAST departure rather than
            // deleting it immediately off a stale `created_at` (BUG-1). Guarded
            // on an ACTUAL removal — a no-op remove (player already gone) is not
            // activity and must not keep an otherwise-stale room alive.
            if removed_player.is_some() {
                liveness.insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));
                self.session_records.record_player_left(room_id);
            }

            Ok(removed_player)
        } else {
            Ok(None)
        }
    }

    /// Apply a validated authority change under the room membership write lock.
    async fn update_room_authority(
        &self,
        room_id: &RoomId,
        authority_player: Option<PlayerId>,
    ) -> Result<bool> {
        let mut rooms = self.rooms.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;
        let Some(room) = rooms.get_mut(room_id) else {
            return Ok(false);
        };
        let previous = room.authority_player;
        let accepted = room.set_authority(authority_player);
        if accepted && previous != room.authority_player {
            liveness.insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));
        }
        Ok(accepted)
    }

    /// Apply the authority request policy and renew both clocks on a grant or release.
    async fn request_room_authority(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        become_authority: bool,
    ) -> Result<AuthorityOutcome> {
        #[cfg(all(test, signal_fish_repository_tests))]
        if self
            .fail_request_room_authority_once
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            anyhow::bail!("injected request_room_authority failure for test");
        }
        let mut rooms = self.rooms.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            // Check if room supports authority
            if !room.supports_authority {
                return Ok(AuthorityOutcome::Denied(AuthorityDenial::NotSupported));
            }

            // Check if player exists in room
            if !room.players.contains_key(player_id) {
                return Ok(AuthorityOutcome::Denied(AuthorityDenial::NotAMember));
            }

            if become_authority {
                // REQUEST AUTHORITY CASE

                // Rule: Can only request authority if no one currently has it
                if room.authority_player.is_some() {
                    return Ok(AuthorityOutcome::Denied(AuthorityDenial::AlreadyHeld));
                }

                // Grant authority to the requesting player
                room.set_authority(Some(*player_id));
                liveness.insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));

                drop(liveness);
                drop(rooms);
                #[cfg(test)]
                self.pause_after_authority_request_commit_for_test().await;
                Ok(AuthorityOutcome::Granted)
            } else {
                // RELEASE AUTHORITY CASE

                // Rule: Can only release authority if you currently have it
                if room.authority_player != Some(*player_id) {
                    return Ok(AuthorityOutcome::Denied(AuthorityDenial::NotHeld));
                }

                // Release authority
                room.set_authority(None);
                liveness.insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));

                drop(liveness);
                drop(rooms);
                #[cfg(test)]
                self.pause_after_authority_request_commit_for_test().await;
                Ok(AuthorityOutcome::Granted)
            }
        } else {
            Ok(AuthorityOutcome::Denied(AuthorityDenial::RoomNotFound))
        }
    }

    /// Store a changed member name and renew both room activity clocks.
    async fn update_player_name(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        name: &str,
    ) -> Result<bool> {
        #[cfg(all(test, signal_fish_repository_tests))]
        if self
            .panic_update_player_name_once
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            panic!("injected player name write panic for test");
        }
        #[cfg(all(test, signal_fish_repository_tests))]
        if self
            .miss_update_player_name_once
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            return Ok(false);
        }
        #[cfg(all(test, signal_fish_repository_tests))]
        if self
            .fail_update_player_name
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            anyhow::bail!("injected update_player_name failure for test");
        }
        let mut rooms = self.rooms.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            if let Some(player) = room.players.get_mut(player_id) {
                if player.name != name {
                    name.clone_into(&mut player.name);
                    room.update_activity();
                    liveness.insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));
                }
                Ok(true)
            } else {
                Ok(false)
            }
        } else {
            Ok(false)
        }
    }

    /// Store changed peer metadata and renew both room activity clocks.
    async fn update_player_connection_info(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        connection_info: ConnectionInfo,
    ) -> Result<bool> {
        #[cfg(all(test, signal_fish_repository_tests))]
        if self
            .pause_update_player_connection_info_once
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            self.update_player_connection_info_reached.notify_one();
            self.release_update_player_connection_info.notified().await;
        }
        let mut rooms = self.rooms.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            if let Some(player) = room.players.get_mut(player_id) {
                if player.connection_info.as_ref() != Some(&connection_info) {
                    player.connection_info = Some(connection_info);
                    room.update_activity();
                    liveness.insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));
                }
                Ok(true)
            } else {
                Ok(false)
            }
        } else {
            Ok(false)
        }
    }

    /// Clear retained peer metadata and renew activity only when metadata was present.
    async fn clear_player_connection_info(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
    ) -> Result<bool> {
        let mut rooms = self.rooms.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;
        let Some(room) = rooms.get_mut(room_id) else {
            return Ok(false);
        };
        let Some(player) = room.players.get_mut(player_id) else {
            return Ok(false);
        };
        if player.connection_info.take().is_some() {
            room.update_activity();
            liveness.insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));
        }
        Ok(true)
    }

    async fn get_room_players(&self, room_id: &RoomId) -> Result<Vec<PlayerInfo>> {
        #[cfg(test)]
        self.get_room_players_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        #[cfg(all(test, signal_fish_repository_tests))]
        // The one-shot pause precedes the failure injection: an armed pause
        // is an observation point, so the caller resumes before a co-armed
        // `fail_get_room_players` fails the same call.
        if self
            .pause_get_room_players
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            self.get_room_players_reached.notify_one();
            self.release_get_room_players.notified().await;
        }

        #[cfg(test)]
        if self
            .fail_get_room_players
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            anyhow::bail!("injected get_room_players failure for test");
        }

        let rooms = self.rooms.read().await;
        if let Some(room) = rooms.get(room_id) {
            Ok(room.players.values().cloned().collect())
        } else {
            Ok(Vec::new())
        }
    }

    /// Remove expired empty published rooms, preserving protected rooms and all live indexes.
    async fn cleanup_empty_rooms(
        &self,
        empty_timeout: chrono::Duration,
        protected: &HashSet<RoomId>,
    ) -> Result<Vec<RoomId>> {
        let mut rooms = self.rooms.write().await;
        let mut room_codes = self.room_codes.write().await;

        let effective_timeout = if empty_timeout <= chrono::Duration::zero() {
            chrono::Duration::zero()
        } else {
            empty_timeout
        };
        let mut liveness = self.room_liveness_monotonic.write().await;
        let publication = self.room_publication.read().await;

        let mut deleted_ids = Vec::new();
        for (room_id, room) in rooms.extract_if(|room_id, room| {
            let expired = !publication.contains_key(room_id)
                && !room.has_occupants()
                && !protected.contains(room_id)
                && room_idle_for(liveness.get(room_id).cloned(), room.last_activity)
                    > effective_timeout;
            if expired {
                liveness.remove(room_id);
            }
            expired
        }) {
            room_codes.remove(&(room.game_name, room.code));
            self.session_records
                .record_closed(&room_id, crate::session_records::SessionCloseReason::Empty);
            deleted_ids.push(room_id);
        }

        Ok(deleted_ids)
    }

    /// Extract unprotected published rooms after their empty or occupied idle timeout.
    async fn cleanup_expired_rooms(
        &self,
        empty_timeout: chrono::Duration,
        inactive_timeout: chrono::Duration,
        protected: &HashSet<RoomId>,
    ) -> Result<RoomCleanupOutcome> {
        let mut rooms = self.rooms.write().await;
        let mut room_codes = self.room_codes.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;
        let publication = self.room_publication.read().await;

        let mut outcome = RoomCleanupOutcome::default();
        for (room_id, room) in rooms.extract_if(|room_id, room| {
            if protected.contains(room_id) || publication.contains_key(room_id) {
                return false;
            }
            let idle_for = room_idle_for(liveness.get(room_id).cloned(), room.last_activity);
            let expired = idle_for
                > if room.has_occupants() {
                    inactive_timeout
                } else {
                    empty_timeout
                };
            if expired {
                liveness.remove(room_id);
            }
            expired
        }) {
            let was_empty = !room.has_occupants();
            room_codes.remove(&(room.game_name, room.code));
            self.session_records.record_closed(
                &room_id,
                crate::session_records::SessionCloseReason::Expired,
            );

            if was_empty {
                outcome.empty_rooms_cleaned = outcome.empty_rooms_cleaned.saturating_add(1);
            } else {
                outcome.inactive_rooms_cleaned = outcome.inactive_rooms_cleaned.saturating_add(1);
            }
        }

        Ok(outcome)
    }

    async fn update_room_activity(&self, room_id: &RoomId) -> Result<()> {
        let mut rooms = self.rooms.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            room.last_activity = chrono::Utc::now();
            liveness.insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));
        }
        Ok(())
    }

    async fn delete_room(&self, room_id: &RoomId) -> Result<bool> {
        #[cfg(all(test, signal_fish_repository_tests))]
        if self
            .fail_delete_room_once
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            anyhow::bail!("injected room deletion failure for test");
        }
        // All three guards are acquired before any mutation: a dropped future
        // can only cancel while waiting for a lock, never between the primary
        // commit and the liveness stamp (see the dropped-delete-room test).
        let mut rooms = self.rooms.write().await;
        let mut room_codes = self.room_codes.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;
        let mut publication = self.room_publication.write().await;

        if let Some(room) = rooms.remove(room_id) {
            self.session_records
                .record_closed(room_id, crate::session_records::SessionCloseReason::Deleted);
            let accounting = publication.get(room_id).and_then(|state| match state {
                PendingRoomState::Creating { accounting, .. }
                | PendingRoomState::Abandoned { accounting, .. } => accounting.clone(),
            });
            let game_room_key = (room.game_name.clone(), room.code);
            room_codes.remove(&game_room_key);
            liveness.remove(room_id);
            publication.remove(room_id);
            if let Some(accounting) = accounting {
                accounting.record_creator_left();
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn publish_room(&self, room_id: &RoomId) -> Result<()> {
        #[cfg(all(test, signal_fish_repository_tests))]
        if self
            .pause_publish_room_once
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            self.publish_room_reached.notify_one();
            self.release_publish_room.notified().await;
        }
        let rooms = self.rooms.read().await;
        if !rooms.contains_key(room_id) {
            anyhow::bail!("Room {room_id} disappeared before publication");
        }
        let mut publication = self.room_publication.write().await;
        if matches!(
            publication.get(room_id),
            Some(PendingRoomState::Abandoned { .. })
        ) {
            anyhow::bail!("Room {room_id} was abandoned before publication");
        }
        publication.remove(room_id);
        self.session_records.record_published(room_id);
        Ok(())
    }

    async fn abandon_room(&self, room_id: &RoomId, creator_id: &PlayerId) -> Result<()> {
        let rooms = self.rooms.read().await;
        let room = rooms
            .get(room_id)
            .ok_or_else(|| anyhow::anyhow!("Room {room_id} disappeared before abandonment"))?;
        if room.players.len() != 1 || !room.players.contains_key(creator_id) {
            anyhow::bail!("Room {room_id} gained another member before abandonment");
        }
        // Publication may have started inside the response builder but its
        // queue commit can still fail. The caller holds the room lane until
        // rollback finishes, so revoke lookup visibility before detaching.
        let mut publication = self.room_publication.write().await;
        let accounting = publication.get(room_id).and_then(|state| match state {
            PendingRoomState::Creating { accounting, .. }
            | PendingRoomState::Abandoned { accounting, .. } => accounting.clone(),
        });
        publication.insert(
            *room_id,
            PendingRoomState::Abandoned {
                creator_id: *creator_id,
                accounting,
            },
        );
        Ok(())
    }

    async fn pending_room_ids(&self) -> Result<Vec<RoomId>> {
        let publication = self.room_publication.read().await;
        Ok(publication
            .iter()
            .filter_map(|(id, state)| match state {
                PendingRoomState::Abandoned { .. } => Some(*id),
                PendingRoomState::Creating {
                    owner: Some(owner), ..
                } if owner.upgrade().is_none() => Some(*id),
                PendingRoomState::Creating { .. } => None,
            })
            .collect())
    }

    async fn is_room_published(&self, room_id: &RoomId) -> Result<bool> {
        let rooms = self.rooms.read().await;
        let publication = self.room_publication.read().await;
        Ok(rooms.contains_key(room_id) && !publication.contains_key(room_id))
    }

    async fn delete_room_if_pending(&self, room_id: &RoomId) -> Result<bool> {
        #[cfg(all(test, signal_fish_repository_tests))]
        if self
            .fail_delete_room_once
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            anyhow::bail!("injected room deletion failure for test");
        }
        let mut rooms = self.rooms.write().await;
        let mut room_codes = self.room_codes.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;
        let mut publication = self.room_publication.write().await;
        let Some(room) = rooms.get(room_id) else {
            return Ok(false);
        };
        let Some(state) = publication.get(room_id) else {
            return Ok(false);
        };
        let (creator_id, accounting) = match state {
            PendingRoomState::Creating {
                creator_id,
                owner,
                accounting,
            } => {
                if owner.as_ref().is_none_or(|owner| owner.upgrade().is_some()) {
                    return Ok(false);
                }
                (creator_id, accounting.clone())
            }
            PendingRoomState::Abandoned {
                creator_id,
                accounting,
            } => (creator_id, accounting.clone()),
        };
        if room.players.len() > 1 || room.players.keys().any(|player_id| player_id != creator_id) {
            return Ok(false);
        }
        let key = (room.game_name.clone(), room.code.clone());
        rooms.remove(room_id);
        room_codes.remove(&key);
        liveness.remove(room_id);
        publication.remove(room_id);
        self.session_records
            .record_closed(room_id, crate::session_records::SessionCloseReason::Deleted);
        if let Some(accounting) = accounting {
            accounting.record_creator_left();
        }
        Ok(true)
    }

    async fn get_game_room_count(&self, game_name: &str) -> Result<usize> {
        let rooms = self.rooms.read().await;
        let count = rooms
            .values()
            .filter(|room| room.game_name == game_name)
            .count();
        Ok(count)
    }

    async fn get_application_room_count(&self, application_id: &Uuid) -> Result<usize> {
        #[cfg(test)]
        {
            self.get_application_room_count_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if self
                .pause_get_application_room_count
                .swap(false, std::sync::atomic::Ordering::AcqRel)
            {
                self.get_application_room_count_reached.notify_one();
                self.release_get_application_room_count.notified().await;
            }
            if self
                .fail_get_application_room_count
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                anyhow::bail!("injected application room count failure for test");
            }
        }
        let rooms = self.rooms.read().await;
        Ok(rooms
            .values()
            .filter(|room| room.application_id == Some(*application_id))
            .count())
    }

    async fn get_total_room_count(&self) -> Result<usize> {
        #[cfg(test)]
        if self
            .pause_get_total_room_count
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            self.get_total_room_count_reached.notify_one();
            self.release_get_total_room_count.notified().await;
        }
        let rooms = self.rooms.read().await;
        Ok(rooms.len())
    }

    async fn health_check(&self) -> bool {
        true
    }

    async fn update_player_last_seen(&self, _player_id: &PlayerId) -> Result<()> {
        // In-memory DB has no per-player last_seen tracking; no-op
        Ok(())
    }

    async fn get_rooms_by_game(&self) -> Result<HashMap<String, usize>> {
        let rooms = self.rooms.read().await;
        let mut game_counts: HashMap<String, usize> = HashMap::new();

        for room in rooms.values() {
            if let Some(count) = game_counts.get_mut(room.game_name.as_str()) {
                *count = count.saturating_add(1);
            } else {
                game_counts.insert(room.game_name.clone(), 1);
            }
        }

        Ok(game_counts)
    }

    async fn get_player_count_percentiles(&self) -> Result<HashMap<String, f64>> {
        let counts = {
            let rooms = self.rooms.read().await;
            let mut counts = PlayerCountDistribution::default();
            for room in rooms.values() {
                counts.record(room.players.len());
            }
            counts
        };
        Ok(counts.percentiles())
    }

    async fn get_game_player_percentiles(&self) -> Result<HashMap<String, HashMap<String, f64>>> {
        let counts = {
            let rooms = self.rooms.read().await;
            let mut counts: HashMap<String, PlayerCountDistribution> = HashMap::new();
            for room in rooms.values() {
                if let Some(distribution) = counts.get_mut(room.game_name.as_str()) {
                    distribution.record(room.players.len());
                } else {
                    let mut distribution = PlayerCountDistribution::default();
                    distribution.record(room.players.len());
                    counts.insert(room.game_name.clone(), distribution);
                }
            }
            counts
        };
        Ok(counts
            .into_iter()
            .map(|(game, counts)| (game, counts.percentiles()))
            .collect())
    }

    /// Enter an occupied waiting room into its lobby and renew both clocks once.
    async fn transition_room_to_lobby(&self, room_id: &RoomId) -> Result<()> {
        let mut rooms = self.rooms.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            if room.enter_lobby() {
                liveness.insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));
            }
        }
        Ok(())
    }

    /// Toggle a lobby member's readiness and renew both clocks on acceptance.
    async fn toggle_player_ready(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
    ) -> Result<Option<(crate::protocol::LobbyState, Vec<PlayerId>, bool)>> {
        let mut rooms = self.rooms.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            if room.lobby_state == crate::protocol::LobbyState::Lobby {
                let Some(player) = room.players.get(player_id) else {
                    return Ok(None);
                };
                room.set_player_ready(player_id, !player.is_ready);
                liveness.insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));

                let all_ready = room.all_players_ready();
                return Ok(Some((
                    room.lobby_state.clone(),
                    room.ready_players.clone(),
                    all_ready,
                )));
            }
        }
        Ok(None)
    }

    /// Finalize the matching room snapshot and renew both clocks only for the winner.
    async fn finalize_room_game(
        &self,
        room_id: &RoomId,
        expected: &FinalizeRoomGameExpectation,
    ) -> Result<FinalizeRoomGameOutcome> {
        let mut rooms = self.rooms.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;
        let room = rooms
            .get_mut(room_id)
            .ok_or_else(|| anyhow::anyhow!("room not found while finalizing game"))?;
        if room.lobby_state == crate::protocol::LobbyState::Finalized {
            return Ok(FinalizeRoomGameOutcome::AlreadyFinalized);
        }
        let mut expected_members = expected.members.clone();
        expected_members.sort_unstable();
        if expected_members.len() != room.players.len()
            || expected_members
                .windows(2)
                .any(|pair| matches!(pair, [left, right] if left == right))
            || expected_members
                .iter()
                .any(|member| !room.players.contains_key(member))
            || room.authority_player != expected.authority_player
            || room.lobby_state != expected.lobby_state
        {
            return Ok(FinalizeRoomGameOutcome::SnapshotChanged);
        }

        // The caller is the policy authority that determined every player is
        // ready. Ready state is coordinated separately, so synchronize the
        // persisted player flags as part of the same one-time transition.
        let member_ids: Vec<PlayerId> = room.players.keys().copied().collect();
        for player in room.players.values_mut() {
            player.is_ready = true;
        }
        room.ready_players = member_ids;
        room.lobby_state = crate::protocol::LobbyState::Finalized;
        // Wall clock (durable record): absolute lifecycle stamp.
        room.game_finalized_at = Some(chrono::Utc::now());
        room.update_activity();
        liveness.insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));
        Ok(FinalizeRoomGameOutcome::Finalized)
    }

    async fn add_spectator_to_room(
        &self,
        room_id: &RoomId,
        spectator: SpectatorInfo,
    ) -> Result<bool> {
        let mut rooms = self.rooms.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            let admitted = room.add_spectator(spectator);
            if admitted {
                // A spectator join is activity: `Room::add_spectator` already
                // refreshed the wall-clock record, so the monotonic GC stamp
                // must move in lockstep.
                liveness.insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));
                self.session_records.record_spectator_joined(room_id);
            }
            Ok(admitted)
        } else {
            Ok(false)
        }
    }

    async fn remove_spectator_from_room(
        &self,
        room_id: &RoomId,
        spectator_id: &PlayerId,
    ) -> Result<Option<SpectatorInfo>> {
        #[cfg(test)]
        if self
            .fail_remove_spectator_from_room
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            anyhow::bail!("injected remove_spectator_from_room failure for test");
        }

        let mut rooms = self.rooms.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            let removed = room.remove_spectator(spectator_id);
            if removed.is_some() {
                // A real departure is activity and starts the empty-room clock:
                // refresh the monotonic stamp alongside the wall-clock record.
                liveness.insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));
                self.session_records.record_spectator_left(room_id);
            }
            Ok(removed)
        } else {
            Ok(None)
        }
    }

    async fn get_room_spectators(&self, room_id: &RoomId) -> Result<Vec<SpectatorInfo>> {
        let rooms = self.rooms.read().await;
        if let Some(room) = rooms.get(room_id) {
            Ok(room.get_spectators())
        } else {
            Ok(Vec::new())
        }
    }

    async fn set_room_application_id(&self, room_id: &RoomId, application_id: Uuid) -> Result<()> {
        #[cfg(test)]
        if self
            .fail_set_room_application_id
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            anyhow::bail!("injected room application persistence failure for test");
        }
        let mut rooms = self.rooms.write().await;
        let room = rooms
            .get_mut(room_id)
            .ok_or_else(|| anyhow::anyhow!("Room not found"))?;
        room.application_id = Some(application_id);
        self.session_records
            .record_application_id_changed(room_id, Some(application_id));
        Ok(())
    }

    async fn clear_room_application_id(&self, room_id: &RoomId) -> Result<()> {
        #[cfg(test)]
        if self
            .fail_clear_room_application_id
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            anyhow::bail!("injected room application clear failure for test");
        }
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            room.application_id = None;
            self.session_records
                .record_application_id_changed(room_id, None);
        }
        Ok(())
    }

    async fn clear_room_application_id_if_matches(
        &self,
        room_id: &RoomId,
        application_id: Uuid,
    ) -> Result<bool> {
        #[cfg(test)]
        if self
            .fail_clear_room_application_id
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            anyhow::bail!("injected room application clear failure for test");
        }
        let mut rooms = self.rooms.write().await;
        let Some(room) = rooms.get_mut(room_id) else {
            return Ok(false);
        };
        if room.application_id != Some(application_id) {
            return Ok(false);
        }
        room.application_id = None;
        self.session_records
            .record_application_id_changed(room_id, None);
        Ok(true)
    }

    async fn try_claim_room_cleanup(
        &self,
        room_id: &RoomId,
        cleanup_type: &str,
        instance_id: &uuid::Uuid,
    ) -> Result<bool> {
        let mut cleanup_events = self.cleanup_events.write().await;

        // A claim blocks re-cleanup of the same room for the reclaim window,
        // then expires so a room that is re-created and becomes empty again
        // can be cleaned once more. Monotonic decision input: a wall-clock
        // step must not revive or shorten the dedupe window.
        let cleanup_id = format!("{room_id}:{cleanup_type}");
        let now = tokio::time::Instant::now();
        match cleanup_events.entry(cleanup_id) {
            std::collections::hash_map::Entry::Vacant(e) => {
                // We claimed it
                e.insert(CleanupEventEntry {
                    instance_id: *instance_id,
                    claimed_at: now,
                });
                Ok(true)
            }
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let elapsed = now.saturating_duration_since(e.get().claimed_at);
                if chrono_elapsed(elapsed) >= CLEANUP_RECLAIM_WINDOW {
                    // The previous claim's window has passed: take over the
                    // claim and restart the window.
                    e.insert(CleanupEventEntry {
                        instance_id: *instance_id,
                        claimed_at: now,
                    });
                    Ok(true)
                } else {
                    // Already processed by this instance within the window
                    Ok(false)
                }
            }
        }
    }

    async fn cleanup_old_room_cleanup_events(&self) -> Result<u64> {
        let mut cleanup_events = self.cleanup_events.write().await;
        let now = tokio::time::Instant::now();

        let initial_count = cleanup_events.len();
        cleanup_events.retain(|_, entry| {
            let elapsed = now.saturating_duration_since(entry.claimed_at);
            chrono_elapsed(elapsed) < CLEANUP_EVENT_RETENTION
        });
        let deleted_count = initial_count.saturating_sub(cleanup_events.len());

        Ok(u64::try_from(deleted_count).unwrap_or(u64::MAX))
    }

    async fn live_room_counts(&self) -> Option<LiveRoomCounts> {
        let rooms = self.rooms.read().await;
        let publications = self.room_publication.read().await;
        Some(LiveRoomCounts {
            rooms: u64::try_from(rooms.len()).unwrap_or(u64::MAX),
            occupants: u64::try_from(
                rooms
                    .values()
                    .map(|room| room.players.len().saturating_add(room.spectators.len()))
                    .fold(0usize, |total, occupants| total.saturating_add(occupants)),
            )
            .unwrap_or(u64::MAX),
            pending_publications: u64::try_from(publications.len()).unwrap_or(u64::MAX),
        })
    }

    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }
}

const PERCENTILE_SCALE: usize = 1_000;

/// Exact occupancy ranks retain one counter per distinct room size.
#[derive(Default)]
struct PlayerCountDistribution {
    frequencies: std::collections::BTreeMap<usize, usize>,
    rooms: usize,
}

impl PlayerCountDistribution {
    fn record(&mut self, players: usize) {
        let count = self.frequencies.entry(players).or_default();
        *count = count.saturating_add(1);
        self.rooms = self.rooms.saturating_add(1);
    }

    fn percentile(&self, per_mille: usize) -> f64 {
        let mut rank = percentile_index(self.rooms.saturating_sub(1), per_mille);
        for (&players, &count) in &self.frequencies {
            if rank < count {
                return players as f64;
            }
            rank = rank.saturating_sub(count);
        }
        0.0
    }

    fn percentiles(&self) -> HashMap<String, f64> {
        if self.rooms == 0 {
            return HashMap::new();
        }
        [
            ("p50", 500),
            ("p90", 900),
            ("p99", 990),
            ("p99_5", 995),
            ("p99_9", 999),
            ("p100", 1000),
        ]
        .into_iter()
        .map(|(name, rank)| (name.to_string(), self.percentile(rank)))
        .collect()
    }
}

#[cfg(test)]
fn percentile(sorted_values: &[usize], per_mille: usize) -> f64 {
    if sorted_values.is_empty() {
        return 0.0;
    }

    let max_index = sorted_values.len().saturating_sub(1);
    let index = percentile_index(max_index, per_mille);
    let value = sorted_values
        .get(index.min(max_index))
        .copied()
        .unwrap_or_default();
    value as f64
}

fn percentile_index(max_index: usize, per_mille: usize) -> usize {
    let per_mille = per_mille.min(PERCENTILE_SCALE);
    let whole = (max_index / PERCENTILE_SCALE).saturating_mul(per_mille);
    let remainder = max_index % PERCENTILE_SCALE;
    let rounded_remainder = remainder
        .saturating_mul(per_mille)
        .saturating_add(PERCENTILE_SCALE / 2)
        / PERCENTILE_SCALE;
    whole.saturating_add(rounded_remainder).min(max_index)
}

#[cfg(all(test, signal_fish_repository_tests))]
mod member_mutation_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ErrorCode;
    use std::collections::HashSet;
    use std::sync::Arc;

    /// The server-wide room-cap pause hooks must stay usable from the
    /// packaged crate's own test surface (their other caller lives in a test
    /// file excluded from packaging), and `get_total_room_count` must report
    /// the exact live-room row count.
    #[tokio::test]
    async fn total_room_count_reports_exact_rows_and_supports_pauses() {
        let database = Arc::new(InMemoryDatabase::new());
        database
            .create_room(
                "count-game".to_string(),
                Some("COUNT1".to_string()),
                4,
                true,
                crate::protocol::PlayerId::new_v4(),
                "udp".to_string(),
                "region-a".to_string(),
                None,
            )
            .await
            .expect("fixture room created");

        database.pause_next_get_total_room_count_for_test();
        let paused_database = Arc::clone(&database);
        let read_task = tokio::spawn(async move { paused_database.get_total_room_count().await });
        database
            .wait_for_paused_get_total_room_count_for_test()
            .await;
        database.release_paused_get_total_room_count_for_test();
        assert_eq!(
            read_task
                .await
                .expect("count task completes")
                .expect("total count reads"),
            1,
            "one live room row must count as one"
        );
    }

    #[test]
    fn test_percentile_index_rounds_without_overflow() {
        let cases = [
            (0, 500, 0, "empty index range"),
            (1, 500, 1, "half rounds upward"),
            (9, 500, 5, "median of ten values"),
            (999, 999, 998, "p99.9 boundary"),
            (usize::MAX, 1_000, usize::MAX, "maximum rank"),
            (usize::MAX, usize::MAX, usize::MAX, "percentile clamps"),
        ];

        for (max_index, per_mille, expected, description) in cases {
            assert_eq!(
                percentile_index(max_index, per_mille),
                expected,
                "{description}: max_index={max_index}, per_mille={per_mille}"
            );
        }
    }

    #[test]
    fn test_percentile_uses_exact_integer_ranks() {
        let values = [0, 10, 20, 30, 40, 50, 60, 70, 80, 90];
        assert_eq!(percentile(&values, 500), 50.0);
        assert_eq!(percentile(&values, 900), 80.0);
        assert_eq!(percentile(&values, 999), 90.0);
        assert_eq!(percentile(&[], 999), 0.0);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn occupancy_distribution_matches_sorted_ranks_for_repeated_and_extreme_sizes() {
        for sizes in [
            vec![],
            vec![0],
            vec![usize::MAX],
            vec![0, 255, 256, usize::MAX],
            (0..1024).map(|index| (index * 17) % 256).collect(),
        ] {
            let mut distribution = PlayerCountDistribution::default();
            for &size in &sizes {
                distribution.record(size);
            }
            let mut sorted = sizes.clone();
            sorted.sort_unstable();
            for rank in 0..=1001 {
                assert_eq!(
                    distribution.percentile(rank),
                    percentile(&sorted, rank),
                    "sizes={}, rank={rank}",
                    sizes.len()
                );
            }
            assert_eq!(
                distribution.frequencies.len(),
                sizes.iter().collect::<HashSet<_>>().len()
            );
        }
        let mut repeated = PlayerCountDistribution::default();
        for _ in 0..100_000 {
            repeated.record(16);
        }
        assert_eq!(repeated.frequencies.len(), 1);
        assert_eq!(repeated.percentile(999), 16.0);
    }

    #[tokio::test]
    async fn game_metrics_preserve_exact_counts_and_percentiles() {
        let database = InMemoryDatabase::new();
        assert!(database.get_rooms_by_game().await.unwrap().is_empty());
        assert!(database
            .get_player_count_percentiles()
            .await
            .unwrap()
            .is_empty());
        assert!(database
            .get_game_player_percentiles()
            .await
            .unwrap()
            .is_empty());
        for (index, (game, count)) in [("水", 0), ("水", 2), ("水", 4), ("水", 1), ("other", 3)]
            .into_iter()
            .enumerate()
        {
            let room = create_test_room(&database, game, &format!("MET{index:03}"))
                .await
                .unwrap();
            let mut rooms = database.rooms.write().await;
            let room = rooms.get_mut(&room.id).unwrap();
            room.players.clear();
            for _ in 0..count {
                let player = member("metrics");
                room.players.insert(player.id, player);
            }
        }
        assert_eq!(
            database.get_rooms_by_game().await.unwrap(),
            HashMap::from([("水".to_string(), 4), ("other".to_string(), 1),])
        );
        let overall = database.get_player_count_percentiles().await.unwrap();
        let per_game = database.get_game_player_percentiles().await.unwrap();
        for (key, global, water) in [
            ("p50", 2.0, 2.0),
            ("p90", 4.0, 4.0),
            ("p99", 4.0, 4.0),
            ("p99_5", 4.0, 4.0),
            ("p99_9", 4.0, 4.0),
            ("p100", 4.0, 4.0),
        ] {
            assert_eq!(overall.get(key), Some(&global), "overall {key}");
            assert_eq!(per_game["水"].get(key), Some(&water), "water {key}");
            assert_eq!(per_game["other"].get(key), Some(&3.0), "other {key}");
        }
        assert_eq!(overall.len(), 6);
        assert_eq!(per_game.len(), 2);
        assert!(per_game.values().all(|values| values.len() == 6));
    }

    // NOTE: These tests run under Miri. `InMemoryDatabase::create_room` calls
    // `chrono::Utc::now()` (which invokes `clock_gettime(CLOCK_REALTIME)`), but the
    // Miri job runs with `-Zmiri-disable-isolation`, so that syscall is serviced
    // rather than aborting the run. The `tokio::spawn`-based concurrency tests below
    // also run under Miri (the default `#[tokio::test]` current-thread runtime is
    // interpretable) — a useful place for Miri's data-race detection.

    /// Helper: create a room with the given game name and room code using sensible defaults.
    pub(super) async fn create_test_room(
        db: &InMemoryDatabase,
        game_name: &str,
        room_code: &str,
    ) -> Result<Room> {
        db.create_room(
            game_name.to_string(),
            Some(room_code.to_string()),
            4,
            true,
            Uuid::new_v4(),
            "relay".to_string(),
            "us-east-1".to_string(),
            None,
        )
        .await
    }

    /// A `delete_room` future dropped while parked on a contended lock must
    /// not tear the primary commit from the liveness-stamp removal: every
    /// guard is acquired before any mutation, so a dropped future leaves the
    /// room and its stamp exactly as they were. An orphaned liveness entry
    /// keyed by a deleted room has no sweep and would otherwise live forever.
    #[tokio::test]
    async fn dropped_delete_room_cannot_tear_the_primary_commit_from_the_liveness_stamp() {
        let db = Arc::new(InMemoryDatabase::new());
        let room = create_test_room(&db, "torn_delete", "DROP01")
            .await
            .expect("room created");
        let room_id = room.id;

        let gate = db.room_liveness_monotonic.write().await;
        let task_db = Arc::clone(&db);
        let task = tokio::spawn(async move { task_db.delete_room(&room_id).await });
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        task.abort();
        drop(gate);

        assert!(
            db.rooms.read().await.contains_key(&room_id),
            "a dropped delete_room must not commit the rooms removal"
        );
        assert!(
            db.room_liveness_monotonic
                .read()
                .await
                .contains_key(&room_id),
            "a dropped delete_room must not remove the liveness stamp"
        );
    }

    /// A `create_room` future dropped while parked on a contended lock must
    /// not commit a room whose monotonic GC stamp was never written: that
    /// torn state silently degrades the room to the wall-clock GC fallback.
    #[tokio::test]
    async fn dropped_create_room_cannot_commit_a_room_without_its_liveness_stamp() {
        let db = Arc::new(InMemoryDatabase::new());

        let gate = db.room_liveness_monotonic.write().await;
        let task_db = Arc::clone(&db);
        let task =
            tokio::spawn(async move { create_test_room(&task_db, "torn_create", "DROP02").await });
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        task.abort();
        drop(gate);

        assert!(
            db.rooms.read().await.is_empty(),
            "a dropped create_room must not commit the room row"
        );
        assert!(
            db.room_codes.read().await.is_empty(),
            "a dropped create_room must not commit the room code"
        );
        assert!(
            db.room_liveness_monotonic.read().await.is_empty(),
            "a dropped create_room must not leave the stamp unwritten behind a committed room"
        );
    }

    /// The cleanup-claim dedupe runs entirely on monotonic tokio time: a
    /// claim blocks re-cleanup for `CLEANUP_RECLAIM_WINDOW` (inclusive
    /// expiry: at exactly `claim + window` the window is closed), then
    /// expires so a re-created room can be cleaned again; the maintenance
    /// prune forgets entries once they reach `CLEANUP_EVENT_RETENTION`.
    /// Paused time proves the boundaries without any real waiting — and
    /// would fail against a wall-clock implementation, since advancing tokio
    /// time does not move the wall clock.
    #[tokio::test(start_paused = true)]
    async fn cleanup_claim_and_prune_boundaries_run_on_monotonic_time() {
        let db = InMemoryDatabase::new();
        let room_id = Uuid::new_v4();
        let instance = Uuid::new_v4();
        let window = CLEANUP_RECLAIM_WINDOW.to_std().expect("positive window");
        let retention = CLEANUP_EVENT_RETENTION
            .to_std()
            .expect("positive retention");

        // First claim wins.
        assert!(db
            .try_claim_room_cleanup(&room_id, "empty_cleanup", &instance)
            .await
            .expect("first claim succeeds"));

        // One tick inside the window a re-claim is blocked; exactly at the
        // boundary the window has expired and the claim is taken over.
        tokio::time::advance(window - std::time::Duration::from_secs(1)).await;
        assert!(
            !db.try_claim_room_cleanup(&room_id, "empty_cleanup", &instance)
                .await
                .expect("re-claim inside the window is blocked"),
            "the same cleanup must stay claimed until the window elapses"
        );
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        assert!(
            db.try_claim_room_cleanup(&room_id, "empty_cleanup", &instance)
                .await
                .expect("re-claim at the window boundary succeeds"),
            "a room re-created and re-emptied after the window must be cleanable again"
        );

        // Different cleanup types and rooms never interfere; they are claimed
        // one tick before the first entry's window boundary to keep the ages
        // distinct for the prune assertions below.
        tokio::time::advance(window - std::time::Duration::from_secs(1)).await;
        assert!(db
            .try_claim_room_cleanup(&room_id, "inactive_cleanup", &instance)
            .await
            .expect("a different cleanup type claims independently"));
        assert!(db
            .try_claim_room_cleanup(&Uuid::new_v4(), "empty_cleanup", &instance)
            .await
            .expect("a different room claims independently"));

        // The retention prune forgets an entry exactly when it reaches the
        // retention age (inclusive expiry), and nothing before that. Advance
        // so the takeover entry is exactly retention-old while the two fresh
        // claims are one window younger.
        tokio::time::advance(retention - (window - std::time::Duration::from_secs(1))).await;
        let deleted = db
            .cleanup_old_room_cleanup_events()
            .await
            .expect("prune succeeds");
        assert_eq!(
            deleted, 1,
            "exactly the entry that reached the retention boundary is forgotten"
        );

        // The remaining claims age out the same way; pruning is idempotent.
        tokio::time::advance(window).await;
        let deleted = db
            .cleanup_old_room_cleanup_events()
            .await
            .expect("prune succeeds");
        assert_eq!(
            deleted, 2,
            "both remaining entries reach retention and are forgotten together"
        );
        let deleted = db
            .cleanup_old_room_cleanup_events()
            .await
            .expect("prune succeeds");
        assert_eq!(deleted, 0, "pruning is idempotent");
    }

    #[test]
    fn authority_denials_map_to_their_exact_wire_contract() {
        for (denial, reason, error_code) in [
            (
                AuthorityDenial::NotSupported,
                "Room does not support authority",
                ErrorCode::AuthorityNotSupported,
            ),
            (
                AuthorityDenial::AlreadyHeld,
                "Another player already has authority",
                ErrorCode::AuthorityConflict,
            ),
            (
                AuthorityDenial::NotAMember,
                "Player not found in room",
                ErrorCode::AuthorityDenied,
            ),
            (
                AuthorityDenial::NotHeld,
                "You do not have authority to release",
                ErrorCode::AuthorityDenied,
            ),
            (
                AuthorityDenial::RoomNotFound,
                "Room not found",
                ErrorCode::RoomNotFound,
            ),
            (
                AuthorityDenial::StorageError,
                "Storage error",
                ErrorCode::StorageError,
            ),
        ] {
            assert_eq!(denial.reason(), reason, "{denial:?} reason");
            assert_eq!(denial.error_code(), error_code, "{denial:?} code");
        }
    }

    #[tokio::test]
    async fn test_create_room_generates_unique_ids() {
        let db = InMemoryDatabase::new();
        let mut ids = HashSet::new();
        let count = 100;

        for i in 0..count {
            let room_code = format!("ROOM{i:03}");
            let room = create_test_room(&db, "uniqueness_game", &room_code)
                .await
                .expect("room creation should succeed");
            ids.insert(room.id);
        }

        assert_eq!(
            ids.len(),
            count,
            "all {count} room IDs must be distinct, but only {} unique IDs found",
            ids.len()
        );
    }

    #[tokio::test]
    async fn test_create_room_id_is_retrievable_by_id() {
        let db = InMemoryDatabase::new();
        let room = create_test_room(&db, "lookup_game", "LOOK01")
            .await
            .expect("room creation should succeed");

        let fetched = db
            .get_room_by_id(&room.id)
            .await
            .expect("get_room_by_id should not error")
            .expect("room should exist in the rooms map");

        assert_eq!(fetched.id, room.id);
        assert_eq!(fetched.code, room.code);
        assert_eq!(fetched.game_name, room.game_name);
    }

    #[tokio::test]
    async fn test_create_room_room_code_collision_rejected() {
        let db = InMemoryDatabase::new();

        create_test_room(&db, "game1", "TEST01")
            .await
            .expect("first room creation should succeed");

        let result = create_test_room(&db, "game1", "TEST01").await;
        assert!(
            result.is_err(),
            "duplicate room code for the same game must be rejected"
        );

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("already exists"),
            "error message should contain 'already exists', got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn room_code_collision_has_typed_classification() {
        let db = InMemoryDatabase::new();
        create_test_room(&db, "typed_collision", "TAKEN1")
            .await
            .expect("fixture room should be created");

        let error = db
            .create_room_classified(
                "typed_collision".to_string(),
                Some("TAKEN1".to_string()),
                4,
                true,
                Uuid::new_v4(),
                "relay".to_string(),
                "us-east-1".to_string(),
                None,
                None,
            )
            .await
            .expect_err("duplicate code should be classified");

        assert!(matches!(
            error,
            CreateRoomError::RoomCodeCollision {
                ref game_name,
                ref room_code,
            } if game_name == "typed_collision" && room_code == "TAKEN1"
        ));
    }

    #[tokio::test]
    async fn classified_direct_creation_is_visible_on_return() {
        let db = InMemoryDatabase::new();
        let room = db
            .create_room_classified(
                "classified_direct".into(),
                Some("DIRECT".into()),
                4,
                true,
                Uuid::new_v4(),
                "relay".into(),
                "us-east-1".into(),
                None,
                None,
            )
            .await
            .expect("classified direct creation succeeds");
        assert_eq!(
            db.get_room("classified_direct", "DIRECT")
                .await
                .expect("room lookup succeeds")
                .map(|visible| visible.id),
            Some(room.id)
        );
        assert!(db.pending_room_ids().await.unwrap().is_empty());
    }

    /// Regression #1298: refused grants preserve the current authority and room state.
    #[tokio::test]
    async fn test_update_room_authority_absent_member_preserves_current_authority() {
        for supports_authority in [false, true] {
            let db = InMemoryDatabase::new();
            let creator = Uuid::new_v4();
            let room = db
                .create_room(
                    "authority-membership".into(),
                    Some("AUTH01".into()),
                    4,
                    supports_authority,
                    creator,
                    "relay".into(),
                    "region".into(),
                    None,
                )
                .await
                .unwrap();
            let peer = member("present peer");
            assert!(db.add_player_to_room(&room.id, peer.clone()).await.unwrap());
            let spectator = spectator("observer");
            assert!(db
                .add_spectator_to_room(&room.id, spectator.clone())
                .await
                .unwrap());
            let departed = member("departed peer");
            assert!(db
                .add_player_to_room(&room.id, departed.clone())
                .await
                .unwrap());
            assert!(db
                .remove_player_from_room(&room.id, &departed.id)
                .await
                .unwrap()
                .is_some());

            for target in [Uuid::new_v4(), spectator.id, departed.id] {
                let before = db.get_room_by_id(&room.id).await.unwrap().unwrap();
                assert!(
                    !db.update_room_authority(&room.id, Some(target))
                        .await
                        .unwrap(),
                    "supports_authority={supports_authority}, target={target}"
                );
                let after = db.get_room_by_id(&room.id).await.unwrap().unwrap();
                assert_eq!(after.authority_player, before.authority_player);
                assert_eq!(
                    serde_json::to_value(&after.players).unwrap(),
                    serde_json::to_value(&before.players).unwrap()
                );
                assert_eq!(after.last_activity, before.last_activity);
                assert_eq!(after.ready_players, before.ready_players);
            }
            assert_eq!(
                db.update_room_authority(&room.id, None).await.unwrap(),
                supports_authority
            );
            assert!(!db
                .update_room_authority(&room.id, Some(departed.id))
                .await
                .unwrap());
            assert_eq!(
                db.get_room_by_id(&room.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .authority_player,
                None
            );
            for target in [Some(peer.id), Some(peer.id), None, Some(creator)] {
                assert_eq!(
                    db.update_room_authority(&room.id, target).await.unwrap(),
                    supports_authority
                );
                let after = db.get_room_by_id(&room.id).await.unwrap().unwrap();
                assert_eq!(
                    after.authority_player,
                    if supports_authority { target } else { None }
                );
                for player in after.players.values() {
                    assert_eq!(
                        player.is_authority,
                        after.authority_player == Some(player.id)
                    );
                }
            }
            assert!(!db
                .update_room_authority(&Uuid::new_v4(), Some(creator))
                .await
                .unwrap());
        }
    }

    // --- BUG-1: room lifecycle GC (activity refresh + reconnection-aware GC) ---

    fn member(name: &str) -> PlayerInfo {
        PlayerInfo {
            id: Uuid::new_v4(),
            name: name.to_string(),
            is_authority: false,
            is_ready: false,
            connected_at: Some(chrono::Utc::now()),
            connection_info: None,
            epoch: None,
            seq: None,
            region_id: "us-east-1".to_string(),
        }
    }

    fn spectator(name: &str) -> SpectatorInfo {
        SpectatorInfo {
            id: Uuid::new_v4(),
            name: name.to_string(),
            connected_at: Some(chrono::Utc::now()),
        }
    }

    /// Regression #1304: a refused second role must not count as activity or a join.
    #[tokio::test(start_paused = true)]
    async fn cross_role_admission_preserves_membership_clocks_and_session_counts() {
        for spectator_first in [false, true] {
            let db = InMemoryDatabase::new();
            let room = create_test_room(&db, "exclusive_roles", "ROLE01")
                .await
                .unwrap();
            let player = member("member");
            let mut watcher = spectator("watcher");
            watcher.id = player.id;
            if spectator_first {
                assert!(db
                    .add_spectator_to_room(&room.id, watcher.clone())
                    .await
                    .unwrap());
            } else {
                assert!(db
                    .add_player_to_room(&room.id, player.clone())
                    .await
                    .unwrap());
            }
            let before = db.get_room_by_id(&room.id).await.unwrap().unwrap();
            let RoomLiveness::Live(activity) = db.room_liveness_monotonic.read().await[&room.id]
            else {
                panic!("admitted room must have a live clock");
            };
            let joined = db.session_records.snapshot().active[0].clone();
            tokio::time::advance(std::time::Duration::from_secs(10)).await;
            let admitted = if spectator_first {
                db.add_player_to_room(&room.id, player.clone())
                    .await
                    .unwrap()
            } else {
                db.add_spectator_to_room(&room.id, watcher.clone())
                    .await
                    .unwrap()
            };
            assert!(!admitted, "spectator_first={spectator_first}");
            let after = db.get_room_by_id(&room.id).await.unwrap().unwrap();
            assert_eq!(after.last_activity, before.last_activity);
            assert_eq!(after.authority_player, before.authority_player);
            assert_eq!(after.ready_players, before.ready_players);
            assert_eq!(
                serde_json::to_value(after.players).unwrap(),
                serde_json::to_value(before.players).unwrap()
            );
            assert_eq!(
                serde_json::to_value(after.spectators).unwrap(),
                serde_json::to_value(before.spectators).unwrap()
            );
            assert!(
                matches!(db.room_liveness_monotonic.read().await[&room.id], RoomLiveness::Live(current) if current == activity)
            );
            let refused = db.session_records.snapshot().active[0].clone();
            assert_eq!(refused.players_joined, joined.players_joined);
            assert_eq!(refused.spectators_joined, joined.spectators_joined);
            if spectator_first {
                assert!(db
                    .remove_spectator_from_room(&room.id, &player.id)
                    .await
                    .unwrap()
                    .is_some());
                assert!(db.add_player_to_room(&room.id, player).await.unwrap());
            } else {
                assert!(db
                    .remove_player_from_room(&room.id, &player.id)
                    .await
                    .unwrap()
                    .is_some());
                assert!(db.add_spectator_to_room(&room.id, watcher).await.unwrap());
            }
        }
    }

    /// Regression #1304: both roles compete for one membership under the room lock.
    #[tokio::test]
    async fn concurrent_cross_role_admission_has_one_winner_and_one_join_record() {
        for spectator_first in [false, true] {
            let db = InMemoryDatabase::new();
            let room = create_test_room(&db, "exclusive_role_race", "ROLE02")
                .await
                .unwrap();
            let player = member("member");
            let mut watcher = spectator("watcher");
            watcher.id = player.id;
            let id = player.id;
            let room_guard = db.rooms.write().await;
            let player_join = db.add_player_to_room(&room.id, player);
            let spectator_join = db.add_spectator_to_room(&room.id, watcher);
            tokio::pin!(player_join, spectator_join);
            if spectator_first {
                assert!(futures_util::poll!(&mut spectator_join).is_pending());
                assert!(futures_util::poll!(&mut player_join).is_pending());
            } else {
                assert!(futures_util::poll!(&mut player_join).is_pending());
                assert!(futures_util::poll!(&mut spectator_join).is_pending());
            }
            drop(room_guard);
            let (player_joined, spectator_joined) = if spectator_first {
                let (spectator_result, player_result) =
                    tokio::join!(biased; spectator_join, player_join);
                (player_result.unwrap(), spectator_result.unwrap())
            } else {
                let (player_result, spectator_result) =
                    tokio::join!(biased; player_join, spectator_join);
                (player_result.unwrap(), spectator_result.unwrap())
            };
            assert_ne!(player_joined, spectator_joined);
            assert_eq!(spectator_joined, spectator_first);
            let room = db.get_room_by_id(&room.id).await.unwrap().unwrap();
            assert_eq!(room.players.contains_key(&id), player_joined);
            assert_eq!(room.spectators.contains_key(&id), spectator_joined);
            let record = &db.session_records.snapshot().active[0];
            assert_eq!(record.players_joined, 1 + u64::from(player_joined));
            assert_eq!(record.spectators_joined, u64::from(spectator_joined));
        }
    }

    /// Backdate a room's timestamps so it looks stale to the GC without waiting
    /// wall-clock time. Reaches the in-memory map directly (same module).
    pub(super) async fn age_room(db: &InMemoryDatabase, room_id: &RoomId, age: chrono::Duration) {
        let now = chrono::Utc::now();
        {
            let mut rooms = db.rooms.write().await;
            let room = rooms.get_mut(room_id).expect("room exists");
            room.created_at = now - age;
            room.last_activity = now - age;
        }
        // Emulate genuine aging: the monotonic GC stamp moves in lockstep so
        // cleanup decisions see a room that has truly been idle for `age`.
        // The emulated idle duration is stored directly rather than subtracted
        // from the clock, which would panic on hosts with a young monotonic
        // epoch.
        let mut liveness = db.room_liveness_monotonic.write().await;
        liveness.insert(
            *room_id,
            RoomLiveness::AgedFor(age.to_std().expect("test ages are positive")),
        );
    }

    fn is_fresh(ts: chrono::DateTime<chrono::Utc>) -> bool {
        chrono::Utc::now().signed_duration_since(ts) < chrono::Duration::minutes(1)
    }

    #[tokio::test]
    /// A refused spectator retry must preserve its snapshot and join count.
    async fn duplicate_spectator_admission_preserves_snapshot_and_counts() {
        for cap in [None, Some(1), Some(2)] {
            let db = InMemoryDatabase::new();
            let room = create_test_room(&db, "duplicate_spectator", "DUP003")
                .await
                .unwrap();
            db.rooms
                .write()
                .await
                .get_mut(&room.id)
                .unwrap()
                .max_spectators = cap;
            let watcher = spectator("original");
            assert!(db
                .add_spectator_to_room(&room.id, watcher.clone())
                .await
                .unwrap());
            let before = db.get_room_by_id(&room.id).await.unwrap().unwrap();
            let mut duplicate = watcher.clone();
            duplicate.name = "replacement".to_string();
            assert!(
                !db.add_spectator_to_room(&room.id, duplicate).await.unwrap(),
                "cap={cap:?}"
            );
            let after = db.get_room_by_id(&room.id).await.unwrap().unwrap();
            assert_eq!(
                serde_json::to_value(after.spectators).unwrap(),
                serde_json::to_value(before.spectators).unwrap()
            );
            assert_eq!(after.last_activity, before.last_activity);
            assert_eq!(db.session_records.snapshot().active[0].spectators_joined, 1);
            assert!(db
                .remove_spectator_from_room(&room.id, &watcher.id)
                .await
                .unwrap()
                .is_some());
            assert!(db.add_spectator_to_room(&room.id, watcher).await.unwrap());
            assert_eq!(db.session_records.snapshot().active[0].spectators_joined, 2);
        }
    }

    #[tokio::test]
    /// Duplicate admission futures must share one successful membership.
    async fn concurrent_duplicate_admission_adds_exactly_one_membership() {
        let db = InMemoryDatabase::new();
        let room = create_test_room(&db, "duplicate_race", "DUP002")
            .await
            .unwrap();
        let player = member("one membership");
        let duplicate = player.clone();
        let (first, second) = tokio::join!(
            db.add_player_to_room(&room.id, player),
            db.add_player_to_room(&room.id, duplicate)
        );
        assert_ne!(first.unwrap(), second.unwrap());
        assert_eq!(db.get_room_players(&room.id).await.unwrap().len(), 2);
        assert_eq!(db.session_records.snapshot().active[0].players_joined, 2);
    }

    #[tokio::test]
    /// Spare capacity must not let a retry replace an existing player.
    async fn duplicate_player_admission_preserves_member_and_session_counts() {
        let db = InMemoryDatabase::new();
        let room = create_test_room(&db, "duplicate_member", "DUP001")
            .await
            .unwrap();
        let creator = room.players.values().next().unwrap().clone();
        for full in [false, true] {
            if full {
                for _ in 1..room.max_players {
                    assert!(db
                        .add_player_to_room(&room.id, member("peer"))
                        .await
                        .unwrap());
                }
            }
            let before = db.get_room_by_id(&room.id).await.unwrap().unwrap();
            let counts = db.session_records.snapshot();
            let mut duplicate = creator.clone();
            duplicate.name = "replacement".to_string();
            duplicate.connection_info = None;
            assert!(
                !db.add_player_to_room(&room.id, duplicate).await.unwrap(),
                "full={full}"
            );
            let after = db.get_room_by_id(&room.id).await.unwrap().unwrap();
            assert_eq!(
                serde_json::to_value(&after.players).unwrap(),
                serde_json::to_value(&before.players).unwrap()
            );
            assert_eq!(after.players.len(), before.players.len());
            assert_eq!(after.last_activity, before.last_activity);
            assert_eq!(after.authority_player, before.authority_player);
            assert_eq!(after.ready_players, before.ready_players);
            assert_eq!(
                db.session_records.snapshot().active[0].players_joined,
                counts.active[0].players_joined
            );
        }
        assert!(db
            .remove_player_from_room(&room.id, &creator.id)
            .await
            .unwrap()
            .is_some());
        assert!(db.add_player_to_room(&room.id, creator).await.unwrap());
    }

    /// A join must refresh `last_activity`: without it, a room that fills up
    /// long after creation keeps a stale timestamp and is reaped mid-game
    /// (`inactive_room_timeout` measured from creation, BUG-1 corollary A).
    #[tokio::test]
    async fn add_player_to_room_refreshes_last_activity() {
        let db = InMemoryDatabase::new();
        let room = create_test_room(&db, "activity_game", "ACT001")
            .await
            .expect("room creation should succeed");
        age_room(&db, &room.id, chrono::Duration::hours(2)).await;

        assert!(db
            .add_player_to_room(&room.id, member("Joiner"))
            .await
            .expect("add_player_to_room should not error"));

        let after = db
            .get_room_by_id(&room.id)
            .await
            .expect("get_room_by_id should not error")
            .expect("room exists");
        assert!(
            is_fresh(after.last_activity),
            "joining must refresh last_activity so an active room is not reaped mid-game"
        );
    }

    /// A departure must refresh `last_activity`: it is activity AND it starts
    /// the empty-room clock, so a long-lived room that empties gets the full
    /// `empty_room_timeout` window from the last departure (BUG-1 corollary B).
    #[tokio::test]
    async fn remove_player_from_room_refreshes_last_activity() {
        let db = InMemoryDatabase::new();
        let room = create_test_room(&db, "activity_game", "ACT002")
            .await
            .expect("room creation should succeed");
        let creator = *room.players.keys().next().expect("creator present");
        age_room(&db, &room.id, chrono::Duration::hours(2)).await;

        db.remove_player_from_room(&room.id, &creator)
            .await
            .expect("remove_player_from_room should not error");

        let after = db
            .get_room_by_id(&room.id)
            .await
            .expect("get_room_by_id should not error")
            .expect("room exists");
        assert!(
            is_fresh(after.last_activity),
            "a departure must refresh last_activity (starts the empty-room clock)"
        );
    }

    #[tokio::test]
    async fn test_spectator_mutation_refreshes_room_activity_issue_241() {
        let db = InMemoryDatabase::new();
        let room = create_test_room(&db, "activity_game", "ACT003")
            .await
            .expect("room creation should succeed");
        let spectator = spectator("Watcher");

        age_room(&db, &room.id, chrono::Duration::hours(2)).await;
        assert!(db
            .add_spectator_to_room(&room.id, spectator.clone())
            .await
            .expect("spectator join should not error"));
        let joined = db
            .get_room_by_id(&room.id)
            .await
            .expect("room lookup should not error")
            .expect("room exists");
        assert!(
            is_fresh(joined.last_activity),
            "spectator join must refresh room activity"
        );

        age_room(&db, &room.id, chrono::Duration::hours(2)).await;
        db.remove_spectator_from_room(&room.id, &spectator.id)
            .await
            .expect("spectator detach should not error")
            .expect("spectator should be removed");
        let detached = db
            .get_room_by_id(&room.id)
            .await
            .expect("room lookup should not error")
            .expect("room exists");
        assert!(
            is_fresh(detached.last_activity),
            "spectator detach must refresh the empty-room clock"
        );
    }

    /// A connected spectator is a live room occupant. Neither GC sweep may
    /// delete the room merely because its seated-player set is empty; once the
    /// spectator leaves, the normal empty-room clock applies again.
    #[tokio::test]
    async fn test_room_cleanup_with_spectator_preserves_then_reaps_after_detach_issue_241() {
        #[derive(Clone, Copy)]
        enum Sweep {
            Empty,
            Expired,
        }

        for sweep in [Sweep::Empty, Sweep::Expired] {
            let db = InMemoryDatabase::new();
            let room = create_test_room(&db, "spectator_gc", "SPGC01")
                .await
                .expect("room creation should succeed");
            let creator = *room.players.keys().next().expect("creator present");
            let spectator = spectator("Watcher");
            assert!(db
                .add_spectator_to_room(&room.id, spectator.clone())
                .await
                .expect("spectator join should not error"));
            db.remove_player_from_room(&room.id, &creator)
                .await
                .expect("player departure should not error");
            let occupied_age = match sweep {
                Sweep::Empty => chrono::Duration::hours(2),
                Sweep::Expired => chrono::Duration::minutes(10),
            };
            age_room(&db, &room.id, occupied_age).await;

            match sweep {
                Sweep::Empty => {
                    let deleted = db
                        .cleanup_empty_rooms(chrono::Duration::seconds(300), &HashSet::new())
                        .await
                        .expect("empty cleanup should not error");
                    assert!(deleted.is_empty(), "spectator-occupied room was deleted");
                }
                Sweep::Expired => {
                    let outcome = db
                        .cleanup_expired_rooms(
                            chrono::Duration::seconds(300),
                            chrono::Duration::seconds(3600),
                            &HashSet::new(),
                        )
                        .await
                        .expect("expired cleanup should not error");
                    assert!(
                        outcome.is_empty(),
                        "spectator-occupied room was classified as expired"
                    );
                }
            }

            assert!(
                db.get_room_by_id(&room.id)
                    .await
                    .expect("room lookup should not error")
                    .is_some(),
                "connected spectator must keep the room durable"
            );

            db.remove_spectator_from_room(&room.id, &spectator.id)
                .await
                .expect("spectator detach should not error")
                .expect("spectator should be removed");
            age_room(&db, &room.id, chrono::Duration::hours(2)).await;
            match sweep {
                Sweep::Empty => {
                    let deleted = db
                        .cleanup_empty_rooms(chrono::Duration::seconds(300), &HashSet::new())
                        .await
                        .expect("post-detach empty cleanup should not error");
                    assert_eq!(deleted, vec![room.id]);
                }
                Sweep::Expired => {
                    let outcome = db
                        .cleanup_expired_rooms(
                            chrono::Duration::seconds(300),
                            chrono::Duration::seconds(3600),
                            &HashSet::new(),
                        )
                        .await
                        .expect("post-detach expired cleanup should not error");
                    assert_eq!(outcome.empty_rooms_cleaned, 1);
                    assert_eq!(outcome.inactive_rooms_cleaned, 0);
                }
            }
        }
    }

    #[tokio::test]
    async fn test_expired_cleanup_reaps_inactive_spectator_room_issue_241() {
        let db = InMemoryDatabase::new();
        let room = create_test_room(&db, "spectator_gc", "SPGC02")
            .await
            .expect("room creation should succeed");
        let spectator = spectator("Inactive Watcher");
        assert!(db
            .add_spectator_to_room(&room.id, spectator)
            .await
            .expect("spectator join should not error"));
        age_room(&db, &room.id, chrono::Duration::hours(2)).await;

        let outcome = db
            .cleanup_expired_rooms(
                chrono::Duration::seconds(300),
                chrono::Duration::seconds(3600),
                &HashSet::new(),
            )
            .await
            .expect("expired cleanup should not error");

        assert_eq!(outcome.empty_rooms_cleaned, 0);
        assert_eq!(outcome.inactive_rooms_cleaned, 1);
        assert!(db
            .get_room_by_id(&room.id)
            .await
            .expect("room lookup should not error")
            .is_none());
    }

    /// Both GC sweeps must spare a stale empty room whose id is `protected`
    /// (it still holds a valid reconnection record), and must delete it when
    /// unprotected. Data-driven over the two sweeps × {protected, not}.
    #[tokio::test]
    async fn cleanup_sweeps_spare_reconnection_protected_rooms() {
        enum Sweep {
            Empty,
            Expired,
        }
        let cases = [
            (Sweep::Empty, true, true),
            (Sweep::Empty, false, false),
            (Sweep::Expired, true, true),
            (Sweep::Expired, false, false),
        ];

        for (sweep, protect, expect_survives) in cases {
            let db = InMemoryDatabase::new();
            let room = create_test_room(&db, "gc_game", "GC0001")
                .await
                .expect("room creation should succeed");
            let creator = *room.players.keys().next().expect("creator present");
            db.remove_player_from_room(&room.id, &creator)
                .await
                .expect("remove should not error");
            // Age past both the 300s empty and 3600s inactive timeouts.
            age_room(&db, &room.id, chrono::Duration::hours(2)).await;

            let mut protected = HashSet::new();
            if protect {
                protected.insert(room.id);
            }

            match sweep {
                Sweep::Empty => {
                    db.cleanup_empty_rooms(chrono::Duration::seconds(300), &protected)
                        .await
                        .expect("cleanup_empty_rooms should not error");
                }
                Sweep::Expired => {
                    db.cleanup_expired_rooms(
                        chrono::Duration::seconds(300),
                        chrono::Duration::seconds(3600),
                        &protected,
                    )
                    .await
                    .expect("cleanup_expired_rooms should not error");
                }
            }

            let survives = db
                .get_room_by_id(&room.id)
                .await
                .expect("get_room_by_id should not error")
                .is_some();
            assert_eq!(
                survives, expect_survives,
                "protect={protect}: a reconnection-protected room must survive GC"
            );
        }
    }

    /// A wall-clock step (NTP correction, manual clock change, host
    /// suspend/resume) must not reap an occupied room whose members are
    /// monotonic-fresh. The stale-looking wall stamp is exactly what every
    /// member's continued activity leaves behind after the step: production
    /// refreshes both stamps in lockstep, so only the monotonic one can decide
    /// liveness. Pinned for both sweeps.
    #[tokio::test]
    async fn wall_clock_step_cannot_reap_monotonic_fresh_occupied_room() {
        for empty_only in [false, true] {
            let db = InMemoryDatabase::new();
            let room = create_test_room(&db, "wall_step", "WSTEP01")
                .await
                .expect("room creation should succeed");
            // Emulate genuine idleness first (both stamps rewind), then real
            // ongoing activity after a forward wall step (only the monotonic
            // stamp returns to fresh).
            db.backdate_room_activity_for_test(&room.id, chrono::Duration::hours(2))
                .await;
            db.refresh_room_monotonic_liveness_for_test(&room.id).await;

            if empty_only {
                let deleted = db
                    .cleanup_empty_rooms(chrono::Duration::seconds(300), &HashSet::new())
                    .await
                    .expect("cleanup_empty_rooms should not error");
                assert!(
                    deleted.is_empty(),
                    "a wall-clock step must not delete a monotonic-fresh room"
                );
            } else {
                let outcome = db
                    .cleanup_expired_rooms(
                        chrono::Duration::seconds(300),
                        chrono::Duration::seconds(3600),
                        &HashSet::new(),
                    )
                    .await
                    .expect("cleanup_expired_rooms should not error");
                assert!(
                    outcome.is_empty(),
                    "a wall-clock step must not classify a monotonic-fresh room as inactive"
                );
            }
            assert!(db.get_room_by_id(&room.id).await.expect("lookup").is_some());
        }
    }

    /// The inverse contract: genuinely idle rooms are reaped by elapsed
    /// monotonic time even though their wall-clock stamps look fresh, which is
    /// exactly the state a backward wall-clock step (or suspended host clock)
    /// produces.
    #[tokio::test(start_paused = true)]
    async fn monotonic_idle_rooms_are_reaped_despite_fresh_wall_stamps() {
        let db = InMemoryDatabase::new();
        let room = create_test_room(&db, "mono_reap", "MREAP01")
            .await
            .expect("room creation should succeed");
        let creator = *room.players.keys().next().expect("creator present");
        db.remove_player_from_room(&room.id, &creator)
            .await
            .expect("departure should not error");

        tokio::time::advance(std::time::Duration::from_secs(7200)).await;

        let deleted = db
            .cleanup_empty_rooms(chrono::Duration::seconds(300), &HashSet::new())
            .await
            .expect("cleanup_empty_rooms should not error");
        assert_eq!(
            deleted,
            vec![room.id],
            "monotonic-idle empty rooms must be reclaimed"
        );
        assert!(db.get_room_by_id(&room.id).await.expect("lookup").is_none());

        let second = create_test_room(&db, "mono_reap", "MREAP02")
            .await
            .expect("second room creation should succeed");
        tokio::time::advance(std::time::Duration::from_secs(7200)).await;
        let outcome = db
            .cleanup_expired_rooms(
                chrono::Duration::seconds(300),
                chrono::Duration::seconds(3600),
                &HashSet::new(),
            )
            .await
            .expect("cleanup_expired_rooms should not error");
        assert_eq!(
            outcome.inactive_rooms_cleaned, 1,
            "monotonic-idle occupied rooms must be classified inactive"
        );
        assert!(
            db.get_room_by_id(&second.id)
                .await
                .expect("lookup")
                .is_none(),
            "an occupied room past its monotonic inactivity timeout is reaped"
        );
    }

    /// Every activity path that refreshes the wall record must move the
    /// monotonic GC stamp too; otherwise a room could look active on the wall
    /// clock yet be reaped mid-game.
    #[tokio::test(start_paused = true)]
    async fn every_activity_path_refreshes_monotonic_liveness() {
        const INACTIVE_TIMEOUT: chrono::Duration = chrono::Duration::seconds(60);

        // A fresh monotonic stamp survives cleanup with no elapsed time; a
        // stale one (activity path failed to refresh) is reaped immediately.
        async fn survives_cleanup(db: &InMemoryDatabase) -> bool {
            let outcome = db
                .cleanup_expired_rooms(
                    chrono::Duration::seconds(300),
                    INACTIVE_TIMEOUT,
                    &HashSet::new(),
                )
                .await
                .expect("cleanup should not error");
            outcome.is_empty()
        }

        // Baseline: with no activity after aging, the room does NOT survive.
        {
            let db = InMemoryDatabase::new();
            let room = create_test_room(&db, "parity", "PAR0001")
                .await
                .expect("room creation should succeed");
            age_room(&db, &room.id, chrono::Duration::hours(2)).await;
            assert!(
                !survives_cleanup(&db).await,
                "aged room without activity must be reaped"
            );
        }

        // Joining a player after aging keeps the room alive.
        {
            let db = InMemoryDatabase::new();
            let room = create_test_room(&db, "parity", "PAR0002")
                .await
                .expect("room creation should succeed");
            age_room(&db, &room.id, chrono::Duration::hours(2)).await;
            let joined = crate::protocol::PlayerInfo {
                id: Uuid::new_v4(),
                name: "Joiner".to_string(),
                is_authority: false,
                is_ready: false,
                connected_at: Some(chrono::Utc::now()),
                connection_info: None,
                epoch: None,
                seq: None,
                region_id: "us-east-1".to_string(),
            };
            assert!(db
                .add_player_to_room(&room.id, joined)
                .await
                .expect("join should not error"));
            assert!(
                survives_cleanup(&db).await,
                "player join must refresh the monotonic GC stamp"
            );
        }

        // A spectator join after aging keeps the room alive...
        {
            let db = InMemoryDatabase::new();
            let room = create_test_room(&db, "parity", "PAR0003")
                .await
                .expect("room creation should succeed");
            age_room(&db, &room.id, chrono::Duration::hours(2)).await;
            let watcher = spectator("Watcher");
            let watcher_id = watcher.id;
            assert!(db
                .add_spectator_to_room(&room.id, watcher)
                .await
                .expect("spectator join should not error"));
            assert!(
                survives_cleanup(&db).await,
                "spectator join must refresh the monotonic GC stamp"
            );

            // ...and its detach starts a fresh empty-room window rather than
            // inheriting whatever age the room had before the join.
            db.remove_player_from_room(
                &room.id,
                room.players.keys().next().expect("creator present"),
            )
            .await
            .expect("creator departure should not error");
            let detached = db
                .remove_spectator_from_room(&room.id, &watcher_id)
                .await
                .expect("spectator detach should not error");
            assert!(detached.is_some(), "spectator must be removable");
            tokio::time::advance(std::time::Duration::from_secs(299)).await;
            let deleted = db
                .cleanup_empty_rooms(chrono::Duration::seconds(300), &HashSet::new())
                .await
                .expect("empty cleanup should not error");
            assert!(
                deleted.is_empty(),
                "spectator detach must start a fresh monotonic empty-room window"
            );
            tokio::time::advance(std::time::Duration::from_secs(2)).await;
            let deleted = db
                .cleanup_empty_rooms(chrono::Duration::seconds(300), &HashSet::new())
                .await
                .expect("empty cleanup should not error");
            assert_eq!(
                deleted,
                vec![room.id],
                "the empty-room window opened by the detach must eventually close"
            );
        }

        // Explicit activity refreshes keep the room alive.
        {
            let db = InMemoryDatabase::new();
            let room = create_test_room(&db, "parity", "PAR0004")
                .await
                .expect("room creation should succeed");
            age_room(&db, &room.id, chrono::Duration::hours(2)).await;
            db.update_room_activity(&room.id)
                .await
                .expect("activity update should not error");
            assert!(
                survives_cleanup(&db).await,
                "update_room_activity must refresh the monotonic GC stamp"
            );
        }
    }

    /// #1306: cancellation at the second lock must leave the room untouched.
    #[tokio::test]
    async fn cancelled_state_change_cannot_separate_room_and_expiry_commit() {
        for finalize in [false, true] {
            let db = InMemoryDatabase::new();
            let room = create_test_room(&db, "activity", "CANCEL").await.unwrap();
            let member = *room.players.keys().next().unwrap();
            db.transition_room_to_lobby(&room.id).await.unwrap();
            age_room(&db, &room.id, chrono::Duration::hours(2)).await;
            let before = db.get_room_by_id(&room.id).await.unwrap().unwrap();
            let expected = FinalizeRoomGameExpectation::from_room(&before);
            let liveness = db.room_liveness_monotonic.write().await;
            {
                let change = async {
                    if finalize {
                        db.finalize_room_game(&room.id, &expected).await.unwrap();
                    } else {
                        db.toggle_player_ready(&room.id, &member).await.unwrap();
                    }
                };
                let mut change = std::pin::pin!(change);
                assert!(futures_util::poll!(change.as_mut()).is_pending());
                assert!(
                    db.rooms.try_read().is_err(),
                    "transition holds the first lock"
                );
            }
            let after = db.get_room_by_id(&room.id).await.unwrap().unwrap();
            assert_eq!(after.last_activity, before.last_activity);
            assert_eq!(after.lobby_state, before.lobby_state);
            assert_eq!(after.ready_players, before.ready_players);
            assert_eq!(
                after.players[&member].is_ready,
                before.players[&member].is_ready
            );
            assert_eq!(after.game_finalized_at, before.game_finalized_at);
            assert!(matches!(liveness[&room.id], RoomLiveness::AgedFor(_)));
        }
    }

    /// #1306: no-op and refused transitions must not extend either clock.
    #[tokio::test(start_paused = true)]
    async fn refused_room_state_changes_preserve_expiry_clocks() {
        let db = InMemoryDatabase::new();
        let room = create_test_room(&db, "activity", "REFUSE").await.unwrap();
        let member = *room.players.keys().next().unwrap();
        db.transition_room_to_lobby(&room.id).await.unwrap();
        db.update_room_authority(&room.id, Some(member))
            .await
            .unwrap();
        let before = db.get_room_by_id(&room.id).await.unwrap().unwrap();
        let expected = FinalizeRoomGameExpectation::from_room(&before);
        for finalized in [false, true] {
            if finalized {
                assert_eq!(
                    db.finalize_room_game(&room.id, &expected).await.unwrap(),
                    FinalizeRoomGameOutcome::Finalized
                );
            }
            age_room(&db, &room.id, chrono::Duration::hours(2)).await;
            let before = db.get_room_by_id(&room.id).await.unwrap().unwrap();
            db.transition_room_to_lobby(&room.id).await.unwrap();
            assert!(db
                .update_room_authority(&room.id, Some(member))
                .await
                .unwrap());
            assert!(!db
                .update_room_authority(&room.id, Some(Uuid::nil()))
                .await
                .unwrap());
            assert_eq!(
                db.request_room_authority(&room.id, &member, true)
                    .await
                    .unwrap(),
                AuthorityOutcome::Denied(AuthorityDenial::AlreadyHeld)
            );
            assert_eq!(
                db.request_room_authority(&room.id, &Uuid::nil(), false)
                    .await
                    .unwrap(),
                AuthorityOutcome::Denied(AuthorityDenial::NotAMember)
            );
            assert!(db
                .toggle_player_ready(&room.id, &Uuid::nil())
                .await
                .unwrap()
                .is_none());
            if finalized {
                assert!(db
                    .toggle_player_ready(&room.id, &member)
                    .await
                    .unwrap()
                    .is_none());
            }
            let mut changed = expected.clone();
            changed.members.clear();
            assert_eq!(
                db.finalize_room_game(&room.id, &changed).await.unwrap(),
                if finalized {
                    FinalizeRoomGameOutcome::AlreadyFinalized
                } else {
                    FinalizeRoomGameOutcome::SnapshotChanged
                }
            );
            assert_eq!(
                db.get_room_by_id(&room.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .last_activity,
                before.last_activity
            );
            assert!(
                matches!(db.room_liveness_monotonic.read().await[&room.id], RoomLiveness::AgedFor(age) if age == std::time::Duration::from_secs(7200))
            );
        }
        let cleanup = db
            .cleanup_expired_rooms(
                chrono::Duration::seconds(300),
                chrono::Duration::hours(1),
                &HashSet::new(),
            )
            .await
            .unwrap();
        assert_eq!(cleanup.inactive_rooms_cleaned, 1);
    }

    /// #1306: each state transition starts a full, finite expiry window.
    #[tokio::test(start_paused = true)]
    async fn room_state_changes_refresh_both_expiry_clocks() {
        for operation in [
            "lobby",
            "ready",
            "authority",
            "request",
            "release",
            "finalize",
        ] {
            let db = InMemoryDatabase::new();
            let room = create_test_room(&db, "activity", "ACTIVE").await.unwrap();
            let member = *room.players.keys().next().unwrap();
            db.transition_room_to_lobby(&room.id).await.unwrap();
            if operation == "lobby" {
                db.rooms
                    .write()
                    .await
                    .get_mut(&room.id)
                    .unwrap()
                    .lobby_state = crate::protocol::LobbyState::Waiting;
            }
            db.update_room_authority(&room.id, None).await.unwrap();
            if operation == "release" {
                db.update_room_authority(&room.id, Some(member))
                    .await
                    .unwrap();
            }
            age_room(&db, &room.id, chrono::Duration::hours(2)).await;
            let before = db.get_room_by_id(&room.id).await.unwrap().unwrap();
            match operation {
                "lobby" => db.transition_room_to_lobby(&room.id).await.unwrap(),
                "ready" => assert!(db
                    .toggle_player_ready(&room.id, &member)
                    .await
                    .unwrap()
                    .is_some()),
                "authority" => assert!(db
                    .update_room_authority(&room.id, Some(member))
                    .await
                    .unwrap()),
                "request" | "release" => assert_eq!(
                    db.request_room_authority(&room.id, &member, operation == "request")
                        .await
                        .unwrap(),
                    AuthorityOutcome::Granted,
                ),
                "finalize" => assert_eq!(
                    db.finalize_room_game(
                        &room.id,
                        &FinalizeRoomGameExpectation::from_room(&before)
                    )
                    .await
                    .unwrap(),
                    FinalizeRoomGameOutcome::Finalized,
                ),
                _ => unreachable!(),
            }
            let after = db.get_room_by_id(&room.id).await.unwrap().unwrap();
            assert!(
                after.last_activity > before.last_activity,
                "{operation}: wall clock"
            );
            let cleanup = db
                .cleanup_expired_rooms(
                    chrono::Duration::seconds(300),
                    chrono::Duration::hours(1),
                    &HashSet::new(),
                )
                .await
                .unwrap();
            assert_eq!(
                cleanup.inactive_rooms_cleaned, 0,
                "{operation}: monotonic clock"
            );
            tokio::time::advance(std::time::Duration::from_secs(3601)).await;
            let cleanup = db
                .cleanup_expired_rooms(
                    chrono::Duration::seconds(300),
                    chrono::Duration::hours(1),
                    &HashSet::new(),
                )
                .await
                .unwrap();
            assert_eq!(
                cleanup.inactive_rooms_cleaned, 1,
                "{operation}: renewed window expires"
            );
        }
    }

    #[tokio::test]
    async fn test_create_room_same_code_different_game_allowed() {
        let db = InMemoryDatabase::new();

        let room1 = create_test_room(&db, "game1", "TEST01")
            .await
            .expect("room creation for game1 should succeed");

        let room2 = create_test_room(&db, "game2", "TEST01")
            .await
            .expect("room creation for game2 with same code should succeed");

        assert_ne!(
            room1.id, room2.id,
            "rooms for different games must have different IDs"
        );
        assert_eq!(room1.code, room2.code);
        assert_ne!(room1.game_name, room2.game_name);
    }

    #[tokio::test]
    async fn test_create_room_concurrent_unique_ids() {
        let db = Arc::new(InMemoryDatabase::new());
        let task_count = 50;
        let barrier = Arc::new(tokio::sync::Barrier::new(task_count));

        let mut handles = Vec::with_capacity(task_count);
        for i in 0..task_count {
            let db = Arc::clone(&db);
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                let room_code = format!("CONC{i:03}");
                db.create_room(
                    "concurrent_game".to_string(),
                    Some(room_code),
                    4,
                    true,
                    Uuid::new_v4(),
                    "relay".to_string(),
                    "us-east-1".to_string(),
                    None,
                )
                .await
            }));
        }

        let mut ids = HashSet::new();
        for handle in handles {
            let room = handle
                .await
                .expect("task should not panic")
                .expect("room creation should succeed");
            ids.insert(room.id);
        }

        assert_eq!(
            ids.len(),
            task_count,
            "all {task_count} concurrently created rooms must have unique IDs"
        );
    }

    #[tokio::test]
    async fn test_create_room_concurrent_same_code_only_one_succeeds() {
        let db = Arc::new(InMemoryDatabase::new());
        let task_count = 10;
        let barrier = Arc::new(tokio::sync::Barrier::new(task_count));

        let mut handles = Vec::with_capacity(task_count);
        for _ in 0..task_count {
            let db = Arc::clone(&db);
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                db.create_room(
                    "game1".to_string(),
                    Some("RACE01".to_string()),
                    4,
                    true,
                    Uuid::new_v4(),
                    "relay".to_string(),
                    "us-east-1".to_string(),
                    None,
                )
                .await
            }));
        }

        let mut successes = 0usize;
        let mut failures = 0usize;
        for handle in handles {
            match handle.await.expect("task should not panic") {
                Ok(_) => successes += 1,
                Err(e) => {
                    assert!(
                        e.to_string().contains("already exists"),
                        "failure reason should be 'already exists', got: {e}"
                    );
                    failures += 1;
                }
            }
        }

        assert_eq!(successes, 1, "exactly one task should win the race");
        assert_eq!(
            failures,
            task_count - 1,
            "all other tasks should fail with 'already exists'"
        );

        // Verify only one room exists in the database for this game+code
        let room = db
            .get_room("game1", "RACE01")
            .await
            .expect("get_room should not error")
            .expect("the winning room should be findable");
        assert_eq!(room.code, "RACE01");
    }

    #[tokio::test]
    async fn test_create_room_atomic_consistency() {
        let db = InMemoryDatabase::new();
        let room = create_test_room(&db, "atomic_game", "ATOM01")
            .await
            .expect("room creation should succeed");

        // Lookup via room ID
        let by_id = db
            .get_room_by_id(&room.id)
            .await
            .expect("get_room_by_id should not error")
            .expect("room should be in the rooms map");

        // Lookup via game name + room code
        let by_code = db
            .get_room("atomic_game", "ATOM01")
            .await
            .expect("get_room should not error")
            .expect("room should be in the room_codes map");

        assert_eq!(by_id.id, room.id);
        assert_eq!(by_code.id, room.id);
        assert_eq!(
            by_id.id, by_code.id,
            "both lookups must resolve to the same room"
        );
    }

    #[tokio::test]
    async fn test_room_lookup_serializes_with_publication_deletion_and_code_reuse() {
        let db = InMemoryDatabase::new();
        let room_guard = db.rooms.read().await;
        let creation = create_test_room(&db, "lookup_game", "CREATE");
        tokio::pin!(creation);
        assert!(futures_util::poll!(&mut creation).is_pending());
        let lookup = db.get_room("lookup_game", "CREATE");
        tokio::pin!(lookup);
        assert!(futures_util::poll!(&mut lookup).is_pending());
        drop(room_guard);
        let created = creation.await.unwrap();
        let observed = lookup.await.unwrap().unwrap();
        assert_eq!(observed.id, created.id);
        assert_eq!(observed.code, created.code);
        assert_eq!(observed.region_id, created.region_id);
        let pending = db
            .create_pending_room_classified(
                "lookup_game".to_string(),
                Some("LOOKUP".to_string()),
                2,
                true,
                Uuid::new_v4(),
                "auto".to_string(),
                "region-a".to_string(),
                Some(Uuid::new_v4()),
                None,
            )
            .await
            .unwrap();
        let publication_guard = db.room_publication.write().await;
        let lookup = db.get_room("lookup_game", "LOOKUP");
        tokio::pin!(lookup);
        assert!(futures_util::poll!(&mut lookup).is_pending());
        let publication = db.publish_room(&pending.id);
        tokio::pin!(publication);
        assert!(futures_util::poll!(&mut publication).is_pending());
        drop(publication_guard);
        assert!(lookup.await.unwrap().is_none());
        publication.await.unwrap();
        assert_eq!(
            db.get_room("lookup_game", "LOOKUP")
                .await
                .unwrap()
                .unwrap()
                .id,
            pending.id
        );

        let room_guard = db.rooms.read().await;
        let deletion = db.delete_room(&pending.id);
        tokio::pin!(deletion);
        assert!(futures_util::poll!(&mut deletion).is_pending());
        let lookup = db.get_room("lookup_game", "LOOKUP");
        tokio::pin!(lookup);
        assert!(futures_util::poll!(&mut lookup).is_pending());
        drop(room_guard);
        assert!(deletion.await.unwrap());
        assert!(lookup.await.unwrap().is_none());

        let replacement = create_test_room(&db, "lookup_game", "LOOKUP")
            .await
            .unwrap();
        let room_guard = db.rooms.read().await;
        let rotation = db.update_room_code(&replacement.id, "ROTATE".to_string());
        tokio::pin!(rotation);
        assert!(futures_util::poll!(&mut rotation).is_pending());
        let lookup = db.get_room("lookup_game", "LOOKUP");
        tokio::pin!(lookup);
        assert!(futures_util::poll!(&mut lookup).is_pending());
        drop(room_guard);
        rotation.await.unwrap();
        assert!(lookup.await.unwrap().is_none());
        let observed = db.get_room("lookup_game", "ROTATE").await.unwrap().unwrap();
        assert_eq!(observed.id, replacement.id);
        assert_ne!(observed.id, pending.id);
        assert_eq!(observed.code, "ROTATE");
    }

    #[tokio::test]
    async fn test_delete_room_frees_room_code() {
        let db = InMemoryDatabase::new();

        let room = create_test_room(&db, "reuse_game", "REUSE1")
            .await
            .expect("initial room creation should succeed");

        let deleted = db
            .delete_room(&room.id)
            .await
            .expect("delete_room should not error");
        assert!(
            deleted,
            "delete_room should return true for an existing room"
        );

        // The room code is now free; re-creating with the same code should work.
        let room2 = create_test_room(&db, "reuse_game", "REUSE1")
            .await
            .expect("re-creating room with freed code should succeed");

        assert_ne!(
            room.id, room2.id,
            "the new room must have a different ID than the deleted one"
        );
        assert_eq!(room2.code, "REUSE1");
    }

    #[tokio::test]
    async fn test_create_room_preserves_all_fields() {
        let db = InMemoryDatabase::new();
        let creator_id = Uuid::new_v4();
        let app_id = Uuid::new_v4();

        let room = db
            .create_room(
                "my_game".to_string(),
                Some("FIELD1".to_string()),
                8,
                true,
                creator_id,
                "webrtc".to_string(),
                "eu-west-1".to_string(),
                Some(app_id),
            )
            .await
            .expect("room creation should succeed");

        assert_eq!(room.game_name, "my_game");
        assert_eq!(room.code, "FIELD1");
        assert_eq!(room.max_players, 8);
        assert!(room.supports_authority);
        assert_eq!(room.relay_type, "webrtc");
        assert_eq!(room.region_id, "eu-west-1");
        assert_eq!(room.application_id, Some(app_id));

        // Creator should be in the players map
        assert!(
            room.players.contains_key(&creator_id),
            "creator must appear in the players map"
        );
        let creator = &room.players[&creator_id];
        assert_eq!(creator.id, creator_id);
        assert!(
            creator.is_authority,
            "creator should be marked as authority when supports_authority is true"
        );

        // Authority player should be set to creator
        assert_eq!(room.authority_player, Some(creator_id));
    }

    #[tokio::test]
    async fn test_create_room_without_authority_creator_flag_matches_authority_player() {
        // In a `supports_authority: false` room nobody holds authority:
        // `authority_player` is `None` and the creator's stored `is_authority`
        // must mirror it. (It previously seeded `true`, contradicting every
        // surface derived from `authority_player`.)
        let db = InMemoryDatabase::new();
        let creator_id = Uuid::new_v4();

        let room = db
            .create_room(
                "no_auth_game".to_string(),
                Some("NOAUT1".to_string()),
                4,
                false,
                creator_id,
                "relay".to_string(),
                "us-east-1".to_string(),
                None,
            )
            .await
            .expect("room creation should succeed");

        assert!(!room.supports_authority);
        assert_eq!(
            room.authority_player, None,
            "an authority-less room elects no authority player"
        );
        let creator = &room.players[&creator_id];
        assert!(
            !creator.is_authority,
            "the creator's stored flag must mirror authority_player (None)"
        );
    }

    // #1300: no mutation must not be reported as a successful toggle.
    #[tokio::test]
    async fn ready_toggle_refuses_absent_members_without_changing_room() {
        let database = InMemoryDatabase::new();
        let room = create_test_room(&database, "ready-member", "MEMBER")
            .await
            .unwrap();
        database.transition_room_to_lobby(&room.id).await.unwrap();
        let creator = *room.players.keys().next().unwrap();
        assert!(
            database
                .toggle_player_ready(&room.id, &creator)
                .await
                .unwrap()
                .unwrap()
                .2
        );
        let before = database.get_room_by_id(&room.id).await.unwrap().unwrap();
        assert!(database
            .toggle_player_ready(&room.id, &Uuid::new_v4())
            .await
            .unwrap()
            .is_none());
        let after = database.get_room_by_id(&room.id).await.unwrap().unwrap();
        assert_eq!(before.ready_players, after.ready_players);
        assert_eq!(before.authority_player, after.authority_player);
        assert_eq!(before.last_activity, after.last_activity);
        assert_eq!(
            serde_json::to_value(&before.players).unwrap(),
            serde_json::to_value(&after.players).unwrap()
        );
        assert!(
            !database
                .toggle_player_ready(&room.id, &creator)
                .await
                .unwrap()
                .unwrap()
                .2
        );
        assert!(database
            .toggle_player_ready(&Uuid::new_v4(), &creator)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn lobby_promotion_preserves_stored_room_readiness() {
        let database = InMemoryDatabase::new();
        let room = create_test_room(&database, "ready-promotion", "READY1")
            .await
            .expect("create room");
        let creator_id = *room.players.keys().next().expect("creator");
        {
            let mut rooms = database.rooms.write().await;
            assert!(rooms
                .get_mut(&room.id)
                .expect("stored room")
                .set_player_ready(&creator_id, true));
        }
        database
            .transition_room_to_lobby(&room.id)
            .await
            .expect("promote stored room");
        let mut snapshot = database
            .get_room_by_id(&room.id)
            .await
            .expect("read promoted room")
            .expect("room exists");
        assert_eq!(snapshot.lobby_state, crate::protocol::LobbyState::Lobby);
        assert_eq!(snapshot.ready_players, vec![creator_id]);
        assert!(snapshot.players[&creator_id].is_ready);
        assert!(snapshot.all_players_ready());
        assert!(snapshot.finalize_game());
        let started_at = snapshot.lobby_started_at;
        database
            .transition_room_to_lobby(&room.id)
            .await
            .expect("repeat promotion");
        let repeated = database
            .get_room_by_id(&room.id)
            .await
            .expect("read room again")
            .expect("room exists");
        assert_eq!(repeated.lobby_started_at, started_at);
        assert_eq!(repeated.ready_players, vec![creator_id]);
    }

    #[tokio::test]
    async fn test_remove_player_from_room_prunes_ready_players() {
        // `finalize_room_game` force-populates `ready_players` with every
        // member; removing a player must prune its id so it cannot linger in
        // `RoomJoined` / `Reconnected` payloads while the room state and the
        // remaining members' readiness stay unchanged.
        let db = InMemoryDatabase::new();
        let room = create_test_room(&db, "prune_game", "PRUNE1")
            .await
            .expect("room creation should succeed");
        let creator_id = *room
            .players
            .keys()
            .next()
            .expect("creator is in the players map");

        let member_id = Uuid::new_v4();
        let member = PlayerInfo {
            id: member_id,
            name: "Member".to_string(),
            is_authority: false,
            is_ready: false,
            connected_at: Some(chrono::Utc::now()),
            connection_info: None,
            epoch: None,
            seq: None,
            region_id: "us-east-1".to_string(),
        };
        assert!(db
            .add_player_to_room(&room.id, member)
            .await
            .expect("add_player_to_room should not error"));

        let start_snapshot = db
            .get_room_by_id(&room.id)
            .await
            .expect("room lookup should not error")
            .expect("room should exist");
        let expectation = FinalizeRoomGameExpectation::from_room(&start_snapshot);
        assert_eq!(
            db.finalize_room_game(&room.id, &expectation)
                .await
                .expect("finalize_room_game should not error"),
            FinalizeRoomGameOutcome::Finalized
        );
        assert_eq!(
            db.finalize_room_game(&room.id, &expectation)
                .await
                .expect("repeated finalize should be a normal CAS result"),
            FinalizeRoomGameOutcome::AlreadyFinalized
        );
        let finalized = db
            .get_room_by_id(&room.id)
            .await
            .expect("get_room_by_id should not error")
            .expect("room should exist");
        assert_eq!(
            finalized.lobby_state,
            crate::protocol::LobbyState::Finalized
        );
        assert!(
            finalized.ready_players.contains(&member_id),
            "finalize must populate ready_players with every member"
        );

        let removed = db
            .remove_player_from_room(&room.id, &member_id)
            .await
            .expect("remove_player_from_room should not error");
        assert!(removed.is_some(), "the member must be removed");

        let after = db
            .get_room_by_id(&room.id)
            .await
            .expect("get_room_by_id should not error")
            .expect("room should exist");
        assert!(
            !after.ready_players.contains(&member_id),
            "the departed player's id must be pruned from ready_players"
        );
        assert!(
            after.ready_players.contains(&creator_id),
            "remaining members keep their ready entries"
        );
        assert_eq!(
            after.lobby_state,
            crate::protocol::LobbyState::Finalized,
            "a departure never regresses the Finalized state"
        );
    }

    // --- Session records (issues #708, #763) ---

    fn active_session_record(
        records: &crate::session_records::SessionRecords,
        room_id: &RoomId,
    ) -> crate::session_records::SessionRecord {
        records
            .snapshot()
            .active
            .into_iter()
            .find(|record| &record.room_id == room_id)
            .expect("active session record for the room")
    }

    /// Every storage seam (create, join, leave, spectator add/remove, delete)
    /// must land exactly once on the session record, and a published room's
    /// close must move the record to the completed ring with its reason.
    #[tokio::test]
    async fn session_records_track_the_storage_lifecycle_exactly_once() {
        let db = InMemoryDatabase::new();
        let records = db
            .session_records()
            .expect("in-memory backend tracks session records");

        let room = create_test_room(&db, "session_game", "SESS01")
            .await
            .expect("room creation should succeed");
        let creator = *room.players.keys().next().expect("creator present");

        let created = active_session_record(&records, &room.id);
        assert_eq!(created.room_id, room.id);
        assert_eq!(created.room_code, "SESS01");
        assert_eq!(created.game_name, "session_game");
        assert!(created.published, "create_room rows are born visible");
        assert_eq!(
            created.players_joined, 1,
            "the creator is the room's first membership"
        );
        assert_eq!(created.ended_at_ms, None);

        assert!(db
            .add_player_to_room(&room.id, member("Joiner"))
            .await
            .expect("add_player_to_room should not error"));
        let watcher = spectator("Watcher");
        assert!(db
            .add_spectator_to_room(&room.id, watcher.clone())
            .await
            .expect("add_spectator_to_room should not error"));
        let joined = active_session_record(&records, &room.id);
        assert_eq!(joined.players_joined, 2);
        assert_eq!(joined.spectators_joined, 1);
        assert_eq!(joined.players_left, 0);

        assert!(db
            .remove_player_from_room(&room.id, &creator)
            .await
            .expect("remove_player_from_room should not error")
            .is_some());
        assert!(db
            .remove_spectator_from_room(&room.id, &watcher.id)
            .await
            .expect("remove_spectator_from_room should not error")
            .is_some());
        let departed = active_session_record(&records, &room.id);
        assert_eq!(departed.players_left, 1);
        assert_eq!(departed.spectators_left, 1);

        assert!(db
            .delete_room(&room.id)
            .await
            .expect("delete_room should not error"));
        let snapshot = records.snapshot();
        assert_eq!(snapshot.active_count, 0);
        assert_eq!(snapshot.completed_count, 1);
        let completed = &snapshot.completed[0];
        assert_eq!(
            completed.close_reason,
            Some(crate::session_records::SessionCloseReason::Deleted)
        );
        assert!(completed.ended_at_ms.is_some());
        // Counters accumulate, they do not reset at close.
        assert_eq!(completed.players_joined, 2);
        assert_eq!(completed.players_left, 1);
    }

    /// A hidden pending room is recorded unpublished, code rotation keeps the
    /// routable code current, publication flips visibility, and a pending
    /// room deleted before publication leaves no completed record (no
    /// directory ever saw it).
    #[tokio::test]
    async fn session_records_track_pending_publication_rotation_and_repair_deletion() {
        let db = InMemoryDatabase::new();
        let records = db
            .session_records()
            .expect("in-memory backend tracks session records");

        let pending = db
            .create_pending_room_classified(
                "pending_game".to_string(),
                Some("PEND01".to_string()),
                4,
                true,
                Uuid::new_v4(),
                "udp".to_string(),
                "region-a".to_string(),
                None,
                None,
            )
            .await
            .expect("pending room creation should succeed");
        let pending_record = active_session_record(&records, &pending.id);
        assert!(
            !pending_record.published,
            "a pending row is hidden until publication"
        );

        db.publish_room(&pending.id)
            .await
            .expect("publication should succeed");
        assert!(active_session_record(&records, &pending.id).published);

        db.update_room_code(&pending.id, "PEND02".to_string())
            .await
            .expect("code rotation should succeed");
        assert_eq!(
            active_session_record(&records, &pending.id).room_code,
            "PEND02",
            "the record must carry the currently routable code"
        );

        // A second hidden room abandoned by its creator and repaired by the
        // sweep never becomes a completed record.
        let aborted_creator = Uuid::new_v4();
        let aborted = db
            .create_pending_room_classified(
                "pending_game".to_string(),
                Some("PEND03".to_string()),
                4,
                true,
                aborted_creator,
                "udp".to_string(),
                "region-a".to_string(),
                None,
                None,
            )
            .await
            .expect("second pending room creation should succeed");
        db.abandon_room(&aborted.id, &aborted_creator)
            .await
            .expect("abandonment should succeed");
        assert!(db
            .delete_room_if_pending(&aborted.id)
            .await
            .expect("delete_room_if_pending should not error"));

        let snapshot = records.snapshot();
        assert_eq!(snapshot.active_count, 1, "only the published room remains");
        assert_eq!(
            snapshot.completed_count, 0,
            "an unpublished room leaves no completed record"
        );

        db.delete_room(&pending.id)
            .await
            .expect("delete_room should not error");
        assert_eq!(records.snapshot().completed_count, 1);
    }

    /// A room that gains or loses its application owner after creation must
    /// keep the record current: the admission path claims the room's owner
    /// at first claimed join (`record_room_application`), and rollback clears
    /// it. A stale creation-time `None` would hide the session from its
    /// owning application's directory (bugbot: session records miss later
    /// app claims).
    #[tokio::test]
    async fn session_records_track_application_claims_after_creation() {
        let db = InMemoryDatabase::new();
        let records = db
            .session_records()
            .expect("in-memory backend tracks session records");

        let room = create_test_room(&db, "claim_game", "CLAM01")
            .await
            .expect("room creation should succeed");
        assert_eq!(
            active_session_record(&records, &room.id).application_id,
            None,
            "fixture rooms are created unowned"
        );

        let owner = Uuid::new_v4();
        db.set_room_application_id(&room.id, owner)
            .await
            .expect("claim should succeed");
        assert_eq!(
            active_session_record(&records, &room.id).application_id,
            Some(owner),
            "the record must follow the storage claim"
        );

        // A conditional clear that does not match the owner changes nothing.
        let impostor = Uuid::new_v4();
        assert!(!db
            .clear_room_application_id_if_matches(&room.id, impostor)
            .await
            .expect("conditional clear should not error"));
        assert_eq!(
            active_session_record(&records, &room.id).application_id,
            Some(owner),
            "a non-matching conditional clear must not touch the record"
        );

        // The matching conditional clear (the rollback path) does.
        assert!(db
            .clear_room_application_id_if_matches(&room.id, owner)
            .await
            .expect("conditional clear should not error"));
        assert_eq!(
            active_session_record(&records, &room.id).application_id,
            None,
            "the record must follow the storage clear"
        );

        // The unconditional clear stays consistent too.
        db.set_room_application_id(&room.id, owner)
            .await
            .expect("re-claim should succeed");
        db.clear_room_application_id(&room.id)
            .await
            .expect("unconditional clear should succeed");
        assert_eq!(
            active_session_record(&records, &room.id).application_id,
            None,
            "the record must follow the unconditional clear"
        );
    }

    /// The GC close reasons distinguish the cleanup path that removed the
    /// room, so a directory can tell an expired session from an emptied one.
    #[tokio::test]
    async fn session_records_carry_cleanup_reasons_from_the_gc_paths() {
        for expected in [
            crate::session_records::SessionCloseReason::Empty,
            crate::session_records::SessionCloseReason::Expired,
        ] {
            let db = InMemoryDatabase::new();
            let records = db
                .session_records()
                .expect("in-memory backend tracks session records");
            let room = create_test_room(&db, "gc_game", "GCSE01")
                .await
                .expect("room creation should succeed");
            let creator = *room.players.keys().next().expect("creator present");
            db.remove_player_from_room(&room.id, &creator)
                .await
                .expect("removal should not error");
            age_room(&db, &room.id, chrono::Duration::hours(2)).await;

            let empty_sweep = expected == crate::session_records::SessionCloseReason::Empty;
            if empty_sweep {
                let deleted = db
                    .cleanup_empty_rooms(chrono::Duration::seconds(300), &HashSet::new())
                    .await
                    .expect("empty cleanup should not error");
                assert_eq!(deleted, vec![room.id]);
            } else {
                let outcome = db
                    .cleanup_expired_rooms(
                        chrono::Duration::seconds(300),
                        chrono::Duration::seconds(3600),
                        &HashSet::new(),
                    )
                    .await
                    .expect("expired cleanup should not error");
                assert_eq!(
                    outcome.empty_rooms_cleaned + outcome.inactive_rooms_cleaned,
                    1
                );
            }

            let snapshot = records.snapshot();
            assert_eq!(snapshot.completed_count, 1);
            assert_eq!(snapshot.completed[0].close_reason, Some(expected));
        }
    }
    /// Preserve surviving rows and indexes while reclaiming codes across both sweeps.
    #[tokio::test]
    async fn cleanup_extraction_preserves_protected_pending_and_live_room_indexes() {
        for expired in [false, true] {
            let db = InMemoryDatabase::new();
            let mut survivors = HashSet::new();
            let mut removed = HashSet::new();
            let mut protected = HashSet::new();
            let mut inputs = Vec::new();
            for index in 0..8 {
                let game = format!("cleanup-{index}");
                let room = create_test_room(&db, &game, "SHARED").await.unwrap();
                let creator = *room.players.keys().next().unwrap();
                if index != 1 && index != 6 {
                    db.remove_player_from_room(&room.id, &creator)
                        .await
                        .unwrap();
                }
                if index == 2 {
                    db.add_spectator_to_room(&room.id, spectator("watcher"))
                        .await
                        .unwrap();
                }
                if index == 3 {
                    protected.insert(room.id);
                }
                if index == 4 {
                    db.room_publication.write().await.insert(
                        room.id,
                        PendingRoomState::Creating {
                            creator_id: creator,
                            owner: None,
                            accounting: None,
                        },
                    );
                }
                if index != 5 {
                    age_room(&db, &room.id, chrono::Duration::hours(2)).await;
                }
                let should_remove =
                    matches!(index, 0 | 7) || (expired && matches!(index, 1 | 2 | 6));
                if should_remove {
                    removed.insert(room.id);
                } else {
                    survivors.insert(room.id);
                }
                inputs.push((game, room.id, should_remove));
            }
            if expired {
                let outcome = db
                    .cleanup_expired_rooms(
                        chrono::Duration::minutes(5),
                        chrono::Duration::hours(1),
                        &protected,
                    )
                    .await
                    .unwrap();
                assert_eq!(outcome.empty_rooms_cleaned, 2);
                assert_eq!(outcome.inactive_rooms_cleaned, 3);
            } else {
                let deleted = db
                    .cleanup_empty_rooms(chrono::Duration::minutes(5), &protected)
                    .await
                    .unwrap();
                assert_eq!(deleted.into_iter().collect::<HashSet<_>>(), removed);
            }
            assert_eq!(
                db.rooms
                    .read()
                    .await
                    .keys()
                    .copied()
                    .collect::<HashSet<_>>(),
                survivors
            );
            assert_eq!(
                db.room_liveness_monotonic
                    .read()
                    .await
                    .keys()
                    .copied()
                    .collect::<HashSet<_>>(),
                survivors
            );
            assert_eq!(
                db.room_codes
                    .read()
                    .await
                    .values()
                    .copied()
                    .collect::<HashSet<_>>(),
                survivors
            );
            assert_eq!(db.room_publication.read().await.len(), 1);
            for (game, id, should_remove) in inputs {
                if should_remove {
                    let replacement = create_test_room(&db, &game, "SHARED").await.unwrap();
                    assert_ne!(replacement.id, id);
                    assert_eq!(
                        db.get_room(&game, "SHARED").await.unwrap().unwrap().id,
                        replacement.id
                    );
                } else {
                    assert!(db.get_room_by_id(&id).await.unwrap().is_some());
                }
            }
        }
    }
}
