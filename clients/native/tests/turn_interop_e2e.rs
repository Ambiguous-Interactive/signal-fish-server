//! Deterministic TURN-only interoperability against a local coturn process.
//!
//! These ignored tests are activated exclusively by
//! `scripts/run-turn-interop.sh`, which starts the repository-pinned coturn
//! image on loopback. Both clients use `RTCIceTransportPolicy::Relay`, so a
//! connected pair is proof of a TURN allocation rather than direct host ICE.

mod harness;

use std::collections::BTreeMap;

use harness::{
    advertised_candidates, events_named, player_id_of, scenario_window, single_event, spawn_client,
    spawn_server_with_turn, str_field, ClientProcess, ClientSpec, ServerProcess,
    CLIENT_EXIT_TIMEOUT, EVENT_TIMEOUT,
};
use serde_json::Value;

const TURN_URL_ENV: &str = "SIGNAL_FISH_TURN_INTEROP_URL";
const TURN_SECRET_ENV: &str = "SIGNAL_FISH_TURN_INTEROP_SECRET";
const CLIENT_NAMES: [&str; 2] = ["c0", "c1"];
const BAD_SECRET_ARGS: [&str; 4] = ["--ice-transport-policy", "relay", "--p2p-timeout-secs", "5"];

static SCENARIO_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct TurnRun {
    ids: [String; 2],
    logs: [Vec<Value>; 2],
    stderr: [String; 2],
}

struct ActiveTurnRun {
    server: ServerProcess,
    clients: [ClientProcess; 2],
    _workdir: tempfile::TempDir,
}

fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!("{name} is required; run this ignored test through scripts/run-turn-interop.sh")
    })
}

async fn drain_expect_success(client: &mut ClientProcess) {
    let code = client.drain_to_exit(CLIENT_EXIT_TIMEOUT).await;
    assert_eq!(
        code,
        0,
        "client {} exited nonzero;\n{}",
        client.name,
        client.diagnostics()
    );
    assert_eq!(
        single_event(&client.events, "exiting", &client.name)
            .get("code")
            .and_then(Value::as_i64),
        Some(0),
        "client {} must report a successful exit",
        client.name
    );
}

async fn spawn_two_clients(
    server_secret: &str,
    exchange: bool,
    extra_args: &[&str],
) -> ActiveTurnRun {
    let turn_url = required_env(TURN_URL_ENV);
    let server = spawn_server_with_turn("mesh", &turn_url, server_secret).await;
    let workdir = tempfile::tempdir().expect("create client workdir");
    let relay_payloads = ["turn-relay-floor-c0", "turn-relay-floor-c1"];

    let mut creator = spawn_client(
        &ClientSpec {
            name: CLIENT_NAMES[0],
            server_url: &server.v3_ws_url(),
            game_name: "turn-only-interop",
            join_code: None,
            peers: 2,
            exchange,
            relay_payload: Some(relay_payloads[0]),
            extra_args,
        },
        workdir.path(),
    );
    let created = creator.await_event("room_created", EVENT_TIMEOUT).await;
    let room_code = str_field(&created, "room_code").to_string();
    let joiner = spawn_client(
        &ClientSpec {
            name: CLIENT_NAMES[1],
            server_url: &server.v3_ws_url(),
            game_name: "turn-only-interop",
            join_code: Some(&room_code),
            peers: 2,
            exchange,
            relay_payload: Some(relay_payloads[1]),
            extra_args,
        },
        workdir.path(),
    );

    ActiveTurnRun {
        server,
        clients: [creator, joiner],
        _workdir: workdir,
    }
}

async fn finish_two_clients(mut active: ActiveTurnRun) -> TurnRun {
    for client in &mut active.clients {
        drain_expect_success(client).await;
    }
    active.server.shutdown().await;
    let ids = [
        player_id_of(&active.clients[0].events, &active.clients[0].name),
        player_id_of(&active.clients[1].events, &active.clients[1].name),
    ];
    assert_ne!(ids[0], ids[1], "the two clients need distinct identities");
    TurnRun {
        ids,
        logs: [
            active.clients[0].events.clone(),
            active.clients[1].events.clone(),
        ],
        stderr: [
            active.clients[0].stderr_text(),
            active.clients[1].stderr_text(),
        ],
    }
}

