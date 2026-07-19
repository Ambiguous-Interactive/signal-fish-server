# DDoS Prevention — Application Protection, Infrastructure, and Monitoring

---

## When to Use

- Adding load shedding, circuit breakers, or degradation levels
- Configuring infrastructure-layer DDoS protection (WAF, Shield, CloudFront)
- Setting up monitoring and alerting for abuse detection
- Reviewing code for unbounded allocations controlled by external input

## When NOT to Use

- Rate limiting middleware or connection caps
  (see [DDoS Rate Limiting Connections](../SKILL.md))
- General authentication/authorization (see [Web Service Security Auth](../../web-service-security/SKILL.md))

---

## 4. Application-Layer Protection

### Bounded Deserialization

Prevent attackers from sending unexpected fields or oversized payloads:

```rust
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignalMessage {
    pub target: PeerId,           // newtype, validated
    #[serde(deserialize_with = "bounded_sdp")]
    pub sdp: String,              // max 8 KB
}

fn bounded_sdp<'de, D: serde::Deserializer<'de>>(de: D) -> Result<String, D::Error> {
    let s = String::deserialize(de)?;
    if s.len() > 8_192 { return Err(serde::de::Error::custom("SDP too large")); }
    Ok(s)
}
```

### Computational Complexity Caps

Hard-cap resources to prevent amplification attacks:

```rust
const MAX_PEERS_PER_ROOM: usize = 64;
const MAX_ROOMS_PER_USER: usize = 5;
const MAX_ROOMS_TOTAL: usize = 10_000;

fn join_room(&self, user: &UserId, room: &RoomId) -> Result<(), JoinError> {
    if self.rooms.get(room).map_or(0, |r| r.len()) >= MAX_PEERS_PER_ROOM {
        return Err(JoinError::RoomFull);
    }
    if self.user_rooms(user).count() >= MAX_ROOMS_PER_USER {
        return Err(JoinError::TooManyRooms);
    }
    Ok(())
}
```

### Never Let User Input Control Allocation Sizes

```rust
// BAD: Attacker sends count=999999999 → OOM
let items: Vec<Item> = Vec::with_capacity(user_request.count);

// GOOD: Clamp to a safe maximum before allocating
let count = user_request.count.min(MAX_ITEMS);
let items: Vec<Item> = Vec::with_capacity(count);
```

### Upgrade Handshake Validation

Validate Origin header before accepting WebSocket upgrades.
Version negotiation (Sec-WebSocket-Version: 13) is handled automatically by axum/tungstenite.

```rust
async fn validate_origin(headers: &HeaderMap) -> Result<(), StatusCode> {
    let origin = headers.get("origin")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::FORBIDDEN)?;
    if !ALLOWED_ORIGINS.contains(&origin) { return Err(StatusCode::FORBIDDEN); }
    Ok(())
}
```

---

## 5. Infrastructure-Layer Protection

### AWS WAF Rate-Based Rules

Configure rate-based rules specifically for WebSocket upgrade requests:

```json
{
  "Name": "ws-upgrade-rate-limit",
  "Priority": 1,
  "Action": { "Block": {} },
  "Statement": {
    "RateBasedStatement": {
      "Limit": 100,
      "AggregateKeyType": "IP",
      "ScopeDownStatement": {
        "ByteMatchStatement": {
          "FieldToMatch": { "SingleHeader": { "Name": "upgrade" } },
          "PositionalConstraint": "EXACTLY",
          "SearchString": "websocket"
        }
      }
    }
  }
}
```

### Infrastructure Checklist

| Layer | Action | Purpose |
|-------|--------|---------|
| AWS Shield Standard | Enable (free, automatic) | Volumetric L3/L4 protection |
| CloudFront | Place in front of ALB | Absorb edge floods, cache static assets |
| Security Groups | Restrict to CloudFront IPs only | Prevent direct-to-origin attacks |
| WAF | Rate-based + IP reputation rules | Application-layer filtering |
| Geo-blocking | Block non-applicable regions | Reduce attack surface |

---

## 6. Monitoring and Detection

### Key Metrics

Emit these counters and gauges for DDoS detection:

> **Note:** Examples use the `metrics` crate API. If the project uses `opentelemetry`, adapt to its Meter API.

```rust
use metrics::{counter, gauge, histogram};

counter!("connections.total").increment(1);
counter!("connections.rejected", "reason" => "rate_limit").increment(1);
gauge!("connections.active").set(active_count as f64);
counter!("messages.received", "type" => msg_type).increment(1);
counter!("rate_limit.rejected", "tier" => "per_ip").increment(1);
histogram!("message.processing_time_ms").record(elapsed.as_millis() as f64);
```

### Progressive Defense Escalation

Implement three escalation levels triggered by metric thresholds:

| Level | Trigger | Action |
|-------|---------|--------|
| 1. Alert | Rejection rate > 5% for 2 min | Page on-call, increase logging verbosity |
| 2. Throttle | Rejection rate > 20% for 5 min | Halve rate limits, enable aggressive IP blocking |
| 3. Shed | Active connections > 80% ceiling | Reject new connections, drain lowest-priority sessions |

### Circuit Breakers

Wrap downstream calls (DB, Redis, auth service) in a circuit breaker.
Open after N consecutive failures; half-open after a recovery interval. Use `AtomicU8` for lock-free state tracking.

---

## 7. Graceful Degradation

Implement three degradation levels: **Healthy** (all features), **Degraded** (non-essential disabled),
**Critical** (reject new connections, drain existing).

```rust
#[repr(u8)]
enum DegradationLevel { Healthy = 0, Degraded = 1, Critical = 2 }

async fn health_check(State(health): State<Arc<ServerHealth>>) -> impl IntoResponse {
    match health.current() {
        DegradationLevel::Healthy  => (StatusCode::OK, "healthy"),
        DegradationLevel::Degraded => (StatusCode::OK, "degraded"),
        DegradationLevel::Critical => (StatusCode::SERVICE_UNAVAILABLE, "critical"),
    }
}
```

On `SIGTERM`, stop accepting new connections and drain with
`axum::serve(...).with_graceful_shutdown(shutdown_signal())`.

---

## Quick Reference

| Protection | Setting | Value |
|------------|---------|-------|
| HTTP body limit | `DefaultBodyLimit` | 16 KB |
| SDP payload max | Custom deserializer | 8 KB |
| Peers per room | Application cap | 64 |
| Rooms per user | Application cap | 5 |

---

## Agent Checklist

- [ ] `#[serde(deny_unknown_fields)]` on all deserialized message types
- [ ] User-controlled sizes clamped before allocation
- [ ] Origin header validated before WebSocket upgrade
- [ ] AWS Shield Standard enabled
- [ ] CloudFront placed in front of ALB
- [ ] WAF rate-based rules configured for WebSocket upgrades
- [ ] Metrics emitted for rejection rates and active connections
- [ ] Degradation levels defined (70%/85%/95% thresholds)
- [ ] Circuit breakers wrap all external dependencies

---

## Related Skills

- [DDoS Rate Limiting Connections](../SKILL.md) —
  Rate limiting, connection caps, WebSocket throttling
- [Graceful Degradation Deployment](../../graceful-degradation/SKILL.md) — Full graceful degradation and
  circuit breaker patterns
- [Web Service Security Auth](../../web-service-security/SKILL.md) — Authentication, authorization, input
  validation, TLS
- [Observability And Logging](../../observability-and-logging/SKILL.md) — Metrics emission, tracing, anomaly alerting
- [Rust Performance Optimization](../../rust-performance-optimization/SKILL.md) — Bounded allocations, zero-copy, profiling
