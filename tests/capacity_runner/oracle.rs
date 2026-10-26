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
use std::num::NonZeroU64;
use std::sync::Arc;

use hdrhistogram::{Counter, Histogram};

use crate::config::{count_u64, DeliveryClass, Experiment};
use crate::records::{ChurnEvent, ChurnPhase, GapEvent, ReceiptName, RunRecords, SentEvent};
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
    /// A rejoin snapshot cannot prove its member identity or stream tail.
    InvalidSnapshotTails {
        count: u64,
        first: SnapshotTailViolation,
    },
    /// Recorded churn actions differ from the declared reconnect workload.
    InvalidChurnEvents { count: u64, first: ChurnViolation },
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
    /// The generator stopped after retaining the first event beyond its
    /// receive-side evidence budget. The verdict covers an incomplete prefix.
    GeneratorRecordBudgetExceeded {
        event_limit: u64,
        retained_events: u64,
        control_byte_limit: u64,
        retained_control_bytes: u64,
        first_excess_kind: String,
        unretained_events: u64,
        allocation_failure: Option<String>,
    },
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
pub struct ChurnViolation {
    pub recipient: String,
    pub phase: ChurnPhase,
    pub mismatch: ChurnMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChurnMismatch {
    UnknownRecipient,
    UnplannedAction,
    UnexpectedPhase,
    BeforeScheduledTime,
    DisconnectMetadata,
    RejoinEpoch { expected: u32, actual: Option<u32> },
    UnregisteredEpoch { epoch: u32 },
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
    InactiveEpoch { epoch: u32 },
    BeforeScheduledTime,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SnapshotTailViolation {
    pub recipient: String,
    pub member: String,
    pub player_id: String,
    pub mismatch: SnapshotTailMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotTailMismatch {
    UnknownRecipient,
    ForeignMember,
    UnknownPlayerId,
    WrongPlayer {
        registered_member: String,
    },
    InvalidEpoch,
    UnannouncedEpoch {
        epoch: u32,
    },
    TailBeforeScheduledSend {
        epoch: u32,
        tail: u64,
        intended_us: u64,
        snapshot_us: u64,
    },
    TailBeyondSends {
        epoch: u32,
        tail: u64,
        sent: u64,
    },
}

/// Per-recipient accounting, in roster order.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RecipientOutcome {
    pub recipient: String,
    pub room: u32,
    /// False once a disconnect was recorded for this recipient.
    pub connected_through: bool,
    /// Unique deliveries received, per sender.
    pub received: BTreeMap<ReceiptName, u64>,
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
            recipient: gap.recipient.to_string(),
            sender: gap.sender.to_string(),
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
/// uses compact offsets and explicit raw-order ties, preserving the former map's
/// last-key-wins rule without allocating stable-sort scratch or one pointer per send.
/// Reordered sender blocks use direct offsets only for proven contiguous sequences.
/// No receipt or raw send evidence is changed by this lookup index.
enum SendIndex<'a> {
    Ordered(Vec<SenderSends<'a>>),
    Reordered {
        events: &'a [SentEvent],
        positions: Vec<u32>,
        senders: Vec<SenderOffsets>,
    },
    // Explicit host overrides may admit more sends than a compact offset fits.
    WideReordered(Vec<&'a SentEvent>),
}

struct SenderOffsets {
    start: usize,
    end: usize,
    contiguous: bool,
}

struct SenderSends<'a> {
    events: &'a [SentEvent],
    contiguous: bool,
}

impl<'a> SendIndex<'a> {
    #[cfg(test)]
    fn retained_index_bytes(&self) -> usize {
        match self {
            Self::Ordered(senders) => senders.capacity() * std::mem::size_of::<SenderSends<'_>>(),
            Self::Reordered {
                positions, senders, ..
            } => {
                positions.capacity() * std::mem::size_of::<u32>()
                    + senders.capacity() * std::mem::size_of::<SenderOffsets>()
            }
            Self::WideReordered(events) => events.capacity() * std::mem::size_of::<&SentEvent>(),
        }
    }

    fn sorted_events(&self) -> impl Iterator<Item = &'a SentEvent> + '_ {
        let (senders, reordered, events, positions) = match self {
            Self::Ordered(senders) => (senders.as_slice(), &[][..], &[][..], &[][..]),
            Self::Reordered {
                events, positions, ..
            } => (&[][..], &[][..], *events, positions.as_slice()),
            Self::WideReordered(events) => (&[][..], events.as_slice(), &[][..], &[][..]),
        };
        senders
            .iter()
            .flat_map(|sender| sender.events.iter())
            .chain(reordered.iter().copied())
            .chain(positions.iter().map(|&position| &events[position as usize]))
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
        if let Ok(count) = u32::try_from(sent.len()) {
            let mut positions: Vec<u32> = (0..count).collect();
            positions.sort_unstable_by_key(|&position| {
                let event = &sent[position as usize];
                (event.sender.as_str(), event.seq, position)
            });
            let sender_count = positions
                .windows(2)
                .filter(|pair| sent[pair[0] as usize].sender != sent[pair[1] as usize].sender)
                .count()
                + usize::from(!positions.is_empty());
            let mut senders = Vec::with_capacity(sender_count);
            let mut start = 0;
            while start < positions.len() {
                let sender = &sent[positions[start] as usize].sender;
                let length = positions[start..]
                    .partition_point(|&position| sent[position as usize].sender == *sender);
                let end = start + length;
                let contiguous = positions[start..end].windows(2).all(|pair| {
                    sent[pair[0] as usize].seq.checked_add(1) == Some(sent[pair[1] as usize].seq)
                });
                senders.push(SenderOffsets {
                    start,
                    end,
                    contiguous,
                });
                start = end;
            }
            return Self::Reordered {
                events: sent,
                positions,
                senders,
            };
        }
        let mut ordered: Vec<_> = sent.iter().collect();
        ordered.sort_by_key(|event| (event.sender.as_str(), event.seq));
        Self::WideReordered(ordered)
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
            Self::Reordered {
                events,
                positions,
                senders,
            } => {
                let index = senders
                    .binary_search_by(|block| {
                        events[positions[block.start] as usize]
                            .sender
                            .as_str()
                            .cmp(sender)
                    })
                    .ok()?;
                let block = &senders[index];
                let offsets = &positions[block.start..block.end];
                if block.contiguous {
                    let first = &events[offsets[0] as usize];
                    let offset = usize::try_from(seq.checked_sub(first.seq)?).ok()?;
                    offsets
                        .get(offset)
                        .map(|&position| &events[position as usize])
                } else {
                    find(
                        offsets.len(),
                        |index| &events[offsets[index] as usize],
                        sender,
                        seq,
                    )
                }
            }
            Self::WideReordered(sent) => find(sent.len(), |index| sent[index], sender, seq),
        }
    }
}

fn ordered_churn_events(records: &RunRecords) -> Vec<(usize, usize)> {
    let mut events: Vec<_> = (0..records.churn.len())
        .map(|position| (position, 0_usize))
        .collect();
    events.sort_unstable_by_key(|&(position, _)| {
        let event = &records.churn[position];
        (event.recipient.as_str(), event.at_us, position)
    });
    /*
        Delayed cycles can share a microsecond. Disconnect ordinals and rejoin
        epochs recover their alternating order only within that timestamp.
    */
    let mut disconnect_ordinals: BTreeMap<&str, usize> = BTreeMap::new();
    for (position, ordinal) in &mut events {
        let event = &records.churn[*position];
        *ordinal = match event.phase {
            ChurnPhase::Disconnect => {
                let next = disconnect_ordinals
                    .entry(event.recipient.as_str())
                    .or_default();
                let ordinal = next.saturating_mul(2);
                *next = next.saturating_add(1);
                ordinal
            }
            ChurnPhase::Rejoined => event
                .epoch
                .and_then(|epoch| usize::try_from(epoch.checked_sub(2)?).ok())
                .map_or(usize::MAX, |epoch| {
                    epoch.saturating_mul(2).saturating_add(1)
                }),
        };
    }
    events.sort_unstable_by_key(|&(position, ordinal)| {
        let event = &records.churn[position];
        (event.recipient.as_str(), event.at_us, ordinal, position)
    });
    events
}

fn churn_evidence(
    churn: &ChurnPlan,
    roster: &[(String, u32)],
    records: &RunRecords,
    reasons: &mut Vec<InvalidReason>,
) {
    let roster: BTreeSet<_> = roster.iter().map(|(name, _)| name.as_str()).collect();
    let mut planned: BTreeMap<&str, Vec<(u64, u64)>> = BTreeMap::new();
    for cycle in &churn.cycles {
        for peer in &cycle.peers {
            if let Some(reconnect) = cycle.reconnects_us.get(peer) {
                planned
                    .entry(peer)
                    .or_default()
                    .push((cycle.disconnect_us, *reconnect));
            }
        }
    }
    let registered: BTreeSet<_> = records
        .registry
        .values()
        .map(|(peer, epoch)| (peer.as_str(), *epoch))
        .collect();
    let events = ordered_churn_events(records);
    let mut positions: BTreeMap<&str, usize> = BTreeMap::new();
    let mut count = 0_u64;
    let mut first = None;
    for (raw_position, _) in events {
        let event = &records.churn[raw_position];
        let position = positions.entry(event.recipient.as_str()).or_default();
        let expected_phase = if position.is_multiple_of(2) {
            ChurnPhase::Disconnect
        } else {
            ChurnPhase::Rejoined
        };
        let cycle = planned
            .get(event.recipient.as_str())
            .and_then(|cycles| cycles.get(*position / 2));
        let mismatch = if !roster.contains(event.recipient.as_str()) {
            Some(ChurnMismatch::UnknownRecipient)
        } else if let Some(&(disconnect, reconnect)) = cycle {
            if event.phase != expected_phase {
                Some(ChurnMismatch::UnexpectedPhase)
            } else {
                let expected_epoch = u32::try_from(*position / 2)
                    .unwrap_or(u32::MAX)
                    .saturating_add(2);
                *position = position.saturating_add(1);
                if event.at_us
                    < if event.phase == ChurnPhase::Disconnect {
                        disconnect
                    } else {
                        reconnect
                    }
                {
                    Some(ChurnMismatch::BeforeScheduledTime)
                } else if event.phase == ChurnPhase::Disconnect
                    && (event.epoch.is_some() || !event.tails.is_empty())
                {
                    Some(ChurnMismatch::DisconnectMetadata)
                } else if event.phase == ChurnPhase::Rejoined && event.epoch != Some(expected_epoch)
                {
                    Some(ChurnMismatch::RejoinEpoch {
                        expected: expected_epoch,
                        actual: event.epoch,
                    })
                } else if event.phase == ChurnPhase::Rejoined
                    && !registered.contains(&(event.recipient.as_str(), expected_epoch))
                {
                    Some(ChurnMismatch::UnregisteredEpoch {
                        epoch: expected_epoch,
                    })
                } else {
                    None
                }
            }
        } else {
            Some(ChurnMismatch::UnplannedAction)
        };
        if let Some(mismatch) = mismatch {
            count = count.saturating_add(1);
            first.get_or_insert_with(|| ChurnViolation {
                recipient: event.recipient.clone(),
                phase: event.phase,
                mismatch,
            });
        }
    }
    if let Some(first) = first {
        reasons.push(InvalidReason::InvalidChurnEvents { count, first });
    }
}

fn scheduled_send_evidence(
    plans: &[SenderPlan],
    records: &RunRecords,
    index: &SendIndex<'_>,
    reasons: &mut Vec<InvalidReason>,
) -> u64 {
    let plans: BTreeMap<_, _> = plans
        .iter()
        .map(|plan| (plan.name.as_str(), plan))
        .collect();
    // Send completion and churn share one clock in the same peer task.
    // Keep one interval per incarnation, never one entry per scheduled send.
    // Equal microseconds remain admissible on both sides of a boundary because
    // the artifact clock rounds distinct operations to the same microsecond.
    let mut epoch_times: BTreeMap<(&str, u32), (u64, Option<u64>)> = plans
        .keys()
        .map(|sender| ((*sender, 1), (0, None)))
        .collect();
    for event in &records.churn {
        if let Some(epoch) = event.epoch.filter(|epoch| *epoch > 1) {
            if event.phase == ChurnPhase::Rejoined {
                epoch_times
                    .entry((event.recipient.as_str(), epoch))
                    .or_insert((event.at_us, None));
            }
        }
    }
    let mut disconnects: Vec<_> = records
        .churn
        .iter()
        .filter(|event| event.phase == ChurnPhase::Disconnect)
        .collect();
    disconnects.sort_unstable_by_key(|event| (event.recipient.as_str(), event.at_us));
    let mut closing_epochs: BTreeMap<&str, u32> = BTreeMap::new();
    for event in disconnects {
        let epoch = closing_epochs.entry(event.recipient.as_str()).or_insert(1);
        if let Some((_, end)) = epoch_times.get_mut(&(event.recipient.as_str(), *epoch)) {
            *end = Some(event.at_us);
        }
        *epoch = epoch.saturating_add(1);
    }
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
                Some(_) if !epoch_times.contains_key(&(event.sender.as_str(), event.epoch)) => {
                    Some(SendPlanMismatch::UnannouncedEpoch { epoch: event.epoch })
                }
                Some(_)
                    if epoch_times
                        .get(&(event.sender.as_str(), event.epoch))
                        .is_some_and(|(start, end)| {
                            event.sent_us < *start || end.is_some_and(|end| event.sent_us > end)
                        }) =>
                {
                    Some(SendPlanMismatch::InactiveEpoch { epoch: event.epoch })
                }
                Some(_) if event.sent_us < event.intended_us => {
                    Some(SendPlanMismatch::BeforeScheduledTime)
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
/// borrow raw events; arbitrary artifact order uses four-byte raw offsets.
/// Wide host overrides retain the pointer fallback when offsets cannot fit.
enum RelayBlock<'a> {
    Borrowed {
        events: &'a [SentEvent],
    },
    Compact {
        events: &'a [SentEvent],
        positions: Vec<u32>,
    },
    Indexed(Vec<&'a SentEvent>),
}

impl<'a> RelayBlock<'a> {
    #[cfg(test)]
    fn retained_index_bytes(&self) -> usize {
        match self {
            Self::Borrowed { .. } => 0,
            Self::Compact { positions, .. } => positions.capacity() * std::mem::size_of::<u32>(),
            Self::Indexed(events) => events.capacity() * std::mem::size_of::<&SentEvent>(),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Borrowed { events, .. } => events.len(),
            Self::Compact { positions, .. } => positions.len(),
            Self::Indexed(events) => events.len(),
        }
    }

    fn get(&self, index: usize) -> Option<&'a SentEvent> {
        match self {
            Self::Borrowed { events, .. } => events.get(index),
            Self::Compact { events, positions } => events.get(*positions.get(index)? as usize),
            Self::Indexed(events) => events.get(index).copied(),
        }
    }

    fn completed_by(&self, at_us: u64) -> usize {
        match self {
            Self::Borrowed { events, .. } => {
                events.iter().filter(|event| event.sent_us <= at_us).count()
            }
            Self::Compact { events, positions } => positions
                .iter()
                .filter(|&&position| events[position as usize].sent_us <= at_us)
                .count(),
            Self::Indexed(events) => events.iter().filter(|event| event.sent_us <= at_us).count(),
        }
    }
}

fn relay_blocks(sent: &[SentEvent]) -> BTreeMap<(&str, u32), RelayBlock<'_>> {
    let compact = u32::try_from(sent.len()).is_ok();
    let mut shapes = BTreeMap::new();
    let mut start = 0;
    while start < sent.len() {
        let key = (sent[start].sender.as_str(), sent[start].epoch);
        let mut end = start + 1;
        while end < sent.len() && (sent[end].sender.as_str(), sent[end].epoch) == key {
            end += 1;
        }
        let shape = shapes.entry(key).or_insert((start, end, 0));
        shape.2 += end - start;
        start = end;
    }
    let mut blocks: BTreeMap<_, _> = shapes
        .into_iter()
        .map(|(key, (start, end, count))| {
            let events = &sent[start..end];
            let block = if count == events.len() && events.is_sorted_by_key(|event| event.seq) {
                RelayBlock::Borrowed { events }
            } else if compact {
                RelayBlock::Compact {
                    events: sent,
                    positions: Vec::with_capacity(count),
                }
            } else {
                RelayBlock::Indexed(Vec::with_capacity(count))
            };
            (key, block)
        })
        .collect();
    let mut start = 0;
    while start < sent.len() {
        let key = (sent[start].sender.as_str(), sent[start].epoch);
        let mut end = start + 1;
        while end < sent.len() && (sent[end].sender.as_str(), sent[end].epoch) == key {
            end += 1;
        }
        match blocks.get_mut(&key).expect("counted relay block") {
            RelayBlock::Compact { positions, .. } => positions.extend(
                u32::try_from(start).expect("compact ledger start fits u32")
                    ..u32::try_from(end).expect("compact ledger end fits u32"),
            ),
            RelayBlock::Indexed(events) => events.extend(&sent[start..end]),
            RelayBlock::Borrowed { .. } => {}
        }
        start = end;
    }
    for block in blocks.values_mut() {
        match block {
            // Raw offsets break duplicate ties exactly as the stable pointer sort did.
            RelayBlock::Compact { events, positions } => positions
                .sort_unstable_by_key(|&position| (events[position as usize].seq, position)),
            // Stable order preserves original evidence for duplicate keys.
            RelayBlock::Indexed(events) => events.sort_by_key(|event| event.seq),
            RelayBlock::Borrowed { .. } => {}
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
    indexed_latency_pairs(records, SendIndex::new(&records.sent))
}

fn indexed_latency_pairs<'a>(
    records: &'a RunRecords,
    sent_by_key: impl std::borrow::Borrow<SendIndex<'a>>,
) -> impl Iterator<Item = (&'a str, u64)> {
    records.receipts.iter().filter_map(move |receipt| {
        let sent = sent_by_key
            .borrow()
            .get(receipt.sender.as_str(), receipt.seq)?;
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
struct LatencyAccumulator<T: Counter = u64> {
    histogram: Histogram<T>,
    max_us: u64,
}

impl<T: Counter> Default for LatencyAccumulator<T> {
    fn default() -> Self {
        Self {
            histogram: Histogram::<T>::new(3).expect("latency histogram precision"),
            max_us: 0,
        }
    }
}

impl<T: Counter> LatencyAccumulator<T> {
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

// HDR counters saturate silently. Bound each bucket by every raw arrival at
// the busiest recipient, before allocating any recipient histogram.
fn recipient_latency_counter_bytes(receipts: u64) -> usize {
    if u8::try_from(receipts).is_ok() {
        1
    } else if u16::try_from(receipts).is_ok() {
        2
    } else if u32::try_from(receipts).is_ok() {
        4
    } else {
        8
    }
}

/// Preserve each recipient's tail while bounding each histogram counter by
/// the caller's proven sample limit. The aggregate still uses u64 counters.
fn recipient_latency_statistics<T: Counter>(
    records: &RunRecords,
    sent_by_key: &SendIndex<'_>,
    member_names: &BTreeSet<&str>,
    per_recipient: &mut [RecipientOutcome],
    maximum_recipient_receipts: u64,
) -> LatencyStats {
    assert!(
        T::from_u64(maximum_recipient_receipts).is_some(),
        "latency counter must fit every raw receipt"
    );
    let mut latency = LatencyAccumulator::<u64>::default();
    let mut latency_by_recipient: BTreeMap<&str, LatencyAccumulator<T>> = BTreeMap::new();
    for (recipient, sample) in indexed_latency_pairs(records, sent_by_key) {
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
    for outcome in per_recipient {
        outcome.latency_us = latency_by_recipient
            .get(outcome.recipient.as_str())
            .map(LatencyAccumulator::stats)
            .unwrap_or_default();
    }

    latency.stats()
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
#[derive(Debug, Default, PartialEq, Eq)]
struct ChurnView<'a> {
    // All name keys borrow the immutable roster during summarization.
    /// Member -> the epoch this recipient's seat adopts (and every epoch it
    /// has adopted, which are the streams it can be owed).
    adopted: BTreeMap<&'a str, Vec<u32>>,
    /// `(sender, epoch) -> instant past which deliveries for the stream are
    /// stale-epoch misroutes (the sender rejoined past it)`.
    closed_at: BTreeMap<(&'a str, u32), u64>,
    /// `(sender, epoch) -> (owed floor, from_us, server_enforced)`: from
    /// `from_us` on, the stream's owed window starts at floor + 1. A
    /// `server_enforced` floor (a rejoin snapshot tail) is also a delivery
    /// contract — the server gates its own queued frames against it, so a
    /// redelivery is a misroute. A derived floor (send times at the rejoin
    /// instant) is bookkeeping only: the server has no watermark for an
    /// omitted member, so a frame just past the seat can legally arrive
    /// with its recorded send time behind the rejoin instant.
    floors: BTreeMap<(&'a str, u32), (u64, u64, bool)>,
}

fn insert_epoch(epochs: &mut Vec<u32>, epoch: u32) {
    if let Err(position) = epochs.binary_search(&epoch) {
        epochs.insert(position, epoch);
    }
}

fn room_churn_events<'a>(
    events: &'a [ChurnEvent],
    room_of: &BTreeMap<&str, u32>,
) -> BTreeMap<u32, Vec<&'a ChurnEvent>> {
    let mut rooms: BTreeMap<u32, Vec<&ChurnEvent>> = BTreeMap::new();
    for event in events {
        if let Some(room) = room_of.get(event.recipient.as_str()) {
            rooms.entry(*room).or_default().push(event);
        }
    }
    for events in rooms.values_mut() {
        events.sort_by_key(|event| event.at_us);
    }
    rooms
}

fn viewer_churn_events<'a>(
    events: &'a [&'a ChurnEvent],
    viewer: &'a str,
) -> impl Iterator<Item = &'a ChurnEvent> + 'a {
    events
        .chunk_by(|first, next| first.at_us == next.at_us)
        .flat_map(move |tied| {
            tied.iter()
                .copied()
                .filter(move |event| event.recipient == viewer)
                .chain(
                    tied.iter()
                        .copied()
                        .filter(move |event| event.recipient != viewer),
                )
        })
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
fn churn_views<'a>(
    records: &RunRecords,
    roster: &'a [(String, u32)],
    blocks: &BTreeMap<(&str, u32), RelayBlock<'_>>,
) -> (BTreeMap<&'a str, Arc<ChurnView<'a>>>, Vec<String>) {
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
    let mut members_by_room: BTreeMap<u32, Vec<&str>> = BTreeMap::new();
    for (member, room) in roster {
        members_by_room
            .entry(*room)
            .or_default()
            .push(member.as_str());
    }
    let events_by_room = room_churn_events(&records.churn, &room_of);
    // Ordinary rooms have the same immutable initial view at every seat.
    // Keep one per room, rather than one tree of names and epochs per peer.
    // A room with any recorded churn retains the full viewer-specific path.
    let mut initial_views: BTreeMap<u32, Arc<ChurnView<'a>>> = BTreeMap::new();
    let mut views: BTreeMap<&'a str, Arc<ChurnView<'a>>> = BTreeMap::new();
    let mut unresolved: Vec<String> = Vec::new();
    for (viewer, room) in roster {
        let Some(events) = events_by_room.get(room) else {
            let initial = initial_views.entry(*room).or_insert_with(|| {
                let mut view = ChurnView::default();
                for member in &members_by_room[room] {
                    view.adopted.insert(*member, vec![1]);
                }
                Arc::new(view)
            });
            views.insert(viewer.as_str(), Arc::clone(initial));
            continue;
        };
        let mut view = ChurnView::default();
        // Seed: every co-room member starts in epoch 1 (the initial join).
        for member in &members_by_room[room] {
            view.adopted.insert(*member, vec![1]);
        }
        for event in viewer_churn_events(events, viewer) {
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
                    let Some((&member, _)) = room_of.get_key_value(recipient.as_str()) else {
                        continue;
                    };
                    let Some(epochs) = view.adopted.get_mut(member) else {
                        continue;
                    };
                    for &old in epochs.iter().filter(|old| **old != *new_epoch) {
                        view.closed_at.entry((member, old)).or_insert(*at_us);
                    }
                    insert_epoch(epochs, *new_epoch);
                }
                ChurnPhase::Rejoined => {
                    // This viewer rejoined. The snapshot is the authority on
                    // every member it names: adopt the named incarnation and
                    // floor the stream at its tail. Members it omits are not
                    // closed — see the function doc — they receive a
                    // derived floor over every adopted stream instead.
                    for (&member, epochs) in &mut view.adopted {
                        match tails.get(member) {
                            Some(&(ref player_id, tail)) => {
                                let Some(snap_epoch) = resolve(records, player_id, &mut unresolved)
                                else {
                                    continue;
                                };
                                for &old in epochs.iter().filter(|old| **old != snap_epoch) {
                                    view.closed_at.entry((member, old)).or_insert(*at_us);
                                }
                                insert_epoch(epochs, snap_epoch);
                                view.floors
                                    .insert((member, snap_epoch), (tail, *at_us, true));
                            }
                            None => {
                                // Absent from the snapshot: floor every
                                // adopted stream at the sends that completed
                                // at or before this rejoin.
                                for &old in epochs.iter() {
                                    let completed = blocks
                                        .get(&(member, old))
                                        .map(|block| block.completed_by(*at_us))
                                        .unwrap_or(0);
                                    let completed = u64::try_from(completed).unwrap_or(u64::MAX);
                                    view.floors
                                        .insert((member, old), (completed, *at_us, false));
                                }
                            }
                        }
                    }
                }
            }
        }
        views.insert(viewer.as_str(), Arc::new(view));
    }
    (views, unresolved)
}

fn snapshot_tail_evidence(
    records: &RunRecords,
    roster: &[(String, u32)],
    blocks: &BTreeMap<(&str, u32), RelayBlock<'_>>,
    reasons: &mut Vec<InvalidReason>,
) {
    let rooms: BTreeMap<_, _> = roster
        .iter()
        .map(|(name, room)| (name.as_str(), *room))
        .collect();
    let rejoined_epochs: BTreeSet<_> = records
        .churn
        .iter()
        .filter_map(|event| {
            (event.phase == ChurnPhase::Rejoined)
                .then_some((event.recipient.as_str(), event.epoch?))
        })
        .collect();
    let mut count = 0;
    let mut first = None;
    for event in records
        .churn
        .iter()
        .filter(|event| event.phase == ChurnPhase::Rejoined)
    {
        for (member, (player_id, tail)) in &event.tails {
            let mismatch = match rooms.get(event.recipient.as_str()) {
                None => Some(SnapshotTailMismatch::UnknownRecipient),
                Some(room) if rooms.get(member.as_str()) != Some(room) => {
                    Some(SnapshotTailMismatch::ForeignMember)
                }
                Some(_) => match records.registry.get(player_id) {
                    None => Some(SnapshotTailMismatch::UnknownPlayerId),
                    Some((registered_member, _)) if registered_member != member => {
                        Some(SnapshotTailMismatch::WrongPlayer {
                            registered_member: registered_member.clone(),
                        })
                    }
                    Some((_, 0)) => Some(SnapshotTailMismatch::InvalidEpoch),
                    Some((_, epoch))
                        if *epoch > 1 && !rejoined_epochs.contains(&(member.as_str(), *epoch)) =>
                    {
                        Some(SnapshotTailMismatch::UnannouncedEpoch { epoch: *epoch })
                    }
                    Some((_, epoch)) => {
                        let sent = blocks
                            .get(&(member.as_str(), *epoch))
                            .map_or(0, |block| count_u64(block.len()));
                        if *tail > sent {
                            Some(SnapshotTailMismatch::TailBeyondSends {
                                epoch: *epoch,
                                tail: *tail,
                                sent,
                            })
                        } else {
                            tail.checked_sub(1)
                                .and_then(|index| usize::try_from(index).ok())
                                .and_then(|index| {
                                    blocks.get(&(member.as_str(), *epoch))?.get(index)
                                })
                                .filter(|send| send.intended_us > event.at_us)
                                .map(|send| SnapshotTailMismatch::TailBeforeScheduledSend {
                                    epoch: *epoch,
                                    tail: *tail,
                                    intended_us: send.intended_us,
                                    snapshot_us: event.at_us,
                                })
                        }
                    }
                },
            };
            if let Some(mismatch) = mismatch {
                count += 1;
                first.get_or_insert_with(|| SnapshotTailViolation {
                    recipient: event.recipient.clone(),
                    member: member.clone(),
                    player_id: player_id.clone(),
                    mismatch,
                });
            }
        }
    }
    if let Some(first) = first {
        reasons.push(InvalidReason::InvalidSnapshotTails { count, first });
    }
}

/// Validate every raw event before delivery filtering, including duplicates,
/// misroutes, warm-up records, and receipt keys absent from the send ledger.
fn payload_size_evidence(
    records: &RunRecords,
    sent_by_key: &SendIndex<'_>,
    expected_application_bytes: u32,
    reasons: &mut Vec<InvalidReason>,
) -> PayloadBytes {
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
    // Share exact send lookups across validation and latency passes. Arbitrary
    // replay order needs one sort and one compact offset index, not three.
    let sent_by_key = SendIndex::new(&records.sent);
    let mut reasons = records.faults.clone();
    let payload_bytes = payload_size_evidence(
        records,
        &sent_by_key,
        expected_application_bytes,
        &mut reasons,
    );
    churn_evidence(churn, roster, records, &mut reasons);
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
    let completed = scheduled_send_evidence(plans, records, &sent_by_key, &mut reasons);
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
    snapshot_tail_evidence(records, roster, &blocks, &mut reasons);
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
    let mut arrivals: BTreeMap<ArrivalKey<'_>, ArrivalEvidence<'_>> = BTreeMap::new();
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
                    let stream_key = (receipt.sender.as_str(), epoch);
                    Some(ArrivalContract {
                        closed_after: view.closed_at.get(&stream_key),
                        floor: view.floors.get(&stream_key),
                    })
                };
                ArrivalEvidence {
                    contract,
                    ..ArrivalEvidence::default()
                }
            });
        if stream.total == 0 {
            stream.first = receipt.server_seq;
        }
        stream.total += 1;
        // Opaque leaks still owe overlap, duplicate, and order evidence.
        stream.observe(receipt.server_seq);
        let Some(contract) = stream.contract else {
            continue;
        };
        let stale = contract
            .closed_after
            .is_some_and(|at_us| receipt.received_us > *at_us);
        let below_floor = contract.floor.is_some_and(|(floor, from_us, enforced)| {
            *enforced && receipt.received_us >= *from_us && receipt.server_seq <= *floor
        });
        let ledger_matches = receipt
            .server_seq
            .checked_sub(1)
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| blocks.get(&(sender, epoch))?.get(index))
            .is_some_and(|event| event.seq == receipt.seq);
        if !ledger_matches || stale || below_floor {
            stream.violations_mut().misrouted.record(receipt.server_seq);
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
    let mut gap_streams: BTreeMap<ArrivalKey<'_>, GapStreamEvidence> = BTreeMap::new();
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
        let recipient = gap.recipient.as_str();
        let sender = gap.sender.as_str();
        let gap_stream = gap_streams
            .entry((recipient, sender, gap.epoch))
            .or_insert_with(|| {
                // Every observed report must fit the sender's recorded epoch,
                // including streams whose completeness obligation has ended.
                let total_owed = blocks
                    .get(&(sender, gap.epoch))
                    .map_or(0, |block| count_u64(block.len()));
                GapStreamEvidence {
                    total_owed,
                    ..GapStreamEvidence::default()
                }
            });
        let total_owed = gap_stream.total_owed;
        let stream = arrivals.entry((recipient, sender, gap.epoch)).or_default();
        // Receipts are complete before this pass. Insert gaps directly into
        // their compact coverage, without retaining one pointer per report.
        if gap.to_seq > total_owed {
            gap_stream.invalid.record_violation(
                gap,
                format!(
                    "gap range reaches beyond what the sender sent ({} > \
                     {total_owed} relayed in epoch {})",
                    gap.to_seq, gap.epoch
                ),
            );
        } else if !stream.observed.covered.insert(gap.from_seq, gap.to_seq) {
            gap_stream.invalid.record_violation(
                gap,
                format!(
                    "gap range {}:{} overlaps an already-covered sequence",
                    gap.from_seq, gap.to_seq
                ),
            );
        } else {
            gap_stream.covered += gap.to_seq - gap.from_seq + 1;
        }
    }

