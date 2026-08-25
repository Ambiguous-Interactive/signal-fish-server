use std::future::pending;
use std::time::Duration;
use tokio::time::Instant;

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
            let mut step = Duration::from_secs(u64::MAX);
            let mut latest = start;
            while step > Duration::ZERO {
                if let Some(candidate) = latest.checked_add(step) {
                    latest = candidate;
                } else {
                    step /= 2;
                }
            }
            debug_assert!(latest > start, "saturation must never expire immediately");
            latest
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
