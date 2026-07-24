---
name: websocket-protocol-patterns
description: >-
  Apply project guidance for WebSocket protocol patterns. Use when working with WebSocket
  handlers, message protocol design, or broadcast patterns.
---

# WebSocket Protocol Patterns

---

## When to Use

- Implementing WebSocket connection lifecycle (upgrade, auth, close)
- Designing message types with serde tagging
- Building room broadcast with backpressure
- Handling ping-pong heartbeat and timeouts
- Testing WebSocket connections with tokio-tungstenite

---

## When NOT to Use

- General HTTP API endpoints (see [API Design Guidelines](../api-design-guidelines/SKILL.md))
- Generic async patterns (see [Async Rust Best Practices](../async-rust-best-practices/SKILL.md))

---

## TL;DR

- Handle the full WebSocket lifecycle: upgrade → authenticate → heartbeat → graceful close.
- Use enum-based messages with `#[serde(tag = "type")]` for extensible, type-safe protocols.
- Broadcast with `Bytes` for zero-copy fan-out to room participants.
- Apply backpressure on slow clients — drop or disconnect rather than buffer unboundedly.
- Test WebSocket handlers with `tokio-tungstenite` in integration tests.

---

## Connection Lifecycle

### Upgrade Handling in axum

```rust
async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(server): State<Arc<GameServer>>,
    Query(params): Query<ConnectParams>,
) -> impl IntoResponse {
    // Validate before upgrading — reject early if auth fails
    ws.on_upgrade(move |socket| handle_connection(socket, server, params))
}

async fn handle_connection(socket: WebSocket, server: Arc<GameServer>, params: ConnectParams) {
    // Requires: use futures_util::{SinkExt, StreamExt};
    let (mut sender, mut receiver) = socket.split();
}
```

### Authentication During Connection

```rust
async fn authenticate(
    receiver: &mut SplitStream<WebSocket>, server: &GameServer,
) -> Result<PlayerId, AuthError> {
    let auth_msg = tokio::time::timeout(Duration::from_secs(5), receiver.next())
        .await.map_err(|_| AuthError::Timeout)?
        .ok_or(AuthError::Disconnected)?
        .map_err(|_| AuthError::ProtocolError)?;

    match auth_msg {
        Message::Text(text) => {
            let auth: AuthMessage = serde_json::from_str(&text)
                .map_err(|_| AuthError::InvalidMessage)?;
            server.verify_token(&auth.token).await
        }
        _ => Err(AuthError::InvalidMessage),
    }
}
```

### Heartbeat / Ping-Pong

Server transport probes are idle-only. Maintain one bounded/coalescing activity
generation and at most one active unpredictable nonce:

1. Immediately after decoding any non-Pong frame, publish transport activity
   before parsing, metrics, or awaited application handling.
2. At a scheduled tick, skip the probe if the generation changed during the
   preceding interval.
3. Recheck the generation when the writer begins, because the Ping command may
   have waited behind another socket write.
4. Once written, accept only the exact matching Pong. A decoded non-Pong frame
   cancels the probe as fresh activity; wrong, stale, or unsolicited Pongs do
   neither. Silence through the deadline closes `4003 activity_timeout`.

Use O(1) watch/state-machine storage, keep application handling sequential, and
never insert an unbounded transport-to-application queue. Awaiting application
work in the receive loop is safe only because activity is published first.

### Graceful Disconnection

Use a `CleanupGuard` (RAII) to ensure server state cleanup runs even on panic or early return:

```rust
struct CleanupGuard { player_id: PlayerId, server: Arc<GameServer> }
impl Drop for CleanupGuard {
    fn drop(&mut self) {
        let (server, pid) = (self.server.clone(), self.player_id);
        tokio::spawn(async move { server.remove_player(pid).await; });
    }
}

let _cleanup = CleanupGuard { player_id, server: server.clone() };
connection_loop(sender, receiver, server.clone()).await;
```

Use timeouts at every stage: upgrade (10s), auth (5s), idle (300s).

---

## Message Design

### JSON vs MessagePack

This project supports both JSON and MessagePack (via `rmp-serde`).
Dispatch on `WireFormat` to encode/decode with `serde_json` or `rmp_serde`.

### Enum-Based Message Types with Serde Tagging

```rust
// ✅ Internally tagged — each message carries its type as a field
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    JoinRoom { code: String },
    LeaveRoom,
    Offer { sdp: String, target: PlayerId },
    Answer { sdp: String, target: PlayerId },
    IceCandidate { candidate: String, target: PlayerId },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Welcome { player_id: PlayerId },
    PeerJoined { peer_id: PlayerId },
    PeerLeft { peer_id: PlayerId },
    Error { code: String, message: String },
}
```

Include a `version: u32` field in the handshake for forward compatibility.
Use route versioning (`/v2/ws`, `/v1/ws`) for breaking protocol changes.
Use `Bytes` for zero-copy sharing of binary relay data.

