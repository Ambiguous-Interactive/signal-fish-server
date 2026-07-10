mod websocket_test_helpers;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use chrono::Utc;
use futures_util::{Stream, StreamExt};
use signal_fish_server::protocol::{
    ErrorCode, GameDataEncoding, LobbyState, PlayerId, PlayerInfo, ReconnectedPayload,
    ReplayStatus, RoomJoinedPayload, SenderWatermark, ServerMessage,
};
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use websocket_test_helpers::conformance::{
    ConformanceAuditor, ReceiverDisconnectCause, RecordedBinaryGameData,
};
use websocket_test_helpers::{
    deadline_after, expect_no_server_message_within,
    maybe_next_matching_server_message_with_skipped_until, next_matching_server_message_within,
};

struct RepeatingTextFrames {
    text: String,
    frames_emitted: usize,
    yield_next_poll: bool,
}

impl RepeatingTextFrames {
    fn new(message: ServerMessage) -> Self {
        Self {
            text: serde_json::to_string(&message).expect("ServerMessage serializes"),
            frames_emitted: 0,
            yield_next_poll: false,
        }
    }
}

impl Stream for RepeatingTextFrames {
    type Item = Result<Message, WebSocketError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.yield_next_poll {
            self.yield_next_poll = false;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }

        self.yield_next_poll = true;
        self.frames_emitted += 1;
        Poll::Ready(Some(Ok(Message::Text(self.text.clone().into()))))
    }
}

#[tokio::test]
async fn matching_server_message_skips_noise_until_match() {
    let frames = vec![
        text_frame(ServerMessage::Pong),
        text_frame(ServerMessage::Error {
            message: "target".to_string(),
            error_code: None,
        }),
    ];
    let mut stream = futures_util::stream::iter(frames.into_iter().map(Ok));

    let message = next_matching_server_message_within(
        &mut stream,
        Duration::from_secs(1),
        "test match",
        |message| match message {
            ServerMessage::Error { message, .. } => Some(message),
            _ => None,
        },
    )
    .await;

    assert_eq!(message, "target");
}

#[tokio::test]
async fn optional_matching_server_message_uses_absolute_deadline_for_noisy_stream() {
    let mut stream = RepeatingTextFrames::new(ServerMessage::Pong);
    // Wide enough that a task briefly starved on an oversubscribed full-suite
    // runner still reads several frames before the deadline (a 10ms window
    // flaked when the task was first scheduled after the deadline had already
    // passed); the outer timeout only guards against an unbounded loop.
    let deadline = deadline_after(Duration::from_millis(250));

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        maybe_next_matching_server_message_with_skipped_until(
            &mut stream,
            deadline,
            "test deadline",
            |message| match message {
                ServerMessage::SessionPlan(_) => Some(()),
                _ => None,
            },
        ),
    )
    .await
    .expect("absolute deadline should finish despite continuous non-matching frames");

    let (value, skipped) = result;
    assert!(value.is_none());
    assert!(
        skipped.contains("Pong"),
        "skipped diagnostics should include non-matching message types, got {skipped:?}"
    );
    assert!(
        stream.frames_emitted > 1,
        "test must exercise skipped frames, got {}",
        stream.frames_emitted
    );
}

#[tokio::test]
async fn optional_matching_server_message_reports_skipped_non_text_frames() {
    let frames = vec![
        Ok(Message::Ping(Vec::new().into())),
        Ok(Message::Binary(Vec::new().into())),
    ];
    let mut stream = futures_util::stream::iter(frames).chain(futures_util::stream::pending::<
        Result<Message, WebSocketError>,
    >());

    let (value, skipped) = maybe_next_matching_server_message_with_skipped_until(
        &mut stream,
        // Wide enough to survive scheduler starvation under a parallel full
        // suite (the frames are ready instantly; the deadline only ends the
        // wait on the pending tail).
        deadline_after(Duration::from_millis(250)),
        "test non-text diagnostics",
        |message| match message {
            ServerMessage::SessionPlan(_) => Some(()),
            _ => None,
        },
    )
    .await;

    assert!(value.is_none());
    assert!(
        skipped.contains("PingFrame") && skipped.contains("BinaryFrame"),
        "skipped diagnostics should include non-text frames, got {skipped:?}"
    );
}

