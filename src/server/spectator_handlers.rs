use super::EnhancedGameServer;
use crate::protocol::PlayerId;

impl EnhancedGameServer {
    /// Handle joining a room as spectator, surfacing validation errors back to the client.
    pub async fn handle_join_as_spectator(
        &self,
        player_id: &PlayerId,
        game_name: String,
        room_code: String,
        spectator_name: String,
    ) {
        if let Err(err) = self
            .spectator_service
            .join(player_id, game_name, room_code, spectator_name)
            .await
        {
            let _ = self
                .send_error_to_player(player_id, err.message, err.code)
                .await;
        }
    }

    /// Handle leaving spectator mode, falling back to the standard error path.
    pub async fn handle_leave_spectator(&self, player_id: &PlayerId) {
        match self.spectator_service.leave(player_id).await {
            Ok(()) => tracing::info!(%player_id, "Spectator left room"),
            Err(err) => {
                let _ = self
                    .send_error_to_player(player_id, err.message, err.code)
                    .await;
            }
        }
    }
}
