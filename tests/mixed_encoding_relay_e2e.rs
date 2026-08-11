//! P10.C H14: mixed JSON/MessagePack rooms over real WebSockets.
//!
//! The pre-registered concern was that one incompatible recipient could turn
//! every binary relay into an `UNSUPPORTED_GAME_DATA_FORMAT` error and amplify
//! a stream into a slow-consumer eviction. Negotiated MessagePack is, however,
//! losslessly convertible to JSON. This test pins that boundary: same-format
//! recipients receive binary frames, JSON recipients receive text fallbacks,
//! and neither path emits an unsupported-format report or loses accountability.

mod test_helpers;
mod websocket_test_helpers;

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::config::ProtocolConfig;
use signal_fish_server::protocol::{
    decode_v3_binary_game_data, ClientMessage, DeliveryGap, DeliveryGapReason, ErrorCode,
    GameDataEncoding, PlayerId, ServerMessage, V3BinaryGameDataFrame,
};
use signal_fish_server::websocket::create_router;
use tokio_tungstenite::tungstenite::Message;

use test_helpers::{create_test_server_with_config, test_server_config, RunningTestServer};
use websocket_test_helpers::chaos_proxy::{ChaosProxy, Direction, PumpTermination};
use websocket_test_helpers::conformance::{ConformanceAuditor, ReceiverProtocolMode};
use websocket_test_helpers::delivery_ledger::{ReceiverExpectation, SenderExpectation};
use websocket_test_helpers::room16::{authenticate_with_encoding, connect, try_join};

const MESSAGES_PER_SENDER: u64 = 128;
const FRAME_DEADLINE: Duration = Duration::from_secs(30);

fn encode_game_data(encoding: GameDataEncoding, sender: &str, seq: u64) -> Message {
    let data = serde_json::json!({
        "ledger_sender": sender,
        "seq": seq,
        "payload": "mixed-encoding-fallback",
    });
    match encoding {
        GameDataEncoding::Json => Message::Text(
            serde_json::to_string(&ClientMessage::GameData {
                data,
                class: None,
                key: None,
            })
            .expect("serialize JSON GameData")
            .into(),
        ),
        GameDataEncoding::MessagePack => Message::Binary(
            rmp_serde::to_vec_named(&data)
                .expect("serialize MessagePack GameData")
                .into(),
        ),
        GameDataEncoding::Rkyv => panic!("rkyv is reserved and cannot be negotiated"),
    }
}

fn decode_message_pack(frame: &V3BinaryGameDataFrame) -> serde_json::Value {
    assert_eq!(frame.encoding, GameDataEncoding::MessagePack);
    rmp_serde::from_slice(&frame.payload).expect("decode relayed MessagePack payload")
}

