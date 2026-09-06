---
name: devcontainer-agent-tooling
description: >-
  Maintain the devcontainer agent-tooling contract: sudo-free npm, fast
  latest-version CLI/MCP refresh (Codex, OpenCode, Nanocoder, Z.AI Vision),
  and GitHub/Z.AI MCP wiring for every agent harness. Use when touching
  .devcontainer/, agent CLI installation, or any MCP config file.
---

# Devcontainer Agent Tooling

---

## When to Use

- Editing `.devcontainer/` (Dockerfile, devcontainer.json, post-create.sh,
  post-start.sh, lib-agent-tools.sh)
- Adding, removing, or reconfiguring an agent harness (Codex, Claude Code,
  Copilot, VS Code, OpenCode, Nanocoder) or its MCP servers
- Touching npm global installs, the npm prefix, or GitHub/Z.AI MCP servers
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
   The root and browser-client `node_modules` named volumes must be writable by
   the remote user. `sudo npm install` (local or global) is always a bug.
2. **Latest npm agent tools**: `@openai/codex@latest`, `opencode-ai@latest`,
   `@nanocollective/nanocoder@latest`, and `@z_ai/mcp-server@latest` install on
   create and refresh on every start via `.devcontainer/lib-agent-tools.sh`.
   Pass `--allow-scripts="$pkg"` so npm 11 runs OpenCode's required postinstall
   while authorizing lifecycle scripts only for the selected package.
3. **Fast launches**: the refresh is gated by a registry version-check fast
   path — use one bulk `npm outdated -g --json` request with local
   `npm ls -g --json` state, use `npm view` only for a missing tool, and skip
   reinstalls when current. When the registry is unreachable, keep installed
   tools and skip otherwise-doomed absent-tool installs (warn and continue;
   rerun post-create when online). Never reintroduce an unconditional
   `npm install -g <pkg>@latest` on the launch path.
4. **Fast attach**: keep `waitFor: updateContentCommand` explicit so
   post-create/start network refreshes run behind the editor attach point.
5. **Best-effort lifecycle**: every step in post-create/post-start warns and
   continues; a failure must never block the container from opening.
   Bootstrap functions `return`, never `exit`.
6. **GitHub MCP everywhere**: the pinned, checksum-verified
   `github-mcp-server` binary (`ARG GITHUB_MCP_VERSION` in the Dockerfile)
   is wired into every harness below. Auth is environmental
   (`GITHUB_PERSONAL_ACCESS_TOKEN` via runtime `.env.local` and `remoteEnv`), never stored in config.

7. **Z.AI MCP everywhere**: all frontends invoke `.devcontainer/zai-mcp.mjs`
   over stdio. It parses repository `.env.local` at startup using Node `parseEnv`;
   file keys override inherited credentials, including explicit empty values.
   Missing files/keys fall back to `Z_AI_API_KEY`. Restart MCP servers after key
   changes; no rebuild required. The launcher runs the preinstalled Vision binary
   and uses its bundled MCP SDK for remote HTTP/SSE. Never download at startup.
   `configure-zai-mcp.py` migrates only marker-owned Codex entries to absolute
   launcher paths and preserves custom tables. No keys are stored in config.
   Keep `.env*` out of Docker builds; empty `.env.local` is valid without MCP use.
   `check-zai-mcp.py --live` checks the exact launcher through tools/list.
8. **The image builds and runs in CI**: `.github/workflows/devcontainer-build.yml`
   builds the devcontainer image via `devcontainers/ci` on `.devcontainer/**`
   changes and monthly (upstream base-image/feature drift). Feature installs
   only fail at image-build time, so this is the only check that catches
   devcontainer-only breakage before a developer's local rebuild. It runs the
   real post-start path and `.devcontainer/verify-agent-tooling-runtime.sh` to
   prove sudo-free global npm installation, writable local dependency volumes,
   and tool availability.

| Harness | Config | Key fields |
| --- | --- | --- |
| Codex | `~/.codex/config.toml` (written idempotently) | `[mcp_servers.github]`, `command = "/usr/local/bin/github-mcp-server"`, `args = ["stdio"]` |
| VS Code + Copilot | `.vscode/mcp.json` | `servers.github`, `command`, `${env:GITHUB_PERSONAL_ACCESS_TOKEN}` |
| Claude Code | `.mcp.json` | `mcpServers.github`, **`"type": "stdio"`** |
| Nanocoder | `.mcp.json` (same file) | `mcpServers.github`, **`"transport": "stdio"`** |
| OpenCode | `opencode.json` | `mcp.github`, `"type": "local"`, `command: [...]`, **`environment: { GITHUB_PERSONAL_ACCESS_TOKEN: "{env:GITHUB_PERSONAL_ACCESS_TOKEN}" }`** |

Each config also registers `zai-vision`, `zai-web-search`, `zai-web-reader`,
and `zai-zread`. Local Vision always invokes the preinstalled
`/home/vscode/.npm-global/bin/zai-mcp-server`; do not replace it with `npx`,
which introduces a network/cache dependency during MCP startup.

**Dual-key invariant**: `.mcp.json` must carry BOTH `type` (Claude Code) and
`transport` (Nanocoder) — dropping either silently unwires one harness.

**OpenCode env pass-through invariant**: observed in issue #496, the
OpenCode-launched `github-mcp-server` fell back to the interactive OAuth
device-authorization flow every session even with
`GITHUB_PERSONAL_ACCESS_TOKEN` present in the container shell, while every
other harness authenticated — so `opencode.json` must forward the token
explicitly via its `environment` map
(`"{env:GITHUB_PERSONAL_ACCESS_TOKEN}"` substitution). Do not rely on
shell-environment inheritance for opencode-launched MCP processes.

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
python3 scripts/test_zai_mcp.py
node --test scripts/test-zai-mcp.mjs
cargo nextest run -E 'test(agent_tooling_guards)'
```

For runtime behavior, run `bash .devcontainer/post-start.sh`, then
`bash .devcontainer/verify-agent-tooling-runtime.sh`. Current packages must
print the fast-path skip and the smoke check must install its local fixture
globally without sudo.

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
- Removing the `environment` pass-through from `opencode.json` → the OpenCode
  GitHub MCP server starts unauthenticated and prompts the OAuth device flow
  every session (issue #496).
- Replacing the preinstalled Z.AI Vision command with `npx` → every MCP startup
  can wait on the registry and becomes unreliable offline.
- Dropping the package-scoped `--allow-scripts` option → npm 11 can skip
  OpenCode's platform-binary postinstall and leave a fresh install unusable.
- Hardcoding `Z_AI_API_KEY` or derived authorization headers → leaks a secret;
  use the shared dotenv-aware launcher.
- Removing `waitFor: updateContentCommand` → future default changes can put
  registry work back on the editor attach path.
- Bumping `GITHUB_MCP_VERSION` without fresh SHA256 ARGs → build fails
  checksum verification (that is the supply-chain guard working).
- Making post-start fail the container on a network error → violates
  best-effort; warn and continue instead.
- Deleting or skipping `devcontainer-build.yml` → devcontainer-only build
  breakage becomes invisible until someone rebuilds locally.
