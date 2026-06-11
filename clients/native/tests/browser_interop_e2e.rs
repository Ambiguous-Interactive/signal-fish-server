//! Browser↔native interop matrix cells (feature `browser-interop`): the REAL
//! `signal-fish-server` binary (via `SIGNAL_FISH_SERVER_BIN`) + REAL
//! `signal-fish-reference-native` processes + the REAL browser reference
//! client (`clients/browser/`, located via `SIGNAL_FISH_BROWSER_CLI`), which
//! drives an actual Chromium `RTCPeerConnection` (headless shell via
//! playwright-core). Every WebRTC pair involving the browser is a true
//! webrtc-rs↔Chromium (or Chromium↔Chromium) DTLS+SCTP session over loopback.
//!
//! # Scenarios
//!
//! 1. `mixed_mesh_n3_full_webrtc_with_browser` — the headline browser↔native
//!    cell: 2 native + 1 browser in a mesh; full glare matrix (total +
//!    antisymmetric, smaller UUID offers), all 6 directed pair endpoints
//!    connect, the full 12-message channel matrix crosses, everyone reports
//!    `TransportStatus{webrtc,true}` and observes both fan-outs, and the
//!    relay floor carries every member's GameData in-window. All exit 0.
//! 2. `browser_pair_mesh_n3` — 2 browser + 1 native (browser creates the
//!    room): proves the page engine against ITSELF through the server
//!    (Chromium↔Chromium pair) alongside two more browser↔native pairs, with
//!    the same full-matrix assertions. Two browsers + one native was chosen
//!    over three browsers: it adds the browser↔browser pair AND keeps
//!    browser↔native coverage while spawning one fewer Chromium (wall-clock
//!    and memory on small CI runners); a third browser adds no new pair type.
//! 3. `host_star_n3_browser_client` — `host + webrtc` star with the browser
//!    as a NON-host client: all plans name the native creator as host, the
//!    browser offers to the host only, pairs exist ONLY along star edges (no
//!    client↔client traffic), and the exchange completes both ways on both
//!    channels. (A browser-as-host variant was considered and dropped for
//!    wall-clock: the mixed mesh already proves the browser answering and
//!    offering across multiple simultaneous pairs.)
//! 4. `mesh_n3_browser_crippled_ice_fallback` — the BROWSER member runs
//!    `--cripple-ice` (drops all outbound AND inbound IceCandidate signals —
//!    the browser cannot filter interfaces, but with zero remote candidates
//!    on both sides no ICE check pair ever forms): it resolves
//!    `TransportStatus{webrtc,false}` exactly once, engages the relay
//!    fallback, produces zero pairs/channel traffic, and is fully served by
//!    the relay floor while the two native members still pair P2P.
//! 5. `mesh_n3_browser_mdns_obfuscation` — the browser runs with Chromium's
//!    default mDNS host-candidate obfuscation LEFT ON (`--mdns-obfuscation`):
//!    its host candidates are opaque `<uuid>.local` names the native side
//!    cannot resolve (no real mDNS responder in CI). EMPIRICALLY PINNED
//!    OUTCOME: P2P still establishes — the browser learns the native side's
//!    REAL host candidates from the relayed signals and initiates ICE
//!    connectivity checks toward them; the native agent answers and learns
//!    the browser's transport address as a peer-reflexive candidate, so every
//!    pair connects, everyone reports `{webrtc,true}`, and the fallback never
//!    engages (webrtc-rs also tolerates the `.local` candidate without
//!    erroring). The cell pins exactly that.
//! 6. `mixed_v2_browser_v3_native_relay_floor` — the browser as a PURE-v2
//!    member (`--protocol-version 2` on `/v2/ws`) floors a mesh-preferring
//!    room: zero `SessionPlan`/`NewPeer`/`Signal`/WebRTC events anywhere, the
//!    browser negotiates protocol version 2, and the full GameData relay
//!    matrix completes for all three. All exit 0.
//! 7. `browser_cli_mid_handshake_close_single_error_exit_3` — a stub server
//!    accepts the WebSocket, consumes `Authenticate`, then closes: the CLI
//!    must emit exactly ONE `error` event carrying the real close reason and
//!    exit 3 promptly (causally — not by waiting out the 20 s handshake-frame
//!    timeout, which would also double-emit `error`).
//! 8. `browser_cli_signal_teardown_reaps_chromium` — the Chromium-never-
//!    outlives-the-CLI guarantee, both halves: a SIGTERM'd CLI tears Chromium
//!    down itself and exits 143; a SIGKILL'd CLI (this harness's kill-on-drop
//!    path, untrappable in-process) is covered by the detached reaper. Either
//!    way zero `headless_shell` descendants survive a bounded window.
//!
//! Scenario assertions are copies of the native suite's
//! (`tests/interop_e2e.rs`) — deliberately NOT shared, so this feature-gated
//! target never forces edits to the always-on native suite. Scenarios are
//! serialized behind a mutex for the same robustness reasons (each spawns 4+
//! OS processes, here including 1-2 Chromium instances).

