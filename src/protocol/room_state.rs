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
// Waiting --> Lobby: First player present (max_players is a ceiling, not a gate)
// Lobby --> Finalized: Explicit StartGame (all current players ready)
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
//   - Readiness toggles are honored and carried into the Lobby promotion
//   - Room can expire if empty for too long
//
// ### 2. Lobby State
//
// - **Description**: Room has at least one player and players are coordinating
//   readiness to start the game.
// - **Characteristics**:
//   - Room has ≥1 player; `max_players` is a ceiling, not a required count, so
//     the room keeps accepting players (up to `max_players`) while in Lobby
//   - Players can mark themselves ready/unready via `PlayerReady` messages
//   - `ready_players` list tracks who is ready
//   - Transitions to Finalized on an explicit `StartGame` (all current players
//     ready); a full all-ready room does NOT auto-start
//   - Broadcasts `LobbyStateChanged` (with `all_ready`) when ready state changes
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
//   - No further state transitions possible: Finalized is terminal. Departures
//     never regress room state, and post-finalize `PlayerReady` toggles are
//     rejected with `INVALID_ROOM_STATE`
//   - A Finalized room with an open seat (a member departed) still accepts
//     joins — `add_player_to_room` gates on fullness, plus the running-session
//     capability gate (issue #421: a joiner that cannot run the finalized
//     non-relay session is rejected with `ROOM_SESSION_INCOMPATIBLE`) — so
//     capable seat-filling late joins enter the running session without
//     replaying the lobby cycle
//
// ## Key State Transitions and Protocol Messages
//
// ### Waiting → Lobby
// - **Trigger**: The first player is present (the creator). `max_players` is a
//   ceiling, not a gate — the room enters Lobby immediately and keeps filling.
// - **Condition**: `should_enter_lobby()` returns true (Waiting and non-empty)
// - **Action**: Calls `enter_lobby()`, sets `lobby_started_at` timestamp
// - **Message**: Broadcasts `LobbyStateChanged` with `lobby_state: "lobby"`
//
// ### Lobby → Finalized
// - **Trigger**: An explicit `StartGame` from the authority — or any member when
//   no authority is set. Readiness alone does NOT finalize: `handle_player_ready`
//   only records readiness and broadcasts `all_ready` so clients know `StartGame`
//   is now permitted.
// - **Condition**: The room coordinator's `handle_start_game` (holding the
//   room-operation lock) re-checks that every current member is ready
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
//   |<-- LobbyStateChanged-|   (state: lobby; the     |
//   |                      |    one-time Waiting→Lobby|
//   |                      |    promotion snapshot)   |
//   |                      |<------- JoinRoom --------|
//   |<-- PlayerJoined -----|--- RoomJoined ---------->|
//   |-- PlayerReady ------>|                          |
//   |<-- LobbyStateChanged-|--- LobbyStateChanged --->|
//   |                      |<------- PlayerReady -----|
//   |<-- LobbyStateChanged-|--- LobbyStateChanged --->|
//   |                      |   (all_ready: true)      |
//   |-- StartGame -------->|   (creator finalizes)    |
//   |<-- GameStarting -----|--- GameStarting -------->|
// ```
//
// ## Related Client Messages
//
// - `JoinRoom`: Join or create a room (triggers room creation or player join)
// - `PlayerReady`: Toggle player ready state in lobby
// - `StartGame`: Finalize the lobby (authority, or any member if none set); the
//   server requires every current player ready, then broadcasts `GameStarting`
// - `LeaveRoom`: Leave a room without regressing its lifecycle state
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
// - **Single Player Rooms** (`max_players = 1`): The room enters Lobby as soon
//   as the lone player joins (`should_enter_lobby()` requires only ≥1 player).
//   The player readies and sends `StartGame`; the server permits a solo start
//   (min 1 ready), finalizing the room.
//
// - **Player Disconnection in Lobby**: The room remains in Lobby. The departing
//   player is removed from readiness; remaining members keep their ready state
//   and may still start once every current member is ready.
//
// - **Authority Player Leaves**: If the authority player disconnects, authority
//   is cleared (`authority_player = None`) with no automatic reassignment.
//
// - **Stale Finalization**: Ready-state version tracking and the process-local
//   room lock prevent concurrent handlers from finalizing the same room twice.
//
// ## Timestamps and Activity Tracking
//
// Rooms track several timestamps for lifecycle management:
// - `created_at`: Room creation time
// - `last_activity`: Last message/event (updated via `update_activity()`)
// - `lobby_started_at`: When lobby state was entered
// - `game_finalized_at`: When game was finalized
//
// Activity is updated on: player and spectator joins/leaves, throttled transport
// activity, GameData messages, ready toggles, and authority requests.
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
// See: [`docs/concepts/rooms-and-lobbies.md`](../../../docs/concepts/rooms-and-lobbies.md)
//
// ============================================================================

