//! Per-room session-plan selection, emission, and mid-session re-planning
//! (Protocol v3).
//!
//! At lobby finalization the server picks a single room-wide plan from the
//! intersection of every member's negotiated capabilities, then hands each v3
//! member a *per-recipient* [`SessionPlanPayload`] whose peer list and
//! `initiate` flags are tailored to that recipient by the deterministic offerer
//! rule. A room that resolves to the relay floor stores no sticky plan but still
//! emits an explicit Relay/Relay plan to v3 members so finalization has one
//! authoritative pairing result. Finalized-room joins and reconnects use the
//! same reset; v2 bytes remain frozen.
//!
//! A non-relay decision is also recorded as the room's sticky
//! [`ActiveSessionPlan`], the single source of truth for the session the room is
//! actually running. Finalized join/reconnect refreshes consult it instead of
//! re-running the ladder, and whenever a membership-touching event (a departure
//! via [`EnhancedGameServer::handle_session_member_departure`], or a late join /
//! reconnect) finds a `host`-topology entry whose stored host is invalid — no
//! longer a member, or seated but no longer capable of the session
//! ([`ActiveSessionPlan::host_invalid`]) — execution-aware repair re-elects a
//! host and re-emits fresh per-recipient `SessionPlan`s. Topology and
//! transport are **sticky for the session lifetime**: the ladder runs once at
//! finalize and is never re-run mid-session, even though the capability
//! intersection can only widen when members depart — a mid-game data-path
//! migration would disrupt gameplay for zero correctness gain.
//!
//! The selection core ([`choose_session_plan`], [`elect_host`],
//! [`SessionPlanDecision::plan_for`]) is pure and unit-testable; the thin
//! [`EnhancedGameServer::emit_session_plan`] method gathers per-member
//! capabilities, runs the core, and best-effort delivers the result — gated on
//! v3 so a v2 client never observes a `SessionPlan` (the per-recipient v3 gate).
//!
//! This module also hosts the **ICE pre-gather** seam (the deferred
//! "RoomJoined ICE pre-gather" refinement): the pure [`ice_pregather_eligible`]
//! gate plus [`EnhancedGameServer::pregather_ice_servers`], which surfaces the
//! same composed ICE list ([`EnhancedGameServer::composed_ice_servers_for`] —
//! the single composition site shared with `SessionPlan` delivery) on
//! `RoomJoined` / `Reconnected` so v3 WebRTC-capable clients can gather
//! candidates during the lobby wait. The `SessionPlan` ICE list supersedes it.

use std::collections::HashSet;
use std::sync::Arc;

use crate::config::SessionConfig;
#[cfg(test)]
#[cfg(signal_fish_repository_tests)]
use crate::coordination::FinalizedRoom;
use crate::coordination::{
    RoomEventMutationGuard, RoomMessageTransactionOutcome, RoomRecipientMessages,
    StartGamePublication, StartGamePublicationBuilder,
};
use crate::protocol::{
    ConnectionInfo, DirectEndpoint, IceServer, LobbyState, PlayerId, PlayerInfo, Room, RoomId,
    ServerMessage, SessionGeneration, SessionPeer, SessionPlanPayload, Topology, Transport,
};

use super::signaling::local_initiates;
use super::{EnhancedGameServer, NegotiatedProtocol};

/// The sticky per-room session decision recorded when a room finalizes to a
/// non-relay plan. Relay-floor rooms record nothing; a later reconnect derives
/// its explicit Relay/Relay refresh from that absence rather than sticky state.
///
/// This is what the room is *actually running*: late-join / reconnect pairing
/// and departure re-planning consult it rather than re-running
/// [`choose_session_plan`] over the current member list, because the live
/// membership can drift from the finalize-time membership (departures open
/// seats; reconnects can re-seat an incumbent with downgraded capabilities)
/// and a recompute could contradict the session every member already
/// configured. New seat-fills cannot drift the membership: the admission gate
/// ([`joiner_supports_sticky_plan`], issue #421) rejects joiners that cannot
/// run the stored pair.
///
/// Topology and transport are immutable for the session lifetime; only `host`
/// is rewritten (by host failover re-election when the host departs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActiveSessionPlan {
    pub topology: Topology,
    pub transport: Transport,
    pub host: Option<PlayerId>,
}

impl ActiveSessionPlan {
    /// Rehydrate a full [`SessionPlanDecision`] from this sticky decision plus
    /// the room's *current* member list. The ladder is **not** re-run — the
    /// stored topology/transport/host shape per-recipient plans for finalized
    /// joins, reconnects, and departure re-planning.
    pub(crate) fn decision_with(self, members: Vec<SessionMember>) -> SessionPlanDecision {
        SessionPlanDecision {
            generation: SessionGeneration::new_v4(),
            topology: self.topology,
            transport: self.transport,
            host: self.host,
            members,
        }
    }

    /// Whether this is a `host`-topology decision whose stored host can no
    /// longer anchor the session: no host was recorded, the recorded host is no
    /// longer in `members`, or the recorded host is still a member but no
    /// longer satisfies [`Self::supported_by`] (reachable only through a
    /// reconnect-with-downgraded-capabilities race — departure re-planning
    /// never fires for a member that stays seated, so without this arm such a
    /// host would wedge the room forever). This is the wedge state both
    /// membership-touching events repair before publication: left alone, every
    /// plan the room hands out would point at a host that cannot answer.
    pub(crate) fn host_invalid(&self, members: &[SessionMember]) -> bool {
        self.topology == Topology::Host
            && !self.host.is_some_and(|host| {
                members
                    .iter()
                    .any(|member| member.player_id == host && self.supported_by(member))
            })
    }

    /// Whether `member` negotiated everything required to participate in — and
    /// in particular to *host* — this stored session: protocol v3 plus the
    /// sticky (topology, transport) pair, and a usable endpoint for Direct.
    fn supported_by(&self, member: &SessionMember) -> bool {
        member.can_host(self.topology, self.transport)
    }
}

/// Per-member input to session selection: identity + negotiated capabilities +
/// join time (for deterministic host election).
///
/// `Debug` is required by the proptest strategies in `session_policy_tests.rs`
/// (generated values must be printable in failure reports).
#[derive(Debug, Clone)]
pub(crate) struct SessionMember {
    pub player_id: PlayerId,
    pub player_name: String,
    pub is_authority: bool,
    pub joined_at: chrono::DateTime<chrono::Utc>,
    pub connection_info: Option<ConnectionInfo>,
    pub version: u16,
    pub transports: Vec<Transport>,
    pub topologies: Vec<Topology>,
}

impl SessionMember {
    /// Whether this member negotiated protocol v3 or higher.
    fn supports_v3(&self) -> bool {
        self.version >= 3
    }

    /// Whether this member negotiated the given data-path transport.
    fn supports_transport(&self, transport: Transport) -> bool {
        self.transports.contains(&transport)
    }

    /// Whether this member negotiated the given session topology.
    fn supports_topology(&self, topology: Topology) -> bool {
        self.topologies.contains(&topology)
    }

    /// Whether this member negotiated everything required to run a session on
    /// the given (topology, transport) pair: protocol v3 plus both axes.
    ///
    /// The capability predicate shared by plan selection ([`all_support`]) and
    /// per-recipient peer-list filtering ([`SessionPlanDecision::plan_for`]).
    /// Host validity and election wrap this with [`Self::can_host`], which adds
    /// Direct endpoint readiness.
    fn supports_session(&self, topology: Topology, transport: Transport) -> bool {
        self.supports_v3() && self.supports_topology(topology) && self.supports_transport(transport)
    }

