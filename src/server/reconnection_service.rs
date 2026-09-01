use crate::protocol::{
    ErrorCode, PlayerId, PlayerInfo, ReconnectedPayload, ReplayStatus, RoomId, SenderWatermark,
    ServerMessage,
};
use crate::reconnection::{ClaimedReconnection, DisconnectedPlayer, ReconnectionManager};
use futures_util::FutureExt;
use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use crate::coordination::{
    RoomEventMutationGuard, RoomMessageTransactionOutcome, RoomRecipientMessages,
};

use super::session_policy::{membership_session_decision, ActiveSessionPlan};
use super::{EnhancedGameServer, PendingApplicationClaimRollback};

struct ReconnectionClaimGuard {
    manager: Arc<ReconnectionManager>,
    claim: Option<ClaimedReconnection>,
}

/// Durable state a reconnect attempt has already changed but has not yet
/// committed. Rejection unwinds exactly this set, so every failure path is
/// driven from one value instead of a widening argument list that a new
/// early-return can silently under-populate.
#[derive(Clone, Default)]
struct ReconnectRestoreState {
    /// This attempt re-added the membership row that the disconnect removed.
    restored_membership: bool,
    /// This attempt was granted the room's vacant authority back.
    restored_authority: bool,
    /// This attempt took ownership of a queued durable detach: `Some(value)`
    /// is the retry it removed, carrying whatever application-claim rollback
    /// that retry still owed. Taking it over is required — maintenance must
    /// not delete a membership this reconnect is about to make live again —
    /// but a rejection has to hand it back, or the phantom row it describes
    /// keeps a seat in every capacity check with nothing left to repair it
    /// until the reconnection window expires.
    cleared_pending_detach: Option<Option<PendingApplicationClaimRollback>>,
}

#[derive(Clone, Default)]
struct ReconnectPanicRecovery {
    claim: Option<ClaimedReconnection>,
    restore: ReconnectRestoreState,
    reassigned: bool,
    room_event_guard: Option<RoomEventMutationGuard>,
}

impl ReconnectionClaimGuard {
    fn new(manager: Arc<ReconnectionManager>, claim: ClaimedReconnection) -> Self {
        Self {
            manager,
            claim: Some(claim),
        }
    }

    fn disconnected(&self) -> Option<DisconnectedPlayer> {
        self.claim.as_ref().map(|claim| claim.disconnected.clone())
    }

    async fn release(mut self) -> bool {
        let Some(claim) = self.claim.take() else {
            return false;
        };
        self.manager.release_reconnection_claim(&claim).await
    }

    async fn complete(mut self) -> bool {
        let Some(claim) = self.claim.take() else {
            return false;
        };
        self.manager.complete_claimed_reconnection(&claim).await
    }
}

impl Drop for ReconnectionClaimGuard {
    fn drop(&mut self) {
        if let Some(claim) = &self.claim {
            // The enclosing owned task retains a cloned claim and performs
            // phase-aware rollback or completion after catching an unwind.
            // Releasing asynchronously here would race that supervisor and can
            // make an already-delivered one-time token reusable.
            tracing::error!(
                player_id = %claim.disconnected.player_id,
                room_id = %claim.disconnected.room_id,
                "Reconnection claim guard dropped without explicit completion; supervisor recovery required"
            );
        }
    }
}

impl EnhancedGameServer {
    /// Repair a delivered reconnect while the unwind supervisor retains the
    /// connection lifecycle guard.
    async fn repair_panicked_reconnect_publication_locked(
        self: &Arc<Self>,
        reconnect_player_id: PlayerId,
        room_id: RoomId,
        disconnected: &DisconnectedPlayer,
        _room_event_guard: RoomEventMutationGuard,
    ) {
        let Some(stamp) = self
            .connection_manager
            .current_relay_stamp_in_room(&reconnect_player_id, &room_id)
        else {
            return;
        };
        let room = match self.database.get_room_by_id(&room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => return,
            Err(error) => {
                tracing::error!(%reconnect_player_id, %room_id, %error, "Failed to load room for panicked reconnect repair");
                return;
            }
        };
        let player = room
            .players
            .get(&reconnect_player_id)
            .cloned()
            .or_else(|| disconnected.player_info.clone());
        let notification = Arc::new(ServerMessage::PlayerReconnected {
            player_id: reconnect_player_id,
            epoch: Some(stamp.epoch),
        });
        if let Some(player) = player {
            self.terminate_room_generation_after_publication_failure(
                reconnect_player_id,
                room_id,
                stamp.epoch,
                player,
                room.authority_player == Some(reconnect_player_id) || disconnected.was_authority,
                "reconnect_panic",
            )
            .await;
        }
        if let Err(error) = self
            .publish_reconnect_lifecycle_fallback(
                room_id,
                reconnect_player_id,
                notification,
                room.authority_player == Some(reconnect_player_id),
            )
            .await
        {
            tracing::error!(%reconnect_player_id, %room_id, %error, "Failed to repair reconnect lifecycle after panic");
        }
    }

    async fn commit_reconnect_publication_state(
        &self,
        room_id: RoomId,
        replay_notification: Arc<ServerMessage>,
        active_plan_update: Option<Option<ActiveSessionPlan>>,
        is_replan: bool,
        count_reconnector_plan: bool,
        turn_credentials_issued: u64,
    ) {
        if let Some(update) = active_plan_update {
            if let Some(plan) = update {
                self.active_session_plans.insert(room_id, plan);
            } else {
                self.active_session_plans.remove(&room_id);
            }
        }
        if is_replan {
            self.metrics.increment_session_replans_emitted();
        }
        if count_reconnector_plan {
            self.metrics.increment_session_plans_late_join();
        }
        self.metrics
            .add_turn_credentials_issued(turn_credentials_issued);
        if let Some(reconnection_manager) = &self.reconnection_manager {
            reconnection_manager
                .record_room_event(&room_id, replay_notification.as_ref())
                .await;
        }
    }

    async fn publish_reconnect_lifecycle_fallback(
        &self,
        room_id: RoomId,
        reconnect_player_id: PlayerId,
        notification: Arc<ServerMessage>,
        restored_authority: bool,
    ) -> anyhow::Result<bool> {
        let lifecycle_committed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let lifecycle_committed_in_hook = Arc::clone(&lifecycle_committed);
        let replay_notification = Arc::clone(&notification);
        let (_drain_owner, drain) = tokio::sync::watch::channel(false);
        let should_send = || true;
        let _ = self
            .message_coordinator
            .broadcast_to_room_except_if_with_hook(
                &room_id,
                &reconnect_player_id,
                notification,
                &should_send,
                drain,
                Box::new(move || {
                    Box::pin(async move {
                        self.commit_reconnect_publication_state(
                            room_id,
                            replay_notification,
                            None,
                            false,
                            false,
                            0,
                        )
                        .await;
                        lifecycle_committed_in_hook
                            .store(true, std::sync::atomic::Ordering::Release);
                    })
                }),
            )
            .await?;
        if restored_authority {
            self.publish_reconnect_authority_change(room_id, reconnect_player_id)
                .await?;
        }
        Ok(lifecycle_committed.load(std::sync::atomic::Ordering::Acquire))
    }

    async fn publish_reconnect_authority_change(
        &self,
        room_id: RoomId,
        authority_player: PlayerId,
    ) -> anyhow::Result<bool> {
        let notification = Arc::new(ServerMessage::AuthorityChanged {
            authority_player: Some(authority_player),
            you_are_authority: false,
        });
        let replay_notification = Arc::clone(&notification);
        let reconnection_manager = self.reconnection_manager.clone();
        self.message_coordinator
            .broadcast_to_room_with_hook(
                &room_id,
                notification,
                Box::new(move || {
                    Box::pin(async move {
                        if let Some(reconnection_manager) = reconnection_manager {
                            reconnection_manager
                                .record_room_event(&room_id, replay_notification.as_ref())
                                .await;
                        }
                    })
                }),
            )
            .await
    }

