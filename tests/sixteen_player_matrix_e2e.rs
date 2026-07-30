//! P10.C relay matrix: encoding x room size x network profile over real WebSockets.
//!
//! The PR lane covers the six deterministic clean cells from PLAN.md:
//! `{json, message_pack} x {2, 8, 16}`. Every player sends a one-second,
//! 30 msg/s stream with a 1 KiB payload while every peer drains concurrently.
//! Each cell must preserve the complete per-sender payload ledger, satisfy the
//! protocol-v3 `(epoch, seq)` rules through `ConformanceAuditor`, avoid all
//! backpressure/evictions, and keep observed p99 relay latency below 250 ms.
//! The nightly lane repeats the grid behind one chaos proxy per client for
//! jitter/throttle and complete-burst pause/resume recovery, then sweeps the
//! 16-player JSON cell through increasing sender rates to expose the measured
//! throughput/latency knee without turning runner-specific timing into a
//! brittle correctness threshold.

mod test_helpers;
mod websocket_test_helpers;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::config::ProtocolConfig;
use signal_fish_server::protocol::{
    ClientMessage, GameDataEncoding, ServerMessage, V3BinaryGameDataFrame,
};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::create_router;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use test_helpers::{create_test_server_with_config, test_server_config, RunningTestServer};
use websocket_test_helpers::chaos_proxy::{ChaosProxy, Direction};
use websocket_test_helpers::conformance::{ConformanceAuditor, ReceiverProtocolMode};
use websocket_test_helpers::delivery_ledger::{ReceiverExpectation, SenderExpectation};
use websocket_test_helpers::room16::{authenticate_with_encoding, connect, try_join, PlayerHandle};

const PROTOCOL_VERSION: u16 = 3;
const MESSAGES_PER_SENDER: u64 = 30;
const PAYLOAD_BYTES: usize = 1024;
const CELL_DEADLINE: Duration = Duration::from_secs(45);
const FRAME_DEADLINE: Duration = Duration::from_secs(30);
const P99_LIMIT_MICROS: u64 = 250_000;
const NANOS_PER_SECOND: u64 = 1_000_000_000;

#[derive(Clone, Copy)]
struct MatrixCell {
    encoding: GameDataEncoding,
    players: usize,
}

#[derive(Clone, Copy)]
struct TrafficProfile {
    messages_per_sender: u64,
    send_interval: Duration,
    target_sender_rate_hz: u64,
    enforce_pr_latency_limit: bool,
}

impl TrafficProfile {
    fn one_second_at(target_sender_rate_hz: u64, enforce_pr_latency_limit: bool) -> Self {
        assert!(target_sender_rate_hz > 0, "sender rate must be positive");
        Self {
            messages_per_sender: target_sender_rate_hz,
            send_interval: Duration::from_nanos(NANOS_PER_SECOND / target_sender_rate_hz),
            target_sender_rate_hz,
            enforce_pr_latency_limit,
        }
    }
}

#[derive(Debug)]
struct CellObservation {
    target_deliveries_per_second: usize,
    completed_deliveries: usize,
    achieved_ingress_messages_per_second: f64,
    sender_completion_millis: u128,
    observed_deliveries_per_second: f64,
    completion_millis: u128,
    p50_micros: u64,
    p95_micros: u64,
    p99_micros: u64,
    max_micros: u64,
    backpressure_events: u64,
    rss_kib: Option<u64>,
}

impl std::fmt::Display for CellObservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "target_deliveries_per_second={} completed_deliveries={} \
             achieved_ingress_messages_per_second={:.1} sender_completion_ms={} \
             observed_deliveries_per_second={:.1} completion_ms={} p50_us={} \
             p95_us={} p99_us={} max_us={} backpressure_events={} rss_kib={:?}",
            self.target_deliveries_per_second,
            self.completed_deliveries,
            self.achieved_ingress_messages_per_second,
            self.sender_completion_millis,
            self.observed_deliveries_per_second,
            self.completion_millis,
            self.p50_micros,
            self.p95_micros,
            self.p99_micros,
            self.max_micros,
            self.backpressure_events,
            self.rss_kib,
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NetworkProfile {
    Clean,
    JitterThrottle,
    BurstPauseResume,
}

