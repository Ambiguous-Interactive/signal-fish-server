//! Drift guards for the devcontainer agent-tooling contract.
//!
//! The devcontainer promises, on every build/rebuild/launch:
//!   1. A user-owned npm global prefix — `npm install -g` never needs sudo
//!      (the nvm-managed Node under /usr/local/share/nvm is root-owned).
//!      The prefix is RUNTIME-ONLY (containerEnv + configure_user_npm_prefix):
//!      a Dockerfile `ENV NPM_CONFIG_PREFIX` leaks into devcontainer feature
//!      installs, which run in layers appended AFTER the Dockerfile, and nvm
//!      aborts when NPM_CONFIG_PREFIX is set — breaking every image build.
//!   2. The terminal agent CLIs (OpenAI Codex, OpenCode, Nanocoder) installed
//!      at the latest npm version and refreshed on every container start —
//!      through a registry version-check fast path, so an ordinary launch
//!      pays a couple of registry probes instead of three full reinstalls.
//!   3. The pinned, checksum-verified GitHub MCP server wired into every
//!      agent harness: Codex (`~/.codex/config.toml`, written idempotently by
//!      `.devcontainer/lib-agent-tools.sh`), VS Code + Copilot
//!      (`.vscode/mcp.json`), Claude Code + Nanocoder (`.mcp.json`), and
//!      OpenCode (`opencode.json`).
//!   4. The devcontainer image itself builds in CI
//!      (`.github/workflows/devcontainer-build.yml`): feature installs only
//!      fail at image-build time, so a broken build must surface there, not
//!      the first time a developer rebuilds locally.
//!
//! `scripts/check-tooling-parity.sh` enforces the same contract in CI; these
//! guards run in the cargo test matrix on every OS so the contract cannot
//! silently regress on a machine that skips the script. Presence assertions
//! read the live (comment-stripped) view via `read_live_file`: a
//! commented-out line must never satisfy a guard — see
//! tests/drift_guard_hygiene.rs and .llm/skills/ci-config-live-view-tests/SKILL.md.
//!
//! Cross-harness subtlety encoded below: Nanocoder loads project `.mcp.json`
//! via the `transport` key while Claude Code requires `type`, so the shared
//! committed file must carry BOTH keys — dropping either one silently
//! unwires a whole harness from GitHub.

mod common;

use common::{read_file, read_live_file, repo_root};
use std::path::{Path, PathBuf};

/// Assert every `fragment` occurs in `live`, failing with a message that names
/// the contract (not just the missing token) so the fix is obvious.
fn require_fragments(live: &str, fragments: &[&str], contract: &str) {
    for fragment in fragments {
        assert!(
            live.contains(fragment),
            "{contract}\n\nMissing required fragment: {fragment}\n\n\
             Restore it, or update the devcontainer agent-tooling contract in \
             .llm/context.md (Tooling Parity Rules) deliberately — in the same \
             commit as the behavior change, together with \
             scripts/check-tooling-parity.sh."
        );
    }
}

/// Assert no `fragment` occurs in `content`, failing with a message that names
/// the contract (not just the forbidden token) so the fix is obvious.
///
/// Unlike [`require_fragments`], callers must pass the RAW file content
/// (`common::read_file`, not `read_live_file`): for absence assertions a
/// commented-out occurrence is still a real occurrence worth flagging — see
/// the presence-vs-absence caveat in `common::strip_comment_lines`.
fn forbid_fragments(content: &str, fragments: &[&str], contract: &str) {
    for fragment in fragments {
        assert!(
            !content.contains(fragment),
            "{contract}\n\nForbidden fragment found: {fragment}\n\n\
             Remove it, or update the devcontainer agent-tooling contract in \
             .llm/context.md (Tooling Parity Rules) deliberately — in the same \
             commit as the behavior change, together with \
             scripts/check-tooling-parity.sh."
        );
    }
}

fn read_live(path: &str) -> String {
    let full: PathBuf = repo_root().join(path);
    assert!(
        Path::new(&full).is_file(),
        "Required agent-tooling file is missing: {path}"
    );
    read_live_file(&full)
}

fn read_raw(path: &str) -> String {
    let full: PathBuf = repo_root().join(path);
    assert!(
        Path::new(&full).is_file(),
        "Required agent-tooling file is missing: {path}"
    );
    read_file(&full)
}

