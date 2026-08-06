use crate::protocol::{
    ErrorCode, PlayerId, PlayerInfo, ReconnectedPayload, ReplayStatus, RoomId, SenderWatermark,
    ServerMessage,
};
use crate::reconnection::{ClaimedReconnection, DisconnectedPlayer, ReconnectionManager};
use std::collections::HashSet;
use std::sync::Arc;

use crate::coordination::{RoomMessageTransactionOutcome, RoomRecipientMessages};

use super::session_policy::{membership_session_decision, ActiveSessionPlan};
use super::EnhancedGameServer;

struct ReconnectionClaimGuard {
    manager: Arc<ReconnectionManager>,
    claim: Option<ClaimedReconnection>,
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
        let Some(claim) = self.claim.take() else {
            return;
        };
        let manager = Arc::clone(&self.manager);

        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                player_id = %claim.disconnected.player_id,
                room_id = %claim.disconnected.room_id,
                "Reconnection claim guard dropped outside a Tokio runtime; claim release could not be scheduled"
            );
            return;
        };

        drop(handle.spawn(async move {
            let released = manager.release_reconnection_claim(&claim).await;
            tracing::warn!(
                player_id = %claim.disconnected.player_id,
                room_id = %claim.disconnected.room_id,
                %released,
                "Reconnection claim released by dropped restore guard"
            );
        }));
    }
}

