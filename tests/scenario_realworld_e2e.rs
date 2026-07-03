//! Real-world traffic-profile scenarios for the relay delivery contract
//! (issue #131): rollback-netcode pacing, wifi jitter, backgrounded tabs,
//! reconnect-under-fire, and lobby churn — each expressed as deterministic
//! fault/traffic schedules over real sockets, verified with the delivery
//! ledger ("zero loss or loud disconnect") and the conservation counters.
//!
//! Where the burst suite (`tests/relay_backpressure_e2e.rs`) pins the
//! mechanism (queues, timeouts, eviction) and the chaos suite
//! (`tests/relay_chaos_e2e.rs`) pins transport faults, this binary pins the
//! SHAPES real games produce:
//!
//! - `rollback_profile_short` (PR lane): four 60Hz-paced senders with
//!   deterministic rollback bursts; zero loss, zero evictions.
//! - `wifi_jitter_profile` (PR lane): latency spikes and throughput dips all
//!   inside the slow-consumer window; the contract must NOT be trigger-happy
//!   (zero loss AND zero evictions).
//! - `backgrounded_tab_profile` (nightly, `#[ignore]`): multi-second drain
//!   pauses below the timeout are absorbed; a pause beyond it is evicted
//!   loudly.
//! - `reconnect_under_fire` (PR lane): a player disconnected mid-burst
//!   reconnects inside the window; the v3 `Reconnected.replay` marker is
//!   present, GameData is never replayed, and relay resumes gap-free.
//! - `lobby_churn_during_relay` (PR lane): joins/leaves/ready toggles
//!   interleaved with relay traffic disturb nothing.
//!
//! All pacing intervals and pause durations below are the INJECTED WORKLOAD'S
//! SHAPE (what a 60Hz game loop / a flaky wifi link / a backgrounded tab
//! does), never synchronization: every assertion waits on ledger counts,
//! metrics, or socket events under generous ceilings (zero-flakiness policy,
//! `.llm/context-testing.md`). Every test carries `#[serial_test::serial]`
//! like the sibling flood suites, so plain `cargo test` never co-schedules
//! two floods in one process (under nextest each test is its own process and
//! the lock is a no-op).

mod test_helpers;
mod websocket_test_helpers;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::config::{AppAuthEntry, ProtocolConfig};
use signal_fish_server::protocol::{ClientMessage, PlayerId, ReplayStatus, RoomId, ServerMessage};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::{create_router, websocket_handler_v3};
use test_helpers::{create_test_server, create_test_server_with_config, test_server_config};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::chaos_proxy::{ChaosProxy, Direction};
use websocket_test_helpers::delivery_ledger::{
    extract, DeliveryLedger, DisconnectReason, LedgerPayload, ReceiverExpectation,
    SenderExpectation,
};
use websocket_test_helpers::{assert_message_conservation, next_matching_server_message_within};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
type WsReceiver = futures_util::stream::SplitStream<WsStream>;

const GAME_NAME: &str = "scenario_game";
/// Per-step ceiling for expected events (connects, arrivals, metric
/// convergence). Generous: only a genuine wedge spends it.
const EVENT_DEADLINE: Duration = Duration::from_secs(30);
const SERVER_MESSAGE_TIMEOUT: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// Shared infrastructure (mirrors tests/relay_chaos_e2e.rs).
// ---------------------------------------------------------------------------

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

/// Connect a websocket client to `addr` — the server directly, or a
/// [`ChaosProxy`] address to route the link through fault injection.
async fn connect(addr: std::net::SocketAddr) -> (WsSink, WsReceiver) {
    let url = format!("ws://{addr}/ws");
    let (stream, _response) = tokio::time::timeout(EVENT_DEADLINE, connect_async(&url))
        .await
        .expect("websocket connect timed out")
        .expect("websocket connect failed");
    stream.split()
}

/// Join `room_code` and return the full `RoomJoined` payload (callers that
/// only need the id use [`join_room`]).
async fn join_room_payload(
    sink: &mut WsSink,
    receiver: &mut WsReceiver,
    room_code: &str,
    player_name: &str,
) -> Box<signal_fish_server::protocol::RoomJoinedPayload> {
    let join = ClientMessage::JoinRoom {
        game_name: GAME_NAME.to_string(),
        room_code: Some(room_code.to_string()),
        player_name: player_name.to_string(),
        max_players: Some(4),
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
            ServerMessage::RoomJoined(payload) => return payload,
            ServerMessage::RoomJoinFailed { reason, .. } => {
                panic!("room join failed for {player_name}: {reason}")
            }
            _ => continue,
        }
    }
}

/// Join `room_code` and return the server-assigned player id.
async fn join_room(
    sink: &mut WsSink,
    receiver: &mut WsReceiver,
    room_code: &str,
    player_name: &str,
) -> PlayerId {
    join_room_payload(sink, receiver, room_code, player_name)
        .await
        .player_id
}

/// Record one text frame into the ledger for `receiver_name`; returns any
/// `PlayerLeft` id observed instead. Server errors fail loudly.
fn record_frame(ledger: &DeliveryLedger, receiver_name: &str, text: &str) -> Option<PlayerId> {
    let message: ServerMessage = serde_json::from_str(text).expect("valid ServerMessage");
    match message {
        ServerMessage::GameData { data, .. } => {
            let (sender, seq) = extract(&data).unwrap_or_else(|| {
                panic!("{receiver_name}: GameData without ledger fields: {data}")
            });
            ledger.record(receiver_name, &sender, seq, None);
            None
        }
        ServerMessage::PlayerLeft { player_id } => Some(player_id),
        ServerMessage::Error {
            message,
            error_code,
        } => panic!("{receiver_name}: server error mid-scenario: {message} ({error_code:?})"),
        _ => None,
    }
}

