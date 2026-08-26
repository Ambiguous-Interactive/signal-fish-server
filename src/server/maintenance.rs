use crate::coordination::CloseReason;
use crate::protocol::{ErrorCode, PlayerId, RoomId};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::{chrono_duration_from_std, EnhancedGameServer};

impl EnhancedGameServer {
    /// Retry player-row removals that failed after a dead connection had to be
    /// unrouted. The room mutation gate orders this check with reconnect/join,
    /// preventing cleanup from deleting a newly restored live membership.
    pub(crate) async fn cleanup_pending_durable_player_detaches(&self) -> usize {
        let candidates: Vec<(RoomId, PlayerId)> = self
            .pending_durable_player_detaches
            .iter()
            .map(|entry| *entry.key())
            .collect();
        let mut cleaned = 0_usize;

        for (room_id, player_id) in candidates {
            let _guard = self
                .message_coordinator
                .lock_room_event_mutation(&room_id)
                .await;
            if self
                .connection_manager
                .current_relay_stamp_in_room(&player_id, &room_id)
                .is_some()
            {
                self.pending_durable_player_detaches
                    .remove(&(room_id, player_id));
                continue;
            }

            if matches!(self.database.get_room_by_id(&room_id).await, Ok(None)) {
                if self
                    .pending_durable_player_detaches
                    .remove(&(room_id, player_id))
                    .is_some()
                {
                    cleaned = cleaned.saturating_add(1);
                }
                continue;
            }

            // Re-read rollback provenance only after entering the room event
            // lane. A successfully published same-app admission clears it
            // under this same lane so a stale pre-lock snapshot cannot erase
            // ownership that admission adopted.
            let application_claim_rollback = self
                .pending_durable_player_detaches
                .get(&(room_id, player_id))
                .and_then(|entry| entry.value().clone());

            match self
                .database
                .remove_player_from_room(&room_id, &player_id)
                .await
            {
                Ok(_) => {
                    if let Some(rollback) = application_claim_rollback.as_ref() {
                        if let Err(error) = self
                            .rollback_room_application_claim_if_unchanged(&room_id, rollback)
                            .await
                        {
                            tracing::warn!(%player_id, %room_id, %error, "Failed pending room application-claim rollback; retaining it for retry");
                            continue;
                        }
                    }
                    if self
                        .pending_durable_player_detaches
                        .remove(&(room_id, player_id))
                        .is_some()
                    {
                        cleaned = cleaned.saturating_add(1);
                    }
                }
                Err(error) => {
                    tracing::warn!(%player_id, %room_id, %error, "Failed pending durable player detach; retaining it for retry");
                }
            }
        }

        cleaned
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
            match self.database.get_room_by_id(&room_id).await {
                Ok(Some(_)) => {}
                Ok(None) => {
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
        match &self.reconnection_manager {
            Some(manager) => {
                let protected = manager.room_gc_protection().await;
                self.database
                    .cleanup_empty_rooms(empty_timeout, protected.room_ids())
                    .await
            }
            None => {
                self.database
                    .cleanup_empty_rooms(empty_timeout, &HashSet::new())
                    .await
            }
        }
    }

    /// Run the count-only room sweep under a fresh pinned reconnection view.
    /// Post-cleanup work can be slow, so each database sweep gets its own guard
    /// rather than blocking new disconnect registrations for the whole tick.
    pub(crate) async fn cleanup_expired_rooms_protecting_reconnections(
        &self,
        empty_timeout: chrono::Duration,
        inactive_timeout: chrono::Duration,
    ) -> anyhow::Result<crate::database::RoomCleanupOutcome> {
        match &self.reconnection_manager {
            Some(manager) => {
                let protected = manager.room_gc_protection().await;
                self.database
                    .cleanup_expired_rooms(empty_timeout, inactive_timeout, protected.room_ids())
                    .await
            }
            None => {
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
            match self.database.get_room_by_id(&room_id).await {
                Ok(Some(_)) => {}
                Ok(None) => {
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
    /// Unlike [`Self::cleanup_pending_durable_player_detaches`] this does not
    /// take the room mutation gate, and does not need to: a candidate is
    /// expired and unclaimed, expiry is monotonic, and `claim_reconnection`
    /// refuses an expired record — so no reconnect can restore this membership
    /// after the snapshot, and the removal can only ever delete a row whose
    /// owner is already gone. The detach backlog takes the gate because its
    /// candidates have no such proof: a live reconnect legitimately reclaims
    /// one, and the gate is what orders that claim against this deletion.
    pub(crate) async fn cleanup_expired_reconnections(&self) -> usize {
        let Some(reconnection_manager) = &self.reconnection_manager else {
            return 0;
        };

        let mut count = 0_usize;
        for (player_id, room_id) in reconnection_manager.expired_cleanup_candidates().await {
            match self
                .database
                .remove_player_from_room(&room_id, &player_id)
                .await
            {
                Ok(_) => {
                    if reconnection_manager
                        .remove_expired_reconnection(&player_id)
                        .await
                    {
                        count = count.saturating_add(1);
                    }
                }
                Err(error) => {
                    tracing::warn!(%player_id, %room_id, %error, "Failed durable cleanup for expired reconnection; retaining it for retry");
                }
            }
        }
        if count > 0 {
            tracing::info!(
                count,
                instance_id = %self.instance_id,
                "Cleaned up expired reconnection records"
            );
        }
        count
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
        let mut candidates = HashMap::<RoomId, Vec<PlayerId>>::new();
        for player_id in self.connection_manager.client_ids() {
            if let Some(room_id) = self.connection_manager.get_client_room(&player_id) {
                candidates.entry(room_id).or_default().push(player_id);
            }
        }

        let mut cleaned = 0usize;
        for (room_id, player_ids) in candidates {
            match self.database.get_room_by_id(&room_id).await {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%room_id, %error, "Failed to verify assigned room during cleanup; retaining clients");
                    continue;
                }
            }

            for player_id in player_ids {
                if self.is_draining() {
                    return cleaned;
                }
                if self
                    .cleanup_client_in_missing_room(player_id, room_id)
                    .await
                {
                    cleaned = cleaned.saturating_add(1);
                }
            }
        }
        cleaned
    }

    async fn cleanup_client_in_missing_room(
        self: &Arc<Self>,
        player_id: PlayerId,
        expected_room: RoomId,
    ) -> bool {
        let Some(lifecycle) = self.connection_manager.client_lifecycle(&player_id) else {
            return false;
        };
        let _lifecycle_guard = Arc::clone(&lifecycle).lock_owned().await;
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
        match self.database.get_room_by_id(&expected_room).await {
            Ok(Some(_)) => return false,
            Ok(None) => {}
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
        let enqueued = self
            .send_farewell_to_player_if(
                &effective_player_id,
                "Room closed after exceeding the inactivity timeout".to_string(),
                Some(ErrorCode::RoomNotFound),
                &should_send,
            )
            .await;

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
        // Clamp to a 1s floor: `room_cleanup_interval` is validated `> 0` at
        // startup (`validate_config_security`), but guard here too because the
        // server is constructible directly via the public API, and
        // `tokio::time::interval` panics on a zero period (mirrors the
        // dashboard-cache `.max(..)` zero-guard).
        let mut interval = tokio::time::interval(
            self.config
                .room_cleanup_interval
                .max(std::time::Duration::from_secs(1)),
        );
        let empty_timeout = chrono_duration_from_std(self.config.empty_room_timeout);
        let inactive_timeout = chrono_duration_from_std(self.config.inactive_room_timeout);
        tokio::pin!(shutdown);

        'cleanup: loop {
            tokio::select! {
                () = &mut shutdown => {
                    tracing::info!(
                        instance_id = %self.instance_id,
                        "Cleanup task stopping for server shutdown"
                    );
                    break;
                }
                _ = interval.tick() => {}
            }

            if self.is_draining() {
                tracing::info!(
                    instance_id = %self.instance_id,
                    "Cleanup task stopping for server shutdown drain"
                );
                break 'cleanup;
            }

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
                break 'cleanup;
            }
            let mut evicted_client_count = 0u64;
            let mut stop_for_drain = false;
            for player_id in expired_clients {
                if self.is_draining() {
                    tracing::info!(
                        instance_id = %self.instance_id,
                        "Cleanup task stopping for server shutdown drain during activity eviction"
                    );
                    stop_for_drain = true;
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
                        && self.connection_manager.request_activity_timeout_if_expired(
                            &player_id,
                            self.config.ping_timeout,
                        );
                    eviction_pinned.store(pinned, Ordering::Release);
                    pinned
                };
                let enqueued = self
                    .send_farewell_to_player_if(
                        &player_id,
                        format!(
                            "Disconnected: no activity received for {timeout_ms} ms \
                             (server.ping_timeout)"
                        ),
                        // Deliberately NOT ConnectionIdleTimeout: that code is
                        // the socket-level `websocket.idle_timeout_secs` close;
                        // this eviction is the activity reaper's.
                        Some(crate::protocol::ErrorCode::ActivityTimeout),
                        &should_send,
                    )
                    .await;

                let mut pinned = eviction_pinned.load(Ordering::Acquire);
                if !pinned {
                    if self.is_draining() {
                        tracing::info!(
                            instance_id = %self.instance_id,
                            "Cleanup task stopping for server shutdown drain after activity farewell"
                        );
                        stop_for_drain = true;
                        break;
                    }
                    // A missing coordinator route does not evaluate the
                    // predicate. Close the still-expired socket without an
                    // advisory, preserving the same pin-before-teardown
                    // ordering.
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
                self.unregister_client(&player_id).await;
            }
            if evicted_client_count > 0 {
                self.metrics
                    .add_expired_players_cleaned(evicted_client_count);
            }
            if stop_for_drain {
                break 'cleanup;
            }

            let detached_spectators = self.spectator_service.retry_disconnected_detaches().await;
            if detached_spectators > 0 {
                tracing::info!(
                    count = detached_spectators,
                    instance_id = %self.instance_id,
                    "Retried durable detach for disconnected spectators"
                );
            }

            self.cleanup_pending_durable_player_detaches().await;

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

            let cleaned_missing_room_clients = self.cleanup_clients_in_missing_rooms().await;
            if cleaned_missing_room_clients > 0 {
                tracing::info!(
                    count = cleaned_missing_room_clients,
                    instance_id = %self.instance_id,
                    "Terminally closed clients assigned to removed rooms"
                );
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
                    if count > 0 {
                        tracing::info!(count, instance_id = %self.instance_id, "Cleaned up expired distributed locks");
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to cleanup expired locks: {}", e);
                }
            }

            self.cleanup_expired_reconnections().await;

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
        }
    }
}
