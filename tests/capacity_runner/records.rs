//! Raw run events — the artifact-level truth a run replays from.
//!
//! Everything the oracle consumes is recorded as append-only event lists
//! (sends, receipts, gap reports, unsupported-format notices, disconnects)
//! plus join failures. The
//! summary is a pure function of these events plus the plans and the run's
//! delivery class, so "replay the artifacts" and "summarize the run" are
//! literally the same code path.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::Mutex;
#[cfg(test)]
use std::time::Duration;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::oracle::InvalidReason;
use crate::schedule::{Phase, SenderPlan};

/// One send a sender task actually completed (it left this process).
///
/// `intended_us` vs `sent_us` is the scheduled-send lag: a pause must appear
/// here (generator-side), never as reduced offered load. `epoch` is the
/// sender's incarnation epoch at send time (1 on the first connection; each
/// rejoin bumps it) — the wire `(epoch, server seq)` stream key pairs with
/// it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SentEvent<Name = ReceiptName> {
    pub sender: Name,
    pub room: u32,
    pub seq: u64,
    pub epoch: u32,
    pub intended_us: u64,
    pub sent_us: u64,
    /// Serialized application value size, excluding the protocol envelope.
    pub application_bytes: u64,
    /// Actual encoded WebSocket message body size, excluding WebSocket headers.
    pub encoded_frame_body_bytes: u64,
    pub phase: Phase,
}

/// An event identity shared within one collected or replayed run.
/// Serialization stays a JSON string, including names outside the run roster.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
// Arc<String> keeps each stored name to one pointer; Arc<str> needs two.
// Live tasks allocate once per peer identity. The replay pool shares exact names.
pub struct ReceiptName(Arc<String>);

impl ReceiptName {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl From<String> for ReceiptName {
    fn from(name: String) -> Self {
        Self(Arc::new(name))
    }
}

impl From<&str> for ReceiptName {
    fn from(name: &str) -> Self {
        name.to_string().into()
    }
}

impl std::ops::Deref for ReceiptName {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl std::borrow::Borrow<str> for ReceiptName {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl<T: AsRef<str> + ?Sized> PartialEq<T> for ReceiptName {
    fn eq(&self, other: &T) -> bool {
        self.as_str() == other.as_ref()
    }
}

impl std::fmt::Display for ReceiptName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl serde::Serialize for ReceiptName {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for ReceiptName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::from)
    }
}

/// Replay-only send and receipt names. Live tasks clone their peer and registry names
/// directly, without a pool lookup or duplicate-string cleanup in the log.
#[derive(Default)]
pub(super) struct ReceiptNames(BTreeSet<ReceiptName>);

impl ReceiptNames {
    fn intern(&mut self, name: &str) -> ReceiptName {
        if let Some(shared) = self.0.get(name) {
            return shared.clone();
        }
        let shared = ReceiptName::from(name);
        self.0.insert(shared.clone());
        shared
    }

    pub(super) fn share_sent(&mut self, sent: SentEvent<impl AsRef<str>>) -> SentEvent {
        SentEvent {
            sender: self.intern(sent.sender.as_ref()),
            room: sent.room,
            seq: sent.seq,
            epoch: sent.epoch,
            intended_us: sent.intended_us,
            sent_us: sent.sent_us,
            application_bytes: sent.application_bytes,
            encoded_frame_body_bytes: sent.encoded_frame_body_bytes,
            phase: sent.phase,
        }
    }

    pub(super) fn share(&mut self, receipt: ReceiptEvent<impl AsRef<str>>) -> ReceiptEvent {
        ReceiptEvent {
            recipient: self.intern(receipt.recipient.as_ref()),
            sender: self.intern(receipt.sender.as_ref()),
            seq: receipt.seq,
            epoch: receipt.epoch,
            server_seq: receipt.server_seq,
            received_us: receipt.received_us,
            application_bytes: receipt.application_bytes,
            encoded_frame_body_bytes: receipt.encoded_frame_body_bytes,
        }
    }
}

/// One delivery a recipient task actually read off the socket.
///
/// `seq` is the sender-stamped ledger sequence (unique across the whole run,
/// what latency pairs on). `epoch` and `server_seq` are the server's
/// per-`(sender, epoch)` relay stamps read off the v3 wire frame — the
/// stream-identity pair the oracle validates.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReceiptEvent<Name = ReceiptName> {
    pub recipient: Name,
    pub sender: Name,
    pub seq: u64,
    pub epoch: u32,
    pub server_seq: u64,
    pub received_us: u64,
    /// Serialized application value size, excluding the protocol envelope.
    pub application_bytes: u64,
    /// Actual encoded WebSocket message body size, excluding WebSocket headers.
    pub encoded_frame_body_bytes: u64,
}

/// One declared churn action a peer task performed: its socket closed for
/// the storm (`disconnect`) or it rejoined under a new incarnation epoch
/// (`rejoined`). The `tails` of a `rejoined` event are the rejoin
/// snapshot's per-member `(PlayerId, seq tail)` stamps, UNRESOLVED — the
/// oracle resolves each id through the run's recorded registry, so the
/// mapping never depends on task scheduling order. A tail is the owed
/// floor for the away window ("a recipient owes no GameData at or below
/// this sequence in the paired stream").
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChurnEvent {
    pub recipient: String,
    pub phase: ChurnPhase,
    pub at_us: u64,
    /// The peer's incarnation epoch after the action (`None` on disconnect,
    /// where the connection is simply gone).
    pub epoch: Option<u32>,
    /// For `rejoined`: the snapshot's member `(PlayerId, seq tail)` stamps.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tails: BTreeMap<String, (String, u64)>,
}

/// Which half of a churn cycle an event records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChurnPhase {
    Disconnect,
    Rejoined,
}

/// Why a recipient's stream ended before quiescence.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DisconnectObservation {
    /// The server sent a WebSocket close frame (code recorded when present).
    ServerClosed(Option<u16>),
    /// The socket errored or ended without a close frame.
    StreamEnded,
}

/// One exact server-stamped omission a recipient read off a
/// `DeliveryReport`: the inclusive server sequence range
/// `from_seq..=to_seq` (1-based, one sender, one epoch) was not delivered,
/// with the contract reason. The oracle maps the range onto the sender's
/// ledger stream (`server seq = ledger seq + 1`) and checks exact coverage.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GapEvent {
    pub recipient: String,
    /// Peer name resolved from the report's `from_player` id.
    pub sender: String,
    pub epoch: u32,
    pub from_seq: u64,
    pub to_seq: u64,
    pub reason: signal_fish_server::protocol::DeliveryGapReason,
}