impl NetworkProfile {
    const fn label(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::JitterThrottle => "jitter-throttle",
            Self::BurstPauseResume => "burst-pause-resume",
        }
    }
}

impl MatrixCell {
    fn label(self) -> String {
        format!("{}-{}p", self.encoding.as_wire_str(), self.players)
    }
}

fn matrix_cells() -> [MatrixCell; 6] {
    [
        MatrixCell {
            encoding: GameDataEncoding::Json,
            players: 2,
        },
        MatrixCell {
            encoding: GameDataEncoding::Json,
            players: 8,
        },
        MatrixCell {
            encoding: GameDataEncoding::Json,
            players: 16,
        },
        MatrixCell {
            encoding: GameDataEncoding::MessagePack,
            players: 2,
        },
        MatrixCell {
            encoding: GameDataEncoding::MessagePack,
            players: 8,
        },
        MatrixCell {
            encoding: GameDataEncoding::MessagePack,
            players: 16,
        },
    ]
}

fn matrix_server_config() -> ServerConfig {
    // The clean 16-player cell peaks at 7,200 recipient deliveries per second
    // and must remain healthy at the production WebSocket queue defaults. The
    // test fixture changes unrelated cleanup/auth settings only.
    test_server_config()
}

fn matrix_protocol_config() -> ProtocolConfig {
    let mut config = ProtocolConfig::default();
    config.sdk_compatibility.enforce = false;
    config
}

async fn start_server(server: Arc<EnhancedGameServer>) -> RunningTestServer {
    let router = create_router("http://localhost:3000").with_state(server.clone());
    RunningTestServer::spawn(server, router).await
}

fn encode_client_game_data(encoding: GameDataEncoding, data: serde_json::Value) -> Message {
    match encoding {
        GameDataEncoding::Json => {
            let message = ClientMessage::GameData {
                class: None,
                key: None,
                data,
            };
            Message::Text(
                serde_json::to_string(&message)
                    .expect("serialize matrix GameData")
                    .into(),
            )
        }
        GameDataEncoding::MessagePack => Message::Binary(
            rmp_serde::to_vec_named(&data)
                .expect("serialize matrix MessagePack payload")
                .into(),
        ),
        GameDataEncoding::Rkyv => panic!("rkyv is not a negotiable matrix encoding"),
    }
}

async fn join_players(
    client_addrs: &[std::net::SocketAddr],
    cell: MatrixCell,
) -> Vec<PlayerHandle> {
    assert_eq!(
        client_addrs.len(),
        cell.players,
        "{}: every player needs one client endpoint",
        cell.label()
    );
    let mut players = Vec::with_capacity(cell.players);
    for ordinal in 0..cell.players {
        let endpoint = client_addrs
            .get(ordinal)
            .copied()
            .expect("matrix endpoint count checked above");
        let mut ws = connect(endpoint).await;
        authenticate_with_encoding(&mut ws, PROTOCOL_VERSION, Some(cell.encoding)).await;
        let name = format!("P{ordinal}");
        let player = try_join(
            ws,
            "relay_matrix",
            "MATRIX",
            Some(u8::try_from(cell.players).expect("matrix size fits protocol cap")),
            &name,
        )
        .await
        .unwrap_or_else(|(reason, code)| {
            panic!(
                "{}: {name} failed to join: {reason} ({code:?})",
                cell.label()
            )
        });
        players.push(player);
    }
    players
}

fn set_all_proxies(proxies: &[ChaosProxy], action: impl Fn(&ChaosProxy)) {
    for proxy in proxies {
        action(proxy);
    }
}