#[test]
fn npm_global_installs_never_need_sudo() {
    let contract = "Devcontainer npm global installs must be routed through the \
                    user-owned prefix /home/vscode/.npm-global so `npm install -g` \
                    never needs sudo.";

    let devcontainer = read_live(".devcontainer/devcontainer.json");
    require_fragments(
        &devcontainer,
        &[
            "\"NPM_CONFIG_PREFIX\": \"/home/vscode/.npm-global\"",
            "\"postStartCommand\": \"bash .devcontainer/post-start.sh\"",
            "\"GITHUB_PERSONAL_ACCESS_TOKEN\"",
        ],
        contract,
    );

    let dockerfile = read_live(".devcontainer/Dockerfile");
    require_fragments(
        &dockerfile,
        &[
            // Runtime wiring only — see build_time_env_never_breaks_the_nvm_node_feature.
            "ENV PATH=\"/home/vscode/.npm-global/bin:${PATH}\"",
            "RUN mkdir -p /home/vscode/.npm-global",
        ],
        contract,
    );

    let post_start = read_live(".devcontainer/post-start.sh");
    require_fragments(&post_start, &["configure_user_npm_prefix"], contract);

    let lib = read_live(".devcontainer/lib-agent-tools.sh");
    require_fragments(
        &lib,
        &[
            "prefix=${npm_prefix}",
            "export NPM_CONFIG_PREFIX=\"$npm_prefix\"",
        ],
        contract,
    );
}

#[test]
fn build_time_env_never_breaks_the_nvm_node_feature() {
    let contract = "The devcontainer Dockerfile must never export NPM_CONFIG_PREFIX \
                    (or PREFIX) via ENV: devcontainer features install in layers \
                    appended AFTER the Dockerfile, and the nvm-based Node feature \
                    (ghcr.io/devcontainers/features/node) aborts its install with \
                    \"nvm is not compatible with the \\\"NPM_CONFIG_PREFIX\\\" \
                    environment variable\" when the variable is already set — \
                    failing every no-cache image build with exit code 11. The \
                    user-owned prefix is enforced at runtime only, by \
                    devcontainer.json containerEnv plus configure_user_npm_prefix \
                    in .devcontainer/lib-agent-tools.sh.";

    // Absence assertions read the RAW file: a commented-out `ENV
    // NPM_CONFIG_PREFIX=...` is still a real occurrence worth flagging.
    let dockerfile = read_raw(".devcontainer/Dockerfile");
    forbid_fragments(
        &dockerfile,
        &["ENV NPM_CONFIG_PREFIX", "ENV PREFIX="],
        contract,
    );

    // The runtime halves of the contract must stay in place.
    let devcontainer = read_live(".devcontainer/devcontainer.json");
    require_fragments(
        &devcontainer,
        &["\"NPM_CONFIG_PREFIX\": \"/home/vscode/.npm-global\""],
        contract,
    );
    let lib = read_live(".devcontainer/lib-agent-tools.sh");
    require_fragments(
        &lib,
        &["export NPM_CONFIG_PREFIX=\"$npm_prefix\""],
        contract,
    );
}

#[test]
fn agent_clis_refresh_to_latest_on_every_launch() {
    let contract = "Codex, OpenCode, and Nanocoder must install/refresh to the \
                    latest npm version on container create and on every start, \
                    best-effort and skippable, via .devcontainer/lib-agent-tools.sh.";

    let post_start = read_live(".devcontainer/post-start.sh");
    require_fragments(
        &post_start,
        &[
            "install_codex_cli",
            "install_opencode_cli",
            "install_nanocoder_cli",
            "configure_codex_github_mcp",
            "SIGNAL_FISH_SKIP_AGENT_REFRESH",
        ],
        contract,
    );

    let lib = read_live(".devcontainer/lib-agent-tools.sh");
    require_fragments(
        &lib,
        &[
            "@openai/codex@latest",
            "opencode-ai@latest",
            "@nanocollective/nanocoder@latest",
        ],
        contract,
    );
}

#[test]
fn agent_cli_refresh_is_fast_by_default() {
    let contract = "The post-start CLI refresh must be gated by a registry \
                    version-check fast path: skip the reinstall when the \
                    installed version is already current, and an unreachable \
                    registry must never block startup — keep the installed \
                    CLI and skip the otherwise-doomed install of an absent \
                    CLI instead of paying the full retry/backoff cost on \
                    every launch. An unconditional \
                    `npm install -g <pkg>@latest` on every launch regresses \
                    container open time.";

    let lib = read_live(".devcontainer/lib-agent-tools.sh");
    require_fragments(
        &lib,
        &[
            "npm_registry_latest_version",
            "npm_global_installed_version",
            "[[ -z \"$latest\" ]]",
            "[[ -n \"$latest\" && \"$latest\" = \"$installed\" ]]",
            "skipping reinstall",
            "Registry unreachable; keeping installed",
            "skipping install (rerun post-create when online)",
        ],
        contract,
    );
}

