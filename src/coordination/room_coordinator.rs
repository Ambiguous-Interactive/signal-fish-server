//! Room operation coordination for distributed state management
//!
//! This module provides coordinators for managing room operations (lobby transitions,
//! authority transfers, player ready states) with distributed locking to ensure consistency.

use anyhow::Result;
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::distributed::{DistributedLock, LockHandle};
use crate::protocol::{PeerConnectionInfo, PlayerId, PlayerInfo, RoomId};

use super::MessageCoordinator;

const LOBBY_TRANSITION_LOCK_TTL: Duration = Duration::from_secs(10);
const ROOM_OPERATION_LOCK_TTL: Duration = Duration::from_secs(5);

/// Snapshot of a room at finalization, surfaced so the capability-aware server
/// layer can compute and emit the v3 SessionPlan after `GameStarting`.
///
/// Returned by [`RoomOperationCoordinatorTrait::handle_start_game`] on the
/// finalize path (all current players ready and the sender authorized).
#[derive(Debug, Clone)]
pub struct FinalizedRoom {
    /// The room's game name (used for per-game topology selection).
    pub game_name: String,
    /// The current authority player, if any (preferred host in `host` topology).
    pub authority_player: Option<PlayerId>,
    /// The finalized member list (the players that received `GameStarting`).
    pub members: Vec<PlayerInfo>,
}

/// The outcome of an explicit `StartGame`
/// ([`RoomOperationCoordinatorTrait::handle_start_game`]).
#[derive(Debug, Clone)]
pub enum StartGameOutcome {
    /// The room finalized; `GameStarting` has already been broadcast and the
    /// caller should emit the v3 `SessionPlan` from this snapshot.
    Started(FinalizedRoom),
    /// Not every current player is ready (maps to `GAME_START_NOT_READY`).
    NotReady,
    /// The sender is not permitted to start (the room has a designated
    /// authority and the sender is not it; maps to `GAME_START_FORBIDDEN`).
    Forbidden,
    /// The room is already `Finalized` (maps to `INVALID_ROOM_STATE`).
    AlreadyStarted,
}

/// Trait for room operation coordination
#[async_trait]
pub trait RoomOperationCoordinatorTrait: Send + Sync {
    /// Transition a room to lobby state
    async fn transition_room_to_lobby(&self, room_id: &RoomId) -> Result<bool>;

    /// Coordinate authority transfer between players
    async fn coordinate_authority_transfer(
        &self,
        room_id: &RoomId,
        new_authority: &PlayerId,
    ) -> Result<bool>;

    /// Execute a distributed operation on a room
    async fn execute_distributed_operation(&self, operation: &str, room_id: &RoomId) -> Result<()>;

    /// Handle authority request from a player
    async fn handle_authority_request(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        become_authority: bool,
    ) -> Result<(bool, Option<String>)>;

    /// Handle a player ready-state toggle.
    ///
    /// Records the toggle and broadcasts the updated `LobbyStateChanged` (with
    /// `all_ready` once every current player is ready). Readiness no longer
    /// starts the game — finalization is driven by an explicit `StartGame`
    /// ([`Self::handle_start_game`]).
    async fn handle_player_ready(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        app_id: Option<Uuid>,
    ) -> Result<()>;

    /// Handle an explicit `StartGame` from `player_id`.
    ///
    /// Finalizes the room with its *current* members when every current player
    /// is ready and the sender is authorized (the room's authority, or any
    /// member if no authority is set). See [`StartGameOutcome`].
    async fn handle_start_game(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
    ) -> Result<StartGameOutcome>;

    /// The current ready set for a room, filtered to present members.
    ///
    /// The live ready state is tracked by the coordinator (not persisted to the
    /// room record until finalize), so `RoomJoined` / `Reconnected` must read it
    /// from here to report an accurate ready set to a player joining a lobby that
    /// already has ready members. Returns an empty vec for an unknown room.
    async fn current_ready_players(&self, room_id: &RoomId) -> Vec<PlayerId>;

    /// Clear ready players for a room
    async fn clear_ready_players(&self, room_id: &RoomId) -> Result<()>;
}

/// In-memory room operation coordinator
pub struct InMemoryRoomOperationCoordinator {
    coordinator: Arc<dyn MessageCoordinator>,
    distributed_lock: Arc<dyn DistributedLock>,
    database: Arc<dyn crate::database::GameDatabase>,
    /// Track ready players per room for in-memory coordinator
    ready_players: Arc<RwLock<HashMap<RoomId, HashSet<PlayerId>>>>,
}

impl InMemoryRoomOperationCoordinator {
    /// Create a new in-memory room operation coordinator
    pub fn new(
        coordinator: Arc<dyn MessageCoordinator>,
        distributed_lock: Arc<dyn DistributedLock>,
        database: Arc<dyn crate::database::GameDatabase>,
    ) -> Self {
        Self {
            coordinator,
            distributed_lock,
            database,
            ready_players: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

struct RoomOperationLockGuard {
    distributed_lock: Arc<dyn DistributedLock>,
    handle: Option<LockHandle>,
    operation: &'static str,
}

impl RoomOperationLockGuard {
    async fn acquire(
        distributed_lock: Arc<dyn DistributedLock>,
        lock_key: String,
        ttl: Duration,
        operation: &'static str,
    ) -> Result<Self> {
        let handle = distributed_lock.acquire(&lock_key, ttl).await?;
        Ok(Self {
            distributed_lock,
            handle: Some(handle),
            operation,
        })
    }

    async fn release(mut self) {
        if let Some(handle) = self.handle.clone() {
            Self::release_handle(Arc::clone(&self.distributed_lock), handle, self.operation).await;
            self.handle = None;
        }
    }

    async fn release_handle(
        distributed_lock: Arc<dyn DistributedLock>,
        handle: LockHandle,
        operation: &'static str,
    ) {
        match distributed_lock.release(&handle).await {
            Ok(true) => {
                tracing::trace!(
                    operation,
                    lock_key = %handle.key,
                    "Released room operation lock"
                );
            }
            Ok(false) => {
                tracing::warn!(
                    operation,
                    lock_key = %handle.key,
                    "Room operation lock was already absent during release"
                );
            }
            Err(error) => {
                tracing::warn!(
                    operation,
                    lock_key = %handle.key,
                    error = ?error,
                    "Failed to release room operation lock"
                );
            }
        }
    }
}

impl Drop for RoomOperationLockGuard {
    fn drop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };

        let distributed_lock = Arc::clone(&self.distributed_lock);
        let operation = self.operation;

        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                tracing::trace!(
                    operation,
                    lock_key = %handle.key,
                    "Scheduling async release for dropped room operation lock guard"
                );
                let release_task = runtime.spawn(async move {
                    Self::release_handle(distributed_lock, handle, operation).await;
                });
                std::mem::drop(release_task);
            }
            Err(error) => {
                tracing::warn!(
                    operation,
                    lock_key = %handle.key,
                    error = ?error,
                    "Unable to schedule async release for dropped room operation lock guard"
                );
            }
        }
    }
}