#[tokio::test]
async fn no_server_message_skips_non_text_frames_until_timeout() {
    let frames = vec![
        Ok(Message::Ping(Vec::new().into())),
        Ok(Message::Binary(Vec::new().into())),
    ];
    let mut stream = futures_util::stream::iter(frames).chain(futures_util::stream::pending::<
        Result<Message, WebSocketError>,
    >());

    expect_no_server_message_within(
        &mut stream,
        // Wide enough that the non-text frames are actually read (exercising
        // the skip path) even when the task starts late under suite load.
        Duration::from_millis(100),
        "test no server message",
    )
    .await;
}

#[tokio::test]
#[should_panic(expected = "expected no ServerMessage, got Pong")]
async fn no_server_message_panics_on_text_server_message() {
    let frames = vec![Ok(text_frame(ServerMessage::Pong))];
    let mut stream = futures_util::stream::iter(frames);

    expect_no_server_message_within(
        &mut stream,
        Duration::from_secs(1),
        "test unexpected message",
    )
    .await;
}

fn text_frame(message: ServerMessage) -> Message {
    Message::Text(
        serde_json::to_string(&message)
            .expect("ServerMessage serializes")
            .into(),
    )
}

fn id(value: u128) -> PlayerId {
    PlayerId::from_u128(value)
}

fn player(player_id: PlayerId, epoch: u32) -> PlayerInfo {
    PlayerInfo {
        id: player_id,
        name: format!("player-{player_id}"),
        is_authority: false,
        is_ready: false,
        connected_at: Utc::now(),
        connection_info: None,
        epoch: Some(epoch),
        region_id: String::new(),
    }
}

fn player_joined(player_id: PlayerId, epoch: u32) -> ServerMessage {
    ServerMessage::PlayerJoined {
        player: player(player_id, epoch),
    }
}

fn room_joined(player_id: PlayerId, epoch: u32) -> ServerMessage {
    ServerMessage::RoomJoined(Box::new(RoomJoinedPayload {
        room_id: id(98),
        room_code: "AUDIT2".to_string(),
        player_id: id(100),
        game_name: "audit".to_string(),
        max_players: 4,
        supports_authority: false,
        current_players: vec![player(player_id, epoch)],
        is_authority: false,
        lobby_state: LobbyState::Lobby,
        ready_players: Vec::new(),
        relay_type: "WebSocket".to_string(),
        current_spectators: Vec::new(),
        ice_servers: Vec::new(),
        reconnection_token: None,
    }))
}

fn game_data(player_id: PlayerId, seq: Option<u64>, epoch: Option<u32>) -> ServerMessage {
    ServerMessage::GameData {
        from_player: player_id,
        data: serde_json::json!({}),
        seq,
        epoch,
    }
}

fn format_error() -> ServerMessage {
    ServerMessage::Error {
        message: "undeliverable".to_string(),
        error_code: Some(ErrorCode::UnsupportedGameDataFormat),
    }
}

fn reconnected(player_id: PlayerId, epoch: u32, seq: u64) -> ServerMessage {
    ServerMessage::Reconnected(Box::new(ReconnectedPayload {
        room_id: id(99),
        room_code: "AUDIT1".to_string(),
        player_id: id(100),
        game_name: "audit".to_string(),
        max_players: 4,
        supports_authority: false,
        current_players: vec![player(player_id, epoch)],
        is_authority: false,
        lobby_state: LobbyState::Lobby,
        ready_players: Vec::new(),
        relay_type: "WebSocket".to_string(),
        current_spectators: Vec::new(),
        ice_servers: Vec::new(),
        missed_events: Vec::new(),
        replay: Some(ReplayStatus::Truncated),
        sender_watermarks: vec![SenderWatermark {
            player_id,
            epoch,
            seq,
        }],
        reconnection_token: None,
    }))
}

