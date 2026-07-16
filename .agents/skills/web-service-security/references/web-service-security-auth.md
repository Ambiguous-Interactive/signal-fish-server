# Web Service Security — Input Validation and Authentication

**Applies to**: When implementing authentication, authorization, input validation, or WebSocket security for the server.

---

## When to Use

- Adding or modifying authentication/authorization logic
- Accepting external input (HTTP, WebSocket, query params, headers)
- Configuring CORS or Origin validation
- Reviewing per-message authorization logic

## When NOT to Use

- Secrets management and TLS (see [Web Service Security Hardening](./web-service-security-hardening.md))
- Rate limiting and connection caps (see [DDoS Rate Limiting Connections](./ddos-rate-limiting-connections.md))

## Rationalizations to Reject

| Excuse | Why It's Wrong | Required Action |
|--------|---------------|-----------------|
| "It's an internal API" | Internal networks get compromised. Lateral movement is a top attack vector. | Apply the same auth and validation as external APIs. |
| "We'll add auth later" | Unauthenticated endpoints ship to production and get forgotten. | Authenticate from day one. Block the PR until auth is in place. |
| "Only trusted clients connect" | Clients can be reverse-engineered, spoofed, or compromised. | Validate every message server-side regardless of client trust. |
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
// BAD: Denylist — always incomplete, bypassable
fn is_valid(input: &str) -> bool { !input.contains('<') && !input.contains('>') }

// GOOD: Allowlist — only permit known-good characters
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

## 2. Authentication and Authorization

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
// BAD: Leaks whether the user exists
"Invalid password for user admin@example.com"

// GOOD: Generic — no information leakage
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

Compression enables CRIME/BREACH-style attacks and adds CPU overhead for small signaling messages.
axum does NOT enable it by default — do not add `.enable_compression()`.

---

## Agent Checklist

- [ ] All external input validated via newtypes or `#[serde(deny_unknown_fields)]`
- [ ] Authentication happens before WebSocket upgrade
- [ ] Token comparisons use `subtle::ConstantTimeEq`, not `==`
- [ ] JWT validation uses explicit algorithm, issuer, and audience
- [ ] WebSocket frame/message sizes capped (≤ 64 KB)
- [ ] Concurrent connections limited via `Semaphore`
- [ ] Per-message authorization checked on every WebSocket message
- [ ] Error messages are generic — no user existence or credential leakage

## Quick Reference

| Area | Key Crate | Critical Setting |
|------|-----------|-----------------|
| Input validation | Newtypes + serde | `#[serde(deny_unknown_fields)]` |
| Auth tokens | `subtle` | `ct_eq()` for all comparisons |
| JWT | `jsonwebtoken` | Explicit `Algorithm`, required claims |
| WebSocket limits | axum WS | `max_frame_size(16_384)` |
| Connection caps | `tokio::sync::Semaphore` | `try_acquire_owned()` |
| CORS | `tower-http` | Explicit origin list, never `*` |

## Related References

- [Web Service Security Hardening](./web-service-security-hardening.md) — Secrets, TLS, Rust safety, security logging
- [Defensive Programming](../../rust-development/references/defensive-programming.md) — Input validation, panic prevention, safe arithmetic
- [Error Handling Guide](../../rust-development/references/error-handling-guide.md) — Error types, generic messages, context propagation
- [DDoS Rate Limiting Connections](./ddos-rate-limiting-connections.md) —
  Rate limiting, connection management, DDoS prevention
