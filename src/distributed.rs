use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tokio::time::MissedTickBehavior;
use uuid::Uuid;

/// Lock interface used for process-local room coordination.
///
/// The shipped implementation is in-memory and cannot coordinate server
/// processes. The trait is only an extension seam for a future backend.
#[async_trait]
pub trait DistributedLock: Send + Sync {
    /// Acquire a lock with specified TTL
    async fn acquire(&self, key: &str, ttl: Duration) -> Result<LockHandle>;

    /// Try to acquire a lock without blocking
    async fn try_acquire(&self, key: &str, ttl: Duration) -> Result<Option<LockHandle>>;

    /// Extend the TTL of an existing lock
    async fn extend(&self, handle: &LockHandle, ttl: Duration) -> Result<bool>;

    /// Release an active lock lease owned by `handle`.
    ///
    /// Returns `Ok(true)` only when an unexpired entry with the matching token
    /// was removed. Returns `Ok(false)` when the key is absent, the lease has
    /// expired, or another token owns the key. Implementations may reclaim a
    /// matching expired entry while returning `Ok(false)`.
    async fn release(&self, handle: &LockHandle) -> Result<bool>;

    /// Check if a lock is held
    async fn is_locked(&self, key: &str) -> Result<bool>;

    /// Cleanup expired locks - returns number of locks cleaned
    async fn cleanup_expired_locks(&self) -> Result<usize>;

    #[cfg(test)]
    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync);
}

/// Handle for a coordination lock.
#[derive(Debug, Clone)]
pub struct LockHandle {
    pub key: String,
    pub token: Uuid,
    pub acquired_at: chrono::DateTime<chrono::Utc>,
    pub ttl: Duration,
}

impl LockHandle {
    pub fn new(key: String, ttl: Duration) -> Self {
        Self {
            key,
            token: Uuid::new_v4(),
            // Wall clock (durable record): informational acquisition stamp on
            // the embedder-facing handle; the lease decision itself runs on
            // monotonic time (`try_acquire`).
            acquired_at: chrono::Utc::now(),
            ttl,
        }
    }
}

/// In-memory, process-local coordination lock.
pub struct InMemoryDistributedLock {
    locks: Arc<RwLock<HashMap<String, LockEntry>>>,
    #[cfg(test)]
    fail_acquire_key: Arc<RwLock<Option<String>>>,
}

#[derive(Debug, Clone)]
struct LockEntry {
    token: Uuid,
    /// Monotonic expiry of the lease.
    ///
    /// Wall-clock steps (NTP correction, manual clock change, host
    /// suspend/resume) must not shorten or extend a lease; the same
    /// discipline is pinned for circuit-breaker windows, reconnect windows,
    /// and client pings.
    expires_at: tokio::time::Instant,
}

impl InMemoryDistributedLock {
    pub fn new() -> Self {
        Self {
            locks: Arc::new(RwLock::new(HashMap::new())),
            #[cfg(test)]
            fail_acquire_key: Arc::new(RwLock::new(None)),
        }
    }

    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub(crate) async fn fail_acquire_for_test(&self, key: Option<String>) {
        *self.fail_acquire_key.write().await = key;
    }

    #[cfg(test)]
    async fn should_fail_acquire_for_test(&self, key: &str) -> bool {
        self.fail_acquire_key
            .read()
            .await
            .as_deref()
            .is_some_and(|failed_key| failed_key == key)
    }

    async fn cleanup_expired(&self) -> usize {
        let mut locks = self.locks.write().await;
        let now = tokio::time::Instant::now();
        let initial_count = locks.len();
        locks.retain(|_, entry| entry.expires_at > now);
        initial_count.saturating_sub(locks.len())
    }
}

