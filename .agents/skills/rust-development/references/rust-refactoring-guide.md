# Rust Refactoring Guide

**Applies to**: When restructuring, extracting, splitting, or modernizing existing Rust code.

---

## When to Use

- Breaking up large files or functions
- Extracting reusable types, traits, or modules
- Replacing `unwrap()` chains with proper error handling
- Converting `String` parameters to `&str`
- Modernizing older Rust idioms
- Using `cargo clippy --fix` for automated improvements

---

## When NOT to Use

- Writing brand new code from scratch (see [Rust Idioms And Patterns](./rust-idioms-and-patterns.md))
- Performance-focused changes (see [Rust Performance Optimization](./rust-performance-optimization.md))

---

## TL;DR

- Always have passing tests before starting any refactoring.
- Make one type of change at a time — compile and test after each step.
- Use the compiler as your refactoring tool: rename, break things, fix the errors.
- Prefer automated fixes (`cargo clippy --fix`, `cargo fmt`) before manual changes.
- Extract types and functions to reduce complexity; don't add abstractions preemptively.

---

## Identifying Code Smells in Rust

| Smell | Symptom | Fix |
|-------|---------|-----|
| Long function | >100 lines, multiple responsibilities | Extract helper functions |
| Deep nesting | >3 levels of indent | Early returns, extract branches |
| Large struct | >10 fields | Break into sub-structs, builder pattern |
| Stringly-typed | `String` where newtype fits | Introduce validated newtype |
| Boolean params | `fn create(true, false, true)` | Replace with enums |
| `unwrap()` chains | Multiple `.unwrap()` in sequence | Convert to `?` propagation |
| Repeated code | Same logic in 3+ places | Extract into shared function/trait |
| Large enum variant | One variant 10x larger than others | Box the large variant |
| Magic numbers | `if timeout > 300` | Extract constant |
| God module | 1000+ line file | Split into sub-modules |

---

## Safe Refactoring Workflow

```text
1. Ensure all tests pass:     cargo test --all-features
2. Make ONE change
3. Compile:                    cargo check
4. Fix compiler errors
5. Run clippy:                 cargo clippy --all-targets --all-features
6. Run tests:                  cargo test --all-features
7. Commit
8. Repeat from step 2
```

**Never skip step 1.** If tests don't pass before you start, you can't verify your refactoring is correct.

---

## Extracting Modules and Types

```rust
// Before: src/server.rs — 2000 lines with 30+ methods on GameServer

// Step 1: Create src/server/room_manager.rs
pub(crate) struct RoomManager { ... }
impl RoomManager {
    pub(crate) fn create(&self, config: RoomConfig) -> Result<Room, CreateError> { ... }
    pub(crate) fn join(&self, code: &RoomCode, player: PlayerId) -> Result<(), JoinError> { ... }
}

// Step 2: Declare in src/server/mod.rs
mod room_manager;
use room_manager::RoomManager;

// Step 3: Delegate from original methods
impl GameServer {
    pub fn create_room(&self, config: RoomConfig) -> Result<Room, CreateError> {
        self.room_manager.create(config)
    }
}
// Step 4: Compile, test, commit. Then move more implementation details.
```

---

## Breaking Up Large Files

```text
# Before: src/server.rs (2000 lines)
# After:
src/server/
├── mod.rs            (100 lines — public API, re-exports)
├── room_manager.rs   (300 lines)
├── player_manager.rs (250 lines)
└── message_handler.rs(400 lines)
```

Move `src/server.rs` to `src/server/mod.rs`, then extract one section at a time, compiling after each.

---

## Replacing Magic Numbers/Strings with Constants

```rust
// ❌ Before: magic numbers scattered
if room.players.len() >= 8 { return Err(Error::Full); }
if timeout > 300 { return Err(Error::Timeout); }

// ✅ After: named constants
const MAX_PLAYERS_PER_ROOM: usize = 8;
const CONNECTION_TIMEOUT_SECS: u64 = 300;
```

Grep for numeric/string literals, replace one at a time, compile after each.

---

## Converting `unwrap()` Chains to Proper Error Handling

See [Error Handling Guide](./error-handling-guide.md) for the full unwrap hierarchy and `?` propagation patterns.

**Quick workflow:**

1. Find unwrap sites: `cargo clippy -- -W clippy::unwrap_used`
2. Change return type to `Result<T, E>`
3. Replace each `.unwrap()` with `?` plus `.map_err()` / `.ok_or_else()`
4. Update callers (compiler-driven) — fix each call site the compiler flags
5. Compile, test, commit