async fn drain_join_lifecycle(
    receiver: &str,
    ws: &mut websocket_test_helpers::WsStream,
    expected_later_joiners: usize,
    auditor: &ConformanceAuditor,
) {
    let deadline = tokio::time::Instant::now() + FRAME_DEADLINE;
    let mut joined = 0usize;
    while joined < expected_later_joiners {
        let frame = tokio::time::timeout_at(deadline, ws.next())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "{receiver} timed out after {joined}/{expected_later_joiners} join lifecycle frames"
                )
            })
            .unwrap_or_else(|| panic!("{receiver} closed while draining join lifecycle"))
            .unwrap_or_else(|error| {
                panic!("{receiver} WebSocket failed while draining join lifecycle: {error}")
            });
        match frame {
            Message::Text(text) => match auditor.record_text_frame(receiver, &text) {
                ServerMessage::PlayerJoined { .. } => joined += 1,
                ServerMessage::LobbyStateChanged { .. } => {}
                other => panic!("{receiver} observed unexpected join setup message: {other:?}"),
            },
            Message::Ping(_) | Message::Pong(_) => {}
            Message::Binary(bytes) => {
                panic!("{receiver} observed unexpected binary join frame: {bytes:?}")
            }
            Message::Close(reason) => {
                panic!("{receiver} closed while draining join lifecycle: {reason:?}")
            }
            Message::Frame(frame) => {
                panic!("{receiver} observed unexpected raw join frame: {frame:?}")
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_json_and_message_pack_relay_without_error_amplification() {
    let mut protocol = ProtocolConfig::default();
    protocol.sdk_compatibility.enforce = false;
    let server = create_test_server_with_config(test_server_config(), protocol).await;
    let metrics = server.metrics();
    let router = create_router("http://localhost:3000").with_state(server.clone());
    let running_server = RunningTestServer::spawn(server, router).await;

    // P0 is JSON; P1/P2 are MessagePack. This produces all meaningful paths:
    // JSON -> MessagePack recipient stays text, MessagePack -> JSON falls back
    // to text, and MessagePack -> MessagePack remains binary.
    let encodings = [
        GameDataEncoding::Json,
        GameDataEncoding::MessagePack,
        GameDataEncoding::MessagePack,
    ];
    let mut players = Vec::with_capacity(encodings.len());
    for (ordinal, encoding) in encodings.into_iter().enumerate() {
        let mut ws = connect(running_server.addr()).await;
        authenticate_with_encoding(&mut ws, 3, Some(encoding)).await;
        let player = try_join(
            ws,
            "mixed_encoding",
            "MIXED1",
            Some(3),
            &format!("P{ordinal}"),
        )
        .await
        .unwrap_or_else(|(reason, code)| {
            panic!("P{ordinal} failed to join mixed-encoding room: {reason} ({code:?})")
        });
        players.push(player);
    }

    let identities: Arc<BTreeMap<PlayerId, (String, GameDataEncoding)>> = Arc::new(
        players
            .iter()
            .enumerate()
            .map(|(ordinal, player)| {
                (
                    player.player_id,
                    (format!("P{ordinal}"), encodings[ordinal]),
                )
            })
            .collect(),
    );
    let auditor = Arc::new(ConformanceAuditor::new(ReceiverProtocolMode::V3));

    let expected_per_receiver = (encodings.len() - 1)
        * usize::try_from(MESSAGES_PER_SENDER).expect("message count fits usize");
    let mut writers = Vec::with_capacity(players.len());
    let mut readers = Vec::with_capacity(players.len());
    for (ordinal, player) in players.into_iter().enumerate() {
        let name = format!("P{ordinal}");
        auditor.record_message(
            &name,
            &ServerMessage::RoomJoined(Box::new(player.room_joined)),
        );

        let expected_later_joiners = encodings.len() - player.room_player_count;
        let sender_encoding = encodings[ordinal];
        let (mut sink, mut stream) = player.ws.split();
        let writer_name = name.clone();
        writers.push(tokio::spawn(async move {
            for seq in 0..MESSAGES_PER_SENDER {
                sink.send(encode_game_data(sender_encoding, &writer_name, seq))
                    .await
                    .unwrap_or_else(|error| {
                        panic!("{writer_name} failed to send mixed frame {seq}: {error}")
                    });
            }
            sink
        }));

        let reader_auditor = Arc::clone(&auditor);
        let reader_identities = Arc::clone(&identities);
        let receiver_encoding = encodings[ordinal];
        readers.push(tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + FRAME_DEADLINE;
            let mut delivered = 0usize;
            let mut later_joiners = 0usize;
            let mut text_deliveries = 0usize;
            let mut binary_deliveries = 0usize;
            while delivered < expected_per_receiver {
                let frame = tokio::time::timeout_at(deadline, stream.next())
                    .await
                    .unwrap_or_else(|_| {
                        panic!(
                            "{name} timed out after {delivered}/{expected_per_receiver} mixed deliveries"
                        )
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "{name} closed after {delivered}/{expected_per_receiver} mixed deliveries"
                        )
                    })
                    .unwrap_or_else(|error| panic!("{name} WebSocket read failed: {error}"));

                let (from_player, data, physical_binary) = match frame {
                    Message::Text(text) => match reader_auditor.record_text_frame(&name, &text) {
                        ServerMessage::PlayerJoined { .. } => {
                            later_joiners += 1;
                            assert!(
                                later_joiners <= expected_later_joiners,
                                "{name} observed too many PlayerJoined frames"
                            );
                            continue;
                        }
                        ServerMessage::LobbyStateChanged { .. } => continue,
                        ServerMessage::GameData {
                            from_player, data, ..
                        } => (from_player, data, false),
                        ServerMessage::Error { message, error_code } => panic!(
                            "{name} observed mixed-encoding error {error_code:?}: {message}"
                        ),
                        other => panic!("{name} observed unexpected text message: {other:?}"),
                    },
                    Message::Binary(bytes) => {
                        let frame = reader_auditor.record_binary_frame(&name, &bytes);
                        let from_player = frame.from_player;
                        (from_player, decode_message_pack(&frame), true)
                    }
                    Message::Ping(_) | Message::Pong(_) => continue,
                    Message::Close(reason) => panic!(
                        "{name} closed after {delivered}/{expected_per_receiver} mixed deliveries: {reason:?}"
                    ),
                    Message::Frame(frame) => {
                        panic!("{name} received unexpected raw frame: {frame:?}")
                    }
                };

                let (sender, sender_encoding) = reader_identities
                    .get(&from_player)
                    .unwrap_or_else(|| panic!("{name} received data from unknown {from_player}"));
                let expected_binary = *sender_encoding == GameDataEncoding::MessagePack
                    && receiver_encoding == GameDataEncoding::MessagePack;
                assert_eq!(
                    physical_binary, expected_binary,
                    "{name} received {sender}'s {:?} payload in the wrong physical frame",
                    sender_encoding
                );
                assert_eq!(
                    data.get("ledger_sender").and_then(serde_json::Value::as_str),
                    Some(sender.as_str()),
                    "{name} received a fallback with changed payload identity"
                );
                if physical_binary {
                    binary_deliveries += 1;
                } else {
                    text_deliveries += 1;
                }
                delivered += 1;
            }
            assert_eq!(
                later_joiners, expected_later_joiners,
                "{name} data overtook an expected join lifecycle"
            );
            (stream, text_deliveries, binary_deliveries)
        }));
    }

    let mut sinks = Vec::with_capacity(encodings.len());
    let mut streams = Vec::with_capacity(encodings.len());
    let mut text_deliveries = 0usize;
    let mut binary_deliveries = 0usize;
    for writer in writers {
        sinks.push(writer.await.expect("mixed-encoding writer task panicked"));
    }
    for reader in readers {
        let (stream, text, binary) = reader.await.expect("mixed-encoding reader task panicked");
        streams.push(stream);
        text_deliveries += text;
        binary_deliveries += binary;
    }

    let expectations: Vec<_> = (0..encodings.len())
        .map(|receiver| ReceiverExpectation {
            receiver: format!("P{receiver}"),
            senders: (0..encodings.len())
                .filter(|sender| *sender != receiver)
                .map(|sender| SenderExpectation {
                    sender: format!("P{sender}"),
                    total_sent: MESSAGES_PER_SENDER,
                })
                .collect(),
        })
        .collect();
    auditor.assert_conformance(&metrics, &expectations).await;

    let per_sender = usize::try_from(MESSAGES_PER_SENDER).expect("message count fits usize");
    assert_eq!(text_deliveries, 4 * per_sender);
    assert_eq!(binary_deliveries, 2 * per_sender);
    let delivery = metrics.delivery_metrics_by_class();
    assert_eq!(delivery.reliable.unsupported_format, 0);
    assert_eq!(delivery.latest.unsupported_format, 0);
    assert_eq!(delivery.volatile.unsupported_format, 0);
    assert_eq!(
        metrics.websocket_messages_dropped.load(Ordering::Relaxed),
        0
    );
    assert_eq!(
        metrics
            .websocket_backpressure_events
            .load(Ordering::Relaxed),
        0
    );
    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        0
    );

    drop(sinks);
    drop(streams);
    running_server.shutdown().await;
}

#[derive(Debug)]
struct ObservedError {
    code: Option<ErrorCode>,
    message: String,
}

impl std::fmt::Display for ObservedError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:?}: {}", self.code, self.message)
    }
}

fn summarize_errors(errors: &[ObservedError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" | ")
}

#[derive(Debug)]
enum ReaderTerminal {
    Complete,
    Close { code: u16, reason: String },
    TimedOut,
    Eof,
    WebSocketError(String),
    UnexpectedMessage(String),
    TaskFailure(String),
}

