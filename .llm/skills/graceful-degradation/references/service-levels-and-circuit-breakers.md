# Graceful Degradation — Service Levels and Circuit Breakers

---

## When to Use

- Wrapping dependent services (database, Redis, auth) in circuit breakers
- Implementing progressive feature shedding under load
- Adding feature flags for gradual rollout
- Handling database write failures with cache fallback

## When NOT to Use

- Health check endpoints and Kubernetes probes
  (see [Graceful Degradation Deployment](../SKILL.md))
- Rate limiting or DDoS prevention (see [DDoS Rate Limiting Connections](../../ddos-protection/SKILL.md))

## Rationalizations to Reject

| Excuse | Why It's Wrong | Required Action |
|--------|---------------|-----------------|
| "Circuit breakers add complexity" | A single hung DB call blocks tokio and cascades to all connections. | Wrap every external dependency. Fail fast > hang forever. |
| "Feature flags are over-engineering" | A bad deploy to 100% takes down the entire service. | Gate new features behind flags. Roll out 1% → 10% → 50% → 100%. |

---

## TL;DR

- **Degrade progressively** — shed features before users: stop new rooms → stop joins → reconnections only → reject all.
- **Circuit-break every dependency** — database, Redis, external auth. Open after 5 failures, half-open after 30s.
- **Feature flags with consistent hashing** — stable per-app assignment across restarts.
- **DB writes degrade to Redis queue** — replay pending writes when writer recovers.

---

## 1. Service Level Degradation

```rust
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceLevel { Full = 0, Degraded = 1, Critical = 2, Overloaded = 3 }

struct ServerHealth {
    level: AtomicU8,
    active_connections: AtomicU32,
    max_connections: u32,
}

impl ServerHealth {
    fn update_level(&self) {
        let ratio = self.active_connections.load(Ordering::Relaxed) as f64
            / self.max_connections as f64;
        let level = match ratio {
            r if r < 0.70 => ServiceLevel::Full,
            r if r < 0.85 => ServiceLevel::Degraded,
            r if r < 0.95 => ServiceLevel::Critical,
            r if r >= 0.95 => ServiceLevel::Overloaded,
            r if r.is_nan() => ServiceLevel::Overloaded,
        };
        self.level.store(level as u8, Ordering::Relaxed);
    }
}
```

### Progressive Feature Shedding

```rust
// BAD: Binary on/off, no middle ground
if server_overloaded() { return Err(StatusCode::SERVICE_UNAVAILABLE); }

// GOOD: Progressive shedding based on service level
match health.current() {
    ServiceLevel::Full => { /* allow everything */ }
    ServiceLevel::Degraded => {
        if params.action == Action::CreateRoom { return Err(StatusCode::SERVICE_UNAVAILABLE); }
    }
    ServiceLevel::Critical => {
        if !params.is_reconnection() { return Err(StatusCode::SERVICE_UNAVAILABLE); }
    }
    ServiceLevel::Overloaded => { return Err(StatusCode::SERVICE_UNAVAILABLE); }
}
```

---

## 2. Circuit Breaker Pattern

```rust
use std::sync::{Mutex, atomic::{AtomicU8, AtomicU32, Ordering}};
use tokio::time::{Instant, Duration};

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
enum CBState { Closed = 0, Open = 1, HalfOpen = 2 }

struct CircuitBreaker {
    state: AtomicU8,
    failure_count: AtomicU32,
    failure_threshold: u32,       // 5
    recovery_timeout: Duration,   // 30s
    last_failure: Mutex<Option<Instant>>,
}

impl CircuitBreaker {
    fn can_execute(&self) -> bool {
        match self.current_state() {
            CBState::Closed | CBState::HalfOpen => true,
            CBState::Open => {
                let last = self.last_failure.lock().unwrap();
                if last.map_or(false, |t| t.elapsed() >= self.recovery_timeout) {
                    self.state.store(CBState::HalfOpen as u8, Ordering::Relaxed);
                    true // allow one probe request
                } else { false }
            }
        }
    }
    fn record_success(&self) {
        self.failure_count.store(0, Ordering::Relaxed);
        self.state.store(CBState::Closed as u8, Ordering::Relaxed);
    }
    fn record_failure(&self) {
        let count = self.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
        *self.last_failure.lock().unwrap() = Some(Instant::now());
        if count >= self.failure_threshold {
            self.state.store(CBState::Open as u8, Ordering::Relaxed);
        }
    }
}
```

### Usage for Dependent Services (DB, Redis, Auth)

```rust
// BAD: Unbounded wait on a hung database
let user = sqlx::query_as::<_, User>("SELECT ...").fetch_one(&pool).await?;

// GOOD: Circuit breaker + timeout
if !cb.can_execute() { return Err(AppError::ServiceUnavailable("db circuit open")); }
match tokio::time::timeout(Duration::from_secs(5),
    sqlx::query_as::<_, User>("SELECT ...").fetch_one(pool),
).await {
    Ok(Ok(user)) => { cb.record_success(); Ok(user) }
    Ok(Err(e))   => { cb.record_failure(); Err(e.into()) }
    Err(_)       => { cb.record_failure(); Err(AppError::Timeout("database")) }
}
```

