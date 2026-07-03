//! PR-lane chaos coverage for the relay delivery contract (issue #131):
//! transport-level fault injection through a hand-rolled TCP proxy
//! (`tests/websocket_test_helpers/chaos_proxy.rs`) between real WebSocket
//! clients and the server.
//!
//! The burst suite (`tests/relay_backpressure_e2e.rs`) exercises well-behaved
//! sockets that merely stop draining; this binary exercises sockets that
//! FAIL — reset mid-relay, drip-fed through a throttled link, churned through
//! connect/kill cycles — and asserts the same terminal contract via the
//! delivery ledger: every surviving receiver holds the exact gap-free stream,
//! every torn-down receiver holds a gap-free prefix, and the conservation
//! counters balance. Nothing is ever silently lost.
//!
//! Every test carries `#[serial_test::serial]` for the same reasons as the
//! burst suite: the churn test asserts on the process-wide `/proc/self/fd`
//! table, which is only deterministic when no sibling test churns sockets in
//! the same process (plain `cargo test` runs a binary's tests concurrently in
//! one process; under nextest each test is its own process and the lock is a
//! no-op), and serializing keeps the flood-heavy tests from CPU-starving each
//! other on loaded runners.

mod test_helpers;
mod websocket_test_helpers;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::config::ProtocolConfig;
use signal_fish_server::protocol::{ClientMessage, PlayerId, ServerMessage};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::create_router;
use test_helpers::{create_test_server, create_test_server_with_config};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::assert_message_conservation;
use websocket_test_helpers::chaos_proxy::{ChaosProxy, Direction};
use websocket_test_helpers::delivery_ledger::{
    extract, DeliveryLedger, DisconnectReason, LedgerPayload, ReceiverExpectation,
    SenderExpectation,
};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
type WsReceiver = futures_util::stream::SplitStream<WsStream>;

/// Per-step ceiling for expected events (connects, message arrivals, gauge
/// convergence). Generous: only a genuine wedge spends it.
const EVENT_DEADLINE: tokio::time::Duration = tokio::time::Duration::from_secs(30);

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

