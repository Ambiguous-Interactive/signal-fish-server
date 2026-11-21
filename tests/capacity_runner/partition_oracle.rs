//! Room-wise evaluation for steady reliable captures.
//!
//! A room-local fault may require a different global first-error key or a
//! lookup in another shard. Return the capture to the exact legacy oracle
//! under its admission limits when the room summaries cannot be combined.

mod raw_faults;

use std::collections::{BTreeMap, BTreeSet};

use hdrhistogram::Histogram;
use raw_faults::RawFaults;

use crate::config::{count_u64, micros, ChurnSchedule, DeliveryClass, Encoding, RunConfig};
use crate::oracle::{
    self, ByteStats, DeliveryKey, DirectionBytes, GapViolation, InvalidReason, LagStats,
    LatencyStats, OutcomeSummary, Totals,
};
use crate::records::{ReceiptEvent, ReceiptNames, RunRecords};
use crate::room_spool::{RoomEvent, RoomReplay};
use crate::schedule::{ChurnPlan, SenderPlan};

const MAX_LAG_BUCKET_US: u64 = 1_000_000;

pub(super) struct Evaluation {
    pub summary: OutcomeSummary,
    pub histogram: Histogram<u64>,
}

pub(super) fn validate_profile(config: &RunConfig) -> Result<(), String> {
    if config.encoding != Encoding::V3Json
        || config.delivery_class != DeliveryClass::Reliable
        || config.churn != ChurnSchedule::None
        || config.experiment.is_some()
        || config.receipt_timings
    {
        return Err("room capture requires steady reliable v3 JSON traffic".to_string());
    }
    Ok(())
}

