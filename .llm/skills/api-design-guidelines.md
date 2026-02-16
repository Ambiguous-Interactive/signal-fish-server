# Skill: API Design Guidelines

<!-- trigger: api, design, public, interface, sdk, newtype, endpoint | Designing public APIs, protocol types, or SDK interfaces | Feature -->

**Trigger**: When designing or modifying any public API surface, protocol types, HTTP endpoints, or SDK interfaces.

---

## When to Use

- Designing new public types, traits, or functions
- Adding HTTP or WebSocket API endpoints
- Creating SDK-facing interfaces
- Reviewing API ergonomics or naming conventions
- Future-proofing with `#[non_exhaustive]` or sealed traits

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
- Future-proof with `#[non_exhaustive]`, sealed traits, and private fields.
- Document every public item with examples, error conditions, and panic conditions.

---

## Naming Conventions

### Casing

| Item | Convention | Example |
|------|-----------|---------|
| Types, traits | `UpperCamelCase` | `RoomConfig`, `Database` |
| Functions, methods | `snake_case` | `find_room`, `player_count` |
| Local variables | `snake_case` | `room_code`, `max_players` |
| Constants | `SCREAMING_SNAKE_CASE` | `MAX_ROOMS`, `DEFAULT_TIMEOUT` |
| Modules | `snake_case` | `room_manager`, `auth` |
| Type parameters | Short `UpperCamelCase` | `T`, `E`, `S: State` |
| Lifetimes | Short lowercase | `'a`, `'de`, `'conn` |

### Conversion Methods

| Pattern | Meaning |
|---------|---------|
| `as_x()` | Cheap borrow-to-borrow cast |
| `to_x()` | Expensive borrow-to-owned conversion |
| `into_x()` | Owned-to-owned conversion (consumes self) |
| `from_x()` | Constructor from another type |
| `try_x()` | Fallible version of `x()` |
| `x_mut()` | Mutable variant of `x()` |

### Getters and Predicates

```rust
impl Room {
    // ✅ Getters: field name, no get_ prefix
    fn code(&self) -> &RoomCode { &self.code }
    fn player_count(&self) -> usize { self.players.len() }

    // ✅ Predicates: is_/has_/can_ prefix
    fn is_full(&self) -> bool { self.players.len() >= self.max_players }
    fn has_authority(&self) -> bool { self.authority.is_some() }
    fn can_join(&self, player: &Player) -> bool { !self.is_full() && !self.has_player(player) }
}

```

### Iterator Methods

```rust

impl Room {
    fn iter(&self) -> impl Iterator<Item = &Player> { ... }
    fn iter_mut(&mut self) -> impl Iterator<Item = &mut Player> { ... }
    fn player_ids(&self) -> impl Iterator<Item = PlayerId> + '_ { ... }
}

// Also implement IntoIterator for owned iteration
impl IntoIterator for Room {
    type Item = Player;
    type IntoIter = std::vec::IntoIter<Player>;
    fn into_iter(self) -> Self::IntoIter { self.players.into_iter() }
}

```

---

## Interoperability — Common Traits

Derive `Debug`, `Clone`, `PartialEq`, `Eq`, `Hash`, `Default`, `Serialize`/`Deserialize` eagerly on all public types. Implement `Display`, `From`/`TryFrom`, and `AsRef` where applicable.

See [rust-idioms-and-patterns.md](rust-idioms-and-patterns.md) for the full common traits derive checklist and conversion trait patterns.

---

## Type Safety

Use newtypes for domain identifiers and enums instead of booleans for function parameters to get compile-time safety. See [rust-idioms-and-patterns.md](rust-idioms-and-patterns.md) for newtypes and enums-over-booleans patterns.

### Bitflags for Options

```rust

// ✅ Use bitflags for combinable options
bitflags::bitflags! {
    pub struct Capabilities: u32 {
        const RELAY    = 0b0001;
        const OBSERVE  = 0b0010;
        const ADMIN    = 0b0100;
        const METRICS  = 0b1000;
    }
}

fn configure_client(caps: Capabilities) { todo!() }
configure_client(Capabilities::RELAY | Capabilities::OBSERVE);

```

> **Note:** Add `bitflags` to `Cargo.toml` to use; not currently in project dependencies.

---

## Flexibility

### Accept Generics, Return Concrete

