//! P10.H8 empirical 16-player native WebRTC mesh signal-budget experiments.
//!
//! The model test proves the arithmetic. These nightly real-process experiments
//! prove both the complete clean mesh and a partial partition: one client with
//! deterministic crippled ICE falls back to the WebSocket relay while the
//! other 15 retain their exact 105-edge WebRTC submesh. Both stay under the
//! production 600-signal per-connection budget and preserve exact relay-floor
//! delivery. They are ignored because running 16 real webrtc-rs stacks is
//! intentionally outside the PR machine's cheap local test budget.

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

const PLAYERS: usize = 16;
const GAME_NAME: &str = "webrtc-mesh-budget";
const EVENT_DEADLINE: Duration = Duration::from_secs(60);
const SUCCESS_BARRIER_DEADLINE: Duration = Duration::from_secs(360);
const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(30);
const METRIC_QUIESCENCE_DEADLINE: Duration = Duration::from_secs(30);
const CLIENT_MAX_RUNTIME_SECS: u64 = 540;
const CHANNEL_LABELS: [&str; 2] = ["reliable", "unreliable"];

#[derive(Clone, Copy, Debug)]
enum MeshScenario {
    Clean,
    OneCrippled { ordinal: usize },
}

impl MeshScenario {
    fn label(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::OneCrippled { .. } => "one-crippled",
        }
    }

    fn crippled_ordinal(self) -> Option<usize> {
        match self {
            Self::Clean => None,
            Self::OneCrippled { ordinal } => Some(ordinal),
        }
    }

    fn p2p_timeout_secs(self) -> u64 {
        match self {
            Self::Clean => 180,
            Self::OneCrippled { .. } => 30,
        }
    }

    fn run_for_secs(self) -> u64 {
        match self {
            Self::Clean => 240,
            Self::OneCrippled { .. } => 90,
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

fn client_args(
    server: &ServerProcess,
    name: &str,
    room_code: Option<&str>,
    success_release_file: &Path,
    scenario: MeshScenario,
    ordinal: usize,
) -> Vec<String> {
    let mut args = vec![
        "--server-url".to_string(),
        format!("ws://127.0.0.1:{}/v3/ws", server.port),
        "--game-name".to_string(),
        GAME_NAME.to_string(),
        "--player-name".to_string(),
        name.to_string(),
        "--peers".to_string(),
        PLAYERS.to_string(),
        "--exchange".to_string(),
        "--relay-payload".to_string(),
        format!("relay-from-{name}"),
        "--supported-topologies".to_string(),
        "mesh".to_string(),
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

fn mesh_server_config() -> Value {
    json!({
        "session": {
            "default_topology": "mesh",
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

fn expected_connected_peers<'a>(
    player_id: &'a str,
    all_ids: &BTreeSet<&'a str>,
    crippled_id: Option<&'a str>,
) -> BTreeSet<&'a str> {
    if crippled_id == Some(player_id) {
        return BTreeSet::new();
    }
    all_ids
        .iter()
        .copied()
        .filter(|candidate| *candidate != player_id && Some(*candidate) != crippled_id)
        .collect()
}

fn assert_client_barrier(
    client: &NativeClientProcess,
    player_id: &str,
    all_ids: &BTreeSet<&str>,
    names_by_id: &BTreeMap<&str, &str>,
    signal_budget: usize,
    crippled_id: Option<&str>,
) -> usize {
    let who = &client.name;
    let window = success_window(&client.events, who);
    let expected_peers: BTreeSet<&str> = all_ids
        .iter()
        .copied()
        .filter(|candidate| *candidate != player_id)
        .collect();
    let expected_connected_peers = expected_connected_peers(player_id, all_ids, crippled_id);
    let expected_connected = !expected_connected_peers.is_empty();

    assert!(
        events_named(&client.events, "error").is_empty(),
        "{who}: errors before the successful mesh barrier: {:?}",
        client.error_messages()
    );
    assert_eq!(
        events_named(window, "fallback_engaged").len(),
        usize::from(!expected_connected),
        "{who}: fallback count must match its WebRTC connectivity"
    );
    let plan = single_event(window, "session_plan", who);
    assert_eq!(string_field(plan, "topology", who), "mesh");
    assert_eq!(string_field(plan, "transport", who), "webrtc");
    assert_eq!(string_field(plan, "fallback", who), "relay");
    assert!(plan.get("host").is_some_and(Value::is_null));
    assert_eq!(
        plan.get("ice_servers_count").and_then(Value::as_u64),
        Some(0),
        "{who}: loopback experiment must not contact external ICE servers"
    );
    let peers = plan
        .get("peers")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{who}: session plan lacks peers: {plan}"));
    assert_eq!(peers.len(), PLAYERS - 1, "{who}: session-plan peer count");
    let mut planned = BTreeSet::new();
    let self_uuid = Uuid::parse_str(player_id).expect("player id is a UUID");
    for peer in peers {
        let peer_id = string_field(peer, "player_id", who);
        assert!(
            planned.insert(peer_id),
            "{who}: duplicate planned peer {peer_id}"
        );
        let peer_uuid = Uuid::parse_str(peer_id).expect("planned peer id is a UUID");
        assert_eq!(
            peer.get("initiate").and_then(Value::as_bool),
            Some(self_uuid < peer_uuid),
            "{who}: glare role for {peer_id} must follow UUID ordering"
        );
    }
    assert_eq!(planned, expected_peers, "{who}: exact mesh plan peer set");

    assert_exact_peer_events(
        window,
        "p2p_pair_connected",
        "peer",
        &expected_connected_peers,
        who,
    );
    assert_exact_channel_ledger(
        window,
        "channel_open",
        &expected_connected_peers,
        player_id,
        who,
    );
    assert_exact_channel_ledger(
        window,
        "channel_message_sent",
        &expected_connected_peers,
        player_id,
        who,
    );
    assert_exact_channel_ledger(
        window,
        "channel_message",
        &expected_connected_peers,
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
        assert!(
            expected_peers.contains(peer),
            "{who}: stray peer status {peer}"
        );
        *peer_status_counts.entry(peer).or_default() += 1;
        let expected_peer_connected = Some(peer) != crippled_id;
        assert_eq!(
            peer_status.get("connected").and_then(Value::as_bool),
            Some(expected_peer_connected),
            "{who}: peer transport status for {peer}"
        );
    }
    assert_eq!(
        peer_status_counts.keys().copied().collect::<BTreeSet<_>>(),
        expected_peers,
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
            .copied()
            .unwrap_or_else(|| panic!("{who}: relay sender {from} is outside the mesh"));
        assert_eq!(payload, format!("relay-from-{sender_name}"));
        assert!(
            relay_ledger.insert(from, payload).is_none(),
            "{who}: duplicate relay payload from {from}"
        );
    }
    assert_eq!(
        relay_ledger.keys().copied().collect::<BTreeSet<_>>(),
        expected_peers,
        "{who}: relay-floor sender ledger"
    );

    let signal_count = events_named(window, "signal_sent").len();
    assert!(
        signal_count <= signal_budget,
        "{who}: emitted {signal_count} signals, exceeding production budget {signal_budget}"
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
        "{}: errors after mesh release: {:?}",
        client.name,
        client.error_messages()
    );
    let exit = single_event(&client.events, "exiting", &client.name);
    assert_eq!(exit.get("code").and_then(Value::as_i64), Some(0));
}

async fn run_mesh_scenario(scenario: MeshScenario) {
    let total_started = tokio::time::Instant::now();
    let maximum_bounded_pre_release_wait =
        EVENT_DEADLINE + SUCCESS_BARRIER_DEADLINE + EVENT_DEADLINE;
    assert!(
        Duration::from_secs(CLIENT_MAX_RUNTIME_SECS) > maximum_bounded_pre_release_wait,
        "client watchdog must leave headroom beyond room creation, the success barrier, and signal-ledger settlement"
    );
    let signal_budget = usize::try_from(Config::default().rate_limit.max_signals)
        .expect("production signal budget fits usize");
    assert_eq!(
        signal_budget, 600,
        "H8 is registered against the real default"
    );
    if let Some(ordinal) = scenario.crippled_ordinal() {
        assert!(ordinal < PLAYERS, "crippled ordinal must name a client");
    }

    let mut server = spawn_server(mesh_server_config()).await;
    let workdir = tempfile::tempdir().expect("create native-client workdir");
    let success_release_file = workdir.path().join("release-success");
    assert!(!success_release_file.exists());
    let mut creator = spawn_native_client(
        "c00",
        &client_args(&server, "c00", None, &success_release_file, scenario, 0),
        workdir.path(),
    );
    let created = creator.await_event("room_created", EVENT_DEADLINE).await;
    let room_code = string_field(&created, "room_code", "c00").to_string();

    let mut clients = Vec::with_capacity(PLAYERS);
    clients.push(creator);
    for ordinal in 1..PLAYERS {
        let name = format!("c{ordinal:02}");
        clients.push(spawn_native_client(
            &name,
            &client_args(
                &server,
                &name,
                Some(&room_code),
                &success_release_file,
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
    assert_eq!(all_ids.len(), PLAYERS, "all player ids must be distinct");
    let names_by_id: BTreeMap<&str, &str> = ids
        .iter()
        .zip(&clients)
        .map(|(id, client)| (id.as_str(), client.name.as_str()))
        .collect();
    let crippled_id = scenario
        .crippled_ordinal()
        .map(|ordinal| ids[ordinal].as_str());

    let mut per_client_signals = Vec::with_capacity(PLAYERS);
    for (client, id) in clients.iter().zip(&ids) {
        per_client_signals.push(assert_client_barrier(
            client,
            id,
            &all_ids,
            &names_by_id,
            signal_budget,
            crippled_id,
        ));
    }
    let total_signals: usize = per_client_signals.iter().sum();
    let mut sorted_signals = per_client_signals.clone();
    sorted_signals.sort_unstable();
    let signal_p50 = sorted_signals[(sorted_signals.len() - 1) / 2];
    let signal_p99 = *sorted_signals.last().expect("the matrix has clients");

    let held_counters = scrape_delivery_counters(server.port).await;
    assert_eq!(
        held_counters.backpressure_events,
        0,
        "{} mesh backpressure",
        scenario.label()
    );
    assert_eq!(
        held_counters.slow_consumer_disconnects,
        0,
        "{} mesh slow-consumer evictions",
        scenario.label()
    );
    assert_eq!(
        held_counters.dropped,
        0,
        "{} mesh messages dropped before coordinated teardown",
        scenario.label()
    );
    assert_scraped_message_conservation(&held_counters);
    wait_for_exact_signal_ledger(
        server.port,
        u64::try_from(total_signals).expect("signal total fits u64"),
    )
    .await;
    let held_metrics = fetch_prometheus_text(server.port).await;
    let expected_fallbacks = u64::from(scenario.crippled_ordinal().is_some());
    assert_eq!(
        sample_value(&held_metrics, "signal_fish_transport_p2p_established_total"),
        u64::try_from(PLAYERS).expect("player count fits u64") - expected_fallbacks,
        "{} mesh P2P-established client count",
        scenario.label()
    );
    assert_eq!(
        sample_value(&held_metrics, "signal_fish_transport_relay_fallback_total"),
        expected_fallbacks,
        "{} mesh relay-fallback client count",
        scenario.label()
    );
    assert_eq!(
        sample_value(&held_metrics, "signal_fish_websocket_ping_timeouts_total"),
        0,
        "{} mesh must not lose clients to ping timeout",
        scenario.label()
    );

    std::fs::write(&success_release_file, b"release")
        .expect("release successful clients from the mesh barrier");
    let statuses = join_all(
        clients
            .iter_mut()
            .map(|client| client.drain_to_termination(CLIENT_EXIT_DEADLINE)),
    )
    .await;
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
        "{} mesh backpressure",
        scenario.label()
    );
    assert_eq!(
        counters.slow_consumer_disconnects,
        0,
        "{} mesh slow-consumer evictions",
        scenario.label()
    );
    assert_scraped_message_conservation(&counters);
    let metrics = fetch_prometheus_text(server.port).await;
    assert_eq!(
        sample_value(&metrics, "signal_fish_websocket_ping_timeouts_total"),
        0,
        "{} mesh must not lose clients to ping timeout",
        scenario.label()
    );

    let connected_players = PLAYERS - usize::from(scenario.crippled_ordinal().is_some());
    let connected_pairs = connected_players * (connected_players - 1) / 2;
    println!(
        "H8 mesh complete: scenario={} players={PLAYERS} connected_pairs={connected_pairs} total_signals={total_signals} signals_per_client={per_client_signals:?} signal_p50={signal_p50} signal_p99={signal_p99} all_clients_at_success_barrier={barrier_elapsed:?} total_elapsed={total_elapsed:?} coordinated_teardown_drops={} post_spawn_server_peak_rss_kib={} post_spawn_client_peak_rss_kib={:?}",
        scenario.label(),
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
fn connectivity_oracle_preserves_clean_and_partial_mesh_graphs() {
    let all = BTreeSet::from(["a", "b", "c"]);
    assert_eq!(
        expected_connected_peers("b", &all, None),
        BTreeSet::from(["a", "c"]),
        "clean mesh connects every other peer"
    );
    assert_eq!(
        expected_connected_peers("b", &all, Some("c")),
        BTreeSet::from(["a"]),
        "healthy peer excludes the crippled member"
    );
    assert!(
        expected_connected_peers("c", &all, Some("c")).is_empty(),
        "crippled member forms no WebRTC edges"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nightly-only (verification-nightly.yml): spawns 16 real webrtc-rs clients"]
async fn sixteen_native_clients_form_complete_mesh_within_signal_budget() {
    run_mesh_scenario(MeshScenario::Clean).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "nightly-only (verification-nightly.yml): spawns 16 real webrtc-rs clients"]
async fn one_crippled_ice_client_falls_back_without_breaking_healthy_submesh() {
    run_mesh_scenario(MeshScenario::OneCrippled {
        ordinal: PLAYERS - 1,
    })
    .await;
}