/// A rate-limited unsupported-format advisory a cross-format recipient read
/// off an `Error` frame (`UnsupportedGameDataFormat`): the prose companion
/// of the exact `unsupported_format` gap reports. Recorded as evidence —
/// the oracle bounds its cadence (at most one per opaque sender per second)
/// instead of treating it as a rejection.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnsupportedNoticeEvent {
    pub recipient: String,
    pub at_us: u64,
}

/// A recipient that stopped being a delivery target before quiescence.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DisconnectEvent {
    pub recipient: String,
    pub observation: DisconnectObservation,
}

/// Diagnostics are opt-in and bounded independently of receipt count.
const MAX_SLOW_RECEIPT_TIMINGS: usize = 64;
const SLOW_RECEIPT_NS: u64 = 1_000_000;

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TimingAggregate {
    pub count: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    pub overflowed: bool,
}

impl TimingAggregate {
    fn record(&mut self, elapsed_ns: u64) {
        let (count, count_overflow) = self.count.overflowing_add(1);
        let (total, total_overflow) = self.total_ns.overflowing_add(elapsed_ns);
        self.overflowed |= count_overflow || total_overflow;
        self.count = if count_overflow { u64::MAX } else { count };
        self.total_ns = if total_overflow { u64::MAX } else { total };
        self.max_ns = self.max_ns.max(elapsed_ns);
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ReceiptTimingObservation {
    receipt_index: usize,
    seq: u64,
    epoch: u32,
    started_offset_ns: u64,
    finished_offset_ns: u64,
    acquire_wait_ns: u64,
    receipt_push_ns: u64,
    measured_lock_hold_ns: u64,
    receipt_len_before: usize,
    receipt_capacity_before: usize,
    receipt_len_after: usize,
    receipt_capacity_after: usize,
}

impl ReceiptTimingObservation {
    fn score(&self) -> u64 {
        self.acquire_wait_ns
            .max(self.receipt_push_ns)
            .max(self.measured_lock_hold_ns)
    }
}

#[derive(Debug, serde::Serialize)]
pub struct ReceiptTimingLagPrefix {
    notes: &'static str,
    request_offset_ns: u64,
    capture_offset_ns: u64,
    sender: String,
    seq: u64,
    stage: &'static str,
    observed_us: u64,
    bound_us: u64,
    acquire_wait: TimingAggregate,
    receipt_push: TimingAggregate,
    measured_lock_hold: TimingAggregate,
    worst_acquire_wait: Option<ReceiptTimingObservation>,
    worst_receipt_push: Option<ReceiptTimingObservation>,
    worst_measured_lock_hold: Option<ReceiptTimingObservation>,
}

#[derive(Debug, serde::Serialize)]
pub struct ReceiptTimingSnapshot {
    enabled: bool,
    notes: &'static str,
    wall_anchor_unix_ns: Option<u64>,
    wall_anchor_offset_before_ns: u64,
    wall_anchor_offset_after_ns: u64,
    workload_epoch_offset_ns: Option<u64>,
    first_accepted_lag_prefix: Option<Box<ReceiptTimingLagPrefix>>,
    worst_acquire_wait: Option<ReceiptTimingObservation>,
    worst_receipt_push: Option<ReceiptTimingObservation>,
    worst_measured_lock_hold: Option<ReceiptTimingObservation>,
    slow_threshold_ns: u64,
    slow_observation_limit: usize,
    pub acquire_wait: TimingAggregate,
    pub receipt_push: TimingAggregate,
    pub measured_lock_hold: TimingAggregate,
    slow_observations_seen: u64,
    slow_observations_not_retained: u64,
    slow_observations: Vec<ReceiptTimingObservation>,
}

impl ReceiptTimingSnapshot {
    fn new(start: Instant) -> Self {
        let before = elapsed_ns(start);
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| u64::try_from(elapsed.as_nanos()).ok());
        let after = elapsed_ns(start);
        Self {
            enabled: true,
            notes: "Opt-in diagnostics add clock reads and bookkeeping. All stage durations are elapsed wall-clock time and include descheduling. Acquire wait does not distinguish mutex contention from scheduling delay. Offsets start at EventLog construction; workload_epoch_offset_ns maps the armed workload epoch when provided. The wall anchor lies within its before/after monotonic offset bracket. Measured lock hold ends before its final counter update, slow-observation finalization, and unlock. Retains the first 64 slow observations plus one worst observation per measured stage; aggregates cover every receipt. At most 70 observation rows are retained across the slow buffer, three final maxima, and three frozen prefix maxima. Observation rows retain scalar receipt positions; exact names are resolved only after collection. No TCP attribution.",
            wall_anchor_unix_ns: wall,
            wall_anchor_offset_before_ns: before,
            wall_anchor_offset_after_ns: after,
            workload_epoch_offset_ns: None,
            first_accepted_lag_prefix: None,
            worst_acquire_wait: None,
            worst_receipt_push: None,
            worst_measured_lock_hold: None,
            slow_threshold_ns: SLOW_RECEIPT_NS,
            slow_observation_limit: MAX_SLOW_RECEIPT_TIMINGS,
            acquire_wait: TimingAggregate::default(),
            receipt_push: TimingAggregate::default(),
            measured_lock_hold: TimingAggregate::default(),
            slow_observations_seen: 0,
            slow_observations_not_retained: 0,
            slow_observations: Vec::with_capacity(MAX_SLOW_RECEIPT_TIMINGS),
        }
    }

    pub fn into_evidence(self, records: &RunRecords) -> Result<serde_json::Value, String> {
        fn add_identity(row: &mut serde_json::Value, records: &RunRecords) -> Result<(), String> {
            if row.is_null() {
                return Ok(());
            }
            let index = row["receipt_index"]
                .as_u64()
                .and_then(|index| usize::try_from(index).ok())
                .ok_or_else(|| {
                    "Receipt timing observation has an invalid receipt index".to_string()
                })?;
            let receipt = records.receipts.get(index).ok_or_else(|| {
                format!(
                    "Receipt timing index {index} exceeds {} receipts",
                    records.receipts.len()
                )
            })?;
            row["recipient"] = serde_json::Value::String(receipt.recipient.to_string());
            row["sender"] = serde_json::Value::String(receipt.sender.to_string());
            Ok(())
        }
        let mut evidence = serde_json::to_value(self)
            .map_err(|error| format!("Serialize receipt timings: {error}"))?;
        for field in [
            "worst_acquire_wait",
            "worst_receipt_push",
            "worst_measured_lock_hold",
        ] {
            add_identity(&mut evidence[field], records)?;
            if !evidence["first_accepted_lag_prefix"].is_null() {
                add_identity(&mut evidence["first_accepted_lag_prefix"][field], records)?;
            }
        }
        if let Some(rows) = evidence["slow_observations"].as_array_mut() {
            for row in rows {
                add_identity(row, records)?;
            }
        }
        Ok(evidence)
    }

    fn retain_observation(&mut self, observation: ReceiptTimingObservation) {
        if self
            .worst_acquire_wait
            .as_ref()
            .is_none_or(|old| observation.acquire_wait_ns > old.acquire_wait_ns)
        {
            self.worst_acquire_wait = Some(observation.clone());
        }
        if self
            .worst_receipt_push
            .as_ref()
            .is_none_or(|old| observation.receipt_push_ns > old.receipt_push_ns)
        {
            self.worst_receipt_push = Some(observation.clone());
        }
        if self
            .worst_measured_lock_hold
            .as_ref()
            .is_none_or(|old| observation.measured_lock_hold_ns > old.measured_lock_hold_ns)
        {
            self.worst_measured_lock_hold = Some(observation.clone());
        }
        self.retain_slow(observation);
    }

    fn retain_slow(&mut self, observation: ReceiptTimingObservation) {
        if observation.score() < SLOW_RECEIPT_NS {
            return;
        }
        self.slow_observations_seen = self.slow_observations_seen.saturating_add(1);
        if self.slow_observations.len() < MAX_SLOW_RECEIPT_TIMINGS {
            self.slow_observations.push(observation);
        } else {
            self.slow_observations_not_retained =
                self.slow_observations_not_retained.saturating_add(1);
        }
    }
}

fn elapsed_ns(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[derive(Default)]
struct ReceiptTimingHooks {
    wait_started: Option<std::sync::mpsc::SyncSender<()>>,
    push_delay: Duration,
    bookkeeping_delay: Duration,
}

#[cfg(test)]
fn pause_receipt_timing(delay: Duration) {
    if !delay.is_zero() {
        std::thread::sleep(delay);
    }
}

/// Every event one run recorded. Collectors push through an
/// `Arc<EventLog>`; the mutex is never held across an `.await`.
#[derive(Default)]
pub struct EventLog {
    state: Mutex<EventState>,
    receipt_timing_start: Option<Instant>,
    #[cfg(test)]
    receipt_timing_hooks: ReceiptTimingHooks,
}

#[derive(Default)]
struct EventState {
    receipt_timings: Option<ReceiptTimingSnapshot>,
    sent: Vec<SentEvent>,
    receipts: Vec<ReceiptEvent>,
    gaps: Vec<GapEvent>,
    unsupported_notices: Vec<UnsupportedNoticeEvent>,
    disconnects: Vec<DisconnectEvent>,
    churn: Vec<ChurnEvent>,
    join_failures: Vec<String>,
    /// Faults the runner itself observed (hook firings, send failures,
    /// malformed frames). The oracle folds these into the verdict, so a
    /// replay reproduces it exactly from the same events.
    faults: Vec<InvalidReason>,
    server_terminated: bool,
    /// The complete sender registry at end of run: `PlayerId` ->
    /// `(peer name, incarnation)`. Recorded once, after every task has
    /// joined, so artifact consumers can resolve any snapshot identity
    /// deterministically — never through a racy mid-run lookup.
    registry: BTreeMap<String, (String, u32)>,
}

impl EventLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_receipt_timings(enabled: bool) -> Self {
        let mut log = Self::new();
        if enabled {
            let start = Instant::now();
            log.receipt_timing_start = Some(start);
            log.state
                .get_mut()
                .expect("event log poisoned")
                .receipt_timings = Some(ReceiptTimingSnapshot::new(start));
        }
        log
    }

    pub fn for_plans(plans: &[SenderPlan], receipt_timings: bool) -> Result<Self, String> {
        let mut members_by_room = BTreeMap::<u32, usize>::new();
        for plan in plans {
            *members_by_room.entry(plan.room).or_default() += 1;
        }
        let (sent, receipts) =
            plans
                .iter()
                .try_fold((0_usize, 0_usize), |(sent, receipts), plan| {
                    let fanout = members_by_room[&plan.room] - 1;
                    let planned_receipts = plan
                        .sends
                        .len()
                        .checked_mul(fanout)
                        .ok_or_else(|| "planned receipt capacity overflows".to_string())?;
                    Ok::<_, String>((
                        sent.checked_add(plan.sends.len())
                            .ok_or_else(|| "planned send capacity overflows".to_string())?,
                        receipts
                            .checked_add(planned_receipts)
                            .ok_or_else(|| "planned receipt capacity overflows".to_string())?,
                    ))
                })?;
        Self::with_planned_capacity(sent, receipts, receipt_timings)
    }

    fn with_planned_capacity(
        sent: usize,
        receipts: usize,
        receipt_timings: bool,
    ) -> Result<Self, String> {
        let mut log = Self::with_receipt_timings(receipt_timings);
        let state = log.state.get_mut().expect("new event log");
        state
            .sent
            .try_reserve_exact(sent)
            .map_err(|error| format!("reserve {sent} planned sends: {error}"))?;
        state
            .receipts
            .try_reserve_exact(receipts)
            .map_err(|error| format!("reserve {receipts} planned receipts: {error}"))?;
        Ok(log)
    }

    pub fn set_workload_epoch(&self, epoch: Instant) {
        let Some(start) = self.receipt_timing_start else {
            return;
        };
        let offset = epoch
            .checked_duration_since(start)
            .and_then(|duration| u64::try_from(duration.as_nanos()).ok());
        if let Some(timings) = &mut self
            .state
            .lock()
            .expect("event log poisoned")
            .receipt_timings
        {
            timings.workload_epoch_offset_ns = offset;
        }
    }

    pub fn capture_receipt_timings_at_send_lag(
        &self,
        sender: &str,
        seq: u64,
        stage: &'static str,
        observed_us: u64,
        bound_us: u64,
    ) {
        let Some(start) = self.receipt_timing_start else {
            return;
        };
        let request_offset_ns = elapsed_ns(start);
        let mut state = self.state.lock().expect("event log poisoned");
        let Some(timings) = &mut state.receipt_timings else {
            return;
        };
        if timings.first_accepted_lag_prefix.is_some() {
            return;
        }
        let capture_offset_ns = elapsed_ns(start);
        timings.first_accepted_lag_prefix = Some(Box::new(ReceiptTimingLagPrefix {
            notes: "First accepted capture under the existing receipt-log mutex, not guaranteed earliest request or exact lag crossing. Counters and stage maxima describe the prefix at acquisition, before warning serialization. Request-to-capture time includes scheduling and mutex delay. Capture allocation and bookkeeping occur after a confirmed bound breach and can affect subsequent failure history.",
            request_offset_ns,
            capture_offset_ns,
            sender: sender.to_string(),
            seq,
            stage,
            observed_us,
            bound_us,
            acquire_wait: timings.acquire_wait.clone(),
            receipt_push: timings.receipt_push.clone(),
            measured_lock_hold: timings.measured_lock_hold.clone(),
            worst_acquire_wait: timings.worst_acquire_wait.clone(),
            worst_receipt_push: timings.worst_receipt_push.clone(),
            worst_measured_lock_hold: timings.worst_measured_lock_hold.clone(),
        }));
    }

    /// Call only after every evidence producer has stopped. This sidecar never
    /// enters RunRecords, delivery artifacts, or the delivery oracle.
    pub fn take_receipt_timings(&self) -> Option<ReceiptTimingSnapshot> {
        self.state
            .lock()
            .expect("event log poisoned")
            .receipt_timings
            .take()
    }

    pub fn push_sent(&self, event: SentEvent) {
        self.state
            .lock()
            .expect("event log poisoned")
            .sent
            .push(event);
    }

    pub fn push_receipt(&self, event: ReceiptEvent) {
        let Some(start) = self.receipt_timing_start else {
            let mut state = self.state.lock().expect("event log poisoned");
            state.receipts.push(event);
            return;
        };
        let waiting = elapsed_ns(start);
        #[cfg(test)]
        if let Some(entered) = &self.receipt_timing_hooks.wait_started {
            entered.send(()).expect("receipt timing test entry");
        }
        let mut state = self.state.lock().expect("event log poisoned");
        let acquired = elapsed_ns(start);
        let len_before = state.receipts.len();
        let capacity_before = state.receipts.capacity();
        let push_started = elapsed_ns(start);
        #[cfg(test)]
        pause_receipt_timing(self.receipt_timing_hooks.push_delay);
        state.receipts.push(event);
        let pushed = elapsed_ns(start);
        let len_after = state.receipts.len();
        let capacity_after = state.receipts.capacity();
        if let Some(timings) = &mut state.receipt_timings {
            timings
                .acquire_wait
                .record(acquired.saturating_sub(waiting));
            timings
                .receipt_push
                .record(pushed.saturating_sub(push_started));
        }
        #[cfg(test)]
        pause_receipt_timing(self.receipt_timing_hooks.bookkeeping_delay);
        let finished = elapsed_ns(start);
        let hold = finished.saturating_sub(acquired);
        let wait = acquired.saturating_sub(waiting);
        let push = pushed.saturating_sub(push_started);
        // Capture scalar receipt positions only for retained rows or new stage maxima.
        let is_slow = wait.max(push).max(hold) >= SLOW_RECEIPT_NS;
        let needs_observation = state.receipt_timings.as_ref().is_some_and(|timings| {
            (is_slow && timings.slow_observations.len() < MAX_SLOW_RECEIPT_TIMINGS)
                || timings
                    .worst_acquire_wait
                    .as_ref()
                    .is_none_or(|old| wait > old.acquire_wait_ns)
                || timings
                    .worst_receipt_push
                    .as_ref()
                    .is_none_or(|old| push > old.receipt_push_ns)
                || timings
                    .worst_measured_lock_hold
                    .as_ref()
                    .is_none_or(|old| hold > old.measured_lock_hold_ns)
        });
        let slow = if needs_observation {
            state
                .receipts
                .last()
                .map(|receipt| ReceiptTimingObservation {
                    receipt_index: len_before,
                    seq: receipt.seq,
                    epoch: receipt.epoch,
                    started_offset_ns: waiting,
                    finished_offset_ns: finished,
                    acquire_wait_ns: wait,
                    receipt_push_ns: push,
                    measured_lock_hold_ns: hold,
                    receipt_len_before: len_before,
                    receipt_capacity_before: capacity_before,
                    receipt_len_after: len_after,
                    receipt_capacity_after: capacity_after,
                })
        } else {
            None
        };
        if let Some(timings) = &mut state.receipt_timings {
            timings.measured_lock_hold.record(hold);
            if let Some(observation) = slow {
                timings.retain_observation(observation);
            } else if is_slow {
                // The first-slow buffer is full and no stage maximum changed.
                // Count the omitted row without constructing an observation.
                timings.slow_observations_seen = timings.slow_observations_seen.saturating_add(1);
                timings.slow_observations_not_retained =
                    timings.slow_observations_not_retained.saturating_add(1);
            }
        }
    }

    pub fn push_gap(&self, event: GapEvent) {
        self.state
            .lock()
            .expect("event log poisoned")
            .gaps
            .push(event);
    }

    pub fn push_unsupported_notice(&self, event: UnsupportedNoticeEvent) {
        self.state
            .lock()
            .expect("event log poisoned")
            .unsupported_notices
            .push(event);
    }

    pub fn push_disconnect(&self, event: DisconnectEvent) {
        self.state
            .lock()
            .expect("event log poisoned")
            .disconnects
            .push(event);
    }

    pub fn push_churn(&self, event: ChurnEvent) {
        self.state
            .lock()
            .expect("event log poisoned")
            .churn
            .push(event);
    }

    pub fn push_join_failure(&self, failure: String) {
        self.state
            .lock()
            .expect("event log poisoned")
            .join_failures
            .push(failure);
    }

    /// Record a runner-observed fault (hook firing, send failure, malformed
    /// frame, wedged generator). Declared hooks and per-sender saturation
    /// are deduplicated: repeated kills/evictions are the same fault, and
    /// repeated saturation keeps the worst observed lag.
    pub fn push_fault(&self, reason: InvalidReason) {
        let mut state = self.state.lock().expect("event log poisoned");
        match &reason {
            InvalidReason::ServerTerminated => {
                if state.server_terminated {
                    return;
                }
                state.server_terminated = true;
            }
            InvalidReason::GeneratorSaturated {
                max_lag_us,
                bound_us,
            } => {
                for existing in &mut state.faults {
                    if let InvalidReason::GeneratorSaturated {
                        max_lag_us: recorded,
                        ..
                    } = existing
                    {
                        *recorded = (*recorded).max(*max_lag_us);
                        let _ = bound_us;
                        return;
                    }
                }
            }
            InvalidReason::SlowConsumerDisconnect { .. } => {
                if state.faults.iter().any(|existing| {
                    matches!(existing, InvalidReason::SlowConsumerDisconnect { .. })
                }) {
                    return;
                }
            }
            _ => {}
        }
        state.faults.push(reason);
    }

    /// Whether the server-termination fault has been declared (senders stop
    /// treating their own socket errors as independent faults).
    pub fn was_server_terminated(&self) -> bool {
        self.state
            .lock()
            .expect("event log poisoned")
            .server_terminated
    }

    /// Record the complete sender registry once, after every peer task has
    /// finished joining. The registry is run-level state, not an event: it
    /// exists so replay can resolve every rejoin snapshot identity exactly
    /// as this run did.
    pub fn set_registry(&self, registry: BTreeMap<String, (String, u32)>) {
        self.state.lock().expect("event log poisoned").registry = registry;
    }

    /// Observe test progress without transferring the run's final evidence.
    #[cfg(test)]
    pub(super) fn observation_counts(&self) -> (usize, usize) {
        let state = self.state.lock().expect("event log poisoned");
        (state.receipts.len(), state.faults.len())
    }

    /// Drain every recorded event after all run tasks finish. Sends sort by
    /// `(sender, seq)`; receipts, disconnects, churn actions, join failures,
    /// and faults retain arrival order. The oracle checks stream arrival order,
    /// so receipts must never be reordered.
    pub fn take_records(&self) -> RunRecords {
        let mut state = self.state.lock().expect("event log poisoned");
        let mut records = RunRecords {
            sent: std::mem::take(&mut state.sent),
            receipts: std::mem::take(&mut state.receipts),
            gaps: std::mem::take(&mut state.gaps),
            unsupported_notices: std::mem::take(&mut state.unsupported_notices),
            disconnects: std::mem::take(&mut state.disconnects),
            churn: std::mem::take(&mut state.churn),
            join_failures: std::mem::take(&mut state.join_failures),
            faults: std::mem::take(&mut state.faults),
            registry: std::mem::take(&mut state.registry),
        };
        records
            .sent
            .sort_by(|a, b| (&a.sender, a.seq).cmp(&(&b.sender, b.seq)));
        records
    }
}

/// The replayable event set of one run (the contents of
/// `deliveries.jsonl`).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RunRecords {
    pub sent: Vec<SentEvent>,
    pub receipts: Vec<ReceiptEvent>,
    pub gaps: Vec<GapEvent>,
    #[serde(default)]
    pub unsupported_notices: Vec<UnsupportedNoticeEvent>,
    pub disconnects: Vec<DisconnectEvent>,
    pub churn: Vec<ChurnEvent>,
    pub join_failures: Vec<String>,
    pub faults: Vec<InvalidReason>,
    /// The complete sender registry at end of run (`PlayerId` ->
    /// `(peer name, incarnation)`). Recorded as one `registry` line of
    /// `deliveries.jsonl` so replay resolves rejoin snapshot identities
    /// exactly.
    #[serde(default)]
    pub registry: BTreeMap<String, (String, u32)>,
}

