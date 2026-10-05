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

use crate::config::{count_u64, DeliveryClass};
use crate::records::{GapEvent, RunRecords, SentEvent};
use crate::schedule::{Phase, SenderPlan};

/// One delivery key that violated the contract, named exactly.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeliveryKey {
    pub recipient: String,
    pub sender: String,
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
    /// Generator tasks outlived the quiescence margin (a wedged generator).
    RunnerDeadlineExceeded { detail: String },
    /// The server rejected a frame mid-run (bad class, payload cap, rate
    /// limit) — recorded from the `Error` frame so the deficit is never
    /// unexplained.
    ServerRejected { recipient: String, detail: String },
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
    /// (latest/volatile classes; always 0 for reliable).
    pub gap_covered: u64,
    pub duplicates: u64,
    pub misrouted: u64,
    pub out_of_order: u64,
    /// This recipient's own one-way latency tail over the measured window,
    /// so an aggregate percentile cannot hide a starved room.
    pub latency_us: LatencyStats,
}

/// One-way latency stats over the measured window (microseconds).
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
    /// (latest/volatile classes; always 0 for reliable).
    pub gap_covered: u64,
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
/// every receipt, the receipt time minus the send time of the same
/// `(sender, seq)` delivery. Single source for the summary percentiles (run
/// total and per recipient) and the histogram artifact, so they can never
/// disagree.
fn latency_pairs(records: &RunRecords) -> Vec<(String, u64)> {
    let sent_by_key: BTreeMap<(&str, u64), &SentEvent> = records
        .sent
        .iter()
        .map(|event| ((event.sender.as_str(), event.seq), event))
        .collect();
    let mut pairs = Vec::with_capacity(records.receipts.len());
    for receipt in &records.receipts {
        if let Some(sent) = sent_by_key.get(&(receipt.sender.as_str(), receipt.seq)) {
            if sent.phase == Phase::Measured {
                pairs.push((
                    receipt.recipient.clone(),
                    receipt.received_us.saturating_sub(sent.sent_us),
                ));
            }
        }
    }
    pairs
}

/// One-way latency samples over the measured window (all recipients).
pub fn latency_samples(records: &RunRecords) -> Vec<u64> {
    latency_pairs(records)
        .into_iter()
        .map(|(_recipient, sample)| sample)
        .collect()
}

