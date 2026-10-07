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

/// One-way latency samples over the measured window, per recipient: for
/// every receipt, the receipt time minus the intended send time of the same
/// `(sender, seq)` delivery. Single source for the summary percentiles (run
/// total and per recipient) and the histogram artifact, so they can never
/// disagree.
pub fn latency_pairs(records: &RunRecords) -> impl Iterator<Item = (&str, u64)> {
    let sent_by_key: BTreeMap<(&str, u64), &SentEvent> = records
        .sent
        .iter()
        .map(|event| ((event.sender.as_str(), event.seq), event))
        .collect();
    records.receipts.iter().filter_map(move |receipt| {
        let sent = sent_by_key.get(&(receipt.sender.as_str(), receipt.seq))?;
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
    sent_times: &BTreeMap<(&str, u32), Vec<u64>>,
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
                                        let completed = sent_times
                                            .get(&(member.as_str(), old))
                                            .map(|times| {
                                                times
                                                    .iter()
                                                    .filter(|sent_us| **sent_us <= *at_us)
                                                    .count()
                                            })
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
    let phases: BTreeMap<(&str, u64), Phase> = records
        .sent
        .iter()
        .map(|sent| ((sent.sender.as_str(), sent.seq), sent.phase))
        .collect();
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
        let direction = match phases.get(&(receipt.sender.as_str(), receipt.seq)) {
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
    let total_unsent = total_scheduled.saturating_sub(total_sent);

    // Generator lag over completed sends.
    let mut lags: Vec<u64> = records
        .sent
        .iter()
        .map(|event| event.sent_us.saturating_sub(event.intended_us))
        .collect();
    lags.sort_unstable();
    let max_lag = lags.last().copied().unwrap_or(0);
    let lag_p99 = percentile_of_sorted(&lags, 99, 100);
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
    let mut blocks: BTreeMap<(&str, u32), Vec<u64>> = BTreeMap::new();
    for sent in &records.sent {
        blocks
            .entry((sent.sender.as_str(), sent.epoch))
            .or_default()
            .push(sent.seq);
    }
    for block in blocks.values_mut() {
        block.sort_unstable();
    }
    // Per-stream send completion times (ledger-seq order = send order), the
    // basis for deriving an absent member's owed floor: the server cannot
    // deliver a freshly seated recipient any frame fanned out before its
    // seat, so a member missing from the rejoin snapshot owes exactly the
    // sends that completed after it.
    let sent_times: BTreeMap<(&str, u32), Vec<u64>> =
        records.sent.iter().fold(BTreeMap::new(), |mut acc, sent| {
            acc.entry((sent.sender.as_str(), sent.epoch))
                .or_default()
                .push(sent.sent_us);
            acc
        });
    let sender_epochs: BTreeMap<&str, BTreeSet<u32>> =
        blocks
            .keys()
            .fold(BTreeMap::new(), |mut acc, (sender, epoch)| {
                acc.entry(*sender).or_default().insert(*epoch);
                acc
            });

    // Receipts grouped per `(recipient, sender, epoch)` stream, in arrival
    // order: `(server_seq, ledger_seq, received_us)`.
    let mut arrivals: BTreeMap<(&str, &str, u32), Vec<(u64, u64, u64)>> = BTreeMap::new();
    for receipt in &records.receipts {
        arrivals
            .entry((
                receipt.recipient.as_str(),
                receipt.sender.as_str(),
                receipt.epoch,
            ))
            .or_default()
            .push((receipt.server_seq, receipt.seq, receipt.received_us));
    }
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
    let (views, unresolved) = churn_views(records, roster, &sent_times);
    for player_id in unresolved {
        reasons.push(InvalidReason::UnresolvedSenderIdentity { player_id });
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
    let member_names: BTreeSet<&str> = roster.iter().map(|(name, _)| name.as_str()).collect();
    let room_of: BTreeMap<&str, u32> = roster
        .iter()
        .map(|(name, room)| (name.as_str(), *room))
        .collect();
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
    let mut misrouted = Category::default();
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
        for ((arrival_recipient, sender, epoch), stream) in arrivals.iter() {
            if *arrival_recipient != recipient.as_str() {
                continue;
            }
            if opaque_senders.contains(sender) {
                // The unsupported-conversion contract: the opaque sender's
                // payload reaches NO recipient as a delivery. An arrival is
                // the leak class — a conversion (lossy or lucky) happened
                // that the server must refuse. Counted only in the leak
                // category: its verdict names the recipient and stream
                // exactly, and a leak already invalidates the run.
                let (seq, _, _) = stream.first().copied().unwrap_or((0, 0, 0));
                leaked.record(DeliveryKey {
                    recipient: recipient.clone(),
                    sender: (*sender).to_string(),
                    epoch: *epoch,
                    seq,
                });
                continue;
            }
            if !expected_senders.contains(sender) {
                let (seq, _, _) = stream.first().copied().unwrap_or((0, 0, 0));
                outcome.misrouted += count_u64(stream.len());
                misrouted.record(DeliveryKey {
                    recipient: recipient.clone(),
                    sender: (*sender).to_string(),
                    epoch: *epoch,
                    seq,
                });
                continue;
            }
            // Delivered evidence counts every unique arrival on every
            // stream — closed streams included: their arrivals were real
            // deliveries, even though nothing further is owed.
            let unique = stream
                .iter()
                .map(|(server_seq, _, _)| *server_seq)
                .collect::<BTreeSet<_>>()
                .len();
            *received_by_sender.entry(sender).or_insert(0) += count_u64(unique);
            let block_len = count_u64(blocks.get(&(*sender, *epoch)).map_or(0, Vec::len));
            let closed_after = view.closed_at.get(&((*sender).to_string(), *epoch));
            let floor = view.floors.get(&((*sender).to_string(), *epoch));
            for &(server_seq, _ledger_seq, received_us) in stream {
                let key = DeliveryKey {
                    recipient: recipient.clone(),
                    sender: (*sender).to_string(),
                    epoch: *epoch,
                    seq: server_seq,
                };
                if server_seq == 0 || server_seq > block_len {
                    outcome.misrouted += 1;
                    misrouted.record(key);
                } else if closed_after.is_some_and(|at_us| received_us > *at_us) {
                    // A delivery for a stream its sender had already
                    // rejoined past: the stale epoch must be silent.
                    outcome.misrouted += 1;
                    misrouted.record(key);
                } else if floor.is_some_and(|(owed_floor, from_us, enforced)| {
                    *enforced && received_us >= *from_us && server_seq <= *owed_floor
                }) {
                    // The rejoin snapshot accounted this sequence, and the
                    // server gates its own queue against that watermark:
                    // the server must never send it to the new seat.
                    outcome.misrouted += 1;
                    misrouted.record(key);
                }
            }
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
                    .get(&(recipient.as_str(), sender, *epoch))
                    .cloned()
                    .unwrap_or_default();

                // Duplicates: a stream sequence observed more than once.
                let mut seen = BTreeSet::new();
                let mut unique: Vec<(u64, u64, u64)> = Vec::with_capacity(arrival.len());
                for (server_seq, ledger_seq, received_us) in arrival {
                    if !seen.insert(server_seq) {
                        outcome.duplicates += 1;
                        duplicates.record(DeliveryKey {
                            recipient: recipient.clone(),
                            sender: sender.to_string(),
                            epoch: *epoch,
                            seq: server_seq,
                        });
                        continue;
                    }
                    unique.push((server_seq, ledger_seq, received_us));
                }

                // Arrival order: per-stream sequences must strictly increase.
                for pair in unique.windows(2) {
                    if pair[1].0 <= pair[0].0 {
                        outcome.out_of_order += 1;
                        out_of_order.record(DeliveryKey {
                            recipient: recipient.clone(),
                            sender: sender.to_string(),
                            epoch: *epoch,
                            seq: pair[1].0,
                        });
                    }
                }

                // The stream's rejoin floor: from this instant on, every
                // sequence at or below the snapshot tail is not owed (the
                // loud away window), and deliveries must resume exactly one
                // past it.
                let (owed_floor, _floor_from_us, _enforced) = view
                    .floors
                    .get(&((*sender).to_string(), *epoch))
                    .copied()
                    .unwrap_or((0, 0, false));
                let received_unique = count_u64(unique.len());

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
                let mut covered: BTreeSet<u64> = unique
                    .iter()
                    .map(|(server_seq, _, _)| *server_seq)
                    .collect();
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
                    if (gap.from_seq..=gap.to_seq).any(|seq| covered.contains(&seq)) {
                        invalid_gaps.record_violation(
                            gap,
                            format!(
                                "gap range {}:{} overlaps an already-covered sequence",
                                gap.from_seq, gap.to_seq
                            ),
                        );
                        continue;
                    }
                    for seq in gap.from_seq..=gap.to_seq {
                        covered.insert(seq);
                    }
                    outcome.gap_covered += gap.to_seq - gap.from_seq + 1;
                }
                // Work in the owed domain: shift everything at or below the
                // rejoin floor out (not owed), so the hole scan and the
                // deficit math see exactly the seat's owed window.
                let owed: BTreeSet<u64> = covered
                    .iter()
                    .filter(|seq| **seq > owed_floor)
                    .map(|seq| seq - owed_floor)
                    .collect();
                let owed_total = total_owed.saturating_sub(owed_floor);
                // Holes: uncovered owed sequences at or below the highest
                // covered position — the head below the first covered value
                // and the spans between covered values. A hole means the
                // server relayed (stamped) past it without delivering or
                // reporting it — silent loss. The first hole is named
                // exactly (in stream coordinates).
                let mut holes: u64 = 0;
                let mut first_hole: Option<u64> = None;
                if let Some(lowest) = owed.first() {
                    if *lowest > 1 {
                        holes += *lowest - 1;
                        first_hole = Some(owed_floor + 1);
                    }
                }
                let mut previous: Option<u64> = None;
                for seq in &owed {
                    if let Some(position) = previous {
                        let span = seq - position - 1;
                        if span > 0 {
                            holes += span;
                            first_hole.get_or_insert(owed_floor + position + 1);
                        }
                    }
                    previous = Some(*seq);
                }
                let deficit = owed_total
                    .saturating_sub(count_u64(owed.len()))
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
                            seq: owed_floor + previous.map_or(0, |highest| highest) + 1,
                        });
                    }
                } else if !connected_through {
                    outcome.undelivered_at_disconnect += deficit;
                }
                let _ = received_unique;
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

/// Nearest-rank percentile of an ascending-sorted sample list, computed in
/// exact integer arithmetic: `rank = ceil(count * numerator / denominator)`.
fn percentile_of_sorted(sorted: &[u64], numerator: u64, denominator: u64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let count = u64::try_from(sorted.len()).unwrap_or(u64::MAX);
    let rank = (count * numerator).div_ceil(denominator);
    let index = usize::try_from(rank).unwrap_or(1).clamp(1, sorted.len());
    sorted[index - 1]
}
