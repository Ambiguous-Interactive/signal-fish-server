# Skill: Async Rust Best Practices

<!--
  trigger: async, await, tokio, spawn, channel, select, concurrency, cancellation
  | Working with tokio, channels, async code, or concurrency
  | Performance
-->

**Trigger**: When writing or modifying any async code, tokio tasks, channels, or concurrent data access.

---

## When to Use

- Writing async functions or spawning tokio tasks
- Using channels (bounded/unbounded) for message passing
- Working with `tokio::select!` or cancellation
- Implementing graceful shutdown patterns
- Debugging deadlocks or async performance issues

---

## When NOT to Use

- Synchronous-only code paths with no I/O
- Pure data structure design (see [Rust-idioms-and-patterns](./rust-idioms-and-patterns.md))
- CPU-bound computation that should use `spawn_blocking`

---

## TL;DR

- Use async for I/O-bound work, `spawn_blocking` for CPU-bound work.
- Always use bounded channels with backpressure — never unbounded.
- Never hold a `std::sync::Mutex` guard across an `.await` point.
- Use `tokio::select!` with cancellation-safe futures only.
- Implement graceful shutdown with broadcast channels or cancellation tokens.

---

## Choosing Async vs Threads

| Workload | Use | Reason |
|----------|-----|--------|
| WebSocket I/O | `async` / `tokio::spawn` | I/O-bound, many concurrent connections |
| Database queries | `async` | I/O-bound, waiting on network |
| Hashing / crypto | `spawn_blocking` | CPU-bound, would block the executor |
| JSON serialization (large) | `spawn_blocking` | CPU-bound if payload is large |
| File I/O | `tokio::fs` or `spawn_blocking` | OS file I/O can block |

```rust
// ✅ CPU-bound work on blocking thread pool
let hash = tokio::task::spawn_blocking(move || {
    compute_expensive_hash(&data)
}).await?;

// ❌ CPU-bound work blocking the async runtime
let hash = compute_expensive_hash(&data);  // Blocks executor thread!
```

---

## Bounded Channels with Backpressure

```rust
// ✅ Bounded channel — provides backpressure
let (tx, mut rx) = tokio::sync::mpsc::channel::<Message>(1024);

// Sender blocks when buffer is full — this is correct backpressure
tx.send(msg).await?;

// ✅ Use try_send for non-blocking with explicit overflow handling
match tx.try_send(msg) {
    Ok(()) => {},
    Err(TrySendError::Full(msg)) => {
        tracing::warn!("Channel full, dropping message");
        metrics.dropped_messages.increment(1);
    }
    Err(TrySendError::Closed(_)) => return Err(Error::ChannelClosed),
}
```

**Channel sizing guidance:**

| Use case | Capacity |
|----------|----------|
| Player message relay | 256–1024 |
| Admin commands | 16–64 |
| Metrics/telemetry | 1024–4096 |
| Shutdown signals | 1 (use broadcast) |

---

## Never Hold Locks Across `.await`

```rust
// ❌ DEADLOCK: guard held across .await
let guard = self.state.lock().unwrap();
let result = database.query(&guard.key).await;  // .await while holding guard!

// ✅ Copy data out, drop the guard, then await
let key = {
    let guard = self.state.lock().expect("SAFETY: lock should not be poisoned");
    guard.key.clone()
};  // guard dropped here
let result = database.query(&key).await;

// ✅ Use DashMap for concurrent read/write access — no lock needed
let rooms: DashMap<RoomId, Room> = DashMap::new();
rooms.insert(room_id, room);
```

**Watch out:** `if let Some(x) = mutex.lock().await.get(&key)` keeps the guard alive through the
entire `if let` block. Extract the value first: `let val = mutex.lock().await.get(&key).cloned();`

---

## Cancellation Safety with `tokio::select!`

