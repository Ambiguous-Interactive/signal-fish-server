//! Nightly soak verification of the relay delivery contract under sustained,
//! oscillating load (issue #131 hardening).
//!
//! Every test here is `#[ignore]`d: they are minutes-scale soak runs executed
//! by the nightly verification lane
//! (`.github/workflows/verification-nightly.yml`, `--run-ignored all`), not by
//! PR CI — the `#[ignore]` is a scheduling decision, not a flakiness mask.
//! The PR-lane counterparts are `tests/relay_backpressure_e2e.rs` (burst
//! shapes) and `tests/relay_chaos_e2e.rs` (fault injection).
//!
//! Where the burst suite asserts one flood/one drain, these soaks run paced
//! senders against receivers whose drain rate oscillates for the whole run,
//! repeatedly driving the per-connection queue across full and back, and then
//! assert the terminal ledger: every receiver either holds the exact gap-free
//! `0..sent` stream from every sender or was evicted loudly (metrics + close).
//! Waits are event/metric-driven with generous ceilings; the only timers are
//! the paced-send intervals and the oscillating receiver's deliberate drain
//! stalls — the workload itself, not synchronization.
//!
//! Tests carry `#[serial_test::serial]`: under a plain
//! `cargo test -- --ignored` both soaks would otherwise flood the same
//! process concurrently and CPU-starve each other's paced drains (under
//! nextest each test is its own process and the lock is a no-op).

mod test_helpers;
mod websocket_test_helpers;

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::config::ProtocolConfig;
use signal_fish_server::protocol::{ClientMessage, ErrorCode, ServerMessage};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::create_router;
use test_helpers::create_test_server_with_config;
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::delivery_ledger::{
    extract, DeliveryLedger, DisconnectReason, LedgerPayload, ReceiverExpectation,
    SenderExpectation,
};
use websocket_test_helpers::{assert_message_conservation, connect_with_small_recv_buffer};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
type WsReceiver = futures_util::stream::SplitStream<WsStream>;

/// Sent-totals broadcast: `None` while senders are still running, then the
/// final per-sender counts. Drain tasks exit the moment their ledger matches.
type SentTotals = Option<BTreeMap<String, u64>>;

/// Messages each paced sender emits per soak.
const MESSAGES_PER_SENDER: u64 = 3_000;

/// Paced-send interval (2ms -> 500 msg/s per sender; the workload's clock).
const SEND_INTERVAL: tokio::time::Duration = tokio::time::Duration::from_millis(2);

/// Payload padding, sized so bursts cannot hide in kernel socket buffers and
/// the per-connection queue (capacity 64 below) does the absorbing.
const PAYLOAD_PADDING_BYTES: usize = 2_048;

/// Requested `SO_RCVBUF` for deliberately slow receivers, clamped pre-connect
/// so their kernel window saturates within a handful of messages.
const SLOW_RECEIVER_RECV_BUFFER_BYTES: u32 = 4_096;

/// Whole-test ceiling: generous headroom over the ~30s expected wall time so
/// only a genuine wedge (lost messages, missed eviction) can spend it.
const SOAK_DEADLINE: tokio::time::Duration = tokio::time::Duration::from_secs(90);

async fn start_server(server: Arc<EnhancedGameServer>) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("read listener address");

    let router = create_router("http://localhost:3000").with_state(server);
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("test server serve loop");
    });

    addr
}

async fn connect(addr: std::net::SocketAddr) -> (WsSink, WsReceiver) {
    let url = format!("ws://{addr}/ws");
    let (stream, _response) =
        tokio::time::timeout(tokio::time::Duration::from_secs(10), connect_async(&url))
            .await
            .expect("websocket connect timed out")
            .expect("websocket connect failed");
    stream.split()
}

/// Soak server config: a small queue so the oscillating drain genuinely
/// crosses full many times per run, with the grace window per variant.
fn soak_config(slow_consumer_timeout_ms: u64) -> ServerConfig {
    let mut config = ServerConfig::default();
    config.websocket_config.send_queue_capacity = 64;
    config.websocket_config.slow_consumer_timeout_ms = slow_consumer_timeout_ms;
    config
}