    /// Validated direct endpoint this member can expose when elected host.
    fn direct_endpoint(&self) -> Option<DirectEndpoint> {
        self.connection_info
            .as_ref()
            .and_then(DirectEndpoint::from_connection_info)
    }

    /// Whether this member can anchor the selected host transport. Direct
    /// requires a usable endpoint in addition to negotiated capabilities;
    /// WebRTC carries its own connection establishment through signaling.
    fn can_host(&self, topology: Topology, transport: Transport) -> bool {
        self.supports_session(topology, transport)
            && (transport != Transport::Direct || self.direct_endpoint().is_some())
    }
}

/// The room-wide selection result (not yet specialized per recipient).
///
/// Carries the canonical member list so [`SessionPlanDecision::plan_for`] can
/// build each recipient's tailored peer list. The ICE servers are intentionally
/// **not** stored here: they are per-recipient (each WebRTC member receives its
/// own freshly minted TURN credentials), so the emit site builds them and passes
/// them into [`SessionPlanDecision::plan_for`].
pub(crate) struct SessionPlanDecision {
    pub generation: SessionGeneration,
    pub topology: Topology,
    pub transport: Transport,
    pub host: Option<PlayerId>,
    pub members: Vec<SessionMember>,
}

/// Pure resolution of the session plan every member must observe after a
/// membership publication. State and metrics are deliberately deferred to the
/// caller's exact-routing transaction hook.
pub(crate) struct MembershipSessionDecision {
    pub decision: SessionPlanDecision,
    /// `None`: keep stored state; `Some(Some(_))`: replace it;
    /// `Some(None)`: remove an unrecoverable host plan.
    pub active_plan_update: Option<Option<ActiveSessionPlan>>,
    pub is_replan: bool,
}

pub(crate) fn membership_session_decision(
    stored: Option<ActiveSessionPlan>,
    authority: Option<PlayerId>,
    members: Vec<SessionMember>,
) -> MembershipSessionDecision {
    let Some(stored) = stored else {
        return MembershipSessionDecision {
            decision: SessionPlanDecision {
                generation: SessionGeneration::new_v4(),
                topology: Topology::Relay,
                transport: Transport::Relay,
                host: None,
                members,
            },
            active_plan_update: None,
            is_replan: false,
        };
    };

    if !stored.host_invalid(&members) {
        return MembershipSessionDecision {
            decision: stored.decision_with(members),
            active_plan_update: None,
            is_replan: false,
        };
    }

    let electable: Vec<_> = members
        .iter()
        .filter(|member| stored.supported_by(member))
        .cloned()
        .collect();
    if let Some(host) = elect_host(authority, &electable) {
        let updated = ActiveSessionPlan {
            host: Some(host),
            ..stored
        };
        MembershipSessionDecision {
            decision: updated.decision_with(members),
            active_plan_update: Some(Some(updated)),
            is_replan: true,
        }
    } else {
        MembershipSessionDecision {
            decision: SessionPlanDecision {
                generation: SessionGeneration::new_v4(),
                topology: Topology::Relay,
                transport: Transport::Relay,
                host: None,
                members,
            },
            active_plan_update: Some(None),
            is_replan: false,
        }
    }
}

/// Whether *every* member supports v3 and the given (topology, transport) pair.
///
/// An empty room never supports an upgrade (there is nothing to connect).
fn all_support(members: &[SessionMember], topology: Topology, transport: Transport) -> bool {
    !members.is_empty()
        && members
            .iter()
            .all(|member| member.supports_session(topology, transport))
}

/// Whether a joiner's negotiated capabilities satisfy a stored sticky session
/// — the seat-fill admission predicate (issue #421).
///
/// A finalized non-relay session runs one (topology, transport) pair every
/// finalize-time member negotiated (`all_support`). Admitting a joiner that
/// cannot run that pair would silently split the room's data path: the
/// joiner's WebSocket-relayed `GameData` reaches everyone, but the capable
/// members' peer-to-peer traffic never reaches it ([`SessionPlanDecision::plan_for`]
/// filters both sides), so it would play on permanently divergent state. The
/// admission gate therefore applies the same membership predicate selection
/// used at finalize: protocol v3 plus both axes. Direct endpoints are NOT part
/// of this predicate (only an elected host needs one), matching
/// [`SessionMember::supports_session`].
///
/// Relay-floored rooms store no sticky plan (`emit_session_plan` removes the
/// entry), so they never reach this check: the universal floor admits anyone.
pub(crate) fn joiner_supports_sticky_plan(
    protocol: &NegotiatedProtocol,
    stored: &ActiveSessionPlan,
) -> bool {
    protocol.version >= 3
        && protocol.topologies.contains(&stored.topology)
        && protocol.transports.contains(&stored.transport)
}

/// Whether a capability-compatible rung has every piece needed to execute it.
/// Only `host + direct` has an additional requirement: at least one capable
/// member must have provided a syntactically usable connect endpoint.
fn rung_is_executable(members: &[SessionMember], topology: Topology, transport: Transport) -> bool {
    transport != Transport::Direct
        || members
            .iter()
            .any(|member| member.can_host(topology, transport))
}

/// The session-upgrade ladder (ADR-0001 §1), richest rung first.
///
/// Each entry is a legal `(topology, transport)` pair the room may settle on, in
/// strict preference order. [`choose_session_plan`] selects the first rung that
/// fits the room's `desired` ceiling, has its transport enabled, and is supported
/// by every member. Holding the ladder in one constant makes it the single source
/// of truth shared by the selector, its doc comment, the CHANGELOG, and the
/// selection tests — `ladder_is_the_documented_adr_waterfall` fails on any drift.
pub(crate) const UPGRADE_LADDER: [(Topology, Transport); 3] = [
    (Topology::Mesh, Transport::WebRtc),
    (Topology::Host, Transport::WebRtc),
    (Topology::Host, Transport::Direct),
];

/// The universal floor: server WebSocket relay, always available (ADR-0001 §3).
///
/// Selected only when no [`UPGRADE_LADDER`] rung fits. Each v3 member receives an
/// explicit no-peer `SessionPlan` for this pair; v2 members remain plan-free.
pub(crate) const RELAY_FLOOR: (Topology, Transport) = (Topology::Relay, Transport::Relay);

/// Richness rank of a topology ceiling: `Relay < Host < Mesh` (ADR-0001 §1).
///
/// `desired` is a *ceiling*, so a room may settle on any topology whose rank is
/// `<=` the desired rank (a mesh-preferring room can fall back to host). Written
/// as an explicit match — not the wire enum's declaration order — so reordering
/// [`Topology`] variants can never silently reshape selection.
const fn topology_rank(topology: Topology) -> u8 {
    match topology {
        Topology::Relay => 0,
        Topology::Host => 1,
        Topology::Mesh => 2,
    }
}

/// Whether `cfg` permits the given data-path transport. `Relay` is the mandatory
/// floor (always permitted); `Direct` and `WebRtc` are opt-in upgrade gates.
const fn transport_enabled(cfg: &SessionConfig, transport: Transport) -> bool {
    match transport {
        Transport::Relay => true,
        Transport::Direct => cfg.enable_direct,
        Transport::WebRtc => cfg.enable_webrtc,
    }
}

