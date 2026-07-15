mod websocket_test_helpers;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use chrono::Utc;
use futures_util::{Stream, StreamExt};
use signal_fish_server::metrics::{DeliveryClassMetrics, DeliveryMetricsByClass};
use signal_fish_server::protocol::{
    DeliveryClass, DeliveryCountersByClass, DeliveryGap, DeliveryGapReason, DeliveryReportPayload,
    ErrorCode, GameDataEncoding, LobbyState, PlayerId, PlayerInfo, ReconnectedPayload,
    ReplayStatus, RoomJoinedPayload, SenderWatermark, ServerMessage, SpectatorJoinedPayload,
    DELIVERY_REPORT_MAX_GAPS,
};
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use websocket_test_helpers::conformance::{
    assert_delivery_class_snapshot_conserves, ConformanceAuditor, ReceiverDisconnectCause,
    ReceiverProtocolMode, RecordedBinaryGameData,
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
    player_with_epoch(player_id, Some(epoch))
}

fn player_with_epoch(player_id: PlayerId, epoch: Option<u32>) -> PlayerInfo {
    player_with_stamp(player_id, epoch, epoch.map(|_| 0))
}

fn player_with_stamp(player_id: PlayerId, epoch: Option<u32>, seq: Option<u64>) -> PlayerInfo {
    PlayerInfo {
        id: player_id,
        name: format!("player-{player_id}"),
        is_authority: false,
        is_ready: false,
        connected_at: Utc::now(),
        connection_info: None,
        epoch,
        seq,
        region_id: String::new(),
    }
}

fn player_joined(player_id: PlayerId, epoch: u32) -> ServerMessage {
    ServerMessage::PlayerJoined {
        player: player(player_id, epoch),
    }
}

fn player_joined_with_epoch(player_id: PlayerId, epoch: Option<u32>) -> ServerMessage {
    ServerMessage::PlayerJoined {
        player: player_with_epoch(player_id, epoch),
    }
}

fn room_joined(player_id: PlayerId, epoch: u32) -> ServerMessage {
    room_joined_with_epoch(player_id, Some(epoch))
}

fn room_joined_with_stamp(player_id: PlayerId, epoch: u32, seq: u64) -> ServerMessage {
    room_joined_with_players(vec![player_with_stamp(player_id, Some(epoch), Some(seq))])
}

fn room_joined_with_epoch(player_id: PlayerId, epoch: Option<u32>) -> ServerMessage {
    room_joined_with_players(vec![player_with_epoch(player_id, epoch)])
}

fn room_joined_with_players(current_players: Vec<PlayerInfo>) -> ServerMessage {
    ServerMessage::RoomJoined(Box::new(RoomJoinedPayload {
        room_id: id(98),
        room_code: "AUDIT2".to_string(),
        player_id: id(100),
        game_name: "audit".to_string(),
        max_players: 4,
        supports_authority: false,
        current_players,
        is_authority: false,
        lobby_state: LobbyState::Lobby,
        ready_players: Vec::new(),
        relay_type: "WebSocket".to_string(),
        current_spectators: Vec::new(),
        ice_servers: Vec::new(),
        reconnection_token: None,
    }))
}

fn spectator_joined(player_id: PlayerId, epoch: u32) -> ServerMessage {
    spectator_joined_with_epoch(player_id, Some(epoch))
}

fn spectator_joined_with_stamp(player_id: PlayerId, epoch: u32, seq: u64) -> ServerMessage {
    ServerMessage::SpectatorJoined(Box::new(SpectatorJoinedPayload {
        room_id: id(97),
        room_code: "WATCH1".to_string(),
        spectator_id: id(100),
        game_name: "audit".to_string(),
        current_players: vec![player_with_stamp(player_id, Some(epoch), Some(seq))],
        current_spectators: Vec::new(),
        lobby_state: LobbyState::Lobby,
        reason: None,
    }))
}

fn spectator_joined_with_epoch(player_id: PlayerId, epoch: Option<u32>) -> ServerMessage {
    ServerMessage::SpectatorJoined(Box::new(SpectatorJoinedPayload {
        room_id: id(97),
        room_code: "WATCH1".to_string(),
        spectator_id: id(100),
        game_name: "audit".to_string(),
        current_players: vec![player_with_epoch(player_id, epoch)],
        current_spectators: Vec::new(),
        lobby_state: LobbyState::Lobby,
        reason: None,
    }))
}

fn game_data(player_id: PlayerId, seq: Option<u64>, epoch: Option<u32>) -> ServerMessage {
    classified_game_data(player_id, seq, epoch, None, None)
}

fn player_left(player_id: PlayerId, epoch: u32, final_seq: u64) -> ServerMessage {
    ServerMessage::PlayerLeft {
        player_id,
        epoch: Some(epoch),
        final_seq: Some(final_seq),
    }
}

fn v2_player_left(player_id: PlayerId) -> ServerMessage {
    ServerMessage::PlayerLeft {
        player_id,
        epoch: None,
        final_seq: None,
    }
}

fn classified_game_data(
    player_id: PlayerId,
    seq: Option<u64>,
    epoch: Option<u32>,
    class: Option<DeliveryClass>,
    key: Option<u32>,
) -> ServerMessage {
    ServerMessage::GameData {
        from_player: player_id,
        data: serde_json::json!({}),
        seq,
        epoch,
        class,
        key,
    }
}

fn format_error() -> ServerMessage {
    ServerMessage::Error {
        message: "undeliverable".to_string(),
        error_code: Some(ErrorCode::UnsupportedGameDataFormat),
    }
}

fn delivery_report(
    counters: DeliveryCountersByClass,
    gaps: impl IntoIterator<Item = DeliveryGap>,
) -> ServerMessage {
    ServerMessage::DeliveryReport(Box::new(DeliveryReportPayload {
        per_class: counters,
        gaps: gaps.into_iter().collect(),
    }))
}

fn reconnected_payload(player_id: PlayerId, epoch: u32, seq: u64) -> ReconnectedPayload {
    ReconnectedPayload {
        room_id: id(99),
        room_code: "AUDIT1".to_string(),
        player_id: id(100),
        game_name: "audit".to_string(),
        max_players: 4,
        supports_authority: false,
        current_players: vec![player_with_stamp(player_id, Some(epoch), Some(seq))],
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
    }
}

fn panics(action: impl FnOnce()) -> bool {
    catch_unwind(AssertUnwindSafe(action)).is_err()
}

fn increment_gap_counter(counters: &mut DeliveryCountersByClass, gap: &DeliveryGap) {
    let count = gap.to_seq - gap.from_seq + 1;
    match gap.reason {
        DeliveryGapReason::LatestSuperseded => counters.latest.superseded += count,
        DeliveryGapReason::LatestDroppedFull => counters.latest.dropped_full += count,
        DeliveryGapReason::VolatileDropped => counters.volatile.dropped += count,
        DeliveryGapReason::UnsupportedFormat => counters.reliable.unsupported_format += count,
    }
}

