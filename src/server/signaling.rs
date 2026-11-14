//! Targeted WebRTC signal relay (Protocol v3).
//!
//! Relays opaque WebRTC signals (offer/answer/trickle-ICE) to a specific peer in
//! the same room ([`EnhancedGameServer::handle_signal`]) and publishes a peer
//! joining an already-finalized room as one authoritative membership event.
//!
//! Finalized joins and reconnects consult the room's stored sticky decision,
//! repair an invalid host when necessary, and publish a fresh tailored
//! `SessionPlan` to every v3 member. The actor plan and incumbent lifecycle
//! frames are phase zero; incumbent plans are phase one. A missing sticky plan
//! produces an explicit Relay/Relay plan with no peers, making no-pairing an
//! authoritative result. Pre-finalized membership changes remain lifecycle-only.
//! `handle_signal` shares the same room gate, so no offer/answer/ICE frame can
//! overtake any recipient's plan.
//!
//! Every code path is gated on negotiated v3 (plus the WebRTC transport for
//! `Signal`) so v2 clients never observe `Signal` or `SessionPlan` (the v3 gate).

use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::coordination::{
    RoomEventMutationGuard, RoomMessageTransactionOutcome, RoomRecipientMessages,
};
use crate::protocol::{
    room_state::Room, ErrorCode, LobbyState, PlayerId, PlayerInfo, RoomId, ServerMessage,
    SessionGeneration, Transport,
};

use super::session_policy::membership_session_decision;
use super::{ClientLifecycle, EnhancedGameServer};

/// Reuse a routed roster for membership lookup without allocating a second set.
/// Custom coordinators may return unordered or duplicate player ids.
pub(super) fn sorted_routed_player_ids(mut players: Vec<PlayerId>) -> Vec<PlayerId> {
    players.sort_unstable();
    players
}

