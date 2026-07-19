---
name: ci-cd-troubleshooting
description: >-
  Diagnose and prevent CI/CD failures across workflows, caches, toolchains, linting, scripts,
  link checking, and supply-chain checks. Use when a CI job fails, drifts from local behavior,
  or needs an evidence-driven incident analysis.
---

# CI/CD Troubleshooting Index

## Related Skills Map

- [CI CD Troubleshooting Categories](references/diagnostic-workflow.md) —
  Diagnostic workflow and quick-reference table
- [CI CD Troubleshooting Ecosystem](references/ecosystem-and-toolchains.md) —
  Patterns 1-6: ecosystem, cache, toolchain, Docker
- [CI CD Troubleshooting Linting](references/linting-and-documentation.md) —
  Patterns 7-9: Clippy, typos, markdown
- [CI CD Troubleshooting Scripts](references/scripts-and-tests.md) —
  Patterns 9-16: locked, Miri, shell scripts, YAML
- [CI CD Troubleshooting Links](references/link-checking.md) —
  Patterns 10-20: lychee, regex, cargo-deny
- [CI CD Troubleshooting Supply Chain](references/supply-chain.md) —
  Patterns 21-25: action refs, stale scripts, Dockerfile

---

## Incident Examples

- [CI CD Troubleshooting Example Python Cache Mismatch](references/example-python-cache-mismatch.md)
- [CI CD Troubleshooting Example Stale Nightly Toolchain](references/example-stale-nightly-toolchain.md)
- [CI CD Troubleshooting Example Unused Dependencies](references/example-unused-dependencies.md)
- [CI CD Troubleshooting Example Changelog Dependabot Bump](references/example-changelog-dependabot-bump.md)
- [CI CD Troubleshooting Example Dependabot Comment Drift](references/example-dependabot-comment-drift.md)

---

## Adjacent Guides

- [GitHub Actions Workflow Config](../github-actions-workflow-config/SKILL.md) —
  Workflow authoring and maintenance patterns
- [MSRV Management](../msrv-management/SKILL.md) — Toolchain and MSRV consistency
- [Supply Chain Audit Policy](../supply-chain-security/SKILL.md) —
  Dependency/security audit policy
- [Agent Self Review Checklist](../agent-self-review-checklist/SKILL.md) —
  Pre-commit verification workflow
