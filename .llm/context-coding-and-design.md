# Software Design Philosophy

See [Rust Idioms and Patterns](skills/rust-idioms-and-patterns/SKILL.md) and
[SOLID Principles Enforcement](skills/solid-principles-enforcement/SKILL.md) for full details.

- Strive for zero comments. Code must be self-documenting through descriptive,
  unabbreviated names for classes, functions, and variables -- prefer the longer,
  clearer name over the shortened one. A comment is a last resort for exceptional
  circumstances only: a non-obvious "why" (invariant, safety proof, external-contract
  constraint) that a better name cannot carry. Never comment "what", and never keep
  history in comments -- git and `progress/` notes own history.
- When a comment is genuinely required, keep it minimal. A multi-line, non-doc
  comment uses the block form (not stacked `//` lines):

  ```rust
  /*
      The re-serialization envelope can exceed the frame cap even when the
      admitted frame did not; the write-side check is the binding limit.
  */
  ```

- Apply SOLID, DRY, and Clean Architecture consistently
- Build lightweight, zero-cost abstractions (value types -> borrows -> generics -> `Arc`/`Box`)
- Extract repeated patterns into shared modules; use domain types to encapsulate validation
- Don't add patterns "just in case" -- start simple, refactor when patterns emerge

## Rust Coding Standards

Performance: [Rust Performance Optimization](skills/rust-performance-optimization/SKILL.md)
and [Async Rust Best Practices](skills/async-rust-best-practices/SKILL.md)

Error handling: [Error Handling Guide](skills/error-handling-guide/SKILL.md)

Defensive programming: [Defensive Programming](skills/defensive-programming/SKILL.md)

Linting: [Clippy and Linting](skills/clippy-and-linting/SKILL.md)

Key rules:

- Always use `Result<T, E>` with `?` -- never `.unwrap()` in production code
  unless a compile-time invariant is documented with `SAFETY:` and an explicit
  `#[allow(clippy::unwrap_used)]`; apply the same documented exception rule to
  production `.expect()`
- Validate all input at system boundaries
- Use `checked_`/`saturating_` arithmetic -- never raw `as` casts that truncate
- Use `Bytes` for shared network data and `DashMap` for concurrent access;
  choose specialized collections only when a production measurement justifies them
- Never hold a sync `Mutex` across `.await`; use bounded channels with backpressure
- Use structured logging with `tracing` -- no string interpolation in log macros
- Classify our OWN errors with types, never by matching error strings. When a
  server/coordinator/manager failure must be mapped to a client `ErrorCode` (or
  otherwise branched on), the failing function returns a typed error enum and the
  caller maps it with an exhaustive `match` (or a `fn error_code(&self) ->
  ErrorCode` on the type). Both `e.to_string().contains("...")` AND a
  non-exhaustive `e.downcast_ref::<X>()` chain on an error we constructed are
  forbidden -- they are fragile and silently misclassify a cause they do not name
  (a new business rejection falls through to the catch-all code). The exhaustive
  `error_code()` instead turns a missed classification into a COMPILE error (see
  the `PlayerReadyError`, `ReconnectionError`, and `JoinRoomError` enums; the
  last replaced a `downcast` + `anyhow!("Room is full")` shape that had been
  mis-reporting a full room as `ROOM_CREATION_FAILED` instead of `ROOM_FULL`).
  String matching is acceptable ONLY for opaque EXTERNAL errors with no typed
  representation (e.g. sqlx/db/lock backends in `src/retry.rs`,
  `src/distributed.rs`). When a typed error also carries the wire-facing
  `reason`, make `Display` reproduce it so the type is the single source of truth
  for both code and text.
