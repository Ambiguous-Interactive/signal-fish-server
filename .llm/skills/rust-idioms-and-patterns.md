# Skill: Rust Idioms and Patterns

<!-- trigger: naming, convention, derive, newtype, builder, enum, trait, pattern, idiomatic | Canonical Rust patterns for writing and reviewing code | Core -->

**Trigger**: When writing new Rust code or reviewing code for idiomatic style and patterns.

---

## When to Use

- Writing any new Rust types, functions, or modules
- Reviewing code for naming conventions
- Choosing between newtypes, enums, and booleans
- Implementing standard traits (`From`, `Display`, `Default`)
- Deciding on builder vs constructor patterns

---

## When NOT to Use

- Performance-specific optimizations (see [Rust-performance-optimization](./rust-performance-optimization.md))
- Error type design specifically (see [error-handling-guide](./error-handling-guide.md))

---

## TL;DR

- Follow Rust naming conventions exactly — `as_`/`to_`/`into_` prefixes, no `get_` on getters, snake_case everywhere.
- Eagerly derive `Debug`, `Clone`, `PartialEq`, `Eq`, `Hash`, `Default` on all types where applicable.
- Use newtypes, enums-over-booleans, and the builder pattern to make illegal states unrepresentable.
- Accept borrows in parameters, return owned types. Use `Cow<str>` when ownership is conditional.
- Prefer `impl Iterator` returns over collecting to `Vec`.

---

## Naming Conventions

### Conversion Method Prefixes

| Prefix | Cost | Ownership | Example |
|--------|------|-----------|---------|
| `as_` | Free | Borrow→Borrow | `fn as_str(&self) -> &str` |
| `to_` | Expensive | Borrow→Owned | `fn to_string(&self) -> String` |
| `into_` | Free/cheap | Owned→Owned | `fn into_inner(self) -> T` |

```rust
// ✅ Correct naming
impl RoomCode {
    fn as_str(&self) -> &str { &self.0 }
    fn to_uppercase(&self) -> String { self.0.to_uppercase() }
    fn into_inner(self) -> String { self.0 }
}

// ❌ Wrong: using get_ prefix or wrong conversion prefix
impl RoomCode {
    fn get_str(&self) -> &str { &self.0 }         // Don't use get_
    fn into_uppercase(&self) -> String { todo!() }      // into_ implies ownership
}

```

### Getter Naming — No `get_` Prefix

```rust

impl Player {
    fn name(&self) -> &str { &self.name }
    fn id(&self) -> PlayerId { self.id }
    fn is_ready(&self) -> bool { self.ready }       // is_ for booleans
    fn has_authority(&self) -> bool { self.authority }// has_ for booleans
}

```

### Iterator Naming

Use `iter()`, `iter_mut()`, `into_iter()` for standard iteration. Use descriptive names like `player_ids()` for filtered/mapped iterators.

---

## Common Trait Implementations

Derive eagerly — if a type can implement a trait, it should:

| Trait | Derive when... |
|-------|---------------|
| `Debug` | Always |
| `Clone` | No unique resources (file handles, etc.) |
| `PartialEq`, `Eq` | All fields comparable |
| `Hash` | Used as map key or set element |
| `Default` | Sensible zero/empty value exists |
| `Display` | User-facing string representation |
| `Serialize`/`Deserialize` | API/persistence boundary |

`Send`/`Sync` are automatic if all fields are Send/Sync — don't manually impl.

---

## Conversion Traits

```rust
// Implement From for infallible conversions (Into is auto-derived)
impl From<String> for RoomCode {
    fn from(s: String) -> Self { Self(s) }
}

// TryFrom for fallible conversions
impl TryFrom<&str> for RoomCode {
    type Error = ValidationError;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        if s.len() != 6 { return Err(ValidationError::InvalidLength); }
        Ok(Self(s.to_uppercase()))
    }
}

// AsRef for cheap borrowed access
impl AsRef<str> for RoomCode {
    fn as_ref(&self) -> &str { &self.0 }
}

```

**In function signatures:** Use `impl Into<T>` for owned+flexibility, `&str`/`AsRef<T>` for read-only, `impl AsRef<Path>` for file paths.

---

## The Newtype Pattern

Wrap primitive types to add type safety and domain semantics.

```rust

// ✅ Newtype: prevents mixing up PlayerId and RoomId
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlayerId(pub(crate) Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoomCode(String);

// Constructors enforce invariants
impl RoomCode {
    pub fn new(code: &str) -> Result<Self, ValidationError> {
        if code.len() != 6 { return Err(ValidationError::InvalidLength); }
        if !code.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(ValidationError::InvalidCharacters);
        }
        Ok(Self(code.to_uppercase()))
    }

    pub fn as_str(&self) -> &str { &self.0 }
}

// ❌ Without newtypes: easy to mix up arguments
fn transfer_authority(from: Uuid, to: Uuid, room: String) { todo!() }

// ✅ With newtypes: compiler catches mistakes
fn transfer_authority(from: PlayerId, to: PlayerId, room: RoomCode) { todo!() }

```

---

## Enums Instead of Booleans

