//! WebSocket configuration types.

use super::defaults::{
    default_auth_timeout_secs, default_batch_interval_ms, default_batch_size,
    default_enable_batching,
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
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            enable_batching: default_enable_batching(),
            batch_size: default_batch_size(),
            batch_interval_ms: default_batch_interval_ms(),
            auth_timeout_secs: default_auth_timeout_secs(),
        }
    }
}

impl WebSocketConfig {
    /// Validate WebSocket configuration
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
        Ok(())
    }
}