impl ReaderTerminal {
    fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }
}

impl std::fmt::Display for ReaderTerminal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Complete => formatter.write_str("complete"),
            Self::Close { code, reason } => write!(formatter, "close {code}: {reason}"),
            Self::TimedOut => formatter.write_str("timed out"),
            Self::Eof => formatter.write_str("EOF"),
            Self::WebSocketError(error) => write!(formatter, "WebSocket error: {error}"),
            Self::UnexpectedMessage(message) => {
                write!(formatter, "unexpected message: {message}")
            }
            Self::TaskFailure(message) => write!(formatter, "reader task failed: {message}"),
        }
    }
}

#[derive(Debug)]
enum AuditFrame {
    Text(String),
    Binary(Vec<u8>),
    Close { code: u16, reason: String },
}

fn replay_audit(auditor: &ConformanceAuditor, receiver: &str, frames: &[AuditFrame]) {
    for frame in frames {
        match frame {
            AuditFrame::Text(text) => {
                auditor.record_text_frame(receiver, text);
            }
            AuditFrame::Binary(bytes) => {
                auditor.record_binary_frame(receiver, bytes);
            }
            AuditFrame::Close { code, reason } => auditor.record_close(receiver, *code, reason),
        }
    }
}

/// Everything the throttled compatible recipient observed, including a
/// terminal failure instead of an early task panic. Keeping this symmetric
/// with [`FallbackObservation`] preserves both sides of a RED H14 run.
#[derive(Debug)]
struct CompatibleObservation {
    delivered: u64,
    wire_bytes: u64,
    player_left: Vec<PlayerId>,
    errors: Vec<ObservedError>,
    terminal: ReaderTerminal,
    elapsed: Duration,
    audit_frames: Vec<AuditFrame>,
}

/// Everything the throttled JSON recipient observed, so the amplification
/// oracle can be asserted against measured wire bytes rather than frame counts.
#[derive(Debug)]
struct FallbackObservation {
    /// `DeliveryReport` frames received.
    reports: u64,
    /// Rate-limited `UnsupportedGameDataFormat` advisories received.
    advisories: u64,
    /// Sequences named by `UnsupportedFormat` gap ranges, summed over reports.
    accounted: u64,
    /// Total WebSocket payload bytes this recipient had to drain.
    wire_bytes: u64,
    player_left: Vec<PlayerId>,
    errors: Vec<ObservedError>,
    terminal: ReaderTerminal,
    elapsed: Duration,
    audit_frames: Vec<AuditFrame>,
}

#[derive(Debug)]
struct ProxyObservation {
    destination_bytes: u64,
    measurement_elapsed: Duration,
    bytes_per_second: f64,
    terminations_at_reader_terminal: Vec<PumpTermination>,
    natural_termination_wait: TerminationWait,
    terminations_after_teardown: Vec<PumpTermination>,
    teardown_termination_wait: TerminationWait,
    control_errors: Vec<String>,
}

struct ReaderProxySnapshot {
    destination_bytes: u64,
    measurement_elapsed: Duration,
    terminations: Vec<PumpTermination>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminationWait {
    NotRequested,
    Observed,
    TimedOut(Duration),
}

struct H14Diagnostics<'a> {
    fallback: &'a FallbackObservation,
    compatible: &'a CompatibleObservation,
    fallback_proxy: &'a ProxyObservation,
    compatible_proxy: &'a ProxyObservation,
    burst: u64,
    sender_elapsed: Duration,
    proxy_recv_buffer_bytes: u32,
    backpressure: u64,
    slow_consumer_evictions: u64,
}

impl std::fmt::Display for H14Diagnostics<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "sender burst_elapsed={:?} proxy_upstream_recv_buffer_request={} bytes; \
             fallback accounted={}/{} reports={} advisories={} player_left={:?} errors=[{}] \
             terminal={} wire_bytes={} elapsed={:?}; compatible delivered={}/{} \
             player_left={:?} errors=[{}] terminal={} wire_bytes={} elapsed={:?}; \
             proxy destination_bytes: fallback={} over {:?} ({:.0} B/s), compatible={} \
             over {:?} ({:.0} B/s); \
             proxy diagnostics: fallback terminations_at_reader_terminal={:?} \
             natural_wait={:?} terminations_after_teardown={:?} teardown_wait={:?} \
             control_errors={:?}, compatible terminations_at_reader_terminal={:?} \
             natural_wait={:?} terminations_after_teardown={:?} teardown_wait={:?} \
             control_errors={:?}; amplification={:.2}x backpressure_events={} \
             slow_consumer_evictions={}",
            self.sender_elapsed,
            self.proxy_recv_buffer_bytes,
            self.fallback.accounted,
            self.burst,
            self.fallback.reports,
            self.fallback.advisories,
            self.fallback.player_left,
            summarize_errors(&self.fallback.errors),
            self.fallback.terminal,
            self.fallback.wire_bytes,
            self.fallback.elapsed,
            self.compatible.delivered,
            self.burst,
            self.compatible.player_left,
            summarize_errors(&self.compatible.errors),
            self.compatible.terminal,
            self.compatible.wire_bytes,
            self.compatible.elapsed,
            self.fallback_proxy.destination_bytes,
            self.fallback_proxy.measurement_elapsed,
            self.fallback_proxy.bytes_per_second,
            self.compatible_proxy.destination_bytes,
            self.compatible_proxy.measurement_elapsed,
            self.compatible_proxy.bytes_per_second,
            self.fallback_proxy.terminations_at_reader_terminal,
            self.fallback_proxy.natural_termination_wait,
            self.fallback_proxy.terminations_after_teardown,
            self.fallback_proxy.teardown_termination_wait,
            self.fallback_proxy.control_errors,
            self.compatible_proxy.terminations_at_reader_terminal,
            self.compatible_proxy.natural_termination_wait,
            self.compatible_proxy.terminations_after_teardown,
            self.compatible_proxy.teardown_termination_wait,
            self.compatible_proxy.control_errors,
            self.fallback.wire_bytes as f64 / self.compatible.wire_bytes.max(1) as f64,
            self.backpressure,
            self.slow_consumer_evictions,
        )
    }
}

