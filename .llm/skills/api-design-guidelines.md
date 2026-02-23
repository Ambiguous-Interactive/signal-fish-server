# Skill: API Design Guidelines

<!--
  trigger: api, design, public, interface, sdk, newtype, endpoint
  | Designing public APIs, protocol types, or SDK interfaces
  | Feature
-->

**Trigger**: When designing or modifying any public API surface, protocol types, HTTP endpoints, or SDK interfaces.

---

## When to Use

- Designing new public types, traits, or functions
- Adding HTTP or WebSocket API endpoints
- Creating SDK-facing interfaces
- Reviewing API ergonomics or naming conventions
- Future-proofing with sealed traits, private fields, and explicit exhaustive protocol enums

---

## When NOT to Use

- Internal implementation details not exposed to consumers
- Pure performance optimization (see [Rust-performance-optimization](./rust-performance-optimization.md))
- Error type design specifically (see [error-handling-guide](./error-handling-guide.md))

---

## TL;DR

- Follow the Rust API Guidelines Checklist (RFC 1105) for all public types.
- Use newtypes and enums for type safety — never raw strings or booleans for distinct concepts.
- Minimize public API surface — expose the minimum needed, keep everything else `pub(crate)`.
- Future-proof with sealed traits, private fields, and explicit exhaustive enum matching guidance.
- Document every public item with examples, error conditions, and panic conditions.

---

## Naming Conventions

### Casing

| Item | Convention | Example |
|------|-----------|---------|
| Types, traits | `UpperCamelCase` | `RoomConfig`, `Database` |
| Functions, methods | `snake_case` | `find_room`, `player_count` |
| Constants | `SCREAMING_SNAKE_CASE` | `MAX_ROOMS`, `DEFAULT_TIMEOUT` |
| Modules | `snake_case` | `room_manager`, `auth` |
| Lifetimes | Short lowercase | `'a`, `'de`, `'conn` |

### Conversion Methods

| Pattern | Meaning |
|---------|---------|
| `as_x()` | Cheap borrow-to-borrow cast |
| `to_x()` | Expensive borrow-to-owned conversion |
| `into_x()` | Owned-to-owned conversion (consumes self) |
| `try_x()` | Fallible version of `x()` |

### Getters and Predicates

```rust
impl Room {
    // ✅ Getters: field name, no get_ prefix
    fn code(&self) -> &RoomCode { &self.code }
    fn player_count(&self) -> usize { self.players.len() }

    // ✅ Predicates: is_/has_/can_ prefix
    fn is_full(&self) -> bool { self.players.len() >= self.max_players }
    fn can_join(&self, player: &Player) -> bool { !self.is_full() && !self.has_player(player) }
}

impl Room {
    fn iter(&self) -> impl Iterator<Item = &Player> { ... }
    fn player_ids(&self) -> impl Iterator<Item = PlayerId> + '_ { ... }
}
```

---

## Interoperability — Common Traits

Derive `Debug`, `Clone`, `PartialEq`, `Eq`, `Hash`, `Default`, `Serialize`/`Deserialize` eagerly on all public types.
Implement `Display`, `From`/`TryFrom`, and `AsRef` where applicable.

See [Rust Idioms and Patterns](rust-idioms-and-patterns.md) for the full common traits derive checklist
and conversion trait patterns.

---

## Type Safety

Use newtypes for domain identifiers and enums instead of booleans for function parameters.
See [Rust Idioms and Patterns](rust-idioms-and-patterns.md) for newtypes and enums-over-booleans patterns.

### Bitflags for Options

```rust
// ✅ Use bitflags for combinable options
bitflags::bitflags! {
    pub struct Capabilities: u32 {
        const RELAY    = 0b0001;
        const OBSERVE  = 0b0010;
        const ADMIN    = 0b0100;
    }
}
configure_client(Capabilities::RELAY | Capabilities::OBSERVE);
```

> **Note:** Add `bitflags` to `Cargo.toml` to use; not currently in project dependencies.

---

## Flexibility

```rust
// ✅ Accept generics, return concrete
pub fn set_name(&mut self, name: impl Into<String>) { self.name = name.into(); }
pub fn get_players(&self) -> &[Player] { &self.players }

// ✅ Return iterators — caller decides how to use
pub fn find_rooms(&self, filter: &Filter) -> impl Iterator<Item = RoomInfo> + '_ {
    self.rooms.iter().filter(move |r| filter.matches(r)).map(|r| r.info())
}
```

