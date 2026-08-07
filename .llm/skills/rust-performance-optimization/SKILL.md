---
name: rust-performance-optimization
description: >-
  Apply project guidance for Rust performance optimization. Use when optimizing hot paths,
  reducing allocations, or profiling performance-critical code.
---

# Rust Performance Optimization

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
- API design decisions (see [API Design Guidelines](../api-design-guidelines/SKILL.md))

---

## TL;DR

- Use `with_capacity()` for all collections where the size is known or estimable.
- Prefer `SmallVec`, `Bytes`, and `Arc<str>` over heap-heavy alternatives.
- Use `DashMap`/`FxHashMap` over `HashMap` in hot paths.
- Profile before optimizing — use `criterion` for benchmarks, `flamegraph` for profiling.
- Avoid cloning in hot paths; use `Bytes` for zero-copy network data.

---

## Release Profile Configuration

This project's [Cargo.toml](../../../Cargo.toml) already has optimized release profiles (`lto = "thin"`,
`codegen-units = 1`, `strip = true`, `opt-level = 3` for deps).
Use `lto = "fat"` only if benchmarks show measurable gain.
Consider `panic = "abort"` for production binaries (smaller binary, no unwind overhead).

---

## Alternative Allocators

Consider `tikv-jemallocator` (multi-threaded server workloads) or `mimalloc` (good cross-platform default).
Neither is currently in project dependencies. Benchmark before committing —
the default is often fine for I/O-bound servers.

```rust
#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;
```

---

## Heap Allocation Reduction

```rust
// ✅ Pre-allocation
let mut players: Vec<Player> = Vec::with_capacity(room.max_players());
let mut map: HashMap<K, V> = HashMap::with_capacity(expected_entries);

// ✅ SmallVec — stack for ≤8 elements, heap otherwise
use smallvec::SmallVec;
let players: SmallVec<[PlayerId; 8]> = SmallVec::new();

// ✅ Box<[T]> / Arc<str> over heavier alternatives
let frozen: Box<[Player]> = players.into_boxed_slice();  // Saves capacity field
let name: Arc<str> = "room_alpha".into();  // One fewer indirection vs Arc<String>
```

Use `tracing`/`log` macros (format lazily) instead of `format!()` in log calls.
See [Observability And Logging](../observability-and-logging/SKILL.md).

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
    Ping,
    Data(Box<LargePayload>),  // 8 bytes (pointer) vs entire LargePayload size
}

// ✅ Use smaller integer types where range permits
struct RoomConfig {
    max_players: u16,   // Not usize — rooms don't have 2^64 players
    timeout_secs: u16,  // Not u64 — max ~18 hours is plenty
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

// .copied() for Copy types
let ids: Vec<u32> = id_refs.iter().copied().collect();
```

---

## Zero-Copy Patterns

This project uses `bytes::Bytes` extensively for network data:

```rust
use bytes::Bytes;

// ✅ Bytes: reference-counted, zero-copy slice/clone
let data: Bytes = Bytes::from(raw_data);
let slice = data.slice(0..100);    // No copy — shares underlying buffer
broadcast(data.clone());           // Cheap Arc increment, not memcpy

// ✅ Use Cow when data might or might not need modification
fn process(input: &[u8]) -> Cow<'_, [u8]> {
    if needs_transform(input) { Cow::Owned(transform(input)) }
    else { Cow::Borrowed(input) }  // Zero-copy for the common case
}
```

---

## Cache-Friendly Data Structures

Prefer struct-of-arrays for batch processing. Use `Vec<T>` (contiguous storage) over `LinkedList`.

```rust
// Struct-of-arrays for batch processing
struct Players { ids: Vec<PlayerId>, positions: Vec<Position>, health: Vec<u16> }
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

```rust
// Criterion benchmark example
fn bench_room_lookup(c: &mut Criterion) {
    let server = setup_server_with_rooms(1000);
    c.bench_function("room_lookup", |b| {
        b.iter(|| server.find_room(black_box(&test_room_code())))
    });
}
criterion_group!(benches, bench_room_lookup);
criterion_main!(benches);
```

---

## Avoid Cloning in Hot Paths

Use references/borrows, `Arc` for shared ownership, and `Bytes` for network data (O(1) clone via refcount bump).

```rust
let shared_msg = Bytes::from(message);
for peer in peers { peer.send(shared_msg.clone()).await?; }  // Just bumps refcount
```

See [Rust Idioms And Patterns](../rust-idioms-and-patterns/SKILL.md) for `clone_from()` and `Cow<str>` patterns.

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
- [ ] Hot paths profiled with `criterion` before micro-optimizing

---

## Related Skills

- [Async Rust Best Practices](../async-rust-best-practices/SKILL.md) — Async performance and task management
- [Rust Idioms And Patterns](../rust-idioms-and-patterns/SKILL.md) — Iterator patterns and zero-cost abstractions
- [Dependency Management Cargo](../dependency-management/SKILL.md) — Alternative crate recommendations
- [Observability And Logging](../observability-and-logging/SKILL.md) — Metrics for performance monitoring