async fn join_room(sink: &mut WsSink, receiver: &mut WsReceiver, room: &str, player_name: &str) {
    let join = ClientMessage::JoinRoom {
        game_name: "soak_game".to_string(),
        room_code: Some(room.to_string()),
        player_name: player_name.to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };
    let json = serde_json::to_string(&join).expect("serialize JoinRoom");
    sink.send(Message::Text(json.into()))
        .await
        .expect("send JoinRoom");

    // Wait for the RoomJoined acknowledgement (lobby chatter may precede it;
    // no GameData can arrive before the flood starts).
    loop {
        let frame = tokio::time::timeout(tokio::time::Duration::from_secs(10), receiver.next())
            .await
            .expect("timed out waiting for RoomJoined")
            .expect("connection closed while joining room")
            .expect("websocket error while joining room");
        let Message::Text(text) = frame else {
            continue;
        };
        let message: ServerMessage = serde_json::from_str(&text).expect("valid ServerMessage");
        match message {
            ServerMessage::RoomJoined(_) => return,
            ServerMessage::RoomJoinFailed { reason, .. } => {
                panic!("room join failed for {player_name}: {reason}")
            }
            _ => continue,
        }
    }
}

/// Record one websocket frame into the ledger. Server errors fail loudly so
/// a drop can never masquerade as lobby chatter; non-GameData control frames
/// are irrelevant to the delivery contract and are skipped.
fn record_frame(ledger: &DeliveryLedger, receiver_name: &str, text: &str) {
    let message: ServerMessage = serde_json::from_str(text).expect("valid ServerMessage");
    match message {
        ServerMessage::GameData { data, .. } => {
            let (sender, seq) = extract(&data).unwrap_or_else(|| {
                panic!("{receiver_name}: GameData without ledger fields: {data}")
            });
            // `server_seq: None` until the server stamps per-connection
            // delivery sequences (see the ledger's cross-check hook).
            ledger.record(receiver_name, &sender, seq, None);
        }
        ServerMessage::Error {
            message,
            error_code,
        } => panic!("{receiver_name}: server error mid-soak: {message} ({error_code:?})"),
        _ => {}
    }
}

/// [`record_frame`] variant for the deliberately stalled receiver: the
/// best-effort SLOW_CONSUMER farewell is the one server error this client
/// has earned (returns `true` when it arrives); any other error still fails
/// loudly.
fn record_stalled_frame(ledger: &DeliveryLedger, text: &str) -> bool {
    let message: ServerMessage = serde_json::from_str(text).expect("valid ServerMessage");
    match message {
        ServerMessage::GameData { data, .. } => {
            let (sender, seq) = extract(&data)
                .unwrap_or_else(|| panic!("Stalled: GameData without ledger fields: {data}"));
            ledger.record("Stalled", &sender, seq, None);
            false
        }
        ServerMessage::Error {
            error_code: Some(ErrorCode::SlowConsumer),
            ..
        } => true,
        ServerMessage::Error {
            message,
            error_code,
        } => panic!("Stalled: unexpected server error mid-soak: {message} ({error_code:?})"),
        _ => false,
    }
}

/// Have this receiver's ledger entries reached the (final) sent totals for
/// every sender it expects? `false` while the totals are still unknown.
fn drained_everything(
    ledger: &DeliveryLedger,
    receiver_name: &str,
    expected_senders: &[String],
    totals: &SentTotals,
) -> bool {
    let Some(totals) = totals else { return false };
    expected_senders.iter().all(|sender| {
        let total = totals
            .get(sender)
            .unwrap_or_else(|| panic!("sent totals missing sender {sender}"));
        ledger.received_count(receiver_name, sender) >= *total
    })
}

/// Steady drain: read frames into the ledger until every expected sender's
/// final total has been observed. The completion condition is event-driven
/// (ledger counts vs. the totals watch); the caller enforces the deadline.
async fn drain_steadily(
    receiver_name: String,
    mut rx: WsReceiver,
    ledger: Arc<DeliveryLedger>,
    expected_senders: Vec<String>,
    mut totals_rx: tokio::sync::watch::Receiver<SentTotals>,
) -> WsReceiver {
    loop {
        let totals = totals_rx.borrow_and_update().clone();
        if drained_everything(&ledger, &receiver_name, &expected_senders, &totals) {
            return rx;
        }
        tokio::select! {
            frame = rx.next() => {
                let frame = frame
                    .unwrap_or_else(|| panic!("{receiver_name}: connection closed mid-drain"))
                    .unwrap_or_else(|error| {
                        panic!("{receiver_name}: websocket error mid-drain: {error}")
                    });
                if let Message::Text(text) = frame {
                    record_frame(&ledger, &receiver_name, &text);
                }
            }
            changed = totals_rx.changed() => {
                changed.unwrap_or_else(|_dropped| {
                    panic!("{receiver_name}: sent-totals channel dropped before totals were set")
                });
            }
        }
    }
}

