use std::sync::Arc;

use crate::coordination::CloseReason;
use crate::database::UpdateRoomCodeError;
use crate::protocol::{ErrorCode, PlayerId, RoomOperationId, RoomOperationResult, ServerMessage};

use super::room_service::ROOM_JOIN_LOCK_TTL;
use super::EnhancedGameServer;

/// Maximum fresh-code candidates tried per `RegenerateRoomCode` request.
/// Matches the generated-room-code creation retry budget
/// (`GENERATED_ROOM_CODE_MAX_ATTEMPTS`).
const REGENERATED_ROOM_CODE_MAX_ATTEMPTS: u8 = 8;

impl EnhancedGameServer {
    /// Handle an authority-initiated `KickPlayer` room operation (v3 only,
    /// issue #525).
    ///
    /// Only the room's designated authority may kick, the target must be a
    /// seated member other than the sender. The removal itself reuses the
    /// ordinary departure machinery (`leave_room_locked`), so the target's
    /// seat receives exactly the same durable removal, reconnection-token
    /// discard, unrouted terminal watermark, and sequenced replay-recorded
    /// `PlayerLeft` broadcast as any other departure — a kicked seat is never
    /// reconnectable. The target's connection closes with the dedicated
    /// `4007 kicked` close code after a best-effort farewell `Error` frame.
    pub(super) async fn handle_kick_player_operation(
        self: &Arc<Self>,
        authority_id: &PlayerId,
        operation_id: RoomOperationId,
        target_id: PlayerId,
    ) {
        // Fix the authority's connection identity and membership with its
        // lifecycle gate (same prologue as every room operation handler).
        let Some(lifecycle) = self.connection_manager.client_lifecycle(authority_id) else {
            return;
        };
        let _authority_lifecycle_guard = lifecycle.lock().await;
        if lifecycle.player_id() != *authority_id
            || !self
                .connection_manager
                .lifecycle_matches(authority_id, &lifecycle)
        {
            return;
        }

        let Some(room_id) = self.get_client_room(authority_id).await else {
            self.fail_operation(
                authority_id,
                operation_id,
                "Not currently in a room",
                ErrorCode::NotInRoom,
            )
            .await;
            return;
        };
        let room = match self.database.get_room_by_id(&room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                self.fail_operation(
                    authority_id,
                    operation_id,
                    "Room no longer exists",
                    ErrorCode::RoomNotFound,
                )
                .await;
                return;
            }
            Err(error) => {
                tracing::error!(%authority_id, %room_id, %error, "Kick failed to load room");
                self.fail_operation(
                    authority_id,
                    operation_id,
                    "Failed to load room",
                    ErrorCode::StorageError,
                )
                .await;
                return;
            }
        };
        if room.authority_player != Some(*authority_id) {
            self.fail_operation(
                authority_id,
                operation_id,
                "Only the room's authority player may kick players",
                ErrorCode::NotRoomAuthority,
            )
            .await;
            return;
        }
        if target_id == *authority_id {
            self.fail_operation(
                authority_id,
                operation_id,
                "The authority cannot kick itself; use LeaveRoom instead",
                ErrorCode::InvalidInput,
            )
            .await;
            return;
        }

        // Hold the target's lifecycle gate across validation and removal:
        // the `leave_room_locked` contract requires the caller to hold the
        // departing connection's lifecycle gate, and holding it here
        // serializes against a concurrent disconnect (which snapshots
        // membership and arms reconnection under the same gate) so neither
        // path can act on a stale seat. A player-id mismatch means a
        // reconnect identity swap is mid-flight; the durable removal is still
        // the convergent outcome (the swapped-in incarnation fails its
        // reconnect against the removed seat), so proceed rather than strand
        // the authority without a terminal result.
        let target_lifecycle = self.connection_manager.client_lifecycle(&target_id);
        let _target_lifecycle_guard = match &target_lifecycle {
            Some(target) => {
                let guard = target.lock().await;
                if target.player_id() != target_id {
                    tracing::debug!(
                        %target_id,
                        %room_id,
                        "Kick raced a target identity swap; proceeding with removal"
                    );
                }
                Some(guard)
            }
            None => None,
        };

