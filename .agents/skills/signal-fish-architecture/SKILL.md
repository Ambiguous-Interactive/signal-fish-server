---
name: signal-fish-architecture
description: Navigate Signal Fish Server architecture, repository layout, product identity, and architectural invariants. Use when locating where a change belongs, understanding rooms, players, connections, authentication, protocol ownership, crate structure, file responsibilities, or project-specific naming and branding.
---

<!-- markdownlint-disable MD013 -->

# Signal Fish Architecture

Treat Signal Fish Server as Ambiguous Interactive's lightweight in-memory WebSocket signaling server, not as Matchbox Signaling Server. The upstream Matchbox project is a dependency, not this product's identity.

## Workflow

1. Read [architecture-and-files.md](references/architecture-and-files.md) before cross-cutting or server-state changes.
2. Read [file-reference.md](references/file-reference.md) when locating configuration, workflows, tests, documentation, or ownership boundaries.
3. Read [project-context.md](references/project-context.md) for the consolidated product identity and cross-domain decision map; prefer `AGENTS.md` for current always-on procedure.
4. Invoke `$websocket-protocol`, `$rust-development`, `$testing-rust`, or `$web-service-security` for the task-specific workflow.
5. Preserve the single self-contained binary and zero external runtime dependencies unless the user explicitly changes that product constraint.
