//! End-to-end protocol v3 GameData sequencing tests through the real
//! WebSocket stack.
//!
//! v3 adds a server-stamped, per-(sender, room) `seq` to relayed `GameData` so
//! recipients can DETECT any server-side loss as a gap — the protocol-level
//! capability the reporter of issue #131 lacked (they had to keep manual
//! sent-vs-received deficit counters). The contract under test:
//!
//! - `seq` starts at 1 per sender incarnation and is strictly increasing;
//!   reliable delivery is contiguous, while every lossy-class hole requires
//!   an exact causally prior `DeliveryReport`;
//! - pre-v3 recipients never see the key (their bytes are byte-identical to
//!   v2, frozen separately by `tests/v2_wire_golden.rs`);
//! - `seq` restarts at 1 when the sender leaves/rejoins or reconnects, paired
//!   with a higher `epoch` so the reset is self-describing;
//! - a reconnecting recipient receives authoritative sender watermarks before
//!   later data instead of inferring missed high-rate data from a wire gap.

mod test_helpers;
mod websocket_test_helpers;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::config::ProtocolConfig;
use signal_fish_server::protocol::{
    ClientMessage, DeliveryClass, DeliveryGapReason, ErrorCode, PlayerId, RoomId, ServerMessage,
};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::create_router;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use test_helpers::{create_test_server_with_config, test_server_config, RunningTestServer};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::{
    assert_message_conservation, next_matching_server_message_within, WsStream,
};

const SERVER_MESSAGE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(10);

/// Whole-test ceiling for the burst/eviction tests (oversubscribed CI runners
/// included) — a ceiling, not an expected wait.
const TEST_DEADLINE: tokio::time::Duration = tokio::time::Duration::from_secs(120);

/// Burst size for the contiguity test: large enough to exercise batching and
/// queueing, small enough to stay fast on loaded runners.
const BURST_MESSAGE_COUNT: u64 = 2_000;

/// Protocol config for these tests: default `[2, 4]` version range with SDK
/// platform enforcement off so a bare `Authenticate` (no platform) succeeds.
fn v3_protocol_config() -> ProtocolConfig {
    let mut protocol_config = ProtocolConfig::default();
    protocol_config.sdk_compatibility.enforce = false;
    protocol_config
}

async fn start_test_server(
    server_config: ServerConfig,
) -> (RunningTestServer, Arc<EnhancedGameServer>) {
    let server = create_test_server_with_config(server_config, v3_protocol_config()).await;
    let running_server = start_server(server.clone()).await;
    (running_server, server)
}

async fn start_server(server: Arc<EnhancedGameServer>) -> RunningTestServer {
    let router = create_router("http://localhost:3000").with_state(server.clone());
    RunningTestServer::spawn(server, router).await
}

async fn connect(addr: std::net::SocketAddr) -> WsStream {
    let url = format!("ws://{addr}/ws");
    let (ws, _) = tokio::time::timeout(tokio::time::Duration::from_secs(10), connect_async(&url))
        .await
        .expect("websocket connect timed out")
        .expect("websocket connect failed");
    ws
}

async fn send(ws: &mut WsStream, msg: &ClientMessage) {
    let json = serde_json::to_string(msg).expect("serialize client message");
    ws.send(Message::Text(json.into()))
        .await
        .expect("send client message");
}

/// Authenticate advertising `protocol_version` and assert the negotiated
/// version echoed in `ProtocolInfo` (None expected for a negotiated v2).
async fn authenticate_with_version(
    ws: &mut WsStream,
    protocol_version: u16,
    expect_reported: Option<u16>,
) {
    send(
        ws,
        &ClientMessage::Authenticate {
            app_id: "v3-seq-test".to_string(),
            sdk_version: None,
            platform: None,
            game_data_format: None,
            protocol_version: Some(protocol_version),
            supported_transports: None,
            supported_topologies: None,
        },
    )
    .await;

    let authed = next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "Authenticated response",
        |message| matches!(message, ServerMessage::Authenticated { .. }).then_some(()),
    )
    .await;
    let () = authed;

    let info = next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "ProtocolInfo response",
        |message| match message {
            ServerMessage::ProtocolInfo(info) => Some(info),
            _ => None,
        },
    )
    .await;
    assert_eq!(
        info.protocol_version, expect_reported,
        "negotiated protocol version mismatch"
    );
}

async fn authenticate_v3(ws: &mut WsStream) {
    authenticate_with_version(ws, 3, Some(3)).await;
}

async fn authenticate_v2(ws: &mut WsStream) {
    authenticate_with_version(ws, 2, None).await;
}

fn join_message(room_code: &str, player_name: &str) -> ClientMessage {
    ClientMessage::JoinRoom {
        game_name: "v3_seq_game".to_string(),
        room_code: Some(room_code.to_string()),
        player_name: player_name.to_string(),
        max_players: Some(8),
        supports_authority: Some(false),
        relay_transport: None,
    }
}

/// Join a room, returning `(player_id, room_id)` from the RoomJoined payload.
async fn join_room(ws: &mut WsStream, room_code: &str, player_name: &str) -> (PlayerId, RoomId) {
    send(ws, &join_message(room_code, player_name)).await;
    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "room join response",
        |message| match message {
            ServerMessage::RoomJoined(payload) => Some((payload.player_id, payload.room_id)),
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                panic!("room join failed: {reason} ({error_code:?})")
            }
            _ => None,
        },
    )
    .await
}

/// A received relayed frame: `(from_player, server-stamped seq, incarnation
/// epoch, payload)`.
struct ReceivedGameData {
    from_player: PlayerId,
    seq: Option<u64>,
    epoch: Option<u32>,
    data: serde_json::Value,
}

