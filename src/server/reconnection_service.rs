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

        let token = reconnection_manager
            .register_disconnection(*player_id, room_id, was_authority)
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
    ) {
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
            return;
        };

        // Validate reconnection
        let disconnected = match reconnection_manager
            .validate_reconnection(reconnect_player_id, room_id, auth_token)
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
                return;
            }
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
            return;
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
                return;
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
                return;
            }
        };

        // Get missed events
        let missed_events = reconnection_manager
            .get_missed_events(room_id, disconnected.last_sequence)
            .await;

        // Update client connection to use new sender
        self.connection_manager.reassign_connection(
            current_player_id,
            reconnect_player_id,
            *room_id,
        );

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
            .complete_reconnection(reconnect_player_id)
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
                    current_players,
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

        self.metrics.increment_players_joined();
        tracing::info!(
            %reconnect_player_id,
            %room_id,
            room_code = %room.code,
            "Player reconnected successfully"
        );
    }
}
