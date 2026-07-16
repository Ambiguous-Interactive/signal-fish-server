# Signal Fish Project Context

Use this cross-domain reference when a task spans multiple repository areas. Follow `AGENTS.md` for current always-on policy and select focused skills for procedural detail.

## Product identity

- **Company:** Ambiguous Interactive
- **Product:** Signal Fish Server, a lightweight in-memory WebSocket signaling server for peer-to-peer game networking
- **Crate:** `signal-fish-server` binary and `signal_fish_server` library
- **Version:** 0.4.0
- **Runtime shape:** one self-contained binary with zero external runtime dependencies
- **Upstream relationship:** Matchbox is a dependency and historical source, not this product or its team

Use “Signal Fish Server” or “the signaling server” in documentation and “Ambiguous Interactive” in authorship and branding.

## Task routing

| Change area | Skill | Primary reference |
| --- | --- | --- |
| Architecture, files, room, or player state | `$signal-fish-architecture` | [Architecture and files](architecture-and-files.md) |
| Rust implementation, API, errors, or performance | `$rust-development` | [Project coding and design](../../rust-development/references/project-coding-and-design.md) |
| Tests, fixtures, coverage, or mutation testing | `$testing-rust` | [Project testing](../../testing-rust/references/project-testing.md) |
| Wire messages, WebSocket lifecycle, or sessions | `$websocket-protocol` | [Protocol and scenarios](../../websocket-protocol/references/protocol-and-scenarios.md) |
| Authentication, session security, or abuse | `$web-service-security` | [Authentication and input validation](../../web-service-security/references/web-service-security-auth.md) |
| CI or GitHub Actions | `$ci-troubleshooting` | [Troubleshooting index](../../ci-troubleshooting/references/ci-cd-troubleshooting-index.md) |
| Dependencies, audit, or SBOM | `$dependency-supply-chain` | [Supply-chain policy](../../dependency-supply-chain/references/supply-chain-audit-policy.md) |
| MSRV or Rust pins | `$toolchain-management` | [MSRV management](../../toolchain-management/references/msrv-management.md) |
| Containers, deployment, resilience, or telemetry | `$deployment-operations` | [Deployment strategies](../../deployment-operations/references/deployment-strategies.md) |
| Documentation, links, or changelog | `$documentation-quality` | [Documentation standards](../../documentation-quality/references/documentation-standards.md) |
| Git or hooks | `$version-control-workflow` | [Git hook checks](../../version-control-workflow/references/git-hooks-checks.md) |
| Repository policy or skills | `$repository-maintenance` | [Mandatory workflow](../../repository-maintenance/references/mandatory-workflow.md) |

## Protocol samples

Treat these JSONL resources as executable documentation and compatibility fixtures:

- [v2 client messages](../../websocket-protocol/references/v2-client-messages.jsonl)
- [v2 server messages](../../websocket-protocol/references/v2-server-messages.jsonl)
- [v3 client messages](../../websocket-protocol/references/v3-client-messages.jsonl)
- [v3 server messages](../../websocket-protocol/references/v3-server-messages.jsonl)

## Skill catalog

Codex discovers packages directly from `.agents/skills/*/SKILL.md`. The generated [Agent Skills Index](../../index.md) exists only for human navigation; regenerate it with `./scripts/generate-skills-index.sh` after skill metadata changes.