#[test]
fn conformance_stamps_require_prior_exact_gap_reports() {
    #[derive(Clone, Copy)]
    enum Step {
        Data(Option<u64>, Option<u32>),
        FormatError,
        Gap {
            sender: u128,
            epoch: u32,
            from_seq: u64,
            to_seq: u64,
            reason: DeliveryGapReason,
        },
    }

    let cases: &[(&str, ReceiverProtocolMode, &[Step], bool)] = &[
        (
            "contiguous",
            ReceiverProtocolMode::V3,
            &[Step::Data(Some(1), Some(1)), Step::Data(Some(2), Some(1))],
            false,
        ),
        (
            "v2 unstamped",
            ReceiverProtocolMode::V2,
            &[Step::Data(None, None)],
            false,
        ),
        (
            "v3 unstamped",
            ReceiverProtocolMode::V3,
            &[Step::Data(None, None)],
            true,
        ),
        (
            "half stamp",
            ReceiverProtocolMode::V3,
            &[Step::Data(Some(1), None)],
            true,
        ),
        (
            "zero seq",
            ReceiverProtocolMode::V3,
            &[Step::Data(Some(0), Some(1))],
            true,
        ),
        (
            "duplicate",
            ReceiverProtocolMode::V3,
            &[Step::Data(Some(1), Some(1)), Step::Data(Some(1), Some(1))],
            true,
        ),
        (
            "unexplained gap",
            ReceiverProtocolMode::V3,
            &[Step::Data(Some(1), Some(1)), Step::Data(Some(3), Some(1))],
            true,
        ),
        (
            "prior exact unsupported-format cause",
            ReceiverProtocolMode::V3,
            &[
                Step::Data(Some(1), Some(1)),
                Step::Gap {
                    sender: 1,
                    epoch: 1,
                    from_seq: 2,
                    to_seq: 2,
                    reason: DeliveryGapReason::UnsupportedFormat,
                },
                Step::FormatError,
                Step::Data(Some(3), Some(1)),
            ],
            false,
        ),
        (
            "adjacent exact causes",
            ReceiverProtocolMode::V3,
            &[
                Step::Data(Some(1), Some(1)),
                Step::Gap {
                    sender: 1,
                    epoch: 1,
                    from_seq: 2,
                    to_seq: 3,
                    reason: DeliveryGapReason::LatestSuperseded,
                },
                Step::Gap {
                    sender: 1,
                    epoch: 1,
                    from_seq: 4,
                    to_seq: 4,
                    reason: DeliveryGapReason::VolatileDropped,
                },
                Step::Data(Some(5), Some(1)),
            ],
            false,
        ),
        (
            "future exact cause remains pending",
            ReceiverProtocolMode::V3,
            &[
                Step::Data(Some(1), Some(1)),
                Step::Gap {
                    sender: 1,
                    epoch: 1,
                    from_seq: 4,
                    to_seq: 4,
                    reason: DeliveryGapReason::LatestDroppedFull,
                },
                Step::Data(Some(2), Some(1)),
                Step::Data(Some(3), Some(1)),
                Step::Data(Some(5), Some(1)),
            ],
            false,
        ),
        (
            "unsupported-format error is not a gap budget",
            ReceiverProtocolMode::V3,
            &[
                Step::Data(Some(1), Some(1)),
                Step::FormatError,
                Step::Data(Some(3), Some(1)),
            ],
            true,
        ),
        (
            "wrong sender",
            ReceiverProtocolMode::V3,
            &[Step::Gap {
                sender: 2,
                epoch: 1,
                from_seq: 1,
                to_seq: 1,
                reason: DeliveryGapReason::VolatileDropped,
            }],
            true,
        ),
        (
            "wrong epoch",
            ReceiverProtocolMode::V3,
            &[Step::Gap {
                sender: 1,
                epoch: 2,
                from_seq: 1,
                to_seq: 1,
                reason: DeliveryGapReason::VolatileDropped,
            }],
            true,
        ),
        (
            "duplicate range",
            ReceiverProtocolMode::V3,
            &[
                Step::Gap {
                    sender: 1,
                    epoch: 1,
                    from_seq: 1,
                    to_seq: 2,
                    reason: DeliveryGapReason::LatestSuperseded,
                },
                Step::Gap {
                    sender: 1,
                    epoch: 1,
                    from_seq: 1,
                    to_seq: 2,
                    reason: DeliveryGapReason::LatestSuperseded,
                },
            ],
            true,
        ),
        (
            "overlapping range",
            ReceiverProtocolMode::V3,
            &[
                Step::Gap {
                    sender: 1,
                    epoch: 1,
                    from_seq: 2,
                    to_seq: 3,
                    reason: DeliveryGapReason::LatestSuperseded,
                },
                Step::Gap {
                    sender: 1,
                    epoch: 1,
                    from_seq: 3,
                    to_seq: 4,
                    reason: DeliveryGapReason::VolatileDropped,
                },
            ],
            true,
        ),
        (
            "partial coverage",
            ReceiverProtocolMode::V3,
            &[
                Step::Data(Some(1), Some(1)),
                Step::Gap {
                    sender: 1,
                    epoch: 1,
                    from_seq: 2,
                    to_seq: 2,
                    reason: DeliveryGapReason::VolatileDropped,
                },
                Step::Data(Some(4), Some(1)),
            ],
            true,
        ),
        (
            "report claims delivered successor",
            ReceiverProtocolMode::V3,
            &[
                Step::Data(Some(1), Some(1)),
                Step::Gap {
                    sender: 1,
                    epoch: 1,
                    from_seq: 2,
                    to_seq: 4,
                    reason: DeliveryGapReason::VolatileDropped,
                },
                Step::Data(Some(4), Some(1)),
            ],
            true,
        ),
        (
            "late range",
            ReceiverProtocolMode::V3,
            &[
                Step::Data(Some(1), Some(1)),
                Step::Data(Some(2), Some(1)),
                Step::Gap {
                    sender: 1,
                    epoch: 1,
                    from_seq: 2,
                    to_seq: 2,
                    reason: DeliveryGapReason::UnsupportedFormat,
                },
            ],
            true,
        ),
    ];

    for (name, mode, steps, should_panic) in cases {
        let auditor = ConformanceAuditor::new(*mode);
        let sender = id(1);
        let lifecycle_epoch = (*mode == ReceiverProtocolMode::V3).then_some(1);
        auditor.record_message("receiver", &room_joined_with_epoch(sender, lifecycle_epoch));
        let failed = panics(|| {
            let mut wire_counters = DeliveryCountersByClass::default();
            for step in *steps {
                match step {
                    Step::Data(seq, epoch) => {
                        auditor.record_message("receiver", &game_data(sender, *seq, *epoch));
                        wire_counters.reliable.delivered += 1;
                    }
                    Step::FormatError => auditor.record_message("receiver", &format_error()),
                    Step::Gap {
                        sender,
                        epoch,
                        from_seq,
                        to_seq,
                        reason,
                    } => {
                        let gap = DeliveryGap {
                            from_player: id(*sender),
                            epoch: *epoch,
                            from_seq: *from_seq,
                            to_seq: *to_seq,
                            reason: *reason,
                        };
                        increment_gap_counter(&mut wire_counters, &gap);
                        auditor.record_message("receiver", &delivery_report(wire_counters, [gap]));
                    }
                }
            }
        });
        assert_eq!(failed, *should_panic, "case {name}");
    }
}

#[test]
fn conformance_modes_enforce_delivery_class_key_and_epoch_shapes() {
    let sender = id(70);
    let class_cases = [
        ("implicit reliable", None, None, false),
        (
            "explicit reliable",
            Some(DeliveryClass::Reliable),
            None,
            false,
        ),
        ("keyed latest", Some(DeliveryClass::Latest), Some(0), false),
        (
            "latest without key",
            Some(DeliveryClass::Latest),
            None,
            true,
        ),
        (
            "reliable with key",
            Some(DeliveryClass::Reliable),
            Some(1),
            true,
        ),
        (
            "volatile with key",
            Some(DeliveryClass::Volatile),
            Some(1),
            true,
        ),
        ("key without class", None, Some(1), true),
    ];
    for (name, class, key, should_panic) in class_cases {
        let auditor = ConformanceAuditor::new(ReceiverProtocolMode::V3);
        auditor.record_message("receiver", &room_joined(sender, 1));
        assert_eq!(
            panics(|| auditor.record_message(
                "receiver",
                &classified_game_data(sender, Some(1), Some(1), class, key)
            )),
            should_panic,
            "case {name}"
        );
    }

    let v2 = ConformanceAuditor::new(ReceiverProtocolMode::V2);
    v2.record_message("receiver", &room_joined_with_epoch(sender, None));
    assert!(panics(|| v2.record_message(
        "receiver",
        &classified_game_data(sender, None, None, Some(DeliveryClass::Reliable), None)
    )));
    v2.record_message("other", &room_joined_with_epoch(id(700), None));
    assert!(panics(|| v2.record_message(
        "other",
        &player_joined_with_epoch(sender, Some(1))
    )));

    let v3 = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    v3.record_message("receiver", &room_joined(id(701), 1));
    assert!(panics(|| v3.record_message(
        "receiver",
        &player_joined_with_epoch(sender, None)
    )));
}