    let mut duplicates = Category::default();
    let mut misrouted = unknown_recipients;
    let mut out_of_order = Category::default();
    let mut missing = Category::default();
    let mut leaked = Category::default();

    // Share sender spellings across the fan-out summary. Retain owned keys so
    // summaries outlive their raw run records without changing artifact JSON.
    let mut summary_sender_names: BTreeMap<&str, ReceiptName> = BTreeMap::new();
    let mut per_recipient = Vec::with_capacity(roster.len());
    let mut maximum_recipient_receipts = 0_u64;
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
        let mut recipient_receipts = 0_u64;
        for ((_, sender, epoch), stream) in recipient_arrivals(&arrivals, recipient) {
            // Count malformed, duplicate, opaque, and misrouted arrivals too.
            recipient_receipts += stream.total;
            if stream.total == 0 {
                continue;
            }
            if opaque_senders.contains(sender) {
                // The unsupported-conversion contract: the opaque sender's
                // payload reaches NO recipient as a delivery. An arrival is
                // the leak class — a conversion (lossy or lucky) happened
                // that the server must refuse. Counted only in the leak
                // category: its verdict names the recipient and stream
                // exactly, and a leak already invalidates the run.
                let seq = stream.first;
                leaked.record(DeliveryKey {
                    recipient: recipient.clone(),
                    sender: (*sender).to_string(),
                    epoch: *epoch,
                    seq,
                });
                continue;
            }
            if !expected_senders.contains(sender) {
                outcome.misrouted += stream.total;
                SequenceViolations {
                    count: stream.total,
                    first: stream.first,
                }
                .append_to(&mut misrouted, recipient, sender, *epoch);
                continue;
            }
            // Closed-stream and invalid-sequence arrivals still count as evidence.
            *received_by_sender.entry(sender).or_insert(0) += stream.unique;
            if let Some(violations) = stream.violations.as_deref() {
                outcome.misrouted += violations.misrouted.count;
                violations
                    .misrouted
                    .append_to(&mut misrouted, recipient, sender, *epoch);
            }
        }

