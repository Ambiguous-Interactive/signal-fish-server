//! Targeted WebRTC signal relay (Protocol v3, PLAN §P2/§P3).
//!
//! Relays opaque WebRTC signals (offer/answer/trickle-ICE) to a specific peer in
//! the same room ([`EnhancedGameServer::handle_signal`]) and, when a peer joins
//! or reconnects into an **already-active** P2P session, designates exactly one
//! offerer per pair ([`EnhancedGameServer::handle_webrtc_late_join`]).
//!
//! Initial pairing for a freshly finalized lobby is delivered by the
//! per-recipient `SessionPlan` (see `session_policy.rs`); `NewPeer` is reserved
//! for the late-join / reconnect case. A room is "active" iff its
//! `lobby_state == Finalized` AND its recomputed plan is non-relay, so late join
//! is finalization-gated and topology-aware (mesh pairs every peer; host pairs
//! clients with the host only; relay-resolved rooms emit none — PLAN Appendix L
//! decision #4, Appendix E). Every code path is gated on negotiated v3 + WebRTC
//! capability so v2 clients never observe `Signal`/`NewPeer` (Appendix K).

use std::sync::Arc;

use crate::protocol::{
    room_state::Room, ErrorCode, LobbyState, PlayerId, PlayerInfo, ServerMessage, Topology,
    Transport,
};

use super::session_policy::choose_session_plan;
use super::EnhancedGameServer;

/// Glare-avoidance offerer designation (Appendix E mesh rule).
///
/// For a pair of peers, exactly one side must send the offer. The local peer
/// initiates iff its id sorts before the remote peer's id (UUID compare). This
/// is stateless and antisymmetric: `local_initiates(a, b) != local_initiates(b, a)`
/// for any `a != b`, and `local_initiates(x, x) == false`.
pub(crate) fn local_initiates(local: PlayerId, remote: PlayerId) -> bool {
    local < remote
}

impl EnhancedGameServer {
    /// Relay an opaque WebRTC signal from `from` to `to`, enforcing the P2
    /// security invariants (same room, negotiated WebRTC, rate limit, v3 target).
    pub async fn handle_signal(&self, from: &PlayerId, to: PlayerId, signal: serde_json::Value) {
        // 1. Sender must be in a room.
        let Some(from_room) = self.get_client_room(from).await else {
            self.reject_signal(
                from,
                to,
                "You are not in a room",
                ErrorCode::NotInRoom,
                "sender_not_in_room",
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
            )
            .await;
            return;
        };

        // 3. Same-room enforcement (PLAN invariant #6 / Appendix I).
        if from_room != to_room {
            self.reject_signal(
                from,
                to,
                "Cannot signal a peer in a different room",
                ErrorCode::CrossRoomSignal,
                "cross_room",
            )
            .await;
            return;
        }

        // 4. Sender must have negotiated v3 + WebRTC.
        if !self.supports_webrtc_signaling(from) {
            self.reject_signal(
                from,
                to,
                "WebRTC transport was not negotiated for this connection",
                ErrorCode::UnsupportedTransport,
                "sender_unsupported_transport",
            )
            .await;
            return;
        }

        // 5. Deliver only to a v3 + WebRTC target. A webrtc plan can never have
        //    chosen a peer that lacks the WebRTC transport, but enforce
        //    defense-in-depth: a sender that targets a v2 or v3-relay-only peer
        //    is told the target was not found rather than delivering a `Signal`
        //    the target never opted into. This mirrors the late-join path, which
        //    likewise requires BOTH v3 AND WebRTC for each peer (Appendix K).
        if !self.supports_webrtc_signaling(&to) {
            self.reject_signal(
                from,
                to,
                "Signal target does not support WebRTC signaling",
                ErrorCode::SignalTargetNotFound,
                "target_unsupported_transport",
            )
            .await;
            return;
        }

        // 6. Per-connection valid signal rate limit.
        if let Err(err) = self.rate_limiter.check_signal(from).await {
            self.send_signal_error(from, err.to_string(), ErrorCode::SignalRateLimited)
                .await;
            return;
        }

        // Best-effort delivery: `send_to_player` returns `Ok(())` even if the
        // target's channel is full or closed, so a backpressured peer may
        // silently drop this signal. That is acceptable for trickle-ICE (the
        // relay floor remains the fallback transport), so we deliberately
        // ignore the result here.
        let _ = self
            .message_coordinator
            .send_to_player(
                &to,
                Arc::new(ServerMessage::Signal {
                    from: *from,
                    signal,
                }),
            )
            .await;
    }

