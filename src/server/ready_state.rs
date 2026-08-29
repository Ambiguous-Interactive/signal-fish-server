use crate::coordination::{PlayerReadyError, RoomOperationCoordinatorTrait, StartGameOutcome};
use crate::protocol::{ErrorCode, LobbyState, PlayerId, Room, ServerMessage};
use std::sync::Arc;

use super::EnhancedGameServer;

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

impl EnhancedGameServer {
    /// Handle a player ready-state toggle under process-local room coordination.
    ///
    /// Readiness can be toggled at any time while the room is open; it no longer
    /// starts the game. The server broadcasts the updated lobby snapshot (with
    /// `all_ready`); finalization is driven by an explicit `StartGame`
    /// ([`Self::handle_start_game`]).
    pub async fn handle_player_ready(&self, player_id: &PlayerId) {
        let Some(lifecycle) = self.connection_manager.client_lifecycle(player_id) else {
            return;
        };
        let _lifecycle_guard = lifecycle.lock().await;
        if lifecycle.player_id() != *player_id
            || !self
                .connection_manager
                .lifecycle_matches(player_id, &lifecycle)
        {
            return;
        }

        let Some(room_id) = self.get_client_room(player_id).await else {
            let _ = self
                .message_coordinator
                .send_to_player(
                    player_id,
                    Arc::new(ServerMessage::Error {
                        message: "Not in a room".to_string(),
                        error_code: Some(ErrorCode::NotInRoom),
                    }),
                )
                .await;
            return;
        };

        if let Err(error) = self
            .room_coordinator
            .handle_player_ready(&room_id, player_id, self.client_app_id(player_id))
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
            let _ = self
                .message_coordinator
                .send_to_player(
                    player_id,
                    Arc::new(ServerMessage::Error {
                        message,
                        error_code: Some(error_code),
                    }),
                )
                .await;
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
        let Some(lifecycle) = self.connection_manager.client_lifecycle(player_id) else {
            return;
        };
        let _lifecycle_guard = lifecycle.lock().await;
        if lifecycle.player_id() != *player_id
            || !self
                .connection_manager
                .lifecycle_matches(player_id, &lifecycle)
        {
            return;
        }

        let Some(room_id) = self.get_client_room(player_id).await else {
            let _ = self
                .message_coordinator
                .send_to_player(
                    player_id,
                    Arc::new(ServerMessage::Error {
                        message: "Not in a room".to_string(),
                        error_code: Some(ErrorCode::NotInRoom),
                    }),
                )
                .await;
            return;
        };

        let publication = self.start_game_publication_builder(room_id);
        match self
            .room_coordinator
            .handle_start_game_with_publication(&room_id, player_id, publication)
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
                        "Only the room's authority player may start the game.".to_string(),
                        ErrorCode::GameStartForbidden,
                    ),
                    StartGameOutcome::AlreadyStarted => (
                        "The game has already started.".to_string(),
                        ErrorCode::InvalidRoomState,
                    ),
                    // Unreachable: the success arm is handled above.
                    StartGameOutcome::Started(_) => return,
                };
                let _ = self
                    .message_coordinator
                    .send_to_player(
                        player_id,
                        Arc::new(ServerMessage::Error {
                            message,
                            error_code: Some(error_code),
                        }),
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
                let _ = self
                    .message_coordinator
                    .send_to_player(
                        player_id,
                        Arc::new(ServerMessage::Error {
                            message: "Failed to start the game".to_string(),
                            error_code: Some(ErrorCode::InternalError),
                        }),
                    )
                    .await;
            }
        }
    }
}
