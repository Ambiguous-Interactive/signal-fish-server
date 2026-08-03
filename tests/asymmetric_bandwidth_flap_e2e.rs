//! P10.C H10 — asymmetric downstream bandwidth must be loud or loss-accounted.
//!
//! A 256-kbps recipient is offered roughly 90 KiB/s of relayed game data while
//! a healthy peer shares the room. The same physical link exercises both v3
//! delivery contracts:
//!
//! - reliable traffic eventually exceeds the 15-second sojourn ceiling, makes
//!   the server's `4002 slow_consumer` decision (the close frame is asserted
//!   when the already-congested TCP path preserves it), and can reconnect twice
//!   with the token carried on `RoomJoined` / `Reconnected`;
//! - volatile traffic never parks or evicts the recipient, survives production
//!   transport Pings, and sends causally prior exact `DeliveryReport` ranges
//!   for every sequence gap the peer observes.
//!
//! The server-facing receive window is clamped before connect. Without that
//! control, localhost TCP autotuning can absorb megabytes and turn the test
//! into a measurement of the host kernel's buffer policy instead of the
//! server's queue contract. Sleeps below express the offered traffic duration
//! and cadence; every completion condition is event-driven with a ceiling.

mod test_helpers;
mod websocket_test_helpers;

use futures_util::{Sink, SinkExt, StreamExt};
use signal_fish_server::config::ProtocolConfig;
use signal_fish_server::protocol::{
    ClientMessage, DeliveryClass, DeliveryGap, DeliveryGapReason, PlayerId, ReconnectedPayload,
    ServerMessage,
};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::create_router;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use test_helpers::{create_test_server_with_config, RunningTestServer};
use tokio_tungstenite::tungstenite::{error::ProtocolError, Error as WebSocketError, Message};
use websocket_test_helpers::chaos_proxy::{ChaosProxy, Direction};
use websocket_test_helpers::room16::{self, PlayerHandle};
use websocket_test_helpers::{assert_message_conservation, next_matching_server_message_within};

type WsStream = websocket_test_helpers::WsStream;

const GAME: &str = "h10-asymmetric-bandwidth";
const ROOM: &str = "H10BW1";
const DOWNSTREAM_BYTES_PER_SEC: u64 = 32 * 1_024;
const OFFERED_BYTES_PER_SEC: u64 = 90 * 1_024;
const PROXY_RECV_BUFFER_BYTES: u32 = 4 * 1_024;
const RELIABLE_CYCLES: u64 = 2;
const VOLATILE_DURATION: Duration = Duration::from_secs(60);
const PHASE_DEADLINE: Duration = Duration::from_secs(90);
const EVENT_DEADLINE: Duration = Duration::from_secs(30);
const POST_FAULT_DRAIN_DEADLINE: Duration = Duration::from_secs(60);
const PADDING_BYTES: usize = 512;

#[derive(Default)]
struct WatcherState {
    game_data: AtomicU64,
    victim_departures: AtomicU64,
    max_interarrival: Mutex<Duration>,
    last_game_data_at: Mutex<Option<tokio::time::Instant>>,
}

struct CloseObservation {
    termination: ReliableTermination,
    game_data: u64,
}

enum ReliableTermination {
    SemanticClose { code: u16, reason: String },
    ResetWithoutClosingHandshake,
}

#[derive(Default)]
struct VolatileObservation {
    delivered: u64,
    reported_dropped: u64,
    exact_gaps: Vec<DeliveryGap>,
    last_seq: u64,
    termination: Option<VolatileTermination>,
}

#[derive(Debug)]
enum VolatileTermination {
    Marker,
    Close { code: u16, reason: String },
    TransportError(String),
    Ended,
}

async fn start_server(server: Arc<EnhancedGameServer>) -> RunningTestServer {
    let router = create_router("http://localhost:3000").with_state(server.clone());
    RunningTestServer::spawn(server, router).await
}

async fn join(addr: std::net::SocketAddr, name: &str) -> PlayerHandle {
    let mut ws = room16::connect(addr).await;
    room16::authenticate(&mut ws, 3).await;
    room16::try_join(ws, GAME, ROOM, Some(4), name)
        .await
        .unwrap_or_else(|(reason, code)| panic!("{name} failed to join: {reason} ({code:?})"))
}