        maximum_recipient_receipts = maximum_recipient_receipts.max(recipient_receipts);

        // (b) Validate all observed gaps, then check completeness only for
        // streams this seat still owes. Closed epochs keep their evidence.
        for sender in expected_senders {
            // Keep roster sender order for the first offending key, including
            // epochs that no longer owe completeness or were never adopted.
            for ((_, _, epoch), stream) in arrivals
                .range((recipient.as_str(), sender, 0)..=(recipient.as_str(), sender, u32::MAX))
            {
                if let Some(violations) = stream.violations.as_deref() {
                    outcome.duplicates += violations.duplicates.count;
                    violations
                        .duplicates
                        .append_to(&mut duplicates, recipient, sender, *epoch);
                    outcome.out_of_order += violations.out_of_order.count;
                    violations
                        .out_of_order
                        .append_to(&mut out_of_order, recipient, sender, *epoch);
                }
            }
            let adopted = view.adopted.get(sender);
            let Some(epochs) = sender_epochs.get(sender) else {
                continue;
            };
            for epoch in epochs {
                let gaps = gap_streams.remove(&(recipient.as_str(), sender, *epoch));
                if let Some(gaps) = &gaps {
                    invalid_gaps.count += gaps.invalid.count;
                    if invalid_gaps.first.is_none() {
                        invalid_gaps.first = gaps.invalid.first.clone();
                    }
                }
                // A closed stream is finished: its sender rejoined past it
                // (or the viewer's snapshot superseded it), so nothing
                // further is owed. Arrivals there are stale-epoch
                // misroutes, caught by the arrival scan above.
                if adopted.is_none_or(|epochs| epochs.binary_search(epoch).is_err())
                    || view.closed_at.contains_key(&(sender, *epoch))
                {
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
                // The stream's rejoin floor: from this instant on, every
                // sequence at or below the snapshot tail is not owed (the
                // loud away window), and deliveries must resume exactly one
                // past it.
                let (owed_floor, _floor_from_us, _enforced) = view
                    .floors
                    .get(&(sender, *epoch))
                    .copied()
                    .unwrap_or((0, 0, false));

                // Coverage model, shared by every class over the stream's
                // owed window (above the rejoin floor, bounded by what the
                // sender sent in this epoch). Delivered unique sequences are
                // covered; the lossy classes may additionally cover an
                // omission only with its exact gap report, while reliable
                // permits no gap report at all (validated globally above) —
                // so its coverage is receipts alone.
                let covered = arrival.observed.covered;
                if let Some(gaps) = gaps {
                    outcome.gap_covered += gaps.covered;
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
            .map(|(sender, count)| {
                let name = summary_sender_names
                    .entry(sender)
                    .or_insert_with(|| ReceiptName::from(*sender));
                (name.clone(), *count)
            })
            .collect();

        per_recipient.push(outcome);
    }

    // Duplicate, opaque, and misrouted arrivals also count toward this bound.
    // The aggregate always retains u64 counters, even when each peer fits u8.
    let latency_us = match recipient_latency_counter_bytes(maximum_recipient_receipts) {
        1 => recipient_latency_statistics::<u8>(
            records,
            &sent_by_key,
            &member_names,
            &mut per_recipient,
            maximum_recipient_receipts,
        ),
        2 => recipient_latency_statistics::<u16>(
            records,
            &sent_by_key,
            &member_names,
            &mut per_recipient,
            maximum_recipient_receipts,
        ),
        4 => recipient_latency_statistics::<u32>(
            records,
            &sent_by_key,
            &member_names,
            &mut per_recipient,
            maximum_recipient_receipts,
        ),
        _ => recipient_latency_statistics::<u64>(
            records,
            &sent_by_key,
            &member_names,
            &mut per_recipient,
            maximum_recipient_receipts,
        ),
    };

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
                | InvalidReason::GeneratorRecordBudgetExceeded { .. }
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
        latency_us,
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
    first: u64,
}

impl SequenceViolations {
    fn record(&mut self, sequence: u64) {
        if self.count == 0 {
            self.first = sequence;
        }
        self.count += 1;
    }

    fn append_to(&self, category: &mut Category, recipient: &str, sender: &str, epoch: u32) {
        category.count += self.count;
        if self.count > 0 {
            category.first.get_or_insert_with(|| DeliveryKey {
                recipient: recipient.to_string(),
                sender: sender.to_string(),
                epoch,
                seq: self.first,
            });
        }
    }
}

/// Stream boundaries are resolved once, without cloning a sender per receipt.
#[derive(Clone, Copy)]
struct ArrivalContract<'a> {
    closed_after: Option<&'a u64>,
    floor: Option<&'a (u64, u64, bool)>,
}

type ArrivalKey<'a> = (&'a str, &'a str, u32);

/// Seek directly to one recipient's streams. The empty sender and epoch zero
/// include malformed names and epochs, which must remain delivery evidence.
fn recipient_arrivals<'map, 'records, 'view>(
    arrivals: &'map BTreeMap<ArrivalKey<'records>, ArrivalEvidence<'view>>,
    recipient: &'records str,
) -> impl Iterator<Item = (&'map ArrivalKey<'records>, &'map ArrivalEvidence<'view>)> {
    arrivals
        .range((recipient, "", 0)..)
        .take_while(move |((name, _, _), _)| *name == recipient)
}

/// One gap-report accumulator per stream, independent of report count.
/// Reliable streams without reports allocate no gap state.
#[derive(Default)]
struct GapStreamEvidence {
    total_owed: u64,
    covered: u64,
    invalid: GapViolations,
}

/// Ordered complete streams retain one range, rather than one receipt pointer.
#[derive(Default)]
struct ArrivalEvidence<'a> {
    contract: Option<ArrivalContract<'a>>,
    total: u64,
    first: u64,
    unique: u64,
    observed: ArrivalCoverage,
    violations: Option<Box<ArrivalViolations>>,
}

#[derive(Default)]
struct ArrivalViolations {
    duplicates: SequenceViolations,
    out_of_order: SequenceViolations,
    misrouted: SequenceViolations,
}

