use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use super::types::{
    PeerConnectionInfo, PlayerId, PlayerInfo, RoomId, SpectatorInfo, DEFAULT_REGION_ID,
};

// ============================================================================
// ROOM LIFECYCLE DOCUMENTATION
// ============================================================================
//
// This module defines the core room lifecycle state machine for the Signal Fish
// signaling server. Rooms progress through three main states with specific
// triggers and rules governing each transition.
//
// ## Room Lifecycle States
//
// ```text
// [*] --> Waiting: Room Created
//
// Waiting --> Lobby: Room Full (players == max_players)
// Lobby --> Waiting: Player Leaves (players < max_players)
// Lobby --> Finalized: All Players Ready
//
// Finalized --> [*]: Game Started (Room Cleanup)
// Waiting --> [*]: Room Expired (Empty/Inactive Timeout)
// Lobby --> [*]: Room Expired (Inactive Timeout)
// ```
//
// ### 1. Waiting State
//
// - **Description**: Initial state when a room is created. The room is open
//   and accepting new players.
// - **Characteristics**:
//   - Room has fewer players than `max_players`
//   - Players can join freely
//   - No ready state tracking
//   - Room can expire if empty for too long
//
// ### 2. Lobby State
//
// - **Description**: Room is full and players are coordinating readiness to
//   start the game.
// - **Characteristics**:
//   - Room has exactly `max_players` players
//   - Players can mark themselves ready/unready via `PlayerReady` messages
//   - `ready_players` list tracks who is ready
//   - Cannot accept new players (room is full)
//   - Transitions to Finalized when all players ready
//   - Broadcasts `LobbyStateChanged` when ready state changes
//
// ### 3. Finalized State
//
// - **Description**: All players ready, game starting. Legacy, self-declared
//   peer metadata is exchanged.
// - **Characteristics**:
//   - All players have marked ready
//   - `game_finalized_at` timestamp recorded
//   - `GameStarting` message sent with legacy peer metadata
//   - Room typically cleaned up shortly after
//   - No further state transitions possible: Finalized is terminal. A player
//     departure does NOT regress the room (only Lobby → Waiting exists), and
//     post-finalize `PlayerReady` toggles are rejected with
//     `INVALID_ROOM_STATE`
//   - A Finalized room with an open seat (a member departed) still accepts
//     joins — `add_player_to_room` gates only on fullness — so seat-filling
//     late joins enter the running session without replaying the lobby cycle
//
// ## Key State Transitions and Protocol Messages
//
// ### Waiting → Lobby
// - **Trigger**: Room becomes full (player count reaches `max_players`)
// - **Condition**: `should_enter_lobby()` returns true
// - **Action**: Calls `enter_lobby()`, sets `lobby_started_at` timestamp
// - **Message**: Broadcasts `LobbyStateChanged` with `lobby_state: "lobby"`
//
// ### Lobby → Waiting
// - **Trigger**: A player leaves, bringing player count below `max_players`
// - **Action**: Revert to Waiting state, clear `ready_players` list
// - **Messages**: Broadcasts `PlayerLeft` and `LobbyStateChanged`
//
// ### Lobby → Finalized
// - **Trigger**: All players in lobby mark themselves ready
// - **Condition**: The room coordinator's `handle_player_ready` (holding the
//   room-operation lock) observes every member ready in its own ready map
// - **Action**: The coordinator persists the decision via the storage trait's
//   `finalize_room_game`, which sets `lobby_state = Finalized`, synchronizes
//   the per-player ready flags / `ready_players`, and records the
//   `game_finalized_at` timestamp. (`Room::finalize_game()` is a test-only
//   convenience and is not on the production path.)
// - **Message**: Broadcasts `GameStarting` with legacy peer metadata for all players
//
// ## Protocol Message Flow Example (2 Players)
//
// ```text
// Player1                Server                   Player2
//   |                      |                          |
//   |-- JoinRoom --------->|                          |
//   |<-- RoomJoined -------|                          |
//   |                      |<------- JoinRoom --------|
//   |<-- PlayerJoined -----|--- RoomJoined ---------->|
//   |<-- LobbyStateChanged-|--- LobbyStateChanged --->|
//   |                      |      (state: lobby)      |
//   |-- PlayerReady ------>|                          |
//   |<-- LobbyStateChanged-|--- LobbyStateChanged --->|
//   |                      |<------- PlayerReady -----|
//   |<-- LobbyStateChanged-|--- LobbyStateChanged --->|
//   |<-- GameStarting -----|--- GameStarting -------->|
// ```
//
// ## Related Client Messages
//
// - `JoinRoom`: Join or create a room (triggers room creation or player join)
// - `PlayerReady`: Toggle player ready state in lobby
// - `LeaveRoom`: Leave a room (may trigger Lobby → Waiting transition)
// - `Reconnect`: Reconnect to a room after disconnection
//
// ## Related Server Messages
//
// - `RoomJoined`: Confirm successful room join
// - `PlayerJoined`: Notify others when a player joins
// - `PlayerLeft`: Notify others when a player leaves
// - `LobbyStateChanged`: Notify lobby state changes (waiting/lobby) and ready status
// - `GameStarting`: Notify game finalization with legacy peer metadata
//
// ## Edge Cases
//
// - **Single Player Rooms** (`max_players = 1`): Room does NOT enter Lobby
//   state per `should_enter_lobby()`. Player immediately receives legacy peer metadata.
//
// - **Player Disconnection in Lobby**: If player disconnects and room drops
//   below `max_players`, room reverts to Waiting state and all ready states
//   are cleared.
//
// - **Authority Player Leaves**: If the authority player disconnects, authority
//   is cleared (`authority_player = None`) with no automatic reassignment.
//
// - **Stale Finalization**: Ready state version tracking prevents multiple
//   server instances from finalizing the same room (distributed lock protection).
//
// ## Timestamps and Activity Tracking
//
// Rooms track several timestamps for lifecycle management:
// - `created_at`: Room creation time
// - `last_activity`: Last message/event (updated via `update_activity()`)
// - `lobby_started_at`: When lobby state was entered
// - `game_finalized_at`: When game was finalized
//
// Activity is updated on: player joins/leaves, GameData messages, ready toggles,
// and authority requests.
//
// ## Full Documentation
//
// For complete details including:
// - Player lifecycle within rooms
// - Authority protocol rules
// - Spectator lifecycle
// - Reconnection flow
// - Message flow examples
// - Code references and test coverage
//
// See: [`docs/architecture/room-lifecycle.md`](../../../docs/architecture/room-lifecycle.md)
//
// ============================================================================

