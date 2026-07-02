use crate::protocol::RoomId;

use super::{chrono_duration_from_std, EnhancedGameServer};

impl EnhancedGameServer {
    /// Log that a room has been closed during cleanup.
    pub(crate) fn publish_room_closed(&self, room_id: RoomId, reason: &str) {
        tracing::debug!(%room_id, %reason, "Room closed");
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
        let mut removed = 0;
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
                        removed += 1;
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

    pub(crate) async fn cleanup_expired_reconnections(&self) -> usize {
        let Some(reconnection_manager) = &self.reconnection_manager else {
            return 0;
        };

        let count = reconnection_manager.cleanup_expired().await;
        if count > 0 {
            tracing::info!(
                count,
                instance_id = %self.instance_id,
                "Cleaned up expired reconnection records"
            );
        }
        count
    }

    /// Enhanced cleanup task with distributed coordination and idempotency
    ///
    /// In multi-instance deployments, this task uses idempotency keys to ensure
    /// that post-cleanup operations (event publishing, relay session cleanup,
    /// application mapping cleanup) only happen once per room, even if multiple
    /// instances attempt cleanup simultaneously.
    pub async fn cleanup_task(&self) {
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

        loop {
            interval.tick().await;

            // Cleanup expired clients
            let expired_clients = self
                .connection_manager
                .collect_expired_clients(self.config.ping_timeout);

            let expired_client_count = expired_clients.len() as u64;
            if expired_client_count > 0 {
                self.metrics
                    .add_expired_players_cleaned(expired_client_count);
            }

            // Loud eviction: tell each expired client WHY it is being closed
            // before the unregister tears its socket down. Farewells are
            // best-effort by contract — they never wait on a full queue and
            // never reclassify the close as a slow-consumer disconnect (the
            // eviction itself is the authoritative signal) — so this sweep is
            // non-blocking regardless of how many clients expired at once.
            for player_id in &expired_clients {
                let timeout_secs = self.config.ping_timeout.as_secs();
                let enqueued = self
                    .send_farewell_to_player(
                        player_id,
                        format!(
                            "Disconnected: no activity received for {timeout_secs} seconds \
                             (server.ping_timeout)"
                        ),
                        // Deliberately NOT ConnectionIdleTimeout: that code is
                        // the socket-level `websocket.idle_timeout_secs` close;
                        // this eviction is the activity reaper's.
                        Some(crate::protocol::ErrorCode::ActivityTimeout),
                    )
                    .await;
                if !enqueued {
                    tracing::debug!(
                        %player_id,
                        "Expired client did not receive the eviction farewell (queue full or gone)"
                    );
                }
            }

            for player_id in expired_clients {
                tracing::info!(%player_id, instance_id = %self.instance_id, "Removing expired client");
                self.unregister_client(&player_id).await;
            }

            // Cleanup empty rooms with idempotency
            match self.database.cleanup_empty_rooms(empty_timeout).await {
                Ok(deleted_room_ids) => {
                    let count = deleted_room_ids.len();
                    if count > 0 {
                        tracing::info!(
                            count,
                            instance_id = %self.instance_id,
                            "Cleaned up empty rooms"
                        );
                        self.metrics.add_empty_rooms_cleaned(count as u64);

                        // Process post-cleanup operations with idempotency check
                        for room_id in &deleted_room_ids {
                            // The stored v3 session decision is per-node in-memory
                            // state, so it is dropped unconditionally for every
                            // deleted room — independent of the cross-instance
                            // idempotency claim below.
                            self.clear_active_session_plan(room_id);

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
                                self.clear_room_application(room_id).await;
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
                .database
                .cleanup_expired_rooms(empty_timeout, inactive_timeout)
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
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::error!("Failed to cleanup expired rooms: {}", e);
                }
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