/// Drain `ws` until `expected` GameData frames arrive, returning them in
/// arrival order. Server errors and early closes fail loudly so a drop can
/// never masquerade as success.
async fn collect_game_data(ws: &mut WsStream, expected: usize) -> Vec<ReceivedGameData> {
    let mut received = Vec::with_capacity(expected);
    while received.len() < expected {
        let Some(frame) = ws.next().await else {
            panic!(
                "connection closed after {} of {expected} GameData frames",
                received.len()
            );
        };
        let frame = frame.expect("websocket error while draining GameData");
        let Message::Text(text) = frame else {
            continue;
        };
        let message: ServerMessage = serde_json::from_str(&text).expect("valid ServerMessage");
        match message {
            ServerMessage::GameData {
                from_player,
                data,
                seq,
                epoch,
                ..
            } => received.push(ReceivedGameData {
                from_player,
                seq,
                epoch,
                data,
            }),
            ServerMessage::Error {
                message,
                error_code,
            } => panic!(
                "server error while draining GameData (after {} frames): \
                 {message} ({error_code:?})",
                received.len()
            ),
            _ => continue,
        }
    }
    received
}

/// Assert `frames` (all from one sender) carry exactly `first..=last`,
/// contiguous and in order.
fn assert_contiguous_seqs(frames: &[ReceivedGameData], first: u64, last: u64, who: &str) {
    assert_eq!(
        frames.len() as u64,
        last - first + 1,
        "{who}: expected seqs {first}..={last}"
    );
    for (index, frame) in frames.iter().enumerate() {
        let expected = first + index as u64;
        assert_eq!(
            frame.seq,
            Some(expected),
            "{who}: seq must be server-stamped and contiguous (frame {index})"
        );
    }
}

/// (a) A 2000-message burst from one v3 sender reaches a v3 recipient with a
/// server-stamped `seq` on every frame — exactly 1..=2000, contiguous, in
/// order — so any server-side loss would be observable as a gap.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v3_burst_is_seq_stamped_contiguously_from_one() {
    let (running_server, server) = start_test_server(test_server_config()).await;
    let addr = running_server.addr();
    let metrics = server.metrics();

    let mut sender = connect(addr).await;
    let mut recipient = connect(addr).await;
    authenticate_v3(&mut sender).await;
    authenticate_v3(&mut recipient).await;
    join_room(&mut sender, "SEQBS1", "SeqSender").await;
    join_room(&mut recipient, "SEQBS1", "SeqRecipient").await;

    let writer = tokio::spawn(async move {
        for n in 0..BURST_MESSAGE_COUNT {
            let message = ClientMessage::GameData {
                class: None,
                key: None,
                data: serde_json::json!({ "n": n }),
            };
            let json = serde_json::to_string(&message).expect("serialize GameData");
            sender
                .send(Message::Text(json.into()))
                .await
                .expect("send GameData burst frame");
        }
        sender
    });

    let reader = tokio::spawn(async move {
        let frames = collect_game_data(&mut recipient, BURST_MESSAGE_COUNT as usize).await;
        (frames, recipient)
    });

    let (writer_result, reader_result) =
        tokio::time::timeout(TEST_DEADLINE, async { tokio::join!(writer, reader) })
            .await
            .expect("seq burst test exceeded its deadline");
    let _sender = writer_result.expect("burst writer task panicked");
    let (frames, _recipient) = reader_result.expect("burst reader task panicked");

    assert_contiguous_seqs(&frames, 1, BURST_MESSAGE_COUNT, "v3 recipient");
    // Payload order must match stamp order (the stamp is assigned at relay
    // time on the sender's in-order message stream).
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(
            frame.data.get("n").and_then(serde_json::Value::as_u64),
            Some(index as u64),
            "payload order must match seq order"
        );
    }

    assert_message_conservation(&metrics).await;
    running_server.shutdown().await;
}

