# Container Security

See also:

- [Container Docker](../SKILL.md) — Dockerfile builds, image scanning, CI/CD pipelines
- [Deployment Strategies](../../deployment-strategies/SKILL.md) — Kubernetes deployment patterns, health checks

---

## TL;DR

- Never bake secrets into image layers — inject at runtime via volumes or env vars
- Use immutable image tags (sha256 digest or git SHA) — never `:latest` in production
- Run as non-root, drop ALL capabilities, read-only root filesystem
- Mount secrets as read-only volumes, not environment variables
- Sign images with Cosign for supply chain integrity

---

## Rationalizations to Reject

| Excuse | Why It's Wrong | Required Action |
|--------|---------------|-----------------|
| "We run as root for simplicity" | Root in container = root on host if container escapes | Always `USER nonroot:nonroot`. No exceptions. |
| "We'll harden later" | Unhardened images stay in production. Attack surface compounds. | Multi-stage + distroless from day one. |
| "Latest tag is fine for dev" | `:latest` is mutable and unreproducible | Use sha256 digests or semver + git SHA. |

---

## Immutable Image Tags

```yaml
# WRONG: Mutable tag — unreproducible, can be overwritten
image: signal-fish-server:latest

# CORRECT: Git SHA tag — traceable to exact commit
image: signal-fish-server:sha-abc1234

# CORRECT: Digest — cryptographically immutable
image: ghcr.io/example/signal-fish-server@sha256:abcdef1234567890
```

---

## Secrets in Containers

### Never Bake Secrets into Images

```dockerfile
# WRONG: Secret in image layer — visible to anyone who pulls the image
ENV JWT_SECRET=my-secret-key
COPY secrets.json /app/secrets.json

# CORRECT: Secrets injected at runtime via environment or mounted volumes
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

## Security Context (Kubernetes)

```yaml
spec:
  securityContext:
    runAsNonRoot: true
    runAsUser: 65534
    runAsGroup: 65534
    fsGroup: 65534
    seccompProfile:
      type: RuntimeDefault
  containers:
    - name: signaling
      securityContext:
        allowPrivilegeEscalation: false
        readOnlyRootFilesystem: true
        capabilities:
          drop: [ALL]
```

---

## Image Signing with Cosign

```yaml
# In CI/CD pipeline after push
- name: Sign image with Cosign
  uses: sigstore/cosign-installer@v3

- run: cosign sign --yes ghcr.io/example/signal-fish-server@${{ steps.push.outputs.digest }}
```

---

## Agent Checklist

- [ ] No secrets baked into image layers (no `ENV SECRET=` or `COPY secrets` in Dockerfile)
- [ ] Secrets mounted as read-only volumes (`defaultMode: 0400`), not environment variables
- [ ] Image runs as non-root (`USER nonroot:nonroot` or UID 65534)
- [ ] Capabilities dropped (`--cap-drop=ALL`, no `allowPrivilegeEscalation`)
- [ ] Read-only root filesystem enabled
- [ ] Images tagged with sha256 digest or git SHA — never `:latest` in production
- [ ] Image scanned with Trivy/Grype in CI — critical/high CVEs block deploy
- [ ] `cargo audit` and `cargo deny check` run in CI pipeline
- [ ] Image signed with Cosign for supply chain integrity
- [ ] `seccompProfile: RuntimeDefault` set on pod security context
- [ ] Secret rotation handled without container restart (file watcher or ESO refresh)

---

## See Also

- [Container Docker](../SKILL.md) — Dockerfile builds, multi-stage, image scanning, CI/CD pipelines
- [Deployment Strategies](../../deployment-strategies/SKILL.md) — Kubernetes, health checks, graceful shutdown
- [Web Service Security Auth](../../web-service-security/SKILL.md) — TLS, auth, input validation
- [Supply Chain Audit Policy](../../supply-chain-security/SKILL.md) — cargo audit, cargo deny, dependency pinning
