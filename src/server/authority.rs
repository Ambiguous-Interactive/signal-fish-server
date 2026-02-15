use crate::protocol::{ErrorCode, PlayerId, ServerMessage};
use std::sync::Arc;

use super::EnhancedGameServer;

impl EnhancedGameServer {
    /// Handle authority request with distributed coordination.
    pub async fn handle_authority_request(&self, player_id: &PlayerId, become_authority: bool) {
        tracing::info!(%player_id, %become_authority, "Server handling authority request");

        let Some(room_id) = self.get_client_room(player_id).await else {
            tracing::warn!(%player_id, "Player not in room for authority request");
            if let Err(e) = self
                .message_coordinator
                .send_to_player(
                    player_id,
                    Arc::new(ServerMessage::AuthorityResponse {
                        granted: false,
                        reason: Some("Not in a room".to_string()),
                        error_code: Some(ErrorCode::NotInRoom),
                    }),
                )
                .await
            {
                tracing::error!(%player_id, "Failed to send via coordinator: {}", e);
            }
            return;
        };

        tracing::info!(
            %player_id,
            %room_id,
            %become_authority,
            "Processing authority request with coordinator"
        );

        match self
            .room_coordinator
            .handle_authority_request(&room_id, player_id, become_authority)
            .await
        {
            Ok((granted, reason)) => {
                tracing::info!(%player_id, %granted, ?reason, "Authority request processed");

                if let Err(e) = self
                    .message_coordinator
                    .send_to_player(
                        player_id,
                        Arc::new(ServerMessage::AuthorityResponse {
                            granted,
                            reason: reason.clone(),
                            error_code: if granted {
                                None
                            } else {
                                Some(ErrorCode::AuthorityConflict)
                            },
                        }),
                    )
                    .await
                {
                    tracing::error!(
                        %player_id,
                        "Failed to send authority response via coordinator: {}",
                        e
                    );
                }
            }
            Err(e) => {
                tracing::error!("Authority request failed: {}", e);

                if let Err(e) = self
                    .message_coordinator
                    .send_to_player(
                        player_id,
                        Arc::new(ServerMessage::AuthorityResponse {
                            granted: false,
                            reason: Some("Internal error".to_string()),
                            error_code: Some(ErrorCode::InternalError),
                        }),
                    )
                    .await
                {
                    tracing::error!(
                        %player_id,
                        "Failed to send error response via coordinator: {}",
                        e
                    );
                }
            }
        }
    }
}
