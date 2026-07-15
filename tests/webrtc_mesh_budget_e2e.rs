//! P10.C empirical native WebRTC topology/size and H8 signal-budget experiments.
//!
//! The nightly real-process matrix covers clean mesh and host topologies at
//! N=2/8/16. The H8 fault variant additionally proves a partial partition: one
//! client with deterministic crippled ICE falls back to the WebSocket relay
//! while the other 15 retain their exact 105-edge WebRTC submesh. Every cell
//! stays under the production 600-signal per-connection budget and preserves
//! exact WebRTC-channel and relay-floor ledgers. The tests are ignored because
//! running up to 16 real webrtc-rs stacks is intentionally outside the PR
//! machine's cheap local test budget.

#![cfg(unix)]

#[path = "websocket_test_helpers/native_client_process.rs"]
mod native_client_process;
mod websocket_test_helpers;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use futures_util::future::join_all;
use native_client_process::{spawn_native_client, NativeClientProcess};
use serde_json::{json, Value};
use signal_fish_server::config::Config;
use uuid::Uuid;
use websocket_test_helpers::prometheus_scrape::{
    assert_scraped_message_conservation, fetch_prometheus_text, sample_value,
    scrape_delivery_counters,
};
use websocket_test_helpers::server_process::{spawn_server, ServerProcess};

const H8_PLAYERS: usize = 16;
const EVENT_DEADLINE: Duration = Duration::from_secs(60);
const SUCCESS_BARRIER_DEADLINE: Duration = Duration::from_secs(360);
const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(30);
const METRIC_QUIESCENCE_DEADLINE: Duration = Duration::from_secs(30);
const CLIENT_MAX_RUNTIME_SECS: u64 = 540;
const CHANNEL_LABELS: [&str; 2] = ["reliable", "unreliable"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MatrixTopology {
    Mesh,
    Host,
}

impl MatrixTopology {
    const fn label(self) -> &'static str {
        match self {
            Self::Mesh => "mesh",
            Self::Host => "host",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct WebRtcScenario {
    topology: MatrixTopology,
    players: usize,
    crippled_ordinal: Option<usize>,
}

impl WebRtcScenario {
    const fn clean(topology: MatrixTopology, players: usize) -> Self {
        Self {
            topology,
            players,
            crippled_ordinal: None,
        }
    }

    const fn one_crippled_mesh(players: usize, ordinal: usize) -> Self {
        Self {
            topology: MatrixTopology::Mesh,
            players,
            crippled_ordinal: Some(ordinal),
        }
    }

    fn label(self) -> String {
        let fault = if self.crippled_ordinal.is_some() {
            "one-crippled"
        } else {
            "clean"
        };
        format!("{}-n{}-{fault}", self.topology.label(), self.players)
    }

    const fn crippled_ordinal(self) -> Option<usize> {
        self.crippled_ordinal
    }

    const fn p2p_timeout_secs(self) -> u64 {
        if self.crippled_ordinal.is_some() {
            30
        } else {
            180
        }
    }

    const fn run_for_secs(self) -> u64 {
        if self.crippled_ordinal.is_some() {
            90
        } else {
            240
        }
    }
}

fn event_name(event: &Value) -> Option<&str> {
    event.get("event").and_then(Value::as_str)
}

fn events_named<'a>(events: &'a [Value], name: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| event_name(event) == Some(name))
        .collect()
}

fn string_field<'a>(event: &'a Value, field: &str, who: &str) -> &'a str {
    event
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{who}: event lacks string field {field:?}: {event}"))
}

fn single_event<'a>(events: &'a [Value], name: &str, who: &str) -> &'a Value {
    let found = events_named(events, name);
    assert_eq!(
        found.len(),
        1,
        "{who}: expected exactly one {name:?} event, got {}: {found:?}",
        found.len()
    );
    found[0]
}

fn success_window<'a>(events: &'a [Value], who: &str) -> &'a [Value] {
    let end = events
        .iter()
        .position(|event| event_name(event) == Some("success_criteria_met"))
        .unwrap_or_else(|| panic!("{who}: missing success_criteria_met barrier event"));
    assert_eq!(
        events_named(events, "success_criteria_met").len(),
        1,
        "{who}: success barrier must be emitted exactly once"
    );
    &events[..=end]
}

fn held_window<'a>(events: &'a [Value], who: &str) -> &'a [Value] {
    let _success_prefix = success_window(events, who);
    events
}