---

## Future-Proofing

### Exhaustive Public Enum Design

```rust
// ✅ Keep protocol and domain enums explicit and fully matched
pub enum DisconnectReason { Timeout, Kicked, ClientLeft, ServerShutdown }
```

### Sealed Traits

```rust
// ✅ Prevent external implementations — you control the trait
mod private { pub trait Sealed {} }

pub trait Transport: private::Sealed {
    fn send(&self, data: &[u8]) -> Result<(), Error>;
}

impl private::Sealed for WebSocketTransport {}
impl Transport for WebSocketTransport { ... }
```

### Private Fields with Constructors

```rust
// ✅ Private fields → can change representation without breaking API
pub struct Duration { millis: u64 }

impl Duration {
    pub fn from_secs(secs: u64) -> Self { Self { millis: secs * 1000 } }
    pub fn from_millis(millis: u64) -> Self { Self { millis } }
    pub fn as_millis(&self) -> u64 { self.millis }
}
```

---

## Documentation Standards

Every public item needs: summary line, `# Errors` listing each variant, `# Panics` if any,
`# Safety` for unsafe, `# Examples` with compilable code, cross-references with `[`TypeName`]` links.

```rust
/// Creates a new room with the given configuration.
///
/// # Errors
/// Returns [`CreateError::InvalidConfig`] if `max_players` is 0.
///
/// # Examples
/// ```
/// let config = RoomConfig::builder().max_players(4).build()?;
/// let room = server.create_room(config).await?;
/// # Ok::<(), anyhow::Error>(())
/// ```
pub async fn create_room(&self, config: RoomConfig) -> Result<Room, CreateError> { todo!() }
```

---

## Public API Surface and Semver

Start with `pub(crate)` by default, expand only when needed.

**Breaking** (major bump): Removing/changing public items, adding required fields, changing trait bounds.
**Non-breaking**: Adding new public items, adding optional methods with defaults, weakening trait bounds.

---

## HTTP/WebSocket API Patterns (axum)

```rust
// Typed extractors with validation
async fn join_room(
    State(server): State<Arc<GameServer>>,
    Json(req): Json<JoinRequest>,
) -> Result<Json<JoinResponse>, AppError> {
    let validated = req.validate()?;
    Ok(Json(server.join_room(validated).await?))
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            AppError::Internal(e) => {
                tracing::error!(error = %e, "Internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
        };
        (status, Json(ErrorResponse { error: message })).into_response()
    }
}
```

Use versioned routes (`/v2/...`). Return structured JSON errors.
Log internal errors server-side; return generic messages to clients.
Maintain an OpenAPI specification for REST APIs with multiple endpoints.

---

## Signaling Server-Specific Guidance

Use `#[serde(tag = "type", rename_all = "snake_case")]` on all client/server message enums.
See [WebSocket-protocol-patterns](./websocket-protocol-patterns.md) for full message design.

Use the typestate pattern to prevent invalid operations.
See [Rust-idioms-and-patterns](./rust-idioms-and-patterns.md) for the full typestate pattern.

---

## Agent Checklist

- [ ] All public types implement `Debug`, `Display`, `Clone` (where applicable)
- [ ] All public types implement `Send + Sync`
- [ ] Newtypes for IDs, codes, tokens — no raw `String`/`Uuid` in APIs
- [ ] Enums instead of booleans for function parameters
- [ ] Protocol/domain enums matched explicitly without wildcard catch-all arms
- [ ] `#[must_use]` on Result-returning functions
- [ ] Functions accept borrows/generics, return owned/concrete
- [ ] Private fields with constructors enforce invariants
- [ ] Every public item has rustdoc with `# Errors`, `# Examples`
- [ ] Minimum visibility: `pub(crate)` by default, `pub` only when needed
- [ ] Sealed traits for traits that shouldn't be externally implemented
- [ ] Serde derives on all API boundary types with `#[serde(rename_all = "snake_case")]`

---

## Related Skills

- [Rust-idioms-and-patterns](./rust-idioms-and-patterns.md) — Rust naming conventions and canonical patterns
- [error-handling-guide](./error-handling-guide.md) — Designing error types for APIs
- [defensive-programming](./defensive-programming.md) — Input validation at API boundaries
- [WebSocket-protocol-patterns](./websocket-protocol-patterns.md) — WebSocket-specific API patterns
