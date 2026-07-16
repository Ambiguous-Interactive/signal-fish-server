# CI/CD Troubleshooting Index

**Applies to**: When you need a compact map of CI/CD troubleshooting skills,
incident examples, and related remediation guides.

---

## Related Reference Map

- [CI CD Troubleshooting Categories](./ci-cd-troubleshooting-categories.md) —
  Diagnostic workflow and quick-reference table
- [CI CD Troubleshooting Ecosystem](./ci-cd-troubleshooting-ecosystem.md) —
  Patterns 1-6: ecosystem, cache, toolchain, Docker
- [CI CD Troubleshooting Linting](./ci-cd-troubleshooting-linting.md) —
  Patterns 7-9: Clippy, typos, markdown
- [CI CD Troubleshooting Scripts](./ci-cd-troubleshooting-scripts.md) —
  Patterns 9-16: locked, Miri, shell scripts, YAML
- [CI CD Troubleshooting Links](./ci-cd-troubleshooting-links.md) —
  Patterns 10-20: lychee, regex, cargo-deny
- [CI CD Troubleshooting Supply Chain](./ci-cd-troubleshooting-supply-chain.md) —
  Patterns 21-25: action refs, stale scripts, Dockerfile

---

## Incident Examples

- [CI CD Troubleshooting Example Python Cache Mismatch](./ci-cd-troubleshooting-example-python-cache-mismatch.md)
- [CI CD Troubleshooting Example Stale Nightly Toolchain](./ci-cd-troubleshooting-example-stale-nightly-toolchain.md)
- [CI CD Troubleshooting Example Unused Dependencies](./ci-cd-troubleshooting-example-unused-dependencies.md)
- [CI CD Troubleshooting Example Changelog Dependabot Bump](./ci-cd-troubleshooting-example-changelog-dependabot-bump.md)
- [CI CD Troubleshooting Example Dependabot Comment Drift](./ci-cd-troubleshooting-example-dependabot-comment-drift.md)

---

## Adjacent Guides

- [GitHub Actions Workflow Config](./github-actions-workflow-config.md) —
  Workflow authoring and maintenance patterns
- [MSRV Management](../../toolchain-management/references/msrv-management.md) — Toolchain and MSRV consistency
- [Supply Chain Audit Policy](../../dependency-supply-chain/references/supply-chain-audit-policy.md) —
  Dependency/security audit policy
- [Agent Self Review Checklist](../../agent-quality/references/agent-self-review-checklist.md) —
  Pre-commit verification workflow
