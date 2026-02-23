# Skill: Update Changelog in Keep a Changelog Format

<!--
  trigger: update changelog, keep a changelog, changelog format, unreleased, release notes
  | Write CHANGELOG.md entries in strict Keep a Changelog format
  | Core
-->

**Trigger**: After any user-visible change classified as changelog-required.

---

## When to Use

- Any user-visible feature, fix, change, deprecation, removal, or security update
- Any breaking behavior/API change
- Before handing work back to user

---

## When NOT to Use

- Internal-only CI/tooling/refactor/test updates with zero user impact
- Rewriting historical released notes

---

## Required Rules

1. Add entries under `## [Unreleased]` only.
2. Follow Keep a Changelog section names:
   - `Added`
   - `Changed`
   - `Deprecated`
   - `Removed`
   - `Fixed`
   - `Security`
3. Do not edit prior released version sections unless explicitly asked.
4. Use concise, user-centered language that states externally observable impact.
5. Mark breaking changes explicitly with `**Breaking:**`.
6. Do not add custom change categories outside Keep a Changelog.

---

## Entry Template

```markdown
## [Unreleased]

### Added
- Add ...

### Changed
- **Breaking:** Change ...

### Fixed
- Fix ...

### Security
- Harden ...
```

Only include sections that have entries.

---

## Writing Guidance

- Start with an imperative verb: `Add`, `Change`, `Fix`, `Deprecate`, `Remove`.
- Keep bullets scannable; avoid internal implementation details.
- Prefer one primary bullet per user-facing change area.
- Include compatibility/migration notes for breaking changes.
- If adjusting an unreleased feature, update its existing unreleased bullet instead of creating noisy duplicates.

---

## Process

1. Open `CHANGELOG.md`.
2. Locate `## [Unreleased]`.
3. Add or update entries in correct section(s).
4. Remove empty section headers if none are used.
5. Re-read for user language and compatibility clarity.

---

## Good vs Bad

| Good | Bad |
| --- | --- |
| `- Fix WebSocket reconnect cleanup on abrupt disconnect.` | `- Refactor connection lifecycle code.` |
| `- **Breaking:** Change room code validation to reject lowercase.` | `- Update validation logic.` |
| `- Add server metric for active lobby count.` | `- Misc metrics updates.` |

---

## Exit Checklist

- [ ] Change is user-visible and classified as changelog-required
- [ ] Entry added or updated under `[Unreleased]`
- [ ] Keep a Changelog section names are used
- [ ] No custom non-standard section categories were introduced
- [ ] Breaking changes are clearly labeled
- [ ] No released historical section was edited unintentionally

---

## Related Skills

- [classify-user-visible-changes](./classify-user-visible-changes.md) — Determine whether entry is required
- [review-changelog-entries](./review-changelog-entries.md) — Final quality gate
- [documentation-standards](./documentation-standards.md) — Documentation completeness policy
