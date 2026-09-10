use std::sync::Arc;

use crate::coordination::CloseReason;
use crate::database::UpdateRoomCodeError;
use crate::protocol::{
    ErrorCode, PlayerId, Room, RoomId, RoomOperationId, RoomOperationResult,
    RoomPasswordCredential, ServerMessage,
};

use super::room_service::ROOM_JOIN_LOCK_TTL;
use super::EnhancedGameServer;

/// Maximum fresh-code candidates tried per `RegenerateRoomCode` request.
/// Matches the generated-room-code creation retry budget
/// (`GENERATED_ROOM_CODE_MAX_ATTEMPTS`).
const REGENERATED_ROOM_CODE_MAX_ATTEMPTS: u8 = 8;

/// Validated target context for a seat-targeting moderation operation
/// (`KickPlayer`, `BanPlayer`).
struct ModerationTarget {
    room_id: RoomId,
    /// Whether the target's durable row is currently seated (a
    /// disconnected-but-pending seat is still valid to evict).
    target_seated_in_row: bool,
    /// The room a pending reconnection record holds a seat for, if any.
    pending_record_room: Option<RoomId>,
    target_lifecycle_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    /// The acting authority's lifecycle gate, held across validation and
    /// every durable removal write so the authority's own
    /// disconnect/leave processing cannot interleave with the removal it
    /// authorized. Released before the best-effort close request and
    /// terminal result, both of which tolerate a vanished authority.
    _authority_lifecycle_guard: tokio::sync::OwnedMutexGuard<()>,
}

