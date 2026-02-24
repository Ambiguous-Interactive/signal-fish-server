# Skill: Classify User-Visible Changes

<!--
  trigger: user-visible, changelog-required, changelog-scope, internal-change, customer-impact
  | Decide whether a change must be added to CHANGELOG.md
  | Core
-->

**Trigger**: When deciding whether a change requires a `CHANGELOG.md` entry.

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
| CI workflow/internal script updates only | No | No | Unless they change user-facing release behavior |
| Test-only changes | No | No | Unless they document a shipped behavior change |
| Docs-only clarifications | Usually No | Optional | Required only if documenting a shipped behavior correction |

---

## Classification Workflow

1. List changed files and the behavior affected.
2. Ask: "Would a user of the server observe any change in behavior, API, performance, security, or configuration?"
3. If yes, mark as changelog-required and open `CHANGELOG.md`.
4. If no, explicitly note "internal-only change" in your task summary.
5. If mixed changes exist, log user-visible parts only.

---

## Edge Cases

- Dependency upgrades: include only when they change exposed behavior, security posture, compatibility, or documented guarantees.
- Performance work: include if measurable and user-relevant.
- Docs updates: include only if they reflect a real shipped behavior change or migration requirement.
- Unreleased feature edits: update existing unreleased bullet instead of creating duplicate fragmented bullets.

---

## Exit Checklist

- [ ] Classification performed for every user request touching code/docs/config
- [ ] A clear yes/no changelog decision is documented
- [ ] If yes, `CHANGELOG.md` was updated under `[Unreleased]`
- [ ] If no, internal-only rationale is documented

---

## Related Skills

- [update-changelog-keep-a-changelog](./update-changelog-keep-a-changelog.md) — Write compliant entries
- [review-changelog-entries](./review-changelog-entries.md) — Verify quality and consistency
- [documentation-standards](./documentation-standards.md) — Full documentation requirements
