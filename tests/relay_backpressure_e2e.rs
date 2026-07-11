//! End-to-end coverage for relay delivery reliability under burst load.
//!
//! Regression tests for <https://github.com/Ambiguous-Interactive/signal-fish-server/issues/131>:
//! the per-connection outbound queue used to be a small bounded channel written
//! with `try_send`, so a `GameData` burst larger than the queue silently
//! dropped messages with only a server-side log line. Rollback-netcode style
//! clients (which require reliable, ordered relay of every input packet)
//! observed a permanent sent-vs-received deficit that no amount of client-side
//! buffering could fix.
//!
//! The contract under test: **the relay never silently drops a message** — a
//! room-scoped payload is either delivered to every connected peer (with
//! backpressure applied to the sender when a recipient's queue is full) or the
//! unresponsive recipient is loudly disconnected as a slow consumer.
//!
//! Every test in this binary carries `#[serial_test::serial]`: the wedged-
//! socket test asserts on the process-wide `/proc/self/fd` table, which is
//! only deterministic when no sibling test is churning sockets in the same
//! process (plain `cargo test` runs a binary's tests concurrently in one
//! process; under nextest each test is its own process and the lock is a
//! no-op). Serializing also keeps these deliberately flood-heavy tests from
//! CPU-starving each other on loaded runners.

mod test_helpers;
mod websocket_test_helpers;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::config::ProtocolConfig;
use signal_fish_server::protocol::{ClientMessage, ErrorCode, PlayerId, ServerMessage};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::create_router;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use test_helpers::{create_test_server, create_test_server_with_config, RunningTestServer};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::{assert_message_conservation, connect_with_small_recv_buffer};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
type WsReceiver = futures_util::stream::SplitStream<WsStream>;

/// Number of `GameData` messages the sender bursts without pacing.
const BURST_MESSAGE_COUNT: usize = 5_000;

/// Per-message payload padding. Sized so the full burst (~10 MB) cannot hide
/// inside kernel socket buffers: the per-connection queue and the server's
/// backpressure behavior — not TCP buffering — must absorb the burst.
const PAYLOAD_PADDING_BYTES: usize = 2_048;

/// How long the receivers wait before draining. Long enough that an unpaced
/// sender overflows a drop-based bounded queue many times over; irrelevant to
/// a backpressure-based server, which simply paces the sender until readers
/// start draining.
const READER_START_DELAY: tokio::time::Duration = tokio::time::Duration::from_millis(750);

/// Generous whole-test deadline (oversubscribed CI runners included).
const BURST_TEST_DEADLINE: tokio::time::Duration = tokio::time::Duration::from_secs(120);

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

