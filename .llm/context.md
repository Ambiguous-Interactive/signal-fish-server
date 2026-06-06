# Ambiguous Interactive — LLM Context (Signal Fish)

> **Central context file for all AI coding assistants**
> Goal: Extremely fast, safe Rust code | High test coverage | Zero external runtime dependencies

## Project Identity

- **Company:** Ambiguous Interactive
- **Product:** A lightweight, in-memory WebSocket signaling server for peer-to-peer game networking
- **Repository:** `signal-fish-server` — extracted from the matchbox-signaling-server with
  production-ready signaling stripped down to a single self-contained binary
- **Crate name:** Binary: `signal-fish-server` | Library: `signal_fish_server`
- **Version:** 0.2.0
- **Code name:** Signal Fish
- **Not Matchbox:** This project is built by Ambiguous Interactive, not the upstream Matchbox team.
  The upstream `matchbox` crate/project (by Johan Helsing) is a dependency we build upon,
  but our product and infrastructure are our own
- **Author attribution:** Always use "Ambiguous Interactive" in `authors` fields, copyright notices,
  and user-facing branding
- **Documentation voice:** Refer to the product as "Signal Fish Server" or "the signaling server" —
  not "Matchbox Signaling Server". "Signal Fish" is acceptable as an informal project reference.

## Skills Index

- Generated skill catalog: [skills/index.md](skills/index.md)
- Regenerate after skill changes: `./scripts/generate-skills-index.sh`

---

## CRITICAL: Git Safety Protocol - NEVER COMMIT

**NEVER CREATE GIT COMMITS OR MODIFY GIT CONFIGURATION. ZERO EXCEPTIONS. EVER.**

This is the #1 policy. You prepare the work; the user commits it.

**Full details:** [Git Safety Protocol](context-git-safety.md)

---

## Quick Decision Trees

### What Am I Changing?

```text
Start here:
    |
  +-- Protocol/Messages? ----------> See context-protocol-and-scenarios.md
    +-- WebSocket/Connection? -------> src/websocket/, tests/e2e_tests.rs
    +-- Room/Player Logic? ----------> src/server.rs, src/server/, tests/integration_tests.rs
  |                                  context-architecture-and-files.md (Architectural Invariants)
    +-- Security/Auth/Sessions? -----> src/auth/, src/security/
    |                                  skills/web-service-security-auth.md
    |                                  skills/websocket-session-hijacking.md
    +-- Deployment/Containers? ------> skills/container-docker.md
    +-- CI/CD/GitHub Actions? -------> skills/github-actions-workflow-config.md
    |                                  skills/ci-cd-troubleshooting-index.md
    +-- Dependencies/Supply Chain? --> skills/supply-chain-audit-policy.md
    |                                  skills/dependency-management-cargo.md
    |                                  skills/msrv-management.md
    +-- Performance Issue? ----------> skills/rust-performance-optimization.md
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

## Mandatory Workflow (Every Change)

See [Mandatory Workflow and Checklists](skills/mandatory-workflow.md) for full details.

```bash
# Rust changes (ALWAYS run in order)
cargo fmt && cargo clippy --all-targets --all-features && cargo test --all-features
```

**Zero warnings policy** -- all linters enforce strict compliance.

---

## Core Reference Map

Detailed guidance was moved out of this core file. Use these companion references:

- [Software Design and Coding Standards](context-coding-and-design.md)
- [Testing Requirements Reference](context-testing.md)
- [Documentation and CI Pitfalls](context-docs-and-ci-pitfalls.md)
- [Architecture and File Reference](context-architecture-and-files.md)
- [Protocol and Common Scenarios](context-protocol-and-scenarios.md)
- [Git Safety Protocol](context-git-safety.md)

Also see:

- [Detailed Context File Reference](context-file-reference.md)
- [Config and Wire-Format Drift](config-wire-format-drift.md)

Canonical protocol samples:

- [v2 client messages](code-samples/protocol/v2-client-messages.jsonl)
- [v2 server messages](code-samples/protocol/v2-server-messages.jsonl)

---

## Skills Library

The canonical skill list is generated in [skills/index.md](skills/index.md).
Do not maintain a duplicate generated list in this file.
