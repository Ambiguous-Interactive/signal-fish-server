//! Deterministic protocol decisions shared by production and source-linked proofs.

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
