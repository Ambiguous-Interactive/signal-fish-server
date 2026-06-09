//! Tests for the P3 session-plan selection + emission (`session_policy.rs`).
//!
//! Two layers: pure-logic tests construct [`SessionMember`]s directly and assert
//! the selection ladder, host election, and per-recipient peer/initiate shaping;
//! emission tests build a real server (with a chosen `SessionConfig`), register
//! v3 clients, hand-build a [`FinalizedRoom`], call `emit_session_plan`, and
//! assert on each client's mpsc receiver — including that a relay-resolved room
//! sends nothing (Appendix K).

use crate::config::{
    AuthMaintenanceConfig, CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig,
    SessionConfig, TransportSecurityConfig,
};
use crate::coordination::FinalizedRoom;
use crate::database::DatabaseConfig;
use crate::protocol::{IceServer, PlayerId, PlayerInfo, ServerMessage, Topology, Transport};
use crate::rate_limit::RateLimitConfig;
use crate::server::{EnhancedGameServer, NegotiatedProtocol, ServerConfig};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

use super::session_policy::{
    choose_session_plan, elect_host, is_valid_pair, SessionMember, SessionPlanDecision,
    RELAY_FLOOR, UPGRADE_LADDER,
};

/// A fully-inert `[turn]` block (disabled, *no* STUN urls) for the P3 selection /
/// `plan_for` tests that isolate the operator's static `session.ice_servers`: with
/// it, `build_ice_servers` contributes nothing, so a recipient's ICE list equals
/// `session.ice_servers` and the original P3 expectations hold unchanged.
fn turn_off() -> crate::config::TurnConfig {
    crate::config::TurnConfig {
        enabled: false,
        stun_urls: Vec::new(),
        ..crate::config::TurnConfig::default()
    }
}

/// Build the recipient's ICE list exactly as `emit_session_plan` does — the
/// operator's static `session.ice_servers` followed by the per-recipient
/// TURN-derived entries — but only for a WebRTC plan (Host+Direct / Relay carry an
/// empty list). Mirrors the emit site so the pure-logic tests can assert on the
/// ICE a recipient would actually receive now that `SessionPlanDecision` no longer
/// carries an `ice_servers` field.
fn ice_for(
    decision: &SessionPlanDecision,
    recipient: PlayerId,
    session: &SessionConfig,
    turn: &crate::config::TurnConfig,
    now_unix: i64,
) -> Vec<IceServer> {
    if decision.uses_webrtc_signaling() {
        let mut ice = session.ice_servers.clone();
        ice.extend(crate::security::build_ice_servers(
            turn, recipient, now_unix,
        ));
        ice
    } else {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Pure-logic fixtures.
// ---------------------------------------------------------------------------

/// A fixed, deterministic base instant for test fixtures.
///
/// Using a constant instead of `Utc::now()` keeps the pure-logic tests
/// reproducible: host-election tie-breaks resolve on the fixture data alone and
/// never depend on real-clock skew between two `Utc::now()` calls.
fn base_time() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(1_700_000_000, 0).expect("fixed timestamp is valid")
}

/// A member that supports v3 + the given transports/topologies.
fn member(
    id: PlayerId,
    name: &str,
    version: u16,
    transports: Vec<Transport>,
    topologies: Vec<Topology>,
) -> SessionMember {
    SessionMember {
        player_id: id,
        player_name: name.to_string(),
        is_authority: false,
        joined_at: base_time(),
        version,
        transports,
        topologies,
    }
}

/// A fully-capable v3 member (relay+webrtc transports, relay+host+mesh topologies).
fn v3_full(id: PlayerId, name: &str) -> SessionMember {
    member(
        id,
        name,
        3,
        vec![Transport::Relay, Transport::WebRtc, Transport::Direct],
        vec![Topology::Relay, Topology::Host, Topology::Mesh],
    )
}

fn mesh_config() -> SessionConfig {
    SessionConfig {
        default_topology: Topology::Mesh,
        ice_servers: vec![IceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_string()],
            username: None,
            credential: None,
        }],
        ..SessionConfig::default()
    }
}

fn host_config() -> SessionConfig {
    SessionConfig {
        default_topology: Topology::Host,
        ice_servers: vec![IceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_string()],
            username: None,
            credential: None,
        }],
        ..SessionConfig::default()
    }
}

// ---------------------------------------------------------------------------
// Selection table (Appendix D).
//
// The capability ladder (mesh+webrtc -> host+webrtc -> host+direct -> relay
// floor) is a pure function of {members' capabilities, config gates, per-game
// mapping}. It is expressed once as a data table so every rung — and every
// downgrade trigger — is asserted uniformly; adding a row is the canonical way
// to cover a new selection scenario.
// ---------------------------------------------------------------------------

/// One member's advertised capabilities for a selection-table row.
struct MemberSpec {
    version: u16,
    transports: Vec<Transport>,
    topologies: Vec<Topology>,
}

/// Shorthand for a [`MemberSpec`] row.
fn spec(version: u16, transports: Vec<Transport>, topologies: Vec<Topology>) -> MemberSpec {
    MemberSpec {
        version,
        transports,
        topologies,
    }
}

/// A fully-capable v3 member spec (relay+webrtc+direct, relay+host+mesh).
fn full_spec() -> MemberSpec {
    spec(
        3,
        vec![Transport::Relay, Transport::WebRtc, Transport::Direct],
        vec![Topology::Relay, Topology::Host, Topology::Mesh],
    )
}

/// A host+direct-capable v3 member spec (no WebRTC transport, no mesh topology).
fn host_direct_spec() -> MemberSpec {
    spec(
        3,
        vec![Transport::Relay, Transport::Direct],
        vec![Topology::Relay, Topology::Host],
    )
}

/// Expected outcome of [`choose_session_plan`] for one selection-table row.
struct Expect {
    topology: Topology,
    transport: Transport,
    has_host: bool,
    has_ice: bool,
}

struct SelectionCase {
    name: &'static str,
    game: &'static str,
    members: Vec<MemberSpec>,
    config: SessionConfig,
    expect: Expect,
}

