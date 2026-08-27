use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::ProtocolConfig;
use crate::coordination::{MessageCoordinator, RoomOperationCoordinatorTrait};
use crate::database::GameDatabase;
use crate::protocol::{
    validation, ErrorCode, PlayerId, PlayerInfo, RoomId, ServerMessage, SpectatorInfo,
    SpectatorJoinedPayload, SpectatorStateChangeReason,
};
use crate::rate_limit::RoomRateLimiter;
use crate::reconnection::ReconnectionManager;
use tokio::sync::watch;

use super::ConnectionManager;

#[cfg(test)]
use crate::protocol::Room;

#[derive(Clone)]
pub(crate) struct SpectatorService {
    spectator_rooms: Arc<DashMap<PlayerId, RoomId>>,
    /// Durable rows created by a spectator admission that never became
    /// externally visible, but whose compensating delete failed. These are
    /// deliberately separate from `spectator_rooms`: the client must not look
    /// joined while maintenance retains enough identity to repair storage.
    pending_unpublished_detaches: Arc<DashMap<(RoomId, PlayerId), ()>>,
    database: Arc<dyn GameDatabase>,
    /// Readiness is coordinator state, not room-record state, until finalize
    /// writes it through. The spectator snapshot reads it from here so it
    /// reports the same lobby the members themselves see.
    room_coordinator: Arc<dyn RoomOperationCoordinatorTrait>,
    message_coordinator: Arc<dyn MessageCoordinator>,
    room_applications: Arc<DashMap<RoomId, Uuid>>,
    protocol_config: ProtocolConfig,
    /// Records this service's room-uniform broadcasts (`NewSpectatorJoined` /
    /// `SpectatorDisconnected`) for reconnection replay; `None` when
    /// reconnection is disabled.
    reconnection_manager: Option<Arc<ReconnectionManager>>,
    connection_manager: Arc<ConnectionManager>,
    app_id_allowlist_enabled: bool,
    rate_limiter: Arc<RoomRateLimiter>,
}

#[derive(Debug)]
pub(crate) struct SpectatorError {
    pub message: String,
    pub code: Option<ErrorCode>,
}

impl SpectatorError {
    fn new(message: impl Into<String>, code: Option<ErrorCode>) -> Self {
        Self {
            message: message.into(),
            code,
        }
    }
}

impl SpectatorService {
    pub(crate) fn new(
        database: Arc<dyn GameDatabase>,
        room_coordinator: Arc<dyn RoomOperationCoordinatorTrait>,
        message_coordinator: Arc<dyn MessageCoordinator>,
        room_applications: Arc<DashMap<RoomId, Uuid>>,
        protocol_config: ProtocolConfig,
        reconnection_manager: Option<Arc<ReconnectionManager>>,
        connection_manager: Arc<ConnectionManager>,
        app_id_allowlist_enabled: bool,
        rate_limiter: Arc<RoomRateLimiter>,
    ) -> Self {
        Self {
            spectator_rooms: Arc::new(DashMap::new()),
            pending_unpublished_detaches: Arc::new(DashMap::new()),
            database,
            room_coordinator,
            message_coordinator,
            room_applications,
            protocol_config,
            reconnection_manager,
            connection_manager,
            app_id_allowlist_enabled,
            rate_limiter,
        }
    }

    async fn rollback_unpublished_spectator_join(&self, player_id: &PlayerId, room_id: &RoomId) {
        self.spectator_rooms.remove(player_id);
        self.connection_manager
            .rollback_delivery_generation(player_id)
            .await;
        match self
            .database
            .remove_spectator_from_room(room_id, player_id)
            .await
        {
            Ok(_) => {
                self.pending_unpublished_detaches
                    .remove(&(*room_id, *player_id));
            }
            Err(err) => {
                self.pending_unpublished_detaches
                    .insert((*room_id, *player_id), ());
                warn!(%player_id, %room_id, error = %err, "Failed to roll back unpublished spectator join; queued durable repair");
            }
        }
    }

    #[cfg(test)]
    pub(crate) async fn join(
        &self,
        player_id: &PlayerId,
        game_name: String,
        room_code: String,
        spectator_name: String,
    ) -> Result<(), SpectatorError> {
        self.join_operation(player_id, None, game_name, room_code, spectator_name)
            .await
    }

    pub(crate) async fn join_operation(
        &self,
        player_id: &PlayerId,
        operation_id: Option<crate::protocol::RoomOperationId>,
        game_name: String,
        room_code: String,
        spectator_name: String,
    ) -> Result<(), SpectatorError> {
        let service = self.clone();
        let player_id = *player_id;
        tokio::spawn(async move {
            service
                .join_owned(
                    player_id,
                    operation_id,
                    game_name,
                    room_code,
                    spectator_name,
                )
                .await
        })
        .await
        .map_err(|error| {
            SpectatorError::new(
                format!("Spectator join transaction failed: {error}"),
                Some(ErrorCode::SpectatorJoinFailed),
            )
        })?
    }

