# Ambiguous Interactive — LLM Context (Signal Fish)

> **Central context file for all AI coding assistants**
> Goal: Extremely fast, safe Rust code | High test coverage | Zero external runtime dependencies

## Project Identity

- **Company:** Ambiguous Interactive
- **Product:** A lightweight, in-memory WebSocket signaling server for peer-to-peer game networking
- **Repository:** `signal-fish-server` — extracted from the matchbox-signaling-server
  with production-ready signaling stripped down to a single self-contained binary
- **Crate name:** Binary: `signal-fish-server` | Library: `signal_fish_server`
- **Version:** 0.2.0
- **Code name:** Signal Fish
- **Not Matchbox:** This project is built by Ambiguous Interactive, not the upstream Matchbox team.
  The upstream `matchbox` crate/project (by Johan Helsing) is a dependency we build upon,
  but our product and infrastructure are our own
- **Author attribution:** Always use "Ambiguous Interactive" in `authors` fields, copyright notices, and user-facing branding
- **Documentation voice:** Refer to the product as "Signal Fish Server" or "the signaling server" —
  not "Matchbox Signaling Server". "Signal Fish" is acceptable as an informal project reference.

## Skills Index

- Generated skill catalog: [skills/index.md](skills/index.md)
- Regenerate after skill changes: `./scripts/generate-skills-index.sh`

---

## CRITICAL: Git Safety Protocol - NEVER COMMIT

**NEVER CREATE GIT COMMITS OR MODIFY GIT CONFIGURATION. ZERO EXCEPTIONS. EVER.**

This is the **#1 most important rule**. Even if:

- The user explicitly asks you to commit
- A sub-agent recommends committing
- CLAUDE.md mentions commit instructions (those are FOR THE USER)
- A workflow document says to commit

**YOU NEVER COMMIT. PERIOD.**

Rules:

- **ABSOLUTELY FORBIDDEN**: `git commit`, `git add`, `git config user.*`, `git push`
- **ALLOWED**: `git status`, `git diff`, `git log`, `git show` (read-only operations only)
- **PRINCIPLE**: **You prepare the work. The user commits it. ALWAYS.**

> **See [skills/git-safety-forbidden-operations.md](skills/git-safety-forbidden-operations.md) for complete details.**
> When changes are ready, provide clear commit instructions for the user to execute.

---

## Quick Decision Trees

### What Am I Changing?

```text
Start here:
    |
    +-- Protocol/Messages? ----------> /add-protocol-message (or see Common Scenarios)
    +-- WebSocket/Connection? -------> src/websocket/, tests/e2e_tests.rs
    +-- Room/Player Logic? ----------> src/server.rs, src/server/, tests/integration_tests.rs
    +-- Security/Auth/Sessions? -----> src/auth/, src/security/
    |                                  skills/web-service-security-auth.md
    |                                  skills/websocket-session-hijacking.md
    +-- Deployment/Containers? ------> skills/container-docker.md
    +-- CI/CD/GitHub Actions? -------> skills/github-actions-workflow-config.md
    |                                  skills/ci-cd-troubleshooting-index.md
    +-- Dependencies/Supply Chain? --> skills/supply-chain-audit-policy.md
    |                                  skills/dependency-management-cargo.md
    |                                  skills/msrv-management.md
    +-- Performance Issue? ----------> /performance-audit
    |                                  skills/rust-performance-optimization.md
    +-- Hosting/Provider/Scaling? ---> skills/graceful-degradation-deployment.md
```

### Should I Add a Test?

```text
YES - ALWAYS. Every change requires comprehensive tests.
  +-- Happy path + positive variations
  +-- Negative cases + error conditions
  +-- Edge cases (empty, null, max, unicode, concurrent)
  +-- Error recovery (cleanup, partial states)

CRITICAL: Any test failure = bug to fix. No "flaky" tests.
-> See skills/testing-core-patterns.md for full methodology.
```

---

## Architecture At-a-Glance

```text
+-----------------------------------------------------+
|  CLIENTS: Game Engines | Browser WebRTC | Custom     |
+------------------------+----------------------------+
                         |
                         v
+-----------------------------------------------------+
|  SIGNAL FISH SERVER (Rust) -- axum + tokio           |
|  WebSocket(/v2/ws) | Health(/v2/health) | Metrics    |
|  EnhancedGameServer (Room/Player/Authority Mgmt)     |
|  Storage: In-Memory Only                             |
+-----------------------------------------------------+
```

