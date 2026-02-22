use crate::protocol::{ConnectionInfo, PlayerId, PlayerInfo, Room, RoomId, SpectatorInfo};
use anyhow::Result;
use async_trait::async_trait;
use std::any::Any;
use std::collections::HashMap;
use uuid::Uuid;

/// Summary describing how many rooms were removed by the cleanup routine.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RoomCleanupOutcome {
    pub empty_rooms_cleaned: usize,
    pub inactive_rooms_cleaned: usize,
}

impl RoomCleanupOutcome {
    /// Total rooms removed (empty + inactive).
    pub fn total_cleaned(&self) -> usize {
        self.empty_rooms_cleaned + self.inactive_rooms_cleaned
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
    async fn set_room_application_id(
        &self,
        _room_id: &RoomId,
        _application_id: Uuid,
    ) -> Result<()> {
        Ok(())
    }

    async fn clear_room_application_id(&self, _room_id: &RoomId) -> Result<()> {
        Ok(())
    }

    /// Get room by game name and room code
    async fn get_room(&self, game_name: &str, room_code: &str) -> Result<Option<Room>>;

    /// Get room by ID
    async fn get_room_by_id(&self, room_id: &RoomId) -> Result<Option<Room>>;

    /// Add player to room (atomic operation)
    async fn add_player_to_room(&self, room_id: &RoomId, player: PlayerInfo) -> Result<bool>;

    /// Remove player from room
    async fn remove_player_from_room(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
    ) -> Result<Option<PlayerInfo>>;

    /// Update room authority
    #[allow(dead_code)]
    async fn update_room_authority(
        &self,
        room_id: &RoomId,
        authority_player: Option<PlayerId>,
    ) -> Result<bool>;

    /// Atomically request room authority with proper protocol enforcement
    async fn request_room_authority(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        become_authority: bool,
    ) -> Result<(bool, Option<String>)>;

    /// Update player name in room
    async fn update_player_name(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        name: &str,
    ) -> Result<bool>;

    /// Update player connection info for P2P establishment
    async fn update_player_connection_info(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        connection_info: ConnectionInfo,
    ) -> Result<bool>;

    /// Get all players in a room
    async fn get_room_players(&self, room_id: &RoomId) -> Result<Vec<PlayerInfo>>;

    /// Delete empty rooms and return their IDs for relay cleanup
    async fn cleanup_empty_rooms(&self, empty_timeout: chrono::Duration) -> Result<Vec<RoomId>>;

    /// Delete expired rooms based on timeouts and return a summary of what was removed.
    async fn cleanup_expired_rooms(
        &self,
        empty_timeout: chrono::Duration,
        inactive_timeout: chrono::Duration,
    ) -> Result<RoomCleanupOutcome>;

    /// Update room activity timestamp
    async fn update_room_activity(&self, room_id: &RoomId) -> Result<()>;

    /// Delete a specific room by ID
    #[allow(dead_code)]
    async fn delete_room(&self, room_id: &RoomId) -> Result<bool>;

    /// Get room count for a specific game (for rate limiting)
    async fn get_game_room_count(&self, game_name: &str) -> Result<usize>;

    /// Health check
    async fn health_check(&self) -> bool;

    /// Update player's last_seen (heartbeat) for cross-instance liveness
    async fn update_player_last_seen(&self, player_id: &PlayerId) -> Result<()>;

    /// Get room counts by game name for metrics
    async fn get_rooms_by_game(&self) -> Result<HashMap<String, usize>>;

    /// Get player count statistics for metrics
    async fn get_player_count_percentiles(&self) -> Result<HashMap<String, f64>>;

    /// Get player count statistics by game for metrics
    async fn get_game_player_percentiles(&self) -> Result<HashMap<String, HashMap<String, f64>>>;

    /// Transition room to lobby state when full
    async fn transition_room_to_lobby(&self, room_id: &RoomId) -> Result<()>;

    /// Transition room back to waiting state when no longer full
    async fn transition_room_to_waiting(&self, room_id: &RoomId) -> Result<()>;

    /// Toggle player ready state and return lobby information if successful
    /// Returns (lobby_state, ready_players, all_ready) if in lobby state
    async fn toggle_player_ready(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
    ) -> Result<Option<(crate::protocol::LobbyState, Vec<PlayerId>, bool)>>;

    /// Finalize room game when all players are ready
    async fn finalize_room_game(&self, room_id: &RoomId) -> Result<()>;

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
    /// Returns true if this instance should process the cleanup (we claimed it),
    /// false if another instance already processed it.
    ///
    /// This is used in multi-instance deployments to ensure post-cleanup operations
    /// (publishing room_closed events, clearing relay sessions, etc.) only happen once.
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

    /// Check if an admin user exists. Always returns false for in-memory backend.
    /// Placeholder for future auth integration.
    async fn admin_user_exists(&self, email: &str) -> Result<bool> {
        let _ = email;
        Ok(false)
    }
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
    /// Tracks claimed cleanup operations for idempotency (cleanup_id -> entry)
    cleanup_events: std::sync::Arc<tokio::sync::RwLock<HashMap<String, CleanupEventEntry>>>,
}

impl InMemoryDatabase {
    pub fn new() -> Self {
        Self {
            rooms: std::sync::Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            room_codes: std::sync::Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            cleanup_events: std::sync::Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        }
    }
}

impl Default for InMemoryDatabase {
    fn default() -> Self {
        Self::new()
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
        let room_code =
            room_code.unwrap_or_else(crate::protocol::room_codes::generate_clean_room_code);

        // Create creator player info before acquiring locks
        let creator_info = PlayerInfo {
            id: creator_id,
            name: "Creator".to_string(), // This will be updated later when we have the actual name
            is_authority: true,
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info: None,
            region_id: region_id.clone(),
        };

        let mut players = HashMap::new();
        let creator_id_val = creator_info.id;
        players.insert(creator_id_val, creator_info);

        // Lock ordering: rooms first, then room_codes (consistent with delete_room, cleanup_*)
        // Both locks are held simultaneously to ensure atomicity of the room creation:
        // no other task can observe a partial state where room_codes has an entry but rooms does not.
        let mut rooms = self.rooms.write().await;
        let mut room_codes = self.room_codes.write().await;

        // Check room code uniqueness under the write lock (no TOCTOU gap)
        let game_room_key = (game_name.clone(), room_code.clone());
        if room_codes.contains_key(&game_room_key) {
            anyhow::bail!("Room code {room_code} already exists for game {game_name}");
        }

        // Generate a unique room ID
        let room_id = {
            let mut id = uuid::Uuid::new_v4();
            let mut attempts = 0u8;
            while rooms.contains_key(&id) {
                attempts += 1;
                if attempts >= 16 {
                    anyhow::bail!("Failed to generate unique room ID after {attempts} attempts");
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
            authority_player: if supports_authority {
                Some(creator_id_val)
            } else {
                None
            },
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
        let rooms = self.rooms.read().await;
        Ok(rooms.get(room_id).cloned())
    }

    async fn add_player_to_room(&self, room_id: &RoomId, player: PlayerInfo) -> Result<bool> {
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            if room.players.len() < room.max_players as usize {
                room.players.insert(player.id, player);
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
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            let removed_player = room.players.remove(player_id);

            // If removed player was authority, CLEAR authority (don't auto-reassign per protocol)
            if room.authority_player == Some(*player_id) {
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
    ) -> Result<(bool, Option<String>)> {
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            // Check if room supports authority
            if !room.supports_authority {
                return Ok((false, Some("Room does not support authority".to_string())));
            }

            // Check if player exists in room
            if !room.players.contains_key(player_id) {
                return Ok((false, Some("Player not found in room".to_string())));
            }

            if become_authority {
                // REQUEST AUTHORITY CASE

                // Rule: Can only request authority if no one currently has it
                if room.authority_player.is_some() {
                    return Ok((
                        false,
                        Some("Another player already has authority".to_string()),
                    ));
                }

                // Grant authority to the requesting player
                room.authority_player = Some(*player_id);
                if let Some(player) = room.players.get_mut(player_id) {
                    player.is_authority = true;
                }

                Ok((true, None))
            } else {
                // RELEASE AUTHORITY CASE

                // Rule: Can only release authority if you currently have it
                if room.authority_player != Some(*player_id) {
                    return Ok((
                        false,
                        Some("You do not have authority to release".to_string()),
                    ));
                }

                // Release authority
                room.authority_player = None;
                if let Some(player) = room.players.get_mut(player_id) {
                    player.is_authority = false;
                }

                Ok((true, None))
            }
        } else {
            Ok((false, Some("Room not found".to_string())))
        }
    }

    async fn update_player_name(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        name: &str,
    ) -> Result<bool> {
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
        let rooms = self.rooms.read().await;
        if let Some(room) = rooms.get(room_id) {
            Ok(room.players.values().cloned().collect())
        } else {
            Ok(Vec::new())
        }
    }

    async fn cleanup_empty_rooms(&self, empty_timeout: chrono::Duration) -> Result<Vec<RoomId>> {
        let mut rooms = self.rooms.write().await;
        let mut room_codes = self.room_codes.write().await;

        let effective_timeout = if empty_timeout <= chrono::Duration::zero() {
            chrono::Duration::zero()
        } else {
            empty_timeout
        };
        let cutoff = chrono::Utc::now() - effective_timeout;

        let mut to_remove = Vec::new();
        for (room_id, room) in rooms.iter() {
            if room.players.is_empty() && room.last_activity <= cutoff {
                to_remove.push((*room_id, room.game_name.clone(), room.code.clone()));
            }
        }

        let mut deleted_ids = Vec::new();
        for (room_id, game_name, room_code) in to_remove {
            rooms.remove(&room_id);
            room_codes.remove(&(game_name, room_code));
            deleted_ids.push(room_id);
        }

        Ok(deleted_ids)
    }

    async fn cleanup_expired_rooms(
        &self,
        empty_timeout: chrono::Duration,
        inactive_timeout: chrono::Duration,
    ) -> Result<RoomCleanupOutcome> {
        let mut rooms = self.rooms.write().await;
        let mut room_codes = self.room_codes.write().await;

        let mut to_remove = Vec::new();
        for (room_id, room) in rooms.iter() {
            if room.is_expired(empty_timeout, inactive_timeout) {
                let was_empty = room.players.is_empty();
                to_remove.push((
                    *room_id,
                    room.game_name.clone(),
                    room.code.clone(),
                    was_empty,
                ));
            }
        }

        let mut outcome = RoomCleanupOutcome::default();
        for (room_id, game_name, room_code, was_empty) in to_remove {
            rooms.remove(&room_id);
            room_codes.remove(&(game_name, room_code));

            if was_empty {
                outcome.empty_rooms_cleaned += 1;
            } else {
                outcome.inactive_rooms_cleaned += 1;
            }
        }

        Ok(outcome)
    }

    async fn update_room_activity(&self, room_id: &RoomId) -> Result<()> {
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            room.last_activity = chrono::Utc::now();
        }
        Ok(())
    }

    async fn delete_room(&self, room_id: &RoomId) -> Result<bool> {
        let mut rooms = self.rooms.write().await;
        let mut room_codes = self.room_codes.write().await;

        if let Some(room) = rooms.remove(room_id) {
            let game_room_key = (room.game_name.clone(), room.code);
            room_codes.remove(&game_room_key);
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
            *game_counts.entry(room.game_name.clone()).or_insert(0) += 1;
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
        percentiles.insert("p50".to_string(), percentile(&player_counts, 0.5));
        percentiles.insert("p90".to_string(), percentile(&player_counts, 0.9));
        percentiles.insert("p99".to_string(), percentile(&player_counts, 0.99));
        percentiles.insert("p99_5".to_string(), percentile(&player_counts, 0.995));
        percentiles.insert("p99_9".to_string(), percentile(&player_counts, 0.999));
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
                percentiles.insert("p50".to_string(), percentile(&player_counts, 0.5));
                percentiles.insert("p90".to_string(), percentile(&player_counts, 0.9));
                percentiles.insert("p99".to_string(), percentile(&player_counts, 0.99));
                percentiles.insert("p99_5".to_string(), percentile(&player_counts, 0.995));
                percentiles.insert("p99_9".to_string(), percentile(&player_counts, 0.999));
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

    async fn transition_room_to_waiting(&self, room_id: &RoomId) -> Result<()> {
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            // Only transition if room is currently in lobby but no longer meets lobby requirements
            if room.lobby_state == crate::protocol::LobbyState::Lobby && !room.should_enter_lobby()
            {
                room.lobby_state = crate::protocol::LobbyState::Waiting;
                room.lobby_started_at = None;
                room.ready_players.clear();
                // Clear ready status for all players
                for player in room.players.values_mut() {
                    player.is_ready = false;
                }
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

    async fn finalize_room_game(&self, room_id: &RoomId) -> Result<()> {
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            room.finalize_game();
        }
        Ok(())
    }

    async fn add_spectator_to_room(
        &self,
        room_id: &RoomId,
        spectator: SpectatorInfo,
    ) -> Result<bool> {
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            Ok(room.add_spectator(spectator))
        } else {
            Ok(false)
        }
    }

    async fn remove_spectator_from_room(
        &self,
        room_id: &RoomId,
        spectator_id: &PlayerId,
    ) -> Result<Option<SpectatorInfo>> {
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            Ok(room.remove_spectator(spectator_id))
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
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            room.application_id = Some(application_id);
        }
        Ok(())
    }

    async fn clear_room_application_id(&self, room_id: &RoomId) -> Result<()> {
        let mut rooms = self.rooms.write().await;
        if let Some(room) = rooms.get_mut(room_id) {
            room.application_id = None;
        }
        Ok(())
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
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(1);

        let initial_count = cleanup_events.len();
        cleanup_events.retain(|_, entry| entry.processed_at > cutoff);
        let deleted_count = initial_count - cleanup_events.len();

        Ok(deleted_count as u64)
    }

    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }

    async fn admin_user_exists(&self, _email: &str) -> Result<bool> {
        Ok(false)
    }
}

fn percentile(sorted_values: &[usize], p: f64) -> f64 {
    if sorted_values.is_empty() {
        return 0.0;
    }

    let index = (p * (sorted_values.len() - 1) as f64).round() as usize;
    // SAFETY: The early return guarantees len >= 1, so `len - 1` does not
    // underflow and `.min(len - 1)` clamps `index` to a valid bound.
    #[allow(clippy::indexing_slicing)]
    let value = sorted_values[index.min(sorted_values.len() - 1)];
    value as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Arc;

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
}
