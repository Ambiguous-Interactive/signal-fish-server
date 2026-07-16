# CI/CD Troubleshooting Example - Python Cache on Rust Project

**Applies to**: When CI on a Rust project fails with pip/cache-related errors due to ecosystem cache mismatch.

---

## Incident

**Problem:** CI had `actions/cache@v4` with `~/.cache/pip` on a Rust project.

---

## Symptoms

- Cache deserialization failures
- `pip` executable not found
- Slower CI due to ineffective caching

---

## Root Cause

Workflow cache configuration targeted the Python ecosystem instead of Rust.

---

## Resolution

Replace generic Python cache configuration with Rust-aware cache action:
`Swatinem/rust-cache@v2.7.5`.

---

## Related References

- [CI CD Troubleshooting Categories](./ci-cd-troubleshooting-categories.md) — Category map and diagnostic workflow
- [CI CD Troubleshooting Ecosystem](./ci-cd-troubleshooting-ecosystem.md) — Ecosystem mismatch patterns
- [GitHub Actions Workflow Config](./github-actions-workflow-config.md) — Workflow authoring best practices
