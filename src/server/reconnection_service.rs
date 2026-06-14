use crate::protocol::{ErrorCode, PlayerId, PlayerInfo, ReconnectedPayload, RoomId, ServerMessage};
use crate::reconnection::{ClaimedReconnection, DisconnectedPlayer, ReconnectionManager};
use std::sync::Arc;

use super::EnhancedGameServer;

struct ReconnectionClaimGuard {
    manager: Arc<ReconnectionManager>,
    claim: Option<ClaimedReconnection>,
}

impl ReconnectionClaimGuard {
    fn new(manager: Arc<ReconnectionManager>, claim: ClaimedReconnection) -> Self {
        Self {
            manager,
            claim: Some(claim),
        }
    }

    fn disconnected(&self) -> Option<DisconnectedPlayer> {
        self.claim.as_ref().map(|claim| claim.disconnected.clone())
    }

    async fn release(mut self) -> bool {
        let Some(claim) = self.claim.take() else {
            return false;
        };
        self.manager.release_reconnection_claim(&claim).await
    }

    async fn complete(mut self) -> bool {
        let Some(claim) = self.claim.take() else {
            return false;
        };
        self.manager.complete_claimed_reconnection(&claim).await
    }
}

impl Drop for ReconnectionClaimGuard {
    fn drop(&mut self) {
        let Some(claim) = self.claim.take() else {
            return;
        };
        let manager = Arc::clone(&self.manager);

        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                player_id = %claim.disconnected.player_id,
                room_id = %claim.disconnected.room_id,
                "Reconnection claim guard dropped outside a Tokio runtime; claim release could not be scheduled"
            );
            return;
        };

        drop(handle.spawn(async move {
            let released = manager.release_reconnection_claim(&claim).await;
            tracing::warn!(
                player_id = %claim.disconnected.player_id,
                room_id = %claim.disconnected.room_id,
                %released,
                "Reconnection claim released by dropped restore guard"
            );
        }));
    }
}

impl EnhancedGameServer {
    pub(crate) async fn register_disconnection_for_reconnect(
        &self,
        player_id: &PlayerId,
        room_id: RoomId,
        was_authority: bool,
    ) {
        let Some(reconnection_manager) = &self.reconnection_manager else {
            return;
        };

        let player_info = match self.database.get_room_by_id(&room_id).await {
            Ok(Some(room)) => room.players.get(player_id).cloned(),
            Ok(None) => None,
            Err(err) => {
                tracing::warn!(
                    %player_id,
                    %room_id,
                    error = %err,
                    "Failed to snapshot player info for reconnection"
                );
                None
            }
        };

        let token = reconnection_manager
            .register_disconnection(*player_id, room_id, was_authority, player_info)
            .await;

        tracing::info!(
            %player_id,
            %room_id,
            %was_authority,
            reconnection_token = %token[..8].to_string(),
            "Player disconnection registered for reconnection"
        );
    }

    async fn reject_claimed_reconnect(
        &self,
        current_player_id: &PlayerId,
        claim_guard: ReconnectionClaimGuard,
        restored_membership: bool,
        restored_authority: bool,
        reason: &str,
        error_code: ErrorCode,
    ) -> bool {
        let Some(disconnected) = claim_guard.disconnected() else {
            tracing::warn!(%reason, "Reconnection rejection had no active claim to release");
            return false;
        };
        if restored_membership {
            if let Err(err) = self
                .database
                .remove_player_from_room(&disconnected.room_id, &disconnected.player_id)
                .await
            {
                tracing::warn!(
                    player_id = %disconnected.player_id,
                    room_id = %disconnected.room_id,
                    error = %err,
                    "Failed to roll back restored room membership after reconnect failure"
                );
            }
        } else if restored_authority {
            if let Err(err) = self
                .database
                .update_room_authority(&disconnected.room_id, None)
                .await
            {
                tracing::warn!(
                    player_id = %disconnected.player_id,
                    room_id = %disconnected.room_id,
                    error = %err,
                    "Failed to roll back restored authority after reconnect failure"
                );
            }
        }

        let released = claim_guard.release().await;
        tracing::warn!(
            player_id = %disconnected.player_id,
            room_id = %disconnected.room_id,
            %released,
            %reason,
            "Reconnection claim released after restore failure"
        );

        let _ = self
            .message_coordinator
            .send_to_player(
                current_player_id,
                Arc::new(ServerMessage::ReconnectionFailed {
                    reason: reason.to_string(),
                    error_code,
                }),
            )
            .await;
        false
    }

