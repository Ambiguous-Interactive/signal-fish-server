use crate::protocol::{
    ErrorCode, PlayerId, PlayerInfo, ReconnectedPayload, ReplayStatus, RoomId, SenderWatermark,
    ServerMessage,
};
use crate::reconnection::{ClaimedReconnection, DisconnectedPlayer, ReconnectionManager};
use std::sync::Arc;

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
    /// Record a room-uniform broadcast for reconnection replay.
    ///
    /// The one-line hook every uniform broadcast site calls right BEFORE (or
    /// beside) delivery — the event is recorded even if the broadcast
    /// partially fails, matching "what a connected player would have been
    /// sent". No-ops when reconnection is disabled; non-replayable messages
    /// and rooms with nobody pending are filtered cheaply inside
    /// [`ReconnectionManager::record_room_event`].
    pub(crate) async fn record_replayable_room_event(
        &self,
        room_id: &RoomId,
        message: &ServerMessage,
    ) {
        let Some(reconnection_manager) = &self.reconnection_manager else {
            return;
        };
        reconnection_manager
            .record_room_event(room_id, message)
            .await;
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

    pub(crate) async fn register_disconnection_for_reconnect(
        &self,
        player_id: &PlayerId,
        room_id: RoomId,
        was_authority: bool,
    ) {
        let Some(reconnection_manager) = &self.reconnection_manager else {
            return;
        };

        let player_info = match self.database.get_room_by_id(&room_id).await {
            Ok(Some(room)) => room.players.get(player_id).cloned(),
            Ok(None) => None,
            Err(err) => {
                tracing::warn!(
                    %player_id,
                    %room_id,
                    error = %err,
                    "Failed to snapshot player info for reconnection"
                );
                None
            }
        };

        // Capture the connection's current game-data incarnation epoch WHILE it
        // is still registered (unregister removes it right after this call), so
        // the reconnect can resume at `last_epoch + 1` and keep the recipient's
        // (epoch, seq) view strictly increasing (v3 reliability surface).
        // `game_data_epoch` returns `None` only if the connection already
        // vanished (a disconnect race) — never merely because the client is v2;
        // that case falls back to `0` here (equivalent to "never stamped", so
        // the reconnect just resumes at epoch 1).
        let last_epoch = self
            .connection_manager
            .game_data_epoch(player_id)
            .unwrap_or(0);

        let token = reconnection_manager
            .register_disconnection(*player_id, room_id, was_authority, player_info, last_epoch)
            .await;

        tracing::info!(
            %player_id,
            %room_id,
            %was_authority,
            reconnection_token = %token[..8].to_string(),
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
        &self,
        current_player_id: &PlayerId,
        reconnect_player_id: &PlayerId,
        room_id: &RoomId,
        auth_token: &str,
    ) -> bool {
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

        // Get missed events (fetched before the claim completes below — the
        // completion may release the room's replay ring when this player is
        // the last one pending).
        let mut missed_events = reconnection_manager
            .get_missed_events(room_id, disconnected.last_sequence)
            .await;
        // Never replay the reconnecting player's OWN membership deltas back to
        // it: on reconnect it is being RESTORED (its presence is in the
        // `Reconnected` snapshot and peers get `PlayerReconnected`), so a
        // buffered `PlayerLeft`/`PlayerJoined`/`PlayerReconnected` for THIS
        // player is a self-referential teardown artifact, not room news it
        // missed. (This also keeps the replay stable regardless of how many
        // times the player's disconnect was registered before it reconnected.)
        missed_events.events.retain(|event| match event {
            ServerMessage::PlayerLeft { player_id }
            | ServerMessage::PlayerReconnected { player_id, .. } => {
                player_id != reconnect_player_id
            }
            ServerMessage::PlayerJoined { player } => player.id != *reconnect_player_id,
            _ => true,
        });

        // v3 wire gate for REPLAYED snapshots. The replay ring stores
        // `PlayerJoined` / `PlayerReconnected` in their live-broadcast form —
        // carrying the v3 incarnation `epoch` — but `missed_events` is embedded
        // in the `Reconnected` payload and so BYPASSES the per-recipient strip in
        // `websocket::sending` (which only rewrites the top-level frame). Strip
        // `epoch` for a pre-v3 (v2) reconnector so its `Reconnected` stays
        // byte-identical to the frozen v2 wire. The reconnecting socket's
        // negotiated version lives on `current_player_id` here (the reassignment
        // to `reconnect_player_id` happens below), and the protocol survives it.
        if self.client_protocol(current_player_id).version < 3 {
            for event in &mut missed_events.events {
                match event {
                    ServerMessage::PlayerJoined { player } => player.epoch = None,
                    ServerMessage::PlayerReconnected { epoch, .. } => *epoch = None,
                    _ => {}
                }
            }
        }

        if !room.players.contains_key(reconnect_player_id) {
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

        if disconnected.was_authority && room.supports_authority && room.authority_player.is_none()
        {
            match self
                .database
                .update_room_authority(room_id, Some(*reconnect_player_id))
                .await
            {
                Ok(true) => {
                    restored_authority = true;
                }
                Ok(false) => {}
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

        // Prepare room state. The live ready set is held by the coordinator (the
        // room record only syncs at finalize), so read it from there to report an
        // accurate ready set to a reconnector rejoining an in-progress lobby, and
        // reflect it on each player's `is_ready`.
        let ready_players = self.room_coordinator.current_ready_players(room_id).await;
        let mut current_players: Vec<PlayerInfo> = room.players.values().cloned().collect();
        // v3 room snapshot: give a v3 reconnector each member's current
        // incarnation epoch (including its own freshly bumped epoch) so it can
        // re-baseline every per-sender (epoch, seq) stream. Single recipient, so
        // gate on its version at construction — a pre-v3 (v2) reconnector keeps
        // every epoch `None` and byte-identical v2 bytes.
        let recipient_is_v3 = self.connection_manager.supports_v3(reconnect_player_id);
        for player in current_players.iter_mut() {
            player.is_ready = ready_players.contains(&player.id);
            player.epoch = if recipient_is_v3 {
                self.connection_manager.game_data_epoch(&player.id)
            } else {
                None
            };
        }
        let is_authority = room.authority_player == Some(*reconnect_player_id);

        // Replay completeness (v3+ recipients only; absent on the v2 wire,
        // mirroring the per-recipient `ice_servers` gate below). The connection
        // was reassigned above, so the reconnecting socket's negotiated
        // protocol is queryable under the restored player id. `Unavailable`
        // (ring disabled) wins over `Truncated`: a zero-capacity ring evicts
        // everything, but the honest contract is "replay is off, resync".
        let replay = if self.client_protocol(reconnect_player_id).version >= 3 {
            Some(if reconnection_manager.event_buffer_size() == 0 {
                ReplayStatus::Unavailable
            } else if missed_events.truncated {
                ReplayStatus::Truncated
            } else {
                ReplayStatus::Complete
            })
        } else {
            None
        };

        let reconnection_token = self
            .pre_issue_reconnection_token_for(reconnect_player_id, *room_id)
            .await;
        let ice_servers = self.pregather_ice_servers(&room, reconnect_player_id);
        let missed_events = missed_events.events;
        let response_room_id = *room_id;
        let response_player_id = *reconnect_player_id;
        let room_code = room.code.clone();
        let game_name = room.game_name.clone();
        let max_players = room.max_players;
        let supports_authority = room.supports_authority;
        let lobby_state = room.lobby_state.clone();
        let response_ready_players = ready_players.clone();
        let relay_type = room.relay_type.clone();
        let current_spectators = room.get_spectators();
        let response_players = current_players.clone();

        // Queue `Reconnected` before putting the restored connection back into
        // room routing. The coordinator holds the room-routing write lock while
        // this closure runs. Game-data broadcasts allocate their relay stamp in
        // the same coordinator read section that snapshots recipients, so every
        // earlier stamp is reflected in the watermarks and cannot route to this
        // socket; later broadcasts cannot route to it until after the baseline
        // frame is queued.
        let initial_delivery = self
            .message_coordinator
            .register_local_client_with_initial_message(
                *reconnect_player_id,
                *room_id,
                reassigned_delivery,
                Box::new(move || {
                    let sender_watermarks = if recipient_is_v3 {
                        response_players
                            .iter()
                            .filter_map(|player| {
                                self.connection_manager.current_relay_stamp(&player.id).map(
                                    |stamp| SenderWatermark {
                                        player_id: player.id,
                                        epoch: stamp.epoch,
                                        seq: stamp.seq,
                                    },
                                )
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    Arc::new(ServerMessage::Reconnected(Box::new(ReconnectedPayload {
                        room_id: response_room_id,
                        room_code,
                        player_id: response_player_id,
                        game_name,
                        max_players,
                        supports_authority,
                        current_players: response_players,
                        is_authority,
                        lobby_state,
                        ready_players: response_ready_players,
                        relay_type,
                        current_spectators,
                        // v3 ICE pre-gather (deferred refinement): empty —
                        // and skipped on the wire — unless this reconnector passes
                        // the pre-gather gate (its original credentials may have
                        // expired while it was away), so v2 bytes are untouched. A
                        // reconnect into a Finalized room gets fresh ICE from the
                        // late-join SessionPlan below instead (never both).
                        ice_servers,
                        missed_events,
                        replay,
                        sender_watermarks,
                        // Rotate: the token just used was consumed with the
                        // completed claim; the restored player gets a fresh one
                        // for its NEXT unexpected disconnect (v3+ only).
                        reconnection_token,
                    })))
                }),
            )
            .await;
        match initial_delivery {
            Ok(crate::coordination::DeliveryOutcome::Delivered) => {}
            Ok(outcome) => {
                tracing::warn!(
                    %reconnect_player_id,
                    %room_id,
                    ?outcome,
                    "Reconnection restored state but could not queue the Reconnected baseline"
                );
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

        // Notify other players. Recorded for replay BEFORE delivery (the
        // room's ring persists when other players are still pending): a
        // reconnector must learn this player came back exactly like a
        // connected member would have.
        // v3 wire snapshot: carry the reconnector's new incarnation epoch (Some
        // after the `reassign_connection` above bumped it) — the same value now
        // stamped on its relayed GameData, so recipients re-baseline the
        // per-sender (epoch, seq) stream immediately. Stripped per-recipient for
        // pre-v3 members in `websocket::sending`.
        let notification = Arc::new(ServerMessage::PlayerReconnected {
            player_id: *reconnect_player_id,
            epoch: self.connection_manager.game_data_epoch(reconnect_player_id),
        });
        self.record_replayable_room_event(room_id, notification.as_ref())
            .await;

        // Concurrent fan-out (issue #136, F2): a serial per-recipient loop
        // lets one backpressured peer stall the notification for everyone
        // after it by up to the slow-consumer window EACH; concurrently the
        // whole fan-out is bounded by the single slowest recipient, like the
        // coordinator's own broadcast path.
        futures_util::future::join_all(
            room.players
                .keys()
                .filter(|other_player_id| *other_player_id != reconnect_player_id)
                .map(|other_player_id| {
                    let notification = Arc::clone(&notification);
                    async move {
                        let _ = self
                            .message_coordinator
                            .send_to_player(other_player_id, notification)
                            .await;
                    }
                }),
        )
        .await;

        // Re-entry into an active session: if the room is finalized with a
        // stored non-relay plan, the reconnector receives a fresh tailored
        // `SessionPlan` (fresh ICE — its original TURN credentials may have
        // expired) and existing members receive the `NewPeer` delta per the
        // stored topology when the transport is WebRTC. A non-finalized room
        // or the relay floor emits nothing.
        self.handle_active_session_late_join(&room, reconnect_player_id, &current_players)
            .await;

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