mod harness;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use futures_util::StreamExt;
use harness::{
    events_named, player_id_of, scenario_window, single_event, spawn_browser_client, spawn_client,
    spawn_server, str_field, ClientProcess, ClientSpec, CLIENT_EXIT_TIMEOUT, EVENT_TIMEOUT,
};
use serde_json::Value;
use uuid::Uuid;

/// Serializes the multi-process scenarios (see module docs).
static SCENARIO_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// Number of scenarios queueing behind [`SCENARIO_SERIAL`] (keep in sync with
/// the `#[tokio::test]` functions in this file).
const SCENARIO_COUNT: u64 = 8;
/// Generous ceiling for ONE scenario in a fully degraded run: server spawn
/// (up to 3 attempts x 15 s health deadline = 45 s) plus one client wave
/// hard-bounded by `--max-runtime-secs 90`, with the rest absorbing Chromium
/// launches and drain/reap overhead.
const SCENARIO_CEILING: Duration = Duration::from_secs(240);
/// Worst case for the LAST test in the queue: every other scenario runs to
/// its completion (or its panic deadline) first.
const SERIAL_ACQUIRE_TIMEOUT: Duration =
    Duration::from_secs(SCENARIO_CEILING.as_secs() * (SCENARIO_COUNT - 1));

const RELIABLE: &str = "reliable";
const UNRELIABLE: &str = "unreliable";
const CLIENT_NAMES: [&str; 3] = ["c0", "c1", "c2"];

async fn acquire_serial() -> tokio::sync::MutexGuard<'static, ()> {
    tokio::time::timeout(SERIAL_ACQUIRE_TIMEOUT, SCENARIO_SERIAL.lock())
        .await
        .expect("acquire the scenario serialization lock in time")
}

/// One fully drained scenario: 3 clients (creator + 2 joiners by room code),
/// run to process exit.
struct ScenarioRun {
    /// Player id strings, indexed like [`CLIENT_NAMES`] (creator first).
    ids: Vec<String>,
    /// Complete stdout event logs, same indexing.
    logs: Vec<Vec<Value>>,
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

    /// The client's pre-teardown event window (see [`scenario_window`]).
    fn window(&self, index: usize) -> &[Value] {
        scenario_window(&self.logs[index])
    }
}

fn relay_payload_for(name: &str) -> String {
    format!("relay-from-{name}")
}

/// Per-client knobs for [`run_three_clients`]; everything else (names, the
/// creator/joiner split, run windows) follows the fixed 3-client pattern.
struct ClientConfig {
    /// Spawn the BROWSER reference client instead of the native one.
    browser: bool,
    exchange: bool,
    /// Send (and expect) the `relay-from-<name>` relay-floor payload.
    relay: bool,
    extra_args: &'static [&'static str],
    /// Connect to `/v2/ws` instead of `/v3/ws` (for pure-v2 members).
    v2_endpoint: bool,
}

