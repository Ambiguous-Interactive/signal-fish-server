# Skill: Documentation Accuracy Guarantees

<!--
  trigger: documentation accuracy, docs drift, stale docs, protocol docs, examples accuracy
  | Prevent documentation, examples, and claims from drifting away from actual implementation
  | Core
-->

**Trigger**: When editing public docs/examples, protocol docs, or behavioral guarantees.

---

## Policy

- Documentation must describe current behavior, not intended behavior.
- Code samples and protocol examples must match the implementation in `src/`.
- Remove stale claims immediately when implementation changes.
- Avoid absolute wording (`always`, `never`, `guaranteed`) unless every code path is verified.

---

## Required Validation

Run:

```bash
./scripts/check-doc-consistency.sh
```

This enforces:

- Cargo package-version sync in selected docs/LLM references.
- Keep a Changelog structure/link consistency.
- Anti-drift checks for README/context protocol quick references.

---

## Accuracy Checklist

- [ ] README protocol examples match `src/protocol/messages.rs`
- [ ] `.llm/context.md` protocol quick reference matches implementation
- [ ] Dependency snippets using `signal-fish-server` match `Cargo.toml` package version
- [ ] CHANGELOG entries stay under `## [Unreleased]` and use Keep a Changelog sections
- [ ] No stale message names or removed fields remain in docs/examples

---

## Related Skills

- [documentation-standards](./documentation-standards.md)
- [classify-user-visible-changes](./classify-user-visible-changes.md)
- [update-changelog-keep-a-changelog](./update-changelog-keep-a-changelog.md)
