//! True multi-process protocol v3 interop: the REAL `signal-fish-server`
//! binary (via `SIGNAL_FISH_SERVER_BIN`) + three REAL
//! `signal-fish-reference-native` client processes, talking over real TCP
//! (WebSocket signaling) and real UDP (loopback WebRTC: DTLS + SCTP data
//! channels). This is the end-to-end proof of the documented server-brokered
//! offer/answer/trickle-ICE producing live data channels, with the WebSocket
//! relay floor carrying GameData in the same session window as the live
//! WebRTC pairs (no relay/pair ordering is asserted; post-pair WebSocket
//! liveness is separately proven by the `peer_transport_status` fan-outs).
//!
//! # Scenarios
//!
//! 1. `mesh_n3_full_webrtc_session_with_live_relay_floor` — the headline:
//!    a 3-peer mesh room negotiates `mesh + webrtc`, the glare matrix over the
//!    three per-recipient plans is total + antisymmetric (smaller UUID offers,
//!    the deterministic offerer rule), all 6 directed pair endpoints connect,
//!    one message per channel crosses every ordered pair (12 receive events),
//!    every client reports `TransportStatus{webrtc,true}` and observes both
//!    peers' reports (`PeerTransportStatus` fan-out), and every client's
//!    `--relay-payload` GameData reaches the other two over the WebSocket in the
//!    same session window as the live pairs (no relay/pair ordering is asserted;
//!    the post-pair status fan-outs prove the WebSocket outlives pairing).
//! 2. `host_star_n3_webrtc` — a `host + webrtc` star: all plans name the same
//!    host (the creator: earliest joiner wins the election), clients offer to
//!    the host (host plan lists `initiate: false` peers), pairs exist ONLY
//!    along star edges (4 directed endpoints, no client<->client traffic),
//!    and the exchange completes in both directions on both channels.
//! 3. `mesh_n3_partial_ice_cripple_relay_fallback` — one member runs with
//!    `--cripple-ice` (no usable local candidates registered; candidate
//!    signals dropped both ways): the healthy pair still connects and exchanges on both channels,
//!    the crippled member resolves `TransportStatus{webrtc,false}` exactly
//!    once and engages the relay fallback, the asymmetric status fan-out is
//!    observed by everyone, and the relay floor carries every member's
//!    payload (the crippled one is fully served by the floor).
//! 4. `late_join_authoritative_replan_real_webrtc_n3` — a seat-holder departs
//!    a finalized 3-seat mesh room (the server only finalizes FULL rooms, so a
//!    late join is always a seat fill); a new client then joins the running session:
//!    `RoomJoined` reports `finalized`, then every v3 member receives a fresh
//!    authoritative `SessionPlan` with antisymmetric pairing roles, and all 3
//!    live pairs connect with the full
//!    12-message channel matrix. The joiner's `--relay-payload` probe is
//!    armed on ENTRY (the GameStarting trigger pre-dates the join) and
//!    reaches both incumbents, while the joiner itself observes zero
//!    replayed traffic (no peer statuses, no pre-join GameData).
//! 5. `mixed_v2_v3_n3_relay_floor_with_reference_client` — two v3 members
//!    plus one pure-v2 member (on `/v2/ws`) in a mesh-PREFERRING room: the v2
//!    member forces the relay floor, each v3 member receives an explicit
//!    Relay/Relay empty `SessionPlan`, the v2 member receives none, no
//!    signaling/WebRTC activity occurs, and the GameData relay matrix completes.
//! 6. `two_peer_mesh_exchanges_over_live_webrtc` — the cross-platform smoke
//!    cell: the smallest complete session (two clients) establishes a direct
//!    host path and exchanges one message on both data channels. The remote
//!    side may be reported as peer-reflexive when rtc omits its dynamically
//!    learned registry entry; with no STUN or TURN configured, that remains a
//!    direct path. CI runs this exact selector on Linux, Windows, and macOS.
//! 7. `departed_connected_peer_keeps_unreliable_exchange_debt` — withholds the
//!    unreliable half after both clients prove reliable exchange, terminates
//!    one connected peer, and proves the survivor fails closed with the
//!    departed peer's logical exchange debt still named.
//! 8. `ipv6_only_mesh_pair_exchanges_on_a_host_ipv6_path` — the only cell whose
//!    live path is IPv6. webrtc 0.20 turns each application-supplied UDP bind
//!    directly into a host candidate, so the bound family decides the
//!    negotiated family; every other cell leaves `--ip-family any`, and a
//!    dual-family host then selects IPv4 (verified: the same scenario with
//!    `any` selects `127.0.0.1`), leaving the IPv6 bind/candidate/socket-key
//!    branch unexercised by any live transport. Two members run with
//!    `--ip-family ipv6`, so only IPv6 host candidates can be advertised, and
//!    the pair must be host/host over a concrete dialable IPv6 address with
//!    the exact exchange on both channel labels and no relay fallback.
//!    Signaling still runs over the harness server's IPv4 loopback listener,
//!    so the IPv6 claim is about the WebRTC data path (ICE, DTLS, SCTP). A
//!    runner without IPv6 loopback fails the cell; it is never skipped.
//!
//! Scenarios are serialized behind a mutex (each spawns 3+ OS processes and
//! up to three concurrent WebRTC stacks; running them in parallel on small CI
//! runners invites scheduling flakes for zero extra coverage). Ports are
//! reserved per scenario, so serialization is a robustness choice, not a
//! correctness requirement.

mod harness;

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, UdpSocket};
use std::time::Duration;

use harness::{
    advertised_candidate_addresses, advertised_candidates, event_tags, events_named, player_id_of,
    scenario_window, single_event, spawn_client, spawn_client_with_windows, spawn_server,
    str_field, ClientProcess, ClientSpec, CLIENT_EXIT_TIMEOUT, EVENT_TIMEOUT,
};
use serde_json::{json, Value};
use signal_fish_reference_native::engine::{local_udp_addrs, EngineSettings, IpFamily};
use uuid::Uuid;

/// Serializes the multi-process scenarios (see module docs).
static SCENARIO_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// Fully degraded ceiling for scenario 4's TWO client waves:
///  - server spawn: up to 3 attempts x 15 s health deadline each (see
///    `spawn_server`) = 45 s;
///  - first wave (incumbents + seat-holder): every process is hard-bounded
///    by its `--max-runtime-secs 90` watchdog (see `spawn_client`) = 90 s;
///  - joiner wave (spawned only after the pre-join gates, i.e. while the
///    first wave still lives): bounded by its own 90 s watchdog = 90 s.
const LATE_JOIN_SCENARIO_CEILING_SECS: u64 = 45 + 90 + 90;
/// Each of the six ordinary one-wave scenarios is bounded by server startup
/// plus one 90 s client watchdog; the departure regression uses 30 s clients.
const STANDARD_SCENARIO_CEILING_SECS: u64 = 45 + 90;
const DEPARTURE_SCENARIO_CEILING_SECS: u64 = 45 + 30;
/// A test may queue behind every other test. Bounding that wait by the entire
/// suite's real composition remains conservative while keeping a degraded run
/// within the workflow's 30-minute job policy (unlike multiplying the unique
/// two-wave ceiling by all eight scenarios).
const SERIAL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(
    LATE_JOIN_SCENARIO_CEILING_SECS
        + 6 * STANDARD_SCENARIO_CEILING_SECS
        + DEPARTURE_SCENARIO_CEILING_SECS,
);

const RELIABLE: &str = "reliable";
const UNRELIABLE: &str = "unreliable";
const CLIENT_NAMES: [&str; 3] = ["c0", "c1", "c2"];
const TWO_CLIENT_NAMES: [&str; 2] = ["c0", "c1"];
/// Scenario 8 only: bind IPv6 exclusively, so an IPv6 host candidate is the
/// only thing this client can advertise. (No `--disable-mdns`: rtc 0.20's
/// default multicast-DNS mode is query-only, so native host candidates are
/// raw addresses either way.)
const IPV6_ARGS: [&str; 2] = ["--ip-family", "ipv6"];

async fn acquire_serial() -> tokio::sync::MutexGuard<'static, ()> {
    tokio::time::timeout(SERIAL_ACQUIRE_TIMEOUT, SCENARIO_SERIAL.lock())
        .await
        .expect("acquire the scenario serialization lock in time")
}

/// One fully drained multi-client scenario, indexed in spawn order.
struct ScenarioRun {
    /// Player id strings (creator first).
    ids: Vec<String>,
    /// Complete stdout event logs, same indexing.
    logs: Vec<Vec<Value>>,
    /// Recent stdout events plus captured stderr, same indexing.
    diagnostics: Vec<String>,
    /// Reaped process exit codes, same indexing.
    exit_codes: Vec<i32>,
}

impl ScenarioRun {
    fn other_ids(&self, index: usize) -> BTreeSet<&str> {
        self.ids
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != index)
            .map(|(_, id)| id.as_str())
            .collect()
    }

    /// The client's pre-teardown event window (see [`scenario_window`]):
    /// scenario assertions exclude post-departure traffic such as the
    /// host-failover replans triggered by siblings exiting first.
    fn window(&self, index: usize) -> &[Value] {
        scenario_window(&self.logs[index])
    }

    fn diagnostics(&self, index: usize) -> &str {
        &self.diagnostics[index]
    }

    fn selected_pair_failure_context(&self, index: usize) -> String {
        let local = selected_pair_failure_context(
            TWO_CLIENT_NAMES[index],
            self.exit_codes[index],
            &self.logs[index],
            self.diagnostics(index),
        );
        if self.logs.len() != TWO_CLIENT_NAMES.len() {
            return local;
        }
        let peer_index = 1 - index;
        let peer = selected_pair_failure_context(
            TWO_CLIENT_NAMES[peer_index],
            self.exit_codes[peer_index],
            &self.logs[peer_index],
            self.diagnostics(peer_index),
        );
        format!("local evidence:\n{local}\npeer corroboration:\n{peer}")
    }
}

fn relay_payload_for(name: &str) -> String {
    format!("relay-from-{name}")
}

/// Per-client knobs for [`run_three_clients`]; everything else (names, the
/// creator/joiner split, run windows) follows the fixed 3-client pattern.
struct ClientConfig {
    exchange: bool,
    /// Send (and expect) the `relay-from-<name>` relay-floor payload.
    relay: bool,
    extra_args: &'static [&'static str],
    /// Connect to `/v2/ws` instead of `/v3/ws` (for pure-v2 members: a
    /// faithful v2 client would not know the v3 endpoint exists).
    v2_endpoint: bool,
}

