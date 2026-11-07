//! Production session membership and host readiness.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::session_capability::{SessionCapabilities, Topology, Transport};

/// A syntactically usable direct host endpoint carried by a v3
/// [`crate::protocol::SessionPlanPayload`].
///
/// This type deliberately models only the connect target. It does not claim
/// that the self-declared address is reachable from another room member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectEndpoint {
    pub host: String,
    pub port: u16,
}

/// Per-member input to session selection: identity + negotiated capabilities +
/// join time (for deterministic host election).
///
/// `Debug` is required by the proptest strategies in `session_policy_tests.rs`
/// (generated values must be printable in failure reports).
#[derive(Debug, Clone)]
pub(crate) struct SessionMember {
    pub player_id: Uuid,
    pub player_name: String,
    pub is_authority: bool,
    pub joined_at: chrono::DateTime<chrono::Utc>,
    pub direct_endpoint: Option<DirectEndpoint>,
    pub capabilities: SessionCapabilities,
}

impl SessionMember {
    /// Whether this member negotiated everything required to run a session on
    /// the given (topology, transport) pair: protocol v3 plus both axes.
    ///
    /// The capability predicate shared by plan selection (`all_support`) and
    /// per-recipient peer-list filtering (`SessionPlanDecision::plan_for`).
    /// Host validity and election wrap this with [`Self::can_host`], which adds
    /// Direct endpoint readiness.
    pub(crate) fn supports_session(&self, topology: Topology, transport: Transport) -> bool {
        crate::session_capability::supports_session(self.capabilities, topology, transport)
    }

    /// Whether this member can anchor the selected host transport. Direct
    /// requires a usable endpoint in addition to negotiated capabilities;
    /// WebRTC carries its own connection establishment through signaling.
    pub(crate) fn can_host(&self, topology: Topology, transport: Transport) -> bool {
        self.supports_session(topology, transport)
            && (transport != Transport::Direct || self.direct_endpoint.is_some())
    }
}

/// Whether *every* member supports v3 and the given (topology, transport) pair.
///
/// An empty room never supports an upgrade (there is nothing to connect).
pub(crate) fn all_support(
    members: &[SessionMember],
    topology: Topology,
    transport: Transport,
) -> bool {
    crate::session_capability::all_support(
        members,
        |member| member.capabilities,
        topology,
        transport,
    )
}

/// Whether a capability-compatible rung has every piece needed to execute it.
/// Only `host + direct` has an additional requirement: at least one capable
/// member must have provided a syntactically usable connect endpoint.
pub(crate) fn rung_is_executable(
    members: &[SessionMember],
    topology: Topology,
    transport: Transport,
) -> bool {
    transport != Transport::Direct
        || members
            .iter()
            .any(|member| member.can_host(topology, transport))
}

/// The session-upgrade ladder (ADR-0001 §1), richest rung first.
///
/// Each entry is a legal `(topology, transport)` pair the room may settle on, in
/// strict preference order. The session selector selects the first rung that
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

/// Whether the switches permit the data-path transport. `Relay` is the mandatory
/// floor (always permitted); `Direct` and `WebRtc` are opt-in upgrade gates.
const fn transport_enabled(enable_direct: bool, enable_webrtc: bool, transport: Transport) -> bool {
    match transport {
        Transport::Relay => true,
        Transport::Direct => enable_direct,
        Transport::WebRtc => enable_webrtc,
    }
}

