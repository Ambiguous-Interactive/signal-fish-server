use std::sync::Arc;

#[cfg(test)]
#[cfg(signal_fish_repository_tests)]
use std::sync::LazyLock;
#[cfg(test)]
#[cfg(signal_fish_repository_tests)]
use tokio::sync::Notify;

use crate::coordination::ConnectionCloseSignal;
use crate::protocol::{ClientMessage, PlayerId, RoomOperationRequest, ServerMessage};

use super::{ClientLifecycle, EnhancedGameServer, TransportStatusUpdate};

#[cfg(test)]
#[cfg(signal_fish_repository_tests)]
static TRANSPORT_STATUS_DELIVERY_PAUSES: LazyLock<
    dashmap::DashMap<crate::protocol::PlayerId, Arc<TransportStatusDeliveryPause>>,
> = LazyLock::new(dashmap::DashMap::new);

#[cfg(all(test, signal_fish_repository_tests))]
static SOCKET_DISPATCH_PAUSES: LazyLock<
    dashmap::DashMap<crate::protocol::PlayerId, Arc<SocketDispatchPause>>,
> = LazyLock::new(dashmap::DashMap::new);

#[cfg(all(test, signal_fish_repository_tests))]
pub(super) struct SocketDispatchPause {
    pub(super) reached: Notify,
    pub(super) release: Notify,
}

#[cfg(all(test, signal_fish_repository_tests))]
pub(super) fn arm_socket_dispatch_pause(player_id: PlayerId) -> Arc<SocketDispatchPause> {
    let pause = Arc::new(SocketDispatchPause {
        reached: Notify::new(),
        release: Notify::new(),
    });
    SOCKET_DISPATCH_PAUSES.insert(player_id, Arc::clone(&pause));
    pause
}

#[cfg(test)]
#[cfg(signal_fish_repository_tests)]
pub(super) struct TransportStatusDeliveryPause {
    pub(super) reached: Notify,
    pub(super) release: Notify,
}

#[cfg(test)]
#[cfg(signal_fish_repository_tests)]
pub(super) fn arm_transport_status_delivery_pause(
    player_id: crate::protocol::PlayerId,
) -> Arc<TransportStatusDeliveryPause> {
    let pause = Arc::new(TransportStatusDeliveryPause {
        reached: Notify::new(),
        release: Notify::new(),
    });
    TRANSPORT_STATUS_DELIVERY_PAUSES.insert(player_id, Arc::clone(&pause));
    pause
}

#[cfg(test)]
#[cfg(signal_fish_repository_tests)]
pub(super) fn disarm_transport_status_delivery_pause(player_id: &crate::protocol::PlayerId) {
    TRANSPORT_STATUS_DELIVERY_PAUSES.remove(player_id);
}

/// The prepared room fan-out of an accepted transport-state change: the exact
/// recipient snapshot and the shared event, ready to dispatch once the caller
/// has released its lifecycle gate.
struct TransportStatusFanOut {
    sender: PlayerId,
    source_close: ConnectionCloseSignal,
    room_id: crate::protocol::RoomId,
    membership_generation: uuid::Uuid,
    recipients: Vec<PlayerId>,
    message: Arc<ServerMessage>,
}

impl TransportStatusFanOut {
    /// Deliver outside the lifecycle and room gates. Sender and recipient
    /// membership are revalidated at queue commit, and each leg
    /// re-checks the sender's close state and the recipient's negotiated v3
    /// capability. An evicted sender can retain its room assignment until
    /// teardown completes.
    async fn deliver(&self, server: &EnhancedGameServer) {
        // Deliver to all peers concurrently: one slow room member costs this
        // event one slow-consumer window, never `(N - 1)` windows. Recipient
        // filtering is v3-only but deliberately transport-agnostic: a
        // relay-only client still needs to know that a peer fell back.
        const MAX_INLINE_RECIPIENTS: usize = 30;
        const RECIPIENTS_PER_GROUP: usize = 16;

        let delivered = if self.recipients.len() <= MAX_INLINE_RECIPIENTS {
            self.deliver_group(server, &self.recipients).await
        } else {
            futures_util::future::join_all(
                self.recipients
                    .chunks(RECIPIENTS_PER_GROUP)
                    .map(|recipients| self.deliver_group(server, recipients)),
            )
            .await
            .into_iter()
            .any(std::convert::identity)
        };

        // Count one event only when at least one peer received the status.
        if delivered {
            server.metrics.record_transport_status_fanout();
        }
    }

