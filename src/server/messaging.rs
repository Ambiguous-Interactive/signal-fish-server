use super::EnhancedGameServer;
use crate::protocol::{ErrorCode, PlayerId, ServerMessage};
use std::sync::Arc;

impl EnhancedGameServer {
    /// Send an error message to a specific player, tracking back-pressure metrics.
    pub async fn send_error_to_player(
        &self,
        player_id: &PlayerId,
        message: String,
        error_code: Option<ErrorCode>,
    ) -> anyhow::Result<()> {
        self.message_coordinator
            .send_to_player(
                player_id,
                Arc::new(ServerMessage::Error {
                    message,
                    error_code,
                }),
            )
            .await
    }
}
