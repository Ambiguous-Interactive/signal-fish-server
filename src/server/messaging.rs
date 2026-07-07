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

    /// Best-effort pre-close farewell: never waits on recipient queue
    /// capacity and never escalates (a trait-level contract of
    /// [`try_send_to_player`](crate::coordination::MessageCoordinator::try_send_to_player),
    /// which deliberately has no default implementation).
    ///
    /// For connections the caller is about to terminate (reaper eviction and
    /// similar lifecycle closes): the close itself carries the authoritative
    /// reason, so a full queue skips this advisory frame instead of stalling
    /// the teardown or reclassifying the close as a slow-consumer disconnect.
    /// Returns whether the frame was enqueued.
    pub async fn send_farewell_to_player(
        &self,
        player_id: &PlayerId,
        message: String,
        error_code: Option<ErrorCode>,
    ) -> bool {
        self.message_coordinator
            .try_send_to_player(
                player_id,
                Arc::new(ServerMessage::Error {
                    message,
                    error_code,
                }),
            )
            .await
            .unwrap_or(false)
    }

    pub async fn send_farewell_to_player_if(
        &self,
        player_id: &PlayerId,
        message: String,
        error_code: Option<ErrorCode>,
        should_send: &(dyn Fn() -> bool + Send + Sync),
    ) -> bool {
        self.message_coordinator
            .try_send_to_player_if(
                player_id,
                Arc::new(ServerMessage::Error {
                    message,
                    error_code,
                }),
                should_send,
            )
            .await
            .unwrap_or(false)
    }
}
