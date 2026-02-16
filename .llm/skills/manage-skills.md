# Skill: Manage Skills

<!-- trigger: skill, skills, manage, create-skill, edit-skill, split-skill | Creating, editing, and maintaining skill files | Core -->

**Trigger**: When creating, editing, splitting, or organizing skill files in `.llm/skills/`.

---

## When to Use

- Creating a new skill file
- Editing an existing skill to add or update content
- Splitting a skill that exceeds the size limit
- Reviewing skill compliance with formatting rules
- Regenerating the skills index in context.md

---

## When NOT to Use

- Editing context.md rules or architecture sections
- Writing production code

---

## Skill File Template

Every skill file MUST follow this structure:

```markdown
# Skill: [Title Case Name]

<!-- trigger: keyword1, keyword2 | Short description | Category -->

**Trigger**: When to invoke this skill (one sentence).

---

## When to Use
- Bullet list of situations

---

## When NOT to Use
- Situations where this skill is NOT appropriate

---

## [Main Content Sections]

---

## Related Skills
- [related-skill](./related-skill.md) — Brief description


```

---

## Trigger Comment Format

```text

<!-- trigger: keywords | description | category -->

```

| Field | Purpose | Example |
|-------|---------|---------|
| keywords | Comma-separated search terms | `test, testing, nunit` |
| description | Brief description for index | `Writing or modifying tests` |
| category | `Core`, `Performance`, or `Feature` | `Core` |

**Categories:**

- **Core** — Skills agents should consider for most tasks
- **Performance** — Optimization, profiling, allocation-related
- **Feature** — Feature-specific (WebSocket, serialization, etc.)

---

## Size Limits

| Lines | Status | Action |
|-------|--------|--------|
| < 200 | Ideal | Focused, easy to consume |
| 200–300 | Good | Acceptable for complex topics |
| 300–500 | Large | Consider splitting |
| > 500 | **MUST Split** | Lint script blocks commit |

Run the size linter: `bash scripts/lint-skill-sizes.sh`

---

## Naming Conventions

- **lowercase-kebab-case**: `create-test.md`, `use-pooling.md`
- **verb-noun pattern preferred**: `create-`, `use-`, `avoid-`, `debug-`

---

## Content Rules

1. **No duplication** — Reference other skills, don't copy content
2. **Inline code examples** — Under 20 lines stay in the skill file
3. **External code examples** — Longer examples go in `.llm/code-samples/`
4. **Reference tables** — Reusable tables go in `.llm/references/`

---

## Editing Workflow

1. Edit the skill file
2. Run size linter: `bash scripts/lint-skill-sizes.sh`
3. If > 300 lines: consider splitting now
4. If > 500 lines: **STOP** — must split before continuing
5. Run format linter: `bash scripts/lint-llm-instructions.sh`
6. Regenerate index: `bash scripts/generate-skills-index.sh`
7. Verify context.md updated correctly

---

## Related Skills

- [Rust-idioms-and-patterns](./rust-idioms-and-patterns.md) — Patterns that skills should reference
- [testing-strategies](./testing-strategies.md) — Testing methodology that all skills reference
- [clippy-and-linting](./clippy-and-linting.md) — Linting workflow skills must follow