async fn wait_for_proxy_termination(proxy: &ChaosProxy, budget: Duration) -> TerminationWait {
    if !proxy.terminations().is_empty() {
        return TerminationWait::Observed;
    }
    let wait = async {
        while proxy.terminations().is_empty() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    };
    match tokio::time::timeout(budget, wait).await {
        Ok(()) => TerminationWait::Observed,
        Err(_) => TerminationWait::TimedOut(budget),
    }
}

async fn collect_task_with_snapshot<T, U>(
    task: tokio::task::JoinHandle<T>,
    snapshot: impl FnOnce() -> U,
) -> (Result<T, tokio::task::JoinError>, U) {
    let result = task.await;
    (result, snapshot())
}

fn unsupported_gap_count(gaps: &[DeliveryGap]) -> Result<u64, String> {
    gaps.iter()
        .filter(|gap| gap.reason == DeliveryGapReason::UnsupportedFormat)
        .try_fold(0u64, |accounted, gap| {
            let length = gap
                .to_seq
                .checked_sub(gap.from_seq)
                .and_then(|length| length.checked_add(1))
                .ok_or_else(|| {
                    format!(
                        "invalid unsupported-format gap {}..={} for player {} epoch {}",
                        gap.from_seq, gap.to_seq, gap.from_player, gap.epoch
                    )
                })?;
            accounted
                .checked_add(length)
                .ok_or_else(|| "unsupported-format accounted count overflowed".to_string())
        })
}

fn first_h14_reader_failure(
    sender_error: Option<&str>,
    fallback: &FallbackObservation,
    compatible: &CompatibleObservation,
    burst: u64,
) -> Option<String> {
    if let Some(error) = sender_error {
        return Some(format!(
            "H14 sender failed before completing the fixed burst: {error}"
        ));
    }
    // Prefer the directly observed recipient failure over the peer's later
    // lifecycle consequence. In the historical RED signature the compatible
    // recipient's SlowConsumer farewell/close caused the fallback PlayerLeft.
    if !compatible.errors.is_empty() {
        return Some(format!(
            "compatible recipient observed unexpected server errors: {}",
            summarize_errors(&compatible.errors)
        ));
    }
    if !compatible.terminal.is_complete() {
        return Some(format!(
            "compatible recipient terminated after {}/{} deliveries: {}",
            compatible.delivered, burst, compatible.terminal
        ));
    }
    if let Some(error) = fallback
        .errors
        .iter()
        .find(|error| !matches!(&error.code, Some(ErrorCode::UnsupportedGameDataFormat)))
    {
        return Some(format!(
            "fallback recipient observed a non-advisory error: {error}"
        ));
    }
    if usize::try_from(fallback.advisories).ok() != Some(fallback.errors.len()) {
        return Some(format!(
            "fallback observed {} Error frame(s), but counted {} advisories",
            fallback.errors.len(),
            fallback.advisories
        ));
    }
    if !fallback.terminal.is_complete() {
        return Some(format!(
            "fallback recipient terminated after {}/{} accounted sequences: {}",
            fallback.accounted, burst, fallback.terminal
        ));
    }
    if !fallback.player_left.is_empty() {
        return Some(format!(
            "fallback recipient observed compatible-recipient eviction: {:?}",
            fallback.player_left
        ));
    }
    if !compatible.player_left.is_empty() {
        return Some(format!(
            "compatible recipient observed fallback-recipient eviction: {:?}",
            compatible.player_left
        ));
    }
    None
}

