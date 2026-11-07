//! Sticky-session admission over the stored production plan and connection profile.

use uuid::Uuid;

use crate::session_capability::{NegotiatedProtocol, Topology, Transport};
use crate::session_member::SessionMember;

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

impl ActiveSessionPlan {
    /// Whether a host-topology session has no usable stored host because it is
    /// absent, lacks the negotiated capabilities, or has no usable Direct
    /// endpoint. Membership refreshes and Direct host endpoint updates resolve
    /// this state before publishing a new authoritative plan.
    pub(crate) fn host_invalid<'a>(
        &self,
        members: impl IntoIterator<Item = &'a SessionMember>,
    ) -> bool {
        self.topology == Topology::Host
            && !self.host.is_some_and(|host| {
                members
                    .into_iter()
                    .any(|member| member.player_id == host && self.supported_by(member))
            })
    }

    /// Whether `member` negotiated everything required to participate in — and
    /// in particular to *host* — this stored session: protocol v3 plus the
    /// sticky (topology, transport) pair, and a usable endpoint for Direct.
    pub(crate) fn supported_by(&self, member: &SessionMember) -> bool {
        member.can_host(self.topology, self.transport)
    }
}

/// Deterministic sticky plan update after a membership or endpoint change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MembershipPlan {
    pub plan: ActiveSessionPlan,
    /// None keeps state; Some(Some(plan)) replaces it; Some(None) removes it.
    pub active_plan_update: Option<Option<ActiveSessionPlan>>,
    pub is_replan: bool,
}