/// Oscillating drain: read `chunk` frames, stall for `pause` (the workload's
/// deliberate drain gap — smaller than the grace window in the survive
/// variant), repeat until every expected sender's final total is observed.
async fn drain_oscillating(
    receiver_name: String,
    mut rx: WsReceiver,
    ledger: Arc<DeliveryLedger>,
    expected_senders: Vec<String>,
    mut totals_rx: tokio::sync::watch::Receiver<SentTotals>,
    chunk: u32,
    pause: tokio::time::Duration,
) -> WsReceiver {
    let mut read_since_pause = 0u32;
    loop {
        let totals = totals_rx.borrow_and_update().clone();
        if drained_everything(&ledger, &receiver_name, &expected_senders, &totals) {
            return rx;
        }
        if read_since_pause >= chunk {
            read_since_pause = 0;
            // The oscillation itself: a deliberate stall in the workload.
            tokio::time::sleep(pause).await;
        }
        tokio::select! {
            frame = rx.next() => {
                let frame = frame
                    .unwrap_or_else(|| panic!("{receiver_name}: connection closed mid-drain"))
                    .unwrap_or_else(|error| {
                        panic!("{receiver_name}: websocket error mid-drain: {error}")
                    });
                if let Message::Text(text) = frame {
                    record_frame(&ledger, &receiver_name, &text);
                    read_since_pause += 1;
                }
            }
            changed = totals_rx.changed() => {
                changed.unwrap_or_else(|_dropped| {
                    panic!("{receiver_name}: sent-totals channel dropped before totals were set")
                });
            }
        }
    }
}

/// Paced sender: one `GameData` per `SEND_INTERVAL` tick until `count`
/// messages are out. Returns the sink and the exact number sent.
async fn paced_sender(mut sink: WsSink, mut payload: LedgerPayload, count: u64) -> (WsSink, u64) {
    let mut ticker = tokio::time::interval(SEND_INTERVAL);
    for _ in 0..count {
        ticker.tick().await;
        let message = ClientMessage::GameData {
            data: payload.next(),
        };
        let json = serde_json::to_string(&message).expect("serialize GameData");
        sink.send(Message::Text(json.into()))
            .await
            .expect("send paced GameData");
    }
    (sink, payload.sent())
}

fn expectation(receiver: &str, senders: &[(&str, u64)]) -> ReceiverExpectation {
    ReceiverExpectation {
        receiver: receiver.to_string(),
        senders: senders
            .iter()
            .map(|(sender, total_sent)| SenderExpectation {
                sender: (*sender).to_string(),
                total_sent: *total_sent,
            })
            .collect(),
    }
}

