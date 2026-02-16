# Skill: Container Security & Deployment

<!-- trigger: docker, container, dockerfile, kubernetes, k8s, deploy, helm, image, registry, ci-cd, pipeline | Container security and deployment patterns for game servers | Infrastructure -->

**Trigger**: When building Docker images, writing Kubernetes manifests, configuring CI/CD pipelines, or hardening container deployments for the signaling server.

---

## When to Use

- Writing or modifying Dockerfiles for the signaling server
- Configuring Kubernetes deployments, services, or ingress for WebSocket workloads
- Setting up CI/CD pipelines with image scanning and security gates
- Managing secrets in containerized environments
- Configuring health checks, probes, or graceful shutdown for containers
- Reviewing container resource limits, capabilities, or security contexts

## When NOT to Use

- Application-level security (see [web-service-security](./web-service-security.md))
- Rust code changes unrelated to deployment (see [rust-idioms-and-patterns](./rust-idioms-and-patterns.md))
- Observability instrumentation inside the application (see [observability-and-logging](./observability-and-logging.md))

## Rationalizations to Reject

| Excuse                               | Why It's Wrong                                                                          | Required Action                                                          |
| ------------------------------------ | --------------------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| "We run as root for simplicity"      | Root in a container means root on the host if the container escapes.                    | Always use `USER nonroot:nonroot`. No exceptions.                        |
| "We'll harden the image later"       | Unhardened images ship to production and stay there. Attack surface compounds.          | Harden from the first Dockerfile. Multi-stage + distroless from day one. |
| "We need shell access for debugging" | A shell in production is a shell for attackers. Debug with logs and exec into sidecars. | Use distroless or scratch. No `/bin/sh` in the production image.         |
| "Latest tag is fine for dev"         | `:latest` is mutable and unreproducible. A deploy can silently change behavior.         | Use immutable tags: sha256 digests or semver + git SHA.                  |
| "Resource limits slow things down"   | Without limits, one pod can starve the node and cascade-fail the cluster.               | Always set CPU/memory requests and limits. Benchmark to right-size.      |

---

## TL;DR

- Use multi-stage builds with distroless runtime images; never ship build tools to production.
- Run as non-root, drop ALL capabilities, use read-only root filesystem.
- Configure PodDisruptionBudget and graceful shutdown for WebSocket connection draining.
- Scan images with Trivy/Grype in CI; block deployment on critical/high CVEs.
- Use immutable image tags (sha256 digests) and never bake secrets into images.

---

## 1. Dockerfile Best Practices

### Multi-Stage Build

```dockerfile
# ---- Builder stage ----
FROM rust:1.83-bookworm AS builder

WORKDIR /app
# Copy manifests first for layer caching
COPY Cargo.toml Cargo.lock ./
COPY build.rs ./

# Create dummy main for dependency caching
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release --locked
RUN rm -rf src

# Copy real source and rebuild
COPY src/ src/
RUN touch src/main.rs && cargo build --release --locked

# ---- Runtime stage ----
FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /app/target/release/matchbox-signaling-server /usr/local/bin/server

EXPOSE 3536
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/server"]
```

### Key Principles

| Principle               | Implementation                                                        |
| ----------------------- | --------------------------------------------------------------------- |
| **Reproducible builds** | `cargo build --locked` — uses exact `Cargo.lock` versions             |
| **Minimal runtime**     | `distroless/cc-debian12:nonroot` — no shell, no package manager       |
| **Non-root user**       | `USER nonroot:nonroot` — UID 65534 by convention                      |
| **Layer cache**         | Copy `Cargo.toml` + `Cargo.lock` before source for dependency caching |
| **Single binary**       | COPY only the compiled binary — no source, no build artifacts         |

### Scratch Alternative (Fully Static)

For a statically linked binary (using musl):

```dockerfile
FROM rust:1.83-bookworm AS builder
RUN rustup target add x86_64-unknown-linux-musl
WORKDIR /app
COPY Cargo.toml Cargo.lock build.rs ./
COPY src/ src/
RUN cargo build --release --locked --target x86_64-unknown-linux-musl

FROM scratch
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/matchbox-signaling-server /server
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
USER 65534:65534
EXPOSE 3536
ENTRYPOINT ["/server"]
```

---

## 2. Container Hardening

### Docker Run Flags

```bash
docker run \
  --read-only \
  --cap-drop=ALL \
  --security-opt=no-new-privileges:true \
  --memory=256m \
  --cpus=1.0 \
  --tmpfs /tmp:rw,noexec,nosuid,size=16m \
  -p 3536:3536 \
  matchbox-signaling-server:latest
```

### Compose Hardening