/// Select the richest executable pair within the configured ceiling.
pub(crate) fn select_session_pair(
    members: &[SessionMember],
    desired: Topology,
    enable_direct: bool,
    enable_webrtc: bool,
) -> (Topology, Transport) {
    UPGRADE_LADDER
        .into_iter()
        .find(|&(topology, transport)| {
            topology_rank(topology) <= topology_rank(desired)
                && transport_enabled(enable_direct, enable_webrtc, transport)
                && all_support(members, topology, transport)
                && rung_is_executable(members, topology, transport)
        })
        .unwrap_or(RELAY_FLOOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1464: endpoints alone cannot make a Direct rung executable.
    #[test]
    fn executable_rungs_require_a_capable_direct_host_in_any_position() {
        let topologies = [Topology::Relay, Topology::Host, Topology::Mesh];
        let transports = [Transport::Relay, Transport::Direct, Transport::WebRtc];
        let unready = SessionMember {
            player_id: Uuid::nil(),
            player_name: String::new(),
            is_authority: false,
            joined_at: chrono::DateTime::UNIX_EPOCH,
            direct_endpoint: None,
            capabilities: SessionCapabilities::default(),
        };
        let endpoint = DirectEndpoint {
            host: "127.0.0.1".into(),
            port: 9000,
        };
        for version in [0, 1, 2, 3, 4, u16::MAX] {
            for mask in 0u8..64 {
                for has_endpoint in [false, true] {
                    let candidate = SessionMember {
                        capabilities: SessionCapabilities {
                            version,
                            relay_transport: mask & 1 != 0,
                            direct_transport: mask & 2 != 0,
                            webrtc_transport: mask & 4 != 0,
                            relay_topology: mask & 8 != 0,
                            host_topology: mask & 16 != 0,
                            mesh_topology: mask & 32 != 0,
                        },
                        direct_endpoint: has_endpoint.then(|| endpoint.clone()),
                        ..unready.clone()
                    };
                    for (index, topology) in topologies.iter().copied().enumerate() {
                        for transport in transports {
                            let eligible = version >= 3
                                && mask & 2 != 0
                                && mask & (1 << (index + 3)) != 0
                                && has_endpoint;
                            for (members, direct_ready) in [
                                (vec![], false),
                                (vec![unready.clone()], false),
                                (vec![candidate.clone()], eligible),
                                (vec![unready.clone(), candidate.clone()], eligible),
                                (vec![candidate.clone(), unready.clone()], eligible),
                                (vec![candidate.clone(), candidate.clone()], eligible),
                            ] {
                                assert_eq!(rung_is_executable(&members, topology, transport),
                                    transport != Transport::Direct || direct_ready,
                                    "version={version} mask={mask} endpoint={has_endpoint} topology={topology:?} transport={transport:?} members={members:?}");
                            }
                        }
                    }
                }
            }
        }
    }

    /// Independent ADR-0001 oracle: literal legal pairs, explicit ceiling cases,
    /// and direct field reads avoid inheriting production ladder or gate errors.
    fn expected_session_pair(
        members: &[SessionMember],
        desired: Topology,
        direct_enabled: bool,
        webrtc_enabled: bool,
    ) -> (Topology, Transport) {
        [
            (Topology::Mesh, Transport::WebRtc),
            (Topology::Host, Transport::WebRtc),
            (Topology::Host, Transport::Direct),
        ]
        .into_iter()
        .find(|&(topology, transport)| {
            let allowed = match desired {
                Topology::Relay => false,
                Topology::Host => topology == Topology::Host,
                Topology::Mesh => true,
            };
            let enabled = match transport {
                Transport::WebRtc => webrtc_enabled,
                Transport::Direct => direct_enabled,
                Transport::Relay => unreachable!("The upgrade oracle has no relay rung"),
            };
            allowed
                && enabled
                && !members.is_empty()
                && members.iter().all(|member| {
                    let caps = member.capabilities;
                    caps.version >= 3
                        && match topology {
                            Topology::Host => caps.host_topology,
                            Topology::Mesh => caps.mesh_topology,
                            Topology::Relay => false,
                        }
                        && match transport {
                            Transport::WebRtc => caps.webrtc_transport,
                            Transport::Direct => caps.direct_transport,
                            Transport::Relay => false,
                        }
                })
                && (transport == Transport::WebRtc
                    || members
                        .iter()
                        .any(|member| member.direct_endpoint.is_some()))
        })
        .unwrap_or((Topology::Relay, Transport::Relay))
    }

    /// #1464: the whole production pair selector must enforce the ADR waterfall.
    #[test]
    fn session_pair_matches_protocol_for_capabilities_ceiling_and_switches() {
        let legacy = SessionMember {
            player_id: Uuid::nil(),
            player_name: String::new(),
            is_authority: false,
            joined_at: chrono::DateTime::UNIX_EPOCH,
            direct_endpoint: None,
            capabilities: SessionCapabilities::default(),
        };
        let full = SessionMember {
            player_id: Uuid::from_u128(1),
            capabilities: SessionCapabilities {
                version: 3,
                relay_transport: true,
                direct_transport: true,
                webrtc_transport: true,
                relay_topology: true,
                host_topology: true,
                mesh_topology: true,
            },
            ..legacy.clone()
        };
        let endpoint = DirectEndpoint {
            host: "127.0.0.1".into(),
            port: 9000,
        };
        let ready = SessionMember {
            direct_endpoint: Some(endpoint.clone()),
            ..full.clone()
        };
        for version in [0, 1, 2, 3, 4, u16::MAX] {
            for mask in 0u8..64 {
                for has_endpoint in [false, true] {
                    let candidate = SessionMember {
                        player_id: Uuid::from_u128(2),
                        capabilities: SessionCapabilities {
                            version,
                            relay_transport: mask & 1 != 0,
                            direct_transport: mask & 2 != 0,
                            webrtc_transport: mask & 4 != 0,
                            relay_topology: mask & 8 != 0,
                            host_topology: mask & 16 != 0,
                            mesh_topology: mask & 32 != 0,
                        },
                        direct_endpoint: has_endpoint.then(|| endpoint.clone()),
                        ..legacy.clone()
                    };
                    for members in [
                        vec![],
                        vec![candidate.clone()],
                        vec![full.clone(), candidate.clone()],
                        vec![candidate.clone(), full.clone()],
                        vec![ready.clone(), candidate.clone()],
                        vec![candidate.clone(), ready.clone()],
                        vec![candidate.clone(), candidate.clone()],
                        vec![legacy.clone(), candidate.clone()],
                        vec![candidate.clone(), legacy.clone()],
                    ] {
                        for desired in [Topology::Relay, Topology::Host, Topology::Mesh] {
                            for direct in [false, true] {
                                for webrtc in [false, true] {
                                    assert_eq!(
                                        select_session_pair(&members, desired, direct, webrtc),
                                        expected_session_pair(&members, desired, direct, webrtc),
                                        "version={version} mask={mask} endpoint={has_endpoint} \
                                         desired={desired:?} direct={direct} webrtc={webrtc} \
                                         members={members:?}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
