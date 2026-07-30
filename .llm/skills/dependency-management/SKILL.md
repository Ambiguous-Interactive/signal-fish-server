---
name: dependency-management
description: >-
  Manage Rust dependencies with Cargo, audit and deny tooling, feature hygiene, version
  selection, and MSRV compatibility. Use when adding, updating, removing, pinning, or auditing
  crates and dependency features.
---

# Dependency Management — Cargo Tooling

---

## When to Use

- Evaluating a new crate for inclusion
- Running `cargo deny check` or `cargo audit`
- Managing feature flags across workspace crates
- Finding and removing unused dependencies
- Reducing build times by trimming dependencies

---

## When NOT to Use

- Designing APIs for your own crate (see [API Design Guidelines](../api-design-guidelines/SKILL.md))
- MSRV compliance and version pinning (see [Dependency Management Versioning](references/versioning-and-msrv.md))
- Supply chain security auditing (see [Supply Chain Audit Policy](../supply-chain-security/SKILL.md))

---

## TL;DR

- Run `cargo deny check` before adding any new dependency.
- Prefer well-maintained, minimal crates — check downloads, recent commits, and license.
- Use feature flags to keep optional functionality behind gates.
- Use workspace dependencies for version consistency across sub-crates.
- Audit regularly with `cargo audit` and `cargo outdated`.

---

## cargo-deny for Security and License Compliance

This project uses [deny.toml](../../../deny.toml) for automated checks:

```bash
cargo deny check              # Run all checks
cargo deny check advisories   # Known vulnerabilities
cargo deny check licenses     # License compliance
cargo deny check bans         # Banned crates
cargo deny check sources      # Crate source restrictions
```

The deny.toml configures: `vulnerability = "deny"`, `yanked = "deny"`, allowed licenses (MIT, Apache-2.0, BSD, ISC,
NCSA, etc.), and banned/duplicate crate rules. Add `cargo deny check` to CI.

---

## Dependency Watch List and Ban Policy

### Watch List

Crates on this list are not banned but carry known risks and should be
monitored for migration opportunities. Review this list when upgrading
dependencies or evaluating alternatives.

| Crate | Risk / Reason | Action Trigger | Recommended Replacement |
|-------|---------------|----------------|------------------------|
| `once_cell` | `LazyLock` / `OnceLock` stabilized in std (Rust 1.80). Direct use is unnecessary; transitive use remains common. | When all transitive dependents drop it, or when adding new direct usage. | `std::sync::LazyLock` / `std::sync::OnceLock` |
| `async-trait` | Native `async fn` in traits stabilized in Rust 1.75. The proc-macro adds compile time and allocations. | When updating a trait that uses `#[async_trait]` or adding a new async trait. | Native `async fn` in trait definitions |
| `chrono` | Has had past RUSTSEC advisories (e.g., RUSTSEC-2020-0159 localtime_r segfault). Large dependency tree. | On any new RUSTSEC advisory, or when evaluating date/time needs. | `time` crate, or `jiff` for newer projects |
| `futures-util` | Large dependency tree; many utilities overlap with tokio built-ins (`tokio::select!`, `tokio::pin!`). | When the only usage is a single combinator that tokio provides natively. | `tokio` built-in utilities, `futures-lite` |
| `rmp-serde` | MessagePack codec on the relay compatibility path with a moderate maintenance cadence. | If `rmp-serde` goes unmaintained (>12 months without release) or a RUSTSEC advisory is filed. | `msgpacker`, `messagepack-rs`, or a reviewed fork |

### Ban List Policy

A crate should be added to the `[[bans.deny]]` list in `deny.toml` when
any of the following criteria are met:

1. **RUSTSEC advisory with no fix** — an active advisory exists and the
   crate author has not released a patched version.
2. **Officially deprecated or unmaintained** — the crate README or
   crates.io page states it is deprecated, or it has had no commits for
   18+ months with open security issues.