/// Send ONE ledger-shaped GameData frame with an explicit seq and padding
/// size. The backgrounded-tab scenario emits one continuous per-sender
/// sequence whose two phases use different frame sizes, which
/// [`LedgerPayload`]'s fixed padding cannot express.
async fn send_ledger_frame(sink: &mut WsSink, sender: &str, seq: u64, padding_bytes: usize) {
    let message = ClientMessage::GameData {
        data: serde_json::json!({
            "ledger_sender": sender,
            "seq": seq,
            "padding": "x".repeat(padding_bytes),
        }),
    };
    let json = serde_json::to_string(&message).expect("serialize GameData");
    sink.send(Message::Text(json.into()))
        .await
        .expect("send ledger GameData frame");
}

/// Send `count` ledger-tracked GameData messages on `sink`.
async fn send_burst(sink: &mut WsSink, payload: &mut LedgerPayload, count: u64) {
    for _ in 0..count {
        let message = ClientMessage::GameData {
            data: payload.next(),
        };
        let json = serde_json::to_string(&message).expect("serialize GameData");
        sink.send(Message::Text(json.into()))
            .await
            .expect("send GameData burst frame");
    }
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

/// Poll `condition` every 10ms until it holds, failing loudly at the ceiling.
/// A ceiling, not an expected wait: returns the instant the state holds.
async fn poll_until(context: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + EVENT_DEADLINE;
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{context}: condition never held within {EVENT_DEADLINE:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Drain `receiver` (recording into the ledger as `receiver_name`) until
/// `done` holds, with a per-frame ceiling. `done` is re-checked before every
/// read so a satisfied condition never blocks on a silent wire.
async fn drain_until(
    receiver: &mut WsReceiver,
    ledger: &DeliveryLedger,
    receiver_name: &str,
    mut done: impl FnMut(&DeliveryLedger) -> bool,
) {
    while !done(ledger) {
        let frame = tokio::time::timeout(EVENT_DEADLINE, receiver.next())
            .await
            .unwrap_or_else(|_elapsed| {
                panic!("{receiver_name}: timed out draining toward the scenario condition")
            })
            .unwrap_or_else(|| panic!("{receiver_name}: connection closed mid-drain"))
            .unwrap_or_else(|error| panic!("{receiver_name}: websocket error mid-drain: {error}"));
        if let Message::Text(text) = frame {
            let _player_left = record_frame(ledger, receiver_name, &text);
        }
    }
}

/// Drain `receiver` (still recording GameData into the ledger) until the
/// `PlayerLeft` broadcast for `expected` is observed — the deterministic
/// phase boundary after a churner departs.
async fn drain_until_player_left(
    receiver: &mut WsReceiver,
    ledger: &DeliveryLedger,
    receiver_name: &str,
    expected: PlayerId,
) {
    loop {
        let frame = tokio::time::timeout(EVENT_DEADLINE, receiver.next())
            .await
            .unwrap_or_else(|_elapsed| {
                panic!("{receiver_name}: timed out awaiting PlayerLeft({expected})")
            })
            .unwrap_or_else(|| panic!("{receiver_name}: connection closed awaiting PlayerLeft"))
            .unwrap_or_else(|error| {
                panic!("{receiver_name}: websocket error awaiting PlayerLeft: {error}")
            });
        if let Message::Text(text) = frame {
            if record_frame(ledger, receiver_name, &text) == Some(expected) {
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 1. Rollback profile (PR lane): 4 clients, 60Hz pacing, deterministic
//    rollback bursts, ~8s of wall traffic.
// ---------------------------------------------------------------------------

/// Ticks per sender: ~8 seconds at ~60Hz.
const ROLLBACK_TICKS: u64 = 480;
/// Deterministic rollback schedule: the first 12 ticks of every 60 (a 200ms
/// window each second) send at 3x rate — a rollback resimulation burst —
/// the rest send one input packet. Schedule is a pure function of the tick
/// index, so every run emits the identical stream.
fn rollback_burst_at(tick: u64) -> u64 {
    if tick % 60 < 12 {
        3
    } else {
        1
    }
}
/// Total messages each sender emits under the schedule.
fn rollback_total_per_sender() -> u64 {
    (0..ROLLBACK_TICKS).map(rollback_burst_at).sum()
}

/// Four rollback-netcode peers relay at 60Hz with deterministic burst
/// windows for ~8s: every peer must hold the exact gap-free stream from
/// every other peer, with zero evictions and balanced conservation counters.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn rollback_profile_short() {
    const ROOM: &str = "RBK001";
    const PADDING_BYTES: usize = 128;
    const CLIENTS: usize = 4;

    let server = create_test_server().await;
    let metrics = server.metrics();
    let addr = start_server(server).await;
    let ledger = Arc::new(DeliveryLedger::new());
    let total_per_sender = rollback_total_per_sender();

    let scenario = async {
        let names: Vec<String> = (0..CLIENTS).map(|index| format!("Roll{index}")).collect();
        let mut sinks = Vec::new();
        let mut receivers = Vec::new();
        for name in &names {
            let (mut sink, mut receiver) = connect(addr).await;
            let _id = join_room(&mut sink, &mut receiver, ROOM, name).await;
            sinks.push(sink);
            receivers.push(receiver);
        }

        // Paced senders: a tokio interval models the 60Hz game loop (the
        // injected workload's shape, not a synchronization wait).
        let mut sender_tasks = Vec::new();
        for (index, mut sink) in sinks.into_iter().enumerate() {
            let name = names[index].clone();
            sender_tasks.push(tokio::spawn(async move {
                let mut payload = LedgerPayload::new(&name, PADDING_BYTES);
                let mut cadence = tokio::time::interval(Duration::from_millis(16));
                for tick in 0..ROLLBACK_TICKS {
                    cadence.tick().await;
                    send_burst(&mut sink, &mut payload, rollback_burst_at(tick)).await;
                }
                (sink, payload.sent())
            }));
        }

        // Concurrent drains: each peer records until it holds every other
        // sender's complete stream (ledger-count driven).
        let mut drain_tasks = Vec::new();
        for (index, mut receiver) in receivers.into_iter().enumerate() {
            let my_name = names[index].clone();
            let sender_names: Vec<String> = names
                .iter()
                .filter(|name| **name != my_name)
                .cloned()
                .collect();
            let drain_ledger = Arc::clone(&ledger);
            drain_tasks.push(tokio::spawn(async move {
                drain_until(&mut receiver, &drain_ledger, &my_name, |ledger| {
                    sender_names
                        .iter()
                        .all(|sender| ledger.received_count(&my_name, sender) >= total_per_sender)
                })
                .await;
                receiver
            }));
        }

        for task in sender_tasks {
            let (_sink, sent) = task.await.expect("rollback sender task panicked");
            assert_eq!(sent, total_per_sender, "schedule emitted a fixed total");
        }
        for task in drain_tasks {
            let _receiver = task.await.expect("rollback drain task panicked");
        }
    };
    tokio::time::timeout(Duration::from_secs(120), scenario)
        .await
        .expect("rollback profile exceeded its deadline");

    // Zero evictions: a healthy 60Hz session must never trip the contract.
    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        0,
        "rollback pacing must not trigger slow-consumer evictions"
    );
    let expectations: Vec<ReceiverExpectation> = (0..CLIENTS)
        .map(|index| {
            let receiver = format!("Roll{index}");
            ReceiverExpectation {
                receiver: receiver.clone(),
                senders: (0..CLIENTS)
                    .filter(|sender| format!("Roll{sender}") != receiver)
                    .map(|sender| SenderExpectation {
                        sender: format!("Roll{sender}"),
                        total_sent: total_per_sender,
                    })
                    .collect(),
            }
        })
        .collect();
    ledger.assert_zero_loss_or_loud_disconnect(&metrics, &expectations);
    assert_message_conservation(&metrics).await;
}

// ---------------------------------------------------------------------------
// 2. Wifi jitter (PR lane): latency spikes + throughput dips, all inside the
//    slow-consumer window — the contract must not be trigger-happy.
// ---------------------------------------------------------------------------

/// Deterministic jitter schedule: (pause_ms, gap_ms) latency spikes — every
/// pause is far below the 5s slow-consumer window.
const JITTER_SPIKES_MS: &[(u64, u64)] = &[
    (300, 400),
    (50, 300),
    (250, 400),
    (100, 300),
    (300, 500),
    (150, 400),
];

/// One receiver rides a jittery link (pause spikes of 50-300ms plus a
/// throttle dip) while a 60Hz sender keeps pacing: ZERO loss and ZERO
/// evictions are required — absorbing exactly this is what the queue +
/// grace-window design is for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn wifi_jitter_profile() {
    const ROOM: &str = "WIF001";
    const PADDING_BYTES: usize = 1_024;
    const SEND_TICKS: u64 = 480;
    /// The mid-run throughput dip (bytes/sec) and how long it lasts.
    const DIP_BYTES_PER_SEC: u64 = 32 * 1_024;
    const DIP_DURATION: Duration = Duration::from_millis(1_500);

    let mut server_config = ServerConfig::default();
    server_config.websocket_config.send_queue_capacity = 16;
    server_config.websocket_config.slow_consumer_timeout_ms = 5_000;
    let server = create_test_server_with_config(server_config, ProtocolConfig::default()).await;
    let metrics = server.metrics();
    let addr = start_server(server).await;
    let proxy = ChaosProxy::spawn(addr).await;
    let ledger = Arc::new(DeliveryLedger::new());

    let scenario = async {
        let (mut sender_sink, mut sender_rx) = connect(addr).await;
        let (mut jittery_sink, mut jittery_rx) = connect(proxy.addr()).await;
        let _sender_id = join_room(&mut sender_sink, &mut sender_rx, ROOM, "Sender").await;
        let _jittery_id = join_room(&mut jittery_sink, &mut jittery_rx, ROOM, "Jittery").await;

        // 60Hz paced sender for ~8s (interval = the game loop's shape).
        let sender_task = tokio::spawn(async move {
            let mut payload = LedgerPayload::new("Sender", PADDING_BYTES);
            let mut cadence = tokio::time::interval(Duration::from_millis(16));
            for _tick in 0..SEND_TICKS {
                cadence.tick().await;
                send_burst(&mut sender_sink, &mut payload, 1).await;
            }
            (sender_sink, payload.sent())
        });

        // The jittery receiver drains as the link permits.
        let drain_ledger = Arc::clone(&ledger);
        let drain_task = tokio::spawn(async move {
            drain_until(&mut jittery_rx, &drain_ledger, "Jittery", |ledger| {
                ledger.received_count("Jittery", "Sender") >= SEND_TICKS
            })
            .await;
            jittery_rx
        });

        // The deterministic fault schedule: every sleep below is the injected
        // link's SHAPE (how long the spike/dip lasts), not synchronization —
        // completion is signalled by the drains above.
        for (pause_ms, gap_ms) in JITTER_SPIKES_MS {
            proxy.pause(Direction::ServerToClient);
            tokio::time::sleep(Duration::from_millis(*pause_ms)).await;
            proxy.resume(Direction::ServerToClient);
            tokio::time::sleep(Duration::from_millis(*gap_ms)).await;
        }
        proxy.throttle(Direction::ServerToClient, Some(DIP_BYTES_PER_SEC));
        tokio::time::sleep(DIP_DURATION).await;
        proxy.throttle(Direction::ServerToClient, None);

        let (_sender_sink, sent) = sender_task.await.expect("jitter sender task panicked");
        assert_eq!(sent, SEND_TICKS);
        let _jittery_rx = drain_task.await.expect("jittery drain task panicked");
    };
    tokio::time::timeout(Duration::from_secs(120), scenario)
        .await
        .expect("wifi jitter profile exceeded its deadline");

    // The contract must not be trigger-happy: jitter inside the window means
    // zero evictions AND zero drops AND zero loss.
    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        0,
        "sub-window jitter must never evict the receiver"
    );
    // Deliberately NO raw `websocket_messages_dropped == 0` assert here: that
    // counter also tallies close-time flush abandonment — a trailing broadcast
    // (e.g. PlayerLeft) enqueued for a socket that a departing client already
    // closed — which is teardown accounting, not delivery loss, and races the
    // moment this assertion samples the counter (caught on a loaded CI
    // runner). The zero-loss intent is carried exactly by the three checks
    // around it: zero evictions, the ledger's gap-free completeness, and the
    // conservation law.
    ledger.assert_zero_loss_or_loud_disconnect(
        &metrics,
        &[expectation("Jittery", &[("Sender", SEND_TICKS)])],
    );
    assert_message_conservation(&metrics).await;
}

// ---------------------------------------------------------------------------
// 3. Backgrounded tab (nightly): multi-second drain pauses below the window
//    are absorbed; a pause beyond it is evicted loudly.
// ---------------------------------------------------------------------------

/// Phase 1's drain pauses (all below the 5s window) and per-pause traffic.
const TAB_ABSORBED_PAUSES_MS: &[u64] = &[1_000, 2_000, 4_000];

/// A "backgrounded tab" stops draining for 1-4s at a time under a 5s
/// slow-consumer window: every pause is absorbed with zero loss. Then the
/// tab goes away for good (pause > window) under a flood: it must be evicted
/// loudly, holding a gap-free prefix, while the foreground watcher keeps the
/// complete stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
#[ignore = "nightly-only (verification-nightly.yml): multi-second absorbed pauses plus a full eviction window make this a minutes-scale profile"]
async fn backgrounded_tab_profile() {
    const ROOM: &str = "TAB001";
    /// Phase-1 traffic while the tab is paused.
    const PHASE1_CHUNK: u64 = 40;
    const PHASE1_PADDING_BYTES: usize = 2_048;
    /// Phase-2 flood shape: large frames so kernel buffers fill quickly.
    const PHASE2_PADDING_BYTES: usize = 8 * 1_024;
    const PHASE2_CAP: u64 = 3_000;
    /// Eviction phase ceiling: the 5s window plus generous margin.
    const EVICTION_DEADLINE: Duration = Duration::from_secs(60);

    let mut server_config = ServerConfig::default();
    server_config.websocket_config.send_queue_capacity = 64;
    server_config.websocket_config.slow_consumer_timeout_ms = 5_000;
    let server = create_test_server_with_config(server_config, ProtocolConfig::default()).await;
    let metrics = server.metrics();
    let addr = start_server(server).await;
    let proxy = ChaosProxy::spawn(addr).await;
    let ledger = Arc::new(DeliveryLedger::new());

    let scenario = async {
        let (mut sender_sink, mut sender_rx) = connect(addr).await;
        let (mut watcher_sink, mut watcher_rx) = connect(addr).await;
        let (mut tab_sink, mut tab_rx) = connect(proxy.addr()).await;
        let _sender_id = join_room(&mut sender_sink, &mut sender_rx, ROOM, "Sender").await;
        let _watcher_id = join_room(&mut watcher_sink, &mut watcher_rx, ROOM, "Watcher").await;
        let _tab_id = join_room(&mut tab_sink, &mut tab_rx, ROOM, "Tab").await;

        // The tab drains whenever its link allows, for the whole scenario;
        // when the server finally evicts it (phase 2), it records the
        // eviction. One task spans both phases so its ledger is one stream.
        let tab_ledger = Arc::clone(&ledger);
        let tab_drain = tokio::spawn(async move {
            loop {
                match tab_rx.next().await {
                    Some(Ok(Message::Text(text))) => {
                        let _player_left = record_frame(&tab_ledger, "Tab", &text);
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_control_frame)) => continue,
                }
            }
            tab_ledger.note_receiver_disconnected("Tab", DisconnectReason::SlowConsumerEviction);
        });

        // Watcher drains continuously; its completion target arrives late
        // (totals are only known after phase 2), hence the watch channel.
        let (totals_tx, mut totals_rx) = tokio::sync::watch::channel::<Option<u64>>(None);
        let watcher_ledger = Arc::clone(&ledger);
        let watcher_drain = tokio::spawn(async move {
            loop {
                let total = *totals_rx.borrow_and_update();
                if let Some(total) = total {
                    if watcher_ledger.received_count("Watcher", "Sender") >= total {
                        return watcher_rx;
                    }
                }
                tokio::select! {
                    frame = watcher_rx.next() => {
                        let frame = frame
                            .expect("Watcher: connection closed mid-drain")
                            .expect("Watcher: websocket error mid-drain");
                        if let Message::Text(text) = frame {
                            let _player_left = record_frame(&watcher_ledger, "Watcher", &text);
                        }
                    }
                    changed = totals_rx.changed() => {
                        changed.expect("sent-totals channel dropped before totals were set");
                    }
                }
            }
        });

        // --- Phase 1: absorbed pauses -----------------------------------
        let mut next_seq = 0u64;
        for pause_ms in TAB_ABSORBED_PAUSES_MS {
            proxy.pause(Direction::ServerToClient);
            for _ in 0..PHASE1_CHUNK {
                send_ledger_frame(&mut sender_sink, "Sender", next_seq, PHASE1_PADDING_BYTES).await;
                next_seq += 1;
            }
            // The backgrounded duration itself — the injected fault's shape.
            tokio::time::sleep(Duration::from_millis(*pause_ms)).await;
            proxy.resume(Direction::ServerToClient);
            // Absorption proof, event-driven: once resumed, the tab must
            // catch up to everything sent so far.
            let sent_so_far = next_seq;
            let catchup_ledger = Arc::clone(&ledger);
            poll_until("tab catches up after an absorbed pause", || {
                catchup_ledger.received_count("Tab", "Sender") >= sent_so_far
            })
            .await;
        }
        let phase1_total = next_seq;
        assert_eq!(
            metrics
                .websocket_slow_consumer_disconnects
                .load(Ordering::Relaxed),
            0,
            "every phase-1 pause is below the window and must be absorbed"
        );
        assert_eq!(
            ledger.received_count("Tab", "Sender"),
            phase1_total,
            "phase 1 must end with the tab fully caught up (zero loss)"
        );

        // --- Phase 2: the tab goes away for good -------------------------
        proxy.pause(Direction::ServerToClient);
        let eviction_flood = async {
            let mut seq = next_seq;
            while metrics
                .websocket_slow_consumer_disconnects
                .load(Ordering::Relaxed)
                == 0
            {
                assert!(
                    seq - phase1_total < PHASE2_CAP,
                    "flood cap reached without an eviction — the timeout never fired"
                );
                send_ledger_frame(&mut sender_sink, "Sender", seq, PHASE2_PADDING_BYTES).await;
                seq += 1;
            }
            seq
        };
        let total_sent = tokio::time::timeout(EVICTION_DEADLINE, eviction_flood)
            .await
            .expect("the abandoned tab was never evicted within the window + margin");
        totals_tx.send_replace(Some(total_sent));

        // Resume the link so the evicted tab's socket observes its close.
        proxy.resume(Direction::ServerToClient);
        tokio::time::timeout(EVENT_DEADLINE, tab_drain)
            .await
            .expect("tab socket never observed its eviction")
            .expect("tab drain task panicked");
        let _watcher_rx = tokio::time::timeout(Duration::from_secs(60), watcher_drain)
            .await
            .expect("watcher never completed the stream")
            .expect("watcher drain task panicked");
        total_sent
    };
    let total_sent = tokio::time::timeout(Duration::from_secs(180), scenario)
        .await
        .expect("backgrounded-tab profile exceeded its deadline");

    assert!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed)
            >= 1,
        "the abandoned tab must be evicted loudly"
    );
    ledger.assert_zero_loss_or_loud_disconnect(
        &metrics,
        &[
            expectation("Watcher", &[("Sender", total_sent)]),
            expectation("Tab", &[("Sender", total_sent)]),
        ],
    );
    assert_message_conservation(&metrics).await;
}

