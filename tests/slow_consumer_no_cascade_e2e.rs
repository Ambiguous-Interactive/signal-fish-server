//! P10.C H1+H13 — a slow consumer's eviction must NOT cascade to healthy peers.
//!
//! The distinctive, deterministic PR-lane claim of H1+H13 is the NO-CASCADE
//! guarantee: when one recipient stalls hard enough to be evicted by the
//! delivery contract, EXACTLY that one connection is torn down — the room's
//! other healthy members are neither evicted nor starved, and each still
//! receives the sender's complete, in-order stream.
//!
//! The existing delivery suite proves eviction is loud and that ONE healthy
//! receiver keeps flowing (`relay_backpressure_e2e::
//! slow_consumer_is_disconnected_loudly_and_room_keeps_flowing`,
//! `wedged_socket_write_is_preempted_and_fd_released`;
//! `v3_game_data_sequencing_e2e::evicted_recipient_observes_seq_gap_after_reconnect`).
//! But "cascade" is inherently a MULTI-peer phenomenon, and every one of those
//! tests uses a single healthy witness and asserts only
//! `slow_consumer_disconnects >= 1` — none pins that the eviction did not
//! SPREAD. This test closes that gap directly:
//!
//! - a room of one flooding sender, one hard-stalled recipient (never drains),
//!   and THREE healthy recipients (all draining continuously);
//! - each of the three healthy peers must receive every one of the sender's
//!   messages, contiguous and in order (a cascade would show up as a healthy
//!   peer's socket closing early or its stream losing frames);
//! - each healthy peer must observe a `PlayerLeft` for the stalled peer ONLY —
//!   a `PlayerLeft` for any OTHER member would be a peer being cascade-evicted;
//! - `websocket_slow_consumer_disconnects == 1` EXACTLY (not `>= 1`): the
//!   direct no-cascade oracle. This is sound and deterministic because the
//!   coordinator fans a broadcast out to every recipient CONCURRENTLY
//!   (`server.rs::deliver_to_all` via `join_all`), each recipient's
//!   slow-consumer timeout is independent, the evicted connection is removed
//!   before the next broadcast, and the disconnect metric is incremented once
//!   per CLOSED CONNECTION, not per delivery
//!   (`coordination::deliver_or_disconnect`: only the `request_close` that
//!   returns `true` counts).
//!
//! Zero-flaky policy: a hard-stalled peer (never reads) is evicted regardless
//! of timing, so the eviction is not a timing race; the healthy peers drain in
//! tight loops and are given a generous grace window, so they never approach
//! the slow-consumer timeout; all waits are bounded by a whole-test ceiling,
//! never a fixed sleep used as a sync or negative oracle. The brittle facets of
//! H1+H13 (the whole-room freeze DURATION / Pong-RTT spike, join-during-stall)
//! are deliberately NOT asserted here — they are timing-coupled and belong to
//! the nightly lane.

mod test_helpers;
mod websocket_test_helpers;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::config::ProtocolConfig;
use signal_fish_server::protocol::{ClientMessage, PlayerId, ServerMessage};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::create_router;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use test_helpers::create_test_server_with_config;
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::assert_message_conservation;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
type WsReceiver = futures_util::stream::SplitStream<WsStream>;

const ROOM: &str = "NOCAS1";
/// Healthy recipients that must all survive the one eviction. Three is enough
/// to make "cascade" (a spreading eviction) observable while staying a fast
/// PR-lane profile.
const HEALTHY_PEERS: usize = 3;
/// Fixed flood size. Large enough that (a) the stalled peer's kernel buffers +
/// 8-slot queue overflow into an eviction well before the flood ends and (b)
/// there is a long post-eviction tail proving the room keeps flowing; small
/// enough to stay fast even fanned out to every healthy peer.
const MESSAGE_COUNT: u64 = 1_000;
/// 16 KiB per frame so the flood cannot hide inside kernel socket buffers — the
/// stalled peer's queue must genuinely overflow into eviction.
const STALL_PADDING_BYTES: usize = 16 * 1_024;
/// Whole-test ceiling (oversubscribed CI runners included): a ceiling, not an
/// expected wait — the happy path finishes far sooner.
const TEST_DEADLINE: tokio::time::Duration = tokio::time::Duration::from_secs(120);

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

