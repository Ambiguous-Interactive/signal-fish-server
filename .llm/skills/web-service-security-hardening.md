# Skill: Web Service Security — Hardening and Operational Security

<!--
  trigger: secrets, tls, security headers, unsafe code, overflow checks, security logging, audit, hsts, csp
  | Secrets management, TLS, Rust safety, dependency audit, and security logging
  | Core
-->

**Trigger**: When managing secrets, configuring TLS/security headers, auditing dependencies, or
implementing security logging for the server.

---

## When to Use

- Managing secrets, API keys, or tokens
- Configuring TLS, CORS, or security headers
- Reviewing dependencies for vulnerabilities
- Adding logging for security-relevant events
- Using Rust safety features (`forbid(unsafe_code)`, overflow checks)

## When NOT to Use

- Input validation and authentication patterns (see [web-service-security-auth](./web-service-security-auth.md))
- Rate limiting and connection caps (see [ddos-rate-limiting-connections](./ddos-rate-limiting-connections.md))

---

## 4. Secrets Management

### Use the `secrecy` Crate

```rust
use secrecy::{Secret, ExposeSecret};
pub struct AppConfig {
    pub db_url: Secret<String>,
    pub jwt_secret: Secret<String>,
}
let pool = PgPool::connect(config.db_url.expose_secret()).await?;
```

> **Note:** The `secrecy` and `subtle` crates must be added to `Cargo.toml` if not already present:
>
> ```toml
> secrecy = "0.10"
> subtle = "2"
> ```

### Load from Environment or Vault — Never Hardcode

```rust
let jwt_secret = Secret::new(
    std::env::var("JWT_SECRET").context("JWT_SECRET must be set")?
);
// NEVER: let jwt_secret = "super-secret-key";
```

### Redact from Logs

`secrecy::Secret` implements `Debug` as `Secret([REDACTED])` — secrets are safe in structured log output automatically.

### Separate Secrets Per Environment

Never share secrets between dev/staging/production.
Use distinct env vars, vault paths, or secret manager entries for each environment.

---

## 5. TLS and Security Headers

### Enforce TLS 1.2+

```rust
let tls_config = RustlsConfig::from_pem_file("certs/server.pem", "certs/server.key").await?;
// rustls defaults to TLS 1.2+ with safe cipher suites
axum_server::bind_rustls(addr, tls_config).serve(app.into_make_service()).await?;
```

### Security Headers Middleware

```rust
async fn security_headers(req: Request, next: middleware::Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert("strict-transport-security", "max-age=63072000; includeSubDomains; preload".parse().unwrap());
    h.insert("x-content-type-options", "nosniff".parse().unwrap());
    h.insert("x-frame-options", "DENY".parse().unwrap());
    h.insert("referrer-policy", "no-referrer".parse().unwrap());
    h.insert("content-security-policy", "default-src 'none'".parse().unwrap());
    h.insert("cache-control", "no-store".parse().unwrap());
    res
}
```

### CORS Allowlist — Never Wildcard in Production

```rust
let cors = CorsLayer::new()
    .allow_origin(AllowOrigin::list([
        "https://app.example.com".parse().unwrap(),
    ]))
    .allow_methods([Method::GET, Method::POST])
    .allow_headers([CONTENT_TYPE, AUTHORIZATION]);
```

---

## 6. Dependency Security

Run in CI on every PR:

```bash
cargo audit              # known CVE database
cargo deny check         # license + advisory + ban checks
cargo vet                # require review of new third-party code
```

Pin security-critical dependencies with exact versions:

```toml
[dependencies]
jsonwebtoken = "=9.3.0"
```

Always commit `Cargo.lock` to the repository for reproducible builds.

---

## 7. Rust-Specific Security

### Forbid Unsafe Code

```rust
#![forbid(unsafe_code)]
```

### Overflow Checks in Release

```toml
[profile.release]
overflow-checks = true
```

