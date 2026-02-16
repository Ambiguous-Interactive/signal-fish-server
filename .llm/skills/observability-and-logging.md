# Skill: Observability and Logging

<!-- trigger: logging, tracing, metrics, opentelemetry, spans, instrument, structured | Adding metrics, tracing spans, and structured logging | Performance -->

**Trigger**: When adding logging, metrics, tracing spans, or OpenTelemetry instrumentation.

---

## When to Use

- Adding structured log events or tracing spans
- Using `#[instrument]` on functions
- Configuring log levels or subscriber output
- Exporting metrics via OpenTelemetry
- Reviewing log hygiene (no PII, no secrets)

---

## When NOT to Use

- Error type design (see [error-handling-guide](./error-handling-guide.md))
- Performance profiling with criterion (see [Rust-performance-optimization](./rust-performance-optimization.md))

---

## TL;DR

- Use `tracing` for all logging — never `println!`, `eprintln!`, or the `log` crate directly.
- Attach structured fields to spans and events; avoid string interpolation in log messages.
- Use `#[instrument]` on async functions with `skip` for non-Debug / large arguments.
- Configure JSON output for production, pretty-printed output for development.
- Export metrics and traces via OpenTelemetry OTLP to your observability backend.

---

## Structured Logging with tracing

### Use tracing, Not println!

```rust
// ❌ Unstructured, no levels, no context
println!("Player {} joined room {}", player_id, room_code);

// ❌ log crate — less structured, no spans
log::info!("Player {} joined room {}", player_id, room_code);

// ✅ tracing with structured fields
tracing::info!(
    player_id = %player_id,
    room_code = %room_code,
    "Player joined room"
);

```

### Structured Fields vs String Interpolation

```rust

// ❌ String interpolation — loses queryability
tracing::error!("Failed to join room {room_code}: {err}");

// ✅ Structured fields — searchable, filterable in log aggregators
tracing::error!(
    error = %err,
    room_code = %room_code,
    player_id = %player_id,
    "Failed to join room"
);

// Display (%) vs Debug (?)
tracing::warn!(error = %err, "user-facing format");   // uses Display
tracing::debug!(error = ?err, "internal detail");      // uses Debug — full chain

```

### Log Levels

| Level | When to Use | Example |
|-------|-------------|---------|
| `error!` | Unrecoverable failures, data loss risk | Database connection lost, invariant violated |
| `warn!` | Recoverable issues, degraded operation | Rate limit hit, retry succeeded after failure |
| `info!` | Significant normal events | Server started, player joined, room created |
| `debug!` | Detailed flow for development | Message parsed, cache hit/miss |
| `trace!` | Very verbose, per-message detail | Raw bytes received, individual state transitions |

### Spans and the `#[instrument]` Attribute

```rust

// ✅ #[instrument] creates a span around the function
#[tracing::instrument(skip(self, db), fields(room_code = %req.room_code))]
pub async fn join_room(
    &self,
    db: &Database,
    req: JoinRequest,
) -> Result<JoinResponse, JoinError> {
    // All events inside this function are automatically within the span
    tracing::debug!("looking up room");
    let room = db.find_room(&req.room_code).await?;
    tracing::info!(player_count = room.player_count(), "room found");
    Ok(room.join(req.player_id)?)
}

// ✅ Manual spans for finer control
async fn broadcast_message(room: &Room, msg: &Message) {
    let span = tracing::info_span!("broadcast", room_code = %room.code(), recipients = room.player_count());
    let _guard = span.enter();
    // ...
}

// ✅ Async-compatible span with .instrument()
use tracing::Instrument;
tokio::spawn(
    async move { process(msg).await }
        .instrument(tracing::info_span!("process_task", msg_id = %id))
);

```

---

## Subscriber Configuration

```rust

use tracing_subscriber::{fmt, EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("matchbox_signaling_server=info,tower_http=debug"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer())  // Use .json() for production
        .init();
}

```

Use `fmt::layer().json()` for machine-readable production output. For file rotation, use `tracing_appender::rolling` (hold the `_guard` for app lifetime).

---

## OpenTelemetry Integration