#[test]
fn selection_table_resolves_each_rung_and_downgrade() {
    let stun = vec![IceServer {
        urls: vec!["stun:host".to_string()],
        username: None,
        credential: None,
    }];
    let mapped_cfg = || SessionConfig {
        default_topology: Topology::Relay,
        game_topology_mappings: HashMap::from([("FastFPS".to_string(), Topology::Mesh)]),
        ice_servers: stun.clone(),
        ..SessionConfig::default()
    };

    let cases = vec![
        SelectionCase {
            name: "all-v3 mesh+webrtc selects mesh+webrtc",
            game: "game",
            members: vec![full_spec(), full_spec()],
            config: mesh_config(),
            expect: Expect {
                topology: Topology::Mesh,
                transport: Transport::WebRtc,
                has_host: false,
                has_ice: true,
            },
        },
        SelectionCase {
            name: "default host with webrtc selects host+webrtc",
            game: "game",
            members: vec![full_spec(), full_spec()],
            config: host_config(),
            expect: Expect {
                topology: Topology::Host,
                transport: Transport::WebRtc,
                has_host: true,
                has_ice: true,
            },
        },
        SelectionCase {
            // Members support host+direct but NOT webrtc; the ladder falls
            // through from host+webrtc to host+direct (which carries no ICE).
            name: "host-only-direct members select host+direct",
            game: "game",
            members: vec![host_direct_spec(), host_direct_spec()],
            config: host_config(),
            expect: Expect {
                topology: Topology::Host,
                transport: Transport::Direct,
                has_host: true,
                has_ice: false,
            },
        },
        SelectionCase {
            // One v2 / relay-only member forces the whole room to the floor.
            name: "single relay-only member downgrades to relay",
            game: "game",
            members: vec![
                full_spec(),
                spec(2, vec![Transport::Relay], vec![Topology::Relay]),
            ],
            config: mesh_config(),
            expect: Expect {
                topology: Topology::Relay,
                transport: Transport::Relay,
                has_host: false,
                has_ice: false,
            },
        },
        SelectionCase {
            // v3 + mesh topology but missing the WebRTC transport on one member.
            name: "member without webrtc transport downgrades mesh to relay",
            game: "game",
            members: vec![
                full_spec(),
                spec(
                    3,
                    vec![Transport::Relay],
                    vec![Topology::Relay, Topology::Mesh],
                ),
            ],
            config: mesh_config(),
            expect: Expect {
                topology: Topology::Relay,
                transport: Transport::Relay,
                has_host: false,
                has_ice: false,
            },
        },
        SelectionCase {
            // `desired` is a *ceiling*: a mesh-preferring room with WebRTC disabled
            // walks past mesh+webrtc and host+webrtc to land on host+direct (members
            // are fully capable and direct stays enabled). It must NOT collapse
            // straight to relay (ADR-0001 §1 ladder — the bug Copilot flagged).
            name: "mesh ceiling with webrtc disabled falls back to host+direct",
            game: "game",
            members: vec![full_spec(), full_spec()],
            config: SessionConfig {
                enable_webrtc: false,
                ..mesh_config()
            },
            expect: Expect {
                topology: Topology::Host,
                transport: Transport::Direct,
                has_host: true,
                has_ice: false,
            },
        },
        SelectionCase {
            // `desired` mesh, but one member lacks the mesh *topology* while still
            // supporting host+webrtc: the ladder falls mesh→host+webrtc, not to
            // relay (WebRTC stays enabled).
            name: "mesh ceiling falls back to host+webrtc when a member lacks mesh topology",
            game: "game",
            members: vec![
                full_spec(),
                spec(
                    3,
                    vec![Transport::Relay, Transport::WebRtc],
                    vec![Topology::Relay, Topology::Host],
                ),
            ],
            config: mesh_config(),
            expect: Expect {
                topology: Topology::Host,
                transport: Transport::WebRtc,
                has_host: true,
                has_ice: true,
            },
        },
        SelectionCase {
            // mesh ceiling, WebRTC disabled: host+direct is the only viable rung,
            // but one member lacks the host *topology*, so every rung fails and the
            // room correctly lands on the relay floor (fallback needs capability).
            name: "mesh ceiling with webrtc disabled downgrades to relay when a member lacks host",
            game: "game",
            members: vec![
                full_spec(),
                spec(
                    3,
                    vec![Transport::Relay, Transport::Direct],
                    vec![Topology::Relay, Topology::Mesh],
                ),
            ],
            config: SessionConfig {
                enable_webrtc: false,
                ..mesh_config()
            },
            expect: Expect {
                topology: Topology::Relay,
                transport: Transport::Relay,
                has_host: false,
                has_ice: false,
            },
        },
        SelectionCase {
            // host+direct-capable members, webrtc disabled, direct disabled => relay.
            name: "enable_webrtc=false + enable_direct=false blocks host+direct",
            game: "game",
            members: vec![host_direct_spec(), host_direct_spec()],
            config: SessionConfig {
                enable_webrtc: false,
                enable_direct: false,
                ..host_config()
            },
            expect: Expect {
                topology: Topology::Relay,
                transport: Transport::Relay,
                has_host: false,
                has_ice: false,
            },
        },
        SelectionCase {
            // webrtc enabled but members lack webrtc; direct disabled => relay
            // (the host+direct rung is gated off even though members support direct).
            name: "enable_direct=false alone blocks the host+direct path",
            game: "game",
            members: vec![host_direct_spec(), host_direct_spec()],
            config: SessionConfig {
                enable_webrtc: true,
                enable_direct: false,
                ..host_config()
            },
            expect: Expect {
                topology: Topology::Relay,
                transport: Transport::Relay,
                has_host: false,
                has_ice: false,
            },
        },
        SelectionCase {
            // default_topology = relay, but the per-game mapping requests mesh.
            name: "per-game mapping upgrades the mapped game to mesh",
            game: "FastFPS",
            members: vec![full_spec(), full_spec()],
            config: mapped_cfg(),
            expect: Expect {
                topology: Topology::Mesh,
                transport: Transport::WebRtc,
                has_host: false,
                has_ice: true,
            },
        },
        SelectionCase {
            // An unmapped game uses the relay default.
            name: "per-game mapping leaves an unmapped game on the relay default",
            game: "OtherGame",
            members: vec![full_spec(), full_spec()],
            config: mapped_cfg(),
            expect: Expect {
                topology: Topology::Relay,
                transport: Transport::Relay,
                has_host: false,
                has_ice: false,
            },
        },
        SelectionCase {
            // A per-game mapping is a ceiling too: the mapped game prefers mesh, but
            // with WebRTC disabled it falls to host+direct (not relay), exactly like
            // a default-topology mesh ceiling would.
            name: "per-game mesh mapping with webrtc disabled falls back to host+direct",
            game: "FastFPS",
            members: vec![full_spec(), full_spec()],
            config: SessionConfig {
                enable_webrtc: false,
                ..mapped_cfg()
            },
            expect: Expect {
                topology: Topology::Host,
                transport: Transport::Direct,
                has_host: true,
                has_ice: false,
            },
        },
        SelectionCase {
            name: "empty room resolves to relay",
            game: "game",
            members: Vec::new(),
            config: mesh_config(),
            expect: Expect {
                topology: Topology::Relay,
                transport: Transport::Relay,
                has_host: false,
                has_ice: false,
            },
        },
    ];

    for case in cases {
        let members = case
            .members
            .iter()
            .enumerate()
            .map(|(i, s)| {
                member(
                    PlayerId::new_v4(),
                    &format!("P{i}"),
                    s.version,
                    s.transports.clone(),
                    s.topologies.clone(),
                )
            })
            .collect();
        let members: Vec<SessionMember> = members;
        let recipient = members.first().map(|m| m.player_id);
        let decision = choose_session_plan(case.game, None, members, &case.config);

        assert_eq!(
            decision.topology, case.expect.topology,
            "topology [{}]",
            case.name
        );
        assert_eq!(
            decision.transport, case.expect.transport,
            "transport [{}]",
            case.name
        );
        assert_eq!(
            decision.host.is_some(),
            case.expect.has_host,
            "host presence [{}]",
            case.name
        );
        // ICE now flows through the emit site, not the decision: a recipient's
        // plan carries ICE iff the plan is WebRTC (the test configs all set a
        // static STUN list). TURN is disabled here, so ICE presence == WebRTC.
        if let Some(recipient) = recipient {
            let ice = ice_for(&decision, recipient, &case.config, &turn_off(), 0);
            assert_eq!(
                !ice.is_empty(),
                case.expect.has_ice,
                "ice presence [{}]",
                case.name
            );
        } else {
            // Empty room ⇒ relay ⇒ no recipient and no ICE.
            assert!(!case.expect.has_ice, "ice presence [{}]", case.name);
        }
    }
}