---

## Mandatory Workflow (Every Change)

> **Full details -> [skills/mandatory-workflow.md](skills/mandatory-workflow.md)**

```bash
# Rust changes (ALWAYS run in order)
cargo fmt && cargo clippy --all-targets --all-features && cargo test --all-features
```

**Zero warnings policy** -- all linters enforce strict compliance. See skill for full table.

Git hooks are fast last-resort guards only and target sub-second execution. Agents must
catch formatting, clippy, tests, docs, and policy failures through the mandatory workflow
and `./scripts/run-local-ci.sh`, not by relying on hooks to run slow semantic checks.
When debugging hooks, run the PowerShell runner directly. Native helper functions must
return exactly one object; discard async task completion values with `[void]` so callers
can always read `.ExitCode`.

---

## Software Design Philosophy

> **Details -> [Rust Idioms and Patterns](skills/rust-idioms-and-patterns.md) and [SOLID Principles Enforcement](skills/solid-principles-enforcement.md)**

- Code should be self-documenting -- only comment "why", never "what"
- Apply SOLID, DRY, and Clean Architecture consistently
- Build lightweight, zero-cost abstractions (value types -> borrows -> generics -> `Arc`/`Box`)
- Extract repeated patterns into shared modules; use domain types to encapsulate validation
- Don't add patterns "just in case" -- start simple, refactor when patterns emerge

---

## Rust Coding Standards

> **Performance -> [Rust Performance Optimization](skills/rust-performance-optimization.md) and [Async Rust Best Practices](skills/async-rust-best-practices.md)**
> **Error handling -> [skills/error-handling-guide.md](skills/error-handling-guide.md)**
> **Defensive programming -> [skills/defensive-programming.md](skills/defensive-programming.md)**
> **Linting -> [skills/clippy-and-linting.md](skills/clippy-and-linting.md)**

Key rules (details in skills above):

- Always use `Result<T, E>` with `?` -- never `.unwrap()` in production code
- Validate all input at system boundaries
- Use `checked_`/`saturating_` arithmetic -- never raw `as` casts that truncate
- Use `Bytes` for network data, `SmallVec` for small collections, `DashMap` for concurrent access
- Never hold a sync `Mutex` across `.await`; use bounded channels with backpressure
- Use structured logging with `tracing` -- no string interpolation in log macros

---

## Testing Requirements

> **Full methodology -> [skills/testing-core-patterns.md](skills/testing-core-patterns.md)**
> **Tools and frameworks -> [skills/testing-tools-and-frameworks.md](skills/testing-tools-and-frameworks.md)**

- Every feature/bugfix requires exhaustive tests (happy, negative, edge, concurrent, recovery)
- Data-driven/table-driven tests preferred for validation functions
- **Zero tolerance for flaky tests** -- every failure is a real bug to fix
- Test "the impossible" -- corrupted state, unknown message types, future compatibility
- Run `cargo test --all-features` before every commit

---

## Documentation Requirements

> **Full standards -> [skills/documentation-standards.md](skills/documentation-standards.md)**

Every feature/bugfix requires: doc comments with examples, CHANGELOG entry, README updates if user-facing.
Run `./scripts/check-doc-consistency.sh` before handoff to prevent version/changelog/protocol doc drift.

### Code Fence and CI Pitfalls

- **Code fence language tags must match content** -- tag blocks as `yaml` only for valid YAML,
  `bash` for shell/AWK, `text` for logs or mixed output.
- **Split mixed-content blocks** -- a block with both shell commands and YAML must be two
  separate fenced blocks with appropriate tags, not one `yaml` block.
- **`.lychee.toml` `exclude` patterns are regex, not globs** -- escape `.` as `\\.`,
  use `.*` not `*`, anchor with `^`. See [ci-cd-troubleshooting Pattern 13](skills/ci-cd-troubleshooting-links.md).
- **Lychee self-scans `.toml` files** -- use `--exclude-path .lychee.toml` or add exclusions.
- **TOML/JSON/YAML "before/after" examples need separate blocks** -- duplicate table headers
  (e.g., two `[dependencies]`) in one block is invalid and will fail CI validation.