### Custom Metrics

```rust

use opentelemetry::global;
use opentelemetry::metrics::{Counter, Histogram, Gauge};

struct ServerMetrics {
    connections_total: Counter<u64>,
    active_rooms: Gauge<u64>,
    message_latency: Histogram<f64>,
}

impl ServerMetrics {
    fn new() -> Self {
        let meter = global::meter("matchbox_signaling");
        Self {
            connections_total: meter.u64_counter("connections_total")
                .with_description("Total WebSocket connections").build(),
            active_rooms: meter.u64_gauge("active_rooms")
                .with_description("Currently active rooms").build(),
            message_latency: meter.f64_histogram("message_latency_seconds")
                .with_description("Message processing latency").build(),
        }
    }
}

```

### OTLP Export Configuration

Configure via environment variables or programmatically:

```bash

OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
OTEL_SERVICE_NAME=matchbox-signaling-server
OTEL_RESOURCE_ATTRIBUTES=deployment.environment=production

```

---

## Logging Best Practices

### Never Log Secrets or PII

```rust

// ❌ Leaks auth tokens into logs
tracing::info!(token = %req.auth_token, "authenticating");

// ❌ Leaks IP addresses (PII under GDPR)
tracing::info!(ip = %addr, "connection from");

// ✅ Log only non-sensitive identifiers
tracing::info!(player_id = %player_id, "authenticating");

// ✅ If IP is needed for security, hash or truncate it
tracing::info!(ip_hash = %hash_ip(&addr), "connection from");

```

### Consistent Field Names Across Spans

```rust

// ✅ Use consistent field names project-wide
// Standardized field names:
//   room_code, player_id, peer_id, session_id
//   error, message_type, transport
//   duration_ms, count, size_bytes

tracing::info!(room_code = %code, player_id = %pid, "player joined");
tracing::info!(room_code = %code, player_id = %pid, "player left");
// Both events can be correlated by room_code and player_id

```

### Log at the Boundary, Not Everywhere

```rust

// ❌ Logging at every level creates noise
async fn join_room(&self, req: JoinRequest) -> Result<(), JoinError> {
    tracing::info!("joining room");           // noise
    let room = self.find_room(&req.code)?;
    tracing::info!("found room");             // noise
    room.add_player(req.player_id)?;
    tracing::info!("added player");           // noise
    Ok(())
}

// ✅ Log at the handler boundary — internal logic uses spans
#[tracing::instrument(skip(self), fields(room_code = %req.room_code))]
async fn handle_join(&self, req: JoinRequest) -> Result<Json<Response>, AppError> {
    let result = self.join_room(req).await;
    if let Err(ref e) = result {
        tracing::error!(error = %e, "join failed");
    }
    result.map(Json)
}

```

### Performance: Avoid Expensive Log Computations

```rust

// Use debug format (cheaper) instead of serialization in log fields
tracing::debug!(state = ?large_state, "state snapshot");

// Guard expensive computation behind level check
if tracing::enabled!(tracing::Level::TRACE) {
    let snapshot = compute_expensive_snapshot();
    tracing::trace!(snapshot = %snapshot, "detailed state");
}

```

---

## Agent Checklist

- [ ] All logging uses `tracing` macros — no `println!`, `eprintln!`, or `log` crate
- [ ] Structured fields on all events — no string interpolation for variable data
- [ ] `#[instrument]` on public async functions with appropriate `skip` list
- [ ] Consistent field names across the codebase (room_code, player_id, etc.)
- [ ] No secrets, tokens, or PII in log output
- [ ] Errors logged once at the handler boundary, not at every intermediate step
- [ ] JSON output configured for production deployments
- [ ] Log levels set appropriately (error/warn for problems, info for events, debug/trace for detail)
- [ ] OpenTelemetry metrics exported for key server operations
- [ ] Expensive computations guarded behind level checks

---

## Related Skills

- [error-handling-guide](./error-handling-guide.md) — Structured error logging patterns
- [async-Rust-best-practices](./async-rust-best-practices.md) — Tracing spans for async functions
- [defensive-programming](./defensive-programming.md) — Logging at system boundaries