    /// Handle player reconnection
    pub async fn handle_reconnect(
        &self,
        current_player_id: &PlayerId,
        reconnect_player_id: &PlayerId,
        room_id: &RoomId,
        auth_token: &str,
    ) -> bool {
        // Check if reconnection is enabled
        let Some(reconnection_manager) = &self.reconnection_manager else {
            tracing::warn!("Reconnection attempt but reconnection is disabled");
            let _ = self
                .message_coordinator
                .send_to_player(
                    current_player_id,
                    Arc::new(ServerMessage::ReconnectionFailed {
                        reason: "Reconnection is not enabled".to_string(),
                        error_code: ErrorCode::ReconnectionFailed,
                    }),
                )
                .await;
            return false;
        };

        // Check if player is already connected
        if self.connection_manager.has_client(reconnect_player_id) {
            let _ = self
                .message_coordinator
                .send_to_player(
                    current_player_id,
                    Arc::new(ServerMessage::ReconnectionFailed {
                        reason: "Player is already connected".to_string(),
                        error_code: ErrorCode::PlayerAlreadyConnected,
                    }),
                )
                .await;
            return false;
        }

        if self.get_client_room(current_player_id).await.is_some()
            || self.spectator_service.is_spectating(current_player_id)
        {
            let _ = self
                .message_coordinator
                .send_to_player(
                    current_player_id,
                    Arc::new(ServerMessage::ReconnectionFailed {
                        reason: "Reconnect must be attempted from a fresh connection".to_string(),
                        error_code: ErrorCode::ReconnectionFailed,
                    }),
                )
                .await;
            return false;
        }

        // Validate and atomically reserve the reconnection token before any
        // room or connection side effects. The record is only removed after
        // the restore succeeds; post-claim failures release it for retry.
        let claim = match reconnection_manager
            .claim_reconnection(current_player_id, reconnect_player_id, room_id, auth_token)
            .await
        {
            Ok(claim) => claim,
            Err(reason) => {
                tracing::warn!(
                    %reconnect_player_id,
                    %room_id,
                    %reason,
                    "Reconnection validation failed"
                );
                let error_code = if reason.contains("expired") {
                    ErrorCode::ReconnectionExpired
                } else if reason.contains("token") {
                    ErrorCode::ReconnectionTokenInvalid
                } else {
                    ErrorCode::ReconnectionFailed
                };

                let _ = self
                    .message_coordinator
                    .send_to_player(
                        current_player_id,
                        Arc::new(ServerMessage::ReconnectionFailed { reason, error_code }),
                    )
                    .await;
                return false;
            }
        };
        let claim_guard = ReconnectionClaimGuard::new(Arc::clone(reconnection_manager), claim);
        let Some(disconnected) = claim_guard.disconnected() else {
            tracing::warn!(
                %reconnect_player_id,
                %room_id,
                "Reconnection claim guard was empty immediately after claim"
            );
            return false;
        };
        let mut restored_membership = false;
        let mut restored_authority = false;

        // Defense-in-depth for unexpected concurrent ownership paths. The
        // claim above is what resolves duplicate same-token races.
        if self.connection_manager.has_client(reconnect_player_id) {
            return self
                .reject_claimed_reconnect(
                    current_player_id,
                    claim_guard,
                    restored_membership,
                    restored_authority,
                    "Player is already connected",
                    ErrorCode::PlayerAlreadyConnected,
                )
                .await;
        }

        // Get room from database
        let room = match self.database.get_room_by_id(room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        restored_membership,
                        restored_authority,
                        "Room no longer exists",
                        ErrorCode::RoomNotFound,
                    )
                    .await;
            }
            Err(e) => {
                tracing::error!("Failed to get room for reconnection: {}", e);
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        restored_membership,
                        restored_authority,
                        "Storage error",
                        ErrorCode::InternalError,
                    )
                    .await;
            }
        };

        // Get missed events
        let missed_events = reconnection_manager
            .get_missed_events(room_id, disconnected.last_sequence)
            .await;

        if !room.players.contains_key(reconnect_player_id) {
            let Some(player_info) = disconnected.player_info.clone() else {
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        restored_membership,
                        restored_authority,
                        "Player room membership could not be restored",
                        ErrorCode::ReconnectionFailed,
                    )
                    .await;
            };

            match self.database.add_player_to_room(room_id, player_info).await {
                Ok(true) => {
                    restored_membership = true;
                }
                Ok(false) => {
                    return self
                        .reject_claimed_reconnect(
                            current_player_id,
                            claim_guard,
                            restored_membership,
                            restored_authority,
                            "Room is full",
                            ErrorCode::RoomFull,
                        )
                        .await;
                }
                Err(err) => {
                    tracing::error!(
                        %reconnect_player_id,
                        %room_id,
                        error = %err,
                        "Failed to restore player room membership on reconnection"
                    );
                    return self
                        .reject_claimed_reconnect(
                            current_player_id,
                            claim_guard,
                            restored_membership,
                            restored_authority,
                            "Storage error",
                            ErrorCode::InternalError,
                        )
                        .await;
                }
            }
        }

        if disconnected.was_authority && room.supports_authority && room.authority_player.is_none()
        {
            match self
                .database
                .update_room_authority(room_id, Some(*reconnect_player_id))
                .await
            {
                Ok(true) => {
                    restored_authority = true;
                }
                Ok(false) => {}
                Err(err) => {
                    tracing::warn!(
                        %reconnect_player_id,
                        %room_id,
                        error = %err,
                        "Failed to restore authority on reconnection"
                    );
                }
            }
        }

        let room = match self.database.get_room_by_id(room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        restored_membership,
                        restored_authority,
                        "Room no longer exists",
                        ErrorCode::RoomNotFound,
                    )
                    .await;
            }
            Err(err) => {
                tracing::error!("Failed to get restored room for reconnection: {}", err);
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        restored_membership,
                        restored_authority,
                        "Storage error",
                        ErrorCode::InternalError,
                    )
                    .await;
            }
        };

        // Update client connection to use the reconnecting player's original id.
        let Some(reassigned_sender) = self.connection_manager.reassign_connection(
            current_player_id,
            reconnect_player_id,
            *room_id,
        ) else {
            return self
                .reject_claimed_reconnect(
                    current_player_id,
                    claim_guard,
                    restored_membership,
                    restored_authority,
                    "Current connection no longer exists",
                    ErrorCode::ReconnectionFailed,
                )
                .await;
        };

        // Complete once the fallible connection reassignment succeeds. The
        // remaining coordinator/message operations are best-effort updates.
        if !claim_guard.complete().await {
            tracing::warn!(
                %reconnect_player_id,
                %room_id,
                "Reconnection succeeded but pending claim was already released"
            );
        }

        let _ = self
            .message_coordinator
            .unregister_local_client(current_player_id)
            .await;
        if let Err(err) = self
            .message_coordinator
            .register_local_client(*reconnect_player_id, Some(*room_id), reassigned_sender)
            .await
        {
            tracing::warn!(
                %reconnect_player_id,
                %room_id,
                error = %err,
                "Failed to register reassigned connection with coordinator"
            );
        }

        // Update database last_seen
        if let Err(e) = self
            .database
            .update_player_last_seen(reconnect_player_id)
            .await
        {
            tracing::warn!(
                %reconnect_player_id,
                "Failed to update last_seen on reconnection: {}",
                e
            );
        }

        // Prepare room state. The live ready set is held by the coordinator (the
        // room record only syncs at finalize), so read it from there to report an
        // accurate ready set to a reconnector rejoining an in-progress lobby, and
        // reflect it on each player's `is_ready`.
        let ready_players = self.room_coordinator.current_ready_players(room_id).await;
        let mut current_players: Vec<PlayerInfo> = room.players.values().cloned().collect();
        for player in current_players.iter_mut() {
            player.is_ready = ready_players.contains(&player.id);
        }
        let is_authority = room.authority_player == Some(*reconnect_player_id);

        // Send reconnected message
        let _ = self
            .message_coordinator
            .send_to_player(
                reconnect_player_id,
                Arc::new(ServerMessage::Reconnected(Box::new(ReconnectedPayload {
                    room_id: *room_id,
                    room_code: room.code.clone(),
                    player_id: *reconnect_player_id,
                    game_name: room.game_name.clone(),
                    max_players: room.max_players,
                    supports_authority: room.supports_authority,
                    current_players: current_players.clone(),
                    is_authority,
                    lobby_state: room.lobby_state.clone(),
                    ready_players: ready_players.clone(),
                    relay_type: room.relay_type.clone(),
                    current_spectators: room.get_spectators(),
                    // v3 ICE pre-gather (PLAN §P4 deferred refinement): empty —
                    // and skipped on the wire — unless this reconnector passes
                    // the pre-gather gate (its original credentials may have
                    // expired while it was away), so v2 bytes are untouched. A
                    // reconnect into a Finalized room gets fresh ICE from the
                    // late-join SessionPlan below instead (never both).
                    ice_servers: self.pregather_ice_servers(&room, reconnect_player_id),
                    missed_events,
                }))),
            )
            .await;

        // Notify other players
        let notification = Arc::new(ServerMessage::PlayerReconnected {
            player_id: *reconnect_player_id,
        });

        for other_player_id in room.players.keys() {
            if other_player_id != reconnect_player_id {
                let _ = self
                    .message_coordinator
                    .send_to_player(other_player_id, Arc::clone(&notification))
                    .await;
            }
        }

        // Re-entry into an active session: if the room is finalized with a
        // stored non-relay plan, the reconnector receives a fresh tailored
        // `SessionPlan` (fresh ICE — its original TURN credentials may have
        // expired) and existing members receive the `NewPeer` delta per the
        // stored topology when the transport is WebRTC. A non-finalized room
        // or the relay floor emits nothing (PLAN §P3).
        self.handle_active_session_late_join(&room, reconnect_player_id, &current_players)
            .await;

        self.metrics.increment_players_joined();
        tracing::info!(
            %reconnect_player_id,
            %room_id,
            room_code = %room.code,
            "Player reconnected successfully"
        );
        true
    }
}
