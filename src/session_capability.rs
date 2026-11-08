//! Session capability decisions and transport-status updates shared with source-linked proofs.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

/// Resolve the negotiated transport/topology capability sets for a connection.
///
/// Relay is the universal floor, so it is always present in both sets. A
/// connection negotiated below v3 is **relay-only** regardless of what it
/// advertised — peer-to-peer transports/topologies are a v3+ upgrade and must
/// never be recorded for a v2 peer. For v3+, the client's advertised sets are
/// used with `Relay` forced in (if the client omitted it) and duplicates
/// removed while preserving the client's stated preference order.
pub(crate) fn negotiate_capabilities(
    negotiated_version: u16,
    supported_transports: Option<Vec<Transport>>,
    supported_topologies: Option<Vec<Topology>>,
) -> (Vec<Transport>, Vec<Topology>) {
    if negotiated_version < 3 {
        return (vec![Transport::Relay], vec![Topology::Relay]);
    }

    let mut transports = supported_transports.unwrap_or_else(|| vec![Transport::Relay]);
    if !transports.contains(&Transport::Relay) {
        transports.push(Transport::Relay);
    }
    dedup_preserving_order(&mut transports);

    let mut topologies = supported_topologies.unwrap_or_else(|| vec![Topology::Relay]);
    if !topologies.contains(&Topology::Relay) {
        topologies.push(Topology::Relay);
    }
    dedup_preserving_order(&mut topologies);

    (transports, topologies)
}

/// Remove duplicate entries while preserving the first occurrence of each.
///
/// Unlike [`Vec::dedup`], which only collapses *consecutive* duplicates, this
/// drops every later repeat regardless of position. The lists it operates on
/// (negotiated transports/topologies) have at most three variants, so the
/// quadratic scan is trivially cheap and keeps the relative ordering the client
/// advertised (which conveys preference for the P3 selection logic).
pub(crate) fn dedup_preserving_order<T: PartialEq>(items: &mut Vec<T>) {
    let mut unique = Vec::with_capacity(items.len());
    for item in items.drain(..) {
        if !unique.contains(&item) {
            unique.push(item);
        }
    }
    *items = unique;
}

/// Result of a negotiated transport-status report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportStatusUpdate {
    Changed,
    Duplicate,
    MissingConnection,
    UnsupportedProtocolVersion,
    UnsupportedTransport,
}

/// Validate a report and update only the current membership's transport state.
/// The caller owns the connection lookup and holds its mutation guard.
pub(crate) fn update_transport_status(
    protocol: &NegotiatedProtocol,
    membership_generation: Uuid,
    transport_status: &mut Option<(Uuid, Transport, bool)>,
    transport: Transport,
    connected: bool,
) -> TransportStatusUpdate {
    if protocol.version < 3 {
        return TransportStatusUpdate::UnsupportedProtocolVersion;
    }
    if !protocol.transports.contains(&transport) {
        return TransportStatusUpdate::UnsupportedTransport;
    }
    let duplicate = match *transport_status {
        Some((generation, previous_transport, previous_connected)) => {
            generation == membership_generation
                && previous_transport == transport
                && previous_connected == connected
        }
        None => false,
    };
    if duplicate {
        return TransportStatusUpdate::Duplicate;
    }
    *transport_status = Some((membership_generation, transport, connected));
    TransportStatusUpdate::Changed
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

    /// #1464: every rejection preserves state; accepted reports belong to the current generation.
    #[test]
    fn transport_status_update_matches_negotiation_and_membership_contract() {
        use super::{update_transport_status, TransportStatusUpdate};

        let current = uuid::Uuid::nil();
        let previous = uuid::Uuid::from_u128(u128::MAX);
        let transports = [Transport::Relay, Transport::Direct, Transport::WebRtc];
        let mut prior_states = vec![None];
        for generation in [current, previous] {
            for transport in transports {
                for connected in [false, true] {
                    prior_states.push(Some((generation, transport, connected)));
                }
            }
        }
        for version in [0, 2, 3, 4, u16::MAX] {
            for mask in 0_u8..8 {
                let protocol = NegotiatedProtocol {
                    version,
                    transports: transports
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| mask & (1 << index) != 0)
                        .flat_map(|(_, transport)| [*transport, *transport])
                        .collect(),
                    // Transport reports do not require a negotiated topology.
                    topologies: vec![],
                };
                for (index, transport) in transports.into_iter().enumerate() {
                    for connected in [false, true] {
                        for &prior in &prior_states {
                            let accepted = version >= 3 && mask & (1 << index) != 0;
                            let report = Some((current, transport, connected));
                            let expected_outcome = match (version >= 3, accepted, prior == report) {
                                (false, _, _) => TransportStatusUpdate::UnsupportedProtocolVersion,
                                (true, false, _) => TransportStatusUpdate::UnsupportedTransport,
                                (true, true, true) => TransportStatusUpdate::Duplicate,
                                (true, true, false) => TransportStatusUpdate::Changed,
                            };
                            let expected_state = if accepted { report } else { prior };
                            let mut state = prior;
                            let outcome = update_transport_status(
                                &protocol, current, &mut state, transport, connected,
                            );
                            assert_eq!(
                                (outcome, state),
                                (expected_outcome, expected_state),
                                "version={version} mask={mask} prior={prior:?} transport={transport:?} connected={connected}"
                            );
                            let repeated = update_transport_status(
                                &protocol, current, &mut state, transport, connected,
                            );
                            assert_eq!(
                                (repeated, state),
                                (
                                    if accepted {
                                        TransportStatusUpdate::Duplicate
                                    } else {
                                        expected_outcome
                                    },
                                    expected_state
                                ),
                                "repeat version={version} mask={mask} prior={prior:?} transport={transport:?} connected={connected}"
                            );
                        }
                    }
                }
            }
        }
    }

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
