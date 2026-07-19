# Nightly Rust Toolchain for CI Tools

See also:

- [MSRV Management](../../msrv-management/SKILL.md) — MSRV definition and consistent config files
- [Toolchain Pinning](../SKILL.md) — Stable toolchain pinning and MSRV testing

---

## TL;DR

- Nightly is **only** for CI-only analysis tools (cargo-udeps, Miri, AddressSanitizer)
- **Never** use nightly for production builds, Docker images, or release artifacts
- Pin to a specific date (`nightly-2026-02-01`) for reproducibility — never use rolling `nightly`
- Update the nightly pin when it is >6 months old or when upgraded dependencies need a newer nightly
- Nightly version is **independent** of stable MSRV — update based on tool needs, not MSRV changes

---

## When Nightly is Acceptable

### Acceptable: CI-only analysis tools

- `cargo-udeps` — unused dependency detection
- `cargo-miri` — undefined behavior detection
- `cargo-fuzz` — fuzzing infrastructure
- `AddressSanitizer` — memory error detection
- Any tool that uses unstable compiler APIs for analysis only

### Not Acceptable: Production builds

- Building the application binary
- Building Docker images for deployment
- Building release artifacts
- Any code that users depend on

---

## Current Nightly Usage

| Tool | Purpose | Workflow File | Nightly Version |
|------|---------|--------------|----------------|
| cargo-udeps | Unused dependency detection | `.github/workflows/unused-deps.yml` | nightly-2026-02-01 |
| Miri | Undefined behavior detection | `.github/workflows/ci-safety.yml` | nightly-2026-02-01 |
| AddressSanitizer | Memory error detection | `.github/workflows/ci-safety.yml` | nightly-2026-02-01 |

---

## Nightly Version Policy

### Pinning Strategy

- Pin to a specific nightly date (e.g., `nightly-2026-02-01`)
- Do NOT use rolling `nightly` (always latest)
- Pinning provides reproducibility and stability

### Update Criteria

Update the nightly version when:

1. **Age**: Nightly version is >6 months old
2. **Security**: Security advisories affect this version
3. **Features**: Tool requires newer nightly features
4. **Compatibility**: Dependency upgrade requires a newer nightly (see below)
5. **Availability**: Nightly version becomes unavailable or broken

### Pinned vs Rolling Nightly

| Aspect | Pinned (`nightly-YYYY-MM-DD`) | Rolling (`nightly`) |
|--------|-------------------------------|---------------------|
| Reproducibility | Same version every CI run | Changes daily |
| Stability | No surprise breakage | May break unexpectedly |
| Freshness | Becomes stale over time | Always latest |
| Maintenance | Requires periodic updates | No updates needed |

**Decision:** Use **pinned nightly** for reproducibility and stability.

---

## Nightly vs Dependency Compatibility

When upgrading dependencies that use unstable Rust features through polyfills (e.g., `rkyv`),
the pinned nightly date may also need updating.

**Symptom:**

```text
error[E0599]: no method named `dangling_ptr` found for reference `&std::alloc::Layout`
  in the current scope
```

**Root Cause:** The dependency's polyfill code expects a nightly API that was added after
the pinned nightly date. The nightly toolchain is too old for the new version of the dependency.

**Fix:** Update the nightly pin date to approximately match when the dependency was released.

```yaml
# ❌ PROBLEM: Nightly from 2025 is incompatible with rkyv v0.8.10 (released Jan 2026)
toolchain: nightly-2025-02-21

# ✅ CORRECT: Nightly date matches the dependency's release timeframe
toolchain: nightly-2026-02-01
```

**Dependency upgrade checklist (nightly-sensitive deps):**

- [ ] Check if the dependency uses nightly/unstable features (polyfills, `#![feature(...)]`)
- [ ] If yes, verify the pinned nightly date is after the dependency's release date
- [ ] Run `cargo +nightly-YYYY-MM-DD check` locally before pushing
- [ ] Update all workflow files that reference the nightly date

The test `test_pinned_nightly_staleness_warning` catches nightlies older than 12 months.
For earlier detection, verify nightly compatibility when upgrading nightly-sensitive dependencies.

---

## Nightly vs MSRV Relationship

**Key Principle:** Nightly for CI tools is **independent** of production MSRV.

```text
Production Code (Stable MSRV)
  → rust-version = "1.88.0" in Cargo.toml
  → Used for: Building binaries, Docker images, production artifacts

CI Analysis Tools (Nightly)
  → nightly-2026-02-01 in workflow files  ← Independent of MSRV
  → Used for: cargo-udeps, cargo-miri (analysis only, no artifacts)
```

**Common Confusion (Avoid):**

- "Nightly must be newer than MSRV" (incorrect)
- "If MSRV is 1.88, nightly must be from after 1.88 release" (incorrect)
- "Nightly is for CI tools only; MSRV is for production code" (correct)
- "Update nightly based on staleness/tool needs, not MSRV changes" (correct)

---

## Updating a Nightly Version

### Update Checklist

- [ ] Identify current nightly version (check workflow file)
- [ ] Choose new nightly version (within last 30 days preferred)
- [ ] Update workflow file (change `toolchain: nightly-YYYY-MM-DD`)
- [ ] Update "Last Updated" documentation comment
- [ ] Update **all occurrences** in the workflow (search for old nightly date)
- [ ] Test in CI (push to branch, verify workflow succeeds)
- [ ] Document in commit (explain reason for nightly update)

See dedicated example:
[Toolchain Nightly Example Update Script](example-nightly-update-script.md).

---

## Required Workflow Documentation

Every nightly usage **must** be documented in the workflow file:

```yaml
# cargo-udeps requires nightly Rust because it uses unstable compiler features
# to analyze dependency usage at a deeper level than stable tools can provide.
#
# Nightly Version: nightly-2026-02-01
# Last Updated: 2026-02-22
#
# Update Criteria (when to update this nightly version):
#   - If the nightly version is >6 months old
#   - If security advisories affect this version
#   - If cargo-udeps requires newer nightly features
#   - If the nightly version becomes unavailable or broken
#
# Policy:
#   - Production code MUST use stable MSRV (see Cargo.toml rust-version)
#   - CI-only analysis tools MAY use nightly if required by the tool
#   - Nightly is NEVER used for building production artifacts
```

---

## Agent Workflow: Nightly Version Updates

1. Verify nightly is still needed: check if the tool still requires nightly
2. Choose a recent nightly: within last 30 days (e.g., `nightly-2026-02-01`)
3. Update **all occurrences** in the workflow file
4. Update documentation (change "Last Updated: YYYY-MM-DD" comment)
5. Ensure workflow file has comprehensive comments explaining why nightly is needed
6. Test in CI: verify workflow passes with new nightly
7. Commit with context: explain age of old nightly, reason for update

---

## Related Skills

- [MSRV Management](../../msrv-management/SKILL.md) — MSRV definition and config file consistency
- [Toolchain Pinning](../SKILL.md) — Stable toolchain pinning and testing
- [GitHub Actions Workflow Config](../../github-actions-workflow-config/SKILL.md) — Workflow patterns
- [Dependency Management Cargo](../../dependency-management/SKILL.md) — Dependency auditing and pinning
- [Toolchain Nightly Example Update Script](example-nightly-update-script.md) — Nightly update script example