async fn send<S>(ws: &mut S, message: &ClientMessage)
where
    S: Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let json = serde_json::to_string(message).expect("serialize ClientMessage");
    ws.send(Message::Text(json.into()))
        .await
        .expect("send ClientMessage");
}

async fn reconnect(
    addr: std::net::SocketAddr,
    player_id: PlayerId,
    room_id: signal_fish_server::protocol::RoomId,
    token: String,
) -> (WsStream, Box<ReconnectedPayload>) {
    let mut ws = room16::connect(addr).await;
    room16::authenticate(&mut ws, 3).await;
    send(
        &mut ws,
        &ClientMessage::Reconnect {
            player_id,
            room_id,
            auth_token: token,
        },
    )
    .await;
    let payload = next_matching_server_message_within(
        &mut ws,
        EVENT_DEADLINE,
        "H10 reconnect response",
        |message| match message {
            ServerMessage::Reconnected(payload) => Some(payload),
            ServerMessage::ReconnectionFailed { reason, error_code } => {
                panic!("H10 reconnect failed: {reason} ({error_code:?})")
            }
            _ => None,
        },
    )
    .await;
    (ws, payload)
}

fn spawn_watcher(
    mut ws: WsStream,
    victim_id: PlayerId,
) -> (
    Arc<WatcherState>,
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<WsStream>,
) {
    let state = Arc::new(WatcherState::default());
    let task_state = Arc::clone(&state);
    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                changed = stop_rx.changed() => {
                    if changed.is_err() || *stop_rx.borrow() {
                        return ws;
                    }
                }
                frame = ws.next() => {
                    let frame = frame
                        .expect("healthy watcher closed during H10")
                        .expect("healthy watcher websocket error during H10");
                    let Message::Text(text) = frame else { continue };
                    match serde_json::from_str::<ServerMessage>(&text)
                        .expect("healthy watcher received valid ServerMessage")
                    {
                        ServerMessage::GameData { .. } => {
                            let now = tokio::time::Instant::now();
                            let mut last = task_state
                                .last_game_data_at
                                .lock()
                                .expect("watcher timestamp lock poisoned");
                            if let Some(previous) = last.replace(now) {
                                let gap = now.saturating_duration_since(previous);
                                let mut max_gap = task_state
                                    .max_interarrival
                                    .lock()
                                    .expect("watcher gap lock poisoned");
                                *max_gap = (*max_gap).max(gap);
                            }
                            task_state.game_data.fetch_add(1, Ordering::Relaxed);
                        }
                        ServerMessage::PlayerLeft { player_id, .. } if player_id == victim_id => {
                            task_state.victim_departures.fetch_add(1, Ordering::Relaxed);
                        }
                        ServerMessage::Error { message, error_code } => {
                            panic!("healthy watcher received server error: {message} ({error_code:?})")
                        }
                        _ => {}
                    }
                }
            }
        }
    });
    (state, stop_tx, task)
}

fn spawn_close_observer(mut ws: WsStream, cycle: u64) -> tokio::task::JoinHandle<CloseObservation> {
    tokio::spawn(async move {
        let mut game_data = 0;
        loop {
            let frame = match ws.next().await {
                Some(Ok(frame)) => frame,
                Some(Err(WebSocketError::Protocol(
                    ProtocolError::ResetWithoutClosingHandshake,
                ))) => {
                    return CloseObservation {
                        termination: ReliableTermination::ResetWithoutClosingHandshake,
                        game_data,
                    };
                }
                Some(Err(WebSocketError::Io(error)))
                    if error.kind() == std::io::ErrorKind::ConnectionReset =>
                {
                    return CloseObservation {
                        termination: ReliableTermination::ResetWithoutClosingHandshake,
                        game_data,
                    };
                }
                Some(Err(error)) => {
                    panic!("reliable cycle {cycle}: victim websocket error before Close: {error}")
                }
                None => panic!("reliable cycle {cycle}: victim ended without Close"),
            };
            match frame {
                Message::Text(text) => {
                    if matches!(
                        serde_json::from_str::<ServerMessage>(&text)
                            .expect("victim received valid ServerMessage"),
                        ServerMessage::GameData { .. }
                    ) {
                        game_data += 1;
                    }
                }
                Message::Close(Some(frame)) => {
                    return CloseObservation {
                        termination: ReliableTermination::SemanticClose {
                            code: frame.code.into(),
                            reason: frame.reason.to_string(),
                        },
                        game_data,
                    };
                }
                Message::Close(None) => {
                    panic!("reliable cycle {cycle}: victim received a bare Close")
                }
                _ => {}
            }
        }
    })
}

