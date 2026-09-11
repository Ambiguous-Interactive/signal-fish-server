use crate::protocol::{ErrorCode, PlayerId, ServerMessage};
use std::sync::Arc;

use super::EnhancedGameServer;

impl EnhancedGameServer {
    /// Handle an authority request under process-local room coordination.
    pub async fn handle_authority_request(&self, player_id: &PlayerId, become_authority: bool) {
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

        tracing::info!(%player_id, %become_authority, "Server handling authority request");

        let Some(room_id) = self.get_client_room(player_id).await else {
            tracing::warn!(%player_id, "Player not in room for authority request");
            // The refusal is a polite per-frame reply: it charges the
            // per-connection error-reply budget (issue #518), so a
            // roomless RequestAuthority flood cannot buy unbounded denials.
            if !self.connection_manager.charge_error_reply(player_id).await {
                return;
            }
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
            Ok(outcome) => {
                tracing::info!(
                    %player_id,
                    granted = outcome.granted(),
                    denial = ?outcome.denial(),
                    "Authority request processed"
                );
                // A denial is a 1:1 refusal reply to an unbudgeted request
                // kind, so it charges the per-connection error-reply budget
                // (issue #518). The coordinator's FIFO job may already have
                // enqueued this one response; the charge still pins the
                // semantic `4006` close at the first exhausted observation,
                // and the pinned close stops later frames from producing
                // further denials.
                if outcome.denial().is_some() {
                    self.connection_manager.charge_error_reply(player_id).await;
                }
            }
            Err(e) => {
                tracing::error!("Authority request failed: {}", e);

                // Charged like the direct refusal above (issue #518).
                if !self.connection_manager.charge_error_reply(player_id).await {
                    return;
                }
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