```rust

pub fn set_name(&mut self, name: impl Into<String>) { self.name = name.into(); }
pub fn get_players(&self) -> &[Player] { &self.players }
pub fn active_players(&self) -> impl Iterator<Item = &Player> {
    self.players.iter().filter(|p| p.is_connected())
}

```

### Return Iterators, Not Collected Vecs

```rust

// ✅ Return iterator — caller decides how to use
pub fn find_rooms(&self, filter: &Filter) -> impl Iterator<Item = RoomInfo> + '_ {
    self.rooms.iter()
        .filter(move |r| filter.matches(r))
        .map(|r| r.info())
}
// Caller: let first_5: Vec<_> = server.find_rooms(&f).take(5).collect();

```

---

## Future-Proofing

### `#[non_exhaustive]` on Public Enums and Structs

```rust

// ✅ Allows adding variants without semver break
#[non_exhaustive]
pub enum DisconnectReason {
    Timeout,
    Kicked,
    ClientLeft,
    // Can add ServerShutdown later without breaking downstream
}

// ✅ Allows adding fields without semver break
#[non_exhaustive]
pub struct RoomInfo {
    pub code: RoomCode,
    pub player_count: usize,
    // Can add created_at later without breaking downstream
}

```

### Sealed Traits

```rust

// ✅ Prevent external implementations — you control the trait
mod private {
    pub trait Sealed {}
}

pub trait Transport: private::Sealed {
    fn send(&self, data: &[u8]) -> Result<(), Error>;
    fn recv(&self) -> Result<Vec<u8>, Error>;
}

// Only your types can implement Transport
impl private::Sealed for WebSocketTransport {}
impl Transport for WebSocketTransport { ... }

```

### Private Fields with Constructors

```rust

// ✅ Private fields → can change representation without breaking API
pub struct Duration {
    millis: u64,  // Private — could change to nanos later
}

impl Duration {
    pub fn from_secs(secs: u64) -> Self { Self { millis: secs * 1000 } }
    pub fn from_millis(millis: u64) -> Self { Self { millis } }
    pub fn as_secs(&self) -> u64 { self.millis / 1000 }
    pub fn as_millis(&self) -> u64 { self.millis }
}

```

---

## Documentation Standards

Every public item needs:

- Summary line (what, not how)
- `# Errors` listing each error variant
- `# Panics` if any panics are possible
- `# Safety` for unsafe code
- `# Examples` with compilable code
- Cross-references with `[`TypeName`]` links

```rust

/// Creates a new room with the given configuration.
///
/// # Errors
/// Returns [`CreateError::InvalidConfig`] if `max_players` is 0.
/// Returns [`CreateError::DuplicateCode`] if a room with this code exists.
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

## Public API Surface Minimization

Start with minimum visibility (`pub(crate)` by default), expand only when needed. Expose stable, documented methods as `pub`; keep implementation details `pub(crate)` or `pub(super)`.

---

## Semver Compatibility

**Breaking** (major bump): Removing public items, changing signatures, adding required fields without `#[non_exhaustive]`, changing trait bounds, changing enum variants.

**Non-breaking**: Adding new public items, adding variants/fields to `#[non_exhaustive]` types, adding default trait methods, weakening trait bounds.

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

// Implement IntoResponse for application errors
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

Use versioned routes (`/v2/...`). Return structured JSON errors. Log internal errors server-side; return generic messages
to clients. For REST APIs with multiple endpoints, maintain an OpenAPI specification to document the API contract.

---

## Signaling Server-Specific Guidance

### Protocol Messages

Use `#[non_exhaustive]` and `#[serde(tag = "type", rename_all = "snake_case")]` on all client/server message enums. See [WebSocket-protocol-patterns](./websocket-protocol-patterns.md) for full message design.

### Error Responses

```rust
#[derive(Debug, serde::Serialize)]
pub struct ApiError {
    pub error: String,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

```

### Connection State

Use the typestate pattern to prevent invalid operations. See [Rust-idioms-and-patterns](./rust-idioms-and-patterns.md) for the full typestate pattern.

---

## Agent Checklist

- [ ] All public types implement `Debug`, `Display`, `Clone` (where applicable)
- [ ] All public types implement `Send + Sync` (verify with `static_assertions`)
- [ ] Newtypes for IDs, codes, tokens — no raw `String`/`Uuid` in APIs
- [ ] Enums instead of booleans for function parameters
- [ ] `#[non_exhaustive]` on public enums and structs
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