fn client_args(
    server: &ServerProcess,
    name: &str,
    room_code: Option<&str>,
    success_release_file: &Path,
    scenario: WebRtcScenario,
    ordinal: usize,
) -> Vec<String> {
    let mut args = vec![
        "--server-url".to_string(),
        format!("ws://127.0.0.1:{}/v3/ws", server.port),
        "--game-name".to_string(),
        format!("webrtc-{}", scenario.label()),
        "--player-name".to_string(),
        name.to_string(),
        "--peers".to_string(),
        scenario.players.to_string(),
        "--exchange".to_string(),
        "--relay-payload".to_string(),
        format!("relay-from-{name}"),
        "--supported-topologies".to_string(),
        scenario.topology.label().to_string(),
        "--p2p-timeout-secs".to_string(),
        scenario.p2p_timeout_secs().to_string(),
        "--run-for-secs".to_string(),
        scenario.run_for_secs().to_string(),
        "--max-runtime-secs".to_string(),
        CLIENT_MAX_RUNTIME_SECS.to_string(),
        "--success-release-file".to_string(),
        success_release_file.to_string_lossy().into_owned(),
    ];
    if scenario.crippled_ordinal() == Some(ordinal) {
        args.push("--cripple-ice".to_string());
    }
    match room_code {
        Some(code) => args.extend(["--join-code".to_string(), code.to_string()]),
        None => args.push("--create-room".to_string()),
    }
    args
}

fn server_config(topology: MatrixTopology) -> Value {
    json!({
        "session": {
            "default_topology": topology.label(),
            "enable_webrtc": true
        },
        "turn": {
            "enabled": false,
            "stun_urls": []
        }
    })
}