fn assert_turn_plan(events: &[Value], who: &str) {
    let plan = single_event(events, "session_plan", who);
    assert_eq!(str_field(plan, "topology"), "mesh");
    assert_eq!(str_field(plan, "transport"), "webrtc");
    assert_eq!(str_field(plan, "fallback"), "relay");
    assert_eq!(
        plan.get("ice_servers_count").and_then(Value::as_u64),
        Some(1),
        "{who}: production SessionPlan must contain exactly the local TURN server"
    );
}

/// Under `--ice-transport-policy relay` the only candidate a client may
/// advertise is one a TURN allocation produced.
///
/// This is the oracle issue #276 lacked. Run 30962028644 gathered nothing —
/// every Allocate left from a socket that could not route to the coturn
/// container — and the only symptom the cell could report was
/// "expected exactly one `p2p_pair_connected` event, got 0: []", with no
/// evidence naming the cause. Asserting the advertised set makes a failed
/// allocation self-describing, and forbids a host or reflexive candidate from
/// silently carrying a run this lane exists to prove relayed.
fn assert_only_relay_candidates_advertised(events: &[Value], who: &str) {
    let advertised = advertised_candidates(events);
    assert!(
        !advertised.is_empty(),
        "{who}: the relay-only session advertised no ICE candidate at all, so the \
         TURN allocation produced none (issue #276); the client's stderr carries \
         the allocation errors"
    );
    assert!(
        advertised.iter().all(|entry| entry.starts_with("relay ")),
        "{who}: --ice-transport-policy relay must advertise relay candidates only, \
         got {advertised:?}"
    );
}

fn assert_relay_floor(run: &TurnRun, index: usize) {
    let events = scenario_window(&run.logs[index]);
    single_event(events, "game_data_sent", CLIENT_NAMES[index]);
    let expected_sender = run.ids[1 - index].as_str();
    let expected_payload = format!("turn-relay-floor-c{}", 1 - index);
    let received = events_named(events, "game_data_received");
    assert_eq!(received.len(), 1, "exactly one relay-floor frame expected");
    assert_eq!(str_field(received[0], "from"), expected_sender);
    assert_eq!(
        received[0]
            .get("payload")
            .and_then(|payload| payload.get("relay_msg"))
            .and_then(Value::as_str),
        Some(expected_payload.as_str()),
        "relay-floor payload must remain exact"
    );
}