/// Exhaustively asserts the selection invariant: across every `desired` ceiling,
/// both transport gates, and a representative spread of member capabilities,
/// [`choose_session_plan`] only ever yields a *legal* (topology, transport) pair
/// and the cross-field couplings hold — relay topology ⇔ relay transport, a host
/// topology always elects a host, ICE accompanies only WebRTC, and the chosen
/// topology never exceeds the desired ceiling. This is the machine-checked guard
/// for the whole class of "topology/transport drift" bugs.
///
/// It guards *legality and overshoot* (every pair is legal; topology never exceeds
/// the ceiling). The downgrade *ladder* itself — that a mesh ceiling falls to host
/// before relay — is pinned separately by
/// `selection_table_resolves_each_rung_and_downgrade`.
#[test]
fn selection_only_ever_yields_a_legal_pair() {
    let member_sets: Vec<Vec<MemberSpec>> = vec![
        vec![],
        vec![full_spec()],
        vec![full_spec(), full_spec()],
        vec![full_spec(), host_direct_spec()],
        vec![host_direct_spec(), host_direct_spec()],
        vec![
            full_spec(),
            spec(2, vec![Transport::Relay], vec![Topology::Relay]),
        ],
        vec![
            full_spec(),
            spec(
                3,
                vec![Transport::Relay, Transport::WebRtc],
                vec![Topology::Relay, Topology::Host],
            ),
        ],
    ];

    for desired in [Topology::Relay, Topology::Host, Topology::Mesh] {
        for enable_webrtc in [false, true] {
            for enable_direct in [false, true] {
                for set in &member_sets {
                    let config = SessionConfig {
                        default_topology: desired,
                        enable_webrtc,
                        enable_direct,
                        ice_servers: vec![IceServer {
                            urls: vec!["stun:host".to_string()],
                            username: None,
                            credential: None,
                        }],
                        ..SessionConfig::default()
                    };
                    let members: Vec<SessionMember> = set
                        .iter()
                        .enumerate()
                        .map(|(i, s)| {
                            member(
                                PlayerId::new_v4(),
                                &format!("P{i}"),
                                s.version,
                                s.transports.clone(),
                                s.topologies.clone(),
                            )
                        })
                        .collect();
                    let recipient = members.first().map(|m| m.player_id);
                    let decision = choose_session_plan("game", None, members, &config);
                    let label = format!(
                        "desired={desired:?} webrtc={enable_webrtc} direct={enable_direct} \
                         members={}",
                        set.len()
                    );

                    assert!(
                        is_valid_pair(decision.topology, decision.transport),
                        "illegal pair {:?}/{:?} [{label}]",
                        decision.topology,
                        decision.transport,
                    );
                    assert_eq!(
                        decision.topology == Topology::Relay,
                        decision.transport == Transport::Relay,
                        "relay topology and relay transport must coincide [{label}]",
                    );
                    assert_eq!(
                        decision.topology == Topology::Host,
                        decision.host.is_some(),
                        "a host is elected exactly when the topology is host [{label}]",
                    );
                    if let Some(recipient) = recipient {
                        let ice = ice_for(&decision, recipient, &config, &turn_off(), 0);
                        assert!(
                            ice.is_empty() || decision.transport == Transport::WebRtc,
                            "ICE servers must accompany only a WebRTC transport [{label}]",
                        );
                    }
                    // The chosen topology never exceeds the `desired` ceiling.
                    let within_ceiling = match desired {
                        Topology::Relay => decision.topology == Topology::Relay,
                        Topology::Host => {
                            matches!(decision.topology, Topology::Relay | Topology::Host)
                        }
                        Topology::Mesh => true,
                    };
                    assert!(
                        within_ceiling,
                        "chosen topology {:?} exceeds desired ceiling {desired:?} [{label}]",
                        decision.topology,
                    );
                }
            }
        }
    }
}

/// Pins the ladder constants to the ADR-0001 §1 waterfall so the single source of
/// truth cannot silently drift from the documented design, and spot-checks that
/// only the four legal pairs pass [`is_valid_pair`].
#[test]
fn ladder_is_the_documented_adr_waterfall() {
    assert_eq!(
        UPGRADE_LADDER,
        [
            (Topology::Mesh, Transport::WebRtc),
            (Topology::Host, Transport::WebRtc),
            (Topology::Host, Transport::Direct),
        ],
        "the upgrade ladder must match ADR-0001 §1 (mesh+webrtc → host+webrtc → host+direct)",
    );
    assert_eq!(RELAY_FLOOR, (Topology::Relay, Transport::Relay));

    for rung in UPGRADE_LADDER {
        assert!(
            is_valid_pair(rung.0, rung.1),
            "ladder rung {rung:?} must be legal"
        );
    }
    assert!(is_valid_pair(RELAY_FLOOR.0, RELAY_FLOOR.1));

    // Representative illegal pairings must be rejected.
    assert!(!is_valid_pair(Topology::Mesh, Transport::Direct));
    assert!(!is_valid_pair(Topology::Mesh, Transport::Relay));
    assert!(!is_valid_pair(Topology::Host, Transport::Relay));
    assert!(!is_valid_pair(Topology::Relay, Transport::WebRtc));
    assert!(!is_valid_pair(Topology::Relay, Transport::Direct));
}

