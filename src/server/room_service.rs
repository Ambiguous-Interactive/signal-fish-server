use super::{
    EnhancedGameServer, MaxPlayersPerApplicationExceededError, MaxRoomsPerApplicationExceededError,
    MaxRoomsPerGameExceededError, PendingApplicationClaimRollback,
};
use crate::database::CreateRoomError;
use crate::distributed::LockHandle;
use crate::protocol::validation;
use crate::protocol::{
    ErrorCode, LobbyState, PlayerId, PlayerInfo, RelayTransport, Room, RoomId, RoomJoinedPayload,
    ServerMessage,
};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

const ROOM_JOIN_LOCK_TTL: Duration = Duration::from_secs(10);
const APPLICATION_ROOM_CAP_LOCK_TTL: Duration = Duration::from_secs(10);
const GAME_ROOM_CAP_LOCK_TTL: Duration = Duration::from_secs(10);
const GENERATED_ROOM_CODE_MAX_ATTEMPTS: u8 = 8;

#[derive(Clone, Copy)]
enum RoomAdmissionIntent {
    ExistingOrCreate,
    CreateOnly,
}

#[derive(Clone)]
enum RoomAdmissionKind {
    Existing {
        application_claim_rollback: Option<PendingApplicationClaimRollback>,
    },
    Created {
        application_claim_rollback: Option<PendingApplicationClaimRollback>,
    },
}

impl RoomAdmissionKind {
    fn application_claim_rollback(&self) -> Option<&PendingApplicationClaimRollback> {
        match self {
            Self::Existing {
                application_claim_rollback,
            }
            | Self::Created {
                application_claim_rollback,
            } => application_claim_rollback.as_ref(),
        }
    }
}

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
    /// The accepted app label's cross-game room quota is reached. This
    /// uses the existing v2 room-cap wire code for backward compatibility.
    #[error(transparent)]
    MaxRoomsPerApplicationExceeded(#[from] MaxRoomsPerApplicationExceededError),
    /// A creator requested a capacity above its configured application cap.
    #[error(transparent)]
    MaxPlayersPerApplicationExceeded(#[from] MaxPlayersPerApplicationExceededError),
    /// The room is absent or belongs to another accepted app label.
    /// Both cases intentionally share one non-enumerating wire outcome.
    #[error("Room not found")]
    RoomNotFound,
    /// The server is draining for shutdown and must not create new rooms
    /// (→ `SERVER_DRAINING`).
    #[error("Server is draining for shutdown")]
    ServerDraining,
    /// An automatic room-code candidate was already occupied. This never
    /// reaches the client directly; the bounded create loop consumes it.
    #[error("Generated room code collision")]
    RoomCodeCollision,
    /// Every automatic room-code candidate in the bounded budget collided.
    #[error("Unable to allocate a unique generated room code after {attempts} attempts")]
    RoomCodeRetryExhausted { attempts: u8 },
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
            Self::MaxRoomsPerApplicationExceeded(_) => ErrorCode::MaxRoomsPerGameExceeded,
            Self::MaxPlayersPerApplicationExceeded(_) => ErrorCode::InvalidMaxPlayers,
            Self::RoomNotFound => ErrorCode::RoomNotFound,
            Self::ServerDraining => ErrorCode::ServerDraining,
            Self::RoomCodeCollision | Self::RoomCodeRetryExhausted { .. } => {
                ErrorCode::RoomCreationFailed
            }
            Self::Internal(_) => ErrorCode::RoomCreationFailed,
        }
    }
}

impl EnhancedGameServer {
    async fn rollback_unpublished_player_admission(
        &self,
        room_id: RoomId,
        player_id: PlayerId,
        admission_kind: RoomAdmissionKind,
        reason: &'static str,
    ) {
        if matches!(admission_kind, RoomAdmissionKind::Created { .. }) {
            match self.database.delete_room(&room_id).await {
                Ok(_) => {
                    self.room_applications.remove(&room_id);
                    let _ = self.room_coordinator.clear_ready_players(&room_id).await;
                    self.metrics.increment_players_left();
                    return;
                }
                Err(error) => {
                    tracing::error!(%player_id, %room_id, %error, reason, "Failed to roll back unpublished room creation; falling back to durable player detach");
                }
            }
        }

        let application_claim_rollback = admission_kind.application_claim_rollback().cloned();
        let admission_removed = match self
            .database
            .remove_player_from_room(&room_id, &player_id)
            .await
        {
            Ok(_) => {
                self.pending_durable_player_detaches
                    .remove(&(room_id, player_id));
                true
            }
            Err(error) => {
                self.pending_durable_player_detaches
                    .insert((room_id, player_id), application_claim_rollback.clone());
                tracing::error!(%player_id, %room_id, %error, reason, "Failed to roll back unpublished room admission; queued durable detach retry");
                false
            }
        };
        if admission_removed {
            if let Some(rollback) = application_claim_rollback {
                if let Err(error) = self
                    .rollback_room_application_claim_if_unchanged(&room_id, &rollback)
                    .await
                {
                    self.pending_durable_player_detaches
                        .insert((room_id, player_id), Some(rollback));
                    tracing::error!(%player_id, %room_id, %error, reason, "Failed to roll back unpublished room ownership claim; queued durable retry");
                }
            }
        }
        // Admission already moved `players_joined`, but no RoomJoined reached
        // the client. Balance that logical membership immediately; eventual
        // storage repair must not move activity metrics a second time.
        self.metrics.increment_players_left();
    }

