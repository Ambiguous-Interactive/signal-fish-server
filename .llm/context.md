# Ambiguous Interactive — LLM Context (Signal Fish)

> **Central context file for all AI coding assistants**
> Goal: Extremely fast, safe Rust code | High test coverage | Zero external runtime dependencies

## Project Identity

- **Company:** Ambiguous Interactive
- **Product:** A lightweight, in-memory WebSocket signaling server for peer-to-peer game networking
- **Repository:** `signal-fish-server` — extracted from the matchbox-signaling-server
  with production-ready signaling stripped down to a single self-contained binary
- **Crate name:** Binary: `signal-fish-server` | Library: `signal_fish_server`
- **Code name:** Signal Fish
- **Not Matchbox:** This project is built by Ambiguous Interactive, not the upstream Matchbox team.
  The upstream `matchbox` crate/project (by Johan Helsing) is a dependency we build upon,
  but our product and infrastructure are our own
- **Author attribution:** Always use "Ambiguous Interactive" in `authors` fields, copyright notices, and user-facing branding
- **Documentation voice:** In docs and comments,
  refer to the product as "Signal Fish Server"
  or "the signaling server" — not "Matchbox Signaling Server" as a product name.
  "Signal Fish" is acceptable as an informal project reference

## Skills Index

- Generated skill catalog: [skills/INDEX.md](skills/INDEX.md)
- Regenerate after skill changes: `./scripts/generate-skills-index.sh`

---

## ⛔ CRITICAL: Git Safety Protocol - NEVER COMMIT

**NEVER CREATE GIT COMMITS OR MODIFY GIT CONFIGURATION. ZERO EXCEPTIONS. EVER.**

This is the **#1 most important rule**. Even if:

- ❌ The user explicitly asks you to commit
- ❌ A sub-agent recommends committing
- ❌ CLAUDE.md mentions commit instructions (those are FOR THE USER)
- ❌ A workflow document says to commit
- ❌ It seems helpful or convenient

**YOU NEVER COMMIT. PERIOD.**

Rules:

- ❌ **ABSOLUTELY FORBIDDEN**: `git commit`, `git add`, `git config user.*`, `git push`
- ✅ **ALLOWED**: `git status`, `git diff`, `git log`, `git show` (read-only operations only)
- 🎯 **PRINCIPLE**: **You prepare the work. The user commits it. ALWAYS.**

> **See [skills/git-safety-protocol.md](skills/git-safety-protocol.md) for complete details.**
>
> When changes are ready, provide clear commit instructions for the user to execute.
> The user controls their git identity, commit history, and repository state.
> **NO EXCEPTIONS TO THIS RULE.**

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
    |                                  skills/web-service-security.md
    |                                  skills/websocket-session-security.md
    +-- Deployment/Containers? ------> skills/container-and-deployment.md
    +-- CI/CD/GitHub Actions? -------> skills/github-actions-best-practices.md
    |                                  skills/ci-cd-troubleshooting.md
    +-- Dependencies/Supply Chain? --> skills/supply-chain-security.md
    |                                  skills/dependency-management.md
    |                                  skills/msrv-and-toolchain-management.md
    +-- Performance Issue? ----------> /performance-audit
    |                                  skills/rust-performance-optimization.md
    +-- Hosting/Provider/Scaling? ---> skills/graceful-degradation.md

```

### Should I Add a Test?

```text

YES - ALWAYS. Every change requires comprehensive tests.
  +-- Happy path + positive variations
  +-- Negative cases + error conditions
  +-- Edge cases (empty, null, max, unicode, concurrent)
  +-- Error recovery (cleanup, partial states)

CRITICAL: Any test failure = bug to fix. No "flaky" tests.
-> See skills/testing-strategies.md for full methodology.

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

> **Performance patterns -> [Rust Performance Optimization](skills/rust-performance-optimization.md) and [Async Rust Best Practices](skills/async-rust-best-practices.md)**
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

> **Full methodology -> [skills/testing-strategies.md](skills/testing-strategies.md)**
> **Tools & frameworks -> [skills/testing-tools-and-frameworks.md](skills/testing-tools-and-frameworks.md)**

