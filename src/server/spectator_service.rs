use std::collections::{HashMap, HashSet};
#[cfg(test)]
use std::sync::atomic::AtomicU8;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use futures_util::FutureExt;
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::ProtocolConfig;
use crate::coordination::{
    MessageCoordinator, RoomEventMutationGuard, RoomOperationCoordinatorTrait,
};
use crate::database::GameDatabase;
use crate::distributed::{keep_lease_renewed, DistributedLock};
use crate::protocol::{
    validation, ErrorCode, PlayerId, PlayerInfo, Room, RoomId, ServerMessage, SpectatorInfo,
    SpectatorJoinedPayload, SpectatorStateChangeReason,
};
use crate::rate_limit::RoomRateLimiter;
use crate::reconnection::ReconnectionManager;
use tokio::sync::watch;

use super::connection_manager::ClientLifecycle;
use super::room_service::ROOM_JOIN_LOCK_TTL;
use super::ConnectionManager;

/// The owed repair behind a [`SpectatorService::pending_unpublished_detaches`]
/// entry. The two markers are owned by different writers and must not clear
/// each other: a compensating rollback clears only its own `RowDelete`, and
/// the maintenance sweep converts a deleted ghost row's `NotifyRetry` into a
/// published correction (re-queueing `NotifyRetry` if publication could not
/// read the room).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PendingSpectatorDetachRepair {
    /// The durable row still exists and its compensating delete is owed.
    RowDelete,
    /// The row is deleted; the absolute correcting
    /// `SpectatorDisconnected` event is still owed to live members.
    NotifyRetry,
}

#[derive(Clone)]
pub(crate) struct SpectatorService {
    spectator_rooms: Arc<DashMap<PlayerId, RoomId>>,
    /// Durable rows created by a spectator admission that never became
    /// externally visible, but whose compensating delete failed. These are
    /// deliberately separate from `spectator_rooms`: the client must not look
    /// joined while maintenance retains enough identity to repair storage.
    /// The value names the owed repair: `RowDelete` belongs to a failed
    /// compensating rollback (the row still exists), while `NotifyRetry`
    /// belongs to the maintenance sweep after it deleted a ghost row but
    /// could not publish the correcting event. The distinction keeps a
    /// later successful rollback from clearing a notification it does not
    /// own (a rollback that deleted its own row must still let the ghost
    /// correction converge).
    pending_unpublished_detaches: Arc<DashMap<(RoomId, PlayerId), PendingSpectatorDetachRepair>>,
    database: Arc<dyn GameDatabase>,
    /// Readiness is coordinator state, not room-record state, until finalize
    /// writes it through. The spectator snapshot reads it from here so it
    /// reports the same lobby the members themselves see.
    room_coordinator: Arc<dyn RoomOperationCoordinatorTrait>,
    message_coordinator: Arc<dyn MessageCoordinator>,
    distributed_lock: Arc<dyn DistributedLock>,
    metrics: Arc<crate::metrics::ServerMetrics>,
    room_applications: Arc<DashMap<RoomId, Uuid>>,
    protocol_config: ProtocolConfig,
    /// Records this service's room-uniform broadcasts (`NewSpectatorJoined` /
    /// `SpectatorDisconnected`) for reconnection replay; `None` when
    /// reconnection is disabled.
    reconnection_manager: Option<Arc<ReconnectionManager>>,
    connection_manager: Arc<ConnectionManager>,
    rate_limiter: Arc<RoomRateLimiter>,
    shutdown_drain_deadline_ms: Arc<AtomicU64>,
    shutdown_drain_rx: watch::Receiver<bool>,
    /// Dev-only panic-injection slot for the owned spectator join transaction
    /// (mirrors `EnhancedGameServer::owned_room_operation_panic`).
    #[cfg(test)]
    join_panic_point: Arc<AtomicU8>,
    /// Dev-only panic-injection slot for the owned spectator detach
    /// transaction (same discipline as [`Self::join_panic_point`]).
    #[cfg(test)]
    detach_panic_point: Arc<AtomicU8>,
}