/// Join the shared room (creating it with an 8-seat cap on the first join, so
/// all five members fit) and return the server-assigned player id.
async fn join_room_returning_id(
    sink: &mut WsSink,
    receiver: &mut WsReceiver,
    player_name: &str,
) -> PlayerId {
    let join = ClientMessage::JoinRoom {
        game_name: "no_cascade_game".to_string(),
        room_code: Some(ROOM.to_string()),
        player_name: player_name.to_string(),
        max_players: Some(8),
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

/// One healthy recipient's drain: it must receive the sender's complete, in-order
/// stream and observe the stalled peer's `PlayerLeft` and NOTHING else that
/// smells of a cascade. Returns the number of GameData frames received (which
/// the caller asserts equals the flood size).
async fn drain_healthy_peer(mut receiver: WsReceiver, index: usize, stalled_id: PlayerId) -> u64 {
    let mut received: u64 = 0;
    let mut saw_stalled_left = false;

    while received < MESSAGE_COUNT || !saw_stalled_left {
        // A healthy peer's socket must never close: an early end-of-stream or a
        // transport error here IS the cascade this test exists to forbid.
        let Some(frame) = receiver.next().await else {
            panic!(
                "healthy peer {index} socket closed after {received} of {MESSAGE_COUNT} frames — \
                 the eviction cascaded to a healthy peer"
            );
        };
        let frame = frame.unwrap_or_else(|error| {
            panic!(
                "healthy peer {index} websocket error after {received} frames (cascade?): {error}"
            )
        });
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
                assert_eq!(
                    seq, received,
                    "healthy peer {index}: relay must stay complete and in order while a \
                     peer is evicted (expected seq {received}, got {seq})"
                );
                received += 1;
            }
            ServerMessage::PlayerLeft { player_id, .. } => {
                assert_eq!(
                    player_id, stalled_id,
                    "healthy peer {index} observed a PlayerLeft for a NON-stalled member \
                     ({player_id}) — the eviction cascaded to a healthy peer"
                );
                saw_stalled_left = true;
            }
            ServerMessage::Error {
                message,
                error_code,
            } => panic!("healthy peer {index} got a server error: {message} ({error_code:?})"),
            _ => continue,
        }
    }
    received
}

