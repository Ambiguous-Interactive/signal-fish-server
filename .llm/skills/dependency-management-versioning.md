# Skill: Dependency Management — Versioning and MSRV

<!--
  trigger: dependency version, msrv dependency, pin version, cargo outdated, crate alternatives, semver
  | Versioning strategy, MSRV compliance, and recommended crate alternatives
  | Feature
-->

**Trigger**: When pinning dependency versions, checking MSRV compatibility, or choosing between crate alternatives.

---

## When to Use

- Updating or pinning dependency versions
- Verifying a dependency supports the project MSRV
- Choosing between alternative crates for a use case
- Running `cargo outdated` or `cargo update`

---

## When NOT to Use

- Cargo tooling and feature flags (see [Dependency Management Cargo](./dependency-management-cargo.md))
- Supply chain security audits (see [Supply Chain Audit Policy](./supply-chain-audit-policy.md))

---

## TL;DR

- Verify MSRV compatibility before adding or updating any dependency.
- Use semver ranges for libraries, exact pins for security-critical crates.
- Update one dep at a time: check → test → deny → commit.
- Consult the recommended crate table before picking an alternative.

---

## Keeping Dependencies Up to Date

```bash
cargo outdated                     # See what's available
cargo update                       # Update patch versions (safe)
cargo update -p tokio              # Update specific crate
cargo outdated --root-deps-only    # Focus on direct deps
```

**Update workflow:** Update one dep at a time → `cargo check` → `cargo test --all-features` → `cargo deny check` →
commit as `deps: update <crate> to <version>`.

---

## Pinning vs Floating Versions

```toml
# GOOD: Use semver ranges for libraries (allow patch updates)
tokio = "1.49"          # Equivalent to >=1.49.0, <2.0.0

# GOOD: Pin exact versions only for security-critical crates
rustls = "=0.23.36"    # Exact version — no automatic updates

# GOOD: Use Cargo.lock (committed for binaries, not libraries)
# This project is a binary — Cargo.lock should be committed

# BAD: Don't use "*" wildcard
serde = "*"             # Any version — breaks reproducibility
```

---

## MSRV (Minimum Supported Rust Version) Compliance

**CRITICAL**: Before adding or updating any dependency, verify it supports the project's MSRV.

### Check Dependency MSRV

```bash
# View dependency's MSRV (if specified)
cargo metadata --format-version=1 | jq '.packages[] | select(.name == "rand") | .rust_version'

# Or check the dependency's Cargo.toml on crates.io or GitHub
curl -s https://crates.io/api/v1/crates/rand | jq '.crate.rust_version'
```

### MSRV Policy

- **Project MSRV**: Defined in `Cargo.toml` (`rust-version = "1.88.0"`)
- **All dependencies** must support this MSRV or lower
- **CI validates** MSRV compliance on every PR (`.github/workflows/ci.yml` msrv job)
- **MSRV updates** are coordinated changes affecting multiple files

### When Dependency Requires Newer Rust

If a dependency update requires a Rust version newer than the project MSRV:

**Option 1: Pin to older version** (preferred if possible)

```toml
[dependencies]
rand = "=0.9.0"  # Pin to version compatible with current MSRV
```

#### Option 2: Evaluate alternatives

- Search for alternative crates with lower MSRV
- Check if the feature requiring newer Rust is actually needed
- Consider forking and backporting if critical

**Option 3: Update project MSRV** (coordinated change)

- Follow the MSRV update checklist in [MSRV Management](./msrv-management.md)
- Update ALL configuration files: `Cargo.toml`, `rust-toolchain.toml`, `clippy.toml`, `Dockerfile`
- Run `scripts/check-msrv-consistency.sh` to verify consistency
- Document the MSRV bump in `CHANGELOG.md`

### MSRV Verification

```bash
# Verify current dependency tree is MSRV-compatible
cargo check --locked --all-targets

# Check for dependencies requiring newer Rust
cargo tree --all-features | grep -i "requires rustc"

# Run MSRV consistency check
./scripts/check-msrv-consistency.sh
```

See [MSRV Management](./msrv-management.md) for comprehensive guidance.

---

## Recommended Crate Alternatives

