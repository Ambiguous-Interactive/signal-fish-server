# CI/CD Troubleshooting Example - Stale Nightly Toolchain

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

- [CI CD Troubleshooting Categories](diagnostic-workflow.md) — Category map and diagnostic workflow
- [Toolchain Nightly](../../rust-toolchains/references/nightly-toolchains.md) — Nightly pinning strategy
- [MSRV Management](../../msrv-management/SKILL.md) — Toolchain consistency and policy
