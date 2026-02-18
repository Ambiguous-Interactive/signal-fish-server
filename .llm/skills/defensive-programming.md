# Skill: Defensive Programming

<!--
  trigger: panic, unwrap, indexing, validation, safety, bounds, overflow, cast
  | Eliminating runtime panics and compile-time safety
  | Core
-->

**Trigger**: When handling external input, fallible operations, or any code that could panic at runtime.

---

## When to Use

- Handling user input or external data
- Accessing slices, arrays, or collections by index
- Performing integer arithmetic or type casting
- Implementing constructors with validation
- Working with locks, mutexes, or shared state
- Any code path that could panic

---

## When NOT to Use

- Test code where `.unwrap()` and `.expect()` are acceptable
- Performance-critical inner loops already validated (see [Rust-performance-optimization](./rust-performance-optimization.md))

---

## TL;DR

- Use slice pattern matching instead of indexing (`vec.get(i)`, not `vec[i]`).
- Use exhaustive matching without wildcard catch-all on owned enums.
- Enforce invariants through constructors with private fields.
- Use `checked_`/`saturating_` arithmetic — never raw `as` casts.
- Validate all input at system boundaries (API handlers, deserialization points).

---

## Slice Pattern Matching Instead of Indexing

```rust
// ❌ Panics on empty input
let first = items[0];
let last = items[items.len() - 1];

// ✅ Pattern matching — compiler-verified exhaustive
match items.as_slice() {
    [] => return Err(Error::Empty),
    [single] => process_single(single),
    [first, rest @ ..] => process_with_first(first, rest),
    [first, .., last] => process_range(first, last),
}

// ✅ Safe accessors
let first = items.first().ok_or(Error::Empty)?;
let last = items.last().ok_or(Error::Empty)?;
let third = items.get(2).ok_or(Error::InsufficientItems)?;

// ✅ Iterator methods for safe element access
let second = items.iter().nth(1).ok_or(Error::TooFew)?;

```

---

## Explicit Field Initialization

```rust

// ❌ ..Default::default() hides new fields — they get default values silently
let config = ServerConfig {
    port: 8080,
    ..Default::default()  // New fields get defaults without review
};

// ✅ Initialize all fields explicitly — compiler error on new fields
let config = ServerConfig {
    port: 8080,
    max_rooms: 100,
    timeout: Duration::from_secs(30),
    tls: None,
};
// Adding a new field to ServerConfig forces updates everywhere

```

---

## Exhaustive Matching Without Wildcards

```rust

// ❌ Wildcard hides new variants — silent bugs
match transport {
    Transport::WebSocket => handle_ws(),
    Transport::Quic => handle_quic(),
    _ => handle_default(),  // New transport types silently fall through
}

// ✅ Explicit matching — compiler error on new variants
match transport {
    Transport::WebSocket => handle_ws(),
    Transport::Quic => handle_quic(),
    Transport::Tcp => handle_tcp(),
}
// Adding Transport::WebTransport forces handling everywhere

```

---

## Destructure Structs in Trait Impls

```rust

// ✅ Destructure to catch new fields at compile time
impl Display for PlayerInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { id, name, connected, room } = self;
        write!(f, "Player {name} ({id}), room: {room:?}, connected: {connected}")
    }
}

// ✅ Also in serialization, comparison, and conversion code
impl From<PlayerInfo> for PlayerDto {
    fn from(info: PlayerInfo) -> Self {
        let PlayerInfo { id, name, connected, room } = info;
        Self { player_id: id.to_string(), display_name: name, online: connected, room_code: room }
    }
}

```

Without destructuring, new fields are silently ignored.

---

## Enforce Construction via Constructors

Use private fields with validated constructors to make invalid states unrepresentable.
See [Rust Idioms and Patterns](rust-idioms-and-patterns.md) for the newtype pattern and enums-over-booleans.

```rust

// ✅ For structs: make fields private, provide builder or constructor
pub struct RateLimitConfig {
    requests_per_second: u32,  // Private!
    burst_size: u32,           // Private!
}

impl RateLimitConfig {
    pub fn new(rps: u32, burst: u32) -> Result<Self, ConfigError> {
        if rps == 0 { return Err(ConfigError::ZeroRate); }
        if burst < rps { return Err(ConfigError::BurstTooLow); }
        Ok(Self { requests_per_second: rps, burst_size: burst })
    }
}

```

---

## Integer Arithmetic Safety

```rust

// ✅ checked_ — returns None on overflow
let total = count
    .checked_mul(size)
    .and_then(|v| v.checked_add(offset))
    .ok_or(Error::Overflow)?;

// ✅ saturating_ — clamps at MAX/MIN (good for metrics)
let capped = current.saturating_add(increment);

// ✅ wrapping_ — explicit wrapping (sequence numbers, hashing)
let seq = current_seq.wrapping_add(1);

```

---

## Safe Casting with `TryFrom`

```rust

// ❌ `as` silently truncates
let port: u16 = large_number as u16;

// ✅ TryFrom returns error on out-of-range
let port: u16 = u16::try_from(large_number)
    .map_err(|_| ConfigError::PortOutOfRange(large_number))?;

// ✅ Saturating conversion when clamping is acceptable
let players: u16 = u16::try_from(count).unwrap_or(u16::MAX);

```

---

