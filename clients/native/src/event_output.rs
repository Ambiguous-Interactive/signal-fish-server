//! Bounded JSONL output with writes outside the async executor.

use std::io::Write;
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};

use tokio::sync::Notify;

use crate::events::Event;

const MAX_PENDING_EVENTS: usize = 256;
const MAX_PENDING_BYTES: usize = 128 * 1024 * 1024;
const MAX_TERMINAL_BYTES: usize = 4096;

#[derive(Default)]
struct State {
    pending_events: usize,
    pending_bytes: usize,
    failure: Option<&'static str>,
    finishing: bool,
    writer_stopped: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Notify,
    flushed: Condvar,
}

impl Shared {
    fn fail(&self, reason: &'static str) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .failure
            .get_or_insert(reason);
        self.changed.notify_waiters();
    }
}

/// One bounded writer for the process event stream.
pub struct EventOutput {
    shared: Arc<Shared>,
    sender: mpsc::SyncSender<PendingOutput>,
    max_events: usize,
    max_bytes: usize,
}

struct PendingOutput {
    bytes: Vec<u8>,
    _receive_permit: Option<crate::engine::MessageQueuePermit>,
}

struct BoundedLine {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for BoundedLine {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len()
            > self
                .limit
                .saturating_sub(1)
                .saturating_sub(self.bytes.len())
        {
            return Err(std::io::Error::other("stdout event size limit exceeded"));
        }
        let required = self.bytes.len().saturating_add(bytes.len());
        if required >= self.bytes.capacity() {
            let capacity = required
                .saturating_add(1)
                .max(self.bytes.capacity().saturating_mul(2))
                .min(self.limit);
            self.bytes
                .reserve_exact(capacity.saturating_sub(self.bytes.len()));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn serialize(event: &Event, limit: usize) -> Result<Vec<u8>, serde_json::Error> {
    let mut line = BoundedLine {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut line, event)?;
    line.bytes.push(b'\n');
    Ok(line.bytes)
}

impl EventOutput {
    pub(crate) fn new(writer: impl Write + Send + 'static) -> Self {
        Self::with_limits(writer, MAX_PENDING_EVENTS, MAX_PENDING_BYTES)
    }

    fn with_limits(
        mut writer: impl Write + Send + 'static,
        max_events: usize,
        max_bytes: usize,
    ) -> Self {
        let shared = Arc::new(Shared::default());
        let (sender, receiver) = mpsc::sync_channel::<PendingOutput>(max_events.saturating_add(2));
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("event-output".into())
            .spawn(move || {
                while let Ok(line) = receiver.recv() {
                    let size = line.bytes.len();
                    let result = writer.write_all(&line.bytes).and_then(|()| writer.flush());
                    drop(line);
                    let mut state = worker_shared
                        .state
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    state.pending_events = state.pending_events.saturating_sub(1);
                    state.pending_bytes = state.pending_bytes.saturating_sub(size);
                    if result.is_err() {
                        drop(receiver);
                        state.failure.get_or_insert("stdout write failed");
                        state.writer_stopped = true;
                        state.pending_events = 0;
                        state.pending_bytes = 0;
                        drop(state);
                        worker_shared.changed.notify_waiters();
                        worker_shared.flushed.notify_all();
                        break;
                    }
                    drop(state);
                    worker_shared.changed.notify_waiters();
                    worker_shared.flushed.notify_all();
                }
            });
        if worker.is_err() {
            shared.fail("stdout writer could not start");
            shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .writer_stopped = true;
        }
        Self {
            shared,
            sender,
            max_events,
            max_bytes,
        }
    }

    fn enqueue(&self, state: &mut State, line: PendingOutput) {
        let size = line.bytes.len();
        if self.sender.try_send(line).is_ok() {
            state.pending_events = state.pending_events.saturating_add(1);
            state.pending_bytes = state.pending_bytes.saturating_add(size);
        } else {
            state.failure.get_or_insert("stdout writer unavailable");
            state.writer_stopped = true;
        }
    }

    /// Queue one event without blocking the executor. Overload fails the run.
    pub fn emit(&self, event: &Event) {
        self.emit_owned(event, None);
    }

    fn emit_owned(
        &self,
        event: &Event,
        mut receive_permit: Option<crate::engine::MessageQueuePermit>,
    ) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.failure.is_some() || state.finishing {
            return;
        }
        if state.pending_events >= self.max_events {
            state.failure = Some("stdout event backlog limit exceeded");
        } else {
            match serialize(event, self.max_bytes.saturating_sub(state.pending_bytes)) {
                Ok(bytes) => self.enqueue(
                    &mut state,
                    PendingOutput {
                        bytes,
                        _receive_permit: receive_permit.take(),
                    },
                ),
                Err(_) => {
                    state.failure = Some("stdout event byte limit exceeded");
                }
            }
        }
        drop(receive_permit);
        drop(state);
        self.shared.changed.notify_waiters();
    }

    /// Keep the receive charge until output is consumed or discarded.
    pub(crate) fn emit_with_permit(
        &self,
        event: &Event,
        permit: crate::engine::MessageQueuePermit,
    ) {
        self.emit_owned(event, Some(permit));
    }

    /// Return when required output fails or its queue fills.
    pub async fn failed(&self) -> &'static str {
        loop {
            let notified = self.shared.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let failure = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .failure;
            if let Some(reason) = failure {
                return reason;
            }
            notified.await;
        }
    }