```yaml
services:
  signaling:
    image: matchbox-signaling-server:sha-abc1234
    read_only: true
    cap_drop: [ALL]
    security_opt: [no-new-privileges:true]
    deploy:
      resources:
        limits:
          cpus: "1.0"
          memory: 256M
        reservations:
          cpus: "0.25"
          memory: 64M
    tmpfs:
      - /tmp:size=16M,noexec,nosuid
    healthcheck:
      test: ["/bin/true"] # distroless — use HTTP probe from orchestrator
      interval: 15s
      timeout: 5s
      retries: 3
```

### Image Scanning in CI

```yaml
# GitHub Actions step
- name: Scan image with Trivy
  uses: aquasecurity/trivy-action@v0.28.0
  with:
    image-ref: matchbox-signaling-server:${{ github.sha }}
    format: table
    exit-code: 1
    severity: CRITICAL,HIGH
    ignore-unfixed: true

- name: Generate SBOM
  uses: anchore/sbom-action@v0.17.0
  with:
    image: matchbox-signaling-server:${{ github.sha }}
    format: spdx-json
    output-file: sbom.spdx.json
```

---

## 3. Kubernetes Configuration for WebSocket Services

### Deployment

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
          image: ghcr.io/example/matchbox-signaling-server@sha256:abcdef1234567890
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
          # Probes — see Section 4
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

### PodDisruptionBudget

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

### Service with Session Affinity

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

## 4. Health Check Endpoints

### What Each Probe Checks

| Probe         | Endpoint    | Purpose                                  | Failure Action                 |
| ------------- | ----------- | ---------------------------------------- | ------------------------------ |
| **Liveness**  | `/healthz`  | Is the process alive and not deadlocked? | Container restart              |
| **Readiness** | `/readyz`   | Can the server accept new connections?   | Remove from Service endpoints  |
| **Startup**   | `/startupz` | Has initial setup completed?             | Keep waiting (blocks liveness) |

### Axum Route Handlers

```rust
use axum::{Router, Json, extract::State, http::StatusCode, routing::get};
use serde::Serialize;
use std::sync::Arc;

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
}

async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn readyz(State(state): State<Arc<AppState>>) -> Result<Json<HealthResponse>, StatusCode> {
    // Check that the server can accept connections
    if state.is_shutting_down() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    // Optionally check downstream dependencies
    if !state.db_pool_healthy().await {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    Ok(Json(HealthResponse { status: "ready", version: env!("CARGO_PKG_VERSION") }))
}

async fn startupz(State(state): State<Arc<AppState>>) -> Result<Json<HealthResponse>, StatusCode> {
    if !state.startup_complete() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    Ok(Json(HealthResponse { status: "started", version: env!("CARGO_PKG_VERSION") }))
}

pub fn health_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/startupz", get(startupz))
}
```

### Graceful Shutdown for Connection Draining

```rust
use tokio::signal;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct AppState {
    shutting_down: AtomicBool,
    // ... other fields
}

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

## 5. CI/CD Pipeline Security

### Image Build and Scan Pipeline (GitHub Actions)

```yaml
jobs:
  build-and-scan:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Cargo audit
        run: cargo audit --deny warnings

      - name: Cargo deny
        run: cargo deny check

      - name: Build image
        run: docker build -t matchbox-signaling-server:${{ github.sha }} .

      - name: Scan with Trivy
        uses: aquasecurity/trivy-action@v0.28.0
        with:
          image-ref: matchbox-signaling-server:${{ github.sha }}
          exit-code: 1
          severity: CRITICAL,HIGH

      - name: Sign image with Cosign
        uses: sigstore/cosign-installer@v3
      - run: cosign sign --yes ghcr.io/example/matchbox-signaling-server@${{ steps.push.outputs.digest }}

      - name: Push with digest tag
        id: push
        run: |
          docker tag matchbox-signaling-server:${{ github.sha }} \
            ghcr.io/example/matchbox-signaling-server:${{ github.sha }}
          docker push ghcr.io/example/matchbox-signaling-server:${{ github.sha }}
```

### Immutable Tags — Never `:latest`

```yaml
# ❌ Mutable tag — unreproducible, can be overwritten
image: matchbox-signaling-server:latest

# ✅ Git SHA tag — traceable to exact commit
image: matchbox-signaling-server:sha-abc1234

# ✅ Digest — cryptographically immutable
image: ghcr.io/example/matchbox-signaling-server@sha256:abcdef1234567890
```

### Deployment Verification

```yaml
- name: Smoke test
  run: |
    kubectl rollout status deployment/signaling-server --timeout=120s
    kubectl exec deploy/signaling-server -- /bin/true || true
    curl -sf http://signaling-server.svc:3536/healthz | jq .status
```

---

## 6. Secrets in Containers

### Never Bake Secrets into Images

```dockerfile
# ❌ Secret in image layer — visible to anyone who pulls the image
ENV JWT_SECRET=my-secret-key
COPY secrets.json /app/secrets.json

