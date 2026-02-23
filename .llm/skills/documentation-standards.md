# Skill: Documentation Standards

<!--
  trigger: docs, documentation, changelog, doc-comments, readme, api-docs
  | Documentation requirements and quality standards for all changes
  | Core
-->

**Trigger**: When adding features, fixing bugs, or making any user-facing change that requires documentation updates.

---

## When to Use

- After implementing any feature or bugfix
- Updating API documentation or doc comments
- Writing or updating CHANGELOG entries
- Updating READMEs or architecture docs
- Reviewing documentation completeness

---

## When NOT to Use

- Writing test documentation (see [testing-strategies](./testing-core-patterns.md))
- Formatting/linting docs (see [mandatory-workflow](./mandatory-workflow.md))

---

## TL;DR

- Every feature/bugfix requires documentation updates across all relevant locations.
- Code samples in docs must compile and run correctly.
- CHANGELOG uses [Keep a Changelog](https://keepachangelog.com/) format under `[Unreleased]`.
- Doc comments explain "why", include examples, and use `@since`/`Added in v2.x` annotations.

---

## What Must Be Updated

| Documentation Type    | Location                        | When to Update                      |
| --------------------- | ------------------------------- | ----------------------------------- |
| **README**            | `README.md`, `sdks/*/README.md` | User-facing features, setup changes |
| **API docs**          | `///` doc comments in Rust      | Public APIs, trait methods          |
| **Code samples**      | Docs, READMEs, examples/        | Any API changes                     |
| **CHANGELOG**         | `CHANGELOG.md`                  | ALL user-facing changes             |
| **Architecture docs** | `docs/`                         | Structural changes                  |

---

## Documentation Quality Standards

- **Clear and succinct** — Get to the point; no filler
- **Correct code samples** — Every sample must compile/run; test them
- **Explain the "why"** — Not just what it does, but why you'd use it
- **Note new behavior** — Clearly indicate when behavior is new or changed
- **Version annotations** — Use `@since`, `Added in v2.x`, etc.

---

## Rust Doc Comment Template

```rust
/// Creates a new room with the specified configuration.
///
/// # Arguments
/// * `config` - Room configuration including max players and timeout
///
/// # Errors
/// Returns `RoomError::InvalidConfig` if max_players is 0 or exceeds 100
///
/// # Example
/// ```
/// let config = RoomConfig::new().max_players(4);
/// let room_code = server.create_room(config).await?;
/// ```
///
/// _Added in v2.3.0_
pub async fn create_room(&self, config: RoomConfig) -> Result<RoomCode, RoomError>
```

---

## CHANGELOG Format

Use [Keep a Changelog](https://keepachangelog.com/) format:

```markdown
## [Unreleased]

### Added
- Add spectator mode for rooms (#234)

### Changed
- Increase room timeout from 30s to 60s (breaking)

### Fixed
- Fix WebSocket connection leak on abnormal disconnect (#245)

### Security
- Fix authentication bypass in admin API (#250)
```

**Rules:**

- Add entries under `[Unreleased]` during development
- Use imperative mood ("Add feature X", not "Added feature X") — section headers use past tense per Keep a Changelog
- Reference issue/PR numbers; mark breaking changes explicitly

---

## Markdown Quality Standards

All fenced code blocks MUST have a language identifier (`bash`, `rust`, `json`, `yaml`, `toml`, `text`).

Tables must have consistent column alignment:

```markdown
| Column | Description | Example |
|--------|-------------|---------|
| Foo    | First item  | abc     |
```

### Local Validation

```bash
# Check all markdown files
./scripts/check-markdown.sh

# Auto-fix issues where possible
./scripts/check-markdown.sh fix
```

Install the pinned markdownlint version for local checks:
`npm install -g markdownlint-cli2@$(cat .markdownlint-version)`

The pre-commit hook automatically checks markdown files (if the pinned markdownlint version is installed).
Enable with `./scripts/enable-hooks.sh`.

### README Badge Consistency

For README Shields badges (`https://img.shields.io/...`), enforce a consistent
visual style by including `style=for-the-badge` on every badge URL.
This rule is conditional: if no Shields badges are present, the check still passes.
Use strict mode only when repository policy explicitly requires at least one badge:
`./scripts/check-readme-badges.sh --require-at-least-one README.md`.

Validate locally with:

```bash
./scripts/check-readme-badges.sh README.md
```

### Common Markdown Linting Issues

| Rule  | Issue                                    | Fix                                              |
|-------|------------------------------------------|--------------------------------------------------|
| MD040 | Code block missing language identifier   | Add language after opening backticks             |
| MD013 | Line too long                            | Break long lines (or disable for technical docs) |
| MD031 | Missing blank lines around code blocks   | Add blank line before and after code blocks      |

---

## Spelling Consistency: American English

Use **American English** consistently across all source files, CI workflows, and documentation.

| British (do NOT use) | American (use this) |
|----------------------|---------------------|
| behaviour            | behavior            |
| colour               | color               |
| initialise           | initialize          |
| organise             | organize            |
| serialise            | serialize           |
| uninitialised        | uninitialized       |

### Enforcement

1. **`.typos.toml`** — Automatically catches British spellings and suggests American equivalents.
2. **`test_no_british_english_spellings`** in `tests/ci_config_tests.rs` — Scans all `.rs`, `.yml`,
   and `.md` files for common British spellings with file path, line number, and replacement.

Lines containing URLs (`http://`, `https://`) are excluded from the spelling scan.
The test file itself (`ci_config_tests.rs`) is excluded as it contains British spellings as test data.

---

## Documentation Checklist

After every feature/bugfix:

- [ ] Updated relevant `///` doc comments with examples
- [ ] Code samples compile and run correctly
- [ ] CHANGELOG entry added under `[Unreleased]`
- [ ] README updated if user-facing
- [ ] New behavior clearly marked as new
- [ ] Markdown files pass linting (`./scripts/check-markdown.sh`)
- [ ] README Shields badges include `style=for-the-badge` (`./scripts/check-readme-badges.sh README.md`)
- [ ] If enforcing a minimum badge count, run strict mode (`./scripts/check-readme-badges.sh --require-at-least-one README.md`)
- [ ] All code blocks have language identifiers
- [ ] Technical terms added to `.typos.toml` if needed
- [ ] All text uses American English spellings (not British)
