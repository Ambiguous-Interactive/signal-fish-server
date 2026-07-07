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
///
/// Every reason maps to a distinct RFC 6455 private-range WebSocket close
/// code ([`Self::websocket_close_code`]) carried on the close frame, so a
/// client that observes only the stream termination — the farewell `Error`
/// frame may be undeliverable on a congested socket — can still attribute
/// the disconnect (issue #136, F1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// The server is shutting down. Defined for the close-code contract;
    /// no in-process trigger exists today (the binary installs no graceful
    /// shutdown handler), but embedders and future shutdown paths must use
    /// this reason rather than overloading `Unregistered`.
    Shutdown,
    /// The connection never completed authentication within
    /// `websocket.auth_timeout_secs`.
    AuthTimeout,
    /// The connection's outbound queue stayed full past the configured
    /// slow-consumer timeout. Keeping it would force either unbounded
    /// buffering or silent message drops; disconnecting loudly is the only
    /// behavior that preserves the delivery contract.
    SlowConsumer,
    /// The activity reaper evicted the connection: it stopped answering the
    /// liveness expectations (`server.ping_timeout`) while still nominally
    /// open.
    ActivityTimeout,
    /// No inbound frame arrived within `websocket.idle_timeout_secs`.
    IdleTimeout,
    /// The connection was unregistered server-side (explicit disconnect,
    /// normal teardown). Socket tasks should flush whatever is already
    /// queued and exit instead of lingering until a socket timeout.
    Unregistered,
}

impl CloseReason {
    /// The WebSocket close code carried on this connection's close frame.
    ///
    /// RFC 6455 reserves 4000-4999 for private application use; the exact
    /// assignments below are part of the documented protocol surface
    /// (docs/protocol.md, "Close codes") and must never be renumbered:
    /// clients switch on them to attribute a disconnect without needing the
    /// (best-effort) farewell `Error` frame to have survived the congested
    /// socket it escapes.
    pub fn websocket_close_code(&self) -> u16 {
        match self {
            Self::Shutdown => 4000,
            Self::AuthTimeout => 4001,
            Self::SlowConsumer => 4002,
            Self::ActivityTimeout => 4003,
            Self::IdleTimeout => 4004,
            // A plain unregistration (leave, replaced connection, normal
            // teardown) is a normal closure, not an application fault.
            Self::Unregistered => 1000,
        }
    }

