use crate::protocol::{PlayerId, ServerMessage};
use axum::extract::ws::{Message, WebSocket};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

use crate::server::EnhancedGameServer;

use super::sending::send_single_message;

/// Message batcher for WebSocket connections
/// Batches multiple messages together to reduce syscall overhead
pub(super) struct MessageBatcher {
    pending: VecDeque<Arc<ServerMessage>>,
    batch_size: usize,
    batch_interval: Duration,
    last_flush: Instant,
}

impl MessageBatcher {
    pub(super) fn new(batch_size: usize, batch_interval_ms: u64) -> Self {
        Self {
            pending: VecDeque::with_capacity(batch_size),
            batch_size,
            batch_interval: Duration::from_millis(batch_interval_ms),
            last_flush: Instant::now(),
        }
    }

    /// Queue a message for batching
    pub(super) fn queue(&mut self, message: Arc<ServerMessage>) {
        self.pending.push_back(message);
    }

    /// Check if batch should be flushed
    pub(super) fn should_flush(&self) -> bool {
        // Flush if batch is full or time threshold exceeded
        self.pending.len() >= self.batch_size
            || (!self.pending.is_empty() && self.last_flush.elapsed() >= self.batch_interval)
    }

    /// Flush all pending messages at once (unit-test convenience; production
    /// sends drain incrementally via [`Self::pop_front`] so a cancelled write
    /// cannot lose the rest of the batch).
    #[cfg(test)]
    pub(super) fn flush(&mut self) -> Vec<Arc<ServerMessage>> {
        self.last_flush = Instant::now();
        std::mem::take(&mut self.pending).into_iter().collect()
    }

    /// Take the oldest pending message (FIFO), if any.
    pub(super) fn pop_front(&mut self) -> Option<Arc<ServerMessage>> {
        self.pending.pop_front()
    }

    /// Record that a (possibly incremental) flush completed, resetting the
    /// batch-interval timer.
    pub(super) fn mark_flushed(&mut self) {
        self.last_flush = Instant::now();
    }

    /// Get pending message count
    pub(super) fn len(&self) -> usize {
        self.pending.len()
    }

    /// Check if batch is empty
    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
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
    player_id: &PlayerId,
    server: &Arc<EnhancedGameServer>,
) -> Result<(), ()> {
    let mut batch_size = 0_usize;
    while let Some(message) = batcher.pop_front() {
        if send_single_message(sender, message, player_id, server)
            .await
            .is_err()
        {
            return Err(());
        }
        batch_size += 1;
    }
    if batch_size > 0 {
        tracing::trace!(%player_id, batch_size, "Flushed message batch");
    }
    batcher.mark_flushed();
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
            });
            batcher.queue(message);
        }

        assert_eq!(batcher.len(), 2);
        assert!(!batcher.should_flush()); // Not full yet

        // Add one more to reach batch size
        let message = Arc::new(ServerMessage::PlayerLeft {
            player_id: uuid::Uuid::new_v4(),
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
