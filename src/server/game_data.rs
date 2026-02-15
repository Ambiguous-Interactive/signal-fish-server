use crate::protocol::{ErrorCode, GameDataEncoding, PlayerId, RoomId, ServerMessage};
use bytes::Bytes;
use std::sync::Arc;

use super::EnhancedGameServer;

impl EnhancedGameServer {
    /// Handle connection info from client for P2P establishment.
    pub async fn handle_provide_connection_info(
        &self,
        player_id: &PlayerId,
        connection_info: crate::protocol::ConnectionInfo,
    ) {
        let Some(room_id) = self.get_client_room(player_id).await else {
            let _ = self
                .message_coordinator
                .send_to_player(
                    player_id,
                    Arc::new(ServerMessage::Error {
                        message: "Not in a room".to_string(),
                        error_code: Some(ErrorCode::NotInRoom),
                    }),
                )
                .await;
            return;
        };

        tracing::info!(%player_id, %room_id, "Player provided connection info for P2P establishment");

        if let Err(e) = self
            .database
            .update_player_connection_info(&room_id, player_id, connection_info)
            .await
        {
            tracing::error!(%player_id, "Failed to store connection info: {}", e);
            let _ = self
                .message_coordinator
                .send_to_player(
                    player_id,
                    Arc::new(ServerMessage::Error {
                        message: "Failed to store connection info".to_string(),
                        error_code: Some(ErrorCode::InternalError),
                    }),
                )
                .await;
        }
    }

    /// Handle JSON game data fan-out with coordination.
    pub async fn handle_game_data(&self, player_id: &PlayerId, data: serde_json::Value) {
        if let Some(room_id) = self.get_client_room(player_id).await {
            self.broadcast_game_data(
                player_id,
                &room_id,
                ServerMessage::GameData {
                    from_player: *player_id,
                    data,
                },
            )
            .await;
        }
    }

    /// Handle binary game data payloads with coordination.
    /// Uses Bytes for zero-copy cloning during broadcast.
    pub async fn handle_game_data_binary(
        &self,
        player_id: &PlayerId,
        encoding: GameDataEncoding,
        payload: Bytes,
    ) {
        if payload.len() > self.config.max_message_size {
            tracing::warn!(
                %player_id,
                payload_size = payload.len(),
                max = self.config.max_message_size,
                "Binary game data payload exceeds maximum message size"
            );
            let _ = self
                .send_error_to_player(
                    player_id,
                    format!(
                        "Binary payload exceeded maximum size ({} bytes)",
                        self.config.max_message_size
                    ),
                    Some(ErrorCode::MessageTooLarge),
                )
                .await;
            return;
        }

        if let Some(room_id) = self.get_client_room(player_id).await {
            self.broadcast_game_data(
                player_id,
                &room_id,
                ServerMessage::GameDataBinary {
                    from_player: *player_id,
                    encoding,
                    payload,
                },
            )
            .await;
        }
    }

    async fn broadcast_game_data(
        &self,
        player_id: &PlayerId,
        room_id: &RoomId,
        message: ServerMessage,
    ) {
        // Update last_seen with throttling (same mechanism as heartbeat)
        self.maybe_update_last_seen(player_id).await;

        if let Err(e) = self
            .message_coordinator
            .broadcast_to_room_except(room_id, player_id, Arc::new(message))
            .await
        {
            tracing::error!(
                %player_id,
                %room_id,
                error = %e,
                "Failed to broadcast game data to room"
            );
        }
    }
}
