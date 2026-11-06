//! Pure session capability decisions shared with source-linked proofs.

use serde::{Deserialize, Serialize};

/// Data-path transport a client can use for game traffic.
///
/// `Relay` is the universal floor (WebSocket fan-out through the server) and is
/// always available. `Direct` is a LAN / routable IP:port path and `WebRtc` is a
/// peer-to-peer WebRTC data channel; both are opt-in upgrades negotiated in
/// `Authenticate`. Protocol v3 wire tokens: `relay`, `direct`, `webrtc`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// Server WebSocket fan-out — the mandatory floor every client supports.
    Relay,
    /// Direct IP:port connection (LAN / routable host).
    Direct,
    /// Peer-to-peer WebRTC data channel.
    ///
    /// `rename_all = "snake_case"` would emit `web_rtc`; the wire contract requires the
    /// token `webrtc`, so the variant is renamed explicitly.
    #[serde(rename = "webrtc")]
    WebRtc,
}

/// Session topology negotiated for a room.
///
/// `Relay` keeps the v2 server-relay hub. `Host` is a star topology around a
/// single authoritative peer, and `Mesh` is everyone-to-everyone. Protocol v3
/// wire tokens: `relay`, `host`, `mesh`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Topology {
    /// Server relay hub (v2 behavior, always available).
    Relay,
    /// Star topology around a single host/authority.
    Host,
    /// Full mesh: every peer connects to every other peer.
    Mesh,
}

/// Protocol capabilities negotiated for a single connection during `Authenticate`.
///
/// The default is a pure v2 client: protocol version 2, relay-only transport and
/// relay-only topology. v3 negotiation overwrites this when the connection manager stores the profile.
#[derive(Debug, Clone)]
pub(crate) struct NegotiatedProtocol {
    pub version: u16,
    pub transports: Vec<Transport>,
    /// Session topologies the client supports; consumed by the P3 session-plan
    /// selection path (`session_policy::choose_session_plan`).
    pub topologies: Vec<Topology>,
}

impl Default for NegotiatedProtocol {
    fn default() -> Self {
        Self {
            version: crate::protocol_core::SERVER_MIN_PROTOCOL_VERSION,
            transports: vec![Transport::Relay],
            topologies: vec![Topology::Relay],
        }
    }
}

/// Version and set membership used by live session selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionCapabilities {
    pub version: u16,
    pub relay_transport: bool,
    pub direct_transport: bool,
    pub webrtc_transport: bool,
    pub relay_topology: bool,
    pub host_topology: bool,
    pub mesh_topology: bool,
}

impl Default for SessionCapabilities {
    fn default() -> Self {
        resolve_connection_capabilities(None)
    }
}

impl From<&NegotiatedProtocol> for SessionCapabilities {
    fn from(protocol: &NegotiatedProtocol) -> Self {
        crate::session_capability::project_capabilities(
            protocol.version,
            &protocol.transports,
            &protocol.topologies,
        )
    }
}

impl SessionCapabilities {
    pub(crate) fn supports_transport(self, transport: Transport) -> bool {
        match transport {
            Transport::Relay => self.relay_transport,
            Transport::Direct => self.direct_transport,
            Transport::WebRtc => self.webrtc_transport,
        }
    }

    pub(crate) fn supports_topology(self, topology: Topology) -> bool {
        match topology {
            Topology::Relay => self.relay_topology,
            Topology::Host => self.host_topology,
            Topology::Mesh => self.mesh_topology,
        }
    }
}

/// Keep a known profile, or use the relay-only floor when lookup returns no profile.
pub(crate) fn resolve_capabilities(
    capabilities: Option<SessionCapabilities>,
    minimum: u16,
) -> SessionCapabilities {
    match capabilities {
        Some(capabilities) => capabilities,
        None => SessionCapabilities {
            version: minimum,
            relay_transport: true,
            direct_transport: false,
            webrtc_transport: false,
            relay_topology: true,
            host_topology: false,
            mesh_topology: false,
        },
    }
}

/// Resolve a live connection profile using this server's fixed relay floor.
pub(crate) fn resolve_connection_capabilities(
    capabilities: Option<SessionCapabilities>,
) -> SessionCapabilities {
    resolve_capabilities(
        capabilities,
        crate::protocol_core::SERVER_MIN_PROTOCOL_VERSION,
    )
}

/// Require protocol v3 or higher and both requested capability axes.
pub(crate) fn supports_session(
    capabilities: SessionCapabilities,
    topology: Topology,
    transport: Transport,
) -> bool {
    capabilities.version >= 3
        && capabilities.supports_topology(topology)
        && capabilities.supports_transport(transport)
}

/// Preserve the version and project axis membership, independent of order or duplicates.
pub(crate) fn project_capabilities(
    version: u16,
    transports: &[Transport],
    topologies: &[Topology],
) -> SessionCapabilities {
    SessionCapabilities {
        version,
        relay_transport: transports.contains(&Transport::Relay),
        direct_transport: transports.contains(&Transport::Direct),
        webrtc_transport: transports.contains(&Transport::WebRtc),
        relay_topology: topologies.contains(&Topology::Relay),
        host_topology: topologies.contains(&Topology::Host),
        mesh_topology: topologies.contains(&Topology::Mesh),
    }
}

/// Require a nonempty room and support from each member's capabilities.
pub(crate) fn all_support<T>(
    members: &[T],
    capabilities: impl Fn(&T) -> SessionCapabilities,
    topology: Topology,
    transport: Transport,
) -> bool {
    crate::protocol_core::nonempty_all_support(members, |member| {
        supports_session(capabilities(member), topology, transport)
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::{all_support, NegotiatedProtocol, SessionCapabilities, Topology, Transport};

    /// The server floor and both live default representations retain the v2 contract.
    #[test]
    fn negotiated_and_compact_defaults_keep_v2_relay_floor() {
        assert_eq!(crate::protocol_core::SERVER_MIN_PROTOCOL_VERSION, 2);
        let profile = NegotiatedProtocol::default();
        assert_eq!(profile.version, 2);
        assert_eq!(profile.transports, vec![Transport::Relay]);
        assert_eq!(profile.topologies, vec![Topology::Relay]);
        let compact = SessionCapabilities::default();
        assert_eq!(SessionCapabilities::from(&profile), compact);
        for topology in [Topology::Relay, Topology::Host, Topology::Mesh] {
            for transport in [Transport::Relay, Transport::Direct, Transport::WebRtc] {
                assert!(!super::supports_session(compact, topology, transport));
            }
        }
    }

    /// Check that aggregation reads no members after the first rejection.
    #[test]
    fn concrete_aggregation_reads_members_once_until_first_rejection() {
        for (members, expected, visited) in [
            (vec![], false, vec![]),
            (vec![3, u16::MAX], true, vec![3, u16::MAX]),
            (vec![3, 2, u16::MAX], false, vec![3, 2]),
            (vec![2, 3], false, vec![2]),
        ] {
            let reads = RefCell::new(Vec::new());
            let actual = all_support(
                &members,
                |version| {
                    reads.borrow_mut().push(*version);
                    SessionCapabilities {
                        version: *version,
                        relay_transport: false,
                        direct_transport: false,
                        webrtc_transport: true,
                        relay_topology: false,
                        host_topology: false,
                        mesh_topology: true,
                    }
                },
                Topology::Mesh,
                Transport::WebRtc,
            );
            assert_eq!(actual, expected, "members={members:?}");
            assert_eq!(*reads.borrow(), visited, "members={members:?}");
        }
    }
}
