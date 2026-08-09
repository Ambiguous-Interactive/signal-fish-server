//! Room operation coordination for process-local state management.
//!
//! This module provides coordinators for managing room operations (lobby transitions,
//! authority transfers, player ready states) with in-memory locking to ensure
//! consistency inside one server process.

use anyhow::Result;
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::database::{AuthorityDenial, AuthorityOutcome};
use crate::distributed::{DistributedLock, LockHandle};
use crate::protocol::{PeerConnectionInfo, PlayerId, PlayerInfo, RoomId};

use super::{
    MessageCoordinator, RoomEventCompletion, RoomEventMutationGuard, RoomMessageTransactionOutcome,
    RoomRecipientMessages,
};

const LOBBY_TRANSITION_LOCK_TTL: Duration = Duration::from_secs(10);
const ROOM_OPERATION_LOCK_TTL: Duration = Duration::from_secs(5);

/// Snapshot of a room at finalization, used to build the capability-aware
/// `GameStarting` and v3 `SessionPlan` publication from one member snapshot.
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

/// The complete outbound publication associated with one successful game start.
///
/// `after_game_starting` records the sticky session decision and its metrics
/// after every `GameStarting` frame is queued and before any tailored plan. It
/// runs under the same final routing snapshot as the reserved message batches.
pub struct StartGamePublication {
    pub recipient_messages: Vec<RoomRecipientMessages>,
    pub after_game_starting: Box<dyn FnOnce() + Send + 'static>,
}

/// Builds the capability-aware publication once the coordinator has captured
/// the exact finalized member snapshot and uniform `GameStarting` frame.
pub type StartGamePublicationBuilder = Box<
    dyn FnOnce(&FinalizedRoom, Arc<crate::protocol::ServerMessage>) -> StartGamePublication
        + Send
        + 'static,
>;

impl StartGamePublication {
    fn game_starting_only(
        finalized: &FinalizedRoom,
        game_starting: Arc<crate::protocol::ServerMessage>,
    ) -> Self {
        Self {
            recipient_messages: finalized
                .members
                .iter()
                .map(|member| RoomRecipientMessages {
                    player_id: member.id,
                    first_phase: 0,
                    messages: vec![Arc::clone(&game_starting)],
                })
                .collect(),
            after_game_starting: Box::new(|| {}),
        }
    }
}

/// The outcome of an explicit `StartGame`
/// ([`RoomOperationCoordinatorTrait::handle_start_game`]).
#[derive(Debug, Clone)]
pub enum StartGameOutcome {
    /// The room finalized and its complete start publication was committed.
    Started(FinalizedRoom),
    /// Not every current player is ready (maps to `GAME_START_NOT_READY`).
    NotReady,
    /// The sender is not permitted to start (the room has a designated
    /// authority and the sender is not it; maps to `GAME_START_FORBIDDEN`).
    Forbidden,
    /// The room is already `Finalized` (maps to `INVALID_ROOM_STATE`).
    AlreadyStarted,
}

/// Why a `PlayerReady` toggle failed
/// ([`RoomOperationCoordinatorTrait::handle_player_ready`]).
///
/// Typed so the caller maps each case to the correct client `ErrorCode` by an
/// exhaustive `match` — never by inspecting an error string. Only [`Self::Finalized`]
/// is a business rejection the client should surface as a room-state error; the
/// other variants are infrastructure failures that must NOT masquerade as one
/// (see `src/server/ready_state.rs`).
#[derive(Debug)]
pub enum PlayerReadyError {
    /// The room is `Finalized` — the game already started, so further ready
    /// toggles are rejected (maps to `INVALID_ROOM_STATE`).
    Finalized,
    /// The room no longer exists (maps to `ROOM_NOT_FOUND`).
    RoomNotFound,
    /// An unexpected infrastructure failure — lock acquisition, storage, or the
    /// lobby broadcast (maps to `INTERNAL_ERROR`).
    Internal(anyhow::Error),
}

impl PlayerReadyError {
    /// The client-facing [`crate::protocol::ErrorCode`] for this failure.
    ///
    /// Only [`Self::Finalized`] is a business rejection (`INVALID_ROOM_STATE`);
    /// the rest are infrastructure faults that must surface as `ROOM_NOT_FOUND`
    /// or `INTERNAL_ERROR`, never as a room-state error.
    pub fn error_code(&self) -> crate::protocol::ErrorCode {
        match self {
            Self::Finalized => crate::protocol::ErrorCode::InvalidRoomState,
            Self::RoomNotFound => crate::protocol::ErrorCode::RoomNotFound,
            Self::Internal(_) => crate::protocol::ErrorCode::InternalError,
        }
    }
}

impl std::fmt::Display for PlayerReadyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Finalized => {
                write!(f, "the game has already started (room is Finalized)")
            }
            Self::RoomNotFound => write!(f, "room not found"),
            Self::Internal(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for PlayerReadyError {}

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

    /// Handle authority request from a player.
    ///
    /// The returned outcome is the same decision the requester's
    /// `AuthorityResponse` carries, including the typed refusal cause.
    async fn handle_authority_request(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        become_authority: bool,
    ) -> Result<AuthorityOutcome>;

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
    ) -> std::result::Result<(), PlayerReadyError>;

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

    /// Start a game with one pre-reserved, exact-membership publication that
    /// includes `GameStarting`, sticky session state, and tailored plans.
    async fn handle_start_game_with_publication(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        build_publication: StartGamePublicationBuilder,
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

    /// Forget one player's readiness in a room.
    ///
    /// Readiness belongs to a membership, not to a player id: reads filter the
    /// set by current membership, which silently reinstates a departed member's
    /// readiness if that same id joins the room again. The JOIN path calls this
    /// so a new membership always starts unready. Reconnection deliberately does
    /// not: it resumes the same membership rather than creating a new one.
    async fn forget_player_ready(&self, room_id: &RoomId, player_id: &PlayerId);

    /// Snapshot the room ids that currently hold a ready-state entry.
    ///
    /// The maintenance prune sweep uses this to reclaim entries for rooms that
    /// no longer exist — the all-paths backstop to [`Self::clear_ready_players`]
    /// (mirrors the server's `prune_active_session_plans`).
    async fn ready_player_room_ids(&self) -> Vec<RoomId>;
}

/// In-memory room operation coordinator
#[derive(Clone)]
pub struct InMemoryRoomOperationCoordinator {
    coordinator: Arc<dyn MessageCoordinator>,
    distributed_lock: Arc<dyn DistributedLock>,
    database: Arc<dyn crate::database::GameDatabase>,
    /// Track ready players per room for in-memory coordinator
    ready_players: Arc<RwLock<HashMap<RoomId, HashSet<PlayerId>>>>,
    /// Records this coordinator's room-uniform broadcasts (`LobbyStateChanged`
    /// / uniform `AuthorityChanged`) for reconnection replay; `None` when
    /// reconnection is disabled. The per-player customized `AuthorityChanged`
    /// (`you_are_authority` differs by recipient) and the per-recipient
    /// `GameStarting` are deliberately NOT recorded.
    reconnection_manager: Option<Arc<crate::reconnection::ReconnectionManager>>,
}

impl InMemoryRoomOperationCoordinator {
    /// Create a new in-memory room operation coordinator
    pub fn new(
        coordinator: Arc<dyn MessageCoordinator>,
        distributed_lock: Arc<dyn DistributedLock>,
        database: Arc<dyn crate::database::GameDatabase>,
        reconnection_manager: Option<Arc<crate::reconnection::ReconnectionManager>>,
    ) -> Self {
        Self {
            coordinator,
            distributed_lock,
            database,
            ready_players: Arc::new(RwLock::new(HashMap::new())),
            reconnection_manager,
        }
    }

    /// Commit a room-uniform replay record and its live broadcast under one
    /// routing snapshot. Reconnect registration takes the write side of that
    /// snapshot, so it observes the event through replay or live delivery,
    /// never both.
    fn enqueue_replayable_room_event(
        &self,
        room_id: &RoomId,
        message: crate::protocol::ServerMessage,
        mutation_guard: RoomEventMutationGuard,
    ) -> RoomEventCompletion {
        let message = Arc::new(message);
        let replay_message = Arc::clone(&message);
        let reconnection_manager = self.reconnection_manager.clone();
        let replay_room_id = *room_id;
        let coordinator = Arc::clone(&self.coordinator);
        self.coordinator.enqueue_room_event(
            mutation_guard,
            Box::new(move || {
                Box::pin(async move {
                    coordinator
                        .broadcast_to_room_with_hook(
                            &replay_room_id,
                            message,
                            Box::new(move || {
                                Box::pin(async move {
                                    if let Some(reconnection_manager) = reconnection_manager {
                                        reconnection_manager
                                            .record_room_event(
                                                &replay_room_id,
                                                replay_message.as_ref(),
                                            )
                                            .await;
                                    }
                                })
                            }),
                        )
                        .await
                })
            }),
        )
    }