/// (b) Mixed room: the v3 recipient's raw JSON frame carries a `seq` key; the
/// v2 recipient's raw JSON frame has NO `seq` key at all (byte-level absence,
/// not just a null — the pre-v3 wire is unchanged).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_room_v2_recipient_raw_json_has_no_seq_key() {
    let (running_server, server) = start_test_server(test_server_config()).await;
    let addr = running_server.addr();
    let metrics = server.metrics();

    let mut sender = connect(addr).await;
    let mut v3_recipient = connect(addr).await;
    let mut v2_recipient = connect(addr).await;
    authenticate_v3(&mut sender).await;
    authenticate_v3(&mut v3_recipient).await;
    authenticate_v2(&mut v2_recipient).await;
    join_room(&mut sender, "SEQMIX", "MixSender").await;
    join_room(&mut v3_recipient, "SEQMIX", "MixV3").await;
    join_room(&mut v2_recipient, "SEQMIX", "MixV2").await;

    send(
        &mut sender,
        &ClientMessage::GameData {
            class: Some(DeliveryClass::Latest),
            key: Some(7),
            data: serde_json::json!({ "probe": true }),
        },
    )
    .await;

    let v3_raw = next_raw_game_data(&mut v3_recipient, "v3 recipient raw frame").await;
    let v2_raw = next_raw_game_data(&mut v2_recipient, "v2 recipient raw frame").await;

    let v3_value: serde_json::Value = serde_json::from_str(&v3_raw).expect("v3 frame JSON");
    let v3_data = v3_value
        .get("data")
        .and_then(serde_json::Value::as_object)
        .expect("v3 frame /data object");
    assert_eq!(
        v3_data.get("seq").and_then(serde_json::Value::as_u64),
        Some(1),
        "v3 recipient must see the server-stamped seq: {v3_raw}"
    );
    // The v3 recipient also sees the incarnation epoch (first incarnation ⇒ 1),
    // stamped together with seq.
    assert_eq!(
        v3_data.get("epoch").and_then(serde_json::Value::as_u64),
        Some(1),
        "v3 recipient must see the server-stamped epoch: {v3_raw}"
    );
    assert_eq!(v3_data.get("class"), Some(&serde_json::json!("latest")));
    assert_eq!(v3_data.get("key"), Some(&serde_json::json!(7)));

    let v2_value: serde_json::Value = serde_json::from_str(&v2_raw).expect("v2 frame JSON");
    let v2_data = v2_value
        .get("data")
        .and_then(serde_json::Value::as_object)
        .expect("v2 frame /data object");
    assert!(
        !v2_data.contains_key("seq"),
        "v2 recipient's raw frame must not contain a seq key: {v2_raw}"
    );
    assert!(
        !v2_data.contains_key("epoch"),
        "v2 recipient's raw frame must not contain an epoch key: {v2_raw}"
    );
    assert!(!v2_data.contains_key("class"));
    assert!(!v2_data.contains_key("key"));
    // Everything else is identical between the two recipients' frames.
    assert_eq!(v2_data.get("from_player"), v3_data.get("from_player"));
    assert_eq!(v2_data.get("data"), v3_data.get("data"));

    assert_message_conservation(&metrics).await;
    running_server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_delivery_class_does_not_consume_a_relay_sequence() {
    let (running_server, _server) = start_test_server(test_server_config()).await;
    let addr = running_server.addr();
    let mut sender = connect(addr).await;
    let mut recipient = connect(addr).await;
    authenticate_v3(&mut sender).await;
    authenticate_v3(&mut recipient).await;
    join_room(&mut sender, "CLSINV", "ClassSender").await;
    join_room(&mut recipient, "CLSINV", "ClassRecipient").await;

    for (class, key) in [
        (Some(DeliveryClass::Latest), None),
        (Some(DeliveryClass::Reliable), Some(1)),
        (Some(DeliveryClass::Volatile), Some(1)),
        (None, Some(1)),
    ] {
        send(
            &mut sender,
            &ClientMessage::GameData {
                data: serde_json::json!({ "invalid": true }),
                class,
                key,
            },
        )
        .await;
        let error_code = next_matching_server_message_within(
            &mut sender,
            SERVER_MESSAGE_TIMEOUT,
            "invalid delivery-class error",
            |message| match message {
                ServerMessage::Error { error_code, .. } => error_code,
                _ => None,
            },
        )
        .await;
        assert_eq!(error_code, ErrorCode::InvalidDeliveryClass);
    }

    send(
        &mut sender,
        &ClientMessage::GameData {
            data: serde_json::json!({ "valid": true }),
            class: None,
            key: None,
        },
    )
    .await;
    let frame = collect_game_data(&mut recipient, 1).await.remove(0);
    assert_eq!(frame.seq, Some(1));
    running_server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn latest_coalescing_reports_exact_gap_before_successor() {
    let mut config = test_server_config();
    config.websocket_config.enable_batching = true;
    config.websocket_config.batch_size = 8;
    config.websocket_config.batch_interval_ms = 250;
    let (running_server, server) = start_test_server(config).await;
    let addr = running_server.addr();
    let mut sender = connect(addr).await;
    let mut recipient = connect(addr).await;
    authenticate_v3(&mut sender).await;
    authenticate_v3(&mut recipient).await;
    let (sender_id, _) = join_room(&mut sender, "CLSCOA", "LatestSender").await;
    join_room(&mut recipient, "CLSCOA", "LatestRecipient").await;

    for n in [1, 2] {
        send(
            &mut sender,
            &ClientMessage::GameData {
                data: serde_json::json!({ "n": n }),
                class: Some(DeliveryClass::Latest),
                key: Some(99),
            },
        )
        .await;
    }

    let report = next_matching_server_message_within(
        &mut recipient,
        SERVER_MESSAGE_TIMEOUT,
        "DeliveryReport before latest successor",
        |message| match message {
            ServerMessage::DeliveryReport(report) => Some(*report),
            ServerMessage::GameData { .. } => {
                panic!("latest successor arrived before its causal DeliveryReport")
            }
            _ => None,
        },
    )
    .await;
    assert_eq!(report.per_class.latest.superseded, 1);
    assert_eq!(report.gaps.len(), 1);
    assert_eq!(report.gaps[0].from_player, sender_id);
    assert_eq!(report.gaps[0].epoch, 1);
    assert_eq!(report.gaps[0].from_seq, 1);
    assert_eq!(report.gaps[0].to_seq, 1);
    assert_eq!(report.gaps[0].reason, DeliveryGapReason::LatestSuperseded);

    let successor = next_matching_server_message_within(
        &mut recipient,
        SERVER_MESSAGE_TIMEOUT,
        "coalesced latest successor",
        |message| match message {
            ServerMessage::GameData {
                data,
                seq,
                class,
                key,
                ..
            } => Some((data, seq, class, key)),
            _ => None,
        },
    )
    .await;
    assert_eq!(successor.0, serde_json::json!({ "n": 2 }));
    assert_eq!(successor.1, Some(2));
    assert_eq!(successor.2, Some(DeliveryClass::Latest));
    assert_eq!(successor.3, Some(99));

    let class_metrics = server.metrics().delivery_metrics_by_class().latest;
    assert_eq!(class_metrics.attempted, 2);
    assert_eq!(class_metrics.delivered, 1);
    assert_eq!(class_metrics.superseded, 1);
    running_server.shutdown().await;
}

/// Read raw text frames until the next GameData frame, returning the raw JSON
/// text so field presence/absence can be asserted without deserialization
/// (mirrors `next_raw_server_message_of_type` in tests/v3_ice_pregather_e2e.rs,
/// but skips non-GameData protocol frames such as PlayerJoined chatter).
async fn next_raw_game_data(ws: &mut WsStream, context: &str) -> String {
    let deadline = tokio::time::Instant::now() + SERVER_MESSAGE_TIMEOUT;
    loop {
        let frame = tokio::time::timeout_at(deadline, ws.next())
            .await
            .unwrap_or_else(|_| panic!("{context}: timed out waiting for a GameData text frame"))
            .unwrap_or_else(|| panic!("{context}: websocket stream closed"))
            .unwrap_or_else(|err| panic!("{context}: websocket error: {err}"));
        let Message::Text(text) = frame else {
            continue;
        };
        let value: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|err| panic!("{context}: invalid JSON text frame: {err}; {text:?}"));
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("GameData") => return text.to_string(),
            Some("Error") => panic!("{context}: unexpected server error frame: {text}"),
            _ => continue,
        }
    }
}

