//! True multi-process delivery-contract tests (issue #131): a REAL
//! `signal-fish-server` child process, REAL client processes, and OS-level
//! faults (SIGSTOP / SIGKILL) that no in-process suite can express.
//!
//! The in-process burst/chaos suites (`tests/relay_backpressure_e2e.rs`,
//! `tests/relay_chaos_e2e.rs`) already pin "zero loss or loud disconnect"
//! against sockets that stop draining or die. What only a process boundary
//! can add:
//!
//! - **A genuinely frozen peer** (`SIGSTOP`): the whole client process — its
//!   reactor, its reads, its keepalives — stops, so the kernel receive buffer
//!   fills exactly the way a livelocked/backgrounded real client's would.
//! - **A frozen signaling server** (`SIGSTOP` then `SIGCONT`): ingress crosses
//!   a real process-wide blind interval and the resumed v3 stream must retain
//!   exact stamps, payloads, and causal gap accounting.
//! - **A vanished peer** (`SIGKILL` on the client): the kernel tears the
//!   socket down with no cooperation from the process.
//! - **A vanished server** (`SIGKILL` on the server): clients must observe
//!   the death promptly, the loss must be exactly the in-flight tail (a
//!   gap-free received prefix, never a hole), and a fresh process on the same
//!   port must serve a fresh session.
//!
//! # Infrastructure
//!
//! The server-spawning harness is the shared
//! `websocket_test_helpers::server_process` (its module doc explains the
//! temp-config / port-reservation / readiness / kill-on-drop design); this
//! suite drives it with a config overlay carrying per-test
//! `websocket.send_queue_capacity` / `websocket.slow_consumer_timeout_ms`
//! knobs. Because the server runs in a
//! separate process, delivery metrics are asserted by scraping its
//! `/metrics/prom` endpoint (`websocket_test_helpers::prometheus_scrape`;
//! the temp config disables metrics auth) — the same counters the in-process
//! suites read from `ServerMetrics`.
//!
//! Client processes are the NATIVE REFERENCE CLIENT
//! (`clients/native`, binary `signal-fish-reference-native`): it speaks the
//! full v3 flow, keeps reading relay traffic for its whole `--run-for-secs`
//! window, and emits a machine-readable JSONL event stream this suite asserts
//! on (`room_joined`, `game_data_received`, `error`, `exiting`). It lives in
//! a SEPARATE crate, so `CARGO_BIN_EXE_*` cannot locate it from here: the
//! binary comes from the `SIGNAL_FISH_CLIENT_BIN` env var when set (the
//! nightly workflow pre-builds it), falling back to a once-per-process
//! `cargo build --manifest-path clients/native/Cargo.toml` (the client crate
//! is its own workspace with its own target dir, so the nested build cannot
//! deadlock against this one). The three tests that need the client binary are
//! `#[ignore]`d as nightly-only precisely because that fallback build (a full
//! webrtc dependency graph) is too heavy for the PR lane; the raw-client
//! server-restart test runs in the PR lane.
//!
//! Direct raw `tokio_tungstenite` clients (in this test process) are used
//! wherever the test needs exact socket control: flooding senders, gap-free
//! drain assertions, and observing the server's death. The mix per test is
//! documented on each test.
//!
//! The process-fault cases are Unix-only (`kill -STOP` / `-CONT` / `-KILL`),
//! and the resume oracle registers an explicit ignored stub on unsupported
//! targets. Deadlines follow the
//! zero-flakiness policy: generous saturation-tolerant CEILINGS polled
//! against real state (scraped metrics, JSONL events, socket closes), never
//! sleeps as synchronization. Under nextest this binary runs in the bounded
//! `process-spawning` group (`.config/nextest.toml`); `#[serial_test::serial]`
//! keeps plain `cargo test` from co-scheduling the floods in one process.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

// This shared harness also exposes WebRTC-only observation helpers. They are
// intentionally unused by the delivery-fault consumer of the module.
#[allow(dead_code)]
#[path = "websocket_test_helpers/native_client_process.rs"]
mod native_client_process;
mod websocket_test_helpers;

use std::collections::BTreeSet;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use native_client_process::{spawn_native_client, NativeClientProcess};
use serde_json::{json, Value};
use signal_fish_server::protocol::{
    ClientMessage, DeliveryClass, DeliveryGap, DeliveryGapReason, DeliveryReportPayload, PlayerId,
    ServerMessage,
};
#[cfg(target_os = "linux")]
use signal_fish_server::protocol::{Topology, Transport};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::delivery_ledger::{extract, LedgerPayload};
use websocket_test_helpers::prometheus_scrape::{
    assert_scraped_message_conservation, scrape_delivery_counters,
};
use websocket_test_helpers::server_process::{
    spawn_server, spawn_server_on_fixed_port, CONNECT_TIMEOUT,
};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
type WsReceiver = futures_util::stream::SplitStream<WsStream>;

const GAME_NAME: &str = "mp-delivery";

// Saturation-tolerant ceilings (zero-flakiness policy, .llm/context-testing.md):
// every wait below polls real state and returns the instant it holds; these
// only bite when something is genuinely broken, so their size never slows a
// passing run. (The server-spawn / connect / health ceilings live with the
// harness in `websocket_test_helpers::server_process`.)
const EVENT_DEADLINE: Duration = Duration::from_secs(30);
/// Whole-phase ceiling for flood + drain sections.
const PHASE_DEADLINE: Duration = Duration::from_secs(90);
/// Ceiling for a client process to drain its JSONL stream and exit.
const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(60);

/// Native-client exit code for transport-level failure (socket died); see
/// `clients/native/src/client.rs` (`EXIT_CONNECTION_ERROR`).
const CLIENT_EXIT_CONNECTION_ERROR: i32 = 3;

/// Per-test websocket delivery knobs written into the child's temp config.
#[derive(Debug, Clone, Copy)]
struct DeliveryKnobs {
    send_queue_capacity: usize,
    slow_consumer_timeout_ms: u64,
    max_sojourn_ms: u64,
    socket_send_buffer_bytes: u32,
}

/// This suite's config overlay, deep-merged over the shared harness's base
/// config (`websocket_test_helpers::server_process`): the relay default
/// topology plus this test's websocket delivery knobs. Metrics auth stays
/// disabled by the base config, so the tests can scrape `/metrics/prom`.
fn config_overlay(knobs: DeliveryKnobs) -> Value {
    json!({
        "session": {
            "default_topology": "relay"
        },
        "websocket": {
            "send_queue_capacity": knobs.send_queue_capacity,
            "slow_consumer_timeout_ms": knobs.slow_consumer_timeout_ms,
            "max_sojourn_ms": knobs.max_sojourn_ms,
            "socket_send_buffer_bytes": knobs.socket_send_buffer_bytes
        }
    })
}

/// Spawn one native reference client that joins `room_code` and lingers,
/// reading relay traffic, for the given soft window. `peers` is set to a
/// count the room never reaches a full lobby with, so the client stays a
/// quiet relay recipient for its whole run.
fn spawn_lingering_client(
    server_port: u16,
    room_code: &str,
    name: &str,
    peers: usize,
    run_for_secs: u64,
    workdir: &std::path::Path,
) -> NativeClientProcess {
    let args = vec![
        "--server-url".to_string(),
        format!("ws://127.0.0.1:{server_port}/v3/ws"),
        "--join-code".to_string(),
        room_code.to_string(),
        "--game-name".to_string(),
        GAME_NAME.to_string(),
        "--player-name".to_string(),
        name.to_string(),
        "--peers".to_string(),
        peers.to_string(),
        "--run-for-secs".to_string(),
        run_for_secs.to_string(),
        "--max-runtime-secs".to_string(),
        (run_for_secs + 60).to_string(),
    ];
    spawn_native_client(name, &args, workdir)
}

