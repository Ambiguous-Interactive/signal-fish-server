//! Per-room session-plan selection and emission (Protocol v3, PLAN §P3).
//!
//! At lobby finalization the server picks a single room-wide plan from the
//! intersection of every member's negotiated capabilities (Appendix D), then
//! hands each v3 member a *per-recipient* [`SessionPlanPayload`] whose peer list
//! and `initiate` flags are tailored to that recipient (Appendix E). A room that
//! resolves to the relay floor emits no plan and behaves byte-identically to v2.
//!
//! The selection core ([`choose_session_plan`], [`elect_host`],
//! [`SessionPlanDecision::plan_for`]) is pure and unit-testable; the thin
//! [`EnhancedGameServer::emit_session_plan`] method gathers per-member
//! capabilities, runs the core, and best-effort delivers the result — gated on
//! v3 so a v2 client never observes a `SessionPlan` (Appendix K).

use std::sync::Arc;

use crate::config::SessionConfig;
use crate::coordination::FinalizedRoom;
use crate::protocol::{
    IceServer, PlayerId, PlayerInfo, RoomId, ServerMessage, SessionPeer, SessionPlanPayload,
    Topology, Transport,
};

use super::signaling::local_initiates;
use super::EnhancedGameServer;

/// Per-member input to session selection: identity + negotiated capabilities +
/// join time (for deterministic host election).
#[derive(Clone)]
pub(crate) struct SessionMember {
    pub player_id: PlayerId,
    pub player_name: String,
    pub is_authority: bool,
    pub joined_at: chrono::DateTime<chrono::Utc>,
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
}

/// The room-wide selection result (not yet specialized per recipient).
///
/// Carries the canonical member list so [`SessionPlanDecision::plan_for`] can
/// build each recipient's tailored peer list. The ICE servers are intentionally
/// **not** stored here: they are per-recipient (each WebRTC member receives its
/// own freshly minted TURN credentials), so the emit site builds them and passes
/// them into [`SessionPlanDecision::plan_for`].
pub(crate) struct SessionPlanDecision {
    pub topology: Topology,
    pub transport: Transport,
    pub host: Option<PlayerId>,
    pub members: Vec<SessionMember>,
}

