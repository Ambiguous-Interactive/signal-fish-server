use std::future::pending;
use std::pin::Pin;
use std::time::Duration;
use tokio::time::{Instant, MissedTickBehavior, Sleep};

/// Periodic timer with checked deadline arithmetic, including missed ticks.
/// A deadline beyond the platform clock range stays pending. A zero period
/// emits only the initial tick.
pub(crate) struct Interval {
    delay: Pin<Box<Sleep>>,
    next: Option<Instant>,
    period: Duration,
    missed_tick_behavior: MissedTickBehavior,
}

pub(crate) fn interval(period: Duration) -> Interval {
    let start = Instant::now();
    Interval {
        delay: Box::pin(tokio::time::sleep_until(start)),
        next: Some(start),
        period,
        missed_tick_behavior: MissedTickBehavior::Burst,
    }
}

impl Interval {
    pub(crate) fn set_missed_tick_behavior(&mut self, behavior: MissedTickBehavior) {
        self.missed_tick_behavior = behavior;
    }

    pub(crate) async fn tick(&mut self) -> Instant {
        let Some(scheduled) = self.next else {
            return pending().await;
        };
        self.delay.as_mut().await;
        let now = Instant::now();
        self.next = self.next_deadline(scheduled, now);
        if let Some(next) = self.next {
            self.delay.as_mut().reset(next);
        }
        scheduled
    }

    fn next_deadline(&self, scheduled: Instant, now: Instant) -> Option<Instant> {
        if self.period.is_zero() {
            return None;
        }
        if now.saturating_duration_since(scheduled) <= Duration::from_millis(5) {
            return scheduled.checked_add(self.period);
        }
        match self.missed_tick_behavior {
            MissedTickBehavior::Burst => scheduled.checked_add(self.period),
            MissedTickBehavior::Delay => now.checked_add(self.period),
            MissedTickBehavior::Skip => {
                now.checked_add(self.skipped_delay(now.saturating_duration_since(scheduled))?)
            }
        }
    }

    fn skipped_delay(&self, elapsed: Duration) -> Option<Duration> {
        let remainder = elapsed.as_nanos().checked_rem(self.period.as_nanos())?;
        let seconds = u64::try_from(remainder.checked_div(1_000_000_000)?).ok()?;
        let nanos = u32::try_from(remainder.checked_rem(1_000_000_000)?).ok()?;
        self.period.checked_sub(Duration::new(seconds, nanos))
    }
}

/// Convert a relative duration into an absolute deadline without changing an
/// overflow into an already-expired instant.
pub(crate) fn after(start: Instant, duration: Duration) -> Option<Instant> {
    start.checked_add(duration)
}

/// Wait for a representable deadline. An overflowed deadline is later than
/// this process can represent, so that branch remains pending.
pub(crate) async fn wait_until(deadline: Option<Instant>) -> Instant {
    match deadline {
        Some(deadline) => {
            tokio::time::sleep_until(deadline).await;
            deadline
        }
        None => pending().await,
    }
}

