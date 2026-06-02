use crate::protocol::{ErrorCode, PlayerId, PlayerInfo, ReconnectedPayload, RoomId, ServerMessage};
use std::sync::Arc;

use super::EnhancedGameServer;

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

        // Validate and atomically claim the reconnection token before any room
        // or connection side effects. Reconnection tokens are single-use.
        let disconnected = match reconnection_manager
            .claim_reconnection(reconnect_player_id, room_id, auth_token)
            .await
        {
            Ok(d) => d,
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

        // Defense-in-depth for unexpected concurrent ownership paths. The
        // single-use claim above is what resolves duplicate same-token races.
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

        // Get room from database
        let room = match self.database.get_room_by_id(room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                let _ = self
                    .message_coordinator
                    .send_to_player(
                        current_player_id,
                        Arc::new(ServerMessage::ReconnectionFailed {
                            reason: "Room no longer exists".to_string(),
                            error_code: ErrorCode::RoomNotFound,
                        }),
                    )
                    .await;
                return false;
            }
            Err(e) => {
                tracing::error!("Failed to get room for reconnection: {}", e);
                let _ = self
                    .message_coordinator
                    .send_to_player(
                        current_player_id,
                        Arc::new(ServerMessage::ReconnectionFailed {
                            reason: "Storage error".to_string(),
                            error_code: ErrorCode::InternalError,
                        }),
                    )
                    .await;
                return false;
            }
        };

        // Get missed events
        let missed_events = reconnection_manager
            .get_missed_events(room_id, disconnected.last_sequence)
            .await;

        if !room.players.contains_key(reconnect_player_id) {
            let Some(player_info) = disconnected.player_info.clone() else {
                let _ = self
                    .message_coordinator
                    .send_to_player(
                        current_player_id,
                        Arc::new(ServerMessage::ReconnectionFailed {
                            reason: "Player room membership could not be restored".to_string(),
                            error_code: ErrorCode::ReconnectionFailed,
                        }),
                    )
                    .await;
                return false;
            };

            match self.database.add_player_to_room(room_id, player_info).await {
                Ok(true) => {}
                Ok(false) => {
                    let _ = self
                        .message_coordinator
                        .send_to_player(
                            current_player_id,
                            Arc::new(ServerMessage::ReconnectionFailed {
                                reason: "Room is full".to_string(),
                                error_code: ErrorCode::RoomFull,
                            }),
                        )
                        .await;
                    return false;
                }
                Err(err) => {
                    tracing::error!(
                        %reconnect_player_id,
                        %room_id,
                        error = %err,
                        "Failed to restore player room membership on reconnection"
                    );
                    let _ = self
                        .message_coordinator
                        .send_to_player(
                            current_player_id,
                            Arc::new(ServerMessage::ReconnectionFailed {
                                reason: "Storage error".to_string(),
                                error_code: ErrorCode::InternalError,
                            }),
                        )
                        .await;
                    return false;
                }
            }
        }

        if disconnected.was_authority && room.supports_authority && room.authority_player.is_none()
        {
            if let Err(err) = self
                .database
                .update_room_authority(room_id, Some(*reconnect_player_id))
                .await
            {
                tracing::warn!(
                    %reconnect_player_id,
                    %room_id,
                    error = %err,
                    "Failed to restore authority on reconnection"
                );
            }
        }

        let room = match self.database.get_room_by_id(room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                let _ = self
                    .message_coordinator
                    .send_to_player(
                        current_player_id,
                        Arc::new(ServerMessage::ReconnectionFailed {
                            reason: "Room no longer exists".to_string(),
                            error_code: ErrorCode::RoomNotFound,
                        }),
                    )
                    .await;
                return false;
            }
            Err(err) => {
                tracing::error!("Failed to get restored room for reconnection: {}", err);
                let _ = self
                    .message_coordinator
                    .send_to_player(
                        current_player_id,
                        Arc::new(ServerMessage::ReconnectionFailed {
                            reason: "Storage error".to_string(),
                            error_code: ErrorCode::InternalError,
                        }),
                    )
                    .await;
                return false;
            }
        };

        // Update client connection to use the reconnecting player's original id.
        let Some(reassigned_sender) = self.connection_manager.reassign_connection(
            current_player_id,
            reconnect_player_id,
            *room_id,
        ) else {
            let _ = self
                .message_coordinator
                .send_to_player(
                    current_player_id,
                    Arc::new(ServerMessage::ReconnectionFailed {
                        reason: "Current connection no longer exists".to_string(),
                        error_code: ErrorCode::ReconnectionFailed,
                    }),
                )
                .await;
            return false;
        };
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

        // Complete reconnection in manager
        reconnection_manager
            .complete_claimed_reconnection(&disconnected)
            .await;

        // Prepare room state
        let current_players: Vec<PlayerInfo> = room.players.values().cloned().collect();
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
                    ready_players: room.ready_players.clone(),
                    relay_type: room.relay_type.clone(),
                    current_spectators: room.get_spectators(),
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

        self.pair_webrtc_peer_with_members(reconnect_player_id, &current_players)
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