/// (c) Two senders interleaved: each sender's stamp sequence is independent
/// and contiguous (per-(sender, room) counters, not a room-global counter).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interleaved_senders_have_independent_contiguous_seqs() {
    const PER_SENDER: u64 = 50;

    let (running_server, server) = start_test_server(test_server_config()).await;
    let addr = running_server.addr();
    let metrics = server.metrics();

    let mut sender_a = connect(addr).await;
    let mut sender_b = connect(addr).await;
    let mut recipient = connect(addr).await;
    authenticate_v3(&mut sender_a).await;
    authenticate_v3(&mut sender_b).await;
    authenticate_v3(&mut recipient).await;
    let (id_a, _) = join_room(&mut sender_a, "SEQ2SX", "SenderA").await;
    let (id_b, _) = join_room(&mut sender_b, "SEQ2SX", "SenderB").await;
    join_room(&mut recipient, "SEQ2SX", "TwoSenderRecipient").await;

    // Interleave: A then B, alternating, awaiting each write so the two
    // streams genuinely interleave at the server.
    for n in 0..PER_SENDER {
        send(
            &mut sender_a,
            &ClientMessage::GameData {
                class: None,
                key: None,
                data: serde_json::json!({ "sender": "a", "n": n }),
            },
        )
        .await;
        send(
            &mut sender_b,
            &ClientMessage::GameData {
                class: None,
                key: None,
                data: serde_json::json!({ "sender": "b", "n": n }),
            },
        )
        .await;
    }

    let frames = tokio::time::timeout(
        TEST_DEADLINE,
        collect_game_data(&mut recipient, (PER_SENDER * 2) as usize),
    )
    .await
    .expect("two-sender test exceeded its deadline");

    let mut next_expected_a = 1u64;
    let mut next_expected_b = 1u64;
    for frame in &frames {
        let expected = if frame.from_player == id_a {
            let e = next_expected_a;
            next_expected_a += 1;
            e
        } else if frame.from_player == id_b {
            let e = next_expected_b;
            next_expected_b += 1;
            e
        } else {
            panic!("GameData from unexpected sender {}", frame.from_player);
        };
        assert_eq!(
            frame.seq,
            Some(expected),
            "per-sender seq must be independent and contiguous"
        );
    }
    assert_eq!(next_expected_a, PER_SENDER + 1, "all of A's frames arrived");
    assert_eq!(next_expected_b, PER_SENDER + 1, "all of B's frames arrived");

    assert_message_conservation(&metrics).await;
    running_server.shutdown().await;
}

/// (d) The sender leaves and rejoins the room: its stamp counter RESTARTS at
/// 1 (documented reset semantics — recipients must treat a rejoin/reconnect
/// of the sender as a seq reset, correlating via PlayerLeft/PlayerJoined).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sender_leave_and_rejoin_restarts_seq_at_one() {
    const BEFORE_LEAVE: u64 = 3;

    let (running_server, server) = start_test_server(test_server_config()).await;
    let addr = running_server.addr();
    let metrics = server.metrics();

    let mut sender = connect(addr).await;
    let mut recipient = connect(addr).await;
    authenticate_v3(&mut sender).await;
    authenticate_v3(&mut recipient).await;
    let (sender_id, _) = join_room(&mut sender, "SEQRJX", "RejoinSender").await;
    join_room(&mut recipient, "SEQRJX", "RejoinRecipient").await;

    for n in 0..BEFORE_LEAVE {
        send(
            &mut sender,
            &ClientMessage::GameData {
                class: None,
                key: None,
                data: serde_json::json!({ "phase": "before", "n": n }),
            },
        )
        .await;
    }
    let before = collect_game_data(&mut recipient, BEFORE_LEAVE as usize).await;
    assert_contiguous_seqs(&before, 1, BEFORE_LEAVE, "recipient before leave");
    // First incarnation ⇒ epoch 1 on every pre-leave frame.
    for frame in &before {
        assert_eq!(
            frame.epoch,
            Some(1),
            "the first incarnation's frames all carry epoch 1"
        );
    }

    // Leave, observe the departure, rejoin the same room.
    send(&mut sender, &ClientMessage::LeaveRoom).await;
    next_matching_server_message_within(
        &mut recipient,
        SERVER_MESSAGE_TIMEOUT,
        "sender departure",
        |message| match message {
            ServerMessage::PlayerLeft { player_id, .. } if player_id == sender_id => Some(()),
            _ => None,
        },
    )
    .await;
    join_room(&mut sender, "SEQRJX", "RejoinSender").await;

    send(
        &mut sender,
        &ClientMessage::GameData {
            class: None,
            key: None,
            data: serde_json::json!({ "phase": "after" }),
        },
    )
    .await;
    let after = collect_game_data(&mut recipient, 1).await;
    assert_eq!(
        after[0].seq,
        Some(1),
        "the per-(sender, room) counter must restart at 1 after leave + rejoin"
    );
    // The epoch bumps to 2 on the new incarnation, so the seq restart is
    // SELF-DESCRIBING: the recipient sees (epoch 1, seq 3) then (epoch 2, seq 1)
    // and attributes the backwards seq jump to the epoch bump directly — it does
    // not have to correlate the separately-ordered PlayerLeft/PlayerJoined.
    assert_eq!(
        after[0].epoch,
        Some(2),
        "a leave + rejoin is a new incarnation, so the epoch advances to 2"
    );

    assert_message_conservation(&metrics).await;
    running_server.shutdown().await;
}