/// Whether *every* member supports v3 and the given (topology, transport) pair.
///
/// An empty room never supports an upgrade (there is nothing to connect).
fn all_support(members: &[SessionMember], topology: Topology, transport: Transport) -> bool {
    !members.is_empty()
        && members.iter().all(|member| {
            member.supports_v3()
                && member.supports_topology(topology)
                && member.supports_transport(transport)
        })
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
/// Selected only when no [`UPGRADE_LADDER`] rung fits. A relay-floor room emits no
/// `SessionPlan`, so v3 clients relay byte-identically to v2.
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

/// Whether `(topology, transport)` is one of the four legal session pairs — the
/// three [`UPGRADE_LADDER`] rungs plus the [`RELAY_FLOOR`].
///
/// Every other combination (e.g. `Mesh + Direct`, `Host + Relay`) is illegal and
/// must never reach a client: downstream consumers — late-join WebRTC pairing, ICE
/// emission, the relay-floor short-circuit — rely on this topology/transport
/// coupling. Backs the `debug_assert!` in [`choose_session_plan`] and the
/// exhaustive `selection_only_ever_yields_a_legal_pair` invariant test.
#[must_use]
pub(crate) fn is_valid_pair(topology: Topology, transport: Transport) -> bool {
    UPGRADE_LADDER
        .into_iter()
        .chain(std::iter::once(RELAY_FLOOR))
        .any(|rung| rung == (topology, transport))
}

/// Choose the room-wide session plan from member capabilities (ADR-0001, Appendix D).
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
/// 3. `Host` + Direct — `desired ∈ {Mesh, Host}`, direct enabled, all support host+direct (LAN).
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
    let desired = cfg
        .game_topology_mappings
        .get(game_name)
        .copied()
        .unwrap_or(cfg.default_topology);

    // Walk the richest-first ladder and settle on the first rung that fits the
    // `desired` ceiling, has its transport enabled, and is supported by *every*
    // member; otherwise fall to the universal relay floor (ADR-0001 §1/§3).
    let (topology, transport) = UPGRADE_LADDER
        .into_iter()
        .find(|&(topology, transport)| {
            topology_rank(topology) <= topology_rank(desired)
                && transport_enabled(cfg, transport)
                && all_support(&members, topology, transport)
        })
        .unwrap_or(RELAY_FLOOR);

    debug_assert!(
        is_valid_pair(topology, transport),
        "choose_session_plan must yield a legal (topology, transport) pair"
    );

    let host = if topology == Topology::Host {
        elect_host(authority, &members)
    } else {
        None
    };

    SessionPlanDecision {
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
    /// A relay-floor room emits no `SessionPlan` (v3 clients relay exactly like
    /// v2). Equivalent to the transport being [`Transport::Relay`]: the floor is
    /// the only legal relay pairing ([`is_valid_pair`]).
    ///
    /// This is **not** the same gate as [`Self::uses_webrtc_signaling`]: a
    /// `Host + Direct` plan is non-relay (so it *does* receive a `SessionPlan`)
    /// yet is non-WebRTC (so it emits no `NewPeer`/`Signal`). The two gates'
    /// truth table over the four legal pairs is pinned by the
    /// `emission_gates_track_relay_topology_and_webrtc_transport` test.
    #[must_use]
    pub(crate) fn is_relay(&self) -> bool {
        self.topology == Topology::Relay
    }

    /// Whether this plan uses server-mediated WebRTC signaling.
    ///
    /// This is the gate for emitting `Signal` / `NewPeer` control messages: it is
    /// true **iff** the chosen transport is [`Transport::WebRtc`]. A `Host + Direct`
    /// (LAN) plan is non-relay yet is *not* WebRTC, so it must never trigger WebRTC
    /// pairing — keying off topology (or [`Self::is_relay`]) alone would misfire
    /// (see [`EnhancedGameServer::handle_webrtc_late_join`]). The truth table
    /// versus [`Self::is_relay`] is pinned by the
    /// `emission_gates_track_relay_topology_and_webrtc_transport` test.
    #[must_use]
    pub(crate) fn uses_webrtc_signaling(&self) -> bool {
        self.transport == Transport::WebRtc
    }

    /// Build the per-recipient [`SessionPlanPayload`] for `recipient` (Appendix E).
    ///
    /// `ice_servers` is the recipient's already-prepared ICE list (the operator's
    /// static `session.ice_servers` plus this recipient's freshly minted
    /// TURN-derived entries), built at the emit site because TURN credentials embed
    /// the recipient's id. It is empty for non-WebRTC plans (Host+Direct, Relay).
    ///
    /// The `fallback` is always [`Transport::Relay`] (the floor). The peer list
    /// always excludes the recipient itself and is shaped by topology:
    ///
    /// - **Mesh:** every other member; `initiate` follows the glare rule
    ///   ([`local_initiates`]) so exactly one side of each pair offers.
    /// - **Host:** the host receives every client (each `initiate = false`); each
    ///   client receives only the host (`initiate = true`). Clients never signal
    ///   each other in a star topology.
    /// - **Relay:** never emitted (the relay floor sends no plan), but returns an
    ///   empty peer list defensively.
    #[must_use]
    pub(crate) fn plan_for(
        &self,
        recipient: PlayerId,
        ice_servers: Vec<IceServer>,
    ) -> SessionPlanPayload {
        let peers = match self.topology {
            Topology::Mesh => self
                .members
                .iter()
                .filter(|member| member.player_id != recipient)
                .map(|member| SessionPeer {
                    player_id: member.player_id,
                    player_name: member.player_name.clone(),
                    is_authority: member.is_authority,
                    initiate: local_initiates(recipient, member.player_id),
                })
                .collect(),
            Topology::Host => self.host_peers_for(recipient),
            Topology::Relay => Vec::new(),
        };

        SessionPlanPayload {
            topology: self.topology,
            transport: self.transport,
            host: self.host,
            peers,
            ice_servers,
            fallback: Transport::Relay,
        }
    }

    /// Build the per-recipient peer list for `host` topology.
    fn host_peers_for(&self, recipient: PlayerId) -> Vec<SessionPeer> {
        let Some(host) = self.host else {
            // A host plan with no elected host is degenerate; emit no peers
            // rather than fabricate connections.
            return Vec::new();
        };

        if recipient == host {
            // The host answers every client and initiates to none.
            self.members
                .iter()
                .filter(|member| member.player_id != host)
                .map(|member| SessionPeer {
                    player_id: member.player_id,
                    player_name: member.player_name.clone(),
                    is_authority: false,
                    initiate: false,
                })
                .collect()
        } else {
            // Each client connects only to the host and offers to it.
            self.members
                .iter()
                .find(|member| member.player_id == host)
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
    /// Shared by [`Self::emit_session_plan`] (finalize) and
    /// [`Self::handle_webrtc_late_join`] (late join / reconnect into an active
    /// session) so both paths run the identical selection over identical inputs.
    ///
    /// Capability resolution is **local-node only**: [`Self::client_protocol`]
    /// returns the v2 / relay-only default for any id absent from this node's
    /// connection manager, so a member hosted on another node fails the v3 gate
    /// and downgrades the whole room to the relay floor. That is the safe failure
    /// direction (Appendix K — never emit a v3 message to an unconfirmed peer; the
    /// relay floor always works) and is correct under the room-affinity model
    /// (PLAN Appendix J). Revisit when a single room can span nodes (P8).
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
                    version: proto.version,
                    transports: proto.transports.clone(),
                    topologies: proto.topologies.clone(),
                }
            })
            .collect()
    }

    /// Compute and emit the per-recipient v3 SessionPlan for a finalized room.
    ///
    /// Called from the `handle_player_ready` wrapper AFTER the coordinator
    /// broadcasts the unchanged `GameStarting`, preserving the per-recipient
    /// ordering GameStarting → SessionPlan. A room that resolves to the relay
    /// floor emits no plan, so v2 (and v3-relay-only) members observe exactly the
    /// v2 finalization flow.
    pub(crate) async fn emit_session_plan(&self, room_id: &RoomId, finalized: &FinalizedRoom) {
        let members = self.session_members_from(&finalized.members);

        let decision = choose_session_plan(
            &finalized.game_name,
            finalized.authority_player,
            members,
            &self.session_config,
        );

        // Record the per-finalized-room topology/transport selection here — once
        // per finalize, and *before* the relay-floor early-return so a
        // relay-resolved room is counted too (it picks Relay/Relay). This is the
        // sole counting site for selection: `choose_session_plan` is also called
        // from `handle_webrtc_late_join`, which must NOT count (it would
        // double-count an already-finalized room on every late join / reconnect).
        self.metrics.record_topology_selected(decision.topology);
        self.metrics.record_transport_selected(decision.transport);

        // Relay floor: no SessionPlan is sent. v3 clients fall back to relaying
        // game data exactly like v2 (the floor never closes).
        if decision.is_relay() {
            tracing::debug!(
                %room_id,
                "Room finalized to the relay floor; no v3 SessionPlan emitted"
            );
            return;
        }

        tracing::info!(
            %room_id,
            topology = ?decision.topology,
            transport = ?decision.transport,
            "Computed v3 session plan"
        );

        // Capture `now` once so every member finalized together shares one TURN
        // credential expiry (deterministic and testable). Evaluated only for
        // WebRTC plans, where ICE is built per recipient; Host+Direct (and the
        // never-emitted Relay) carry an empty list and never read it.
        let webrtc = decision.uses_webrtc_signaling();
        let now_unix = webrtc.then(|| chrono::Utc::now().timestamp());

        // One non-relay SessionPlan finalize event: count once per finalized
        // non-relay room (the relay floor returned above and is never counted
        // here). The per-recipient sends below are the delivery of this single
        // logical event, not separate plans.
        self.metrics.increment_session_plans_emitted();

        // Tally the TURN credentials this finalize actually mints across all
        // recipients, incrementing the metric once at the end (one event per
        // finalize) rather than per recipient.
        let mut turn_credentials_issued: u64 = 0;

        for member in &decision.members {
            // Defense-in-depth gate: in a non-relay plan all members are v3 by
            // construction (`all_support` requires v3), but mirror signaling.rs's
            // airtight gating so a non-v3 connection can never be sent a plan.
            if !self.client_supports_v3(&member.player_id) {
                continue;
            }

            let ice_servers = if let Some(now_unix) = now_unix {
                // Operator's static ICE list first (preserved verbatim for
                // back-compat), then this recipient's TURN-derived entries.
                let mut ice = self.session_config.ice_servers.clone();
                let turn_derived = crate::security::build_ice_servers(
                    &self.turn_config,
                    member.player_id,
                    now_unix,
                );
                // A minted TURN entry is the one carrying credentials (a `username`);
                // credential-less STUN entries are not counted. This is the exact
                // "credential issued" event (per recipient).
                turn_credentials_issued += turn_derived
                    .iter()
                    .filter(|server| server.username.is_some())
                    .count() as u64;
                ice.extend(turn_derived);
                ice
            } else {
                Vec::new()
            };

            let plan = decision.plan_for(member.player_id, ice_servers);
            // Best-effort delivery: `send_to_player` returns `Ok(())` even when a
            // peer's channel is full/closed, so a backpressured client may miss
            // the plan. That is acceptable — the relay floor remains the fallback
            // transport — so the result is deliberately ignored (mirrors
            // `handle_signal`).
            let _ = self
                .message_coordinator
                .send_to_player(
                    &member.player_id,
                    Arc::new(ServerMessage::SessionPlan(Box::new(plan))),
                )
                .await;
        }

        self.metrics
            .add_turn_credentials_issued(turn_credentials_issued);
    }
}
