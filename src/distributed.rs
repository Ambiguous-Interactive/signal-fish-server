use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
        initial_count - locks.len()
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
        // Use common retry executor with a persistent profile (10 attempts)
        let executor = crate::retry::RetryExecutor::new(crate::retry::RetryConfig::persistent());

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
        let handle = LockHandle::new(key.to_string(), ttl);
        let expires_at = handle.acquired_at + chrono::Duration::from_std(ttl)?;

        // Single write lock acquisition: cleanup expired entries and check/insert atomically
        // to prevent TOCTOU race where another task acquires the same lock between cleanup and insert
        let mut locks = self.locks.write().await;
        let now = chrono::Utc::now();
        locks.retain(|_, entry| entry.expires_at > now);

        if locks.contains_key(key) {
            return Ok(None);
        }

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
        let new_expires_at = chrono::Utc::now() + chrono::Duration::from_std(ttl)?;

        // Single write lock acquisition: cleanup and extend atomically
        let mut locks = self.locks.write().await;
        let now = chrono::Utc::now();
        locks.retain(|_, entry| entry.expires_at > now);

        if let Some(entry) = locks.get_mut(&handle.key) {
            if entry.token == handle.token {
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
    last_failure_time: Option<chrono::DateTime<chrono::Utc>>,
}

/// Circuit breaker extension seam for fallible coordination operations.
pub struct CircuitBreaker {
    inner: Arc<Mutex<CircuitBreakerInner>>,
    failure_threshold: u32,
    timeout: Duration,
}

impl CircuitBreaker {
    pub fn new(failure_threshold: u32, timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CircuitBreakerInner {
                state: CircuitState::Closed,
                failure_count: 0,
                last_failure_time: None,
            })),
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
        {
            let mut inner = self.inner.lock().await;
            match inner.state {
                CircuitState::Open => {
                    if let Some(last_failure_time) = inner.last_failure_time {
                        let elapsed = chrono::Utc::now()
                            .signed_duration_since(last_failure_time)
                            .to_std()
                            .unwrap_or(Duration::ZERO);

                        if elapsed < self.timeout {
                            return Err(E::from(anyhow::anyhow!("Circuit breaker is open")));
                        }
                    }
                    // Transition to half-open atomically
                    inner.state = CircuitState::HalfOpen;
                }
                CircuitState::HalfOpen | CircuitState::Closed => {
                    // Allow limited calls through / Normal operation
                }
            }
        }

        // Execute operation (lock is NOT held during the operation itself)
        match operation.await {
            Ok(result) => {
                let mut inner = self.inner.lock().await;
                if inner.state == CircuitState::HalfOpen {
                    inner.state = CircuitState::Closed;
                    inner.failure_count = 0;
                }
                Ok(result)
            }
            Err(error) => {
                let mut inner = self.inner.lock().await;
                inner.failure_count += 1;

                if inner.failure_count >= self.failure_threshold {
                    inner.state = CircuitState::Open;
                    inner.last_failure_time = Some(chrono::Utc::now());
                }

                Err(error)
            }
        }
    }

    pub async fn get_state(&self) -> CircuitState {
        self.inner.lock().await.state.clone()
    }

    pub async fn reset(&self) {
        let mut inner = self.inner.lock().await;
        inner.state = CircuitState::Closed;
        inner.failure_count = 0;
        inner.last_failure_time = None;
    }
}
