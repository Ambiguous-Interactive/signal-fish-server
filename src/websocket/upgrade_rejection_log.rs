//! Per-source throttled warnings for rejected WebSocket upgrades.
//!
//! The upgrade endpoint is unauthenticated, so one `warn!` per rejection would
//! let an anonymous request loop grow the log indefinitely — the same
//! log-disk amplification class the metrics endpoints closed in #410. Unlike
//! metric rejections, an upgrade rejection carries per-source forensics, and
//! collapsing by outcome alone would merge distinct attackers into one line.
//! Warnings are therefore throttled per `(peer, outcome)` window: the first
//! occurrence is emitted verbatim, in-window repeats are counted silently,
//! and the next emission after the quiet period summarizes them.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Minimum quiet period between emitted WebSocket-upgrade-rejection warnings
/// for one source.
///
/// Sixty seconds matches the metrics-rejection throttle: it keeps the first
/// signal and a periodic suppressed-count summary at negligible volume while
/// an attacker gains nothing from request volume.
const UPGRADE_REJECTION_LOG_MIN_INTERVAL: Duration = Duration::from_secs(60);

/// Maximum simultaneously tracked `(peer, outcome)` windows.
///
/// The map must stay bounded because rotating real source addresses across a
/// botnet or NAT pool is a cheaper attack than the log volume it would
/// otherwise cause. 4096 windows
/// cover legitimate deployment scales (NAT gateways plus distinct outcomes)
/// at a few hundred kilobytes; eviction prefers windows that already elapsed
/// their quiet period, then the least recently useful live window.
const UPGRADE_REJECTION_LOG_MAX_TRACKED_SOURCES: usize = 4096;

/// One decision to emit an upgrade-rejection warning for a source.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UpgradeRejectionLogEmission {
    /// The first rejection for this source after a quiet period (or ever).
    First,
    /// A quiet-period boundary reached while earlier rejections were
    /// suppressed; the count summarizes them.
    WithSuppressedCount { suppressed: u64 },
}

#[derive(Default)]
struct SourceWindows {
    /// Last emission instant plus suppressed-rejection count per source.
    windows: HashMap<(IpAddr, &'static str), (Instant, u64)>,
}

impl SourceWindows {
    fn within_quiet_period(last_emission: Instant, now: Instant) -> bool {
        now.saturating_duration_since(last_emission) < UPGRADE_REJECTION_LOG_MIN_INTERVAL
    }

    /// Drop windows whose quiet period already elapsed, then — if still at
    /// capacity — evict the live window with the oldest last emission,
    /// tie-broken by source key so exactly one loser always results.
    fn make_room(&mut self, now: Instant) {
        if self.windows.len() < UPGRADE_REJECTION_LOG_MAX_TRACKED_SOURCES {
            return;
        }
        self.windows
            .retain(|_, (last_emission, _)| Self::within_quiet_period(*last_emission, now));
        if self.windows.len() >= UPGRADE_REJECTION_LOG_MAX_TRACKED_SOURCES {
            // Tie-break equal emissions by source key so eviction is
            // deterministic regardless of hash-map iteration order.
            let oldest = self
                .windows
                .iter()
                .min_by_key(|(key, (last_emission, _))| (*last_emission, *key))
                .map(|(key, _)| *key);
            if let Some(key) = oldest {
                self.windows.remove(&key);
            }
        }
    }

