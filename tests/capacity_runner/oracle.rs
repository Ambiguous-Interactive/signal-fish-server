//! Delivery-contract oracle: pure function from run events to an exact
//! outcome summary.
//!
//! The oracle is the negative-control surface: missing, duplicate, misrouted,
//! and out-of-order deliveries each produce a distinct, explicit invalidation
//! reason with the first offending key, and a run is valid only when every
//! connected recipient observed the complete, in-order stream from every
//! co-room sender — gap-free for reliable, exactly gap-covered for the
//! latest/volatile classes — no offered work was left unsent (without a
//! declared fault explaining it) or outstanding, and no scheduled send
//! lagged past the generator bound. A disconnected recipient must hold a
//! complete-through prefix — loss may only be the loud tail cut off with
//! the connection, never a silent hole.
//!
//! Because this module is pure, the runner's `summary.json` and an artifact
//! replay (`artifacts::replay`) run the same code and must agree exactly.

use std::collections::{BTreeMap, BTreeSet};

use hdrhistogram::Histogram;

use crate::config::{count_u64, DeliveryClass, Experiment};
use crate::records::{ChurnPhase, GapEvent, RunRecords, SentEvent};
use crate::schedule::{ChurnPlan, Phase, SenderPlan};

/// One delivery key that violated the contract, named exactly. `epoch` and
/// `seq` are the server's per-`(sender, epoch)` stream coordinates.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeliveryKey {
    pub recipient: String,
    pub sender: String,
    pub epoch: u32,
    pub seq: u64,
}

/// The first gap-report violation of a run, named exactly.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GapViolation {
    pub recipient: String,
    pub sender: String,
    pub detail: String,
}

/// Every way a run can be invalid. Each variant is an explicit, actionable
/// reason — a negative control asserts its exact variant.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InvalidReason {
    /// Recorded sends differ from the declared deterministic workload.
    InvalidScheduledSends {
        count: u64,
        first: ScheduledSendViolation,
    },
    /// A GameData frame lacked a usable ledger or registered sender identity.
    /// Its observed bytes remain raw evidence even though delivery pairing failed.
    UnidentifiedGameData {
        recipient: String,
        received_us: u64,
        application_bytes: u64,
        encoded_frame_body_bytes: u64,
        detail: String,
    },
    /// Recorded application sizes disagree with the configured target.
    PayloadSizeMismatch {
        count: u64,
        first: PayloadSizeViolation,
    },
    /// An encoded message body cannot contain its recorded application value.
    InvalidEncodedFrameBodySize {
        count: u64,
        first: PayloadSizeViolation,
    },
    /// A raw byte total exceeded the summary integer range.
    PayloadByteTotalOverflow {
        count: u64,
        first: PayloadSizeViolation,
    },
    /// A client failed to join its room; the run never measured the roster.
    JoinFailed { failures: Vec<String> },
    /// A recipient missed deliveries it was owed (a hole mid-stream or, for
    /// a connected-through recipient, any deficit).
    MissingDeliveries { count: u64, first: DeliveryKey },
    /// The same delivery reached a recipient more than once.
    DuplicateDeliveries { count: u64, first: DeliveryKey },
    /// A recipient observed a delivery outside its contract: a sender from
    /// another room, its own echo, or a sequence beyond what was sent.
    MisroutedDeliveries { count: u64, first: DeliveryKey },
    /// A gap report violated the delivery contract: an overlap with a
    /// delivery or another gap, a range outside what the sender sent, a
    /// 1-based range reaching below the first server sequence, a reason the
    /// run's class cannot produce, an unknown sender, or any gap at all in
    /// a reliable run (reliable delivery permits no loss).
    InvalidGapReports { count: u64, first: GapViolation },
    /// Per-sender stream arrived out of order at a recipient.
    OutOfOrderDeliveries { count: u64, first: DeliveryKey },
    /// A connected-through recipient was still owed deliveries after the
    /// drain window: reliable delivery left work outstanding.
    OutstandingAtEnd { count: u64 },
    /// Scheduled sends fell behind their intended times past the generator
    /// bound: the measurement is generator-limited, not server evidence.
    GeneratorSaturated { max_lag_us: u64, bound_us: u64 },
    /// Reliable delivery left work unsent that no declared fault explains:
    /// a sender stopped without recording why (a generator bookkeeping bug).
    UnsentWork { count: u64 },
    /// The spawned server was terminated mid-run (control hook).
    ServerTerminated,
    /// A recipient deliberately stopped reading, so the run is not a
    /// capacity measurement (control hook; eviction accounting is asserted
    /// against the server's counter by the control test). Exempted
    /// recipients' deficits stay visible in `per_recipient` and `totals`
    /// but are excluded from the verdict.
    SlowConsumerDisconnect { recipients: Vec<String> },
    /// A recipient's stream ended while the run believed the server healthy.
    UnexpectedDisconnect { recipients: Vec<String> },
    /// A sender task could not push a scheduled message into the socket
    /// while the server was believed healthy.
    SendFailed { sender: String, detail: String },
    /// A server frame a recipient task could not decode.
    MalformedServerFrame { recipient: String, detail: String },
    /// A peer could not rejoin after its declared churn disconnect (its
    /// remaining schedule is unsent work, and its seat is unproven).
    ReconnectFailed { peer: String, detail: String },
    /// A planned churn cycle never happened (the runner failed to execute
    /// its own deterministic plan), so the run is not the churn measurement
    /// its manifest claims.
    ChurnNotPerformed { peer: String },
    /// A rejoin snapshot named a `PlayerId` the run's registry never
    /// recorded — the identity table a replay resolves against is
    /// incomplete, so stream attribution cannot be trusted.
    UnresolvedSenderIdentity { player_id: String },
    /// Generator tasks outlived the quiescence margin (a wedged generator).
    RunnerDeadlineExceeded { detail: String },
    /// The server rejected a frame mid-run (bad class, payload cap, rate
    /// limit) — recorded from the `Error` frame so the deficit is never
    /// unexplained.
    ServerRejected { recipient: String, detail: String },
    /// The unsupported-conversion contract was broken: a payload the server
    /// must refuse to convert reached a cross-format recipient as a
    /// delivery (or any binary frame arrived where the experiment permits
    /// none). The exact-report path was bypassed — the leak class.
    UnsupportedFormatLeak { count: u64, first: DeliveryKey },
    /// The rate-limited unsupported-format advisory cadence was exceeded at
    /// a recipient (at most one notice per opaque sender per second): the
    /// advisory path is flooding, which is its own contract violation.
    UnsupportedNoticeFlood {
        recipient: String,
        count: u64,
        bound: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScheduledSendViolation {
    pub sender: String,
    pub seq: u64,
    pub mismatch: SendPlanMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendPlanMismatch {
    UnknownSender,
    UnscheduledSequence,
    Duplicate,
    Room,
    IntendedTime,
    Phase,
    UnannouncedEpoch { epoch: u32 },
}

/// Per-recipient accounting, in roster order.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RecipientOutcome {
    pub recipient: String,
    pub room: u32,
    /// False once a disconnect was recorded for this recipient.
    pub connected_through: bool,
    /// Unique deliveries received, per sender.
    pub received: BTreeMap<String, u64>,
    /// Owed-but-unreceived count for connected recipients.
    pub missing: u64,
    /// Tail a disconnected recipient never saw (permitted with a disconnect).
    pub undelivered_at_disconnect: u64,
    /// Omissions the server accounted with exact gap reports
    /// (latest/volatile classes and the unsupported-format experiment;
    /// always 0 for a plain reliable run).
    pub gap_covered: u64,
    pub duplicates: u64,
    pub misrouted: u64,
    pub out_of_order: u64,
    /// This recipient's own one-way latency tail over the measured window,
    /// so an aggregate percentile cannot hide a starved room.
    pub latency_us: LatencyStats,
}

/// Scheduled application send to receipt latency over the measured window
/// (microseconds). Percentiles use HDR buckets; max is the exact sample.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct LatencyStats {
    pub samples: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
}

/// Scheduled-send lag stats over completed sends (microseconds).
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct LagStats {
    pub max_us: u64,
    pub p99_us: u64,
    pub bound_us: u64,
}

/// Run totals over offered work.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct Totals {
    /// Scheduled sends across all senders (warm-up plus measured).
    pub scheduled: u64,
    /// Sends that actually left the generator.
    pub sent: u64,
    /// Scheduled sends the generator never emitted (saturation or a declared
    /// fault — never a silent drop).
    pub unsent: u64,
    /// Deliveries observed across all recipients (expected or not).
    pub receipts: u64,
    /// Sends connected-through recipients were still owed at the end.
    pub outstanding: u64,
    /// Omissions accounted by exact gap reports across all recipients
    /// (latest/volatile classes and the unsupported-format experiment;
    /// always 0 for a plain reliable run).
    pub gap_covered: u64,
}

/// The first raw size violation, including its direction and event identity.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PayloadSizeViolation {
    /// `ingress` for a completed send; `egress` for a received delivery.
    pub direction: String,
    pub sender: Option<String>,
    pub recipient: Option<String>,
    pub seq: Option<u64>,
    pub application_bytes: u64,
    pub encoded_frame_body_bytes: u64,
    pub expected_application_bytes: u32,
}

/// Observed byte sizes. Empty populations have zero minimum and maximum.
/// Totals saturate only when the verdict records `PayloadByteTotalOverflow`.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct ByteStats {
    pub samples: u64,
    pub total_bytes: u64,
    pub min_bytes: u64,
    pub max_bytes: u64,
}

impl ByteStats {
    fn record(&mut self, bytes: u64) -> bool {
        if self.samples == 0 {
            self.min_bytes = bytes;
        } else {
            self.min_bytes = self.min_bytes.min(bytes);
        }
        self.samples += 1;
        let overflow = self.total_bytes.checked_add(bytes).is_none();
        self.total_bytes = self.total_bytes.saturating_add(bytes);
        self.max_bytes = self.max_bytes.max(bytes);
        overflow
    }
}

/// Application bytes and encoded message body bytes for one direction.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct DirectionBytes {
    pub application: ByteStats,
    pub encoded_frame_body: ByteStats,
}

impl DirectionBytes {
    fn record(&mut self, application: u64, encoded: u64) -> bool {
        let application_overflow = self.application.record(application);
        let encoded_overflow = self.encoded_frame_body.record(encoded);
        application_overflow || encoded_overflow
    }
}

/// Raw observed traffic for one scheduled phase, including duplicate receipts.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct PhaseBytes {
    pub ingress: DirectionBytes,
    pub egress: DirectionBytes,
}

/// Raw GameData size evidence. Receipt phases follow their sender-ledger key.
/// Unknown receipt keys remain visible separately; they cannot be assigned a phase.
/// These bytes exclude control frames, WebSocket headers, TLS, and TCP overhead.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct PayloadBytes {
    pub warmup: PhaseBytes,
    pub measured: PhaseBytes,
    pub unmatched_egress: DirectionBytes,
}

/// The exact outcome of one run.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OutcomeSummary {
    pub valid: bool,
    pub reasons: Vec<InvalidReason>,
    pub totals: Totals,
    pub per_recipient: Vec<RecipientOutcome>,
    pub latency_us: LatencyStats,
    pub generator_lag_us: LagStats,
    pub payload_bytes: PayloadBytes,
    /// The run's contract-experiment label (`None` for a default-contract
    /// run), echoed from the config so an experiment's summary can never be
    /// read as a default-contract measurement.
    #[serde(default)]
    pub experiment: Option<String>,
    /// Rate-limited unsupported-format advisories observed across all
    /// recipients (always 0 outside the unsupported-format experiment).
    #[serde(default)]
    pub unsupported_notices: u64,
}

