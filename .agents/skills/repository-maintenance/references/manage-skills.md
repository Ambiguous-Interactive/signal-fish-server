# Repository Skill Maintenance

Use this reference when creating, editing, splitting, consolidating, or validating repository skills under `.agents/skills/`.

## Package contract

Store every skill at `.agents/skills/<skill-name>/SKILL.md`. Use lowercase hyphen-case for both the directory and frontmatter `name`, with a maximum of 64 characters.

Use only `name` and `description` in `SKILL.md` frontmatter. Make the description carry the complete trigger contract: state what the skill does and list concrete tasks, files, or situations that should select it. Front-load the strongest matching terms because discovery may truncate long descriptions.

Add `agents/openai.yaml` with:

- a human-readable `display_name`;
- a 25–64 character `short_description`;
- a one-sentence `default_prompt` that explicitly names `$skill-name`.

Do not add icons, brand colors, tool dependencies, or invocation policy without a real requirement.

## Progressive disclosure

Keep `SKILL.md` focused on task routing and the essential workflow. Assume the agent already knows general software engineering.

- Put detailed domain knowledge and examples in `references/`.
- Put deterministic reusable operations in `scripts/` and execute representative cases after changes.
- Put templates or files copied into output in `assets/`.
- Link every reference directly from `SKILL.md`; avoid reference-to-reference discovery chains.
- Keep `SKILL.md` below 500 lines and split earlier when unrelated variants would load together.
- Do not add per-skill README, changelog, installation, or quick-reference files.

## Decide whether to split or consolidate

Create a separate skill when the task has a distinct trigger vocabulary and workflow. Keep variants as references when they share the same trigger and top-level procedure.

Consolidate skills when users would struggle to choose between them, their descriptions substantially overlap, or catalog size risks discovery truncation. Split a skill when selecting it routinely loads unrelated domain material or its workflow contains independent task families.

## Workflow

1. Capture concrete prompts that should and should not select the skill.
2. Establish a structural or behavioral red case before editing.
3. Scaffold a new package with the official skill-creator `init_skill.py`; skip initialization only for an existing package.
4. Write reusable resources first, then write the concise routing workflow.
5. Generate `agents/openai.yaml` with the official helper.
6. Run `scripts/validate-agent-skills.sh` and the official `quick_validate.py` against every changed package.
7. Run repository tests for scripts, hooks, or policies affected by the change.
8. Forward-test a complex skill on realistic prompts when an isolated agent is available and the test cannot affect production.

## Review checklist

- Confirm the frontmatter name matches the directory.
- Confirm the description includes scope and specific triggers.
- Confirm the body uses imperative language and has no scaffold TODOs.
- Confirm every routed reference exists and unrelated references are not loaded.
- Confirm UI metadata still matches the skill.
- Confirm old paths, trigger comments, and pseudo-skill headings were not reintroduced.
- Report the red baseline and green validation evidence at handoff.
