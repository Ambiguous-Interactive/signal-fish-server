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