    /// Pair a joiner/reconnector into an **already-active** P2P session,
    /// designating exactly one offerer per pair (Appendix E late-join rule).
    ///
    /// Initial pairing for a freshly finalized lobby is the `SessionPlan`'s job
    /// (`session_policy.rs`); this path only fires once the room is live, so it is
    /// gated and topology-aware (PLAN Appendix L decision #4):
    ///
    /// 1. The joiner must have negotiated v3 + WebRTC.
    /// 2. The room must be `Finalized` — premature lobby-fill pairing is
    ///    suppressed (the `SessionPlan` delivers initial pairing at finalize).
    /// 3. The recomputed plan must use the **WebRTC transport**. `NewPeer` is a
    ///    WebRTC-signaling control message, so a non-WebRTC active session emits
    ///    none — both the relay floor *and* a `Host + Direct` (LAN) session, even
    ///    though `Host + Direct` is a non-relay *topology*.
    /// 4. The plan's **topology** then shapes the WebRTC pairing:
    ///    - **Mesh** ⇒ pair the joiner with every other WebRTC member (UUID glare
    ///      rule, exactly one offerer per pair).
    ///    - **Host** ⇒ star pairing around the elected host: the host pairs with
    ///      every client; a client pairs with the host only (clients never offer
    ///      to each other).
    ///
    /// `members` is the room's current player list, already fetched by the caller
    /// (`handle_join_room` / reconnect), avoiding a redundant `get_room_players`
    /// round-trip. `room` supplies `lobby_state`, `game_name`, and the explicit
    /// `authority_player` for host election.
    pub async fn handle_webrtc_late_join(
        &self,
        room: &Room,
        joiner: &PlayerId,
        members: &[PlayerInfo],
    ) {
        // 1. The joiner itself must be v3 + WebRTC.
        if !self.supports_webrtc_signaling(joiner) {
            return;
        }

        // 2. Only pair into an active (finalized) session; the SessionPlan owns
        //    finalize-time initial pairing, so lobby-fill joins emit nothing.
        if room.lobby_state != LobbyState::Finalized {
            return;
        }

        // 3. Recompute the room's plan over the identical inputs `emit_session_plan`
        //    uses.
        let decision = choose_session_plan(
            &room.game_name,
            room.authority_player,
            self.session_members_from(members),
            &self.session_config,
        );

        // 4. `NewPeer` is a WebRTC-signaling control message, so only pair when the
        //    active session actually uses the WebRTC transport. A relay-floor or
        //    `Host + Direct` (LAN) plan must never push clients into WebRTC
        //    negotiation — even though `Host + Direct` is a non-relay *topology*.
        //    This mirrors `emit_session_plan`, which advertises ICE only for a
        //    WebRTC transport.
        if !decision.uses_webrtc_signaling() {
            return;
        }

        // 5. The transport is WebRTC; the topology shapes the pairing.
        match decision.topology {
            // Unreachable: a WebRTC transport never pairs with a relay topology
            // (`is_valid_pair`). Kept for an exhaustive, future-proof match.
            Topology::Relay => {}
            // Mesh: every pair establishes exactly one offerer (UUID rule).
            Topology::Mesh => self.pair_webrtc_peer_with_members(joiner, members).await,
            // Host: star pairing around the elected host.
            Topology::Host => {
                let Some(host) = decision.host else {
                    // A host plan with no elected host is degenerate; emit nothing
                    // rather than fabricate pairings.
                    return;
                };
                self.pair_webrtc_peer_with_host(joiner, host, members).await;
            }
        }
    }

    /// Whether this connection can participate in targeted WebRTC signaling.
    pub(crate) fn supports_webrtc_signaling(&self, player_id: &PlayerId) -> bool {
        self.client_supports_v3(player_id)
            && self.client_supports_transport(player_id, Transport::WebRtc)
    }

