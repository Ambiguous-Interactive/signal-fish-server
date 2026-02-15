use super::{EnhancedGameServer, MaxRoomsPerGameExceededError};
use crate::distributed::LockHandle;
use crate::protocol::validation;
use crate::protocol::{
    LobbyState, PlayerId, PlayerInfo, RelayTransport, Room, RoomJoinedPayload, ServerMessage,
};
use std::sync::Arc;
use std::time::Duration;

impl EnhancedGameServer {
    /// Enhanced room joining with distributed coordination
    #[allow(clippy::too_many_arguments)]
    pub async fn handle_join_room(
        &self,
        player_id: &PlayerId,
        game_name: String,
        room_code: Option<String>,
        player_name: String,
        max_players: Option<u8>,
        supports_authority: Option<bool>,
        _relay_transport: Option<RelayTransport>, // Reserved for future transport selection
    ) {
        let requested_room_code = room_code.clone();
        let room_join_span = tracing::info_span!(
            "room.join",
            player_id = %player_id,
            game_name = %game_name,
            requested_room_code = requested_room_code
                .as_deref()
                .unwrap_or("auto"),
            room_code = tracing::field::Empty,
            room_id = tracing::field::Empty,
            instance_id = %self.instance_id,
            is_room_creation = room_code.is_none()
        );
        let _span_guard = room_join_span.enter();

        // Rate limiting check
        let is_room_creation = room_code.is_none();
        let rate_limit_result = if is_room_creation {
            self.rate_limiter.check_room_creation(player_id).await
        } else {
            self.rate_limiter.check_join_attempt(player_id).await
        };

        if let Err(rate_limit_error) = rate_limit_result {
            if let Err(e) = self
                .message_coordinator
                .send_to_player(
                    player_id,
                    Arc::new(ServerMessage::RoomJoinFailed {
                        reason: rate_limit_error.to_string(),
                        error_code: Some(crate::protocol::ErrorCode::RateLimitExceeded),
                    }),
                )
                .await
            {
                tracing::error!(%player_id, "Failed to send rate limit error: {}", e);
            }
            return;
        }

        // Validate inputs
        if let Err(reason) =
            validation::validate_game_name_with_config(&game_name, &self.protocol_config)
        {
            let _ = self
                .message_coordinator
                .send_to_player(
                    player_id,
                    Arc::new(ServerMessage::RoomJoinFailed {
                        reason,
                        error_code: Some(crate::protocol::ErrorCode::InvalidGameName),
                    }),
                )
                .await;
            return;
        }

        if let Err(reason) =
            validation::validate_player_name_with_config(&player_name, &self.protocol_config)
        {
            let _ = self
                .message_coordinator
                .send_to_player(
                    player_id,
                    Arc::new(ServerMessage::RoomJoinFailed {
                        reason,
                        error_code: Some(crate::protocol::ErrorCode::InvalidInput),
                    }),
                )
                .await;
            return;
        }

        let max_players = max_players.unwrap_or(self.config.default_max_players);
        if let Err(reason) =
            validation::validate_max_players_with_config(max_players, &self.protocol_config)
        {
            let _ = self
                .message_coordinator
                .send_to_player(
                    player_id,
                    Arc::new(ServerMessage::RoomJoinFailed {
                        reason,
                        error_code: Some(crate::protocol::ErrorCode::InvalidInput),
                    }),
                )
                .await;
            return;
        }

        let supports_authority = supports_authority.unwrap_or(true);

        // Check if player is already in a room
        if self.get_client_room(player_id).await.is_some() {
            let _ = self
                .message_coordinator
                .send_to_player(
                    player_id,
                    Arc::new(ServerMessage::RoomJoinFailed {
                        reason: "Already in a room".to_string(),
                        error_code: Some(crate::protocol::ErrorCode::AlreadyInRoom),
                    }),
                )
                .await;
            return;
        }

        let room_code = match room_code {
            Some(code) => {
                if let Err(reason) =
                    validation::validate_room_code_with_config(&code, &self.protocol_config)
                {
                    let _ = self
                        .message_coordinator
                        .send_to_player(
                            player_id,
                            Arc::new(ServerMessage::RoomJoinFailed {
                                reason,
                                error_code: Some(crate::protocol::ErrorCode::InvalidRoomCode),
                            }),
                        )
                        .await;
                    return;
                }
                code.to_uppercase()
            }
            None => self.generate_region_room_code(),
        };
        room_join_span.record("room_code", tracing::field::display(&room_code));

        // Use distributed coordination for room operations
        let room_join_result = self
            .join_room_with_coordination(
                player_id,
                &game_name,
                &room_code,
                &player_name,
                max_players,
                supports_authority,
            )
            .await;

        match room_join_result {
            Ok(room) => {
                room_join_span.record("room_id", tracing::field::display(room.id));
                self.connection_manager
                    .assign_client_to_room(player_id, room.id)
                    .await;

                // Get current players from database
                let current_players = match self.database.get_room_players(&room.id).await {
                    Ok(players) => players,
                    Err(e) => {
                        tracing::error!("Failed to get room players: {}", e);
                        Vec::new()
                    }
                };

                // Send success response
                let is_authority = room.authority_player == Some(*player_id);
                let _ = self
                    .message_coordinator
                    .send_to_player(
                        player_id,
                        Arc::new(ServerMessage::RoomJoined(Box::new(RoomJoinedPayload {
                            room_id: room.id,
                            room_code: room.code.clone(),
                            player_id: *player_id,
                            game_name: room.game_name.clone(),
                            max_players: room.max_players,
                            supports_authority: room.supports_authority,
                            current_players: current_players.clone(),
                            is_authority,
                            lobby_state: room.lobby_state.clone(),
                            ready_players: room.ready_players.clone(),
                            relay_type: room.relay_type.clone(),
                            current_spectators: room.get_spectators(),
                        }))),
                    )
                    .await;

                // Notify other players
                let player_info = PlayerInfo {
                    id: *player_id,
                    name: player_name,
                    is_authority,
                    is_ready: false,
                    connected_at: chrono::Utc::now(),
                    connection_info: None,
                    region_id: self.region_id().to_string(),
                };
                let _ = self
                    .message_coordinator
                    .broadcast_to_room_except(
                        &room.id,
                        player_id,
                        Arc::new(ServerMessage::PlayerJoined {
                            player: player_info,
                        }),
                    )
                    .await;

                // Check if room should transition to lobby state
                if room.should_enter_lobby() {
                    if let Err(e) = self
                        .room_coordinator
                        .transition_room_to_lobby(&room.id)
                        .await
                    {
                        tracing::error!("Failed to transition room to lobby: {}", e);
                    }
                }

                tracing::info!(
                    %player_id,
                    room_id = %room.id,
                    %game_name,
                    room_code = %room.code,
                    instance_id = %self.instance_id,
                    "Player joined room with distributed coordination"
                );
            }
            Err(e) => {
                let reason = e.to_string();
                let error_code = if e.downcast_ref::<MaxRoomsPerGameExceededError>().is_some() {
                    Some(crate::protocol::ErrorCode::MaxRoomsPerGameExceeded)
                } else {
                    Some(crate::protocol::ErrorCode::RoomCreationFailed)
                };
                let _ = self
                    .message_coordinator
                    .send_to_player(
                        player_id,
                        Arc::new(ServerMessage::RoomJoinFailed { reason, error_code }),
                    )
                    .await;
            }
        }
    }

