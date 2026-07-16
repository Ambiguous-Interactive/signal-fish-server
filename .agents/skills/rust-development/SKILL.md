---
name: rust-development
description: Design, implement, optimize, lint, and refactor Signal Fish Rust code. Use for .rs changes involving APIs, async Tokio behavior, concurrency, errors, panic avoidance, idioms, performance, architecture, Clippy, SOLID design, or safe incremental refactoring.
---

<!-- markdownlint-disable MD013 -->

# Rust Development

Preserve correctness first, then measure performance. Add tests with the implementation and keep production code warning-free and panic-resistant.

## Route the task

- Read [project-coding-and-design.md](references/project-coding-and-design.md) for repository-wide Rust conventions.
- Read [api-design-guidelines.md](references/api-design-guidelines.md) for public APIs and protocol types.
- Read [async-Rust-best-practices.md](references/async-rust-best-practices.md) for Tokio, cancellation, channels, and concurrency.
- Read [error-handling-guide.md](references/error-handling-guide.md) and [defensive-programming.md](references/defensive-programming.md) for fallible paths and panic avoidance.
- Read [Rust-idioms-and-patterns.md](references/rust-idioms-and-patterns.md) and [solid-principles-enforcement.md](references/solid-principles-enforcement.md) for design choices.
- Read [Rust-performance-optimization.md](references/rust-performance-optimization.md) only after identifying a measurable hot path.
- Read [Rust-refactoring-guide.md](references/rust-refactoring-guide.md) for behavior-preserving restructuring.
- Read [clippy-and-linting.md](references/clippy-and-linting.md) for lint configuration or diagnostics.

Invoke `$testing-rust` for test design. Run `cargo fmt`, `cargo clippy --all-targets --all-features`, and `cargo test --all-features` in that order for Rust changes.
