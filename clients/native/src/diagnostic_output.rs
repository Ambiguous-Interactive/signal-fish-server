//! Bounded, best-effort diagnostics with no pipe writes on executor threads.

use std::fmt::{self, Write as _};
use std::io::Write;
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::Duration;

use tracing_subscriber::fmt::{format, FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

const MAX_RECORDS: usize = 256;
const MAX_RECORD_BYTES: usize = 4096;
const TRUNCATED: &str = " [truncated]\n";

/// Limit cached span fields, including repeated updates, before allocation.
pub struct BoundedFields;

struct FieldWriter<'writer, 'borrow> {
    writer: &'borrow mut format::Writer<'writer>,
    remaining: usize,
}

impl fmt::Write for FieldWriter<'_, '_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut end = self.remaining.min(text.len());
        while !text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        if let Some(prefix) = text.get(..end) {
            self.writer.write_str(prefix)?;
        }
        self.remaining = self.remaining.saturating_sub(end);
        if end < text.len() {
            self.remaining = 0;
        }
        Ok(())
    }
}

fn format_fields<R: tracing_subscriber::field::RecordFields>(
    mut writer: format::Writer<'_>,
    fields: R,
    remaining: usize,
) -> fmt::Result {
    let mut bounded = FieldWriter {
        writer: &mut writer,
        remaining,
    };
    format::DefaultFields::new().format_fields(format::Writer::new(&mut bounded), fields)
}

impl<'writer> FormatFields<'writer> for BoundedFields {
    fn format_fields<R: tracing_subscriber::field::RecordFields>(
        &self,
        writer: format::Writer<'writer>,
        fields: R,
    ) -> fmt::Result {
        format_fields(writer, fields, MAX_RECORD_BYTES)
    }

    fn add_fields(
        &self,
        current: &'writer mut tracing_subscriber::fmt::FormattedFields<Self>,
        fields: &tracing::span::Record<'_>,
    ) -> fmt::Result {
        if current.fields.len() >= MAX_RECORD_BYTES {
            return Ok(());
        }
        if !current.fields.is_empty() {
            current.fields.push(' ');
        }
        let remaining = MAX_RECORD_BYTES.saturating_sub(current.fields.len());
        format_fields(current.as_writer(), fields, remaining)
    }
}

#[derive(Default)]
struct State {
    pending: usize,
    dropped: u64,
    stopped: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    flushed: Condvar,
}

/// Diagnostic record admission includes formatting, queued and in-flight work.
#[derive(Clone)]
pub struct DiagnosticOutput {
    shared: Arc<Shared>,
    sender: mpsc::SyncSender<String>,
}

impl DiagnosticOutput {
    /// Start the process's single diagnostic worker.
    pub fn stderr() -> Self {
        Self::new(std::io::stderr())
    }

    fn new(mut writer: impl Write + Send + 'static) -> Self {
        let shared = Arc::new(Shared::default());
        let (sender, receiver) = mpsc::sync_channel::<String>(MAX_RECORDS);
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("diagnostic-output".into())
            .spawn(move || {
                for record in &receiver {
                    let result = writer
                        .write_all(record.as_bytes())
                        .and_then(|()| writer.flush());
                    drop(record);
                    let mut state = worker_shared
                        .state
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    state.pending = state.pending.saturating_sub(1);
                    if result.is_err() {
                        state.stopped = true;
                        state.pending = 0;
                    }
                    worker_shared.flushed.notify_all();
                    if result.is_err() {
                        break;
                    }
                }
                let mut state = worker_shared
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                state.stopped = true;
                worker_shared.flushed.notify_all();
            });
        if worker.is_err() {
            shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .stopped = true;
        }
        Self { shared, sender }
    }

