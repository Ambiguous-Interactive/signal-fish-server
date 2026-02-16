# Skill: Rust Refactoring Guide

<!-- trigger: refactor, extract, split, rename, modernize, code smell, cleanup | Safe incremental Rust refactoring workflows | Core -->

**Trigger**: When restructuring, extracting, splitting, or modernizing existing Rust code.

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

- Writing brand new code from scratch (see [Rust-idioms-and-patterns](./rust-idioms-and-patterns.md))
- Performance-focused changes (see [Rust-performance-optimization](./rust-performance-optimization.md))

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

## Replacing Magic Numbers/Strings with Constants/Enums

```rust

// ❌ Before: magic numbers scattered
if room.players.len() >= 8 { return Err(Error::Full); }
if timeout > 300 { return Err(Error::Timeout); }
if msg.len() > 65536 { return Err(Error::TooLarge); }

// ✅ After: named constants
const MAX_PLAYERS_PER_ROOM: usize = 8;
const CONNECTION_TIMEOUT_SECS: u64 = 300;
const MAX_MESSAGE_BYTES: usize = 65_536;

if room.players.len() >= MAX_PLAYERS_PER_ROOM { ... }
if timeout > CONNECTION_TIMEOUT_SECS { ... }
if msg.len() > MAX_MESSAGE_BYTES { ... }

```

**Workflow:**

1. `grep` for numeric/string literals in the codebase
2. Replace one constant at a time
3. Compile after each replacement
4. Consider grouping related constants in a `config` module

---

## Converting `unwrap()` Chains to Proper Error Handling

See [error-handling-guide](./error-handling-guide.md) for the full unwrap hierarchy and `?` propagation patterns.

**Quick workflow:**

1. Find unwrap sites: `cargo clippy -- -W clippy::unwrap_used`
2. Change return type to `Result<T, E>`
3. Replace each `.unwrap()` with `?` plus `.map_err()` / `.ok_or_else()`
4. Update callers (compiler-driven) — fix each call site the compiler flags
5. Compile, test, commit

---

## Moving from `String` to `&str` in Parameters

Change parameter from `String` to `&str`. Callers passing `String` get automatic coercion. If the function needs ownership internally, use `impl Into<String>`.

---

## Replacing HashMap with DashMap

Replace `Arc<Mutex<HashMap<K,V>>>` with `DashMap<K,V>`, remove `.lock().unwrap()` calls, and update `.get()` (returns `Ref` guard). The `.entry()` API differs slightly from `HashMap` — check DashMap docs. Run concurrent tests to verify.

---

## Converting Synchronous Code to Async

See [async-Rust-best-practices](./async-rust-best-practices.md) for async patterns.

**Quick workflow:** Add `async`, replace blocking I/O with async equivalents (e.g., `tokio::fs`), add `.await`, update callers, check for `std::sync::Mutex` → `tokio::sync::Mutex`, test with `#[tokio::test]`.

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

## Reducing `clone()` Usage

See [Rust-performance-optimization](./rust-performance-optimization.md) for detailed clone reduction and zero-copy patterns.

**Quick checks:** Can you pass `&T` instead? Use `Arc<T>` for shared ownership across tasks? Use `Bytes` for network data? Use `Cow<str>` for conditional ownership?

---

## AI-Assisted Refactoring Patterns

**How agents should approach refactoring:**

1. Read tests first — understand expected behavior before changing code
2. Make one structural change → `cargo check` → fix errors → `cargo test` → commit
3. Never combine renames with logic changes in the same step
4. Use the compiler as your guide: rename a type, then fix every error it reports

**Red flags an AI must stop and flag:**

| Red Flag | Why |
|----------|-----|
| Removing or weakening a public API method | Breaks downstream callers/SDKs |
| Deleting tests without replacement | Loses coverage — always replace first |
| Skipping `cargo test` after changes | Unverified refactoring is a regression risk |
| Changing trait signatures on `GameDatabase` | Affects `InMemoryDatabase`, `PostgresDatabase`, `DynamoDbDatabase` |
| Modifying protocol message types | Breaks client SDK compatibility |

**Structured output for agent refactoring steps:**

```text
REFACTOR: [description]
  Files: [list of files changed]
  Risk: low/medium/high
  Verification: cargo check && cargo test --all-features
  Rollback: git checkout -- [files]

```

See [code-review-checklist](./code-review-checklist.md) for review patterns and
[solid-principles-enforcement](./solid-principles-enforcement.md) for design principle checks.

---

## Using `cargo clippy --fix`

See [clippy-and-linting](./clippy-and-linting.md) for full clippy configuration and `--fix` usage.

**Quick workflow:** Commit → `cargo clippy --fix --allow-dirty` → `git diff` to review → revert readability regressions → commit.

---

## Modernizing Older Rust Idioms

| Old Pattern | Modern Replacement |
|-------------|-------------------|
| `extern crate foo;` | Remove (edition 2018+) |
| `#[macro_use] extern crate;` | `use foo::macro_name;` |
| `impl Trait for Box<T>` | Use `impl Trait for T` with generics |
| `fn foo() -> Box<dyn Iterator>` | `fn foo() -> impl Iterator` |
| `.to_owned()` on `&str` | `.to_string()` (same perf, clearer) |
| `&vec[..]` | `&vec` (auto-deref) |
| `0..vec.len()` loop | `for item in &vec` or `.iter()` |
| `try!()` macro | `?` operator |
| `#[async_trait]` everywhere | Native async traits (Rust 1.75+) |

---

## Agent Checklist

Before refactoring:

- [ ] All tests pass
- [ ] Code is committed (can revert)

During refactoring:

- [ ] One type of change at a time
- [ ] Compile after each change
- [ ] Test after each logical step
- [ ] Commit working increments

Common refactorings:

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

## Related Skills

- [Rust-idioms-and-patterns](./rust-idioms-and-patterns.md) — Target patterns for refactoring
- [clippy-and-linting](./clippy-and-linting.md) — Automated fixes with clippy
- [error-handling-guide](./error-handling-guide.md) — Refactoring unwrap chains
- [testing-strategies](./testing-strategies.md) — Tests must pass before and after refactoring
- [code-review-checklist](./code-review-checklist.md) — AI-driven code review with structured output
- [solid-principles-enforcement](./solid-principles-enforcement.md) — SOLID principle enforcement during refactoring
