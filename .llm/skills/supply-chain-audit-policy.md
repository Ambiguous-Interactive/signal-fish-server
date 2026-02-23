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

- SBOM generation and update automation (see [supply-chain-sbom-updates](./supply-chain-sbom-updates.md))
- Application-level security (see [web-service-security-auth](./web-service-security-auth.md))
- Choosing between crates for functionality (see [dependency-management-cargo](./dependency-management-cargo.md))

## Rationalizations to Reject

| Excuse | Why It's Wrong | Required Action |
|--------|---------------|-----------------|
| "We only use well-known crates" | Popular crates get compromised too. Transitive deps hide risk. | Audit the full tree. Run `cargo deny check` on every PR. |
| "We'll audit before release" | Vulnerabilities accumulate silently between audits. | Run `cargo audit` in CI on every push and on a daily schedule. |
| "Pinning versions slows us down" | Unpinned deps can silently pull in breaking or malicious updates. | Pin security-critical deps exactly. Always commit `Cargo.lock`. Build with `--locked`. |

---

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

```rust
// BAD: Silently ignoring an advisory with no justification
// Just add RUSTSEC-2024-0001 to the ignore list and move on

// GOOD: Documented ignore with expiry and rationale in audit.toml
// ignore RUSTSEC-2024-0001: "Utc-only usage, not exploitable", expires 2026-06-01
```

### `audit.toml` Configuration

```toml
[advisories]
ignore = [
    # RUSTSEC-2024-0001: Utc-only, not exploitable. Revisit by 2026-06-01.
    "RUSTSEC-2024-0001",
]
```

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

```toml
[licenses]
allow = ["MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception",
         "BSD-2-Clause", "BSD-3-Clause", "ISC", "OpenSSL",
         "Unicode-DFS-2016", "Unicode-3.0", "Zlib", "0BSD", "CC0-1.0"]
```

```rust
// BAD: Adding a GPL-licensed crate to a permissive project
// Cargo.toml: my-gpl-dep = "1.0"  # License: GPL-3.0

// GOOD: Verify license before adding: cargo deny check licenses
```

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

### Docker Multi-Stage with Locked Deps

```dockerfile
FROM rust:1.83-slim AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN mkdir src && echo "fn main() {}" > src/main.rs \
    && cargo build --release --locked && rm -rf src
COPY src/ src/
RUN cargo build --release --locked

FROM gcr.io/distroless/cc-debian12
COPY --from=builder /app/target/release/matchbox-server /
ENTRYPOINT ["/matchbox-server"]
```

---

## 5. Tooling Execution Policy (Scripts and Hooks)

- Do not execute third-party CLIs via `npx` in CI scripts, hooks, or repo automation.
- Do not use external Docker images with mutable `:latest` tags in automation.
- Pin tool versions explicitly (for example, via `.markdownlint-version`) and validate versions at runtime.
- Prefer preinstalled/pinned tools over on-demand network downloads.

```bash
# BAD: On-demand execution from registry
npx --yes markdownlint-cli2

# BAD: Mutable Docker tag for third-party image
docker run davidanson/markdownlint-cli2:latest

# GOOD: Pinned local/global install and explicit version check
markdownlint-cli2 --version
```

---

## Agent Checklist

- [ ] `cargo deny check` passes on every PR (advisories, licenses, bans, sources)
- [ ] `cargo audit` runs in CI and on a daily schedule
- [ ] `Cargo.lock` committed; `--locked` used for all CI build/test commands
- [ ] Security-critical deps pinned with exact versions (`=x.y.z`)
- [ ] No git dependencies — `[sources] allow-git = []`
- [ ] License allowlist reviewed — no copyleft in a permissive project
- [ ] Banned crates list includes `openssl`, `atty`, and known-problematic crates
- [ ] `audit.toml` ignores documented with rationale and expiry dates
- [ ] Docker builds use `--locked` and multi-stage pattern
- [ ] Automation scripts avoid `npx` runtime downloads and external `:latest` image tags

---

## Related Skills

- [supply-chain-sbom-updates](./supply-chain-sbom-updates.md) — SBOM generation, update policy,
  CI pipeline, action compatibility
- [dependency-management-cargo](./dependency-management-cargo.md) — Crate evaluation, feature flags,
  workspace dependency patterns
- [web-service-security-hardening](./web-service-security-hardening.md) —
  Application-level security, auth, input validation, TLS
- [Container Docker](./container-docker.md) — Dockerfile hardening, image scanning, CI/CD pipelines
