# Skill: WebSocket Protocol Patterns

<!--
  trigger: WebSocket, ws, connection, message, broadcast, heartbeat, close, upgrade
  | WebSocket lifecycle, message design, and broadcast patterns
  | Feature
-->

**Trigger**: When working with WebSocket handlers, message protocol design, or broadcast patterns.

---

## When to Use

- Implementing WebSocket connection lifecycle (upgrade, auth, close)
- Designing message types with serde tagging
- Building room broadcast with backpressure
- Handling ping-pong heartbeat and timeouts
- Testing WebSocket connections with tokio-tungstenite

---

## When NOT to Use

- General HTTP API endpoints (see [api-design-guidelines](./api-design-guidelines.md))
- Generic async patterns (see [async-Rust-best-practices](./async-rust-best-practices.md))

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
use axum::{
    extract::{ws::{WebSocket, WebSocketUpgrade, Message}, State, Query},
    response::IntoResponse,
};
use std::sync::Arc;

async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(server): State<Arc<GameServer>>,
    Query(params): Query<ConnectParams>,
) -> impl IntoResponse {
    // Validate before upgrading — reject early if auth fails
    // (headers and query params are available before upgrade)
    ws.on_upgrade(move |socket| handle_connection(socket, server, params))
}

async fn handle_connection(
    socket: WebSocket,
    server: Arc<GameServer>,
    params: ConnectParams,
) {
    // Requires: use futures_util::{SinkExt, StreamExt};
    let (mut sender, mut receiver) = socket.split();
    // Connection is now established — proceed with auth handshake
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

```rust

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const CLIENT_TIMEOUT: Duration = Duration::from_secs(30);

async fn connection_loop(mut sender: SplitSink<WebSocket, Message>, mut receiver: SplitStream<WebSocket>) {
    let mut heartbeat = interval(HEARTBEAT_INTERVAL);
    let mut last_pong = Instant::now();

    loop {
        tokio::select! {
            msg = receiver.next() => match msg {
                Some(Ok(Message::Pong(_))) => last_pong = Instant::now(),
                Some(Ok(msg)) => handle_message(msg).await,
                Some(Err(_)) | None => break,
            },
            _ = heartbeat.tick() => {
                if last_pong.elapsed() > CLIENT_TIMEOUT { break; }
                if sender.send(Message::Ping(vec![].into())).await.is_err() { break; }
            }
        }
    }
}

```

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
#[non_exhaustive]
pub enum ClientMessage {
    JoinRoom { code: String },
    LeaveRoom,
    Offer { sdp: String, target: PlayerId },
    Answer { sdp: String, target: PlayerId },
    IceCandidate { candidate: String, target: PlayerId },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ServerMessage {
    Welcome { player_id: PlayerId },
    PeerJoined { peer_id: PlayerId },
    PeerLeft { peer_id: PlayerId },
    Offer { sdp: String, from: PlayerId },
    Answer { sdp: String, from: PlayerId },
    IceCandidate { candidate: String, from: PlayerId },
    Error { code: String, message: String },
}

```

### Message Framing and Versioning

Include a `version: u32` field in the handshake for forward compatibility.
Use route versioning (`/v2/ws`, `/v1/ws`) for breaking protocol changes.

### Binary Message Handling

Distinguish text from binary messages. Use `Bytes` for zero-copy sharing of binary relay data.

```rust

match msg {
    Message::Text(text) => handle_signaling(serde_json::from_str(&text)?).await,
    Message::Binary(data) => relay_to_peer(Bytes::from(data)).await,
    Message::Ping(_) | Message::Pong(_) => { /* handled by framework */ }
    Message::Close(_) => break,
}

```

---

## Broadcast and Fan-out

### Room-Based Broadcasting with Backpressure

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

### Handling Disconnections Gracefully

Treat disconnection as normal, not an error:

```rust

match receiver.next().await {
    Some(Ok(msg)) => process(msg).await,
    Some(Err(e)) => { tracing::debug!(error = %e, "connection error"); break; }
    None => { tracing::info!(player_id = %pid, "client disconnected"); break; }
}

```

### Reconnection Protocol

See [ADR-001: Reconnection Protocol](../../docs/adr/reconnection-protocol.md). Support reconnection with session
tokens and server-side replay buffers for message continuity.

### Close Frame Reasons

Use standard WebSocket close codes (1000–1011) plus application-specific codes in the 4000–4999 range:

| Code | Meaning |
|------|---------|
| 1000 | Normal closure |
| 1001 | Going away |
| 4001 | Auth failed |
| 4002 | Room full |
| 4003 | Kicked |
| 4004 | Server shutdown |
| 4005 | Rate limited |

---

## Testing WebSocket Code

### Integration Tests with tokio-tungstenite

```rust
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures::{SinkExt, StreamExt};

#[tokio::test]
async fn test_join_and_peer_signaling() {
    let server = TestServer::start().await;
    let url = format!("ws://{}/v2/ws", server.addr());

    let (mut ws1, _) = connect_async(&url).await.unwrap();
    let (mut ws2, _) = connect_async(&url).await.unwrap();

    // Both join the same room
    let join = |code: &str| serde_json::json!({"type": "join_room", "code": code});
    ws1.send(Message::Text(join("ROOM01").to_string().into())).await.unwrap();
    ws2.send(Message::Text(join("ROOM01").to_string().into())).await.unwrap();

    // Use timeouts to avoid hanging tests
    let msg = tokio::time::timeout(Duration::from_secs(2), ws2.next())
        .await.expect("timed out")
        .expect("stream ended")
        .expect("message error");
    let parsed: serde_json::Value = serde_json::from_str(
        msg.to_text().expect("expected text message")
    ).expect("invalid JSON");
    assert_eq!(parsed["type"], "peer_joined");
}

```

For load tests, measure: connections/sec, message throughput, P50/P95/P99 latency, memory per connection.

---

## Agent Checklist

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

- [async-Rust-best-practices](./async-rust-best-practices.md) — Async patterns for connection handling
- [api-design-guidelines](./api-design-guidelines.md) — Message type design
- [error-handling-guide](./error-handling-guide.md) — WebSocket error codes and handling
- [observability-and-logging](./observability-and-logging.md) — Connection lifecycle tracing