    async fn deliver_group(&self, server: &EnhancedGameServer, recipients: &[PlayerId]) -> bool {
        match recipients {
            [] => false,
            [recipient] => self.deliver_to(server, recipient).await,
            [first, second] => {
                let (first, second) = tokio::join!(
                    self.deliver_to(server, first),
                    self.deliver_to(server, second),
                );
                first || second
            }
            [first, second, third] => {
                let (first, second, third) = tokio::join!(
                    self.deliver_to(server, first),
                    self.deliver_to(server, second),
                    self.deliver_to(server, third),
                );
                first || second || third
            }
            _ => futures_util::future::join_all(
                recipients
                    .iter()
                    .map(|recipient| self.deliver_to(server, recipient)),
            )
            .await
            .into_iter()
            .any(std::convert::identity),
        }
    }

    async fn deliver_to(&self, server: &EnhancedGameServer, recipient: &PlayerId) -> bool {
        if !server.client_supports_v3(recipient) {
            return false;
        }
        #[cfg(test)]
        #[cfg(signal_fish_repository_tests)]
        if let Some(pause) = TRANSPORT_STATUS_DELIVERY_PAUSES
            .get(&self.sender)
            .map(|entry| Arc::clone(entry.value()))
        {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
        let delivery_is_current = || {
            server.client_supports_v3(recipient)
                && server
                    .connection_manager
                    .live_membership_generation_in_room(&self.sender, &self.room_id)
                    == Some(self.membership_generation)
        };
        server
            .message_coordinator
            .send_to_player_in_room_from_socket_if(
                recipient,
                &self.room_id,
                &self.sender,
                &self.source_close,
                Arc::clone(&self.message),
                &delivery_is_current,
            )
            .await
            .unwrap_or(false)
    }
}

impl EnhancedGameServer {
    /// Resolve status peers for coordinators that do not expose routed membership.
    pub(super) async fn transport_status_fallback_recipients(
        &self,
        player_id: &PlayerId,
        room_id: &crate::protocol::RoomId,
    ) -> anyhow::Result<Vec<PlayerId>> {
        let mut members = self.database.get_room_player_ids(room_id).await?;
        members.retain(|member| member != player_id && self.client_supports_v3(member));
        Ok(members)
    }

    /// Handle incoming client message with enhanced coordination.
    pub async fn handle_client_message(
        self: &Arc<Self>,
        player_id: &PlayerId,
        message: ClientMessage,
    ) {
        self.handle_client_message_with_lifecycle(player_id, message, None)
            .await;
    }

    pub(crate) async fn handle_client_message_from_lifecycle(
        self: &Arc<Self>,
        player_id: &PlayerId,
        message: ClientMessage,
        lifecycle: Arc<ClientLifecycle>,
    ) {
        self.handle_client_message_with_lifecycle(player_id, message, Some(lifecycle))
            .await;
    }

