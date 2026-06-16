use crate::coordination::{PlayerReadyError, StartGameOutcome};
use crate::protocol::{ErrorCode, PlayerId, ServerMessage};
use std::sync::Arc;

use super::EnhancedGameServer;

impl EnhancedGameServer {
    /// Handle a player ready-state toggle with distributed coordination.
    ///
    /// Readiness can be toggled at any time while the room is open; it no longer
    /// starts the game. The server broadcasts the updated lobby snapshot (with
    /// `all_ready`); finalization is driven by an explicit `StartGame`
    /// ([`Self::handle_start_game`]).
    pub async fn handle_player_ready(&self, player_id: &PlayerId) {
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
    /// otherwise any member may. On success the coordinator has already
    /// broadcast `GameStarting`, so we only emit the per-recipient v3
    /// `SessionPlan` (gated to v3 clients) here.
    pub async fn handle_start_game(&self, player_id: &PlayerId) {
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

        match self
            .room_coordinator
            .handle_start_game(&room_id, player_id)
            .await
        {
            Ok(StartGameOutcome::Started(finalized)) => {
                self.emit_session_plan(&room_id, &finalized).await;
            }
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
