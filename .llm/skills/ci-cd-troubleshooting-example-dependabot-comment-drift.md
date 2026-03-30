# Skill: CI CD Troubleshooting Example — Dependabot Comment Drift

<!--
  trigger: dependabot comment drift, stale pr limit comment, higher pr limit,
  moderate pr limit, lower pr limit, dependabot config mismatch,
  open-pull-requests-limit comment wrong
  | Dependabot config: relative PR-limit terminology drifts when limits are
  changed without updating surrounding comments
  | Infrastructure
-->

**Trigger**: When a PR review flags that Dependabot config comments say
"Higher PR limit", "Moderate PR limit", or "Lower PR limit" but the configured
`open-pull-requests-limit` values are all the same, or when the number stated in
a comment does not match the configured value.

---

## Incident Summary

**PR**: chore: favor larger area-grouped Dependabot PRs\
**Root cause**: All five `package-ecosystem` entries were migrated to
`open-pull-requests-limit: 2`, but the surrounding section comments kept their
original relative qualifiers ("Higher PR limit", "Moderate PR limit", "Lower PR
limit") from when limits were different per section.

**Reviewer feedback**:

> The root Cargo section header comments still say this entry uses a "Higher PR
> limit"… but the configuration now sets `open-pull-requests-limit: 2`.
> This section's earlier rationale comments describe a "Moderate PR limit", but
> the config now uses `open-pull-requests-limit: 2`.

---

## Root Cause

Relative terminology (Higher / Moderate / Lower) is inherently fragile:

- It depends on _all_ sections having _different_ limits.
- Changing any one limit without updating all related comments breaks the
  relative ordering.
- Reviewers and future maintainers see a contradiction between the comment and
  the actual config, which erodes trust in the documentation.

---

## Fix Applied

Replaced all five relative-terminology comments with neutral, factual language:

```yaml
# BEFORE (misleading — all sections had limit=2):
# Higher PR limit: Cargo updates tend to be well-tested and low-risk
- package-ecosystem: "cargo"
  directory: "/"
  open-pull-requests-limit: 2
# Lower PR limit: Changes here require careful compatibility testing
- package-ecosystem: "cargo"
  directory: "/third_party/rmp"
  open-pull-requests-limit: 2
# Moderate PR limit: Workflow changes need testing but aren't as critical
- package-ecosystem: "github-actions"
  directory: "/"
  open-pull-requests-limit: 2

# AFTER (factual, stable regardless of limit value):
# Consolidated PR limit: Area-grouped updates reduce review overhead
- package-ecosystem: "cargo"
  directory: "/"
  open-pull-requests-limit: 2
# Consolidated PR limit: Keep third_party updates grouped and manageable
- package-ecosystem: "cargo"
  directory: "/third_party/rmp"
  open-pull-requests-limit: 2
# Consolidated PR limit: Consolidate workflow updates into fewer review PRs
- package-ecosystem: "github-actions"
  directory: "/"
  open-pull-requests-limit: 2
```

---

## Prevention Infrastructure

### Validation script

`scripts/check-dependabot-config.sh` now catches three drift patterns:

1. **Relative terminology** — `grep` for `Higher|Moderate|Lower PR limit`.
2. **Missing limit** — Python YAML parsing verifies every entry declares
   `open-pull-requests-limit`.
3. **Numeric mismatch** — if a comment says `PR limit: N`, Python confirms `N`
   equals the configured value.

Run locally at any time:

```bash
./scripts/check-dependabot-config.sh
```

### Pre-commit hook (Check 23)

Triggered automatically when `.github/dependabot.yml` is staged.
Calls `check-dependabot-config.sh` and blocks the commit on failure.

### CI (yaml-lint.yml)

The YAML Lint workflow now runs `check-dependabot-config.sh` as an additional
step, ensuring every PR that touches `dependabot.yml` or the validation script
is checked before merge.

---

## Comment Authoring Rules

| Pattern | Guidance |
|---------|----------|
| Relative terms (Higher / Moderate / Lower) | **Never use** — stable only when all limits differ |
| Exact value | Use `PR limit: 2 — reason` for explicit, auditable documentation |
| Neutral rationale | Use `Consolidated PR limit: reason` when all limits are equal |

---

## Related Skills

- [CI CD Troubleshooting Supply Chain](./ci-cd-troubleshooting-supply-chain.md) —
  Action refs, stale scripts, Dockerfile
- [CI CD Troubleshooting Index](./ci-cd-troubleshooting-index.md) —
  Full map of troubleshooting skills