/// Two paced senders + one receiver whose drain oscillates (drain a chunk,
/// stall 250ms, repeat — each stall far inside the 5s grace window) + one
/// healthy receiver, for the full sender budget. The oscillator's clamped
/// kernel window plus the 64-slot queue force the queue across full on every
/// stall, so this proves the backpressure path *sustains*: nobody is
/// disconnected, both receivers (and both senders, who receive each other's
/// stream) end gap-free and complete, and the queue demonstrably crossed
/// full (`backpressure_events > 0` — the whole point of the oscillation).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
#[ignore = "nightly verification lane: minutes-scale soak (see module docs)"]
async fn soak_backpressure_survives_oscillating_drain() {
    const ROOM: &str = "SOAKBP";
    const OSCILLATION_CHUNK: u32 = 150;
    const OSCILLATION_PAUSE: tokio::time::Duration = tokio::time::Duration::from_millis(250);

    // Grace window 5s: 20x the oscillation pause, so eviction is impossible
    // unless the drain genuinely stops (which this variant never does).
    let server =
        create_test_server_with_config(soak_config(5_000), ProtocolConfig::default()).await;
    let metrics = server.metrics();
    let addr = start_server(server).await;

    let soak = async {
        let (mut sender_one_sink, mut sender_one_rx) = connect(addr).await;
        let (mut sender_two_sink, mut sender_two_rx) = connect(addr).await;
        let (mut oscillating_sink, mut oscillating_rx) =
            connect_with_small_recv_buffer(addr, SLOW_RECEIVER_RECV_BUFFER_BYTES)
                .await
                .split();
        let (mut healthy_sink, mut healthy_rx) = connect(addr).await;

        join_room(&mut sender_one_sink, &mut sender_one_rx, ROOM, "SenderOne").await;
        join_room(&mut sender_two_sink, &mut sender_two_rx, ROOM, "SenderTwo").await;
        join_room(
            &mut oscillating_sink,
            &mut oscillating_rx,
            ROOM,
            "Oscillator",
        )
        .await;
        join_room(&mut healthy_sink, &mut healthy_rx, ROOM, "Healthy").await;

        let ledger = Arc::new(DeliveryLedger::new());
        let (totals_tx, totals_rx) = tokio::sync::watch::channel::<SentTotals>(None);

        // Every room member receives the OTHER members' streams; senders
        // drain (and are asserted on) too, so a sender wedging as a slow
        // consumer could never hide.
        let drains = [
            ("SenderOne", vec!["SenderTwo".to_string()], sender_one_rx),
            ("SenderTwo", vec!["SenderOne".to_string()], sender_two_rx),
            (
                "Healthy",
                vec!["SenderOne".to_string(), "SenderTwo".to_string()],
                healthy_rx,
            ),
        ];
        let [sender_one_drain, sender_two_drain, healthy_drain] =
            drains.map(|(name, senders, rx)| {
                tokio::spawn(drain_steadily(
                    name.to_string(),
                    rx,
                    Arc::clone(&ledger),
                    senders,
                    totals_rx.clone(),
                ))
            });
        let oscillating_drain = tokio::spawn(drain_oscillating(
            "Oscillator".to_string(),
            oscillating_rx,
            Arc::clone(&ledger),
            vec!["SenderOne".to_string(), "SenderTwo".to_string()],
            totals_rx.clone(),
            OSCILLATION_CHUNK,
            OSCILLATION_PAUSE,
        ));

        let sender_one = tokio::spawn(paced_sender(
            sender_one_sink,
            LedgerPayload::new("SenderOne", PAYLOAD_PADDING_BYTES),
            MESSAGES_PER_SENDER,
        ));
        let sender_two = tokio::spawn(paced_sender(
            sender_two_sink,
            LedgerPayload::new("SenderTwo", PAYLOAD_PADDING_BYTES),
            MESSAGES_PER_SENDER,
        ));

        let (_sink_one, sent_one) = sender_one.await.expect("sender one task panicked");
        let (_sink_two, sent_two) = sender_two.await.expect("sender two task panicked");
        totals_tx.send_replace(Some(BTreeMap::from([
            ("SenderOne".to_string(), sent_one),
            ("SenderTwo".to_string(), sent_two),
        ])));

        let _rx1 = sender_one_drain.await.expect("sender one drain panicked");
        let _rx2 = sender_two_drain.await.expect("sender two drain panicked");
        let _rx3 = healthy_drain.await.expect("healthy drain panicked");
        let _rx4 = oscillating_drain.await.expect("oscillating drain panicked");

        (ledger, sent_one, sent_two)
    };
    let (ledger, sent_one, sent_two) = tokio::time::timeout(SOAK_DEADLINE, soak)
        .await
        .expect("oscillating-drain soak exceeded its deadline (lost messages or a wedge?)");

    assert_eq!(sent_one, MESSAGES_PER_SENDER);
    assert_eq!(sent_two, MESSAGES_PER_SENDER);

    // The point of the oscillation: the queue must have genuinely crossed
    // full (deliveries parked) — otherwise this soak proved nothing.
    let backpressure_events = metrics
        .websocket_backpressure_events
        .load(Ordering::Relaxed);
    assert!(
        backpressure_events > 0,
        "the oscillating drain never drove the queue across full — the soak is vacuous \
         (tighten the queue or the oscillation)"
    );
    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        0,
        "an oscillating-but-draining consumer must never be evicted"
    );
    assert_eq!(
        metrics.websocket_messages_dropped.load(Ordering::Relaxed),
        0,
        "nothing may be dropped while every consumer keeps draining"
    );

    ledger.assert_zero_loss_or_loud_disconnect(
        &metrics,
        &[
            expectation("SenderOne", &[("SenderTwo", sent_two)]),
            expectation("SenderTwo", &[("SenderOne", sent_one)]),
            expectation(
                "Healthy",
                &[("SenderOne", sent_one), ("SenderTwo", sent_two)],
            ),
            expectation(
                "Oscillator",
                &[("SenderOne", sent_one), ("SenderTwo", sent_two)],
            ),
        ],
    );
    assert_message_conservation(&metrics).await;
}

