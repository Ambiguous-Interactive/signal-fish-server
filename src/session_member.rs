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

/// Elect the host for a `host`-topology room.
///
/// Prefers the explicitly designated `authority` *if that id is present in
/// `members`*; otherwise the earliest joiner (ties broken by the smaller
/// `player_id` for determinism); `None` for an empty room.
#[must_use]
pub(crate) fn elect_host<'a>(
    authority: Option<Uuid>,
    members: impl IntoIterator<Item = &'a SessionMember>,
) -> Option<Uuid> {
    let mut earliest: Option<(chrono::DateTime<chrono::Utc>, Uuid)> = None;
    for member in members {
        if Some(member.player_id) == authority {
            return Some(member.player_id);
        }
        if earliest.is_none_or(|candidate| (member.joined_at, member.player_id) < candidate) {
            earliest = Some((member.joined_at, member.player_id));
        }
    }
    earliest.map(|(_, player_id)| player_id)
}

/// Elect only members that can run the selected pair and expose any required
/// Direct endpoint. Authority preference never bypasses these requirements.
#[must_use]
pub(crate) fn elect_eligible_host<'a>(
    authority: Option<Uuid>,
    members: impl IntoIterator<Item = &'a SessionMember>,
    topology: Topology,
    transport: Transport,
) -> Option<Uuid> {
    elect_host(
        authority,
        members
            .into_iter()
            .filter(move |member| member.can_host(topology, transport)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1464: authority, version, both axes and Direct readiness use an independent set oracle.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn eligible_host_election_matches_capability_and_endpoint_oracle() {
        let topologies = [Topology::Relay, Topology::Host, Topology::Mesh];
        let transports = [Transport::Relay, Transport::Direct, Transport::WebRtc];
        for version in [0, 1, 2, 3, 4, u16::MAX] {
            for mask in 0_u8..64 {
                for has_endpoint in [false, true] {
                    let profile = SessionCapabilities {
                        version,
                        relay_transport: mask & 1 != 0,
                        direct_transport: mask & 2 != 0,
                        webrtc_transport: mask & 4 != 0,
                        relay_topology: mask & 8 != 0,
                        host_topology: mask & 16 != 0,
                        mesh_topology: mask & 32 != 0,
                    };
                    let member = SessionMember {
                        player_id: Uuid::from_u128(1),
                        player_name: String::new(),
                        is_authority: false,
                        joined_at: chrono::DateTime::UNIX_EPOCH,
                        direct_endpoint: has_endpoint.then(|| DirectEndpoint {
                            host: "127.0.0.1".into(),
                            port: 1,
                        }),
                        capabilities: profile,
                    };
                    for (topology_index, topology) in topologies.into_iter().enumerate() {
                        for (transport_index, transport) in transports.into_iter().enumerate() {
                            let eligible = version >= 3
                                && mask & (1 << (topology_index + 3)) != 0
                                && mask & (1 << transport_index) != 0
                                && (transport != Transport::Direct || has_endpoint);
                            let expected = eligible.then_some(member.player_id);
                            for authority in [None, Some(member.player_id), Some(Uuid::max())] {
                                assert_eq!(
                                    elect_eligible_host(
                                        authority,
                                        std::slice::from_ref(&member),
                                        topology,
                                        transport,
                                    ),
                                    expected,
                                    "version={version}, mask={mask}, endpoint={has_endpoint}, pair=({topology:?}, {transport:?}), authority={authority:?}",
                                );
                                assert_eq!(
                                    elect_eligible_host(authority, &[], topology, transport),
                                    None,
                                    "empty pair=({topology:?}, {transport:?}), authority={authority:?}",
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// #1464: ineligible authority cannot displace the least eligible timestamp/UUID.
    #[test]
    fn eligible_host_election_filters_before_authority_and_minimum_selection() {
        let complete = SessionCapabilities {
            version: 3,
            relay_transport: true,
            direct_transport: true,
            webrtc_transport: true,
            relay_topology: true,
            host_topology: true,
            mesh_topology: true,
        };
        let member = |identity, seconds, capabilities, has_endpoint: bool| SessionMember {
            player_id: Uuid::from_u128(identity),
            player_name: String::new(),
            is_authority: identity == 1,
            joined_at: chrono::DateTime::from_timestamp(seconds, 0).unwrap(),
            direct_endpoint: has_endpoint.then(|| DirectEndpoint {
                host: "127.0.0.1".into(),
                port: 1,
            }),
            capabilities,
        };
        for (label, rejected) in [
            (
                "old protocol",
                member(
                    1,
                    -1,
                    SessionCapabilities {
                        version: 2,
                        ..complete
                    },
                    true,
                ),
            ),
            (
                "missing topology",
                member(
                    1,
                    -1,
                    SessionCapabilities {
                        host_topology: false,
                        ..complete
                    },
                    true,
                ),
            ),
            (
                "missing transport",
                member(
                    1,
                    -1,
                    SessionCapabilities {
                        direct_transport: false,
                        ..complete
                    },
                    true,
                ),
            ),
            ("missing endpoint", member(1, -1, complete, false)),
        ] {
            let mut members = [
                rejected,
                member(3, 1, complete, true),
                member(4, 0, complete, true),
                member(2, 0, complete, true),
            ];
            for _ in 0..members.len() {
                for authority in [None, Some(Uuid::from_u128(1)), Some(Uuid::max())] {
                    assert_eq!(
                        elect_eligible_host(authority, &members, Topology::Host, Transport::Direct),
                        Some(Uuid::from_u128(2)),
                        "case={label}, authority={authority:?}, members={members:?}",
                    );
                }
                assert_eq!(
                    elect_eligible_host(
                        Some(Uuid::from_u128(3)),
                        &members,
                        Topology::Host,
                        Transport::Direct
                    ),
                    Some(Uuid::from_u128(3)),
                    "eligible authority: case={label}, members={members:?}",
                );
                members.rotate_left(1);
            }
        }
    }

    /// #1464: the extracted election must select the protocol minimum, including timestamp boundaries.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn host_election_matches_authority_and_independent_sorted_order() {
        let instants = [
            chrono::DateTime::<chrono::Utc>::MIN_UTC,
            chrono::DateTime::from_timestamp(-1, 999_999_999).unwrap(),
            chrono::DateTime::UNIX_EPOCH,
            chrono::DateTime::from_timestamp(0, 1).unwrap(),
            chrono::DateTime::from_timestamp(1_483_228_799, 999_999_999).unwrap(),
            chrono::DateTime::from_timestamp(1_483_228_799, 1_999_999_999).unwrap(),
            chrono::DateTime::from_timestamp(1_483_228_800, 0).unwrap(),
            chrono::DateTime::<chrono::Utc>::MAX_UTC,
        ];
        let identities = [Uuid::nil(), Uuid::from_u128(1 << 120), Uuid::max()];
        let candidates: Vec<_> = instants
            .into_iter()
            .flat_map(|joined_at| {
                identities.into_iter().map(move |player_id| SessionMember {
                    player_id,
                    player_name: String::new(),
                    is_authority: true,
                    joined_at,
                    direct_endpoint: None,
                    capabilities: SessionCapabilities::default(),
                })
            })
            .collect();
        let absent = Uuid::from_u128(123);
        for authority in [None, Some(absent)] {
            assert_eq!(elect_host(authority, &[]), None);
        }
        for first in &candidates {
            for second in &candidates {
                for third in &candidates {
                    let members = [first.clone(), second.clone(), third.clone()];
                    let mut sorted = [&members[0], &members[1], &members[2]];
                    sorted.sort_by_key(|member| (member.joined_at, member.player_id));
                    for authority in [
                        None,
                        Some(first.player_id),
                        Some(third.player_id),
                        Some(absent),
                    ] {
                        let expected = authority
                            .filter(|id| members.iter().any(|member| member.player_id == *id))
                            .or(Some(sorted[0].player_id));
                        assert_eq!(
                            elect_host(authority, &members),
                            expected,
                            "authority={authority:?}, members={members:?}"
                        );
                        assert_eq!(
                            elect_host(authority, members.iter().rev()),
                            expected,
                            "reverse order: authority={authority:?}, members={members:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn host_election_reads_every_uuid_byte_after_timestamp_ties() {
        for position in 0..16 {
            for byte in [0_u8, 1, 127, 128, 254] {
                let mut lower = [0xff; 16];
                let mut higher = [0; 16];
                lower[..position].fill(0x7f);
                higher[..position].fill(0x7f);
                lower[position] = byte;
                higher[position] = byte + 1;
                let first = SessionMember {
                    player_id: Uuid::from_bytes(higher),
                    player_name: String::new(),
                    is_authority: false,
                    joined_at: chrono::DateTime::UNIX_EPOCH,
                    direct_endpoint: None,
                    capabilities: SessionCapabilities::default(),
                };
                let second = SessionMember {
                    player_id: Uuid::from_bytes(lower),
                    ..first.clone()
                };
                let members = [first, second];
                assert_eq!(
                    elect_host(None, &members),
                    Some(Uuid::from_bytes(lower)),
                    "byte={position}, value={byte}"
                );
            }
        }
    }

    #[test]
    fn host_election_preserves_calendar_order_across_year_and_leap_boundaries() {
        for (year, month, day) in [
            (-400, 2, 28),
            (-1, 12, 31),
            (0, 2, 28),
            (1999, 12, 31),
            (2000, 2, 28),
        ] {
            let date = chrono::NaiveDate::from_ymd_opt(year, month, day).unwrap();
            let earlier = date
                .and_hms_nano_opt(23, 59, 59, 999_999_999)
                .unwrap()
                .and_utc();
            let later = date
                .succ_opt()
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc();
            let old = SessionMember {
                player_id: Uuid::max(),
                player_name: String::new(),
                is_authority: false,
                joined_at: earlier,
                direct_endpoint: None,
                capabilities: SessionCapabilities::default(),
            };
            let new = SessionMember {
                player_id: Uuid::nil(),
                joined_at: later,
                ..old.clone()
            };
            assert_eq!(
                elect_host(None, &[new, old]),
                Some(Uuid::max()),
                "calendar boundary={date}"
            );
        }
    }

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