```rust
// ✅ Use cancellation-safe operations in select!
tokio::select! {
    msg = rx.recv() => {                        // recv() is cancellation-safe
        if let Some(msg) = msg { handle(msg).await; }
    }
    _ = tokio::time::sleep(Duration::from_secs(30)) => { handle_timeout().await; }
    _ = shutdown.cancelled() => { return Ok(()); }
}

// ❌ read_exact is NOT cancellation-safe — partial reads are lost
tokio::select! {
    result = stream.read_exact(&mut buf) => { ... }  // Data loss if cancelled!
    _ = shutdown.cancelled() => { return; }
}
```

**Cancellation-safe:** `recv()`, `oneshot`, `sleep()`, `accept()`.
**NOT safe:** `read_exact()`, `read_to_end()`, most streaming mid-read.

---

## Structured Concurrency with JoinSet

```rust
use tokio::task::JoinSet;

let mut set = JoinSet::new();
for player in players {
    set.spawn(async move { notify_player(player).await });
}

while let Some(result) = set.join_next().await {
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!("Notification failed: {e}"),
        Err(e) => tracing::error!("Task panicked: {e}"),
    }
}
// All tasks complete here — no leaked futures
```

---

## `spawn` vs `spawn_blocking`

```rust
// ✅ tokio::spawn — for async I/O-bound tasks
tokio::spawn(async move { handle_websocket(stream).await });

// ✅ tokio::task::spawn_blocking — for sync CPU-bound work
let result = tokio::task::spawn_blocking(move || {
    serde_json::to_string(&large_state)
}).await?;
```

---

## Graceful Shutdown

```rust
use tokio_util::sync::CancellationToken;

async fn run_server(shutdown: CancellationToken) -> std::io::Result<()> {
    let listener = TcpListener::bind("0.0.0.0:3536").await?;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let token = shutdown.child_token();
                        tokio::spawn(async move { handle_connection(stream, token).await });
                    }
                    Err(error) => {
                        tracing::warn!("Accept failed: {error}");
                    }
                }
            }
            _ = shutdown.cancelled() => { tracing::info!("Shutting down"); break; }
        }
    }
    Ok(())
}
```

Alternative: use `tokio::sync::broadcast` channels for shutdown signals.
This project may use alternative approaches — check the actual codebase implementation.

---

## Async Trait Methods

```rust
// Native async trait (preferred for generics, Rust 1.75+)
trait Database: Send + Sync {
    async fn get_room(&self, id: &RoomId) -> Result<Option<Room>, DbError>;
}

// async-trait for dyn dispatch: Box<dyn DatabaseDyn>
#[async_trait]
trait DatabaseDyn: Send + Sync {
    async fn get_room(&self, id: &RoomId) -> Result<Option<Room>, DbError>;
}
```

This project uses `async_trait` for trait objects.

---

## Timeout Patterns

```rust
use tokio::time::{timeout, Duration};

// ✅ Timeout on any async operation
match timeout(Duration::from_secs(5), database.query(&key)).await {
    Ok(Ok(result)) => use_result(result),
    Ok(Err(db_err)) => return Err(db_err.into()),
    Err(_elapsed) => return Err(Error::Timeout),
}
```

---

## Agent Checklist

- [ ] All channels are bounded with explicit capacity
- [ ] No `std::sync::Mutex` guards held across `.await` points
- [ ] CPU-bound work dispatched via `spawn_blocking`
- [ ] `tokio::select!` uses only cancellation-safe branches
- [ ] Graceful shutdown implemented with appropriate cancellation mechanism
- [ ] Timeouts on all external I/O (database, network, WebSocket)
- [ ] `JoinSet` used for structured spawning with cleanup
- [ ] No `std::thread::sleep` in async code
- [ ] No `std::fs` in async code (use `tokio::fs`)
- [ ] `#[tokio::test]` used for async tests

---

## Related Skills

- [Rust-performance-optimization](./rust-performance-optimization.md) — Allocation reduction and profiling
- [error-handling-guide](./error-handling-guide.md) — Async error propagation patterns
- [observability-and-logging](./observability-and-logging.md) — Tracing spans for async functions
- [WebSocket-protocol-patterns](./websocket-protocol-patterns.md) — Async WebSocket handling