- Every feature/bugfix requires exhaustive tests (happy, negative, edge, concurrent, recovery)
- Data-driven/table-driven tests preferred for validation functions
- **Zero tolerance for flaky tests** -- every failure is a real bug to fix
- Test "the impossible" -- corrupted state, unknown message types, future compatibility
- Run `cargo test --all-features` before every commit

---

## Documentation Requirements

> **Full standards -> [skills/documentation-standards.md](skills/documentation-standards.md)**

Every feature/bugfix requires: doc comments with examples, CHANGELOG entry, README updates if user-facing.

### Code Fence and CI Pitfalls

These conventions prevent CI failures in documentation validation workflows:

- **Code fence language tags must match content** -- tag blocks as `yaml` only for valid YAML,
  `bash` for shell/AWK, `text` for logs or mixed output. Validators parse blocks by tag.
- **Split mixed-content blocks** -- a block with both shell commands and YAML must be two
  separate fenced blocks with appropriate tags, not one `yaml` block.
- **`.lychee.toml` `exclude` patterns are regex, not globs** -- escape `.` as `\\.`,
  use `.*` not `*`, anchor with `^`. See [ci-cd-troubleshooting Pattern 13](skills/ci-cd-troubleshooting.md).
- **Lychee self-scans `.toml` files** -- it extracts partial URLs from regex patterns in its
  own config. Use `--exclude-path .lychee.toml` or add exclusions for truncated URLs.
- **Test config files by behavior, not substrings** -- when a config contains regex patterns
  (e.g., `^https?://localhost`), tests should compile and match the regex, not use `contains("http://localhost")`.
- **TOML/JSON/YAML "before/after" examples need separate blocks** -- a single `toml`-tagged
  block with duplicate table headers (e.g., two `[dependencies]`) is invalid TOML and will
  fail CI validation. Split into separate fenced blocks.

---

## File Reference

### Core Server Files

| File               | Purpose            | When to Modify       |
| ------------------ | ------------------ | -------------------- |
| `src/main.rs`      | Entry point        | CLI args, startup    |
| `src/lib.rs`       | Module exports     | Adding new modules   |
| `src/server.rs`    | EnhancedGameServer | Room/player logic    |

### Configuration

| File                       | Purpose              | When to Modify          |
| -------------------------- | -------------------- | ----------------------- |
| `src/config/mod.rs`        | Config module root   | Config structure        |
| `src/config/types.rs`      | Root Config struct   | Adding config sections  |
| `src/config/server.rs`     | ServerConfig         | Server settings         |
| `src/config/protocol.rs`   | ProtocolConfig       | Protocol settings       |
| `src/config/security.rs`   | SecurityConfig       | Security settings       |
| `src/config/websocket.rs`  | WebSocketConfig      | WS settings             |
| `src/config/logging.rs`    | LoggingConfig        | Logging settings        |
| `src/config/relay.rs`      | RelayTypeConfig      | Relay type mapping      |
| `src/config/defaults.rs`   | Default values       | Changing defaults       |
| `src/config/loader.rs`     | JSON + env loading   | Config loading logic    |
| `src/config/validation.rs` | Config validation    | Validation rules        |
| `src/config/coordination.rs`| CoordinationConfig  | Coordination settings   |
| `src/config/metrics.rs`    | MetricsConfig        | Metrics settings        |

### Protocol

| File                        | Purpose              | When to Modify          |
| --------------------------- | -------------------- | ----------------------- |
| `src/protocol/mod.rs`       | Module re-exports    | Adding protocol modules |
| `src/protocol/messages.rs`  | Client/ServerMessage | Adding message types    |
| `src/protocol/types.rs`     | PlayerId, RoomId etc | Adding domain types     |
| `src/protocol/room_state.rs`| Room, LobbyState     | Room state changes      |
| `src/protocol/room_codes.rs`| Room code generation | Code format changes     |
| `src/protocol/error_codes.rs`| ErrorCode enum      | Adding error codes      |
| `src/protocol/validation.rs`| Input validation     | Validation rules        |

