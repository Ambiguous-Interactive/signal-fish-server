# Skill: Clippy and Linting

<!--
  trigger: clippy, lint, warning, allow, deny, cargo clippy, ci
  | Configuring lints; resolving clippy warnings; CI setup
  | Core
-->

**Trigger**: When configuring lints, resolving clippy warnings, or setting up CI lint enforcement.

---

## When to Use

- Adding or modifying lint configuration in `Cargo.toml` or `clippy.toml`
- Resolving clippy warnings or errors
- Suppressing lints with `#[allow()]` annotations
- Setting up CI lint pipelines
- Understanding which lints to enable or disable

---

## When NOT to Use

- Writing code patterns (see [Rust Idioms And Patterns](./rust-idioms-and-patterns.md))
- Understanding error handling (see [Error Handling Guide](./error-handling-guide.md))

---

## TL;DR

- Run `cargo clippy --all-targets --all-features` with zero warnings before every commit.
- Enable pedantic lints project-wide, selectively allow noisy ones.
- Use restriction lints (`unwrap_used`, `indexing_slicing`, `panic`) to enforce safety.
- Suppress lints at the item level with `#[allow()]`, never at the crate level.
- Configure `clippy.toml` for project-specific thresholds.

---

## Recommended Cargo.toml Lint Configuration

> **Note:** This `[lints.clippy]` configuration is recommended but not yet in the project's `Cargo.toml`.

```toml
[lints.rust]
unsafe_code = "forbid"

[lints.clippy]
# ── Correctness (always errors) ──────────────────────────
correctness = { level = "deny", priority = -1 }

# ── Suspicious (likely bugs) ─────────────────────────────
suspicious = { level = "deny", priority = -1 }

# ── Style ────────────────────────────────────────────────
style = { level = "warn", priority = -1 }

# ── Complexity ───────────────────────────────────────────
complexity = { level = "warn", priority = -1 }

# ── Perf (performance issues) ────────────────────────────
perf = { level = "warn", priority = -1 }

# ── Pedantic (extra strictness) ──────────────────────────
pedantic = { level = "warn", priority = -1 }

# ── Selectively allow noisy pedantic lints ───────────────
missing_errors_doc = "allow"
missing_panics_doc = "allow"
module_name_repetitions = "allow"
must_use_candidate = "allow"
struct_excessive_bools = "allow"

# ── Key restriction lints (opt-in safety net) ────────────
unwrap_used = "warn"
indexing_slicing = "warn"
panic = "warn"
expect_used = "warn"
unreachable = "warn"
todo = "warn"
unimplemented = "warn"
dbg_macro = "warn"
print_stdout = "warn"
print_stderr = "warn"
string_to_string = "warn"
str_to_string = "warn"
clone_on_ref_ptr = "warn"
empty_structs_with_brackets = "warn"
format_push_string = "warn"
if_then_some_else_none = "warn"
mixed_read_write_in_expression = "warn"
rest_pat_in_fully_bound_structs = "warn"
same_name_method = "warn"
self_named_module_files = "warn"
semicolon_outside_block = "warn"
tests_outside_test_module = "warn"
unnecessary_self_imports = "warn"
wildcard_enum_match_arm = "warn"
```

---

## clippy.toml Configuration

The project's [clippy.toml](../../clippy.toml) is already configured:

```toml
cognitive-complexity-threshold = 30    # Max cognitive complexity per function
too-many-lines-threshold = 150         # Max lines per function
enum-variant-size-threshold = 200      # Trigger large_enum_variant above this
type-complexity-threshold = 300        # Max type complexity score
too-many-arguments-threshold = 8       # Max function parameters
missing-docs-in-crate-items = false    # Don't require docs on all crate items
trivial-copy-size-limit = 8            # Stack size threshold for pass-by-value
allowed-idents-below-min-chars = ["x", "y", "z", "i", "j", "k", "n", "f", "_"]
msrv = "1.88.0"                        # Minimum supported Rust version

# Disallow specific types in favor of project alternatives
disallowed-types = [
    { path = "std::collections::HashMap", reason = "Use DashMap for concurrent or FxHashMap for single-thread" },
    { path = "std::sync::Mutex", reason = "Use tokio::sync::Mutex in async code, or DashMap" },
]

# Disallow specific methods
disallowed-methods = [
    { path = "std::thread::sleep", reason = "Use tokio::time::sleep in async code" },
    { path = "std::env::var", reason = "Use clap for argument parsing" },
]
```

---

## Essential Clippy Lints by Category

### Correctness (Always Deny) — find bugs, never suppress without exceptional reason

| Lint                  | What it catches                       |
| --------------------- | ------------------------------------- |
| `eq_op`               | Comparing a value to itself           |
| `erasing_op`          | `x * 0` — probably a mistake          |
| `almost_swapped`      | Incomplete variable swap              |
| `invalid_regex`       | Regex that won't compile              |
| `infinite_iter`       | `.iter()` chains that never terminate |

### Perf (Performance Issues)

