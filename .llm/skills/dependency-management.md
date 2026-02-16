# Skill: Dependency Management

<!-- trigger: dependency, crate, cargo deny, audit, feature flag, workspace, update | Adding, auditing, and managing Rust crate dependencies | Feature -->

**Trigger**: When adding, updating, removing, or auditing Rust crate dependencies.

---

## When to Use

- Evaluating a new crate for inclusion
- Running `cargo deny check` or `cargo audit`
- Managing feature flags across workspace crates
- Updating or pinning dependency versions
- Reducing build times by trimming dependencies

---

## When NOT to Use

- Designing APIs for your own crate (see [api-design-guidelines](./api-design-guidelines.md))
- Performance tuning unrelated to dependencies (see [rust-performance-optimization](./rust-performance-optimization.md))

---

## TL;DR

- Run `cargo deny check` before adding any new dependency.
- Prefer well-maintained, minimal crates — check downloads, recent commits, and license.
- Use feature flags to keep optional functionality behind gates.
- Use workspace dependencies for version consistency across sub-crates.
- Audit regularly with `cargo audit` and `cargo outdated`.

---

## cargo-deny for Security and License Compliance

This project uses [deny.toml](../../deny.toml) for automated checks:

```bash
cargo deny check              # Run all checks
cargo deny check advisories   # Known vulnerabilities
cargo deny check licenses     # License compliance
cargo deny check bans         # Banned crates
cargo deny check sources      # Crate source restrictions
```

The deny.toml configures: `vulnerability = "deny"`, `yanked = "deny"`, allowed licenses (MIT, Apache-2.0, BSD, ISC, etc.), and banned/duplicate crate rules. Add `cargo deny check` to CI.

---

## Choosing Between Crates — Evaluation Criteria

| Criterion         | Check                     | Red flag                             | Notes                                   |
| ----------------- | ------------------------- | ------------------------------------ | --------------------------------------- |
| **Maintenance**   | Last commit date          | >1 year inactive                     | Check GitHub activity, not just release |
| **Downloads**     | crates.io stats           | <1000 total downloads                | Higher downloads = more battle-tested   |
| **Dependencies**  | `cargo tree -p <crate>`   | Pulls in 50+ transitive deps         | Increases supply chain risk             |
| **License**       | Cargo.toml license field  | GPL/AGPL in MIT project              | Must be compatible with project license |
| **Safety**        | `unsafe` usage            | Lots of unsafe without justification | Review unsafe code carefully            |
| **Build time**    | `cargo build --timings`   | >10s for one crate                   | Impacts developer productivity          |
| **MSRV**          | Minimum Rust version      | Higher than project MSRV             | **CRITICAL**: See MSRV guidance below   |
| **API stability** | Version number, changelog | Frequent breaking releases           | Check semver adherence                  |

### Decision Process

1. Do we actually need this crate? Can we write <50 lines instead?
2. Does it have acceptable license, no known vulnerabilities, and reasonable transitive deps?
3. Will it impact build time significantly? (`cargo build --timings`)

---

## Feature Flag Management

### This Project's Feature Flags

```toml
[features]
default = []
dynamodb = ["aws-config", "aws-sdk-dynamodb"]
postgres = ["sqlx", "tokio-postgres", "refinery", "sea-orm"]
aws-ses = ["aws-config", "aws-sdk-sesv2"]
aws-cost = ["aws-config", "aws-sdk-costexplorer"]
kafka = ["rdkafka"]
```

### Best Practices

Put heavy/optional dependencies behind feature flags. Use `#[cfg(feature = "...")]` on modules and functions. Don't put commonly-needed deps behind flags (if 90% of users need it, make it default).

**Native C dependencies:** If a feature pulls in a crate that requires native libraries (e.g., `rdkafka` needs `cmake`, `libcurl-dev`, `libssl-dev`), you must also update `.github/actions/install-build-deps/action.yml` and the `Dockerfile` builder stage. See [container-and-deployment § CI Native Build Dependencies](./container-and-deployment.md) for details.

### Testing All Feature Combinations

```bash
cargo test                          # No features
cargo test --all-features           # All features
cargo test --features "postgres,kafka"  # Specific combinations
```

---

## Minimizing Dependency Count