    /// Leave room with coordination
    pub async fn leave_room(&self, player_id: &PlayerId) {
        let leave_span = tracing::info_span!(
            "room.leave",
            player_id = %player_id,
            room_id = tracing::field::Empty,
            room_code = tracing::field::Empty,
            instance_id = %self.instance_id
        );
        let _span_guard = leave_span.enter();
        let Some(room_id) = self.get_client_room(player_id).await else {
            return;
        };
        leave_span.record("room_id", tracing::field::display(room_id));

        // Remove player from room in database
        let player_removed = match self
            .database
            .remove_player_from_room(&room_id, player_id)
            .await
        {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(e) => {
                tracing::error!("Failed to remove player from room: {}", e);
                false
            }
        };

        if !player_removed {
            return;
        }

        self.metrics.increment_players_left();

        // Update client connection and coordinator
        let existing_sender = self.connection_manager.clear_room_assignment(player_id);

        if let Some(sender) = existing_sender {
            let _ = self
                .message_coordinator
                .register_local_client(*player_id, None, sender)
                .await;
        } else {
            tracing::warn!(%player_id, "Could not find existing sender for player when leaving room");
        }

        // First send confirmation to the leaving player
        let _ = self
            .message_coordinator
            .send_to_player(player_id, Arc::new(ServerMessage::RoomLeft))
            .await;

        // Then notify other players (excluding the player who left)
        let _ = self
            .message_coordinator
            .broadcast_to_room_except(
                &room_id,
                player_id,
                Arc::new(ServerMessage::PlayerLeft {
                    player_id: *player_id,
                }),
            )
            .await;

        // Check if room should transition out of lobby state after player left
        let mut latest_room_code: Option<String> = None;
        if let Ok(Some(room)) = self.database.get_room_by_id(&room_id).await {
            latest_room_code = Some(room.code.clone());
            if room.lobby_state == LobbyState::Lobby && !room.should_enter_lobby() {
                if let Err(e) = self.database.transition_room_to_waiting(&room_id).await {
                    tracing::warn!("Failed to transition room back to waiting state: {}", e);
                } else {
                    tracing::info!(
                        %room_id,
                        "Room transitioned from lobby back to waiting state after player left"
                    );

                    if let Err(e) = self.room_coordinator.clear_ready_players(&room_id).await {
                        tracing::warn!("Failed to clear ready players from coordinator: {}", e);
                    }
                }
            }
        }
        if let Some(code) = &latest_room_code {
            leave_span.record("room_code", tracing::field::display(code));
        }

        tracing::info!(
            %player_id,
            %room_id,
            room_code = latest_room_code.as_deref().unwrap_or("unknown"),
            instance_id = %self.instance_id,
            "Player left room with distributed coordination"
        );
    }