        // Fresh storage truth under the gate: the target may have left (or a
        // previous kick landed) between the routing check and now, and the
        // authority may have changed hands. Re-validate both rather than
        // trusting the pre-gate snapshot.
        let room = match self.database.get_room_by_id(&room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                self.fail_operation(
                    authority_id,
                    operation_id,
                    "Room no longer exists",
                    ErrorCode::RoomNotFound,
                )
                .await;
                return;
            }
            Err(error) => {
                tracing::error!(%authority_id, %room_id, %error, "Kick failed to re-load room");
                self.fail_operation(
                    authority_id,
                    operation_id,
                    "Failed to load room",
                    ErrorCode::StorageError,
                )
                .await;
                return;
            }
        };
        if room.authority_player != Some(*authority_id) {
            self.fail_operation(
                authority_id,
                operation_id,
                "Only the room's authority player may kick players",
                ErrorCode::NotRoomAuthority,
            )
            .await;
            return;
        }
        // A disconnect with reconnection enabled removes the durable member
        // and arms a pending record; from the room's perspective that seat is
        // still held until the window closes. Kicking such a seat tombstones
        // the record instead of removing an already-absent row.
        let pending_record_room = match &self.reconnection_manager {
            Some(manager) => manager.pending_reconnection_room(&target_id).await,
            None => None,
        };
        let target_seated_in_row = room.players.contains_key(&target_id);
        if !target_seated_in_row && pending_record_room != Some(room_id) {
            self.fail_operation(
                authority_id,
                operation_id,
                "Kick target is not a member of this room",
                ErrorCode::KickTargetNotFound,
            )
            .await;
            return;
        }

        // Best-effort farewell: the non-blocking send parks neither this
        // handler (which holds two lifecycle gates) nor the target's writer,
        // and a full queue must neither delay the removal nor reclassify the
        // close. The close frame (`4007 kicked`) remains the attribution
        // signal that always survives.
        let _ = self
            .message_coordinator
            .try_send_to_player(
                &target_id,
                Arc::new(ServerMessage::Error {
                    message: "You were removed from the room by its authority player.".to_string(),
                    error_code: Some(ErrorCode::Kicked),
                }),
            )
            .await;

        tracing::info!(
            authority = %authority_id,
            player_id = %target_id,
            room_id = %room_id,
            "Authority kicked a player from the room"
        );

        // Tombstone every reconnection path for the seat BEFORE the durable
        // removal, so a kicked seat can never resurrect through a pending or
        // in-flight reconnection claim (issue #525).
        self.discard_pre_issued_reconnection_token(&target_id).await;
        if pending_record_room == Some(room_id) {
            // Serialize the tombstone with an in-flight claim's restore
            // transaction through the room mutation gate: the claim's
            // restore path re-checks the tombstone under this gate, so the
            // mark lands either before the claim's decision (claim refused)
            // or after its completion (record consumed; the routing check
            // below sees the restored seat and the routed branch removes
            // it). Marking without this gate could lose the race against a
            // claim that restores and completes between the mark and the
            // routing check.
            let tombstone_event_guard = self
                .message_coordinator
                .lock_room_event_mutation(&room_id)
                .await;
            if let Some(manager) = &self.reconnection_manager {
                manager.mark_reconnection_kicked(&target_id).await;
            }
            drop(tombstone_event_guard);
        }

        if self.get_client_room(&target_id).await.is_some() {
            // Routed seat: the ordinary departure machinery owns the durable
            // removal, terminal watermark, sequenced replay-recorded
            // `PlayerLeft` broadcast, and departure re-planning.
            self.leave_room_locked(&target_id, false).await;
        } else if target_seated_in_row {
            // Disconnected seat still present in durable state (e.g. a
            // storage-failed detach): no live route and no terminal watermark
            // exist, so the honest `PlayerLeft` is suppressed and peers
            // reconcile the roster from their next baseline (the same
            // degradation class as the leave path's no-watermark branch).
            // The removal and the tombstone above are serialized against an
            // in-flight claim through the room mutation gate: a claim that
            // restores after this gate re-reads the room, finds no seat, and
            // observes the tombstone.
            let room_event_guard = self
                .message_coordinator
                .lock_room_event_mutation(&room_id)
                .await;
            match self
                .database
                .remove_player_from_room(&room_id, &target_id)
                .await
            {
                Ok(_) => {
                    self.pending_durable_player_detaches
                        .remove(&(room_id, target_id));
                }
                Err(error) => {
                    // Keep the seat in the durable-detach backlog like the
                    // leave path's storage-error branch; the tombstone still
                    // blocks every claim.
                    tracing::error!(
                        %target_id,
                        %room_id,
                        %error,
                        "Durable removal failed during kick; queueing durable-detach repair"
                    );
                    self.pending_durable_player_detaches
                        .insert((room_id, target_id), None);
                }
            }
            drop(room_event_guard);

            // Mirror the routed path's post-departure re-planning: a kicked
            // member may have hosted the room's active non-relay session.
            self.handle_session_member_departure(&room_id, &target_id)
                .await;
        }
        drop(_target_lifecycle_guard);

        self.connection_manager
            .request_close_for(&target_id, CloseReason::Kicked);

        self.metrics.increment_room_kicks();
        let _ = self
            .message_coordinator
            .send_to_player(
                authority_id,
                Arc::new(ServerMessage::RoomOperationResult {
                    operation_id,
                    result: Box::new(RoomOperationResult::PlayerKicked {
                        player_id: target_id,
                    }),
                }),
            )
            .await;
    }

    /// Handle an authority-initiated `RegenerateRoomCode` room operation
    /// (v3 only, issue #525).
    ///
    /// Only the room's designated authority may rotate the code. The fresh
    /// code replaces the stored code and the code registry entry as one
    /// storage action; a candidate that already resolves to another room is
    /// retried with the same bounded budget room creation uses. Each attempt
    /// serializes against same-code joiners through the shared
    /// `room_join:{game}:{code}` distributed lock, so a joiner that resolved
    /// the candidate before the swap joins this room instead of opening a
    /// duplicate.
    pub(super) async fn handle_regenerate_room_code_operation(
        self: &Arc<Self>,
        authority_id: &PlayerId,
        operation_id: RoomOperationId,
    ) {
        let Some(lifecycle) = self.connection_manager.client_lifecycle(authority_id) else {
            return;
        };
        let _authority_lifecycle_guard = lifecycle.lock().await;
        if lifecycle.player_id() != *authority_id
            || !self
                .connection_manager
                .lifecycle_matches(authority_id, &lifecycle)
        {
            return;
        }

        let Some(room_id) = self.get_client_room(authority_id).await else {
            self.fail_operation(
                authority_id,
                operation_id,
                "Not currently in a room",
                ErrorCode::NotInRoom,
            )
            .await;
            return;
        };
        let room = match self.database.get_room_by_id(&room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                self.fail_operation(
                    authority_id,
                    operation_id,
                    "Room no longer exists",
                    ErrorCode::RoomNotFound,
                )
                .await;
                return;
            }
            Err(error) => {
                tracing::error!(%authority_id, %room_id, %error, "Room-code rotation failed to load room");
                self.fail_operation(
                    authority_id,
                    operation_id,
                    "Failed to load room",
                    ErrorCode::StorageError,
                )
                .await;
                return;
            }
        };
        if room.authority_player != Some(*authority_id) {
            self.fail_operation(
                authority_id,
                operation_id,
                "Only the room's authority player may regenerate the room code",
                ErrorCode::NotRoomAuthority,
            )
            .await;
            return;
        }

        for _ in 0..REGENERATED_ROOM_CODE_MAX_ATTEMPTS {
            let candidate = self.generate_region_room_code();
            // Serialize against same-code joiners exactly like room creation:
            // a joiner holding this lock is mid-admission, and a joiner that
            // arrives after the swap resolves the new code to this room.
            let lock_key = format!("room_join:{}:{}", room.game_name, candidate);
            let lock_handle = match self
                .distributed_lock
                .acquire(&lock_key, ROOM_JOIN_LOCK_TTL)
                .await
            {
                Ok(handle) => handle,
                Err(error) => {
                    tracing::error!(%authority_id, %room_id, %error, "Failed to acquire candidate room-code lock");
                    self.fail_operation(
                        authority_id,
                        operation_id,
                        "Failed to regenerate the room code",
                        ErrorCode::StorageError,
                    )
                    .await;
                    return;
                }
            };
            match self.database.update_room_code(&room_id, candidate).await {
                Ok(room) => {
                    self.release_lock_accounted(&lock_handle).await;
                    self.metrics.increment_room_code_regenerations();
                    tracing::info!(
                        authority = %authority_id,
                        room_id = %room_id,
                        room_code = %room.code,
                        "Authority regenerated the room code"
                    );
                    let _ = self
                        .message_coordinator
                        .send_to_player(
                            authority_id,
                            Arc::new(ServerMessage::RoomOperationResult {
                                operation_id,
                                result: Box::new(RoomOperationResult::RoomCodeRegenerated {
                                    room_code: room.code,
                                }),
                            }),
                        )
                        .await;
                    return;
                }
                Err(UpdateRoomCodeError::RoomCodeCollision { .. }) => {
                    self.release_lock_accounted(&lock_handle).await;
                    self.metrics.increment_room_code_collisions();
                    continue;
                }
                Err(UpdateRoomCodeError::Storage(error)) => {
                    self.release_lock_accounted(&lock_handle).await;
                    tracing::error!(%authority_id, %room_id, %error, "Room-code rotation failed in storage");
                    self.fail_operation(
                        authority_id,
                        operation_id,
                        "Failed to regenerate the room code",
                        ErrorCode::StorageError,
                    )
                    .await;
                    return;
                }
            }
        }

        tracing::error!(
            %authority_id,
            %room_id,
            attempts = REGENERATED_ROOM_CODE_MAX_ATTEMPTS,
            "Regenerated room-code retry budget exhausted"
        );
        self.fail_operation(
            authority_id,
            operation_id,
            "Could not generate an unused room code",
            ErrorCode::InternalError,
        )
        .await;
    }

    /// Send the correlated terminal failure for a moderation operation.
    async fn fail_operation(
        &self,
        player_id: &PlayerId,
        operation_id: RoomOperationId,
        reason: impl Into<String>,
        error_code: ErrorCode,
    ) {
        let _ = self
            .message_coordinator
            .send_to_player(
                player_id,
                Arc::new(ServerMessage::room_operation_failed(
                    operation_id,
                    reason,
                    Some(error_code),
                )),
            )
            .await;
    }
}
