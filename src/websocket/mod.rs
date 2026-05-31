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
// Exposed so golden wire tests can freeze the real binary frame bytes produced
// by the production send path (see `sending::BinaryGameDataFrame`).
pub use metrics::{metrics_handler, prometheus_metrics_handler, MetricsQuery};
pub use routes::{create_router, run_server};
pub use sending::{encode_binary_game_data, BinaryGameDataFrame};
