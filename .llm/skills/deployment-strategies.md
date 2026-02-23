# Skill: Deployment Strategies for WebSocket Services

<!--
  trigger: kubernetes, k8s, helm, deploy, health check, graceful shutdown,
  WebSocket, PodDisruptionBudget, prometheus, monitoring
  | Kubernetes deployment patterns, health probes, graceful shutdown, and monitoring for WebSocket servers
  | Infrastructure
-->

**Trigger**: When configuring Kubernetes deployments, health checks, graceful shutdown for WebSocket
connections, or setting up container monitoring.

See also:

- [container-Docker](./container-docker.md) — Dockerfile builds, image scanning, CI/CD pipelines
- [container-security](./container-security.md) — Secrets management, security contexts

---

## TL;DR

- Configure `terminationGracePeriodSeconds: 90` for WebSocket connection draining
- Always set `maxUnavailable: 0` on rolling updates (zero downtime)
- Use `PodDisruptionBudget` with `minAvailable: 2` to prevent over-draining
- Set CPU/memory requests **and** limits — without limits, one pod can starve the node
- Use session affinity for WebSocket reconnection stability

---

## Kubernetes Deployment

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: signaling-server
spec:
  replicas: 3
  strategy:
    type: RollingUpdate
    rollingUpdate:
      maxSurge: 1
      maxUnavailable: 0 # Zero downtime — never kill before new is ready
  template:
    metadata:
      labels:
        app: signaling-server
    spec:
      terminationGracePeriodSeconds: 90 # Allow WebSocket connections to drain
      securityContext:
        runAsNonRoot: true
        runAsUser: 65534
        runAsGroup: 65534
        fsGroup: 65534
        seccompProfile:
          type: RuntimeDefault
      containers:
        - name: signaling
          image: ghcr.io/example/signal-fish-server@sha256:abcdef1234567890
          ports:
            - containerPort: 3536
              protocol: TCP
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities:
              drop: [ALL]
          resources:
            requests:
              cpu: 250m
              memory: 64Mi
            limits:
              cpu: "1"
              memory: 256Mi
          livenessProbe:
            httpGet:
              path: /healthz
              port: 3536
            initialDelaySeconds: 5
            periodSeconds: 10
            failureThreshold: 3
          readinessProbe:
            httpGet:
              path: /readyz
              port: 3536
            initialDelaySeconds: 3
            periodSeconds: 5
            failureThreshold: 2
          startupProbe:
            httpGet:
              path: /startupz
              port: 3536
            initialDelaySeconds: 2
            periodSeconds: 3
            failureThreshold: 10
      affinity:
        podAntiAffinity:
          preferredDuringSchedulingIgnoredDuringExecution:
            - weight: 100
              podAffinityTerm:
                labelSelector:
                  matchLabels:
                    app: signaling-server
                topologyKey: kubernetes.io/hostname
```

---

## PodDisruptionBudget

Critical for WebSocket services — prevents draining too many pods at once:

```yaml
apiVersion: policy/v1
kind: PodDisruptionBudget
metadata:
  name: signaling-server-pdb
spec:
  minAvailable: 2 # At least 2 pods always running
  selector:
    matchLabels:
      app: signaling-server
```

---

## Service with Session Affinity

```yaml
apiVersion: v1
kind: Service
metadata:
  name: signaling-server
spec:
  type: ClusterIP
  sessionAffinity: ClientIP # Sticky sessions for WebSocket reconnection
  sessionAffinityConfig:
    clientIP:
      timeoutSeconds: 600
  ports:
    - port: 3536
      targetPort: 3536
      protocol: TCP
  selector:
    app: signaling-server
```

---

## Health Check Endpoints

| Probe | Endpoint | Purpose | Failure Action |
|-------|----------|---------|---------------|
| **Liveness** | `/healthz` | Is the process alive and not deadlocked? | Container restart |
| **Readiness** | `/readyz` | Can the server accept new connections? | Remove from Service endpoints |
| **Startup** | `/startupz` | Has initial setup completed? | Keep waiting (blocks liveness) |

### Axum Route Handlers

```rust
use axum::{Router, Json, extract::State, http::StatusCode, routing::get};
use serde::Serialize;
use std::sync::Arc;

