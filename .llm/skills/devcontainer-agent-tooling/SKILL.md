---
name: devcontainer-agent-tooling
description: >-
  Maintain the devcontainer agent-tooling contract: sudo-free npm, fast
  latest-version CLI refresh (Codex, OpenCode, Nanocoder), and GitHub MCP
  wiring for every agent harness. Use when touching .devcontainer/, agent CLI
  installation, or any MCP config file.
---

# Devcontainer Agent Tooling

---

## When to Use

- Editing `.devcontainer/` (Dockerfile, devcontainer.json, post-create.sh,
  post-start.sh, lib-agent-tools.sh)
- Adding, removing, or reconfiguring an agent harness (Codex, Claude Code,
  Copilot, VS Code, OpenCode, Nanocoder) or its MCP servers
- Touching npm global installs, the npm prefix, or the GitHub MCP server pin
- Diagnosing "devcontainer opens slowly" or "npm install needs sudo"

---

## The Contract (guaranteed on every build/rebuild/launch)

1. **Sudo-free npm**: global installs route through the user-owned prefix
   `NPM_CONFIG_PREFIX=/home/vscode/.npm-global`. The prefix is RUNTIME-ONLY:
   `containerEnv` in `.devcontainer/devcontainer.json` sets it, and
   `configure_user_npm_prefix` re-applies it to `~/.npmrc` + PATH. NEVER export
   it via Dockerfile `ENV` — devcontainer features install in layers appended
   AFTER the Dockerfile, and the nvm-based Node feature aborts its install when
   `NPM_CONFIG_PREFIX` (or `PREFIX`) is set ("nvm is not compatible with the
   NPM_CONFIG_PREFIX environment variable"), failing every image build.
   `sudo npm install -g` is always a bug.
2. **Latest agent CLIs**: `@openai/codex@latest`, `opencode-ai@latest`,
   `@nanocollective/nanocoder@latest` install on create and refresh on every
   start via `.devcontainer/lib-agent-tools.sh`.
3. **Fast launches**: the refresh is gated by a registry version-check fast
   path — probe `npm view <spec> version`, compare with `npm ls -g --json`,
   skip the reinstall when equal, and keep the installed CLI when the
   registry is unreachable. Never reintroduce an unconditional
   `npm install -g <pkg>@latest` on the launch path.
4. **Best-effort lifecycle**: every step in post-create/post-start warns and
   continues; a failure must never block the container from opening.
   Bootstrap functions `return`, never `exit`.
5. **GitHub MCP everywhere**: the pinned, checksum-verified
   `github-mcp-server` binary (`ARG GITHUB_MCP_VERSION` in the Dockerfile)
   is wired into every harness below. Auth is environmental
   (`GITHUB_PERSONAL_ACCESS_TOKEN` via `remoteEnv`), never stored in config.

6. **The image builds in CI**: `.github/workflows/devcontainer-build.yml`
   builds the devcontainer image via `devcontainers/ci` on `.devcontainer/**`
   changes and monthly (upstream base-image/feature drift). Feature installs
   only fail at image-build time, so this is the only check that catches
   devcontainer-only breakage before a developer's local rebuild.

| Harness | Config | Key fields |
| --- | --- | --- |
| Codex | `~/.codex/config.toml` (written idempotently) | `[mcp_servers.github]`, `command = "/usr/local/bin/github-mcp-server"`, `args = ["stdio"]` |
| VS Code + Copilot | `.vscode/mcp.json` | `servers.github`, `command`, `${env:GITHUB_PERSONAL_ACCESS_TOKEN}` |
| Claude Code | `.mcp.json` | `mcpServers.github`, **`"type": "stdio"`** |
| Nanocoder | `.mcp.json` (same file) | `mcpServers.github`, **`"transport": "stdio"`** |
| OpenCode | `opencode.json` | `mcp.github`, `"type": "local"`, `command: [...]` |

**Dual-key invariant**: `.mcp.json` must carry BOTH `type` (Claude Code) and
`transport` (Nanocoder) — dropping either silently unwires one harness.

---

## Where It Is Enforced

- `tests/agent_tooling_guards.rs` — live-view presence guards in the cargo
  test matrix (all OSes).
- `scripts/check-tooling-parity.sh` — same contract in CI (ci.yml).
- `scripts/validate-ci.sh` — shellchecks `.devcontainer/*.sh` on every run.
- `.llm/context.md` — Tooling Parity Rules (Required).

Changing the contract requires updating ALL of the above in the same commit.

---

## Verification

```bash
bash -n .devcontainer/*.sh
shellcheck --severity=warning .devcontainer/*.sh
bash scripts/check-tooling-parity.sh
cargo nextest run -E 'test(agent_tooling_guards)'
```

For runtime behavior, simulate a launch refresh:
`bash -c 'source .devcontainer/lib-agent-tools.sh && install_codex_cli'` —
it must print the fast-path skip when already current.

The full image build (features included) is validated by
`.github/workflows/devcontainer-build.yml`; it is too heavy to run locally on
every change, so rely on it in CI and on the static guards above.

---

## Common Regressions

- Unconditional reinstall on launch → slow open; gate with the version check.
- `sudo npm install -g` in scripts or docs → breaks the prefix contract.
- `ENV NPM_CONFIG_PREFIX` (or `ENV PREFIX=`) in `.devcontainer/Dockerfile` →
  the nvm Node feature aborts during image build ("nvm is not compatible with
  the NPM_CONFIG_PREFIX environment variable", exit code 11) and the container
  never starts. The prefix belongs in `containerEnv` only.
- Removing `type` or `transport` from `.mcp.json` → a harness loses GitHub.
- Bumping `GITHUB_MCP_VERSION` without fresh SHA256 ARGs → build fails
  checksum verification (that is the supply-chain guard working).
- Making post-start fail the container on a network error → violates
  best-effort; warn and continue instead.
- Deleting or skipping `devcontainer-build.yml` → devcontainer-only build
  breakage becomes invisible until someone rebuilds locally.
