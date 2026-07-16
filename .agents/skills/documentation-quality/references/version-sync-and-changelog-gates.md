# Version Sync and Changelog Gates

**Applies to**: When changing public behavior, docs/examples, or release-related files.

---

## Rules

1. `Cargo.toml` `[package].version` is canonical for project version references.
2. Selected version references in docs/agent context must match that version.
3. `CHANGELOG.md` must remain Keep a Changelog compliant.
4. If non-internal files change, `CHANGELOG.md` must be updated in the same change.

## Version Scan Scope

`scripts/check-doc-consistency.sh` scans first-party Markdown for
`signal-fish-server = "<version>"` dependency examples, including `.agents/skills` guidance.
It intentionally prunes generated, vendored, and fixture directories such as
`target/`, `third_party/`, `node_modules/`, `site/`, `.git/`, `test-fixtures/`,
and `mutants.out/` at any depth so copied upstream docs or generated output do
not false-fail against this crate's `Cargo.toml`.

Versionless dependency examples using `path`, `git`, `workspace`, or `registry`
without a `version = "..."` key are rejected. They cannot be auto-synced on a
version bump and would make the hook and CI disagree. In prose, describe local
path dependencies without writing a TOML assignment; in templates, use the
`<version>` placeholder.

---

## Commands

```bash
# Full policy validation
./scripts/check-doc-consistency.sh

# Pre-commit scope validation (staged files)
./scripts/check-doc-consistency.sh --staged
```

---

## Non-Internal Change Gate

The following paths are classified as internal (no CHANGELOG entry required):

- **Dot-directories:** `.github/`, `.githooks/`, `.devcontainer/`, `.config/`, `.vscode/`, `.claude/`, `.agents/skills/`
- **Dev/build directories:** `scripts/`, `tests/`, `test-fixtures/`, `target/`, `progress/`
- **Standalone files:** `Cargo.lock`, `PLAN.md`, `AGENTS.md`, `pre-push.txt`, `logs_*.zip`
- **Lint/tool configs:** `.markdownlint*`, `.lychee.toml`, `.lycheecache`, `.typos.toml`, `.yamllint.yml`
- **VCS/Docker ignores:** `.gitignore`, `.dockerignore`
- **Build tool configs:** `clippy.toml`, `deny.toml`, `tarpaulin.toml`, `rust-toolchain.toml`, `mkdocs.yml`, `requirements-docs.txt`

Any changed file outside internal-only scope requires a `CHANGELOG.md` update.

When adding a new internal path category, update both `is_internal_path()` in
`scripts/check-doc-consistency.sh` and the test fixture at
`.github/test-fixtures/test-doc-consistency.sh`.

---

## Related References

- [Classify User Visible Changes](./classify-user-visible-changes.md)
- [Update Changelog Keep A Changelog](./update-changelog-keep-a-changelog.md)
- [Review Changelog Entries](./review-changelog-entries.md)
- [Doc Accuracy Guarantees](./doc-accuracy-guarantees.md)
