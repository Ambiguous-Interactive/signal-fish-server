# Skill: Manage Skills

<!--
  trigger: skill, skills, manage, create-skill, edit-skill, split-skill
  | Creating, editing, and maintaining skill files
  | Core
-->

**Trigger**: When creating, editing, splitting, or organizing skill files in `.llm/skills/`.

---

## When to Use

- Creating a new skill file
- Editing an existing skill to add or update content
- Splitting a skill that exceeds the size limit
- Reviewing skill compliance with formatting rules
- Regenerating the skills index in `.llm/skills/index.md`

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
- **Infrastructure** — CI/CD, workflows, shell scripting, deployment, toolchain management

---

## Size Limits

| Lines | Status | Action |
|-------|--------|--------|
| < 200 | Ideal | Focused, easy to consume |
| 200–300 | Good | Acceptable for complex topics |
| = 300 | At limit | Warnings issued; trim soon |
| > 300 | **MUST Split** | `check-llm-file-sizes.sh` blocks commit |

Run the size linter: `./scripts/check-llm-file-sizes.sh`

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
5. **Prefer focused extensions** — If a new section mostly adds one distinct topic, create a new skill
   or move the detail into an existing focused skill instead of growing a general-purpose file

---

## Editing Workflow

1. Edit the skill file
2. Check related skills for overlap before adding large new sections
3. Run size linter: `./scripts/check-llm-file-sizes.sh`
4. Run regression guard: `cargo test --test llm_file_size_script_tests`
5. If > 300 lines: **STOP** — must split before committing
6. Regenerate index: `./scripts/generate-skills-index.sh`
   - Index ordering: deterministic `LC_ALL=C` sort by file path/filename (not by title)
7. Ensure `.llm/skills/index.md` is updated and staged
8. Verify `.llm/context.md` references `skills/index.md`

---

## Related Skills

- [Rust-idioms-and-patterns](./rust-idioms-and-patterns.md) — Patterns that skills should reference
- [testing-strategies](./testing-core-patterns.md) — Testing methodology that all skills reference
- [clippy-and-linting](./clippy-and-linting.md) — Linting workflow skills must follow