```rust
// ❌ Adding a crate for one function (once_cell)
use once_cell::sync::Lazy;

// ✅ Use std (stabilized in Rust 1.80)
use std::sync::LazyLock;
static CONFIG: LazyLock<Config> = LazyLock::new(|| load_config());
```

**Rule of thumb:** If you can write it in <50 lines without sacrificing correctness, don't add a dependency.

---

## Workspace Dependencies

```toml
# Root Cargo.toml — single source of truth for versions
[workspace.dependencies]
tokio = { version = "1.49", features = ["rt-multi-thread", "macros"] }
serde = { version = "1.0", features = ["derive"] }
tracing = "0.1"

# Sub-crate Cargo.toml — reference workspace versions
[dependencies]
tokio = { workspace = true }
serde = { workspace = true }
tracing = { workspace = true }
```

---

## Finding Unused Dependencies

```bash
# Install cargo-machete
cargo install cargo-machete

# Find potentially unused dependencies
cargo machete

# Note: May have false positives for procedural macros and build deps
```

---

## Keeping Dependencies Up to Date

```bash
cargo outdated                     # See what's available
cargo update                       # Update patch versions (safe)
cargo update -p tokio              # Update specific crate
cargo outdated --root-deps-only    # Focus on direct deps
```

**Update workflow:** Update one dep at a time → `cargo check` → `cargo test --all-features` → `cargo deny check` → commit as `deps: update <crate> to <version>`.

---

## Pinning vs Floating Versions

```toml
# ✅ Use semver ranges for libraries (allow patch updates)
tokio = "1.49"          # Equivalent to >=1.49.0, <2.0.0

# ✅ Pin exact versions only for security-critical crates
rustls = "=0.23.36"    # Exact version — no automatic updates

# ✅ Use Cargo.lock (committed for binaries, not libraries)
# This project is a binary — Cargo.lock should be committed

# ❌ Don't use "*" wildcard
serde = "*"             # Any version — breaks reproducibility
```

---

## Build Time Impact

Use `cargo build --timings` to generate timing reports. Check dependency tree with `cargo tree | wc -l`
and duplicates with `cargo tree -d`. This project already uses `lto = "thin"` and `codegen-units = 1`
in release. Consider `sccache` or `mold` linker for development.

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

**Option 2: Evaluate alternatives**

- Search for alternative crates with lower MSRV
- Check if the feature requiring newer Rust is actually needed
- Consider forking and backporting if critical

**Option 3: Update project MSRV** (coordinated change)

- Follow the MSRV update checklist in [msrv-and-toolchain-management](./msrv-and-toolchain-management.md)
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

See [msrv-and-toolchain-management](./msrv-and-toolchain-management.md) for comprehensive guidance.

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

## When to Vendor vs Depend

Vendor when: crate is unmaintained and you need patches, crate is <100 lines and you need one function, or you've forked with significant modifications. Depend normally otherwise.

This project vendors `rmp` (MessagePack): `[patch.crates-io] rmp = { path = "third_party/rmp" }`

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

## Agent Checklist

- [ ] **MSRV compatibility verified** — dependency supports project MSRV (see above section)
- [ ] `scripts/check-msrv-consistency.sh` passes if MSRV changed
- [ ] `cargo deny check` passes before adding any dependency
- [ ] `cargo audit` run regularly (weekly in CI)
- [ ] New dependencies evaluated against criteria table (including MSRV)
- [ ] Heavy/optional deps behind feature flags
- [ ] `Cargo.lock` committed (binary project)
- [ ] No `*` version wildcards
- [ ] `cargo outdated` checked monthly
- [ ] Build times monitored with `cargo build --timings`
- [ ] Duplicate versions investigated with `cargo tree -d`
- [ ] Vendored crates documented with reason in `third_party/`

---

## Related Skills

- [msrv-and-toolchain-management](./msrv-and-toolchain-management.md) — MSRV updates and consistency
- [clippy-and-linting](./clippy-and-linting.md) — CI integration for dependency checks
- [supply-chain-security](./supply-chain-security.md) — Dependency security audits
- [rust-performance-optimization](./rust-performance-optimization.md) — Alternative crate recommendations
- [testing-strategies](./testing-strategies.md) — Testing with optional dependencies