/// The incarnation epoch is a single monotonic per-connection counter and is
/// deliberately NOT reset when a sender leaves one room and joins another: a v3
/// recipient in the NEW room sees the sender's epoch carry forward (2, not a
/// fresh 1) with `seq` restarted. This is the design that keeps `(epoch, seq)`
/// strictly increasing per (sender, room) even across a same-room leave+rejoin
/// (a per-room reset would replay a lower epoch there); clients baseline
/// relatively, so a first-observed epoch above 1 is expected and harmless.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn epoch_carries_across_a_room_switch_not_reset_to_one() {
    let (running_server, server) = start_test_server(test_server_config()).await;
    let addr = running_server.addr();
    let metrics = server.metrics();

    let mut sender = connect(addr).await;
    authenticate_v3(&mut sender).await;

    // Room A: the sender's first incarnation ⇒ epoch 1.
    let mut recipient_a = connect(addr).await;
    authenticate_v3(&mut recipient_a).await;
    let (sender_id, _) = join_room(&mut sender, "EPRMA0", "Switcher").await;
    join_room(&mut recipient_a, "EPRMA0", "WatcherA").await;
    send(
        &mut sender,
        &ClientMessage::GameData {
            class: None,
            key: None,
            data: serde_json::json!({ "room": "a" }),
        },
    )
    .await;
    let in_a = collect_game_data(&mut recipient_a, 1).await;
    assert_eq!(
        in_a[0].epoch,
        Some(1),
        "the first room membership is epoch 1"
    );
    assert_eq!(in_a[0].seq, Some(1));

    // Leave A (observed), then join a DIFFERENT room B.
    send(&mut sender, &ClientMessage::LeaveRoom).await;
    next_matching_server_message_within(
        &mut recipient_a,
        SERVER_MESSAGE_TIMEOUT,
        "sender departure from A",
        |message| match message {
            ServerMessage::PlayerLeft { player_id, .. } if player_id == sender_id => Some(()),
            _ => None,
        },
    )
    .await;

    let mut recipient_b = connect(addr).await;
    authenticate_v3(&mut recipient_b).await;
    join_room(&mut sender, "EPRMB0", "Switcher").await;
    join_room(&mut recipient_b, "EPRMB0", "WatcherB").await;
    send(
        &mut sender,
        &ClientMessage::GameData {
            class: None,
            key: None,
            data: serde_json::json!({ "room": "b" }),
        },
    )
    .await;
    let in_b = collect_game_data(&mut recipient_b, 1).await;
    // Carried forward + incremented (NOT reset to 1); seq restarted within it.
    assert_eq!(
        in_b[0].epoch,
        Some(2),
        "epoch carries across the room switch (monotonic per connection), not reset to 1"
    );
    assert_eq!(
        in_b[0].seq,
        Some(1),
        "seq restarts within the new room incarnation"
    );

    assert_message_conservation(&metrics).await;
    running_server.shutdown().await;
}

