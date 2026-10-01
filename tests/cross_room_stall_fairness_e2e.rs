//! C1 delivery slice — healthy-room progress during another room's stall.
//!
//! The slow-consumer suite proves a stalled recipient is evicted without
//! cascading WITHIN its room (`slow_consumer_no_cascade_e2e.rs`,
//! `relay_backpressure_e2e.rs`). The cross-room half of the delivery contract —
//! "healthy-room progress during another room's stall" — had no executable
//! pin: nothing forbade a shared lock, lane, or queue from letting room A's
//! wedged recipient stall room B's fan-out or lifecycle events for the whole
//! slow-consumer grace window.
//!
//! This test pins the cross-room fairness invariant end to end on real
//! sockets:
//!
//! - room A holds one flooding sender and one hard-stalled recipient (never
//!   reads), so room A's fan-out parks in backpressure until the slow-consumer
//!   deadline evicts exactly that recipient;
//! - room B runs on an independent connection set in a distinct game: a
//!   continuously flooding sender and two draining recipients, one of which
//!   joins WHILE room A's stall is live;
//! - the eviction is observed from room A's sender (`PlayerLeft` for the
//!   stalled peer only), and AT THAT MOMENT room B must already show progress:
//!   its recipients have relayed frames, and the mid-stall join has completed;
//! - after the stall resolves, room B's streams must still be complete and in
//!   order — with no `PlayerLeft`, no `Error`, and no socket close ever
//!   reaching a room-B member (a cross-room leak or cascade).
//!
//! Zero-flaky policy: the stalled peer never reads, so its eviction is not a
//! timing race; the eviction deadline (10 s) only widens the stall window that
//! the during-stall oracles are evaluated in, and those oracles need
//! milliseconds of actual work; the flood ends via a stop flag and a sentinel
//! frame, never an unbounded wait; the whole region is bounded by a generous
//! ceiling that is a deadline, never a fixed sleep used as a sync or negative
//! oracle.

mod test_helpers;
mod websocket_test_helpers;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::config::ProtocolConfig;
use signal_fish_server::protocol::{ClientMessage, PlayerId, ServerMessage};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::create_router;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use test_helpers::{create_test_server_with_config, RunningTestServer};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::assert_message_conservation;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
type WsReceiver = futures_util::stream::SplitStream<WsStream>;

const STALL_ROOM: &str = "STALA1";
const STALL_GAME: &str = "stall_game";
const FAIR_ROOM: &str = "FAIRB2";
const FAIR_GAME: &str = "fair_game";
/// Whole-test ceiling (oversubscribed CI runners included): a ceiling, not an
/// expected wait — the happy path finishes far sooner.
const TEST_DEADLINE: tokio::time::Duration = tokio::time::Duration::from_secs(120);
/// Small queue so the stalled peer's connection wedges quickly.
const SEND_QUEUE_CAPACITY: usize = 4;
/// Long grace window so the eviction lands strictly after the room-B
/// during-stall oracles have had ample time to complete. It only widens the
/// stall window; the eviction itself fires deterministically because the
/// stalled peer never reads.
const SLOW_CONSUMER_TIMEOUT_MS: u64 = 10_000;
/// 16 KiB per stall-room frame: small enough to stay under the server's
/// 64 KiB inbound `max_message_size` (larger frames are refused before they
/// ever reach fan-out), large enough that the wedged connection cannot hide
/// the backlog inside kernel socket buffers.
const STALL_PADDING_BYTES: usize = 16 * 1_024;
/// The stall-room flood ends when the sender's own fan-out parks in
/// backpressure on the wedged recipient; this bound only terminates the tail
/// it resumes after the eviction empties the room. ~16 MiB of offered volume
/// guarantees the wedged recipient's delivery queue reports `Full` long
/// before the flood is exhausted — the wedge is a queue-full event, not a
/// guess. Only the pipe volume is ever transferred; the rest never leaves
/// the sender.
const STALL_FLOOD_BOUND: u64 = 1000;
/// Hard bound for the stop-flag-driven fair-room flood. The flood ends via the
/// stop flag, not by exhausting this; it only prevents a runaway task if the
/// test already failed elsewhere.
const FAIR_FLOOD_BOUND: u64 = 1_000_000;
/// Last fair-room frame, sent when the stop flag is observed.
const FAIR_SENTINEL_SEQ: u64 = u64::MAX;