| Lint                   | What it catches                        |
| ---------------------- | -------------------------------------- |
| `needless_collect`     | `.collect()` immediately iterated      |
| `large_enum_variant`   | Enum with one huge variant             |
| `redundant_clone`      | `.clone()` on value about to drop      |
| `iter_on_single_items` | `.iter()` on single-element collection |

### Key Restriction Lints

```rust
// unwrap_used — use ? or explicit handling
let val = option.ok_or(Error::Missing)?;   // ✅

// indexing_slicing — use safe accessors
let first = vec.first().ok_or(Error::Empty)?;  // ✅

// panic/todo/unimplemented — return error instead
return Err(Error::UnexpectedState);  // ✅

// dbg_macro — use tracing
tracing::debug!(?value, "debugging value");  // ✅
```

---

## Suppressing Lints Properly

```rust
// ✅ Suppress at the item level with a reason
#[allow(clippy::too_many_arguments)] // Builder pattern not yet extracted
fn create_room(code: &str, name: &str, max_players: u32, ...) -> Result<Room, Error> { ... }

// ✅ Suppress for a single expression
#[allow(clippy::unwrap_used)] // SAFETY: regex literal is compile-time constant
let re = Regex::new(r"^\d+$").unwrap();

// ❌ NEVER suppress at crate level
#![allow(clippy::unwrap_used)]  // Disables lint for entire crate!

// ❌ NEVER suppress entire categories
#[allow(clippy::pedantic)]  // Hides real issues
```

---

## `cargo clippy --fix`

See [Rust Refactoring Guide](./rust-refactoring-guide.md) for the full `cargo clippy --fix` workflow.

```bash
cargo clippy --all-targets --all-features --fix --allow-dirty
```

Handles well: redundant clones, match simplification, unnecessary borrows, `use` suggestions.
Always review with `git diff` after.

---

## CI/CD Integration

```bash
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

Use `-D warnings` in CI to fail on any lint warning. **Always use `--all-targets`** — without it,
`#[cfg(test)]` modules and integration tests are not linted and warnings only surface in CI.

Don't use `#![deny(warnings)]` in libraries — new compiler warnings break downstream builds.
In binaries, use `-D warnings` as a CI flag instead of in source code.

---

## Common CI-Breaking Clippy Lints in Test Code

| Lint                   | What it catches                                           |
| ---------------------- | --------------------------------------------------------- |
| `collapsible_if`       | Nested `if` statements that can be combined with `&&`     |
| `needless_return`      | Explicit `return` statements that can be removed          |
| `single_match`         | `match` with one arm + wildcard that should be `if let`   |
| `needless_range_loop`  | Range loop only used to index a collection (use iterator) |

```rust
// ❌ needless_range_loop — auto-fix uses placeholder `<item>`, so manual fix required
for j in start..end {
    let val = lines[j].trim();
}

// ✅ Fix: use iterator (forward)
for line in lines.iter().take(end).skip(start) {
    let val = line.trim();
}

// ✅ Fix: use slice iterator (reverse)
for line in lines[start..end].iter().rev() {
    let val = line.trim();
}
```

```rust
// ❌ collapsible_if warning
if room.is_public() {
    if room.player_count() > 0 { assert!(room.is_joinable()); }
}

// ✅ Fix: combine with &&
if room.is_public() && room.player_count() > 0 { assert!(room.is_joinable()); }
```

---

## Project-Specific Recommendations

| Area                   | Lints to enforce                                                 |
| ---------------------- | ---------------------------------------------------------------- |
| **WebSocket handlers** | `unwrap_used`, `indexing_slicing` — untrusted input              |
| **Async code**         | Verify no `std::thread::sleep` via `disallowed-methods`          |
| **Serialization**      | `unwrap_used` — malformed data must not panic                    |
| **Tests**              | Allow `unwrap_used`, `indexing_slicing` — panics are ok in tests |

```rust
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    #![allow(clippy::indexing_slicing)]
}
```

---

## Agent Checklist

- [ ] `cargo clippy --all-targets --all-features` passes with zero warnings
- [ ] `cargo fmt -- --check` passes
- [ ] `[lints.clippy]` section in Cargo.toml with pedantic + restriction lints
- [ ] `clippy.toml` with project-specific thresholds
- [ ] No crate-level `#![allow(...)]` for safety lints
- [ ] Item-level `#[allow(...)]` has a comment explaining why
- [ ] CI runs clippy with `-D warnings`
- [ ] `disallowed-types` blocks `std::collections::HashMap` in favor of `DashMap`
- [ ] `disallowed-methods` blocks `std::thread::sleep` in async code
- [ ] Tests have appropriate `#![allow()]` overrides

---

## Related Skills

- [Rust Idioms And Patterns](./rust-idioms-and-patterns.md) — Code patterns that satisfy lints
- [Defensive Programming](./defensive-programming.md) — Patterns enforced by restriction lints
- [Rust Refactoring Guide](./rust-refactoring-guide.md) — Using `cargo clippy --fix` for automated fixes
- [Testing Core Patterns](./testing-core-patterns.md) — CI pipeline integration