async fn join_room(sink: &mut WsSink, receiver: &mut WsReceiver, player_name: &str) {
    let join = ClientMessage::JoinRoom {
        game_name: "burst_game".to_string(),
        room_code: Some("BURST1".to_string()),
        player_name: player_name.to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };
    let json = serde_json::to_string(&join).expect("serialize JoinRoom");
    sink.send(Message::Text(json.into()))
        .await
        .expect("send JoinRoom");

    // Wait for the RoomJoined acknowledgement (other lobby chatter may arrive
    // first; GameData never arrives here because no one has sent any yet).
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

/// Drain `receiver` until `expected` GameData messages arrive, returning their
/// sequence numbers in arrival order. Panics loudly on server-sent errors or
/// early connection close so drops can never masquerade as success.
async fn collect_game_data_seqs(receiver: &mut WsReceiver, expected: usize) -> Vec<u64> {
    let mut seqs = Vec::with_capacity(expected);
    while seqs.len() < expected {
        let Some(frame) = receiver.next().await else {
            panic!(
                "connection closed after {} of {expected} GameData messages",
                seqs.len()
            );
        };
        let frame = frame.expect("websocket error while draining GameData");
        let Message::Text(text) = frame else {
            continue;
        };
        let message: ServerMessage = serde_json::from_str(&text).expect("valid ServerMessage");
        match message {
            ServerMessage::GameData { data, .. } => {
                let seq = data
                    .get("seq")
                    .and_then(serde_json::Value::as_u64)
                    .expect("GameData payload carries a numeric seq");
                seqs.push(seq);
            }
            ServerMessage::Error {
                message,
                error_code,
            } => {
                panic!(
                    "server error while draining GameData (after {} messages): \
                     {message} ({error_code:?})",
                    seqs.len()
                );
            }
            // Join/lobby notifications and other control chatter are expected
            // and irrelevant to the delivery contract under test.
            _ => continue,
        }
    }
    seqs
}

fn assert_complete_and_ordered(seqs: &[u64], expected: usize, who: &str) {
    assert_eq!(
        seqs.len(),
        expected,
        "{who}: expected the full burst to be delivered"
    );
    for (index, seq) in seqs.iter().enumerate() {
        assert_eq!(
            *seq, index as u64,
            "{who}: relay delivery must be complete and in order \
             (message {index} arrived with seq {seq})"
        );
    }
}

/// A fast, unpaced `GameData` burst must reach every peer completely and in
/// order. With the old drop-based bounded queue this fails within the first
/// few hundred messages; with backpressure it must hold for any burst size.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn game_data_burst_is_relayed_completely_and_in_order() {
    let server = create_test_server().await;
    let metrics = server.metrics();
    let running_server = start_server(server).await;
    let addr = running_server.addr();

    let (mut sender_sink, mut sender_rx) = connect(addr).await;
    let (mut receiver_a_sink, mut receiver_a_rx) = connect(addr).await;
    let (mut receiver_b_sink, mut receiver_b_rx) = connect(addr).await;

    join_room(&mut sender_sink, &mut sender_rx, "BurstSender").await;
    join_room(&mut receiver_a_sink, &mut receiver_a_rx, "ReceiverA").await;
    join_room(&mut receiver_b_sink, &mut receiver_b_rx, "ReceiverB").await;

    // Burst the full payload set with zero pacing, exactly like a rollback
    // netcode client catching a peer up. The writer runs on its own task so
    // server-side backpressure (which is expected to slow this loop down once
    // queues fill) cannot deadlock against the readers below.
    let writer = tokio::spawn(async move {
        let padding = "x".repeat(PAYLOAD_PADDING_BYTES);
        for seq in 0..BURST_MESSAGE_COUNT {
            let message = ClientMessage::GameData {
                class: None,
                key: None,
                data: serde_json::json!({ "seq": seq as u64, "padding": padding.as_str() }),
            };
            let json = serde_json::to_string(&message).expect("serialize GameData");
            sender_sink
                .send(Message::Text(json.into()))
                .await
                .expect("send GameData burst frame");
        }
        sender_sink
    });

    // Readers intentionally start late: a drop-based server has already lost
    // most of the burst by now, while a backpressure-based server has simply
    // paced the sender and delivers everything.
    let reader_a = tokio::spawn(async move {
        tokio::time::sleep(READER_START_DELAY).await;
        let seqs = collect_game_data_seqs(&mut receiver_a_rx, BURST_MESSAGE_COUNT).await;
        (seqs, receiver_a_rx)
    });
    let reader_b = tokio::spawn(async move {
        tokio::time::sleep(READER_START_DELAY).await;
        let seqs = collect_game_data_seqs(&mut receiver_b_rx, BURST_MESSAGE_COUNT).await;
        (seqs, receiver_b_rx)
    });

    let (writer_result, reader_a_result, reader_b_result) =
        tokio::time::timeout(BURST_TEST_DEADLINE, async {
            tokio::join!(writer, reader_a, reader_b)
        })
        .await
        .expect("burst relay test exceeded its deadline (deadlock or lost messages?)");

    let _sender_sink = writer_result.expect("burst writer task panicked");
    let (seqs_a, _rx_a) = reader_a_result.expect("reader A task panicked");
    let (seqs_b, _rx_b) = reader_b_result.expect("reader B task panicked");

    assert_complete_and_ordered(&seqs_a, BURST_MESSAGE_COUNT, "receiver A");
    assert_complete_and_ordered(&seqs_b, BURST_MESSAGE_COUNT, "receiver B");

    // Both receivers observed the full burst, so every delivery has resolved:
    // the conservation counters must balance.
    assert_message_conservation(&metrics).await;
    running_server.shutdown().await;
}

