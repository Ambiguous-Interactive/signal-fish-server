---
name: agent-self-review-checklist
description: >-
  Review completed work for correctness, completeness, safety, and evidence. Use before marking a
  task complete, presenting changes, or handing work back to the user.
---

# Agent Self-Review Checklist

## When to Use

- Before marking any task as complete
- Before presenting changes to user (user commits, not you)
- When reviewing your own work for quality and correctness
- After implementing a fix to verify it actually resolves the issue
- Before responding "done" to the user

⛔ **CRITICAL**: You NEVER commit changes - see [Git Safety Forbidden Operations](../git-safety/references/forbidden-operations.md)

---

## When NOT to Use

- When reviewing another developer's code (use [Code Review Checklist](../code-review-checklist/SKILL.md) instead)
- When just exploring or reading code without making changes
- During initial research or context gathering

## TL;DR

- Run cargo check → clippy → test → fmt after every change
- Run worktree hook preflights before handoff; do not rely on staged/push hook dry runs
- After `.llm` edits, run `scripts/check-llm-file-sizes.sh` before handoff
- Use Deep Review checklist for significant changes
- Walk the "Am I Done?" decision tree before committing
- Never modify test expectations to make tests pass

---

## Rationalizations to Reject

| Excuse | Why It's Wrong | Required Action |
|--------|---------------|-----------------|
| "It compiled, so it's correct" | Compilation doesn't verify logic, edge cases, or security | Run full test suite AND review against checklist |
| "The tests pass, ship it" | Tests might not cover the changed paths; coverage gaps exist | Verify new code has corresponding new tests |
| "I checked manually, no need for formal review" | Manual review misses systematic patterns agents catch | Use the structured checklist every time |
| "This is just a refactor, no review needed" | Refactors are the most common source of subtle regressions | Test suite + clippy + review, same as any change |

---

## Quick Review (Every Change, < 2 min)

Run these four commands after every Rust change. All must pass before proceeding:

```bash
cargo check --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo fmt --check

```

If any command fails, fix the issue and re-run the full set.

---

## TypeScript Quick Review (Dashboard / Frontend / Infra)

For changes in `dashboard/`, `frontend/`, `client-portal/`, or `infra/`:

```bash

cd "$PROJECT_DIR"
npm run format
npm run lint
npm run build

```

Run from the specific project directory. All three must pass.

---

## Deep Review Checklist (Significant Changes)

For non-trivial changes, dispatch as a subagent or work through manually:

### Rust Code Quality

- [ ] No new `unwrap()` on user input or external data
- [ ] Production `.expect()` / `.unwrap()` additions are either removed or explicitly documented
      with a nearby `SAFETY:` rationale plus matching `#[allow(clippy::*_used)]`
- [ ] No new `clone()` where a reference would work
- [ ] Error messages are actionable (include context about what failed)
- [ ] Public API changes have doc comments
- [ ] New async functions don't block the runtime
- [ ] Database queries use parameterized inputs (no string interpolation)
- [ ] No hardcoded secrets, URLs, or credentials
- [ ] Sensitive data not logged at info/debug level

### Test Coverage

- [ ] New behavior has corresponding new tests
- [ ] Edge cases covered (empty input, max values, invalid data)
- [ ] Error paths tested, not just happy paths
- [ ] Existing tests not modified to make them pass (understand why they fail first)

---

## GitHub Actions / CI Workflow Review

For changes in `.github/workflows/` or CI configuration:

- [ ] **Language ecosystem match**: Caching and tools match project language (Rust = `rust-cache`, not pip)
- [ ] **Hash files exist**: Files in `hashFiles()` exist (Cargo.lock for Rust, not requirements.txt)
- [ ] **Pinned versions recent**: Action SHAs <1 year old, nightly toolchains <6 months old
- [ ] **Documentation complete**: Workflow has header comment, pinned versions have update criteria
- [ ] **Local scripts use interpreters**: Workflow `run:` commands use `bash`,
  `pwsh -File`, `awk -f`, or `node` for repo scripts, never direct
  `scripts/foo.sh` / `./scripts/foo.sh` execution
- [ ] **Config drift guards assert the live view**: presence checks
  (`assert!(c.contains(..))`) on comment-bearing config (`.yml`, `Dockerfile`,
  `.sh`, `.ps1`, `.toml`, JSONC) read via `read_live_file` / `strip_comment_lines`,
  never raw `read_file` — a commented-out line must not satisfy the guard. Absence
  checks (`!c.contains`) and Markdown (`#` = heading) stay raw. See
  [CI Config Live View Tests](../ci-config-live-view-tests/SKILL.md)
- [ ] **MSRV consistency**: If Rust version changed, updated in ALL files (`Cargo.toml`, `rust-toolchain.toml`,

  clippy.toml, Dockerfile)

- [ ] **Tested in CI**: Pushed to branch and verified workflow passes
- [ ] **Cache keys versioned**: Cache keys include version suffix for invalidation (`v1`, `v2`)
- [ ] **Permissions minimal**: Only required permissions granted (default: `contents: read`)

## AWS / Infrastructure Review (CDK / Infra Changes)

For changes in `infra/`, CDK stacks, or Terraform files:

- [ ] No IAM `*` resource wildcards
- [ ] All resources have required cost allocation tags
- [ ] Non-prod environments use cost-saving defaults
- [ ] No hardcoded account IDs or regions
- [ ] Removal policies appropriate (RETAIN for databases, DESTROY for logs)
- [ ] VPC endpoints used instead of NAT for AWS service access
- [ ] Graviton/ARM64 used where possible
- [ ] `cdk synth` produces valid template
- [ ] `cdk diff` shows only expected changes

---

## Security Review (Auth, Input, or Networking Changes)

For any change touching authentication, user input, or network configuration:

- [ ] Input validation on all user-facing endpoints
- [ ] Rate limiting on all public endpoints
- [ ] Authentication required for non-health endpoints
- [ ] WebSocket messages validated before processing
- [ ] No new dependencies with known vulnerabilities (`cargo audit` / `cargo deny check`)
- [ ] CORS configuration uses explicit origin list (never `*`)
- [ ] No secrets in logs, error messages, or responses

---

## Git Safety Check (BEFORE Presenting Changes)

⛔ **MANDATORY**: Before presenting changes to user, verify you have NOT:

- [ ] Created any git commits (`git commit`)
- [ ] Modified git configuration (`git config user.*`)
- [ ] Staged files without explicit user request (`git add`)
- [ ] Pushed to remote (`git push`)
- [ ] Modified git history (`git rebase`, `git reset --hard`, `git commit --amend`)

✅ **REQUIRED**: Instead, provide clear commit instructions for user to execute.

See [Git Safety Forbidden Operations](../git-safety/references/forbidden-operations.md) for complete details.

---

## "Am I Done?" Decision Tree

Work through this tree top-to-bottom. Stop at the first NO and fix it.

```text
Compiled? ─── NO ──► Fix compilation errors first
    │ YES
    ▼
Clippy clean? ─── NO ──► Fix all warnings
    │ YES
    ▼
Tests pass? ─── NO ──► Fix failing tests (don't modify test expectations)
    │ YES
    ▼
Formatted? ─── NO ──► Run cargo fmt / npm run format
    │ YES
    ▼
New tests for new behavior? ─── NO ──► Add tests
    │ YES
    ▼
Policy scripts pass? ─── NO ──► Fix doc-consistency / workflow-hygiene / LLM issues
    │ YES
    ▼
Worktree hook preflights pass? ─── NO ──► Run: pwsh -NoLogo -NoProfile -NonInteractive
    │ YES                                   -File scripts/hooks/pre-commit.ps1 -Worktree
    │                                       pwsh -NoLogo -NoProfile -NonInteractive
    │                                       -File scripts/hooks/pre-push.ps1 -Worktree
    ▼
Hook test suites pass? ─── NO ──► Run: cargo test --locked --test doc_consistency_*
    │ YES                          --test ci_config_tests
    ▼
Deep review passed? ─── NO ──► Fix findings
    │ YES
    ▼
Git safety verified? ─── NO ──► Remove git commits/config; provide instructions instead
    │ YES
    ▼
✅ DONE — present to user with commit instructions

```

**Key rule**: Never modify test expectations to make tests pass.
If a test fails, understand _why_ the existing expectation exists before changing anything.

---

## Common Self-Review Failures

| Failure Pattern | Consequence | Prevention |
|----------------|-------------|------------|
| Skipping clippy | Subtle bugs ship (e.g., unused results, redundant clones) | Always run clippy with `-D warnings` |
| Not running tests after "small" change | Regressions in unrelated modules | Run full test suite every time |
| Reviewing only changed files | Missing broken imports, type mismatches in dependents | `cargo check --all-targets` catches cross-file issues |
| Forgetting TypeScript checks | Lint errors in dashboard/frontend discovered later | Run format + lint + build for any TS change |
| Committing without `cargo fmt` | Formatting noise in next commit | Always `cargo fmt --check` before presenting to user |
| Creating git commits | Wrong author attribution, user loses control | NEVER commit - provide instructions to user instead |

---

## Subagent Dispatch Template

When dispatching a review subagent, use this prompt structure:

```text
Review the following changes against the self-review checklist:

1. Run Quick Review commands
2. Apply Deep Review checklist items relevant to the change type
3. Apply Security Review if auth/input/networking touched
4. Apply AWS Review if infrastructure touched
5. Walk the "Am I Done?" decision tree
6. Report: PASS with summary, or FAIL with specific items to fix


```

---

## Post-Review Actions

After the checklist passes, provide these instructions to the user:

### What YOU Do (Agent)

1. **Verify changes**: All checks pass
2. **Summarize changes**: Clear description of what was modified
3. **Provide commit instructions**: Exact commands for user to run

### What USER Does (Not You)

1. **Review changes**: `git status` and `git diff` to inspect
2. **Stage changes**: `git add <files>` or `git add -p` (review each hunk)
3. **Commit**: Follow conventional commits (`feat:`, `fix:`, `refactor:`)
4. **Push**: `git push origin branch-name` when ready

⛔ **YOU NEVER**: Stage, commit, configure git, or push. See [Git Safety Forbidden Operations](../git-safety/references/forbidden-operations.md).

## Related Skills

- [Git Safety Forbidden Operations](../git-safety/references/forbidden-operations.md) — **CRITICAL** -
  Never commit or configure git
- [Code Review Checklist](../code-review-checklist/SKILL.md) — For reviewing others' code
- [Agentic Workflow Patterns](../agentic-workflow-patterns/SKILL.md) — Implement → Verify → Review → Present cycle
- [Testing Core Patterns](../testing/SKILL.md) — How to write effective tests