/// Category accumulator: total count plus the first offending key.
#[derive(Default, Clone)]
struct Category {
    count: u64,
    first: Option<DeliveryKey>,
}

impl Category {
    fn record(&mut self, key: DeliveryKey) {
        self.count += 1;
        self.first.get_or_insert(key);
    }

    fn into_reason(
        self,
        build: impl FnOnce(u64, DeliveryKey) -> InvalidReason,
    ) -> Option<InvalidReason> {
        let first = self.first?;
        Some(build(self.count, first))
    }
}

/// Accumulator for gap-report violations: total count plus the first
/// offending event and why.
#[derive(Default)]
struct GapViolations {
    count: u64,
    first: Option<GapViolation>,
}

impl GapViolations {
    fn record_violation(&mut self, gap: &GapEvent, detail: String) {
        self.count += 1;
        self.first.get_or_insert(GapViolation {
            recipient: gap.recipient.clone(),
            sender: gap.sender.clone(),
            detail,
        });
    }

    fn into_reason(self) -> Option<InvalidReason> {
        let first = self.first?;
        Some(InvalidReason::InvalidGapReports {
            count: self.count,
            first,
        })
    }
}

/// Borrow canonical sends in sender blocks, with direct offsets for contiguous
/// sequences and binary search for sparse or repeated keys. Arbitrary artifact order
/// uses a stable pointer sort, preserving the former map's last-key-wins rule.
/// No receipt or raw send evidence is changed by this lookup index.
enum SendIndex<'a> {
    Ordered(Vec<SenderSends<'a>>),
    Reordered(Vec<&'a SentEvent>),
}

struct SenderSends<'a> {
    events: &'a [SentEvent],
    contiguous: bool,
}

impl<'a> SendIndex<'a> {
    fn sorted_events(&self) -> impl Iterator<Item = &'a SentEvent> + '_ {
        let (senders, reordered) = match self {
            Self::Ordered(senders) => (senders.as_slice(), &[][..]),
            Self::Reordered(events) => (&[][..], events.as_slice()),
        };
        senders
            .iter()
            .flat_map(|sender| sender.events.iter())
            .chain(reordered.iter().copied())
    }

    fn new(sent: &'a [SentEvent]) -> Self {
        if sent.is_sorted_by_key(|event| (event.sender.as_str(), event.seq)) {
            let sender_count = usize::from(!sent.is_empty())
                + sent
                    .windows(2)
                    .filter(|pair| pair[0].sender != pair[1].sender)
                    .count();
            let mut senders = Vec::with_capacity(sender_count);
            let mut remaining = sent;
            while let Some(first) = remaining.first() {
                let end = remaining.partition_point(|event| event.sender == first.sender);
                let (events, rest) = remaining.split_at(end);
                let contiguous = events
                    .windows(2)
                    .all(|pair| pair[0].seq.checked_add(1) == Some(pair[1].seq));
                senders.push(SenderSends { events, contiguous });
                remaining = rest;
            }
            return Self::Ordered(senders);
        }
        let mut ordered: Vec<_> = sent.iter().collect();
        ordered.sort_by_key(|event| (event.sender.as_str(), event.seq));
        Self::Reordered(ordered)
    }

    fn get(&self, sender: &str, seq: u64) -> Option<&'a SentEvent> {
        fn find<'a>(
            count: usize,
            event_at: impl Fn(usize) -> &'a SentEvent,
            sender: &str,
            seq: u64,
        ) -> Option<&'a SentEvent> {
            let mut left = 0;
            let mut right = count;
            while left < right {
                let middle = left + (right - left) / 2;
                let event = event_at(middle);
                if (event.sender.as_str(), event.seq) <= (sender, seq) {
                    left = middle + 1;
                } else {
                    right = middle;
                }
            }
            let event = event_at(left.checked_sub(1)?);
            ((event.sender.as_str(), event.seq) == (sender, seq)).then_some(event)
        }
        match self {
            Self::Ordered(senders) => {
                let index = senders
                    .binary_search_by(|block| block.events[0].sender.as_str().cmp(sender))
                    .ok()?;
                let block = &senders[index];
                if block.contiguous {
                    let offset = usize::try_from(seq.checked_sub(block.events[0].seq)?).ok()?;
                    block.events.get(offset)
                } else {
                    find(
                        block.events.len(),
                        |index| &block.events[index],
                        sender,
                        seq,
                    )
                }
            }
            Self::Reordered(sent) => find(sent.len(), |index| sent[index], sender, seq),
        }
    }
}

fn scheduled_send_evidence(
    plans: &[SenderPlan],
    records: &RunRecords,
    reasons: &mut Vec<InvalidReason>,
) -> u64 {
    let plans: BTreeMap<_, _> = plans
        .iter()
        .map(|plan| (plan.name.as_str(), plan))
        .collect();
    let rejoined_epochs: BTreeSet<_> = records
        .churn
        .iter()
        .filter_map(|event| {
            let epoch = event.epoch?;
            (event.phase == ChurnPhase::Rejoined && epoch > 1)
                .then_some((event.recipient.as_str(), epoch))
        })
        .collect();
    let index = SendIndex::new(&records.sent);
    let mut previous_key = None;
    let mut covered = false;
    let mut completed = 0;
    let mut count = 0;
    let mut first = None;
    for event in index.sorted_events() {
        let key = (event.sender.as_str(), event.seq);
        let duplicate = previous_key == Some(key);
        if !duplicate {
            covered = false;
        }
        previous_key = Some(key);
        let mismatch = match plans.get(event.sender.as_str()) {
            None => Some(SendPlanMismatch::UnknownSender),
            Some(plan) => match usize::try_from(event.seq)
                .ok()
                .and_then(|seq| plan.sends.get(seq))
            {
                None => Some(SendPlanMismatch::UnscheduledSequence),
                Some(_) if event.room != plan.room => Some(SendPlanMismatch::Room),
                Some(send) if event.intended_us != send.intended_us => {
                    Some(SendPlanMismatch::IntendedTime)
                }
                Some(send) if event.phase != send.phase => Some(SendPlanMismatch::Phase),
                Some(_)
                    if event.epoch != 1
                        && !rejoined_epochs.contains(&(event.sender.as_str(), event.epoch)) =>
                {
                    Some(SendPlanMismatch::UnannouncedEpoch { epoch: event.epoch })
                }
                Some(_) => None,
            },
        };
        if mismatch.is_none() && !covered {
            completed += 1;
            covered = true;
        }
        if let Some(mismatch) = if duplicate {
            Some(SendPlanMismatch::Duplicate)
        } else {
            mismatch
        } {
            count += 1;
            first.get_or_insert_with(|| ScheduledSendViolation {
                sender: event.sender.to_string(),
                seq: event.seq,
                mismatch,
            });
        }
    }
    if let Some(first) = first {
        reasons.push(InvalidReason::InvalidScheduledSends { count, first });
    }
    completed
}

/// A sender incarnation's exact ledger, in sequence order. Canonical runs
/// borrow raw events; arbitrary artifact order retains sorted pointers.
enum RelayBlock<'a> {
    Borrowed(&'a [SentEvent]),
    Indexed(Vec<&'a SentEvent>),
}

impl<'a> RelayBlock<'a> {
    fn len(&self) -> usize {
        match self {
            Self::Borrowed(events) => events.len(),
            Self::Indexed(events) => events.len(),
        }
    }

    fn get(&self, index: usize) -> Option<&'a SentEvent> {
        match self {
            Self::Borrowed(events) => events.get(index),
            Self::Indexed(events) => events.get(index).copied(),
        }
    }

    fn completed_by(&self, at_us: u64) -> usize {
        match self {
            Self::Borrowed(events) => events.iter().filter(|event| event.sent_us <= at_us).count(),
            Self::Indexed(events) => events.iter().filter(|event| event.sent_us <= at_us).count(),
        }
    }
}

fn relay_blocks(sent: &[SentEvent]) -> BTreeMap<(&str, u32), RelayBlock<'_>> {
    let mut blocks = BTreeMap::new();
    let mut start = 0;
    while start < sent.len() {
        let key = (sent[start].sender.as_str(), sent[start].epoch);
        let mut end = start + 1;
        while end < sent.len() && (sent[end].sender.as_str(), sent[end].epoch) == key {
            end += 1;
        }
        let chunk = &sent[start..end];
        match blocks.entry(key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(if chunk.is_sorted_by_key(|event| event.seq) {
                    RelayBlock::Borrowed(chunk)
                } else {
                    RelayBlock::Indexed(chunk.iter().collect())
                });
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let block = entry.get_mut();
                if let RelayBlock::Borrowed(events) = block {
                    *block = RelayBlock::Indexed(events.iter().collect());
                }
                if let RelayBlock::Indexed(events) = block {
                    events.extend(chunk);
                }
            }
        }
        start = end;
    }
    for block in blocks.values_mut() {
        if let RelayBlock::Indexed(events) = block {
            // Stable order preserves original evidence for duplicate keys.
            events.sort_by_key(|event| event.seq);
        }
    }
    blocks
}

/// One-way latency samples over the measured window, per recipient: for
/// every receipt, the receipt time minus the intended send time of the same
/// `(sender, seq)` delivery. Single source for the summary percentiles (run
/// total and per recipient) and the histogram artifact, so they can never
/// disagree.
pub fn latency_pairs(records: &RunRecords) -> impl Iterator<Item = (&str, u64)> {
    let sent_by_key = SendIndex::new(&records.sent);
    records.receipts.iter().filter_map(move |receipt| {
        let sent = sent_by_key.get(receipt.sender.as_str(), receipt.seq)?;
        (sent.phase == Phase::Measured).then(|| {
            (
                receipt.recipient.as_str(),
                receipt.received_us.saturating_sub(sent.intended_us),
            )
        })
    })
}

/// One-way latency samples over the measured window (all recipients).
/// Only deterministic test callers need to collect these samples.
pub fn latency_samples(records: &RunRecords) -> Vec<u64> {
    latency_pairs(records)
        .map(|(_recipient, sample)| sample)
        .collect()
}

/// Streaming latency statistics retain HDR buckets and the exact raw maximum.
struct LatencyAccumulator {
    histogram: Histogram<u64>,
    max_us: u64,
}

impl Default for LatencyAccumulator {
    fn default() -> Self {
        Self {
            histogram: Histogram::<u64>::new(3).expect("latency histogram precision"),
            max_us: 0,
        }
    }
}

impl LatencyAccumulator {
    fn record(&mut self, sample: u64) {
        self.histogram
            .record(sample)
            .expect("latency histogram auto-resizes to fit sample");
        self.max_us = self.max_us.max(sample);
    }

    fn stats(&self) -> LatencyStats {
        LatencyStats {
            samples: self.histogram.len(),
            p50_us: self.histogram.value_at_quantile(0.5),
            p95_us: self.histogram.value_at_quantile(0.95),
            p99_us: self.histogram.value_at_quantile(0.99),
            max_us: self.max_us,
        }
    }
}

