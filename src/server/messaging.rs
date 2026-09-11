use super::EnhancedGameServer;
use crate::protocol::{ErrorCode, PlayerId, ServerMessage};
use std::sync::Arc;

impl EnhancedGameServer {
    /// Charge one polite per-frame reply against the player's per-connection
    /// error-reply budget (issue #518). Returns `false` when the budget is
    /// exhausted: the caller must NOT send the reply — the budget's own
    /// farewell has been sent and the `4006 inbound_rate_limited` close
    /// requested instead.
    pub(crate) async fn charge_error_reply(&self, player_id: &PlayerId) -> bool {
        self.connection_manager.charge_error_reply(player_id).await
    }

    /// Send an error message to a specific player, tracking back-pressure metrics.
    ///
    /// The reply charges the recipient's per-connection error-reply budget
    /// (issue #518). An exhausted budget withholds the reply: the budget's own
    /// farewell has been sent and the `4006 inbound_rate_limited` close
    /// requested instead.
    pub async fn send_error_to_player(
        &self,
        player_id: &PlayerId,
        message: String,
        error_code: Option<ErrorCode>,
    ) -> anyhow::Result<()> {
        if !self.connection_manager.charge_error_reply(player_id).await {
            return Ok(());
        }
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

    /// Send the `RoomJoinFailed` refusal for one join frame.
    ///
    /// Charged like every polite per-frame reply (issue #518): an exhausted
    /// budget withholds the refusal and closes with `4006` instead.
    pub(crate) async fn send_join_failure_to_player(
        &self,
        player_id: &PlayerId,
        reason: String,
        error_code: Option<ErrorCode>,
        operation_id: Option<crate::protocol::RoomOperationId>,
    ) -> anyhow::Result<()> {
        if !self.connection_manager.charge_error_reply(player_id).await {
            return Ok(());
        }
        self.message_coordinator
            .send_to_player(
                player_id,
                Arc::new(
                    (ServerMessage::RoomJoinFailed { reason, error_code })
                        .correlate_room_operation(operation_id),
                ),
            )
            .await
    }

    /// Send the `SpectatorJoinFailed` refusal for one spectator-join frame.
    ///
    /// Charged like every polite per-frame reply (issue #518).
    pub(crate) async fn send_spectator_join_failure_to_player(
        &self,
        player_id: &PlayerId,
        reason: String,
        error_code: Option<ErrorCode>,
        operation_id: Option<crate::protocol::RoomOperationId>,
    ) -> anyhow::Result<()> {
        if !self.connection_manager.charge_error_reply(player_id).await {
            return Ok(());
        }
        self.message_coordinator
            .send_to_player(
                player_id,
                Arc::new(
                    (ServerMessage::SpectatorJoinFailed { reason, error_code })
                        .correlate_room_operation(operation_id),
                ),
            )
            .await
    }

    /// Send the `ReconnectionFailed` refusal for one reconnect frame.
    ///
    /// Charged like every polite per-frame reply (issue #518).
    pub(crate) async fn send_reconnection_failure_to_player(
        &self,
        player_id: &PlayerId,
        reason: String,
        error_code: ErrorCode,
        operation_id: Option<crate::protocol::RoomOperationId>,
    ) -> anyhow::Result<()> {
        if !self.connection_manager.charge_error_reply(player_id).await {
            return Ok(());
        }
        self.message_coordinator
            .send_to_player(
                player_id,
                Arc::new(
                    (ServerMessage::ReconnectionFailed { reason, error_code })
                        .correlate_room_operation(operation_id),
                ),
            )
            .await
    }

    /// Send the correlated terminal failure envelope for one room-operation
    /// frame.
    ///
    /// Charged like every polite per-frame reply (issue #518): refused-
    /// operation spam must not buy unbounded uncharged envelopes.
    pub(crate) async fn send_room_operation_failure_to_player(
        &self,
        player_id: &PlayerId,
        operation_id: crate::protocol::RoomOperationId,
        reason: impl Into<String>,
        error_code: Option<ErrorCode>,
    ) -> anyhow::Result<()> {
        if !self.connection_manager.charge_error_reply(player_id).await {
            return Ok(());
        }
        self.message_coordinator
            .send_to_player(
                player_id,
                Arc::new(ServerMessage::room_operation_failed(
                    operation_id,
                    reason,
                    error_code,
                )),
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
