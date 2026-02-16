# Skill: Error Handling Guide

<!-- trigger: error, thiserror, anyhow, result, unwrap, context, map_err, propagation | Designing error types and propagation patterns | Core -->

**Trigger**: When designing error types, converting `.unwrap()` to proper handling, or adding error context.

---

## When to Use

- Creating new error enums with `thiserror`
- Converting `.unwrap()` calls to `?` propagation
- Adding `.context()` or `.map_err()` at boundaries
- Implementing `IntoResponse` for HTTP error responses
- Designing error variants for caller-oriented matching

---

## When NOT to Use

- Test code where `.unwrap()` / `.expect()` is acceptable
- Logging errors specifically (see [observability-and-logging](./observability-and-logging.md))

---

## TL;DR

- Use `thiserror` for library/domain error enums, `anyhow` for application-level plumbing.
- Design error variants around what callers need to match on, not internal implementation details.
- Always use `?` for propagation — never `.unwrap()` in production code.
- Add context with `.context()` or `.map_err()` at every boundary crossing.
- Log errors with structured tracing fields, not string interpolation.

---

## `thiserror` vs `anyhow`

| Crate | Purpose | When to use |
|-------|---------|-------------|
| `thiserror` | Define typed error enums | Public APIs, domain boundaries, matchable errors |
| `anyhow` | Flexible error propagation | Application top-level, scripts, quick prototyping |

```rust
// ✅ thiserror for domain errors — callers can match on variants
#[derive(Debug, thiserror::Error)]
pub enum JoinError {
    #[error("room not found: {code}")]
    RoomNotFound { code: String },

    #[error("room is full ({current}/{max} players)")]
    RoomFull { current: usize, max: usize },

    #[error("player already in room")]
    AlreadyJoined,

    #[error("authentication failed")]
    AuthFailed(#[from] AuthError),

    #[error("database error")]
    Database(#[from] sqlx::Error),
}

// ✅ anyhow at the application boundary (main, handler top-level)
use anyhow::Context;
async fn main() -> anyhow::Result<()> {
    let config = load_config()
        .context("Failed to load server configuration")?;
    run_server(config).await
        .context("Server exited with error")
}
```rust

---

## Designing Error Enums

Design around **what the caller needs to do**, not implementation details:

```rust
// ❌ Implementation-leaked errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("sqlx error")]
    Sqlx(#[from] sqlx::Error),            // Caller doesn't care about sqlx
    #[error("serde error")]
    Serde(#[from] serde_json::Error),      // Leaks serialization choice
}

// ✅ Caller-oriented errors
#[derive(Debug, thiserror::Error)]
pub enum RoomError {
    #[error("room {0} not found")]
    NotFound(RoomCode),                    // Caller: return 404

    #[error("room {0} is full")]
    Full(RoomCode),                        // Caller: return 409

    #[error("invalid room configuration: {0}")]
    InvalidConfig(String),                 // Caller: return 400

    #[error("internal error")]
    Internal(#[source] anyhow::Error),     // Caller: return 500, log details
}

// Convert implementation errors at the boundary
impl From<sqlx::Error> for RoomError {
    fn from(err: sqlx::Error) -> Self {
        Self::Internal(err.into())
    }
}
```

---

## The `?` Operator and Error Propagation

```rust
// ✅ Chain ? for clean propagation
pub async fn join_room(&self, req: JoinRequest) -> Result<JoinResponse, JoinError> {
    let room = self.db.find_room(&req.room_code).await?;      // DbError → JoinError
    let player = self.auth.validate_token(&req.token).await?;  // AuthError → JoinError
    room.add_player(player)?;                                   // RoomError → JoinError
    Ok(JoinResponse { room_id: room.id() })
}
```rust

---

## Adding Context

```rust
use anyhow::Context;

// ✅ .context() adds human-readable context to error chain
let content = std::fs::read_to_string(&path)
    .context("Failed to read configuration file")?;

let config: ServerConfig = serde_json::from_str(&content)
    .with_context(|| format!("Failed to parse config from {}", path.display()))?;

// ✅ .map_err() for typed conversions at boundaries
let port: u16 = env::var("PORT")
    .map_err(|_| ConfigError::Missing("PORT"))?
    .parse()
    .map_err(|_| ConfigError::InvalidValue("PORT", "must be valid u16"))?;

// ✅ .ok_or() / .ok_or_else() to convert Option → Result
let room = self.rooms.get(&room_id)
    .ok_or(RoomError::NotFound(room_id.clone()))?;

let player = self.rooms.get(&room_id)
    .ok_or_else(|| RoomError::NotFound(room_id.clone()))?;  // Lazy allocation
```

---

## The Unwrap Hierarchy

```rust
// ✅ Propagate with ?
let value = operation()?;
let value = maybe_value.ok_or(Error::Missing)?;

// ✅ Provide a fallback
let value = maybe_value.unwrap_or(default);
let value = maybe_value.unwrap_or_default();

// ✅ Transform without unwrapping
let result = maybe_value.map(|v| v.to_string());
let result = maybe_value.and_then(|v| v.parse().ok());
```rust