/// Room lobby state
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
pub enum LobbyState {
    #[default]
    Waiting,
    Lobby,
    Finalized,
}

/// Room configuration and state
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Room {
    pub id: RoomId,
    pub code: String,
    pub game_name: String,
    pub max_players: u8,
    pub supports_authority: bool,
    pub players: HashMap<PlayerId, PlayerInfo>,
    pub authority_player: Option<PlayerId>,
    pub lobby_state: LobbyState,
    pub ready_players: Vec<PlayerId>,
    pub lobby_started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub game_finalized_at: Option<chrono::DateTime<chrono::Utc>>,
    pub relay_type: String,
    /// Deployment region currently hosting this room.
    pub region_id: String,
    /// Owning application for per-app rate limiting and access control.
    pub application_id: Option<Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_activity: chrono::DateTime<chrono::Utc>,
    /// Spectators watching the room (read-only observers)
    pub spectators: HashMap<PlayerId, SpectatorInfo>,
    /// Maximum number of spectators allowed (None = unlimited)
    pub max_spectators: Option<u8>,
}

impl Room {
    #[allow(dead_code)]
    pub fn new(
        game_name: String,
        room_code: String,
        max_players: u8,
        supports_authority: bool,
        relay_type: String,
    ) -> Self {
        let now = chrono::Utc::now();
        Self {
            id: Uuid::new_v4(),
            code: room_code,
            game_name,
            max_players,
            supports_authority,
            players: HashMap::new(),
            authority_player: None,
            lobby_state: LobbyState::Waiting,
            ready_players: Vec::new(),
            lobby_started_at: None,
            game_finalized_at: None,
            relay_type,
            region_id: DEFAULT_REGION_ID.to_string(),
            application_id: None,
            created_at: now,
            last_activity: now,
            spectators: HashMap::new(),
            max_spectators: None, // Unlimited spectators by default
        }
    }

    /// Update the last activity timestamp
    #[allow(dead_code)]
    pub fn update_activity(&mut self) {
        self.last_activity = chrono::Utc::now();
    }

    /// Check if room is expired based on the given timeouts
    #[allow(dead_code)]
    pub fn is_expired(
        &self,
        empty_timeout: chrono::Duration,
        inactive_timeout: chrono::Duration,
    ) -> bool {
        let now = chrono::Utc::now();

        if self.players.is_empty() {
            // Empty room - check against creation time
            now.signed_duration_since(self.created_at) > empty_timeout
        } else {
            // Room has players - check against last activity
            now.signed_duration_since(self.last_activity) > inactive_timeout
        }
    }

    #[allow(dead_code)]
    pub fn can_join(&self) -> bool {
        self.players.len() < self.max_players as usize
    }

    #[allow(dead_code)]
    pub fn add_player(&mut self, player: PlayerInfo) -> bool {
        if self.can_join() {
            self.players.insert(player.id, player);
            true
        } else {
            false
        }
    }

    #[allow(dead_code)]
    pub fn remove_player(&mut self, player_id: &PlayerId) -> Option<PlayerInfo> {
        let removed = self.players.remove(player_id);

        // If the authority player left, clear authority
        if self.authority_player == Some(*player_id) {
            self.authority_player = None;
        }

        removed
    }

