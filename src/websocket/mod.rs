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

use std::{future::Future, time::Duration};
use tokio::time::Instant;

mod batching;
mod connection;
mod handler;
mod metrics;
pub(crate) mod prometheus;
mod routes;
mod sending;
mod token_binding;

pub(crate) use sending::RelayFrameCache;

/// Run an operation only while its exclusive deadline remains live.
///
/// Success means the operation was observed complete strictly before
/// `deadline`; when both the operation and timer are ready on the same poll,
/// expiry wins.
pub(super) async fn complete_before_deadline<F>(
    deadline: Instant,
    operation: F,
) -> Result<F::Output, DeadlineElapsed>
where
    F: Future,
{
    complete_before_optional_deadline(Some(deadline), operation).await
}

/// Run an operation before an optional representable deadline.
///
/// A configured duration can be valid while extending beyond the platform's
/// representable `Instant` range. `None` preserves that meaning as a deadline
/// beyond this process lifetime instead of inverting it into immediate expiry.
pub(super) async fn complete_before_optional_deadline<F>(
    deadline: Option<Instant>,
    operation: F,
) -> Result<F::Output, DeadlineElapsed>
where
    F: Future,
{
    let wait_for_deadline = crate::deadline::wait_until(deadline);
    tokio::pin!(operation);
    tokio::pin!(wait_for_deadline);
    tokio::select! {
        biased;
        _ = &mut wait_for_deadline => Err(DeadlineElapsed),
        output = &mut operation => Ok(output),
    }
}

/// Convert a relative duration into an absolute deadline without changing an
/// overflow into an already-expired instant.
pub(super) fn deadline_after(start: Instant, duration: Duration) -> Option<Instant> {
    crate::deadline::after(start, duration)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DeadlineElapsed;

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
pub use handler::{
    websocket_handler, websocket_handler_v3, WEBSOCKET_REQUEST_ID_HEADER,
    WEBSOCKET_UPGRADE_OUTCOME_HEADER,
};
pub use metrics::{metrics_handler, prometheus_metrics_handler, MetricsQuery};
#[cfg(feature = "tls")]
pub use routes::ConfiguredAcceptor;
pub use routes::{
    bind_serve_listener, bind_tcp_listener, create_router, create_router_with_origin_policy,
    create_standalone_router, create_standalone_router_with_origin_policy, run_server,
    try_create_router, try_create_standalone_router, try_websocket_route_v3, websocket_route_v3,
    websocket_route_v3_with_origin_policy,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn exclusive_deadline_rejects_completion_ready_at_or_after_boundary() {
        for (advance_by, boundary) in [
            (Duration::from_millis(10), "exact deadline"),
            (Duration::from_millis(11), "after deadline"),
        ] {
            let (release, operation) = tokio::sync::oneshot::channel();
            let deadline = Instant::now() + Duration::from_millis(10);
            let mut bounded = Box::pin(complete_before_deadline(deadline, operation));

            assert!(
                futures_util::poll!(&mut bounded).is_pending(),
                "{boundary}: operation must first be observed pending"
            );
            tokio::time::advance(advance_by).await;
            release.send(()).expect("release operation at boundary");

            assert_eq!(
                bounded.await,
                Err(DeadlineElapsed),
                "{boundary}: expired work must not be revived"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn exclusive_deadline_accepts_completion_before_boundary() {
        let (release, operation) = tokio::sync::oneshot::channel();
        let deadline = Instant::now() + Duration::from_millis(10);
        let mut bounded = Box::pin(complete_before_deadline(deadline, operation));

        assert!(
            futures_util::poll!(&mut bounded).is_pending(),
            "operation must first be observed pending"
        );
        tokio::time::advance(Duration::from_millis(9)).await;
        release.send(()).expect("release operation before boundary");

        assert_eq!(
            bounded.await,
            Ok(Ok(())),
            "completion strictly before the deadline must remain healthy"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unrepresentable_deadline_does_not_expire_ready_work() {
        let deadline = deadline_after(Instant::now(), Duration::from_secs(u64::MAX));
        assert_eq!(
            deadline, None,
            "the extreme valid duration must exercise the overflow boundary"
        );
        assert_eq!(
            complete_before_optional_deadline(deadline, async { 7 }).await,
            Ok(7),
            "deadline overflow must mean later than the process can represent"
        );
    }
}