impl Default for InMemoryDistributedLock {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DistributedLock for InMemoryDistributedLock {
    async fn acquire(&self, key: &str, ttl: Duration) -> Result<LockHandle> {
        #[cfg(test)]
        if self.should_fail_acquire_for_test(key).await {
            anyhow::bail!("injected lock acquisition failure for {key}");
        }
        // Retry while the key is held, but never past it: the whole scheduled
        // backoff stays strictly inside this lease's TTL, so a waiter can
        // neither abandon acquisition after the key already expired nor race a
        // later acquirer that took the re-expired key (issue #414).
        let executor = crate::retry::RetryExecutor::new(
            crate::retry::RetryConfig::persistent().clamped_to_total_backoff(ttl),
        );

        executor
            .execute_with_condition(
                "in_memory_lock_acquire",
                || {
                    let key = key.to_string();
                    async move {
                        match self.try_acquire(&key, ttl).await? {
                            Some(handle) => Ok(handle),
                            None => Err(anyhow::anyhow!("lock busy: {key}")),
                        }
                    }
                },
                |error| error.to_string().to_lowercase().contains("lock busy"),
            )
            .await
    }

    async fn try_acquire(&self, key: &str, ttl: Duration) -> Result<Option<LockHandle>> {
        #[cfg(test)]
        if self.should_fail_acquire_for_test(key).await {
            anyhow::bail!("injected lock acquisition failure for {key}");
        }

        // Single write lock acquisition: cleanup expired entries and check/insert atomically
        // to prevent TOCTOU races. Start the lease only after this internal
        // contention ends; otherwise a short lease can expire before this
        // method inserts it and returns ownership to its caller.
        let mut locks = self.locks.write().await;
        let now = tokio::time::Instant::now();
        locks.retain(|_, entry| entry.expires_at > now);

        if locks.contains_key(key) {
            return Ok(None);
        }

        let expires_at = now
            .checked_add(ttl)
            .ok_or_else(|| anyhow::anyhow!("lock TTL exceeds the supported clock range"))?;
        let handle = LockHandle {
            key: key.to_string(),
            token: Uuid::new_v4(),
            // Wall clock (durable record): informational acquisition stamp;
            // the lease expiry that decides contention is the monotonic
            // `expires_at` above.
            acquired_at: chrono::Utc::now(),
            ttl,
        };

        locks.insert(
            key.to_string(),
            LockEntry {
                token: handle.token,
                expires_at,
            },
        );

        Ok(Some(handle))
    }

    async fn extend(&self, handle: &LockHandle, ttl: Duration) -> Result<bool> {
        // Single write lock acquisition: cleanup and extend atomically. The
        // requested extension begins when the state can actually be changed,
        // not while this future is still waiting behind internal contention.
        let mut locks = self.locks.write().await;
        let now = tokio::time::Instant::now();
        locks.retain(|_, entry| entry.expires_at > now);

        if let Some(entry) = locks.get_mut(&handle.key) {
            if entry.token == handle.token {
                let new_expires_at = now
                    .checked_add(ttl)
                    .ok_or_else(|| anyhow::anyhow!("lock TTL exceeds the supported clock range"))?;
                entry.expires_at = new_expires_at;
                return Ok(true);
            }
        }

        Ok(false)
    }

    async fn release(&self, handle: &LockHandle) -> Result<bool> {
        let mut locks = self.locks.write().await;

        if let Some(entry) = locks.get(&handle.key) {
            if entry.token == handle.token {
                let lease_active = entry.expires_at > tokio::time::Instant::now();
                locks.remove(&handle.key);
                return Ok(lease_active);
            }
        }

        Ok(false)
    }

    async fn is_locked(&self, key: &str) -> Result<bool> {
        // Read lock is sufficient: check if key exists and is not expired.
        // Stale expired entries are cleaned up lazily by
        // try_acquire/extend/release.
        let locks = self.locks.read().await;
        let now = tokio::time::Instant::now();
        Ok(locks.get(key).is_some_and(|entry| entry.expires_at > now))
    }

    async fn cleanup_expired_locks(&self) -> Result<usize> {
        Ok(self.cleanup_expired().await)
    }

    #[cfg(test)]
    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
        self
    }
}

/// Interval between lease renewals, as a fraction of the lease TTL.
///
/// A renewal every third of the TTL tolerates two consecutive missed or
/// failing renewal attempts before the lease can expire, while keeping the
/// renewal traffic at three extensions per TTL.
const LEASE_RENEWAL_INTERVAL_FRACTION: u32 = 3;