/// H1+H13: one hard-stalled recipient is evicted while THREE healthy peers all
/// keep the complete stream and exactly one connection is torn down — no
/// cascade.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_consumer_eviction_does_not_cascade_to_healthy_peers() {
    // Small queue so the stalled peer overflows quickly; a grace window well
    // above any drain a continuously-reading peer needs, so the healthy peers
    // have generous margin against a spurious eviction (only the peer that
    // stops reading entirely can trip it).
    let mut server_config = ServerConfig::default();
    server_config.websocket_config.send_queue_capacity = 8;
    server_config.websocket_config.slow_consumer_timeout_ms = 500;
    let server = create_test_server_with_config(server_config, ProtocolConfig::default()).await;
    let metrics = server.metrics();
    let addr = start_server(server).await;

    // Join order: the healthy peers first (so each is an established room member
    // that will observe the stalled peer's later join and eviction), then the
    // sender, then the stalled peer last.
    let mut healthy_sinks: Vec<WsSink> = Vec::with_capacity(HEALTHY_PEERS);
    let mut healthy_receivers: Vec<WsReceiver> = Vec::with_capacity(HEALTHY_PEERS);
    for index in 0..HEALTHY_PEERS {
        let (mut sink, mut receiver) = connect(addr).await;
        let _id =
            join_room_returning_id(&mut sink, &mut receiver, &format!("Healthy{index}")).await;
        healthy_sinks.push(sink);
        healthy_receivers.push(receiver);
    }

    // The sender's own receive half stays alive but unread after the join
    // handshake: during the flood it only ever queues the stalled peer's
    // join/leave notices (two tiny control frames, well within the 8-slot
    // queue), so the sender is never itself evicted — keeping the eviction
    // count at exactly one. `sender_rx` is deliberately kept in scope (never
    // moved into the writer task) so the sender connection stays open.
    let (mut sender_sink, mut sender_rx) = connect(addr).await;
    let _sender_id = join_room_returning_id(&mut sender_sink, &mut sender_rx, "FloodSender").await;

    // The stalled peer: joins, then NEVER reads its socket again. Both halves
    // are kept alive in scope so the connection persists (and genuinely stalls)
    // until the server evicts it.
    let (mut stalled_sink, mut stalled_rx) = connect(addr).await;
    let stalled_id = join_room_returning_id(&mut stalled_sink, &mut stalled_rx, "Stalled").await;

    // Flood the room. The writer runs on its own task so server-side
    // backpressure (the broadcast that parks on the stalled peer until it is
    // evicted) cannot deadlock against the healthy drains below.
    let writer = tokio::spawn(async move {
        let padding = "x".repeat(STALL_PADDING_BYTES);
        for seq in 0..MESSAGE_COUNT {
            let message = ClientMessage::GameData {
                class: None,
                key: None,
                data: serde_json::json!({ "seq": seq, "padding": padding.as_str() }),
            };
            let json = serde_json::to_string(&message).expect("serialize GameData");
            sender_sink
                .send(Message::Text(json.into()))
                .await
                .expect("send GameData while a peer is stalled");
        }
        sender_sink
    });

    // Every healthy peer drains concurrently.
    let mut healthy_readers = Vec::with_capacity(HEALTHY_PEERS);
    for (index, receiver) in healthy_receivers.into_iter().enumerate() {
        healthy_readers.push(tokio::spawn(drain_healthy_peer(
            receiver, index, stalled_id,
        )));
    }

    let (writer_result, healthy_results) = tokio::time::timeout(TEST_DEADLINE, async {
        let writer_result = writer.await;
        let mut healthy_results = Vec::with_capacity(HEALTHY_PEERS);
        for reader in healthy_readers {
            healthy_results.push(reader.await);
        }
        (writer_result, healthy_results)
    })
    .await
    .expect("no-cascade test exceeded its deadline (eviction never happened, or a healthy peer wedged?)");

    let _sender_sink = writer_result.expect("flood writer task panicked");
    for (index, result) in healthy_results.into_iter().enumerate() {
        let received = result.expect("healthy reader task panicked");
        assert_eq!(
            received, MESSAGE_COUNT,
            "healthy peer {index} must receive the full flood despite the eviction"
        );
    }

    // The core no-cascade oracle: EXACTLY one connection was evicted (the
    // stalled peer), not the healthy peers alongside it.
    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        1,
        "exactly one slow-consumer eviction is allowed — the stalled peer; the \
         {HEALTHY_PEERS} healthy peers must not be cascade-evicted"
    );
    // The eviction must be loud: the stalled peer's abandoned frames are counted
    // as drops (non-vacuity: proves the stall really overflowed into eviction).
    assert!(
        metrics.websocket_messages_dropped.load(Ordering::Relaxed) >= 1,
        "the stalled peer's abandoned frames must be counted as drops"
    );

    // The stalled peer's socket must actually be torn down by the server: drain
    // whatever was buffered and require the stream to end. This confirms the
    // single eviction was the stalled peer (non-vacuity for the `== 1` above).
    let stalled_termination =
        tokio::time::timeout(tokio::time::Duration::from_secs(30), async move {
            while let Some(frame) = stalled_rx.next().await {
                match frame {
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => continue,
                }
            }
        })
        .await;
    assert!(
        stalled_termination.is_ok(),
        "the stalled peer's socket was never closed by the server"
    );
    drop(stalled_sink);
    // Keep the healthy sinks alive until here so none of the healthy
    // connections is torn down early by a dropped write half.
    drop(healthy_sinks);

    // Everything has quiesced (flood delivered, one eviction, stalled socket
    // closed): the conservation counters must balance.
    assert_message_conservation(&metrics).await;
}