/// Both passes must enumerate the same snapshot. Production passes a shared slice,
/// whose clone copies the reference and preserves member order and fields.
pub(crate) fn resolve_membership_plan<'a>(
    stored: Option<ActiveSessionPlan>,
    authority: Option<Uuid>,
    members: impl IntoIterator<Item = &'a SessionMember> + Clone,
) -> MembershipPlan {
    let relay = ActiveSessionPlan {
        topology: Topology::Relay,
        transport: Transport::Relay,
        host: None,
    };
    let Some(stored) = stored else {
        return MembershipPlan {
            plan: relay,
            active_plan_update: None,
            is_replan: false,
        };
    };
    if !stored.host_invalid(members.clone()) {
        return MembershipPlan {
            plan: stored,
            active_plan_update: None,
            is_replan: false,
        };
    }
    let host = crate::session_member::elect_eligible_host(
        authority,
        members,
        stored.topology,
        stored.transport,
    );
    if let Some(host) = host {
        let updated = ActiveSessionPlan {
            host: Some(host),
            ..stored
        };
        MembershipPlan {
            plan: updated,
            active_plan_update: Some(Some(updated)),
            is_replan: true,
        }
    } else {
        MembershipPlan {
            plan: relay,
            active_plan_update: Some(None),
            is_replan: false,
        }
    }
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

    /// #1464: independent protocol oracle covers every branch and stored pair.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn membership_resolution_matches_sticky_state_oracle() {
        use crate::session_capability::SessionCapabilities;
        use crate::session_member::DirectEndpoint;

        let axes = [Topology::Relay, Topology::Host, Topology::Mesh];
        let paths = [Transport::Relay, Transport::Direct, Transport::WebRtc];
        let ids = [Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3)];
        let relay = ActiveSessionPlan {
            topology: Topology::Relay,
            transport: Transport::Relay,
            host: None,
        };
        for version in [0, 2, 3, 4, u16::MAX] {
            for mask in 0_u8..64 {
                for endpoint in [false, true] {
                    let candidate = SessionMember {
                        player_id: ids[0],
                        player_name: String::new(),
                        is_authority: false,
                        joined_at: chrono::DateTime::UNIX_EPOCH,
                        direct_endpoint: endpoint.then(|| DirectEndpoint {
                            host: "127.0.0.1".into(),
                            port: 9000,
                        }),
                        capabilities: SessionCapabilities {
                            version,
                            relay_transport: mask & 1 != 0,
                            direct_transport: mask & 2 != 0,
                            webrtc_transport: mask & 4 != 0,
                            relay_topology: mask & 8 != 0,
                            host_topology: mask & 16 != 0,
                            mesh_topology: mask & 32 != 0,
                        },
                    };
                    let capable = SessionMember {
                        player_id: ids[1],
                        joined_at: chrono::DateTime::UNIX_EPOCH + chrono::Duration::seconds(1),
                        direct_endpoint: Some(DirectEndpoint {
                            host: "127.0.0.1".into(),
                            port: 9001,
                        }),
                        capabilities: SessionCapabilities {
                            version: 3,
                            relay_transport: true,
                            direct_transport: true,
                            webrtc_transport: true,
                            relay_topology: true,
                            host_topology: true,
                            mesh_topology: true,
                        },
                        ..candidate.clone()
                    };
                    for members in [
                        vec![],
                        vec![candidate.clone()],
                        vec![candidate.clone(), capable.clone()],
                        vec![capable, candidate.clone()],
                    ] {
                        for (ti, topology) in axes.into_iter().enumerate() {
                            for (pi, transport) in paths.into_iter().enumerate() {
                                let eligible = |member: &&SessionMember| {
                                    member.player_id == ids[1]
                                        || (version >= 3
                                            && mask & (1 << (ti + 3)) != 0
                                            && mask & (1 << pi) != 0
                                            && (transport != Transport::Direct || endpoint))
                                };
                                for host in [None, Some(ids[0]), Some(ids[1]), Some(ids[2])] {
                                    for authority in
                                        [None, Some(ids[0]), Some(ids[1]), Some(ids[2])]
                                    {
                                        for stored in [
                                            None,
                                            Some(ActiveSessionPlan {
                                                topology,
                                                transport,
                                                host,
                                            }),
                                        ] {
                                            let mut candidates: Vec<_> =
                                                members.iter().filter(eligible).collect();
                                            candidates.sort_by_key(|member| {
                                                (
                                                    Some(member.player_id) != authority,
                                                    member.joined_at,
                                                    member.player_id,
                                                )
                                            });
                                            let valid = topology != Topology::Host
                                                || candidates
                                                    .iter()
                                                    .any(|member| Some(member.player_id) == host);
                                            let expected = match stored {
                                                None => MembershipPlan {
                                                    plan: relay,
                                                    active_plan_update: None,
                                                    is_replan: false,
                                                },
                                                Some(plan) if valid => MembershipPlan {
                                                    plan,
                                                    active_plan_update: None,
                                                    is_replan: false,
                                                },
                                                Some(plan) => match candidates.first() {
                                                    Some(member) => {
                                                        let updated = ActiveSessionPlan {
                                                            host: Some(member.player_id),
                                                            ..plan
                                                        };
                                                        MembershipPlan {
                                                            plan: updated,
                                                            active_plan_update: Some(Some(updated)),
                                                            is_replan: true,
                                                        }
                                                    }
                                                    None => MembershipPlan {
                                                        plan: relay,
                                                        active_plan_update: Some(None),
                                                        is_replan: false,
                                                    },
                                                },
                                            };
                                            assert_eq!(resolve_membership_plan(stored, authority, members.as_slice()), expected,
                                                "version={version}, mask={mask}, endpoint={endpoint}, stored={stored:?}, authority={authority:?}, members={members:?}");
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// #1464: a capable bystander cannot make a missing or incapable host valid.
    #[test]
    fn host_validity_requires_saved_identity_capabilities_and_direct_endpoint() {
        use crate::session_capability::SessionCapabilities;
        use crate::session_member::DirectEndpoint;

        let topologies = [Topology::Relay, Topology::Host, Topology::Mesh];
        let transports = [Transport::Relay, Transport::Direct, Transport::WebRtc];
        let host_id = Uuid::nil();
        let other_id = Uuid::from_u128(u128::MAX);
        let member = |player_id, capabilities, has_endpoint| SessionMember {
            player_id,
            player_name: String::new(),
            is_authority: false,
            joined_at: chrono::DateTime::UNIX_EPOCH,
            direct_endpoint: if has_endpoint {
                Some(DirectEndpoint {
                    host: "127.0.0.1".into(),
                    port: 9000,
                })
            } else {
                None
            },
            capabilities,
        };
        let bystander = member(
            other_id,
            SessionCapabilities {
                version: u16::MAX,
                relay_transport: true,
                direct_transport: true,
                webrtc_transport: true,
                relay_topology: true,
                host_topology: true,
                mesh_topology: true,
            },
            true,
        );
        for version in [0, 1, 2, 3, 4, u16::MAX] {
            for mask in 0u8..64 {
                for has_endpoint in [false, true] {
                    let candidate = member(
                        host_id,
                        SessionCapabilities {
                            version,
                            relay_transport: mask & 1 != 0,
                            direct_transport: mask & 2 != 0,
                            webrtc_transport: mask & 4 != 0,
                            relay_topology: mask & 8 != 0,
                            host_topology: mask & 16 != 0,
                            mesh_topology: mask & 32 != 0,
                        },
                        has_endpoint,
                    );
                    for topology in topologies {
                        for (index, transport) in transports.iter().copied().enumerate() {
                            let eligible = version >= 3
                                && mask & 16 != 0
                                && mask & (1 << index) != 0
                                && (transport != Transport::Direct || has_endpoint);
                            for host in [None, Some(host_id), Some(other_id)] {
                                let stored = ActiveSessionPlan {
                                    topology,
                                    transport,
                                    host,
                                };
                                let cases = [
                                    (vec![], false),
                                    (vec![candidate.clone()], host == Some(host_id) && eligible),
                                    (vec![bystander.clone()], host == Some(other_id)),
                                    (
                                        vec![candidate.clone(), bystander.clone()],
                                        host == Some(other_id)
                                            || (host == Some(host_id) && eligible),
                                    ),
                                    (
                                        vec![bystander.clone(), candidate.clone()],
                                        host == Some(other_id)
                                            || (host == Some(host_id) && eligible),
                                    ),
                                ];
                                for (members, has_host) in cases {
                                    assert_eq!(stored.host_invalid(&members),
                                        topology == Topology::Host && !has_host,
                                        "stored={stored:?} version={version} mask={mask} endpoint={has_endpoint} members={members:?}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

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
