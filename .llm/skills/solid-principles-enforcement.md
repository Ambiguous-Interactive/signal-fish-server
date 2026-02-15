# Skill: SOLID Principles Enforcement

<!-- trigger: solid, single-responsibility, open-closed, liskov, interface-segregation, dependency-inversion, clean-code, architecture | Enforcing SOLID principles in Rust and TypeScript | Core -->

**Trigger**: When designing, reviewing, or refactoring code to ensure adherence to SOLID principles and clean architecture.

---

## When to Use
- Designing new modules, traits, or structs
- Reviewing PRs for architectural quality
- Refactoring code that has grown unwieldy
- When code smells suggest principle violations
- Evaluating whether abstractions are appropriate

---

## When NOT to Use
- Prototyping or spike work (SOLID comes during cleanup)
- Performance-critical inner loops where abstraction has overhead
- Simple utility functions that don't warrant trait extraction

---

## TL;DR
- Each struct/module should have exactly one reason to change
- Extend behavior through traits and composition, not modification
- Keep traits small and focused (2-5 methods)
- High-level modules depend on trait abstractions, not concrete types
- Functions under 50 lines, files under 500 lines, nesting under 3 levels

---

## Single Responsibility Principle (SRP)

**Rule**: Every struct, module, and function should have one reason to change.

**Smell**: A struct that handles both room management AND WebSocket serialization.

```rust
// ❌ SRP violation: GameServer does everything  
impl GameServer {
    fn create_room(&self) { /* room logic */ }
    fn serialize_message(&self) { /* serialization */ }
    fn authenticate_player(&self) { /* auth logic */ }
    fn query_metrics(&self) { /* metrics */ }
}

// ✅ Separated responsibilities
struct RoomManager { /* room creation, joining, leaving */ }
struct MessageSerializer { /* protocol serialization */ }
struct Authenticator { /* player authentication */ }
struct MetricsCollector { /* metrics gathering */ }

struct GameServer {
    // Composes focused components
    rooms: RoomManager,
    auth: Authenticator,
    metrics: MetricsCollector,
}
```

**File-level SRP**: One concern per file. `src/server.rs` with 2000 lines → split into `src/server/room_manager.rs`, `src/server/player_handler.rs`, `src/server/message_router.rs`.

**Function-level SRP**: Functions over 50 lines usually do too much. Extract helper functions with descriptive names.

---

## Open/Closed Principle (OCP)

**Rule**: Open for extension, closed for modification. Add new behavior without changing existing code.

**Pattern**: New transport types, new database backends, new message types.

```rust
// ✅ Open for extension: new transports without modifying existing code
#[async_trait]
pub trait RelayTransport: Send + Sync {
    async fn send(&self, data: &[u8]) -> Result<(), TransportError>;
    async fn recv(&self) -> Result<Vec<u8>, TransportError>;
}

struct QuicTransport { /* ... */ }
struct TcpTransport { /* ... */ }
struct WebSocketTransport { /* ... */ }

// Adding a new transport = new struct + impl, zero changes to existing code
struct WebTransportRelay { /* ... */ }
impl RelayTransport for WebTransportRelay { /* ... */ }
```

**In TypeScript**: Use interfaces and factory functions, not class hierarchies.

```typescript
// ✅ Extend with new chart types without modifying existing code
interface DashboardWidget {
    render(data: MetricsData): HTMLElement;
    update(data: MetricsData): void;
}
```

---

## Liskov Substitution Principle (LSP)

**Rule**: Every trait implementation must honor the trait's documented contract. Callers must not need to know which concrete type they're using.

```rust
// ✅ Both implementations honor the Database contract identically
#[async_trait]
pub trait Database: Send + Sync {
    async fn find_room(&self, code: &RoomCode) -> Result<Option<Room>, DbError>;
    async fn save_room(&self, room: &Room) -> Result<(), DbError>;
}

// InMemoryDatabase and PostgresDatabase must behave identically:
// - find_room returns None for missing rooms (not an error)
// - save_room overwrites existing rooms with same code
// - Both return DbError for infrastructure failures only
```

**Violation smell**: An implementation that panics, returns different error types, or has different side effects than other implementations of the same trait.

---

## Interface Segregation Principle (ISP)

**Rule**: Many small, focused traits beat one large trait. No implementor should be forced to provide methods it doesn't use.

