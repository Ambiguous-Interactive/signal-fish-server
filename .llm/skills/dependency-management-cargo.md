# Skill: Dependency Management — Cargo Tooling

<!--
  trigger: dependency, crate, cargo deny, audit, feature flag, workspace, unused deps, cargo machete
  | Adding, auditing, and managing Rust crate dependencies with Cargo tools
  | Feature
-->

**Trigger**: When adding, updating, removing, or auditing Rust crate dependencies.

---

## When to Use

- Evaluating a new crate for inclusion
- Running `cargo deny check` or `cargo audit`
- Managing feature flags across workspace crates
- Finding and removing unused dependencies
- Reducing build times by trimming dependencies

---

## When NOT to Use

- Designing APIs for your own crate (see [api-design-guidelines](./api-design-guidelines.md))
- MSRV compliance and version pinning (see [dependency-management-versioning](./dependency-management-versioning.md))
- Supply chain security auditing (see [supply-chain-security](./supply-chain-audit-policy.md))

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
| **MSRV**          | Minimum Rust version      | Higher than project MSRV             | **CRITICAL**: See [dependency-management-versioning](./dependency-management-versioning.md) |
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
See [Container Docker § CI Native Build Dependencies](./container-docker.md) for details.

### Testing All Feature Combinations

```bash
cargo test                          # No features
cargo test --all-features           # All features
cargo test --features "postgres,kafka"  # Specific combinations
```

---

## Minimizing Dependency Count

```rust
// BAD: Adding a crate for one function (once_cell)
use once_cell::sync::Lazy;

// GOOD: Use std (stabilized in Rust 1.80)
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
cargo machete

# Install cargo-udeps (slow, nightly, more thorough)
cargo install cargo-udeps
cargo +nightly udeps --all-targets
```

### Remove vs Keep Decision Matrix

| Scenario | Decision | Action | Rationale |
|----------|----------|--------|-----------|
| Unused, last commit >1 year ago | **Remove immediately** | Delete from Cargo.toml | Unmaintained = security risk |
| Unused, actively maintained | **Remove** | Delete, can re-add later | Reduces supply chain surface |
| Unused behind feature flag | **Keep** | Document in comment | Optional dependency, may be used |
| Unused, added in last week | **Keep temporarily** | Review in 1 week | May be work-in-progress |
| False positive (proc macro) | **Keep** | Add `# keep:` comment | Actually used, tool limitation |
| Unused but API-stable | **Remove** | Delete | Stability doesn't justify keeping |

### Commenting Dependencies to Keep

```toml
[dependencies]
# Core runtime
tokio = { version = "1.49", features = ["rt-multi-thread", "macros"] }

# keep: Used by serde derive macros (false positive from cargo-udeps)
serde_derive = "1.0"

# keep: Platform-specific, used on Windows only
winapi = { version = "0.3", features = ["winuser"], optional = true }
```

### Handling False Positives

```bash
# Search for usage in code
rg "use.*dependency_name" src/
rg "dependency_name::" src/

# Check if it's a proc macro
cargo metadata --format-version=1 | jq '.packages[] | select(.name == "dependency_name") | .targets[] | .kind'
# If output includes "proc-macro", it's used at compile-time
```

For known false positives, configure tools to ignore them:

```toml
# .cargo/machete.toml
[[skip]]
package = "serde_derive"
reason = "Used by serde derive macros"
```

### Regular Audit Schedule

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

---

## Build Time Impact

Use `cargo build --timings` to generate timing reports. Check dependency tree with `cargo tree | wc -l`
and duplicates with `cargo tree -d`. This project already uses `lto = "thin"` and `codegen-units = 1`
in release. Consider `sccache` or `mold` linker for development.

---

## When to Vendor vs Depend

Vendor when: crate is unmaintained and you need patches, crate is <100 lines and you need one function,
or you've forked with significant modifications. Depend normally otherwise.

This project vendors `rmp` (MessagePack): `[patch.crates-io] rmp = { path = "third_party/rmp" }`

---

## Agent Checklist

- [ ] `cargo deny check` passes before adding any dependency
- [ ] `cargo audit` run regularly (weekly in CI)
- [ ] New dependencies evaluated against criteria table (including MSRV)
- [ ] Heavy/optional deps behind feature flags
- [ ] `Cargo.lock` committed (binary project)
- [ ] Build times monitored with `cargo build --timings`
- [ ] Duplicate versions investigated with `cargo tree -d`
- [ ] Vendored crates documented with reason in `third_party/`

---

## Related Skills

- [dependency-management-versioning](./dependency-management-versioning.md) — MSRV, pinning, and recommended crates
- [supply-chain-security](./supply-chain-audit-policy.md) — Dependency security audits and SBOMs
- [msrv-management](./msrv-management.md) — MSRV updates and consistency
- [clippy-and-linting](./clippy-and-linting.md) — CI integration for dependency checks
