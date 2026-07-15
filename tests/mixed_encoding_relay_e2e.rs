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
    ClientMessage, GameDataEncoding, PlayerId, ServerMessage, V3BinaryGameDataFrame,
};
use signal_fish_server::websocket::create_router;
use tokio_tungstenite::tungstenite::Message;

use test_helpers::{create_test_server_with_config, test_server_config, RunningTestServer};
use websocket_test_helpers::chaos_proxy::{ChaosProxy, Direction};
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
#[ignore = "nightly-only (verification-nightly.yml): throttled unsupported-format amplification"]
async fn unsupported_message_pack_fallback_does_not_flap_weaker_recipient() {
    const BURST: u64 = 5_000;
    const THROTTLE_BYTES_PER_SEC: u64 = 32 * 1_024;
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

    let compatible_proxy = ChaosProxy::spawn(running_server.addr()).await;
    let fallback_proxy = ChaosProxy::spawn(running_server.addr()).await;
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

    let mut sender = players.remove(0).ws;
    let compatible = players.remove(0).ws;
    let fallback = players.remove(0).ws;

    let compatible_auditor = Arc::clone(&auditor);
    let compatible_reader = tokio::spawn(async move {
        let (_, mut stream) = compatible.split();
        let deadline = tokio::time::Instant::now() + EXPERIMENT_DEADLINE;
        let mut delivered = 0u64;
        let mut player_left = 0u64;
        while delivered < BURST {
            let frame = tokio::time::timeout_at(deadline, stream.next())
                .await
                .unwrap_or_else(|_| {
                    panic!("compatible recipient timed out after {delivered}/{BURST} binary frames")
                })
                .unwrap_or_else(|| {
                    panic!("compatible recipient closed after {delivered}/{BURST} binary frames")
                })
                .unwrap_or_else(|error| panic!("compatible recipient read failed: {error}"));
            match frame {
                Message::Binary(bytes) => {
                    let relayed = compatible_auditor.record_binary_frame("P1", &bytes);
                    assert_eq!(relayed.encoding, GameDataEncoding::MessagePack);
                    assert_eq!(relayed.payload.as_slice(), [0xc1]);
                    delivered += 1;
                }
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Text(text) => match compatible_auditor.record_text_frame("P1", &text) {
                    ServerMessage::PlayerLeft { .. } => player_left += 1,
                    other => panic!("compatible recipient expected binary delivery, got {other:?}"),
                },
                Message::Close(reason) => {
                    panic!("compatible recipient was evicted after {delivered}/{BURST}: {reason:?}")
                }
                Message::Frame(frame) => {
                    panic!("compatible recipient observed raw frame: {frame:?}")
                }
            }
        }
        (stream, player_left)
    });

    let fallback_auditor = Arc::clone(&auditor);
    let fallback_reader = tokio::spawn(async move {
        let (_, mut stream) = fallback.split();
        let deadline = tokio::time::Instant::now() + EXPERIMENT_DEADLINE;
        let mut reports = 0u64;
        let mut errors = 0u64;
        while reports < BURST {
            let frame = tokio::time::timeout_at(deadline, stream.next())
                .await
                .unwrap_or_else(|_| {
                    panic!("fallback recipient timed out after {reports} reports/{errors} errors")
                })
                .unwrap_or_else(|| {
                    panic!("fallback recipient closed without a semantic close frame")
                })
                .unwrap_or_else(|error| panic!("fallback recipient read failed: {error}"));
            match frame {
                Message::Text(text) => match fallback_auditor.record_text_frame("P2", &text) {
                    ServerMessage::DeliveryReport(_) => reports += 1,
                    ServerMessage::Error { error_code, .. } => {
                        assert_eq!(
                            error_code,
                            Some(
                                signal_fish_server::protocol::ErrorCode::UnsupportedGameDataFormat
                            )
                        );
                        errors += 1;
                    }
                    other => panic!("fallback recipient observed unexpected message: {other:?}"),
                },
                Message::Close(reason) => {
                    let reason = reason.expect("fallback close must carry code and reason");
                    let code = u16::from(reason.code);
                    fallback_auditor.record_close("P2", code, &reason.reason);
                    return (
                        stream,
                        reports,
                        errors,
                        Some((code, reason.reason.to_string())),
                    );
                }
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Binary(bytes) => {
                    panic!("JSON fallback recipient received unconvertible binary bytes: {bytes:?}")
                }
                Message::Frame(frame) => {
                    panic!("fallback recipient observed raw frame: {frame:?}")
                }
            }
        }
        (stream, reports, errors, None)
    });

    for _ in 0..BURST {
        // 0xc1 is MessagePack's reserved/invalid marker. The server correctly
        // treats binary game data as opaque for same-format peers, while a JSON
        // recipient cannot convert it and needs explicit gap accountability.
        sender
            .send(Message::Binary(vec![0xc1].into()))
            .await
            .expect("send unconvertible MessagePack payload");
    }

    let (fallback_stream, reports, errors, close) = fallback_reader
        .await
        .expect("fallback recipient task panicked");
    compatible_proxy.throttle(Direction::ServerToClient, None);
    fallback_proxy.throttle(Direction::ServerToClient, None);

    let (compatible_stream, compatible_player_left) = compatible_reader
        .await
        .expect("compatible recipient task panicked");

    // This is the pre-registered falsification oracle. A RED result here means
    // the per-message advisory doubles the fallback stream enough to evict a
    // recipient that survives the same throttle on compact binary delivery.
    assert!(
        close.is_none(),
        "unsupported-format amplification evicted only the JSON fallback recipient after \
         {reports} reports/{errors} rate-limited errors ({close:?})"
    );
    assert_eq!(
        reports, BURST,
        "every omitted sequence needs an exact report"
    );
    assert!(
        errors > 0,
        "the first supplemental advisory must be visible"
    );
    assert!(
        errors < reports / 100,
        "advisory rate limiting was ineffective: {errors} errors for {reports} reports"
    );
    assert_eq!(
        compatible_player_left, 0,
        "compatible recipient observed fallback-recipient eviction"
    );
    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        0
    );
    auditor.assert_conformance(&metrics, &[]).await;
    eprintln!(
        "mixed-encoding H14: exact_reports={reports} advisory_errors={errors} \
         compatible_deliveries={BURST} slow_consumer_evictions=0"
    );

    drop(sender);
    drop(compatible_stream);
    drop(fallback_stream);
    drop(compatible_proxy);
    drop(fallback_proxy);
    running_server.shutdown().await;
}