```rust

// ❌ Unclear: create_room("ABC123", true, false)

#[derive(Debug, Clone, Copy)]
pub enum Persistence { Temporary, Persistent }
#[derive(Debug, Clone, Copy)]
pub enum Visibility { Private, Public }

// ✅ Self-documenting
create_room("ABC123", Persistence::Persistent, Visibility::Private);

```

---

## Builder Pattern

```rust

// ✅ Builder for complex configuration
pub struct ServerConfig { /* private fields */ }

pub struct ServerConfigBuilder {
    port: u16,
    max_rooms: Option<usize>,
    tls: Option<TlsConfig>,
}

impl ServerConfigBuilder {
    pub fn new(port: u16) -> Self {
        Self { port, max_rooms: None, tls: None }
    }

    pub fn max_rooms(mut self, n: usize) -> Self { self.max_rooms = Some(n); self }
    pub fn tls(mut self, config: TlsConfig) -> Self { self.tls = Some(config); self }

    pub fn build(self) -> Result<ServerConfig, ConfigError> {
        Ok(ServerConfig {
            port: self.port,
            max_rooms: self.max_rooms.unwrap_or(100),
            tls: self.tls,
        })
    }
}

// Usage: ServerConfigBuilder::new(8080).max_rooms(50).tls(tls).build()?

```

---

## Typestate Pattern

Compile-time state machine — invalid transitions are unrepresentable:

```rust

pub struct Connection<S: ConnectionState> { inner: TcpStream, _state: PhantomData<S> }
pub struct Disconnected;
pub struct Connected;
pub struct Authenticated;

impl Connection<Disconnected> {
    pub fn connect(addr: &str) -> Result<Connection<Connected>, Error> { todo!() }
}
impl Connection<Connected> {
    pub fn authenticate(self, token: &str) -> Result<Connection<Authenticated>, Error> { todo!() }
}
impl Connection<Authenticated> {
    pub fn send(&self, msg: &Message) -> Result<(), Error> { todo!() }
}
// conn.send(&msg) on Connected won't compile — must authenticate first.

```

---

## Sealed Traits

Use sealed traits to prevent external implementations of traits you control.

See [api-design-guidelines.md](api-design-guidelines.md) for the sealed trait pattern and future-proofing strategies.

---

## Temporary Mutability

```rust

// ✅ Limit mutable scope with a block
let sorted_players = {
    let mut players = room.players().to_vec();
    players.sort_by_key(|p| p.join_time);
    players // Now immutable
};

```

---

## `#[must_use]` and `#[non_exhaustive]`

Use `#[must_use]` on Result-returning functions, guards, and important return values. Use `#[non_exhaustive]` on public enums and structs to allow adding variants/fields without semver breaks.

See [api-design-guidelines](./api-design-guidelines.md) for detailed future-proofing patterns.

---

## Exhaustive Matching

Always match all enum variants explicitly without wildcard `_` catch-alls on owned enums. Destructure structs in trait impls to catch new fields at compile time.

See [defensive-programming.md](defensive-programming.md) for exhaustive matching and destructuring patterns with examples.

---

## `Cow<str>` for Flexible Ownership

```rust
use std::borrow::Cow;

// ✅ Avoid cloning when the borrow suffices
fn format_error(code: u16, msg: Option<&str>) -> Cow<'_, str> {
    match msg {
        Some(m) => Cow::Borrowed(m),
        None => Cow::Owned(format!("Error {code}")),
    }
}

```

---

## Return `impl Iterator`, Accept Borrows

```rust

// ✅ Return impl Iterator — lazy, no allocation
fn active_players(&self) -> impl Iterator<Item = &Player> {
    self.players.iter().filter(|p| p.is_connected())
}

// ✅ Accept borrows, return owned
fn normalize(input: &str) -> String { input.trim().to_lowercase() }

```

---

## Use `clone_from()` Over `= .clone()`

```rust

// ✅ Reuse existing allocation
let mut buffer = String::with_capacity(1024);
buffer.clone_from(&new_data);  // Reuses buffer's allocation

// ❌ Discards existing allocation
buffer = new_data.clone();  // Allocates new, drops old

```

---

## Agent Checklist

- [ ] Naming: `as_`/`to_`/`into_` conversions, no `get_` prefix, `is_`/`has_` for bools
- [ ] Derive: `Debug`, `Clone`, `PartialEq`, `Eq`, `Hash`, `Default` where applicable
- [ ] Conversion: `From`/`TryFrom` on types, `Into`/`AsRef` in function parameters
- [ ] Newtypes for IDs, codes, and domain primitives
- [ ] Enums instead of booleans for function parameters
- [ ] Builder for 3+ optional fields
- [ ] `#[must_use]` on Result-returning functions, guards, and important values
- [ ] `#[non_exhaustive]` on public enums and structs
- [ ] No wildcard `_` catch-all on owned enums
- [ ] Destructure structs in trait impls to catch new fields
- [ ] Return `impl Iterator` instead of `Vec` where possible
- [ ] `clone_from()` when reusing an existing allocation

---

## Related Skills

- [api-design-guidelines](./api-design-guidelines.md) — Public API design patterns
- [defensive-programming](./defensive-programming.md) — Safety-first coding patterns
- [error-handling-guide](./error-handling-guide.md) — Error type conventions
- [Rust-refactoring-guide](./rust-refactoring-guide.md) — Modernizing code to idiomatic patterns
