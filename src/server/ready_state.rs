use crate::coordination::{
    ConnectionCloseSignal, MessageCoordinator, PlayerReadyError, RoomEventMutationGuard,
    RoomOperationCoordinatorTrait, StartGameOutcome,
};
use crate::protocol::{ErrorCode, LobbyState, PlayerId, PlayerInfo, Room, RoomId};
use std::sync::Arc;
use tokio::sync::OwnedMutexGuard;

use super::{ClientLifecycle, EnhancedGameServer};

/// The readiness a room snapshot (`RoomJoined`, `Reconnected`, `SpectatorJoined`)
/// must report for `room`.
///
/// Readiness lives in the coordinator while the room is open — the stored player
/// records stay `false` for the whole lobby. Finalization moves it: the
/// coordinator's entry is dropped and the final set is written into the room
/// record. Reading only one of the two would report an all-unready lobby before
/// the game starts, or an all-unready game after it.
pub(crate) async fn snapshot_ready_players(
    room: &Room,
    room_coordinator: &dyn RoomOperationCoordinatorTrait,
) -> Vec<PlayerId> {
    if room.lobby_state == LobbyState::Finalized {
        return room.ready_players.clone();
    }
    room_coordinator.current_ready_players(&room.id).await
}

/// Set snapshot flags without changing player or ready-list order.
/// Use bounded scratch storage within the shipped room limit and retain larger custom inputs.
pub(crate) fn apply_snapshot_readiness(players: &mut [PlayerInfo], ready_players: &[PlayerId]) {
    if players.len() >= 128 && ready_players.len() >= 128 {
        let mut inline = [PlayerId::nil(); u8::MAX as usize];
        let mut overflow;
        let ready = if let Some(ready) = inline.get_mut(..ready_players.len()) {
            ready.copy_from_slice(ready_players);
            ready
        } else {
            overflow = ready_players.to_vec();
            &mut overflow
        };
        ready.sort_unstable();
        for player in players {
            player.is_ready = ready.binary_search(&player.id).is_ok();
        }
    } else {
        for player in players {
            player.is_ready = ready_players.contains(&player.id);
        }
    }
}

struct LobbyCommandSource {
    close: ConnectionCloseSignal,
    _lifecycle_guard: OwnedMutexGuard<()>,
}

impl LobbyCommandSource {
    async fn admit_room(
        &self,
        coordinator: &dyn MessageCoordinator,
        room_id: &RoomId,
    ) -> Option<RoomEventMutationGuard> {
        let room_guard = self
            .close
            .while_open(coordinator.lock_room_event_mutation(room_id))
            .await?;
        // The optimistic poll can acquire the gate as close is requested.
        // Order admission against close before the worker can change the room.
        self.close.commit_if_open(|| room_guard)
    }
}

impl EnhancedGameServer {
    /// Pin the source before scheduling its command. Close can interrupt the wait.
    async fn lock_lobby_source(
        &self,
        player_id: &PlayerId,
        source_lifecycle: Option<Arc<ClientLifecycle>>,
    ) -> Option<LobbyCommandSource> {
        let lifecycle =
            source_lifecycle.or_else(|| self.connection_manager.client_lifecycle(player_id))?;
        let close = self
            .connection_manager
            .close_signal_for_lifecycle(player_id, &lifecycle)?;
        let lifecycle_guard = close
            .while_open(Arc::clone(&lifecycle).lock_owned())
            .await?;
        if lifecycle.player_id() != *player_id
            || !self
                .connection_manager
                .lifecycle_matches(player_id, &lifecycle)
        {
            return None;
        }
        Some(LobbyCommandSource {
            close,
            _lifecycle_guard: lifecycle_guard,
        })
    }

    /// Handle a player ready-state toggle under process-local room coordination.
    ///
    /// Readiness can be toggled at any time while the room is open; it no longer
    /// starts the game. The server broadcasts the updated lobby snapshot (with
    /// `all_ready`); finalization is driven by an explicit `StartGame`
    /// ([`Self::handle_start_game`]).
    pub async fn handle_player_ready(&self, player_id: &PlayerId) {
        self.handle_player_ready_from_lifecycle(player_id, None)
            .await;
    }