    async fn handle_client_message_with_lifecycle(
        self: &Arc<Self>,
        player_id: &PlayerId,
        message: ClientMessage,
        source_lifecycle: Option<Arc<ClientLifecycle>>,
    ) {
        let source_lifecycle =
            source_lifecycle.or_else(|| self.connection_manager.client_lifecycle(player_id));
        let source_close = if let Some(lifecycle) = &source_lifecycle {
            let Some(close) = self
                .connection_manager
                .close_signal_for_lifecycle(player_id, lifecycle)
            else {
                return;
            };
            if !self
                .refresh_socket_activity(player_id, lifecycle, &close)
                .await
            {
                return;
            }
            Some(close)
        } else {
            if matches!(message, ClientMessage::TransportStatus { .. }) {
                return;
            }
            self.refresh_client_activity(player_id).await;
            None
        };
        #[cfg(all(test, signal_fish_repository_tests))]
        if let Some((_, pause)) = SOCKET_DISPATCH_PAUSES.remove(player_id) {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
        match message {
            ClientMessage::Authenticate { app_id, .. } => {
                tracing::warn!(
                    %player_id,
                    // Debug-escaped deliberately: this anomalous path warns
                    // BEFORE resolve_app_id's log-safety gate can vet the ID,
                    // so a raw `%` field would let control characters forge
                    // this log line.
                    app_id = ?app_id,
                    "Received Authenticate message after connection established - this should not happen. \
                     App-ID negotiation must occur during the WebSocket handshake."
                );
            }
            ClientMessage::JoinRoom {
                game_name,
                room_code,
                player_name,
                max_players,
                supports_authority,
                relay_transport,
                password,
                join_only,
            } => {
                self.handle_join_room_operation_from_lifecycle(
                    player_id,
                    None,
                    game_name,
                    room_code,
                    player_name,
                    max_players,
                    supports_authority,
                    relay_transport,
                    password,
                    join_only,
                    source_lifecycle,
                )
                .await;
            }
            ClientMessage::LeaveRoom => {
                self.leave_room_operation_from_lifecycle(player_id, None, source_lifecycle)
                    .await;
            }
            ClientMessage::GameData { data, class, key } => {
                self.handle_game_data_from_lifecycle(player_id, data, class, key, source_lifecycle)
                    .await;
            }
            ClientMessage::Signal {
                to,
                generation,
                signal,
            } => {
                self.handle_signal_in_generation_from_lifecycle(
                    player_id,
                    to,
                    generation,
                    signal,
                    source_lifecycle,
                )
                .await;
            }
            ClientMessage::AuthorityRequest { become_authority } => {
                self.handle_authority_request_from_lifecycle(
                    player_id,
                    become_authority,
                    source_lifecycle,
                )
                .await;
            }
            ClientMessage::PlayerReady => {
                self.handle_player_ready_from_lifecycle(player_id, source_lifecycle)
                    .await;
            }
            ClientMessage::StartGame => {
                self.handle_start_game_from_lifecycle(player_id, source_lifecycle)
                    .await;
            }
            ClientMessage::ProvideConnectionInfo { connection_info } => {
                self.handle_provide_connection_info_from_lifecycle(
                    player_id,
                    connection_info,
                    source_lifecycle,
                )
                .await;
            }
            ClientMessage::Ping => {
                self.handle_ping_from_lifecycle(player_id, source_lifecycle)
                    .await;
            }
            ClientMessage::Reconnect { .. } => {
                // Reconnection swaps the socket's routing identity and is
                // driven only by the connection task that owns the socket's
                // `effective_player_id` (see `websocket/connection.rs`). The
                // router cannot update that identity, so dispatching here
                // would half-reconnect: the routing map would move to the
                // reconnected identity while this socket keeps stamping
                // frames as the transient sender, and every later frame it
                // sends would be dropped. Fail closed instead.
                tracing::warn!(
                    player = %player_id,
                    "Reconnect reached the message router; reconnection is dispatched only by the connection task"
                );
                self.send_router_refusal(
                    player_id,
                    ServerMessage::Error {
                        message: "Reconnection is dispatched by the connection that owns the \
                                      reconnection identity"
                            .to_string(),
                        error_code: Some(crate::protocol::ErrorCode::ReconnectionFailed),
                    },
                    source_lifecycle,
                    None,
                )
                .await;
            }
            ClientMessage::JoinAsSpectator {
                game_name,
                room_code,
                spectator_name,
                password,
            } => {
                self.handle_join_as_spectator_operation_from_lifecycle(
                    player_id,
                    None,
                    game_name,
                    room_code,
                    spectator_name,
                    password,
                    source_lifecycle,
                )
                .await;
            }
            ClientMessage::LeaveSpectator => {
                self.handle_leave_spectator_operation_from_lifecycle(
                    player_id,
                    None,
                    source_lifecycle,
                )
                .await;
            }
            ClientMessage::RoomOperation {
                operation_id,
                operation,
            } => {
                let _guard = if let (Some(lifecycle), Some(close)) =
                    (&source_lifecycle, &source_close)
                {
                    let Some(guard) = close.while_open(Arc::clone(lifecycle).lock_owned()).await
                    else {
                        return;
                    };
                    if lifecycle.player_id() != *player_id
                        || !self
                            .connection_manager
                            .lifecycle_matches(player_id, lifecycle)
                    {
                        return;
                    }
                    Some(guard)
                } else {
                    None
                };
                if !self.client_supports_room_operation_ids(player_id) {
                    // A pre-v3 session has no RoomOperation surface at all, so
                    // the version code is truthful there. A negotiated-v3
                    // session that never requested the capability has a valid
                    // version; only the capability constraint is unmet, and
                    // "upgrade the client" guidance would be wrong.
                    let error_code = if self.client_supports_v3(player_id) {
                        crate::protocol::ErrorCode::InvalidInput
                    } else {
                        crate::protocol::ErrorCode::UnsupportedProtocolVersion
                    };
                    self
                        .send_router_refusal(
                            player_id,
                            ServerMessage::Error {
                                message: "RoomOperation requires the negotiated room_operation_ids capability".to_string(),
                                error_code: Some(error_code),
                            },
                            source_lifecycle,
                            _guard,
                        )
                        .await;
                    return;
                }
                match *operation {
                    RoomOperationRequest::JoinRoom {
                        game_name,
                        room_code,
                        player_name,
                        max_players,
                        supports_authority,
                        relay_transport,
                        password,
                        join_only,
                    } => {
                        drop(_guard);
                        self.handle_join_room_operation_from_lifecycle(
                            player_id,
                            Some(operation_id),
                            game_name,
                            room_code,
                            player_name,
                            max_players,
                            supports_authority,
                            relay_transport,
                            password,
                            join_only,
                            source_lifecycle,
                        )
                        .await;
                    }
                    RoomOperationRequest::LeaveRoom => {
                        drop(_guard);
                        self.leave_room_operation_from_lifecycle(
                            player_id,
                            Some(operation_id),
                            source_lifecycle,
                        )
                        .await;
                    }
                    RoomOperationRequest::Reconnect { .. } => {
                        // Same fail-closed contract as the plain
                        // `ClientMessage::Reconnect` arm above: only the
                        // connection task may drive a reconnect identity swap.
                        tracing::warn!(
                            player = %player_id,
                            "Reconnect operation reached the message router; reconnection is dispatched only by the connection task"
                        );
                        self.send_router_refusal(
                            player_id,
                            ServerMessage::room_operation_failed(
                                operation_id,
                                "Reconnection is dispatched by the connection that owns \
                                     the reconnection identity",
                                Some(crate::protocol::ErrorCode::ReconnectionFailed),
                            ),
                            source_lifecycle,
                            _guard,
                        )
                        .await;
                    }
                    RoomOperationRequest::JoinAsSpectator {
                        game_name,
                        room_code,
                        spectator_name,
                        password,
                    } => {
                        drop(_guard);
                        self.handle_join_as_spectator_operation_from_lifecycle(
                            player_id,
                            Some(operation_id),
                            game_name,
                            room_code,
                            spectator_name,
                            password,
                            source_lifecycle,
                        )
                        .await;
                    }
                    RoomOperationRequest::LeaveSpectator => {
                        drop(_guard);
                        self.handle_leave_spectator_operation_from_lifecycle(
                            player_id,
                            Some(operation_id),
                            source_lifecycle,
                        )
                        .await;
                    }
                    RoomOperationRequest::KickPlayer { player_id: target } => {
                        drop(_guard);
                        self.handle_kick_player_from_lifecycle(
                            player_id,
                            operation_id,
                            target,
                            source_lifecycle,
                        )
                        .await;
                    }
                    RoomOperationRequest::RegenerateRoomCode => {
                        drop(_guard);
                        self.handle_regenerate_room_code_from_lifecycle(
                            player_id,
                            operation_id,
                            source_lifecycle,
                        )
                        .await;
                    }
                    RoomOperationRequest::SetRoomAccess { password } => {
                        drop(_guard);
                        self.handle_set_room_access_from_lifecycle(
                            player_id,
                            operation_id,
                            password,
                            source_lifecycle,
                        )
                        .await;
                    }
                    RoomOperationRequest::BanPlayer { player_id: target } => {
                        drop(_guard);
                        self.handle_ban_player_from_lifecycle(
                            player_id,
                            operation_id,
                            target,
                            source_lifecycle,
                        )
                        .await;
                    }
                    RoomOperationRequest::UnbanPlayer { player_id: target } => {
                        drop(_guard);
                        self.handle_unban_player_from_lifecycle(
                            player_id,
                            operation_id,
                            target,
                            source_lifecycle,
                        )
                        .await;
                    }
                    RoomOperationRequest::TransferAuthority { player_id: target } => {
                        drop(_guard);
                        self.handle_transfer_authority_from_lifecycle(
                            player_id,
                            operation_id,
                            target,
                            source_lifecycle,
                        )
                        .await;
                    }
                }
            }
            ClientMessage::TransportStatus {
                transport,
                connected,
            } => {
                if let (Some(lifecycle), Some(close)) = (source_lifecycle, source_close) {
                    self.handle_transport_status(player_id, transport, connected, lifecycle, close)
                        .await;
                }
            }
        }
    }

    async fn send_router_refusal(
        self: &Arc<Self>,
        player_id: &PlayerId,
        message: ServerMessage,
        source_lifecycle: Option<Arc<ClientLifecycle>>,
        source_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    ) {
        let Some(lifecycle) =
            source_lifecycle.or_else(|| self.connection_manager.client_lifecycle(player_id))
        else {
            return;
        };
        let guard = match source_guard {
            Some(guard) => guard,
            None => Arc::clone(&lifecycle).lock_owned().await,
        };
        if lifecycle.player_id() != *player_id
            || !self
                .connection_manager
                .lifecycle_matches(player_id, &lifecycle)
        {
            return;
        }
        let server = Arc::clone(self);
        let player_id = *player_id;
        let completion = tokio::spawn(async move {
            let _guard = guard;
            if !server.charge_error_reply(&player_id).await {
                return Ok(());
            }
            server
                .message_coordinator
                .send_to_player(&player_id, Arc::new(message))
                .await
        });
        match completion.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::error!(%player_id, %error, "Router refusal delivery failed"),
            Err(error) => tracing::error!(%player_id, %error, "Router refusal transaction failed"),
        }
    }

