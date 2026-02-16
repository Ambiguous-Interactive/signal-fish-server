# Skill: Supply Chain Security

<!-- trigger: supply-chain, cargo-audit, cargo-deny, sbom, dependency, cve, vulnerability, advisory, license, reproducible-build | Securing the dependency supply chain for Rust projects | Security -->

**Trigger**: When auditing dependencies for vulnerabilities, configuring cargo-deny policies, generating SBOMs, enforcing reproducible builds, or managing dependency pinning and update workflows.

---

## When to Use

- Running or configuring `cargo audit` or `cargo deny`
- Adding, updating, or reviewing third-party dependencies for security
- Generating or consuming Software Bills of Materials (SBOMs)
- Configuring CI pipelines for supply chain gates
- Investigating CVEs, RustSec advisories, or license compliance
- Setting up reproducible or hermetic builds

## When NOT to Use

- Application-level security (see [web-service-security](./web-service-security.md))
- Container image hardening (see [container-and-deployment](./container-and-deployment.md))
- Choosing between crates for functionality (see [dependency-management](./dependency-management.md))

## Rationalizations to Reject

| Excuse | Why It's Wrong | Required Action |
|--------|---------------|-----------------|
| "We only use well-known crates" | Popular crates get compromised too. Transitive deps hide risk. | Audit the full tree. Run `cargo deny check` on every PR. |
| "We'll audit before release" | Vulnerabilities accumulate silently between audits. | Run `cargo audit` in CI on every push and on a daily schedule. |
| "Pinning versions slows us down" | Unpinned deps can silently pull in breaking or malicious updates. | Pin security-critical deps exactly. Always commit `Cargo.lock`. Build with `--locked`. |
| "SBOMs are just compliance theater" | SBOMs enable automated vulnerability correlation when a new CVE drops. | Generate SBOMs in CI and store as build artifacts. |

---

## TL;DR

- Run `cargo audit` and `cargo deny check` in CI on every PR — block merges on failure.
- Pin security-critical dependencies with exact versions (`=1.2.3`) and always commit `Cargo.lock`.
- Build with `cargo build --locked` in CI to guarantee reproducibility.
- Generate CycloneDX/SPDX SBOMs as build artifacts for vulnerability correlation.
- Automate dependency updates with Dependabot/Renovate; review each update against a checklist.

---

## 1. Cargo Audit and Advisory Database

```bash
cargo audit              # Check against RustSec Advisory Database
cargo audit --json       # JSON output for CI parsing

```

Every advisory must result in: **Fix** (update the crate), **Ignore with justification** (document in `audit.toml`), or **Deny** (replace the crate).

```rust

// ❌ Bad — silently ignoring an advisory with no justification
// Just add RUSTSEC-2024-0001 to the ignore list and move on
// ✅ Good — documented ignore with expiry and rationale in audit.toml
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

```rust

// ❌ Bad — adding a GPL-licensed crate to a permissive project
// Cargo.toml: my-gpl-dep = "1.0"  # License: GPL-3.0
// ✅ Good — verify license before adding: cargo deny check licenses

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

```rust

// ❌ Bad — pulling in openssl via native-tls feature
// reqwest = { version = "0.12", features = ["native-tls"] }
// ✅ Good — using rustls backend to stay off the banned list
// reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }

```

### `[sources]` — no git dependencies in production

```toml

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
allow-git = []

```rust

// ❌ Bad — git dep bypasses crates.io auditing
// my-crate = { git = "https://github.com/user/my-crate" }
// ✅ Good — use crates.io or vendor locally
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

Applications and servers **must** commit `Cargo.lock`. This project is a server — `Cargo.lock` is committed. Workspace crates share a single `Cargo.lock` at the root.

### Lockfile Verification in CI

```bash
# ❌ Bad — CI resolves fresh deps, may differ from lockfile
cargo build
# ✅ Good — fails if lockfile is stale or missing
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

## 5. SBOM Generation

```bash

cargo install cargo-sbom
cargo sbom --output-format cyclonedx-json > sbom.cdx.json   # CycloneDX
cargo sbom --output-format spdx-json > sbom.spdx.json       # SPDX

```

### Integration with Vulnerability Scanners

```bash

grype sbom:sbom.cdx.json --output table              # Grype
trivy sbom sbom.cdx.json --severity HIGH,CRITICAL     # Trivy

```

### CI Artifact Upload

```yaml


- run: cargo sbom --output-format cyclonedx-json > sbom.cdx.json
- uses: actions/upload-artifact@v4

  with:
    name: sbom-${{ github.sha }}
    path: sbom.cdx.json
    retention-days: 90

```

---

## 6. Dependency Update Policy

### Dependabot Configuration

```yaml
# .github/dependabot.yml
version: 2
updates:

  - package-ecosystem: "cargo"

    directory: "/"
    schedule:
      interval: "weekly"
    open-pull-requests-limit: 10
    groups:
      minor-and-patch:
        update-types: ["minor", "patch"]