    #[allow(dead_code)]
    pub fn set_authority(&mut self, player_id: Option<PlayerId>) -> bool {
        // Check if room supports authority
        if !self.supports_authority {
            return false;
        }

        match player_id {
            Some(id) if self.players.contains_key(&id) => {
                // Remove authority from previous player
                if let Some(prev_auth) = self.authority_player {
                    if let Some(player) = self.players.get_mut(&prev_auth) {
                        player.is_authority = false;
                    }
                }

                // Set new authority
                self.authority_player = Some(id);
                if let Some(player) = self.players.get_mut(&id) {
                    player.is_authority = true;
                }
                true
            }
            None => {
                // Clear authority
                if let Some(prev_auth) = self.authority_player.take() {
                    if let Some(player) = self.players.get_mut(&prev_auth) {
                        player.is_authority = false;
                    }
                }
                true
            }
            Some(_) => false, // Player not in room
        }
    }

    #[allow(dead_code)]
    pub fn clear_authority(&mut self) -> bool {
        self.set_authority(None)
    }

    /// Check if room should transition to lobby (ready-coordination) state.
    ///
    /// `max_players` is a *ceiling*, not a required count: a room enters the
    /// lobby as soon as it has at least one player (the creator), so players can
    /// ready up while the room continues to fill up to `max_players`. The game
    /// does not start automatically when all are ready — an explicit `StartGame`
    /// from the authority (or any member if no authority is set) finalizes it.
    #[allow(dead_code)]
    pub fn should_enter_lobby(&self) -> bool {
        self.lobby_state == LobbyState::Waiting && !self.players.is_empty()
    }

    /// Transition room to lobby state
    #[allow(dead_code)]
    pub fn enter_lobby(&mut self) -> bool {
        if self.should_enter_lobby() {
            self.lobby_state = LobbyState::Lobby;
            self.lobby_started_at = Some(chrono::Utc::now());
            self.ready_players.clear();
            true
        } else {
            false
        }
    }

    /// Mark a player as ready in the lobby.
    ///
    /// Readiness can be toggled at any time before the game is `Finalized`; the
    /// room need not be full.
    #[allow(dead_code)]
    pub fn set_player_ready(&mut self, player_id: &PlayerId, ready: bool) -> bool {
        if self.lobby_state == LobbyState::Finalized || !self.players.contains_key(player_id) {
            return false;
        }

        if ready && !self.ready_players.contains(player_id) {
            self.ready_players.push(*player_id);
        } else if !ready {
            self.ready_players.retain(|id| id != player_id);
        }

        // Update player ready status
        if let Some(player) = self.players.get_mut(player_id) {
            player.is_ready = ready;
        }

        true
    }

    /// Check if all current players are ready (the `StartGame` precondition).
    ///
    /// True when the room is not yet `Finalized`, has at least one player, and
    /// every current player is ready. `max_players` is not consulted — a room
    /// can be all-ready (and so startable) before it is full.
    #[allow(dead_code)]
    pub fn all_players_ready(&self) -> bool {
        if self.lobby_state == LobbyState::Finalized {
            return false;
        }
        self.ready_players.len() == self.players.len() && !self.players.is_empty()
    }

    /// Finalize the game and prepare legacy peer metadata.
    ///
    /// Finalization is driven by an explicit `StartGame` (whose authorization is
    /// enforced by the handler); this transition only requires that the room is
    /// not already `Finalized` and that every current player is ready.
    #[allow(dead_code)]
    pub fn finalize_game(&mut self) -> bool {
        if self.lobby_state != LobbyState::Finalized && self.all_players_ready() {
            self.lobby_state = LobbyState::Finalized;
            self.game_finalized_at = Some(chrono::Utc::now());
            true
        } else {
            false
        }
    }

    /// Get legacy `GameStarting` peer metadata for all players.
    #[allow(dead_code)]
    pub fn get_peer_connections(&self) -> Vec<PeerConnectionInfo> {
        PeerConnectionInfo::from_players(self.players.values(), &self.relay_type)
    }

    /// Check if room is finalized and ready for cleanup
    #[allow(dead_code)]
    pub fn is_finalized(&self) -> bool {
        self.lobby_state == LobbyState::Finalized
    }

    /// Check if spectators can join this room
    #[allow(dead_code)]
    pub fn can_spectate(&self) -> bool {
        if let Some(max_spectators) = self.max_spectators {
            self.spectators.len() < max_spectators as usize
        } else {
            true // Unlimited spectators
        }
    }

    /// Add a spectator to the room
    #[allow(dead_code)]
    pub fn add_spectator(&mut self, spectator: SpectatorInfo) -> bool {
        if self.can_spectate() {
            self.spectators.insert(spectator.id, spectator);
            true
        } else {
            false
        }
    }

    /// Remove a spectator from the room
    #[allow(dead_code)]
    pub fn remove_spectator(&mut self, spectator_id: &PlayerId) -> Option<SpectatorInfo> {
        self.spectators.remove(spectator_id)
    }

    /// Get list of all spectators
    #[allow(dead_code)]
    pub fn get_spectators(&self) -> Vec<SpectatorInfo> {
        self.spectators.values().cloned().collect()
    }
}
