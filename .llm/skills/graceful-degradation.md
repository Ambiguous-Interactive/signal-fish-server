# Skill: Graceful Degradation & Reliability

<!-- trigger: degradation, circuit-breaker, health-check, failover, resilience, availability, deployment, drain, shutdown | Reliability patterns for real-time game services | Core -->

**Trigger**: When implementing, reviewing, or hardening health checks, graceful shutdown, circuit breakers, deployment strategies, or any reliability-critical code path for the signaling server.

---

## When to Use
- Adding or modifying health check, graceful shutdown, or connection draining logic
- Wrapping dependent services (database, Redis, auth) in circuit breakers
- Configuring Kubernetes deployments, PDBs, or rolling updates
- Adding feature flags, gradual rollout, or connection lifecycle management

## When NOT to Use
- Rate limiting or DDoS prevention (see [ddos-and-rate-limiting](./ddos-and-rate-limiting.md))
- WebSocket protocol design unrelated to availability (see [websocket-protocol-patterns](./websocket-protocol-patterns.md))
- General error handling without a reliability motivation (see [error-handling-guide](./error-handling-guide.md))

## Rationalizations to Reject

| Excuse | Why It's Wrong | Required Action |
|--------|---------------|-----------------|
| "We'll add health checks later" | Kubernetes kills pods without readiness probes on any restart. | Ship `/healthz`, `/readyz`, `/startupz` before first deployment. |
| "Connections will just reconnect" | Abrupt termination drops all players mid-game. Reconnection storms amplify load. | Implement SIGTERM handling with a 30s drain period. |
| "Circuit breakers add complexity" | A single hung DB call blocks tokio and cascades to all connections. | Wrap every external dependency. Fail fast > hang forever. |
| "Our service is stateless" | WebSocket connections are inherently stateful. Killing a pod kills sessions. | Treat every pod as stateful. Use PDBs and rolling updates. |
| "Feature flags are over-engineering" | A bad deploy to 100% takes down the entire service. | Gate new features behind flags. Roll out 1% → 10% → 50% → 100%. |

---

## TL;DR

- **Degrade progressively** — shed features before users: stop new rooms → stop joins → reconnections only → reject all.
- **Three health endpoints** — `/healthz` (alive), `/readyz` (can serve), `/startupz` (initialized). Never combine.
- **Circuit-break every dependency** — database, Redis, external auth. Open after 5 failures, half-open after 30s.
- **Drain before dying** — on SIGTERM, stop accepting new connections, drain existing for 30s.
- **Deploy like it's stateful** — `maxUnavailable: 0`, PDB, `terminationGracePeriodSeconds ≥ drain`.

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
            _             => ServiceLevel::Overloaded,
        };
        self.level.store(level as u8, Ordering::Relaxed);
    }
}
```rust

### Progressive Feature Shedding

```rust
// ❌ Bad — binary on/off, no middle ground
if server_overloaded() { return Err(StatusCode::SERVICE_UNAVAILABLE); }

// ✅ Good — progressive shedding based on service level
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
```rust

### Usage for Dependent Services (DB, Redis, Auth)

```rust
// ❌ Bad — unbounded wait on a hung database
let user = sqlx::query_as::<_, User>("SELECT ...").fetch_one(&pool).await?;

// ✅ Good — circuit breaker + timeout
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

## 3. Health Check Design

Three separate endpoints — never combine them:

```rust
async fn liveness() -> StatusCode {
    StatusCode::OK  // Process alive — no dependency checks
}

async fn readiness(State(state): State<Arc<AppState>>) -> StatusCode {
    let db_ok = state.db_pool.acquire().await.is_ok();
    let capacity_ok = state.health.current() != ServiceLevel::Overloaded;
    if db_ok && capacity_ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE }
}

async fn startup(State(state): State<Arc<AppState>>) -> StatusCode {
    if state.initialized.load(Ordering::Relaxed) { StatusCode::OK }
    else { StatusCode::SERVICE_UNAVAILABLE }
}

fn health_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/healthz", get(liveness))
        .route("/readyz", get(readiness))
        .route("/startupz", get(startup))
}
```text

### Kubernetes Probes

```yaml
livenessProbe:  { httpGet: { path: /healthz, port: 3536 }, periodSeconds: 10, failureThreshold: 3 }
readinessProbe: { httpGet: { path: /readyz,  port: 3536 }, periodSeconds: 5,  failureThreshold: 2 }
startupProbe:   { httpGet: { path: /startupz, port: 3536 }, periodSeconds: 3, failureThreshold: 20 }
```

---

## 4. Graceful Shutdown with Connection Draining

```rust
use tokio::sync::watch;
const DRAIN_PERIOD: Duration = Duration::from_secs(30);

async fn shutdown_signal() {
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .unwrap().recv().await;
    };
    tokio::select! { _ = tokio::signal::ctrl_c() => {} _ = terminate => {} }
}

// ❌ Bad — abrupt shutdown drops all connections
axum::serve(listener, app).await.unwrap();

// ✅ Good — signal propagation + drain period
async fn serve_with_graceful_shutdown(app: Router, shutdown_tx: watch::Sender<bool>) {
    let listener = TcpListener::bind("0.0.0.0:3536").await.unwrap();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            let _ = shutdown_tx.send(true);
            tracing::info!("Draining for {}s", DRAIN_PERIOD.as_secs());
            tokio::time::sleep(DRAIN_PERIOD).await;
        }).await.unwrap();
}
```rust

