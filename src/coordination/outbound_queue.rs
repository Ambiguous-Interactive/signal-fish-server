//! Bounded per-connection outbound queue with protocol-v3 delivery classes.
//!
//! Protocol v2 uses one FIFO lane. Once a connection negotiates v3, control
//! traffic and delivery reports use a dedicated priority lane while game data
//! uses a separately bounded data lane. Lossy data operations reserve report
//! capacity before changing the data queue, so every observable sequence gap
//! has a causally prior exact report.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU16, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::Notify;
use tokio::time::{Duration, Instant};

use crate::protocol::{
    DeliveryClass, DeliveryCountersByClass, DeliveryGap, DeliveryGapReason, DeliveryReportPayload,
    GameDataEncoding, PlayerId, RoomId, ServerMessage, DELIVERY_REPORT_MAX_GAPS,
};

/// Delivery metadata carried beside a stamped game-data message in memory.
///
/// `room_id` participates in the latest-value key but is intentionally absent
/// from [`DeliveryGap`]: a recipient has at most one active room and already
/// knows which room its sequence stream belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataDeliveryMetadata {
    pub class: DeliveryClass,
    pub key: Option<u32>,
    pub from_player: PlayerId,
    pub room_id: RoomId,
    pub epoch: u32,
    pub seq: u64,
}

impl DataDeliveryMetadata {
    fn latest_key(self) -> Option<LatestKey> {
        if self.class != DeliveryClass::Latest {
            return None;
        }
        let key = self.key?;
        Some(LatestKey {
            from_player: self.from_player,
            room_id: self.room_id,
            key,
        })
    }

