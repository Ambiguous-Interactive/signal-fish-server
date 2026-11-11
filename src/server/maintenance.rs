use crate::coordination::{CloseReason, RoomEventCompletion, RoomEventMutationGuard};
use crate::protocol::{ErrorCode, PlayerId, PlayerInfo, RoomId, ServerMessage};
use futures_util::FutureExt;
use std::collections::HashSet;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::maintenance_workers::MaintenanceWorkers;
use super::room_service::ROOM_JOIN_LOCK_TTL;
use super::{chrono_duration_from_std, EnhancedGameServer};

/// Limit one candidate's wait before maintenance mutates durable or connection state.
const MAINTENANCE_ADMISSION_WAIT: std::time::Duration = std::time::Duration::from_millis(100);
/// Share one observation deadline across activity farewells and teardowns.
const ACTIVITY_EVICTION_SWEEP_WAIT: std::time::Duration = std::time::Duration::from_millis(100);
/// Bound deleted-room advice before requesting the authoritative socket close.
const MISSING_ROOM_FAREWELL_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

/// One room's latest confirmed role release awaiting publication.
pub(super) struct PendingAuthorityRelease {
    player_id: PlayerId,
}

impl EnhancedGameServer {
    /// Abandon only admission waits; an admitted mutation retains its normal completion rules.
    async fn admit_maintenance<T>(&self, admission: impl Future<Output = T>) -> Option<T> {
        tokio::time::timeout(MAINTENANCE_ADMISSION_WAIT, admission)
            .await
            .ok()
    }

    /// Reclaim unpublished rooms after a failed or interrupted creation, including during drain.
    /// The code lock excludes a creator still preparing its first response;
    /// storage rechecks publication before deleting a stale scan result.
    pub(crate) async fn cleanup_abandoned_rooms(self: &Arc<Self>) -> usize {
        self.abandoned_room_workers
            .run_counted(
                vec![(RoomId::nil(), PlayerId::nil())],
                MaintenanceWorkers::deadline(),
                None,
                |_| {
                    let server = Arc::clone(self);
                    async move { server.cleanup_abandoned_rooms_owned().await }
                },
            )
            .await
    }

    /// Discover pending rooms and observe their independently retained deletion jobs.
    async fn cleanup_abandoned_rooms_owned(self: &Arc<Self>) -> usize {
        let mut room_ids = match self.database.pending_room_ids().await {
            Ok(ids) => ids,
            Err(error) => {
                tracing::warn!(%error, "Failed to list abandoned rooms for cleanup");
                Vec::new()
            }
        };
        room_ids.extend(
            self.pending_creation_rollbacks
                .iter()
                .map(|entry| *entry.key()),
        );
        room_ids.sort_unstable();
        room_ids.dedup();
        self.abandoned_room_deletion_workers
            .run(
                room_ids
                    .into_iter()
                    .map(|room| (room, PlayerId::nil()))
                    .collect(),
                MaintenanceWorkers::deadline(),
                None,
                |(room, _)| {
                    let server = Arc::clone(self);
                    async move { server.cleanup_abandoned_room(room).await }
                },
            )
            .await
    }

