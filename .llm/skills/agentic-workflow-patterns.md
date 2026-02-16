# Skill: Agentic Workflow Patterns

<!-- trigger: agent, agentic, workflow, subagent, ai-review, automation, ai-workflow, code-review-automation | Patterns for effective AI agent workflows and subagent dispatch | Core -->

**Trigger**: When planning or executing multi-step AI agent workflows, dispatching subagents, or structuring automated code review.

---

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
| "The test is wrong, I'll update it" | Tests document intended behavior; don't change the spec | Understand why the test fails before touching it |
| "I need more context, let me read everything" | Context overflow is the #1 agent failure mode | Use targeted grep/search; progressive disclosure |
| "I'll fix this warning later" | Warnings predict bugs; "later" never comes | Fix warnings before proceeding |
| "Two attempts is enough, I'll try once more" | Third attempt with same approach = same result | Two-Strike Rule: new approach required |

---

## TL;DR
- Implement → Verify → Review → Fix → Commit cycle for every change
- Use subagents for isolated tasks to keep context clean
- Verification beats inspection — always run cargo check/clippy/test
- Two-Strike Rule: new approach after 2 failed fixes
- Load context progressively, not all at once

---

## Core Workflow: Implement → Verify → Review → Fix → Present

Every significant code change follows this cycle:

```text

1. IMPLEMENT — Make the change (one logical change at a time)
2. VERIFY   — Run cargo check, clippy, test
3. REVIEW   — Check against code-review-checklist
4. FIX      — Address any issues found
5. VERIFY   — Run cargo check, clippy, test again
6. PRESENT  — Provide commit instructions to user (NEVER commit yourself)


```

**Never skip verification.** Running `cargo clippy` and `cargo test` catches more issues than visual inspection.

**⛔ CRITICAL:** Step 6 is "PRESENT" not "COMMIT" — you provide instructions, user commits. See [git-safety-protocol](./git-safety-protocol.md).

---

## Subagent Dispatch Patterns

### When to Use Subagents
- Tasks affecting different files/subsystems (parallel-safe)
- Code review of your own changes (fresh context)
- Research tasks that require reading many files
- Tasks that might fill the context window

### Subagent Prompt Structure

Effective subagent prompts are:

1. **Specific** — exact files, exact problem, exact expected output
2. **Constrained** — what NOT to do is as important as what to do
3. **Self-contained** — all needed context included, no assumptions

```markdown
## Good Subagent Prompt
Fix the error handling in src/server/room_manager.rs:

1. Read the file and find all `.unwrap()` calls on user input
2. Replace each with proper `?` propagation using the existing error types
3. Run `cargo check` to verify compilation
4. Run `cargo test --lib` to verify no regressions

Do NOT:

- Change function signatures that are part of the public API
- Modify error types (use existing ones)
- Touch files other than room_manager.rs

Return: List of changes made with before/after snippets.

```markdown
## Bad Subagent Prompt (too vague)
Fix the error handling in the server code.
Make it better and more robust.

