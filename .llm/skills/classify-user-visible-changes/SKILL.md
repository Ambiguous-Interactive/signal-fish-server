---
name: classify-user-visible-changes
description: >-
  Apply project guidance for classify user-visible changes. Use when deciding whether a change
  requires a `CHANGELOG.md` entry.
---

# Classify User-Visible Changes

---

## When to Use

- Any feature, fix, behavior change, API change, or performance change
- Before marking work complete
- During review when changelog scope is unclear

---

## When NOT to Use

- Writing the changelog entry text itself
- Formatting markdown or links

---

## TL;DR

- If users can notice it, configure it, call it, or depend on it: update `CHANGELOG.md`.
- If only internal tooling changed and user behavior is unchanged: usually no changelog entry.
- Never add `Tests:` or `CI:` bullets. Regression pins, red-proof probes, and
  workflow changes are internal quality work; their record lives in tests,
  PRs, issues, and the audit ledger — never in the changelog.
- When in doubt, treat as user-visible and add an entry.

---

## Decision Matrix

| Change Type | User-Visible? | CHANGELOG Required? | Notes |
| --- | --- | --- | --- |
| New endpoint/feature/flag | Yes | Yes | Add under `Added` |
| Bug fix affecting runtime behavior | Yes | Yes | Add under `Fixed` |
| Backward-compatible behavior adjustment | Yes | Yes | Add under `Changed` |
| Breaking API/behavior | Yes | Yes | Add under `Changed` and label as breaking |
| Security fix with user impact | Yes | Yes | Add under `Security` |
| Pure refactor with no behavior change | No | No | Mention only in PR/commit notes |
| CI workflow/internal script updates only | No | No | Changelog must stay free of CI notes |
| Test-only changes, regression pins, red-proof probes | No | No | Never write a `Tests:` bullet; no exception |
| Docs-only clarifications | Usually No | Optional | Required only if documenting a shipped behavior correction |

---

## Classification Workflow

1. List changed files and the behavior affected.
2. Ask: "Would a user of the server observe any change in behavior, API, performance, security, or configuration?"
3. If yes, mark as changelog-required and open `CHANGELOG.md`.
4. If no, explicitly note "internal-only change" in your task summary and add no entry.
5. If mixed changes exist, log user-visible parts only.
6. If a change is test-only but touches non-internal paths, do not add an
   entry to satisfy the changelog gate: Rust diffs confined to a file's
   trailing test module are classified internal automatically, and a mixed
   diff warrants a real user-visible entry (never a `Tests:` bullet).

---

## Edge Cases

- Dependency upgrades: include only when they change exposed behavior, security posture, compatibility, or documented guarantees.
- Performance work: include if measurable and user-relevant.
- Docs updates: include only if they reflect a real shipped behavior change or migration requirement.
- Unreleased feature edits: update existing unreleased bullet instead of creating duplicate fragmented bullets.
- Test-support seams in production files (`#[cfg(test)]` additions): internal;
  covered by the changelog gate's test-module exemption, not by an entry.

## Section Mechanics (PR #737 failure class)

- `[Unreleased]` accumulates bullets across many sessions. Keep-a-Changelog
  kinds (`Added`, `Changed`, `Fixed`, ...) usually exist already: **append
  bullets to the existing kind heading; do not add a second one.** markdownlint
  MD024 (`siblings_only`) fails hosted Markdown Lint on duplicate sibling
  headings, and the pre-commit hook rejects them at commit time.
- After editing `CHANGELOG.md` (or any markdown), run `bash
  scripts/check-markdown.sh` once before pushing: it is fast, uses the pinned
  markdownlint version, and catches MD013/MD024-class issues the Rust gates
  never see.

---

## Exit Checklist

- [ ] Classification performed for every user request touching code/docs/config
- [ ] A clear yes/no changelog decision is documented
- [ ] If yes, `CHANGELOG.md` was updated under `[Unreleased]`, appending to an existing kind heading when present
- [ ] If yes, `bash scripts/check-markdown.sh` passed
- [ ] If no, internal-only rationale is documented

---

## Related Skills

- [Update Changelog Keep A Changelog](../update-changelog-keep-a-changelog/SKILL.md) — Write compliant entries
- [Review Changelog Entries](../review-changelog-entries/SKILL.md) — Verify quality and consistency
- [Documentation Standards](../documentation/SKILL.md) — Full documentation requirements