#[test]
fn github_mcp_server_is_pinned_and_checksum_verified() {
    let contract = "The GitHub MCP server binary must be pinned \
                    (ARG GITHUB_MCP_VERSION) and checksum-verified for both \
                    architectures in .devcontainer/Dockerfile, installed to \
                    /usr/local/bin, and smoke-checked at build time.";

    let dockerfile = read_live(".devcontainer/Dockerfile");
    require_fragments(
        &dockerfile,
        &[
            "ARG GITHUB_MCP_VERSION=",
            "ARG GITHUB_MCP_X86_64_SHA256=",
            "ARG GITHUB_MCP_ARM64_SHA256=",
            "install -m 0755 github-mcp-server /usr/local/bin/github-mcp-server",
            "github-mcp-server --version",
        ],
        contract,
    );
}

#[test]
fn every_harness_is_wired_to_the_github_mcp_server() {
    let contract = "Every agent harness must be wired to the pinned GitHub MCP \
                    server binary (github-mcp-server), authenticated through the \
                    environment (GITHUB_PERSONAL_ACCESS_TOKEN), never a stored \
                    token.";

    let vscode = read_live(".vscode/mcp.json");
    require_fragments(
        &vscode,
        &[
            "\"github\"",
            "github-mcp-server",
            "${env:GITHUB_PERSONAL_ACCESS_TOKEN}",
        ],
        contract,
    );

    let claude_nanocoder = read_live(".mcp.json");
    require_fragments(
        &claude_nanocoder,
        &[
            "\"github\"",
            "github-mcp-server",
            // Claude Code requires `type`; Nanocoder requires `transport`.
            "\"type\": \"stdio\"",
            "\"transport\": \"stdio\"",
            "${GITHUB_PERSONAL_ACCESS_TOKEN:-}",
        ],
        contract,
    );

    let opencode = read_live("opencode.json");
    require_fragments(
        &opencode,
        &["\"github\"", "github-mcp-server", "\"type\": \"local\""],
        contract,
    );

    let lib = read_live(".devcontainer/lib-agent-tools.sh");
    require_fragments(
        &lib,
        &[
            "configure_codex_github_mcp",
            "[mcp_servers.github]",
            "command = \"/usr/local/bin/github-mcp-server\"",
            "args = [\"stdio\"]",
        ],
        contract,
    );
}

#[test]
fn ci_enforces_the_agent_tooling_contract() {
    let contract = "CI must keep enforcing the agent-tooling contract even when \
                    the cargo test matrix is skipped: \
                    scripts/check-tooling-parity.sh (run by ci.yml) must cover the \
                    agent CLIs, the fast-path refresh, the shared .mcp.json dual \
                    keys, OpenCode local wiring, and shellcheck coverage of the \
                    devcontainer lifecycle scripts (scripts/validate-ci.sh).";

    let parity = read_live("scripts/check-tooling-parity.sh");
    require_fragments(
        &parity,
        &[
            ".devcontainer/lib-agent-tools.sh",
            "npm_registry_latest_version",
            "npm_global_installed_version",
            "\"transport\": \"stdio\"",
            "\"type\": \"local\"",
            ".devcontainer/*.sh",
            // The flipped npm-prefix guard lives in the parity script too.
            "ENV NPM_CONFIG_PREFIX",
        ],
        contract,
    );

    let validate_ci = read_live("scripts/validate-ci.sh");
    require_fragments(&validate_ci, &[".devcontainer/*.sh"], contract);
}

#[test]
fn ci_builds_the_devcontainer_image() {
    let contract = "The devcontainer image must be built in CI \
                    (.github/workflows/devcontainer-build.yml): devcontainer \
                    features only fail at image-build time, and no other workflow \
                    builds this image, so deleting the workflow makes devcontainer \
                    breakage invisible until a developer rebuilds locally \
                    (e.g. the nvm/NPM_CONFIG_PREFIX feature-install failure).";

    let workflow = read_live(".github/workflows/devcontainer-build.yml");
    require_fragments(
        &workflow,
        &[
            "devcontainers/ci@",
            "push: never",
            "'.devcontainer/**'",
            "cron:",
            "node --version",
            "/home/vscode/.npm-global",
        ],
        contract,
    );

    // The parity script must pin the workflow's existence so the two guards
    // fail together and cannot drift apart.
    let parity = read_live("scripts/check-tooling-parity.sh");
    require_fragments(
        &parity,
        &["devcontainer-build.yml", "devcontainers/ci@"],
        contract,
    );
}
