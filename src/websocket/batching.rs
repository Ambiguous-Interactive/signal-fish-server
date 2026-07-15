use crate::coordination::outbound_queue::{
    OutboundPayload, OutboundReceiver, QueuedOutbound, TryReceiveError,
};
use crate::coordination::{CloseReason, ConnectionCloseSignal};
use crate::protocol::{PlayerId, ServerMessage};
use axum::extract::ws::{Message, WebSocket};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

use crate::server::EnhancedGameServer;

use super::sending::{send_single_message, SendAccounting, SendDisposition};

/// Message batcher for WebSocket connections
/// Batches multiple messages together to reduce syscall overhead
pub(super) struct MessageBatcher {
    pending: VecDeque<QueuedOutbound>,
    #[cfg(test)]
    batch_size: usize,
    #[cfg(test)]
    batch_interval: Duration,
    #[cfg(test)]
    last_flush: Instant,
}

impl MessageBatcher {
    pub(super) fn new(batch_size: usize, _batch_interval_ms: u64) -> Self {
        Self {
            pending: VecDeque::with_capacity(batch_size),
            #[cfg(test)]
            batch_size,
            #[cfg(test)]
            batch_interval: Duration::from_millis(_batch_interval_ms),
            #[cfg(test)]
            last_flush: Instant::now(),
        }
    }

    /// Queue a message for batching
    pub(super) fn queue(&mut self, message: impl Into<QueuedOutbound>) {
        self.pending.push_back(message.into());
    }

    /// Check if batch should be flushed
    #[cfg(test)]
    pub(super) fn should_flush(&self) -> bool {
        // Flush if batch is full or time threshold exceeded
        self.pending.len() >= self.batch_size
            || (!self.pending.is_empty() && self.last_flush.elapsed() >= self.batch_interval)
    }

    /// Flush all pending messages at once (unit-test convenience; production
    /// sends drain incrementally via [`Self::pop_front`] so a cancelled write
    /// cannot lose the rest of the batch).
    #[cfg(test)]
    pub(super) fn flush(&mut self) -> Vec<QueuedOutbound> {
        self.last_flush = Instant::now();
        std::mem::take(&mut self.pending).into_iter().collect()
    }

    /// Take the oldest pending message (FIFO), if any.
    pub(super) fn pop_front(&mut self) -> Option<QueuedOutbound> {
        self.pending.pop_front()
    }

    /// Get pending message count
    pub(super) fn len(&self) -> usize {
        self.pending.len()
    }

    /// Check if batch is empty
    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub(super) fn oldest_enqueued_at(&self) -> Option<Instant> {
        self.pending.front().map(|message| message.enqueued_at)
    }

    pub(super) fn count_by_class(&self) -> [(crate::protocol::DeliveryClass, u64); 3] {
        let mut counts = [
            (crate::protocol::DeliveryClass::Reliable, 0),
            (crate::protocol::DeliveryClass::Latest, 0),
            (crate::protocol::DeliveryClass::Volatile, 0),
        ];
        for class in self.pending.iter().filter_map(QueuedOutbound::class) {
            match class {
                crate::protocol::DeliveryClass::Reliable => counts[0].1 += 1,
                crate::protocol::DeliveryClass::Latest => counts[1].1 += 1,
                crate::protocol::DeliveryClass::Volatile => counts[2].1 += 1,
            }
        }
        counts
    }
}

