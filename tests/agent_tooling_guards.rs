//! Drift guards for the devcontainer agent-tooling contract.
//!
//! The devcontainer promises, on every build/rebuild/launch:
//!   1. A user-owned npm global prefix — `npm install -g` never needs sudo
//!      (the nvm-managed Node under /usr/local/share/nvm is root-owned).
//!      The prefix is RUNTIME-ONLY (containerEnv + configure_user_npm_prefix):
//!      a Dockerfile `ENV NPM_CONFIG_PREFIX` leaks into devcontainer feature
//!      installs, which run in layers appended AFTER the Dockerfile, and nvm
//!      aborts when NPM_CONFIG_PREFIX is set — breaking every image build.
//!   2. The terminal agent CLIs (OpenAI Codex, OpenCode, Nanocoder) and Z.AI
//!      Vision MCP server installed at the latest npm version and refreshed on
//!      every container start — through a registry version-check fast path, so
//!      an ordinary launch never performs unconditional reinstalls.
//!   3. The pinned, checksum-verified GitHub MCP server wired into every
//!      agent harness: Codex (`~/.codex/config.toml`, written idempotently by
//!      `.devcontainer/lib-agent-tools.sh`), VS Code + Copilot
//!      (`.vscode/mcp.json`), Claude Code + Nanocoder (`.mcp.json`), and
//!      OpenCode (`opencode.json`). The same harnesses receive the official
//!      Z.AI Vision, Web Search, Web Reader, and Zread MCP servers without
//!      storing the Z.AI API key in the repository.
//!   4. Every lifecycle step is best-effort so an optional network/tooling
//!      failure cannot prevent VS Code from attaching to the container.
//!   5. The devcontainer image itself builds in CI
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
//! unwires a whole harness from GitHub. OpenCode has the mirror-image
//! subtlety: observed in issue #496, its `github-mcp-server` fell back to
//! the OAuth device-authorization flow even with the token present in the
//! container shell, so `opencode.json` must forward
//! `GITHUB_PERSONAL_ACCESS_TOKEN` explicitly via its `environment` map.

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

