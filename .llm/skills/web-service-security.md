# Skill: Web Service Security

<!-- trigger: security, auth, authentication, authorization, tls, secrets, input-validation, WebSocket-security, cors, headers, audit | Hardening a Rust/axum WebSocket signaling server against common attacks | Core -->

**Trigger**: When implementing, reviewing, or hardening authentication, authorization, input validation, TLS, secrets management, or any security-sensitive code path.

---

## When to Use
- Adding or modifying authentication/authorization logic
- Accepting external input (HTTP, WebSocket, query params, headers)
- Managing secrets, API keys, or tokens
- Configuring TLS, CORS, or security headers
- Reviewing dependencies for vulnerabilities
- Adding logging for security-relevant events

## When NOT to Use
- Pure business logic with no external input or auth boundaries
- Test fixtures or mock data (but tests should still cover security paths)
- Frontend-only styling or layout changes

## Rationalizations to Reject

| Excuse | Why It's Wrong | Required Action |
|--------|---------------|-----------------|
| "It's an internal API" | Internal networks get compromised. Lateral movement is a top attack vector. | Apply the same auth and validation as external APIs. |
| "We'll add auth later" | Unauthenticated endpoints ship to production and get forgotten. | Authenticate from day one. Block the PR until auth is in place. |
| "Only trusted clients connect" | Clients can be reverse-engineered, spoofed, or compromised. The server is the trust boundary. | Validate every message server-side regardless of client trust. |
| "It's just a signaling server" | Signaling controls who connects to whom. Hijacking signaling hijacks the session. | Treat signaling messages as security-critical. |
| "Input validation is too slow" | Validation cost is negligible vs. network I/O. A malformed message can crash the server. | Validate all input at the boundary. Benchmark if concerned. |
| "We'll fix it when pen-tested" | Pen tests find issues after deployment. Fixing in production is 10–100× costlier. | Build security in during development. Every PR, every review. |

---

## 1. Input Validation

### Newtypes to Enforce Invariants

```rust
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(try_from = "String")]
pub struct RoomCode(String);

impl TryFrom<String> for RoomCode {
    type Error = ValidationError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() < 4 || value.len() > 32 { return Err(ValidationError::length("room_code", 4, 32)); }
        if !value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(ValidationError::charset("room_code"));
        }
        Ok(Self(value))
    }
}

```

### Serde Validation at Deserialization

```rust
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinRequest {
    pub room_code: RoomCode,       // newtype-validated
    pub display_name: DisplayName, // newtype: 1–64 chars, no control chars
    #[serde(default)]
    pub metadata: Option<BoundedString<256>>,
}

```

### Allowlists, Not Denylists

```rust

// ❌ Denylist — always incomplete, bypassable
fn is_valid(input: &str) -> bool { !input.contains('<') && !input.contains('>') }
// ✅ Allowlist — only permit known-good characters
fn is_valid(input: &str) -> bool {
    input.chars().all(|c| c.is_ascii_alphanumeric() || "-_. ".contains(c))
}

```

### Message Size Limits

```rust

let app = Router::new()
    .route("/api/rooms", post(create_room))
    .layer(DefaultBodyLimit::max(16_384)); // 16 KB

```

---

## 2. Authentication & Authorization

### Authenticate Before WebSocket Upgrade

Never upgrade an unauthenticated connection:

```rust

async fn ws_handler(
    claims: AuthClaims,         // extracted + validated before upgrade
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, claims, state))
}

```

### Constant-Time Token Comparison

Use `subtle` to prevent timing attacks:

```rust

use subtle::ConstantTimeEq;
fn verify_api_key(provided: &[u8], expected: &[u8]) -> bool {
    provided.len() == expected.len() && provided.ct_eq(expected).into()
}

```

### JWT Validation with Explicit Algorithms

Never allow the token to specify its own algorithm:

```rust

let mut validation = Validation::new(Algorithm::ES256);
validation.set_required_spec_claims(&["exp", "iss", "sub"]);
validation.set_issuer(&["matchbox-server"]);
validation.set_audience(&["matchbox-client"]);
let token_data = jsonwebtoken::decode::<Claims>(token, &key, &validation)?;

```

### Per-Message Authorization

Validate permissions on every WebSocket message, not just at connection time:

```rust

async fn handle_message(msg: ClientMessage, claims: &AuthClaims, state: &AppState)
    -> Result<(), ProtocolError>
{
    match &msg {
        ClientMessage::Signal { target, .. } => state.authz.can_signal(claims, target).await?,
        ClientMessage::JoinRoom { room, .. } => state.authz.can_join(claims, room).await?,
    }
    process_message(msg, state).await
}

```

### Generic Error Messages

```rust

// ❌ Leaks whether the user exists
"Invalid password for user admin@example.com"
// ✅ Generic — no information leakage
"Invalid credentials"

```

---

## 3. WebSocket Security

### Origin Validation

```rust

async fn validate_origin(headers: &HeaderMap, allowed: &[String]) -> Result<(), StatusCode> {
    let origin = headers.get("origin")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::FORBIDDEN)?;
    if !allowed.iter().any(|a| a == origin) { return Err(StatusCode::FORBIDDEN); }
    Ok(())
}

```

### Frame/Message Size Limits (64 KB)