/// Helper function to send a batch of messages
///
/// Messages are popped one at a time rather than taken out wholesale, so that
/// cancellation (the send task's close-signal select racing an in-flight
/// socket write) leaves every unsent message inside the batcher where the
/// finalize path can still flush or count it. Only the single message
/// actively being written can be lost with the connection — it is already
/// (partially) on the wire.
pub(super) async fn send_batch(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    batcher: &mut MessageBatcher,
    receiver: &mut OutboundReceiver,
    player_id: &PlayerId,
    server: &Arc<EnhancedGameServer>,
    close_signal: &ConnectionCloseSignal,
    max_sojourn: Duration,
    write_phase: WritePhase,
) -> Result<(), QueueWriteError> {
    let mut batch_size = 0_usize;
    while !batcher.is_empty() {
        loop {
            match receiver.try_recv_control() {
                Ok(Some(control)) => {
                    send_queued(
                        sender,
                        control,
                        batcher.oldest_enqueued_at(),
                        receiver,
                        player_id,
                        server,
                        close_signal,
                        max_sojourn,
                        write_phase,
                    )
                    .await?;
                }
                Ok(None) => break,
                Err(TryReceiveError::AccountabilityFailed) => {
                    #[cfg(feature = "trace-validation")]
                    close_signal.record_trace(
                        crate::trace_validation::DeliveryTraceAction::Unsupported,
                        None,
                        Some("writer-accountability-failed"),
                    );
                    if close_signal.request_close(CloseReason::SlowConsumer) {
                        server
                            .metrics()
                            .increment_websocket_slow_consumer_disconnects();
                    }
                    return Err(QueueWriteError::AccountabilityFailed);
                }
                Err(TryReceiveError::Empty | TryReceiveError::Disconnected) => break,
            }
        }

        let Some(message) = batcher.pop_front() else {
            break;
        };
        send_queued(
            sender,
            message,
            batcher.oldest_enqueued_at(),
            receiver,
            player_id,
            server,
            close_signal,
            max_sojourn,
            write_phase,
        )
        .await?;
        batch_size += 1;
    }
    if batch_size > 0 {
        tracing::trace!(%player_id, batch_size, "Flushed message batch");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QueueWriteError {
    SocketClosed,
    SojournExpired,
    AccountabilityFailed,
}

/// Whether a queued item belongs to the live writer loop or the bounded final
/// flush after a close request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WritePhase {
    Live,
    CloseFlush,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn send_queued(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    queued: QueuedOutbound,
    oldest_batched: Option<Instant>,
    receiver: &OutboundReceiver,
    player_id: &PlayerId,
    server: &Arc<EnhancedGameServer>,
    close_signal: &ConnectionCloseSignal,
    max_sojourn: Duration,
    write_phase: WritePhase,
) -> Result<(), QueueWriteError> {
    #[cfg(not(feature = "trace-validation"))]
    let _ = write_phase;
    #[cfg(feature = "trace-validation")]
    let trace_write = match &queued.payload {
        OutboundPayload::Message(message) => {
            close_signal.start_trace_write(message, write_phase == WritePhase::CloseFlush)
        }
        OutboundPayload::DeliveryReport(_) => None,
    };
    let class = queued.class();
    let metadata = queued.metadata;
    let oldest = receiver
        .oldest_enqueued_at()
        .into_iter()
        .chain(oldest_batched)
        .chain(std::iter::once(queued.enqueued_at))
        .min();
    let message = match queued.payload {
        OutboundPayload::Message(message) => message,
        OutboundPayload::DeliveryReport(report) => {
            Arc::new(ServerMessage::DeliveryReport(Box::new(report)))
        }
    };
    let mut accounting = SendAccounting::new(receiver, server, *player_id, class);
    let recipient_supports_v3 = receiver.supports_v3();
    let recipient_format = receiver.game_data_format();
    let write = send_single_message(
        sender,
        message,
        player_id,
        recipient_supports_v3,
        recipient_format,
        metadata,
        &mut accounting,
    );
    let result = if max_sojourn.is_zero() {
        write.await.map_err(|()| QueueWriteError::SocketClosed)
    } else {
        let deadline = oldest.unwrap_or_else(Instant::now) + max_sojourn;
        match tokio::time::timeout_at(deadline, write).await {
            Ok(result) => result.map_err(|()| QueueWriteError::SocketClosed),
            Err(_) => {
                #[cfg(feature = "trace-validation")]
                close_signal.record_trace(
                    crate::trace_validation::DeliveryTraceAction::Unsupported,
                    None,
                    Some("writer-sojourn-expired"),
                );
                let initiated_close = close_signal.request_close(CloseReason::SlowConsumer);
                if initiated_close {
                    server
                        .metrics()
                        .increment_websocket_slow_consumer_disconnects();
                }
                tracing::warn!(
                    %player_id,
                    max_sojourn_ms = max_sojourn.as_millis() as u64,
                    initiated_close,
                    "Outbound message exceeded the maximum queue sojourn; closing recipient"
                );
                return Err(QueueWriteError::SojournExpired);
            }
        }
    };
    let disposition = result?;
    if disposition == SendDisposition::Written {
        accounting.complete_written();
        #[cfg(feature = "trace-validation")]
        if let Some(delivery_id) = trace_write {
            close_signal.finish_trace_write(delivery_id, write_phase == WritePhase::CloseFlush);
        }
    } else {
        #[cfg(feature = "trace-validation")]
        close_signal.record_trace(
            crate::trace_validation::DeliveryTraceAction::Unsupported,
            None,
            Some("writer-accounted-drop"),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_batcher_new() {
        let batcher = MessageBatcher::new(10, 16);
        assert_eq!(batcher.len(), 0);
        assert!(batcher.is_empty());
        assert!(!batcher.should_flush());
    }

    #[test]
    fn test_message_batcher_queue() {
        let mut batcher = MessageBatcher::new(10, 16);
        let message = Arc::new(ServerMessage::PlayerLeft {
            player_id: uuid::Uuid::new_v4(),
            epoch: None,
            final_seq: None,
        });

        batcher.queue(message);
        assert_eq!(batcher.len(), 1);
        assert!(!batcher.is_empty());
    }

    #[test]
    fn test_message_batcher_flush_on_size() {
        let mut batcher = MessageBatcher::new(3, 1000); // Large interval, flush on size

        // Add messages up to batch size
        for _ in 0..2 {
            let message = Arc::new(ServerMessage::PlayerLeft {
                player_id: uuid::Uuid::new_v4(),
                epoch: None,
                final_seq: None,
            });
            batcher.queue(message);
        }

        assert_eq!(batcher.len(), 2);
        assert!(!batcher.should_flush()); // Not full yet

        // Add one more to reach batch size
        let message = Arc::new(ServerMessage::PlayerLeft {
            player_id: uuid::Uuid::new_v4(),
            epoch: None,
            final_seq: None,
        });
        batcher.queue(message);

        assert_eq!(batcher.len(), 3);
        assert!(batcher.should_flush()); // Should flush now

        // Test flush
        let messages = batcher.flush();
        assert_eq!(messages.len(), 3);
        assert_eq!(batcher.len(), 0);
        assert!(batcher.is_empty());
    }

    // Deterministic under the paused-clock runtime: the flush timer reads
    // `tokio::time::Instant`, so `advance(..)` drives the interval exactly — no
    // `thread::sleep`, so nothing can overshoot or be descheduled. This removes
    // the FLAKE-002 wall-clock dependence at the root rather than band-aiding it
    // with large intervals; one batcher exercises both the "not yet" and
    // "elapsed" edges of the `>=` boundary.
    #[tokio::test(start_paused = true)]
    async fn test_message_batcher_flush_on_time() {
        let mut batcher = MessageBatcher::new(100, 50); // size 100 (unreachable), 50ms interval
        batcher.queue(Arc::new(ServerMessage::PlayerLeft {
            player_id: uuid::Uuid::new_v4(),
            epoch: None,
            final_seq: None,
        }));
        assert_eq!(batcher.len(), 1);
        assert!(
            !batcher.should_flush(),
            "a fresh batch must not flush before its interval elapses"
        );

        // Just under the interval: still not flushing (boundary is `>=`).
        tokio::time::advance(Duration::from_millis(49)).await;
        assert!(
            !batcher.should_flush(),
            "a batch just under its interval must not flush yet"
        );

        // Crossing the interval: flushes on time.
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(
            batcher.should_flush(),
            "the batch must flush once its interval has elapsed"
        );
        let messages = batcher.flush();
        assert_eq!(messages.len(), 1);
        assert_eq!(batcher.len(), 0);
    }

    #[test]
    fn test_message_batcher_multiple_flushes() {
        let mut batcher = MessageBatcher::new(2, 1000);

        // First batch
        for _ in 0..2 {
            let message = Arc::new(ServerMessage::PlayerLeft {
                player_id: uuid::Uuid::new_v4(),
                epoch: None,
                final_seq: None,
            });
            batcher.queue(message);
        }

        assert!(batcher.should_flush());
        let messages1 = batcher.flush();
        assert_eq!(messages1.len(), 2);
        assert_eq!(batcher.len(), 0);

        // Second batch
        for _ in 0..2 {
            let message = Arc::new(ServerMessage::PlayerLeft {
                player_id: uuid::Uuid::new_v4(),
                epoch: None,
                final_seq: None,
            });
            batcher.queue(message);
        }

        assert!(batcher.should_flush());
        let messages2 = batcher.flush();
        assert_eq!(messages2.len(), 2);
        assert_eq!(batcher.len(), 0);
    }

    #[test]
    fn test_message_batcher_empty_flush() {
        let mut batcher = MessageBatcher::new(10, 16);

        // Flush empty batcher
        let messages = batcher.flush();
        assert_eq!(messages.len(), 0);
        assert_eq!(batcher.len(), 0);
    }

    // Partial-batch (below `batch_size`) time flush, deterministic under the
    // paused clock — same `>=`-boundary pattern as
    // `test_message_batcher_flush_on_time` (fresh → just-under → crossing).
    #[tokio::test(start_paused = true)]
    async fn test_message_batcher_partial_batch() {
        let mut batcher = MessageBatcher::new(10, 50); // 3 < size 10, 50ms interval
        for _ in 0..3 {
            batcher.queue(Arc::new(ServerMessage::PlayerLeft {
                player_id: uuid::Uuid::new_v4(),
                epoch: None,
                final_seq: None,
            }));
        }
        assert_eq!(batcher.len(), 3);
        assert!(
            !batcher.should_flush(),
            "a partial batch must not flush before its interval elapses"
        );

        // Just under the interval: still not flushing (boundary is `>=`).
        tokio::time::advance(Duration::from_millis(49)).await;
        assert!(
            !batcher.should_flush(),
            "a partial batch just under its interval must not flush yet"
        );

        // Crossing the interval: flushes on time.
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(
            batcher.should_flush(),
            "a partial batch must flush once its interval has elapsed"
        );
        let messages = batcher.flush();
        assert_eq!(messages.len(), 3);
    }
}
