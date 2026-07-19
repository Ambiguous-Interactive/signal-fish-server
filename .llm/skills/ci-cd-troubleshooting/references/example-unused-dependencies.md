# CI/CD Troubleshooting Example - Unused Dependencies Accumulation

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

## Related Skills

- [CI CD Troubleshooting Categories](diagnostic-workflow.md) — Category map and diagnostic workflow
- [Dependency Management Cargo](../../dependency-management/SKILL.md) — Cargo dependency hygiene
- [Supply Chain Audit Policy](../../supply-chain-security/SKILL.md) — Supply-chain policy and audits