    fn gap(self, reason: DeliveryGapReason) -> DeliveryGap {
        DeliveryGap {
            from_player: self.from_player,
            epoch: self.epoch,
            from_seq: self.seq,
            to_seq: self.seq,
            reason,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LatestKey {
    from_player: PlayerId,
    room_id: RoomId,
    key: u32,
}

/// One game-data item submitted to the outbound queue.
#[derive(Debug)]
pub struct OutboundData {
    pub message: Arc<ServerMessage>,
    /// Stamped metadata is present for normal routed game data. A reliable
    /// unstamped item remains supported for disconnect races and test doubles;
    /// lossy classes require metadata so a gap can be reported exactly.
    pub metadata: Option<DataDeliveryMetadata>,
}

impl OutboundData {
    pub fn new(message: Arc<ServerMessage>, metadata: DataDeliveryMetadata) -> Self {
        Self {
            message,
            metadata: Some(metadata),
        }
    }

    pub fn reliable_unstamped(message: Arc<ServerMessage>) -> Self {
        Self {
            message,
            metadata: None,
        }
    }

    fn class(&self) -> DeliveryClass {
        self.metadata
            .map_or(DeliveryClass::Reliable, |metadata| metadata.class)
    }

    fn metadata_matches_message(&self) -> bool {
        let Some(metadata) = self.metadata else {
            return match self.message.as_ref() {
                ServerMessage::GameData {
                    seq,
                    epoch,
                    class,
                    key,
                    ..
                } => {
                    class.unwrap_or_default() == DeliveryClass::Reliable
                        && key.is_none()
                        && valid_class_key(*class, *key)
                        && seq.is_none()
                        && epoch.is_none()
                }
                ServerMessage::GameDataBinary { seq, epoch, .. } => {
                    seq.is_none() && epoch.is_none()
                }
                _ => false,
            };
        };
        if !valid_class_key(Some(metadata.class), metadata.key) {
            return false;
        }
        match self.message.as_ref() {
            ServerMessage::GameData {
                from_player,
                seq,
                epoch,
                class,
                key,
                ..
            } => {
                *from_player == metadata.from_player
                    && *seq == Some(metadata.seq)
                    && *epoch == Some(metadata.epoch)
                    && class.unwrap_or_default() == metadata.class
                    && *key == metadata.key
                    && valid_class_key(*class, *key)
            }
            ServerMessage::GameDataBinary {
                from_player,
                seq,
                epoch,
                ..
            } => {
                *from_player == metadata.from_player
                    && *seq == Some(metadata.seq)
                    && *epoch == Some(metadata.epoch)
                    && metadata.class == DeliveryClass::Reliable
                    && metadata.key.is_none()
            }
            _ => false,
        }
    }
}

/// Queue payload. Delivery reports are represented directly until the socket
/// writer materializes the corresponding `ServerMessage` frame.
#[derive(Debug, Clone)]
pub enum OutboundPayload {
    Message(Arc<ServerMessage>),
    DeliveryReport(DeliveryReportPayload),
}

/// One dequeued outbound item with its original enqueue timestamp.
#[derive(Debug, Clone)]
pub struct QueuedOutbound {
    pub payload: OutboundPayload,
    delivery_class: Option<DeliveryClass>,
    pub metadata: Option<DataDeliveryMetadata>,
    pub enqueued_at: Instant,
    generation: u64,
    transition_barrier: bool,
}

impl QueuedOutbound {
    pub fn is_control(&self) -> bool {
        self.delivery_class.is_none()
    }

    pub fn class(&self) -> Option<DeliveryClass> {
        self.delivery_class
    }

    #[cfg(test)]
    pub(crate) fn test_control(message: Arc<ServerMessage>) -> Self {
        Self {
            payload: OutboundPayload::Message(message),
            delivery_class: None,
            metadata: None,
            enqueued_at: Instant::now(),
            generation: 0,
            transition_barrier: false,
        }
    }
}

#[cfg(test)]
impl From<Arc<ServerMessage>> for QueuedOutbound {
    fn from(message: Arc<ServerMessage>) -> Self {
        Self::test_control(message)
    }
}

/// Successful queue operation and any older/current data loss it accounted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnqueueOutcome {
    /// Whether the submitted item itself remains queued.
    pub enqueued: bool,
    /// Number of data items omitted by this operation. Every loss has an exact
    /// report queued before any successor data can be dequeued.
    pub losses: u64,
}

impl EnqueueOutcome {
    const ENQUEUED: Self = Self {
        enqueued: true,
        losses: 0,
    };
    const CANCELED: Self = Self {
        enqueued: false,
        losses: 0,
    };
}

/// Non-blocking enqueue failure, preserving ownership of the submitted item.
#[derive(Debug)]
pub enum TryEnqueueError<T> {
    Full(T),
    Closed(T),
    /// A lossy operation could not reserve causal report capacity. The caller
    /// must close the connection loudly instead of continuing its data stream.
    AccountabilityUnavailable(T),
    /// A lossy class was missing the stamp/key needed for exact accountability.
    InvalidMetadata(T),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lane {
    Legacy,
    Control,
    Data,
}

#[derive(Debug)]
struct QueueState {
    legacy: VecDeque<QueuedOutbound>,
    control: VecDeque<QueuedOutbound>,
    data: VecDeque<QueuedOutbound>,
    barriers: VecDeque<QueuedOutbound>,
    reserved_legacy: usize,
    reserved_control: usize,
    reserved_data: usize,
    receiver_open: bool,
    accountability_failed: bool,
    sender_count: usize,
    permit_count: usize,
    counters: DeliveryCountersByClass,
    wire_counters: DeliveryCountersByClass,
    pending_unsupported: PendingUnsupported,
    enqueue_generation: u64,
    receive_generation: u64,
    active_room: Option<RoomId>,
}

/// Exact omissions discovered by the socket writer that have not reached the
/// wire yet, coalesced under the same merge rule the queue applies to its own
/// gap reports ([`try_append_gap`]).
///
/// `QueueState::wire_counters` deliberately does **not** advance while these are
/// pending: a recipient must never see an `unsupported_format` counter move
/// without the exact ranges that explain it, so the increments and their gaps
/// always reach the socket in the same frame. The per-class counts are carried
/// alongside the ranges because [`DeliveryGap`] does not name a delivery class,
/// and the flushed report must advance exactly the counters its ranges explain.
///
/// Ranges are bucketed by delivery class for that reason: `UnsupportedFormat`
/// is the one gap reason every class shares, so a single list could merge two
/// adjacent omissions of different classes into one range and lose the
/// attribution the counters need. Merging within a bucket keeps the per-class
/// totals derivable from the ranges themselves.
#[derive(Debug, Default)]
struct PendingUnsupported {
    reliable: Vec<DeliveryGap>,
    latest: Vec<DeliveryGap>,
    volatile: Vec<DeliveryGap>,
    /// When the oldest unsent range was recorded, so an idle writer still tells
    /// the recipient about it within a bounded time instead of holding it until
    /// the next omission or the connection's close.
    since: Option<Instant>,
}

impl PendingUnsupported {
    fn bucket(&mut self, class: DeliveryClass) -> &mut Vec<DeliveryGap> {
        match class {
            DeliveryClass::Reliable => &mut self.reliable,
            DeliveryClass::Latest => &mut self.latest,
            DeliveryClass::Volatile => &mut self.volatile,
        }
    }

    fn len(&self) -> usize {
        self.reliable.len() + self.latest.len() + self.volatile.len()
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Merge one omission into the pending report. `false` means the report
    /// cannot absorb it and must be flushed first.
    fn append(&mut self, gap: &DeliveryGap, class: DeliveryClass) -> bool {
        // One report carries at most `DELIVERY_REPORT_MAX_GAPS` ranges however
        // they are distributed across classes, so the bound is checked against
        // the combined length before a bucket is allowed to grow.
        let would_grow = self
            .bucket(class)
            .last()
            .is_none_or(|previous| !gaps_merge(previous, gap));
        if would_grow && self.len() >= DELIVERY_REPORT_MAX_GAPS {
            return false;
        }
        if self.is_empty() {
            self.since = Some(Instant::now());
        }
        append_gap(self.bucket(class), gap)
    }

    /// When this report must reach the recipient even if nothing else is
    /// written.
    fn flush_deadline(&self) -> Option<Instant> {
        self.since
            .map(|since| since + PENDING_UNSUPPORTED_FLUSH_INTERVAL)
    }

    /// Take the pending ranges, advancing `wire` by exactly the omissions they
    /// account for.
    fn take(&mut self, wire: &mut DeliveryCountersByClass) -> Option<DeliveryReportPayload> {
        if self.is_empty() {
            return None;
        }
        self.since = None;
        let mut gaps = Vec::with_capacity(self.len());
        for (bucket, counter) in [
            (
                std::mem::take(&mut self.reliable),
                &mut wire.reliable.unsupported_format,
            ),
            (
                std::mem::take(&mut self.latest),
                &mut wire.latest.unsupported_format,
            ),
            (
                std::mem::take(&mut self.volatile),
                &mut wire.volatile.unsupported_format,
            ),
        ] {
            for gap in bucket {
                *counter = counter
                    .saturating_add(gap.to_seq.saturating_sub(gap.from_seq).saturating_add(1));
                gaps.push(gap);
            }
        }
        Some(DeliveryReportPayload {
            per_class: *wire,
            gaps,
        })
    }
}

impl QueueState {
    fn new() -> Self {
        Self {
            legacy: VecDeque::new(),
            control: VecDeque::new(),
            data: VecDeque::new(),
            barriers: VecDeque::new(),
            reserved_legacy: 0,
            reserved_control: 0,
            reserved_data: 0,
            receiver_open: true,
            accountability_failed: false,
            sender_count: 1,
            permit_count: 0,
            counters: DeliveryCountersByClass::default(),
            wire_counters: DeliveryCountersByClass::default(),
            pending_unsupported: PendingUnsupported::default(),
            enqueue_generation: 0,
            receive_generation: 0,
            active_room: None,
        }
    }

    fn len(&self) -> usize {
        self.legacy.len() + self.control.len() + self.data.len() + self.barriers.len()
    }

    fn accepting(&self) -> bool {
        self.receiver_open && !self.accountability_failed
    }

    fn producers_open(&self) -> bool {
        self.sender_count > 0 || self.permit_count > 0
    }

    fn lane_len_with_reservations(&self, lane: Lane) -> usize {
        match lane {
            Lane::Legacy => self.legacy.len() + self.reserved_legacy,
            Lane::Control => self.control.len() + self.barriers.len() + self.reserved_control,
            Lane::Data => self.data.len() + self.reserved_data,
        }
    }

    fn reserve(&mut self, lane: Lane) {
        match lane {
            Lane::Legacy => self.reserved_legacy += 1,
            Lane::Control => self.reserved_control += 1,
            Lane::Data => self.reserved_data += 1,
        }
    }

    fn release_reservation(&mut self, lane: Lane) {
        let reserved = match lane {
            Lane::Legacy => &mut self.reserved_legacy,
            Lane::Control => &mut self.reserved_control,
            Lane::Data => &mut self.reserved_data,
        };
        debug_assert!(*reserved > 0);
        *reserved = reserved.saturating_sub(1);
    }
}

#[derive(Debug)]
struct SharedQueue {
    state: Mutex<QueueState>,
    item_available: Notify,
    capacity_available: Notify,
    protocol_version: AtomicU16,
    game_data_format: AtomicU8,
    unsupported_notices: Mutex<UnsupportedNoticeLimiter>,
    data_capacity: usize,
    control_capacity: usize,
    metrics: Option<Arc<crate::metrics::ServerMetrics>>,
}

const UNSUPPORTED_NOTICE_INTERVAL: Duration = Duration::from_secs(1);
/// How long the writer may keep coalescing undeliverable omissions before the
/// recipient is told, when nothing else is being written to it.
///
/// Matched to [`UNSUPPORTED_NOTICE_INTERVAL`] so the exact report and its prose
/// advisory share one cadence: at most one of each per second, instead of one of
/// each per omitted message.
const PENDING_UNSUPPORTED_FLUSH_INTERVAL: Duration = UNSUPPORTED_NOTICE_INTERVAL;
const MAX_UNSUPPORTED_NOTICE_SENDERS: usize = 256;

#[derive(Debug, Default)]
struct UnsupportedNoticeLimiter {
    senders: BTreeMap<PlayerId, UnsupportedNoticeState>,
}

#[derive(Debug, Clone, Copy)]
struct UnsupportedNoticeState {
    last_emitted: Instant,
    suppressed: u64,
}

impl UnsupportedNoticeLimiter {
    fn observe(&mut self, sender: PlayerId, now: Instant) -> Option<u64> {
        if let Some(state) = self.senders.get_mut(&sender) {
            if now.duration_since(state.last_emitted) < UNSUPPORTED_NOTICE_INTERVAL {
                state.suppressed = state.suppressed.saturating_add(1);
                return None;
            }
            let suppressed = std::mem::take(&mut state.suppressed);
            state.last_emitted = now;
            return Some(suppressed);
        }

        if self.senders.len() >= MAX_UNSUPPORTED_NOTICE_SENDERS {
            let oldest = self
                .senders
                .iter()
                .min_by_key(|(_, state)| state.last_emitted)
                .map(|(sender, _)| *sender);
            if let Some(oldest) = oldest {
                self.senders.remove(&oldest);
            }
        }
        self.senders.insert(
            sender,
            UnsupportedNoticeState {
                last_emitted: now,
                suppressed: 0,
            },
        );
        Some(0)
    }
}

impl SharedQueue {
    fn state(&self) -> MutexGuard<'_, QueueState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn capacity(&self, lane: Lane) -> usize {
        match lane {
            Lane::Legacy | Lane::Data => self.data_capacity,
            Lane::Control => self.control_capacity,
        }
    }

    fn has_capacity(&self, state: &QueueState, lane: Lane) -> bool {
        state.lane_len_with_reservations(lane) < self.capacity(lane)
    }

    fn v3(&self) -> bool {
        self.protocol_version.load(Ordering::Acquire) >= 3
    }

    fn record_attempted(&self, class: DeliveryClass) {
        if let Some(metrics) = &self.metrics {
            metrics.increment_delivery_class_attempted(class);
        }
    }

    fn record_delivered(&self, class: DeliveryClass) {
        if let Some(metrics) = &self.metrics {
            metrics.increment_delivery_class_delivered(class);
        }
    }

    fn record_superseded(&self) {
        if let Some(metrics) = &self.metrics {
            metrics.increment_delivery_class_superseded();
        }
    }

    fn record_dropped_full(&self) {
        if let Some(metrics) = &self.metrics {
            metrics.increment_delivery_class_dropped_full();
        }
    }

    fn record_dropped(&self, class: DeliveryClass) {
        if let Some(metrics) = &self.metrics {
            metrics.increment_delivery_class_dropped(class);
        }
    }

    fn record_abandoned(&self, class: DeliveryClass, count: u64) {
        if let Some(metrics) = &self.metrics {
            metrics.add_delivery_class_abandoned(class, count);
        }
    }

    fn record_unsupported(&self, class: DeliveryClass) {
        if let Some(metrics) = &self.metrics {
            metrics.increment_delivery_class_unsupported_format(class);
        }
    }
}

/// Sender half of a bounded outbound queue.
#[derive(Debug)]
pub struct OutboundSender {
    shared: Arc<SharedQueue>,
}

impl Clone for OutboundSender {
    fn clone(&self) -> Self {
        self.shared.state().sender_count += 1;
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl Drop for OutboundSender {
    fn drop(&mut self) {
        let no_producers = {
            let mut state = self.shared.state();
            debug_assert!(state.sender_count > 0);
            state.sender_count = state.sender_count.saturating_sub(1);
            !state.producers_open()
        };
        if no_producers {
            self.shared.item_available.notify_waiters();
        }
    }
}

/// Receiver half of a bounded outbound queue. There is exactly one socket
/// writer per connection, so this type intentionally is not cloneable.
#[derive(Debug)]
pub struct OutboundReceiver {
    shared: Arc<SharedQueue>,
    batch_remaining: usize,
}

impl Drop for OutboundReceiver {
    fn drop(&mut self) {
        self.close();
    }
}

/// Create one bounded outbound queue. Both capacities must be nonzero; config
/// validation enforces that invariant before production reaches this function.
#[cfg(test)]
pub fn channel(
    data_capacity: usize,
    control_capacity: usize,
) -> (OutboundSender, OutboundReceiver) {
    channel_inner(data_capacity, control_capacity, None)
}

pub fn channel_with_metrics(
    data_capacity: usize,
    control_capacity: usize,
    metrics: Arc<crate::metrics::ServerMetrics>,
) -> (OutboundSender, OutboundReceiver) {
    channel_inner(data_capacity, control_capacity, Some(metrics))
}

fn channel_inner(
    data_capacity: usize,
    control_capacity: usize,
    metrics: Option<Arc<crate::metrics::ServerMetrics>>,
) -> (OutboundSender, OutboundReceiver) {
    assert!(data_capacity > 0, "outbound data capacity must be nonzero");
    assert!(
        control_capacity > 0,
        "outbound control capacity must be nonzero"
    );
    let shared = Arc::new(SharedQueue {
        state: Mutex::new(QueueState::new()),
        item_available: Notify::new(),
        capacity_available: Notify::new(),
        protocol_version: AtomicU16::new(crate::config::SERVER_MIN_PROTOCOL_VERSION),
        game_data_format: AtomicU8::new(encoding_tag(GameDataEncoding::Json)),
        unsupported_notices: Mutex::new(UnsupportedNoticeLimiter::default()),
        data_capacity,
        control_capacity,
        metrics,
    });
    (
        OutboundSender {
            shared: Arc::clone(&shared),
        },
        OutboundReceiver {
            shared,
            batch_remaining: 0,
        },
    )
}

impl OutboundSender {
    pub fn delivery_classes_enabled(&self) -> bool {
        self.shared.v3()
    }

    pub fn record_abandoned(&self, class: DeliveryClass, count: u64) {
        let mut state = self.shared.state();
        increment_abandoned(&mut state.counters, class, count);
        drop(state);
        self.shared.record_abandoned(class, count);
    }

    pub fn record_rejected_with_close(&self, class: DeliveryClass) {
        self.shared.record_attempted(class);
        self.record_abandoned(class, 1);
    }

    pub fn set_protocol_version(&self, version: u16) {
        self.shared
            .protocol_version
            .store(version, Ordering::Release);
    }

    pub fn set_game_data_format(&self, format: GameDataEncoding) {
        self.shared
            .game_data_format
            .store(encoding_tag(format), Ordering::Release);
    }

    pub fn same_channel(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }

    #[cfg(test)]
    pub fn is_closed(&self) -> bool {
        !self.shared.state().accepting()
    }

    pub fn try_enqueue_control(
        &self,
        message: Arc<ServerMessage>,
    ) -> Result<EnqueueOutcome, TryEnqueueError<Arc<ServerMessage>>> {
        self.try_enqueue_control_inner(message, None)
    }

    pub fn try_enqueue_control_scoped(
        &self,
        message: Arc<ServerMessage>,
        room_id: Option<RoomId>,
        generation: u64,
    ) -> Result<EnqueueOutcome, TryEnqueueError<Arc<ServerMessage>>> {
        self.try_enqueue_control_inner(message, Some((generation, room_id)))
    }

    fn try_enqueue_control_inner(
        &self,
        message: Arc<ServerMessage>,
        expected_scope: Option<(u64, Option<RoomId>)>,
    ) -> Result<EnqueueOutcome, TryEnqueueError<Arc<ServerMessage>>> {
        if is_data_message(&message) {
            return Err(TryEnqueueError::InvalidMetadata(message));
        }
        let lane = if self.shared.v3() {
            Lane::Control
        } else {
            Lane::Legacy
        };
        let mut state = self.shared.state();
        if !state.accepting() {
            return Err(TryEnqueueError::Closed(message));
        }
        if expected_scope
            .is_some_and(|(generation, room_id)| !scope_matches(&state, generation, room_id))
        {
            return Ok(EnqueueOutcome::CANCELED);
        }
        if !self.shared.has_capacity(&state, lane) {
            return Err(TryEnqueueError::Full(message));
        }
        push_message(&mut state, lane, message, None, None);
        drop(state);
        self.shared.item_available.notify_one();
        Ok(EnqueueOutcome::ENQUEUED)
    }

    pub async fn enqueue_control_scoped(
        &self,
        mut message: Arc<ServerMessage>,
        room_id: Option<RoomId>,
        generation: u64,
    ) -> Result<EnqueueOutcome, TryEnqueueError<Arc<ServerMessage>>> {
        loop {
            let notified = self.shared.capacity_available.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.try_enqueue_control_scoped(message, room_id, generation) {
                Ok(outcome) => return Ok(outcome),
                Err(TryEnqueueError::Full(returned)) => message = returned,
                Err(other) => return Err(other),
            }
            notified.as_mut().await;
        }
    }

    #[cfg(test)]
    pub fn try_enqueue_data(
        &self,
        data: OutboundData,
    ) -> Result<EnqueueOutcome, TryEnqueueError<OutboundData>> {
        self.try_enqueue_data_inner(data, None)
    }

    pub fn try_enqueue_data_scoped(
        &self,
        data: OutboundData,
        generation: u64,
    ) -> Result<EnqueueOutcome, TryEnqueueError<OutboundData>> {
        let room_id = data.metadata.map(|metadata| metadata.room_id);
        self.try_enqueue_data_inner(data, Some((generation, room_id)))
    }

    fn try_enqueue_data_inner(
        &self,
        data: OutboundData,
        expected_scope: Option<(u64, Option<RoomId>)>,
    ) -> Result<EnqueueOutcome, TryEnqueueError<OutboundData>> {
        if !data.metadata_matches_message() {
            return Err(TryEnqueueError::InvalidMetadata(data));
        }
        if !self.shared.v3() {
            return self.try_enqueue_legacy_data(data, expected_scope);
        }

        match data.class() {
            DeliveryClass::Reliable => self.try_enqueue_reliable_data(data, expected_scope),
            DeliveryClass::Latest => self.try_enqueue_latest(data, expected_scope),
            DeliveryClass::Volatile => self.try_enqueue_volatile(data, expected_scope),
        }
    }

    pub async fn enqueue_data_scoped(
        &self,
        mut data: OutboundData,
        generation: u64,
    ) -> Result<EnqueueOutcome, TryEnqueueError<OutboundData>> {
        loop {
            let notified = self.shared.capacity_available.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.try_enqueue_data_scoped(data, generation) {
                Ok(outcome) => return Ok(outcome),
                Err(TryEnqueueError::Full(returned)) => data = returned,
                Err(other) => return Err(other),
            }
            notified.as_mut().await;
        }
    }

    fn try_enqueue_legacy_data(
        &self,
        data: OutboundData,
        expected_scope: Option<(u64, Option<RoomId>)>,
    ) -> Result<EnqueueOutcome, TryEnqueueError<OutboundData>> {
        let mut state = self.shared.state();
        if !state.accepting() {
            return Err(TryEnqueueError::Closed(data));
        }
        if expected_scope
            .is_some_and(|(generation, room_id)| !scope_matches(&state, generation, room_id))
        {
            return Ok(EnqueueOutcome::CANCELED);
        }
        if !self.shared.has_capacity(&state, Lane::Legacy) {
            return Err(TryEnqueueError::Full(data));
        }
        push_data(&mut state, Lane::Legacy, data, DeliveryClass::Reliable);
        self.shared.record_attempted(DeliveryClass::Reliable);
        drop(state);
        self.shared.item_available.notify_one();
        Ok(EnqueueOutcome::ENQUEUED)
    }

    fn try_enqueue_reliable_data(
        &self,
        data: OutboundData,
        expected_scope: Option<(u64, Option<RoomId>)>,
    ) -> Result<EnqueueOutcome, TryEnqueueError<OutboundData>> {
        let mut state = self.shared.state();
        if !state.accepting() {
            return Err(TryEnqueueError::Closed(data));
        }
        if expected_scope
            .is_some_and(|(generation, room_id)| !scope_matches(&state, generation, room_id))
        {
            return Ok(EnqueueOutcome::CANCELED);
        }
        if !self.shared.has_capacity(&state, Lane::Data) {
            return Err(TryEnqueueError::Full(data));
        }
        push_data(&mut state, Lane::Data, data, DeliveryClass::Reliable);
        self.shared.record_attempted(DeliveryClass::Reliable);
        drop(state);
        self.shared.item_available.notify_one();
        Ok(EnqueueOutcome::ENQUEUED)
    }

    fn try_enqueue_latest(
        &self,
        data: OutboundData,
        expected_scope: Option<(u64, Option<RoomId>)>,
    ) -> Result<EnqueueOutcome, TryEnqueueError<OutboundData>> {
        let Some(metadata) = data.metadata else {
            return Err(TryEnqueueError::InvalidMetadata(data));
        };
        let Some(latest_key) = metadata.latest_key() else {
            return Err(TryEnqueueError::InvalidMetadata(data));
        };

        let mut state = self.shared.state();
        if !state.accepting() {
            return Err(TryEnqueueError::Closed(data));
        }
        if expected_scope
            .is_some_and(|(generation, room_id)| !scope_matches(&state, generation, room_id))
        {
            return Ok(EnqueueOutcome::CANCELED);
        }

        if let Some(position) = state.data.iter().position(|queued| {
            queued.generation == state.enqueue_generation
                && queued.metadata.and_then(DataDeliveryMetadata::latest_key) == Some(latest_key)
        }) {
            let Some(predecessor_metadata) = state
                .data
                .get(position)
                .and_then(|predecessor| predecessor.metadata)
            else {
                return self.fail_accountability(state, data);
            };
            let gap = predecessor_metadata.gap(DeliveryGapReason::LatestSuperseded);
            if !can_record_gap(&state, self.shared.control_capacity, &gap) {
                return self.fail_accountability(state, data);
            }
            state.data.remove(position);
            state.counters.latest.superseded = state.counters.latest.superseded.saturating_add(1);
            self.shared.record_superseded();
            enqueue_gap_report(&mut state, gap);
            push_data(&mut state, Lane::Data, data, DeliveryClass::Latest);
            self.shared.record_attempted(DeliveryClass::Latest);
            drop(state);
            self.shared.item_available.notify_waiters();
            return Ok(EnqueueOutcome {
                enqueued: true,
                losses: 1,
            });
        }

        if self.shared.has_capacity(&state, Lane::Data) {
            push_data(&mut state, Lane::Data, data, DeliveryClass::Latest);
            self.shared.record_attempted(DeliveryClass::Latest);
            drop(state);
            self.shared.item_available.notify_one();
            return Ok(EnqueueOutcome::ENQUEUED);
        }

        if let Some(position) = oldest_volatile_position(&state.data, state.enqueue_generation) {
            let Some(evicted_metadata) = state
                .data
                .get(position)
                .and_then(|evicted| evicted.metadata)
            else {
                return self.fail_accountability(state, data);
            };
            let gap = evicted_metadata.gap(DeliveryGapReason::VolatileDropped);
            if !can_record_gap(&state, self.shared.control_capacity, &gap) {
                return self.fail_accountability(state, data);
            }
            state.data.remove(position);
            state.counters.volatile.dropped = state.counters.volatile.dropped.saturating_add(1);
            self.shared.record_dropped(DeliveryClass::Volatile);
            enqueue_gap_report(&mut state, gap);
            push_data(&mut state, Lane::Data, data, DeliveryClass::Latest);
            self.shared.record_attempted(DeliveryClass::Latest);
            drop(state);
            self.shared.item_available.notify_waiters();
            Ok(EnqueueOutcome {
                enqueued: true,
                losses: 1,
            })
        } else {
            let gap = metadata.gap(DeliveryGapReason::LatestDroppedFull);
            if !can_record_gap(&state, self.shared.control_capacity, &gap) {
                return self.fail_accountability(state, data);
            }
            state.counters.latest.dropped_full =
                state.counters.latest.dropped_full.saturating_add(1);
            self.shared.record_attempted(DeliveryClass::Latest);
            self.shared.record_dropped_full();
            enqueue_gap_report(&mut state, gap);
            drop(state);
            self.shared.item_available.notify_one();
            Ok(EnqueueOutcome {
                enqueued: false,
                losses: 1,
            })
        }
    }

    fn try_enqueue_volatile(
        &self,
        data: OutboundData,
        expected_scope: Option<(u64, Option<RoomId>)>,
    ) -> Result<EnqueueOutcome, TryEnqueueError<OutboundData>> {
        let Some(metadata) = data.metadata else {
            return Err(TryEnqueueError::InvalidMetadata(data));
        };
        let mut state = self.shared.state();
        if !state.accepting() {
            return Err(TryEnqueueError::Closed(data));
        }
        if expected_scope
            .is_some_and(|(generation, room_id)| !scope_matches(&state, generation, room_id))
        {
            return Ok(EnqueueOutcome::CANCELED);
        }
        if self.shared.has_capacity(&state, Lane::Data) {
            push_data(&mut state, Lane::Data, data, DeliveryClass::Volatile);
            self.shared.record_attempted(DeliveryClass::Volatile);
            drop(state);
            self.shared.item_available.notify_one();
            return Ok(EnqueueOutcome::ENQUEUED);
        }
        let volatile_position = oldest_volatile_position(&state.data, state.enqueue_generation);
        let gap = match volatile_position {
            Some(position) => {
                let Some(evicted) = state.data.get(position).and_then(|queued| queued.metadata)
                else {
                    return self.fail_accountability(state, data);
                };
                evicted.gap(DeliveryGapReason::VolatileDropped)
            }
            None => metadata.gap(DeliveryGapReason::VolatileDropped),
        };
        if !can_record_gap(&state, self.shared.control_capacity, &gap) {
            return self.fail_accountability(state, data);
        }

        state.counters.volatile.dropped = state.counters.volatile.dropped.saturating_add(1);
        self.shared.record_attempted(DeliveryClass::Volatile);
        self.shared.record_dropped(DeliveryClass::Volatile);
        if let Some(position) = volatile_position {
            state.data.remove(position);
            enqueue_gap_report(&mut state, gap);
            push_data(&mut state, Lane::Data, data, DeliveryClass::Volatile);
            drop(state);
            self.shared.item_available.notify_waiters();
            Ok(EnqueueOutcome {
                enqueued: true,
                losses: 1,
            })
        } else {
            enqueue_gap_report(&mut state, gap);
            drop(state);
            self.shared.item_available.notify_one();
            Ok(EnqueueOutcome {
                enqueued: false,
                losses: 1,
            })
        }
    }

    pub fn try_enqueue_transition(
        &self,
        message: Arc<ServerMessage>,
        target_generation: u64,
    ) -> Result<EnqueueOutcome, TryEnqueueError<Arc<ServerMessage>>> {
        let Some(target_room) = transition_target(message.as_ref()) else {
            return Err(TryEnqueueError::InvalidMetadata(message));
        };
        let v3 = self.shared.v3();
        let lane = if v3 { Lane::Control } else { Lane::Legacy };
        let mut state = self.shared.state();
        if !state.accepting() {
            return Err(TryEnqueueError::Closed(message));
        }
        if target_generation != state.enqueue_generation.saturating_add(1) {
            return Ok(EnqueueOutcome::CANCELED);
        }
        if !self.shared.has_capacity(&state, lane) {
            return Err(TryEnqueueError::Full(message));
        }

        let barrier = QueuedOutbound {
            payload: OutboundPayload::Message(message),
            delivery_class: None,
            metadata: None,
            enqueued_at: Instant::now(),
            generation: target_generation,
            transition_barrier: true,
        };
        if v3 {
            state.barriers.push_back(barrier);
        } else {
            state.legacy.push_back(barrier);
        }
        state.enqueue_generation = target_generation;
        state.active_room = target_room;
        drop(state);
        self.shared.item_available.notify_waiters();
        Ok(EnqueueOutcome::ENQUEUED)
    }

    pub async fn enqueue_transition(
        &self,
        mut message: Arc<ServerMessage>,
        target_generation: u64,
    ) -> Result<EnqueueOutcome, TryEnqueueError<Arc<ServerMessage>>> {
        loop {
            let notified = self.shared.capacity_available.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.try_enqueue_transition(message, target_generation) {
                Ok(outcome) => return Ok(outcome),
                Err(TryEnqueueError::Full(returned)) => message = returned,
                Err(other) => return Err(other),
            }
            notified.as_mut().await;
        }
    }

    #[cfg(test)]
    pub fn try_reserve_control(&self) -> Result<OutboundPermit, ReserveError> {
        let lane = if self.shared.v3() {
            Lane::Control
        } else {
            Lane::Legacy
        };
        self.try_reserve(lane, None)
    }

    pub fn try_reserve_control_scoped(
        &self,
        generation: u64,
        room_id: Option<RoomId>,
    ) -> Result<OutboundPermit, ReserveError> {
        let lane = if self.shared.v3() {
            Lane::Control
        } else {
            Lane::Legacy
        };
        self.try_reserve(lane, Some((generation, room_id)))
    }

    pub async fn reserve_control_scoped(
        &self,
        generation: u64,
        room_id: Option<RoomId>,
    ) -> Result<OutboundPermit, ReserveError> {
        let lane = if self.shared.v3() {
            Lane::Control
        } else {
            Lane::Legacy
        };
        self.reserve(lane, Some((generation, room_id))).await
    }

    fn try_reserve(
        &self,
        lane: Lane,
        expected_scope: Option<(u64, Option<RoomId>)>,
    ) -> Result<OutboundPermit, ReserveError> {
        let mut state = self.shared.state();
        if !state.accepting() {
            return Err(ReserveError::Closed);
        }
        if expected_scope.is_some_and(|(generation, room_id)| {
            let current = scope_matches(&state, generation, room_id);
            let possible_transition =
                room_id.is_none() && generation == state.enqueue_generation.saturating_add(1);
            !current && !possible_transition
        }) {
            return Err(ReserveError::Canceled);
        }
        if !self.shared.has_capacity(&state, lane) {
            return Err(ReserveError::Full);
        }
        state.reserve(lane);
        state.permit_count += 1;
        Ok(OutboundPermit {
            shared: Arc::clone(&self.shared),
            lane,
            active: true,
        })
    }

    async fn reserve(
        &self,
        lane: Lane,
        expected_scope: Option<(u64, Option<RoomId>)>,
    ) -> Result<OutboundPermit, ReserveError> {
        loop {
            let notified = self.shared.capacity_available.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.try_reserve(lane, expected_scope) {
                Ok(permit) => return Ok(permit),
                Err(ReserveError::Closed) => return Err(ReserveError::Closed),
                Err(ReserveError::Canceled) => return Err(ReserveError::Canceled),
                Err(ReserveError::Full) => notified.as_mut().await,
            }
        }
    }

    fn fail_accountability(
        &self,
        mut state: MutexGuard<'_, QueueState>,
        data: OutboundData,
    ) -> Result<EnqueueOutcome, TryEnqueueError<OutboundData>> {
        state.accountability_failed = true;
        increment_abandoned(&mut state.counters, data.class(), 1);
        self.shared.record_attempted(data.class());
        self.shared.record_abandoned(data.class(), 1);
        drop(state);
        self.shared.capacity_available.notify_waiters();
        self.shared.item_available.notify_waiters();
        Err(TryEnqueueError::AccountabilityUnavailable(data))
    }

    /// Queue a counter-only report with the current cumulative snapshot.
    /// Dequeue only raises that snapshot to the prior wire watermark, keeping
    /// reports monotonic without attributing later queue activity to this one.
    #[cfg(test)]
    pub fn try_enqueue_report_snapshot(&self) -> Result<EnqueueOutcome, ReserveError> {
        let mut state = self.shared.state();
        if !state.accepting() {
            return Err(ReserveError::Closed);
        }
        if !self.shared.has_capacity(&state, Lane::Control) {
            return Err(ReserveError::Full);
        }
        enqueue_delivery_report(&mut state, Vec::new());
        drop(state);
        self.shared.item_available.notify_one();
        Ok(EnqueueOutcome::ENQUEUED)
    }

    /// Atomically queue periodic delivery advisories while keeping a trailing
    /// report available for future exact gaps. An appendable same-generation
    /// report is refreshed in place; otherwise one control slot is reserved for
    /// a new report. Any remaining slot may carry `RelayStats`. Returns whether
    /// RelayStats was queued.
    pub fn try_enqueue_delivery_advisories(
        &self,
        relay_stats: Arc<ServerMessage>,
    ) -> Result<bool, TryEnqueueError<Arc<ServerMessage>>> {
        if !matches!(relay_stats.as_ref(), ServerMessage::RelayStats { .. }) {
            return Err(TryEnqueueError::InvalidMetadata(relay_stats));
        }
        let mut state = self.shared.state();
        if !state.accepting() {
            return Err(TryEnqueueError::Closed(relay_stats));
        }
        let counters = state.counters;
        let generation = state.enqueue_generation;
        let report_reused = state.control.back_mut().is_some_and(|queued| {
            if queued.generation != generation {
                return false;
            }
            let OutboundPayload::DeliveryReport(report) = &mut queued.payload else {
                return false;
            };
            if report.gaps.len() >= DELIVERY_REPORT_MAX_GAPS {
                return false;
            }
            report.per_class = counters;
            true
        });

        let occupied = state.lane_len_with_reservations(Lane::Control);
        let available = self.shared.control_capacity.saturating_sub(occupied);
        let report_slots = usize::from(!report_reused);
        if available < report_slots {
            return Err(TryEnqueueError::Full(relay_stats));
        }
        let stats_enqueued = available > report_slots;
        if stats_enqueued {
            push_message(&mut state, Lane::Control, relay_stats, None, None);
        }
        if !report_reused {
            enqueue_delivery_report(&mut state, Vec::new());
        }
        drop(state);
        if stats_enqueued || !report_reused {
            self.shared.item_available.notify_waiters();
        }
        Ok(stats_enqueued)
    }
}

/// Reservation error for conditional control delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReserveError {
    Full,
    Closed,
    Canceled,
}

/// One reserved queue slot. Dropping it releases capacity; sending consumes it.
#[derive(Debug)]
pub struct OutboundPermit {
    shared: Arc<SharedQueue>,
    lane: Lane,
    active: bool,
}

impl OutboundPermit {
    #[cfg(test)]
    pub fn send_control(
        mut self,
        message: Arc<ServerMessage>,
    ) -> Result<EnqueueOutcome, Arc<ServerMessage>> {
        self.send_control_inner(message, None)
    }

    pub fn send_control_scoped(
        mut self,
        message: Arc<ServerMessage>,
        generation: u64,
        room_id: Option<RoomId>,
    ) -> Result<EnqueueOutcome, Arc<ServerMessage>> {
        self.send_control_inner(message, Some((generation, room_id)))
    }

    fn send_control_inner(
        &mut self,
        message: Arc<ServerMessage>,
        expected_scope: Option<(u64, Option<RoomId>)>,
    ) -> Result<EnqueueOutcome, Arc<ServerMessage>> {
        if is_data_message(&message) {
            return Err(message);
        }
        let mut state = self.shared.state();
        state.release_reservation(self.lane);
        debug_assert!(state.permit_count > 0);
        state.permit_count = state.permit_count.saturating_sub(1);
        self.active = false;
        if !state.accepting() {
            let no_producers = !state.producers_open();
            drop(state);
            self.shared.capacity_available.notify_waiters();
            if no_producers {
                self.shared.item_available.notify_waiters();
            }
            return Err(message);
        }
        if let Some(target_room) = transition_target(message.as_ref()) {
            let Some((target_generation, _)) = expected_scope else {
                drop(state);
                self.shared.capacity_available.notify_waiters();
                return Err(message);
            };
            if target_generation != state.enqueue_generation.saturating_add(1) {
                drop(state);
                self.shared.capacity_available.notify_waiters();
                return Ok(EnqueueOutcome::CANCELED);
            }
            let barrier = QueuedOutbound {
                payload: OutboundPayload::Message(message),
                delivery_class: None,
                metadata: None,
                enqueued_at: Instant::now(),
                generation: target_generation,
                transition_barrier: true,
            };
            if self.lane == Lane::Legacy {
                state.legacy.push_back(barrier);
            } else {
                state.barriers.push_back(barrier);
            }
            state.enqueue_generation = target_generation;
            state.active_room = target_room;
        } else {
            if expected_scope
                .is_some_and(|(generation, room_id)| !scope_matches(&state, generation, room_id))
            {
                drop(state);
                self.shared.capacity_available.notify_waiters();
                return Ok(EnqueueOutcome::CANCELED);
            }
            push_message(&mut state, self.lane, message, None, None);
        }
        drop(state);
        self.shared.capacity_available.notify_waiters();
        self.shared.item_available.notify_one();
        Ok(EnqueueOutcome::ENQUEUED)
    }
}

impl Drop for OutboundPermit {
    fn drop(&mut self) {
        if self.active {
            {
                let mut state = self.shared.state();
                state.release_reservation(self.lane);
                debug_assert!(state.permit_count > 0);
                state.permit_count = state.permit_count.saturating_sub(1);
            }
            self.shared.item_available.notify_waiters();
            self.shared.capacity_available.notify_waiters();
        }
    }
}

impl OutboundReceiver {
    /// Stop all producers before teardown snapshots or drains the queue.
    /// Existing items remain readable by this receiver.
    pub fn close(&mut self) {
        {
            let mut state = self.shared.state();
            state.receiver_open = false;
        }
        self.shared.capacity_available.notify_waiters();
        self.shared.item_available.notify_waiters();
    }

    pub(crate) fn supports_v3(&self) -> bool {
        self.shared.v3()
    }

    pub(crate) fn game_data_format(&self) -> GameDataEncoding {
        encoding_from_tag(self.shared.game_data_format.load(Ordering::Acquire))
    }

    /// Decide whether this recipient should receive the supplemental
    /// unsupported-format advisory for `sender`. Exact `DeliveryReport` gaps
    /// are never throttled; only the redundant prose `Error` is rate-limited.
    pub(crate) fn unsupported_notice(&self, sender: PlayerId) -> Option<u64> {
        self.shared
            .unsupported_notices
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .observe(sender, Instant::now())
    }

    /// Receive the next item. Pre-v3 traffic stays FIFO; after v3 negotiation,
    /// any pre-negotiation FIFO prefix drains first and then control is strict
    /// priority over data.
    pub async fn recv(&mut self) -> Result<Option<QueuedOutbound>, TryReceiveError> {
        loop {
            let notified = self.shared.item_available.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.try_pop() {
                Ok(item) => return Ok(Some(item)),
                Err(PopState::Disconnected) => return Ok(None),
                Err(PopState::Terminal) => return Err(TryReceiveError::AccountabilityFailed),
                Err(PopState::Empty) => match self.pending_unsupported_flush_deadline() {
                    Some(deadline) => {
                        tokio::select! {
                            () = notified.as_mut() => {}
                            () = tokio::time::sleep_until(deadline) => {
                                if let Some(item) = self.take_pending_unsupported_item() {
                                    return Ok(Some(item));
                                }
                            }
                        }
                    }
                    None => notified.as_mut().await,
                },
            }
        }
    }

    /// Receive the next item while leaving a not-yet-ready data batch inside
    /// the accountable queue. This preserves v2 FIFO and lets `latest`
    /// supersede data until the writer actually starts sending it.
    pub async fn recv_batched(
        &mut self,
        batch_size: usize,
        batch_interval: std::time::Duration,
    ) -> Result<Option<QueuedOutbound>, TryReceiveError> {
        let batch_size = batch_size.max(1);
        loop {
            let shared = Arc::clone(&self.shared);
            let notified = shared.item_available.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.try_pop_batched(batch_size, batch_interval) {
                Ok(item) => return Ok(Some(item)),
                Err(BatchedPopState::Disconnected) => return Ok(None),
                Err(BatchedPopState::Terminal) => {
                    return Err(TryReceiveError::AccountabilityFailed)
                }
                Err(BatchedPopState::Empty) => match self.pending_unsupported_flush_deadline() {
                    Some(deadline) => {
                        tokio::select! {
                            () = notified.as_mut() => {}
                            () = tokio::time::sleep_until(deadline) => {
                                if let Some(item) = self.take_pending_unsupported_item() {
                                    return Ok(Some(item));
                                }
                            }
                        }
                    }
                    None => notified.as_mut().await,
                },
                Err(BatchedPopState::WaitUntil(deadline)) => {
                    // The pending exact report is control traffic and outranks a
                    // data batch still waiting to coalesce, so whichever
                    // deadline comes first wins.
                    let pending_deadline = self.pending_unsupported_flush_deadline();
                    tokio::select! {
                        () = notified.as_mut() => {}
                        () = tokio::time::sleep_until(deadline) => {}
                        () = async {
                            match pending_deadline {
                                Some(deadline) => tokio::time::sleep_until(deadline).await,
                                None => std::future::pending().await,
                            }
                        } => {
                            if let Some(item) = self.take_pending_unsupported_item() {
                                return Ok(Some(item));
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn try_recv(&mut self) -> Result<QueuedOutbound, TryReceiveError> {
        match self.try_pop() {
            Ok(item) => Ok(item),
            Err(PopState::Empty) => Err(TryReceiveError::Empty),
            Err(PopState::Disconnected) => Err(TryReceiveError::Disconnected),
            Err(PopState::Terminal) => Err(TryReceiveError::AccountabilityFailed),
        }
    }

    /// Pop only a priority control item, leaving the legacy prefix and data lane
    /// untouched. Used between data writes so a newly arrived control can bypass
    /// the remaining data batch.
    pub fn try_recv_control(&mut self) -> Result<Option<QueuedOutbound>, TryReceiveError> {
        let mut state = self.shared.state();
        if state.accountability_failed {
            return Err(TryReceiveError::AccountabilityFailed);
        }
        let item = if state.legacy.is_empty()
            && state
                .control
                .front()
                .is_some_and(|item| item.generation == state.receive_generation)
        {
            state.control.pop_front()
        } else {
            None
        }
        .map(|item| prepare_for_wire(&mut state, item));
        drop(state);
        if item.is_some() {
            self.shared.capacity_available.notify_waiters();
        }
        Ok(item)
    }

    fn try_pop(&self) -> Result<QueuedOutbound, PopState> {
        let mut state = self.shared.state();
        if state.accountability_failed {
            return Err(PopState::Terminal);
        }
        let item = if let Some(item) = state.legacy.pop_front() {
            if item.transition_barrier {
                state.receive_generation = item.generation;
            }
            Some(item)
        } else if state
            .control
            .front()
            .is_some_and(|item| item.generation == state.receive_generation)
        {
            state.control.pop_front()
        } else if state
            .data
            .front()
            .is_some_and(|item| item.generation == state.receive_generation)
        {
            state.data.pop_front()
        } else if state
            .barriers
            .front()
            .is_some_and(|item| item.generation == state.receive_generation.saturating_add(1))
        {
            let item = state.barriers.pop_front();
            if let Some(item) = &item {
                state.receive_generation = item.generation;
            }
            item
        } else {
            None
        };
        let result = match item {
            Some(item) => Ok(prepare_for_wire(&mut state, item)),
            None if state.receiver_open && state.producers_open() => Err(PopState::Empty),
            None => Err(PopState::Disconnected),
        };
        drop(state);
        if result.is_ok() {
            self.shared.capacity_available.notify_waiters();
        }
        result
    }

    fn try_pop_batched(
        &mut self,
        batch_size: usize,
        batch_interval: std::time::Duration,
    ) -> Result<QueuedOutbound, BatchedPopState> {
        let mut state = self.shared.state();
        if state.accountability_failed {
            return Err(BatchedPopState::Terminal);
        }

        let now = Instant::now();
        let mut decrement_batch = false;
        let mut crossed_barrier = false;
        let item = if let Some(front) = state.legacy.front() {
            // A `Latest` front waits up to `batch_interval` (unless the lane is
            // already `batch_size` deep) so a superseding same-key value can
            // still coalesce. Control, Reliable, and Volatile are
            // latency-sensitive: released immediately, and they never start a
            // batch, so a Latest queued behind them keeps its own coalesce wait
            // (issue #198). `class() == None` is control.
            let ready = front.class() != Some(DeliveryClass::Latest)
                || self.batch_remaining > 0
                || state.legacy.len() >= batch_size
                || now >= front.enqueued_at + batch_interval;
            if !ready {
                return Err(BatchedPopState::WaitUntil(
                    front.enqueued_at + batch_interval,
                ));
            }
            if front.class() == Some(DeliveryClass::Latest) && self.batch_remaining == 0 {
                self.batch_remaining = state.legacy.len().min(batch_size);
            }
            decrement_batch = self.batch_remaining > 0;
            let item = state.legacy.pop_front();
            if item.as_ref().is_some_and(|item| item.transition_barrier) {
                crossed_barrier = true;
            }
            item
        } else if state
            .control
            .front()
            .is_some_and(|item| item.generation == state.receive_generation)
        {
            state.control.pop_front()
        } else if state
            .data
            .front()
            .is_some_and(|item| item.generation == state.receive_generation)
        {
            let Some((front_class, front_enqueued_at)) = state
                .data
                .front()
                .map(|front| (front.class(), front.enqueued_at))
            else {
                return Err(BatchedPopState::Empty);
            };
            let phase_len = state
                .data
                .iter()
                .take_while(|item| item.generation == state.receive_generation)
                .count();
            // Same rule as the legacy lane: only a `Latest` front waits (and
            // starts a batch); control/Reliable/Volatile release immediately and
            // never arm one, so a trailing Latest keeps its coalesce wait (#198).
            let ready = front_class != Some(DeliveryClass::Latest)
                || self.batch_remaining > 0
                || phase_len >= batch_size
                || now >= front_enqueued_at + batch_interval;
            if !ready {
                return Err(BatchedPopState::WaitUntil(
                    front_enqueued_at + batch_interval,
                ));
            }
            if front_class == Some(DeliveryClass::Latest) && self.batch_remaining == 0 {
                self.batch_remaining = phase_len.min(batch_size);
            }
            decrement_batch = self.batch_remaining > 0;
            state.data.pop_front()
        } else if state
            .barriers
            .front()
            .is_some_and(|item| item.generation == state.receive_generation.saturating_add(1))
        {
            crossed_barrier = true;
            state.barriers.pop_front()
        } else {
            None
        };

        let result = match item {
            Some(item) => {
                if decrement_batch {
                    self.batch_remaining = self.batch_remaining.saturating_sub(1);
                }
                if crossed_barrier {
                    self.batch_remaining = 0;
                    state.receive_generation = item.generation;
                }
                Ok(prepare_for_wire(&mut state, item))
            }
            None if state.receiver_open && state.producers_open() => Err(BatchedPopState::Empty),
            None => Err(BatchedPopState::Disconnected),
        };
        drop(state);
        if result.is_ok() {
            self.shared.capacity_available.notify_waiters();
        }
        result
    }

    pub fn len(&self) -> usize {
        self.shared.state().len()
    }

    /// Oldest timestamp still resident in any queue lane. The control lane is
    /// not timestamp-FIFO because fresh control is inserted ahead of a trailing
    /// causal delivery report, so inspect every resident item rather than only
    /// each lane's front. The socket writer combines this with in-flight/batched
    /// timestamps so sustained fresh traffic cannot hide stale work from the
    /// global sojourn deadline.
    #[cfg(test)]
    pub fn oldest_enqueued_at(&self) -> Option<Instant> {
        let state = self.shared.state();
        state
            .legacy
            .iter()
            .chain(state.control.iter())
            .chain(state.data.iter())
            .chain(state.barriers.iter())
            .map(|queued| queued.enqueued_at)
            .min()
    }

    /// Oldest reliable item still resident in any queue lane.
    ///
    /// Reliable queue age drives the slow-consumer sojourn close. Control and
    /// lossy traffic deliberately do not lend their age to reliable traffic:
    /// each has an independent writer deadline policy.
    pub(crate) fn oldest_reliable_enqueued_at(&self) -> Option<Instant> {
        let state = self.shared.state();
        state
            .legacy
            .iter()
            .chain(state.control.iter())
            .chain(state.data.iter())
            .chain(state.barriers.iter())
            .filter(|queued| queued.class() == Some(DeliveryClass::Reliable))
            .map(|queued| queued.enqueued_at)
            .min()
    }

    pub fn record_written(&self, class: DeliveryClass) {
        let mut state = self.shared.state();
        match class {
            DeliveryClass::Reliable => {
                state.counters.reliable.delivered =
                    state.counters.reliable.delivered.saturating_add(1)
            }
            DeliveryClass::Latest => {
                state.counters.latest.delivered = state.counters.latest.delivered.saturating_add(1)
            }
            DeliveryClass::Volatile => {
                state.counters.volatile.delivered =
                    state.counters.volatile.delivered.saturating_add(1)
            }
        }
        drop(state);
        self.shared.record_delivered(class);
    }

    pub fn record_abandoned(&self, class: DeliveryClass, count: u64) {
        let mut state = self.shared.state();
        increment_abandoned(&mut state.counters, class, count);
        drop(state);
        self.shared.record_abandoned(class, count);
    }

    /// Account an encoding failure discovered only by the socket writer,
    /// coalescing contiguous omissions into one pending exact report.
    ///
    /// One frame per omitted message made accountability cost the recipient
    /// least able to afford it several times the bytes of the payload it
    /// replaced — measured at 5.4x for a MessagePack burst relayed to a JSON
    /// peer — which is what evicted that peer as a slow consumer under a
    /// bandwidth fault (issue #212). Contiguous ranges from one sender now
    /// merge under exactly the rules the queue applies to its own gap reports
    /// ([`try_append_gap`]), so the accounting stays exact while the wire cost
    /// collapses to one report per burst.
    ///
    /// Returns `Some(report)` when the accumulated accountability must be
    /// written before this omission can be recorded (the pending report is full
    /// or the new range cannot merge into it). `None` means the omission merged
    /// into the pending report, which
    /// [`take_pending_unsupported_report`](Self::take_pending_unsupported_report)
    /// emits before the next frame of any other kind, alongside the rate-limited
    /// advisory, or at close.
    pub fn record_unsupported_format(
        &self,
        metadata: DataDeliveryMetadata,
    ) -> Option<DeliveryReportPayload> {
        let mut state = self.shared.state();
        increment_unsupported(&mut state.counters, metadata.class);
        self.shared.record_unsupported(metadata.class);
        let gap = metadata.gap(DeliveryGapReason::UnsupportedFormat);
        // A recipient that never negotiated v3 receives no `DeliveryReport` at
        // all, so nothing may accumulate for it: the pending buffer must stay
        // empty or a later flush would emit a v3-only frame on a v2 wire.
        if !self.shared.v3() {
            return None;
        }
        if state.pending_unsupported.append(&gap, metadata.class) {
            return None;
        }
        // The pending report cannot absorb this omission. Flush it and start a
        // new one from this gap, so the returned report is complete and this
        // omission is still accounted for exactly once.
        let mut wire_counters = state.wire_counters;
        let flushed = state.pending_unsupported.take(&mut wire_counters);
        state.wire_counters = wire_counters;
        let appended = state.pending_unsupported.append(&gap, metadata.class);
        debug_assert!(appended, "an emptied pending report must accept any gap");
        flushed
    }

    /// Take the coalesced unsupported-format report so it reaches the wire
    /// before any other frame, with the advisory that explains it, or at close.
    ///
    /// This ordering is required rather than cosmetic: `wire_counters` only
    /// advances here, so emitting any other report first would advertise an
    /// `unsupported_format` counter delta whose exact ranges had not been sent.
    pub fn take_pending_unsupported_report(&self) -> Option<DeliveryReportPayload> {
        let mut state = self.shared.state();
        if state.pending_unsupported.is_empty() {
            return None;
        }
        let mut wire_counters = state.wire_counters;
        let report = state.pending_unsupported.take(&mut wire_counters);
        state.wire_counters = wire_counters;
        report
    }

    /// When a coalesced unsupported-format report must reach the recipient even
    /// though nothing else is queued for it.
    fn pending_unsupported_flush_deadline(&self) -> Option<Instant> {
        self.shared.state().pending_unsupported.flush_deadline()
    }

    /// The coalesced report as a writable queue item, for the receive paths that
    /// reach its flush deadline while the lanes are empty. It is synthesized
    /// rather than enqueued: the accounting is already owned here, so it must not
    /// depend on control-lane capacity it could be denied.
    fn take_pending_unsupported_item(&self) -> Option<QueuedOutbound> {
        let report = self.take_pending_unsupported_report()?;
        let generation = self.shared.state().receive_generation;
        Some(QueuedOutbound {
            payload: OutboundPayload::DeliveryReport(report),
            delivery_class: None,
            metadata: None,
            enqueued_at: Instant::now(),
            generation,
            transition_barrier: false,
        })
    }

    pub(crate) fn record_unsupported_class(&self, class: DeliveryClass) {
        let mut state = self.shared.state();
        increment_unsupported(&mut state.counters, class);
        drop(state);
        self.shared.record_unsupported(class);
    }

    pub fn count_by_class(&self) -> [(DeliveryClass, u64); 3] {
        let state = self.shared.state();
        let mut counts = [
            (DeliveryClass::Reliable, 0),
            (DeliveryClass::Latest, 0),
            (DeliveryClass::Volatile, 0),
        ];
        for class in state
            .legacy
            .iter()
            .chain(state.data.iter())
            .filter_map(|queued| queued.delivery_class)
        {
            match class {
                DeliveryClass::Reliable => counts[0].1 += 1,
                DeliveryClass::Latest => counts[1].1 += 1,
                DeliveryClass::Volatile => counts[2].1 += 1,
            }
        }
        counts
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryReceiveError {
    Empty,
    Disconnected,
    AccountabilityFailed,
}

enum PopState {
    Empty,
    Disconnected,
    Terminal,
}

enum BatchedPopState {
    Empty,
    Disconnected,
    Terminal,
    WaitUntil(Instant),
}

fn valid_class_key(class: Option<DeliveryClass>, key: Option<u32>) -> bool {
    matches!(
        (class, key),
        (
            None | Some(DeliveryClass::Reliable | DeliveryClass::Volatile),
            None
        ) | (Some(DeliveryClass::Latest), Some(_))
    )
}

const fn encoding_tag(format: GameDataEncoding) -> u8 {
    match format {
        GameDataEncoding::Json => 0,
        GameDataEncoding::MessagePack => 1,
        GameDataEncoding::Rkyv => 2,
    }
}

const fn encoding_from_tag(tag: u8) -> GameDataEncoding {
    match tag {
        1 => GameDataEncoding::MessagePack,
        2 => GameDataEncoding::Rkyv,
        _ => GameDataEncoding::Json,
    }
}

fn scope_matches(state: &QueueState, generation: u64, room_id: Option<RoomId>) -> bool {
    generation == state.enqueue_generation
        && room_id.is_none_or(|room_id| state.active_room == Some(room_id))
}

fn transition_target(message: &ServerMessage) -> Option<Option<RoomId>> {
    match message {
        ServerMessage::RoomJoined(payload) => Some(Some(payload.room_id)),
        ServerMessage::RoomLeft => Some(None),
        ServerMessage::Reconnected(payload) => Some(Some(payload.room_id)),
        ServerMessage::SpectatorJoined(payload) => Some(Some(payload.room_id)),
        ServerMessage::SpectatorLeft { .. } => Some(None),
        _ => None,
    }
}

fn is_data_message(message: &ServerMessage) -> bool {
    matches!(
        message,
        ServerMessage::GameData { .. } | ServerMessage::GameDataBinary { .. }
    )
}

fn push_message(
    state: &mut QueueState,
    lane: Lane,
    message: Arc<ServerMessage>,
    delivery_class: Option<DeliveryClass>,
    metadata: Option<DataDeliveryMetadata>,
) {
    let queued = QueuedOutbound {
        payload: OutboundPayload::Message(message),
        delivery_class,
        metadata,
        enqueued_at: Instant::now(),
        generation: state.enqueue_generation,
        transition_barrier: false,
    };
    match lane {
        Lane::Legacy => state.legacy.push_back(queued),
        Lane::Control => {
            let trailing_current_report = state.control.back().is_some_and(|trailing| {
                trailing.generation == state.enqueue_generation
                    && matches!(&trailing.payload, OutboundPayload::DeliveryReport(_))
            });
            if trailing_current_report {
                let position = state.control.len() - 1;
                state.control.insert(position, queued);
            } else {
                state.control.push_back(queued);
            }
        }
        Lane::Data => state.data.push_back(queued),
    }
}

fn push_data(
    state: &mut QueueState,
    lane: Lane,
    data: OutboundData,
    effective_class: DeliveryClass,
) {
    push_message(
        state,
        lane,
        data.message,
        Some(effective_class),
        data.metadata,
    );
}

fn oldest_volatile_position(data: &VecDeque<QueuedOutbound>, generation: u64) -> Option<usize> {
    data.iter().position(|queued| {
        queued.generation == generation
            && queued.metadata.map(|metadata| metadata.class) == Some(DeliveryClass::Volatile)
    })
}

fn enqueue_gap_report(state: &mut QueueState, gap: DeliveryGap) {
    let generation = state.enqueue_generation;
    let counters = state.counters;
    if let Some(QueuedOutbound {
        payload: OutboundPayload::DeliveryReport(report),
        generation: report_generation,
        ..
    }) = state.control.back_mut()
    {
        if *report_generation == generation && try_append_gap(report, &gap) {
            report.per_class = counters;
            return;
        }
    }
    enqueue_delivery_report(state, vec![gap]);
}

fn can_record_gap(state: &QueueState, control_capacity: usize, gap: &DeliveryGap) -> bool {
    let can_append = state.control.back().is_some_and(|queued| {
        queued.generation == state.enqueue_generation
            && matches!(
                &queued.payload,
                OutboundPayload::DeliveryReport(report) if report_can_append(report, gap)
            )
    });
    can_append
        || state.control.len() + state.barriers.len() + state.reserved_control < control_capacity
}

fn report_can_append(report: &DeliveryReportPayload, gap: &DeliveryGap) -> bool {
    report
        .gaps
        .last()
        .is_some_and(|previous| gaps_merge(previous, gap))
        || report.gaps.len() < DELIVERY_REPORT_MAX_GAPS
}

fn try_append_gap(report: &mut DeliveryReportPayload, gap: &DeliveryGap) -> bool {
    append_gap(&mut report.gaps, gap)
}

/// The single merge rule for exact gap ranges: extend the trailing range when
/// it is adjacent or overlapping ([`gaps_merge`]), otherwise start a new range
/// while the bounded report still has room. Shared by the queue's own gap
/// reports and by the writer's pending unsupported-format report so both
/// coalesce identically.
fn append_gap(gaps: &mut Vec<DeliveryGap>, gap: &DeliveryGap) -> bool {
    if let Some(previous) = gaps.last_mut() {
        if gaps_merge(previous, gap) {
            previous.from_seq = previous.from_seq.min(gap.from_seq);
            previous.to_seq = previous.to_seq.max(gap.to_seq);
            return true;
        }
    }
    if gaps.len() >= DELIVERY_REPORT_MAX_GAPS {
        return false;
    }
    gaps.push(gap.clone());
    true
}

fn gaps_merge(left: &DeliveryGap, right: &DeliveryGap) -> bool {
    left.from_player == right.from_player
        && left.epoch == right.epoch
        && left.reason == right.reason
        && right.from_seq <= left.to_seq.saturating_add(1)
        && left.from_seq <= right.to_seq.saturating_add(1)
}

fn enqueue_delivery_report(state: &mut QueueState, gaps: Vec<DeliveryGap>) {
    state.control.push_back(QueuedOutbound {
        payload: OutboundPayload::DeliveryReport(DeliveryReportPayload {
            per_class: state.counters,
            gaps,
        }),
        delivery_class: None,
        metadata: None,
        enqueued_at: Instant::now(),
        generation: state.enqueue_generation,
        transition_barrier: false,
    });
}

fn prepare_for_wire(state: &mut QueueState, mut item: QueuedOutbound) -> QueuedOutbound {
    if let OutboundPayload::DeliveryReport(report) = &mut item.payload {
        // The writer's coalesced omissions ride along in this report whenever
        // they fit, so one frame carries both. That keeps a single owner of the
        // `unsupported_format` wire frontier: whatever is not absorbed here is
        // still pending, and the counters below therefore never advertise a
        // delta whose exact ranges have not been sent.
        let absorbed =
            if report.gaps.len() + state.pending_unsupported.len() <= DELIVERY_REPORT_MAX_GAPS {
                state.pending_unsupported.take(&mut state.wire_counters)
            } else {
                None
            };
        report.per_class = max_counters(report.per_class, state.wire_counters);
        // `state.counters` may already count omissions that are still pending
        // (they are counted when discovered, reported when coalesced), so this
        // counter is clamped to the frontier rather than raised by a report that
        // carries no ranges for it.
        report.per_class.reliable.unsupported_format =
            state.wire_counters.reliable.unsupported_format;
        report.per_class.latest.unsupported_format = state.wire_counters.latest.unsupported_format;
        report.per_class.volatile.unsupported_format =
            state.wire_counters.volatile.unsupported_format;
        if let Some(absorbed) = absorbed {
            report.gaps.extend(absorbed.gaps);
        }
        state.wire_counters = report.per_class;
    }
    item
}

fn max_counters(
    left: DeliveryCountersByClass,
    right: DeliveryCountersByClass,
) -> DeliveryCountersByClass {
    DeliveryCountersByClass {
        reliable: crate::protocol::ReliableDeliveryCounters {
            delivered: left.reliable.delivered.max(right.reliable.delivered),
            abandoned: left.reliable.abandoned.max(right.reliable.abandoned),
            unsupported_format: left
                .reliable
                .unsupported_format
                .max(right.reliable.unsupported_format),
        },
        latest: crate::protocol::LatestDeliveryCounters {
            delivered: left.latest.delivered.max(right.latest.delivered),
            superseded: left.latest.superseded.max(right.latest.superseded),
            dropped_full: left.latest.dropped_full.max(right.latest.dropped_full),
            abandoned: left.latest.abandoned.max(right.latest.abandoned),
            unsupported_format: left
                .latest
                .unsupported_format
                .max(right.latest.unsupported_format),
        },
        volatile: crate::protocol::VolatileDeliveryCounters {
            delivered: left.volatile.delivered.max(right.volatile.delivered),
            dropped: left.volatile.dropped.max(right.volatile.dropped),
            abandoned: left.volatile.abandoned.max(right.volatile.abandoned),
            unsupported_format: left
                .volatile
                .unsupported_format
                .max(right.volatile.unsupported_format),
        },
    }
}

fn increment_abandoned(counters: &mut DeliveryCountersByClass, class: DeliveryClass, count: u64) {
    match class {
        DeliveryClass::Reliable => {
            counters.reliable.abandoned = counters.reliable.abandoned.saturating_add(count)
        }
        DeliveryClass::Latest => {
            counters.latest.abandoned = counters.latest.abandoned.saturating_add(count)
        }
        DeliveryClass::Volatile => {
            counters.volatile.abandoned = counters.volatile.abandoned.saturating_add(count)
        }
    }
}

fn increment_unsupported(counters: &mut DeliveryCountersByClass, class: DeliveryClass) {
    match class {
        DeliveryClass::Reliable => {
            counters.reliable.unsupported_format =
                counters.reliable.unsupported_format.saturating_add(1)
        }
        DeliveryClass::Latest => {
            counters.latest.unsupported_format =
                counters.latest.unsupported_format.saturating_add(1)
        }
        DeliveryClass::Volatile => {
            counters.volatile.unsupported_format =
                counters.volatile.unsupported_format.saturating_add(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn message(id: u64) -> Arc<ServerMessage> {
        Arc::new(ServerMessage::Error {
            message: id.to_string(),
            error_code: None,
        })
    }

    fn relay_stats() -> Arc<ServerMessage> {
        Arc::new(ServerMessage::RelayStats {
            interval_ms: 1_000,
            sent_to_you: 0,
            dropped_for_you: 0,
            backpressure_events: 0,
        })
    }

    fn data(id: u64, class: DeliveryClass, key: Option<u32>, seq: u64) -> OutboundData {
        data_with_epoch(id, class, key, 1, seq)
    }

    fn data_with_epoch(
        id: u64,
        class: DeliveryClass,
        key: Option<u32>,
        epoch: u32,
        seq: u64,
    ) -> OutboundData {
        OutboundData::new(
            Arc::new(ServerMessage::GameData {
                from_player: PlayerId::from_u128(1),
                data: json!({ "id": id }),
                seq: Some(seq),
                epoch: Some(epoch),
                class: Some(class),
                key,
            }),
            DataDeliveryMetadata {
                class,
                key,
                from_player: PlayerId::from_u128(1),
                room_id: RoomId::from_u128(2),
                epoch,
                seq,
            },
        )
    }

    fn message_id(queued: &QueuedOutbound) -> u64 {
        match &queued.payload {
            OutboundPayload::Message(message) => match message.as_ref() {
                ServerMessage::Error { message, .. } => message.parse().unwrap(),
                ServerMessage::GameData { data, .. } => data["id"].as_u64().unwrap(),
                other => panic!("unexpected message: {other:?}"),
            },
            OutboundPayload::DeliveryReport(_) => panic!("expected message"),
        }
    }

    fn report(queued: QueuedOutbound) -> DeliveryReportPayload {
        match queued.payload {
            OutboundPayload::DeliveryReport(report) => report,
            other => panic!("expected report, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pre_v3_uses_one_fifo_and_backpressures_every_class() {
        let (tx, mut rx) = channel(2, 1);
        tx.try_enqueue_control(message(1)).unwrap();
        tx.try_enqueue_data(data(2, DeliveryClass::Latest, Some(7), 1))
            .unwrap();
        assert!(matches!(
            tx.try_enqueue_control(message(3)),
            Err(TryEnqueueError::Full(_))
        ));
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 1);
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 2);
    }

    #[test]
    fn receiver_retains_negotiated_projection_after_registration_sender_drops() {
        let (tx, rx) = channel(1, 1);
        tx.set_protocol_version(3);
        tx.set_game_data_format(GameDataEncoding::MessagePack);
        drop(tx);

        assert!(rx.supports_v3());
        assert_eq!(rx.game_data_format(), GameDataEncoding::MessagePack);
    }

    #[tokio::test]
    async fn v3_control_is_strictly_prioritized_over_data() {
        let (tx, mut rx) = channel(4, 2);
        tx.set_protocol_version(3);
        tx.try_enqueue_data(data(1, DeliveryClass::Reliable, None, 1))
            .unwrap();
        tx.try_enqueue_data(data(2, DeliveryClass::Reliable, None, 2))
            .unwrap();
        tx.try_enqueue_control(message(9)).unwrap();
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 9);
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 1);
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 2);
    }

    #[tokio::test]
    async fn transition_fence_orders_old_data_before_new_generation_control() {
        let (tx, mut rx) = channel(4, 4);
        tx.set_protocol_version(3);
        tx.try_enqueue_data(data(1, DeliveryClass::Reliable, None, 1))
            .unwrap();
        tx.try_enqueue_transition(Arc::new(ServerMessage::RoomLeft), 1)
            .unwrap();
        tx.try_enqueue_control_scoped(message(2), None, 1).unwrap();

        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 1);
        assert!(matches!(
            rx.recv().await.unwrap().unwrap().payload,
            OutboundPayload::Message(message) if matches!(message.as_ref(), ServerMessage::RoomLeft)
        ));
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 2);
    }

    #[test]
    fn stale_generation_is_canceled_even_after_same_scope_transition() {
        let (tx, _rx) = channel(4, 4);
        tx.set_protocol_version(3);
        tx.try_enqueue_transition(Arc::new(ServerMessage::RoomLeft), 1)
            .unwrap();
        assert_eq!(
            tx.try_enqueue_control_scoped(message(9), None, 0).unwrap(),
            EnqueueOutcome::CANCELED
        );
        assert_eq!(
            tx.try_enqueue_transition(Arc::new(ServerMessage::RoomLeft), 1)
                .unwrap(),
            EnqueueOutcome::CANCELED
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fresh_control_cannot_hide_old_data_sojourn_age() {
        let (tx, mut rx) = channel(2, 2);
        tx.set_protocol_version(3);
        tx.try_enqueue_data(data(1, DeliveryClass::Reliable, None, 1))
            .unwrap();
        let data_enqueued_at = rx.oldest_enqueued_at().unwrap();

        for id in 2..=4 {
            tokio::time::advance(std::time::Duration::from_secs(1)).await;
            tx.try_enqueue_control(message(id)).unwrap();
            assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), id);
            assert_eq!(rx.oldest_enqueued_at(), Some(data_enqueued_at));
        }
        assert_eq!(
            data_enqueued_at.elapsed(),
            std::time::Duration::from_secs(3)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fresh_control_cannot_hide_overtaken_report_sojourn_age() {
        let (tx, mut rx) = channel(1, 2);
        tx.set_protocol_version(3);
        tx.try_enqueue_data(data(1, DeliveryClass::Reliable, None, 1))
            .unwrap();
        let dropped = tx
            .try_enqueue_data(data(2, DeliveryClass::Volatile, None, 2))
            .unwrap();
        assert_eq!(
            dropped,
            EnqueueOutcome {
                enqueued: false,
                losses: 1
            }
        );
        let report_enqueued_at = rx.oldest_enqueued_at().unwrap();

        // A report remains at the control-lane tail so later control can be
        // delivered first. Its original timestamp must still drive the global
        // sojourn deadline no matter how often that slot is refilled.
        for id in 3..=5 {
            tokio::time::advance(std::time::Duration::from_secs(1)).await;
            tx.try_enqueue_control(message(id)).unwrap();
            assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), id);
            assert_eq!(rx.oldest_enqueued_at(), Some(report_enqueued_at));
        }

        let report = rx.recv().await.unwrap().unwrap();
        assert!(matches!(report.payload, OutboundPayload::DeliveryReport(_)));
        assert_eq!(report.enqueued_at, report_enqueued_at);
        assert_eq!(
            report_enqueued_at.elapsed(),
            std::time::Duration::from_secs(3)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn batching_wait_keeps_latest_data_coalescible() {
        let (tx, mut rx) = channel(4, 4);
        tx.set_protocol_version(3);
        tx.try_enqueue_data(data(1, DeliveryClass::Latest, Some(10), 1))
            .unwrap();

        let waiter = tokio::spawn(async move {
            let report = rx
                .recv_batched(4, std::time::Duration::from_secs(10))
                .await
                .unwrap()
                .unwrap();
            let successor = rx
                .recv_batched(4, std::time::Duration::from_secs(10))
                .await
                .unwrap()
                .unwrap();
            (report, successor)
        });
        tokio::task::yield_now().await;

        tx.try_enqueue_data(data(2, DeliveryClass::Latest, Some(10), 2))
            .unwrap();
        tokio::time::advance(std::time::Duration::from_secs(10)).await;

        let (gap, successor) = waiter.await.unwrap();
        assert_eq!(report(gap).gaps[0].from_seq, 1);
        assert_eq!(message_id(&successor), 2);
    }

    /// Regression: issue #198 — the outbound batch timer must never delay
    /// latency-sensitive traffic. Only `Latest` waits for a fuller batch (to
    /// coalesce); `Reliable` and `Volatile` are released immediately, on both
    /// the pre-v3 legacy FIFO lane and the v3 data lane. Under the paused clock
    /// an idle-hold shows up as elapsed virtual time; the release must be
    /// instantaneous (`Duration::ZERO`).
    #[tokio::test(start_paused = true)]
    async fn regression_198_latency_sensitive_data_bypasses_batch_timer() {
        for v3 in [false, true] {
            for class in [DeliveryClass::Reliable, DeliveryClass::Volatile] {
                let (tx, mut rx) = channel(4, 4);
                if v3 {
                    tx.set_protocol_version(3);
                }
                tx.try_enqueue_data(data(1, class, None, 1)).unwrap();

                let started = Instant::now();
                let item = rx
                    .recv_batched(10, std::time::Duration::from_millis(16))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(message_id(&item), 1);
                assert_eq!(
                    started.elapsed(),
                    std::time::Duration::ZERO,
                    "class {class:?} (v3={v3}) must not be held by the 16ms batch timer (#198)"
                );
            }
        }
    }

    /// Regression: issue #198 refinement — a `Reliable` release must not start a
    /// batch, so a `Latest` queued behind it keeps its own coalesce wait instead
    /// of riding out immediately. Proves the batch is armed only by a `Latest`
    /// front.
    #[tokio::test(start_paused = true)]
    async fn regression_198_latest_behind_reliable_still_coalesces() {
        let (tx, mut rx) = channel(4, 4);
        tx.set_protocol_version(3);
        tx.try_enqueue_data(data(1, DeliveryClass::Reliable, None, 1))
            .unwrap();
        tx.try_enqueue_data(data(2, DeliveryClass::Latest, Some(10), 2))
            .unwrap();

        // The Reliable front is released immediately without arming a batch.
        let started = Instant::now();
        let first = rx
            .recv_batched(4, std::time::Duration::from_secs(10))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message_id(&first), 1);
        assert_eq!(
            started.elapsed(),
            std::time::Duration::ZERO,
            "Reliable must not be held by the batch timer (#198)"
        );

        // The trailing Latest now waits its coalesce window; a newer same-key
        // value supersedes it during the wait.
        let waiter = tokio::spawn(async move {
            let report = rx
                .recv_batched(4, std::time::Duration::from_secs(10))
                .await
                .unwrap()
                .unwrap();
            let successor = rx
                .recv_batched(4, std::time::Duration::from_secs(10))
                .await
                .unwrap()
                .unwrap();
            (report, successor)
        });
        tokio::task::yield_now().await;
        assert!(
            !waiter.is_finished(),
            "a Latest behind a Reliable must still wait to coalesce"
        );

        tx.try_enqueue_data(data(3, DeliveryClass::Latest, Some(10), 3))
            .unwrap();
        tokio::time::advance(std::time::Duration::from_secs(10)).await;

        let (gap, successor) = waiter.await.unwrap();
        assert_eq!(report(gap).gaps[0].from_seq, 2);
        assert_eq!(message_id(&successor), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn batching_wait_preserves_pre_v3_fifo_across_message_kinds() {
        // Pre-v3 FIFO is preserved and, per issue #198, latency-sensitive
        // Reliable data is not held by the batch timer: the data frame and the
        // trailing control frame are both delivered immediately, in order.
        let (tx, mut rx) = channel(4, 1);
        tx.try_enqueue_data(data(1, DeliveryClass::Reliable, None, 1))
            .unwrap();
        tx.try_enqueue_control(message(2)).unwrap();

        let started = Instant::now();
        let first = rx
            .recv_batched(4, std::time::Duration::from_secs(10))
            .await
            .unwrap()
            .unwrap();
        let second = rx
            .recv_batched(4, std::time::Duration::from_secs(10))
            .await
            .unwrap()
            .unwrap();
        assert_eq!((message_id(&first), message_id(&second)), (1, 2));
        assert_eq!(
            started.elapsed(),
            std::time::Duration::ZERO,
            "Reliable data and control must not be held by the batch timer (#198)"
        );
    }

    #[tokio::test]
    async fn latest_supersession_appends_successor_and_reports_exact_predecessor() {
        let (tx, mut rx) = channel(4, 4);
        tx.set_protocol_version(3);
        tx.try_enqueue_data(data(1, DeliveryClass::Latest, Some(10), 1))
            .unwrap();
        tx.try_enqueue_data(data(2, DeliveryClass::Latest, Some(20), 2))
            .unwrap();
        assert_eq!(
            tx.try_enqueue_data(data(3, DeliveryClass::Latest, Some(10), 3))
                .unwrap(),
            EnqueueOutcome {
                enqueued: true,
                losses: 1
            }
        );

        let report = report(rx.recv().await.unwrap().unwrap());
        assert_eq!(report.gaps.len(), 1);
        assert_eq!(report.gaps[0].from_seq, 1);
        assert_eq!(report.gaps[0].to_seq, 1);
        assert_eq!(report.gaps[0].reason, DeliveryGapReason::LatestSuperseded);
        assert_eq!(report.per_class.latest.superseded, 1);
        // Remove-and-append preserves global sequence order across keys.
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 2);
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 3);
    }

    #[tokio::test]
    async fn sustained_latest_overload_coalesces_reports_without_fail_close() {
        let (tx, mut rx) = channel(1, 1);
        tx.set_protocol_version(3);
        tx.try_enqueue_data(data(1, DeliveryClass::Latest, Some(10), 1))
            .unwrap();
        for seq in 2..=1_000 {
            let outcome = tx
                .try_enqueue_data(data(seq, DeliveryClass::Latest, Some(10), seq))
                .unwrap();
            assert!(outcome.enqueued);
            assert_eq!(outcome.losses, 1);
        }

        let report = report(rx.recv().await.unwrap().unwrap());
        assert_eq!(report.gaps.len(), 1);
        assert_eq!(report.gaps[0].from_seq, 1);
        assert_eq!(report.gaps[0].to_seq, 999);
        assert_eq!(report.per_class.latest.superseded, 999);
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 1_000);
        assert!(!tx.is_closed());
    }

    #[test]
    fn nonmergeable_gap_reports_are_bounded_by_control_capacity() {
        let worst_case = DeliveryGap {
            from_player: PlayerId::from_u128(u128::MAX),
            epoch: u32::MAX,
            from_seq: u64::MAX,
            to_seq: u64::MAX,
            reason: DeliveryGapReason::LatestDroppedFull,
        };
        let wire_size = serde_json::to_vec(&ServerMessage::DeliveryReport(Box::new(
            DeliveryReportPayload {
                per_class: DeliveryCountersByClass::default(),
                gaps: vec![worst_case; DELIVERY_REPORT_MAX_GAPS],
            },
        )))
        .expect("worst-case report serializes")
        .len();
        assert!(
            wire_size < 64 * 1024,
            "bounded report must fit the default WebSocket frame cap: {wire_size} bytes"
        );

        fn fill_nonmergeable_gaps(
            control_capacity: usize,
            count: usize,
        ) -> (OutboundSender, OutboundReceiver, bool) {
            let (tx, rx) = channel(1, control_capacity);
            tx.set_protocol_version(3);
            tx.try_enqueue_data(data(1, DeliveryClass::Reliable, None, 1))
                .unwrap();
            let mut accountability_failed = false;
            for index in 0..count {
                let seq = index as u64 + 2;
                let (class, key) = if index % 2 == 0 {
                    (DeliveryClass::Latest, Some(seq as u32))
                } else {
                    (DeliveryClass::Volatile, None)
                };
                match tx.try_enqueue_data(data(seq, class, key, seq)) {
                    Ok(_) => {}
                    Err(TryEnqueueError::AccountabilityUnavailable(_)) => {
                        accountability_failed = true;
                        break;
                    }
                    Err(other) => panic!("unexpected enqueue failure: {other:?}"),
                }
            }
            (tx, rx, accountability_failed)
        }

        let (tx, rx, failed) = fill_nonmergeable_gaps(2, DELIVERY_REPORT_MAX_GAPS + 1);
        assert!(!failed, "a second control slot should hold overflow");
        let gap_counts: Vec<_> = rx
            .shared
            .state()
            .control
            .iter()
            .map(|queued| match &queued.payload {
                OutboundPayload::DeliveryReport(report) => report.gaps.len(),
                other => panic!("expected delivery report, got {other:?}"),
            })
            .collect();
        assert_eq!(gap_counts, vec![DELIVERY_REPORT_MAX_GAPS, 1]);
        let state = rx.shared.state();
        let reports: Vec<_> = state
            .control
            .iter()
            .map(|queued| match &queued.payload {
                OutboundPayload::DeliveryReport(report) => report,
                other => panic!("expected delivery report, got {other:?}"),
            })
            .collect();
        assert_eq!(reports[0].per_class.latest.dropped_full, 128);
        assert_eq!(reports[0].per_class.volatile.dropped, 128);
        assert_eq!(reports[1].per_class.latest.dropped_full, 129);
        assert_eq!(reports[1].per_class.volatile.dropped, 128);
        drop(state);
        assert!(!tx.is_closed());

        let (tx, rx, failed) = fill_nonmergeable_gaps(1, DELIVERY_REPORT_MAX_GAPS + 1);
        assert!(failed);
        let state = rx.shared.state();
        let OutboundPayload::DeliveryReport(report) = &state.control[0].payload else {
            panic!("expected bounded delivery report")
        };
        assert_eq!(report.gaps.len(), DELIVERY_REPORT_MAX_GAPS);
        assert_eq!(state.data.len(), 1, "failed omission must not mutate data");
        drop(state);
        assert!(tx.is_closed());
    }

    /// A queued report popped while the writer still holds coalesced omissions
    /// carries both, so exactly one frame owns the `unsupported_format` wire
    /// frontier and no frame advertises a counter delta without its ranges.
    #[tokio::test]
    async fn queued_report_absorbs_the_writers_pending_unsupported_ranges() {
        let (tx, mut rx) = channel(1, 2);
        tx.set_protocol_version(3);
        tx.try_enqueue_data(data(1, DeliveryClass::Reliable, None, 1))
            .unwrap();
        let active = rx.recv().await.unwrap().unwrap();
        let active_metadata = active.metadata.expect("stamped active data");

        tx.try_enqueue_data(data(2, DeliveryClass::Reliable, None, 2))
            .unwrap();
        tx.try_enqueue_data(data(3, DeliveryClass::Latest, Some(3), 3))
            .unwrap();

        assert!(
            rx.record_unsupported_format(active_metadata).is_none(),
            "the first omission coalesces instead of costing its own frame"
        );

        let queued = report(rx.recv().await.unwrap().unwrap());
        assert_eq!(queued.per_class.reliable.unsupported_format, 1);
        assert_eq!(queued.per_class.latest.dropped_full, 1);
        let reasons: Vec<_> = queued.gaps.iter().map(|gap| gap.reason).collect();
        assert_eq!(
            reasons,
            vec![
                DeliveryGapReason::LatestDroppedFull,
                DeliveryGapReason::UnsupportedFormat
            ],
            "the absorbed range must ride along with the queued report"
        );
        assert!(
            rx.take_pending_unsupported_report().is_none(),
            "an absorbed range must not be reported a second time"
        );
    }

    /// When the pending ranges cannot fit in the report being handed out, the
    /// report must not advertise an `unsupported_format` delta it carries no
    /// ranges for — otherwise the following flush would report the same
    /// omissions again and the counters would regress on the wire.
    #[tokio::test]
    async fn a_full_report_leaves_the_pending_ranges_and_the_frontier_alone() {
        let (tx, mut rx) = channel(1, 2);
        tx.set_protocol_version(3);
        // Fill the pending report to its bound with non-mergeable ranges.
        for index in 0..DELIVERY_REPORT_MAX_GAPS {
            assert!(rx
                .record_unsupported_format(unsupported_metadata(
                    DeliveryClass::Reliable,
                    4,
                    (index as u64) * 2
                ))
                .is_none());
        }

        // A queued gap report of its own: one `latest` key superseded.
        tx.try_enqueue_data(data(2, DeliveryClass::Latest, Some(3), 2))
            .unwrap();
        tx.try_enqueue_data(data(3, DeliveryClass::Latest, Some(3), 3))
            .unwrap();

        let queued = report(rx.recv().await.unwrap().unwrap());
        assert_eq!(queued.gaps.len(), 1, "no pending range could fit");
        assert_eq!(queued.gaps[0].reason, DeliveryGapReason::LatestSuperseded);
        assert_eq!(
            queued.per_class.reliable.unsupported_format, 0,
            "the frontier may not advance without the ranges that explain it"
        );

        let flushed = rx
            .take_pending_unsupported_report()
            .expect("the pending ranges survive a report that could not absorb them");
        assert_eq!(flushed.gaps.len(), DELIVERY_REPORT_MAX_GAPS);
        assert_eq!(
            flushed.per_class.reliable.unsupported_format,
            DELIVERY_REPORT_MAX_GAPS as u64
        );
    }

    #[tokio::test]
    async fn per_class_metrics_conserve_every_terminal_outcome() {
        let metrics = Arc::new(crate::metrics::ServerMetrics::new());
        let (tx, mut rx) = channel_with_metrics(2, 4, Arc::clone(&metrics));
        tx.set_protocol_version(3);
        tx.try_enqueue_data(data(1, DeliveryClass::Reliable, None, 1))
            .unwrap();
        tx.try_enqueue_data(data(2, DeliveryClass::Latest, Some(7), 2))
            .unwrap();
        tx.try_enqueue_data(data(3, DeliveryClass::Latest, Some(7), 3))
            .unwrap();
        tx.try_enqueue_data(data(4, DeliveryClass::Volatile, None, 4))
            .unwrap();

        let _report = report(rx.recv().await.unwrap().unwrap());
        for _ in 0..2 {
            let queued = rx.recv().await.unwrap().unwrap();
            rx.record_written(queued.class().unwrap());
        }

        let by_class = metrics.delivery_metrics_by_class();
        assert_eq!(by_class.reliable.attempted, 1);
        assert_eq!(by_class.reliable.delivered, 1);
        assert_eq!(by_class.latest.attempted, 2);
        assert_eq!(by_class.latest.delivered, 1);
        assert_eq!(by_class.latest.superseded, 1);
        assert_eq!(by_class.volatile.attempted, 1);
        assert_eq!(by_class.volatile.dropped, 1);
        for counters in [by_class.reliable, by_class.latest, by_class.volatile] {
            assert_eq!(
                counters.attempted,
                counters.delivered
                    + counters.superseded
                    + counters.dropped_full
                    + counters.dropped
                    + counters.abandoned
                    + counters.unsupported_format
            );
        }
    }

    #[tokio::test]
    async fn latest_key_spans_sender_epochs_and_reports_the_replaced_epoch() {
        let (tx, mut rx) = channel(2, 2);
        tx.set_protocol_version(3);
        tx.try_enqueue_data(data_with_epoch(1, DeliveryClass::Latest, Some(10), 4, 8))
            .unwrap();
        tx.try_enqueue_data(data_with_epoch(2, DeliveryClass::Latest, Some(10), 5, 1))
            .unwrap();

        let report = report(rx.recv().await.unwrap().unwrap());
        assert_eq!(report.gaps[0].epoch, 4);
        assert_eq!(report.gaps[0].from_seq, 8);
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 2);
    }

    #[tokio::test]
    async fn queued_reports_materialize_current_counters_without_regression() {
        let (tx, mut rx) = channel(4, 4);
        tx.set_protocol_version(3);
        tx.try_enqueue_data(data(1, DeliveryClass::Latest, Some(1), 1))
            .unwrap();
        tx.try_enqueue_data(data(2, DeliveryClass::Latest, Some(1), 2))
            .unwrap();
        tx.try_enqueue_data(data(3, DeliveryClass::Latest, Some(2), 3))
            .unwrap();
        tx.try_enqueue_data(data(4, DeliveryClass::Latest, Some(2), 4))
            .unwrap();
        tx.try_enqueue_report_snapshot().unwrap();

        let first = report(rx.recv().await.unwrap().unwrap());
        let second = report(rx.recv().await.unwrap().unwrap());
        assert_eq!(first.per_class.latest.superseded, 2);
        assert_eq!(second.per_class.latest.superseded, 2);
        assert_eq!(first.gaps.len(), 2);
        assert!(second.gaps.is_empty());
    }

    #[tokio::test]
    async fn advisory_never_consumes_the_trailing_causal_report_slot() {
        for control_capacity in [1, 2, 3] {
            let (tx, mut rx) = channel(1, control_capacity);
            tx.set_protocol_version(3);
            assert_eq!(
                tx.try_enqueue_delivery_advisories(relay_stats()).unwrap(),
                control_capacity >= 2
            );
            if control_capacity == 3 {
                tx.try_enqueue_control(Arc::new(ServerMessage::PlayerReconnected {
                    player_id: PlayerId::from_u128(1),
                    epoch: Some(2),
                }))
                .expect("lifecycle control fits before the trailing report");
            }

            let epoch = if control_capacity == 3 { 2 } else { 1 };
            tx.try_enqueue_data(data_with_epoch(1, DeliveryClass::Reliable, None, epoch, 1))
                .unwrap();
            let outcome = tx
                .try_enqueue_data(data_with_epoch(2, DeliveryClass::Latest, Some(7), epoch, 2))
                .expect("the future exact gap appends to the trailing snapshot");
            assert_eq!(outcome.losses, 1);
            assert!(!outcome.enqueued);
            assert!(
                !tx.is_closed(),
                "capacity={control_capacity}: an advisory must not fail accountability"
            );

            if control_capacity >= 2 {
                assert!(matches!(
                    rx.recv().await.unwrap().unwrap().payload,
                    OutboundPayload::Message(message)
                        if matches!(message.as_ref(), ServerMessage::RelayStats { .. })
                ));
            }
            if control_capacity == 3 {
                assert!(matches!(
                    rx.recv().await.unwrap().unwrap().payload,
                    OutboundPayload::Message(message)
                        if matches!(
                            message.as_ref(),
                            ServerMessage::PlayerReconnected {
                                player_id,
                                epoch: Some(2),
                            } if *player_id == PlayerId::from_u128(1)
                        )
                ));
            }
            let report = report(rx.recv().await.unwrap().unwrap());
            assert_eq!(report.gaps.len(), 1);
            assert_eq!(report.gaps[0].epoch, epoch);
            assert_eq!(report.gaps[0].from_seq, 2);
            assert_eq!(report.gaps[0].to_seq, 2);
            assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 1);
        }
    }

    #[test]
    fn repeated_advisory_ticks_reuse_one_appendable_report() {
        let cases: &[(usize, &[bool])] = &[
            (1, &[false, false, false]),
            (2, &[true, false, false]),
            (3, &[true, true, false]),
        ];

        for &(control_capacity, expected_stats) in cases {
            let (tx, rx) = channel(1, control_capacity);
            tx.set_protocol_version(3);
            for (tick, &expected) in expected_stats.iter().enumerate() {
                if tick == 1 {
                    tx.record_abandoned(DeliveryClass::Latest, 3);
                }
                assert_eq!(
                    tx.try_enqueue_delivery_advisories(relay_stats())
                        .expect("an appendable tail remains usable when the lane is full"),
                    expected,
                    "control_capacity={control_capacity}, tick={tick}"
                );
            }

            let state = rx.shared.state();
            assert_eq!(state.control.len(), control_capacity);
            assert_eq!(
                state
                    .control
                    .iter()
                    .filter(|queued| matches!(&queued.payload, OutboundPayload::Message(_)))
                    .count(),
                control_capacity - 1
            );
            let reports: Vec<_> = state
                .control
                .iter()
                .filter_map(|queued| match &queued.payload {
                    OutboundPayload::DeliveryReport(report) => Some((queued.generation, report)),
                    OutboundPayload::Message(_) => None,
                })
                .collect();
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].0, state.enqueue_generation);
            assert!(reports[0].1.gaps.is_empty());
            assert_eq!(reports[0].1.per_class.latest.abandoned, 3);
        }
    }

    #[test]
    fn full_advisory_report_rolls_over_only_when_capacity_exists() {
        let full_gaps: Vec<_> = (0..DELIVERY_REPORT_MAX_GAPS)
            .map(|index| DeliveryGap {
                from_player: PlayerId::from_u128(1),
                epoch: 1,
                from_seq: (index as u64) * 2 + 1,
                to_seq: (index as u64) * 2 + 1,
                reason: DeliveryGapReason::VolatileDropped,
            })
            .collect();

        for (control_capacity, expected_stats, expected_reports) in
            [(1, None, 1), (2, Some(false), 2), (3, Some(true), 2)]
        {
            let (tx, rx) = channel(1, control_capacity);
            tx.set_protocol_version(3);
            enqueue_delivery_report(&mut rx.shared.state(), full_gaps.clone());

            let result = tx.try_enqueue_delivery_advisories(relay_stats());
            match expected_stats {
                Some(expected) => assert_eq!(result.unwrap(), expected),
                None => assert!(matches!(result, Err(TryEnqueueError::Full(_)))),
            }

            let state = rx.shared.state();
            assert_eq!(state.control.len(), control_capacity);
            let report_lengths: Vec<_> = state
                .control
                .iter()
                .filter_map(|queued| match &queued.payload {
                    OutboundPayload::DeliveryReport(report) => Some(report.gaps.len()),
                    OutboundPayload::Message(_) => None,
                })
                .collect();
            assert_eq!(report_lengths.len(), expected_reports);
            assert_eq!(report_lengths[0], DELIVERY_REPORT_MAX_GAPS);
            if expected_reports == 2 {
                assert_eq!(report_lengths[1], 0);
            }
        }
    }

    #[test]
    fn advisory_preserves_reservations_and_prior_generation_reports() {
        let (tx, rx) = channel(1, 2);
        tx.set_protocol_version(3);
        let permit = tx.try_reserve_control().unwrap();
        assert!(!tx.try_enqueue_delivery_advisories(relay_stats()).unwrap());
        assert!(!tx.try_enqueue_delivery_advisories(relay_stats()).unwrap());
        permit.send_control(message(9)).unwrap();

        tx.try_enqueue_data(data(1, DeliveryClass::Reliable, None, 1))
            .unwrap();
        tx.try_enqueue_data(data(2, DeliveryClass::Latest, Some(7), 2))
            .expect("the reserved control and reused report leave exact gap capacity");
        let state = rx.shared.state();
        assert_eq!(state.reserved_control, 0);
        assert_eq!(state.control.len(), 2);
        let OutboundPayload::DeliveryReport(report) = &state.control.back().unwrap().payload else {
            panic!("expected trailing report")
        };
        assert_eq!(report.gaps.len(), 1);
        drop(state);

        let full_gaps = vec![
            DeliveryGap {
                from_player: PlayerId::from_u128(1),
                epoch: 1,
                from_seq: 1,
                to_seq: 1,
                reason: DeliveryGapReason::VolatileDropped,
            };
            DELIVERY_REPORT_MAX_GAPS
        ];
        let (tx, rx) = channel(1, 2);
        tx.set_protocol_version(3);
        enqueue_delivery_report(&mut rx.shared.state(), full_gaps);
        let _permit = tx.try_reserve_control().unwrap();
        assert!(matches!(
            tx.try_enqueue_delivery_advisories(relay_stats()),
            Err(TryEnqueueError::Full(_))
        ));
        assert_eq!(rx.shared.state().control.len(), 1);

        for (control_capacity, expected_stats) in [(2, None), (3, Some(false)), (4, Some(true))] {
            let (tx, rx) = channel(1, control_capacity);
            tx.set_protocol_version(3);
            tx.try_enqueue_report_snapshot().unwrap();
            tx.try_enqueue_transition(Arc::new(ServerMessage::RoomLeft), 1)
                .unwrap();
            let result = tx.try_enqueue_delivery_advisories(relay_stats());
            match expected_stats {
                Some(expected) => assert_eq!(result.unwrap(), expected),
                None => assert!(matches!(result, Err(TryEnqueueError::Full(_)))),
            }

            let state = rx.shared.state();
            let report_generations: Vec<_> = state
                .control
                .iter()
                .filter_map(|queued| {
                    matches!(&queued.payload, OutboundPayload::DeliveryReport(_))
                        .then_some(queued.generation)
                })
                .collect();
            let expected_generations = if expected_stats.is_some() {
                vec![0, 1]
            } else {
                vec![0]
            };
            assert_eq!(report_generations, expected_generations);
            assert_eq!(state.barriers.len(), 1);
        }
    }

    #[tokio::test]
    async fn full_latest_evicts_oldest_volatile_before_dropping_arrival() {
        let (tx, mut rx) = channel(2, 4);
        tx.set_protocol_version(3);
        tx.try_enqueue_data(data(1, DeliveryClass::Reliable, None, 1))
            .unwrap();
        tx.try_enqueue_data(data(2, DeliveryClass::Volatile, None, 2))
            .unwrap();
        let outcome = tx
            .try_enqueue_data(data(3, DeliveryClass::Latest, Some(8), 3))
            .unwrap();
        assert_eq!(outcome.losses, 1);
        assert!(outcome.enqueued);
        let first_report = report(rx.recv().await.unwrap().unwrap());
        assert_eq!(first_report.gaps[0].from_seq, 2);
        assert_eq!(
            first_report.gaps[0].reason,
            DeliveryGapReason::VolatileDropped
        );
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 1);
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 3);

        let (tx, mut rx) = channel(1, 2);
        tx.set_protocol_version(3);
        tx.try_enqueue_data(data(1, DeliveryClass::Reliable, None, 1))
            .unwrap();
        let outcome = tx
            .try_enqueue_data(data(2, DeliveryClass::Latest, Some(8), 2))
            .unwrap();
        assert_eq!(
            outcome,
            EnqueueOutcome {
                enqueued: false,
                losses: 1
            }
        );
        let second_report = report(rx.recv().await.unwrap().unwrap());
        assert_eq!(second_report.gaps[0].from_seq, 2);
        assert_eq!(
            second_report.gaps[0].reason,
            DeliveryGapReason::LatestDroppedFull
        );
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 1);
    }

    #[tokio::test]
    async fn volatile_replaces_oldest_volatile_but_never_reliable() {
        let (tx, mut rx) = channel(3, 4);
        tx.set_protocol_version(3);
        for item in [
            data(1, DeliveryClass::Reliable, None, 1),
            data(2, DeliveryClass::Volatile, None, 2),
            data(3, DeliveryClass::Volatile, None, 3),
        ] {
            tx.try_enqueue_data(item).unwrap();
        }
        tx.try_enqueue_data(data(4, DeliveryClass::Volatile, None, 4))
            .unwrap();
        let report = report(rx.recv().await.unwrap().unwrap());
        assert_eq!(report.gaps[0].from_seq, 2);
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 1);
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 3);
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 4);
    }

    #[tokio::test]
    async fn legacy_prefix_stays_ahead_of_post_negotiation_control() {
        let (tx, mut rx) = channel(2, 2);
        tx.try_enqueue_control(message(1)).unwrap();
        tx.set_protocol_version(3);
        tx.try_enqueue_control(message(2)).unwrap();
        assert!(rx.try_recv_control().unwrap().is_none());
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 1);
        assert_eq!(message_id(&rx.try_recv_control().unwrap().unwrap()), 2);
    }

