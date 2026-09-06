# Ambiguous Interactive — LLM Context (Signal Fish)

> **Central context file for all AI coding assistants**
> Goal: Extremely fast, safe Rust code | High test coverage | Zero external runtime dependencies

## Project Identity

- **Company:** Ambiguous Interactive
- **Product:** A lightweight, in-memory WebSocket signaling server for peer-to-peer game networking
- **Repository:** `signal-fish-server` — extracted from the matchbox-signaling-server with
  production-ready signaling stripped down to a single self-contained binary
- **Crate name:** Binary: `signal-fish-server` | Library: `signal_fish_server`
- **Version:** 0.8.0
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
- Select skills by their catalog descriptions, then read the chosen `SKILL.md` completely.
- Regenerate after skill changes:
  `python3 .llm/skills/manage-skills/scripts/generate_skills_index.py`

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
    |                                  skills/web-service-security/SKILL.md
    |                                  skills/websocket-session-security/SKILL.md
    +-- Deployment/Containers? ------> skills/containers/SKILL.md
    +-- CI/CD/GitHub Actions? -------> skills/github-actions-workflow-config/SKILL.md
    |                                  skills/ci-cd-troubleshooting/SKILL.md
    +-- Mutation testing slow/timeout? -> skills/mutation-testing-performance/SKILL.md
    +-- Dependencies/Supply Chain? --> skills/supply-chain-security/SKILL.md
    |                                  skills/dependency-management/SKILL.md
    |                                  skills/msrv-management/SKILL.md
    +-- Performance Issue? ----------> skills/rust-performance-optimization/SKILL.md
    +-- Hosting/Provider/Scaling? ---> skills/graceful-degradation/SKILL.md
```

### Should I Add a Test?

```text
YES - ALWAYS. Every change requires comprehensive tests.
  +-- Happy path + positive variations
  +-- Negative cases + error conditions
  +-- Edge cases (empty, null, max, unicode, concurrent)
  +-- Error recovery (cleanup, partial states)