/// Canonical serialized JSON byte length of an attacker-controlled payload —
/// the measure the per-payload byte caps (`security.max_signal_bytes`,
/// `security.max_connection_info_bytes`, the relay byte budget) enforce.
///
/// Count the same serializer output without allocating a temporary payload.
/// Treat serialization or byte-count overflow as oversized.
pub(crate) fn canonical_json_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    #[derive(Default)]
    struct ByteCount(usize);

    impl std::io::Write for ByteCount {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("JSON byte count overflows"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut count = ByteCount::default();
    serde_json::to_writer(&mut count, value).map_or(usize::MAX, |()| count.0)
}

impl EnhancedGameServer {
    /// Revalidate a signal target after the room publication gate. Production
    /// coordinators provide the routed-member snapshot; lightweight
    /// coordinators fall back to the connection's current room assignment.
    pub(super) async fn signal_target_is_routed_after_gate(
        &self,
        routed_target: Option<bool>,
        target: &PlayerId,
        room_id: RoomId,
    ) -> bool {
        match routed_target {
            Some(routed) => routed,
            None => self.get_client_room(target).await == Some(room_id),
        }
    }

    /// Relay an opaque WebRTC signal from `from` to `to`, enforcing the P2
    /// security invariants (payload size cap, same room, negotiated WebRTC,
    /// rate limit, v3 target).
    #[cfg(test)]
    pub async fn handle_signal(&self, from: &PlayerId, to: PlayerId, signal: serde_json::Value) {
        self.handle_signal_in_generation(from, to, SessionGeneration::nil(), signal)
            .await;
    }

    /// Relay one generation-fenced opaque WebRTC signal.
    /// Once admitted, delivery continues if the caller is canceled.
    ///
    /// The server deliberately does not interpret or topology-gate the
    /// generation: it preserves the same room-ordered dumb-plumbing contract
    /// as the opaque signal body. Recipients compare it with their latest
    /// authoritative `SessionPlan`, which safely drops both delayed traffic
    /// and frames from endpoints that have not applied the same plan yet.
    pub async fn handle_signal_in_generation(
        &self,
        from: &PlayerId,
        to: PlayerId,
        generation: SessionGeneration,
        signal: serde_json::Value,
    ) {
        self.handle_signal_in_generation_from_lifecycle(from, to, generation, signal, None)
            .await;
    }

    pub(super) async fn handle_signal_in_generation_from_lifecycle(
        &self,
        from: &PlayerId,
        to: PlayerId,
        generation: SessionGeneration,
        signal: serde_json::Value,
        source_lifecycle: Option<Arc<ClientLifecycle>>,
    ) {
        let Some(lifecycle) =
            source_lifecycle.or_else(|| self.connection_manager.client_lifecycle(from))
        else {
            return;
        };
        let lifecycle_guard = Arc::clone(&lifecycle).lock_owned().await;
        if lifecycle.player_id() != *from
            || !self.connection_manager.lifecycle_matches(from, &lifecycle)
        {
            return;
        }

        // 0. Payload size cap. Checked first because the cap
        //    is a property of the frame itself, independent of room/transport
        //    state, and rejecting before any lookup keeps oversized payloads
        //    maximally cheap. Size is the canonical serialized JSON byte
        //    length of the opaque `signal` value — the same bytes the relay
        //    would otherwise fan out.
        let payload_bytes = super::game_data::game_data_json_len(&signal);
        let max_signal_bytes = self.config.max_signal_bytes;
        if payload_bytes > max_signal_bytes {
            self.reject_signal(
                from,
                to,
                format!(
                    "Signal payload is {payload_bytes} bytes; \
                     the maximum allowed is {max_signal_bytes} bytes"
                ),
                ErrorCode::SignalTooLarge,
                "payload_too_large",
                lifecycle_guard,
            )
            .await;
            return;
        }

        // 1. Sender must be in a room.
        let Some(from_room) = self.get_client_room(from).await else {
            self.reject_signal(
                from,
                to,
                "You are not in a room",
                ErrorCode::NotInRoom,
                "sender_not_in_room",
                lifecycle_guard,
            )
            .await;
            return;
        };

        // Self-signal guard: a peer cannot WebRTC to itself. Reject before
        // target lookup so the diagnostic is deterministic.
        if *from == to {
            self.reject_signal(
                from,
                to,
                "Cannot signal yourself",
                ErrorCode::SignalTargetNotFound,
                "self_signal",
                lifecycle_guard,
            )
            .await;
            return;
        }

        // 2. Target must be in a room.
        let Some(to_room) = self.get_client_room(&to).await else {
            self.reject_signal(
                from,
                to,
                "Signal target is not in any room",
                ErrorCode::SignalTargetNotFound,
                "target_not_in_room",
                lifecycle_guard,
            )
            .await;
            return;
        };

        // 3. Same-room enforcement: signaling never crosses a room boundary.
        if from_room != to_room {
            self.reject_signal(
                from,
                to,
                "Cannot signal a peer in a different room",
                ErrorCode::CrossRoomSignal,
                "cross_room",
                lifecycle_guard,
            )
            .await;
            return;
        }

        // 4. Sender must have negotiated v3 + WebRTC. Deliberately
        //    transport-only (see the step-5 note below).
        if !self.supports_webrtc_signaling(from) {
            self.reject_signal(
                from,
                to,
                "WebRTC transport was not negotiated for this connection",
                ErrorCode::UnsupportedTransport,
                "sender_unsupported_transport",
                lifecycle_guard,
            )
            .await;
            return;
        }

        // 5. Deliver only to a v3 + WebRTC target. A webrtc plan can never have
        //    chosen a peer that lacks the WebRTC transport, but enforce
        //    defense-in-depth: a sender that targets a v2 or v3-relay-only peer
        //    is told the target was not found rather than delivering a `Signal`
        //    the target never opted into (the target's negotiated WebRTC
        //    capability gate). Both `handle_signal` gates (sender + target) are
        //    deliberately TRANSPORT-only, weaker
        //    than the full session predicate that gates `NewPeer` / plan peer
        //    lists: `Signal` relay is dumb plumbing between endpoints that both
        //    negotiated the transport, and it must not second-guess (or
        //    topology-gate) which pairs the session plan brokered.
        if !self.supports_webrtc_signaling(&to) {
            self.reject_signal(
                from,
                to,
                "Signal target does not support WebRTC signaling",
                ErrorCode::SignalTargetNotFound,
                "target_unsupported_transport",
                lifecycle_guard,
            )
            .await;
            return;
        }
        // Defensive duplicate of gate 2: this re-reads the same
        // connection-manager assignment those gates just read, with no
        // suspension point between, so diverging here requires a true
        // cross-thread parallel mutation (microsecond TOCTOU). Deliberately
        // untested — the interleaving is not deterministically constructible;
        // the under-gate revalidation below is the authoritative re-check.
        let Some(target_epoch) = self
            .connection_manager
            .current_relay_stamp_in_room(&to, &from_room)
            .map(|stamp| stamp.epoch)
        else {
            self.reject_signal(
                from,
                to,
                "Signal target is not currently routed in the room",
                ErrorCode::SignalTargetNotFound,
                "target_not_routed",
                lifecycle_guard,
            )
            .await;
            return;
        };

        // SessionPlan publication is room-ordered, while socket writers run on
        // independent tasks. Without sharing this gate, a peer whose plan was
        // queued first could immediately offer to a later recipient and place
        // `Signal` ahead of that recipient's plan. Holding the shared room
        // fence through dispatch makes plan-before-signal a server guarantee for
        // finalize, re-plan, join, and reconnect publications alike.
        let signal_publication_guard = self.message_coordinator.lock_room_signal(&from_room).await;

        // Either endpoint can be pruned as slow/closed while this signal waits
        // behind a room publication, before its connection assignment is
        // asynchronously cleared. Revalidate coordinator routing under the gate
        // so neither a removed sender nor a replacement target can act on the
        // stale pre-wait snapshot.
        let routed_pair = match self
            .message_coordinator
            .routed_player_pair(&from_room, from, &to)
            .await
        {
            Ok(pair) => pair,
            Err(error) => {
                tracing::warn!(%from_room, %from, %to, %error, "Failed to revalidate signal routing");
                drop(signal_publication_guard);
                self.send_signal_error(
                    from,
                    "Failed to check signal routing; retry the signal",
                    ErrorCode::InternalError,
                    lifecycle_guard,
                )
                .await;
                return;
            }
        };
        let sender_is_routed = routed_pair.is_none_or(|[sender, _]| sender);
        if !sender_is_routed {
            drop(signal_publication_guard);
            self.reject_signal(
                from,
                to,
                "Signal sender is no longer routed in the room",
                ErrorCode::NotInRoom,
                "sender_routing_changed",
                lifecycle_guard,
            )
            .await;
            return;
        }
        let target_is_routed = self
            .signal_target_is_routed_after_gate(
                routed_pair.map(|[_, target]| target),
                &to,
                from_room,
            )
            .await;
        let target_incarnation_matches = self
            .connection_manager
            .current_relay_stamp_in_room(&to, &from_room)
            .is_some_and(|stamp| stamp.epoch == target_epoch);
        if !target_is_routed || !target_incarnation_matches || !self.supports_webrtc_signaling(&to)
        {
            drop(signal_publication_guard);
            self.reject_signal(
                from,
                to,
                "Signal target changed while session routing was being published",
                ErrorCode::SignalTargetNotFound,
                "target_routing_changed",
                lifecycle_guard,
            )
            .await;
            return;
        }

        // 6. Per-connection valid signal rate limit.
        if let Err(err) = self.rate_limiter.check_signal(from).await {
            drop(signal_publication_guard);
            self.send_signal_error(
                from,
                err.to_string(),
                ErrorCode::SignalRateLimited,
                lifecycle_guard,
            )
            .await;
            return;
        }

        // Count the signal as relayed here, at dispatch — after every same-room +
        // transport + rate-limit check has passed — matching the best-effort
        // "relayed = dispatched" semantics of the send below (a rejected
        // cross-room / rate-limited signal returns earlier and is never counted).
        self.metrics.increment_signals_relayed();

        // Delivery follows the server-wide contract (`deliver_or_disconnect`):
        // never a silent drop — a backpressured recipient stalls this send for
        // up to the slow-consumer grace window and is then loudly disconnected
        // (metrics + close), abandoning its queue only with the connection.
        let coordinator = Arc::clone(&self.message_coordinator);
        let from = *from;
        let completion = tokio::spawn(async move {
            let _signal_publication_guard = signal_publication_guard;
            let _lifecycle_guard = lifecycle_guard;
            coordinator
                .send_to_player_in_room(
                    &to,
                    &from_room,
                    Arc::new(ServerMessage::Signal {
                        from,
                        generation,
                        signal,
                    }),
                )
                .await
        });
        if let Err(error) = completion.await.unwrap_or_else(|error| Err(error.into())) {
            tracing::warn!(%from, %to, %error, "Signal delivery transaction failed");
        }
    }

    pub(crate) async fn publish_join_lifecycle_fallback(
        &self,
        room_id: RoomId,
        joiner: PlayerId,
        player_joined: Arc<ServerMessage>,
    ) -> anyhow::Result<bool> {
        let replay_committed = Arc::new(AtomicBool::new(false));
        let replay_committed_in_hook = Arc::clone(&replay_committed);
        let replay_message = Arc::clone(&player_joined);
        let reconnection_manager = self.reconnection_manager.clone();
        let (_drain_tx, drain) = tokio::sync::watch::channel(false);
        let should_publish = || true;
        let _ = self
            .message_coordinator
            .broadcast_to_room_except_if_with_hook(
                &room_id,
                &joiner,
                player_joined,
                &should_publish,
                drain,
                Box::new(move || {
                    Box::pin(async move {
                        if let Some(reconnection_manager) = reconnection_manager {
                            reconnection_manager
                                .record_room_event(&room_id, replay_message.as_ref())
                                .await;
                        }
                        replay_committed_in_hook.store(true, Ordering::Release);
                    })
                }),
            )
            .await?;
        Ok(replay_committed.load(Ordering::Acquire))
    }

    /// Fail a room publication closed without terminating a replacement
    /// connection that reused the same player id.
    ///
    /// The terminal socket teardown runs after the owned room job releases its
    /// mutation guard. Incumbents may briefly observe the opening lifecycle
    /// event, but the ensuing `PlayerLeft` clears their authoritative-plan
    /// obligation; the affected client cannot remain live waiting for a plan
    /// that the server failed to publish.
    ///
    /// Callers must retain the actor's lifecycle lock until this method
    /// returns. The generation checks are defensive; the held lifecycle lock
    /// is what makes snapshot registration and close initiation one logical
    /// transition with respect to connection replacement.
    pub(crate) async fn terminate_room_generation_after_publication_failure(
        self: &Arc<Self>,
        player_id: PlayerId,
        room_id: RoomId,
        expected_epoch: u32,
        player_info: PlayerInfo,
        was_authority: bool,
        publication: &'static str,
    ) -> bool {
        let generation_is_current = self
            .connection_manager
            .current_relay_stamp_in_room(&player_id, &room_id)
            .is_some_and(|stamp| stamp.epoch == expected_epoch);
        if !generation_is_current {
            return false;
        }

        let Some(lifecycle) = self.connection_manager.client_lifecycle(&player_id) else {
            return false;
        };
        if !self
            .connection_manager
            .lifecycle_matches(&player_id, &lifecycle)
        {
            return false;
        }

        let initiated = self
            .connection_manager
            .request_close_for(&player_id, crate::coordination::CloseReason::Unregistered);
        tracing::error!(
            %player_id,
            %room_id,
            expected_epoch,
            publication,
            initiated,
            "Authoritative room publication failed; closing the affected connection"
        );

        // Never await teardown while this job owns the room mutation gate.
        // The captured lifecycle fences the task against a replacement
        // connection; teardown resumes after the handler releases its
        // lifecycle lock and this job releases the room gate.
        let server = Arc::clone(self);
        tokio::spawn(async move {
            server.unregister_client_with_lifecycle(lifecycle).await;
        });
        // Preserve a complete reconnect/expiry-cleanup record before the
        // socket teardown can race a failing database read. The normal
        // unregister path may register the same room again; that operation is
        // idempotent and retains this complete snapshot.
        if AssertUnwindSafe(self.register_disconnection_for_reconnect(
            &player_id,
            room_id,
            was_authority,
            player_info,
        ))
        .catch_unwind()
        .await
        .is_err()
        {
            tracing::error!(%player_id, %room_id, publication, "Failed to preserve reconnect snapshot after publication failure");
        }

        true
    }

    /// Publish a finalized-room join as one exact-membership, two-phase event.
    ///
    /// The joiner's `RoomJoined` baseline is already queued while the caller
    /// owns `room_event_guard`. Phase zero queues the joiner's authoritative
    /// plan and every incumbent's `PlayerJoined`; phase one then queues every
    /// incumbent's authoritative plan. Consequently no incumbent can signal the
    /// joiner before the joiner's plan is queued, and no client can satisfy a
    /// membership-count gate in the gap between lifecycle and pairing state.
    ///
    /// Reservation failures happen before the replay/state hook. A slow or
    /// closed incumbent is removed and the decision is rebuilt over the new
    /// routed set. If the joiner itself disappears, the lifecycle boundary is
    /// still replayed and broadcast tolerantly to incumbents so its later
    /// terminal epoch cannot appear without a matching opening event.
    pub(crate) async fn publish_finalized_join_membership(
        self: &Arc<Self>,
        room: Arc<Room>,
        joiner: PlayerId,
        joiner_snapshot: PlayerInfo,
        room_event_guard: RoomEventMutationGuard,
    ) -> bool {
        let room_id = room.id;
        let baseline_room = room;
        let expected_join_epoch = joiner_snapshot.epoch.or_else(|| {
            self.connection_manager
                .current_relay_stamp_in_room(&joiner, &room_id)
                .map(|stamp| stamp.epoch)
        });
        let player_joined = Arc::new(ServerMessage::PlayerJoined {
            player: joiner_snapshot,
        });
        let joiner_was_authority = baseline_room.authority_player == Some(joiner);
        let server = Arc::clone(self);
        let coordinator = Arc::clone(&self.message_coordinator);
        let completion = self.message_coordinator.enqueue_room_event(
            room_event_guard,
            Box::new(move || {
                Box::pin(async move {
                    let ServerMessage::PlayerJoined { player: joiner_snapshot } = player_joined.as_ref() else {
                        return Err(anyhow::anyhow!("join publication requires a player snapshot"));
                    };
                    let mut attempts_remaining = baseline_room.players.len().saturating_add(1);
                    // Mixed-path membership is a fact about the room snapshot
                    // this publication resolved, not about its delivery
                    // outcome: observe it exactly once per event, not once per
                    // reservation retry.
                    let mut mixed_path_observed = false;
                    loop {
                        let include_direct = server.active_session_plan(&room_id)
                            .is_some_and(|plan| plan.transport == Transport::Direct);
                        let refreshed_room = match server.database
                            .get_room_session_selection_snapshot_for_transport(&room_id, include_direct).await {
                            Ok(Some(room))
                                if room.lobby_state == LobbyState::Finalized
                                    && room.players.iter().any(|player| player.id == joiner) =>
                            {
                                Some(room)
                            }
                            Ok(Some(_)) => {
                                tracing::warn!(%room_id, %joiner, "Joined-room refresh no longer matched the already-delivered finalized baseline; using that baseline for publication");
                                None
                            }
                            Ok(None) => {
                                tracing::warn!(%room_id, %joiner, "Joined room disappeared during finalized publication refresh; using the already-delivered baseline");
                                None
                            }
                            Err(error) => {
                                tracing::warn!(%room_id, %joiner, %error, "Failed to refresh joined room before finalized publication; using the already-delivered baseline");
                                None
                            }
                        };
                        let routed = match server
                            .message_coordinator
                            .routed_player_ids(&room_id)
                            .await
                        {
                            Ok(players) => {
                                players.map(sorted_routed_player_ids)
                            }
                            Err(error) => {
                                tracing::warn!(%room_id, %joiner, %error, "Failed to refresh joined-room routing; deriving the local route from generation-fenced connections");
                                None
                            }
                        };
                        let is_routed = |player_id: &PlayerId| {
                            routed.as_ref().map_or_else(
                                || server.connection_manager.current_relay_stamp_in_room(player_id, &room_id).is_some(),
                                |routed| routed.binary_search(player_id).is_ok(),
                            )
                        };
                        let (authority_player, members) = match refreshed_room {
                            Some(room) => (room.authority_player, server.session_members_from_selection(
                                room.players.into_iter().filter(|player| is_routed(&player.id)),
                            )),
                            None => (baseline_room.authority_player, server.session_members_from(
                                baseline_room.players.values().filter(|player| is_routed(&player.id)),
                            )),
                        };

                        if !members.iter().any(|member| member.player_id == joiner) {
                            if let Some(expected_epoch) = expected_join_epoch {
                                server
                                    .terminate_room_generation_after_publication_failure(
                                        joiner,
                                        room_id,
                                        expected_epoch,
                                        joiner_snapshot.clone(),
                                        joiner_was_authority,
                                        "finalized_join_unrouted",
                                    )
                                    .await;
                            }
                            return server
                                .publish_join_lifecycle_fallback(
                                    room_id,
                                    joiner,
                                    Arc::clone(&player_joined),
                                )
                                .await;
                        }

                        let resolved = membership_session_decision(
                            server.active_session_plan(&room_id),
                            authority_player,
                            members,
                        );
                        if !mixed_path_observed {
                            server.observe_mixed_path_members(&room_id, &resolved.decision);
                            mixed_path_observed = true;
                        }
                        // Wall clock (durable record): TURN credentials carry
                        // absolute expiry instants the client sees.
                        let now_unix = resolved
                            .decision
                            .uses_webrtc_signaling()
                            .then(|| chrono::Utc::now().timestamp());
                        let expected_members: Vec<PlayerId> = resolved
                            .decision
                            .members
                            .iter()
                            .map(|member| member.player_id)
                            .collect();
                        let projection = resolved.decision.projection();
                        let mut turn_credentials_issued = 0_u64;
                        let mut joiner_has_plan = false;
                        let recipient_messages: Vec<RoomRecipientMessages> = resolved
                            .decision
                            .members
                            .iter()
                            .map(|member| {
                                let plan = server
                                    .build_session_plan_message_for_member(
                                        &projection,
                                        member,
                                        now_unix,
                                    )
                                    .map(|(message, minted)| {
                                        turn_credentials_issued =
                                            turn_credentials_issued.saturating_add(minted);
                                        message
                                    });
                                if member.player_id == joiner {
                                    joiner_has_plan = plan.is_some();
                                    RoomRecipientMessages::in_order(
                                        member.player_id,
                                        plan.into_iter().collect(),
                                    )
                                } else {
                                    RoomRecipientMessages::with_followup(member.player_id, Arc::clone(&player_joined), plan)
                                }
                            })
                            .collect();

                        let active_plan_update = resolved.active_plan_update;
                        let is_replan = resolved.is_replan;
                        let active_session_plans = Arc::clone(&server.active_session_plans);
                        let metrics = Arc::clone(&server.metrics);
                        let session_records = server.session_records();
                        let reconnection_manager = server.reconnection_manager.clone();
                        let replay_message = Arc::clone(&player_joined);

                        if expected_members.is_empty() {
                            if let Some(update) = active_plan_update {
                                if let Some(plan) = update {
                                    active_session_plans.insert(room_id, plan);
                                } else {
                                    active_session_plans.remove(&room_id);
                                }
                            }
                            if is_replan {
                                metrics.increment_session_replans_emitted();
                            }
                            if let Some(reconnection_manager) = reconnection_manager {
                                reconnection_manager
                                    .record_room_event(&room_id, replay_message.as_ref())
                                    .await;
                            }
                            return Ok(true);
                        }

                        let outcome = match coordinator
                            .commit_room_messages_if_members_with_hook(
                                &room_id,
                                &expected_members,
                                recipient_messages,
                                vec![Arc::clone(&player_joined)],
                                Box::new(move || {
                                    Box::pin(async move {
                                        if let Some(update) = active_plan_update {
                                            if let Some(plan) = update {
                                                active_session_plans.insert(room_id, plan);
                                            } else {
                                                active_session_plans.remove(&room_id);
                                            }
                                        }
                                        if is_replan {
                                            metrics.increment_session_replans_emitted();
                                        }
                                        if joiner_has_plan && !is_replan {
                                            metrics.increment_session_plans_late_join();
                                        }
                                        // Total issuance, not actor issuance:
                                        // incumbents are re-issued credentials
                                        // even when the joiner gets no plan (v2)
                                        // or the join forces a re-plan, and the
                                        // reconnect and host-replan paths count
                                        // theirs unconditionally too.
                                        metrics.add_turn_credentials_issued(turn_credentials_issued);
                                        // Per-session attribution (issue
                                        // #766), mirroring the server-wide
                                        // counter at the same commit hook.
                                        if let Some(records) = session_records.as_deref() {
                                            records.record_turn_credentials(
                                                &room_id,
                                                turn_credentials_issued,
                                            );
                                        }
                                        if let Some(reconnection_manager) = reconnection_manager {
                                            reconnection_manager
                                                .record_room_event(
                                                    &room_id,
                                                    replay_message.as_ref(),
                                                )
                                                .await;
                                        }
                                        Ok(true)
                                    })
                                }),
                                // A failed actor plan must not suppress healthy
                                // incumbents' definitive phase-one plans.
                                Box::new(|_| true),
                            )
                            .await
                        {
                            Ok(outcome) => outcome,
                            Err(error) => {
                                tracing::error!(%room_id, %joiner, %error, "Finalized join transaction failed");
                                if let Some(expected_epoch) = expected_join_epoch {
                                    server
                                        .terminate_room_generation_after_publication_failure(
                                            joiner,
                                            room_id,
                                            expected_epoch,
                                            joiner_snapshot.clone(),
                                            joiner_was_authority,
                                            "finalized_join",
                                        )
                                        .await;
                                }
                                return server
                                    .publish_join_lifecycle_fallback(
                                        room_id,
                                        joiner,
                                        Arc::clone(&player_joined),
                                    )
                                    .await;
                            }
                        };
                        match outcome {
                            RoomMessageTransactionOutcome::Committed => return Ok(true),
                            RoomMessageTransactionOutcome::CommittedDegraded { failed_frames } => {
                                tracing::debug!(
                                    %room_id,
                                    failed_frames,
                                    "Finalized join committed with degraded frame delivery"
                                );
                                return Ok(true);
                            }
                            RoomMessageTransactionOutcome::RoutingChanged => {
                                attempts_remaining = attempts_remaining.saturating_sub(1);
                                if attempts_remaining == 0 {
                                    if let Some(expected_epoch) = expected_join_epoch {
                                        server
                                            .terminate_room_generation_after_publication_failure(
                                                joiner,
                                                room_id,
                                                expected_epoch,
                                                joiner_snapshot.clone(),
                                                joiner_was_authority,
                                                "finalized_join_routing",
                                            )
                                            .await;
                                    }
                                    return server
                                        .publish_join_lifecycle_fallback(
                                            room_id,
                                            joiner,
                                            Arc::clone(&player_joined),
                                        )
                                        .await;
                                }
                                tokio::task::yield_now().await;
                            }
                            RoomMessageTransactionOutcome::HookRejected => {
                                if let Some(expected_epoch) = expected_join_epoch {
                                    server
                                        .terminate_room_generation_after_publication_failure(
                                            joiner,
                                            room_id,
                                            expected_epoch,
                                            joiner_snapshot.clone(),
                                            joiner_was_authority,
                                            "finalized_join_hook",
                                        )
                                        .await;
                                }
                                return server
                                    .publish_join_lifecycle_fallback(
                                        room_id,
                                        joiner,
                                        Arc::clone(&player_joined),
                                    )
                                    .await;
                            }
                        }
                    }
                })
            }),
        );

        match completion.await {
            Ok(committed) => committed,
            Err(error) => {
                tracing::warn!(%room_id, %error, "Finalized join publication failed");
                false
            }
        }
    }

    /// Whether this connection can participate in targeted WebRTC signaling
    /// (negotiated v3 + the WebRTC transport).
    ///
    /// This is `handle_signal`'s transport-level v3 capability gate,
    /// deliberately weaker than the full sticky topology/transport predicate
    /// that shapes plan peer lists: the relay forwards signals between any two
    /// transport-capable endpoints without reinterpreting the plan.
    pub(crate) fn supports_webrtc_signaling(&self, player_id: &PlayerId) -> bool {
        self.connection_manager
            .project_protocol(player_id, |protocol| {
                protocol.version >= 3 && protocol.transports.contains(&Transport::WebRtc)
            })
            .unwrap_or(false)
    }

    async fn reject_signal(
        &self,
        from: &PlayerId,
        to: PlayerId,
        message: impl Into<String>,
        error_code: ErrorCode,
        reason: &'static str,
        lifecycle_guard: tokio::sync::OwnedMutexGuard<()>,
    ) {
        tracing::debug!(
            %from,
            %to,
            %reason,
            ?error_code,
            "Rejected WebRTC signal"
        );

        match self.rate_limiter.check_signal_error(from).await {
            Ok(()) => {
                self.send_signal_error(from, message, error_code, lifecycle_guard)
                    .await
            }
            Err(err) => {
                self.send_signal_error(
                    from,
                    err.to_string(),
                    ErrorCode::SignalRateLimited,
                    lifecycle_guard,
                )
                .await;
            }
        }
    }

    /// Retain a refused signal's reply and source order after observer cancellation.
    async fn send_signal_error(
        &self,
        player_id: &PlayerId,
        message: impl Into<String>,
        error_code: ErrorCode,
        lifecycle_guard: tokio::sync::OwnedMutexGuard<()>,
    ) {
        let connection_manager = Arc::clone(&self.connection_manager);
        let message_coordinator = Arc::clone(&self.message_coordinator);
        let player_id = *player_id;
        let message = message.into();
        let completion = tokio::spawn(async move {
            let _lifecycle_guard = lifecycle_guard;
            super::messaging::send_error_to_player(
                connection_manager.as_ref(),
                message_coordinator.as_ref(),
                &player_id,
                message,
                Some(error_code),
            )
            .await
        });
        if let Err(error) = completion.await {
            tracing::error!(%player_id, %error, "Signal refusal transaction failed");
        }
    }
}

#[cfg(test)]
mod json_length_tests {
    use super::canonical_json_len;
    use crate::protocol::ConnectionInfo;
    use serde::Serialize;
    use serde_json::json;

    #[test]
    fn canonical_json_length_preserves_payload_and_metadata_bytes() {
        let mut values = vec![
            serde_json::Value::Null,
            json!([true, false, u64::MAX, i64::MIN, -0.0, 1.25e100]),
            json!(""),
            json!({"escaped\"key": [null, "quotes\", slash\\, newline\n, 雪 😀"]}),
            json!({"control": (0_u8..32).map(char::from).collect::<String>()}),
        ];
        for size in [96, 1_024, 16_384, 65_536] {
            values.push(json!({"payload": "a".repeat(size), "nested": [{"seq": 42}]}));
        }
        for (index, value) in values.into_iter().enumerate() {
            assert_eq!(
                canonical_json_len(&value),
                serde_json::to_vec(&value).expect("canonical payload").len(),
                "payload cohort {index}"
            );
            let metadata = ConnectionInfo::Custom { data: value };
            assert_eq!(
                canonical_json_len(&metadata),
                serde_json::to_vec(&metadata)
                    .expect("canonical metadata")
                    .len(),
                "metadata cohort {index}"
            );
        }
        let direct = ConnectionInfo::Direct {
            host: "host-雪.example".into(),
            port: u16::MAX,
        };
        assert_eq!(
            canonical_json_len(&direct),
            serde_json::to_vec(&direct).expect("direct metadata").len()
        );
    }

    #[test]
    fn canonical_json_length_keeps_failed_serialization_oversized() {
        let invalid_key = std::collections::BTreeMap::from([((1, 2), "invalid JSON key")]);
        assert!(serde_json::to_vec(&invalid_key).is_err());
        assert_eq!(canonical_json_len(&invalid_key), usize::MAX);

        struct PartialFailure;
        impl Serialize for PartialFailure {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                use serde::ser::SerializeSeq;
                let mut sequence = serializer.serialize_seq(Some(2))?;
                sequence.serialize_element("written before error")?;
                Err(serde::ser::Error::custom("injected serializer failure"))
            }
        }
        assert!(serde_json::to_vec(&PartialFailure).is_err());
        assert_eq!(canonical_json_len(&PartialFailure), usize::MAX);
    }
}