    /// Revalidate and reclaim one room without delaying other admitted rooms.
    async fn cleanup_abandoned_room(&self, room_id: RoomId) -> bool {
        let room = match self.database.get_room_by_id(&room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                self.pending_creation_rollbacks.remove(&room_id);
                return false;
            }
            Err(error) => {
                tracing::warn!(%room_id, %error, "Failed to read unpublished room for cleanup");
                return false;
            }
        };
        let lock_key = format!("room_join:{}:{}", room.game_name, room.code);
        let ttl = self.coordination_lock_ttl(ROOM_JOIN_LOCK_TTL);
        let lock_handle = match self.distributed_lock.try_acquire(&lock_key, ttl).await {
            Ok(Some(handle)) => handle,
            Ok(None) => return false,
            Err(error) => {
                tracing::warn!(%room_id, %error, "Failed to lock unpublished room for cleanup");
                return false;
            }
        };
        let mut lock_renewal = self.keep_lock_renewed(lock_handle, ttl);
        // Follow admission's code-lock -> room-lane order. A failed baseline
        // may already be published, so pending-only storage scans are not enough.
        let Some(_room_guard) = self
            .admit_maintenance(self.message_coordinator.lock_room_event_mutation(&room_id))
            .await
        else {
            self.release_renewed_lock(&mut lock_renewal).await;
            return false;
        };
        let failed_creator = self
            .pending_creation_rollbacks
            .get(&room_id)
            .map(|entry| *entry.value());
        let deletion = AssertUnwindSafe(async {
            if let Some(creator) = failed_creator {
                match self.database.get_room_by_id(&room_id).await? {
                    None => {
                        self.pending_creation_rollbacks.remove(&room_id);
                        return Ok(false);
                    }
                    Some(current)
                        if current.spectators.is_empty()
                            && (current.players.is_empty()
                                || (current.players.len() == 1
                                    && current.players.contains_key(&creator))) =>
                    {
                        return self.database.delete_room(&room_id).await;
                    }
                    // Never remove another member admitted by an external writer.
                    Some(_) => return Ok(false),
                }
            }
            self.database.delete_room_if_pending(&room_id).await
        })
        .catch_unwind()
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("Abandoned room deletion panicked")));
        let deleted = match deletion {
            Ok(true) => {
                self.metrics.add_rooms_deleted(1);
                self.room_applications.remove(&room_id);
                self.pending_creation_rollbacks.remove(&room_id);
                true
            }
            Ok(false) => false,
            Err(error) => {
                tracing::warn!(%room_id, %error, "Failed to delete abandoned room; retaining it for retry");
                false
            }
        };
        self.release_renewed_lock(&mut lock_renewal).await;
        deleted
    }

    /// Retry player-row removals that failed after a dead connection had to be
    /// unrouted. The room mutation gate orders this check with reconnect/join,
    /// preventing cleanup from deleting a newly restored live membership.
    pub(crate) async fn cleanup_pending_durable_player_detaches(self: &Arc<Self>) -> usize {
        let deadline = MaintenanceWorkers::deadline();
        let candidates: Vec<(RoomId, PlayerId)> = self
            .pending_durable_player_detaches
            .iter()
            .map(|entry| *entry.key())
            .chain(
                self.pending_authority_releases
                    .iter()
                    .map(|entry| (*entry.key(), entry.value().player_id)),
            )
            .collect();
        self.durable_detach_workers
            .run(candidates, deadline, None, |(room_id, player_id)| {
                let server = Arc::clone(self);
                async move {
                    server
                        .cleanup_pending_durable_player_detach(room_id, player_id)
                        .await
                }
            })
            .await
    }

    /// Remove one unrouted residue and publish a confirmed authority departure.
    async fn cleanup_pending_durable_player_detach(
        &self,
        room_id: RoomId,
        player_id: PlayerId,
    ) -> bool {
        let Some(guard) = self
            .admit_maintenance(self.message_coordinator.lock_room_event_mutation(&room_id))
            .await
        else {
            return false;
        };
        if !self
            .pending_durable_player_detaches
            .contains_key(&(room_id, player_id))
        {
            if let Some(obligation) = self
                .pending_authority_releases
                .get(&room_id)
                .map(|entry| Arc::clone(entry.value()))
            {
                let completion = self.enqueue_pending_authority_release(room_id, obligation, guard);
                if let Err(error) = completion.await {
                    tracing::warn!(%room_id, %player_id, %error, "Failed to retry repaired authority departure");
                }
            }
            return false;
        }
        if self
            .connection_manager
            .current_relay_stamp_in_room(&player_id, &room_id)
            .is_some()
        {
            self.pending_durable_player_detaches
                .remove(&(room_id, player_id));
            return false;
        }
        if matches!(self.database.room_exists(&room_id).await, Ok(false)) {
            return self
                .pending_durable_player_detaches
                .remove(&(room_id, player_id))
                .is_some();
        }
        let application_claim_rollback = self
            .pending_durable_player_detaches
            .get(&(room_id, player_id))
            .and_then(|entry| entry.value().clone());
        let removed = match self
            .database
            .remove_player_from_room(&room_id, &player_id)
            .await
        {
            Ok(removed) => removed,
            Err(error) => {
                tracing::warn!(%player_id, %room_id, %error, "Failed pending durable player detach; retaining it for retry");
                return false;
            }
        };
        // Keep the publication evidence before application rollback can wait
        // or unwind after storage has removed the only authority snapshot.
        let authority_release = self
            .retain_removed_member_authority(&room_id, removed.as_ref())
            .or_else(|| {
                self.pending_authority_releases
                    .get(&room_id)
                    .map(|entry| Arc::clone(entry.value()))
            });
        let rollback_result = if let Some(rollback) = application_claim_rollback.as_ref() {
            self.rollback_room_application_claim_if_unchanged(&room_id, rollback)
                .await
        } else {
            Ok(())
        };
        let completion = authority_release
            .map(|obligation| self.enqueue_pending_authority_release(room_id, obligation, guard));
        let cleaned = match rollback_result {
            Ok(()) => self
                .pending_durable_player_detaches
                .remove(&(room_id, player_id))
                .is_some(),
            Err(error) => {
                tracing::warn!(%player_id, %room_id, %error, "Failed pending room application-claim rollback; retaining it for retry");
                false
            }
        };
        if let Some(completion) = completion {
            if let Err(error) = completion.await {
                tracing::warn!(%player_id, %room_id, %error, "Failed to publish repaired authority departure");
            }
        }
        cleaned
    }

    /// Keep only the latest confirmed authority release for each room.
    fn retain_removed_member_authority(
        &self,
        room_id: &RoomId,
        removed: Option<&PlayerInfo>,
    ) -> Option<Arc<PendingAuthorityRelease>> {
        let player_id = removed.filter(|player| player.is_authority)?.id;
        let obligation = Arc::new(PendingAuthorityRelease { player_id });
        self.pending_authority_releases
            .insert(*room_id, Arc::clone(&obligation));
        Some(obligation)
    }

    /// Retain the obligation until its replay and live publication commit.
    fn enqueue_pending_authority_release(
        &self,
        room_id: RoomId,
        obligation: Arc<PendingAuthorityRelease>,
        guard: RoomEventMutationGuard,
    ) -> RoomEventCompletion {
        let player_id = obligation.player_id;
        let drain = self.shutdown_drain_receiver();
        let predicate_drain = drain.clone();
        let coordinator = Arc::clone(&self.message_coordinator);
        let replay = self.reconnection_manager.clone();
        let pending = Arc::clone(&self.pending_authority_releases);
        let database = Arc::clone(&self.database);
        self.message_coordinator.enqueue_room_event(
            guard,
            Box::new(move || {
                Box::pin(async move {
                    if !pending
                        .get(&room_id)
                        .is_some_and(|entry| Arc::ptr_eq(entry.value(), &obligation))
                    {
                        return Ok(false);
                    }
                    // A new authority supersedes the delayed clear. The room
                    // gate keeps normal authority transitions behind this check.
                    let authority_is_vacant = database
                        .get_room_by_id(&room_id)
                        .await?
                        .is_some_and(|room| room.authority_player.is_none());
                    if !authority_is_vacant || *predicate_drain.borrow() {
                        pending.remove_if(&room_id, |_, current| Arc::ptr_eq(current, &obligation));
                        return Ok(false);
                    }
                    let message = Arc::new(ServerMessage::AuthorityChanged {
                        authority_player: None,
                        you_are_authority: false,
                    });
                    let replay_message = Arc::clone(&message);
                    let should_send = || !*predicate_drain.borrow();
                    coordinator
                        .broadcast_to_room_except_if_with_hook(
                            &room_id,
                            &player_id,
                            message,
                            &should_send,
                            drain,
                            Box::new(move || {
                                Box::pin(async move {
                                    if let Some(replay) = replay {
                                        replay
                                            .record_room_event(&room_id, replay_message.as_ref())
                                            .await;
                                    }
                                    pending.remove_if(&room_id, |_, current| {
                                        Arc::ptr_eq(current, &obligation)
                                    });
                                })
                            }),
                        )
                        .await
                })
            }),
        )
    }

    /// Drop process-local application mappings for rooms removed by any
    /// storage cleanup path, including count-only inactive-room cleanup.
    pub(crate) async fn prune_room_applications(&self) -> usize {
        let room_ids: Vec<RoomId> = self
            .room_applications
            .iter()
            .map(|entry| *entry.key())
            .collect();
        let mut removed = 0usize;
        for room_id in room_ids {
            match self.database.room_exists(&room_id).await {
                Ok(true) => {}
                Ok(false) => {
                    if self.room_applications.remove(&room_id).is_some() {
                        removed = removed.saturating_add(1);
                    }
                }
                Err(error) => {
                    tracing::warn!(%room_id, %error, "Failed to verify cached room application mapping; retaining it for retry");
                }
            }
        }
        removed
    }

    /// Log that a room has been closed during cleanup.
    pub(crate) fn publish_room_closed(&self, room_id: RoomId, reason: &str) {
        tracing::debug!(%room_id, %reason, "Room closed");
    }

    /// Delete empty rooms while pinning the reconnection snapshot that
    /// authorizes the deletion. A disconnect registration needs the matching
    /// write lock, so it must happen wholly before this snapshot or after the
    /// storage sweep instead of appearing in the deletion race window.
    pub(crate) async fn cleanup_empty_rooms_protecting_reconnections(
        &self,
        empty_timeout: chrono::Duration,
    ) -> anyhow::Result<Vec<RoomId>> {
        let prepared = self.database.lease_room_cleanup().await?;
        match (prepared, &self.reconnection_manager) {
            (Some(lease), Some(manager)) => {
                let Some(protected) = manager.try_room_gc_protection() else {
                    return Ok(Vec::new());
                };
                Ok(lease.cleanup_empty(empty_timeout, protected.room_ids()))
            }
            (None, Some(manager)) => {
                let protected = manager.room_gc_protection().await;
                self.database
                    .cleanup_empty_rooms(empty_timeout, protected.room_ids())
                    .await
            }
            (Some(lease), None) => Ok(lease.cleanup_empty(empty_timeout, &HashSet::new())),
            (None, None) => {
                self.database
                    .cleanup_empty_rooms(empty_timeout, &HashSet::new())
                    .await
            }
        }
    }

    /// Prepare storage before pinning the current reconnect view for an atomic commit.
    pub(crate) async fn cleanup_expired_rooms_protecting_reconnections(
        &self,
        empty_timeout: chrono::Duration,
        inactive_timeout: chrono::Duration,
    ) -> anyhow::Result<crate::database::RoomCleanupOutcome> {
        let prepared = self.database.lease_room_cleanup().await?;
        match (prepared, &self.reconnection_manager) {
            (Some(lease), Some(manager)) => {
                let Some(protected) = manager.try_room_gc_protection() else {
                    return Ok(crate::database::RoomCleanupOutcome::default());
                };
                Ok(lease.cleanup_expired(empty_timeout, inactive_timeout, protected.room_ids()))
            }
            (None, Some(manager)) => {
                let protected = manager.room_gc_protection().await;
                self.database
                    .cleanup_expired_rooms(empty_timeout, inactive_timeout, protected.room_ids())
                    .await
            }
            (Some(lease), None) => {
                Ok(lease.cleanup_expired(empty_timeout, inactive_timeout, &HashSet::new()))
            }
            (None, None) => {
                self.database
                    .cleanup_expired_rooms(empty_timeout, inactive_timeout, &HashSet::new())
                    .await
            }
        }
    }

    /// Drop the coordinator's in-memory ready-state entries for rooms that no
    /// longer exist in storage. Returns the number of entries pruned.
    ///
    /// This mirrors [`Self::prune_active_session_plans`]: `cleanup_expired_rooms`
    /// reports only counts (no per-room ids) and removes inactive rooms even when
    /// they still had members, so this sweep is the guaranteed reclaim for every
    /// room-removal path. A transient storage error keeps the entry for the next
    /// tick rather than risk clearing a live room's ready set.
    pub(crate) async fn prune_ready_players(&self) -> usize {
        let room_ids = self.room_coordinator.ready_player_room_ids().await;
        let mut removed = 0usize;
        for room_id in room_ids {
            match self.database.room_exists(&room_id).await {
                Ok(true) => {}
                Ok(false) => {
                    if self
                        .room_coordinator
                        .clear_ready_players(&room_id)
                        .await
                        .is_ok()
                    {
                        removed = removed.saturating_add(1);
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        %room_id,
                        error = %err,
                        "Failed to check room existence while pruning ready players"
                    );
                }
            }
        }
        removed
    }

    /// Discard reconnect reservations whose window has closed, removing the
    /// room row each one was holding.
    ///
    /// The room mutation gate excludes fresh admissions. Revalidate the
    /// expired record under the replay-state write lock, then retain its
    /// record cleanup pin through storage so new disconnections cannot replace it.
    pub(crate) async fn cleanup_expired_reconnections(self: &Arc<Self>) -> usize {
        let Some(reconnection_manager) = &self.reconnection_manager else {
            return 0;
        };

        let deadline = MaintenanceWorkers::deadline();
        let candidates = MaintenanceWorkers::observe(
            deadline,
            reconnection_manager.expired_cleanup_candidates(),
        )
        .await
        .unwrap_or_default();
        self.cleanup_expired_reconnection_candidates_until(candidates, deadline)
            .await
    }

    /// Process supplied expiry hints through the same bounded worker queue.
    #[cfg(all(test, signal_fish_repository_tests))]
    pub(super) async fn cleanup_expired_reconnection_candidates(
        self: &Arc<Self>,
        candidates: Vec<(PlayerId, RoomId)>,
    ) -> usize {
        self.cleanup_expired_reconnection_candidates_until(
            candidates,
            MaintenanceWorkers::deadline(),
        )
        .await
    }

    /// Share the worker deadline across expired seats and retained authority notices.
    async fn cleanup_expired_reconnection_candidates_until(
        self: &Arc<Self>,
        candidates: Vec<(PlayerId, RoomId)>,
        deadline: Option<tokio::time::Instant>,
    ) -> usize {
        let candidates: Vec<_> = candidates
            .into_iter()
            .map(|(player, room)| (room, player))
            .chain(
                self.pending_authority_releases
                    .iter()
                    .map(|entry| (*entry.key(), entry.value().player_id)),
            )
            .collect();
        let count = self
            .expired_reconnect_workers
            .run(candidates, deadline, None, |(room_id, player_id)| {
                let server = Arc::clone(self);
                async move {
                    server
                        .cleanup_expired_reconnection_candidate(player_id, room_id)
                        .await
                }
            })
            .await;
        if count > 0 {
            tracing::info!(count, instance_id = %self.instance_id, "Cleaned up expired reconnection records");
        }
        count
    }

    /// Remove an eligible expired seat and publish its confirmed authority departure.
    async fn cleanup_expired_reconnection_candidate(
        &self,
        player_id: PlayerId,
        room_id: RoomId,
    ) -> bool {
        let Some(reconnection_manager) = &self.reconnection_manager else {
            return false;
        };
        let Some(guard) = self
            .admit_maintenance(self.message_coordinator.lock_room_event_mutation(&room_id))
            .await
        else {
            return false;
        };
        if self
            .connection_manager
            .current_relay_stamp_in_room(&player_id, &room_id)
            .is_some()
        {
            return false;
        }
        match reconnection_manager
            .cleanup_expired_membership(
                &player_id,
                &room_id,
                self.database.remove_player_from_room(&room_id, &player_id),
            )
            .await
        {
            Ok(Some(removed)) => {
                let obligation = self
                    .retain_removed_member_authority(&room_id, removed.as_ref())
                    .or_else(|| {
                        self.pending_authority_releases
                            .get(&room_id)
                            .map(|entry| Arc::clone(entry.value()))
                    });
                if let Some(obligation) = obligation {
                    let completion =
                        self.enqueue_pending_authority_release(room_id, obligation, guard);
                    if let Err(error) = completion.await {
                        tracing::warn!(%player_id, %room_id, %error, "Failed to publish expired authority departure");
                    }
                }
                true
            }
            Ok(None) => {
                if let Some(obligation) = self
                    .pending_authority_releases
                    .get(&room_id)
                    .map(|entry| Arc::clone(entry.value()))
                {
                    let completion =
                        self.enqueue_pending_authority_release(room_id, obligation, guard);
                    if let Err(error) = completion.await {
                        tracing::warn!(%player_id, %room_id, %error, "Failed to retry expired authority departure");
                    }
                }
                false
            }
            Err(error) => {
                tracing::warn!(%player_id, %room_id, %error, "Failed durable cleanup for expired reconnection; retaining it for retry");
                false
            }
        }
    }

    /// Terminally reconcile connected players whose assigned room no longer
    /// exists in storage.
    ///
    /// Inactive-room GC intentionally deletes occupied rooms. Storage is the
    /// membership authority, but relay routing and connection assignments are
    /// process-local, so deletion must be followed by an explicit terminal
    /// transition. This all-clients sweep is also the retry path if a cleanup
    /// task is cancelled after the durable delete but before local teardown.
    pub(crate) async fn cleanup_clients_in_missing_rooms(self: &Arc<Self>) -> usize {
        let deadline = MaintenanceWorkers::deadline();
        let candidates = self
            .connection_manager
            .client_ids()
            .into_iter()
            .filter_map(|player_id| {
                self.connection_manager
                    .get_client_room(&player_id)
                    .map(|room_id| (room_id, player_id))
            })
            .collect();
        let cleaned = self.missing_room_workers
            .run(
                candidates,
                deadline,
                Some(self.shutdown_drain_receiver()),
                |(room_id, player_id)| {
                    let server = Arc::clone(self);
                    async move {
                        match server.database.room_exists(&room_id).await {
                            Ok(true) => false,
                            Ok(false) => server.cleanup_client_in_missing_room(player_id, room_id).await,
                            Err(error) => {
                                tracing::warn!(%room_id, %player_id, %error, "Failed to verify assigned room during cleanup; retaining client");
                                false
                            }
                        }
                    }
                },
            )
            .await;
        if cleaned > 0 {
            tracing::info!(
                count = cleaned,
                instance_id = %self.instance_id,
                "Closed clients assigned to removed rooms"
            );
        }
        cleaned
    }

    /// Revalidate the assigned room under the lifecycle gate before closing its client.
    async fn cleanup_client_in_missing_room(
        self: &Arc<Self>,
        player_id: PlayerId,
        expected_room: RoomId,
    ) -> bool {
        let Some(lifecycle) = self.connection_manager.client_lifecycle(&player_id) else {
            return false;
        };
        let Some(_lifecycle_guard) = self
            .admit_maintenance(Arc::clone(&lifecycle).lock_owned())
            .await
        else {
            return false;
        };
        let effective_player_id = lifecycle.player_id();
        if effective_player_id != player_id
            || !self
                .connection_manager
                .lifecycle_matches(&effective_player_id, &lifecycle)
            || self
                .connection_manager
                .get_client_room(&effective_player_id)
                != Some(expected_room)
        {
            return false;
        }

        // The scan above is only a hint. Revalidate under the same lifecycle
        // gate used by join, leave, reconnect, and unregister so a client that
        // moved to a live room cannot be closed from a stale cleanup snapshot.
        match self.database.room_exists(&expected_room).await {
            Ok(true) => return false,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%effective_player_id, %expected_room, %error, "Failed final missing-room cleanup check; retaining client");
                return false;
            }
        }

        let close_pinned = AtomicBool::new(false);
        let should_send = || {
            if close_pinned.load(Ordering::Acquire) {
                return true;
            }
            let pinned = !self.is_draining()
                && self
                    .connection_manager
                    .get_client_room(&effective_player_id)
                    == Some(expected_room)
                && self
                    .connection_manager
                    .request_close_for(&effective_player_id, CloseReason::RoomInactive);
            close_pinned.store(pinned, Ordering::Release);
            pinned
        };
        let enqueued = tokio::time::timeout(
            MISSING_ROOM_FAREWELL_WAIT,
            self.send_farewell_to_player_if(
                &effective_player_id,
                "Room closed after exceeding the inactivity timeout".to_string(),
                Some(ErrorCode::RoomNotFound),
                &should_send,
            ),
        )
        .await
        .unwrap_or(false);

        if self.is_draining() {
            self.connection_manager
                .request_close_for(&effective_player_id, CloseReason::Shutdown);
            return false;
        }
        if !close_pinned.load(Ordering::Acquire) {
            // A missing coordinator route does not evaluate the predicate.
            // The close frame still needs the semantic 4005 attribution even
            // when the best-effort farewell cannot be routed.
            close_pinned.store(
                self.connection_manager
                    .request_close_for(&effective_player_id, CloseReason::RoomInactive),
                Ordering::Release,
            );
        }
        if !enqueued {
            tracing::debug!(%effective_player_id, %expected_room, "Inactive-room farewell was not enqueued (queue full, route gone, or another close owner)");
        }

        // A deleted room cannot be a reconnection target. Clear both token
        // stages before unregister runs so no cancellation point can mint or
        // retain a claim for storage that is already gone.
        self.discard_pre_issued_reconnection_token(&effective_player_id)
            .await;
        if let Some(reconnection_manager) = &self.reconnection_manager {
            reconnection_manager
                .discard_pending_reconnection(&effective_player_id)
                .await;
        }

        self.unregister_client_locked(&effective_player_id).await;
        !self.connection_manager.has_client(&effective_player_id)
    }

    /// Enhanced cleanup task with process-local coordination and idempotency.
    ///
    /// Idempotency keys ensure post-cleanup operations (event publishing, relay
    /// session cleanup, application mapping cleanup) happen once per room in
    /// this process. They do not coordinate cleanup between server processes.
    pub async fn cleanup_task(self: &Arc<Self>) {
        self.cleanup_task_until(std::future::pending::<()>()).await;
    }

    /// Cleanup task variant that exits when `shutdown` resolves.
    pub async fn cleanup_task_until(self: &Arc<Self>, shutdown: impl Future<Output = ()>) {
        let period = self
            .config
            .room_cleanup_interval
            .max(std::time::Duration::from_secs(1));
        let housekeeping = self.cleanup_housekeeping_until(shutdown, period);
        tokio::pin!(housekeeping);

        // A storage wait in housekeeping must not suspend peer and seat recovery.
        let mut repairs = crate::deadline::interval(period);
        repairs.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                () = &mut housekeeping => return,
                _ = repairs.tick() => {}
            }
            if self.is_draining() {
                housekeeping.await;
                return;
            }
            tokio::select! {
                biased;
                () = &mut housekeeping => return,
                () = async {
                    tokio::join!(
                        self.retry_pending_session_replans(),
                        self.cleanup_pending_durable_player_detaches(),
                        self.cleanup_expired_reconnections(),
                        self.cleanup_inactive_clients(),
                        self.cleanup_clients_in_missing_rooms(),
                        self.cleanup_abandoned_rooms(),
                    );
                } => {}
            }
        }
    }

    /// Close inactive connections independently of room storage housekeeping.
    async fn cleanup_inactive_clients(self: &Arc<Self>) {
        // Cleanup expired clients
        let expired_clients = if self.config.ping_timeout.is_zero() {
            Vec::new()
        } else {
            self.connection_manager
                .collect_expired_clients(self.config.ping_timeout)
        };

        if self.is_draining() {
            tracing::info!(
                instance_id = %self.instance_id,
                "Cleanup task stopping for server shutdown drain before activity eviction"
            );
            return;
        }
        let activity_eviction_deadline =
            crate::deadline::after(tokio::time::Instant::now(), ACTIVITY_EVICTION_SWEEP_WAIT);
        let mut evicted_client_count = 0u64;
        for player_id in expired_clients {
            if self.is_draining() {
                tracing::info!(
                    instance_id = %self.instance_id,
                    "Cleanup task stopping for server shutdown drain during activity eviction"
                );
                break;
            }

            // Loud eviction: tell this client WHY it is being closed
            // before unregister tears its socket down. The final enqueue
            // predicate atomically revalidates expiry and pins the close
            // first, so terminal advice can never escape from a stale
            // snapshot while a later pass rescues the connection.
            let eviction_pinned = AtomicBool::new(false);
            // Milliseconds, not truncated seconds: sub-second windows
            // (tests, aggressive deployments) must not read as "0 seconds".
            let timeout_ms = self.config.ping_timeout.as_millis();
            let should_send = || {
                if eviction_pinned.load(Ordering::Acquire) {
                    return true;
                }
                let pinned = !self.is_draining()
                    && self
                        .connection_manager
                        .request_activity_timeout_if_expired(&player_id, self.config.ping_timeout);
                eviction_pinned.store(pinned, Ordering::Release);
                pinned
            };
            let farewell = self.send_farewell_to_player_if(
                &player_id,
                format!(
                    "Disconnected: no activity received for {timeout_ms} ms \
                     (server.ping_timeout)"
                ),
                Some(crate::protocol::ErrorCode::ActivityTimeout),
                &should_send,
            );
            // Give an immediately available route a chance even when an
            // earlier lookup used the batch's wait. The socket close owns
            // the timeout reason if the best-effort lookup is still pending.
            let enqueued = tokio::select! {
                biased;
                enqueued = farewell => enqueued,
                _ = crate::deadline::wait_until(activity_eviction_deadline) => false,
            };

            let mut pinned = eviction_pinned.load(Ordering::Acquire);
            if !pinned {
                if self.is_draining() {
                    tracing::info!(
                        instance_id = %self.instance_id,
                        "Cleanup task stopping for server shutdown drain after activity farewell"
                    );
                    break;
                }
                // A missing or delayed route may never evaluate the
                // predicate. Recheck liveness before closing without advice.
                pinned = self
                    .connection_manager
                    .request_activity_timeout_if_expired(&player_id, self.config.ping_timeout);
            }
            if !pinned {
                tracing::debug!(
                    %player_id,
                    "Client refreshed activity after the cleanup snapshot; skipping eviction"
                );
                continue;
            }
            if !enqueued {
                tracing::debug!(
                    %player_id,
                    "Expired client did not receive the eviction farewell (queue full or gone)"
                );
            }

            evicted_client_count = evicted_client_count.saturating_add(1);
            tracing::info!(%player_id, instance_id = %self.instance_id, "Removing expired client");
            /*
                Poll unregister first so an expired observation budget still
                starts its owned transaction. Dropping the wait leaves it running.
            */
            tokio::select! {
                biased;
                () = self.unregister_client(&player_id) => {}
                _ = crate::deadline::wait_until(activity_eviction_deadline) => {
                    tracing::debug!(%player_id, "Activity-timeout teardown continues outside the maintenance wait");
                }
            }
        }
        if evicted_client_count > 0 {
            self.metrics
                .add_expired_players_cleaned(evicted_client_count);
        }
    }

    /// Run the serial cleanup sweep and finish its current work before exiting.
    async fn cleanup_housekeeping_until(
        self: &Arc<Self>,
        shutdown: impl Future<Output = ()>,
        period: std::time::Duration,
    ) {
        let mut interval = crate::deadline::interval(period);
        let mut draining = self.shutdown_drain_receiver();
        let empty_timeout = chrono_duration_from_std(self.config.empty_room_timeout);
        let inactive_timeout = chrono_duration_from_std(self.config.inactive_room_timeout);
        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                () = &mut shutdown => {
                    tracing::info!(
                        instance_id = %self.instance_id,
                        "Cleanup task stopping for server shutdown"
                    );
                    break;
                }
                _ = draining.changed() => {}
                _ = interval.tick() => {}
            }

            if self.is_draining() {
                tracing::info!(
                    instance_id = %self.instance_id,
                    "Cleanup task stopping for server shutdown drain"
                );
                break;
            }

            // The sweep is timed as one unit so the export shows what a
            // maintenance pass costs at the current occupancy (the C3
            // maintenance-complexity signal). Drain exits skip the
            // stamp: an aborted pass is not a completed sweep.
            let sweep_started = tokio::time::Instant::now();

            let detached_spectators = self.spectator_service.retry_disconnected_detaches().await;
            if detached_spectators > 0 {
                tracing::info!(
                    count = detached_spectators,
                    instance_id = %self.instance_id,
                    "Retried durable detach for disconnected spectators"
                );
            }

            // Cleanup empty rooms with idempotency
            match self
                .cleanup_empty_rooms_protecting_reconnections(empty_timeout)
                .await
            {
                Ok(deleted_room_ids) => {
                    let count = deleted_room_ids.len();
                    if count > 0 {
                        tracing::info!(
                            count,
                            instance_id = %self.instance_id,
                            "Cleaned up empty rooms"
                        );
                        self.metrics.add_empty_rooms_cleaned(count as u64);
                        self.metrics.add_rooms_deleted(count as u64);

                        // Process post-cleanup operations with idempotency check
                        for room_id in &deleted_room_ids {
                            // The stored v3 session decision is per-node in-memory
                            // state, so it is dropped unconditionally for every
                            // deleted room — independent of the cleanup
                            // idempotency claim below.
                            self.clear_active_session_plan(room_id);
                            self.clear_room_application(room_id);

                            // Likewise drop the coordinator's per-node in-memory
                            // ready set for this deleted room. This is the prompt
                            // reclaim on the empty-room path; the all-paths
                            // backstop (for rooms reaped via `cleanup_expired_rooms`,
                            // which reports no ids) is `prune_ready_players` below.
                            // Idempotent.
                            if let Err(e) = self.room_coordinator.clear_ready_players(room_id).await
                            {
                                tracing::warn!(
                                    %room_id,
                                    error = %e,
                                    "Failed to clear ready players during empty-room cleanup"
                                );
                            }

                            // Try to claim the cleanup operation for this room
                            // Only proceed with post-cleanup if we successfully claimed it
                            let should_process = self
                                .database
                                .try_claim_room_cleanup(room_id, "empty_cleanup", &self.instance_id)
                                .await
                                .unwrap_or_else(|e| {
                                    tracing::warn!(
                                        %room_id,
                                        error = %e,
                                        "Failed to check cleanup idempotency, proceeding with cleanup"
                                    );
                                    true // Fail open to maintain backward compatibility
                                });

                            if should_process {
                                self.publish_room_closed(*room_id, "empty_cleanup");
                                // Relay server removed in signal-fish-server
                            } else {
                                tracing::debug!(
                                    %room_id,
                                    instance_id = %self.instance_id,
                                    "Skipping post-cleanup for room (already processed by another instance)"
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to cleanup empty rooms: {}", e);
                }
            }

            match self
                .cleanup_expired_rooms_protecting_reconnections(empty_timeout, inactive_timeout)
                .await
            {
                Ok(outcome) if !outcome.is_empty() => {
                    let total = outcome.total_cleaned();
                    tracing::info!(
                        total,
                        empty = outcome.empty_rooms_cleaned,
                        inactive = outcome.inactive_rooms_cleaned,
                        instance_id = %self.instance_id,
                        "Cleaned up expired rooms"
                    );

                    if outcome.empty_rooms_cleaned > 0 {
                        self.metrics
                            .add_empty_rooms_cleaned(outcome.empty_rooms_cleaned as u64);
                    }
                    if outcome.inactive_rooms_cleaned > 0 {
                        self.metrics
                            .add_inactive_rooms_cleaned(outcome.inactive_rooms_cleaned as u64);
                    }
                    self.metrics
                        .add_rooms_deleted(outcome.total_cleaned() as u64);
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::error!("Failed to cleanup expired rooms: {}", e);
                }
            }

            // Inactive cleanup may deliberately close an occupied room. Its
            // count-only result cannot identify those rooms, so reconcile the
            // spectator role index against storage on every tick.
            let pruned_spectator_roles = self
                .spectator_service
                .prune_missing_rooms(self.shutdown_drain_receiver())
                .await;
            if pruned_spectator_roles > 0 {
                tracing::debug!(
                    count = pruned_spectator_roles,
                    instance_id = %self.instance_id,
                    "Pruned spectator roles for removed rooms"
                );
            }

            // Drop stored v3 session decisions for rooms that no longer exist.
            // `cleanup_expired_rooms` reports only counts (no per-room ids), so
            // this sweep is the guaranteed reclaim for every removal path.
            let pruned_session_plans = self.prune_active_session_plans().await;
            if pruned_session_plans > 0 {
                tracing::debug!(
                    count = pruned_session_plans,
                    instance_id = %self.instance_id,
                    "Pruned stored session plans for removed rooms"
                );
            }

            // App ownership authorization is persisted, but relay policy keeps
            // a process-local cache. Reconcile it for count-only inactive-room
            // cleanup just like session plans and ready state.
            let pruned_room_applications = self.prune_room_applications().await;
            if pruned_room_applications > 0 {
                tracing::debug!(
                    count = pruned_room_applications,
                    instance_id = %self.instance_id,
                    "Pruned application mappings for removed rooms"
                );
            }

            // Drop coordinator ready-state entries for rooms that no longer
            // exist. Like the session-plan sweep above, this is the guaranteed
            // all-paths reclaim — `cleanup_expired_rooms` removes inactive rooms
            // (including non-empty ones) reporting only counts, so neither the
            // per-room empty-cleanup clear nor any departure hook would otherwise
            // catch them.
            let pruned_ready_players = self.prune_ready_players().await;
            if pruned_ready_players > 0 {
                tracing::debug!(
                    count = pruned_ready_players,
                    instance_id = %self.instance_id,
                    "Pruned ready-state entries for removed rooms"
                );
            }

            // Cleanup expired distributed locks
            match self.distributed_lock.cleanup_expired_locks().await {
                Ok(count) => {
                    self.metrics.record_distributed_lock_cleanup(count);
                    if count > 0 {
                        tracing::info!(count, instance_id = %self.instance_id, "Cleaned up expired distributed locks");
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to cleanup expired locks: {}", e);
                }
            }

            // Cleanup old room cleanup events (idempotency tracking table)
            match self.database.cleanup_old_room_cleanup_events().await {
                Ok(count) => {
                    if count > 0 {
                        tracing::debug!(count, instance_id = %self.instance_id, "Cleaned up old room cleanup events");
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to cleanup old room cleanup events: {}", e);
                }
            }

            self.metrics
                .record_maintenance_sweep(sweep_started.elapsed());
        }
    }
}

#[cfg(all(test, signal_fish_repository_tests))]
#[path = "maintenance_admission_tests.rs"]
mod admission_tests;
