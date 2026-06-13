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
use crate::protocol::{
    IceServer, LobbyState, PlayerId, PlayerInfo, Room, ServerMessage, Topology, Transport,
};
use crate::rate_limit::RateLimitConfig;
use crate::server::{EnhancedGameServer, NegotiatedProtocol, ServerConfig};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

use super::session_policy::{
    choose_session_plan, desired_topology_for, elect_host, ice_pregather_eligible, is_valid_pair,
    ActiveSessionPlan, SessionMember, SessionPlanDecision, RELAY_FLOOR, UPGRADE_LADDER,
};

const STATIC_STUN_URL: &str = "stun:static.example.com:3478";
const TURN_STUN_URL: &str = "stun:stun.l.google.com:19302";
const TURN_URL: &str = "turn:turn.example.com:3478";
const TURN_CREDENTIAL_TTL_SECS: u64 = 3600;

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

#[test]
fn ice_ordering_fixtures_are_source_distinguishable() {
    assert_ne!(
        STATIC_STUN_URL, TURN_STUN_URL,
        "static and [turn] STUN fixture URLs must stay distinct so ordering assertions catch swaps"
    );
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
            urls: vec![STATIC_STUN_URL.to_string()],
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
            urls: vec![STATIC_STUN_URL.to_string()],
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
// plan_for: capability filter (re-issued / late-join peer lists).
//
// Rehydrated member lists (host failover, self-heal, late join / reconnect)
// can contain members that never negotiated the session's sticky pair —
// `add_player_to_room` gates only on fullness. A WebRTC pair is doomed unless
// BOTH sides negotiated the transport (`handle_signal` rejects either
// direction and the offers burn signal rate-limit budget), so `plan_for`
// filters `peers[]` on both sides with the same predicate host election uses.
// At finalize the filter is a no-op (`all_support` gates selection), which the
// emit_* tests above already pin by asserting full peer lists.
// ---------------------------------------------------------------------------

/// A v3 member that negotiated only the relay floor — the seat-filling joiner
/// shape that can legitimately sit inside a finalized non-relay room.
fn relay_only_member(id: PlayerId, name: &str) -> SessionMember {
    member(id, name, 3, vec![Transport::Relay], vec![Topology::Relay])
}

#[test]
fn plan_for_mesh_filters_peers_to_session_capable_members_on_both_sides() {
    // A mesh+webrtc rehydrated decision over {two capable, one relay-only}:
    // capable recipients must not see the relay-only member listed (no doomed
    // offers toward it), and the relay-only recipient's own plan must carry an
    // EMPTY peer list (it must not be told to offer to anyone) — truthful:
    // it has no P2P peers and the relay floor is its data path.
    let a = PlayerId::new_v4();
    let b = PlayerId::new_v4();
    let relay_only = PlayerId::new_v4();
    let stored = ActiveSessionPlan {
        topology: Topology::Mesh,
        transport: Transport::WebRtc,
        host: None,
    };
    let decision = stored.decision_with(vec![
        v3_full(a, "A"),
        v3_full(b, "B"),
        relay_only_member(relay_only, "RelayOnly"),
    ]);

    for &recipient in &[a, b] {
        let plan = decision.plan_for(recipient, Vec::new());
        assert_eq!(
            plan.peers.len(),
            1,
            "a capable member pairs only with the other capable member"
        );
        assert!(
            plan.peers.iter().all(|peer| peer.player_id != relay_only),
            "the relay-only member must never appear in a capable member's peers"
        );
    }

    let degenerate = decision.plan_for(relay_only, Vec::new());
    assert!(
        degenerate.peers.is_empty(),
        "a recipient that cannot run the session has no P2P peers"
    );
    assert_eq!(degenerate.topology, Topology::Mesh);
    assert_eq!(degenerate.transport, Transport::WebRtc);
    assert_eq!(
        degenerate.fallback,
        Transport::Relay,
        "the relay floor remains its data path"
    );
}

#[test]
fn plan_for_host_filters_peers_to_session_capable_members_on_both_sides() {
    // A host+webrtc rehydrated decision over {host, capable client, relay-only
    // client}: the host's plan lists ONLY the capable client; the capable
    // client still targets the host (initiate=true); the relay-only client's
    // plan has EMPTY peers (it must NOT be told to offer to the host) while
    // `host` stays as elected — informational.
    let host_id = PlayerId::new_v4();
    let client = PlayerId::new_v4();
    let relay_only = PlayerId::new_v4();
    let stored = ActiveSessionPlan {
        topology: Topology::Host,
        transport: Transport::WebRtc,
        host: Some(host_id),
    };
    let decision = stored.decision_with(vec![
        v3_full(host_id, "Host"),
        v3_full(client, "Client"),
        relay_only_member(relay_only, "RelayOnly"),
    ]);

    let host_plan = decision.plan_for(host_id, Vec::new());
    assert_eq!(
        host_plan.peers.len(),
        1,
        "the host answers only session-capable clients"
    );
    assert_eq!(host_plan.peers[0].player_id, client);
    assert!(!host_plan.peers[0].initiate, "the host initiates to none");

    let client_plan = decision.plan_for(client, Vec::new());
    assert_eq!(client_plan.peers.len(), 1);
    assert_eq!(client_plan.peers[0].player_id, host_id);
    assert!(
        client_plan.peers[0].initiate,
        "a capable client still offers to the host"
    );

    let degenerate = decision.plan_for(relay_only, Vec::new());
    assert!(
        degenerate.peers.is_empty(),
        "a relay-only client must not be told to offer to the host"
    );
    assert_eq!(
        degenerate.host,
        Some(host_id),
        "the host field stays as elected (informational)"
    );
}

#[test]
fn host_invalid_flags_absent_and_unpairable_hosts() {
    // `host_invalid` is the re-plan trigger for both membership-touching
    // events. It must fire when the stored host is absent, recorded as None,
    // or — the reconnect-with-downgraded-capabilities race — still a member
    // but no longer capable of the stored session; and must stay quiet for a
    // present, capable host and for non-host topologies.
    let host_id = PlayerId::new_v4();
    let other = PlayerId::new_v4();
    let stored = ActiveSessionPlan {
        topology: Topology::Host,
        transport: Transport::WebRtc,
        host: Some(host_id),
    };

    // Present and fully capable: valid.
    assert!(!stored.host_invalid(&[v3_full(host_id, "Host"), v3_full(other, "Other")]));

    // Absent from the member list: invalid.
    assert!(stored.host_invalid(&[v3_full(other, "Other")]));

    // No host recorded at all: invalid.
    let hostless = ActiveSessionPlan {
        host: None,
        ..stored
    };
    assert!(hostless.host_invalid(&[v3_full(host_id, "Host")]));

    // Present but downgraded to relay-only (cannot run host+webrtc): invalid —
    // a member-but-unpairable host would otherwise wedge forever (departure
    // re-planning never fires for a member that stays seated).
    assert!(stored.host_invalid(&[
        relay_only_member(host_id, "DowngradedHost"),
        v3_full(other, "Other"),
    ]));

    // Present with the transport but not the host topology: invalid too (the
    // predicate is the full sticky pair, not the transport alone).
    assert!(stored.host_invalid(&[
        member(
            host_id,
            "MeshOnlyHost",
            3,
            vec![Transport::Relay, Transport::WebRtc],
            vec![Topology::Relay, Topology::Mesh],
        ),
        v3_full(other, "Other"),
    ]));

    // Non-host topologies have no host parameter to invalidate.
    let mesh = ActiveSessionPlan {
        topology: Topology::Mesh,
        transport: Transport::WebRtc,
        host: None,
    };
    assert!(!mesh.host_invalid(&[v3_full(other, "Other")]));
    assert!(!mesh.host_invalid(&[]));
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

/// A v3 client that negotiated ONLY the relay transport/topology: a valid v3
/// peer (it may receive `SessionPlan`s) whose data path is the floor — it can
/// never run, nor host, a non-relay session.
fn v3_relay_only() -> NegotiatedProtocol {
    NegotiatedProtocol {
        version: 3,
        transports: vec![Transport::Relay],
        topologies: vec![Topology::Relay],
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
        static_auth_secret: "super-secret".to_string(),
        urls: vec![TURN_URL.to_string()],
        stun_urls: vec![TURN_STUN_URL.to_string()],
        credential_ttl_secs: TURN_CREDENTIAL_TTL_SECS,
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
        assert_eq!(plan.ice_servers[0].urls, vec![TURN_STUN_URL]);
        assert!(plan.ice_servers[0].username.is_none());
        assert!(plan.ice_servers[0].credential.is_none());
        assert_eq!(plan.ice_servers[1].urls, vec![TURN_URL]);
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
        assert_eq!(plan.ice_servers[0].urls, vec![TURN_STUN_URL]);
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
    assert_eq!(alice_plan.ice_servers[0].urls, vec![STATIC_STUN_URL]);
    assert!(alice_plan.ice_servers[0].username.is_none());
    assert!(alice_plan.ice_servers[0].credential.is_none());
    assert_eq!(
        alice_plan.ice_servers[1].urls,
        vec![TURN_STUN_URL],
        "the [turn] STUN entry must remain between static ICE and minted TURN"
    );
    assert!(alice_plan.ice_servers[1].username.is_none());
    assert!(alice_plan.ice_servers[1].credential.is_none());
    // The last entry is the minted TURN credential.
    assert_eq!(alice_plan.ice_servers[2].urls, vec![TURN_URL]);
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
    // emit_session_plan counts the topology/transport selection once per
    // finalize; a subsequent late-join (which consults the STORED decision and
    // never re-runs the ladder) must NOT bump any selection counter, nor
    // session_plans_emitted. (Its own session_plans_late_join counter is
    // asserted separately below.)
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
    let late_join_plans_before = server
        .metrics
        .session_plans_late_join
        .load(Ordering::Relaxed);

    // A late join into the already-finalized room reads the stored decision but
    // must not count any selection again.
    let mut room = crate::protocol::Room::new(
        "mesh-game".to_string(),
        "ROOMAB".to_string(),
        4,
        false,
        "matchbox".to_string(),
    );
    // The stored decision is keyed by the room id emit_session_plan saw.
    room.id = room_id;
    room.lobby_state = crate::protocol::LobbyState::Finalized;
    server
        .handle_active_session_late_join(&room, &bob, &members)
        .await;
    // The joiner (bob) receives its tailored SessionPlan; the existing member
    // (alice) receives the NewPeer delta. Consume (and verify) both so the
    // receivers stay drained — this also proves the late-join path ran (the
    // double-count guard is non-vacuous).
    match recv(&mut bob_rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => {
            assert_eq!(plan.topology, Topology::Mesh);
            assert_eq!(plan.transport, Transport::WebRtc);
        }
        other => panic!("joiner expected SessionPlan from late-join, got {other:?}"),
    }
    match recv(&mut alice_rx).await.as_ref() {
        ServerMessage::NewPeer { peer_id, .. } => assert_eq!(*peer_id, bob),
        other => panic!("existing member expected NewPeer from late-join, got {other:?}"),
    }
    assert_silent(&mut alice_rx).await;
    assert_silent(&mut bob_rx).await;

    let after_late_join = selection_counters(&server);
    assert_eq!(
        after_finalize, after_late_join,
        "late-join must not double-count any selection metric"
    );
    assert_eq!(
        server
            .metrics
            .session_plans_late_join
            .load(Ordering::Relaxed),
        late_join_plans_before + 1,
        "the late-join plan is counted on its own dedicated counter"
    );
}

// ---------------------------------------------------------------------------
// Stored ActiveSessionPlan (the sticky per-room decision).
// ---------------------------------------------------------------------------

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn emit_session_plan_stores_active_plan_for_non_relay_decisions() {
    // A mesh finalize stores {Mesh, WebRtc, host: None}; a host finalize stores
    // {Host, WebRtc, host: Some(elected)} — the source of truth for late-join
    // and departure re-planning.
    let server = create_server_with_session(mesh_config()).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (bob, mut bob_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    server.set_client_protocol(&bob, v3_webrtc());

    let room_id = uuid::Uuid::new_v4();
    let finalized_room = finalized(
        "mesh-game",
        vec![
            player_info(alice, "Alice", false),
            player_info(bob, "Bob", false),
        ],
        None,
    );
    assert!(
        server.active_session_plan(&room_id).is_none(),
        "no decision is stored before finalize"
    );
    server.emit_session_plan(&room_id, &finalized_room).await;
    let _ = recv(&mut alice_rx).await;
    let _ = recv(&mut bob_rx).await;
    assert_eq!(
        server.active_session_plan(&room_id),
        Some(ActiveSessionPlan {
            topology: Topology::Mesh,
            transport: Transport::WebRtc,
            host: None,
        })
    );

    // Host-topology server: the stored entry carries the elected host.
    let host_server = create_server_with_session(host_config()).await;
    let (host, mut host_rx) = register_client(&host_server).await;
    let (client, mut client_rx) = register_client(&host_server).await;
    host_server.set_client_protocol(&host, v3_webrtc());
    host_server.set_client_protocol(&client, v3_webrtc());

    let host_room_id = uuid::Uuid::new_v4();
    let finalized_host_room = finalized(
        "host-game",
        vec![
            player_info(host, "Host", true),
            player_info(client, "Client", false),
        ],
        Some(host),
    );
    host_server
        .emit_session_plan(&host_room_id, &finalized_host_room)
        .await;
    let _ = recv(&mut host_rx).await;
    let _ = recv(&mut client_rx).await;
    assert_eq!(
        host_server.active_session_plan(&host_room_id),
        Some(ActiveSessionPlan {
            topology: Topology::Host,
            transport: Transport::WebRtc,
            host: Some(host),
        })
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn emit_session_plan_relay_resolution_removes_stale_stored_entry() {
    // A relay-resolved finalize stores nothing AND removes any stale entry (a
    // room can re-finalize after future definalization flows).
    let server = create_server_with_session(mesh_config()).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (legacy, mut legacy_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    // legacy stays on the default v2 / relay-only protocol.

    let room_id = uuid::Uuid::new_v4();
    // Simulate a stale decision from an earlier (richer) finalize.
    server.active_session_plans.insert(
        room_id,
        ActiveSessionPlan {
            topology: Topology::Mesh,
            transport: Transport::WebRtc,
            host: None,
        },
    );

    let finalized_room = finalized(
        "mesh-game",
        vec![
            player_info(alice, "Alice", false),
            player_info(legacy, "Legacy", false),
        ],
        None,
    );
    server.emit_session_plan(&room_id, &finalized_room).await;

    assert_silent(&mut alice_rx).await;
    assert_silent(&mut legacy_rx).await;
    assert!(
        server.active_session_plan(&room_id).is_none(),
        "a relay-resolved finalize must remove the stale stored decision"
    );
}

// ---------------------------------------------------------------------------
// Departure re-planning (host failover) — handle_session_member_departure.
// ---------------------------------------------------------------------------

/// Build a REAL finalized DB room owned by `owner` with `others` as additional
/// members (room is exactly full), so `handle_session_member_departure` —
/// which re-reads membership and `lobby_state` from storage — sees a live
/// finalized session. `supports_authority` is true, so `owner` is the room's
/// authority (and thus the finalize-time host election winner).
async fn finalized_db_room_with(
    server: &EnhancedGameServer,
    owner: PlayerId,
    others: &[(PlayerId, &str)],
) -> crate::protocol::RoomId {
    let max_players = u8::try_from(others.len() + 1).expect("test rooms are small");
    let room = server
        .database
        .create_room(
            "host-game".to_string(),
            None,
            max_players,
            true,
            owner,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("create room");
    for (offset, (id, name)) in others.iter().enumerate() {
        let mut info = player_info(*id, name, false);
        // Members must start NOT ready: the toggle below flips each one to
        // ready (the shared `player_info` fixture defaults to ready).
        info.is_ready = false;
        // Distinct, increasing join times (the owner's `connected_at` is `now`,
        // far after `base_time()`), so post-departure re-election
        // deterministically picks the first `other`.
        info.connected_at = base_time() + chrono::Duration::seconds(offset as i64);
        server
            .database
            .add_player_to_room(&room.id, info)
            .await
            .expect("add member");
    }

    server
        .database
        .transition_room_to_lobby(&room.id)
        .await
        .expect("transition to lobby");
    let mut all_members = vec![owner];
    all_members.extend(others.iter().map(|(id, _)| *id));
    for id in &all_members {
        server
            .database
            .toggle_player_ready(&room.id, id)
            .await
            .expect("toggle ready");
    }
    server
        .database
        .finalize_room_game(&room.id)
        .await
        .expect("finalize room");
    let stored = server
        .database
        .get_room_by_id(&room.id)
        .await
        .expect("room lookup")
        .expect("room exists");
    assert_eq!(stored.lobby_state, crate::protocol::LobbyState::Finalized);
    room.id
}

/// Expect the next message on `rx` to be a `SessionPlan` and return it.
async fn expect_plan(
    rx: &mut mpsc::Receiver<Arc<ServerMessage>>,
    who: &str,
) -> Box<crate::protocol::SessionPlanPayload> {
    match recv(rx).await.as_ref() {
        ServerMessage::SessionPlan(plan) => plan.clone(),
        other => panic!("{who} expected SessionPlan, got {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn host_departure_reemits_fresh_plans_with_reelected_host() {
    // Host failover: the stored host departs a Finalized host+webrtc room. The
    // remaining members each receive exactly ONE fresh SessionPlan with the
    // SAME (sticky) topology/transport, the re-elected host (earliest joiner of
    // the remaining — the DB cleared the departed authority), and correct
    // per-recipient peers/initiate. The stored entry is updated and the re-plan
    // event is counted once.
    let server = create_server_with_session(host_config()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (client_a, mut client_a_rx) = register_client(&server).await;
    let (client_b, mut client_b_rx) = register_client(&server).await;
    for id in [&host, &client_a, &client_b] {
        server.set_client_protocol(id, v3_webrtc());
    }

    let room_id = finalized_db_room_with(
        &server,
        host,
        &[(client_a, "ClientA"), (client_b, "ClientB")],
    )
    .await;

    // Finalize-time emission stores {Host, WebRtc, host}.
    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    let finalized_room = finalized("host-game", members, Some(host));
    server.emit_session_plan(&room_id, &finalized_room).await;
    for (rx, who) in [
        (&mut host_rx, "host"),
        (&mut client_a_rx, "client_a"),
        (&mut client_b_rx, "client_b"),
    ] {
        let _ = expect_plan(rx, who).await;
    }
    assert_eq!(
        server.active_session_plan(&room_id).map(|plan| plan.host),
        Some(Some(host))
    );

    // The host departs: removed from storage (which clears its authority),
    // then the departure hook runs.
    server
        .database
        .remove_player_from_room(&room_id, &host)
        .await
        .expect("remove host");
    let replans_before = server
        .metrics
        .session_replans_emitted
        .load(Ordering::Relaxed);
    let plans_before = server.metrics.session_plans_emitted.load(Ordering::Relaxed);
    server
        .handle_session_member_departure(&room_id, &host)
        .await;

    // client_a joined before client_b (explicit connected_at offsets), so it is
    // the re-elected host.
    let new_host = client_a;
    let plan_a = expect_plan(&mut client_a_rx, "client_a").await;
    let plan_b = expect_plan(&mut client_b_rx, "client_b").await;
    for plan in [&plan_a, &plan_b] {
        assert_eq!(plan.topology, Topology::Host, "topology is sticky");
        assert_eq!(plan.transport, Transport::WebRtc, "transport is sticky");
        assert_eq!(plan.host, Some(new_host), "both plans name the new host");
        assert_eq!(plan.fallback, Transport::Relay);
    }
    // The new host answers its single remaining client.
    assert_eq!(plan_a.peers.len(), 1);
    assert_eq!(plan_a.peers[0].player_id, client_b);
    assert!(!plan_a.peers[0].initiate, "the host initiates to none");
    // The remaining client offers to the new host.
    assert_eq!(plan_b.peers.len(), 1);
    assert_eq!(plan_b.peers[0].player_id, new_host);
    assert!(plan_b.peers[0].initiate, "clients offer to the host");
    assert!(plan_b.peers[0].is_authority);

    // Exactly one plan each; the departed host got nothing.
    assert_silent(&mut client_a_rx).await;
    assert_silent(&mut client_b_rx).await;
    assert_silent(&mut host_rx).await;

    // Stored entry rewritten with the new host; metrics: one re-plan event,
    // finalize counter untouched.
    assert_eq!(
        server.active_session_plan(&room_id),
        Some(ActiveSessionPlan {
            topology: Topology::Host,
            transport: Transport::WebRtc,
            host: Some(new_host),
        })
    );
    assert_eq!(
        server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        replans_before + 1,
        "one departure-triggered re-plan event"
    );
    assert_eq!(
        server.metrics.session_plans_emitted.load(Ordering::Relaxed),
        plans_before,
        "re-plans never count as finalize emissions"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn host_departure_replan_mints_fresh_turn_credentials_per_recipient() {
    // With an enabled [turn] block, the failover re-plan mints fresh
    // per-recipient credentials (one per remaining member, distinct usernames
    // embedding each recipient's id, one shared expiry) and counts them on the
    // shared turn_credentials_issued counter.
    let server = create_server_with_session_and_turn(host_config(), enabled_turn()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (client_a, mut client_a_rx) = register_client(&server).await;
    let (client_b, mut client_b_rx) = register_client(&server).await;
    for id in [&host, &client_a, &client_b] {
        server.set_client_protocol(id, v3_webrtc());
    }

    let room_id = finalized_db_room_with(
        &server,
        host,
        &[(client_a, "ClientA"), (client_b, "ClientB")],
    )
    .await;
    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    server
        .emit_session_plan(&room_id, &finalized("host-game", members, Some(host)))
        .await;
    for (rx, who) in [
        (&mut host_rx, "host"),
        (&mut client_a_rx, "client_a"),
        (&mut client_b_rx, "client_b"),
    ] {
        let _ = expect_plan(rx, who).await;
    }

    server
        .database
        .remove_player_from_room(&room_id, &host)
        .await
        .expect("remove host");
    let creds_before = server
        .metrics
        .turn_credentials_issued
        .load(Ordering::Relaxed);
    server
        .handle_session_member_departure(&room_id, &host)
        .await;

    let plan_a = expect_plan(&mut client_a_rx, "client_a").await;
    let plan_b = expect_plan(&mut client_b_rx, "client_b").await;
    let turn_user = |plan: &crate::protocol::SessionPlanPayload, who: &str| {
        plan.ice_servers
            .iter()
            .find_map(|server| server.username.clone())
            .unwrap_or_else(|| panic!("{who}'s re-plan must carry a minted TURN credential"))
    };
    let user_a = turn_user(&plan_a, "client_a");
    let user_b = turn_user(&plan_b, "client_b");
    assert!(user_a.ends_with(&client_a.to_string()));
    assert!(user_b.ends_with(&client_b.to_string()));
    assert_ne!(user_a, user_b, "credentials are per recipient");
    // One shared expiry per re-plan event (single captured `now`).
    let expiry_a = user_a.split(':').next().expect("expiry prefix");
    let expiry_b = user_b.split(':').next().expect("expiry prefix");
    assert_eq!(expiry_a, expiry_b);
    let expiry: i64 = expiry_a.parse().expect("expiry is an integer");
    assert!(
        expiry > chrono::Utc::now().timestamp(),
        "freshly minted credentials expire in the future"
    );

    assert_eq!(
        server
            .metrics
            .turn_credentials_issued
            .load(Ordering::Relaxed),
        creds_before + 2,
        "one credential minted per remaining recipient"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn host_departure_replan_skips_v2_members() {
    // Appendix K on the re-plan path: a v2 member (possible mid-session via a
    // seat-filling late join) never receives a SessionPlan; v3 members still
    // get theirs.
    let server = create_server_with_session(host_config()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (v3_client, mut v3_client_rx) = register_client(&server).await;
    let (legacy, mut legacy_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_webrtc());
    server.set_client_protocol(&v3_client, v3_webrtc());
    // legacy stays on the default v2 / relay-only protocol.

    let room_id = finalized_db_room_with(
        &server,
        host,
        &[(v3_client, "V3Client"), (legacy, "Legacy")],
    )
    .await;
    // Hand-store the running decision (a real finalize with a v2 member would
    // have resolved to relay; the v2 member is modeled as a post-finalize
    // seat-filler).
    server.active_session_plans.insert(
        room_id,
        ActiveSessionPlan {
            topology: Topology::Host,
            transport: Transport::WebRtc,
            host: Some(host),
        },
    );

    server
        .database
        .remove_player_from_room(&room_id, &host)
        .await
        .expect("remove host");
    server
        .handle_session_member_departure(&room_id, &host)
        .await;

    // v3_client joined first among the remaining, so it is the new host; it
    // receives a plan. The v2 member receives NOTHING — and, since it cannot
    // run the session, it must not be listed in the new host's peers either.
    let plan = expect_plan(&mut v3_client_rx, "v3_client").await;
    assert_eq!(plan.host, Some(v3_client));
    assert!(
        plan.peers.is_empty(),
        "the v2 member cannot signal and must not appear in the new host's plan"
    );
    assert_silent(&mut legacy_rx).await;
    assert_silent(&mut v3_client_rx).await;
    assert_silent(&mut host_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn host_departure_election_skips_relay_only_authority() {
    // Capability-aware election: the departed host leaves behind a v3
    // RELAY-ONLY seat-filler that HOLDS AUTHORITY (plain v2 `RequestAuthority`
    // has no version gate) and joined earlier than the other member — so BOTH
    // preference rules (authority, earliest joiner) would pick it if election
    // ignored capabilities. It cannot run the stored host+webrtc session, so
    // the v3+webrtc member must be elected instead. Both remaining members are
    // v3, so both receive the fresh plan, but peer lists are capability-
    // filtered on BOTH sides: the relay-only member's plan has EMPTY peers (it
    // must not be told to offer to the new host; its data path stays the relay
    // fallback) and the new host's plan does not list it; one re-plan event is
    // counted.
    let server = create_server_with_session(host_config()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (relay_authority, mut relay_authority_rx) = register_client(&server).await;
    let (webrtc_member, mut webrtc_member_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_webrtc());
    server.set_client_protocol(&relay_authority, v3_relay_only());
    server.set_client_protocol(&webrtc_member, v3_webrtc());

    // relay_authority is listed first => earliest `connected_at` among the
    // remaining members (finalized_db_room_with assigns increasing offsets).
    let room_id = finalized_db_room_with(
        &server,
        host,
        &[
            (relay_authority, "RelayAuthority"),
            (webrtc_member, "WebrtcMember"),
        ],
    )
    .await;
    // The running session is host+webrtc (the relay-only member is modeled as
    // a post-finalize seat-filler; a real finalize with it present would have
    // resolved to the relay floor and stored nothing).
    server.active_session_plans.insert(
        room_id,
        ActiveSessionPlan {
            topology: Topology::Host,
            transport: Transport::WebRtc,
            host: Some(host),
        },
    );

    // The host departs (clearing its authority); the relay-only member then
    // claims authority before the departure hook runs.
    server
        .database
        .remove_player_from_room(&room_id, &host)
        .await
        .expect("remove host");
    assert!(server
        .database
        .update_room_authority(&room_id, Some(relay_authority))
        .await
        .expect("authority update succeeds"));
    let replans_before = server
        .metrics
        .session_replans_emitted
        .load(Ordering::Relaxed);
    server
        .handle_session_member_departure(&room_id, &host)
        .await;

    // The v3+webrtc member is elected despite the authority/earliest-joiner
    // preference for the relay-only member.
    let authority_plan = expect_plan(&mut relay_authority_rx, "relay_authority").await;
    let member_plan = expect_plan(&mut webrtc_member_rx, "webrtc_member").await;
    for plan in [&authority_plan, &member_plan] {
        assert_eq!(plan.topology, Topology::Host, "topology is sticky");
        assert_eq!(plan.transport, Transport::WebRtc, "transport is sticky");
        assert_eq!(
            plan.host,
            Some(webrtc_member),
            "the capability filter must exclude the relay-only authority from election"
        );
    }
    // Peer lists are capability-filtered on both sides: the relay-only
    // authority is not told to offer to the new host (empty peers — the relay
    // floor is its data path), and the new host's plan does not list the
    // relay-only member as a client (its only would-be client cannot signal).
    assert!(
        authority_plan.peers.is_empty(),
        "the relay-only member's plan must carry an empty peer list"
    );
    assert!(
        member_plan.peers.is_empty(),
        "the new host's plan must not list the transport-incapable member"
    );
    assert_eq!(
        server.active_session_plan(&room_id),
        Some(ActiveSessionPlan {
            topology: Topology::Host,
            transport: Transport::WebRtc,
            host: Some(webrtc_member),
        })
    );
    assert_eq!(
        server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        replans_before + 1,
        "one re-plan event"
    );

    // Exactly one plan each; the departed host got nothing.
    assert_silent(&mut relay_authority_rx).await;
    assert_silent(&mut webrtc_member_rx).await;
    assert_silent(&mut host_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn host_failover_replan_filters_peer_lists_to_session_capable_members() {
    // End-to-end failover shape of the capability filter across THREE remaining
    // members {relay-only, capable_a, capable_b}: capable_a (earliest capable
    // joiner) is elected; its host plan lists ONLY capable_b; capable_b's plan
    // still targets the new host (initiate=true); the relay-only member — a v3
    // recipient — receives its plan with EMPTY peers (it must not be told to
    // offer to the host). Every plan names the same elected host.
    let server = create_server_with_session(host_config()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (relay_only, mut relay_only_rx) = register_client(&server).await;
    let (capable_a, mut capable_a_rx) = register_client(&server).await;
    let (capable_b, mut capable_b_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_webrtc());
    server.set_client_protocol(&relay_only, v3_relay_only());
    server.set_client_protocol(&capable_a, v3_webrtc());
    server.set_client_protocol(&capable_b, v3_webrtc());

    // relay_only is listed first => earliest joiner among the remaining (it
    // would win the election if the capability filter were ignored).
    let room_id = finalized_db_room_with(
        &server,
        host,
        &[
            (relay_only, "RelayOnly"),
            (capable_a, "CapableA"),
            (capable_b, "CapableB"),
        ],
    )
    .await;
    // The running session is host+webrtc (the relay-only member is modeled as
    // a post-finalize seat-filler; a real finalize with it present would have
    // resolved to the relay floor and stored nothing).
    server.active_session_plans.insert(
        room_id,
        ActiveSessionPlan {
            topology: Topology::Host,
            transport: Transport::WebRtc,
            host: Some(host),
        },
    );

    server
        .database
        .remove_player_from_room(&room_id, &host)
        .await
        .expect("remove host");
    server
        .handle_session_member_departure(&room_id, &host)
        .await;

    let relay_only_plan = expect_plan(&mut relay_only_rx, "relay_only").await;
    let plan_a = expect_plan(&mut capable_a_rx, "capable_a").await;
    let plan_b = expect_plan(&mut capable_b_rx, "capable_b").await;
    for plan in [&relay_only_plan, &plan_a, &plan_b] {
        assert_eq!(plan.topology, Topology::Host, "topology is sticky");
        assert_eq!(plan.transport, Transport::WebRtc, "transport is sticky");
        assert_eq!(
            plan.host,
            Some(capable_a),
            "the earliest session-capable joiner is elected"
        );
    }

    // The new host's star contains only the capable client.
    assert_eq!(
        plan_a.peers.len(),
        1,
        "the host's plan lists only session-capable clients"
    );
    assert_eq!(plan_a.peers[0].player_id, capable_b);
    assert!(!plan_a.peers[0].initiate, "the host initiates to none");

    // The capable client still offers to the new host.
    assert_eq!(plan_b.peers.len(), 1);
    assert_eq!(plan_b.peers[0].player_id, capable_a);
    assert!(
        plan_b.peers[0].initiate,
        "capable clients offer to the host"
    );

    // The relay-only member's plan instructs no pairing at all.
    assert!(
        relay_only_plan.peers.is_empty(),
        "a relay-only member must not be told to initiate to the host"
    );

    // Exactly one plan each; the departed host got nothing.
    assert_silent(&mut relay_only_rx).await;
    assert_silent(&mut capable_a_rx).await;
    assert_silent(&mut capable_b_rx).await;
    assert_silent(&mut host_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn host_departure_with_no_electable_member_drops_plan_without_replan() {
    // No remaining member can run the stored host+webrtc session (one pure v2
    // member, one v3 relay-only member): the stored entry is REMOVED, nobody
    // receives a SessionPlan (the v3 relay-only member would have received one
    // if emission wrongly proceeded), and `session_replans_emitted` does NOT
    // move — no re-plan happened; the relay floor carries the room.
    let server = create_server_with_session(host_config()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (legacy, mut legacy_rx) = register_client(&server).await;
    let (relay_only, mut relay_only_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_webrtc());
    // legacy stays on the default v2 / relay-only protocol.
    server.set_client_protocol(&relay_only, v3_relay_only());

    let room_id = finalized_db_room_with(
        &server,
        host,
        &[(legacy, "Legacy"), (relay_only, "RelayOnly")],
    )
    .await;
    server.active_session_plans.insert(
        room_id,
        ActiveSessionPlan {
            topology: Topology::Host,
            transport: Transport::WebRtc,
            host: Some(host),
        },
    );

    server
        .database
        .remove_player_from_room(&room_id, &host)
        .await
        .expect("remove host");
    let replans_before = server
        .metrics
        .session_replans_emitted
        .load(Ordering::Relaxed);
    server
        .handle_session_member_departure(&room_id, &host)
        .await;

    assert!(
        server.active_session_plan(&room_id).is_none(),
        "with no electable member the stored plan must be dropped"
    );
    assert_eq!(
        server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        replans_before,
        "dropping the plan is NOT a re-plan event"
    );
    assert_silent(&mut legacy_rx).await;
    assert_silent(&mut relay_only_rx).await;
    assert_silent(&mut host_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn non_host_departure_heals_already_missing_host() {
    // Self-heal pin: the stored host is ALREADY absent from the member list
    // (hand-wired, simulating the finalize insert-after-departure race, a
    // concurrent host+candidate departure interleaving, or a departure hook
    // skipped by a transient storage error). A NON-host member departing must
    // still trigger re-election — the gate is "the stored host is invalid
    // (missing or unpairable)", not "the departed player was the host" —
    // emitting fresh plans to the remaining members and counting one re-plan
    // event.
    let server = create_server_with_session(host_config()).await;
    let (owner, mut owner_rx) = register_client(&server).await;
    let (member_b, mut member_b_rx) = register_client(&server).await;
    let (member_c, mut member_c_rx) = register_client(&server).await;
    for id in [&owner, &member_b, &member_c] {
        server.set_client_protocol(id, v3_webrtc());
    }

    let room_id = finalized_db_room_with(&server, owner, &[(member_b, "B"), (member_c, "C")]).await;
    // Wedged entry: the recorded host was never (or is no longer) a member.
    let ghost_host = PlayerId::new_v4();
    server.active_session_plans.insert(
        room_id,
        ActiveSessionPlan {
            topology: Topology::Host,
            transport: Transport::WebRtc,
            host: Some(ghost_host),
        },
    );

    // A NON-host member departs.
    server
        .database
        .remove_player_from_room(&room_id, &member_c)
        .await
        .expect("remove member_c");
    let replans_before = server
        .metrics
        .session_replans_emitted
        .load(Ordering::Relaxed);
    server
        .handle_session_member_departure(&room_id, &member_c)
        .await;

    // The owner holds authority and qualifies, so it is the healed host; both
    // remaining members get exactly one fresh plan naming it.
    let owner_plan = expect_plan(&mut owner_rx, "owner").await;
    let b_plan = expect_plan(&mut member_b_rx, "member_b").await;
    for plan in [&owner_plan, &b_plan] {
        assert_eq!(plan.topology, Topology::Host);
        assert_eq!(plan.transport, Transport::WebRtc);
        assert_eq!(plan.host, Some(owner), "healed host is the authority");
    }
    assert_eq!(b_plan.peers.len(), 1);
    assert_eq!(b_plan.peers[0].player_id, owner);
    assert!(b_plan.peers[0].initiate, "clients offer to the healed host");
    assert_eq!(
        server.active_session_plan(&room_id),
        Some(ActiveSessionPlan {
            topology: Topology::Host,
            transport: Transport::WebRtc,
            host: Some(owner),
        }),
        "the wedged entry is healed in place"
    );
    assert_eq!(
        server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        replans_before + 1,
        "the heal is one re-plan event"
    );
    assert_silent(&mut owner_rx).await;
    assert_silent(&mut member_b_rx).await;
    assert_silent(&mut member_c_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn non_host_departure_heals_present_but_unpairable_host() {
    // The widened `host_invalid` gate: the stored host is still a MEMBER but
    // its connection now reports relay-only capabilities (hand-wired, modeling
    // the reconnect-with-downgraded-capabilities race — the only way a seated
    // host can stop satisfying the session predicate). Departure re-planning
    // never fires for a member that stays seated, so before the widening this
    // wedged forever. A NON-host departure must now trigger re-election to the
    // capable member: both remaining v3 members get a fresh plan naming the
    // new host (the downgraded ex-host with EMPTY peers — it cannot pair), the
    // stored entry is rewritten, and one re-plan event is counted.
    let server = create_server_with_session(host_config()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (capable, mut capable_rx) = register_client(&server).await;
    let (departing, mut departing_rx) = register_client(&server).await;
    // The stored host's connection no longer carries the host+webrtc pair.
    server.set_client_protocol(&host, v3_relay_only());
    server.set_client_protocol(&capable, v3_webrtc());
    server.set_client_protocol(&departing, v3_webrtc());

    let room_id = finalized_db_room_with(
        &server,
        host,
        &[(capable, "Capable"), (departing, "Departing")],
    )
    .await;
    // The running session still names the (now-downgraded) host.
    server.active_session_plans.insert(
        room_id,
        ActiveSessionPlan {
            topology: Topology::Host,
            transport: Transport::WebRtc,
            host: Some(host),
        },
    );

    // A NON-host member departs; the host itself never left.
    server
        .database
        .remove_player_from_room(&room_id, &departing)
        .await
        .expect("remove departing");
    let replans_before = server
        .metrics
        .session_replans_emitted
        .load(Ordering::Relaxed);
    server
        .handle_session_member_departure(&room_id, &departing)
        .await;

    // The capable member is elected (the downgraded host also holds authority
    // as the room owner — the capability filter must outrank it).
    let host_plan = expect_plan(&mut host_rx, "downgraded ex-host").await;
    let capable_plan = expect_plan(&mut capable_rx, "capable").await;
    for plan in [&host_plan, &capable_plan] {
        assert_eq!(plan.topology, Topology::Host, "topology is sticky");
        assert_eq!(plan.transport, Transport::WebRtc, "transport is sticky");
        assert_eq!(
            plan.host,
            Some(capable),
            "a present-but-unpairable host must be replaced by the capable member"
        );
    }
    assert!(
        host_plan.peers.is_empty(),
        "the downgraded ex-host cannot pair; its data path is the relay floor"
    );
    assert!(
        capable_plan.peers.is_empty(),
        "the new host's star excludes the unpairable ex-host"
    );
    assert_eq!(
        server.active_session_plan(&room_id),
        Some(ActiveSessionPlan {
            topology: Topology::Host,
            transport: Transport::WebRtc,
            host: Some(capable),
        }),
        "the wedged entry is healed in place"
    );
    assert_eq!(
        server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        replans_before + 1,
        "one re-plan event"
    );
    assert_silent(&mut host_rx).await;
    assert_silent(&mut capable_rx).await;
    assert_silent(&mut departing_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn departure_with_unpairable_host_and_no_qualifier_drops_plan() {
    // The widened gate's no-qualifier arm: the stored host is a member but
    // downgraded to relay-only, and the only other member (a v2 client) then
    // departs. `host_invalid` fires, no remaining member can run the session,
    // so the stored entry is REMOVED, nothing is emitted (the v3 ex-host gets
    // no plan), and `session_replans_emitted` does not move — the relay floor
    // carries the room.
    let server = create_server_with_session(host_config()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (legacy, mut legacy_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_relay_only());
    // legacy stays on the default v2 / relay-only protocol.

    let room_id = finalized_db_room_with(&server, host, &[(legacy, "Legacy")]).await;
    server.active_session_plans.insert(
        room_id,
        ActiveSessionPlan {
            topology: Topology::Host,
            transport: Transport::WebRtc,
            host: Some(host),
        },
    );

    server
        .database
        .remove_player_from_room(&room_id, &legacy)
        .await
        .expect("remove legacy");
    let replans_before = server
        .metrics
        .session_replans_emitted
        .load(Ordering::Relaxed);
    server
        .handle_session_member_departure(&room_id, &legacy)
        .await;

    assert!(
        server.active_session_plan(&room_id).is_none(),
        "an unpairable host with no electable replacement must drop the stored plan"
    );
    assert_eq!(
        server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        replans_before,
        "dropping the plan is NOT a re-plan event"
    );
    assert_silent(&mut host_rx).await;
    assert_silent(&mut legacy_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn non_host_departures_do_not_replan() {
    // Sticky plans: a departure that changes no plan parameter re-emits nothing
    // and leaves the stored entry untouched. Covers BOTH a host-topology client
    // departure and a mesh member departure.
    // --- host topology, client departs ---
    let server = create_server_with_session(host_config()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (client_a, mut client_a_rx) = register_client(&server).await;
    let (client_b, _client_b_rx) = register_client(&server).await;
    for id in [&host, &client_a, &client_b] {
        server.set_client_protocol(id, v3_webrtc());
    }
    let room_id = finalized_db_room_with(
        &server,
        host,
        &[(client_a, "ClientA"), (client_b, "ClientB")],
    )
    .await;
    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    server
        .emit_session_plan(&room_id, &finalized("host-game", members, Some(host)))
        .await;
    let _ = expect_plan(&mut host_rx, "host").await;
    let _ = expect_plan(&mut client_a_rx, "client_a").await;
    let stored_before = server.active_session_plan(&room_id);
    let replans_before = server
        .metrics
        .session_replans_emitted
        .load(Ordering::Relaxed);

    server
        .database
        .remove_player_from_room(&room_id, &client_b)
        .await
        .expect("remove client_b");
    server
        .handle_session_member_departure(&room_id, &client_b)
        .await;

    assert_silent(&mut host_rx).await;
    assert_silent(&mut client_a_rx).await;
    assert_eq!(
        server.active_session_plan(&room_id),
        stored_before,
        "a client departure leaves the stored decision untouched"
    );
    assert_eq!(
        server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        replans_before
    );

    // --- mesh topology, member departs ---
    let mesh_server = create_server_with_session(mesh_config()).await;
    let (alice, mut alice_rx) = register_client(&mesh_server).await;
    let (bob, _bob_rx) = register_client(&mesh_server).await;
    let (carol, mut carol_rx) = register_client(&mesh_server).await;
    for id in [&alice, &bob, &carol] {
        mesh_server.set_client_protocol(id, v3_webrtc());
    }
    let mesh_room_id =
        finalized_db_room_with(&mesh_server, alice, &[(bob, "Bob"), (carol, "Carol")]).await;
    let mesh_members = mesh_server
        .database
        .get_room_players(&mesh_room_id)
        .await
        .expect("room players");
    mesh_server
        .emit_session_plan(&mesh_room_id, &finalized("mesh-game", mesh_members, None))
        .await;
    let _ = expect_plan(&mut alice_rx, "alice").await;
    let _ = expect_plan(&mut carol_rx, "carol").await;
    let mesh_stored_before = mesh_server.active_session_plan(&mesh_room_id);
    assert_eq!(
        mesh_stored_before.map(|plan| plan.topology),
        Some(Topology::Mesh)
    );

    mesh_server
        .database
        .remove_player_from_room(&mesh_room_id, &bob)
        .await
        .expect("remove bob");
    mesh_server
        .handle_session_member_departure(&mesh_room_id, &bob)
        .await;

    assert_silent(&mut alice_rx).await;
    assert_silent(&mut carol_rx).await;
    assert_eq!(
        mesh_server.active_session_plan(&mesh_room_id),
        mesh_stored_before,
        "a mesh member departure leaves the stored decision untouched"
    );
    assert_eq!(
        mesh_server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        0
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn departure_from_relay_floor_room_does_nothing() {
    // No stored decision (relay floor) => the hook is a no-op: no messages, no
    // counters, no entry materialized.
    let server = create_server_with_session(mesh_config()).await;
    let (alice, mut alice_rx) = register_client(&server).await;
    let (legacy, _legacy_rx) = register_client(&server).await;
    server.set_client_protocol(&alice, v3_webrtc());
    // legacy stays v2 / relay-only => the room finalized to the relay floor.

    let room_id = finalized_db_room_with(&server, alice, &[(legacy, "Legacy")]).await;
    assert!(server.active_session_plan(&room_id).is_none());

    server
        .database
        .remove_player_from_room(&room_id, &legacy)
        .await
        .expect("remove legacy");
    server
        .handle_session_member_departure(&room_id, &legacy)
        .await;

    assert_silent(&mut alice_rx).await;
    assert!(server.active_session_plan(&room_id).is_none());
    assert_eq!(
        server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        0
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn departure_from_non_finalized_room_does_nothing() {
    // A stored entry with a non-Finalized room (anomalous) emits nothing — there
    // is no live session to re-plan.
    let server = create_server_with_session(host_config()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (client, mut client_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_webrtc());
    server.set_client_protocol(&client, v3_webrtc());

    // Room stays in the default Waiting state (capacity 3 leaves it unfull).
    let room = server
        .database
        .create_room(
            "host-game".to_string(),
            None,
            3,
            true,
            host,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("create room");
    server
        .database
        .add_player_to_room(&room.id, player_info(client, "Client", false))
        .await
        .expect("add client");
    server.active_session_plans.insert(
        room.id,
        ActiveSessionPlan {
            topology: Topology::Host,
            transport: Transport::WebRtc,
            host: Some(host),
        },
    );

    server
        .database
        .remove_player_from_room(&room.id, &host)
        .await
        .expect("remove host");
    server
        .handle_session_member_departure(&room.id, &host)
        .await;

    assert_silent(&mut host_rx).await;
    assert_silent(&mut client_rx).await;
    assert_eq!(
        server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        0
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn last_member_departure_removes_stored_entry() {
    let server = create_server_with_session(host_config()).await;
    let (host, mut host_rx) = register_client(&server).await;
    let (client, mut client_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_webrtc());
    server.set_client_protocol(&client, v3_webrtc());

    let room_id = finalized_db_room_with(&server, host, &[(client, "Client")]).await;
    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    server
        .emit_session_plan(&room_id, &finalized("host-game", members, Some(host)))
        .await;
    let _ = expect_plan(&mut host_rx, "host").await;
    let _ = expect_plan(&mut client_rx, "client").await;

    // Client leaves first: triggers nothing (non-host), entry retained.
    server
        .database
        .remove_player_from_room(&room_id, &client)
        .await
        .expect("remove client");
    server
        .handle_session_member_departure(&room_id, &client)
        .await;
    assert!(server.active_session_plan(&room_id).is_some());

    // The host departs an otherwise-empty room: the entry is removed without
    // any re-emission (nobody is left to receive one).
    server
        .database
        .remove_player_from_room(&room_id, &host)
        .await
        .expect("remove host");
    server
        .handle_session_member_departure(&room_id, &host)
        .await;

    assert!(
        server.active_session_plan(&room_id).is_none(),
        "the stored decision is dropped when the room empties"
    );
    assert_silent(&mut host_rx).await;
    assert_silent(&mut client_rx).await;
    assert_eq!(
        server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        0
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn departure_when_room_is_gone_removes_stored_entry() {
    // The room vanished from storage (cleaned up concurrently): the stale
    // stored decision is dropped and nothing is emitted.
    let server = create_server_with_session(host_config()).await;
    let (host, mut host_rx) = register_client(&server).await;
    server.set_client_protocol(&host, v3_webrtc());

    let room_id = uuid::Uuid::new_v4(); // never created in storage
    server.active_session_plans.insert(
        room_id,
        ActiveSessionPlan {
            topology: Topology::Host,
            transport: Transport::WebRtc,
            host: Some(host),
        },
    );

    server
        .handle_session_member_departure(&room_id, &host)
        .await;

    assert!(server.active_session_plan(&room_id).is_none());
    assert_silent(&mut host_rx).await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn prune_active_session_plans_drops_only_dead_rooms() {
    // The maintenance sweep removes entries whose room no longer exists and
    // keeps entries for live rooms.
    let server = create_server_with_session(host_config()).await;
    let (owner, _owner_rx) = register_client(&server).await;
    server.set_client_protocol(&owner, v3_webrtc());

    let live_room = server
        .database
        .create_room(
            "host-game".to_string(),
            None,
            2,
            true,
            owner,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("create room");
    let dead_room_id = uuid::Uuid::new_v4();
    let entry = ActiveSessionPlan {
        topology: Topology::Mesh,
        transport: Transport::WebRtc,
        host: None,
    };
    server.active_session_plans.insert(live_room.id, entry);
    server.active_session_plans.insert(dead_room_id, entry);

    let removed = server.prune_active_session_plans().await;

    assert_eq!(removed, 1, "exactly the dead room's entry is pruned");
    assert_eq!(server.active_session_plan(&live_room.id), Some(entry));
    assert!(server.active_session_plan(&dead_room_id).is_none());
}

#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(miri, ignore)]
async fn leave_room_host_disconnect_triggers_failover_replan() {
    // End-to-end through the real choke point: `leave_room` (the path BOTH
    // explicit LeaveRoom and disconnects take) broadcasts PlayerLeft and then
    // re-plans. Each remaining member sees PlayerLeft FOLLOWED BY a fresh
    // SessionPlan naming the re-elected host.
    let server = create_server_with_session(host_config()).await;
    let (host, _host_rx) = register_client(&server).await;
    let (client_a, mut client_a_rx) = register_client(&server).await;
    let (client_b, mut client_b_rx) = register_client(&server).await;
    for id in [&host, &client_a, &client_b] {
        server.set_client_protocol(id, v3_webrtc());
    }

    let room_id = finalized_db_room_with(
        &server,
        host,
        &[(client_a, "ClientA"), (client_b, "ClientB")],
    )
    .await;
    // Room assignment wires the message coordinator so broadcast + targeted
    // sends reach the receivers.
    for id in [&host, &client_a, &client_b] {
        server
            .connection_manager
            .assign_client_to_room(id, room_id)
            .await;
    }
    let members = server
        .database
        .get_room_players(&room_id)
        .await
        .expect("room players");
    server
        .emit_session_plan(&room_id, &finalized("host-game", members, Some(host)))
        .await;
    let _ = expect_plan(&mut client_a_rx, "client_a").await;
    let _ = expect_plan(&mut client_b_rx, "client_b").await;

    // The host leaves through the production path.
    server.leave_room(&host).await;

    for (rx, who) in [
        (&mut client_a_rx, "client_a"),
        (&mut client_b_rx, "client_b"),
    ] {
        match recv(rx).await.as_ref() {
            ServerMessage::PlayerLeft { player_id } => assert_eq!(*player_id, host),
            other => panic!("{who} expected PlayerLeft first, got {other:?}"),
        }
        let plan = expect_plan(rx, who).await;
        assert_eq!(plan.topology, Topology::Host);
        assert_eq!(plan.transport, Transport::WebRtc);
        assert_eq!(
            plan.host,
            Some(client_a),
            "{who}'s fresh plan names the re-elected host (earliest remaining joiner)"
        );
    }
    assert_silent(&mut client_a_rx).await;
    assert_silent(&mut client_b_rx).await;
    assert_eq!(
        server
            .metrics
            .session_replans_emitted
            .load(Ordering::Relaxed),
        1
    );
}

// ---------------------------------------------------------------------------
// Property-based invariants over the pure selection core (Deliverable C2).
//
// These complement the example-based tables above: proptest drives randomized
// member sets (capability profiles, join times, authority designations) and
// randomized `SessionConfig`s through `choose_session_plan` / `elect_host` /
// `plan_for` and asserts the CONTRACT-level invariants (legal pairs, the
// desired ceiling, first-fit ladder semantics, permutation stability, subset
// monotonicity, glare antisymmetry, star shape, capability filtering) by
// recomputing each expectation independently from member/config data — never
// by calling the code under test a second way.
//
// All proptest tests carry `#[cfg_attr(miri, ignore)]` per repository policy
// (see tests/ci_config_tests.rs::test_proptest_tests_ignored_under_miri).
// ---------------------------------------------------------------------------
mod properties {
    use super::super::session_policy::{
        choose_session_plan, elect_host, is_valid_pair, ActiveSessionPlan, SessionMember,
        SessionPlanDecision, RELAY_FLOOR, UPGRADE_LADDER,
    };
    use super::super::signaling::local_initiates;
    use super::base_time;
    use crate::config::SessionConfig;
    use crate::protocol::{IceServer, PlayerId, Topology, Transport};
    use proptest::prelude::*;
    use std::collections::HashMap;
    use uuid::Uuid;

    /// The game name the generated configs may map; selection is queried with
    /// either this name or an unmapped one, exercising both the per-game
    /// override and the default-topology fallback.
    const MAPPED_GAME: &str = "mapped-game";
    const UNMAPPED_GAME: &str = "unmapped-game";

    // -- Independent re-computations of the documented contract --------------
    // (deliberately NOT calling the private helpers in session_policy.rs)

    /// ADR-0001 §1 richness rank: Relay < Host < Mesh.
    fn rank(topology: Topology) -> u8 {
        match topology {
            Topology::Relay => 0,
            Topology::Host => 1,
            Topology::Mesh => 2,
        }
    }

    /// Config transport gates: relay is the mandatory floor.
    fn transport_allowed(cfg: &SessionConfig, transport: Transport) -> bool {
        match transport {
            Transport::Relay => true,
            Transport::Direct => cfg.enable_direct,
            Transport::WebRtc => cfg.enable_webrtc,
        }
    }

    /// Appendix D capability predicate, recomputed from raw member data.
    fn member_supports(member: &SessionMember, topology: Topology, transport: Transport) -> bool {
        member.version >= 3
            && member.topologies.contains(&topology)
            && member.transports.contains(&transport)
    }

    /// Whether one ladder rung fits: ceiling + config gate + every member.
    fn rung_fits(
        members: &[SessionMember],
        desired: Topology,
        cfg: &SessionConfig,
        (topology, transport): (Topology, Transport),
    ) -> bool {
        rank(topology) <= rank(desired)
            && transport_allowed(cfg, transport)
            && !members.is_empty()
            && members
                .iter()
                .all(|member| member_supports(member, topology, transport))
    }

    /// The desired ceiling exactly as `choose_session_plan` resolves it.
    fn desired_for(cfg: &SessionConfig, game: &str) -> Topology {
        cfg.game_topology_mappings
            .get(game)
            .copied()
            .unwrap_or(cfg.default_topology)
    }

    /// Index of the pair in the richest-first ladder (floor = ladder length),
    /// so "never poorer" is a simple `<=` on indices.
    fn rung_index(topology: Topology, transport: Transport) -> usize {
        UPGRADE_LADDER
            .into_iter()
            .position(|rung| rung == (topology, transport))
            .unwrap_or(UPGRADE_LADDER.len())
    }

    // -- Strategies -----------------------------------------------------------

    /// One member's raw capability surface + a small join-time offset (small
    /// range on purpose, so equal `joined_at` values occur and the
    /// smaller-UUID election tie-break is actually exercised).
    fn arb_caps() -> impl Strategy<Value = (u16, Vec<Transport>, Vec<Topology>, i64)> {
        (
            2u16..=4u16,
            proptest::sample::subsequence(
                vec![Transport::Relay, Transport::Direct, Transport::WebRtc],
                0..=3,
            ),
            proptest::sample::subsequence(
                vec![Topology::Relay, Topology::Host, Topology::Mesh],
                0..=3,
            ),
            0i64..4i64,
        )
    }

    /// 1..=max members with DISTINCT ids. Ids are assigned from a shuffled
    /// pool so UUID order is decorrelated from join order (otherwise the
    /// earliest joiner would always hold the smallest UUID and the glare /
    /// tie-break assertions would never see the other orderings).
    fn arb_members(max: usize) -> impl Strategy<Value = Vec<SessionMember>> {
        proptest::collection::vec(arb_caps(), 1..=max)
            .prop_flat_map(|caps| {
                let ids: Vec<u128> = (1..=caps.len() as u128).collect();
                (Just(caps), Just(ids).prop_shuffle())
            })
            .prop_map(|(caps, ids)| {
                caps.into_iter()
                    .zip(ids)
                    .map(
                        |((version, transports, topologies, offset), id)| SessionMember {
                            player_id: Uuid::from_u128(id),
                            player_name: format!("p{id}"),
                            is_authority: false,
                            joined_at: base_time() + chrono::Duration::seconds(offset),
                            version,
                            transports,
                            topologies,
                        },
                    )
                    .collect()
            })
    }

    /// A randomized session config (both gates, default topology, an optional
    /// per-game mapping) plus the game name selection is queried with.
    fn arb_cfg_and_game() -> impl Strategy<Value = (SessionConfig, &'static str)> {
        let topo =
            || proptest::sample::select(vec![Topology::Relay, Topology::Host, Topology::Mesh]);
        (
            any::<bool>(),
            any::<bool>(),
            topo(),
            proptest::option::of(topo()),
            any::<bool>(),
        )
            .prop_map(|(webrtc, direct, default_topology, mapped, use_mapped)| {
                let mut cfg = SessionConfig {
                    default_topology,
                    enable_webrtc: webrtc,
                    enable_direct: direct,
                    game_topology_mappings: HashMap::new(),
                    ..SessionConfig::default()
                };
                if let Some(mapped_topology) = mapped {
                    cfg.game_topology_mappings
                        .insert(MAPPED_GAME.to_string(), mapped_topology);
                }
                let game = if use_mapped {
                    MAPPED_GAME
                } else {
                    UNMAPPED_GAME
                };
                (cfg, game)
            })
    }

    /// An authority designation: absent, a seated member (by index), or an id
    /// that is NOT in the room (a departed authority is cleared in
    /// production, but `elect_host` must also tolerate an absent id).
    fn arb_authority(member_count: usize) -> impl Strategy<Value = Option<PlayerId>> {
        prop_oneof![
            Just(None),
            (0..member_count).prop_map(|idx| Some(Uuid::from_u128(idx as u128 + 1))),
            Just(Some(Uuid::from_u128(0xDEAD_0001))),
        ]
    }

    /// One of the three legal non-relay rungs (a stored `ActiveSessionPlan`
    /// pair, as recorded at finalize).
    fn arb_stored_pair() -> impl Strategy<Value = (Topology, Transport)> {
        proptest::sample::select(UPGRADE_LADDER.to_vec())
    }

    // -- choose_session_plan --------------------------------------------------

    proptest! {
        /// Glare designation (Appendix E): antisymmetric for distinct ids,
        /// irreflexive for any id — exactly one side of every pair offers.
        #[test]
        #[cfg_attr(miri, ignore)]
        fn local_initiates_is_antisymmetric_and_irreflexive(a in any::<u128>(), b in any::<u128>()) {
            let (a, b) = (Uuid::from_u128(a), Uuid::from_u128(b));
            prop_assert!(!local_initiates(a, a));
            prop_assert!(!local_initiates(b, b));
            if a != b {
                prop_assert_ne!(local_initiates(a, b), local_initiates(b, a));
                prop_assert_eq!(local_initiates(a, b), a < b);
            }
        }

        /// The selection is always one of the four legal pairs, never exceeds
        /// the desired ceiling, and is EXACTLY the first fitting rung of the
        /// richest-first ladder (the relay floor iff no rung fits).
        #[test]
        #[cfg_attr(miri, ignore)]
        fn selection_is_first_fitting_rung_under_ceiling(
            members in arb_members(5),
            (cfg, game) in arb_cfg_and_game(),
            authority in arb_authority(5),
        ) {
            let desired = desired_for(&cfg, game);
            let expected = UPGRADE_LADDER
                .into_iter()
                .find(|&rung| rung_fits(&members, desired, &cfg, rung))
                .unwrap_or(RELAY_FLOOR);

            let decision = choose_session_plan(game, authority, members.clone(), &cfg);

            prop_assert!(is_valid_pair(decision.topology, decision.transport));
            prop_assert!(rank(decision.topology) <= rank(desired), "ceiling violated");
            prop_assert_eq!(
                (decision.topology, decision.transport),
                expected,
                "selection must be the first fitting rung (or the floor when none fits)"
            );
            // Relay iff no rung fits — implied by the equality above, restated
            // as the explicit floor-soundness contract.
            let any_rung_fits = UPGRADE_LADDER
                .into_iter()
                .any(|rung| rung_fits(&members, desired, &cfg, rung));
            prop_assert_eq!(decision.topology == Topology::Relay, !any_rung_fits);
        }

        /// Host shape: a host is elected iff the topology is `host`; the host
        /// is always a seated member; a seated authority is always preferred.
        #[test]
        #[cfg_attr(miri, ignore)]
        fn selection_host_shape_matches_topology_and_authority(
            members in arb_members(5),
            (cfg, game) in arb_cfg_and_game(),
            authority in arb_authority(5),
        ) {
            let decision = choose_session_plan(game, authority, members.clone(), &cfg);

            if decision.topology == Topology::Host {
                let host = decision.host.expect("host topology elects a host");
                prop_assert!(members.iter().any(|m| m.player_id == host));
                if let Some(auth) = authority {
                    if members.iter().any(|m| m.player_id == auth) {
                        prop_assert_eq!(host, auth, "seated authority must be preferred");
                    }
                }
            } else {
                prop_assert_eq!(decision.host, None);
            }
        }

        /// Member-order independence: permuting the member list never changes
        /// the selected (topology, transport, host).
        #[test]
        #[cfg_attr(miri, ignore)]
        fn selection_is_member_order_independent(
            (members, shuffled) in arb_members(5)
                .prop_flat_map(|m| (Just(m.clone()), Just(m).prop_shuffle())),
            (cfg, game) in arb_cfg_and_game(),
            authority in arb_authority(5),
        ) {
            let a = choose_session_plan(game, authority, members, &cfg);
            let b = choose_session_plan(game, authority, shuffled, &cfg);
            prop_assert_eq!(a.topology, b.topology);
            prop_assert_eq!(a.transport, b.transport);
            prop_assert_eq!(a.host, b.host);
        }

        /// Subset monotonicity: removing members (any non-empty subset, same
        /// config and game) never makes the chosen rung POORER — `all_support`
        /// is a universal quantifier, so every rung that fits a set fits each
        /// of its non-empty subsets, and the first-fit walk can only move up.
        #[test]
        #[cfg_attr(miri, ignore)]
        fn selection_is_subset_monotone(
            (members, subset_indices) in arb_members(6).prop_flat_map(|m| {
                let len = m.len();
                let indices: Vec<usize> = (0..len).collect();
                (Just(m), proptest::sample::subsequence(indices, 1..=len))
            }),
            (cfg, game) in arb_cfg_and_game(),
            authority in arb_authority(6),
        ) {
            let subset: Vec<SessionMember> = subset_indices
                .iter()
                .map(|&i| members[i].clone())
                .collect();

            let full = choose_session_plan(game, authority, members, &cfg);
            let sub = choose_session_plan(game, authority, subset, &cfg);

            prop_assert!(
                rung_index(sub.topology, sub.transport)
                    <= rung_index(full.topology, full.transport),
                "subset selected a poorer rung ({:?},{:?}) than the superset ({:?},{:?})",
                sub.topology, sub.transport, full.topology, full.transport
            );
        }
    }

    // -- elect_host -----------------------------------------------------------

    proptest! {
        /// Election is permutation-stable and lands on the spec-level minimum:
        /// a seated authority wins outright; otherwise the elected member has
        /// the minimal (joined_at, player_id) key — i.e. earliest joiner with
        /// the smaller-UUID tie-break (ties DO occur here: join offsets are
        /// drawn from a 4-value range).
        #[test]
        #[cfg_attr(miri, ignore)]
        fn elect_host_is_permutation_stable_and_minimal(
            (members, shuffled) in arb_members(6)
                .prop_flat_map(|m| (Just(m.clone()), Just(m).prop_shuffle())),
            authority in arb_authority(6),
        ) {
            let elected = elect_host(authority, &members);
            prop_assert_eq!(elect_host(authority, &shuffled), elected, "permutation changed the host");

            let elected = elected.expect("non-empty member list elects a host");
            let seated_authority = authority
                .filter(|id| members.iter().any(|m| m.player_id == *id));
            match seated_authority {
                Some(auth) => prop_assert_eq!(elected, auth, "seated authority must win"),
                None => {
                    let winner = members
                        .iter()
                        .find(|m| m.player_id == elected)
                        .expect("elected host is a member");
                    for member in &members {
                        prop_assert!(
                            (winner.joined_at, winner.player_id)
                                <= (member.joined_at, member.player_id),
                            "elected host must minimize (joined_at, player_id)"
                        );
                    }
                }
            }
        }

        /// The replan-site election (replan_host_session): the member slice is
        /// pre-filtered to session-capable members and the authority preference
        /// passes through the SAME filter, so an authority that cannot run the
        /// session is neither preferred nor electable — authority never
        /// outranks the capability gate. Mirrors the production filter exactly
        /// (capability recomputed independently).
        #[test]
        #[cfg_attr(miri, ignore)]
        fn replan_election_filters_authority_and_members_by_capability(
            members in arb_members(6),
            authority in arb_authority(6),
            (topology, transport) in arb_stored_pair(),
        ) {
            // The production filter from replan_host_session, with the
            // capability predicate recomputed from raw member data.
            let electable: Vec<SessionMember> = members
                .iter()
                .filter(|m| member_supports(m, topology, transport))
                .cloned()
                .collect();
            let electable_authority = authority
                .filter(|id| electable.iter().any(|m| m.player_id == *id));

            let elected = elect_host(electable_authority, &electable);

            prop_assert_eq!(elected.is_none(), electable.is_empty(), "no qualifier <=> no host");
            if let Some(host) = elected {
                let host_member = members
                    .iter()
                    .find(|m| m.player_id == host)
                    .expect("elected host is a member");
                prop_assert!(
                    member_supports(host_member, topology, transport),
                    "an elected host must be capable of the stored session pair"
                );
                // A capable seated authority always wins the filtered election.
                if let Some(auth) = authority {
                    if electable.iter().any(|m| m.player_id == auth) {
                        prop_assert_eq!(host, auth);
                    } else {
                        // An incapable (or absent) authority must never be
                        // elected unless it independently qualifies — which
                        // the branch above already covers.
                        prop_assert!(
                            host != auth
                                || members
                                    .iter()
                                    .any(|m| m.player_id == auth
                                        && member_supports(m, topology, transport))
                        );
                    }
                }
            }
        }
    }

    // -- plan_for ---------------------------------------------------------------

    /// Assert every per-recipient contract of `plan_for` for one decision:
    /// recipient exclusion, two-sided capability filtering, glare-correct mesh
    /// flags, star-shaped host plans, empty peers for non-pairable recipients,
    /// verbatim ICE passthrough, and the relay fallback constant.
    fn assert_plan_for_contract(
        decision: &SessionPlanDecision,
        members: &[SessionMember],
    ) -> Result<(), TestCaseError> {
        for recipient in members {
            let r = recipient.player_id;
            let ice = vec![IceServer {
                urls: vec![format!("stun:probe/{r}")],
                username: None,
                credential: None,
            }];
            let plan = decision.plan_for(r, ice.clone());

            // Envelope: the decision's pair/host, the floor fallback, and the
            // caller's ICE list passed through verbatim (per-recipient TURN
            // minting happens at the emit site, not in plan_for).
            prop_assert_eq!(plan.topology, decision.topology);
            prop_assert_eq!(plan.transport, decision.transport);
            prop_assert_eq!(plan.host, decision.host);
            prop_assert_eq!(plan.fallback, Transport::Relay);
            prop_assert_eq!(
                serde_json::to_value(&plan.ice_servers).expect("ice serializes"),
                serde_json::to_value(&ice).expect("ice serializes"),
                "plan_for must pass the recipient's ICE list through verbatim"
            );

            // Peers never include the recipient and never include a member
            // that cannot run the session pair (two-sided filter).
            for peer in &plan.peers {
                prop_assert_ne!(peer.player_id, r, "recipient listed as its own peer");
                let peer_member = members
                    .iter()
                    .find(|m| m.player_id == peer.player_id)
                    .expect("every listed peer is a member");
                prop_assert!(
                    member_supports(peer_member, decision.topology, decision.transport),
                    "peer list names a member that cannot run the session"
                );
            }

            let recipient_capable =
                member_supports(recipient, decision.topology, decision.transport);
            if !recipient_capable {
                prop_assert!(
                    plan.peers.is_empty(),
                    "a non-pairable recipient must receive an empty peer list"
                );
                continue;
            }

            match decision.topology {
                Topology::Mesh => {
                    // Exactness: every OTHER capable member, with the glare flag.
                    let mut expected: Vec<(PlayerId, bool)> = members
                        .iter()
                        .filter(|m| {
                            m.player_id != r
                                && member_supports(m, decision.topology, decision.transport)
                        })
                        .map(|m| (m.player_id, local_initiates(r, m.player_id)))
                        .collect();
                    let mut actual: Vec<(PlayerId, bool)> = plan
                        .peers
                        .iter()
                        .map(|p| (p.player_id, p.initiate))
                        .collect();
                    expected.sort();
                    actual.sort();
                    prop_assert_eq!(actual, expected, "mesh peer set must be exact");
                }
                Topology::Host => {
                    let host = decision
                        .host
                        .expect("host decisions under test carry a host");
                    if r == host {
                        // The host answers every capable client.
                        let mut expected: Vec<(PlayerId, bool)> = members
                            .iter()
                            .filter(|m| {
                                m.player_id != host
                                    && member_supports(m, decision.topology, decision.transport)
                            })
                            .map(|m| (m.player_id, false))
                            .collect();
                        let mut actual: Vec<(PlayerId, bool)> = plan
                            .peers
                            .iter()
                            .map(|p| (p.player_id, p.initiate))
                            .collect();
                        expected.sort();
                        actual.sort();
                        prop_assert_eq!(actual, expected, "host peer set must be exact");
                        for peer in &plan.peers {
                            prop_assert!(!peer.is_authority, "clients are not the authority peer");
                        }
                    } else {
                        // A capable client offers to exactly the host.
                        prop_assert_eq!(plan.peers.len(), 1, "star clients see only the host");
                        prop_assert_eq!(plan.peers[0].player_id, host);
                        prop_assert!(plan.peers[0].initiate, "clients offer to the host");
                        prop_assert!(plan.peers[0].is_authority, "the host peer is the authority");
                    }
                }
                Topology::Relay => {
                    prop_assert!(plan.peers.is_empty(), "relay floor plans carry no peers");
                }
            }
        }
        Ok(())
    }

    proptest! {
        /// plan_for over finalize-shaped decisions (built by
        /// `choose_session_plan`, where a non-relay pair implies every member
        /// is capable, so the capability filter is provably a no-op there).
        #[test]
        #[cfg_attr(miri, ignore)]
        fn plan_for_contract_holds_for_finalize_decisions(
            members in arb_members(5),
            (cfg, game) in arb_cfg_and_game(),
            authority in arb_authority(5),
        ) {
            let decision = choose_session_plan(game, authority, members.clone(), &cfg);
            assert_plan_for_contract(&decision, &members)?;
        }

        /// plan_for over replan/late-join-shaped decisions: a STORED sticky
        /// pair rehydrated over a CURRENT member list that may contain members
        /// that never negotiated the pair (seat-fillers) — the case where the
        /// two-sided capability filter actually bites. The host is re-elected
        /// through the production capability filter; when nobody qualifies the
        /// hostless defensive arm must emit no peers at all.
        #[test]
        #[cfg_attr(miri, ignore)]
        fn plan_for_contract_holds_for_rehydrated_stored_plans(
            members in arb_members(5),
            authority in arb_authority(5),
            (topology, transport) in arb_stored_pair(),
        ) {
            let electable: Vec<SessionMember> = members
                .iter()
                .filter(|m| member_supports(m, topology, transport))
                .cloned()
                .collect();
            let electable_authority = authority
                .filter(|id| electable.iter().any(|m| m.player_id == *id));
            let host = if topology == Topology::Host {
                elect_host(electable_authority, &electable)
            } else {
                None
            };

            let stored = ActiveSessionPlan { topology, transport, host };
            let decision = stored.decision_with(members.clone());

            if topology == Topology::Host && host.is_none() {
                // Degenerate hostless host plan (production drops the entry
                // instead of emitting, but plan_for stays defensive): nobody
                // may be told to connect to anyone.
                for member in &members {
                    let plan = decision.plan_for(member.player_id, Vec::new());
                    prop_assert!(plan.peers.is_empty());
                }
            } else {
                assert_plan_for_contract(&decision, &members)?;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ICE pre-gather on RoomJoined / Reconnected (PLAN §P4's deferred refinement).
//
// Pure-logic tests pin the eligibility predicate's full gating matrix (every
// conjunct flipped independently) and the desired-topology lookup; server-based
// tests pin the shared composition helper's ordering (static ice_servers first,
// then the [turn] STUN entry, then the minted TURN entry) and the
// `pregather_ice_servers` metrics contract (one `ice_pregather_emitted` per
// non-empty list actually handed out; minted credentials counted on the shared
// `turn_credentials_issued` total; nothing counted for ineligible or
// nothing-configured calls).
// ---------------------------------------------------------------------------

/// A session config under which the pre-gather baseline below is eligible:
/// pre-gather on, WebRTC on, mesh desired by default, one relay-desired game
/// mapped for the per-game-override conjunct.
fn pregather_session_config() -> SessionConfig {
    let mut mappings = HashMap::new();
    mappings.insert("relay-game".to_string(), Topology::Relay);
    SessionConfig {
        default_topology: Topology::Mesh,
        game_topology_mappings: mappings,
        ..SessionConfig::default()
    }
}

#[test]
fn desired_topology_prefers_per_game_mapping_over_default() {
    let cfg = pregather_session_config();
    assert_eq!(desired_topology_for("relay-game", &cfg), Topology::Relay);
    assert_eq!(desired_topology_for("unmapped-game", &cfg), Topology::Mesh);
}

#[test]
fn ice_pregather_eligibility_flips_every_conjunct_independently() {
    let cfg = pregather_session_config();
    let v3 = v3_webrtc();

    // Baseline: every conjunct holds.
    assert!(ice_pregather_eligible(
        &cfg,
        "unmapped-game",
        &LobbyState::Waiting,
        &v3
    ));
    // The `Lobby` (full, pre-finalize) state is just as eligible as `Waiting`.
    assert!(ice_pregather_eligible(
        &cfg,
        "unmapped-game",
        &LobbyState::Lobby,
        &v3
    ));

    // 1. Kill switch off.
    let pregather_off = SessionConfig {
        enable_ice_pregather: false,
        ..pregather_session_config()
    };
    assert!(!ice_pregather_eligible(
        &pregather_off,
        "unmapped-game",
        &LobbyState::Waiting,
        &v3
    ));

    // 2. WebRTC transport disabled: no WebRTC plan can ever be selected, so
    //    pre-gathered candidates could never be used.
    let webrtc_off = SessionConfig {
        enable_webrtc: false,
        ..pregather_session_config()
    };
    assert!(!ice_pregather_eligible(
        &webrtc_off,
        "unmapped-game",
        &LobbyState::Waiting,
        &v3
    ));

    // 3. Relay-desired game (per-game mapping, and via the default).
    assert!(!ice_pregather_eligible(
        &cfg,
        "relay-game",
        &LobbyState::Waiting,
        &v3
    ));
    let relay_default = SessionConfig {
        default_topology: Topology::Relay,
        game_topology_mappings: HashMap::new(),
        ..SessionConfig::default()
    };
    assert!(!ice_pregather_eligible(
        &relay_default,
        "unmapped-game",
        &LobbyState::Waiting,
        &v3
    ));
    // A host-desired game is still eligible (host + webrtc is a WebRTC rung).
    let host_default = SessionConfig {
        default_topology: Topology::Host,
        game_topology_mappings: HashMap::new(),
        ..SessionConfig::default()
    };
    assert!(ice_pregather_eligible(
        &host_default,
        "unmapped-game",
        &LobbyState::Waiting,
        &v3
    ));

    // 4. Finalized room: the late-join/reconnect path delivers a fresh
    //    SessionPlan immediately after, so pre-gather would double-mint.
    assert!(!ice_pregather_eligible(
        &cfg,
        "unmapped-game",
        &LobbyState::Finalized,
        &v3
    ));

    // 5. Recipient below v3 (the default negotiated protocol is v2).
    assert!(!ice_pregather_eligible(
        &cfg,
        "unmapped-game",
        &LobbyState::Waiting,
        &NegotiatedProtocol::default()
    ));
    let v2_with_webrtc = NegotiatedProtocol {
        version: 2,
        ..v3_webrtc()
    };
    assert!(!ice_pregather_eligible(
        &cfg,
        "unmapped-game",
        &LobbyState::Waiting,
        &v2_with_webrtc
    ));

    // 6. Recipient without the WebRTC transport (even at v3, with the
    //    topology set intact).
    let v3_no_webrtc_transport = NegotiatedProtocol {
        transports: vec![Transport::Relay],
        ..v3_webrtc()
    };
    assert!(!ice_pregather_eligible(
        &cfg,
        "unmapped-game",
        &LobbyState::Waiting,
        &v3_no_webrtc_transport
    ));

    // 7. Recipient whose negotiated topologies miss the game's desired one
    //    (WebRTC transport intact): a relay-only topology set can never appear
    //    in any WebRTC plan, ...
    let v3_relay_only_topology = NegotiatedProtocol {
        topologies: vec![Topology::Relay],
        ..v3_webrtc()
    };
    assert!(!ice_pregather_eligible(
        &cfg,
        "unmapped-game",
        &LobbyState::Waiting,
        &v3_relay_only_topology
    ));
    // ... and a host-capped set cannot seat a mesh-desired game's rung ...
    let v3_host_capable = NegotiatedProtocol {
        topologies: vec![Topology::Relay, Topology::Host],
        ..v3_webrtc()
    };
    assert!(!ice_pregather_eligible(
        &cfg,
        "unmapped-game",
        &LobbyState::Waiting,
        &v3_host_capable
    ));
    // ... while containing the desired topology (not necessarily every
    // topology) is enough: the same host-capped set is eligible for a
    // host-desired game.
    assert!(ice_pregather_eligible(
        &host_default,
        "unmapped-game",
        &LobbyState::Waiting,
        &v3_host_capable
    ));
    // The canonical relay-floor v3 client (relay-only transport AND topology)
    // flips both recipient conjuncts at once and stays ineligible.
    assert!(!ice_pregather_eligible(
        &cfg,
        "unmapped-game",
        &LobbyState::Waiting,
        &v3_relay_only()
    ));
}

/// Build a `Room` fixture in the given lobby state for the pre-gather tests.
fn pregather_room(game_name: &str, lobby_state: LobbyState) -> Room {
    let mut room = Room::new(
        game_name.to_string(),
        "ROOM01".to_string(),
        4,
        true,
        "matchbox".to_string(),
    );
    room.lobby_state = lobby_state;
    room
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn composed_ice_servers_orders_static_then_stun_then_turn() {
    // The single shared composition seam (used by SessionPlan emission AND
    // pre-gather): operator's static list first (verbatim), then the [turn]
    // block's STUN entry, then the minted per-recipient TURN entry.
    let server = create_server_with_session_and_turn(mesh_config(), enabled_turn()).await;
    let (recipient, _rx) = register_client(&server).await;

    let now_unix = 1_700_000_000;
    let (ice, minted) = server.composed_ice_servers_for(recipient, now_unix);

    assert_eq!(ice.len(), 3, "static + STUN + TURN");
    // Static entry, verbatim and credential-less.
    assert_eq!(ice[0].urls, vec![STATIC_STUN_URL]);
    assert!(ice[0].username.is_none());
    assert!(ice[0].credential.is_none());
    // [turn] STUN entry, credential-less.
    assert_eq!(ice[1].urls, vec![TURN_STUN_URL]);
    assert!(ice[1].username.is_none());
    assert!(ice[1].credential.is_none());
    // Minted TURN entry embedding the shared expiry and THIS recipient's id.
    assert_eq!(ice[2].urls, vec![TURN_URL]);
    assert_eq!(
        ice[2].username.as_deref(),
        Some(
            format!(
                "{}:{recipient}",
                now_unix + i64::try_from(TURN_CREDENTIAL_TTL_SECS).expect("test TTL fits i64")
            )
            .as_str()
        )
    );
    assert!(ice[2].credential.is_some());
    assert_eq!(minted, 1, "exactly the credentialed TURN entry is minted");
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn composed_ice_servers_empty_when_nothing_configured() {
    // No static ICE, TURN disabled, no STUN urls: the composition is empty and
    // nothing is minted.
    let server = create_server_with_session_and_turn(mesh_config_no_static_ice(), turn_off()).await;
    let (recipient, _rx) = register_client(&server).await;

    let (ice, minted) = server.composed_ice_servers_for(recipient, 1_700_000_000);
    assert!(ice.is_empty());
    assert_eq!(minted, 0);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn pregather_counts_emission_and_minted_credentials_when_eligible() {
    let server =
        create_server_with_session_and_turn(mesh_config_no_static_ice(), enabled_turn()).await;
    let (joiner, _rx) = register_client(&server).await;
    server.set_client_protocol(&joiner, v3_webrtc());

    let room = pregather_room("mesh-game", LobbyState::Waiting);
    let emitted_before = server.metrics.ice_pregather_emitted.load(Ordering::Relaxed);
    let creds_before = server
        .metrics
        .turn_credentials_issued
        .load(Ordering::Relaxed);

    let ice = server.pregather_ice_servers(&room, &joiner);

    assert_eq!(ice.len(), 2, "STUN + minted TURN");
    let username = ice[1].username.as_deref().expect("TURN entry has username");
    assert!(
        username.ends_with(&format!(":{joiner}")),
        "credential embeds the joiner's id: {username}"
    );
    assert_eq!(
        server.metrics.ice_pregather_emitted.load(Ordering::Relaxed),
        emitted_before + 1,
        "one pre-gather emission per non-empty list"
    );
    assert_eq!(
        server
            .metrics
            .turn_credentials_issued
            .load(Ordering::Relaxed),
        creds_before + 1,
        "the minted pre-gather TURN credential counts on the shared issuance total"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn pregather_returns_empty_and_counts_nothing_when_ineligible() {
    // Finalized room and v2 recipient: both gates return empty and move no
    // counter (the finalized case is what protects the late-join path from
    // double-minting — its fresh SessionPlan is the only issuance site there).
    let server =
        create_server_with_session_and_turn(mesh_config_no_static_ice(), enabled_turn()).await;
    let (v3_joiner, _rx_a) = register_client(&server).await;
    let (v2_joiner, _rx_b) = register_client(&server).await;
    server.set_client_protocol(&v3_joiner, v3_webrtc());
    // v2_joiner stays on the default v2 / relay-only protocol.

    let emitted_before = server.metrics.ice_pregather_emitted.load(Ordering::Relaxed);
    let creds_before = server
        .metrics
        .turn_credentials_issued
        .load(Ordering::Relaxed);

    let finalized_room = pregather_room("mesh-game", LobbyState::Finalized);
    assert!(server
        .pregather_ice_servers(&finalized_room, &v3_joiner)
        .is_empty());

    let waiting_room = pregather_room("mesh-game", LobbyState::Waiting);
    assert!(server
        .pregather_ice_servers(&waiting_room, &v2_joiner)
        .is_empty());

    assert_eq!(
        server.metrics.ice_pregather_emitted.load(Ordering::Relaxed),
        emitted_before
    );
    assert_eq!(
        server
            .metrics
            .turn_credentials_issued
            .load(Ordering::Relaxed),
        creds_before
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn pregather_eligible_but_nothing_configured_emits_nothing_and_counts_nothing() {
    // Eligible recipient, but the composition is empty (no static ICE, no STUN
    // urls, TURN disabled): the field is skipped on the wire, so this must NOT
    // count as an emission.
    let server = create_server_with_session_and_turn(mesh_config_no_static_ice(), turn_off()).await;
    let (joiner, _rx) = register_client(&server).await;
    server.set_client_protocol(&joiner, v3_webrtc());

    let emitted_before = server.metrics.ice_pregather_emitted.load(Ordering::Relaxed);
    let room = pregather_room("mesh-game", LobbyState::Waiting);
    assert!(server.pregather_ice_servers(&room, &joiner).is_empty());
    assert_eq!(
        server.metrics.ice_pregather_emitted.load(Ordering::Relaxed),
        emitted_before,
        "an empty (skipped-on-the-wire) list is not an emission"
    );
}
