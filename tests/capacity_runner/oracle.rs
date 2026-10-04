//! Delivery-contract oracle: pure function from run events to an exact
//! outcome summary.
//!
//! The oracle is the negative-control surface: missing, duplicate, misrouted,
//! and out-of-order deliveries each produce a distinct, explicit invalidation
//! reason with the first offending key, and a run is valid only when every
//! connected recipient observed the complete, gap-free, in-order stream from
//! every co-room sender, no offered work was left unsent (without a declared
//! fault explaining it) or outstanding, and no scheduled send lagged past
//! the generator bound. A disconnected recipient must hold a gap-free
//! in-order prefix — loss may only be the loud tail cut off with the
//! connection, never a silent hole.
//!
//! Because this module is pure, the runner's `summary.json` and an artifact
//! replay (`artifacts::replay`) run the same code and must agree exactly.

use std::collections::{BTreeMap, BTreeSet};

use hdrhistogram::Histogram;

use crate::config::count_u64;
use crate::records::{RunRecords, SentEvent};
use crate::schedule::{Phase, SenderPlan};

/// One delivery key that violated the contract, named exactly.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeliveryKey {
    pub recipient: String,
    pub sender: String,
    pub seq: u64,
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
    pub duplicates: u64,
    pub misrouted: u64,
    pub out_of_order: u64,
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

/// One-way latency samples over the measured window: for every receipt, the
/// receipt time minus the send time of the same `(sender, seq)` delivery.
/// Single source for the summary percentiles and the histogram artifact, so
/// they can never disagree.
pub fn latency_samples(records: &RunRecords) -> Vec<u64> {
    let sent_by_key: BTreeMap<(&str, u64), &SentEvent> = records
        .sent
        .iter()
        .map(|event| ((event.sender.as_str(), event.seq), event))
        .collect();
    let mut samples = Vec::with_capacity(records.receipts.len());
    for receipt in &records.receipts {
        if let Some(sent) = sent_by_key.get(&(receipt.sender.as_str(), receipt.seq)) {
            if sent.phase == Phase::Measured {
                samples.push(receipt.received_us.saturating_sub(sent.sent_us));
            }
        }
    }
    samples
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
pub fn summarize(
    plans: &[SenderPlan],
    roster: &[(String, u32)],
    records: &RunRecords,
    generator_lag_bound_us: u64,
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
            duplicates: 0,
            misrouted: 0,
            out_of_order: 0,
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

            // Gap-free prefix in arrival order: position i carries seq i.
            // The first mismatch is the first hole; everything from there is
            // a missing delivery (the permitted disconnect loss is only ever
            // the unobserved TAIL, so a hole mid-stream is a violation even
            // for a disconnected recipient).
            let mut prefix_len = unique.len();
            for (position, (seq, _)) in unique.iter().enumerate() {
                if *seq != count_u64(position) {
                    prefix_len = position;
                    break;
                }
            }
            let received_unique = count_u64(unique.len());
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

            // Latency pairs are collected by [`latency_samples`] below.
            outcome.received.insert(sender.to_string(), received_unique);
        }

        per_recipient.push(outcome);
    }

    if let Some(reason) =
        missing.into_reason(|count, first| InvalidReason::MissingDeliveries { count, first })
    {
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