#[async_trait]
impl RoomOperationCoordinatorTrait for InMemoryRoomOperationCoordinator {
    async fn transition_room_to_lobby(&self, room_id: &RoomId) -> Result<bool> {
        // For in-memory implementation, just simulate the operation
        let lock_key = format!("room_lobby_transition:{room_id}");
        let lock_guard = RoomOperationLockGuard::acquire(
            Arc::clone(&self.distributed_lock),
            lock_key,
            LOBBY_TRANSITION_LOCK_TTL,
            "transition_room_to_lobby",
        )
        .await?;

        let result = async {
            // Enter the lobby EXACTLY ONCE. `should_enter_lobby` is now true for
            // any non-empty `Waiting` room (`max_players` is a ceiling), so the
            // join path calls this on every join; persisting the transition and
            // short-circuiting a room that already left `Waiting` keeps a room
            // from re-broadcasting `LobbyStateChanged` on each join. A late
            // joiner therefore reads an accurate `lobby_state: Lobby` in its
            // `RoomJoined` and receives no redundant follow-up.
            match self.database.get_room_by_id(room_id).await {
                Ok(Some(room)) if room.lobby_state == crate::protocol::LobbyState::Waiting => {}
                Ok(Some(_)) => return Ok(false), // already Lobby/Finalized: no-op
                Ok(None) => return Err(anyhow::anyhow!("Room not found")),
                Err(e) => return Err(anyhow::anyhow!("Failed to get room: {e}")),
            }

            // Persist Waiting -> Lobby, then broadcast the single transition.
            self.database.transition_room_to_lobby(room_id).await?;

            let message = crate::protocol::ServerMessage::LobbyStateChanged {
                lobby_state: crate::protocol::LobbyState::Lobby,
                ready_players: Vec::new(),
                all_ready: false,
            };
            self.coordinator
                .broadcast_to_room(room_id, Arc::new(message))
                .await?;
            tracing::info!(%room_id, "Room transitioned to lobby state (in-memory)");
            Ok(true)
        }
        .await;

        lock_guard.release().await;
        result
    }

    async fn coordinate_authority_transfer(
        &self,
        room_id: &RoomId,
        new_authority: &PlayerId,
    ) -> Result<bool> {
        // For in-memory implementation, just simulate the operation
        let lock_key = format!("authority_transfer:{room_id}:{new_authority}");
        let lock_guard = RoomOperationLockGuard::acquire(
            Arc::clone(&self.distributed_lock),
            lock_key,
            ROOM_OPERATION_LOCK_TTL,
            "coordinate_authority_transfer",
        )
        .await?;

        let result = async {
            // Simulate successful authority transfer
            let message = crate::protocol::ServerMessage::AuthorityChanged {
                authority_player: Some(*new_authority),
                you_are_authority: false, // Will be customized per client
            };

            self.coordinator
                .broadcast_to_room(room_id, Arc::new(message))
                .await?;
            tracing::info!(%room_id, %new_authority, "Authority transferred (in-memory)");
            Ok(true)
        }
        .await;

        lock_guard.release().await;
        result
    }

    async fn execute_distributed_operation(&self, operation: &str, room_id: &RoomId) -> Result<()> {
        // For in-memory implementation, just log the operation
        let lock_key = format!("distributed_op:{room_id}:{operation}");
        let lock_guard = RoomOperationLockGuard::acquire(
            Arc::clone(&self.distributed_lock),
            lock_key,
            ROOM_OPERATION_LOCK_TTL,
            "execute_distributed_operation",
        )
        .await?;

        tracing::info!(%room_id, %operation, "Executed distributed operation (in-memory)");
        lock_guard.release().await;
        Ok(())
    }

    async fn handle_authority_request(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        become_authority: bool,
    ) -> Result<(bool, Option<String>)> {
        // For in-memory implementation, use the actual database authority request method
        let lock_key = format!("room_authority:{room_id}");
        let lock_guard = RoomOperationLockGuard::acquire(
            Arc::clone(&self.distributed_lock),
            lock_key,
            ROOM_OPERATION_LOCK_TTL,
            "handle_authority_request",
        )
        .await?;

        let result = async {
            tracing::info!(%room_id, %player_id, %become_authority, "InMemory: Processing authority request");

            // Use the database's atomic authority request method
            let result = self
                .database
                .request_room_authority(room_id, player_id, become_authority)
                .await;

            match result {
                Ok((granted, reason)) => {
                    if granted {
                        // Broadcast authority change to all players
                        let new_authority = if become_authority {
                            Some(*player_id)
                        } else {
                            None
                        };

                        // First send specific authority response to the requesting player
                        tracing::info!(%room_id, %player_id, "Sending AuthorityResponse to player");
                        let response_message = crate::protocol::ServerMessage::AuthorityResponse {
                            granted,
                            reason: reason.clone(),
                            error_code: if granted {
                                None
                            } else {
                                Some(crate::protocol::ErrorCode::AuthorityDenied)
                            },
                        };

                        if let Err(e) = self
                            .coordinator
                            .send_to_player(player_id, Arc::new(response_message))
                            .await
                        {
                            tracing::error!("Failed to send authority response to player: {}", e);
                        }

                        // Instead of broadcasting, we need to send a custom message to each player
                        // to set the you_are_authority flag correctly
                        tracing::info!(%room_id, "Sending customized AuthorityChanged messages");

                        // Get all player IDs in the room from database
                        let room = match self.database.get_room_by_id(room_id).await {
                            Ok(Some(room)) => room,
                            Ok(None) => {
                                tracing::error!(%room_id, "Room not found when handling authority change");
                                return Ok((granted, reason));
                            }
                            Err(e) => {
                                tracing::error!(%room_id, "Failed to get room: {}", e);
                                return Ok((granted, reason));
                            }
                        };

                        // Send a customized message to each player in the room
                        for room_player_id in room.players.keys() {
                            // This player is the authority if they requested authority and it was granted
                            let is_authority = become_authority && room_player_id == player_id;

                            let auth_message = crate::protocol::ServerMessage::AuthorityChanged {
                                authority_player: new_authority,
                                you_are_authority: is_authority,
                            };

                            tracing::info!(
                                %room_id,
                                %room_player_id,
                                %is_authority,
                                "Sending customized AuthorityChanged message"
                            );

                            if let Err(e) = self
                                .coordinator
                                .send_to_player(room_player_id, Arc::new(auth_message))
                                .await
                            {
                                tracing::error!(%room_player_id, "Failed to send authority change: {}", e);
                            }
                        }

                        tracing::info!(%room_id, %player_id, %become_authority, "Authority request granted (in-memory)");
                    } else {
                        // Send denial response to the requesting player
                        let response_message = crate::protocol::ServerMessage::AuthorityResponse {
                            granted,
                            reason: reason.clone(),
                            error_code: if granted {
                                None
                            } else {
                                Some(crate::protocol::ErrorCode::AuthorityDenied)
                            },
                        };

                        if let Err(e) = self
                            .coordinator
                            .send_to_player(player_id, Arc::new(response_message))
                            .await
                        {
                            tracing::error!(
                                "Failed to send authority denial response to player: {}",
                                e
                            );
                        }

                        tracing::info!(%room_id, %player_id, %become_authority, ?reason, "Authority request denied (in-memory)");
                    }

                    Ok((granted, reason))
                }
                Err(e) => {
                    tracing::error!(%room_id, %player_id, %become_authority, "Authority request failed: {}", e);

                    // Send error response to the requesting player
                    let response_message = crate::protocol::ServerMessage::AuthorityResponse {
                        granted: false,
                        reason: Some("Storage error".to_string()),
                        error_code: Some(crate::protocol::ErrorCode::StorageError),
                    };

                    if let Err(send_err) = self
                        .coordinator
                        .send_to_player(player_id, Arc::new(response_message))
                        .await
                    {
                        tracing::error!("Failed to send error response to player: {}", send_err);
                    }

                    Ok((false, Some("Storage error".to_string())))
                }
            }
        }
        .await;

        lock_guard.release().await;
        result
    }