fn arm_fault_profile(profile: NetworkProfile, proxies: &[ChaosProxy]) {
    match profile {
        NetworkProfile::Clean => {
            assert!(
                proxies.is_empty(),
                "clean profile must bypass chaos proxies"
            );
        }
        NetworkProfile::JitterThrottle => {
            const THROTTLE_BYTES_PER_SEC: u64 = 128 * 1_024;
            set_all_proxies(proxies, |proxy| {
                proxy.throttle(Direction::ServerToClient, Some(THROTTLE_BYTES_PER_SEC));
            });
        }
        NetworkProfile::BurstPauseResume => {
            set_all_proxies(proxies, |proxy| {
                proxy.pause(Direction::ServerToClient);
            });
        }
    }
}

async fn exercise_fault_profile(
    profile: NetworkProfile,
    proxies: &[ChaosProxy],
    completed_senders: &AtomicUsize,
    expected_senders: usize,
) {
    match profile {
        NetworkProfile::Clean => {}
        NetworkProfile::JitterThrottle => {
            const SPIKES: &[(u64, u64)] = &[(75, 100), (150, 100), (50, 100)];
            for (pause_ms, recovery_ms) in SPIKES {
                set_all_proxies(proxies, |proxy| {
                    proxy.pause(Direction::ServerToClient);
                });
                tokio::time::sleep(Duration::from_millis(*pause_ms)).await;
                set_all_proxies(proxies, |proxy| {
                    proxy.resume(Direction::ServerToClient);
                });
                tokio::time::sleep(Duration::from_millis(*recovery_ms)).await;
            }
            set_all_proxies(proxies, |proxy| {
                proxy.throttle(Direction::ServerToClient, None);
            });
        }
        NetworkProfile::BurstPauseResume => {
            // Keep every downstream pump paused until the writers positively
            // report that the complete burst reached their WebSockets. This is
            // condition synchronization, not a fixed-duration assumption.
            let deadline = Instant::now() + FRAME_DEADLINE;
            while completed_senders.load(Ordering::Acquire) < expected_senders {
                assert!(
                    Instant::now() < deadline,
                    "burst-pause writers did not finish before {FRAME_DEADLINE:?}"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            set_all_proxies(proxies, |proxy| {
                proxy.resume(Direction::ServerToClient);
            });
        }
    }
}

async fn establish_auditor_baselines(
    cell: MatrixCell,
    players: &mut [PlayerHandle],
    auditor: &ConformanceAuditor,
) {
    // A data-lane barrier from every sender gives every receiver a positive
    // proof that all earlier control-priority join lifecycle reached it. The
    // payload deliberately lacks `ledger_sender`, so it advances the v3 relay
    // stamp observed by the auditor without entering the measured matrix
    // delivery ledger below.
    for (ordinal, player) in players.iter_mut().enumerate() {
        let wire = encode_client_game_data(
            cell.encoding,
            serde_json::json!({ "matrix_baseline": ordinal }),
        );
        player.ws.send(wire).await.unwrap_or_else(|error| {
            panic!("{}: P{ordinal} baseline send failed: {error}", cell.label())
        });
    }

    for (ordinal, player) in players.iter_mut().enumerate() {
        let receiver = format!("P{ordinal}");
        auditor.record_message(
            &receiver,
            &ServerMessage::RoomJoined(Box::new(player.room_joined.clone())),
        );

        let later_joiners = cell.players - player.room_player_count;
        let expected_barriers = cell.players - 1;
        let mut joined = 0usize;
        let mut barriers = BTreeSet::new();
        let deadline = Instant::now() + FRAME_DEADLINE;
        while barriers.len() < expected_barriers {
            let frame = tokio::time::timeout_at(deadline, player.ws.next())
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "{}: {receiver} baseline timed out after {joined}/{later_joiners} joins and {}/{expected_barriers} barriers",
                        cell.label(),
                        barriers.len()
                    )
                })
                .unwrap_or_else(|| panic!("{}: {receiver} closed during baseline", cell.label()))
                .unwrap_or_else(|error| {
                    panic!(
                        "{}: {receiver} WebSocket failed during baseline: {error}",
                        cell.label()
                    )
                });

            let data = match frame {
                Message::Text(text) => match auditor.record_text_frame(&receiver, &text) {
                    ServerMessage::PlayerJoined { .. } => {
                        joined += 1;
                        assert!(
                            joined <= later_joiners,
                            "{}: {receiver} observed too many PlayerJoined frames",
                            cell.label()
                        );
                        continue;
                    }
                    ServerMessage::LobbyStateChanged { .. } => continue,
                    ServerMessage::GameData { data, .. } => data,
                    other => panic!(
                        "{}: {receiver} unexpected baseline message: {other:?}",
                        cell.label()
                    ),
                },
                Message::Binary(bytes) => {
                    let frame = auditor.record_binary_frame(&receiver, &bytes);
                    decode_binary_payload(&cell.label(), &receiver, &frame)
                }
                Message::Ping(_) | Message::Pong(_) => continue,
                Message::Close(reason) => panic!(
                    "{}: {receiver} closed during baseline: {reason:?}",
                    cell.label()
                ),
                Message::Frame(frame) => panic!(
                    "{}: {receiver} received unexpected raw frame during baseline: {frame:?}",
                    cell.label()
                ),
            };
            let sender = data
                .get("matrix_baseline")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or_else(|| {
                    panic!(
                        "{}: {receiver} received non-baseline GameData during setup: {data}",
                        cell.label()
                    )
                });
            assert!(
                sender < cell.players && sender != ordinal,
                "{}: {receiver} received invalid baseline sender P{sender}",
                cell.label()
            );
            assert!(
                barriers.insert(sender),
                "{}: {receiver} received duplicate baseline from P{sender}",
                cell.label()
            );
        }
        assert_eq!(
            joined,
            later_joiners,
            "{}: {receiver} baseline overtook an expected PlayerJoined frame",
            cell.label()
        );
    }
}

