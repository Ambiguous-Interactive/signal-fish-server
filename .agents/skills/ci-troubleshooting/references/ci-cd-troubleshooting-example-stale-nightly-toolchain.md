# CI/CD Troubleshooting Example - Stale Nightly Toolchain

**Applies to**: When CI failures correlate with an outdated pinned nightly toolchain.

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

## Related References

- [CI CD Troubleshooting Categories](./ci-cd-troubleshooting-categories.md) — Category map and diagnostic workflow
- [Toolchain Nightly](../../toolchain-management/references/toolchain-nightly.md) — Nightly pinning strategy
- [MSRV Management](../../toolchain-management/references/msrv-management.md) — Toolchain consistency and policy