// ---------------------------------------------------------------------------
// 4. Reconnect under fire (PR lane): disconnect mid-burst, reconnect inside
//    the window, resume gap-free on a fresh ledger epoch.
// ---------------------------------------------------------------------------

const RECONNECT_APP_ID: &str = "scenario-reconnect-app";

/// v3-capable server for the reconnect scenario: nested `/v2` router plus
/// the `/v3/ws` alias, app auth enabled (mirrors
/// `tests/reconnection_replay_e2e.rs`, the proven reconnect harness), and
/// the in-process handle kept for `register_reconnect_token`.
async fn start_reconnect_server() -> (std::net::SocketAddr, Arc<EnhancedGameServer>) {
    use axum::routing::get;

    let mut server_config = test_server_config();
    server_config.auth_enabled = true;

    let mut protocol_config = ProtocolConfig::default();
    protocol_config.sdk_compatibility.enforce = false;

    let game_server = EnhancedGameServer::new(
        server_config,
        protocol_config,
        signal_fish_server::config::RelayTypeConfig::default(),
        signal_fish_server::config::SessionConfig::default(),
        signal_fish_server::config::TurnConfig::default(),
        signal_fish_server::database::DatabaseConfig::InMemory,
        signal_fish_server::config::MetricsConfig::default(),
        signal_fish_server::config::AuthMaintenanceConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        vec![AppAuthEntry {
            app_id: RECONNECT_APP_ID.to_string(),
            app_secret: "secret".to_string(),
            app_name: "Scenario Reconnect App".to_string(),
            max_rooms: Some(10),
            max_players_per_room: Some(8),
            rate_limit_per_minute: Some(600),
        }],
    )
    .await
    .expect("reconnect scenario server builds");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind reconnect test listener");
    let addr = listener.local_addr().expect("read listener address");
    let enhanced_router = create_router("http://localhost:3000").with_state(game_server.clone());
    let combined_router = axum::Router::new()
        .nest("/v2", enhanced_router)
        .route("/v3/ws", get(websocket_handler_v3))
        .with_state(game_server.clone());
    tokio::spawn(async move {
        axum::serve(
            listener,
            combined_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("reconnect test server serve loop");
    });

    (addr, game_server)
}

async fn connect_v3(addr: std::net::SocketAddr) -> WsStream {
    let url = format!("ws://{addr}/v3/ws");
    let (ws, _response) = tokio::time::timeout(EVENT_DEADLINE, connect_async(&url))
        .await
        .expect("v3 connect timed out")
        .expect("v3 connect failed");
    ws
}

async fn send_on(ws: &mut WsStream, message: &ClientMessage) {
    let json = serde_json::to_string(message).expect("serialize ClientMessage");
    ws.send(Message::Text(json.into()))
        .await
        .expect("send ClientMessage");
}

/// Authenticate as a relay-capable v3 client and consume the
/// `Authenticated` + `ProtocolInfo` pair.
async fn authenticate_v3(ws: &mut WsStream) {
    send_on(
        ws,
        &ClientMessage::Authenticate {
            app_id: RECONNECT_APP_ID.to_string(),
            sdk_version: None,
            platform: None,
            game_data_format: None,
            protocol_version: Some(3),
            supported_transports: None,
            supported_topologies: None,
        },
    )
    .await;
    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "Authenticated", |message| {
        matches!(message, ServerMessage::Authenticated { .. }).then_some(())
    })
    .await;
    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "ProtocolInfo", |message| {
        match message {
            ServerMessage::ProtocolInfo(info) => {
                assert_eq!(info.protocol_version, Some(3), "v3 must be negotiated");
                Some(())
            }
            _ => None,
        }
    })
    .await
}

