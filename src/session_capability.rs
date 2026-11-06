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