/// Room lobby state
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
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
    /// Deployment-defined legacy integration label persisted with the room.
    ///
    /// Informational only: it does not select or prove an active transport.
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
        // Wall clock (durable record): creation/last-activity stamps must stay
        // meaningful across process restarts.
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
        // Wall clock (durable record): `last_activity` keys the GC windows,
        // which must survive process restarts; the in-process GC decision
        // pairs this stamp with a monotonic liveness stamp (see
        // `InMemoryDatabase`).
        self.last_activity = chrono::Utc::now();
    }

    /// Check if room is expired at the given instant, based on the given
    /// timeouts.
    ///
    /// This is the decision seam: the comparison runs entirely on the caller's
    /// `now`, so expiry boundaries are deterministic to test and immune to
    /// wall-clock steps. The production GC decision lives in
    /// `InMemoryDatabase`'s monotonic liveness path; this method is an embedder
    /// surface over the same `last_activity` policy.
    #[allow(dead_code)]
    pub fn is_expired_at(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        empty_timeout: chrono::Duration,
        inactive_timeout: chrono::Duration,
    ) -> bool {
        if !self.has_occupants() {
            // Empty room - time from the LAST activity (which `remove_player_from_room`
            // refreshes on the final departure), not `created_at`. A long-lived room
            // that just emptied must get the full `empty_timeout` window from when it
            // emptied, otherwise it is deleted immediately off a stale creation time,
            // collapsing the reconnection window (BUG-1 corollary B). This also keeps
            // the two cleanup paths (`cleanup_empty_rooms` / `cleanup_expired_rooms`)
            // consistent, since the former already keys off `last_activity`.
            now.signed_duration_since(self.last_activity) > empty_timeout
        } else {
            // Occupied room - check against last activity
            now.signed_duration_since(self.last_activity) > inactive_timeout
        }
    }

    /// Check if room is expired based on the given timeouts.
    ///
    /// Wall clock (embedder convenience): the server's own GC decision never
    /// comes through here (see `is_expired_at` and the monotonic liveness path
    /// in `InMemoryDatabase`).
    #[allow(dead_code)]
    pub fn is_expired(
        &self,
        empty_timeout: chrono::Duration,
        inactive_timeout: chrono::Duration,
    ) -> bool {
        self.is_expired_at(chrono::Utc::now(), empty_timeout, inactive_timeout)
    }

    /// Whether a player or spectator still occupies this room.
    pub fn has_occupants(&self) -> bool {
        !self.players.is_empty() || !self.spectators.is_empty()
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

        // Readiness follows the membership: leaving an id in `ready_players`
        // would let a later admission read it back as readiness this membership
        // never declared (see `GameDatabase::add_player_to_room`).
        self.ready_players.retain(|id| id != player_id);

        // If the authority player left, clear authority. Guarded by an actual
        // removal, and clearing every member's flag, so the authority handling
        // matches `InMemoryDatabase::remove_player_from_room`.
        if removed.is_some() && self.authority_player == Some(*player_id) {
            self.authority_player = None;
            for player in self.players.values_mut() {
                player.is_authority = false;
            }
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
            // Wall clock (durable record): lifecycle timestamps are absolute
            // by design.
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
            // Wall clock (durable record): lifecycle timestamps are absolute
            // by design.
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
            self.update_activity();
            true
        } else {
            false
        }
    }

    /// Remove a spectator from the room
    #[allow(dead_code)]
    pub fn remove_spectator(&mut self, spectator_id: &PlayerId) -> Option<SpectatorInfo> {
        let removed = self.spectators.remove(spectator_id);
        if removed.is_some() {
            self.update_activity();
        }
        removed
    }

    /// Get list of all spectators
    #[allow(dead_code)]
    pub fn get_spectators(&self) -> Vec<SpectatorInfo> {
        self.spectators.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_room() -> Room {
        Room::new(
            "game".to_string(),
            "CODE01".to_string(),
            4,
            true,
            "relay".to_string(),
        )
    }

    /// `is_expired_at` times every window off `last_activity` at the caller's
    /// `now`, so the whole matrix is deterministic:
    /// - an empty room uses `empty_timeout` from the FINAL departure refresh,
    ///   NOT `created_at` (BUG-1 corollary B);
    /// - an occupied room (players or spectators) uses `inactive_timeout`
    ///   (BUG-1 corollary A);
    /// - the boundary is strict: exactly at the timeout the room survives.
    #[test]
    fn is_expired_at_times_windows_off_last_activity_at_the_callers_now() {
        let empty = chrono::Duration::seconds(300);
        let inactive = chrono::Duration::seconds(3600);
        let now = chrono::Utc::now();

        let created_long_ago = now - chrono::Duration::hours(2);
        let player_room = |last_activity: chrono::DateTime<chrono::Utc>| {
            let mut room = empty_room();
            room.created_at = created_long_ago;
            room.last_activity = last_activity;
            let player_id = Uuid::new_v4();
            room.players.insert(
                player_id,
                PlayerInfo {
                    id: player_id,
                    name: "P".to_string(),
                    is_authority: false,
                    is_ready: false,
                    connected_at: last_activity,
                    connection_info: None,
                    epoch: None,
                    seq: None,
                    region_id: "us-east-1".to_string(),
                },
            );
            room
        };
        let spectator_room = |last_activity: chrono::DateTime<chrono::Utc>| {
            let mut room = empty_room();
            room.created_at = created_long_ago;
            room.last_activity = last_activity;
            let spectator_id = Uuid::new_v4();
            room.spectators.insert(
                spectator_id,
                SpectatorInfo {
                    id: spectator_id,
                    name: "Watcher".to_string(),
                    connected_at: last_activity,
                },
            );
            room
        };

        let cases: Vec<(Room, bool, &str)> = vec![
            (
                {
                    let mut room = empty_room();
                    room.created_at = created_long_ago;
                    room.last_activity = now;
                    room
                },
                false,
                "freshly-emptied long-lived room: survives off fresh last_activity, \
                 not the stale creation time",
            ),
            (
                {
                    let mut room = empty_room();
                    room.created_at = created_long_ago;
                    room.last_activity = now - empty;
                    room
                },
                false,
                "empty room exactly at empty_timeout: boundary is strict",
            ),
            (
                {
                    let mut room = empty_room();
                    room.created_at = created_long_ago;
                    room.last_activity = now - empty - chrono::Duration::seconds(1);
                    room
                },
                true,
                "empty room past empty_timeout from the last departure: reaped",
            ),
            (
                player_room(now),
                false,
                "active room with fresh activity: alive",
            ),
            (
                player_room(now - inactive),
                false,
                "occupied room exactly at inactive_timeout: boundary is strict",
            ),
            (
                player_room(now - inactive - chrono::Duration::seconds(1)),
                true,
                "occupied room past inactive_timeout: reaped",
            ),
            (
                spectator_room(now - inactive),
                false,
                "spectator-only room is occupied: inactive_timeout, boundary strict",
            ),
            (
                spectator_room(now - inactive - chrono::Duration::seconds(1)),
                true,
                "spectator-only room past inactive_timeout: reaped",
            ),
        ];

        for (room, expected_expired, description) in cases {
            assert_eq!(
                room.is_expired_at(now, empty, inactive),
                expected_expired,
                "{description}"
            );
        }
    }

    fn watcher(id: Uuid) -> SpectatorInfo {
        SpectatorInfo {
            id,
            name: "Watcher".to_string(),
            connected_at: chrono::Utc::now(),
        }
    }

    /// The spectator-cap contract behind `TOO_MANY_SPECTATORS`: admissions are
    /// allowed strictly below the configured maximum (`<`, never `<=`), a full
    /// room refuses without mutating its roster, an uncapped room always
    /// admits, and a departure frees exactly the seat it occupied.
    #[test]
    fn spectator_cap_admits_up_to_the_limit_and_frees_on_departure() {
        let mut room = empty_room();
        assert!(
            room.can_spectate(),
            "an uncapped room admits spectators unconditionally"
        );

        room.max_spectators = Some(2);
        let departed = Uuid::new_v4();
        assert!(room.add_spectator(watcher(Uuid::new_v4())));
        assert!(room.add_spectator(watcher(departed)));
        assert!(!room.can_spectate(), "a full room refuses new spectators");
        assert!(
            !room.add_spectator(watcher(Uuid::new_v4())),
            "refusal must not mutate the roster"
        );
        assert_eq!(room.get_spectators().len(), 2);

        room.remove_spectator(&departed);
        assert!(room.can_spectate(), "departure frees exactly one seat");
        assert!(room.add_spectator(watcher(departed)));
    }
}