async fn start_server(server: Arc<EnhancedGameServer>) -> RunningTestServer {
    let router = create_router("http://localhost:3000").with_state(server.clone());
    RunningTestServer::spawn(server, router).await
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

/// Join `room_code` (creating it with a 4-seat cap on the first join) and
/// return the server-assigned player id.
async fn join_room(
    sink: &mut WsSink,
    receiver: &mut WsReceiver,
    game_name: &str,
    room_code: &str,
    player_name: &str,
) -> PlayerId {
    let join = ClientMessage::JoinRoom {
        game_name: game_name.to_string(),
        room_code: Some(room_code.to_string()),
        player_name: player_name.to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,

        password: None,
        join_only: None,
    };
    let json = serde_json::to_string(&join).expect("serialize JoinRoom");
    sink.send(Message::Text(json.into()))
        .await
        .expect("send JoinRoom");

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
            ServerMessage::RoomJoined(payload) => return payload.player_id,
            ServerMessage::RoomJoinFailed { reason, .. } => {
                panic!("room join failed for {player_name}: {reason}")
            }
            _ => continue,
        }
    }
}

async fn send_game_data(sink: &mut WsSink, seq: u64, padding: Option<&str>) {
    let mut data = serde_json::json!({ "seq": seq });
    if let Some(padding) = padding {
        data["padding"] = serde_json::Value::String(padding.to_string());
    }
    let message = ClientMessage::GameData {
        class: None,
        key: None,
        data,
    };
    let json = serde_json::to_string(&message).expect("serialize GameData");
    sink.send(Message::Text(json.into()))
        .await
        .expect("send GameData");
}

/// Flood the stall room. The sender's own fan-out parks in backpressure on the
/// never-reading recipient, so this task's sends block server-side well before
/// the bound; the remainder finishes after the eviction empties the room.
async fn flood_stall_room(mut sink: WsSink) {
    let padding = "x".repeat(STALL_PADDING_BYTES);
    for seq in 0..STALL_FLOOD_BOUND {
        send_game_data(&mut sink, seq, Some(&padding)).await;
    }
}

/// Flood the fair room with strictly increasing sequence numbers until the
/// stop flag is set, then send the sentinel so both recipients' drains have a
/// deterministic end marker.
async fn flood_fair_room(mut sink: WsSink, stop: Arc<AtomicBool>) {
    for seq in 0..FAIR_FLOOD_BOUND {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        send_game_data(&mut sink, seq, None).await;
    }
    send_game_data(&mut sink, FAIR_SENTINEL_SEQ, None).await;
}

/// What every fair-room member's drain forbids: a `PlayerLeft`, an `Error`, a
/// transport error, or a socket close. Any of those reaching room B while room
/// A stalls is exactly the cross-room failure this test exists to catch.
async fn next_fair_frame(receiver: &mut WsReceiver, who: &str) -> ServerMessage {
    loop {
        let Some(frame) = receiver.next().await else {
            panic!(
                "{who} socket closed — a room-A stall must never close a healthy room's connection"
            );
        };
        let frame = frame.unwrap_or_else(|error| panic!("{who} websocket error: {error}"));
        let Message::Text(text) = frame else {
            continue;
        };
        match serde_json::from_str::<ServerMessage>(&text).expect("valid ServerMessage") {
            ServerMessage::PlayerLeft { player_id, .. } => panic!(
                "{who} observed a PlayerLeft ({player_id}) — the room-A eviction leaked into \
                 (or cascaded to) a healthy room"
            ),
            ServerMessage::Error {
                message,
                error_code,
            } => panic!("{who} got a server error: {message} ({error_code:?})"),
            message => return message,
        }
    }
}