/// Join `room_code`, returning `(player_id, room_id)`.
async fn join_room_v3(ws: &mut WsStream, room_code: &str, player_name: &str) -> (PlayerId, RoomId) {
    send_on(
        ws,
        &ClientMessage::JoinRoom {
            game_name: GAME_NAME.to_string(),
            room_code: Some(room_code.to_string()),
            player_name: player_name.to_string(),
            max_players: Some(4),
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await;
    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "RoomJoined", |message| {
        match message {
            ServerMessage::RoomJoined(payload) => Some((payload.player_id, payload.room_id)),
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                panic!("room join failed for {player_name}: {reason} ({error_code:?})")
            }
            _ => None,
        }
    })
    .await
}

/// Disconnect `player_id` server-side and mint a reconnect token for it —
/// the wire never carries tokens (the server mints them only when a
/// disconnection is registered), so every reconnect test in this repo drives
/// the disconnect through the in-process handle (the proven
/// `register_reconnect_token` pattern from `tests/v3_ice_pregather_e2e.rs`
/// and `tests/reconnection_replay_e2e.rs`). The victim's socket is closed by
/// the server as part of `disconnect_client`, so from the wire's viewpoint
/// this IS a mid-session cut.
async fn register_reconnect_token(
    game_server: &Arc<EnhancedGameServer>,
    player_id: PlayerId,
    room_id: RoomId,
) -> String {
    let room = game_server
        .database()
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup")
        .expect("room exists");
    let player_info = room
        .players
        .get(&player_id)
        .cloned()
        .expect("player in room before disconnect");

    game_server.disconnect_client(&player_id).await;

    game_server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(player_id, room_id, false, Some(player_info))
        .await
}