/// Extract every `cargo-nextest@<version>` pin from `content` (workflow
/// `tool:` lines and Dockerfile `cargo binstall` list entries). Unpinned
/// forms (`cargo-nextest@latest`, a bare `@`) return nothing: they are the
/// drift this guard exists to catch.
fn cargo_nextest_pins(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| {
            let start = line.find("cargo-nextest@")?;
            let version = &line[start + "cargo-nextest@".len()..];
            let end = version
                .find(|c: char| !(c.is_ascii_digit() || c == '.'))
                .unwrap_or(version.len());
            let pin = line[start..start + "cargo-nextest@".len() + end].to_string();
            let version = pin["cargo-nextest@".len()..].trim_end_matches('.');
            if version.is_empty() {
                return None;
            }
            Some(pin.trim_end_matches('.').to_string())
        })
        .collect()
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

    let post_create = read_live(".devcontainer/post-create.sh");
    require_fragments(
        &post_create,
        &[
            "refresh_agent_npm_tools",
            "node_modules",
            "sudo install -d -m 0755",
        ],
        contract,
    );

    let runtime_check = read_live(".devcontainer/verify-agent-tooling-runtime.sh");
    require_fragments(
        &runtime_check,
        &[
            "npm install --global",
            "$workspace_root/node_modules",
            "$workspace_root/clients/browser/node_modules",
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
    let contract = "Codex, OpenCode, Nanocoder, and the Z.AI Vision MCP server \
                    must install/refresh to the latest npm version on container \
                    create and on every start, best-effort and skippable, via \
                    .devcontainer/lib-agent-tools.sh.";

    let post_start = read_live(".devcontainer/post-start.sh");
    require_fragments(
        &post_start,
        &[
            "refresh_agent_npm_tools",
            "configure_codex_mcp_servers",
            "SIGNAL_FISH_SKIP_AGENT_REFRESH",
        ],
        contract,
    );

    let lib = read_live(".devcontainer/lib-agent-tools.sh");
    require_fragments(
        &lib,
        &[
            "@openai/codex@latest",
            // OpenCode V2 ships in the @opencode/cli npm package; the legacy
            // opencode-ai package still serves V1 1.x from npm's latest tag.
            "@opencode/cli@latest",
            "migrate_opencode_to_v2",
            "@nanocollective/nanocoder@latest",
            "@z_ai/mcp-server@latest",
        ],
        contract,
    );
}

#[test]
fn zai_mcp_suite_is_wired_to_every_harness() {
    let contract = "Every supported agent harness must receive the official Z.AI \
                    Vision, Web Search, Web Reader, and Zread MCP servers. The \
                    API key must come from Z_AI_API_KEY in the environment, \
                    never from committed configuration.";

    let devcontainer = read_live(".devcontainer/devcontainer.json");
    require_fragments(
        &devcontainer,
        &[
            "\"Z_AI_API_KEY\"",
            "\"Z_AI_MODE\": \"ZAI\"",
            "\"--env-file\"",
            "${localWorkspaceFolder}/.env.local",
            "${containerEnv:Z_AI_API_KEY:}",
        ],
        contract,
    );

    for path in [".vscode/mcp.json", ".mcp.json", "opencode.json"] {
        require_fragments(
            &read_live(path),
            &[
                "zai-vision",
                "zai-web-search",
                "zai-web-reader",
                "zai-zread",
                "zai-mcp.mjs",
            ],
            contract,
        );
    }
    // OpenCode V2 groups servers under mcp.servers; the four Z.AI entries must
    // live inside that map, not directly under mcp (the V1 layout).
    require_fragments(
        &read_live("opencode.json"),
        &["\"servers\"", "zai-vision", "zai-web-search", "zai-web-reader", "zai-zread"],
        contract,
    );
    require_fragments(
        &read_live(".devcontainer/lib-agent-tools.sh"),
        &["configure_codex_mcp_servers", "configure-zai-mcp.py"],
        contract,
    );
    require_fragments(
        &read_live(".devcontainer/zai-mcp.mjs"),
        &[
            "../.env.local",
            "parseEnv",
            "values.Z_AI_API_KEY ?? environment.Z_AI_API_KEY",
            "/home/vscode/.npm-global/bin/zai-mcp-server",
            "https://api.z.ai/api/mcp/web_search_prime/mcp",
            "https://api.z.ai/api/mcp/web_reader/mcp",
            "https://api.z.ai/api/mcp/zread/mcp",
        ],
        contract,
    );
}

#[test]
fn opencode_config_is_native_v2() {
    let contract = "opencode.json must use the native OpenCode V2 config shape: \
                    servers grouped under mcp.servers (V2 does not place server \
                    names directly under mcp), the V1-only `enabled` key absent \
                    (V2 inverts it as `disabled`, defaulting to enabled), and \
                    the published $schema URL retained for editor integration. \
                    Servers keep the default classic MCP handshake: the Z.AI \
                    relay's bundled MCP SDK predates the 2026-07-28 revision, \
                    so a `protocol: \"auto\"` probe can never succeed there and \
                    only costs a startup process per relay; revisit only if \
                    Z.AI ships 2026-07-28 support.";

    let opencode = read_live("opencode.json");
    require_fragments(
        &opencode,
        &["\"$schema\": \"https://opencode.ai/config.json\"", "\"mcp\"", "\"servers\""],
        contract,
    );
    // Five servers total — github plus the four Z.AI relays — and every one
    // of them is a local stdio command in this harness.
    for server in ["github", "zai-vision", "zai-web-search", "zai-web-reader", "zai-zread"] {
        require_fragments(&opencode, &[&format!("\"{server}\"")], contract);
    }
    assert_eq!(
        opencode.matches("\"type\": \"local\"").count(),
        5,
        "{contract}\n\nExpected exactly five local stdio servers in opencode.json."
    );
    // Absence reads the RAW file: a commented-out V1 leftover is still a real
    // occurrence worth flagging.
    forbid_fragments(
        &read_raw("opencode.json"),
        &["\"enabled\"", "\"protocol\""],
        contract,
    );
}

#[test]
fn lifecycle_tooling_failures_never_block_container_attach() {
    let contract = "Optional devcontainer setup and refresh steps must be guarded \
                    at the lifecycle-script top level so a transient registry, \
                    filesystem, or tooling failure cannot prevent VS Code from \
                    attaching or skip later setup steps.";

    let post_create = read_live(".devcontainer/post-create.sh");
    require_fragments(
        &post_create,
        &[
            "if ! prepare_worktree_cache_dirs; then",
            "if ! configure_git_safe_directory; then",
            "if ! configure_codex_mcp_servers; then",
            "if ! verify_required_rust_tools; then",
            "if ! make_project_scripts_executable; then",
        ],
        contract,
    );

    let post_start = read_live(".devcontainer/post-start.sh");
    require_fragments(
        &post_start,
        &[
            "if ! configure_codex_mcp_servers; then",
            "if ! refresh_agent_npm_tools; then",
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
            "npm outdated --global --json",
            "refresh_agent_npm_tools",
            "known_installed",
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
fn cargo_nextest_pin_matches_the_ci_lanes() {
    let contract = "The devcontainer must install cargo-nextest at the exact \
                    version the CI lanes pin: .config/nextest.toml encodes \
                    per-profile timeout and filter semantics that drift \
                    between nextest versions (issue #597).";

    let workflows = [
        ".github/workflows/ci.yml",
        ".github/workflows/mutation.yml",
        ".github/workflows/verification-nightly.yml",
    ];
    let mut pins: Vec<String> = Vec::new();
    for path in workflows {
        let found = cargo_nextest_pins(&read_live(path));
        assert!(
            !found.is_empty(),
            "{path} must pin cargo-nextest@<version>: an unpinned lane \
             drifts .config/nextest.toml timeout and filter semantics"
        );
        pins.extend(found);
    }
    pins.sort();
    pins.dedup();
    assert_eq!(
        pins.len(),
        1,
        "all CI lanes must pin the same cargo-nextest version, found: {pins:?}"
    );

    let dockerfile = read_live(".devcontainer/Dockerfile");
    require_fragments(&dockerfile, &[&pins[0]], contract);
}

#[test]
fn amd64_binstall_fails_fast_instead_of_doomed_musl_source_compiles() {
    let contract = "cargo-binstall resolves releases through the unauthenticated \
                    GitHub API. A degraded API response must fail fast on amd64: \
                    every amd64-musl tool in the binstall list has a prebuilt \
                    artifact (GitHub release or QuickInstall), while the \
                    source-compile fallback is doomed on this image — a musl \
                    build of an OpenSSL-dependent crate (e.g. cargo-tarpaulin) \
                    dies on openssl-sys after several wasted minutes (issue \
                    #599). arm64 keeps the source-compile fallback: some tools \
                    (cargo-watch) ship no aarch64-musl release artifact.";

    let dockerfile = read_live(".devcontainer/Dockerfile");
    require_fragments(
        &dockerfile,
        &[
            // amd64 arm disables the source-compile strategy outright.
            "amd64) cargo_binstall_target=\"x86_64-unknown-linux-musl\"; \
             binstall_args+=(--disable-strategies compile) ;;",
            // arm64 arm keeps the default strategies (compile fallback intact).
            "arm64) cargo_binstall_target=\"aarch64-unknown-linux-musl\" ;;",
            // The invocation must actually forward the per-arch strategy args.
            "cargo binstall --no-confirm --locked --target \
             \"$cargo_binstall_target\" \"${binstall_args[@]}\"",
        ],
        contract,
    );

    // Exactly one --disable-strategies occurrence: the per-arch amd64 arm. An
    // unconditional flag on the invocation line would disable arm64's
    // source-compile fallback, which cargo-watch still needs.
    assert_eq!(
        dockerfile.matches("--disable-strategies").count(),
        1,
        "{contract}\n\n--disable-strategies must appear exactly once (the amd64 \
         case arm); extra occurrences would also strip arm64's fallback."
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
        &[
            // V2 groups servers under mcp.servers; a server name directly
            // under mcp is the V1 layout.
            "\"servers\"",
            "\"github\"",
            "github-mcp-server",
            "\"type\": \"local\"",
            // Observed in #496: the opencode-launched server device-flowed
            // even with the token in the container shell — the pass-through
            // is mandatory, shell inheritance cannot be relied on.
            "\"environment\"",
            "\"GITHUB_PERSONAL_ACCESS_TOKEN\": \"{env:GITHUB_PERSONAL_ACCESS_TOKEN}\"",
        ],
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
            "migrate_managed_github_env",
            "env_vars = [\"GITHUB_PERSONAL_ACCESS_TOKEN\"]",
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
                    keys, OpenCode local wiring (including the GitHub token \
                    environment pass-through), and shellcheck coverage of the \
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
            "\"GITHUB_PERSONAL_ACCESS_TOKEN\": \"{env:GITHUB_PERSONAL_ACCESS_TOKEN}\"",
            "@z_ai/mcp-server@latest",
            "for endpoint in web_search_prime web_reader zread",
            "https://api.z.ai/api/mcp/$endpoint/mcp",
            "verify-agent-tooling-runtime.sh",
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
            "verify-agent-tooling-runtime.sh",
        ],
        contract,
    );

    let runtime_check = read_live(".devcontainer/verify-agent-tooling-runtime.sh");
    require_fragments(
        &runtime_check,
        &[
            "npm install --global",
            "zai-mcp-server",
            "scripts/test_zai_mcp.py",
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
