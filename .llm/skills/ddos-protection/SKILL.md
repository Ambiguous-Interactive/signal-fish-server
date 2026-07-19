---
name: ddos-protection
description: >-
  Design application and infrastructure protections against denial-of-service attacks, including
  rate limits, connection controls, load shedding, and monitoring. Use when changing abuse
  prevention, WebSocket limits, backpressure, or DDoS operations.
---

# DDoS Prevention — Rate Limiting and Connection Management

## When to Use

- Adding or modifying rate limiting middleware or connection caps
- Implementing WebSocket message throttling or frame size limits
- Configuring per-IP connection tracking or idle timeouts
- Preventing slow-loris and unbounded channel attacks

## When NOT to Use

- Infrastructure-layer DDoS protection (see [DDoS Infrastructure Monitoring](references/infrastructure-and-monitoring.md))
- General authentication/authorization (see [Web Service Security Auth](../web-service-security/SKILL.md))
- Non-security performance work (see [Rust Performance Optimization](../rust-performance-optimization/SKILL.md))

---

## Rationalizations to Reject

| Excuse | Why It's Wrong | Required Action |
|--------|---------------|-----------------|
| "We don't have enough traffic to worry" | Attackers target small services precisely because they lack protection. | Implement baseline rate limiting and connection caps from day one. |
| "Rate limiting hurts legitimate users" | Properly tuned limits are invisible to legitimate users. Only abusers hit them. | Set limits at 5–10× expected peak. Monitor and adjust, never remove. |
| "CloudFront handles DDoS for us" | CDN absorbs volumetric L3/L4 floods but cannot protect against application-layer abuse. | Layer application-level defenses behind the CDN. Defense in depth. |
| "Rust is fast enough to absorb it" | Speed doesn't prevent OOM from unbounded allocations or CPU exhaustion from algorithmic abuse. | Bound every resource: connections, messages, allocations, queue depths. |

---

## Defense-in-Depth Layers

| Layer | Component | Protection | Crate / Service |
|-------|-----------|------------|-----------------|
| 1. Edge / CDN | CloudFront | Volumetric L3/L4 absorption, geo-blocking | AWS CloudFront |
| 2. WAF | AWS WAF | Rate-based rules, IP reputation, bot control | AWS WAF |
| 3. Network | Security Groups | Restrict ingress to CloudFront IP ranges only | AWS VPC |
| 4. Transport | TLS termination | Protocol validation, handshake limits | `rustls` |
| 5. HTTP | axum middleware | Request body limits, header validation, CORS | `tower-http` |
| 6. Connection | Semaphore + tracker | Per-IP caps, global ceiling, idle timeout | `tokio`, `dashmap` |
| 7. WebSocket | Frame/message limits | Message rate, frame size, ping/pong, backpressure | `tokio-tungstenite` |
| 8. Business Logic | Application code | Room caps, peer caps, bounded deserialization | `serde`, app logic |

---

## 1. Rate Limiting Strategies

### Governor Crate (GCRA Algorithm)

Use `governor` for per-IP rate limiting with the GCRA (Generic Cell Rate Algorithm):

```rust
use tower_governor::{GovernorConfigBuilder, GovernorLayer};
use std::num::NonZeroU32;

let gov_conf = GovernorConfigBuilder::default()
    .per_second(NonZeroU32::new(10).unwrap())   // 10 req/s per IP
    .burst_size(NonZeroU32::new(25).unwrap())    // burst allowance
    .key_extractor(SmartIpKeyExtractor)          // X-Forwarded-For aware
    .finish()
    .unwrap();

let app = Router::new()
    .route("/ws", get(ws_handler))
    .layer(GovernorLayer { config: Arc::new(gov_conf) });
```

### Tiered Rate Limits

Apply three tiers — reject at the most specific level first:

| Tier | Scope | Limit | Purpose |
|------|-------|-------|---------|
| Per-IP | `IpAddr` | 10 conn/s, 100 conn/min | Prevent single-source floods |
| Per-User | `AppId` | 100 req/min, 5K req/hr | Fair multi-tenant allocation |
| Global | Server-wide | 5K conn total | Protect server resources |

### Return 429 with Retry-After

Always include `Retry-After` so well-behaved clients back off:

```rust
use axum::http::{StatusCode, HeaderMap, HeaderValue};

fn rate_limit_response(retry_after_secs: u64) -> (StatusCode, HeaderMap) {
    let mut headers = HeaderMap::new();
    headers.insert("retry-after", HeaderValue::from(retry_after_secs));
    headers.insert("x-ratelimit-remaining", HeaderValue::from(0));
    (StatusCode::TOO_MANY_REQUESTS, headers)
}
```

### Anti-Pattern: Unbounded Rate Limiter State

```rust
// BAD: Grows without bound — attacker sends from millions of spoofed IPs
let limiters: HashMap<IpAddr, RateLimiter> = HashMap::new();

// GOOD: Bounded with TTL eviction — governor handles this internally,
// or use DashMap with periodic cleanup capped at MAX_TRACKED_IPS
const MAX_TRACKED_IPS: usize = 100_000;
```

---

## 2. Connection Management

### Per-IP Connection Tracker

Limit concurrent connections per IP (3–5 for signaling is sufficient):

```rust
use dashmap::DashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::net::IpAddr;

struct ConnectionTracker {
    counts: DashMap<IpAddr, AtomicU32>,
    max_per_ip: u32, // 5
}

impl ConnectionTracker {
    fn try_acquire(&self, ip: IpAddr) -> bool {
        let counter = self.counts.entry(ip).or_insert(AtomicU32::new(0));
        let prev = counter.fetch_add(1, Ordering::Relaxed);
        if prev >= self.max_per_ip {
            counter.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        true
    }
    fn release(&self, ip: IpAddr) {
        if let Some(c) = self.counts.get(&ip) {
            if c.fetch_sub(1, Ordering::Relaxed) <= 1 { self.counts.remove(&ip); }
        }
    }
}
```