    fn record_at(
        &mut self,
        source: (IpAddr, &'static str),
        now: Instant,
    ) -> Option<UpgradeRejectionLogEmission> {
        if let Some((last_emission, suppressed)) = self.windows.get_mut(&source) {
            if Self::within_quiet_period(*last_emission, now) {
                // A u64 counter cannot saturate from log throttling alone.
                *suppressed = suppressed.saturating_add(1);
                return None;
            }
            let emission = UpgradeRejectionLogEmission::WithSuppressedCount {
                suppressed: *suppressed,
            };
            *last_emission = now;
            *suppressed = 0;
            return Some(emission);
        }
        self.make_room(now);
        self.windows.insert(source, (now, 0));
        Some(UpgradeRejectionLogEmission::First)
    }
}

/// Emits at most one rejected-upgrade warning per source quiet period,
/// counting suppressed repeats so the next emission carries their number.
///
/// The decision logic is pure (tests drive it with synthetic instants); the
/// handler maps a returned [`UpgradeRejectionLogEmission`] to the actual
/// `tracing::warn!` with the triggering request's correlation fields.
#[derive(Default)]
pub(crate) struct UpgradeRejectionLogThrottle {
    state: Mutex<SourceWindows>,
}

impl UpgradeRejectionLogThrottle {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record one rejected upgrade from `peer_ip` with `outcome` at `now`.
    ///
    /// Returns the emission to log, or `None` when the rejection falls inside
    /// the source's quiet period following a previous emission.
    fn record_at(
        &self,
        peer_ip: IpAddr,
        outcome: &'static str,
        now: Instant,
    ) -> Option<UpgradeRejectionLogEmission> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.record_at((peer_ip, outcome), now)
    }

    /// Production entry point: decide and emit the warning in one step.
    ///
    /// The first emission keeps the exact field set of the historical
    /// per-request warning; boundary summaries add only `suppressed_repeats`.
    pub(crate) fn record(
        &self,
        peer_ip: IpAddr,
        outcome: &'static str,
        request_id: &str,
        http_status: u16,
    ) {
        match self.record_at(peer_ip, outcome, Instant::now()) {
            Some(UpgradeRejectionLogEmission::First) => {
                tracing::warn!(
                    request_id,
                    peer_ip = %peer_ip,
                    outcome,
                    http_status,
                    "WebSocket upgrade rejected"
                );
            }
            Some(UpgradeRejectionLogEmission::WithSuppressedCount { suppressed }) => {
                tracing::warn!(
                    request_id,
                    peer_ip = %peer_ip,
                    outcome,
                    http_status,
                    suppressed_repeats = suppressed,
                    "WebSocket upgrade rejections (throttled)"
                );
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn ipv4(octets: [u8; 4]) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(octets))
    }

    fn ipv4_from_index(index: u32) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(index))
    }