    /// Pair one WebRTC-capable peer with every WebRTC-capable member in a room.
    pub(crate) async fn pair_webrtc_peer_with_members(
        &self,
        peer: &PlayerId,
        members: &[PlayerInfo],
    ) {
        if !self.supports_webrtc_signaling(peer) {
            return;
        }

        for member in members {
            let existing = member.id;
            if existing == *peer {
                continue;
            }
            // v2 / relay-only members never participate in signaling (gating).
            if !self.supports_webrtc_signaling(&existing) {
                continue;
            }

            self.send_new_peer_pair(peer, &existing).await;
        }
    }

    async fn send_new_peer_pair(&self, peer: &PlayerId, existing: &PlayerId) {
        // Tell the existing peer about the new/reconnected peer...
        let _ = self
            .message_coordinator
            .send_to_player(
                existing,
                Arc::new(ServerMessage::NewPeer {
                    peer_id: *peer,
                    you_initiate: local_initiates(*existing, *peer),
                }),
            )
            .await;
        // ...and the peer about the existing peer. Exactly one of the two
        // `you_initiate` flags is true (local_initiates is antisymmetric).
        let _ = self
            .message_coordinator
            .send_to_player(
                peer,
                Arc::new(ServerMessage::NewPeer {
                    peer_id: *existing,
                    you_initiate: local_initiates(*peer, *existing),
                }),
            )
            .await;
    }

    /// Star-pair a joiner into an active `host`-topology session.
    ///
    /// If the joiner IS the host it pairs with every (WebRTC-capable) client;
    /// otherwise it pairs only with the host. Direction is fixed by the star
    /// rule (Appendix E host): the client offers, the host answers.
    pub(crate) async fn pair_webrtc_peer_with_host(
        &self,
        joiner: &PlayerId,
        host: PlayerId,
        members: &[PlayerInfo],
    ) {
        if !self.supports_webrtc_signaling(joiner) {
            return;
        }

        if *joiner == host {
            // The host (re)joined: pair it with every WebRTC-capable client.
            for member in members {
                let client = member.id;
                if client == host || !self.supports_webrtc_signaling(&client) {
                    continue;
                }
                self.send_host_peer_pair(&client, &host).await;
            }
        } else {
            // A client (re)joined: pair it only with the host (if the host is
            // present and WebRTC-capable).
            if members.iter().any(|member| member.id == host)
                && self.supports_webrtc_signaling(&host)
            {
                self.send_host_peer_pair(joiner, &host).await;
            }
        }
    }

    /// Emit the `NewPeer` pair for a (client, host) edge in a star topology.
    ///
    /// Unlike [`Self::send_new_peer_pair`] (mesh, UUID glare rule), the direction
    /// is fixed: the client offers to the host (`you_initiate: true`) and the host
    /// answers (`you_initiate: false`).
    async fn send_host_peer_pair(&self, client: &PlayerId, host: &PlayerId) {
        // The client offers to the host.
        let _ = self
            .message_coordinator
            .send_to_player(
                client,
                Arc::new(ServerMessage::NewPeer {
                    peer_id: *host,
                    you_initiate: true,
                }),
            )
            .await;
        // The host answers the client.
        let _ = self
            .message_coordinator
            .send_to_player(
                host,
                Arc::new(ServerMessage::NewPeer {
                    peer_id: *client,
                    you_initiate: false,
                }),
            )
            .await;
    }

    async fn reject_signal(
        &self,
        from: &PlayerId,
        to: PlayerId,
        message: &'static str,
        error_code: ErrorCode,
        reason: &'static str,
    ) {
        tracing::debug!(
            %from,
            %to,
            %reason,
            ?error_code,
            "Rejected WebRTC signal"
        );

        match self.rate_limiter.check_signal_error(from).await {
            Ok(()) => self.send_signal_error(from, message, error_code).await,
            Err(err) => {
                self.send_signal_error(from, err.to_string(), ErrorCode::SignalRateLimited)
                    .await;
            }
        }
    }

    /// Send a signaling error to a player (thin wrapper over the shared helper).
    async fn send_signal_error(
        &self,
        player_id: &PlayerId,
        message: impl Into<String>,
        error_code: ErrorCode,
    ) {
        let _ = self
            .send_error_to_player(player_id, message.into(), Some(error_code))
            .await;
    }
}