CRITICAL: Any test failure = bug to fix. No "flaky" tests.
-> See skills/testing/SKILL.md for full methodology.
```

---

## Mandatory Workflow (Every Change)

See [Mandatory Workflow and Checklists](skills/mandatory-workflow/SKILL.md) for full details.

Session notes under `progress/` are local planning artifacts. Keep them
gitignored and never force-add or commit them; durable decisions belong in the
relevant source, test, documentation, issue, or pull request.

```bash
# Rust changes (ALWAYS run in order)
cargo fmt && cargo clippy --all-targets --all-features && cargo test --all-features
```

**Zero warnings policy** -- all linters enforce strict compliance.

### Local vs hosted-CI work split (Required)

- Locally, agents run `cargo fmt` and `cargo clippy --all-targets
  --all-features`, plus narrowly targeted red-green checks only:
  `cargo nextest run -E 'test(<name>)'` filtered to the changed seams.
- Do NOT run expensive suites locally: full `cargo test`/nextest sweeps,
  `cargo doc`, `cargo deny`, mutation testing, or whole integration binaries.
  Those belong to hosted CI; replicate them only when RCA-ing an actual red
  hosted check, and then prefer the narrowest reproduction that shows it.

### GitHub Access (Required for All Agents)

- This policy applies to every repository agent entrypoint (Codex, Claude, and
  GitHub Copilot).
- GitHub authentication comes from the connected VS Code GitHub
  connector/extension session: `.vscode/mcp.json` signs the pinned
  `github-mcp-server` with `GITHUB_PERSONAL_ACCESS_TOKEN` injected through
  `remoteEnv`, so harnesses running under the connector operate authenticated.
  When a GitHub MCP tool call surfaces the server's device-authorization
  fallback instead, retry the call through the connector's session rather than
  treating the prompt as a blocker or downgrading to unauthenticated REST
  reads for write operations.
- Use local `git` for branch management, staging, commits, and pushes.
- Use the connected VS Code GitHub extension / GitHub app for pull-request
  creation, metadata, comments, reviewer requests, review inspection, and every
  other GitHub operation it supports.
- Do not block repository delivery solely because `gh auth status` is
  unauthenticated when the connected GitHub extension/app is available and the
  Git remote can push successfully.
- Fall back to `gh` only for a required capability the extension/app cannot
  supply (notably detailed GitHub Actions logs or GraphQL review-thread
  operations), and only when an authenticated CLI session is available.

### Hook Reliability Rules (Required)

- Git hooks are last-resort guards and must stay cross-platform (`pwsh` + `git` only) and sub-second.
- If you touch hook files or hook-adjacent policy code, run:
  - `pwsh -NoLogo -NoProfile -NonInteractive -File scripts/check-hook-readiness.ps1`
  - `SIGNAL_FISH_HOOK_PROFILE=1 pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-commit.ps1 -Worktree`
  - `pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-push.ps1 -Worktree`
- Runtime target: full pre-commit suite under 1000ms on normal local workloads.
  Any slower run must be investigated and optimized before handoff.
- PowerShell collection safety (strict mode):
  - Always wrap function outputs with `@(...)` before using `.Count`.
  - Example: `if ((Get-Items).Count -eq 0) {}` can fail in strict mode when output
    collapses to a scalar; `if (@(Get-Items).Count -eq 0) {}` is safe.
  - When returning a collection that must stay a collection, use `Write-Output -NoEnumerate` or a unary array return.
  - Prefer typed array boundaries (`[string[]]@(...)`, `[int[]]@(...)`) for helper function inputs.
- Do not add `cargo`, `npm`, `npx`, or installer/bootstrap commands to git hooks.

### Tooling Parity Rules (Required)

- Keep CI and devcontainer tool pins synchronized.
- If you change `YQ_VERSION` or `TAPLO_CLI_VERSION` in
  `.github/workflows/doc-validation.yml`, update matching ARG values in
  `.devcontainer/Dockerfile` in the same PR.
- Keep Docker CLI support enabled in `.devcontainer/devcontainer.json` for local
  Docker CI parity.
- Keep `docker-outside-of-docker` configured with `"moby": false` in
  `.devcontainer/devcontainer.json` unless a tested replacement is introduced.
- Keep heavy cargo tool bootstrap in `.devcontainer/Dockerfile` on the
  `cargo-binstall` path (`cargo install cargo-binstall` + `cargo binstall`) to
  avoid slow no-cache rebuild regressions.
- Keep `.devcontainer/post-create.sh` cargo warm-up opt-in via
  `SIGNAL_FISH_WARM_CARGO_CHECK`; default path should stay fast.
- Keep the user-owned npm prefix (`NPM_CONFIG_PREFIX=/home/vscode/.npm-global`)
  RUNTIME-ONLY: set in `.devcontainer/devcontainer.json` `containerEnv` and
  re-applied by `configure_user_npm_prefix` — never via Dockerfile `ENV`.
  Devcontainer features install in layers appended after the Dockerfile, and
  the nvm-based Node feature aborts its install when `NPM_CONFIG_PREFIX` (or
  `PREFIX`) is already set, failing every image build.
  `npm install -g` must never require sudo. The root and browser-client
  `node_modules` named volumes must also remain owned by the remote user so a
  plain local `npm install` never requires sudo.
- Keep `.github/workflows/devcontainer-build.yml` building the devcontainer
  image on `.devcontainer/**` changes and on a monthly schedule: feature
  installs only fail at image-build time, so this workflow is the only check
  that catches devcontainer-only breakage and upstream base-image/feature
  drift.
- Keep the terminal agent CLIs (OpenAI Codex, OpenCode, Nanocoder) and the
  Z.AI Vision MCP package (`@z_ai/mcp-server@latest`) installed and refreshed
  through `.devcontainer/lib-agent-tools.sh` (sourced by
  `post-create.sh` on create and `post-start.sh` on every launch) and through
  the user-owned npm prefix (`NPM_CONFIG_PREFIX=/home/vscode/.npm-global`) —
  `npm install -g` must never require sudo.
- Keep the launch-path CLI refresh behind the registry version-check fast
  path (one bulk `npm outdated -g --json` plus local installed state;
  `npm view` only for a missing tool, offline-safe, skip when current) — never
  reintroduce an unconditional `npm install -g <pkg>@latest` on container
  start. Keep `waitFor: updateContentCommand` so network refreshes remain
  behind the editor attach point. Every lifecycle step stays best-effort
  (`return`, never `exit`) so optional setup cannot prevent a usable attach.
- Keep npm 11 lifecycle scripts explicitly scoped to the selected package via
  `--allow-scripts="$pkg"`; OpenCode's platform-binary selection depends on its
  postinstall, while a broad script allowlist needlessly expands trust.
- Keep the pinned GitHub MCP server (`GITHUB_MCP_VERSION` in
  `.devcontainer/Dockerfile`) wired into every harness:
  `.vscode/mcp.json` (VS Code + Copilot), `.mcp.json` (Claude Code +
  Nanocoder), `opencode.json` (OpenCode), and `~/.codex/config.toml`
  (written idempotently for Codex). It authenticates via
  `GITHUB_PERSONAL_ACCESS_TOKEN` passed through `remoteEnv`;
  `opencode.json` must additionally forward the token through its
  `environment` map (`{env:GITHUB_PERSONAL_ACCESS_TOKEN}`) — observed in
  #496, an opencode-launched MCP server device-flowed even with the token
  present in the container shell, so shell-environment inheritance must
  not be relied on. The shared
  `.mcp.json` must keep BOTH keys — `type` (Claude Code) and `transport`
  (Nanocoder) — or one harness silently loses GitHub.
- Keep the official Z.AI MCP suite wired through `.devcontainer/zai-mcp.mjs`
  in all four harness configs. It reads `.env.local` at MCP startup, preferring
  file keys over inherited `Z_AI_API_KEY`; restart MCP servers after edits.
  Vision uses the preinstalled binary; HTTP servers use its bundled MCP SDK.
  `configure-zai-mcp.py` migrates only repository-owned Codex blocks.
  Keep keys out of config and Docker builds. Empty `.env.local` is valid for
  credential-free fresh clones and CI. Diagnose the complete startup with
  `python3 scripts/check-zai-mcp.py --live`.
- Keep `.github/workflows/devcontainer-build.yml` running
  `.devcontainer/verify-agent-tooling-runtime.sh` after the real post-start
  path. It proves a local `npm install --global` succeeds as `vscode` without
  sudo, verifies both project `node_modules` volumes are writable, and checks
  all four npm-delivered agent tools are on PATH.
- Any change to the agent-tooling contract (npm prefix, CLI refresh, MCP
  wiring) must update `tests/agent_tooling_guards.rs`,
  `scripts/check-tooling-parity.sh`, and the
  `devcontainer-agent-tooling` skill together in the same commit.
- Run `bash scripts/check-tooling-parity.sh` after tooling changes.

---

## Core Reference Map

Detailed guidance was moved out of this core file. Use these companion references:

- [Software Design and Coding Standards](context-coding-and-design.md)
- [Testing Requirements Reference](context-testing.md)
- [Documentation and CI Pitfalls](context-docs-and-ci-pitfalls.md)
- [Architecture and File Reference](context-architecture-and-files.md)
- [Protocol and Common Scenarios](context-protocol-and-scenarios.md)

Also see:

- [Detailed Context File Reference](context-file-reference.md)
- [Config and Wire-Format Drift](config-wire-format-drift.md)

Canonical protocol samples:

- [v2 client messages](code-samples/protocol/v2-client-messages.jsonl)
- [v2 server messages](code-samples/protocol/v2-server-messages.jsonl)
- [v3 client messages](code-samples/protocol/v3-client-messages.jsonl)
- [v3 server messages](code-samples/protocol/v3-server-messages.jsonl)

---

## Skills Library

The canonical skill list is generated in [skills/index.md](skills/index.md).
Do not maintain a duplicate generated list in this file.