impl ArrivalEvidence<'_> {
    fn violations_mut(&mut self) -> &mut ArrivalViolations {
        self.violations.get_or_insert_with(Box::default)
    }

    fn observe(&mut self, sequence: u64) {
        match self.observed.observe(sequence) {
            None => self.violations_mut().duplicates.record(sequence),
            Some(out_of_order) => {
                self.unique += 1;
                if out_of_order {
                    self.violations_mut().out_of_order.record(sequence);
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
    previous: u64,
    has_previous: bool,
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
        let out_of_order = self.has_previous && sequence <= self.previous;
        self.previous = sequence;
        self.has_previous = true;
        Some(out_of_order)
    }
}

/// Disjoint inclusive ranges for one recipient stream. Adjacent ranges merge,
/// so a compact gap report never expands into one allocation per omitted send.
/// Reject an overlapping report in full before changing coverage.
#[derive(Default)]
struct StreamCoverage {
    // Ordered deliveries and compact gap reports need no tree allocation.
    inline: Option<(NonZeroU64, u64)>,
    #[allow(
        clippy::box_collection,
        reason = "contiguous streams need no tree header"
    )]
    ranges: Option<Box<BTreeMap<u64, u64>>>,
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
        let Some(nonzero_from) = NonZeroU64::new(from) else {
            return false;
        };
        if from > to {
            return false;
        }
        if self.ranges.as_ref().is_none_or(|ranges| ranges.is_empty()) {
            let Some((nonzero_start, end)) = self.inline else {
                self.inline = Some((nonzero_from, to));
                return true;
            };
            let start = nonzero_start.get();
            if from <= end && to >= start {
                return false;
            }
            if end.checked_add(1) == Some(from) {
                self.inline = Some((nonzero_start, to));
            } else if to.checked_add(1) == Some(start) {
                self.inline = Some((nonzero_from, end));
            } else {
                self.inline = None;
                let ranges = self.ranges.get_or_insert_with(Box::default);
                ranges.insert(start, end);
                ranges.insert(from, to);
            }
            return true;
        }
        let Some(ranges) = self.ranges.as_mut() else {
            return false;
        };
        // The last range beginning at or below `to` is the only candidate
        // needed to prove overlap in this disjoint, ordered map.
        if ranges
            .range(..=to)
            .next_back()
            .is_some_and(|(_, end)| *end >= from)
        {
            return false;
        }
        if let Some(next) = to.checked_add(1) {
            if let Some(end) = ranges.remove(&next) {
                to = end;
            }
        }
        // Extend an existing range in place without removing its key.
        if let Some((_, end)) = ranges.range_mut(..from).next_back() {
            if end.checked_add(1) == Some(from) {
                *end = to;
            } else {
                ranges.insert(from, to);
            }
        } else {
            ranges.insert(from, to);
        }
        if ranges.len() == 1 {
            self.inline = ranges
                .pop_first()
                .and_then(|(start, end)| NonZeroU64::new(start).map(|start| (start, end)));
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
            .map(|(start, end)| (start.get(), *end))
            .chain(
                self.ranges
                    .iter()
                    .flat_map(|ranges| ranges.iter())
                    .map(|(&start, &end)| (start, end)),
            )
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
        self.ranges.as_ref().map_or(0, |ranges| ranges.len()) + usize::from(self.inline.is_some())
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
    fn unadopted_epoch_reports_still_require_exact_gap_evidence() {
        let context = crate::unit_context_with_class(DeliveryClass::Latest);
        let mut baseline = crate::complete_records(&context.plans);
        for event in &mut baseline.sent {
            event.epoch = 99;
        }
        for event in &mut baseline.receipts {
            event.epoch = 99;
        }
        for mutation in ["bounds", "receipt", "overlap", "legal"] {
            let mut records = baseline.clone();
            let mut gap = crate::gap_for(
                "r0p0",
                "r0p1",
                0,
                signal_fish_server::protocol::DeliveryGapReason::LatestSuperseded,
            );
            gap.epoch = 99;
            if mutation == "bounds" {
                gap.from_seq = u64::MAX;
                gap.to_seq = u64::MAX;
            }
            if mutation == "overlap" || mutation == "legal" {
                crate::drop_receipt(&mut records, "r0p0", "r0p1", 0);
            }
            records.gaps.push(gap.clone());
            if mutation == "overlap" {
                records.gaps.push(gap);
            }
            let summary = summarize(
                &context.plans,
                &context.roster,
                &records,
                1_000,
                96,
                DeliveryClass::Latest,
                &ChurnPlan::default(),
                None,
            );
            // Unannounced epochs already invalidate the send schedule. They
            // still must not hide a separate impossible observed gap report.
            assert!(!summary.valid);
            assert_eq!(
                summary
                    .reasons
                    .iter()
                    .find_map(|reason| match reason {
                        InvalidReason::InvalidGapReports { count, .. } => Some(*count),
                        _ => None,
                    })
                    .unwrap_or(0),
                u64::from(mutation != "legal"),
                "{mutation}: {:?}",
                summary.reasons
            );
        }
    }

    // #1130: a rejoin must not erase earlier observed contract violations.
    #[test]
    fn observed_arrival_violations_keep_roster_sender_order() {
        let mut config = crate::scenario_config(crate::Encoding::V3Json);
        config.players_per_room = 11;
        let (plans, churn) = crate::build_run_shape(&config).unwrap();
        let roster: Vec<_> = plans
            .iter()
            .map(|plan| (plan.name.clone(), plan.room))
            .collect();
        for duplicate in [true, false] {
            let mut records = crate::complete_records(&plans);
            for sender in ["r0p2", "r0p10"] {
                let positions: Vec<_> = records
                    .receipts
                    .iter()
                    .enumerate()
                    .filter_map(|(position, receipt)| {
                        (receipt.recipient == "r0p0" && receipt.sender == sender)
                            .then_some(position)
                    })
                    .take(2)
                    .collect();
                assert_eq!(positions.len(), 2);
                if duplicate {
                    let mut receipt = records.receipts[positions[0]].clone();
                    receipt.received_us += 1;
                    records.receipts.insert(positions[0] + 1, receipt);
                } else {
                    let first_us = records.receipts[positions[0]].received_us;
                    let second_us = records.receipts[positions[1]].received_us;
                    records.receipts.swap(positions[0], positions[1]);
                    records.receipts[positions[0]].received_us = first_us;
                    records.receipts[positions[1]].received_us = second_us;
                }
            }
            let summary = super::summarize(
                &plans,
                &roster,
                &records,
                1_000,
                96,
                DeliveryClass::Reliable,
                &churn,
                None,
            );
            let first = summary
                .reasons
                .iter()
                .find_map(|reason| match reason {
                    InvalidReason::DuplicateDeliveries { count: 2, first } if duplicate => {
                        Some(first)
                    }
                    InvalidReason::OutOfOrderDeliveries { count: 2, first } if !duplicate => {
                        Some(first)
                    }
                    _ => None,
                })
                .expect("observed violation");
            assert_eq!(first.sender, "r0p2");
        }
    }

    // #1130: a rejoin must not erase earlier observed contract violations.
    #[test]
    fn closed_epochs_preserve_observed_arrival_and_gap_contracts() {
        use signal_fish_server::protocol::DeliveryGapReason;
        let mut missed = Vec::new();
        for class in [
            DeliveryClass::Reliable,
            DeliveryClass::Latest,
            DeliveryClass::Volatile,
        ] {
            let (context, churn) = crate::churn_context(class);
            let baseline = crate::churned_records(&context, &churn);
            let summarize = |records: &RunRecords| {
                super::summarize(
                    &context.plans,
                    &context.roster,
                    records,
                    1_000,
                    96,
                    class,
                    &churn,
                    None,
                )
            };
            assert!(summarize(&baseline).valid);
            for mutation in [
                "duplicate",
                "order",
                "gap_bounds",
                "gap_receipt",
                "gap_overlap",
                "legal_gap",
            ] {
                if class == DeliveryClass::Reliable && mutation.contains("gap") {
                    continue;
                }
                let mut records = baseline.clone();
                let positions: Vec<_> = records
                    .receipts
                    .iter()
                    .enumerate()
                    .filter_map(|(position, receipt)| {
                        (receipt.recipient == "r0p0"
                            && receipt.sender == "r0p1"
                            && receipt.epoch == 1)
                            .then_some(position)
                    })
                    .collect();
                assert_eq!(positions.len(), 2);
                match mutation {
                    "duplicate" => {
                        let mut receipt = records.receipts[positions[0]].clone();
                        receipt.received_us += 1;
                        records.receipts.insert(positions[0] + 1, receipt);
                    }
                    "order" => {
                        let first_us = records.receipts[positions[0]].received_us;
                        let second_us = records.receipts[positions[1]].received_us;
                        records.receipts.swap(positions[0], positions[1]);
                        records.receipts[positions[0]].received_us = first_us;
                        records.receipts[positions[1]].received_us = second_us;
                    }
                    _ => {
                        let reason = if class == DeliveryClass::Latest {
                            DeliveryGapReason::LatestSuperseded
                        } else {
                            DeliveryGapReason::VolatileDropped
                        };
                        let mut gap = crate::gap_for("r0p0", "r0p1", 0, reason);
                        if mutation == "gap_bounds" {
                            gap.from_seq = 3;
                            gap.to_seq = 3;
                        }
                        if mutation == "gap_overlap" || mutation == "legal_gap" {
                            records.receipts.remove(positions[0]);
                        }
                        records.gaps.push(gap.clone());
                        if mutation == "gap_overlap" {
                            records.gaps.push(gap);
                        }
                    }
                }
                let summary = summarize(&records);
                if mutation == "legal_gap" {
                    assert!(summary.valid, "{class:?} {mutation}: {:?}", summary.reasons);
                } else {
                    if !summary.reasons.iter().any(|reason| {
                        matches!(
                            (mutation, reason),
                            (
                                "duplicate",
                                InvalidReason::DuplicateDeliveries { count: 1, .. }
                            ) | (
                                "order",
                                InvalidReason::OutOfOrderDeliveries { count: 1, .. }
                            ) | (
                                "gap_bounds" | "gap_receipt" | "gap_overlap",
                                InvalidReason::InvalidGapReports { count: 1, .. }
                            )
                        )
                    }) {
                        missed.push(format!("{class:?} {mutation}: {:?}", summary.reasons));
                    }
                }
                let output = tempfile::tempdir().expect("closed epoch artifacts");
                crate::artifacts::write_deliveries(output.path(), &records).expect("write records");
                let replayed = crate::artifacts::read_records(output.path()).expect("read records");
                assert_eq!(
                    serde_json::to_value(summarize(&replayed)).unwrap(),
                    serde_json::to_value(&summary).unwrap()
                );
                if class == DeliveryClass::Reliable && mutation == "duplicate" {
                    // The small fixture intentionally records one sender. Full
                    // replay also reports the other peers' unsent work; compare
                    // its whole verdict against the same full declared shape.
                    let mut config = crate::scenario_config(crate::Encoding::V3Json);
                    config.players_per_room = 4;
                    config.warmup = std::time::Duration::ZERO;
                    config.duration = std::time::Duration::from_millis(400);
                    config.send_rate_per_sender = 10.0;
                    config.generator_lag_bound = std::time::Duration::from_micros(1_000);
                    config.churn = crate::ChurnSchedule::ReconnectBurst {
                        fraction_percent: 25,
                        start: std::time::Duration::from_millis(250),
                        window: std::time::Duration::from_millis(100),
                    };
                    let replay_listener = std::net::TcpListener::bind(("127.0.0.1", 0))
                        .expect("replay fixture listener");
                    let endpoint = format!(
                        "ws://{}",
                        replay_listener
                            .local_addr()
                            .expect("replay fixture address")
                    );
                    config.endpoint = Some(endpoint.clone());
                    crate::artifacts::write_manifest(
                        output.path(),
                        "closed-epoch",
                        &config,
                        crate::artifacts::WorkloadShape {
                            senders: 4,
                            recipients: 4,
                            scheduled_sends_per_sender: 4,
                            warmup_sends_per_sender: 0,
                            measured_sends_per_sender: 4,
                        },
                        crate::artifacts::ServerIdentity {
                            endpoint,
                            pid: None,
                            binary_sha256: None,
                            binary_bytes: None,
                            config_overlay_sha256: crate::diagnostics::sha256_bytes(
                                &serde_json::to_vec(&config.server_overlay).unwrap(),
                            ),
                            config_provenance: crate::artifacts::ConfigProvenance::UnknownExternal,
                        },
                        crate::artifacts::BuildIdentity::current(),
                    )
                    .expect("write replay manifest");
                    let (plans, churn) = crate::build_run_shape(&config).unwrap();
                    let expected = super::summarize(
                        &plans,
                        &context.roster,
                        &records,
                        1_000,
                        96,
                        class,
                        &churn,
                        None,
                    );
                    let replayed =
                        crate::artifacts::replay(output.path()).expect("replay closed epoch");
                    assert_eq!(
                        serde_json::to_value(&replayed).unwrap(),
                        serde_json::to_value(expected).unwrap()
                    );
                    assert!(replayed.reasons.iter().any(|reason| matches!(
                        reason,
                        InvalidReason::DuplicateDeliveries { count: 1, .. }
                    )));
                }
            }
        }
        assert!(missed.is_empty(), "missed observed violations: {missed:?}");
    }

    // #775/#636: fan-out summaries retain one sender spelling per run.
    #[test]
    fn summary_sender_names_share_storage_and_preserve_json_replay() {
        for unusual_names in [false, true] {
            let mut context = crate::unit_context();
            if unusual_names {
                let names = ["", "a", "aa", "β\"\\\n"];
                assert_eq!(context.plans.len(), names.len());
                for (plan, name) in context.plans.iter_mut().zip(names) {
                    plan.name = name.to_string();
                }
                context.roster = context
                    .plans
                    .iter()
                    .map(|plan| (plan.name.clone(), plan.room))
                    .collect();
            }
            let records = crate::complete_records(&context.plans);
            let summarize_records = |records: &RunRecords| {
                summarize(
                    &context.plans,
                    &context.roster,
                    records,
                    1000,
                    96,
                    DeliveryClass::Reliable,
                    &ChurnPlan::default(),
                    None,
                )
            };
            let summary = summarize_records(&records);
            assert!(summary.valid);
            let mut names = BTreeMap::new();
            let mut shared = 0;
            for outcome in &summary.per_recipient {
                let legacy: BTreeMap<String, u64> = outcome
                    .received
                    .iter()
                    .map(|(name, count)| (name.to_string(), *count))
                    .collect();
                assert_eq!(
                    serde_json::to_value(&outcome.received).unwrap(),
                    serde_json::to_value(&legacy).unwrap()
                );
                assert_eq!(
                    outcome
                        .received
                        .keys()
                        .map(|name| name.as_str())
                        .collect::<Vec<_>>(),
                    legacy.keys().map(String::as_str).collect::<Vec<_>>()
                );
                for name in outcome.received.keys() {
                    assert_eq!(
                        outcome.received.get(name.as_str()),
                        legacy.get(name.as_str())
                    );
                    if let Some(previous) = names.insert(name.as_str(), name.as_str().as_ptr()) {
                        assert_eq!(
                            previous,
                            name.as_str().as_ptr(),
                            "sender {name} must share storage"
                        );
                        shared += 1;
                    }
                }
            }
            assert!(shared > 0);
            let json = serde_json::to_value(&summary).unwrap();
            let decoded: OutcomeSummary = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), json);
            let output = tempfile::tempdir().unwrap();
            crate::artifacts::write_deliveries(output.path(), &records).unwrap();
            let replayed = crate::artifacts::read_records(output.path()).unwrap();
            assert_eq!(
                serde_json::to_value(summarize_records(&replayed)).unwrap(),
                json
            );
            assert_eq!(
                serde_json::to_value(&records).unwrap(),
                serde_json::to_value(replayed).unwrap()
            );
            drop(records);
            assert_eq!(serde_json::to_value(&summary).unwrap(), json);
        }
        let empty = summarize(
            &[],
            &[],
            &RunRecords::default(),
            1000,
            96,
            DeliveryClass::Reliable,
            &ChurnPlan::default(),
            None,
        );
        assert!(empty.valid);
        assert!(empty.per_recipient.is_empty());
    }

    /// Isolate retained sender-name maps from raw records and oracle scratch.
    /// Both modes emit the same JSON through the bounded artifact writer.
    #[test]
    #[ignore = "isolated summary sender-name memory profile; run in a fresh process"]
    fn generator_summary_sender_names_memory_profile() {
        let rooms = std::env::var("CAPACITY_SUMMARY_NAMES_ROOMS")
            .unwrap_or_else(|_| "250".into())
            .parse::<u32>()
            .expect("rooms");
        assert!((1..=1000).contains(&rooms));
        fn measure<Name: From<String> + Clone + Ord + serde::Serialize>(rooms: u32) {
            let started = std::time::Instant::now();
            let mut outcomes = Vec::with_capacity(rooms as usize * 64);
            for room in 0..rooms {
                let names: Vec<Name> = (0..64)
                    .map(|player| crate::config::RunConfig::peer_name(room, player).into())
                    .collect();
                for player in 0..64 {
                    let received: BTreeMap<Name, u64> = names
                        .iter()
                        .enumerate()
                        .filter(|(sender, _)| *sender != player)
                        .map(|(_, name)| (name.clone(), 43_200))
                        .collect();
                    outcomes.push(received);
                }
            }
            let retained_rss = crate::diagnostics::resident_memory_bytes(std::process::id());
            let build_us = started.elapsed().as_micros();
            let output = tempfile::tempdir().unwrap();
            let path = output.path().join("received.json");
            crate::artifacts::write_json(&path, &outcomes).unwrap();
            use sha2::{Digest, Sha256};
            use std::io::Read as _;
            let mut digest = Sha256::new();
            let mut file = std::fs::File::open(&path).unwrap();
            let mut buffer = [0_u8; 8192];
            loop {
                let read = file.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                digest.update(&buffer[..read]);
            }
            println!(
                "SUMMARY_SENDER_NAMES_PROFILE {}",
                serde_json::json!({
                    "rooms": rooms, "players_per_room": 64, "name_bytes": std::mem::size_of::<Name>(),
                    "entries": u64::from(rooms) * 64 * 63, "retained_rss_bytes": retained_rss,
                    "build_us": build_us, "json_bytes": std::fs::metadata(path).unwrap().len(),
                    "sha256": digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
                })
            );
            std::hint::black_box(outcomes);
        }
        if std::env::var_os("CAPACITY_SUMMARY_NAMES_STRINGS").is_some() {
            measure::<String>(rooms);
        } else {
            measure::<ReceiptName>(rooms);
        }
    }

    // #636: room indexing must preserve the stable per-viewer transition order.
    #[test]
    fn indexed_churn_events_match_independent_viewer_sorts() {
        let room_of = BTreeMap::from([
            ("", 0),
            ("alice", 0),
            ("βob", 0),
            ("other", 1),
            ("last", 1),
            ("idle", 2),
        ]);
        let names = ["", "alice", "βob", "other", "last", "absent"];
        let mut state = 0x2718_2818_u64;
        for count in 0..192 {
            let events: Vec<_> = (0..count)
                .map(|position| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    ChurnEvent {
                        recipient: names[usize::try_from(state % 6).unwrap()].to_string(),
                        phase: if state & 8 == 0 {
                            ChurnPhase::Disconnect
                        } else {
                            ChurnPhase::Rejoined
                        },
                        at_us: [0, 1, 1, 2, u64::MAX][usize::try_from((state >> 8) % 5).unwrap()],
                        epoch: Some(position),
                        tails: BTreeMap::new(),
                    }
                })
                .collect();
            let rooms = room_churn_events(&events, &room_of);
            assert_eq!(
                rooms.values().map(Vec::len).sum::<usize>(),
                events
                    .iter()
                    .filter(|event| room_of.contains_key(event.recipient.as_str()))
                    .count()
            );
            for (viewer, room) in &room_of {
                let mut expected: Vec<_> = events
                    .iter()
                    .filter(|event| room_of.get(event.recipient.as_str()) == Some(room))
                    .collect();
                expected.sort_by_key(|event| (event.at_us, event.recipient != *viewer));
                let actual: Vec<_> =
                    viewer_churn_events(rooms.get(room).map_or(&[], Vec::as_slice), viewer)
                        .map(std::ptr::from_ref)
                        .collect();
                assert_eq!(
                    actual,
                    expected
                        .into_iter()
                        .map(std::ptr::from_ref)
                        .collect::<Vec<_>>(),
                    "count={count}, viewer={viewer:?}"
                );
            }
        }
    }

    // #775/#636: ordinary rooms share their immutable epoch evidence while
    // affected rooms retain exact per-seat snapshots and unresolved identities.
    #[test]
    fn churn_views_share_only_unchanged_rooms_and_preserve_exact_evidence() {
        let roster: Vec<_> = [("alice", 0), ("βob", 0), ("other", 1), ("last", 1)]
            .into_iter()
            .map(|(name, room)| (name.to_string(), room))
            .collect();
        let blocks = BTreeMap::new();
        let mut records = RunRecords::default();
        let (initial, unresolved) = churn_views(&records, &roster, &blocks);
        assert!(unresolved.is_empty());
        assert!(Arc::ptr_eq(&initial["alice"], &initial["βob"]));
        assert!(Arc::ptr_eq(&initial["other"], &initial["last"]));
        assert!(!Arc::ptr_eq(&initial["alice"], &initial["other"]));

        // Disconnects change no epoch or snapshot state. Force each room
        // through the existing viewer-specific event path as an exact control.
        for recipient in ["alice", "other"] {
            records.churn.push(crate::records::ChurnEvent {
                recipient: recipient.to_string(),
                phase: ChurnPhase::Disconnect,
                at_us: 10,
                epoch: None,
                tails: BTreeMap::new(),
            });
        }
        let (unshared, unresolved) = churn_views(&records, &roster, &blocks);
        assert!(unresolved.is_empty());
        assert_eq!(initial, unshared);
        assert!(!Arc::ptr_eq(&unshared["alice"], &unshared["βob"]));
        assert!(!Arc::ptr_eq(&unshared["other"], &unshared["last"]));

        records.churn.truncate(1);
        records
            .registry
            .insert("bob-id".to_string(), ("βob".to_string(), 1));
        records.churn.push(crate::records::ChurnEvent {
            recipient: "alice".to_string(),
            phase: ChurnPhase::Rejoined,
            at_us: 100,
            epoch: Some(2),
            tails: BTreeMap::from([("βob".to_string(), ("bob-id".to_string(), 5))]),
        });
        let (mixed, unresolved) = churn_views(&records, &roster, &blocks);
        assert!(unresolved.is_empty());
        assert!(Arc::ptr_eq(&mixed["other"], &mixed["last"]));
        assert!(!Arc::ptr_eq(&mixed["alice"], &mixed["βob"]));
        assert_eq!(mixed["other"], initial["other"]);
        assert_eq!(mixed["alice"].floors[&("βob", 1)], (5, 100, true));
        assert_eq!(mixed["βob"].closed_at[&("alice", 1)], 100);
        assert_eq!(mixed["βob"].adopted["alice"], [1, 2]);

        records.registry.clear();
        let (_, unresolved) = churn_views(&records, &roster, &blocks);
        assert_eq!(unresolved, ["bob-id"]);
        let (empty, unresolved) = churn_views(&records, &[], &blocks);
        assert!(empty.is_empty());
        assert!(unresolved.is_empty());
    }

    #[test]
    fn adopted_epochs_match_set_order_for_sparse_repeated_and_reversed_inputs() {
        for order in 0..3 {
            let mut epochs = Vec::new();
            let mut reference = BTreeSet::new();
            let mut input: Vec<_> = (0..256_u32).chain([u32::MAX, 0, 1, u32::MAX]).collect();
            if order == 1 {
                input.reverse();
            } else if order == 2 {
                let mut state = 1_u64;
                for end in (1..input.len()).rev() {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1);
                    let position =
                        usize::try_from(state % u64::try_from(end + 1).unwrap()).unwrap();
                    input.swap(end, position);
                }
            }
            for epoch in input {
                insert_epoch(&mut epochs, epoch);
                reference.insert(epoch);
                assert_eq!(epochs, reference.iter().copied().collect::<Vec<_>>());
                for query in [0, 1, 128, 255, 256, u32::MAX - 1, u32::MAX] {
                    assert_eq!(
                        epochs.binary_search(&query).is_ok(),
                        reference.contains(&query)
                    );
                }
            }
        }
    }

    // #775: reuse immutable roster spellings in every oracle epoch and floor map.
    #[test]
    fn churn_view_keys_borrow_roster_names() {
        let roster = vec![("αlice".to_string(), 0), ("bob".to_string(), 0)];
        let records = RunRecords {
            churn: vec![crate::records::ChurnEvent {
                recipient: "αlice".to_string(),
                phase: ChurnPhase::Rejoined,
                at_us: 100,
                epoch: Some(2),
                tails: BTreeMap::from([("bob".to_string(), ("bob-id".to_string(), 5))]),
            }],
            registry: BTreeMap::from([("bob-id".to_string(), ("bob".to_string(), 2))]),
            ..RunRecords::default()
        };
        let blocks = BTreeMap::new();
        let (views, unresolved) = churn_views(&records, &roster, &blocks);
        assert!(unresolved.is_empty());
        let sources: Vec<_> = roster.iter().map(|(name, _)| name.as_ptr()).collect();
        for (viewer, view) in &views {
            assert!(sources.contains(&viewer.as_ptr()), "viewer name was copied");
            for name in view
                .adopted
                .keys()
                .chain(view.closed_at.keys().map(|(name, _)| name))
                .chain(view.floors.keys().map(|(name, _)| name))
            {
                assert!(sources.contains(&name.as_ptr()), "stream name was copied");
            }
        }
    }

    /// Isolate retained oracle views from event storage and summary serialization.
    #[test]
    #[ignore = "churn-view memory profile; run in a fresh process"]
    fn generator_churn_view_memory_profile() {
        let rooms = std::env::var("CAPACITY_VIEW_PROBE_ROOMS")
            .unwrap_or_else(|_| "1000".to_string())
            .parse::<u32>()
            .expect("room count");
        let players = std::env::var("CAPACITY_VIEW_PROBE_PLAYERS")
            .unwrap_or_else(|_| "16".to_string())
            .parse::<u32>()
            .expect("player count");
        assert!((1..=2000).contains(&rooms));
        assert!((1..=128).contains(&players));
        let roster: Vec<_> = (0..rooms)
            .flat_map(|room| (0..players).map(move |player| (format!("r{room}p{player}"), room)))
            .collect();
        let mut records = RunRecords::default();
        if std::env::var_os("CAPACITY_VIEW_PROBE_CHURN").is_some() {
            for (recipient, _) in &roster {
                records.churn.push(crate::records::ChurnEvent {
                    recipient: recipient.clone(),
                    phase: ChurnPhase::Rejoined,
                    at_us: 100,
                    epoch: Some(2),
                    tails: BTreeMap::new(),
                });
            }
        }
        let blocks = BTreeMap::new();
        let before = crate::diagnostics::resident_memory_bytes(std::process::id());
        let started = std::time::Instant::now();
        let (views, unresolved) = churn_views(&records, &roster, &blocks);
        assert!(unresolved.is_empty());
        assert_eq!(views.len(), roster.len());
        assert!(views
            .values()
            .all(|view| view.adopted.len() == usize::try_from(players).unwrap()));
        println!(
            "{}",
            serde_json::json!({
                "rooms": rooms, "players": players, "views": views.len(),
                "rss_before_bytes": before,
                "rss_retained_bytes": crate::diagnostics::resident_memory_bytes(std::process::id()),
                "elapsed_us": started.elapsed().as_micros(),
            })
        );
        std::hint::black_box(views);
    }

    #[test]
    fn compact_recipient_latency_matches_wide_counts_and_preserves_raw_evidence() {
        let context = crate::unit_context();
        let mut records = crate::complete_records(&context.plans);
        for (index, receipt) in records.receipts.iter_mut().enumerate() {
            receipt.received_us = [0, 8, 500_000, 180_000_008, u64::MAX][index % 5];
        }
        let mut extra = records.receipts[0].clone();
        extra.recipient = "unknown-recipient".into();
        records
            .receipts
            .extend([extra, records.receipts[0].clone()]);
        let mut malformed = records.receipts[0].clone();
        malformed.epoch = u32::MAX;
        malformed.server_seq = 0;
        records.receipts.push(malformed.clone());
        malformed.recipient = malformed.sender.clone();
        records.receipts.push(malformed.clone());
        malformed.sender = "unknown-sender".into();
        let mut unplanned = records.sent[0].clone();
        unplanned.sender = malformed.sender.clone();
        records.sent.push(unplanned);
        records.receipts.push(malformed);
        let before = serde_json::to_value(&records).expect("raw records");
        let baseline = summarize(
            &context.plans,
            &context.roster,
            &records,
            1000,
            96,
            DeliveryClass::Reliable,
            &ChurnPlan::default(),
            None,
        );
        let members = context
            .roster
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        let mut compact_peers = baseline.per_recipient.clone();
        let mut wide_peers = compact_peers.clone();
        let compact = recipient_latency_statistics::<u32>(
            &records,
            &SendIndex::new(&records.sent),
            &members,
            &mut compact_peers,
            count_u64(records.receipts.len()),
        );
        let wide = recipient_latency_statistics::<u64>(
            &records,
            &SendIndex::new(&records.sent),
            &members,
            &mut wide_peers,
            count_u64(records.receipts.len()),
        );
        assert_eq!(
            serde_json::to_value(compact).unwrap(),
            serde_json::to_value(wide).unwrap()
        );
        assert_eq!(
            serde_json::to_value(compact_peers).unwrap(),
            serde_json::to_value(wide_peers).unwrap()
        );
        let mut legacy = baseline.clone();
        legacy.latency_us = recipient_latency_statistics::<u64>(
            &records,
            &SendIndex::new(&records.sent),
            &members,
            &mut legacy.per_recipient,
            count_u64(records.receipts.len()),
        );
        assert_eq!(
            serde_json::to_value(&baseline).unwrap(),
            serde_json::to_value(&legacy).unwrap()
        );
        let output = tempfile::tempdir().expect("latency replay");
        crate::artifacts::write_deliveries(output.path(), &records).expect("write latency events");
        let replayed = crate::artifacts::read_records(output.path()).expect("read latency events");
        let replay_summary = summarize(
            &context.plans,
            &context.roster,
            &replayed,
            1000,
            96,
            DeliveryClass::Reliable,
            &ChurnPlan::default(),
            None,
        );
        assert_eq!(
            serde_json::to_value(&legacy).unwrap(),
            serde_json::to_value(replay_summary).unwrap()
        );
        assert_eq!(before, serde_json::to_value(&replayed).unwrap());
        assert_eq!(before, serde_json::to_value(&records).unwrap());
        for records in [&records, &RunRecords::default()] {
            let compact = recipient_latency_statistics::<u32>(
                records,
                &SendIndex::new(&records.sent),
                &BTreeSet::new(),
                &mut [],
                count_u64(records.receipts.len()),
            );
            let wide = recipient_latency_statistics::<u64>(
                records,
                &SendIndex::new(&records.sent),
                &BTreeSet::new(),
                &mut [],
                count_u64(records.receipts.len()),
            );
            assert_eq!(
                serde_json::to_value(compact).unwrap(),
                serde_json::to_value(wide).unwrap()
            );
        }

        // Every inclusive counter limit is safe. The next raw arrival must
        // widen counters before a saturated HDR bucket can lose evidence.
        for (receipts, expected_bytes) in [
            (0, 1),
            (u64::from(u8::MAX), 1),
            (u64::from(u8::MAX) + 1, 2),
            (u64::from(u16::MAX), 2),
            (u64::from(u16::MAX) + 1, 4),
            (u64::from(u32::MAX), 4),
            (u64::from(u32::MAX) + 1, 8),
            (u64::MAX, 8),
        ] {
            assert_eq!(
                recipient_latency_counter_bytes(receipts),
                expected_bytes,
                "raw receipt bound {receipts}"
            );
        }
        fn full_bucket_matches_wide<T: Counter>(maximum: u64) {
            let mut narrow = Histogram::<T>::new(3).unwrap();
            let mut wide = Histogram::<u64>::new(3).unwrap();
            narrow.record_n(8, T::from_u64(maximum).unwrap()).unwrap();
            wide.record_n(8, maximum).unwrap();
            assert_eq!(narrow.len(), wide.len());
            for quantile in [0.5, 0.95, 0.99] {
                assert_eq!(
                    narrow.value_at_quantile(quantile),
                    wide.value_at_quantile(quantile)
                );
            }
        }
        full_bucket_matches_wide::<u8>(u64::from(u8::MAX));
        full_bucket_matches_wide::<u16>(u64::from(u16::MAX));
        full_bucket_matches_wide::<u32>(u64::from(u32::MAX));
        full_bucket_matches_wide::<u64>(u64::MAX);
    }

    // #775/#636: all arrivals can hit one HDR bucket. Duplicates, self-echoes,
    // and opaque leaks must widen its counter even though they fail delivery checks.
    #[test]
    fn recipient_latency_width_changes_preserve_full_hot_buckets() {
        for count in [255, 256, 65_535, 65_536] {
            for opaque in [false, true] {
                let context = if opaque {
                    crate::unit_context_with_experiment()
                } else {
                    crate::unit_context()
                };
                let mut records = crate::complete_records(&context.plans);
                let mut receipt = records
                    .receipts
                    .iter()
                    .find(|receipt| !opaque || receipt.sender.as_str() == context.plans[0].name)
                    .expect("opaque sender has receipts")
                    .clone();
                // A self-echo still contributes to this peer's latency population.
                receipt.recipient = receipt.sender.clone();
                receipt.received_us = u64::MAX;
                records.receipts = vec![receipt; count];
                let summary = summarize(
                    &context.plans,
                    &context.roster,
                    &records,
                    1000,
                    96,
                    DeliveryClass::Reliable,
                    &ChurnPlan::default(),
                    context.experiment,
                );
                let members = context
                    .roster
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect();
                let mut wide_peers = summary.per_recipient.clone();
                let wide = recipient_latency_statistics::<u64>(
                    &records,
                    &SendIndex::new(&records.sent),
                    &members,
                    &mut wide_peers,
                    count_u64(count),
                );
                assert_eq!(summary.latency_us.samples, count_u64(count));
                assert_eq!(
                    serde_json::to_value(summary.latency_us).unwrap(),
                    serde_json::to_value(wide).unwrap(),
                    "count={count}, opaque={opaque}"
                );
                assert_eq!(
                    serde_json::to_value(&summary.per_recipient).unwrap(),
                    serde_json::to_value(wide_peers).unwrap(),
                    "count={count}, opaque={opaque}"
                );
                assert_eq!(records.receipts.len(), count);
            }
        }
    }

    #[test]
    #[ignore = "recipient HDR memory profile; run in a fresh process"]
    fn generator_recipient_latency_memory_profile() {
        let peers = std::env::var("CAPACITY_LATENCY_PROBE_PEERS")
            .unwrap_or_else(|_| "1000".into())
            .parse::<usize>()
            .expect("peer count");
        assert!((1..=16000).contains(&peers));
        let compact = std::env::var("CAPACITY_LATENCY_PROBE_COMPACT").as_deref() == Ok("1");
        let counter_bytes = std::env::var("CAPACITY_LATENCY_PROBE_COUNTER_BYTES")
            .map(|value| value.parse::<usize>().expect("counter bytes"))
            .unwrap_or(if compact { 4 } else { 8 });
        fn measure<T: Counter>(peers: usize) {
            let started = std::time::Instant::now();
            let mut histograms: Vec<LatencyAccumulator<T>> =
                (0..peers).map(|_| LatencyAccumulator::default()).collect();
            for histogram in &mut histograms {
                // Sweep low and high buckets, including a long-tail sample.
                for sample in 0..4096 {
                    histogram.record(sample * 251);
                }
            }
            let mut checksum = 0_u64;
            let mut bucket_bytes = 0_usize;
            for histogram in &histograms {
                let stats = histogram.stats();
                assert_eq!(stats.samples, 4096);
                assert_eq!(stats.max_us, 4095 * 251);
                checksum += stats.p50_us + stats.p95_us + stats.p99_us + stats.samples;
                bucket_bytes += histogram.histogram.distinct_values() * std::mem::size_of::<T>();
            }
            println!(
                "RECIPIENT_LATENCY_PROFILE {}",
                serde_json::json!({
                    "peers": peers, "counter_bytes": std::mem::size_of::<T>(),
                    "bucket_bytes": bucket_bytes, "checksum": checksum,
                    "rss_bytes": crate::diagnostics::resident_memory_bytes(std::process::id()),
                    "elapsed_us": started.elapsed().as_micros(),
                })
            );
            std::hint::black_box(histograms);
        }
        match counter_bytes {
            2 => measure::<u16>(peers),
            4 => measure::<u32>(peers),
            8 => measure::<u64>(peers),
            _ => panic!("counter bytes must be 2, 4, or 8 for 4096 samples"),
        }
    }

    #[test]
    fn scheduled_sends_cannot_complete_before_their_offered_time() {
        let context = crate::unit_context();
        let baseline = crate::complete_records(&context.plans);
        let position = baseline
            .sent
            .iter()
            .position(|send| send.intended_us > 0)
            .unwrap();
        for offset in [-1_i64, 0, 1] {
            let mut records = baseline.clone();
            let send = &mut records.sent[position];
            send.sent_us = send.intended_us.checked_add_signed(offset).unwrap();
            let output = tempfile::tempdir().unwrap();
            crate::artifacts::write_deliveries(output.path(), &records).unwrap();
            let replayed = crate::artifacts::read_records(output.path()).unwrap();
            let mut exact_summary = None;
            for evidence in [&records, &replayed] {
                let summary = summarize(
                    &context.plans,
                    &context.roster,
                    evidence,
                    u64::MAX,
                    96,
                    DeliveryClass::Reliable,
                    &ChurnPlan::default(),
                    None,
                );
                assert_eq!(
                    summary.valid,
                    offset >= 0,
                    "{offset}: {:?}",
                    summary.reasons
                );
                if offset < 0 {
                    assert!(summary
                        .reasons
                        .contains(&InvalidReason::InvalidScheduledSends {
                            count: 1,
                            first: ScheduledSendViolation {
                                sender: records.sent[position].sender.to_string(),
                                seq: records.sent[position].seq,
                                mismatch: SendPlanMismatch::BeforeScheduledTime,
                            },
                        }));
                }
                assert_eq!(summary.totals.unsent, u64::from(offset < 0));
                assert_eq!(summary.totals.sent, baseline.sent.len() as u64);
                let value = serde_json::to_value(summary).unwrap();
                assert_eq!(exact_summary.get_or_insert_with(|| value.clone()), &value);
            }
        }
    }

    // #1159: an announced epoch does not prove that a send occurred while live.
    #[test]
    fn scheduled_sends_require_an_active_incarnation_at_completion() {
        let context = crate::unit_context();
        let base = crate::complete_records(&context.plans).sent[0].clone();
        for (epoch, sent_us, valid) in [
            (1, 9, true),
            (1, 10, true),
            (1, 11, false),
            (1, 21, false),
            (2, 19, false),
            (2, 20, true),
            (2, 25, true),
            (2, 30, true),
            (2, 31, false),
            (2, 41, false),
            (3, 39, false),
            (3, 40, true),
            (3, 41, true),
        ] {
            let mut records = RunRecords::default();
            records.sent.push(SentEvent {
                epoch,
                sent_us: base.intended_us + sent_us,
                ..base.clone()
            });
            for (phase, at_us, epoch) in [
                (ChurnPhase::Disconnect, 10, None),
                (ChurnPhase::Rejoined, 20, Some(2)),
                (ChurnPhase::Disconnect, 30, None),
                (ChurnPhase::Rejoined, 40, Some(3)),
            ] {
                records.churn.push(crate::records::ChurnEvent {
                    recipient: base.sender.to_string(),
                    phase,
                    at_us: base.intended_us + at_us,
                    epoch,
                    tails: BTreeMap::new(),
                });
            }
            for reverse in [false, true] {
                if reverse {
                    records.churn.reverse();
                }
                let mut reasons = Vec::new();
                let completed = scheduled_send_evidence(
                    &context.plans,
                    &records,
                    &SendIndex::new(&records.sent),
                    &mut reasons,
                );
                assert_eq!(
                    completed,
                    u64::from(valid),
                    "epoch={epoch} at={sent_us}: {reasons:?}"
                );
                assert_eq!(
                    reasons.is_empty(),
                    valid,
                    "epoch={epoch} at={sent_us}: {reasons:?}"
                );
            }
        }
    }

    // #1159: closed-epoch exemptions must not hide impossible completed work.
    #[test]
    fn inactive_send_epochs_invalidate_the_complete_summary_and_exact_replay() {
        let (context, churn) = crate::churn_context(DeliveryClass::Reliable);
        let baseline = crate::churned_records(&context, &churn);
        let disconnect = baseline.churn[0].at_us;
        let rejoin = baseline.churn[1].at_us;
        for (epoch, sent_us) in [(1, disconnect + 1), (1, rejoin + 1), (2, rejoin - 1)] {
            let mut records = baseline.clone();
            let sent = records
                .sent
                .iter_mut()
                .find(|event| event.epoch == epoch)
                .unwrap();
            let sender = sent.sender.to_string();
            let seq = sent.seq;
            sent.sent_us = sent_us;
            if epoch == 1 {
                // Closed epochs owe no remaining deliveries. The old oracle
                // accepted this impossible send even with its entire fan-out absent.
                records.receipts.retain(|receipt| receipt.seq != seq);
            }
            let expected = InvalidReason::InvalidScheduledSends {
                count: 1,
                first: ScheduledSendViolation {
                    sender,
                    seq,
                    mismatch: SendPlanMismatch::InactiveEpoch { epoch },
                },
            };
            let output = tempfile::tempdir().unwrap();
            crate::artifacts::write_deliveries(output.path(), &records).unwrap();
            let replayed = crate::artifacts::read_records(output.path()).unwrap();
            let mut exact_summary = None;
            for evidence in [&records, &replayed] {
                let summary = summarize(
                    &context.plans,
                    &context.roster,
                    evidence,
                    u64::MAX,
                    96,
                    DeliveryClass::Reliable,
                    &churn,
                    None,
                );
                assert!(
                    !summary.valid,
                    "epoch={epoch} at={sent_us}: {:?}",
                    summary.reasons
                );
                assert!(summary.reasons.contains(&expected), "{:?}", summary.reasons);
                assert_eq!(summary.totals.unsent, 1);
                assert_eq!(summary.totals.sent, 4);
                let value = serde_json::to_value(summary).unwrap();
                assert_eq!(exact_summary.get_or_insert_with(|| value.clone()), &value);
            }
        }
    }

    #[test]
    fn churn_evidence_cannot_replace_planned_disconnects_with_rejoins() {
        let (context, churn) = crate::churn_context(DeliveryClass::Reliable);
        let baseline = crate::churned_records(&context, &churn);
        for mutation in [
            "missing_disconnect",
            "duplicate_disconnect",
            "extra_rejoin",
            "unknown_peer",
            "unplanned_peer",
            "early_disconnect",
            "early_rejoin",
            "rejoin_before_disconnect",
            "disconnect_epoch",
            "disconnect_tails",
            "missing_epoch",
            "zero_epoch",
            "reused_epoch",
            "skipped_epoch",
            "max_epoch",
            "missing_identity",
            "missing_rejoin",
        ] {
            let mut records = baseline.clone();
            let (count, recipient, phase, mismatch) = match mutation {
                "missing_disconnect" => {
                    records.churn.remove(0);
                    (
                        1,
                        "r0p1",
                        ChurnPhase::Rejoined,
                        ChurnMismatch::UnexpectedPhase,
                    )
                }
                "duplicate_disconnect" => {
                    records.churn.push(records.churn[0].clone());
                    (
                        1,
                        "r0p1",
                        ChurnPhase::Disconnect,
                        ChurnMismatch::UnexpectedPhase,
                    )
                }
                "extra_rejoin" => {
                    records.churn.push(records.churn[1].clone());
                    (
                        1,
                        "r0p1",
                        ChurnPhase::Rejoined,
                        ChurnMismatch::UnplannedAction,
                    )
                }
                "unknown_peer" | "unplanned_peer" => {
                    let peer = if mutation == "unknown_peer" {
                        "absent"
                    } else {
                        "r0p0"
                    };
                    for event in &mut records.churn {
                        event.recipient = peer.into();
                    }
                    (
                        2,
                        peer,
                        ChurnPhase::Disconnect,
                        if mutation == "unknown_peer" {
                            ChurnMismatch::UnknownRecipient
                        } else {
                            ChurnMismatch::UnplannedAction
                        },
                    )
                }
                "early_disconnect" => {
                    records.churn[0].at_us -= 1;
                    (
                        1,
                        "r0p1",
                        ChurnPhase::Disconnect,
                        ChurnMismatch::BeforeScheduledTime,
                    )
                }
                "early_rejoin" => {
                    records.churn[1].at_us -= 1;
                    (
                        1,
                        "r0p1",
                        ChurnPhase::Rejoined,
                        ChurnMismatch::BeforeScheduledTime,
                    )
                }
                "rejoin_before_disconnect" => {
                    records.churn[1].at_us = records.churn[0].at_us - 1;
                    (
                        1,
                        "r0p1",
                        ChurnPhase::Rejoined,
                        ChurnMismatch::UnexpectedPhase,
                    )
                }
                "disconnect_epoch" | "disconnect_tails" => {
                    if mutation == "disconnect_epoch" {
                        records.churn[0].epoch = Some(1);
                    } else {
                        records.churn[0].tails = records.churn[1].tails.clone();
                    }
                    (
                        1,
                        "r0p1",
                        ChurnPhase::Disconnect,
                        ChurnMismatch::DisconnectMetadata,
                    )
                }
                "missing_epoch" | "zero_epoch" | "reused_epoch" | "skipped_epoch" | "max_epoch" => {
                    let actual = match mutation {
                        "missing_epoch" => None,
                        "zero_epoch" => Some(0),
                        "reused_epoch" => Some(1),
                        "skipped_epoch" => Some(3),
                        _ => Some(u32::MAX),
                    };
                    records.churn[1].epoch = actual;
                    (
                        1,
                        "r0p1",
                        ChurnPhase::Rejoined,
                        ChurnMismatch::RejoinEpoch {
                            expected: 2,
                            actual,
                        },
                    )
                }
                "missing_identity" => {
                    records.registry.remove("id-r0p1b");
                    (
                        1,
                        "r0p1",
                        ChurnPhase::Rejoined,
                        ChurnMismatch::UnregisteredEpoch { epoch: 2 },
                    )
                }
                "missing_rejoin" => {
                    records.churn.pop();
                    let summary = summarize(
                        &context.plans,
                        &context.roster,
                        &records,
                        1_000,
                        96,
                        context.delivery_class,
                        &churn,
                        None,
                    );
                    assert!(summary.reasons.contains(&InvalidReason::ChurnNotPerformed {
                        peer: "r0p1".into()
                    }));
                    continue;
                }
                _ => unreachable!(),
            };
            let expected = InvalidReason::InvalidChurnEvents {
                count,
                first: ChurnViolation {
                    recipient: recipient.into(),
                    phase,
                    mismatch,
                },
            };
            let mut expected_summary = None;
            for reverse in [false, true] {
                if reverse {
                    records.churn.reverse();
                }
                let output = tempfile::tempdir().unwrap();
                crate::artifacts::write_deliveries(output.path(), &records).unwrap();
                let replayed = crate::artifacts::read_records(output.path()).unwrap();
                for evidence in [&records, &replayed] {
                    let summary = summarize(
                        &context.plans,
                        &context.roster,
                        evidence,
                        1_000,
                        96,
                        context.delivery_class,
                        &churn,
                        None,
                    );
                    assert!(!summary.valid, "{mutation}: {:?}", summary.reasons);
                    assert!(
                        summary.reasons.contains(&expected),
                        "{mutation}: {:?}",
                        summary.reasons
                    );
                    let value = serde_json::to_value(summary).unwrap();
                    assert_eq!(
                        expected_summary.get_or_insert_with(|| value.clone()),
                        &value,
                        "{mutation}"
                    );
                }
            }
        }
    }

    fn stable_churn_order(records: &RunRecords) -> Vec<(usize, usize)> {
        let mut events: Vec<_> = (0..records.churn.len())
            .map(|position| (position, 0))
            .collect();
        events.sort_by_key(|&(position, _)| {
            let event = &records.churn[position];
            (event.recipient.as_str(), event.at_us)
        });
        let mut disconnects = BTreeMap::<&str, usize>::new();
        for (position, ordinal) in &mut events {
            let event = &records.churn[*position];
            *ordinal = match event.phase {
                ChurnPhase::Disconnect => {
                    let next = disconnects.entry(event.recipient.as_str()).or_default();
                    let ordinal = next.saturating_mul(2);
                    *next = next.saturating_add(1);
                    ordinal
                }
                ChurnPhase::Rejoined => event
                    .epoch
                    .and_then(|epoch| usize::try_from(epoch.checked_sub(2)?).ok())
                    .map_or(usize::MAX, |epoch| {
                        epoch.saturating_mul(2).saturating_add(1)
                    }),
            };
        }
        events.sort_by_key(|&(position, ordinal)| {
            let event = &records.churn[position];
            (event.recipient.as_str(), event.at_us, ordinal)
        });
        events
    }

    fn churn_sort_fixture(count: usize) -> RunRecords {
        let mut records = RunRecords::default();
        for position in 0..count {
            records.churn.push(ChurnEvent {
                recipient: format!("peer-{}", position % 17),
                phase: if position.is_multiple_of(3) {
                    ChurnPhase::Disconnect
                } else {
                    ChurnPhase::Rejoined
                },
                at_us: (position % 7) as u64,
                epoch: [None, Some(0), Some(1), Some(2), Some(3), Some(u32::MAX)][position % 6],
                tails: BTreeMap::new(),
            });
        }
        records
    }

    #[test]
    fn churn_order_preserves_stable_raw_identity_for_ties_and_invalid_epochs() {
        for count in [0, 1, 2, 17, 257, 4096] {
            let mut records = churn_sort_fixture(count);
            for reversed in [false, true] {
                if reversed {
                    records.churn.reverse();
                }
                assert_eq!(
                    ordered_churn_events(&records),
                    stable_churn_order(&records),
                    "count={count}, reversed={reversed}"
                );
            }
        }
    }

    #[test]
    #[ignore = "isolated churn sort memory comparison; run in a fresh process"]
    fn generator_churn_sort_memory_profile() {
        let records = churn_sort_fixture(1_000_000);
        let stable = std::env::var_os("CAPACITY_CHURN_SORT_STABLE").is_some();
        let start = std::time::Instant::now();
        let ordered = if stable {
            stable_churn_order(&records)
        } else {
            ordered_churn_events(&records)
        };
        assert_eq!(ordered.len(), records.churn.len());
        assert!(ordered.windows(2).all(|pair| {
            let key = |(position, ordinal): (usize, usize)| {
                let event = &records.churn[position];
                (event.recipient.as_str(), event.at_us, ordinal, position)
            };
            key(pair[0]) <= key(pair[1])
        }));
        println!(
            "CHURN_SORT_PROFILE stable={stable} rows={} index_bytes={} elapsed_ms={}",
            ordered.len(),
            ordered.capacity() * std::mem::size_of::<(usize, usize)>(),
            start.elapsed().as_millis()
        );
        std::hint::black_box((records, ordered));
    }

    #[test]
    fn churn_evidence_accepts_delayed_tied_and_shuffled_repeated_cycles() {
        let roster = vec![("a".into(), 0), ("b".into(), 0)];
        let churn = crate::snapshot_churn_plan(&[
            ("a", 10, 20),
            ("b", 10, 20),
            ("a", 30, 40),
            ("b", 30, 40),
        ]);
        let mut records = RunRecords::default();
        for peer in ["a", "b"] {
            for (epoch, at_us) in [(2, 25), (3, 45)] {
                records
                    .registry
                    .insert(format!("{peer}-{epoch}"), (peer.into(), epoch));
                for phase in [ChurnPhase::Disconnect, ChurnPhase::Rejoined] {
                    records.churn.push(crate::records::ChurnEvent {
                        recipient: peer.into(),
                        phase,
                        at_us,
                        epoch: (phase == ChurnPhase::Rejoined).then_some(epoch),
                        tails: BTreeMap::new(),
                    });
                }
            }
        }
        for same_timestamp in [false, true] {
            if same_timestamp {
                for event in &mut records.churn {
                    event.at_us = 45;
                }
            }
            for reverse in [false, true] {
                if reverse {
                    records.churn.reverse();
                }
                let summary = summarize(
                    &[],
                    &roster,
                    &records,
                    1_000,
                    96,
                    DeliveryClass::Reliable,
                    &churn,
                    None,
                );
                assert!(summary.valid, "{:?}", summary.reasons);
            }
        }
        for mutation in [
            "missing_disconnect",
            "missing_rejoin",
            "extra_disconnect",
            "extra_rejoin",
        ] {
            let mut changed = records.clone();
            let phase = if mutation.ends_with("disconnect") {
                ChurnPhase::Disconnect
            } else {
                ChurnPhase::Rejoined
            };
            let index = changed
                .churn
                .iter()
                .position(|event| {
                    event.recipient == "a"
                        && event.phase == phase
                        && (phase == ChurnPhase::Disconnect || event.epoch == Some(2))
                })
                .unwrap();
            if mutation.starts_with("missing") {
                changed.churn.remove(index);
            } else {
                changed.churn.push(changed.churn[index].clone());
            }
            let (count, phase, mismatch) = match mutation {
                "missing_disconnect" => (1, ChurnPhase::Rejoined, ChurnMismatch::UnexpectedPhase),
                "missing_rejoin" => (2, ChurnPhase::Disconnect, ChurnMismatch::UnexpectedPhase),
                "extra_disconnect" => (1, ChurnPhase::Disconnect, ChurnMismatch::UnplannedAction),
                _ => (1, ChurnPhase::Rejoined, ChurnMismatch::UnexpectedPhase),
            };
            let summary = summarize(
                &[],
                &roster,
                &changed,
                1_000,
                96,
                DeliveryClass::Reliable,
                &churn,
                None,
            );
            assert!(
                summary
                    .reasons
                    .contains(&InvalidReason::InvalidChurnEvents {
                        count,
                        first: ChurnViolation {
                            recipient: "a".into(),
                            phase,
                            mismatch
                        }
                    }),
                "{mutation}: {:?}",
                summary.reasons
            );
        }
    }

    #[test]
    fn snapshot_evidence_rejects_wrong_members_identities_and_impossible_tails() {
        let (context, churn) = crate::churn_context(DeliveryClass::Reliable);
        let baseline = crate::churned_records(&context, &churn);
        let summarize = |records: &RunRecords| {
            summarize(
                &context.plans,
                &context.roster,
                records,
                1_000,
                96,
                context.delivery_class,
                &churn,
                None,
            )
        };
        assert!(summarize(&baseline).valid);
        for mutation in [
            "wrong_player",
            "foreign_member",
            "unknown_foreign_id",
            "unknown_member_id",
            "excess_tail",
            "zero_epoch",
            "unannounced_epoch",
            "future_tail",
        ] {
            let mut records = baseline.clone();
            let rejoin = records
                .churn
                .iter_mut()
                .find(|event| event.phase == ChurnPhase::Rejoined)
                .unwrap();
            let (member, mismatch) = match mutation {
                "wrong_player" => {
                    rejoin.tails.get_mut("r0p0").unwrap().0 = "id-r0p2".into();
                    (
                        "r0p0",
                        SnapshotTailMismatch::WrongPlayer {
                            registered_member: "r0p2".into(),
                        },
                    )
                }
                "foreign_member" => {
                    rejoin.tails.insert("foreign".into(), ("id-r0p2".into(), 0));
                    ("foreign", SnapshotTailMismatch::ForeignMember)
                }
                "unknown_foreign_id" => {
                    rejoin
                        .tails
                        .insert("foreign".into(), ("unrecorded".into(), 0));
                    ("foreign", SnapshotTailMismatch::ForeignMember)
                }
                "unknown_member_id" => {
                    rejoin.tails.get_mut("r0p0").unwrap().0 = "unrecorded".into();
                    ("r0p0", SnapshotTailMismatch::UnknownPlayerId)
                }
                "excess_tail" => {
                    rejoin.tails.get_mut("r0p0").unwrap().1 = u64::MAX;
                    (
                        "r0p0",
                        SnapshotTailMismatch::TailBeyondSends {
                            epoch: 1,
                            tail: u64::MAX,
                            sent: 0,
                        },
                    )
                }
                "zero_epoch" => {
                    records.registry.get_mut("id-r0p0").unwrap().1 = 0;
                    ("r0p0", SnapshotTailMismatch::InvalidEpoch)
                }
                "unannounced_epoch" => {
                    records.registry.get_mut("id-r0p0").unwrap().1 = u32::MAX;
                    (
                        "r0p0",
                        SnapshotTailMismatch::UnannouncedEpoch { epoch: u32::MAX },
                    )
                }
                "future_tail" => {
                    rejoin.tails.get_mut("r0p1").unwrap().1 = 1;
                    (
                        "r0p1",
                        SnapshotTailMismatch::TailBeforeScheduledSend {
                            epoch: 2,
                            tail: 1,
                            intended_us: records
                                .sent
                                .iter()
                                .find(|send| send.epoch == 2)
                                .unwrap()
                                .intended_us,
                            snapshot_us: rejoin.at_us,
                        },
                    )
                }
                _ => unreachable!(),
            };
            let rejoin = records
                .churn
                .iter()
                .find(|event| event.phase == ChurnPhase::Rejoined)
                .unwrap();
            let expected = InvalidReason::InvalidSnapshotTails {
                count: 1,
                first: SnapshotTailViolation {
                    recipient: rejoin.recipient.clone(),
                    member: member.into(),
                    player_id: rejoin.tails[member].0.clone(),
                    mismatch,
                },
            };
            let expected_summary = serde_json::to_value(summarize(&records)).unwrap();
            for reverse in [false, true] {
                if reverse {
                    records.sent.reverse();
                    records.churn.reverse();
                }
                let output = tempfile::tempdir().unwrap();
                crate::artifacts::write_deliveries(output.path(), &records).unwrap();
                let replayed = crate::artifacts::read_records(output.path()).unwrap();
                assert_eq!(
                    serde_json::to_value(&records).unwrap(),
                    serde_json::to_value(&replayed).unwrap()
                );
                for evidence in [&records, &replayed] {
                    let summary = summarize(evidence);
                    assert!(!summary.valid, "{mutation}");
                    assert!(
                        summary.reasons.contains(&expected),
                        "{mutation}: {:?}",
                        summary.reasons
                    );
                    assert_eq!(
                        serde_json::to_value(summary).unwrap(),
                        expected_summary,
                        "{mutation}"
                    );
                }
            }
        }
    }

    #[test]
    fn snapshot_evidence_accepts_write_completion_and_rejoin_timestamp_inversions() {
        let (context, churn) = crate::churn_context(DeliveryClass::Reliable);
        for inversion in ["write_completion", "epoch_announcement"] {
            let mut records = crate::churned_records(&context, &churn);
            let (snapshot_us, player_id, tail) = match inversion {
                "write_completion" => {
                    let send = records
                        .sent
                        .iter_mut()
                        .find(|send| send.epoch == 1)
                        .unwrap();
                    let snapshot_us = send.intended_us + 1;
                    // The server can include the frame before the writer task
                    // records completion of its write.
                    send.sent_us = snapshot_us + 1;
                    let seq = send.seq;
                    records
                        .receipts
                        .retain(|receipt| receipt.recipient != "r0p0" || receipt.seq != seq);
                    (snapshot_us, "id-r0p1", 1)
                }
                "epoch_announcement" => {
                    let announcement = records
                        .churn
                        .iter()
                        .find(|event| {
                            event.recipient == "r0p1" && event.phase == ChurnPhase::Rejoined
                        })
                        .unwrap();
                    // The other seat can observe this incarnation before the
                    // member's task records its own completed rejoin.
                    (announcement.at_us - 1, "id-r0p1b", 0)
                }
                _ => unreachable!(),
            };
            let mut churn = churn.clone();
            churn
                .cycles
                .extend(crate::snapshot_churn_plan(&[("r0p0", snapshot_us, snapshot_us)]).cycles);
            records.churn.push(crate::records::ChurnEvent {
                recipient: "r0p0".into(),
                phase: ChurnPhase::Disconnect,
                at_us: snapshot_us,
                epoch: None,
                tails: BTreeMap::new(),
            });
            records.churn.push(crate::records::ChurnEvent {
                recipient: "r0p0".into(),
                phase: ChurnPhase::Rejoined,
                at_us: snapshot_us,
                epoch: Some(2),
                tails: BTreeMap::from([("r0p1".into(), (player_id.into(), tail))]),
            });
            // The replay ledger need not arrive in sender or sequence order.
            records.sent.reverse();
            let summary = summarize(
                &context.plans,
                &context.roster,
                &records,
                1_000,
                96,
                context.delivery_class,
                &churn,
                None,
            );
            assert!(summary.valid, "{inversion}: {:?}", summary.reasons);
        }
    }

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
                assert!(matches!(block, RelayBlock::Borrowed { .. }));
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

    // #775: arbitrary artifact order must not retain a wide pointer per send.
    #[test]
    fn reordered_relay_block_retains_compact_offsets() {
        let context = crate::unit_context();
        let mut records = crate::complete_records(&context.plans);
        let sender = records.sent[0].sender.clone();
        records.sent.retain(|event| event.sender == sender);
        records.sent.reverse();
        let blocks = relay_blocks(&records.sent);
        let retained: usize = blocks.values().map(RelayBlock::retained_index_bytes).sum();
        assert!(
            retained <= records.sent.len() * 4,
            "relay index retains {retained} bytes for {} sends",
            records.sent.len()
        );
    }

    #[test]
    fn interleaved_relay_blocks_retain_only_required_offsets() {
        let context = crate::unit_context();
        let template = crate::complete_records(&context.plans).sent.remove(0);
        for count in [3, 5, 17, 1001, 50_000] {
            let mut sent = Vec::new();
            for position in 0..count {
                let mut event = template.clone();
                event.sender = format!("peer{}", position % 7).into();
                event.epoch = u32::try_from(position % 3).expect("three epochs");
                event.seq = u64::try_from(count - position).expect("bounded sequence");
                sent.push(event);
            }
            let blocks = relay_blocks(&sent);
            let retained: usize = blocks.values().map(RelayBlock::retained_index_bytes).sum();
            assert!(
                retained <= count * 4,
                "{count} sends retain {retained} offset bytes, budget {}",
                count * 4
            );
        }
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
                // The wide-host fallback retains these same pointer-ledger semantics.
                let wide = RelayBlock::Indexed(events.clone());
                for block in [&actual[&key], &wide] {
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
                    assert!(block.get(usize::MAX).is_none());
                }
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
                let actual = payload_size_evidence(
                    &records,
                    &SendIndex::new(&records.sent),
                    96,
                    &mut reasons,
                );
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

    // #775/#636: compact replay offsets add only one descriptor per sender.
    #[test]
    fn reordered_send_index_retains_compact_offsets_and_stable_duplicate_order() {
        let context = crate::unit_context();
        let mut records = crate::complete_records(&context.plans);
        records.sent.reverse();
        let mut duplicate = records.sent[0].clone();
        duplicate.sent_us = u64::MAX;
        records.sent.push(duplicate);
        let mut expected: Vec<_> = records.sent.iter().collect();
        expected.sort_by_key(|event| (event.sender.as_str(), event.seq));
        let index = SendIndex::new(&records.sent);
        assert!(
            index.retained_index_bytes()
                <= records.sent.len() * 4
                    + expected
                        .iter()
                        .map(|event| event.sender.as_str())
                        .collect::<BTreeSet<_>>()
                        .len()
                        * std::mem::size_of::<SenderOffsets>(),
            "replay index retains {} bytes for {} sends",
            index.retained_index_bytes(),
            records.sent.len(),
        );
        assert_eq!(
            index
                .sorted_events()
                .map(std::ptr::from_ref)
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|event| std::ptr::from_ref(*event))
                .collect::<Vec<_>>(),
            "raw order breaks ties without changing any send",
        );
        let expected: BTreeMap<_, _> = records
            .sent
            .iter()
            .map(|event| ((event.sender.as_str(), event.seq), event))
            .collect();
        for ((sender, seq), event) in expected {
            assert!(std::ptr::eq(index.get(sender, seq).unwrap(), event));
        }
    }

    #[test]
    #[ignore = "isolated replay index memory profile; run in a fresh process"]
    fn generator_reordered_send_index_memory_profile() {
        let count = std::env::var("CAPACITY_SEND_INDEX_PROBE_COUNT")
            .unwrap_or_else(|_| "1000000".into())
            .parse::<usize>()
            .expect("send count");
        assert!((1..=8_000_000).contains(&count));
        let kind =
            std::env::var("CAPACITY_SEND_INDEX_PROBE_KIND").unwrap_or_else(|_| "compact".into());
        assert!(["legacy", "compact", "ordered"].contains(&kind.as_str()));
        let context = crate::unit_context();
        let template = crate::complete_records(&context.plans).sent.remove(0);
        let names: Vec<ReceiptName> = (0..64).map(|peer| format!("r0p{peer}").into()).collect();
        let mut events = Vec::with_capacity(count);
        for position in 0..count {
            // Deterministic scrambled keys and multiple epochs; widen before multiplying.
            let key = usize::try_from(position as u64 * 48_271 % count as u64)
                .expect("bounded probe key");
            let mut event = template.clone();
            event.sender = names[key % names.len()].clone();
            event.seq = (key / names.len() / 2) as u64;
            event.epoch = 1 + u32::try_from(position % 3).expect("three epochs");
            event.sent_us = position as u64;
            events.push(event);
        }
        if kind == "ordered" {
            events.sort_by_key(|event| (event.sender.clone(), event.seq));
        }
        // During latency evaluation the relay ledger remains alive beside SendIndex.
        let blocks = relay_blocks(&events);
        let relay_index_bytes: usize = blocks.values().map(RelayBlock::retained_index_bytes).sum();
        let before_rss = crate::diagnostics::resident_memory_bytes(std::process::id());
        let started = std::time::Instant::now();
        let index = if kind == "legacy" {
            let mut ordered: Vec<_> = events.iter().collect();
            ordered.sort_by_key(|event| (event.sender.as_str(), event.seq));
            SendIndex::WideReordered(ordered)
        } else {
            SendIndex::new(&events)
        };
        let mut checksum = 0_u64;
        let mut visited = 0;
        for event in index.sorted_events() {
            checksum = checksum
                .wrapping_mul(1_099_511_628_211)
                .wrapping_add(event.sent_us)
                .wrapping_add(event.seq.rotate_left(17));
            visited += 1;
        }
        assert_eq!(visited, count);
        println!(
            "REORDERED_SEND_INDEX_PROFILE {}",
            serde_json::json!({
                "kind": kind, "sends": count, "checksum": checksum,
                "index_bytes": index.retained_index_bytes(),
                "relay_index_bytes": relay_index_bytes,
                "before_rss_bytes": before_rss,
                "retained_rss_bytes": crate::diagnostics::resident_memory_bytes(std::process::id()),
                "elapsed_us": started.elapsed().as_micros(),
            })
        );
        std::hint::black_box((&index, &blocks, &events));
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
            let mut stable: Vec<_> = sent.iter().collect();
            stable.sort_by_key(|event| (event.sender.as_str(), event.seq));
            let wide = SendIndex::WideReordered(stable.clone());
            assert_eq!(
                index
                    .sorted_events()
                    .map(std::ptr::from_ref)
                    .collect::<Vec<_>>(),
                stable
                    .iter()
                    .map(|event| std::ptr::from_ref(*event))
                    .collect::<Vec<_>>(),
            );
            if order == 2 {
                assert!(matches!(index, SendIndex::Ordered(_)));
            }
            for ((sender, seq), event) in &expected {
                for candidate in [&index, &wide] {
                    assert!(std::ptr::eq(
                        candidate.get(sender, *seq).expect("recorded key"),
                        *event
                    ));
                }
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

    #[test]
    fn reordered_send_lookup_matches_independent_map_across_sender_shapes() {
        let context = crate::unit_context();
        let template = crate::complete_records(&context.plans).sent.remove(0);
        let names = ["", "alice", "βob", "未知"];
        for layout in 0_u64..1024 {
            let mut sent = Vec::new();
            let mut state = layout + 1;
            for (sender_number, sender) in names.iter().enumerate() {
                let shape = (layout >> (sender_number * 2)) % 4;
                for offset in 0..17 {
                    let mut event = template.clone();
                    event.sender = (*sender).into();
                    event.seq = match shape {
                        0 => offset,
                        1 => u64::MAX - 16 + offset,
                        2 => offset * 2,
                        _ => offset / 2,
                    };
                    event.epoch = 1 + u32::try_from(offset % 3).unwrap();
                    event.sent_us = sent.len() as u64;
                    sent.push(event);
                }
            }
            for end in (1..sent.len()).rev() {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                let other = usize::try_from(state % (end + 1) as u64).unwrap();
                sent.swap(end, other);
            }
            let expected: BTreeMap<_, _> = sent
                .iter()
                .map(|event| ((event.sender.as_str(), event.seq), event))
                .collect();
            let index = SendIndex::new(&sent);
            for sender in names.into_iter().chain(["unknown"]) {
                for sequence in (0..35).chain(u64::MAX - 18..=u64::MAX) {
                    assert_eq!(
                        index.get(sender, sequence).map(std::ptr::from_ref),
                        expected
                            .get(&(sender, sequence))
                            .map(|event| std::ptr::from_ref(*event)),
                        "layout={layout} sender={sender:?} sequence={sequence}",
                    );
                }
            }
            let mut expected_order: Vec<_> = sent.iter().collect();
            expected_order.sort_by_key(|event| (event.sender.as_str(), event.seq));
            assert_eq!(
                index
                    .sorted_events()
                    .map(std::ptr::from_ref)
                    .collect::<Vec<_>>(),
                expected_order
                    .into_iter()
                    .map(std::ptr::from_ref)
                    .collect::<Vec<_>>(),
                "layout={layout}",
            );
        }
    }

    #[test]
    #[ignore = "isolated arrival-state memory profile; run in a fresh process"]
    fn generator_arrival_state_memory_profile() {
        let rooms: u32 = std::env::var("CAPACITY_ARRIVAL_PROBE_ROOMS")
            .unwrap_or_else(|_| "999".into())
            .parse()
            .expect("probe rooms");
        assert!((1..=999).contains(&rooms));
        let invalid = std::env::var_os("CAPACITY_ARRIVAL_PROBE_INVALID").is_some();
        let fragmented = std::env::var_os("CAPACITY_ARRIVAL_PROBE_FRAGMENTED").is_some();
        let started = std::time::Instant::now();
        let mut arrivals = BTreeMap::new();
        for room in 0..rooms {
            for recipient in 0_u8..16 {
                for sender in 0_u8..16 {
                    if recipient == sender {
                        continue;
                    }
                    let mut stream = ArrivalEvidence::default();
                    for sequence in 1..=100 {
                        stream.observe(if fragmented { sequence * 2 } else { sequence });
                    }
                    if invalid {
                        for sequence in [100, 0, 0] {
                            stream.observe(if fragmented { sequence * 2 } else { sequence });
                        }
                        stream.violations_mut().misrouted.record(0);
                    }
                    arrivals.insert((room, recipient, sender), stream);
                }
            }
        }
        let unique: u64 = arrivals.values().map(|stream| stream.unique).sum();
        assert_eq!(
            unique,
            u64::from(rooms) * 16 * 15 * (100 + u64::from(invalid))
        );
        println!(
            "ARRIVAL_STATE_PROFILE {}",
            serde_json::json!({
                "rooms": rooms, "streams": arrivals.len(), "invalid": invalid, "fragmented": fragmented,
                "stream_bytes": std::mem::size_of::<ArrivalEvidence<'_>>(),
                "retained_rss_bytes": crate::diagnostics::resident_memory_bytes(std::process::id()),
                "unique": unique, "elapsed_us": started.elapsed().as_micros(),
            })
        );
        std::hint::black_box(arrivals);
    }

    #[test]
    fn violation_counts_preserve_empty_zero_and_full_width_first_keys() {
        for sequences in [vec![], vec![0], vec![0, u64::MAX, 1], vec![u64::MAX, 0, 7]] {
            let mut violations = SequenceViolations::default();
            let mut category = Category::default();
            violations.append_to(&mut category, "recipient", "sender", u32::MAX);
            assert_eq!(category.count, 0);
            assert!(category.first.is_none());
            for &sequence in &sequences {
                violations.record(sequence);
            }
            violations.append_to(&mut category, "recipient", "sender", u32::MAX);
            assert_eq!(category.count, count_u64(sequences.len()));
            assert_eq!(
                category.first,
                sequences.first().map(|&seq| DeliveryKey {
                    recipient: "recipient".into(),
                    sender: "sender".into(),
                    epoch: u32::MAX,
                    seq,
                })
            );
            let first = category.first.clone();
            violations.append_to(&mut category, "later", "later", 0);
            assert_eq!(category.first, first);
        }
    }

    #[test]
    fn unexpected_sender_counts_each_receipt_and_preserves_first_key_and_replay() {
        for class in [
            DeliveryClass::Reliable,
            DeliveryClass::Latest,
            DeliveryClass::Volatile,
        ] {
            let mut config = crate::scenario_config(crate::Encoding::V2Json);
            config.rooms = 2;
            config.delivery_class = class;
            config.warmup = std::time::Duration::ZERO;
            config.duration = std::time::Duration::from_millis(400);
            config.send_rate_per_sender = 10.0;
            let (plans, churn) = crate::build_run_shape(&config).unwrap();
            let roster: Vec<_> = plans
                .iter()
                .map(|plan| (plan.name.clone(), plan.room))
                .collect();
            for sender in ["unknown-sender", "r0p0", "r1p1"] {
                for first in [0, u64::MAX] {
                    for duplicate in [false, true] {
                        let mut records = crate::complete_records(&plans);
                        let mut receipt = records.receipts[0].clone();
                        receipt.recipient = "r0p0".into();
                        receipt.sender = sender.into();
                        receipt.server_seq = first;
                        records.receipts.push(receipt.clone());
                        if duplicate {
                            records.receipts.push(receipt.clone());
                        }
                        receipt.server_seq = 7;
                        records.receipts.push(receipt);
                        let summarize = |records: &RunRecords| {
                            super::summarize(
                                &plans, &roster, records, 1_000, 96, class, &churn, None,
                            )
                        };
                        let summary = summarize(&records);
                        let count = 2 + u64::from(duplicate);
                        assert!(
                            summary
                                .reasons
                                .contains(&InvalidReason::MisroutedDeliveries {
                                    count,
                                    first: DeliveryKey {
                                        recipient: "r0p0".into(),
                                        sender: sender.into(),
                                        epoch: 1,
                                        seq: first,
                                    },
                                }),
                            "{class:?} sender={sender} first={first} duplicate={duplicate}: {:?}",
                            summary.reasons
                        );
                        assert_eq!(
                            summary
                                .per_recipient
                                .iter()
                                .find(|outcome| outcome.recipient == "r0p0")
                                .unwrap()
                                .misrouted,
                            count
                        );
                        assert!(!summary.valid);
                        let output = tempfile::tempdir().unwrap();
                        crate::artifacts::write_deliveries(output.path(), &records).unwrap();
                        let replayed = crate::artifacts::read_records(output.path()).unwrap();
                        assert_eq!(
                            serde_json::to_value(summarize(&replayed)).unwrap(),
                            serde_json::to_value(summary).unwrap()
                        );
                        assert_eq!(
                            serde_json::to_value(replayed).unwrap(),
                            serde_json::to_value(records).unwrap()
                        );
                    }
                }
            }
        }
    }

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
            let empty = ArrivalViolations::default();
            let violations = evidence.violations.as_deref().unwrap_or(&empty);
            assert_eq!(
                violations.duplicates.count,
                count_u64(duplicates.len()),
                "{sequences:?}"
            );
            assert_eq!(
                (violations.duplicates.count > 0).then_some(violations.duplicates.first),
                duplicates.first().copied(),
                "{sequences:?}"
            );
            assert_eq!(
                violations.out_of_order.count,
                count_u64(inversions.len()),
                "{sequences:?}"
            );
            assert_eq!(
                (violations.out_of_order.count > 0).then_some(violations.out_of_order.first),
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
        assert!(ordered.violations.is_none());
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
                    coverage
                        .ranges
                        .as_ref()
                        .is_none_or(|ranges| ranges.is_empty()),
                    "seq={seq} descending={descending}"
                );
            }
            assert_eq!(
                coverage.inline.map(|(start, end)| (start.get(), end)),
                Some((1, 100_000))
            );
            assert!(!coverage.insert(50_000, 100_001));
            assert_eq!(
                coverage.inline.map(|(start, end)| (start.get(), end)),
                Some((1, 100_000))
            );
            assert!(coverage.insert(100_002, 100_002));
            assert!(coverage.inline.is_none());
            assert_eq!(coverage.ranges.as_ref().map_or(0, |ranges| ranges.len()), 2);
            assert!(coverage.insert(100_001, 100_001));
            assert!(coverage
                .ranges
                .as_ref()
                .is_none_or(|ranges| ranges.is_empty()));
            assert_eq!(
                coverage.inline.map(|(start, end)| (start.get(), end)),
                Some((1, 100_002))
            );
            assert_eq!(coverage.above(99_999).covered, 3);
        }
        let mut fragmented = StreamCoverage::default();
        assert!(fragmented.insert(1, 1));
        for seq in 3..=100_000 {
            assert!(fragmented.insert(seq, seq));
            assert_eq!(
                fragmented.ranges.as_ref().map_or(0, |ranges| ranges.len()),
                2
            );
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
        assert!(fragmented
            .ranges
            .as_ref()
            .is_none_or(|ranges| ranges.is_empty()));
        assert_eq!(
            fragmented.inline.map(|(start, end)| (start.get(), end)),
            Some((1, 100_000))
        );
        for cycle in 0..2_000 {
            let base = 100_000 + cycle * 3;
            for seq in [base + 1, base + 3, base + 2] {
                assert!(fragmented.insert(seq, seq));
            }
            assert_eq!(
                fragmented.inline.map(|(start, end)| (start.get(), end)),
                Some((1, base + 3))
            );
            assert!(fragmented
                .ranges
                .as_ref()
                .is_none_or(|ranges| ranges.is_empty()));
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

    #[test]
    fn streamed_gaps_preserve_first_violation_order_and_exact_replay() {
        let context = crate::unit_context_with_class(DeliveryClass::Latest);
        for global_invalid in [false, true] {
            for reverse in [false, true] {
                let mut records = crate::complete_records(&context.plans);
                for (recipient, sender) in [("r0p1", "r0p0"), ("r0p0", "r0p1")] {
                    // Record order differs from roster order. The first
                    // stream violation must still follow roster order.
                    records.gaps.push(crate::gap_for(
                        recipient,
                        sender,
                        0,
                        signal_fish_server::protocol::DeliveryGapReason::LatestSuperseded,
                    ));
                }
                if reverse {
                    records.gaps.reverse();
                }
                if global_invalid {
                    let mut gap = records.gaps[0].clone();
                    gap.recipient = "outside-roster".into();
                    records.gaps.push(gap);
                }
                let summarize = |records: &RunRecords| {
                    super::summarize(
                        &context.plans,
                        &context.roster,
                        records,
                        1000,
                        96,
                        DeliveryClass::Latest,
                        &ChurnPlan::default(),
                        None,
                    )
                };
                let summary = summarize(&records);
                let invalid = summary
                    .reasons
                    .iter()
                    .find_map(|reason| match reason {
                        InvalidReason::InvalidGapReports { count, first } => Some((*count, first)),
                        _ => None,
                    })
                    .expect("invalid gaps");
                assert_eq!(invalid.0, if global_invalid { 3 } else { 2 });
                assert_eq!(
                    invalid.1.recipient,
                    if global_invalid {
                        "outside-roster"
                    } else {
                        "r0p0"
                    }
                );
                assert_eq!(summary.totals.gap_covered, 0);
                assert_eq!(summary.totals.outstanding, 0);
                let output = tempfile::tempdir().expect("gap artifacts");
                crate::artifacts::write_deliveries(output.path(), &records).expect("write gaps");
                let replayed = crate::artifacts::read_records(output.path()).expect("read gaps");
                assert_eq!(
                    serde_json::to_value(summarize(&replayed)).unwrap(),
                    serde_json::to_value(summary).unwrap()
                );
                assert_eq!(
                    serde_json::to_value(replayed).unwrap(),
                    serde_json::to_value(records).unwrap()
                );
            }
        }
    }

    #[test]
    #[ignore = "all-gap oracle memory profile; run in a fresh process"]
    fn generator_all_gap_oracle_memory_profile() {
        let sends = std::env::var("CAPACITY_GAP_ORACLE_PROBE_SENDS")
            .unwrap_or_else(|_| "100000".into())
            .parse::<u64>()
            .expect("send count");
        assert!((1..=1_000_000).contains(&sends));
        let mut config = crate::scenario_config(crate::Encoding::V3Json);
        config.players_per_room = 2;
        config.warmup = std::time::Duration::ZERO;
        config.duration = std::time::Duration::from_micros(sends);
        config.send_rate_per_sender = 1_000_000.0;
        let (plans, churn) = crate::build_run_shape(&config).expect("probe shape");
        let roster: Vec<_> = plans
            .iter()
            .map(|plan| (plan.name.clone(), plan.room))
            .collect();
        let mut records = RunRecords::default();
        for plan in &plans {
            let sender: crate::ReceiptName = plan.name.as_str().into();
            let recipient: crate::ReceiptName = plans
                .iter()
                .find(|peer| peer.name != plan.name)
                .expect("other peer")
                .name
                .as_str()
                .into();
            for send in &plan.sends {
                records.sent.push(SentEvent {
                    sender: sender.clone(),
                    room: plan.room,
                    seq: send.seq,
                    epoch: 1,
                    intended_us: send.intended_us,
                    sent_us: send.intended_us + 5,
                    application_bytes: 96,
                    encoded_frame_body_bytes: 150,
                    phase: send.phase,
                });
                records.gaps.push(GapEvent {
                    sender: sender.clone(),
                    recipient: recipient.clone(),
                    epoch: 1,
                    from_seq: send.seq + 1,
                    to_seq: send.seq + 1,
                    reason: signal_fish_server::protocol::DeliveryGapReason::LatestSuperseded,
                });
            }
        }
        let collected_rss = crate::diagnostics::resident_memory_bytes(std::process::id());
        let started = std::time::Instant::now();
        let summary = summarize(
            &plans,
            &roster,
            &records,
            1000,
            96,
            DeliveryClass::Latest,
            &churn,
            None,
        );
        assert!(summary.valid, "{:?}", summary.reasons);
        assert_eq!(summary.totals.gap_covered, sends * 2);
        println!(
            "GAP_ORACLE_PROFILE {}",
            serde_json::json!({
                "gaps": records.gaps.len(), "collected_rss_bytes": collected_rss,
                "after_oracle_rss_bytes": crate::diagnostics::resident_memory_bytes(std::process::id()),
                "oracle_elapsed_us": started.elapsed().as_micros(),
            })
        );
        std::hint::black_box(records);
    }
}
