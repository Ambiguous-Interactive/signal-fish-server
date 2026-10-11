//! `signal-fish-reference-native` binary: parse flags, run the client under
//! the configured watchdog, and emit the final `exiting` event.
//!
//! stdout is exclusively the JSONL event stream; ALL logging goes to stderr
//! (configure verbosity with `RUST_LOG`, default `info`).

use std::time::Duration;

use clap::Parser;
use signal_fish_reference_native::cli::{Cli, RuntimeFlavor};
use signal_fish_reference_native::client::{self, EXIT_HARD_TIMEOUT};
use signal_fish_reference_native::deadline;
use signal_fish_reference_native::diagnostic_output::{BoundedFields, DiagnosticOutput};
use signal_fish_reference_native::event_output;

/// Build the tokio runtime per `--runtime`: the default multi-threaded
/// flavor, or a single current-thread runtime for the starved-runtime
/// conformance matrix (one executor thread that `--tick-stall-ms` can
/// deliberately block).
fn build_runtime(flavor: RuntimeFlavor) -> std::io::Result<tokio::runtime::Runtime> {
    match flavor {
        RuntimeFlavor::Multi => tokio::runtime::Runtime::new(),
        RuntimeFlavor::Current => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build(),
    }
}

fn main() {
    let diagnostics = DiagnosticOutput::stderr();
    let panic_diagnostics = diagnostics.clone();
    std::panic::set_hook(Box::new(move |info| {
        panic_diagnostics.emit(format_args!("{info}"))
    }));
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            if error.use_stderr() {
                diagnostics.emit(format_args!("{error}"));
                diagnostics.finish();
            } else {
                let _result = error.print();
            }
            std::process::exit(error.exit_code());
        }
    };

    tracing_subscriber::fmt()
        .log_internal_errors(false)
        .fmt_fields(BoundedFields)
        .event_format(diagnostics.clone())
        .with_writer(std::io::sink)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_unset| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init()
        .unwrap_or_else(|error| {
            diagnostics.emit(format_args!("failed to start diagnostics: {error}"))
        });

    let runtime = match build_runtime(cli.runtime) {
        Ok(runtime) => runtime,
        Err(error) => {
            event_output::stdout().finish_blocking(
                &format!("failed to start tokio runtime: {error}"),
                EXIT_HARD_TIMEOUT,
            );
            diagnostics.finish();
            std::process::exit(EXIT_HARD_TIMEOUT);
        }
    };

    // A representable watchdog deadline bounds every await inside the client,
    // and expiry is a hard abort with exit code 4. A duration beyond Tokio's
    // Instant range intentionally remains beyond this process's lifetime.
    let max_runtime = Duration::from_secs(cli.max_runtime_secs);
    let output = event_output::stdout();
    let code = runtime.block_on(async {
        let (mut code, error) = tokio::select! {
            reason = output.failed() => (client::EXIT_CONNECTION_ERROR, Some(reason.to_owned())),
            result = deadline::timeout(max_runtime, client::run(&cli)) => match result {
                Ok(code) => (code, None),
                Err(_) => (EXIT_HARD_TIMEOUT, Some(format!("--max-runtime-secs ({}) watchdog fired; hard abort", cli.max_runtime_secs))),
            },
        };
        if code != EXIT_HARD_TIMEOUT {
            if let Err(reason) = tokio::time::timeout(Duration::from_secs(1), output.drained()).await.unwrap_or(Err("stdout flush timed out")) {
                code = client::EXIT_CONNECTION_ERROR;
                output.finish(Some(reason), code);
            }
        }
        output.finish(error.as_deref(), code);
        if !matches!(tokio::time::timeout(Duration::from_secs(1), output.drained()).await, Ok(Ok(()))) && code != EXIT_HARD_TIMEOUT {
            code = client::EXIT_CONNECTION_ERROR;
        }
        code
    });

    // Bounded teardown of any webrtc background tasks, then a deterministic
    // process exit (skipping destructors is fine after the explicit shutdown).
    runtime.shutdown_timeout(Duration::from_secs(2));
    diagnostics.finish();
    std::process::exit(code);
}