fn rss_kib(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

async fn sample_peak_rss(pids: Vec<u32>, mut stop: tokio::sync::watch::Receiver<bool>) -> Vec<u64> {
    let mut peaks = vec![0; pids.len()];
    loop {
        for (peak, pid) in peaks.iter_mut().zip(&pids) {
            if let Some(sample) = rss_kib(*pid) {
                *peak = (*peak).max(sample);
            }
        }
        tokio::select! {
            result = stop.changed() => {
                if result.is_err() || *stop.borrow() {
                    return peaks;
                }
            }
            () = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
}

async fn wait_for_metrics_quiescence(port: u16) {
    let deadline = tokio::time::Instant::now() + METRIC_QUIESCENCE_DEADLINE;
    loop {
        let counters = scrape_delivery_counters(port).await;
        let resolved = counters.enqueued + counters.channel_closed + counters.canceled;
        if counters.active_connections == 0
            && resolved <= counters.attempts
            && counters.attempts <= resolved + counters.dropped
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "server metrics did not quiesce within {METRIC_QUIESCENCE_DEADLINE:?}: {counters:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_exact_signal_ledger(port: u16, expected: u64) {
    let deadline = tokio::time::Instant::now() + EVENT_DEADLINE;
    loop {
        let metrics = fetch_prometheus_text(port).await;
        let relayed = sample_value(&metrics, "signal_fish_transport_signals_relayed_total");
        if relayed == expected {
            return;
        }
        assert!(
            relayed < expected,
            "server relayed {relayed} signals after every client reported ICE gathering complete, exceeding the exact client ledger {expected}"
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "server signal ledger did not reach {expected} within {EVENT_DEADLINE:?}; last value {relayed}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn assert_exact_peer_events(
    events: &[Value],
    event: &str,
    field: &str,
    expected_peers: &BTreeSet<&str>,
    who: &str,
) {
    let mut counts = BTreeMap::<&str, usize>::new();
    for item in events_named(events, event) {
        *counts.entry(string_field(item, field, who)).or_default() += 1;
    }
    let actual: BTreeSet<&str> = counts.keys().copied().collect();
    assert_eq!(actual, *expected_peers, "{who}: {event} peer set");
    for (peer, count) in counts {
        assert_eq!(count, 1, "{who}: duplicate {event} for {peer}");
    }
}

fn assert_exact_channel_ledger(
    events: &[Value],
    event: &str,
    expected_peers: &BTreeSet<&str>,
    player_id: &str,
    who: &str,
) {
    let mut counts = BTreeMap::<(&str, &str), usize>::new();
    for item in events_named(events, event) {
        let peer = string_field(item, "peer", who);
        let label = string_field(item, "label", who);
        assert!(
            expected_peers.contains(peer),
            "{who}: {event} names unexpected peer {peer}"
        );
        assert!(
            CHANNEL_LABELS.contains(&label),
            "{who}: {event} names unexpected channel {label}"
        );
        if event != "channel_open" {
            let text = string_field(item, "text", who);
            let payload: Value = serde_json::from_str(text).unwrap_or_else(|error| {
                panic!("{who}: invalid {event} JSON payload {text:?}: {error}")
            });
            let expected_from = if event == "channel_message_sent" {
                player_id
            } else {
                peer
            };
            assert_eq!(
                payload.get("from").and_then(Value::as_str),
                Some(expected_from),
                "{who}: {event} payload sender for peer {peer}"
            );
            assert_eq!(
                payload.get("channel").and_then(Value::as_str),
                Some(label),
                "{who}: {event} payload channel for peer {peer}"
            );
            assert_eq!(
                payload.get("seq").and_then(Value::as_u64),
                Some(0),
                "{who}: {event} payload sequence for peer {peer}"
            );
            assert_eq!(
                payload.as_object().map(serde_json::Map::len),
                Some(3),
                "{who}: {event} payload must contain only from/channel/seq"
            );
        }
        *counts.entry((peer, label)).or_default() += 1;
    }
    for peer in expected_peers {
        for label in CHANNEL_LABELS {
            assert_eq!(
                counts.get(&(*peer, label)),
                Some(&1),
                "{who}: expected exactly one {event} for ({peer}, {label}); ledger={counts:?}"
            );
        }
    }
    assert_eq!(
        counts.len(),
        expected_peers.len() * CHANNEL_LABELS.len(),
        "{who}: {event} contains stray ledger entries: {counts:?}"
    );
}

fn assert_exact_signal_ledger(
    clients: &[NativeClientProcess],
    ids: &[String],
    all_ids: &BTreeSet<&str>,
    host_id: &str,
    topology: MatrixTopology,
) {
    let mut sent = BTreeMap::<(String, String, String), usize>::new();
    let mut received = BTreeMap::<(String, String, String), usize>::new();

    for (client, player_id) in clients.iter().zip(ids) {
        let expected_peers = expected_planned_peers(player_id, all_ids, host_id, topology);
        for event in events_named(held_window(&client.events, &client.name), "signal_sent") {
            let to = string_field(event, "to", &client.name);
            let kind = string_field(event, "kind", &client.name);
            assert!(
                expected_peers.contains(to),
                "{}: sent off-graph {kind} signal to {to}",
                client.name
            );
            assert_ne!(kind, "other", "{}: emitted opaque signal kind", client.name);
            *sent
                .entry((player_id.clone(), to.to_string(), kind.to_string()))
                .or_default() += 1;
        }
        for event in events_named(held_window(&client.events, &client.name), "signal_received") {
            let from = string_field(event, "from", &client.name);
            let kind = string_field(event, "kind", &client.name);
            assert!(
                expected_peers.contains(from),
                "{}: received off-graph {kind} signal from {from}",
                client.name
            );
            assert_ne!(
                kind, "other",
                "{}: received opaque signal kind",
                client.name
            );
            *received
                .entry((from.to_string(), player_id.clone(), kind.to_string()))
                .or_default() += 1;
        }
    }

    assert_eq!(
        received,
        sent,
        "{}: every outbound signal must arrive exactly once at its intended peer",
        topology.label()
    );

    for (left_index, left) in ids.iter().enumerate() {
        for right in ids.iter().skip(left_index + 1) {
            if !expected_planned_peers(left, all_ids, host_id, topology).contains(right.as_str()) {
                continue;
            }
            let kind_count = |kind: &str| {
                sent.get(&(left.clone(), right.clone(), kind.to_string()))
                    .copied()
                    .unwrap_or_default()
                    + sent
                        .get(&(right.clone(), left.clone(), kind.to_string()))
                        .copied()
                        .unwrap_or_default()
            };
            assert_eq!(
                kind_count("offer"),
                1,
                "{} edge {left}<->{right}: exact offer ledger",
                topology.label()
            );
            assert_eq!(
                kind_count("answer"),
                1,
                "{} edge {left}<->{right}: exact answer ledger",
                topology.label()
            );
            assert!(
                kind_count("ice_candidate") > 0,
                "{} edge {left}<->{right}: at least one trickle-ICE candidate",
                topology.label()
            );
        }
    }
}

fn expected_planned_peers<'a>(
    player_id: &'a str,
    all_ids: &BTreeSet<&'a str>,
    host_id: &'a str,
    topology: MatrixTopology,
) -> BTreeSet<&'a str> {
    match topology {
        MatrixTopology::Mesh => all_ids
            .iter()
            .copied()
            .filter(|candidate| *candidate != player_id)
            .collect(),
        MatrixTopology::Host if player_id == host_id => all_ids
            .iter()
            .copied()
            .filter(|candidate| *candidate != player_id)
            .collect(),
        MatrixTopology::Host => BTreeSet::from([host_id]),
    }
}

fn expected_connected_peers<'a>(
    player_id: &'a str,
    all_ids: &BTreeSet<&'a str>,
    host_id: &'a str,
    topology: MatrixTopology,
    crippled_id: Option<&'a str>,
) -> BTreeSet<&'a str> {
    if crippled_id == Some(player_id) {
        return BTreeSet::new();
    }
    expected_planned_peers(player_id, all_ids, host_id, topology)
        .into_iter()
        .filter(|candidate| Some(*candidate) != crippled_id)
        .collect()
}

fn assert_client_barrier(
    client: &NativeClientProcess,
    player_id: &str,
    all_ids: &BTreeSet<&str>,
    names_by_id: &BTreeMap<&str, String>,
    signal_budget: usize,
    host_id: &str,
    scenario: WebRtcScenario,
    crippled_id: Option<&str>,
) -> usize {
    let who = &client.name;
    let window = held_window(&client.events, who);
    let room_peers: BTreeSet<&str> = all_ids
        .iter()
        .copied()
        .filter(|candidate| *candidate != player_id)
        .collect();
    let expected_peers = expected_planned_peers(player_id, all_ids, host_id, scenario.topology);
    let expected_connections =
        expected_connected_peers(player_id, all_ids, host_id, scenario.topology, crippled_id);
    let expected_connected = !expected_connections.is_empty();

    let barrier_errors = events_named(window, "error");
    assert!(
        barrier_errors.is_empty(),
        "{who}: errors before the successful {} barrier: {barrier_errors:?}",
        scenario.label()
    );
    assert_eq!(
        events_named(window, "fallback_engaged").len(),
        usize::from(!expected_connected),
        "{who}: fallback count must match its WebRTC connectivity"
    );
    let plan = single_event(window, "session_plan", who);
    assert_eq!(
        string_field(plan, "topology", who),
        scenario.topology.label()
    );
    assert_eq!(string_field(plan, "transport", who), "webrtc");
    assert_eq!(string_field(plan, "fallback", who), "relay");
    match scenario.topology {
        MatrixTopology::Mesh => assert!(plan.get("host").is_some_and(Value::is_null)),
        MatrixTopology::Host => assert_eq!(
            plan.get("host").and_then(Value::as_str),
            Some(host_id),
            "{who}: every host plan must name the creator"
        ),
    }
    assert_eq!(
        plan.get("ice_servers_count").and_then(Value::as_u64),
        Some(0),
        "{who}: loopback experiment must not contact external ICE servers"
    );
    let peers = plan
        .get("peers")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{who}: session plan lacks peers: {plan}"));
    assert_eq!(
        peers.len(),
        expected_peers.len(),
        "{who}: session-plan peer count"
    );
    let mut planned = BTreeSet::new();
    let self_uuid = Uuid::parse_str(player_id).expect("player id is a UUID");
    for peer in peers {
        let peer_id = string_field(peer, "player_id", who);
        assert!(
            planned.insert(peer_id),
            "{who}: duplicate planned peer {peer_id}"
        );
        let expected_initiate = match scenario.topology {
            MatrixTopology::Mesh => {
                let peer_uuid = Uuid::parse_str(peer_id).expect("planned peer id is a UUID");
                self_uuid < peer_uuid
            }
            MatrixTopology::Host => player_id != host_id,
        };
        assert_eq!(
            peer.get("initiate").and_then(Value::as_bool),
            Some(expected_initiate),
            "{who}: glare role for {peer_id} in {}",
            scenario.topology.label()
        );
    }
    assert_eq!(
        planned,
        expected_peers,
        "{who}: exact {} plan peer set",
        scenario.topology.label()
    );

    assert_exact_peer_events(
        window,
        "p2p_pair_connected",
        "peer",
        &expected_connections,
        who,
    );
    assert_exact_channel_ledger(
        window,
        "channel_open",
        &expected_connections,
        player_id,
        who,
    );
    assert_exact_channel_ledger(
        window,
        "channel_message_sent",
        &expected_connections,
        player_id,
        who,
    );
    assert_exact_channel_ledger(
        window,
        "channel_message",
        &expected_connections,
        player_id,
        who,
    );

    let status = single_event(window, "transport_status_sent", who);
    assert_eq!(string_field(status, "transport", who), "webrtc");
    assert_eq!(
        status.get("connected").and_then(Value::as_bool),
        Some(expected_connected),
        "{who}: own transport status"
    );
    let mut peer_status_counts = BTreeMap::<&str, usize>::new();
    for peer_status in events_named(window, "peer_transport_status") {
        assert_eq!(
            string_field(peer_status, "transport", who),
            "webrtc",
            "{who}: peer status transport"
        );
        let peer = string_field(peer_status, "peer", who);
        assert!(room_peers.contains(peer), "{who}: stray peer status {peer}");
        *peer_status_counts.entry(peer).or_default() += 1;
        let expected_peer_connected =
            !expected_connected_peers(peer, all_ids, host_id, scenario.topology, crippled_id)
                .is_empty();
        assert_eq!(
            peer_status.get("connected").and_then(Value::as_bool),
            Some(expected_peer_connected),
            "{who}: peer transport status for {peer}"
        );
    }
    assert_eq!(
        peer_status_counts.keys().copied().collect::<BTreeSet<_>>(),
        room_peers,
        "{who}: exact peer transport-status set"
    );
    for (peer, count) in peer_status_counts {
        assert_eq!(count, 1, "{who}: duplicate transport status from {peer}");
    }

    single_event(window, "game_data_sent", who);
    let relay_events = events_named(window, "game_data_received");
    let mut relay_ledger = BTreeMap::new();
    for event in relay_events {
        let from = string_field(event, "from", who);
        let payload = event
            .get("payload")
            .and_then(|value| value.get("relay_msg"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{who}: relay event lacks relay_msg: {event}"));
        let sender_name = names_by_id
            .get(from)
            .map(String::as_str)
            .unwrap_or_else(|| panic!("{who}: relay sender {from} is outside the room"));
        assert_eq!(payload, format!("relay-from-{sender_name}"));
        assert!(
            relay_ledger.insert(from, payload).is_none(),
            "{who}: duplicate relay payload from {from}"
        );
    }
    assert_eq!(
        relay_ledger.keys().copied().collect::<BTreeSet<_>>(),
        room_peers,
        "{who}: relay-floor sender ledger"
    );

    let signal_count = events_named(window, "signal_sent").len();
    assert!(
        signal_count < signal_budget,
        "{who}: emitted {signal_count} signals plus one TransportStatus, exceeding production control-plane budget {signal_budget}"
    );
    signal_count
}

fn assert_client_exit(client: &NativeClientProcess, status: &std::process::ExitStatus) {
    assert!(
        status.success(),
        "{} exited {status};\n{}",
        client.name,
        client.diagnostics()
    );
    assert!(
        events_named(&client.events, "error").is_empty(),
        "{}: errors after WebRTC release: {:?}",
        client.name,
        client.error_messages()
    );
    let exit = single_event(&client.events, "exiting", &client.name);
    assert_eq!(exit.get("code").and_then(Value::as_i64), Some(0));
}

async fn run_webrtc_scenario(scenario: WebRtcScenario) {
    let total_started = tokio::time::Instant::now();
    let maximum_bounded_host_release_wait =
        EVENT_DEADLINE + SUCCESS_BARRIER_DEADLINE + EVENT_DEADLINE + CLIENT_EXIT_DEADLINE;
    assert!(
        Duration::from_secs(CLIENT_MAX_RUNTIME_SECS) > maximum_bounded_host_release_wait,
        "client watchdog must leave headroom beyond room creation, the success barrier, signal-ledger settlement, and staged host teardown"
    );
    let signal_budget = usize::try_from(Config::default().rate_limit.max_signals)
        .expect("production signal budget fits usize");
    assert_eq!(
        signal_budget, 600,
        "H8 is registered against the real default"
    );
    if let Some(ordinal) = scenario.crippled_ordinal() {
        assert!(
            ordinal < scenario.players,
            "crippled ordinal must name a client"
        );
    }
    assert!(
        scenario.players >= 2,
        "WebRTC cells require at least two clients"
    );

    let mut server = spawn_server(server_config(scenario.topology)).await;
    let workdir = tempfile::tempdir().expect("create native-client workdir");
    let success_release_files = match scenario.topology {
        MatrixTopology::Mesh => {
            let shared = workdir.path().join("release-success");
            vec![shared; scenario.players]
        }
        MatrixTopology::Host => (0..scenario.players)
            .map(|ordinal| {
                workdir
                    .path()
                    .join(format!("release-success-c{ordinal:02}"))
            })
            .collect(),
    };
    assert!(success_release_files.iter().all(|path| !path.exists()));
    let mut creator = spawn_native_client(
        "c00",
        &client_args(&server, "c00", None, &success_release_files[0], scenario, 0),
        workdir.path(),
    );
    let created = creator.await_event("room_created", EVENT_DEADLINE).await;
    let room_code = string_field(&created, "room_code", "c00").to_string();

    let mut clients = Vec::with_capacity(scenario.players);
    clients.push(creator);
    for (ordinal, success_release_file) in success_release_files.iter().enumerate().skip(1) {
        let name = format!("c{ordinal:02}");
        clients.push(spawn_native_client(
            &name,
            &client_args(
                &server,
                &name,
                Some(&room_code),
                success_release_file,
                scenario,
                ordinal,
            ),
            workdir.path(),
        ));
    }

    let pids: Vec<u32> = std::iter::once(server.pid())
        .chain(clients.iter().map(NativeClientProcess::pid))
        .collect();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let rss_task = tokio::spawn(sample_peak_rss(pids, stop_rx));
    let barrier_started = tokio::time::Instant::now();
    join_all(clients.iter_mut().map(|client| {
        client.await_event_count("success_criteria_met", 1, SUCCESS_BARRIER_DEADLINE)
    }))
    .await;
    let barrier_elapsed = barrier_started.elapsed();

    let ids: Vec<String> = clients
        .iter()
        .map(|client| {
            string_field(
                single_event(&client.events, "room_joined", &client.name),
                "player_id",
                &client.name,
            )
            .to_string()
        })
        .collect();
    let all_ids: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
    assert_eq!(
        all_ids.len(),
        scenario.players,
        "all player ids must be distinct"
    );
    let names_by_id: BTreeMap<&str, String> = ids
        .iter()
        .zip(&clients)
        .map(|(id, client)| (id.as_str(), client.name.clone()))
        .collect();
    let crippled_id = scenario
        .crippled_ordinal()
        .map(|ordinal| ids[ordinal].as_str());
    let host_id = ids[0].as_str();

    // Mesh clients already wait for every room peer's status because every
    // peer is a pairing obligation. Host leaves wait only for the host, so a
    // sibling leaf's room-wide fan-out may still be buffered after the global
    // success barrier. Settle that exact full-room ledger while the clients
    // remain held. In parallel, settle the server signal metric against the
    // signal events frozen causally before each success marker.
    let barrier_signal_counts: Vec<usize> = clients
        .iter()
        .map(|client| {
            events_named(success_window(&client.events, &client.name), "signal_sent").len()
        })
        .collect();
    let barrier_total_signals: usize = barrier_signal_counts.iter().sum();
    let id_indexes: BTreeMap<&str, usize> = ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index))
        .collect();
    let mut expected_inbound_signals = vec![0; scenario.players];
    for client in &clients {
        for event in events_named(success_window(&client.events, &client.name), "signal_sent") {
            let to = string_field(event, "to", &client.name);
            let target_index = id_indexes.get(to).copied().unwrap_or_else(|| {
                panic!("{}: signal target {to} is outside the room", client.name)
            });
            expected_inbound_signals[target_index] += 1;
        }
    }
    let event_settlement = join_all(clients.iter_mut().zip(expected_inbound_signals).map(
        |(client, inbound_signals)| async move {
            let mut expected = vec![("peer_transport_status", scenario.players - 1)];
            if scenario.crippled_ordinal().is_none() {
                expected.push(("signal_received", inbound_signals));
            }
            client.await_event_counts(&expected, EVENT_DEADLINE).await;
        },
    ));
    let signal_settlement = wait_for_exact_signal_ledger(
        server.port,
        u64::try_from(barrier_total_signals).expect("signal total fits u64"),
    );
    let ((), ()) = tokio::join!(
        async {
            event_settlement.await;
        },
        signal_settlement
    );

    let mut per_client_signals = Vec::with_capacity(scenario.players);
    for (client, id) in clients.iter().zip(&ids) {
        per_client_signals.push(assert_client_barrier(
            client,
            id,
            &all_ids,
            &names_by_id,
            signal_budget,
            host_id,
            scenario,
            crippled_id,
        ));
    }
    assert_eq!(
        per_client_signals,
        barrier_signal_counts,
        "{}: signal ledger changed after the success barrier",
        scenario.label()
    );
    let total_signals: usize = per_client_signals.iter().sum();
    let mut sorted_signals = per_client_signals.clone();
    sorted_signals.sort_unstable();
    let signal_p50 = sorted_signals[(sorted_signals.len() - 1) / 2];
    let signal_p99 = *sorted_signals.last().expect("the matrix has clients");

    let held_counters = scrape_delivery_counters(server.port).await;
    assert_eq!(
        held_counters.backpressure_events,
        0,
        "{} backpressure",
        scenario.label()
    );
    assert_eq!(
        held_counters.slow_consumer_disconnects,
        0,
        "{} slow-consumer evictions",
        scenario.label()
    );
    assert_eq!(
        held_counters.dropped,
        0,
        "{} messages dropped before coordinated teardown",
        scenario.label()
    );
    assert_scraped_message_conservation(&held_counters);
    assert_eq!(
        total_signals,
        barrier_total_signals,
        "{}: settled signal total",
        scenario.label()
    );
    if crippled_id.is_none() {
        assert_exact_signal_ledger(&clients, &ids, &all_ids, host_id, scenario.topology);
    }
    let held_metrics = fetch_prometheus_text(server.port).await;
    let expected_connected_clients = ids
        .iter()
        .filter(|id| {
            !expected_connected_peers(id, &all_ids, host_id, scenario.topology, crippled_id)
                .is_empty()
        })
        .count();
    let expected_fallbacks = scenario.players - expected_connected_clients;
    assert_eq!(
        sample_value(&held_metrics, "signal_fish_transport_p2p_established_total"),
        u64::try_from(expected_connected_clients).expect("player count fits u64"),
        "{} P2P-established client count",
        scenario.label()
    );
    assert_eq!(
        sample_value(&held_metrics, "signal_fish_transport_relay_fallback_total"),
        u64::try_from(expected_fallbacks).expect("fallback count fits u64"),
        "{} relay-fallback client count",
        scenario.label()
    );
    assert_eq!(
        sample_value(&held_metrics, "signal_fish_websocket_ping_timeouts_total"),
        0,
        "{} must not lose clients to ping timeout",
        scenario.label()
    );

    let statuses = match scenario.topology {
        MatrixTopology::Mesh => {
            std::fs::write(&success_release_files[0], b"release")
                .expect("release successful mesh clients from the WebRTC barrier");
            join_all(
                clients
                    .iter_mut()
                    .map(|client| client.drain_to_termination(CLIENT_EXIT_DEADLINE)),
            )
            .await
        }
        MatrixTopology::Host => {
            // Keep the elected host alive until every leaf has completed its
            // teardown signaling and the server has causally broadcast every
            // departure. Releasing the whole star at once can let the host
            // leave first and turn a leaf's final signal into a genuine
            // SignalTargetNotFound server error, obscuring the measured phase.
            for release_file in success_release_files.iter().skip(1) {
                std::fs::write(release_file, b"release")
                    .expect("release successful host-topology leaf client");
            }
            let (host, leaves) = clients
                .split_first_mut()
                .expect("the scenario always has a creator");
            let leaf_terminations = join_all(
                leaves
                    .iter_mut()
                    .map(|client| client.drain_to_termination(CLIENT_EXIT_DEADLINE)),
            );
            let host_departure_barrier =
                host.await_event_count("player_left", scenario.players - 1, CLIENT_EXIT_DEADLINE);
            let (leaf_statuses, ()) = tokio::join!(leaf_terminations, host_departure_barrier);
            std::fs::write(&success_release_files[0], b"release")
                .expect("release successful host-topology creator client");
            let host_status = host.drain_to_termination(CLIENT_EXIT_DEADLINE).await;
            std::iter::once(host_status).chain(leaf_statuses).collect()
        }
    };
    let total_elapsed = total_started.elapsed();
    stop_tx.send(true).expect("stop RSS sampler");
    let peak_rss = rss_task.await.expect("RSS sampler task succeeds");
    for (client, status) in clients.iter().zip(&statuses) {
        assert_client_exit(client, status);
    }

    wait_for_metrics_quiescence(server.port).await;
    let counters = scrape_delivery_counters(server.port).await;
    assert_eq!(
        counters.backpressure_events,
        0,
        "{} backpressure",
        scenario.label()
    );
    assert_eq!(
        counters.slow_consumer_disconnects,
        0,
        "{} slow-consumer evictions",
        scenario.label()
    );
    assert_scraped_message_conservation(&counters);
    let metrics = fetch_prometheus_text(server.port).await;
    assert_eq!(
        sample_value(&metrics, "signal_fish_websocket_ping_timeouts_total"),
        0,
        "{} must not lose clients to ping timeout",
        scenario.label()
    );

    let directed_connected_edges: usize = ids
        .iter()
        .map(|id| {
            expected_connected_peers(id, &all_ids, host_id, scenario.topology, crippled_id).len()
        })
        .sum();
    assert_eq!(
        directed_connected_edges % 2,
        0,
        "the expected WebRTC graph must be symmetric"
    );
    let connected_pairs = directed_connected_edges / 2;
    println!(
        "WebRTC matrix cell complete: scenario={} players={} connected_pairs={connected_pairs} total_signals={total_signals} signals_per_client={per_client_signals:?} signal_p50={signal_p50} signal_p99={signal_p99} all_clients_at_success_barrier={barrier_elapsed:?} total_elapsed={total_elapsed:?} coordinated_teardown_drops={} post_spawn_server_peak_rss_kib={} post_spawn_client_peak_rss_kib={:?}",
        scenario.label(),
        scenario.players,
        counters
            .dropped
            .checked_sub(held_counters.dropped)
            .expect("server dropped-message counter is monotonic"),
        peak_rss.first().copied().unwrap_or(0),
        &peak_rss[1..]
    );
    server.kill_and_wait().await;
}

#[test]
fn connectivity_oracle_preserves_mesh_and_host_graphs() {
    let all = BTreeSet::from(["a", "b", "c"]);
    assert_eq!(
        expected_connected_peers("b", &all, "a", MatrixTopology::Mesh, None),
        BTreeSet::from(["a", "c"]),
        "clean mesh connects every other peer"
    );
    assert_eq!(
        expected_connected_peers("b", &all, "a", MatrixTopology::Mesh, Some("c")),
        BTreeSet::from(["a"]),
        "healthy peer excludes the crippled member"
    );
    assert!(
        expected_connected_peers("c", &all, "a", MatrixTopology::Mesh, Some("c")).is_empty(),
        "crippled member forms no WebRTC edges"
    );
    assert_eq!(
        expected_connected_peers("a", &all, "a", MatrixTopology::Host, None),
        BTreeSet::from(["b", "c"]),
        "host connects to every client"
    );
    assert_eq!(
        expected_connected_peers("b", &all, "a", MatrixTopology::Host, None),
        BTreeSet::from(["a"]),
        "each client connects only to the host"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nightly-only (verification-nightly.yml): spawns 16 real webrtc-rs clients"]
async fn sixteen_native_clients_form_complete_mesh_within_signal_budget() {
    run_webrtc_scenario(WebRtcScenario::clean(MatrixTopology::Mesh, H8_PLAYERS)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nightly-only (verification-nightly.yml): spawns 16 real webrtc-rs clients"]
async fn one_crippled_ice_client_falls_back_without_breaking_healthy_submesh() {
    run_webrtc_scenario(WebRtcScenario::one_crippled_mesh(
        H8_PLAYERS,
        H8_PLAYERS - 1,
    ))
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nightly-only (verification-nightly.yml): spawns 2 real webrtc-rs clients"]
async fn clean_mesh_n2_has_exact_graph_and_ledgers() {
    run_webrtc_scenario(WebRtcScenario::clean(MatrixTopology::Mesh, 2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nightly-only (verification-nightly.yml): spawns 8 real webrtc-rs clients"]
async fn clean_mesh_n8_has_exact_graph_and_ledgers() {
    run_webrtc_scenario(WebRtcScenario::clean(MatrixTopology::Mesh, 8)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nightly-only (verification-nightly.yml): spawns 2 real webrtc-rs clients"]
async fn clean_host_n2_has_exact_graph_and_ledgers() {
    run_webrtc_scenario(WebRtcScenario::clean(MatrixTopology::Host, 2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nightly-only (verification-nightly.yml): spawns 8 real webrtc-rs clients"]
async fn clean_host_n8_has_exact_graph_and_ledgers() {
    run_webrtc_scenario(WebRtcScenario::clean(MatrixTopology::Host, 8)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nightly-only (verification-nightly.yml): spawns 16 real webrtc-rs clients"]
async fn clean_host_n16_has_exact_graph_and_ledgers() {
    run_webrtc_scenario(WebRtcScenario::clean(MatrixTopology::Host, 16)).await;
}
