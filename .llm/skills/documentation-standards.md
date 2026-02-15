# Skill: Documentation Standards

<!-- trigger: docs, documentation, changelog, doc-comments, readme, api-docs | Documentation requirements and quality standards for all changes | Core -->

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

- Writing test documentation (see [testing-strategies](./testing-strategies.md))
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
| **XML docs**          | `///` in C#, GDScript comments  | SDK public APIs                     |
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

````rust
/// Creates a new room with the specified configuration.
///
/// # Arguments
/// * `config` - Room configuration including max players and timeout
///
/// # Returns
/// The created room's unique code on success
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
/// *Added in v2.3.0*
pub async fn create_room(&self, config: RoomConfig) -> Result<RoomCode, RoomError>
````

---

## CHANGELOG Format

Use [Keep a Changelog](https://keepachangelog.com/) format:

```markdown
## [Unreleased]

### Added

- Add spectator mode for rooms (#234)
- Add support for custom room metadata

### Changed

- Increase room timeout from 30s to 60s (breaking)

### Fixed

- Fix WebSocket connection leak on abnormal disconnect (#245)

### Deprecated

- Deprecate `join_room_legacy()` — use `join_room()` instead

### Removed

- Remove support for v1 protocol (deprecated since v2.0)

### Security

- Fix authentication bypass in admin API (#250)
```

**Rules:**

- Add entries under `[Unreleased]` during development
- Use imperative mood in entry text ("Add feature X", not "Added feature X") — section headers use past tense per Keep a Changelog convention
- Reference issue/PR numbers
- Mark breaking changes explicitly
- Group by type (Added, Changed, Fixed, etc.)

---

## Documentation Checklist

After every feature/bugfix:

- [ ] Updated relevant `///` doc comments with examples
- [ ] Code samples compile and run correctly
- [ ] CHANGELOG entry added under `[Unreleased]`
- [ ] README updated if user-facing
- [ ] SDK documentation updated if protocol changes
- [ ] New behavior clearly marked as new