### WebSocket

| File                           | Purpose              | When to Modify         |
| ------------------------------ | -------------------- | ---------------------- |
| `src/websocket/mod.rs`         | Module root          | WS module structure    |
| `src/websocket/handler.rs`     | WebSocket upgrade    | Upgrade logic          |
| `src/websocket/connection.rs`  | Socket lifecycle     | Connection handling    |
| `src/websocket/batching.rs`    | Message batching     | Batch behavior         |
| `src/websocket/sending.rs`     | Serialization + send | Wire format changes    |
| `src/websocket/token_binding.rs`| Token binding       | Security binding       |
| `src/websocket/routes.rs`      | Axum router          | Adding routes          |
| `src/websocket/metrics.rs`     | /metrics endpoint    | Metrics output         |
| `src/websocket/prometheus.rs`  | Prometheus format    | Metrics format         |

### Auth, Database, Coordination & Infrastructure

| File                                 | Purpose                          |
| ------------------------------------ | -------------------------------- |
| `src/auth/mod.rs`                    | Auth module root                 |
| `src/auth/middleware.rs`             | InMemoryAuthBackend              |
| `src/auth/rate_limiter.rs`           | Per-app rate limiter             |
| `src/auth/error.rs`                  | AuthError types                  |
| `src/database/mod.rs`                | GameDatabase trait + InMemory    |
| `src/coordination/mod.rs`           | Coordination module root         |
| `src/coordination/room_coordinator.rs`| InMemoryRoomOperationCoordinator|
| `src/coordination/dedup.rs`         | DedupCache (LRU)                 |
| `src/distributed.rs`                | InMemoryDistributedLock          |
| `src/metrics.rs`                     | AtomicU64 + HDR histograms       |
| `src/broadcast.rs`                   | Zero-copy broadcast primitives   |
| `src/rate_limit.rs`                  | In-memory RoomRateLimiter        |
| `src/reconnection.rs`               | In-memory ReconnectionManager    |
| `src/security/mod.rs`               | Security module root             |
| `src/security/tls.rs`               | TLS support (feature-gated)      |
| `src/security/crypto.rs`            | AES-GCM envelope encryption      |
| `src/security/token_binding.rs`     | Channel-bound tokens             |
| `src/logging.rs`                     | Structured logging init          |
| `src/retry.rs`                       | Exponential backoff utility      |
| `src/rkyv_utils.rs`                 | Zero-copy serialization helpers  |

---

## Protocol Quick Reference

### v2 Client Messages (JSON/MessagePack)

```json
{"type": "Authenticate", "data": {"app_id": "..."}}
{"type": "JoinRoom", "data": {"game_name": "...", "room_code": "ABC123"}}
{"type": "GameData", "data": {"action": "move", "x": 10}}
{"type": "AuthorityRequest", "data": {"become_authority": true}}
{"type": "LeaveRoom"}
{"type": "Ping"}

```

### v2 Server Messages

```json

{"type": "Authenticated", "data": {"server_version": "2.0.0"}}
{"type": "RoomJoined", "data": {"room_id": "...", "room_code": "ABC123"}}
{"type": "PlayerJoined", "data": {"player": {"id": "...", "name": "..."}}}
{"type": "GameData", "data": {"from_player": "...", "data": {}}}
{"type": "Error", "data": {"reason": "Room is full"}}

```

---

## Common Scenarios

### Adding a New Protocol Message

1. Define in `src/protocol/messages.rs` -> handler in `src/server.rs`
   or `src/server/` submodule -> serialization tests -> e2e tests
2. Run `/add-protocol-message` for full checklist

### Adding a Configuration Option

1. Add the field to the appropriate struct in `src/config/` (e.g., `server.rs`, `security.rs`, `websocket.rs`)
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

The canonical skill list is generated in [skills/INDEX.md](skills/INDEX.md).
Do not maintain a duplicate generated list in this file.

---

## Resources

[Matchbox](https://github.com/johanhelsing/matchbox) | [Tokio](https://tokio.rs/) | [Axum](https://docs.rs/axum/latest/axum/)