### Connection Handler with Shutdown Awareness

```rust
async fn handle_socket(mut socket: WebSocket, mut shutdown_rx: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(msg)) => handle_message(msg).await,
                _ => break,
            },
            _ = shutdown_rx.changed() => {
                // CloseFrame from axum::extract::ws::{CloseFrame, Message}
                let _ = socket.send(Message::Close(Some(CloseFrame {
                    code: 1001, reason: "server shutting down".into(),
                }))).await;
                break;
            }
        }
    }
}
```

---

## 5. Deployment Safety for Stateful Services

```yaml
apiVersion: policy/v1
kind: PodDisruptionBudget
metadata: { name: signaling-pdb }
spec:
  maxUnavailable: 1
  selector: { matchLabels: { app: matchbox-signaling } }
---
apiVersion: apps/v1
kind: Deployment
spec:
  strategy:
    rollingUpdate: { maxUnavailable: 0, maxSurge: 1 }
  template:
    spec:
      terminationGracePeriodSeconds: 45  # > drain(30s) + preStop(5s) + buffer
      containers:
        - lifecycle:
            preStop: { exec: { command: ["sh", "-c", "sleep 5"] } }
```rust

| Setting | Value | Reason |
|---------|-------|--------|
| `maxUnavailable` | 0 | Active games are on those pods |
| `maxSurge` | 1 | New pod passes readiness before old drains |
| `terminationGracePeriodSeconds` | 45 | 5s preStop + 30s drain + 10s buffer |
| PDB `maxUnavailable` | 1 | Prevents cluster ops from killing multiple pods |

**Blue/green**: deploy green alongside blue, shift new connections via LB weight, wait for blue to drain naturally, then tear down.

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

// ❌ Bad — deploy new matchmaking to everyone at once
new_matchmaking_algorithm(req).await

// ✅ Good — gradual rollout with fallback
if state.flags.is_enabled("new_matchmaking_v2", &req.app_id) {
    match new_matchmaking_algorithm(req.clone()).await {
        Ok(room) => return Ok(room),
        Err(e) => tracing::warn!(error = %e, "v2 failed, falling back"),
    }
}
legacy_matchmaking_algorithm(req).await
```

---

## 7. Connection Management Under Load

```rust
const MAX_CONNECTIONS: usize = 10_000;
const MAX_LIFETIME: Duration = Duration::from_secs(4 * 3600);

async fn handle_bounded_connection(
    mut socket: WebSocket,
    state: Arc<AppState>,
    _permit: tokio::sync::OwnedSemaphorePermit,
) {
    let mut shutdown_rx = state.shutdown_rx.clone();
    let deadline = tokio::time::sleep(MAX_LIFETIME);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(msg)) => handle_message(msg, &state).await,
                _ => break,
            },
            _ = shutdown_rx.changed() => {
                let _ = socket.send(Message::Close(None)).await; break;
            }
            _ = &mut deadline => {
                tracing::info!("Max connection lifetime reached");
                let _ = socket.send(Message::Close(None)).await; break;
            }
        }
    } // _permit dropped → semaphore slot released
}
```rust

### Per-Room Tracking with JoinSet

```rust
struct Room { id: String, tasks: tokio::task::JoinSet<()> }

impl Room {
    fn add_peer(&mut self, socket: WebSocket, peer_id: String) {
        self.tasks.spawn(async move { handle_peer_session(socket, peer_id).await });
    }
    async fn drain(&mut self) {
        while let Some(res) = self.tasks.join_next().await {
            if let Err(e) = res { tracing::warn!(error=%e, room=%self.id, "Peer panicked"); }
        }
    }
}
```

---

## 8. Database Failover

```rust
// ❌ Bad — write failure = total failure
sqlx::query("UPDATE rooms SET state = $1 WHERE id = $2").execute(pool).await?;

// ✅ Good — degrade writes to cache, replay later
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
```rust

### Read Replica Failover

Read replica failover: round-robin across healthy replicas, fall back to writer if all fail:

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
```bash

---

## Agent Checklist

- [ ] Service levels defined with load ratio thresholds (70% / 85% / 95%)
- [ ] Progressive feature shedding: rooms → joins → reconnections → reject all
- [ ] Circuit breakers wrap every external dependency (DB, Redis, auth)
- [ ] `/healthz`, `/readyz`, `/startupz` are separate; readiness checks DB + capacity
- [ ] SIGTERM triggers graceful shutdown with `watch` propagation, drain ≥ 30s
- [ ] `terminationGracePeriodSeconds` exceeds drain; PDB prevents simultaneous disruption
- [ ] Rolling update: `maxUnavailable: 0`, `maxSurge: 1`
- [ ] Feature flags support per-app targeting and percentage rollout
- [ ] Connection semaphore enforces global connection ceiling
- [ ] Absolute connection timeout (4h) prevents resource leaks
- [ ] DB writes degrade to cache when writer circuit opens; room state checkpointed to Redis

## Related Skills

- [ddos-and-rate-limiting](./ddos-and-rate-limiting.md) — Rate limiting, connection caps, load shedding
- [async-rust-best-practices](./async-rust-best-practices.md) — Tokio patterns, `select!`, cancellation safety
- [observability-and-logging](./observability-and-logging.md) — Health metrics, tracing spans, alert thresholds
- [error-handling-guide](./error-handling-guide.md) — Error types, fallible operations, context propagation
- [websocket-protocol-patterns](./websocket-protocol-patterns.md) — WebSocket lifecycle, close frames, heartbeat