**`expect()` only for compile-time-provable cases** with a `// SAFETY:` comment — it still panics:

```rust
// SAFETY: Regex literal is known valid at compile time
let re = Regex::new(r"^\d+$").expect("valid regex literal");

// ❌ NEVER in production code for runtime-dependent values
let value = map.get("key").expect("key must exist");  // NOT provable!
```

---

## Custom Error Types with Rich Information

```rust
#[derive(Debug, thiserror::Error)]
pub enum WebSocketError {
    #[error("connection closed by peer (code: {code:?}, reason: {reason})")]
    PeerClosed {
        code: Option<u16>,
        reason: String,
        peer_id: PlayerId,
    },

    #[error("message too large: {size} bytes (max: {max})")]
    MessageTooLarge { size: usize, max: usize },

    #[error("rate limited: {requests} requests in {window_secs}s (max: {limit})")]
    RateLimited {
        requests: u32,
        window_secs: u64,
        limit: u32,
    },
}
```rust

---

## Error Conversion with `From`/`Into`

```rust
// Use #[from] in thiserror for simple forwarding
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("I/O error")]
    Io(#[from] std::io::Error),
    #[error("configuration error")]
    Config(#[from] ConfigError),
}

// Use #[source] when you want wrapping without auto-From
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("failed to bind to port {port}")]
    Bind { port: u16, #[source] source: std::io::Error },
}

// Implement From manually for conditional mapping
impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        match err {
            sqlx::Error::RowNotFound => Self::NotFound,
            other => Self::Internal(other.into()),
        }
    }
}
```

---

## Error Handling in Async Code

The `?` operator works normally in async functions. For spawned tasks, handle `JoinError` (panics/cancellation) separately:

```rust
match tokio::spawn(async move { process(msg).await }).await {
    Ok(Ok(())) => {}                           // Task succeeded
    Ok(Err(process_err)) => log_error(process_err),  // Task returned error
    Err(join_err) => tracing::error!("Task failed: {join_err}"), // Panicked/cancelled
}
```rust

See [async-rust-best-practices](./async-rust-best-practices.md) for structured concurrency with `JoinSet`.

---

## Error Types in Public APIs (axum Handlers)

Map domain errors to HTTP status codes via `IntoResponse`. See [api-design-guidelines](./api-design-guidelines.md) for the full pattern.

```rust
impl IntoResponse for RoomError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            Self::NotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
            Self::Full(_) => (StatusCode::CONFLICT, self.to_string()),
            Self::InvalidConfig(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            Self::Internal(e) => {
                tracing::error!(error = %e, "Internal server error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string())
            }
        };
        (status, axum::Json(serde_json::json!({ "error": message }))).into_response()
    }
}
```

---

## Logging Errors with Tracing

See [observability-and-logging](./observability-and-logging.md) for full structured logging patterns.

```rust
// ✅ Structured fields, not string interpolation
tracing::error!(error = %err, room_id = %room_id, "Failed to join room");
tracing::warn!(error = ?err, "Operation failed, retrying");  // Debug (?) for full chain
```rust

---

## Testing Error Conditions

See [testing-strategies](./testing-strategies.md) for comprehensive test patterns.

```rust
#[tokio::test]
async fn test_join_nonexistent_room() {
    let server = TestServer::new().await;
    let result = server.join_room("INVALID").await;
    assert!(matches!(result, Err(JoinError::RoomNotFound { .. })));
}
```

---

## Panic-Free Alternatives

See [defensive-programming](./defensive-programming.md) for comprehensive safe alternatives.

| Panicking | Panic-Free |
|-----------|------------|
| `vec[i]` | `vec.get(i).ok_or(Error)?` |
| `s.parse().unwrap()` | `s.parse()?` |
| `a / b` | `a.checked_div(b).ok_or(Error)?` |
| `x as u32` | `u32::try_from(x)?` |
| `mutex.lock().unwrap()` | `mutex.lock().unwrap_or_else(\|p\| p.into_inner())` |

---

## Agent Checklist

- [ ] Domain errors defined with `thiserror`, app-level uses `anyhow`
- [ ] Error variants match what callers need to handle (not internal impl)
- [ ] `.context()` or `.map_err()` at every module/crate boundary
- [ ] Zero `.unwrap()` in production code
- [ ] `.expect()` only for compile-time-provable cases with `// SAFETY:` comment
- [ ] `#[must_use]` on all Result-returning public functions
- [ ] Errors logged with structured tracing fields
- [ ] Error test covers every variant
- [ ] axum handlers map errors to appropriate HTTP status codes
- [ ] Internal error details never leaked to API consumers

---

## Related Skills

- [defensive-programming](./defensive-programming.md) — Panic-free patterns and safe alternatives
- [api-design-guidelines](./api-design-guidelines.md) — Error design for public APIs
- [async-rust-best-practices](./async-rust-best-practices.md) — Async error propagation
- [observability-and-logging](./observability-and-logging.md) — Structured error logging
