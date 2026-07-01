//! Message coordination and room operation management
//!
//! This module provides facilities for coordinating messages and room operations:
//! - Message deduplication (LRU-based cache)
//! - Room operation coordination with distributed locking
//!
//! For signal-fish-server, this is an in-memory-only implementation.

// Public modules
pub mod dedup;
pub mod room_coordinator;

// Re-export public types
pub use dedup::DedupCacheSettings;
pub use room_coordinator::{
    FinalizedRoom, InMemoryRoomOperationCoordinator, PlayerReadyError,
    RoomOperationCoordinatorTrait, StartGameOutcome,
};

// MessageCoordinator trait (defined in server.rs as InMemoryMessageCoordinator)
use crate::protocol::{PlayerId, RoomId, ServerMessage};
use std::sync::Arc;

/// Why the server requested a connection be closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// The connection's outbound queue stayed full past the configured
    /// slow-consumer timeout. Keeping it would force either unbounded
    /// buffering or silent message drops; disconnecting loudly is the only
    /// behavior that preserves the delivery contract.
    SlowConsumer,
    /// The connection was unregistered server-side (activity reaper, explicit
    /// disconnect, normal teardown). Socket tasks should flush whatever is
    /// already queued and exit instead of lingering until a socket timeout.
    Unregistered,
}

/// Server-side kill switch for one connection.
///
/// Held by the delivery layer (message coordinator / connection manager);
/// requesting a close wakes the connection's I/O tasks, which write a
/// best-effort error frame and tear the socket down. Every clone addresses
/// the same connection; the first requested reason wins.
///
/// Dropping *all* signal clones (the natural consequence of unregistering a
/// connection everywhere) also completes the paired
/// [`ConnectionCloseListener`], so unregistration alone is enough to end the
/// connection's I/O tasks — no message can be quietly routed into a
/// half-alive socket.
#[derive(Debug, Clone)]
pub struct ConnectionCloseSignal {
    tx: tokio::sync::watch::Sender<Option<CloseReason>>,
}

impl ConnectionCloseSignal {
    /// Create a connected signal/listener pair for one connection.
    pub fn channel() -> (Self, ConnectionCloseListener) {
        let (tx, rx) = tokio::sync::watch::channel(None);
        (Self { tx }, ConnectionCloseListener { rx })
    }

    /// Create a signal whose listener side is discarded.
    ///
    /// Used by test paths that register clients without real socket tasks;
    /// close requests become no-ops instead of errors.
    pub fn detached() -> Self {
        Self::channel().0
    }

    /// Request the connection be closed. The first reason wins; repeat
    /// requests are no-ops. Returns whether this call set the reason.
    pub fn request_close(&self, reason: CloseReason) -> bool {
        self.tx.send_if_modified(|current| {
            if current.is_none() {
                *current = Some(reason);
                true
            } else {
                false
            }
        })
    }
}

/// Listener half of [`ConnectionCloseSignal`], owned by a connection's I/O
/// tasks.
#[derive(Debug, Clone)]
pub struct ConnectionCloseListener {
    rx: tokio::sync::watch::Receiver<Option<CloseReason>>,
}

impl ConnectionCloseListener {
    /// Wait until the connection should close.
    ///
    /// Resolves with `Some(reason)` for an explicit close request, or `None`
    /// when every [`ConnectionCloseSignal`] clone has been dropped (i.e. the
    /// connection was unregistered from all delivery maps). Cancel-safe.
    pub async fn closed(&mut self) -> Option<CloseReason> {
        loop {
            if let Some(reason) = *self.rx.borrow_and_update() {
                return Some(reason);
            }
            if self.rx.changed().await.is_err() {
                // All signal holders are gone; report any reason set right
                // before the final drop, otherwise a plain unregistration.
                return *self.rx.borrow();
            }
        }
    }
}

/// Everything the delivery layer needs to reach one connection: the bounded
/// outbound message queue plus the kill switch used when that queue cannot
/// absorb traffic.
#[derive(Debug, Clone)]
pub struct ClientDeliveryHandle {
    pub sender: tokio::sync::mpsc::Sender<Arc<ServerMessage>>,
    pub close: ConnectionCloseSignal,
}

