#![cfg_attr(not(test), deny(clippy::panic))]
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::module_name_repetitions,
    clippy::struct_excessive_bools,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::similar_names
)]

//! # Signal Fish Server
//!
//! A lightweight, in-memory WebSocket signaling server for peer-to-peer game networking.
//!
//! Zero external dependencies — no database, no cloud services.
//! Just run the binary and connect via WebSocket.

/// Public app-ID allowlist and per-label rate limiting.
pub mod auth;

/// Optimized broadcast message handling
pub mod broadcast;

/// Server configuration and environment variables
pub mod config;

/// Room and player coordination logic
pub mod coordination;

/// Database abstraction layer (in-memory implementation)
pub mod database;

/// Process-local locking behind a future coordination extension seam
pub mod distributed;

/// Structured logging configuration
pub mod logging;

/// Metrics collection and reporting
pub mod metrics;

/// WebSocket message protocol definitions
pub mod protocol;

/// Rate limiting implementation
pub mod rate_limit;

/// Reconnection token and state management
pub mod reconnection;

/// Retry logic utilities
pub mod retry;

/// Zero-copy serialization utilities
pub mod rkyv_utils;

/// TLS and crypto utilities
pub mod security;

/// Feature-gated delivery-contract trace capture for formal replay.
#[cfg(feature = "trace-validation")]
pub mod trace_validation;

/// Main server orchestration
pub mod server;

/// WebSocket connection handling
pub mod websocket;