/// Keeps a held lock lease alive for the whole duration of a critical section.
///
/// Failure class (issue #550): the room-cap checks hold their coordination
/// locks across storage calls. If one hold stalls longer than the lease TTL —
/// slow or failing storage, a stalled task — the lease expires mid-hold and a
/// second creation can acquire the same key, read the same count, and insert.
/// Both holders then passed the cap check, so the ceiling overshoots.
///
/// The guard spawns one renewal task per held lock. The task extends the lease
/// every [`LEASE_RENEWAL_INTERVAL_FRACTION`]-th of the TTL until the guard is
/// dropped. A lost lease (`extend` reports `Ok(false)`: expired and reclaimed,
/// or re-keyed) is fail-visible — an error log plus a
/// `signal_fish_distributed_lock_renewal_failures_total` increment — and ends
/// the renewal; holding the critical section itself is still safe, it only
/// degrades that one cap check to best-effort, which is exactly the state the
/// metrics now report instead of hiding.
///
/// Dropping the guard aborts the renewal task; the underlying release stays
/// with the caller, which already accounts for stale and failed releases.
pub struct LeaseRenewalGuard {
    renewal: tokio::task::JoinHandle<()>,
    /// Caller-owned copy; the task renews its own clone.
    handle_ref: LockHandle,
}

impl LeaseRenewalGuard {
    /// The still-owned lock handle, for the caller's accounted release.
    pub fn handle(&self) -> &LockHandle {
        // The renewal task only reads its own clone; no mutable access exists.
        &self.handle_ref
    }

    /// Cancel the renewal task without consuming the guard.
    ///
    /// Call this before releasing the lock: a tick firing after the release
    /// would extend an already-surrendered key or report a phantom loss.
    pub fn stop_renewal(&mut self) {
        self.renewal.abort();
    }

    fn spawn(
        lock: Arc<dyn DistributedLock>,
        handle: LockHandle,
        ttl: Duration,
        metrics: Arc<crate::metrics::ServerMetrics>,
    ) -> Self {
        let interval = ttl / LEASE_RENEWAL_INTERVAL_FRACTION;
        let task_handle = handle.clone();
        let renewal = tokio::spawn(async move {
            let mut ticks = tokio::time::interval(interval);
            ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
            ticks.tick().await; // `interval` fires its first tick immediately; skip it.
            loop {
                ticks.tick().await;
                match lock.extend(&task_handle, ttl).await {
                    Ok(true) => {}
                    Ok(false) => {
                        metrics.increment_distributed_lock_renewal_failures();
                        tracing::error!(
                            key = %task_handle.key,
                            "Distributed-lock lease expired or was stolen mid-hold; \
                             the critical section continues without coordination"
                        );
                        break;
                    }
                    Err(error) => {
                        // The lease may still be alive; keep retrying until a
                        // successful extension or a reported loss.
                        tracing::warn!(key = %task_handle.key, %error, "Failed to renew distributed-lock lease");
                    }
                }
            }
        });
        Self {
            renewal,
            handle_ref: handle,
        }
    }
}

impl Drop for LeaseRenewalGuard {
    fn drop(&mut self) {
        self.renewal.abort();
    }
}

/// Acquire-with-renewal shorthand: wraps a freshly acquired handle in a
/// [`LeaseRenewalGuard`] so the lease cannot expire mid-hold (issue #550).
pub fn keep_lease_renewed(
    lock: Arc<dyn DistributedLock>,
    handle: LockHandle,
    ttl: Duration,
    metrics: Arc<crate::metrics::ServerMetrics>,
) -> LeaseRenewalGuard {
    LeaseRenewalGuard::spawn(lock, handle, ttl, metrics)
}

/// Message with sequence number for deduplication
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequencedMessage {
    pub sequence_id: u64,
    pub instance_id: Uuid,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub message: crate::protocol::ServerMessage,
    pub room_id: Option<crate::protocol::RoomId>,
    pub target_player: Option<crate::protocol::PlayerId>,
    #[serde(default)]
    pub excluded_players: Vec<crate::protocol::PlayerId>,
}

