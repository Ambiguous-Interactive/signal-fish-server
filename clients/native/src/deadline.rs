//! Overflow-safe Tokio deadlines for caller-supplied durations.
//!
//! A duration that cannot be represented as an absolute [`Instant`] is
//! effectively beyond this process's lifetime. It must never wrap or fall
//! back to `Instant::now()`, because either interpretation turns a distant
//! accepted timeout into immediate expiry.

use std::future::Future;
use std::time::Duration;

use tokio::time::{error::Elapsed, Instant};

/// An absolute deadline, or one beyond the process's representable lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Deadline(Option<Instant>);

impl From<Instant> for Deadline {
    fn from(value: Instant) -> Self {
        Self(Some(value))
    }
}

impl Deadline {
    /// Derive a deadline without converting arithmetic overflow into expiry.
    #[must_use]
    pub(crate) fn after(start: Instant, duration: Duration) -> Self {
        Self(start.checked_add(duration))
    }

    /// The finite absolute instant, when the platform can represent it.
    #[must_use]
    pub(crate) fn finite(self) -> Option<Instant> {
        self.0
    }

    /// Whether this deadline has elapsed at `now`.
    #[must_use]
    pub(crate) fn is_due(self, now: Instant) -> bool {
        self.0.is_some_and(|deadline| now >= deadline)
    }

    pub(crate) async fn timeout<F>(self, future: F) -> Result<F::Output, Elapsed>
    where
        F: Future,
    {
        match self.0 {
            Some(deadline) => tokio::time::timeout_at(deadline, future).await,
            None => Ok(future.await),
        }
    }
}

/// Await `future` with `duration`, treating overflow as no process-lifetime
/// deadline instead of an immediate timeout or panic.
pub fn timeout<F>(duration: Duration, future: F) -> impl Future<Output = Result<F::Output, Elapsed>>
where
    F: Future,
{
    // Box before constructing the async state machine. The reference
    // client's top-level future is large enough that retaining it inline
    // here can exhaust Windows' smaller main-thread stack.
    let future = Box::pin(future);
    async move {
        // Match `tokio::time::timeout` semantics: relative timeout accounting
        // starts when the returned future is first polled, not when it is
        // constructed and potentially stored for later.
        Deadline::after(Instant::now(), duration)
            .timeout(future)
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::future;

    use super::*;

    #[test]
    fn deadline_preserves_zero_finite_and_unrepresentable_boundaries() {
        let now = Instant::now();
        let cases = [
            (Duration::ZERO, true, true, "zero is immediately due"),
            (
                Duration::from_secs(1),
                true,
                false,
                "ordinary duration remains in the future",
            ),
            (
                Duration::from_secs(u64::MAX),
                false,
                false,
                "largest CLI duration is beyond process lifetime",
            ),
        ];

        for (duration, finite, due, description) in cases {
            let deadline = Deadline::after(now, duration);
            assert_eq!(deadline.finite().is_some(), finite, "{description}");
            assert_eq!(deadline.is_due(now), due, "{description}");
        }
    }

    #[tokio::test]
    async fn unrepresentable_timeout_does_not_complete_a_pending_future() {
        let outcome = tokio::time::timeout(
            Duration::from_millis(20),
            timeout(Duration::from_secs(u64::MAX), future::pending::<()>()),
        )
        .await;

        assert!(
            outcome.is_err(),
            "the outer finite test timeout must expire before an unrepresentable inner timeout"
        );
    }

    #[tokio::test]
    async fn unrepresentable_timeout_still_returns_ready_output() {
        let outcome = timeout(Duration::from_secs(u64::MAX), future::ready(42)).await;
        assert!(matches!(outcome, Ok(42)));
    }

    #[tokio::test(start_paused = true)]
    async fn relative_timeout_duration_starts_on_first_poll() {
        let duration = Duration::from_secs(5);
        let stored = timeout(duration, future::pending::<()>());
        tokio::pin!(stored);

        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(
            futures_util::poll!(stored.as_mut()).is_pending(),
            "storing an unpolled timeout must not consume its duration"
        );

        tokio::time::advance(duration - Duration::from_millis(1)).await;
        assert!(
            futures_util::poll!(stored.as_mut()).is_pending(),
            "the timeout must remain pending until one full duration after its first poll"
        );

        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(
            matches!(
                futures_util::poll!(stored.as_mut()),
                std::task::Poll::Ready(Err(_))
            ),
            "the timeout must expire one full duration after its first poll"
        );
    }

    #[test]
    fn timeout_future_boxes_large_callers_before_building_its_state_machine() {
        let large_caller = async move {
            let payload = std::hint::black_box([0_u8; 256 * 1024]);
            future::pending::<()>().await;
            std::hint::black_box(payload);
        };
        let wrapped = timeout(Duration::from_secs(1), large_caller);

        assert!(
            std::mem::size_of_val(&wrapped) < 1024,
            "the timeout wrapper must not retain a large caller future inline"
        );
    }
}
