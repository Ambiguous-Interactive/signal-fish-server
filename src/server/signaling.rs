//! Targeted WebRTC signal relay (Protocol v3, PLAN §P2/§P3).
//!
//! Relays opaque WebRTC signals (offer/answer/trickle-ICE) to a specific peer in
//! the same room ([`EnhancedGameServer::handle_signal`]) and brings a peer that
//! joins or reconnects into an **already-active** session up to date
//! ([`EnhancedGameServer::handle_active_session_late_join`]).
//!
//! Initial pairing for a freshly finalized lobby is delivered by the
//! per-recipient `SessionPlan` (see `session_policy.rs`). The late-join path
//! consults the room's **stored** `ActiveSessionPlan` — the session the room is
//! actually running — instead of re-running the selection ladder, because the
//! live membership can drift from the finalize-time membership and a recompute
//! could contradict the running session. With a stored (non-relay) plan and a
//! `Finalized` room:
//!
//! - the **joiner** receives a fresh tailored `SessionPlan` (current members,
//!   sticky topology/transport/host, fresh per-recipient ICE for WebRTC) and is
//!   deliberately **not** sent `NewPeer` — its pairing arrives in the plan's
//!   `peers[].initiate` flags. Plan peer lists are capability-filtered on both
//!   sides (`SessionPlanDecision::plan_for`): a joiner that cannot run the
//!   session's sticky pair gets an empty `peers` list (the relay floor is its
//!   data path);
//! - **existing members** receive only the additive `NewPeer` delta, and only
//!   when the stored transport is WebRTC (mesh announces the joiner to every
//!   member; host announces along the star edge only). Both the relay floor
//!   (which stores no plan) *and* a `Host + Direct` (LAN) session emit no
//!   `NewPeer` — even though `Host + Direct` is a non-relay *topology* — PLAN
//!   Appendix L decision #4, Appendix E. Per member, `NewPeer` applies the
//!   SAME full session predicate the plan peer lists use
//!   (`SessionPlanDecision::recipient_pairable`: v3 + the session's sticky
//!   topology AND transport) in both directions, so the server never instructs
//!   a pair its own plan contract excludes.
//!
//! A `host`-topology entry whose stored host is found invalid (missing, or
//! seated but no longer capable of the session) is self-healed first
//! (`replan_host_session` in `session_policy.rs`: capability-aware
//! re-election + a full re-plan to every member, joiner included), replacing the
//! per-joiner emission for that event.
//!
//! Every code path is gated on negotiated v3 (plus the WebRTC transport for
//! `Signal`, and the full session predicate above for `NewPeer` pairing) so v2
//! clients never observe `Signal`/`NewPeer`/`SessionPlan` (Appendix K).

use std::sync::Arc;

use crate::protocol::{
    room_state::Room, ErrorCode, LobbyState, PlayerId, PlayerInfo, ServerMessage, Topology,
    Transport,
};

use super::session_policy::SessionPlanDecision;
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

/// Canonical serialized JSON byte length of an opaque `signal` payload — the
/// measure `security.max_signal_bytes` caps.
///
/// `serde_json::to_vec` over a `serde_json::Value` can only fail for maps with
/// non-string keys, which `Value` cannot represent, so the error arm is purely
/// defensive: treat an unserializable payload as oversized rather than relay it.
pub(crate) fn canonical_signal_len(signal: &serde_json::Value) -> usize {
    serde_json::to_vec(signal).map_or(usize::MAX, |bytes| bytes.len())
}

