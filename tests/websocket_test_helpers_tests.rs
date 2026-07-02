mod websocket_test_helpers;

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::{Stream, StreamExt};
use signal_fish_server::protocol::ServerMessage;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
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