/// The primary fair-room recipient: every frame contiguous from seq 0, plus
/// the mid-stall joiner's `PlayerJoined`. Publishes its progress count after
/// every frame so the during-stall oracle can read it from the main task, and
/// reports the longest inter-frame arrival gap so the during-stall oracle can
/// forbid a mid-window strangulation (early frames alone would satisfy a
/// bare count).
async fn drain_fair_recipient(
    mut receiver: WsReceiver,
    joiner_id: PlayerId,
    progress: Arc<AtomicU64>,
    saw_join_broadcast: Arc<AtomicBool>,
) -> (u64, tokio::time::Duration) {
    let mut expected: u64 = 0;
    let mut last_arrival = tokio::time::Instant::now();
    let mut max_gap = tokio::time::Duration::ZERO;
    loop {
        match next_fair_frame(&mut receiver, "fair-room recipient").await {
            ServerMessage::GameData { data, .. } => {
                let arrived_at = tokio::time::Instant::now();
                max_gap = max_gap.max(arrived_at - last_arrival);
                last_arrival = arrived_at;
                let seq = data
                    .get("seq")
                    .and_then(serde_json::Value::as_u64)
                    .expect("GameData payload carries a numeric seq");
                if seq == FAIR_SENTINEL_SEQ {
                    return (expected, max_gap);
                }
                assert_eq!(
                    seq, expected,
                    "fair-room relay must stay complete and in order while room A stalls \
                     (expected seq {expected}, got {seq})"
                );
                expected += 1;
                progress.store(expected, Ordering::SeqCst);
            }
            ServerMessage::PlayerJoined { player } => {
                assert_eq!(
                    player.id, joiner_id,
                    "fair room saw a PlayerJoined for an unexpected player"
                );
                saw_join_broadcast.store(true, Ordering::SeqCst);
            }
            _ => continue,
        }
    }
}

/// The mid-stall joiner: its frames start mid-stream, so contiguity is checked
/// from the first seq it happens to see. It must still reach the sentinel
/// untouched.
async fn drain_mid_stall_joiner(mut receiver: WsReceiver) -> u64 {
    let mut expected: Option<u64> = None;
    loop {
        match next_fair_frame(&mut receiver, "mid-stall joiner").await {
            ServerMessage::GameData { data, .. } => {
                let seq = data
                    .get("seq")
                    .and_then(serde_json::Value::as_u64)
                    .expect("GameData payload carries a numeric seq");
                if seq == FAIR_SENTINEL_SEQ {
                    return expected.unwrap_or(0);
                }
                if let Some(next) = expected {
                    assert_eq!(
                        seq, next,
                        "mid-stall joiner's relay stream must stay contiguous \
                         (expected {next}, got {seq})"
                    );
                }
                expected = Some(seq + 1);
            }
            _ => continue,
        }
    }
}

