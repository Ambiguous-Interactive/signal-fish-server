//! Message coordination and room operation management
//!
//! This module provides facilities for coordinating messages and room operations:
//! - Message deduplication (LRU-based cache)
//! - Room operation coordination with process-local locking
//!
//! For signal-fish-server, this is an in-memory-only implementation.

// Public modules
pub mod dedup;
pub(crate) mod outbound_queue;
pub mod room_coordinator;

// Re-export public types
pub use dedup::DedupCacheSettings;
pub use room_coordinator::{
    FinalizedRoom, InMemoryRoomOperationCoordinator, PlayerReadyError,
    RoomOperationCoordinatorTrait, StartGameOutcome, StartGamePublication,
    StartGamePublicationBuilder,
};

// MessageCoordinator trait (defined in server.rs as InMemoryMessageCoordinator)
use crate::protocol::{PlayerId, RoomId, ServerMessage};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};

use outbound_queue::{
    DataDeliveryMetadata, EnqueueOutcome as QueueEnqueueOutcome, OutboundData, OutboundPermit,
    OutboundSender, TryEnqueueError,
};

/// An owned room-event job. The closure is enqueued synchronously, and its
/// returned future is run by the room's FIFO lane independently of the caller's
/// lifetime. [`MessageCoordinator::enqueue_room_event`] separately
/// requires and owns the matching [`RoomEventMutationGuard`], so queued work
/// cannot be admitted without the mutation gate that bounds this queue.
pub type RoomEventJob = Box<
    dyn FnOnce() -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send + 'static>>
        + Send
        + 'static,
>;

/// Completion of one FIFO room event.
pub type RoomEventCompletion = Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send + 'static>>;

/// Ordered control frames destined for one member of a room transaction.
///
/// All batches in a transaction are fully reserved before its commit hook is
/// allowed to mutate durable state. Frames are committed by phase: every
/// recipient's phase-zero frame, then every phase-one frame. `first_phase`
/// permits a recipient to participate only in the later phase; an empty batch
/// still participates in exact-membership validation without reserving a slot.
/// Production validates a control-queue capacity of at least two, matching the
/// hard two-frame limit enforced by the coordinator before reservations begin.
#[derive(Debug, Clone)]
pub struct RoomRecipientMessages {
    pub player_id: PlayerId,
    pub first_phase: usize,
    pub messages: Vec<Arc<ServerMessage>>,
}

impl RoomRecipientMessages {
    pub fn from_first_phase(
        player_id: PlayerId,
        first_phase: usize,
        messages: Vec<Arc<ServerMessage>>,
    ) -> Self {
        Self {
            player_id,
            first_phase,
            messages,
        }
    }

    pub fn in_order(player_id: PlayerId, messages: Vec<Arc<ServerMessage>>) -> Self {
        Self::from_first_phase(player_id, 0, messages)
    }

    pub(crate) fn phase_count(&self) -> usize {
        self.first_phase.saturating_add(self.messages.len())
    }

    pub(crate) fn message_in_phase(&self, phase: usize) -> Option<&Arc<ServerMessage>> {
        phase
            .checked_sub(self.first_phase)
            .and_then(|index| self.messages.get(index))
    }
}

/// Result of an exact-membership room-message transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomMessageTransactionOutcome {
    /// The fallible hook succeeded and every reserved frame was committed.
    Committed,
    /// Durable state committed, but one or more already-reserved frames could
    /// not be enqueued because their recipient closed or changed generation
    /// during the async commit hook, or the phase callback canceled dependent
    /// later frames after phase zero degraded. Independent healthy phases are
    /// still attempted, and transaction state callbacks run exactly once.
    CommittedDegraded { failed_frames: usize },
    /// Published membership or connection identity changed before commit.
    RoutingChanged,
    /// The hook declined the commit, for example because another StartGame won
    /// the durable compare-and-set.
    HookRejected,
}

/// Guard for the short mutation/enqueue portion of a room state transition.
///
/// Every coordinator implementation must share one lane across room,
/// authority, ready-state, session, and spectator services.
pub struct RoomEventMutationGuard {
    _guard: tokio::sync::OwnedMutexGuard<()>,
    _lane: Arc<RoomEventLane>,
}

struct QueuedRoomEvent {
    job: RoomEventJob,
    completion: tokio::sync::oneshot::Sender<anyhow::Result<bool>>,
}

#[derive(Default)]
struct RoomEventQueue {
    running: bool,
    jobs: VecDeque<QueuedRoomEvent>,
}

struct RoomEventLane {
    room_id: RoomId,
    owner: Weak<RoomEventSequencer>,
    mutation_gate: Arc<tokio::sync::Mutex<()>>,
    queue: Mutex<RoomEventQueue>,
}

impl RoomEventLane {
    fn enqueue(self: &Arc<Self>, job: RoomEventJob) -> RoomEventCompletion {
        let (completion, receiver) = tokio::sync::oneshot::channel();
        let should_start = {
            let mut queue = self.queue.lock().unwrap_or_else(|error| error.into_inner());
            debug_assert!(
                queue.jobs.is_empty(),
                "guard-coupled room enqueue permits at most one pending job per lane"
            );
            queue.jobs.push_back(QueuedRoomEvent { job, completion });
            if queue.running {
                false
            } else {
                queue.running = true;
                true
            }
        };

        if should_start {
            let lane = Arc::clone(self);
            tokio::spawn(async move { lane.drain().await });
        }

        Box::pin(async move {
            receiver
                .await
                .map_err(|_| anyhow::anyhow!("room event lane stopped before completion"))?
        })
    }

    async fn drain(self: Arc<Self>) {
        loop {
            let next = {
                let mut queue = self.queue.lock().unwrap_or_else(|error| error.into_inner());
                match queue.jobs.pop_front() {
                    Some(next) => next,
                    None => {
                        queue.running = false;
                        return;
                    }
                }
            };

            // Isolate each job so a panic cannot strand the lane or later
            // events. Dropping the caller's completion future also cannot
            // cancel the already-enqueued job.
            let result = tokio::spawn(async move { (next.job)().await })
                .await
                .map_err(|error| anyhow::anyhow!("room event job failed: {error}"))
                .and_then(std::convert::identity);
            let _ = next.completion.send(result);
        }
    }
}

impl Drop for RoomEventLane {
    fn drop(&mut self) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let mut lanes = owner
            .lanes
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if lanes
            .get(&self.room_id)
            .is_some_and(|lane| std::ptr::eq(lane.as_ptr(), self))
        {
            lanes.remove(&self.room_id);
        }
    }
}

/// Shared, mutation-ordered room-event domain used by the production message
/// coordinator. The registry stores only weak lanes; the last guard/job drops
/// its lane and removes the matching registry entry, so idle rooms do not
/// accumulate workers or map entries.
#[derive(Default)]
pub(crate) struct RoomEventSequencer {
    lanes: Mutex<HashMap<RoomId, Weak<RoomEventLane>>>,
}

impl RoomEventSequencer {
    fn lane(self: &Arc<Self>, room_id: RoomId) -> Arc<RoomEventLane> {
        let mut lanes = self.lanes.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(lane) = lanes.get(&room_id).and_then(Weak::upgrade) {
            return lane;
        }

        let lane = Arc::new(RoomEventLane {
            room_id,
            owner: Arc::downgrade(self),
            mutation_gate: Arc::new(tokio::sync::Mutex::new(())),
            queue: Mutex::new(RoomEventQueue::default()),
        });
        lanes.insert(room_id, Arc::downgrade(&lane));
        lane
    }

    pub(crate) async fn lock(self: &Arc<Self>, room_id: RoomId) -> RoomEventMutationGuard {
        let lane = self.lane(room_id);
        let guard = Arc::clone(&lane.mutation_gate).lock_owned().await;
        RoomEventMutationGuard {
            _guard: guard,
            _lane: lane,
        }
    }

    pub(crate) fn enqueue(
        self: &Arc<Self>,
        mutation_guard: RoomEventMutationGuard,
        job: RoomEventJob,
    ) -> RoomEventCompletion {
        let lane = Arc::clone(&mutation_guard._lane);
        lane.enqueue(Box::new(move || {
            Box::pin(async move {
                let _mutation_guard = mutation_guard;
                job().await
            })
        }))
    }
}

/// Why the server requested a connection be closed.
///
/// Every reason maps to a distinct RFC 6455 private-range WebSocket close
/// code ([`Self::websocket_close_code`]) carried on the close frame, so a
/// client that observes only the stream termination — the farewell `Error`
/// frame may be undeliverable on a congested socket — can still attribute
/// the disconnect (issue #136, F1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// The server is shutting down. The binary's graceful shutdown drain uses
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
/// half-alive socket. `Shutdown` is the one priority reason: once process
/// drain starts, a semantic 4000 close supersedes earlier lifecycle eviction
/// reasons that raced the drain.
#[derive(Debug, Clone)]
pub struct ConnectionCloseSignal {
    tx: tokio::sync::watch::Sender<Option<CloseReason>>,
    #[cfg(feature = "trace-validation")]
    trace: Option<Arc<crate::trace_validation::DeliveryTraceRecorder>>,
}

impl ConnectionCloseSignal {
    /// Create a connected signal/listener pair for one connection.
    pub fn channel() -> (Self, ConnectionCloseListener) {
        let (tx, rx) = tokio::sync::watch::channel(None);
        (
            Self {
                tx,
                #[cfg(feature = "trace-validation")]
                trace: None,
            },
            ConnectionCloseListener { rx },
        )
    }

    /// Create a close channel instrumented for one formal-replay trace.
    #[cfg(feature = "trace-validation")]
    #[doc(hidden)]
    pub fn channel_with_trace(
        trace: Arc<crate::trace_validation::DeliveryTraceRecorder>,
    ) -> (Self, ConnectionCloseListener) {
        let (tx, rx) = tokio::sync::watch::channel(None);
        (
            Self {
                tx,
                trace: Some(trace),
            },
            ConnectionCloseListener { rx },
        )
    }

    /// Create a signal whose listener side is discarded.
    ///
    /// Used by test paths that register clients without real socket tasks;
    /// close requests become no-ops instead of errors.
    pub fn detached() -> Self {
        Self::channel().0
    }

    /// Request the connection be closed. The first reason wins, except that
    /// `Shutdown` may supersede any previous non-shutdown reason. Returns
    /// whether this call set or upgraded the reason.
    pub fn request_close(&self, reason: CloseReason) -> bool {
        self.request_close_inner(reason, true)
    }

    fn request_close_inner(&self, reason: CloseReason, trace_lifecycle: bool) -> bool {
        #[cfg(not(feature = "trace-validation"))]
        let _ = trace_lifecycle;
        self.tx
            .send_if_modified(|current| match (*current, reason) {
                (None, _) => {
                    // Record the lifecycle transition while the watch value is
                    // still exclusively borrowed. This preserves the same
                    // total order in the trace as competing close requests;
                    // recording after send_if_modified can invert a lifecycle
                    // winner with a delivery timeout that observed it.
                    #[cfg(feature = "trace-validation")]
                    if trace_lifecycle {
                        if reason == CloseReason::SlowConsumer {
                            // Modeled delivery-grace expiration uses the
                            // dedicated request_delivery_timeout_close path.
                            // Every other slow-consumer source (accountability,
                            // sojourn, reservation, etc.) is outside the pilot.
                            self.record_trace(
                                crate::trace_validation::DeliveryTraceAction::Unsupported,
                                None,
                                Some("unmodeled-slow-consumer-close"),
                            );
                        } else {
                            self.record_trace(
                                crate::trace_validation::DeliveryTraceAction::LifecycleClose,
                                None,
                                None,
                            );
                        }
                    }
                    *current = Some(reason);
                    true
                }
                (Some(CloseReason::Shutdown), _) => false,
                (Some(_), CloseReason::Shutdown) => {
                    #[cfg(feature = "trace-validation")]
                    if trace_lifecycle {
                        self.record_trace(
                            crate::trace_validation::DeliveryTraceAction::Unsupported,
                            None,
                            Some("shutdown-close-reason-upgrade"),
                        );
                    }
                    *current = Some(CloseReason::Shutdown);
                    true
                }
                (Some(_), _) => false,
            })
    }

