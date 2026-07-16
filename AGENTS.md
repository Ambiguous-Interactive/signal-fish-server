# Signal Fish Server Agent Guidelines

## Project identity

- Treat this repository as Ambiguous Interactive's lightweight, in-memory WebSocket signaling
  server for peer-to-peer game networking.
- Use "Signal Fish Server" or "the signaling server" in documentation. "Signal Fish" is acceptable informally.
- Do not call the product "Matchbox Signaling Server." Matchbox is an upstream dependency, not this product or its team.
- Use "Ambiguous Interactive" for authorship, copyright, and user-facing branding.
- **Version:** 0.4.0
- Preserve the single self-contained binary and zero external runtime dependencies unless the
  task explicitly changes those constraints.

## Repository skills

Task workflows live in discoverable [`.agents/skills/`](.agents/skills/) packages. Invoke the
narrowest matching skill explicitly when useful; Codex can also select skills from frontmatter.

- Architecture and code location: `$signal-fish-architecture`
- Rust implementation and refactoring: `$rust-development`
- Test design and mutation testing: `$testing-rust`
- Protocol, WebSocket, room, or session behavior: `$websocket-protocol`
- Authentication, abuse controls, or security: `$web-service-security`
- CI and GitHub Actions failures: `$ci-troubleshooting`
- Dependencies and supply chain: `$dependency-supply-chain`
- Rust MSRV and toolchains: `$toolchain-management`
- Containers, deployment, resilience, or observability: `$deployment-operations`
- Documentation and changelog work: `$documentation-quality`
- Shell and AWK automation: `$shell-scripting`
- Git and hook workflows: `$version-control-workflow`
- Repository policy, validation, or skill maintenance: `$repository-maintenance`
- Review and pre-handoff verification: `$agent-quality`

Read only the references routed by the selected `SKILL.md`; do not load the entire reference library by default.

Canonical protocol fixtures: [v2 client messages](.agents/skills/websocket-protocol/references/v2-client-messages.jsonl)
and [v2 server messages](.agents/skills/websocket-protocol/references/v2-server-messages.jsonl).

## Required engineering workflow

- Work from a reproducible failing case or explicit structural baseline when practical.
- Add comprehensive tests for every behavior change: happy path, negative and error paths,
  boundaries, and cleanup or concurrency where relevant.
- Treat every test failure as a bug to explain and fix. Do not dismiss failures as flaky.
- Keep production Rust warning-free and avoid panic-prone paths.
- For Rust changes, run in order:

  ```bash
  cargo fmt
  cargo clippy --all-targets --all-features
  cargo test --all-features
  ```

## GitHub access

- Use local `git` for branches, commits, and pushes.
- Use the connected VS Code GitHub extension or GitHub app for pull requests, comments,
  reviewers, review state, and other supported GitHub operations.
- Fall back to authenticated `gh` only when the extension or app lacks a required capability,
  such as detailed Actions logs or GraphQL review threads.
- Do not let an unauthenticated `gh` session block delivery when the GitHub app and Git remote are available.

## Hook reliability

- Keep hooks cross-platform (`pwsh` and `git` only), deterministic, and sub-second. Do not add
  `cargo`, `npm`, `npx`, installers, or bootstrap commands to hooks.
- Under PowerShell strict mode, wrap function output with `@(...)` before `.Count`; preserve
  arrays with `Write-Output -NoEnumerate`, unary array returns, or typed array boundaries.
- After hook or hook-policy changes, run:

  ```bash
  pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1
  SIGNAL_FISH_HOOK_PROFILE=1 pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-commit.ps1 -Worktree
  pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-push.ps1 -Worktree
  ```

## Tooling parity

- Keep `YQ_VERSION` and `TAPLO_CLI_VERSION` synchronized between `.github/workflows/doc-validation.yml` and `.devcontainer/Dockerfile`.
- Keep Docker CLI support enabled in `.devcontainer/devcontainer.json`, with
  `docker-outside-of-docker` configured as `"moby": false` unless a tested replacement is introduced.
- Keep heavy Cargo tool bootstrap on the `cargo-binstall` path in `.devcontainer/Dockerfile`.
- Keep `.devcontainer/post-create.sh` Cargo warm-up opt-in through `SIGNAL_FISH_WARM_CARGO_CHECK`.
- Run `bash scripts/check-tooling-parity.sh` after tooling changes.
