# Agent Skills Standards

Use the open Agent Skills format as the portability baseline. Its required unit is a named folder
with a `SKILL.md` containing YAML `name` and `description`; `scripts/`, `references/`, and `assets/`
are optional colocated resources.

## Design Sources

- Read the [Agent Skills specification](https://agentskills.io/specification) when changing schema,
  naming, or directory rules.
- Read [skill-creation best practices](https://agentskills.io/skill-creation/best-practices) when
  deciding whether to split a skill or extract resources.
- Read [description optimization](https://agentskills.io/skill-creation/optimizing-descriptions)
  when a skill triggers too broadly or fails to trigger.
- Read the [client implementation guide](https://agentskills.io/client-implementation/adding-skills-support)
  when changing catalog or loader behavior.

## Repository Decisions

- Keep the portable `name` and `description` fields only. Add optional or product-specific metadata
  only for an identified consumer and validate it separately.
- Keep skills in `.llm/skills` because `.llm/context.md` is this repository's cross-agent harness.
  The generated catalog provides discovery for agents that do not scan this custom location.
- Enforce 300 lines per Markdown file, which is stricter than the open recommendation to keep a
  `SKILL.md` under 500 lines and roughly 5,000 tokens.
- Treat `agents/openai.yaml` as optional UI metadata, not part of the portable core. Add it only if
  these repository skills are packaged for an OpenAI UI surface that consumes it.