/// Percentile helper over microsecond samples (histogram-backed).
fn percentiles(samples: &[u64]) -> (u64, u64, u64, u64) {
    if samples.is_empty() {
        return (0, 0, 0, 0);
    }
    // One-way latencies below 60 s fit every capacity cell; sigfig 3 keeps
    // the histogram small while staying well inside run-to-run noise.
    // Saturate (never fail) on a sample beyond the ceiling: the summary
    // carries the same values via max/percentiles, and a >60 s stall lands
    // at the ceiling instead of aborting the verdict.
    let mut histogram =
        Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).expect("fixed latency histogram");
    for sample in samples {
        histogram.saturating_record(*sample);
    }
    (
        histogram.value_at_quantile(0.5),
        histogram.value_at_quantile(0.95),
        histogram.value_at_quantile(0.99),
        histogram.max(),
    )
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
pub fn summarize(
    plans: &[SenderPlan],
    roster: &[(String, u32)],
    records: &RunRecords,
    generator_lag_bound_us: u64,
    delivery_class: DeliveryClass,
) -> OutcomeSummary {
    let mut reasons = records.faults.clone();
    if !records.join_failures.is_empty() {
        reasons.push(InvalidReason::JoinFailed {
            failures: records.join_failures.clone(),
        });
    }
    let hook_exemptions: Vec<String> = reasons
        .iter()
        .filter_map(|reason| match reason {
            InvalidReason::SlowConsumerDisconnect { recipients } => Some(recipients.clone()),
            _ => None,
        })
        .flatten()
        .collect();

    // Offered work per sender.
    let sent_count: BTreeMap<&str, u64> = plans
        .iter()
        .map(|plan| {
            let sent = records
                .sent
                .iter()
                .filter(|event| event.sender == plan.name)
                .count();
            (plan.name.as_str(), count_u64(sent))
        })
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

    // Receipts grouped per (recipient, sender), in arrival order.
    let mut arrivals: BTreeMap<(&str, &str), Vec<(u64, u64)>> = BTreeMap::new();
    for receipt in &records.receipts {
        arrivals
            .entry((receipt.recipient.as_str(), receipt.sender.as_str()))
            .or_default()
            .push((receipt.seq, receipt.received_us));
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

    // Gap reports: global contract validation, then per-stream coverage.
    // Server-stamped sequences are 1-based over one sender's relay stream;
    // the runner's ledger sequences are 0-based over the same stream, so the
    // mapping is `server seq = ledger seq + 1` (single-epoch slice: the
    // runner never reconnects a sender).
    let reasons_permitted =
        |class: DeliveryClass, reason: signal_fish_server::protocol::DeliveryGapReason| match class
        {
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
        };
    let member_names: BTreeSet<&str> = roster.iter().map(|(name, _)| name.as_str()).collect();
    let room_of: BTreeMap<&str, u32> = roster
        .iter()
        .map(|(name, room)| (name.as_str(), *room))
        .collect();
    let mut invalid_gaps = GapViolations::default();
    let mut gaps_by_stream: BTreeMap<(&str, &str), Vec<&GapEvent>> = BTreeMap::new();
    for gap in &records.gaps {
        let mut detail: Option<String> = None;
        if !member_names.contains(gap.recipient.as_str())
            || !member_names.contains(gap.sender.as_str())
        {
            detail = Some("gap references a peer outside the run roster".to_string());
        } else if gap.recipient == gap.sender {
            detail = Some("gap names the recipient as its own sender".to_string());
        } else if room_of.get(gap.recipient.as_str()) != room_of.get(gap.sender.as_str()) {
            detail = Some("gap names a sender from another room".to_string());
        } else if delivery_class == DeliveryClass::Reliable {
            detail =
                Some("reliable delivery permits no loss, yet a gap report arrived".to_string());
        } else if gap.epoch != 1 {
            detail = Some(format!(
                "gap names epoch {} but a no-reconnect run has only epoch 1",
                gap.epoch
            ));
        } else if !reasons_permitted(delivery_class, gap.reason) {
            detail = Some(format!(
                "reason {reason:?} is not a {class} loss reason",
                reason = gap.reason,
                class = match delivery_class {
                    DeliveryClass::Latest => "latest",
                    DeliveryClass::Volatile => "volatile",
                    DeliveryClass::Reliable => "reliable",
                }
            ));
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
            .entry((gap.recipient.as_str(), gap.sender.as_str()))
            .or_default()
            .push(gap);
    }

    let mut duplicates = Category::default();
    let mut misrouted = Category::default();
    let mut out_of_order = Category::default();
    let mut missing = Category::default();

    let mut per_recipient = Vec::new();
    for (recipient, room) in roster {
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

        // Every observed sender must be a co-room member of this recipient.
        // Each delivery from an out-of-roster sender is one misroute, with
        // the first arrival naming the key.
        for ((arrival_recipient, sender), stream) in arrivals.iter() {
            if *arrival_recipient != recipient.as_str() {
                continue;
            }
            if !expected_senders.contains(sender) {
                let (seq, _) = stream.first().copied().unwrap_or((0, 0));
                outcome.misrouted += count_u64(stream.len());
                misrouted.record(DeliveryKey {
                    recipient: recipient.clone(),
                    sender: (*sender).to_string(),
                    seq,
                });
            }
        }

        for sender in expected_senders {
            let total_sent = sent_count.get(sender).copied().unwrap_or(0);
            let arrival = arrivals
                .get(&(recipient.as_str(), sender))
                .cloned()
                .unwrap_or_default();

            // Duplicates: a sequence observed more than once.
            let mut seen = BTreeSet::new();
            let mut unique: Vec<(u64, u64)> = Vec::with_capacity(arrival.len());
            for (seq, received_us) in arrival {
                if !seen.insert(seq) {
                    outcome.duplicates += 1;
                    duplicates.record(DeliveryKey {
                        recipient: recipient.clone(),
                        sender: sender.to_string(),
                        seq,
                    });
                    continue;
                }
                unique.push((seq, received_us));
            }

            // Range: no sequence may exist beyond what the sender sent.
            for (seq, _) in &unique {
                if *seq >= total_sent {
                    outcome.misrouted += 1;
                    misrouted.record(DeliveryKey {
                        recipient: recipient.clone(),
                        sender: sender.to_string(),
                        seq: *seq,
                    });
                }
            }

            // Arrival order: per-sender sequences must strictly increase.
            for pair in unique.windows(2) {
                if pair[1].0 <= pair[0].0 {
                    outcome.out_of_order += 1;
                    out_of_order.record(DeliveryKey {
                        recipient: recipient.clone(),
                        sender: sender.to_string(),
                        seq: pair[1].0,
                    });
                }
            }

            // Omissions: the class selects the contract. Reliable permits
            // none — the gap-free prefix rule. Latest/volatile permit policy
            // loss, but only with exact gap coverage: delivered ∪ gap ranges
            // must partition the sender's whole sent stream, holes without
            // coverage are `MissingDeliveries`, and only the loud
            // disconnect tail may stay uncovered.
            let received_unique = count_u64(unique.len());
            if delivery_class == DeliveryClass::Reliable {
                // Gap-free prefix in arrival order: position i carries seq i.
                // The first mismatch is the first hole; everything from there
                // is a missing delivery (the permitted disconnect loss is
                // only ever the unobserved TAIL, so a hole mid-stream is a
                // violation even for a disconnected recipient).
                let mut prefix_len = unique.len();
                for (position, (seq, _)) in unique.iter().enumerate() {
                    if *seq != count_u64(position) {
                        prefix_len = position;
                        break;
                    }
                }
                let deficit = total_sent.saturating_sub(received_unique);
                if prefix_len < unique.len() && deficit > 0 {
                    // A hole (with unobserved deliveries) is missing work. A
                    // pure reorder with zero deficit is already flagged by the
                    // out-of-order category above — never double-counted as
                    // loss.
                    outcome.missing += deficit;
                    if !exempt {
                        missing.record(DeliveryKey {
                            recipient: recipient.clone(),
                            sender: sender.to_string(),
                            seq: count_u64(prefix_len),
                        });
                    }
                } else if connected_through && deficit > 0 {
                    outcome.missing += deficit;
                    if !exempt {
                        missing.record(DeliveryKey {
                            recipient: recipient.clone(),
                            sender: sender.to_string(),
                            seq: received_unique,
                        });
                    }
                } else if !connected_through {
                    outcome.undelivered_at_disconnect += deficit;
                }
            } else {
                // Coverage model for the lossy classes. Delivered unique
                // sequences are covered; every gap range must add only
                // uncovered ledger sequences (`server seq - 1`).
                let stream_gaps = gaps_by_stream
                    .get(&(recipient.as_str(), sender))
                    .map_or(&[][..], Vec::as_slice);
                let mut covered: BTreeSet<u64> = unique.iter().map(|(seq, _)| *seq).collect();
                for gap in stream_gaps {
                    if gap.to_seq > total_sent {
                        invalid_gaps.record_violation(
                            gap,
                            format!(
                                "gap range reaches beyond what the sender sent ({} > \
                                 {total_sent} relayed)",
                                gap.to_seq
                            ),
                        );
                        continue;
                    }
                    let from = gap.from_seq - 1; // pre-validated: from_seq >= 1
                    let to = gap.to_seq - 1;
                    // Reject BEFORE inserting: a rejected range contributes
                    // no coverage, so its non-overlapping remainder stays an
                    // honest uncovered omission in the totals.
                    if (from..=to).any(|seq| covered.contains(&seq)) {
                        invalid_gaps.record_violation(
                            gap,
                            format!(
                                "gap range {}:{} overlaps an already-covered sequence",
                                gap.from_seq, gap.to_seq
                            ),
                        );
                        continue;
                    }
                    for seq in from..=to {
                        covered.insert(seq);
                    }
                    outcome.gap_covered += to - from + 1;
                }
                // Holes: uncovered sequences at or below the highest covered
                // position — the head below the first covered value and the
                // spans between covered values. A hole means the server
                // relayed (stamped) past it without delivering or reporting
                // it — silent loss. The first hole is named exactly.
                let mut holes: u64 = 0;
                let mut first_hole: Option<u64> = None;
                if let Some(lowest) = covered.first() {
                    if *lowest > 0 {
                        holes += *lowest;
                        first_hole = Some(0);
                    }
                }
                let mut previous: Option<u64> = None;
                for seq in &covered {
                    if let Some(position) = previous {
                        let span = seq - position - 1;
                        if span > 0 {
                            holes += span;
                            first_hole.get_or_insert(position + 1);
                        }
                    }
                    previous = Some(*seq);
                }
                let deficit = total_sent
                    .saturating_sub(count_u64(covered.len()))
                    .saturating_sub(holes);
                if holes > 0 {
                    outcome.missing += holes;
                    if !exempt {
                        missing.record(DeliveryKey {
                            recipient: recipient.clone(),
                            sender: sender.to_string(),
                            seq: first_hole.unwrap_or(0),
                        });
                    }
                }
                if connected_through && deficit > 0 {
                    outcome.missing += deficit;
                    if !exempt {
                        missing.record(DeliveryKey {
                            recipient: recipient.clone(),
                            sender: sender.to_string(),
                            seq: previous.map_or(0, |highest| highest + 1),
                        });
                    }
                } else if !connected_through {
                    outcome.undelivered_at_disconnect += deficit;
                }
            }

            // Latency pairs are collected by [`latency_samples`] below.
            outcome.received.insert(sender.to_string(), received_unique);
        }

        per_recipient.push(outcome);
    }

    // Per-recipient latency tails: one starved room must be visible in the
    // summary, not smoothed away by the run aggregate.
    let mut latency_by_recipient: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for (recipient, sample) in latency_pairs(records) {
        latency_by_recipient
            .entry(recipient)
            .or_default()
            .push(sample);
    }
    for outcome in &mut per_recipient {
        let samples = latency_by_recipient
            .get(&outcome.recipient)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let (p50, p95, p99, max) = percentiles(samples);
        outcome.latency_us = LatencyStats {
            samples: count_u64(samples.len()),
            p50_us: p50,
            p95_us: p95,
            p99_us: p99,
            max_us: max,
        };
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

    let latency = latency_samples(records);
    let (p50, p95, p99, max) = percentiles(&latency);
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
        latency_us: LatencyStats {
            samples: count_u64(latency.len()),
            p50_us: p50,
            p95_us: p95,
            p99_us: p99,
            max_us: max,
        },
        generator_lag_us: LagStats {
            max_us: max_lag,
            p99_us: lag_p99,
            bound_us: generator_lag_bound_us,
        },
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