    #[test]
    fn class_counts_ignore_control_and_treat_pre_v3_lossy_as_reliable() {
        let (tx, rx) = channel(2, 1);
        tx.try_enqueue_control(message(1)).unwrap();
        tx.try_enqueue_data(data(2, DeliveryClass::Latest, Some(3), 1))
            .unwrap();
        assert_eq!(
            rx.count_by_class(),
            [
                (DeliveryClass::Reliable, 1),
                (DeliveryClass::Latest, 0),
                (DeliveryClass::Volatile, 0),
            ]
        );
    }

    #[test]
    fn queue_rejects_control_data_and_mismatched_delivery_metadata() {
        let (tx, _rx) = channel(2, 2);
        tx.set_protocol_version(3);
        let game_data = data(1, DeliveryClass::Reliable, None, 1);
        assert!(matches!(
            tx.try_enqueue_control(Arc::clone(&game_data.message)),
            Err(TryEnqueueError::InvalidMetadata(_))
        ));
        let permit = tx.try_reserve_control().unwrap();
        assert!(permit.send_control(Arc::clone(&game_data.message)).is_err());

        let stamped_without_metadata =
            OutboundData::reliable_unstamped(Arc::clone(&game_data.message));
        assert!(matches!(
            tx.try_enqueue_data(stamped_without_metadata),
            Err(TryEnqueueError::InvalidMetadata(_))
        ));

        let mut mismatched = game_data;
        mismatched.metadata.as_mut().unwrap().seq = 9;
        assert!(matches!(
            tx.try_enqueue_data(mismatched),
            Err(TryEnqueueError::InvalidMetadata(_))
        ));
    }

