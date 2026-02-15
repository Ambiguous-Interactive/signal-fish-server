use tracing_subscriber::{fmt::time::UtcTime, layer::Identity, prelude::*};

use crate::config::{LogFormat, LoggingConfig};

/// Initialize logging: console + rolling file appender (buffered), configurable via config file.
/// Notes:
/// - If logging.level is provided in config, it is used; otherwise RUST_LOG env var is used; fallback is "info".
pub fn init_with_config(cfg: &LoggingConfig) {
    // Choose filter: config level > env var > default "info"
    let env_filter = if let Some(level) = &cfg.level {
        // Convert enum to a simple level directive string for EnvFilter
        tracing_subscriber::EnvFilter::new(level.as_str())
    } else {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
    };

    match cfg.format {
        LogFormat::Json => init_json_logging(cfg, env_filter),
        LogFormat::Text => init_text_logging(cfg, env_filter),
    }
}

fn init_json_logging(cfg: &LoggingConfig, env_filter: tracing_subscriber::EnvFilter) {
    let registry = tracing_subscriber::registry().with(env_filter).with(
        tracing_subscriber::fmt::layer()
            .json()
            .with_ansi(false)
            .with_timer(UtcTime::rfc_3339())
            .with_writer(std::io::stdout),
    );

    if cfg.enable_file_logging {
        if let Some(file_layer) = build_file_layer(cfg, |writer| {
            tracing_subscriber::fmt::layer()
                .json()
                .with_ansi(false)
                .with_timer(UtcTime::rfc_3339())
                .with_writer(writer)
        }) {
            let subscriber = registry.with(file_layer);
            let _ = subscriber.try_init();
            return;
        }
    }

    let _ = registry.with(Identity::new()).try_init();
}

fn init_text_logging(cfg: &LoggingConfig, env_filter: tracing_subscriber::EnvFilter) {
    let registry = tracing_subscriber::registry().with(env_filter).with(
        tracing_subscriber::fmt::layer()
            .with_ansi(true)
            .with_timer(UtcTime::rfc_3339())
            .with_writer(std::io::stdout),
    );

    if cfg.enable_file_logging {
        if let Some(file_layer) = build_file_layer(cfg, |writer| {
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_timer(UtcTime::rfc_3339())
                .with_writer(writer)
        }) {
            let subscriber = registry.with(file_layer);
            let _ = subscriber.try_init();
            return;
        }
    }

    let _ = registry.with(Identity::new()).try_init();
}

fn build_file_layer<F, L>(cfg: &LoggingConfig, build_layer: F) -> Option<L>
where
    F: FnOnce(tracing_appender::non_blocking::NonBlocking) -> L,
{
    let rotation = match cfg.rotation.to_lowercase().as_str() {
        "hourly" => tracing_appender::rolling::Rotation::HOURLY,
        "never" => tracing_appender::rolling::Rotation::NEVER,
        _ => tracing_appender::rolling::Rotation::DAILY,
    };

    if std::fs::create_dir_all(&cfg.dir).is_err() {
        eprintln!(
            "Failed to create log directory '{}', continuing with stdout logs",
            cfg.dir
        );
        return None;
    }

    let file_appender =
        tracing_appender::rolling::RollingFileAppender::new(rotation, &cfg.dir, &cfg.filename);
    let (non_blocking, file_guard) = tracing_appender::non_blocking(file_appender);

    // Keep guard alive for process lifetime
    let _leaked: &'static _ = Box::leak(Box::new(file_guard));

    Some(build_layer(non_blocking))
}