    /// Queue the owner's terminal events once, using two reserved small slots.
    pub fn finish(&self, error: Option<&str>, code: i32) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.finishing {
            return;
        }
        state.finishing = true;
        if state.writer_stopped {
            return;
        }
        let events = error
            .map(|message| Event::Error {
                message: message.to_owned(),
            })
            .into_iter()
            .chain(std::iter::once(Event::Exiting { code }));
        for event in events {
            match serialize(&event, MAX_TERMINAL_BYTES) {
                Ok(bytes) => self.enqueue(
                    &mut state,
                    PendingOutput {
                        bytes,
                        _receive_permit: None,
                    },
                ),
                Err(_) => {
                    state
                        .failure
                        .get_or_insert("terminal stdout event size limit exceeded");
                }
            }
        }
        drop(state);
        self.shared.changed.notify_waiters();
    }

    /// Finish before a runtime exists, with at most one second to flush.
    pub fn finish_blocking(&self, error: &str, code: i32) {
        self.finish(Some(error), code);
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _result = self.shared.flushed.wait_timeout_while(
            state,
            std::time::Duration::from_secs(1),
            |state| state.pending_events != 0 && !state.writer_stopped,
        );
    }

    /// Wait until every admitted event has been written and flushed.
    pub async fn drained(&self) -> Result<(), &'static str> {
        loop {
            let notified = self.shared.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let (pending, failure, writer_stopped) = {
                let state = self
                    .shared
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                (state.pending_events, state.failure, state.writer_stopped)
            };
            if writer_stopped {
                return Err(failure.unwrap_or("stdout writer unavailable"));
            }
            if pending == 0 {
                return failure.map_or(Ok(()), Err);
            }
            notified.await;
        }
    }
}