fn assert_exact_channel_ledger(run: &TurnRun, index: usize) {
    let events = scenario_window(&run.logs[index]);
    let peer = run.ids[1 - index].as_str();
    for event_name in ["channel_message_sent", "channel_message"] {
        let entries = events_named(events, event_name);
        let mut labels: BTreeMap<&str, usize> = BTreeMap::new();
        for event in entries {
            assert_eq!(str_field(event, "peer"), peer);
            let label = str_field(event, "label");
            let payload: Value =
                serde_json::from_str(str_field(event, "text")).unwrap_or_else(|error| {
                    panic!("invalid {event_name} JSON payload: {error}: {event}")
                });
            let expected_sender = if event_name == "channel_message_sent" {
                run.ids[index].as_str()
            } else {
                peer
            };
            assert_eq!(str_field(&payload, "from"), expected_sender);
            assert_eq!(str_field(&payload, "channel"), label);
            assert_eq!(payload.get("seq").and_then(Value::as_u64), Some(0));
            *labels.entry(label).or_default() += 1;
        }
        assert_eq!(
            labels,
            BTreeMap::from([("reliable", 1), ("unreliable", 1)]),
            "{}: exact reliable/unreliable {event_name} ledger",
            CLIENT_NAMES[index]
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires pinned local coturn; use scripts/run-turn-interop.sh"]
async fn turn_only_pair_selects_relay_candidates_and_keeps_websocket_floor_live() {
    let _serial = SCENARIO_SERIAL.lock().await;
    let secret = required_env(TURN_SECRET_ENV);
    let release_dir = tempfile::tempdir().expect("create P2P gate directory");
    let release_path = release_dir.path().join("release-p2p");
    let release_arg = release_path.display().to_string();
    let args = [
        "--ice-transport-policy",
        "relay",
        "--disable-mdns",
        "--p2p-timeout-secs",
        "30",
        "--p2p-release-file",
        release_arg.as_str(),
    ];
    let mut active = spawn_two_clients(&secret, true, &args).await;

    // The pair-creation gate is still closed: prove the relay floor completes
    // in both directions while there cannot yet be an ICE connection.
    for client in &mut active.clients {
        client
            .await_event_or_recorded("game_data_sent", EVENT_TIMEOUT)
            .await;
        client
            .await_event_or_recorded("game_data_received", EVENT_TIMEOUT)
            .await;
        assert!(events_named(&client.events, "signal_sent").is_empty());
        assert!(events_named(&client.events, "p2p_pair_connected").is_empty());
    }
    std::fs::write(&release_path, b"release").expect("release P2P establishment");
    for client in &mut active.clients {
        let released = client
            .await_event_or_recorded("p2p_gate_released", EVENT_TIMEOUT)
            .await;
        assert_eq!(
            released.get("pending_pairs").and_then(Value::as_u64),
            Some(1),
            "{} must release exactly its one planned TURN pair",
            client.name
        );
    }
    let run = finish_two_clients(active).await;

    for (index, who) in CLIENT_NAMES.iter().copied().enumerate() {
        let events = scenario_window(&run.logs[index]);
        assert_turn_plan(events, who);
        assert_only_relay_candidates_advertised(events, who);
        single_event(events, "p2p_pair_connected", who);
        let selected = single_event(events, "selected_candidate_pair", who);
        assert_eq!(str_field(selected, "peer"), run.ids[1 - index]);
        assert_eq!(str_field(selected, "local_candidate_type"), "relay");
        assert_eq!(str_field(selected, "remote_candidate_type"), "relay");
        assert!(
            events_named(events, "signal_sent")
                .iter()
                .all(|event| str_field(event, "kind") != "pair_retry"),
            "{who}: TURN proof must connect on its first pairing attempt"
        );
        assert_exact_channel_ledger(&run, index);
        assert_relay_floor(&run, index);
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires pinned local coturn; use scripts/run-turn-interop.sh"]
async fn mismatched_turn_secret_fails_p2p_and_uses_websocket_fallback() {
    let _serial = SCENARIO_SERIAL.lock().await;
    let valid_secret = required_env(TURN_SECRET_ENV);
    let bad_secret = format!("{valid_secret}-mismatch");
    let active = spawn_two_clients(&bad_secret, false, &BAD_SECRET_ARGS).await;
    let run = finish_two_clients(active).await;

    for (index, who) in CLIENT_NAMES.iter().copied().enumerate() {
        let events = scenario_window(&run.logs[index]);
        assert_turn_plan(events, who);
        assert!(events_named(events, "p2p_pair_connected").is_empty());
        assert!(events_named(events, "selected_candidate_pair").is_empty());
        // The complement of the positive control: a rejected allocation yields
        // no relay candidate, and the relay policy admits no other kind. An
        // advertised candidate here would mean the fallback was reached with a
        // usable path still in hand.
        assert!(
            advertised_candidates(events).is_empty(),
            "{who}: a rejected TURN allocation must advertise no candidate, got {:?}",
            advertised_candidates(events)
        );
        let status = single_event(events, "transport_status_sent", who);
        assert_eq!(str_field(status, "transport"), "webrtc");
        assert_eq!(
            status.get("connected").and_then(Value::as_bool),
            Some(false)
        );
        single_event(events, "fallback_engaged", who);
        assert_relay_floor(&run, index);
        assert!(
            run.stderr[index].contains("TURN allocation failed")
                && run.stderr[index].contains("Allocate error response (error 401:"),
            "{who}: mismatched secret must fail at TURN allocation authentication: {}",
            run.stderr[index]
        );
        assert!(
            !run.stderr[index].contains("no route to host")
                && !run.stderr[index].contains("Network is unreachable")
                && !run.stderr[index].contains("connection refused"),
            "{who}: negative control must not pass through a network outage"
        );
    }
}
