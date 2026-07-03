//! Prometheus text-exposition scraping for the MULTI-PROCESS delivery suites.
//!
//! The in-process suites assert the delivery contract directly against
//! `ServerMetrics` atomics; a spawned `signal-fish-server` binary offers no
//! such handle, so its counters are read the way an operator reads them —
//! over HTTP from the `/metrics/prom` endpoint (mounted at the router root by
//! `src/main.rs`; the per-test temp config sets `require_metrics_auth: false`
//! so no bearer token is needed). This module fetches the exposition text
//! (reqwest, already a dev-dependency) and parses individual un-labelled
//! samples, plus mirrors the two invariants the in-process helpers provide:
//!
//! - [`assert_scraped_message_conservation`] — the two-sided delivery
//!   conservation law over the scraped counters (see
//!   `websocket_test_helpers::assert_message_conservation` for the law's
//!   derivation);
//! - metric-driven polling via [`scrape_delivery_counters`] in the callers'
//!   own deadline loops (never sleeps as synchronization).
//!
//! Sample names come from `src/websocket/prometheus.rs`
//! (`render_prometheus_metrics`); every sample this module reads is a plain
//! `name value` line with no labels, which is all the exporter emits.

use std::time::Duration;

/// One scrape's worth of the delivery-contract counters (plus the
/// active-connections gauge used for reclaim assertions).
#[derive(Debug, Clone, Copy)]
pub struct DeliveryCounters {
    pub attempts: u64,
    pub enqueued: u64,
    pub channel_closed: u64,
    pub dropped: u64,
    pub slow_consumer_disconnects: u64,
    pub backpressure_events: u64,
    pub active_connections: u64,
}

/// Fetch the Prometheus exposition from `http://127.0.0.1:{port}/metrics/prom`
/// and panic on any transport/HTTP failure — a server that stops answering
/// its metrics endpoint mid-test is itself a bug worth failing loudly on.
pub async fn fetch_prometheus_text(port: u16) -> String {
    let url = format!("http://127.0.0.1:{port}/metrics/prom");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("build metrics scrape client");
    let response = client
        .get(&url)
        .send()
        .await
        .unwrap_or_else(|error| panic!("scrape {url} failed: {error}"));
    let status = response.status();
    assert!(
        status.is_success(),
        "scrape {url} answered {status} (is require_metrics_auth disabled in the temp config?)"
    );
    response
        .text()
        .await
        .unwrap_or_else(|error| panic!("read {url} body failed: {error}"))
}

/// Parse the single un-labelled sample `name value` from exposition text.
/// Panics when the sample is missing or malformed: a silently-defaulted
/// counter would let a delivery-contract violation pass unnoticed.
pub fn sample_value(text: &str, name: &str) -> u64 {
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(sample_name), Some(raw_value)) = (parts.next(), parts.next()) else {
            continue;
        };
        if sample_name != name {
            continue;
        }
        // Counters/gauges are `u64` rendered as exact integer text
        // (`render_prometheus_metrics` writes `{value}`), so parse `u64`
        // DIRECTLY — no float round-trip, exact for any counter value.
        if let Ok(exact) = raw_value.parse::<u64>() {
            return exact;
        }
        // Fallback for a value rendered with a decimal point (an f64 gauge
        // like `3.0`). Parsing back through f64 loses precision above 2^53 —
        // the exact-integer limit of f64 — so that, not `u64::MAX`, is the
        // honest ceiling: a value past it cannot round-trip and a naive
        // `as u64` cast would silently saturate. Anything non-finite,
        // fractional, negative, or past 2^53 is a broken/hostile exporter and
        // must panic loudly, never coerce.
        const F64_EXACT_INT_MAX: f64 = (1u64 << 53) as f64;
        let value: f64 = raw_value.parse().unwrap_or_else(|error| {
            panic!("sample {name} has non-numeric value {raw_value:?}: {error}")
        });
        assert!(
            value.is_finite() && value >= 0.0 && value.fract() == 0.0 && value <= F64_EXACT_INT_MAX,
            "sample {name} must be a non-negative integer within the f64 exact-integer \
             range (<= 2^53), got {value}"
        );
        return value as u64;
    }
    panic!("sample {name} not found in the scraped exposition:\n{text}");
}