/// (e) The end-to-end loss-detection story #131's reporter lacked: a slow
/// consumer is evicted mid-burst, reconnects with its reconnection token, and
/// can OBSERVE the loss as a seq gap between its last-seen stamp and the next
/// received stamp — no manual sent-vs-received bookkeeping required.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn evicted_recipient_observes_seq_gap_after_reconnect() {
    /// Warm-up frames delivered to the victim before it stalls.
    const WARMUP: u64 = 5;
    /// Flood size: with 16 KiB padding this cannot hide inside kernel socket
    /// buffers, so the victim's queue must overflow into eviction.
    const FLOOD: u64 = 2_000;
    const FLOOD_PADDING_BYTES: usize = 16 * 1_024;

    let mut server_config = test_server_config();
    server_config.websocket_config.send_queue_capacity = 8;
    server_config.websocket_config.slow_consumer_timeout_ms = 300;
    let (running_server, server) = start_test_server(server_config).await;
    let addr = running_server.addr();
    let metrics = server.metrics();

    let mut sender = connect(addr).await;
    let mut healthy = connect(addr).await;
    let mut victim = connect(addr).await;
    authenticate_v3(&mut sender).await;
    authenticate_v3(&mut healthy).await;
    authenticate_v3(&mut victim).await;
    let (sender_id, _) = join_room(&mut sender, "SEQGAP", "GapSender").await;
    join_room(&mut healthy, "SEQGAP", "GapHealthy").await;
    let (victim_id, room_id) = join_room(&mut victim, "SEQGAP", "GapVictim").await;

    // Snapshot the victim's room-membership record BEFORE the eviction so a
    // reconnection token can be minted afterwards (helper pattern from
    // tests/v3_ice_pregather_e2e.rs::register_reconnect_token).
    let victim_info = server
        .database()
        .get_room_by_id(&room_id)
        .await
        .expect("room lookup")
        .expect("room exists")
        .players
        .get(&victim_id)
        .cloned()
        .expect("victim is in the room");

    // Warm-up: the victim establishes its last-seen seq baseline.
    for n in 0..WARMUP {
        send(
            &mut sender,
            &ClientMessage::GameData {
                class: None,
                key: None,
                data: serde_json::json!({ "phase": "warmup", "n": n }),
            },
        )
        .await;
    }
    let warmup_frames = collect_game_data(&mut victim, WARMUP as usize).await;
    assert_contiguous_seqs(&warmup_frames, 1, WARMUP, "victim warm-up");
    let mut last_seen = WARMUP;

    // The victim now goes silent (never polls its socket) while the sender
    // floods; the healthy recipient drains everything and observes the
    // victim's eviction via the normal disconnect flow.
    let writer = tokio::spawn(async move {
        let padding = "x".repeat(FLOOD_PADDING_BYTES);
        for n in 0..FLOOD {
            let message = ClientMessage::GameData {
                class: None,
                key: None,
                data: serde_json::json!({ "phase": "flood", "n": n, "padding": padding.as_str() }),
            };
            let json = serde_json::to_string(&message).expect("serialize GameData");
            sender
                .send(Message::Text(json.into()))
                .await
                .expect("send GameData while the victim is stalled");
        }
        sender
    });
    let healthy_reader = tokio::spawn(async move {
        let mut game_data_seen = 0u64;
        let mut victim_left = false;
        while game_data_seen < WARMUP + FLOOD || !victim_left {
            let Some(frame) = healthy.next().await else {
                panic!("healthy recipient closed early (after {game_data_seen} frames)");
            };
            let frame = frame.expect("healthy recipient websocket error");
            let Message::Text(text) = frame else { continue };
            let message: ServerMessage = serde_json::from_str(&text).expect("valid ServerMessage");
            match message {
                ServerMessage::GameData { .. } => game_data_seen += 1,
                ServerMessage::PlayerLeft { player_id, .. } if player_id == victim_id => {
                    victim_left = true;
                }
                ServerMessage::Error {
                    message,
                    error_code,
                } => panic!("healthy recipient got server error: {message} ({error_code:?})"),
                _ => continue,
            }
        }
        healthy
    });

    let (writer_result, healthy_result) = tokio::time::timeout(TEST_DEADLINE, async {
        tokio::join!(writer, healthy_reader)
    })
    .await
    .expect("gap test exceeded its deadline (eviction never happened?)");
    let mut sender = writer_result.expect("flood writer task panicked");
    let _healthy = healthy_result.expect("healthy reader task panicked");

    assert!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed)
            >= 1,
        "the stalled victim must be evicted loudly as a slow consumer"
    );

    // The victim resumes reading: it drains whatever was buffered before the
    // close (updating last_seen) and then observes termination.
    let victim_drain = tokio::time::timeout(tokio::time::Duration::from_secs(30), async move {
        while let Some(frame) = victim.next().await {
            match frame {
                Ok(Message::Text(text)) => {
                    if let Ok(ServerMessage::GameData { seq, .. }) =
                        serde_json::from_str::<ServerMessage>(&text)
                    {
                        let seq = seq.expect("v3 victim frames carry seq");
                        assert_eq!(
                            seq,
                            last_seen + 1,
                            "frames delivered before the close stay contiguous"
                        );
                        last_seen = seq;
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => continue,
            }
        }
        last_seen
    })
    .await
    .expect("victim never observed termination after resuming reads");
    let last_seen = victim_drain;
    assert!(
        last_seen < WARMUP + FLOOD,
        "the eviction must have cost the victim at least one stamped frame \
         (last_seen={last_seen})"
    );

    // Mint a reconnection token (server-side helper pattern) and reconnect on
    // a fresh v3 socket under the same player id.
    let token = server
        .reconnection_manager()
        .expect("reconnection enabled in test config")
        // last_epoch = 1: the victim joined once (its own incarnation epoch),
        // though this test asserts the SENDER's stream, not the victim's epoch.
        .register_disconnection(victim_id, room_id, false, Some(victim_info), 1)
        .await;

    let mut reconnected = connect(addr).await;
    authenticate_v3(&mut reconnected).await;
    send(
        &mut reconnected,
        &ClientMessage::Reconnect {
            player_id: victim_id,
            room_id,
            auth_token: token,
        },
    )
    .await;
    let reconnect_payload = next_matching_server_message_within(
        &mut reconnected,
        SERVER_MESSAGE_TIMEOUT,
        "reconnect response",
        |message| match message {
            ServerMessage::Reconnected(payload) => Some(payload),
            ServerMessage::ReconnectionFailed { reason, error_code } => {
                panic!("reconnect failed: {reason} ({error_code:?})")
            }
            _ => None,
        },
    )
    .await;
    let sender_watermark = reconnect_payload
        .sender_watermarks
        .iter()
        .find(|watermark| watermark.player_id == sender_id)
        .unwrap_or_else(|| {
            panic!(
                "reconnect must baseline the still-connected sender: {:?}",
                reconnect_payload.sender_watermarks
            )
        });
    assert_eq!(
        sender_watermark.epoch, 1,
        "the sender never rejoined, so its epoch stays at the original incarnation"
    );
    assert_eq!(
        sender_watermark.seq,
        WARMUP + FLOOD,
        "the reconnect watermark reports the sender's tail before the marker frame"
    );

    // One marker frame from the (never-disconnected) sender: its counter has
    // kept counting, so the reconnected victim observes the gap.
    send(
        &mut sender,
        &ClientMessage::GameData {
            class: None,
            key: None,
            data: serde_json::json!({ "phase": "marker" }),
        },
    )
    .await;
    let marker = collect_game_data(&mut reconnected, 1).await;
    let marker_seq = marker[0].seq.expect("marker frame carries seq");
    assert_eq!(
        marker_seq,
        sender_watermark.seq + 1,
        "the sender's counter continues across the victim's eviction"
    );
    let gap = marker_seq - last_seen - 1;
    assert!(
        gap > 0,
        "the reconnected victim must observe its loss as a seq gap \
         (last_seen={last_seen}, next={marker_seq})"
    );
    // The observed gap is exactly the number of stamped frames the server
    // abandoned for (or never enqueued to) the evicted connection; the
    // conservation law below ties those to the loud drop counters.
    assert!(
        metrics.websocket_messages_dropped.load(Ordering::Relaxed) >= 1,
        "the gap must correspond to loudly counted drops"
    );

    assert_message_conservation(&metrics).await;
    running_server.shutdown().await;
}

/// Read text frames until the next server message of `type_name`, returning its
/// parsed JSON `Value` so field presence/absence can be asserted directly (a v3
/// field omitted for a pre-v3 recipient is ABSENT from the object, so
/// `value["data"][..].get("epoch")` is `None`, not a JSON null).
async fn next_message_value_of_type(
    ws: &mut WsStream,
    type_name: &str,
    context: &str,
) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + SERVER_MESSAGE_TIMEOUT;
    loop {
        let frame = tokio::time::timeout_at(deadline, ws.next())
            .await
            .unwrap_or_else(|_| panic!("{context}: timed out waiting for a {type_name} frame"))
            .unwrap_or_else(|| panic!("{context}: websocket stream closed"))
            .unwrap_or_else(|err| panic!("{context}: websocket error: {err}"));
        let Message::Text(text) = frame else {
            continue;
        };
        let value: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|err| panic!("{context}: invalid JSON text frame: {err}; {text:?}"));
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some(found) if found == type_name => return value,
            Some("Error") => panic!("{context}: unexpected server error frame: {text}"),
            _ => continue,
        }
    }
}