# ✅ Secrets injected at runtime via environment or mounted volumes
# (Nothing secret in the Dockerfile)
```

### Kubernetes Secrets as Volumes (Preferred)

```yaml
spec:
  containers:
    - name: signaling
      volumeMounts:
        - name: secrets
          mountPath: /etc/secrets
          readOnly: true
  volumes:
    - name: secrets
      secret:
        secretName: signaling-secrets
        defaultMode: 0400 # Read-only by owner
```

```rust
// Read secret from mounted file
let jwt_secret = secrecy::Secret::new(
    std::fs::read_to_string("/etc/secrets/jwt-secret")
        .context("Failed to read JWT secret from volume mount")?
        .trim()
        .to_string()
);
```

### External Secrets Operator

```yaml
apiVersion: external-secrets.io/v1beta1
kind: ExternalSecret
metadata:
  name: signaling-secrets
spec:
  refreshInterval: 1h
  secretStoreRef:
    name: aws-secrets-manager
    kind: ClusterSecretStore
  target:
    name: signaling-secrets
  data:
    - secretKey: jwt-secret
      remoteRef:
        key: prod/signaling/jwt-secret
```

### Secret Rotation Without Restart

Watch the mounted secret file for changes:

```rust
use notify::{Watcher, RecursiveMode, Event};

async fn watch_secrets(state: Arc<AppState>) -> anyhow::Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let mut watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
        if let Ok(event) = res {
            if event.kind.is_modify() { let _ = tx.blocking_send(()); }
        }
    })?;
    watcher.watch(std::path::Path::new("/etc/secrets"), RecursiveMode::NonRecursive)?;

    while rx.recv().await.is_some() {
        tracing::info!("Secret file changed, reloading...");
        state.reload_secrets().await?;
    }
    Ok(())
}
```

---

## 7. Monitoring and Logging in Containers

### Log to stdout/stderr

Containers must write logs to stdout/stderr for the orchestrator to collect:

```rust
// ✅ JSON structured logs to stdout — collected by Fluentd/Loki/CloudWatch
tracing_subscriber::fmt()
    .json()
    .with_target(true)
    .with_env_filter(EnvFilter::from_default_env())
    .init();
```

Never write to files inside the container — they are ephemeral and lost on restart.

### Prometheus Metrics Endpoint

```rust
use axum::{Router, routing::get, response::IntoResponse};
use prometheus::{Encoder, TextEncoder, IntGauge, IntCounter, register_int_gauge, register_int_counter};
use std::sync::LazyLock;

static ACTIVE_CONNECTIONS: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!("signaling_active_connections", "Number of active WebSocket connections").unwrap()
});

static MESSAGES_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!("signaling_messages_total", "Total signaling messages processed").unwrap()
});