fn game_data(n: u64, class: DeliveryClass, marker: bool) -> ClientMessage {
    ClientMessage::GameData {
        class: (class != DeliveryClass::Reliable).then_some(class),
        key: None,
        data: serde_json::json!({
            "n": n,
            "marker": marker,
            "padding": "x".repeat(PADDING_BYTES),
        }),
    }
}

fn offered_cadence() -> Duration {
    let wire_len = serde_json::to_vec(&game_data(0, DeliveryClass::Reliable, false))
        .expect("serialize H10 sizing frame")
        .len() as u64;
    assert!(
        (500..=800).contains(&wire_len),
        "H10 frame shape drifted outside its approximately 600-byte pre-registration: {wire_len}"
    );
    Duration::from_secs_f64(wire_len as f64 / OFFERED_BYTES_PER_SEC as f64)
}

async fn drive_until_departure(
    sender: &mut futures_util::stream::SplitSink<WsStream, Message>,
    watcher: &WatcherState,
    target_departures: u64,
    next_n: &mut u64,
) -> (u64, Duration) {
    let started = tokio::time::Instant::now();
    let deadline = started + PHASE_DEADLINE;
    let mut sent = 0;
    let mut cadence = tokio::time::interval(offered_cadence());
    cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    while watcher.victim_departures.load(Ordering::Relaxed) < target_departures {
        tokio::time::timeout_at(deadline, cadence.tick())
            .await
            .unwrap_or_else(|_| {
                panic!("reliable cycle {target_departures}: victim was not evicted before deadline")
            });
        send(sender, &game_data(*next_n, DeliveryClass::Reliable, false)).await;
        *next_n += 1;
        sent += 1;
    }
    (sent, started.elapsed())
}

