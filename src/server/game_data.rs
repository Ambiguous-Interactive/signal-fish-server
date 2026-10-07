use crate::protocol::{
    DeliveryClass, ErrorCode, GameDataEncoding, PlayerId, RoomId, ServerMessage,
};
use bytes::Bytes;
use std::sync::Arc;
#[cfg(all(test, signal_fish_repository_tests))]
use std::sync::LazyLock;

use super::signaling::canonical_json_len;
use super::{ClientLifecycle, EnhancedGameServer};

/// Where a relay admission pauses for reconnect-race regressions (issue
/// #686): the sender-budget wait and the room-budget wait are the two
/// suspension points between the source check and the lifecycle-guarded
/// stamp.
#[cfg(all(test, signal_fish_repository_tests))]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum RelayAdmissionStage {
    BeforeSenderBudget,
    BeforeRoomBudget,
}

#[cfg(all(test, signal_fish_repository_tests))]
pub(super) struct RelayAdmissionPause {
    pub(super) reached: tokio::sync::Notify,
    pub(super) release: tokio::sync::Notify,
}

#[cfg(all(test, signal_fish_repository_tests))]
pub(super) fn arm_relay_admission_pause(
    player_id: PlayerId,
    stage: RelayAdmissionStage,
) -> Arc<RelayAdmissionPause> {
    let pause = Arc::new(RelayAdmissionPause {
        reached: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    RELAY_ADMISSION_PAUSES.insert((player_id, stage), Arc::clone(&pause));
    pause
}

#[cfg(all(test, signal_fish_repository_tests))]
async fn pause_relay_admission(player_id: &PlayerId, stage: RelayAdmissionStage) {
    if let Some((_, pause)) = RELAY_ADMISSION_PAUSES.remove(&(*player_id, stage)) {
        pause.reached.notify_one();
        pause.release.notified().await;
    }
}

#[cfg(all(test, signal_fish_repository_tests))]
static RELAY_ADMISSION_PAUSES: LazyLock<
    dashmap::DashMap<(PlayerId, RelayAdmissionStage), Arc<RelayAdmissionPause>>,
> = LazyLock::new(dashmap::DashMap::new);

impl EnhancedGameServer {
    fn relay_source_is_current(
        &self,
        player_id: &PlayerId,
        source_lifecycle: Option<&Arc<ClientLifecycle>>,
    ) -> bool {
        source_lifecycle
            .is_none_or(|source| self.connection_manager.lifecycle_matches(player_id, source))
    }

    /// Store legacy, self-declared peer metadata for the `GameStarting` handoff.
    pub async fn handle_provide_connection_info(
        &self,
        player_id: &PlayerId,
        connection_info: crate::protocol::ConnectionInfo,
    ) {
        self.handle_provide_connection_info_from_lifecycle(player_id, connection_info, None)
            .await;
    }

    pub(super) async fn handle_provide_connection_info_from_lifecycle(
        &self,
        player_id: &PlayerId,
        connection_info: crate::protocol::ConnectionInfo,
        source_lifecycle: Option<Arc<ClientLifecycle>>,
    ) {
        let Some(lifecycle) =
            source_lifecycle.or_else(|| self.connection_manager.client_lifecycle(player_id))
        else {
            return;
        };
        let _lifecycle_guard = lifecycle.lock().await;
        if lifecycle.player_id() != *player_id
            || !self
                .connection_manager
                .lifecycle_matches(player_id, &lifecycle)
        {
            return;
        }

        // Per-entry size cap, checked before any storage or fan-out
        // (issue #524). The entry is stored verbatim and broadcast to every
        // room member in `GameStarting.peer_connections` and room snapshots,
        // so an unbounded entry shared across a full roster can push those
        // aggregate payloads past `max_outbound_message_size` and close every
        // recipient with `OutboundMessageTooLarge` — a one-shot peer-eviction
        // primitive. The measure is the same canonical JSON bytes that are
        // broadcast, mirroring the `Signal` payload cap.
        let info_bytes = canonical_json_len(&connection_info);
        if info_bytes > self.config.max_connection_info_bytes {
            let _ = self
                .send_error_to_player(
                    player_id,
                    format!(
                        "Connection info is {} bytes; the maximum allowed is {} bytes",
                        info_bytes, self.config.max_connection_info_bytes
                    ),
                    Some(ErrorCode::MessageTooLarge),
                )
                .await;
            return;
        }

        let Some(room_id) = self.get_client_room(player_id).await else {
            let _ = self
                .send_error_to_player(
                    player_id,
                    "Not in a room".to_string(),
                    Some(ErrorCode::NotInRoom),
                )
                .await;
            return;
        };

        tracing::info!(%player_id, %room_id, "Player provided legacy peer connection metadata");

        match self
            .database
            .update_player_connection_info(&room_id, player_id, connection_info)
            .await
        {
            // Confirmed write: the caller asked for no receipt, so silence is
            // the success contract.
            Ok(true) => return,
            // A vanished membership row (`Ok(false)`; teardown raced us) and
            // a storage failure both mean peers will never see this metadata
            // at `GameStarting`. Treating either as success used to make the
            // loss indistinguishable from stored (#396 sweep): both fall
            // through to one honest client reply.
            Ok(false) => tracing::warn!(
                %player_id,
                %room_id,
                "Legacy peer metadata write landed on a vanished membership row"
            ),
            Err(e) => tracing::error!(
                %player_id,
                %room_id,
                "Failed to store legacy peer metadata: {}",
                e
            ),
        }

        let _ = self
            .send_error_to_player(
                player_id,
                "Failed to store legacy peer metadata".to_string(),
                Some(ErrorCode::InternalError),
            )
            .await;
    }

    /// Charge one game-data frame's sender-controlled payload size against
    /// the sender's per-window byte budget (`rate_limit.max_relay_bytes`,
    /// issue #519, or the sender's per-app override, issue #530) and the
    /// relaying room's aggregate per-window ceiling
    /// (`rate_limit.max_room_relay_bytes`, issue #530).
    ///
    /// Called only for a sender that is routed in a room (a roomless frame is
    /// never relayed and must keep its pinned `NOT_IN_ROOM` reply), before
    /// the fan-out is built. The sender budget is charged first so a frame
    /// its own sender cannot afford never drains its room's ceiling; a room
    /// rejection leaves the frame unrelayed with a wire error. The accepted
    /// charge is recorded for egress accounting — server-wide and, for
    /// allowlisted applications, per-app — only after both budgets admitted
    /// the frame.
    ///
    /// The caller holds the source lifecycle gate across this call (issue
    /// #686), so a rejection is returned instead of replied to: the refusal
    /// reply locks the same gate and must be sent after the caller releases
    /// it.
    async fn check_and_charge_relay_bytes(
        &self,
        player_id: &PlayerId,
        room_id: &crate::protocol::RoomId,
        encoding: crate::protocol::GameDataEncoding,
        bytes: u64,
    ) -> Result<(), crate::rate_limit::RateLimitError> {
        // Resolved once per frame; reads are lock-guarded and project only
        // Copy fields, so the relay hot path stays allocation-free.
        let app_policy = self.client_app_relay_policy(player_id);
        // Both budget waits suspend the admission; regressions pin the
        // source gate across each one (issue #686).
        #[cfg(all(test, signal_fish_repository_tests))]
        pause_relay_admission(player_id, RelayAdmissionStage::BeforeSenderBudget).await;
        self.rate_limiter
            .check_relay_bytes(player_id, bytes, app_policy)
            .await?;
        #[cfg(all(test, signal_fish_repository_tests))]
        pause_relay_admission(player_id, RelayAdmissionStage::BeforeRoomBudget).await;
        self.rate_limiter
            .check_room_relay_bytes(room_id, bytes)
            .await?;
        self.metrics.record_relay_bytes(bytes);
        if let Some(policy) = app_policy {
            self.metrics.record_app_relay_bytes(&policy.app_id, bytes);
        }
        // Per-session traffic attribution (issue #766): sender-side accepted
        // frames, bytes, and the frame's wire encoding. A DashMap read-guard
        // under the already-charged admission; unknown rooms (closed between
        // admission and attribution) attribute nothing.
        if let Some(records) = self.session_records() {
            records.record_game_data(room_id, bytes, encoding.as_wire_str());
        }
        Ok(())
    }

    /// Handle JSON game data fan-out with coordination.
    pub async fn handle_game_data(
        &self,
        player_id: &PlayerId,
        data: serde_json::Value,
        class: Option<DeliveryClass>,
        key: Option<u32>,
    ) {
        self.handle_game_data_from_lifecycle(player_id, data, class, key, None)
            .await;
    }

    pub(crate) async fn handle_game_data_from_lifecycle(
        &self,
        player_id: &PlayerId,
        data: serde_json::Value,
        class: Option<DeliveryClass>,
        key: Option<u32>,
        source_lifecycle: Option<Arc<ClientLifecycle>>,
    ) {
        if !self.relay_source_is_current(player_id, source_lifecycle.as_ref()) {
            return;
        }
        let valid = if self.client_supports_v3(player_id) {
            matches!(
                (class, key),
                (
                    None | Some(DeliveryClass::Reliable | DeliveryClass::Volatile),
                    None
                ) | (Some(DeliveryClass::Latest), Some(_))
            )
        } else {
            class.is_none() && key.is_none()
        };
        if !valid {
            let _ = self
                .send_error_to_player_for_source(
                    player_id,
                    "Invalid delivery class: latest requires a key; reliable and volatile forbid one"
                        .to_string(),
                    Some(ErrorCode::InvalidDeliveryClass),
                    source_lifecycle.as_ref(),
                )
                .await;
            return;
        }

        // Optional text-lane ceiling (issue #634): a configured
        // `security.max_game_data_bytes.json` bounds the sender-controlled
        // payload bytes (the same canonical-JSON measure the relay budgets
        // charge) before anything is charged or fanned out. Absent knob keeps
        // the frame cap as the only limit, byte-identical to the pre-#634
        // behavior. Rejected frames charge no budget and relay nothing; like
        // delivery-class rejections, the router has already recorded client
        // liveness before dispatch (a pre-existing lane shape).
        // The canonical-JSON measure is memoized: computed only when a
        // consumer needs it (the cap or the relay-byte charge), at most once
        // per frame.
        let mut payload_bytes_cache: Option<usize> = None;
        let mut payload_bytes =
            || *payload_bytes_cache.get_or_insert_with(|| canonical_json_len(&data));
        if let Some(cap) = self
            .config
            .max_game_data_bytes
            .as_ref()
            .and_then(|limits| limits.cap_for(GameDataEncoding::Json))
        {
            if payload_bytes() > cap {
                let _ = self
                    .send_error_to_player_for_source(
                        player_id,
                        format!(
                            "JSON game data is {} bytes; the maximum allowed for \
                             the json encoding is {cap} bytes",
                            payload_bytes()
                        ),
                        Some(ErrorCode::MessageTooLarge),
                        source_lifecycle.as_ref(),
                    )
                    .await;
                return;
            }
        }

        if let Some(room_id) = self.get_client_room(player_id).await {
            if !self.relay_source_is_current(player_id, source_lifecycle.as_ref()) {
                return;
            }
            // Hold the source gate across admission and stamp/enqueue (issue
            // #686): the reconnect rekey waits for this gate, so the budget
            // charge and the stamp land while the source is provably the
            // incumbent — or the re-check dismisses the stale frame with no
            // charge at all. The gate is released before the backpressured
            // fan-out completion so a reconnect never waits on queue drains.
            let source_gate = match source_lifecycle.as_ref() {
                Some(lifecycle) => Some(lifecycle.lock().await),
                None => None,
            };
            if !self.relay_source_is_current(player_id, source_lifecycle.as_ref()) {
                return;
            }
            // The sender-controlled JSON payload is the budget measure
            // (memoized above, shared with the per-encoding cap).
            if let Err(e) = self
                .check_and_charge_relay_bytes(
                    player_id,
                    &room_id,
                    crate::protocol::GameDataEncoding::Json,
                    payload_bytes() as u64,
                )
                .await
            {
                // The refusal reply locks the source gate itself; it is sent
                // only after this frame's admission released it.
                drop(source_gate);
                let _ = self
                    .send_error_to_player_for_source(
                        player_id,
                        e.to_string(),
                        Some(ErrorCode::RateLimitExceeded),
                        source_lifecycle.as_ref(),
                    )
                    .await;
                return;
            }
            let connection_manager = &self.connection_manager;
            let expected_room = room_id;
            let source_ref = &source_lifecycle;
            let mut build_message = one_shot_message_builder(move || {
                let stamp = connection_manager.next_relay_stamp_in_room_from_lifecycle(
                    player_id,
                    &expected_room,
                    source_ref.as_ref(),
                )?;
                Some(ServerMessage::GameData {
                    from_player: *player_id,
                    data,
                    seq: Some(stamp.seq),
                    epoch: Some(stamp.epoch),
                    class,
                    key,
                })
            });
            self.broadcast_game_data_with(player_id, &room_id, source_gate, &mut build_message)
                .await;
        } else {
            // Every sibling surface (ProvideConnectionInfo, Signal,
            // Authority) rejects unseated senders with NOT_IN_ROOM; leaving
            // these lanes silent made "relayed" indistinguishable from
            // "dropped" during teardown races (#396 sweep).
            let _ = self
                .send_error_to_player_for_source(
                    player_id,
                    "Not in a room".to_string(),
                    Some(ErrorCode::NotInRoom),
                    source_lifecycle.as_ref(),
                )
                .await;
        }
    }

    /// Handle binary game data payloads with coordination.
    /// Uses Bytes for zero-copy cloning during broadcast.
    pub async fn handle_game_data_binary(
        &self,
        player_id: &PlayerId,
        encoding: GameDataEncoding,
        payload: Bytes,
    ) {
        self.handle_game_data_binary_with_lifecycle(player_id, encoding, payload, None)
            .await;
    }

    pub(crate) async fn handle_game_data_binary_from_lifecycle(
        &self,
        player_id: &PlayerId,
        encoding: GameDataEncoding,
        payload: Bytes,
        source_lifecycle: Arc<ClientLifecycle>,
    ) {
        self.handle_game_data_binary_with_lifecycle(
            player_id,
            encoding,
            payload,
            Some(source_lifecycle),
        )
        .await;
    }

    async fn handle_game_data_binary_with_lifecycle(
        &self,
        player_id: &PlayerId,
        encoding: GameDataEncoding,
        payload: Bytes,
        source_lifecycle: Option<Arc<ClientLifecycle>>,
    ) {
        if !self.relay_source_is_current(player_id, source_lifecycle.as_ref()) {
            return;
        }
        // Per-encoding ceiling (issue #634): a configured
        // `security.max_game_data_bytes.<encoding>` replaces the global frame
        // cap for that encoding's raw payload bytes; absent knobs keep the
        // frame cap, byte-identical to the pre-#634 behavior (same cap value
        // AND the legacy rejection text below).
        let configured_cap = self
            .config
            .max_game_data_bytes
            .as_ref()
            .and_then(|limits| limits.cap_for(encoding));
        let cap = configured_cap.unwrap_or(self.config.max_message_size);
        if payload.len() > cap {
            // A rejection under the frame-cap fallback must stay
            // indistinguishable from the pre-#634 wire text; only a
            // per-encoding cap names the encoding.
            let (log_message, client_message) = match configured_cap {
                Some(_) => (
                    "Binary game data payload exceeds the maximum message size for its encoding",
                    format!(
                        "Binary payload exceeded maximum size for encoding {} ({} bytes)",
                        encoding.as_wire_str(),
                        cap
                    ),
                ),
                None => (
                    "Binary game data payload exceeds maximum message size",
                    format!(
                        "Binary payload exceeded maximum size ({} bytes)",
                        self.config.max_message_size
                    ),
                ),
            };
            tracing::warn!(
                %player_id,
                payload_size = payload.len(),
                max = cap,
                log_message
            );
            let _ = self
                .send_error_to_player_for_source(
                    player_id,
                    client_message,
                    Some(ErrorCode::MessageTooLarge),
                    source_lifecycle.as_ref(),
                )
                .await;
            return;
        }

        // Binary frames bypass the message router, so admitted frames record
        // liveness here (mirrors `handle_client_message`). Validation comes
        // first, and rejected frames — oversized or over-budget — record no
        // liveness: a stream of rejected frames must not keep an otherwise
        // idle client or room alive indefinitely.
        if let Some(room_id) = self.get_client_room(player_id).await {
            if !self.relay_source_is_current(player_id, source_lifecycle.as_ref()) {
                return;
            }
            // Source gate across admission and stamp/enqueue; see the text
            // lane for the issue #686 rationale.
            let source_gate = match source_lifecycle.as_ref() {
                Some(lifecycle) => Some(lifecycle.lock().await),
                None => None,
            };
            if !self.relay_source_is_current(player_id, source_lifecycle.as_ref()) {
                return;
            }
            // Sender-side relay byte budget (issue #519) plus the room's
            // aggregate ceiling (issue #530): charge the binary payload
            // before the fan-out, mirroring the text lane.
            if let Err(e) = self
                .check_and_charge_relay_bytes(player_id, &room_id, encoding, payload.len() as u64)
                .await
            {
                // The refusal reply locks the source gate itself; it is sent
                // only after this frame's admission released it.
                drop(source_gate);
                let _ = self
                    .send_error_to_player_for_source(
                        player_id,
                        e.to_string(),
                        Some(ErrorCode::RateLimitExceeded),
                        source_lifecycle.as_ref(),
                    )
                    .await;
                return;
            }
            // Charged frames record liveness (rejected frames never do), and
            // under the gate the refresh can only land on the incumbent. The
            // throttled database write holds the gate; the in-memory database
            // makes that bounded today, and a real backend must stay bounded
            // too or this call moves behind the gate release (#686 audit).
            self.record_client_activity(player_id);
            self.maybe_update_last_seen(player_id).await;
            let connection_manager = &self.connection_manager;
            let expected_room = room_id;
            let source_ref = &source_lifecycle;
            let mut build_message = one_shot_message_builder(move || {
                let stamp = connection_manager.next_relay_stamp_in_room_from_lifecycle(
                    player_id,
                    &expected_room,
                    source_ref.as_ref(),
                )?;
                Some(ServerMessage::GameDataBinary {
                    from_player: *player_id,
                    encoding,
                    payload,
                    seq: Some(stamp.seq),
                    epoch: Some(stamp.epoch),
                })
            });
            self.broadcast_game_data_with(player_id, &room_id, source_gate, &mut build_message)
                .await;
        } else {
            // Roomless frames keep their pre-existing liveness contract: the
            // frame was validly sized and is answered with NOT_IN_ROOM.
            self.record_client_activity(player_id);
            self.maybe_update_last_seen(player_id).await;
            // See the text-lane rationale above: an unseated sender must be
            // able to observe the rejection, not infer it from silence.
            let _ = self
                .send_error_to_player_for_source(
                    player_id,
                    "Not in a room".to_string(),
                    Some(ErrorCode::NotInRoom),
                    source_lifecycle.as_ref(),
                )
                .await;
        }
    }

    /// Broadcast one relayed game-data message (already stamped with its
    /// per-(sender, room) `seq` — text and binary share the single counter on
    /// the sender's `ClientConnection`) to the rest of the room.
    ///
    /// The stamp is carried inside the shared message envelope, so this layer
    /// — and the [`MessageCoordinator`](crate::coordination::MessageCoordinator)
    /// below it — stays protocol-version-agnostic: per-recipient gating
    /// (stripping `seq` for pre-v3 recipients) happens at serialization time
    /// in `websocket::sending`, where every other per-recipient wire decision
    /// (binary vs JSON-fallback encoding) already lives. Because the stamp is
    /// an ordinary serde field of `ServerMessage`, it also survives the
    /// cross-instance bus (`distributed::SequencedMessage` serializes the
    /// whole message); the in-memory single-instance coordinator is the only
    /// production backend today, so no remote instance can re-stamp or lose it.
    ///
    /// `source_gate` is the sender's held source lifecycle gate: it stays
    /// held across admission and stamp/enqueue and is released before any
    /// backpressured completion is awaited (issue #686). `None` keeps the
    /// legacy ungated shape for the no-lifecycle test entries.
    async fn broadcast_game_data_with(
        &self,
        player_id: &PlayerId,
        room_id: &RoomId,
        source_gate: Option<tokio::sync::MutexGuard<'_, ()>>,
        build_message: &mut (dyn FnMut() -> Option<ServerMessage> + Send),
    ) {
        if let Err(e) = self
            .start_game_data_broadcast(player_id, room_id, build_message, source_gate)
            .await
        {
            tracing::error!(
                %player_id,
                %room_id,
                error = %e,
                "Failed to broadcast game data to room"
            );
        }
    }

    /// Drive the production metric and the gated coordinator handoff. The
    /// caller holds the source lifecycle gate across admission and
    /// stamp/enqueue; the gate is released before any backpressured
    /// completion is awaited (issue #686).
    async fn start_game_data_broadcast(
        &self,
        player_id: &PlayerId,
        room_id: &RoomId,
        build_message: &mut (dyn FnMut() -> Option<ServerMessage> + Send),
        source_gate: Option<tokio::sync::MutexGuard<'_, ()>>,
    ) -> anyhow::Result<()> {
        // Acceptance-time semantics, deliberately counted BEFORE the
        // builder runs: the increment doubles as the synchronization marker
        // that a broadcast was accepted (contention tests observe
        // `game_data_messages` moving while the builder has not been consumed
        // yet), so it also covers accepted-but-cancelled builds (unseated
        // sender / stamp exhaustion). Delivery attempts are counted separately
        // by the coordinator metrics. This differs from `signals_relayed`,
        // which is counted at dispatch after every gate (signaling.rs) — the
        // two families are intentionally not interchangeable.
        self.metrics.increment_game_data_messages();
        let coordinator = self.message_coordinator.as_ref();
        // Fast path: the synchronous admission never awaits, so the stamp and
        // enqueue cannot race the gate.
        match coordinator.try_broadcast_to_room_except_with_borrowed_owned_message(
            room_id,
            player_id,
            build_message,
        ) {
            crate::coordination::ImmediateGameDataBroadcast::Complete => return Ok(()),
            crate::coordination::ImmediateGameDataBroadcast::Pending(completion) => {
                // The rekey may proceed while backpressured fan-out drains;
                // every stamp and enqueue already completed under the gate.
                drop(source_gate);
                completion.await;
                return Ok(());
            }
            crate::coordination::ImmediateGameDataBroadcast::Unavailable => {}
        }
        // Contended: run the fallback enqueue under the caller's gate, then
        // release the gate before the backpressured completion.
        match coordinator
            .enqueue_relay_broadcast_after_contention(room_id, player_id, build_message)
            .await
        {
            crate::coordination::RelayEnqueueOutcome::Finished(result) => {
                drop(source_gate);
                result
            }
            crate::coordination::RelayEnqueueOutcome::AwaitFinish(completion) => {
                drop(source_gate);
                completion.await
            }
        }
    }
}

/// Drive the production metric, one-shot adapter, and the ungated
/// coordinator handoff. Compatibility entry for direct-coordinator tests;
/// the production lanes use the gated `EnhancedGameServer`
/// `broadcast_game_data_with` method above.
#[cfg(any(test, feature = "allocation-tracking"))]
pub async fn broadcast_game_data_with<F>(
    message_coordinator: &dyn crate::coordination::MessageCoordinator,
    metrics: &crate::metrics::ServerMetrics,
    player_id: &PlayerId,
    room_id: &RoomId,
    build_message: F,
) -> anyhow::Result<()>
where
    F: FnOnce() -> Option<ServerMessage> + Send,
{
    metrics.increment_game_data_messages();
    let mut build_message = one_shot_message_builder(build_message);
    match message_coordinator.try_broadcast_to_room_except_with_borrowed_owned_message(
        room_id,
        player_id,
        &mut build_message,
    ) {
        crate::coordination::ImmediateGameDataBroadcast::Complete => Ok(()),
        crate::coordination::ImmediateGameDataBroadcast::Pending(completion) => {
            completion.await;
            Ok(())
        }
        crate::coordination::ImmediateGameDataBroadcast::Unavailable => {
            match message_coordinator
                .enqueue_relay_broadcast_after_contention(room_id, player_id, &mut build_message)
                .await
            {
                crate::coordination::RelayEnqueueOutcome::Finished(result) => result,
                crate::coordination::RelayEnqueueOutcome::AwaitFinish(completion) => {
                    completion.await
                }
            }
        }
    }
}

pub(super) fn one_shot_message_builder<F>(build_message: F) -> impl FnMut() -> Option<ServerMessage>
where
    F: FnOnce() -> Option<ServerMessage>,
{
    let mut build_message = Some(build_message);
    move || build_message.take().and_then(|build| build())
}