#[test]
fn conformance_gap_counters_are_causal_with_rate_limited_unsupported_advisories() {
    type Mutate = fn(&mut DeliveryCountersByClass);
    let mismatches: &[(&str, DeliveryGapReason, Mutate)] = &[
        (
            "superseded reported as dropped-full",
            DeliveryGapReason::LatestSuperseded,
            |c| c.latest.dropped_full = 1,
        ),
        (
            "dropped-full reported as volatile",
            DeliveryGapReason::LatestDroppedFull,
            |c| c.volatile.dropped = 1,
        ),
        (
            "volatile reported as superseded",
            DeliveryGapReason::VolatileDropped,
            |c| c.latest.superseded = 1,
        ),
        (
            "unsupported without counter",
            DeliveryGapReason::UnsupportedFormat,
            |_| {},
        ),
    ];
    for (name, reason, mutate) in mismatches {
        let auditor = ConformanceAuditor::new(ReceiverProtocolMode::V3);
        let sender = id(71);
        auditor.record_message("receiver", &room_joined(sender, 1));
        let mut counters = DeliveryCountersByClass::default();
        mutate(&mut counters);
        assert!(
            panics(|| auditor.record_message(
                "receiver",
                &delivery_report(
                    counters,
                    [DeliveryGap {
                        from_player: sender,
                        epoch: 1,
                        from_seq: 1,
                        to_seq: 1,
                        reason: *reason,
                    }],
                )
            )),
            "case {name}"
        );
    }

    let deferred = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    let sender = id(72);
    deferred.record_message("receiver", &room_joined(sender, 1));
    let mut counters = DeliveryCountersByClass::default();
    counters.reliable.unsupported_format = 1;
    deferred.record_message(
        "receiver",
        &delivery_report(
            counters,
            [DeliveryGap {
                from_player: sender,
                epoch: 1,
                from_seq: 1,
                to_seq: 1,
                reason: DeliveryGapReason::UnsupportedFormat,
            }],
        ),
    );
    deferred.record_message("receiver", &ServerMessage::Pong);
    deferred.record_message("receiver", &format_error());

    let terminal = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    terminal.record_message("receiver", &room_joined(sender, 1));
    terminal.record_message(
        "receiver",
        &delivery_report(
            counters,
            [DeliveryGap {
                from_player: sender,
                epoch: 1,
                from_seq: 1,
                to_seq: 1,
                reason: DeliveryGapReason::UnsupportedFormat,
            }],
        ),
    );
    terminal.note_injected_fault("receiver", "Error replacement write failed");
}

#[test]
fn conformance_delivery_report_uses_canonical_gap_bound_and_rollover() {
    let players: Vec<_> = (0..=DELIVERY_REPORT_MAX_GAPS)
        .map(|index| player(id(10_000 + index as u128), 1))
        .collect();
    let gaps: Vec<_> = players[..DELIVERY_REPORT_MAX_GAPS]
        .iter()
        .map(|player| DeliveryGap {
            from_player: player.id,
            epoch: 1,
            from_seq: 1,
            to_seq: 1,
            reason: DeliveryGapReason::LatestSuperseded,
        })
        .collect();

    let rollover = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    rollover.record_message("receiver", &room_joined_with_players(players.clone()));
    let mut counters = DeliveryCountersByClass::default();
    counters.latest.superseded = DELIVERY_REPORT_MAX_GAPS as u64;
    rollover.record_message("receiver", &delivery_report(counters, gaps.clone()));
    counters.latest.superseded += 1;
    rollover.record_message(
        "receiver",
        &delivery_report(
            counters,
            [DeliveryGap {
                from_player: players[DELIVERY_REPORT_MAX_GAPS].id,
                epoch: 1,
                from_seq: 1,
                to_seq: 1,
                reason: DeliveryGapReason::LatestSuperseded,
            }],
        ),
    );

    let oversized = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    oversized.record_message("receiver", &room_joined_with_players(players));
    let mut oversized_gaps = gaps;
    oversized_gaps.push(DeliveryGap {
        from_player: id(10_000 + DELIVERY_REPORT_MAX_GAPS as u128),
        epoch: 1,
        from_seq: 1,
        to_seq: 1,
        reason: DeliveryGapReason::LatestSuperseded,
    });
    assert_eq!(oversized_gaps.len(), DELIVERY_REPORT_MAX_GAPS + 1);
    assert!(panics(|| oversized.record_message(
        "receiver",
        &delivery_report(counters, oversized_gaps)
    )));
}

#[test]
fn conformance_delivery_report_counters_are_monotone_per_connection() {
    type Mutate = fn(&mut DeliveryCountersByClass);
    let fields: &[(
        &str,
        Mutate,
        Option<DeliveryGapReason>,
        Option<DeliveryClass>,
    )] = &[
        (
            "reliable.delivered",
            |c| c.reliable.delivered = 1,
            None,
            Some(DeliveryClass::Reliable),
        ),
        (
            "reliable.unsupported_format",
            |c| c.reliable.unsupported_format = 1,
            Some(DeliveryGapReason::UnsupportedFormat),
            None,
        ),
        (
            "latest.delivered",
            |c| c.latest.delivered = 1,
            None,
            Some(DeliveryClass::Latest),
        ),
        (
            "latest.superseded",
            |c| c.latest.superseded = 1,
            Some(DeliveryGapReason::LatestSuperseded),
            None,
        ),
        (
            "latest.dropped_full",
            |c| c.latest.dropped_full = 1,
            Some(DeliveryGapReason::LatestDroppedFull),
            None,
        ),
        (
            "latest.unsupported_format",
            |c| c.latest.unsupported_format = 1,
            Some(DeliveryGapReason::UnsupportedFormat),
            None,
        ),
        (
            "volatile.delivered",
            |c| c.volatile.delivered = 1,
            None,
            Some(DeliveryClass::Volatile),
        ),
        (
            "volatile.dropped",
            |c| c.volatile.dropped = 1,
            Some(DeliveryGapReason::VolatileDropped),
            None,
        ),
        (
            "volatile.unsupported_format",
            |c| c.volatile.unsupported_format = 1,
            Some(DeliveryGapReason::UnsupportedFormat),
            None,
        ),
    ];

    let monotone = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    monotone.record_message("receiver", &room_joined(id(79), 1));
    monotone.record_message("receiver", &game_data(id(79), Some(1), Some(1)));
    let mut delivered = DeliveryCountersByClass::default();
    delivered.reliable.delivered = 1;
    monotone.record_message("receiver", &delivery_report(delivered, []));
    monotone.record_message("receiver", &delivery_report(delivered, []));
    monotone.record_message("receiver", &game_data(id(79), Some(2), Some(1)));
    delivered.reliable.delivered = 2;
    monotone.record_message("receiver", &delivery_report(delivered, []));

    for (name, set_one, reason, delivered_class) in fields {
        let auditor = ConformanceAuditor::new(ReceiverProtocolMode::V3);
        let sender = id(80);
        auditor.record_message("receiver", &room_joined(sender, 1));
        if let Some(class) = delivered_class {
            auditor.record_message(
                "receiver",
                &classified_game_data(
                    sender,
                    Some(1),
                    Some(1),
                    (*class != DeliveryClass::Reliable).then_some(*class),
                    (*class == DeliveryClass::Latest).then_some(1),
                ),
            );
        }
        let mut first = DeliveryCountersByClass::default();
        set_one(&mut first);
        let gaps = reason.map_or_else(Vec::new, |reason| {
            vec![DeliveryGap {
                from_player: sender,
                epoch: 1,
                from_seq: 1,
                to_seq: 1,
                reason,
            }]
        });
        auditor.record_message("receiver", &delivery_report(first, gaps));
        if *reason == Some(DeliveryGapReason::UnsupportedFormat) {
            auditor.record_message("receiver", &format_error());
        }
        assert!(
            panics(|| auditor.record_message(
                "receiver",
                &delivery_report(DeliveryCountersByClass::default(), [])
            )),
            "counter {name} regression must fail"
        );
    }
}