/// (f) Room snapshots carry the incarnation epoch for v3 recipients and OMIT it
/// (byte-absent, not null) for pre-v3 recipients — both directions:
/// `RoomJoined.current_players[].epoch` (seen by the joiner) and
/// `PlayerJoined.player.epoch` (seen by existing members).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v3_room_snapshots_carry_epoch_pre_v3_omit_it() {
    let (running_server, server) = start_test_server(test_server_config()).await;
    let addr = running_server.addr();
    let metrics = server.metrics();

    // Anchor A (v3) establishes the room; its first incarnation ⇒ epoch 1.
    let mut anchor = connect(addr).await;
    authenticate_v3(&mut anchor).await;
    join_room(&mut anchor, "SNAPEP", "SnapAnchor").await;

    // A v2 joiner: its RoomJoined snapshot must OMIT epoch on every member.
    let mut v2_joiner = connect(addr).await;
    authenticate_v2(&mut v2_joiner).await;
    send(&mut v2_joiner, &join_message("SNAPEP", "SnapV2")).await;
    let v2_room = next_message_value_of_type(&mut v2_joiner, "RoomJoined", "v2 RoomJoined").await;
    let v2_players = v2_room["data"]["current_players"]
        .as_array()
        .expect("v2 RoomJoined current_players array");
    assert!(
        !v2_players.is_empty(),
        "the snapshot lists existing members"
    );
    for player in v2_players {
        assert!(
            player.get("epoch").is_none(),
            "a v2 joiner's RoomJoined must omit epoch on every member: {v2_room}"
        );
    }

    // A (v3) sees the v2 joiner arrive; the joiner's epoch (1) IS present on A's
    // v3 wire (the server stamps it regardless of the joiner's own version).
    let a_sees_v2 =
        next_message_value_of_type(&mut anchor, "PlayerJoined", "A sees the v2 join").await;
    assert_eq!(
        a_sees_v2["data"]["player"]["epoch"].as_u64(),
        Some(1),
        "a v3 member sees a joiner's epoch on PlayerJoined: {a_sees_v2}"
    );

    // A v3 joiner: its RoomJoined snapshot CARRIES each member's epoch (all 1).
    let mut v3_joiner = connect(addr).await;
    authenticate_v3(&mut v3_joiner).await;
    send(&mut v3_joiner, &join_message("SNAPEP", "SnapV3")).await;
    let v3_room = next_message_value_of_type(&mut v3_joiner, "RoomJoined", "v3 RoomJoined").await;
    let v3_players = v3_room["data"]["current_players"]
        .as_array()
        .expect("v3 RoomJoined current_players array");
    assert!(
        !v3_players.is_empty(),
        "the snapshot lists existing members"
    );
    for player in v3_players {
        assert_eq!(
            player["epoch"].as_u64(),
            Some(1),
            "a v3 joiner's RoomJoined carries each member's epoch: {v3_room}"
        );
    }

    // The v2 member now sees the v3 joiner arrive; its PlayerJoined omits epoch.
    let v2_sees_v3 =
        next_message_value_of_type(&mut v2_joiner, "PlayerJoined", "v2 sees the v3 join").await;
    assert!(
        v2_sees_v3["data"]["player"].get("epoch").is_none(),
        "a v2 member's PlayerJoined must omit epoch: {v2_sees_v3}"
    );

    // SpectatorJoined is another full sender snapshot and obeys the same
    // projection: live epochs for v3, byte-absent for v2.
    let mut v2_spectator = connect(addr).await;
    authenticate_v2(&mut v2_spectator).await;
    send(
        &mut v2_spectator,
        &ClientMessage::JoinAsSpectator {
            game_name: "v3_seq_game".to_string(),
            room_code: "SNAPEP".to_string(),
            spectator_name: "SnapSpecV2".to_string(),
        },
    )
    .await;
    let v2_spectator_joined =
        next_message_value_of_type(&mut v2_spectator, "SpectatorJoined", "v2 SpectatorJoined")
            .await;
    for player in v2_spectator_joined["data"]["current_players"]
        .as_array()
        .expect("v2 spectator current_players")
    {
        assert!(
            player.get("epoch").is_none(),
            "a v2 spectator snapshot must omit epoch: {v2_spectator_joined}"
        );
    }

    let mut v3_spectator = connect(addr).await;
    authenticate_v3(&mut v3_spectator).await;
    send(
        &mut v3_spectator,
        &ClientMessage::JoinAsSpectator {
            game_name: "v3_seq_game".to_string(),
            room_code: "SNAPEP".to_string(),
            spectator_name: "SnapSpecV3".to_string(),
        },
    )
    .await;
    let v3_spectator_joined =
        next_message_value_of_type(&mut v3_spectator, "SpectatorJoined", "v3 SpectatorJoined")
            .await;
    for player in v3_spectator_joined["data"]["current_players"]
        .as_array()
        .expect("v3 spectator current_players")
    {
        assert!(
            player.get("epoch").is_some(),
            "a v3 spectator snapshot must carry epoch: {v3_spectator_joined}"
        );
    }

    assert_message_conservation(&metrics).await;
    running_server.shutdown().await;
}