### Constant-Time Comparisons for All Secrets

```rust
// BAD: Short-circuits — leaks secret length via timing
if user_token == stored_token { ... }

// GOOD: Constant-time — no timing side channel
if user_token.as_bytes().ct_eq(stored_token.as_bytes()).into() { ... }
```

### Type-State Pattern for Auth Boundaries

Encode auth status in the type system so unauthenticated access cannot compile:

```rust
pub struct Connection<S> { inner: WebSocket, _state: S }
pub struct Unauthenticated;
pub struct Authenticated { claims: AuthClaims }

impl Connection<Unauthenticated> {
    pub async fn authenticate(self, key: &DecodingKey)
        -> Result<Connection<Authenticated>, AuthError> { /* ... */ }
}
impl Connection<Authenticated> {
    pub async fn join_room(&self, room: &RoomCode) -> Result<(), Error> { todo!() }
}
```

### Never `.unwrap()` on User Input

```rust
// BAD: Panics on invalid input — attacker-controlled crash
let room: RoomCode = serde_json::from_str(&msg).unwrap();

// GOOD: Propagate the error
let room: RoomCode = serde_json::from_str(&msg)
    .map_err(|e| ProtocolError::InvalidMessage(e.to_string()))?;
```

---

## 8. Security Logging

### Structured JSON for Production

```rust
tracing_subscriber::fmt().json()
    .with_env_filter(EnvFilter::from_default_env())
    .with_target(true).init();
```

### Log Security-Relevant Events

Always emit structured events for: auth success/failure, authorization denial, rate limit triggers,
connection open/close with peer address, invalid message format.

```rust
tracing::warn!(
    peer_addr = %addr, room_code = %room, reason = "unauthorized",
    "Authorization denied for room join"
);
```

### Never Log Secrets

```rust
// BAD: Token in logs — compromises the credential
tracing::info!(token = %bearer_token, "Auth attempt");

// GOOD: Log only non-sensitive identifiers
tracing::info!(user_id = %claims.sub, token_prefix = &bearer_token[..8], "Auth attempt");
```

### Anomaly Detection Alerts

Set alerting thresholds for security anomalies:

- **Auth failures** > 10/min from single IP → alert + temporary block
- **Invalid messages** > 50/min from single connection → close + alert
- **Connection rate** > 100/sec total → trigger rate limiting alert

---

## Agent Checklist

- [ ] Secrets use `secrecy::Secret` — never logged or hardcoded
- [ ] TLS 1.2+ enforced; security headers set (HSTS, CSP, X-Frame-Options)
- [ ] `cargo audit` and `cargo deny check` pass in CI
- [ ] `#![forbid(unsafe_code)]` set at crate root
- [ ] `overflow-checks = true` in release profile
- [ ] Auth events logged with structured fields; no secrets in logs
- [ ] Type-state pattern used for auth boundaries where applicable

## Quick Reference

| Area | Key Crate | Critical Setting |
|------|-----------|-----------------|
| Secrets | `secrecy` | `Secret<String>`, `expose_secret()` |
| TLS | `rustls` | TLS 1.2+ (default) |
| Dep audit | `cargo-audit`, `cargo-deny` | Run in CI on every PR |
| Unsafe | Compiler | `#![forbid(unsafe_code)]` |
| Logging | `tracing` | JSON format, structured fields |

## Related Skills

- [web-service-security-auth](./web-service-security-auth.md) — Input validation, authentication, WebSocket security
- [observability-and-logging](./observability-and-logging.md) — Structured logging, tracing spans, log hygiene
- [dependency-management-cargo](./dependency-management-cargo.md) — Cargo.lock, audit workflows
- [supply-chain-security](./supply-chain-audit-policy.md) — Full supply chain audit pipeline
- [ddos-rate-limiting-connections](./ddos-rate-limiting-connections.md) — Rate limiting, connection management, DDoS prevention