fn record_latency(
    cell_label: &str,
    receiver: &str,
    data: &serde_json::Value,
    origin: Instant,
) -> u64 {
    let sent_at = data
        .get("sent_at_micros")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_else(|| panic!("{cell_label}: {receiver} payload lacks sent_at_micros: {data}"));
    let observed_at =
        u64::try_from(origin.elapsed().as_micros()).expect("matrix duration fits u64 micros");
    observed_at.checked_sub(sent_at).unwrap_or_else(|| {
        panic!("{cell_label}: {receiver} observed a payload timestamp from the future")
    })
}

fn decode_binary_payload(
    cell_label: &str,
    receiver: &str,
    frame: &V3BinaryGameDataFrame,
) -> serde_json::Value {
    assert_eq!(
        frame.encoding,
        GameDataEncoding::MessagePack,
        "{cell_label}: {receiver} binary relay used the wrong encoding"
    );
    rmp_serde::from_slice(&frame.payload).unwrap_or_else(|error| {
        panic!("{cell_label}: {receiver} received invalid MessagePack payload: {error}")
    })
}

async fn run_cell(
    cell: MatrixCell,
    profile: NetworkProfile,
    traffic: TrafficProfile,
) -> CellObservation {
    let cell_label = format!(
        "{}-{}-{}hz",
        cell.label(),
        profile.label(),
        traffic.target_sender_rate_hz
    );
    let server =
        create_test_server_with_config(matrix_server_config(), matrix_protocol_config()).await;
    let metrics = server.metrics();
    let running_server = start_server(server).await;
    let addr = running_server.addr();
    let mut proxies = Vec::new();
    if profile != NetworkProfile::Clean {
        for _ in 0..cell.players {
            proxies.push(ChaosProxy::spawn(addr).await);
        }
    }
    let client_addrs = if proxies.is_empty() {
        vec![addr; cell.players]
    } else {
        proxies.iter().map(ChaosProxy::addr).collect()
    };
    let mut players = join_players(&client_addrs, cell).await;

    let auditor = Arc::new(ConformanceAuditor::new(ReceiverProtocolMode::V3));
    establish_auditor_baselines(cell, &mut players, &auditor).await;
    arm_fault_profile(profile, &proxies);

    let origin = Instant::now();
    let measurement_start = origin + Duration::from_millis(50);
    // Tokio intervals tick immediately at their start instant. Scheduling the
    // first message one interval after the measurement boundary makes N
    // messages span N intervals, so a perfectly paced 30 Hz writer reports
    // 30 Hz rather than 30 / 29 intervals.
    let first_send = measurement_start + traffic.send_interval;
    let cell_deadline = first_send + CELL_DEADLINE;
    let expected_per_receiver = (cell.players - 1)
        * usize::try_from(traffic.messages_per_sender).expect("message count fits usize");

    let mut writers = Vec::with_capacity(cell.players);
    let mut readers = Vec::with_capacity(cell.players);
    let completed_senders = Arc::new(AtomicUsize::new(0));
    for (ordinal, player) in players.into_iter().enumerate() {
        let sender_name = format!("P{ordinal}");
        let receiver_name = sender_name.clone();
        let (mut sink, mut stream) = player.ws.split();

        let writer_label = cell_label.clone();
        let writer_completion = Arc::clone(&completed_senders);
        writers.push(tokio::spawn(async move {
            let mut ticker = tokio::time::interval_at(first_send, traffic.send_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let padding = "x".repeat(PAYLOAD_BYTES);
            for seq in 0..traffic.messages_per_sender {
                ticker.tick().await;
                let sent_at_micros = u64::try_from(origin.elapsed().as_micros())
                    .expect("matrix duration fits u64 micros");
                let wire = encode_client_game_data(
                    cell.encoding,
                    serde_json::json!({
                        "ledger_sender": sender_name.as_str(),
                        "seq": seq,
                        "padding": padding.as_str(),
                        "sent_at_micros": sent_at_micros,
                    }),
                );
                sink.send(wire).await.unwrap_or_else(|error| {
                    panic!("{writer_label}: {sender_name} send {seq} failed: {error}")
                });
            }
            writer_completion.fetch_add(1, Ordering::Release);
            (sink, origin.elapsed())
        }));

        let reader_auditor = Arc::clone(&auditor);
        let reader_label = cell_label.clone();
        readers.push(tokio::spawn(async move {
            let mut delivered = 0usize;
            let mut latencies_micros = Vec::with_capacity(expected_per_receiver);
            while delivered < expected_per_receiver {
                let frame = tokio::time::timeout_at(cell_deadline, stream.next())
                    .await
                    .unwrap_or_else(|_| {
                        panic!(
                            "{reader_label}: {receiver_name} timed out after {delivered}/{expected_per_receiver} deliveries"
                        )
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "{reader_label}: {receiver_name} closed after {delivered}/{expected_per_receiver} deliveries"
                        )
                    })
                    .unwrap_or_else(|error| {
                        panic!("{reader_label}: {receiver_name} WebSocket read failed: {error}")
                    });

                let data = match frame {
                    Message::Text(text) => {
                        assert_eq!(
                            cell.encoding,
                            GameDataEncoding::Json,
                            "{reader_label}: {receiver_name} expected MessagePack binary delivery"
                        );
                        match reader_auditor.record_text_frame(&receiver_name, &text) {
                            ServerMessage::GameData { data, .. } => data,
                            other => panic!(
                                "{reader_label}: {receiver_name} expected GameData, got {other:?}"
                            ),
                        }
                    }
                    Message::Binary(bytes) => {
                        assert_eq!(
                            cell.encoding,
                            GameDataEncoding::MessagePack,
                            "{reader_label}: {receiver_name} expected JSON text delivery"
                        );
                        let frame = reader_auditor.record_binary_frame(&receiver_name, &bytes);
                        decode_binary_payload(&reader_label, &receiver_name, &frame)
                    }
                    Message::Ping(_) | Message::Pong(_) => continue,
                    Message::Close(reason) => panic!(
                        "{reader_label}: {receiver_name} closed after {delivered}/{expected_per_receiver} deliveries: {reason:?}"
                    ),
                    Message::Frame(frame) => panic!(
                        "{reader_label}: {receiver_name} received unexpected raw frame: {frame:?}"
                    ),
                };
                latencies_micros.push(record_latency(
                    &reader_label,
                    &receiver_name,
                    &data,
                    origin,
                ));
                delivered += 1;
            }
            (stream, latencies_micros)
        }));
    }

    exercise_fault_profile(profile, &proxies, &completed_senders, cell.players).await;

    let (sinks, streams, mut latencies, sender_completed_at) =
        tokio::time::timeout_at(cell_deadline, async {
            let mut sinks = Vec::with_capacity(cell.players);
            let mut streams = Vec::with_capacity(cell.players);
            let mut latencies = Vec::with_capacity(cell.players * expected_per_receiver);
            let mut sender_completion = Duration::ZERO;
            for writer in writers {
                let (sink, completed_at) = writer.await.expect("matrix writer task panicked");
                sender_completion = sender_completion.max(completed_at);
                sinks.push(sink);
            }
            for reader in readers {
                let (stream, mut receiver_latencies) =
                    reader.await.expect("matrix reader task panicked");
                streams.push(stream);
                latencies.append(&mut receiver_latencies);
            }
            (sinks, streams, latencies, sender_completion)
        })
        .await
        .unwrap_or_else(|_| panic!("{cell_label}: cell exceeded {CELL_DEADLINE:?}"));
    // Capture performance observations at the delivery barrier. Conformance
    // polling and percentile sorting below validate/describe the completed
    // cell but must not inflate its measured wall time or RSS.
    let delivery_completed_at = origin.elapsed();
    let rss_kib = resident_set_kib();

    let expectations: Vec<_> = (0..cell.players)
        .map(|receiver| ReceiverExpectation {
            receiver: format!("P{receiver}"),
            senders: (0..cell.players)
                .filter(|sender| *sender != receiver)
                .map(|sender| SenderExpectation {
                    sender: format!("P{sender}"),
                    total_sent: traffic.messages_per_sender,
                })
                .collect(),
        })
        .collect();
    auditor.assert_conformance(&metrics, &expectations).await;

    let backpressure_events = metrics
        .websocket_backpressure_events
        .load(Ordering::Relaxed);
    if traffic.enforce_pr_latency_limit {
        assert_eq!(
            backpressure_events, 0,
            "{cell_label}: bounded matrix cell must not enter backpressure"
        );
    }
    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        0,
        "{cell_label}: bounded matrix cell must not evict a receiver"
    );

    let expected_samples = cell.players * expected_per_receiver;
    assert_eq!(
        latencies.len(),
        expected_samples,
        "{cell_label}: every delivery contributes one latency sample"
    );
    latencies.sort_unstable();
    let percentile = |percent: usize| {
        let index = latencies.len().saturating_mul(percent).div_ceil(100) - 1;
        latencies[index]
    };
    let p50_micros = percentile(50);
    let p95_micros = percentile(95);
    let p99_micros = percentile(99);
    let max_micros = *latencies.last().expect("matrix produced latency samples");
    if profile == NetworkProfile::Clean && traffic.enforce_pr_latency_limit {
        assert!(
            p99_micros < P99_LIMIT_MICROS,
            "{cell_label}: p99 relay latency {p99_micros}us exceeded {P99_LIMIT_MICROS}us"
        );
    }
    let sender_completion = sender_completed_at.saturating_sub(Duration::from_millis(50));
    let ingress_messages = cell.players as f64 * traffic.messages_per_sender as f64;
    let achieved_ingress_messages_per_second = ingress_messages / sender_completion.as_secs_f64();
    let completion = delivery_completed_at.saturating_sub(Duration::from_millis(50));
    let observed_deliveries_per_second = expected_samples as f64 / completion.as_secs_f64();
    let target_deliveries_per_second = cell
        .players
        .saturating_mul(cell.players.saturating_sub(1))
        .saturating_mul(
            usize::try_from(traffic.target_sender_rate_hz).expect("sender rate fits usize"),
        );
    eprintln!(
        "matrix cell {cell_label}: target_deliveries_per_second={target_deliveries_per_second} \
         completed_deliveries={expected_samples} \
         achieved_ingress_messages_per_second={achieved_ingress_messages_per_second:.1} \
         sender_completion_ms={} \
         observed_deliveries_per_second={observed_deliveries_per_second:.1} \
         completion_ms={} p50_us={p50_micros} p95_us={p95_micros} \
         p99_us={p99_micros} max_us={max_micros} \
         backpressure_events={backpressure_events} rss_kib={rss_kib:?}",
        sender_completion.as_millis(),
        completion.as_millis(),
    );

    drop(sinks);
    drop(streams);
    drop(proxies);
    running_server.shutdown().await;

    CellObservation {
        target_deliveries_per_second,
        completed_deliveries: expected_samples,
        achieved_ingress_messages_per_second,
        sender_completion_millis: sender_completion.as_millis(),
        observed_deliveries_per_second,
        completion_millis: completion.as_millis(),
        p50_micros,
        p95_micros,
        p99_micros,
        max_micros,
        backpressure_events,
        rss_kib,
    }
}

