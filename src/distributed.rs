use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
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

    /// Release a lock
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
            acquired_at: chrono::Utc::now(),
            ttl,
        }
    }

    pub fn is_expired(&self) -> bool {
        let elapsed = chrono::Utc::now()
            .signed_duration_since(self.acquired_at)
            .to_std()
            .unwrap_or(Duration::ZERO);
        elapsed > self.ttl
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
    #[allow(dead_code)]
    instance_id: Uuid,
    expires_at: chrono::DateTime<chrono::Utc>,
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
        let now = chrono::Utc::now();
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
        let ttl_delta = chrono::Duration::from_std(ttl)?;

        // Single write lock acquisition: cleanup expired entries and check/insert atomically
        // to prevent TOCTOU races. Start the lease only after this internal
        // contention ends; otherwise a short lease can expire before this
        // method inserts it and returns ownership to its caller.
        let mut locks = self.locks.write().await;
        let now = chrono::Utc::now();
        locks.retain(|_, entry| entry.expires_at > now);

        if locks.contains_key(key) {
            return Ok(None);
        }

        let expires_at = now
            .checked_add_signed(ttl_delta)
            .ok_or_else(|| anyhow::anyhow!("lock TTL exceeds the supported date range"))?;
        let handle = LockHandle {
            key: key.to_string(),
            token: Uuid::new_v4(),
            acquired_at: now,
            ttl,
        };

        locks.insert(
            key.to_string(),
            LockEntry {
                token: handle.token,
                instance_id: Uuid::new_v4(), // Simulate different instances
                expires_at,
            },
        );

        Ok(Some(handle))
    }

    async fn extend(&self, handle: &LockHandle, ttl: Duration) -> Result<bool> {
        let ttl_delta = chrono::Duration::from_std(ttl)?;

        // Single write lock acquisition: cleanup and extend atomically. The
        // requested extension begins when the state can actually be changed,
        // not while this future is still waiting behind internal contention.
        let mut locks = self.locks.write().await;
        let now = chrono::Utc::now();
        locks.retain(|_, entry| entry.expires_at > now);

        if let Some(entry) = locks.get_mut(&handle.key) {
            if entry.token == handle.token {
                let new_expires_at = now
                    .checked_add_signed(ttl_delta)
                    .ok_or_else(|| anyhow::anyhow!("lock TTL exceeds the supported date range"))?;
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
                locks.remove(&handle.key);
                return Ok(true);
            }
        }

        Ok(false)
    }

    async fn is_locked(&self, key: &str) -> Result<bool> {
        // Read lock is sufficient: check if key exists and is not expired.
        // Stale expired entries are cleaned up lazily by try_acquire/extend.
        let locks = self.locks.read().await;
        let now = chrono::Utc::now();
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
    use std::time::Duration;

    /// Issue #414: `acquire` retries while the key it wants is held, and that
    /// whole scheduled backoff must fit strictly inside the lease TTL itself
    /// (the join-path locks use 10 s): a waiter still backing off *after* its
    /// key may have expired would give up on — or race — an already-free or
    /// re-taken resource.
    #[test]
    fn lock_acquire_backoff_cannot_outlive_the_lease_it_waits_for() {
        const SHORTEST_PRODUCTION_LOCK_TTL: Duration = Duration::from_secs(10);
        // The untrimmed persistent profile really does overrun the lease (its
        // worst case is ~22.5 s once jitter pushes delays onto the cap), which
        // is exactly why acquire must clamp.
        let unclamped = crate::retry::RetryConfig::persistent();
        assert!(
            unclamped.worst_case_total_backoff() >= SHORTEST_PRODUCTION_LOCK_TTL,
            "precondition: the raw persistent budget exceeds the shortest production \
             lock TTL; this test guards the clamp"
        );

        let effective = unclamped.clamped_to_total_backoff(SHORTEST_PRODUCTION_LOCK_TTL);
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
        assert_eq!(effective.initial_delay, unclamped.initial_delay);
        assert_eq!(effective.max_delay, unclamped.max_delay);
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

        let extension_must_start_at = chrono::Utc::now();
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
            .checked_add_signed(chrono::Duration::from_std(ttl).expect("test TTL is valid"))
            .expect("test timestamp remains representable");
        assert!(
            expires_at >= expected_not_before,
            "a successful extension must start after internal contention ends"
        );
    }
}
