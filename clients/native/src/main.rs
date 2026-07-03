//! `signal-fish-reference-native` binary: parse flags, run the client under
//! the hard watchdog, and emit the final `exiting` event.
//!
//! stdout is exclusively the JSONL event stream; ALL logging goes to stderr
//! (configure verbosity with `RUST_LOG`, default `info`).

use std::time::Duration;

use clap::Parser;
use signal_fish_reference_native::cli::{Cli, RuntimeFlavor};
use signal_fish_reference_native::client::{self, EXIT_HARD_TIMEOUT};
use signal_fish_reference_native::events::{emit, Event};

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
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_unset| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let runtime = match build_runtime(cli.runtime) {
        Ok(runtime) => runtime,
        Err(error) => {
            emit(&Event::Error {
                message: format!("failed to start tokio runtime: {error}"),
            });
            emit(&Event::Exiting {
                code: EXIT_HARD_TIMEOUT,
            });
            std::process::exit(EXIT_HARD_TIMEOUT);
        }
    };

    // The watchdog is the binary's no-hang guarantee: every await inside the
    // client is bounded by it, and expiry is a hard abort with exit code 4.
    let max_runtime = Duration::from_secs(cli.max_runtime_secs);
    let code = runtime.block_on(async {
        match tokio::time::timeout(max_runtime, client::run(&cli)).await {
            Ok(code) => code,
            Err(_elapsed) => {
                emit(&Event::Error {
                    message: format!(
                        "--max-runtime-secs ({}) watchdog fired; hard abort",
                        cli.max_runtime_secs
                    ),
                });
                EXIT_HARD_TIMEOUT
            }
        }
    });

    emit(&Event::Exiting { code });
    // Bounded teardown of any webrtc background tasks, then a deterministic
    // process exit (skipping destructors is fine after the explicit shutdown).
    runtime.shutdown_timeout(Duration::from_secs(2));
    std::process::exit(code);
}
