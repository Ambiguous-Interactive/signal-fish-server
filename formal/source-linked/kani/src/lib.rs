#![cfg(kani)]

#[path = "../../../../src/protocol_core.rs"]
mod protocol_core;

use protocol_core::{negotiate_protocol_version, nonempty_all_support};

#[kani::proof]
fn negotiation_respects_ceilings() {
    let client: Option<u16> = kani::any();
    let minimum: u16 = kani::any();
    let maximum: u16 = kani::any();
    let result = negotiate_protocol_version(client, minimum, maximum);
    assert!(result <= maximum, "server ceiling");
    if let Some(client) = client {
        assert!(result <= client, "client ceiling");
        if client <= maximum {
            assert_eq!(result, client, "preserve client below ceiling");
        } else {
            assert_eq!(result, maximum, "downgrade to server ceiling");
        }
    }
}

#[kani::proof]
fn negotiation_defaults_and_refusal() {
    let minimum: u16 = kani::any();
    let maximum: u16 = kani::any();
    let client: u16 = kani::any();
    kani::assume(minimum <= maximum);
    assert_eq!(
        negotiate_protocol_version(None, minimum, maximum),
        minimum,
        "missing version uses floor"
    );
    if client < minimum {
        assert_eq!(
            negotiate_protocol_version(Some(client), minimum, maximum),
            client,
            "below-floor client remains rejectable"
        );
    }
    kani::cover!(
        minimum == 3
            && maximum == 3
            && client == 2
            && client == protocol_core::SERVER_MIN_PROTOCOL_VERSION,
        "v2 refusal reachable"
    );
    kani::cover!(minimum == 0 && maximum == u16::MAX, "full range reachable");
}

#[kani::proof]
#[kani::unwind(10)]
fn capability_aggregation_through_eight_members() {
    let members: [bool; 8] = kani::any();
    let length: usize = kani::any();
    kani::assume(length <= members.len());
    let result = nonempty_all_support(&members[..length], |supported| *supported);
    assert!(!result || length > 0, "empty room never upgrades");
    let mut all_supported = true;
    for index in 0..length {
        assert!(
            !result || members[index],
            "every member must support upgrade"
        );
        all_supported &= members[index];
    }
    assert!(
        length == 0 || !all_supported || result,
        "nonempty all-supported room upgrades"
    );
    kani::cover!(length == 0, "empty room reachable");
    kani::cover!(length == 8 && result, "full supported room reachable");
    kani::cover!(length == 8 && !result, "full rejected room reachable");
}
