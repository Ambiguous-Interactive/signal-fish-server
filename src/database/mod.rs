use crate::protocol::{ConnectionInfo, PlayerId, PlayerInfo, Room, RoomId, SpectatorInfo};
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
    ) -> CreateRoomResult {
        self.create_room(
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
        .map_err(CreateRoomError::Storage)
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

    /// Update room authority.
    ///
    /// A `Some(id)` grant is a raw write: it is the caller's contract to pass
    /// only a current room member. Granting a non-member leaves
    /// `authority_player` pointing outside the roster with no `is_authority`
    /// flag set, which wedges `request_room_authority` (`AlreadyHeld`) and is
    /// never cleared by a departure (removal clears the role only for the
    /// departing member). Production must go through
    /// [`Self::request_room_authority`] (membership-checked) for grants; the
    /// only production caller of this method passes `None` to clear the role
    /// during reconnect rollback.
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

    /// Update player name in room
    async fn update_player_name(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        name: &str,
    ) -> Result<bool>;

    /// Update legacy self-declared peer metadata for `GameStarting`.
    async fn update_player_connection_info(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        connection_info: ConnectionInfo,
    ) -> Result<bool>;

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

    /// Get room count for a specific game (for rate limiting)
    async fn get_game_room_count(&self, game_name: &str) -> Result<usize>;

    /// Get the authoritative live-room count for one application across every
    /// game name. Backends that cannot provide this query must return an error;
    /// configured application quotas fail closed rather than using a cache or
    /// silently bypassing the limit.
    async fn get_application_room_count(&self, _application_id: &Uuid) -> Result<usize> {
        anyhow::bail!("application room counting is not supported by this database")
    }

    /// Health check
    async fn health_check(&self) -> bool;

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
    /// Returns (lobby_state, ready_players, all_ready) if in lobby state
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
    /// Returns true if successfully added, false if room is full or doesn't exist
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

/// Entry tracking a claimed room cleanup operation for idempotency
#[derive(Debug, Clone)]
struct CleanupEventEntry {
    #[allow(dead_code)]
    instance_id: uuid::Uuid,
    processed_at: chrono::DateTime<chrono::Utc>,
}

/// Simple in-memory database for testing and single-instance deployments
pub struct InMemoryDatabase {
    rooms: std::sync::Arc<tokio::sync::RwLock<HashMap<RoomId, Room>>>,
    /// Maps (game_name, room_code) -> room_id to allow same room codes across different games
    room_codes: std::sync::Arc<tokio::sync::RwLock<HashMap<(String, String), RoomId>>>,
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
    get_room_by_id_calls: std::sync::atomic::AtomicU32,
    #[cfg(test)]
    pause_get_room_by_id: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    get_room_by_id_reached: tokio::sync::Notify,
    #[cfg(test)]
    release_get_room_by_id: tokio::sync::Notify,
    #[cfg(test)]
    fail_remove_player_from_room: std::sync::atomic::AtomicBool,
    #[cfg(all(test, signal_fish_repository_tests))]
    fail_update_player_name: std::sync::atomic::AtomicBool,
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
}

impl InMemoryDatabase {
    pub fn new() -> Self {
        Self {
            rooms: std::sync::Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            room_codes: std::sync::Arc::new(tokio::sync::RwLock::new(HashMap::new())),
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
            fail_update_player_name: std::sync::atomic::AtomicBool::new(false),
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

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn fail_update_player_name_for_test(&self, fail: bool) {
        self.fail_update_player_name
            .store(fail, std::sync::atomic::Ordering::Relaxed);
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
        None => chrono::Utc::now().signed_duration_since(last_activity_wall),
    }
}

#[async_trait]
impl GameDatabase for InMemoryDatabase {
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
        self.create_room_classified(
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
            connected_at: chrono::Utc::now(),
            connection_info: None,
            // Room-state record, not a wire snapshot: the v3 incarnation epoch
            // is filled at snapshot-send time, so this stays `None`.
            epoch: None,
            seq: None,
            region_id: region_id.clone(),
        };

        let mut players = HashMap::new();
        players.insert(creator_id, creator_info);

        // Lock ordering: rooms first, then room_codes (consistent with delete_room, cleanup_*)
        // Both locks are held simultaneously to ensure atomicity of the room creation:
        // no other task can observe a partial state where room_codes has an entry but rooms does not.
        let mut rooms = self.rooms.write().await;
        let mut room_codes = self.room_codes.write().await;

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
        };

        // Insert into both maps atomically while holding both locks
        rooms.insert(room_id, room.clone());
        room_codes.insert(game_room_key, room_id);
        self.room_liveness_monotonic
            .write()
            .await
            .insert(room_id, RoomLiveness::Live(tokio::time::Instant::now()));

        Ok(room)
    }

    async fn get_room(&self, game_name: &str, room_code: &str) -> Result<Option<Room>> {
        // Lock ordering: rooms first, then room_codes (consistent with write paths)
        let rooms = self.rooms.read().await;
        let room_codes = self.room_codes.read().await;
        let game_room_key = (game_name.to_string(), room_code.to_string());
        if let Some(room_id) = room_codes.get(&game_room_key) {
            if let Some(room) = rooms.get(room_id) {
                return Ok(Some(room.clone()));
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

    async fn add_player_to_room(&self, room_id: &RoomId, mut player: PlayerInfo) -> Result<bool> {
        #[cfg(all(test, signal_fish_repository_tests))]
        if self
            .pause_add_player_to_room
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            self.add_player_reached.notify_one();
            self.release_add_player.notified().await;
        }
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            if room.players.len() < room.max_players as usize {
                // `authority_player` is the single source of truth for the room
                // (see `create_room_classified`). An inbound `PlayerInfo` can be
                // a pre-disconnect snapshot whose flag went stale while the
                // member was away — restoring it verbatim would flag a second
                // authority in every membership payload. Derive the flag here so
                // no caller can insert a member that contradicts the room.
                player.is_authority = room.authority_player == Some(player.id);
                // Readiness has two regimes. While the room is open it is
                // coordinator state and every stored flag is `false`, so the
                // room's own list decides and a snapshot's flag must not
                // resurrect readiness the membership it described has lost.
                // Once the room is finalized the list is the frozen fact of who
                // started the game, and removal prunes a departing member from
                // it — so a membership being RESTORED carries the only surviving
                // evidence that it started. A fresh joiner cannot smuggle
                // readiness in that way, and the guarantee rests on three
                // properties this file and the join path own: the join record is
                // constructed with `is_ready: false`; readiness cannot be
                // toggled in a finalized room (`toggle_player_ready` requires
                // `Lobby`); and no production caller writes `is_ready = true`
                // into an open room's record — `toggle_player_ready`, the only
                // method that could, has none. So a member that disconnected
                // before the start reconnects as the seat-filler it is.
                let finalized = room.lobby_state == crate::protocol::LobbyState::Finalized;
                player.is_ready =
                    room.ready_players.contains(&player.id) || (finalized && player.is_ready);
                if player.is_ready && !room.ready_players.contains(&player.id) {
                    room.ready_players.push(player.id);
                }
                room.players.insert(player.id, player);
                // A join is activity: refresh the reaper clock so a room that
                // fills up long after creation is not GC'd mid-game (BUG-1).
                room.last_activity = chrono::Utc::now();
                self.room_liveness_monotonic
                    .write()
                    .await
                    .insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));
                Ok(true)
            } else {
                Ok(false) // Room is full
            }
        } else {
            anyhow::bail!("Room not found")
        }
    }

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
        if let Some(room) = rooms.get_mut(room_id) {
            let removed_player = room.players.remove(player_id);

            // A real departure is activity, and it starts the empty-room clock:
            // both cleanup paths time an empty room from `last_activity`, so
            // refreshing it gives a room emptied long after creation the full
            // `empty_room_timeout` window from the LAST departure rather than
            // deleting it immediately off a stale `created_at` (BUG-1). Guarded
            // on an ACTUAL removal — a no-op remove (player already gone) is not
            // activity and must not keep an otherwise-stale room alive.
            if removed_player.is_some() {
                room.last_activity = chrono::Utc::now();
                self.room_liveness_monotonic
                    .write()
                    .await
                    .insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));
            }

            // Prune the departed player's ready entry so it cannot linger in
            // `RoomJoined` / `Reconnected` payloads. Departures intentionally
            // preserve the room lifecycle state and every remaining member's
            // readiness, so the departing id must be removed directly.
            room.ready_players.retain(|id| id != player_id);

            // If removed player was authority, CLEAR authority (don't auto-reassign per protocol).
            // Guarded by an ACTUAL removal: `leave_room_locked` reports the
            // vacated role from this call's returned record, so a no-op remove
            // must not silently clear a role nobody is told about.
            if removed_player.is_some() && room.authority_player == Some(*player_id) {
                room.authority_player = None;
                // Clear authority flag from all players to maintain consistency
                for player in room.players.values_mut() {
                    if player.is_authority {
                        player.is_authority = false;
                    }
                }
            }

            Ok(removed_player)
        } else {
            Ok(None)
        }
    }

    async fn update_room_authority(
        &self,
        room_id: &RoomId,
        authority_player: Option<PlayerId>,
    ) -> Result<bool> {
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            // Check if room supports authority
            if !room.supports_authority {
                return Ok(false);
            }

            // Remove authority from previous player
            if let Some(prev_auth) = room.authority_player {
                if let Some(player) = room.players.get_mut(&prev_auth) {
                    player.is_authority = false;
                }
            }

            // Set new authority
            room.authority_player = authority_player;
            if let Some(new_auth) = authority_player {
                if let Some(player) = room.players.get_mut(&new_auth) {
                    player.is_authority = true;
                }
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn request_room_authority(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        become_authority: bool,
    ) -> Result<AuthorityOutcome> {
        let mut rooms = self.rooms.write().await;
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
                room.authority_player = Some(*player_id);
                if let Some(player) = room.players.get_mut(player_id) {
                    player.is_authority = true;
                }

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
                room.authority_player = None;
                if let Some(player) = room.players.get_mut(player_id) {
                    player.is_authority = false;
                }

                drop(rooms);
                #[cfg(test)]
                self.pause_after_authority_request_commit_for_test().await;
                Ok(AuthorityOutcome::Granted)
            }
        } else {
            Ok(AuthorityOutcome::Denied(AuthorityDenial::RoomNotFound))
        }
    }

    async fn update_player_name(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        name: &str,
    ) -> Result<bool> {
        #[cfg(all(test, signal_fish_repository_tests))]
        if self
            .fail_update_player_name
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            anyhow::bail!("injected update_player_name failure for test");
        }
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            if let Some(player) = room.players.get_mut(player_id) {
                player.name = name.to_string();
                Ok(true)
            } else {
                Ok(false)
            }
        } else {
            Ok(false)
        }
    }

    async fn update_player_connection_info(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        connection_info: ConnectionInfo,
    ) -> Result<bool> {
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            if let Some(player) = room.players.get_mut(player_id) {
                player.connection_info = Some(connection_info);
                Ok(true)
            } else {
                Ok(false)
            }
        } else {
            Ok(false)
        }
    }

    async fn get_room_players(&self, room_id: &RoomId) -> Result<Vec<PlayerInfo>> {
        #[cfg(test)]
        self.get_room_players_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

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

        let mut to_remove = Vec::new();
        for (room_id, room) in rooms.iter() {
            if !room.has_occupants()
                && !protected.contains(room_id)
                && room_idle_for(liveness.get(room_id).cloned(), room.last_activity)
                    > effective_timeout
            {
                to_remove.push((*room_id, room.game_name.clone(), room.code.clone()));
            }
        }

        let mut deleted_ids = Vec::new();
        for (room_id, game_name, room_code) in to_remove {
            rooms.remove(&room_id);
            room_codes.remove(&(game_name, room_code));
            liveness.remove(&room_id);
            deleted_ids.push(room_id);
        }

        Ok(deleted_ids)
    }

    async fn cleanup_expired_rooms(
        &self,
        empty_timeout: chrono::Duration,
        inactive_timeout: chrono::Duration,
        protected: &HashSet<RoomId>,
    ) -> Result<RoomCleanupOutcome> {
        let mut rooms = self.rooms.write().await;
        let mut room_codes = self.room_codes.write().await;
        let mut liveness = self.room_liveness_monotonic.write().await;

        let mut to_remove = Vec::new();
        for (room_id, room) in rooms.iter() {
            if protected.contains(room_id) {
                continue;
            }
            let idle_for = room_idle_for(liveness.get(room_id).cloned(), room.last_activity);
            let expired = if room.has_occupants() {
                idle_for > inactive_timeout
            } else {
                idle_for > empty_timeout
            };
            if expired {
                to_remove.push((
                    *room_id,
                    room.game_name.clone(),
                    room.code.clone(),
                    !room.has_occupants(),
                ));
            }
        }

        let mut outcome = RoomCleanupOutcome::default();
        for (room_id, game_name, room_code, was_empty) in to_remove {
            rooms.remove(&room_id);
            room_codes.remove(&(game_name, room_code));
            liveness.remove(&room_id);

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
        if let Some(room) = rooms.get_mut(room_id) {
            room.last_activity = chrono::Utc::now();
            self.room_liveness_monotonic
                .write()
                .await
                .insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));
        }
        Ok(())
    }

    async fn delete_room(&self, room_id: &RoomId) -> Result<bool> {
        let mut rooms = self.rooms.write().await;
        let mut room_codes = self.room_codes.write().await;

        if let Some(room) = rooms.remove(room_id) {
            let game_room_key = (room.game_name.clone(), room.code);
            room_codes.remove(&game_room_key);
            self.room_liveness_monotonic.write().await.remove(room_id);
            Ok(true)
        } else {
            Ok(false)
        }
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

    async fn health_check(&self) -> bool {
        true
    }

    async fn update_player_last_seen(&self, _player_id: &PlayerId) -> Result<()> {
        // In-memory DB has no per-player last_seen tracking; no-op
        Ok(())
    }

    async fn get_rooms_by_game(&self) -> Result<HashMap<String, usize>> {
        let rooms = self.rooms.read().await;
        let mut game_counts = HashMap::new();

        for room in rooms.values() {
            let count = game_counts.entry(room.game_name.clone()).or_insert(0usize);
            *count = count.saturating_add(1);
        }

        Ok(game_counts)
    }

    async fn get_player_count_percentiles(&self) -> Result<HashMap<String, f64>> {
        let rooms = self.rooms.read().await;
        let mut player_counts: Vec<usize> = rooms.values().map(|room| room.players.len()).collect();

        if player_counts.is_empty() {
            return Ok(HashMap::new());
        }

        player_counts.sort_unstable();

        let mut percentiles = HashMap::new();
        percentiles.insert("p50".to_string(), percentile(&player_counts, 500));
        percentiles.insert("p90".to_string(), percentile(&player_counts, 900));
        percentiles.insert("p99".to_string(), percentile(&player_counts, 990));
        percentiles.insert("p99_5".to_string(), percentile(&player_counts, 995));
        percentiles.insert("p99_9".to_string(), percentile(&player_counts, 999));
        // SAFETY: We checked player_counts.is_empty() above, so .last() is guaranteed to succeed
        percentiles.insert(
            "p100".to_string(),
            player_counts.last().copied().unwrap_or(0) as f64,
        );

        Ok(percentiles)
    }

    async fn get_game_player_percentiles(&self) -> Result<HashMap<String, HashMap<String, f64>>> {
        let rooms = self.rooms.read().await;
        let mut game_player_counts: HashMap<String, Vec<usize>> = HashMap::new();

        for room in rooms.values() {
            game_player_counts
                .entry(room.game_name.clone())
                .or_default()
                .push(room.players.len());
        }

        let mut result = HashMap::new();

        for (game_name, mut player_counts) in game_player_counts {
            if !player_counts.is_empty() {
                player_counts.sort_unstable();

                let mut percentiles = HashMap::new();
                percentiles.insert("p50".to_string(), percentile(&player_counts, 500));
                percentiles.insert("p90".to_string(), percentile(&player_counts, 900));
                percentiles.insert("p99".to_string(), percentile(&player_counts, 990));
                percentiles.insert("p99_5".to_string(), percentile(&player_counts, 995));
                percentiles.insert("p99_9".to_string(), percentile(&player_counts, 999));
                // SAFETY: We're inside if !player_counts.is_empty(), so .last() is guaranteed to succeed
                percentiles.insert(
                    "p100".to_string(),
                    player_counts.last().copied().unwrap_or(0) as f64,
                );

                result.insert(game_name, percentiles);
            }
        }

        Ok(result)
    }

    async fn transition_room_to_lobby(&self, room_id: &RoomId) -> Result<()> {
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            if room.should_enter_lobby() {
                room.enter_lobby();
            }
        }
        Ok(())
    }

    async fn toggle_player_ready(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
    ) -> Result<Option<(crate::protocol::LobbyState, Vec<PlayerId>, bool)>> {
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            if room.lobby_state == crate::protocol::LobbyState::Lobby {
                // Toggle player ready state
                let current_ready = room
                    .players
                    .get(player_id)
                    .map(|p| p.is_ready)
                    .unwrap_or(false);
                room.set_player_ready(player_id, !current_ready);

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

    async fn finalize_room_game(
        &self,
        room_id: &RoomId,
        expected: &FinalizeRoomGameExpectation,
    ) -> Result<FinalizeRoomGameOutcome> {
        let mut rooms = self.rooms.write().await;
        let room = rooms
            .get_mut(room_id)
            .ok_or_else(|| anyhow::anyhow!("room not found while finalizing game"))?;
        if room.lobby_state == crate::protocol::LobbyState::Finalized {
            return Ok(FinalizeRoomGameOutcome::AlreadyFinalized);
        }
        let mut current_members: Vec<PlayerId> = room.players.keys().copied().collect();
        current_members.sort_unstable();
        let mut expected_members = expected.members.clone();
        expected_members.sort_unstable();
        if current_members != expected_members
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
        room.game_finalized_at = Some(chrono::Utc::now());
        Ok(FinalizeRoomGameOutcome::Finalized)
    }

    async fn add_spectator_to_room(
        &self,
        room_id: &RoomId,
        spectator: SpectatorInfo,
    ) -> Result<bool> {
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            let admitted = room.add_spectator(spectator);
            if admitted {
                // A spectator join is activity: `Room::add_spectator` already
                // refreshed the wall-clock record, so the monotonic GC stamp
                // must move in lockstep.
                self.room_liveness_monotonic
                    .write()
                    .await
                    .insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));
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
        if let Some(room) = rooms.get_mut(room_id) {
            let removed = room.remove_spectator(spectator_id);
            if removed.is_some() {
                // A real departure is activity and starts the empty-room clock:
                // refresh the monotonic stamp alongside the wall-clock record.
                self.room_liveness_monotonic
                    .write()
                    .await
                    .insert(*room_id, RoomLiveness::Live(tokio::time::Instant::now()));
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
        Ok(true)
    }

    async fn try_claim_room_cleanup(
        &self,
        room_id: &RoomId,
        cleanup_type: &str,
        instance_id: &uuid::Uuid,
    ) -> Result<bool> {
        let mut cleanup_events = self.cleanup_events.write().await;

        // Create a cleanup ID with time bucket (5 minute window) to allow re-cleanup
        // if the room somehow gets recreated and becomes empty again
        let time_bucket = chrono::Utc::now().timestamp() / 300;
        let cleanup_id = format!("{room_id}:{cleanup_type}:{time_bucket}");

        // Try to claim the cleanup operation using entry API
        if let std::collections::hash_map::Entry::Vacant(e) = cleanup_events.entry(cleanup_id) {
            // We claimed it
            e.insert(CleanupEventEntry {
                instance_id: *instance_id,
                processed_at: chrono::Utc::now(),
            });
            Ok(true)
        } else {
            // Already processed by another instance
            Ok(false)
        }
    }

    async fn cleanup_old_room_cleanup_events(&self) -> Result<u64> {
        let mut cleanup_events = self.cleanup_events.write().await;
        let cutoff = chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::hours(1))
            .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC);

        let initial_count = cleanup_events.len();
        cleanup_events.retain(|_, entry| entry.processed_at > cutoff);
        let deleted_count = initial_count.saturating_sub(cleanup_events.len());

        Ok(u64::try_from(deleted_count).unwrap_or(u64::MAX))
    }

    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }
}

