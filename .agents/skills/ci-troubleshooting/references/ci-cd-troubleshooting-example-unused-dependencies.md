# CI/CD Troubleshooting Example - Unused Dependencies Accumulation

**Applies to**: When dependency sprawl causes avoidable CI risk, maintenance overhead, or security noise.

---

## Incident

**Problem:** 15+ unused dependencies accumulated in `Cargo.toml` without recurring audits.

---

## Symptoms

- Unnecessary build and audit surface area
- More frequent dependency-related review churn

---

## Root Cause

No scheduled dependency hygiene guard in CI and no periodic cleanup cadence.

---

## Resolution

- Add a weekly CI job running `cargo machete`
- Remove unused dependencies in a dedicated cleanup PR

---

## Related References

- [CI CD Troubleshooting Categories](./ci-cd-troubleshooting-categories.md) — Category map and diagnostic workflow
- [Dependency Management Cargo](../../dependency-supply-chain/references/dependency-management-cargo.md) — Cargo dependency hygiene
- [Supply Chain Audit Policy](../../dependency-supply-chain/references/supply-chain-audit-policy.md) — Supply-chain policy and audits