/// The desired (ceiling) topology for `game_name`: the per-game override when
/// mapped, else the configured default (the session-selection `desired` lookup).
///
/// Shared by the ladder walk in [`choose_session_plan`] and the ICE pre-gather
/// gate ([`ice_pregather_eligible`]) so "what does this game want" is one rule
/// everywhere.
#[must_use]
pub(crate) fn desired_topology_for(game_name: &str, cfg: &SessionConfig) -> Topology {
    cfg.game_topology_mappings
        .get(game_name)
        .copied()
        .unwrap_or(cfg.default_topology)
}

/// Whether a joiner/reconnector should receive the ICE pre-gather list on its
/// `RoomJoined` / `Reconnected` payload (the deferred "RoomJoined ICE
/// pre-gather" refinement). Pure over plain data so the full gating matrix is
/// unit-testable without a server. Eligible iff ALL of:
///
/// - `session.enable_ice_pregather` (the operator kill switch), and
/// - `session.enable_webrtc` (with WebRTC disabled no ladder rung can ever
///   select a WebRTC plan, so pre-gathered candidates could never be used), and
/// - the game's desired topology is non-relay (a relay-desired game can never
///   select a WebRTC plan — the selection ladder caps at the desired ceiling —
///   so minting for it would hand out TURN credentials that can never be
///   used), and
/// - the room is **not** `Finalized`: a finalized room either runs a stored
///   non-relay plan — the late-join/reconnect path already delivers a fresh
///   per-recipient `SessionPlan` with fresh ICE immediately after, so
///   pre-gather would double-mint — or was floored to relay (sticky for the
///   session lifetime — WebRTC can never start, pre-gather is pointless), and
/// - the recipient negotiated protocol v3 (`version >= 3`, exactly the
///   [`EnhancedGameServer::client_supports_v3`] check: v2 wire
///   stays byte-identical), and
/// - the recipient negotiated the WebRTC transport (a relay/direct-only client
///   never runs ICE), and
/// - the recipient's negotiated topologies contain the game's desired topology
///   (the relay-desired "credentials that can never be used" argument applied
///   per-recipient: the selection ladder seats a member on a rung only when
///   that member negotiated the rung's topology, so a relay-only-topology
///   recipient can never appear in *any* WebRTC plan and minting for it would
///   hand out live TURN credentials that can never be used. A recipient that
///   negotiated only a rung below the desired one forfeits the head start —
///   the finalize-time `SessionPlan` still delivers its ICE if the whole room
///   settles there).
#[must_use]
pub(crate) fn ice_pregather_eligible(
    cfg: &SessionConfig,
    game_name: &str,
    lobby_state: &LobbyState,
    recipient: &NegotiatedProtocol,
) -> bool {
    let desired = desired_topology_for(game_name, cfg);
    cfg.enable_ice_pregather
        && cfg.enable_webrtc
        && desired != Topology::Relay
        && *lobby_state != LobbyState::Finalized
        && recipient.version >= 3
        && recipient.transports.contains(&Transport::WebRtc)
        && recipient.topologies.contains(&desired)
}

/// Whether `(topology, transport)` is one of the four legal session pairs — the
/// three [`UPGRADE_LADDER`] rungs plus the [`RELAY_FLOOR`].
///
/// Every other combination (e.g. `Mesh + Direct`, `Host + Relay`) is illegal and
/// must never reach a client: downstream consumers — late-join WebRTC pairing, ICE
/// emission, the relay-floor short-circuit — rely on this topology/transport
/// coupling. The exhaustive `selection_only_ever_yields_a_legal_pair`
/// invariant test protects the ladder and fallback constants.
#[must_use]
#[cfg(test)]
#[cfg(signal_fish_repository_tests)]
pub(crate) fn is_valid_pair(topology: Topology, transport: Transport) -> bool {
    UPGRADE_LADDER
        .into_iter()
        .chain(std::iter::once(RELAY_FLOOR))
        .any(|rung| rung == (topology, transport))
}

/// Choose the room-wide session plan from member capabilities (ADR-0001).
///
/// `desired` (the per-game override, else the configured default) is a *ceiling*,
/// not an exact match: the room settles on the richest [`UPGRADE_LADDER`] rung that
/// is **no richer than `desired`**, has its transport enabled, and is supported by
/// *every* member. A rung fails when any single member lacks its topology/transport
/// (or the transport is disabled); the walk then continues to the next rung, reaching
/// the universal [`RELAY_FLOOR`] only when no rung fits. With the richest-first ladder
/// that is exactly (ADR-0001 §1):
///
/// 1. `Mesh` + WebRTC — `desired == Mesh`, webrtc enabled, all support mesh+webrtc.
/// 2. `Host` + WebRTC — `desired ∈ {Mesh, Host}`, webrtc enabled, all support host+webrtc.
/// 3. `Host` + Direct — `desired ∈ {Mesh, Host}`, direct enabled, all support
///    host+direct, and at least one electable host has a validated endpoint (LAN).
/// 4. `Relay` + Relay — the universal floor (always available).
///
/// So a `Mesh`-preferring room that cannot run mesh still falls back to a host
/// topology before the relay floor, instead of collapsing straight to relay.
///
/// `authority` is the room's explicitly designated authority player (e.g.
/// `Room::authority_player`); under `host` topology it is preferred as the host
/// when present in `members`. The `members` vector is consumed and moved into the
/// returned [`SessionPlanDecision`] (no clone).
#[must_use]
pub(crate) fn choose_session_plan(
    game_name: &str,
    authority: Option<PlayerId>,
    members: Vec<SessionMember>,
    cfg: &SessionConfig,
) -> SessionPlanDecision {
    let desired = desired_topology_for(game_name, cfg);

    // Walk the richest-first ladder and settle on the first rung that fits the
    // `desired` ceiling, has its transport enabled, and is supported by *every*
    // member; otherwise fall to the universal relay floor (ADR-0001 §1/§3).
    let (topology, transport) = UPGRADE_LADDER
        .into_iter()
        .find(|&(topology, transport)| {
            topology_rank(topology) <= topology_rank(desired)
                && transport_enabled(cfg, transport)
                && all_support(&members, topology, transport)
                && rung_is_executable(&members, topology, transport)
        })
        .unwrap_or(RELAY_FLOOR);

    let host = if topology == Topology::Host {
        let electable: Vec<_> = members
            .iter()
            .filter(|member| member.can_host(topology, transport))
            .cloned()
            .collect();
        elect_host(authority, &electable)
    } else {
        None
    };

    SessionPlanDecision {
        generation: SessionGeneration::new_v4(),
        topology,
        transport,
        host,
        members,
    }
}

/// Elect the host for a `host`-topology room.
///
/// Prefers the explicitly designated `authority` *if that id is present in
/// `members`*; otherwise the earliest joiner (ties broken by the smaller
/// `player_id` for determinism); `None` for an empty room.
#[must_use]
pub(crate) fn elect_host(
    authority: Option<PlayerId>,
    members: &[SessionMember],
) -> Option<PlayerId> {
    authority
        .filter(|id| members.iter().any(|member| member.player_id == *id))
        .or_else(|| {
            members
                .iter()
                .min_by(|a, b| {
                    a.joined_at
                        .cmp(&b.joined_at)
                        .then(a.player_id.cmp(&b.player_id))
                })
                .map(|member| member.player_id)
        })
}