/// Scrape the delivery-contract counters from a spawned server process.
pub async fn scrape_delivery_counters(port: u16) -> DeliveryCounters {
    let text = fetch_prometheus_text(port).await;
    DeliveryCounters {
        attempts: sample_value(&text, "signal_fish_websocket_delivery_attempts_total"),
        enqueued: sample_value(&text, "signal_fish_websocket_deliveries_enqueued_total"),
        channel_closed: sample_value(
            &text,
            "signal_fish_websocket_deliveries_channel_closed_total",
        ),
        dropped: sample_value(&text, "signal_fish_websocket_messages_dropped_total"),
        slow_consumer_disconnects: sample_value(
            &text,
            "signal_fish_websocket_slow_consumer_disconnects_total",
        ),
        backpressure_events: sample_value(&text, "signal_fish_websocket_backpressure_events_total"),
        active_connections: sample_value(&text, "signal_fish_connections_active"),
    }
}

/// The delivery-conservation law over a scraped snapshot, mirrored from
/// `websocket_test_helpers::assert_message_conservation`:
///
/// `enqueued + channel_closed <= attempts <= enqueued + channel_closed + dropped`
///
/// Call only at quiescent points (every send completed and observed), because
/// an in-flight delivery has its attempt counted before its outcome exists.
pub fn assert_scraped_message_conservation(counters: &DeliveryCounters) {
    let resolved = counters.enqueued + counters.channel_closed;
    assert!(
        resolved <= counters.attempts && counters.attempts <= resolved + counters.dropped,
        "delivery conservation violated on the scraped counters: expected \
         enqueued + channel_closed <= attempts <= enqueued + channel_closed + dropped, got \
         attempts={} enqueued={} channel_closed={} dropped={}",
        counters.attempts,
        counters.enqueued,
        counters.channel_closed,
        counters.dropped
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPOSITION: &str = "\
# HELP signal_fish_websocket_delivery_attempts_total help text\n\
# TYPE signal_fish_websocket_delivery_attempts_total counter\n\
signal_fish_websocket_delivery_attempts_total 42\n\
signal_fish_connections_active 3\n\
signal_fish_websocket_messages_dropped_total 0\n";

    #[test]
    fn sample_value_parses_counters_and_gauges() {
        assert_eq!(
            sample_value(EXPOSITION, "signal_fish_websocket_delivery_attempts_total"),
            42
        );
        assert_eq!(
            sample_value(EXPOSITION, "signal_fish_connections_active"),
            3
        );
        assert_eq!(
            sample_value(EXPOSITION, "signal_fish_websocket_messages_dropped_total"),
            0
        );
    }

    #[test]
    fn sample_value_ignores_comment_lines_and_prefix_collisions() {
        // `...attempts_total` must not match the HELP/TYPE lines naming it,
        // nor a metric whose name merely prefixes another.
        let text = "# HELP foo_total x\nfoo_total_extra 9\nfoo_total 7\n";
        assert_eq!(sample_value(text, "foo_total"), 7);
    }

    #[test]
    #[should_panic(expected = "not found in the scraped exposition")]
    fn sample_value_panics_on_missing_sample() {
        sample_value(EXPOSITION, "signal_fish_nonexistent_total");
    }

    #[test]
    #[should_panic(expected = "must be a non-negative integer within the f64 exact-integer")]
    fn sample_value_panics_on_out_of_range_value() {
        // Finite and whole, but far past the f64 exact-integer range: it does
        // not parse as `u64`, and the f64 fallback's saturating cast would
        // silently coerce it without the range guard.
        sample_value(
            "signal_fish_hostile_total 1e100\n",
            "signal_fish_hostile_total",
        );
    }

    #[test]
    fn conservation_law_accepts_balanced_and_rejects_unbalanced() {
        let balanced = DeliveryCounters {
            attempts: 10,
            enqueued: 7,
            channel_closed: 2,
            dropped: 1,
            slow_consumer_disconnects: 1,
            backpressure_events: 4,
            active_connections: 2,
        };
        assert_scraped_message_conservation(&balanced);

        let silent_loss = DeliveryCounters {
            attempts: 10,
            enqueued: 5,
            channel_closed: 2,
            dropped: 1,
            ..balanced
        };
        let violation = std::panic::catch_unwind(|| {
            assert_scraped_message_conservation(&silent_loss);
        });
        assert!(
            violation.is_err(),
            "attempts exceeding enqueued+channel_closed+dropped must fail the law"
        );
    }
}
