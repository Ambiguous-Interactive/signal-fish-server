# Skill: Graceful Degradation — Health Checks, Shutdown, and Deployment

<!--
  trigger: health-check, healthz, readyz, graceful-shutdown, drain, deployment, kubernetes, pdb, rolling-update, sigterm
  | Health endpoints, graceful shutdown, connection draining, and Kubernetes deployment safety
  | Core
-->

**Trigger**: When implementing health check endpoints, graceful shutdown, connection draining,
or configuring Kubernetes deployment strategies for the signaling server.

---

## When to Use

- Adding or modifying health check endpoints (`/healthz`, `/readyz`, `/startupz`)
- Implementing SIGTERM handling with connection draining
- Configuring Kubernetes PDBs or rolling update strategies
- Managing connection lifecycle under load with semaphores and JoinSets

## When NOT to Use

- Service level degradation or circuit breakers
  (see [graceful-degradation-service-levels](./graceful-degradation-service-levels.md))
- Rate limiting or DDoS prevention (see [ddos-rate-limiting-connections](./ddos-rate-limiting-connections.md))
- WebSocket protocol design unrelated to availability
  (see [WebSocket-protocol-patterns](./websocket-protocol-patterns.md))

## Rationalizations to Reject

| Excuse | Why It's Wrong | Required Action |
|--------|---------------|-----------------|
| "We'll add health checks later" | Kubernetes kills pods without readiness probes on any restart. | Ship `/healthz`, `/readyz`, `/startupz` before first deployment. |
| "Connections will just reconnect" | Abrupt termination drops all players mid-game. Reconnection storms amplify load. | Implement SIGTERM handling with a 30s drain period. |
| "Our service is stateless" | WebSocket connections are inherently stateful. Killing a pod kills sessions. | Treat every pod as stateful. Use PDBs and rolling updates. |

---

## TL;DR

- **Three health endpoints** — `/healthz` (alive), `/readyz` (can serve), `/startupz` (initialized). Never combine.
- **Drain before dying** — on SIGTERM, stop accepting new connections, drain existing for 30s.
- **Deploy like it's stateful** — `maxUnavailable: 0`, PDB, `terminationGracePeriodSeconds ≥ drain`.

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
```

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

// BAD: Abrupt shutdown drops all connections
axum::serve(listener, app).await.unwrap();

// GOOD: Signal propagation + drain period
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
```

### Connection Handler with Shutdown Awareness

```rust
async fn handle_socket(mut socket: WebSocket, mut shutdown_rx: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(msg)) => handle_message(msg).await,
                Some(Err(_)) | None => break,
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
```

| Setting | Value | Reason |
|---------|-------|--------|
| `maxUnavailable` | 0 | Active games are on those pods |
| `maxSurge` | 1 | New pod passes readiness before old drains |
| `terminationGracePeriodSeconds` | 45 | 5s preStop + 30s drain + 10s buffer |
| PDB `maxUnavailable` | 1 | Prevents cluster ops from killing multiple pods |

**Blue/green**: deploy green alongside blue, shift new connections via LB weight, wait for blue to drain naturally,
then tear down.

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
                Some(Err(_)) | None => break,
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
```

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

## Agent Checklist

- [ ] `/healthz`, `/readyz`, `/startupz` are separate; readiness checks DB + capacity
- [ ] SIGTERM triggers graceful shutdown with `watch` propagation, drain ≥ 30s
- [ ] `terminationGracePeriodSeconds` exceeds drain; PDB prevents simultaneous disruption
- [ ] Rolling update: `maxUnavailable: 0`, `maxSurge: 1`
- [ ] Connection semaphore enforces global connection ceiling
- [ ] Absolute connection timeout (4h) prevents resource leaks

---

## Related Skills

- [graceful-degradation-service-levels](./graceful-degradation-service-levels.md) — Service levels,
  circuit breakers, feature flags, DB failover
- [async-Rust-best-practices](./async-rust-best-practices.md) — Tokio patterns, `select!`, cancellation safety
- [observability-and-logging](./observability-and-logging.md) — Health metrics, tracing spans, alert thresholds
- [error-handling-guide](./error-handling-guide.md) — Error types, fallible operations, context propagation
- [WebSocket-protocol-patterns](./websocket-protocol-patterns.md) — WebSocket lifecycle, close frames, heartbeat
