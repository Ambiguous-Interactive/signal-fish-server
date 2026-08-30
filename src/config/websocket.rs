//! WebSocket configuration types.

use super::defaults::{
    default_auth_timeout_secs, default_batch_interval_ms, default_batch_size,
    default_control_queue_capacity, default_delivery_stats_interval_secs, default_enable_batching,
    default_idle_timeout_secs, default_max_sojourn_ms, default_pong_timeout_secs,
    default_send_queue_capacity, default_server_ping_interval_secs,
    default_slow_consumer_timeout_ms, default_socket_send_buffer_bytes,
};
use serde::{Deserialize, Serialize};

/// Operational ceiling for a single WebSocket message batch.
pub const MAX_BATCH_SIZE: usize = 65_536;

/// Operational ceiling for the batch-coalescing window.
///
/// The window must stay representable as an `Instant` offset from any
/// enqueue stamp: a duration this small can never overflow the deadline
/// arithmetic in the batched receiver (`try_pop_batched` parks a `Latest`
/// front indefinitely when `front_enqueued_at + batch_interval` overflows,
/// releasing only on batch fill, producer close, receiver close, or the
/// queue-saturation slow-consumer close). One minute is far above the
/// 16 ms default and the 15 s reliable-sojourn default — a coalescing
/// window beyond it defeats low-latency signaling — while keeping the
/// overflow region unreachable from any accepted configuration.
pub const MAX_BATCH_INTERVAL_MS: u64 = 60_000;

/// WebSocket configuration.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct WebSocketConfig {
    /// Enable message batching for WebSocket connections
    #[serde(default = "default_enable_batching")]
    pub enable_batching: bool,
    /// Maximum number of messages to batch before flushing. When
    /// `enable_batching` is true this must be `> 0`: a zero size flushes on
    /// every message, silently disabling batching (startup validation rejects
    /// it). With batching disabled the value is unused.
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    /// Maximum time in milliseconds to wait before flushing batch
    #[serde(default = "default_batch_interval_ms")]
    pub batch_interval_ms: u64,
    /// Exclusive app-ID handshake-input deadline in seconds.
    #[serde(default = "default_auth_timeout_secs")]
    pub auth_timeout_secs: u64,
    /// Exclusive post-handshake idle-input deadline in seconds; `0`
    /// disables the timeout.
    ///
    /// A handshake-complete connection that produces no inbound WebSocket frame of
    /// any kind (including Ping/Pong) for this long is closed (normal
    /// disconnect path, so the reconnection grace period still applies). The
    /// app-ID handshake is bounded separately by
    /// [`auth_timeout_secs`](Self::auth_timeout_secs).
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
    /// Cadence for server-initiated RFC 6455 Ping frames; `0` disables them.
    ///
    /// These transport probes bypass the application/control queues so a
    /// backed-up relay cannot hide a half-open socket.
    #[serde(default = "default_server_ping_interval_secs")]
    pub server_ping_interval_secs: u64,
    /// Time allowed for the matching RFC 6455 Pong after a server Ping is
    /// written. A miss closes the connection with `4003 activity_timeout`.
    #[serde(default = "default_pong_timeout_secs")]
    pub pong_timeout_secs: u64,
    /// Requested TCP send-buffer size for accepted HTTP/WebSocket sockets.
    ///
    /// This bounds bytes the WebSocket writer can hand to the kernel ahead of
    /// priority control traffic. Operating systems may clamp or account this
    /// value differently; `0` opts out and keeps the platform default.
    #[serde(default = "default_socket_send_buffer_bytes")]
    pub socket_send_buffer_bytes: u32,
    /// Per-connection outbound message queue capacity.
    ///
    /// Bounds how many undelivered server messages may queue for one
    /// connection before delivery applies backpressure to senders. Larger
    /// values absorb bigger relay bursts without slowing senders; the cost is
    /// only pointer-sized per slot until messages actually queue.
    #[serde(default = "default_send_queue_capacity")]
    pub send_queue_capacity: usize,
    /// Per-connection control-plane queue capacity.
    ///
    /// The dedicated control lane is drained before game data so lifecycle,
    /// error, and heartbeat traffic cannot starve behind a data backlog.
    #[serde(default = "default_control_queue_capacity")]
    pub control_queue_capacity: usize,
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
    /// Maximum exclusive reliable/control sojourn and per-write progress time.
    ///
    /// Reliable traffic uses an end-to-end queue-plus-write deadline. Control
    /// traffic uses its own enqueue age, while latest/volatile traffic uses
    /// this only after selection so lossy queue age cannot evict a recipient.
    /// Expressed in milliseconds and must be nonzero.
    #[serde(default = "default_max_sojourn_ms")]
    pub max_sojourn_ms: u64,
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
            server_ping_interval_secs: default_server_ping_interval_secs(),
            pong_timeout_secs: default_pong_timeout_secs(),
            socket_send_buffer_bytes: default_socket_send_buffer_bytes(),
            send_queue_capacity: default_send_queue_capacity(),
            control_queue_capacity: default_control_queue_capacity(),
            slow_consumer_timeout_ms: default_slow_consumer_timeout_ms(),
            max_sojourn_ms: default_max_sojourn_ms(),
            delivery_stats_interval_secs: default_delivery_stats_interval_secs(),
        }
    }
}

