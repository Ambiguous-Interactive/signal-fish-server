# Skill: CI/CD Troubleshooting Example - Stale Nightly Toolchain

<!--
  trigger: ci example stale nightly, nightly too old ci, toolchain staleness incident
  | Example incident: stale nightly toolchain breaks CI reliability
  | Infrastructure
-->

**Trigger**: When CI failures correlate with an outdated pinned nightly toolchain.

---

## Incident

**Problem:** `toolchain: nightly-2025-02-21` was roughly 360 days old.

---

## Symptoms

- Dependencies failed to compile
- Security posture drifted due to stale toolchain baseline

---

## Root Cause

Pinned nightly was not refreshed as dependency and compiler expectations evolved.

---

## Resolution

Update pinned nightly to a current validated baseline, for example:
`nightly-2026-02-01`.

---

## Related Skills

- [CI CD Troubleshooting Categories](./ci-cd-troubleshooting-categories.md) — Category map and diagnostic workflow
- [Toolchain Nightly](./toolchain-nightly.md) — Nightly pinning strategy
- [MSRV Management](./msrv-management.md) — Toolchain consistency and policy
