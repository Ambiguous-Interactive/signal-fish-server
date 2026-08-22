use std::sync::Arc;

#[cfg(test)]
#[cfg(signal_fish_repository_tests)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
#[cfg(signal_fish_repository_tests)]
use std::sync::LazyLock;

use crate::protocol::{ClientMessage, PlayerId, RoomOperationRequest, ServerMessage};

use super::{EnhancedGameServer, TransportStatusUpdate};

#[cfg(test)]
#[cfg(signal_fish_repository_tests)]
static TRANSPORT_STATUS_LIFECYCLE_PROBES: LazyLock<
    dashmap::DashMap<crate::protocol::PlayerId, Arc<AtomicBool>>,
> = LazyLock::new(dashmap::DashMap::new);

#[cfg(test)]
#[cfg(signal_fish_repository_tests)]
pub(super) fn arm_transport_status_lifecycle_probe(
    player_id: crate::protocol::PlayerId,
) -> Arc<AtomicBool> {
    let probe = Arc::new(AtomicBool::new(false));
    TRANSPORT_STATUS_LIFECYCLE_PROBES.insert(player_id, Arc::clone(&probe));
    probe
}

#[cfg(test)]
#[cfg(signal_fish_repository_tests)]
pub(super) fn disarm_transport_status_lifecycle_probe(player_id: &crate::protocol::PlayerId) {
    TRANSPORT_STATUS_LIFECYCLE_PROBES.remove(player_id);
}

