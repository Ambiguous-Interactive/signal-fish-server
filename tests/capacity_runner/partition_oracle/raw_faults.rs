//! Reduce raw violations without changing their whole-run precedence.

use crate::oracle::{
    InvalidReason, PayloadSizeViolation, RawViolationPositions, ReceiptTimeViolation,
    ScheduledSendViolation,
};
use crate::room_spool::{RoomEvent, RoomReplay};

#[derive(Default)]
pub(super) struct RawFaults {
    early: First<u64, ReceiptTimeViolation>,
    size: First<PayloadOrder, PayloadSizeViolation>,
    body: First<PayloadOrder, PayloadSizeViolation>,
    schedule: First<(String, u64), ScheduledSendViolation>,
}

/// The legacy scan reads sorted sends before receipts in capture order.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum PayloadOrder {
    Send(String, u64),
    Receipt(u64),
    Control,
}

struct First<K, V> {
    count: u64,
    evidence: Option<(K, V)>,
}

impl<K, V> Default for First<K, V> {
    fn default() -> Self {
        Self {
            count: 0,
            evidence: None,
        }
    }
}

impl<K: Ord, V> First<K, V> {
    fn record(&mut self, count: u64, order: K, first: V) {
        self.count += count;
        if self
            .evidence
            .as_ref()
            .is_none_or(|(prior, _)| order < *prior)
        {
            self.evidence = Some((order, first));
        }
    }

    fn take(&mut self, build: impl FnOnce(u64, V) -> InvalidReason) -> Option<InvalidReason> {
        self.evidence
            .take()
            .map(|(_, first)| build(self.count, first))
    }
}

impl RawFaults {
    pub(super) fn take_room_reasons(
        &mut self,
        reasons: &mut Vec<InvalidReason>,
        positions: RawViolationPositions,
        replay: &RoomReplay,
        room: usize,
        rows: u64,
    ) -> Result<(), String> {
        let requested = [positions.early, positions.size, positions.body];
        let mut capture_order = [None; 3];
        if requested.iter().any(Option::is_some) {
            let mut position = 0;
            replay.visit(room, rows, |ordinal, event| {
                if matches!(event, RoomEvent::Receipt(_)) {
                    for (requested, found) in requested.iter().zip(&mut capture_order) {
                        if *requested == Some(position) {
                            *found = Some(ordinal);
                        }
                    }
                    position += 1;
                }
            })?;
        }
        let other = std::mem::take(reasons);
        for reason in other {
            match reason {
                InvalidReason::InvalidReceiptTimes { count, first } => {
                    let ordinal = capture_order[0].ok_or_else(|| {
                        "missing capture ordinal for invalid receipt time".to_string()
                    })?;
                    self.early.record(count, ordinal, first);
                }
                InvalidReason::PayloadSizeMismatch { count, first } => {
                    self.size
                        .record(count, payload_order(&first, capture_order[1])?, first);
                }
                InvalidReason::InvalidEncodedFrameBodySize { count, first } => {
                    self.body
                        .record(count, payload_order(&first, capture_order[2])?, first);
                }
                InvalidReason::InvalidScheduledSends { count, first } => {
                    self.schedule
                        .record(count, (first.sender.clone(), first.seq), first);
                }
                other => reasons.push(other),
            }
        }
        Ok(())
    }

    pub(super) fn take_control_reasons(&mut self, reasons: Vec<InvalidReason>) -> bool {
        for reason in reasons {
            match reason {
                InvalidReason::PayloadSizeMismatch { count, first } => {
                    self.size.record(count, PayloadOrder::Control, first);
                }
                InvalidReason::InvalidEncodedFrameBodySize { count, first } => {
                    self.body.record(count, PayloadOrder::Control, first);
                }
                _ => return false,
            }
        }
        true
    }

    pub(super) fn append_payload(&mut self, reasons: &mut Vec<InvalidReason>) {
        reasons.extend(
            self.early
                .take(|count, first| InvalidReason::InvalidReceiptTimes { count, first }),
        );
        reasons.extend(
            self.size
                .take(|count, first| InvalidReason::PayloadSizeMismatch { count, first }),
        );
        reasons.extend(
            self.body
                .take(|count, first| InvalidReason::InvalidEncodedFrameBodySize { count, first }),
        );
    }

    pub(super) fn append_schedule(&mut self, reasons: &mut Vec<InvalidReason>) {
        reasons.extend(
            self.schedule
                .take(|count, first| InvalidReason::InvalidScheduledSends { count, first }),
        );
    }
}

fn payload_order(
    first: &PayloadSizeViolation,
    ordinal: Option<u64>,
) -> Result<PayloadOrder, String> {
    if let Some(ordinal) = ordinal {
        return Ok(PayloadOrder::Receipt(ordinal));
    }
    if first.direction == "ingress" {
        if let (Some(sender), Some(seq)) = (&first.sender, first.seq) {
            return Ok(PayloadOrder::Send(sender.clone(), seq));
        }
    }
    Err("missing capture order for payload violation".into())
}
