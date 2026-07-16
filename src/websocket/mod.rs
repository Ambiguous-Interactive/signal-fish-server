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

// Re-export public API to maintain backward compatibility
pub use handler::{websocket_handler, websocket_handler_v3};
pub use metrics::{metrics_handler, prometheus_metrics_handler, MetricsQuery};
pub use routes::{bind_tcp_listener, create_router, create_standalone_router, run_server};

/// Upper bound on each best-effort WebSocket close-path write.
///
/// Closing a registered socket can spend this budget once flushing queued
/// messages, once writing the semantic close frame, and once driving the sink
/// close handshake.
pub const CONNECTION_CLOSE_WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// Number of sequential close-write budgets in registered socket shutdown.
pub const REGISTERED_SHUTDOWN_CLOSE_WRITE_STEPS: u32 =
    connection::REGISTERED_SHUTDOWN_CLOSE_WRITE_STEPS;

/// Process-level wait needed to let registered socket handlers finish shutdown.
pub fn registered_connection_shutdown_settle_timeout() -> Duration {
    CONNECTION_CLOSE_WRITE_TIMEOUT.saturating_mul(REGISTERED_SHUTDOWN_CLOSE_WRITE_STEPS)
}