/// A recipient that stops draining its socket entirely must be disconnected
/// as a slow consumer — loudly (metrics, close) — while the rest of the room
/// keeps flowing and the sender is never wedged behind the dead connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn slow_consumer_is_disconnected_loudly_and_room_keeps_flowing() {
    // Small queue + short grace window so the stalled recipient is evicted
    // quickly; large payloads so the burst cannot hide in kernel TCP buffers.
    const MESSAGE_COUNT: usize = 2_000;
    const STALL_PADDING_BYTES: usize = 16 * 1_024;

    let mut server_config = ServerConfig::default();
    server_config.websocket_config.send_queue_capacity = 8;
    server_config.websocket_config.slow_consumer_timeout_ms = 300;
    let server = create_test_server_with_config(server_config, ProtocolConfig::default()).await;
    let metrics = server.metrics();
    let running_server = start_server(server).await;
    let addr = running_server.addr();

    // Join order matters for the observers below: the healthy receiver joins
    // first so it sees PlayerJoined for the stalled client (capturing its id
    // indirectly is unnecessary — the stalled client reports its own id).
    let (mut healthy_sink, mut healthy_rx) = connect(addr).await;
    let (mut sender_sink, mut sender_rx) = connect(addr).await;
    let (mut stalled_sink, mut stalled_rx) = connect(addr).await;

    join_room(&mut healthy_sink, &mut healthy_rx, "HealthyReceiver").await;
    join_room(&mut sender_sink, &mut sender_rx, "FloodSender").await;
    let stalled_player_id =
        join_room_returning_id(&mut stalled_sink, &mut stalled_rx, "Stalled").await;

    // The stalled client now goes silent: it never polls its socket again
    // until after the flood. Its kernel buffers will fill, then its outbound
    // queue, then the slow-consumer timeout must evict it.
    let writer = tokio::spawn(async move {
        let padding = "x".repeat(STALL_PADDING_BYTES);
        for seq in 0..MESSAGE_COUNT {
            let message = ClientMessage::GameData {
                class: None,
                key: None,
                data: serde_json::json!({ "seq": seq as u64, "padding": padding.as_str() }),
            };
            let json = serde_json::to_string(&message).expect("serialize GameData");
            sender_sink
                .send(Message::Text(json.into()))
                .await
                .expect("send GameData while a peer is stalled");
        }
        sender_sink
    });

    // The healthy receiver must observe the complete, ordered burst AND the
    // stalled player's eviction (PlayerLeft via the normal disconnect flow).
    let healthy_reader = tokio::spawn(async move {
        let mut seqs = Vec::with_capacity(MESSAGE_COUNT);
        let mut left_players: Vec<PlayerId> = Vec::new();
        while seqs.len() < MESSAGE_COUNT || !left_players.contains(&stalled_player_id) {
            let Some(frame) = healthy_rx.next().await else {
                panic!(
                    "healthy receiver closed early ({} of {MESSAGE_COUNT} messages, left={left_players:?})",
                    seqs.len()
                );
            };
            let frame = frame.expect("healthy receiver websocket error");
            let Message::Text(text) = frame else { continue };
            let message: ServerMessage = serde_json::from_str(&text).expect("valid ServerMessage");
            match message {
                ServerMessage::GameData { data, .. } => {
                    let seq = data
                        .get("seq")
                        .and_then(serde_json::Value::as_u64)
                        .expect("GameData payload carries a numeric seq");
                    seqs.push(seq);
                }
                ServerMessage::PlayerLeft { player_id, .. } => left_players.push(player_id),
                ServerMessage::Error {
                    message,
                    error_code,
                } => panic!("healthy receiver got server error: {message} ({error_code:?})"),
                _ => continue,
            }
        }
        (seqs, left_players)
    });

    let deadline = tokio::time::Duration::from_secs(120);
    let (writer_result, healthy_result) =
        tokio::time::timeout(deadline, async { tokio::join!(writer, healthy_reader) })
            .await
            .expect("slow-consumer test exceeded its deadline (sender wedged behind dead peer?)");

    let _sender_sink = writer_result.expect("flood writer task panicked");
    let (seqs, left_players) = healthy_result.expect("healthy reader task panicked");
    assert_complete_and_ordered(&seqs, MESSAGE_COUNT, "healthy receiver");
    assert!(
        left_players.contains(&stalled_player_id),
        "stalled player must be evicted from the room (PlayerLeft), saw {left_players:?}"
    );

    // The eviction must be loud: counted as a slow-consumer disconnect with
    // its abandoned messages recorded as drops.
    assert!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed)
            >= 1,
        "slow-consumer disconnect must be recorded in metrics"
    );
    assert!(
        metrics.websocket_messages_dropped.load(Ordering::Relaxed) >= 1,
        "messages abandoned with the dead connection must be counted as dropped"
    );

    // The stalled client's socket must actually terminate: drain whatever was
    // buffered and require the stream to end (server-initiated close). The
    // best-effort SLOW_CONSUMER farewell is asserted only when it made it
    // through the (intentionally saturated) socket.
    let stalled_termination =
        tokio::time::timeout(tokio::time::Duration::from_secs(30), async move {
            let mut saw_slow_consumer_error = false;
            while let Some(frame) = stalled_rx.next().await {
                match frame {
                    Ok(Message::Text(text)) => {
                        if let Ok(ServerMessage::Error {
                            error_code: Some(ErrorCode::SlowConsumer),
                            ..
                        }) = serde_json::from_str::<ServerMessage>(&text)
                        {
                            saw_slow_consumer_error = true;
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => continue,
                }
            }
            saw_slow_consumer_error
        })
        .await;
    match stalled_termination {
        Ok(saw_farewell) => {
            if saw_farewell {
                println!("stalled client received the SLOW_CONSUMER farewell before close");
            }
        }
        Err(_elapsed) => panic!("stalled client's socket was never closed by the server"),
    }

    // Everything has quiesced (burst delivered, stalled socket closed):
    // attempts must balance against enqueues, disconnect races, and drops.
    assert_message_conservation(&metrics).await;
    running_server.shutdown().await;
}

/// A stalled recipient whose kernel receive window is already full must be
/// preempted even while the server's socket write is wedged mid-frame — and
/// the eviction must actually reclaim the connection's resources.
///
/// The stalled client connects with a deliberately tiny `SO_RCVBUF` (clamped
/// BEFORE the TCP connect) and then stops reading, so the server's send task
/// wedges inside a socket write after a few kilobytes — precisely the state
/// the close-preemption `select!` in the send task exists for. Asserted via
/// metric/gauge polling with generous deadlines (never byte counts — the
/// kernel doubles `SO_RCVBUF` and buffer sizes vary by platform):
///
/// 1. the stall is detected and counted (`slow_consumer_disconnects >= 1`)
///    while the healthy peer keeps receiving (the room keeps flowing);
/// 2. the active-connections gauge returns to its pre-stall baseline within
///    the slow-consumer timeout + the bounded close writes
///    (`CLOSE_WRITE_TIMEOUT`, 1s each for farewell, semantic close frame, and
///    close handshake) + generous scheduling margin;
/// 3. (Linux only) the process's open-fd count returns to its pre-stall
///    baseline — the wedged socket's file descriptor is genuinely released,
///    not leaked to a zombie task;
/// 4. the stalled client, once it resumes reading, observes termination
///    (close frame, EOF, or error — any is acceptable) rather than hanging.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn wedged_socket_write_is_preempted_and_fd_released() {
    /// Sized so a handful of messages saturate the clamped client window and
    /// the server-side socket buffer (≤64KB effective) while staying well
    /// under `max_message_size` (64KB default).
    const WEDGE_PADDING_BYTES: usize = 12 * 1_024;
    /// Requested client `SO_RCVBUF`; the kernel doubles it, which is why all
    /// assertions below are metric-driven rather than byte-count-driven.
    const STALLED_RECV_BUFFER_BYTES: u32 = 4_096;
    /// Hard cap on the flood so a broken eviction path fails the test loudly
    /// instead of flooding forever (reached only when eviction never happens).
    const FLOOD_MESSAGE_CAP: u64 = 50_000;
    /// Reclaim deadline: slow_consumer_timeout (300ms) + three bounded close
    /// writes (1s CLOSE_WRITE_TIMEOUT each: farewell frame, semantic close
    /// frame, close handshake) + generous margin for oversubscribed runners. A
    /// ceiling, not an expected wait — polling returns the instant the state
    /// holds.
    const RECLAIM_DEADLINE: tokio::time::Duration = tokio::time::Duration::from_secs(15);

    let mut server_config = ServerConfig::default();
    server_config.websocket_config.send_queue_capacity = 8;
    server_config.websocket_config.slow_consumer_timeout_ms = 300;
    let server = create_test_server_with_config(server_config, ProtocolConfig::default()).await;
    let metrics = server.metrics();
    let running_server = start_server(server).await;
    let addr = running_server.addr();

    // Healthy receiver and sender join first: their connections (and fds)
    // define the pre-stall baseline that eviction must restore.
    let (mut healthy_sink, mut healthy_rx) = connect(addr).await;
    let (mut sender_sink, mut sender_rx) = connect(addr).await;
    join_room(&mut healthy_sink, &mut healthy_rx, "WedgeHealthy").await;
    join_room(&mut sender_sink, &mut sender_rx, "WedgeSender").await;

    let baseline_active_connections = metrics.active_connections.load(Ordering::Relaxed);
    #[cfg(target_os = "linux")]
    let baseline_fd_count = count_open_fds();

    // The stalled client: tiny receive window, joins, then never reads again
    // until the server has torn it down.
    let stalled_ws = connect_with_small_recv_buffer(addr, STALLED_RECV_BUFFER_BYTES).await;
    let (mut stalled_sink, mut stalled_rx) = stalled_ws.split();
    let stalled_player_id =
        join_room_returning_id(&mut stalled_sink, &mut stalled_rx, "WedgeStalled").await;

    // Flood GameData until the eviction is visible in metrics (metric-driven
    // stop; the cap only bites if eviction is broken). Each relayed message
    // pushes ~12KB toward the stalled client's saturated window.
    let writer_metrics = Arc::clone(&metrics);
    let writer = tokio::spawn(async move {
        let padding = "x".repeat(WEDGE_PADDING_BYTES);
        let mut sent: u64 = 0;
        while writer_metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed)
            == 0
            && sent < FLOOD_MESSAGE_CAP
        {
            let message = ClientMessage::GameData {
                class: None,
                key: None,
                data: serde_json::json!({ "seq": sent, "padding": padding.as_str() }),
            };
            let json = serde_json::to_string(&message).expect("serialize GameData");
            sender_sink
                .send(Message::Text(json.into()))
                .await
                .expect("send GameData while a peer's socket is wedged");
            sent += 1;
        }
        (sender_sink, sent)
    });

    // The healthy receiver must keep flowing during the stall and observe the
    // stalled player's eviction (PlayerLeft via the normal disconnect flow).
    let healthy_reader = tokio::spawn(async move {
        let mut game_data_received: u64 = 0;
        loop {
            let Some(frame) = healthy_rx.next().await else {
                panic!(
                    "healthy receiver closed early (after {game_data_received} GameData messages)"
                );
            };
            let frame = frame.expect("healthy receiver websocket error");
            let Message::Text(text) = frame else { continue };
            let message: ServerMessage = serde_json::from_str(&text).expect("valid ServerMessage");
            match message {
                ServerMessage::GameData { .. } => game_data_received += 1,
                ServerMessage::PlayerLeft { player_id, .. } if player_id == stalled_player_id => {
                    return (game_data_received, healthy_rx);
                }
                ServerMessage::Error {
                    message,
                    error_code,
                } => panic!("healthy receiver got server error: {message} ({error_code:?})"),
                _ => continue,
            }
        }
    });

    // (1) Eviction detected: both tasks complete under a generous deadline —
    // the writer stops on the disconnect metric, the healthy reader on the
    // PlayerLeft broadcast — proving the wedged write never blocked either.
    let deadline = tokio::time::Duration::from_secs(60);
    let (writer_result, healthy_result) =
        tokio::time::timeout(deadline, async { tokio::join!(writer, healthy_reader) })
            .await
            .expect("wedged-socket test exceeded its deadline (eviction never happened?)");
    let (_sender_sink, sent) = writer_result.expect("flood writer task panicked");
    let (healthy_game_data, _healthy_rx) = healthy_result.expect("healthy reader task panicked");
    assert!(
        sent < FLOOD_MESSAGE_CAP,
        "flood cap reached without a slow-consumer disconnect being recorded"
    );
    assert!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed)
            >= 1,
        "the wedged connection must be evicted as a slow consumer"
    );
    assert!(
        healthy_game_data >= 1,
        "the healthy receiver must keep receiving while a peer's socket is wedged"
    );

    // (2) The connection is reclaimed: the active-connections gauge returns to
    // its pre-stall baseline even though the socket write was wedged mid-frame
    // (the close request preempts the write inside the send task's select).
    let reclaim_deadline = tokio::time::Instant::now() + RECLAIM_DEADLINE;
    loop {
        let active = metrics.active_connections.load(Ordering::Relaxed);
        if active <= baseline_active_connections {
            break;
        }
        assert!(
            tokio::time::Instant::now() < reclaim_deadline,
            "active connections never returned to the pre-stall baseline: \
             {active} > {baseline_active_connections}"
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;
    }

    // (4, before 3 so the client-side fd is released too) The stalled client
    // resumes reading and must observe termination — close frame, EOF, or
    // error, anything but a hang.
    let stalled_termination =
        tokio::time::timeout(tokio::time::Duration::from_secs(30), async move {
            loop {
                match stalled_rx.next().await {
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => continue,
                }
            }
            stalled_rx
        })
        .await;
    let stalled_rx = match stalled_termination {
        Ok(stalled_rx) => stalled_rx,
        Err(_elapsed) => {
            panic!("stalled client never observed termination after resuming reads")
        }
    };
    drop(stalled_rx);
    drop(stalled_sink);

    // (3) Linux only: the wedged socket's file descriptor is genuinely
    // released back to the process. Serialized test execution (see the module
    // doc) keeps the process-wide fd table deterministic here.
    #[cfg(target_os = "linux")]
    {
        let fd_deadline = tokio::time::Instant::now() + RECLAIM_DEADLINE;
        loop {
            let fd_count = count_open_fds();
            if fd_count <= baseline_fd_count {
                break;
            }
            assert!(
                tokio::time::Instant::now() < fd_deadline,
                "open fd count never returned to the pre-stall baseline: \
                 {fd_count} > {baseline_fd_count}"
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;
        }
    }

    // Everything has quiesced (flood finished, eviction completed, stalled
    // socket torn down): the conservation counters must balance.
    assert_message_conservation(&metrics).await;
    running_server.shutdown().await;
}

/// Count this process's open file descriptors via `/proc/self/fd`.
///
/// The `read_dir` handle itself briefly adds one entry, but it does so for
/// the baseline and the polled samples alike, so comparisons stay exact.
#[cfg(target_os = "linux")]
fn count_open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("read /proc/self/fd")
        .count()
}

