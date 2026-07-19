---
name: manage-skills
description: >-
  Create, restructure, review, and validate portable Agent Skills in `.llm/skills`. Use when adding
  a skill, editing `SKILL.md`, grouping supporting resources, tuning skill discovery metadata, or
  regenerating the skill catalog.
---

# Manage Skills

Treat each skill as a self-contained capability that agents discover from metadata and load only
when relevant. Read [Agent Skills Standards](references/agent-skills-standards.md) before changing
the library contract or introducing product-specific metadata.

## Required Layout

```text
.llm/skills/<skill-name>/
├── SKILL.md
├── references/  # Optional knowledge loaded on demand
├── scripts/     # Optional deterministic or repeated operations
└── assets/      # Optional files copied or transformed into outputs
```

Keep the folder name identical to the frontmatter `name`. Use lowercase ASCII letters, digits, and
hyphens; keep names under 64 characters. Do not place skill entrypoints directly in
`.llm/skills/`.

## Write the Entrypoint

Start every `SKILL.md` with exactly the portable discovery fields:

```markdown
---
name: inspect-release
description: >-
  Inspect release readiness and publication state. Use when preparing a release, recovering a
  partial publication, or validating tags and registry state.
---

# Inspect Release
```

Make `description` answer both questions agents must decide before loading the body:

1. What capability does this skill provide?
2. Which concrete requests, files, failures, or situations should trigger it?

Put all activation guidance in the description. Keep the body procedural and use imperative
language. Do not recreate legacy trigger comments or `When to Use` boilerplate merely for
discovery.

## Apply Progressive Disclosure

Keep `SKILL.md` focused on the workflow and routing decisions needed every time the skill runs.
Move details into a resource only when they are conditional, reusable, or large:

- Put domain facts, detailed examples, and variant guidance in `references/`.
- Put reliable repeated operations in `scripts/`, then execute representative paths after edits.
- Put templates and output material that agents need not read into context in `assets/`.

Link every bundled resource directly from `SKILL.md` and say when to load or run it. Avoid chains
where a resource is discoverable only through another resource. Keep information in one place;
do not summarize the same rules in both the entrypoint and a reference.

Keep each Markdown file within the repository's stricter 300-line limit. Prefer fewer coherent
skills over several entrypoints with indistinguishable triggers, but split capabilities that serve
independent user tasks.

## Work Red-Green

1. Define a representative request or structural assertion that the current skill fails.
2. Record the failure or baseline measurement before editing.
3. Make the smallest coherent content or structure change.
4. Run the structural validator and relevant bundled scripts.
5. Forward-test complex behavior with an isolated task when doing so is safe and authorized.
6. Compare the result with the baseline and keep only evidence-backed changes.

Do not leak the expected answer into a forward-test prompt. Pass the skill and raw task artifacts,
then evaluate whether its discovery and instructions generalize.

## Validate and Publish the Catalog

Run the portable structural validator:

```bash
python3 .llm/skills/manage-skills/scripts/validate_skills.py
```

Regenerate the discovery catalog after any entrypoint or description change:

```bash
python3 .llm/skills/manage-skills/scripts/generate_skills_index.py
python3 .llm/skills/manage-skills/scripts/generate_skills_index.py --check
```

Then run repository Markdown and link checks. If a skill bundles an executable script, run at least
one success case and one relevant failure or edge case.

## Review Checklist

- Confirm the folder and frontmatter names match.
- Confirm the description states capability and concrete triggers without becoming universal.
- Confirm the body contains only instructions needed after activation.
- Confirm every resource is linked directly with a load/run condition.
- Confirm relative links resolve from the file that contains them.
- Confirm no content was duplicated or lost during regrouping.
- Confirm validation and catalog freshness checks pass.

## Related Skills

- [Agentic Workflow Patterns](../agentic-workflow-patterns/SKILL.md) — Structure task-driven agent work.
- [Documentation](../documentation/SKILL.md) — Maintain accurate project Markdown.
- [Testing](../testing/SKILL.md) — Design evidence-backed validation.