/// Result of attempting to deliver one message to one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// The message was enqueued on the recipient's outbound queue.
    Delivered,
    /// The recipient's connection is already tearing down (its queue receiver
    /// is gone). This is a normal disconnect race, not a delivery fault.
    ChannelClosed,
    /// The recipient's queue stayed full past the slow-consumer timeout; the
    /// message was abandoned and the recipient's connection was asked to
    /// close.
    SlowConsumer,
}

/// Deliver one message to one connection without ever silently dropping it.
///
/// This is the single implementation of the server's delivery contract, used
/// by the message coordinator (relay/broadcast paths) and by the WebSocket
/// layer's direct control-message sends alike:
///
/// - fast path: lock-free `try_send`;
/// - full queue: wait (true backpressure) up to `slow_consumer_timeout`,
///   counting a backpressure event;
/// - still full after the timeout: count and log the failure, signal the
///   recipient's connection to close ([`CloseReason::SlowConsumer`]), and
///   report [`DeliveryOutcome::SlowConsumer`] so the caller can prune the
///   recipient. The message is abandoned only together with the connection
///   itself — never silently.
pub async fn deliver_or_disconnect(
    metrics: &crate::metrics::ServerMetrics,
    slow_consumer_timeout: std::time::Duration,
    player_id: &PlayerId,
    handle: &ClientDeliveryHandle,
    message: Arc<ServerMessage>,
) -> DeliveryOutcome {
    let message = match handle.sender.try_send(message) {
        Ok(()) => return DeliveryOutcome::Delivered,
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            tracing::debug!(
                %player_id,
                "Recipient connection already closing; message unroutable"
            );
            return DeliveryOutcome::ChannelClosed;
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(message)) => message,
    };

    metrics.increment_websocket_backpressure_events();
    match tokio::time::timeout(slow_consumer_timeout, handle.sender.send(message)).await {
        Ok(Ok(())) => DeliveryOutcome::Delivered,
        Ok(Err(_receiver_gone)) => {
            tracing::debug!(%player_id, "Recipient connection closed while backpressured");
            DeliveryOutcome::ChannelClosed
        }
        Err(_elapsed) => {
            // Several deliveries (e.g. concurrent broadcasts from different
            // senders) can time out against the same stuck recipient; only the
            // one that actually initiates the close counts a disconnect, so
            // the metric tallies connections rather than delivery attempts.
            // Every abandoned message counts as dropped regardless.
            let initiated_close = handle.close.request_close(CloseReason::SlowConsumer);
            if initiated_close {
                metrics.increment_websocket_slow_consumer_disconnects();
            }
            metrics.increment_websocket_messages_dropped();
            tracing::warn!(
                %player_id,
                timeout_ms = slow_consumer_timeout.as_millis() as u64,
                initiated_close,
                "Outbound queue full past the slow-consumer timeout; disconnecting recipient \
                 instead of silently dropping messages"
            );
            DeliveryOutcome::SlowConsumer
        }
    }
}

#[async_trait::async_trait]
pub trait MessageCoordinator: Send + Sync {
    async fn send_to_player(
        &self,
        player_id: &PlayerId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<()>;

    async fn broadcast_to_room(
        &self,
        room_id: &RoomId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<()>;

    async fn broadcast_to_room_except(
        &self,
        room_id: &RoomId,
        except_player: &PlayerId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<()>;

    async fn register_local_client(
        &self,
        player_id: PlayerId,
        room_id: Option<RoomId>,
        delivery: ClientDeliveryHandle,
    ) -> anyhow::Result<()>;

    async fn unregister_local_client(&self, player_id: &PlayerId) -> anyhow::Result<()>;

    async fn should_process_message(
        &self,
        message: &crate::distributed::SequencedMessage,
    ) -> anyhow::Result<bool>;

    async fn mark_message_processed(
        &self,
        message: &crate::distributed::SequencedMessage,
    ) -> anyhow::Result<()>;

    async fn handle_bus_message(
        &self,
        message: crate::distributed::SequencedMessage,
    ) -> anyhow::Result<()>;

    async fn handle_membership_update(
        &self,
        update: crate::coordination::MembershipUpdate,
    ) -> anyhow::Result<()> {
        let _ = update;
        Ok(())
    }
}

/// Membership update for cross-instance coordination.
#[derive(Debug, Clone, serde::Deserialize)]
#[allow(dead_code)]
pub struct MembershipUpdate {
    #[allow(dead_code)]
    pub instance_id: String,
}