    /// Mint (or rotate) the reconnection token surfaced on `RoomJoined` /
    /// `Reconnected` for a v3+ recipient joining `room_id` (issue #136, F4:
    /// a token minted only at disconnect time can never legitimately reach
    /// the client it is for). Returns `None` — keeping the field off the
    /// wire — when reconnection is disabled or the recipient negotiated v2:
    /// a v2 client could not receive the token anyway, so its disconnect
    /// keeps the old mint-at-disconnect fallback unchanged.
    pub(crate) async fn pre_issue_reconnection_token_for(
        &self,
        player_id: &PlayerId,
        room_id: RoomId,
    ) -> Option<String> {
        let reconnection_manager = self.reconnection_manager.as_ref()?;
        if self.client_protocol(player_id).version < 3 {
            return None;
        }
        Some(
            reconnection_manager
                .pre_issue_token_with_identity(
                    *player_id,
                    room_id,
                    self.client_reconnection_identity(player_id),
                )
                .await,
        )
    }

    /// Drop a player's pre-issued reconnection token (voluntary leave or a
    /// roomless teardown — neither may leave a claimable token behind, and
    /// the pre-issued map must stay bounded by currently-joined players).
    pub(crate) async fn discard_pre_issued_reconnection_token(&self, player_id: &PlayerId) {
        if let Some(reconnection_manager) = &self.reconnection_manager {
            reconnection_manager.discard_pre_issued(player_id).await;
        }
    }

    pub(crate) async fn discard_pending_reconnection_for_shutdown_drain(
        &self,
        player_id: &PlayerId,
    ) {
        if let Some(reconnection_manager) = &self.reconnection_manager {
            if reconnection_manager
                .discard_pending_reconnection(player_id)
                .await
            {
                tracing::debug!(
                    %player_id,
                    "Discarded pending reconnection record after shutdown drain won disconnect race"
                );
            }
        }
    }

    pub(crate) async fn register_disconnection_for_reconnect(
        &self,
        player_id: &PlayerId,
        room_id: RoomId,
        was_authority: bool,
        player_info: PlayerInfo,
    ) -> bool {
        let Some(reconnection_manager) = &self.reconnection_manager else {
            return false;
        };

        // Capture the connection's current game-data incarnation epoch WHILE it
        // is still registered (unregister removes it right after this call), so
        // the reconnect can resume at `last_epoch + 1` and keep the recipient's
        // (epoch, seq) view strictly increasing (v3 reliability surface).
        // The room-bound read returns `None` if the connection vanished or its
        // membership changed during a disconnect race. Never carry an epoch
        // from a replacement room into this reconnect record. This is
        // independent of protocol version; a missing stamp falls back to `0`
        // (equivalent to "never stamped", so reconnect resumes at epoch 1).
        let last_epoch = self
            .connection_manager
            .current_relay_stamp_in_room(player_id, &room_id)
            .map(|stamp| stamp.epoch)
            .unwrap_or(0);

        let token = reconnection_manager
            .register_disconnection_with_identity(
                *player_id,
                room_id,
                was_authority,
                Some(player_info),
                last_epoch,
                self.client_reconnection_identity(player_id),
            )
            .await;

        // Room GC pins replay state while it makes its storage decision. If
        // registration wins that ordering, the new record protects the room.
        // If inactive-room cleanup wins, it may delete an occupied stale room
        // before this write lock becomes available. Re-check storage after the
        // record is visible: a successful read now cannot race a later GC
        // deletion because that GC must observe this record, while a missing
        // room means the advertised credential can no longer be fulfilled and
        // must not survive as a pending reconnect.
        match self.database.get_room_by_id(&room_id).await {
            Ok(Some(_)) => {}
            Ok(None) => {
                let discarded = reconnection_manager
                    .discard_pending_reconnection(player_id)
                    .await;
                if discarded {
                    tracing::warn!(
                        %player_id,
                        %room_id,
                        "Discarded reconnection registration after room cleanup won the race"
                    );
                } else {
                    // A claimed record must not be discarded (its in-flight
                    // transaction owns it); the reconnect's own room
                    // re-verification reports the deletion to the client.
                    tracing::warn!(
                        %player_id,
                        %room_id,
                        "Retained claimed reconnection registration after room cleanup won the race"
                    );
                }
                return false;
            }
            Err(error) => {
                // A transient read failure is not proof that the room is gone.
                // Retain the record so maintenance cannot delete a live room
                // merely because this verification was unavailable.
                tracing::warn!(
                    %player_id,
                    %room_id,
                    %error,
                    "Failed to verify room after reconnection registration; retaining protection"
                );
            }
        }

        tracing::info!(
            %player_id,
            %room_id,
            %was_authority,
            reconnection_token = %token.get(..8).unwrap_or("<invalid>"),
            "Player disconnection registered for reconnection"
        );
        true
    }

    async fn reject_claimed_reconnect(
        &self,
        current_player_id: &PlayerId,
        claim_guard: ReconnectionClaimGuard,
        restore: &ReconnectRestoreState,
        reason: &str,
        error_code: ErrorCode,
        operation_id: Option<crate::protocol::RoomOperationId>,
    ) -> bool {
        self.rollback_claimed_reconnect(claim_guard, restore, reason)
            .await;

        let _ = self
            .message_coordinator
            .send_to_player(
                current_player_id,
                Arc::new(
                    (ServerMessage::ReconnectionFailed {
                        reason: reason.to_string(),
                        error_code,
                    })
                    .correlate_room_operation(operation_id),
                ),
            )
            .await;
        false
    }

    async fn rollback_claimed_reconnect(
        &self,
        claim_guard: ReconnectionClaimGuard,
        restore: &ReconnectRestoreState,
        reason: &str,
    ) {
        let Some(disconnected) = claim_guard.disconnected() else {
            tracing::warn!(%reason, "Reconnection rejection had no active claim to release");
            return;
        };
        // A detach this attempt took over is owed back to maintenance unless
        // the rejection itself made the durable state clean. `None` here means
        // "nothing left to repair"; `Some(rollback)` re-queues the retry with
        // the provenance it started with.
        let mut requeue_detach = restore.cleared_pending_detach.clone();
        if restore.restored_membership {
            if let Err(err) = self
                .database
                .remove_player_from_room(&disconnected.room_id, &disconnected.player_id)
                .await
            {
                // The row survives, so a detach retry is owed whether or not
                // this attempt inherited one.
                requeue_detach = Some(requeue_detach.flatten());
                tracing::warn!(
                    player_id = %disconnected.player_id,
                    room_id = %disconnected.room_id,
                    error = %err,
                    "Failed to roll back restored room membership after reconnect failure"
                );
            } else {
                // The rollback removed the row, so only an outstanding
                // application-claim rollback still needs a retry.
                requeue_detach = requeue_detach.filter(Option::is_some);
            }
        } else if restore.restored_authority {
            if let Err(err) = self
                .database
                .update_room_authority(&disconnected.room_id, None)
                .await
            {
                tracing::warn!(
                    player_id = %disconnected.player_id,
                    room_id = %disconnected.room_id,
                    error = %err,
                    "Failed to roll back restored authority after reconnect failure"
                );
            }
        }
        if let Some(rollback) = requeue_detach {
            self.pending_durable_player_detaches
                .insert((disconnected.room_id, disconnected.player_id), rollback);
        }

        let released = claim_guard.release().await;
        tracing::warn!(
            player_id = %disconnected.player_id,
            room_id = %disconnected.room_id,
            %released,
            %reason,
            "Reconnection claim released after restore failure"
        );
    }