    async fn handle_player_ready(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        _app_id: Option<Uuid>,
    ) -> Result<()> {
        // For in-memory implementation, simulate player ready toggle
        let lock_key = format!("room_ready_state:{room_id}");
        let lock_guard = RoomOperationLockGuard::acquire(
            Arc::clone(&self.distributed_lock),
            lock_key,
            ROOM_OPERATION_LOCK_TTL,
            "handle_player_ready",
        )
        .await?;

        let result = async {
            // Get current room state to check if it has enough players for lobby actions
            let room = match self.database.get_room_by_id(room_id).await {
                Ok(Some(room)) => room,
                Ok(None) => {
                    return Err(anyhow::anyhow!("Room not found"));
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("Failed to get room: {e}"));
                }
            };

            // Readiness can be toggled any time the room is open (`max_players`
            // is a ceiling, not a required count); only a `Finalized` room — the
            // game already started — rejects further ready toggles.
            if room.lobby_state == crate::protocol::LobbyState::Finalized {
                return Err(anyhow::anyhow!(
                    "Player ready failed: the game has already started (room is Finalized)"
                ));
            }

            // Fetch the live membership first so readiness is computed over the
            // current players (a departed player must not count toward
            // `all_ready`, nor linger in the broadcast ready list).
            let room_players = match self.database.get_room_players(room_id).await {
                Ok(players) => players,
                Err(e) => {
                    tracing::error!("Failed to get room players: {}", e);
                    Vec::new()
                }
            };
            let current_ids: HashSet<PlayerId> = room_players.iter().map(|p| p.id).collect();

            // Toggle player ready state in ready_players map
            let mut ready_map = self.ready_players.write().await;
            let room_ready_players = ready_map.entry(*room_id).or_insert_with(HashSet::new);

            let was_ready = room_ready_players.contains(player_id);
            if was_ready {
                room_ready_players.remove(player_id);
            } else {
                room_ready_players.insert(*player_id);
            }
            // Drop any ids that are no longer current members (departed players).
            room_ready_players.retain(|id| current_ids.contains(id));

            let ready_players_vec: Vec<PlayerId> = room_ready_players.iter().copied().collect();

            // Every current player ready (min 1). Robust to a ready id that just
            // departed: membership, not a raw count, decides `all_ready`.
            let all_ready = !room_players.is_empty()
                && room_players
                    .iter()
                    .all(|p| room_ready_players.contains(&p.id));

            drop(ready_map); // Release write lock

            // Broadcast lobby state change
            let message = crate::protocol::ServerMessage::LobbyStateChanged {
                lobby_state: crate::protocol::LobbyState::Lobby,
                ready_players: ready_players_vec.clone(),
                all_ready,
            };

            self.coordinator
                .broadcast_to_room(room_id, Arc::new(message))
                .await?;

            // Readiness no longer starts the game. `max_players` is a ceiling,
            // not a required count, and the game begins only on an explicit
            // `StartGame` (see `handle_start_game`) — so a `PlayerReady` toggle
            // just records readiness and broadcasts the new lobby snapshot
            // (with `all_ready` so clients know `StartGame` is now permitted).
            let _ = all_ready;
            tracing::info!(%room_id, %player_id, ready = !was_ready, "Player ready state toggled (in-memory)");
            Ok(())
        }
        .await;