fn panics(action: impl FnOnce()) -> bool {
    catch_unwind(AssertUnwindSafe(action)).is_err()
}

#[test]
fn conformance_stamps_and_prior_gap_causes_are_data_driven() {
    #[derive(Clone, Copy)]
    enum Step {
        Data(Option<u64>, Option<u32>),
        FormatError,
    }

    let cases: &[(&str, &[Step], bool)] = &[
        (
            "contiguous",
            &[Step::Data(Some(1), Some(1)), Step::Data(Some(2), Some(1))],
            false,
        ),
        ("v2 unstamped", &[Step::Data(None, None)], false),
        ("half stamp", &[Step::Data(Some(1), None)], true),
        ("zero seq", &[Step::Data(Some(0), Some(1))], true),
        (
            "duplicate",
            &[Step::Data(Some(1), Some(1)), Step::Data(Some(1), Some(1))],
            true,
        ),
        (
            "unexplained gap",
            &[Step::Data(Some(1), Some(1)), Step::Data(Some(3), Some(1))],
            true,
        ),
        (
            "prior exact cause",
            &[
                Step::Data(Some(1), Some(1)),
                Step::FormatError,
                Step::Data(Some(3), Some(1)),
            ],
            false,
        ),
        (
            "late cause",
            &[
                Step::Data(Some(1), Some(1)),
                Step::Data(Some(3), Some(1)),
                Step::FormatError,
            ],
            true,
        ),
    ];

    for (name, steps, should_panic) in cases {
        let auditor = ConformanceAuditor::new();
        let sender = id(1);
        auditor.record_message("receiver", &player_joined(sender, 1));
        let failed = panics(|| {
            for step in *steps {
                match step {
                    Step::Data(seq, epoch) => {
                        auditor.record_message("receiver", &game_data(sender, *seq, *epoch));
                    }
                    Step::FormatError => auditor.record_message("receiver", &format_error()),
                }
            }
        });
        assert_eq!(failed, *should_panic, "case {name}");
    }
}

#[test]
fn conformance_lifecycle_and_late_join_baselines_are_data_driven() {
    let late_join_cases: &[(&str, &[u64], bool)] = &[
        ("new sender", &[1, 2], false),
        ("existing sender", &[41, 42], false),
        ("gap after baseline", &[41, 43], true),
    ];
    for (name, seqs, should_panic) in late_join_cases {
        let auditor = ConformanceAuditor::new();
        let sender = id(2);
        auditor.record_message("receiver", &room_joined(sender, 7));
        let failed = panics(|| {
            for seq in *seqs {
                auditor.record_message("receiver", &game_data(sender, Some(*seq), Some(7)));
            }
        });
        assert_eq!(failed, *should_panic, "case {name}");
    }

    let sender = id(3);
    let departed = ConformanceAuditor::new();
    departed.record_message("receiver", &player_joined(sender, 1));
    departed.record_message("receiver", &game_data(sender, Some(1), Some(1)));
    departed.record_message("receiver", &ServerMessage::PlayerLeft { player_id: sender });
    assert!(panics(|| departed.record_message(
        "receiver",
        &game_data(sender, Some(2), Some(1))
    )));

    let rejoined = ConformanceAuditor::new();
    rejoined.record_message("receiver", &player_joined(sender, 1));
    rejoined.record_message("receiver", &game_data(sender, Some(1), Some(1)));
    rejoined.record_message("receiver", &ServerMessage::PlayerLeft { player_id: sender });
    rejoined.record_message("receiver", &player_joined(sender, 2));
    rejoined.record_message("receiver", &game_data(sender, Some(1), Some(2)));
}

#[test]
fn conformance_reconnect_watermark_rebaselines_before_data() {
    let sender = id(4);
    let auditor = ConformanceAuditor::new();
    auditor.record_message("receiver", &player_joined(sender, 1));
    auditor.record_message("receiver", &game_data(sender, Some(1), Some(1)));
    auditor.record_message("receiver", &reconnected(sender, 1, 5));
    auditor.record_message("receiver", &game_data(sender, Some(6), Some(1)));

    let without_baseline = ConformanceAuditor::new();
    without_baseline.record_message("receiver", &player_joined(sender, 1));
    without_baseline.record_message("receiver", &game_data(sender, Some(1), Some(1)));
    assert!(panics(|| without_baseline.record_message(
        "receiver",
        &game_data(sender, Some(6), Some(1))
    )));
}