impl SessionPlanDecision {
    /// Whether the room resolved to the universal relay floor.
    ///
    /// Whether this decision is the relay floor. Finalization, Finalized-room
    /// joins, and reconnects serialize it as an explicit authoritative reset
    /// for v3 members. Equivalent to the
    /// transport being [`Transport::Relay`]: the floor is the only legal relay
    /// pairing ([`is_valid_pair`]).
    ///
    /// This is **not** the same gate as [`Self::uses_webrtc_signaling`]: a
    /// `Host + Direct` plan is non-relay (so it *does* receive a `SessionPlan`)
    /// yet is non-WebRTC (so it permits no `Signal`). The two gates'
    /// truth table over the four legal pairs is pinned by the
    /// `emission_gates_track_relay_topology_and_webrtc_transport` test.
    #[must_use]
    pub(crate) fn is_relay(&self) -> bool {
        self.topology == Topology::Relay
    }

    /// Whether this plan uses server-mediated WebRTC signaling.
    ///
    /// This is the gate for server-mediated `Signal` control traffic: it is true
    /// **iff** the chosen transport is [`Transport::WebRtc`]. A `Host + Direct`
    /// (LAN) plan is non-relay yet is not WebRTC. The truth table versus
    /// [`Self::is_relay`] is pinned by the
    /// `emission_gates_track_relay_topology_and_webrtc_transport` test.
    #[must_use]
    pub(crate) fn uses_webrtc_signaling(&self) -> bool {
        self.transport == Transport::WebRtc
    }

    /// Whether `member` negotiated everything required to participate in this
    /// plan's P2P pairing (v3 + this decision's (topology, transport) pair).
    /// Host election deliberately uses the stricter [`SessionMember::can_host`]
    /// predicate, because only a Direct host needs to expose an endpoint.
    ///
    /// Re-issued and late-join member lists can contain members that never
    /// negotiated the session's sticky pair. New seat-fills cannot: the
    /// admission gate (`joiner_supports_sticky_plan`, issue #421) rejects a
    /// joiner that cannot run the stored pair with
    /// `ROOM_SESSION_INCOMPATIBLE`, so a running session never splits its data
    /// path. What remains is an incumbent reconnecting with downgraded
    /// capabilities — a reconnect owns its seat and is never rejected. A
    /// WebRTC pair is doomed unless BOTH sides negotiated the transport —
    /// `handle_signal` rejects either direction and the wasted offers burn
    /// signal rate-limit budget — so [`Self::plan_for`] still filters the peer
    /// list on both sides using the shared capability predicate.
    pub(crate) fn pairable(&self, member: &SessionMember) -> bool {
        member.supports_session(self.topology, self.transport)
    }

    /// Whether the recipient (looked up by id in `members`) is [`Self::pairable`].
    /// An id absent from `members` is defensively non-pairable.
    pub(crate) fn recipient_pairable(&self, recipient: PlayerId) -> bool {
        self.members
            .iter()
            .any(|member| member.player_id == recipient && self.pairable(member))
    }

    /// Build the per-recipient [`SessionPlanPayload`] for `recipient`.
    ///
    /// `ice_servers` is the recipient's already-prepared ICE list (the operator's
    /// static `session.ice_servers` plus this recipient's freshly minted
    /// TURN-derived entries), built at the emit site because TURN credentials embed
    /// the recipient's id. It is empty for non-WebRTC plans (Host+Direct, Relay).
    ///
    /// The `fallback` is always [`Transport::Relay`] (the floor). The peer list
    /// always excludes the recipient itself, is **capability-filtered on both
    /// sides** ([`Self::pairable`]: a recipient that cannot run the session gets
    /// an empty list — truthful, it has no P2P peers and the relay floor is its
    /// data path, with `host` kept as elected, informational — and a capable
    /// recipient never sees a non-capable member listed), and is shaped by
    /// topology:
    ///
    /// - **Mesh:** every other pairable member; `initiate` follows the glare rule
    ///   ([`local_initiates`]) so exactly one side of each pair offers.
    /// - **Host:** the host receives every pairable client (each
    ///   `initiate = false`); each pairable client receives only the host
    ///   (`initiate = true`). Clients never signal each other in a star topology.
    /// - **Relay:** an empty peer list. This is serialized for authoritative
    ///   Finalized-room join/reconnect refreshes and otherwise remains defensive.
    ///
    /// This is the single peer-list seam every emission path shares (finalize,
    /// host-failover re-plan, finalized join, and reconnect), so the filter
    /// holds uniformly. At FINALIZE it is
    /// provably a no-op: [`choose_session_plan`] selects a non-relay pair only
    /// when [`all_support`] holds, i.e. every member already satisfies the
    /// predicate.
    #[must_use]
    pub(crate) fn plan_for(
        &self,
        recipient: PlayerId,
        ice_servers: Vec<IceServer>,
    ) -> SessionPlanPayload {
        let peers = if !self.recipient_pairable(recipient) {
            // The recipient never negotiated this session's (topology,
            // transport): it must not be told to attempt P2P pairs that
            // `handle_signal` would reject. Empty is truthful — it has no P2P
            // peers; `fallback: relay` below is its data path.
            Vec::new()
        } else {
            match self.topology {
                Topology::Mesh => self
                    .members
                    .iter()
                    .filter(|member| member.player_id != recipient && self.pairable(member))
                    .map(|member| SessionPeer {
                        player_id: member.player_id,
                        player_name: member.player_name.clone(),
                        is_authority: member.is_authority,
                        initiate: local_initiates(recipient, member.player_id),
                    })
                    .collect(),
                Topology::Host => self.host_peers_for(recipient),
                Topology::Relay => Vec::new(),
            }
        };

        SessionPlanPayload {
            generation: self.generation,
            topology: self.topology,
            transport: self.transport,
            host: self.host,
            direct_endpoint: self.direct_endpoint(),
            peers,
            ice_servers,
            fallback: Transport::Relay,
        }
    }

    /// The elected host's validated connect target for `host + direct`.
    fn direct_endpoint(&self) -> Option<DirectEndpoint> {
        if self.transport != Transport::Direct {
            return None;
        }

        let host = self.host?;
        self.members
            .iter()
            .find(|member| member.player_id == host)?
            .direct_endpoint()
    }

    /// Build the per-recipient peer list for `host` topology. Only reached for
    /// a [`Self::pairable`] recipient ([`Self::plan_for`] gates first).
    fn host_peers_for(&self, recipient: PlayerId) -> Vec<SessionPeer> {
        let Some(host) = self.host else {
            // A host plan with no elected host is degenerate; emit no peers
            // rather than fabricate connections.
            return Vec::new();
        };

        if recipient == host {
            // The host answers every pairable client and initiates to none.
            // Non-pairable members (e.g. relay-only seat-fillers) stay off the
            // star: they participate via the relay floor.
            self.members
                .iter()
                .filter(|member| member.player_id != host && self.pairable(member))
                .map(|member| SessionPeer {
                    player_id: member.player_id,
                    player_name: member.player_name.clone(),
                    is_authority: false,
                    initiate: false,
                })
                .collect()
        } else {
            // Each pairable client connects only to the host and offers to it.
            self.members
                .iter()
                .find(|member| member.player_id == host && self.pairable(member))
                .map(|host_member| {
                    vec![SessionPeer {
                        player_id: host_member.player_id,
                        player_name: host_member.player_name.clone(),
                        is_authority: true,
                        initiate: true,
                    }]
                })
                .unwrap_or_default()
        }
    }
}