/// Join `room` and return the server-assigned player id.
async fn join_room(
    sink: &mut WsSink,
    receiver: &mut WsReceiver,
    room: &str,
    player_name: &str,
) -> PlayerId {
    let join = ClientMessage::JoinRoom {
        game_name: "chaos_game".to_string(),
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

/// Record one text frame into the ledger for `receiver_name`; returns any
/// `PlayerLeft` id observed instead. Server errors fail loudly.
fn record_frame(ledger: &DeliveryLedger, receiver_name: &str, text: &str) -> Option<PlayerId> {
    let message: ServerMessage = serde_json::from_str(text).expect("valid ServerMessage");
    match message {
        ServerMessage::GameData { data, .. } => {
            let (sender, seq) = extract(&data).unwrap_or_else(|| {
                panic!("{receiver_name}: GameData without ledger fields: {data}")
            });
            // `server_seq: None` until the server stamps per-connection
            // delivery sequences (see the ledger's cross-check hook).
            ledger.record(receiver_name, &sender, seq, None);
            None
        }
        ServerMessage::PlayerLeft { player_id } => Some(player_id),
        ServerMessage::Error {
            message,
            error_code,
        } => panic!("{receiver_name}: server error mid-chaos: {message} ({error_code:?})"),
        _ => None,
    }
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

/// Poll `condition` every 10ms until it holds, failing loudly at `deadline`.
/// A ceiling, not an expected wait: returns the instant the state holds.
async fn poll_until(context: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + EVENT_DEADLINE;
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{context}: condition never held within {EVENT_DEADLINE:?}"
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
}

/// A TCP RST mid-relay must not disturb anyone else: the surviving receiver
/// keeps getting a gap-free stream (including messages sent AFTER the reset),
/// the room observes `PlayerLeft` for the killed client, the killed client's
/// own ledger is a gap-free prefix, and the conservation counters balance —
/// with zero slow-consumer disconnects (a dead socket is a disconnect race,
/// never a silent drop or a spurious eviction).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn rst_during_relay_heals_room_and_conserves() {
    const ROOM: &str = "CHRST1";
    const PRE_RST_MESSAGES: u64 = 60;
    const POST_RST_MESSAGES: u64 = 60;
    const PAYLOAD_PADDING_BYTES: usize = 256;

    let server = create_test_server().await;
    let metrics = server.metrics();
    let addr = start_server(server).await;
    let proxy = ChaosProxy::spawn(addr).await;

    let chaos = async {
        let (mut sender_sink, mut sender_rx) = connect(addr).await;
        let (mut watcher_sink, mut watcher_rx) = connect(addr).await;
        let (mut victim_sink, mut victim_rx) = connect(proxy.addr()).await;

        let _sender_id = join_room(&mut sender_sink, &mut sender_rx, ROOM, "Sender").await;
        let _watcher_id = join_room(&mut watcher_sink, &mut watcher_rx, ROOM, "Watcher").await;
        let victim_id = join_room(&mut victim_sink, &mut victim_rx, ROOM, "Victim").await;

        let ledger = Arc::new(DeliveryLedger::new());

        // The victim drains through the proxy until the RST severs it, then
        // records its own (gap-free-prefix) disconnect.
        let victim_ledger = Arc::clone(&ledger);
        let victim_drain = tokio::spawn(async move {
            loop {
                match victim_rx.next().await {
                    Some(Ok(Message::Text(text))) => {
                        let _player_left = record_frame(&victim_ledger, "Victim", &text);
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => continue,
                }
            }
            victim_ledger.note_receiver_disconnected(
                "Victim",
                DisconnectReason::InjectedFault("tcp-rst mid-relay".to_string()),
            );
        });

        let mut payload = LedgerPayload::new("Sender", PAYLOAD_PADDING_BYTES);
        send_burst(&mut sender_sink, &mut payload, PRE_RST_MESSAGES).await;

        // Mid-burst: reset once the victim has demonstrably received a chunk
        // of the stream (so the kill lands with relay traffic in flight).
        let victim_progress_ledger = Arc::clone(&ledger);
        poll_until("victim receives a pre-RST chunk", || {
            victim_progress_ledger.received_count("Victim", "Sender") >= PRE_RST_MESSAGES / 2
        })
        .await;
        proxy.rst_all();

        send_burst(&mut sender_sink, &mut payload, POST_RST_MESSAGES).await;
        let total_sent = payload.sent();

        // The watcher must observe the complete stream AND the victim's
        // PlayerLeft (the room healing around the dead connection).
        let mut saw_victim_leave = false;
        while ledger.received_count("Watcher", "Sender") < total_sent || !saw_victim_leave {
            let frame = tokio::time::timeout(EVENT_DEADLINE, watcher_rx.next())
                .await
                .expect("watcher timed out waiting for the post-RST stream")
                .expect("watcher connection closed mid-stream")
                .expect("watcher websocket error mid-stream");
            let Message::Text(text) = frame else {
                continue;
            };
            if record_frame(&ledger, "Watcher", &text) == Some(victim_id) {
                saw_victim_leave = true;
            }
        }

        victim_drain.await.expect("victim drain panicked");
        (ledger, total_sent)
    };
    let (ledger, total_sent) = tokio::time::timeout(tokio::time::Duration::from_secs(60), chaos)
        .await
        .expect("RST chaos test exceeded its deadline");

    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        0,
        "a reset connection is a disconnect race, never a slow-consumer eviction"
    );
    ledger.assert_zero_loss_or_loud_disconnect(
        &metrics,
        &[
            expectation("Watcher", &[("Sender", total_sent)]),
            expectation("Victim", &[("Sender", total_sent)]),
        ],
    );
    assert_message_conservation(&metrics);
}

/// A consumer drip-fed through a throttled link repeatedly fills the (small)
/// per-connection queue but keeps draining well inside the grace window:
/// backpressure must engage (`backpressure_events > 0`), nobody may be
/// disconnected, nothing may be dropped, and once the throttle lifts the
/// consumer must hold the complete gap-free stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn drip_fed_consumer_backpressures_without_eviction() {
    const ROOM: &str = "CHDRIP";
    /// 8KiB payloads: a handful saturate the kernel path to the throttled
    /// proxy, so the queue — not TCP buffering — absorbs the burst.
    const PAYLOAD_PADDING_BYTES: usize = 8 * 1_024;
    /// Drip rate: ~60 messages/s through the proxy, so a parked delivery
    /// waits ~16ms for a queue slot — two orders of magnitude inside the 5s
    /// grace window (eviction would require the drain to genuinely stop).
    const DRIP_BYTES_PER_SEC: u64 = 512 * 1_024;
    /// Loud cap (~25MB of flood): reached only if backpressure never fires.
    const BURST_MESSAGE_CAP: u64 = 3_000;
    /// Messages sent after backpressure has been observed, so the assertion
    /// window covers queue-full crossings, not a single grazing touch.
    const POST_BACKPRESSURE_TAIL: u64 = 25;

    let mut server_config = ServerConfig::default();
    server_config.websocket_config.send_queue_capacity = 16;
    server_config.websocket_config.slow_consumer_timeout_ms = 5_000;
    let server = create_test_server_with_config(server_config, ProtocolConfig::default()).await;
    let metrics = server.metrics();
    let addr = start_server(server).await;
    let proxy = ChaosProxy::spawn(addr).await;
    proxy.throttle(Direction::ServerToClient, Some(DRIP_BYTES_PER_SEC));

    let chaos = async {
        let (mut sender_sink, mut sender_rx) = connect(addr).await;
        let (mut dripped_sink, mut dripped_rx) = connect(proxy.addr()).await;

        let _sender_id = join_room(&mut sender_sink, &mut sender_rx, ROOM, "Sender").await;
        let _dripped_id = join_room(&mut dripped_sink, &mut dripped_rx, ROOM, "Dripped").await;

        let ledger = Arc::new(DeliveryLedger::new());
        let (totals_tx, mut totals_rx) = tokio::sync::watch::channel::<Option<u64>>(None);

        // The dripped consumer drains as fast as the throttle feeds it.
        let drip_ledger = Arc::clone(&ledger);
        let dripped_drain = tokio::spawn(async move {
            loop {
                let total = *totals_rx.borrow_and_update();
                if let Some(total) = total {
                    if drip_ledger.received_count("Dripped", "Sender") >= total {
                        return dripped_rx;
                    }
                }
                tokio::select! {
                    frame = dripped_rx.next() => {
                        let frame = frame
                            .expect("Dripped: connection closed mid-drain")
                            .expect("Dripped: websocket error mid-drain");
                        if let Message::Text(text) = frame {
                            let _player_left = record_frame(&drip_ledger, "Dripped", &text);
                        }
                    }
                    changed = totals_rx.changed() => {
                        changed.expect("sent-totals channel dropped before totals were set");
                    }
                }
            }
        });

        // Metric-driven burst: flood until the queue demonstrably crossed
        // full (a backpressure event), then a short tail, then stop.
        let mut payload = LedgerPayload::new("Sender", PAYLOAD_PADDING_BYTES);
        while metrics
            .websocket_backpressure_events
            .load(Ordering::Relaxed)
            == 0
        {
            assert!(
                payload.sent() < BURST_MESSAGE_CAP,
                "flood cap reached without a single backpressure event — the queue never \
                 filled (enlarge the payload or shrink the queue)"
            );
            send_burst(&mut sender_sink, &mut payload, 1).await;
        }
        send_burst(&mut sender_sink, &mut payload, POST_BACKPRESSURE_TAIL).await;
        let total_sent = payload.sent();

        // Lift the throttle and let the consumer catch up completely.
        proxy.throttle(Direction::ServerToClient, None);
        totals_tx.send_replace(Some(total_sent));
        let _dripped_rx = dripped_drain.await.expect("dripped drain panicked");

        (ledger, total_sent)
    };
    let (ledger, total_sent) = tokio::time::timeout(tokio::time::Duration::from_secs(90), chaos)
        .await
        .expect("drip-feed chaos test exceeded its deadline");

    assert!(
        metrics
            .websocket_backpressure_events
            .load(Ordering::Relaxed)
            > 0,
        "the drip-fed queue must have crossed full at least once"
    );
    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        0,
        "a consumer draining inside the grace window must never be evicted"
    );
    assert_eq!(
        metrics.websocket_messages_dropped.load(Ordering::Relaxed),
        0,
        "nothing may be dropped while the consumer keeps draining"
    );
    ledger.assert_zero_loss_or_loud_disconnect(
        &metrics,
        &[expectation("Dripped", &[("Sender", total_sent)])],
    );
    assert_message_conservation(&metrics);
}