pub(super) fn evaluate_rooms(
    replay: &RoomReplay,
    config: &RunConfig,
    plans: &[SenderPlan],
    control: &RunRecords,
) -> Result<Option<Evaluation>, String> {
    if !control.sent.is_empty() || !control.receipts.is_empty() || !control.gaps.is_empty() {
        return Err("room capture control artifact contains room records".into());
    }
    if !control.churn.is_empty() {
        return Ok(None);
    }
    let (_, room_rows) = crate::room_spool::row_limits(config)?;
    if !replay.fits_room_row_limit(room_rows) {
        // More than one room's expected receipts landed in a shard. Give
        // exact fallback its separate whole-run memory admission check.
        return Ok(None);
    }
    let exempt: BTreeSet<&str> = control
        .faults
        .iter()
        .filter_map(|fault| match fault {
            InvalidReason::SlowConsumerDisconnect { recipients } => Some(recipients.iter()),
            _ => None,
        })
        .flatten()
        .map(String::as_str)
        .collect();
    let bound_us = micros(config.generator_lag_bound);
    let mut summary = OutcomeSummary {
        valid: true,
        reasons: control.faults.clone(),
        totals: Totals::default(),
        per_recipient: Vec::new(),
        latency_us: LatencyStats::default(),
        generator_lag_us: LagStats {
            max_us: 0,
            p99_us: 0,
            bound_us,
        },
        payload_bytes: Default::default(),
        experiment: None,
        unsupported_notices: count_u64(control.unsupported_notices.len()),
    };
    let mut histogram = oracle::latency_histogram([]);
    let mut lags = BTreeMap::<u64, u64>::new();
    let mut faults = BTreeMap::new();
    let mut raw_faults = RawFaults::default();
    let room_of: BTreeMap<_, _> = plans
        .iter()
        .map(|plan| (plan.name.as_str(), plan.room))
        .collect();
    let mut disconnects = BTreeMap::<u32, Vec<_>>::new();
    for event in &control.disconnects {
        if let Some(&room) = room_of.get(event.recipient.as_str()) {
            disconnects.entry(room).or_default().push(event.clone());
        }
    }
    let mut gap_count = 0;
    let mut first_gap: Option<(u64, GapViolation)> = None;
    for room in 0..config.rooms {
        let room_plans = plans
            .iter()
            .filter(|plan| plan.room == room)
            .cloned()
            .collect::<Vec<_>>();
        let roster = room_plans
            .iter()
            .map(|plan| (plan.name.clone(), plan.room))
            .collect::<Vec<_>>();
        let mut records = RunRecords {
            disconnects: disconnects.remove(&room).unwrap_or_default(),
            ..RunRecords::default()
        };
        let room_exempt: Vec<_> = roster
            .iter()
            .filter(|(name, _)| exempt.contains(name.as_str()))
            .map(|(name, _)| name.clone())
            .collect();
        if !room_exempt.is_empty() {
            records.faults.push(InvalidReason::SlowConsumerDisconnect {
                recipients: room_exempt,
            });
        }
        let mut names = ReceiptNames::default();
        replay.visit(room as usize, room_rows, |ordinal, event| match event {
            RoomEvent::Sent(event) => records.sent.push(names.share_sent(event)),
            RoomEvent::Receipt(event) => records.receipts.push(names.share(event)),
            RoomEvent::Gap(event) => {
                gap_count += 1;
                if first_gap.as_ref().is_none_or(|(first, _)| ordinal < *first) {
                    first_gap = Some((ordinal, oracle::reliable_gap_violation(event, &room_of)));
                }
            }
        })?;
        crate::records::sort_sent_events(&mut records.sent);
        let summarize_room = |foreign| {
            oracle::summarize_with_foreign(
                &room_plans,
                &roster,
                &records,
                bound_us,
                config.payload_bytes,
                config.delivery_class,
                &ChurnPlan::default(),
                None,
                foreign,
            )
        };
        let (mut result, mut raw_positions) = summarize_room(None);
        let mut foreign = None;
        if !result.valid {
            if records
                .sent
                .iter()
                .any(|sent| room_of.get(sent.sender.as_str()).copied().unwrap_or(0) != room)
            {
                return Ok(None);
            }
            let mut requests: BTreeMap<u32, Vec<(usize, &ReceiptEvent)>> = BTreeMap::new();
            for (ordinal, event) in records.receipts.iter().enumerate() {
                // Capture routes unknown recipients to shard zero. Their
                // first misroute precedes all roster-recipient violations
                // in the legacy oracle.
                if room_of.get(event.recipient.as_str()).copied().unwrap_or(0) != room {
                    return Ok(None);
                }
                let source = room_of.get(event.sender.as_str()).copied().unwrap_or(0);
                if source != room {
                    requests.entry(source).or_default().push((ordinal, event));
                }
            }
            if !requests.is_empty() {
                foreign = Some(read_foreign_timings(replay, room_rows, requests)?);
                (result, raw_positions) = summarize_room(foreign.as_ref());
            }
            result.reasons.drain(..records.faults.len());
            raw_faults.take_room_reasons(
                &mut result.reasons,
                raw_positions,
                replay,
                room as usize,
                room_rows,
            )?;
            if !merge_room_faults(&mut faults, result.reasons) {
                return Ok(None);
            }
        }
        summary.generator_lag_us.max_us = summary
            .generator_lag_us
            .max_us
            .max(result.generator_lag_us.max_us);
        let incoming = result.payload_bytes;
        if !merge_direction(
            &mut summary.payload_bytes.warmup.ingress,
            incoming.warmup.ingress,
        ) || !merge_direction(
            &mut summary.payload_bytes.warmup.egress,
            incoming.warmup.egress,
        ) || !merge_direction(
            &mut summary.payload_bytes.measured.ingress,
            incoming.measured.ingress,
        ) || !merge_direction(
            &mut summary.payload_bytes.measured.egress,
            incoming.measured.egress,
        ) || !merge_direction(
            &mut summary.payload_bytes.unmatched_egress,
            incoming.unmatched_egress,
        ) {
            return Ok(None);
        }
        summary.totals.scheduled += result.totals.scheduled;
        summary.totals.sent += result.totals.sent;
        summary.totals.unsent += result.totals.unsent;
        summary.totals.receipts += result.totals.receipts;
        summary.totals.outstanding += result.totals.outstanding;
        summary.latency_us.max_us = summary.latency_us.max_us.max(result.latency_us.max_us);
        summary.per_recipient.extend(result.per_recipient);
        let samples = oracle::latency_pairs_with_foreign(&records, foreign.as_ref())
            .map(|(_, latency)| latency);
        histogram
            .add(oracle::latency_histogram(samples))
            .map_err(|error| format!("merge latency histogram: {error}"))?;
        if summary.generator_lag_us.max_us <= MAX_LAG_BUCKET_US {
            for event in &records.sent {
                let lag = event.sent_us.saturating_sub(event.intended_us);
                *lags.entry(lag).or_default() += 1;
            }
        }
    }
    summary.latency_us.samples = histogram.len();
    summary.latency_us.p50_us = histogram.value_at_quantile(0.50);
    summary.latency_us.p95_us = histogram.value_at_quantile(0.95);
    summary.latency_us.p99_us = histogram.value_at_quantile(0.99);
    if summary.generator_lag_us.max_us > MAX_LAG_BUCKET_US {
        summary.generator_lag_us.p99_us = oracle::select_generator_lag(
            summary.totals.sent,
            summary.generator_lag_us.max_us,
            |scan| {
                for room in 0..config.rooms as usize {
                    replay.visit(room, room_rows, |_, event| {
                        if let RoomEvent::Sent(event) = event {
                            scan(event.sent_us.saturating_sub(event.intended_us));
                        }
                    })?;
                }
                Ok::<(), String>(())
            },
        )?;
    } else {
        let rank = summary.totals.sent - summary.totals.sent / 100;
        let mut count = 0;
        for (lag, population) in lags {
            count += population;
            if count >= rank {
                summary.generator_lag_us.p99_us = lag;
                break;
            }
        }
    }
    let (control_payload, control_reasons) =
        oracle::control_payload_evidence(control, config.payload_bytes);
    if !raw_faults.take_control_reasons(control_reasons)
        || !merge_direction(
            &mut summary.payload_bytes.unmatched_egress,
            control_payload.unmatched_egress,
        )
    {
        return Ok(None);
    }
    raw_faults.append_payload(&mut summary.reasons);
    if !control.join_failures.is_empty() {
        summary.reasons.push(InvalidReason::JoinFailed {
            failures: control.join_failures.clone(),
        });
    }
    raw_faults.append_schedule(&mut summary.reasons);
    if summary.generator_lag_us.max_us > bound_us {
        summary.reasons.push(InvalidReason::GeneratorSaturated {
            max_lag_us: summary.generator_lag_us.max_us,
            bound_us,
        });
    }
    if let Some((count, first)) = faults.remove(&DeliveryFault::Missing) {
        summary
            .reasons
            .push(InvalidReason::MissingDeliveries { count, first });
    }
    if let Some((_, first)) = first_gap {
        summary.reasons.push(InvalidReason::InvalidGapReports {
            count: gap_count,
            first,
        });
    }
    summary
        .reasons
        .extend(faults.into_iter().map(|(kind, (count, first))| match kind {
            DeliveryFault::Missing => InvalidReason::MissingDeliveries { count, first },
            DeliveryFault::Duplicate => InvalidReason::DuplicateDeliveries { count, first },
            DeliveryFault::Misrouted => InvalidReason::MisroutedDeliveries { count, first },
            DeliveryFault::Order => InvalidReason::OutOfOrderDeliveries { count, first },
        }));
    if summary.totals.unsent > 0 && !summary.reasons.iter().any(oracle::explains_unsent) {
        summary.reasons.push(InvalidReason::UnsentWork {
            count: summary.totals.unsent,
        });
    }
    let outstanding_for_verdict = if exempt.is_empty() {
        summary.totals.outstanding
    } else {
        summary
            .per_recipient
            .iter()
            .filter(|outcome| !exempt.contains(outcome.recipient.as_str()))
            .map(|outcome| outcome.missing)
            .sum()
    };
    if outstanding_for_verdict > 0 {
        summary.reasons.push(InvalidReason::OutstandingAtEnd {
            count: outstanding_for_verdict,
        });
    }
    if !control.disconnects.is_empty()
        && !control
            .faults
            .iter()
            .any(|fault| matches!(fault, InvalidReason::ServerTerminated))
    {
        summary.reasons.push(InvalidReason::UnexpectedDisconnect {
            recipients: control
                .disconnects
                .iter()
                .map(|event| event.recipient.clone())
                .collect(),
        });
    }
    summary.valid = summary.reasons.is_empty();
    Ok(Some(Evaluation { summary, histogram }))
}

