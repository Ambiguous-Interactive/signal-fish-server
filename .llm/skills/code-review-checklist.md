# Skill: Code Review Checklist

<!--
  trigger: review, code-review, pr-review, pull-request, quality, audit
  | AI-driven code review with structured output
  | Core
-->

**Trigger**: When reviewing code changes, pull requests, or auditing code quality.

---

## When to Use

- Reviewing pull requests or merge requests
- Before committing significant changes
- During refactoring sessions
- Auditing existing code for quality issues
- When asked to review or critique code

---

## When NOT to Use

- Writing brand new code from scratch
- Purely cosmetic/formatting changes (use linters instead)
- Reviewing auto-generated code (migrations, bindings)

---

## TL;DR

- Use structured output: severity, file, line, issue, fix
- Focus on bugs, security, and logic — never nitpick formatting
- Apply Writer/Reviewer separation: fresh context catches more issues
- Rate confidence for each finding (high/medium/low)
- Verify with tests and clippy, don't rely on visual inspection alone

---

## Review Output Format

For each issue found, use this structured format:

```text
**[SEVERITY]** File: path/to/file | Line: ~N
Issue: One-line description of the problem
Fix: Concrete suggested resolution
Confidence: high | medium | low

```

Severity levels:

- **CRITICAL** — Bugs, security vulnerabilities, data loss, crashes
- **WARNING** — Logic errors, missing error handling, performance issues
- **SUGGESTION** — Improvements, better patterns, readability

---

## Pre-Review Verification

Before reviewing logic, verify the basics:

- [ ] Code compiles: `cargo check --all-targets --all-features`
- [ ] Tests pass: `cargo test --all-features`
- [ ] Lints clean: `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] Formatted: `cargo fmt --check`

If any fail, report them as CRITICAL before reviewing other issues.

---

## Correctness Checklist

- [ ] New code handles all error paths (no silent failures)
- [ ] Edge cases identified: empty inputs, zero values, Unicode, max sizes
- [ ] No unintended behavioral changes to existing functionality
- [ ] State machines have exhaustive match arms
- [ ] Concurrent access is properly synchronized

```rust
// ❌ Panics if room doesn't exist
let room = rooms.get(&code).unwrap();
room.add_player(player_id);

// ✅ All paths handled
let room = rooms.get(&code).ok_or(JoinError::RoomNotFound)?;
room.add_player(player_id).map_err(JoinError::RoomFull)?;

```

---

## Security Checklist

### Rust-Specific

- [ ] No `unwrap()` / `expect()` on user-controlled input
- [ ] SQL queries use parameterized queries (sqlx bind params)
- [ ] No secrets, tokens, or API keys hardcoded
- [ ] Auth checks on all new endpoints
- [ ] No `unsafe` blocks without documented justification
- [ ] Input sizes validated before allocation (prevent OOM)
- [ ] WebSocket message sizes bounded

### TypeScript-Specific

- [ ] Input validation on all API boundaries
- [ ] No `eval()` or dynamic code execution
- [ ] XSS prevention in rendered output
- [ ] CORS configuration changes reviewed
- [ ] No sensitive data in localStorage/sessionStorage

---

## Performance Checklist

- [ ] No N+1 query patterns (batch database calls)
- [ ] No unbounded collection growth (Vec, HashMap without limits)
- [ ] No blocking I/O in async contexts (`std::fs` → `tokio::fs`)
- [ ] No excessive `.clone()` where borrows suffice
- [ ] No `Arc<Mutex<HashMap>>` where `DashMap` fits
- [ ] String allocations minimized in hot paths

```rust
// ❌ Blocking I/O in async context
async fn load_config() -> Config {
    let data = std::fs::read_to_string("config.json").unwrap(); // BLOCKS runtime
    serde_json::from_str(&data).unwrap()
}

// ✅ Non-blocking
async fn load_config() -> Result<Config, ConfigError> {
    let data = tokio::fs::read_to_string("config.json").await?;
    Ok(serde_json::from_str(&data)?)
}

```

---

## Testing Checklist

- [ ] New behavior has corresponding test(s)
- [ ] Error paths tested, not just happy paths
- [ ] Tests are deterministic (no time/order/random dependencies)
- [ ] Async tests use `#[tokio::test]` with proper timeouts
- [ ] No `thread::sleep` in tests (use tokio::time::sleep or condition-based waiting)
- [ ] Test names describe the scenario: `test_join_room_returns_error_when_full`

---

## SOLID Principle Quick Checks

| Check | Question to Ask |
|-------|----------------|
| **S**RP | Does this function/struct do exactly one thing? |
| **O**CP | Can new behavior be added without modifying this code? |
| **L**SP | Do all trait implementations honor the trait's contract? |
| **I**SP | Is any implementor forced to stub out methods it doesn't need? |
| **D**IP | Does high-level code depend on concrete types it shouldn't? |

See [solid-principles-enforcement](./solid-principles-enforcement.md) for detailed guidance.

---

## Anti-Patterns in AI Code Review

### DO NOT

- Flag formatting issues (linters handle this)
- Invent issues that don't exist — if uncertain, say "Potential issue (low confidence)"
- Suggest rewriting working code without clear defect
- Use vague language ("this could be improved") without a specific suggestion

### DO

- Show reasoning for each finding
- Provide concrete fix suggestions with code
- Rate confidence honestly — low confidence is better than false certainty
- Verify claims by checking the actual code, not assuming

---

## Writer/Reviewer Separation

When reviewing your own code changes, simulate a fresh perspective:

1. Complete the implementation
2. Re-read the diff as if seeing it for the first time
3. Check each item in the checklists above
4. Run verification commands (test, clippy)
5. Report findings in structured format

For agentic workflows, use a separate subagent for review. See [agentic-workflow-patterns](./agentic-workflow-patterns.md).

---

## Agent Checklist

- [ ] Pre-review: code compiles, tests pass, lints clean
- [ ] Correctness: error paths, edge cases, state machines
- [ ] Security: no unwrap on user input, parameterized SQL, auth checks
- [ ] Performance: no blocking async, no unbounded growth, no N+1
- [ ] Testing: new behavior tested, deterministic, error paths covered
- [ ] SOLID: single responsibility, focused traits, depend on abstractions
- [ ] Output: structured findings with severity, confidence, concrete fixes

---

## Related Skills

- [solid-principles-enforcement](./solid-principles-enforcement.md) — Detailed SOLID principle guidance
- [Rust-refactoring-guide](./rust-refactoring-guide.md) — Safe refactoring workflows
- [defensive-programming](./defensive-programming.md) — Zero runtime panics
- [error-handling-guide](./error-handling-guide.md) — Proper error propagation
- [agentic-workflow-patterns](./agentic-workflow-patterns.md) — Subagent review workflows