/// Where an injected owned-transaction panic fires (test-only).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpectatorJoinPanicPoint {
    /// Fires inside the `Ok(true)` arm of the durable spectator admission,
    /// BEFORE the delivery generation advances: the DB row exists but no
    /// local role is published yet and the generation state is untouched, so
    /// the compensation must roll the row back WITHOUT rewinding generations.
    JoinAfterDurableAdmission = 1,
    /// Fires after the advance: the compensation must roll both the row and
    /// the delivery generation back.
    JoinAfterGenerationAdvance = 2,
}

/// Where an injected owned-detach panic fires (test-only).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpectatorDetachPanicPoint {
    /// Fires after the durable spectator removal committed, BEFORE the local
    /// completion (delivery-generation advance + local role removal): the
    /// compensation must complete the local transition or the live connection
    /// stays wedged out of re-spectating until it retries or disconnects.
    DetachAfterDurableRemove = 1,
    /// Fires after local completion but before terminal event publication.
    /// Recovery must preserve the generation and continue the normal events.
    DetachAfterLocalCompletion = 2,
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
        distributed_lock: Arc<dyn DistributedLock>,
        metrics: Arc<crate::metrics::ServerMetrics>,
        room_applications: Arc<DashMap<RoomId, Uuid>>,
        protocol_config: ProtocolConfig,
        reconnection_manager: Option<Arc<ReconnectionManager>>,
        connection_manager: Arc<ConnectionManager>,
        rate_limiter: Arc<RoomRateLimiter>,
        shutdown_drain_deadline_ms: Arc<AtomicU64>,
        shutdown_drain_rx: watch::Receiver<bool>,
    ) -> Self {
        Self {
            spectator_rooms: Arc::new(DashMap::new()),
            pending_unpublished_detaches: Arc::new(DashMap::new()),
            database,
            room_coordinator,
            message_coordinator,
            distributed_lock,
            metrics,
            room_applications,
            protocol_config,
            reconnection_manager,
            connection_manager,
            rate_limiter,
            shutdown_drain_deadline_ms,
            shutdown_drain_rx,
            #[cfg(test)]
            join_panic_point: Arc::new(AtomicU8::new(0)),
            #[cfg(test)]
            detach_panic_point: Arc::new(AtomicU8::new(0)),
        }
    }

    fn is_draining(&self) -> bool {
        self.shutdown_drain_deadline_ms.load(Ordering::Acquire) != 0
    }

    /// Undo a durable spectator admission that never became externally
    /// visible. `generation_advanced` must reflect whether this transaction's
    /// `advance_delivery_generation` already ran: rolling back an advance that
    /// never happened would resurrect a retired delivery generation from an
    /// earlier join/leave cycle.
    async fn rollback_unpublished_spectator_join(
        &self,
        player_id: &PlayerId,
        room_id: &RoomId,
        generation_advanced: bool,
    ) {
        self.spectator_rooms.remove(player_id);
        if generation_advanced {
            self.connection_manager
                .rollback_delivery_generation(player_id)
                .await;
        }
        match self
            .database
            .remove_spectator_from_room(room_id, player_id)
            .await
        {
            Ok(_) => {
                // Clear only a repair this rollback owns. A `NotifyRetry`
                // marker means maintenance deleted an earlier ghost row and
                // still owes members the correcting event; this rollback
                // deleted its own (later) row and must leave that retry
                // standing.
                if self
                    .pending_unpublished_detaches
                    .get(&(*room_id, *player_id))
                    .is_some_and(|entry| *entry.value() == PendingSpectatorDetachRepair::RowDelete)
                {
                    self.pending_unpublished_detaches
                        .remove(&(*room_id, *player_id));
                }
            }
            Err(err) => {
                self.pending_unpublished_detaches.insert(
                    (*room_id, *player_id),
                    PendingSpectatorDetachRepair::RowDelete,
                );
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
        self.join_operation(player_id, None, game_name, room_code, spectator_name, None)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn join_operation(
        &self,
        player_id: &PlayerId,
        operation_id: Option<crate::protocol::RoomOperationId>,
        game_name: String,
        room_code: String,
        spectator_name: String,
        password: Option<String>,
    ) -> Result<(), SpectatorError> {
        self.join_operation_from_lifecycle(
            player_id,
            operation_id,
            game_name,
            room_code,
            spectator_name,
            password,
            None,
        )
        .await
    }

    pub(crate) async fn join_operation_from_lifecycle(
        &self,
        player_id: &PlayerId,
        operation_id: Option<crate::protocol::RoomOperationId>,
        game_name: String,
        room_code: String,
        spectator_name: String,
        password: Option<String>,
        source_lifecycle: Option<Arc<ClientLifecycle>>,
    ) -> Result<(), SpectatorError> {
        let service = self.clone();
        let player_id = *player_id;
        // Compensation key for a panic between the durable admission and the
        // local publication: while set, a panic must roll the row back, or a
        // ghost row survives that no maintenance sweep can see (no local role,
        // no pending-repair key) and it keeps consuming spectator capacity.
        // The flag records whether this transaction already advanced the
        // delivery generation, so the rollback never rewinds an advance that
        // never happened.
        let pending_rollback = Arc::new(std::sync::Mutex::new(None::<(RoomId, bool)>));
        let pending_rollback_in_task = Arc::clone(&pending_rollback);
        // `join_owned` consumes the service clone, so the compensation arm
        // needs its own handle.
        let rollback_service = service.clone();
        tokio::spawn(async move {
            // The lifecycle guard is taken HERE, outside the panicking
            // transaction, and held across the compensation: unwind drops the
            // guards inside `join_owned`, and a compensation that ran without
            // the lifecycle lock could race a concurrent admission for the
            // same connection (deleting a newer row or rewinding a newer
            // advance). Same discipline as the player join's panic repair.
            let Some(lifecycle) = source_lifecycle
                .or_else(|| service.connection_manager.client_lifecycle(&player_id))
            else {
                return Err(SpectatorError::new(
                    "Connection is no longer active",
                    Some(ErrorCode::SpectatorJoinFailed),
                ));
            };
            // Held (not merely acquired) through the compensation below:
            // dropping it early would reopen the concurrent-admission race.
            let _lifecycle_guard = Arc::clone(&lifecycle).lock_owned().await;
            if lifecycle.player_id() != player_id
                || !service
                    .connection_manager
                    .lifecycle_matches(&player_id, &lifecycle)
            {
                return Err(SpectatorError::new(
                    "Connection identity changed",
                    Some(ErrorCode::SpectatorJoinFailed),
                ));
            }
            let outcome = std::panic::AssertUnwindSafe(service.join_owned(
                player_id,
                operation_id,
                game_name,
                room_code,
                spectator_name,
                password,
                pending_rollback_in_task,
            ))
            .catch_unwind()
            .await;
            match outcome {
                Ok(result) => result,
                Err(_panic) => {
                    tracing::error!(%player_id, "Owned spectator join transaction panicked");
                    // The marker lock guard must drop before the async rollback.
                    let pending = pending_rollback
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .take();
                    if let Some((room_id, generation_advanced)) = pending {
                        // Serialize the compensating delete with the room's
                        // event lane (same discipline as the maintenance
                        // retry sweep) so it cannot interleave with a
                        // republished admission's add + publish.
                        let room_event_guard = rollback_service
                            .message_coordinator
                            .lock_room_event_mutation(&room_id)
                            .await;
                        rollback_service
                            .rollback_unpublished_spectator_join(
                                &player_id,
                                &room_id,
                                generation_advanced,
                            )
                            .await;
                        drop(room_event_guard);
                    }
                    Err(SpectatorError::new(
                        "Spectator join failed unexpectedly",
                        Some(ErrorCode::SpectatorJoinFailed),
                    ))
                }
            }
            // `lifecycle_guard` drops here — only after compensation finished.
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
        password: Option<String>,
        pending_rollback: Arc<std::sync::Mutex<Option<(RoomId, bool)>>>,
    ) -> Result<(), SpectatorError> {
        let player_id = &player_id;
        // The lifecycle guard and identity validation live in `join_operation`
        // so they outlive a panic; this body runs with the connection already
        // serialized against concurrent joins and disconnect-time detaches.
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
        // Join-password shape check (issue #525), mirroring the seated-join
        // path: an empty or oversized password is a malformed request,
        // rejected before any room lookup.
        if let Some(password) = password.as_deref() {
            if !Room::is_valid_room_password(Some(password)) {
                return Err(SpectatorError::new(
                    format!(
                        "Join password must be non-empty and at most {} bytes",
                        crate::protocol::MAX_ROOM_PASSWORD_LENGTH
                    ),
                    Some(ErrorCode::InvalidInput),
                ));
            }
        }
        let room_code = room_code.to_ascii_uppercase();

        // A creator holds this code lock until it has applied the deployment
        // spectator cap. Resolve the code and enter the room event lane while
        // holding the same lock, or a spectator can join against the fresh
        // room's temporary unlimited cap.
        let lock_key = format!("room_join:{game_name}:{room_code}");
        let lock_handle = self
            .distributed_lock
            .acquire(&lock_key, ROOM_JOIN_LOCK_TTL)
            .await
            .map_err(|error| {
                warn!(%lock_key, %error, "Failed to acquire spectator room-code lock");
                SpectatorError::new("Storage error", Some(ErrorCode::StorageError))
            })?;
        let mut lock_renewal = keep_lease_renewed(
            Arc::clone(&self.distributed_lock),
            lock_handle,
            ROOM_JOIN_LOCK_TTL,
            Arc::clone(&self.metrics),
        );
        let room_and_guard = async {
            let room = match self.database.get_room(&game_name, &room_code).await {
                Ok(Some(room)) => room,
                Ok(None) => {
                    return Err(SpectatorError::new(
                        "Room not found",
                        Some(ErrorCode::RoomNotFound),
                    ));
                }
                Err(error) => {
                    warn!(%error, "Failed to fetch room for spectator");
                    return Err(SpectatorError::new(
                        "Storage error",
                        Some(ErrorCode::StorageError),
                    ));
                }
            };
            let guard = self
                .message_coordinator
                .lock_room_event_mutation(&room.id)
                .await;
            Ok((room, guard))
        }
        .await;
        lock_renewal.stop_renewal();
        match self.distributed_lock.release(lock_renewal.handle()).await {
            Ok(true) => {}
            Ok(false) => {
                warn!(%lock_key, "Spectator room-code lock expired before release");
                self.metrics.increment_distributed_lock_release_failures();
            }
            Err(error) => {
                warn!(%lock_key, %error, "Failed to release spectator room-code lock");
                self.metrics.increment_distributed_lock_release_failures();
            }
        }
        let (room, room_event_guard) = room_and_guard?;
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
        if !self
            .database
            .is_room_published(&room.id)
            .await
            .map_err(|err| {
                warn!(room_id = %room.id, error = %err, "Failed to verify room publication");
                SpectatorError::new("Storage error", Some(ErrorCode::StorageError))
            })?
        {
            return Err(SpectatorError::new(
                "Room not found",
                Some(ErrorCode::RoomNotFound),
            ));
        }

        // The handler checks drain before it starts the transaction, but the
        // code lock and room lane can keep this join waiting past that check.
        if self.is_draining() {
            return Err(SpectatorError::new(
                "Server is draining for shutdown",
                Some(ErrorCode::ServerDraining),
            ));
        }

        // Room persistence is authoritative. The process-local room/app map is
        // only a relay cache and may be empty after restart or cache loss.
        // App scoping applies in both admission modes (issue #520): an owned
        // room is invisible to members of other applications and to
        // context-less connections, while a context-less connection keeps
        // the legacy behavior for unowned rooms.
        if let Some(owner) = room.application_id {
            let Some(client_app_id) = self.connection_manager.app_id(player_id) else {
                return Err(SpectatorError::new(
                    "Room not found",
                    Some(ErrorCode::RoomNotFound),
                ));
            };
            if owner != client_app_id {
                return Err(SpectatorError::new(
                    "Room not found",
                    Some(ErrorCode::RoomNotFound),
                ));
            }
            self.room_applications.insert(room.id, owner);
        }

        // Join-password perimeter and the authority ban list (issue #525):
        // checked against the fresh in-lane room state before capacity, so a
        // protected room leaks nothing — not even its fullness — to a request
        // that has not presented its credential. As on the seated-join path,
        // a missing, mismatched, or stray (open-room) password share one
        // non-enumerating outcome (issue #546).
        if !room.admits_join_password(password.as_deref()) {
            return Err(SpectatorError::new(
                "Room requires the join password set by its authority",
                Some(ErrorCode::PasswordRequired),
            ));
        }
        if room.is_banned(player_id) {
            return Err(SpectatorError::new(
                "Player is banned from this room",
                Some(ErrorCode::Banned),
            ));
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
            // Wall clock (durable record): membership stamp on a durable
            // room-state row.
            connected_at: Some(chrono::Utc::now()),
        };

        // From here until the local role is published, a panic must be
        // compensated: the durable row would otherwise survive as a ghost
        // (no local role, no pending-repair key) that consumes capacity. The
        // flag records that the delivery generation has NOT been advanced
        // yet, so the compensation must not rewind it.
        *pending_rollback
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((room.id, false));

        match self
            .database
            .add_spectator_to_room(&room.id, spectator.clone())
            .await
        {
            Ok(true) => {
                if self.is_draining() {
                    self.rollback_unpublished_spectator_join(player_id, &room.id, false)
                        .await;
                    return Err(SpectatorError::new(
                        "Server is draining for shutdown",
                        Some(ErrorCode::ServerDraining),
                    ));
                }
                #[cfg(test)]
                self.trigger_spectator_join_panic_for_test(
                    SpectatorJoinPanicPoint::JoinAfterDurableAdmission,
                );
                self.connection_manager
                    .advance_delivery_generation(player_id)
                    .await;
                // Published generation state: the compensation must now roll
                // the advance back alongside the row.
                *pending_rollback
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((room.id, true));
                #[cfg(test)]
                self.trigger_spectator_join_panic_for_test(
                    SpectatorJoinPanicPoint::JoinAfterGenerationAdvance,
                );
                let recipient_is_v3 = self.connection_manager.supports_v3(player_id);
                let current_room = match self.database.get_room_by_id(&room.id).await {
                    Ok(Some(current_room)) => current_room,
                    Ok(None) => {
                        self.rollback_unpublished_spectator_join(player_id, &room.id, true)
                            .await;
                        return Err(SpectatorError::new(
                            "Room not found",
                            Some(ErrorCode::RoomNotFound),
                        ));
                    }
                    Err(err) => {
                        warn!(room_id = %room.id, error = %err, "Failed to build fresh spectator baseline");
                        self.rollback_unpublished_spectator_join(player_id, &room.id, true)
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
                // `current_room` was fetched after the successful insert while
                // this room's mutation guard was held, so it is the
                // authoritative roster for both the baseline and broadcast.
                // Never turn a storage read failure into an empty roster.
                let spectator_snapshot = current_room.get_spectators();

                let join_reason = SpectatorStateChangeReason::Joined;
                let drain = self.shutdown_drain_rx.clone();
                let should_deliver = || !self.is_draining();
                let Some(delivery) = self.connection_manager.client_delivery_handle(player_id)
                else {
                    self.rollback_unpublished_spectator_join(player_id, &room.id, true)
                        .await;
                    return Err(SpectatorError::new(
                        "Connection is no longer active",
                        Some(ErrorCode::SpectatorJoinFailed),
                    ));
                };
                let snapshot_room = current_room.clone();
                let snapshot_spectators = spectator_snapshot.clone();
                let connection_manager = Arc::clone(&self.connection_manager);
                let baseline_delivered = self
                    .message_coordinator
                    .register_local_spectator_with_initial_message_async(
                        *player_id,
                        room.id,
                        delivery,
                        &should_deliver,
                        None,
                        Some(drain),
                        Box::new(move |routed_player_ids| {
                            Box::pin(async move {
                                let current_room = snapshot_room;
                                let routed_player_ids: HashSet<_> =
                                    routed_player_ids.into_iter().collect();
                                let current_players: Vec<PlayerInfo> = current_room
                                    .players
                                    .values()
                                    .cloned()
                                    .filter_map(|mut player| {
                                        // Same derivation as `RoomJoined` / `Reconnected`: the
                                        // stored flag is only written at finalize.
                                        player.is_ready = ready_players.contains(&player.id);
                                        let relay_stamp = connection_manager
                                            .current_relay_stamp_in_room(&player.id, &room.id);
                                        let is_published = routed_player_ids.contains(&player.id);
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
                                Ok(Arc::new(
                                    (ServerMessage::SpectatorJoined(Box::new(
                                        SpectatorJoinedPayload {
                                            room_id: current_room.id,
                                            room_code: current_room.code.clone(),
                                            spectator_id: *player_id,
                                            game_name: current_room.game_name.clone(),
                                            current_players,
                                            current_spectators: snapshot_spectators,
                                            lobby_state: current_room.lobby_state.clone(),
                                            reason: Some(SpectatorStateChangeReason::Joined),
                                        },
                                    )))
                                    .correlate_room_operation(operation_id),
                                ))
                            })
                        }),
                    )
                    .await
                    .map(|outcome| outcome == crate::coordination::DeliveryOutcome::Delivered)
                    .unwrap_or(false);
                if !baseline_delivered {
                    self.rollback_unpublished_spectator_join(player_id, &room.id, true)
                        .await;
                    let draining = self.is_draining();
                    return Err(SpectatorError::new(
                        if draining {
                            "Server is draining for shutdown"
                        } else {
                            "Spectator join response was not deliverable"
                        },
                        Some(if draining {
                            ErrorCode::ServerDraining
                        } else {
                            ErrorCode::SpectatorJoinFailed
                        }),
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
                // Published: a panic past this point leaves a live, mapped
                // spectator whose member notification is best-effort — no
                // rollback may run, or it would delete a live admission.
                *pending_rollback
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;

                // The spectator admission is a new role choice in this room:
                // it supersedes any unclaimed pending reconnection record the
                // player left here, exactly as a seated join does. A surviving
                // record would let the pre-spectator token re-seat the player
                // after the spectator session ends — a superseded membership
                // resurrected through a stale credential. Claimed records (an
                // in-flight token restore) and records for other rooms are
                // untouched.
                if let Some(reconnection_manager) = self.reconnection_manager.as_ref() {
                    reconnection_manager
                        .discard_pending_reconnection_in_room(player_id, &room.id)
                        .await;
                }

                let notification = Arc::new(ServerMessage::NewSpectatorJoined {
                    spectator: spectator.clone(),
                    current_spectators: spectator_snapshot.clone(),
                    // The roster is u8-capped (`max_spectators`), so the
                    // saturating fallback is unreachable; it keeps the
                    // explicit-narrowing lint honest without a panic path.
                    spectator_count: Some(
                        u32::try_from(spectator_snapshot.len()).unwrap_or(u32::MAX),
                    ),
                    reason: Some(join_reason),
                });
                let replay_notification = Arc::clone(&notification);
                let committed = Arc::new(AtomicBool::new(false));
                let committed_in_hook = Arc::clone(&committed);
                let room_id_for_replay = room.id;
                let joined_spectator = *player_id;
                let notification_drain = self.shutdown_drain_rx.clone();
                let coordinator = Arc::clone(&self.message_coordinator);
                let reconnection_manager = self.reconnection_manager.clone();
                let completion = self.message_coordinator.enqueue_room_event(
                    room_event_guard,
                    Box::new(move || {
                        Box::pin(async move {
                            coordinator
                                .broadcast_to_room_except_if_with_hook(
                                    &room_id_for_replay,
                                    &joined_spectator,
                                    notification,
                                    &|| !*notification_drain.borrow(),
                                    notification_drain.clone(),
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

    #[cfg(test)]
    pub(crate) async fn leave(&self, player_id: &PlayerId) -> Result<(), SpectatorError> {
        self.leave_from_lifecycle(player_id, None).await
    }

    pub(crate) async fn leave_from_lifecycle(
        &self,
        player_id: &PlayerId,
        source_lifecycle: Option<Arc<ClientLifecycle>>,
    ) -> Result<(), SpectatorError> {
        if !self.is_spectating(player_id) {
            return Err(SpectatorError::new(
                "You are not currently spectating a room",
                Some(ErrorCode::NotASpectator),
            ));
        }
        if self
            .detach_from_lifecycle(
                player_id,
                SpectatorStateChangeReason::VoluntaryLeave,
                source_lifecycle,
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

    pub(crate) async fn leave_operation_from_lifecycle(
        &self,
        player_id: &PlayerId,
        operation_id: crate::protocol::RoomOperationId,
        source_lifecycle: Option<Arc<ClientLifecycle>>,
    ) -> Result<(), SpectatorError> {
        if !self.is_spectating(player_id) {
            return Err(SpectatorError::new(
                "You are not currently spectating a room",
                Some(ErrorCode::NotASpectator),
            ));
        }
        if self
            .detach_operation_from_lifecycle(
                player_id,
                SpectatorStateChangeReason::VoluntaryLeave,
                operation_id,
                source_lifecycle,
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
            match self.database.room_exists(&room_id).await {
                Ok(true) => {}
                Ok(false) => {
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
            let room_event_guard = self
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
                        self.publish_ghost_repair_notification(
                            &room_id,
                            &player_id,
                            room_event_guard,
                        )
                        .await;
                        continue;
                    }
                }
                Err(err) => {
                    warn!(%player_id, %room_id, error = %err, "Failed to retry unpublished spectator rollback");
                }
            }
            drop(room_event_guard);
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

    /// Publish the terminal roster after the sweep deletes a ghost row that a
    /// failed compensating rollback kept visible (issue #396 sweep): a racing
    /// admission may already have announced the ghost through an absolute
    /// baseline or delta, so live members need the absolute correcting event.
    /// Members that never saw the ghost apply the identical absolute shape as
    /// a no-op. Runs under the room's event-lane guard; the notification and
    /// its replay record move into one FIFO lane job.
    async fn publish_ghost_repair_notification(
        &self,
        room_id: &RoomId,
        ghost_id: &PlayerId,
        room_event_guard: RoomEventMutationGuard,
    ) {
        let room = match self.database.get_room_by_id(room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                // The room row is gone; nobody remains to correct.
                return;
            }
            Err(error) => {
                // The ghost row is already deleted and the repair key
                // cleared; only the correcting broadcast is owed. Re-queue
                // the repair key so the next sweep retries the notification
                // instead of dropping it (the sweep's delete-then-repair
                // ordering leaves no other path back to this state), and say
                // so — silence here would resurrect the exact stale-count
                // state this repair exists to close. A durable identity
                // republished before the retry is voided by the sweep's
                // republished-identity check, exactly like a failed delete.
                self.pending_unpublished_detaches.insert(
                    (*room_id, *ghost_id),
                    PendingSpectatorDetachRepair::NotifyRetry,
                );
                warn!(%room_id, ghost_id = %ghost_id, error = %error, "Failed to read room for ghost-repair notification; queued a retry");
                return;
            }
        };
        let current_spectators = room.get_spectators();
        let notification = Arc::new(ServerMessage::SpectatorDisconnected {
            spectator_id: *ghost_id,
            reason: Some(SpectatorStateChangeReason::Disconnected),
            // Same saturating fallback discipline as the detach path: the
            // roster is u8-capped (`max_spectators`), so it is unreachable.
            spectator_count: Some(u32::try_from(current_spectators.len()).unwrap_or(u32::MAX)),
            current_spectators,
        });
        let replay_notification = Arc::clone(&notification);
        let room_id_for_replay = *room_id;
        let ghost_id_for_send = *ghost_id;
        let coordinator = Arc::clone(&self.message_coordinator);
        let reconnection_manager = self.reconnection_manager.clone();
        // Maintenance is not drain-aware: a fresh never-drained channel keeps
        // the broadcast predicates unconditionally true for the job's life.
        let (drain_tx, drain_rx) = watch::channel(false);
        let completion = self.message_coordinator.enqueue_room_event(
            room_event_guard,
            Box::new(move || {
                Box::pin(async move {
                    // Keep the sender alive for as long as the receivers.
                    let _keep_drain_sender = drain_tx;
                    let should_broadcast = || !*drain_rx.borrow();
                    coordinator
                        .broadcast_to_room_except_if_with_hook(
                            &room_id_for_replay,
                            &ghost_id_for_send,
                            notification,
                            &should_broadcast,
                            drain_rx.clone(),
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
    }

    pub(crate) async fn detach(
        &self,
        player_id: &PlayerId,
        reason: SpectatorStateChangeReason,
    ) -> bool {
        self.detach_from_lifecycle(player_id, reason, None).await
    }

    async fn detach_from_lifecycle(
        &self,
        player_id: &PlayerId,
        reason: SpectatorStateChangeReason,
        source_lifecycle: Option<Arc<ClientLifecycle>>,
    ) -> bool {
        let (drain_tx, drain_rx) = watch::channel(false);
        self.detach_expected(
            player_id,
            reason,
            None,
            drain_rx,
            Some(drain_tx),
            None,
            source_lifecycle,
        )
        .await
    }

    async fn detach_operation_from_lifecycle(
        &self,
        player_id: &PlayerId,
        reason: SpectatorStateChangeReason,
        operation_id: crate::protocol::RoomOperationId,
        source_lifecycle: Option<Arc<ClientLifecycle>>,
    ) -> bool {
        let (drain_tx, drain_rx) = watch::channel(false);
        self.detach_expected(
            player_id,
            reason,
            None,
            drain_rx,
            Some(drain_tx),
            Some(operation_id),
            source_lifecycle,
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
        source_lifecycle: Option<Arc<ClientLifecycle>>,
    ) -> bool {
        let lifecycle =
            source_lifecycle.or_else(|| self.connection_manager.client_lifecycle(player_id));
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
            let outcome = std::panic::AssertUnwindSafe(service.detach_owned(
                player_id,
                reason,
                send_notifications,
                drain,
                lifecycle_guard,
                operation_id,
            ))
            .catch_unwind()
            .await;
            match outcome {
                Ok(result) => result,
                Err(_panic) => {
                    tracing::error!(%player_id, "Owned spectator detach transaction panicked");
                    false
                }
            }
        })
        .await
        .unwrap_or_else(|error| {
            warn!(%player_id, %error, "Owned spectator detach transaction failed");
            false
        })
    }

    /// Finish local state while the caller retains the room and lifecycle gates.
    /// A failed handle publication retries once without advancing again. If
    /// that fails, close the socket and still publish the room's terminal event.
    async fn complete_local_detach(&self, player_id: &PlayerId, room_id: &RoomId) -> bool {
        let mut delivery = None;
        let mut publication_completed = false;
        let local_completion = std::panic::AssertUnwindSafe(async {
            #[cfg(test)]
            self.trigger_spectator_detach_panic_for_test(
                SpectatorDetachPanicPoint::DetachAfterDurableRemove,
            );
            delivery = self
                .connection_manager
                .advance_delivery_generation_local(player_id);
            publication_completed = match delivery.as_ref() {
                Some(handle) => self
                    .message_coordinator
                    .register_local_client(*player_id, None, handle.clone())
                    .await
                    .is_ok(),
                None => true,
            };
            self.spectator_rooms.remove(player_id);
            #[cfg(test)]
            self.trigger_spectator_detach_panic_for_test(
                SpectatorDetachPanicPoint::DetachAfterLocalCompletion,
            );
        })
        .catch_unwind()
        .await;
        if local_completion.is_err() {
            warn!(%player_id, %room_id, "Recovered spectator detach local completion panic");
        }
        if !publication_completed {
            // A panic before the synchronous advance leaves no handle. A
            // publication panic retains the advanced handle for the retry.
            if delivery.is_none() {
                delivery = self
                    .connection_manager
                    .advance_delivery_generation_local(player_id);
            }
            publication_completed = match delivery {
                Some(handle) => matches!(
                    std::panic::AssertUnwindSafe(
                        self.message_coordinator
                            .register_local_client(*player_id, None, handle)
                    )
                    .catch_unwind()
                    .await,
                    Ok(Ok(()))
                ),
                None => true,
            };
            if !publication_completed {
                warn!(%player_id, "Spectator detach generation publication failed; closing connection");
                self.connection_manager
                    .request_close_for(player_id, crate::coordination::CloseReason::Unregistered);
            }
        }
        self.spectator_rooms.remove(player_id);
        publication_completed
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
        // INVARIANT all role-mutating paths must preserve: the room mutation
        // gate is held here and the (player → room) mapping is re-validated
        // under it, which is what closes the prune/detach TOCTOU (issue #241).
        // The lifecycle guard is mandatory only for LIVE connections (it
        // serializes against a concurrent join/leave for the same socket);
        // the maintenance backstops (`retry_disconnected_detaches`, the
        // fallback unregister) legitimately run without one because no live
        // connection exists to race.
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
                let can_acknowledge = self.complete_local_detach(player_id, &room_id).await;
                drop(room_event_guard);
                if send_notifications && can_acknowledge {
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
        let can_acknowledge = self.complete_local_detach(player_id, &room_id).await;

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
            // The roster is u8-capped (`max_spectators`), so the saturating
            // fallback is unreachable; it keeps the explicit-narrowing lint
            // honest without a panic path.
            spectator_count: Some(u32::try_from(current_spectators.len()).unwrap_or(u32::MAX)),
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

                    let should_acknowledge =
                        || can_acknowledge && !*acknowledgement_predicate_drain.borrow();
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
#[path = "spectator_service_tests.rs"]
mod tests;