#[tokio::test]
async fn h14_red_diagnostics_preserve_both_recipients_and_proxy_outcomes() {
    let mut fallback = FallbackObservation {
        reports: 3,
        advisories: 2,
        accounted: 4_999,
        wire_bytes: 2_218,
        player_left: vec![PlayerId::nil()],
        errors: vec![ObservedError {
            code: Some(ErrorCode::UnsupportedGameDataFormat),
            message: "advisory".to_string(),
        }],
        terminal: ReaderTerminal::Complete,
        elapsed: Duration::from_secs(3),
        audit_frames: Vec::new(),
    };
    let mut compatible = CompatibleObservation {
        delivered: 4_000,
        wire_bytes: 320_000,
        player_left: Vec::new(),
        errors: vec![ObservedError {
            code: Some(ErrorCode::SlowConsumer),
            message: "farewell".to_string(),
        }],
        terminal: ReaderTerminal::Close {
            code: 4002,
            reason: "slow_consumer".to_string(),
        },
        elapsed: Duration::from_secs(12),
        audit_frames: Vec::new(),
    };
    let fallback_proxy = ProxyObservation {
        destination_bytes: 2_242,
        measurement_elapsed: Duration::from_secs(3),
        bytes_per_second: 747.0,
        terminations_at_reader_terminal: Vec::new(),
        natural_termination_wait: TerminationWait::TimedOut(Duration::from_secs(5)),
        terminations_after_teardown: vec![PumpTermination {
            direction: Direction::ClientToServer,
            cause: "source reached EOF".to_string(),
        }],
        teardown_termination_wait: TerminationWait::Observed,
        control_errors: Vec::new(),
    };
    let compatible_proxy = ProxyObservation {
        destination_bytes: 327_680,
        measurement_elapsed: Duration::from_secs(12),
        bytes_per_second: 27_307.0,
        terminations_at_reader_terminal: vec![PumpTermination {
            direction: Direction::ServerToClient,
            cause: "destination write failed".to_string(),
        }],
        natural_termination_wait: TerminationWait::Observed,
        terminations_after_teardown: Vec::new(),
        teardown_termination_wait: TerminationWait::Observed,
        control_errors: vec!["retained control failure".to_string()],
    };

    let diagnostics = H14Diagnostics {
        fallback: &fallback,
        compatible: &compatible,
        fallback_proxy: &fallback_proxy,
        compatible_proxy: &compatible_proxy,
        burst: 5_000,
        sender_elapsed: Duration::from_secs(2),
        proxy_recv_buffer_bytes: 4 * 1_024,
        backpressure: 7,
        slow_consumer_evictions: 1,
    }
    .to_string();

    for expected in [
        "sender burst_elapsed=2s",
        "proxy_upstream_recv_buffer_request=4096 bytes",
        "fallback accounted=4999/5000",
        "player_left=[00000000-0000-0000-0000-000000000000]",
        "terminal=complete",
        "compatible delivered=4000/5000",
        "terminal=close 4002: slow_consumer",
        "fallback=2242 over 3s (747 B/s)",
        "compatible=327680 over 12s (27307 B/s)",
        "source reached EOF",
        "destination write failed",
        "retained control failure",
        "natural_wait=TimedOut(5s)",
        "teardown_wait=Observed",
        "backpressure_events=7",
        "slow_consumer_evictions=1",
    ] {
        assert!(
            diagnostics.contains(expected),
            "RED diagnostic omitted {expected:?}: {diagnostics}"
        );
    }

    let counter = Arc::new(std::sync::atomic::AtomicU64::new(17));
    let snapshot_counter = Arc::clone(&counter);
    let (reader_result, snapshot) =
        collect_task_with_snapshot(tokio::spawn(async { 4_000u64 }), move || {
            snapshot_counter.load(Ordering::Relaxed)
        })
        .await;
    counter.store(99, Ordering::Relaxed);
    assert_eq!(reader_result.expect("synthetic reader task"), 4_000);
    assert_eq!(
        snapshot, 17,
        "reader proxy frontier must be captured when that reader completes"
    );

    let malformed_gap = DeliveryGap {
        from_player: PlayerId::nil(),
        epoch: 0,
        from_seq: 10,
        to_seq: 9,
        reason: DeliveryGapReason::UnsupportedFormat,
    };
    assert!(
        unsupported_gap_count(&[malformed_gap]).is_err(),
        "malformed gap ranges must become terminal observations, not reader panics"
    );
    let failure = first_h14_reader_failure(None, &fallback, &compatible, 5_000)
        .expect("synthetic RED observations must fail");
    assert!(
        failure.starts_with("compatible recipient observed unexpected server errors"),
        "first failed control was not preserved: {failure}"
    );

    compatible.errors.clear();
    compatible.terminal = ReaderTerminal::Complete;
    compatible.delivered = 5_000;
    fallback.player_left.clear();
    fallback.advisories = 1;
    fallback.accounted = 5_000;
    assert!(
        first_h14_reader_failure(None, &fallback, &compatible, 5_000).is_none(),
        "complete observations with only the expected fallback advisory must be GREEN"
    );

    fallback.advisories = 2;
    let mismatch = first_h14_reader_failure(None, &fallback, &compatible, 5_000)
        .expect("advisory count mismatch must fail");
    assert!(mismatch.contains("counted 2 advisories"), "{mismatch}");

    fallback.advisories = 1;
    fallback.terminal = ReaderTerminal::TimedOut;
    let fallback_terminal = first_h14_reader_failure(None, &fallback, &compatible, 5_000)
        .expect("fallback terminal failure must fail");
    assert!(
        fallback_terminal.starts_with("fallback recipient terminated"),
        "{fallback_terminal}"
    );

    let upstream = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind unused upstream");
    let proxy =
        ChaosProxy::spawn(upstream.local_addr().expect("read unused upstream address")).await;
    assert_eq!(
        wait_for_proxy_termination(&proxy, Duration::from_millis(1)).await,
        TerminationWait::TimedOut(Duration::from_millis(1)),
        "missing pump termination must surface its wait timeout"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
#[ignore = "nightly-only (verification-nightly.yml): throttled unsupported-format amplification"]
async fn unsupported_message_pack_fallback_does_not_flap_weaker_recipient() {
    const BURST: u64 = 5_000;
    const THROTTLE_BYTES_PER_SEC: u64 = 32 * 1_024;
    // Keep localhost TCP autotuning from hiding more than one production
    // full-queue deadline of already accepted bytes in the proxy's
    // server-facing socket. H10 uses the same bound for its 32 KiB/s lane.
    const PROXY_RECV_BUFFER_BYTES: u32 = 4 * 1_024;
    const EXPERIMENT_DEADLINE: Duration = Duration::from_secs(90);

    let mut config = test_server_config();
    // Isolate delivery-sojourn behavior from transport-probe timeouts. The
    // production delivery queue, 15-second max sojourn, and 5-second full-queue
    // timeout remain unchanged.
    config.websocket_config.server_ping_interval_secs = 0;
    let mut protocol = ProtocolConfig::default();
    protocol.sdk_compatibility.enforce = false;
    let server = create_test_server_with_config(config, protocol).await;
    let metrics = server.metrics();
    let router = create_router("http://localhost:3000").with_state(server.clone());
    let running_server = RunningTestServer::spawn(server, router).await;

    let compatible_proxy = ChaosProxy::spawn_with_upstream_recv_buffer(
        running_server.addr(),
        Some(PROXY_RECV_BUFFER_BYTES),
    )
    .await;
    let fallback_proxy = ChaosProxy::spawn_with_upstream_recv_buffer(
        running_server.addr(),
        Some(PROXY_RECV_BUFFER_BYTES),
    )
    .await;
    let endpoints = [
        running_server.addr(),
        compatible_proxy.addr(),
        fallback_proxy.addr(),
    ];
    let encodings = [
        GameDataEncoding::MessagePack,
        GameDataEncoding::MessagePack,
        GameDataEncoding::Json,
    ];
    let auditor = Arc::new(ConformanceAuditor::new(ReceiverProtocolMode::V3));
    let mut players = Vec::with_capacity(encodings.len());
    for ordinal in 0..encodings.len() {
        let mut ws = connect(endpoints[ordinal]).await;
        authenticate_with_encoding(&mut ws, 3, Some(encodings[ordinal])).await;
        let player = try_join(
            ws,
            "mixed_encoding",
            "MIXED2",
            Some(3),
            &format!("P{ordinal}"),
        )
        .await
        .unwrap_or_else(|(reason, code)| {
            panic!("P{ordinal} failed to join amplification room: {reason} ({code:?})")
        });
        auditor.record_message(
            &format!("P{ordinal}"),
            &ServerMessage::RoomJoined(Box::new(player.room_joined.clone())),
        );
        players.push(player);
    }
    for (ordinal, player) in players.iter_mut().enumerate() {
        drain_join_lifecycle(
            &format!("P{ordinal}"),
            &mut player.ws,
            encodings.len() - player.room_player_count,
            &auditor,
        )
        .await;
    }

    compatible_proxy.throttle(Direction::ServerToClient, Some(THROTTLE_BYTES_PER_SEC));
    fallback_proxy.throttle(Direction::ServerToClient, Some(THROTTLE_BYTES_PER_SEC));
    let compatible_proxy_bytes_before =
        compatible_proxy.destination_write_bytes(Direction::ServerToClient);
    let fallback_proxy_bytes_before =
        fallback_proxy.destination_write_bytes(Direction::ServerToClient);
    let proxy_measurement_started = std::time::Instant::now();

    let mut sender = players.remove(0).ws;
    let compatible = players.remove(0).ws;
    let fallback = players.remove(0).ws;

    let compatible_reader = tokio::spawn(async move {
        let (_, mut stream) = compatible.split();
        let started = std::time::Instant::now();
        let deadline = tokio::time::Instant::now() + EXPERIMENT_DEADLINE;
        let mut delivered = 0u64;
        let mut player_left = Vec::new();
        let mut errors = Vec::new();
        let mut wire_bytes = 0u64;
        let mut audit_frames =
            Vec::with_capacity(usize::try_from(BURST).expect("test burst fits usize"));
        let terminal = loop {
            if delivered >= BURST {
                break ReaderTerminal::Complete;
            }
            let frame = match tokio::time::timeout_at(deadline, stream.next()).await {
                Err(_) => break ReaderTerminal::TimedOut,
                Ok(None) => break ReaderTerminal::Eof,
                Ok(Some(Err(error))) => break ReaderTerminal::WebSocketError(error.to_string()),
                Ok(Some(Ok(frame))) => frame,
            };
            match frame {
                Message::Binary(bytes) => {
                    wire_bytes += bytes.len() as u64;
                    audit_frames.push(AuditFrame::Binary(bytes.to_vec()));
                    let relayed = match decode_v3_binary_game_data(&bytes) {
                        Ok(relayed) => relayed,
                        Err(error) => {
                            break ReaderTerminal::UnexpectedMessage(format!(
                                "invalid binary delivery: {error}"
                            ))
                        }
                    };
                    if relayed.encoding != GameDataEncoding::MessagePack
                        || relayed.payload.as_slice() != [0xc1]
                    {
                        break ReaderTerminal::UnexpectedMessage(format!(
                            "binary delivery had encoding {:?} and payload {:?}",
                            relayed.encoding, relayed.payload
                        ));
                    }
                    delivered += 1;
                }
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Text(text) => {
                    wire_bytes += text.len() as u64;
                    audit_frames.push(AuditFrame::Text(text.to_string()));
                    let message = match serde_json::from_str(&text) {
                        Ok(message) => message,
                        Err(error) => {
                            break ReaderTerminal::UnexpectedMessage(format!(
                                "invalid ServerMessage text frame: {error}; text={text:?}"
                            ))
                        }
                    };
                    match message {
                        ServerMessage::PlayerLeft { player_id, .. } => player_left.push(player_id),
                        ServerMessage::Error {
                            error_code,
                            message,
                        } => errors.push(ObservedError {
                            code: error_code,
                            message,
                        }),
                        other => {
                            break ReaderTerminal::UnexpectedMessage(format!(
                                "expected binary delivery, got {other:?}"
                            ));
                        }
                    }
                }
                Message::Close(reason) => {
                    let (code, reason) = reason
                        .map(|reason| (u16::from(reason.code), reason.reason.to_string()))
                        .unwrap_or((1005, "close frame carried no status".to_string()));
                    audit_frames.push(AuditFrame::Close {
                        code,
                        reason: reason.clone(),
                    });
                    break ReaderTerminal::Close { code, reason };
                }
                Message::Frame(frame) => {
                    break ReaderTerminal::UnexpectedMessage(format!(
                        "observed raw frame: {frame:?}"
                    ));
                }
            }
        };
        (
            stream,
            CompatibleObservation {
                delivered,
                wire_bytes,
                player_left,
                errors,
                terminal,
                elapsed: started.elapsed(),
                audit_frames,
            },
        )
    });

    let fallback_reader = tokio::spawn(async move {
        let (_, mut stream) = fallback.split();
        let started = std::time::Instant::now();
        let deadline = tokio::time::Instant::now() + EXPERIMENT_DEADLINE;
        let mut reports = 0u64;
        let mut errors = 0u64;
        let mut wire_bytes = 0u64;
        let mut accounted = 0u64;
        let mut player_left = Vec::new();
        let mut observed_errors = Vec::new();
        let mut audit_frames = Vec::new();
        let terminal = loop {
            if accounted >= BURST {
                break ReaderTerminal::Complete;
            }
            let frame = match tokio::time::timeout_at(deadline, stream.next()).await {
                Err(_) => break ReaderTerminal::TimedOut,
                Ok(None) => break ReaderTerminal::Eof,
                Ok(Some(Err(error))) => break ReaderTerminal::WebSocketError(error.to_string()),
                Ok(Some(Ok(frame))) => frame,
            };
            match frame {
                Message::Text(text) => {
                    wire_bytes += text.len() as u64;
                    audit_frames.push(AuditFrame::Text(text.to_string()));
                    let message = match serde_json::from_str(&text) {
                        Ok(message) => message,
                        Err(error) => {
                            break ReaderTerminal::UnexpectedMessage(format!(
                                "invalid ServerMessage text frame: {error}; text={text:?}"
                            ))
                        }
                    };
                    match message {
                        ServerMessage::DeliveryReport(report) => {
                            reports += 1;
                            // The auditor already proves these ranges never overlap
                            // and never leave a hole, so summing their lengths is an
                            // exact count of the sequences accounted for. Counting
                            // sequences rather than frames is what lets one
                            // coalesced range stand in for a burst of omissions
                            // without weakening the accountability oracle.
                            let newly_accounted = match unsupported_gap_count(&report.gaps) {
                                Ok(count) => count,
                                Err(error) => {
                                    break ReaderTerminal::UnexpectedMessage(error);
                                }
                            };
                            accounted = match accounted.checked_add(newly_accounted) {
                                Some(next) => next,
                                None => {
                                    break ReaderTerminal::UnexpectedMessage(
                                        "fallback accounted count overflowed".to_string(),
                                    );
                                }
                            };
                        }
                        ServerMessage::Error {
                            error_code,
                            message,
                        } => {
                            if error_code == Some(ErrorCode::UnsupportedGameDataFormat) {
                                errors += 1;
                            }
                            observed_errors.push(ObservedError {
                                code: error_code,
                                message,
                            });
                        }
                        ServerMessage::PlayerLeft { player_id, .. } => player_left.push(player_id),
                        other => {
                            break ReaderTerminal::UnexpectedMessage(format!(
                                "observed unexpected message: {other:?}"
                            ))
                        }
                    }
                }
                Message::Close(reason) => {
                    let (code, reason) = reason
                        .map(|reason| (u16::from(reason.code), reason.reason.to_string()))
                        .unwrap_or((1005, "close frame carried no status".to_string()));
                    audit_frames.push(AuditFrame::Close {
                        code,
                        reason: reason.clone(),
                    });
                    break ReaderTerminal::Close { code, reason };
                }
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Binary(bytes) => {
                    break ReaderTerminal::UnexpectedMessage(format!(
                        "received unconvertible binary bytes: {bytes:?}"
                    ));
                }
                Message::Frame(frame) => {
                    break ReaderTerminal::UnexpectedMessage(format!(
                        "observed raw frame: {frame:?}"
                    ));
                }
            }
        };
        (
            stream,
            FallbackObservation {
                reports,
                advisories: errors,
                accounted,
                wire_bytes,
                player_left,
                errors: observed_errors,
                terminal,
                elapsed: started.elapsed(),
                audit_frames,
            },
        )
    });

    let sender_started = std::time::Instant::now();
    let mut sender_error = None;
    for seq in 0..BURST {
        // 0xc1 is MessagePack's reserved/invalid marker. The server correctly
        // treats binary game data as opaque for same-format peers, while a JSON
        // recipient cannot convert it and needs explicit gap accountability.
        if let Err(error) = sender.send(Message::Binary(vec![0xc1].into())).await {
            sender_error = Some(format!("send failed at sequence {seq}/{BURST}: {error}"));
            break;
        }
    }
    let sender_elapsed = sender_started.elapsed();

    // Both recipients must complete their streams while the bandwidth fault is
    // still applied. Lifting the throttle as soon as the fallback recipient
    // finished would let the compatible recipient drain its remainder on an
    // unimpaired link, and "the compatible peer survives the same fault" is half
    // the oracle.
    let ((fallback_result, fallback_reader_proxy), (compatible_result, compatible_reader_proxy)) = tokio::join!(
        collect_task_with_snapshot(fallback_reader, || ReaderProxySnapshot {
            destination_bytes: fallback_proxy
                .destination_write_bytes(Direction::ServerToClient)
                .saturating_sub(fallback_proxy_bytes_before),
            measurement_elapsed: proxy_measurement_started.elapsed(),
            terminations: fallback_proxy.terminations(),
        }),
        collect_task_with_snapshot(compatible_reader, || ReaderProxySnapshot {
            destination_bytes: compatible_proxy
                .destination_write_bytes(Direction::ServerToClient)
                .saturating_sub(compatible_proxy_bytes_before),
            measurement_elapsed: proxy_measurement_started.elapsed(),
            terminations: compatible_proxy.terminations(),
        }),
    );

    let (fallback_stream, fallback) = match fallback_result {
        Ok((stream, observation)) => (Some(stream), observation),
        Err(error) => (
            None,
            FallbackObservation {
                reports: 0,
                advisories: 0,
                accounted: 0,
                wire_bytes: 0,
                player_left: Vec::new(),
                errors: Vec::new(),
                terminal: ReaderTerminal::TaskFailure(error.to_string()),
                elapsed: Duration::ZERO,
                audit_frames: Vec::new(),
            },
        ),
    };
    let (compatible_stream, compatible) = match compatible_result {
        Ok((stream, observation)) => (Some(stream), observation),
        Err(error) => (
            None,
            CompatibleObservation {
                delivered: 0,
                wire_bytes: 0,
                player_left: Vec::new(),
                errors: Vec::new(),
                terminal: ReaderTerminal::TaskFailure(error.to_string()),
                elapsed: Duration::ZERO,
                audit_frames: Vec::new(),
            },
        ),
    };

    const PROXY_TERMINATION_BUDGET: Duration = Duration::from_secs(5);
    let (fallback_natural_wait, compatible_natural_wait) = tokio::join!(
        async {
            if fallback.terminal.is_complete() {
                TerminationWait::NotRequested
            } else {
                wait_for_proxy_termination(&fallback_proxy, PROXY_TERMINATION_BUDGET).await
            }
        },
        async {
            if compatible.terminal.is_complete() {
                TerminationWait::NotRequested
            } else {
                wait_for_proxy_termination(&compatible_proxy, PROXY_TERMINATION_BUDGET).await
            }
        },
    );

    // Byte-rate evidence ends at the readers' terminal observations. Lift the
    // fault only after that immutable frontier is captured so queued bytes
    // cannot flush unpaced into a RED run's numerator.
    compatible_proxy.throttle(Direction::ServerToClient, None);
    fallback_proxy.throttle(Direction::ServerToClient, None);

    // The client streams own the proxy-facing sockets. Close them, then wait
    // for the proxy supervisor to retain the pump's terminal cause so the
    // snapshot cannot race an otherwise decisive RED-run diagnostic.
    drop(compatible_stream);
    drop(fallback_stream);
    let (fallback_teardown_wait, compatible_teardown_wait) = tokio::join!(
        wait_for_proxy_termination(&fallback_proxy, PROXY_TERMINATION_BUDGET),
        wait_for_proxy_termination(&compatible_proxy, PROXY_TERMINATION_BUDGET),
    );

    let compatible_proxy_observation = ProxyObservation {
        destination_bytes: compatible_reader_proxy.destination_bytes,
        measurement_elapsed: compatible_reader_proxy.measurement_elapsed,
        bytes_per_second: compatible_reader_proxy.destination_bytes as f64
            / compatible_reader_proxy
                .measurement_elapsed
                .as_secs_f64()
                .max(f64::EPSILON),
        terminations_at_reader_terminal: compatible_reader_proxy.terminations,
        natural_termination_wait: compatible_natural_wait,
        terminations_after_teardown: compatible_proxy.terminations(),
        teardown_termination_wait: compatible_teardown_wait,
        control_errors: compatible_proxy.control_errors(),
    };
    let fallback_proxy_observation = ProxyObservation {
        destination_bytes: fallback_reader_proxy.destination_bytes,
        measurement_elapsed: fallback_reader_proxy.measurement_elapsed,
        bytes_per_second: fallback_reader_proxy.destination_bytes as f64
            / fallback_reader_proxy
                .measurement_elapsed
                .as_secs_f64()
                .max(f64::EPSILON),
        terminations_at_reader_terminal: fallback_reader_proxy.terminations,
        natural_termination_wait: fallback_natural_wait,
        terminations_after_teardown: fallback_proxy.terminations(),
        teardown_termination_wait: fallback_teardown_wait,
        control_errors: fallback_proxy.control_errors(),
    };

    let backpressure = metrics
        .websocket_backpressure_events
        .load(Ordering::Relaxed);
    let slow_consumer_evictions = metrics
        .websocket_slow_consumer_disconnects
        .load(Ordering::Relaxed);
    // Printed before the oracles so a RED run still reports the numbers that
    // separate genuine amplification from a link that never kept pace (#212).
    eprintln!(
        "mixed-encoding H14: {}",
        H14Diagnostics {
            fallback: &fallback,
            compatible: &compatible,
            fallback_proxy: &fallback_proxy_observation,
            compatible_proxy: &compatible_proxy_observation,
            burst: BURST,
            sender_elapsed,
            proxy_recv_buffer_bytes: PROXY_RECV_BUFFER_BYTES,
            backpressure,
            slow_consumer_evictions,
        }
    );

    if let Some(failure) =
        first_h14_reader_failure(sender_error.as_deref(), &fallback, &compatible, BURST)
    {
        panic!("{failure}");
    }

    // Conformance assertions run only after both readers and every transport
    // diagnostic have been preserved. A protocol assertion can therefore
    // still fail loudly without poisoning the sibling reader or erasing the
    // first-failure evidence.
    replay_audit(&auditor, "P1", &compatible.audit_frames);
    replay_audit(&auditor, "P2", &fallback.audit_frames);

    assert!(
        fallback.player_left.is_empty(),
        "fallback recipient observed compatible-recipient eviction: {:?}",
        fallback.player_left
    );
    assert!(
        fallback
            .errors
            .iter()
            .all(|error| matches!(&error.code, Some(ErrorCode::UnsupportedGameDataFormat))),
        "fallback recipient observed a non-advisory error: {}",
        summarize_errors(&fallback.errors)
    );
    assert_eq!(
        usize::try_from(fallback.advisories).expect("advisory count fits usize"),
        fallback.errors.len(),
        "every fallback Error must be an UnsupportedGameDataFormat advisory"
    );
    assert!(
        compatible.errors.is_empty(),
        "compatible recipient observed unexpected server errors: {}",
        summarize_errors(&compatible.errors)
    );

    // This is the pre-registered falsification oracle. A RED result here means
    // unsupported-format accountability inflates the fallback stream enough to
    // evict a recipient that survives the same throttle on compact binary
    // delivery.
    assert!(
        fallback.terminal.is_complete(),
        "unsupported-format amplification evicted only the JSON fallback recipient after \
         {}/{BURST} accounted sequences and {} advisories ({:?})",
        fallback.accounted,
        fallback.advisories,
        fallback.terminal
    );
    // Exactness is a property of the *sequences* named, not of the frame count:
    // one coalesced range may account for a whole burst of omissions. The
    // conformance auditor independently proves those ranges never overlap and
    // never leave a hole.
    assert_eq!(
        fallback.accounted, BURST,
        "every omitted sequence needs exact accounting"
    );
    assert!(
        fallback.advisories > 0,
        "the first supplemental advisory must be visible"
    );
    // The advisory limiter admits at most one notice per sender per second;
    // allow one extra for the immediate first notice and one for rounding.
    let advisory_ceiling = fallback.elapsed.as_secs() + 2;
    assert!(
        fallback.advisories <= advisory_ceiling,
        "advisory rate limiting was ineffective: {} advisories in {:?}",
        fallback.advisories,
        fallback.elapsed
    );
    // The amplification invariant, and the whole point of H14: accounting for
    // undeliverable game data must not cost the weaker recipient more wire
    // bytes than delivering the payload costs the compatible one. Before the
    // reports were coalesced this ratio was ~8x, which is what evicted the
    // fallback recipient under an equal bandwidth fault.
    assert!(
        fallback.wire_bytes <= compatible.wire_bytes,
        "unsupported-format accountability cost the fallback recipient {} bytes against \
         {} bytes of compact binary delivery ({:.2}x amplification)",
        fallback.wire_bytes,
        compatible.wire_bytes,
        fallback.wire_bytes as f64 / compatible.wire_bytes.max(1) as f64
    );
    assert!(
        compatible.terminal.is_complete(),
        "compatible recipient did not complete the equal-throttle control: {:?}",
        compatible.terminal
    );
    assert_eq!(
        compatible.delivered, BURST,
        "compatible recipient must receive every compact binary delivery"
    );
    assert!(
        compatible.player_left.is_empty(),
        "compatible recipient observed fallback-recipient eviction: {:?}",
        compatible.player_left
    );
    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        0
    );
    // Non-vacuity: the injected bandwidth fault must actually have reached the
    // server's outbound queues. A run where kernel buffering absorbed the whole
    // experiment would prove nothing about amplification.
    assert!(
        backpressure > 0,
        "the throttle never produced server-side backpressure; the experiment was vacuous"
    );
    auditor.assert_conformance(&metrics, &[]).await;

    drop(sender);
    drop(compatible_proxy);
    drop(fallback_proxy);
    running_server.shutdown().await;
}