---

## Broadcast and Fan-out

```rust
impl RoomHandle {
    /// Broadcast to all players except the sender; use try_send for backpressure
    async fn broadcast_except(&self, from: PlayerId, msg: Bytes) {
        for entry in self.players.iter() {
            if *entry.key() != from {
                if entry.value().try_send(msg.clone()).is_err() {
                    tracing::warn!(peer_id = %entry.key(), "slow client — dropped");
                }
            }
        }
    }
}
```

`Bytes::clone()` is O(1) (reference-counted).
Always use bounded `mpsc` channels per client — drop or disconnect slow receivers rather than buffering unboundedly.

---

## Error Handling in WebSocket Contexts

Treat disconnection as normal, not an error:

```rust
match receiver.next().await {
    Some(Ok(msg)) => process(msg).await,
    Some(Err(e)) => { tracing::debug!(error = %e, "connection error"); break; }
    None => { tracing::info!(player_id = %pid, "client disconnected"); break; }
}
```

See [Reconnection Protocol](../../../docs/adr/reconnection-protocol.md) for reconnection with session tokens.

### Close Frame Reasons

| Code | Meaning |
|------|---------|
| 1000 | Normal closure |
| 4001 | Auth failed |
| 4002 | Room full |
| 4003 | Kicked |
| 4004 | Server shutdown |
| 4005 | Rate limited |

---

## Testing WebSocket Code

```rust
#[tokio::test]
async fn test_join_and_peer_signaling() {
    let server = TestServer::start().await;
    let url = format!("ws://{}/v2/ws", server.addr());
    let (mut ws1, _) = connect_async(&url).await.unwrap();
    let (mut ws2, _) = connect_async(&url).await.unwrap();

    let join = |code: &str| serde_json::json!({"type": "join_room", "code": code});
    ws1.send(Message::Text(join("ROOM01").to_string().into())).await.unwrap();
    ws2.send(Message::Text(join("ROOM01").to_string().into())).await.unwrap();

    // Use timeouts to avoid hanging tests
    let msg = tokio::time::timeout(Duration::from_secs(2), ws2.next())
        .await.expect("timed out").expect("stream ended").expect("message error");
    let parsed: serde_json::Value = serde_json::from_str(msg.to_text().expect("text")).expect("json");
    assert_eq!(parsed["type"], "peer_joined");
}
```

For load tests, measure: connections/sec, message throughput, P50/P95/P99 latency, memory per connection.

---

## Socket-level latency (Nagle and batching)

Real-time relay frames are small and latency-sensitive. Two defaults protect them:

- **`TCP_NODELAY` on every accepted socket.** Nagle's algorithm plus delayed ACK
  can stall small bidirectional frames ~40-90 ms on loopback. `TCP_NODELAY` is
  per-connection and not reliably inherited from the listen socket, so set it on
  each accepted stream. Both serve paths funnel through one seam
  (`websocket::bind_serve_listener` for plain `axum::serve`,
  `websocket::ConfiguredAcceptor` for the TLS stack), so tests and production
  share identical socket semantics. A WebSocket sink flush does NOT disable
  Nagle — the option must be set explicitly.
- **Outbound batching is opt-in.** The batch timer holds a frame up to
  `batch_interval_ms`; `enable_batching` is `false` by default so real-time
  traffic is never delayed. When enabled for throughput, only
  `DeliveryClass::Latest` waits to coalesce — control, `Reliable`, and
  `Volatile` are released immediately.

## Agent Checklist

- [ ] Accepted sockets set `TCP_NODELAY` (via `bind_serve_listener` / `ConfiguredAcceptor`) so real-time frames dodge Nagle x delayed-ACK stalls
- [ ] The outbound batch timer never holds latency-sensitive traffic — batching is opt-in and only `DeliveryClass::Latest` waits to coalesce
- [ ] WebSocket upgrade validates auth before upgrading when possible
- [ ] Heartbeat ping-pong runs at regular intervals with client timeout
- [ ] Graceful close sends a close frame with appropriate code
- [ ] Server state is cleaned up on disconnect (always, including errors)
- [ ] Messages use `#[serde(tag = "type")]` for extensible enums
- [ ] Binary data uses `Bytes` for zero-copy broadcast
- [ ] Broadcast channels are bounded — slow clients get dropped
- [ ] Disconnections logged at `debug`/`info`, not `error`
- [ ] Reconnection restores session state (see ADR-001)
- [ ] Integration tests cover multi-client scenarios with timeouts

---

## Related Skills

- [Async Rust Best Practices](../async-rust-best-practices/SKILL.md) — Async patterns for connection handling
- [API Design Guidelines](../api-design-guidelines/SKILL.md) — Message type design
- [Error Handling Guide](../error-handling-guide/SKILL.md) — WebSocket error codes and handling
- [Observability And Logging](../observability-and-logging/SKILL.md) — Connection lifecycle tracing
