use crate::protocol::{ErrorCode, PlayerId, ServerMessage};
use std::sync::Arc;

use super::EnhancedGameServer;

impl EnhancedGameServer {
    /// Handle player ready with distributed coordination.
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

        if let Err(e) = self
            .room_coordinator
            .handle_player_ready(&room_id, player_id, self.client_app_id(player_id))
            .await
        {
            tracing::debug!(
                "Player {:?} attempted to change ready status: {}",
                player_id,
                e
            );
            let error_message = if e.to_string().contains("room may not be in lobby state") {
                "Cannot change ready status. Room must be in lobby state (full with all players joined)."
                    .to_string()
            } else {
                "Failed to update ready state".to_string()
            };
            let _ = self
                .message_coordinator
                .send_to_player(
                    player_id,
                    Arc::new(ServerMessage::Error {
                        message: error_message,
                        error_code: Some(ErrorCode::InvalidRoomState),
                    }),
                )
                .await;
        }
    }
}