    fn enqueue_authority_result(
        &self,
        room_id: &RoomId,
        player_id: PlayerId,
        response: crate::protocol::ServerMessage,
        authority_player: Option<Option<PlayerId>>,
        mutation_guard: RoomEventMutationGuard,
    ) -> RoomEventCompletion {
        let coordinator = Arc::clone(&self.coordinator);
        let reconnection_manager = self.reconnection_manager.clone();
        let event_room_id = *room_id;
        self.coordinator.enqueue_room_event(
            mutation_guard,
            Box::new(move || {
                Box::pin(async move {
                    coordinator
                        .send_to_player(&player_id, Arc::new(response))
                        .await?;

                    let Some(authority_player) = authority_player else {
                        return Ok(true);
                    };
                    let message = Arc::new(crate::protocol::ServerMessage::AuthorityChanged {
                        authority_player,
                        you_are_authority: false,
                    });
                    let replay_message = Arc::clone(&message);
                    coordinator
                        .broadcast_to_room_with_hook(
                            &event_room_id,
                            message,
                            Box::new(move || {
                                Box::pin(async move {
                                    if let Some(reconnection_manager) = reconnection_manager {
                                        reconnection_manager
                                            .record_room_event(
                                                &event_room_id,
                                                replay_message.as_ref(),
                                            )
                                            .await;
                                    }
                                })
                            }),
                        )
                        .await
                })
            }),
        )
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
        let mutation_guard = self.coordinator.lock_room_event_mutation(room_id).await;
        let lock_key = format!("room_lobby_transition:{room_id}");
        let lock_guard = RoomOperationLockGuard::acquire(
            Arc::clone(&self.distributed_lock),
            lock_key,
            LOBBY_TRANSITION_LOCK_TTL,
            "transition_room_to_lobby",
        )
        .await?;

        // Enter the lobby exactly once. All fallible snapshot work happens
        // before persistence changes. The room-event guard keeps membership
        // and ready-state mutations out of this snapshot until the resulting
        // event has committed.
        match self.database.get_room_by_id(room_id).await {
            Ok(Some(room)) if room.lobby_state == crate::protocol::LobbyState::Waiting => {}
            Ok(Some(_)) => {
                drop(mutation_guard);
                lock_guard.release().await;
                return Ok(false);
            }
            Ok(None) => {
                drop(mutation_guard);
                lock_guard.release().await;
                return Err(anyhow::anyhow!("Room not found"));
            }
            Err(error) => {
                drop(mutation_guard);
                lock_guard.release().await;
                return Err(anyhow::anyhow!("Failed to get room: {error}"));
            }
        }

        let room_players = self.database.get_room_players(room_id).await?;
        let routed_players = self.coordinator.routed_player_ids(room_id).await?;
        let current_ids: HashSet<PlayerId> = match routed_players {
            Some(routed_players) => {
                let routed: HashSet<PlayerId> = routed_players.into_iter().collect();
                room_players
                    .iter()
                    .filter(|player| routed.contains(&player.id))
                    .map(|player| player.id)
                    .collect()
            }
            None => room_players.iter().map(|player| player.id).collect(),
        };
        let ready_set = self
            .ready_players
            .read()
            .await
            .get(room_id)
            .cloned()
            .unwrap_or_default();
        let mut ready_players: Vec<PlayerId> =
            ready_set.intersection(&current_ids).copied().collect();
        ready_players.sort_unstable();
        let all_ready = !current_ids.is_empty()
            && current_ids
                .iter()
                .all(|player_id| ready_set.contains(player_id));
        let message = crate::protocol::ServerMessage::LobbyStateChanged {
            lobby_state: crate::protocol::LobbyState::Lobby,
            ready_players,
            all_ready,
        };

        // From the first durable mutation onward this task owns the room guard
        // and distributed lock. Dropping the caller only detaches its join
        // handle; the transition still reaches the synchronous FIFO enqueue.
        let coordinator = self.clone();
        let room_id = *room_id;
        let transaction = tokio::spawn(async move {
            let transition = coordinator
                .database
                .transition_room_to_lobby(&room_id)
                .await;
            let completion = match transition {
                Ok(()) => {
                    coordinator.enqueue_replayable_room_event(&room_id, message, mutation_guard)
                }
                Err(error) => {
                    lock_guard.release().await;
                    return Err(error);
                }
            };

            // Delivery backpressure must not extend the TTL-bounded operation
            // lock; the FIFO job itself retains the room mutation guard.
            lock_guard.release().await;
            completion.await?;
            tracing::info!(%room_id, "Room transitioned to lobby state (in-memory)");
            Ok(true)
        });

        transaction
            .await
            .map_err(|error| anyhow::anyhow!("Owned lobby transition task failed: {error}"))?
    }

    async fn coordinate_authority_transfer(
        &self,
        room_id: &RoomId,
        new_authority: &PlayerId,
    ) -> Result<bool> {
        // For in-memory implementation, just simulate the operation
        let mutation_guard = self.coordinator.lock_room_event_mutation(room_id).await;
        let lock_key = format!("authority_transfer:{room_id}:{new_authority}");
        let lock_guard = RoomOperationLockGuard::acquire(
            Arc::clone(&self.distributed_lock),
            lock_key,
            ROOM_OPERATION_LOCK_TTL,
            "coordinate_authority_transfer",
        )
        .await?;

        let message = crate::protocol::ServerMessage::AuthorityChanged {
            authority_player: Some(*new_authority),
            you_are_authority: false, // Will be customized per client
        };
        let completion = self.enqueue_replayable_room_event(room_id, message, mutation_guard);

        // Nothing to mutate for the in-memory simulation; enqueue before
        // releasing the ordering gate, then await delivery outside the
        // TTL-bounded room-operation lock.
        lock_guard.release().await;
        completion.await?;
        tracing::info!(%room_id, %new_authority, "Authority transferred (in-memory)");
        Ok(true)
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
    ) -> Result<AuthorityOutcome> {
        let mutation_guard = self.coordinator.lock_room_event_mutation(room_id).await;
        let lock_key = format!("room_authority:{room_id}");
        let lock_guard = RoomOperationLockGuard::acquire(
            Arc::clone(&self.distributed_lock),
            lock_key,
            ROOM_OPERATION_LOCK_TTL,
            "handle_authority_request",
        )
        .await?;

        // From the first durable mutation onward this owned task retains both
        // operation guards. Dropping the caller only detaches its join handle;
        // the database result still reaches the synchronous FIFO enqueue.
        let coordinator = self.clone();
        let room_id = *room_id;
        let player_id = *player_id;
        let transaction = tokio::spawn(async move {
            tracing::info!(%room_id, %player_id, %become_authority, "InMemory: Processing authority request");

            let (result, response, authority_change) = match coordinator
                .database
                .request_room_authority(&room_id, &player_id, become_authority)
                .await
            {
                Ok(outcome) => {
                    // The single site that turns a storage decision into the
                    // client's `AuthorityResponse`. Both fields come from the
                    // same typed denial, so the code can never contradict the
                    // reason, and a losing claimant is told it lost a contest
                    // (`AUTHORITY_CONFLICT`) rather than that it lacks
                    // permission.
                    let denial = outcome.denial();
                    let response = crate::protocol::ServerMessage::AuthorityResponse {
                        granted: outcome.granted(),
                        reason: denial.map(|denial| denial.reason().to_string()),
                        error_code: denial.map(AuthorityDenial::error_code),
                    };
                    let authority_change = outcome.granted().then_some(if become_authority {
                        Some(player_id)
                    } else {
                        None
                    });
                    if outcome.granted() {
                        tracing::info!(%room_id, %player_id, %become_authority, "Authority request granted (in-memory)");
                    } else {
                        tracing::info!(%room_id, %player_id, %become_authority, ?denial, "Authority request denied (in-memory)");
                    }

                    (outcome, response, authority_change)
                }
                Err(e) => {
                    tracing::error!(%room_id, %player_id, %become_authority, "Authority request failed: {}", e);
                    let denial = AuthorityDenial::StorageError;
                    (
                        AuthorityOutcome::Denied(denial),
                        crate::protocol::ServerMessage::AuthorityResponse {
                            granted: false,
                            reason: Some(denial.reason().to_string()),
                            error_code: Some(denial.error_code()),
                        },
                        None,
                    )
                }
            };
            let completion = coordinator.enqueue_authority_result(
                &room_id,
                player_id,
                response,
                authority_change,
                mutation_guard,
            );

            // Delivery backpressure must not extend the TTL-bounded operation
            // lock. The FIFO job owns the room mutation guard and sends exactly
            // one response before any room-scoped AuthorityChanged.
            lock_guard.release().await;
            if let Err(error) = completion.await {
                tracing::error!(%room_id, %player_id, %error, "Failed to emit authority result");
            }
            result
        });

        transaction
            .await
            .map_err(|error| anyhow::anyhow!("Owned authority request task failed: {error}"))
    }