---

## 6. Feature Flags for Gradual Rollout

```rust
use std::collections::{HashMap, HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};

struct FeatureFlags { flags: HashMap<String, FlagConfig> }
struct FlagConfig { enabled: bool, rollout_percentage: u8, targeted_apps: HashSet<String> }

impl FeatureFlags {
    fn is_enabled(&self, flag: &str, app_id: &str) -> bool {
        let Some(config) = self.flags.get(flag) else { return false };
        if !config.enabled { return false; }
        if config.targeted_apps.contains(app_id) { return true; }
        // Consistent hash → stable assignment across restarts
        let mut h = DefaultHasher::new();
        flag.hash(&mut h); app_id.hash(&mut h);
        (h.finish() % 100) < config.rollout_percentage as u64
    }
}

// BAD: Deploy new matchmaking to everyone at once
new_matchmaking_algorithm(req).await

// GOOD: Gradual rollout with fallback
if state.flags.is_enabled("new_matchmaking_v2", &req.app_id) {
    match new_matchmaking_algorithm(req.clone()).await {
        Ok(room) => return Ok(room),
        Err(e) => tracing::warn!(error = %e, "v2 failed, falling back"),
    }
}
legacy_matchmaking_algorithm(req).await
```

---

## 8. Database Failover

```rust
// BAD: Write failure = total failure
sqlx::query("UPDATE rooms SET state = $1 WHERE id = $2").execute(pool).await?;

// GOOD: Degrade writes to cache, replay later
async fn update_room_state(
    cluster: &DbCluster, redis: &redis::Client, room: &Room,
) -> Result<(), Error> {
    if cluster.writer_cb.can_execute() {
        match sqlx::query("UPDATE rooms SET state = $1 WHERE id = $2")
            .bind(&room.state).bind(&room.id).execute(&cluster.writer).await
        {
            Ok(_) => { cluster.writer_cb.record_success(); return Ok(()) }
            Err(e) => { cluster.writer_cb.record_failure();
                tracing::warn!(error = %e, "DB write failed, queuing to Redis"); }
        }
    }
    let payload = serde_json::to_string(room)?;
    redis::cmd("RPUSH").arg("pending_writes").arg(&payload)
        .query_async(&mut redis.get_async_connection().await?).await?;
    Ok(())
}
```

### Read Replica Failover

```rust
async fn read<T: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>>(
    &self, sql: &str,
) -> Result<T, AppError> {
    for _ in 0..self.readers.len() {
        let idx = self.reader_idx.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        if let Ok(r) = sqlx::query_as::<_,T>(sql).fetch_one(&self.readers[idx]).await { return Ok(r); }
    }
    Ok(sqlx::query_as::<_, T>(sql).fetch_one(&self.writer).await?)
}
```

### Checkpoint Room State for Restart Recovery

```rust
async fn checkpoint_room(redis: &redis::Client, room: &Room) -> Result<(), Error> {
    let key = format!("room:{}:state", room.id);
    redis::cmd("SET").arg(&key).arg(serde_json::to_vec(room)?)
        .arg("EX").arg(3600)
        .query_async(&mut redis.get_async_connection().await?).await?;
    Ok(())
}

async fn restore_room(redis: &redis::Client, room_id: &str) -> Result<Option<Room>, Error> {
    let data: Option<Vec<u8>> = redis::cmd("GET").arg(format!("room:{}:state", room_id))
        .query_async(&mut redis.get_async_connection().await?).await?;
    Ok(data.and_then(|d| serde_json::from_slice(&d).ok()))
}
```

---

## Agent Checklist

- [ ] Service levels defined with load ratio thresholds (70% / 85% / 95%)
- [ ] Progressive feature shedding: rooms → joins → reconnections → reject all
- [ ] Circuit breakers wrap every external dependency (DB, Redis, auth)
- [ ] Feature flags support per-app targeting and percentage rollout
- [ ] DB writes degrade to cache when writer circuit opens
- [ ] Room state checkpointed to Redis for restart recovery
- [ ] Read replica failover falls back to writer if all replicas fail

---

## Related Skills

- [Graceful Degradation Deployment](../SKILL.md) — Health checks,
  graceful shutdown, Kubernetes deployment
- [DDoS Rate Limiting Connections](../../ddos-protection/SKILL.md) — Rate limiting, connection caps, load shedding
- [Async Rust Best Practices](../../async-rust-best-practices/SKILL.md) — Tokio patterns, `select!`, cancellation safety
- [Observability And Logging](../../observability-and-logging/SKILL.md) — Health metrics, tracing spans, alert thresholds
