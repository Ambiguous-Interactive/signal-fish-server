use crate::protocol::{ClientMessage, PlayerId};

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
    /// counters; the metrics below are emitted only for the first report or a
    /// real per-connection state transition.
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
    }
}