/// Same shape, but the slow receiver's stall OUTLASTS the grace window: it
/// drains a warm-up chunk and then stops reading entirely (a stall strictly
/// longer than the 500ms timeout, held until the server reacts — the
/// metric-driven equivalent of "pause 2s with a 500ms timeout"). The server
/// must evict it loudly — exactly one slow-consumer disconnect, the abandoned
/// tail counted as drops — while the healthy receiver and both senders end
/// gap-free and complete, and conservation holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
#[ignore = "nightly verification lane: minutes-scale soak (see module docs)"]
async fn soak_stalled_drain_is_evicted_loudly() {
    const ROOM: &str = "SOAKEV";
    /// Messages the stalled receiver drains before stopping: enough to prove
    /// it held a healthy gap-free prefix, small enough that the stall lands
    /// mid-flood.
    const STALL_AFTER_MESSAGES: u64 = 100;
    /// Floor on each sender's output before eviction may end the flood, so
    /// the healthy receiver's completeness is asserted over a real soak.
    const MIN_MESSAGES_BEFORE_EVICTION: u64 = 500;
    /// Post-eviction tail per sender: proves the room keeps flowing after.
    const POST_EVICTION_TAIL: u64 = 50;
    /// Loud cap: reached only if eviction is broken (the test then fails on
    /// the eviction assertions rather than flooding forever).
    const SENDER_MESSAGE_CAP: u64 = 10_000;

    let server = create_test_server_with_config(soak_config(500), ProtocolConfig::default()).await;
    let metrics = server.metrics();
    let addr = start_server(server).await;

    let soak = async {
        let (mut sender_one_sink, mut sender_one_rx) = connect(addr).await;
        let (mut sender_two_sink, mut sender_two_rx) = connect(addr).await;
        let (mut stalled_sink, mut stalled_rx) =
            connect_with_small_recv_buffer(addr, SLOW_RECEIVER_RECV_BUFFER_BYTES)
                .await
                .split();
        let (mut healthy_sink, mut healthy_rx) = connect(addr).await;

        join_room(&mut sender_one_sink, &mut sender_one_rx, ROOM, "SenderOne").await;
        join_room(&mut sender_two_sink, &mut sender_two_rx, ROOM, "SenderTwo").await;
        join_room(&mut stalled_sink, &mut stalled_rx, ROOM, "Stalled").await;
        join_room(&mut healthy_sink, &mut healthy_rx, ROOM, "Healthy").await;

        let ledger = Arc::new(DeliveryLedger::new());
        let (totals_tx, totals_rx) = tokio::sync::watch::channel::<SentTotals>(None);

        // The stalled receiver: drain a warm-up chunk, stop reading until the
        // server terminates the connection, recording everything that was
        // already delivered on the way out (still a gap-free prefix). Its
        // frames go through `record_stalled_frame`, which tolerates the one
        // server error this client has EARNED — the best-effort SLOW_CONSUMER
        // farewell — while any other error still fails loudly.
        let stall_ledger = Arc::clone(&ledger);
        let stall_metrics = Arc::clone(&metrics);
        let stalled_drain = tokio::spawn(async move {
            let mut drained = 0u64;
            let mut saw_farewell = false;
            // Warm-up drain; exits early if the farewell already arrived
            // (possible on a starved runner where the warm-up itself stalls
            // past the 500ms grace window — still a valid eviction).
            while drained < STALL_AFTER_MESSAGES && !saw_farewell {
                let frame = stalled_rx
                    .next()
                    .await
                    .expect("Stalled: connection closed during warm-up drain")
                    .expect("Stalled: websocket error during warm-up drain");
                if let Message::Text(text) = frame {
                    saw_farewell = record_stalled_frame(&stall_ledger, &text);
                    drained += 1;
                }
            }

            // The stall: no reads at all until the eviction is visible in
            // metrics (strictly longer than the 500ms grace window by
            // construction — the server itself defines when it ends).
            loop {
                if stall_metrics
                    .websocket_slow_consumer_disconnects
                    .load(Ordering::Relaxed)
                    >= 1
                {
                    break;
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;
            }

            // Resume reads to observe the termination the server owes us:
            // buffered frames (recorded — they were delivered pre-eviction),
            // the farewell if it squeezed through, then close/EOF/error.
            // Anything but a hang.
            loop {
                match stalled_rx.next().await {
                    Some(Ok(Message::Text(text))) => {
                        let _farewell = record_stalled_frame(&stall_ledger, &text);
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => continue,
                }
            }
            stall_ledger
                .note_receiver_disconnected("Stalled", DisconnectReason::SlowConsumerEviction);
        });

        let drains = [
            ("SenderOne", vec!["SenderTwo".to_string()], sender_one_rx),
            ("SenderTwo", vec!["SenderOne".to_string()], sender_two_rx),
            (
                "Healthy",
                vec!["SenderOne".to_string(), "SenderTwo".to_string()],
                healthy_rx,
            ),
        ];
        let [sender_one_drain, sender_two_drain, healthy_drain] =
            drains.map(|(name, senders, rx)| {
                tokio::spawn(drain_steadily(
                    name.to_string(),
                    rx,
                    Arc::clone(&ledger),
                    senders,
                    totals_rx.clone(),
                ))
            });

        // Paced senders, metric-driven stop: flood until the eviction has
        // been recorded (and a soak-worthy floor reached), then a short tail
        // to prove the room keeps flowing without the evicted peer.
        let senders = [
            ("SenderOne", sender_one_sink),
            ("SenderTwo", sender_two_sink),
        ];
        let [sender_one, sender_two] = senders.map(|(name, mut sink)| {
            let mut payload = LedgerPayload::new(name, PAYLOAD_PADDING_BYTES);
            let metrics = Arc::clone(&metrics);
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(SEND_INTERVAL);
                let mut tail_remaining: Option<u64> = None;
                loop {
                    ticker.tick().await;
                    let message = ClientMessage::GameData {
                        data: payload.next(),
                    };
                    let json = serde_json::to_string(&message).expect("serialize GameData");
                    sink.send(Message::Text(json.into()))
                        .await
                        .expect("send paced GameData while a peer is stalled");

                    match tail_remaining.as_mut() {
                        Some(0) => break,
                        Some(remaining) => *remaining -= 1,
                        None => {
                            let evicted = metrics
                                .websocket_slow_consumer_disconnects
                                .load(Ordering::Relaxed)
                                >= 1;
                            if (evicted && payload.sent() >= MIN_MESSAGES_BEFORE_EVICTION)
                                || payload.sent() >= SENDER_MESSAGE_CAP
                            {
                                tail_remaining = Some(POST_EVICTION_TAIL);
                            }
                        }
                    }
                }
                (sink, payload.sent())
            })
        });

        let (_sink_one, sent_one) = sender_one.await.expect("sender one task panicked");
        let (_sink_two, sent_two) = sender_two.await.expect("sender two task panicked");
        assert!(
            sent_one < SENDER_MESSAGE_CAP && sent_two < SENDER_MESSAGE_CAP,
            "sender cap reached without an eviction being recorded \
             (sent {sent_one}/{sent_two}) — the slow-consumer timeout never fired"
        );
        totals_tx.send_replace(Some(BTreeMap::from([
            ("SenderOne".to_string(), sent_one),
            ("SenderTwo".to_string(), sent_two),
        ])));

        let _rx1 = sender_one_drain.await.expect("sender one drain panicked");
        let _rx2 = sender_two_drain.await.expect("sender two drain panicked");
        let _rx3 = healthy_drain.await.expect("healthy drain panicked");
        stalled_drain.await.expect("stalled drain panicked");

        (ledger, sent_one, sent_two)
    };
    let (ledger, sent_one, sent_two) = tokio::time::timeout(SOAK_DEADLINE, soak)
        .await
        .expect("stalled-drain soak exceeded its deadline (eviction or drains wedged?)");

    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        1,
        "exactly one connection (the stalled receiver) must be evicted"
    );
    assert!(
        metrics.websocket_messages_dropped.load(Ordering::Relaxed) >= 1,
        "the tail abandoned with the evicted connection must be counted as dropped"
    );
    assert!(
        metrics
            .websocket_backpressure_events
            .load(Ordering::Relaxed)
            > 0,
        "the stall must have parked deliveries (backpressure) before evicting"
    );

    ledger.assert_zero_loss_or_loud_disconnect(
        &metrics,
        &[
            expectation("SenderOne", &[("SenderTwo", sent_two)]),
            expectation("SenderTwo", &[("SenderOne", sent_one)]),
            expectation(
                "Healthy",
                &[("SenderOne", sent_one), ("SenderTwo", sent_two)],
            ),
            expectation(
                "Stalled",
                &[("SenderOne", sent_one), ("SenderTwo", sent_two)],
            ),
        ],
    );
    assert_message_conservation(&metrics).await;
}
