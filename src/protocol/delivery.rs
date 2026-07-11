//! Protocol-v3 delivery classes and delivery-accountability payloads.
//!
//! Game-data metadata is projected away for older recipients, and
//! `DeliveryReport` emission is v3-gated, preserving the frozen v2 wire.

use serde::{Deserialize, Serialize};

use super::PlayerId;

/// Maximum exact ranges carried by one `DeliveryReport` frame. Additional
/// ranges roll into another bounded control-lane frame.
pub const DELIVERY_REPORT_MAX_GAPS: usize = 256;

/// Delivery policy requested for one relayed game-data message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryClass {
    /// Preserve every message or disconnect the recipient loudly.
    #[default]
    Reliable,
    /// Retain only the newest undelivered value for a sender-defined key.
    Latest,
    /// Deliver opportunistically without applying sender backpressure.
    Volatile,
}

/// Why an exact server-stamped sequence range was not delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryGapReason {
    /// A newer value for the same `latest` key replaced this range.
    LatestSuperseded,
    /// A new `latest` key could not fit after eligible volatile eviction.
    LatestDroppedFull,
    /// Volatile queue policy dropped this range.
    VolatileDropped,
    /// The recipient could not represent the sender's negotiated encoding.
    UnsupportedFormat,
}

/// Exact inclusive server-stamped sequence range omitted for one recipient.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryGap {
    pub from_player: PlayerId,
    pub epoch: u32,
    /// First omitted sequence number, inclusive.
    pub from_seq: u64,
    /// Last omitted sequence number, inclusive.
    pub to_seq: u64,
    pub reason: DeliveryGapReason,
}

/// Cumulative reliable-delivery outcomes for one connection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReliableDeliveryCounters {
    pub delivered: u64,
    /// Messages abandoned when the connection closed loudly.
    pub abandoned: u64,
    pub unsupported_format: u64,
}

/// Cumulative keyed-latest delivery outcomes for one connection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatestDeliveryCounters {
    pub delivered: u64,
    pub superseded: u64,
    pub dropped_full: u64,
    /// Messages abandoned when the connection closed loudly.
    pub abandoned: u64,
    pub unsupported_format: u64,
}

/// Cumulative volatile-delivery outcomes for one connection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolatileDeliveryCounters {
    pub delivered: u64,
    pub dropped: u64,
    /// Messages abandoned when the connection closed loudly.
    pub abandoned: u64,
    pub unsupported_format: u64,
}

/// Cumulative delivery outcomes partitioned by delivery class.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryCountersByClass {
    pub reliable: ReliableDeliveryCounters,
    pub latest: LatestDeliveryCounters,
    pub volatile: VolatileDeliveryCounters,
}

/// Cumulative per-connection delivery report plus exact new gap records.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryReportPayload {
    pub per_class: DeliveryCountersByClass,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<DeliveryGap>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn delivery_class_and_gap_reason_tokens_are_stable() {
        let class_cases = [
            (DeliveryClass::Reliable, "reliable"),
            (DeliveryClass::Latest, "latest"),
            (DeliveryClass::Volatile, "volatile"),
        ];
        for (value, token) in class_cases {
            let encoded = serde_json::to_value(value).expect("serialize delivery class");
            assert_eq!(encoded, json!(token));
            assert_eq!(
                serde_json::from_value::<DeliveryClass>(encoded)
                    .expect("deserialize delivery class"),
                value
            );
        }

        let reason_cases = [
            (DeliveryGapReason::LatestSuperseded, "latest_superseded"),
            (DeliveryGapReason::LatestDroppedFull, "latest_dropped_full"),
            (DeliveryGapReason::VolatileDropped, "volatile_dropped"),
            (DeliveryGapReason::UnsupportedFormat, "unsupported_format"),
        ];
        for (value, token) in reason_cases {
            let encoded = serde_json::to_value(value).expect("serialize gap reason");
            assert_eq!(encoded, json!(token));
            assert_eq!(
                serde_json::from_value::<DeliveryGapReason>(encoded)
                    .expect("deserialize gap reason"),
                value
            );
        }
    }

    #[test]
    fn delivery_report_round_trips_exact_ranges_and_all_terminal_buckets() {
        let report = DeliveryReportPayload {
            per_class: DeliveryCountersByClass {
                reliable: ReliableDeliveryCounters {
                    delivered: 10,
                    abandoned: 1,
                    unsupported_format: 2,
                },
                latest: LatestDeliveryCounters {
                    delivered: 20,
                    superseded: 3,
                    dropped_full: 4,
                    abandoned: 5,
                    unsupported_format: 6,
                },
                volatile: VolatileDeliveryCounters {
                    delivered: 30,
                    dropped: 7,
                    abandoned: 8,
                    unsupported_format: 9,
                },
            },
            gaps: vec![DeliveryGap {
                from_player: PlayerId::from_u128(1),
                epoch: 2,
                from_seq: 11,
                to_seq: 14,
                reason: DeliveryGapReason::LatestSuperseded,
            }],
        };

        let encoded = serde_json::to_value(&report).expect("serialize delivery report");
        assert_eq!(
            encoded,
            json!({
                "per_class": {
                    "reliable": {
                        "delivered": 10,
                        "abandoned": 1,
                        "unsupported_format": 2
                    },
                    "latest": {
                        "delivered": 20,
                        "superseded": 3,
                        "dropped_full": 4,
                        "abandoned": 5,
                        "unsupported_format": 6
                    },
                    "volatile": {
                        "delivered": 30,
                        "dropped": 7,
                        "abandoned": 8,
                        "unsupported_format": 9
                    }
                },
                "gaps": [{
                    "from_player": "00000000-0000-0000-0000-000000000001",
                    "epoch": 2,
                    "from_seq": 11,
                    "to_seq": 14,
                    "reason": "latest_superseded"
                }]
            })
        );
        assert_eq!(
            serde_json::from_value::<DeliveryReportPayload>(encoded)
                .expect("deserialize delivery report"),
            report
        );
    }

    #[test]
    fn reliable_is_the_default_and_empty_gap_list_is_omitted() {
        assert_eq!(DeliveryClass::default(), DeliveryClass::Reliable);
        let encoded = serde_json::to_value(DeliveryReportPayload::default())
            .expect("serialize default report");
        assert!(encoded.get("gaps").is_none());
    }
}