#[cfg(target_os = "linux")]
fn resident_set_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        let value = line.strip_prefix("VmRSS:")?.split_whitespace().next()?;
        value.parse().ok()
    })
}

#[cfg(not(target_os = "linux"))]
fn resident_set_kib() -> Option<u64> {
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn clean_relay_matrix_is_complete_fast_and_backpressure_free() {
    let traffic = TrafficProfile::one_second_at(MESSAGES_PER_SENDER, true);
    for cell in matrix_cells() {
        run_cell(cell, NetworkProfile::Clean, traffic).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
#[ignore = "nightly-only (verification-nightly.yml): twelve per-client proxy fault cells"]
async fn fault_relay_matrix_recovers_completely_after_fault_lift() {
    let traffic = TrafficProfile::one_second_at(MESSAGES_PER_SENDER, true);
    for profile in [
        NetworkProfile::JitterThrottle,
        NetworkProfile::BurstPauseResume,
    ] {
        for cell in matrix_cells() {
            run_cell(cell, profile, traffic).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
#[ignore = "nightly-only (verification-nightly.yml): machine-measured 16-player saturation sweep"]
async fn sixteen_player_relay_knee_sweep_preserves_exact_delivery() {
    let observations = observe_sixteen_player_knee(&[30, 60, 120, 240, 480]).await;

    assert_eq!(
        observations.len(),
        5,
        "the registered knee grid must report every rate"
    );
    for pair in observations.windows(2) {
        assert!(
            pair[0].target_deliveries_per_second < pair[1].target_deliveries_per_second,
            "knee targets must increase strictly: {pair:?}"
        );
    }
    for observation in observations {
        eprintln!("relay knee observation: {observation}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
#[ignore = "manual-only: release-profile 16-player saturation comparison"]
async fn sixteen_player_relay_saturation_diagnostic_preserves_exact_delivery() {
    let observations = observe_sixteen_player_knee(&[960, 1_920]).await;
    for observation in observations {
        eprintln!("relay saturation observation: {observation}");
    }
}

async fn observe_sixteen_player_knee(sender_rates_hz: &[u64]) -> Vec<CellObservation> {
    let cell = MatrixCell {
        encoding: GameDataEncoding::Json,
        players: 16,
    };
    let mut observations = Vec::new();

    for &sender_rate_hz in sender_rates_hz {
        observations.push(
            run_cell(
                cell,
                NetworkProfile::Clean,
                TrafficProfile::one_second_at(sender_rate_hz, false),
            )
            .await,
        );
    }
    observations
}
