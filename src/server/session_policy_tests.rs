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
    choose_session_plan, elect_host, is_valid_pair, SessionMember, RELAY_FLOOR, UPGRADE_LADDER,
};

// ---------------------------------------------------------------------------
// Pure-logic fixtures.
// ---------------------------------------------------------------------------

/// A fixed, deterministic base instant for test fixtures.
///
/// Using a constant instead of `Utc::now()` keeps the pure-logic tests
/// reproducible (host-election tie-breaks never depend on real-clock skew) and
/// free of wall-clock syscalls, so they run identically under Miri's isolated
/// interpreter, which cannot service `clock_gettime(REALTIME)`.
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
        assert_eq!(
            !decision.ice_servers.is_empty(),
            case.expect.has_ice,
            "ice presence [{}]",
            case.name
        );
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
                    let members = set
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
                    assert!(
                        decision.ice_servers.is_empty() || decision.transport == Transport::WebRtc,
                        "ICE servers must accompany only a WebRTC transport [{label}]",
                    );
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

    for &recipient in &[a, b, c] {
        let plan = decision.plan_for(recipient);
        assert_eq!(plan.topology, Topology::Mesh);
        assert_eq!(plan.transport, Transport::WebRtc);
        assert_eq!(plan.fallback, Transport::Relay);
        // ICE servers present for webrtc.
        assert_eq!(plan.ice_servers.len(), 1);
        // No self in the peer list.
        assert!(plan.peers.iter().all(|p| p.player_id != recipient));
        // Exactly the other two members.
        assert_eq!(plan.peers.len(), 2);
    }

    // Antisymmetry: for each unordered pair, exactly one side initiates.
    let initiates = |from: PlayerId, to: PlayerId| {
        decision
            .plan_for(from)
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
    // Still a webrtc mesh plan, but ice_servers is empty (config supplied none).
    assert_eq!(decision.transport, Transport::WebRtc);
    let plan = decision.plan_for(a);
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

    let plan = decision.plan_for(client_a);
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

    let plan = decision.plan_for(host_id);
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
    let decision = choose_session_plan("game", None, members, &host_config());
    assert_eq!(decision.transport, Transport::Direct);
    assert!(decision.ice_servers.is_empty());
    let plan = decision.plan_for(members_first_id(&decision));
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

/// Build a server with the given session config (mirrors signaling_tests's
/// harness but threads a chosen `SessionConfig` so emission can pick non-relay
/// plans).
async fn create_server_with_session(session: SessionConfig) -> Arc<EnhancedGameServer> {
    let config = ServerConfig {
        rate_limit_config: RateLimitConfig::default(),
        ..ServerConfig::default()
    };
    EnhancedGameServer::new(
        config,
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        session,
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
    assert!(
        timeout(Duration::from_millis(100), receiver.recv())
            .await
            .unwrap_or(None)
            .is_none(),
        "expected no message to be delivered"
    );
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
