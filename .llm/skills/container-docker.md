# Skill: Docker and Container Builds

<!--
  trigger: Docker, dockerfile, container, multi-stage build, distroless, image scanning, trivy, CI build deps
  | Dockerfile best practices, image hardening, CI/CD image pipelines, and native build dependencies
  | Infrastructure
-->

**Trigger**: When writing Dockerfiles, setting up image scanning in CI, or managing native build
dependencies for Rust cargo features.

See also:

- [Deployment Strategies](./deployment-strategies.md) — Kubernetes, health checks, graceful shutdown
- [Container Security](./container-security.md) — Secrets management, immutable tags, security contexts

---

## TL;DR

- Use multi-stage builds with distroless runtime images; never ship build tools to production
- `cargo build --locked` for reproducible builds
- Run as non-root, drop ALL capabilities, use read-only root filesystem
- Scan images with Trivy/Grype in CI; block deployment on critical/high CVEs
- Native build deps must be in both the composite action AND the Dockerfile builder stage

---

## Multi-Stage Build

```dockerfile
# ---- Builder stage ----
FROM rust:1.88-bookworm AS builder

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

COPY --from=builder /app/target/release/signal-fish-server /usr/local/bin/server

EXPOSE 3536
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/server"]
```

| Principle | Implementation |
|-----------|---------------|
| **Reproducible builds** | `cargo build --locked` — uses exact `Cargo.lock` versions |
| **Minimal runtime** | `distroless/cc-debian12:nonroot` — no shell, no package manager |
| **Non-root user** | `USER nonroot:nonroot` — UID 65534 by convention |
| **Layer cache** | Copy `Cargo.toml` + `Cargo.lock` before source for dependency caching |
| **Single binary** | COPY only the compiled binary — no source, no build artifacts |

---

## Multi-Architecture Images (cross-compile, not emulate)

Publish one manifest list (e.g. `linux/amd64,linux/arm64`) so every arch pulls the same tag.
Cross-compile the heavy Rust build; reserve QEMU for the trivial runtime stage only.

- **Pin the builder to the host:** `FROM --platform=$BUILDPLATFORM rust:... AS builder`, then
  cross-compile to `$TARGETARCH`. The compile runs at native speed; emulated Rust builds are
  minutes-to-tens-of-minutes slower and flakier.
- **Decide native-vs-cross by `$TARGETARCH` == `$BUILDARCH`** — never hard-code one arch as native;
  the runner may be amd64 (arm cross) or arm64 (amd64 cross).
- **Name the cross libc dev package explicitly** under `--no-install-recommends`
  (`gcc-aarch64-linux-gnu` **+** `libc6-dev-arm64-cross`); otherwise linking fails with
  `cannot find Scrt1.o`.
- **`docker/setup-qemu-action` is still required** for the runtime stage's `useradd`/`apt-get`.
- Pin the exact `platforms:` and supported target triples in a drift test so a "simplification"
  can't silently drop an arch (see `tests/ci_config_tests.rs` in this repo).

---

## Container Hardening

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
  signal-fish-server:latest
```

### Compose Hardening

```yaml
services:
  signaling:
    image: signal-fish-server:sha-abc1234
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

---

## Image Scanning in CI

```yaml
# GitHub Actions
- name: Scan image with Trivy
  uses: aquasecurity/trivy-action@v0.28.0
  with:
    image-ref: signal-fish-server:${{ github.sha }}
    format: table
    exit-code: 1
    severity: CRITICAL,HIGH
    ignore-unfixed: true

- name: Generate SBOM
  uses: anchore/sbom-action@v0.17.0
  with:
    image: signal-fish-server:${{ github.sha }}
    format: spdx-json
    output-file: sbom.spdx.json
```

---

## CI Native Build Dependencies

When a Cargo feature requires native C libraries (e.g., `kafka` → `rdkafka` → `librdkafka`
→ `cmake`, `libcurl-dev`, `libssl-dev`), CI workflows using `--all-features` will fail
unless those libraries are installed in the runner environment.

