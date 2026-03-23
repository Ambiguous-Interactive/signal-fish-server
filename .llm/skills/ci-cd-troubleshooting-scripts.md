# Skill: CI/CD Troubleshooting - Script & Test Validation Patterns

<!--
  trigger: miri failure, proptest miri, test code filtering, coverage flags,
  bash code blocks, shellcheck, yaml validation, toml validation,
  fixture exclusion, missing locked flag, cargo test multi-filter
  | Patterns 9-17: locked flag, test code filtering, coverage flags, Miri,
  bash code blocks, shell script pitfalls, fixture exclusion, YAML validation,
  cargo test syntax | Infrastructure
-->

**Trigger**: When debugging `--locked` flag omissions, Miri isolation failures, test
code false negatives, inconsistent coverage, bash code block validation, shell script
edge cases, test fixture exclusion mismatches, YAML/TOML fenced-block validation,
or `cargo test` multi-filter syntax errors.

---

## TL;DR

- **Missing `--locked`**: Add to all Cargo commands that resolve dependencies
- **AWK patterns**: Use prefix matching (`/^```rust/`) for flexibility, not exact patterns
- **Miri clock_gettime**: Add `#[cfg_attr(miri, ignore)]` to tests that call wall-clock APIs
- **Miri getcwd (proptest)**: Add `#[cfg_attr(miri, ignore)]` to all tests inside `proptest!` blocks
- **Bash code blocks**: Tag non-bash content as `text` (not `bash`) so shellcheck passes
- **POSIX shell portability**: Use `[[:space:]]` not `\s`; avoid `tac` on macOS

---

## Pattern 9: Missing `--locked` in CI Cargo Commands

### Symptom

```text
CI produces different results than local builds because dependency resolution
drifted from the checked-in Cargo.lock.
```

### Root Cause

Cargo commands in CI workflows missing the `--locked` flag allow Cargo to silently
update `Cargo.lock`, leading to non-reproducible CI results.

### Solution

Add `--locked` to all Cargo commands that resolve dependencies:

```yaml
- run: cargo test --locked --all-features
- run: cargo +nightly miri test --locked --lib
- run: cargo +nightly udeps --locked --all-targets
- run: cargo llvm-cov report --locked --fail-under-lines 70
```

Commands that do NOT need `--locked`: `cargo fmt` (no deps), `cargo publish`
(resolves from registry), `cargo miri setup` (tool setup), `cargo machete`
(static analysis only), `cargo sbom` (subcommand does not support the flag).

---

## Pattern 10: Incorrect Test Code Filtering (False Negatives)

### Symptom

```text
Panic policy check passes but production code contains panic!() or todo!()
macros that should have been caught.
```

### Root Cause

`filter_test_code` in `check-no-panics.sh` treats everything after `#[cfg(test)] mod foo;`
(an external module declaration) as test code, silently skipping production code below it.

### Solution

Use AWK brace-depth scanning to correctly determine module boundaries. Distinguish:

- `#[cfg(test)] mod foo;` (semicolon) — only 2 lines are test code; skip and continue
- `#[cfg(test)] mod tests { ... }` (braces) — track brace depth to find the closing `}`

Test with files that have `#[cfg(test)] mod` near the top (like `src/server.rs`).

---

## Pattern 11: Inconsistent Coverage Flags

Collection uses `--all-features --workspace` but `report` omits them, enforcing
against a different build. Apply build-selection flags only to collection; use
only reporting flags on `report`:

```yaml
- run: cargo llvm-cov --locked --all-features --workspace --lcov --output-path lcov.info
- run: cargo llvm-cov report --locked --fail-under-lines 70
```

---

## Pattern 12: Proptest Tests Fail Under Miri

### Symptom

```text
error: unsupported operation: `getcwd` not available when isolation is enabled
```

### Root Cause

Proptest's failure-persistence layer calls `std::env::current_dir()` to absolutize
source file paths. Miri blocks `getcwd` in isolation mode, aborting the entire test binary.

### Solution

Add `#[cfg_attr(miri, ignore)]` above each `#[test]` inside `proptest!` blocks,
with a comment explaining the `getcwd` / isolation reason:

```rust
proptest! {
    #[test]
    #[cfg_attr(miri, ignore)]  // proptest getcwd blocked by Miri isolation
    fn my_property_test(input in any::<u32>()) { /* ... */ }
}
```

CI config test `proptest_tests_ignored_under_miri` enforces this automatically.

Also see: **Miri clock_gettime** — add `#[cfg_attr(miri, ignore)]` to any test
that calls `chrono::Utc::now()` or any wall-clock API.

---

## Pattern 13: Bash Code Block Validation Fails on Non-Bash Syntax

`SC2283` or `bash -n` errors in docs — a code block tagged `bash` contains
non-bash syntax (TOML, YAML, etc.). Change the tag to `text` or split into
separate correctly-tagged blocks.

---

## Pattern 14: Shell Script Validation Pitfalls

### Symptom

```text
Error: Process completed with exit code 1.
# OR: CI reports broken links that are actually inside code blocks
```

### Root Causes and Fixes

**A. `grep` returns exit code 1 when no matches are found:**

```bash
# WRONG: grep returns 1 if no matches, killing the script under set -e
links=$(grep -oP '\[.*?\]\(.*?\)' "$file")

# CORRECT: Suppress no-match exit code
links=$(grep -oP '\[.*?\]\(.*?\)' "$file" || true)

# BETTER: Use AWK instead (always exits 0)
links=$(awk '/\[.*\]\(.*\)/' "$file")
```

**B. Link extraction scanning inside fenced code blocks:**

Use AWK to track fenced code blocks and strip inline code spans before extracting
links. Toggle `in_block` on each fence delimiter line, then `gsub` backtick-delimited
spans to empty string. Never use bare `grep` for link extraction in markdown files.

**C. Nested fence tracking (4+ backtick fences):**

The simple `in_fence = !in_fence` toggle breaks when a 4+ backtick outer fence
contains 3-backtick examples. Use **fence-width tracking**: record `fence_width = n`
(count of opening backticks) on open, and only close when `n >= fence_width` and
no trailing content. See the original `check-no-panics.sh` for the full AWK
implementation using this pattern.

**D. Link validator**: check both files (`-f`) and directories (`-d`) — a link
to a directory is valid but `[ ! -f ]` alone will falsely flag it as broken.

### Shell Script Checklist

- [ ] Every `grep` in a `set -e` script has `|| true` or is wrapped in `if`
- [ ] Link extraction skips fenced code blocks
- [ ] Link extraction strips inline code spans before matching
- [ ] Path validation checks both files (`-f`) and directories (`-d`)
- [ ] AWK is preferred over `grep` for pattern extraction
- [ ] Fence tracking handles nested fences (4+ backtick outer fences)
- [ ] File-list tooling is path-safe (`git diff -z` + `xargs -0`, or `while read -r`)
- [ ] Use `[[:space:]]` not `\s`; avoid `tac` on macOS

---

## Pattern 15: Test Fixture Exclusion Consistency

Test fixtures excluded from some validators but not all. Add
`! -path './.github/test-fixtures/*'` and `! -path './target/*'` to **every**
`find` command in every validator (JSON, YAML, TOML, Bash, Markdown, links, spelling).

---

## Pattern 16: YAML Validation Fails on Non-YAML Code Blocks

Code blocks tagged `yaml` containing non-YAML content (logs, shell, mixed)
cause validators to fail. Use `text` for logs/mixed, `bash` for shell.
Before tagging a block as `yaml`/`json`/`toml`/`bash`, verify the **entire**
block is valid in that language.

---

## Pattern 17: Cargo Test Multi-Filter Syntax Error

### Symptom

```text
cargo test --test ci_config_tests test_foo test_bar
# Second test name causes 'unexpected argument' error
```

### Root Cause

`cargo test [TESTNAME] [-- [ARGS]]` accepts only **one** positional TESTNAME
before `--`. A second positional arg is not a second filter — it is rejected as an unexpected argument.

### Solution

Pass multiple test names after the `--` separator. Always include `--locked`.

```bash
cargo test --locked --test ci_config_tests -- test_foo test_bar
```

---

## Pattern 18: Test Output Assertions Match Diagnostic Help Text

### Symptom

`must_not_contain` assertion fails because the forbidden substring (e.g.,
`"Cargo.lock"`) appears in diagnostic help text, not in error output.

### Root Cause

Bare substring assertions match the **entire** output, including help text
that legitimately mentions the same tokens.

### Solution

Use **error-line-specific prefixes** so assertions only match structured error output:

```rust
// WRONG: bare substring matches help text too
must_not_contain: vec!["Cargo.lock"],
// CORRECT: prefix matches only the error-listed file format
must_not_contain: vec!["  - Cargo.lock"],
```

**Prevention**: Document format coupling in both script and test. Prefer
referencing functions over printing raw pattern lists in diagnostics.

---

## Related Skills

- [Ecosystem Troubleshooting](./ci-cd-troubleshooting-ecosystem.md) — Language mismatch, cache, toolchain
- [Supply Chain Troubleshooting](./ci-cd-troubleshooting-supply-chain.md) — Action ref policy, Dockerfile
- [Diagnostic Workflow](./ci-cd-troubleshooting-categories.md) — Diagnosing CI failures
