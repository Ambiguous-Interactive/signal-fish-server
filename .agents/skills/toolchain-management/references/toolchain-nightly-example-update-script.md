# Toolchain Nightly Example - Update Script

**Applies to**: When updating a pinned nightly toolchain version and validating all occurrences in workflow files.

---

## Example Update Script

```bash
# 1. Check current nightly version
grep -n "nightly-" .github/workflows/unused-deps.yml

# 2. Update workflow file (all occurrences)
sed -i 's/nightly-2025-02-21/nightly-2026-01-15/g' .github/workflows/unused-deps.yml

# 3. Update "Last Updated" comment
sed -i 's/Last Updated: .*/Last Updated: 2026-02-16/' .github/workflows/unused-deps.yml

# 4. Verify changes
git diff .github/workflows/unused-deps.yml
```

---

## Related References

- [Toolchain Nightly](./toolchain-nightly.md) — Nightly policy and update workflow
- [MSRV Management](./msrv-management.md) — Stable MSRV policy
- [GitHub Actions Workflow Config](../../ci-troubleshooting/references/github-actions-workflow-config.md) — Workflow editing patterns