    #[tokio::test]
    async fn lossy_change_fails_closed_when_report_lane_is_full() {
        let (tx, mut rx) = channel(1, 1);
        tx.set_protocol_version(3);
        tx.try_enqueue_control(message(9)).unwrap();
        tx.try_enqueue_data(data(1, DeliveryClass::Latest, Some(1), 1))
            .unwrap();
        assert!(matches!(
            tx.try_enqueue_data(data(2, DeliveryClass::Latest, Some(1), 2)),
            Err(TryEnqueueError::AccountabilityUnavailable(_))
        ));
        assert!(tx.is_closed());
        assert!(matches!(
            tx.try_enqueue_control(message(10)),
            Err(TryEnqueueError::Closed(_))
        ));
        assert!(matches!(
            tx.try_reserve_control(),
            Err(ReserveError::Closed)
        ));
        let receive_error = rx
            .try_recv()
            .expect_err("accountability failure is terminal");
        assert_eq!(receive_error, TryReceiveError::AccountabilityFailed);
        assert_eq!(
            rx.try_recv_control().unwrap_err(),
            TryReceiveError::AccountabilityFailed
        );
        assert_eq!(
            rx.recv().await.unwrap_err(),
            TryReceiveError::AccountabilityFailed
        );
    }

    #[tokio::test]
    async fn permit_drop_releases_capacity_and_last_sender_closes_receiver() {
        let (tx, mut rx) = channel(1, 1);
        tx.set_protocol_version(3);
        let permit = tx.try_reserve_control().unwrap();
        assert!(matches!(
            tx.try_enqueue_control(message(1)),
            Err(TryEnqueueError::Full(_))
        ));
        drop(permit);
        tx.try_enqueue_control(message(2)).unwrap();
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 2);
        drop(tx);
        assert!(rx.recv().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn permit_keeps_receiver_open_after_last_sender_and_can_commit() {
        let (tx, mut rx) = channel(1, 1);
        tx.set_protocol_version(3);
        let permit = tx.try_reserve_control().unwrap();
        drop(tx);
        let receive_error = rx
            .try_recv()
            .expect_err("reserved capacity keeps the queue empty");
        assert_eq!(receive_error, TryReceiveError::Empty);
        permit.send_control(message(4)).unwrap();
        assert_eq!(message_id(&rx.recv().await.unwrap().unwrap()), 4);
        assert!(rx.recv().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn waiting_receiver_observes_final_producer_enqueue_before_disconnect() {
        let (tx, mut rx) = channel(1, 1);
        let waiter = tokio::spawn(async move {
            let item = rx.recv().await;
            let closed = rx.recv().await;
            (item, closed)
        });
        tokio::task::yield_now().await;
        tx.try_enqueue_control(message(5)).unwrap();
        drop(tx);
        let (item, closed) = waiter.await.unwrap();
        assert_eq!(message_id(&item.unwrap().unwrap()), 5);
        assert!(closed.unwrap().is_none());
    }

    fn unsupported_metadata(
        class: DeliveryClass,
        from_player: u128,
        seq: u64,
    ) -> DataDeliveryMetadata {
        DataDeliveryMetadata {
            class,
            key: None,
            from_player: PlayerId::from_u128(from_player),
            room_id: RoomId::from_u128(5),
            epoch: 6,
            seq,
        }
    }

    /// One frame per omitted message made accountability cost the weakest
    /// recipient several times the payload it replaced (issue #212). Contiguous
    /// omissions must collapse into one range while the counters stay exact.
    #[test]
    fn unsupported_format_reports_coalesce_and_stay_exact_per_class() {
        let (tx, rx) = channel(1, 1);
        tx.set_protocol_version(3);
        for seq in 7..=9 {
            assert!(
                rx.record_unsupported_format(unsupported_metadata(DeliveryClass::Volatile, 4, seq))
                    .is_none(),
                "contiguous omissions from one sender must coalesce"
            );
        }
        // A different class cannot share a range: `UnsupportedFormat` is the one
        // gap reason every class uses, so merging across classes would lose the
        // attribution the per-class counters need.
        assert!(rx
            .record_unsupported_format(unsupported_metadata(DeliveryClass::Reliable, 4, 10))
            .is_none());
        // A different sender starts its own range.
        assert!(rx
            .record_unsupported_format(unsupported_metadata(DeliveryClass::Volatile, 11, 1))
            .is_none());

        let report = rx
            .take_pending_unsupported_report()
            .expect("pending accountability must flush");
        assert_eq!(report.per_class.volatile.unsupported_format, 4);
        assert_eq!(report.per_class.reliable.unsupported_format, 1);
        let ranges: Vec<_> = report
            .gaps
            .iter()
            .map(|gap| (gap.from_player, gap.from_seq, gap.to_seq, gap.reason))
            .collect();
        assert_eq!(
            ranges,
            vec![
                (
                    PlayerId::from_u128(4),
                    10,
                    10,
                    DeliveryGapReason::UnsupportedFormat
                ),
                (
                    PlayerId::from_u128(4),
                    7,
                    9,
                    DeliveryGapReason::UnsupportedFormat
                ),
                (
                    PlayerId::from_u128(11),
                    1,
                    1,
                    DeliveryGapReason::UnsupportedFormat
                ),
            ],
            "five omissions must be reported as three exact ranges"
        );
        assert!(
            rx.take_pending_unsupported_report().is_none(),
            "a flushed report leaves nothing pending"
        );
    }

    /// The connection's teardown flushes whatever is still coalesced, so closing
    /// the queue must not strand it: `finalize_closed_connection` takes the
    /// pending report *after* `rx.close()`, and a cleared or refused buffer there
    /// would silently drop the last omissions instead of reporting them.
    #[test]
    fn closing_the_queue_preserves_pending_unsupported_accountability() {
        let (tx, mut rx) = channel(1, 1);
        tx.set_protocol_version(3);
        assert!(rx
            .record_unsupported_format(unsupported_metadata(DeliveryClass::Reliable, 4, 1))
            .is_none());
        rx.close();
        let report = rx
            .take_pending_unsupported_report()
            .expect("a closing connection must still be able to report its last omissions");
        assert_eq!(report.per_class.reliable.unsupported_format, 1);
        assert_eq!(report.gaps.len(), 1);
    }

    /// The pending report is bounded by the canonical per-report gap maximum,
    /// and hitting that bound flushes rather than dropping accountability.
    #[test]
    fn pending_unsupported_report_is_bounded_and_flushes_when_full() {
        let (tx, rx) = channel(1, 1);
        tx.set_protocol_version(3);
        // Every second sequence is a hole, so no two omissions can merge.
        let mut flushed = None;
        for index in 0..=DELIVERY_REPORT_MAX_GAPS {
            let seq = (index as u64) * 2;
            let report =
                rx.record_unsupported_format(unsupported_metadata(DeliveryClass::Reliable, 4, seq));
            if let Some(report) = report {
                assert!(flushed.is_none(), "only the bounded report may flush");
                flushed = Some(report);
            }
        }
        let flushed = flushed.expect("the bounded report must flush when it is full");
        assert_eq!(flushed.gaps.len(), DELIVERY_REPORT_MAX_GAPS);
        assert_eq!(
            flushed.per_class.reliable.unsupported_format,
            DELIVERY_REPORT_MAX_GAPS as u64
        );
        let remainder = rx
            .take_pending_unsupported_report()
            .expect("the omission that displaced the report is still accounted for");
        assert_eq!(remainder.gaps.len(), 1);
        assert_eq!(
            remainder.per_class.reliable.unsupported_format,
            DELIVERY_REPORT_MAX_GAPS as u64 + 1,
            "counters advance monotonically with the ranges that explain them"
        );
    }

    /// A recipient that never negotiated v3 receives no `DeliveryReport` at all,
    /// so nothing may accumulate for it — a later flush would put a v3-only
    /// frame on a frozen v2 wire.
    #[test]
    fn pre_v3_recipients_accumulate_no_delivery_report() {
        let (_tx, rx) = channel(1, 1);
        assert!(rx
            .record_unsupported_format(unsupported_metadata(DeliveryClass::Reliable, 4, 1))
            .is_none());
        assert!(rx.take_pending_unsupported_report().is_none());
        assert_eq!(
            rx.shared.state().counters.reliable.unsupported_format,
            1,
            "the omission is still counted internally"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unsupported_notice_limiter_is_per_sender_and_reports_suppression() {
        let (_tx, rx) = channel(1, 1);
        let first = PlayerId::from_u128(1);
        let second = PlayerId::from_u128(2);

        assert_eq!(rx.unsupported_notice(first), Some(0));
        assert_eq!(rx.unsupported_notice(first), None);
        assert_eq!(rx.unsupported_notice(second), Some(0));
        tokio::time::advance(UNSUPPORTED_NOTICE_INTERVAL - Duration::from_millis(1)).await;
        assert_eq!(rx.unsupported_notice(first), None);
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(rx.unsupported_notice(first), Some(2));
        assert_eq!(rx.unsupported_notice(first), None);
    }

    #[tokio::test(start_paused = true)]
    async fn unsupported_notice_limiter_bounds_sender_state() {
        let (_tx, rx) = channel(1, 1);
        for ordinal in 0..=MAX_UNSUPPORTED_NOTICE_SENDERS {
            assert_eq!(
                rx.unsupported_notice(PlayerId::from_u128(ordinal as u128 + 1)),
                Some(0)
            );
        }
        assert_eq!(
            rx.shared
                .unsupported_notices
                .lock()
                .expect("notice limiter poisoned")
                .senders
                .len(),
            MAX_UNSUPPORTED_NOTICE_SENDERS
        );
    }
}