/// Pins the two emission gates' truth table across all four legal pairs, derived
/// from the single source of truth ([`UPGRADE_LADDER`] plus [`RELAY_FLOOR`]) so a
/// ladder edit reshapes it automatically (mirrors [`is_valid_pair`]).
///
/// Distinct from `selection_only_ever_yields_a_legal_pair`, which reads the
/// `topology` / `transport` fields and so cannot catch an inverted accessor body:
/// this calls `is_relay()` / `uses_webrtc_signaling()` directly. It pins the
/// discriminator the doc drift hinged on — `Host + Direct` is non-relay yet
/// non-WebRTC (it gets a `SessionPlan` but no `NewPeer`).
#[test]
fn emission_gates_track_relay_topology_and_webrtc_transport() {
    let mut non_relay_non_webrtc = Vec::new();

    for (topology, transport) in UPGRADE_LADDER
        .into_iter()
        .chain(std::iter::once(RELAY_FLOOR))
    {
        let decision = SessionPlanDecision {
            topology,
            transport,
            host: None,
            members: Vec::new(),
        };

        assert_eq!(
            decision.is_relay(),
            topology == Topology::Relay,
            "is_relay() must be true iff the topology is Relay ({topology:?}/{transport:?})",
        );
        assert_eq!(
            decision.uses_webrtc_signaling(),
            transport == Transport::WebRtc,
            "uses_webrtc_signaling() must be true iff the transport is WebRtc \
             ({topology:?}/{transport:?})",
        );

        if !decision.is_relay() && !decision.uses_webrtc_signaling() {
            non_relay_non_webrtc.push((topology, transport));
        }
    }

    // The whole point of two separate gates: a non-relay plan does NOT imply
    // WebRTC signaling. `Host + Direct` is the discriminating rung — gating
    // late-join `NewPeer` on `is_relay()` (instead of `uses_webrtc_signaling()`)
    // would wrongly push a LAN session into WebRTC negotiation.
    assert_eq!(
        non_relay_non_webrtc,
        vec![(Topology::Host, Transport::Direct)],
        "exactly one legal pair is non-relay yet non-WebRTC: Host + Direct (a \
         SessionPlan is emitted but no NewPeer/Signal)",
    );
}

// ---------------------------------------------------------------------------
// Host election.
// ---------------------------------------------------------------------------

#[test]
fn host_election_prefers_authority_over_earlier_joiner() {
    let early = PlayerId::new_v4();
    let authority = PlayerId::new_v4();
    let now = base_time();

    let mut early_member = v3_full(early, "Early");
    early_member.joined_at = now - chrono::Duration::seconds(100);
    let mut authority_member = v3_full(authority, "Authority");
    authority_member.joined_at = now; // joined later
    authority_member.is_authority = true;

    let members = vec![early_member, authority_member];
    let decision = choose_session_plan("game", Some(authority), members, &host_config());
    assert_eq!(decision.host, Some(authority));
}

#[test]
fn host_election_uses_earliest_joiner_without_authority() {
    let early = PlayerId::new_v4();
    let late = PlayerId::new_v4();
    let now = base_time();

    let mut early_member = v3_full(early, "Early");
    early_member.joined_at = now - chrono::Duration::seconds(50);
    let mut late_member = v3_full(late, "Late");
    late_member.joined_at = now;

    // Order in the slice should not matter — late listed first.
    let members = vec![late_member, early_member];
    let decision = choose_session_plan("game", None, members, &host_config());
    assert_eq!(decision.host, Some(early));
}

#[test]
fn host_election_breaks_timestamp_ties_by_smaller_uuid() {
    let now = base_time();
    let id_a = PlayerId::new_v4();
    let id_b = PlayerId::new_v4();
    let smaller = id_a.min(id_b);

    let mut a = v3_full(id_a, "A");
    a.joined_at = now;
    let mut b = v3_full(id_b, "B");
    b.joined_at = now; // identical timestamp

    let decision = choose_session_plan("game", None, vec![a, b], &host_config());
    assert_eq!(decision.host, Some(smaller));
}

#[test]
fn host_election_is_deterministic_regardless_of_order() {
    let now = base_time();
    let id_a = PlayerId::new_v4();
    let id_b = PlayerId::new_v4();
    let id_c = PlayerId::new_v4();

    let build = |order: [(PlayerId, &str); 3]| {
        let members: Vec<SessionMember> = order
            .into_iter()
            .map(|(id, name)| {
                let mut m = v3_full(id, name);
                m.joined_at = now; // all tie => smallest UUID wins
                m
            })
            .collect();
        choose_session_plan("game", None, members, &host_config()).host
    };

    let host1 = build([(id_a, "A"), (id_b, "B"), (id_c, "C")]);
    let host2 = build([(id_c, "C"), (id_a, "A"), (id_b, "B")]);
    assert_eq!(host1, host2);
    assert_eq!(host1, Some(id_a.min(id_b).min(id_c)));
}

#[test]
fn elect_host_prefers_explicit_authority_over_earliest_joiner() {
    // The explicit authority must win even though another member joined earlier.
    let authority = PlayerId::new_v4();
    let early = PlayerId::new_v4();
    let now = base_time();

    let mut authority_member = v3_full(authority, "Authority");
    authority_member.joined_at = now; // joined later
    let mut early_member = v3_full(early, "Early");
    early_member.joined_at = now - chrono::Duration::seconds(100);

    // `is_authority` is intentionally left false on every member to prove
    // election keys off the explicit `authority` argument, not the proxy flag.
    let members = vec![authority_member, early_member];
    assert_eq!(elect_host(Some(authority), &members), Some(authority));
}

#[test]
fn elect_host_falls_through_to_earliest_joiner_when_authority_absent() {
    // An authority id that is not among the members is ignored; election falls
    // through to the earliest joiner.
    let absent_authority = PlayerId::new_v4();
    let early = PlayerId::new_v4();
    let late = PlayerId::new_v4();
    let now = base_time();

    let mut early_member = v3_full(early, "Early");
    early_member.joined_at = now - chrono::Duration::seconds(50);
    let mut late_member = v3_full(late, "Late");
    late_member.joined_at = now;

    let members = vec![late_member, early_member];
    assert_eq!(elect_host(Some(absent_authority), &members), Some(early));
    // No authority at all behaves identically.
    assert_eq!(elect_host(None, &members), Some(early));
}

#[test]
fn elect_host_returns_none_for_empty_members() {
    assert_eq!(elect_host(Some(PlayerId::new_v4()), &[]), None);
    assert_eq!(elect_host(None, &[]), None);
}

// ---------------------------------------------------------------------------
// plan_for: mesh.
// ---------------------------------------------------------------------------

#[test]
fn plan_for_mesh_excludes_self_and_has_antisymmetric_initiate() {
    let a = PlayerId::new_v4();
    let b = PlayerId::new_v4();
    let c = PlayerId::new_v4();
    let members = vec![v3_full(a, "A"), v3_full(b, "B"), v3_full(c, "C")];
    let decision = choose_session_plan("game", None, members, &mesh_config());

    let cfg = mesh_config();
    let turn = turn_off();
    for &recipient in &[a, b, c] {
        let ice = ice_for(&decision, recipient, &cfg, &turn, 0);
        let plan = decision.plan_for(recipient, ice);
        assert_eq!(plan.topology, Topology::Mesh);
        assert_eq!(plan.transport, Transport::WebRtc);
        assert_eq!(plan.fallback, Transport::Relay);
        // ICE servers present for webrtc (the static STUN from `mesh_config`).
        assert_eq!(plan.ice_servers.len(), 1);
        // No self in the peer list.
        assert!(plan.peers.iter().all(|p| p.player_id != recipient));
        // Exactly the other two members.
        assert_eq!(plan.peers.len(), 2);
    }

    // Antisymmetry: for each unordered pair, exactly one side initiates.
    let initiates = |from: PlayerId, to: PlayerId| {
        decision
            .plan_for(from, Vec::new())
            .peers
            .iter()
            .find(|p| p.player_id == to)
            .map(|p| p.initiate)
            .expect("peer present")
    };
    for &(x, y) in &[(a, b), (a, c), (b, c)] {
        assert_ne!(
            initiates(x, y),
            initiates(y, x),
            "exactly one side of each pair must initiate"
        );
    }
}