/// Coordinator-level check against in-process delivery channels — NOT
/// real-socket evidence. The "clients" here are bare `register_client` mpsc
/// handles with no WebSocket, no TCP, and no kernel buffers, so this pins the
/// coordinator's backpressure accounting deterministically: with a tiny
/// recipient queue, a burst must wait for the consumer (counted as
/// backpressure) and still deliver every message in order. The socket-level
/// version of this contract is covered by the real-socket tests above and by
/// [`wedged_socket_write_is_preempted_and_fd_released`].
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn backpressure_delivers_every_message_in_order_without_disconnecting() {
    const MESSAGE_COUNT: usize = 10;

    let server = create_test_server().await;
    let metrics = server.metrics();

    let (tx1, mut rx1) = tokio::sync::mpsc::channel(64);
    // Deliberately tiny: the burst below cannot fit, forcing the send().await path.
    let (tx2, mut rx2) = tokio::sync::mpsc::channel(2);

    let sender_id = server
        .register_client(tx1, "127.0.0.1:0".parse().expect("addr"))
        .await
        .expect("register sender");
    let receiver_id = server
        .register_client(tx2, "127.0.0.1:0".parse().expect("addr"))
        .await
        .expect("register receiver");

    join_room_direct(&server, &sender_id, "BPSender").await;
    join_room_direct(&server, &receiver_id, "BPReceiver").await;

    // Drain the join-time notifications so the tiny queue starts empty.
    drain_join_chatter(&mut rx2, "backpressure receiver");
    drain_join_chatter(&mut rx1, "backpressure sender");

    let flood_server = Arc::clone(&server);
    let flood = tokio::spawn(async move {
        for seq in 0..MESSAGE_COUNT {
            flood_server
                .handle_client_message(
                    &sender_id,
                    ClientMessage::GameData {
                        class: None,
                        key: None,
                        data: serde_json::json!({ "seq": seq as u64 }),
                    },
                )
                .await;
        }
    });

    // Do not drain until the sender has demonstrably hit backpressure, so this
    // test cannot pass by racing the reader ahead of the writer.
    let observed_backpressure = tokio::time::timeout(tokio::time::Duration::from_secs(10), async {
        while metrics
            .websocket_backpressure_events
            .load(Ordering::Relaxed)
            == 0
        {
            tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(
        observed_backpressure.is_ok(),
        "a {MESSAGE_COUNT}-message burst into a 2-slot queue must register backpressure"
    );

    let drained = tokio::time::timeout(tokio::time::Duration::from_secs(30), async {
        let mut seqs = Vec::with_capacity(MESSAGE_COUNT);
        while seqs.len() < MESSAGE_COUNT {
            let Some(message) = rx2.recv().await else {
                panic!("receiver channel closed after {} messages", seqs.len());
            };
            if let ServerMessage::GameData { data, .. } = message.as_ref() {
                let seq = data
                    .get("seq")
                    .and_then(serde_json::Value::as_u64)
                    .expect("numeric seq");
                seqs.push(seq);
            }
        }
        seqs
    })
    .await
    .expect("backpressured delivery must complete once the consumer drains");

    for (index, seq) in drained.iter().enumerate() {
        assert_eq!(
            *seq, index as u64,
            "backpressured delivery must stay ordered"
        );
    }

    flood.await.expect("flood task panicked");
    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        0,
        "a consumer that drains within the grace window must not be disconnected"
    );

    // The flood completed and the receiver drained every message, so all
    // deliveries have resolved: the conservation counters must balance.
    assert_message_conservation(&metrics).await;
}

/// Coordinator-level check against in-process delivery channels — NOT
/// real-socket evidence. The unresponsive "recipient" is a bare
/// `register_client` mpsc handle that is never read (no WebSocket, no TCP, no
/// kernel buffers), pinning the coordinator's pruning deterministically: a
/// recipient that never drains is pruned after the grace window, senders
/// complete promptly afterwards, and the loss is visible in metrics. The
/// socket-level version of this contract — a real wedged socket write being
/// preempted — is covered by
/// [`wedged_socket_write_is_preempted_and_fd_released`].
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn unresponsive_recipient_is_pruned_without_blocking_senders() {
    let mut server_config = ServerConfig::default();
    server_config.websocket_config.slow_consumer_timeout_ms = 100;
    let server = create_test_server_with_config(server_config, ProtocolConfig::default()).await;
    let metrics = server.metrics();

    let (tx1, mut rx1) = tokio::sync::mpsc::channel(64);
    // Two slots and NO reader: fills immediately and never drains.
    let (tx2, _rx2_kept_alive_but_never_read) = tokio::sync::mpsc::channel(2);

    let sender_id = server
        .register_client(tx1, "127.0.0.1:0".parse().expect("addr"))
        .await
        .expect("register sender");
    let receiver_id = server
        .register_client(tx2, "127.0.0.1:0".parse().expect("addr"))
        .await
        .expect("register receiver");

    join_room_direct(&server, &sender_id, "PruneSender").await;
    join_room_direct(&server, &receiver_id, "PruneReceiver").await;
    drain_join_chatter(&mut rx1, "prune sender");

    // Six sends: the receiver's queue holds its RoomJoined + one GameData at
    // most; every subsequent send must ride the backpressure path, and the
    // 100ms grace window must convert the dead consumer into a pruned one
    // rather than stalling this loop forever.
    let sends = tokio::time::timeout(tokio::time::Duration::from_secs(30), async {
        for seq in 0..6_u64 {
            server
                .handle_client_message(
                    &sender_id,
                    ClientMessage::GameData {
                        class: None,
                        key: None,
                        data: serde_json::json!({ "seq": seq }),
                    },
                )
                .await;
        }
    })
    .await;
    assert!(
        sends.is_ok(),
        "senders must never be wedged indefinitely behind an unresponsive recipient"
    );

    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        1,
        "exactly one slow-consumer disconnect must be recorded"
    );
    assert!(
        metrics.websocket_messages_dropped.load(Ordering::Relaxed) >= 1,
        "the message abandoned at disconnect must be counted"
    );
    assert!(
        metrics
            .websocket_backpressure_events
            .load(Ordering::Relaxed)
            >= 1,
        "the full queue must have been recorded as backpressure first"
    );

    // All six sends have returned (the timeout above proved it), so every
    // delivery has resolved: the conservation counters must balance.
    assert_message_conservation(&metrics).await;
}

/// Empty a receiver of join-time notifications. `Empty` is the expected
/// terminal state; a disconnected channel here means the client was torn down
/// mid-setup, which must fail the test loudly.
fn drain_join_chatter(receiver: &mut tokio::sync::mpsc::Receiver<Arc<ServerMessage>>, who: &str) {
    loop {
        match receiver.try_recv() {
            Ok(_join_notification) => continue,
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                panic!("{who}: channel disconnected while draining join chatter")
            }
        }
    }
}

async fn join_room_direct(server: &Arc<EnhancedGameServer>, player_id: &PlayerId, name: &str) {
    server
        .handle_join_room(
            player_id,
            "backpressure_game".to_string(),
            Some("BPRESS".to_string()),
            name.to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;
}

/// Like [`join_room`] but returns the server-assigned player id.
async fn join_room_returning_id(
    sink: &mut WsSink,
    receiver: &mut WsReceiver,
    player_name: &str,
) -> PlayerId {
    let join = ClientMessage::JoinRoom {
        game_name: "burst_game".to_string(),
        room_code: Some("BURST1".to_string()),
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