impl EnhancedGameServer {
    /// Handle incoming client message with enhanced coordination.
    pub async fn handle_client_message(
        self: &Arc<Self>,
        player_id: &PlayerId,
        message: ClientMessage,
    ) {
        // EVERY inbound message is liveness, not just `Ping`: the activity
        // reaper (`server.ping_timeout`) must never disconnect a client that
        // is actively streaming GameData/Signal traffic but not heartbeating.
        // This matches the socket-level idle timeout, which already counts
        // any inbound frame as activity.
        self.record_client_activity(player_id);
        // ...and every inbound message refreshes the sender's ROOM clock the
        // same way (throttled), so a room stays alive as long as any member is
        // doing ANYTHING — pinging, relaying GameData, OR exchanging WebRTC
        // `Signal`s (a long handshake with no pings must not let GC reap an
        // occupied room, BUG-1). This is the single room-liveness refresh site;
        // it subsumes the former per-handler calls in `handle_ping` /
        // `broadcast_game_data`. No-ops for a roomless sender (pre-join).
        self.maybe_update_last_seen(player_id).await;
        match message {
            ClientMessage::Authenticate { app_id, .. } => {
                tracing::warn!(
                    %player_id,
                    %app_id,
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
            } => {
                self.handle_join_room(
                    player_id,
                    game_name,
                    room_code,
                    player_name,
                    max_players,
                    supports_authority,
                    relay_transport,
                )
                .await;
            }
            ClientMessage::LeaveRoom => {
                self.leave_room(player_id).await;
            }
            ClientMessage::GameData { data, class, key } => {
                self.handle_game_data(player_id, data, class, key).await;
            }
            ClientMessage::Signal {
                to,
                generation,
                signal,
            } => {
                self.handle_signal_in_generation(player_id, to, generation, signal)
                    .await;
            }
            ClientMessage::AuthorityRequest { become_authority } => {
                self.handle_authority_request(player_id, become_authority)
                    .await;
            }
            ClientMessage::PlayerReady => {
                self.handle_player_ready(player_id).await;
            }
            ClientMessage::StartGame => {
                self.handle_start_game(player_id).await;
            }
            ClientMessage::ProvideConnectionInfo { connection_info } => {
                self.handle_provide_connection_info(player_id, connection_info)
                    .await;
            }
            ClientMessage::Ping => {
                self.handle_ping(player_id).await;
            }
            ClientMessage::Reconnect {
                player_id: reconnect_player_id,
                room_id,
                auth_token,
            } => {
                self.handle_reconnect(player_id, &reconnect_player_id, &room_id, &auth_token)
                    .await;
            }
            ClientMessage::JoinAsSpectator {
                game_name,
                room_code,
                spectator_name,
            } => {
                self.handle_join_as_spectator(player_id, game_name, room_code, spectator_name)
                    .await;
            }
            ClientMessage::LeaveSpectator => {
                self.handle_leave_spectator(player_id).await;
            }
            ClientMessage::RoomOperation {
                operation_id,
                operation,
            } => {
                if !self.client_supports_room_operation_ids(player_id) {
                    let _ = self
                        .send_error_to_player(
                            player_id,
                            "RoomOperation requires the negotiated room_operation_ids capability"
                                .to_string(),
                            Some(crate::protocol::ErrorCode::UnsupportedProtocolVersion),
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
                    } => {
                        self.handle_join_room_operation(
                            player_id,
                            Some(operation_id),
                            game_name,
                            room_code,
                            player_name,
                            max_players,
                            supports_authority,
                            relay_transport,
                        )
                        .await;
                    }
                    RoomOperationRequest::LeaveRoom => {
                        self.leave_room_operation(player_id, Some(operation_id))
                            .await;
                    }
                    RoomOperationRequest::Reconnect {
                        player_id: reconnect_player_id,
                        room_id,
                        auth_token,
                    } => {
                        self.handle_reconnect_operation(
                            player_id,
                            &reconnect_player_id,
                            &room_id,
                            &auth_token,
                            Some(operation_id),
                        )
                        .await;
                    }
                    RoomOperationRequest::JoinAsSpectator {
                        game_name,
                        room_code,
                        spectator_name,
                    } => {
                        self.handle_join_as_spectator_operation(
                            player_id,
                            Some(operation_id),
                            game_name,
                            room_code,
                            spectator_name,
                        )
                        .await;
                    }
                    RoomOperationRequest::LeaveSpectator => {
                        self.handle_leave_spectator_operation(player_id, Some(operation_id))
                            .await;
                    }
                }
            }
            ClientMessage::TransportStatus {
                transport,
                connected,
            } => {
                self.handle_transport_status(player_id, transport, connected)
                    .await;
            }
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
        &self,
        player_id: &PlayerId,
        transport: crate::protocol::Transport,
        connected: bool,
    ) {
        let Some(lifecycle) = self.connection_manager.client_lifecycle(player_id) else {
            return;
        };
        #[cfg(test)]
        #[cfg(signal_fish_repository_tests)]
        if let Some(probe) = TRANSPORT_STATUS_LIFECYCLE_PROBES.get(player_id) {
            probe.store(true, Ordering::Release);
        }
        let _lifecycle_guard = lifecycle.lock().await;
        if lifecycle.player_id() != *player_id
            || !self
                .connection_manager
                .lifecycle_matches(player_id, &lifecycle)
        {
            return;
        }

        self.handle_transport_status_under_lifecycle(player_id, transport, connected)
            .await;
    }

    /// Process a transport report after the caller has fixed the connection
    /// identity and membership with its lifecycle guard.
    async fn handle_transport_status_under_lifecycle(
        &self,
        player_id: &PlayerId,
        transport: crate::protocol::Transport,
        connected: bool,
    ) {
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
                return;
            }
            TransportStatusUpdate::MissingConnection => {
                tracing::debug!(
                    %player_id,
                    ?transport,
                    connected,
                    "Ignoring TransportStatus for connection that no longer exists"
                );
                return;
            }
            TransportStatusUpdate::UnsupportedProtocolVersion => {
                tracing::debug!(
                    %player_id,
                    ?transport,
                    connected,
                    "Ignoring TransportStatus from a non-v3 connection (v3-only message)"
                );
                return;
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
                return;
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

        // Fan the accepted state change out to the sender's CURRENT room as
        // `PeerTransportStatus`, so peers learn e.g. that
        // the host's WebRTC path died and relay-path traffic should be
        // expected. Duplicate reports returned early above, so a fan-out fires
        // once per real state change in the current membership generation
        // (including its first report). No room ⇒ nothing to fan out — the
        // generation-scoped state was still recorded above.
        let Some(room_id) = self.get_client_room(player_id).await else {
            return;
        };

        // Keep membership and connection generations fixed while resolving and
        // dispatching this room-wide status event. The sender lifecycle lock is
        // already held, so this follows the same lifecycle -> room ordering as
        // join/leave/reconnect and prevents a stale database member or
        // replacement v2 connection from receiving a v3-only frame.
        let _room_event_guard = self
            .message_coordinator
            .lock_room_event_mutation(&room_id)
            .await;

        // Cheap non-consuming preflight before the fallible/O(room) membership
        // snapshot below. The consuming check still happens after recipient
        // resolution, immediately before dispatch, so failed lookups and empty
        // fan-outs do not burn a slot while already-over-budget clients cannot
        // keep forcing room scans.
        if self
            .rate_limiter
            .check_signal_available(player_id)
            .await
            .is_err()
        {
            tracing::debug!(
                %player_id,
                ?transport,
                connected,
                "Dropping TransportStatus fan-out: per-connection signal rate limit exceeded"
            );
            return;
        }

        // Resolve the exact live v3 recipients before charging the sender's
        // control-plane budget. Production exposes coordinator routing; the
        // database fallback keeps lightweight/distributed test coordinators
        // compatible without weakening the production source-route check.
        let recipients: Vec<PlayerId> = match self
            .message_coordinator
            .routed_player_ids(&room_id)
            .await
        {
            Ok(Some(routed)) => {
                if !routed.contains(player_id) {
                    tracing::debug!(%player_id, %room_id, "Skipping TransportStatus from an unrouted sender");
                    return;
                }
                routed
                    .into_iter()
                    .filter(|recipient| {
                        *recipient != *player_id && self.client_supports_v3(recipient)
                    })
                    .collect()
            }
            Ok(None) => match self.database.get_room_players(&room_id).await {
                Ok(members) => members
                    .into_iter()
                    .filter(|member| member.id != *player_id && self.client_supports_v3(&member.id))
                    .map(|member| member.id)
                    .collect(),
                Err(err) => {
                    tracing::warn!(
                        %player_id,
                        %room_id,
                        error = %err,
                        "Failed to load room members for PeerTransportStatus fan-out"
                    );
                    return;
                }
            },
            Err(err) => {
                tracing::warn!(
                    %player_id,
                    %room_id,
                    error = %err,
                    "Failed to resolve routed members for PeerTransportStatus fan-out"
                );
                return;
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
            return;
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
        if self.rate_limiter.check_signal(player_id).await.is_err() {
            tracing::debug!(
                %player_id,
                ?transport,
                connected,
                "Dropping TransportStatus fan-out: per-connection signal rate limit exceeded"
            );
            return;
        }

        let message = Arc::new(ServerMessage::PeerTransportStatus {
            peer_id: *player_id,
            transport,
            connected,
        });
        // Deliver to all peers concurrently: one slow room member costs this
        // event one slow-consumer window, never `(N - 1)` windows. Recipient
        // filtering above is v3-only but deliberately transport-agnostic: a
        // relay-only client still needs to know that a peer fell back.
        futures_util::future::join_all(recipients.iter().map(|recipient| {
            self.message_coordinator.send_to_player_in_room(
                recipient,
                &room_id,
                Arc::clone(&message),
            )
        }))
        .await;

        // One fan-out EVENT per accepted in-room state change — not per
        // recipient (see `ServerMetrics::record_transport_status_fanout`).
        self.metrics.record_transport_status_fanout();
    }
}
