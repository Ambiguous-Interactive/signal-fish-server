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
}