const PERCENTILE_SCALE: usize = 1_000;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ErrorCode;
    use std::collections::HashSet;
    use std::sync::Arc;

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

    // NOTE: These tests run under Miri. `InMemoryDatabase::create_room` calls
    // `chrono::Utc::now()` (which invokes `clock_gettime(CLOCK_REALTIME)`), but the
    // Miri job runs with `-Zmiri-disable-isolation`, so that syscall is serviced
    // rather than aborting the run. The `tokio::spawn`-based concurrency tests below
    // also run under Miri (the default `#[tokio::test]` current-thread runtime is
    // interpretable) — a useful place for Miri's data-race detection.

    /// Helper: create a room with the given game name and room code using sensible defaults.
    async fn create_test_room(
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

    // --- BUG-1: room lifecycle GC (activity refresh + reconnection-aware GC) ---

    fn member(name: &str) -> PlayerInfo {
        PlayerInfo {
            id: Uuid::new_v4(),
            name: name.to_string(),
            is_authority: false,
            is_ready: false,
            connected_at: chrono::Utc::now(),
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
            connected_at: chrono::Utc::now(),
        }
    }

    /// Backdate a room's timestamps so it looks stale to the GC without waiting
    /// wall-clock time. Reaches the in-memory map directly (same module).
    async fn age_room(db: &InMemoryDatabase, room_id: &RoomId, age: chrono::Duration) {
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
                connected_at: chrono::Utc::now(),
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
            connected_at: chrono::Utc::now(),
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
}
