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
