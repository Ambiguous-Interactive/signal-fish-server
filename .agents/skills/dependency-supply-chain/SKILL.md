---
name: dependency-supply-chain
description: Add, upgrade, audit, or remove Rust dependencies while preserving MSRV, feature, license, vulnerability, lockfile, SBOM, and reproducible-build policy. Use for Cargo.toml, Cargo.lock, cargo-deny, cargo-audit, Dependabot, SBOM, or supply-chain security changes.
---

<!-- markdownlint-disable MD013 -->

# Dependency and Supply Chain

Minimize dependency surface and verify policy with repository tools rather than version intuition.

## Workflow

1. Read [dependency-management-cargo.md](references/dependency-management-cargo.md) for Cargo operations and feature hygiene.
2. Read [dependency-management-versioning.md](references/dependency-management-versioning.md) for version and MSRV decisions; also invoke `$toolchain-management` when MSRV can move.
3. Read [supply-chain-audit-policy.md](references/supply-chain-audit-policy.md) for advisory, license, source, and proactive-ban policy.
4. Read [supply-chain-sbom-updates.md](references/supply-chain-sbom-updates.md) for SBOM, update automation, and CI integration.
5. Load [supply-chain-example-rustls-pemfile-to-rustls-pki-types.md](references/supply-chain-example-rustls-pemfile-to-rustls-pki-types.md) only for a comparable replacement migration.
6. Validate the lockfile, feature graph, MSRV, audit policy, and tests affected by the change.
