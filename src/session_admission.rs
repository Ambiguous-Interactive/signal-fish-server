//! Sticky-session admission over the stored production plan and connection profile.

use uuid::Uuid;

use crate::session_capability::{NegotiatedProtocol, Topology, Transport};

/// The sticky per-room session decision recorded when a room finalizes to a
/// non-relay plan. Relay-floor rooms record nothing; a later reconnect derives
/// its explicit Relay/Relay refresh from that absence rather than sticky state.
///
/// This is what the room is *actually running*: membership and Direct endpoint
/// refreshes consult it rather than re-running
/// `choose_session_plan` over the current member list, because the live
/// membership can drift from the finalize-time membership (departures open
/// seats; reconnects can re-seat an incumbent with downgraded capabilities)
/// and a recompute could contradict the session every member already
/// configured. New seat-fills cannot drift the membership: the admission gate
/// (`joiner_supports_sticky_plan`, issue #421) rejects joiners that cannot
/// run the stored pair.
///
/// Topology and transport are immutable for the session lifetime; only `host`
/// is rewritten by execution-aware host re-election.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActiveSessionPlan {
    pub topology: Topology,
    pub transport: Transport,
    pub host: Option<Uuid>,
}

/// Whether a joiner's negotiated capabilities satisfy a stored sticky session
/// — the seat-fill admission predicate (issue #421).
///
/// A finalized non-relay session runs one (topology, transport) pair every
/// finalize-time member negotiated (`all_support`). Admitting a joiner that
/// cannot run that pair would silently split the room's data path: the
/// joiner's WebSocket-relayed `GameData` reaches everyone, but the capable
/// members' peer-to-peer traffic never reaches it (`SessionPlanDecision::plan_for`
/// filters both sides), so it would play on permanently divergent state. The
/// admission gate therefore applies the same membership predicate selection
/// used at finalize: protocol v3 plus both axes. Direct endpoints are NOT part
/// of this predicate (only an elected host needs one), matching
/// `SessionMember::supports_session`.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// #1464: check stored axes and host independence against an independent set oracle.
    #[test]
    fn admission_requires_version_and_exact_stored_axes_for_every_subset() {
        let topologies = [Topology::Relay, Topology::Host, Topology::Mesh];
        let transports = [Transport::Relay, Transport::Direct, Transport::WebRtc];
        for version in [0, 1, 2, 3, 4, u16::MAX] {
            for mask in 0u8..64 {
                let profile = NegotiatedProtocol {
                    version,
                    transports: transports
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| mask & (1 << index) != 0)
                        .map(|(_, transport)| *transport)
                        .collect(),
                    topologies: topologies
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| mask & (1 << (index + 3)) != 0)
                        .map(|(_, topology)| *topology)
                        .collect(),
                };
                for (topology_index, topology) in topologies.iter().enumerate() {
                    for (transport_index, transport) in transports.iter().enumerate() {
                        let expected = version >= 3
                            && mask & (1 << (topology_index + 3)) != 0
                            && mask & (1 << transport_index) != 0;
                        for host in [None, Some(Uuid::nil()), Some(Uuid::from_u128(u128::MAX))] {
                            let stored = ActiveSessionPlan {
                                topology: *topology,
                                transport: *transport,
                                host,
                            };
                            assert_eq!(
                                joiner_supports_sticky_plan(&profile, &stored),
                                expected,
                                "version={version} mask={mask} stored={stored:?}"
                            );
                        }
                    }
                }
            }
        }
    }
}