/// One tagged line of `deliveries.jsonl`: the raw events of a run, in
/// recording-independent tagged form so a replay can rebuild
/// [`RunRecords`] exactly.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event_kind", rename_all = "snake_case")]
pub enum DeliveryEvent<Name = ReceiptName> {
    Sent(SentEvent<Name>),
    Receipt(ReceiptEvent<Name>),
    Gap(GapEvent),
    UnsupportedNotice(UnsupportedNoticeEvent),
    Disconnect(DisconnectEvent),
    Churn(ChurnEvent),
    JoinFailure {
        detail: String,
    },
    Fault(InvalidReason),
    /// The complete sender registry, recorded once. Not a run event — the
    /// identity table replay needs to resolve churn snapshot tails.
    Registry {
        senders: BTreeMap<String, (String, u32)>,
    },
}

impl RunRecords {
    /// Every event as a tagged JSONL line, in the canonical order
    /// (sends, receipts, gaps, unsupported notices, disconnects, churn,
    /// join failures, faults, the registry).
    pub fn events(&self) -> impl Iterator<Item = DeliveryEvent> + '_ {
        self.sent
            .iter()
            .cloned()
            .map(DeliveryEvent::Sent)
            .chain(self.receipts.iter().cloned().map(DeliveryEvent::Receipt))
            .chain(self.gaps.iter().cloned().map(DeliveryEvent::Gap))
            .chain(
                self.unsupported_notices
                    .iter()
                    .cloned()
                    .map(DeliveryEvent::UnsupportedNotice),
            )
            .chain(
                self.disconnects
                    .iter()
                    .cloned()
                    .map(DeliveryEvent::Disconnect),
            )
            .chain(self.churn.iter().cloned().map(DeliveryEvent::Churn))
            .chain(
                self.join_failures
                    .iter()
                    .cloned()
                    .map(|detail| DeliveryEvent::JoinFailure { detail }),
            )
            .chain(self.faults.iter().cloned().map(DeliveryEvent::Fault))
            .chain(std::iter::once(DeliveryEvent::Registry {
                senders: self.registry.clone(),
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planned_event_capacity_covers_each_room_without_changing_receipts() {
        let context = crate::unit_context();
        for count in [0, 1, 4] {
            for split_room in [false, true] {
                for timings in [false, true] {
                    let mut plans = context.plans[..count].to_vec();
                    if split_room {
                        if let Some(first) = plans.first_mut() {
                            first.room = 1;
                        }
                    }
                    let expected = crate::complete_records(&plans);
                    let log = EventLog::for_plans(&plans, timings).expect("planned log");
                    let initial = {
                        let state = log.state.lock().unwrap();
                        assert!(state.sent.capacity() >= expected.sent.len());
                        assert!(state.receipts.capacity() >= expected.receipts.len());
                        assert_eq!(state.receipt_timings.is_some(), timings);
                        (state.sent.capacity(), state.receipts.capacity())
                    };
                    for event in &expected.sent {
                        log.push_sent(event.clone());
                    }
                    for event in &expected.receipts {
                        log.push_receipt(event.clone());
                    }
                    {
                        let state = log.state.lock().unwrap();
                        assert_eq!((state.sent.capacity(), state.receipts.capacity()), initial);
                    }
                    assert_eq!(
                        serde_json::to_vec(&log.take_records()).unwrap(),
                        serde_json::to_vec(&expected).unwrap()
                    );
                }
            }
        }
    }

    #[test]
    fn planned_event_capacity_preserves_excess_events_and_faults() {
        let context = crate::unit_context();
        let mut expected = crate::complete_records(&context.plans);
        let log = EventLog::for_plans(&context.plans, false).expect("planned log");
        let mut extra = expected.receipts[0].clone();
        extra.recipient = "未知".into();
        expected.receipts.extend([extra.clone(), extra]);
        let mut extra = expected.sent[0].clone();
        extra.seq = u64::MAX;
        expected.sent.push(extra);
        expected
            .sent
            .sort_by(|a, b| (&a.sender, a.seq).cmp(&(&b.sender, b.seq)));
        expected.faults.push(InvalidReason::ServerTerminated);
        for event in &expected.sent {
            log.push_sent(event.clone());
        }
        for event in &expected.receipts {
            log.push_receipt(event.clone());
        }
        for fault in &expected.faults {
            log.push_fault(fault.clone());
        }
        let actual = log.take_records();
        assert_eq!(
            serde_json::to_vec(&actual).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
        let output = tempfile::tempdir().expect("excess event artifacts");
        crate::artifacts::write_deliveries(output.path(), &actual).expect("write excess events");
        let replayed = crate::artifacts::read_records(output.path()).expect("replay excess events");
        assert_eq!(
            serde_json::to_vec(&replayed).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
    }

    #[test]
    fn planned_event_capacity_overflow_returns_an_error() {
        for (sent, receipts, expected) in [
            (usize::MAX, 0, "planned sends"),
            (0, usize::MAX, "planned receipts"),
        ] {
            let error = EventLog::with_planned_capacity(sent, receipts, false)
                .err()
                .expect("impossible capacity must fail");
            assert!(error.contains(expected), "{error}");
        }
    }

    // #775: send retention must not allocate a name for every ledger row.
    #[test]
    fn send_records_share_live_and_replayed_identity_storage() {
        assert!(std::mem::size_of::<SentEvent>() <= 72);
        let context = crate::unit_context();
        let records = crate::complete_records(&context.plans);
        let output = tempfile::tempdir().expect("send artifacts");
        crate::artifacts::write_deliveries(output.path(), &records).unwrap();
        let replayed = crate::artifacts::read_records(output.path()).unwrap();
        assert_eq!(
            serde_json::to_vec(&records).unwrap(),
            serde_json::to_vec(&replayed).unwrap()
        );
        for collected in [&records, &replayed] {
            let first = &collected.sent[0];
            let next = collected
                .sent
                .iter()
                .skip(1)
                .find(|event| event.sender == first.sender)
                .unwrap();
            assert_eq!(first.sender.as_ptr(), next.sender.as_ptr());
        }
    }

    // #775: replay must share exact names without changing arbitrary evidence.
    #[test]
    fn replay_name_sharing_preserves_unknown_unicode_duplicates_and_run_lifetimes() {
        let log = EventLog::new();
        let known: ReceiptName = "r0p1".into();
        let unknown: ReceiptName = "未知\"\\\n".into();
        for (recipient, sender, seq) in [
            (known.clone(), unknown.clone(), 7),
            (known.clone(), unknown.clone(), 7),
            (unknown.clone(), known.clone(), 2),
        ] {
            log.push_receipt(ReceiptEvent {
                recipient,
                sender,
                seq,
                epoch: 3,
                server_seq: 9,
                received_us: 42,
                application_bytes: 96,
                encoded_frame_body_bytes: 250,
            });
        }
        for (sender, seq, epoch) in [
            (unknown.clone(), 7, 3),
            (unknown.clone(), 7, 99),
            (known.clone(), u64::MAX, 3),
        ] {
            log.push_sent(SentEvent {
                sender,
                room: 42,
                seq,
                epoch,
                intended_us: 2,
                sent_us: 5,
                application_bytes: 96,
                encoded_frame_body_bytes: 150,
                phase: Phase::Measured,
            });
        }
        let collected = log.take_records();
        let output = tempfile::tempdir().expect("artifacts");
        crate::artifacts::write_deliveries(output.path(), &collected).expect("write records");
        let first = crate::artifacts::read_records(output.path()).expect("read records");
        let second = crate::artifacts::read_records(output.path()).expect("read another run");
        for replayed in [&first, &second] {
            assert_eq!(
                serde_json::to_vec(&collected).unwrap(),
                serde_json::to_vec(replayed).unwrap()
            );
            assert_eq!(replayed.sent.len(), 3);
            let unknown_sends: Vec<_> = replayed
                .sent
                .iter()
                .filter(|sent| sent.sender == unknown)
                .collect();
            assert_eq!(unknown_sends.len(), 2);
            assert_eq!(unknown_sends[0].seq, unknown_sends[1].seq);
            assert_eq!((unknown_sends[0].epoch, unknown_sends[1].epoch), (3, 99));
            assert!(Arc::ptr_eq(
                &unknown_sends[0].sender.0,
                &unknown_sends[1].sender.0
            ));
            assert!(Arc::ptr_eq(
                &unknown_sends[0].sender.0,
                &replayed.receipts[0].sender.0
            ));
            assert_eq!(replayed.receipts.len(), 3);
            assert_eq!(replayed.receipts[0].sender.as_str(), "未知\"\\\n");
            assert_eq!(replayed.receipts[0].seq, replayed.receipts[1].seq);
            assert!(Arc::ptr_eq(
                &replayed.receipts[0].sender.0,
                &replayed.receipts[2].recipient.0
            ));
            assert!(Arc::ptr_eq(
                &replayed.receipts[0].recipient.0,
                &replayed.receipts[2].sender.0
            ));
        }
        assert!(!Arc::ptr_eq(
            &first.receipts[0].sender.0,
            &second.receipts[0].sender.0
        ));
        let weak = Arc::downgrade(&collected.receipts[0].sender.0);
        drop(unknown);
        drop(collected);
        assert!(
            weak.upgrade().is_none(),
            "the log must not retain a name pool"
        );
        assert!(std::mem::size_of::<ReceiptEvent>() < std::mem::size_of::<ReceiptEvent<String>>());
        let original: ReceiptEvent<String> =
            serde_json::from_value(serde_json::to_value(&first.receipts[0]).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_vec(&first.receipts[0]).unwrap(),
            serde_json::to_vec(&original).unwrap()
        );
    }

    fn timing_receipt(seq: u64) -> ReceiptEvent {
        ReceiptEvent {
            recipient: "r0p1".into(),
            sender: "r0p0".into(),
            seq,
            epoch: 1,
            server_seq: seq + 1,
            received_us: 10 + seq,
            application_bytes: 96,
            encoded_frame_body_bytes: 250,
        }
    }

    #[test]
    fn receipt_timings_measure_push_and_hold_stages_separately() {
        let mut log = EventLog::with_receipt_timings(true);
        log.receipt_timing_hooks.push_delay = Duration::from_millis(15);
        log.receipt_timing_hooks.bookkeeping_delay = Duration::from_millis(20);
        log.push_receipt(timing_receipt(7));
        let timings = log.take_receipt_timings().expect("enabled timings");
        for stage in [
            &timings.acquire_wait,
            &timings.receipt_push,
            &timings.measured_lock_hold,
        ] {
            assert_eq!(stage.count, 1);
            assert_eq!(stage.max_ns, stage.total_ns);
            assert!(!stage.overflowed);
        }
        assert!(timings.receipt_push.total_ns >= 15_000_000);
        assert!(timings.measured_lock_hold.total_ns >= timings.receipt_push.total_ns + 20_000_000);
        let row = &timings.slow_observations[0];
        assert_eq!((row.seq, row.epoch), (7, 1));
        assert_eq!((row.receipt_len_before, row.receipt_len_after), (0, 1));
        assert_eq!(row.receipt_capacity_before, 0);
        assert!(row.receipt_capacity_after >= 1);
        assert_eq!(
            row.finished_offset_ns - row.started_offset_ns,
            row.acquire_wait_ns + row.measured_lock_hold_ns
        );
        assert!(timings.wall_anchor_offset_after_ns >= timings.wall_anchor_offset_before_ns);
    }

    #[test]
    fn receipt_timings_separate_real_mutex_contention_from_lock_hold() {
        let mut log = EventLog::with_receipt_timings(true);
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        log.receipt_timing_hooks.wait_started = Some(entered);
        let log = Arc::new(log);
        let guard = log.state.lock().expect("hold event log");
        let (done, finished) = std::sync::mpsc::sync_channel(1);
        let producer = {
            let log = log.clone();
            std::thread::spawn(move || {
                log.push_receipt(timing_receipt(0));
                done.send(()).expect("producer completion");
            })
        };
        if let Err(error) = waiting.recv_timeout(Duration::from_secs(5)) {
            drop(guard);
            panic!("producer did not begin acquisition: {error}");
        }
        std::thread::sleep(Duration::from_millis(20));
        drop(guard);
        finished
            .recv_timeout(Duration::from_secs(5))
            .expect("producer completed after unlock");
        producer.join().expect("receipt producer");
        let timings = log.take_receipt_timings().expect("enabled timings");
        assert!(timings.acquire_wait.total_ns >= 20_000_000);
        let row = &timings.slow_observations[0];
        assert_eq!(row.acquire_wait_ns, timings.acquire_wait.total_ns);
        assert_eq!(
            row.measured_lock_hold_ns,
            timings.measured_lock_hold.total_ns
        );
    }

    #[test]
    fn receipt_timings_bound_slow_rows_but_aggregate_every_receipt() {
        let mut log = EventLog::with_receipt_timings(true);
        log.receipt_timing_hooks.bookkeeping_delay = Duration::from_millis(2);
        for seq in 0..80 {
            log.push_receipt(timing_receipt(seq));
        }
        let timings = log.take_receipt_timings().expect("enabled timings");
        assert_eq!(timings.slow_observations.len(), MAX_SLOW_RECEIPT_TIMINGS);
        assert_eq!(
            timings.slow_observations.capacity(),
            MAX_SLOW_RECEIPT_TIMINGS
        );
        assert_eq!(timings.slow_observations_seen, 80);
        assert_eq!(timings.slow_observations_not_retained, 16);
        assert_eq!(timings.measured_lock_hold.count, 80);
        assert!(timings.measured_lock_hold.max_ns >= 2_000_000);
        assert_eq!(timings.slow_observations[0].seq, 0);
        assert_eq!(timings.slow_observations[63].seq, 63);
        assert_eq!(log.take_records().receipts.len(), 80);
    }

    #[test]
    fn disabled_receipt_timings_keep_exact_records_and_allocate_no_observation_buffer() {
        let ordinary = EventLog::new();
        let disabled = EventLog::with_receipt_timings(false);
        let enabled = EventLog::with_receipt_timings(true);
        for seq in 0..5 {
            for log in [&ordinary, &disabled, &enabled] {
                log.push_receipt(timing_receipt(seq));
            }
        }
        assert!(disabled.receipt_timing_start.is_none());
        assert!(disabled.take_receipt_timings().is_none());
        assert!(ordinary.take_receipt_timings().is_none());
        let expected = serde_json::to_vec(&ordinary.take_records()).expect("ordinary records");
        assert_eq!(
            serde_json::to_vec(&disabled.take_records()).expect("disabled records"),
            expected
        );
        assert_eq!(
            serde_json::to_vec(&enabled.take_records()).expect("enabled records"),
            expected
        );
        assert!(enabled.take_receipt_timings().is_some());
    }

    #[test]
    fn receipt_timing_aggregates_report_overflow_instead_of_wrapping() {
        let mut aggregate = TimingAggregate::default();
        aggregate.record(u64::MAX);
        aggregate.record(1);
        assert_eq!(aggregate.total_ns, u64::MAX);
        assert_eq!(aggregate.max_ns, u64::MAX);
        assert_eq!(aggregate.count, 2);
        assert!(aggregate.overflowed);
    }

    #[test]
    fn receipt_timings_keep_late_stage_maxima_after_slow_buffer_fills() {
        let mut timings = ReceiptTimingSnapshot::new(Instant::now());
        let make = |seq: u64, duration: u64| ReceiptTimingObservation {
            receipt_index: usize::try_from(seq).expect("small fixture"),
            seq,
            epoch: 1,
            started_offset_ns: seq * 10_000_000,
            finished_offset_ns: seq * 10_000_000 + duration * 2,
            acquire_wait_ns: duration,
            receipt_push_ns: duration,
            measured_lock_hold_ns: duration,
            receipt_len_before: usize::try_from(seq).expect("small fixture"),
            receipt_capacity_before: 128,
            receipt_len_after: usize::try_from(seq).expect("small fixture") + 1,
            receipt_capacity_after: 128,
        };
        for seq in 0..64 {
            timings.retain_observation(make(seq, SLOW_RECEIPT_NS));
        }
        timings.retain_observation(make(99, SLOW_RECEIPT_NS * 10));
        assert_eq!(timings.slow_observations.len(), 64);
        assert_eq!(timings.slow_observations[63].seq, 63);
        assert_eq!(timings.slow_observations_not_retained, 1);
        for worst in [
            &timings.worst_acquire_wait,
            &timings.worst_receipt_push,
            &timings.worst_measured_lock_hold,
        ] {
            let row = worst.as_ref().expect("late maximum");
            assert_eq!(row.seq, 99);
            assert_eq!(row.receipt_capacity_after, 128);
        }
    }
    #[test]
    fn first_accepted_lag_prefix_freezes_counters_context_and_stage_maxima() {
        let log = EventLog::with_receipt_timings(true);
        log.push_receipt(timing_receipt(7));
        log.capture_receipt_timings_at_send_lag("r0p3", 42, "before_write", 400_000, 250_000);
        let frozen = {
            let state = log.state.lock().expect("timings");
            let prefix = state
                .receipt_timings
                .as_ref()
                .unwrap()
                .first_accepted_lag_prefix
                .as_ref()
                .unwrap();
            assert_eq!(
                (&*prefix.sender, prefix.seq, prefix.stage),
                ("r0p3", 42, "before_write")
            );
            assert_eq!((prefix.observed_us, prefix.bound_us), (400_000, 250_000));
            assert!(prefix.capture_offset_ns >= prefix.request_offset_ns);
            assert_eq!(prefix.receipt_push.count, 1);
            assert_eq!(prefix.worst_receipt_push.as_ref().unwrap().seq, 7);
            serde_json::to_value(prefix).expect("frozen prefix")
        };
        log.push_receipt(timing_receipt(8));
        {
            let mut state = log.state.lock().expect("update later maximum");
            let timings = state.receipt_timings.as_mut().unwrap();
            let mut later = timings.worst_receipt_push.as_ref().unwrap().clone();
            later.seq = 99;
            later.receipt_push_ns = u64::MAX;
            timings.retain_observation(later);
            assert_eq!(timings.receipt_push.count, 2);
            assert_eq!(timings.worst_receipt_push.as_ref().unwrap().seq, 99);
        }
        for seq in 43..143 {
            log.capture_receipt_timings_at_send_lag(
                "r0p4",
                seq,
                "completed_write",
                900_000,
                250_000,
            );
        }
        let timings = log.take_receipt_timings().unwrap();
        assert_eq!(
            serde_json::to_value(timings.first_accepted_lag_prefix).unwrap(),
            frozen
        );
        assert_eq!(timings.receipt_push.count, 2);
        assert_eq!(log.take_records().receipts.len(), 2);
    }

    #[test]
    fn disabled_lag_prefix_capture_preserves_records_without_acquiring_the_log() {
        let log = Arc::new(EventLog::with_receipt_timings(false));
        log.push_receipt(timing_receipt(7));
        let guard = log.state.lock().expect("hold disabled log");
        let (done, finished) = std::sync::mpsc::sync_channel(1);
        let producer = {
            let log = Arc::clone(&log);
            std::thread::spawn(move || {
                log.capture_receipt_timings_at_send_lag(
                    "r0p3",
                    42,
                    "before_write",
                    400_000,
                    250_000,
                );
                done.send(()).expect("disabled capture completion");
            })
        };
        let completed = finished.recv_timeout(Duration::from_secs(5));
        drop(guard);
        completed.expect("disabled capture must not acquire the held mutex");
        producer.join().expect("disabled capture producer");
        assert!(log.take_receipt_timings().is_none());
        let records = log.take_records();
        assert_eq!(records.receipts.len(), 1);
        assert_eq!(records.receipts[0].seq, 7);
    }

    #[test]
    fn timing_evidence_resolves_exact_names_and_rejects_invalid_receipt_positions() {
        let log = EventLog::with_receipt_timings(true);
        log.push_receipt(ReceiptEvent {
            recipient: "未知\"\\\n".into(),
            sender: "outside-the-roster".into(),
            ..timing_receipt(7)
        });
        log.capture_receipt_timings_at_send_lag("r0p3", 42, "before_write", 400_000, 250_000);
        let records = log.take_records();
        let timings = log.take_receipt_timings().unwrap();
        let evidence = timings
            .into_evidence(&records)
            .expect("resolve timing names");
        for row in [
            &evidence["worst_receipt_push"],
            &evidence["first_accepted_lag_prefix"]["worst_receipt_push"],
        ] {
            assert_eq!(row["receipt_index"], 0);
            assert_eq!(row["recipient"], "未知\"\\\n");
            assert_eq!(row["sender"], "outside-the-roster");
            assert_eq!(row["seq"], 7);
        }
        let invalid = EventLog::with_receipt_timings(true);
        invalid.push_receipt(timing_receipt(7));
        let error = invalid
            .take_receipt_timings()
            .unwrap()
            .into_evidence(&RunRecords::default())
            .expect_err("missing receipt must fail");
        assert!(error.contains("index 0 exceeds 0 receipts"), "{error}");
    }
}