---

## Quick Refactoring Recipes

- **`String` → `&str` params:** Change parameter type; callers passing `String` get auto-coercion.
  Use `impl Into<String>` if ownership is needed internally.
- **`HashMap` → `DashMap`:** Replace `Arc<Mutex<HashMap<K,V>>>`, remove `.lock()` calls,
  update `.get()` (returns `Ref` guard). Check DashMap docs for `.entry()` API differences.
- **Sync → Async:** Add `async`, replace blocking I/O with `tokio::fs`, add `.await`,
  check for `std::sync::Mutex` → `tokio::sync::Mutex`.
  See [Async Rust Best Practices](./async-rust-best-practices.md).
- **Reduce clone():** Pass `&T`, use `Arc<T>` for shared ownership, `Bytes` for network data,
  `Cow<str>` for conditional ownership.
  See [Rust Performance Optimization](./rust-performance-optimization.md).

---

## Extracting Traits from Concrete Implementations

```rust
// Step 1: Extract trait from concrete methods
#[async_trait]
pub trait Database: Send + Sync {
    async fn find_room(&self, code: &str) -> Result<Option<Room>, DbError>;
    async fn save_room(&self, room: &Room) -> Result<(), DbError>;
}

// Step 2: Implement for production backend
struct PostgresDatabase { pool: PgPool }
#[async_trait]
impl Database for PostgresDatabase { ... }

// Step 3: Make server generic over the trait
struct GameServer<D: Database> { db: D }

// Step 4: Implement for testing
struct InMemoryDatabase { rooms: DashMap<String, Room> }
#[async_trait]
impl Database for InMemoryDatabase { ... }
```

---

## AI-Assisted Refactoring Patterns

1. Read tests first — understand expected behavior before changing code
2. Make one structural change → `cargo check` → fix errors → `cargo test` → commit
3. Never combine renames with logic changes in the same step

**Red flags to stop and flag:**

| Red Flag | Why |
|----------|-----|
| Removing or weakening a public API method | Breaks downstream callers/SDKs |
| Deleting tests without replacement | Loses coverage — always replace first |
| Changing trait signatures on `GameDatabase` | Affects all implementations |
| Modifying protocol message types | Breaks client SDK compatibility |

---

## Using `cargo clippy --fix`

See [Clippy And Linting](./clippy-and-linting.md) for full clippy configuration.

```bash
# Commit first, then fix, review, commit
cargo clippy --fix --allow-dirty
git diff  # Review all changes before committing
```

---

## Modernizing Older Rust Idioms

| Old Pattern | Modern Replacement |
|-------------|-------------------|
| `extern crate foo;` | Remove (edition 2018+) |
| `#[macro_use] extern crate;` | `use foo::macro_name;` |
| `fn foo() -> Box<dyn Iterator>` | `fn foo() -> impl Iterator` |
| `0..vec.len()` loop | `for item in &vec` or `.iter()` |
| `try!()` macro | `?` operator |
| `#[async_trait]` everywhere | Native async traits (Rust 1.75+) |

---

## Agent Checklist

Before: all tests pass, code is committed (can revert).
During: one type of change at a time, compile and test after each logical step.

- [ ] `unwrap()` → `?` / `.ok_or()` / `.unwrap_or_default()`
- [ ] `String` params → `&str` (or `impl Into<String>`)
- [ ] Magic numbers → named constants
- [ ] Boolean params → enums
- [ ] Large files → sub-modules
- [ ] `HashMap` → `DashMap` (concurrent) or `FxHashMap` (single-thread)
- [ ] `Mutex<HashMap>` → `DashMap`
- [ ] Concrete types → traits (for testability)
- [ ] `.clone()` → `&T` borrows / `Arc<T>` / `Bytes`

---

## Related References

- [Rust Idioms And Patterns](./rust-idioms-and-patterns.md) — Target patterns for refactoring
- [Clippy And Linting](./clippy-and-linting.md) — Automated fixes with clippy
- [Error Handling Guide](./error-handling-guide.md) — Refactoring unwrap chains
- [Testing Core Patterns](../../testing-rust/references/testing-core-patterns.md) — Tests must pass before and after refactoring
- [Code Review Checklist](../../agent-quality/references/code-review-checklist.md) — AI-driven code review with structured output
- [Solid Principles Enforcement](./solid-principles-enforcement.md) — SOLID principle enforcement during refactoring