```

### Update Urgency Guide

| Type | Urgency | Auto-Merge? |
|------|---------|-------------|
| Security patch (CVE) | Immediate | Yes, if tests pass |
| Patch (bug fix) | Days | Yes, if tests pass |
| Minor (features) | Weekly | After manual review |
| Major (breaking) | Sprint planning | Never auto-merge |

### Review Checklist for Dependency PRs

- [ ] Changelog reviewed — no unexpected changes
- [ ] `cargo deny check` and `cargo audit` pass
- [ ] `cargo test --locked` passes
- [ ] No new transitive deps added (`cargo tree -d`)
- [ ] No license changes in updated crate
- [ ] Binary size and build time delta acceptable

---

## 7. CI Pipeline Integration

### Complete Supply Chain Job

```yaml
name: Supply Chain Audit
on:
  push:
    branches: [main]
  pull_request:
  schedule:

    - cron: "0 8 * * *"  # Daily scan

jobs:
  audit:
    runs-on: ubuntu-latest
    steps:

      - uses: actions/checkout@v4
      - run: cargo install cargo-audit cargo-deny cargo-sbom
      - run: cargo deny check
      - run: cargo audit
      - run: cargo build --release --locked
      - run: cargo sbom --output-format cyclonedx-json > sbom.cdx.json
      - uses: actions/upload-artifact@v4

        with:
          name: sbom-${{ github.sha }}
          path: sbom.cdx.json

```

### Local Pre-Push Hook

```bash
#!/bin/bash
# .git/hooks/pre-push
set -e
cargo deny check && cargo audit
echo "Supply chain checks passed."

```

### Alerting on New Advisories

```yaml
# In scheduled workflow — notify on failure
- name: Notify on vulnerability

  if: failure()
  uses: slackapi/slack-github-action@v2
  with:
    webhook: ${{ secrets.SLACK_SECURITY_WEBHOOK }}
    payload: |
      {"text": "🚨 cargo audit found new advisories in matchbox-signaling-server"}

```

---

## 8. CI Action Version Compatibility

GitHub Actions that parse `Cargo.lock` or invoke Cargo internally may break when the lockfile format changes. Always verify that CI action versions are compatible with the project's `Cargo.lock` version after upgrading the Rust toolchain.

### `Cargo.lock` Version History

| Lockfile Version | Minimum Rust | Notes |
|-----------------|--------------|-------|
| v3 | 1.38+ | Widely supported by older CI actions |
| v4 | 1.78+ | Requires `cargo-deny-action@v2` or later |

### Rules

- **`Cargo.lock` v4 requires `EmbarkStudios/cargo-deny-action@v2` or later** — `@v1` ships an older Cargo that cannot parse v4 lockfiles and will fail silently or with cryptic errors.
- **When upgrading the Rust toolchain**, check whether the new version bumps the `Cargo.lock` format. If it does, audit every CI action that touches `Cargo.lock` for compatibility.
- **When adding or updating CI actions** that invoke Cargo or parse `Cargo.lock`, verify they support the lockfile version used by the project.
- **Run `scripts/check-ci-config.sh`** before pushing — it detects outdated action versions and lockfile incompatibilities automatically.

```bash
# ❌ Bad — using v1 with Cargo.lock v4 (will fail in CI)
- uses: EmbarkStudios/cargo-deny-action@v1

# ✅ Good — v2 supports Cargo.lock v4
- uses: EmbarkStudios/cargo-deny-action@v2


```

### Pre-Push Validation

```bash
# Run the CI config validator to catch version mismatches before pushing
bash scripts/check-ci-config.sh

```

This script checks:

- `Cargo.lock` version and warns if actions need upgrading
- Presence of `deny.toml`
- CI workflow files for outdated `cargo-deny-action` references

---

## Agent Checklist

- [ ] `cargo deny check` passes on every PR (advisories, licenses, bans, sources)
- [ ] `cargo audit` runs in CI and on a daily schedule
- [ ] `Cargo.lock` committed; `--locked` used for all CI build/test commands
- [ ] Security-critical deps pinned with exact versions (`=x.y.z`)
- [ ] No git dependencies — `[sources] allow-git = []`
- [ ] License allowlist reviewed — no copyleft in a permissive project
- [ ] Banned crates list includes `openssl`, `atty`, and known-problematic crates
- [ ] SBOM generated as a build artifact (CycloneDX or SPDX)
- [ ] Dependabot or Renovate configured for automated updates
- [ ] Dependency update PRs reviewed against checklist
- [ ] Docker builds use `--locked` and multi-stage pattern
- [ ] `audit.toml` ignores documented with rationale and expiry dates
- [ ] CI action versions compatible with `Cargo.lock` version (run `scripts/check-ci-config.sh`)
- [ ] `cargo-deny-action@v2` or later used when `Cargo.lock` is v4+

## Related Skills

- [dependency-management](./dependency-management.md) — Crate evaluation, feature flags, workspace dependency patterns
- [web-service-security](./web-service-security.md) — Application-level security, auth, input validation, TLS
- [container-and-deployment](./container-and-deployment.md) — Dockerfile hardening, image scanning, CI/CD pipelines
- [clippy-and-linting](./clippy-and-linting.md) — CI integration for static analysis gates
