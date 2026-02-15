use crate::protocol::{PlayerId, ServerMessage};
use axum::extract::ws::{Message, WebSocket};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

use crate::server::EnhancedGameServer;

use super::sending::send_single_message;

/// Message batcher for WebSocket connections
/// Batches multiple messages together to reduce syscall overhead
pub(super) struct MessageBatcher {
    pending: Vec<Arc<ServerMessage>>,
    batch_size: usize,
    batch_interval: Duration,
    last_flush: Instant,
}

impl MessageBatcher {
    pub(super) fn new(batch_size: usize, batch_interval_ms: u64) -> Self {
        Self {
            pending: Vec::with_capacity(batch_size),
            batch_size,
            batch_interval: Duration::from_millis(batch_interval_ms),
            last_flush: Instant::now(),
        }
    }

    /// Queue a message for batching
    pub(super) fn queue(&mut self, message: Arc<ServerMessage>) {
        self.pending.push(message);
    }

    /// Check if batch should be flushed
    pub(super) fn should_flush(&self) -> bool {
        // Flush if batch is full or time threshold exceeded
        self.pending.len() >= self.batch_size
            || (!self.pending.is_empty() && self.last_flush.elapsed() >= self.batch_interval)
    }

    /// Flush all pending messages
    pub(super) fn flush(&mut self) -> Vec<Arc<ServerMessage>> {
        self.last_flush = Instant::now();
        std::mem::take(&mut self.pending)
    }

    /// Get pending message count
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.pending.len()
    }

    /// Check if batch is empty
    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// Helper function to send a batch of messages
pub(super) async fn send_batch(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    batcher: &mut MessageBatcher,
    player_id: &PlayerId,
    server: &Arc<EnhancedGameServer>,
) -> Result<(), ()> {
    let messages = batcher.flush();
    if messages.is_empty() {
        return Ok(());
    }

    let batch_size = messages.len();

    // Send each message in the batch
    for message in messages {
        if send_single_message(sender, message, player_id, server)
            .await
            .is_err()
        {
            return Err(());
        }
    }

    tracing::trace!(%player_id, batch_size, "Flushed message batch");
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

    #[test]
    fn test_message_batcher_flush_on_time() {
        use std::thread;
        use std::time::Duration;

        let mut batcher = MessageBatcher::new(100, 50); // Small interval, flush on time

        // Add a single message
        let message = Arc::new(ServerMessage::PlayerLeft {
            player_id: uuid::Uuid::new_v4(),
        });
        batcher.queue(message);

        assert_eq!(batcher.len(), 1);
        assert!(!batcher.should_flush()); // Not enough time passed

        // Wait for interval to pass
        thread::sleep(Duration::from_millis(60));

        assert!(batcher.should_flush()); // Should flush now due to time

        // Test flush
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

    #[test]
    fn test_message_batcher_partial_batch() {
        use std::thread;
        use std::time::Duration;

        let mut batcher = MessageBatcher::new(10, 20);

        // Add fewer messages than batch size
        for _ in 0..3 {
            let message = Arc::new(ServerMessage::PlayerLeft {
                player_id: uuid::Uuid::new_v4(),
            });
            batcher.queue(message);
        }

        assert_eq!(batcher.len(), 3);
        assert!(!batcher.should_flush()); // Batch not full, time not elapsed

        // Wait for time interval
        thread::sleep(Duration::from_millis(25));

        assert!(batcher.should_flush()); // Should flush due to time

        let messages = batcher.flush();
        assert_eq!(messages.len(), 3);
    }
}
