# Skill: Supply Chain Security — Audit Tools and Policy

<!--
  trigger: supply-chain, cargo-audit, cargo-deny, vulnerability, advisory, license, reproducible-build, cargo-lock
  | Auditing dependencies, cargo-deny policy, pinning versions, and reproducible builds
  | Security
-->

**Trigger**: When auditing dependencies for vulnerabilities, configuring cargo-deny policies,
or setting up reproducible builds.

---

## When to Use

- Running or configuring `cargo audit` or `cargo deny`
- Adding, updating, or reviewing third-party dependencies for security
- Configuring CI pipelines for supply chain gates
- Setting up reproducible or hermetic builds

## When NOT to Use

- SBOM generation and update automation (see [Supply Chain Sbom Updates](./supply-chain-sbom-updates.md))
- Application-level security (see [Web Service Security Auth](./web-service-security-auth.md))
- Choosing between crates for functionality (see [Dependency Management Cargo](./dependency-management-cargo.md))

## TL;DR

- Run `cargo audit` and `cargo deny check` in CI on every PR — block merges on failure.
- Pin security-critical dependencies with exact versions (`=1.2.3`) and always commit `Cargo.lock`.
- Build with `cargo build --locked` in CI to guarantee reproducibility.

---

## 1. Cargo Audit and Advisory Database

```bash
cargo audit              # Check against RustSec Advisory Database
cargo audit --json       # JSON output for CI parsing
```

Every advisory must result in: **Fix** (update the crate), **Ignore with justification** (document in `audit.toml`),
or **Deny** (replace the crate).

Document ignored advisories in `audit.toml` with rationale and expiry dates.
Never silently ignore — every ignore must have a justification and a revisit date.

---

## 2. Cargo Deny Configuration

This project's [deny.toml](../../deny.toml) enforces four policy areas:

### `[advisories]` — deny vulnerabilities, deny yanked crates

```toml
[advisories]
vulnerability = "deny"
yanked = "deny"
unmaintained = "workspace"
```

### `[licenses]` — allowlist of permissive licenses

Allows MIT, Apache-2.0, BSD, ISC, OpenSSL, Unicode, Zlib, 0BSD, CC0-1.0.
Always run `cargo deny check licenses` before adding a new dependency.

### `[bans]` — block problematic crates, detect duplicates

```toml
[bans]
multiple-versions = "warn"
wildcards = "deny"

[[bans.deny]]
name = "openssl"
wrappers = ["native-tls"]
reason = "Prefer rustls for TLS - openssl has had numerous CVEs"
```

```rust
// BAD: Pulling in openssl via native-tls feature
// reqwest = { version = "0.12", features = ["native-tls"] }

// GOOD: Using rustls backend to stay off the banned list
// reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }
```

### `[sources]` — no git dependencies in production

```toml
[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
allow-git = []
```

```rust
// BAD: Git dep bypasses crates.io auditing
// my-crate = { git = "https://github.com/user/my-crate" }

// GOOD: Use crates.io or vendor locally
// my-crate = "1.2.3"
// Or: [patch.crates-io] my-crate = { path = "third_party/my-crate" }
```

---

## 3. Dependency Pinning Strategy

### Exact Pinning for Security-Critical Deps

```toml
[dependencies]
rustls = "=0.23.20"        # TLS — pin exactly
ring = "=0.17.8"           # Crypto — pin exactly
jsonwebtoken = "=9.3.0"    # Auth tokens — pin exactly
serde = "1.0"              # Non-security — semver range OK
```

### Always Commit `Cargo.lock`

Applications and servers **must** commit `Cargo.lock`. This project is a server — `Cargo.lock` is committed.
Workspace crates share a single `Cargo.lock` at the root.

### Lockfile Verification in CI

```bash
# BAD: CI resolves fresh deps, may differ from lockfile
cargo build

# GOOD: Fails if lockfile is stale or missing
cargo build --locked
cargo test --locked
```

---

## 4. Reproducible Builds

### `--locked` Everywhere in CI

```yaml
steps:
  - run: cargo build --release --locked
  - run: cargo test --locked
  - run: cargo clippy --locked -- -D warnings
```

### Deterministic Compilation

```toml
[profile.release]
lto = "thin"
codegen-units = 1        # Single codegen unit for deterministic output
strip = "symbols"
overflow-checks = true
```

Docker multi-stage builds must also use `--locked`. See [Container Docker](./container-docker.md)
for the full Dockerfile pattern.

---

## 5. Tooling Execution Policy (Scripts and Hooks)

- Do not execute third-party CLIs via `npx` in CI scripts, hooks, or repo automation.
- Do not use external Docker images with mutable `:latest` tags in automation.
- Pin tool versions explicitly and validate versions at runtime.
- Prefer preinstalled/pinned tools over on-demand network downloads.

---

## 6. Handling RUSTSEC Unmaintained Advisories