impl EnhancedGameServer {
    /// Relay an opaque WebRTC signal from `from` to `to`, enforcing the P2
    /// security invariants (payload size cap, same room, negotiated WebRTC,
    /// rate limit, v3 target).
    pub async fn handle_signal(&self, from: &PlayerId, to: PlayerId, signal: serde_json::Value) {
        // 0. Payload size cap (PLAN Appendix I). Checked first because the cap
        //    is a property of the frame itself, independent of room/transport
        //    state, and rejecting before any lookup keeps oversized payloads
        //    maximally cheap. Size is the canonical serialized JSON byte
        //    length of the opaque `signal` value — the same bytes the relay
        //    would otherwise fan out.
        let payload_bytes = canonical_signal_len(&signal);
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

        // 4. Sender must have negotiated v3 + WebRTC. Deliberately
        //    transport-only (see the step-5 note below).
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
        //    the target never opted into (Appendix K). Both `handle_signal`
        //    gates (sender + target) are deliberately TRANSPORT-only, weaker
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

        // Count the signal as relayed here, at dispatch — after every same-room +
        // transport + rate-limit check has passed — matching the best-effort
        // "relayed = dispatched" semantics of the send below (a rejected
        // cross-room / rate-limited signal returns earlier and is never counted).
        self.metrics.increment_signals_relayed();

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

    /// Bring a joiner/reconnector into an **already-active** session: send the
    /// joiner its tailored `SessionPlan` for the session the room is running,
    /// then announce it to existing members via the `NewPeer` delta (Appendix E
    /// late-join rule).
    ///
    /// Initial pairing for a freshly finalized lobby is the `SessionPlan`'s job
    /// at finalize (`session_policy.rs`); this path only fires once the room is
    /// live, and it consults the **stored** `ActiveSessionPlan` rather than
    /// re-running the selection ladder (PLAN Appendix L decision #4):
    ///
    /// 1. The room must be `Finalized` — premature lobby-fill pairing is
    ///    suppressed (the `SessionPlan` delivers initial pairing at finalize).
    /// 2. The room must have a stored non-relay decision. A relay-floor (or
    ///    pre-v3) room stores none and emits nothing — even when a recompute
    ///    over the *current* members would now fit a richer rung, because the
    ///    running session is still relay (sticky for the session lifetime).
    /// 3. **Self-heal:** a `host`-topology decision whose stored host is
    ///    invalid — missing from the current members, or seated but no longer
    ///    capable of the session (`ActiveSessionPlan::host_invalid`) — is
    ///    repaired first, via the same capability-aware re-election + full
    ///    re-plan a host departure triggers (`replan_host_session`, one
    ///    `session_replans_emitted` event). The heal re-plan delivers EVERY
    ///    current member — including the joiner, even one that cannot run the
    ///    session itself (the heal is about the room; an incapable v3 joiner
    ///    still gets its plan with empty `peers`) — a fresh plan that already
    ///    lists any capable joiner with glare-correct `initiate` flags, so the
    ///    per-joiner plan and `NewPeer` deltas below are skipped as duplicates
    ///    (and the joiner is counted on the re-plan event, not
    ///    `session_plans_late_join`). If no member qualifies, the entry is
    ///    removed and nothing is emitted. A normal late join — stored host
    ///    present and capable — never re-plans.
    /// 4. The **joiner**, when v3, receives a fresh `SessionPlan` built from the
    ///    sticky topology/transport/host over the current member list, with
    ///    fresh per-recipient ICE when the transport is WebRTC (a reconnector's
    ///    original TURN credentials may have expired; a seat-filling joiner
    ///    never had any). Its pairing arrives in `peers[].initiate`, so the
    ///    joiner is deliberately **not** sent `NewPeer`. This holds for every
    ///    stored plan, including `Host + Direct` (which received plans at
    ///    finalize too). The plan's peer list is capability-filtered on both
    ///    sides (`SessionPlanDecision::plan_for`): a joiner that did not
    ///    negotiate the session's sticky pair (v3 relay-only, or v3 + WebRTC
    ///    transport without the session's topology) receives an empty `peers`
    ///    list and participates via the relay floor.
    /// 5. **Existing members** receive the additive `NewPeer` delta only when
    ///    the stored transport is **WebRTC** (`NewPeer` is a WebRTC-signaling
    ///    control message) and the joiner satisfies the full session predicate
    ///    (`SessionPlanDecision::recipient_pairable`: v3 + the sticky topology
    ///    AND transport — the same rule that shapes plan peer lists, so
    ///    existing members are never told to connect to a peer the plan would
    ///    not list): mesh announces the joiner to every session-capable member
    ///    (UUID glare rule); host announces along the star edge only (host
    ///    learns of a client joiner; clients learn of a host joiner; clients
    ///    never of each other). Members that cannot run the session are
    ///    skipped in BOTH directions — neither announced to nor announced.
    ///
    /// `members` is the room's current player list **including the joiner**,
    /// already fetched by the caller (`handle_join_room` / reconnect), avoiding
    /// a redundant `get_room_players` round-trip. `room` supplies `id`,
    /// `lobby_state`, and `authority_player` (for the self-heal re-election).
    pub async fn handle_active_session_late_join(
        &self,
        room: &Room,
        joiner: &PlayerId,
        members: &[PlayerInfo],
    ) {
        // 1. Only into an active (finalized) session; the SessionPlan owns
        //    finalize-time initial pairing, so lobby-fill joins emit nothing.
        if room.lobby_state != LobbyState::Finalized {
            return;
        }

        // 2. The session the room is RUNNING, not a recompute. No stored entry
        //    (relay floor / pre-v3) ⇒ emit nothing.
        let Some(stored) = self.active_session_plan(&room.id) else {
            return;
        };

        // 3. Self-heal (host-failover recovery): a `host`-topology entry whose
        //    stored host is invalid — no longer a member (an
        //    insert-after-departure race at finalize, concurrent departures, or
        //    a departure hook skipped by a transient storage error) or seated
        //    but no longer capable of the session (a capability-downgrading
        //    reconnect) — is repaired with the SAME capability-aware
        //    re-election + full re-plan a host departure triggers — so the
        //    joiner pairs against a live host, and can itself be elected if it
        //    qualifies. The heal runs regardless of the JOINER's own
        //    pairability (the heal is about the room, not the joiner: an
        //    incapable v3 joiner still receives the healed plan with empty
        //    `peers`, and is never `NewPeer`-announced). The heal re-plan
        //    already delivered every current member — `members` includes the
        //    joiner — a fresh plan listing any capable joiner with
        //    glare-correct `initiate` flags, so the separate joiner plan and
        //    the `NewPeer` deltas below would be duplicates: return instead.
        //    (If no member qualifies, the entry was removed and nothing was
        //    emitted — the session is over and the relay floor carries the
        //    room.) A normal late join, with the stored host present and
        //    capable, never re-plans.
        let session_members = self.session_members_from(members);
        if stored.host_invalid(&session_members) {
            self.replan_host_session(&room.id, stored, room.authority_player, session_members)
                .await;
            return;
        }

        // 4. Joiner-directed SessionPlan (v3-gated inside `send_session_plan_to`),
        //    sent BEFORE the NewPeer delta fires for existing members — and sent
        //    even to a joiner that cannot run the session (it receives its
        //    truthful empty-`peers` view; only the pairing below is gated on
        //    the full session predicate).
        let decision = stored.decision_with(session_members);
        let now_unix = decision
            .uses_webrtc_signaling()
            .then(|| chrono::Utc::now().timestamp());
        if let Some(minted) = self
            .send_session_plan_to(&decision, *joiner, now_unix)
            .await
        {
            self.metrics.increment_session_plans_late_join();
            self.metrics.add_turn_credentials_issued(minted);
        }

        // 5. `NewPeer` is a WebRTC-signaling control message: only announce when
        //    the active session actually uses the WebRTC transport (a relay or
        //    `Host + Direct` (LAN) session must never push clients into WebRTC
        //    negotiation) AND the joiner satisfies the full session predicate —
        //    v3 + the sticky topology AND transport, the same rule that filters
        //    plan peer lists (existing members must never be told to connect to
        //    a peer the plan itself would not list, e.g. a v3 joiner with the
        //    WebRTC transport but without the session's topology).
        if !decision.uses_webrtc_signaling() || !decision.recipient_pairable(*joiner) {
            return;
        }

        match decision.topology {
            // Unreachable: a WebRTC transport never pairs with a relay topology
            // (`is_valid_pair`). Kept for an exhaustive, future-proof match.
            Topology::Relay => {}
            // Mesh: announce the joiner to every other session-capable member.
            Topology::Mesh => {
                self.announce_webrtc_peer_to_members(&decision, joiner)
                    .await
            }
            // Host: announce along the star edge around the STORED host (the
            // host-failover re-election updates the stored entry, so an ex-host
            // reconnecting after a failover is announced as a client).
            Topology::Host => {
                let Some(host) = decision.host else {
                    // Defensive: unreachable after the self-heal gate above (a
                    // hostless host plan re-planned or was removed). Announce
                    // nothing rather than fabricate pairings.
                    return;
                };
                self.announce_webrtc_peer_in_star(&decision, joiner, host)
                    .await;
            }
        }
    }

    /// Whether this connection can participate in targeted WebRTC signaling
    /// (negotiated v3 + the WebRTC transport).
    ///
    /// This is `handle_signal`'s transport-level plumbing gate (Appendix K),
    /// deliberately WEAKER than the full session predicate
    /// (`SessionPlanDecision::recipient_pairable`: v3 + the session's sticky
    /// topology AND transport) that gates `NewPeer` pairing and plan peer
    /// lists — the relay forwards signals between any two transport-capable
    /// endpoints without second-guessing which pairs the plan brokered.
    pub(crate) fn supports_webrtc_signaling(&self, player_id: &PlayerId) -> bool {
        self.client_supports_v3(player_id)
            && self.client_supports_transport(player_id, Transport::WebRtc)
    }

    /// Announce a (re)joined peer to every other session-capable member of an
    /// active mesh session via `NewPeer`.
    ///
    /// One-directional by design: only the **existing** members are told about
    /// the joiner (`you_initiate` per the UUID glare rule, so exactly one side
    /// of each pair offers). The joiner itself is never sent `NewPeer` — its
    /// pairing (the same peers with the mirrored `initiate` flags) arrives in
    /// the tailored `SessionPlan` that
    /// [`Self::handle_active_session_late_join`] sends first.
    ///
    /// Gating is the full session predicate on BOTH sides
    /// (`SessionPlanDecision::pairable` / `recipient_pairable`: v3 + the
    /// session's sticky topology and transport — one rule shared with plan
    /// peer lists and host election), so a member the plan would never list
    /// (v2, v3 relay-only, or v3 lacking the session's topology) is neither
    /// announced nor announced to.
    pub(crate) async fn announce_webrtc_peer_to_members(
        &self,
        decision: &SessionPlanDecision,
        peer: &PlayerId,
    ) {
        if !decision.recipient_pairable(*peer) {
            return;
        }

        for member in &decision.members {
            let existing = member.player_id;
            if existing == *peer {
                continue;
            }
            // Members that cannot run this session never participate in
            // pairing (the same filter `plan_for` applies to peer lists).
            if !decision.pairable(member) {
                continue;
            }

            self.send_new_peer(&existing, peer, local_initiates(existing, *peer))
                .await;
        }
    }

    /// Announce a (re)joined peer along the star edge of an active
    /// `host`-topology session via `NewPeer`.
    ///
    /// One-directional by design (the joiner's own pairing arrives in its
    /// `SessionPlan`), and gated on the full session predicate on BOTH sides
    /// (see [`Self::announce_webrtc_peer_to_members`]):
    ///
    /// - joiner IS the host ⇒ every session-capable client is told to offer to
    ///   it (`you_initiate: true` — the star rule: clients offer, the host
    ///   answers);
    /// - joiner is a client ⇒ only the host (when present and session-capable)
    ///   is told to answer it (`you_initiate: false`). Clients are never
    ///   announced to each other in a star.
    pub(crate) async fn announce_webrtc_peer_in_star(
        &self,
        decision: &SessionPlanDecision,
        joiner: &PlayerId,
        host: PlayerId,
    ) {
        if !decision.recipient_pairable(*joiner) {
            return;
        }

        if *joiner == host {
            // The host (re)joined: every session-capable client offers to it.
            for member in &decision.members {
                let client = member.player_id;
                if client == host || !decision.pairable(member) {
                    continue;
                }
                self.send_new_peer(&client, &host, true).await;
            }
        } else {
            // A client (re)joined: the host answers it (the joiner's own
            // "offer to the host" instruction is in its SessionPlan).
            // `recipient_pairable` covers both host membership and capability.
            if decision.recipient_pairable(host) {
                self.send_new_peer(&host, joiner, false).await;
            }
        }
    }

    /// Best-effort `NewPeer { peer_id, you_initiate }` to one recipient.
    async fn send_new_peer(&self, to: &PlayerId, peer_id: &PlayerId, you_initiate: bool) {
        let _ = self
            .message_coordinator
            .send_to_player(
                to,
                Arc::new(ServerMessage::NewPeer {
                    peer_id: *peer_id,
                    you_initiate,
                }),
            )
            .await;
    }

    async fn reject_signal(
        &self,
        from: &PlayerId,
        to: PlayerId,
        message: impl Into<String>,
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
