# Skill: Rust Performance Optimization

<!--
  trigger: performance, allocation, profiling, benchmark, cache, zero-copy, smallvec, dashmap
  | Optimizing hot paths, reducing allocations, and profiling
  | Performance
-->

**Trigger**: When optimizing hot paths, reducing allocations, or profiling performance-critical code.

---

## When to Use

- Reducing heap allocations in hot paths
- Choosing between collection types (`SmallVec`, `DashMap`, etc.)
- Configuring release profiles or alternative allocators
- Profiling with `criterion`, `flamegraph`, or `perf`
- Optimizing string handling or serialization

---

## When NOT to Use

- Premature optimization before profiling
- Code correctness issues (fix bugs first, then optimize)
- API design decisions (see [api-design-guidelines](./api-design-guidelines.md))

---

## TL;DR

- Use `with_capacity()` for all collections where the size is known or estimable.
- Prefer `SmallVec`, `Bytes`, and `Arc<str>` over heap-heavy alternatives.
- Use `DashMap`/`FxHashMap` over `HashMap` in hot paths.
- Profile before optimizing — use `criterion` for benchmarks, `flamegraph` for profiling.
- Avoid cloning in hot paths; use `Bytes` for zero-copy network data.

---

## Release Profile Configuration

This project's [Cargo.toml](../../Cargo.toml) already has optimized release profiles (`lto = "thin"`,
`codegen-units = 1`, `strip = true`, `opt-level = 3` for deps).
Use `lto = "fat"` only if benchmarks show measurable gain.
Consider `panic = "abort"` for production binaries (smaller binary, no unwind overhead).

---

## Alternative Allocators

Consider `tikv-jemallocator` (multi-threaded server workloads) or `mimalloc` (good cross-platform default).
Neither is currently in project dependencies.
Benchmark before committing — the default allocator is often fine for I/O-bound servers.

```rust
#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

```

---

## Heap Allocation Reduction

### Pre-allocation with `with_capacity`

```rust

let mut players: Vec<Player> = Vec::with_capacity(room.max_players());
let mut map: HashMap<K, V> = HashMap::with_capacity(expected_entries);
let mut s = String::with_capacity(estimated_len);

```

### SmallVec — Stack-First Collections

```rust

use smallvec::SmallVec;
let players: SmallVec<[PlayerId; 8]> = SmallVec::new();  // Stack for ≤8, heap otherwise

```

**Note:** This project uses `SmallVec` for stack-first collections. `ArrayVec` (fixed-capacity, never heap) is a
valid alternative pattern for external projects with hard capacity limits, but is not a dependency of this codebase.

### `Box<[T]>` / `Arc<str>` Over Heavier Alternatives

```rust

let frozen: Box<[Player]> = players.into_boxed_slice();  // Saves capacity field
let name: Arc<str> = "room_alpha".into();  // One fewer indirection vs Arc<String>

```

---

## Avoid Unnecessary `format!`

Use `tracing`/`log` macros (format lazily) instead of `format!()` in log calls. Use `&str` literals for static strings.
See [observability-and-logging](./observability-and-logging.md).

---

## Hashing Alternatives

| Hasher | When |
|--------|------|
| `FxHash` | Integer/pointer keys, trusted input |
| `AHash` | General purpose, untrusted input |
| `DashMap` | Concurrent reads/writes (uses AHash internally) |
| `std SipHash` | Only when HashDoS resistance is paramount |

This project uses `DashMap` for concurrent access.
Add `rustc-hash`/`ahash` to `Cargo.toml` for single-threaded hot-path maps.

---

## Type Size Optimization

```rust
// ✅ Box large variants to keep enum size small
enum Message {
    Ping,                           // 0 bytes payload
    Pong,                           // 0 bytes payload
    Data(Box<LargePayload>),        // 8 bytes (pointer)
}

// ❌ Large enum due to one variant
enum Message {
    Ping,
    Pong,
    Data(LargePayload),             // enum is as large as LargePayload
}

// ✅ Use smaller integer types where range permits
struct RoomConfig {
    max_players: u16,               // Not usize — rooms don't have 2^64 players
    timeout_secs: u16,              // Not u64 — max ~18 hours is plenty
}

// ✅ Assert sizes at compile time
const _: () = assert!(std::mem::size_of::<Message>() <= 64);

```

---

## Iterator Optimization

```rust

// Chain iterators — avoid intermediate collections
let active_count = players.iter().filter(|p| p.is_connected()).count();

// Use extend() instead of collect+append
let mut all = Vec::with_capacity(room_a.len() + room_b.len());
all.extend(room_a.iter());
all.extend(room_b.iter());

// filter_map instead of filter + map
let ids: Vec<PlayerId> = players.iter()
    .filter_map(|p| p.is_connected().then_some(p.id))
    .collect();

// .copied() for Copy types, chunks_exact over chunks
let ids: Vec<u32> = id_refs.iter().copied().collect();

```

