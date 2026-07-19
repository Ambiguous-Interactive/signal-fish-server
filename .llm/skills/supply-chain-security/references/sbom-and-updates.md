# Supply Chain Security — SBOMs, Updates, and CI Integration

---

## When to Use

- Generating or consuming Software Bills of Materials (SBOMs)
- Configuring Dependabot/Renovate for automated updates
- Setting up the complete supply chain CI job
- Verifying CI action versions are compatible with the project's `Cargo.lock` version

## When NOT to Use

- Audit tools and cargo-deny policy (see [Supply Chain Audit Policy](../SKILL.md))
- Choosing between crates for functionality (see [Dependency Management Cargo](../../dependency-management/SKILL.md))

## Rationalizations to Reject

| Excuse | Why It's Wrong | Required Action |
|--------|---------------|-----------------|
| "SBOMs are just compliance theater" | SBOMs enable automated vulnerability correlation when a new CVE drops. | Generate SBOMs in CI and store as build artifacts. |

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
      {"text": "cargo audit found new advisories in matchbox-signaling-server"}
```

---

## 8. CI Action Version Compatibility

GitHub Actions that parse `Cargo.lock` or invoke Cargo internally may break when the lockfile format changes.
Always verify that CI action versions are compatible with the project's `Cargo.lock` version after upgrading
the Rust toolchain.

### `Cargo.lock` Version History

| Lockfile Version | Minimum Rust | Notes |
|-----------------|--------------|-------|
| v3 | 1.38+ | Widely supported by older CI actions |
| v4 | 1.78+ | Requires `cargo-deny-action@v2` or later |

### Rules

- **`Cargo.lock` v4 requires `EmbarkStudios/cargo-deny-action@v2` or later** — `@v1` ships an older Cargo
  that cannot parse v4 lockfiles and will fail silently or with cryptic errors.
- **When upgrading the Rust toolchain**, check whether the new version bumps the `Cargo.lock` format.
  If it does, audit every CI action that touches `Cargo.lock` for compatibility.
- **When adding or updating CI actions** that invoke Cargo or parse `Cargo.lock`,
  verify they support the lockfile version used by the project.
- **Run `scripts/check-ci-config.sh`** before pushing — it detects outdated action versions
  and lockfile incompatibilities automatically.

```bash
# BAD: Using v1 with Cargo.lock v4 (will fail in CI)
- uses: EmbarkStudios/cargo-deny-action@v1

# GOOD: v2 supports Cargo.lock v4
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

- [ ] SBOM generated as a build artifact (CycloneDX or SPDX)
- [ ] Dependabot or Renovate configured for automated updates
- [ ] Dependency update PRs reviewed against checklist
- [ ] Supply chain CI job runs on every PR and daily schedule
- [ ] CI action versions compatible with `Cargo.lock` version (run `scripts/check-ci-config.sh`)
- [ ] `cargo-deny-action@v2` or later used when `Cargo.lock` is v4+

---

## Related Skills

- [Supply Chain Audit Policy](../SKILL.md) — cargo audit, cargo-deny config,
  pinning, reproducible builds
- [Dependency Management Cargo](../../dependency-management/SKILL.md) — Crate evaluation, feature flags,
  workspace dependency patterns
- [Container Docker](../../containers/SKILL.md) — Dockerfile hardening, image scanning, CI/CD pipelines
- [Clippy And Linting](../../clippy-and-linting/SKILL.md) — CI integration for static analysis gates