#[test]
fn plan_for_mesh_has_no_ice_servers_when_config_has_none() {
    let a = PlayerId::new_v4();
    let b = PlayerId::new_v4();
    let members = vec![v3_full(a, "A"), v3_full(b, "B")];
    let cfg = SessionConfig {
        default_topology: Topology::Mesh,
        ice_servers: Vec::new(),
        ..SessionConfig::default()
    };
    let decision = choose_session_plan("game", None, members, &cfg);
    // Still a webrtc mesh plan, but ice_servers is empty (config supplied none and
    // TURN is disabled, so `ice_for` produces nothing).
    assert_eq!(decision.transport, Transport::WebRtc);
    let ice = ice_for(&decision, a, &cfg, &turn_off(), 0);
    let plan = decision.plan_for(a, ice);
    assert!(plan.ice_servers.is_empty());
}

// ---------------------------------------------------------------------------
// plan_for: host.
// ---------------------------------------------------------------------------

#[test]
fn plan_for_host_non_host_recipient_targets_only_host() {
    let host_id = PlayerId::new_v4();
    let client_a = PlayerId::new_v4();
    let client_b = PlayerId::new_v4();

    let mut host_member = v3_full(host_id, "Host");
    host_member.is_authority = true;
    let members = vec![
        host_member,
        v3_full(client_a, "ClientA"),
        v3_full(client_b, "ClientB"),
    ];
    let decision = choose_session_plan("game", Some(host_id), members, &host_config());
    assert_eq!(decision.host, Some(host_id));

    let plan = decision.plan_for(client_a, Vec::new());
    assert_eq!(plan.topology, Topology::Host);
    assert_eq!(plan.host, Some(host_id));
    assert_eq!(plan.peers.len(), 1);
    let peer = &plan.peers[0];
    assert_eq!(peer.player_id, host_id);
    assert!(peer.initiate, "clients initiate to the host");
    assert!(peer.is_authority, "the host peer is marked authority");
}

#[test]
fn plan_for_host_host_recipient_targets_all_clients() {
    let host_id = PlayerId::new_v4();
    let client_a = PlayerId::new_v4();
    let client_b = PlayerId::new_v4();

    let mut host_member = v3_full(host_id, "Host");
    host_member.is_authority = true;
    let members = vec![
        host_member,
        v3_full(client_a, "ClientA"),
        v3_full(client_b, "ClientB"),
    ];
    let decision = choose_session_plan("game", Some(host_id), members, &host_config());

    let plan = decision.plan_for(host_id, Vec::new());
    assert_eq!(plan.peers.len(), 2, "host sees every client");
    assert!(
        plan.peers.iter().all(|p| !p.initiate),
        "host initiates to none"
    );
    assert!(
        plan.peers.iter().all(|p| !p.is_authority),
        "client peers are not marked authority from the host's view"
    );
    // The host never lists itself.
    assert!(plan.peers.iter().all(|p| p.player_id != host_id));
    let listed: std::collections::HashSet<PlayerId> =
        plan.peers.iter().map(|p| p.player_id).collect();
    assert!(listed.contains(&client_a));
    assert!(listed.contains(&client_b));
}

// ---------------------------------------------------------------------------
// ice_servers gating.
// ---------------------------------------------------------------------------

#[test]
fn ice_servers_empty_when_transport_not_webrtc() {
    // Host + Direct path: ICE servers must be empty even though config has them.
    let members = vec![
        member(
            PlayerId::new_v4(),
            "A",
            3,
            vec![Transport::Relay, Transport::Direct],
            vec![Topology::Relay, Topology::Host],
        ),
        member(
            PlayerId::new_v4(),
            "B",
            3,
            vec![Transport::Relay, Transport::Direct],
            vec![Topology::Relay, Topology::Host],
        ),
    ];
    let cfg = host_config();
    let decision = choose_session_plan("game", None, members, &cfg);
    assert_eq!(decision.transport, Transport::Direct);
    // A non-WebRTC plan carries no ICE even though the config supplies a STUN list.
    let recipient = members_first_id(&decision);
    let ice = ice_for(
        &decision,
        recipient,
        &cfg,
        &crate::config::TurnConfig::default(),
        0,
    );
    assert!(ice.is_empty());
    let plan = decision.plan_for(recipient, ice);
    assert!(plan.ice_servers.is_empty());
}

/// Helper: pick any member id from a decision (for plan_for in a small test).
fn members_first_id(decision: &super::session_policy::SessionPlanDecision) -> PlayerId {
    decision
        .members
        .first()
        .expect("at least one member")
        .player_id
}

// ---------------------------------------------------------------------------
// Emission tests (server + mpsc harness).
// ---------------------------------------------------------------------------

static PORT: AtomicU16 = AtomicU16::new(53000);

fn next_addr() -> SocketAddr {
    let port = PORT.fetch_add(1, Ordering::Relaxed);
    format!("127.0.0.1:{port}").parse().expect("valid addr")
}

/// Build a server with the given session config and a fully-inert TURN config
/// ([`turn_off`]: disabled, no STUN), so the P3 emission tests continue to observe
/// ICE coming *only* from `session.ice_servers`. The P4 ICE-emission tests use
/// [`create_server_with_session_and_turn`] to supply an active `[turn]` block.
async fn create_server_with_session(session: SessionConfig) -> Arc<EnhancedGameServer> {
    create_server_with_session_and_turn(session, turn_off()).await
}

/// Build a server with the given session **and** TURN config, so the P4 ICE-
/// emission tests can exercise minted TURN credentials end to end.
async fn create_server_with_session_and_turn(
    session: SessionConfig,
    turn: crate::config::TurnConfig,
) -> Arc<EnhancedGameServer> {
    let config = ServerConfig {
        rate_limit_config: RateLimitConfig::default(),
        ..ServerConfig::default()
    };
    EnhancedGameServer::new(
        config,
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        session,
        turn,
        DatabaseConfig::InMemory,
        MetricsConfig::default(),
        AuthMaintenanceConfig::default(),
        CoordinationConfig::default(),
        TransportSecurityConfig::default(),
        Vec::new(),
    )
    .await
    .expect("failed to construct test server")
}

async fn register_client(
    server: &EnhancedGameServer,
) -> (PlayerId, mpsc::Receiver<Arc<ServerMessage>>) {
    let (sender, receiver) = mpsc::channel(16);
    let player_id = server
        .connection_manager
        .register_client(sender, next_addr(), server.instance_id)
        .await
        .expect("client registration succeeds");
    (player_id, receiver)
}

fn v3_webrtc() -> NegotiatedProtocol {
    NegotiatedProtocol {
        version: 3,
        transports: vec![Transport::Relay, Transport::WebRtc],
        topologies: vec![Topology::Relay, Topology::Host, Topology::Mesh],
    }
}

async fn recv(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>) -> Arc<ServerMessage> {
    timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("channel still open")
        .expect("message present")
}