    /// Join a room under process-local coordination.
    #[allow(clippy::too_many_arguments)]
    pub async fn handle_join_room(
        self: &Arc<Self>,
        player_id: &PlayerId,
        game_name: String,
        room_code: Option<String>,
        player_name: String,
        max_players: Option<u8>,
        supports_authority: Option<bool>,
        relay_transport: Option<RelayTransport>,
    ) {
        let server = Arc::clone(self);
        let player_id = *player_id;
        let task = tokio::spawn(async move {
            server
                .handle_join_room_owned(
                    player_id,
                    game_name,
                    room_code,
                    player_name,
                    max_players,
                    supports_authority,
                    relay_transport,
                )
                .await;
        });
        if let Err(error) = task.await {
            tracing::error!(%player_id, %error, "Owned room join transaction failed");
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_join_room_owned(
        self: Arc<Self>,
        player_id: PlayerId,
        game_name: String,
        room_code: Option<String>,
        player_name: String,
        max_players: Option<u8>,
        supports_authority: Option<bool>,
        _relay_transport: Option<RelayTransport>,
    ) {
        let player_id = &player_id;
        let Some(lifecycle) = self.connection_manager.client_lifecycle(player_id) else {
            return;
        };
        let _lifecycle_guard = Arc::clone(&lifecycle).lock_owned().await;
        if lifecycle.player_id() != *player_id
            || !self
                .connection_manager
                .lifecycle_matches(player_id, &lifecycle)
        {
            return;
        }

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

        let is_room_creation = room_code.is_none();
        if self
            .join_would_create_room_while_draining(&game_name, room_code.as_deref())
            .await
        {
            self.reject_join_for_shutdown_drain(player_id).await;
            return;
        }

        // Rate limiting check
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

        // One physical connection has exactly one room role. Spectator
        // membership lives outside ConnectionManager, so check both registries
        // before any seated-room mutation or delivery-generation advance.
        let is_spectating = self.spectator_service.is_spectating(player_id);
        if self.get_client_room(player_id).await.is_some() || is_spectating {
            let _ = self
                .message_coordinator
                .send_to_player(
                    player_id,
                    Arc::new(ServerMessage::RoomJoinFailed {
                        reason: if is_spectating {
                            "Already participating in a room as a spectator".to_string()
                        } else {
                            "Already in a room".to_string()
                        },
                        error_code: Some(crate::protocol::ErrorCode::AlreadyInRoom),
                    }),
                )
                .await;
            return;
        }

        let room_join_result = match room_code {
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
                let room_code = code.to_uppercase();
                room_join_span.record("room_code", tracing::field::display(&room_code));
                self.join_room_with_coordination(
                    player_id,
                    &game_name,
                    &room_code,
                    &player_name,
                    max_players,
                    supports_authority,
                    RoomAdmissionIntent::ExistingOrCreate,
                )
                .await
            }
            None => {
                self.create_room_with_generated_code(
                    player_id,
                    &game_name,
                    &player_name,
                    max_players,
                    supports_authority,
                )
                .await
            }
        };

        match room_join_result {
            Ok((room, room_event_guard, admission_kind)) => {
                room_join_span.record("room_code", tracing::field::display(&room.code));
                room_join_span.record("room_id", tracing::field::display(room.id));
                let Some((delivery, membership_stamp)) = self
                    .connection_manager
                    .prepare_client_to_room(player_id, room.id)
                else {
                    self.rollback_unpublished_player_admission(
                        room.id,
                        *player_id,
                        admission_kind,
                        "missing_prepared_connection",
                    )
                    .await;
                    return;
                };

                let recipient_is_v3 = self.connection_manager.supports_v3(player_id);
                let server = Arc::clone(&self);
                let response_player_id = *player_id;
                let response_room_id = room.id;
                let baseline_room = Arc::new(std::sync::Mutex::new(None));
                let baseline_room_in_builder = Arc::clone(&baseline_room);
                let initial_delivery = self
                    .message_coordinator
                    .register_local_client_with_initial_message_async(
                        *player_id,
                        room.id,
                        delivery,
                        Box::new(move |routed_player_ids| {
                            Box::pin(async move {
                                let routed_player_ids: HashSet<PlayerId> =
                                    routed_player_ids.into_iter().collect();
                                let current_room = server
                                    .database
                                    .get_room_by_id(&response_room_id)
                                    .await
                                    .map_err(|error| {
                                        anyhow::anyhow!(
                                            "failed to fetch joined room baseline: {error}"
                                        )
                                    })?
                                    .ok_or_else(|| anyhow::anyhow!("joined room disappeared"))?;
                                let mut current_players = server
                                    .database
                                    .get_room_players(&response_room_id)
                                    .await
                                    .map_err(|error| {
                                        anyhow::anyhow!(
                                            "failed to fetch joined player baseline: {error}"
                                        )
                                    })?;
                                current_players
                                    .retain(|player| routed_player_ids.contains(&player.id));
                                if !current_players
                                    .iter()
                                    .any(|player| player.id == response_player_id)
                                {
                                    return Err(anyhow::anyhow!(
                                        "joiner missing from routed room baseline"
                                    ));
                                }

                                let mut ready_players = server
                                    .room_coordinator
                                    .current_ready_players(&response_room_id)
                                    .await;
                                for player in &mut current_players {
                                    player.is_ready = ready_players.contains(&player.id);
                                    player.epoch = None;
                                    player.seq = None;
                                }
                                if recipient_is_v3 {
                                    current_players.retain_mut(|player| {
                                        let Some(stamp) =
                                            server.connection_manager.current_relay_stamp_in_room(
                                                &player.id,
                                                &response_room_id,
                                            )
                                        else {
                                            return false;
                                        };
                                        player.epoch = Some(stamp.epoch);
                                        player.seq = Some(stamp.seq);
                                        true
                                    });
                                }
                                ready_players.retain(|ready_player_id| {
                                    current_players
                                        .iter()
                                        .any(|player| player.id == *ready_player_id)
                                });
                                if server.should_retain_room_publication_snapshot() {
                                    *baseline_room_in_builder
                                        .lock()
                                        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                                        Some(current_room.clone());
                                }

                                Ok(Arc::new(ServerMessage::RoomJoined(Box::new(
                                    RoomJoinedPayload {
                                        room_id: current_room.id,
                                        room_code: current_room.code.clone(),
                                        player_id: response_player_id,
                                        game_name: current_room.game_name.clone(),
                                        max_players: current_room.max_players,
                                        supports_authority: current_room.supports_authority,
                                        current_players,
                                        is_authority: current_room.authority_player
                                            == Some(response_player_id),
                                        lobby_state: current_room.lobby_state.clone(),
                                        ready_players,
                                        relay_type: current_room.relay_type.clone(),
                                        current_spectators: current_room.get_spectators(),
                                        ice_servers: server.pregather_ice_servers(
                                            &current_room,
                                            &response_player_id,
                                        ),
                                        reconnection_token: server
                                            .pre_issue_reconnection_token_for(
                                                &response_player_id,
                                                current_room.id,
                                            )
                                            .await,
                                    },
                                ))))
                            })
                        }),
                    )
                    .await;

                if !matches!(
                    initial_delivery,
                    Ok(crate::coordination::DeliveryOutcome::Delivered)
                ) {
                    tracing::warn!(%player_id, room_id = %room.id, ?initial_delivery, "Room join baseline could not be queued atomically");
                    self.rollback_unpublished_player_admission(
                        room.id,
                        *player_id,
                        admission_kind,
                        "undeliverable_room_join_baseline",
                    )
                    .await;
                    self.discard_pre_issued_reconnection_token(player_id).await;
                    self.connection_manager.rollback_prepared_room_assignment(
                        player_id,
                        room.id,
                        membership_stamp.epoch,
                    );
                    return;
                }

                let Some(current_room) = baseline_room
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take()
                else {
                    tracing::error!(%player_id, room_id = %room.id, "Delivered room baseline did not retain its source snapshot");
                    let mut player_snapshot =
                        room.players
                            .get(player_id)
                            .cloned()
                            .unwrap_or_else(|| PlayerInfo {
                                id: *player_id,
                                name: player_name.clone(),
                                is_authority: room.authority_player == Some(*player_id),
                                is_ready: false,
                                connected_at: chrono::Utc::now(),
                                connection_info: None,
                                epoch: None,
                                seq: None,
                                region_id: room.region_id.clone(),
                            });
                    player_snapshot.epoch = Some(membership_stamp.epoch);
                    player_snapshot.seq = Some(membership_stamp.seq);
                    let opening = Arc::new(ServerMessage::PlayerJoined {
                        player: player_snapshot.clone(),
                    });
                    self.terminate_room_generation_after_publication_failure(
                        *player_id,
                        room.id,
                        membership_stamp.epoch,
                        player_snapshot,
                        room.authority_player == Some(*player_id),
                        "join_baseline_snapshot",
                    )
                    .await;
                    if let Err(error) = self
                        .publish_join_lifecycle_fallback(room.id, *player_id, opening)
                        .await
                    {
                        tracing::error!(%player_id, room_id = %room.id, %error, "Failed to publish opening lifecycle before terminating inconsistent join");
                    }
                    return;
                };
                if self.config.app_id_allowlist_enabled {
                    if let (Some(application_id), Some(client_application_id)) =
                        (current_room.application_id, self.client_app_id(player_id))
                    {
                        if application_id == client_application_id {
                            self.mark_pending_room_application_claim_adopted(
                                current_room.id,
                                application_id,
                            );
                        }
                    }
                }
                let is_authority = current_room.authority_player == Some(*player_id);

                // Notify other players. This is a v3 wire snapshot, so it
                // carries the joiner's current incarnation epoch (Some after the
                // `assign_client_to_room` above bumped it); pre-v3 (v2)
                // recipients have it stripped per-recipient in `websocket::sending`.
                let player_info = PlayerInfo {
                    id: *player_id,
                    name: player_name,
                    is_authority,
                    is_ready: false,
                    connected_at: chrono::Utc::now(),
                    connection_info: None,
                    epoch: Some(membership_stamp.epoch),
                    seq: Some(membership_stamp.seq),
                    region_id: self.region_id().to_string(),
                };
                let committed = if current_room.lobby_state == LobbyState::Finalized {
                    self.publish_finalized_join_membership(
                        &current_room,
                        *player_id,
                        player_info,
                        room_event_guard,
                    )
                    .await
                } else {
                    let player_joined = Arc::new(ServerMessage::PlayerJoined {
                        player: player_info,
                    });
                    let replay_message = Arc::clone(&player_joined);
                    let committed = Arc::new(AtomicBool::new(false));
                    let committed_in_hook = Arc::clone(&committed);
                    let room_id_for_replay = room.id;
                    let joiner_id = *player_id;
                    let expected_epoch = membership_stamp.epoch;
                    let coordinator = Arc::clone(&self.message_coordinator);
                    let connection_manager = Arc::clone(&self.connection_manager);
                    let reconnection_manager = self.reconnection_manager.clone();
                    let drain = self.shutdown_drain_receiver();
                    let completion = self.message_coordinator.enqueue_room_event(
                        room_event_guard,
                        Box::new(move || {
                            Box::pin(async move {
                                let membership_is_current = || {
                                    connection_manager
                                        .current_relay_stamp_in_room(
                                            &joiner_id,
                                            &room_id_for_replay,
                                        )
                                        .is_some_and(|stamp| stamp.epoch == expected_epoch)
                                };
                                coordinator
                                    .broadcast_to_room_except_if_with_hook(
                                        &room_id_for_replay,
                                        &joiner_id,
                                        player_joined,
                                        &membership_is_current,
                                        drain,
                                        Box::new(move || {
                                            Box::pin(async move {
                                                if let Some(reconnection_manager) =
                                                    reconnection_manager
                                                {
                                                    reconnection_manager
                                                        .record_room_event(
                                                            &room_id_for_replay,
                                                            replay_message.as_ref(),
                                                        )
                                                        .await;
                                                }
                                                committed_in_hook.store(true, Ordering::Release);
                                            })
                                        }),
                                    )
                                    .await
                            })
                        }),
                    );
                    let _ = completion.await;
                    committed.load(Ordering::Acquire)
                };
                if !committed {
                    return;
                }

                // Check if room should transition to lobby state
                if current_room.should_enter_lobby() {
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
                    "Player joined room with process-local coordination"
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

    async fn create_room_with_generated_code(
        &self,
        player_id: &PlayerId,
        game_name: &str,
        player_name: &str,
        max_players: u8,
        supports_authority: bool,
    ) -> Result<
        (
            Room,
            crate::coordination::RoomEventMutationGuard,
            RoomAdmissionKind,
        ),
        JoinRoomError,
    > {
        for attempt in 1..=GENERATED_ROOM_CODE_MAX_ATTEMPTS {
            let candidate = self.generate_region_room_code();
            match self
                .join_room_with_coordination(
                    player_id,
                    game_name,
                    &candidate,
                    player_name,
                    max_players,
                    supports_authority,
                    RoomAdmissionIntent::CreateOnly,
                )
                .await
            {
                Ok(result) => {
                    if attempt > 1 {
                        self.metrics.increment_room_code_retry_successes();
                    }
                    return Ok(result);
                }
                Err(JoinRoomError::RoomCodeCollision) => {
                    self.metrics.increment_room_code_collisions();
                    if attempt == 1 {
                        self.metrics.increment_room_code_retry_operations();
                    }
                    if attempt < GENERATED_ROOM_CODE_MAX_ATTEMPTS {
                        tracing::warn!(
                            attempt,
                            max_attempts = GENERATED_ROOM_CODE_MAX_ATTEMPTS,
                            "Generated room code collided; retrying with a new candidate"
                        );
                    }
                }
                Err(error) => return Err(error),
            }
        }

        tracing::error!(
            attempts = GENERATED_ROOM_CODE_MAX_ATTEMPTS,
            "Generated room code retry budget exhausted"
        );
        self.metrics.increment_room_code_retry_exhaustions();
        Err(JoinRoomError::RoomCodeRetryExhausted {
            attempts: GENERATED_ROOM_CODE_MAX_ATTEMPTS,
        })
    }

    /// Leave room with coordination
    pub async fn leave_room(self: &Arc<Self>, player_id: &PlayerId) {
        let server = Arc::clone(self);
        let player_id = *player_id;
        let task = tokio::spawn(async move {
            server.leave_room_owned(player_id, true).await;
        });
        if let Err(error) = task.await {
            tracing::error!(%player_id, %error, "Owned room leave transaction failed");
        }
    }

    async fn leave_room_owned(self: Arc<Self>, player_id: PlayerId, notify_player: bool) {
        let player_id = &player_id;
        let Some(lifecycle) = self.connection_manager.client_lifecycle(player_id) else {
            self.leave_room_locked(player_id, notify_player).await;
            return;
        };
        let _lifecycle_guard = Arc::clone(&lifecycle).lock_owned().await;
        let effective_player_id = lifecycle.player_id();
        if effective_player_id != *player_id
            || !self
                .connection_manager
                .lifecycle_matches(&effective_player_id, &lifecycle)
        {
            return;
        }
        self.leave_room_locked(&effective_player_id, notify_player)
            .await;
    }

    /// Room departure after the caller acquired the connection lifecycle gate.
    pub(super) async fn leave_room_locked(&self, player_id: &PlayerId, notify_player: bool) {
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
        let room_event_guard = self
            .message_coordinator
            .lock_room_event_mutation(&room_id)
            .await;

        // Remove player from room in database
        match self
            .database
            .remove_player_from_room(&room_id, player_id)
            .await
        {
            Ok(Some(_)) => {
                self.pending_durable_player_detaches
                    .remove(&(room_id, *player_id));
            }
            Ok(None) => {
                // Persistence is authoritative. A stale local assignment can
                // survive a prior removal path, so converge routing and publish
                // the terminal event below even though this call did not
                // perform the durable removal itself.
                tracing::warn!(%player_id, %room_id, "Player persistence entry was already absent during leave");
                self.pending_durable_player_detaches
                    .remove(&(room_id, *player_id));
            }
            Err(e) => {
                if notify_player {
                    // A live caller can retry safely, so preserve its complete
                    // role when the durable outcome is unknown.
                    tracing::error!(%player_id, %room_id, error = %e, "Failed to remove player from room");
                    return;
                }

                // A physically dead socket cannot retain a local assignment:
                // that would prevent connection removal and make its pending
                // reconnect claim unclaimable. Publish the terminal route
                // transition below while leaving the durable member in place.
                // The generic durable-detach backlog retries removal whether or
                // not reconnect support is enabled; reconnect claims clear that
                // backlog before publishing a restored generation.
                tracing::warn!(%player_id, %room_id, error = %e, "Storage removal failed during disconnect; forcing local terminal teardown");
                self.pending_durable_player_detaches
                    .insert((room_id, *player_id), None);
                if let Err(authority_error) = self
                    .database
                    .request_room_authority(&room_id, player_id, false)
                    .await
                {
                    tracing::warn!(%player_id, %room_id, error = %authority_error, "Failed to clear disconnected authority after storage removal error");
                }
            }
        }

        // The player is out of the room: its pre-issued reconnection token
        // must not stay claimable. On the DISCONNECT path this is a no-op
        // (register_disconnection_for_reconnect already consumed the entry
        // before unregistration reaches this method); on a voluntary
        // LeaveRoom it is the discard that keeps the token map bounded by
        // currently-joined players.
        self.discard_pre_issued_reconnection_token(player_id).await;

        // Capture the terminal stamp and remove room routing atomically with
        // respect to relay allocation and recipient snapshots.
        let connection_manager = Arc::clone(&self.connection_manager);
        let departed_player = *player_id;
        let terminal_tail = match self
            .message_coordinator
            .unroute_local_client_with_tail(
                departed_player,
                room_id,
                Box::new(move || {
                    connection_manager
                        .clear_room_assignment_with_tail(&departed_player)
                        .map(|(delivery, stamp)| (delivery, stamp.epoch, stamp.seq))
                }),
            )
            .await
        {
            Ok(Some(tail)) => tail,
            Ok(None) => {
                tracing::error!(%player_id, %room_id, "Terminal unroute found no relay watermark; suppressing incomplete PlayerLeft");
                return;
            }
            Err(error) => {
                tracing::error!(%player_id, %room_id, %error, "Failed to atomically unroute departing player");
                return;
            }
        };
        // `players_left` measures the logical membership transition observed
        // by clients, not whether this invocation deleted a storage row. Count
        // exactly once at the successful terminal unroute; later durable
        // repair is storage bookkeeping only.
        self.metrics.increment_players_left();

        if self.is_draining() {
            tracing::info!(
                %player_id,
                %room_id,
                "Skipping normal room-leave traffic because shutdown drain started"
            );
            return;
        }

        // Queue the directed acknowledgement and room lifecycle delta as one
        // owned FIFO job before releasing the mutation gate. Once persistence
        // and routing say the member left, canceling this request cannot strand
        // peers without PlayerLeft. An unexpected disconnect deliberately skips
        // RoomLeft; the terminal close is that socket's observable transition.
        let player_left = ServerMessage::PlayerLeft {
            player_id: *player_id,
            epoch: Some(terminal_tail.0),
            final_seq: Some(terminal_tail.1),
        };
        let player_left = Arc::new(player_left);
        let replay_message = Arc::clone(&player_left);
        let room_id_for_replay = room_id;
        let drain = self.shutdown_drain_receiver();
        let acknowledgement_predicate_drain = drain.clone();
        let acknowledgement_delivery_drain = drain.clone();
        let broadcast_predicate_drain = drain.clone();
        let coordinator = Arc::clone(&self.message_coordinator);
        let reconnection_manager = self.reconnection_manager.clone();
        let committed = Arc::new(AtomicBool::new(false));
        let committed_in_hook = Arc::clone(&committed);
        let completion = self.message_coordinator.enqueue_room_event(
            room_event_guard,
            Box::new(move || {
                Box::pin(async move {
                    if notify_player {
                        let should_acknowledge = || !*acknowledgement_predicate_drain.borrow();
                        let _ = coordinator
                            .send_to_player_if(
                                &departed_player,
                                Arc::new(ServerMessage::RoomLeft),
                                &should_acknowledge,
                                acknowledgement_delivery_drain,
                            )
                            .await;
                    }

                    let should_broadcast = || !*broadcast_predicate_drain.borrow();
                    coordinator
                        .broadcast_to_room_except_if_with_hook(
                            &room_id_for_replay,
                            &departed_player,
                            player_left,
                            &should_broadcast,
                            drain,
                            Box::new(move || {
                                Box::pin(async move {
                                    if let Some(reconnection_manager) = reconnection_manager {
                                        reconnection_manager
                                            .record_room_event(
                                                &room_id_for_replay,
                                                replay_message.as_ref(),
                                            )
                                            .await;
                                    }
                                    committed_in_hook.store(true, Ordering::Release);
                                })
                            }),
                        )
                        .await
                })
            }),
        );
        let _ = completion.await;
        if !committed.load(Ordering::Acquire) {
            tracing::debug!(%player_id, %room_id, "Skipping departure follow-up because PlayerLeft did not commit");
            return;
        }

        if self.is_draining() {
            tracing::info!(
                %player_id,
                %room_id,
                "Skipping departure session re-plan because shutdown drain started"
            );
            return;
        }

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
            "Player left room with process-local coordination"
        );
    }

    /// Remove shutdown-drained membership without emitting normal leave traffic.
    ///
    /// During process shutdown every connected client is already getting the
    /// v3 `GoingAway` advisory and semantic 4000 close frame. Enqueuing normal
    /// `RoomLeft` / `PlayerLeft` traffic in between makes the shutdown contract
    /// noisy and can delay the close frame behind irrelevant room updates.
    pub(super) async fn remove_player_for_shutdown_drain(
        &self,
        player_id: &PlayerId,
        room_id: &RoomId,
    ) {
        let player_removed = match self
            .database
            .remove_player_from_room(room_id, player_id)
            .await
        {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(err) => {
                tracing::error!(%player_id, %room_id, error = %err, "Failed to remove draining player from room");
                false
            }
        };

        if player_removed {
            self.metrics.increment_players_left();
        }

        if self
            .connection_manager
            .clear_room_assignment(player_id)
            .is_none()
        {
            tracing::warn!(%player_id, %room_id, "Could not clear draining player's room assignment");
        }

        tracing::info!(
            %player_id,
            %room_id,
            instance_id = %self.instance_id,
            "Player removed from room during shutdown drain"
        );
    }

    /// Join a room with process-local admission coordination.
    async fn join_room_with_coordination(
        &self,
        player_id: &PlayerId,
        game_name: &str,
        room_code: &str,
        player_name: &str,
        max_players: u8,
        supports_authority: bool,
        admission_intent: RoomAdmissionIntent,
    ) -> Result<
        (
            Room,
            crate::coordination::RoomEventMutationGuard,
            RoomAdmissionKind,
        ),
        JoinRoomError,
    > {
        // Open-policy connections still carry a default AppContext for protocol
        // compatibility. Ownership and quotas apply only when allowlist enforcement
        // is enabled; otherwise newly-created rooms must remain unowned.
        let client_app_context = if self.config.app_id_allowlist_enabled {
            match self.client_app_context(player_id) {
                Some(app_context) => Some(app_context),
                None => {
                    return Err(anyhow::anyhow!(
                        "allowlisted room admission is missing application context"
                    )
                    .into())
                }
            }
        } else {
            None
        };
        let client_app_id = client_app_context.as_ref().map(|app| app.id);

        // Global lock order is room-code -> application-cap -> game-cap. Every
        // release happens in reverse order. Existing-room joins acquire the
        // application-cap lock only when claiming a legacy unowned room.
        let lock_key = format!("room_join:{game_name}:{room_code}");
        let lock_handle = self
            .distributed_lock
            .acquire(&lock_key, ROOM_JOIN_LOCK_TTL)
            .await?;
        let mut application_cap_lock: Option<LockHandle> = None;
        let mut game_cap_lock: Option<LockHandle> = None;
        let mut room_event_guard = None;

        // Try to join existing room or create new one
        let result = match self.database.get_room(game_name, room_code).await {
            Ok(Some(_)) if matches!(admission_intent, RoomAdmissionIntent::CreateOnly) => {
                Err(JoinRoomError::RoomCodeCollision)
            }
            Ok(Some(room_lane)) => {
                // Admission, routing publication, the directed RoomJoined
                // baseline, and PlayerJoined enqueue share one room mutation
                // transaction. Returning this owned guard to the handler keeps
                // a second join/leave/ready transition from observing the DB
                // member before its route and lifecycle event are published.
                room_event_guard = Some(
                    self.message_coordinator
                        .lock_room_event_mutation(&room_lane.id)
                        .await,
                );
                // The code lookup identifies only the mutation lane. Refresh
                // persistence after entering it so ownership, capacity, and
                // name checks never authorize from a stale pre-lock snapshot.
                let mut room = match self.database.get_room_by_id(&room_lane.id).await {
                    Ok(Some(room)) => room,
                    Ok(None) => {
                        let _ = self.distributed_lock.release(&lock_handle).await;
                        return Err(JoinRoomError::RoomNotFound);
                    }
                    Err(error) => {
                        let _ = self.distributed_lock.release(&lock_handle).await;
                        return Err(error.into());
                    }
                };

                if self.config.app_id_allowlist_enabled
                    && room
                        .application_id
                        .is_some_and(|owner| Some(owner) != client_app_id)
                {
                    Err(JoinRoomError::RoomNotFound)
                } else if let Err(reason) =
                    validation::validate_player_name_uniqueness(player_name, &room.players)
                {
                    Err(anyhow::anyhow!(reason).into())
                } else {
                    let effective_max_players = client_app_context
                        .as_ref()
                        .and_then(|app| app.max_players_per_room)
                        .map_or(room.max_players, |app_limit| {
                            room.max_players.min(app_limit)
                        });
                    if room.players.len() >= usize::from(effective_max_players) {
                        Err(JoinRoomError::RoomFull)
                    } else {
                        let claimed_application =
                            room.application_id.is_none() && client_app_id.is_some();
                        let application_claim_rollback = if claimed_application {
                            client_app_id.map(|application_id| PendingApplicationClaimRollback {
                                application_id,
                            })
                        } else {
                            None
                        };
                        if let Some(app_id) = client_app_id {
                            if claimed_application {
                                if let Some(app_limit) =
                                    client_app_context.as_ref().and_then(|app| app.max_rooms)
                                {
                                    match self
                                        .acquire_application_room_cap_lock(app_id, app_limit)
                                        .await
                                    {
                                        Ok(lock) => application_cap_lock = Some(lock),
                                        Err(error) => {
                                            let _ =
                                                self.distributed_lock.release(&lock_handle).await;
                                            return Err(error);
                                        }
                                    }
                                }
                                if let Err(error) =
                                    self.record_room_application(&room.id, app_id).await
                                {
                                    self.release_cap_lock(&application_cap_lock).await;
                                    let _ = self.distributed_lock.release(&lock_handle).await;
                                    return Err(error.into());
                                }
                                room.application_id = Some(app_id);
                            } else if room.application_id == Some(app_id) {
                                self.cache_room_application(&room.id, app_id);
                            }
                        }

                        let player_info = PlayerInfo {
                            id: *player_id,
                            name: player_name.to_string(),
                            is_authority: false,
                            is_ready: false,
                            connected_at: chrono::Utc::now(),
                            connection_info: None,
                            // Room-state record (stored in the DB + `room.players`),
                            // not a wire snapshot: the v3 epoch is filled at
                            // snapshot-send time, so this stays `None`.
                            epoch: None,
                            seq: None,
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
                                Ok((
                                    room,
                                    RoomAdmissionKind::Existing {
                                        application_claim_rollback,
                                    },
                                ))
                            }
                            Ok(false) => {
                                if let Some(rollback) = application_claim_rollback.as_ref() {
                                    match self
                                        .rollback_or_queue_room_application_claim(
                                            &room.id, player_id, rollback,
                                        )
                                        .await
                                    {
                                        Ok(()) => Err(JoinRoomError::RoomFull),
                                        Err(error) => Err(error),
                                    }
                                } else {
                                    Err(JoinRoomError::RoomFull)
                                }
                            }
                            Err(error) => {
                                if let Some(rollback) = application_claim_rollback.as_ref() {
                                    match self
                                        .rollback_or_queue_room_application_claim(
                                            &room.id, player_id, rollback,
                                        )
                                        .await
                                    {
                                        Ok(()) => Err(error.into()),
                                        Err(rollback_error) => Err(rollback_error),
                                    }
                                } else {
                                    Err(error.into())
                                }
                            }
                        }
                    }
                }
            }
            Ok(None) => {
                if self.is_draining() {
                    Err(JoinRoomError::ServerDraining)
                } else {
                    if let Some(app_limit) = client_app_context
                        .as_ref()
                        .and_then(|app| app.max_players_per_room)
                        .filter(|limit| max_players > *limit)
                    {
                        let _ = self.distributed_lock.release(&lock_handle).await;
                        return Err(JoinRoomError::MaxPlayersPerApplicationExceeded(
                            MaxPlayersPerApplicationExceededError {
                                requested: max_players,
                                limit: app_limit,
                            },
                        ));
                    }

                    let creation_result = async {
                        if let (Some(app_id), Some(app_limit)) = (
                            client_app_id,
                            client_app_context.as_ref().and_then(|app| app.max_rooms),
                        ) {
                            application_cap_lock = Some(
                                self.acquire_application_room_cap_lock(app_id, app_limit)
                                    .await?,
                            );
                        }

                        let cap_lock_key = format!("game_room_cap:{game_name}");
                        game_cap_lock = Some(
                            self.distributed_lock
                                .acquire(&cap_lock_key, GAME_ROOM_CAP_LOCK_TTL)
                                .await
                                .map_err(|error| {
                                    tracing::error!(%game_name, %error, "Failed to acquire game room-cap lock");
                                    self.metrics.increment_room_cap_lock_failures();
                                    JoinRoomError::Internal(error)
                                })?,
                        );
                        self.metrics.increment_room_cap_lock_acquisitions();

                        if self.is_draining() {
                            return Err(JoinRoomError::ServerDraining);
                        }
                        let current_room_count =
                            self.database.get_game_room_count(game_name).await?;
                        if current_room_count >= self.config.max_rooms_per_game {
                            self.metrics.increment_room_cap_denials();
                            return Err(JoinRoomError::MaxRoomsPerGameExceeded(
                                MaxRoomsPerGameExceededError {
                                    game_name: game_name.to_string(),
                                    current: current_room_count,
                                    limit: self.config.max_rooms_per_game,
                                },
                            ));
                        }

                        let relay_type = self.resolve_relay_type(game_name);
                        let region_id = self.region_id().to_string();
                        let room = match self
                            .database
                            .create_room_classified(
                                game_name.to_string(),
                                Some(room_code.to_string()),
                                max_players,
                                supports_authority,
                                *player_id,
                                relay_type,
                                region_id,
                                client_app_id,
                            )
                            .await
                        {
                            Ok(room) => room,
                            Err(error @ CreateRoomError::RoomCodeCollision { .. }) => {
                                if matches!(admission_intent, RoomAdmissionIntent::CreateOnly) {
                                    return Err(JoinRoomError::RoomCodeCollision);
                                }
                                return Err(anyhow::Error::new(error).into());
                            }
                            Err(CreateRoomError::Storage(error)) => {
                                // A source-compatible adapter may still expose
                                // uniqueness races through its legacy untyped
                                // `create_room` result. For automatic creation,
                                // confirm whether the candidate became occupied
                                // before treating that error as unrelated
                                // storage failure. Explicit requests retain the
                                // adapter's original error semantics.
                                if matches!(admission_intent, RoomAdmissionIntent::CreateOnly) {
                                    if let Ok(Some(room)) =
                                        self.database.get_room(game_name, room_code).await
                                    {
                                        if room.players.contains_key(player_id) {
                                            // The write may have committed even
                                            // though the legacy adapter returned
                                            // an error (for example, transport
                                            // loss after acknowledgement). Adopt
                                            // that exact creator-owned result so
                                            // retrying cannot orphan a room or
                                            // consume quota twice.
                                            room
                                        } else {
                                            return Err(JoinRoomError::RoomCodeCollision);
                                        }
                                    } else {
                                        return Err(error.into());
                                    }
                                } else {
                                    return Err(error.into());
                                }
                            }
                        };

                        if self.is_draining() {
                            match self.database.delete_room(&room.id).await {
                                Ok(true) => {}
                                Ok(false) => {
                                    tracing::warn!(
                                        room_id = %room.id,
                                        "Room created during shutdown drain was already absent during rollback"
                                    );
                                }
                                Err(error) => {
                                    tracing::error!(
                                        room_id = %room.id,
                                        %error,
                                        "Failed to roll back room created during shutdown drain"
                                    );
                                    return Err(error.into());
                                }
                            }
                            return Err(JoinRoomError::ServerDraining);
                        }

                        Ok(room)
                    }
                    .await;

                    match creation_result {
                        Ok(mut room) => {
                            // The database generates a new room id, so no other
                            // room-scoped publisher can address its lane before
                            // creation. Retain the lane through routing and
                            // lifecycle publication.
                            room_event_guard = Some(
                                self.message_coordinator
                                    .lock_room_event_mutation(&room.id)
                                    .await,
                            );
                            self.metrics.increment_rooms_created();
                            self.metrics.increment_players_joined();
                            if let Some(app_id) = client_app_id {
                                self.cache_room_application(&room.id, app_id);
                            }
                            if let Err(error) = self
                                .database
                                .update_player_name(&room.id, player_id, player_name)
                                .await
                            {
                                tracing::warn!(%player_id, %error, "Failed to update creator name");
                            } else if let Some(creator_info) = room.players.get_mut(player_id) {
                                creator_info.name = player_name.to_string();
                            }
                            let application_claim_rollback = client_app_id.map(|application_id| {
                                PendingApplicationClaimRollback { application_id }
                            });
                            Ok((
                                room,
                                RoomAdmissionKind::Created {
                                    application_claim_rollback,
                                },
                            ))
                        }
                        Err(error) => Err(error),
                    }
                }
            }
            Err(e) => Err(e.into()),
        };

        self.release_cap_lock(&game_cap_lock).await;
        self.release_cap_lock(&application_cap_lock).await;
        let _ = self.distributed_lock.release(&lock_handle).await;
        let (room, admission_kind) = result?;
        let guard = room_event_guard.ok_or_else(|| {
            anyhow::anyhow!("successful room admission lost its room publication guard")
        })?;
        Ok((room, guard, admission_kind))
    }

    async fn release_cap_lock(&self, cap_lock: &Option<LockHandle>) {
        if let Some(lock) = cap_lock {
            let _ = self.distributed_lock.release(lock).await;
        }
    }

    async fn acquire_application_room_cap_lock(
        &self,
        app_id: uuid::Uuid,
        app_limit: u32,
    ) -> Result<LockHandle, JoinRoomError> {
        let cap_lock_key = format!("application_room_cap:{app_id}");
        let lock = self
            .distributed_lock
            .acquire(&cap_lock_key, APPLICATION_ROOM_CAP_LOCK_TTL)
            .await
            .map_err(|error| {
                tracing::error!(%app_id, %error, "Failed to acquire application room-cap lock");
                JoinRoomError::Internal(error)
            })?;
        let current = match self.database.get_application_room_count(&app_id).await {
            Ok(current) => current,
            Err(error) => {
                tracing::error!(%app_id, %error, "Failed to count application rooms");
                let _ = self.distributed_lock.release(&lock).await;
                return Err(JoinRoomError::Internal(error));
            }
        };
        let limit = usize::try_from(app_limit).unwrap_or(usize::MAX);
        if current >= limit {
            let _ = self.distributed_lock.release(&lock).await;
            return Err(JoinRoomError::MaxRoomsPerApplicationExceeded(
                MaxRoomsPerApplicationExceededError { current, limit },
            ));
        }
        Ok(lock)
    }

    pub(super) async fn rollback_room_application_claim_if_unchanged(
        &self,
        room_id: &RoomId,
        rollback: &PendingApplicationClaimRollback,
    ) -> Result<(), JoinRoomError> {
        if self
            .database
            .clear_room_application_id_if_matches(room_id, rollback.application_id)
            .await?
        {
            self.room_applications.remove(room_id);
        }
        Ok(())
    }

    pub(super) fn mark_pending_room_application_claim_adopted(
        &self,
        room_id: RoomId,
        application_id: uuid::Uuid,
    ) {
        for mut entry in self.pending_durable_player_detaches.iter_mut() {
            if entry.key().0 == room_id
                && entry
                    .value()
                    .as_ref()
                    .is_some_and(|rollback| rollback.application_id == application_id)
            {
                *entry.value_mut() = None;
            }
        }
    }

    async fn rollback_or_queue_room_application_claim(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        rollback: &PendingApplicationClaimRollback,
    ) -> Result<(), JoinRoomError> {
        match self
            .rollback_room_application_claim_if_unchanged(room_id, rollback)
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => {
                self.pending_durable_player_detaches
                    .insert((*room_id, *player_id), Some(rollback.clone()));
                Err(error)
            }
        }
    }

    async fn join_would_create_room_while_draining(
        &self,
        game_name: &str,
        room_code: Option<&str>,
    ) -> bool {
        if !self.is_draining() {
            return false;
        }

        let Some(room_code) = room_code else {
            return true;
        };

        if validation::validate_room_code_with_config(room_code, &self.protocol_config).is_err() {
            return false;
        }

        let room_code = room_code.to_uppercase();
        matches!(
            self.database.get_room(game_name, &room_code).await,
            Ok(None)
        )
    }

    async fn reject_join_for_shutdown_drain(&self, player_id: &PlayerId) {
        match self
            .message_coordinator
            .try_send_to_player(
                player_id,
                Arc::new(ServerMessage::RoomJoinFailed {
                    reason: "Server is draining for shutdown".to_string(),
                    error_code: Some(crate::protocol::ErrorCode::ServerDraining),
                }),
            )
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!(
                    %player_id,
                    "Shutdown drain room-creation rejection skipped: queue full or connection gone"
                );
            }
            Err(err) => {
                tracing::debug!(
                    %player_id,
                    error = %err,
                    "Shutdown drain room-creation rejection failed"
                );
            }
        }
    }
}