impl EnhancedGameServer {
    /// Build the [`SessionMember`] input list for session selection from a room's
    /// player list, attaching each player's negotiated capabilities.
    ///
    /// Shared by finalize (which runs the ladder), finalized membership
    /// refreshes, and [`Self::handle_session_member_departure`] (host failover).
    /// Membership refreshes rehydrate the stored decision over current members
    /// instead of re-running the ladder.
    ///
    /// Capability resolution is **local-node only**: [`Self::client_protocol`]
    /// returns the v2 / relay-only default for any id absent from this node's
    /// connection manager, so a member hosted on another node fails the v3 gate
    /// and downgrades the whole room to the relay floor. That is the safe failure
    /// direction (the v3 capability gate: never emit a v3 message to an
    /// unconfirmed peer; the relay floor always works) and is correct under the
    /// room-affinity model.
    /// Revisit when a single room can span nodes.
    pub(crate) fn session_members_from(&self, players: &[PlayerInfo]) -> Vec<SessionMember> {
        players
            .iter()
            .map(|player| {
                let proto = self.client_protocol(&player.id);
                SessionMember {
                    player_id: player.id,
                    player_name: player.name.clone(),
                    is_authority: player.is_authority,
                    joined_at: player.connected_at,
                    connection_info: player.connection_info.clone(),
                    version: proto.version,
                    transports: proto.transports.clone(),
                    topologies: proto.topologies.clone(),
                }
            })
            .collect()
    }

    /// Build the complete, one-shot publication for an explicit game start.
    ///
    /// The room coordinator invokes this only after capturing its exact routed
    /// member snapshot. Every recipient gets `GameStarting` as phase zero; each
    /// v3 recipient gets its tailored `SessionPlan` as phase one, including an
    /// explicit Relay/Relay no-peer result. Sticky state and metrics are deferred until the database
    /// finalization compare-and-set wins under the final routing guard.
    pub(crate) fn start_game_publication_builder(
        &self,
        room_id: RoomId,
    ) -> StartGamePublicationBuilder {
        let connection_manager = Arc::clone(&self.connection_manager);
        let session_config = self.session_config.clone();
        let turn_config = self.turn_config.clone();
        let active_session_plans = Arc::clone(&self.active_session_plans);
        let metrics = Arc::clone(&self.metrics);

        Box::new(move |finalized, game_starting| {
            let members: Vec<SessionMember> = finalized
                .members
                .iter()
                .map(|player| {
                    let protocol = connection_manager.protocol(&player.id);
                    SessionMember {
                        player_id: player.id,
                        player_name: player.name.clone(),
                        is_authority: player.is_authority,
                        joined_at: player.connected_at,
                        connection_info: player.connection_info.clone(),
                        version: protocol.version,
                        transports: protocol.transports,
                        topologies: protocol.topologies,
                    }
                })
                .collect();
            let decision = choose_session_plan(
                &finalized.game_name,
                finalized.authority_player,
                members,
                &session_config,
            );
            let relay_floor = decision.is_relay();
            let emits_v3_plan = decision.members.iter().any(SessionMember::supports_v3);
            // Wall clock (durable record): TURN credentials carry absolute
            // expiry instants the client sees; capture once per emission.
            let now_unix = decision
                .uses_webrtc_signaling()
                .then(|| chrono::Utc::now().timestamp());
            let mut turn_credentials_issued = 0u64;
            let recipient_messages = finalized
                .members
                .iter()
                .map(|member| {
                    let mut messages = vec![Arc::clone(&game_starting)];
                    let supports_v3 = decision.members.iter().any(|candidate| {
                        candidate.player_id == member.id && candidate.supports_v3()
                    });
                    if supports_v3 {
                        let (ice_servers, minted) = now_unix.map_or_else(
                            || (Vec::new(), 0),
                            |now| {
                                compose_ice_servers_for(
                                    &session_config,
                                    &turn_config,
                                    member.id,
                                    now,
                                )
                            },
                        );
                        turn_credentials_issued = turn_credentials_issued.saturating_add(minted);
                        messages.push(Arc::new(ServerMessage::SessionPlan(Box::new(
                            decision.plan_for(member.id, ice_servers),
                        ))));
                    }
                    RoomRecipientMessages {
                        player_id: member.id,
                        first_phase: 0,
                        messages,
                    }
                })
                .collect();

            let active = ActiveSessionPlan {
                topology: decision.topology,
                transport: decision.transport,
                host: decision.host,
            };
            StartGamePublication {
                recipient_messages,
                after_game_starting: Box::new(move || {
                    metrics.record_topology_selected(active.topology);
                    metrics.record_transport_selected(active.transport);
                    if relay_floor {
                        active_session_plans.remove(&room_id);
                        tracing::debug!(%room_id, "Room finalized to an explicit v3 relay-floor plan");
                    } else {
                        active_session_plans.insert(room_id, active);
                    }
                    if emits_v3_plan {
                        metrics.increment_session_plans_emitted();
                        metrics.add_turn_credentials_issued(turn_credentials_issued);
                        tracing::info!(
                            %room_id,
                            topology = ?active.topology,
                            transport = ?active.transport,
                            "Computed v3 session plan"
                        );
                    }
                }),
            }
        })
    }

    /// Isolated session-plan emission harness retained for policy unit tests.
    /// Production finalization uses [`Self::start_game_publication_builder`] so
    /// plan state and delivery cannot split from `GameStarting`.
    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub(crate) async fn emit_session_plan(&self, room_id: &RoomId, finalized: &FinalizedRoom) {
        let members = self.session_members_from(&finalized.members);

        let decision = choose_session_plan(
            &finalized.game_name,
            finalized.authority_player,
            members,
            &self.session_config,
        );

        // Record the per-finalized-room topology/transport selection here once;
        // relay-resolved rooms pick Relay/Relay. This is the
        // sole counting site for selection: late join/reconnect and departure
        // re-planning rehydrate the STORED decision instead of re-running
        // `choose_session_plan`, so they can never double-count a room.
        self.metrics.record_topology_selected(decision.topology);
        self.metrics.record_transport_selected(decision.transport);

        // Relay is an explicit v3 no-peer plan but has no sticky entry. Remove
        // any stale stored decision so the map cannot describe a session the
        // room is no longer running.
        if decision.is_relay() {
            self.active_session_plans.remove(room_id);
        } else {
            // Record the sticky decision the room now runs: consulted instead
            // of recomputing on later membership changes.
            self.active_session_plans.insert(
                *room_id,
                ActiveSessionPlan {
                    topology: decision.topology,
                    transport: decision.transport,
                    host: decision.host,
                },
            );
        }

        tracing::info!(
            %room_id,
            topology = ?decision.topology,
            transport = ?decision.transport,
            "Computed v3 session plan"
        );

        // Count one finalization plan event when at least one v3 recipient sees
        // it. Per-recipient frames remain one logical plan publication.
        if decision.members.iter().any(SessionMember::supports_v3) {
            self.metrics.increment_session_plans_emitted();
        }

        let turn_credentials_issued = self.send_session_plans_to_members(room_id, &decision).await;
        self.metrics
            .add_turn_credentials_issued(turn_credentials_issued);
    }

    /// Copy a room's stored active session decision out of the map.
    ///
    /// Returns a copied value — never a `DashMap` guard — so callers can freely
    /// `.await` afterwards without risking a shard-lock deadlock.
    pub(crate) fn active_session_plan(&self, room_id: &RoomId) -> Option<ActiveSessionPlan> {
        self.active_session_plans
            .get(room_id)
            .map(|entry| *entry.value())
    }