async fn assert_silent(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>) {
    match timeout(Duration::from_millis(100), receiver.recv()).await {
        Err(_) => {}
        Ok(Some(message)) => panic!("expected no message to be delivered, got {message:?}"),
        Ok(None) => panic!("channel closed while checking for silence"),
    }
}

fn player_info(id: PlayerId, name: &str, is_authority: bool) -> PlayerInfo {
    PlayerInfo {
        id,
        name: name.to_string(),
        is_authority,
        is_ready: true,
        connected_at: base_time(),
        connection_info: None,
        region_id: "region-a".to_string(),
    }
}

fn finalized(
    game_name: &str,
    members: Vec<PlayerInfo>,
    authority: Option<PlayerId>,
) -> FinalizedRoom {
    FinalizedRoom {
        game_name: game_name.to_string(),
        authority_player: authority,
        members,
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn emit_all_v3_mesh_room_sends_one_plan_each_with_correct_initiate() {
    let server = create_server_with_session(mesh_config()).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    let finalized = finalized(
        "mesh-game",
        vec![
            player_info(alice, "Alice", false),
            player_info(bob, "Bob", false),
        ],
        None,
    );

    server.emit_session_plan(&room_id, &finalized).await;

    let alice_plan = match recv(&mut alice_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("alice expected SessionPlan, got {other:?}"),
    };
    let bob_plan = match recv(&mut bob_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("bob expected SessionPlan, got {other:?}"),
    };

    for plan in [&alice_plan, &bob_plan] {
        assert_eq!(plan.topology, Topology::Mesh);
        assert_eq!(plan.transport, Transport::WebRtc);
        assert_eq!(plan.fallback, Transport::Relay);
        assert!(plan.host.is_none());
        assert_eq!(plan.peers.len(), 1);
        assert_eq!(plan.ice_servers.len(), 1);
    }

    // Each names the other, and exactly one initiates (glare avoidance).
    assert_eq!(alice_plan.peers[0].player_id, bob);
    assert_eq!(bob_plan.peers[0].player_id, alice);
    assert_ne!(alice_plan.peers[0].initiate, bob_plan.peers[0].initiate);
    assert_eq!(alice_plan.peers[0].initiate, alice < bob);

    // Exactly one plan per recipient.
    assert_silent(&mut alice_rx).await;
    assert_silent(&mut bob_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn emit_relay_resolved_room_sends_no_plan() {
    // One v3 + one default v2 (relay-only) member => relay floor => no SessionPlan.
    let server = create_server_with_session(mesh_config()).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (legacy, mut legacy_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    // legacy stays on the default v2 / relay-only protocol.

    let room_id = uuid::Uuid::new_v4();
    let finalized = finalized(
        "mesh-game",
        vec![
            player_info(alice, "Alice", false),
            player_info(legacy, "Legacy", false),
        ],
        None,
    );

    server.emit_session_plan(&room_id, &finalized).await;

    // Neither member receives a SessionPlan (the room runs on the relay floor).
    assert_silent(&mut alice_rx).await;
    assert_silent(&mut legacy_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn emit_host_room_pairs_clients_with_host() {
    let server = create_server_with_session(host_config()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (client_a, mut client_a_rx) = register_client(&server).await;
    let (client_b, mut client_b_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_webrtc());
    server.set_client_protocol(&client_a, v3_webrtc());
    server.set_client_protocol(&client_b, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    let finalized = finalized(
        "host-game",
        vec![
            player_info(host, "Host", true),
            player_info(client_a, "ClientA", false),
            player_info(client_b, "ClientB", false),
        ],
        Some(host),
    );

    server.emit_session_plan(&room_id, &finalized).await;

    // Host receives a plan listing both clients, all initiate=false.
    let host_plan = match recv(&mut host_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("host expected SessionPlan, got {other:?}"),
    };
    assert_eq!(host_plan.topology, Topology::Host);
    assert_eq!(host_plan.host, Some(host));
    assert_eq!(host_plan.peers.len(), 2);
    assert!(host_plan.peers.iter().all(|p| !p.initiate));

    // Each client receives a plan with the host only, initiate=true.
    for rx in [&mut client_a_rx, &mut client_b_rx] {
        let plan = match recv(rx).await.as_ref() {
            ServerMessage::SessionPlan(plan) => plan.clone(),
            other => panic!("client expected SessionPlan, got {other:?}"),
        };
        assert_eq!(plan.peers.len(), 1);
        assert_eq!(plan.peers[0].player_id, host);
        assert!(plan.peers[0].initiate);
        assert!(plan.peers[0].is_authority);
    }

    assert_silent(&mut host_rx).await;
    assert_silent(&mut client_a_rx).await;
    assert_silent(&mut client_b_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn emit_default_relay_config_sends_no_plan_even_for_v3_room() {
    // Default SessionConfig keeps the relay floor, so even an all-v3 room gets no
    // SessionPlan — the v2-equivalent behavior.
    let server = create_server_with_session(SessionConfig::default()).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    let finalized = finalized(
        "game",
        vec![
            player_info(alice, "Alice", false),
            player_info(bob, "Bob", false),
        ],
        None,
    );

    server.emit_session_plan(&room_id, &finalized).await;

    assert_silent(&mut alice_rx).await;
    assert_silent(&mut bob_rx).await;
}

// ---------------------------------------------------------------------------
// P4 ICE / TURN emission.
// ---------------------------------------------------------------------------

/// A `mesh` `SessionConfig` with *no* static `ice_servers`, so every ICE entry a
/// recipient receives comes purely from the `[turn]` block (clean assertions).
fn mesh_config_no_static_ice() -> SessionConfig {
    SessionConfig {
        default_topology: Topology::Mesh,
        ice_servers: Vec::new(),
        ..SessionConfig::default()
    }
}

/// An enabled static-secret `[turn]` block with a STUN url and a TURN url.
fn enabled_turn() -> crate::config::TurnConfig {
    crate::config::TurnConfig {
        enabled: true,
        mode: crate::config::TurnMode::StaticSecret,
        static_auth_secret: "super-secret".to_string(),
        urls: vec!["turn:turn.example.com:3478".to_string()],
        stun_urls: vec!["stun:stun.l.google.com:19302".to_string()],
        credential_ttl_secs: 3600,
        managed_provider: None,
        managed_api_token: None,
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn emit_webrtc_room_with_turn_gives_each_recipient_distinct_credentials() {
    // Acceptance (a): with `[turn]` enabled, each recipient's SessionPlan carries
    // the public STUN entry plus a TURN entry whose username embeds *that*
    // recipient's id — distinct, time-limited credentials per player.
    let server =
        create_server_with_session_and_turn(mesh_config_no_static_ice(), enabled_turn()).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    let finalized = finalized(
        "mesh-game",
        vec![
            player_info(alice, "Alice", false),
            player_info(bob, "Bob", false),
        ],
        None,
    );

    server.emit_session_plan(&room_id, &finalized).await;

    let alice_plan = match recv(&mut alice_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("alice expected SessionPlan, got {other:?}"),
    };
    let bob_plan = match recv(&mut bob_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("bob expected SessionPlan, got {other:?}"),
    };

    // Each plan: STUN entry (credential-less) followed by a TURN entry (with creds).
    for plan in [&alice_plan, &bob_plan] {
        assert_eq!(plan.ice_servers.len(), 2, "STUN + TURN");
        assert_eq!(
            plan.ice_servers[0].urls,
            vec!["stun:stun.l.google.com:19302"]
        );
        assert!(plan.ice_servers[0].username.is_none());
        assert_eq!(plan.ice_servers[1].urls, vec!["turn:turn.example.com:3478"]);
        assert!(plan.ice_servers[1].username.is_some());
        assert!(plan.ice_servers[1].credential.is_some());
    }

    let alice_user = alice_plan.ice_servers[1].username.clone().unwrap();
    let bob_user = bob_plan.ice_servers[1].username.clone().unwrap();
    // Each username embeds the recipient's own id.
    assert!(alice_user.ends_with(&alice.to_string()));
    assert!(bob_user.ends_with(&bob.to_string()));
    // Distinct usernames and credentials per recipient...
    assert_ne!(alice_user, bob_user);
    assert_ne!(
        alice_plan.ice_servers[1].credential,
        bob_plan.ice_servers[1].credential
    );
    // ...but a shared expiry (one `now` captured per finalize): the `{expiry}:`
    // prefix is identical.
    let alice_expiry = alice_user.split(':').next().unwrap();
    let bob_expiry = bob_user.split(':').next().unwrap();
    assert_eq!(alice_expiry, bob_expiry, "all members share one expiry");
    // Expiry is in the future (now + ttl), not a wrapped/past value.
    let expiry: i64 = alice_expiry.parse().expect("expiry is an integer");
    assert!(expiry > chrono::Utc::now().timestamp());
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn emit_webrtc_room_with_turn_disabled_carries_only_public_stun() {
    // Acceptance (b): with `[turn]` disabled but `stun_urls` set, each plan carries
    // only the public STUN entry — no credentials.
    let turn = crate::config::TurnConfig {
        enabled: false,
        ..enabled_turn()
    };
    let server = create_server_with_session_and_turn(mesh_config_no_static_ice(), turn).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    let finalized = finalized(
        "mesh-game",
        vec![
            player_info(alice, "Alice", false),
            player_info(bob, "Bob", false),
        ],
        None,
    );

    server.emit_session_plan(&room_id, &finalized).await;

    for rx in [&mut alice_rx, &mut bob_rx] {
        let plan = match recv(rx).await.as_ref() {
            ServerMessage::SessionPlan(plan) => plan.clone(),
            other => panic!("expected SessionPlan, got {other:?}"),
        };
        assert_eq!(plan.ice_servers.len(), 1, "STUN only when TURN disabled");
        assert_eq!(
            plan.ice_servers[0].urls,
            vec!["stun:stun.l.google.com:19302"]
        );
        assert!(plan.ice_servers[0].username.is_none());
        assert!(plan.ice_servers[0].credential.is_none());
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn emit_webrtc_room_prepends_static_ice_then_turn() {
    // The operator's static `session.ice_servers` are preserved verbatim and come
    // first; the TURN-derived STUN + TURN entries follow.
    let server = create_server_with_session_and_turn(mesh_config(), enabled_turn()).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    // Bob is registered (so the mesh has two v3 members and resolves to webrtc) but
    // his plan is not asserted on here.
    let (bob, _bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    let finalized = finalized(
        "mesh-game",
        vec![
            player_info(alice, "Alice", false),
            player_info(bob, "Bob", false),
        ],
        None,
    );

    server.emit_session_plan(&room_id, &finalized).await;

    let alice_plan = match recv(&mut alice_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("alice expected SessionPlan, got {other:?}"),
    };
    // Static STUN (from mesh_config), then TURN STUN, then TURN creds: 3 entries.
    assert_eq!(alice_plan.ice_servers.len(), 3);
    // The operator's static entry is first and untouched.
    assert_eq!(
        alice_plan.ice_servers[0].urls,
        vec!["stun:stun.l.google.com:19302"]
    );
    assert!(alice_plan.ice_servers[0].username.is_none());
    // The last entry is the minted TURN credential.
    assert_eq!(
        alice_plan.ice_servers[2].urls,
        vec!["turn:turn.example.com:3478"]
    );
    assert!(alice_plan.ice_servers[2].username.is_some());
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn emit_host_direct_room_carries_empty_ice_even_with_turn_enabled() {
    // Host+Direct is non-WebRTC: it must carry an empty ICE list regardless of the
    // `[turn]` block (ICE is only meaningful for WebRTC).
    let cfg = SessionConfig {
        default_topology: Topology::Host,
        enable_webrtc: false,
        enable_direct: true,
        ice_servers: Vec::new(),
        ..SessionConfig::default()
    };
    let server = create_server_with_session_and_turn(cfg, enabled_turn()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (client, mut client_rx) = register_client(&server).await;
    // Direct-capable (no WebRTC) v3 members so the room resolves to Host+Direct.
    let direct = NegotiatedProtocol {
        version: 3,
        transports: vec![Transport::Relay, Transport::Direct],
        topologies: vec![Topology::Relay, Topology::Host],
    };
    server.set_client_protocol(&host, direct.clone());
    server.set_client_protocol(&client, direct);

    let room_id = uuid::Uuid::new_v4();
    let finalized = finalized(
        "host-game",
        vec![
            player_info(host, "Host", true),
            player_info(client, "Client", false),
        ],
        Some(host),
    );

    server.emit_session_plan(&room_id, &finalized).await;

    for rx in [&mut host_rx, &mut client_rx] {
        let plan = match recv(rx).await.as_ref() {
            ServerMessage::SessionPlan(plan) => plan.clone(),
            other => panic!("expected SessionPlan, got {other:?}"),
        };
        assert_eq!(plan.transport, Transport::Direct);
        assert!(
            plan.ice_servers.is_empty(),
            "a non-WebRTC plan carries no ICE even with TURN enabled"
        );
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn emit_webrtc_room_managed_turn_is_stun_only() {
    // Managed mode is a P4 stub: each plan carries only the public STUN entry, no
    // minted TURN credentials.
    let turn = crate::config::TurnConfig {
        enabled: true,
        mode: crate::config::TurnMode::Managed,
        managed_provider: Some("cloudflare".to_string()),
        managed_api_token: Some("token".to_string()),
        ..enabled_turn()
    };
    let server = create_server_with_session_and_turn(mesh_config_no_static_ice(), turn).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    let finalized = finalized(
        "mesh-game",
        vec![
            player_info(alice, "Alice", false),
            player_info(bob, "Bob", false),
        ],
        None,
    );

    server.emit_session_plan(&room_id, &finalized).await;

    for rx in [&mut alice_rx, &mut bob_rx] {
        let plan = match recv(rx).await.as_ref() {
            ServerMessage::SessionPlan(plan) => plan.clone(),
            other => panic!("expected SessionPlan, got {other:?}"),
        };
        assert_eq!(plan.ice_servers.len(), 1, "managed mode is STUN-only in P4");
        assert!(plan.ice_servers[0].username.is_none());
        assert!(plan.ice_servers[0].credential.is_none());
    }
}

// ---------------------------------------------------------------------------
// P5 metric increments at finalize (emit_session_plan).
// ---------------------------------------------------------------------------

/// Snapshot of the P5 selection counters for compact before/after assertions.
#[derive(Debug, PartialEq, Eq)]
struct SelectionCounters {
    session_plans_emitted: u64,
    topology_mesh: u64,
    topology_host: u64,
    topology_relay: u64,
    transport_webrtc: u64,
    transport_direct: u64,
    transport_relay: u64,
    turn_credentials_issued: u64,
}

fn selection_counters(server: &EnhancedGameServer) -> SelectionCounters {
    let m = &server.metrics;
    SelectionCounters {
        session_plans_emitted: m.session_plans_emitted.load(Ordering::Relaxed),
        topology_mesh: m.topology_mesh_selected.load(Ordering::Relaxed),
        topology_host: m.topology_host_selected.load(Ordering::Relaxed),
        topology_relay: m.topology_relay_selected.load(Ordering::Relaxed),
        transport_webrtc: m.transport_webrtc_selected.load(Ordering::Relaxed),
        transport_direct: m.transport_direct_selected.load(Ordering::Relaxed),
        transport_relay: m.transport_relay_selected.load(Ordering::Relaxed),
        turn_credentials_issued: m.turn_credentials_issued.load(Ordering::Relaxed),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn emit_mesh_webrtc_finalize_increments_topology_transport_and_session_plans() {
    let server = create_server_with_session(mesh_config()).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    let finalized = finalized(
        "mesh-game",
        vec![
            player_info(alice, "Alice", false),
            player_info(bob, "Bob", false),
        ],
        None,
    );

    let before = selection_counters(&server);
    server.emit_session_plan(&room_id, &finalized).await;
    // Drain the two plans so the receivers stay alive for the duration.
    let _ = recv(&mut alice_rx).await;
    let _ = recv(&mut bob_rx).await;
    let after = selection_counters(&server);

    assert_eq!(after.topology_mesh, before.topology_mesh + 1);
    assert_eq!(after.transport_webrtc, before.transport_webrtc + 1);
    assert_eq!(
        after.session_plans_emitted,
        before.session_plans_emitted + 1,
        "exactly one non-relay SessionPlan finalize event"
    );
    // No TURN block configured (mesh_config + turn_off) => no credentials minted.
    assert_eq!(
        after.turn_credentials_issued, before.turn_credentials_issued,
        "no TURN credentials when the [turn] block is inert"
    );
    // Untouched counters stay put.
    assert_eq!(after.topology_host, before.topology_host);
    assert_eq!(after.topology_relay, before.topology_relay);
    assert_eq!(after.transport_relay, before.transport_relay);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn emit_mesh_webrtc_with_turn_counts_one_credential_per_recipient() {
    // Two webrtc recipients with an enabled static-secret TURN block => one minted
    // TURN entry per recipient => turn_credentials_issued += 2 for this finalize.
    let server =
        create_server_with_session_and_turn(mesh_config_no_static_ice(), enabled_turn()).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    let finalized = finalized(
        "mesh-game",
        vec![
            player_info(alice, "Alice", false),
            player_info(bob, "Bob", false),
        ],
        None,
    );

    let before = selection_counters(&server);
    server.emit_session_plan(&room_id, &finalized).await;
    let _ = recv(&mut alice_rx).await;
    let _ = recv(&mut bob_rx).await;
    let after = selection_counters(&server);

    assert_eq!(
        after.turn_credentials_issued,
        before.turn_credentials_issued + 2,
        "one TURN credential minted per WebRTC recipient (two members)"
    );
    assert_eq!(after.topology_mesh, before.topology_mesh + 1);
    assert_eq!(after.transport_webrtc, before.transport_webrtc + 1);
    assert_eq!(
        after.session_plans_emitted,
        before.session_plans_emitted + 1
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn emit_relay_resolved_finalize_counts_relay_but_not_session_plan() {
    // One v3 + one default v2 (relay-only) member => relay floor.
    let server = create_server_with_session(mesh_config()).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (legacy, mut legacy_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    // legacy stays v2 / relay-only.

    let room_id = uuid::Uuid::new_v4();
    let finalized = finalized(
        "mesh-game",
        vec![
            player_info(alice, "Alice", false),
            player_info(legacy, "Legacy", false),
        ],
        None,
    );

    let before = selection_counters(&server);
    server.emit_session_plan(&room_id, &finalized).await;
    // No plan is sent on the relay floor.
    assert_silent(&mut alice_rx).await;
    assert_silent(&mut legacy_rx).await;
    let after = selection_counters(&server);

    assert_eq!(
        after.topology_relay,
        before.topology_relay + 1,
        "a relay-resolved finalize counts the relay topology"
    );
    assert_eq!(
        after.transport_relay,
        before.transport_relay + 1,
        "a relay-resolved finalize counts the relay transport"
    );
    assert_eq!(
        after.session_plans_emitted, before.session_plans_emitted,
        "a relay-resolved room emits NO SessionPlan => session_plans_emitted unchanged"
    );
    assert_eq!(after.topology_mesh, before.topology_mesh);
    assert_eq!(after.transport_webrtc, before.transport_webrtc);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn late_join_does_not_double_count_selection_metrics() {
    // emit_session_plan counts once; a subsequent late-join (which also calls
    // choose_session_plan) must NOT bump any selection counter.
    let server = create_server_with_session(mesh_config()).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    let members = vec![
        player_info(alice, "Alice", false),
        player_info(bob, "Bob", false),
    ];
    let finalized = finalized("mesh-game", members.clone(), None);

    server.emit_session_plan(&room_id, &finalized).await;
    let _ = recv(&mut alice_rx).await;
    let _ = recv(&mut bob_rx).await;
    let after_finalize = selection_counters(&server);

    // A late join into the already-finalized room recomputes the plan but must not
    // count it again.
    let mut room = crate::protocol::Room::new(
        "mesh-game".to_string(),
        "ROOMAB".to_string(),
        4,
        false,
        "matchbox".to_string(),
    );
    room.lobby_state = crate::protocol::LobbyState::Finalized;
    server.handle_webrtc_late_join(&room, &bob, &members).await;
    // The late join pairs the two mesh members, so each receives exactly one
    // `NewPeer`. Consume (and verify) them so the receivers stay drained — this
    // also proves the late-join path ran (the double-count guard is non-vacuous).
    for rx in [&mut alice_rx, &mut bob_rx] {
        match recv(rx).await.as_ref() {
            ServerMessage::NewPeer { .. } => {}
            other => panic!("expected NewPeer from late-join pairing, got {other:?}"),
        }
    }
    assert_silent(&mut alice_rx).await;
    assert_silent(&mut bob_rx).await;

    let after_late_join = selection_counters(&server);
    assert_eq!(
        after_finalize, after_late_join,
        "late-join must not double-count any selection metric"
    );
}