async fn poll_until_within(context: &str, timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + timeout;
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{context}: condition not reached before {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn range_is_covered(from: u64, to: u64, gaps: &[DeliveryGap]) -> bool {
    let mut cursor = from;
    let mut ordered: Vec<&DeliveryGap> = gaps.iter().collect();
    ordered.sort_by_key(|gap| gap.from_seq);
    for gap in ordered {
        if gap.to_seq < cursor {
            continue;
        }
        if gap.from_seq > cursor {
            return false;
        }
        cursor = gap.to_seq.saturating_add(1);
        if cursor > to {
            return true;
        }
    }
    cursor > to
}

fn spawn_volatile_observer(
    mut ws: WsStream,
    sender_id: PlayerId,
    baseline_seq: u64,
) -> tokio::task::JoinHandle<VolatileObservation> {
    tokio::spawn(async move {
        let mut observation = VolatileObservation {
            last_seq: baseline_seq,
            ..VolatileObservation::default()
        };
        loop {
            let frame = match ws.next().await {
                Some(Ok(frame)) => frame,
                Some(Err(error)) => {
                    observation.termination =
                        Some(VolatileTermination::TransportError(error.to_string()));
                    return observation;
                }
                None => {
                    observation.termination = Some(VolatileTermination::Ended);
                    return observation;
                }
            };
            match frame {
                Message::Text(text) => match serde_json::from_str::<ServerMessage>(&text)
                    .expect("volatile victim received valid ServerMessage")
                {
                    ServerMessage::DeliveryReport(report) => {
                        observation.reported_dropped = observation
                            .reported_dropped
                            .max(report.per_class.volatile.dropped);
                        for gap in report.gaps {
                            if gap.from_player == sender_id {
                                assert_eq!(gap.reason, DeliveryGapReason::VolatileDropped);
                                observation.exact_gaps.push(gap);
                            }
                        }
                    }
                    ServerMessage::GameData {
                        from_player,
                        seq: Some(seq),
                        class,
                        data,
                        ..
                    } if from_player == sender_id => {
                        assert!(
                            seq > observation.last_seq,
                            "volatile stream moved backward/duplicated: {} -> {seq}",
                            observation.last_seq
                        );
                        if seq > observation.last_seq.saturating_add(1) {
                            let missing_from = observation.last_seq + 1;
                            let missing_to = seq - 1;
                            assert!(
                                range_is_covered(
                                    missing_from,
                                    missing_to,
                                    &observation.exact_gaps
                                ),
                                "volatile gap [{missing_from}..={missing_to}] lacked a causally prior exact DeliveryReport; reports={:?}",
                                observation.exact_gaps
                            );
                        }
                        assert!(
                            !observation
                                .exact_gaps
                                .iter()
                                .any(|gap| { gap.from_seq <= seq && seq <= gap.to_seq }),
                            "server delivered seq {seq} after reporting it omitted"
                        );
                        observation.last_seq = seq;
                        if class == Some(DeliveryClass::Volatile) {
                            observation.delivered += 1;
                        }
                        if data.get("marker").and_then(serde_json::Value::as_bool) == Some(true) {
                            observation.termination = Some(VolatileTermination::Marker);
                            return observation;
                        }
                    }
                    ServerMessage::Error {
                        message,
                        error_code,
                    } => {
                        panic!("volatile victim received server error: {message} ({error_code:?})")
                    }
                    _ => {}
                },
                Message::Close(Some(frame)) => {
                    observation.termination = Some(VolatileTermination::Close {
                        code: frame.code.into(),
                        reason: frame.reason.to_string(),
                    });
                    return observation;
                }
                Message::Close(None) => {
                    panic!("volatile victim received a bare Close frame")
                }
                _ => {}
            }
        }
    })
}

/// Reliable traffic flaps under sustained asymmetric bandwidth, but every
/// eviction is explicit and both reconnect tokens are usable. The volatile
/// phase then proves the slow-but-draining link stays connected and every
/// intentional volatile omission remains exact and observable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
#[ignore = "nightly-only (verification-nightly.yml): two default 15s sojourn cycles plus a 60s volatile stability proof"]
async fn asymmetric_bandwidth_preserves_lossy_delivery_and_control_progress() {
    let config = ServerConfig {
        ping_timeout: Duration::from_secs(60),
        ..ServerConfig::default()
    };
    let server = create_test_server_with_config(config, ProtocolConfig::default()).await;
    let metrics = server.metrics();
    let running = start_server(server).await;
    let proxy =
        ChaosProxy::spawn_with_upstream_recv_buffer(running.addr(), Some(PROXY_RECV_BUFFER_BYTES))
            .await;

    let sender = join(running.addr(), "Sender").await;
    let sender_id = sender.player_id;
    let watcher = join(running.addr(), "Watcher").await;
    let victim = join(proxy.addr(), "Victim").await;
    let victim_id = victim.player_id;
    let room_id = victim.room_joined.room_id;
    let mut reconnect_token = victim
        .room_joined
        .reconnection_token
        .clone()
        .expect("v3 RoomJoined must surface the H10 reconnect token");
    let (watcher_state, watcher_stop, watcher_task) = spawn_watcher(watcher.ws, victim_id);

    // A conforming client must poll both directions: tungstenite processes
    // server WebSocket Ping frames while reading and queues the automatic Pong
    // on the shared socket. Leaving this half idle would make the sender fail
    // the server's independent liveness contract after roughly 15 seconds.
    let (mut sender_sink, mut sender_rx) = sender.ws.split();
    let (sender_stop, mut sender_stop_rx) = tokio::sync::watch::channel(false);
    let sender_reader = tokio::spawn(async move {
        loop {
            tokio::select! {
                changed = sender_stop_rx.changed() => {
                    if changed.is_err() || *sender_stop_rx.borrow() {
                        return;
                    }
                }
                frame = sender_rx.next() => {
                    match frame {
                        Some(Ok(Message::Close(frame))) => {
                            panic!("H10 sender closed unexpectedly: {frame:?}")
                        }
                        Some(Ok(_)) => {}
                        Some(Err(error)) => panic!("H10 sender websocket error: {error}"),
                        None => panic!("H10 sender ended unexpectedly"),
                    }
                }
            }
        }
    });

    let mut victim_ws = victim.ws;
    let mut total_sent = 0;
    let mut next_n = 0;
    let mut cycle_measurements = Vec::new();
    let mut last_reconnected = None;

    for cycle in 1..=RELIABLE_CYCLES {
        proxy.throttle(Direction::ServerToClient, Some(DOWNSTREAM_BYTES_PER_SEC));
        let close_task = spawn_close_observer(victim_ws, cycle);
        let (sent, elapsed) =
            drive_until_departure(&mut sender_sink, &watcher_state, cycle, &mut next_n).await;
        total_sent += sent;

        // Remove the workload fault only after the healthy watcher proves the
        // room observed the departure. This lets the victim drain buffered
        // bytes and expose the semantic close frame promptly.
        proxy.throttle(Direction::ServerToClient, None);
        let close = tokio::time::timeout(EVENT_DEADLINE, close_task)
            .await
            .unwrap_or_else(|_| panic!("reliable cycle {cycle}: close frame stayed buried"))
            .expect("reliable close observer panicked");
        match &close.termination {
            ReliableTermination::SemanticClose { code, reason } => {
                assert_eq!(*code, 4002, "reliable cycle {cycle}: wrong close code");
                assert_eq!(
                    reason, "slow_consumer",
                    "reliable cycle {cycle}: wrong close reason"
                );
            }
            ReliableTermination::ResetWithoutClosingHandshake => {
                assert_eq!(
                    metrics
                        .websocket_slow_consumer_disconnects
                        .load(Ordering::Relaxed),
                    cycle,
                    "reliable cycle {cycle}: transport reset lacked the independent server-side slow-consumer decision"
                );
                eprintln!(
                    "H10 reliable cycle {cycle}: close frame lost after proven slow-consumer decision"
                );
            }
        }
        assert!(
            close.game_data > 0,
            "reliable cycle {cycle}: constrained peer received no data; throttle was vacuous"
        );
        cycle_measurements.push((sent, elapsed, close.game_data));
        eprintln!(
            "H10 reliable cycle {cycle}: sent={sent} delivered_before_close={} elapsed={elapsed:?}",
            close.game_data
        );

        let (new_ws, reconnected) =
            reconnect(proxy.addr(), victim_id, room_id, reconnect_token.clone()).await;
        let rotated = reconnected
            .reconnection_token
            .clone()
            .expect("Reconnected must rotate and surface the next token");
        assert_ne!(
            rotated, reconnect_token,
            "reconnect token must rotate after reliable cycle {cycle}"
        );
        if cycle < RELIABLE_CYCLES {
            reconnect_token = rotated;
        }
        victim_ws = new_ws;
        last_reconnected = Some(reconnected);
    }

    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        RELIABLE_CYCLES,
        "each reliable flap must produce exactly one slow-consumer eviction"
    );
    assert!(
        metrics
            .websocket_backpressure_events
            .load(Ordering::Relaxed)
            > 0,
        "reliable asymmetric traffic never reached queue backpressure"
    );

    // The second successful reconnect is now kept alive under the volatile
    // contract. Its sender watermark is the exact baseline for causal gap
    // validation on this fresh physical connection.
    let sender_watermark = last_reconnected
        .as_ref()
        .expect("the reliable loop must finish on a live reconnect")
        .sender_watermarks
        .iter()
        .find(|watermark| watermark.player_id == sender_id)
        .unwrap_or_else(|| panic!("Reconnected omitted the active sender watermark"));
    poll_until_within(
        "healthy watcher drains both reliable cycles",
        EVENT_DEADLINE,
        || watcher_state.game_data.load(Ordering::Relaxed) >= total_sent,
    )
    .await;
    // `victim_ws` is the live second reconnect from the loop.
    let sender_baseline = metrics.delivery_metrics_by_class().reliable.attempted;
    let volatile_attempted_baseline = metrics.delivery_metrics_by_class().volatile.attempted;
    let volatile_observer = spawn_volatile_observer(victim_ws, sender_id, sender_watermark.seq);
    proxy.throttle(Direction::ServerToClient, Some(DOWNSTREAM_BYTES_PER_SEC));
    let volatile_started = tokio::time::Instant::now();
    let volatile_deadline = volatile_started + VOLATILE_DURATION;
    let mut cadence = tokio::time::interval(offered_cadence());
    cadence.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let volatile_start_count = watcher_state.game_data.load(Ordering::Relaxed);
    let mut volatile_sent = 0;
    while tokio::time::Instant::now() < volatile_deadline && !volatile_observer.is_finished() {
        cadence.tick().await;
        send(
            &mut sender_sink,
            &game_data(next_n, DeliveryClass::Volatile, false),
        )
        .await;
        next_n += 1;
        volatile_sent += 1;
    }
    proxy.throttle(Direction::ServerToClient, None);

    // Reliable and Volatile share one FIFO data lane. If the marker is queued
    // behind the overload residue, its reliable sojourn ages while Volatile
    // frames are still draining and the marker itself can cause a third
    // slow-consumer close. First establish that both recipients' Volatile
    // fanouts reached terminal accounting, then use Reliable as the post-fault
    // stream delimiter it is intended to be.
    let volatile_attempted_target = volatile_attempted_baseline + volatile_sent * 2;
    poll_until_within(
        "volatile fanouts drain before the reliable marker",
        POST_FAULT_DRAIN_DEADLINE,
        || {
            let volatile = metrics.delivery_metrics_by_class().volatile;
            let resolved = volatile.delivered
                + volatile.superseded
                + volatile.dropped_full
                + volatile.dropped
                + volatile.abandoned
                + volatile.unsupported_format;
            volatile.attempted == volatile_attempted_target && resolved == volatile.attempted
        },
    )
    .await;

    send(
        &mut sender_sink,
        &game_data(next_n, DeliveryClass::Reliable, true),
    )
    .await;
    total_sent += volatile_sent + 1;

    // `sender_sink.send` only hands the marker to the sender socket. Establish
    // that the server ingested and fanned it out on the healthy path before
    // starting the constrained connection's post-fault drain budget; otherwise
    // this deadline also charges unrelated sender/server scheduling delay.
    poll_until_within(
        "healthy watcher receives the complete H10 stream",
        EVENT_DEADLINE,
        || watcher_state.game_data.load(Ordering::Relaxed) >= total_sent,
    )
    .await;

    let volatile = match tokio::time::timeout(EVENT_DEADLINE, volatile_observer).await {
        Ok(observation) => observation.expect("volatile observer panicked"),
        Err(_) => {
            let slow_consumer_disconnects = metrics
                .websocket_slow_consumer_disconnects
                .load(Ordering::Relaxed);
            let ping_timeouts = metrics.websocket_ping_timeouts.load(Ordering::Relaxed);
            let ping_probes_skipped = metrics
                .websocket_ping_probes_skipped_activity
                .load(Ordering::Relaxed);
            let ping_probes_cancelled = metrics
                .websocket_ping_probes_cancelled_activity
                .load(Ordering::Relaxed);
            let expired_players = metrics.expired_players_cleaned.load(Ordering::Relaxed);
            let proxy_terminations = proxy.terminations();
            let delivery = metrics.delivery_metrics_by_class();
            let watcher_delivered = watcher_state.game_data.load(Ordering::Relaxed);
            panic!(
                "volatile victim did not reach the post-fault marker after {volatile_sent} \
                 offers; server recorded {slow_consumer_disconnects} slow-consumer disconnects, \
                 {ping_timeouts} ping timeouts, {ping_probes_skipped} skipped and \
                 {ping_probes_cancelled} cancelled probes, and {expired_players} \
                 activity-reaper evictions; delivery={delivery:?}; healthy watcher \
                 delivered={watcher_delivered}/{total_sent}; proxy terminations: \
                 {proxy_terminations:?}"
            );
        }
    };

    assert_eq!(
        watcher_state.game_data.load(Ordering::Relaxed) - volatile_start_count,
        volatile_sent + 1,
        "healthy watcher must receive every volatile frame plus the marker"
    );
    assert!(
        metrics.delivery_metrics_by_class().volatile.dropped > 0,
        "volatile overload never exercised the lossy queue policy"
    );
    assert!(
        metrics.delivery_metrics_by_class().reliable.attempted > sender_baseline,
        "the final reliable marker did not traverse the delivery layer"
    );
    let slow_consumer_disconnects = metrics
        .websocket_slow_consumer_disconnects
        .load(Ordering::Relaxed);
    let ping_timeouts = metrics.websocket_ping_timeouts.load(Ordering::Relaxed);
    let ping_probes_skipped = metrics
        .websocket_ping_probes_skipped_activity
        .load(Ordering::Relaxed);
    let ping_probes_cancelled = metrics
        .websocket_ping_probes_cancelled_activity
        .load(Ordering::Relaxed);
    let expired_players = metrics.expired_players_cleaned.load(Ordering::Relaxed);
    let proxy_terminations = proxy.terminations();
    match volatile
        .termination
        .as_ref()
        .expect("volatile observer returned without a terminal classification")
    {
        VolatileTermination::Marker => {
            assert_eq!(
                slow_consumer_disconnects, RELIABLE_CYCLES,
                "a stable volatile phase must not add an eviction"
            );
            assert!(
                volatile.reported_dropped > 0,
                "a continuing volatile stream must report every intentional drop"
            );
            assert!(
                !volatile.exact_gaps.is_empty(),
                "a continuing volatile stream must name exact omitted ranges"
            );
        }
        VolatileTermination::Close { code, reason } => panic!(
            "volatile traffic must remain connected through the marker; closed with \
             {code} {reason} after {volatile_sent} offers, {} deliveries, {} reported drops, \
             and {} exact ranges; server recorded {slow_consumer_disconnects} slow-consumer \
             disconnects, {ping_timeouts} ping timeouts, {ping_probes_skipped} skipped and \
             {ping_probes_cancelled} cancelled probes, and {expired_players} activity-reaper \
             evictions; proxy terminations: \
             {proxy_terminations:?}",
            volatile.delivered,
            volatile.reported_dropped,
            volatile.exact_gaps.len(),
        ),
        VolatileTermination::TransportError(error) => panic!(
            "volatile traffic must remain connected through the marker; transport failed with \
             {error} after {volatile_sent} offers, {} deliveries, {} reported drops, and {} \
             exact ranges; server recorded {slow_consumer_disconnects} slow-consumer disconnects; \
             {ping_timeouts} ping timeouts, {ping_probes_skipped} skipped and \
             {ping_probes_cancelled} cancelled probes, and {expired_players} activity-reaper \
             evictions; proxy terminations: \
             {proxy_terminations:?}",
            volatile.delivered,
            volatile.reported_dropped,
            volatile.exact_gaps.len(),
        ),
        VolatileTermination::Ended => panic!(
            "volatile traffic must remain connected through the marker; socket ended after \
             {volatile_sent} offers, {} deliveries, {} reported drops, and {} exact ranges; \
             server recorded {slow_consumer_disconnects} slow-consumer disconnects; proxy \
             observed {ping_timeouts} ping timeouts, {ping_probes_skipped} skipped and \
             {ping_probes_cancelled} cancelled probes, and {expired_players} activity-reaper \
             evictions; proxy terminations: \
             {proxy_terminations:?}",
            volatile.delivered,
            volatile.reported_dropped,
            volatile.exact_gaps.len(),
        ),
    }
    let exact_dropped = volatile
        .exact_gaps
        .iter()
        .map(|gap| gap.to_seq - gap.from_seq + 1)
        .sum::<u64>();
    assert_eq!(
        volatile.delivered + volatile.reported_dropped,
        volatile_sent,
        "the constrained connection's cumulative volatile outcomes must conserve every offer: \
         delivered={} reported_dropped={} exact_dropped={} sent={} exact_ranges={} last_seq={}",
        volatile.delivered,
        volatile.reported_dropped,
        exact_dropped,
        volatile_sent,
        volatile.exact_gaps.len(),
        volatile.last_seq,
    );

    let max_gap = *watcher_state
        .max_interarrival
        .lock()
        .expect("watcher gap lock poisoned");
    eprintln!(
        "H10 result: downstream={}B/s offered={}B/s reliable_cycles={:?} \
         volatile_outcome=stable_through_marker volatile_sent={} volatile_delivered={} volatile_dropped={} exact_ranges={} \
         healthy_max_interarrival={max_gap:?}",
        DOWNSTREAM_BYTES_PER_SEC,
        OFFERED_BYTES_PER_SEC,
        cycle_measurements,
        volatile_sent,
        volatile.delivered,
        volatile.reported_dropped,
        volatile.exact_gaps.len(),
    );

    watcher_stop.send_replace(true);
    let _watcher_ws = watcher_task.await.expect("watcher task panicked");
    sender_stop.send_replace(true);
    sender_reader.await.expect("sender reader task panicked");
    assert_eq!(
        metrics.websocket_ping_timeouts.load(Ordering::Relaxed),
        0,
        "active H10 connections must never fail an idle liveness probe"
    );
    assert!(
        metrics
            .websocket_ping_probes_skipped_activity
            .load(Ordering::Relaxed)
            > 0,
        "H10 sustained inbound traffic never exercised activity-based probe skipping"
    );
    assert_message_conservation(&metrics).await;
    running.shutdown().await;
}
