//! Targeted WebRTC signal relay (Protocol v3, PLAN §P2).
//!
//! Relays opaque WebRTC signals (offer/answer/trickle-ICE) to a specific peer
//! in the same room and designates exactly one offerer per pair on late join.
//! Every code path here is gated on negotiated v3 + WebRTC capability so v2
//! clients never observe `Signal`/`NewPeer` (Appendix K).

use std::sync::Arc;

use crate::protocol::{ErrorCode, PlayerId, PlayerInfo, ServerMessage, Transport};

use super::EnhancedGameServer;

/// Glare-avoidance offerer designation (Appendix E mesh rule).
///
/// For a pair of peers, exactly one side must send the offer. The local peer
/// initiates iff its id sorts before the remote peer's id (UUID compare). This
/// is stateless and symmetric: `local_initiates(a, b) != local_initiates(b, a)`
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
            self.send_signal_error(from, "You are not in a room", ErrorCode::NotInRoom)
                .await;
            return;
        };

        // 2. Target must be in a room.
        let Some(to_room) = self.get_client_room(&to).await else {
            self.send_signal_error(
                from,
                "Signal target is not in any room",
                ErrorCode::SignalTargetNotFound,
            )
            .await;
            return;
        };

        // Self-signal guard: a peer cannot WebRTC to itself. Reject before
        // consuming the rate-limit budget so a misbehaving client cannot burn
        // its quota looping signals back to itself.
        if *from == to {
            tracing::debug!(%from, "Ignoring self-targeted signal");
            self.send_signal_error(
                from,
                "Cannot signal yourself",
                ErrorCode::SignalTargetNotFound,
            )
            .await;
            return;
        }

        // 3. Same-room enforcement (PLAN invariant #6 / Appendix I).
        if from_room != to_room {
            self.send_signal_error(
                from,
                "Cannot signal a peer in a different room",
                ErrorCode::CrossRoomSignal,
            )
            .await;
            return;
        }

        // 4. Sender must have negotiated the WebRTC transport.
        if !self.client_supports_transport(from, Transport::WebRtc) {
            self.send_signal_error(
                from,
                "WebRTC transport was not negotiated for this connection",
                ErrorCode::UnsupportedTransport,
            )
            .await;
            return;
        }

        // 5. Per-connection signal rate limit.
        if let Err(err) = self.rate_limiter.check_signal(from).await {
            self.send_signal_error(from, err.to_string(), ErrorCode::SignalRateLimited)
                .await;
            return;
        }

        // 6. Deliver only to a v3 + WebRTC target. A webrtc plan can never have
        //    chosen a peer that lacks the WebRTC transport, but enforce
        //    defense-in-depth: a sender that targets a v2 or v3-relay-only peer
        //    is told the target was not found rather than delivering a `Signal`
        //    the target never opted into. This mirrors the late-join path, which
        //    likewise requires BOTH v3 AND WebRTC for each peer (Appendix K).
        if !self.client_supports_v3(&to) || !self.client_supports_transport(&to, Transport::WebRtc)
        {
            self.send_signal_error(
                from,
                "Signal target does not support WebRTC signaling",
                ErrorCode::SignalTargetNotFound,
            )
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

    /// On a successful room join, pair the joiner with every existing WebRTC peer
    /// so each pair establishes exactly one offerer (Appendix E late-join rule).
    ///
    /// P2 gates this purely on negotiated WebRTC capability. P3 will refine the
    /// trigger to consult the chosen per-room SessionPlan (mesh/host/relay) — see
    /// PLAN §P3 — so that, e.g., a relay-only room emits no `NewPeer`.
    ///
    /// `members` is the room's current player list, already fetched by the
    /// caller (`handle_join_room`). Passing it in avoids a redundant
    /// `get_room_players` round-trip to the database.
    pub async fn handle_webrtc_late_join(&self, joiner: &PlayerId, members: &[PlayerInfo]) {
        // The joiner itself must be v3 + WebRTC capable; otherwise nothing to do.
        if !self.client_supports_v3(joiner)
            || !self.client_supports_transport(joiner, Transport::WebRtc)
        {
            return;
        }

        for member in members {
            let existing = member.id;
            if existing == *joiner {
                continue;
            }
            // v2 / relay-only members never participate in signaling (gating).
            if !self.client_supports_v3(&existing)
                || !self.client_supports_transport(&existing, Transport::WebRtc)
            {
                continue;
            }

            // Tell the existing peer about the joiner...
            let _ = self
                .message_coordinator
                .send_to_player(
                    &existing,
                    Arc::new(ServerMessage::NewPeer {
                        peer_id: *joiner,
                        you_initiate: local_initiates(existing, *joiner),
                    }),
                )
                .await;
            // ...and the joiner about the existing peer. Exactly one of the two
            // `you_initiate` flags is true (local_initiates is antisymmetric).
            let _ = self
                .message_coordinator
                .send_to_player(
                    joiner,
                    Arc::new(ServerMessage::NewPeer {
                        peer_id: existing,
                        you_initiate: local_initiates(*joiner, existing),
                    }),
                )
                .await;
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