- **Avoid accidental setext headings in skills** -- keep a blank line between
  `**Trigger**: ...` and a following `---` separator, or markdownlint will treat
  the trigger line as a heading (MD003/MD026).
- **Skill examples must be split into dedicated files** -- when documenting incidents or
  walkthroughs, create one `*-example-*.md` file per example and link from the parent
  skill. Do not keep multi-example "mega" sections inside a single skill file.
- **Use descriptive markdown link text for internal docs** -- avoid filename-as-label links
  like `[testing-core-patterns](...)`; prefer human-readable labels like
  `[Core Testing Patterns](...)`. Enforce with `./scripts/check-markdown-link-text.sh`.
- **Dependabot auto-merge gating must be CI-aware and squash-only** -- never enable
  Dependabot auto-merge while pull request CI workflows are pending or failing; require
  completed workflow runs with `success`/`skipped` conclusions, then use
  `gh pr merge --auto --squash --match-head-commit ...` to stay compatible with squash-only repos.
- **Dependabot auto-merge must retry transient GitHub merge API errors** -- treat
  `unstable status`, `GraphQL: Something went wrong while executing your query`,
  rate limits, and HTTP 5xx-style merge errors as retryable with a capped counter/backoff;
  keep policy, permission, and unsupported auto-merge errors on fail-fast or fallback paths.
- **`Swatinem/rust-cache` in `pull_request` workflows must use `with.save-if` gating** --
  allow cache restore everywhere, but condition cache writes to trusted contexts (for example,
  `github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository`)
  so fork PRs cannot fail CI in `Swatinem/rust-cache` post-job save steps.

---

## File Reference

> **Full file tables -> [context-file-reference.md](context-file-reference.md)**

Key files at a glance: `src/main.rs` (entry), `src/server.rs` (room/player logic),
`src/websocket/` (WS lifecycle), `src/protocol/` (messages and types),
`src/config/` (all config structs), `src/auth/` (auth and rate limiting).

Protocol v3 routing invariant: `websocket::create_router()` is nest-safe and
must not expose `/v3/ws` by itself; production mounts it under `/v2` and adds
top-level `/v3/ws` separately. Standalone/library servers that serve Signal Fish
at the HTTP root should use `websocket::create_standalone_router()` when they
want both `/ws` and `/v3/ws`.

Signaling rate limits are split intentionally: `max_signals` counts valid
deliverable WebRTC relays, while `max_signal_errors` counts rejected `Signal`
attempts. Do not move target/transport validation in a way that lets invalid
traffic avoid `max_signal_errors` or consume the valid ICE budget.

---

## Protocol Quick Reference

### v2 Client Messages (JSON/MessagePack)

Canonical sample: [v2-client-messages.jsonl](code-samples/protocol/v2-client-messages.jsonl)

### v2 Server Messages

Canonical sample: [v2-server-messages.jsonl](code-samples/protocol/v2-server-messages.jsonl)

---

## Common Scenarios

### Adding a New Protocol Message

1. Define in `src/protocol/messages.rs` -> handler in `src/server.rs`
   or `src/server/` submodule -> serialization tests -> e2e tests
2. Run `/add-protocol-message` for full checklist

### Adding a Configuration Option

1. Add the field to the appropriate struct in `src/config/`
2. Add a default value in `src/config/defaults.rs`
3. Add validation in `src/config/validation.rs` if needed
4. Update `config.example.json` with the new option and a comment
5. Add tests for default value, custom value, and invalid value cases

### Performance Debugging

```bash
RUST_LOG=signal_fish_server=trace cargo run   # Trace logging
cargo bench                                    # Benchmarks
```

### Commit Format: `<type>: <imperative subject>` (feat|fix|perf|test|docs|refactor|chore)

> **Full workflow/checklist -> [skills/mandatory-workflow.md](skills/mandatory-workflow.md)**

---

## Skills Library

The canonical skill list is generated in [skills/index.md](skills/index.md).
Do not maintain a duplicate generated list in this file.

---

## Resources

[Matchbox](https://github.com/johanhelsing/matchbox) | [Tokio](https://tokio.rs/) | [Axum](https://docs.rs/axum/latest/axum/)