/// The shared histogram for summary percentiles and the artifact. Grow the
/// range as observations arrive so long stalls remain visible.
pub fn latency_histogram(samples: impl IntoIterator<Item = u64>) -> Histogram<u64> {
    let mut latency = LatencyAccumulator::default();
    for sample in samples {
        latency.record(sample);
    }
    latency.histogram
}

/// How one recipient's co-room relay streams evolved across the storm.
///
/// Every member starts in epoch 1 (the initial join). A member's rejoin
/// adopts its new epoch (and closes its previous epoch's stream — deliveries
/// for a closed stream are stale-epoch misroutes). The recipient's own
/// rejoin re-adopts every member from its join snapshot: the snapshot's
/// per-member `(epoch, seq tail)` sets the stream's owed floor — everything
/// at or below the tail was accounted to the away window ("a recipient owes
/// no GameData at or below this sequence in the paired epoch"), and the
/// stream's owed window starts one past it.
#[derive(Default)]
struct ChurnView {
    /// Member -> the epoch this recipient's seat adopts (and every epoch it
    /// has adopted, which are the streams it can be owed).
    adopted: BTreeMap<String, BTreeSet<u32>>,
    /// `(sender, epoch) -> instant past which deliveries for the stream are
    /// stale-epoch misroutes (the sender rejoined past it)`.
    closed_at: BTreeMap<(String, u32), u64>,
    /// `(sender, epoch) -> (owed floor, from_us, server_enforced)`: from
    /// `from_us` on, the stream's owed window starts at floor + 1. A
    /// `server_enforced` floor (a rejoin snapshot tail) is also a delivery
    /// contract — the server gates its own queued frames against it, so a
    /// redelivery is a misroute. A derived floor (send times at the rejoin
    /// instant) is bookkeeping only: the server has no watermark for an
    /// omitted member, so a frame just past the seat can legally arrive
    /// with its recorded send time behind the rejoin instant.
    floors: BTreeMap<(String, u32), (u64, u64, bool)>,
}