#[test]
fn conformance_reports_match_written_classes_and_abandonment_is_terminal() {
    let sender = id(81);
    let auditor = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    auditor.record_message("receiver", &room_joined(sender, 1));
    for (seq, class, key) in [
        (1, None, None),
        (2, Some(DeliveryClass::Latest), Some(7)),
        (3, Some(DeliveryClass::Volatile), None),
    ] {
        auditor.record_message(
            "receiver",
            &classified_game_data(sender, Some(seq), Some(1), class, key),
        );
    }
    let delivered = DeliveryCountersByClass {
        reliable: signal_fish_server::protocol::ReliableDeliveryCounters {
            delivered: 1,
            ..Default::default()
        },
        latest: signal_fish_server::protocol::LatestDeliveryCounters {
            delivered: 1,
            ..Default::default()
        },
        volatile: signal_fish_server::protocol::VolatileDeliveryCounters {
            delivered: 1,
            ..Default::default()
        },
    };
    auditor.record_message("receiver", &delivery_report(delivered, []));

    // Strict control priority may put a zero snapshot before older queued
    // data; only frames already written count at each report frontier.
    let priority = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    priority.record_message("receiver", &room_joined(sender, 1));
    priority.record_message(
        "receiver",
        &delivery_report(DeliveryCountersByClass::default(), []),
    );
    priority.record_message("receiver", &game_data(sender, Some(1), Some(1)));
    let mut after_data = DeliveryCountersByClass::default();
    after_data.reliable.delivered = 1;
    priority.record_message("receiver", &delivery_report(after_data, []));

    // The report snapshot can be captured while a data write is in flight, then
    // reach the stream after that write completes. Lag is valid; claiming a
    // delivery that has not reached the physical stream is not.
    let in_flight = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    in_flight.record_message("receiver", &room_joined(sender, 1));
    in_flight.record_message("receiver", &game_data(sender, Some(1), Some(1)));
    in_flight.record_message(
        "receiver",
        &delivery_report(DeliveryCountersByClass::default(), []),
    );
    let mut ahead = DeliveryCountersByClass::default();
    ahead.reliable.delivered = 2;
    assert!(panics(
        || in_flight.record_message("receiver", &delivery_report(ahead, []))
    ));

    let continuing = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    continuing.record_message("receiver", &room_joined(sender, 1));
    let mut abandoned = DeliveryCountersByClass::default();
    abandoned.reliable.abandoned = 1;
    continuing.record_message("receiver", &delivery_report(abandoned, []));
    assert!(panics(
        || continuing.record_message("receiver", &ServerMessage::Pong)
    ));

    let terminal = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    terminal.record_message("receiver", &room_joined(sender, 1));
    terminal.record_message("receiver", &delivery_report(abandoned, []));
    terminal.record_message(
        "receiver",
        &ServerMessage::Error {
            message: "closing after abandoned delivery".to_string(),
            error_code: Some(ErrorCode::SlowConsumer),
        },
    );
    terminal.note_injected_fault("receiver", "terminal socket close");

    let combined = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    combined.record_message("receiver", &room_joined(sender, 1));
    let mut abandoned_unsupported = abandoned;
    abandoned_unsupported.reliable.unsupported_format = 1;
    let combined_report = delivery_report(
        abandoned_unsupported,
        [DeliveryGap {
            from_player: sender,
            epoch: 1,
            from_seq: 1,
            to_seq: 1,
            reason: DeliveryGapReason::UnsupportedFormat,
        }],
    );
    combined.record_message("receiver", &combined_report);
    combined.record_message("receiver", &format_error());
    assert!(panics(|| combined.record_message(
        "receiver",
        &ServerMessage::Error {
            message: "duplicate terminal advisory".to_string(),
            error_code: Some(ErrorCode::SlowConsumer),
        },
    )));

    let combined_terminal = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    combined_terminal.record_message("receiver", &room_joined(sender, 1));
    combined_terminal.record_message("receiver", &combined_report);
    combined_terminal.record_message("receiver", &format_error());
    combined_terminal.note_injected_fault("receiver", "terminal socket close");
}