#[test]
fn conformance_relay_stats_are_stable_and_monotone() {
    type Sample = (u64, u64, u64, u64);
    let cases: &[(&str, &[Sample], bool)] = &[
        ("valid", &[(1_000, 2, 0, 0), (1_000, 4, 1, 2)], false),
        ("zero interval", &[(0, 2, 0, 0)], true),
        (
            "interval changed",
            &[(1_000, 2, 0, 0), (2_000, 4, 0, 0)],
            true,
        ),
        (
            "counter regressed",
            &[(1_000, 2, 1, 1), (1_000, 1, 1, 1)],
            true,
        ),
    ];
    for (name, samples, should_panic) in cases {
        let auditor = ConformanceAuditor::new();
        let failed = panics(|| {
            for (interval_ms, sent_to_you, dropped_for_you, backpressure_events) in *samples {
                auditor.record_message(
                    "receiver",
                    &ServerMessage::RelayStats {
                        interval_ms: *interval_ms,
                        sent_to_you: *sent_to_you,
                        dropped_for_you: *dropped_for_you,
                        backpressure_events: *backpressure_events,
                    },
                );
            }
        });
        assert_eq!(failed, *should_panic, "case {name}");
    }
}

#[test]
fn conformance_close_causes_remain_distinct() {
    let cases = [
        (4000, ReceiverDisconnectCause::ServerRestart),
        (4002, ReceiverDisconnectCause::SlowConsumer),
        (4003, ReceiverDisconnectCause::ActivityTimeout),
        (
            4004,
            ReceiverDisconnectCause::ServerClose {
                code: 4004,
                reason: "idle_timeout".to_string(),
            },
        ),
    ];
    for (code, expected) in cases {
        let auditor = ConformanceAuditor::new();
        let reason = if code == 4004 {
            "idle_timeout"
        } else {
            "fixture"
        };
        auditor.record_close("receiver", code, reason);
        assert_eq!(auditor.disconnect_cause("receiver"), Some(expected));
    }

    let restart = ConformanceAuditor::new();
    restart.note_server_restart("receiver");
    assert_eq!(
        restart.disconnect_cause("receiver"),
        Some(ReceiverDisconnectCause::ServerRestart)
    );
}

#[test]
fn conformance_text_and_binary_entrypoints_record_once() {
    let text_auditor = ConformanceAuditor::new();
    let text_sender = id(5);
    text_auditor.record_message("receiver", &player_joined(text_sender, 1));
    let text = serde_json::to_string(&ServerMessage::GameData {
        from_player: text_sender,
        data: serde_json::json!({"ledger_sender": "text", "seq": 0}),
        seq: Some(1),
        epoch: Some(1),
    })
    .expect("serialize text fixture");
    text_auditor.record_text_frame("receiver", &text);
    assert_eq!(text_auditor.received_count("receiver", "text"), 1);

    let binary_auditor = ConformanceAuditor::new();
    let binary_sender = id(6);
    binary_auditor.record_message("receiver", &player_joined(binary_sender, 1));
    let fixture = RecordedBinaryGameData {
        from_player: binary_sender,
        encoding: GameDataEncoding::MessagePack,
        payload: rmp_serde::to_vec_named(&serde_json::json!({
            "ledger_sender": "binary",
            "seq": 0
        }))
        .expect("serialize binary payload"),
        seq: Some(1),
        epoch: Some(1),
    };
    let wire = rmp_serde::to_vec_named(&fixture).expect("serialize binary frame");
    assert_eq!(
        binary_auditor.record_binary_frame("receiver", &wire),
        fixture
    );
    assert_eq!(binary_auditor.received_count("receiver", "binary"), 1);
}
