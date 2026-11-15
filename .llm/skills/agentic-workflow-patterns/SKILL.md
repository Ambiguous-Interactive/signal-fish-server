---
name: agentic-workflow-patterns
description: >-
  Plan and execute multi-step agent workflows with clear task boundaries and verification. Use
  when coordinating agentic work, dispatching subagents, or structuring automated code review.
---

# Agentic Workflow Patterns

For a GOAL.md session, the user authorizes publication and merging of the fully
green session PR. Follow the [GOAL completion gate](../mandatory-workflow/SKILL.md#goal-completion-gate),
then synchronize local git state and end the session. That authorized workflow
takes precedence over the user-commit-only advice below and in linked references.

## When to Use

- Planning multi-step implementation tasks
- Dispatching subagents for review or implementation
- Structuring automated code review workflows
- Optimizing context usage across agent interactions
- Recovering from failed agent attempts

---

## When NOT to Use

- Simple single-file changes that don't need orchestration
- User-interactive workflows (this is for autonomous operation)
- Manual code review by humans

---

## Rationalizations to Reject

| Excuse | Why It's Wrong | Required Action |
|--------|---------------|-----------------|
| "I'll test at the end" | Errors compound; late testing means debugging ALL changes at once | Verify after EVERY logical change |
| "This is a small change, skip review" | Small changes cause most production incidents | All changes get the full cycle |
| "The test is wrong, I'll update it" | Tests document intended behavior | Understand why the test fails before touching it |
| "I need more context, let me read everything" | Context overflow is the #1 agent failure mode | Use targeted grep/search; progressive disclosure |
| "I'll fix this warning later" | Warnings predict bugs; "later" never comes | Fix warnings before proceeding |
| "Two attempts is enough, I'll try once more" | Third attempt with same approach = same result | Two-Strike Rule: new approach required |

---

## TL;DR

- Implement → Verify → Review → Fix → Commit cycle for every change
- Use subagents for isolated tasks to keep context clean
- Verify changed behavior with scoped tests and lint
- Two-Strike Rule: new approach after 2 failed fixes
- Load context progressively, not all at once
- Write all user-facing text in Simplified Technical English — canonical scope
  list in `.llm/context.md` ("Writing Style for User-Facing Text"): extremely
  short (a few sentences max), answer "how" (plus "why/what" in PR
  descriptions), active voice, no fluff

---

## Bounded task workflow

1. Select one task with a failing reproduction or measured baseline and acceptance.
2. Assign one owner. Use up to two workers only for independent work.
3. Give workers owned files, exclusions, commands, and expected evidence.
4. Implement one logical change and run its scoped red-green checks.
5. Use one independent review. Revisit concrete unresolved findings only.
6. Accept when the task's checks and review findings are resolved. Escalate
   unresolved correctness findings; do not seek indefinite unanimous approval.
7. Publish only when authorized, using the mandatory workflow completion gate.

Use a coordination session for cross-component dependencies and ownership.
Use a focused implementation session for ordinary tasks. Keep handoffs to the
revision, next action, reproduction, results, and blockers. Avoid rescanning the
backlog during implementation. Compare accepted-task time, rework, and compute
before increasing team size.

Workers must not run competing Cargo builds in one target directory. The owner
runs integrated verification; use separate worktrees and target directories for
concurrent implementation. Research and review can proceed while it builds.

## Context Management

### Progressive Disclosure

Load context in layers:

1. **Level 1**: Read skill metadata (name, description, trigger keywords)
2. **Level 2**: Read full skill content when task matches
3. **Level 3**: Read referenced files only when needed

### Context Efficiency Rules

- Don't read entire codebase upfront — use targeted searches
- Use grep/search to find relevant files before reading them
- Read related files in parallel when you know what you need
- Close investigation early — if you have enough context, act

### Context Budget Planning

| Task Type | Files to Read | Strategy |
|-----------|--------------|----------|
| Single-file fix | 1-3 files | Direct work, no subagent |
| Cross-file refactor | 5-15 files | Read interfaces first, then implementations |
| New feature | 10-30 files | Use subagent for research phase |
| Architecture change | 20+ files | Split into bounded independent tasks |

If estimated tokens > 30K, use subagents for research.

---

## Verification-First Development

Run verification after EVERY logical change, not just at the end.
If verification fails, fix before proceeding — don't accumulate errors.
Remove or rewrite a fragile check only after proving the intended behavior remains covered.

For the complete verification command sequence, see [Agent Self Review Checklist](../agent-self-review-checklist/SKILL.md).

---

## Error Recovery Patterns

### Two-Strike Rule

If you've attempted to fix an issue twice and it's still broken:

1. **Stop** — don't try a third time with the same approach
2. **Analyze** — read the error carefully, understand root cause
3. **Restart** — take a completely different approach
4. **Escalate** — if still stuck, document what was tried and report

### Common Agent Mistakes

| Mistake | Fix |
|---------|-----|
| **Publishing without authorization** | **Publish only within the authorized workflow** |
| Fixing symptoms, not root cause | Read the full error trace, find the origin |
| Over-reading files (context overflow) | Use targeted grep, read specific line ranges |
| Making multiple changes without verifying | One change → verify → next change |
| Changing test expectations instead of fixing code | Tests are the spec; fix the code |
| Adding `#[allow(unused)]` to suppress warnings | Unused code is dead code; delete or implement it |
| Creating a new utility when one already exists | Search the codebase first with grep |
| Apologizing instead of acting | State what was wrong and what was fixed |

---

## Structured Review Output

Write review text in Simplified Technical English (canonical scope list in
`.llm/context.md`): extremely short, active voice, common words, concrete
fixes. No praise padding, no hedging, no verbosity.

```markdown
## Code Review: [description of changes]

### Summary
[1-2 sentence overview of what was changed and overall assessment]

### Findings

**[CRITICAL]** `path/to/file.rs` ~L42
Issue: Description of the critical problem
Fix: Concrete code suggestion

**[WARNING]** `path/to/file.rs` ~L87
Issue: Description of the warning
Fix: Suggested improvement

### Verdict
- [ ] Ready to merge
- [ ] Needs fixes (list critical items)

### Verification
- [x] cargo check passes
- [x] cargo clippy clean
- [x] cargo test passes
```

---

## Agent Checklist

- [ ] One logical change at a time
- [ ] Verify each change with owning-target tests and lint
- [ ] Use subagents for isolated review and research
- [ ] Subagent prompts are specific, constrained, self-contained
- [ ] Load context progressively, not all at once
- [ ] Two-strike rule: new approach after 2 failed fixes
- [ ] All quality gates pass before marking task done

---

## Related Skills

- [Git Safety Forbidden Operations](../git-safety/references/forbidden-operations.md) — **CRITICAL** -
  Never commit or configure git
- [Code Review Checklist](../code-review-checklist/SKILL.md) — Detailed review criteria
- [Rust Refactoring Guide](../rust-refactoring-guide/SKILL.md) — Safe refactoring workflow
- [Testing Core Patterns](../testing/SKILL.md) — Test writing methodology
- [Agent Self Review Checklist](../agent-self-review-checklist/SKILL.md) — Pre-commit verification workflow