    fn ipv6(segment: u16) -> IpAddr {
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, segment))
    }

    #[track_caller]
    fn assert_emission(
        throttle: &UpgradeRejectionLogThrottle,
        peer_ip: IpAddr,
        outcome: &'static str,
        now: Instant,
        expected: Option<UpgradeRejectionLogEmission>,
        context: &str,
    ) {
        assert_eq!(
            throttle.record_at(peer_ip, outcome, now),
            expected,
            "{context}"
        );
    }

    /// Data-driven, sleep-free: one source emits its first rejection,
    /// suppresses everything inside its quiet period (counting the
    /// suppressions), summarizes them at the next boundary, and starts a
    /// fresh quiet period.
    #[test]
    fn upgrade_rejection_throttle_emits_first_and_window_summaries() {
        let throttle = UpgradeRejectionLogThrottle::new();
        let start = Instant::now();
        let peer = ipv4([192, 0, 2, 7]);
        let step = UPGRADE_REJECTION_LOG_MIN_INTERVAL / 4;

        assert_emission(
            &throttle,
            peer,
            "rejected_origin",
            start,
            Some(UpgradeRejectionLogEmission::First),
            "the first rejection from a source is always emitted",
        );

        // Three suppressed rejections inside the quiet period...
        for offset in [1 * step, 2 * step, 3 * step] {
            assert_emission(
                &throttle,
                peer,
                "rejected_origin",
                start + offset,
                None,
                "rejections inside the quiet period must be suppressed",
            );
        }

        // ...then the boundary emission carries their count.
        let boundary = start + UPGRADE_REJECTION_LOG_MIN_INTERVAL;
        assert_emission(
            &throttle,
            peer,
            "rejected_origin",
            boundary,
            Some(UpgradeRejectionLogEmission::WithSuppressedCount { suppressed: 3 }),
            "the first post-window emission must summarize suppressed repeats",
        );

        // The quiet period restarts after a summary emission; an immediate
        // repeat is suppressed again rather than double-counted.
        assert_emission(
            &throttle,
            peer,
            "rejected_origin",
            boundary + Duration::from_secs(1),
            None,
            "the summary emission starts a fresh quiet period",
        );
        assert_emission(
            &throttle,
            peer,
            "rejected_origin",
            boundary + UPGRADE_REJECTION_LOG_MIN_INTERVAL,
            Some(UpgradeRejectionLogEmission::WithSuppressedCount { suppressed: 1 }),
            "the next boundary carries exactly the repeats since the summary",
        );
    }

    /// Distinct peers and outcomes each keep their own first warning: an
    /// active attacker cannot merge victims or outcomes into one line, and
    /// one noisy source cannot silence another.
    #[test]
    fn distinct_peers_and_outcomes_throttle_independently() {
        let throttle = UpgradeRejectionLogThrottle::new();
        let start = Instant::now();
        let attacker = ipv4([203, 0, 113, 10]);
        let victim_a = ipv4([198, 51, 100, 21]);
        let victim_b = ipv6(0x42);

        for (peer_ip, context) in [
            (attacker, "attacker emits the very first warning"),
            (
                victim_a,
                "a distinct peer emits its own first warning immediately",
            ),
            (
                victim_b,
                "an IPv6 peer also emits its own first warning immediately",
            ),
        ] {
            assert_emission(
                &throttle,
                peer_ip,
                "rejected_draining",
                start,
                Some(UpgradeRejectionLogEmission::First),
                context,
            );
        }

        assert_emission(
            &throttle,
            attacker,
            "rejected_token_binding_offer",
            start + Duration::from_secs(30),
            Some(UpgradeRejectionLogEmission::First),
            "a different outcome from the same peer keeps its own window",
        );
        assert_emission(
            &throttle,
            attacker,
            "rejected_draining",
            start + Duration::from_secs(31),
            None,
            "the attacker's original outcome stays throttled by the others",
        );
    }

    /// At capacity, expired windows are reclaimed before any live window is
    /// evicted, so steady-state churn never disturbs actively warning sources.
    #[test]
    fn expired_windows_are_reclaimed_before_live_eviction() {
        let capacity =
            u32::try_from(UPGRADE_REJECTION_LOG_MAX_TRACKED_SOURCES).expect("capacity fits u32");
        let throttle = UpgradeRejectionLogThrottle::new();
        let start = Instant::now();
        for index in 0..capacity {
            assert_emission(
                &throttle,
                ipv4_from_index(index),
                "rejected_origin",
                start,
                Some(UpgradeRejectionLogEmission::First),
                "capacity fill emits one first warning per fresh source",
            );
        }

        // After the quiet period every stored window is expired, so inserting
        // one more source reclaims them instead of evicting a live neighbor.
        let after_expiry = start + UPGRADE_REJECTION_LOG_MIN_INTERVAL + Duration::from_secs(1);
        assert_emission(
            &throttle,
            ipv6(0xffff),
            "rejected_origin",
            after_expiry,
            Some(UpgradeRejectionLogEmission::First),
            "insertion past capacity succeeds via expired-window reclaim",
        );
        assert_emission(
            &throttle,
            ipv4_from_index(0),
            "rejected_origin",
            after_expiry + Duration::from_secs(1),
            Some(UpgradeRejectionLogEmission::First),
            "an expired source starts a fresh window instead of resuming suppression",
        );

        // Live windows are untouched: a surviving source still summarizes on
        // schedule even though two other sources were admitted around it.
        assert_emission(
            &throttle,
            ipv6(0xffff),
            "rejected_origin",
            after_expiry + UPGRADE_REJECTION_LOG_MIN_INTERVAL,
            Some(UpgradeRejectionLogEmission::WithSuppressedCount { suppressed: 0 }),
            "an unchallenged window summarizes zero suppressed repeats at its boundary",
        );
    }

    /// When no window has expired, capacity eviction deterministically drops
    /// the least recently useful live window and memory stays bounded.
    #[test]
    fn capacity_eviction_drops_the_oldest_live_window_when_none_expired() {
        let capacity =
            u32::try_from(UPGRADE_REJECTION_LOG_MAX_TRACKED_SOURCES).expect("capacity fits u32");
        let throttle = UpgradeRejectionLogThrottle::new();
        let start = Instant::now();
        for index in 0..capacity {
            assert_emission(
                &throttle,
                ipv4_from_index(index),
                "rejected_origin",
                start,
                Some(UpgradeRejectionLogEmission::First),
                "capacity fill emits one first warning per fresh source",
            );
        }

        // Every window is live, so this insertion must evict exactly the
        // oldest emitter (tie-broken by smallest source key) instead of
        // growing the map or panicking.
        assert_emission(
            &throttle,
            ipv6(0x1234),
            "rejected_origin",
            start + Duration::from_secs(1),
            Some(UpgradeRejectionLogEmission::First),
            "insertion past full capacity still emits a bounded first warning",
        );

        // The evicted oldest source lost its window entirely: it restarts
        // with a fresh first emission rather than a boundary summary.
        assert_emission(
            &throttle,
            ipv4_from_index(0),
            "rejected_origin",
            start + Duration::from_secs(2),
            Some(UpgradeRejectionLogEmission::First),
            "eviction resets the source instead of preserving stale suppression",
        );

        // A survivor keeps its window: its repeat is suppressed even though
        // two other sources were admitted around it.
        assert_emission(
            &throttle,
            ipv4_from_index(capacity - 1),
            "rejected_origin",
            start + Duration::from_secs(3),
            None,
            "non-evicted survivors remain throttled within their quiet period",
        );
    }

    /// Mixed live and expired windows at capacity: reclaiming expired
    /// windows must spare a live neighbor that just crossed its boundary,
    /// so an insertion-heavy flood cannot reset actively warning sources.
    #[test]
    fn mixed_capacity_reclaim_spares_refreshed_live_windows() {
        let capacity =
            u32::try_from(UPGRADE_REJECTION_LOG_MAX_TRACKED_SOURCES).expect("capacity fits u32");
        let throttle = UpgradeRejectionLogThrottle::new();
        let start = Instant::now();
        for index in 0..capacity {
            assert_emission(
                &throttle,
                ipv4_from_index(index),
                "rejected_origin",
                start,
                Some(UpgradeRejectionLogEmission::First),
                "capacity fill emits one first warning per fresh source",
            );
        }

        // Cross the quiet period, then re-emit exactly one source so its
        // boundary summary restarts a live 60-second window while every
        // other stored window ages out.
        let survivor = ipv4_from_index(capacity - 1);
        let after_expiry = start + UPGRADE_REJECTION_LOG_MIN_INTERVAL + Duration::from_secs(1);
        assert_emission(
            &throttle,
            survivor,
            "rejected_origin",
            after_expiry,
            Some(UpgradeRejectionLogEmission::WithSuppressedCount { suppressed: 0 }),
            "the refreshed source summarizes its unchallenged window at the boundary",
        );

        // Inserting a fresh source now must reclaim only the stale windows...
        assert_emission(
            &throttle,
            ipv6(0xabcd),
            "rejected_origin",
            after_expiry + Duration::from_secs(1),
            Some(UpgradeRejectionLogEmission::First),
            "the fresh source is admitted via expired-window reclaim",
        );

        // ...leaving the refreshed live window fully intact: its repeat
        // inside the new quiet period stays suppressed instead of restarting
        // as a first emission.
        assert_emission(
            &throttle,
            survivor,
            "rejected_origin",
            after_expiry + Duration::from_secs(2),
            None,
            "reclaim must not disturb the live window's quiet period",
        );

        // A reclaimed stale peer restarts cleanly rather than resuming its
        // pre-expiry suppression state.
        assert_emission(
            &throttle,
            ipv4_from_index(0),
            "rejected_origin",
            after_expiry + Duration::from_secs(3),
            Some(UpgradeRejectionLogEmission::First),
            "a reclaimed expired source starts a fresh first-emission window",
        );
    }
}
