// WebSocket module - organized into focused submodules
//
// This module provides the WebSocket handler and HTTP endpoints for the signaling server.
// It is organized as follows:
//
// - handler: WebSocket upgrade handler (entry point)
// - connection: Main WebSocket connection handling logic
// - batching: Message batching for performance optimization
// - sending: Message serialization and sending functions
// - token_binding: Token binding security features
// - routes: HTTP route setup (health, metrics, etc.)
// - metrics: Metrics endpoints and authentication
// - prometheus: Prometheus metrics rendering

use std::time::Duration;

mod batching;
mod connection;
mod handler;
mod metrics;
mod prometheus;
mod routes;
mod sending;
mod token_binding;

pub(crate) use sending::RelayFrameCache;

/// Narrow, dev-only exports used by the relay serialization benchmarks.
///
/// The benchmark drives the production projector immediately upstream of the
/// socket write. It receives the exact Axum frame plus explicit codec work
/// counters, avoiding a benchmark-only serialization implementation.
#[cfg(feature = "allocation-tracking")]
#[doc(hidden)]
pub mod allocation_benchmark {
    use super::sending::{
        materialize_game_data_frame, preflight_binary_fallback, GameDataMaterializationError,
    };
    use crate::coordination::allocation_benchmark::DeliveryMessage;
    use crate::protocol::GameDataEncoding;
    use axum::extract::ws::Message;

    pub struct MaterializedFrame {
        pub frame: Message,
        pub json_encodes: u64,
        pub message_pack_encodes: u64,
        pub message_pack_decodes: u64,
    }

    pub fn materialize_game_data(
        delivery: &DeliveryMessage,
        recipient_supports_v3: bool,
        recipient_format: GameDataEncoding,
    ) -> Result<MaterializedFrame, String> {
        let message = delivery.message();
        let relay_cache = delivery.relay_frame_cache();
        let preflight = preflight_binary_fallback(message, recipient_format, relay_cache);
        let materialized = materialize_game_data_frame(
            message,
            recipient_supports_v3,
            recipient_format,
            preflight,
            relay_cache,
        )
        .map_err(|error| match error {
            GameDataMaterializationError::InvalidV3Stamp => {
                "protocol-v3 binary game data lacked a complete stamp".to_string()
            }
            GameDataMaterializationError::Serialization(error)
            | GameDataMaterializationError::Undeliverable(error) => error,
        })?;
        Ok(MaterializedFrame {
            frame: materialized.frame,
            json_encodes: materialized.work.json_encodes,
            message_pack_encodes: materialized.work.message_pack_encodes,
            message_pack_decodes: materialized.work.message_pack_decodes,
        })
    }
}

// Re-export public API to maintain backward compatibility
pub use handler::{websocket_handler, websocket_handler_v3};
pub use metrics::{metrics_handler, prometheus_metrics_handler, MetricsQuery};
#[cfg(feature = "tls")]
pub use routes::ConfiguredAcceptor;
pub use routes::{
    bind_serve_listener, bind_tcp_listener, create_router, create_standalone_router, run_server,
};

/// Upper bound on each best-effort WebSocket close-path write.
///
/// Closing a registered socket can spend this budget once flushing queued
/// messages, once writing the semantic close frame, and once driving the sink
/// close handshake.
pub const CONNECTION_CLOSE_WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// Number of sequential close-write budgets in registered socket shutdown.
pub const REGISTERED_SHUTDOWN_CLOSE_WRITE_STEPS: u32 =
    connection::REGISTERED_SHUTDOWN_CLOSE_WRITE_STEPS;

/// Scheduling margin after the final registered close-write budget expires.
///
/// The socket-task guard is released only after the timed operation returns
/// and its parent handler completes. Waiting for exactly the sum of the write
/// budgets races that cleanup at the same Tokio deadline, especially under
/// sanitizer instrumentation.
pub const REGISTERED_SHUTDOWN_SETTLE_MARGIN: Duration = Duration::from_secs(1);

/// Process-level wait needed to let registered socket handlers finish shutdown.
pub fn registered_connection_shutdown_settle_timeout() -> Duration {
    CONNECTION_CLOSE_WRITE_TIMEOUT
        .saturating_mul(REGISTERED_SHUTDOWN_CLOSE_WRITE_STEPS)
        .saturating_add(REGISTERED_SHUTDOWN_SETTLE_MARGIN)
}