impl EnhancedGameServer {
    /// Handle an authority-initiated `KickPlayer` room operation (v3 only,
    /// issue #525).
    ///
    /// Only the room's designated authority may kick, the target must be a
    /// seated member — or this room's pending-record holder — other than
    /// the sender. The removal itself reuses the
    /// ordinary departure machinery (`leave_room_locked`), so the target's
    /// seat receives exactly the same durable removal, reconnection-token
    /// discard, unrouted terminal watermark, and sequenced replay-recorded
    /// `PlayerLeft` broadcast as any other departure — a kicked seat is never
    /// reconnectable. A target still routed in this room closes with the
    /// dedicated `4007 kicked` close code after a best-effort farewell
    /// `Error` frame; a target whose live route is in another room — a
    /// seated route or a spectator session — loses only the residue row or
    /// the tombstoned record, and its live connection stays open.
    pub(super) async fn handle_kick_player_operation(
        self: &Arc<Self>,
        authority_id: &PlayerId,
        operation_id: RoomOperationId,
        target_id: PlayerId,
    ) {
        let Some(ModerationTarget {
            room_id,
            target_seated_in_row,
            pending_record_room,
            target_lifecycle_guard,
            _authority_lifecycle_guard,
        }) = self
            .resolve_kick_style_target(authority_id, operation_id, &target_id)
            .await
        else {
            return;
        };

        self.evict_member_by_authority(
            authority_id,
            &target_id,
            room_id,
            target_seated_in_row,
            pending_record_room,
            target_lifecycle_guard,
            _authority_lifecycle_guard,
        )
        .await;

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

    /// Handle an authority-initiated `BanPlayer` room operation (v3 only,
    /// issue #525).
    ///
    /// The target is validated and evicted exactly as by `KickPlayer`, and —
    /// before the eviction — recorded on the room's in-memory ban list, so
    /// the player id cannot rejoin this room (seated or spectator) for the
    /// room's remaining lifetime. The ban write is serialized behind the
    /// room mutation gate ahead of the removal, so a concurrent admission
    /// cannot slip in between the ban decision and the eviction.
    pub(super) async fn handle_ban_player_operation(
        self: &Arc<Self>,
        authority_id: &PlayerId,
        operation_id: RoomOperationId,
        target_id: PlayerId,
    ) {
        let Some(ModerationTarget {
            room_id,
            target_seated_in_row,
            pending_record_room,
            target_lifecycle_guard,
            _authority_lifecycle_guard,
        }) = self
            .resolve_kick_style_target(authority_id, operation_id, &target_id)
            .await
        else {
            return;
        };

        // Record the ban before the eviction, under the room mutation gate:
        // the gate serializes the durable write with any admission already
        // resolved onto this room lane. A storage failure keeps the seat —
        // a ban that cannot be persisted must not silently evict without
        // its refusal semantics.
        let ban_event_guard = self
            .message_coordinator
            .lock_room_event_mutation(&room_id)
            .await;
        let ban_applied = self.database.set_room_ban(&room_id, &target_id, true).await;
        drop(ban_event_guard);
        if let Err(error) = ban_applied {
            tracing::error!(
                %authority_id,
                %target_id,
                %room_id,
                %error,
                "Ban failed to persist; refusing the eviction"
            );
            self.fail_operation(
                authority_id,
                operation_id,
                "Failed to ban the player",
                ErrorCode::StorageError,
            )
            .await;
            return;
        }

        tracing::info!(
            authority = %authority_id,
            player_id = %target_id,
            room_id = %room_id,
            "Authority banned a player from the room"
        );

        self.evict_member_by_authority(
            authority_id,
            &target_id,
            room_id,
            target_seated_in_row,
            pending_record_room,
            target_lifecycle_guard,
            _authority_lifecycle_guard,
        )
        .await;

        self.metrics.increment_room_bans();
        let _ = self
            .message_coordinator
            .send_to_player(
                authority_id,
                Arc::new(ServerMessage::RoomOperationResult {
                    operation_id,
                    result: Box::new(RoomOperationResult::PlayerBanned {
                        player_id: target_id,
                    }),
                }),
            )
            .await;
    }

    /// Handle an authority-initiated `UnbanPlayer` room operation (v3 only,
    /// issue #525).
    ///
    /// Lifts a room ban so the named player id may join again. Idempotent:
    /// lifting a ban that is not set succeeds.
    pub(super) async fn handle_unban_player_operation(
        self: &Arc<Self>,
        authority_id: &PlayerId,
        operation_id: RoomOperationId,
        target_id: PlayerId,
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
        // Serialize the ban lift behind the room mutation gate, holding it
        // across the authority re-check so a concurrent role change cannot
        // interleave between the check and the write.
        let unban_event_guard = self
            .message_coordinator
            .lock_room_event_mutation(&room_id)
            .await;
        if !self
            .require_room_authority(authority_id, operation_id, &room_id)
            .await
        {
            drop(unban_event_guard);
            return;
        }
        let unban_applied = self
            .database
            .set_room_ban(&room_id, &target_id, false)
            .await;
        drop(unban_event_guard);
        if let Err(error) = unban_applied {
            tracing::error!(
                %authority_id,
                %target_id,
                %room_id,
                %error,
                "Unban failed to persist"
            );
            self.fail_operation(
                authority_id,
                operation_id,
                "Failed to lift the ban",
                ErrorCode::StorageError,
            )
            .await;
            return;
        }

        tracing::info!(
            authority = %authority_id,
            player_id = %target_id,
            room_id = %room_id,
            "Authority lifted a room ban"
        );
        self.metrics.increment_room_unbans();
        let _ = self
            .message_coordinator
            .send_to_player(
                authority_id,
                Arc::new(ServerMessage::RoomOperationResult {
                    operation_id,
                    result: Box::new(RoomOperationResult::PlayerUnbanned {
                        player_id: target_id,
                    }),
                }),
            )
            .await;
    }

    /// Handle an authority-initiated `SetRoomAccess` room operation (v3
    /// only, issue #525).
    ///
    /// `Some(password)` seals the room behind a hashed join password; `None`
    /// reopens it. Current members and their reconnection tokens are
    /// unaffected; so is a pending reconnection record armed before the
    /// seal — restore is a resumption of a prior membership, not a fresh
    /// admission, so the password perimeter does not apply to it (a ban is
    /// the tool that refuses restores). The plaintext password is hashed
    /// once here and never logged, echoed, or persisted.
    pub(super) async fn handle_set_room_access_operation(
        self: &Arc<Self>,
        authority_id: &PlayerId,
        operation_id: RoomOperationId,
        password: Option<String>,
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
        if !Room::is_valid_room_password(password.as_deref()) {
            self.fail_operation(
                authority_id,
                operation_id,
                format!(
                    "The room password must be non-empty and at most {} bytes",
                    crate::protocol::MAX_ROOM_PASSWORD_LENGTH
                ),
                ErrorCode::InvalidInput,
            )
            .await;
            return;
        }

        let credential = password.as_deref().map(RoomPasswordCredential::new);
        // Serialize the policy flip with admissions already resolved onto
        // this room lane (the same gate the join and spectator paths hold
        // while they read the password), and hold the gate across the
        // authority re-check so a concurrent role change cannot interleave
        // between the check and the write.
        let access_event_guard = self
            .message_coordinator
            .lock_room_event_mutation(&room_id)
            .await;
        if !self
            .require_room_authority(authority_id, operation_id, &room_id)
            .await
        {
            drop(access_event_guard);
            return;
        }
        let applied = self.database.set_room_password(&room_id, credential).await;
        drop(access_event_guard);
        if let Err(error) = applied {
            tracing::error!(
                %authority_id,
                %room_id,
                %error,
                "Room access update failed to persist"
            );
            self.fail_operation(
                authority_id,
                operation_id,
                "Failed to update room access",
                ErrorCode::StorageError,
            )
            .await;
            return;
        }

        tracing::info!(
            authority = %authority_id,
            room_id = %room_id,
            requires_password = password.is_some(),
            "Authority updated the room's join password"
        );
        let _ = self
            .message_coordinator
            .send_to_player(
                authority_id,
                Arc::new(ServerMessage::RoomOperationResult {
                    operation_id,
                    result: Box::new(RoomOperationResult::RoomAccessUpdated {
                        requires_password: password.is_some(),
                    }),
                }),
            )
            .await;
    }

    /// Handle an authority-initiated `TransferAuthority` room operation (v3
    /// only, issue #525).
    ///
    /// The named seated member becomes the room's authority. Every member
    /// receives the usual replay-recorded `AuthorityChanged` broadcast
    /// (personalized `you_are_authority` per recipient); the sender receives
    /// `AuthorityTransferred` and loses every authority capability —
    /// `StartGame`, kick, ban, access, rotation, and further transfers.
    pub(super) async fn handle_transfer_authority_operation(
        self: &Arc<Self>,
        authority_id: &PlayerId,
        operation_id: RoomOperationId,
        target_id: PlayerId,
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
        if target_id == *authority_id {
            self.fail_operation(
                authority_id,
                operation_id,
                "The authority cannot transfer to itself",
                ErrorCode::InvalidInput,
            )
            .await;
            return;
        }

        // Atomic grant under the room mutation gate: a membership change
        // (the target leaving, a joiner arriving) can neither interleave
        // with the fresh validation nor with the role write, so the grant
        // can never land on a departed member.
        let transfer_event_guard = self
            .message_coordinator
            .lock_room_event_mutation(&room_id)
            .await;
        let room = match self.database.get_room_by_id(&room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                drop(transfer_event_guard);
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
                drop(transfer_event_guard);
                tracing::error!(%authority_id, %room_id, %error, "Authority transfer failed to load room");
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
            drop(transfer_event_guard);
            self.fail_operation(
                authority_id,
                operation_id,
                "Only the room's authority player may transfer the authority role",
                ErrorCode::NotRoomAuthority,
            )
            .await;
            return;
        }
        if !room.players.contains_key(&target_id) {
            drop(transfer_event_guard);
            self.metrics.increment_authority_transfer_conflicts();
            self.fail_operation(
                authority_id,
                operation_id,
                "Transfer target is not a member of this room",
                ErrorCode::TransferTargetNotFound,
            )
            .await;
            return;
        }
        // Fresh route read (issue #396 sweep, same class as the kick path's
        // target resolution): a durable row can outlive its membership as
        // residue from a storage-failed detach while the player is routed —
        // or seated — elsewhere. Granting the role onto such a row would
        // announce an authority that can never act for this room, wedge every
        // authority operation behind `NotRoomAuthority`, and later clear the
        // role silently when the residue is repaired — a stale view no event
        // would ever correct.
        if self.get_client_room(&target_id).await != Some(room_id) {
            drop(transfer_event_guard);
            self.metrics.increment_authority_transfer_conflicts();
            self.fail_operation(
                authority_id,
                operation_id,
                "Transfer target is not a member of this room",
                ErrorCode::TransferTargetNotFound,
            )
            .await;
            return;
        }
        let granted = match self
            .database
            .update_room_authority(&room_id, Some(target_id))
            .await
        {
            Ok(granted) => granted,
            Err(error) => {
                drop(transfer_event_guard);
                tracing::error!(%authority_id, %room_id, %error, "Authority transfer failed in storage");
                self.fail_operation(
                    authority_id,
                    operation_id,
                    "Failed to transfer the authority role",
                    ErrorCode::StorageError,
                )
                .await;
                return;
            }
        };
        if !granted {
            // Unreachable for an authority room (`supports_authority` holds
            // whenever `authority_player` is set); a source-compatible
            // storage adapter reporting otherwise is an infrastructure
            // fault, not a client refusal.
            drop(transfer_event_guard);
            tracing::error!(
                %authority_id,
                %room_id,
                "Authority transfer reported an unsupported authority room"
            );
            self.fail_operation(
                authority_id,
                operation_id,
                "Failed to transfer the authority role",
                ErrorCode::StorageError,
            )
            .await;
            return;
        }

        // Sequenced, replay-recorded announcement (same shape as the claim
        // and departure paths): the room mutation guard moves into the FIFO
        // job, so the `AuthorityChanged` can never be overtaken by a
        // membership change that commits after the role write. Delivering it
        // outside this gate let a departure of the freshly granted authority
        // announce the cleared role first, leaving live members with a stale
        // authority view that no event would ever correct (issue #396).
        let notification = Arc::new(ServerMessage::AuthorityChanged {
            authority_player: Some(target_id),
            you_are_authority: false,
        });
        let replay_notification = Arc::clone(&notification);
        let reconnection_manager = self.reconnection_manager.clone();
        let job_coordinator = Arc::clone(&self.message_coordinator);
        let job_room_id = room_id;
        let job_authority_id = *authority_id;
        let operation_result = Arc::new(ServerMessage::RoomOperationResult {
            operation_id,
            result: Box::new(RoomOperationResult::AuthorityTransferred {
                player_id: target_id,
            }),
        });
        let completion = self.message_coordinator.enqueue_room_event(
            transfer_event_guard,
            Box::new(move || {
                Box::pin(async move {
                    let announced = job_coordinator
                        .broadcast_to_room_with_hook(
                            &job_room_id,
                            notification,
                            Box::new(move || {
                                Box::pin(async move {
                                    if let Some(reconnection_manager) = reconnection_manager {
                                        reconnection_manager
                                            .record_room_event(
                                                &job_room_id,
                                                replay_notification.as_ref(),
                                            )
                                            .await;
                                    }
                                })
                            }),
                        )
                        .await;
                    match announced {
                        Ok(true) => {
                            tracing::info!(
                                authority = %job_authority_id,
                                player_id = %target_id,
                                room_id = %job_room_id,
                                "Authority transferred the authority role"
                            );
                        }
                        Ok(false) => {
                            tracing::info!(
                                authority = %job_authority_id,
                                player_id = %target_id,
                                room_id = %job_room_id,
                                "Authority transferred the authority role; no live recipient was available for the announcement"
                            );
                        }
                        Err(_) => {
                            tracing::info!(
                                authority = %job_authority_id,
                                player_id = %target_id,
                                room_id = %job_room_id,
                                "Authority transferred the authority role; the announcement failed and is reported by the completion"
                            );
                        }
                    }
                    // The per-recipient projection personalizes
                    // `you_are_authority`, so the new authority learns its
                    // role from this event and the former authority learns
                    // it lost.
                    let _ = job_coordinator
                        .send_to_player(&job_authority_id, operation_result)
                        .await;
                    announced
                })
            }),
        );
        if let Err(error) = completion.await {
            tracing::error!(
                authority = %authority_id,
                %room_id,
                %error,
                "Failed to announce the transferred authority role"
            );
        }

        self.metrics.increment_authority_transfers();
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
    /// duplicate. The old code's lock is held across the loop for the same
    /// reason on the outgoing side: a joiner already admitted on the old code
    /// resolves this room, and only post-swap joiners observe the drop.
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

        // Serialize against old-code joiners too (issue #396 sweep): a joiner
        // holding `room_join:{game}:{old}` is mid-admission and must resolve
        // this room before the swap, never observe the dropped code as free
        // and resurrect it as a duplicate. The hold spans the candidate loop;
        // the candidate acquisition below nests under it (lock order
        // old → candidate), and a join takes exactly one code lock plus the
        // cap locks — which rotation never touches — so the order cannot
        // cycle. Released on every exit from the loop.
        let old_lock_key = format!("room_join:{}:{}", room.game_name, room.code);
        let mut old_lock_renewal = match self
            .distributed_lock
            .acquire(&old_lock_key, ROOM_JOIN_LOCK_TTL)
            .await
        {
            Ok(handle) => self.keep_lock_renewed(handle, ROOM_JOIN_LOCK_TTL),
            Err(error) => {
                tracing::error!(%authority_id, %room_id, %error, "Failed to acquire old room-code lock");
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
                    self.release_renewed_lock(&mut old_lock_renewal).await;
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
            // The hold spans the storage swap; keep the lease renewed so the
            // mutual exclusion cannot silently expire mid-hold (issue #550).
            let mut lock_renewal = self.keep_lock_renewed(lock_handle, ROOM_JOIN_LOCK_TTL);
            match self.database.update_room_code(&room_id, candidate).await {
                Ok(room) => {
                    self.release_renewed_lock(&mut lock_renewal).await;
                    self.release_renewed_lock(&mut old_lock_renewal).await;
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
                    self.release_renewed_lock(&mut lock_renewal).await;
                    self.metrics.increment_room_code_collisions();
                    continue;
                }
                Err(UpdateRoomCodeError::Storage(error)) => {
                    self.release_renewed_lock(&mut lock_renewal).await;
                    self.release_renewed_lock(&mut old_lock_renewal).await;
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
        self.release_renewed_lock(&mut old_lock_renewal).await;
        self.fail_operation(
            authority_id,
            operation_id,
            "Could not generate an unused room code",
            ErrorCode::InternalError,
        )
        .await;
    }

    /// Shared prologue for the authority-only seat-targeting moderation
    /// operations (`KickPlayer`, `BanPlayer`): fixes the authority's
    /// identity under its lifecycle gate, resolves the room, re-checks
    /// authority on fresh storage state, and validates the target under the
    /// target's lifecycle gate. Every refusal is already emitted; `None`
    /// means the caller is done.
    async fn resolve_kick_style_target(
        self: &Arc<Self>,
        authority_id: &PlayerId,
        operation_id: RoomOperationId,
        target_id: &PlayerId,
    ) -> Option<ModerationTarget> {
        // Fix the authority's connection identity and membership with its
        // lifecycle gate (same prologue as every room operation handler).
        // The guard is carried out through [`ModerationTarget`] so the
        // eviction it authorizes cannot interleave with the authority's own
        // disconnect/leave processing.
        let lifecycle = self.connection_manager.client_lifecycle(authority_id)?;
        let authority_lifecycle_guard = Arc::clone(&lifecycle).lock_owned().await;
        if lifecycle.player_id() != *authority_id
            || !self
                .connection_manager
                .lifecycle_matches(authority_id, &lifecycle)
        {
            return None;
        }

        let Some(room_id) = self.get_client_room(authority_id).await else {
            self.fail_operation(
                authority_id,
                operation_id,
                "Not currently in a room",
                ErrorCode::NotInRoom,
            )
            .await;
            return None;
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
                return None;
            }
            Err(error) => {
                tracing::error!(%authority_id, %room_id, %error, "Moderation failed to load room");
                self.fail_operation(
                    authority_id,
                    operation_id,
                    "Failed to load room",
                    ErrorCode::StorageError,
                )
                .await;
                return None;
            }
        };
        if room.authority_player != Some(*authority_id) {
            self.fail_operation(
                authority_id,
                operation_id,
                "Only the room's authority player may moderate this room",
                ErrorCode::NotRoomAuthority,
            )
            .await;
            return None;
        }
        if target_id == authority_id {
            self.fail_operation(
                authority_id,
                operation_id,
                "The authority cannot target itself; use LeaveRoom instead",
                ErrorCode::InvalidInput,
            )
            .await;
            return None;
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
        let target_lifecycle = self.connection_manager.client_lifecycle(target_id);
        let target_lifecycle_guard = match target_lifecycle {
            Some(target) => {
                let guard = Arc::clone(&target).lock_owned().await;
                if target.player_id() != *target_id {
                    tracing::debug!(
                        %target_id,
                        %room_id,
                        "Moderation raced a target identity swap; proceeding with removal"
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
                return None;
            }
            Err(error) => {
                tracing::error!(%authority_id, %room_id, %error, "Moderation failed to re-load room");
                self.fail_operation(
                    authority_id,
                    operation_id,
                    "Failed to load room",
                    ErrorCode::StorageError,
                )
                .await;
                return None;
            }
        };
        if room.authority_player != Some(*authority_id) {
            self.fail_operation(
                authority_id,
                operation_id,
                "Only the room's authority player may moderate this room",
                ErrorCode::NotRoomAuthority,
            )
            .await;
            return None;
        }
        // A disconnect with reconnection enabled removes the durable member
        // and arms a pending record; from the room's perspective that seat is
        // still held until the window closes. Evicting such a seat tombstones
        // the record instead of removing an already-absent row.
        let pending_record_room = match &self.reconnection_manager {
            Some(manager) => manager.pending_reconnection_room(target_id).await,
            None => None,
        };
        let target_seated_in_row = room.players.contains_key(target_id);
        if !target_seated_in_row && pending_record_room != Some(room_id) {
            self.fail_operation(
                authority_id,
                operation_id,
                "Moderation target is not a member of this room",
                ErrorCode::KickTargetNotFound,
            )
            .await;
            return None;
        }

        Some(ModerationTarget {
            room_id,
            target_seated_in_row,
            pending_record_room,
            target_lifecycle_guard,
            _authority_lifecycle_guard: authority_lifecycle_guard,
        })
    }

    /// Shared eviction core for `KickPlayer` and `BanPlayer` (issue #525).
    ///
    /// Callers have validated authority and target membership via
    /// [`Self::resolve_kick_style_target`] and still hold the returned
    /// target lifecycle guard. Every reconnection path is tombstoned before
    /// the durable removal. A target still routed in this room receives a
    /// best-effort farewell and the `4007 kicked` close, and the seat is
    /// removed through the ordinary departure machinery. A target whose live
    /// route is in another room — a seated route or a spectator session —
    /// means this room's row is stale residue or a tombstoned record: its
    /// live membership, the reconnection credential for its actual room, its
    /// farewell, and its connection are untouched (issue #396).
    async fn evict_member_by_authority(
        self: &Arc<Self>,
        authority_id: &PlayerId,
        target_id: &PlayerId,
        room_id: RoomId,
        target_seated_in_row: bool,
        pending_record_room: Option<RoomId>,
        _target_lifecycle_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
        _authority_lifecycle_guard: tokio::sync::OwnedMutexGuard<()>,
    ) {
        // The target's live route decides what this eviction removes: a
        // route in THIS room is the seat being evicted, while a route in any
        // other room means this room's row is stale residue (a
        // storage-failed detach that outlived its departure). Removing the
        // residue must not disturb the target's live membership elsewhere —
        // its farewell, credential, and close all belong to the room it is
        // actually in (issue #396).

        // Tombstone every reconnection path for the seat BEFORE the durable
        // removal, so a kicked seat can never resurrect through a pending or
        // in-flight reconnection claim (issue #525).
        if pending_record_room == Some(room_id) {
            // Serialize the tombstone with an in-flight claim's restore
            // transaction through the room mutation gate: the claim's
            // restore path re-checks the tombstone under this gate, so the
            // mark lands either before the claim's decision (claim refused)
            // or after its completion (record consumed; the fresh routing
            // check below sees the restored seat and the routed branch
            // removes it). Marking without this gate could lose the race
            // against a claim that restores and completes between the mark
            // and the routing check.
            let tombstone_event_guard = self
                .message_coordinator
                .lock_room_event_mutation(&room_id)
                .await;
            if let Some(manager) = &self.reconnection_manager {
                manager.mark_reconnection_kicked(target_id).await;
            }
            drop(tombstone_event_guard);
        }

        // Fresh route read after the tombstone's gate section: a reconnection
        // claim that completed against the tombstone re-keyed its live
        // connection to the restored seat, and the branch below must observe
        // that route rather than a pre-gate snapshot (issue #396). This
        // stays correct only while the claim's restore AND its connection
        // re-key happen inside one room-mutation-gate hold
        // (`ReconnectionManager`'s restore transaction spans the gate
        // acquire through the reassignment, `src/server/reconnection_service.rs`);
        // moving the re-key outside that hold would let a kick lose to a
        // mid-flight claim.
        // A live spectator session in another room is a live membership
        // elsewhere too (issue #396): the seated-route read cannot see it,
        // so it is queried separately.
        let routed_room = self.get_client_room(target_id).await;
        let spectator_room = self.spectator_service.spectator_room(target_id);
        let evicts_live_membership_elsewhere = matches!(routed_room, Some(routed) if routed != room_id)
            || matches!(spectator_room, Some(live) if live != room_id);

        // The pre-issued token is keyed by player id alone and binds to the
        // room the target currently occupies, so removing it here would
        // destroy the credential of a live membership elsewhere. Only the
        // eviction of a seat in THIS room discards it.
        if !evicts_live_membership_elsewhere {
            self.discard_pre_issued_reconnection_token(target_id).await;
        }

        // Best-effort farewell: the non-blocking send parks neither this
        // handler (which holds both lifecycle gates) nor the target's writer,
        // and a full queue must neither delay the removal nor reclassify the
        // close. The close frame (`4007 kicked`) remains the attribution
        // signal that always survives. A target whose live membership is in
        // another room — a seated route or a spectator session — is not
        // being removed from anything it can observe, so it receives neither
        // the farewell nor the close (issue #396).
        if !evicts_live_membership_elsewhere {
            let _ = self
                .message_coordinator
                .try_send_to_player(
                    target_id,
                    Arc::new(ServerMessage::Error {
                        message: "You were removed from the room by its authority player."
                            .to_string(),
                        error_code: Some(ErrorCode::Kicked),
                    }),
                )
                .await;
        }

        tracing::info!(
            authority = %authority_id,
            player_id = %target_id,
            room_id = %room_id,
            "Authority removed a player from the room"
        );

        if routed_room == Some(room_id) {
            // Routed seat: the ordinary departure machinery owns the durable
            // removal, terminal watermark, sequenced replay-recorded
            // `PlayerLeft` broadcast, and departure re-planning.
            self.leave_room_locked(target_id, false).await;
        } else if target_seated_in_row {
            // Stale durable row: the seat is present in storage without a
            // route in this room (a storage-failed detach, or the target
            // already joined another room before the repair ran). Remove the
            // residue without touching the target's live membership
            // elsewhere: no live route and no terminal watermark exist here,
            // so the honest `PlayerLeft` is suppressed and peers reconcile
            // the roster from their next baseline (the same degradation
            // class as the leave path's no-watermark branch).
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
                .remove_player_from_room(&room_id, target_id)
                .await
            {
                Ok(_) => {
                    self.pending_durable_player_detaches
                        .remove(&(room_id, *target_id));
                }
                Err(error) => {
                    // Keep the seat in the durable-detach backlog like the
                    // leave path's storage-error branch; the tombstone still
                    // blocks every claim.
                    tracing::error!(
                        %target_id,
                        %room_id,
                        %error,
                        "Durable removal failed during authority eviction; queueing durable-detach repair"
                    );
                    self.pending_durable_player_detaches
                        .insert((room_id, *target_id), None);
                }
            }
            drop(room_event_guard);

            // Mirror the routed path's post-departure re-planning: a removed
            // member may have hosted the room's active non-relay session.
            self.handle_session_member_departure(&room_id, target_id)
                .await;
        }
        drop(_target_lifecycle_guard);
        drop(_authority_lifecycle_guard);

        if !evicts_live_membership_elsewhere {
            self.connection_manager
                .request_close_for(target_id, CloseReason::Kicked);
        }
    }

    /// Refuse unless the room's current authority is `authority_id`.
    /// Used by the moderation operations that do not target a seat
    /// (`SetRoomAccess`, `UnbanPlayer`); callers hold the room mutation gate
    /// across this check and their subsequent write, so the role can neither
    /// change nor lose its holder between the two.
    async fn require_room_authority(
        self: &Arc<Self>,
        authority_id: &PlayerId,
        operation_id: RoomOperationId,
        room_id: &RoomId,
    ) -> bool {
        let room = match self.database.get_room_by_id(room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                self.fail_operation(
                    authority_id,
                    operation_id,
                    "Room no longer exists",
                    ErrorCode::RoomNotFound,
                )
                .await;
                return false;
            }
            Err(error) => {
                tracing::error!(%authority_id, %room_id, %error, "Moderation failed to load room");
                self.fail_operation(
                    authority_id,
                    operation_id,
                    "Failed to load room",
                    ErrorCode::StorageError,
                )
                .await;
                return false;
            }
        };
        if room.authority_player != Some(*authority_id) {
            self.fail_operation(
                authority_id,
                operation_id,
                "Only the room's authority player may moderate this room",
                ErrorCode::NotRoomAuthority,
            )
            .await;
            return false;
        }
        true
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