/// Rebuild every roster peer's [`ChurnView`] from the run's recorded churn
/// events, per room. Each viewer applies its room's events in
/// `(at_us, own-events-first)` order: the recorded instants come from
/// different tasks and can tie or invert, so every transition reconciles
/// against the already-adopted epochs instead of trusting event order, and
/// the earliest close of a stream wins.
///
/// Rejoin snapshot tails arrive UNRESOLVED — `(PlayerId, tail)` — and are
/// resolved here against the registry recorded at end of run, so snapshot
/// attribution never depends on task scheduling order. A member the
/// snapshot omits (the documented join-snapshot race under concurrent
/// rejoins) is not closed: its dead incarnations cannot deliver to the new
/// seat, its live incarnation is adopted by its own rejoin event, and a
/// cross-room leak is caught by the roster check. Instead it receives a
/// derived owed floor — the server cannot deliver a freshly seated
/// recipient any frame fanned out before its seat, so the sends of its
/// streams that completed at or before the rejoin instant are exactly the
/// permitted-unobserved set. The derivation errs safe: a send whose
/// recorded completion crossed the seat instant is either received (harmless
/// to floor) or genuinely pre-seat (correctly floored).
fn churn_views(
    records: &RunRecords,
    roster: &[(String, u32)],
    blocks: &BTreeMap<(&str, u32), RelayBlock<'_>>,
) -> (BTreeMap<String, ChurnView>, Vec<String>) {
    let room_of: BTreeMap<&str, u32> = roster
        .iter()
        .map(|(name, room)| (name.as_str(), *room))
        .collect();
    let resolve =
        |records: &RunRecords, player_id: &str, unresolved: &mut Vec<String>| -> Option<u32> {
            records
                .registry
                .get(player_id)
                .map(|(_, incarnation)| *incarnation)
                .or_else(|| {
                    unresolved.push(player_id.to_string());
                    None
                })
        };
    let mut views: BTreeMap<String, ChurnView> = BTreeMap::new();
    let mut unresolved: Vec<String> = Vec::new();
    for (viewer, room) in roster {
        let view = views.entry(viewer.clone()).or_default();
        // Seed: every co-room member starts in epoch 1 (the initial join).
        for (member, _) in roster
            .iter()
            .filter(|(_, member_room)| *member_room == *room)
        {
            view.adopted.entry(member.clone()).or_default().insert(1);
        }
        let mut events: Vec<&crate::records::ChurnEvent> = records
            .churn
            .iter()
            .filter(|event| room_of.get(event.recipient.as_str()) == Some(room))
            .collect();
        events.sort_by_key(|event| (event.at_us, event.recipient != *viewer));
        for event in events {
            let crate::records::ChurnEvent {
                recipient,
                phase,
                at_us,
                epoch,
                tails,
            } = event;
            match phase {
                ChurnPhase::Disconnect => {}
                ChurnPhase::Rejoined if recipient != viewer => {
                    // Another member rejoined: adopt its new epoch and close
                    // its previous epoch's stream.
                    let Some(new_epoch) = epoch else {
                        continue;
                    };
                    let Some(epochs) = view.adopted.get_mut(recipient) else {
                        continue;
                    };
                    for old in epochs
                        .iter()
                        .filter(|old| **old != *new_epoch)
                        .copied()
                        .collect::<Vec<_>>()
                    {
                        view.closed_at
                            .entry((recipient.clone(), old))
                            .or_insert(*at_us);
                    }
                    epochs.insert(*new_epoch);
                }
                ChurnPhase::Rejoined => {
                    // This viewer rejoined. The snapshot is the authority on
                    // every member it names: adopt the named incarnation and
                    // floor the stream at its tail. Members it omits are not
                    // closed — see the function doc — they receive a
                    // derived floor over every adopted stream instead.
                    let adopted_members: Vec<String> = view.adopted.keys().cloned().collect();
                    for member in &adopted_members {
                        match tails.get(member) {
                            Some(&(ref player_id, tail)) => {
                                let Some(snap_epoch) = resolve(records, player_id, &mut unresolved)
                                else {
                                    continue;
                                };
                                let epochs = view.adopted.entry(member.clone()).or_default();
                                for old in epochs
                                    .iter()
                                    .filter(|old| **old != snap_epoch)
                                    .copied()
                                    .collect::<Vec<_>>()
                                {
                                    view.closed_at
                                        .entry((member.clone(), old))
                                        .or_insert(*at_us);
                                }
                                epochs.insert(snap_epoch);
                                view.floors
                                    .insert((member.clone(), snap_epoch), (tail, *at_us, true));
                            }
                            None => {
                                // Absent from the snapshot: floor every
                                // adopted stream at the sends that completed
                                // at or before this rejoin.
                                if let Some(epochs) = view.adopted.get(member).cloned() {
                                    for old in epochs {
                                        let completed = blocks
                                            .get(&(member.as_str(), old))
                                            .map(|block| block.completed_by(*at_us))
                                            .unwrap_or(0);
                                        let completed =
                                            u64::try_from(completed).unwrap_or(u64::MAX);
                                        view.floors.insert(
                                            (member.clone(), old),
                                            (completed, *at_us, false),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    (views, unresolved)
}

/// Validate every raw event before delivery filtering, including duplicates,
/// misroutes, warm-up records, and receipt keys absent from the send ledger.
fn payload_size_evidence(
    records: &RunRecords,
    expected_application_bytes: u32,
    reasons: &mut Vec<InvalidReason>,
) -> PayloadBytes {
    let sent_by_key = SendIndex::new(&records.sent);
    let mut evidence = PayloadBytes::default();
    let mut mismatches = (0_u64, None);
    let mut invalid_bodies = (0_u64, None);
    let mut overflows = (0_u64, None);
    let mut check = |direction: &str,
                     sender: Option<&str>,
                     recipient: Option<&str>,
                     seq: Option<u64>,
                     application: u64,
                     encoded: u64,
                     overflow: bool| {
        let violation = || PayloadSizeViolation {
            direction: direction.to_string(),
            sender: sender.map(str::to_string),
            recipient: recipient.map(str::to_string),
            seq,
            application_bytes: application,
            encoded_frame_body_bytes: encoded,
            expected_application_bytes,
        };
        if overflow {
            overflows.0 += 1;
            overflows.1.get_or_insert_with(violation);
        }
        if application != u64::from(expected_application_bytes) {
            mismatches.0 += 1;
            mismatches.1.get_or_insert_with(violation);
        }
        // Current formats contain the full application value in their body.
        // Do not infer an envelope size: actual encoded sizes are raw evidence.
        if encoded < application || encoded == 0 {
            invalid_bodies.0 += 1;
            invalid_bodies.1.get_or_insert_with(violation);
        }
    };
    for sent in &records.sent {
        let phase = match sent.phase {
            Phase::Warmup => &mut evidence.warmup,
            Phase::Measured => &mut evidence.measured,
        };
        let overflow = phase
            .ingress
            .record(sent.application_bytes, sent.encoded_frame_body_bytes);
        check(
            "ingress",
            Some(&sent.sender),
            None,
            Some(sent.seq),
            sent.application_bytes,
            sent.encoded_frame_body_bytes,
            overflow,
        );
    }
    for receipt in &records.receipts {
        let direction = match sent_by_key
            .get(receipt.sender.as_str(), receipt.seq)
            .map(|sent| sent.phase)
        {
            Some(Phase::Warmup) => &mut evidence.warmup.egress,
            Some(Phase::Measured) => &mut evidence.measured.egress,
            None => &mut evidence.unmatched_egress,
        };
        let overflow =
            direction.record(receipt.application_bytes, receipt.encoded_frame_body_bytes);
        check(
            "egress",
            Some(&receipt.sender),
            Some(&receipt.recipient),
            Some(receipt.seq),
            receipt.application_bytes,
            receipt.encoded_frame_body_bytes,
            overflow,
        );
    }
    // Unidentified deliveries are structured raw faults rather than fabricated
    // receipt keys. They still contribute observed bytes and size validation.
    for fault in &records.faults {
        if let InvalidReason::UnidentifiedGameData {
            recipient,
            application_bytes,
            encoded_frame_body_bytes,
            ..
        } = fault
        {
            let overflow = evidence
                .unmatched_egress
                .record(*application_bytes, *encoded_frame_body_bytes);
            check(
                "egress",
                None,
                Some(recipient),
                None,
                *application_bytes,
                *encoded_frame_body_bytes,
                overflow,
            );
        }
    }
    if let Some(first) = overflows.1 {
        reasons.push(InvalidReason::PayloadByteTotalOverflow {
            count: overflows.0,
            first,
        });
    }
    if let Some(first) = mismatches.1 {
        reasons.push(InvalidReason::PayloadSizeMismatch {
            count: mismatches.0,
            first,
        });
    }
    if let Some(first) = invalid_bodies.1 {
        reasons.push(InvalidReason::InvalidEncodedFrameBodySize {
            count: invalid_bodies.0,
            first,
        });
    }
    evidence
}

/// Summarize one run: the single source of the outcome summary.
///
/// Every input comes from the recorded events: faults the runner observed
/// are `records.faults`, so a replay over the same events reproduces the
/// verdict exactly. A declared slow-reader hook exempts its named recipients
/// from the missing/outstanding verdict — their deficits stay visible in
/// `per_recipient` and `totals`, but the run is by declaration not a
/// capacity measurement.
///
/// The delivery class selects the omissions contract. Reliable delivery
/// permits no omission: every gap report is a violation. Latest and
/// volatile delivery permit policy loss, but every omitted sequence must be
/// covered by exactly one exact gap report with a reason the class can
/// produce — a hole without its report is `MissingDeliveries`, and any
/// overlap or out-of-range coverage is an `InvalidGapReports` violation.
///
/// The unsupported-format experiment overlays the reliable contract with
/// the cross-format refusal family: every stream FROM the room's opaque
/// sender expects no payload at any recipient — every omitted sequence
/// must arrive as an exact `unsupported_format` gap report — while every
/// text stream stays exactly-once. Advisory notices are permitted evidence
/// at the per-sender rate the server enforces.
#[allow(clippy::too_many_arguments)] // Replay inputs remain explicit and independent.
pub fn summarize(
    plans: &[SenderPlan],
    roster: &[(String, u32)],
    records: &RunRecords,
    generator_lag_bound_us: u64,
    expected_application_bytes: u32,
    delivery_class: DeliveryClass,
    churn: &ChurnPlan,
    experiment: Option<Experiment>,
) -> OutcomeSummary {
    let mut reasons = records.faults.clone();
    let payload_bytes = payload_size_evidence(records, expected_application_bytes, &mut reasons);
    if !records.join_failures.is_empty() {
        reasons.push(InvalidReason::JoinFailed {
            failures: records.join_failures.clone(),
        });
    }
    // A churn run must show its storm: every planned victimization needs its
    // rejoin half recorded (a wave run plans one cycle per wave, so a peer
    // its room keeps getting replaced owes one rejoin per wave). A silent
    // no-op storm — or a disconnect whose rejoin half never ran — would
    // mislabel the run as churn evidence while the new incarnation's
    // streams go unvalidated.
    let mut required_rejoins: BTreeMap<&str, usize> = BTreeMap::new();
    for cycle in &churn.cycles {
        for peer in &cycle.peers {
            *required_rejoins.entry(peer.as_str()).or_default() += 1;
        }
    }
    for (peer, required) in &required_rejoins {
        let rejoins = count_u64(
            records
                .churn
                .iter()
                .filter(|event| event.recipient == *peer && event.phase == ChurnPhase::Rejoined)
                .count(),
        );
        if rejoins < count_u64(*required) {
            reasons.push(InvalidReason::ChurnNotPerformed {
                peer: (*peer).to_string(),
            });
        }
    }
    let hook_exemptions: Vec<String> = reasons
        .iter()
        .filter_map(|reason| match reason {
            InvalidReason::SlowConsumerDisconnect { recipients } => Some(recipients.clone()),
            _ => None,
        })
        .flatten()
        .collect();

    let total_scheduled: u64 = plans.iter().map(|plan| count_u64(plan.sends.len())).sum();
    let total_sent: u64 = count_u64(records.sent.len());
    let completed = scheduled_send_evidence(plans, records, &mut reasons);
    let total_unsent = total_scheduled.saturating_sub(completed);

    let (max_lag, lag_p99) = generator_lag_stats(&records.sent);
    if max_lag > generator_lag_bound_us {
        reasons.push(InvalidReason::GeneratorSaturated {
            max_lag_us: max_lag,
            bound_us: generator_lag_bound_us,
        });
    }

    // Per-sender relay blocks: every incarnation the sender actually sent
    // in, with its ledger sequences in send order. The server stamps each
    // incarnation's stream 1..=n in arrival order, so a wire
    // `(incarnation, server_seq)` stamp is in range exactly when
    // `1 <= server_seq <= block.len()` — the bound the misroute checks
    // enforce; the ledger position (`block[server_seq - 1]`) pairs the
    // stream coordinates with the sender-stamped ledger sequence.
    let blocks = relay_blocks(&records.sent);
    let sender_epochs: BTreeMap<&str, BTreeSet<u32>> =
        blocks
            .keys()
            .fold(BTreeMap::new(), |mut acc, (sender, epoch)| {
                acc.entry(*sender).or_default().insert(*epoch);
                acc
            });

    let disconnected: BTreeSet<&str> = records
        .disconnects
        .iter()
        .map(|event| event.recipient.as_str())
        .collect();

    // Roster lookups: peer -> room, room -> members.
    let mut members_of_room: BTreeMap<u32, Vec<&str>> = BTreeMap::new();
    for (name, room) in roster {
        members_of_room
            .entry(*room)
            .or_default()
            .push(name.as_str());
    }

    // The experiment's opaque senders: peer 0 of every room (a pure
    // function of the deterministic shape). Its binary stream is the
    // unsupported-conversion family's source; every co-room peer is a
    // cross-format recipient of it, and every other stream is text.
    let opaque_senders: BTreeSet<&str> = match experiment {
        Some(Experiment::UnsupportedFormat) => plans
            .iter()
            .filter(|plan| plan.player == 0)
            .map(|plan| plan.name.as_str())
            .collect(),
        // A new experiment variant opts in here explicitly; until it does,
        // it has no opaque senders.
        None => BTreeSet::new(),
    };

    // Per-recipient churn view: how each co-room member's relay stream
    // evolved across the storm. Every member starts in epoch 1 (the initial
    // join); a member's rejoin adopts its new epoch, and the recipient's own
    // rejoin re-adopts every member from its snapshot — where the snapshot's
    // per-member `(epoch, seq tail)` raises the stream's owed floor ("a
    // recipient owes no GameData at or below this sequence in the paired
    // epoch"), covering the away window loudly.
    let (views, unresolved) = churn_views(records, roster, &blocks);
    for player_id in unresolved {
        reasons.push(InvalidReason::UnresolvedSenderIdentity { player_id });
    }

    // Validate arrivals once, retaining exact compact coverage per stream.
    // Raw records remain unchanged for replay and latency accounting.
    let member_names: BTreeSet<&str> = roster.iter().map(|(name, _)| name.as_str()).collect();
    let room_of: BTreeMap<&str, u32> = roster
        .iter()
        .map(|(name, room)| (name.as_str(), *room))
        .collect();
    let mut unknown_recipients = Category::default();
    let mut arrivals: BTreeMap<ArrivalKey<'_>, ArrivalEvidence> = BTreeMap::new();
    for receipt in &records.receipts {
        let recipient = receipt.recipient.as_str();
        let sender = receipt.sender.as_str();
        let epoch = receipt.epoch;
        if !member_names.contains(recipient) {
            unknown_recipients.record(DeliveryKey {
                recipient: receipt.recipient.to_string(),
                sender: receipt.sender.to_string(),
                epoch,
                seq: receipt.server_seq,
            });
            continue;
        }
        let stream = arrivals
            .entry((recipient, sender, epoch))
            .or_insert_with(|| {
                let contract = if opaque_senders.contains(sender)
                    || recipient == sender
                    || !room_of.contains_key(sender)
                    || room_of.get(sender) != room_of.get(recipient)
                {
                    None
                } else {
                    let view = views.get(recipient).expect("roster peer has a churn view");
                    let stream_key = (receipt.sender.to_string(), epoch);
                    Some(ArrivalContract {
                        closed_after: view.closed_at.get(&stream_key).copied(),
                        floor: view.floors.get(&stream_key).copied(),
                    })
                };
                ArrivalEvidence {
                    contract,
                    ..ArrivalEvidence::default()
                }
            });
        stream.total += 1;
        stream.first.get_or_insert(receipt.server_seq);
        // Opaque leaks still owe overlap, duplicate, and order evidence.
        stream.observe(receipt.server_seq);
        let Some(contract) = stream.contract else {
            continue;
        };
        let stale = contract
            .closed_after
            .is_some_and(|at_us| receipt.received_us > at_us);
        let below_floor = contract.floor.is_some_and(|(floor, from_us, enforced)| {
            enforced && receipt.received_us >= from_us && receipt.server_seq <= floor
        });
        let ledger_matches = receipt
            .server_seq
            .checked_sub(1)
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| blocks.get(&(sender, epoch))?.get(index))
            .is_some_and(|event| event.seq == receipt.seq);
        if !ledger_matches || stale || below_floor {
            stream.misrouted.record(receipt.server_seq);
        }
    }

    // Gap reports: global contract validation, then per-stream coverage.
    // Server-stamped sequences are 1-based within one `(sender, epoch)`
    // stream; the runner's ledger sequences are 0-based over that same
    // stream's sends, so the block built above maps between them.
    let reasons_permitted = |class: DeliveryClass,
                             reason: signal_fish_server::protocol::DeliveryGapReason,
                             opaque_pair: bool| {
        if opaque_pair {
            // The cross-format refusal family's one reason: an opaque
            // payload was withheld from this recipient and accounted.
            return reason == signal_fish_server::protocol::DeliveryGapReason::UnsupportedFormat;
        }
        match class {
            DeliveryClass::Reliable => false,
            DeliveryClass::Latest => matches!(
                reason,
                signal_fish_server::protocol::DeliveryGapReason::LatestSuperseded
                    | signal_fish_server::protocol::DeliveryGapReason::LatestDroppedFull
            ),
            DeliveryClass::Volatile => matches!(
                reason,
                signal_fish_server::protocol::DeliveryGapReason::VolatileDropped
            ),
        }
    };
    let mut invalid_gaps = GapViolations::default();
    let mut gaps_by_stream: BTreeMap<(&str, &str, u32), Vec<&GapEvent>> = BTreeMap::new();
    for gap in &records.gaps {
        let mut detail: Option<String> = None;
        // The experiment's cross-format pair (opaque sender -> any co-room
        // peer): gap reports are the contract, not a violation.
        let opaque_pair = opaque_senders.contains(gap.sender.as_str());
        if !member_names.contains(gap.recipient.as_str())
            || !member_names.contains(gap.sender.as_str())
        {
            detail = Some("gap references a peer outside the run roster".to_string());
        } else if gap.recipient == gap.sender {
            detail = Some("gap names the recipient as its own sender".to_string());
        } else if room_of.get(gap.recipient.as_str()) != room_of.get(gap.sender.as_str()) {
            detail = Some("gap names a sender from another room".to_string());
        } else if delivery_class == DeliveryClass::Reliable && !opaque_pair {
            detail =
                Some("reliable delivery permits no loss, yet a gap report arrived".to_string());
        } else if !sender_epochs
            .get(gap.sender.as_str())
            .is_some_and(|epochs| epochs.contains(&gap.epoch))
        {
            detail = Some(format!(
                "gap names epoch {} but the sender never sent in it",
                gap.epoch
            ));
        } else if !reasons_permitted(delivery_class, gap.reason, opaque_pair) {
            detail = Some(if opaque_pair {
                format!(
                    "an opaque stream's omission must carry the unsupported_format reason, \
                     got {reason:?}",
                    reason = gap.reason
                )
            } else {
                format!(
                    "reason {reason:?} is not a {class} loss reason",
                    reason = gap.reason,
                    class = match delivery_class {
                        DeliveryClass::Latest => "latest",
                        DeliveryClass::Volatile => "volatile",
                        DeliveryClass::Reliable => "reliable",
                    }
                )
            });
        } else if gap.from_seq == 0 {
            detail = Some("gap range reaches below the first server sequence".to_string());
        } else if gap.from_seq > gap.to_seq {
            detail = Some(format!(
                "gap range runs backwards ({}:{})",
                gap.from_seq, gap.to_seq
            ));
        }
        if let Some(detail) = detail {
            invalid_gaps.record_violation(gap, detail);
            continue;
        }
        gaps_by_stream
            .entry((gap.recipient.as_str(), gap.sender.as_str(), gap.epoch))
            .or_default()
            .push(gap);
    }

    let mut duplicates = Category::default();
    let mut misrouted = unknown_recipients;
    let mut out_of_order = Category::default();
    let mut missing = Category::default();
    let mut leaked = Category::default();

    let mut per_recipient = Vec::new();
    for (recipient, room) in roster {
        let view = views
            .get(recipient.as_str())
            .expect("every roster peer has a churn view");
        let connected_through = !disconnected.contains(recipient.as_str());
        let exempt = hook_exemptions.iter().any(|name| name == recipient);
        let mut outcome = RecipientOutcome {
            recipient: recipient.clone(),
            room: *room,
            connected_through,
            received: BTreeMap::new(),
            missing: 0,
            undelivered_at_disconnect: 0,
            gap_covered: 0,
            duplicates: 0,
            misrouted: 0,
            out_of_order: 0,
            latency_us: LatencyStats::default(),
        };

        let expected_senders: Vec<&str> = members_of_room
            .get(room)
            .map(|members| {
                members
                    .iter()
                    .copied()
                    .filter(|name| *name != recipient.as_str())
                    .collect()
            })
            .unwrap_or_default();

        // (a) Arrival-stream validity: an arrival must come from a co-room
        // sender, carry a stream sequence the sender actually stamped in
        // that epoch, not outlive its stream (the sender rejoined past it),
        // and never repeat what the rejoin snapshot already accounted.
        let mut received_by_sender: BTreeMap<&str, u64> = BTreeMap::new();
        for ((_, sender, epoch), stream) in recipient_arrivals(&arrivals, recipient) {
            if opaque_senders.contains(sender) {
                // The unsupported-conversion contract: the opaque sender's
                // payload reaches NO recipient as a delivery. An arrival is
                // the leak class — a conversion (lossy or lucky) happened
                // that the server must refuse. Counted only in the leak
                // category: its verdict names the recipient and stream
                // exactly, and a leak already invalidates the run.
                let seq = stream.first.unwrap_or(0);
                leaked.record(DeliveryKey {
                    recipient: recipient.clone(),
                    sender: (*sender).to_string(),
                    epoch: *epoch,
                    seq,
                });
                continue;
            }
            if !expected_senders.contains(sender) {
                let seq = stream.first.unwrap_or(0);
                outcome.misrouted += stream.total;
                misrouted.record(DeliveryKey {
                    recipient: recipient.clone(),
                    sender: (*sender).to_string(),
                    epoch: *epoch,
                    seq,
                });
                continue;
            }
            // Closed-stream and invalid-sequence arrivals still count as evidence.
            *received_by_sender.entry(sender).or_insert(0) += stream.unique;
            outcome.misrouted += stream.misrouted.count;
            stream
                .misrouted
                .append_to(&mut misrouted, recipient, sender, *epoch);
        }

        // (b) Per-owed-stream completeness: duplicates, arrival order, and
        // the class's omission contract, over every `(sender, epoch)` stream
        // this recipient's seat ever adopted.
        for sender in expected_senders {
            let Some(adopted) = view.adopted.get(sender) else {
                continue;
            };
            for epoch in adopted {
                // A closed stream is finished: its sender rejoined past it
                // (or the viewer's snapshot superseded it), so nothing
                // further is owed. Arrivals there are stale-epoch
                // misroutes, caught by the arrival scan above.
                if view.closed_at.contains_key(&(sender.to_string(), *epoch)) {
                    continue;
                }
                let Some(block) = blocks.get(&(sender, *epoch)) else {
                    // Adopted, but the sender never got to send in this
                    // epoch (it rejoined before its first send): nothing is
                    // owed, so there is nothing to check.
                    continue;
                };
                let total_owed = count_u64(block.len());
                let arrival = arrivals
                    .remove(&(recipient.as_str(), sender, *epoch))
                    .unwrap_or_default();
                outcome.duplicates += arrival.duplicates.count;
                arrival
                    .duplicates
                    .append_to(&mut duplicates, recipient, sender, *epoch);
                outcome.out_of_order += arrival.out_of_order.count;
                arrival
                    .out_of_order
                    .append_to(&mut out_of_order, recipient, sender, *epoch);

                // The stream's rejoin floor: from this instant on, every
                // sequence at or below the snapshot tail is not owed (the
                // loud away window), and deliveries must resume exactly one
                // past it.
                let (owed_floor, _floor_from_us, _enforced) = view
                    .floors
                    .get(&((*sender).to_string(), *epoch))
                    .copied()
                    .unwrap_or((0, 0, false));

                // Coverage model, shared by every class over the stream's
                // owed window (above the rejoin floor, bounded by what the
                // sender sent in this epoch). Delivered unique sequences are
                // covered; the lossy classes may additionally cover an
                // omission only with its exact gap report, while reliable
                // permits no gap report at all (validated globally above) —
                // so its coverage is receipts alone.
                let stream_gaps = match delivery_class {
                    DeliveryClass::Reliable if !opaque_senders.contains(sender) => &[][..],
                    _ => gaps_by_stream
                        .get(&(recipient.as_str(), sender, *epoch))
                        .map_or(&[][..], Vec::as_slice),
                };
                let mut covered = arrival.observed.covered;
                for gap in stream_gaps {
                    if gap.to_seq > total_owed {
                        invalid_gaps.record_violation(
                            gap,
                            format!(
                                "gap range reaches beyond what the sender sent ({} > \
                                 {total_owed} relayed in epoch {epoch})",
                                gap.to_seq
                            ),
                        );
                        continue;
                    }
                    // Reject BEFORE inserting: a rejected range contributes
                    // no coverage, so its non-overlapping remainder stays an
                    // honest uncovered omission in the totals.
                    if !covered.insert(gap.from_seq, gap.to_seq) {
                        invalid_gaps.record_violation(
                            gap,
                            format!(
                                "gap range {}:{} overlaps an already-covered sequence",
                                gap.from_seq, gap.to_seq
                            ),
                        );
                        continue;
                    }
                    outcome.gap_covered += gap.to_seq - gap.from_seq + 1;
                }
                // Count covered positions and holes above the seat's floor.
                let accounting = covered.above(owed_floor);
                let owed_total = total_owed.saturating_sub(owed_floor);
                let holes = accounting.holes;
                let first_hole = accounting.first_hole;
                let deficit = owed_total
                    .saturating_sub(accounting.covered)
                    .saturating_sub(holes);
                if holes > 0 {
                    outcome.missing += holes;
                    if !exempt {
                        missing.record(DeliveryKey {
                            recipient: recipient.clone(),
                            sender: sender.to_string(),
                            epoch: *epoch,
                            seq: first_hole.unwrap_or(owed_floor + 1),
                        });
                    }
                }
                if connected_through && deficit > 0 {
                    outcome.missing += deficit;
                    if !exempt {
                        missing.record(DeliveryKey {
                            recipient: recipient.clone(),
                            sender: sender.to_string(),
                            epoch: *epoch,
                            seq: accounting.highest + 1,
                        });
                    }
                } else if !connected_through {
                    outcome.undelivered_at_disconnect += deficit;
                }
            }
        }
        outcome.received = received_by_sender
            .iter()
            .map(|(sender, count)| ((*sender).to_string(), *count))
            .collect();

        per_recipient.push(outcome);
    }

    // Per-recipient latency tails: one starved room must be visible in the
    // summary, not smoothed away by the run aggregate.
    let mut latency = LatencyAccumulator::default();
    let mut latency_by_recipient: BTreeMap<&str, LatencyAccumulator> = BTreeMap::new();
    for (recipient, sample) in latency_pairs(records) {
        latency.record(sample);
        // Misrouted receipts remain in the aggregate. Allocate recipient
        // buckets only for roster peers with measured receipts.
        if let Some(&recipient) = member_names.get(recipient) {
            latency_by_recipient
                .entry(recipient)
                .or_default()
                .record(sample);
        }
    }
    for outcome in &mut per_recipient {
        outcome.latency_us = latency_by_recipient
            .get(outcome.recipient.as_str())
            .map(LatencyAccumulator::stats)
            .unwrap_or_default();
    }

    if let Some(reason) =
        missing.into_reason(|count, first| InvalidReason::MissingDeliveries { count, first })
    {
        reasons.push(reason);
    }
    if let Some(reason) = invalid_gaps.into_reason() {
        reasons.push(reason);
    }
    if let Some(reason) =
        duplicates.into_reason(|count, first| InvalidReason::DuplicateDeliveries { count, first })
    {
        reasons.push(reason);
    }
    if let Some(reason) =
        misrouted.into_reason(|count, first| InvalidReason::MisroutedDeliveries { count, first })
    {
        reasons.push(reason);
    }
    if let Some(reason) = out_of_order
        .into_reason(|count, first| InvalidReason::OutOfOrderDeliveries { count, first })
    {
        reasons.push(reason);
    }
    if let Some(reason) =
        leaked.into_reason(|count, first| InvalidReason::UnsupportedFormatLeak { count, first })
    {
        reasons.push(reason);
    }

    // Unsupported-format advisory cadence: the server emits at most one
    // notice per (recipient, opaque sender) per second, so the run's span
    // bounds how many can legitimately appear at one recipient. The margin
    // is the limiter's shape made explicit: the FIRST notice for a sender
    // is immediate (1), subsequent notices are >= 1 s apart (span/1s upper
    // bound), plus one boundary slot and one drain slot for a final notice
    // flushed while the recipient drains (the drain grace the config
    // carries). Anything above the bound is the flood class, not evidence.
    if !opaque_senders.is_empty() {
        let span_us = records
            .sent
            .iter()
            .map(|sent| sent.sent_us)
            .max()
            .unwrap_or(0);
        // 1 (the immediate first notice) + the span's per-second cadence
        // ceiling + 1 boundary slot + 1 drain slot.
        let bound_per_opaque = 1 + span_us.div_ceil(1_000_000) + 1 + 1;
        let mut opaque_per_room: BTreeMap<u32, u64> = BTreeMap::new();
        for plan in plans {
            if opaque_senders.contains(plan.name.as_str()) {
                *opaque_per_room.entry(plan.room).or_default() += 1;
            }
        }
        for (recipient, room) in roster {
            let count = count_u64(
                records
                    .unsupported_notices
                    .iter()
                    .filter(|notice| notice.recipient == *recipient)
                    .count(),
            );
            let bound = opaque_per_room.get(room).copied().unwrap_or(0) * bound_per_opaque;
            if count > bound {
                reasons.push(InvalidReason::UnsupportedNoticeFlood {
                    recipient: recipient.clone(),
                    count,
                    bound,
                });
            }
        }
    }

    // Unsent work with no explanatory fault is a generator bookkeeping bug:
    // every sender exit path must record why it stopped.
    let fault_explains_unsent = reasons.iter().any(|reason| {
        matches!(
            reason,
            InvalidReason::ServerTerminated
                | InvalidReason::GeneratorSaturated { .. }
                | InvalidReason::SendFailed { .. }
                | InvalidReason::JoinFailed { .. }
                | InvalidReason::RunnerDeadlineExceeded { .. }
                | InvalidReason::ReconnectFailed { .. }
        )
    });
    if total_unsent > 0 && !fault_explains_unsent {
        reasons.push(InvalidReason::UnsentWork {
            count: total_unsent,
        });
    }

    let total_outstanding_all: u64 = per_recipient.iter().map(|outcome| outcome.missing).sum();
    let total_gap_covered: u64 = per_recipient
        .iter()
        .map(|outcome| outcome.gap_covered)
        .sum();
    let outstanding_for_verdict: u64 = per_recipient
        .iter()
        .filter(|outcome| {
            !hook_exemptions
                .iter()
                .any(|name| name == &outcome.recipient)
        })
        .map(|outcome| outcome.missing)
        .sum();
    if outstanding_for_verdict > 0 {
        reasons.push(InvalidReason::OutstandingAtEnd {
            count: outstanding_for_verdict,
        });
    }

    // Observed disconnects are faults unless the run itself declared the
    // server terminated (then every stream ending is the expected
    // consequence, with prefixes still enforced above).
    let declared_terminated = reasons
        .iter()
        .any(|reason| matches!(reason, InvalidReason::ServerTerminated));
    if !declared_terminated && !records.disconnects.is_empty() {
        reasons.push(InvalidReason::UnexpectedDisconnect {
            recipients: records
                .disconnects
                .iter()
                .map(|event| event.recipient.clone())
                .collect(),
        });
    }

    OutcomeSummary {
        valid: reasons.is_empty(),
        reasons,
        totals: Totals {
            scheduled: total_scheduled,
            sent: total_sent,
            unsent: total_unsent,
            receipts: count_u64(records.receipts.len()),
            outstanding: total_outstanding_all,
            gap_covered: total_gap_covered,
        },
        per_recipient,
        latency_us: latency.stats(),
        generator_lag_us: LagStats {
            max_us: max_lag,
            p99_us: lag_p99,
            bound_us: generator_lag_bound_us,
        },
        payload_bytes,
        experiment: experiment.map(Experiment::label).map(str::to_string),
        unsupported_notices: count_u64(records.unsupported_notices.len()),
    }
}

/// Counts and the first offending sequence preserve the category's exact key.
#[derive(Default)]
struct SequenceViolations {
    count: u64,
    first: Option<u64>,
}

impl SequenceViolations {
    fn record(&mut self, sequence: u64) {
        self.count += 1;
        self.first.get_or_insert(sequence);
    }

    fn append_to(&self, category: &mut Category, recipient: &str, sender: &str, epoch: u32) {
        category.count += self.count;
        if let Some(seq) = self.first {
            category.first.get_or_insert_with(|| DeliveryKey {
                recipient: recipient.to_string(),
                sender: sender.to_string(),
                epoch,
                seq,
            });
        }
    }
}

/// Stream boundaries are resolved once, without cloning a sender per receipt.
#[derive(Clone, Copy)]
struct ArrivalContract {
    closed_after: Option<u64>,
    floor: Option<(u64, u64, bool)>,
}

type ArrivalKey<'a> = (&'a str, &'a str, u32);

/// Seek directly to one recipient's streams. The empty sender and epoch zero
/// include malformed names and epochs, which must remain delivery evidence.
fn recipient_arrivals<'map, 'records>(
    arrivals: &'map BTreeMap<ArrivalKey<'records>, ArrivalEvidence>,
    recipient: &'records str,
) -> impl Iterator<Item = (&'map ArrivalKey<'records>, &'map ArrivalEvidence)> {
    arrivals
        .range((recipient, "", 0)..)
        .take_while(move |((name, _, _), _)| *name == recipient)
}

/// Ordered complete streams retain one range, rather than one receipt pointer.
#[derive(Default)]
struct ArrivalEvidence {
    contract: Option<ArrivalContract>,
    total: u64,
    first: Option<u64>,
    unique: u64,
    observed: ArrivalCoverage,
    duplicates: SequenceViolations,
    out_of_order: SequenceViolations,
    misrouted: SequenceViolations,
}

impl ArrivalEvidence {
    fn observe(&mut self, sequence: u64) {
        match self.observed.observe(sequence) {
            None => self.duplicates.record(sequence),
            Some(out_of_order) => {
                self.unique += 1;
                if out_of_order {
                    self.out_of_order.record(sequence);
                }
            }
        }
    }
}

/// Exact distinct-arrival and ordering state without a per-receipt copy.
/// Zero remains observable for duplicate/order checks even though it cannot
/// cover an owed (one-based) server sequence.
#[derive(Default)]
struct ArrivalCoverage {
    covered: StreamCoverage,
    zero_seen: bool,
    previous: Option<u64>,
}

impl ArrivalCoverage {
    /// None is a duplicate; Some(true) is a first arrival out of order.
    fn observe(&mut self, sequence: u64) -> Option<bool> {
        let fresh = if sequence == 0 {
            !std::mem::replace(&mut self.zero_seen, true)
        } else {
            self.covered.insert(sequence, sequence)
        };
        if !fresh {
            return None;
        }
        let out_of_order = self.previous.is_some_and(|previous| sequence <= previous);
        self.previous = Some(sequence);
        Some(out_of_order)
    }
}

/// Disjoint inclusive ranges for one recipient stream. Adjacent ranges merge,
/// so a compact gap report never expands into one allocation per omitted send.
/// Reject an overlapping report in full before changing coverage.
#[derive(Default)]
struct StreamCoverage {
    // Ordered deliveries and compact gap reports need no tree allocation.
    inline: Option<(u64, u64)>,
    ranges: BTreeMap<u64, u64>,
}

#[derive(Debug, PartialEq, Eq)]
struct CoverageAccounting {
    covered: u64,
    holes: u64,
    first_hole: Option<u64>,
    highest: u64,
}

impl StreamCoverage {
    fn insert(&mut self, from: u64, mut to: u64) -> bool {
        if from == 0 || from > to {
            return false;
        }
        if self.ranges.is_empty() {
            let Some((start, end)) = self.inline else {
                self.inline = Some((from, to));
                return true;
            };
            if from <= end && to >= start {
                return false;
            }
            if end.checked_add(1) == Some(from) {
                self.inline = Some((start, to));
            } else if to.checked_add(1) == Some(start) {
                self.inline = Some((from, end));
            } else {
                self.inline = None;
                self.ranges.insert(start, end);
                self.ranges.insert(from, to);
            }
            return true;
        }
        // The last range beginning at or below `to` is the only candidate
        // needed to prove overlap in this disjoint, ordered map.
        if self
            .ranges
            .range(..=to)
            .next_back()
            .is_some_and(|(_, end)| *end >= from)
        {
            return false;
        }
        if let Some(next) = to.checked_add(1) {
            if let Some(end) = self.ranges.remove(&next) {
                to = end;
            }
        }
        // Extend an existing range in place without removing its key.
        if let Some((_, end)) = self.ranges.range_mut(..from).next_back() {
            if end.checked_add(1) == Some(from) {
                *end = to;
            } else {
                self.ranges.insert(from, to);
            }
        } else {
            self.ranges.insert(from, to);
        }
        if self.ranges.len() == 1 {
            self.inline = self.ranges.pop_first();
            // Keep the empty root to reuse it across fragment/repair cycles.
        }
        true
    }

    fn above(&self, floor: u64) -> CoverageAccounting {
        let mut result = CoverageAccounting {
            covered: 0,
            holes: 0,
            first_hole: None,
            highest: floor,
        };
        for (start, end) in self
            .inline
            .iter()
            .copied()
            .chain(self.ranges.iter().map(|(&start, &end)| (start, end)))
        {
            if end <= floor {
                continue;
            }
            // end > floor proves floor + 1 fits the run's sequence space.
            let start = start.max(floor + 1);
            let hole = start - result.highest - 1;
            if hole > 0 {
                result.holes += hole;
                result.first_hole.get_or_insert(result.highest + 1);
            }
            result.covered += end - start + 1;
            result.highest = end;
        }
        result
    }

    #[cfg(test)]
    fn stored_ranges(&self) -> usize {
        self.ranges.len() + usize::from(self.inline.is_some())
    }
}

/// Exact maximum and nearest-rank p99 with fixed storage, including u64 stalls.
fn generator_lag_stats(sent: &[SentEvent]) -> (u64, u64) {
    let lag = |event: &SentEvent| event.sent_us.saturating_sub(event.intended_us);
    let mut minimum = u64::MAX;
    let mut maximum = 0;
    for event in sent {
        let value = lag(event);
        minimum = minimum.min(value);
        maximum = maximum.max(value);
    }
    if sent.is_empty() || minimum == maximum {
        return (maximum, maximum);
    }
    let mut rank = sent.len() - sent.len() / 100;
    let mut prefix = 0;
    let highest_byte = (u64::BITS - 1 - maximum.leading_zeros()) / 8;
    for byte in (0..=highest_byte).rev() {
        let shift = byte * 8;
        let mask = u64::MAX.checked_shl(shift + 8).unwrap_or(0);
        let mut buckets = [0_usize; 256];
        for event in sent {
            let value = lag(event);
            if value & mask == prefix {
                let digit =
                    usize::from(value.to_le_bytes()[usize::try_from(byte).expect("u64 byte")]);
                buckets[digit] += 1;
            }
        }
        for (digit, count) in buckets.into_iter().enumerate() {
            if rank > count {
                rank -= count;
            } else {
                prefix |= u64::try_from(digit).expect("byte digit") << shift;
                break;
            }
        }
    }
    (maximum, prefix)
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    #[test]
    fn bounded_lag_statistics_match_sorted_nearest_rank_for_full_u64_values() {
        let base = crate::complete_records(&crate::unit_context().plans).sent[0].clone();
        for count in [0, 1, 2, 99, 100, 101, 199, 200, 201, 1000] {
            for distribution in 0..7 {
                let sent = lag_probe_records(&base, count, distribution);
                let mut expected: Vec<_> = sent
                    .iter()
                    .map(|event| event.sent_us.saturating_sub(event.intended_us))
                    .collect();
                expected.sort_unstable();
                let maximum = expected.last().copied().unwrap_or(0);
                let percentile = if expected.is_empty() {
                    0
                } else {
                    expected[(99 * expected.len()).div_ceil(100) - 1]
                };
                assert_eq!(
                    generator_lag_stats(&sent),
                    (maximum, percentile),
                    "count={count}, distribution={distribution}"
                );
            }
        }
        for constant in [0, 5, u64::MAX] {
            let mut event = base.clone();
            event.intended_us = 0;
            event.sent_us = constant;
            assert_eq!(generator_lag_stats(&vec![event; 101]), (constant, constant));
        }
    }

    fn lag_probe_records(base: &SentEvent, count: usize, distribution: u8) -> Vec<SentEvent> {
        let mut state = 1_u64;
        (0..count)
            .map(|index| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let mut event = base.clone();
                event.intended_us = if index % 7 == 0 { u64::MAX } else { 0 };
                event.sent_us = match distribution {
                    0 => 5,
                    1 => u64::try_from(index).expect("sample index"),
                    2 => u64::try_from(count - index).expect("sample index"),
                    3 => state % 1_000_000,
                    4 => state,
                    5 => [
                        0,
                        u64::MAX,
                        1,
                        u64::MAX - 1,
                        1 << 63,
                        255,
                        256,
                        65535,
                        65536,
                    ][index % 9],
                    _ => 0x1234_ffff_0000 | (state % 65536),
                };
                event
            })
            .collect()
    }

    #[test]
    #[ignore = "exact lag CPU and memory probe; run in a fresh process"]
    fn generator_lag_statistics_profile() {
        let count = std::env::var("CAPACITY_LAG_PROBE_COUNT")
            .unwrap_or_else(|_| "1000000".into())
            .parse()
            .expect("sample count");
        assert!((1..=4_000_000).contains(&count));
        let distribution = std::env::var("CAPACITY_LAG_PROBE_DISTRIBUTION")
            .unwrap_or_else(|_| "3".into())
            .parse()
            .expect("sample distribution");
        let base = crate::complete_records(&crate::unit_context().plans).sent[0].clone();
        let sent = lag_probe_records(&base, count, distribution);
        let started = std::time::Instant::now();
        let stats = generator_lag_stats(&sent);
        let elapsed = started.elapsed();
        println!(
            "LAG_PROFILE {}",
            serde_json::json!({"count": count, "distribution": distribution, "maximum": stats.0, "p99": stats.1, "elapsed_us": elapsed.as_micros()})
        );
        std::hint::black_box(sent);
    }

    // #636: recipient lookup must retain exact stream order and all malformed
    // evidence while avoiding a scan of other recipients' streams.
    #[test]
    fn recipient_ranges_match_independent_full_scan() {
        let recipients = ["", "a", "a\0", "a0", "a1", "z", "未知", "\u{10ffff}"];
        let senders = ["", "a", "unknown", "未知", "\u{10ffff}"];
        let epochs = [0, 1, 2, u32::MAX];
        let mut arrivals = BTreeMap::new();
        for (index, recipient) in recipients.iter().enumerate().rev() {
            for sender in senders.into_iter().rev() {
                for epoch in epochs.into_iter().rev() {
                    let mut stream = ArrivalEvidence::default();
                    for seq in [u64::MAX, 0, 1, 1, 0] {
                        stream.observe(seq);
                    }
                    arrivals.insert((*recipient, sender, epoch), stream);
                }
            }
            // Compare after every insertion group, not just a full key space.
            for recipient in recipients.into_iter().chain(["absent", "a00", "未知x"]) {
                let expected = arrivals
                    .iter()
                    .filter(|((name, _, _), _)| *name == recipient)
                    .map(|(key, stream)| (*key, std::ptr::from_ref(stream)))
                    .collect::<Vec<_>>();
                let actual = recipient_arrivals(&arrivals, recipient)
                    .map(|(key, stream)| (*key, std::ptr::from_ref(stream)))
                    .collect::<Vec<_>>();
                assert_eq!(actual, expected, "recipient={recipient:?}, group={index}");
            }
        }
        assert_eq!(recipient_arrivals(&BTreeMap::new(), "").count(), 0);
        // Summarization removes owed streams as recipients are completed.
        // Retained stale and foreign streams must not affect the next lookup.
        arrivals.retain(|(_, sender, epoch), _| *sender == "unknown" || *epoch == 0);
        for recipient in recipients {
            let expected = arrivals
                .keys()
                .filter(|(name, _, _)| *name == recipient)
                .copied()
                .collect::<Vec<_>>();
            assert_eq!(
                recipient_arrivals(&arrivals, recipient)
                    .map(|(key, _)| *key)
                    .collect::<Vec<_>>(),
                expected,
                "remaining recipient={recipient:?}"
            );
        }
    }

    // Issue #775: canonical ledgers must not retain a pointer per send.
    #[test]
    fn relay_blocks_borrow_canonical_slices_across_epochs() {
        let context = crate::unit_context();
        let mut records = crate::complete_records(&context.plans);
        records
            .sent
            .sort_by_key(|event| (event.sender.clone(), event.seq));
        // Gapped and duplicate sequence values still borrow. No arithmetic
        // sequence assumption may discard malformed evidence.
        records.sent[0].seq = 0;
        records.sent[1].seq = 0;
        let first_sender = records.sent[0].sender.clone();
        for event in records
            .sent
            .iter_mut()
            .skip(2)
            .take_while(|event| event.sender == first_sender)
        {
            event.epoch = 2;
        }
        records
            .sent
            .iter_mut()
            .take_while(|event| event.sender == first_sender)
            .last()
            .unwrap()
            .seq = u64::MAX;
        // Live tasks append out of order. The collector sorts the final
        // ledger, and its exact JSONL replay must keep the borrowing path.
        let log = crate::records::EventLog::new();
        for event in records.sent.iter().rev() {
            log.push_sent(event.clone());
        }
        let collected = log.take_records();
        let output = tempfile::tempdir().expect("ledger artifacts");
        crate::artifacts::write_deliveries(output.path(), &collected).unwrap();
        let replayed = crate::artifacts::read_records(output.path()).unwrap();
        for records in [&records, &collected, &replayed] {
            let blocks = relay_blocks(&records.sent);
            for block in blocks.values() {
                assert!(matches!(block, RelayBlock::Borrowed(_)));
                for index in 0..block.len() {
                    assert!(records
                        .sent
                        .iter()
                        .any(|event| std::ptr::eq(event, block.get(index).unwrap())));
                }
                assert!(block.get(block.len()).is_none());
                assert!(block.get(usize::MAX).is_none());
            }
        }
        assert!(relay_blocks(&[]).is_empty());
    }

    #[test]
    fn relay_blocks_match_independent_sorted_ledgers_and_completion_counts() {
        let context = crate::unit_context();
        let base = crate::complete_records(&context.plans).sent[0].clone();
        // Exhaustively interleave senders, epochs, duplicate/gapped sequences,
        // and completion times that disagree with ledger order.
        for mut encoded in 0_u32..4096 {
            let mut sent = Vec::new();
            for index in 0..6 {
                let digit = encoded % 4;
                encoded /= 4;
                let mut event = base.clone();
                event.sender = if digit % 2 == 0 { "未知" } else { "other" }.into();
                event.epoch = digit / 2;
                event.seq = [0, 7, 7, u64::MAX][(index + digit as usize) % 4];
                event.sent_us = [u64::MAX, 10, 0][index % 3];
                sent.push(event);
            }
            let mut expected: BTreeMap<_, Vec<_>> = BTreeMap::new();
            for event in &sent {
                expected
                    .entry((event.sender.as_str(), event.epoch))
                    .or_default()
                    .push(event);
            }
            for events in expected.values_mut() {
                events.sort_by_key(|event| event.seq);
            }
            let actual = relay_blocks(&sent);
            assert_eq!(actual.len(), expected.len());
            for (key, events) in expected {
                let block = &actual[&key];
                assert_eq!(block.len(), events.len());
                for (index, event) in events.iter().enumerate() {
                    assert!(std::ptr::eq(block.get(index).unwrap(), *event));
                }
                for cutoff in [0, 9, 10, u64::MAX] {
                    assert_eq!(
                        block.completed_by(cutoff),
                        events
                            .iter()
                            .filter(|event| event.sent_us <= cutoff)
                            .count()
                    );
                }
                assert!(block.get(events.len()).is_none());
            }
        }
    }

    #[test]
    fn send_joins_preserve_duplicate_phase_latency_and_unmatched_receipts() {
        let context = crate::unit_context();
        let base = crate::complete_records(&context.plans);
        for phase in [Phase::Warmup, Phase::Measured] {
            for reverse in [false, true] {
                let mut records = base.clone();
                let mut duplicate = records.sent[0].clone();
                duplicate.phase = phase;
                duplicate.intended_us = 1;
                duplicate.epoch = 99; // Ledger joins intentionally ignore epoch.
                records.sent.push(duplicate);
                // Retain receipt evidence whose send is absent from the ledger.
                records.sent.retain(|sent| sent.seq != 1);
                if reverse {
                    records.sent.reverse();
                }
                let expected: BTreeMap<_, _> = records
                    .sent
                    .iter()
                    .map(|sent| ((sent.sender.as_str(), sent.seq), sent))
                    .collect();
                let expected_latency: Vec<_> = records
                    .receipts
                    .iter()
                    .filter_map(|receipt| {
                        let sent = expected.get(&(receipt.sender.as_str(), receipt.seq))?;
                        (sent.phase == Phase::Measured).then_some((
                            receipt.recipient.as_str(),
                            receipt.received_us.saturating_sub(sent.intended_us),
                        ))
                    })
                    .collect();
                assert_eq!(
                    latency_pairs(&records).collect::<Vec<_>>(),
                    expected_latency
                );
                let mut warmup = DirectionBytes::default();
                let mut measured = DirectionBytes::default();
                let mut unmatched = DirectionBytes::default();
                for receipt in &records.receipts {
                    let direction = match expected.get(&(receipt.sender.as_str(), receipt.seq)) {
                        Some(sent) if sent.phase == Phase::Warmup => &mut warmup,
                        Some(_) => &mut measured,
                        None => &mut unmatched,
                    };
                    assert!(!direction
                        .record(receipt.application_bytes, receipt.encoded_frame_body_bytes));
                }
                let mut reasons = Vec::new();
                let actual = payload_size_evidence(&records, 96, &mut reasons);
                assert!(reasons.is_empty(), "{reasons:?}");
                for (actual, expected) in [
                    (actual.warmup.egress, warmup),
                    (actual.measured.egress, measured),
                    (actual.unmatched_egress, unmatched),
                ] {
                    assert_eq!(
                        serde_json::to_value(actual).unwrap(),
                        serde_json::to_value(expected).unwrap()
                    );
                }
            }
        }
    }

    #[test]
    fn canonical_send_index_borrows_without_per_send_storage() {
        // Issue #775: exact ledger joins must not rebuild a node per send.
        let context = crate::unit_context();
        let records = crate::complete_records(&context.plans);
        assert!(matches!(
            SendIndex::new(&records.sent),
            SendIndex::Ordered(_)
        ));
        assert!(matches!(SendIndex::new(&[]), SendIndex::Ordered(_)));
    }

    #[test]
    fn sender_blocks_match_map_at_holes_duplicates_and_sequence_limits() {
        let context = crate::unit_context();
        let template = crate::complete_records(&context.plans).sent.remove(0);
        let values = [0, 1, 2, u64::MAX - 1, u64::MAX];
        for &first in &values {
            for &second in &values {
                for &third in &values {
                    let mut sent = Vec::new();
                    for sender in ["", "r0p0", "未知"] {
                        for seq in [first, second, third] {
                            let mut event = template.clone();
                            event.sender = sender.into();
                            event.seq = seq;
                            sent.push(event);
                        }
                    }
                    sent.sort_by_key(|event| (event.sender.clone(), event.seq));
                    let expected: BTreeMap<_, _> = sent
                        .iter()
                        .map(|event| ((event.sender.as_str(), event.seq), event))
                        .collect();
                    let index = SendIndex::new(&sent);
                    for sender in ["", "missing", "r0p0", "未知"] {
                        for seq in values {
                            let actual = index.get(sender, seq);
                            let expected = expected.get(&(sender, seq)).copied();
                            assert_eq!(
                                actual.map(std::ptr::from_ref),
                                expected.map(std::ptr::from_ref),
                                "sender={sender:?} seq={seq} ledger={first},{second},{third}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn send_index_matches_map_for_arbitrary_order_and_duplicate_keys() {
        let context = crate::unit_context();
        let base = crate::complete_records(&context.plans).sent;
        for order in 0..3 {
            let mut sent = base.clone();
            let mut duplicate = sent[0].clone();
            duplicate.intended_us = u64::MAX;
            duplicate.phase = Phase::Warmup;
            sent.push(duplicate);
            let mut boundary = sent[0].clone();
            boundary.sender = "未知".into();
            boundary.seq = u64::MAX;
            sent.push(boundary);
            match order {
                0 => {}
                1 => sent.reverse(),
                _ => sent.sort_by_key(|event| (event.sender.clone(), event.seq)),
            }
            let expected: BTreeMap<_, _> = sent
                .iter()
                .map(|event| ((event.sender.as_str(), event.seq), event))
                .collect();
            let index = SendIndex::new(&sent);
            if order == 2 {
                assert!(matches!(index, SendIndex::Ordered(_)));
            }
            for ((sender, seq), event) in &expected {
                assert!(std::ptr::eq(
                    index.get(sender, *seq).expect("recorded key"),
                    *event
                ));
            }
            for sender in ["", "unknown", "r0p0", "未知"] {
                for seq in [0, 1, u64::MAX] {
                    assert_eq!(
                        index.get(sender, seq).map(|event| event.intended_us),
                        expected.get(&(sender, seq)).map(|event| event.intended_us),
                        "sender={sender:?} seq={seq} order={order}",
                    );
                }
            }
            assert_eq!(
                SendIndex::new(&[]).get("unknown", 0).map(|event| event.seq),
                None
            );
        }
    }

    // Issue #775: the old per-position set retained a node per receipt.
    #[test]
    fn compact_arrival_evidence_matches_independent_event_scan() {
        let values = [0, 1, 2, u64::MAX];
        for mut code in 0_u32..4096 {
            let mut evidence = ArrivalEvidence::default();
            let mut sequences = Vec::new();
            for _ in 0..6 {
                let sequence = values[(code % 4) as usize];
                code /= 4;
                sequences.push(sequence);
                evidence.observe(sequence);
            }
            let mut seen = BTreeSet::new();
            let mut unique = Vec::new();
            let mut duplicates = Vec::new();
            for &sequence in &sequences {
                if seen.insert(sequence) {
                    unique.push(sequence);
                } else {
                    duplicates.push(sequence);
                }
            }
            let inversions = unique
                .windows(2)
                .filter(|pair| pair[1] <= pair[0])
                .map(|pair| pair[1])
                .collect::<Vec<_>>();
            assert_eq!(evidence.unique, count_u64(unique.len()), "{sequences:?}");
            assert_eq!(
                evidence.duplicates.count,
                count_u64(duplicates.len()),
                "{sequences:?}"
            );
            assert_eq!(
                evidence.duplicates.first,
                duplicates.first().copied(),
                "{sequences:?}"
            );
            assert_eq!(
                evidence.out_of_order.count,
                count_u64(inversions.len()),
                "{sequences:?}"
            );
            assert_eq!(
                evidence.out_of_order.first,
                inversions.first().copied(),
                "{sequences:?}"
            );
        }
        let mut ordered = ArrivalEvidence::default();
        for sequence in 1..=100_000 {
            ordered.observe(sequence);
        }
        assert_eq!(ordered.unique, 100_000);
        assert_eq!(ordered.observed.covered.stored_ranges(), 1);
        assert_eq!(ordered.duplicates.count, 0);
        assert_eq!(ordered.out_of_order.count, 0);
    }

    #[test]
    fn ordered_arrivals_retain_one_range_without_losing_duplicate_checks() {
        let mut arrivals = ArrivalCoverage::default();
        for sequence in 1..=100_000 {
            assert_eq!(arrivals.observe(sequence), Some(false));
        }
        assert_eq!(arrivals.covered.stored_ranges(), 1);
        for sequence in [1, 50_000, 100_000] {
            assert_eq!(arrivals.observe(sequence), None);
        }
        assert_eq!(arrivals.covered.above(0).covered, 100_000);
    }

    // #775/#636: ordered receipt coverage needs no tree nodes.
    // A repaired hole must return to inline storage too.
    #[test]
    fn contiguous_coverage_stays_inline_across_fragmentation_and_repair() {
        for descending in [false, true] {
            let mut coverage = StreamCoverage::default();
            for offset in 0..100_000 {
                let seq = if descending {
                    100_000 - offset
                } else {
                    offset + 1
                };
                assert!(coverage.insert(seq, seq));
                assert!(
                    coverage.ranges.is_empty(),
                    "seq={seq} descending={descending}"
                );
            }
            assert_eq!(coverage.inline, Some((1, 100_000)));
            assert!(!coverage.insert(50_000, 100_001));
            assert_eq!(coverage.inline, Some((1, 100_000)));
            assert!(coverage.insert(100_002, 100_002));
            assert!(coverage.inline.is_none());
            assert_eq!(coverage.ranges.len(), 2);
            assert!(coverage.insert(100_001, 100_001));
            assert!(coverage.ranges.is_empty());
            assert_eq!(coverage.inline, Some((1, 100_002)));
            assert_eq!(coverage.above(99_999).covered, 3);
        }
        let mut fragmented = StreamCoverage::default();
        assert!(fragmented.insert(1, 1));
        for seq in 3..=100_000 {
            assert!(fragmented.insert(seq, seq));
            assert_eq!(fragmented.ranges.len(), 2);
            assert!(fragmented.inline.is_none());
        }
        assert_eq!(
            fragmented.above(0),
            CoverageAccounting {
                covered: 99_999,
                holes: 1,
                first_hole: Some(2),
                highest: 100_000,
            }
        );
        assert!(fragmented.insert(2, 2));
        assert!(fragmented.ranges.is_empty());
        assert_eq!(fragmented.inline, Some((1, 100_000)));
        for cycle in 0..2_000 {
            let base = 100_000 + cycle * 3;
            for seq in [base + 1, base + 3, base + 2] {
                assert!(fragmented.insert(seq, seq));
            }
            assert_eq!(fragmented.inline, Some((1, base + 3)));
            assert!(fragmented.ranges.is_empty());
            assert!(!fragmented.insert(base + 2, base + 4));
            assert_eq!(fragmented.above(base).covered, 3);
        }
    }

    /// Compare the old independent position-set and unique-window rules on
    /// every short stream, including zero, overflow boundaries, and repeats.
    #[test]
    fn compact_arrivals_match_position_sets_and_unique_arrival_order() {
        let values = [0, 1, 2, u64::MAX];
        for encoded in 0_u32..4096 {
            let mut digits = encoded;
            let mut seen = BTreeSet::new();
            let mut unique = Vec::new();
            let mut arrivals = ArrivalCoverage::default();
            for _ in 0..6 {
                let sequence = values[usize::try_from(digits % 4).expect("digit")];
                digits /= 4;
                let expected = if seen.insert(sequence) {
                    let out_of_order = unique.last().is_some_and(|last| sequence <= *last);
                    unique.push(sequence);
                    Some(out_of_order)
                } else {
                    None
                };
                assert_eq!(arrivals.observe(sequence), expected, "stream {encoded}");
            }
            assert_eq!(
                arrivals.covered.above(0).covered,
                count_u64(seen.iter().filter(|sequence| **sequence > 0).count()),
                "stream {encoded}"
            );
        }
    }

    // Issue #775: one compact report must not allocate once per omitted send.
    #[test]
    fn a_long_gap_retains_one_range_and_exact_floor_accounting() {
        let mut coverage = StreamCoverage::default();
        assert!(coverage.insert(1, 1_000_000));
        assert_eq!(coverage.stored_ranges(), 1);
        assert_eq!(
            coverage.above(123),
            CoverageAccounting {
                covered: 999_877,
                holes: 0,
                first_hole: None,
                highest: 1_000_000,
            }
        );
        assert!(!coverage.insert(1_000_000, 1_000_001));
        assert_eq!(coverage.stored_ranges(), 1);
    }

    /// Frozen position-set reference: insertion order, overlap rejection,
    /// floors inside ranges, empty windows, and exact first holes must agree.
    #[test]
    fn range_coverage_matches_position_set_for_all_small_range_triples() {
        let ranges: Vec<_> = (1..=5)
            .flat_map(|start| (start..=5).map(move |end| (start, end)))
            .collect();
        for a in &ranges {
            for b in &ranges {
                for c in &ranges {
                    let mut coverage = StreamCoverage::default();
                    let mut reference = BTreeSet::new();
                    for &(from, to) in [a, b, c] {
                        let accepted = !(from..=to).any(|seq| reference.contains(&seq));
                        assert_eq!(
                            coverage.insert(from, to),
                            accepted,
                            "insert {from}:{to}; {a:?} {b:?} {c:?}"
                        );
                        if accepted {
                            reference.extend(from..=to);
                        }
                        for floor in 0..=6 {
                            let owed: BTreeSet<_> = reference
                                .iter()
                                .copied()
                                .filter(|seq| *seq > floor)
                                .collect();
                            let highest = owed.last().copied().unwrap_or(floor);
                            let missing: Vec<_> = ((floor + 1)..=highest)
                                .filter(|seq| !owed.contains(seq))
                                .collect();
                            assert_eq!(
                                coverage.above(floor),
                                CoverageAccounting {
                                    covered: count_u64(owed.len()),
                                    holes: count_u64(missing.len()),
                                    first_hole: missing.first().copied(),
                                    highest,
                                },
                                "floor {floor}; {a:?} {b:?} {c:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn range_coverage_handles_sequence_limits_and_adjacent_bridges() {
        let mut coverage = StreamCoverage::default();
        assert!(!coverage.insert(0, 1));
        assert!(!coverage.insert(2, 1));
        assert!(coverage.insert(1, 2));
        assert!(coverage.insert(5, u64::MAX));
        assert_eq!(coverage.stored_ranges(), 2);
        assert_eq!(
            coverage.above(0),
            CoverageAccounting {
                covered: u64::MAX - 2,
                holes: 2,
                first_hole: Some(3),
                highest: u64::MAX,
            }
        );
        assert!(!coverage.insert(3, 5));
        assert_eq!(
            coverage.stored_ranges(),
            2,
            "rejected overlap cannot cover the hole"
        );
        assert!(coverage.insert(3, 4));
        assert_eq!(
            coverage.stored_ranges(),
            1,
            "the bridge merges both neighbors"
        );
        assert_eq!(
            coverage.above(0),
            CoverageAccounting {
                covered: u64::MAX,
                holes: 0,
                first_hole: None,
                highest: u64::MAX,
            }
        );
        assert_eq!(
            coverage.above(u64::MAX),
            CoverageAccounting {
                covered: 0,
                holes: 0,
                first_hole: None,
                highest: u64::MAX,
            }
        );
        assert!(!coverage.insert(u64::MAX, u64::MAX));
    }

    #[test]
    #[ignore = "isolated coverage scratch profile; run in fresh processes"]
    fn compact_gap_coverage_memory_profile() {
        let count = std::env::var("CAPACITY_COVERAGE_PROBE_POSITIONS")
            .expect("position count")
            .parse::<u64>()
            .expect("u64 count");
        assert!((1..=1_000_000).contains(&count));
        let mut coverage = StreamCoverage::default();
        assert!(coverage.insert(1, count));
        let accounting = coverage.above(count / 10);
        assert_eq!(accounting.covered, count - count / 10);
        assert_eq!(accounting.holes, 0);
        println!(
            "{}",
            serde_json::json!({"positions": count,
            "covered": accounting.covered, "holes": accounting.holes,
            "first_hole": accounting.first_hole, "highest": accounting.highest})
        );
    }
}