impl WebSocketConfig {
    /// Validate WebSocket configuration
    ///
    /// `idle_timeout_secs` is deliberately unconstrained: `0` disables the
    /// post-handshake idle timeout, and any positive value is a valid operator
    /// choice (aggressive timeouts are useful for tests and hardened
    /// deployments alike).
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.batch_size > MAX_BATCH_SIZE {
            anyhow::bail!(
                "websocket.batch_size must not exceed {MAX_BATCH_SIZE} (configured: {})",
                self.batch_size
            );
        }
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
        if self.pong_timeout_secs == 0 {
            anyhow::bail!("websocket.pong_timeout_secs must be greater than 0 (configured: 0)");
        }
        if self.server_ping_interval_secs > 3_600 {
            anyhow::bail!(
                "websocket.server_ping_interval_secs must not exceed 3600 (1 hour); configured: {}",
                self.server_ping_interval_secs
            );
        }
        if self.pong_timeout_secs > 3_600 {
            anyhow::bail!(
                "websocket.pong_timeout_secs must not exceed 3600 (1 hour); configured: {}",
                self.pong_timeout_secs
            );
        }
        if self.socket_send_buffer_bytes > 16 * 1_024 * 1_024 {
            anyhow::bail!(
                "websocket.socket_send_buffer_bytes must not exceed 16777216 (16 MiB); \
                 configured: {} (0 keeps the platform default)",
                self.socket_send_buffer_bytes
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
        // The batched receiver parks a `Latest` front on its coalesce deadline
        // `front_enqueued_at + batch_interval`; an unrepresentable deadline
        // (`Instant` overflow) releases only on queue progress. Bounding the
        // interval keeps that arithmetic overflow-free for every accepted
        // configuration instead of relying on the self-healing teardowns.
        // With batching disabled the value is never read, mirroring the zero
        // rule above.
        if self.enable_batching && self.batch_interval_ms > MAX_BATCH_INTERVAL_MS {
            anyhow::bail!(
                "websocket.batch_interval_ms must not exceed {MAX_BATCH_INTERVAL_MS} (1 minute); \
                 configured: {}",
                self.batch_interval_ms
            );
        }
        // A zero batch size shares the zero-interval failure shape (#431): with
        // batching on, the receive path clamps it up to 1, so every message
        // flushes immediately and the batching an operator explicitly enabled
        // is silently disabled. With batching disabled the value is never read,
        // so any value is fine — mirroring `batch_interval_ms` above.
        if self.enable_batching && self.batch_size == 0 {
            anyhow::bail!(
                "websocket.batch_size must be greater than 0 when websocket.enable_batching \
                 is true (a zero batch size flushes on every message, disabling batching)"
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
        // A game-start transaction reserves `GameStarting` plus the optional
        // tailored `SessionPlan` before finalizing durable room state. Two
        // slots are therefore the minimum that can always make progress while
        // the first reservation is intentionally held until atomic commit.
        if self.control_queue_capacity < 2 {
            anyhow::bail!(
                "websocket.control_queue_capacity must be at least 2 (configured: {}); \
                 atomic game-start publication reserves two control frames",
                self.control_queue_capacity
            );
        }
        if self.max_sojourn_ms == 0 {
            anyhow::bail!(
                "websocket.max_sojourn_ms must be greater than 0; \
                 it bounds reliable/control sojourn and socket write progress"
            );
        }
        // A sojourn ceiling at or below the normal batch-flush interval
        // could evict a healthy connection before its first scheduled flush.
        if self.enable_batching && self.max_sojourn_ms <= self.batch_interval_ms {
            anyhow::bail!(
                "websocket.max_sojourn_ms ({}) must be greater than \
                 websocket.batch_interval_ms ({}) when websocket.enable_batching is true",
                self.max_sojourn_ms,
                self.batch_interval_ms
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
                name: "server ping disabled",
                mutate: |config| config.server_ping_interval_secs = 0,
                expect_ok: true,
                expect_error_containing: "",
            },
            Case {
                name: "extreme idle timeout remains valid",
                mutate: |config| config.idle_timeout_secs = u64::MAX,
                expect_ok: true,
                expect_error_containing: "",
            },
            Case {
                name: "pong timeout cannot be zero",
                mutate: |config| config.pong_timeout_secs = 0,
                expect_ok: false,
                expect_error_containing: "pong_timeout_secs must be greater than 0",
            },
            Case {
                name: "server ping interval above ceiling",
                mutate: |config| config.server_ping_interval_secs = 3_601,
                expect_ok: false,
                expect_error_containing: "server_ping_interval_secs must not exceed 3600",
            },
            Case {
                name: "pong timeout above ceiling",
                mutate: |config| config.pong_timeout_secs = 3_601,
                expect_ok: false,
                expect_error_containing: "pong_timeout_secs must not exceed 3600",
            },
            Case {
                name: "platform socket send buffer accepted",
                mutate: |config| config.socket_send_buffer_bytes = 0,
                expect_ok: true,
                expect_error_containing: "",
            },
            Case {
                name: "socket send buffer above ceiling",
                mutate: |config| config.socket_send_buffer_bytes = 16 * 1_024 * 1_024 + 1,
                expect_ok: false,
                expect_error_containing: "socket_send_buffer_bytes must not exceed",
            },
            Case {
                name: "defaults are valid",
                mutate: |_config| {},
                expect_ok: true,
                expect_error_containing: "",
            },
            Case {
                name: "zero batch size rejected while batching enabled",
                mutate: |config| {
                    config.enable_batching = true;
                    config.batch_size = 0;
                },
                expect_ok: false,
                expect_error_containing: "batch_size must be greater than 0",
            },
            Case {
                name: "zero batch size accepted when batching disabled",
                mutate: |config| {
                    config.enable_batching = false;
                    config.batch_size = 0;
                },
                expect_ok: true,
                expect_error_containing: "",
            },
            Case {
                name: "batch size above allocation ceiling",
                mutate: |config| config.batch_size = MAX_BATCH_SIZE.saturating_add(1),
                expect_ok: false,
                expect_error_containing: "batch_size must not exceed",
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
                name: "batch interval above coalescing ceiling",
                mutate: |config| {
                    config.enable_batching = true;
                    config.batch_interval_ms = MAX_BATCH_INTERVAL_MS + 1;
                    config.max_sojourn_ms = MAX_BATCH_INTERVAL_MS * 2;
                },
                expect_ok: false,
                expect_error_containing: "batch_interval_ms must not exceed",
            },
            Case {
                name: "batch interval at coalescing ceiling",
                mutate: |config| {
                    config.enable_batching = true;
                    config.batch_interval_ms = MAX_BATCH_INTERVAL_MS;
                    config.max_sojourn_ms = MAX_BATCH_INTERVAL_MS * 2;
                },
                expect_ok: true,
                expect_error_containing: "",
            },
            Case {
                name: "batch interval above ceiling accepted when batching disabled",
                mutate: |config| {
                    config.enable_batching = false;
                    config.batch_interval_ms = u64::MAX;
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
                name: "zero control queue capacity",
                mutate: |config| config.control_queue_capacity = 0,
                expect_ok: false,
                expect_error_containing: "control_queue_capacity must be at least 2",
            },
            Case {
                name: "single-slot control queue cannot reserve a game-start transaction",
                mutate: |config| config.control_queue_capacity = 1,
                expect_ok: false,
                expect_error_containing: "control_queue_capacity must be at least 2",
            },
            Case {
                name: "two-slot control queue is the floor",
                mutate: |config| config.control_queue_capacity = 2,
                expect_ok: true,
                expect_error_containing: "",
            },
            Case {
                name: "zero sojourn ceiling",
                mutate: |config| config.max_sojourn_ms = 0,
                expect_ok: false,
                expect_error_containing: "max_sojourn_ms must be greater than 0",
            },
            Case {
                name: "extreme sojourn ceiling remains valid without batching",
                mutate: |config| {
                    config.enable_batching = false;
                    config.max_sojourn_ms = u64::MAX;
                },
                expect_ok: true,
                expect_error_containing: "",
            },
            Case {
                name: "sojourn ceiling below batching interval",
                mutate: |config| {
                    config.enable_batching = true;
                    config.max_sojourn_ms = config.batch_interval_ms - 1;
                },
                expect_ok: false,
                expect_error_containing: "max_sojourn_ms",
            },
            Case {
                name: "sojourn ceiling equals batching interval",
                mutate: |config| {
                    config.enable_batching = true;
                    config.max_sojourn_ms = config.batch_interval_ms;
                },
                expect_ok: false,
                expect_error_containing: "max_sojourn_ms",
            },
            Case {
                name: "sojourn ceiling one millisecond above batching interval",
                mutate: |config| {
                    config.enable_batching = true;
                    config.max_sojourn_ms = config.batch_interval_ms + 1;
                },
                expect_ok: true,
                expect_error_containing: "",
            },
            Case {
                name: "batching-disabled sojourn ignores batch interval",
                mutate: |config| {
                    config.enable_batching = false;
                    config.batch_interval_ms = 100;
                    config.max_sojourn_ms = 1;
                },
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
        assert!(
            !config.enable_batching,
            "batching is off by default so real-time relay traffic is not held by \
             the flush timer (issue #198); throughput deployments opt in"
        );
        assert_eq!(config.batch_size, 10);
        assert_eq!(config.batch_interval_ms, 16);
        assert_eq!(config.auth_timeout_secs, 10);
        assert_eq!(config.idle_timeout_secs, 300);
        assert_eq!(config.send_queue_capacity, 1024);
        assert_eq!(config.control_queue_capacity, 128);
        assert_eq!(config.slow_consumer_timeout_ms, 5_000);
        assert_eq!(config.max_sojourn_ms, 15_000);
        assert_eq!(
            config.delivery_stats_interval_secs, 0,
            "RelayStats emission is disabled by default"
        );
    }
}
