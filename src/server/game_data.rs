use crate::protocol::{ErrorCode, GameDataEncoding, PlayerId, RoomId, ServerMessage};
use bytes::Bytes;
use std::sync::Arc;

use super::EnhancedGameServer;

impl EnhancedGameServer {
    /// Store legacy, self-declared peer metadata for the `GameStarting` handoff.
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

        tracing::info!(%player_id, %room_id, "Player provided legacy peer connection metadata");

        if let Err(e) = self
            .database
            .update_player_connection_info(&room_id, player_id, connection_info)
            .await
        {
            tracing::error!(%player_id, "Failed to store legacy peer metadata: {}", e);
            let _ = self
                .message_coordinator
                .send_to_player(
                    player_id,
                    Arc::new(ServerMessage::Error {
                        message: "Failed to store legacy peer metadata".to_string(),
                        error_code: Some(ErrorCode::InternalError),
                    }),
                )
                .await;
        }
    }

    /// Handle JSON game data fan-out with coordination.
    pub async fn handle_game_data(&self, player_id: &PlayerId, data: serde_json::Value) {
        if let Some(room_id) = self.get_client_room(player_id).await {
            let connection_manager = &self.connection_manager;
            self.broadcast_game_data_with(
                player_id,
                &room_id,
                Box::new(move || {
                    let stamp = connection_manager.next_relay_stamp(player_id);
                    ServerMessage::GameData {
                        from_player: *player_id,
                        data,
                        seq: stamp.map(|s| s.seq),
                        epoch: stamp.map(|s| s.epoch),
                    }
                }),
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
        // Binary frames bypass the message router, so record liveness here
        // (mirrors `handle_client_message`): a client streaming binary game
        // data must never be reaped as inactive, and its ROOM must not be GC'd
        // as inactive either (throttled room + last_seen refresh, BUG-1).
        self.record_client_activity(player_id);
        self.maybe_update_last_seen(player_id).await;
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
            let connection_manager = &self.connection_manager;
            self.broadcast_game_data_with(
                player_id,
                &room_id,
                Box::new(move || {
                    let stamp = connection_manager.next_relay_stamp(player_id);
                    ServerMessage::GameDataBinary {
                        from_player: *player_id,
                        encoding,
                        payload,
                        seq: stamp.map(|s| s.seq),
                        epoch: stamp.map(|s| s.epoch),
                    }
                }),
            )
            .await;
        }
    }

    /// Broadcast one relayed game-data message (already stamped with its
    /// per-(sender, room) `seq` — text and binary share the single counter on
    /// the sender's `ClientConnection`) to the rest of the room.
    ///
    /// The stamp is carried INSIDE the shared `Arc<ServerMessage>`, so this
    /// layer — and the [`MessageCoordinator`](crate::coordination::MessageCoordinator)
    /// below it — stays protocol-version-agnostic: per-recipient gating
    /// (stripping `seq` for pre-v3 recipients) happens at serialization time
    /// in `websocket::sending`, where every other per-recipient wire decision
    /// (binary vs JSON-fallback encoding) already lives. Because the stamp is
    /// an ordinary serde field of `ServerMessage`, it also survives the
    /// cross-instance bus (`distributed::SequencedMessage` serializes the
    /// whole message); the in-memory single-instance coordinator is the only
    /// production backend today, so no remote instance can re-stamp or lose it.
    async fn broadcast_game_data_with<'a>(
        &'a self,
        player_id: &'a PlayerId,
        room_id: &RoomId,
        build_message: Box<dyn FnOnce() -> ServerMessage + Send + 'a>,
    ) {
        // Count every GameData message accepted for relay. This is the sole
        // increment site for the `game_data_messages` metric (both the JSON and
        // binary handlers funnel through here); it was exported to Prometheus
        // but never incremented before (MISC-11), so the counter read a
        // permanent 0.
        self.metrics.increment_game_data_messages();

        // (Room + last_seen liveness is refreshed once upstream per inbound
        // message — by the router for text frames, by `handle_game_data_binary`
        // for binary frames — so it is intentionally not repeated here.)

        if let Err(e) = self
            .message_coordinator
            .broadcast_to_room_except_with_message(
                room_id,
                player_id,
                Box::new(move || Arc::new(build_message())),
            )
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