---

## Zero-Copy Patterns

This project uses `bytes::Bytes` extensively for network data:

```rust

use bytes::Bytes;

// ✅ Bytes: reference-counted, zero-copy slice/clone
let data: Bytes = Bytes::from(raw_data);
let slice = data.slice(0..100);     // No copy — shares underlying buffer
broadcast(data.clone());            // Cheap Arc increment, not memcpy

// ✅ Use Cow when data might or might not need modification
use std::borrow::Cow;
fn process(input: &[u8]) -> Cow<'_, [u8]> {
    if needs_transform(input) {
        Cow::Owned(transform(input))
    } else {
        Cow::Borrowed(input)         // Zero-copy for the common case
    }
}

```

### rkyv for Zero-Copy Deserialization

This project uses `rkyv` for zero-copy serialization:

```rust

use rkyv::{Archive, Serialize, Deserialize};

#[derive(Archive, Serialize, Deserialize)]
struct GameState {
    players: Vec<PlayerData>,
    tick: u64,
}

// Deserialize without copying — access archived data directly
let archived = rkyv::access::<ArchivedGameState, rkyv::rancor::Error>(&bytes)?;
println!("Tick: {}", archived.tick);  // No allocation

```

---

## Cache-Friendly Data Structures

Prefer struct-of-arrays for batch processing over array-of-structs.
Use contiguous storage (`Vec<T>`) over linked structures (`LinkedList`). `Vec<T>` has O(1) cache-friendly iteration.

```rust

// Struct-of-arrays for batch processing
struct Players {
    ids: Vec<PlayerId>,
    positions: Vec<Position>,
    health: Vec<u16>,
}

```

---

## Profiling Tools

| Tool | Purpose | Command |
|------|---------|---------|
| `criterion` | Micro-benchmarks | `cargo bench` |
| `flamegraph` | CPU profiling | `cargo flamegraph` |
| `DHAT` | Heap profiling | `valgrind --tool=dhat` |
| `perf` | Linux system-level | `perf record -g ./target/release/bin` |
| `cargo-bloat` | Binary size analysis | `cargo bloat --release` |

### Criterion Benchmark Example

```rust

use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_room_lookup(c: &mut Criterion) {
    let server = setup_server_with_rooms(1000);
    c.bench_function("room_lookup", |b| {
        b.iter(|| {
            server.find_room(black_box(&test_room_code()))
        })
    });
}

criterion_group!(benches, bench_room_lookup);
criterion_main!(benches);

```

---

## String Optimization

```rust

fn validate(input: &str) -> Result<(), Error> { ... }  // &str params

use std::fmt::Write;
let mut out = String::with_capacity(256);
write!(out, "Player {} in room {}", player_id, room_code)?; // Pre-allocated

const GREETING: &str = concat!("matchbox-signaling-server/", env!("CARGO_PKG_VERSION"));  // Compile-time

```

---

## Avoid Cloning in Hot Paths

Use references/borrows, `Arc` for shared ownership across tasks,
and `Bytes` for network data sharing (O(1) clone via refcount bump).

```rust

let shared_msg = Bytes::from(message);
for peer in peers {
    peer.send(shared_msg.clone()).await?;  // Just bumps refcount
}

```

See [Rust-idioms-and-patterns](./rust-idioms-and-patterns.md) for `clone_from()` and `Cow<str>` patterns.

---

## Agent Checklist

- [ ] `Vec::with_capacity()` / `HashMap::with_capacity()` used where size is known
- [ ] `SmallVec` for small-but-growable collections (≤8 elements typical)
- [ ] `Bytes` for shared network data (not `Vec<u8>.clone()`)
- [ ] `DashMap` for concurrent maps, not `Mutex<HashMap>`
- [ ] No `collect()` into intermediate `Vec` unless needed
- [ ] `extend()` instead of `collect()` + `append()`
- [ ] Large enum variants boxed
- [ ] `Arc<str>` over `Arc<String>` for shared strings
- [ ] `clone_from()` when reusing allocations
- [ ] `format!` avoided when string literals or `write!` suffice
- [ ] Hot paths profiled with `criterion` before micro-optimizing
- [ ] `#[inline]` used only after benchmarking proves benefit

---

## Related Skills

- [async-Rust-best-practices](./async-rust-best-practices.md) — Async performance and task management
- [Rust-idioms-and-patterns](./rust-idioms-and-patterns.md) — Iterator patterns and zero-cost abstractions
- [dependency-management](./dependency-management.md) — Alternative crate recommendations
- [observability-and-logging](./observability-and-logging.md) — Metrics for performance monitoring
