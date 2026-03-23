# Skill: CI/CD Troubleshooting Example - Unused Dependencies Accumulation

<!--
  trigger: ci example unused dependencies, cargo machete incident, dependency hygiene example
  | Example incident: unused dependency accumulation reduced CI and supply-chain hygiene
  | Infrastructure
-->

**Trigger**: When dependency sprawl causes avoidable CI risk, maintenance overhead, or security noise.

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

## Related Skills

- [CI CD Troubleshooting Categories](./ci-cd-troubleshooting-categories.md) — Category map and diagnostic workflow
- [Dependency Management Cargo](./dependency-management-cargo.md) — Cargo dependency hygiene
- [Supply Chain Audit Policy](./supply-chain-audit-policy.md) — Supply-chain policy and audits