    async fn handle_player_ready(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        _app_id: Option<Uuid>,
    ) -> std::result::Result<(), PlayerReadyError> {
        // For in-memory implementation, simulate player ready toggle
        let mut mutation_guard = Some(self.coordinator.lock_room_event_mutation(room_id).await);
        let lock_key = format!("room_ready_state:{room_id}");
        let lock_guard = RoomOperationLockGuard::acquire(
            Arc::clone(&self.distributed_lock),
            lock_key,
            ROOM_OPERATION_LOCK_TTL,
            "handle_player_ready",
        )
        .await
        .map_err(PlayerReadyError::Internal)?;

        let mut completion = None;
        let result = async {
            // Get current room state to check if it has enough players for lobby actions
            let room = match self.database.get_room_by_id(room_id).await {
                Ok(Some(room)) => room,
                Ok(None) => {
                    return Err(PlayerReadyError::RoomNotFound);
                }
                Err(e) => {
                    return Err(PlayerReadyError::Internal(
                        e.context("failed to load room for ready toggle"),
                    ));
                }
            };

            // Readiness can be toggled any time the room is open (`max_players`
            // is a ceiling, not a required count); only a `Finalized` room — the
            // game already started — rejects further ready toggles.
            if room.lobby_state == crate::protocol::LobbyState::Finalized {
                return Err(PlayerReadyError::Finalized);
            }

            // Fetch the live membership first so readiness is computed over the
            // current players (a departed player must not count toward
            // `all_ready`, nor linger in the broadcast ready list).
            let room_players = match self.database.get_room_players(room_id).await {
                Ok(players) => players,
                Err(e) => {
                    return Err(PlayerReadyError::Internal(
                        e.context("failed to load room membership for ready toggle"),
                    ));
                }
            };
            let routed_players = self
                .coordinator
                .routed_player_ids(room_id)
                .await
                .map_err(PlayerReadyError::Internal)?;
            let room_players: Vec<PlayerInfo> = match routed_players {
                Some(routed_players) => {
                    let routed: HashSet<PlayerId> = routed_players.into_iter().collect();
                    room_players
                        .into_iter()
                        .filter(|player| routed.contains(&player.id))
                        .collect()
                }
                None => room_players,
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

            // Readiness no longer starts the game. `max_players` is a ceiling,
            // not a required count, and the game begins only on an explicit
            // `StartGame` (see `handle_start_game`) — so a `PlayerReady` toggle
            // just records readiness; the new lobby snapshot (with `all_ready`
            // so clients know `StartGame` is now permitted) is broadcast after
            // the lock releases below.
            tracing::info!(%room_id, %player_id, ready = !was_ready, "Player ready state toggled (in-memory)");
            let message = crate::protocol::ServerMessage::LobbyStateChanged {
                lobby_state: crate::protocol::LobbyState::Lobby,
                ready_players: ready_players_vec.clone(),
                all_ready,
            };
            let Some(ready_guard) = mutation_guard.take() else {
                return Err(PlayerReadyError::Internal(anyhow::anyhow!(
                    "ready mutation guard was unavailable before publication"
                )));
            };
            completion = Some(self.enqueue_replayable_room_event(room_id, message, ready_guard));
            Ok((ready_players_vec, all_ready))
        }
        .await;

        // Broadcast outside the TTL-bounded critical section: delivery can be
        // backpressured by a slow recipient and must never hold the room lock.
        drop(mutation_guard);
        lock_guard.release().await;

        match result {
            Ok((_ready_players, _all_ready)) => {
                let Some(completion) = completion else {
                    return Err(PlayerReadyError::Internal(anyhow::anyhow!(
                        "successful ready toggle did not enqueue its publication"
                    )));
                };
                completion.await.map_err(PlayerReadyError::Internal)?;
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    /// Handle an explicit `StartGame`: finalize the room with its *current*
    /// members when every current player is ready and the sender is permitted
    /// to start (the room's authority, or any member if no authority is set).
    ///
    /// Returns the [`StartGameOutcome`]: `Started(FinalizedRoom)` after the
    /// complete publication commits, or a rejection variant the caller maps to
    /// the matching `ErrorCode`.
    async fn handle_start_game(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
    ) -> Result<StartGameOutcome> {
        self.handle_start_game_with_publication(
            room_id,
            player_id,
            Box::new(|finalized, game_starting| {
                StartGamePublication::game_starting_only(finalized, game_starting)
            }),
        )
        .await
    }

    async fn handle_start_game_with_publication(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        build_publication: StartGamePublicationBuilder,
    ) -> Result<StartGameOutcome> {
        let mut mutation_guard = Some(self.coordinator.lock_room_event_mutation(room_id).await);
        let lock_key = format!("room_ready_state:{room_id}");
        let lock_guard = RoomOperationLockGuard::acquire(
            Arc::clone(&self.distributed_lock),
            lock_key,
            ROOM_OPERATION_LOCK_TTL,
            "handle_start_game",
        )
        .await?;

        // Captured under the lock for the post-release broadcast.
        let mut relay_type_for_broadcast: Option<String> = None;
        let mut lobby_state_for_finalize = None;
        let result = async {
            let room = match self.database.get_room_by_id(room_id).await {
                Ok(Some(room)) => room,
                Ok(None) => return Err(anyhow::anyhow!("Room not found")),
                Err(e) => return Err(anyhow::anyhow!("Failed to get room: {e}")),
            };
            relay_type_for_broadcast = Some(room.relay_type.clone());
            lobby_state_for_finalize = Some(room.lobby_state.clone());

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
                Err(e) => return Err(e.context("failed to load room membership for game start")),
            };
            let routed_players = self.coordinator.routed_player_ids(room_id).await?;
            let room_players: Vec<PlayerInfo> = match routed_players {
                Some(routed_players) => {
                    let routed: HashSet<PlayerId> = routed_players.into_iter().collect();
                    room_players
                        .into_iter()
                        .filter(|player| routed.contains(&player.id))
                        .collect()
                }
                None => room_players,
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

            Ok(StartGameOutcome::Started(FinalizedRoom {
                game_name: room.game_name.clone(),
                authority_player: room.authority_player,
                members: room_players,
            }))
        }
        .await;

        let completion = if let Ok(StartGameOutcome::Started(finalized)) = &result {
            let Some(lobby_state) = lobby_state_for_finalize.take() else {
                drop(mutation_guard);
                lock_guard.release().await;
                anyhow::bail!("start candidate omitted its pre-finalization lobby state");
            };
            let Some(start_guard) = mutation_guard.take() else {
                lock_guard.release().await;
                anyhow::bail!("start mutation guard was unavailable before publication");
            };
            let relay_type = relay_type_for_broadcast.take().unwrap_or_default();
            let peer_connections =
                PeerConnectionInfo::from_players(&finalized.members, &relay_type);
            let game_start_message =
                Arc::new(crate::protocol::ServerMessage::GameStarting { peer_connections });
            let expected_members: Vec<PlayerId> =
                finalized.members.iter().map(|member| member.id).collect();
            let finalize_expectation = crate::database::FinalizeRoomGameExpectation {
                members: expected_members.clone(),
                authority_player: finalized.authority_player,
                lobby_state,
            };
            let publication = build_publication(finalized, game_start_message);
            let coordinator = Arc::clone(&self.coordinator);
            let database = Arc::clone(&self.database);
            let ready_players = Arc::clone(&self.ready_players);
            let finalize_room_id = *room_id;
            Some(self.coordinator.enqueue_room_event(
                start_guard,
                Box::new(move || {
                    Box::pin(async move {
                        let StartGamePublication {
                            recipient_messages,
                            after_game_starting,
                        } = publication;
                        let outcome = coordinator
                            .commit_room_messages_if_members_with_hook(
                                &finalize_room_id,
                                &expected_members,
                                recipient_messages,
                                Box::new(move || {
                                    Box::pin(async move {
                                        match database
                                            .finalize_room_game(
                                                &finalize_room_id,
                                                &finalize_expectation,
                                            )
                                            .await?
                                        {
                                            crate::database::FinalizeRoomGameOutcome::Finalized => Ok(true),
                                            crate::database::FinalizeRoomGameOutcome::AlreadyFinalized => {
                                                Ok(false)
                                            }
                                            crate::database::FinalizeRoomGameOutcome::SnapshotChanged => {
                                                anyhow::bail!(
                                                    "room state changed while starting the game"
                                                )
                                            }
                                        }
                                    })
                                }),
                                Box::new(move |_failed_phase_zero| {
                                    after_game_starting();
                                    true
                                }),
                            )
                            .await?;
                        match outcome {
                            RoomMessageTransactionOutcome::Committed => {
                                ready_players.write().await.remove(&finalize_room_id);
                                Ok(true)
                            }
                            RoomMessageTransactionOutcome::CommittedDegraded {
                                failed_frames,
                            } => {
                                // The durable CAS won. A socket closed during
                                // that async hook, but healthy recipients were
                                // still attempted phase-by-phase and sticky
                                // session state was published. Finalization is
                                // authoritative, so ready state must not linger.
                                ready_players.write().await.remove(&finalize_room_id);
                                tracing::warn!(
                                    room_id = %finalize_room_id,
                                    failed_frames,
                                    "Game start committed with degraded frame delivery"
                                );
                                Ok(true)
                            }
                            RoomMessageTransactionOutcome::HookRejected => {
                                // Another coordinator won the durable start CAS.
                                // Its publication owns delivery, but this node's
                                // local ready snapshot is equally obsolete.
                                ready_players.write().await.remove(&finalize_room_id);
                                Ok(false)
                            }
                            RoomMessageTransactionOutcome::RoutingChanged => {
                                anyhow::bail!("room membership changed while starting the game")
                            }
                        }
                    })
                }),
            ))
        } else {
            None
        };

        // Release the TTL-bounded distributed lock before awaiting any
        // backpressured delivery. On the start path the detached FIFO job owns
        // the local mutation guard through persistence + broadcast.
        lock_guard.release().await;

        if let Ok(StartGameOutcome::Started(_finalized)) = &result {
            let Some(completion) = completion else {
                anyhow::bail!("successful game start did not enqueue its publication");
            };
            let committed = completion.await?;
            if !committed {
                return Ok(StartGameOutcome::AlreadyStarted);
            }

            tracing::info!(%room_id, %player_id, "Game started via explicit StartGame (in-memory)");
        }
        drop(mutation_guard);

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

    async fn forget_player_ready(&self, room_id: &RoomId, player_id: &PlayerId) {
        let mut ready_map = self.ready_players.write().await;
        let Some(room_ready_players) = ready_map.get_mut(room_id) else {
            return;
        };
        if !room_ready_players.remove(player_id) {
            return;
        }
        // Keep the map free of empty sets so a lobby that fully un-readies costs
        // nothing until the room-scoped entry is needed again.
        if room_ready_players.is_empty() {
            ready_map.remove(room_id);
        }
        tracing::debug!(%room_id, %player_id, "Dropped stale readiness for a new membership");
    }

    async fn ready_player_room_ids(&self) -> Vec<RoomId> {
        // Snapshot keys without holding the lock across any caller `.await`.
        self.ready_players.read().await.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordination::{
        ClientDeliveryHandle, ConnectionCloseSignal, MembershipUpdate, MessageCoordinator,
        RoomEventCompletion, RoomEventJob, RoomEventMutationGuard, RoomEventSequencer,
    };
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

    use crate::protocol::ErrorCode;

    #[test]
    fn player_ready_error_maps_each_variant_to_its_client_code() {
        // Only a Finalized room is a business rejection; the rest are infra
        // faults that must NOT surface as INVALID_ROOM_STATE.
        assert_eq!(
            PlayerReadyError::Finalized.error_code(),
            ErrorCode::InvalidRoomState
        );
        assert_eq!(
            PlayerReadyError::RoomNotFound.error_code(),
            ErrorCode::RoomNotFound
        );
        assert_eq!(
            PlayerReadyError::Internal(anyhow::anyhow!("lock busy")).error_code(),
            ErrorCode::InternalError
        );
    }

    #[tokio::test]
    async fn clear_ready_players_removes_the_rooms_coordinator_entry() {
        // Leak guard: a `PlayerReady` toggle creates a per-room ready entry in
        // the coordinator's in-memory map; that entry is pure garbage once the
        // room is deleted and must be removable. Maintenance uses this method
        // for explicit cleanup, with `prune_ready_players` as the all-paths
        // backstop; this verifies the shared removal mechanism.
        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(RecordingMessageCoordinator::default());
        let lock = Arc::new(InMemoryDistributedLock::new());
        let coord =
            InMemoryRoomOperationCoordinator::new(coordinator, lock, database.clone(), None);

        let player = PlayerId::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888);
        let room = database
            .create_room(
                "clear-ready-game".to_string(),
                None,
                2,
                true,
                player,
                "udp".to_string(),
                "region-a".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");

        coord
            .handle_player_ready(&room.id, &player, None)
            .await
            .expect("ready toggle succeeds");
        assert!(
            coord.ready_players.read().await.contains_key(&room.id),
            "a ready toggle must create the room's coordinator entry"
        );

        coord
            .clear_ready_players(&room.id)
            .await
            .expect("clear succeeds");
        assert!(
            !coord.ready_players.read().await.contains_key(&room.id),
            "the coordinator entry must be gone after clear (no stale retention)"
        );

        // Idempotent: clearing an already-absent room is a harmless no-op.
        coord
            .clear_ready_players(&room.id)
            .await
            .expect("repeat clear is a no-op");
    }

    #[tokio::test]
    async fn lobby_transition_preserves_ready_state_that_won_first() {
        let database = Arc::new(InMemoryDatabase::new());
        let messages = Arc::new(RecordingMessageCoordinator::default());
        let player = PlayerId::from_u128(0x1111_2222_3333_4444_5555_6666_7777_9898);
        let room = database
            .create_room(
                "ready-before-lobby".to_string(),
                None,
                1,
                true,
                player,
                "udp".to_string(),
                "region-a".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");
        let coord = InMemoryRoomOperationCoordinator::new(
            messages.clone(),
            Arc::new(InMemoryDistributedLock::new()),
            database,
            None,
        );

        coord
            .handle_player_ready(&room.id, &player, None)
            .await
            .expect("ready toggle succeeds while room is Waiting");
        assert!(coord
            .transition_room_to_lobby(&room.id)
            .await
            .expect("lobby transition succeeds"));

        let broadcasts = messages.broadcasts().await;
        assert_eq!(broadcasts.len(), 2);
        for event in broadcasts {
            match event.message {
                ServerMessage::LobbyStateChanged {
                    ready_players,
                    all_ready,
                    ..
                } => {
                    assert_eq!(ready_players, vec![player]);
                    assert!(all_ready);
                }
                other => panic!("expected lobby snapshot, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn delayed_ready_broadcast_preserves_mutation_order() {
        let database = Arc::new(InMemoryDatabase::new());
        let metrics = Arc::new(crate::metrics::ServerMetrics::new());
        let messages = Arc::new(
            crate::server::InMemoryMessageCoordinator::with_delivery_policy(
                Duration::from_secs(30),
                Arc::clone(&metrics),
            ),
        );
        let lock = Arc::new(InMemoryDistributedLock::new());
        let player = PlayerId::from_u128(0x1111_2222_3333_4444_5555_6666_7777_9901);
        let room = database
            .create_room(
                "ordered-ready-game".to_string(),
                None,
                1,
                true,
                player,
                "udp".to_string(),
                "region-a".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");
        let (sender, mut receiver) = mpsc::channel(1);
        sender
            .try_send(Arc::new(ServerMessage::Pong))
            .expect("pre-fill recipient queue");
        messages
            .register_local_client(
                player,
                Some(room.id),
                ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
            )
            .await
            .expect("route ready recipient");
        let coord = Arc::new(InMemoryRoomOperationCoordinator::new(
            messages, lock, database, None,
        ));

        let first = {
            let coord = Arc::clone(&coord);
            tokio::spawn(async move { coord.handle_player_ready(&room.id, &player, None).await })
        };
        for _ in 0..10_000 {
            if metrics
                .websocket_backpressure_events
                .load(Ordering::Relaxed)
                == 1
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            metrics
                .websocket_backpressure_events
                .load(Ordering::Relaxed),
            1,
            "first ready snapshot must be stalled before the second mutation"
        );
        let second = {
            let coord = Arc::clone(&coord);
            tokio::spawn(async move { coord.handle_player_ready(&room.id, &player, None).await })
        };

        assert!(matches!(
            receiver.recv().await.as_deref(),
            Some(ServerMessage::Pong)
        ));
        let first_snapshot = receiver.recv().await.expect("first ready snapshot");
        let second_snapshot = receiver.recv().await.expect("second ready snapshot");
        match first_snapshot.as_ref() {
            ServerMessage::LobbyStateChanged {
                ready_players,
                all_ready,
                ..
            } => {
                assert_eq!(ready_players, &[player]);
                assert!(*all_ready);
            }
            other => panic!("expected first ready snapshot, got {other:?}"),
        }
        match second_snapshot.as_ref() {
            ServerMessage::LobbyStateChanged {
                ready_players,
                all_ready,
                ..
            } => {
                assert!(ready_players.is_empty());
                assert!(!all_ready);
            }
            other => panic!("expected second ready snapshot, got {other:?}"),
        }
        first
            .await
            .expect("first ready task should not panic")
            .expect("first ready succeeds");
        second
            .await
            .expect("second ready task should not panic")
            .expect("second ready succeeds");
    }

    #[tokio::test]
    async fn delayed_authority_release_precedes_later_claim() {
        let database = Arc::new(InMemoryDatabase::new());
        let metrics = Arc::new(crate::metrics::ServerMetrics::new());
        let messages = Arc::new(
            crate::server::InMemoryMessageCoordinator::with_delivery_policy(
                Duration::from_secs(30),
                Arc::clone(&metrics),
            ),
        );
        let lock = Arc::new(InMemoryDistributedLock::new());
        let authority = PlayerId::from_u128(0x1111_2222_3333_4444_5555_6666_7777_9911);
        let claimant = PlayerId::from_u128(0x1111_2222_3333_4444_5555_6666_7777_9912);
        let room = database
            .create_room(
                "ordered-authority-game".to_string(),
                None,
                2,
                true,
                authority,
                "udp".to_string(),
                "region-a".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");
        assert!(database
            .add_player_to_room(
                &room.id,
                PlayerInfo {
                    id: claimant,
                    name: "claimant".to_string(),
                    is_authority: false,
                    is_ready: false,
                    connected_at: chrono::Utc::now(),
                    connection_info: None,
                    epoch: None,
                    seq: None,
                    region_id: "region-a".to_string(),
                },
            )
            .await
            .expect("add claimant"));

        let (authority_sender, mut authority_receiver) = mpsc::channel(1);
        authority_sender
            .try_send(Arc::new(ServerMessage::Pong))
            .expect("pre-fill authority queue");
        let (claimant_sender, mut claimant_receiver) = mpsc::channel(8);
        messages
            .register_local_client(
                authority,
                Some(room.id),
                ClientDeliveryHandle::new(authority_sender, ConnectionCloseSignal::detached()),
            )
            .await
            .expect("route authority");
        messages
            .register_local_client(
                claimant,
                Some(room.id),
                ClientDeliveryHandle::new(claimant_sender, ConnectionCloseSignal::detached()),
            )
            .await
            .expect("route claimant");
        let coord = Arc::new(InMemoryRoomOperationCoordinator::new(
            messages, lock, database, None,
        ));

        let release = {
            let coord = Arc::clone(&coord);
            tokio::spawn(async move {
                coord
                    .handle_authority_request(&room.id, &authority, false)
                    .await
            })
        };
        for _ in 0..10_000 {
            if metrics
                .websocket_backpressure_events
                .load(Ordering::Relaxed)
                == 1
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        let claim = {
            let coord = Arc::clone(&coord);
            tokio::spawn(async move {
                coord
                    .handle_authority_request(&room.id, &claimant, true)
                    .await
            })
        };

        assert!(matches!(
            authority_receiver.recv().await.as_deref(),
            Some(ServerMessage::Pong)
        ));
        assert!(matches!(
            authority_receiver.recv().await.as_deref(),
            Some(ServerMessage::AuthorityResponse { granted: true, .. })
        ));
        assert!(matches!(
            authority_receiver.recv().await.as_deref(),
            Some(ServerMessage::AuthorityChanged {
                authority_player: None,
                you_are_authority: false,
            })
        ));
        assert!(matches!(
            authority_receiver.recv().await.as_deref(),
            Some(ServerMessage::AuthorityChanged {
                authority_player: Some(id),
                you_are_authority: false,
            }) if *id == claimant
        ));
        assert!(matches!(
            claimant_receiver.recv().await.as_deref(),
            Some(ServerMessage::AuthorityChanged {
                authority_player: None,
                you_are_authority: false,
            })
        ));
        assert!(matches!(
            claimant_receiver.recv().await.as_deref(),
            Some(ServerMessage::AuthorityResponse { granted: true, .. })
        ));
        assert!(matches!(
            claimant_receiver.recv().await.as_deref(),
            Some(ServerMessage::AuthorityChanged {
                authority_player: Some(id),
                you_are_authority: true,
            }) if *id == claimant
        ));
        release
            .await
            .expect("release task should not panic")
            .expect("release succeeds");
        claim
            .await
            .expect("claim task should not panic")
            .expect("claim succeeds");
    }

    #[tokio::test]
    async fn aborted_authority_request_after_commit_still_publishes_exactly_once() {
        let database = Arc::new(InMemoryDatabase::new());
        let messages = Arc::new(crate::server::InMemoryMessageCoordinator::new());
        let lock = Arc::new(InMemoryDistributedLock::new());
        let replay = Arc::new(crate::reconnection::ReconnectionManager::new(
            30,
            8,
            Arc::new(crate::metrics::ServerMetrics::new()),
        ));
        let authority = PlayerId::from_u128(0x1111_2222_3333_4444_5555_6666_7777_9921);
        let room = database
            .create_room(
                "abort-authority-game".to_string(),
                None,
                2,
                true,
                authority,
                "udp".to_string(),
                "region-a".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");
        let (sender, mut receiver) = mpsc::channel(8);
        messages
            .register_local_client(
                authority,
                Some(room.id),
                ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
            )
            .await
            .expect("route authority");
        let pending_reconnector = PlayerId::from_u128(0x1111_2222_3333_4444_5555_6666_7777_9922);
        replay
            .register_disconnection(pending_reconnector, room.id, false, None, 0)
            .await;
        let coord = Arc::new(InMemoryRoomOperationCoordinator::new(
            messages,
            lock.clone(),
            database.clone(),
            Some(replay.clone()),
        ));
        database.pause_authority_request_after_commit_for_test();

        let request = {
            let coord = Arc::clone(&coord);
            tokio::spawn(async move {
                coord
                    .handle_authority_request(&room.id, &authority, false)
                    .await
            })
        };
        timeout(
            Duration::from_secs(1),
            database.wait_for_authority_request_commit_for_test(),
        )
        .await
        .expect("authority mutation reaches the post-commit pause");
        assert_eq!(
            database
                .get_room_by_id(&room.id)
                .await
                .expect("read committed authority state")
                .expect("room remains present")
                .authority_player,
            None,
            "storage commit occurs before the caller is canceled"
        );

        request.abort();
        request
            .await
            .expect_err("test aborts only the caller awaiting the owned transaction");
        database.release_authority_request_commit_for_test();

        let response = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("authority response arrives");
        assert!(matches!(
            response.as_deref(),
            Some(ServerMessage::AuthorityResponse { granted: true, .. })
        ));
        let event = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("authority event arrives");
        assert!(matches!(
            event.as_deref(),
            Some(ServerMessage::AuthorityChanged {
                authority_player: None,
                you_are_authority: false,
            })
        ));
        let unexpected = receiver.try_recv();
        assert!(matches!(unexpected, Err(mpsc::error::TryRecvError::Empty)));
        let replayed = replay.get_missed_events(&room.id, 0).await;
        assert_eq!(
            replayed
                .events
                .iter()
                .filter(|event| matches!(
                    event,
                    ServerMessage::AuthorityChanged {
                        authority_player: None,
                        ..
                    }
                ))
                .count(),
            1,
            "the committed authority change is recorded exactly once"
        );
        assert!(
            !lock
                .is_locked(&format!("room_authority:{}", room.id))
                .await
                .expect("lock state is readable"),
            "delivery wait must not retain the TTL-bounded authority lock"
        );
    }

    #[derive(Debug, Clone)]
    struct BroadcastEvent {
        room_id: RoomId,
        message: ServerMessage,
    }

    #[derive(Default)]
    struct RecordingMessageCoordinator {
        room_events: Arc<RoomEventSequencer>,
        broadcasts: Mutex<Vec<BroadcastEvent>>,
        failed_broadcast_calls: Mutex<HashSet<usize>>,
        broadcast_attempts: AtomicUsize,
        degraded_transaction_frames: AtomicUsize,
    }

    impl RecordingMessageCoordinator {
        async fn fail_broadcast_on(&self, call_number: usize) {
            self.failed_broadcast_calls.lock().await.insert(call_number);
        }

        async fn broadcasts(&self) -> Vec<BroadcastEvent> {
            self.broadcasts.lock().await.clone()
        }

        fn degrade_next_transaction(&self, failed_frames: usize) {
            self.degraded_transaction_frames
                .store(failed_frames, Ordering::Release);
        }
    }

    #[async_trait]
    impl MessageCoordinator for RecordingMessageCoordinator {
        async fn lock_room_event_mutation(&self, room_id: &RoomId) -> RoomEventMutationGuard {
            self.room_events.lock(*room_id).await
        }

        fn enqueue_room_event(
            &self,
            mutation_guard: RoomEventMutationGuard,
            job: RoomEventJob,
        ) -> RoomEventCompletion {
            self.room_events.enqueue(mutation_guard, job)
        }

        async fn send_to_player(
            &self,
            _player_id: &PlayerId,
            _message: Arc<ServerMessage>,
        ) -> Result<()> {
            Ok(())
        }

        async fn try_send_to_player(
            &self,
            player_id: &PlayerId,
            message: Arc<ServerMessage>,
        ) -> Result<bool> {
            // Test double: send_to_player is non-blocking here, so delegating
            // honors the non-waiting farewell contract while preserving
            // whatever recording/blocking behavior the double implements.
            self.send_to_player(player_id, message).await.map(|()| true)
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

        async fn broadcast_to_room_with_hook<'a>(
            &'a self,
            room_id: &RoomId,
            message: Arc<ServerMessage>,
            before_send: Box<
                dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                    + Send
                    + 'a,
            >,
        ) -> Result<bool> {
            before_send().await;
            self.broadcast_to_room(room_id, message).await?;
            Ok(true)
        }

        async fn broadcast_to_room_if_members_with_hook<'a>(
            &'a self,
            room_id: &RoomId,
            _expected_members: &[PlayerId],
            message: Arc<ServerMessage>,
            before_send: Box<
                dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                    + Send
                    + 'a,
            >,
        ) -> Result<bool> {
            self.broadcast_to_room_with_hook(room_id, message, before_send)
                .await
        }

        async fn broadcast_to_room_except_if_with_hook<'a>(
            &'a self,
            room_id: &RoomId,
            except_player: &PlayerId,
            message: Arc<ServerMessage>,
            should_send: &(dyn Fn() -> bool + Send + Sync),
            drain: tokio::sync::watch::Receiver<bool>,
            before_send: Box<
                dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                    + Send
                    + 'a,
            >,
        ) -> Result<bool> {
            if *drain.borrow() || !should_send() {
                return Ok(false);
            }
            before_send().await;
            self.broadcast_to_room_except(room_id, except_player, message)
                .await?;
            Ok(true)
        }

        async fn commit_room_messages_if_members_with_hook<'a>(
            &'a self,
            room_id: &RoomId,
            _expected_members: &[PlayerId],
            recipient_messages: Vec<RoomRecipientMessages>,
            before_send: Box<
                dyn FnOnce() -> std::pin::Pin<
                        Box<dyn std::future::Future<Output = Result<bool>> + Send + 'a>,
                    > + Send
                    + 'a,
            >,
            after_first_phase: Box<dyn FnOnce(usize) -> bool + Send + 'a>,
        ) -> Result<RoomMessageTransactionOutcome> {
            let call_number = self.broadcast_attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if self
                .failed_broadcast_calls
                .lock()
                .await
                .contains(&call_number)
            {
                anyhow::bail!("injected broadcast failure on call {call_number}");
            }
            if !before_send().await? {
                return Ok(RoomMessageTransactionOutcome::HookRejected);
            }
            let message = recipient_messages
                .first()
                .and_then(|batch| batch.messages.first())
                .expect("start transaction has a uniform first frame");
            self.broadcasts.lock().await.push(BroadcastEvent {
                room_id: *room_id,
                message: message.as_ref().clone(),
            });
            let _ = after_first_phase(0);
            let failed_frames = self.degraded_transaction_frames.swap(0, Ordering::AcqRel);
            Ok(if failed_frames == 0 {
                RoomMessageTransactionOutcome::Committed
            } else {
                RoomMessageTransactionOutcome::CommittedDegraded { failed_frames }
            })
        }

        async fn register_local_client(
            &self,
            _player_id: PlayerId,
            _room_id: Option<RoomId>,
            _delivery: crate::coordination::ClientDeliveryHandle,
        ) -> Result<()> {
            Ok(())
        }

        async fn unroute_local_client_with_tail<'a>(
            &'a self,
            _player_id: PlayerId,
            _room_id: RoomId,
            clear_assignment: Box<
                dyn FnOnce() -> Option<(crate::coordination::ClientDeliveryHandle, u32, u64)>
                    + Send
                    + 'a,
            >,
        ) -> Result<Option<(u32, u64)>> {
            Ok(clear_assignment().map(|(_, epoch, final_seq)| (epoch, final_seq)))
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
        room_events: Arc<RoomEventSequencer>,
        broadcast_started: Notify,
        release_broadcast: Notify,
    }

    impl BlockingBroadcastMessageCoordinator {
        fn new() -> Self {
            Self {
                room_events: Arc::new(RoomEventSequencer::default()),
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
        async fn lock_room_event_mutation(&self, room_id: &RoomId) -> RoomEventMutationGuard {
            self.room_events.lock(*room_id).await
        }

        fn enqueue_room_event(
            &self,
            mutation_guard: RoomEventMutationGuard,
            job: RoomEventJob,
        ) -> RoomEventCompletion {
            self.room_events.enqueue(mutation_guard, job)
        }

        async fn send_to_player(
            &self,
            _player_id: &PlayerId,
            _message: Arc<ServerMessage>,
        ) -> Result<()> {
            Ok(())
        }

        async fn try_send_to_player(
            &self,
            player_id: &PlayerId,
            message: Arc<ServerMessage>,
        ) -> Result<bool> {
            // Test double: send_to_player is non-blocking here, so delegating
            // honors the non-waiting farewell contract while preserving
            // whatever recording/blocking behavior the double implements.
            self.send_to_player(player_id, message).await.map(|()| true)
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

        async fn broadcast_to_room_with_hook<'a>(
            &'a self,
            room_id: &RoomId,
            message: Arc<ServerMessage>,
            before_send: Box<
                dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                    + Send
                    + 'a,
            >,
        ) -> Result<bool> {
            before_send().await;
            self.broadcast_to_room(room_id, message).await?;
            Ok(true)
        }

        async fn broadcast_to_room_if_members_with_hook<'a>(
            &'a self,
            room_id: &RoomId,
            _expected_members: &[PlayerId],
            message: Arc<ServerMessage>,
            before_send: Box<
                dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                    + Send
                    + 'a,
            >,
        ) -> Result<bool> {
            self.broadcast_to_room_with_hook(room_id, message, before_send)
                .await
        }

        async fn broadcast_to_room_except_if_with_hook<'a>(
            &'a self,
            room_id: &RoomId,
            except_player: &PlayerId,
            message: Arc<ServerMessage>,
            should_send: &(dyn Fn() -> bool + Send + Sync),
            drain: tokio::sync::watch::Receiver<bool>,
            before_send: Box<
                dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                    + Send
                    + 'a,
            >,
        ) -> Result<bool> {
            if *drain.borrow() || !should_send() {
                return Ok(false);
            }
            before_send().await;
            self.broadcast_to_room_except(room_id, except_player, message)
                .await?;
            Ok(true)
        }

        async fn commit_room_messages_if_members_with_hook<'a>(
            &'a self,
            _room_id: &RoomId,
            _expected_members: &[PlayerId],
            _recipient_messages: Vec<RoomRecipientMessages>,
            before_send: Box<
                dyn FnOnce() -> std::pin::Pin<
                        Box<dyn std::future::Future<Output = Result<bool>> + Send + 'a>,
                    > + Send
                    + 'a,
            >,
            after_first_phase: Box<dyn FnOnce(usize) -> bool + Send + 'a>,
        ) -> Result<RoomMessageTransactionOutcome> {
            self.broadcast_started.notify_one();
            self.release_broadcast.notified().await;
            if before_send().await? {
                let _ = after_first_phase(0);
                Ok(RoomMessageTransactionOutcome::Committed)
            } else {
                Ok(RoomMessageTransactionOutcome::HookRejected)
            }
        }

        async fn register_local_client(
            &self,
            _player_id: PlayerId,
            _room_id: Option<RoomId>,
            _delivery: crate::coordination::ClientDeliveryHandle,
        ) -> Result<()> {
            Ok(())
        }

        async fn unroute_local_client_with_tail<'a>(
            &'a self,
            _player_id: PlayerId,
            _room_id: RoomId,
            clear_assignment: Box<
                dyn FnOnce() -> Option<(crate::coordination::ClientDeliveryHandle, u32, u64)>
                    + Send
                    + 'a,
            >,
        ) -> Result<Option<(u32, u64)>> {
            Ok(clear_assignment().map(|(_, epoch, final_seq)| (epoch, final_seq)))
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

        fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
            self
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
            epoch: None,
            seq: None,
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
        let room_coordinator = InMemoryRoomOperationCoordinator::new(
            coordinator,
            lock.clone(),
            database.clone(),
            None,
        );
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
    async fn aborting_lobby_transition_after_commit_does_not_strand_its_event() {
        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(BlockingBroadcastMessageCoordinator::new());
        let lock = Arc::new(InMemoryDistributedLock::new());
        let room_coordinator = InMemoryRoomOperationCoordinator::new(
            coordinator.clone(),
            lock.clone(),
            database.clone(),
            None,
        );
        let player_id = PlayerId::from_u128(0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbc);
        let room = database
            .create_room(
                "owned-lobby-transition".to_string(),
                Some("OWN001".to_string()),
                4,
                true,
                player_id,
                "matchbox".to_string(),
                "test-region".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");

        let transition = {
            let room_coordinator = room_coordinator.clone();
            tokio::spawn(async move { room_coordinator.transition_room_to_lobby(&room.id).await })
        };
        coordinator.wait_for_broadcast_start().await;
        assert_eq!(
            database
                .get_room_by_id(&room.id)
                .await
                .expect("room lookup succeeds")
                .expect("room remains present")
                .lobby_state,
            crate::protocol::LobbyState::Lobby,
            "the durable transition happens before the queued broadcast"
        );

        transition.abort();
        transition
            .await
            .expect_err("test aborts only the caller awaiting the owned transaction");
        coordinator.release_broadcast();

        let room_guard = timeout(
            Duration::from_secs(1),
            coordinator.lock_room_event_mutation(&room.id),
        )
        .await
        .expect("owned event finishes and releases its room mutation guard");
        drop(room_guard);
        wait_until_unlocked(&lock, &format!("room_lobby_transition:{}", room.id)).await;
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
            InMemoryRoomOperationCoordinator::new(coordinator, lock.clone(), database, None);

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
            InMemoryRoomOperationCoordinator::new(coordinator, lock.clone(), database, None);

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
        let ready_coordinator = InMemoryRoomOperationCoordinator::new(
            coordinator,
            lock.clone(),
            database.clone(),
            None,
        );
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
        let ready_coordinator = InMemoryRoomOperationCoordinator::new(
            coordinator,
            lock.clone(),
            database.clone(),
            None,
        );
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
        let persisted = database
            .get_room_by_id(&room.id)
            .await
            .expect("room lookup succeeds")
            .expect("room remains present");
        assert_eq!(
            persisted.lobby_state,
            crate::protocol::LobbyState::Lobby,
            "a pre-commit publication failure must not finalize durable state"
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
            InMemoryRoomOperationCoordinator::new(coordinator, lock.clone(), database, None);
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
            None,
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
        .expect("ready operation should reach the post-release broadcast");
        // Delivery can be backpressured by a slow recipient (bounded by the
        // slow-consumer timeout), so the TTL-bounded room-operation lock must
        // be released BEFORE any coordinator send starts — otherwise a slow
        // consumer could stretch the critical section past the lock TTL and
        // break mutual exclusion for concurrent room operations.
        assert!(
            !lock
                .is_locked(&lock_key)
                .await
                .expect("lock state can be read"),
            "ready-state lock must be released before the lobby broadcast starts"
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
    async fn aborted_start_request_still_finishes_publication_and_ready_cleanup() {
        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(BlockingBroadcastMessageCoordinator::new());
        let ready_coordinator = Arc::new(InMemoryRoomOperationCoordinator::new(
            coordinator.clone(),
            Arc::new(InMemoryDistributedLock::new()),
            database.clone(),
            None,
        ));
        let player = PlayerId::from_u128(0x0a0b0c0d0e0f10111213141516171819);
        let room = database
            .create_room(
                "aborted-start".to_string(),
                Some("ABRT01".to_string()),
                1,
                false,
                player,
                "test-relay".to_string(),
                "test-region".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");
        database
            .transition_room_to_lobby(&room.id)
            .await
            .expect("room enters lobby");
        ready_coordinator
            .ready_players
            .write()
            .await
            .entry(room.id)
            .or_default()
            .insert(player);

        let start_task = {
            let ready_coordinator = Arc::clone(&ready_coordinator);
            tokio::spawn(
                async move { ready_coordinator.handle_start_game(&room.id, &player).await },
            )
        };
        coordinator.wait_for_broadcast_start().await;
        start_task.abort();
        coordinator.release_broadcast();

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let finalized = database
                    .get_room_by_id(&room.id)
                    .await
                    .expect("room lookup succeeds")
                    .is_some_and(|room| room.lobby_state == crate::protocol::LobbyState::Finalized);
                if finalized
                    && ready_coordinator
                        .current_ready_players(&room.id)
                        .await
                        .is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached start transaction must outlive its canceled caller");
    }

    #[tokio::test]
    async fn ready_membership_read_failure_does_not_mutate_or_publish_empty_state() {
        let database = Arc::new(InMemoryDatabase::new());
        let messages = Arc::new(RecordingMessageCoordinator::default());
        let coordinator = InMemoryRoomOperationCoordinator::new(
            messages.clone(),
            Arc::new(NoopDistributedLock),
            database.clone(),
            None,
        );
        let player = PlayerId::from_u128(0x9101);
        let room = database
            .create_room(
                "ready-storage-failure".to_string(),
                Some("RFAIL1".to_string()),
                2,
                false,
                player,
                "matchbox".to_string(),
                "test-region".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");
        database.fail_get_room_players_for_test(true);

        assert!(matches!(
            coordinator
                .handle_player_ready(&room.id, &player, None)
                .await,
            Err(PlayerReadyError::Internal(_))
        ));
        assert!(coordinator.current_ready_players(&room.id).await.is_empty());
        assert!(messages.broadcasts().await.is_empty());
    }

    #[tokio::test]
    async fn start_membership_read_failure_is_infrastructure_error_not_not_ready() {
        let database = Arc::new(InMemoryDatabase::new());
        let messages = Arc::new(RecordingMessageCoordinator::default());
        let coordinator = InMemoryRoomOperationCoordinator::new(
            messages.clone(),
            Arc::new(NoopDistributedLock),
            database.clone(),
            None,
        );
        let player = PlayerId::from_u128(0x9102);
        let room = database
            .create_room(
                "start-storage-failure".to_string(),
                Some("SFAIL1".to_string()),
                2,
                false,
                player,
                "matchbox".to_string(),
                "test-region".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");
        coordinator
            .ready_players
            .write()
            .await
            .insert(room.id, HashSet::from([player]));
        database.fail_get_room_players_for_test(true);

        assert!(coordinator
            .handle_start_game(&room.id, &player)
            .await
            .is_err());
        assert_eq!(
            database
                .get_room_by_id(&room.id)
                .await
                .expect("room read succeeds")
                .expect("room remains present")
                .lobby_state,
            LobbyState::Waiting
        );
        assert_eq!(
            coordinator.current_ready_players(&room.id).await,
            vec![player]
        );
        assert!(messages.broadcasts().await.is_empty());
    }

    #[tokio::test]
    async fn handle_player_ready_does_not_wait_for_previous_ready_lock_ttl() {
        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(RecordingMessageCoordinator::default());
        let lock = Arc::new(InMemoryDistributedLock::new());
        let ready_coordinator = InMemoryRoomOperationCoordinator::new(
            coordinator,
            lock.clone(),
            database.clone(),
            None,
        );
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
    async fn start_game_rejects_until_database_and_published_membership_match() {
        let database = Arc::new(InMemoryDatabase::new());
        let messages = Arc::new(crate::server::InMemoryMessageCoordinator::new());
        let authority = PlayerId::from_u128(0xeeee_eeee_eeee_eeee_eeee_eeee_eeee_ee01);
        let pending = PlayerId::from_u128(0xeeee_eeee_eeee_eeee_eeee_eeee_eeee_ee02);
        let room = database
            .create_room(
                "published-start-game".to_string(),
                None,
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
            .add_player_to_room(&room.id, player_fixture(pending, "Pending", false, None),)
            .await
            .expect("pending DB membership succeeds"));

        let (authority_sender, mut authority_receiver) = mpsc::channel(4);
        messages
            .register_local_client(
                authority,
                Some(room.id),
                ClientDeliveryHandle::new(authority_sender, ConnectionCloseSignal::detached()),
            )
            .await
            .expect("publish authority route");
        let coord = InMemoryRoomOperationCoordinator::new(
            messages.clone(),
            Arc::new(InMemoryDistributedLock::new()),
            database.clone(),
            None,
        );
        coord
            .ready_players
            .write()
            .await
            .insert(room.id, HashSet::from([authority, pending]));

        let first_start = coord.handle_start_game(&room.id, &authority).await;
        assert!(
            first_start.is_err(),
            "DB-only membership must reject rather than finalize a partial snapshot"
        );
        let unexpected = authority_receiver.try_recv();
        assert!(matches!(unexpected, Err(mpsc::error::TryRecvError::Empty)));
        let before_publication = database
            .get_room_by_id(&room.id)
            .await
            .expect("read room after rejected start")
            .expect("room remains present");
        assert_ne!(before_publication.lobby_state, LobbyState::Finalized);

        let (pending_sender, mut pending_receiver) = mpsc::channel(4);
        messages
            .register_local_client(
                pending,
                Some(room.id),
                ClientDeliveryHandle::new(pending_sender, ConnectionCloseSignal::detached()),
            )
            .await
            .expect("publish pending route");

        let finalized = match coord
            .handle_start_game(&room.id, &authority)
            .await
            .expect("start succeeds once both snapshots match")
        {
            StartGameOutcome::Started(finalized) => finalized,
            other => panic!("matching membership should start, got {other:?}"),
        };
        let mut finalized_ids: Vec<PlayerId> =
            finalized.members.iter().map(|member| member.id).collect();
        finalized_ids.sort_unstable();
        let mut expected_ids = vec![authority, pending];
        expected_ids.sort_unstable();
        assert_eq!(finalized_ids, expected_ids);
        for receiver in [&mut authority_receiver, &mut pending_receiver] {
            match receiver
                .recv()
                .await
                .expect("every published member receives GameStarting")
                .as_ref()
            {
                ServerMessage::GameStarting { peer_connections } => {
                    let mut peers: Vec<PlayerId> =
                        peer_connections.iter().map(|peer| peer.player_id).collect();
                    peers.sort_unstable();
                    assert_eq!(peers, expected_ids);
                }
                other => panic!("expected GameStarting, got {other:?}"),
            }
        }
        let persisted = database
            .get_room_by_id(&room.id)
            .await
            .expect("read finalized room")
            .expect("room remains present");
        assert_eq!(persisted.lobby_state, LobbyState::Finalized);
    }

    #[tokio::test]
    async fn degraded_start_commit_still_publishes_state_and_clears_ready_snapshot() {
        let database = Arc::new(InMemoryDatabase::new());
        let messages = Arc::new(RecordingMessageCoordinator::default());
        let coordinator = InMemoryRoomOperationCoordinator::new(
            messages.clone(),
            Arc::new(NoopDistributedLock),
            database.clone(),
            None,
        );
        let player = PlayerId::from_u128(0xd301);
        let room = database
            .create_room(
                "degraded-start".to_string(),
                Some("DGRD01".to_string()),
                1,
                false,
                player,
                "matchbox".to_string(),
                "test-region".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");
        coordinator
            .ready_players
            .write()
            .await
            .insert(room.id, HashSet::from([player]));
        messages.degrade_next_transaction(2);
        let state_callbacks = Arc::new(AtomicUsize::new(0));
        let callback_marker = Arc::clone(&state_callbacks);

        let outcome = coordinator
            .handle_start_game_with_publication(
                &room.id,
                &player,
                Box::new(move |finalized, game_starting| StartGamePublication {
                    recipient_messages: finalized
                        .members
                        .iter()
                        .map(|member| RoomRecipientMessages {
                            player_id: member.id,
                            first_phase: 0,
                            messages: vec![
                                Arc::clone(&game_starting),
                                Arc::new(ServerMessage::Pong),
                            ],
                        })
                        .collect(),
                    after_game_starting: Box::new(move || {
                        callback_marker.fetch_add(1, Ordering::AcqRel);
                    }),
                }),
            )
            .await
            .expect("durable start remains successful under degraded delivery");

        assert!(matches!(outcome, StartGameOutcome::Started(_)));
        assert_eq!(state_callbacks.load(Ordering::Acquire), 1);
        assert!(coordinator.current_ready_players(&room.id).await.is_empty());
        assert_eq!(
            database
                .get_room_by_id(&room.id)
                .await
                .expect("room read succeeds")
                .expect("room remains present")
                .lobby_state,
            LobbyState::Finalized
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
            None,
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
            None,
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
            None,
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
            None,
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
        // Two concurrent StartGame calls: capacity is reserved before the
        // durable CAS, so exactly one publishes and the CAS loser observes the
        // normal AlreadyStarted outcome without a second GameStarting.
        let database = Arc::new(InMemoryDatabase::new());
        let coordinator = Arc::new(RecordingMessageCoordinator::default());
        let coord = Arc::new(InMemoryRoomOperationCoordinator::new(
            coordinator.clone(),
            Arc::new(InMemoryDistributedLock::new()),
            database.clone(),
            None,
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
        let already_started = [&r1, &r2]
            .iter()
            .filter(|result| matches!(result, Ok(StartGameOutcome::AlreadyStarted)))
            .count();
        assert_eq!(
            already_started, 1,
            "the CAS loser must report AlreadyStarted"
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

    #[tokio::test(start_paused = true)]
    async fn independent_membership_change_while_capacity_waits_rejects_start_cas() {
        let database = Arc::new(InMemoryDatabase::new());
        let metrics = Arc::new(crate::metrics::ServerMetrics::new());
        let message_coordinator = Arc::new(
            crate::server::InMemoryMessageCoordinator::with_delivery_policy(
                Duration::from_secs(1),
                Arc::clone(&metrics),
            ),
        );
        let coord = Arc::new(InMemoryRoomOperationCoordinator::new(
            message_coordinator.clone(),
            Arc::new(NoopDistributedLock),
            database.clone(),
            None,
        ));
        let alice = PlayerId::from_u128(0xd101);
        let bob = PlayerId::from_u128(0xd102);
        let room = database
            .create_room(
                "membership-cas".to_string(),
                Some("CAS001".to_string()),
                2,
                false,
                alice,
                "matchbox".to_string(),
                "test-region".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");
        assert!(database
            .add_player_to_room(&room.id, player_fixture(bob, "Bob", false, None))
            .await
            .expect("adding bob succeeds"));
        database
            .transition_room_to_lobby(&room.id)
            .await
            .expect("room enters lobby");
        coord
            .ready_players
            .write()
            .await
            .insert(room.id, HashSet::from([alice, bob]));

        let (alice_tx, mut alice_rx) = mpsc::channel(2);
        let (bob_tx, mut bob_rx) = mpsc::channel(2);
        bob_tx.try_send(Arc::new(ServerMessage::Pong)).unwrap();
        bob_tx.try_send(Arc::new(ServerMessage::Pong)).unwrap();
        for (player, sender) in [(alice, alice_tx), (bob, bob_tx)] {
            message_coordinator
                .register_local_client(
                    player,
                    Some(room.id),
                    ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
                )
                .await
                .expect("publish routed member");
        }

        let start = {
            let coord = Arc::clone(&coord);
            tokio::spawn(async move {
                coord
                    .handle_start_game_with_publication(
                        &room.id,
                        &alice,
                        Box::new(|finalized, game_starting| StartGamePublication {
                            recipient_messages: finalized
                                .members
                                .iter()
                                .map(|member| RoomRecipientMessages {
                                    player_id: member.id,
                                    first_phase: 0,
                                    messages: vec![
                                        Arc::clone(&game_starting),
                                        Arc::new(ServerMessage::Pong),
                                    ],
                                })
                                .collect(),
                            after_game_starting: Box::new(|| {}),
                        }),
                    )
                    .await
            })
        };
        for _ in 0..10_000 {
            if metrics.websocket_delivery_attempts.load(Ordering::Relaxed) >= 3 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            metrics.websocket_delivery_attempts.load(Ordering::Relaxed) >= 3,
            "the transaction must be waiting on Bob's reserved frame capacity"
        );

        // Models an independent lifecycle coordinator that shares storage but
        // not this node's local event gate or routing map.
        database
            .remove_player_from_room(&room.id, &bob)
            .await
            .expect("independent membership mutation succeeds")
            .expect("bob was present");
        assert!(matches!(
            bob_rx.recv().await.as_deref(),
            Some(ServerMessage::Pong)
        ));
        assert!(matches!(
            bob_rx.recv().await.as_deref(),
            Some(ServerMessage::Pong)
        ));

        let result = start.await.expect("start task must not panic");
        assert!(
            result.is_err(),
            "snapshot drift must reject the finalize CAS"
        );
        let persisted = database
            .get_room_by_id(&room.id)
            .await
            .expect("room lookup succeeds")
            .expect("room remains present");
        assert_ne!(persisted.lobby_state, LobbyState::Finalized);
        assert_eq!(
            metrics
                .websocket_slow_consumer_disconnects
                .load(Ordering::Relaxed),
            0,
            "the deterministic capacity wait must not race its delivery deadline"
        );
        let unexpected_alice = alice_rx.try_recv();
        assert!(
            matches!(unexpected_alice, Err(mpsc::error::TryRecvError::Empty)),
            "Alice received unexpected post-CAS output: {unexpected_alice:?}"
        );
        let unexpected_bob = bob_rx.try_recv();
        assert!(
            matches!(unexpected_bob, Err(mpsc::error::TryRecvError::Empty)),
            "Bob received unexpected post-CAS output: {unexpected_bob:?}"
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
            None,
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
            None,
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
