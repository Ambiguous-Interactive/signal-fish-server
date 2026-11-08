//! Deterministic protocol decisions shared by production and source-linked proofs.

/// Lowest protocol version this server build still accepts (the v2 floor).
pub const SERVER_MIN_PROTOCOL_VERSION: u16 = 2;

/// Highest protocol version this server build implements. Single source of truth
/// for the `max_protocol_version` default and validation ceiling. v3 is the
/// single unshipped/mutable "current" protocol: it adds the WebRTC signaling
/// surface (`Signal`/`NewPeer`/`SessionPlan`/`TransportStatus`) AND the delivery
/// reliability surface (server-stamped `GameData.seq` + incarnation `epoch` and
/// the opt-in `RelayStats` frame) over the frozen v2 floor. Deployments can clamp
/// back to 2 (pure v2) via `protocol.max_protocol_version`.
pub const SERVER_MAX_PROTOCOL_VERSION: u16 = 3;

/// First refusal reason when validating deployment protocol bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtocolBoundsDecision {
    Valid,
    BelowFloor,
    Reversed,
    AboveCeiling,
}

/// Admit only ordered deployment bounds within this build's implemented versions.
pub(crate) fn validate_protocol_version_bounds(
    minimum: u16,
    maximum: u16,
) -> ProtocolBoundsDecision {
    if minimum < SERVER_MIN_PROTOCOL_VERSION {
        ProtocolBoundsDecision::BelowFloor
    } else if maximum < minimum {
        ProtocolBoundsDecision::Reversed
    } else if maximum > SERVER_MAX_PROTOCOL_VERSION {
        ProtocolBoundsDecision::AboveCeiling
    } else {
        ProtocolBoundsDecision::Valid
    }
}

/// Negotiate without raising an explicit client ceiling.
///
/// A result below `minimum` remains below it for the caller to reject.
pub(crate) fn negotiate_protocol_version(
    client_max: Option<u16>,
    minimum: u16,
    maximum: u16,
) -> u16 {
    let requested = match client_max {
        Some(version) => version,
        None => minimum,
    };
    if requested < maximum {
        requested
    } else {
        maximum
    }
}

/// Version decision for an Authenticate frame and the socket's current handshake state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandshakeVersionDecision {
    Accepted(u16),
    Retry(u16),
    Close(u16),
}

/// Apply the endpoint default, deployment ceiling, and refusal lifecycle policy.
/// A completed socket must close after a below-floor declaration. A pending
/// socket may retry; neither refusal may install the rejected version.
pub(crate) fn decide_handshake_version(
    client_max: Option<u16>,
    endpoint_default: u16,
    minimum: u16,
    maximum: u16,
    handshake_complete: bool,
) -> HandshakeVersionDecision {
    let requested = client_max.or(Some(endpoint_default));
    let version = negotiate_protocol_version(requested, minimum, maximum);
    if version < minimum {
        if handshake_complete {
            HandshakeVersionDecision::Close(version)
        } else {
            HandshakeVersionDecision::Retry(version)
        }
    } else {
        HandshakeVersionDecision::Accepted(version)
    }
}

/// Require at least one member and support from every member.
///
/// The caller supplies the capability predicate and its mapping from
/// negotiated capabilities.
pub(crate) fn nonempty_all_support<T>(members: &[T], supports: impl FnMut(&T) -> bool) -> bool {
    !members.is_empty() && members.iter().all(supports)
}

#[cfg(test)]
mod tests {
    use super::{negotiate_protocol_version, nonempty_all_support};