    /// Short machine-readable close-frame reason string (close frames cap
    /// the reason at 123 bytes; keep these terse and stable).
    pub fn close_frame_reason(&self) -> &'static str {
        match self {
            Self::Shutdown => "server_shutdown",
            Self::AuthTimeout => "auth_timeout",
            Self::SlowConsumer => "slow_consumer",
            Self::ActivityTimeout => "activity_timeout",
            Self::IdleTimeout => "idle_timeout",
            Self::Unregistered => "unregistered",
        }
    }
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

    /// Non-blocking peek at the requested close reason, if any.
    ///
    /// Racing futures can make a connection's write loop end on its own
    /// (queue senders dropped by unregistration) in the same instant a close
    /// reason was requested; an unbiased `select!` may then take the
    /// loop-ended arm and lose the reason — and with it the semantic close
    /// code. Terminal paths consult this before concluding "no reason".
    pub fn requested_reason(&self) -> Option<CloseReason> {
        *self.rx.borrow()
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
    // Conservation accounting: one attempt per call, resolved below as exactly
    // one of enqueued / channel-closed / slow-consumer drop, so the exported
    // counters can prove no delivery outcome went unrecorded.
    metrics.increment_websocket_delivery_attempts();
    // Per-connection ledger for the v3 RelayStats frame. `None` (registry
    // empty) unless `websocket.delivery_stats_interval_secs` enabled tracking,
    // so the default deployment pays one cheap map miss here. Relaxed: these
    // are monotonic diagnostics, never synchronization.
    let connection_stats = metrics.connection_delivery_stats(player_id);
    let message = match handle.sender.try_send(message) {
        Ok(()) => {
            metrics.increment_websocket_deliveries_enqueued();
            if let Some(stats) = &connection_stats {
                stats
                    .sent_to_you
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            return DeliveryOutcome::Delivered;
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            metrics.increment_websocket_deliveries_channel_closed();
            tracing::debug!(
                %player_id,
                "Recipient connection already closing; message unroutable"
            );
            return DeliveryOutcome::ChannelClosed;
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(message)) => message,
    };

    metrics.increment_websocket_backpressure_events();
    if let Some(stats) = &connection_stats {
        stats
            .backpressure_events
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    match tokio::time::timeout(slow_consumer_timeout, handle.sender.send(message)).await {
        Ok(Ok(())) => {
            metrics.increment_websocket_deliveries_enqueued();
            if let Some(stats) = &connection_stats {
                stats
                    .sent_to_you
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            DeliveryOutcome::Delivered
        }
        Ok(Err(_receiver_gone)) => {
            metrics.increment_websocket_deliveries_channel_closed();
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
            if let Some(stats) = &connection_stats {
                stats
                    .dropped_for_you
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
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

    /// Best-effort, non-waiting delivery for pre-close farewells.
    ///
    /// Use ONLY for advisory frames sent to a connection that is about to be
    /// terminated (reaper eviction, timeout notices): a full queue must
    /// neither delay the teardown nor reclassify the close as a
    /// slow-consumer disconnect — the close itself, with its lifecycle
    /// reason, is the authoritative signal. Returns whether the message was
    /// enqueued.
    ///
    /// CONTRACT: implementations must not wait on recipient queue capacity.
    /// There is deliberately no default implementation — falling back to the
    /// reliable (backpressured) path would silently violate this contract.
    async fn try_send_to_player(
        &self,
        player_id: &PlayerId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<bool>;

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

    /// Build and broadcast a room message while the implementation still holds
    /// the room-routing snapshot lock.
    ///
    /// The in-memory coordinator overrides this for v3 game-data stamping: the
    /// sender's next `(epoch, seq)` must be allocated in the same critical
    /// section that snapshots recipients, so a reconnect baseline can never
    /// observe a stamp whose broadcast has not yet chosen whether the restored
    /// socket is a recipient. Test coordinators that do not model concurrent
    /// routing can use this fallback.
    async fn broadcast_to_room_except_with_message<'a>(
        &'a self,
        room_id: &RoomId,
        except_player: &PlayerId,
        build_message: Box<dyn FnOnce() -> Arc<ServerMessage> + Send + 'a>,
    ) -> anyhow::Result<()> {
        self.broadcast_to_room_except(room_id, except_player, build_message())
            .await
    }

    async fn register_local_client(
        &self,
        player_id: PlayerId,
        room_id: Option<RoomId>,
        delivery: ClientDeliveryHandle,
    ) -> anyhow::Result<()>;

    /// Queue an initial message on a room-bound connection before it becomes
    /// visible to room broadcasts.
    ///
    /// The in-memory coordinator overrides this to hold the same room-routing
    /// write lock used by registration while `build_message` runs, so a
    /// reconnect baseline can be captured, queued, and registered without a
    /// broadcast observing the connection halfway through the transition. Test
    /// coordinators that do not model concurrent routing may use this fallback.
    async fn register_local_client_with_initial_message<'a>(
        &'a self,
        player_id: PlayerId,
        room_id: RoomId,
        delivery: ClientDeliveryHandle,
        build_message: Box<dyn FnOnce() -> Arc<ServerMessage> + Send + 'a>,
    ) -> anyhow::Result<DeliveryOutcome> {
        let outcome = match delivery.sender.try_send(build_message()) {
            Ok(()) => DeliveryOutcome::Delivered,
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                DeliveryOutcome::ChannelClosed
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                delivery.close.request_close(CloseReason::SlowConsumer);
                DeliveryOutcome::SlowConsumer
            }
        };

        if outcome == DeliveryOutcome::Delivered {
            self.register_local_client(player_id, Some(room_id), delivery)
                .await?;
        }

        Ok(outcome)
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::ServerMetrics;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Mutex;

    /// Slow-consumer grace used by these tests. Under
    /// `#[tokio::test(start_paused = true)]` the clock only advances when every
    /// task is idle, so this window elapses instantly and deterministically the
    /// moment a delivery is parked on a full queue with no reader — and can
    /// never elapse while a test is actively draining.
    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    /// Generous spin bound for "the spawned delivery has demonstrably reached
    /// the backpressure path" waits. Cooperative yields (not sleeps) keep the
    /// paused clock frozen, so the slow-consumer window cannot fire while a
    /// test is waiting to drain.
    const MAX_YIELD_SPINS: u32 = 10_000;

    fn test_message() -> Arc<ServerMessage> {
        Arc::new(ServerMessage::Pong)
    }

    fn test_player() -> PlayerId {
        PlayerId::from_u128(0x5104A1F1_54D5_44E5_9E57_C0A5E17E57ED)
    }

    /// Build one connection's delivery plumbing: a bounded queue plus the
    /// close signal/listener pair, exactly as the WebSocket layer wires it.
    fn delivery_handle(
        capacity: usize,
    ) -> (
        ClientDeliveryHandle,
        tokio::sync::mpsc::Receiver<Arc<ServerMessage>>,
        ConnectionCloseListener,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        let (close, listener) = ConnectionCloseSignal::channel();
        (ClientDeliveryHandle { sender: tx, close }, rx, listener)
    }

    #[derive(Default)]
    struct FallbackCoordinator {
        broadcasts_except: Mutex<Vec<(RoomId, PlayerId, ServerMessage)>>,
        registrations: Mutex<Vec<(PlayerId, Option<RoomId>)>>,
    }

    #[async_trait::async_trait]
    impl MessageCoordinator for FallbackCoordinator {
        async fn send_to_player(
            &self,
            _player_id: &PlayerId,
            _message: Arc<ServerMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn try_send_to_player(
            &self,
            _player_id: &PlayerId,
            _message: Arc<ServerMessage>,
        ) -> anyhow::Result<bool> {
            Ok(true)
        }

        async fn broadcast_to_room(
            &self,
            _room_id: &RoomId,
            _message: Arc<ServerMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn broadcast_to_room_except(
            &self,
            room_id: &RoomId,
            except_player: &PlayerId,
            message: Arc<ServerMessage>,
        ) -> anyhow::Result<()> {
            self.broadcasts_except.lock().await.push((
                *room_id,
                *except_player,
                (*message).clone(),
            ));
            Ok(())
        }

        async fn register_local_client(
            &self,
            player_id: PlayerId,
            room_id: Option<RoomId>,
            _delivery: ClientDeliveryHandle,
        ) -> anyhow::Result<()> {
            self.registrations.lock().await.push((player_id, room_id));
            Ok(())
        }

        async fn unregister_local_client(&self, _player_id: &PlayerId) -> anyhow::Result<()> {
            Ok(())
        }

        async fn should_process_message(
            &self,
            _message: &crate::distributed::SequencedMessage,
        ) -> anyhow::Result<bool> {
            Ok(true)
        }

        async fn mark_message_processed(
            &self,
            _message: &crate::distributed::SequencedMessage,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn handle_bus_message(
            &self,
            _message: crate::distributed::SequencedMessage,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// Exact conservation law at unit scope, where `deliver_or_disconnect` is
    /// the only writer of these counters: every attempt resolves as exactly
    /// one of enqueued / channel-closed / slow-consumer drop. (The e2e helper
    /// asserts the two-sided form instead, because the full server also counts
    /// post-enqueue abandonment in `websocket_messages_dropped`.)
    fn assert_conservation(metrics: &ServerMetrics) {
        let attempts = metrics.websocket_delivery_attempts.load(Ordering::Relaxed);
        let enqueued = metrics
            .websocket_deliveries_enqueued
            .load(Ordering::Relaxed);
        let channel_closed = metrics
            .websocket_deliveries_channel_closed
            .load(Ordering::Relaxed);
        let dropped = metrics.websocket_messages_dropped.load(Ordering::Relaxed);
        assert_eq!(
            attempts,
            enqueued + channel_closed + dropped,
            "delivery conservation violated: attempts={attempts} != \
             enqueued={enqueued} + channel_closed={channel_closed} + dropped={dropped}"
        );
    }

    /// Spawn a delivery against `handle` and return its join handle. The
    /// delivery owns clones of everything so the test retains the originals.
    fn spawn_delivery(
        metrics: &Arc<ServerMetrics>,
        handle: &ClientDeliveryHandle,
    ) -> tokio::task::JoinHandle<DeliveryOutcome> {
        let metrics = Arc::clone(metrics);
        let handle = handle.clone();
        tokio::spawn(async move {
            deliver_or_disconnect(
                &metrics,
                TEST_TIMEOUT,
                &test_player(),
                &handle,
                test_message(),
            )
            .await
        })
    }

    /// Yield until `condition` holds, bounded by `MAX_YIELD_SPINS`, failing
    /// loudly (never hanging) if it does not. Yields keep the paused clock
    /// frozen, so timers cannot fire as a side effect of waiting.
    async fn yield_until(context: &str, mut condition: impl FnMut() -> bool) {
        let mut spins = 0u32;
        while !condition() {
            assert!(spins < MAX_YIELD_SPINS, "{context}: condition never held");
            spins += 1;
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn default_broadcast_builder_delegates_to_except_broadcast_once() {
        let coordinator = FallbackCoordinator::default();
        let room_id = RoomId::from_u128(0x11111111111111111111111111111111);
        let sender = PlayerId::from_u128(0x22222222222222222222222222222222);
        let build_calls = Arc::new(AtomicUsize::new(0));
        let calls_for_builder = Arc::clone(&build_calls);

        coordinator
            .broadcast_to_room_except_with_message(
                &room_id,
                &sender,
                Box::new(move || {
                    calls_for_builder.fetch_add(1, Ordering::Relaxed);
                    test_message()
                }),
            )
            .await
            .expect("default fallback broadcast succeeds");

        assert_eq!(
            build_calls.load(Ordering::Relaxed),
            1,
            "the fallback must build exactly one message"
        );
        let broadcasts = coordinator.broadcasts_except.lock().await;
        assert_eq!(
            broadcasts.len(),
            1,
            "the fallback must delegate to broadcast_to_room_except"
        );
        let (broadcast_room, except_player, message) = &broadcasts[0];
        assert_eq!(*broadcast_room, room_id);
        assert_eq!(*except_player, sender);
        assert!(
            matches!(message, ServerMessage::Pong),
            "unexpected fallback broadcast message: {message:?}"
        );
    }

    #[tokio::test]
    async fn default_initial_registration_registers_only_after_delivery() {
        let coordinator = FallbackCoordinator::default();
        let room_id = RoomId::from_u128(0x33333333333333333333333333333333);
        let player = PlayerId::from_u128(0x44444444444444444444444444444444);
        let (handle, mut rx, _listener) = delivery_handle(1);

        let outcome = coordinator
            .register_local_client_with_initial_message(
                player,
                room_id,
                handle,
                Box::new(test_message),
            )
            .await
            .expect("default fallback registration succeeds");

        assert_eq!(outcome, DeliveryOutcome::Delivered);
        let initial_message = rx
            .try_recv()
            .expect("initial message must be queued before registration");
        assert!(
            matches!(initial_message.as_ref(), ServerMessage::Pong),
            "unexpected initial message"
        );
        assert_eq!(
            coordinator.registrations.lock().await.as_slice(),
            &[(player, Some(room_id))],
            "the fallback must register the client after queuing the initial message"
        );

        let blocked_player = PlayerId::from_u128(0x55555555555555555555555555555555);
        let (blocked_handle, blocked_rx, blocked_listener) = delivery_handle(1);
        blocked_handle
            .sender
            .try_send(test_message())
            .expect("prefill the single-slot queue");

        let outcome = coordinator
            .register_local_client_with_initial_message(
                blocked_player,
                room_id,
                blocked_handle,
                Box::new(test_message),
            )
            .await
            .expect("full-queue fallback returns an outcome, not an error");

        assert_eq!(outcome, DeliveryOutcome::SlowConsumer);
        assert_eq!(
            coordinator.registrations.lock().await.as_slice(),
            &[(player, Some(room_id))],
            "the fallback must not register a client whose initial message was not queued"
        );
        assert_eq!(
            blocked_listener.requested_reason(),
            Some(CloseReason::SlowConsumer),
            "the fallback must request a slow-consumer close for a full initial queue"
        );
        drop(blocked_rx);
    }

    /// (a) Fast path: an attempt against a queue with room is enqueued
    /// immediately — no backpressure, no drops, conservation exact.
    #[tokio::test(start_paused = true)]
    async fn fast_path_delivery_is_enqueued_without_backpressure() {
        let metrics = ServerMetrics::new();
        let (handle, mut rx, _listener) = delivery_handle(4);

        let outcome = deliver_or_disconnect(
            &metrics,
            TEST_TIMEOUT,
            &test_player(),
            &handle,
            test_message(),
        )
        .await;

        assert_eq!(outcome, DeliveryOutcome::Delivered);
        let delivered = rx
            .try_recv()
            .expect("delivered message must be on the recipient queue");
        assert!(
            matches!(delivered.as_ref(), ServerMessage::Pong),
            "unexpected message on the recipient queue: {delivered:?}"
        );
        assert_eq!(
            metrics.websocket_delivery_attempts.load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics
                .websocket_deliveries_enqueued
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics
                .websocket_backpressure_events
                .load(Ordering::Relaxed),
            0,
            "fast-path delivery must not register backpressure"
        );
        assert_eq!(
            metrics
                .websocket_slow_consumer_disconnects
                .load(Ordering::Relaxed),
            0
        );
        assert_eq!(
            metrics.websocket_messages_dropped.load(Ordering::Relaxed),
            0
        );
        assert_conservation(&metrics);
    }

    /// (b) Full queue drained by a reader: the delivery waits (exactly one
    /// backpressure event) and still lands — nothing dropped, no disconnect.
    #[tokio::test(start_paused = true)]
    async fn backpressured_delivery_waits_for_drain_and_still_lands() {
        let metrics = Arc::new(ServerMetrics::new());
        let (handle, mut rx, _listener) = delivery_handle(1);
        handle
            .sender
            .try_send(test_message())
            .expect("prefill the single-slot queue");

        let delivery = spawn_delivery(&metrics, &handle);

        // Wait until the delivery has demonstrably parked on the full queue,
        // so this test cannot pass by draining ahead of the writer.
        let metrics_for_wait = Arc::clone(&metrics);
        yield_until("delivery must hit the backpressure path", move || {
            metrics_for_wait
                .websocket_backpressure_events
                .load(Ordering::Relaxed)
                > 0
        })
        .await;

        // Drain the prefilled message; the parked send now completes.
        rx.recv().await.expect("prefilled message must be readable");
        let outcome = delivery.await.expect("delivery task must not panic");
        assert_eq!(outcome, DeliveryOutcome::Delivered);
        rx.recv()
            .await
            .expect("backpressured message must be enqueued after the drain");

        assert_eq!(
            metrics.websocket_delivery_attempts.load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics
                .websocket_deliveries_enqueued
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics
                .websocket_backpressure_events
                .load(Ordering::Relaxed),
            1,
            "exactly one backpressure event for one parked delivery"
        );
        assert_eq!(
            metrics
                .websocket_slow_consumer_disconnects
                .load(Ordering::Relaxed),
            0,
            "a consumer that drains within the grace window must not be disconnected"
        );
        assert_eq!(
            metrics.websocket_messages_dropped.load(Ordering::Relaxed),
            0
        );
        assert_conservation(&metrics);
    }

    /// (c) Full queue never drained: every racing delivery times out as a
    /// slow consumer and counts its abandoned message as dropped, but the
    /// disconnect metric counts the CONNECTION exactly once — only the racer
    /// that actually initiated the close (the `request_close` dedup) — and the
    /// close listener resolves with `CloseReason::SlowConsumer`.
    #[tokio::test(start_paused = true)]
    async fn stuck_recipient_counts_one_disconnect_across_racing_deliveries() {
        const RACING_DELIVERIES: u64 = 4;

        let metrics = Arc::new(ServerMetrics::new());
        let (handle, rx, mut listener) = delivery_handle(1);
        handle
            .sender
            .try_send(test_message())
            .expect("prefill the single-slot queue");

        // Four concurrent deliveries against the same stuck handle. With the
        // clock paused, the slow-consumer window elapses the instant all four
        // are parked — no wall-clock waiting, no scheduling luck.
        let deliveries: Vec<_> = (0..RACING_DELIVERIES)
            .map(|_| spawn_delivery(&metrics, &handle))
            .collect();
        for delivery in deliveries {
            let outcome = delivery.await.expect("delivery task must not panic");
            assert_eq!(outcome, DeliveryOutcome::SlowConsumer);
        }

        assert_eq!(
            metrics.websocket_delivery_attempts.load(Ordering::Relaxed),
            RACING_DELIVERIES
        );
        assert_eq!(
            metrics
                .websocket_backpressure_events
                .load(Ordering::Relaxed),
            RACING_DELIVERIES
        );
        assert_eq!(
            metrics.websocket_messages_dropped.load(Ordering::Relaxed),
            RACING_DELIVERIES,
            "every abandoned message must be counted as dropped"
        );
        assert_eq!(
            metrics
                .websocket_slow_consumer_disconnects
                .load(Ordering::Relaxed),
            1,
            "racing timeouts against one stuck connection must count ONE disconnect"
        );
        assert_eq!(
            metrics
                .websocket_deliveries_enqueued
                .load(Ordering::Relaxed),
            0
        );
        assert_eq!(
            metrics
                .websocket_deliveries_channel_closed
                .load(Ordering::Relaxed),
            0
        );
        assert_eq!(
            listener.closed().await,
            Some(CloseReason::SlowConsumer),
            "the stuck connection must be asked to close as a slow consumer"
        );
        assert_conservation(&metrics);
        drop(rx); // kept alive throughout so the queue stayed full, not closed
    }

    /// (d) Receiver dropped before the send: a normal disconnect race —
    /// reported as `ChannelClosed`, never counted as a drop.
    #[tokio::test(start_paused = true)]
    async fn receiver_dropped_before_send_is_channel_closed_not_a_drop() {
        let metrics = ServerMetrics::new();
        let (handle, rx, _listener) = delivery_handle(1);
        drop(rx);

        let outcome = deliver_or_disconnect(
            &metrics,
            TEST_TIMEOUT,
            &test_player(),
            &handle,
            test_message(),
        )
        .await;

        assert_eq!(outcome, DeliveryOutcome::ChannelClosed);
        assert_eq!(
            metrics.websocket_delivery_attempts.load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics
                .websocket_deliveries_channel_closed
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics.websocket_messages_dropped.load(Ordering::Relaxed),
            0,
            "a disconnect race is not a delivery fault and must not count as a drop"
        );
        assert_eq!(
            metrics
                .websocket_slow_consumer_disconnects
                .load(Ordering::Relaxed),
            0
        );
        assert_conservation(&metrics);
    }

    /// (e) Receiver dropped while the delivery is already backpressured: the
    /// parked send fails over to `ChannelClosed`, still not a drop.
    #[tokio::test(start_paused = true)]
    async fn receiver_dropped_while_backpressured_is_channel_closed() {
        let metrics = Arc::new(ServerMetrics::new());
        let (handle, rx, _listener) = delivery_handle(1);
        handle
            .sender
            .try_send(test_message())
            .expect("prefill the single-slot queue");

        let delivery = spawn_delivery(&metrics, &handle);

        let metrics_for_wait = Arc::clone(&metrics);
        yield_until("delivery must hit the backpressure path", move || {
            metrics_for_wait
                .websocket_backpressure_events
                .load(Ordering::Relaxed)
                > 0
        })
        .await;

        drop(rx);
        let outcome = delivery.await.expect("delivery task must not panic");
        assert_eq!(outcome, DeliveryOutcome::ChannelClosed);

        assert_eq!(
            metrics.websocket_delivery_attempts.load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics
                .websocket_deliveries_channel_closed
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics
                .websocket_backpressure_events
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics.websocket_messages_dropped.load(Ordering::Relaxed),
            0
        );
        assert_eq!(
            metrics
                .websocket_slow_consumer_disconnects
                .load(Ordering::Relaxed),
            0
        );
        assert_conservation(&metrics);
    }

    /// (f) First close reason wins: a second request is a no-op (returns
    /// false) and the listener observes the original reason — repeatedly.
    #[tokio::test]
    async fn close_signal_first_reason_wins_and_listener_observes_it() {
        let (signal, mut listener) = ConnectionCloseSignal::channel();

        assert!(
            signal.request_close(CloseReason::SlowConsumer),
            "the first close request must set the reason"
        );
        assert!(
            !signal.request_close(CloseReason::Unregistered),
            "a second close request must be a no-op"
        );

        assert_eq!(listener.closed().await, Some(CloseReason::SlowConsumer));
        // The listener is level-triggered: once closed, it stays closed with
        // the same (first) reason.
        assert_eq!(listener.closed().await, Some(CloseReason::SlowConsumer));
    }

    /// (f) Dropping every signal clone without a reason completes the
    /// listener with `None` (the `changed().await.is_err()` arm) — both for a
    /// listener that is already waiting and for one that starts waiting after
    /// the drop.
    #[tokio::test]
    async fn close_listener_resolves_when_all_signal_clones_drop() {
        let (signal, listener) = ConnectionCloseSignal::channel();
        let signal_clone = signal.clone();

        // A listener parked BEFORE the drop must be woken by it.
        let mut waiting_listener = listener.clone();
        let waiter = tokio::spawn(async move { waiting_listener.closed().await });
        // Let the waiter park on `changed()` before dropping the signals.
        tokio::task::yield_now().await;

        drop(signal);
        drop(signal_clone);
        assert_eq!(
            waiter.await.expect("waiter task must not panic"),
            None,
            "unregistration alone (all signal clones dropped) must end the wait"
        );

        // A listener that starts waiting AFTER the drop resolves immediately.
        let mut late_listener = listener;
        assert_eq!(late_listener.closed().await, None);
    }

    /// The non-blocking peek used by terminal paths to recover a reason an
    /// unbiased select may have raced past: `None` until a close is
    /// requested, then exactly the first requested reason.
    #[tokio::test]
    async fn requested_reason_peeks_without_blocking() {
        let (signal, listener) = ConnectionCloseSignal::channel();
        assert_eq!(listener.requested_reason(), None);

        assert!(signal.request_close(CloseReason::IdleTimeout));
        assert_eq!(listener.requested_reason(), Some(CloseReason::IdleTimeout));

        // First reason wins in the peek too.
        assert!(!signal.request_close(CloseReason::Unregistered));
        assert_eq!(listener.requested_reason(), Some(CloseReason::IdleTimeout));
    }

    /// The RFC 6455 private-range close-code assignments are documented
    /// protocol surface (docs/protocol.md "Close codes"): pin every mapping
    /// exactly so a renumbering cannot slip through, and pin the reason
    /// strings' close-frame constraints (non-empty, stable, ≤123 bytes).
    #[test]
    fn close_reasons_map_to_pinned_codes_and_reason_strings() {
        let expectations = [
            (CloseReason::Shutdown, 4000, "server_shutdown"),
            (CloseReason::AuthTimeout, 4001, "auth_timeout"),
            (CloseReason::SlowConsumer, 4002, "slow_consumer"),
            (CloseReason::ActivityTimeout, 4003, "activity_timeout"),
            (CloseReason::IdleTimeout, 4004, "idle_timeout"),
            (CloseReason::Unregistered, 1000, "unregistered"),
        ];
        for (reason, code, text) in expectations {
            assert_eq!(
                reason.websocket_close_code(),
                code,
                "{reason:?} must close with pinned code {code}"
            );
            assert_eq!(reason.close_frame_reason(), text);
            assert!(
                !text.is_empty() && text.len() <= 123,
                "close-frame reason for {reason:?} must fit the 123-byte cap"
            );
        }
    }
}