/// (g) A reconnecting SENDER's incarnation epoch BUMPS and survives across the
/// reconnect (the pre-disconnect epoch is captured into the reconnection record
/// and resumed at `+1`): the recipient learns the new epoch via
/// `PlayerReconnected`, AND the reconnected sender's `GameData` carries the same
/// bumped epoch with `seq` restarted at 1 — so the `(epoch, seq)` stream
/// strictly increases for a recipient that never left, making the reset
/// self-describing without correlating the control message.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnecting_sender_bumps_epoch_and_stamps_it_on_game_data() {
    let (running_server, server) = start_test_server(test_server_config()).await;
    let addr = running_server.addr();
    let metrics = server.metrics();

    let mut sender = connect(addr).await;
    let mut recipient = connect(addr).await;
    authenticate_v3(&mut sender).await;
    authenticate_v3(&mut recipient).await;

    // Join the sender directly so we can capture its pre-issued reconnection
    // token from RoomJoined (v3 clients get one at join).
    send(&mut sender, &join_message("RCEPCH", "RcSender")).await;
    let (sender_id, room_id, token) = next_matching_server_message_within(
        &mut sender,
        SERVER_MESSAGE_TIMEOUT,
        "sender RoomJoined",
        |message| match message {
            ServerMessage::RoomJoined(payload) => Some((
                payload.player_id,
                payload.room_id,
                payload.reconnection_token.clone(),
            )),
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                panic!("sender room join failed: {reason} ({error_code:?})")
            }
            _ => None,
        },
    )
    .await;
    let token = token.expect("a v3 join issues a reconnection token");
    join_room(&mut recipient, "RCEPCH", "RcRecipient").await;

    // First incarnation: epoch 1, seq 1.
    send(
        &mut sender,
        &ClientMessage::GameData {
            class: None,
            key: None,
            data: serde_json::json!({ "phase": "before" }),
        },
    )
    .await;
    let before = collect_game_data(&mut recipient, 1).await;
    assert_eq!(before[0].seq, Some(1));
    assert_eq!(before[0].epoch, Some(1), "the first incarnation is epoch 1");

    // Disconnect the sender through the real teardown path. Call the
    // server-side unregister DIRECTLY (socket still open) so it runs exactly
    // once and deterministically: it captures epoch 1 from the still-live
    // connection into the reconnection record, reuses the pre-issued token, and
    // removes the connection before we reconnect (no PlayerAlreadyConnected
    // race). Dropping the socket first would instead let the close-driven
    // unregister consume the pre-issued token and this call mint a fresh one,
    // invalidating the token we captured above.
    server.disconnect_client(&sender_id).await;
    drop(sender);

    // Reconnect on a fresh v3 socket under the original id, with the pre-issued
    // token.
    let mut reconnected = connect(addr).await;
    authenticate_v3(&mut reconnected).await;
    send(
        &mut reconnected,
        &ClientMessage::Reconnect {
            player_id: sender_id,
            room_id,
            auth_token: token,
        },
    )
    .await;
    next_matching_server_message_within(
        &mut reconnected,
        SERVER_MESSAGE_TIMEOUT,
        "reconnect response",
        |message| match message {
            ServerMessage::Reconnected(payload) => Some(payload),
            ServerMessage::ReconnectionFailed { reason, error_code } => {
                panic!("reconnect failed: {reason} ({error_code:?})")
            }
            _ => None,
        },
    )
    .await;

    // The recipient (never left) learns of the reconnection with the sender's
    // NEW epoch (2) — the pre-disconnect epoch 1 survived and bumped.
    let notice = next_message_value_of_type(
        &mut recipient,
        "PlayerReconnected",
        "recipient sees the reconnection",
    )
    .await;
    assert_eq!(
        notice["data"]["epoch"].as_u64(),
        Some(2),
        "PlayerReconnected advertises the reconnector's bumped epoch: {notice}"
    );

    // The reconnected sender's GameData carries the same new epoch, seq restarted
    // — (epoch 1, seq 1) then (epoch 2, seq 1): strictly increasing, no ambiguity.
    send(
        &mut reconnected,
        &ClientMessage::GameData {
            class: None,
            key: None,
            data: serde_json::json!({ "phase": "after" }),
        },
    )
    .await;
    let after = collect_game_data(&mut recipient, 1).await;
    assert_eq!(after[0].seq, Some(1), "seq restarts within the new epoch");
    assert_eq!(
        after[0].epoch,
        Some(2),
        "the reconnected sender stamps its GameData with the bumped epoch"
    );

    assert_message_conservation(&metrics).await;
    running_server.shutdown().await;
}