```

### Sequential vs Parallel Dispatch

**Sequential** — when tasks depend on each other:

- Refactor struct → Update all usages → Add tests

**Parallel** — when tasks are independent:

- Fix auth module + Fix relay module + Update docs
- Default batch: 2-3 independent tasks

### Multi-Agent Coordination

For complex tasks requiring multiple agents:

1. **Dispatcher** (you) — Plans subtasks, assigns to subagents, integrates results
2. **Workers** (subagents) — Execute isolated tasks with constrained context
3. **Reviewer** (subagent) — Reviews integrated result with fresh context

**Handoff protocol:**

- Include: exact files, exact problem, expected output format
- Exclude: exploration context, previous conversation history
- Constrain: what NOT to change, what NOT to read
- Verify: how the dispatcher will check results

---

## Context Management

### Progressive Disclosure

Load context in layers, not all at once:

1. **Level 1**: Read skill metadata (name, description, trigger keywords)
2. **Level 2**: Read full skill content when task matches
3. **Level 3**: Read referenced files only when needed

### Context Efficiency Rules

- **Don't read entire codebase upfront** — use targeted searches
- **Use grep/search to find relevant files** before reading them
- **Read related files in parallel** when you know what you need
- **Summarize findings** before proceeding to implementation
- **Close investigation early** — if you have enough context, act

### Avoiding Context Pollution

- Use subagents for exploratory research (their context is isolated)
- Don't mix unrelated tasks in one conversation
- After completing a task, mental-reset before starting the next

---

## Context Budget Planning

Estimate context needs before starting:

| Task Type | Files to Read | Strategy |
|-----------|--------------|----------|
| Single-file fix | 1-3 files | Direct work, no subagent |
| Cross-file refactor | 5-15 files | Read interfaces first, then implementations |
| New feature | 10-30 files | Use subagent for research phase |
| Architecture change | 20+ files | MUST use subagents; split into phases |

Rules:

- If estimated tokens > 30K, use subagents for research
- Read trait/interface definitions BEFORE implementation files
- Read test files to understand expected behavior
- Use grep to find relevant files, not directory listing

---

## Verification-First Development

Verification is the highest-leverage activity. Prioritize it over everything else.

For the complete verification command sequence, see [agent-self-review-checklist](./agent-self-review-checklist.md).

### Verification Rules

- Run verification after EVERY logical change, not just at the end
- If verification fails, fix before proceeding — don't accumulate errors
- If tests fail, understand WHY before fixing — the test might be correct
- Never delete a failing test to make the build green

---

## Quality Gates

Before considering any task "done", walk through the verification checklist in [agent-self-review-checklist](./agent-self-review-checklist.md). All gates must pass before committing.

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
| **Creating git commits or modifying git config** | **NEVER commit or configure git - provide instructions to user instead** |
| Fixing symptoms, not root cause | Read the full error trace, find the origin |
| Over-reading files (context overflow) | Use targeted grep, read specific line ranges |
| Making multiple changes without verifying | One change → verify → next change |
| Guessing at API signatures | Read the actual type definitions first |
| Ignoring compiler warnings | Warnings often predict bugs; fix them |
| Changing test expectations instead of fixing code | Tests document intended behavior; respect them |
| Editing test to match broken code | Tests are the spec; fix the code to match the test |
| Adding `#[allow(unused)]` to suppress warnings | Unused code is dead code; delete it or implement it |
| Using `todo!()` or `unimplemented!()` in production code | These panic at runtime; use proper error handling |
| Creating a new utility when one already exists | Search the codebase first with grep before creating |
| Apologizing instead of acting | Never apologize; state what was wrong and what was fixed |

---

## Structured Review Output

When acting as a reviewer, use this format:

```markdown
## Code Review: [description of changes]

### Summary
[1-2 sentence overview of what was changed and overall assessment]

### Findings

**[CRITICAL]** `path/to/file.rs` ~L42
Issue: Description of the critical problem
Fix: Concrete code suggestion
Confidence: high

**[WARNING]** `path/to/file.rs` ~L87
Issue: Description of the warning
Fix: Suggested improvement
Confidence: medium

### Verdict
- [ ] Ready to merge
- [ ] Needs fixes (list critical items)
- [ ] Needs rework (fundamental issues)

### Verification
- [x] cargo check passes
- [x] cargo clippy clean
- [x] cargo test passes
- [ ] New tests added for new behavior


```

---

## Agent Checklist

- [ ] One logical change at a time
- [ ] Verify (check, clippy, test) after each change
- [ ] Use subagents for isolated review and research
- [ ] Subagent prompts are specific, constrained, self-contained
- [ ] Load context progressively, not all at once
- [ ] Two-strike rule: new approach after 2 failed fixes
- [ ] Structured review output with severity and confidence
- [ ] All quality gates pass before marking task done

---

## Related Skills

- [git-safety-protocol](./git-safety-protocol.md) — **CRITICAL** - Never commit or configure git
- [code-review-checklist](./code-review-checklist.md) — Detailed review criteria
- [solid-principles-enforcement](./solid-principles-enforcement.md) — SOLID checks during review
- [Rust-refactoring-guide](./rust-refactoring-guide.md) — Safe refactoring workflow
- [testing-strategies](./testing-strategies.md) — Test writing methodology
- [manage-skills](./manage-skills.md) — How to create and maintain skills
- [agent-self-review-checklist](./agent-self-review-checklist.md) — Pre-commit verification workflow