    async fn reject_after_reassigned_reconnect_failure(
        &self,
        current_player_id: &PlayerId,
        reconnect_player_id: &PlayerId,
        claim_guard: ReconnectionClaimGuard,
        restore: &ReconnectRestoreState,
        rollback_context: &'static str,
        operation_id: Option<crate::protocol::RoomOperationId>,
    ) -> bool {
        self.discard_pre_issued_reconnection_token(reconnect_player_id)
            .await;
        let _ = self
            .message_coordinator
            .unregister_local_client(reconnect_player_id)
            .await;
        if self
            .connection_manager
            .restore_reassigned_connection(current_player_id, reconnect_player_id)
            .is_none()
        {
            tracing::warn!(
                %current_player_id,
                %reconnect_player_id,
                %rollback_context,
                "Failed to restore temporary connection identity after reconnect failure"
            );
        }
        self.reject_claimed_reconnect(
            current_player_id,
            claim_guard,
            restore,
            "Reconnected baseline could not be delivered",
            ErrorCode::ReconnectionFailed,
            operation_id,
        )
        .await
    }

    /// Handle player reconnection.
    ///
    /// Identity contract: this entry performs the routing-map identity swap
    /// without touching any socket-side identity (no
    /// `effective_player_id` handle is supplied). It must only be driven by a
    /// caller that owns the reconnecting socket's identity — the connection
    /// task uses `handle_reconnect_with_identity`. The message router
    /// refuses `Reconnect` frames with a coded error for exactly this reason;
    /// see the `ClientMessage::Reconnect` arm in `message_router.rs`.
    pub async fn handle_reconnect(
        self: &Arc<Self>,
        current_player_id: &PlayerId,
        reconnect_player_id: &PlayerId,
        room_id: &RoomId,
        auth_token: &str,
    ) -> bool {
        self.handle_reconnect_operation(
            current_player_id,
            reconnect_player_id,
            room_id,
            auth_token,
            None,
        )
        .await
    }

    pub(super) async fn handle_reconnect_operation(
        self: &Arc<Self>,
        current_player_id: &PlayerId,
        reconnect_player_id: &PlayerId,
        room_id: &RoomId,
        auth_token: &str,
        operation_id: Option<crate::protocol::RoomOperationId>,
    ) -> bool {
        // See the identity contract on [`Self::handle_reconnect`]: no
        // socket-side identity is updated here.
        self.spawn_reconnect_transaction(
            *current_player_id,
            *reconnect_player_id,
            *room_id,
            auth_token.to_string(),
            None,
            operation_id,
        )
        .await
    }

    pub(crate) async fn handle_reconnect_with_identity(
        self: &Arc<Self>,
        current_player_id: &PlayerId,
        reconnect_player_id: &PlayerId,
        room_id: &RoomId,
        auth_token: &str,
        effective_player_id: Arc<tokio::sync::RwLock<PlayerId>>,
    ) -> bool {
        self.handle_reconnect_with_identity_operation(
            current_player_id,
            reconnect_player_id,
            room_id,
            auth_token,
            effective_player_id,
            None,
        )
        .await
    }

    pub(crate) async fn handle_reconnect_with_identity_operation(
        self: &Arc<Self>,
        current_player_id: &PlayerId,
        reconnect_player_id: &PlayerId,
        room_id: &RoomId,
        auth_token: &str,
        effective_player_id: Arc<tokio::sync::RwLock<PlayerId>>,
        operation_id: Option<crate::protocol::RoomOperationId>,
    ) -> bool {
        self.spawn_reconnect_transaction(
            *current_player_id,
            *reconnect_player_id,
            *room_id,
            auth_token.to_string(),
            Some(effective_player_id),
            operation_id,
        )
        .await
    }