impl ClientConfig {
    /// The scenario-1/2 default: full exchange + relay probe on `/v3/ws`.
    const fn full() -> Self {
        Self {
            exchange: true,
            relay: true,
            extra_args: &[],
            v2_endpoint: false,
        }
    }
}

/// Drain one client to EOF + reap it; it must exit 0 AND report that via its
/// final `exiting` event.
async fn drain_expect_success(client: &mut ClientProcess) -> i32 {
    let code = client.drain_to_exit(CLIENT_EXIT_TIMEOUT).await;
    assert_eq!(
        code,
        0,
        "client {} exited nonzero;\n{}",
        client.name,
        client.diagnostics()
    );
    let exiting = single_event(&client.events, "exiting", &client.name);
    assert_eq!(
        exiting.get("code").and_then(Value::as_i64),
        Some(0),
        "client {}: exiting event must report code 0",
        client.name
    );
    code
}

async fn await_event_count(
    client: &mut ClientProcess,
    event_name: &str,
    expected: usize,
    timeout: Duration,
) {
    while events_named(&client.events, event_name).len() < expected {
        client.await_event(event_name, timeout).await;
    }
}

/// Spawn the server with the given default topology, run the 3-client flow
/// (creator + 2 joiners by room code, per-client [`ClientConfig`]) to
/// completion, assert every process exits 0, and return the event logs.
async fn run_three_clients(
    default_topology: &str,
    game_name: &str,
    configs: &[ClientConfig; 3],
) -> ScenarioRun {
    let server = spawn_server(default_topology).await;
    // Holds the clients' captured stderr logs for failure diagnostics.
    let workdir = tempfile::tempdir().expect("create client workdir");
    let success_release_file = workdir.path().join("release-successful-clients");
    let success_release_path = success_release_file
        .to_str()
        .expect("temporary success-release path is UTF-8");

    let urls: Vec<String> = configs
        .iter()
        .map(|config| {
            if config.v2_endpoint {
                server.v2_ws_url()
            } else {
                server.v3_ws_url()
            }
        })
        .collect();
    let payloads: Vec<Option<String>> = CLIENT_NAMES
        .iter()
        .zip(configs)
        .map(|(name, config)| config.relay.then(|| relay_payload_for(name)))
        .collect();

    let creator_args: Vec<&str> = configs[0]
        .extra_args
        .iter()
        .copied()
        .chain(["--success-release-file", success_release_path])
        .collect();
    let mut creator = spawn_client(
        &ClientSpec {
            name: CLIENT_NAMES[0],
            server_url: &urls[0],
            game_name,
            join_code: None,
            peers: 3,
            exchange: configs[0].exchange,
            relay_payload: payloads[0].as_deref(),
            extra_args: &creator_args,
        },
        workdir.path(),
    );
    // The creator's stdout supplies the room code for the joiners.
    let created = creator.await_event("room_created", EVENT_TIMEOUT).await;
    let room_code = str_field(&created, "room_code").to_string();

    let mut clients: Vec<ClientProcess> = vec![creator];
    for (index, name) in CLIENT_NAMES.iter().enumerate().skip(1) {
        let client_args: Vec<&str> = configs[index]
            .extra_args
            .iter()
            .copied()
            .chain(["--success-release-file", success_release_path])
            .collect();
        clients.push(spawn_client(
            &ClientSpec {
                name,
                server_url: &urls[index],
                game_name,
                join_code: Some(&room_code),
                peers: 3,
                exchange: configs[index].exchange,
                relay_payload: payloads[index].as_deref(),
                extra_args: &client_args,
            },
            workdir.path(),
        ));
    }

    // A local SCTP send completion is not remote delivery. Hold all clients
    // until every process independently reports complete success criteria, so
    // sibling teardown cannot erase an outstanding exchange obligation.
    for client in &mut clients {
        client
            .await_event("success_criteria_met", CLIENT_EXIT_TIMEOUT)
            .await;
    }
    tokio::time::sleep(Duration::from_millis(350)).await;
    for client in &mut clients {
        client.assert_running("while held at the shared success barrier");
    }
    for client in &clients {
        assert!(
            events_named(&client.events, "player_left").is_empty(),
            "{} observed a departure before the success barrier;\n{}",
            client.name,
            client.diagnostics()
        );
    }
    std::fs::write(&success_release_file, b"release").expect("release successful clients together");

    // Drain everyone to EOF + reap; every client must succeed on its own and
    // report that through the exiting event.
    let mut exit_codes = Vec::with_capacity(clients.len());
    for client in &mut clients {
        exit_codes.push(drain_expect_success(client).await);
    }

    let ids: Vec<String> = clients
        .iter()
        .map(|client| player_id_of(&client.events, &client.name))
        .collect();
    let distinct: BTreeSet<&String> = ids.iter().collect();
    assert_eq!(distinct.len(), 3, "player ids must be distinct: {ids:?}");
    let diagnostics = clients.iter().map(ClientProcess::diagnostics).collect();

    ScenarioRun {
        ids,
        // `ClientProcess` implements `Drop`, so take the log instead of
        // moving the field out (the drained child is already reaped).
        logs: clients
            .into_iter()
            .map(|mut client| std::mem::take(&mut client.events))
            .collect(),
        diagnostics,
        exit_codes,
    }
}

/// The scenario-1/2 shape: all three clients with exchange + relay probes.
async fn run_three_client_scenario(default_topology: &str, game_name: &str) -> ScenarioRun {
    run_three_clients(
        default_topology,
        game_name,
        &[
            ClientConfig::full(),
            ClientConfig::full(),
            ClientConfig::full(),
        ],
    )
    .await
}

/// Run the smallest complete WebRTC mesh: two real client processes establish
/// one pair and exchange on both data channels. Keeping this independent of the
/// three-client matrix gives non-Linux CI a fast live-transport proof without
/// weakening the full Linux suite.
async fn run_two_peer_mesh(game_name: &str, extra_args: &'static [&'static str]) -> ScenarioRun {
    let mut server = spawn_server("mesh").await;
    let workdir = tempfile::tempdir().expect("create client workdir");
    let url = server.v3_ws_url();
    let success_release_file = workdir.path().join("release-successful-clients");
    let success_release_path = success_release_file
        .to_str()
        .expect("temporary success-release path is UTF-8");
    let client_args: Vec<&str> = extra_args
        .iter()
        .copied()
        .chain(["--success-release-file", success_release_path])
        .collect();

    let mut creator = spawn_client(
        &ClientSpec {
            name: TWO_CLIENT_NAMES[0],
            server_url: &url,
            game_name,
            join_code: None,
            peers: 2,
            exchange: true,
            relay_payload: None,
            extra_args: &client_args,
        },
        workdir.path(),
    );
    let created = creator.await_event("room_created", EVENT_TIMEOUT).await;
    let room_code = str_field(&created, "room_code").to_string();
    let joiner = spawn_client(
        &ClientSpec {
            name: TWO_CLIENT_NAMES[1],
            server_url: &url,
            game_name,
            join_code: Some(&room_code),
            peers: 2,
            exchange: true,
            relay_payload: None,
            extra_args: &client_args,
        },
        workdir.path(),
    );

    let mut clients = vec![creator, joiner];
    for client in &mut clients {
        client
            .await_event("success_criteria_met", CLIENT_EXIT_TIMEOUT)
            .await;
    }
    tokio::time::sleep(Duration::from_millis(350)).await;
    for client in &mut clients {
        client.assert_running("while held at the shared success barrier");
    }
    for client in &clients {
        assert!(
            events_named(&client.events, "player_left").is_empty(),
            "{} observed a departure before the success barrier;\n{}",
            client.name,
            client.diagnostics()
        );
    }
    std::fs::write(&success_release_file, b"release").expect("release successful clients together");
    let mut exit_codes = Vec::with_capacity(clients.len());
    for client in &mut clients {
        exit_codes.push(drain_expect_success(client).await);
    }
    server.shutdown().await;

    let ids: Vec<String> = clients
        .iter()
        .map(|client| player_id_of(&client.events, &client.name))
        .collect();
    assert_ne!(ids[0], ids[1], "the two clients need distinct identities");
    let diagnostics = clients.iter().map(ClientProcess::diagnostics).collect();

    ScenarioRun {
        ids,
        logs: clients
            .into_iter()
            .map(|mut client| std::mem::take(&mut client.events))
            .collect(),
        diagnostics,
        exit_codes,
    }
}

/// Assert the client's single `session_plan` event and return it.
fn session_plan<'a>(
    events: &'a [Value],
    who: &str,
    topology: &str,
    expected_peer_count: usize,
) -> &'a Value {
    let plan = single_event(events, "session_plan", who);
    assert_eq!(str_field(plan, "topology"), topology, "{who} plan topology");
    assert_eq!(
        str_field(plan, "transport"),
        "webrtc",
        "{who} plan transport"
    );
    assert_eq!(str_field(plan, "fallback"), "relay", "{who} plan fallback");
    assert_eq!(
        plan.get("ice_servers_count").and_then(Value::as_u64),
        Some(0),
        "{who}: the no-external-network server config must yield zero ICE servers"
    );
    let peers = plan_peers(plan, who);
    assert_eq!(
        peers.len(),
        expected_peer_count,
        "{who} plan must list exactly {expected_peer_count} peers: {plan}"
    );
    plan
}

/// Assert the universal v3 relay-floor plan: explicit and pairing-free.
fn relay_session_plan<'a>(events: &'a [Value], who: &str) -> &'a Value {
    let plan = single_event(events, "session_plan", who);
    assert_eq!(str_field(plan, "topology"), "relay", "{who} plan topology");
    assert_eq!(
        str_field(plan, "transport"),
        "relay",
        "{who} plan transport"
    );
    assert_eq!(str_field(plan, "fallback"), "relay", "{who} plan fallback");
    assert!(
        plan.get("host").is_some_and(Value::is_null),
        "{who}: relay plan must not name a host: {plan}"
    );
    assert!(
        plan_peers(plan, who).is_empty(),
        "{who}: relay plan must have no peers"
    );
    assert_eq!(
        plan.get("ice_servers_count").and_then(Value::as_u64),
        Some(0),
        "{who}: relay plan must not carry ICE servers"
    );
    plan
}