```rust
// (simplified signatures — use concrete error types in real code)
// ❌ Fat interface: test implementations must stub 8+ methods
trait GameOperations {
    fn create_room(&self) -> Result<Room>;
    fn join_room(&self) -> Result<()>;
    fn leave_room(&self) -> Result<()>;
    fn send_message(&self) -> Result<()>;
    fn get_metrics(&self) -> Metrics;
    fn authenticate(&self) -> Result<Token>;
    fn manage_authority(&self) -> Result<()>;
    fn relay_data(&self) -> Result<()>;
}

// (simplified signatures — use concrete error types in real code)
// ✅ Segregated: implement only what you need
trait RoomManagement {
    fn create_room(&self) -> Result<Room>;
    fn join_room(&self) -> Result<()>;
    fn leave_room(&self) -> Result<()>;
}

trait Messaging {
    fn send_message(&self) -> Result<()>;
    fn relay_data(&self) -> Result<()>;
}

trait Authentication {
    fn authenticate(&self) -> Result<Token>;
}
```

**Guideline**: 2-5 methods per trait. If a trait has 8+ methods, it probably needs splitting.

---

## Dependency Inversion Principle (DIP)

**Rule**: High-level modules depend on abstractions (traits), not concrete implementations.

```rust
// ❌ Direct dependency on concrete type
struct GameServer {
    db: PostgresDatabase,  // Tightly coupled to Postgres
}

// ✅ Depends on abstraction
struct GameServer<D: Database> {
    db: D,  // Any Database implementation works
}

// Production: GameServer<PostgresDatabase>
// Testing:    GameServer<InMemoryDatabase>
// Serverless: GameServer<DynamoDatabase>
```

**In TypeScript**: Use dependency injection, not direct imports of concrete implementations.

---

## AI-Friendly Code Patterns

Code structured for SOLID is also structured for AI agents. These patterns help agents understand and modify code effectively:

### Naming
- **Domain-specific vocabulary**: `RoomManager`, `PeerConnection`, `SignalingMessage` — not `Manager`, `Connection`, `Message`
- **Intent-revealing functions**: `establish_peer_connection()` not `do_connect()`
- **Consistent terminology**: If it's a "room" everywhere, don't alternate with "channel" or "session"

### Structure
- **One concern per file/module** — AI reads entire files; focused files = faster comprehension
- **Consistent patterns across the codebase** — AI learns patterns and replicates them
- **Flat module trees** — prefer `src/auth/`, `src/rooms/` over `src/core/internal/impl/auth/`
- **Top-level doc comments** — AI uses these as navigation hints

### Decomposition Limits

| Metric | Ideal | Max | Action |
|--------|-------|-----|--------|
| Function lines | < 30 | 50 | Extract helper functions |
| File lines | < 300 | 500 | Split into sub-modules |
| Nesting depth | 2 | 3 | Early returns, extract branches |
| Struct fields | < 7 | 10 | Break into sub-structs |
| Trait methods | 2-3 | 5 | Split trait |
| Enum variants | < 8 | 12 | Consider sub-enums or grouping |

### Naming Anti-Patterns

```rust
// ❌ Generic, meaningless names
mod utils;
mod helpers;
mod common;
mod shared;
mod misc;

// ✅ Domain-specific, descriptive names
mod room_lifecycle;
mod player_authentication;
mod protocol_serialization;
mod connection_health;
mod relay_transport;
```

---

## Agent Checklist

- [ ] **SRP**: Each struct/function has one reason to change
- [ ] **OCP**: New behavior added via new types, not modifying existing code
- [ ] **LSP**: All trait implementations honor the same contract
- [ ] **ISP**: Traits have 2-5 methods; no forced stub implementations
- [ ] **DIP**: High-level code depends on traits, not concrete types
- [ ] Functions under 50 lines
- [ ] Files under 500 lines
- [ ] Nesting under 3 levels
- [ ] Domain-specific names (no `utils`, `helpers`, `misc`)
- [ ] Consistent vocabulary throughout codebase

---

## Related Skills

- [code-review-checklist](./code-review-checklist.md) — Review process incorporating SOLID checks
- [rust-refactoring-guide](./rust-refactoring-guide.md) — Refactoring workflow for fixing violations
- [rust-idioms-and-patterns](./rust-idioms-and-patterns.md) — Idiomatic Rust patterns
- [api-design-guidelines](./api-design-guidelines.md) — API design following SOLID
- [agentic-workflow-patterns](./agentic-workflow-patterns.md) — AI agent workflow patterns for code review