    async fn spawn_reconnect_transaction(
        self: &Arc<Self>,
        current_player_id: PlayerId,
        reconnect_player_id: PlayerId,
        room_id: RoomId,
        auth_token: String,
        effective_player_id: Option<Arc<tokio::sync::RwLock<PlayerId>>>,
        operation_id: Option<crate::protocol::RoomOperationId>,
    ) -> bool {
        let server = Arc::clone(self);
        let effective_player_id_for_recovery = effective_player_id.clone();
        let terminal_response_committed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let terminal_response_committed_in_task = Arc::clone(&terminal_response_committed);
        let lifecycle_finalized = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let lifecycle_finalized_in_task = Arc::clone(&lifecycle_finalized);
        let opening_accounted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let opening_accounted_in_task = Arc::clone(&opening_accounted);
        let panic_recovery = Arc::new(std::sync::Mutex::new(ReconnectPanicRecovery::default()));
        let panic_recovery_in_task = Arc::clone(&panic_recovery);
        let task = tokio::spawn(async move {
            let Some(lifecycle) = server
                .connection_manager
                .client_lifecycle(&current_player_id)
            else {
                return false;
            };
            let _lifecycle_guard = Arc::clone(&lifecycle).lock_owned().await;
            if lifecycle.player_id() != current_player_id
                || !server
                    .connection_manager
                    .lifecycle_matches(&current_player_id, &lifecycle)
            {
                return false;
            }
            let outcome = AssertUnwindSafe(Arc::clone(&server).handle_reconnect_owned(
                current_player_id,
                reconnect_player_id,
                room_id,
                auth_token,
                effective_player_id,
                operation_id,
                terminal_response_committed_in_task,
                lifecycle_finalized_in_task,
                opening_accounted_in_task,
                panic_recovery_in_task,
            ))
            .catch_unwind()
            .await;
            match outcome {
                Ok(result) => result,
                Err(_) => {
                    tracing::error!(%current_player_id, %reconnect_player_id, %room_id, "Owned reconnect transaction panicked");
                    let recovery = panic_recovery
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .clone();
                    if terminal_response_committed.load(std::sync::atomic::Ordering::Acquire) {
                        let _ = server
                            .message_coordinator
                            .unregister_local_client(&current_player_id)
                            .await;
                        if let (Some(manager), Some(claim)) =
                            (&server.reconnection_manager, recovery.claim.as_ref())
                        {
                            let _ = manager.complete_claimed_reconnection(claim).await;
                            if !opening_accounted.swap(true, std::sync::atomic::Ordering::AcqRel) {
                                server.metrics.increment_players_joined();
                            }
                            if !lifecycle_finalized.load(std::sync::atomic::Ordering::Acquire) {
                                if let Some(room_event_guard) = recovery.room_event_guard.clone() {
                                    server
                                        .repair_panicked_reconnect_publication_locked(
                                            reconnect_player_id,
                                            room_id,
                                            &claim.disconnected,
                                            room_event_guard,
                                        )
                                        .await;
                                }
                            }
                        }
                        return true;
                    }

                    if recovery.reassigned {
                        server
                            .discard_pre_issued_reconnection_token(&reconnect_player_id)
                            .await;
                        let _ = server
                            .message_coordinator
                            .unregister_local_client(&reconnect_player_id)
                            .await;
                        let _ = server.connection_manager.restore_reassigned_connection(
                            &current_player_id,
                            &reconnect_player_id,
                        );
                        if let Some(effective_player_id) = &effective_player_id_for_recovery {
                            *effective_player_id.write().await = current_player_id;
                        }
                    }

                    if let (Some(manager), Some(claim)) =
                        (&server.reconnection_manager, recovery.claim)
                    {
                        let claim_guard = ReconnectionClaimGuard::new(Arc::clone(manager), claim);
                        server
                            .rollback_claimed_reconnect(
                                claim_guard,
                                &recovery.restore,
                                "Reconnect failed unexpectedly",
                            )
                            .await;
                    }
                    server
                        .send_unexpected_room_operation_failure(
                            current_player_id,
                            operation_id,
                            "Reconnect failed unexpectedly",
                        )
                        .await;
                    false
                }
            }
        });
        match task.await {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(%current_player_id, %reconnect_player_id, %room_id, %error, "Owned reconnect transaction failed");
                false
            }
        }
    }

    async fn handle_reconnect_owned(
        self: Arc<Self>,
        current_player_id: PlayerId,
        reconnect_player_id: PlayerId,
        room_id: RoomId,
        auth_token: String,
        effective_player_id: Option<Arc<tokio::sync::RwLock<PlayerId>>>,
        operation_id: Option<crate::protocol::RoomOperationId>,
        terminal_response_committed: Arc<std::sync::atomic::AtomicBool>,
        lifecycle_finalized: Arc<std::sync::atomic::AtomicBool>,
        opening_accounted: Arc<std::sync::atomic::AtomicBool>,
        panic_recovery: Arc<std::sync::Mutex<ReconnectPanicRecovery>>,
    ) -> bool {
        #[cfg(test)]
        self.trigger_owned_room_operation_panic_for_test(
            super::OwnedRoomOperationPanicPoint::ReconnectBeforeReassignment,
        );
        let current_player_id = &current_player_id;
        let reconnect_player_id = &reconnect_player_id;
        let room_id = &room_id;
        let auth_token = auth_token.as_str();
        // Shutdown-drain parity with the registration gate: only a socket
        // upgraded before the drain flip can still deliver `Reconnect` inside
        // the grace window. Admitting it would restore state the drain
        // teardown immediately removes, consume the one-time token, and end
        // in the forced 4000 close — refuse before any claim or restore side
        // effect, so the token stays spendable on a healthy instance.
        if self.is_draining() {
            let _ = self
                .message_coordinator
                .send_to_player(
                    current_player_id,
                    Arc::new(
                        (ServerMessage::ReconnectionFailed {
                            reason: "Server is draining for shutdown".to_string(),
                            error_code: ErrorCode::ServerDraining,
                        })
                        .correlate_room_operation(operation_id),
                    ),
                )
                .await;
            return false;
        }
        // Check if reconnection is enabled
        let Some(reconnection_manager) = &self.reconnection_manager else {
            tracing::warn!("Reconnection attempt but reconnection is disabled");
            let _ = self
                .message_coordinator
                .send_to_player(
                    current_player_id,
                    Arc::new(
                        (ServerMessage::ReconnectionFailed {
                            reason: "Reconnection is not enabled".to_string(),
                            error_code: ErrorCode::ReconnectionFailed,
                        })
                        .correlate_room_operation(operation_id),
                    ),
                )
                .await;
            return false;
        };

        // Check if player is already connected
        if self.connection_manager.has_client(reconnect_player_id) {
            let _ = self
                .message_coordinator
                .send_to_player(
                    current_player_id,
                    Arc::new(
                        (ServerMessage::ReconnectionFailed {
                            reason: "Player is already connected".to_string(),
                            error_code: ErrorCode::PlayerAlreadyConnected,
                        })
                        .correlate_room_operation(operation_id),
                    ),
                )
                .await;
            return false;
        }

        if self.get_client_room(current_player_id).await.is_some()
            || self.spectator_service.is_spectating(current_player_id)
        {
            let _ = self
                .message_coordinator
                .send_to_player(
                    current_player_id,
                    Arc::new(
                        (ServerMessage::ReconnectionFailed {
                            reason: "Reconnect must be attempted from a fresh connection"
                                .to_string(),
                            error_code: ErrorCode::ReconnectionFailed,
                        })
                        .correlate_room_operation(operation_id),
                    ),
                )
                .await;
            return false;
        }

        // Validate and atomically reserve the reconnection token before any
        // room or connection side effects. The record is only removed after
        // the restore succeeds; post-claim failures release it for retry.
        let reconnect_identity = self.client_reconnection_identity(current_player_id);
        let claim = match reconnection_manager
            .claim_reconnection_with_identity(
                current_player_id,
                reconnect_player_id,
                room_id,
                auth_token,
                reconnect_identity.as_deref(),
            )
            .await
        {
            Ok(claim) => claim,
            Err(error) => {
                // Typed classification: the error variant maps to its own
                // `ErrorCode` and wire `reason` (see [`ReconnectionError`]); no
                // error-string matching, so a token failure can never be
                // mislabeled as an expired window (and vice versa).
                let error_code = error.error_code();
                let reason = error.to_string();
                tracing::warn!(
                    %reconnect_player_id,
                    %room_id,
                    %error_code,
                    "Reconnection validation failed: {reason}"
                );
                let _ = self
                    .message_coordinator
                    .send_to_player(
                        current_player_id,
                        Arc::new(
                            (ServerMessage::ReconnectionFailed { reason, error_code })
                                .correlate_room_operation(operation_id),
                        ),
                    )
                    .await;
                return false;
            }
        };
        panic_recovery
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .claim = Some(claim.clone());
        let claim_guard = ReconnectionClaimGuard::new(Arc::clone(reconnection_manager), claim);
        let Some(disconnected) = claim_guard.disconnected() else {
            tracing::warn!(
                %reconnect_player_id,
                %room_id,
                "Reconnection claim guard was empty immediately after claim"
            );
            return false;
        };
        let last_sequence = disconnected.last_sequence;
        let mut restore = ReconnectRestoreState::default();

        // Defense-in-depth for unexpected concurrent ownership paths. The
        // claim above is what resolves duplicate same-token races.
        if self.connection_manager.has_client(reconnect_player_id) {
            return self
                .reject_claimed_reconnect(
                    current_player_id,
                    claim_guard,
                    &restore,
                    "Player is already connected",
                    ErrorCode::PlayerAlreadyConnected,
                    operation_id,
                )
                .await;
        }

        // Membership restoration, identity/routing publication, the directed
        // Reconnected baseline, and PlayerReconnected enqueue are one room
        // mutation transaction. In particular, a ready/spectator/join snapshot
        // cannot observe the restored DB member before its route is published.
        let mut room_event_guard = Some(
            self.message_coordinator
                .lock_room_event_mutation(room_id)
                .await,
        );
        panic_recovery
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .room_event_guard = room_event_guard.clone();

        // Get room from database
        let room = match self.database.get_room_by_id(room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        &restore,
                        "Room no longer exists",
                        ErrorCode::RoomNotFound,
                        operation_id,
                    )
                    .await;
            }
            Err(e) => {
                tracing::error!("Failed to get room for reconnection: {}", e);
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        &restore,
                        "Storage error",
                        ErrorCode::InternalError,
                        operation_id,
                    )
                    .await;
            }
        };

        // Reconnection tokens prove the prior player identity, not the
        // application principal on this new socket. Re-authorize against the
        // persisted room owner before restoring membership. Return the same
        // non-enumerating outcome as a missing room so an app cannot probe
        // another app's room through reconnect.
        let reconnect_app_context = if self.config.app_id_allowlist_enabled {
            let client_app_context = self.client_app_context(current_player_id);
            let client_app_id = client_app_context.as_ref().map(|app| app.id);
            if client_app_id.is_none()
                || room
                    .application_id
                    .is_some_and(|owner| Some(owner) != client_app_id)
            {
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        &restore,
                        "Room no longer exists",
                        ErrorCode::RoomNotFound,
                        operation_id,
                    )
                    .await;
            }
            if let Some(owner) = room.application_id {
                self.cache_room_application(room_id, owner);
            }
            client_app_context
        } else {
            None
        };
        let reconnect_app_id = reconnect_app_context.as_ref().map(|app| app.id);

        if !room.players.contains_key(reconnect_player_id) {
            let effective_max_players = reconnect_app_context
                .as_ref()
                .and_then(|app| app.max_players_per_room)
                .map_or(room.max_players, |app_limit| {
                    room.max_players.min(app_limit)
                });
            if room.players.len() >= usize::from(effective_max_players) {
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        &restore,
                        "Room is full",
                        ErrorCode::RoomFull,
                        operation_id,
                    )
                    .await;
            }

            let Some(player_info) = disconnected.player_info.clone() else {
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        &restore,
                        "Player room membership could not be restored",
                        ErrorCode::ReconnectionFailed,
                        operation_id,
                    )
                    .await;
            };

            match self.database.add_player_to_room(room_id, player_info).await {
                Ok(true) => {
                    restore.restored_membership = true;
                    panic_recovery
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .restore = restore.clone();
                }
                Ok(false) => {
                    return self
                        .reject_claimed_reconnect(
                            current_player_id,
                            claim_guard,
                            &restore,
                            "Room is full",
                            ErrorCode::RoomFull,
                            operation_id,
                        )
                        .await;
                }
                Err(err) => {
                    // A room deleted between the lane-held existence recheck
                    // above and this membership write (inactive-room GC)
                    // surfaces as a storage error here. Reclassify by
                    // re-checking existence — parity with the join path — so
                    // the client sees the accurate ROOM_NOT_FOUND wire code
                    // instead of a storage fault; anything else stays
                    // internal.
                    let room_missing =
                        matches!(self.database.get_room_by_id(room_id).await, Ok(None));
                    let (reason, error_code) = if room_missing {
                        ("Room not found", ErrorCode::RoomNotFound)
                    } else {
                        ("Storage error", ErrorCode::InternalError)
                    };
                    tracing::error!(
                        %reconnect_player_id,
                        %room_id,
                        error = %err,
                        "Failed to restore player room membership on reconnection"
                    );
                    return self
                        .reject_claimed_reconnect(
                            current_player_id,
                            claim_guard,
                            &restore,
                            reason,
                            error_code,
                            operation_id,
                        )
                        .await;
                }
            }
        }

        // A prior disconnect may have forced local teardown while durable
        // removal was unavailable. This room-gated reconnect now owns the
        // membership again, so maintenance must not delete it as stale. Keep
        // the retry it displaced: everything below can still reject, and a
        // rejection that owns no membership of its own would otherwise leave
        // the phantom row holding a seat with nothing queued to remove it.
        restore.cleared_pending_detach = self
            .pending_durable_player_detaches
            .remove(&(*room_id, *reconnect_player_id))
            .map(|(_, rollback)| rollback);
        panic_recovery
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .restore = restore.clone();

        if disconnected.was_authority && room.supports_authority && room.authority_player.is_none()
        {
            match self
                .database
                .request_room_authority(room_id, reconnect_player_id, true)
                .await
            {
                Ok(outcome) if outcome.granted() => {
                    restore.restored_authority = true;
                    panic_recovery
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .restore = restore.clone();
                }
                Ok(outcome) => {
                    tracing::debug!(
                        %reconnect_player_id,
                        %room_id,
                        denial = ?outcome.denial(),
                        "Reconnect authority restore lost an atomic authority race"
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        %reconnect_player_id,
                        %room_id,
                        error = %err,
                        "Failed to restore authority on reconnection"
                    );
                }
            }
        }
        // Settled for the rest of this attempt: publication below reads it from
        // an owned copy so the rejection paths keep the whole restore state.
        let restored_authority = restore.restored_authority;

        let room = match self.database.get_room_by_id(room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        &restore,
                        "Room no longer exists",
                        ErrorCode::RoomNotFound,
                        operation_id,
                    )
                    .await;
            }
            Err(err) => {
                tracing::error!("Failed to get restored room for reconnection: {}", err);
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        &restore,
                        "Storage error",
                        ErrorCode::InternalError,
                        operation_id,
                    )
                    .await;
            }
        };

        // Update client connection to use the reconnecting player's original id.
        // The resumed epoch is folded into the reassignment: `last_epoch + 1`
        // (the pre-disconnect epoch survived in the reconnection record) is the
        // epoch of the entry from the moment it becomes visible, so a recipient
        // that stayed connected sees the per-(sender, room) `(epoch, seq)`
        // stream strictly INCREASE across the reconnect instead of an ambiguous
        // reset to (1, 1) — and no provisional epoch is ever observable between
        // reassignment and a separate override. (Epoch is tracked server-side
        // for EVERY sender — it bumps on each join regardless of the sender's
        // own protocol version. It is still stripped per-recipient, so a v2
        // recipient never sees it while a v3 recipient gets a correct monotonic
        // stamp.)
        let resumed_epoch = disconnected.last_epoch.saturating_add(1);
        if disconnected.last_epoch == u32::MAX {
            // Saturation reuses the terminal incarnation (seq restarts at 1),
            // which is the ambiguity `epoch` exists to remove. Unreachable in
            // practice (~2^32 reconnects of one sender), but never silent.
            tracing::error!(
                %reconnect_player_id,
                "Reconnect epoch saturated at u32::MAX; the resumed stream reuses it"
            );
        }
        let reassigned_delivery = match self.connection_manager.reassign_connection(
            current_player_id,
            reconnect_player_id,
            *room_id,
            resumed_epoch,
        ) {
            crate::server::connection_manager::ReassignmentOutcome::Reassigned(delivery) => {
                delivery
            }
            crate::server::connection_manager::ReassignmentOutcome::TransientConnectionMissing => {
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        &restore,
                        "Current connection no longer exists",
                        ErrorCode::ReconnectionFailed,
                        operation_id,
                    )
                    .await;
            }
            crate::server::connection_manager::ReassignmentOutcome::RefusedTransientClose(
                reason,
            ) => {
                // The claiming socket was already scheduled to close for a
                // per-socket reason (reaper timeout, slow consumer, ...)
                // before the swap. Refusing keeps the stale close from
                // killing the restored identity; the record below is
                // released unspent, so a retry from a fresh socket
                // reconnects normally. The precise cause rides on the
                // close frame that follows (code + close_frame_reason).
                tracing::warn!(
                    %current_player_id,
                    %reconnect_player_id,
                    close_reason = reason.close_frame_reason(),
                    "Reconnect refused: claiming socket already carries a per-socket close"
                );
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        &restore,
                        "Connection scheduled to close; reconnect from a fresh connection",
                        ErrorCode::ReconnectionFailed,
                        operation_id,
                    )
                    .await;
            }
        };
        if let Some(effective_player_id) = &effective_player_id {
            *effective_player_id.write().await = *reconnect_player_id;
        }
        panic_recovery
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .reassigned = true;
        #[cfg(test)]
        self.trigger_owned_room_operation_panic_for_test(
            super::OwnedRoomOperationPanicPoint::ReconnectAfterReassignment,
        );

        // Update database last_seen
        if let Err(e) = self
            .database
            .update_player_last_seen(reconnect_player_id)
            .await
        {
            tracing::warn!(
                %reconnect_player_id,
                "Failed to update last_seen on reconnection: {}",
                e
            );
        }

        // The authoritative room snapshot is fetched inside the coordinator's
        // atomic registration builder below. An earlier DB snapshot cannot tell
        // whether a concurrent join is already routed or is still waiting to
        // publish PlayerJoined after this baseline.
        let recipient_is_v3 = self.connection_manager.supports_v3(reconnect_player_id);
        let response_room_id = *room_id;
        let response_player_id = *reconnect_player_id;
        let reconnection_manager_for_baseline = Arc::clone(reconnection_manager);
        let baseline_publication = Arc::new(std::sync::Mutex::new(None));
        let baseline_publication_in_builder = Arc::clone(&baseline_publication);
        let server = Arc::clone(&self);

        // Queue `Reconnected` before putting the restored connection back into
        // room routing. The coordinator holds the room-routing write lock while
        // this async closure runs. Room-control broadcasts record replay under
        // the corresponding read lock, so missed-event capture cannot slip
        // before an older broadcast's replay record while the restored socket
        // also misses live delivery. Game-data broadcasts allocate their relay
        // stamp in the same read section that snapshots recipients, so every
        // earlier stamp is reflected in the watermarks and cannot route to this
        // socket; later broadcasts cannot route to it until after the baseline
        // frame is queued.
        let initial_delivery = self
            .message_coordinator
            .register_local_client_with_initial_message_async(
                *reconnect_player_id,
                *room_id,
                reassigned_delivery,
                Box::new(move |routed_player_ids| {
                    Box::pin(async move {
                        let routed_player_ids: HashSet<PlayerId> =
                            routed_player_ids.into_iter().collect();
                        let current_room = server
                            .database
                            .get_room_by_id(&response_room_id)
                            .await
                            .map_err(|error| {
                                anyhow::anyhow!("failed to fetch reconnect room baseline: {error}")
                            })?
                            .ok_or_else(|| anyhow::anyhow!("reconnect room disappeared"))?;
                        let mut response_players = server
                            .database
                            .get_room_players(&response_room_id)
                            .await
                            .map_err(|error| {
                                anyhow::anyhow!(
                                    "failed to fetch reconnect baseline players: {error}"
                                )
                            })?;
                        response_players.retain(|player| routed_player_ids.contains(&player.id));
                        if !response_players
                            .iter()
                            .any(|player| player.id == response_player_id)
                        {
                            return Err(anyhow::anyhow!(
                                "restored player missing from reconnect baseline"
                            ));
                        }
                        if server.should_retain_room_publication_snapshot() {
                            *baseline_publication_in_builder
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                                Some((current_room.clone(), response_players.clone()));
                        }

                        let ready_players = super::ready_state::snapshot_ready_players(
                            &current_room,
                            server.room_coordinator.as_ref(),
                        )
                        .await;
                        for player in &mut response_players {
                            player.is_ready = ready_players.contains(&player.id);
                            player.epoch = None;
                            player.seq = None;
                        }

                        // Get missed events inside the coordinator registration
                        // critical section. Completion below may release the
                        // room's replay ring when this player is the last one
                        // pending, so this remains the last capture point.
                        let mut missed_events = reconnection_manager_for_baseline
                            .get_missed_events(&response_room_id, last_sequence)
                            .await;
                        // Project the DB membership through one room-bound live
                        // stamp read per player. The same captured stamp supplies
                        // both the snapshot epoch and watermark, so concurrent
                        // leave/unregister can only filter a player, never create
                        // an epoch-less or incomplete v3 baseline.
                        let (response_players, sender_watermarks) = if recipient_is_v3 {
                            let projected: Vec<_> = response_players
                                .into_iter()
                                .filter_map(|mut player| {
                                    let stamp =
                                        server.connection_manager.current_relay_stamp_in_room(
                                            &player.id,
                                            &response_room_id,
                                        )?;
                                    player.epoch = Some(stamp.epoch);
                                    player.seq = Some(stamp.seq);
                                    Some((player, stamp))
                                })
                                .collect();
                            let watermarks = projected
                                .iter()
                                .map(|(player, stamp)| SenderWatermark {
                                    player_id: player.id,
                                    epoch: stamp.epoch,
                                    seq: stamp.seq,
                                })
                                .collect();
                            (
                                projected.into_iter().map(|(player, _)| player).collect(),
                                watermarks,
                            )
                        } else {
                            (response_players, Vec::new())
                        };
                        let mut response_ready_players = ready_players;
                        response_ready_players.retain(|player_id| {
                            response_players
                                .iter()
                                .any(|player| player.id == *player_id)
                        });
                        // Never replay the reconnecting player's OWN membership
                        // deltas back to it: on reconnect it is being RESTORED
                        // (its presence is in the `Reconnected` snapshot and
                        // peers get `PlayerReconnected`), so a buffered
                        // self-delta is not room news it missed.
                        missed_events.events.retain(|event| match event {
                            ServerMessage::PlayerLeft { player_id, .. }
                            | ServerMessage::PlayerReconnected { player_id, .. } => {
                                *player_id != response_player_id
                            }
                            ServerMessage::PlayerJoined { player } => {
                                player.id != response_player_id
                            }
                            _ => true,
                        });

                        // The `Reconnected` snapshot states the room's CURRENT
                        // authority, so any buffered `AuthorityChanged` naming a
                        // different holder is already superseded — replaying it
                        // would contradict the frame that carries it. A
                        // departing authority's `{null}` and this member's own
                        // restore are the common case: the reconnector left as
                        // authority, the departure cleared the role, and the
                        // restore granted it back before this payload was built.
                        let current_authority = current_room.authority_player;
                        missed_events.events.retain(|event| match event {
                            ServerMessage::AuthorityChanged {
                                authority_player, ..
                            } => *authority_player == current_authority,
                            _ => true,
                        });

                        // AuthorityChanged is recorded once in canonical
                        // room-uniform form. Its `you_are_authority` bit is
                        // recipient-relative, so project it for this
                        // reconnector just as live room delivery does.
                        for event in &mut missed_events.events {
                            if let ServerMessage::AuthorityChanged {
                                authority_player,
                                you_are_authority,
                            } = event
                            {
                                *you_are_authority = *authority_player == Some(response_player_id);
                            }
                        }

                        // The replay ring stores v3 incarnation epochs in live
                        // broadcast form, but `missed_events` is embedded in
                        // `Reconnected` and bypasses per-recipient top-level
                        // stripping. Strip those fields for pre-v3 reconnectors.
                        if !recipient_is_v3 {
                            for event in &mut missed_events.events {
                                match event {
                                    ServerMessage::PlayerJoined { player } => {
                                        player.epoch = None;
                                        player.seq = None;
                                    }
                                    ServerMessage::PlayerReconnected { epoch, .. } => *epoch = None,
                                    ServerMessage::PlayerLeft {
                                        epoch, final_seq, ..
                                    } => {
                                        *epoch = None;
                                        *final_seq = None;
                                    }
                                    _ => {}
                                }
                            }
                        }

                        // Replay completeness (v3+ recipients only; absent on
                        // the v2 wire). `Unavailable` wins over `Truncated`: a
                        // zero-capacity ring evicts everything, but the honest
                        // contract is "replay is off, resync".
                        let replay = if recipient_is_v3 {
                            Some(
                                if reconnection_manager_for_baseline.event_buffer_size() == 0 {
                                    ReplayStatus::Unavailable
                                } else if missed_events.truncated {
                                    ReplayStatus::Truncated
                                } else {
                                    ReplayStatus::Complete
                                },
                            )
                        } else {
                            None
                        };
                        let missed_events = missed_events.events;
                        Ok(Arc::new(
                            (ServerMessage::Reconnected(Box::new(ReconnectedPayload {
                                room_id: response_room_id,
                                room_code: current_room.code.clone(),
                                player_id: response_player_id,
                                game_name: current_room.game_name.clone(),
                                max_players: current_room.max_players,
                                supports_authority: current_room.supports_authority,
                                current_players: response_players,
                                is_authority: current_room.authority_player
                                    == Some(response_player_id),
                                lobby_state: current_room.lobby_state.clone(),
                                ready_players: response_ready_players,
                                relay_type: current_room.relay_type.clone(),
                                current_spectators: current_room.get_spectators(),
                                // v3 ICE pre-gather (deferred refinement): empty —
                                // and skipped on the wire — unless this reconnector passes
                                // the pre-gather gate (its original credentials may have
                                // expired while it was away), so v2 bytes are untouched. A
                                // reconnect into a Finalized room gets fresh ICE from the
                                // late-join SessionPlan below instead (never both).
                                ice_servers: server
                                    .pregather_ice_servers(&current_room, &response_player_id),
                                missed_events,
                                replay,
                                sender_watermarks,
                                // Rotate: the token just used was consumed with the
                                // completed claim; the restored player gets a fresh one
                                // for its NEXT unexpected disconnect (v3+ only).
                                reconnection_token: server
                                    .pre_issue_reconnection_token_for(
                                        &response_player_id,
                                        response_room_id,
                                    )
                                    .await,
                            })))
                            .correlate_room_operation(operation_id),
                        ))
                    })
                }),
            )
            .await;
        match initial_delivery {
            Ok(crate::coordination::DeliveryOutcome::Delivered) => {
                terminal_response_committed.store(true, std::sync::atomic::Ordering::Release);
                #[cfg(test)]
                self.trigger_owned_room_operation_panic_for_test(
                    super::OwnedRoomOperationPanicPoint::ReconnectAfterTerminal,
                );
                if room.application_id == reconnect_app_id {
                    if let Some(application_id) = reconnect_app_id {
                        self.mark_pending_room_application_claim_adopted(*room_id, application_id);
                    }
                }
            }
            Ok(outcome) => {
                tracing::warn!(
                    %reconnect_player_id,
                    %room_id,
                    ?outcome,
                    "Reconnection restored state but could not queue the Reconnected baseline"
                );
                if let Some(effective_player_id) = &effective_player_id {
                    *effective_player_id.write().await = *current_player_id;
                }
                return self
                    .reject_after_reassigned_reconnect_failure(
                        current_player_id,
                        reconnect_player_id,
                        claim_guard,
                        &restore,
                        "baseline_delivery",
                        operation_id,
                    )
                    .await;
            }
            Err(err) => {
                tracing::warn!(
                    %reconnect_player_id,
                    %room_id,
                    error = %err,
                    "Failed to atomically register reassigned connection with coordinator"
                );
                if let Some(effective_player_id) = &effective_player_id {
                    *effective_player_id.write().await = *current_player_id;
                }
                return self
                    .reject_after_reassigned_reconnect_failure(
                        current_player_id,
                        reconnect_player_id,
                        claim_guard,
                        &restore,
                        "coordinator_registration",
                        operation_id,
                    )
                    .await;
            }
        }

        let _ = self
            .message_coordinator
            .unregister_local_client(current_player_id)
            .await;

        // Complete only after `Reconnected` is queued and the restored player is
        // visible to room routing. A failure before this point releases the claim
        // for retry, so the token is never consumed without a delivered baseline.
        if !claim_guard.complete().await {
            tracing::warn!(
                %reconnect_player_id,
                %room_id,
                "Reconnection succeeded but pending claim was already released"
            );
        }

        // Publish the canonical lifecycle event and, for a Finalized room, an
        // authoritative v3 plan refresh as one exact-membership transaction.
        // Phase zero queues the actor's plan and each incumbent's
        // PlayerReconnected; phase one queues incumbent plans. Thus the actor's
        // inbound handler cannot admit Signal before its plan is queued. A
        // missing sticky plan is explicit Relay/Relay rather than silence, so
        // every v3 client can leave stale P2P state deterministically. Waiting
        // and Lobby rooms publish only the lifecycle event.
        let Some(stamp) = self
            .connection_manager
            .current_relay_stamp_in_room(reconnect_player_id, room_id)
        else {
            drop(room_event_guard);
            tracing::debug!(%reconnect_player_id, %room_id, "Reconnect publication skipped because the restored connection already left");
            return true;
        };
        let notification = Arc::new(ServerMessage::PlayerReconnected {
            player_id: *reconnect_player_id,
            epoch: Some(stamp.epoch),
        });
        let Some((publication_room, publication_players)) = baseline_publication
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        else {
            tracing::warn!(%reconnect_player_id, %room_id, "Reconnect baseline committed without its publication snapshot");
            if let Some(player_info) = room
                .players
                .get(reconnect_player_id)
                .cloned()
                .or_else(|| disconnected.player_info.clone())
            {
                self.terminate_room_generation_after_publication_failure(
                    *reconnect_player_id,
                    *room_id,
                    stamp.epoch,
                    player_info,
                    restored_authority || disconnected.was_authority,
                    "reconnect_baseline_snapshot",
                )
                .await;
            }
            if let Err(error) = self
                .publish_reconnect_lifecycle_fallback(
                    *room_id,
                    *reconnect_player_id,
                    Arc::clone(&notification),
                    restored_authority,
                )
                .await
            {
                tracing::error!(%reconnect_player_id, %room_id, %error, "Failed to publish opening lifecycle before terminating inconsistent reconnect");
            }
            drop(room_event_guard);
            return true;
        };
        if !publication_players
            .iter()
            .any(|player| player.id == *reconnect_player_id)
        {
            tracing::warn!(%reconnect_player_id, %room_id, "Reconnect publication snapshot omitted the restored member");
            if let Some(player_info) = publication_room
                .players
                .get(reconnect_player_id)
                .cloned()
                .or_else(|| disconnected.player_info.clone())
            {
                self.terminate_room_generation_after_publication_failure(
                    *reconnect_player_id,
                    *room_id,
                    stamp.epoch,
                    player_info,
                    publication_room.authority_player == Some(*reconnect_player_id)
                        || restored_authority
                        || disconnected.was_authority,
                    "reconnect_publication_membership",
                )
                .await;
            }
            if let Err(error) = self
                .publish_reconnect_lifecycle_fallback(
                    *room_id,
                    *reconnect_player_id,
                    Arc::clone(&notification),
                    restored_authority,
                )
                .await
            {
                tracing::error!(%reconnect_player_id, %room_id, %error, "Failed to publish opening lifecycle before terminating incomplete reconnect publication");
            }
            drop(room_event_guard);
            return true;
        }
        let reconnect_epoch = stamp.epoch;
        let room_id_for_publication = *room_id;
        let reconnect_player_for_publication = *reconnect_player_id;
        let authority_player = publication_room.authority_player;
        let reconnect_player_snapshot = publication_room
            .players
            .get(reconnect_player_id)
            .cloned()
            .or_else(|| {
                publication_players
                    .iter()
                    .find(|player| player.id == *reconnect_player_id)
                    .cloned()
            });
        let reconnect_was_authority = authority_player == Some(*reconnect_player_id);
        let finalized = publication_room.lobby_state == crate::protocol::LobbyState::Finalized;
        let server_for_publication = Arc::clone(&self);
        let Some(lifecycle_guard) = room_event_guard.take() else {
            tracing::error!(%reconnect_player_id, %room_id, "Reconnect publication lost its room event guard");
            let fallback_guard = panic_recovery
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .room_event_guard
                .take();
            let Some(fallback_guard) = fallback_guard else {
                tracing::error!(%reconnect_player_id, %room_id, "Reconnect publication also lost its retained recovery lease");
                return true;
            };
            if let Some(player_info) = reconnect_player_snapshot {
                self.terminate_room_generation_after_publication_failure(
                    *reconnect_player_id,
                    *room_id,
                    reconnect_epoch,
                    player_info,
                    reconnect_was_authority,
                    "reconnect_publication_guard",
                )
                .await;
            }
            if let Err(error) = self
                .publish_reconnect_lifecycle_fallback(
                    *room_id,
                    *reconnect_player_id,
                    Arc::clone(&notification),
                    restored_authority,
                )
                .await
            {
                tracing::error!(%reconnect_player_id, %room_id, %error, "Failed to publish reconnect lifecycle after publication guard loss");
            }
            drop(fallback_guard);
            return true;
        };
        let completion = self.message_coordinator.enqueue_room_event(
            lifecycle_guard,
            Box::new(move || {
                Box::pin(async move {
                    let mut live_players = publication_players;
                    let mut attempts_remaining = live_players.len().saturating_add(1);
                    // Mixed-path membership is a fact about the room snapshot
                    // this publication resolved, not about its delivery
                    // outcome: observe it exactly once per event, not once per
                    // reservation retry.
                    let mut mixed_path_observed = false;
                    loop {
                        let resolved = membership_session_decision(
                            finalized
                                .then(|| {
                                    server_for_publication
                                        .active_session_plan(&room_id_for_publication)
                                })
                                .flatten(),
                            authority_player,
                            server_for_publication.session_members_from(&live_players),
                        );
                        if finalized && !mixed_path_observed {
                            server_for_publication.observe_mixed_path_members(
                                &room_id_for_publication,
                                &resolved.decision,
                            );
                            mixed_path_observed = true;
                        }
                        let expected_members: Vec<_> = resolved
                            .decision
                            .members
                            .iter()
                            .map(|member| member.player_id)
                            .collect();
                        // Wall clock (durable record): TURN credentials carry
                        // absolute expiry instants the client sees.
                        let now_unix = resolved
                            .decision
                            .uses_webrtc_signaling()
                            .then(|| chrono::Utc::now().timestamp());
                        let mut turn_credentials_issued = 0_u64;
                        let mut reconnector_has_plan = false;
                        let recipient_messages: Vec<_> = resolved
                            .decision
                            .members
                            .iter()
                            .map(|member| {
                                let plan = finalized
                                    .then(|| {
                                        server_for_publication.build_session_plan_message(
                                            &resolved.decision,
                                            member.player_id,
                                            now_unix,
                                        )
                                    })
                                    .flatten()
                                    .map(|(message, minted)| {
                                        turn_credentials_issued =
                                            turn_credentials_issued.saturating_add(minted);
                                        message
                                    });
                                if member.player_id == reconnect_player_for_publication {
                                    reconnector_has_plan = plan.is_some();
                                    return RoomRecipientMessages::in_order(
                                        member.player_id,
                                        plan.into_iter().collect(),
                                    );
                                }
                                let mut messages = vec![Arc::clone(&notification)];
                                messages.extend(plan);
                                RoomRecipientMessages::in_order(member.player_id, messages)
                            })
                            .collect();

                        if recipient_messages
                            .iter()
                            .all(|batch| batch.messages.is_empty())
                        {
                            return server_for_publication
                                .publish_reconnect_lifecycle_fallback(
                                    room_id_for_publication,
                                    reconnect_player_for_publication,
                                    Arc::clone(&notification),
                                    restored_authority,
                                )
                                .await;
                        }
                        let active_plan_update = resolved.active_plan_update;
                        let is_replan = resolved.is_replan;
                        let replay_notification = Arc::clone(&notification);
                        let server_for_hook = Arc::clone(&server_for_publication);
                        let outcome = match server_for_publication
                            .message_coordinator
                            .commit_room_messages_if_members_with_hook(
                                &room_id_for_publication,
                                &expected_members,
                                recipient_messages,
                                Box::new(move || {
                                    Box::pin(async move {
                                        server_for_hook
                                            .commit_reconnect_publication_state(
                                                room_id_for_publication,
                                                replay_notification,
                                                active_plan_update,
                                                is_replan,
                                                reconnector_has_plan && !is_replan,
                                                turn_credentials_issued,
                                            )
                                            .await;
                                        Ok(true)
                                    })
                                }),
                                // Phase-one incumbent plan refreshes remain
                                // authoritative even if the actor's phase-zero
                                // plan closes after the final commit hook.
                                Box::new(move |_failed_phase_zero| true),
                            )
                            .await
                        {
                            Ok(outcome) => outcome,
                            Err(error) => {
                                tracing::error!(
                                    %room_id_for_publication,
                                    player_id = %reconnect_player_for_publication,
                                    %error,
                                    "Reconnect room publication transaction failed"
                                );
                                if let Some(player_info) = reconnect_player_snapshot.clone() {
                                    server_for_publication
                                        .terminate_room_generation_after_publication_failure(
                                            reconnect_player_for_publication,
                                            room_id_for_publication,
                                            reconnect_epoch,
                                            player_info,
                                            reconnect_was_authority,
                                            "reconnect",
                                        )
                                        .await;
                                }
                                return server_for_publication
                                    .publish_reconnect_lifecycle_fallback(
                                        room_id_for_publication,
                                        reconnect_player_for_publication,
                                        Arc::clone(&notification),
                                        restored_authority,
                                    )
                                    .await;
                            }
                        };
                        match outcome {
                            RoomMessageTransactionOutcome::Committed => {
                                if restored_authority {
                                    server_for_publication
                                        .publish_reconnect_authority_change(
                                            room_id_for_publication,
                                            reconnect_player_for_publication,
                                        )
                                        .await?;
                                }
                                return Ok(true);
                            }
                            RoomMessageTransactionOutcome::CommittedDegraded { failed_frames } => {
                                tracing::warn!(
                                    %room_id_for_publication,
                                    failed_frames,
                                    "Reconnect publication committed with degraded frame delivery"
                                );
                                if restored_authority {
                                    server_for_publication
                                        .publish_reconnect_authority_change(
                                            room_id_for_publication,
                                            reconnect_player_for_publication,
                                        )
                                        .await?;
                                }
                                return Ok(true);
                            }
                            RoomMessageTransactionOutcome::HookRejected => {
                                if let Some(player_info) = reconnect_player_snapshot.clone() {
                                    server_for_publication
                                        .terminate_room_generation_after_publication_failure(
                                            reconnect_player_for_publication,
                                            room_id_for_publication,
                                            reconnect_epoch,
                                            player_info,
                                            reconnect_was_authority,
                                            "reconnect_hook",
                                        )
                                        .await;
                                }
                                return server_for_publication
                                    .publish_reconnect_lifecycle_fallback(
                                        room_id_for_publication,
                                        reconnect_player_for_publication,
                                        Arc::clone(&notification),
                                        restored_authority,
                                    )
                                    .await;
                            }
                            RoomMessageTransactionOutcome::RoutingChanged => {}
                        }

                        attempts_remaining = attempts_remaining.saturating_sub(1);
                        let routed = match server_for_publication
                            .message_coordinator
                            .routed_player_ids(&room_id_for_publication)
                            .await
                        {
                            Ok(Some(routed)) => routed.into_iter().collect::<HashSet<_>>(),
                            Ok(None) => live_players
                                .iter()
                                .filter(|player| {
                                    server_for_publication
                                        .connection_manager
                                        .current_relay_stamp_in_room(
                                            &player.id,
                                            &room_id_for_publication,
                                        )
                                        .is_some()
                                })
                                .map(|player| player.id)
                                .collect(),
                            Err(error) => {
                                tracing::warn!(
                                    %room_id_for_publication,
                                    %error,
                                    "Failed to refresh reconnect routing; deriving the local route from generation-fenced connections"
                                );
                                live_players
                                    .iter()
                                    .filter(|player| {
                                        server_for_publication
                                            .connection_manager
                                            .current_relay_stamp_in_room(
                                                &player.id,
                                                &room_id_for_publication,
                                            )
                                            .is_some()
                                    })
                                    .map(|player| player.id)
                                    .collect()
                            }
                        };
                        live_players.retain(|player| routed.contains(&player.id));
                        let actor_is_routed = routed.contains(&reconnect_player_for_publication);
                        if attempts_remaining == 0 && actor_is_routed {
                            if let Some(player_info) = reconnect_player_snapshot.clone() {
                                server_for_publication
                                    .terminate_room_generation_after_publication_failure(
                                        reconnect_player_for_publication,
                                        room_id_for_publication,
                                        reconnect_epoch,
                                        player_info,
                                        reconnect_was_authority,
                                        "reconnect_routing",
                                    )
                                .await;
                            }
                        }
                        if !actor_is_routed {
                            if let Some(player_info) = reconnect_player_snapshot.clone() {
                                server_for_publication
                                    .terminate_room_generation_after_publication_failure(
                                        reconnect_player_for_publication,
                                        room_id_for_publication,
                                        reconnect_epoch,
                                        player_info,
                                        reconnect_was_authority,
                                        "reconnect_unrouted",
                                    )
                                    .await;
                            }
                        }
                        if !actor_is_routed || attempts_remaining == 0 {
                            return server_for_publication
                                .publish_reconnect_lifecycle_fallback(
                                    room_id_for_publication,
                                    reconnect_player_for_publication,
                                    Arc::clone(&notification),
                                    restored_authority,
                                )
                                .await;
                        }
                    }
                })
            }),
        );
        drop(room_event_guard);
        match completion.await {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!(%reconnect_player_id, %room_id, "Reconnect publication canceled by a routing change");
            }
            Err(error) => {
                tracing::warn!(%reconnect_player_id, %room_id, %error, "Reconnect publication failed");
                let repair_guard = panic_recovery
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .room_event_guard
                    .clone();
                if let Some(repair_guard) = repair_guard {
                    self.repair_panicked_reconnect_publication_locked(
                        *reconnect_player_id,
                        *room_id,
                        &disconnected,
                        repair_guard,
                    )
                    .await;
                }
            }
        }
        panic_recovery
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .room_event_guard
            .take();
        if !opening_accounted.swap(true, std::sync::atomic::Ordering::AcqRel) {
            self.metrics.increment_players_joined();
        }
        lifecycle_finalized.store(true, std::sync::atomic::Ordering::Release);
        tracing::info!(
            %reconnect_player_id,
            %room_id,
            room_code = %room.code,
            "Player reconnected successfully"
        );
        true
    }
}