/// Drain `ws` (recording GameData into the ledger as `receiver_name`) until
/// `done` holds. Unsplit-stream variant for the reconnect scenario's
/// sequential phases.
async fn drain_ws_until(
    ws: &mut WsStream,
    ledger: &DeliveryLedger,
    receiver_name: &str,
    mut done: impl FnMut(&DeliveryLedger) -> bool,
) {
    while !done(ledger) {
        let frame = tokio::time::timeout(EVENT_DEADLINE, ws.next())
            .await
            .unwrap_or_else(|_elapsed| {
                panic!("{receiver_name}: timed out draining toward the scenario condition")
            })
            .unwrap_or_else(|| panic!("{receiver_name}: connection closed mid-drain"))
            .unwrap_or_else(|error| panic!("{receiver_name}: websocket error mid-drain: {error}"));
        if let Message::Text(text) = frame {
            let _player_left = record_frame(ledger, receiver_name, &text);
        }
    }
}

/// A peer cut off mid-burst reconnects within the window: the v3
/// `Reconnected.replay` completeness marker must be present (`complete` or
/// `truncated` — both legal), GameData must NOT have been replayed, and the
/// relay resumes gap-free on a fresh ledger epoch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn reconnect_under_fire() {
    const ROOM: &str = "RCF001";
    const PADDING_BYTES: usize = 512;
    /// Pre-cut and post-cut halves of the phase-A burst.
    const BURST_HALF: u64 = 100;
    /// The cut lands only after the victim demonstrably consumed this much.
    const VICTIM_PROGRESS_BEFORE_CUT: u64 = 60;
    /// Phase-B (post-reconnect) burst.
    const POST_RECONNECT_BURST: u64 = 100;

    let (addr, game_server) = start_reconnect_server().await;
    let metrics = game_server.metrics();
    let ledger = DeliveryLedger::new();

    let scenario = async {
        let mut sender = connect_v3(addr).await;
        authenticate_v3(&mut sender).await;
        let (_sender_id, _room) = join_room_v3(&mut sender, ROOM, "Sender").await;

        let mut watcher = connect_v3(addr).await;
        authenticate_v3(&mut watcher).await;
        let (_watcher_id, _room) = join_room_v3(&mut watcher, ROOM, "Watcher").await;

        let mut victim = connect_v3(addr).await;
        authenticate_v3(&mut victim).await;
        let (victim_id, room_id) = join_room_v3(&mut victim, ROOM, "Victim").await;

        // --- Phase A: burst, cut mid-burst, keep bursting ----------------
        let mut pre_payload = LedgerPayload::new("SenderPre", PADDING_BYTES);
        for _ in 0..BURST_HALF {
            send_on(
                &mut sender,
                &ClientMessage::GameData {
                    data: pre_payload.next(),
                },
            )
            .await;
        }
        drain_ws_until(&mut victim, &ledger, "Victim", |ledger| {
            ledger.received_count("Victim", "SenderPre") >= VICTIM_PROGRESS_BEFORE_CUT
        })
        .await;

        // The cut: the server disconnects the victim (closing its socket)
        // and mints the reconnect token — see `register_reconnect_token`.
        let token = register_reconnect_token(&game_server, victim_id, room_id).await;

        // The victim's socket observes the cut; whatever arrived before it
        // stays a gap-free prefix.
        let victim_tail = tokio::time::timeout(EVENT_DEADLINE, async {
            loop {
                match victim.next().await {
                    Some(Ok(Message::Text(text))) => {
                        let _player_left = record_frame(&ledger, "Victim", &text);
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                    Some(Ok(_control_frame)) => continue,
                }
            }
        })
        .await;
        victim_tail.expect("victim socket never observed the server-side cut");
        ledger.note_receiver_disconnected(
            "Victim",
            DisconnectReason::InjectedFault("server-side mid-burst disconnect".to_string()),
        );

        // The fire continues: the second half of the burst goes out while
        // the victim is gone.
        for _ in 0..BURST_HALF {
            send_on(
                &mut sender,
                &ClientMessage::GameData {
                    data: pre_payload.next(),
                },
            )
            .await;
        }
        let pre_total = pre_payload.sent();
        // Quiesce phase A: the watcher holds the complete pre-cut stream
        // BEFORE the reconnect, so no phase-A frame can race into the
        // reconnected epoch.
        drain_ws_until(&mut watcher, &ledger, "Watcher", |ledger| {
            ledger.received_count("Watcher", "SenderPre") >= pre_total
        })
        .await;

        // --- Reconnect inside the window ---------------------------------
        let mut reborn = connect_v3(addr).await;
        authenticate_v3(&mut reborn).await;
        send_on(
            &mut reborn,
            &ClientMessage::Reconnect {
                player_id: victim_id,
                room_id,
                auth_token: token,
            },
        )
        .await;
        let reconnected = next_matching_server_message_within(
            &mut reborn,
            SERVER_MESSAGE_TIMEOUT,
            "Reconnected",
            |message| match message {
                ServerMessage::Reconnected(payload) => Some(payload),
                ServerMessage::ReconnectionFailed { reason, error_code } => {
                    panic!("reconnect failed: {reason} ({error_code:?})")
                }
                _ => None,
            },
        )
        .await;

        // v3 contract: the replay-completeness marker is PRESENT and one of
        // the two states a live replay ring can report (`unavailable` would
        // mean event replay was off, which this server config enables).
        let replay = reconnected
            .replay
            .expect("a v3 reconnector must receive the replay completeness marker");
        assert!(
            matches!(replay, ReplayStatus::Complete | ReplayStatus::Truncated),
            "replay must be complete or truncated with an active replay ring, got {replay:?}"
        );
        // GameData is NEVER replayed: reconnectors resync from the snapshot.
        let replayed_game_data: Vec<&ServerMessage> = reconnected
            .missed_events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    ServerMessage::GameData { .. } | ServerMessage::GameDataBinary { .. }
                )
            })
            .collect();
        assert!(
            replayed_game_data.is_empty(),
            "missed_events must never carry GameData: {replayed_game_data:?}"
        );

        // --- Phase B: fresh epoch, gap-free resume ------------------------
        let mut post_payload = LedgerPayload::new("SenderPost", PADDING_BYTES);
        for _ in 0..POST_RECONNECT_BURST {
            send_on(
                &mut sender,
                &ClientMessage::GameData {
                    data: post_payload.next(),
                },
            )
            .await;
        }
        let post_total = post_payload.sent();
        // The reborn receiver records under a FRESH ledger name: any replayed
        // or leaked phase-A GameData would surface as an unexpected
        // `SenderPre` stream on `VictimReborn` and fail the terminal ledger
        // assertion loudly.
        drain_ws_until(&mut reborn, &ledger, "VictimReborn", |ledger| {
            ledger.received_count("VictimReborn", "SenderPost") >= post_total
        })
        .await;
        drain_ws_until(&mut watcher, &ledger, "Watcher", |ledger| {
            ledger.received_count("Watcher", "SenderPost") >= post_total
        })
        .await;

        (pre_total, post_total)
    };
    let (pre_total, post_total) = tokio::time::timeout(Duration::from_secs(120), scenario)
        .await
        .expect("reconnect-under-fire scenario exceeded its deadline");

    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        0,
        "a deliberate disconnect + reconnect must never register as a slow consumer"
    );
    ledger.assert_zero_loss_or_loud_disconnect(
        &metrics,
        &[
            expectation(
                "Watcher",
                &[("SenderPre", pre_total), ("SenderPost", post_total)],
            ),
            // Disconnected mid-burst: any gap-free prefix of the pre-cut
            // stream is legal; a mid-stream hole is not.
            expectation("Victim", &[("SenderPre", pre_total)]),
            // Fresh epoch: ONLY the post-reconnect stream, complete.
            expectation("VictimReborn", &[("SenderPost", post_total)]),
        ],
    );
    assert_message_conservation(&metrics).await;
}

