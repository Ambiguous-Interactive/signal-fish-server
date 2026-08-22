use crate::protocol::{
    DeliveryClass, ErrorCode, GameDataEncoding, PlayerId, RoomId, ServerMessage,
};
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
    pub async fn handle_game_data(
        &self,
        player_id: &PlayerId,
        data: serde_json::Value,
        class: Option<DeliveryClass>,
        key: Option<u32>,
    ) {
        let valid = if self.client_supports_v3(player_id) {
            matches!(
                (class, key),
                (
                    None | Some(DeliveryClass::Reliable | DeliveryClass::Volatile),
                    None
                ) | (Some(DeliveryClass::Latest), Some(_))
            )
        } else {
            class.is_none() && key.is_none()
        };
        if !valid {
            let _ = self
                .send_error_to_player(
                    player_id,
                    "Invalid delivery class: latest requires a key; reliable and volatile forbid one"
                        .to_string(),
                    Some(ErrorCode::InvalidDeliveryClass),
                )
                .await;
            return;
        }

        if let Some(room_id) = self.get_client_room(player_id).await {
            let connection_manager = &self.connection_manager;
            let expected_room = room_id;
            self.broadcast_game_data_with(player_id, &room_id, move || {
                let stamp =
                    connection_manager.next_relay_stamp_in_room(player_id, &expected_room)?;
                Some(ServerMessage::GameData {
                    from_player: *player_id,
                    data,
                    seq: Some(stamp.seq),
                    epoch: Some(stamp.epoch),
                    class,
                    key,
                })
            })
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

        // Binary frames bypass the message router, so valid frames record
        // liveness here (mirrors `handle_client_message`). Validate first: a
        // stream of rejected oversized frames must not keep an otherwise idle
        // client or room alive indefinitely.
        self.record_client_activity(player_id);
        self.maybe_update_last_seen(player_id).await;

        if let Some(room_id) = self.get_client_room(player_id).await {
            let connection_manager = &self.connection_manager;
            let expected_room = room_id;
            self.broadcast_game_data_with(player_id, &room_id, move || {
                let stamp =
                    connection_manager.next_relay_stamp_in_room(player_id, &expected_room)?;
                Some(ServerMessage::GameDataBinary {
                    from_player: *player_id,
                    encoding,
                    payload,
                    seq: Some(stamp.seq),
                    epoch: Some(stamp.epoch),
                })
            })
            .await;
        }
    }

    /// Broadcast one relayed game-data message (already stamped with its
    /// per-(sender, room) `seq` — text and binary share the single counter on
    /// the sender's `ClientConnection`) to the rest of the room.
    ///
    /// The stamp is carried inside the shared message envelope, so this layer
    /// — and the [`MessageCoordinator`](crate::coordination::MessageCoordinator)
    /// below it — stays protocol-version-agnostic: per-recipient gating
    /// (stripping `seq` for pre-v3 recipients) happens at serialization time
    /// in `websocket::sending`, where every other per-recipient wire decision
    /// (binary vs JSON-fallback encoding) already lives. Because the stamp is
    /// an ordinary serde field of `ServerMessage`, it also survives the
    /// cross-instance bus (`distributed::SequencedMessage` serializes the
    /// whole message); the in-memory single-instance coordinator is the only
    /// production backend today, so no remote instance can re-stamp or lose it.
    async fn broadcast_game_data_with<'a, F>(
        &'a self,
        player_id: &'a PlayerId,
        room_id: &RoomId,
        build_message: F,
    ) where
        F: FnOnce() -> Option<ServerMessage> + Send + 'a,
    {
        if let Err(e) = broadcast_game_data_with(
            self.message_coordinator.as_ref(),
            self.metrics.as_ref(),
            player_id,
            room_id,
            build_message,
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

/// Drive the production metric, one-shot adapter, and coordinator handoff.
pub async fn broadcast_game_data_with<'a, F>(
    message_coordinator: &'a dyn crate::coordination::MessageCoordinator,
    metrics: &crate::metrics::ServerMetrics,
    player_id: &'a PlayerId,
    room_id: &RoomId,
    build_message: F,
) -> anyhow::Result<()>
where
    F: FnOnce() -> Option<ServerMessage> + Send + 'a,
{
    metrics.increment_game_data_messages();
    let mut build_message = one_shot_message_builder(build_message);
    match message_coordinator.try_broadcast_to_room_except_with_borrowed_owned_message(
        room_id,
        player_id,
        &mut build_message,
    ) {
        crate::coordination::ImmediateGameDataBroadcast::Complete => Ok(()),
        crate::coordination::ImmediateGameDataBroadcast::Pending(completion) => {
            completion.await;
            Ok(())
        }
        crate::coordination::ImmediateGameDataBroadcast::Unavailable => {
            message_coordinator
                .broadcast_to_room_except_with_borrowed_owned_message(
                    room_id,
                    player_id,
                    &mut build_message,
                )
                .await
        }
    }
}

pub(super) fn one_shot_message_builder<F>(build_message: F) -> impl FnMut() -> Option<ServerMessage>
where
    F: FnOnce() -> Option<ServerMessage>,
{
    let mut build_message = Some(build_message);
    move || build_message.take().and_then(|build| build())
}