    /// Record a client's reported data-path transport state (Protocol v3).
    ///
    /// Purely informational and v3-only: a v2 client can never legitimately send
    /// this, and a v3 report is accepted only for a transport negotiated by that
    /// connection. Invalid reports are ignored (debug-logged) as defense-in-depth
    /// (the reporting connection's negotiated-transport gate). The relay floor
    /// never closes regardless of what is reported
    /// — this only drives observability and, in future, targeted relay for stuck
    /// peers.
    ///
    /// Duplicate reports of the same `(transport, connected)` pair in one
    /// membership generation update no counters and fan nothing out; the
    /// metrics and the `PeerTransportStatus` fan-out below are emitted only for
    /// the generation's first report or a real state transition.
    ///
    /// Metric interpretation:
    /// - `connected == true` AND a P2P transport (`Direct` / `WebRtc`) ⇒
    ///   `record_p2p_established` (a peer-to-peer path came up).
    /// - `connected == false` ⇒ `record_relay_fallback` (the client dropped back to
    ///   the relay floor), regardless of which transport it names.
    /// - `connected == true` with `transport: relay` is just "I am on the floor":
    ///   it is not a P2P establishment and not a fallback event, so it moves no
    ///   counter — only the current generation's stored state is updated.
    ///   (Documented here and in `docs/architecture/transport-fallback.md`.)
    async fn handle_transport_status(
        self: &Arc<Self>,
        player_id: &PlayerId,
        transport: crate::protocol::Transport,
        connected: bool,
        lifecycle: Arc<ClientLifecycle>,
        source_close: ConnectionCloseSignal,
    ) {
        let server = Arc::clone(self);
        let player_id = *player_id;
        let transaction = tokio::spawn(async move {
            server
                .transport_status_owned(&player_id, transport, connected, lifecycle, source_close)
                .await;
        });
        if let Err(error) = transaction.await {
            tracing::error!(%player_id, %error, "Transport report transaction failed");
        }
    }