/// C1 delivery: a room-A stall must not strand room B. Room B relays keep
/// flowing through the whole grace window, a mid-stall join completes with its
/// lifecycle broadcast, and the eviction tears down exactly one connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_room_does_not_strand_a_healthy_room() {
    let mut server_config = ServerConfig::default();
    server_config.websocket_config.send_queue_capacity = SEND_QUEUE_CAPACITY;
    server_config.websocket_config.slow_consumer_timeout_ms = SLOW_CONSUMER_TIMEOUT_MS;
    // The stalled peer never reads, so it never answers Pings either. Raise
    // the Pong deadline above the delivery deadline so the eviction under test
    // is the slow-consumer delivery contract, not the heartbeat reaper.
    server_config.websocket_config.pong_timeout_secs = 30;
    let server = create_test_server_with_config(server_config, ProtocolConfig::default()).await;
    let metrics = server.metrics();
    let running_server = start_server(server).await;
    let addr = running_server.addr();

    // Room A: the flooding sender creates the room; the stalled peer joins
    // last and never reads again (both halves stay in scope so the connection
    // genuinely wedges).
    let (mut stall_sender_sink, mut stall_sender_rx) = connect(addr).await;
    let _stall_sender_id = join_room(
        &mut stall_sender_sink,
        &mut stall_sender_rx,
        STALL_GAME,
        STALL_ROOM,
        "StallSender",
    )
    .await;
    let (mut stall_sink, mut stall_rx) = connect(addr).await;
    let stalled_id = join_room(
        &mut stall_sink,
        &mut stall_rx,
        STALL_GAME,
        STALL_ROOM,
        "Stalled",
    )
    .await;

    // Room B: an independent connection set in a distinct game and room.
    let (mut fair_sender_sink, mut fair_sender_rx) = connect(addr).await;
    let _fair_sender_id = join_room(
        &mut fair_sender_sink,
        &mut fair_sender_rx,
        FAIR_GAME,
        FAIR_ROOM,
        "FairSender",
    )
    .await;
    let (mut fair_recipient_sink, mut fair_recipient_rx) = connect(addr).await;
    let _fair_recipient_id = join_room(
        &mut fair_recipient_sink,
        &mut fair_recipient_rx,
        FAIR_GAME,
        FAIR_ROOM,
        "FairRecipient",
    )
    .await;

    // Start the room-A flood: the sender's fan-out parks on the never-reading
    // recipient and stays parked until the slow-consumer deadline evicts it.
    let stall_flood = tokio::spawn(flood_stall_room(stall_sender_sink));

    // Join a fourth member into room B while the stall is live. It joins
    // before the fair-room flood starts, so its drain sees a mid-stream
    // GameData window rather than the flood's beginning.
    let (mut joiner_sink, mut joiner_rx) = connect(addr).await;
    let joiner_id = join_room(
        &mut joiner_sink,
        &mut joiner_rx,
        FAIR_GAME,
        FAIR_ROOM,
        "MidStallJoiner",
    )
    .await;
    let join_completed_at = tokio::time::Instant::now();

    // Start the room-B flood and drain both fair-room recipients concurrently
    // with the eviction wait.
    let fair_stop = Arc::new(AtomicBool::new(false));
    let fair_flood = tokio::spawn(flood_fair_room(fair_sender_sink, Arc::clone(&fair_stop)));
    let progress = Arc::new(AtomicU64::new(0));
    let saw_join_broadcast = Arc::new(AtomicBool::new(false));
    let fair_drain = tokio::spawn(drain_fair_recipient(
        fair_recipient_rx,
        joiner_id,
        Arc::clone(&progress),
        Arc::clone(&saw_join_broadcast),
    ));
    let joiner_drain = tokio::spawn(drain_mid_stall_joiner(joiner_rx));

    // Observe the eviction from room A: the stalled peer's `PlayerLeft`, and
    // nothing else.
    tokio::time::timeout(TEST_DEADLINE, async {
        loop {
            let frame = stall_sender_rx
                .next()
                .await
                .expect("stall-room sender socket closed before the eviction was observed");
            let frame = frame.expect("stall-room sender websocket error");
            let Message::Text(text) = frame else {
                continue;
            };
            let message: ServerMessage = serde_json::from_str(&text).expect("valid ServerMessage");
            match message {
                ServerMessage::PlayerLeft { player_id, .. } => {
                    assert_eq!(
                        player_id, stalled_id,
                        "room A observed a PlayerLeft for a non-stalled member"
                    );
                    return;
                }
                _ => continue,
            }
        }
    })
    .await
    .expect("the stalled peer was never evicted (the wedge never reached the deadline)");
    let eviction_observed_at = tokio::time::Instant::now();

    // --- During-stall oracles, evaluated the moment the eviction is observed.

    // 1. Room B relayed frames THROUGH the whole grace window: the fair-room
    //    flood ran continuously from before this point, so a zero here means
    //    the stall strangled a healthy room's relay plane.
    let progress_at_eviction = progress.load(Ordering::SeqCst);
    assert!(
        progress_at_eviction >= 1,
        "room B relayed no frames during room A's {SLOW_CONSUMER_TIMEOUT_MS} ms stall window \
         — a stalled room stranded a healthy room"
    );

    // 2. The mid-stall join completed while the stall was still live.
    assert!(
        join_completed_at < eviction_observed_at,
        "the mid-stall join outlived room A's stall window — the event plane stalled too"
    );
    // Release room B's flood and let both drains observe the sentinel.
    fair_stop.store(true, Ordering::SeqCst);
    let ((fair_total, fair_max_gap), joiner_total) = tokio::time::timeout(TEST_DEADLINE, async {
        (
            fair_drain.await.expect("fair-room drain task panicked"),
            joiner_drain
                .await
                .expect("mid-stall joiner drain task panicked"),
        )
    })
    .await
    .expect("fair-room drains exceeded their ceiling");
    // Both floods terminate once their rooms are quiet: the fair flood at the
    // stop flag, the stall flood after the eviction emptied its room.
    tokio::time::timeout(TEST_DEADLINE, async {
        let _ = fair_flood.await;
        let _ = stall_flood.await;
    })
    .await
    .expect("flood writers exceeded their ceiling");

    // --- Post-stall oracles.

    // The relay plane flowed CONTINUOUSLY through the window: no single
    // inter-frame gap may approach the stall window. A mid-window
    // strangulation (for example a shared gate held across another room's
    // slow-consumer wait) surfaces as one gap spanning the blocked span even
    // though the bare frame count looks healthy. The bound is half the stall
    // window — generous against scheduler jitter (healthy gaps are
    // microseconds to milliseconds), far below any real coupling.
    assert!(
        fair_max_gap < tokio::time::Duration::from_millis(SLOW_CONSUMER_TIMEOUT_MS / 2),
        "room B's relay went silent for {fair_max_gap:?} between frames while room A was \
         wedged — healthy-room progress was interrupted mid-window"
    );

    // The two progress views must agree: the drain is the only writer, and it
    // never rewinds.
    assert!(
        fair_total >= progress_at_eviction,
        "fair-room progress rewound across the eviction boundary \
         (at eviction: {progress_at_eviction}, total: {fair_total})"
    );

    // The mid-stall join's lifecycle broadcast reached room B's members.
    assert!(
        saw_join_broadcast.load(Ordering::SeqCst),
        "room B never broadcast the mid-stall joiner's PlayerJoined"
    );
    assert!(
        joiner_total >= 1,
        "the mid-stall joiner never received fair-room relay traffic"
    );

    // Exactly one connection was torn down: room A's stalled peer.
    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        1,
        "exactly one slow-consumer eviction is allowed — room A's stalled peer"
    );
    assert!(
        metrics.websocket_messages_dropped.load(Ordering::Relaxed) >= 1,
        "the stalled peer's abandoned frames must be counted as drops (non-vacuity)"
    );

    // The stalled peer's socket must actually be closed by the server: drain
    // whatever was buffered and require the stream to end.
    let stall_termination =
        tokio::time::timeout(tokio::time::Duration::from_secs(30), async move {
            while let Some(frame) = stall_rx.next().await {
                match frame {
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => continue,
                }
            }
        })
        .await;
    assert!(
        stall_termination.is_ok(),
        "the stalled peer's socket was never closed by the server"
    );
    drop(stall_sink);
    drop(joiner_sink);
    drop(fair_recipient_sink);

    // Everything has quiesced: the conservation counters must balance.
    assert_message_conservation(&metrics).await;
    running_server.shutdown().await;
}