### Global Connection Ceiling with Semaphore

```rust
use tokio::sync::Semaphore;

const MAX_CONNECTIONS: usize = 10_000;
let semaphore = Arc::new(Semaphore::new(MAX_CONNECTIONS));

async fn ws_handler(
    ws: WebSocketUpgrade, State(sem): State<Arc<Semaphore>>,
) -> Result<impl IntoResponse, StatusCode> {
    let permit = sem.try_acquire_owned()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(ws.on_upgrade(move |socket| async move {
        handle_socket(socket).await;
        drop(permit);
    }))
}
```

### Idle Timeout

Disconnect clients that send no data within the timeout window:

```rust
use tokio::time::{timeout, Duration};

const IDLE_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes

loop {
    match timeout(IDLE_TIMEOUT, receiver.next()).await {
        Ok(Some(Ok(msg))) => handle_message(msg).await,
        Ok(Some(Err(_))) | Ok(None) => break, // error or disconnect
        Err(_) => {                            // timeout elapsed
            tracing::info!(peer = %addr, "Idle timeout — disconnecting");
            break;
        }
    }
}
```

### Slow-Loris Prevention

Set header read timeouts at the `hyper` level to prevent slow-loris attacks:

```rust
use hyper_util::server::conn::auto::Builder;

let builder = Builder::new(TokioExecutor::new());
builder.http1()
    .header_read_timeout(Duration::from_secs(5))   // 5s to send headers
    .keep_alive(false);                              // no HTTP keep-alive
```

---

## 3. WebSocket-Specific DDoS Prevention

### Per-Connection Message Rate Limiting

Throttle messages per connection, differentiated by message type:

```rust
use governor::{RateLimiter, Quota, clock::DefaultClock, state::{InMemoryState, NotKeyed}};
use std::num::NonZeroU32;

struct PerConnectionLimits {
    signal: RateLimiter<NotKeyed, InMemoryState, DefaultClock>,   // 30/s
    join:   RateLimiter<NotKeyed, InMemoryState, DefaultClock>,   // 2/s
    chat:   RateLimiter<NotKeyed, InMemoryState, DefaultClock>,   // 10/s
}
```

### Frame and Message Size Limits

```rust
// axum WebSocketUpgrade configuration
ws.max_frame_size(16_384)      // 16 KB per frame
  .max_message_size(65_536)    // 64 KB per message
  .on_upgrade(move |socket| handle_socket(socket, state))
```

### Ping/Pong with Strict Pong Timeout

```rust
const PING_INTERVAL: Duration = Duration::from_secs(15);
const PONG_TIMEOUT: Duration = Duration::from_secs(10);

async fn heartbeat_loop(sender: &mut SplitSink<WebSocket, Message>) -> bool {
    if sender.send(Message::Ping(vec![1, 2, 3, 4])).await.is_err() {
        return false;
    }
    // Pong must arrive within PONG_TIMEOUT — checked in the recv loop
    true
}
// In the recv loop: if Instant::now() - last_pong > PING_INTERVAL + PONG_TIMEOUT { break; }
```

### Backpressure with Bounded Channels

Never use unbounded channels for outgoing messages:

```rust
// BAD: Unbounded — OOM if client stops reading
let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

// GOOD: Bounded — applies backpressure, drops slow clients
let (tx, rx) = tokio::sync::mpsc::channel(32);
if tx.try_send(msg).is_err() {
    tracing::warn!(peer = %addr, "Outbound buffer full — disconnecting slow client");
    break; // close the connection
}
```

---

## Quick Reference

| Protection | Setting | Value |
|------------|---------|-------|
| Per-IP rate limit | `governor` GCRA | 10 req/s, burst 25 |
| Concurrent connections per IP | `ConnectionTracker` | 3–5 |
| Global connection ceiling | `Semaphore` | 10,000 |
| WebSocket frame size | `max_frame_size` | 16 KB |
| WebSocket message size | `max_message_size` | 64 KB |
| Outbound queue depth | `mpsc::channel` | 32 |
| Idle timeout | `tokio::time::timeout` | 300s (5 min) |
| Ping interval | Heartbeat loop | 15s |
| Pong timeout | Recv loop check | 10s |
| Header read timeout | `hyper` builder | 5s |
| 429 response | Always include | `Retry-After` header |

---

## Key Crates

| Crate | Purpose | Key API |
|-------|---------|---------|
| `tower-governor` | GCRA rate limiting (wraps `governor`) | `GovernorConfigBuilder`, `GovernorLayer` |
| `tower` | Middleware layers | `RateLimitLayer`, `ConcurrencyLimitLayer` |
| `tower-http` | HTTP-specific layers | `RequestBodyLimitLayer`, `CorsLayer` |
| `dashmap` | Concurrent hash map | `DashMap<IpAddr, AtomicU32>` for connection tracking |
| `tokio` | Async runtime | `Semaphore`, `timeout`, `signal` |
| `tokio-tungstenite` | WebSocket protocol | `max_frame_size`, `max_message_size` |

---

## Related Skills

- [DDoS Infrastructure Monitoring](references/infrastructure-and-monitoring.md) — WAF, monitoring, graceful degradation
- [Web Service Security Auth](../web-service-security/SKILL.md) — Authentication, authorization, input validation, TLS
- [WebSocket Protocol Patterns](../websocket-protocol-patterns/SKILL.md) — WebSocket lifecycle, message design, heartbeat
- [Observability And Logging](../observability-and-logging/SKILL.md) — Metrics emission, tracing, anomaly alerting
