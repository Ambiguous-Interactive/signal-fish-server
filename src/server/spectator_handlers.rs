use super::EnhancedGameServer;
use crate::protocol::PlayerId;
use std::sync::Arc;

use super::connection_manager::ClientLifecycle;

impl EnhancedGameServer {
    /// Handle joining a room as spectator, surfacing validation errors back to the client.
    pub async fn handle_join_as_spectator(
        &self,
        player_id: &PlayerId,
        game_name: String,
        room_code: String,
        spectator_name: String,
        password: Option<String>,
    ) {
        self.handle_join_as_spectator_operation(
            player_id,
            None,
            game_name,
            room_code,
            spectator_name,
            password,
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
        password: Option<String>,
    ) {
        self.handle_join_as_spectator_operation_from_lifecycle(
            player_id,
            operation_id,
            game_name,
            room_code,
            spectator_name,
            password,
            None,
        )
        .await;
    }

    pub(super) async fn handle_join_as_spectator_operation_from_lifecycle(
        &self,
        player_id: &PlayerId,
        operation_id: Option<crate::protocol::RoomOperationId>,
        game_name: String,
        room_code: String,
        spectator_name: String,
        password: Option<String>,
        source_lifecycle: Option<Arc<ClientLifecycle>>,
    ) {
        let lifecycle =
            source_lifecycle.or_else(|| self.connection_manager.client_lifecycle(player_id));
        let initial_guard = if let Some(lifecycle) = &lifecycle {
            let guard = lifecycle.lock().await;
            if lifecycle.player_id() != *player_id
                || !self
                    .connection_manager
                    .lifecycle_matches(player_id, lifecycle)
            {
                return;
            }
            Some(guard)
        } else {
            None
        };
        // Shutdown-drain parity with the join path: only a socket upgraded
        // before the drain flipped can still deliver `JoinAsSpectator` inside
        // the grace window. Admitting it would publish a role the drain
        // teardown detaches at unregister, and the socket closes 4000 at the
        // deadline anyway — refuse before any admission side effect.
        if self.is_draining() {
            let _ = self
                .send_spectator_join_failure_to_player(
                    player_id,
                    "Server is draining for shutdown".to_string(),
                    Some(crate::protocol::ErrorCode::ServerDraining),
                    operation_id,
                )
                .await;
            return;
        }
        drop(initial_guard);
        if let Err(err) = self
            .spectator_service
            .join_operation_from_lifecycle(
                player_id,
                operation_id,
                game_name,
                room_code,
                spectator_name,
                password,
                lifecycle.clone(),
            )
            .await
        {
            let _reply_guard = if let Some(lifecycle) = &lifecycle {
                let guard = lifecycle.lock().await;
                if lifecycle.player_id() != *player_id
                    || !self
                        .connection_manager
                        .lifecycle_matches(player_id, lifecycle)
                {
                    return;
                }
                Some(guard)
            } else {
                None
            };
            // The terminal response to a `JoinAsSpectator`, mirroring
            // `RoomJoinFailed` for `JoinRoom`: a client that awaits
            // `SpectatorJoined | SpectatorJoinFailed` — the pair `docs/protocol.md`
            // and the AsyncAPI document define — must never have to time out
            // instead. The reason and code are the same values the generic
            // `Error` frame carried.
            let _ = self
                .send_spectator_join_failure_to_player(
                    player_id,
                    err.message,
                    err.code,
                    operation_id,
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
        self.handle_leave_spectator_operation_from_lifecycle(player_id, operation_id, None)
            .await;
    }

    pub(super) async fn handle_leave_spectator_operation_from_lifecycle(
        &self,
        player_id: &PlayerId,
        operation_id: Option<crate::protocol::RoomOperationId>,
        source_lifecycle: Option<Arc<ClientLifecycle>>,
    ) {
        let lifecycle =
            source_lifecycle.or_else(|| self.connection_manager.client_lifecycle(player_id));
        if let Some(lifecycle) = &lifecycle {
            let guard = lifecycle.lock().await;
            if lifecycle.player_id() != *player_id
                || !self
                    .connection_manager
                    .lifecycle_matches(player_id, lifecycle)
            {
                return;
            }
            drop(guard);
        }
        let outcome = match operation_id {
            Some(operation_id) => {
                self.spectator_service
                    .leave_operation_from_lifecycle(player_id, operation_id, lifecycle.clone())
                    .await
            }
            None => {
                self.spectator_service
                    .leave_from_lifecycle(player_id, lifecycle.clone())
                    .await
            }
        };
        let _guard = if let Some(lifecycle) = &lifecycle {
            let guard = lifecycle.lock().await;
            if lifecycle.player_id() != *player_id
                || !self
                    .connection_manager
                    .lifecycle_matches(player_id, lifecycle)
            {
                return;
            }
            Some(guard)
        } else {
            None
        };
        match outcome {
            Ok(()) => tracing::info!(%player_id, "Spectator left room"),
            Err(err) => match operation_id {
                Some(operation_id) => {
                    let _ = self
                        .send_room_operation_failure_to_player(
                            player_id,
                            operation_id,
                            err.message,
                            err.code,
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