async fn metrics_handler() -> impl IntoResponse {
    let encoder = TextEncoder::new();
    let mut buffer = Vec::new();
    encoder.encode(&prometheus::gather(), &mut buffer).unwrap();
    (
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        String::from_utf8(buffer).unwrap(),
    )
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

### Container Resource Monitoring

Set resource alerts with Prometheus rules:

```yaml
groups:
  - name: signaling-server
    rules:
      - alert: HighMemoryUsage
        expr: container_memory_usage_bytes{container="signaling"} > 200e6
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Signaling server memory usage above 200MB"
      - alert: HighCPUThrottling
        expr: rate(container_cpu_cfs_throttled_seconds_total{container="signaling"}[5m]) > 0.1
        for: 5m
        labels:
          severity: warning
```

---

## 8. CI Native Build Dependencies

### The Problem

When a Cargo feature requires native C libraries (e.g., `kafka` → `rdkafka` → `librdkafka` → `cmake`, `libcurl-dev`, `libssl-dev`), CI workflows using `--all-features` will fail unless those libraries are installed in the runner environment. This is easy to miss because developers often have these packages locally.

### The Solution: Composite Action

All native build dependencies are centralized in a reusable composite action:

```text
.github/actions/install-build-deps/action.yml
```

Every CI job that builds Rust code with `--all-features` **must** use this action:

```yaml
steps:
  - uses: ./.github/actions/install-build-deps
  - run: cargo build --all-features
```

### Keeping Package Lists in Sync

The same native packages must be installed in **two places**:

| Location                                        | Purpose             |
| ----------------------------------------------- | ------------------- |
| `.github/actions/install-build-deps/action.yml` | CI runners (Ubuntu) |
| `Dockerfile` builder stage                      | Docker image builds |

When adding a new native dependency, update **both** files.

### Validation Script

A regression-prevention script checks that workflows using `--all-features` reference the composite action:

```bash
# Run before pushing CI workflow changes
scripts/check-ci-config.sh
```

### When Adding a Cargo Feature with Native Dependencies

1. Add the feature to `Cargo.toml`
2. Add required packages to `.github/actions/install-build-deps/action.yml`
3. Add the same packages to the `Dockerfile` builder stage
4. Verify all workflows using `--all-features` include the composite action step
5. Run `scripts/check-ci-config.sh` to confirm
6. Run `actionlint` to validate workflow syntax

---

## Config Validation & Docker Startup

### The "Auth Defaults" Pitfall

When `default_require_auth()` returns `true` (secure-by-default), Docker containers without a
mounted config file **will crash at startup** unless the Dockerfile explicitly disables auth via
environment variable overrides:

```dockerfile
# Disable auth by default so the container starts without a config file.
# Production deployments should mount a config.json or set auth env vars.
ENV SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH=false
ENV SIGNAL_FISH__SECURITY__REQUIRE_WEBSOCKET_AUTH=false
```

### Prevention Checklist

When changing configuration defaults, **always verify**:

1. **Default `Config` validation** — Does `Config::default()` pass `validate_config_security()`?
   If not, does the Dockerfile set ENV overrides for the failing fields?
2. **Docker smoke test** — CI must retry the health check (not bare `sleep + curl`),
   and must dump `docker logs` on failure for diagnostics.
3. **`check-ci-config.sh`** — Must validate that the Dockerfile contains the required ENV overrides.
4. **Tests** — Must include a `test_docker_default_config_passes_validation` regression test
   that simulates the Docker ENV overrides and asserts validation passes.

### Smoke Test Pattern (CI)

Use a retry loop instead of a bare `sleep`:

```yaml
- name: Smoke test
  run: |
    docker run -d --name test-server -p 3536:3536 signal-fish-server:ci
    for i in $(seq 1 15); do
      if curl -sf http://localhost:3536/v2/health; then
        echo ""
        echo "Health check passed on attempt $i/15"
        exit 0
      fi
      echo "Attempt $i/15: server not ready, retrying in 2s..."
      sleep 2
    done
    echo "ERROR: Server failed to become healthy after 30s"
    echo "=== Docker logs ==="
    docker logs test-server
    exit 1
```

### Related Test Pattern

Always test that the Docker-style config (auth disabled, no config file) passes validation:

```rust
#[test]
fn test_docker_default_config_passes_validation() {
    let mut config = Config::default();
    config.security.require_metrics_auth = false;
    config.security.require_websocket_auth = false;
    assert!(validate_config_security(&config).is_ok());
}
```

---

## Agent Checklist

- [ ] Dockerfile uses multi-stage build with distroless or scratch runtime
- [ ] `cargo build --locked` used for reproducible builds
- [ ] Image runs as non-root (`USER nonroot:nonroot` or UID 65534)
- [ ] Capabilities dropped (`--cap-drop=ALL`, no `allowPrivilegeEscalation`)
- [ ] Read-only root filesystem enabled
- [ ] CPU and memory requests/limits set in Kubernetes manifests
- [ ] PodDisruptionBudget configured (`minAvailable` ≥ 2)
- [ ] `terminationGracePeriodSeconds` set for WebSocket draining (60–120s)
- [ ] Rolling update: `maxSurge: 1`, `maxUnavailable: 0`
- [ ] Liveness, readiness, and startup probes configured
- [ ] Image scanned with Trivy/Grype in CI — critical/high CVEs block deploy
- [ ] `cargo audit` and `cargo deny check` run in CI pipeline
- [ ] Native build deps in composite action (`.github/actions/install-build-deps/action.yml`) match Dockerfile builder stage
- [ ] `scripts/check-ci-config.sh` passes after CI workflow changes
- [ ] Docker ENV overrides set for any config fields that default to secure-but-crash (e.g., auth)
- [ ] CI smoke test uses retry loop with `docker logs` dump on failure (no bare `sleep`)
- [ ] Config validation regression test exists for Docker-style defaults
- [ ] Images tagged with sha256 digest or git SHA — never `:latest` in production
- [ ] No secrets baked into image layers
- [ ] Secrets mounted as read-only volumes, not environment variables
- [ ] Logs written to stdout/stderr in JSON format
- [ ] Prometheus metrics endpoint exposed at `/metrics`

## Related Skills

- [web-service-security](./web-service-security.md) — TLS, auth, input validation, secrets management with `secrecy`
- [graceful-degradation](./graceful-degradation.md) — Connection draining, shutdown signaling, circuit breakers
- [observability-and-logging](./observability-and-logging.md) — Structured logging, tracing spans, OpenTelemetry export
- [dependency-management](./dependency-management.md) — `cargo audit`, `cargo deny`, dependency pinning, `Cargo.lock`
