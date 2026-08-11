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
    pub canceled: u64,
    pub dropped: u64,
    pub slow_consumer_disconnects: u64,
    pub backpressure_events: u64,
    pub active_connections: u64,
}

/// Process-wide scrape client, built once and reused. Delivery suites poll
/// `/metrics/prom` in tight deadline loops; a fresh `reqwest::Client` per
/// scrape would rebuild a connection pool + TLS config every iteration. A
/// `reqwest::Client` is internally reference-counted and cheap to clone, so
/// one shared instance is the idiomatic, allocation-light choice.
fn scrape_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("build metrics scrape client")
    })
}

/// Fetch the Prometheus exposition from `http://127.0.0.1:{port}/metrics/prom`
/// and panic on any transport/HTTP failure — a server that stops answering
/// its metrics endpoint mid-test is itself a bug worth failing loudly on.
pub async fn fetch_prometheus_text(port: u16) -> String {
    let url = format!("http://127.0.0.1:{port}/metrics/prom");
    let response = scrape_client()
        .get(&url)
        .send()
        .await
        .unwrap_or_else(|error| panic!("scrape {url} failed: {error}"));
    let status = response.status();
    // Always drain the body first — even on a non-2xx — so the shared client's
    // connection is cleanly released (an undrained `Response` can leak it and
    // disturb later scrapes in the same process) and the failure carries any
    // server-provided error text.
    let body = response
        .text()
        .await
        .unwrap_or_else(|error| panic!("read {url} body failed: {error}"));
    assert!(
        status.is_success(),
        "scrape {url} answered {status} (is require_metrics_auth disabled in the temp config?): {body}"
    );
    body
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
        // Accept decimal rendering only when its fractional part is all zero.
        // Parse the integer digits directly so large exact counters never pass
        // through f64 and silently lose precision.
        if let Some((integer, fraction)) = raw_value.split_once('.') {
            assert!(
                !integer.starts_with('-')
                    && !integer.is_empty()
                    && !fraction.is_empty()
                    && fraction.bytes().all(|digit| digit == b'0'),
                "sample {name} must be a non-negative integer, got {raw_value:?}"
            );
            return integer.parse::<u64>().unwrap_or_else(|error| {
                panic!("sample {name} has out-of-range integer value {raw_value:?}: {error}")
            });
        }
        panic!("sample {name} has non-integer value {raw_value:?}");
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
        canceled: sample_value(&text, "signal_fish_websocket_deliveries_canceled_total"),
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
/// `enqueued + channel_closed + canceled <= attempts <= enqueued + channel_closed + canceled + dropped`
///
/// Call only at quiescent points (every send completed and observed), because
/// an in-flight delivery has its attempt counted before its outcome exists.
pub fn assert_scraped_message_conservation(counters: &DeliveryCounters) {
    let resolved = counters.enqueued + counters.channel_closed + counters.canceled;
    assert!(
        resolved <= counters.attempts && counters.attempts <= resolved + counters.dropped,
        "delivery conservation violated on the scraped counters: expected \
         enqueued + channel_closed + canceled <= attempts <= enqueued + channel_closed + \
         canceled + dropped, got attempts={} enqueued={} channel_closed={} canceled={} \
         dropped={}",
        counters.attempts,
        counters.enqueued,
        counters.channel_closed,
        counters.canceled,
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
signal_fish_websocket_deliveries_canceled_total 1\n\
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
        assert_eq!(
            sample_value(
                EXPOSITION,
                "signal_fish_websocket_deliveries_canceled_total"
            ),
            1
        );
        assert_eq!(
            sample_value(
                "signal_fish_large_integer_gauge 18446744073709551615.0\n",
                "signal_fish_large_integer_gauge"
            ),
            u64::MAX
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
    #[should_panic(expected = "has non-integer value")]
    fn sample_value_panics_on_out_of_range_value() {
        // Scientific notation is not an exact integer representation for this
        // test helper and must never silently saturate through an f64 cast.
        sample_value(
            "signal_fish_hostile_total 1e100\n",
            "signal_fish_hostile_total",
        );
    }

    #[test]
    fn conservation_law_accepts_balanced_and_rejects_unbalanced() {
        let balanced = DeliveryCounters {
            attempts: 10,
            enqueued: 6,
            channel_closed: 2,
            canceled: 1,
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