        lock_guard.release().await;
        result
    }

    /// Handle an explicit `StartGame`: finalize the room with its *current*
    /// members when every current player is ready and the sender is permitted
    /// to start (the room's authority, or any member if no authority is set).
    ///
    /// Returns the [`StartGameOutcome`]: `Started(FinalizedRoom)` on success (the
    /// `GameStarting` broadcast has already been sent, so the caller only needs
    /// to emit the v3 `SessionPlan`), or a rejection variant the caller maps to
    /// the matching `ErrorCode`.
    async fn handle_start_game(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
    ) -> Result<StartGameOutcome> {
        let lock_key = format!("room_ready_state:{room_id}");
        let lock_guard = RoomOperationLockGuard::acquire(
            Arc::clone(&self.distributed_lock),
            lock_key,
            ROOM_OPERATION_LOCK_TTL,
            "handle_start_game",
        )
        .await?;

        let result = async {
            let room = match self.database.get_room_by_id(room_id).await {
                Ok(Some(room)) => room,
                Ok(None) => return Err(anyhow::anyhow!("Room not found")),
                Err(e) => return Err(anyhow::anyhow!("Failed to get room: {e}")),
            };

            // Already started: a finalized room cannot be started again.
            if room.lobby_state == crate::protocol::LobbyState::Finalized {
                return Ok(StartGameOutcome::AlreadyStarted);
            }

            // Authorization: a designated authority may start; otherwise any
            // member may. The sender is already known to be in this room.
            if let Some(authority) = room.authority_player {
                if authority != *player_id {
                    return Ok(StartGameOutcome::Forbidden);
                }
            }

            let room_players = match self.database.get_room_players(room_id).await {
                Ok(players) => players,
                Err(e) => {
                    tracing::error!("Failed to get room players: {}", e);
                    Vec::new()
                }
            };

            // All current players must be ready (min 1 — solo is allowed).
            let ready_map = self.ready_players.read().await;
            let ready_set = ready_map.get(room_id);
            let all_ready = !room_players.is_empty()
                && room_players
                    .iter()
                    .all(|p| ready_set.is_some_and(|set| set.contains(&p.id)));
            drop(ready_map);

            if !all_ready {
                return Ok(StartGameOutcome::NotReady);
            }

            // Persist the finalized lobby state BEFORE broadcasting, still under
            // the room-operation lock, so storage and clients agree the session
            // is live. Without this write the stored room stays `Lobby` forever:
            // a post-game departure would wrongly regress it and replay the
            // lobby cycle, and every `Finalized`-gated path (v3 late-join /
            // reconnect pairing, departure re-planning) would be unreachable.
            // Best-effort: a persist failure must not abort the v2 GameStarting
            // broadcast (the relay floor still works).
            if let Err(err) = self.database.finalize_room_game(room_id).await {
                tracing::warn!(%room_id, error = %err, "Failed to persist finalized lobby state");
            }

            // Re-read membership AFTER persisting `Finalized` so the broadcast and
            // the emitted plan reflect the authoritative finalized member set, not
            // the pre-check snapshot. `leave_room` is not serialized against this
            // lock, so a player could depart between the all-ready check above and
            // here; using the post-finalize read keeps `GameStarting.peer_connections`
            // and `FinalizedRoom.members` consistent with the persisted room (a
            // player who left before finalize is not named as a phantom peer/host).
            // Any departure AFTER finalize is a Finalized-room leave, handled by
            // `handle_session_member_departure` (PlayerLeft + v3 re-plan).
            let finalized_members = match self.database.get_room_players(room_id).await {
                Ok(players) if !players.is_empty() => players,
                // Fall back to the pre-check snapshot if the room vanished (e.g.
                // the last member left concurrently) — the broadcast below is then
                // best-effort and the departure path carries the relay floor.
                _ => room_players,
            };

            // Preserve the legacy GameStarting metadata; v3 transport proof comes
            // from negotiation and TransportStatus, not this payload.
            let peer_connections =
                PeerConnectionInfo::from_players(&finalized_members, &room.relay_type);
            let game_start_message =
                Arc::new(crate::protocol::ServerMessage::GameStarting { peer_connections });
            self.coordinator
                .broadcast_to_room(room_id, game_start_message)
                .await?;

            // Clear ready players for this room since the game is starting.
            let mut ready_map = self.ready_players.write().await;
            ready_map.remove(room_id);
            drop(ready_map);

            tracing::info!(%room_id, %player_id, "Game started via explicit StartGame (in-memory)");

            Ok(StartGameOutcome::Started(FinalizedRoom {
                game_name: room.game_name.clone(),
                authority_player: room.authority_player,
                members: finalized_members,
            }))
        }
        .await;

        lock_guard.release().await;
        result
    }

    async fn current_ready_players(&self, room_id: &RoomId) -> Vec<PlayerId> {
        // Snapshot the set without holding the lock across the DB await.
        let set: HashSet<PlayerId> = {
            let ready_map = self.ready_players.read().await;
            match ready_map.get(room_id) {
                Some(s) => s.clone(),
                None => return Vec::new(),
            }
        };
        // Filter to present members so an id that departed without a toggle is
        // never reported as ready.
        match self.database.get_room_players(room_id).await {
            Ok(players) => {
                let current: HashSet<PlayerId> = players.iter().map(|p| p.id).collect();
                set.into_iter().filter(|id| current.contains(id)).collect()
            }
            Err(_) => set.into_iter().collect(),
        }
    }

    async fn clear_ready_players(&self, room_id: &RoomId) -> Result<()> {
        // Clear ready players from the in-memory coordinator map
        let mut ready_map = self.ready_players.write().await;
        ready_map.remove(room_id);
        tracing::info!(%room_id, "Cleared ready players from coordinator (in-memory)");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordination::{MembershipUpdate, MessageCoordinator};
    use crate::database::{GameDatabase, InMemoryDatabase};
    use crate::distributed::{
        DistributedLock, InMemoryDistributedLock, LockHandle, SequencedMessage,
    };
    use crate::protocol::{ConnectionInfo, LobbyState, PeerConnectionInfo, ServerMessage};
    use async_trait::async_trait;
    use std::collections::{BTreeMap, HashSet};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{mpsc, Mutex, Notify};
    use tokio::time::{sleep, timeout};

    #[derive(Debug, Clone)]
    struct BroadcastEvent {
        room_id: RoomId,
        message: ServerMessage,
    }

    #[derive(Default)]
    struct RecordingMessageCoordinator {
        broadcasts: Mutex<Vec<BroadcastEvent>>,
        failed_broadcast_calls: Mutex<HashSet<usize>>,
        broadcast_attempts: AtomicUsize,
    }

    impl RecordingMessageCoordinator {
        async fn fail_broadcast_on(&self, call_number: usize) {
            self.failed_broadcast_calls.lock().await.insert(call_number);
        }

        async fn broadcasts(&self) -> Vec<BroadcastEvent> {
            self.broadcasts.lock().await.clone()
        }
    }

    #[async_trait]
    impl MessageCoordinator for RecordingMessageCoordinator {
        async fn send_to_player(
            &self,
            _player_id: &PlayerId,
            _message: Arc<ServerMessage>,
        ) -> Result<()> {
            Ok(())
        }

        async fn broadcast_to_room(
            &self,
            room_id: &RoomId,
            message: Arc<ServerMessage>,
        ) -> Result<()> {
            let call_number = self.broadcast_attempts.fetch_add(1, Ordering::SeqCst) + 1;
            let should_fail = self
                .failed_broadcast_calls
                .lock()
                .await
                .contains(&call_number);

            if should_fail {
                anyhow::bail!("injected broadcast failure on call {call_number}");
            }

            self.broadcasts.lock().await.push(BroadcastEvent {
                room_id: *room_id,
                message: (*message).clone(),
            });
            Ok(())
        }

        async fn broadcast_to_room_except(
            &self,
            _room_id: &RoomId,
            _except_player: &PlayerId,
            _message: Arc<ServerMessage>,
        ) -> Result<()> {
            Ok(())
        }

        async fn register_local_client(
            &self,
            _player_id: PlayerId,
            _room_id: Option<RoomId>,
            _sender: mpsc::Sender<Arc<ServerMessage>>,
        ) -> Result<()> {
            Ok(())
        }

        async fn unregister_local_client(&self, _player_id: &PlayerId) -> Result<()> {
            Ok(())
        }

        async fn should_process_message(&self, _message: &SequencedMessage) -> Result<bool> {
            Ok(true)
        }

        async fn mark_message_processed(&self, _message: &SequencedMessage) -> Result<()> {
            Ok(())
        }

        async fn handle_bus_message(&self, _message: SequencedMessage) -> Result<()> {
            Ok(())
        }

        async fn handle_membership_update(&self, _update: MembershipUpdate) -> Result<()> {
            Ok(())
        }
    }

    struct BlockingBroadcastMessageCoordinator {
        broadcast_started: Notify,
        release_broadcast: Notify,
    }

    impl BlockingBroadcastMessageCoordinator {
        fn new() -> Self {
            Self {
                broadcast_started: Notify::new(),
                release_broadcast: Notify::new(),
            }
        }

        async fn wait_for_broadcast_start(&self) {
            self.broadcast_started.notified().await;
        }

        fn release_broadcast(&self) {
            self.release_broadcast.notify_one();
        }
    }

    #[async_trait]
    impl MessageCoordinator for BlockingBroadcastMessageCoordinator {
        async fn send_to_player(
            &self,
            _player_id: &PlayerId,
            _message: Arc<ServerMessage>,
        ) -> Result<()> {
            Ok(())
        }

        async fn broadcast_to_room(
            &self,
            _room_id: &RoomId,
            _message: Arc<ServerMessage>,
        ) -> Result<()> {
            self.broadcast_started.notify_one();
            self.release_broadcast.notified().await;
            Ok(())
        }

        async fn broadcast_to_room_except(
            &self,
            _room_id: &RoomId,
            _except_player: &PlayerId,
            _message: Arc<ServerMessage>,
        ) -> Result<()> {
            Ok(())
        }

        async fn register_local_client(
            &self,
            _player_id: PlayerId,
            _room_id: Option<RoomId>,
            _sender: mpsc::Sender<Arc<ServerMessage>>,
        ) -> Result<()> {
            Ok(())
        }

        async fn unregister_local_client(&self, _player_id: &PlayerId) -> Result<()> {
            Ok(())
        }

        async fn should_process_message(&self, _message: &SequencedMessage) -> Result<bool> {
            Ok(true)
        }

        async fn mark_message_processed(&self, _message: &SequencedMessage) -> Result<()> {
            Ok(())
        }

        async fn handle_bus_message(&self, _message: SequencedMessage) -> Result<()> {
            Ok(())
        }

        async fn handle_membership_update(&self, _update: MembershipUpdate) -> Result<()> {
            Ok(())
        }
    }

    struct NoopDistributedLock;

    #[async_trait]
    impl DistributedLock for NoopDistributedLock {
        async fn acquire(&self, key: &str, ttl: Duration) -> Result<LockHandle> {
            Ok(LockHandle::new(key.to_string(), ttl))
        }

        async fn try_acquire(&self, key: &str, ttl: Duration) -> Result<Option<LockHandle>> {
            Ok(Some(LockHandle::new(key.to_string(), ttl)))
        }

        async fn extend(&self, _handle: &LockHandle, _ttl: Duration) -> Result<bool> {
            Ok(true)
        }

        async fn release(&self, _handle: &LockHandle) -> Result<bool> {
            Ok(true)
        }

        async fn is_locked(&self, _key: &str) -> Result<bool> {
            Ok(false)
        }

        async fn cleanup_expired_locks(&self) -> Result<usize> {
            Ok(0)
        }
    }

    #[derive(Debug, PartialEq)]
    struct MemberSnapshot {
        name: String,
        is_authority: bool,
        connection_info: serde_json::Value,
    }

    fn player_fixture(
        id: PlayerId,
        name: &str,
        is_authority: bool,
        connection_info: Option<ConnectionInfo>,
    ) -> PlayerInfo {
        PlayerInfo {
            id,
            name: name.to_string(),
            is_authority,
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info,
            region_id: "test-region".to_string(),
        }
    }

    fn member_snapshot(player: &PlayerInfo) -> MemberSnapshot {
        MemberSnapshot {
            name: player.name.clone(),
            is_authority: player.is_authority,
            connection_info: serde_json::to_value(&player.connection_info)
                .expect("legacy peer metadata serializes"),
        }
    }

    fn peer_snapshot(peer: &PeerConnectionInfo) -> MemberSnapshot {
        MemberSnapshot {
            name: peer.player_name.clone(),
            is_authority: peer.is_authority,
            connection_info: serde_json::to_value(&peer.connection_info)
                .expect("legacy peer metadata serializes"),
        }
    }

    fn finalized_member_map(members: &[PlayerInfo]) -> BTreeMap<PlayerId, MemberSnapshot> {
        members
            .iter()
            .map(|player| (player.id, member_snapshot(player)))
            .collect()
    }

    fn peer_connection_map(peers: &[PeerConnectionInfo]) -> BTreeMap<PlayerId, MemberSnapshot> {
        peers
            .iter()
            .map(|peer| (peer.player_id, peer_snapshot(peer)))
            .collect()
    }

    async fn wait_until_unlocked(lock: &InMemoryDistributedLock, lock_key: &str) {
        timeout(Duration::from_secs(1), async {
            loop {
                if !lock
                    .is_locked(lock_key)
                    .await
                    .expect("lock state can be read")
                {
                    break;
                }

                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("lock should release before the operation lock TTL");
    }

    #[tokio::test]
    async fn room_operations_release_distributed_locks_immediately() {
        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(RecordingMessageCoordinator::default());
        let lock = Arc::new(InMemoryDistributedLock::new());
        let room_coordinator =
            InMemoryRoomOperationCoordinator::new(coordinator, lock.clone(), database.clone());
        let player_id = PlayerId::from_u128(0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb);

        // The lobby transition now persists and is idempotent, so it needs a real
        // Waiting room to transition (the other operations below simulate success
        // independent of room existence).
        let room = database
            .create_room(
                "lock-release-game".to_string(),
                Some("LOCK01".to_string()),
                4,
                true,
                player_id,
                "matchbox".to_string(),
                "test-region".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");
        let room_id = room.id;

        room_coordinator
            .transition_room_to_lobby(&room_id)
            .await
            .expect("lobby transition succeeds");
        assert!(
            !lock
                .is_locked(&format!("room_lobby_transition:{room_id}"))
                .await
                .expect("lock state can be read"),
            "lobby transition lock should be released before TTL expiry"
        );

        room_coordinator
            .coordinate_authority_transfer(&room_id, &player_id)
            .await
            .expect("authority transfer succeeds");
        assert!(
            !lock
                .is_locked(&format!("authority_transfer:{room_id}:{player_id}"))
                .await
                .expect("lock state can be read"),
            "authority transfer lock should be released before TTL expiry"
        );

        room_coordinator
            .execute_distributed_operation("test_operation", &room_id)
            .await
            .expect("distributed operation succeeds");
        assert!(
            !lock
                .is_locked(&format!("distributed_op:{room_id}:test_operation"))
                .await
                .expect("lock state can be read"),
            "distributed operation lock should be released before TTL expiry"
        );

        room_coordinator
            .handle_authority_request(&room_id, &player_id, true)
            .await
            .expect("authority request handles missing room as denial");
        assert!(
            !lock
                .is_locked(&format!("room_authority:{room_id}"))
                .await
                .expect("lock state can be read"),
            "authority request lock should be released before TTL expiry"
        );
    }

    #[tokio::test]
    async fn room_operations_release_locks_after_broadcast_failures() {
        let room_id = RoomId::from_u128(0x1234567890abcdef1234567890abcdef);
        let player_id = PlayerId::from_u128(0xabcdef1234567890abcdef1234567890);

        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(RecordingMessageCoordinator::default());
        coordinator.fail_broadcast_on(1).await;
        let lock = Arc::new(InMemoryDistributedLock::new());
        let room_coordinator =
            InMemoryRoomOperationCoordinator::new(coordinator, lock.clone(), database);

        let result = room_coordinator.transition_room_to_lobby(&room_id).await;
        assert!(
            result.is_err(),
            "lobby transition should propagate broadcast failure"
        );
        assert!(
            !lock
                .is_locked(&format!("room_lobby_transition:{room_id}"))
                .await
                .expect("lock state can be read"),
            "lobby transition lock should release after broadcast failure"
        );

        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(RecordingMessageCoordinator::default());
        coordinator.fail_broadcast_on(1).await;
        let lock = Arc::new(InMemoryDistributedLock::new());
        let room_coordinator =
            InMemoryRoomOperationCoordinator::new(coordinator, lock.clone(), database);

        let result = room_coordinator
            .coordinate_authority_transfer(&room_id, &player_id)
            .await;
        assert!(
            result.is_err(),
            "authority transfer should propagate broadcast failure"
        );
        assert!(
            !lock
                .is_locked(&format!("authority_transfer:{room_id}:{player_id}"))
                .await
                .expect("lock state can be read"),
            "authority transfer lock should release after broadcast failure"
        );

        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(RecordingMessageCoordinator::default());
        coordinator.fail_broadcast_on(1).await;
        let lock = Arc::new(InMemoryDistributedLock::new());
        let ready_coordinator =
            InMemoryRoomOperationCoordinator::new(coordinator, lock.clone(), database.clone());
        let authority = PlayerId::from_u128(0xaaaaaaaa11111111aaaaaaaa11111111);
        let peer = PlayerId::from_u128(0xbbbbbbbb22222222bbbbbbbb22222222);

        let room = database
            .create_room(
                "ready-broadcast-failure-game".to_string(),
                Some("FAIL01".to_string()),
                2,
                true,
                authority,
                "test-relay".to_string(),
                "test-region".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");

        assert!(database
            .add_player_to_room(&room.id, player_fixture(peer, "Peer", false, None),)
            .await
            .expect("adding peer succeeds"));
        database
            .transition_room_to_lobby(&room.id)
            .await
            .expect("lobby transition succeeds");

        let result = ready_coordinator
            .handle_player_ready(&room.id, &authority, None)
            .await;
        assert!(
            result.is_err(),
            "ready toggle should propagate lobby broadcast failure"
        );
        assert!(
            !lock
                .is_locked(&format!("room_ready_state:{}", room.id))
                .await
                .expect("lock state can be read"),
            "ready-state lock should release after broadcast failure"
        );

        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(RecordingMessageCoordinator::default());
        coordinator.fail_broadcast_on(3).await;
        let lock = Arc::new(InMemoryDistributedLock::new());
        let ready_coordinator =
            InMemoryRoomOperationCoordinator::new(coordinator, lock.clone(), database.clone());
        let authority = PlayerId::from_u128(0xcccccccc33333333cccccccc33333333);
        let peer = PlayerId::from_u128(0xdddddddd44444444dddddddd44444444);

        let room = database
            .create_room(
                "ready-final-broadcast-failure-game".to_string(),
                Some("FAIL02".to_string()),
                2,
                true,
                authority,
                "test-relay".to_string(),
                "test-region".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");

        assert!(database
            .add_player_to_room(&room.id, player_fixture(peer, "Peer", false, None),)
            .await
            .expect("adding peer succeeds"));
        database
            .transition_room_to_lobby(&room.id)
            .await
            .expect("lobby transition succeeds");

        // Both ready toggles succeed (broadcasts 1 and 2); the third broadcast —
        // the GameStarting from the explicit StartGame — is the one set to fail.
        ready_coordinator
            .handle_player_ready(&room.id, &peer, None)
            .await
            .expect("first ready toggle succeeds");
        ready_coordinator
            .handle_player_ready(&room.id, &authority, None)
            .await
            .expect("second ready toggle succeeds");

        let result = ready_coordinator
            .handle_start_game(&room.id, &authority)
            .await;
        assert!(
            result.is_err(),
            "StartGame should propagate the GameStarting broadcast failure"
        );
        assert!(
            !lock
                .is_locked(&format!("room_ready_state:{}", room.id))
                .await
                .expect("lock state can be read"),
            "ready-state lock should release after finalization broadcast failure"
        );
    }

    #[tokio::test]
    async fn handle_player_ready_releases_lock_on_error() {
        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(RecordingMessageCoordinator::default());
        let lock = Arc::new(InMemoryDistributedLock::new());
        let ready_coordinator =
            InMemoryRoomOperationCoordinator::new(coordinator, lock.clone(), database);
        let room_id = RoomId::from_u128(0xcccccccccccccccccccccccccccccccc);
        let player_id = PlayerId::from_u128(0xdddddddddddddddddddddddddddddddd);

        let result = ready_coordinator
            .handle_player_ready(&room_id, &player_id, None)
            .await;

        assert!(result.is_err(), "missing room should reject ready toggles");
        assert!(
            !lock
                .is_locked(&format!("room_ready_state:{room_id}"))
                .await
                .expect("lock state can be read"),
            "ready-state lock should be released even when the operation fails"
        );
    }

    #[tokio::test]
    async fn aborted_player_ready_releases_ready_state_lock_without_waiting_for_ttl() {
        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(BlockingBroadcastMessageCoordinator::new());
        let lock = Arc::new(InMemoryDistributedLock::new());
        let ready_coordinator = Arc::new(InMemoryRoomOperationCoordinator::new(
            coordinator.clone(),
            lock.clone(),
            database.clone(),
        ));
        let authority = PlayerId::from_u128(0x99999999999999999999999999999999);
        let peer = PlayerId::from_u128(0x88888888888888888888888888888888);

        let room = database
            .create_room(
                "ready-cancellation-game".to_string(),
                Some("CANCEL".to_string()),
                2,
                true,
                authority,
                "test-relay".to_string(),
                "test-region".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");

        assert!(database
            .add_player_to_room(&room.id, player_fixture(peer, "Peer", false, None),)
            .await
            .expect("adding peer succeeds"));
        database
            .transition_room_to_lobby(&room.id)
            .await
            .expect("lobby transition succeeds");

        let room_id = room.id;
        let lock_key = format!("room_ready_state:{room_id}");
        let ready_task = {
            let ready_coordinator = Arc::clone(&ready_coordinator);
            tokio::spawn(async move {
                ready_coordinator
                    .handle_player_ready(&room_id, &authority, None)
                    .await
            })
        };

        timeout(
            Duration::from_secs(1),
            coordinator.wait_for_broadcast_start(),
        )
        .await
        .expect("ready operation should reach the post-acquire broadcast");
        assert!(
            lock.is_locked(&lock_key)
                .await
                .expect("lock state can be read"),
            "ready-state lock should be held while the operation is suspended after acquisition"
        );

        ready_task.abort();
        let abort_error = ready_task
            .await
            .expect_err("aborted ready task should not complete normally");
        assert!(abort_error.is_cancelled(), "ready task should be aborted");

        wait_until_unlocked(lock.as_ref(), &lock_key).await;
        coordinator.release_broadcast();
    }

    #[tokio::test]
    async fn handle_player_ready_does_not_wait_for_previous_ready_lock_ttl() {
        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(RecordingMessageCoordinator::default());
        let lock = Arc::new(InMemoryDistributedLock::new());
        let ready_coordinator =
            InMemoryRoomOperationCoordinator::new(coordinator, lock.clone(), database.clone());
        let authority = PlayerId::from_u128(0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee);
        let peer = PlayerId::from_u128(0xffffffffffffffffffffffffffffffff);

        let room = database
            .create_room(
                "ready-lock-release-game".to_string(),
                Some("LOCK01".to_string()),
                2,
                true,
                authority,
                "test-relay".to_string(),
                "test-region".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");

        assert!(database
            .add_player_to_room(&room.id, player_fixture(peer, "Peer", false, None),)
            .await
            .expect("adding peer succeeds"));
        database
            .transition_room_to_lobby(&room.id)
            .await
            .expect("lobby transition succeeds");

        ready_coordinator
            .handle_player_ready(&room.id, &authority, None)
            .await
            .expect("first ready toggle succeeds");
        assert!(
            !lock
                .is_locked(&format!("room_ready_state:{}", room.id))
                .await
                .expect("lock state can be read"),
            "first ready toggle should release the ready-state lock immediately"
        );

        timeout(
            Duration::from_secs(1),
            ready_coordinator.handle_player_ready(&room.id, &peer, None),
        )
        .await
        .expect("second ready toggle should not wait for ready-state lock TTL")
        .expect("second ready toggle succeeds");

        assert!(
            !lock
                .is_locked(&format!("room_ready_state:{}", room.id))
                .await
                .expect("lock state can be read"),
            "final ready toggle should release the ready-state lock immediately"
        );
    }

    #[tokio::test]
    async fn handle_player_ready_finalizes_with_member_snapshot_matching_game_starting_peers() {
        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(RecordingMessageCoordinator::default());
        let ready_coordinator = InMemoryRoomOperationCoordinator::new(
            coordinator.clone(),
            Arc::new(NoopDistributedLock),
            database.clone(),
        );
        let authority = PlayerId::from_u128(0x11111111111111111111111111111111);
        let peer_a = PlayerId::from_u128(0x22222222222222222222222222222222);
        let peer_b = PlayerId::from_u128(0x33333333333333333333333333333333);
        const PLAYER_COUNT: u8 = 3;
        let players = [authority, peer_a, peer_b];

        let room = database
            .create_room(
                "finalize-game".to_string(),
                Some("FINAL1".to_string()),
                PLAYER_COUNT,
                true,
                authority,
                "custom-relay".to_string(),
                "test-region".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");

        assert!(database
            .update_player_name(&room.id, &authority, "Authority")
            .await
            .expect("authority name update succeeds"));
        assert!(database
            .update_player_connection_info(
                &room.id,
                &authority,
                ConnectionInfo::Direct {
                    host: "10.0.0.1".to_string(),
                    port: 7777,
                },
            )
            .await
            .expect("authority connection update succeeds"));

        for player in [
            player_fixture(
                peer_a,
                "Peer A",
                false,
                Some(ConnectionInfo::WebRTC {
                    sdp: Some("offer-sdp".to_string()),
                    ice_candidates: vec!["candidate-a".to_string()],
                }),
            ),
            player_fixture(peer_b, "Peer B", false, None),
        ] {
            assert!(database
                .add_player_to_room(&room.id, player)
                .await
                .expect("adding player succeeds"));
        }

        database
            .transition_room_to_lobby(&room.id)
            .await
            .expect("lobby transition succeeds");

        // Ready toggles never finalize — they only broadcast lobby state. The
        // third toggle makes every current player ready (`all_ready: true`), but
        // the game does not start until an explicit `StartGame`.
        for player_id in [authority, peer_a, peer_b] {
            ready_coordinator
                .handle_player_ready(&room.id, &player_id, None)
                .await
                .expect("ready toggle succeeds");
        }

        // The authority starts the game; this finalizes and broadcasts GameStarting.
        let finalized = match ready_coordinator
            .handle_start_game(&room.id, &authority)
            .await
            .expect("start game succeeds")
        {
            StartGameOutcome::Started(finalized) => finalized,
            other => panic!("StartGame by the authority must finalize, got {other:?}"),
        };

        let broadcasts = coordinator.broadcasts().await;
        assert_eq!(
            broadcasts.len(),
            4,
            "three ready toggles should broadcast lobby state, and finalization should broadcast GameStarting"
        );
        assert!(
            broadcasts.iter().all(|event| event.room_id == room.id),
            "every broadcast should target the finalized room"
        );

        for event in &broadcasts[..2] {
            assert!(
                matches!(
                    &event.message,
                    ServerMessage::LobbyStateChanged {
                        lobby_state: LobbyState::Lobby,
                        all_ready: false,
                        ..
                    }
                ),
                "non-final ready toggles must broadcast non-final lobby state: {:?}",
                event.message
            );
        }

        match &broadcasts[2].message {
            ServerMessage::LobbyStateChanged {
                lobby_state,
                ready_players,
                all_ready,
            } => {
                assert_eq!(*lobby_state, LobbyState::Lobby);
                assert_eq!(ready_players.len(), players.len());
                assert!(*all_ready);
            }
            other => panic!("expected final LobbyStateChanged before GameStarting, got {other:?}"),
        }

        let peer_connections = match &broadcasts[3].message {
            ServerMessage::GameStarting { peer_connections } => peer_connections,
            other => panic!("expected GameStarting after final LobbyStateChanged, got {other:?}"),
        };

        assert_eq!(finalized.game_name, "finalize-game");
        assert_eq!(finalized.authority_player, Some(authority));
        assert_eq!(finalized.members.len(), players.len());
        assert_eq!(
            finalized_member_map(&finalized.members),
            peer_connection_map(peer_connections),
            "FinalizedRoom members must match the same room-player snapshot used for GameStarting metadata"
        );
        assert!(
            peer_connections
                .iter()
                .all(|peer| peer.relay_type == "custom-relay"),
            "GameStarting metadata must carry the finalized room relay type"
        );
    }

    #[tokio::test]
    async fn start_game_enforces_readiness_and_authorization() {
        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(RecordingMessageCoordinator::default());
        let coord = InMemoryRoomOperationCoordinator::new(
            coordinator.clone(),
            Arc::new(NoopDistributedLock),
            database.clone(),
        );
        let authority = PlayerId::from_u128(0xa1);
        let peer = PlayerId::from_u128(0xb2);

        let room = database
            .create_room(
                "start-rules".to_string(),
                Some("START1".to_string()),
                4, // ceiling of 4, but the room starts with only 2 present
                true,
                authority,
                "matchbox".to_string(),
                "test-region".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");
        assert!(database
            .add_player_to_room(&room.id, player_fixture(peer, "Peer", false, None))
            .await
            .expect("adding peer succeeds"));

        // Not everyone is ready yet -> NotReady.
        assert!(matches!(
            coord.handle_start_game(&room.id, &authority).await.unwrap(),
            StartGameOutcome::NotReady
        ));

        coord
            .handle_player_ready(&room.id, &authority, None)
            .await
            .unwrap();
        coord
            .handle_player_ready(&room.id, &peer, None)
            .await
            .unwrap();

        // All ready, but a non-authority may not start an authority room.
        assert!(matches!(
            coord.handle_start_game(&room.id, &peer).await.unwrap(),
            StartGameOutcome::Forbidden
        ));

        // The authority starts the partially-full room (2 of a 4-ceiling).
        let started = coord.handle_start_game(&room.id, &authority).await.unwrap();
        let finalized = match started {
            StartGameOutcome::Started(f) => f,
            other => panic!("authority StartGame must finalize, got {other:?}"),
        };
        assert_eq!(
            finalized.members.len(),
            2,
            "started with the 2 present members"
        );
        assert_eq!(finalized.authority_player, Some(authority));

        // Starting again is rejected: the room is already finalized.
        assert!(matches!(
            coord.handle_start_game(&room.id, &authority).await.unwrap(),
            StartGameOutcome::AlreadyStarted
        ));
    }

    #[tokio::test]
    async fn start_game_allows_solo_and_any_member_without_authority() {
        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(RecordingMessageCoordinator::default());
        let coord = InMemoryRoomOperationCoordinator::new(
            coordinator.clone(),
            Arc::new(NoopDistributedLock),
            database.clone(),
        );
        let solo = PlayerId::from_u128(0xc3);

        // No authority (supports_authority=false): any member may start, and a
        // single ready player is enough (solo is allowed).
        let room = database
            .create_room(
                "solo-start".to_string(),
                Some("SOLO01".to_string()),
                4,
                false,
                solo,
                "matchbox".to_string(),
                "test-region".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");

        // Cannot start before readying.
        assert!(matches!(
            coord.handle_start_game(&room.id, &solo).await.unwrap(),
            StartGameOutcome::NotReady
        ));

        coord
            .handle_player_ready(&room.id, &solo, None)
            .await
            .unwrap();

        let started = coord.handle_start_game(&room.id, &solo).await.unwrap();
        assert!(
            matches!(started, StartGameOutcome::Started(ref f) if f.members.len() == 1),
            "a lone ready player may start a no-authority room, got {started:?}"
        );
    }

    #[tokio::test]
    async fn start_game_excludes_a_member_that_left_before_finalize() {
        // Adversarial guard (StartGame vs LeaveRoom): peer_connections and
        // FinalizedRoom.members are built from the POST-finalize membership, so a
        // player that departs before StartGame is never named as a phantom peer.
        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(RecordingMessageCoordinator::default());
        let coord = InMemoryRoomOperationCoordinator::new(
            coordinator.clone(),
            Arc::new(NoopDistributedLock),
            database.clone(),
        );
        let authority = PlayerId::from_u128(0xa1);
        let leaver = PlayerId::from_u128(0xb2);
        let room = database
            .create_room(
                "leave-before-start".to_string(),
                Some("LEAVE1".to_string()),
                4,
                true,
                authority,
                "matchbox".to_string(),
                "test-region".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");
        assert!(database
            .add_player_to_room(&room.id, player_fixture(leaver, "Leaver", false, None))
            .await
            .expect("adding leaver succeeds"));

        coord
            .handle_player_ready(&room.id, &authority, None)
            .await
            .unwrap();
        coord
            .handle_player_ready(&room.id, &leaver, None)
            .await
            .unwrap();

        // The leaver departs before the explicit StartGame.
        database
            .remove_player_from_room(&room.id, &leaver)
            .await
            .expect("removing leaver succeeds");

        let finalized = match coord.handle_start_game(&room.id, &authority).await.unwrap() {
            StartGameOutcome::Started(f) => f,
            other => panic!("authority StartGame must finalize, got {other:?}"),
        };
        assert_eq!(
            finalized.members.len(),
            1,
            "only the remaining member is in the finalized session"
        );
        assert!(
            !finalized.members.iter().any(|m| m.id == leaver),
            "the departed player must not be a finalized session member"
        );

        let peers = coordinator
            .broadcasts()
            .await
            .into_iter()
            .find_map(|e| match e.message {
                ServerMessage::GameStarting { peer_connections } => Some(peer_connections),
                _ => None,
            })
            .expect("a GameStarting was broadcast");
        assert!(
            !peers.iter().any(|p| p.player_id == leaver),
            "GameStarting peer_connections must not name the departed player"
        );
    }

    #[tokio::test]
    async fn concurrent_start_game_finalizes_exactly_once() {
        // Two concurrent StartGame calls: the shared `room_ready_state` lock plus
        // the persist-before-broadcast ordering mean exactly one finalizes (one
        // GameStarting) and the other sees AlreadyStarted (or loses the lock).
        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(RecordingMessageCoordinator::default());
        let coord = Arc::new(InMemoryRoomOperationCoordinator::new(
            coordinator.clone(),
            Arc::new(InMemoryDistributedLock::new()),
            database.clone(),
        ));
        let a = PlayerId::from_u128(0xaa);
        let b = PlayerId::from_u128(0xbb);
        // No authority: either member may start.
        let room = database
            .create_room(
                "concurrent-start".to_string(),
                Some("CONC01".to_string()),
                4,
                false,
                a,
                "matchbox".to_string(),
                "test-region".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");
        assert!(database
            .add_player_to_room(&room.id, player_fixture(b, "B", false, None))
            .await
            .expect("adding b succeeds"));
        coord.handle_player_ready(&room.id, &a, None).await.unwrap();
        coord.handle_player_ready(&room.id, &b, None).await.unwrap();

        let (c1, c2) = (Arc::clone(&coord), Arc::clone(&coord));
        let rid = room.id;
        let (r1, r2) = tokio::join!(
            async move { c1.handle_start_game(&rid, &a).await },
            async move { c2.handle_start_game(&rid, &b).await },
        );
        let started = [&r1, &r2]
            .iter()
            .filter(|r| matches!(r, Ok(StartGameOutcome::Started(_))))
            .count();
        assert_eq!(
            started, 1,
            "exactly one concurrent StartGame finalizes; got {r1:?} and {r2:?}"
        );
        let game_starts = coordinator
            .broadcasts()
            .await
            .into_iter()
            .filter(|e| matches!(e.message, ServerMessage::GameStarting { .. }))
            .count();
        assert_eq!(
            game_starts, 1,
            "GameStarting must be broadcast exactly once"
        );
    }

    #[tokio::test]
    async fn start_game_after_authority_departs_allows_remaining_member() {
        // Liveness guard: after the authority leaves (authority_player is cleared
        // by remove_player_from_room), a remaining ready member CAN start — the
        // room is not locked into Forbidden forever.
        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(RecordingMessageCoordinator::default());
        let coord = InMemoryRoomOperationCoordinator::new(
            coordinator.clone(),
            Arc::new(NoopDistributedLock),
            database.clone(),
        );
        let authority = PlayerId::from_u128(0xc1);
        let member = PlayerId::from_u128(0xc2);
        let room = database
            .create_room(
                "authority-departs".to_string(),
                Some("AUTH01".to_string()),
                4,
                true,
                authority,
                "matchbox".to_string(),
                "test-region".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");
        assert!(database
            .add_player_to_room(&room.id, player_fixture(member, "Member", false, None))
            .await
            .expect("adding member succeeds"));
        coord
            .handle_player_ready(&room.id, &authority, None)
            .await
            .unwrap();
        coord
            .handle_player_ready(&room.id, &member, None)
            .await
            .unwrap();

        // Before the authority leaves, a non-authority member is Forbidden.
        assert!(matches!(
            coord.handle_start_game(&room.id, &member).await.unwrap(),
            StartGameOutcome::Forbidden
        ));

        // The authority departs (clears authority_player).
        database
            .remove_player_from_room(&room.id, &authority)
            .await
            .expect("removing authority succeeds");

        // Now any remaining member may start.
        let outcome = coord.handle_start_game(&room.id, &member).await.unwrap();
        assert!(
            matches!(outcome, StartGameOutcome::Started(_)),
            "a remaining member may start after the authority leaves, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn start_game_after_unready_is_not_ready() {
        // An un-ready toggle before StartGame must block finalization (NotReady),
        // never finalize a not-all-ready room.
        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(RecordingMessageCoordinator::default());
        let coord = InMemoryRoomOperationCoordinator::new(
            coordinator.clone(),
            Arc::new(NoopDistributedLock),
            database.clone(),
        );
        let a = PlayerId::from_u128(0xd1);
        let b = PlayerId::from_u128(0xd2);
        let room = database
            .create_room(
                "unready-start".to_string(),
                Some("UNRDY1".to_string()),
                4,
                false,
                a,
                "matchbox".to_string(),
                "test-region".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");
        assert!(database
            .add_player_to_room(&room.id, player_fixture(b, "B", false, None))
            .await
            .expect("adding b succeeds"));
        coord.handle_player_ready(&room.id, &a, None).await.unwrap();
        coord.handle_player_ready(&room.id, &b, None).await.unwrap();
        // b un-readies.
        coord.handle_player_ready(&room.id, &b, None).await.unwrap();

        let outcome = coord.handle_start_game(&room.id, &a).await.unwrap();
        assert!(
            matches!(outcome, StartGameOutcome::NotReady),
            "StartGame with a not-ready member must be NotReady, got {outcome:?}"
        );
    }
}