    async fn transport_status_owned(
        &self,
        player_id: &PlayerId,
        transport: crate::protocol::Transport,
        connected: bool,
        lifecycle: Arc<ClientLifecycle>,
        source_close: ConnectionCloseSignal,
    ) {
        let Some((_report_guard, lifecycle_guard)) = source_close
            .while_open(async {
                let report_guard = lifecycle.lock_transport_reports().await;
                let lifecycle_guard = lifecycle.lock().await;
                (report_guard, lifecycle_guard)
            })
            .await
        else {
            return;
        };
        if lifecycle.player_id() != *player_id
            || !self
                .connection_manager
                .lifecycle_matches(player_id, &lifecycle)
            || source_close.requested_reason().is_some()
        {
            return;
        }

        let Some(fan_out) = self
            .handle_transport_status_under_lifecycle(player_id, transport, connected, source_close)
            .await
        else {
            return;
        };
        // Release the sender's lifecycle gate before delivery: the fan-out's
        // backpressured legs park up to one slow-consumer window, and this
        // connection's own concurrent transitions (a reconnect claiming this
        // identity, teardown) must not queue behind them. Recipient truth no
        // longer depends on either gate — see the fan-out below.
        drop(lifecycle_guard);
        fan_out.deliver(self).await;
    }

    /// Process a transport report after the caller has fixed the connection
    /// identity and membership with its lifecycle guard. Returns the prepared
    /// room fan-out to dispatch after the caller releases its lifecycle
    /// gate, or `None` when the report fans nothing out.
    async fn handle_transport_status_under_lifecycle(
        &self,
        player_id: &PlayerId,
        transport: crate::protocol::Transport,
        connected: bool,
        source_close: ConnectionCloseSignal,
    ) -> Option<TransportStatusFanOut> {
        use crate::protocol::Transport;

        match self.set_client_transport_status(player_id, transport, connected) {
            TransportStatusUpdate::Changed => {}
            TransportStatusUpdate::Duplicate => {
                tracing::debug!(
                    %player_id,
                    ?transport,
                    connected,
                    "Ignoring duplicate TransportStatus report"
                );
                return None;
            }
            TransportStatusUpdate::MissingConnection => {
                tracing::debug!(
                    %player_id,
                    ?transport,
                    connected,
                    "Ignoring TransportStatus for connection that no longer exists"
                );
                return None;
            }
            TransportStatusUpdate::UnsupportedProtocolVersion => {
                tracing::debug!(
                    %player_id,
                    ?transport,
                    connected,
                    "Ignoring TransportStatus from a non-v3 connection (v3-only message)"
                );
                return None;
            }
            TransportStatusUpdate::UnsupportedTransport => {
                let protocol = self.client_protocol(player_id);
                tracing::debug!(
                    %player_id,
                    ?transport,
                    connected,
                    negotiated_transports = ?protocol.transports,
                    "Ignoring TransportStatus for transport not negotiated by connection"
                );
                return None;
            }
        }

        if !connected {
            // The client fell back to the relay floor (for any transport it names).
            self.metrics.record_relay_fallback();
        } else if matches!(transport, Transport::Direct | Transport::WebRtc) {
            // A peer-to-peer data path came up. `connected: true` with `relay`
            // means "still on the floor" and is intentionally not counted.
            self.metrics.record_p2p_established();
        }

        // Resolve the sender's room once: it scopes both the per-session
        // attribution below (issue #766; a roomless report counts server-wide
        // only, exactly as before) and the fan-out further down. No await
        // intervenes until the fan-out's own resolution, and the sender's
        // lifecycle gate held by the caller pins membership across both uses.
        let room_id = self.get_client_room(player_id).await;
        if let Some(records) = self.session_records() {
            if let Some(room_id) = &room_id {
                if !connected {
                    records.record_relay_fallback(room_id);
                } else if matches!(transport, Transport::Direct | Transport::WebRtc) {
                    records.record_p2p_established(room_id);
                }
            }
        }

        // Fan the accepted state change out to the sender's CURRENT room as
        // `PeerTransportStatus`, so peers learn e.g. that
        // the host's WebRTC path died and relay-path traffic should be
        // expected. Duplicate reports returned early above, so a fan-out fires
        // once per real state change in the current membership generation
        // (including its first report). No room ⇒ nothing to fan out — the
        // generation-scoped state was still recorded above.
        let room_id = room_id?;
        let _room_snapshot_guard = source_close
            .while_open(self.message_coordinator.lock_room_signal(&room_id))
            .await?;

        // Cheap non-consuming preflight before the fallible/O(room) membership
        // snapshot below. The consuming check still happens after recipient
        // resolution, immediately before dispatch, so failed lookups and empty
        // fan-outs do not burn a slot while already-over-budget clients cannot
        // keep forcing room scans.
        if source_close
            .while_open(self.rate_limiter.check_signal_available(player_id))
            .await?
            .is_err()
        {
            tracing::debug!(
                %player_id,
                ?transport,
                connected,
                "Dropping TransportStatus fan-out: per-connection signal rate limit exceeded"
            );
            return None;
        }

        // Resolve the exact live v3 recipients before charging the sender's
        // control-plane budget. Production exposes coordinator routing; the
        // database fallback keeps lightweight/distributed test coordinators
        // compatible without weakening the production source-route check.
        let recipients: Vec<PlayerId> = match source_close
            .while_open(self.message_coordinator.routed_player_ids(&room_id))
            .await?
        {
            Ok(Some(routed)) => {
                if !routed.contains(player_id) {
                    tracing::debug!(%player_id, %room_id, "Skipping TransportStatus from an unrouted sender");
                    return None;
                }
                routed
                    .into_iter()
                    .filter(|recipient| {
                        *recipient != *player_id && self.client_supports_v3(recipient)
                    })
                    .collect()
            }
            Ok(None) => match source_close
                .while_open(self.transport_status_fallback_recipients(player_id, &room_id))
                .await?
            {
                Ok(members) => members,
                Err(err) => {
                    tracing::warn!(
                        %player_id,
                        %room_id,
                        error = %err,
                        "Failed to load room members for PeerTransportStatus fan-out"
                    );
                    return None;
                }
            },
            Err(err) => {
                tracing::warn!(
                    %player_id,
                    %room_id,
                    error = %err,
                    "Failed to resolve routed members for PeerTransportStatus fan-out"
                );
                return None;
            }
        };

        if recipients.is_empty() {
            tracing::trace!(
                %player_id,
                %room_id,
                ?transport,
                connected,
                "Skipping TransportStatus fan-out: no eligible v3 room peers"
            );
            return None;
        }

        // The room fan-out below is the only 1→N amplifier on this path (the
        // per-connection state update and the p2p/relay counters above are O(1)
        // local bookkeeping), so consume the same per-connection WebRTC
        // control-plane budget as `Signal` (`rate_limiter.check_signal`). A
        // client that alternates `connected` to force a `Changed` on every frame
        // (defeating the dedup gate above) therefore cannot use the tiny status
        // message as an unbounded room amplifier. This consuming gate is placed
        // after membership resolution and recipient filtering so a room-less
        // reporter, failed room snapshot, or empty eligible recipient set
        // consumes no budget for a fan-out that cannot happen. It is repeated
        // despite the preflight above because another task can consume the last
        // slot between preflight and dispatch. Over-budget changes are dropped
        // SILENTLY: `TransportStatus` is informational and defines no error
        // reply, and the per-connection state was already recorded above, so
        // the connection's own transport truth stays current regardless of the
        // fan-out budget. (The dominant relay-floor `GameData` fan-out is
        // bounded by other means — size cap, connection/room caps, best-effort
        // sends — so this only closes the control-plane consistency gap with
        // `Signal`.)
        if source_close
            .while_open(self.rate_limiter.check_signal(player_id))
            .await?
            .is_err()
        {
            tracing::debug!(
                %player_id,
                ?transport,
                connected,
                "Dropping TransportStatus fan-out: per-connection signal rate limit exceeded"
            );
            return None;
        }

        let membership_generation = self
            .connection_manager
            .live_membership_generation_in_room(player_id, &room_id)?;
        let message = Arc::new(ServerMessage::PeerTransportStatus {
            peer_id: *player_id,
            transport,
            connected,
        });
        Some(TransportStatusFanOut {
            sender: *player_id,
            source_close,
            room_id,
            membership_generation,
            recipients,
            message,
        })
    }
}