/// The `(player_id, initiate)` peer entries of a `session_plan` event.
fn plan_peers(plan: &Value, who: &str) -> Vec<(String, bool)> {
    plan.get("peers")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{who}: session_plan has no peers array: {plan}"))
        .iter()
        .map(|peer| {
            (
                str_field(peer, "player_id").to_string(),
                peer.get("initiate")
                    .and_then(Value::as_bool)
                    .unwrap_or_else(|| panic!("{who}: plan peer missing initiate: {peer}")),
            )
        })
        .collect()
}

/// Assert exactly one `p2p_pair_connected` per expected peer (and none else).
fn assert_pair_connected_exactly(events: &[Value], who: &str, expected_peers: &BTreeSet<&str>) {
    let events = events_named(events, "p2p_pair_connected");
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for event in &events {
        *seen.entry(str_field(event, "peer")).or_default() += 1;
    }
    let seen_peers: BTreeSet<&str> = seen.keys().copied().collect();
    assert_eq!(
        seen_peers, *expected_peers,
        "{who}: p2p_pair_connected peers mismatch"
    );
    for (peer, count) in seen {
        assert_eq!(count, 1, "{who}: duplicate p2p_pair_connected for {peer}");
    }
}

/// Assert the exchange receive matrix: exactly one `channel_message` per
/// (sender, label), with the documented `{"from","channel","seq"}` payload —
/// and no messages from anyone outside `senders`.
fn assert_exchange_received_from(events: &[Value], who: &str, senders: &BTreeSet<&str>) {
    let events = events_named(events, "channel_message");
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    for event in &events {
        let peer = str_field(event, "peer").to_string();
        let label = str_field(event, "label").to_string();
        assert!(
            senders.contains(peer.as_str()),
            "{who}: unexpected channel_message from non-pair peer {peer}: {event}"
        );
        let text: Value = serde_json::from_str(str_field(event, "text")).unwrap_or_else(|error| {
            panic!("{who}: channel_message text is not JSON ({error}): {event}")
        });
        assert_eq!(
            str_field(&text, "from"),
            peer,
            "{who}: exchange payload `from` must name the sending peer"
        );
        assert_eq!(
            str_field(&text, "channel"),
            label,
            "{who}: exchange payload `channel` must name its channel"
        );
        assert_eq!(
            text.get("seq").and_then(Value::as_u64),
            Some(0),
            "{who}: exchange payload seq must be 0"
        );
        *counts.entry((peer, label)).or_default() += 1;
    }
    for sender in senders {
        for label in [RELIABLE, UNRELIABLE] {
            assert_eq!(
                counts.get(&((*sender).to_string(), label.to_string())),
                Some(&1),
                "{who}: expected exactly one `{label}` message from {sender}; got {counts:?}"
            );
        }
    }
    assert_eq!(
        counts.len(),
        senders.len() * 2,
        "{who}: stray (peer,label) channel_message entries: {counts:?}"
    );
}

/// Assert exactly one `channel_message_sent` per (recipient, label).
fn assert_exchange_sent_to(events: &[Value], who: &str, recipients: &BTreeSet<&str>) {
    let events = events_named(events, "channel_message_sent");
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    for event in &events {
        let peer = str_field(event, "peer").to_string();
        assert!(
            recipients.contains(peer.as_str()),
            "{who}: unexpected channel_message_sent to non-pair peer {peer}: {event}"
        );
        *counts
            .entry((peer, str_field(event, "label").to_string()))
            .or_default() += 1;
    }
    for recipient in recipients {
        for label in [RELIABLE, UNRELIABLE] {
            assert_eq!(
                counts.get(&((*recipient).to_string(), label.to_string())),
                Some(&1),
                "{who}: expected exactly one `{label}` send to {recipient}; got {counts:?}"
            );
        }
    }
}

/// Assert that the overall WebRTC status initially resolves `true` and every
/// later report is a real state transition. A sibling may exit after the
/// exchange criteria are met, producing a legitimate `true -> false` report
/// before this process drains; teardown ordering is not part of the session
/// success contract.
fn assert_transport_status_true(full_log: &[Value], who: &str) {
    let connected = transport_status_sequence(full_log, who);
    assert_eq!(
        connected.first(),
        Some(&true),
        "{who}: overall webrtc status must initially resolve connected: {connected:?}"
    );
    assert!(
        connected.windows(2).all(|states| states[0] != states[1]),
        "{who}: transport reports must be deduplicated state transitions: {connected:?}"
    );
    assert_eq!(
        events_named(full_log, "fallback_engaged").len(),
        connected.iter().filter(|&&state| !state).count(),
        "{who}: every disconnected transition must engage the relay fallback"
    );
}

fn transport_status_sequence(full_log: &[Value], who: &str) -> Vec<bool> {
    let statuses = events_named(full_log, "transport_status_sent");
    assert!(!statuses.is_empty(), "{who}: no transport status was sent");
    statuses
        .iter()
        .map(|status| {
            assert_eq!(str_field(status, "transport"), "webrtc", "{who} transport");
            status
                .get("connected")
                .and_then(Value::as_bool)
                .unwrap_or_else(|| panic!("{who}: status lacks connected flag: {status}"))
        })
        .collect()
}

fn before_nth_event<'a>(events: &'a [Value], event_name: &str, occurrence: usize) -> &'a [Value] {
    assert!(occurrence > 0, "event occurrence is one-indexed");
    let mut seen = 0;
    let end = events
        .iter()
        .position(|event| {
            if event.get("event").and_then(Value::as_str) == Some(event_name) {
                seen += 1;
            }
            seen == occurrence
        })
        .unwrap_or(events.len());
    &events[..end]
}

/// Assert exactly one `peer_transport_status{webrtc, <flag>}` per expected
/// reporter (and none from anyone else).
fn assert_peer_status_fan_out(events: &[Value], who: &str, expected: &BTreeMap<&str, bool>) {
    let events = events_named(events, "peer_transport_status");
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for event in &events {
        assert_eq!(
            str_field(event, "transport"),
            "webrtc",
            "{who}: fan-out transport"
        );
        let peer = str_field(event, "peer");
        let Some(&connected) = expected.get(peer) else {
            panic!("{who}: unexpected status reporter {peer}: {event}");
        };
        assert_eq!(
            event.get("connected").and_then(Value::as_bool),
            Some(connected),
            "{who}: fan-out connected flag from {peer}"
        );
        *counts.entry(peer).or_default() += 1;
    }
    let seen: BTreeSet<&str> = counts.keys().copied().collect();
    let reporters: BTreeSet<&str> = expected.keys().copied().collect();
    assert_eq!(
        seen, reporters,
        "{who}: must hear exactly the expected members' status reports"
    );
    for (peer, count) in counts {
        assert_eq!(
            count, 1,
            "{who}: server dedup must fan out exactly one report from {peer}"
        );
    }
}

/// Assert the exact ordered connected-state sequence received from each peer.
fn assert_peer_status_sequences(events: &[Value], who: &str, expected: &BTreeMap<&str, Vec<bool>>) {
    let events = events_named(events, "peer_transport_status");
    let mut actual: BTreeMap<&str, Vec<bool>> = BTreeMap::new();
    for event in &events {
        assert_eq!(
            str_field(event, "transport"),
            "webrtc",
            "{who}: fan-out transport"
        );
        let peer = str_field(event, "peer");
        assert!(
            expected.contains_key(peer),
            "{who}: unexpected status reporter {peer}: {event}"
        );
        let connected = event
            .get("connected")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| panic!("{who}: status lacks connected flag: {event}"));
        actual.entry(peer).or_default().push(connected);
    }
    assert_eq!(actual, *expected, "{who}: unexpected peer status sequences");
}