    #[cfg(feature = "trace-validation")]
    pub(crate) fn begin_trace_delivery(&self, message: &Arc<ServerMessage>) -> Option<u64> {
        self.trace
            .as_ref()
            .map(|trace| trace.begin_delivery(message))
    }

    #[cfg(feature = "trace-validation")]
    pub(crate) fn record_trace(
        &self,
        action: crate::trace_validation::DeliveryTraceAction,
        delivery_id: Option<u64>,
        detail: Option<&'static str>,
    ) {
        if let Some(trace) = &self.trace {
            trace.record(action, delivery_id, detail);
        }
    }

    #[cfg(feature = "trace-validation")]
    pub(crate) fn start_trace_write(
        &self,
        message: &Arc<ServerMessage>,
        close_flush: bool,
    ) -> Option<u64> {
        self.trace
            .as_ref()
            .and_then(|trace| trace.start_write(message, close_flush))
    }

    #[cfg(feature = "trace-validation")]
    pub(crate) fn finish_trace_write(&self, delivery_id: u64, close_flush: bool) {
        if let Some(trace) = &self.trace {
            trace.finish_write(delivery_id, close_flush);
        }
    }

    #[cfg(feature = "trace-validation")]
    pub(crate) fn record_trace_queue_closed(&self) {
        if let Some(trace) = &self.trace {
            trace.queue_closed();
        }
    }

    #[cfg(feature = "trace-validation")]
    pub(crate) fn record_trace_grace_expired(
        &self,
        delivery_id: Option<u64>,
        initiated_close: bool,
    ) {
        self.record_trace(
            crate::trace_validation::DeliveryTraceAction::GraceExpired,
            delivery_id,
            Some(if initiated_close {
                "initiated-slow-consumer-close"
            } else {
                "close-already-requested"
            }),
        );
    }

    fn request_delivery_timeout_close(&self, delivery_id: Option<u64>) -> bool {
        #[cfg(not(feature = "trace-validation"))]
        let _ = delivery_id;
        self.tx.send_if_modified(|current| {
            let initiated_close = current.is_none();
            // Serialize the timeout action under the same watch-state borrow
            // as the first-reason decision. Otherwise two racing timeouts (or
            // a lifecycle request) can observe one order in production and
            // acquire the trace recorder in the opposite order.
            #[cfg(feature = "trace-validation")]
            self.record_trace_grace_expired(delivery_id, initiated_close);
            if initiated_close {
                *current = Some(CloseReason::SlowConsumer);
            }
            initiated_close
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
pub struct DeliverySender(DeliverySenderKind);

#[derive(Debug, Clone)]
enum DeliverySenderKind {
    Legacy(tokio::sync::mpsc::Sender<Arc<ServerMessage>>),
    Classified {
        sender: OutboundSender,
        generation: u64,
    },
}

impl From<tokio::sync::mpsc::Sender<Arc<ServerMessage>>> for DeliverySender {
    fn from(sender: tokio::sync::mpsc::Sender<Arc<ServerMessage>>) -> Self {
        Self(DeliverySenderKind::Legacy(sender))
    }
}

impl DeliverySender {
    #[cfg(feature = "trace-validation")]
    fn trace_projection_supported(&self) -> bool {
        match &self.0 {
            DeliverySenderKind::Legacy(_) => true,
            DeliverySenderKind::Classified { sender, .. } => !sender.delivery_classes_enabled(),
        }
    }

    pub(crate) fn classified(sender: OutboundSender) -> Self {
        Self(DeliverySenderKind::Classified {
            sender,
            generation: 0,
        })
    }

    pub(crate) fn next_generation(&self) -> Self {
        match &self.0 {
            DeliverySenderKind::Legacy(sender) => Self(DeliverySenderKind::Legacy(sender.clone())),
            DeliverySenderKind::Classified { sender, generation } => {
                Self(DeliverySenderKind::Classified {
                    sender: sender.clone(),
                    generation: generation.saturating_add(1),
                })
            }
        }
    }

    pub(crate) fn previous_generation(&self) -> Self {
        match &self.0 {
            DeliverySenderKind::Legacy(sender) => Self(DeliverySenderKind::Legacy(sender.clone())),
            DeliverySenderKind::Classified { sender, generation } => {
                Self(DeliverySenderKind::Classified {
                    sender: sender.clone(),
                    generation: generation.saturating_sub(1),
                })
            }
        }
    }

    pub(crate) fn set_protocol_version(&self, version: u16) {
        if let DeliverySenderKind::Classified { sender, .. } = &self.0 {
            sender.set_protocol_version(version);
        }
    }

    pub(crate) fn set_game_data_format(&self, format: crate::protocol::GameDataEncoding) {
        if let DeliverySenderKind::Classified { sender, .. } = &self.0 {
            sender.set_game_data_format(format);
        }
    }

    /// Whether two handles address the same physical queue generation.
    pub(crate) fn same_channel(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (DeliverySenderKind::Legacy(left), DeliverySenderKind::Legacy(right)) => {
                left.same_channel(right)
            }
            (
                DeliverySenderKind::Classified {
                    sender: left,
                    generation: left_generation,
                },
                DeliverySenderKind::Classified {
                    sender: right,
                    generation: right_generation,
                },
            ) => left.same_channel(right) && left_generation == right_generation,
            _ => false,
        }
    }

    fn effective_data_class(
        &self,
        message: &ServerMessage,
    ) -> Option<crate::protocol::DeliveryClass> {
        let requested = match message {
            ServerMessage::GameData { class, .. } => class.unwrap_or_default(),
            ServerMessage::GameDataBinary { .. } => crate::protocol::DeliveryClass::Reliable,
            _ => return None,
        };
        match &self.0 {
            DeliverySenderKind::Legacy(_) => Some(crate::protocol::DeliveryClass::Reliable),
            DeliverySenderKind::Classified { sender, .. } if !sender.delivery_classes_enabled() => {
                Some(crate::protocol::DeliveryClass::Reliable)
            }
            DeliverySenderKind::Classified { .. } => Some(requested),
        }
    }

    fn record_rejected_with_close(&self, class: crate::protocol::DeliveryClass) {
        if let DeliverySenderKind::Classified { sender, .. } = &self.0 {
            sender.record_rejected_with_close(class);
        }
    }

    pub(crate) fn try_send(
        &self,
        message: Arc<ServerMessage>,
        room_id: Option<RoomId>,
    ) -> Result<QueueEnqueueOutcome, DeliveryTrySendError> {
        match &self.0 {
            DeliverySenderKind::Legacy(sender) => sender
                .try_send(message)
                .map(|()| QueueEnqueueOutcome {
                    enqueued: true,
                    losses: 0,
                })
                .map_err(|error| match error {
                    tokio::sync::mpsc::error::TrySendError::Full(message) => {
                        DeliveryTrySendError::Full(message)
                    }
                    tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                        DeliveryTrySendError::Closed
                    }
                }),
            DeliverySenderKind::Classified { sender, generation } => {
                if is_delivery_transition(message.as_ref()) {
                    sender
                        .try_enqueue_transition(message, *generation)
                        .map_err(map_control_queue_error)
                } else if matches!(
                    message.as_ref(),
                    ServerMessage::GameData { .. } | ServerMessage::GameDataBinary { .. }
                ) {
                    let data = classify_outbound_data(message, room_id)
                        .map_err(|_| DeliveryTrySendError::InvalidMetadata)?;
                    sender
                        .try_enqueue_data_scoped(data, *generation)
                        .map_err(map_data_queue_error)
                } else {
                    sender
                        .try_enqueue_control_scoped(message, room_id, *generation)
                        .map_err(map_control_queue_error)
                }
            }
        }
    }

    pub(crate) async fn send(
        &self,
        message: Arc<ServerMessage>,
        room_id: Option<RoomId>,
    ) -> Result<QueueEnqueueOutcome, DeliveryTrySendError> {
        match &self.0 {
            DeliverySenderKind::Legacy(sender) => sender
                .send(message)
                .await
                .map(|()| QueueEnqueueOutcome {
                    enqueued: true,
                    losses: 0,
                })
                .map_err(|_| DeliveryTrySendError::Closed),
            DeliverySenderKind::Classified { sender, generation } => {
                if is_delivery_transition(message.as_ref()) {
                    sender
                        .enqueue_transition(message, *generation)
                        .await
                        .map_err(map_control_queue_error)
                } else if matches!(
                    message.as_ref(),
                    ServerMessage::GameData { .. } | ServerMessage::GameDataBinary { .. }
                ) {
                    let data = classify_outbound_data(message, room_id)
                        .map_err(|_| DeliveryTrySendError::InvalidMetadata)?;
                    sender
                        .enqueue_data_scoped(data, *generation)
                        .await
                        .map_err(map_data_queue_error)
                } else {
                    sender
                        .enqueue_control_scoped(message, room_id, *generation)
                        .await
                        .map_err(map_control_queue_error)
                }
            }
        }
    }

    pub(crate) fn try_reserve_control(
        &self,
        room_id: Option<RoomId>,
    ) -> Result<DeliveryPermit, DeliveryReserveError> {
        match &self.0 {
            DeliverySenderKind::Legacy(sender) => sender
                .clone()
                .try_reserve_owned()
                .map(DeliveryPermit::Legacy)
                .map_err(|error| match error {
                    tokio::sync::mpsc::error::TrySendError::Full(_) => DeliveryReserveError::Full,
                    tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                        DeliveryReserveError::Closed
                    }
                }),
            DeliverySenderKind::Classified { sender, generation } => sender
                .try_reserve_control_scoped(*generation, room_id)
                .map(|permit| DeliveryPermit::Classified {
                    permit,
                    generation: *generation,
                    room_id,
                })
                .map_err(|error| match error {
                    outbound_queue::ReserveError::Full => DeliveryReserveError::Full,
                    outbound_queue::ReserveError::Closed => DeliveryReserveError::Closed,
                    outbound_queue::ReserveError::Canceled => DeliveryReserveError::Canceled,
                }),
        }
    }

    pub(crate) async fn reserve_control(
        &self,
        room_id: Option<RoomId>,
    ) -> Result<DeliveryPermit, DeliveryReserveError> {
        match &self.0 {
            DeliverySenderKind::Legacy(sender) => sender
                .clone()
                .reserve_owned()
                .await
                .map(DeliveryPermit::Legacy)
                .map_err(|_| DeliveryReserveError::Closed),
            DeliverySenderKind::Classified { sender, generation } => sender
                .reserve_control_scoped(*generation, room_id)
                .await
                .map(|permit| DeliveryPermit::Classified {
                    permit,
                    generation: *generation,
                    room_id,
                })
                .map_err(|error| match error {
                    outbound_queue::ReserveError::Full => DeliveryReserveError::Full,
                    outbound_queue::ReserveError::Closed => DeliveryReserveError::Closed,
                    outbound_queue::ReserveError::Canceled => DeliveryReserveError::Canceled,
                }),
        }
    }
}

#[derive(Debug)]
pub(crate) enum DeliveryPermit {
    Legacy(tokio::sync::mpsc::OwnedPermit<Arc<ServerMessage>>),
    Classified {
        permit: OutboundPermit,
        generation: u64,
        room_id: Option<RoomId>,
    },
}

impl DeliveryPermit {
    pub(crate) fn send(
        self,
        message: Arc<ServerMessage>,
    ) -> Result<QueueEnqueueOutcome, Arc<ServerMessage>> {
        match self {
            Self::Legacy(permit) => {
                permit.send(message);
                Ok(QueueEnqueueOutcome {
                    enqueued: true,
                    losses: 0,
                })
            }
            Self::Classified {
                permit,
                generation,
                room_id,
            } => permit.send_control_scoped(message, generation, room_id),
        }
    }
}

#[derive(Debug)]
pub(crate) enum DeliveryTrySendError {
    Full(Arc<ServerMessage>),
    Closed,
    AccountabilityUnavailable,
    InvalidMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveryReserveError {
    Full,
    Closed,
    Canceled,
}

#[derive(Debug, Clone)]
pub struct ClientDeliveryHandle {
    pub sender: DeliverySender,
    pub close: ConnectionCloseSignal,
}

impl ClientDeliveryHandle {
    pub fn new(
        sender: tokio::sync::mpsc::Sender<Arc<ServerMessage>>,
        close: ConnectionCloseSignal,
    ) -> Self {
        Self {
            sender: sender.into(),
            close,
        }
    }