```rust

ws.max_frame_size(16_384)      // 16 KB per frame
  .max_message_size(65_536)    // 64 KB per message
  .on_upgrade(move |socket| handle_socket(socket, claims, state))

```

### Connection Caps with Semaphore

```rust

async fn ws_handler(
    ws: WebSocketUpgrade, State(sem): State<Arc<Semaphore>>,
) -> Result<impl IntoResponse, StatusCode> {
    let permit = sem.try_acquire_owned().map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(ws.on_upgrade(move |socket| async move {
        handle_socket(socket).await;
        drop(permit); // released on disconnect
    }))
}

```

### Heartbeat / Ping-Pong

Detect dead connections and reclaim resources:

```rust

tokio::select! {
    msg = socket.recv() => { /* handle message */ }
    _ = tokio::time::sleep(Duration::from_secs(30)) => {
        socket.send(Message::Ping(vec![])).await?;
    }
}

```

### Disable `permessage-deflate`

Compression enables CRIME/BREACH-style attacks and adds CPU overhead for small signaling messages. axum does NOT enable it by default — do not add `.enable_compression()`.

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
> ```toml
> secrecy = "0.10"
> subtle = "2"
> ```

### Load from Environment or Vault — Never Hardcode

```rust

let jwt_secret = Secret::new(
    std::env::var("JWT_SECRET").context("JWT_SECRET must be set")?
);
// ❌ NEVER: let jwt_secret = "super-secret-key";

```

### Redact from Logs

`secrecy::Secret` implements `Debug` as `Secret([REDACTED])` — secrets are safe in structured log output automatically.

### Separate Secrets Per Environment

Never share secrets between dev/staging/production. Use distinct env vars, vault paths, or secret manager entries for each environment.

---

## 5. TLS & Security Headers

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

// ❌ Short-circuits — leaks secret length via timing
if user_token == stored_token { ... }
// ✅ Constant-time — no timing side channel
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

// ❌ Panics on invalid input — attacker-controlled crash
let room: RoomCode = serde_json::from_str(&msg).unwrap();
// ✅ Propagate the error
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

Always emit structured events for: auth success/failure, authorization denial, rate limit triggers, connection open/close with peer address, invalid message format.

```rust

tracing::warn!(
    peer_addr = %addr, room_code = %room, reason = "unauthorized",
    "Authorization denied for room join"
);

```

### Never Log Secrets

```rust

// ❌ Token in logs — compromises the credential
tracing::info!(token = %bearer_token, "Auth attempt");
// ✅ Log only non-sensitive identifiers
tracing::info!(user_id = %claims.sub, token_prefix = &bearer_token[..8], "Auth attempt");

```

### Anomaly Detection Alerts

Set alerting thresholds for security anomalies:

- **Auth failures** > 10/min from single IP → alert + temporary block
- **Invalid messages** > 50/min from single connection → close + alert
- **Connection rate** > 100/sec total → trigger rate limiting alert

---

## Agent Checklist

- [ ] All external input validated via newtypes or `#[serde(deny_unknown_fields)]`
- [ ] Authentication happens before WebSocket upgrade
- [ ] Token comparisons use `subtle::ConstantTimeEq`, not `==`
- [ ] JWT validation uses explicit algorithm, issuer, and audience
- [ ] WebSocket frame/message sizes capped (≤ 64 KB)
- [ ] Concurrent connections limited via `Semaphore`
- [ ] Secrets use `secrecy::Secret` — never logged or hardcoded
- [ ] TLS 1.2+ enforced; security headers set (HSTS, CSP, X-Frame-Options)
- [ ] `cargo audit` and `cargo deny check` pass in CI
- [ ] `#![forbid(unsafe_code)]` set at crate root
- [ ] `overflow-checks = true` in release profile
- [ ] Auth events logged with structured fields; no secrets in logs

## Quick Reference

| Area | Key Crate | Critical Setting |
|------|-----------|-----------------|
| Input validation | Newtypes + serde | `#[serde(deny_unknown_fields)]` |
| Auth tokens | `subtle` | `ct_eq()` for all comparisons |
| JWT | `jsonwebtoken` | Explicit `Algorithm`, required claims |
| Secrets | `secrecy` | `Secret<String>`, `expose_secret()` |
| WebSocket limits | axum WS | `max_frame_size(16_384)` |
| Connection caps | `tokio::sync::Semaphore` | `try_acquire_owned()` |
| TLS | `rustls` | TLS 1.2+ (default) |
| CORS | `tower-http` | Explicit origin list, never `*` |
| Dep audit | `cargo-audit`, `cargo-deny` | Run in CI on every PR |
| Unsafe | Compiler | `#![forbid(unsafe_code)]` |
| Logging | `tracing` | JSON format, structured fields |

## Related Skills

- [defensive-programming](./defensive-programming.md) — Input validation, panic prevention, safe arithmetic
- [error-handling-guide](./error-handling-guide.md) — Error types, generic messages, context propagation
- [observability-and-logging](./observability-and-logging.md) — Structured logging, tracing spans, log hygiene
- [dependency-management](./dependency-management.md) — Cargo.lock, version pinning, audit workflows
- [ddos-and-rate-limiting](./ddos-and-rate-limiting.md) — Rate limiting, connection management, DDoS prevention