    fn record(&self) -> Option<Record> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.stopped || state.pending >= MAX_RECORDS {
            state.dropped = state.dropped.saturating_add(1);
            return None;
        }
        state.pending = state.pending.saturating_add(1);
        Some(Record {
            output: self.clone(),
            text: String::with_capacity(MAX_RECORD_BYTES),
            truncated: false,
            trailing_newline: false,
        })
    }

    /// Queue a bounded diagnostic without allocating an unbounded formatted string.
    pub fn emit(&self, message: fmt::Arguments<'_>) {
        if let Some(mut record) = self.record() {
            let _result = record.write_fmt(message);
        }
    }

    /// Give healthy diagnostics at most one second to drain outside Tokio.
    pub fn finish(&self) {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _result =
            self.shared
                .flushed
                .wait_timeout_while(state, Duration::from_secs(1), |state| {
                    state.pending != 0 && !state.stopped
                });
    }
}

struct Record {
    output: DiagnosticOutput,
    text: String,
    truncated: bool,
    trailing_newline: bool,
}

impl fmt::Write for Record {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if self.truncated {
            return Ok(());
        }
        for character in text.chars() {
            if self.trailing_newline {
                self.trailing_newline = false;
                if self.text.len().saturating_add(2)
                    > MAX_RECORD_BYTES.saturating_sub(TRUNCATED.len())
                {
                    self.truncated = true;
                    break;
                }
                self.text.push_str("\\n");
            }
            if character == '\n' {
                self.trailing_newline = true;
                continue;
            }
            let mut buffer = [0; 4];
            let piece = match character {
                '\r' => "\\r",
                _ => character.encode_utf8(&mut buffer),
            };
            if piece.len()
                > MAX_RECORD_BYTES
                    .saturating_sub(TRUNCATED.len())
                    .saturating_sub(self.text.len())
            {
                self.truncated = true;
                break;
            }
            self.text.push_str(piece);
        }
        Ok(())
    }
}

impl Drop for Record {
    fn drop(&mut self) {
        if self.truncated {
            self.text.push_str(TRUNCATED);
        } else {
            self.text.push('\n');
        }
        let mut state = self
            .output
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.stopped
            || self
                .output
                .sender
                .try_send(std::mem::take(&mut self.text))
                .is_err()
        {
            state.pending = state.pending.saturating_sub(1);
            state.dropped = state.dropped.saturating_add(1);
            self.output.shared.flushed.notify_all();
        }
    }
}

