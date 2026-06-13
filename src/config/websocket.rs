//! WebSocket configuration types.

use super::defaults::{
    default_auth_timeout_secs, default_batch_interval_ms, default_batch_size,
    default_enable_batching, default_idle_timeout_secs,
};
use serde::{Deserialize, Serialize};

/// WebSocket configuration.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct WebSocketConfig {
    /// Enable message batching for WebSocket connections
    #[serde(default = "default_enable_batching")]
    pub enable_batching: bool,
    /// Maximum number of messages to batch before flushing
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    /// Maximum time in milliseconds to wait before flushing batch
    #[serde(default = "default_batch_interval_ms")]
    pub batch_interval_ms: u64,
    /// Authentication timeout in seconds (time allowed for clients to authenticate)
    #[serde(default = "default_auth_timeout_secs")]
    pub auth_timeout_secs: u64,
    /// Post-authentication idle timeout in seconds; `0` disables the timeout.
    ///
    /// An authenticated connection that produces no inbound WebSocket frame of
    /// any kind (including Ping/Pong) for this long is closed (normal
    /// disconnect path, so the reconnection grace period still applies). The
    /// pre-auth handshake is bounded separately by
    /// [`auth_timeout_secs`](Self::auth_timeout_secs).
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            enable_batching: default_enable_batching(),
            batch_size: default_batch_size(),
            batch_interval_ms: default_batch_interval_ms(),
            auth_timeout_secs: default_auth_timeout_secs(),
            idle_timeout_secs: default_idle_timeout_secs(),
        }
    }
}

impl WebSocketConfig {
    /// Validate WebSocket configuration
    ///
    /// `idle_timeout_secs` is deliberately unconstrained: `0` disables the
    /// post-auth idle timeout, and any positive value is a valid operator
    /// choice (aggressive timeouts are useful for tests and hardened
    /// deployments alike).
    pub fn validate(&self) -> anyhow::Result<()> {
        // Validate auth timeout: must be between 5 and 60 seconds
        if self.auth_timeout_secs < 5 {
            anyhow::bail!(
                "websocket.auth_timeout_secs must be at least 5 seconds (configured: {})",
                self.auth_timeout_secs
            );
        }
        if self.auth_timeout_secs > 60 {
            anyhow::bail!(
                "websocket.auth_timeout_secs must not exceed 60 seconds (configured: {})",
                self.auth_timeout_secs
            );
        }
        // When batching is enabled, `batch_interval_ms` is the flush
        // `tokio::time::interval` period; a zero period panics that timer (and
        // the per-connection send task with it), so reject it at startup. With
        // batching disabled the interval is never constructed, so any value is
        // fine.
        if self.enable_batching && self.batch_interval_ms == 0 {
            anyhow::bail!(
                "websocket.batch_interval_ms must be greater than 0 when websocket.enable_batching \
                 is true (it is the batch-flush interval, which cannot be zero)"
            );
        }
        Ok(())
    }
}
