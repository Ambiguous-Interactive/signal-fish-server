//! WebSocket configuration types.

use super::defaults::{
    default_auth_timeout_secs, default_batch_interval_ms, default_batch_size,
    default_delivery_stats_interval_secs, default_enable_batching, default_idle_timeout_secs,
    default_send_queue_capacity, default_slow_consumer_timeout_ms,
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
    /// Per-connection outbound message queue capacity.
    ///
    /// Bounds how many undelivered server messages may queue for one
    /// connection before delivery applies backpressure to senders. Larger
    /// values absorb bigger relay bursts without slowing senders; the cost is
    /// only pointer-sized per slot until messages actually queue.
    #[serde(default = "default_send_queue_capacity")]
    pub send_queue_capacity: usize,
    /// How long (milliseconds) delivery may wait for space in a full outbound
    /// queue before the recipient is disconnected as a slow consumer.
    ///
    /// This is the loud alternative to silently dropping messages: a
    /// connection that cannot absorb traffic for this long (on top of the
    /// buffering provided by [`send_queue_capacity`](Self::send_queue_capacity))
    /// is closed with a best-effort `SLOW_CONSUMER` error, and the room is
    /// notified through the normal disconnect flow.
    #[serde(default = "default_slow_consumer_timeout_ms")]
    pub slow_consumer_timeout_ms: u64,
    /// How often (seconds) each connection that negotiated protocol v3+ is
    /// sent a `RelayStats` frame with its cumulative delivery statistics
    /// (`sent_to_you` / `dropped_for_you` / `backpressure_events`); `0`
    /// (the default) disables emission entirely.
    ///
    /// Pre-v3 recipients never receive the frame regardless of this setting
    /// (the version gate is enforced at emission). Must be at most `3600`.
    #[serde(default = "default_delivery_stats_interval_secs")]
    pub delivery_stats_interval_secs: u64,
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            enable_batching: default_enable_batching(),
            batch_size: default_batch_size(),
            batch_interval_ms: default_batch_interval_ms(),
            auth_timeout_secs: default_auth_timeout_secs(),
            idle_timeout_secs: default_idle_timeout_secs(),
            send_queue_capacity: default_send_queue_capacity(),
            slow_consumer_timeout_ms: default_slow_consumer_timeout_ms(),
            delivery_stats_interval_secs: default_delivery_stats_interval_secs(),
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
        // A zero-capacity queue cannot accept any message (`mpsc::channel`
        // panics on 0), and delivery semantics require at least one slot of
        // real buffering per connection.
        if self.send_queue_capacity == 0 {
            anyhow::bail!(
                "websocket.send_queue_capacity must be at least 1 (configured: 0); \
                 it bounds the per-connection outbound message queue"
            );
        }
        // A zero timeout would disconnect a peer the instant its queue fills,
        // turning routine bursts into disconnect storms; a multi-minute value
        // lets one dead connection stall room senders far past any useful
        // recovery window. Bound it to a sane operational range.
        if self.slow_consumer_timeout_ms == 0 {
            anyhow::bail!(
                "websocket.slow_consumer_timeout_ms must be greater than 0; \
                 it is the grace period before a backpressured connection is disconnected"
            );
        }
        if self.slow_consumer_timeout_ms > 600_000 {
            anyhow::bail!(
                "websocket.slow_consumer_timeout_ms must not exceed 600000 (10 minutes); \
                 configured: {}",
                self.slow_consumer_timeout_ms
            );
        }
        // `0` disables RelayStats emission; anything beyond an hour is
        // indistinguishable from disabled and almost certainly a typo (e.g.
        // milliseconds pasted into a seconds field).
        if self.delivery_stats_interval_secs > 3_600 {
            anyhow::bail!(
                "websocket.delivery_stats_interval_secs must not exceed 3600 (1 hour); \
                 configured: {} (0 disables RelayStats emission)",
                self.delivery_stats_interval_secs
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Data-driven validation coverage: each case mutates one field on an
    /// otherwise-default config and states whether validation must accept it
    /// (plus a substring the rejection message must contain, so error text
    /// stays actionable).
    #[test]
    fn validate_accepts_and_rejects_expected_configurations() {
        struct Case {
            name: &'static str,
            mutate: fn(&mut WebSocketConfig),
            expect_ok: bool,
            expect_error_containing: &'static str,
        }

        let cases = [
            Case {
                name: "defaults are valid",
                mutate: |_config| {},
                expect_ok: true,
                expect_error_containing: "",
            },
            Case {
                name: "auth timeout below floor",
                mutate: |config| config.auth_timeout_secs = 4,
                expect_ok: false,
                expect_error_containing: "auth_timeout_secs must be at least 5",
            },
            Case {
                name: "auth timeout above ceiling",
                mutate: |config| config.auth_timeout_secs = 61,
                expect_ok: false,
                expect_error_containing: "auth_timeout_secs must not exceed 60",
            },
            Case {
                name: "zero batch interval rejected only while batching",
                mutate: |config| {
                    config.enable_batching = true;
                    config.batch_interval_ms = 0;
                },
                expect_ok: false,
                expect_error_containing: "batch_interval_ms must be greater than 0",
            },
            Case {
                name: "zero batch interval accepted when batching disabled",
                mutate: |config| {
                    config.enable_batching = false;
                    config.batch_interval_ms = 0;
                },
                expect_ok: true,
                expect_error_containing: "",
            },
            Case {
                name: "zero send queue capacity",
                mutate: |config| config.send_queue_capacity = 0,
                expect_ok: false,
                expect_error_containing: "send_queue_capacity must be at least 1",
            },
            Case {
                name: "single-slot send queue is the floor",
                mutate: |config| config.send_queue_capacity = 1,
                expect_ok: true,
                expect_error_containing: "",
            },
            Case {
                name: "zero slow-consumer timeout",
                mutate: |config| config.slow_consumer_timeout_ms = 0,
                expect_ok: false,
                expect_error_containing: "slow_consumer_timeout_ms must be greater than 0",
            },
            Case {
                name: "slow-consumer timeout at ceiling",
                mutate: |config| config.slow_consumer_timeout_ms = 600_000,
                expect_ok: true,
                expect_error_containing: "",
            },
            Case {
                name: "slow-consumer timeout above ceiling",
                mutate: |config| config.slow_consumer_timeout_ms = 600_001,
                expect_ok: false,
                expect_error_containing: "slow_consumer_timeout_ms must not exceed 600000",
            },
            Case {
                name: "delivery stats disabled (0) is the valid default",
                mutate: |config| config.delivery_stats_interval_secs = 0,
                expect_ok: true,
                expect_error_containing: "",
            },
            Case {
                name: "delivery stats interval at ceiling",
                mutate: |config| config.delivery_stats_interval_secs = 3_600,
                expect_ok: true,
                expect_error_containing: "",
            },
            Case {
                name: "delivery stats interval above ceiling",
                mutate: |config| config.delivery_stats_interval_secs = 3_601,
                expect_ok: false,
                expect_error_containing: "delivery_stats_interval_secs must not exceed 3600",
            },
        ];

        for case in cases {
            let mut config = WebSocketConfig::default();
            (case.mutate)(&mut config);
            let result = config.validate();
            match (case.expect_ok, result) {
                (true, Ok(())) => {}
                (true, Err(err)) => panic!("case `{}` should validate, got: {err}", case.name),
                (false, Ok(())) => panic!("case `{}` should be rejected", case.name),
                (false, Err(err)) => {
                    let message = err.to_string();
                    assert!(
                        message.contains(case.expect_error_containing),
                        "case `{}`: rejection message `{message}` should contain `{}`",
                        case.name,
                        case.expect_error_containing
                    );
                }
            }
        }
    }

    #[test]
    fn defaults_match_documented_values() {
        let config = WebSocketConfig::default();
        assert!(config.enable_batching);
        assert_eq!(config.batch_size, 10);
        assert_eq!(config.batch_interval_ms, 16);
        assert_eq!(config.auth_timeout_secs, 10);
        assert_eq!(config.idle_timeout_secs, 300);
        assert_eq!(config.send_queue_capacity, 1024);
        assert_eq!(config.slow_consumer_timeout_ms, 5_000);
        assert_eq!(
            config.delivery_stats_interval_secs, 0,
            "RelayStats emission is disabled by default"
        );
    }
}