/// Deliver `signal` (e.g. "-STOP") to `pid` via the portable `kill` binary —
/// dependency-free process fault injection (no `nix`/`libc` needed).
fn signal_process(pid: u32, signal: &str, context: &str) {
    let status = std::process::Command::new("kill")
        .arg(signal)
        .arg(pid.to_string())
        .status()
        .unwrap_or_else(|error| panic!("{context}: failed to run `kill {signal} {pid}`: {error}"));
    assert!(
        status.success(),
        "{context}: `kill {signal} {pid}` exited with {status}"
    );
}

#[cfg(target_os = "linux")]
async fn poll_linux_process_stopped(pid: u32, expected_stopped: bool, context: &str) {
    let deadline = tokio::time::Instant::now() + EVENT_DEADLINE;
    loop {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status"))
            .unwrap_or_else(|error| panic!("{context}: read /proc/{pid}/status: {error}"));
        let state = status
            .lines()
            .find_map(|line| line.strip_prefix("State:"))
            .map(str::trim)
            .unwrap_or_else(|| panic!("{context}: /proc/{pid}/status has no State line"));
        let stopped = state.starts_with('T') || state.starts_with('t');
        if stopped == expected_stopped {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{context}: process state never became {} (last State: {state})",
            if expected_stopped {
                "stopped"
            } else {
                "running"
            }
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ---------------------------------------------------------------------------
// Raw in-test client helpers (direct socket control; auth is disabled in the
// temp config, so — exactly like the in-process relay suites — the raw
// clients join without an Authenticate handshake).
// ---------------------------------------------------------------------------

async fn connect_raw(port: u16) -> (WsSink, WsReceiver) {
    connect_raw_version(port, 2).await
}

async fn connect_raw_version(port: u16, version: u16) -> (WsSink, WsReceiver) {
    let url = format!("ws://127.0.0.1:{port}/v{version}/ws");
    let (stream, _response) = tokio::time::timeout(CONNECT_TIMEOUT, connect_async(&url))
        .await
        .expect("websocket connect timed out")
        .expect("websocket connect failed");
    stream.split()
}

#[cfg(target_os = "linux")]
async fn send_raw(sink: &mut WsSink, message: &ClientMessage) {
    let json = serde_json::to_string(message).expect("serialize client message");
    sink.send(Message::Text(json.into()))
        .await
        .expect("send client message");
}

#[cfg(target_os = "linux")]
async fn authenticate_raw_v3(sink: &mut WsSink, receiver: &mut WsReceiver) {
    send_raw(
        sink,
        &ClientMessage::Authenticate {
            app_id: "reference-native-app".to_string(),
            sdk_version: None,
            platform: Some("multiprocess-resume-test".to_string()),
            game_data_format: None,
            protocol_version: Some(3),
            supported_transports: Some(vec![Transport::Relay]),
            supported_topologies: Some(vec![Topology::Relay]),
            requested_capabilities: None,
        },
    )
    .await;
    let mut authenticated = false;
    let mut negotiated = false;
    while !(authenticated && negotiated) {
        let frame = tokio::time::timeout(EVENT_DEADLINE, receiver.next())
            .await
            .expect("timed out authenticating raw v3 client")
            .expect("raw v3 connection closed during authentication")
            .expect("raw v3 websocket failed during authentication");
        let Message::Text(text) = frame else { continue };
        match serde_json::from_str::<ServerMessage>(&text).expect("valid authentication message") {
            ServerMessage::Authenticated { .. } => authenticated = true,
            ServerMessage::ProtocolInfo(info) => {
                assert_eq!(info.protocol_version, Some(3));
                negotiated = true;
            }
            ServerMessage::AuthenticationError { error, .. } => {
                panic!("raw v3 authentication failed: {error}")
            }
            _ => {}
        }
    }
}

/// Join `room_code` and return the server-assigned player id.
async fn join_room_raw(
    sink: &mut WsSink,
    receiver: &mut WsReceiver,
    room_code: &str,
    player_name: &str,
) -> PlayerId {
    let join = ClientMessage::JoinRoom {
        game_name: GAME_NAME.to_string(),
        room_code: Some(room_code.to_string()),
        player_name: player_name.to_string(),
        max_players: Some(8),
        supports_authority: Some(false),
        relay_transport: None,
    };
    let json = serde_json::to_string(&join).expect("serialize JoinRoom");
    sink.send(Message::Text(json.into()))
        .await
        .expect("send JoinRoom");

    loop {
        let frame = tokio::time::timeout(EVENT_DEADLINE, receiver.next())
            .await
            .expect("timed out waiting for RoomJoined")
            .expect("connection closed while joining room")
            .expect("websocket error while joining room");
        let Message::Text(text) = frame else {
            continue;
        };
        let message: ServerMessage = serde_json::from_str(&text).expect("valid ServerMessage");
        match message {
            ServerMessage::RoomJoined(payload) => return payload.player_id,
            ServerMessage::RoomJoinFailed { reason, .. } => {
                panic!("room join failed for {player_name}: {reason}")
            }
            _ => continue,
        }
    }
}

/// Send `count` ledger-tracked GameData messages on `sink`.
async fn send_burst(sink: &mut WsSink, payload: &mut LedgerPayload, count: u64) {
    for _ in 0..count {
        let message = ClientMessage::GameData {
            class: None,
            key: None,
            data: payload.next(),
        };
        let json = serde_json::to_string(&message).expect("serialize GameData");
        sink.send(Message::Text(json.into()))
            .await
            .expect("send GameData burst frame");
    }
}

fn assert_complete_and_ordered(seqs: &[u64], expected: u64, who: &str) {
    assert_eq!(
        seqs.len() as u64,
        expected,
        "{who}: expected the complete stream ({expected} messages), got {}",
        seqs.len()
    );
    assert_gap_free_prefix(seqs, who);
}

/// Assert `seqs` is the gap-free in-order prefix `0..len` — the only legal
/// shape for a receiver cut off mid-stream (loss is only ever the tail).
fn assert_gap_free_prefix(seqs: &[u64], who: &str) {
    for (position, seq) in seqs.iter().enumerate() {
        assert_eq!(
            *seq, position as u64,
            "{who}: stream is not a gap-free in-order prefix (position {position} carries \
             seq {seq}) — a mid-stream hole means the relay silently dropped messages"
        );
    }
}

/// Poll the scraped metrics until `condition` holds, failing loudly at the
/// ceiling. A ceiling, not an expected wait.
async fn poll_scrape_until(
    port: u16,
    context: &str,
    mut condition: impl FnMut(&websocket_test_helpers::prometheus_scrape::DeliveryCounters) -> bool,
) {
    let deadline = tokio::time::Instant::now() + EVENT_DEADLINE;
    loop {
        let counters = scrape_delivery_counters(port).await;
        if condition(&counters) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{context}: condition never held within {EVENT_DEADLINE:?}; last scrape: {counters:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Extract this client's `player_id` from its `room_joined` JSONL event.
fn player_id_from_room_joined(event: &Value, who: &str) -> PlayerId {
    let raw = event
        .get("player_id")
        .cloned()
        .unwrap_or_else(|| panic!("{who}: room_joined event without player_id: {event}"));
    serde_json::from_value(raw)
        .unwrap_or_else(|error| panic!("{who}: room_joined player_id is not a PlayerId: {error}"))
}

/// Drain task body shared by the SIGSTOP/SIGKILL scenarios: read the healthy
/// receiver until (a) the sender's final total is known via `totals_rx`,
/// (b) every seq up to it has been recorded, and (c) the faulted peer's
/// `PlayerLeft` was observed. Returns the recorded seqs in arrival order.
async fn drain_healthy_until_complete(
    mut receiver: WsReceiver,
    mut totals_rx: tokio::sync::watch::Receiver<Option<u64>>,
    faulted_peer: PlayerId,
) -> Vec<u64> {
    let mut seqs: Vec<u64> = Vec::new();
    let mut saw_faulted_leave = false;
    loop {
        let total = *totals_rx.borrow_and_update();
        if let Some(total) = total {
            if seqs.len() as u64 >= total && saw_faulted_leave {
                return seqs;
            }
        }
        tokio::select! {
            frame = receiver.next() => {
                let frame = frame
                    .expect("healthy receiver closed mid-drain")
                    .expect("healthy receiver websocket error mid-drain");
                let Message::Text(text) = frame else { continue };
                let message: ServerMessage =
                    serde_json::from_str(&text).expect("valid ServerMessage");
                match message {
                    ServerMessage::GameData { data, .. } => {
                        let (_sender, seq) = extract(&data).unwrap_or_else(|| {
                            panic!("healthy receiver: GameData without ledger fields: {data}")
                        });
                        seqs.push(seq);
                    }
                    ServerMessage::PlayerLeft { player_id, .. } if player_id == faulted_peer => {
                        saw_faulted_leave = true;
                    }
                    ServerMessage::Error { message, error_code } => panic!(
                        "healthy receiver got a server error mid-drain: {message} ({error_code:?})"
                    ),
                    _ => {}
                }
            }
            changed = totals_rx.changed() => {
                changed.expect("sent-totals channel dropped before totals were set");
            }
        }
    }
}

#[derive(Debug, Clone)]
enum DeliveryObservation {
    Data {
        from: PlayerId,
        epoch: u32,
        seq: u64,
        class: DeliveryClass,
        key: Option<u32>,
        payload: Value,
    },
    Gap(DeliveryGap),
    Lifecycle {
        epoch: u32,
        prior_final_seq: u64,
    },
}

/// Independent test-side oracle for one sender's causal v3 observation stream.
/// Production `DeliveryAccountability` validates the client; this deliberately
/// separate implementation prevents the nightly test from proving itself with
/// the same code under test.
fn validate_delivery_observations(
    observations: &[DeliveryObservation],
    expected_sender: PlayerId,
    initial_epoch: u32,
    terminal_epoch: u32,
    terminal_seq: u64,
    expected_classes: &[DeliveryClass],
    expected_payloads: &[Value],
) -> Result<(), String> {
    if terminal_epoch != initial_epoch {
        return Err("terminal epoch changed in a fixed-lifecycle scenario".to_string());
    }
    if usize::try_from(terminal_seq).ok() != Some(expected_classes.len()) {
        return Err("expected-class schedule does not end at terminal sequence".to_string());
    }
    if expected_payloads.len() != expected_classes.len() {
        return Err("expected payload and class schedules differ in length".to_string());
    }
    let epoch = initial_epoch;
    let mut next_seq = 1u64;
    let mut authorized_gaps = BTreeSet::new();

    let require_complete_through =
        |through: u64, next_seq: &mut u64, gaps: &mut BTreeSet<u64>| -> Result<(), String> {
            if through < next_seq.saturating_sub(1) {
                return Err("terminal marker regressed behind observed frontier".to_string());
            }
            while *next_seq <= through {
                if !gaps.remove(next_seq) {
                    return Err(format!("incomplete terminal evidence at seq {}", *next_seq));
                }
                *next_seq += 1;
            }
            Ok(())
        };

    for observation in observations {
        match observation {
            DeliveryObservation::Gap(gap) => {
                if gap.from_player != expected_sender {
                    return Err("gap names wrong sender".to_string());
                }
                if gap.epoch != epoch {
                    return Err("gap names wrong epoch".to_string());
                }
                if gap.from_seq == 0 || gap.from_seq > gap.to_seq {
                    return Err("gap has invalid range".to_string());
                }
                if gap.from_seq < next_seq {
                    return Err("gap arrived behind observed frontier".to_string());
                }
                for seq in gap.from_seq..=gap.to_seq {
                    let expected_class = expected_classes
                        .get(usize::try_from(seq - 1).map_err(|_| "gap seq exceeds usize")?)
                        .ok_or_else(|| "gap range extends beyond terminal marker".to_string())?;
                    let compatible = matches!(
                        (expected_class, gap.reason),
                        (
                            DeliveryClass::Latest,
                            DeliveryGapReason::LatestSuperseded
                                | DeliveryGapReason::LatestDroppedFull
                        ) | (DeliveryClass::Volatile, DeliveryGapReason::VolatileDropped)
                    );
                    if !compatible {
                        return Err(
                            "gap reason is incompatible with sent delivery class".to_string()
                        );
                    }
                    if !authorized_gaps.insert(seq) {
                        return Err("gap ranges overlap or duplicate".to_string());
                    }
                }
            }
            DeliveryObservation::Data {
                from,
                epoch: observed_epoch,
                seq,
                class,
                key,
                payload,
            } => {
                if *from != expected_sender {
                    return Err("data names wrong sender".to_string());
                }
                if *observed_epoch < epoch {
                    return Err("data epoch regressed".to_string());
                }
                if *observed_epoch > epoch {
                    return Err("data epoch advanced without lifecycle".to_string());
                }
                if *seq == 0 || *seq < next_seq {
                    return Err("data sequence duplicated or regressed".to_string());
                }
                let expected_class = expected_classes
                    .get(usize::try_from(*seq - 1).map_err(|_| "data seq exceeds usize")?)
                    .ok_or_else(|| "data sequence exceeds terminal marker".to_string())?;
                if class != expected_class {
                    return Err("data class differs from sent schedule".to_string());
                }
                if (*class == DeliveryClass::Latest) != key.is_some() {
                    return Err("data key is incompatible with delivery class".to_string());
                }
                if payload
                    != &expected_payloads
                        [usize::try_from(*seq - 1).map_err(|_| "data seq exceeds usize")?]
                {
                    return Err("data payload differs from sent schedule".to_string());
                }
                while next_seq < *seq {
                    if !authorized_gaps.remove(&next_seq) {
                        return Err(format!("silent hole at seq {next_seq}"));
                    }
                    next_seq += 1;
                }
                if authorized_gaps.remove(seq) {
                    return Err("delivered sequence was already reported missing".to_string());
                }
                next_seq += 1;
            }
            DeliveryObservation::Lifecycle {
                epoch: next_epoch,
                prior_final_seq,
            } => {
                if *next_epoch <= epoch {
                    return Err("lifecycle epoch regressed or duplicated".to_string());
                }
                if *prior_final_seq < next_seq.saturating_sub(1) {
                    return Err(
                        "lifecycle terminal marker regressed behind observed frontier".to_string(),
                    );
                }
                if !authorized_gaps.is_empty() {
                    return Err("gap authorization extends beyond lifecycle marker".to_string());
                }
                return Err("unexpected lifecycle advance in fixed-lifecycle scenario".to_string());
            }
        }
    }

    require_complete_through(terminal_seq, &mut next_seq, &mut authorized_gaps)?;
    if !authorized_gaps.is_empty() {
        return Err("gap range extends beyond terminal marker".to_string());
    }
    Ok(())
}

fn observations_from_native_events(
    events: &[Value],
    expected_sender: PlayerId,
) -> Vec<DeliveryObservation> {
    let mut observations = Vec::new();
    for event in events {
        match event.get("event").and_then(Value::as_str) {
            Some("delivery_report") => {
                let report: DeliveryReportPayload = serde_json::from_value(
                    event
                        .get("report")
                        .cloned()
                        .expect("delivery_report event carries report"),
                )
                .expect("delivery_report event matches protocol payload");
                observations.extend(report.gaps.into_iter().map(DeliveryObservation::Gap));
            }
            Some("game_data_received") => {
                let from = event
                    .get("from")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<PlayerId>().ok())
                    .expect("GameData event carries a player id");
                let class: DeliveryClass = serde_json::from_value(
                    event
                        .get("class")
                        .cloned()
                        .expect("v3 GameData carries class"),
                )
                .expect("GameData class uses a protocol token");
                observations.push(DeliveryObservation::Data {
                    from,
                    epoch: event
                        .get("epoch")
                        .and_then(Value::as_u64)
                        .and_then(|value| u32::try_from(value).ok())
                        .expect("v3 GameData event carries a u32 epoch"),
                    seq: event
                        .get("seq")
                        .and_then(Value::as_u64)
                        .expect("v3 GameData event carries a sequence"),
                    class,
                    key: event
                        .get("key")
                        .and_then(Value::as_u64)
                        .and_then(|value| u32::try_from(value).ok()),
                    payload: event
                        .get("payload")
                        .cloned()
                        .expect("GameData event carries payload"),
                });
            }
            Some("peer_joined" | "player_left")
                if event
                    .get("player_id")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<PlayerId>().ok())
                    == Some(expected_sender) =>
            {
                observations.push(DeliveryObservation::Lifecycle {
                    epoch: event
                        .get("epoch")
                        .and_then(Value::as_u64)
                        .and_then(|value| u32::try_from(value).ok())
                        .unwrap_or(0),
                    prior_final_seq: event.get("final_seq").and_then(Value::as_u64).unwrap_or(0),
                });
            }
            _ => {}
        }
    }
    observations
}

#[test]
fn delivery_observation_validator_accepts_only_causally_accounted_streams() {
    let sender = PlayerId::from_u128(1);
    let other = PlayerId::from_u128(2);
    let data = |from, epoch, seq, class, payload| DeliveryObservation::Data {
        from,
        epoch,
        seq,
        class,
        key: (class == DeliveryClass::Latest).then_some(7),
        payload,
    };
    let gap = |from_player, epoch, from_seq, to_seq, reason| {
        DeliveryObservation::Gap(DeliveryGap {
            from_player,
            epoch,
            from_seq,
            to_seq,
            reason,
        })
    };
    let payloads = vec![json!("one"), json!("two"), json!("three")];
    let reliable = vec![DeliveryClass::Reliable; 3];
    let exact = vec![
        data(sender, 7, 1, DeliveryClass::Reliable, payloads[0].clone()),
        data(sender, 7, 2, DeliveryClass::Reliable, payloads[1].clone()),
        data(sender, 7, 3, DeliveryClass::Reliable, payloads[2].clone()),
    ];
    validate_delivery_observations(&exact, sender, 7, 7, 3, &reliable, &payloads)
        .expect("exact stream is valid");

    let lossy_classes = vec![
        DeliveryClass::Reliable,
        DeliveryClass::Volatile,
        DeliveryClass::Reliable,
    ];
    let prior_gap = vec![
        data(sender, 7, 1, DeliveryClass::Reliable, payloads[0].clone()),
        gap(sender, 7, 2, 2, DeliveryGapReason::VolatileDropped),
        data(sender, 7, 3, DeliveryClass::Reliable, payloads[2].clone()),
    ];
    validate_delivery_observations(&prior_gap, sender, 7, 7, 3, &lossy_classes, &payloads)
        .expect("an exact compatible gap observed before its hole is valid");

    struct Negative {
        name: &'static str,
        observations: Vec<DeliveryObservation>,
        terminal_epoch: u32,
        terminal_seq: u64,
        classes: Vec<DeliveryClass>,
        payloads: Vec<Value>,
        error: &'static str,
    }
    let negative_cases = vec![
        Negative {
            name: "silent hole",
            observations: vec![exact[0].clone(), exact[2].clone()],
            terminal_epoch: 7,
            terminal_seq: 3,
            classes: reliable.clone(),
            payloads: payloads.clone(),
            error: "silent hole",
        },
        Negative {
            name: "late report",
            observations: vec![
                exact[0].clone(),
                exact[2].clone(),
                gap(sender, 7, 2, 2, DeliveryGapReason::VolatileDropped),
            ],
            terminal_epoch: 7,
            terminal_seq: 3,
            classes: lossy_classes.clone(),
            payloads: payloads.clone(),
            error: "silent hole",
        },
        Negative {
            name: "wrong sender data",
            observations: vec![data(
                other,
                7,
                1,
                DeliveryClass::Reliable,
                payloads[0].clone(),
            )],
            terminal_epoch: 7,
            terminal_seq: 1,
            classes: vec![DeliveryClass::Reliable],
            payloads: vec![payloads[0].clone()],
            error: "wrong sender",
        },
        Negative {
            name: "wrong sender gap",
            observations: vec![gap(other, 7, 1, 1, DeliveryGapReason::VolatileDropped)],
            terminal_epoch: 7,
            terminal_seq: 1,
            classes: vec![DeliveryClass::Volatile],
            payloads: vec![payloads[0].clone()],
            error: "wrong sender",
        },
        Negative {
            name: "wrong epoch gap",
            observations: vec![gap(sender, 8, 1, 1, DeliveryGapReason::VolatileDropped)],
            terminal_epoch: 7,
            terminal_seq: 1,
            classes: vec![DeliveryClass::Volatile],
            payloads: vec![payloads[0].clone()],
            error: "wrong epoch",
        },
        Negative {
            name: "invalid range",
            observations: vec![gap(sender, 7, 2, 1, DeliveryGapReason::VolatileDropped)],
            terminal_epoch: 7,
            terminal_seq: 1,
            classes: vec![DeliveryClass::Reliable],
            payloads: vec![payloads[0].clone()],
            error: "invalid range",
        },
        Negative {
            name: "incompatible gap",
            observations: vec![gap(sender, 7, 1, 1, DeliveryGapReason::VolatileDropped)],
            terminal_epoch: 7,
            terminal_seq: 1,
            classes: vec![DeliveryClass::Reliable],
            payloads: vec![payloads[0].clone()],
            error: "incompatible",
        },
        Negative {
            name: "duplicate",
            observations: vec![exact[0].clone(), exact[0].clone()],
            terminal_epoch: 7,
            terminal_seq: 1,
            classes: vec![DeliveryClass::Reliable],
            payloads: vec![payloads[0].clone()],
            error: "duplicated or regressed",
        },
        Negative {
            name: "regression",
            observations: vec![exact[0].clone(), exact[1].clone(), exact[0].clone()],
            terminal_epoch: 7,
            terminal_seq: 2,
            classes: reliable[..2].to_vec(),
            payloads: payloads[..2].to_vec(),
            error: "duplicated or regressed",
        },
        Negative {
            name: "data epoch regression",
            observations: vec![data(
                sender,
                6,
                1,
                DeliveryClass::Reliable,
                payloads[0].clone(),
            )],
            terminal_epoch: 7,
            terminal_seq: 1,
            classes: vec![DeliveryClass::Reliable],
            payloads: vec![payloads[0].clone()],
            error: "epoch regressed",
        },
        Negative {
            name: "unannounced epoch",
            observations: vec![data(
                sender,
                8,
                1,
                DeliveryClass::Reliable,
                payloads[0].clone(),
            )],
            terminal_epoch: 7,
            terminal_seq: 1,
            classes: vec![DeliveryClass::Reliable],
            payloads: vec![payloads[0].clone()],
            error: "without lifecycle",
        },
        Negative {
            name: "backward lifecycle",
            observations: vec![DeliveryObservation::Lifecycle {
                epoch: 6,
                prior_final_seq: 0,
            }],
            terminal_epoch: 7,
            terminal_seq: 0,
            classes: vec![],
            payloads: vec![],
            error: "regressed or duplicated",
        },
        Negative {
            name: "unexpected lifecycle",
            observations: vec![
                exact[0].clone(),
                DeliveryObservation::Lifecycle {
                    epoch: 8,
                    prior_final_seq: 1,
                },
            ],
            terminal_epoch: 7,
            terminal_seq: 1,
            classes: vec![DeliveryClass::Reliable],
            payloads: vec![payloads[0].clone()],
            error: "unexpected lifecycle",
        },
        Negative {
            name: "lifecycle terminal regression",
            observations: vec![
                exact[0].clone(),
                exact[1].clone(),
                DeliveryObservation::Lifecycle {
                    epoch: 8,
                    prior_final_seq: 1,
                },
            ],
            terminal_epoch: 7,
            terminal_seq: 2,
            classes: reliable[..2].to_vec(),
            payloads: payloads[..2].to_vec(),
            error: "terminal marker regressed",
        },
        Negative {
            name: "gap beyond lifecycle marker",
            observations: vec![
                gap(sender, 7, 2, 2, DeliveryGapReason::VolatileDropped),
                DeliveryObservation::Lifecycle {
                    epoch: 8,
                    prior_final_seq: 1,
                },
            ],
            terminal_epoch: 7,
            terminal_seq: 2,
            classes: vec![DeliveryClass::Reliable, DeliveryClass::Volatile],
            payloads: payloads[..2].to_vec(),
            error: "gap authorization extends",
        },
        Negative {
            name: "overlap",
            observations: vec![
                gap(sender, 7, 1, 2, DeliveryGapReason::VolatileDropped),
                gap(sender, 7, 2, 3, DeliveryGapReason::VolatileDropped),
            ],
            terminal_epoch: 7,
            terminal_seq: 3,
            classes: vec![DeliveryClass::Volatile; 3],
            payloads: payloads.clone(),
            error: "overlap or duplicate",
        },
        Negative {
            name: "incomplete range",
            observations: vec![gap(sender, 7, 2, 2, DeliveryGapReason::VolatileDropped)],
            terminal_epoch: 7,
            terminal_seq: 3,
            classes: lossy_classes.clone(),
            payloads: payloads.clone(),
            error: "incomplete terminal evidence",
        },
        Negative {
            name: "incomplete tail",
            observations: vec![exact[0].clone()],
            terminal_epoch: 7,
            terminal_seq: 2,
            classes: reliable[..2].to_vec(),
            payloads: payloads[..2].to_vec(),
            error: "incomplete terminal evidence",
        },
        Negative {
            name: "terminal regression",
            observations: exact[..2].to_vec(),
            terminal_epoch: 7,
            terminal_seq: 1,
            classes: vec![DeliveryClass::Reliable],
            payloads: vec![payloads[0].clone()],
            error: "exceeds terminal",
        },
        Negative {
            name: "range beyond marker",
            observations: vec![gap(sender, 7, 1, 2, DeliveryGapReason::VolatileDropped)],
            terminal_epoch: 7,
            terminal_seq: 1,
            classes: vec![DeliveryClass::Volatile],
            payloads: vec![payloads[0].clone()],
            error: "beyond terminal",
        },
        Negative {
            name: "payload corruption",
            observations: vec![data(
                sender,
                7,
                1,
                DeliveryClass::Reliable,
                json!("corrupt"),
            )],
            terminal_epoch: 7,
            terminal_seq: 1,
            classes: vec![DeliveryClass::Reliable],
            payloads: vec![payloads[0].clone()],
            error: "payload differs",
        },
    ];
    for case in negative_cases {
        let error = validate_delivery_observations(
            &case.observations,
            sender,
            7,
            case.terminal_epoch,
            case.terminal_seq,
            &case.classes,
            &case.payloads,
        )
        .expect_err(case.name);
        assert!(
            error.contains(case.error),
            "negative control {} produced `{error}`, expected `{}`",
            case.name,
            case.error
        );
    }
}

#[test]
fn native_jsonl_adapter_preserves_reports_identity_and_lifecycle_failures() {
    let sender = PlayerId::from_u128(1);
    let other = PlayerId::from_u128(2);
    let mut report = DeliveryReportPayload {
        per_class: Default::default(),
        gaps: vec![DeliveryGap {
            from_player: sender,
            epoch: 7,
            from_seq: 1,
            to_seq: 1,
            reason: DeliveryGapReason::VolatileDropped,
        }],
    };
    report.per_class.volatile.dropped = 1;
    let report_events = vec![json!({
        "event": "delivery_report",
        "report": report,
    })];
    let observations = observations_from_native_events(&report_events, sender);
    validate_delivery_observations(
        &observations,
        sender,
        7,
        7,
        1,
        &[DeliveryClass::Volatile],
        &[json!("omitted")],
    )
    .expect("a real JSONL report maps to causal gap authority");

    let wrong_sender_events = vec![json!({
        "event": "game_data_received",
        "from": other,
        "payload": "one",
        "epoch": 7,
        "seq": 1,
        "class": "reliable"
    })];
    let wrong_sender = observations_from_native_events(&wrong_sender_events, sender);
    assert!(validate_delivery_observations(
        &wrong_sender,
        sender,
        7,
        7,
        1,
        &[DeliveryClass::Reliable],
        &[json!("one")],
    )
    .expect_err("adapter must not filter another sender")
    .contains("wrong sender"));

    let lifecycle_events = vec![json!({
        "event": "player_left",
        "player_id": sender,
        "epoch": 6,
        "final_seq": 0
    })];
    let lifecycle = observations_from_native_events(&lifecycle_events, sender);
    assert!(
        validate_delivery_observations(&lifecycle, sender, 7, 7, 0, &[], &[])
            .expect_err("adapter must surface lifecycle regression")
            .contains("regressed or duplicated")
    );
}

// ---------------------------------------------------------------------------
// Scenarios.
// ---------------------------------------------------------------------------

/// A real server process paused during active sequenced relay and resumed must
/// preserve one causally complete v3 receiver stream and clean teardown.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
#[ignore = "nightly-only (verification-nightly.yml): Linux SIGSTOP/SIGCONT with the clients/native binary"]
async fn sigstop_then_resume_preserves_v3_delivery_accountability() {
    const BLIND_INTERVAL_MESSAGES: u64 = 256;
    const SLOW_CONSUMER_TIMEOUT_MS: u64 = 120_000;

    let knobs = DeliveryKnobs {
        send_queue_capacity: 64,
        slow_consumer_timeout_ms: SLOW_CONSUMER_TIMEOUT_MS,
        max_sojourn_ms: SLOW_CONSUMER_TIMEOUT_MS,
        socket_send_buffer_bytes: 16 * 1024,
    };
    let server = spawn_server({
        let mut overlay = config_overlay(knobs);
        overlay["server"] = json!({ "ping_timeout": 0 });
        overlay
    })
    .await;
    let port = server.port;
    let client_workdir = tempfile::tempdir().expect("create client workdir");
    let success_release = client_workdir.path().join("release-success");
    let evidence_dir = std::env::var_os("SIGNAL_FISH_MULTIPROCESS_EVIDENCE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| client_workdir.path().join("evidence"));
    let args = vec![
        "--server-url".to_string(),
        format!("ws://127.0.0.1:{port}/v3/ws"),
        "--create-room".to_string(),
        "--game-name".to_string(),
        GAME_NAME.to_string(),
        "--player-name".to_string(),
        "Resumed".to_string(),
        "--peers".to_string(),
        "2".to_string(),
        "--supported-topologies".to_string(),
        "relay".to_string(),
        "--supported-transports".to_string(),
        "relay".to_string(),
        "--success-release-file".to_string(),
        success_release.display().to_string(),
        "--run-for-secs".to_string(),
        "120".to_string(),
        "--max-runtime-secs".to_string(),
        "180".to_string(),
    ];
    let mut resumed = spawn_native_client("resumed", &args, client_workdir.path());
    let created = resumed.await_event("room_created", EVENT_DEADLINE).await;
    let room_code = created
        .get("room_code")
        .and_then(Value::as_str)
        .expect("room_created carries room_code")
        .to_string();
    resumed.await_event("room_joined", EVENT_DEADLINE).await;

    let (mut sender_sink, mut sender_rx) = connect_raw_version(port, 3).await;
    authenticate_raw_v3(&mut sender_sink, &mut sender_rx).await;
    let sender_id = join_room_raw(&mut sender_sink, &mut sender_rx, &room_code, "Sender").await;
    send_raw(&mut sender_sink, &ClientMessage::PlayerReady).await;
    resumed
        .await_event("success_criteria_met", EVENT_DEADLINE)
        .await;
    let initial_counters = scrape_delivery_counters(port).await;
    assert_eq!(initial_counters.active_connections, 2);
    assert_eq!(initial_counters.slow_consumer_disconnects, 0);

    let observation_start = resumed.events.len();
    let baseline_payload = json!({ "resume_marker": "baseline" });
    send_raw(
        &mut sender_sink,
        &ClientMessage::GameData {
            data: baseline_payload.clone(),
            class: Some(DeliveryClass::Reliable),
            key: None,
        },
    )
    .await;
    let baseline = resumed
        .await_event_matching("game_data_received", EVENT_DEADLINE, |event| {
            event["payload"]["resume_marker"] == "baseline"
        })
        .await;
    assert_eq!(baseline["from"], sender_id.to_string());
    assert_eq!(baseline["seq"], 1);
    assert_eq!(baseline["class"], "reliable");
    let epoch = baseline["epoch"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .expect("baseline carries a u32 epoch");

    signal_process(
        server.pid(),
        "-STOP",
        "pause the server during active relay",
    );
    poll_linux_process_stopped(server.pid(), true, "confirm server process pause").await;

    let mut expected_payloads = vec![baseline_payload];
    for index in 0..BLIND_INTERVAL_MESSAGES {
        let payload = json!({ "resume_seq": index });
        send_raw(
            &mut sender_sink,
            &ClientMessage::GameData {
                data: payload.clone(),
                class: Some(DeliveryClass::Reliable),
                key: None,
            },
        )
        .await;
        expected_payloads.push(payload);
    }
    poll_linux_process_stopped(
        server.pid(),
        true,
        "server remains paused through the complete blind-interval ingress",
    )
    .await;

    signal_process(
        server.pid(),
        "-CONT",
        "resume the server after blind ingress",
    );
    poll_linux_process_stopped(server.pid(), false, "confirm server process resume").await;
    let terminal_payload = json!({ "resume_marker": "terminal" });
    send_raw(
        &mut sender_sink,
        &ClientMessage::GameData {
            data: terminal_payload.clone(),
            class: Some(DeliveryClass::Reliable),
            key: None,
        },
    )
    .await;
    expected_payloads.push(terminal_payload);
    let terminal = resumed
        .await_event_matching("game_data_received", PHASE_DEADLINE, |event| {
            event["payload"]["resume_marker"] == "terminal"
        })
        .await;
    let terminal_seq = BLIND_INTERVAL_MESSAGES + 2;
    assert_eq!(terminal["from"], sender_id.to_string());
    assert_eq!(terminal["epoch"], epoch);
    assert_eq!(terminal["seq"], terminal_seq);
    assert_eq!(terminal["class"], "reliable");

    let resumed_counters = scrape_delivery_counters(port).await;
    assert_eq!(
        resumed_counters.slow_consumer_disconnects, 0,
        "server pause/resume must not evict either connection: {resumed_counters:?}"
    );
    assert_eq!(resumed_counters.active_connections, 2);
    assert_scraped_message_conservation(&resumed_counters);

    std::fs::File::create(&success_release).expect("release successful native client exit");
    let status = resumed.drain_to_termination(CLIENT_EXIT_DEADLINE).await;
    assert_eq!(
        status.code(),
        Some(0),
        "resumed client must exit cleanly; {}",
        resumed.diagnostics()
    );
    assert!(
        resumed.error_messages().is_empty(),
        "clean resumed session emitted client errors: {}",
        resumed.diagnostics()
    );
    let observations =
        observations_from_native_events(&resumed.events[observation_start..], sender_id);
    let expected_classes = vec![DeliveryClass::Reliable; expected_payloads.len()];
    validate_delivery_observations(
        &observations,
        sender_id,
        epoch,
        epoch,
        terminal_seq,
        &expected_classes,
        &expected_payloads,
    )
    .unwrap_or_else(|error| {
        panic!("complete resumed native stream violated accountability: {error}")
    });
    resumed.persist_diagnostics(&evidence_dir);
    poll_scrape_until(port, "resumed client teardown is observed", |counters| {
        counters.active_connections <= 1
    })
    .await;

    send_raw(&mut sender_sink, &ClientMessage::LeaveRoom).await;
    loop {
        let frame = tokio::time::timeout(EVENT_DEADLINE, sender_rx.next())
            .await
            .expect("timed out waiting for raw sender RoomLeft")
            .expect("raw sender closed before RoomLeft")
            .expect("raw sender websocket error before RoomLeft");
        let Message::Text(text) = frame else { continue };
        if matches!(
            serde_json::from_str::<ServerMessage>(&text).expect("valid teardown message"),
            ServerMessage::RoomLeft
        ) {
            break;
        }
    }
    drop(server);
}

#[cfg(not(target_os = "linux"))]
#[test]
#[ignore = "unsupported platform: requires Linux SIGSTOP/SIGCONT plus /proc process-state synchronization"]
fn sigstop_then_resume_preserves_v3_delivery_accountability() {}

/// A REAL client process frozen with `SIGSTOP` mid-session must be evicted as
/// a slow consumer while the room keeps flowing, and its connection must be
/// reclaimed.
///
/// Client mix: the frozen peer is the NATIVE CLIENT as a child process —
/// `SIGSTOP` must freeze a whole OS process (reactor, reads, keepalives) so
/// its kernel receive buffer genuinely fills, which no in-test task can
/// model. The flooding sender and the gap-free witness are raw in-test
/// sockets, because they need exact send pacing and frame-level drain
/// assertions the JSONL surface does not expose.
///
/// Nightly-only: locating/building the client binary is too heavy for the PR
/// lane (see the module doc); activated by `verification-nightly.yml`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
#[cfg(unix)]
#[ignore = "nightly-only (verification-nightly.yml): needs the clients/native binary (SIGNAL_FISH_CLIENT_BIN or a heavy nested build)"]
async fn sigstop_client_process_is_evicted_and_room_flows() {
    /// Sized so the flood cannot hide in kernel socket buffers: the frozen
    /// process's window plus the server's 64-slot queue fill within a couple
    /// hundred messages.
    const PADDING_BYTES: usize = 16 * 1024;
    /// Flood chunk between metric scrapes.
    const FLOOD_CHUNK: u64 = 25;
    /// Loud cap (~64MB): reached only if the eviction path is broken.
    const FLOOD_CAP: u64 = 4_000;

    let knobs = DeliveryKnobs {
        send_queue_capacity: 64,
        slow_consumer_timeout_ms: 1_000,
        max_sojourn_ms: 15_000,
        socket_send_buffer_bytes: 65_536,
    };
    let server = spawn_server(config_overlay(knobs)).await;
    let port = server.port;
    let room_code = "FRZ001";
    let client_workdir = tempfile::tempdir().expect("create client workdir");

    // Raw sender + raw healthy witness join first.
    let (mut sender_sink, mut sender_rx) = connect_raw(port).await;
    let (healthy_sink, mut healthy_rx) = connect_raw(port).await;
    let _sender_id = join_room_raw(&mut sender_sink, &mut sender_rx, room_code, "Sender").await;
    let mut healthy_sink = healthy_sink;
    let _healthy_id = join_room_raw(&mut healthy_sink, &mut healthy_rx, room_code, "Healthy").await;

    // Baseline AFTER the two raw clients are established: eviction of the
    // frozen child must return the gauge exactly here.
    let baseline = scrape_delivery_counters(port).await;
    assert_eq!(
        baseline.active_connections, 2,
        "two raw clients define the pre-freeze baseline"
    );

    // The native client joins as a real child process and lingers, reading
    // relay traffic (its --peers 4 gate is never met, so it stays a quiet
    // recipient; its --run-for-secs window far outlasts the fault).
    let mut frozen =
        spawn_lingering_client(port, room_code, "Frozen", 4, 120, client_workdir.path());
    let joined = frozen.await_event("room_joined", EVENT_DEADLINE).await;
    let frozen_id = player_id_from_room_joined(&joined, "Frozen");
    // Prove the child actually consumes relay traffic before it is frozen:
    // one probe message must surface in its JSONL stream.
    let mut payload = LedgerPayload::new("Sender", PADDING_BYTES);
    send_burst(&mut sender_sink, &mut payload, 1).await;
    frozen
        .await_event("game_data_received", EVENT_DEADLINE)
        .await;

    // Freeze the whole client process: no reads, no pings, no reactor.
    signal_process(frozen.pid(), "-STOP", "freeze the native client");

    let (totals_tx, totals_rx) = tokio::sync::watch::channel::<Option<u64>>(None);
    let healthy_drain = tokio::spawn(drain_healthy_until_complete(
        healthy_rx, totals_rx, frozen_id,
    ));

    // Metric-driven flood: pump until the server demonstrably evicted the
    // frozen consumer (the cap only bites when eviction is broken). Sends may
    // transiently block on backpressure while the frozen peer's queue is
    // full — that is the contract working, and the eviction unblocks them.
    let flood = async {
        loop {
            let counters = scrape_delivery_counters(port).await;
            if counters.slow_consumer_disconnects >= 1 {
                return;
            }
            assert!(
                payload.sent() < FLOOD_CAP,
                "flood cap reached without a slow-consumer disconnect; last scrape: {counters:?}"
            );
            send_burst(&mut sender_sink, &mut payload, FLOOD_CHUNK).await;
        }
    };
    tokio::time::timeout(PHASE_DEADLINE, flood)
        .await
        .expect("the frozen client was never evicted within the phase deadline");
    let total_sent = payload.sent();
    totals_tx.send_replace(Some(total_sent));

    // The healthy witness observes the COMPLETE gap-free stream plus the
    // frozen peer's PlayerLeft: the room kept flowing through the fault.
    let seqs = tokio::time::timeout(PHASE_DEADLINE, healthy_drain)
        .await
        .expect("healthy drain exceeded the phase deadline")
        .expect("healthy drain task panicked");
    assert_complete_and_ordered(&seqs, total_sent, "healthy witness");

    // Thaw the frozen process: it must observe its own demise (the socket the
    // server closed) and exit through its connection-error path.
    signal_process(frozen.pid(), "-CONT", "thaw the native client");
    let status = frozen.drain_to_termination(CLIENT_EXIT_DEADLINE).await;
    assert_eq!(
        status.code(),
        Some(CLIENT_EXIT_CONNECTION_ERROR),
        "the thawed client must exit through its connection-error path; {}",
        frozen.diagnostics()
    );
    let errors = frozen.error_messages();
    assert!(
        !errors.is_empty(),
        "the thawed client must report the dead connection as an error event; {}",
        frozen.diagnostics()
    );
    // The SLOW_CONSUMER farewell is best-effort by design (the server's
    // close-time write is bounded while the frozen peer's window is full), so
    // its presence is confirmed when observed but its absence is not a
    // failure — the LOUD signals are the metric, the PlayerLeft, and the
    // client's own error/exit above. Mirrors relay_backpressure_e2e.
    if errors
        .iter()
        .any(|message| message.contains("slow consumer"))
    {
        println!("frozen client received the slow-consumer farewell before the close");
    } else {
        eprintln!(
            "note: slow-consumer farewell did not survive the saturated socket \
             (best-effort by design); errors: {errors:?}"
        );
    }

    // The frozen connection is reclaimed: the gauge returns to the raw-client
    // baseline.
    poll_scrape_until(port, "active connections return to baseline", |counters| {
        counters.active_connections <= baseline.active_connections
    })
    .await;

    // Quiescent point (flood delivered, eviction complete, child reaped):
    // the eviction must be loud in the scraped counters and conservation
    // must hold.
    let final_counters = scrape_delivery_counters(port).await;
    assert!(
        final_counters.slow_consumer_disconnects >= 1,
        "the frozen process's eviction must be counted: {final_counters:?}"
    );
    assert!(
        final_counters.dropped >= 1,
        "messages abandoned with the frozen connection must be counted as dropped: \
         {final_counters:?}"
    );
    assert_scraped_message_conservation(&final_counters);

    drop(server);
}

/// A client process SIGKILLed mid-burst simply vanishes: the kernel tears the
/// socket down, peers observe `PlayerLeft`, the room keeps flowing, and the
/// scraped conservation law holds — the death is a disconnect race, never a
/// silent drop or a spurious slow-consumer eviction.
///
/// Client mix: as in the SIGSTOP test — the victim must be a real OS process
/// for `SIGKILL` to exercise kernel-level teardown; sender and witness are
/// raw in-test sockets for exact pacing/drain control.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
#[cfg(unix)]
#[ignore = "nightly-only (verification-nightly.yml): needs the clients/native binary (SIGNAL_FISH_CLIENT_BIN or a heavy nested build)"]
async fn sigkill_client_mid_burst_conserves() {
    const PADDING_BYTES: usize = 1_024;
    /// Burst half sent before the kill and half after.
    const BURST_HALF: u64 = 150;
    /// The kill lands only after the victim demonstrably consumed this much
    /// of the stream (mid-burst, not pre-burst).
    const VICTIM_PROGRESS_BEFORE_KILL: usize = 50;

    let knobs = DeliveryKnobs {
        send_queue_capacity: 256,
        slow_consumer_timeout_ms: 3_000,
        max_sojourn_ms: 15_000,
        socket_send_buffer_bytes: 65_536,
    };
    let server = spawn_server(config_overlay(knobs)).await;
    let port = server.port;
    let room_code = "KIL001";
    let client_workdir = tempfile::tempdir().expect("create client workdir");

    let (mut sender_sink, mut sender_rx) = connect_raw(port).await;
    let (mut healthy_sink, mut healthy_rx) = connect_raw(port).await;
    let _sender_id = join_room_raw(&mut sender_sink, &mut sender_rx, room_code, "Sender").await;
    let _healthy_id = join_room_raw(&mut healthy_sink, &mut healthy_rx, room_code, "Healthy").await;

    let mut victim =
        spawn_lingering_client(port, room_code, "Victim", 4, 120, client_workdir.path());
    let joined = victim.await_event("room_joined", EVENT_DEADLINE).await;
    let victim_id = player_id_from_room_joined(&joined, "Victim");

    let (totals_tx, totals_rx) = tokio::sync::watch::channel::<Option<u64>>(None);
    let healthy_drain = tokio::spawn(drain_healthy_until_complete(
        healthy_rx, totals_rx, victim_id,
    ));

    // First half of the burst; the victim must demonstrably be consuming it
    // (its JSONL stream carries the relayed payloads) before the kill.
    let mut payload = LedgerPayload::new("Sender", PADDING_BYTES);
    send_burst(&mut sender_sink, &mut payload, BURST_HALF).await;
    victim
        .await_event_count(
            "game_data_received",
            VICTIM_PROGRESS_BEFORE_KILL,
            EVENT_DEADLINE,
        )
        .await;

    // SIGKILL: no goodbye, no close frame — the kernel does the teardown.
    signal_process(victim.pid(), "-KILL", "kill the native client mid-burst");
    let status = victim.drain_to_termination(EVENT_DEADLINE).await;
    assert!(
        status.code().is_none(),
        "a SIGKILLed process terminates by signal, got exit status {status}"
    );

    // Second half: the room must keep flowing around the corpse.
    send_burst(&mut sender_sink, &mut payload, BURST_HALF).await;
    let total_sent = payload.sent();
    totals_tx.send_replace(Some(total_sent));

    let seqs = tokio::time::timeout(PHASE_DEADLINE, healthy_drain)
        .await
        .expect("healthy drain exceeded the phase deadline")
        .expect("healthy drain task panicked");
    assert_complete_and_ordered(&seqs, total_sent, "healthy witness");

    // The kernel-level death is detected promptly as a normal disconnect:
    // never a slow-consumer eviction (the 3s grace window is far above the
    // detection latency of a dead socket).
    poll_scrape_until(port, "victim connection reclaimed", |counters| {
        counters.active_connections <= 2
    })
    .await;
    let final_counters = scrape_delivery_counters(port).await;
    assert_eq!(
        final_counters.slow_consumer_disconnects, 0,
        "a SIGKILLed peer is a disconnect race, not a slow consumer: {final_counters:?}"
    );
    assert_scraped_message_conservation(&final_counters);

    drop(server);
}

/// SIGKILL the SERVER mid-relay: both clients observe the death promptly,
/// the receiver's loss is exactly the in-flight tail (its received stream is
/// a gap-free PREFIX — never a mid-stream hole), and a NEW process on the
/// SAME port serves a fresh session.
///
/// Client mix: raw in-test sockets only — observing the server's death and
/// asserting the exact received prefix require direct socket control, and no
/// child client process adds anything here (extends the restart pattern of
/// `tests/v3_multiprocess_e2e.rs` to the delivery contract). PR-lane.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
#[cfg(unix)]
async fn server_sigkill_mid_relay_bounds_loss() {
    const PADDING_BYTES: usize = 512;
    /// The kill lands only after the receiver demonstrably consumed this much.
    const RECEIVER_PROGRESS_BEFORE_KILL: usize = 200;
    /// Loud cap on the writer (reached only if the kill signal is broken).
    const WRITER_CAP: u64 = 200_000;
    /// How long a client socket may take to observe the server's death.
    const SOCKET_CLOSE_TIMEOUT: Duration = Duration::from_secs(30);

    let knobs = DeliveryKnobs {
        send_queue_capacity: 1_024,
        slow_consumer_timeout_ms: 5_000,
        max_sojourn_ms: 15_000,
        socket_send_buffer_bytes: 65_536,
    };
    let mut server = spawn_server(config_overlay(knobs)).await;
    let port = server.port;
    let room_code = "SRV001";

    let (mut sender_sink, mut sender_rx) = connect_raw(port).await;
    let (mut receiver_sink, mut receiver_rx) = connect_raw(port).await;
    let _sender_id = join_room_raw(&mut sender_sink, &mut sender_rx, room_code, "Sender").await;
    let _receiver_id =
        join_room_raw(&mut receiver_sink, &mut receiver_rx, room_code, "Receiver").await;

    // Writer: pump until the kill flag is raised; a send failure BEFORE the
    // flag means the transport broke on its own — a loud test failure.
    let (kill_tx, kill_rx) = tokio::sync::watch::channel(false);
    let writer = tokio::spawn(async move {
        let mut payload = LedgerPayload::new("Sender", PADDING_BYTES);
        while !*kill_rx.borrow() && payload.sent() < WRITER_CAP {
            let message = ClientMessage::GameData {
                class: None,
                key: None,
                data: payload.next(),
            };
            let json = serde_json::to_string(&message).expect("serialize GameData");
            match sender_sink.send(Message::Text(json.into())).await {
                Ok(()) => {}
                Err(error) => {
                    assert!(
                        *kill_rx.borrow(),
                        "sender transport failed before the server was killed: {error}"
                    );
                    break;
                }
            }
        }
        (sender_sink, payload.sent())
    });

    // Receiver: record every relayed seq until the socket observes the
    // server's death (close frame, EOF, or transport error — all legal).
    let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
    let drain_received = std::sync::Arc::clone(&received);
    let receiver_drain = tokio::spawn(async move {
        loop {
            match receiver_rx.next().await {
                Some(Ok(Message::Text(text))) => {
                    let message: ServerMessage =
                        serde_json::from_str(&text).expect("valid ServerMessage");
                    match message {
                        ServerMessage::GameData { data, .. } => {
                            let (_sender, seq) = extract(&data).unwrap_or_else(|| {
                                panic!("receiver: GameData without ledger fields: {data}")
                            });
                            drain_received
                                .lock()
                                .expect("received-seqs mutex poisoned")
                                .push(seq);
                        }
                        ServerMessage::Error {
                            message,
                            error_code,
                        } => panic!(
                            "receiver got a server error mid-relay: {message} ({error_code:?})"
                        ),
                        _ => {}
                    }
                }
                // The server's death, however it manifests on this socket.
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_control_frame)) => {}
            }
        }
    });

    // Kill only once relay traffic is demonstrably in flight end to end.
    let progress_deadline = tokio::time::Instant::now() + PHASE_DEADLINE;
    loop {
        let count = received.lock().expect("received-seqs mutex poisoned").len();
        if count >= RECEIVER_PROGRESS_BEFORE_KILL {
            break;
        }
        assert!(
            tokio::time::Instant::now() < progress_deadline,
            "receiver never reached {RECEIVER_PROGRESS_BEFORE_KILL} messages (got {count})"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    kill_tx.send_replace(true);
    server.kill_and_wait().await;

    // Both clients observe the death promptly (bounded).
    let (sender_sink, sent) = tokio::time::timeout(SOCKET_CLOSE_TIMEOUT, writer)
        .await
        .expect("writer did not stop within the close timeout after the server SIGKILL")
        .expect("writer task panicked");
    drop(sender_sink);
    tokio::time::timeout(SOCKET_CLOSE_TIMEOUT, receiver_drain)
        .await
        .expect("receiver socket never observed the server's death")
        .expect("receiver drain task panicked");
    assert!(
        sent < WRITER_CAP,
        "writer cap reached — the kill flag never stopped the writer"
    );

    // Loss is EXACTLY the in-flight tail: the received stream must be a
    // gap-free in-order prefix of 0..sent — any hole is a silent drop.
    let seqs = std::sync::Arc::try_unwrap(received)
        .expect("all receiver tasks completed")
        .into_inner()
        .expect("received-seqs mutex poisoned");
    assert!(
        seqs.len() as u64 >= RECEIVER_PROGRESS_BEFORE_KILL as u64,
        "receiver progress regressed below the pre-kill threshold"
    );
    assert!(
        seqs.len() as u64 <= sent,
        "receiver recorded {} messages but only {sent} were ever sent",
        seqs.len()
    );
    assert_gap_free_prefix(&seqs, "receiver after server SIGKILL");
    drop(server);

    // A NEW process on the SAME port serves a completely fresh session.
    let restarted = spawn_server_on_fixed_port(port, config_overlay(knobs)).await;
    let fresh_room = "SRV002";
    let (mut fresh_sender_sink, mut fresh_sender_rx) = connect_raw(port).await;
    let (mut fresh_receiver_sink, mut fresh_receiver_rx) = connect_raw(port).await;
    let _fresh_sender = join_room_raw(
        &mut fresh_sender_sink,
        &mut fresh_sender_rx,
        fresh_room,
        "FreshSender",
    )
    .await;
    let _fresh_receiver = join_room_raw(
        &mut fresh_receiver_sink,
        &mut fresh_receiver_rx,
        fresh_room,
        "FreshReceiver",
    )
    .await;

    let mut fresh_payload = LedgerPayload::new("FreshSender", PADDING_BYTES);
    send_burst(&mut fresh_sender_sink, &mut fresh_payload, 3).await;
    let mut fresh_seqs = Vec::new();
    while (fresh_seqs.len() as u64) < fresh_payload.sent() {
        let frame = tokio::time::timeout(EVENT_DEADLINE, fresh_receiver_rx.next())
            .await
            .expect("fresh receiver timed out draining the restarted relay")
            .expect("fresh receiver connection closed")
            .expect("fresh receiver websocket error");
        let Message::Text(text) = frame else { continue };
        let message: ServerMessage = serde_json::from_str(&text).expect("valid ServerMessage");
        match message {
            ServerMessage::GameData { data, .. } => {
                let (_sender, seq) = extract(&data).unwrap_or_else(|| {
                    panic!("fresh receiver: GameData without ledger fields: {data}")
                });
                fresh_seqs.push(seq);
            }
            ServerMessage::Error {
                message,
                error_code,
            } => {
                panic!("fresh receiver got a server error: {message} ({error_code:?})")
            }
            _ => {}
        }
    }
    assert_complete_and_ordered(&fresh_seqs, fresh_payload.sent(), "fresh receiver");

    drop(restarted);
}