    pub(crate) fn classified(sender: OutboundSender, close: ConnectionCloseSignal) -> Self {
        Self {
            sender: DeliverySender::classified(sender),
            close,
        }
    }
}

fn classify_outbound_data(
    message: Arc<ServerMessage>,
    room_id: Option<RoomId>,
) -> Result<OutboundData, Arc<ServerMessage>> {
    let fields = match message.as_ref() {
        ServerMessage::GameData {
            from_player,
            seq,
            epoch,
            class,
            key,
            ..
        } => (*from_player, *seq, *epoch, *class, *key),
        ServerMessage::GameDataBinary {
            from_player,
            seq,
            epoch,
            ..
        } => (*from_player, *seq, *epoch, None, None),
        _ => return Err(message),
    };
    let (from_player, seq, epoch, class, key) = fields;
    match (room_id, seq, epoch) {
        (Some(room_id), Some(seq), Some(epoch)) => Ok(OutboundData::new(
            message,
            DataDeliveryMetadata {
                class: class.unwrap_or_default(),
                key,
                from_player,
                room_id,
                epoch,
                seq,
            },
        )),
        (_, None, None)
            if class.unwrap_or_default() == crate::protocol::DeliveryClass::Reliable
                && key.is_none() =>
        {
            Ok(OutboundData::reliable_unstamped(message))
        }
        _ => Err(message),
    }
}

fn is_delivery_transition(message: &ServerMessage) -> bool {
    matches!(
        message,
        ServerMessage::RoomJoined(_)
            | ServerMessage::RoomLeft
            | ServerMessage::Reconnected(_)
            | ServerMessage::SpectatorJoined(_)
            | ServerMessage::SpectatorLeft { .. }
    )
}

fn map_data_queue_error(error: TryEnqueueError<OutboundData>) -> DeliveryTrySendError {
    match error {
        TryEnqueueError::Full(data) => DeliveryTrySendError::Full(data.message),
        TryEnqueueError::Closed(_) => DeliveryTrySendError::Closed,
        TryEnqueueError::AccountabilityUnavailable(_) => {
            DeliveryTrySendError::AccountabilityUnavailable
        }
        TryEnqueueError::InvalidMetadata(_) => DeliveryTrySendError::InvalidMetadata,
    }
}

fn map_control_queue_error(error: TryEnqueueError<Arc<ServerMessage>>) -> DeliveryTrySendError {
    match error {
        TryEnqueueError::Full(message) => DeliveryTrySendError::Full(message),
        TryEnqueueError::Closed(_) => DeliveryTrySendError::Closed,
        TryEnqueueError::AccountabilityUnavailable(_) => {
            DeliveryTrySendError::AccountabilityUnavailable
        }
        TryEnqueueError::InvalidMetadata(_) => DeliveryTrySendError::InvalidMetadata,
    }
}

/// Result of attempting to deliver one message to one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// The message was enqueued on the recipient's outbound queue.
    Delivered,
    /// A v3 lossy delivery policy omitted the submitted message and queued an
    /// exact causal `DeliveryReport`; the connection remains healthy.
    AccountedDrop,
    /// A stale room-routing snapshot attempted to commit after a room-context
    /// transition fence. It is intentionally outside the new room's ledger.
    Canceled,
    /// The recipient's connection is already tearing down (its queue receiver
    /// is gone). This is a normal disconnect race, not a delivery fault.
    ChannelClosed,
    /// Outbound delivery could not make accountable progress; the message was
    /// abandoned and the recipient's connection was asked to close loudly.
    SlowConsumer,
}

/// Deliver one message to one connection without ever silently dropping it.
///
/// This is the single implementation of the server's delivery contract, used
/// by the message coordinator (relay/broadcast paths) and by the WebSocket
/// layer's direct control-message sends alike:
///
/// - fast path: apply the negotiated queue policy immediately;
/// - full reliable queue: wait (true backpressure) up to
///   `slow_consumer_timeout`, counting a backpressure event;
/// - lossy policy: atomically queue an exact prior report for every omission;
/// - timeout or lost accountability: signal a loud
///   [`CloseReason::SlowConsumer`] and report
///   [`DeliveryOutcome::SlowConsumer`]. The message is abandoned only with
///   the connection itself, never silently.
pub async fn deliver_or_disconnect(
    metrics: &crate::metrics::ServerMetrics,
    slow_consumer_timeout: std::time::Duration,
    player_id: &PlayerId,
    handle: &ClientDeliveryHandle,
    message: Arc<ServerMessage>,
) -> DeliveryOutcome {
    deliver_or_disconnect_in_room(
        metrics,
        slow_consumer_timeout,
        player_id,
        handle,
        message,
        None,
    )
    .await
}

pub(crate) async fn deliver_or_disconnect_in_room(
    metrics: &crate::metrics::ServerMetrics,
    slow_consumer_timeout: std::time::Duration,
    player_id: &PlayerId,
    handle: &ClientDeliveryHandle,
    message: Arc<ServerMessage>,
    room_id: Option<RoomId>,
) -> DeliveryOutcome {
    #[cfg(feature = "trace-validation")]
    let trace_delivery_id = handle.close.begin_trace_delivery(&message);
    #[cfg(feature = "trace-validation")]
    if !handle.sender.trace_projection_supported() {
        handle.close.record_trace(
            crate::trace_validation::DeliveryTraceAction::Unsupported,
            trace_delivery_id,
            Some("v3-classified-queue"),
        );
    }
    // Conservation accounting: one attempt per call, resolved below as exactly
    // one of enqueued / channel-closed / slow-consumer drop, so the exported
    // counters can prove no delivery outcome went unrecorded.
    metrics.increment_websocket_delivery_attempts();
    // Per-connection ledger for the v3 RelayStats frame. `None` (registry
    // empty) unless `websocket.delivery_stats_interval_secs` enabled tracking,
    // so the default deployment pays one cheap map miss here. Relaxed: these
    // are monotonic diagnostics, never synchronization.
    let connection_stats = metrics.connection_delivery_stats(player_id);
    let offered_class = handle.sender.effective_data_class(message.as_ref());
    let message = match handle.sender.try_send(message, room_id) {
        Ok(outcome) => {
            #[cfg(feature = "trace-validation")]
            if trace_queue_outcome_supported(outcome) {
                handle.close.record_trace(
                    crate::trace_validation::DeliveryTraceAction::SendFast,
                    trace_delivery_id,
                    None,
                );
            } else {
                handle.close.record_trace(
                    crate::trace_validation::DeliveryTraceAction::Unsupported,
                    trace_delivery_id,
                    Some("classified-fast-path-outcome"),
                );
            }
            return record_queue_outcome(metrics, connection_stats.as_ref(), outcome);
        }
        Err(DeliveryTrySendError::Closed) => {
            #[cfg(feature = "trace-validation")]
            handle.close.record_trace(
                crate::trace_validation::DeliveryTraceAction::SendChannelClosed,
                trace_delivery_id,
                None,
            );
            if let Some(class) = offered_class {
                handle.sender.record_rejected_with_close(class);
            }
            metrics.increment_websocket_deliveries_channel_closed();
            tracing::debug!(
                %player_id,
                "Recipient connection already closing; message unroutable"
            );
            return DeliveryOutcome::ChannelClosed;
        }
        Err(DeliveryTrySendError::Full(message)) => {
            #[cfg(feature = "trace-validation")]
            handle.close.record_trace(
                crate::trace_validation::DeliveryTraceAction::SendFull,
                trace_delivery_id,
                None,
            );
            message
        }
        Err(DeliveryTrySendError::AccountabilityUnavailable) => {
            #[cfg(feature = "trace-validation")]
            handle.close.record_trace(
                crate::trace_validation::DeliveryTraceAction::Unsupported,
                trace_delivery_id,
                Some("accountability-unavailable"),
            );
            return fail_delivery_closed(
                metrics,
                connection_stats.as_ref(),
                player_id,
                handle,
                "Delivery accountability queue exhausted; closing recipient",
            );
        }
        Err(DeliveryTrySendError::InvalidMetadata) => {
            #[cfg(feature = "trace-validation")]
            handle.close.record_trace(
                crate::trace_validation::DeliveryTraceAction::Unsupported,
                trace_delivery_id,
                Some("invalid-metadata"),
            );
            if let Some(class) = offered_class {
                handle.sender.record_rejected_with_close(class);
            }
            tracing::error!(%player_id, "Invalid internal outbound delivery metadata");
            return fail_delivery_closed(
                metrics,
                connection_stats.as_ref(),
                player_id,
                handle,
                "Invalid internal delivery metadata; closing recipient fail-closed",
            );
        }
    };

    metrics.increment_websocket_backpressure_events();
    if let Some(stats) = &connection_stats {
        stats
            .backpressure_events
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    let pending_class = offered_class;
    match tokio::time::timeout(slow_consumer_timeout, handle.sender.send(message, room_id)).await {
        Ok(Ok(outcome)) => {
            #[cfg(feature = "trace-validation")]
            if trace_queue_outcome_supported(outcome) {
                handle.close.record_trace(
                    crate::trace_validation::DeliveryTraceAction::ParkedEnqueue,
                    trace_delivery_id,
                    None,
                );
            } else {
                handle.close.record_trace(
                    crate::trace_validation::DeliveryTraceAction::Unsupported,
                    trace_delivery_id,
                    Some("classified-parked-outcome"),
                );
            }
            record_queue_outcome(metrics, connection_stats.as_ref(), outcome)
        }
        Ok(Err(DeliveryTrySendError::Closed)) => {
            #[cfg(feature = "trace-validation")]
            handle.close.record_trace(
                crate::trace_validation::DeliveryTraceAction::ParkedChannelClosed,
                trace_delivery_id,
                None,
            );
            if let Some(class) = pending_class {
                handle.sender.record_rejected_with_close(class);
            }
            metrics.increment_websocket_deliveries_channel_closed();
            tracing::debug!(%player_id, "Recipient connection closed while backpressured");
            DeliveryOutcome::ChannelClosed
        }
        Ok(Err(DeliveryTrySendError::AccountabilityUnavailable)) => {
            #[cfg(feature = "trace-validation")]
            handle.close.record_trace(
                crate::trace_validation::DeliveryTraceAction::Unsupported,
                trace_delivery_id,
                Some("parked-accountability-unavailable"),
            );
            fail_delivery_closed(
                metrics,
                connection_stats.as_ref(),
                player_id,
                handle,
                "Delivery accountability queue exhausted while waiting; closing recipient",
            )
        }
        Ok(Err(DeliveryTrySendError::InvalidMetadata)) => {
            #[cfg(feature = "trace-validation")]
            handle.close.record_trace(
                crate::trace_validation::DeliveryTraceAction::Unsupported,
                trace_delivery_id,
                Some("parked-invalid-metadata"),
            );
            if let Some(class) = pending_class {
                handle.sender.record_rejected_with_close(class);
            }
            tracing::error!(%player_id, "Invalid internal outbound delivery metadata after wait");
            fail_delivery_closed(
                metrics,
                connection_stats.as_ref(),
                player_id,
                handle,
                "Invalid internal delivery metadata; closing recipient fail-closed",
            )
        }
        Ok(Err(DeliveryTrySendError::Full(_))) => {
            #[cfg(feature = "trace-validation")]
            handle.close.record_trace(
                crate::trace_validation::DeliveryTraceAction::Unsupported,
                trace_delivery_id,
                Some("blocking-send-returned-full"),
            );
            if let Some(class) = pending_class {
                handle.sender.record_rejected_with_close(class);
            }
            tracing::error!(%player_id, "Blocking outbound send returned Full unexpectedly");
            fail_delivery_closed(
                metrics,
                connection_stats.as_ref(),
                player_id,
                handle,
                "Outbound queue invariant failed; closing recipient fail-closed",
            )
        }
        Err(_elapsed) => {
            if let Some(class) = pending_class {
                handle.sender.record_rejected_with_close(class);
            }
            // Several deliveries (e.g. concurrent broadcasts from different
            // senders) can time out against the same stuck recipient; only the
            // one that actually initiates the close counts a disconnect, so
            // the metric tallies connections rather than delivery attempts.
            // Every abandoned message counts as dropped regardless.
            #[cfg(feature = "trace-validation")]
            let initiated_close = handle
                .close
                .request_delivery_timeout_close(trace_delivery_id);
            #[cfg(not(feature = "trace-validation"))]
            let initiated_close = handle.close.request_delivery_timeout_close(None);
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

#[cfg(feature = "trace-validation")]
fn trace_queue_outcome_supported(outcome: QueueEnqueueOutcome) -> bool {
    outcome.enqueued && outcome.losses == 0
}

pub(crate) fn record_queue_outcome(
    metrics: &crate::metrics::ServerMetrics,
    connection_stats: Option<&Arc<crate::metrics::ConnectionDeliveryStats>>,
    outcome: QueueEnqueueOutcome,
) -> DeliveryOutcome {
    if let Some(losses) = std::num::NonZeroU64::new(outcome.losses) {
        let losses = losses.get();
        metrics.add_websocket_messages_dropped(losses);
        if let Some(stats) = connection_stats {
            stats
                .dropped_for_you
                .fetch_add(losses, std::sync::atomic::Ordering::Relaxed);
        }
    }
    if outcome.enqueued {
        metrics.increment_websocket_deliveries_enqueued();
        if let Some(stats) = connection_stats {
            stats
                .sent_to_you
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        DeliveryOutcome::Delivered
    } else if outcome.losses > 0 {
        DeliveryOutcome::AccountedDrop
    } else {
        metrics.increment_websocket_deliveries_canceled();
        DeliveryOutcome::Canceled
    }
}

pub(crate) fn fail_delivery_closed(
    metrics: &crate::metrics::ServerMetrics,
    connection_stats: Option<&Arc<crate::metrics::ConnectionDeliveryStats>>,
    player_id: &PlayerId,
    handle: &ClientDeliveryHandle,
    log_message: &'static str,
) -> DeliveryOutcome {
    let initiated_close = handle.close.request_close(CloseReason::SlowConsumer);
    if initiated_close {
        metrics.increment_websocket_slow_consumer_disconnects();
    }
    metrics.increment_websocket_messages_dropped();
    if let Some(stats) = connection_stats {
        stats
            .dropped_for_you
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    tracing::error!(%player_id, initiated_close, "{log_message}");
    DeliveryOutcome::SlowConsumer
}

#[async_trait::async_trait]
pub trait MessageCoordinator: Send + Sync {
    /// Serialize the short mutation/enqueue portion of all room-derived state
    /// transitions. Implementations must use the same per-room domain as
    /// [`Self::enqueue_room_event`].
    async fn lock_room_event_mutation(&self, room_id: &RoomId) -> RoomEventMutationGuard;

    /// Enqueue a committed room event into the shared per-room FIFO.
    ///
    /// Callers synchronously transfer the matching mutation guard with the
    /// job, then await the returned completion only after bounded/distributed
    /// locks have been released. Implementations must retain that guard for the
    /// complete queued job and keep work independent of caller cancellation.
    fn enqueue_room_event(
        &self,
        mutation_guard: RoomEventMutationGuard,
        job: RoomEventJob,
    ) -> RoomEventCompletion;

    async fn send_to_player(
        &self,
        player_id: &PlayerId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<()>;

    /// Deliver only while `player_id` is currently routed in `room_id`.
    /// Production also generation-scopes the enqueue, so a leave/reconnect
    /// after recipient lookup cancels the stale delivery at queue commit.
    async fn send_to_player_in_room(
        &self,
        player_id: &PlayerId,
        _room_id: &RoomId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<bool> {
        self.send_to_player(player_id, message).await?;
        Ok(true)
    }

    /// Snapshot player ids currently published in room routing. `None` means a
    /// lightweight test/distributed implementation does not model local
    /// routing; production returns `Some` under its routing locks.
    async fn routed_player_ids(&self, _room_id: &RoomId) -> anyhow::Result<Option<Vec<PlayerId>>> {
        Ok(None)
    }

    /// Deliver a room-scoped control frame only if routing still contains the
    /// exact member set used to build it.
    async fn send_to_player_in_room_if_members(
        &self,
        player_id: &PlayerId,
        room_id: &RoomId,
        expected_members: &[PlayerId],
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<bool> {
        if let Some(mut routed) = self.routed_player_ids(room_id).await? {
            let mut expected = expected_members.to_vec();
            routed.sort_unstable();
            expected.sort_unstable();
            if routed != expected {
                return Ok(false);
            }
        }
        self.send_to_player_in_room(player_id, room_id, message)
            .await
    }

    async fn send_to_player_if(
        &self,
        player_id: &PlayerId,
        message: Arc<ServerMessage>,
        should_send: &(dyn Fn() -> bool + Send + Sync),
        drain: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<bool> {
        if *drain.borrow() || !should_send() {
            return Ok(false);
        }
        self.send_to_player(player_id, message).await?;
        Ok(true)
    }

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

    /// Best-effort delivery guarded by caller-owned state that may change while
    /// the coordinator awaits its routing lookup.
    ///
    /// Implementations should evaluate `should_send` immediately before the
    /// non-blocking enqueue, after any awaited lookup/lock acquisition. The
    /// default keeps test doubles simple; production coordinators override it
    /// to close the awaited-lookup race.
    async fn try_send_to_player_if(
        &self,
        player_id: &PlayerId,
        message: Arc<ServerMessage>,
        should_send: &(dyn Fn() -> bool + Send + Sync),
    ) -> anyhow::Result<bool> {
        if !should_send() {
            return Ok(false);
        }
        self.try_send_to_player(player_id, message).await
    }

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

    /// Broadcast a room-uniform event after committing its replay hook.
    ///
    /// Production implementations should run `before_send` while holding the
    /// same routing snapshot guard used to decide live recipients. This orders
    /// reconnect registration against the pair as one operation: a reconnecting
    /// socket observes the event through replay or live delivery, never both.
    /// Hook implementations must not call back into `MessageCoordinator` or
    /// await work that can depend on its routing locks.
    async fn broadcast_to_room_with_hook<'a>(
        &'a self,
        room_id: &RoomId,
        message: Arc<ServerMessage>,
        before_send: Box<
            dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                + Send
                + 'a,
        >,
    ) -> anyhow::Result<bool>;

    /// Commit a room broadcast only if routing still contains the exact member
    /// set used to build the payload.
    async fn broadcast_to_room_if_members_with_hook<'a>(
        &'a self,
        room_id: &RoomId,
        expected_members: &[PlayerId],
        message: Arc<ServerMessage>,
        before_send: Box<
            dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                + Send
                + 'a,
        >,
    ) -> anyhow::Result<bool>;

    /// Atomically publish an ordered set of per-recipient room control frames.
    ///
    /// Implementations must reserve capacity for every frame, revalidate the
    /// exact routed member and connection-generation snapshot, and only then
    /// run `before_send` under the final routing guard. An error or `false`
    /// result from the hook must release every reservation without delivering a
    /// frame. Once the hook succeeds, frames are committed phase-by-phase so no
    /// tailored second frame can precede another member's uniform first frame.
    /// A valid production configuration provides at least two control slots per
    /// recipient; implementations must reject batches beyond these two phases.
    /// `after_first_phase` runs exactly once between those phases under the same
    /// routing guard and receives the number of phase-zero frames that failed.
    /// Returning `false` cancels all reserved later-phase frames; returning
    /// `true` commits them even when phase zero degraded. Neither callback may
    /// call back into `MessageCoordinator` or wait on routing.
    async fn commit_room_messages_if_members_with_hook<'a>(
        &'a self,
        room_id: &RoomId,
        expected_members: &[PlayerId],
        recipient_messages: Vec<RoomRecipientMessages>,
        before_send: Box<
            dyn FnOnce() -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send + 'a>>
                + Send
                + 'a,
        >,
        after_first_phase: Box<dyn FnOnce(usize) -> bool + Send + 'a>,
    ) -> anyhow::Result<RoomMessageTransactionOutcome>;

    /// Inject persistent room-transaction failures in unit tests that verify
    /// fail-closed publication recovery. Production implementations never
    /// expose or consult this hook.
    #[cfg(test)]
    fn fail_room_transactions_for_test(&self, _fail: bool) {}

    /// Conditionally broadcast a committed room event after running a replay hook.
    ///
    /// Production implementations may run `before_send` while holding routing
    /// locks or equivalent recipient-snapshot guards so replay recording and
    /// live delivery stay in one critical section. Hook implementations must
    /// not call back into `MessageCoordinator` or await work that can depend on
    /// those locks.
    async fn broadcast_to_room_except_if_with_hook<'a>(
        &'a self,
        room_id: &RoomId,
        except_player: &PlayerId,
        message: Arc<ServerMessage>,
        should_send: &(dyn Fn() -> bool + Send + Sync),
        drain: tokio::sync::watch::Receiver<bool>,
        before_send: Box<
            dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                + Send
                + 'a,
        >,
    ) -> anyhow::Result<bool>;

    /// Build and broadcast a room message while the implementation still holds
    /// the room-routing snapshot lock.
    ///
    /// The in-memory coordinator overrides this for v3 game-data stamping: the
    /// sender's next `(epoch, seq)` must be allocated in the same critical
    /// section that snapshots recipients, so a reconnect baseline can never
    /// observe a stamp whose broadcast has not yet chosen whether the restored
    /// socket is a recipient. The builder returns `None` when the sender was
    /// concurrently unregistered before stamp allocation; that relay is then
    /// canceled instead of exposing unstamped data. Test coordinators that do
    /// not model concurrent routing can use this fallback.
    async fn broadcast_to_room_except_with_message<'a>(
        &'a self,
        room_id: &RoomId,
        except_player: &PlayerId,
        build_message: Box<dyn FnOnce() -> Option<Arc<ServerMessage>> + Send + 'a>,
    ) -> anyhow::Result<()> {
        if let Some(message) = build_message() {
            self.broadcast_to_room_except(room_id, except_player, message)
                .await?;
        }
        Ok(())
    }

    async fn register_local_client(
        &self,
        player_id: PlayerId,
        room_id: Option<RoomId>,
        delivery: ClientDeliveryHandle,
    ) -> anyhow::Result<()>;

    /// Capture a room member's terminal relay watermark and remove its route
    /// as one operation relative to room-recipient snapshots and relay-stamp
    /// allocation. `clear_assignment` must synchronously clear the matching
    /// connection-manager membership and return `(delivery, epoch, seq)`.
    async fn unroute_local_client_with_tail<'a>(
        &'a self,
        player_id: PlayerId,
        room_id: RoomId,
        clear_assignment: Box<dyn FnOnce() -> Option<(ClientDeliveryHandle, u32, u64)> + Send + 'a>,
    ) -> anyhow::Result<Option<(u32, u64)>>;

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
        let outcome = match delivery.sender.try_send(build_message(), Some(room_id)) {
            Ok(outcome) if outcome.enqueued => DeliveryOutcome::Delivered,
            Ok(outcome) if outcome.losses > 0 => DeliveryOutcome::AccountedDrop,
            Ok(_) => DeliveryOutcome::Canceled,
            Err(DeliveryTrySendError::Closed) => DeliveryOutcome::ChannelClosed,
            Err(
                DeliveryTrySendError::Full(_)
                | DeliveryTrySendError::AccountabilityUnavailable
                | DeliveryTrySendError::InvalidMetadata,
            ) => {
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

    /// Async-builder variant of
    /// [`Self::register_local_client_with_initial_message`].
    ///
    /// Production uses this for reconnection: the replay baseline is fetched
    /// while the room-routing registration lock is held, so replay capture,
    /// initial-frame enqueue, and room routing registration are one ordered
    /// transition relative to live room broadcasts.
    async fn register_local_client_with_initial_message_async<'a>(
        &'a self,
        player_id: PlayerId,
        room_id: RoomId,
        delivery: ClientDeliveryHandle,
        build_message: Box<
            dyn FnOnce(
                    Vec<PlayerId>,
                ) -> std::pin::Pin<
                    Box<
                        dyn std::future::Future<Output = anyhow::Result<Arc<ServerMessage>>>
                            + Send
                            + 'a,
                    >,
                > + Send
                + 'a,
        >,
    ) -> anyhow::Result<DeliveryOutcome> {
        let message = build_message(vec![player_id]).await?;
        self.register_local_client_with_initial_message(
            player_id,
            room_id,
            delivery,
            Box::new(move || message),
        )
        .await
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

/// Membership-update extension seam for a future remote coordinator.
#[derive(Debug, Clone, serde::Deserialize)]
#[allow(dead_code)]
pub struct MembershipUpdate {
    #[allow(dead_code)]
    pub instance_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{ConnectionDeliveryStats, ServerMetrics};
    use crate::protocol::{DeliveryClass, GameDataEncoding, LobbyState, SpectatorJoinedPayload};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

    fn game_data_message(
        seq: Option<u64>,
        epoch: Option<u32>,
        class: Option<DeliveryClass>,
        key: Option<u32>,
    ) -> Arc<ServerMessage> {
        Arc::new(ServerMessage::GameData {
            from_player: test_player(),
            data: serde_json::json!({"state": 1}),
            seq,
            epoch,
            class,
            key,
        })
    }

    fn binary_game_data_message(seq: Option<u64>, epoch: Option<u32>) -> Arc<ServerMessage> {
        Arc::new(ServerMessage::GameDataBinary {
            from_player: test_player(),
            encoding: GameDataEncoding::MessagePack,
            payload: bytes::Bytes::from_static(b"payload"),
            seq,
            epoch,
        })
    }

    #[test]
    fn room_recipient_messages_maps_offset_phases_exactly() {
        let messages = vec![test_message(), Arc::new(ServerMessage::RoomLeft)];
        let batch = RoomRecipientMessages::from_first_phase(test_player(), 2, messages.clone());

        assert_eq!(batch.phase_count(), 4);
        for (phase, expected_index) in [(0, None), (1, None), (2, Some(0)), (3, Some(1)), (4, None)]
        {
            let actual = batch.message_in_phase(phase);
            match expected_index {
                Some(index) => assert!(
                    actual.is_some_and(|message| Arc::ptr_eq(message, &messages[index])),
                    "phase {phase} must map to message {index}"
                ),
                None => assert!(actual.is_none(), "phase {phase} must be outside the batch"),
            }
        }
    }

    #[tokio::test]
    async fn guard_coupled_enqueue_bounds_lane_and_serializes_handoff() {
        let sequencer = Arc::new(RoomEventSequencer::default());
        let room_id = RoomId::from_u128(0x5104A1F1_54D5_44E5_9E57_C0A5E17E5701);
        let first_started = Arc::new(tokio::sync::Notify::new());
        let release_first = Arc::new(tokio::sync::Notify::new());
        let order = Arc::new(Mutex::new(Vec::new()));

        let first_guard = sequencer.lock(room_id).await;
        let first_completion = {
            let first_started = Arc::clone(&first_started);
            let release_first = Arc::clone(&release_first);
            let order = Arc::clone(&order);
            sequencer.enqueue(
                first_guard,
                Box::new(move || {
                    Box::pin(async move {
                        order.lock().await.push(1);
                        first_started.notify_one();
                        release_first.notified().await;
                        Ok(true)
                    })
                }),
            )
        };
        first_started.notified().await;

        let second_attempted = Arc::new(AtomicBool::new(false));
        let second_acquired = Arc::new(AtomicBool::new(false));
        let second = {
            let sequencer = Arc::clone(&sequencer);
            let attempted = Arc::clone(&second_attempted);
            let acquired = Arc::clone(&second_acquired);
            let order = Arc::clone(&order);
            tokio::spawn(async move {
                attempted.store(true, Ordering::Release);
                let guard = sequencer.lock(room_id).await;
                acquired.store(true, Ordering::Release);
                sequencer
                    .enqueue(
                        guard,
                        Box::new(move || {
                            Box::pin(async move {
                                order.lock().await.push(2);
                                Ok(true)
                            })
                        }),
                    )
                    .await
            })
        };
        while !second_attempted.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        assert!(
            !second_acquired.load(Ordering::Acquire),
            "a second producer cannot enqueue while the first job owns the room guard"
        );

        release_first.notify_one();
        assert!(first_completion.await.expect("first job completes"));
        assert!(second
            .await
            .expect("second task should not panic")
            .expect("second job completes"));
        assert_eq!(*order.lock().await, vec![1, 2]);
    }

    #[tokio::test]
    async fn dropping_last_room_guard_removes_idle_lane_registry_entry() {
        let sequencer = Arc::new(RoomEventSequencer::default());

        for suffix in 1..=3 {
            let room_id = RoomId::from_u128(0x5104A1F1_54D5_44E5_9E57_C0A5E17E5800 + suffix);
            let guard = sequencer.lock(room_id).await;
            assert_eq!(
                sequencer
                    .lanes
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .len(),
                1,
                "the active room must own exactly one registry entry"
            );

            drop(guard);
            assert!(
                sequencer
                    .lanes
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .is_empty(),
                "dropping the last room owner must remove its idle registry entry"
            );
        }
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
        (
            ClientDeliveryHandle {
                sender: tx.into(),
                close,
            },
            rx,
            listener,
        )
    }

    #[cfg(feature = "trace-validation")]
    fn traced_delivery_handle(
        capacity: usize,
        trace_id: &str,
    ) -> (
        ClientDeliveryHandle,
        tokio::sync::mpsc::Receiver<Arc<ServerMessage>>,
        ConnectionCloseListener,
        Arc<crate::trace_validation::DeliveryTraceRecorder>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        let trace = Arc::new(
            crate::trace_validation::DeliveryTraceRecorder::new(trace_id, capacity)
                .expect("valid delivery trace recorder"),
        );
        let (close, listener) = ConnectionCloseSignal::channel_with_trace(Arc::clone(&trace));
        (
            ClientDeliveryHandle {
                sender: tx.into(),
                close,
            },
            rx,
            listener,
            trace,
        )
    }

    #[cfg(feature = "trace-validation")]
    fn trace_actions(trace: &crate::trace_validation::DeliveryTraceRecorder) -> Vec<String> {
        let mut bytes = Vec::new();
        trace
            .write_jsonl_to(&mut bytes)
            .expect("serialize captured trace");
        String::from_utf8(bytes)
            .expect("trace JSONL is UTF-8")
            .lines()
            .filter_map(|line| {
                let record: serde_json::Value =
                    serde_json::from_str(line).expect("valid trace JSON record");
                (record["kind"] == "event")
                    .then(|| record["action"].as_str().expect("event action").to_string())
            })
            .collect()
    }

    #[cfg(feature = "trace-validation")]
    #[test]
    fn trace_projection_support_matches_queue_protocol() {
        let (legacy, _receiver) = tokio::sync::mpsc::channel(1);
        assert!(DeliverySender::from(legacy).trace_projection_supported());

        let (classified, _receiver) = outbound_queue::channel(1, 1);
        let delivery = DeliverySender::classified(classified.clone());
        assert!(
            delivery.trace_projection_supported(),
            "the pre-v3 classified queue retains the legacy reliable FIFO projection"
        );
        classified.set_protocol_version(3);
        assert!(
            !delivery.trace_projection_supported(),
            "negotiated delivery classes are outside the pilot projection"
        );
    }

    #[cfg(feature = "trace-validation")]
    #[test]
    fn trace_queue_outcome_support_requires_lossless_enqueue() {
        for (outcome, expected) in [
            (
                QueueEnqueueOutcome {
                    enqueued: true,
                    losses: 0,
                },
                true,
            ),
            (
                QueueEnqueueOutcome {
                    enqueued: false,
                    losses: 0,
                },
                false,
            ),
            (
                QueueEnqueueOutcome {
                    enqueued: true,
                    losses: 1,
                },
                false,
            ),
        ] {
            assert_eq!(trace_queue_outcome_supported(outcome), expected);
        }
    }

    #[cfg(feature = "trace-validation")]
    #[test]
    fn traced_delivery_timeout_reports_whether_it_won_first_reason() {
        let trace = Arc::new(
            crate::trace_validation::DeliveryTraceRecorder::new("timeout-winner", 1)
                .expect("valid delivery trace recorder"),
        );
        let (close, listener) = ConnectionCloseSignal::channel_with_trace(Arc::clone(&trace));

        assert!(close.request_delivery_timeout_close(None));
        assert_eq!(listener.requested_reason(), Some(CloseReason::SlowConsumer));
        assert!(!close.request_delivery_timeout_close(None));
        assert_eq!(trace_actions(&trace), vec!["GraceExpired", "GraceExpired"]);
    }

    struct FallbackCoordinator {
        room_events: Arc<RoomEventSequencer>,
        routed_player_ids: Option<Vec<PlayerId>>,
        sends: Mutex<Vec<(PlayerId, ServerMessage)>>,
        broadcasts_except: Mutex<Vec<(RoomId, PlayerId, ServerMessage)>>,
        registrations: Mutex<Vec<(PlayerId, Option<RoomId>)>>,
        try_send_attempts: AtomicUsize,
        try_send_result: AtomicBool,
    }

    impl Default for FallbackCoordinator {
        fn default() -> Self {
            Self {
                room_events: Arc::new(RoomEventSequencer::default()),
                routed_player_ids: None,
                sends: Mutex::default(),
                broadcasts_except: Mutex::default(),
                registrations: Mutex::default(),
                try_send_attempts: AtomicUsize::default(),
                try_send_result: AtomicBool::new(true),
            }
        }
    }

    #[async_trait::async_trait]
    impl MessageCoordinator for FallbackCoordinator {
        async fn lock_room_event_mutation(&self, room_id: &RoomId) -> RoomEventMutationGuard {
            self.room_events.lock(*room_id).await
        }

        fn enqueue_room_event(
            &self,
            mutation_guard: RoomEventMutationGuard,
            job: RoomEventJob,
        ) -> RoomEventCompletion {
            self.room_events.enqueue(mutation_guard, job)
        }

        async fn send_to_player(
            &self,
            player_id: &PlayerId,
            message: Arc<ServerMessage>,
        ) -> anyhow::Result<()> {
            self.sends
                .lock()
                .await
                .push((*player_id, (*message).clone()));
            Ok(())
        }

        async fn routed_player_ids(
            &self,
            _room_id: &RoomId,
        ) -> anyhow::Result<Option<Vec<PlayerId>>> {
            Ok(self.routed_player_ids.clone())
        }

        async fn try_send_to_player(
            &self,
            _player_id: &PlayerId,
            _message: Arc<ServerMessage>,
        ) -> anyhow::Result<bool> {
            self.try_send_attempts.fetch_add(1, Ordering::Relaxed);
            Ok(self.try_send_result.load(Ordering::Relaxed))
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

        async fn broadcast_to_room_with_hook<'a>(
            &'a self,
            room_id: &RoomId,
            message: Arc<ServerMessage>,
            before_send: Box<
                dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> + Send + 'a,
            >,
        ) -> anyhow::Result<bool> {
            before_send().await;
            self.broadcast_to_room(room_id, message).await?;
            Ok(true)
        }

        async fn broadcast_to_room_if_members_with_hook<'a>(
            &'a self,
            room_id: &RoomId,
            _expected_members: &[PlayerId],
            message: Arc<ServerMessage>,
            before_send: Box<
                dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> + Send + 'a,
            >,
        ) -> anyhow::Result<bool> {
            self.broadcast_to_room_with_hook(room_id, message, before_send)
                .await
        }

        async fn broadcast_to_room_except_if_with_hook<'a>(
            &'a self,
            room_id: &RoomId,
            except_player: &PlayerId,
            message: Arc<ServerMessage>,
            should_send: &(dyn Fn() -> bool + Send + Sync),
            drain: tokio::sync::watch::Receiver<bool>,
            before_send: Box<
                dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> + Send + 'a,
            >,
        ) -> anyhow::Result<bool> {
            if *drain.borrow() || !should_send() {
                return Ok(false);
            }
            before_send().await;
            self.broadcast_to_room_except(room_id, except_player, message)
                .await?;
            Ok(true)
        }

        async fn commit_room_messages_if_members_with_hook<'a>(
            &'a self,
            _room_id: &RoomId,
            _expected_members: &[PlayerId],
            recipient_messages: Vec<RoomRecipientMessages>,
            before_send: Box<
                dyn FnOnce() -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send + 'a>>
                    + Send
                    + 'a,
            >,
            after_first_phase: Box<dyn FnOnce(usize) -> bool + Send + 'a>,
        ) -> anyhow::Result<RoomMessageTransactionOutcome> {
            if !before_send().await? {
                return Ok(RoomMessageTransactionOutcome::HookRejected);
            }
            let mut sends = self.sends.lock().await;
            let max_phases = recipient_messages
                .iter()
                .map(RoomRecipientMessages::phase_count)
                .max()
                .unwrap_or(0);
            let mut after_first_phase = Some(after_first_phase);
            for phase in 0..max_phases {
                for batch in &recipient_messages {
                    if let Some(message) = batch.message_in_phase(phase) {
                        sends.push((batch.player_id, message.as_ref().clone()));
                    }
                }
                if phase == 0
                    && !after_first_phase
                        .take()
                        .expect("transaction state callback runs once")(0)
                {
                    break;
                }
            }
            Ok(RoomMessageTransactionOutcome::Committed)
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

        async fn unroute_local_client_with_tail<'a>(
            &'a self,
            player_id: PlayerId,
            _room_id: RoomId,
            clear_assignment: Box<
                dyn FnOnce() -> Option<(ClientDeliveryHandle, u32, u64)> + Send + 'a,
            >,
        ) -> anyhow::Result<Option<(u32, u64)>> {
            let Some((delivery, epoch, final_seq)) = clear_assignment() else {
                return Ok(None);
            };
            self.register_local_client(player_id, None, delivery)
                .await?;
            Ok(Some((epoch, final_seq)))
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

    async fn closed_with_timeout(
        listener: &mut ConnectionCloseListener,
        context: &str,
    ) -> Option<CloseReason> {
        tokio::time::timeout(Duration::from_secs(1), listener.closed())
            .await
            .unwrap_or_else(|_| panic!("{context}: close listener never resolved"))
    }

    #[tokio::test]
    async fn default_broadcast_builder_cancels_none_and_delegates_some_once() {
        let room_id = RoomId::from_u128(0x11111111111111111111111111111111);
        let sender = PlayerId::from_u128(0x22222222222222222222222222222222);
        for (context, built, expected_broadcasts) in [
            ("live sender", Some(test_message()), 1),
            ("unregistered sender", None, 0),
        ] {
            let coordinator = FallbackCoordinator::default();
            let build_calls = Arc::new(AtomicUsize::new(0));
            let calls_for_builder = Arc::clone(&build_calls);

            coordinator
                .broadcast_to_room_except_with_message(
                    &room_id,
                    &sender,
                    Box::new(move || {
                        calls_for_builder.fetch_add(1, Ordering::Relaxed);
                        built
                    }),
                )
                .await
                .unwrap_or_else(|err| panic!("{context}: fallback broadcast failed: {err}"));

            assert_eq!(
                build_calls.load(Ordering::Relaxed),
                1,
                "{context}: fallback must build exactly once"
            );
            let broadcasts = coordinator.broadcasts_except.lock().await;
            assert_eq!(broadcasts.len(), expected_broadcasts, "{context}");
            if let Some((broadcast_room, except_player, message)) = broadcasts.first() {
                assert_eq!(*broadcast_room, room_id);
                assert_eq!(*except_player, sender);
                assert!(
                    matches!(message, ServerMessage::Pong),
                    "{context}: unexpected fallback message: {message:?}"
                );
            }
        }
    }

    #[test]
    fn sender_identity_includes_channel_kind_and_delivery_generation() {
        let (legacy_sender, _legacy_receiver) = tokio::sync::mpsc::channel(1);
        let legacy = DeliverySender::from(legacy_sender);
        let same_legacy = legacy.clone();
        let (other_legacy_sender, _other_legacy_receiver) = tokio::sync::mpsc::channel(1);
        let other_legacy = DeliverySender::from(other_legacy_sender);

        assert!(legacy.same_channel(&same_legacy));
        assert!(!legacy.same_channel(&other_legacy));

        let (sender, _receiver) = outbound_queue::channel(1, 1);
        let generation_zero = DeliverySender::classified(sender);
        let same_generation = generation_zero.clone();
        let generation_one = generation_zero.next_generation();
        let (other_sender, _other_receiver) = outbound_queue::channel(1, 1);
        let other_classified = DeliverySender::classified(other_sender);

        assert!(generation_zero.same_channel(&same_generation));
        assert!(!generation_zero.same_channel(&generation_one));
        assert!(generation_zero.same_channel(&generation_one.previous_generation()));
        assert!(!generation_zero.same_channel(&other_classified));
        assert!(!legacy.same_channel(&generation_zero));
    }

    #[test]
    fn classified_sender_forwards_negotiated_game_data_format() {
        let (sender, receiver) = outbound_queue::channel(1, 1);
        let sender = DeliverySender::classified(sender);

        sender.set_game_data_format(GameDataEncoding::MessagePack);

        assert_eq!(receiver.game_data_format(), GameDataEncoding::MessagePack);
    }

    #[test]
    fn outbound_classification_effective_class_respects_sender_mode() {
        for (context, protocol_version, message, expected) in [
            (
                "legacy lossy request",
                None,
                game_data_message(None, None, Some(DeliveryClass::Latest), Some(7)),
                Some(DeliveryClass::Reliable),
            ),
            (
                "v2 lossy request",
                Some(2),
                game_data_message(None, None, Some(DeliveryClass::Volatile), None),
                Some(DeliveryClass::Reliable),
            ),
            (
                "v3 latest request",
                Some(3),
                game_data_message(None, None, Some(DeliveryClass::Latest), Some(7)),
                Some(DeliveryClass::Latest),
            ),
            (
                "v3 binary data",
                Some(3),
                binary_game_data_message(None, None),
                Some(DeliveryClass::Reliable),
            ),
            ("non-data control", Some(3), test_message(), None),
        ] {
            let sender = if let Some(version) = protocol_version {
                let (sender, _receiver) = outbound_queue::channel(1, 1);
                let sender = DeliverySender::classified(sender);
                sender.set_protocol_version(version);
                sender
            } else {
                let (sender, _receiver) = tokio::sync::mpsc::channel(1);
                DeliverySender::from(sender)
            };

            assert_eq!(
                sender.effective_data_class(message.as_ref()),
                expected,
                "{context}"
            );
        }
    }

    #[test]
    fn outbound_classification_requires_complete_valid_metadata() {
        #[derive(Debug)]
        enum ExpectedClassification {
            Stamped(DataDeliveryMetadata),
            Unstamped,
            Rejected,
        }

        let room_id = RoomId::from_u128(0x5104A1F1_54D5_44E5_9E57_C0A5E17E5702);
        let stamped_json = DataDeliveryMetadata {
            class: DeliveryClass::Latest,
            key: Some(9),
            from_player: test_player(),
            room_id,
            epoch: 3,
            seq: 11,
        };
        let stamped_binary = DataDeliveryMetadata {
            class: DeliveryClass::Reliable,
            key: None,
            from_player: test_player(),
            room_id,
            epoch: 4,
            seq: 12,
        };
        let cases = [
            (
                "stamped JSON",
                game_data_message(Some(11), Some(3), Some(DeliveryClass::Latest), Some(9)),
                Some(room_id),
                ExpectedClassification::Stamped(stamped_json),
            ),
            (
                "stamped binary",
                binary_game_data_message(Some(12), Some(4)),
                Some(room_id),
                ExpectedClassification::Stamped(stamped_binary),
            ),
            (
                "unstamped reliable",
                game_data_message(None, None, None, None),
                None,
                ExpectedClassification::Unstamped,
            ),
            (
                "unstamped latest without key",
                game_data_message(None, None, Some(DeliveryClass::Latest), None),
                None,
                ExpectedClassification::Rejected,
            ),
            (
                "unstamped reliable with key",
                game_data_message(None, None, Some(DeliveryClass::Reliable), Some(9)),
                None,
                ExpectedClassification::Rejected,
            ),
            (
                "partial stamp",
                game_data_message(Some(11), None, None, None),
                Some(room_id),
                ExpectedClassification::Rejected,
            ),
            (
                "stamped without room",
                game_data_message(Some(11), Some(3), None, None),
                None,
                ExpectedClassification::Rejected,
            ),
        ];

        for (context, message, room_id, expected) in cases {
            let original = Arc::clone(&message);
            match (classify_outbound_data(message, room_id), expected) {
                (Ok(data), ExpectedClassification::Stamped(metadata)) => {
                    assert!(Arc::ptr_eq(&data.message, &original), "{context}");
                    assert_eq!(data.metadata, Some(metadata), "{context}");
                }
                (Ok(data), ExpectedClassification::Unstamped) => {
                    assert!(Arc::ptr_eq(&data.message, &original), "{context}");
                    assert_eq!(data.metadata, None, "{context}");
                }
                (Err(returned), ExpectedClassification::Rejected) => {
                    assert!(Arc::ptr_eq(&returned, &original), "{context}");
                }
                (actual, expected) => {
                    panic!("{context}: got {actual:?}, expected {expected:?}")
                }
            }
        }
    }

    #[test]
    fn outbound_classification_distinguishes_delivery_transitions() {
        for (context, message, expected) in [
            ("room departure", ServerMessage::RoomLeft, true),
            ("ordinary control", ServerMessage::Pong, false),
        ] {
            assert_eq!(is_delivery_transition(&message), expected, "{context}");
        }
    }

    #[test]
    fn queue_outcome_accounting_distinguishes_enqueued_loss_and_cancellation() {
        for (context, outcome, expected, enqueued, canceled, losses, sent) in [
            (
                "delivered without loss",
                QueueEnqueueOutcome {
                    enqueued: true,
                    losses: 0,
                },
                DeliveryOutcome::Delivered,
                1,
                0,
                0,
                1,
            ),
            (
                "delivered after replacement",
                QueueEnqueueOutcome {
                    enqueued: true,
                    losses: 2,
                },
                DeliveryOutcome::Delivered,
                1,
                0,
                2,
                1,
            ),
            (
                "accounted drop",
                QueueEnqueueOutcome {
                    enqueued: false,
                    losses: 2,
                },
                DeliveryOutcome::AccountedDrop,
                0,
                0,
                2,
                0,
            ),
            (
                "canceled without loss",
                QueueEnqueueOutcome {
                    enqueued: false,
                    losses: 0,
                },
                DeliveryOutcome::Canceled,
                0,
                1,
                0,
                0,
            ),
        ] {
            let metrics = ServerMetrics::new();
            let connection_stats = Arc::new(ConnectionDeliveryStats::default());

            assert_eq!(
                record_queue_outcome(&metrics, Some(&connection_stats), outcome),
                expected,
                "{context}"
            );
            assert_eq!(
                metrics
                    .websocket_deliveries_enqueued
                    .load(Ordering::Relaxed),
                enqueued,
                "{context}: enqueued"
            );
            assert_eq!(
                metrics
                    .websocket_deliveries_canceled
                    .load(Ordering::Relaxed),
                canceled,
                "{context}: canceled"
            );
            assert_eq!(
                metrics.websocket_messages_dropped.load(Ordering::Relaxed),
                losses,
                "{context}: server losses"
            );
            assert_eq!(
                connection_stats.sent_to_you.load(Ordering::Relaxed),
                sent,
                "{context}: connection sends"
            );
            assert_eq!(
                connection_stats.dropped_for_you.load(Ordering::Relaxed),
                losses,
                "{context}: connection losses"
            );
        }
    }

    #[tokio::test]
    async fn default_conditional_player_send_respects_drain_and_predicate() {
        for (context, drain_started, predicate_allows, expected_sent, expected_sends) in [
            ("normal delivery", false, true, true, 1),
            ("drain already active", true, true, false, 0),
            ("caller predicate false", false, false, false, 0),
        ] {
            let coordinator = FallbackCoordinator::default();
            let player = PlayerId::from_u128(0x66666666666666666666666666666666);
            let (_drain_tx, drain_rx) = tokio::sync::watch::channel(drain_started);
            let should_send = || predicate_allows;

            let sent = coordinator
                .send_to_player_if(&player, test_message(), &should_send, drain_rx)
                .await
                .unwrap_or_else(|err| panic!("{context}: conditional send failed: {err}"));

            assert_eq!(sent, expected_sent, "{context}: unexpected return value");
            let sends = coordinator.sends.lock().await;
            assert_eq!(
                sends.len(),
                expected_sends,
                "{context}: unexpected direct-send side effects"
            );
            if expected_sent {
                assert_eq!(sends[0].0, player, "{context}: sent to wrong player");
                assert!(
                    matches!(sends[0].1, ServerMessage::Pong),
                    "{context}: sent unexpected message: {:?}",
                    sends[0].1
                );
            }
        }
    }

    #[tokio::test]
    async fn default_room_send_delegates_when_routing_snapshot_is_unknown() {
        let coordinator = FallbackCoordinator::default();
        let room_id = RoomId::from_u128(0x66666666666666666666666666666667);
        let player_id = PlayerId::from_u128(0x66666666666666666666666666666668);

        assert_eq!(
            coordinator.routed_player_ids(&room_id).await.unwrap(),
            None,
            "the default routing snapshot must remain unknown"
        );
        assert!(
            coordinator
                .send_to_player_in_room(&player_id, &room_id, test_message())
                .await
                .unwrap(),
            "the default room send must report delegated success"
        );

        let sends = coordinator.sends.lock().await;
        assert_eq!(sends.len(), 1, "the default room send must delegate once");
        assert_eq!(
            sends[0].0, player_id,
            "the delegate received the wrong player"
        );
        assert!(
            matches!(sends[0].1, ServerMessage::Pong),
            "the delegate received an unexpected message: {:?}",
            sends[0].1
        );
    }

    #[tokio::test]
    async fn default_exact_membership_send_accepts_only_equal_sets() {
        let room_id = RoomId::from_u128(0x66666666666666666666666666666669);
        let player_id = PlayerId::from_u128(0x6666666666666666666666666666666a);
        let peer_id = PlayerId::from_u128(0x6666666666666666666666666666666b);
        let outsider_id = PlayerId::from_u128(0x6666666666666666666666666666666c);

        for (context, expected_members, expected_sent) in [
            (
                "same members in different order",
                vec![peer_id, player_id],
                true,
            ),
            ("different member set", vec![player_id, outsider_id], false),
        ] {
            let coordinator = FallbackCoordinator {
                routed_player_ids: Some(vec![player_id, peer_id]),
                ..FallbackCoordinator::default()
            };

            let sent = coordinator
                .send_to_player_in_room_if_members(
                    &player_id,
                    &room_id,
                    &expected_members,
                    test_message(),
                )
                .await
                .unwrap_or_else(|err| panic!("{context}: membership send failed: {err}"));

            assert_eq!(sent, expected_sent, "{context}: unexpected return value");
            let sends = coordinator.sends.lock().await;
            assert_eq!(
                sends.len(),
                usize::from(expected_sent),
                "{context}: unexpected direct-send side effects"
            );
            if expected_sent {
                assert_eq!(sends[0].0, player_id, "{context}: sent to wrong player");
                assert!(
                    matches!(sends[0].1, ServerMessage::Pong),
                    "{context}: sent unexpected message: {:?}",
                    sends[0].1
                );
            }
        }
    }

    #[tokio::test]
    async fn default_conditional_try_send_respects_predicate_and_delegates() {
        for (context, predicate_allows, delegate_result, expected_sent, expected_attempts) in [
            ("delegated success", true, true, true, 1),
            ("delegated failure", true, false, false, 1),
            ("caller predicate false", false, true, false, 0),
        ] {
            let coordinator = FallbackCoordinator::default();
            coordinator
                .try_send_result
                .store(delegate_result, Ordering::Relaxed);
            let player = PlayerId::from_u128(0x77777777777777777777777777777777);
            let should_send = || predicate_allows;

            let sent = coordinator
                .try_send_to_player_if(&player, test_message(), &should_send)
                .await
                .unwrap_or_else(|err| panic!("{context}: conditional try-send failed: {err}"));

            assert_eq!(sent, expected_sent, "{context}: unexpected return value");
            assert_eq!(
                coordinator.try_send_attempts.load(Ordering::Relaxed),
                expected_attempts,
                "{context}: unexpected try-send attempt count"
            );
        }
    }

    #[tokio::test]
    async fn default_conditional_broadcast_checks_guards_before_hook() {
        for (context, drain_started, predicate_allows) in [
            ("drain already active", true, true),
            ("caller predicate false", false, false),
        ] {
            let coordinator = FallbackCoordinator::default();
            let room_id = RoomId::from_u128(0x88888888888888888888888888888888);
            let sender = PlayerId::from_u128(0x99999999999999999999999999999999);
            let (_drain_tx, drain_rx) = tokio::sync::watch::channel(drain_started);
            let hook_calls = Arc::new(AtomicUsize::new(0));
            let hook_calls_for_hook = Arc::clone(&hook_calls);
            let should_send = || predicate_allows;

            let sent = coordinator
                .broadcast_to_room_except_if_with_hook(
                    &room_id,
                    &sender,
                    test_message(),
                    &should_send,
                    drain_rx,
                    Box::new(move || {
                        Box::pin(async move {
                            hook_calls_for_hook.fetch_add(1, Ordering::Relaxed);
                        })
                    }),
                )
                .await
                .unwrap_or_else(|err| panic!("{context}: conditional broadcast failed: {err}"));

            assert!(!sent, "{context}: broadcast must be skipped");
            assert_eq!(
                hook_calls.load(Ordering::Relaxed),
                0,
                "{context}: skipped broadcast must not run its hook"
            );
            assert!(
                coordinator.broadcasts_except.lock().await.is_empty(),
                "{context}: skipped broadcast must not delegate"
            );
        }
    }

    #[derive(Clone, Copy)]
    enum HookChange {
        None,
        StartDrain,
        RejectPredicate,
    }

    #[tokio::test]
    async fn default_conditional_broadcast_commits_once_hook_runs() {
        for (context, hook_change) in [
            ("normal delivery", HookChange::None),
            ("hook starts drain", HookChange::StartDrain),
            ("hook rejects predicate", HookChange::RejectPredicate),
        ] {
            let coordinator = FallbackCoordinator::default();
            let room_id = RoomId::from_u128(0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa);
            let sender = PlayerId::from_u128(0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb);
            let (drain_tx, drain_rx) = tokio::sync::watch::channel(false);
            let predicate_allows = Arc::new(AtomicBool::new(true));
            let predicate_for_call = Arc::clone(&predicate_allows);
            let should_send = move || predicate_for_call.load(Ordering::Relaxed);
            let predicate_for_hook = Arc::clone(&predicate_allows);
            let hook_calls = Arc::new(AtomicUsize::new(0));
            let hook_calls_for_hook = Arc::clone(&hook_calls);

            let sent = coordinator
                .broadcast_to_room_except_if_with_hook(
                    &room_id,
                    &sender,
                    test_message(),
                    &should_send,
                    drain_rx,
                    Box::new(move || {
                        Box::pin(async move {
                            hook_calls_for_hook.fetch_add(1, Ordering::Relaxed);
                            match hook_change {
                                HookChange::None => {}
                                HookChange::StartDrain => {
                                    let _ = drain_tx.send(true);
                                }
                                HookChange::RejectPredicate => {
                                    predicate_for_hook.store(false, Ordering::Relaxed);
                                }
                            }
                        })
                    }),
                )
                .await
                .unwrap_or_else(|err| panic!("{context}: conditional broadcast failed: {err}"));

            assert!(sent, "{context}: broadcast must commit once the hook runs");
            assert_eq!(
                hook_calls.load(Ordering::Relaxed),
                1,
                "{context}: hook should run exactly once"
            );
            let broadcasts = coordinator.broadcasts_except.lock().await;
            assert_eq!(
                broadcasts.len(),
                1,
                "{context}: committed broadcast must delegate exactly once"
            );
            let (broadcast_room, except_player, message) = &broadcasts[0];
            assert_eq!(*broadcast_room, room_id, "{context}: wrong broadcast room");
            assert_eq!(*except_player, sender, "{context}: wrong excluded sender");
            assert!(
                matches!(message, ServerMessage::Pong),
                "{context}: unexpected broadcast message: {message:?}"
            );
        }
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
            .try_send(test_message(), None)
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

    #[tokio::test]
    async fn default_initial_registration_distinguishes_zero_and_positive_losses() {
        let room_id = RoomId::from_u128(0x33333333333333333333333333333334);
        let coordinator = FallbackCoordinator::default();

        let (sender, _receiver) = outbound_queue::channel(1, 4);
        let sender = DeliverySender::classified(sender);
        sender.set_protocol_version(3);
        let room_sender = sender.next_generation();
        let transition = Arc::new(ServerMessage::SpectatorJoined(Box::new(
            SpectatorJoinedPayload {
                room_id,
                room_code: "LOSS01".to_string(),
                spectator_id: test_player(),
                game_name: "loss-test".to_string(),
                current_players: Vec::new(),
                current_spectators: Vec::new(),
                lobby_state: LobbyState::Waiting,
                reason: None,
            },
        )));
        assert_eq!(
            room_sender
                .try_send(transition, Some(room_id))
                .expect("establish the classified room scope"),
            QueueEnqueueOutcome {
                enqueued: true,
                losses: 0,
            }
        );
        assert_eq!(
            room_sender
                .try_send(
                    game_data_message(Some(1), Some(1), Some(DeliveryClass::Reliable), None,),
                    Some(room_id),
                )
                .expect("fill the one-slot data lane"),
            QueueEnqueueOutcome {
                enqueued: true,
                losses: 0,
            }
        );
        let (close, _listener) = ConnectionCloseSignal::channel();
        let accounted = coordinator
            .register_local_client_with_initial_message(
                PlayerId::from_u128(0x44444444444444444444444444444445),
                room_id,
                ClientDeliveryHandle {
                    sender: room_sender,
                    close,
                },
                Box::new(|| {
                    game_data_message(Some(2), Some(1), Some(DeliveryClass::Volatile), None)
                }),
            )
            .await
            .expect("a causally reported lossy omission is a valid outcome");
        assert_eq!(accounted, DeliveryOutcome::AccountedDrop);

        let (sender, _receiver) = outbound_queue::channel(1, 1);
        let stale_sender = DeliverySender::classified(sender);
        stale_sender.set_protocol_version(3);
        let (close, _listener) = ConnectionCloseSignal::channel();
        let canceled = coordinator
            .register_local_client_with_initial_message(
                PlayerId::from_u128(0x55555555555555555555555555555556),
                room_id,
                ClientDeliveryHandle {
                    sender: stale_sender,
                    close,
                },
                Box::new(test_message),
            )
            .await
            .expect("a stale zero-loss scope is a valid cancellation");
        assert_eq!(canceled, DeliveryOutcome::Canceled);
        assert!(
            coordinator.registrations.lock().await.is_empty(),
            "neither an accounted drop nor a cancellation publishes the route"
        );
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
            .try_send(test_message(), None)
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

    #[cfg(feature = "trace-validation")]
    #[tokio::test(start_paused = true)]
    async fn trace_records_parked_enqueue_after_capacity_returns() {
        let metrics = Arc::new(ServerMetrics::new());
        let (handle, mut rx, _listener, trace) = traced_delivery_handle(1, "parked-enqueue");
        assert_eq!(
            deliver_or_disconnect(
                &metrics,
                Duration::from_millis(10),
                &test_player(),
                &handle,
                test_message(),
            )
            .await,
            DeliveryOutcome::Delivered
        );
        let delivery = spawn_delivery(&metrics, &handle);
        let metrics_for_wait = Arc::clone(&metrics);
        yield_until("traced delivery must park", move || {
            metrics_for_wait
                .websocket_backpressure_events
                .load(Ordering::Relaxed)
                == 1
        })
        .await;

        let prefilled = rx.recv().await.expect("drain prefilled item");
        let write_id = handle
            .close
            .start_trace_write(&prefilled, false)
            .expect("prefilled delivery is correlated");
        handle.close.finish_trace_write(write_id, false);
        assert_eq!(
            delivery.await.expect("delivery task must not panic"),
            DeliveryOutcome::Delivered
        );
        assert_eq!(
            trace_actions(&trace),
            vec![
                "SendFast",
                "SendFull",
                "WriterStart",
                "WriterDrain",
                "ParkedEnqueue"
            ]
        );
    }

    #[cfg(feature = "trace-validation")]
    #[tokio::test(start_paused = true)]
    async fn trace_records_parked_channel_close_when_receiver_disappears() {
        let metrics = Arc::new(ServerMetrics::new());
        let (handle, rx, _listener, trace) = traced_delivery_handle(1, "parked-close");
        assert_eq!(
            deliver_or_disconnect(
                &metrics,
                Duration::from_millis(10),
                &test_player(),
                &handle,
                test_message(),
            )
            .await,
            DeliveryOutcome::Delivered
        );
        let delivery = spawn_delivery(&metrics, &handle);
        let metrics_for_wait = Arc::clone(&metrics);
        yield_until("traced delivery must park", move || {
            metrics_for_wait
                .websocket_backpressure_events
                .load(Ordering::Relaxed)
                == 1
        })
        .await;

        assert!(handle.close.request_close(CloseReason::Unregistered));
        handle.close.record_trace_queue_closed();
        drop(rx);
        assert_eq!(
            delivery.await.expect("delivery task must not panic"),
            DeliveryOutcome::ChannelClosed
        );
        assert_eq!(
            trace_actions(&trace),
            vec![
                "SendFast",
                "SendFull",
                "LifecycleClose",
                "QueueClose",
                "ParkedChannelClosed"
            ]
        );
    }

    #[cfg(feature = "trace-validation")]
    #[test]
    fn trace_rejects_generic_slow_consumer_close_as_out_of_model() {
        let trace = Arc::new(
            crate::trace_validation::DeliveryTraceRecorder::new("generic-slow-close", 1)
                .expect("valid delivery trace recorder"),
        );
        let (close, _listener) = ConnectionCloseSignal::channel_with_trace(Arc::clone(&trace));

        assert!(close.request_close(CloseReason::SlowConsumer));
        assert_eq!(trace_actions(&trace), vec!["Unsupported"]);
    }

    #[cfg(feature = "trace-validation")]
    #[tokio::test(start_paused = true)]
    async fn trace_retains_lifecycle_close_flush_when_parked_timeout_loses_race() {
        let metrics = Arc::new(ServerMetrics::new());
        let (handle, mut rx, _listener, trace) = traced_delivery_handle(1, "lifecycle-timeout");
        assert_eq!(
            deliver_or_disconnect(
                &metrics,
                Duration::from_millis(10),
                &test_player(),
                &handle,
                test_message(),
            )
            .await,
            DeliveryOutcome::Delivered
        );

        let delivery = spawn_delivery(&metrics, &handle);
        let metrics_for_wait = Arc::clone(&metrics);
        yield_until("traced delivery must park", move || {
            metrics_for_wait
                .websocket_backpressure_events
                .load(Ordering::Relaxed)
                == 1
        })
        .await;

        assert!(handle.close.request_close(CloseReason::Unregistered));
        assert_eq!(
            delivery.await.expect("delivery task must not panic"),
            DeliveryOutcome::SlowConsumer,
            "the parked delivery still expires even though lifecycle close won"
        );
        handle.close.record_trace_queue_closed();

        let prefilled = rx.recv().await.expect("drain prefilled item");
        let write_id = handle
            .close
            .start_trace_write(&prefilled, true)
            .expect("lifecycle close must retain its bounded final flush");
        handle.close.finish_trace_write(write_id, true);

        assert_eq!(
            trace_actions(&trace),
            vec![
                "SendFast",
                "SendFull",
                "LifecycleClose",
                "GraceExpired",
                "QueueClose",
                "CloseFlushStart",
                "CloseFlushDrain",
            ]
        );
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
            .try_send(test_message(), None)
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
            closed_with_timeout(&mut listener, "stuck recipient close").await,
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

    #[tokio::test(start_paused = true)]
    async fn classified_data_closed_race_is_abandoned_exactly_once() {
        let metrics = Arc::new(ServerMetrics::new());
        let (sender, receiver) = outbound_queue::channel_with_metrics(1, 1, Arc::clone(&metrics));
        let (close, _listener) = ConnectionCloseSignal::channel();
        let handle = ClientDeliveryHandle::classified(sender, close);
        drop(receiver);

        let outcome = deliver_or_disconnect(
            &metrics,
            TEST_TIMEOUT,
            &test_player(),
            &handle,
            Arc::new(ServerMessage::GameData {
                from_player: PlayerId::from_u128(7),
                data: serde_json::json!({"state": 1}),
                seq: None,
                epoch: None,
                class: None,
                key: None,
            }),
        )
        .await;

        assert_eq!(outcome, DeliveryOutcome::ChannelClosed);
        let reliable = metrics.delivery_metrics_by_class().reliable;
        assert_eq!(reliable.attempted, 1);
        assert_eq!(reliable.abandoned, 1);
        assert_eq!(
            reliable.delivered
                + reliable.superseded
                + reliable.dropped_full
                + reliable.dropped
                + reliable.abandoned
                + reliable.unsupported_format,
            reliable.attempted
        );
    }

    /// (e) Receiver dropped while the delivery is already backpressured: the
    /// parked send fails over to `ChannelClosed`, still not a drop.
    #[tokio::test(start_paused = true)]
    async fn receiver_dropped_while_backpressured_is_channel_closed() {
        let metrics = Arc::new(ServerMetrics::new());
        let (handle, rx, _listener) = delivery_handle(1);
        handle
            .sender
            .try_send(test_message(), None)
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

    /// (f) First non-shutdown close reason wins: a second ordinary request is
    /// a no-op (returns false) and the listener observes the original reason.
    #[tokio::test]
    async fn close_signal_first_non_shutdown_reason_wins_and_listener_observes_it() {
        let (signal, mut listener) = ConnectionCloseSignal::channel();

        assert!(
            signal.request_close(CloseReason::SlowConsumer),
            "the first close request must set the reason"
        );
        assert!(
            !signal.request_close(CloseReason::Unregistered),
            "a second close request must be a no-op"
        );

        assert_eq!(
            closed_with_timeout(&mut listener, "first non-shutdown close").await,
            Some(CloseReason::SlowConsumer)
        );
        // The listener is level-triggered: once closed, it stays closed with
        // the same (first) reason.
        assert_eq!(
            closed_with_timeout(&mut listener, "repeated first non-shutdown close").await,
            Some(CloseReason::SlowConsumer)
        );
    }

    /// Shutdown drain is the priority lifecycle close: it must be able to
    /// restore the semantic 4000 close when activity cleanup raced first.
    #[tokio::test]
    async fn close_signal_shutdown_supersedes_previous_lifecycle_reason() {
        let (signal, mut listener) = ConnectionCloseSignal::channel();

        assert!(signal.request_close(CloseReason::ActivityTimeout));
        assert!(signal.request_close(CloseReason::Shutdown));
        assert!(
            !signal.request_close(CloseReason::SlowConsumer),
            "shutdown remains the final reason once requested"
        );

        assert_eq!(
            closed_with_timeout(&mut listener, "shutdown close").await,
            Some(CloseReason::Shutdown)
        );
        assert_eq!(
            closed_with_timeout(&mut listener, "repeated shutdown close").await,
            Some(CloseReason::Shutdown)
        );
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
        let waiter = tokio::spawn(async move {
            closed_with_timeout(&mut waiting_listener, "waiting dropped-signal listener").await
        });
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
        assert_eq!(
            closed_with_timeout(&mut late_listener, "late dropped-signal listener").await,
            None
        );
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

        // First non-shutdown reason wins in the peek too.
        assert!(!signal.request_close(CloseReason::Unregistered));
        assert_eq!(listener.requested_reason(), Some(CloseReason::IdleTimeout));

        assert!(signal.request_close(CloseReason::Shutdown));
        assert_eq!(listener.requested_reason(), Some(CloseReason::Shutdown));
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
