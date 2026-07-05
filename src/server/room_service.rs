use super::{EnhancedGameServer, MaxRoomsPerGameExceededError};
use crate::distributed::LockHandle;
use crate::protocol::validation;
use crate::protocol::{
    ErrorCode, PlayerId, PlayerInfo, RelayTransport, Room, RoomJoinedPayload, ServerMessage,
};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

const ROOM_JOIN_LOCK_TTL: Duration = Duration::from_secs(10);
const GAME_ROOM_CAP_LOCK_TTL: Duration = Duration::from_secs(10);

/// Typed failure of [`EnhancedGameServer::join_room_with_coordination`], so the
/// handler classifies each cause to the correct client [`ErrorCode`] by an
/// exhaustive, compiler-checked `match` — never by inspecting an error string or
/// downcasting (the fragile shape that let a "room is full" rejection
/// masquerade as the generic `ROOM_CREATION_FAILED`).
///
/// Mirrors [`crate::coordination::PlayerReadyError`]: a *business* rejection
/// gets its own specific code (`RoomFull`, `MaxRoomsPerGameExceeded`); every
/// other failure — storage, lock, name validation, broadcast — is an
/// infrastructure fault that surfaces as the generic `RoomCreationFailed`.
/// Because [`Self::error_code`] is exhaustive, adding a new business rejection
/// forces a new arm to compile, so the class of "a distinct failure silently
/// collapses into the catch-all code" cannot reappear unnoticed.
#[derive(Debug, Error)]
pub(super) enum JoinRoomError {
    /// The room is at capacity — a business rejection (→ `ROOM_FULL`).
    #[error("Room is full")]
    RoomFull,
    /// The per-game room cap is reached — a business rejection
    /// (→ `MAX_ROOMS_PER_GAME_EXCEEDED`).
    #[error(transparent)]
    MaxRoomsPerGameExceeded(#[from] MaxRoomsPerGameExceededError),
    /// Any other failure — storage, lock, name validation, broadcast — an
    /// infrastructure fault that must NOT masquerade as a specific business
    /// rejection (→ `ROOM_CREATION_FAILED`).
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl JoinRoomError {
    /// The client-facing [`ErrorCode`] for this failure. Exhaustive by design: a
    /// new variant cannot compile without an explicit, reviewed classification.
    pub(super) fn error_code(&self) -> ErrorCode {
        match self {
            Self::RoomFull => ErrorCode::RoomFull,
            Self::MaxRoomsPerGameExceeded(_) => ErrorCode::MaxRoomsPerGameExceeded,
            Self::Internal(_) => ErrorCode::RoomCreationFailed,
        }
    }
}

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
                        error_code: Some(crate::protocol::ErrorCode::InvalidPlayerName),
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
                        error_code: Some(crate::protocol::ErrorCode::InvalidMaxPlayers),
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
                let mut current_players = match self.database.get_room_players(&room.id).await {
                    Ok(players) => players,
                    Err(e) => {
                        tracing::error!("Failed to get room players: {}", e);
                        Vec::new()
                    }
                };

                // The live ready set is held by the coordinator (the room record
                // only syncs `ready_players` / `is_ready` at finalize), so read it
                // from there to report an accurate ready set to a player joining a
                // lobby that already has ready members; reflect it on each player's
                // `is_ready` too.
                let ready_players = self.room_coordinator.current_ready_players(&room.id).await;
                // v4 room snapshot: give a v4 joiner each member's current
                // incarnation epoch so it can baseline the per-sender (epoch,
                // seq) stream before the first relayed frame. Single recipient
                // (the joiner), so gate on its version at construction — a pre-v4
                // joiner keeps every epoch `None` and byte-identical v2/v3 bytes.
                let recipient_is_v4 = self.connection_manager.supports_v4(player_id);
                for player in current_players.iter_mut() {
                    player.is_ready = ready_players.contains(&player.id);
                    player.epoch = if recipient_is_v4 {
                        self.connection_manager.game_data_epoch(&player.id)
                    } else {
                        None
                    };
                }

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
                            ready_players: ready_players.clone(),
                            relay_type: room.relay_type.clone(),
                            current_spectators: room.get_spectators(),
                            // v3 ICE pre-gather (deferred refinement):
                            // empty — and skipped on the wire — unless this
                            // joiner passes the pre-gather gate, so v2 bytes
                            // are untouched. A join into a Finalized room gets
                            // its ICE from the late-join SessionPlan below
                            // instead (never both).
                            ice_servers: self.pregather_ice_servers(&room, player_id),
                            // Minted at join so an unexpected disconnect is
                            // recoverable with a token the client actually
                            // holds (v3+ only; None keeps v2 bytes frozen).
                            reconnection_token: self
                                .pre_issue_reconnection_token_for(player_id, room.id)
                                .await,
                        }))),
                    )
                    .await;

                // Notify other players. This is a v4 wire snapshot, so it
                // carries the joiner's current incarnation epoch (Some after the
                // `assign_client_to_room` above bumped it); pre-v4 recipients
                // have it stripped per-recipient in `websocket::sending`.
                let player_info = PlayerInfo {
                    id: *player_id,
                    name: player_name,
                    is_authority,
                    is_ready: false,
                    connected_at: chrono::Utc::now(),
                    connection_info: None,
                    epoch: self.connection_manager.game_data_epoch(player_id),
                    region_id: self.region_id().to_string(),
                };
                // Recorded for reconnection replay BEFORE delivery (buffered
                // even if the broadcast partially fails, matching "what a
                // connected player would have been sent").
                let player_joined = ServerMessage::PlayerJoined {
                    player: player_info,
                };
                self.record_replayable_room_event(&room.id, &player_joined)
                    .await;
                let _ = self
                    .message_coordinator
                    .broadcast_to_room_except(&room.id, player_id, Arc::new(player_joined))
                    .await;

                // Bring the joiner into an ACTIVE (finalized) v3 session (PLAN
                // §P3, Appendix E/L): the joiner receives a tailored
                // `SessionPlan` for the room's stored running session and
                // existing members receive the additive `NewPeer` delta. A room
                // reaches `Finalized` only while full, but a departure can
                // reopen a seat (`add_player_to_room` gates only on fullness),
                // so this fires for seat-filling joins into live sessions.
                // Purely additive and gated to v3 (+ WebRTC for `NewPeer`), so
                // v2 message ordering and bytes are untouched.
                self.handle_active_session_late_join(&room, player_id, &current_players)
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
            Err(error) => {
                // Each cause carries its own client `ErrorCode` via an
                // exhaustive, compiler-checked `match` (see `JoinRoomError`): a
                // business rejection (room full / per-game cap) is never
                // conflated with an infrastructure fault.
                let error_code = Some(error.error_code());
                let reason = error.to_string();
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

        // The player is out of the room: its pre-issued reconnection token
        // must not stay claimable. On the DISCONNECT path this is a no-op
        // (register_disconnection_for_reconnect already consumed the entry
        // before unregistration reaches this method); on a voluntary
        // LeaveRoom it is the discard that keeps the token map bounded by
        // currently-joined players.
        self.discard_pre_issued_reconnection_token(player_id).await;

        if !player_removed {
            return;
        }

        self.metrics.increment_players_left();

        // Update client connection and coordinator
        let existing_delivery = self.connection_manager.clear_room_assignment(player_id);

        if let Some(delivery) = existing_delivery {
            let _ = self
                .message_coordinator
                .register_local_client(*player_id, None, delivery)
                .await;
        } else {
            tracing::warn!(%player_id, "Could not find existing delivery handle for player when leaving room");
        }

        // First send confirmation to the leaving player
        let _ = self
            .message_coordinator
            .send_to_player(player_id, Arc::new(ServerMessage::RoomLeft))
            .await;

        // Then notify other players (excluding the player who left). Recorded
        // for reconnection replay BEFORE delivery (buffered even if the
        // broadcast partially fails).
        let player_left = ServerMessage::PlayerLeft {
            player_id: *player_id,
        };
        self.record_replayable_room_event(&room_id, &player_left)
            .await;
        let _ = self
            .message_coordinator
            .broadcast_to_room_except(&room_id, player_id, Arc::new(player_left))
            .await;

        // v3 mid-session re-planning (after the PlayerLeft broadcast): if the
        // departed player hosted the room's active non-relay session, re-elect
        // the host and re-emit fresh per-recipient SessionPlans. `leave_room`
        // is the single choke point for explicit LeaveRoom AND disconnects
        // (`unregister_client` routes through here), so one hook covers both.
        // Rooms without a stored plan (relay floor / pre-v3) return immediately
        // — pure v2 semantics, where PlayerLeft alone suffices.
        self.handle_session_member_departure(&room_id, player_id)
            .await;

        // A departure no longer regresses the lobby. `max_players` is a ceiling,
        // not a required count, so a partially-full room stays a valid lobby:
        // the remaining players keep their readiness and can still start the game
        // (an explicit `StartGame`, once all current players are ready). A
        // `Finalized` room likewise stays finalized (the running session is
        // re-planned by `handle_session_member_departure` above, not regressed).
        // The coordinator's in-memory ready set is NOT cleared here: reads filter
        // it by current membership (so a departed id is never reported ready),
        // and the entry itself is reclaimed when the room is deleted — promptly
        // by the empty-room cleanup loop, and as an all-paths backstop by
        // `prune_ready_players` (see `src/server/maintenance.rs`). Keeping this
        // hot path free of coordinator coupling mirrors how session plans are
        // handled (re-planned here, swept for removal elsewhere).
        let mut latest_room_code: Option<String> = None;
        if let Ok(Some(room)) = self.database.get_room_by_id(&room_id).await {
            latest_room_code = Some(room.code.clone());
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
    ) -> Result<Room, JoinRoomError> {
        let lock_key = format!("room_join:{game_name}:{room_code}");
        let lock_handle = self
            .distributed_lock
            .acquire(&lock_key, ROOM_JOIN_LOCK_TTL)
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
                    Err(anyhow::anyhow!(reason).into())
                } else {
                    let player_info = PlayerInfo {
                        id: *player_id,
                        name: player_name.to_string(),
                        is_authority: false,
                        is_ready: false,
                        connected_at: chrono::Utc::now(),
                        connection_info: None,
                        // Room-state record (stored in the DB + `room.players`),
                        // not a wire snapshot: the v4 epoch is filled at
                        // snapshot-send time, so this stays `None`.
                        epoch: None,
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
                        Ok(false) => Err(JoinRoomError::RoomFull),
                        Err(e) => Err(e.into()),
                    }
                }
            }
            Ok(None) => {
                // Enforce per-game room cap before creating a new room
                let cap_lock_key = format!("game_room_cap:{game_name}");
                match self
                    .distributed_lock
                    .acquire(&cap_lock_key, GAME_ROOM_CAP_LOCK_TTL)
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

                match self.database.get_game_room_count(game_name).await {
                    Ok(current_room_count)
                        if current_room_count >= self.config.max_rooms_per_game =>
                    {
                        self.metrics.increment_room_cap_denials();
                        if let Some(lock) = &game_cap_lock {
                            let _ = self.distributed_lock.release(lock).await;
                        }
                        Err(JoinRoomError::MaxRoomsPerGameExceeded(
                            MaxRoomsPerGameExceededError {
                                game_name: game_name.to_string(),
                                current: current_room_count,
                                limit: self.config.max_rooms_per_game,
                            },
                        ))
                    }
                    Ok(_) => {
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
                            Err(e) => Err(anyhow::anyhow!(e).into()),
                        }
                    }
                    Err(err) => {
                        tracing::error!("Failed to read room count for cap enforcement: {}", err);
                        if let Some(lock) = &game_cap_lock {
                            let _ = self.distributed_lock.release(lock).await;
                        }
                        Err(err.into())
                    }
                }
            }
            Err(e) => Err(e.into()),
        };

        let _ = self.distributed_lock.release(&lock_handle).await;
        result
    }
}
