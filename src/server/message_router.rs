use std::sync::Arc;

use crate::protocol::{ClientMessage, PlayerId, ServerMessage};

use super::{EnhancedGameServer, TransportStatusUpdate};

impl EnhancedGameServer {
    /// Handle incoming client message with enhanced coordination.
    pub async fn handle_client_message(&self, player_id: &PlayerId, message: ClientMessage) {
        match message {
            ClientMessage::Authenticate { app_id, .. } => {
                tracing::warn!(
                    %player_id,
                    %app_id,
                    "Received Authenticate message after connection established - this should not happen. \
                     Authentication must occur during WebSocket handshake."
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
            ClientMessage::GameData { data } => {
                self.handle_game_data(player_id, data).await;
            }
            ClientMessage::Signal { to, signal } => {
                self.handle_signal(player_id, to, signal).await;
            }
            ClientMessage::AuthorityRequest { become_authority } => {
                self.handle_authority_request(player_id, become_authority)
                    .await;
            }
            ClientMessage::PlayerReady => {
                self.handle_player_ready(player_id).await;
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
            ClientMessage::TransportStatus {
                transport,
                connected,
            } => {
                self.handle_transport_status(player_id, transport, connected)
                    .await;
            }
        }
    }

    /// Record a client's reported data-path transport state (Protocol v3, PLAN §P5).
    ///
    /// Purely informational and v3-only: a v2 client can never legitimately send
    /// this, and a v3 report is accepted only for a transport negotiated by that
    /// connection. Invalid reports are ignored (debug-logged) as defense-in-depth
    /// (Appendix K). The relay floor never closes regardless of what is reported
    /// — this only drives observability and, in future, targeted relay for stuck
    /// peers.
    ///
    /// Duplicate reports of the same `(transport, connected)` pair update no
    /// counters and fan nothing out; the metrics and the `PeerTransportStatus`
    /// fan-out below are emitted only for the first report or a real
    /// per-connection state transition.
    ///
    /// Metric interpretation:
    /// - `connected == true` AND a P2P transport (`Direct` / `WebRtc`) ⇒
    ///   `record_p2p_established` (a peer-to-peer path came up).
    /// - `connected == false` ⇒ `record_relay_fallback` (the client dropped back to
    ///   the relay floor), regardless of which transport it names.
    /// - `connected == true` with `transport: relay` is just "I am on the floor":
    ///   it is not a P2P establishment and not a fallback event, so it moves no
    ///   counter — only the per-connection state is updated. (Documented here and in
    ///   `docs/architecture/transport-fallback.md`.)
    async fn handle_transport_status(
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
        // `PeerTransportStatus` (PLAN §P5 refinement), so peers learn e.g. that
        // the host's WebRTC path died and relay-path traffic should be
        // expected. Duplicate reports returned early above, so a fan-out fires
        // once per real per-connection state change (including the first
        // report). No room ⇒ nothing to fan out — the per-connection state was
        // still recorded above.
        let Some(room_id) = self.get_client_room(player_id).await else {
            return;
        };

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

        let members = match self.database.get_room_players(&room_id).await {
            Ok(members) => members,
            Err(err) => {
                tracing::warn!(
                    %player_id,
                    %room_id,
                    error = %err,
                    "Failed to load room members for PeerTransportStatus fan-out"
                );
                return;
            }
        };

        // Resolve the exact v3 recipients before charging the sender's
        // control-plane budget. A failed membership lookup, sender-only room,
        // or room with only legacy recipients is not a fan-out event.
        let recipients: Vec<PlayerId> = members
            .iter()
            .filter_map(|member| {
                if member.id != *player_id && self.client_supports_v3(&member.id) {
                    Some(member.id)
                } else {
                    None
                }
            })
            .collect();

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
        for recipient in recipients {
            // Deliver only to peers that negotiated v3 (defense-in-depth,
            // Appendix K — the same per-recipient guard `Signal` / `NewPeer` /
            // `SessionPlan` apply: a v2 member must never observe a v3-only
            // message). Deliberately NOT gated on the recipient's own transport
            // capabilities — weaker than the full session-pairing predicate
            // (`SessionPlanDecision::recipient_pairable`) — because this is
            // informational status about a PEER's data path, useful to any v3
            // client (a relay-only member still wants to know the host fell
            // back to the relay), not an instruction to use that transport. The
            // filtering happened above before the sender's budget was charged.
            // Best-effort delivery, mirroring `Signal` / `NewPeer`: a
            // backpressured peer may miss the notice; the relay floor (and the
            // next state change) is unaffected.
            let _ = self
                .message_coordinator
                .send_to_player(&recipient, Arc::clone(&message))
                .await;
        }

        // One fan-out EVENT per accepted in-room state change — not per
        // recipient (see `ServerMetrics::record_transport_status_fanout`).
        self.metrics.record_transport_status_fanout();
    }
}