## Option/Result Chaining Patterns

Prefer `?`, `.ok_or()`, `.unwrap_or_default()`, `.map()`, `.and_then()`, and `.transpose()` over unwrapping.
Chain transformations to avoid intermediate unwraps.

See [error-handling-guide.md](error-handling-guide.md) for the full unwrap hierarchy and chaining examples.

---

## Lock Poisoning Handling

```rust

// ✅ Recover from poisoning when the data is still valid
let guard = state.lock().unwrap_or_else(|poisoned| {
    tracing::warn!("State lock was poisoned, recovering");
    poisoned.into_inner()
});

// ✅ Fail-closed when data integrity is critical
// SAFETY: Intentional panic — if credentials are corrupted, we must not proceed
let guard = credentials.lock()
    .expect("credentials lock poisoned — cannot proceed safely");
// Note: expect() is acceptable here only because failing-open would be a security
// risk. Prefer unwrap_or_else with recovery when data integrity allows it.

// ✅ Use RwLock for read-heavy workloads with similar poisoning handling
let data = state.read().unwrap_or_else(|poisoned| {
    tracing::warn!("Read lock poisoned, recovering");
    poisoned.into_inner()
});

```

---

## UTF-8 and String Safety

```rust

// ✅ Handle invalid UTF-8
let s = String::from_utf8(bytes)
    .map_err(|e| ParseError::InvalidUtf8(e.utf8_error()))?;

// ✅ Lossy conversion for logging
let s = String::from_utf8_lossy(&bytes);

// ✅ Safe slicing (byte-index slicing panics on invalid boundary)
let sub = s.get(0..4).ok_or(Error::InvalidSlice)?;

// ✅ floor_char_boundary() for safe truncation (Rust 1.80+)
let truncated = &s[..s.floor_char_boundary(max_len)];

// ✅ Character-based operations for Unicode safety
let first_char = s.chars().next().ok_or(Error::Empty)?;

```

---

## Division by Zero Guards

```rust

// ❌ Panics on zero divisor
let avg = total / count;
let pct = (part * 100) / whole;

// ✅ Checked division
let avg = total.checked_div(count).ok_or(Error::DivisionByZero)?;

// ✅ Provide default for zero case
let avg = if count > 0 { total / count } else { 0 };

// ✅ Use NonZeroU32 to make zero impossible
use std::num::NonZeroU32;
fn average(total: u32, count: NonZeroU32) -> u32 {
    total / count.get()  // Can never be zero
}

```

---

## `unreachable!()` vs `debug_assert!()`

```rust

// ✅ unreachable!() — states that are logically impossible
// Use when you've exhausted all valid states via prior checks
match self.validated_state {
    State::Ready => process(),
    State::Error => handle_error(),
    // If state enum is exhaustive and all variants handled,
    // no unreachable!() needed — the compiler checks it
}

// ✅ debug_assert!() — invariants that should hold but are checked only in debug
debug_assert!(index < self.len(), "index out of bounds: {index}");
debug_assert!(!players.is_empty(), "expected non-empty player list");
// Compiled out in release builds — zero runtime cost

// ❌ Don't use unreachable!() as a substitute for proper error handling
match user_input.parse::<u8>() {
    Ok(v) if v < 10 => process(v),
    _ => unreachable!(),  // WRONG: user input CAN be anything
}

```

---

## Input Validation at System Boundaries

```rust

pub async fn handle_join(
    Json(req): Json<JoinRequest>,
) -> Result<Json<JoinResponse>, AppError> {
    let room_code = RoomCode::new(&req.room_code)?;        // Validates format
    let player_name = PlayerName::new(&req.player_name)?;  // Validates length, chars
    let token = AuthToken::verify(&req.token)?;             // Validates auth
    // Interior code works with validated types only
    let response = server.join_room(room_code, player_name, token).await?;
    Ok(Json(response))
}

```

Validate at the handler entry point. Interior code should use validated newtypes, not raw strings.

---

## Type-State for Compile-Time Invariants

Use the typestate pattern to make invalid state transitions a compile error.

See [Rust Idioms and Patterns](rust-idioms-and-patterns.md) for the full typestate pattern with examples.

---

## Agent Checklist

- [ ] No direct indexing (`vec[i]`) without prior bounds check — use `.get()`, `.first()`, `.last()`
- [ ] No raw `+`, `-`, `*`, `/` on user-controlled values — use `checked_`/`saturating_`
- [ ] No `as` casts — use `TryFrom`
- [ ] No wildcard `_` catch-all on owned enums; `#[non_exhaustive]` on public enums
- [ ] Private fields with constructor validation; destructured in trait impls
- [ ] No `..Default::default()` in production code
- [ ] UTF-8 validation on untrusted input; no byte-index slicing without validation
- [ ] Lock poisoning handled explicitly; no mutex guards across `.await`; bounded channels only
- [ ] All API inputs validated at handler level; error types don't leak internal details

---

## Related Skills

- [error-handling-guide](./error-handling-guide.md) — Error type design and propagation
- [Rust-idioms-and-patterns](./rust-idioms-and-patterns.md) — Newtype and typestate patterns
- [api-design-guidelines](./api-design-guidelines.md) — Input validation at API boundaries
- [clippy-and-linting](./clippy-and-linting.md) — Restriction lints that enforce safety