/// Read the send ledger of one requested foreign room at a time. Retain
/// only the timing matches needed by this room's receipts.
fn read_foreign_timings(
    replay: &RoomReplay,
    row_budget: u64,
    requests: BTreeMap<u32, Vec<(usize, &ReceiptEvent)>>,
) -> Result<oracle::ForeignReceiptTimings, String> {
    let mut timings = BTreeMap::new();
    for (source, receipts) in requests {
        let mut sends = Vec::new();
        let mut names = ReceiptNames::default();
        replay.visit(source as usize, row_budget, |_, event| {
            if let RoomEvent::Sent(event) = event {
                sends.push(names.share_sent(event));
            }
        })?;
        crate::records::sort_sent_events(&mut sends);
        oracle::add_foreign_timings(&mut timings, &sends, receipts);
    }
    Ok(timings)
}

/// Match the legacy ordering of emitted reasons. Each room contributes its
/// first key in roster order, so the first contributing room wins.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum DeliveryFault {
    Missing,
    Duplicate,
    Misrouted,
    Order,
}

fn merge_room_faults(
    faults: &mut BTreeMap<DeliveryFault, (u64, DeliveryKey)>,
    reasons: Vec<InvalidReason>,
) -> bool {
    for reason in reasons {
        let (kind, count, first) = match reason {
            InvalidReason::MissingDeliveries { count, first } => {
                (DeliveryFault::Missing, count, first)
            }
            InvalidReason::DuplicateDeliveries { count, first } => {
                (DeliveryFault::Duplicate, count, first)
            }
            InvalidReason::MisroutedDeliveries { count, first } => {
                (DeliveryFault::Misrouted, count, first)
            }
            InvalidReason::OutOfOrderDeliveries { count, first } => {
                (DeliveryFault::Order, count, first)
            }
            InvalidReason::GeneratorSaturated { .. }
            | InvalidReason::UnsentWork { .. }
            | InvalidReason::OutstandingAtEnd { .. }
            | InvalidReason::UnexpectedDisconnect { .. } => continue,
            _ => return false,
        };
        faults
            .entry(kind)
            .and_modify(|current| current.0 += count)
            .or_insert((count, first));
    }
    true
}

fn merge_direction(total: &mut DirectionBytes, incoming: DirectionBytes) -> bool {
    merge_bytes(&mut total.application, incoming.application)
        && merge_bytes(&mut total.encoded_frame_body, incoming.encoded_frame_body)
}

fn merge_bytes(total: &mut ByteStats, incoming: ByteStats) -> bool {
    if incoming.samples == 0 {
        return true;
    }
    let Some(bytes) = total.total_bytes.checked_add(incoming.total_bytes) else {
        return false;
    };
    total.min_bytes = if total.samples == 0 {
        incoming.min_bytes
    } else {
        total.min_bytes.min(incoming.min_bytes)
    };
    total.samples += incoming.samples;
    total.total_bytes = bytes;
    total.max_bytes = total.max_bytes.max(incoming.max_bytes);
    true
}

#[cfg(test)]
mod tests;
