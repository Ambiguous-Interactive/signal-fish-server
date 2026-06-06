# Software Design Philosophy

See [Rust Idioms and Patterns](skills/rust-idioms-and-patterns.md) and
[SOLID Principles Enforcement](skills/solid-principles-enforcement.md) for full details.

- Code should be self-documenting -- only comment "why", never "what"
- Apply SOLID, DRY, and Clean Architecture consistently
- Build lightweight, zero-cost abstractions (value types -> borrows -> generics -> `Arc`/`Box`)
- Extract repeated patterns into shared modules; use domain types to encapsulate validation
- Don't add patterns "just in case" -- start simple, refactor when patterns emerge

## Rust Coding Standards

Performance: [Rust Performance Optimization](skills/rust-performance-optimization.md)
and [Async Rust Best Practices](skills/async-rust-best-practices.md)

Error handling: [Error Handling Guide](skills/error-handling-guide.md)

Defensive programming: [Defensive Programming](skills/defensive-programming.md)

Linting: [Clippy and Linting](skills/clippy-and-linting.md)

Key rules:

- Always use `Result<T, E>` with `?` -- never `.unwrap()` in production code
- Validate all input at system boundaries
- Use `checked_`/`saturating_` arithmetic -- never raw `as` casts that truncate
- Use `Bytes` for network data, `SmallVec` for small collections, `DashMap` for concurrent access
- Never hold a sync `Mutex` across `.await`; use bounded channels with backpressure
- Use structured logging with `tracing` -- no string interpolation in log macros