3. **Superseded by std** — the functionality has been absorbed into the
   Rust standard library (e.g., `atty` replaced by `std::io::IsTerminal`).
4. **Policy conflict** — the crate violates project security policy
   (e.g., `openssl` when the project mandates `rustls`).
5. **Transitive risk** — banning prevents accidental introduction via
   new dependencies (e.g., `native-tls` pulling in `openssl`).

### How to Add a Ban

1. Edit `deny.toml` — add a new `[[bans.deny]]` entry with `name` and
   `reason` fields. The reason must mention the recommended replacement.
2. Update `REQUIRED_DENY_BANS` in `tests/ci_config_tests.rs` — add a
   tuple `("crate_name", "reason_substring")` so the test enforces the
   ban is present.
3. Verify — run `cargo deny check bans` to confirm the ban is active.
4. Run the full test suite — `cargo test --all-features --test ci_config_tests`
   to confirm the new test entry passes.

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
| **MSRV**          | Minimum Rust version      | Higher than project MSRV             | **CRITICAL**: See [Dependency Management Versioning](references/versioning-and-msrv.md) |
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
See [Container Docker](../containers/SKILL.md) for details.

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

### Handling False Positives

For proc-macro false positives, add `# keep:` comments in Cargo.toml and configure
`.cargo/machete.toml` with `[[skip]]` entries. Search for usage with `rg "use.*dep" src/`
and `rg "dep::" src/` before removing. Check proc-macro status with `cargo metadata`.

---

## Build Time Impact

Use `cargo build --timings` to generate timing reports. Check dependency tree with `cargo tree | wc -l`
and duplicates with `cargo tree -d`. This project already uses `lto = "thin"` and `codegen-units = 1`
in release. Consider `sccache` or `mold` linker for development.

---

## When to Vendor vs Depend

Vendor when: crate is unmaintained and you need patches, crate is <100 lines and you need one function,
or you've forked with significant modifications. Depend normally otherwise.

---

## Local Dependency Health Checks

Before pushing dependency changes, run the advisory check script:

```bash
# Quick advisory check
./scripts/check-advisories.sh

# Full cargo-deny check (advisories + licenses + bans + sources)
./scripts/check-advisories.sh --full
```

To see which dependencies have newer versions available (informational, not a CI gate):

```bash
# Show all outdated dependencies
./scripts/check-outdated.sh

# Direct dependencies only
./scripts/check-outdated.sh --root-only
```

For unmaintained crate advisories, check if the functionality has been absorbed into
another crate (e.g., `rustls-pemfile` was absorbed into `rustls-pki-types`). See
[Supply Chain Audit Policy](../supply-chain-security/SKILL.md) for the full resolution
decision tree and migration examples.

---

## Agent Checklist

- [ ] `cargo deny check` passes before adding any dependency
- [ ] `./scripts/check-advisories.sh` run before pushing dependency changes
- [ ] `cargo audit` run regularly (weekly in CI)
- [ ] New dependencies evaluated against criteria table (including MSRV)
- [ ] Heavy/optional deps behind feature flags
- [ ] `Cargo.lock` committed (binary project)
- [ ] Build times monitored with `cargo build --timings`
- [ ] Duplicate versions investigated with `cargo tree -d`
- [ ] Vendored crates documented with reason in `third_party/`
- [ ] Watch list reviewed — no new direct usage of watch-listed crates without justification
- [ ] Ban list policy checked — any newly deprecated or superseded crate added to deny.toml

---

## Related Skills

- [Dependency Management Versioning](references/versioning-and-msrv.md) — MSRV, pinning, and recommended crates
- [Supply Chain Audit Policy](../supply-chain-security/SKILL.md) — Dependency security audits and SBOMs
- [MSRV Management](../msrv-management/SKILL.md) — MSRV updates and consistency
- [Clippy And Linting](../clippy-and-linting/SKILL.md) — CI integration for dependency checks