/// Saturating counterpart to [`after`]: a duration that no longer fits the
/// platform's `Instant` range becomes a deadline as far in the future as the
/// platform can represent instead of an already-expired instant.
///
/// The only way this returns a deadline at or before `start` is a clock already
/// at its platform's maximal representable instant — unreachable in practice,
/// since every supported platform measures from process/boot start.
///
/// Consumers that need to distinguish "beyond process lifetime" from a real
/// timestamp should use [`after`] and branch on `None`; this helper is for
/// seams whose shape requires a concrete `Instant`.
pub(crate) fn saturating_after(start: Instant, duration: Duration) -> Instant {
    match start.checked_add(duration) {
        Some(deadline) => deadline,
        None => {
            // Advance by the largest representable chunks: each successful
            // addition moves the deadline further out, each overflow halves
            // the chunk until it fits. The chunk reaches zero after finitely
            // many halvings, and every accepted chunk keeps `latest` strictly
            // increasing, so this saturates instead of expiring immediately.
            // Halving uses `checked_div` because the crate denies arithmetic
            // with panic potential even where overflow is impossible.
            let mut step = Duration::from_secs(u64::MAX);
            let mut latest = start;
            while step > Duration::ZERO {
                if let Some(candidate) = latest.checked_add(step) {
                    latest = candidate;
                }
                step = step.checked_div(2).unwrap_or(Duration::ZERO);
            }
            latest
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::future::Future;

    pub(crate) async fn park_before_first_tick<F: std::future::Future>(
        mut future: std::pin::Pin<&mut F>,
    ) {
        std::future::poll_fn(|cx| {
            loop {
                let consume = tokio::task::consume_budget();
                tokio::pin!(consume);
                if consume.poll(cx).is_pending() {
                    break;
                }
            }
            assert!(future.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
    }

    #[test]
    fn representable_durations_keep_their_exact_absolute_instant() {
        let start = Instant::now();
        assert_eq!(
            after(start, Duration::from_secs(30)),
            Some(start + Duration::from_secs(30))
        );
    }

    #[test]
    fn unrepresentable_durations_never_invert_into_immediate_expiry() {
        let start = Instant::now();
        let unrepresentable = Duration::from_secs(u64::MAX);

        assert_eq!(
            after(start, unrepresentable),
            None,
            "an overflowed duration is beyond the process lifetime"
        );
        assert!(
            saturating_after(start, unrepresentable) > start,
            "saturation must not turn an overflow into an already-expired instant"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn periodic_cadence_matches_tokio_including_missed_ticks() {
        for behavior in [
            MissedTickBehavior::Burst,
            MissedTickBehavior::Delay,
            MissedTickBehavior::Skip,
        ] {
            for lateness in [0, 5, 6, 10, 250] {
                let mut checked = interval(Duration::from_millis(100));
                let mut reference = tokio::time::interval(Duration::from_millis(100));
                checked.set_missed_tick_behavior(behavior);
                reference.set_missed_tick_behavior(behavior);
                tokio::time::advance(Duration::from_millis(lateness)).await;
                for _ in 0..4 {
                    let (actual, expected) = tokio::join!(checked.tick(), reference.tick());
                    assert_eq!(
                        actual, expected,
                        "{behavior:?}, initial lateness {lateness} ms"
                    );
                }
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn nonrepeating_period_ticks_once_then_remains_cancellable() {
        for period in [Duration::ZERO, Duration::MAX] {
            for behavior in [
                MissedTickBehavior::Burst,
                MissedTickBehavior::Delay,
                MissedTickBehavior::Skip,
            ] {
                let start = Instant::now();
                let mut timer = interval(period);
                timer.set_missed_tick_behavior(behavior);
                tokio::time::advance(Duration::from_millis(10)).await;
                assert_eq!(timer.tick().await, start);
                assert!(timer.next.is_none());
                for _ in 0..2 {
                    assert!(tokio::time::timeout(Duration::from_millis(1), timer.tick())
                        .await
                        .is_err());
                }
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_tick_preserves_its_scheduled_deadline() {
        let mut timer = interval(Duration::from_secs(1));
        let start = timer.tick().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(10), timer.tick())
                .await
                .is_err()
        );
        tokio::time::advance(Duration::from_millis(990)).await;
        assert_eq!(timer.tick().await, start + Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn skipped_ticks_handle_elapsed_nanoseconds_beyond_u64() {
        let mut timer = interval(Duration::from_secs(1_000_000_000_000));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let elapsed = Duration::from_secs(20_000_000_000);
        assert!(elapsed.as_nanos() > u128::from(u64::MAX));
        assert_eq!(
            timer.skipped_delay(elapsed),
            timer.period.checked_sub(elapsed)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn boundary_period_preserves_representable_skip_and_burst_deadlines() {
        let scheduled = Instant::now();
        let maximum = saturating_after(scheduled, Duration::MAX);
        let mut timer = interval(maximum.duration_since(scheduled));
        let delayed = scheduled + Duration::from_millis(10);
        for behavior in [MissedTickBehavior::Burst, MissedTickBehavior::Skip] {
            timer.set_missed_tick_behavior(behavior);
            assert_eq!(timer.next_deadline(scheduled, delayed), Some(maximum));
            assert_eq!(timer.next_deadline(maximum, maximum), None);
        }
        timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
        assert_eq!(timer.next_deadline(scheduled, delayed), None);
    }
}
