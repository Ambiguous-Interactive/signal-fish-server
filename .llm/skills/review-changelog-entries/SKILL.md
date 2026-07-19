---
name: review-changelog-entries
description: >-
  Review changelog entries for user impact, clarity, format, and release-note quality. Use after
  editing `CHANGELOG.md` and before finalizing the work.
---

# Review Changelog Entries

---

## When to Use

- Any time changelog entries were added/edited
- During self-review or reviewer pass
- Before release note extraction workflows rely on changelog text

---

## When NOT to Use

- When no changelog edits were made
- As a substitute for deciding scope (use classification skill first)

---

## Severity Rubric

- `CRITICAL`: Wrong section, missing required entry, non-user-facing noise
  for internal-only changes, or broken structure under `[Unreleased]`.
- `WARNING`: Vague wording, missing breaking label, duplicate or conflicting bullets.
- `SUGGESTION`: Clarity or concision improvements.

---

## Review Checklist

- [ ] Entry is under `## [Unreleased]`
- [ ] Keep a Changelog section names are used
- [ ] Section headers follow canonical order: `Added`, `Changed`, `Deprecated`, `Removed`, `Fixed`, `Security`
- [ ] No custom non-standard categories are used
- [ ] Entry describes user-visible impact, not internal refactor details
- [ ] Section choice is correct (`Added/Changed/Deprecated/Removed/Fixed/Security`)
- [ ] Breaking changes use `**Breaking:**`
- [ ] No accidental edits to previously released sections
- [ ] Wording is concise, specific, and externally meaningful
- [ ] Duplicate bullets for same unreleased feature were consolidated

---

## Review Output Format

```text
[SEVERITY] CHANGELOG.md
Issue: ...
Fix: ...
```

If no issues:

```text
PASS: CHANGELOG.md entries are compliant and high quality.
```

---

## Common Corrections

- Move runtime bug-fix entries from `Changed` to `Fixed`.
- Replace implementation-heavy text with user outcomes.
- Merge multiple bullets that describe the same unreleased feature adjustment.
- Add migration note and `**Breaking:**` marker where compatibility changed.

---

## Exit Criteria

- Zero `CRITICAL` and zero `WARNING` findings
- Changelog entries are release-note ready
- Reviewer can clearly map each entry to user-visible change behavior

---

## Related Skills

- [Classify User Visible Changes](../classify-user-visible-changes/SKILL.md) — Scope decision
- [Update Changelog Keep A Changelog](../update-changelog-keep-a-changelog/SKILL.md) — Authoring workflow
- [Agent Self Review Checklist](../agent-self-review-checklist/SKILL.md) — Full task quality checks