/* The default tracing formatter builds an unbounded thread-local String before
MakeWriter runs. Format directly into admitted bounded records instead. */
impl<S, N> FormatEvent<S, N> for DiagnosticOutput
where
    S: tracing::Subscriber + for<'lookup> LookupSpan<'lookup>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        context: &FmtContext<'_, S, N>,
        _writer: format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> fmt::Result {
        if let Some(mut record) = self.record() {
            format().with_ansi(false).format_event(
                context,
                format::Writer::new(&mut record),
                event,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct HeldWriter {
        started: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for HeldWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.started.send(()).unwrap();
            self.release.recv().unwrap();
            self.bytes.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn admission_counts_formatting_queue_and_in_flight_and_releases_capacity() {
        let (started, wait_started) = mpsc::channel();
        let (release, wait_release) = mpsc::channel();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let output = DiagnosticOutput::new(HeldWriter {
            started,
            release: wait_release,
            bytes: Arc::clone(&bytes),
        });
        output.emit(format_args!("first"));
        wait_started.recv_timeout(Duration::from_secs(5)).unwrap();
        let records: Vec<_> = (1..MAX_RECORDS).map(|_| output.record().unwrap()).collect();
        assert_eq!(output.shared.state.lock().unwrap().pending, MAX_RECORDS);
        output.emit(format_args!("discarded"));
        assert_eq!(output.shared.state.lock().unwrap().dropped, 1);
        drop(records);
        output.emit(format_args!("still discarded"));
        assert_eq!(output.shared.state.lock().unwrap().dropped, 2);
        for _ in 0..MAX_RECORDS {
            release.send(()).unwrap();
        }
        output.finish();
        assert_eq!(output.shared.state.lock().unwrap().pending, 0);
        output.emit(format_args!("after drain"));
        release.send(()).unwrap();
        output.finish();
        let bytes = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(bytes.starts_with("first\n"));
        assert!(bytes.ends_with("after drain\n"));
        assert!(!bytes.contains("discarded"));
    }

    #[test]
    fn record_limits_keep_utf8_and_one_line_without_unbounded_formatting() {
        let output = DiagnosticOutput::new(std::io::sink());
        for text in [
            "",
            "normal",
            "界\n\r",
            "界".repeat(MAX_RECORD_BYTES).as_str(),
        ] {
            let mut record = output.record().unwrap();
            record.write_str(text).unwrap();
            assert!(record.text.len() <= MAX_RECORD_BYTES - TRUNCATED.len());
            assert!(record.text.capacity() <= MAX_RECORD_BYTES);
            assert!(!record.text.contains(['\n', '\r']));
            let length = record.text.len();
            if record.truncated {
                record.write_str("must not grow").unwrap();
                assert_eq!(record.text.len(), length);
            }
        }
        output.finish();
    }

    #[test]
    fn tracing_and_direct_diagnostics_flush_complete_bounded_records() {
        #[derive(Clone)]
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let output = DiagnosticOutput::new(Capture(Arc::clone(&bytes)));
        let subscriber = tracing_subscriber::fmt()
            .log_internal_errors(false)
            .fmt_fields(BoundedFields)
            .event_format(output.clone())
            .with_writer(std::io::sink)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(message = %"界".repeat(100_000), "fatal failure");
            tracing::info!("healthy diagnostic");
        });
        output.emit(format_args!("panic\nnext line"));
        output.finish();
        let bytes = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        let lines: Vec<_> = bytes.split_inclusive('\n').collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].ends_with(TRUNCATED));
        assert!(lines[0].len() <= MAX_RECORD_BYTES);
        assert!(lines[1].contains("healthy diagnostic"));
        assert_eq!(lines[2], "panic\\nnext line\n");
    }

    #[test]
    fn span_field_updates_cannot_grow_cached_diagnostics_past_the_limit() {
        use tracing_subscriber::fmt::FormattedFields;
        let subscriber = Arc::new(
            tracing_subscriber::fmt()
                .fmt_fields(BoundedFields)
                .with_writer(std::io::sink)
                .finish(),
        );
        tracing::subscriber::with_default(Arc::clone(&subscriber), || {
            let span = tracing::info_span!("bounded", remote = tracing::field::Empty);
            for _ in 0..1024 {
                span.record("remote", tracing::field::display("界".repeat(4096)));
            }
            span.with_subscriber(|(id, _dispatch)| {
                let span = subscriber.span(id).unwrap();
                let extensions = span.extensions();
                let fields = extensions.get::<FormattedFields<BoundedFields>>().unwrap();
                assert!(fields.fields.len() <= MAX_RECORD_BYTES);
                assert!(std::str::from_utf8(fields.fields.as_bytes()).is_ok());
            });
        });
    }

    #[test]
    fn failed_writer_stops_admission_and_shutdown_without_recursive_diagnostics() {
        struct FailedWriter;
        impl Write for FailedWriter {
            fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let output = DiagnosticOutput::new(FailedWriter);
        output.emit(format_args!("broken pipe"));
        output.finish();
        assert!(output.shared.state.lock().unwrap().stopped);
        output.emit(format_args!("discarded"));
        assert_eq!(output.shared.state.lock().unwrap().dropped, 1);
        assert_eq!(output.shared.state.lock().unwrap().pending, 0);
    }

    #[test]
    fn dropping_the_last_producer_releases_the_healthy_worker_and_its_writer() {
        struct WriterOnDrop(mpsc::Sender<()>);
        impl Write for WriterOnDrop {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl Drop for WriterOnDrop {
            fn drop(&mut self) {
                self.0.send(()).unwrap();
            }
        }
        let (released, wait_released) = mpsc::channel();
        let output = DiagnosticOutput::new(WriterOnDrop(released));
        output.emit(format_args!("last record"));
        output.finish();
        drop(output);
        wait_released.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}