impl ClientConfig {
    /// Full exchange + relay probe on `/v3/ws`, native client.
    const fn native_full() -> Self {
        Self {
            browser: false,
            exchange: true,
            relay: true,
            extra_args: &[],
            v2_endpoint: false,
        }
    }

    /// Full exchange + relay probe on `/v3/ws`, browser client.
    const fn browser_full() -> Self {
        Self {
            browser: true,
            ..Self::native_full()
        }
    }
}

/// Spawn one client per its config (native binary or browser CLI via node).
fn spawn_configured(
    config: &ClientConfig,
    spec: &ClientSpec<'_>,
    workdir: &std::path::Path,
) -> ClientProcess {
    if config.browser {
        spawn_browser_client(spec, workdir)
    } else {
        spawn_client(spec, workdir)
    }
}

/// Drain one client to EOF + reap it; it must exit 0 AND report that via its
/// final `exiting` event.
async fn drain_expect_success(client: &mut ClientProcess) {
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

    let mut creator = spawn_configured(
        &configs[0],
        &ClientSpec {
            name: CLIENT_NAMES[0],
            server_url: &urls[0],
            game_name,
            join_code: None,
            peers: 3,
            exchange: configs[0].exchange,
            relay_payload: payloads[0].as_deref(),
            extra_args: configs[0].extra_args,
        },
        workdir.path(),
    );
    // The creator's stdout supplies the room code for the joiners.
    let created = creator.await_event("room_created", EVENT_TIMEOUT).await;
    let room_code = str_field(&created, "room_code").to_string();

    let mut clients: Vec<ClientProcess> = vec![creator];
    for (index, name) in CLIENT_NAMES.iter().enumerate().skip(1) {
        clients.push(spawn_configured(
            &configs[index],
            &ClientSpec {
                name,
                server_url: &urls[index],
                game_name,
                join_code: Some(&room_code),
                peers: 3,
                exchange: configs[index].exchange,
                relay_payload: payloads[index].as_deref(),
                extra_args: configs[index].extra_args,
            },
            workdir.path(),
        ));
    }

    for client in &mut clients {
        drain_expect_success(client).await;
    }

    let ids: Vec<String> = clients
        .iter()
        .map(|client| player_id_of(&client.events, &client.name))
        .collect();
    let distinct: BTreeSet<&String> = ids.iter().collect();
    assert_eq!(distinct.len(), 3, "player ids must be distinct: {ids:?}");

    ScenarioRun {
        ids,
        // `ClientProcess` implements `Drop`, so take the log instead of
        // moving the field out (the drained child is already reaped).
        logs: clients
            .into_iter()
            .map(|mut client| std::mem::take(&mut client.events))
            .collect(),
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

/// Assert the single overall `TransportStatus{webrtc, true}` report (pass the
/// FULL log: exactly-once and never-fallback must hold beyond the window).
fn assert_transport_status_true(full_log: &[Value], who: &str) {
    let status = single_event(full_log, "transport_status_sent", who);
    assert_eq!(str_field(status, "transport"), "webrtc", "{who} transport");
    assert_eq!(
        status.get("connected").and_then(Value::as_bool),
        Some(true),
        "{who}: overall webrtc status must resolve connected"
    );
    assert!(
        events_named(full_log, "fallback_engaged").is_empty(),
        "{who}: a fully connected session must not engage the fallback"
    );
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

/// Parse a player-id string as a UUID (for Appendix E glare-rule assertions).
fn uuid_of(id: &str) -> Uuid {
    Uuid::parse_str(id).unwrap_or_else(|error| panic!("player id {id} is not a UUID: {error}"))
}

/// The full mesh-N=3 assertion pack shared by scenarios 1, 2, and 5: glare
/// matrix (total + antisymmetric + UUID rule), all pairs, the 12-message
/// channel matrix, Appendix G statuses + fan-outs, and the live relay floor.
fn assert_full_mesh_run(run: &ScenarioRun) {
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
    for a in 0..3 {
        for b in 0..3 {
            if a == b || uuid_of(&run.ids[a]) >= uuid_of(&run.ids[b]) {
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
        assert_exchange_received_from(run.window(index), who, &others);
        assert_exchange_sent_to(run.window(index), who, &others);
        // Appendix G status + fan-out.
        assert_transport_status_true(&run.logs[index], who);
        assert_peer_status_fan_out(run.window(index), who, &others_true);
        // The relay floor carried GameData in the same session window.
        assert_live_relay_floor(run, index);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mixed_mesh_n3_full_webrtc_with_browser() {
    let _serial = acquire_serial().await;
    // Native creator + native joiner + BROWSER joiner.
    let run = run_three_clients(
        "mesh",
        "binterop-mesh3",
        &[
            ClientConfig::native_full(),
            ClientConfig::native_full(),
            ClientConfig::browser_full(),
        ],
    )
    .await;
    assert_full_mesh_run(&run);
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_pair_mesh_n3() {
    let _serial = acquire_serial().await;
    // BROWSER creator (exercises the browser's room_created path) + a second
    // browser + one native member: one Chromium↔Chromium pair plus two
    // browser↔native pairs (see the module docs for the 2B+1N rationale).
    let run = run_three_clients(
        "mesh",
        "binterop-bpair3",
        &[
            ClientConfig::browser_full(),
            ClientConfig::browser_full(),
            ClientConfig::native_full(),
        ],
    )
    .await;
    assert_full_mesh_run(&run);
}

#[tokio::test(flavor = "multi_thread")]
async fn host_star_n3_browser_client() {
    let _serial = acquire_serial().await;
    // Native creator (the elected host: earliest joiner wins), one native
    // client, one BROWSER client.
    let run = run_three_clients(
        "host",
        "binterop-host3",
        &[
            ClientConfig::native_full(),
            ClientConfig::native_full(),
            ClientConfig::browser_full(),
        ],
    )
    .await;

    let host_id = run.ids[0].clone();
    let client_ids: BTreeSet<&str> = run.other_ids(0);

    // Host plan: exactly the two clients, none of which the host offers to
    // (clients offer to the host; the host answers all — Appendix E).
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

    // Client plans (the browser included): exactly the host, initiate=true.
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
    // endpoints), never client<->client — in particular, never
    // native-client<->browser-client.
    assert_pair_connected_exactly(run.window(0), CLIENT_NAMES[0], &client_ids);
    for (index, who) in CLIENT_NAMES.iter().enumerate().skip(1) {
        let host_only: BTreeSet<&str> = BTreeSet::from([host_id.as_str()]);
        assert_pair_connected_exactly(run.window(index), who, &host_only);
        assert_exchange_received_from(run.window(index), who, &host_only);
        assert_exchange_sent_to(run.window(index), who, &host_only);
        assert_transport_status_true(&run.logs[index], who);
    }
    assert_exchange_received_from(run.window(0), CLIENT_NAMES[0], &client_ids);
    assert_exchange_sent_to(run.window(0), CLIENT_NAMES[0], &client_ids);
    assert_transport_status_true(&run.logs[0], CLIENT_NAMES[0]);

    // The relay floor stays live in a star too.
    for index in 0..3 {
        assert_live_relay_floor(&run, index);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mesh_n3_browser_crippled_ice_fallback() {
    let _serial = acquire_serial().await;
    // All members run a SHORT P2P window: the crippled pairs can never
    // connect (no candidate ever crosses the signaling channel in either
    // direction), so every member resolves its overall status at this
    // deadline — the healthy native pair with one connected pair (>= 1 rule
    // => true), the crippled browser with zero (=> false + fallback). 6 s
    // comfortably covers the healthy pair's loopback establishment.
    const HEALTHY_ARGS: &[&str] = &["--p2p-timeout-secs", "6"];
    const CRIPPLED_ARGS: &[&str] = &["--p2p-timeout-secs", "6", "--cripple-ice"];
    let run = run_three_clients(
        "mesh",
        "binterop-fallback3",
        &[
            ClientConfig {
                browser: false,
                exchange: true,
                relay: true,
                extra_args: HEALTHY_ARGS,
                v2_endpoint: false,
            },
            ClientConfig {
                browser: false,
                exchange: true,
                relay: true,
                extra_args: HEALTHY_ARGS,
                v2_endpoint: false,
            },
            // The crippled BROWSER member drops candidate signals both ways;
            // no --exchange (its pairs never open).
            ClientConfig {
                browser: true,
                exchange: false,
                relay: true,
                extra_args: CRIPPLED_ARGS,
                v2_endpoint: false,
            },
        ],
    )
    .await;
    const CRIPPLED: usize = 2;

    // Finalization is unaffected by runtime ICE health: all three plans are
    // normal mesh plans listing the other two members.
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

    // The healthy native pair: exactly one connected pair each (the OTHER
    // healthy member, never the crippled browser) and the full channel
    // matrix between them.
    for (index, other) in [(0_usize, 1_usize), (1, 0)] {
        let who = CLIENT_NAMES[index];
        let other_only: BTreeSet<&str> = BTreeSet::from([run.ids[other].as_str()]);
        assert_pair_connected_exactly(run.window(index), who, &other_only);
        assert_exchange_received_from(run.window(index), who, &other_only);
        assert_exchange_sent_to(run.window(index), who, &other_only);
        // >= 1 pair connected at the deadline => overall webrtc status true.
        assert_transport_status_true(&run.logs[index], who);
    }

    // The crippled browser: ZERO pairs and zero channel traffic, exactly one
    // overall {webrtc, false} report, and the explicit fallback marker.
    let crippled_log = &run.logs[CRIPPLED];
    let crippled_name = CLIENT_NAMES[CRIPPLED];
    for name in [
        "p2p_pair_connected",
        "channel_open",
        "channel_message",
        "channel_message_sent",
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
    // healthy member's `true` AND the crippled browser's `false`; the
    // crippled browser observes two `true`s. Exactly one report per reporter.
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

    // The relay floor serves EVERY member — the crippled browser entirely.
    for index in 0..3 {
        assert_live_relay_floor(&run, index);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mesh_n3_browser_mdns_obfuscation() {
    let _serial = acquire_serial().await;
    // The browser keeps Chromium's DEFAULT mDNS host-candidate obfuscation ON
    // (`--mdns-obfuscation` suppresses the disable flag every other browser
    // cell passes): its candidates are `<uuid>.local` names. The pinned
    // empirical outcome (see module docs) is FULL connectivity via the
    // peer-reflexive path, so the assertion pack is the full mesh one —
    // including `assert_transport_status_true`, which also pins that the
    // fallback never engages.
    let run = run_three_clients(
        "mesh",
        "binterop-mdns3",
        &[
            ClientConfig::native_full(),
            ClientConfig::native_full(),
            ClientConfig {
                extra_args: &["--mdns-obfuscation"],
                ..ClientConfig::browser_full()
            },
        ],
    )
    .await;
    assert_full_mesh_run(&run);
}

#[tokio::test(flavor = "multi_thread")]
async fn mixed_v2_browser_v3_native_relay_floor() {
    let _serial = acquire_serial().await;
    // The server PREFERS mesh, but the single pure-v2 member — here the
    // BROWSER, connected to the legacy `/v2/ws` endpoint and advertising no
    // v3 fields — forces the universal relay floor for the whole room.
    let run = run_three_clients(
        "mesh",
        "binterop-mixed3",
        &[
            ClientConfig {
                browser: false,
                exchange: false,
                relay: true,
                extra_args: &[],
                v2_endpoint: false,
            },
            ClientConfig {
                browser: true,
                exchange: false,
                relay: true,
                extra_args: &["--protocol-version", "2"],
                v2_endpoint: true,
            },
            ClientConfig {
                browser: false,
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

        // Negotiation result: the v2 browser reports 2, the v3 natives 3.
        let info = single_event(full_log, "protocol_info", who);
        let expected_version = if index == V2_MEMBER { 2 } else { 3 };
        assert_eq!(
            info.get("negotiated_version").and_then(Value::as_u64),
            Some(expected_version),
            "{who}: negotiated protocol version"
        );

        // A relay-floor room emits NO v3 session traffic and no WebRTC
        // activity ever happens — assert zero across the FULL logs.
        for name in [
            "session_plan",
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
        // and the relay GameData matrix completes.
        single_event(full_log, "game_starting", who);
        assert_live_relay_floor(&run, index);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_cli_mid_handshake_close_single_error_exit_3() {
    let _serial = acquire_serial().await;
    // Stub server: complete the WebSocket upgrade, consume the client's
    // first frame (Authenticate), then close — deterministically
    // mid-handshake (the page is awaiting Authenticated).
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the stub listener");
    let addr = listener.local_addr().expect("stub listener addr");
    let stub = tokio::spawn(async move {
        let (stream, _peer) = listener
            .accept()
            .await
            .expect("accept the browser connection");
        let mut ws = tokio_tungstenite::accept_async(stream)
            .await
            .expect("complete the WebSocket upgrade");
        let _authenticate = ws.next().await;
        let _ = ws.close(None).await;
    });

    let workdir = tempfile::tempdir().expect("create client workdir");
    let url = format!("ws://{addr}/v3/ws");
    let started = std::time::Instant::now();
    let mut client = spawn_browser_client(
        &ClientSpec {
            name: "close-probe",
            server_url: &url,
            game_name: "binterop-close",
            join_code: None,
            peers: 3,
            exchange: false,
            relay_payload: None,
            extra_args: &[],
        },
        workdir.path(),
    );
    let code = client.drain_to_exit(CLIENT_EXIT_TIMEOUT).await;
    let elapsed = started.elapsed();
    assert_eq!(
        code,
        3,
        "a mid-handshake close is a connection failure (exit 3);\n{}",
        client.diagnostics()
    );
    // The non-causal shape stalls until the page's 20 s handshake-frame
    // timeout (total >= 20 s + Chromium launch); the causal path needs only
    // the launch plus one round-trip. 20 s discriminates with wide CI margin.
    assert!(
        elapsed < Duration::from_secs(20),
        "mid-handshake close took {elapsed:?}; it must surface causally, not via the handshake timeout"
    );
    let error = single_event(&client.events, "error", "close-probe");
    assert!(
        str_field(error, "message").contains("websocket closed by server"),
        "the single error event must carry the real close reason: {error}"
    );
    let exiting = single_event(&client.events, "exiting", "close-probe");
    assert_eq!(
        exiting.get("code").and_then(Value::as_i64),
        Some(3),
        "exiting event must report code 3"
    );
    // Mid-handshake means Authenticated never arrived.
    assert!(
        events_named(&client.events, "authenticated").is_empty(),
        "the stub never authenticates; the close must precede `authenticated`"
    );
    stub.await.expect("stub server task");
}

/// One `/proc/<pid>/stat` row: `(comm, state, ppid)`. `None` once the pid is
/// gone (or on a non-Linux /proc-less box, where the caller never runs).
#[cfg(target_os = "linux")]
fn proc_stat(pid: u32) -> Option<(String, char, u32)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm sits in parentheses and may itself contain spaces/parens; parse
    // around the LAST closing paren.
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let comm = stat.get(open + 1..close)?.to_string();
    let mut rest = stat.get(close + 2..)?.split(' ');
    let state = rest.next()?.chars().next()?;
    let ppid = rest.next()?.parse().ok()?;
    Some((comm, state, ppid))
}

/// Every live (non-zombie) `headless_shell` descendant of `root`, via one
/// /proc scan (scoped to THIS process tree — concurrent browsers from other
/// suites are invisible here).
#[cfg(target_os = "linux")]
fn headless_shell_descendants(root: u32) -> Vec<u32> {
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let mut by_pid: BTreeMap<u32, (String, char)> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Some((comm, state, ppid)) = proc_stat(pid) else {
            continue;
        };
        children.entry(ppid).or_default().push(pid);
        by_pid.insert(pid, (comm, state));
    }
    let mut found = Vec::new();
    let mut queue = vec![root];
    while let Some(pid) = queue.pop() {
        if let Some(kids) = children.get(&pid) {
            queue.extend(kids);
        }
        if pid == root {
            continue;
        }
        if let Some((comm, state)) = by_pid.get(&pid) {
            if comm == "headless_shell" && *state != 'Z' {
                found.push(pid);
            }
        }
    }
    found.sort_unstable();
    found
}

/// True while `pid` is still a live (non-zombie) `headless_shell` process;
/// a recycled pid running anything else reads as gone.
#[cfg(target_os = "linux")]
fn headless_shell_alive(pid: u32) -> bool {
    matches!(
        proc_stat(pid),
        Some((comm, state, _ppid)) if comm == "headless_shell" && state != 'Z'
    )
}

/// The Chromium-never-outlives-the-CLI guarantee, both halves (advertised in
/// clients/browser/README.md and ADR-0005). /proc-based, hence Linux-only —
/// the platform every browser-interop run uses.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn browser_cli_signal_teardown_reaps_chromium() {
    let _serial = acquire_serial().await;
    let server = spawn_server("mesh").await;
    let workdir = tempfile::tempdir().expect("create client workdir");

    for (signal_name, sigkill) in [("SIGTERM", false), ("SIGKILL", true)] {
        // --peers 3 with a lone member: the client idles in its room for the
        // whole 45 s soft window — a wide, stable kill window with Chromium
        // definitely up.
        let mut client = spawn_browser_client(
            &ClientSpec {
                name: "reap-target",
                server_url: &server.v3_ws_url(),
                game_name: "binterop-reap",
                join_code: None,
                peers: 3,
                exchange: false,
                relay_payload: None,
                extra_args: &[],
            },
            workdir.path(),
        );
        // Any page event proves Chromium is fully up: the page runs inside it.
        client.await_event("room_joined", EVENT_TIMEOUT).await;
        let node_pid = client.pid();
        let chromium = headless_shell_descendants(node_pid);
        assert!(
            !chromium.is_empty(),
            "{signal_name}: no live headless_shell descendant of the CLI (pid {node_pid})"
        );

        if sigkill {
            // Dropping the guard IS the harness kill-on-drop SIGKILL under
            // test: no in-process handler can trap it, so only the CLI's
            // detached reaper stands between Chromium and orphanhood.
            drop(client);
        } else {
            let status = std::process::Command::new("kill")
                .args(["-TERM", &node_pid.to_string()])
                .status()
                .expect("run kill -TERM");
            assert!(status.success(), "kill -TERM {node_pid} failed");
            let code = client.drain_to_exit(CLIENT_EXIT_TIMEOUT).await;
            assert_eq!(code, 128 + 15, "SIGTERM teardown must exit 143");
        }

        // Bounded reap window: the in-process teardown is a 5 s
        // close-then-kill; the reaper polls every 500 ms. 15 s absorbs CI lag.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let survivors: Vec<u32> = chromium
                .iter()
                .copied()
                .filter(|pid| headless_shell_alive(*pid))
                .collect();
            if survivors.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{signal_name}: headless_shell pids {survivors:?} outlived the CLI"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}