    /// Exercise supported bounds, each refusal, and overlapping invalid conditions under Miri.
    #[test]
    fn protocol_bounds_decisions_preserve_boundaries_and_refusal_priority() {
        use super::{validate_protocol_version_bounds, ProtocolBoundsDecision};
        for (minimum, maximum, expected) in [
            (0, 0, ProtocolBoundsDecision::BelowFloor),
            (1, u16::MAX, ProtocolBoundsDecision::BelowFloor),
            (2, 1, ProtocolBoundsDecision::Reversed),
            (2, 2, ProtocolBoundsDecision::Valid),
            (2, 3, ProtocolBoundsDecision::Valid),
            (3, 3, ProtocolBoundsDecision::Valid),
            (2, 4, ProtocolBoundsDecision::AboveCeiling),
            (u16::MAX, 4, ProtocolBoundsDecision::Reversed),
            (u16::MAX, u16::MAX, ProtocolBoundsDecision::AboveCeiling),
        ] {
            assert_eq!(
                validate_protocol_version_bounds(minimum, maximum),
                expected,
                "range={minimum}..={maximum}"
            );
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn negotiation_exhausts_supported_deployments_and_client_versions() {
        for (minimum, maximum) in [(2, 2), (2, 3), (3, 3)] {
            assert_eq!(negotiate_protocol_version(None, minimum, maximum), minimum);
            for client in 0..=u16::MAX {
                let actual = negotiate_protocol_version(Some(client), minimum, maximum);
                assert!(actual <= client && actual <= maximum);
                if client <= maximum {
                    assert_eq!(actual, client);
                } else {
                    assert_eq!(actual, maximum);
                }
            }
        }
    }

    #[test]
    fn negotiation_handles_full_u16_server_bounds() {
        for (minimum, maximum, client, expected) in [
            (0, 0, None, 0),
            (0, u16::MAX, Some(u16::MAX), u16::MAX),
            (u16::MAX, u16::MAX, None, u16::MAX),
            (u16::MAX, u16::MAX, Some(0), 0),
            (u16::MAX, 0, None, 0),
        ] {
            assert_eq!(
                negotiate_protocol_version(client, minimum, maximum),
                expected
            );
        }
    }

    #[test]
    fn capability_aggregation_exhausts_boolean_lists_through_eight_members() {
        for length in 0..=8 {
            for mask in 0_u16..(1 << length) {
                let members: Vec<bool> = (0..length).map(|bit| mask & (1 << bit) != 0).collect();
                let expected = length != 0 && mask == (1 << length) - 1;
                assert_eq!(
                    nonempty_all_support(&members, |supported| *supported),
                    expected
                );
            }
        }
    }

    /// Keep handshake boundary cases enabled in the Miri core lane.
    #[test]
    fn handshake_version_boundaries_preserve_refusal_and_client_precedence() {
        use super::{decide_handshake_version, HandshakeVersionDecision};
        for (client, endpoint, minimum, maximum, complete, expected) in [
            (None, 3, 2, 3, false, HandshakeVersionDecision::Accepted(3)),
            (
                Some(2),
                3,
                2,
                3,
                true,
                HandshakeVersionDecision::Accepted(2),
            ),
            (
                Some(u16::MAX),
                2,
                2,
                3,
                false,
                HandshakeVersionDecision::Accepted(3),
            ),
            (Some(2), 3, 3, 3, false, HandshakeVersionDecision::Retry(2)),
            (Some(2), 3, 3, 3, true, HandshakeVersionDecision::Close(2)),
            (None, 0, 2, 3, true, HandshakeVersionDecision::Close(0)),
            (
                None,
                u16::MAX,
                3,
                2,
                false,
                HandshakeVersionDecision::Retry(2),
            ),
            (
                Some(u16::MAX),
                0,
                u16::MAX,
                u16::MAX,
                true,
                HandshakeVersionDecision::Accepted(u16::MAX),
            ),
        ] {
            assert_eq!(
                decide_handshake_version(client, endpoint, minimum, maximum, complete),
                expected,
                "client={client:?} endpoint={endpoint} range={minimum}..={maximum} complete={complete}"
            );
        }
    }

    /// Check both handshake states against the protocol table for every client version.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn handshake_version_exhausts_requests_routes_and_lifecycle_states() {
        use super::{decide_handshake_version, HandshakeVersionDecision};
        for (minimum, maximum) in [(2, 2), (2, 3), (3, 3), (0, u16::MAX), (3, 2)] {
            for endpoint in [0, 2, 3, u16::MAX] {
                for client in std::iter::once(None).chain((0..=u16::MAX).map(Some)) {
                    for complete in [false, true] {
                        let requested = client.unwrap_or(endpoint);
                        let version = std::cmp::min(requested, maximum);
                        let expected = match (version >= minimum, complete) {
                            (true, _) => HandshakeVersionDecision::Accepted(version),
                            (false, false) => HandshakeVersionDecision::Retry(version),
                            (false, true) => HandshakeVersionDecision::Close(version),
                        };
                        assert_eq!(
                            decide_handshake_version(client, endpoint, minimum, maximum, complete),
                            expected,
                            "client={client:?} endpoint={endpoint} range={minimum}..={maximum} complete={complete}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn capability_aggregation_stops_at_first_rejection() {
        let mut visited = Vec::new();
        assert!(!nonempty_all_support(&[4, 3, 2], |member| {
            visited.push(*member);
            *member != 3
        }));
        assert_eq!(visited, [4, 3]);

        let mut calls = 0;
        assert!(!nonempty_all_support::<bool>(&[], |_| {
            calls += 1;
            true
        }));
        assert_eq!(calls, 0);
    }
}
