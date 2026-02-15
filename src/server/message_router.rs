use crate::protocol::{ClientMessage, PlayerId};

use super::EnhancedGameServer;

impl EnhancedGameServer {
    /// Handle incoming client message with enhanced coordination.
    pub async fn handle_client_message(&self, player_id: &PlayerId, message: ClientMessage) {
        match message {
            ClientMessage::Authenticate { app_id, .. } => {
                tracing::warn!(
                    %player_id,
                    %app_id,
                    "Received Authenticate message after connection established - this should not happen. \
                     Authentication must occur during WebSocket handshake."
                );
            }
            ClientMessage::JoinRoom {
                game_name,
                room_code,
                player_name,
                max_players,
                supports_authority,
                relay_transport,
            } => {
                self.handle_join_room(
                    player_id,
                    game_name,
                    room_code,
                    player_name,
                    max_players,
                    supports_authority,
                    relay_transport,
                )
                .await;
            }
            ClientMessage::LeaveRoom => {
                self.leave_room(player_id).await;
            }
            ClientMessage::GameData { data } => {
                self.handle_game_data(player_id, data).await;
            }
            ClientMessage::AuthorityRequest { become_authority } => {
                self.handle_authority_request(player_id, become_authority)
                    .await;
            }
            ClientMessage::PlayerReady => {
                self.handle_player_ready(player_id).await;
            }
            ClientMessage::ProvideConnectionInfo { connection_info } => {
                self.handle_provide_connection_info(player_id, connection_info)
                    .await;
            }
            ClientMessage::Ping => {
                self.handle_ping(player_id).await;
            }
            ClientMessage::Reconnect {
                player_id: reconnect_player_id,
                room_id,
                auth_token,
            } => {
                self.handle_reconnect(player_id, &reconnect_player_id, &room_id, &auth_token)
                    .await;
            }
            ClientMessage::JoinAsSpectator {
                game_name,
                room_code,
                spectator_name,
            } => {
                self.handle_join_as_spectator(player_id, game_name, room_code, spectator_name)
                    .await;
            }
            ClientMessage::LeaveSpectator => {
                self.handle_leave_spectator(player_id).await;
            }
        }
    }
}
