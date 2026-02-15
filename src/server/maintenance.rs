use crate::protocol::RoomId;

use super::{chrono_duration_from_std, EnhancedGameServer};

impl EnhancedGameServer {
    /// Log that a room has been closed during cleanup.
    pub(crate) fn publish_room_closed(&self, room_id: RoomId, reason: &str) {
        tracing::debug!(%room_id, %reason, "Room closed");
    }

    /// Enhanced cleanup task with distributed coordination and idempotency
    ///
    /// In multi-instance deployments, this task uses idempotency keys to ensure
    /// that post-cleanup operations (event publishing, relay session cleanup,
    /// application mapping cleanup) only happen once per room, even if multiple
    /// instances attempt cleanup simultaneously.
    pub async fn cleanup_task(&self) {
        let mut interval = tokio::time::interval(self.config.room_cleanup_interval);
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