impl SequencedMessage {
    pub fn new(
        sequence_id: u64,
        instance_id: Uuid,
        message: crate::protocol::ServerMessage,
        room_id: Option<crate::protocol::RoomId>,
        target_player: Option<crate::protocol::PlayerId>,
        excluded_players: Vec<crate::protocol::PlayerId>,
    ) -> Self {
        Self {
            sequence_id,
            instance_id,
            // Wall clock (durable record): wire/diagnostic message stamp.
            timestamp: chrono::Utc::now(),
            message,
            room_id,
            target_player,
            excluded_players,
        }
    }
}

/// Circuit breaker states
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

/// Consolidated mutable state for the circuit breaker, protected by a single mutex
/// to prevent deadlocks and ensure atomic state transitions.
struct CircuitBreakerInner {
    state: CircuitState,
    failure_count: u32,
    /// Monotonic timestamp of the transition into [`CircuitState::Open`].
    ///
    /// Wall-clock steps (NTP correction, manual clock change, host
    /// suspend/resume) must not shorten or extend the open window; the same
    /// discipline is pinned for reconnect windows and client pings.
    opened_at_monotonic: Option<tokio::time::Instant>,
    /// Bumped by [`CircuitBreaker::reset`] so outcomes from calls admitted
    /// before the reset cannot mutate the freshly cleared state.
    epoch: u64,
}

/// Circuit breaker extension seam for fallible coordination operations.
///
/// Contract (issue #403):
/// - In the [`CircuitState::Closed`] state, only *consecutive* failures count
///   toward `failure_threshold`; any success resets the streak.
/// - After the open timeout elapses, exactly one call is admitted as a probe.
///   Concurrent calls are rejected while a probe is outstanding.
/// - A successful probe closes the circuit; a failed probe reopens it.
///
/// [`Self::reset`] invalidates every call admitted before it: outcomes from
/// such calls are discarded rather than applied to the cleared state, so a
/// stale probe failing after a reset does not reopen the circuit.
///
/// Concurrency notes: exactly one probe is admitted at a time and only that
/// probe resolves the half-open state, so its outcome stays authoritative even
/// if a straggler call admitted while [`CircuitState::Closed`] resolves while
/// the probe runs. A closed-state straggler success resets the streak only
/// when the circuit is still closed; a closed-state straggler failure still
/// counts toward the streak but cannot steal the half-open transition.
pub struct CircuitBreaker {
    inner: Arc<Mutex<CircuitBreakerInner>>,
    /// Tracks whether a half-open probe is currently admitted. Stored outside
    /// the mutex so an RAII guard can release it synchronously when the probe
    /// future is dropped or cancelled without resolving.
    probe_in_flight: AtomicBool,
    failure_threshold: u32,
    timeout: Duration,
}