/// Assert this client's relay-floor traffic: exactly one `game_data_sent`,
/// plus exactly one received `relay_msg` payload per expected sender id (and
/// none from anyone else).
fn assert_relay_floor_traffic(events: &[Value], who: &str, expected: &BTreeMap<&str, &str>) {
    single_event(events, "game_data_sent", who);
    let received = events_named(events, "game_data_received");
    let mut by_sender: BTreeMap<&str, &str> = BTreeMap::new();
    for event in &received {
        let from = str_field(event, "from");
        let payload = event
            .get("payload")
            .and_then(|payload| payload.get("relay_msg"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{who}: game_data_received without relay_msg: {event}"));
        let previous = by_sender.insert(from, payload);
        assert!(
            previous.is_none(),
            "{who}: duplicate relay payload from {from}"
        );
    }
    assert_eq!(
        by_sender, *expected,
        "{who}: relay payloads by sender must match the expectation exactly"
    );
}

/// Assert the relay floor stayed live: this client sent its payload and
/// received exactly the other members' distinct payloads over the WebSocket.
fn assert_live_relay_floor(run: &ScenarioRun, index: usize) {
    let who = CLIENT_NAMES[index];
    let expected_owned: Vec<(&str, String)> = run
        .ids
        .iter()
        .enumerate()
        .filter(|(other_index, _)| *other_index != index)
        .map(|(other_index, other_id)| {
            (
                other_id.as_str(),
                relay_payload_for(CLIENT_NAMES[other_index]),
            )
        })
        .collect();
    let expected: BTreeMap<&str, &str> = expected_owned
        .iter()
        .map(|(other_id, payload)| (*other_id, payload.as_str()))
        .collect();
    assert_relay_floor_traffic(run.window(index), who, &expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn mesh_n3_full_webrtc_session_with_live_relay_floor() {
    let _serial = acquire_serial().await;
    let run = run_three_client_scenario("mesh", "interop-mesh3").await;

    // Per-recipient plans + the GLOBAL glare matrix: union of
    // (recipient, peer, initiate) must be total and antisymmetric, with the
    // lexicographically smaller UUID initiating (the deterministic offerer rule).
    let mut matrix: BTreeMap<(String, String), bool> = BTreeMap::new();
    for (index, who) in CLIENT_NAMES.iter().enumerate() {
        let plan = session_plan(run.window(index), who, "mesh", 2);
        assert!(
            plan.get("host").is_some_and(Value::is_null),
            "mesh plans elect no host: {plan}"
        );
        let peers = plan_peers(plan, who);
        let peer_ids: BTreeSet<&str> = peers.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            peer_ids,
            run.other_ids(index),
            "{who}: plan must list exactly the other members"
        );
        for (peer_id, initiate) in peers {
            matrix.insert((run.ids[index].clone(), peer_id), initiate);
        }
    }
    let uuids: Vec<Uuid> = run
        .ids
        .iter()
        .map(|id| Uuid::parse_str(id).expect("player ids are UUIDs"))
        .collect();
    for a in 0..3 {
        for b in 0..3 {
            if uuids[a] >= uuids[b] {
                continue;
            }
            let smaller = &run.ids[a];
            let larger = &run.ids[b];
            assert_eq!(
                matrix.get(&(smaller.clone(), larger.clone())),
                Some(&true),
                "{smaller} (smaller UUID) must offer to {larger}"
            );
            assert_eq!(
                matrix.get(&(larger.clone(), smaller.clone())),
                Some(&false),
                "{larger} (larger UUID) must answer {smaller}, not offer"
            );
        }
    }
    assert_eq!(matrix.len(), 6, "glare matrix must cover all ordered pairs");

    for (index, who) in CLIENT_NAMES.iter().enumerate() {
        let others = run.other_ids(index);
        let others_true: BTreeMap<&str, bool> = others.iter().map(|peer| (*peer, true)).collect();
        // 2 connected-pair events per client = 6 directed endpoints total.
        assert_pair_connected_exactly(run.window(index), who, &others);
        // Channel matrix: both labels from both peers = 12 receive events total.
        assert_exchange_received_from(&run.logs[index], who, &others);
        assert_exchange_sent_to(&run.logs[index], who, &others);
        // Transport-fallback status + session-008 fan-out.
        assert_transport_status_true(&run.logs[index], who);
        assert_peer_status_fan_out(run.window(index), who, &others_true);
        // The relay floor carried GameData in the same session window as the
        // live pairs (no relay/pair ordering is asserted — empirically the
        // relay matrix usually completes BEFORE pairs connect; post-pair
        // WebSocket liveness is what the status fan-outs above prove).
        assert_live_relay_floor(&run, index);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn host_star_n3_webrtc() {
    let _serial = acquire_serial().await;
    let run = run_three_client_scenario("host", "interop-host3").await;

    // All plans agree on the same non-null host; with no designated authority
    // the server elects the earliest joiner — the creator.
    let host_id = run.ids[0].clone();
    let client_ids: BTreeSet<&str> = run.other_ids(0);

    // Host plan: exactly the two clients, none of which the host offers to
    // (clients offer to the host; the host answers all — the host offerer rule).
    let host_plan = session_plan(run.window(0), CLIENT_NAMES[0], "host", 2);
    assert_eq!(
        str_field(host_plan, "host"),
        host_id,
        "the host's own plan must name it as the elected host"
    );
    let host_peers = plan_peers(host_plan, CLIENT_NAMES[0]);
    let host_peer_ids: BTreeSet<&str> = host_peers.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(
        host_peer_ids, client_ids,
        "the host's plan must list exactly the clients"
    );
    for (peer_id, initiate) in &host_peers {
        assert!(
            !initiate,
            "host must not initiate toward {peer_id} (clients offer to the host)"
        );
    }

    // Client plans: exactly the host, with initiate=true.
    for (index, who) in CLIENT_NAMES.iter().enumerate().skip(1) {
        let plan = session_plan(run.window(index), who, "host", 1);
        assert_eq!(
            str_field(plan, "host"),
            host_id,
            "{who}: plan must name the elected host"
        );
        let peers = plan_peers(plan, who);
        assert_eq!(peers[0].0, host_id, "client peer must be the host");
        assert!(
            peers[0].1,
            "{who}: client offers to the host (fixed star direction)"
        );
    }

    // Pairs exist ONLY along star edges: host<->each client (4 directed
    // endpoints), never client<->client.
    assert_pair_connected_exactly(run.window(0), CLIENT_NAMES[0], &client_ids);
    for (index, who) in CLIENT_NAMES.iter().enumerate().skip(1) {
        let host_only: BTreeSet<&str> = BTreeSet::from([host_id.as_str()]);
        assert_pair_connected_exactly(run.window(index), who, &host_only);
        // Exchange completes host<->client in both directions on both
        // channels; the receive/send assertions also reject any
        // client<->client channel traffic.
        assert_exchange_received_from(&run.logs[index], who, &host_only);
        assert_exchange_sent_to(&run.logs[index], who, &host_only);
        assert_transport_status_true(&run.logs[index], who);
    }
    assert_exchange_received_from(&run.logs[0], CLIENT_NAMES[0], &client_ids);
    assert_exchange_sent_to(&run.logs[0], CLIENT_NAMES[0], &client_ids);
    assert_transport_status_true(&run.logs[0], CLIENT_NAMES[0]);

    // The relay floor stays live in a star too.
    for index in 0..3 {
        assert_live_relay_floor(&run, index);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mesh_n3_partial_ice_cripple_relay_fallback() {
    let _serial = acquire_serial().await;
    // All members run a SHORT P2P window: the crippled pairs can never
    // connect (the crippled endpoint registers no local candidate and applies
    // no remote candidate), so every member
    // resolves its overall status at this deadline — the healthy pair with
    // one connected pair (>= 1 rule => true), the crippled member with zero
    // (=> false + fallback). 6 s comfortably covers the healthy pair's
    // loopback establishment.
    const HEALTHY_ARGS: &[&str] = &["--p2p-timeout-secs", "6", "--p2p-retry-count", "0"];
    const CRIPPLED_ARGS: &[&str] = &[
        "--p2p-timeout-secs",
        "6",
        "--p2p-retry-count",
        "0",
        "--cripple-ice",
    ];
    let run = run_three_clients(
        "mesh",
        "interop-fallback3",
        &[
            ClientConfig {
                exchange: true,
                relay: true,
                extra_args: HEALTHY_ARGS,
                v2_endpoint: false,
            },
            ClientConfig {
                exchange: true,
                relay: true,
                extra_args: HEALTHY_ARGS,
                v2_endpoint: false,
            },
            // The crippled member registers no usable local candidates and
            // drops candidate signals both ways; no --exchange (its pairs
            // never open).
            ClientConfig {
                exchange: false,
                relay: true,
                extra_args: CRIPPLED_ARGS,
                v2_endpoint: false,
            },
        ],
    )
    .await;
    const CRIPPLED: usize = 2;

    // Finalization is unaffected by runtime ICE health: every member
    // advertises mesh+webrtc, so all three plans are normal mesh plans
    // listing the other two members.
    for (index, who) in CLIENT_NAMES.iter().enumerate() {
        let plan = session_plan(run.window(index), who, "mesh", 2);
        let peer_ids: BTreeSet<String> = plan_peers(plan, who)
            .into_iter()
            .map(|(id, _initiate)| id)
            .collect();
        let peer_ids: BTreeSet<&str> = peer_ids.iter().map(String::as_str).collect();
        assert_eq!(
            peer_ids,
            run.other_ids(index),
            "{who}: plan must list exactly the other members"
        );
    }

    // The healthy pair: exactly one connected pair each (the OTHER healthy
    // member, never the crippled one) and the full channel matrix between
    // them — 2 receive events per member, 4 across the pair.
    for (index, other) in [(0_usize, 1_usize), (1, 0)] {
        let who = CLIENT_NAMES[index];
        let other_only: BTreeSet<&str> = BTreeSet::from([run.ids[other].as_str()]);
        assert_pair_connected_exactly(run.window(index), who, &other_only);
        assert_exchange_received_from(&run.logs[index], who, &other_only);
        assert_exchange_sent_to(&run.logs[index], who, &other_only);
        // >= 1 pair connected at the deadline => overall webrtc status true.
        assert_transport_status_true(&run.logs[index], who);
    }

    // The crippled member: ZERO pairs and zero channel traffic, exactly one
    // overall {webrtc, false} report, and the explicit fallback marker.
    let crippled_log = &run.logs[CRIPPLED];
    let crippled_name = CLIENT_NAMES[CRIPPLED];
    for name in [
        "p2p_pair_connected",
        "channel_open",
        "channel_message",
        "channel_message_sent",
        "ice_candidate_dropped",
    ] {
        assert!(
            events_named(crippled_log, name).is_empty(),
            "{crippled_name}: crippled ICE must produce no `{name}` events"
        );
    }
    let status = single_event(crippled_log, "transport_status_sent", crippled_name);
    assert_eq!(
        str_field(status, "transport"),
        "webrtc",
        "{crippled_name}: status transport"
    );
    assert_eq!(
        status.get("connected").and_then(Value::as_bool),
        Some(false),
        "{crippled_name}: zero connected pairs must resolve connected=false"
    );
    assert_eq!(
        events_named(crippled_log, "fallback_engaged").len(),
        1,
        "{crippled_name}: a zero-pair resolution must engage the relay fallback exactly once"
    );

    // Asymmetric fan-out matrix: the healthy members observe the OTHER
    // healthy member's `true` AND the crippled member's `false`; the crippled
    // member observes two `true`s. Exactly one report per reporter (dedup).
    let crippled_id = run.ids[CRIPPLED].as_str();
    for (index, healthy_other) in [(0_usize, 1_usize), (1, 0)] {
        let expected: BTreeMap<&str, bool> = BTreeMap::from([
            (run.ids[healthy_other].as_str(), true),
            (crippled_id, false),
        ]);
        assert_peer_status_fan_out(run.window(index), CLIENT_NAMES[index], &expected);
    }
    let expected_for_crippled: BTreeMap<&str, bool> =
        BTreeMap::from([(run.ids[0].as_str(), true), (run.ids[1].as_str(), true)]);
    assert_peer_status_fan_out(run.window(CRIPPLED), crippled_name, &expected_for_crippled);

    // The relay floor serves EVERY member — the crippled one entirely: its
    // payload reached both healthy members and it received both of theirs.
    for index in 0..3 {
        assert_live_relay_floor(&run, index);
    }
}

/// Parse a player-id string as a UUID (for deterministic-offerer assertions).
fn uuid_of(id: &str) -> Uuid {
    Uuid::parse_str(id).unwrap_or_else(|error| panic!("player id {id} is not a UUID: {error}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn late_join_authoritative_replan_real_webrtc_n3() {
    let _serial = acquire_serial().await;
    let server = spawn_server("mesh").await;
    let server_url = server.v3_ws_url();
    let workdir = tempfile::tempdir().expect("create client workdir");
    let game_name = "interop-latejoin3";
    let success_release_file = workdir.path().join("release-late-join-clients");
    let success_release_path = success_release_file
        .to_str()
        .expect("temporary success-release path is UTF-8");
    assert!(
        !success_release_file.exists(),
        "success release path must start absent"
    );

    // The server only finalizes FULL rooms, so a late join is always a seat
    // fill: a 3-seat room finalizes with the two incumbents plus a
    // seat-holder that departs right after GameStarting (without ever
    // pairing), and the late joiner then fills the freed seat. The incumbents
    // gate their exit on having seen 4 distinct members (themselves, each
    // other, the seat-holder, and the joiner), so they cannot finish the
    // session before the joiner arrives.
    //
    // Everyone except the seat-holder runs a `--relay-payload` probe: the
    // incumbents send theirs at GameStarting (pre-join), the late joiner
    // sends on ENTRY into the finalized room (GameStarting pre-dates its join
    // and never re-fires — the late-join arming path under test). Receiving
    // the joiner's payload is part of the incumbents' exit criteria
    // (`--peers 3` => two distinct senders: the other incumbent + the
    // joiner), which makes its receipt deterministic, not merely likely.
    let incumbent_args = [
        "--expect-total-peers",
        "4",
        "--success-release-file",
        success_release_path,
    ];
    let joiner_args = ["--success-release-file", success_release_path];
    let inc_relays = [relay_payload_for("inc0"), relay_payload_for("inc1")];
    let joiner_relay = relay_payload_for("joiner");
    let mut inc0 = spawn_client(
        &ClientSpec {
            name: "inc0",
            server_url: &server_url,
            game_name,
            join_code: None,
            peers: 3,
            exchange: true,
            relay_payload: Some(&inc_relays[0]),
            extra_args: &incumbent_args,
        },
        workdir.path(),
    );
    let created = inc0.await_event("room_created", EVENT_TIMEOUT).await;
    let room_code = str_field(&created, "room_code").to_string();

    let mut inc1 = spawn_client(
        &ClientSpec {
            name: "inc1",
            server_url: &server_url,
            game_name,
            join_code: Some(&room_code),
            peers: 3,
            exchange: true,
            relay_payload: Some(&inc_relays[1]),
            extra_args: &incumbent_args,
        },
        workdir.path(),
    );
    let mut leaver = spawn_client(
        &ClientSpec {
            name: "leaver",
            server_url: &server_url,
            game_name,
            join_code: Some(&room_code),
            peers: 3,
            exchange: false,
            relay_payload: None,
            extra_args: &["--leave-on-game-start"],
        },
        workdir.path(),
    );

    // The seat-holder fills the room (finalizing it), observes GameStarting,
    // and exits cleanly without ever pairing.
    let leaver_code = leaver.drain_to_exit(CLIENT_EXIT_TIMEOUT).await;
    assert_eq!(
        leaver_code,
        0,
        "the seat-holder must exit 0;\n{}",
        leaver.diagnostics()
    );

    // Deterministic pre-join gate on BOTH incumbents. The events have no
    // fixed relative order (hence `await_event_or_recorded`):
    //  - `p2p_pair_connected`: the incumbent mesh pair is live;
    //  - `player_left`: the seat is free server-side (the broadcast is
    //    enqueued only after the removal is committed);
    //  - `transport_status_sent`: this incumbent's own report left the
    //    process. A client-side send alone is NOT a happens-before for
    //    server-side processing (it can still be in flight on another
    //    socket), hence the next two gates;
    //  - `peer_transport_status`: the server processed the OTHER incumbent's
    //    report and fanned it out over the current membership (the only
    //    possible pre-join reporter is the other incumbent — the seat-holder
    //    never reports, asserted below). Both incumbents observing this
    //    proves both reports were fully processed BEFORE the joiner exists,
    //    so the joiner observing ZERO peer_transport_status (asserted below)
    //    is deterministic;
    //  - `game_data_received`: likewise, the server processed the OTHER
    //    incumbent's relay payload (the only possible pre-join sender) and
    //    fanned it out, so the joiner can never receive incumbent GameData.
    for incumbent in [&mut inc0, &mut inc1] {
        for event_name in [
            "p2p_pair_connected",
            "player_left",
            "transport_status_sent",
            "peer_transport_status",
            "game_data_received",
        ] {
            incumbent
                .await_event_or_recorded(event_name, EVENT_TIMEOUT)
                .await;
        }
    }
    let pre_join_status_counts = [
        events_named(&inc0.events, "transport_status_sent").len(),
        events_named(&inc1.events, "transport_status_sent").len(),
    ];

    // The late joiner fills the freed seat of the running session. Its
    // `--relay-payload` exercises the late-join probe arming (and the waiver
    // of the relay-receive expectation: nothing pre-join is ever replayed).
    let mut joiner = spawn_client(
        &ClientSpec {
            name: "joiner",
            server_url: &server_url,
            game_name,
            join_code: Some(&room_code),
            peers: 3,
            exchange: true,
            relay_payload: Some(&joiner_relay),
            extra_args: &joiner_args,
        },
        workdir.path(),
    );

    // Hold all three live clients after their complete success criteria so no
    // sibling teardown can race the status-fan-out oracle below. Once every
    // process has reported the barrier, release them together; assertions use
    // each log's pre-PlayerLeft scenario window.
    for client in [&mut inc0, &mut inc1, &mut joiner] {
        client
            .await_event("success_criteria_met", CLIENT_EXIT_TIMEOUT)
            .await;
    }
    let held_incumbent_statuses = [
        transport_status_sequence(&inc0.events, "inc0"),
        transport_status_sequence(&inc1.events, "inc1"),
    ];
    let held_joiner_statuses = transport_status_sequence(&joiner.events, "joiner");
    for (who, statuses) in [
        ("inc0", &held_incumbent_statuses[0]),
        ("inc1", &held_incumbent_statuses[1]),
        ("joiner", &held_joiner_statuses),
    ] {
        assert_eq!(
            statuses.last(),
            Some(&true),
            "{who}: the held success barrier requires final connected status: {statuses:?}"
        );
    }
    await_event_count(
        &mut inc0,
        "peer_transport_status",
        held_incumbent_statuses[1].len() + held_joiner_statuses.len(),
        EVENT_TIMEOUT,
    )
    .await;
    await_event_count(
        &mut inc1,
        "peer_transport_status",
        held_incumbent_statuses[0].len() + held_joiner_statuses.len(),
        EVENT_TIMEOUT,
    )
    .await;
    await_event_count(
        &mut joiner,
        "peer_transport_status",
        held_incumbent_statuses[0].len() - pre_join_status_counts[0]
            + held_incumbent_statuses[1].len()
            - pre_join_status_counts[1],
        EVENT_TIMEOUT,
    )
    .await;
    assert_eq!(
        transport_status_sequence(&inc0.events, "inc0"),
        held_incumbent_statuses[0],
        "inc0: transport status changed while the success gate held"
    );
    assert_eq!(
        transport_status_sequence(&inc1.events, "inc1"),
        held_incumbent_statuses[1],
        "inc1: transport status changed while the success gate held"
    );
    assert_eq!(
        transport_status_sequence(&joiner.events, "joiner"),
        held_joiner_statuses,
        "joiner: transport status changed while the success gate held"
    );
    std::fs::write(&success_release_file, b"release")
        .expect("release late-join clients after every success barrier");
    for client in [&mut inc0, &mut inc1, &mut joiner] {
        drain_expect_success(client).await;
    }

    let inc_ids = [
        player_id_of(&inc0.events, "inc0"),
        player_id_of(&inc1.events, "inc1"),
    ];
    let leaver_id = player_id_of(&leaver.events, "leaver");
    let joiner_id = player_id_of(&joiner.events, "joiner");
    // The incumbents' first PlayerLeft is the deliberate pre-join seat-holder
    // departure; their second is live-session teardown. The late joiner never
    // saw the seat-holder, so its first PlayerLeft is teardown.
    let incumbent_windows = [
        before_nth_event(&inc0.events, "player_left", 2),
        before_nth_event(&inc1.events, "player_left", 2),
    ];
    let joiner_window = before_nth_event(&joiner.events, "player_left", 1);
    let incumbent_transport_statuses = [
        transport_status_sequence(incumbent_windows[0], "inc0"),
        transport_status_sequence(incumbent_windows[1], "inc1"),
    ];
    let joiner_transport_statuses = transport_status_sequence(joiner_window, "joiner");
    assert_eq!(
        incumbent_transport_statuses, held_incumbent_statuses,
        "incumbent transport status must stay stable through the pre-teardown window"
    );
    assert_eq!(
        joiner_transport_statuses, held_joiner_statuses,
        "joiner transport status must stay stable through the pre-teardown window"
    );
    let distinct: BTreeSet<&str> = [
        inc_ids[0].as_str(),
        inc_ids[1].as_str(),
        leaver_id.as_str(),
        joiner_id.as_str(),
    ]
    .into_iter()
    .collect();
    assert_eq!(distinct.len(), 4, "all four player ids must be distinct");

    // Seat-holder: it saw the start and authoritative plan, then produced ZERO
    // pairing traffic — no pairs, channels, outbound signals, or status report.
    single_event(&leaver.events, "game_starting", "leaver");
    session_plan(&leaver.events, "leaver", "mesh", 2);
    for name in [
        "p2p_pair_connected",
        "channel_open",
        "channel_message",
        "channel_message_sent",
        "signal_sent",
        "transport_status_sent",
        "fallback_engaged",
        "new_peer",
        // No --relay-payload: the seat-holder never sends GameData. (Whether
        // it RECEIVES the incumbents' payloads before departing is a race —
        // it exits at GameStarting while they send 250 ms later — so no
        // assertion is made about its game_data_received.)
        "game_data_sent",
    ] {
        assert!(
            events_named(&leaver.events, name).is_empty(),
            "leaver: a non-pairing seat-holder must emit no `{name}` events"
        );
    }

    // Joiner: RoomJoined reports the running session's Finalized state; the
    // GameStarting broadcast pre-dated the join and is never re-sent; the
    // pairing arrives as a fresh SessionPlan (never NewPeer) listing exactly
    // the two incumbents with UUID-glare initiate flags.
    let joined = single_event(&joiner.events, "room_joined", "joiner");
    assert_eq!(
        str_field(joined, "lobby_state"),
        "finalized",
        "the late joiner must observe the room's Finalized state on entry"
    );
    assert!(
        events_named(&joiner.events, "game_starting").is_empty(),
        "joiner: GameStarting is never re-broadcast for a late join"
    );
    assert!(
        events_named(&joiner.events, "new_peer").is_empty(),
        "joiner: the joiner's pairing is its SessionPlan, never NewPeer"
    );
    let joiner_plan = session_plan(&joiner.events, "joiner", "mesh", 2);
    let joiner_pairs: BTreeMap<String, bool> =
        plan_peers(joiner_plan, "joiner").into_iter().collect();
    let joiner_plan_ids: BTreeSet<&str> = joiner_pairs.keys().map(String::as_str).collect();
    let incumbent_ids: BTreeSet<&str> = inc_ids.iter().map(String::as_str).collect();
    assert_eq!(
        joiner_plan_ids, incumbent_ids,
        "joiner: the late-join plan lists exactly the two incumbents"
    );
    for (peer_id, initiate) in &joiner_pairs {
        assert_eq!(
            *initiate,
            uuid_of(&joiner_id) < uuid_of(peer_id),
            "joiner: initiate toward {peer_id} must follow the UUID glare rule"
        );
    }
    // Pre-join reports are never replayed. The generation-fenced authoritative
    // refresh does, however, rebuild each incumbent's retained pair and emits
    // at least one real false/true transition after the joiner is present.
    // The joiner must observe every reporter-local transition after the
    // pre-join initial `true`; this exact correlation permits legitimate
    // teardown ordering without hiding a dropped server fan-out.
    let mut joiner_statuses = BTreeMap::new();
    for (index, peer) in inc_ids.iter().map(String::as_str).enumerate() {
        let reporter_statuses = &incumbent_transport_statuses[index];
        assert!(
            reporter_statuses.len() >= pre_join_status_counts[index] + 2,
            "inc{index}: the retained pair rebuild must add false/true after the captured pre-join prefix: {reporter_statuses:?}"
        );
        let post_join = &reporter_statuses[pre_join_status_counts[index]..];
        assert_eq!(
            post_join.first(),
            Some(&false),
            "inc{index}: retained-pair replacement must first report disconnected: {post_join:?}"
        );
        assert_eq!(
            post_join.last(),
            Some(&true),
            "inc{index}: retained-pair replacement must converge connected: {post_join:?}"
        );
        joiner_statuses.insert(peer, post_join.to_vec());
    }
    assert_peer_status_sequences(joiner_window, "joiner", &joiner_statuses);
    assert_transport_status_true(joiner_window, "joiner");
    assert_pair_connected_exactly(&joiner.events, "joiner", &incumbent_ids);
    assert_exchange_received_from(&joiner.events, "joiner", &incumbent_ids);
    assert_exchange_sent_to(&joiner.events, "joiner", &incumbent_ids);
    // The late-join relay probe fired exactly once (armed on ENTRY — the
    // GameStarting trigger pre-dates the join and never re-fires), and the
    // joiner received NO GameData: the incumbents' payloads were fanned out
    // before the join (harness-gated above) and are never replayed — this
    // also exercises the client's late-join relay-receive waiver, without
    // which the joiner could never exit 0 here.
    single_event(&joiner.events, "game_data_sent", "joiner");
    assert!(
        events_named(&joiner.events, "game_data_received").is_empty(),
        "joiner: GameData fanned out before the join is never replayed"
    );

    // Incumbents: one finalize-time plan and one authoritative late-join
    // replacement plan (never an additive NewPeer), then pairs + the full
    // matrix with {other incumbent, joiner} only. The retained-pair rebuild
    // produces a local true/false/true sequence and the same post-join
    // false/true fan-out from the other incumbent. Each observer's expected
    // sequence is derived from the reporter's local sequence so legitimate
    // teardown transitions remain exact rather than being ignored.
    for (index, incumbent) in [(0_usize, &inc0), (1_usize, &inc1)] {
        let who = ["inc0", "inc1"][index];
        let my_id = inc_ids[index].as_str();
        let other_id = inc_ids[1 - index].as_str();

        let plans = events_named(&incumbent.events, "session_plan");
        assert_eq!(
            plans.len(),
            2,
            "{who}: finalize and late join each require one plan"
        );
        for plan in &plans {
            assert_eq!(str_field(plan, "topology"), "mesh", "{who} plan topology");
            assert_eq!(
                str_field(plan, "transport"),
                "webrtc",
                "{who} plan transport"
            );
            assert_eq!(str_field(plan, "fallback"), "relay", "{who} plan fallback");
            assert_eq!(
                plan_peers(plan, who).len(),
                2,
                "{who}: each plan has two peers"
            );
        }
        let initial_plan_ids: BTreeSet<String> = plan_peers(plans[0], who)
            .into_iter()
            .map(|(id, _initiate)| id)
            .collect();
        let initial_plan_ids: BTreeSet<&str> =
            initial_plan_ids.iter().map(String::as_str).collect();
        assert_eq!(
            initial_plan_ids,
            BTreeSet::from([other_id, leaver_id.as_str()]),
            "{who}: the finalize-time plan lists the other incumbent and the seat-holder"
        );

        assert!(
            events_named(&incumbent.events, "new_peer").is_empty(),
            "{who}: authoritative plans replace additive NewPeer deltas"
        );
        let refreshed_pairs: BTreeMap<String, bool> =
            plan_peers(plans[1], who).into_iter().collect();
        let refreshed_ids: BTreeSet<&str> = refreshed_pairs.keys().map(String::as_str).collect();
        assert_eq!(
            refreshed_ids,
            BTreeSet::from([other_id, joiner_id.as_str()]),
            "{who}: refreshed plan must replace the departed seat-holder with the joiner"
        );
        let initiate_joiner = refreshed_pairs[&joiner_id];
        assert_eq!(
            initiate_joiner,
            uuid_of(my_id) < uuid_of(&joiner_id),
            "{who}: refreshed plan must follow the UUID glare rule"
        );
        assert_eq!(
            initiate_joiner, !joiner_pairs[my_id],
            "{who}: refreshed plan must be antisymmetric to the joiner's flag"
        );

        let live_pairs: BTreeSet<&str> = BTreeSet::from([other_id, joiner_id.as_str()]);
        assert_pair_connected_exactly(&incumbent.events, who, &live_pairs);
        assert_exchange_received_from(&incumbent.events, who, &live_pairs);
        assert_exchange_sent_to(&incumbent.events, who, &live_pairs);
        assert_transport_status_true(incumbent_windows[index], who);
        let expected_reports: BTreeMap<&str, Vec<bool>> = BTreeMap::from([
            (other_id, incumbent_transport_statuses[1 - index].clone()),
            (joiner_id.as_str(), joiner_transport_statuses.clone()),
        ]);
        assert_peer_status_sequences(incumbent_windows[index], who, &expected_reports);

        // Relay floor across the join boundary: one send, and exactly the
        // other incumbent's payload (pre-join) plus the late joiner's
        // (post-join — its receipt is part of this incumbent's exit
        // criteria, so asserting it is deterministic, and it proves the
        // late-join probe's payload actually reached the running session).
        let expected_relay: BTreeMap<&str, &str> = BTreeMap::from([
            (other_id, inc_relays[1 - index].as_str()),
            (joiner_id.as_str(), joiner_relay.as_str()),
        ]);
        assert_relay_floor_traffic(&incumbent.events, who, &expected_relay);

        // The first departure each incumbent observes is the seat-holder's.
        let departures = events_named(&incumbent.events, "player_left");
        assert!(
            !departures.is_empty(),
            "{who}: must observe the seat-holder's departure"
        );
        assert_eq!(
            str_field(departures[0], "player_id"),
            leaver_id,
            "{who}: the first departure must be the seat-holder"
        );
    }

    // The full 12-message channel matrix across the 3 live session members:
    // each received one message per label from each of its 2 pairs.
    let total_received: usize = [&inc0.events, &inc1.events, &joiner.events]
        .into_iter()
        .map(|events| events_named(events, "channel_message").len())
        .sum();
    assert_eq!(
        total_received, 12,
        "3 members x 2 pairs x 2 channels = 12 channel_message receive events"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mixed_v2_v3_n3_relay_floor_with_reference_client() {
    let _serial = acquire_serial().await;
    // The server PREFERS mesh, but the upgrade ladder requires every member
    // to be v3-capable for any non-relay rung — the single pure-v2 member
    // (connected to the legacy `/v2/ws` endpoint, advertising no v3 fields)
    // forces the universal relay floor for the whole room.
    let run = run_three_clients(
        "mesh",
        "interop-mixed3",
        &[
            ClientConfig {
                exchange: false,
                relay: true,
                extra_args: &[],
                v2_endpoint: false,
            },
            ClientConfig {
                exchange: false,
                relay: true,
                extra_args: &["--protocol-version", "2"],
                v2_endpoint: true,
            },
            ClientConfig {
                exchange: false,
                relay: true,
                extra_args: &[],
                v2_endpoint: false,
            },
        ],
    )
    .await;
    const V2_MEMBER: usize = 1;

    for (index, who) in CLIENT_NAMES.iter().enumerate() {
        let full_log = &run.logs[index];

        // Negotiation result: the v2 member reports 2, the v3 members 3.
        let info = single_event(full_log, "protocol_info", who);
        let expected_version = if index == V2_MEMBER { 2 } else { 3 };
        assert_eq!(
            info.get("negotiated_version").and_then(Value::as_u64),
            Some(expected_version),
            "{who}: negotiated protocol version"
        );

        // Finalization is explicit for v3 even on the relay floor: v3 members
        // receive one authoritative Relay/Relay empty plan; v2 remains frozen.
        if index == V2_MEMBER {
            assert!(
                events_named(full_log, "session_plan").is_empty(),
                "{who}: v2 must never observe SessionPlan"
            );
        } else {
            relay_session_plan(full_log, who);
        }

        // No pairing or WebRTC activity ever happens — assert zero across the
        // FULL logs (not just the scenario window).
        for name in [
            "new_peer",
            "signal_sent",
            "signal_received",
            "p2p_pair_connected",
            "pc_state",
            "channel_open",
            "channel_message",
            "channel_message_sent",
            "transport_status_sent",
            "peer_transport_status",
            "fallback_engaged",
        ] {
            assert!(
                events_named(full_log, name).is_empty(),
                "{who}: a relay-floor room must produce no `{name}` events"
            );
        }

        // The v2-visible flow is intact: everyone observes GameStarting once
        // and the relay GameData matrix completes (each member's payload
        // reached both others over the WebSocket).
        single_event(full_log, "game_starting", who);
        assert_live_relay_floor(&run, index);
    }
}

/// Fail loudly (never skip) when the runner cannot serve the IPv6 ICE binds
/// this scenario needs.
///
/// The precondition applies the client's OWN selection rule rather than an
/// approximation of it, so "the harness says yes but the client says no" is
/// impossible; it then binds one of the resulting addresses to prove the
/// socket layer agrees with the interface table. Scenario 8's whole claim is
/// that a live path ran over IPv6, so a silent skip would report a green lane
/// that proved nothing.
fn require_ipv6_ice_interface() {
    let settings = EngineSettings {
        ip_family: IpFamily::Ipv6,
        ..EngineSettings::default()
    };
    let addrs = local_udp_addrs(settings).unwrap_or_else(|error| {
        panic!(
            "this scenario proves the IPv6 WebRTC data path and requires a usable IPv6 \
             interface, but the client's own selection rule found none: {error:#}. Enable \
             IPv6 on the runner (Linux: `sysctl net.ipv6.conf.lo.disable_ipv6=0`; \
             containers: run with IPv6 enabled). The lane must not be skipped silently."
        )
    });
    assert!(
        !addrs.is_empty(),
        "a successful selection is never empty; the client would have nothing to bind"
    );
    // Every one of them, not just the first: the client hands the whole vector
    // to the peer-connection builder, so an address that is enumerated but not
    // yet bindable — an IPv6 address still in duplicate-address detection, for
    // instance — would otherwise fail deep inside the child process with a
    // generic error, the very outcome this precondition exists to replace.
    for addr in &addrs {
        UdpSocket::bind(*addr).unwrap_or_else(|error| {
            panic!(
                "the client would bind {addr} for ICE, but this process cannot: \
                 {error}. The lane must not be skipped silently."
            )
        });
    }
}

/// The IP address a `selected_candidate_pair` field carries, or a loud failure.
/// A missing/`null` address fails: it would otherwise let a run that never
/// proved a concrete live path pass.
fn selected_ip_address(event: &Value, field: &str, who: &str, failure_context: &str) -> IpAddr {
    let raw = event.get(field).and_then(Value::as_str).unwrap_or_else(|| {
        panic!("{who}: selected pair has no `{field}`: {event};\n{failure_context}")
    });
    raw.parse::<IpAddr>().unwrap_or_else(|error| {
        panic!("{who}: `{field}` is not an IP address ({error}): {raw};\n{failure_context}")
    })
}

fn selected_pair_is_exact_host_host(selected: &Value) -> bool {
    ["local_candidate_type", "remote_candidate_type"]
        .into_iter()
        .all(|field| selected.get(field).and_then(Value::as_str) == Some("host"))
}

/// Assert this client's single selected pair is a direct host/host path toward
/// the expected peer, and return its concrete local and remote addresses.
fn assert_direct_host_path(
    events: &[Value],
    who: &str,
    peer_id: &str,
    failure_context: &str,
) -> [IpAddr; 2] {
    let selected = events_named(events, "selected_candidate_pair");
    assert_eq!(
        selected.len(),
        1,
        "{who}: expected exactly one `selected_candidate_pair` event, got {}: {selected:?}\n\
         {failure_context}",
        selected.len(),
    );
    let selected = selected[0];
    assert_eq!(
        str_field(selected, "peer"),
        peer_id,
        "{who}: the selected pair must belong to the planned peer"
    );
    assert!(
        selected_pair_is_exact_host_host(selected),
        "{who}: family-sensitive paths require exact host/host evidence: {selected};\n\
         {failure_context}"
    );
    let addresses = [
        selected_ip_address(selected, "local_candidate_address", who, failure_context),
        selected_ip_address(selected, "remote_candidate_address", who, failure_context),
    ];
    for address in addresses {
        assert!(
            !address.is_unspecified() && !address.is_multicast(),
            "{who}: the selected pair must use concrete unicast addresses, got {address}: {selected}"
        );
    }
    addresses
}

/// Assert the cross-platform, no-STUN/no-TURN smoke cell selected a direct
/// path. rtc can learn the remote host socket from an inbound connectivity
/// check before its signaled candidate is registered, exposing that side as
/// `prflx` with no address in rtc 0.20's public statistics. That shape is safe
/// only for this baseline: both processes prove they advertised host-only
/// candidates, and the cell configures neither STUN nor TURN. IPv6 and TURN
/// assertions retain their exact address/type requirements.
fn assert_baseline_direct_path(
    events: &[Value],
    peer_events: &[Value],
    who: &str,
    peer_id: &str,
    failure_context: &str,
) {
    validate_baseline_direct_path(events, peer_events, peer_id)
        .unwrap_or_else(|error| panic!("{who}: {error};\n{failure_context}"));
}

fn parse_concrete_unicast(raw: &str, field: &str) -> Result<IpAddr, String> {
    let address = raw
        .parse::<IpAddr>()
        .map_err(|error| format!("`{field}` is not an IP address ({error}): {raw}"))?;
    if address.is_unspecified() || address.is_multicast() {
        return Err(format!(
            "`{field}` must be a concrete unicast address, got {address}"
        ));
    }
    Ok(address)
}

fn validate_baseline_direct_path(
    events: &[Value],
    peer_events: &[Value],
    peer_id: &str,
) -> Result<(), String> {
    for (candidate_owner, candidate_events) in [("local", events), ("peer", peer_events)] {
        let advertised = events_named(candidate_events, "local_candidate");
        if advertised.is_empty() {
            return Err(format!(
                "{candidate_owner} advertised no candidate in the direct-path baseline"
            ));
        }
        for candidate in advertised {
            if candidate.get("candidate_type").and_then(Value::as_str) != Some("host") {
                return Err(format!(
                    "{candidate_owner} advertised a non-host candidate even though the \
                     baseline configures no STUN/TURN: {candidate}"
                ));
            }
            if candidate.get("protocol").and_then(Value::as_str) != Some("udp") {
                return Err(format!(
                    "{candidate_owner} advertised a non-UDP candidate in the UDP-only \
                     native baseline: {candidate}"
                ));
            }
            let address = candidate
                .get("address")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    format!("{candidate_owner} candidate has no address: {candidate}")
                })?;
            parse_concrete_unicast(address, "advertised candidate address")?;
        }
    }

    let selected = events_named(events, "selected_candidate_pair");
    if selected.len() != 1 {
        return Err(format!(
            "expected exactly one `selected_candidate_pair` event, got {}: {selected:?}",
            selected.len()
        ));
    }
    let selected = selected[0];
    if selected.get("peer").and_then(Value::as_str) != Some(peer_id) {
        return Err(format!(
            "the selected pair must belong to planned peer {peer_id}: {selected}"
        ));
    }
    if selected.get("local_candidate_type").and_then(Value::as_str) != Some("host") {
        return Err(format!(
            "the local side must be a host candidate: {selected}"
        ));
    }
    let remote_type = selected
        .get("remote_candidate_type")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("selected pair has no remote candidate type: {selected}"))?;
    if !matches!(remote_type, "host" | "prflx") {
        return Err(format!(
            "the no-STUN/no-TURN baseline requires a remote host or peer-reflexive \
             candidate, got {remote_type}: {selected}"
        ));
    }

    let local_address = selected
        .get("local_candidate_address")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("selected pair has no local address: {selected}"))?;
    parse_concrete_unicast(local_address, "local_candidate_address")?;
    match selected
        .get("remote_candidate_address")
        .and_then(Value::as_str)
    {
        Some(raw) => {
            parse_concrete_unicast(raw, "remote_candidate_address")?;
        }
        None if remote_type == "prflx" => {}
        None => {
            return Err(format!(
                "only rtc's peer-reflexive missing-registry shape may omit the remote \
                 address: {selected}"
            ));
        }
    }
    Ok(())
}

fn selected_pair_failure_context(
    who: &str,
    exit_code: i32,
    events: &[Value],
    diagnostics: &str,
) -> String {
    format!(
        "client {who} exit code: {exit_code}\n\
         emitted {} event(s): {}\n\
         selected candidate pairs: {:?}\n\
         advertised local ICE candidates: {:?}\n\
         {diagnostics}",
        events.len(),
        event_tags(events),
        events_named(events, "selected_candidate_pair"),
        advertised_candidates(events),
    )
}

#[test]
fn missing_selected_pair_reports_fully_drained_process_and_ice_evidence() {
    let events = vec![
        json!({
            "event": "local_candidate",
            "candidate_type": "host",
            "address": "192.0.2.7",
            "port": 49152
        }),
        json!({
            "event": "player_left"
        }),
        json!({
            "event": "exiting",
            "code": 0
        }),
    ];
    let run = ScenarioRun {
        ids: Vec::new(),
        logs: vec![events],
        diagnostics: vec!["stderr sentinel".to_owned()],
        exit_codes: vec![0],
    };
    let context = run.selected_pair_failure_context(0);

    assert!(context.contains("client c0 exit code: 0"), "{context}");
    assert!(
        context.contains("emitted 3 event(s): local_candidate, player_left, exiting"),
        "{context}"
    );
    assert!(
        context.contains("advertised local ICE candidates: [\"host 192.0.2.7:49152\"]"),
        "{context}"
    );
    assert!(context.contains("stderr sentinel"), "{context}");

    let failure = std::panic::catch_unwind(|| {
        assert_direct_host_path(run.window(0), "c0", "peer-id", &context);
    })
    .expect_err("the missing selected pair must fail its real assertion path");
    let message = failure
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| failure.downcast_ref::<&str>().copied())
        .expect("assertion panic must carry a string message");
    for required in [
        "expected exactly one `selected_candidate_pair` event, got 0",
        "client c0 exit code: 0",
        "local_candidate, player_left, exiting",
        "advertised local ICE candidates: [\"host 192.0.2.7:49152\"]",
        "stderr sentinel",
    ] {
        assert!(
            message.contains(required),
            "missing `{required}` in:\n{message}"
        );
    }
}

#[test]
fn peer_reflexive_shape_is_accepted_only_by_baseline_direct_oracle() {
    let local_events = vec![
        json!({
            "event": "local_candidate",
            "candidate_type": "host",
            "address": "192.0.2.7",
            "port": 49152,
            "protocol": "udp"
        }),
        json!({
            "event": "selected_candidate_pair",
            "peer": "peer-id",
            "local_candidate_type": "host",
            "remote_candidate_type": "prflx",
            "local_candidate_address": "192.0.2.7",
            "remote_candidate_address": null
        }),
    ];
    let peer_events = vec![json!({
        "event": "local_candidate",
        "candidate_type": "host",
        "address": "192.0.2.8",
        "port": 49153,
        "protocol": "udp"
    })];

    assert_baseline_direct_path(
        &local_events,
        &peer_events,
        "c0",
        "peer-id",
        "deterministic issue #337 shape",
    );

    assert!(
        !selected_pair_is_exact_host_host(&local_events[1]),
        "the family-sensitive host/host oracle must reject missing remote evidence"
    );
}

#[test]
fn baseline_direct_oracle_rejects_non_direct_or_incomplete_evidence() {
    let candidate = || {
        json!({
            "event": "local_candidate",
            "candidate_type": "host",
            "address": "192.0.2.7",
            "port": 49152,
            "protocol": "udp"
        })
    };
    let selected = |local_type: &str, remote_type: &str, remote_address: Value| {
        json!({
            "event": "selected_candidate_pair",
            "peer": "peer-id",
            "local_candidate_type": local_type,
            "remote_candidate_type": remote_type,
            "local_candidate_address": "192.0.2.7",
            "remote_candidate_address": remote_address
        })
    };
    let valid_local = vec![candidate(), selected("host", "host", json!("192.0.2.8"))];
    let valid_peer = vec![candidate()];
    assert!(validate_baseline_direct_path(&valid_local, &valid_peer, "peer-id").is_ok());

    let cases = [
        (
            "remote server-reflexive",
            vec![candidate(), selected("host", "srflx", json!("192.0.2.8"))],
            valid_peer.clone(),
        ),
        (
            "remote relay",
            vec![candidate(), selected("host", "relay", json!("192.0.2.8"))],
            valid_peer.clone(),
        ),
        (
            "local non-host",
            vec![candidate(), selected("srflx", "host", json!("192.0.2.8"))],
            valid_peer.clone(),
        ),
        ("peer advertised nothing", valid_local.clone(), Vec::new()),
        (
            "peer advertised relay",
            valid_local.clone(),
            vec![json!({
                "event": "local_candidate",
                "candidate_type": "relay",
                "address": "192.0.2.8",
                "port": 49153,
                "protocol": "udp"
            })],
        ),
        (
            "peer advertised non-UDP host",
            valid_local.clone(),
            vec![json!({
                "event": "local_candidate",
                "candidate_type": "host",
                "address": "192.0.2.8",
                "port": 49153,
                "protocol": "tcp"
            })],
        ),
        (
            "host omitted remote address",
            vec![candidate(), selected("host", "host", Value::Null)],
            valid_peer.clone(),
        ),
    ];
    for (name, local, peer) in cases {
        assert!(
            validate_baseline_direct_path(&local, &peer, "peer-id").is_err(),
            "{name} must fail closed"
        );
    }
}

/// Assert this client's single selected pair is host/host over concrete IPv6,
/// toward the expected peer.
///
/// The address is not pinned to `::1`: a runner that also has a global IPv6
/// interface binds it too, and either host candidate is a legitimate IPv6
/// proof. What must hold is that the path is IPv6, direct (host/host, since no
/// STUN or TURN is configured), and carried by an address a peer could actually
/// dial — never unspecified, multicast, or link-local, whose scope ID the
/// candidate wire cannot carry.
fn assert_ipv6_host_path(events: &[Value], who: &str, peer_id: &str, failure_context: &str) {
    for address in assert_direct_host_path(events, who, peer_id, failure_context) {
        let IpAddr::V6(address) = address else {
            panic!("{who}: the IPv6-only run selected an IPv4 candidate {address}");
        };
        assert!(
            !address.is_unspecified()
                && !address.is_multicast()
                && !address.is_unicast_link_local(),
            "{who}: the selected pair must use concrete dialable IPv6 addresses, got {address}"
        );
    }
}

/// Assert the *advertised* candidate set carries no IPv4 entry.
///
/// The selected pair alone cannot prove `--ip-family`: on a dual-stack host a
/// regression that made the flag a no-op would still let ICE choose IPv6, and
/// the cell would pass while proving nothing (issue #275, gap 2). What the
/// flag actually controls is the *bind set*, and the advertised candidates are
/// its observable image — one candidate per bound socket under the zero-STUN,
/// zero-TURN configuration this scenario runs.
fn assert_no_ipv4_candidate_advertised(events: &[Value], who: &str) {
    let advertised = advertised_candidate_addresses(events, who);
    assert!(
        !advertised.is_empty(),
        "{who}: the run advertised no local candidate, so there is nothing to \
         prove the family of"
    );
    let ipv4: Vec<&IpAddr> = advertised
        .iter()
        .filter(|address| address.is_ipv4())
        .collect();
    assert!(
        ipv4.is_empty(),
        "{who}: --ip-family ipv6 must bind no IPv4 socket, but the run advertised \
         {ipv4:?} among {advertised:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn two_peer_mesh_exchanges_over_live_webrtc() {
    let _serial = acquire_serial().await;
    let run = run_two_peer_mesh("interop-cross-platform-smoke", &[]).await;

    for (index, who) in TWO_CLIENT_NAMES.iter().enumerate() {
        let peer_id = run.ids[1 - index].as_str();
        let peers = BTreeSet::from([peer_id]);
        let window = run.window(index);
        let plan = session_plan(window, who, "mesh", 1);
        assert!(
            plan.get("host").is_some_and(Value::is_null),
            "{who}: mesh plans elect no host: {plan}"
        );

        assert_pair_connected_exactly(window, who, &peers);
        let failure_context = run.selected_pair_failure_context(index);
        assert_baseline_direct_path(
            window,
            run.window(1 - index),
            who,
            peer_id,
            &failure_context,
        );
        assert_exchange_sent_to(&run.logs[index], who, &peers);
        assert_exchange_received_from(&run.logs[index], who, &peers);
        assert_transport_status_true(&run.logs[index], who);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn departed_connected_peer_keeps_unreliable_exchange_debt() {
    let _serial = acquire_serial().await;
    let mut server = spawn_server("mesh").await;
    let workdir = tempfile::tempdir().expect("create client workdir");
    let exchange_release_file = workdir.path().join("release-reliable-exchange");
    let unreliable_release_file = workdir.path().join("hold-unreliable-exchange");
    std::fs::write(&exchange_release_file, b"release").expect("pre-release the reliable exchange");
    let exchange_release_path = exchange_release_file
        .to_str()
        .expect("temporary exchange-release path is UTF-8");
    let unreliable_release_path = unreliable_release_file
        .to_str()
        .expect("temporary unreliable-release path is UTF-8");
    let args = [
        "--exchange-release-file",
        exchange_release_path,
        "--unreliable-exchange-release-file",
        unreliable_release_path,
        "--p2p-timeout-secs",
        "5",
    ];
    let url = server.v3_ws_url();

    let mut survivor = spawn_client_with_windows(
        &ClientSpec {
            name: TWO_CLIENT_NAMES[0],
            server_url: &url,
            game_name: "interop-departed-exchange-debt",
            join_code: None,
            peers: 2,
            exchange: true,
            relay_payload: None,
            extra_args: &args,
        },
        workdir.path(),
        15,
        30,
    );
    let created = survivor.await_event("room_created", EVENT_TIMEOUT).await;
    let room_code = str_field(&created, "room_code").to_string();
    let mut departing = spawn_client_with_windows(
        &ClientSpec {
            name: TWO_CLIENT_NAMES[1],
            server_url: &url,
            game_name: "interop-departed-exchange-debt",
            join_code: Some(&room_code),
            peers: 2,
            exchange: true,
            relay_payload: None,
            extra_args: &args,
        },
        workdir.path(),
        15,
        30,
    );

    survivor
        .await_event("exchange_reliable_ready", CLIENT_EXIT_TIMEOUT)
        .await;
    departing
        .await_event("exchange_reliable_ready", CLIENT_EXIT_TIMEOUT)
        .await;
    let departed_id = player_id_of(&departing.events, &departing.name);
    departing.terminate().await;
    survivor.await_event("player_left", EVENT_TIMEOUT).await;

    let exit_code = survivor.drain_to_exit(Duration::from_secs(15)).await;
    assert_eq!(
        exit_code,
        1,
        "the survivor must fail closed when a connected peer departs before unreliable exchange;\n{}",
        survivor.diagnostics()
    );
    let error = single_event(&survivor.events, "error", &survivor.name);
    let message = str_field(error, "message");
    assert!(
        message.contains("exchange incomplete") && message.contains(&departed_id),
        "the failure must retain the departed peer's exchange debt, got {message:?};\n{}",
        survivor.diagnostics()
    );
    let exiting = single_event(&survivor.events, "exiting", &survivor.name);
    assert_eq!(exiting.get("code").and_then(Value::as_i64), Some(1));
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ipv6_only_mesh_pair_exchanges_on_a_host_ipv6_path() {
    let _serial = acquire_serial().await;
    require_ipv6_ice_interface();
    let run = run_two_peer_mesh("interop-ipv6", &IPV6_ARGS).await;

    for (index, who) in TWO_CLIENT_NAMES.iter().enumerate() {
        let peer_id = run.ids[1 - index].as_str();
        let peers = BTreeSet::from([peer_id]);
        let window = run.window(index);

        // A live WebRTC pair carried the session, not the relay floor.
        assert_pair_connected_exactly(window, who, &peers);
        assert_transport_status_true(&run.logs[index], who);
        // Bound the fallback claim to the LIVE session. A sibling that exits
        // first legitimately drives this client's status to `false` plus one
        // `fallback_engaged` — and the server emits that transition BEFORE
        // `PlayerLeft`, so `scenario_window` does not exclude it (reproduced
        // 4 times in 12 single-core runs). What must hold is that the relay
        // floor never carried the exchange.
        let live_end = run.logs[index]
            .iter()
            .rposition(|event| {
                event.get("event").and_then(Value::as_str) == Some("channel_message")
            })
            .unwrap_or_else(|| {
                panic!(
                    "{who}: the exchange produced no channel messages;\n{}",
                    run.diagnostics(index)
                )
            })
            + 1;
        assert!(
            events_named(&run.logs[index][..live_end], "fallback_engaged").is_empty(),
            "{who}: the IPv6 host path must carry the exchange, not the relay fallback;\n{}",
            run.diagnostics(index)
        );
        // Teardown produces at most one such transition, so this still catches
        // a path that collapses right after the exchange (or flaps) without
        // re-introducing the race the bound above avoids.
        assert!(
            events_named(&run.logs[index], "fallback_engaged").len() <= 1,
            "{who}: at most one fallback transition (the sibling's departure) may ever occur;\n{}",
            run.diagnostics(index)
        );

        // ...over IPv6, with the exact exchange on both channel labels.
        let failure_context = run.selected_pair_failure_context(index);
        assert_ipv6_host_path(window, who, peer_id, &failure_context);
        assert_no_ipv4_candidate_advertised(window, who);
        assert_exchange_sent_to(&run.logs[index], who, &peers);
        assert_exchange_received_from(&run.logs[index], who, &peers);
    }
}