#[test]
fn conformance_room_and_connection_boundaries_discard_gap_authority() {
    #[derive(Clone, Copy)]
    enum Boundary {
        RoomJoined,
        RoomLeft,
        SpectatorJoined,
        SpectatorLeft,
    }

    let old_sender = id(10);
    let new_sender = id(11);
    for boundary in [
        Boundary::RoomJoined,
        Boundary::RoomLeft,
        Boundary::SpectatorJoined,
        Boundary::SpectatorLeft,
    ] {
        let auditor = ConformanceAuditor::new(ReceiverProtocolMode::V3);
        if matches!(boundary, Boundary::SpectatorLeft) {
            auditor.record_message("receiver", &spectator_joined(old_sender, 1));
        } else {
            auditor.record_message("receiver", &room_joined(old_sender, 1));
        }
        let mut before = DeliveryCountersByClass::default();
        before.volatile.dropped = 1;
        auditor.record_message(
            "receiver",
            &delivery_report(
                before,
                [DeliveryGap {
                    from_player: old_sender,
                    epoch: 1,
                    from_seq: 1,
                    to_seq: 1,
                    reason: DeliveryGapReason::VolatileDropped,
                }],
            ),
        );

        match boundary {
            Boundary::RoomJoined => {
                auditor.record_message("receiver", &ServerMessage::RoomLeft);
                auditor.record_message("receiver", &room_joined(new_sender, 2));
            }
            Boundary::RoomLeft => auditor.record_message("receiver", &ServerMessage::RoomLeft),
            Boundary::SpectatorJoined => {
                auditor.record_message("receiver", &ServerMessage::RoomLeft);
                auditor.record_message("receiver", &spectator_joined(new_sender, 2));
            }
            Boundary::SpectatorLeft => auditor.record_message(
                "receiver",
                &ServerMessage::SpectatorLeft {
                    room_id: None,
                    room_code: None,
                    reason: None,
                    current_spectators: Vec::new(),
                },
            ),
        }

        assert!(
            panics(|| auditor.record_message(
                "receiver",
                &delivery_report(
                    before,
                    [DeliveryGap {
                        from_player: old_sender,
                        epoch: 1,
                        from_seq: 2,
                        to_seq: 2,
                        reason: DeliveryGapReason::VolatileDropped,
                    }],
                ),
            )),
            "old sender report must fail after scope boundary"
        );
    }

    // A reconnect may retain the sender/epoch while moving its baseline. Its
    // new connection must not inherit a future range from the old queue.
    let reconnect = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    reconnect.record_message("receiver", &room_joined(old_sender, 1));
    reconnect.record_message("receiver", &game_data(old_sender, Some(1), Some(1)));
    reconnect.record_message(
        "receiver",
        &delivery_report(
            DeliveryCountersByClass {
                reliable: signal_fish_server::protocol::ReliableDeliveryCounters {
                    delivered: 1,
                    ..Default::default()
                },
                latest: signal_fish_server::protocol::LatestDeliveryCounters {
                    superseded: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
            [DeliveryGap {
                from_player: old_sender,
                epoch: 1,
                from_seq: 6,
                to_seq: 6,
                reason: DeliveryGapReason::LatestSuperseded,
            }],
        ),
    );
    reconnect.note_injected_fault("receiver", "reconnect fixture cut");
    reconnect.record_reconnect(
        "receiver",
        "receiver-reborn",
        &reconnected_payload(old_sender, 1, 5),
    );
    reconnect.record_message(
        "receiver-reborn",
        &delivery_report(DeliveryCountersByClass::default(), []),
    );
    assert!(panics(|| reconnect.record_message(
        "receiver-reborn",
        &game_data(old_sender, Some(7), Some(1))
    )));
}

#[test]
fn conformance_spectator_snapshots_are_mode_explicit_and_reset_scope() {
    let sender = id(12);
    let v3 = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    v3.record_message("spectator", &spectator_joined_with_stamp(sender, 4, 40));
    v3.record_message("spectator", &game_data(sender, Some(41), Some(4)));
    v3.record_message(
        "spectator",
        &ServerMessage::SpectatorLeft {
            room_id: None,
            room_code: None,
            reason: None,
            current_spectators: Vec::new(),
        },
    );
    assert!(panics(|| v3.record_message(
        "spectator",
        &game_data(sender, Some(42), Some(4))
    )));

    let missing_epoch = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    assert!(panics(|| missing_epoch.record_message(
        "spectator",
        &spectator_joined_with_epoch(sender, None)
    )));

    let v2 = ConformanceAuditor::new(ReceiverProtocolMode::V2);
    v2.record_message("spectator", &spectator_joined_with_epoch(sender, None));
    v2.record_message("spectator", &game_data(sender, None, None));
    assert!(panics(|| v2.record_message(
        "other",
        &spectator_joined_with_epoch(sender, Some(4))
    )));
}

#[test]
fn conformance_reconnect_watermark_shape_is_data_driven() {
    type Mutate = fn(&mut ReconnectedPayload);
    let cases: &[(&str, Mutate, bool)] = &[
        ("complete", |_| {}, false),
        ("missing", |payload| payload.sender_watermarks.clear(), true),
        (
            "duplicate",
            |payload| payload.sender_watermarks.push(payload.sender_watermarks[0]),
            true,
        ),
        (
            "snapshot mismatch",
            |payload| payload.sender_watermarks[0].epoch += 1,
            true,
        ),
        (
            "absent player",
            |payload| payload.current_players.clear(),
            true,
        ),
        (
            "backward sequence",
            |payload| payload.sender_watermarks[0].seq = 0,
            true,
        ),
    ];

    let sender = id(13);
    for (name, mutate, should_panic) in cases {
        let auditor = ConformanceAuditor::new(ReceiverProtocolMode::V3);
        auditor.record_message("before", &room_joined(sender, 1));
        auditor.record_message("before", &game_data(sender, Some(1), Some(1)));
        auditor.note_injected_fault("before", "fixture reconnect cut");
        let mut payload = reconnected_payload(sender, 1, 1);
        mutate(&mut payload);
        assert_eq!(
            panics(|| auditor.record_reconnect("before", "after", &payload)),
            *should_panic,
            "case {name}"
        );
    }

    let v2 = ConformanceAuditor::new(ReceiverProtocolMode::V2);
    v2.record_message("before", &room_joined_with_epoch(sender, None));
    v2.note_injected_fault("before", "fixture reconnect cut");
    let mut payload = reconnected_payload(sender, 1, 0);
    payload.current_players[0].epoch = None;
    payload.current_players[0].seq = None;
    payload.sender_watermarks.clear();
    payload.replay = None;
    v2.record_reconnect("before", "after", &payload);

    let implicit = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    assert!(panics(|| implicit.record_message(
        "after",
        &ServerMessage::Reconnected(Box::new(reconnected_payload(sender, 1, 0)))
    )));
}

#[test]
fn conformance_reconnect_modes_prefaces_and_eligibility_are_identity_scoped() {
    let sender = id(14);

    let mixed = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    mixed.record_message("v3-before", &room_joined(sender, 1));
    mixed.record_message("v3-before", &game_data(sender, Some(1), Some(1)));
    mixed.note_injected_fault("v3-before", "transport replacement");
    mixed.register_receiver_mode("v2-after", ReceiverProtocolMode::V2);
    let mut v2_payload = reconnected_payload(sender, 1, 1);
    v2_payload.current_players[0].epoch = None;
    v2_payload.current_players[0].seq = None;
    v2_payload.sender_watermarks.clear();
    v2_payload.replay = None;
    mixed.record_reconnect("v3-before", "v2-after", &v2_payload);
    mixed.record_message("v2-after", &game_data(sender, None, None));

    let delayed = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    delayed.record_message("before", &room_joined(sender, 1));
    delayed.note_injected_fault("before", "transport replacement");
    delayed.record_message(
        "after",
        &ServerMessage::RelayStats {
            interval_ms: 1_000,
            sent_to_you: 0,
            dropped_for_you: 0,
            backpressure_events: 0,
        },
    );
    delayed.record_message(
        "after",
        &delivery_report(DeliveryCountersByClass::default(), []),
    );
    delayed.record_reconnect("before", "after", &reconnected_payload(sender, 1, 0));
    delayed.record_message("after", &game_data(sender, Some(1), Some(1)));
    let mut delivered = DeliveryCountersByClass::default();
    delivered.reliable.delivered = 1;
    delayed.record_message("after", &delivery_report(delivered, []));
    delayed.record_message(
        "after",
        &ServerMessage::RelayStats {
            interval_ms: 1_000,
            sent_to_you: 1,
            dropped_for_you: 0,
            backpressure_events: 0,
        },
    );

    let dirty_preface = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    dirty_preface.record_message("before", &room_joined(sender, 1));
    dirty_preface.note_injected_fault("before", "transport replacement");
    dirty_preface.record_message(
        "after",
        &ServerMessage::RelayStats {
            interval_ms: 1_000,
            sent_to_you: 1,
            dropped_for_you: 0,
            backpressure_events: 0,
        },
    );
    assert!(panics(|| dirty_preface.record_reconnect(
        "before",
        "after",
        &reconnected_payload(sender, 1, 0)
    )));

    let shutdown = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    shutdown.record_message("before", &room_joined(sender, 1));
    shutdown.record_close("before", 4000, "server_shutdown");
    assert!(panics(|| shutdown.record_reconnect(
        "before",
        "after",
        &reconnected_payload(sender, 1, 0)
    )));

    let restart = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    restart.record_message("before", &room_joined(sender, 1));
    restart.note_server_restart("before");
    assert!(panics(|| restart.record_reconnect(
        "before",
        "after",
        &reconnected_payload(sender, 1, 0)
    )));

    let outside = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    outside.note_injected_fault("before", "transport cut before room join");
    assert!(panics(|| outside.record_reconnect(
        "before",
        "after",
        &reconnected_payload(sender, 1, 0)
    )));

    let after_room_left = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    after_room_left.record_message("before", &room_joined(sender, 1));
    after_room_left.record_message("before", &ServerMessage::RoomLeft);
    after_room_left.note_injected_fault("before", "transport cut after room exit");
    assert!(panics(|| after_room_left.record_reconnect(
        "before",
        "after",
        &reconnected_payload(sender, 1, 0)
    )));

    let spectator = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    spectator.record_message("before", &spectator_joined_with_epoch(sender, Some(1)));
    spectator.note_injected_fault("before", "spectator transport cut");
    assert!(panics(|| spectator.record_reconnect(
        "before",
        "after",
        &reconnected_payload(sender, 1, 0)
    )));

    let normal_close = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    normal_close.record_message("before", &room_joined(sender, 1));
    normal_close.record_close("before", 1000, "unregistered");
    assert!(panics(|| normal_close.record_reconnect(
        "before",
        "after",
        &reconnected_payload(sender, 1, 0)
    )));
}

#[test]
fn conformance_lifecycle_and_late_join_baselines_are_data_driven() {
    let late_join_cases: &[(&str, u64, &[u64], bool)] = &[
        ("new sender", 0, &[1, 2], false),
        ("existing sender", 40, &[41, 42], false),
        ("gap after baseline", 40, &[41, 43], true),
    ];
    for (name, baseline, seqs, should_panic) in late_join_cases {
        let auditor = ConformanceAuditor::new(ReceiverProtocolMode::V3);
        let sender = id(2);
        auditor.record_message("receiver", &room_joined_with_stamp(sender, 7, *baseline));
        let failed = panics(|| {
            for seq in *seqs {
                auditor.record_message("receiver", &game_data(sender, Some(*seq), Some(7)));
            }
        });
        assert_eq!(failed, *should_panic, "case {name}");
    }

    let sender = id(3);
    // Strict control priority allows leave/rejoin controls to overtake data
    // already admitted to the FIFO data lane. The old announced epoch may
    // therefore drain until the first frame from the newer epoch arrives.
    let overtaken = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    overtaken.record_message("receiver", &room_joined(sender, 1));
    overtaken.record_message("receiver", &player_left(sender, 1, 3));
    overtaken.record_message("receiver", &game_data(sender, Some(1), Some(1)));
    overtaken.record_message("receiver", &player_joined(sender, 2));
    let mut overtaken_counters = DeliveryCountersByClass::default();
    overtaken_counters.reliable.delivered = 1;
    overtaken_counters.volatile.dropped = 1;
    overtaken.record_message(
        "receiver",
        &delivery_report(
            overtaken_counters,
            [DeliveryGap {
                from_player: sender,
                epoch: 1,
                from_seq: 2,
                to_seq: 2,
                reason: DeliveryGapReason::VolatileDropped,
            }],
        ),
    );
    overtaken.record_message("receiver", &game_data(sender, Some(3), Some(1)));
    overtaken.record_message("receiver", &game_data(sender, Some(1), Some(2)));
    overtaken_counters.reliable.delivered = 3;
    overtaken_counters.volatile.dropped = 2;
    assert!(panics(|| overtaken.record_message(
        "receiver",
        &delivery_report(
            overtaken_counters,
            [DeliveryGap {
                from_player: sender,
                epoch: 1,
                from_seq: 4,
                to_seq: 4,
                reason: DeliveryGapReason::VolatileDropped,
            }],
        )
    )));

    let backward = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    backward.record_message("receiver", &room_joined(sender, 1));
    backward.record_message("receiver", &game_data(sender, Some(1), Some(1)));
    backward.record_message("receiver", &player_left(sender, 1, 1));
    backward.record_message("receiver", &player_joined(sender, 2));
    backward.record_message("receiver", &game_data(sender, Some(1), Some(2)));
    assert!(panics(|| backward.record_message(
        "receiver",
        &game_data(sender, Some(2), Some(1))
    )));

    let unannounced = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    unannounced.record_message("receiver", &room_joined(sender, 1));
    assert!(panics(|| unannounced.record_message(
        "receiver",
        &game_data(sender, Some(1), Some(2))
    )));

    // Snapshot baselines make pre-snapshot traffic explicitly not owed. Only
    // future seq 92 authorizes the jump from delivered 91 to delivered 93.
    for snapshot in [
        room_joined_with_stamp(sender, 9, 90),
        spectator_joined_with_stamp(sender, 9, 90),
    ] {
        let partitioned = ConformanceAuditor::new(ReceiverProtocolMode::V3);
        partitioned.record_message("receiver", &snapshot);
        let mut counters = DeliveryCountersByClass::default();
        counters.latest.superseded = 1;
        partitioned.record_message(
            "receiver",
            &delivery_report(
                counters,
                [DeliveryGap {
                    from_player: sender,
                    epoch: 9,
                    from_seq: 92,
                    to_seq: 92,
                    reason: DeliveryGapReason::LatestSuperseded,
                }],
            ),
        );
        partitioned.record_message("receiver", &game_data(sender, Some(91), Some(9)));
        partitioned.record_message("receiver", &game_data(sender, Some(93), Some(9)));
    }

    let pre_baseline = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    pre_baseline.record_message("receiver", &room_joined_with_stamp(sender, 9, 90));
    let mut pre_baseline_counters = DeliveryCountersByClass::default();
    pre_baseline_counters.latest.superseded = 1;
    assert!(panics(|| pre_baseline.record_message(
        "receiver",
        &delivery_report(
            pre_baseline_counters,
            [DeliveryGap {
                from_player: sender,
                epoch: 9,
                from_seq: 89,
                to_seq: 89,
                reason: DeliveryGapReason::LatestSuperseded,
            }],
        )
    )));
}

#[test]
fn conformance_rejects_illegal_room_peer_and_v2_membership_transitions() {
    let sender = id(90);

    let duplicate_room = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    duplicate_room.record_message("receiver", &room_joined(sender, 1));
    assert!(panics(
        || duplicate_room.record_message("receiver", &room_joined(sender, 1))
    ));

    let wrong_exit = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    wrong_exit.record_message("receiver", &room_joined(sender, 1));
    assert!(panics(|| wrong_exit.record_message(
        "receiver",
        &ServerMessage::SpectatorLeft {
            room_id: None,
            room_code: None,
            reason: None,
            current_spectators: Vec::new(),
        }
    )));

    let duplicate_join = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    duplicate_join.record_message("receiver", &room_joined(id(91), 1));
    duplicate_join.record_message("receiver", &player_joined(sender, 1));
    assert!(panics(
        || duplicate_join.record_message("receiver", &player_joined(sender, 2))
    ));

    // A reconnect snapshot can race same-epoch live lifecycle delivery. The
    // snapshot watermark (including seq 0) already proves presence, so both
    // routed forms are idempotent until a PlayerLeft transition occurs.
    let overlap = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    overlap.record_message("before", &room_joined(sender, 7));
    overlap.note_injected_fault("before", "snapshot/live overlap fixture");
    overlap.record_reconnect("before", "after", &reconnected_payload(sender, 7, 0));
    overlap.record_message("after", &player_joined(sender, 7));
    overlap.record_message(
        "after",
        &ServerMessage::PlayerReconnected {
            player_id: sender,
            epoch: Some(7),
        },
    );
    overlap.record_message("after", &game_data(sender, Some(1), Some(7)));

    let v2_overlap = ConformanceAuditor::new(ReceiverProtocolMode::V2);
    v2_overlap.record_message("receiver", &room_joined_with_epoch(sender, None));
    v2_overlap.record_message("receiver", &player_joined_with_epoch(sender, None));
    v2_overlap.record_message(
        "receiver",
        &ServerMessage::PlayerReconnected {
            player_id: sender,
            epoch: None,
        },
    );
    v2_overlap.record_message("receiver", &game_data(sender, None, None));

    for (name, lifecycle) in [
        ("PlayerJoined", player_joined(sender, 7)),
        (
            "PlayerReconnected",
            ServerMessage::PlayerReconnected {
                player_id: sender,
                epoch: Some(7),
            },
        ),
    ] {
        let after_left = ConformanceAuditor::new(ReceiverProtocolMode::V3);
        after_left.record_message("receiver", &room_joined(sender, 7));
        after_left.record_message("receiver", &player_left(sender, 7, 1));
        assert!(
            panics(|| after_left.record_message("receiver", &lifecycle)),
            "same-epoch {name} after PlayerLeft must not restore presence"
        );
    }

    let duplicate_left = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    duplicate_left.record_message("receiver", &room_joined(sender, 1));
    duplicate_left.record_message("receiver", &player_left(sender, 1, 1));
    duplicate_left.record_message("receiver", &player_left(sender, 1, 1));

    let absent_left = ConformanceAuditor::new(ReceiverProtocolMode::V2);
    absent_left.record_message("receiver", &room_joined_with_epoch(sender, None));
    assert!(panics(
        || absent_left.record_message("receiver", &v2_player_left(id(999)))
    ));

    let reconnect_after_left = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    reconnect_after_left.record_message("receiver", &room_joined(sender, 1));
    reconnect_after_left.record_message("receiver", &player_left(sender, 1, 1));
    reconnect_after_left.record_message(
        "receiver",
        &ServerMessage::PlayerReconnected {
            player_id: sender,
            epoch: Some(2),
        },
    );

    let reconnect_after_retirement = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    reconnect_after_retirement.record_message("receiver", &room_joined(sender, 1));
    reconnect_after_retirement.record_message("receiver", &player_left(sender, 1, 0));
    assert_eq!(
        reconnect_after_retirement.tracked_sender_count("receiver"),
        0
    );
    reconnect_after_retirement.record_message(
        "receiver",
        &ServerMessage::PlayerReconnected {
            player_id: sender,
            epoch: Some(2),
        },
    );

    let unknown_reconnect = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    unknown_reconnect.record_message("receiver", &room_joined(sender, 1));
    assert!(panics(
        || unknown_reconnect.record_message("receiver", &player_left(id(998), 1, 0))
    ));

    let v2_known = ConformanceAuditor::new(ReceiverProtocolMode::V2);
    v2_known.record_message("receiver", &room_joined_with_epoch(sender, None));
    v2_known.record_message("receiver", &game_data(sender, None, None));

    let v2_unknown = ConformanceAuditor::new(ReceiverProtocolMode::V2);
    v2_unknown.record_message("receiver", &room_joined_with_epoch(sender, None));
    assert!(panics(
        || v2_unknown.record_message("receiver", &game_data(id(92), None, None))
    ));

    let v2_trailing = ConformanceAuditor::new(ReceiverProtocolMode::V2);
    v2_trailing.record_message("receiver", &room_joined_with_epoch(sender, None));
    v2_trailing.record_message("receiver", &v2_player_left(sender));
    v2_trailing.record_message("receiver", &game_data(sender, None, None));

    let v2_outside = ConformanceAuditor::new(ReceiverProtocolMode::V2);
    v2_outside.record_message("receiver", &room_joined_with_epoch(sender, None));
    v2_outside.record_message("receiver", &ServerMessage::RoomLeft);
    assert!(panics(
        || v2_outside.record_message("receiver", &game_data(sender, None, None))
    ));
}

#[test]
fn conformance_player_left_terminal_bounds_churn_and_preserves_v2_shape() {
    let overtaken = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    let overtaken_sender = id(9_997);
    overtaken.record_message("receiver", &room_joined(overtaken_sender, 1));
    for epoch in 1..=3 {
        if epoch > 1 {
            overtaken.record_message(
                "receiver",
                &ServerMessage::PlayerReconnected {
                    player_id: overtaken_sender,
                    epoch: Some(epoch),
                },
            );
        }
        overtaken.record_message("receiver", &player_left(overtaken_sender, epoch, 2));
    }
    let mut counters = DeliveryCountersByClass::default();
    counters.latest.superseded = 2;
    overtaken.record_message(
        "receiver",
        &delivery_report(
            counters,
            [DeliveryGap {
                from_player: overtaken_sender,
                epoch: 2,
                from_seq: 1,
                to_seq: 2,
                reason: DeliveryGapReason::LatestSuperseded,
            }],
        ),
    );
    overtaken.record_message("receiver", &game_data(overtaken_sender, Some(1), Some(1)));
    counters.latest.superseded = 3;
    overtaken.record_message(
        "receiver",
        &delivery_report(
            counters,
            [DeliveryGap {
                from_player: overtaken_sender,
                epoch: 1,
                from_seq: 2,
                to_seq: 2,
                reason: DeliveryGapReason::LatestSuperseded,
            }],
        ),
    );
    counters.latest.superseded = 4;
    overtaken.record_message(
        "receiver",
        &delivery_report(
            counters,
            [DeliveryGap {
                from_player: overtaken_sender,
                epoch: 3,
                from_seq: 1,
                to_seq: 1,
                reason: DeliveryGapReason::LatestSuperseded,
            }],
        ),
    );
    overtaken.record_message("receiver", &game_data(overtaken_sender, Some(2), Some(3)));
    assert_eq!(overtaken.tracked_sender_count("receiver"), 0);

    let snapshot_tail = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    let snapshot_sender = id(9_998);
    snapshot_tail.record_message(
        "receiver",
        &room_joined_with_players(vec![player_with_stamp(snapshot_sender, Some(1), Some(41))]),
    );
    snapshot_tail.record_message("receiver", &player_left(snapshot_sender, 1, 43));
    snapshot_tail.record_message("receiver", &game_data(snapshot_sender, Some(42), Some(1)));
    snapshot_tail.record_message("receiver", &game_data(snapshot_sender, Some(43), Some(1)));
    assert_eq!(
        snapshot_tail.tracked_sender_count("receiver"),
        0,
        "the exact snapshot baseline must retire after only the post-snapshot tail"
    );

    let auditor = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    auditor.record_message("receiver", &room_joined(id(9_999), 1));
    for value in 10_000..11_024 {
        let sender = id(value);
        auditor.record_message("receiver", &player_joined(sender, 1));
        auditor.record_message("receiver", &player_left(sender, 1, 0));
    }
    assert_eq!(
        auditor.tracked_sender_count("receiver"),
        1,
        "terminal zero watermarks must retire every departed churn seat"
    );

    let missing = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    let sender = id(12_000);
    missing.record_message("receiver", &room_joined(sender, 1));
    assert!(panics(|| missing.record_message(
        "receiver",
        &ServerMessage::PlayerLeft {
            player_id: sender,
            epoch: None,
            final_seq: None,
        }
    )));

    let leaked = ConformanceAuditor::new(ReceiverProtocolMode::V2);
    leaked.record_message("receiver", &room_joined_with_epoch(sender, None));
    assert!(panics(
        || leaked.record_message("receiver", &player_left(sender, 1, 0))
    ));

    let frozen = ConformanceAuditor::new(ReceiverProtocolMode::V2);
    frozen.record_message("receiver", &room_joined_with_epoch(sender, None));
    frozen.record_message("receiver", &v2_player_left(sender));

    let leaked_seq = ConformanceAuditor::new(ReceiverProtocolMode::V2);
    assert!(panics(|| leaked_seq.record_message(
        "receiver",
        &room_joined_with_players(vec![player_with_stamp(sender, None, Some(0))]),
    )));
}

#[test]
fn conformance_reconnect_watermark_rebaselines_before_data() {
    let sender = id(4);
    let auditor = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    auditor.record_message("before", &room_joined(sender, 1));
    auditor.record_message("before", &game_data(sender, Some(1), Some(1)));
    auditor.note_injected_fault("before", "fixture transport cut");
    auditor.record_reconnect("before", "after", &reconnected_payload(sender, 1, 5));
    auditor.record_message("after", &game_data(sender, Some(6), Some(1)));

    assert!(panics(
        || auditor.record_message("before", &game_data(sender, Some(2), Some(1)))
    ));
    assert!(panics(|| auditor.record_reconnect(
        "before",
        "before",
        &reconnected_payload(sender, 1, 5)
    )));

    let backwards = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    backwards.record_message("before", &room_joined(sender, 1));
    backwards.record_message("before", &game_data(sender, Some(1), Some(1)));
    backwards.note_injected_fault("before", "fixture transport cut");
    assert!(panics(|| backwards.record_reconnect(
        "before",
        "after",
        &reconnected_payload(sender, 1, 0)
    )));

    let without_baseline = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    without_baseline.record_message("receiver", &room_joined(id(702), 1));
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
        let auditor = ConformanceAuditor::new(ReceiverProtocolMode::V3);
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
        (4000, ReceiverDisconnectCause::ServerShutdown),
        (4002, ReceiverDisconnectCause::SlowConsumer),
        (4003, ReceiverDisconnectCause::ActivityTimeout),
        (4004, ReceiverDisconnectCause::IdleTimeout),
    ];
    for (code, expected) in cases {
        let auditor = ConformanceAuditor::new(ReceiverProtocolMode::V3);
        let reason = match code {
            4000 => "server_shutdown",
            4002 => "slow_consumer",
            4003 => "activity_timeout",
            4004 => "idle_timeout",
            _ => "fixture",
        };
        auditor.record_close("receiver", code, reason);
        assert_eq!(auditor.disconnect_cause("receiver"), Some(expected));
    }

    let restart = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    restart.note_server_restart("receiver");
    assert_eq!(
        restart.disconnect_cause("receiver"),
        Some(ReceiverDisconnectCause::ServerRestart)
    );

    let wrong_shutdown_reason = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    assert!(panics(|| wrong_shutdown_reason.record_close(
        "receiver",
        4000,
        "server_restart"
    )));

    for (code, wrong_reason) in [
        (4002, "slow-consumer"),
        (4003, "activity-timeout"),
        (4004, "idle-timeout"),
    ] {
        let auditor = ConformanceAuditor::new(ReceiverProtocolMode::V3);
        assert!(
            panics(|| auditor.record_close("receiver", code, wrong_reason)),
            "close code {code} must reject non-canonical reason {wrong_reason}"
        );
    }
}

#[test]
fn conformance_text_and_binary_entrypoints_record_once() {
    let text_auditor = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    let text_sender = id(5);
    text_auditor.record_message("receiver", &room_joined(text_sender, 1));
    let text = serde_json::to_string(&ServerMessage::GameData {
        from_player: text_sender,
        data: serde_json::json!({"ledger_sender": "text", "seq": 0}),
        seq: Some(1),
        epoch: Some(1),
        class: None,
        key: None,
    })
    .expect("serialize text fixture");
    text_auditor.record_text_frame("receiver", &text);
    assert_eq!(text_auditor.received_count("receiver", "text"), 1);

    for (offset, encoding) in [
        GameDataEncoding::Json,
        GameDataEncoding::MessagePack,
        GameDataEncoding::Rkyv,
    ]
    .into_iter()
    .enumerate()
    {
        let binary_auditor = ConformanceAuditor::new(ReceiverProtocolMode::V3);
        let binary_sender = id(6 + offset as u128);
        binary_auditor.record_message("receiver", &room_joined(binary_sender, 1));
        let ledger = serde_json::json!({ "ledger_sender": "binary", "seq": 0 });
        let payload = match encoding {
            GameDataEncoding::Json => serde_json::to_vec(&ledger).expect("serialize JSON payload"),
            GameDataEncoding::MessagePack => {
                rmp_serde::to_vec_named(&ledger).expect("serialize MessagePack payload")
            }
            GameDataEncoding::Rkyv => vec![0xde, 0xad, 0xbe, 0xef],
        };
        let fixture = RecordedBinaryGameData {
            from_player: binary_sender,
            encoding,
            payload,
            seq: 1,
            epoch: 1,
        };
        let wire = rmp_serde::to_vec_named(&fixture).expect("serialize binary frame");
        assert_eq!(
            binary_auditor.record_binary_frame("receiver", &wire),
            fixture
        );
        if encoding != GameDataEncoding::Rkyv {
            assert_eq!(binary_auditor.received_count("receiver", "binary"), 1);
        }
    }
}

#[test]
fn conformance_text_entrypoint_rejects_the_physical_binary_shadow() {
    let shadow = serde_json::to_string(&ServerMessage::GameDataBinary {
        from_player: id(0x0011_2233_4455_6677_8899_aabb_ccdd_eeff),
        encoding: GameDataEncoding::MessagePack,
        payload: bytes::Bytes::from_static(b"opaque"),
        seq: Some(1),
        epoch: Some(1),
    })
    .expect("serialize in-memory carrier shadow");
    let auditor = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    assert!(
        panics(|| {
            auditor.record_text_frame("receiver", &shadow);
        }),
        "a text GameDataBinary envelope must not masquerade as the physical binary frame"
    );
}

#[test]
fn conformance_v3_rejects_unenveloped_binary_passthrough() {
    let sender = id(6);
    for raw in [
        br#"{"opaque":"raw-json-passthrough"}"#.as_slice(),
        &[0x81, 0xa5, b'v', b'a', b'l', b'u', b'e', 0x01],
        &[0xde, 0xad, 0xbe, 0xef],
    ] {
        let auditor = ConformanceAuditor::new(ReceiverProtocolMode::V3);
        auditor.record_message("receiver", &room_joined(sender, 1));
        assert!(panics(|| {
            auditor.record_binary_frame("receiver", raw);
        }));
    }
}

#[test]
fn conformance_binary_entrypoints_allow_rate_limited_advisory_after_exact_report() {
    let sender = id(95);
    for encoding in [
        GameDataEncoding::Json,
        GameDataEncoding::MessagePack,
        GameDataEncoding::Rkyv,
    ] {
        let auditor = ConformanceAuditor::new(ReceiverProtocolMode::V3);
        auditor.record_message("receiver", &room_joined(sender, 1));
        let mut counters = DeliveryCountersByClass::default();
        counters.reliable.unsupported_format = 1;
        auditor.record_message(
            "receiver",
            &delivery_report(
                counters,
                [DeliveryGap {
                    from_player: sender,
                    epoch: 1,
                    from_seq: 1,
                    to_seq: 1,
                    reason: DeliveryGapReason::UnsupportedFormat,
                }],
            ),
        );
        let fixture = RecordedBinaryGameData {
            from_player: sender,
            encoding,
            payload: vec![1],
            seq: 2,
            epoch: 1,
        };
        let wire = rmp_serde::to_vec_named(&fixture).expect("serialize binary frame");
        assert_eq!(
            auditor.record_binary_frame("receiver", &wire),
            fixture,
            "encoding={encoding:?}"
        );
        auditor.record_message("receiver", &format_error());
    }
}

#[test]
fn conformance_unsupported_advisory_requires_prior_report_but_not_adjacency() {
    let sender = id(96);
    let unmatched = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    unmatched.record_message("receiver", &room_joined(sender, 1));
    assert!(
        panics(|| unmatched.record_message("receiver", &format_error())),
        "an unsupported-format advisory cannot invent an omission"
    );

    let deferred = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    deferred.record_message("receiver", &room_joined(sender, 1));
    let mut counters = DeliveryCountersByClass::default();
    counters.reliable.unsupported_format = 1;
    deferred.record_message(
        "receiver",
        &delivery_report(
            counters,
            [DeliveryGap {
                from_player: sender,
                epoch: 1,
                from_seq: 1,
                to_seq: 1,
                reason: DeliveryGapReason::UnsupportedFormat,
            }],
        ),
    );
    deferred.record_message(
        "receiver",
        &ServerMessage::Error {
            message: "terminal delivery failure".to_string(),
            error_code: Some(ErrorCode::SlowConsumer),
        },
    );
    deferred.record_close("receiver", 4002, "slow_consumer");

    let room_reset = ConformanceAuditor::new(ReceiverProtocolMode::V3);
    room_reset.record_message("receiver", &room_joined(sender, 1));
    room_reset.record_message(
        "receiver",
        &delivery_report(
            counters,
            [DeliveryGap {
                from_player: sender,
                epoch: 1,
                from_seq: 1,
                to_seq: 1,
                reason: DeliveryGapReason::UnsupportedFormat,
            }],
        ),
    );
    room_reset.record_message("receiver", &ServerMessage::RoomLeft);
    assert!(
        panics(|| room_reset.record_message("receiver", &format_error())),
        "an advisory cannot use a report from a prior room lifecycle"
    );
}

#[test]
fn conformance_delivery_class_metrics_conserve_per_class() {
    fn valid_snapshot() -> DeliveryMetricsByClass {
        DeliveryMetricsByClass {
            reliable: DeliveryClassMetrics {
                attempted: 3,
                delivered: 1,
                abandoned: 1,
                unsupported_format: 1,
                ..Default::default()
            },
            latest: DeliveryClassMetrics {
                attempted: 5,
                delivered: 1,
                superseded: 1,
                dropped_full: 1,
                abandoned: 1,
                unsupported_format: 1,
                ..Default::default()
            },
            volatile: DeliveryClassMetrics {
                attempted: 4,
                delivered: 1,
                dropped: 1,
                abandoned: 1,
                unsupported_format: 1,
                ..Default::default()
            },
        }
    }

    assert_delivery_class_snapshot_conserves(valid_snapshot());

    type Mutate = fn(&mut DeliveryMetricsByClass);
    let invalid: &[(&str, Mutate)] = &[
        ("reliable deficit", |m| m.reliable.attempted += 1),
        ("latest surplus", |m| m.latest.delivered += 1),
        ("volatile deficit", |m| m.volatile.attempted += 1),
        ("reliable superseded", |m| m.reliable.superseded = 1),
        ("latest volatile drop", |m| m.latest.dropped = 1),
        ("volatile dropped-full", |m| m.volatile.dropped_full = 1),
    ];
    for (name, mutate) in invalid {
        let mut snapshot = valid_snapshot();
        mutate(&mut snapshot);
        assert!(
            panics(|| assert_delivery_class_snapshot_conserves(snapshot)),
            "case {name}"
        );
    }
}