#[derive(Serialize)]
struct HealthResponse { status: &'static str, version: &'static str }

async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok", version: env!("CARGO_PKG_VERSION") })
}

async fn readyz(State(state): State<Arc<AppState>>) -> Result<Json<HealthResponse>, StatusCode> {
    if state.is_shutting_down() { return Err(StatusCode::SERVICE_UNAVAILABLE); }
    if !state.db_pool_healthy().await { return Err(StatusCode::SERVICE_UNAVAILABLE); }
    Ok(Json(HealthResponse { status: "ready", version: env!("CARGO_PKG_VERSION") }))
}

async fn startupz(State(state): State<Arc<AppState>>) -> Result<Json<HealthResponse>, StatusCode> {
    if !state.startup_complete() { return Err(StatusCode::SERVICE_UNAVAILABLE); }
    Ok(Json(HealthResponse { status: "started", version: env!("CARGO_PKG_VERSION") }))
}

pub fn health_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/startupz", get(startupz))
}
```

---

## Graceful Shutdown for Connection Draining

```rust
use tokio::signal;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct AppState { shutting_down: AtomicBool }

impl AppState {
    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Relaxed)
    }
}

async fn shutdown_signal(state: Arc<AppState>) {
    signal::ctrl_c().await.expect("failed to listen for ctrl-c");
    tracing::info!("Shutdown signal received, draining connections...");

    // Mark as shutting down — readyz will return 503
    state.shutting_down.store(true, Ordering::Relaxed);

    // Give the load balancer time to deregister this instance
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    // Wait for active connections to finish (with timeout)
    let drain_timeout = std::time::Duration::from_secs(60);
    tokio::time::timeout(drain_timeout, state.wait_for_connections()).await.ok();
}
```

---

## Monitoring and Logging

### Log to stdout/stderr

```rust
// JSON structured logs to stdout — collected by Fluentd/Loki/CloudWatch
tracing_subscriber::fmt()
    .json()
    .with_target(true)
    .with_env_filter(EnvFilter::from_default_env())
    .init();
```

### Prometheus Metrics Endpoint

```rust
use axum::{Router, routing::get, response::IntoResponse};
use prometheus::{Encoder, TextEncoder, IntGauge, register_int_gauge};
use std::sync::LazyLock;

static ACTIVE_CONNECTIONS: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!("signaling_active_connections", "Number of active WebSocket connections").unwrap()
});

async fn metrics_handler() -> impl IntoResponse {
    let encoder = TextEncoder::new();
    let mut buffer = Vec::new();
    encoder.encode(&prometheus::gather(), &mut buffer).unwrap();
    ([("content-type", "text/plain; version=0.0.4; charset=utf-8")],
     String::from_utf8(buffer).unwrap())
}

pub fn metrics_routes() -> Router {
    Router::new().route("/metrics", get(metrics_handler))
}
```

### Kubernetes ServiceMonitor

```yaml
apiVersion: monitoring.coreos.com/v1
kind: ServiceMonitor
metadata:
  name: signaling-server
spec:
  selector:
    matchLabels:
      app: signaling-server
  endpoints:
    - port: http
      path: /metrics
      interval: 15s
```

---

## Agent Checklist

- [ ] CPU and memory requests/limits set in Kubernetes manifests
- [ ] `PodDisruptionBudget` configured (`minAvailable` >= 2)
- [ ] `terminationGracePeriodSeconds` set for WebSocket draining (60-120s)
- [ ] Rolling update: `maxSurge: 1`, `maxUnavailable: 0`
- [ ] Liveness, readiness, and startup probes configured
- [ ] Session affinity configured for WebSocket services
- [ ] Pod anti-affinity configured to spread across nodes
- [ ] Logs written to stdout/stderr in JSON format
- [ ] Prometheus metrics endpoint exposed at `/metrics`

---

## Related Skills

- [container-Docker](./container-docker.md) — Dockerfile, image scanning, CI/CD pipelines
- [container-security](./container-security.md) — Secrets management, security contexts
- [observability-and-logging](./observability-and-logging.md) — Structured logging, tracing, OpenTelemetry
- [graceful-degradation](./graceful-degradation-deployment.md) — Connection draining, circuit breakers
