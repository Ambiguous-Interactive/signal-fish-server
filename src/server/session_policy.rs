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
/// build each recipient's tailored peer list.
pub(crate) struct SessionPlanDecision {
    pub topology: Topology,
    pub transport: Transport,
    pub host: Option<PlayerId>,
    pub ice_servers: Vec<IceServer>,
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

/// Choose the room-wide session plan from member capabilities (Appendix D).
///
/// The desired topology comes from the per-game override, falling back to the
/// configured default. The ladder prefers richer topologies but the relay floor
/// always wins ties — any member lacking the required capability (or a disabled
/// transport) downgrades the whole room to `(Relay, Relay)`:
///
/// 1. `Mesh` + WebRTC — if enabled and all members support mesh+webrtc.
/// 2. `Host` + WebRTC — else, if enabled and all support host+webrtc.
/// 3. `Host` + Direct — else, if enabled and all support host+direct (LAN).
/// 4. `Relay` + Relay — the universal floor.
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

    let (topology, transport) = if desired == Topology::Mesh
        && cfg.enable_webrtc
        && all_support(&members, Topology::Mesh, Transport::WebRtc)
    {
        (Topology::Mesh, Transport::WebRtc)
    } else if desired == Topology::Host
        && cfg.enable_webrtc
        && all_support(&members, Topology::Host, Transport::WebRtc)
    {
        (Topology::Host, Transport::WebRtc)
    } else if desired == Topology::Host
        && cfg.enable_direct
        && all_support(&members, Topology::Host, Transport::Direct)
    {
        (Topology::Host, Transport::Direct)
    } else {
        (Topology::Relay, Transport::Relay)
    };

    let host = if topology == Topology::Host {
        elect_host(authority, &members)
    } else {
        None
    };

    let ice_servers = if transport == Transport::WebRtc {
        cfg.ice_servers.clone()
    } else {
        Vec::new()
    };

    SessionPlanDecision {
        topology,
        transport,
        host,
        ice_servers,
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
    /// Build the per-recipient [`SessionPlanPayload`] for `recipient` (Appendix E).
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
    pub(crate) fn plan_for(&self, recipient: PlayerId) -> SessionPlanPayload {
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
            ice_servers: self.ice_servers.clone(),
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

        // Relay floor: no SessionPlan is sent. v3 clients fall back to relaying
        // game data exactly like v2 (the floor never closes).
        if decision.topology == Topology::Relay {
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

        for member in &decision.members {
            // Defense-in-depth gate: in a non-relay plan all members are v3 by
            // construction (`all_support` requires v3), but mirror signaling.rs's
            // airtight gating so a non-v3 connection can never be sent a plan.
            if !self.client_supports_v3(&member.player_id) {
                continue;
            }

            let plan = decision.plan_for(member.player_id);
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
    }
}