| Category             | Recommended            | Alternative           | Avoid                            |
| -------------------- | ---------------------- | --------------------- | -------------------------------- |
| **Async runtime**    | `tokio`                | —                     | `async-std` (less ecosystem)     |
| **HTTP server**      | `axum`                 | —                     | `actix-web` (different paradigm) |
| **Serialization**    | `serde` + `serde_json` | `simd-json` (perf)    | manual parsing                   |
| **Error handling**   | `thiserror` + `anyhow` | `eyre` + `color-eyre` | `failure` (deprecated)           |
| **Logging**          | `tracing`              | —                     | `log` (less structured)          |
| **Database**         | `sqlx`                 | `sea-orm`             | `diesel` (sync-first)            |
| **HTTP client**      | `reqwest`              | `hyper` (low-level)   | `ureq` (sync-only)               |
| **UUID**             | `uuid`                 | —                     | manual generation                |
| **CLI**              | `clap` (derive)        | —                     | `structopt` (merged into clap)   |
| **Hashing**          | `ahash`/`rustc-hash`   | —                     | default `SipHash` (slower)       |
| **Concurrent map**   | `dashmap`              | `flurry`              | `Mutex<HashMap>`                 |
| **Small vec**        | `smallvec`             | `arrayvec` (fixed)    | `tinyvec` (less maintained)      |
| **Bytes**            | `bytes`                | —                     | `Vec<u8>` for shared data        |
| **JWT**              | `jsonwebtoken`         | —                     | manual JWT parsing               |
| **Regex**            | `regex`                | —                     | manual parsing (unless trivial)  |
| **Crypto**           | `rustls` + `ring`      | —                     | `openssl` (C dependency)         |
| **Zero-copy**        | `rkyv`                 | `flatbuffers`         | `bincode` (not zero-copy)        |
| **Date/time**        | `chrono`               | `time`                | manual timestamp math            |
| **Property testing** | `proptest`             | `quickcheck`          | —                                |
| **Benchmarks**       | `criterion`            | `divan`               | manual timing                    |

---

## Project-Specific Dependency Notes

| Dependency           | Why we use it                            | Notes                                       |
| -------------------- | ---------------------------------------- | ------------------------------------------- |
| `axum`               | HTTP/WebSocket server framework          | Core framework — version-locked with tower  |
| `tokio`              | Async runtime                            | Multi-threaded, full features for server    |
| `dashmap`            | Concurrent room/player maps              | Replaces `Mutex<HashMap>` in hot paths      |
| `smallvec`           | Small player lists per room              | Stack-allocated for ≤8 players              |
| `bytes`              | Zero-copy network message passing        | Shared across broadcast recipients          |
| `rkyv`               | Zero-copy deserialization for game state | Performance-critical relay path             |
| `matchbox_signaling` | Base signaling protocol                  | Upstream crate we extend                    |
| `sqlx`               | PostgreSQL async driver                  | Behind `postgres` feature flag              |
| `redis`              | Session/pub-sub for distributed mode     | Connection manager for pooling              |
| `quinn`              | QUIC transport                           | Behind relay feature for UDP-like transport |

---

## Audit Report Template

After running audit tools, document findings:

```markdown
# Dependency Audit Report — 2026-02-16

## Summary
- Total dependencies: 87
- Unused dependencies found: 3
- Action required: Remove 2, keep 1 (false positive)

## Unused Dependencies

### futures (remove)
- Last used: 2025-08-10 (6 months ago)
- Reason unused: Refactored to use tokio directly
- Action: Remove from Cargo.toml
- PR: #123

### proc-macro2 (keep)
- Reported by: cargo-udeps
- Reason to keep: Used by quote proc macro
- False positive: Yes
- Action: Add `# keep:` comment to Cargo.toml
- PR: #124

## Follow-up Actions
- [x] Created PR #123 to remove unused dependencies
- [ ] Schedule next audit: 2026-05-16 (3 months)
```

---

## Agent Checklist

- [ ] **MSRV compatibility verified** — dependency supports project MSRV
- [ ] `scripts/check-msrv-consistency.sh` passes if MSRV changed
- [ ] No `*` version wildcards
- [ ] `cargo outdated` checked monthly
- [ ] Security-critical deps pinned with exact versions (`=x.y.z`)

---

## Related Skills

- [Dependency Management Cargo](./dependency-management-cargo.md) — Cargo tooling, feature flags, unused deps
- [MSRV Management](./msrv-management.md) — MSRV updates and consistency
- [Supply Chain Audit Policy](./supply-chain-audit-policy.md) — Dependency security audits and SBOMs
- [Rust Performance Optimization](./rust-performance-optimization.md) — Alternative crate recommendations
- [Testing Core Patterns](./testing-core-patterns.md) — Testing with optional dependencies
