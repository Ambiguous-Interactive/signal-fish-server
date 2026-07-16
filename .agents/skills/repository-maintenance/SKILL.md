---
name: repository-maintenance
description: Maintain Signal Fish repository policies, agent skills, drift guards, validation scripts, generated metadata, and mandatory quality gates. Use for AGENTS.md, .agents/skills, SKILL.md, agents/openai.yaml, repository validation scripts, source-hygiene rules, config-versus-wire-format drift, or standard workflow changes.
---

<!-- markdownlint-disable MD013 -->

# Repository Maintenance

Make policy machine-checkable when practical. Start with a failing fixture or structural check, implement the smallest source-of-truth change, and prove the original failure now passes.

## Route the task

- Read [manage-skills.md](references/manage-skills.md) before changing repository skills; apply its enduring content through the current `SKILL.md` package layout.
- Read [mandatory-workflow.md](references/mandatory-workflow.md) for full validation and handoff checklists.
- Read [repo-source-hygiene-guards.md](references/repo-source-hygiene-guards.md) for static drift guards and source-of-truth tests.
- Read [validation-script-output-modes.md](references/validation-script-output-modes.md) for quiet and diagnostic output contracts.
- Read [config-wire-format-drift.md](references/config-wire-format-drift.md) when configuration and serialized protocol values can diverge.

## Skill maintenance

Keep each `SKILL.md` concise and imperative. Put detailed material in directly linked `references/`, reusable deterministic operations in `scripts/`, and output resources in `assets/`. Regenerate `agents/openai.yaml` with the official skill-creator helper after trigger or scope changes. Run `scripts/validate-agent-skills.sh` and the skill-creator validator for every package.