    /// Join room with distributed coordination
    pub(super) async fn join_room_with_coordination(
        &self,
        player_id: &PlayerId,
        game_name: &str,
        room_code: &str,
        player_name: &str,
        max_players: u8,
        supports_authority: bool,
    ) -> anyhow::Result<Room> {
        let lock_key = format!("room_join:{game_name}:{room_code}");
        let lock_handle = self
            .distributed_lock
            .acquire(&lock_key, Duration::from_secs(10))
            .await?;
        let mut game_cap_lock: Option<LockHandle> = None;

        // Try to join existing room or create new one
        let result = match self.database.get_room(game_name, room_code).await {
            Ok(Some(mut room)) => {
                let client_app_id = self.client_app_id(player_id);
                // Validate player name uniqueness
                if let Err(reason) =
                    validation::validate_player_name_uniqueness(player_name, &room.players)
                {
                    let _ = self.distributed_lock.release(&lock_handle).await;
                    return Err(anyhow::anyhow!(reason));
                }

                let player_info = PlayerInfo {
                    id: *player_id,
                    name: player_name.to_string(),
                    is_authority: false,
                    is_ready: false,
                    connected_at: chrono::Utc::now(),
                    connection_info: None,
                    region_id: room.region_id.clone(),
                };

                match self
                    .database
                    .add_player_to_room(&room.id, player_info.clone())
                    .await
                {
                    Ok(true) => {
                        self.metrics.increment_rooms_joined();
                        self.metrics.increment_players_joined();
                        room.players.insert(*player_id, player_info);
                        if self.room_application_id(&room.id).is_none() {
                            if let Some(persisted_app) = room.application_id {
                                self.room_applications.insert(room.id, persisted_app);
                            } else if let Some(app_id) = client_app_id {
                                self.record_room_application(&room.id, app_id).await;
                            }
                        }
                        Ok(room)
                    }
                    Ok(false) => Err(anyhow::anyhow!("Room is full")),
                    Err(e) => Err(e),
                }
            }
            Ok(None) => {
                // Enforce per-game room cap before creating a new room
                let cap_lock_key = format!("game_room_cap:{game_name}");
                match self
                    .distributed_lock
                    .acquire(&cap_lock_key, Duration::from_secs(10))
                    .await
                {
                    Ok(lock) => {
                        self.metrics.increment_room_cap_lock_acquisitions();
                        game_cap_lock = Some(lock);
                    }
                    Err(err) => {
                        tracing::error!("Failed to acquire cap lock: {}", err);
                        self.metrics.increment_room_cap_lock_failures();
                    }
                }

                let current_room_count = self.database.get_game_room_count(game_name).await?;
                if current_room_count >= self.config.max_rooms_per_game {
                    self.metrics.increment_room_cap_denials();
                    if let Some(lock) = &game_cap_lock {
                        let _ = self.distributed_lock.release(lock).await;
                    }
                    return Err(anyhow::anyhow!(MaxRoomsPerGameExceededError {
                        game_name: game_name.to_string(),
                        current: current_room_count,
                        limit: self.config.max_rooms_per_game,
                    }));
                }

                let relay_type = self.resolve_relay_type(game_name);
                let client_app_id = self.client_app_id(player_id);
                let region_id = self.region_id().to_string();
                let created_room = self
                    .database
                    .create_room(
                        game_name.to_string(),
                        Some(room_code.to_string()),
                        max_players,
                        supports_authority,
                        *player_id,
                        relay_type,
                        region_id.clone(),
                        client_app_id,
                    )
                    .await;

                if let Some(lock) = &game_cap_lock {
                    let _ = self.distributed_lock.release(lock).await;
                }

                match created_room {
                    Ok(mut room) => {
                        self.metrics.increment_rooms_created();
                        self.metrics.increment_players_joined();
                        if let Some(app_id) = client_app_id {
                            self.record_room_application(&room.id, app_id).await;
                        }
                        if let Err(e) = self
                            .database
                            .update_player_name(&room.id, player_id, player_name)
                            .await
                        {
                            tracing::warn!(%player_id, "Failed to update creator name: {}", e);
                        } else if let Some(creator_info) = room.players.get_mut(player_id) {
                            creator_info.name = player_name.to_string();
                        }
                        Ok(room)
                    }
                    Err(e) => Err(anyhow::anyhow!(e)),
                }
            }
            Err(e) => Err(e),
        };

        let _ = self.distributed_lock.release(&lock_handle).await;
        result
    }
}