/// Connect/relay/kill churn must leak nothing: after eight clients join
/// through fresh proxies, relay in both directions, and die by RST, the
/// active-connections gauge and (on Linux) the process's open-fd count must
/// return to the pre-churn baseline, the persistent peer's ledger must be
/// complete for every churned sender, and conservation must hold.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn reconnect_churn_leaks_nothing() {
    const ROOM: &str = "CHURN1";
    const CHURN_ITERATIONS: usize = 8;
    const MESSAGES_FROM_CHURN: u64 = 3;
    const MESSAGES_TO_CHURN: u64 = 2;
    const PAYLOAD_PADDING_BYTES: usize = 64;

    let server = create_test_server().await;
    let metrics = server.metrics();
    let addr = start_server(server).await;

    let ledger = Arc::new(DeliveryLedger::new());

    let chaos = async {
        let (mut persistent_sink, mut persistent_rx) = connect(addr).await;
        let _persistent_id =
            join_room(&mut persistent_sink, &mut persistent_rx, ROOM, "Persistent").await;

        // Baselines AFTER the persistent client is established: churn must
        // return the process to exactly this state.
        let baseline_connections = metrics.active_connections.load(Ordering::Relaxed);
        #[cfg(target_os = "linux")]
        let baseline_fd_count = count_open_fds();

        for iteration in 0..CHURN_ITERATIONS {
            let churn_name = format!("Churn{iteration}");
            let proxy = ChaosProxy::spawn(addr).await;
            let (mut churn_sink, mut churn_rx) = connect(proxy.addr()).await;
            let churn_id = join_room(&mut churn_sink, &mut churn_rx, ROOM, &churn_name).await;

            // Churn -> persistent relay.
            let mut churn_payload = LedgerPayload::new(&churn_name, PAYLOAD_PADDING_BYTES);
            send_burst(&mut churn_sink, &mut churn_payload, MESSAGES_FROM_CHURN).await;

            // Persistent -> churn relay (fresh sender name per iteration so
            // every churned receiver's ledger is a clean 0..n prefix).
            let persistent_sender = format!("Persistent{iteration}");
            let mut persistent_payload =
                LedgerPayload::new(&persistent_sender, PAYLOAD_PADDING_BYTES);
            send_burst(
                &mut persistent_sink,
                &mut persistent_payload,
                MESSAGES_TO_CHURN,
            )
            .await;

            // Both sides must observe the full exchange BEFORE the kill, so
            // the terminal ledger is exact (nothing was in flight).
            while ledger.received_count("Persistent", &churn_name) < MESSAGES_FROM_CHURN {
                let frame = tokio::time::timeout(EVENT_DEADLINE, persistent_rx.next())
                    .await
                    .expect("persistent client timed out draining churn relay")
                    .expect("persistent connection closed mid-churn")
                    .expect("persistent websocket error mid-churn");
                if let Message::Text(text) = frame {
                    let _player_left = record_frame(&ledger, "Persistent", &text);
                }
            }
            while ledger.received_count(&churn_name, &persistent_sender) < MESSAGES_TO_CHURN {
                let frame = tokio::time::timeout(EVENT_DEADLINE, churn_rx.next())
                    .await
                    .expect("churn client timed out draining persistent relay")
                    .expect("churn connection closed before its kill")
                    .expect("churn websocket error before its kill");
                if let Message::Text(text) = frame {
                    let _player_left = record_frame(&ledger, &churn_name, &text);
                }
            }

            // Kill the link and require the room to heal: PlayerLeft reaches
            // the persistent client, and the gauge returns to baseline.
            proxy.rst_all();
            ledger.note_receiver_disconnected(
                &churn_name,
                DisconnectReason::InjectedFault("tcp-rst churn kill".to_string()),
            );
            let mut saw_churn_leave = false;
            while !saw_churn_leave {
                let frame = tokio::time::timeout(EVENT_DEADLINE, persistent_rx.next())
                    .await
                    .expect("persistent client timed out waiting for churn PlayerLeft")
                    .expect("persistent connection closed awaiting PlayerLeft")
                    .expect("persistent websocket error awaiting PlayerLeft");
                if let Message::Text(text) = frame {
                    saw_churn_leave = record_frame(&ledger, "Persistent", &text) == Some(churn_id);
                }
            }
            poll_until(
                "active connections return to baseline after churn kill",
                || metrics.active_connections.load(Ordering::Relaxed) <= baseline_connections,
            )
            .await;
            drop(proxy);
            drop(churn_sink);
        }

        // (Linux) The churned sockets' and proxies' descriptors must all be
        // genuinely released — polled, because fd release trails task abort.
        #[cfg(target_os = "linux")]
        poll_until("open fd count returns to the pre-churn baseline", || {
            count_open_fds() <= baseline_fd_count
        })
        .await;

        let mut expectations = vec![ReceiverExpectation {
            receiver: "Persistent".to_string(),
            senders: (0..CHURN_ITERATIONS)
                .map(|iteration| SenderExpectation {
                    sender: format!("Churn{iteration}"),
                    total_sent: MESSAGES_FROM_CHURN,
                })
                .collect(),
        }];
        expectations.extend((0..CHURN_ITERATIONS).map(|iteration| ReceiverExpectation {
            receiver: format!("Churn{iteration}"),
            senders: vec![SenderExpectation {
                sender: format!("Persistent{iteration}"),
                total_sent: MESSAGES_TO_CHURN,
            }],
        }));
        expectations
    };
    let expectations = tokio::time::timeout(tokio::time::Duration::from_secs(120), chaos)
        .await
        .expect("reconnect-churn chaos test exceeded its deadline");

    ledger.assert_zero_loss_or_loud_disconnect(&metrics, &expectations);
    assert_message_conservation(&metrics);
}

/// Count this process's open file descriptors via `/proc/self/fd`. The
/// `read_dir` handle itself briefly adds one entry, but it does so for the
/// baseline and the polled samples alike, so comparisons stay exact.
#[cfg(target_os = "linux")]
fn count_open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("read /proc/self/fd")
        .count()
}