/// Shared process output. A blocked writer never occupies a Tokio thread.
pub fn stdout() -> &'static EventOutput {
    static OUTPUT: OnceLock<EventOutput> = OnceLock::new();
    OUTPUT.get_or_init(|| EventOutput::new(std::io::stdout()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    struct HeldWriter {
        started: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for HeldWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if let Some(started) = self.started.take() {
                started.send(()).unwrap();
                self.release.recv().unwrap();
            }
            self.bytes.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn held_output(
        max_events: usize,
        max_bytes: usize,
    ) -> (
        EventOutput,
        mpsc::Receiver<()>,
        mpsc::Sender<()>,
        Arc<Mutex<Vec<u8>>>,
    ) {
        let (started, wait_started) = mpsc::channel();
        let (release, wait_release) = mpsc::channel();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        (
            EventOutput::with_limits(
                HeldWriter {
                    started: Some(started),
                    release: wait_release,
                    bytes: Arc::clone(&bytes),
                },
                max_events,
                max_bytes,
            ),
            wait_started,
            release,
            bytes,
        )
    }

    async fn drain(output: &EventOutput) {
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), output.drained())
            .await
            .unwrap();
        let failure = output.shared.state.lock().unwrap().failure;
        assert_eq!(result, failure.map_or(Ok(()), Err));
    }

    #[tokio::test]
    async fn in_flight_output_counts_toward_the_event_limit_and_terminal_slots_are_reserved() {
        let (output, started, release, bytes) = held_output(2, 1024);
        output.emit(&Event::Authenticated);
        started
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        output.emit(&Event::GameDataSent);
        assert_eq!(output.shared.state.lock().unwrap().pending_events, 2);
        output.emit(&Event::SuccessCriteriaMet);
        assert_eq!(output.failed().await, "stdout event backlog limit exceeded");
        output.finish(Some("stdout event backlog limit exceeded"), 3);
        output.finish(Some("duplicate"), 0);
        output.emit(&Event::SuccessCriteriaMet);
        assert_eq!(output.shared.state.lock().unwrap().pending_events, 4);
        release.send(()).unwrap();
        drain(&output).await;
        let lines: Vec<serde_json::Value> = String::from_utf8(bytes.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            lines
                .iter()
                .map(|line| line["event"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["authenticated", "game_data_sent", "error", "exiting"]
        );
        assert_eq!(lines[3]["code"], 3);
        assert_eq!(output.shared.state.lock().unwrap().pending_bytes, 0);
    }

    #[tokio::test]
    async fn advertised_256_event_limit_counts_the_in_flight_write() {
        let (output, started, release, _) = held_output(MAX_PENDING_EVENTS, MAX_PENDING_BYTES);
        output.emit(&Event::Authenticated);
        started
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        for _ in 1..256 {
            output.emit(&Event::Authenticated);
        }
        assert_eq!(output.shared.state.lock().unwrap().pending_events, 256);
        assert!(output.shared.state.lock().unwrap().failure.is_none());
        output.emit(&Event::Authenticated);
        assert_eq!(output.failed().await, "stdout event backlog limit exceeded");
        release.send(()).unwrap();
        drain(&output).await;
    }

    #[tokio::test]
    async fn serialized_byte_limit_includes_in_flight_unicode_escaping_and_newlines() {
        let event = Event::Error {
            message: "界\n\"".repeat(8),
        };
        let size = serialize(&event, 1024).unwrap().len();
        for extra in [0, 1] {
            let (output, started, release, bytes) = held_output(256, size * 2 - extra);
            output.emit(&event);
            started
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            output.emit(&event);
            let expected = if extra == 0 { size * 2 } else { size };
            assert_eq!(output.shared.state.lock().unwrap().pending_bytes, expected);
            if extra == 1 {
                assert_eq!(output.failed().await, "stdout event byte limit exceeded");
            }
            release.send(()).unwrap();
            drain(&output).await;
            assert_eq!(bytes.lock().unwrap().len(), expected);
        }
    }

    #[tokio::test]
    async fn flush_releases_output_capacity_for_later_events() {
        let (output, started, release, bytes) = held_output(1, 128);
        output.emit(&Event::Authenticated);
        started
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        release.send(()).unwrap();
        drain(&output).await;
        output.emit(&Event::GameDataSent);
        drain(&output).await;
        assert_eq!(
            bytes
                .lock()
                .unwrap()
                .iter()
                .filter(|&&byte| byte == b'\n')
                .count(),
            2
        );
        assert!(output.shared.state.lock().unwrap().failure.is_none());
    }

    #[tokio::test]
    async fn gameplay_receive_charge_survives_queued_output_until_flush() {
        let (output, started, release, _) = held_output(256, 128 * 1024 * 1024);
        let (permit, messages, bytes) =
            crate::engine::MessageQueuePermit::for_test_with_budget(4096);
        let event = Event::ChannelBinaryMessage {
            peer: signal_fish_server::protocol::PlayerId::nil(),
            generation: 7,
            label: "reliable".into(),
            data_base64: "x".repeat(5464),
        };
        output.emit_with_permit(&event, permit);
        started
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let charged = (messages.available_permits(), bytes.available_permits());
        release.send(()).unwrap();
        drain(&output).await;
        assert_eq!(
            charged,
            (255, 1024 * 1024 - 4096),
            "blocked stdout still owns the received gameplay"
        );
        assert_eq!(
            (messages.available_permits(), bytes.available_permits()),
            (256, 1024 * 1024)
        );
    }

    struct HeldFlush {
        started: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
    }

    impl Write for HeldFlush {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            if let Some(started) = self.started.take() {
                started.send(()).unwrap();
                self.release.recv().unwrap();
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn gameplay_charge_covers_in_flight_flush_and_waiting_text_rows() {
        let (started, waiting) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let output = EventOutput::new(HeldFlush {
            started: Some(started),
            release: released,
        });
        let mut budgets = Vec::new();
        for text in ["first 鱼", "second\n"] {
            let (permit, messages, bytes) =
                crate::engine::MessageQueuePermit::for_test_with_budget(text.len());
            output.emit_with_permit(
                &Event::ChannelMessage {
                    peer: signal_fish_server::protocol::PlayerId::nil(),
                    generation: 7,
                    label: "reliable".into(),
                    text: text.into(),
                },
                permit,
            );
            budgets.push((messages, bytes, text.len()));
        }
        waiting
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let held: Vec<_> = budgets
            .iter()
            .map(|(messages, bytes, _)| (messages.available_permits(), bytes.available_permits()))
            .collect();
        release.send(()).unwrap();
        drain(&output).await;
        for ((messages, bytes, size), charged) in budgets.iter().zip(held) {
            assert_eq!(charged, (255, 1024 * 1024 - size));
            assert_eq!(
                (messages.available_permits(), bytes.available_permits()),
                (256, 1024 * 1024)
            );
        }
    }

    #[tokio::test]
    async fn output_refusals_release_unqueued_gameplay_charges() {
        for refusal in ["event_limit", "byte_limit", "finishing"] {
            let (output, started, release, _) =
                held_output(if refusal == "event_limit" { 1 } else { 256 }, 1024);
            output.emit(&Event::Authenticated);
            started
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            if refusal == "finishing" {
                output.finish(None, 0);
            }
            let (permit, messages, bytes) =
                crate::engine::MessageQueuePermit::for_test_with_budget(4096);
            output.emit_with_permit(
                &Event::Error {
                    message: "x".repeat(2048),
                },
                permit,
            );
            assert_eq!(
                (messages.available_permits(), bytes.available_permits()),
                (256, 1024 * 1024),
                "{refusal}"
            );
            let (permit, messages, bytes) =
                crate::engine::MessageQueuePermit::for_test_with_budget(1);
            output.emit_with_permit(&Event::Authenticated, permit);
            assert_eq!(
                (messages.available_permits(), bytes.available_permits()),
                (256, 1024 * 1024),
                "previous refusal or finish must release later charges"
            );
            release.send(()).unwrap();
            drain(&output).await;
        }
    }

    #[tokio::test]
    async fn disconnected_output_receiver_releases_the_refused_receive_charge() {
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(receiver);
        let output = EventOutput {
            shared: Arc::new(Shared::default()),
            sender,
            max_events: 256,
            max_bytes: 1024,
        };
        let (permit, messages, bytes) =
            crate::engine::MessageQueuePermit::for_test_with_budget(4096);
        output.emit_with_permit(&Event::Authenticated, permit);
        assert_eq!(output.failed().await, "stdout writer unavailable");
        assert_eq!(output.drained().await, Err("stdout writer unavailable"));
        assert_eq!(
            (messages.available_permits(), bytes.available_permits()),
            (256, 1024 * 1024)
        );
    }

    struct FailedWriter {
        fail_flush: bool,
        release: mpsc::Receiver<()>,
    }
    impl Write for FailedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.release.recv().unwrap();
            if self.fail_flush {
                Ok(bytes.len())
            } else {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[tokio::test]
    async fn write_and_flush_failures_wake_all_waiters_and_forbid_success() {
        for fail_flush in [false, true] {
            use std::future::Future;
            let (release, ready) = mpsc::channel();
            let output = EventOutput::new(FailedWriter {
                fail_flush,
                release: ready,
            });
            let mut budgets = Vec::new();
            for _ in 0..2 {
                let (permit, messages, bytes) =
                    crate::engine::MessageQueuePermit::for_test_with_budget(4096);
                output.emit_with_permit(&Event::Authenticated, permit);
                budgets.push((messages, bytes));
            }
            let first = output.failed();
            let second = output.failed();
            tokio::pin!(first, second);
            std::future::poll_fn(|context| {
                assert!(first.as_mut().poll(context).is_pending());
                assert!(second.as_mut().poll(context).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            release.send(()).unwrap();
            let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::join!(first, second)
            })
            .await
            .unwrap();
            assert_eq!((a, b), ("stdout write failed", "stdout write failed"));
            assert_eq!(output.drained().await, Err("stdout write failed"));
            for (messages, bytes) in budgets {
                assert_eq!(
                    (messages.available_permits(), bytes.available_permits()),
                    (256, 1024 * 1024),
                    "failure notification follows queued and in-flight permit release"
                );
            }
            let (permit, messages, bytes) =
                crate::engine::MessageQueuePermit::for_test_with_budget(4096);
            output.emit_with_permit(&Event::SuccessCriteriaMet, permit);
            assert_eq!(
                (messages.available_permits(), bytes.available_permits()),
                (256, 1024 * 1024)
            );
            assert_eq!(output.shared.state.lock().unwrap().pending_events, 0);
        }
    }

    #[test]
    fn runtime_start_failure_flushes_its_terminal_events() {
        let (output, _started, release, bytes) = held_output(256, 128 * 1024 * 1024);
        release.send(()).unwrap();
        output.finish_blocking("runtime could not start", 4);
        let bytes = bytes.lock().unwrap();
        let lines: Vec<serde_json::Value> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["message"], "runtime could not start");
        assert_eq!(lines[1]["code"], 4);
    }

    #[test]
    fn bounded_serialization_refuses_oversized_events_without_a_partial_line() {
        let event = Event::Error {
            message: "界\n\"".repeat(32),
        };
        let line = serialize(&event, 4096).unwrap();
        assert_eq!(serialize(&event, line.len()).unwrap(), line);
        assert!(serialize(&event, line.len() - 1).is_err());
        assert!(serialize(&event, 0).is_err());
        assert_eq!(line.iter().filter(|&&byte| byte == b'\n').count(), 1);
    }
}
