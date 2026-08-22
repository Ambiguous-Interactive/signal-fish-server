use super::EnhancedGameServer;
use crate::protocol::{PlayerId, ServerMessage};
use std::sync::Arc;

impl EnhancedGameServer {
    /// Handle joining a room as spectator, surfacing validation errors back to the client.
    pub async fn handle_join_as_spectator(
        &self,
        player_id: &PlayerId,
        game_name: String,
        room_code: String,
        spectator_name: String,
    ) {
        self.handle_join_as_spectator_operation(
            player_id,
            None,
            game_name,
            room_code,
            spectator_name,
        )
        .await;
    }

    pub(super) async fn handle_join_as_spectator_operation(
        &self,
        player_id: &PlayerId,
        operation_id: Option<crate::protocol::RoomOperationId>,
        game_name: String,
        room_code: String,
        spectator_name: String,
    ) {
        if let Err(err) = self
            .spectator_service
            .join_operation(
                player_id,
                operation_id,
                game_name,
                room_code,
                spectator_name,
            )
            .await
        {
            // The terminal response to a `JoinAsSpectator`, mirroring
            // `RoomJoinFailed` for `JoinRoom`: a client that awaits
            // `SpectatorJoined | SpectatorJoinFailed` — the pair `docs/protocol.md`
            // and the AsyncAPI document define — must never have to time out
            // instead. The reason and code are the same values the generic
            // `Error` frame carried.
            let _ = self
                .message_coordinator
                .send_to_player(
                    player_id,
                    Arc::new(
                        (ServerMessage::SpectatorJoinFailed {
                            reason: err.message,
                            error_code: err.code,
                        })
                        .correlate_room_operation(operation_id),
                    ),
                )
                .await;
        }
    }

    /// Handle leaving spectator mode, falling back to the standard error path.
    pub async fn handle_leave_spectator(&self, player_id: &PlayerId) {
        self.handle_leave_spectator_operation(player_id, None).await;
    }

    pub(super) async fn handle_leave_spectator_operation(
        &self,
        player_id: &PlayerId,
        operation_id: Option<crate::protocol::RoomOperationId>,
    ) {
        let outcome = match operation_id {
            Some(operation_id) => {
                self.spectator_service
                    .leave_operation(player_id, operation_id)
                    .await
            }
            None => self.spectator_service.leave(player_id).await,
        };
        match outcome {
            Ok(()) => tracing::info!(%player_id, "Spectator left room"),
            Err(err) => match operation_id {
                Some(operation_id) => {
                    let _ = self
                        .message_coordinator
                        .send_to_player(
                            player_id,
                            Arc::new(ServerMessage::room_operation_failed(
                                operation_id,
                                err.message,
                                err.code,
                            )),
                        )
                        .await;
                }
                None => {
                    let _ = self
                        .send_error_to_player(player_id, err.message, err.code)
                        .await;
                }
            },
        }
    }
}
