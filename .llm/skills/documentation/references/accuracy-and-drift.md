# Documentation Accuracy Guarantees

---

## Policy

- Documentation must describe current behavior, not intended behavior.
- Code samples and protocol examples must match the implementation in `src/`.
- Remove stale claims immediately when implementation changes.
- Avoid absolute wording (`always`, `never`, `guaranteed`) unless every code path is verified.
- Wire strings (`reason`, `error_code`) in examples must be copied verbatim from their
  source of truth, never paraphrased. When the string comes from a single enum `Display`
  (e.g. `ReconnectionError` in `src/reconnection.rs`), a parsed drift guard ties the docs to
  it — see [Repo Source Hygiene Guards](../../repo-source-hygiene-guards/SKILL.md). The reconnection
  guard lives in `tests/docs_site_consistency.rs`.

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

- [Documentation Standards](../SKILL.md)
- [Classify User Visible Changes](../../classify-user-visible-changes/SKILL.md)
- [Update Changelog Keep A Changelog](../../update-changelog-keep-a-changelog/SKILL.md)