    pub(super) async fn handle_player_ready_from_lifecycle(
        &self,
        player_id: &PlayerId,
        source_lifecycle: Option<Arc<ClientLifecycle>>,
    ) {
        let Some(source) = self.lock_lobby_source(player_id, source_lifecycle).await else {
            return;
        };
        let room_id = self.connection_manager.get_client_room(player_id);
        let room_coordinator = Arc::clone(&self.room_coordinator);
        let connection_manager = Arc::clone(&self.connection_manager);
        let message_coordinator = Arc::clone(&self.message_coordinator);
        let player_id = *player_id;
        // Use the same owned-task scheduling as the other room commands.
        let completion = tokio::spawn(async move {
            if source.close.requested_reason().is_some() {
                return;
            }
            let player_id = &player_id;
            let Some(room_id) = room_id else {
                let _ = super::messaging::send_error_to_player(
                    connection_manager.as_ref(),
                    message_coordinator.as_ref(),
                    player_id,
                    "Not in a room".to_string(),
                    Some(ErrorCode::NotInRoom),
                )
                .await;
                return;
            };
            let Some(room_guard) = source
                .admit_room(message_coordinator.as_ref(), &room_id)
                .await
            else {
                return;
            };
            if let Err(error) = room_coordinator
                .handle_player_ready_under_room_gate(&room_id, player_id, room_guard)
                .await
            {
                // The variant's own `error_code()` is the single source of truth for
                // classification (compiler-checked, unit-tested). Only a `Finalized`
                // room is a business rejection (`INVALID_ROOM_STATE`); every other
                // case is an infrastructure failure that MUST NOT masquerade as a
                // room-state error, or clients mishandle transient internal faults
                // as terminal business state. The message is presentation only.
                let error_code = error.error_code();
                let message = match &error {
                    PlayerReadyError::Finalized => {
                        "Cannot change ready status: the game has already started."
                    }
                    PlayerReadyError::RoomNotFound => "Room not found",
                    PlayerReadyError::Internal(_) => "Failed to update ready state",
                }
                .to_string();
                tracing::debug!(
                    %player_id,
                    %error_code,
                    "Player ready toggle rejected: {error}"
                );
                let _ = super::messaging::send_error_to_player(
                    connection_manager.as_ref(),
                    message_coordinator.as_ref(),
                    player_id,
                    message,
                    Some(error_code),
                )
                .await;
            }
        });
        if let Err(error) = completion.await {
            tracing::error!(%player_id, %error, "Ready toggle transaction failed");
        }
    }

    /// Handle an explicit `StartGame`: finalize the lobby with its current
    /// members when every current player is ready and the sender is authorized.
    ///
    /// `max_players` is a ceiling, not a required count, so a partially-full
    /// room may start. Authorization: a designated authority may start;
    /// otherwise any member may. The coordinator commits `GameStarting`, the
    /// sticky v3 session decision, and all tailored `SessionPlan`s as one
    /// cancellation-independent exact-membership transaction.
    pub async fn handle_start_game(&self, player_id: &PlayerId) {
        self.handle_start_game_from_lifecycle(player_id, None).await;
    }

    /// Pin the requesting connection and report the coordinator's current-roster decision.
    pub(super) async fn handle_start_game_from_lifecycle(
        &self,
        player_id: &PlayerId,
        source_lifecycle: Option<Arc<ClientLifecycle>>,
    ) {
        let Some(source) = self.lock_lobby_source(player_id, source_lifecycle).await else {
            return;
        };
        let room_id = self.connection_manager.get_client_room(player_id);
        let publication = room_id.map(|id| (id, self.start_game_publication_builder(id)));
        let room_coordinator = Arc::clone(&self.room_coordinator);
        let connection_manager = Arc::clone(&self.connection_manager);
        let message_coordinator = Arc::clone(&self.message_coordinator);
        let player_id = *player_id;
        // Use the same owned-task scheduling as the other room commands.
        let completion = tokio::spawn(async move {
            if source.close.requested_reason().is_some() {
                return;
            }
            let player_id = &player_id;
            let Some((room_id, publication)) = publication else {
                let _ = super::messaging::send_error_to_player(
                    connection_manager.as_ref(),
                    message_coordinator.as_ref(),
                    player_id,
                    "Not in a room".to_string(),
                    Some(ErrorCode::NotInRoom),
                )
                .await;
                return;
            };
            let Some(room_guard) = source
                .admit_room(message_coordinator.as_ref(), &room_id)
                .await
            else {
                return;
            };
            match room_coordinator
                .start_game_under_room_gate(&room_id, player_id, publication, room_guard)
                .await
            {
                Ok(StartGameOutcome::Started(_)) => {}
                Ok(rejection) => {
                    let (message, error_code) = match rejection {
                        StartGameOutcome::NotReady => (
                            "Cannot start the game: every player must be ready first.".to_string(),
                            ErrorCode::GameStartNotReady,
                        ),
                        StartGameOutcome::Forbidden => (
                            "Only a current member may start; when authority is set, that player must start.".to_string(),
                            ErrorCode::GameStartForbidden,
                        ),
                        StartGameOutcome::AlreadyStarted => (
                            "The game has already started.".to_string(),
                            ErrorCode::InvalidRoomState,
                        ),
                        // Unreachable: the success arm is handled above.
                        StartGameOutcome::Started(_) => return,
                    };
                    let _ = super::messaging::send_error_to_player(
                        connection_manager.as_ref(),
                        message_coordinator.as_ref(),
                        player_id,
                        message,
                        Some(error_code),
                    )
                    .await;
                }
                Err(e) => {
                    // The only shipped path into this arm is the StartGame CAS
                    // seeing a membership snapshot change (`SnapshotChanged`) —
                    // i.e. a `pending_durable_player_detaches` ghost row, which
                    // requires a storage-backend removal error and is therefore
                    // structurally unreachable with the shipped in-memory
                    // storage (the detach backlog repairs it for embedders). A
                    // coded typed rejection is deferred until an embedder can
                    // actually reach it (#396 session-193 sweep).
                    tracing::debug!("Player {:?} attempted to start the game: {}", player_id, e);
                    let _ = super::messaging::send_error_to_player(
                        connection_manager.as_ref(),
                        message_coordinator.as_ref(),
                        player_id,
                        "Failed to start the game".to_string(),
                        Some(ErrorCode::InternalError),
                    )
                    .await;
                }
            }
        });
        if let Err(error) = completion.await {
            tracing::error!(%player_id, %error, "Game start transaction failed");
        }
    }
}