    async fn join_owned(
        self,
        player_id: PlayerId,
        operation_id: Option<crate::protocol::RoomOperationId>,
        game_name: String,
        room_code: String,
        spectator_name: String,
    ) -> Result<(), SpectatorError> {
        let player_id = &player_id;
        let Some(lifecycle) = self.connection_manager.client_lifecycle(player_id) else {
            return Err(SpectatorError::new(
                "Connection is no longer active",
                Some(ErrorCode::SpectatorJoinFailed),
            ));
        };
        let _lifecycle_guard = Arc::clone(&lifecycle).lock_owned().await;
        if lifecycle.player_id() != *player_id
            || !self
                .connection_manager
                .lifecycle_matches(player_id, &lifecycle)
        {
            return Err(SpectatorError::new(
                "Connection identity changed",
                Some(ErrorCode::SpectatorJoinFailed),
            ));
        }
        if let Err(error) = self.rate_limiter.check_join_attempt(player_id).await {
            return Err(SpectatorError::new(
                error.to_string(),
                Some(ErrorCode::RateLimitExceeded),
            ));
        }

        if self.connection_manager.get_client_room(player_id).is_some() {
            return Err(SpectatorError::new(
                "Leave the current player room before joining as a spectator",
                Some(ErrorCode::SpectatorNotAllowed),
            ));
        }
        if self.spectator_rooms.contains_key(player_id) {
            return Err(SpectatorError::new(
                "Already spectating a room",
                Some(ErrorCode::SpectatorJoinFailed),
            ));
        }
        // Spectator names deliberately get charset validation but NO
        // uniqueness contract, unlike player names. Spectator capacity is
        // unlimited by default and spectator identity is non-authoritative
        // display metadata; enforcing canonical uniqueness here would hand any
        // anonymous client a name-squatting denial-of-service lever against an
        // unbounded admission surface ("Guest" joined first, so no other
        // "Guest" ever can), and extending uniqueness across the room's
        // players would let a spectator pre-claim a name and block the real
        // player from joining. See `docs/concepts/spectator-mode.md`.
        if let Err(err) =
            validation::validate_player_name_with_config(&spectator_name, &self.protocol_config)
        {
            return Err(SpectatorError::new(err, Some(ErrorCode::InvalidPlayerName)));
        }
        if let Err(err) =
            validation::validate_game_name_with_config(&game_name, &self.protocol_config)
        {
            return Err(SpectatorError::new(err, Some(ErrorCode::InvalidGameName)));
        }
        if let Err(err) =
            validation::validate_room_code_with_config(&room_code, &self.protocol_config)
        {
            return Err(SpectatorError::new(err, Some(ErrorCode::InvalidRoomCode)));
        }
        let room_code = room_code.to_ascii_uppercase();

        let room = match self.database.get_room(&game_name, &room_code).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                return Err(SpectatorError::new(
                    "Room not found",
                    Some(ErrorCode::RoomNotFound),
                ))
            }
            Err(err) => {
                warn!("Failed to fetch room for spectator: {err}");
                return Err(SpectatorError::new(
                    "Storage error",
                    Some(ErrorCode::StorageError),
                ));
            }
        };

        let room_event_guard = self
            .message_coordinator
            .lock_room_event_mutation(&room.id)
            .await;
        // The lookup by code only identifies the room lane. Capacity and every
        // baseline field come from a fresh read inside that lane so an admission
        // never publishes a stale pre-lock room snapshot.
        let room = match self.database.get_room_by_id(&room.id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                return Err(SpectatorError::new(
                    "Room not found",
                    Some(ErrorCode::RoomNotFound),
                ))
            }
            Err(err) => {
                warn!(room_id = %room.id, error = %err, "Failed to refresh room for spectator");
                return Err(SpectatorError::new(
                    "Storage error",
                    Some(ErrorCode::StorageError),
                ));
            }
        };

        // Room persistence is authoritative. The process-local room/app map is
        // only a relay cache and may be empty after restart or cache loss.
        if self.app_id_allowlist_enabled {
            let Some(client_app_id) = self.connection_manager.app_id(player_id) else {
                return Err(SpectatorError::new(
                    "Room not found",
                    Some(ErrorCode::RoomNotFound),
                ));
            };
            if room
                .application_id
                .is_some_and(|owner| owner != client_app_id)
            {
                return Err(SpectatorError::new(
                    "Room not found",
                    Some(ErrorCode::RoomNotFound),
                ));
            }
            if let Some(owner) = room.application_id {
                self.room_applications.insert(room.id, owner);
            }
        }

        if !room.can_spectate() {
            return Err(SpectatorError::new(
                "Spectator limit reached",
                Some(ErrorCode::TooManySpectators),
            ));
        }

        let spectator = SpectatorInfo {
            id: *player_id,
            name: spectator_name.clone(),
            connected_at: chrono::Utc::now(),
        };

        match self
            .database
            .add_spectator_to_room(&room.id, spectator.clone())
            .await
        {
            Ok(true) => {
                self.connection_manager
                    .advance_delivery_generation(player_id)
                    .await;
                let recipient_is_v3 = self.connection_manager.supports_v3(player_id);
                let current_room = match self.database.get_room_by_id(&room.id).await {
                    Ok(Some(current_room)) => current_room,
                    Ok(None) => {
                        self.rollback_unpublished_spectator_join(player_id, &room.id)
                            .await;
                        return Err(SpectatorError::new(
                            "Room not found",
                            Some(ErrorCode::RoomNotFound),
                        ));
                    }
                    Err(err) => {
                        warn!(room_id = %room.id, error = %err, "Failed to build fresh spectator baseline");
                        self.rollback_unpublished_spectator_join(player_id, &room.id)
                            .await;
                        return Err(SpectatorError::new(
                            "Storage error",
                            Some(ErrorCode::StorageError),
                        ));
                    }
                };
                let routed_player_ids = match self
                    .message_coordinator
                    .routed_player_ids(&room.id)
                    .await
                {
                    Ok(routed) => routed.map(|ids| ids.into_iter().collect::<HashSet<_>>()),
                    Err(err) => {
                        warn!(room_id = %room.id, error = %err, "Failed to resolve published players for spectator baseline");
                        self.rollback_unpublished_spectator_join(player_id, &room.id)
                            .await;
                        return Err(SpectatorError::new(
                            "Storage error",
                            Some(ErrorCode::StorageError),
                        ));
                    }
                };
                let ready_players = crate::server::ready_state::snapshot_ready_players(
                    &current_room,
                    self.room_coordinator.as_ref(),
                )
                .await;
                let current_players: Vec<PlayerInfo> = current_room
                    .players
                    .values()
                    .cloned()
                    .filter_map(|mut player| {
                        // Same derivation as `RoomJoined` / `Reconnected`: the
                        // stored flag is only written at finalize.
                        player.is_ready = ready_players.contains(&player.id);
                        let relay_stamp = self
                            .connection_manager
                            .current_relay_stamp_in_room(&player.id, &room.id);
                        let is_published = routed_player_ids.as_ref().map_or_else(
                            || relay_stamp.is_some(),
                            |routed| routed.contains(&player.id),
                        );
                        if !is_published {
                            return None;
                        }
                        player.epoch = None;
                        player.seq = None;
                        if !recipient_is_v3 {
                            return Some(player);
                        }
                        let stamp = relay_stamp?;
                        player.epoch = Some(stamp.epoch);
                        player.seq = Some(stamp.seq);
                        Some(player)
                    })
                    .collect();
                // `current_room` was fetched after the successful insert while
                // this room's mutation guard was held, so it is the
                // authoritative roster for both the baseline and broadcast.
                // Never turn a storage read failure into an empty roster.
                let spectator_snapshot = current_room.get_spectators();

                let join_reason = SpectatorStateChangeReason::Joined;
                let (_drain_tx, drain) = watch::channel(false);
                let should_deliver = || true;
                let baseline_delivered = self
                    .message_coordinator
                    .send_to_player_if(
                        player_id,
                        Arc::new(
                            (ServerMessage::SpectatorJoined(Box::new(SpectatorJoinedPayload {
                                room_id: current_room.id,
                                room_code: current_room.code.clone(),
                                spectator_id: *player_id,
                                game_name: current_room.game_name.clone(),
                                current_players,
                                current_spectators: spectator_snapshot.clone(),
                                lobby_state: current_room.lobby_state.clone(),
                                reason: Some(join_reason.clone()),
                            })))
                            .correlate_room_operation(operation_id),
                        ),
                        &should_deliver,
                        drain,
                    )
                    .await
                    .unwrap_or(false);
                if !baseline_delivered {
                    self.rollback_unpublished_spectator_join(player_id, &room.id)
                        .await;
                    return Err(SpectatorError::new(
                        "Spectator join response was not deliverable",
                        Some(ErrorCode::SpectatorJoinFailed),
                    ));
                }

                if let Some(previous_room) = self.spectator_rooms.insert(*player_id, room.id) {
                    warn!(
                        %player_id,
                        room_id = %previous_room,
                        new_room_id = %room.id,
                        "Spectator was already mapped to a different room; overwriting"
                    );
                }

                let notification = Arc::new(ServerMessage::NewSpectatorJoined {
                    spectator: spectator.clone(),
                    current_spectators: spectator_snapshot.clone(),
                    reason: Some(join_reason),
                });
                let replay_notification = Arc::clone(&notification);
                let committed = Arc::new(AtomicBool::new(false));
                let committed_in_hook = Arc::clone(&committed);
                let room_id_for_replay = room.id;
                let coordinator = Arc::clone(&self.message_coordinator);
                let reconnection_manager = self.reconnection_manager.clone();
                let completion = self.message_coordinator.enqueue_room_event(
                    room_event_guard,
                    Box::new(move || {
                        Box::pin(async move {
                            coordinator
                                .broadcast_to_room_with_hook(
                                    &room_id_for_replay,
                                    notification,
                                    Box::new(move || {
                                        Box::pin(async move {
                                            if let Some(reconnection_manager) = reconnection_manager
                                            {
                                                reconnection_manager
                                                    .record_room_event(
                                                        &room_id_for_replay,
                                                        replay_notification.as_ref(),
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
                    return Ok(());
                }

                info!(
                    %player_id,
                    spectator_name,
                    room_code,
                    "Spectator joined room"
                );

                Ok(())
            }
            Ok(false) => Err(SpectatorError::new(
                "Failed to join as spectator",
                Some(ErrorCode::SpectatorJoinFailed),
            )),
            Err(err) => {
                warn!("Storage error adding spectator: {err}");
                Err(SpectatorError::new(
                    "Storage error",
                    Some(ErrorCode::StorageError),
                ))
            }
        }
    }

    pub(crate) async fn leave(&self, player_id: &PlayerId) -> Result<(), SpectatorError> {
        if !self.is_spectating(player_id) {
            return Err(SpectatorError::new(
                "You are not currently spectating a room",
                Some(ErrorCode::NotASpectator),
            ));
        }
        if self
            .detach(player_id, SpectatorStateChangeReason::VoluntaryLeave)
            .await
        {
            Ok(())
        } else {
            Err(SpectatorError::new(
                "Failed to leave spectator room",
                Some(ErrorCode::StorageError),
            ))
        }
    }

    pub(crate) async fn leave_operation(
        &self,
        player_id: &PlayerId,
        operation_id: crate::protocol::RoomOperationId,
    ) -> Result<(), SpectatorError> {
        if !self.is_spectating(player_id) {
            return Err(SpectatorError::new(
                "You are not currently spectating a room",
                Some(ErrorCode::NotASpectator),
            ));
        }
        if self
            .detach_operation(
                player_id,
                SpectatorStateChangeReason::VoluntaryLeave,
                operation_id,
            )
            .await
        {
            Ok(())
        } else {
            Err(SpectatorError::new(
                "Failed to leave spectator room",
                Some(ErrorCode::StorageError),
            ))
        }
    }

    pub(crate) fn is_spectating(&self, player_id: &PlayerId) -> bool {
        self.spectator_rooms.contains_key(player_id)
    }

    /// Resolve the room occupied by a spectator connection, if any.
    pub(crate) fn spectator_room(&self, player_id: &PlayerId) -> Option<RoomId> {
        self.spectator_rooms
            .get(player_id)
            .map(|entry| *entry.value())
    }

    /// Converge local spectator roles whose authoritative room was removed by
    /// inactive-room cleanup. Storage errors retain the role for a later tick;
    /// definitive absence clears it and tells a live client the room closed.
    pub(crate) async fn prune_missing_rooms(&self, drain: watch::Receiver<bool>) -> usize {
        let mut candidates = HashMap::<RoomId, Vec<PlayerId>>::new();
        for entry in self.spectator_rooms.iter() {
            candidates
                .entry(*entry.value())
                .or_default()
                .push(*entry.key());
        }
        let mut missing = Vec::<(RoomId, PlayerId)>::new();
        for (room_id, player_ids) in candidates {
            match self.database.get_room_by_id(&room_id).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    missing.extend(player_ids.into_iter().map(|player_id| (room_id, player_id)));
                }
                Err(err) => {
                    warn!(%room_id, error = %err, "Failed to check spectator room during prune");
                }
            }
        }

        let detachments = missing.into_iter().map(|(room_id, player_id)| {
            let drain = drain.clone();
            async move {
                self.detach_expected(
                    &player_id,
                    SpectatorStateChangeReason::RoomClosed,
                    Some(room_id),
                    drain,
                    None,
                    None,
                )
                .await
            }
        });
        futures_util::future::join_all(detachments)
            .await
            .into_iter()
            .filter(|detached| *detached)
            .count()
    }

    /// Retry durable detach for spectator roles whose physical connection has
    /// already gone. A disconnect-time storage error deliberately leaves the
    /// local role indexed so this maintenance sweep can converge persistence
    /// and publish the terminal roster once storage recovers.
    pub(crate) async fn retry_disconnected_detaches(&self) -> usize {
        let pending_unpublished: Vec<(RoomId, PlayerId)> = self
            .pending_unpublished_detaches
            .iter()
            .map(|entry| *entry.key())
            .collect();
        let mut detached = 0_usize;
        for (room_id, player_id) in pending_unpublished {
            let _guard = self
                .message_coordinator
                .lock_room_event_mutation(&room_id)
                .await;
            if self.spectator_room(&player_id) == Some(room_id) {
                // A later admission published the same durable identity. It is
                // no longer an unpublished rollback and must not be deleted.
                self.pending_unpublished_detaches
                    .remove(&(room_id, player_id));
                continue;
            }
            match self
                .database
                .remove_spectator_from_room(&room_id, &player_id)
                .await
            {
                Ok(_) => {
                    if self
                        .pending_unpublished_detaches
                        .remove(&(room_id, player_id))
                        .is_some()
                    {
                        detached = detached.saturating_add(1);
                    }
                }
                Err(err) => {
                    warn!(%player_id, %room_id, error = %err, "Failed to retry unpublished spectator rollback");
                }
            }
        }

        let candidates: Vec<PlayerId> = self
            .spectator_rooms
            .iter()
            .filter(|entry| !self.connection_manager.has_client(entry.key()))
            .map(|entry| *entry.key())
            .collect();
        for player_id in candidates {
            if self
                .detach(&player_id, SpectatorStateChangeReason::Disconnected)
                .await
            {
                detached = detached.saturating_add(1);
            }
        }
        detached
    }

    pub(crate) async fn detach(
        &self,
        player_id: &PlayerId,
        reason: SpectatorStateChangeReason,
    ) -> bool {
        let (drain_tx, drain_rx) = watch::channel(false);
        self.detach_expected(player_id, reason, None, drain_rx, Some(drain_tx), None)
            .await
    }

    pub(crate) async fn detach_operation(
        &self,
        player_id: &PlayerId,
        reason: SpectatorStateChangeReason,
        operation_id: crate::protocol::RoomOperationId,
    ) -> bool {
        let (drain_tx, drain_rx) = watch::channel(false);
        self.detach_expected(
            player_id,
            reason,
            None,
            drain_rx,
            Some(drain_tx),
            Some(operation_id),
        )
        .await
    }

    async fn detach_expected(
        &self,
        player_id: &PlayerId,
        reason: SpectatorStateChangeReason,
        expected_room: Option<RoomId>,
        drain: watch::Receiver<bool>,
        drain_owner: Option<watch::Sender<bool>>,
        operation_id: Option<crate::protocol::RoomOperationId>,
    ) -> bool {
        let lifecycle = self.connection_manager.client_lifecycle(player_id);
        let lifecycle_guard = match lifecycle.as_ref() {
            Some(lifecycle) => Some(Arc::clone(lifecycle).lock_owned().await),
            None => None,
        };
        if let Some(lifecycle) = &lifecycle {
            let effective_player_id = lifecycle.player_id();
            if effective_player_id != *player_id
                || !self
                    .connection_manager
                    .lifecycle_matches(&effective_player_id, lifecycle)
            {
                return false;
            }
        }
        if expected_room.is_some_and(|room_id| self.spectator_room(player_id) != Some(room_id)) {
            return false;
        }

        self.spawn_detach(
            *player_id,
            reason,
            true,
            drain,
            lifecycle_guard,
            drain_owner,
            operation_id,
        )
        .await
    }

    pub(crate) async fn detach_if(
        &self,
        player_id: &PlayerId,
        reason: SpectatorStateChangeReason,
        should_send: &(dyn Fn() -> bool + Send + Sync),
        drain: watch::Receiver<bool>,
    ) -> bool {
        self.spawn_detach(*player_id, reason, should_send(), drain, None, None, None)
            .await
    }

    async fn spawn_detach(
        &self,
        player_id: PlayerId,
        reason: SpectatorStateChangeReason,
        send_notifications: bool,
        drain: watch::Receiver<bool>,
        lifecycle_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
        drain_owner: Option<watch::Sender<bool>>,
        operation_id: Option<crate::protocol::RoomOperationId>,
    ) -> bool {
        let service = self.clone();
        tokio::spawn(async move {
            // A voluntary detach creates its own always-false drain channel.
            // Keep that sender with the owned transaction: dropping the caller
            // must not make `changed()` look like shutdown cancellation.
            let _drain_owner = drain_owner;
            service
                .detach_owned(
                    player_id,
                    reason,
                    send_notifications,
                    drain,
                    lifecycle_guard,
                    operation_id,
                )
                .await
        })
        .await
        .unwrap_or_else(|error| {
            warn!(%player_id, %error, "Owned spectator detach transaction failed");
            false
        })
    }

    async fn detach_owned(
        self,
        player_id: PlayerId,
        reason: SpectatorStateChangeReason,
        send_notifications: bool,
        drain: watch::Receiver<bool>,
        _lifecycle_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
        operation_id: Option<crate::protocol::RoomOperationId>,
    ) -> bool {
        let player_id = &player_id;
        let Some(room_id) = self
            .spectator_rooms
            .get(player_id)
            .map(|entry| *entry.value())
        else {
            return false;
        };
        let room_event_guard = self
            .message_coordinator
            .lock_room_event_mutation(&room_id)
            .await;
        if self
            .spectator_rooms
            .get(player_id)
            .is_none_or(|current| *current != room_id)
        {
            return false;
        }

        // Capture the authoritative pre-mutation snapshot under the same room
        // guard. After a successful remove, filtering the departed id produces
        // the exact committed roster without a fallible post-commit refetch.
        let room = match self.database.get_room_by_id(&room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                warn!(%player_id, %room_id, "Spectator room disappeared before detach");
                self.connection_manager
                    .advance_delivery_generation(player_id)
                    .await;
                self.spectator_rooms.remove(player_id);
                drop(room_event_guard);
                if send_notifications {
                    let predicate_drain = drain.clone();
                    let should_send = || !*predicate_drain.borrow();
                    let _ = self
                        .message_coordinator
                        .send_to_player_if(
                            player_id,
                            Arc::new(
                                (ServerMessage::SpectatorLeft {
                                    room_id: Some(room_id),
                                    room_code: None,
                                    reason: Some(reason),
                                    current_spectators: Vec::new(),
                                })
                                .correlate_room_operation(operation_id),
                            ),
                            &should_send,
                            drain,
                        )
                        .await;
                }
                return true;
            }
            Err(err) => {
                warn!(%player_id, %room_id, error = %err, "Failed to snapshot room before spectator detach");
                return false;
            }
        };
        let current_spectators: Vec<_> = room
            .get_spectators()
            .into_iter()
            .filter(|spectator| spectator.id != *player_id)
            .collect();

        match self
            .database
            .remove_spectator_from_room(&room_id, player_id)
            .await
        {
            Ok(Some(_)) => {}
            Ok(None) => {
                // Persistence is authoritative. Converge a stale local role
                // and publish the terminal roster below so both this client
                // and room members stop treating it as present.
                warn!(%player_id, %room_id, "Spectator persistence entry was already absent during detach");
            }
            Err(err) => {
                warn!(
                    %player_id,
                    %room_id,
                    error = %err,
                    "Failed to remove spectator from persistence"
                );
                return false;
            }
        }
        self.connection_manager
            .advance_delivery_generation(player_id)
            .await;
        self.spectator_rooms.remove(player_id);

        // The acknowledgement and full-roster broadcast are one owned lane
        // job. Once the DB/map transition above commits, dropping the caller
        // cannot suppress its lifecycle event, and a later spectator mutation
        // is enqueued behind this captured roster.
        let acknowledgement = Arc::new(
            (ServerMessage::SpectatorLeft {
                room_id: Some(room_id),
                room_code: Some(room.code.clone()),
                reason: Some(reason.clone()),
                current_spectators: current_spectators.clone(),
            })
            .correlate_room_operation(operation_id),
        );
        let notification = Arc::new(ServerMessage::SpectatorDisconnected {
            spectator_id: *player_id,
            reason: Some(reason.clone()),
            current_spectators,
        });
        let replay_notification = Arc::clone(&notification);
        let room_id_for_replay = room_id;
        let departed_spectator = *player_id;
        let coordinator = Arc::clone(&self.message_coordinator);
        let reconnection_manager = self.reconnection_manager.clone();
        let acknowledgement_predicate_drain = drain.clone();
        let acknowledgement_delivery_drain = drain.clone();
        let broadcast_predicate_drain = drain.clone();
        let completion = self.message_coordinator.enqueue_room_event(
            room_event_guard,
            Box::new(move || {
                Box::pin(async move {
                    if !send_notifications {
                        return Ok(false);
                    }

                    let should_acknowledge = || !*acknowledgement_predicate_drain.borrow();
                    let _ = coordinator
                        .send_to_player_if(
                            &departed_spectator,
                            acknowledgement,
                            &should_acknowledge,
                            acknowledgement_delivery_drain,
                        )
                        .await;

                    let should_broadcast = || !*broadcast_predicate_drain.borrow();
                    coordinator
                        .broadcast_to_room_except_if_with_hook(
                            &room_id_for_replay,
                            &departed_spectator,
                            notification,
                            &should_broadcast,
                            drain,
                            Box::new(move || {
                                Box::pin(async move {
                                    if let Some(reconnection_manager) = reconnection_manager {
                                        reconnection_manager
                                            .record_room_event(
                                                &room_id_for_replay,
                                                replay_notification.as_ref(),
                                            )
                                            .await;
                                    }
                                })
                            }),
                        )
                        .await
                })
            }),
        );
        let _ = completion.await;

        true
    }

    #[allow(dead_code)]
    fn room_app_id(&self, room_id: &RoomId) -> Option<Uuid> {
        self.room_applications
            .get(room_id)
            .map(|entry| *entry.value())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordination::{
        MembershipUpdate, RoomEventCompletion, RoomEventJob, RoomEventMutationGuard,
        RoomEventSequencer,
    };
    use crate::database::{GameDatabase, InMemoryDatabase};
    use crate::distributed::SequencedMessage;
    use anyhow::Result;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::{Mutex, Notify, Semaphore};

    struct RecordingCoordinator {
        sent: Mutex<Vec<(PlayerId, ServerMessage)>>,
        database: Arc<InMemoryDatabase>,
        routed_players: Mutex<HashMap<RoomId, HashSet<PlayerId>>>,
        room_events: Arc<RoomEventSequencer>,
        mutation_lock_attempts: AtomicUsize,
        delay_first_spectator_broadcast: AtomicBool,
        spectator_broadcasts: AtomicUsize,
        first_spectator_broadcast_started: Notify,
        release_first_spectator_broadcast: Notify,
        delay_room_closed_sends: AtomicBool,
        room_closed_sends_started: AtomicUsize,
        room_closed_send_started: Notify,
        release_room_closed_sends: Semaphore,
    }

    impl RecordingCoordinator {
        fn new(database: Arc<InMemoryDatabase>) -> Self {
            Self {
                sent: Mutex::new(Vec::new()),
                database,
                routed_players: Mutex::new(HashMap::new()),
                room_events: Arc::new(RoomEventSequencer::default()),
                mutation_lock_attempts: AtomicUsize::new(0),
                delay_first_spectator_broadcast: AtomicBool::new(false),
                spectator_broadcasts: AtomicUsize::new(0),
                first_spectator_broadcast_started: Notify::new(),
                release_first_spectator_broadcast: Notify::new(),
                delay_room_closed_sends: AtomicBool::new(false),
                room_closed_sends_started: AtomicUsize::new(0),
                room_closed_send_started: Notify::new(),
                release_room_closed_sends: Semaphore::new(0),
            }
        }

        async fn messages_for(&self, player_id: &PlayerId) -> Vec<ServerMessage> {
            self.sent
                .lock()
                .await
                .iter()
                .filter(|(pid, _)| pid == player_id)
                .map(|(_, message)| message.clone())
                .collect()
        }

        fn delay_first_spectator_broadcast(&self) {
            self.delay_first_spectator_broadcast
                .store(true, Ordering::Release);
        }
    }

    #[async_trait]
    impl MessageCoordinator for RecordingCoordinator {
        async fn lock_room_event_mutation(&self, room_id: &RoomId) -> RoomEventMutationGuard {
            self.mutation_lock_attempts.fetch_add(1, Ordering::AcqRel);
            self.room_events.lock(*room_id).await
        }

        fn enqueue_room_event(
            &self,
            mutation_guard: RoomEventMutationGuard,
            job: RoomEventJob,
        ) -> RoomEventCompletion {
            self.room_events.enqueue(mutation_guard, job)
        }

        async fn send_to_player(
            &self,
            player_id: &PlayerId,
            message: Arc<ServerMessage>,
        ) -> Result<()> {
            if matches!(
                message.as_ref(),
                ServerMessage::SpectatorLeft {
                    reason: Some(SpectatorStateChangeReason::RoomClosed),
                    ..
                }
            ) && self.delay_room_closed_sends.load(Ordering::Acquire)
            {
                self.room_closed_sends_started
                    .fetch_add(1, Ordering::AcqRel);
                self.room_closed_send_started.notify_waiters();
                self.release_room_closed_sends
                    .acquire()
                    .await
                    .expect("test semaphore remains open")
                    .forget();
            }
            self.sent
                .lock()
                .await
                .push((*player_id, (*message).clone()));
            Ok(())
        }

        async fn try_send_to_player(
            &self,
            player_id: &PlayerId,
            message: Arc<ServerMessage>,
        ) -> Result<bool> {
            // Test double: send_to_player is non-blocking here, so delegating
            // honors the non-waiting farewell contract while preserving
            // whatever recording/blocking behavior the double implements.
            self.send_to_player(player_id, message).await.map(|()| true)
        }

        async fn broadcast_to_room(
            &self,
            room_id: &RoomId,
            message: Arc<ServerMessage>,
        ) -> Result<()> {
            if matches!(message.as_ref(), ServerMessage::NewSpectatorJoined { .. })
                && self.delay_first_spectator_broadcast.load(Ordering::Acquire)
                && self.spectator_broadcasts.fetch_add(1, Ordering::AcqRel) == 0
            {
                self.first_spectator_broadcast_started.notify_one();
                self.release_first_spectator_broadcast.notified().await;
            }
            if let Ok(Some(room)) = self.database.get_room_by_id(room_id).await {
                let mut sent = self.sent.lock().await;
                for player_id in room.players.keys() {
                    sent.push((*player_id, (*message).clone()));
                }
            }
            Ok(())
        }

        async fn broadcast_to_room_except(
            &self,
            room_id: &RoomId,
            except_player: &PlayerId,
            message: Arc<ServerMessage>,
        ) -> Result<()> {
            if let Ok(Some(room)) = self.database.get_room_by_id(room_id).await {
                let mut sent = self.sent.lock().await;
                for player_id in room.players.keys() {
                    if player_id != except_player {
                        sent.push((*player_id, (*message).clone()));
                    }
                }
            }
            Ok(())
        }

        async fn broadcast_to_room_with_hook<'a>(
            &'a self,
            room_id: &RoomId,
            message: Arc<ServerMessage>,
            before_send: Box<
                dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                    + Send
                    + 'a,
            >,
        ) -> Result<bool> {
            before_send().await;
            self.broadcast_to_room(room_id, message).await?;
            Ok(true)
        }

        async fn broadcast_to_room_if_members_with_hook<'a>(
            &'a self,
            room_id: &RoomId,
            expected_members: &[PlayerId],
            message: Arc<ServerMessage>,
            before_send: Box<
                dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                    + Send
                    + 'a,
            >,
        ) -> Result<bool> {
            let mut routed = self.routed_player_ids(room_id).await?.unwrap_or_default();
            let mut expected = expected_members.to_vec();
            routed.sort_unstable();
            expected.sort_unstable();
            if routed != expected {
                return Ok(false);
            }
            self.broadcast_to_room_with_hook(room_id, message, before_send)
                .await
        }

        async fn broadcast_to_room_except_if_with_hook<'a>(
            &'a self,
            room_id: &RoomId,
            except_player: &PlayerId,
            message: Arc<ServerMessage>,
            should_send: &(dyn Fn() -> bool + Send + Sync),
            drain: tokio::sync::watch::Receiver<bool>,
            before_send: Box<
                dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                    + Send
                    + 'a,
            >,
        ) -> Result<bool> {
            if *drain.borrow() || !should_send() {
                return Ok(false);
            }
            before_send().await;
            self.broadcast_to_room_except(room_id, except_player, message)
                .await?;
            Ok(true)
        }

        async fn commit_room_messages_if_members_with_hook<'a>(
            &'a self,
            _room_id: &RoomId,
            _expected_members: &[PlayerId],
            recipient_messages: Vec<crate::coordination::RoomRecipientMessages>,
            before_send: Box<
                dyn FnOnce() -> std::pin::Pin<
                        Box<dyn std::future::Future<Output = Result<bool>> + Send + 'a>,
                    > + Send
                    + 'a,
            >,
            after_first_phase: Box<dyn FnOnce(usize) -> bool + Send + 'a>,
        ) -> Result<crate::coordination::RoomMessageTransactionOutcome> {
            if !before_send().await? {
                return Ok(crate::coordination::RoomMessageTransactionOutcome::HookRejected);
            }
            let mut sent = self.sent.lock().await;
            let max_phases = recipient_messages
                .iter()
                .map(crate::coordination::RoomRecipientMessages::phase_count)
                .max()
                .unwrap_or(0);
            let mut after_first_phase = Some(after_first_phase);
            for phase in 0..max_phases {
                for batch in &recipient_messages {
                    if let Some(message) = batch.message_in_phase(phase) {
                        sent.push((batch.player_id, message.as_ref().clone()));
                    }
                }
                if phase == 0
                    && !after_first_phase
                        .take()
                        .expect("transaction state callback runs once")(0)
                {
                    break;
                }
            }
            Ok(crate::coordination::RoomMessageTransactionOutcome::Committed)
        }

        async fn register_local_client(
            &self,
            player_id: PlayerId,
            room_id: Option<RoomId>,
            _delivery: crate::coordination::ClientDeliveryHandle,
        ) -> Result<()> {
            let mut routed_players = self.routed_players.lock().await;
            routed_players.retain(|_, players| {
                players.remove(&player_id);
                !players.is_empty()
            });
            if let Some(room_id) = room_id {
                routed_players.entry(room_id).or_default().insert(player_id);
            }
            Ok(())
        }

        async fn unroute_local_client_with_tail<'a>(
            &'a self,
            player_id: PlayerId,
            _room_id: RoomId,
            clear_assignment: Box<
                dyn FnOnce() -> Option<(crate::coordination::ClientDeliveryHandle, u32, u64)>
                    + Send
                    + 'a,
            >,
        ) -> Result<Option<(u32, u64)>> {
            let Some((_delivery, epoch, final_seq)) = clear_assignment() else {
                return Ok(None);
            };
            let mut routed_players = self.routed_players.lock().await;
            routed_players.retain(|_, players| {
                players.remove(&player_id);
                !players.is_empty()
            });
            Ok(Some((epoch, final_seq)))
        }

        async fn routed_player_ids(&self, room_id: &RoomId) -> Result<Option<Vec<PlayerId>>> {
            Ok(Some(
                self.routed_players
                    .lock()
                    .await
                    .get(room_id)
                    .into_iter()
                    .flat_map(|players| players.iter().copied())
                    .collect(),
            ))
        }

        async fn unregister_local_client(&self, _player_id: &PlayerId) -> Result<()> {
            Ok(())
        }

        async fn should_process_message(&self, _message: &SequencedMessage) -> Result<bool> {
            Ok(true)
        }

        async fn mark_message_processed(&self, _message: &SequencedMessage) -> Result<()> {
            Ok(())
        }

        async fn handle_bus_message(&self, _message: SequencedMessage) -> Result<()> {
            Ok(())
        }

        async fn handle_membership_update(&self, _update: MembershipUpdate) -> Result<()> {
            Ok(())
        }
    }

    async fn setup_service() -> (
        SpectatorService,
        Room,
        PlayerId,
        Arc<RecordingCoordinator>,
        Arc<InMemoryDatabase>,
    ) {
        let database = Arc::new(InMemoryDatabase::new());
        let creator_id = PlayerId::new_v4();
        let room = database
            .create_room(
                "spectator-game".to_string(),
                None,
                8,
                true,
                creator_id,
                "udp".to_string(),
                "region-a".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");

        let coordinator = Arc::new(RecordingCoordinator::new(database.clone()));
        let connection_manager = Arc::new(ConnectionManager::new(
            100,
            Arc::new(crate::metrics::ServerMetrics::new()),
            coordinator.clone(),
            false,
        ));
        let room_coordinator: Arc<dyn RoomOperationCoordinatorTrait> =
            Arc::new(crate::coordination::InMemoryRoomOperationCoordinator::new(
                coordinator.clone(),
                database.clone() as Arc<dyn GameDatabase>,
                None,
            ));
        let spectator_service = SpectatorService::new(
            database.clone() as Arc<dyn GameDatabase>,
            room_coordinator,
            coordinator.clone(),
            Arc::new(DashMap::new()),
            ProtocolConfig::default(),
            None,
            connection_manager,
            false,
            Arc::new(RoomRateLimiter::new(
                crate::rate_limit::RateLimitConfig::default(),
            )),
        );

        (spectator_service, room, creator_id, coordinator, database)
    }

    async fn connect_spectator(service: &SpectatorService, player_id: PlayerId, port: u16) {
        let (sender, _receiver) = tokio::sync::mpsc::channel(1);
        service
            .connection_manager
            .connect_test_client(
                player_id,
                sender,
                format!("127.0.0.1:{port}")
                    .parse()
                    .expect("test socket address"),
            )
            .await;
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn join_tracks_room_membership_and_notifies_players() {
        let (service, room, creator_id, coordinator, database) = setup_service().await;
        let spectator_id = PlayerId::new_v4();
        connect_spectator(&service, spectator_id, 35_010).await;

        service
            .join(
                &spectator_id,
                room.game_name.clone(),
                room.code.clone(),
                "Spectator One".to_string(),
            )
            .await
            .expect("spectator join succeeds");

        assert_eq!(
            service
                .spectator_rooms
                .get(&spectator_id)
                .map(|entry| *entry.value()),
            Some(room.id)
        );

        let stored_spectators = database
            .get_room_spectators(&room.id)
            .await
            .expect("fetch spectators");
        assert!(
            stored_spectators.iter().any(|info| info.id == spectator_id),
            "spectator should be persisted in room snapshot"
        );

        let spectator_messages = coordinator.messages_for(&spectator_id).await;
        assert!(
            spectator_messages.into_iter().any(|message| matches!(
                message,
                ServerMessage::SpectatorJoined(ref payload) if payload.room_id == room.id && payload.spectator_id == spectator_id
            )),
            "spectator should receive SpectatorJoined payload"
        );

        let player_messages = coordinator.messages_for(&creator_id).await;
        assert!(
            player_messages.into_iter().any(|message| matches!(
                message,
                ServerMessage::NewSpectatorJoined { spectator, .. }
                    if spectator.id == spectator_id
            )),
            "room players should see NewSpectatorJoined notification"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn spectator_join_rejects_seated_and_duplicate_roles_before_mutation() {
        let (service, room, creator_id, _coordinator, database) = setup_service().await;
        let (creator_tx, _creator_rx) = tokio::sync::mpsc::channel(1);
        service
            .connection_manager
            .connect_test_client(
                creator_id,
                creator_tx,
                "127.0.0.1:35002".parse().expect("test socket address"),
            )
            .await;
        service
            .connection_manager
            .assign_client_to_room(&creator_id, room.id)
            .await;

        let seated_error = service
            .join(
                &creator_id,
                room.game_name.clone(),
                room.code.clone(),
                "Seated Watcher".to_string(),
            )
            .await
            .expect_err("a seated player cannot also spectate");
        assert_eq!(seated_error.code, Some(ErrorCode::SpectatorNotAllowed));
        assert!(!service.is_spectating(&creator_id));

        let spectator_id = PlayerId::new_v4();
        connect_spectator(&service, spectator_id, 35_011).await;
        service
            .join(
                &spectator_id,
                room.game_name.clone(),
                room.code.clone(),
                "Watcher".to_string(),
            )
            .await
            .expect("first spectator join succeeds");
        let duplicate_error = service
            .join(
                &spectator_id,
                room.game_name.clone(),
                room.code.clone(),
                "Watcher Again".to_string(),
            )
            .await
            .expect_err("a spectator cannot join a second time");
        assert_eq!(duplicate_error.code, Some(ErrorCode::SpectatorJoinFailed));

        let stored = database
            .get_room_spectators(&room.id)
            .await
            .expect("fetch spectators after role rejections");
        assert_eq!(stored.len(), 1, "only the valid spectator is persisted");
        assert_eq!(stored[0].id, spectator_id);
    }

    /// A v3 spectator needs the same sender `(epoch, seq)` baseline as a seated player.
    /// The snapshot is populated from live connection state; the socket send
    /// layer strips it again for v2 recipients.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn spectator_snapshot_carries_live_player_epoch() {
        let (service, room, creator_id, coordinator, _database) = setup_service().await;
        let spectator_id = PlayerId::new_v4();
        let (creator_tx, _creator_rx) = tokio::sync::mpsc::channel(1);
        service
            .connection_manager
            .connect_test_client(
                creator_id,
                creator_tx,
                "127.0.0.1:35001".parse().expect("test socket address"),
            )
            .await;
        service
            .connection_manager
            .assign_client_to_room(&creator_id, room.id)
            .await;
        for expected_seq in 1..=3 {
            assert_eq!(
                service
                    .connection_manager
                    .next_relay_stamp_in_room(&creator_id, &room.id),
                Some(crate::server::connection_manager::RelayStamp {
                    epoch: 1,
                    seq: expected_seq,
                })
            );
        }
        let (spectator_tx, _spectator_rx) = tokio::sync::mpsc::channel(1);
        service
            .connection_manager
            .connect_test_client(
                spectator_id,
                spectator_tx,
                "127.0.0.1:35003".parse().expect("test socket address"),
            )
            .await;
        let protocol = crate::server::connection_manager::NegotiatedProtocol {
            version: 3,
            ..Default::default()
        };
        service
            .connection_manager
            .set_protocol(&spectator_id, protocol);

        service
            .join(
                &spectator_id,
                room.game_name.clone(),
                room.code.clone(),
                "Snapshot Watcher".to_string(),
            )
            .await
            .expect("spectator join succeeds");

        let payload = coordinator
            .messages_for(&spectator_id)
            .await
            .into_iter()
            .find_map(|message| match message {
                ServerMessage::SpectatorJoined(payload) => Some(payload),
                _ => None,
            })
            .expect("spectator receives a SpectatorJoined snapshot");

        assert!(
            !payload.current_players.is_empty(),
            "the snapshot lists the room's existing members"
        );
        let creator = payload
            .current_players
            .iter()
            .find(|player| player.id == creator_id)
            .expect("snapshot includes creator");
        assert_eq!((creator.epoch, creator.seq), (Some(1), Some(3)));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn spectator_snapshot_excludes_database_only_pending_join() {
        let (service, room, creator_id, coordinator, database) = setup_service().await;
        let (creator_tx, _creator_rx) = tokio::sync::mpsc::channel(1);
        service
            .connection_manager
            .connect_test_client(
                creator_id,
                creator_tx,
                "127.0.0.1:35004".parse().expect("test socket address"),
            )
            .await;
        service
            .connection_manager
            .assign_client_to_room(&creator_id, room.id)
            .await;

        let pending_id = PlayerId::new_v4();
        let mut pending = room
            .players
            .get(&creator_id)
            .expect("creator record")
            .clone();
        pending.id = pending_id;
        pending.name = "Pending Player".to_string();
        pending.is_authority = false;
        assert!(
            database
                .add_player_to_room(&room.id, pending)
                .await
                .expect("stage database-only member"),
            "pending member fits in room"
        );

        let spectator_id = PlayerId::new_v4();
        connect_spectator(&service, spectator_id, 35_005).await;
        service
            .join(
                &spectator_id,
                room.game_name.clone(),
                room.code.clone(),
                "Published Snapshot".to_string(),
            )
            .await
            .expect("spectator join succeeds");

        let payload = coordinator
            .messages_for(&spectator_id)
            .await
            .into_iter()
            .find_map(|message| match message {
                ServerMessage::SpectatorJoined(payload) => Some(payload),
                _ => None,
            })
            .expect("spectator baseline");
        let ids: HashSet<PlayerId> = payload
            .current_players
            .iter()
            .map(|player| player.id)
            .collect();
        assert!(ids.contains(&creator_id), "published creator is present");
        assert!(
            !ids.contains(&pending_id),
            "database membership is invisible until room routing publishes it"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn delayed_spectator_rosters_stay_in_room_fifo_order() {
        let (service, room, creator_id, coordinator, database) = setup_service().await;
        coordinator.delay_first_spectator_broadcast();
        let service = Arc::new(service);
        let first_id = PlayerId::new_v4();
        let second_id = PlayerId::new_v4();
        connect_spectator(&service, first_id, 35_006).await;
        connect_spectator(&service, second_id, 35_007).await;

        let first_join = {
            let service = Arc::clone(&service);
            let game_name = room.game_name.clone();
            let room_code = room.code.clone();
            tokio::spawn(async move {
                service
                    .join(&first_id, game_name, room_code, "First Watcher".to_string())
                    .await
            })
        };
        coordinator
            .first_spectator_broadcast_started
            .notified()
            .await;

        let second_join = {
            let service = Arc::clone(&service);
            let game_name = room.game_name.clone();
            let room_code = room.code.clone();
            tokio::spawn(async move {
                service
                    .join(
                        &second_id,
                        game_name,
                        room_code,
                        "Second Watcher".to_string(),
                    )
                    .await
            })
        };

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while coordinator.mutation_lock_attempts.load(Ordering::Acquire) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("second admission should reach the occupied room mutation gate");
        assert_eq!(
            database
                .get_room_spectators(&room.id)
                .await
                .expect("read blocked spectator roster")
                .len(),
            1,
            "the second DB mutation waits until the first lifecycle event commits"
        );
        assert!(
            !second_join.is_finished(),
            "the complete second admission waits behind the delayed first room event"
        );

        coordinator.release_first_spectator_broadcast.notify_one();
        first_join
            .await
            .expect("first join task")
            .expect("first spectator joins");
        second_join
            .await
            .expect("second join task")
            .expect("second spectator joins");

        let roster_sizes: Vec<usize> = coordinator
            .messages_for(&creator_id)
            .await
            .into_iter()
            .filter_map(|message| match message {
                ServerMessage::NewSpectatorJoined {
                    current_spectators, ..
                } => Some(current_spectators.len()),
                _ => None,
            })
            .collect();
        assert_eq!(
            roster_sizes,
            vec![1, 2],
            "full-roster events must follow mutation order even when the first delivery stalls"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn leave_detaches_spectator_and_sends_disconnect_notifications() {
        let (service, room, creator_id, coordinator, database) = setup_service().await;
        let spectator_id = PlayerId::new_v4();
        let remaining_id = PlayerId::new_v4();
        connect_spectator(&service, spectator_id, 35_012).await;
        connect_spectator(&service, remaining_id, 35_013).await;

        service
            .join(
                &spectator_id,
                room.game_name.clone(),
                room.code.clone(),
                "Spectator One".to_string(),
            )
            .await
            .expect("spectator join succeeds");
        service
            .join(
                &remaining_id,
                room.game_name.clone(),
                room.code.clone(),
                "Spectator Two".to_string(),
            )
            .await
            .expect("remaining spectator join succeeds");

        service.leave(&spectator_id).await.expect("leave succeeds");

        assert!(
            service.spectator_rooms.get(&spectator_id).is_none(),
            "spectator mapping should be cleared after leaving"
        );

        let stored_spectators = database
            .get_room_spectators(&room.id)
            .await
            .expect("fetch spectators after leave");
        assert_eq!(
            stored_spectators
                .iter()
                .map(|spectator| spectator.id)
                .collect::<Vec<_>>(),
            vec![remaining_id],
            "detach preserves the authoritative remaining roster"
        );

        let spectator_messages = coordinator.messages_for(&spectator_id).await;
        assert!(
            spectator_messages.into_iter().any(|message| matches!(
                message,
                ServerMessage::SpectatorLeft {
                    room_id: Some(left_room),
                    reason: Some(SpectatorStateChangeReason::VoluntaryLeave),
                    current_spectators,
                    ..
                } if left_room == room.id
                    && current_spectators.iter().map(|spectator| spectator.id).eq([remaining_id])
            )),
            "spectator should receive SpectatorLeft notification with voluntary leave reason"
        );

        let player_messages = coordinator.messages_for(&creator_id).await;
        assert!(
            player_messages.into_iter().any(|message| matches!(
                message,
                ServerMessage::SpectatorDisconnected {
                    spectator_id: sid,
                    reason: Some(SpectatorStateChangeReason::VoluntaryLeave),
                    current_spectators,
                } if sid == spectator_id
                    && current_spectators.iter().map(|spectator| spectator.id).eq([remaining_id])
            )),
            "players should see SpectatorDisconnected with voluntary leave reason"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn detach_storage_error_preserves_role_roster_and_retryability() {
        let (service, room, creator_id, coordinator, database) = setup_service().await;
        let spectator_id = PlayerId::new_v4();
        connect_spectator(&service, spectator_id, 35_014).await;
        service
            .join(
                &spectator_id,
                room.game_name.clone(),
                room.code.clone(),
                "Retryable Spectator".to_string(),
            )
            .await
            .expect("spectator join succeeds");
        coordinator.sent.lock().await.clear();
        database.fail_remove_spectator_from_room_for_test(true);

        let error = service
            .leave(&spectator_id)
            .await
            .expect_err("injected storage failure rejects detach");

        assert_eq!(error.code, Some(ErrorCode::StorageError));
        assert!(service.is_spectating(&spectator_id));
        assert!(database
            .get_room_spectators(&room.id)
            .await
            .expect("fetch unchanged roster")
            .iter()
            .any(|spectator| spectator.id == spectator_id));
        assert!(coordinator.messages_for(&spectator_id).await.is_empty());
        assert!(coordinator.messages_for(&creator_id).await.is_empty());

        database.fail_remove_spectator_from_room_for_test(false);
        service
            .leave(&spectator_id)
            .await
            .expect("unchanged role state allows a clean retry");
        assert!(!service.is_spectating(&spectator_id));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn absent_spectator_row_converges_local_role_and_peer_roster() {
        let (service, room, creator_id, coordinator, database) = setup_service().await;
        let spectator_id = PlayerId::new_v4();
        connect_spectator(&service, spectator_id, 35_015).await;
        service
            .join(
                &spectator_id,
                room.game_name.clone(),
                room.code.clone(),
                "Converging Spectator".to_string(),
            )
            .await
            .expect("spectator join succeeds");
        coordinator.sent.lock().await.clear();
        database
            .remove_spectator_from_room(&room.id, &spectator_id)
            .await
            .expect("external spectator removal succeeds")
            .expect("test spectator existed in storage");

        service
            .leave(&spectator_id)
            .await
            .expect("authoritative absence converges as a completed detach");

        assert!(!service.is_spectating(&spectator_id));
        assert!(database
            .get_room_spectators(&room.id)
            .await
            .expect("fetch converged roster")
            .iter()
            .all(|spectator| spectator.id != spectator_id));
        assert!(coordinator
            .messages_for(&spectator_id)
            .await
            .into_iter()
            .any(|message| matches!(
                message,
                ServerMessage::SpectatorLeft {
                    room_id: Some(left_room),
                    current_spectators,
                    ..
                } if left_room == room.id && current_spectators.is_empty()
            )));
        assert!(coordinator
            .messages_for(&creator_id)
            .await
            .into_iter()
            .any(|message| matches!(
                message,
                ServerMessage::SpectatorDisconnected {
                    spectator_id: departed,
                    current_spectators,
                    ..
                } if departed == spectator_id && current_spectators.is_empty()
            )));

        service
            .join(
                &spectator_id,
                room.game_name,
                room.code,
                "Converging Spectator".to_string(),
            )
            .await
            .expect("cleared role and delivery generation allow a fresh join");
        assert!(service.is_spectating(&spectator_id));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_prune_missing_room_clears_spectator_role_issue_241() {
        let (service, room, _creator_id, coordinator, database) = setup_service().await;
        let spectator_id = PlayerId::new_v4();
        connect_spectator(&service, spectator_id, 35_016).await;
        service
            .join(
                &spectator_id,
                room.game_name.clone(),
                room.code.clone(),
                "Expired Room Spectator".to_string(),
            )
            .await
            .expect("spectator join succeeds");
        coordinator.sent.lock().await.clear();
        assert!(database
            .delete_room(&room.id)
            .await
            .expect("inactive cleanup surrogate should delete room"));

        let (_drain_tx, drain) = watch::channel(false);
        assert_eq!(service.prune_missing_rooms(drain).await, 1);
        assert!(!service.is_spectating(&spectator_id));
        assert!(coordinator
            .messages_for(&spectator_id)
            .await
            .into_iter()
            .any(|message| matches!(
                message,
                ServerMessage::SpectatorLeft {
                    room_id: Some(closed_room),
                    room_code: None,
                    reason: Some(SpectatorStateChangeReason::RoomClosed),
                    ref current_spectators,
                } if closed_room == room.id && current_spectators.is_empty()
            )));

        let error = service
            .join(
                &spectator_id,
                "missing-game".to_string(),
                "ABSENT".to_string(),
                "Can Try Again".to_string(),
            )
            .await
            .expect_err("new admission reaches storage instead of stale-role rejection");
        assert_eq!(error.code, Some(ErrorCode::RoomNotFound));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_prune_deduplicates_same_room_existence_checks_issue_241() {
        let (service, room, _creator_id, _coordinator, database) = setup_service().await;
        for _ in 0..64 {
            service.spectator_rooms.insert(PlayerId::new_v4(), room.id);
        }
        database.reset_get_room_by_id_calls_for_test();
        let (_drain_tx, drain) = watch::channel(false);

        assert_eq!(service.prune_missing_rooms(drain).await, 0);
        assert_eq!(database.get_room_by_id_calls_for_test(), 1);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_missing_room_notices_across_rooms_wait_concurrently_issue_241() {
        const SPECTATOR_COUNT: usize = 8;
        let (service, _room, _creator_id, coordinator, _database) = setup_service().await;
        for _ in 0..SPECTATOR_COUNT {
            service
                .spectator_rooms
                .insert(PlayerId::new_v4(), RoomId::new_v4());
        }
        coordinator
            .delay_room_closed_sends
            .store(true, Ordering::Release);
        let service_for_prune = service.clone();
        let (_drain_tx, drain) = watch::channel(false);
        let prune = tokio::spawn(async move { service_for_prune.prune_missing_rooms(drain).await });

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let started = coordinator.room_closed_send_started.notified();
                if coordinator
                    .room_closed_sends_started
                    .load(Ordering::Acquire)
                    >= SPECTATOR_COUNT
                {
                    break;
                }
                started.await;
            }
        })
        .await
        .expect("every missing-room notice must reach its wait concurrently");
        coordinator
            .release_room_closed_sends
            .add_permits(SPECTATOR_COUNT);

        assert_eq!(prune.await.expect("prune task completes"), SPECTATOR_COUNT);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_stale_prune_snapshot_cannot_detach_rejoined_room_issue_241() {
        let (service, old_room, _creator_id, _coordinator, database) = setup_service().await;
        let spectator_id = PlayerId::new_v4();
        connect_spectator(&service, spectator_id, 35_017).await;
        service
            .join(
                &spectator_id,
                old_room.game_name.clone(),
                old_room.code.clone(),
                "Moving Watcher".to_string(),
            )
            .await
            .expect("initial spectator join succeeds");
        assert!(database
            .delete_room(&old_room.id)
            .await
            .expect("old room deletion succeeds"));
        database.pause_next_get_room_by_id_for_test();
        let service_for_prune = service.clone();
        let (_drain_tx, drain) = watch::channel(false);
        let prune = tokio::spawn(async move { service_for_prune.prune_missing_rooms(drain).await });
        database.wait_for_paused_get_room_by_id_for_test().await;

        service
            .leave(&spectator_id)
            .await
            .expect("missing old room still permits local role convergence");

        let new_room = database
            .create_room(
                "rejoined-game".to_string(),
                None,
                8,
                true,
                PlayerId::new_v4(),
                "udp".to_string(),
                "region-b".to_string(),
                None,
            )
            .await
            .expect("replacement room creation succeeds");
        service
            .join(
                &spectator_id,
                new_room.game_name.clone(),
                new_room.code.clone(),
                "Moved Watcher".to_string(),
            )
            .await
            .expect("spectator rejoins replacement room");
        database.release_paused_get_room_by_id_for_test();

        assert_eq!(prune.await.expect("prune task completes"), 0);
        assert_eq!(service.spectator_room(&spectator_id), Some(new_room.id));
        let stored = database
            .get_room_by_id(&new_room.id)
            .await
            .expect("replacement room lookup succeeds")
            .expect("replacement room remains");
        assert!(stored.spectators.contains_key(&spectator_id));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_prune_suppresses_room_closed_notice_after_drain_issue_241() {
        let (service, room, _creator_id, coordinator, database) = setup_service().await;
        let spectator_id = PlayerId::new_v4();
        connect_spectator(&service, spectator_id, 35_018).await;
        service.spectator_rooms.insert(spectator_id, room.id);
        coordinator.sent.lock().await.clear();
        assert!(database
            .delete_room(&room.id)
            .await
            .expect("inactive cleanup surrogate should delete room"));
        let (drain_tx, drain) = watch::channel(false);
        drain_tx.send(true).expect("drain receiver remains live");

        assert_eq!(service.prune_missing_rooms(drain).await, 1);
        assert!(!service.is_spectating(&spectator_id));
        assert!(coordinator.messages_for(&spectator_id).await.is_empty());
    }

    /// A pending unpublished-detach row whose identity has since been
    /// republished to the same room is void, not executable: the maintenance
    /// sweep must clear the rollback record while leaving the live roster row
    /// and the role mapping alone. Executing it instead would ghost a seated
    /// broadcast out of a spectator that the room still lists.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn retry_disconnected_detaches_never_deletes_a_republished_identity() {
        let (service, room, _creator_id, _coordinator, database) = setup_service().await;
        let spectator_id = PlayerId::new_v4();
        connect_spectator(&service, spectator_id, 35_019).await;
        service
            .join(
                &spectator_id,
                room.game_name.clone(),
                room.code.clone(),
                "Republished Watcher".to_string(),
            )
            .await
            .expect("spectator join succeeds");

        // Simulate the stale rollback state: a disconnect-time storage failure
        // left an unpublished detach row behind, then the same durable
        // identity was re-admitted to the same room before maintenance ran.
        service
            .pending_unpublished_detaches
            .insert((room.id, spectator_id), ());

        assert_eq!(
            service.retry_disconnected_detaches().await,
            0,
            "voiding a republished row is not a detach"
        );
        let stored = database
            .get_room_spectators(&room.id)
            .await
            .expect("fetch spectators");
        assert!(
            stored.iter().any(|info| info.id == spectator_id),
            "maintenance must not delete the published roster entry"
        );
        assert_eq!(service.spectator_room(&spectator_id), Some(room.id));
        assert!(
            !service
                .pending_unpublished_detaches
                .contains_key(&(room.id, spectator_id)),
            "the voided rollback record is cleared"
        );
    }
}
