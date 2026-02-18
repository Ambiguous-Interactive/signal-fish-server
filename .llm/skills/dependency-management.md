# Skill: Dependency Management

<!--
  trigger: dependency, crate, cargo deny, audit, feature flag, workspace, update
  | Adding, auditing, and managing Rust crate dependencies
  | Feature
-->

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
- Performance tuning unrelated to dependencies (see [Rust-performance-optimization](./rust-performance-optimization.md))

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

The deny.toml configures: `vulnerability = "deny"`, `yanked = "deny"`, allowed licenses (MIT, Apache-2.0, BSD, ISC,
etc.), and banned/duplicate crate rules. Add `cargo deny check` to CI.

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

Put heavy/optional dependencies behind feature flags. Use `#[cfg(feature = "...")]` on modules and functions.
Don't put commonly-needed deps behind flags (if 90% of users need it, make it default).

**Native C dependencies:** If a feature pulls in a crate that requires native libraries (e.g., `rdkafka` needs `cmake`,
`libcurl-dev`, `libssl-dev`),
you must also update `.github/actions/install-build-deps/action.yml` and the `Dockerfile` builder stage.
See [container-and-deployment § CI Native Build Dependencies](./container-and-deployment.md) for details.

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

### Quick Detection

```bash
# Install cargo-machete (fast, stable, fewer false positives)
cargo install cargo-machete

# Find potentially unused dependencies
cargo machete

# Install cargo-udeps (slow, nightly, more thorough)
cargo install cargo-udeps

# Find unused dependencies and features
cargo +nightly udeps --all-targets

```

### Understanding the Output

Both tools may report false positives:

- **Procedural macros**: Used at compile-time, not in source
- **Build dependencies**: Used only in build.rs
- **Feature-gated dependencies**: May appear unused in default feature set
- **Platform-specific dependencies**: Used only on certain OS/architectures

### Remove vs Keep Decision Matrix

| Scenario | Decision | Action | Rationale |
|----------|----------|--------|-----------|
| Unused, last commit >1 year ago | **Remove immediately** | Delete from Cargo.toml | Unmaintained = security risk |
| Unused, actively maintained | **Remove** | Delete, can re-add later | Reduces supply chain surface |
| Unused behind feature flag | **Keep** | Document in comment | Optional dependency, may be used |
| Unused, added in last week | **Keep temporarily** | Review in 1 week | May be work-in-progress |
| False positive (proc macro) | **Keep** | Add `# keep:` comment | Actually used, tool limitation |
| Unused but API-stable | **Remove** | Delete | Stability doesn't justify keeping |
| Unused experimental dep | **Remove** | Delete | Experimental code should be in branch |

### Commenting Dependencies to Keep

```toml
[dependencies]
# Core runtime
tokio = { version = "1.49", features = ["rt-multi-thread", "macros"] }

# keep: Used by serde derive macros (false positive from cargo-udeps)
serde_derive = "1.0"

# keep: Platform-specific, used on Windows only
winapi = { version = "0.3", features = ["winuser"], optional = true }

# keep: Build dependency for code generation
quote = "1.0"

```

### Regular Audit Schedule

**Weekly automated audits:**

```yaml
# .github/workflows/unused-deps.yml
on:
  schedule:

    - cron: '0 0 * * 1'  # Weekly on Monday at 00:00 UTC

  push:
    branches: [main]
  pull_request:
    branches: [main]

jobs:
  unused-deps:
    runs-on: ubuntu-latest
    steps:

      - uses: actions/checkout@v4
      - run: cargo install cargo-machete
      - run: cargo machete


```

**Benefits:**

- Catches dependencies that become unused over time
- Prevents accumulation of technical debt
- Maintains clean, auditable dependency tree
- Reduces CI build times

**Manual quarterly reviews:**

In addition to automated weekly checks, perform manual quarterly reviews:

```bash
# 1. Run both tools
cargo machete
cargo +nightly udeps --all-targets

# 2. Review dependency tree
cargo tree | wc -l                    # Total dependencies
cargo tree -d                         # Duplicate dependencies
cargo tree --invert tokio             # Who depends on tokio?

# 3. Check maintenance status
cargo outdated --root-deps-only       # Are updates available?
cargo audit                           # Known vulnerabilities?

# 4. Review feature flags
cargo tree --features                 # Feature usage
# Are all features still needed?

# 5. Document findings in issue tracker
# Create tasks to remove unused deps, upgrade outdated deps
```

### Handling False Positives

When cargo-machete or cargo-udeps reports a false positive:

#### Step 1: Verify it is actually used

```bash
# Search for usage in code
rg "use.*dependency_name" src/
rg "dependency_name::" src/

# Check if it's a proc macro
cargo metadata --format-version=1 | jq '.packages[] | select(.name == "dependency_name") | .targets[] | .kind'
# If output includes "proc-macro", it's used at compile-time
```

#### Step 2: Document why it is kept

```toml

[dependencies]
# keep: Used by serde_derive proc macro for deserialization
# cargo-udeps reports false positive because proc macros are analyzed differently
serde = { version = "1.0", features = ["derive"] }

```

#### Step 3: Consider CI configuration

For known false positives, you can configure tools to ignore them:

```toml
# .cargo/machete.toml
[[skip]]
package = "serde_derive"
reason = "Used by serde derive macros"

```

### Audit Report Template

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

### async-trait (remove)
- Last used: Never (added for experiment)
- Reason unused: Experiment abandoned
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
- [x] Created PR #124 to document false positive
- [ ] Schedule next audit: 2026-05-16 (3 months)


```

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

#### Option 2: Evaluate alternatives

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

Vendor when: crate is unmaintained and you need patches, crate is <100 lines and you need one function,
or you've forked with significant modifications. Depend normally otherwise.

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
- [Rust-performance-optimization](./rust-performance-optimization.md) — Alternative crate recommendations
- [testing-strategies](./testing-strategies.md) — Testing with optional dependencies