impl CircuitBreaker {
    pub fn new(failure_threshold: u32, timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CircuitBreakerInner {
                state: CircuitState::Closed,
                failure_count: 0,
                opened_at_monotonic: None,
                epoch: 0,
            })),
            probe_in_flight: AtomicBool::new(false),
            failure_threshold,
            timeout,
        }
    }

    pub async fn call<F, T, E>(&self, operation: F) -> Result<T, E>
    where
        F: std::future::Future<Output = Result<T, E>>,
        E: std::fmt::Debug + From<anyhow::Error>,
    {
        // Check circuit state (single lock acquisition for all state reads/transitions)
        let probing;
        let admitted_epoch;
        {
            let mut inner = self.inner.lock().await;
            admitted_epoch = inner.epoch;
            match inner.state {
                CircuitState::Open => {
                    if let Some(opened_at) = inner.opened_at_monotonic {
                        if opened_at.elapsed() < self.timeout {
                            return Err(E::from(anyhow::anyhow!("Circuit breaker is open")));
                        }
                    }
                    // Transition to half-open atomically
                    inner.state = CircuitState::HalfOpen;
                }
                CircuitState::HalfOpen | CircuitState::Closed => {
                    // HalfOpen falls through to single-probe admission below;
                    // Closed allows normal operation.
                }
            }

            if inner.state == CircuitState::HalfOpen && !self.acquire_probe_slot() {
                return Err(E::from(anyhow::anyhow!(
                    "Circuit breaker is half-open: a probe is already in flight"
                )));
            }
            probing = inner.state == CircuitState::HalfOpen;
        }

        // Release the probe slot on every exit path, including cancellation of
        // this future while the operation is still pending. A free slot in the
        // half-open state lets a later call be admitted as a fresh probe, so an
        // abandoned probe can never wedge the breaker closed.
        struct ProbeSlotGuard<'a> {
            slot: &'a AtomicBool,
        }

        impl Drop for ProbeSlotGuard<'_> {
            fn drop(&mut self) {
                self.slot.store(false, Ordering::Release);
            }
        }

        let _probe_slot_guard = probing.then(|| ProbeSlotGuard {
            slot: &self.probe_in_flight,
        });

        // Execute operation (lock is NOT held during the operation itself)
        match operation.await {
            Ok(result) => {
                let mut inner = self.inner.lock().await;
                if inner.epoch != admitted_epoch {
                    // A reset() invalidated this call's admission: its outcome
                    // belongs to a superseded era and must not mutate the
                    // freshly cleared state.
                    return Ok(result);
                }
                if probing {
                    // The probe outcome is authoritative: close the circuit
                    // even if a concurrent straggler failure reopened it while
                    // this probe ran.
                    inner.state = CircuitState::Closed;
                    inner.failure_count = 0;
                } else if inner.state == CircuitState::Closed {
                    // A closed-state success keeps the failure streak honest:
                    // only consecutive failures may open the circuit.
                    inner.failure_count = 0;
                }
                // Non-probe successes never resolve Open or HalfOpen: only the
                // outstanding probe owns those transitions.
                Ok(result)
            }
            Err(error) => {
                let mut inner = self.inner.lock().await;
                if inner.epoch != admitted_epoch {
                    // A reset() invalidated this call's admission; see the
                    // success arm above.
                    return Err(error);
                }
                inner.failure_count = inner.failure_count.saturating_add(1);

                // A straggler failure from the closed epoch counts toward the
                // streak but must not resolve someone else's half-open probe.
                let straggler_failure_during_probe =
                    inner.state == CircuitState::HalfOpen && !probing;
                if !straggler_failure_during_probe && inner.failure_count >= self.failure_threshold
                {
                    // Stamp the open-window start only on a transition INTO
                    // Open. Failures observed while already open (late closed-
                    // epoch stragglers) must not restart the window: each one
                    // would otherwise push the half-open probe further out and
                    // starve recovery.
                    if inner.state != CircuitState::Open {
                        inner.opened_at_monotonic = Some(tokio::time::Instant::now());
                    }
                    inner.state = CircuitState::Open;
                }

                Err(error)
            }
        }
    }

    /// Admit at most one half-open probe. Returns `false` when another probe
    /// is already outstanding.
    fn acquire_probe_slot(&self) -> bool {
        self.probe_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub async fn get_state(&self) -> CircuitState {
        self.inner.lock().await.state.clone()
    }

    /// Clear the breaker state and invalidate every call admitted so far.
    ///
    /// Outcomes from in-flight calls are discarded rather than applied to the
    /// cleared state: a late straggler failure after an administrative reset
    /// must not reopen the circuit. The probe slot is intentionally left to the
    /// outstanding probe's own guard: clearing it here could free the slot for
    /// a second concurrent probe while the first still runs.
    pub async fn reset(&self) {
        let mut inner = self.inner.lock().await;
        inner.state = CircuitState::Closed;
        inner.failure_count = 0;
        inner.opened_at_monotonic = None;
        inner.epoch = inner.epoch.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{DistributedLock, InMemoryDistributedLock};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    fn test_metrics() -> Arc<crate::metrics::ServerMetrics> {
        Arc::new(crate::metrics::ServerMetrics::new())
    }

    /// Give freshly spawned tasks their polls before advancing the paused
    /// clock, so the renewal interval registers its ticks at the intended
    /// instants (a task spawned and immediately advanced would first be polled
    /// *during* the advance, shifting every tick one boundary late).
    async fn settle_tasks() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    /// Issue #550: while a critical section holds the guard, the renewal task
    /// must keep extending the lease — the lease must still be held many times
    /// the TTL later — and expiry must resume once renewal stops.
    ///
    /// Time advances one renewal interval per step with yields in between, so
    /// every tick is processed deterministically regardless of how the runtime
    /// interleaves the spawned task.
    #[tokio::test(start_paused = true)]
    async fn lease_renewal_keeps_the_lease_alive_across_the_original_ttl() {
        let lock = Arc::new(InMemoryDistributedLock::new());
        let metrics = test_metrics();
        let key = "room_join:game:RENEW";
        let ttl = Duration::from_secs(1);
        let handle = lock
            .try_acquire(key, ttl)
            .await
            .expect("acquisition should not fail")
            .expect("free key should be acquired");
        let mut guard = super::keep_lease_renewed(lock.clone(), handle, ttl, metrics.clone());
        settle_tasks().await;

        // Twelve intervals = four TTLs. Without renewal the lease would die
        // after the first one.
        for _ in 0..12 {
            tokio::time::advance(ttl / 3).await;
            settle_tasks().await;
        }
        assert!(
            lock.is_locked(key)
                .await
                .expect("is_locked should not fail"),
            "a renewed lease must stay held far beyond its original TTL"
        );

        guard.stop_renewal();
        tokio::time::advance(ttl * 2).await;
        settle_tasks().await;
        assert!(
            !lock
                .is_locked(key)
                .await
                .expect("is_locked should not fail"),
            "the lease must expire once renewal stops"
        );
        assert_eq!(
            metrics.snapshot().await.distributed_lock.renewal_failures,
            0,
            "a healthy renewal must never report a lost lease"
        );
    }

    /// A test double whose first `extend` succeeds and every later one reports
    /// the lease as lost (`Ok(false)`), the way a stolen or expired lease looks.
    struct LeasesStolenAfterFirstExtension {
        inner: InMemoryDistributedLock,
        extend_calls: AtomicUsize,
    }

    impl LeasesStolenAfterFirstExtension {
        fn new() -> Self {
            Self {
                inner: InMemoryDistributedLock::new(),
                extend_calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl DistributedLock for LeasesStolenAfterFirstExtension {
        async fn acquire(&self, key: &str, ttl: Duration) -> anyhow::Result<super::LockHandle> {
            self.inner.acquire(key, ttl).await
        }

        async fn try_acquire(
            &self,
            key: &str,
            ttl: Duration,
        ) -> anyhow::Result<Option<super::LockHandle>> {
            self.inner.try_acquire(key, ttl).await
        }

        async fn extend(
            &self,
            _handle: &super::LockHandle,
            _ttl: Duration,
        ) -> anyhow::Result<bool> {
            let call = self.extend_calls.fetch_add(1, Ordering::SeqCst);
            Ok(call == 0)
        }

        async fn release(&self, handle: &super::LockHandle) -> anyhow::Result<bool> {
            self.inner.release(handle).await
        }

        async fn is_locked(&self, key: &str) -> anyhow::Result<bool> {
            self.inner.is_locked(key).await
        }

        async fn cleanup_expired_locks(&self) -> anyhow::Result<usize> {
            self.inner.cleanup_expired_locks().await
        }

        #[cfg(test)]
        fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
            self
        }
    }

    /// Issue #550: a lost lease must be fail-visible (one counted renewal
    /// failure) and the renewal task must stop instead of spinning forever.
    #[tokio::test(start_paused = true)]
    async fn lease_renewal_stops_and_counts_after_the_lease_is_lost() {
        let lock = Arc::new(LeasesStolenAfterFirstExtension::new());
        let metrics = test_metrics();
        let ttl = Duration::from_millis(300);
        let handle = lock
            .try_acquire("server_room_cap", ttl)
            .await
            .expect("acquisition should not fail")
            .expect("free key should be acquired");
        let _guard = super::keep_lease_renewed(lock.clone(), handle, ttl, metrics.clone());
        settle_tasks().await;

        // Tick one: the first extension succeeds. Tick two: the lease is
        // reported lost, which must count once and end the task. Advance one
        // interval per step until both calls have happened.
        for _ in 0..8 {
            if lock.extend_calls.load(Ordering::SeqCst) >= 2 {
                break;
            }
            tokio::time::advance(ttl / 3).await;
            settle_tasks().await;
        }
        assert_eq!(
            lock.extend_calls.load(Ordering::SeqCst),
            2,
            "the first extension succeeds and the second reports the loss"
        );
        assert_eq!(
            metrics.snapshot().await.distributed_lock.renewal_failures,
            1,
            "a lost lease must be fail-visible through the renewal-failure metric"
        );

        // The ended task must not keep extending.
        tokio::time::advance(Duration::from_secs(5)).await;
        settle_tasks().await;
        assert_eq!(
            lock.extend_calls.load(Ordering::SeqCst),
            2,
            "renewal must stop after the lease is reported lost"
        );
    }

    /// A failing (`Err`) renewal must not end the task: the lease may still be
    /// alive, so the next tick retries and can extend again.
    #[tokio::test(start_paused = true)]
    async fn lease_renewal_retries_through_extension_errors() {
        struct FailsOnceThenExtends {
            inner: InMemoryDistributedLock,
            extend_calls: AtomicUsize,
        }

        #[async_trait::async_trait]
        impl DistributedLock for FailsOnceThenExtends {
            async fn acquire(&self, key: &str, ttl: Duration) -> anyhow::Result<super::LockHandle> {
                self.inner.acquire(key, ttl).await
            }

            async fn try_acquire(
                &self,
                key: &str,
                ttl: Duration,
            ) -> anyhow::Result<Option<super::LockHandle>> {
                self.inner.try_acquire(key, ttl).await
            }

            async fn extend(
                &self,
                _handle: &super::LockHandle,
                _ttl: Duration,
            ) -> anyhow::Result<bool> {
                let call = self.extend_calls.fetch_add(1, Ordering::SeqCst);
                if call == 0 {
                    anyhow::bail!("injected extension failure");
                }
                Ok(true)
            }

            async fn release(&self, handle: &super::LockHandle) -> anyhow::Result<bool> {
                self.inner.release(handle).await
            }

            async fn is_locked(&self, key: &str) -> anyhow::Result<bool> {
                self.inner.is_locked(key).await
            }

            async fn cleanup_expired_locks(&self) -> anyhow::Result<usize> {
                self.inner.cleanup_expired_locks().await
            }

            #[cfg(test)]
            fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
                self
            }
        }

        let lock = Arc::new(FailsOnceThenExtends {
            inner: InMemoryDistributedLock::new(),
            extend_calls: AtomicUsize::new(0),
        });
        let metrics = test_metrics();
        let key = "game_room_cap:game";
        let ttl = Duration::from_secs(1);
        let handle = lock
            .try_acquire(key, ttl)
            .await
            .expect("acquisition should not fail")
            .expect("free key should be acquired");
        let mut guard = super::keep_lease_renewed(lock.clone(), handle, ttl, metrics.clone());
        settle_tasks().await;

        // Tick one fails; the following ticks must still run and succeed.
        for _ in 0..8 {
            if lock.extend_calls.load(Ordering::SeqCst) >= 3 {
                break;
            }
            tokio::time::advance(ttl / 3).await;
            settle_tasks().await;
        }
        assert!(
            lock.extend_calls.load(Ordering::SeqCst) >= 3,
            "an Err extension must not end the renewal task"
        );
        assert_eq!(
            metrics.snapshot().await.distributed_lock.renewal_failures,
            0,
            "an Err extension is not a lost lease and must not count as one"
        );

        guard.stop_renewal();
        let calls_before = lock.extend_calls.load(Ordering::SeqCst);
        tokio::time::advance(ttl * 2).await;
        settle_tasks().await;
        assert_eq!(
            lock.extend_calls.load(Ordering::SeqCst),
            calls_before,
            "stopping the guard must end the renewal task"
        );
    }

    /// Issue #414: `acquire` retries while the key it wants is held, and its
    /// whole scheduled backoff must fit strictly inside the lease TTL itself
    /// (the join-path locks use 10 s): a waiter still backing off *after* its
    /// key may have expired would give up on — or race — an already-free or
    /// re-taken resource.
    #[test]
    fn lock_acquire_backoff_cannot_outlive_the_lease_it_waits_for() {
        const SHORTEST_PRODUCTION_LOCK_TTL: Duration = Duration::from_secs(10);

        let effective = crate::retry::RetryConfig::persistent()
            .clamped_to_total_backoff(SHORTEST_PRODUCTION_LOCK_TTL);
        assert!(
            effective.worst_case_total_backoff() < SHORTEST_PRODUCTION_LOCK_TTL,
            "acquire backoff ({:?}) must stay below the shortest production lock TTL \
             ({SHORTEST_PRODUCTION_LOCK_TTL:?})",
            effective.worst_case_total_backoff()
        );
        assert!(
            effective.max_attempts >= 2,
            "trimming must keep meaningful retries instead of a single probe"
        );
        assert_eq!(
            effective.initial_delay,
            crate::retry::RetryConfig::persistent().initial_delay
        );
        assert_eq!(
            effective.max_delay,
            crate::retry::RetryConfig::persistent().max_delay
        );
    }

    #[tokio::test(start_paused = true)]
    async fn lock_lease_expiry_runs_on_the_monotonic_clock() {
        let lock = InMemoryDistributedLock::new();
        let key = "room_join:game:CODE1";
        let first = lock
            .try_acquire(key, Duration::from_secs(10))
            .await
            .expect("acquisition should not fail")
            .expect("free key should be acquired");

        tokio::time::advance(Duration::from_secs(11)).await;
        assert!(
            !lock
                .is_locked(key)
                .await
                .expect("is_locked should not fail"),
            "the lease must expire with elapsed monotonic time, not wall-clock time"
        );
        let second = lock
            .try_acquire(key, Duration::from_secs(10))
            .await
            .expect("re-acquisition should not fail")
            .expect("expired lease must be reclaimable");
        assert_ne!(first.token, second.token);
    }

    #[tokio::test]
    async fn try_acquire_starts_ttl_after_internal_lock_contention() {
        let lock = InMemoryDistributedLock::new();
        let guard = lock.locks.write().await;
        let ttl = Duration::from_secs(1);
        let mut acquisition = Box::pin(lock.try_acquire("contended-acquire", ttl));

        tokio::select! {
            result = &mut acquisition => {
                panic!("acquisition unexpectedly completed while the internal lock was held: {result:?}");
            }
            () = tokio::time::sleep(Duration::from_millis(20)) => {}
        }

        let lease_must_start_at = chrono::Utc::now();
        drop(guard);
        let handle = acquisition
            .await
            .expect("contended acquisition should not fail")
            .expect("contended acquisition should obtain the free key");

        assert!(
            handle.acquired_at >= lease_must_start_at,
            "a successful acquisition must start after internal contention ends"
        );
    }

    #[tokio::test]
    async fn extend_starts_ttl_after_internal_lock_contention() {
        let lock = InMemoryDistributedLock::new();
        let handle = lock
            .try_acquire("contended-extension", Duration::from_secs(60))
            .await
            .expect("initial acquisition should not fail")
            .expect("initial acquisition should obtain the free key");
        let guard = lock.locks.write().await;
        let ttl = Duration::from_secs(1);
        let mut extension = Box::pin(lock.extend(&handle, ttl));

        tokio::select! {
            result = &mut extension => {
                panic!("extension unexpectedly completed while the internal lock was held: {result:?}");
            }
            () = tokio::time::sleep(Duration::from_millis(20)) => {}
        }

        let extension_must_start_at = tokio::time::Instant::now();
        drop(guard);
        assert!(
            extension
                .await
                .expect("contended extension should not fail"),
            "the current owner should retain the lock while extending"
        );
        let expires_at = lock
            .locks
            .read()
            .await
            .get(&handle.key)
            .expect("extended lock should remain stored")
            .expires_at;
        let expected_not_before = extension_must_start_at
            .checked_add(ttl)
            .expect("test deadline remains representable");
        assert!(
            expires_at >= expected_not_before,
            "a successful extension must start after internal contention ends"
        );
    }
}