// ---------------------------------------------------------------------------
// 5. Lobby churn during relay (PR lane).
// ---------------------------------------------------------------------------

/// Joins, leaves, and ready toggles interleaved with GameData must disturb
/// nothing: every recipient (including each transient churner while seated)
/// holds the exact gap-free epoch streams, with zero evictions, zero drops,
/// and balanced conservation counters.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn lobby_churn_during_relay() {
    const ROOM: &str = "CHU001";
    const PADDING_BYTES: usize = 512;
    const CHURN_EPOCHS: usize = 4;
    const MESSAGES_PER_EPOCH: u64 = 30;

    let server = create_test_server().await;
    let metrics = server.metrics();
    let addr = start_server(server).await;
    let ledger = DeliveryLedger::new();

    let scenario = async {
        // Three persistent members in a 4-seat room: every churner's join
        // fills the room (Waiting -> Lobby, enabling ready toggles) and its
        // leave re-opens the seat.
        let (mut sender_sink, mut sender_rx) = connect(addr).await;
        let (mut recv_a_sink, mut recv_a_rx) = connect(addr).await;
        let (mut recv_b_sink, mut recv_b_rx) = connect(addr).await;
        let _sender_id = join_room(&mut sender_sink, &mut sender_rx, ROOM, "Sender").await;
        let _recv_a_id = join_room(&mut recv_a_sink, &mut recv_a_rx, ROOM, "RecvA").await;
        let _recv_b_id = join_room(&mut recv_b_sink, &mut recv_b_rx, ROOM, "RecvB").await;

        for epoch in 0..CHURN_EPOCHS {
            let churn_name = format!("Churn{epoch}");
            let epoch_sender = format!("Epoch{epoch}");

            // Join. The room entered Lobby when its FIRST member joined
            // (`transition_room_to_lobby` fires exactly once per room and is
            // never re-broadcast to late joiners), so the churner reads the
            // authoritative lobby state from its own `RoomJoined` payload —
            // the same source the reference clients use — rather than
            // waiting for a `LobbyStateChanged` broadcast that will never
            // come.
            let (mut churn_sink, mut churn_rx) = connect(addr).await;
            let churn_payload =
                join_room_payload(&mut churn_sink, &mut churn_rx, ROOM, &churn_name).await;
            let churn_id = churn_payload.player_id;
            assert_eq!(
                churn_payload.lobby_state,
                signal_fish_server::protocol::LobbyState::Lobby,
                "a churner joining a populated room must observe the Lobby state \
                 in its RoomJoined payload (epoch {epoch})"
            );

            // Ready toggle: readiness is only legal in the Lobby state,
            // which the RoomJoined payload just confirmed. The churner's
            // later leave clears the readiness — a genuine toggle of the
            // lobby's ready set, interleaved with relay traffic.
            let ready =
                serde_json::to_string(&ClientMessage::PlayerReady).expect("serialize PlayerReady");
            churn_sink
                .send(Message::Text(ready.into()))
                .await
                .expect("send PlayerReady");

            // Relay an epoch of GameData with the churner seated: EVERY
            // recipient (both persistent receivers and the churner) must
            // hold the complete gap-free epoch. Per-epoch sender names keep
            // each churner's ledger a clean 0..n stream (the pattern from
            // relay_chaos_e2e::reconnect_churn_leaks_nothing).
            let mut payload = LedgerPayload::new(&epoch_sender, PADDING_BYTES);
            send_burst(&mut sender_sink, &mut payload, MESSAGES_PER_EPOCH).await;
            drain_until(&mut recv_a_rx, &ledger, "RecvA", |ledger| {
                ledger.received_count("RecvA", &epoch_sender) >= MESSAGES_PER_EPOCH
            })
            .await;
            drain_until(&mut recv_b_rx, &ledger, "RecvB", |ledger| {
                ledger.received_count("RecvB", &epoch_sender) >= MESSAGES_PER_EPOCH
            })
            .await;
            drain_until(&mut churn_rx, &ledger, &churn_name, |ledger| {
                ledger.received_count(&churn_name, &epoch_sender) >= MESSAGES_PER_EPOCH
            })
            .await;

            // Leave: the churner departs cleanly (RoomLeft), and both
            // persistent receivers observe the broadcast before the next
            // epoch starts — a deterministic phase boundary.
            let leave =
                serde_json::to_string(&ClientMessage::LeaveRoom).expect("serialize LeaveRoom");
            churn_sink
                .send(Message::Text(leave.into()))
                .await
                .expect("send LeaveRoom");
            next_matching_server_message_within(
                &mut churn_rx,
                SERVER_MESSAGE_TIMEOUT,
                "churner's RoomLeft",
                |message| matches!(message, ServerMessage::RoomLeft).then_some(()),
            )
            .await;
            drain_until_player_left(&mut recv_a_rx, &ledger, "RecvA", churn_id).await;
            drain_until_player_left(&mut recv_b_rx, &ledger, "RecvB", churn_id).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(120), scenario)
        .await
        .expect("lobby churn scenario exceeded its deadline");

    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        0,
        "lobby churn must never trigger evictions"
    );
    // Deliberately NO raw `websocket_messages_dropped == 0` assert here: that
    // counter also tallies close-time flush abandonment — a trailing broadcast
    // (e.g. PlayerLeft) enqueued for a socket that a departing client already
    // closed — which is teardown accounting, not delivery loss, and races the
    // moment this assertion samples the counter (caught on a loaded CI
    // runner). The zero-loss intent is carried exactly by the three checks
    // around it: zero evictions, the ledger's gap-free completeness, and the
    // conservation law.
    let mut expectations: Vec<ReceiverExpectation> = ["RecvA", "RecvB"]
        .iter()
        .map(|receiver| ReceiverExpectation {
            receiver: (*receiver).to_string(),
            senders: (0..CHURN_EPOCHS)
                .map(|epoch| SenderExpectation {
                    sender: format!("Epoch{epoch}"),
                    total_sent: MESSAGES_PER_EPOCH,
                })
                .collect(),
        })
        .collect();
    expectations.extend((0..CHURN_EPOCHS).map(|epoch| ReceiverExpectation {
        receiver: format!("Churn{epoch}"),
        senders: vec![SenderExpectation {
            sender: format!("Epoch{epoch}"),
            total_sent: MESSAGES_PER_EPOCH,
        }],
    }));
    ledger.assert_zero_loss_or_loud_disconnect(&metrics, &expectations);
    assert_message_conservation(&metrics).await;
}
