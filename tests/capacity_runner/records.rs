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

/// Replay-only send, receipt, and gap names. Live tasks clone their peer and registry names
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

    pub(super) fn share_gap(&mut self, gap: GapEvent<impl AsRef<str>>) -> GapEvent {
        GapEvent {
            recipient: self.intern(gap.recipient.as_ref()),
            sender: self.intern(gap.sender.as_ref()),
            epoch: gap.epoch,
            from_seq: gap.from_seq,
            to_seq: gap.to_seq,
            reason: gap.reason,
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
pub struct GapEvent<Name = ReceiptName> {
    pub recipient: Name,
    /// Peer name resolved from the report's `from_player` id.
    pub sender: Name,
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
            notes: "Opt-in diagnostics add clock reads and bookkeeping. All stage durations are elapsed wall-clock time and include descheduling. Acquire wait does not distinguish mutex contention from scheduling delay. Offsets start at EventLog construction; workload_epoch_offset_ns maps the armed workload epoch when provided. The wall anchor lies within its before/after monotonic offset bracket. Measured lock hold ends before its final counter update, slow-observation finalization, and unlock. Retains the first 64 slow observations plus one worst observation per measured stage; aggregates cover every retained receipt. At most 70 observation rows are retained across the slow buffer, three final maxima, and three frozen prefix maxima. Observation rows retain scalar receipt positions; exact names are resolved only after collection. No TCP attribution.",
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
    budget_changed: tokio::sync::Notify,
    #[cfg(test)]
    receipt_timing_hooks: ReceiptTimingHooks,
}

#[derive(Default)]
struct EventState {
    receipt_timings: Option<ReceiptTimingSnapshot>,
    budget: Option<RecordBudget>,
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

/// Receive-side events can exceed the offered schedule. Keep one terminal
/// event beyond these limits so the cause remains exact, then stop the run.
/// Sends have their own finite schedule and are never discarded by this gate.
struct RecordBudget {
    event_limit: usize,
    control_byte_limit: usize,
    retained_events: usize,
    control_bytes: usize,
    first_excess_kind: Option<&'static str>,
    unretained_events: u64,
    allocation_failure: Option<String>,
}

const CONTROL_BYTE_LIMIT: usize = 16 * 1024 * 1024;
const EXTRA_EVENTS: usize = 1024;
const EXTRA_EVENTS_PER_PEER: usize = 128;

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
        let mut log = Self::with_planned_capacity(sent, receipts, receipt_timings)?;
        let allowance = plans
            .len()
            .checked_mul(EXTRA_EVENTS_PER_PEER)
            .and_then(|count| count.checked_add(EXTRA_EVENTS))
            .and_then(|count| count.checked_add(receipts))
            .ok_or_else(|| "retained event budget overflows".to_string())?;
        log.state.get_mut().expect("new event log").budget = Some(RecordBudget {
            event_limit: allowance,
            control_byte_limit: CONTROL_BYTE_LIMIT,
            retained_events: 0,
            control_bytes: 0,
            first_excess_kind: None,
            unretained_events: 0,
            allocation_failure: None,
        });
        Ok(log)
    }

    fn with_planned_capacity(
        sent: usize,
        receipts: usize,
        receipt_timings: bool,
    ) -> Result<Self, String> {
        let mut log = Self::with_receipt_timings(receipt_timings);
        let state = log.state.get_mut().expect("new event log");
        // Receive allocation failures must still have room for their terminal reason.
        state
            .faults
            .try_reserve_exact(1)
            .map_err(|error| format!("reserve terminal generator fault: {error}"))?;
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

    /// The first crossing event is retained. Later inbound events are counted
    /// as unretained evidence while the runner cancels and joins its producers.
    fn admit_event(
        &self,
        state: &mut EventState,
        kind: &'static str,
        control_bytes: usize,
    ) -> bool {
        let Some(budget) = &mut state.budget else {
            return true;
        };
        if budget.first_excess_kind.is_some() {
            budget.unretained_events = budget.unretained_events.saturating_add(1);
            return false;
        }
        budget.retained_events = budget.retained_events.saturating_add(1);
        budget.control_bytes = budget.control_bytes.saturating_add(control_bytes);
        if budget.retained_events > budget.event_limit
            || budget.control_bytes > budget.control_byte_limit
        {
            budget.first_excess_kind = Some(kind);
            self.budget_changed.notify_waiters();
        }
        true
    }

    fn retain_event<T>(
        &self,
        state: &mut EventState,
        kind: &'static str,
        control_bytes: usize,
        event: T,
        select: fn(&mut EventState) -> &mut Vec<T>,
    ) -> bool {
        if !self.admit_event(state, kind, control_bytes) {
            return false;
        }
        if let Some(budget) = &state.budget {
            // Keep a spare fault slot for the terminal budget reason, which
            // take_records appends after the producers have stopped.
            let terminal_slots = usize::from(kind == "fault");
            let capacity_limit = budget.event_limit.saturating_add(1 + terminal_slots);
            let rows = select(state);
            if rows.len().saturating_add(terminal_slots) >= rows.capacity() {
                let additional = rows
                    .capacity()
                    .max(4)
                    .min(capacity_limit.saturating_sub(rows.len()));
                if let Err(error) = rows.try_reserve_exact(additional) {
                    let budget = state.budget.as_mut().expect("bounded event log");
                    budget.first_excess_kind = Some(kind);
                    budget.retained_events -= 1;
                    budget.control_bytes = budget.control_bytes.saturating_sub(control_bytes);
                    budget.unretained_events = budget.unretained_events.saturating_add(1);
                    budget.allocation_failure = Some(error.to_string());
                    self.budget_changed.notify_waiters();
                    return false;
                }
            }
        }
        select(state).push(event);
        true
    }

    pub fn record_budget_exhausted(&self) -> bool {
        self.state
            .lock()
            .expect("event log poisoned")
            .budget
            .as_ref()
            .is_some_and(|budget| budget.first_excess_kind.is_some())
    }

    pub async fn wait_record_budget_exhausted(&self) {
        loop {
            let changed = self.budget_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.record_budget_exhausted() {
                return;
            }
            changed.await;
        }
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

    /// Record a receipt and report whether the budget remains active at this admission.
    pub fn push_receipt(&self, event: ReceiptEvent) -> bool {
        let Some(start) = self.receipt_timing_start else {
            let mut state = self.state.lock().expect("event log poisoned");
            self.retain_event(&mut state, "receipt", 0, event, |state| &mut state.receipts);
            return state
                .budget
                .as_ref()
                .is_none_or(|budget| budget.first_excess_kind.is_none());
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
        if !self.retain_event(&mut state, "receipt", 0, event, |state| &mut state.receipts) {
            return false;
        }
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
        state
            .budget
            .as_ref()
            .is_none_or(|budget| budget.first_excess_kind.is_none())
    }

    pub fn push_gap(&self, event: GapEvent) {
        let bytes = 0;
        let mut state = self.state.lock().expect("event log poisoned");
        self.retain_event(&mut state, "gap", bytes, event, |state| &mut state.gaps);
    }

    pub fn push_unsupported_notice(&self, event: UnsupportedNoticeEvent) {
        let bytes = crate::runner::json_byte_count(&event).unwrap_or(usize::MAX);
        let mut state = self.state.lock().expect("event log poisoned");
        self.retain_event(&mut state, "unsupported_notice", bytes, event, |state| {
            &mut state.unsupported_notices
        });
    }

    pub fn push_disconnect(&self, event: DisconnectEvent) {
        let bytes = crate::runner::json_byte_count(&event).unwrap_or(usize::MAX);
        let mut state = self.state.lock().expect("event log poisoned");
        self.retain_event(&mut state, "disconnect", bytes, event, |state| {
            &mut state.disconnects
        });
    }

    pub fn push_churn(&self, event: ChurnEvent) {
        let bytes = crate::runner::json_byte_count(&event).unwrap_or(usize::MAX);
        let mut state = self.state.lock().expect("event log poisoned");
        self.retain_event(&mut state, "churn", bytes, event, |state| &mut state.churn);
    }

    pub fn push_join_failure(&self, failure: String) {
        let bytes = crate::runner::json_byte_count(&failure).unwrap_or(usize::MAX);
        let mut state = self.state.lock().expect("event log poisoned");
        self.retain_event(&mut state, "join_failure", bytes, failure, |state| {
            &mut state.join_failures
        });
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
        let bytes = crate::runner::json_byte_count(&reason).unwrap_or(usize::MAX);
        self.retain_event(&mut state, "fault", bytes, reason, |state| {
            &mut state.faults
        });
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
        if let Some(budget) = state.budget.as_ref() {
            if let Some(kind) = budget.first_excess_kind {
                records
                    .faults
                    .push(InvalidReason::GeneratorRecordBudgetExceeded {
                        event_limit: crate::config::count_u64(budget.event_limit),
                        retained_events: crate::config::count_u64(budget.retained_events),
                        control_byte_limit: crate::config::count_u64(budget.control_byte_limit),
                        retained_control_bytes: crate::config::count_u64(budget.control_bytes),
                        first_excess_kind: kind.to_string(),
                        unretained_events: budget.unretained_events,
                        allocation_failure: budget.allocation_failure.clone(),
                    });
            }
        }
        drop(state);
        release_unused_capacity(&mut records.sent);
        release_unused_capacity(&mut records.receipts);
        release_unused_capacity(&mut records.gaps);
        release_unused_capacity(&mut records.unsupported_notices);
        release_unused_capacity(&mut records.disconnects);
        release_unused_capacity(&mut records.churn);
        release_unused_capacity(&mut records.join_failures);
        release_unused_capacity(&mut records.faults);
        sort_sent_events(&mut records.sent);
        records
    }
}

/// Free unused storage without risking the collected evidence on allocation failure.
fn release_unused_capacity<T>(rows: &mut Vec<T>) {
    release_unused_capacity_with_reservation(rows, Vec::try_reserve_exact);
}

/// Move records only after a successful, sufficient replacement reservation.
fn release_unused_capacity_with_reservation<T>(
    rows: &mut Vec<T>,
    reserve: impl FnOnce(&mut Vec<T>, usize) -> Result<(), std::collections::TryReserveError>,
) {
    if rows.is_empty() {
        *rows = Vec::new();
    } else if rows.len() < rows.capacity() {
        let mut compacted = Vec::new();
        if reserve(&mut compacted, rows.len()).is_ok()
            && compacted.is_empty()
            && (rows.len()..rows.capacity()).contains(&compacted.capacity())
        {
            compacted.append(rows);
            *rows = compacted;
        }
    }
}

fn sort_sent_events(events: &mut [SentEvent]) {
    if events.is_sorted_by_key(|event| (event.sender.as_str(), event.seq)) {
        return;
    }
    if events.len() <= 32 || !compact_send_positions_fit(events.len()) {
        events.sort_by(|a, b| (&a.sender, a.seq).cmp(&(&b.sender, b.seq)));
        return;
    }
    let mut source_positions = send_sort_positions(events);
    for start in 0..source_positions.len() {
        let mut destination = start;
        loop {
            let source = source_positions[destination] as usize;
            source_positions[destination] =
                u32::try_from(destination).expect("validated send count");
            if source == start {
                break;
            }
            events.swap(destination, source);
            destination = source;
        }
    }
}

fn send_sort_positions(events: &[SentEvent]) -> Vec<u32> {
    let maximum_grouped_senders = (events.len() / 16).max(16);
    let mut sender_ranges: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for event in events {
        sender_ranges.entry(event.sender.as_str()).or_default().1 += 1;
        if sender_ranges.len() > maximum_grouped_senders {
            drop(sender_ranges);
            let mut positions: Vec<u32> =
                (0..u32::try_from(events.len()).expect("validated send count")).collect();
            positions.sort_unstable_by_key(|&position| {
                let event = &events[position as usize];
                (event.sender.as_str(), event.seq, position)
            });
            return positions;
        }
    }
    let mut offset = 0;
    for (next, count) in sender_ranges.values_mut() {
        *next = offset;
        offset += *count;
    }
    grouped_send_positions(events, &mut sender_ranges)
}

fn compact_send_positions_fit(count: usize) -> bool {
    u32::try_from(count).is_ok()
}

fn grouped_send_positions(
    events: &[SentEvent],
    sender_ranges: &mut BTreeMap<&str, (usize, usize)>,
) -> Vec<u32> {
    let mut source_positions = vec![0_u32; events.len()];
    for (position, event) in events.iter().enumerate() {
        let (next, _) = sender_ranges
            .get_mut(event.sender.as_str())
            .expect("every sender was counted");
        source_positions[*next] = u32::try_from(position).expect("validated send count");
        *next += 1;
    }
    for (end, count) in sender_ranges.values() {
        let positions = &mut source_positions[end - count..*end];
        if !positions.is_sorted_by_key(|&position| events[position as usize].seq) {
            positions.sort_unstable_by_key(|&position| (events[position as usize].seq, position));
        }
    }
    source_positions
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
    Gap(GapEvent<Name>),
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

/// A tagged artifact row borrowed from the retained event log.
/// Keep the owned form above for replay; writing must not clone the registry
/// or increment shared-name reference counts for every delivery.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "event_kind", rename_all = "snake_case")]
pub enum DeliveryEventRef<'a> {
    Sent(&'a SentEvent),
    Receipt(&'a ReceiptEvent),
    Gap(&'a GapEvent),
    UnsupportedNotice(&'a UnsupportedNoticeEvent),
    Disconnect(&'a DisconnectEvent),
    Churn(&'a ChurnEvent),
    JoinFailure {
        detail: &'a str,
    },
    Fault(&'a InvalidReason),
    Registry {
        senders: &'a BTreeMap<String, (String, u32)>,
    },
}

impl RunRecords {
    /// Every event as a borrowed tagged JSONL line, in the canonical order
    /// (sends, receipts, gaps, unsupported notices, disconnects, churn,
    /// join failures, faults, the registry).
    pub fn events(&self) -> impl Iterator<Item = DeliveryEventRef<'_>> {
        self.sent
            .iter()
            .map(DeliveryEventRef::Sent)
            .chain(self.receipts.iter().map(DeliveryEventRef::Receipt))
            .chain(self.gaps.iter().map(DeliveryEventRef::Gap))
            .chain(
                self.unsupported_notices
                    .iter()
                    .map(DeliveryEventRef::UnsupportedNotice),
            )
            .chain(self.disconnects.iter().map(DeliveryEventRef::Disconnect))
            .chain(self.churn.iter().map(DeliveryEventRef::Churn))
            .chain(
                self.join_failures
                    .iter()
                    .map(|detail| DeliveryEventRef::JoinFailure { detail }),
            )
            .chain(self.faults.iter().map(DeliveryEventRef::Fault))
            .chain(std::iter::once(DeliveryEventRef::Registry {
                senders: &self.registry,
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "isolated artifact serialization profile; run explicitly"]
    fn generator_artifact_serialization_profile() {
        let context = crate::unit_context();
        let template = crate::complete_records(&context.plans);
        let mut records = RunRecords::default();
        let count = std::env::var("CAPACITY_ARTIFACT_PROBE_RECEIPTS")
            .unwrap_or_else(|_| "500000".into())
            .parse::<usize>()
            .unwrap();
        records.receipts = vec![template.receipts[0].clone(); count];
        records.registry = (0..15984)
            .map(|peer| {
                (
                    format!("00000000-0000-0000-0000-{peer:012}"),
                    (format!("r{}p{}", peer / 16, peer % 16), 1),
                )
            })
            .collect();
        let source_rss = crate::diagnostics::resident_memory_bytes(std::process::id());
        let started = Instant::now();
        let mut events = records.events();
        for _ in 0..count {
            std::hint::black_box(events.next().unwrap());
        }
        let registry = events.next().unwrap();
        let iteration_us = started.elapsed().as_micros();
        let registry_rss = crate::diagnostics::resident_memory_bytes(std::process::id());
        std::hint::black_box(registry);
        let output = tempfile::tempdir().unwrap();
        let started = Instant::now();
        crate::artifacts::write_deliveries(output.path(), &records).unwrap();
        println!(
            "ARTIFACT_SERIALIZATION_PROFILE {}",
            serde_json::json!({
                "receipts": count, "registry": records.registry.len(), "source_rss": source_rss,
                "registry_rss": registry_rss, "iteration_us": iteration_us,
                "write_us": started.elapsed().as_micros(),
                "bytes": std::fs::metadata(output.path().join(crate::artifacts::DELIVERIES_FILE)).unwrap().len()
            })
        );
    }

    #[test]
    fn compact_send_positions_admit_only_representable_counts() {
        for count in [0, 1, 32, u32::MAX as usize] {
            assert!(compact_send_positions_fit(count), "count={count}");
        }
        if let Some(too_many) = (u32::MAX as usize).checked_add(1) {
            assert!(!compact_send_positions_fit(too_many));
            assert!(!compact_send_positions_fit(usize::MAX));
        }
    }

    #[test]
    fn grouped_send_sort_positions_use_four_bytes_per_send() {
        let context = crate::unit_context();
        let base = crate::complete_records(&context.plans).sent[0].clone();
        let mut events = Vec::new();
        for position in 0..96_u64 {
            let mut event = base.clone();
            event.sender = if position % 2 == 0 {
                "sender-a"
            } else {
                "sender-b"
            }
            .into();
            event.seq = 96 - position;
            event.sent_us = position;
            events.push(event);
        }
        let mut ranges = BTreeMap::from([("sender-a", (0, 48)), ("sender-b", (48, 48))]);
        let positions = grouped_send_positions(&events, &mut ranges);
        assert_eq!(positions.len(), events.len());
        assert!(
            positions.capacity() * std::mem::size_of_val(&positions[0]) <= events.len() * 4,
            "send sorting must retain at most four position bytes per send"
        );
        let mut expected: Vec<_> = (0..events.len()).collect();
        expected.sort_by_key(|&position| (events[position].sender.as_str(), events[position].seq));
        assert_eq!(
            positions
                .iter()
                .map(|&position| position as usize)
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn compact_send_sort_matches_stable_sort_with_duplicate_keys() {
        let context = crate::unit_context();
        let base = crate::complete_records(&context.plans).sent[0].clone();
        for encoded in 0_u32..4096 {
            let mut events = Vec::new();
            for repetition in 0..6 {
                let mut digits = encoded;
                for position in 0..6 {
                    let digit = digits % 4;
                    digits /= 4;
                    let mut event = base.clone();
                    event.sender = if digit < 2 { "other" } else { "未知" }.into();
                    event.seq = if digit % 2 == 0 { 0 } else { u64::MAX };
                    event.sent_us = repetition * 6 + position;
                    events.push(event);
                }
            }
            let mut expected = events.clone();
            expected.sort_by(|a, b| (&a.sender, a.seq).cmp(&(&b.sender, b.seq)));
            sort_sent_events(&mut events);
            assert_eq!(
                serde_json::to_value(&events).unwrap(),
                serde_json::to_value(&expected).unwrap(),
                "input {encoded}"
            );
            sort_sent_events(&mut events);
            assert_eq!(
                serde_json::to_value(&events).unwrap(),
                serde_json::to_value(&expected).unwrap(),
                "already sorted input {encoded}"
            );
        }
        for count in [0, 1, 2, 16, 32] {
            let mut events: Vec<_> = (0..count)
                .rev()
                .map(|seq| {
                    let mut event = base.clone();
                    event.seq = seq;
                    event
                })
                .collect();
            sort_sent_events(&mut events);
            assert_eq!(
                events.iter().map(|event| event.seq).collect::<Vec<_>>(),
                (0..count).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn sparse_sender_sort_positions_keep_exact_stable_identity_in_four_bytes() {
        let context = crate::unit_context();
        let base = crate::complete_records(&context.plans).sent[0].clone();
        for sender_count in [17, 64, 1024] {
            for repeats in [1, 2, 3] {
                let mut events = Vec::new();
                for repetition in 0..repeats {
                    for sender in (0..sender_count).rev() {
                        let mut event = base.clone();
                        event.sender = format!("未知-{sender:04}").into();
                        event.seq = if sender % 2 == 0 { 0 } else { u64::MAX };
                        event.sent_us = repetition;
                        events.push(event);
                    }
                }
                let positions = send_sort_positions(&events);
                assert_eq!(positions.len(), events.len());
                assert!(positions.capacity() * std::mem::size_of::<u32>() <= events.len() * 4);
                let mut expected: Vec<_> = (0..events.len()).collect();
                expected.sort_by_key(|&position| {
                    (events[position].sender.as_str(), events[position].seq)
                });
                assert_eq!(
                    positions
                        .iter()
                        .map(|&position| position as usize)
                        .collect::<Vec<_>>(),
                    expected
                );
                sort_sent_events(&mut events);
                let output = tempfile::tempdir().expect("sparse sender artifacts");
                let records = RunRecords {
                    sent: events,
                    ..RunRecords::default()
                };
                crate::artifacts::write_deliveries(output.path(), &records).unwrap();
                let replayed = crate::artifacts::read_records(output.path()).unwrap();
                assert_eq!(
                    serde_json::to_value(&records).unwrap(),
                    serde_json::to_value(&replayed).unwrap()
                );
            }
        }
    }

    #[test]
    fn sparse_sender_sort_matches_stable_order_with_single_and_repeated_keys() {
        let context = crate::unit_context();
        let base = crate::complete_records(&context.plans).sent[0].clone();
        for sender_count in [17, 64, 1024] {
            for repetitions in [1, 2] {
                let mut events = Vec::new();
                for sender in (0..sender_count).rev() {
                    for duplicate in 0..repetitions {
                        let mut event = base.clone();
                        event.sender = format!("未知-{sender:04}").into();
                        event.seq = u64::MAX;
                        event.sent_us = duplicate;
                        events.push(event);
                    }
                }
                let mut expected = events.clone();
                expected.sort_by(|a, b| (&a.sender, a.seq).cmp(&(&b.sender, b.seq)));
                sort_sent_events(&mut events);
                assert_eq!(
                    serde_json::to_value(&events).unwrap(),
                    serde_json::to_value(&expected).unwrap(),
                    "senders={sender_count}, repetitions={repetitions}"
                );
            }
        }
    }

    #[test]
    fn compact_send_collection_preserves_duplicate_evidence_and_replay() {
        let context = crate::unit_context();
        let mut expected = crate::complete_records(&context.plans);
        let mut duplicate = expected.sent[0].clone();
        duplicate.sent_us = u64::MAX;
        expected.sent.push(duplicate);
        expected.sent.reverse();
        let log = EventLog::new();
        for event in &expected.sent {
            log.push_sent(event.clone());
        }
        for event in &expected.receipts {
            log.push_receipt(event.clone());
        }
        expected
            .sent
            .sort_by(|a, b| (&a.sender, a.seq).cmp(&(&b.sender, b.seq)));
        let actual = log.take_records();
        assert_eq!(
            serde_json::to_value(&actual).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        let output = tempfile::tempdir().expect("send order artifacts");
        crate::artifacts::write_deliveries(output.path(), &actual).unwrap();
        let replayed = crate::artifacts::read_records(output.path()).unwrap();
        assert_eq!(
            serde_json::to_value(&replayed).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        assert!(log.take_records().sent.is_empty());
    }

    #[test]
    #[ignore = "isolated send sorting memory profile; run in a fresh process"]
    fn generator_send_sort_memory_profile() {
        let count = std::env::var("CAPACITY_SORT_PROBE_SENDS")
            .unwrap_or_else(|_| "1000000".into())
            .parse::<usize>()
            .expect("send count");
        assert!((1..=4000000).contains(&count));
        let compact = std::env::var("CAPACITY_SORT_PROBE_COMPACT").as_deref() == Ok("1");
        let context = crate::unit_context();
        let base = crate::complete_records(&context.plans).sent[0].clone();
        let sender_count = std::env::var("CAPACITY_SORT_PROBE_SENDERS")
            .unwrap_or_else(|_| "256".into())
            .parse::<usize>()
            .expect("sender count");
        assert!((1..=count).contains(&sender_count));
        let names: Vec<ReceiptName> = (0..sender_count)
            .map(|index| format!("peer-{index:03}").into())
            .collect();
        let mut events = Vec::with_capacity(count);
        for position in 0..count {
            let mut event = base.clone();
            event.sender = names[position % names.len()].clone();
            event.seq = u64::try_from(position / names.len()).unwrap();
            event.sent_us = u64::try_from(position).unwrap();
            events.push(event);
        }
        let collected_rss = crate::diagnostics::resident_memory_bytes(std::process::id());
        let started = Instant::now();
        if compact {
            sort_sent_events(&mut events);
        } else {
            events.sort_by(|a, b| (&a.sender, a.seq).cmp(&(&b.sender, b.seq)));
        }
        let elapsed_us = started.elapsed().as_micros();
        assert!(events.is_sorted_by_key(|event| (event.sender.as_str(), event.seq)));
        for event in &events {
            let original = usize::try_from(event.sent_us).unwrap();
            assert_eq!(event.sender, names[original % names.len()]);
            assert_eq!(event.seq, u64::try_from(original / names.len()).unwrap());
        }
        println!(
            "SEND_SORT_PROFILE {}",
            serde_json::json!({
                "sends": count, "senders": sender_count, "compact": compact, "collected_rss_bytes": collected_rss,
                "event_bytes": std::mem::size_of::<SentEvent>(),
                "elapsed_us": elapsed_us,
            })
        );
        std::hint::black_box(events);
    }

    /// Measure unused raw-row storage after an identical collected prefix in a fresh process.
    #[test]
    #[ignore = "isolated collected record storage profile; run in a fresh process"]
    fn generator_collected_record_storage_profile() {
        let rooms = std::env::var("CAPACITY_COLLECTED_STORAGE_ROOMS")
            .unwrap_or_else(|_| "999".into())
            .parse::<u32>()
            .unwrap();
        let percent = std::env::var("CAPACITY_COLLECTED_STORAGE_PERCENT")
            .unwrap_or_else(|_| "1".into())
            .parse::<usize>()
            .unwrap();
        assert!(percent <= 100);
        let mut config = crate::scenario_config(crate::Encoding::V3Json);
        config.rooms = rooms;
        config.players_per_room = 16;
        config.warmup = Duration::ZERO;
        config.duration = Duration::from_micros(1);
        config.send_rate_per_sender = 1_000_000.0;
        let (plans, churn) = crate::build_run_shape(&config).unwrap();
        let roster: Vec<_> = plans
            .iter()
            .map(|plan| (plan.name.clone(), plan.room))
            .collect();
        let mut expected = crate::complete_records(&plans);
        expected
            .receipts
            .truncate(expected.receipts.len() * percent / 100);
        let log = EventLog::for_plans(&plans, false).unwrap();
        for event in &expected.sent {
            log.push_sent(event.clone());
        }
        for event in &expected.receipts {
            log.push_receipt(event.clone());
        }
        let storage_bytes = |sent: usize, receipts: usize| {
            sent * std::mem::size_of::<SentEvent>() + receipts * std::mem::size_of::<ReceiptEvent>()
        };
        let before = {
            let state = log.state.lock().unwrap();
            storage_bytes(state.sent.capacity(), state.receipts.capacity())
        };
        let actual = log.take_records();
        let after = storage_bytes(actual.sent.capacity(), actual.receipts.capacity());
        let summarize = |records: &RunRecords| {
            crate::oracle::summarize(
                &plans,
                &roster,
                records,
                1000,
                96,
                crate::DeliveryClass::Reliable,
                &churn,
                None,
            )
        };
        let expected_summary = serde_json::to_vec(&summarize(&expected)).unwrap();
        let actual_summary = serde_json::to_vec(&summarize(&actual)).unwrap();
        assert_eq!(actual_summary, expected_summary);
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        use sha2::Digest;
        let digest = sha2::Sha256::digest(&actual_summary);
        println!(
            "COLLECTED_STORAGE_PROFILE {}",
            serde_json::json!({"rooms":rooms, "retained_percent":percent, "before_bytes":before, "after_bytes":after, "summary_sha256":digest.iter().map(|byte|format!("{byte:02x}")).collect::<String>()})
        );
    }

    /// Isolate retained omission-row storage from delivery and oracle scratch.
    #[test]
    #[ignore = "isolated collected gap storage profile; run in a fresh process"]
    fn generator_collected_gap_storage_profile() {
        let count = std::env::var("CAPACITY_COLLECTED_GAPS")
            .unwrap_or_else(|_| "1000001".into())
            .parse::<u64>()
            .unwrap();
        assert!((1..=2_000_000).contains(&count));
        let log = EventLog::default();
        let recipient = ReceiptName::from("r0p1");
        let sender = ReceiptName::from("r0p0");
        for seq in 1..=count {
            log.push_gap(GapEvent {
                recipient: recipient.clone(),
                sender: sender.clone(),
                epoch: 1,
                from_seq: seq,
                to_seq: seq,
                reason: signal_fish_server::protocol::DeliveryGapReason::LatestSuperseded,
            });
        }
        let before_bytes =
            log.state.lock().unwrap().gaps.capacity() * std::mem::size_of::<GapEvent>();
        let started = Instant::now();
        let records = log.take_records();
        let elapsed_us = started.elapsed().as_micros();
        assert_eq!(crate::config::count_u64(records.gaps.len()), count);
        for (index, event) in records.gaps.iter().enumerate() {
            assert_eq!(event.recipient, recipient);
            assert_eq!(event.sender, sender);
            assert_eq!(event.from_seq, crate::config::count_u64(index) + 1);
            assert_eq!(event.to_seq, event.from_seq);
        }
        println!(
            "COLLECTED_GAP_STORAGE_PROFILE {}",
            serde_json::json!({
                "gaps": count, "before_bytes": before_bytes,
                "after_bytes": records.gaps.capacity() * std::mem::size_of::<GapEvent>(),
                "elapsed_us": elapsed_us,
            })
        );
        std::hint::black_box(records);
    }

    /// Failed compaction must preserve every record, its original buffer and its capacity.
    #[test]
    fn failed_record_compaction_preserves_original_evidence_without_retrying() {
        for mode in ["error", "insufficient", "oversized", "nonempty"] {
            let mut rows = Vec::with_capacity(100);
            rows.extend([11_u64, 22, 33]);
            let pointer = rows.as_ptr();
            let capacity = rows.capacity();
            let mut calls = 0;
            release_unused_capacity_with_reservation(&mut rows, |replacement, count| {
                calls += 1;
                assert_eq!(count, 3);
                match mode {
                    "error" => replacement.try_reserve_exact(usize::MAX),
                    "insufficient" => Ok(()),
                    "oversized" => replacement.try_reserve_exact(capacity + 1),
                    "nonempty" => {
                        replacement.try_reserve_exact(count)?;
                        replacement.push(999);
                        Ok(())
                    }
                    _ => unreachable!(),
                }
            });
            assert_eq!(calls, 1);
            assert_eq!(rows, [11, 22, 33]);
            assert_eq!(rows.as_ptr(), pointer);
            assert_eq!(rows.capacity(), capacity);
        }
        let mut rows = Vec::with_capacity(100);
        rows.extend([11_u64, 22, 33]);
        let old_capacity = rows.capacity();
        let mut replacement_capacity = 0;
        release_unused_capacity_with_reservation(&mut rows, |replacement, count| {
            let result = replacement.try_reserve_exact(count);
            replacement_capacity = replacement.capacity();
            result
        });
        assert_eq!(rows, [11, 22, 33]);
        assert_eq!(
            rows.capacity(),
            if (rows.len()..old_capacity).contains(&replacement_capacity) {
                replacement_capacity
            } else {
                old_capacity
            }
        );
        for mut rows in [Vec::<u64>::with_capacity(100), vec![11, 22, 33]] {
            let original = rows.clone();
            let capacity = rows.capacity();
            release_unused_capacity_with_reservation(&mut rows, |_, _| {
                panic!("empty and full vectors must not allocate")
            });
            assert_eq!(rows, original);
            assert_eq!(rows.capacity(), if rows.is_empty() { 0 } else { capacity });
        }
    }

    /// #775: collection reserves the full shape; analysis keeps only retained rows.
    #[test]
    fn collected_records_release_unused_send_and_receipt_reservations() {
        let context = crate::unit_context();
        let complete = crate::complete_records(&context.plans);
        for count in [0, 1, complete.receipts.len() / 2, complete.receipts.len()] {
            let log = EventLog::for_plans(&context.plans, false).unwrap();
            for event in complete.sent.iter().take(count.min(complete.sent.len())) {
                log.push_sent(event.clone());
            }
            for event in complete.receipts.iter().take(count) {
                log.push_receipt(event.clone());
            }
            let mut expected = complete.clone();
            expected.sent.truncate(count.min(complete.sent.len()));
            expected.receipts.truncate(count);
            let (sent_capacity, receipt_capacity) = {
                let state = log.state.lock().unwrap();
                (state.sent.capacity(), state.receipts.capacity())
            };
            let actual = log.take_records();
            assert_eq!(
                serde_json::to_value(&actual).unwrap(),
                serde_json::to_value(&expected).unwrap()
            );
            assert!(
                actual.sent.capacity() <= sent_capacity,
                "retained send count={count}"
            );
            assert!(
                actual.receipts.capacity() <= receipt_capacity,
                "retained receipt count={count}"
            );
            if actual.sent.is_empty() {
                assert_eq!(actual.sent.capacity(), 0);
            }
            if actual.receipts.is_empty() {
                assert_eq!(actual.receipts.capacity(), 0);
            }
            let output = tempfile::tempdir().unwrap();
            crate::artifacts::write_deliveries(output.path(), &actual).unwrap();
            let replayed = crate::artifacts::read_records(output.path()).unwrap();
            assert_eq!(
                serde_json::to_value(replayed).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
        }
    }

    /// #775: lossy and lifecycle evidence must not retain collection growth slack.
    #[test]
    fn collected_control_records_release_slack_and_preserve_exact_replay() {
        let context = crate::unit_context();
        for count in [0, 1, 9, 17] {
            let log = EventLog::for_plans(&context.plans, false).unwrap();
            for index in 0..count {
                log.push_gap(GapEvent {
                    recipient: "r0p1".into(),
                    sender: "r0p0".into(),
                    epoch: 1,
                    from_seq: index + 1,
                    to_seq: index + 1,
                    reason: signal_fish_server::protocol::DeliveryGapReason::LatestSuperseded,
                });
                log.push_unsupported_notice(UnsupportedNoticeEvent {
                    recipient: "r0p1".into(),
                    at_us: index,
                });
                log.push_disconnect(DisconnectEvent {
                    recipient: "r0p1".into(),
                    observation: DisconnectObservation::StreamEnded,
                });
                log.push_churn(ChurnEvent {
                    recipient: "r0p1".into(),
                    phase: ChurnPhase::Disconnect,
                    at_us: index,
                    epoch: None,
                    tails: BTreeMap::new(),
                });
                log.push_join_failure(format!("join-{index}"));
                log.push_fault(InvalidReason::SendFailed {
                    sender: "r0p0".into(),
                    detail: format!("failure-{index}"),
                });
            }
            let expected = {
                let mut state = log.state.lock().unwrap();
                // Force retained spare capacity, including the empty-vector case.
                state.gaps.reserve(256);
                state.unsupported_notices.reserve(256);
                state.disconnects.reserve(256);
                state.churn.reserve(256);
                state.join_failures.reserve(256);
                state.faults.reserve(256);
                RunRecords {
                    gaps: state.gaps.clone(),
                    unsupported_notices: state.unsupported_notices.clone(),
                    disconnects: state.disconnects.clone(),
                    churn: state.churn.clone(),
                    join_failures: state.join_failures.clone(),
                    faults: state.faults.clone(),
                    ..RunRecords::default()
                }
            };
            let capacities_before = {
                let state = log.state.lock().unwrap();
                [
                    state.gaps.capacity(),
                    state.unsupported_notices.capacity(),
                    state.disconnects.capacity(),
                    state.churn.capacity(),
                    state.join_failures.capacity(),
                    state.faults.capacity(),
                ]
            };
            let actual = log.take_records();
            assert_eq!(
                serde_json::to_value(&actual).unwrap(),
                serde_json::to_value(&expected).unwrap()
            );
            for ((length, capacity), before) in [
                (actual.gaps.len(), actual.gaps.capacity()),
                (
                    actual.unsupported_notices.len(),
                    actual.unsupported_notices.capacity(),
                ),
                (actual.disconnects.len(), actual.disconnects.capacity()),
                (actual.churn.len(), actual.churn.capacity()),
                (actual.join_failures.len(), actual.join_failures.capacity()),
                (actual.faults.len(), actual.faults.capacity()),
            ]
            .into_iter()
            .zip(capacities_before)
            {
                if length == 0 {
                    assert_eq!(capacity, 0, "count={count}");
                } else {
                    assert!(
                        (length..before).contains(&capacity),
                        "count={count}: length={length}, capacity={capacity}, before={before}"
                    );
                }
            }
            let output = tempfile::tempdir().unwrap();
            crate::artifacts::write_deliveries(output.path(), &actual).unwrap();
            let replayed = crate::artifacts::read_records(output.path()).unwrap();
            assert_eq!(
                serde_json::to_value(&replayed).unwrap(),
                serde_json::to_value(&expected).unwrap()
            );
            let summarize = |records: &RunRecords| {
                crate::oracle::summarize(
                    &context.plans,
                    &context.roster,
                    records,
                    1000,
                    96,
                    crate::DeliveryClass::Volatile,
                    &crate::schedule::ChurnPlan::default(),
                    None,
                )
            };
            assert_eq!(
                serde_json::to_value(summarize(&actual)).unwrap(),
                serde_json::to_value(summarize(&replayed)).unwrap()
            );
        }
    }

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
                        assert!(
                            state.faults.capacity() >= 1,
                            "reserve the terminal reason before collection"
                        );
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

    // A lossy run may retain one omission for every attempted delivery. Keep
    // this probe separate from oracle scratch to measure retained gap evidence.
    #[test]
    #[ignore = "gap memory profile; run in a fresh process"]
    fn generator_gap_memory_profile() {
        fn measure<Name: Clone>(recipient: Name, sender: Name, count: usize) {
            let gaps: Vec<_> = (0..count)
                .map(|seq| GapEvent {
                    recipient: recipient.clone(),
                    sender: sender.clone(),
                    epoch: 1,
                    from_seq: u64::try_from(seq).expect("sequence") + 1,
                    to_seq: u64::try_from(seq).expect("sequence") + 1,
                    reason: signal_fish_server::protocol::DeliveryGapReason::LatestSuperseded,
                })
                .collect();
            println!(
                "gap_count={} gap_bytes={} retained_rss={:?}",
                gaps.len(),
                std::mem::size_of::<GapEvent<Name>>(),
                crate::diagnostics::resident_memory_bytes(std::process::id())
            );
            std::hint::black_box(gaps);
        }
        let count = std::env::var("CAPACITY_GAP_PROBE_EVENTS")
            .unwrap_or_else(|_| "100000".into())
            .parse::<usize>()
            .expect("gap count");
        assert!((1..=2_000_000).contains(&count));
        if std::env::var_os("CAPACITY_GAP_PROBE_STRING_NAMES").is_some() {
            measure("r0p1".to_string(), "r0p0".to_string(), count);
        } else {
            measure(ReceiptName::from("r0p1"), ReceiptName::from("r0p0"), count);
        }
    }

    #[test]
    fn gap_names_share_send_identities_without_changing_evidence() {
        let mut names = ReceiptNames::default();
        let mut records = RunRecords::default();
        for (recipient, sender, from_seq, to_seq) in [
            ("r0p1", "未知\"\\\n", 1, 7),
            ("r0p1", "未知\"\\\n", 1, 7),
            ("未知\"\\\n", "r0p1", u64::MAX, 2),
        ] {
            let event = GapEvent {
                recipient,
                sender,
                epoch: 42,
                from_seq,
                to_seq,
                reason: signal_fish_server::protocol::DeliveryGapReason::LatestSuperseded,
            };
            let expected = serde_json::to_vec(&event).unwrap();
            let shared = names.share_gap(event);
            assert_eq!(serde_json::to_vec(&shared).unwrap(), expected);
            records.gaps.push(shared);
        }
        records.sent.push(names.share_sent(SentEvent {
            sender: "未知\"\\\n",
            room: 0,
            seq: 0,
            epoch: 42,
            intended_us: 0,
            sent_us: 1,
            application_bytes: 96,
            encoded_frame_body_bytes: 150,
            phase: Phase::Measured,
        }));
        let output = tempfile::tempdir().expect("gap artifacts");
        crate::artifacts::write_deliveries(output.path(), &records).expect("write gaps");
        let replayed = crate::artifacts::read_records(output.path()).expect("replay gaps");
        assert_eq!(
            serde_json::to_vec(&records).unwrap(),
            serde_json::to_vec(&replayed).unwrap()
        );
        assert_eq!(replayed.gaps.len(), 3);
        assert!(Arc::ptr_eq(
            &replayed.sent[0].sender.0,
            &replayed.gaps[0].sender.0
        ));
        assert!(Arc::ptr_eq(
            &replayed.gaps[0].recipient.0,
            &replayed.gaps[1].recipient.0
        ));
        assert!(Arc::ptr_eq(
            &replayed.gaps[0].sender.0,
            &replayed.gaps[2].recipient.0
        ));
        assert!(std::mem::size_of::<GapEvent>() < std::mem::size_of::<GapEvent<String>>());
    }

    #[test]
    fn unexpected_receipts_stop_growing_after_the_run_budget() {
        let context = crate::unit_context_with_class(crate::DeliveryClass::Reliable);
        let log = EventLog::for_plans(&context.plans, false).expect("bounded log");
        let planned: usize = context
            .plans
            .iter()
            .map(|plan| {
                plan.sends.len()
                    * context
                        .plans
                        .iter()
                        .filter(|peer| peer.room == plan.room && peer.name != plan.name)
                        .count()
            })
            .sum();
        let limit = planned + 1024 + context.plans.len() * 128;
        for seq in 0..10_000 {
            log.push_receipt(timing_receipt(seq));
        }
        let records = log.take_records();
        assert_eq!(
            records.receipts.len(),
            limit + 1,
            "retain the crossing event"
        );
    }

    #[test]
    fn a_full_planned_receipt_buffer_grows_only_by_the_bounded_allowance() {
        let mut config = crate::scenario_config(crate::Encoding::V3Json);
        config.players_per_room = 2;
        config.warmup = Duration::ZERO;
        config.duration = Duration::from_secs(1);
        config.send_rate_per_sender = 50_000.0;
        let (plans, _) = crate::build_run_shape(&config).unwrap();
        let log = EventLog::for_plans(&plans, false).unwrap();
        let planned = log.state.lock().unwrap().receipts.capacity();
        assert_eq!(planned, 100_000);
        for seq in 0..crate::config::count_u64(planned) {
            log.push_receipt(timing_receipt(seq));
        }
        log.push_receipt(timing_receipt(100_000));
        let state = log.state.lock().unwrap();
        let budget = state.budget.as_ref().unwrap();
        assert_eq!(state.receipts.len(), planned + 1);
        assert_eq!(state.receipts.capacity(), budget.event_limit + 1);
        assert!(state.receipts.capacity() < planned * 2);
        println!(
            "BOUNDED_EVENT_PROFILE {}",
            serde_json::json!({
                "planned_receipts": planned, "retained_receipts": state.receipts.len(),
                "receipt_capacity": state.receipts.capacity(),
                "capacity_bytes": state.receipts.capacity() * std::mem::size_of::<ReceiptEvent>(),
                "rss_bytes": crate::diagnostics::resident_memory_bytes(std::process::id()),
            })
        );
    }

    #[test]
    #[ignore = "isolated unexpected-event memory profile; run in a fresh process"]
    fn runtime_event_budget_memory_profile() {
        let context = crate::unit_context_with_class(crate::DeliveryClass::Reliable);
        let bounded = std::env::var_os("CAPACITY_EVENT_BUDGET_PROBE_UNBOUNDED").is_none();
        let log = if bounded {
            EventLog::for_plans(&context.plans, false).unwrap()
        } else {
            EventLog::new()
        };
        let expected_retained = log
            .state
            .lock()
            .unwrap()
            .budget
            .as_ref()
            .map_or(1_000_000, |budget| budget.event_limit + 1);
        let receipt = timing_receipt(7);
        for attempted in 1..=1_000_000 {
            log.push_receipt(receipt.clone());
            if [1_000, 10_000, 100_000, 1_000_000].contains(&attempted) {
                let state = log.state.lock().unwrap();
                println!(
                    "UNEXPECTED_EVENT_PROFILE {}",
                    serde_json::json!({
                        "bounded": bounded, "attempted_receipts": attempted,
                        "retained_receipts": state.receipts.len(),
                        "receipt_capacity": state.receipts.capacity(),
                        "receipt_capacity_bytes": state.receipts.capacity() * std::mem::size_of::<ReceiptEvent>(),
                        "rss_bytes": crate::diagnostics::resident_memory_bytes(std::process::id()),
                    })
                );
            }
        }
        let records = log.take_records();
        assert_eq!(records.receipts.len(), expected_retained);
        assert_eq!(records.faults.len(), usize::from(bounded));
        std::hint::black_box(records);
    }

    fn bounded_test_log(events: usize, bytes: usize, timings: bool) -> EventLog {
        let mut log = EventLog::with_receipt_timings(timings);
        log.state.get_mut().unwrap().budget = Some(RecordBudget {
            event_limit: events,
            control_byte_limit: bytes,
            retained_events: 0,
            control_bytes: 0,
            first_excess_kind: None,
            unretained_events: 0,
            allocation_failure: None,
        });
        log
    }

    fn push_test_event(log: &EventLog, event: DeliveryEvent) {
        match event {
            DeliveryEvent::Sent(event) => log.push_sent(event),
            DeliveryEvent::Receipt(event) => {
                log.push_receipt(event);
            }
            DeliveryEvent::Gap(event) => log.push_gap(event),
            DeliveryEvent::UnsupportedNotice(event) => log.push_unsupported_notice(event),
            DeliveryEvent::Disconnect(event) => log.push_disconnect(event),
            DeliveryEvent::Churn(event) => log.push_churn(event),
            DeliveryEvent::JoinFailure { detail } => log.push_join_failure(detail),
            DeliveryEvent::Fault(reason) => log.push_fault(reason),
            DeliveryEvent::Registry { .. } => unreachable!("not an event producer"),
        }
    }

    #[test]
    fn every_unplanned_event_kind_keeps_the_crossing_event_and_exact_replay() {
        let context = crate::unit_context_with_class(crate::DeliveryClass::Reliable);
        let events = vec![
            DeliveryEvent::Receipt(timing_receipt(7)),
            DeliveryEvent::Gap(crate::gap_for(
                "r0p1",
                "r0p0",
                7,
                signal_fish_server::protocol::DeliveryGapReason::LatestSuperseded,
            )),
            DeliveryEvent::UnsupportedNotice(UnsupportedNoticeEvent {
                recipient: "r0p1".into(),
                at_us: 3,
            }),
            DeliveryEvent::Disconnect(DisconnectEvent {
                recipient: "r0p1".into(),
                observation: DisconnectObservation::StreamEnded,
            }),
            DeliveryEvent::Churn(ChurnEvent {
                recipient: "r0p1".into(),
                phase: ChurnPhase::Disconnect,
                at_us: 3,
                epoch: None,
                tails: BTreeMap::new(),
            }),
            DeliveryEvent::JoinFailure {
                detail: "join failed".into(),
            },
            DeliveryEvent::Fault(InvalidReason::MalformedServerFrame {
                recipient: "r0p1".into(),
                detail: "invalid frame".into(),
            }),
        ];
        for timings in [false, true] {
            for event in &events {
                let log = bounded_test_log(1, usize::MAX, timings);
                push_test_event(&log, event.clone());
                assert!(!log.record_budget_exhausted());
                push_test_event(&log, event.clone());
                assert!(log.record_budget_exhausted());
                push_test_event(&log, event.clone());
                // Every completed send remains evidence after receive-side stop.
                for sent in crate::complete_records(&context.plans).sent {
                    log.push_sent(sent);
                }
                let records = log.take_records();
                assert!(records.faults.iter().any(|fault| matches!(
                    fault,
                    InvalidReason::GeneratorRecordBudgetExceeded {
                        event_limit: 1,
                        retained_events: 2,
                        unretained_events: 1,
                        ..
                    }
                )));
                let captured: Vec<_> = records
                    .events()
                    .filter(|row| {
                        !matches!(
                            row,
                            DeliveryEventRef::Sent(_)
                                | DeliveryEventRef::Registry { .. }
                                | DeliveryEventRef::Fault(
                                    InvalidReason::GeneratorRecordBudgetExceeded { .. }
                                )
                        )
                    })
                    .collect();
                assert_eq!(
                    serde_json::to_value(captured).unwrap(),
                    serde_json::to_value(vec![event, event]).unwrap()
                );
                let output = tempfile::tempdir().unwrap();
                crate::artifacts::write_deliveries(output.path(), &records).unwrap();
                let replayed = crate::artifacts::read_records(output.path()).unwrap();
                assert_eq!(
                    serde_json::to_value(&records).unwrap(),
                    serde_json::to_value(&replayed).unwrap()
                );
                let summarize = |records: &RunRecords| {
                    crate::oracle::summarize(
                        &context.plans,
                        &context.roster,
                        records,
                        1000,
                        96,
                        crate::DeliveryClass::Reliable,
                        &crate::ChurnPlan::default(),
                        None,
                    )
                };
                let summary = summarize(&records);
                assert!(!summary.valid);
                assert_eq!(
                    serde_json::to_value(summary).unwrap(),
                    serde_json::to_value(summarize(&replayed)).unwrap()
                );
            }
        }
    }

    #[test]
    #[ignore = "receipt collection component profile; run in a fresh process"]
    fn receipt_collection_lock_profile() {
        let separate_check = std::env::var_os("CAPACITY_RECEIPT_SEPARATE_CHECK").is_some();
        let workers = 4;
        let per_worker = 250_000;
        let mut log = bounded_test_log(workers * per_worker, usize::MAX, false);
        log.state
            .get_mut()
            .unwrap()
            .receipts
            .reserve_exact(workers * per_worker);
        let log = Arc::new(log);
        let barrier = Arc::new(std::sync::Barrier::new(workers + 1));
        let receipt = timing_receipt(0);
        let threads: Vec<_> = (0..workers)
            .map(|worker| {
                let log = Arc::clone(&log);
                let barrier = Arc::clone(&barrier);
                let receipt = receipt.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    for position in 0..per_worker {
                        let mut receipt = receipt.clone();
                        receipt.seq = crate::config::count_u64(worker * per_worker + position);
                        let active = log.push_receipt(receipt);
                        if separate_check {
                            assert!(!log.record_budget_exhausted());
                        } else {
                            assert!(active);
                        }
                    }
                })
            })
            .collect();
        let started = Instant::now();
        barrier.wait();
        for thread in threads {
            thread.join().unwrap();
        }
        let elapsed = started.elapsed();
        let records = log.take_records();
        assert_eq!(records.receipts.len(), workers * per_worker);
        assert!(records.faults.is_empty());
        let mut sequences: Vec<_> = records.receipts.iter().map(|receipt| receipt.seq).collect();
        sequences.sort_unstable();
        assert!(sequences
            .iter()
            .copied()
            .eq(0..crate::config::count_u64(workers * per_worker)));
        println!(
            "RECEIPT_LOCK_PROFILE {}",
            serde_json::json!({
                "separate_check": separate_check, "workers": workers,
                "receipts": records.receipts.len(), "elapsed_us": elapsed.as_micros(),
                "checksum": sequences.iter().sum::<u64>(),
                "receipt_capacity": records.receipts.capacity(),
            })
        );
    }

    #[test]
    fn receipt_admission_reports_the_exact_budget_crossing() {
        for timings in [false, true] {
            let log = bounded_test_log(2, usize::MAX, timings);
            for (seq, expected) in [(0, true), (1, true), (2, false), (3, false)] {
                assert_eq!(log.push_receipt(timing_receipt(seq)), expected);
                assert_eq!(log.record_budget_exhausted(), !expected);
            }
            let records = log.take_records();
            assert_eq!(records.receipts.len(), 3);
            assert_eq!(records.receipts.last().unwrap().seq, 2);
            assert!(matches!(
                records.faults.as_slice(),
                [InvalidReason::GeneratorRecordBudgetExceeded {
                    retained_events: 3,
                    unretained_events: 1,
                    ..
                }]
            ));
            if let Some(timings) = log.take_receipt_timings() {
                assert_eq!(timings.receipt_push.count, 3);
            }
        }
    }

    #[test]
    fn a_fault_flood_keeps_a_reserved_terminal_reason_slot() {
        let log = bounded_test_log(8, usize::MAX, false);
        for _ in 0..100 {
            log.push_fault(InvalidReason::MalformedServerFrame {
                recipient: "r0p1".into(),
                detail: "bad frame".into(),
            });
        }
        let capacity = log.state.lock().unwrap().faults.capacity();
        let records = log.take_records();
        assert_eq!(records.faults.len(), 10);
        assert_eq!(records.faults.capacity(), capacity);
        assert_eq!(capacity, 10);
    }

    #[test]
    fn control_byte_budget_retains_the_complete_crossing_frame() {
        let event = InvalidReason::ServerRejected {
            recipient: "r0p1".into(),
            detail: "é\\\"".repeat(10_000),
        };
        let size = crate::runner::json_byte_count(&event).unwrap();
        for limit in [size - 1, size] {
            let log = bounded_test_log(100, limit, false);
            log.push_fault(event.clone());
            assert_eq!(log.record_budget_exhausted(), limit < size);
            log.push_fault(event.clone());
            let records = log.take_records();
            assert_eq!(records.faults[0], event);
            let terminal = records.faults.last().unwrap();
            assert!(
                matches!(terminal, InvalidReason::GeneratorRecordBudgetExceeded {
                retained_control_bytes, unretained_events, ..
            } if *retained_control_bytes == crate::config::count_u64(if limit < size { size } else { size * 2 })
                && *unretained_events == u64::from(limit < size))
            );
        }
    }

    #[tokio::test]
    async fn concurrent_budget_exhaustion_has_one_terminal_reason_and_wakes_waiters() {
        let log = Arc::new(bounded_test_log(2, usize::MAX, false));
        let waiter = {
            let log = log.clone();
            tokio::spawn(async move {
                log.wait_record_budget_exhausted().await;
            })
        };
        // Register the waiter before producers cross the limit, so this also
        // exercises the notification path rather than only the late check.
        tokio::task::yield_now().await;
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let log = log.clone();
                std::thread::spawn(move || {
                    for seq in 0..1000 {
                        log.push_receipt(timing_receipt(seq));
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
        // A waiter registered after exhaustion must also complete.
        tokio::time::timeout(Duration::from_secs(1), log.wait_record_budget_exhausted())
            .await
            .unwrap();
        let records = log.take_records();
        assert_eq!(records.receipts.len(), 3);
        assert!(matches!(
            records.faults.as_slice(),
            [InvalidReason::GeneratorRecordBudgetExceeded {
                retained_events: 3,
                unretained_events: 7997,
                ..
            }]
        ));
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