impl EnhancedGameServer {
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
                .pre_issue_token(*player_id, room_id)
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
    ) {
        let Some(reconnection_manager) = &self.reconnection_manager else {
            return;
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
            .register_disconnection(
                *player_id,
                room_id,
                was_authority,
                Some(player_info),
                last_epoch,
            )
            .await;

        tracing::info!(
            %player_id,
            %room_id,
            %was_authority,
            reconnection_token = %token.get(..8).unwrap_or("<invalid>"),
            "Player disconnection registered for reconnection"
        );
    }

    async fn reject_claimed_reconnect(
        &self,
        current_player_id: &PlayerId,
        claim_guard: ReconnectionClaimGuard,
        restored_membership: bool,
        restored_authority: bool,
        reason: &str,
        error_code: ErrorCode,
    ) -> bool {
        let Some(disconnected) = claim_guard.disconnected() else {
            tracing::warn!(%reason, "Reconnection rejection had no active claim to release");
            return false;
        };
        if restored_membership {
            if let Err(err) = self
                .database
                .remove_player_from_room(&disconnected.room_id, &disconnected.player_id)
                .await
            {
                self.pending_durable_player_detaches
                    .insert((disconnected.room_id, disconnected.player_id), None);
                tracing::warn!(
                    player_id = %disconnected.player_id,
                    room_id = %disconnected.room_id,
                    error = %err,
                    "Failed to roll back restored room membership after reconnect failure"
                );
            }
        } else if restored_authority {
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

        let released = claim_guard.release().await;
        tracing::warn!(
            player_id = %disconnected.player_id,
            room_id = %disconnected.room_id,
            %released,
            %reason,
            "Reconnection claim released after restore failure"
        );

        let _ = self
            .message_coordinator
            .send_to_player(
                current_player_id,
                Arc::new(ServerMessage::ReconnectionFailed {
                    reason: reason.to_string(),
                    error_code,
                }),
            )
            .await;
        false
    }

    async fn reject_after_reassigned_reconnect_failure(
        &self,
        current_player_id: &PlayerId,
        reconnect_player_id: &PlayerId,
        claim_guard: ReconnectionClaimGuard,
        restored_membership: bool,
        restored_authority: bool,
        rollback_context: &'static str,
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
            restored_membership,
            restored_authority,
            "Reconnected baseline could not be delivered",
            ErrorCode::ReconnectionFailed,
        )
        .await
    }

    /// Handle player reconnection
    pub async fn handle_reconnect(
        self: &Arc<Self>,
        current_player_id: &PlayerId,
        reconnect_player_id: &PlayerId,
        room_id: &RoomId,
        auth_token: &str,
    ) -> bool {
        self.spawn_reconnect_transaction(
            *current_player_id,
            *reconnect_player_id,
            *room_id,
            auth_token.to_string(),
            None,
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
        self.spawn_reconnect_transaction(
            *current_player_id,
            *reconnect_player_id,
            *room_id,
            auth_token.to_string(),
            Some(effective_player_id),
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
    ) -> bool {
        let server = Arc::clone(self);
        let task = tokio::spawn(async move {
            server
                .handle_reconnect_owned(
                    current_player_id,
                    reconnect_player_id,
                    room_id,
                    auth_token,
                    effective_player_id,
                )
                .await
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
    ) -> bool {
        let current_player_id = &current_player_id;
        let reconnect_player_id = &reconnect_player_id;
        let room_id = &room_id;
        let auth_token = auth_token.as_str();
        let Some(lifecycle) = self.connection_manager.client_lifecycle(current_player_id) else {
            return false;
        };
        let _lifecycle_guard = Arc::clone(&lifecycle).lock_owned().await;
        if lifecycle.player_id() != *current_player_id
            || !self
                .connection_manager
                .lifecycle_matches(current_player_id, &lifecycle)
        {
            return false;
        }

        // Check if reconnection is enabled
        let Some(reconnection_manager) = &self.reconnection_manager else {
            tracing::warn!("Reconnection attempt but reconnection is disabled");
            let _ = self
                .message_coordinator
                .send_to_player(
                    current_player_id,
                    Arc::new(ServerMessage::ReconnectionFailed {
                        reason: "Reconnection is not enabled".to_string(),
                        error_code: ErrorCode::ReconnectionFailed,
                    }),
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
                    Arc::new(ServerMessage::ReconnectionFailed {
                        reason: "Player is already connected".to_string(),
                        error_code: ErrorCode::PlayerAlreadyConnected,
                    }),
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
                    Arc::new(ServerMessage::ReconnectionFailed {
                        reason: "Reconnect must be attempted from a fresh connection".to_string(),
                        error_code: ErrorCode::ReconnectionFailed,
                    }),
                )
                .await;
            return false;
        }

        // Validate and atomically reserve the reconnection token before any
        // room or connection side effects. The record is only removed after
        // the restore succeeds; post-claim failures release it for retry.
        let claim = match reconnection_manager
            .claim_reconnection(current_player_id, reconnect_player_id, room_id, auth_token)
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
                        Arc::new(ServerMessage::ReconnectionFailed { reason, error_code }),
                    )
                    .await;
                return false;
            }
        };
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
        let mut restored_membership = false;
        let mut restored_authority = false;

        // Defense-in-depth for unexpected concurrent ownership paths. The
        // claim above is what resolves duplicate same-token races.
        if self.connection_manager.has_client(reconnect_player_id) {
            return self
                .reject_claimed_reconnect(
                    current_player_id,
                    claim_guard,
                    restored_membership,
                    restored_authority,
                    "Player is already connected",
                    ErrorCode::PlayerAlreadyConnected,
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

        // Get room from database
        let room = match self.database.get_room_by_id(room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        restored_membership,
                        restored_authority,
                        "Room no longer exists",
                        ErrorCode::RoomNotFound,
                    )
                    .await;
            }
            Err(e) => {
                tracing::error!("Failed to get room for reconnection: {}", e);
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        restored_membership,
                        restored_authority,
                        "Storage error",
                        ErrorCode::InternalError,
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
                        restored_membership,
                        restored_authority,
                        "Room no longer exists",
                        ErrorCode::RoomNotFound,
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
                        restored_membership,
                        restored_authority,
                        "Room is full",
                        ErrorCode::RoomFull,
                    )
                    .await;
            }

            let Some(player_info) = disconnected.player_info.clone() else {
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        restored_membership,
                        restored_authority,
                        "Player room membership could not be restored",
                        ErrorCode::ReconnectionFailed,
                    )
                    .await;
            };

            match self.database.add_player_to_room(room_id, player_info).await {
                Ok(true) => {
                    restored_membership = true;
                }
                Ok(false) => {
                    return self
                        .reject_claimed_reconnect(
                            current_player_id,
                            claim_guard,
                            restored_membership,
                            restored_authority,
                            "Room is full",
                            ErrorCode::RoomFull,
                        )
                        .await;
                }
                Err(err) => {
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
                            restored_membership,
                            restored_authority,
                            "Storage error",
                            ErrorCode::InternalError,
                        )
                        .await;
                }
            }
        }

        // A prior disconnect may have forced local teardown while durable
        // removal was unavailable. This room-gated reconnect now owns the
        // membership again, so maintenance must not delete it as stale.
        self.pending_durable_player_detaches
            .remove(&(*room_id, *reconnect_player_id));

        if disconnected.was_authority && room.supports_authority && room.authority_player.is_none()
        {
            match self
                .database
                .request_room_authority(room_id, reconnect_player_id, true)
                .await
            {
                Ok((true, _)) => {
                    restored_authority = true;
                }
                Ok((false, reason)) => {
                    tracing::debug!(
                        %reconnect_player_id,
                        %room_id,
                        ?reason,
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

        let room = match self.database.get_room_by_id(room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        restored_membership,
                        restored_authority,
                        "Room no longer exists",
                        ErrorCode::RoomNotFound,
                    )
                    .await;
            }
            Err(err) => {
                tracing::error!("Failed to get restored room for reconnection: {}", err);
                return self
                    .reject_claimed_reconnect(
                        current_player_id,
                        claim_guard,
                        restored_membership,
                        restored_authority,
                        "Storage error",
                        ErrorCode::InternalError,
                    )
                    .await;
            }
        };

        // Update client connection to use the reconnecting player's original id.
        let Some(reassigned_delivery) = self.connection_manager.reassign_connection(
            current_player_id,
            reconnect_player_id,
            *room_id,
        ) else {
            return self
                .reject_claimed_reconnect(
                    current_player_id,
                    claim_guard,
                    restored_membership,
                    restored_authority,
                    "Current connection no longer exists",
                    ErrorCode::ReconnectionFailed,
                )
                .await;
        };
        if let Some(effective_player_id) = &effective_player_id {
            *effective_player_id.write().await = *reconnect_player_id;
        }

        // `reassign_connection` rebuilt the connection from the transient
        // reconnect socket, whose epoch resets to 1. Restore the sender's real
        // incarnation lineage: resume at `last_epoch + 1` (the pre-disconnect
        // epoch survived in the reconnection record), so a recipient that stayed
        // connected sees the per-(sender, room) `(epoch, seq)` stream strictly
        // INCREASE across the reconnect instead of an ambiguous reset to (1, 1).
        // (Epoch is tracked server-side for EVERY sender — it bumps on each join
        // regardless of the sender's own protocol version — so `last_epoch` is
        // the sender's true pre-disconnect value, not necessarily 0 for a v2
        // sender. It is still stripped per-recipient, so a v2 recipient never
        // sees it while a v3 recipient gets a correct monotonic stamp.)
        self.connection_manager.set_game_data_epoch(
            reconnect_player_id,
            disconnected.last_epoch.saturating_add(1),
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

                        let ready_players = server
                            .room_coordinator
                            .current_ready_players(&response_room_id)
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
                        Ok(Arc::new(ServerMessage::Reconnected(Box::new(
                            ReconnectedPayload {
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
                            },
                        ))))
                    })
                }),
            )
            .await;
        match initial_delivery {
            Ok(crate::coordination::DeliveryOutcome::Delivered) => {
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
                        restored_membership,
                        restored_authority,
                        "baseline_delivery",
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
                        restored_membership,
                        restored_authority,
                        "coordinator_registration",
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
            let fallback_guard = self
                .message_coordinator
                .lock_room_event_mutation(room_id)
                .await;
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
                        let expected_members: Vec<_> = resolved
                            .decision
                            .members
                            .iter()
                            .map(|member| member.player_id)
                            .collect();
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
            }
        }

        self.metrics.increment_players_joined();
        tracing::info!(
            %reconnect_player_id,
            %room_id,
            room_code = %room.code,
            "Player reconnected successfully"
        );
        true
    }
}
