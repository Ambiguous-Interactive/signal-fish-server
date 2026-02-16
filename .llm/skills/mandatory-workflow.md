# Skill: Mandatory Workflow

<!-- trigger: workflow, lint, format, ci, commit, pre-commit, check, validate | Mandatory linting, formatting, and validation workflow for every change | Core -->

**Trigger**: Before committing any change — ensures all linters, formatters, and validation gates pass.

---

## When to Use

- After making ANY code change (Rust)
- Before committing or creating a PR
- When CI fails on lint/format/validation checks
- Setting up a new development environment

---

## When NOT to Use

- Choosing test strategies (see [testing-strategies](./testing-strategies.md))
- Configuring clippy rules (see [clippy-and-linting](./clippy-and-linting.md))

---

## TL;DR

1. **Read the code** before modifying it — NEVER modify code you haven't read.
2. **Run the appropriate linters** after every change (see table below).
3. **Zero warnings, zero errors** — all linters enforce strict compliance.

---

## Core Workflow (Every Change)

```bash
# 1. Before any change - read the code first
# NEVER modify code you haven't read

# 2. After Rust changes (ALWAYS run in order)
cargo fmt
cargo clippy --all-targets --all-features  # Zero warnings allowed
cargo test --all-features

# 3. Supply chain checks (run before pushing)
cargo deny --all-features check            # Advisories, licenses, bans, sources
```bash

### Pre-Push Validation

```bash
# Always run before pushing
scripts/check-ci-config.sh           # Catch CI configuration issues
scripts/check-msrv-consistency.sh    # Verify MSRV consistency (if MSRV-related changes)
```

- `check-ci-config.sh`: Catches outdated action versions incompatible with current `Cargo.lock`
  format (see [supply-chain-security](./supply-chain-security.md))
- `check-msrv-consistency.sh`: Validates all configuration files use the same Rust version as
  `Cargo.toml` (see [msrv-and-toolchain-management](./msrv-and-toolchain-management.md))

---

## Linting Requirements by File Type

| File Type        | Linter Commands                                          | Zero Tolerance |
| ---------------- | -------------------------------------------------------- | -------------- |
| **Rust** (`.rs`) | `cargo fmt && cargo clippy --all-targets --all-features` | No warnings    |

---

## Installing Linters (if missing)

```bash
# Rust toolchain
rustup component add rustfmt
rustup component add clippy
```bash

---

## Commit Format (User Executes, Not You)

**⛔ CRITICAL: YOU NEVER CREATE COMMITS. Provide these instructions to the user.**

Suggested commit message format for user:

```text
<type>: <imperative subject>

feat: add spectator mode to rooms
fix: resolve WebSocket cleanup race (#152)
perf: reduce allocations in message broadcast
test: add concurrency tests for room joins
docs: update protocol documentation
chore: update MSRV from 1.87.0 to 1.88.0
```

**When changes are ready:**

1. ✅ Verify all checks pass (fmt, clippy, test)
2. ✅ Provide commit instructions to user
3. ❌ NEVER execute `git commit` yourself

---

## PR Checklist

- [ ] `cargo fmt` — no formatting issues
- [ ] `cargo clippy --all-targets --all-features` — zero warnings
- [ ] `cargo test --all-features` — all tests pass
- [ ] `cargo deny --all-features check` — supply chain checks pass
- [ ] `scripts/check-ci-config.sh` — CI config validated
- [ ] `scripts/check-msrv-consistency.sh` — MSRV consistency verified (if MSRV changed)
- [ ] New code has exhaustive tests (see [testing-strategies](./testing-strategies.md))
- [ ] Documentation updated (see [documentation-standards](./documentation-standards.md))
- [ ] CHANGELOG updated for user-facing changes
- [ ] Breaking changes documented
- [ ] MSRV update documented (if applicable, see [msrv-and-toolchain-management](./msrv-and-toolchain-management.md))

---

## Security Checklist (Pre-Merge)

- [ ] No `.unwrap()` on user input (see [defensive-programming](./defensive-programming.md))
- [ ] All `.expect()` have `// SAFETY:` comments
- [ ] Rate limiting in place for public endpoints
- [ ] Auth tokens validated before privileged operations
- [ ] No secrets logged (check tracing fields)
- [ ] Input length limits enforced
- [ ] No integer overflow in arithmetic (use `saturating_*` or `checked_*`)
- [ ] No unchecked array/slice indexing (use `.get()` or `.last()`)

Use [web-service-security](./web-service-security.md) and [code-review-checklist](./code-review-checklist.md) skills for comprehensive audit.