Unmaintained advisories (e.g., RUSTSEC-2025-0134 for `rustls-pemfile`) indicate a crate
is no longer maintained and may accumulate unpatched vulnerabilities over time.

### Local Detection

```bash
# Check for advisories (requires cargo-deny)
cargo deny check advisories

# Or use the project script
./scripts/check-advisories.sh

# Full audit including licenses, bans, sources
./scripts/check-advisories.sh --full
```

### Resolution Decision Tree

```text
Unmaintained advisory detected:
    |
    +-- Functionality absorbed into another crate? --> Migrate to replacement
    |   (e.g., rustls-pemfile -> rustls-pki-types)
    |
    +-- Drop-in replacement available? --> Update Cargo.toml dependency
    |
    +-- No replacement, <100 lines? --> Vendor the functionality inline
    |
    +-- No replacement, complex? --> Fork and maintain, or add documented
        ignore with expiry in deny.toml
```

See dedicated migration example:
[Supply Chain Example Rustls Pemfile To Rustls Pki Types](./supply-chain-example-rustls-pemfile-to-rustls-pki-types.md).

---

## Monitoring Obligations

### Scheduled Checks

| Cadence | Check | Tool / Script | CI or Manual |
|---------|-------|---------------|--------------|
| Every PR | Policy compliance (advisories, licenses, bans, sources) | `cargo deny check` | CI — hard gate |
| Every PR | Vulnerability scan (second opinion) | `cargo audit` | CI — hard gate |
| Daily (cron) | New RUSTSEC advisories | `cargo-deny` + `cargo-audit` in CI schedule | CI — scheduled |
| Weekly | Outdated dependency report | `./scripts/check-outdated.sh` | Manual / informational |
| Per dependency change | Advisory pre-check | `./scripts/check-advisories.sh` | Manual before push |

### Response SLAs

| Severity | Examples | Response Time | Action |
|----------|----------|---------------|--------|
| **Critical** | RCE, auth bypass, data exfiltration | Same business day | Patch, update, or ban immediately. Open a PR within 4 hours. |
| **High** | Denial of service, privilege escalation, memory corruption | 3 business days | Update to patched version or add to ban list with replacement. |
| **Medium** | Information disclosure, unmaintained crate with no exploit | 2 weeks | Plan migration. Add documented ignore with expiry if needed. |
| **Low / Informational** | Deprecation notice, unmaintained but no known vulnerability | Next dependency update cycle | Track on watch list. Migrate opportunistically. |

### Escalation Path

When a new RUSTSEC advisory is discovered:

1. **Triage** — determine severity using the table above. Check if the
   advisory affects this project's usage (e.g., UTC-only usage of chrono
   may not be affected by a localtime vulnerability).
2. **Check for patch** — run `cargo update -p <crate>` to see if a fixed
   version is available. If yes, update and open a PR.
3. **No patch available** — follow the Resolution Decision Tree in
   section 6 above (migrate, vendor, or document ignore with expiry).
4. **Add ban if warranted** — if the crate should never be used again,
   add it to `deny.toml` `[[bans.deny]]` following the ban policy in
   [Dependency Management Cargo](./dependency-management-cargo.md).
5. **Verify** — run `cargo deny check` and `cargo audit` to confirm the
   advisory is resolved or properly ignored.

---

## Agent Checklist

- [ ] `cargo deny check` passes on every PR (advisories, licenses, bans, sources)
- [ ] `./scripts/check-advisories.sh` run locally before pushing dependency changes
- [ ] `cargo audit` runs in CI and on a daily schedule
- [ ] No unmaintained crate advisories — migrate to maintained alternatives
- [ ] `Cargo.lock` committed; `--locked` used for all CI build/test commands
- [ ] Security-critical deps pinned with exact versions (`=x.y.z`)
- [ ] No git dependencies — `[sources] allow-git = []`
- [ ] License allowlist reviewed — no copyleft in a permissive project
- [ ] Banned crates list includes `openssl`, `atty`, and known-problematic crates
- [ ] `audit.toml` ignores documented with rationale and expiry dates
- [ ] Docker builds use `--locked` and multi-stage pattern
- [ ] Automation scripts avoid `npx` runtime downloads and external `:latest` image tags
- [ ] Monitoring obligations followed — scheduled checks running, SLAs observed
- [ ] New RUSTSEC advisories triaged per escalation path within SLA

---

## Related Skills

- [Supply Chain Sbom Updates](./supply-chain-sbom-updates.md) — SBOM generation, update policy,
  CI pipeline, action compatibility
- [Dependency Management Cargo](./dependency-management-cargo.md) — Crate evaluation, feature flags,
  workspace dependency patterns
- [Web Service Security Hardening](./web-service-security-hardening.md) —
  Application-level security, auth, input validation, TLS
- [Container Docker](./container-docker.md) — Dockerfile hardening, image scanning, CI/CD pipelines