### The Solution: Composite Action

All native build dependencies are centralized in:

```text
.github/actions/install-build-deps/action.yml
```

Every CI job that builds Rust code with `--all-features` **must** use this action:

```yaml
steps:
  - uses: ./.github/actions/install-build-deps
  - run: cargo build --all-features
```

**Keeping Package Lists in Sync** — the same native packages must be installed in two places:

| Location | Purpose |
|----------|---------|
| `.github/actions/install-build-deps/action.yml` | CI runners (Ubuntu) |
| `Dockerfile` builder stage | Docker image builds |

When adding a new native dependency, update **both** files.

**When Adding a Cargo Feature with Native Dependencies:**

1. Add the feature to `Cargo.toml`
2. Add required packages to `.github/actions/install-build-deps/action.yml`
3. Add the same packages to the `Dockerfile` builder stage
4. Verify all workflows using `--all-features` include the composite action step
5. Run `scripts/check-ci-config.sh` to confirm

---

## Config Validation and Docker Startup

### The "Auth Defaults" Pitfall

When `default_require_auth()` returns `true` (secure-by-default), containers without a
mounted config file **will crash at startup** unless the Dockerfile explicitly disables
auth via environment variable overrides:

```dockerfile
# Disable auth by default so the container starts without a config file.
# Production deployments should mount a config.json or set auth env vars.
ENV SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH=false
ENV SIGNAL_FISH__SECURITY__REQUIRE_WEBSOCKET_AUTH=false
```

### Smoke Test Pattern (CI)

Use a retry loop instead of a bare `sleep`:

```yaml
- name: Smoke test
  run: |
    docker run -d --name test-server -p 3536:3536 signal-fish-server:ci
    for i in $(seq 1 15); do
      if curl -sf http://localhost:3536/v2/health; then
        echo "Health check passed on attempt $i/15"
        exit 0
      fi
      echo "Attempt $i/15: server not ready, retrying in 2s..."
      sleep 2
    done
    echo "ERROR: Server failed to become healthy after 30s"
    docker logs test-server
    exit 1
```

### Regression Test

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

## Dockerfile Shell Portability

Docker `RUN` commands use `/bin/sh` (dash on Debian) by default, **not bash**.
Bash-specific features silently fail or produce unexpected results.

### Common Pitfalls

```dockerfile
# ❌ WRONG: Brace expansion is bash-only — /bin/sh treats it as a literal string
RUN rm -rf /path/{cache,src}

# ✅ CORRECT: Spell out each path explicitly
RUN rm -rf /path/cache /path/src
```

```dockerfile
# ❌ WRONG: [[ ]] is bash-only
RUN if [[ -f /app/config.json ]]; then echo "found"; fi

# ✅ CORRECT: Use single brackets (POSIX)
RUN if [ -f /app/config.json ]; then echo "found"; fi
```

### Validation

Run `scripts/check-dockerfile-portability.sh` to catch bash-isms in Dockerfiles.
This check runs automatically in the pre-commit hook when Dockerfiles are staged.

### If You Need Bash

If a RUN command genuinely requires bash, set the shell explicitly:

```dockerfile
SHELL ["/bin/bash", "-c"]
RUN echo "Now bash features like {a,b} work"
```

Or use `bash -c` for a single command:

```dockerfile
RUN bash -c 'rm -rf /path/{cache,src}'
```

---

## Related Skills

- [Deployment Strategies](./deployment-strategies.md) — Kubernetes, health checks, graceful shutdown
- [Container Security](./container-security.md) — Secrets management, immutable tags
- [MSRV Management](./msrv-management.md) — Matching Dockerfile Rust version to MSRV
- [Dependency Management Cargo](./dependency-management-cargo.md) — `cargo audit`, `cargo deny`
- [Shell Scripting Patterns](./shell-scripting-patterns.md) — Shell portability, POSIX vs bash