    /// Drop a room's stored session decision (the room was removed/cleaned up).
    pub(crate) fn clear_active_session_plan(&self, room_id: &RoomId) {
        self.active_session_plans.remove(room_id);
    }

    /// Drop stored session decisions whose room no longer exists in storage.
    ///
    /// Safety net for room-removal paths that don't report per-room ids (e.g.
    /// `cleanup_expired_rooms` returns only counts), run from the maintenance
    /// cleanup task. Room ids are unique UUIDs, so removing an entry for a
    /// nonexistent room can never collide with a future room. Returns the
    /// number of entries removed.
    pub(crate) async fn prune_active_session_plans(&self) -> usize {
        // Snapshot the keys first: never hold a DashMap entry/iterator guard
        // across the `.await`s below.
        let room_ids: Vec<RoomId> = self
            .active_session_plans
            .iter()
            .map(|entry| *entry.key())
            .collect();

        let mut removed = 0usize;
        for room_id in room_ids {
            match self.database.get_room_by_id(&room_id).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    if self.active_session_plans.remove(&room_id).is_some() {
                        removed = removed.saturating_add(1);
                    }
                }
                // Transient storage error: keep the entry and retry next tick.
                Err(err) => {
                    tracing::warn!(
                        %room_id,
                        error = %err,
                        "Failed to check room existence while pruning active session plans"
                    );
                }
            }
        }
        removed
    }

    /// Mid-session re-planning hook, called from `leave_room` AFTER the
    /// `PlayerLeft` broadcast (for both explicit `LeaveRoom` and disconnects —
    /// `leave_room` is the single choke point) and only when the player was
    /// actually removed.
    ///
    /// Topology and transport are **sticky for the session lifetime**: even
    /// though the capability intersection can only widen when members depart,
    /// the plan is never upgraded mid-session (a data-path migration mid-game
    /// would disrupt gameplay for zero correctness gain — the ladder runs once
    /// at finalize). Only a departure that invalidates a plan *parameter* — a
    /// `host`-topology session whose stored host is **invalid**
    /// ([`ActiveSessionPlan::host_invalid`]: no longer a member, or seated but
    /// no longer capable of the session after a capability-downgrading
    /// reconnect) — triggers a re-emission, via the shared
    /// [`Self::replan_host_session`] (execution-aware re-election + fresh
    /// per-recipient `SessionPlan`s, same topology and transport). The trigger
    /// is deliberately "the stored host is invalid", not "the departed player
    /// was the host": the hook runs after removal, so a departing host is
    /// naturally missing, and the broader gate also self-heals a wedged entry
    /// whose host was already gone or downgraded (an insert-after-departure
    /// race at finalize, concurrent host + candidate departures, an earlier
    /// hook skipped by a transient storage error, or a reconnect that shrank
    /// the host's negotiated capabilities) on the next departure. Any other
    /// departure changes no plan parameter, so nothing is re-emitted —
    /// `PlayerLeft` already tells peers to prune the departed member.
    ///
    /// Concurrency: one shared room-mutation guard is intentionally held across
    /// the awaited storage refresh, host election, sticky-plan replacement, and
    /// exact-membership deliveries. Same-node departures and membership
    /// publication therefore serialize; each emitted plan is computed from the
    /// refreshed routed-member snapshot. No `DashMap` entry guard is held across
    /// an `.await`.
    pub(crate) async fn handle_session_member_departure(
        &self,
        room_id: &RoomId,
        departed: &PlayerId,
    ) {
        // Membership publishers use this same local gate. Keep the routed
        // snapshot stable through host election, stored-plan replacement, and
        // all exact-membership deliveries so a pending join/reconnect cannot
        // wedge the plan on a host that nobody was told about.
        let room_event_guard = self
            .message_coordinator
            .lock_room_event_mutation(room_id)
            .await;

        // No stored decision ⇒ the room runs the relay floor (or pre-dates v3):
        // pure v2 semantics, `PlayerLeft` suffices.
        let Some(stored) = self.active_session_plan(room_id) else {
            return;
        };

        let room = match self.database.get_room_by_id(room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                // The room itself is gone; drop the stale decision.
                self.active_session_plans.remove(room_id);
                return;
            }
            Err(err) => {
                tracing::warn!(
                    %room_id,
                    %departed,
                    error = %err,
                    "Failed to fetch room for departure re-planning"
                );
                return;
            }
        };

        // A stored decision exists only for a finalized session; anything else
        // is anomalous and there is no live session to re-plan.
        if room.lobby_state != LobbyState::Finalized {
            return;
        }

        let routed_player_ids = match self.message_coordinator.routed_player_ids(room_id).await {
            Ok(ids) => ids.map(|ids| ids.into_iter().collect::<HashSet<_>>()),
            Err(error) => {
                tracing::warn!(
                    %room_id,
                    %departed,
                    %error,
                    "Failed to resolve published room membership for departure re-planning"
                );
                return;
            }
        };
        let remaining: Vec<PlayerInfo> = room
            .players
            .values()
            .filter(|member| {
                routed_player_ids
                    .as_ref()
                    .is_none_or(|routed| routed.contains(&member.id))
            })
            .cloned()
            .collect();

        // Last published member left. Storage may still hold an admitted
        // member whose route has not committed yet — a failed finalized
        // publication spawns its teardown after releasing this guard, and a
        // join/reconnect transaction commits its route on its own pass. Keep
        // the sticky decision so that member's committed route repairs this
        // session instead of downgrading the whole generation to the relay
        // floor; the maintenance prune drops the entry if the room itself is
        // removed first.
        if remaining.is_empty() {
            if room.players.is_empty() {
                self.active_session_plans.remove(room_id);
            }
            return;
        }

        // Sticky topology/transport: only an invalid *host* invalidates a plan
        // parameter. Mesh departures, and host-topology departures that leave
        // a still-capable stored host in place, re-emit nothing.
        let members = self.session_members_from(&remaining);
        if !stored.host_invalid(&members) {
            return;
        }

        tracing::info!(
            %room_id,
            %departed,
            stored_host = ?stored.host,
            "Active session host is missing or incapable after a departure; re-electing"
        );

        self.replan_host_session(
            room_id,
            stored,
            room.authority_player,
            members,
            room_event_guard,
        )
        .await;
    }

    /// Re-elect the host of a stored `host`-topology session over `members`
    /// (the room's current members with their negotiated capabilities, from
    /// [`Self::session_members_from`]) and re-emit a fresh per-recipient
    /// `SessionPlan` to every member — same sticky topology/transport, new
    /// `host`, fresh per-recipient ICE for WebRTC. Departures use this owned
    /// transaction; finalized additions use [`membership_session_decision`] to
    /// fold the same repair into their lifecycle publication.
    ///
    /// Election is **execution-aware**: only members that negotiated v3 AND
    /// the stored sticky (topology, transport) pair are electable, and Direct
    /// candidates must expose a validated endpoint. A weaker member can
    /// legitimately sit in a finalized room (an incumbent reconnecting with
    /// downgraded capabilities keeps its seat; new seat-fills are gated by
    /// [`joiner_supports_sticky_plan`]) and can even hold authority (plain v2
    /// `RequestAuthority` has no version gate), but it must never be named host
    /// of a session it cannot run — the v3 members would receive a failover
    /// plan naming a host that cannot anchor the selected transport. The member slice is
    /// pre-filtered and the authority preference passes through the same
    /// filter (authority must not outrank the capability gate); [`elect_host`]
    /// itself stays generic — its other caller, finalize-time
    /// [`choose_session_plan`], needs no filter because `all_support` already
    /// guarantees every member's capability there.
    ///
    /// If **no** member qualifies, the session is over: the stored entry is
    /// removed, nothing is emitted, and `session_replans_emitted` does NOT
    /// move (no re-plan happened) — the relay floor carries the room.
    ///
    /// Otherwise the stored entry is rewritten in the exact publication hook,
    /// one
    /// `session_replans_emitted` event is counted, and every current member is
    /// best-effort delivered its tailored plan — v3-gated per recipient, so a
    /// v3 relay-only member still receives the plan describing the session
    /// while v2 members receive nothing. Peer lists are capability-filtered on
    /// both sides in [`SessionPlanDecision::plan_for`]: a member that did not
    /// negotiate the sticky pair gets an **empty** `peers` list (`fallback:
    /// relay` is its data path) and is never listed in capable members' plans
    /// on both sides. A re-elected host of a 1-member room is fine (a star of one with
    /// an empty peer list; a future late-join re-pairs).
    pub(super) async fn replan_host_session(
        &self,
        room_id: &RoomId,
        stored: ActiveSessionPlan,
        authority: Option<PlayerId>,
        members: Vec<SessionMember>,
        room_event_guard: RoomEventMutationGuard,
    ) {
        // Capability gate (see doc comment): electable ⊆ members.
        let electable: Vec<SessionMember> = members
            .iter()
            .filter(|member| stored.supported_by(member))
            .cloned()
            .collect();
        // The authority preference passes through the SAME gate, so an
        // authority that cannot run the session is neither preferred nor
        // electable (`elect_host` would also drop an id absent from the
        // filtered slice; filtering here keeps the rule explicit).
        let electable_authority =
            authority.filter(|id| electable.iter().any(|member| member.player_id == *id));

        let Some(new_host) = elect_host(electable_authority, &electable) else {
            // Nobody can run the stored session: it is over. Drop the entry and
            // emit nothing — deliberately NOT counted as a re-plan event.
            self.active_session_plans.remove(room_id);
            tracing::info!(
                %room_id,
                topology = ?stored.topology,
                transport = ?stored.transport,
                "No remaining member can host the active session; dropped the stored plan \
                 (the relay floor carries the room)"
            );
            return;
        };

        let updated = ActiveSessionPlan {
            host: Some(new_host),
            ..stored
        };

        tracing::info!(
            %room_id,
            %new_host,
            topology = ?updated.topology,
            transport = ?updated.transport,
            "Re-elected active session host; re-emitting session plans"
        );

        let decision = updated.decision_with(members);
        self.observe_mixed_path_members(room_id, &decision);
        // Wall clock (durable record): TURN credentials carry absolute
        // expiry instants the client sees; capture once per emission.
        let now_unix = decision
            .uses_webrtc_signaling()
            .then(|| chrono::Utc::now().timestamp());
        let mut turn_credentials_issued = 0_u64;
        let recipient_messages: Vec<_> = decision
            .members
            .iter()
            .map(|member| {
                let messages = self
                    .build_session_plan_message(&decision, member.player_id, now_unix)
                    .map(|(message, minted)| {
                        turn_credentials_issued = turn_credentials_issued.saturating_add(minted);
                        vec![message]
                    })
                    .unwrap_or_default();
                RoomRecipientMessages::in_order(member.player_id, messages)
            })
            .collect();
        let expected_members: Vec<_> = decision
            .members
            .iter()
            .map(|member| member.player_id)
            .collect();

        let room_id = *room_id;
        let coordinator_for_job = Arc::clone(&self.message_coordinator);
        let active_session_plans = Arc::clone(&self.active_session_plans);
        let metrics_for_commit = Arc::clone(&self.metrics);
        let metrics_after_phase = Arc::clone(&self.metrics);
        let completion = self.message_coordinator.enqueue_room_event(
            room_event_guard,
            Box::new(move || {
                Box::pin(async move {
                    let outcome = coordinator_for_job
                        .commit_room_messages_if_members_with_hook(
                            &room_id,
                            &expected_members,
                            recipient_messages,
                            Box::new(move || {
                                Box::pin(async move {
                                    active_session_plans.insert(room_id, updated);
                                    metrics_for_commit.increment_session_replans_emitted();
                                    Ok(true)
                                })
                            }),
                            Box::new(move |_failed_phase_zero| {
                                metrics_after_phase
                                    .add_turn_credentials_issued(turn_credentials_issued);
                                true
                            }),
                        )
                        .await?;
                    match outcome {
                        RoomMessageTransactionOutcome::Committed => Ok(true),
                        RoomMessageTransactionOutcome::CommittedDegraded { failed_frames } => {
                            tracing::warn!(
                                %room_id,
                                failed_frames,
                                "Host re-plan committed with degraded frame delivery"
                            );
                            Ok(true)
                        }
                        RoomMessageTransactionOutcome::RoutingChanged => {
                            tracing::debug!(%room_id, "Host re-plan canceled after routing changed");
                            Ok(false)
                        }
                        RoomMessageTransactionOutcome::HookRejected => Ok(false),
                    }
                })
            }),
        );
        if let Err(error) = completion.await {
            tracing::warn!(%room_id, %error, "Host re-plan publication failed");
        }
    }

    /// Deliver `decision` to every member as per-recipient `SessionPlan`s,
    /// sharing one TURN credential expiry across the whole emission event.
    /// Returns the total TURN credentials minted (callers feed it into
    /// `add_turn_credentials_issued` once per event).
    ///
    /// Shared by finalize emission ([`Self::emit_session_plan`]) and host
    /// re-planning ([`Self::replan_host_session`]); the normal late-join path
    /// delivers to a single recipient via [`Self::send_session_plan_to`].
    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub(super) async fn send_session_plans_to_members(
        &self,
        room_id: &RoomId,
        decision: &SessionPlanDecision,
    ) -> u64 {
        // Capture `now` once so every member of one emission event shares one
        // TURN credential expiry (deterministic and testable). Evaluated only
        // for WebRTC plans, where ICE is built per recipient; Host+Direct and
        // explicit Relay/Relay plans carry an empty list and never read it.
        // Wall clock (durable record): credentials are absolute-time records.
        let now_unix = decision
            .uses_webrtc_signaling()
            .then(|| chrono::Utc::now().timestamp());

        let mut turn_credentials_issued: u64 = 0;
        for member in &decision.members {
            turn_credentials_issued += self
                .send_session_plan_to(room_id, decision, member.player_id, now_unix)
                .await
                .unwrap_or(0);
        }
        turn_credentials_issued
    }

    /// Deliver `decision` to one recipient as a tailored `SessionPlan`, minting
    /// the recipient's ICE list when `now_unix` is set (i.e. the plan's
    /// transport is WebRTC; `now_unix` is the shared credential expiry captured
    /// once per emission event).
    ///
    /// Defense-in-depth v3 gate: a non-v3 connection is never sent a
    /// plan — at finalize all members are v3 by construction (`all_support`),
    /// but a drifted incumbent (a reconnect with downgraded capabilities) can
    /// still lack the session pair. Returns `None` when
    /// gated off (nothing sent), otherwise `Some(minted)` — the number of TURN
    /// credentials minted into the delivered plan.
    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub(super) async fn send_session_plan_to(
        &self,
        room_id: &RoomId,
        decision: &SessionPlanDecision,
        recipient: PlayerId,
        now_unix: Option<i64>,
    ) -> Option<u64> {
        let (message, minted) = self.build_session_plan_message(decision, recipient, now_unix)?;
        let expected_members: Vec<PlayerId> = decision
            .members
            .iter()
            .map(|member| member.player_id)
            .collect();
        // Best-effort but room-scoped delivery: a peer that moved after the
        // decision was built must not receive this plan in its replacement
        // session. The relay floor remains the fallback when delivery fails.
        let delivered = self
            .message_coordinator
            .send_to_player_in_room_if_members(&recipient, room_id, &expected_members, message)
            .await
            .unwrap_or(false);
        delivered.then_some(minted)
    }

    /// Build one v3-gated tailored plan without performing delivery. Reserved
    /// room publications use this to prepare every recipient before atomically
    /// reserving the complete phase set.
    pub(super) fn build_session_plan_message(
        &self,
        decision: &SessionPlanDecision,
        recipient: PlayerId,
        now_unix: Option<i64>,
    ) -> Option<(Arc<ServerMessage>, u64)> {
        if !self.client_supports_v3(&recipient) {
            return None;
        }

        // A recipient that never negotiated this session's (topology,
        // transport) receives an empty peer list and the relay fallback, so it
        // can never run ICE for this session: it gets no ICE list at all, TURN
        // or STUN, because there is nothing for it to gather against.
        // `ice_pregather_eligible` applies the same reasoning one step earlier
        // and with the only information available then — the GAME'S DESIRED
        // topology, because no session exists during the lobby. A member
        // eligible there can still end up non-pairable here if the ladder
        // settles below its negotiated rung, so pre-gather can hand out a
        // credential this seam later withholds.
        let (ice_servers, minted) = match now_unix {
            Some(now_unix) if decision.recipient_pairable(recipient) => {
                self.composed_ice_servers_for(recipient, now_unix)
            }
            _ => (Vec::new(), 0),
        };
        let plan = decision.plan_for(recipient, ice_servers);
        Some((Arc::new(ServerMessage::SessionPlan(Box::new(plan))), minted))
    }

    /// Observe a mixed-path membership during a non-relay plan publication
    /// (issue #421): seated members whose negotiated capabilities exclude the
    /// session's sticky pair. Their own plans carry an empty `peers` list and
    /// capable members' plans omit them — the server relays their
    /// WebSocket traffic to everyone, but peer-to-peer traffic between
    /// capable members never reaches them.
    ///
    /// Post-admission-gate this can only arise from an incumbent reconnecting
    /// with downgraded capabilities (a reconnect owns its seat and is never
    /// rejected); each publication counts every such member it observes —
    /// exactly once per publication event, not per delivery retry or outcome.
    /// Finalize needs no call:
    /// a non-relay selection requires [`all_support`](fn@all_support), and the
    /// relay floor is not observed by definition.
    pub(crate) fn observe_mixed_path_members(
        &self,
        room_id: &RoomId,
        decision: &SessionPlanDecision,
    ) {
        if decision.is_relay() {
            return;
        }
        let mixed: Vec<PlayerId> = decision
            .members
            .iter()
            .filter(|member| !decision.pairable(member))
            .map(|member| member.player_id)
            .collect();
        if mixed.is_empty() {
            return;
        }
        tracing::warn!(
            %room_id,
            topology = ?decision.topology,
            transport = ?decision.transport,
            ?mixed,
            "Mixed-path session membership: these members cannot run the sticky \
             topology/transport and only see WebSocket-relayed traffic"
        );
        self.metrics
            .add_mixed_path_members_observed(mixed.len() as u64);
    }

    /// Compose `recipient`'s full ICE list — the operator's static
    /// `session.ice_servers` first (preserved verbatim for back-compat), then
    /// the `[turn]` block's contribution (credential-less STUN, then a TURN
    /// entry freshly minted for `recipient` at `now_unix`, via
    /// [`crate::security::build_ice_servers`]). Returns `(list, minted)` where
    /// `minted` counts the entries carrying credentials (a `username`) — the
    /// exact per-recipient "credential issued" events for
    /// `turn_credentials_issued`.
    ///
    /// This is the **single** ICE composition site in the codebase, shared by
    /// `SessionPlan` delivery ([`Self::send_session_plan_to`]) and the
    /// `RoomJoined` / `Reconnected` pre-gather path
    /// ([`Self::pregather_ice_servers`]), so the two surfaces can never drift.
    pub(crate) fn composed_ice_servers_for(
        &self,
        recipient: PlayerId,
        now_unix: i64,
    ) -> (Vec<IceServer>, u64) {
        compose_ice_servers_for(&self.session_config, &self.turn_config, recipient, now_unix)
    }

    /// Build the ICE pre-gather list for a `RoomJoined` / `Reconnected` payload
    /// (the deferred "RoomJoined ICE pre-gather" refinement): the same
    /// composed list a WebRTC `SessionPlan` would carry, surfaced at join time
    /// so a v3 WebRTC-capable client can gather ICE candidates during the lobby
    /// wait instead of adding that latency at game start. The `SessionPlan` ICE
    /// list supersedes it (fresh credentials always arrive there).
    ///
    /// Returns `Vec::new()` — the field is then skipped on the wire, keeping
    /// the v2 bytes identical — unless [`ice_pregather_eligible`] holds for
    /// this room/recipient (see its doc for the full gate, including why
    /// `Finalized` rooms are excluded: the late-join `SessionPlan` is the sole
    /// issuance site there, so one logical join event never mints twice).
    ///
    /// Metrics: counts one `ice_pregather_emitted` per **non-empty** list (an
    /// eligible joiner with nothing configured emits no field and is not
    /// counted) and adds the minted TURN credentials to the
    /// `turn_credentials_issued` total-issuance counter.
    pub(crate) fn pregather_ice_servers(
        &self,
        room: &Room,
        player_id: &PlayerId,
    ) -> Vec<IceServer> {
        let protocol = self.client_protocol(player_id);
        if !ice_pregather_eligible(
            &self.session_config,
            &room.game_name,
            &room.lobby_state,
            &protocol,
        ) {
            return Vec::new();
        }

        // Wall clock (durable record): TURN credentials carry absolute
        // expiry instants the client sees.
        let now_unix = chrono::Utc::now().timestamp();
        let (ice_servers, minted) = self.composed_ice_servers_for(*player_id, now_unix);
        if ice_servers.is_empty() {
            // Legitimate even when eligible: no static ICE, no STUN urls, TURN
            // disabled. Nothing reaches the wire, so nothing is counted.
            return ice_servers;
        }

        self.metrics.increment_ice_pregather_emitted();
        self.metrics.add_turn_credentials_issued(minted);
        ice_servers
    }
}

fn compose_ice_servers_for(
    session_config: &SessionConfig,
    turn_config: &crate::config::TurnConfig,
    recipient: PlayerId,
    now_unix: i64,
) -> (Vec<IceServer>, u64) {
    let mut ice = session_config.ice_servers.clone();
    let turn_derived = crate::security::build_ice_servers(turn_config, recipient, now_unix);
    let minted = turn_derived
        .iter()
        .filter(|server| server.username.is_some())
        .count() as u64;
    ice.extend(turn_derived);
    (ice, minted)
}
